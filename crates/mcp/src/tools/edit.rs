//! Edit tools: the batched `edit` verb, and `undo` / `redo` over its ledger.
//!
//! `edit` runs its ops in order against the engine session. Each op's engine
//! history steps are counted; an op that fails rolls the batch back by that many
//! undos, so a refused batch changes nothing. A kept batch is one entry in
//! `Server::ledger` (the number of engine steps it pushed), which `undo` pops
//! whole. A batch is counted by the engine's `history_pushes`, not its undo
//! depth: depth stops growing at the 100-step cap, so a batch there would count
//! as zero. Ledger entries the cap has evicted are trimmed (`Server::trim_ledger`). Addresses are `LANE@START`, re-read after every op.

use std::path::{Path, PathBuf};

use engine::beats::{self, Found};
use engine::project::{Edge, Lane, LaneKind, RedoCheckpoint, WallBound};
use engine::scale::FitPolicy;
use engine::transitions::{Dir, TransitionKind};
use engine::{AudioSession, Clip, PlaybackSession, SourceEdit, Speed};
use serde_json::{json, Value};

use super::edit_fx::{self, Applied};
use super::edit_subs;
use super::generated;
use crate::parse;
use crate::server::{arg_str, opt_bool, opt_str, tri, Entry, Reply, Server, Tool};

const NO_PROJECT: &str = "no project open — call project_open or project_new first";
/// Undo and redo with no project open: there is no history to walk, and no checkpoint to name.
const NO_PROJECT_UNDO: &str = "no project is open, so there is nothing to undo; call project_open or project_new first";
const NO_PROJECT_REDO: &str = "no project is open, so there is nothing to redo; call project_open or project_new first";

/// Ops this module answers. Anything else goes to `edit_fx::apply`, then `edit_subs::apply`.
const OWN_OPS: &[&str] = &[
    "chapter_add", "chapter_set", "chapter_remove", "marker_add", "marker_set", "marker_remove", "beats",
    "place", "insert", "overwrite", "append", "keep", "assemble", "split", "trim", "split_edit", "roll", "slip", "slide", "move", "delete", "lift", "ripple_delete",
    "close_gaps", "cut_regions", "insert_time", "remove_time", "freeze", "speed", "speed_ramp", "reverse", "fade", "dissolve", "transition", "crossfade", "visualizer",
    "swap", "copy_look", "replace", "nudge",
];

/// Ops `edit_fx` answers.
const FX_OPS: &[&str] = &[
    "color", "lut", "wheels", "curves", "look", "auto_color", "transform", "transform_keys", "opacity_keys", "mask_keys", "color_keys", "fit", "eq", "gain", "gain_keys", "gain_key", "gain_key_remove", "gain_ramp",
    "gain_dip", "lane_gain", "pan", "pitch", "lane_pan", "limiter", "lane_add", "lane_remove",
    "lane_move", "lane_flags", "lane_name", "deinterlace", "group", "ungroup", "subtitles_import", "duck", "dynamics", "stabilize", "reframe", "nest", "break_apart", "place_sequence", "angle", "title_add", "title_set", "title_remove",
    "generator_add", "generator_set", "adjustment_add", "label", "enable",
];

/// Ops `edit_subs` answers (captions).
const SUB_OPS: &[&str] = edit_subs::OPS;

/// Most shifted clips one op's reply lists; the rest are counted.
const SHIFT_CAP: usize = 20;
/// The most ops whose lines a batch reply prints; the rest are counted.
const OP_LINES_CAP: usize = 40;

/// A still can be trimmed out to STILL_MAX_SECS (the engine's IMAGE_MAX_SECS). How long it is placed
/// for when no length is given is the project's still length (`project_settings still_secs`).
const STILL_MAX_SECS: f64 = 600.0;

const EDIT_DOC: &str = r#"Batched edit: ops run in order as one undo step; if op k fails, all roll back.
- Clip: V1@450 (lane@start) or V1#0. Lanes: V1.. A1.. S1.. (S = captions).
- Timeline times (at, to, frames, gap): 450f, 15.0s, 15, 00:00:15:00, end.
- Signed distances (by, in roll, slip, slide): the same forms with a sign, -15f, +0.5s, -00:00:01:00. A bare number is seconds. `end` is not a distance.
- Source times (in/out/ranges): seconds or 450f of the FILE's own fps.
- [group] = the whole linked take (V1+A1, or the one clip when it has no partners); [clip] = this clip alone: its partners are not moved, and it leaves the take while the partners stay linked to each other (a locked partner refuses).
- Off-frame times round, and the line says so: (1.53s → 38f).
- A locked lane is never changed: an op that would change it refuses and names the lane (lane_flags unlocks it). A ripple that only reaches a locked caption lane through the picture skips it with a note.
- dry_run:true applies, reports, rolls back; undo and redo history are left as they were.
- An op takes only the keys listed for it; any other key refuses the batch, naming the keys it takes.
- A shifted clip is listed as old→new (shifted: V1@40→V1@20), up to 20.
- Addresses refer to the timeline as left by the previous op: ops run in order, so after a ripple the clips after it have new addresses (use the shifted lists). Delete from the end of a lane, or re-read the addresses.
- Every reply lists the side effects of each op: effects: (clips split, holes left, takes detached) and the shifted clips.
- RIPPLE RULE (delete, ripple_delete, speed ripple:true, cut_regions, insert_time, remove_time): the scope is the edited lane(s), plus the lanes of clips linked or grouped to the edited clip(s) (a take moves whole), closed over what the ripple reaches. On a picture lane, captions and titles over the moved picture move too; a locked caption lane in that default is skipped with a note, never refused. Lanes outside the scope never move and are never split. `lanes:[...]` replaces the default scope and must name the edited lane (cut_regions, insert_time and remove_time default to every unlocked lane).
PLACE
place {source, at, lane?, in?, out?, length?, stretch?, audio?, audio_lane?, overwrite?, audio_track?} — audio_track:N picks the sound track of a multi-audio file (0-based, the numbers media_probe lists as audio track N; the reply names it); a track other than the source's own places the whole track: in/out, length and stretch apply to that track, and the file's others are not touched. length is the duration from in (instead of out; not both). stretch:true with length plays the whole source from in (to out, or its end) in that length: the speed is set to fit, and the reply says it (speed 0.25–4). No lane: linked V1+A1; a video lane takes the picture, and a source with sound also puts its sound on A1 (or audio_lane), grouped with the picture. audio:false = picture only. An occupied lane refuses the op and names the clip in the way; overwrite:true replaces what is there and the reply lists it. A still (the project's still length, 5.0 s by default, up to 600 s) takes no in/out: trim its out edge afterwards to change its length.
append {source, in?, out?, length?, audio?, lane?, until?, audio_track?} — audio_track as place's (in/out and length apply to the track; not with until). length is one copy's duration from in (instead of out). With no at it starts at the end of V1 (0 for an empty V1), or of `lane` when given, not the timeline end; sound pairs on A1 as place pairs it, and a busy A1 there refuses (audio:false = picture only, on V1). until:"25s" LOOPS the source (its in..out range, or the whole file) back to back from the end of the lane until that time, the last copy trimmed to end there; one undo step. Music bed: append {source, lane:"A2", until:"25s"}.
insert {source, in?, out?, at, to?, length?, lanes?} — three-point edit of a source range (in..out, the whole file when none; `src` also spells `source`) at timeline `at`; the length is the range, or `to` (a timeline out mark: duration = to − at, from `in`) or `length` (not with out). The take lands on `lanes` (at most one video and one audio lane: V gets the picture, A the sound; a lone V or A gives picture or sound alone), else on the first unlocked video and audio lanes the source has (V1+A1), linked as one take; a still takes the video lane and `length`/`to` (default 5.0 s), a song the audio lane only. insert RIPPLES: on those lanes the clip `at` falls inside is cut there and everything from `at` on moves later by the take's length; a lane not in the take never moves or splits, so an insert that would pull a linked take apart (the other half on a lane outside the take, e.g. an audio-only insert into V1+A1 material) is refused by name: add the partner lane to `lanes`, or detach. Captions and titles keep their time. A locked lane refuses; one undo step.
overwrite {source, in?, out?, at, to?, length?, lanes?} — insert's other half: the same range, `at`, `to`/`length`, `lanes` and take (see insert), but it replaces what is there for the duration, trimming or splitting the clips it covers (the reply lists each), and nothing moves; past the end it keeps the gap. A locked lane refuses; one undo step. (place {overwrite:true} is the per-lane form with speed/stretch/audio_track.)
keep {source, ranges:[[in,out]...], lane?, at?, audio?, audio_lane?, overwrite?} — ranges back to back (rough cut). With no at it starts at the end of V1 (or lane). A video lane pairs sound as place does.
assemble {items:[{source,in?,out?}...], lane?, at?, gap?, audio?, audio_lane?, overwrite?} — keep, mixed sources. With no at it starts at the end of V1 (or lane). A video lane pairs sound as place does. Stills are refused here (no in/out to cut): add a still with place {source, at:"end"} or append {source}.
CUT
split {at, lanes?} — cut the clips at `at`. With `lanes`, only those lanes are cut; the other lanes keep their clip running across `at` unbroken. A linked partner of a clip cut is cut too, and the reply names it.
trim {clip, edge:in|out, to, scope?} — move an edge to `to`. [group]; scope:"clip" unlinks.
split_edit {clip, side:audio|video, edge:in|out, to} — the split edit (a J or L cut): the `edge` of the `side` half of the clip's take moves to `to`, and the other half stays where it is. The pair is unlinked first, so the halves can part; `clip` may name either half. Refused when `to` would pass the other edge of that half. One undo step.
roll {clip, by|to, edge?=out} — move the CUT between two clips that butt: the out edge of `clip` and the in edge of the clip after it move together (edge:"in" rolls the cut before `clip` instead), so nothing else moves and the timeline's length stays. `to` is the new cut place, `by` a signed distance (-15f, +0.5s). Clamped to the source media of both clips and a frame each; the reply says so. A gap or no clip there refuses. [group] a take rolls its cuts together.
slip {clip, by} — the clip keeps its place and length and plays footage `by` later in its source (negative: earlier); `by` is timeline frames, so at speed 2 a +10f slip moves the source 20 frames, and the reply says so. Clamped to the file's ends; a still refuses. [group]
slide {clip, by} — move the clip along its lane by `by` (signed) between its neighbours with its content unchanged: the clip before it ends later/earlier and the clip after it starts later/earlier, so the timeline's length stays. Needs a clip on each side; clamped to what the neighbours can give. [group]
move {clip, at, to_lane?, titles?, insert?, split_captions?} — slide to `at`, same kind (V to V); into occupied space it refuses and names the clip in the way. insert:true is a RIPPLE move: the clip (its take, with linked partners) leaves its slot, which closes on the ripple scope as ripple_delete closes one, and lands at `at` with the later clips on those lanes pushed to make room (`at` is where it starts once its old slot has closed). A caption spanning the clip's old edges is refused by name unless split_captions:true, which splits it at the edge and reports both halves. [group] A title wholly over the moved picture moves with it; titles:true also carries one it only partly covers, titles:false carries none (the reply names each that stays).
swap {a, b, ripple?=true} — two clips on one lane trade places: b takes a's start, what lies between them ripples by the difference in their lengths, and a lands where b was; the reply lists the moves. ripple:false moves nothing else (the two trade slots). [clip] A take is refused: ungroup it first.
delete {clip} — remove and ripple: the later clips in its ripple scope move up to close the gap (help {topic:"ripple"}). A later take moves whole on any lane, even one the delete did not name; if the ripple would cut a clip that is not in the deleted clip's take (a detached picture under a detached sound, say), delete refuses and names it: lift the clip, or lift that one too and close_gaps. [group]
lift {clip} — remove the clip alone and leave a gap; nothing moves. [clip]
ripple_delete {clip, lanes?} — cut its span out of the ripple scope (see help {topic:"ripple"}) · close_gaps {lane} — close its gaps. delete and ripple_delete move the titles after the cut with the picture (a title inside the cut lands at the cut point).
cut_regions {regions:[[from,to]...], lanes?} — ripple-cut spans (silence scans); titles move with the cut too.
copy_look {from, to:[addrs] | to_lane, what?=["color"]} — copies the look of clip `from` onto each clip in `to` (or every clip on `to_lane`), one undo step; the reply lists each target. what: "color", "transform", "fit" and "effects" come from a picture clip and land on picture clips; "eq" and "gain" (the clip's dB) come from a sound clip and land on sound clips; "speed" (speed, ramp and reverse) comes from either and lands on any media clip, refused for a clip with no room for the new length. Each target gets the source's exact setting (a source with no colour clears its targets'); a target the part does not fit is named as skipped.
replace {clip, source, in?} — overwrite the clip (and its linked sound/picture half when it covers the same frames) with `source` from `in` (default its start), keeping the clip's place and length; nothing moves. nudge {clips:[addrs], by} — move the clips and their groups by a signed distance (-1f, +0.5s) as one move, refused when a neighbour blocks it outright, and stopped short at the room there is.
insert_time {at, duration, lanes?} — ripple empty time in at `at`: clips, captions and titles starting at/after it move `duration` later on the unlocked lanes (or `lanes`). A clip `at` falls inside is cut there first; a take moves with its partners. Black lead-in: insert_time {at:0, duration:"3s"}, then title_add at 0.
remove_time {at, duration, lanes?} — ripple a span out of the unlocked lanes (or `lanes`): the cut_regions cut for one span. Titles after it move back; a title wholly inside the span is removed and one straddling an edge is trimmed to the part outside (both named in the reply).
TIME
speed {clip, x, ripple?, lanes?} — rate 0.25..4. [group] Its length changes, so a neighbour in its way refuses; ripple:true moves the later items in the RIPPLE RULE scope (titles too) by the length change: slower pushes them later, faster closes the gap. A clip on another lane that spans the change is never cut: outside the scope it is left in place and the reply names it; a scope lane (named in lanes) that holds one refuses, naming it. One undo step.
speed_ramp {clip, keys:[{at, speed}...], ripple?, lanes?} [group] — a speed that changes over the clip: each key sets a speed 0.25–4 at a clip-relative time `at` ("0s", "2s", "end" or frames), linear between keys and held before the first and after the last. Keys must be in increasing `at`, at most 8, and keys:[] clears the ramp back to the clip's constant speed. The clip's length follows the ramp, and ripple:true moves the later items as speed does.
reverse {clip, on?} — the clip plays backward, picture and sound: on true/false, or omit `on` to toggle. [group] Its length and place do not change, so nothing ripples, and it composes with speed and a speed ramp. Fades stay on the timeline's ends and gain and transform keys keep their time on the timeline. A trim, split or join follows the footage (a reversed clip's start edge trims its source's end). A still refuses. One undo step. clip_info and timeline say "reversed". Limits: a reversed clip decodes at 8 bits (a 10-bit source loses its low 2 bits on a 10-bit export; banding is possible on HDR), hardware decode is off for it, a dissolve's B side plays forward, and waveforms and thumbnails draw forward.
fade {clip, in?, out?, linked?} — ramp lengths: int = frames, "0.5s" = time. A linked partner (V1+A1) gets the same ramp; linked:false = this clip only.
dissolve {clip, frames, linked?} — VIDEO transition into the next clip. A linked sound partner crossfades into its next clip when they meet; linked:false = picture only.
transition {clip, kind?=dissolve, length?, direction?, linked?} — dissolve with a look: kind dissolve|dip-black|dip-white|wipe|slide|push|zoom. wipe/slide/push take direction left|right|up|down (default left; the way things move: wipe-left reveals the next clip from the right edge, slide-left brings it in over the old one, push-left pushes the old one out). length (or frames) is the overlap; omitted keeps the clip's current one. The sound is the same crossfade whatever the kind. `dissolve` also takes kind/direction.
crossfade {clip, frames} — AUDIO crossfade into the next clip. [clip]
visualizer {clip, on?, style?, fast?} — the clip becomes a sound picture: on true/false; style fill|ribbon|ring|wave (one of them, not toggles); fast true = the quicker peak painting. style/fast need the clip on, or on:true with them. On a sound clip, on:true places a waveform picture on a video lane grouped with it (so moves, trims and deletes carry it), and style/fast set that picture's look. Omitted keys keep their setting; on:false turns it off. One undo step.
freeze {at, duration, lane?=V1 (insert:false with no lane: a new top video lane), insert?=true} — hold the picture at `at` for `duration`. The composed frame at `at` is saved as <project>/freeze/<stem>-<frame>-<hash8>.png (needs a project path: project_new with a path, or project_save once; the hash is of the pixels, so an identical frame reuses its file and source) and placed as a still on `lane`. insert=true ripples first, as insert_time: everything after moves `duration` later, audio gets silence. insert=false overwrites `lane` for `duration`; with no `lane` the still goes on a new top video lane over the picture instead.
LOOK
color {clip, brightness?, contrast?, saturation?, tint?, reset?} — grade.
look {clip, name} — the whole grade becomes a look: a built-in (Warm, Cool, Teal & orange, Bleach bypass, Black & white, Faded film, High contrast, Vintage) or a saved one; one undo step. An unknown name is refused with the list.
auto_color {clip} — reads the clip's middle frame as shot and sets white balance and brightness/contrast so the luma percentiles land near 0.02 and 0.98; saturation, tint, curves and LUT kept; one undo step.
wheels {clip, lift?, gamma?, gain?, reset?} — lift (blacks), gamma (mids), gain (whites): each [master, red, green, blue], -1..1, 0 neutral (or one number: master). Runs first.
curves {clip, master?, red?, green?, blue?, reset?} — each up to 16 [input, output] points in 0..1, inputs rising, joined by a monotone spline; [] straightens one. After the wheels.
lut {clip, path?, mix?, clear?} — a .cube file (1D or 3D, size up to 65) after the curves, mix 0..1 (default 1); a missing or broken file is refused. lut {relink, path} points every clip using LUT `relink` at `path`. The four color sliders run last.
transform {clip, x?, y?, scale?, rotate?, crop?:[l,r,t,b], opacity?, reset?} — position, size, crop, opacity (0 clear to 1 opaque).
transform_keys {clip, keys?:[{at, x?, y?, scale?, rotate?, opacity?, ease?}...], clear?} — animates the transform over the clip's own time (at is a clip-relative time, e.g. "0s" or "end"); clear:true takes the keys off, back to the single transform. ease (on every kind of key, the segment that leaves the key): linear (default), ease_in, ease_out, ease_in_out, hold (a step).
opacity_keys {clip, keys?:[{at, opacity, ease?}...], clear?} — opacity (0..1) over the clip's own time; the fades multiply on top.
mask_keys {clip, effect, keys?:[{at, x?, y?, w?, h?, feather?, ease?}...], clear?} — a rect or ellipse mask's position, size and soft edge over the clip's own time; effect is its stack position.
color_keys {clip, keys?:[{at, brightness?, contrast?, saturation?, tint?, ease?}...], clear?} — the grade over the clip's own time; fields a key leaves out keep the previous key's value.
fit {clip, policy: fit|fill|stretch|center} — canvas fit.
deinterlace {clip, mode: frame|field|off} — how a video clip's interlaced source is made progressive (default frame); stored on the clip, and only matters for an interlaced source (media_probe says interlaced). field (double rate) and off are stored; the render acts on frame for now.
lane_name {lane, name} — the lane's display label (empty clears it); the address (V1, A2) is unchanged, timeline shows the name.
stabilize {clip, on?, strength?=0.5, crop?=zoom|full, off?, cancel?} — takes the camera shake out of a video clip (a steady pan is kept); the analysis runs in the background, stabilize_status shows it.
reframe {clip, aspect?=fit, smoothness?=0.5} — auto reframe: follows the subject of a wide clip and writes transform keys so it fills the project canvas (a 16:9 shot in a 9:16 project); replaces the clip's transform keys and sets fit to fit; adjust with transform_keys.
SEQUENCES (several timelines in one project; the sequence tool manages them; every edit tool argument `sequence` acts on another one)
nest {clips:[addr,...], name?} — fold the picked clips into a new sequence and leave one compound clip in their place · break_apart {clips} — put a compound clip's contents back on the timeline · place_sequence {sequence, at, lane?=V1} — play a sequence as a compound clip · angle {clip, at?, angle?, audio?} — cut a multicam clip (multicam_create) at `at` and show another camera from there on. One undo step each; help {topic:"sequences"}, help {topic:"multicam"}. A compound clip takes no speed, ramp or reverse.
SOUND
eq {clip, bands:[{f,db,q,kind}] (max 10), reset?} — kind: low_shelf|peak|...
gain {clip, db (-60..24) | add_db} — the clip's own volume, on top of its lane's; audio clips only (a video clip's gain is set on its audio clip).
gain_keys {clip, keys:[[time, db, ease?],...]} — the clip's volume envelope (audio): points at timeline times inside the clip, dB linear (or eased, see ease above) between them, held beyond the ends, added to gain and fades. Replaces all points; keys:[] clears. Max 256.
gain_key {clip, at, db, ease?} — one point (a point at that frame is replaced) · gain_key_remove {clip, at} — takes out the point within a frame of at
gain_ramp {clip, from, to, to_db, from_db?} — a straight dB ramp from from to to; from_db defaults to the envelope's level at from. Points inside from..to are replaced.
gain_dip {clip, from, to, db, ramp?=0.5s} — dips to db over from..to with ramps in and out; the ramps start from the level there now and clamp to the clip. Points inside the dip are replaced. For a dip that follows speech by itself, use duck.
pan {clip, pan (-1..1)} — the audio clip's place between the speakers, -1 left, 0 middle, 1 right (a balance: the far side is turned down) · pitch {clip, semitones (-12..12)} — the audio clip's pitch in whole semitones, 12 an octave up, -12 down, 0 the original; its length and speed are kept · lane_pan {lane, pan (-1..1)} — the same for a whole audio lane, on top of the clips' own
lane_gain {lane, db (-60..12)} — level · limiter {on, ceiling_db?} — the master limiter on the output: ceiling_db -24..0 dB, the highest level the output may reach (default -1 dB)
duck {lane, under, depth_db?=-12, threshold_db?=-40, attack_ms?=80, release_ms?=400, off?} — music lane dips while the key lane speaks
dynamics {lane, gate?, nr?, compressor?, deesser?, reset?} — an audio lane's voice processors: each of gate, nr, compressor, deesser is true, false (bypass) or an object of settings; clean_voice sets the whole chain at once
LANES
lane_add {kind} · lane_remove {lane} · lane_move {lane, to} (`to` is the 1-based position; lanes are named by position, so they renumber after a move) · lane_flags {lane, hidden?, muted?, locked?}
group {clips:[addr,...]} · ungroup {clip}
CAPTIONS
subtitles_import {path, lang?} — adds the file's tracks; they have no lane until captions_place. lang (ISO 639-2, e.g. fra) overrides the guess from the file name (talk.fr.srt, fra.srt).
subtitles_lang {track, lang} — sets a subtitle track's language (ISO 639-2, e.g. fra; "und" clears it). Exports write it on the mp4/MKV track. One undo step.
captions_place {track, lane?, at?} — the whole track as one placement from its first cue: optional `lane` (default S1, made if missing) and `at` (a frame, default 0).
sub_move {clip, at, to_lane?} [group] · sub_trim {clip, edge, to} · sub_delete {clip}
cue_text {track, index, text} — retype a cue (\n = line break; empty text removes the cue).
cue_add {track, at, to, text, overwrite?, track_clock?, extend?} — add a cue to a subtitle track; at and to are timeline times (seconds exact), mapped through the placement, or the track's own clock with track_clock:true. On an unplaced track the cue is kept and nothing shows until captions_place puts the track on a caption lane, and over an existing cue unless overwrite:true, which replaces the cues it overlaps. A cue past its caption's end does not show: extend:true lengthens that caption to cover it.
captions_remove {track, force?} — remove a caption track (its name and cues) as one step. A track with placements on the timeline is refused, naming them; force:true deletes those placements with it (a grouped placement is refused: ungroup it first).
cue_delete {track, index} or {at, lane?} — remove one caption cue (as captions lists it, or the one shown at that time); it leaves every placement of its track and nothing moves.
cue_time {track, index, at?, to?} — trim or move one cue: its new start (at) and/or end (to) as timeline times; it stays inside its neighbours and at least 0.1 s long. One undo step.
cue_merge {track, index} — join a cue with the next one. cue_split {track, index, at} — divide a cue in two at a timeline time.
cue_replace {track?, find, replace, case_sensitive?} — replace a word or phrase in every cue of a track (all tracks when track is omitted); case-insensitive unless case_sensitive. Reports occurrences and cues; one undo step.
TITLES
title_add {at, duration, text, preset?=lower_third|centered|caption_bar, size?=80, outline?=thin|none|thick, boxed?=false, place?=centre|top|bottom, hplace?=centre|left|right, color?=#ffffff, weight?=regular|bold, align?=centre|left|right, box_color?=#000000 (or #rrggbbaa), box_opacity?=0.6 (0..1), fade_in?, fade_out?} — a card over the picture (black where no clip plays). place is the vertical position, hplace the side the block sits against (a lower third is place:bottom, hplace:left), align how its lines line up inside it. preset gives the app's one-click look (lower_third: bold, bottom left, boxed; centered: large, outlined, middle; caption_bar: boxed, bottom, no fades); the fields given with it win over its look, and title_set applies a preset to a title and keeps its words and timing. A look off the default saves as format v49. at and duration are times (a bare number is seconds). Fades: int = frames, "0.5s" = time. Past the last clip it extends the timeline. Named T1.. in start order.
title_set {title:"T1" or "T@45", ...any of the same fields} — merged onto the title. A title is named T1.. in start order, or by its start frame as T@45 (the form a reply prints after each title op, which stays right when titles renumber). title_remove {title} — takes it off.
GENERATED
generator_add {kind: solid|gradient|countdown|bars|tone|rectangle|ellipse|line|arrow, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?, lane?=V1, at?, duration?=5s} — a clip made from numbers, not a file, on a video lane that exists (else lane_add first). color is hex such as ff3366 or #ff3366 (solid; gradient start), color2 the gradient end, angle 0..359 degrees (0 left to right, 90 top to bottom), from the countdown's seconds (1..3600; its duration defaults to the count). Shapes (rectangle, ellipse, line, arrow) draw on a transparent canvas and land at 40% scale over the lanes below: fill and stroke are hex or none, width the stroke in pixels (0..400), radius rounds a rectangle; size and place come from the transform op. at defaults to the end of the lane. It trims, moves, grades and takes effects like any clip; it overwrites what it lands on, as a still does.
generator_set {clip, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?} — change a generator clip's params, keeping its place and length; a clip that plays a file is refused.
adjustment_add {lane?=V2, at?, duration?=5s} — an adjustment layer: no picture, its color grade and effects reach everything composed beneath it. With no lane it goes on V2, added when the timeline has only V1.
label {clip | clips, value} — the clip's colour label: a name from the palette (Violet, Iris, Caribbean, …, any case), 1..8, or none. enable {clip | clips, on} — on:false switches the clip off (it stays, but picture and sound skip it), on:true brings it back. One undo step each.
CHAPTERS
chapter_add {at, title} — a chapter marker at `at` (a time or frame), named `title` (at most 100 characters). One per frame. Chapters are the list a video platform jumps through; `timeline` prints them as a YouTube description.
chapter_set {chapter:"C@150" or "C#2", title?, at?} — renames the chapter and/or moves it to `at`. A chapter is named C@<frame>, the frame it starts on (bare digits are frames, as T@ is), or C#n, its place in the list (1 first). Moving onto a frame another chapter holds is refused.
chapter_remove {chapter} — takes the chapter off. Chapters move with the picture: a cut, a ripple or an insert moves them as it moves titles, and one inside a cut lands at the cut point.
MARKERS (comment markers, separate from chapters: a name, a note, a colour, and a length that makes a range)
marker_add {at, name, note?, color?=blue, length?} — a marker at `at`, named `name` (at most 100 characters). color is red, orange, yellow, green, blue or purple. length (a duration) makes it a range from `at`; without it the marker is a point. Several markers may share a frame.
marker_set {marker:"M@150" or "M#2", name?, note?, color?, at?, length?} — changes the marker. A marker is named M@<frame> (the first one on that frame) or M#n, its place in frame order (1 first). note:"" clears the note and length:0 turns a range back into a point.
marker_remove {marker} — takes the marker off. Markers move with the picture as chapters do (a ripple, cut or insert moves them; a range grows or shrinks with it); `timeline` lists them.
beats {clip, every?=1} — listens to a sound clip for its beat and marks every `every`th beat (1 for each beat, 2 or 4 for bars) as a yellow comment marker noted "Beat". The reply gives the tempo, the count and the M@ addresses. The clip's earlier beat markers are replaced in the same undo step. A picture-only clip, a silent one or one with no steady pulse is refused.
"#;
const UNDO_DOC: &str = "Take back the last `steps` edit batches (default 1), each as one undo step. Reports each batch taken back by its label (the batch `label`, else a summary of its ops), the undo and redo depth, and the next undo.";
const CHECKPOINT_DOC: &str = "Save the whole project under a name (letters, digits, - and _), as a copy in `<project dir>/checkpoints/<project stem>/<name>.edith`; the open project file is not touched and the project stays as it is. \
Trade-off: a checkpoint is a full copy, so it is not limited by the engine's undo history (100 steps), it survives a server restart, and each one costs a save of the whole project. Saving the same name again replaces it. With no project path yet (project_save first) the copy goes to the temp directory and is lost on reboot.";
const RESTORE_DOC: &str = "Replace the open project with checkpoint `name` from this project's checkpoint folder (any checkpoint on disk, made by this server or an earlier one): its timeline, captions, tracks and settings come back, and the project is dirty afterwards (save it to keep the restored state). \
Trade-off: restore reloads the checkpoint, so the undo and redo history is cleared and restore itself cannot be undone; a checkpoint taken before a later restore stays available.";
const CHECKPOINTS_DOC: &str = "List the checkpoints on disk in this project's checkpoint folder (<project dir>/checkpoints/<project stem>/), by name, so ones made by an earlier server run show too.";
const REDO_DOC: &str = "Redo the last `steps` batches taken back by undo (default 1). An edit clears redo history.";

/// What one op did: its report text and the clips it left at new addresses.
struct Done {
    text: String,
    touched: Vec<(Lane, u32)>,
    /// Lanes to digest even when no clip is named (a lift leaves none to name).
    lanes: Vec<Lane>,
}

impl From<Applied> for Done {
    fn from(a: Applied) -> Self {
        Done { text: a.text, touched: a.touched, lanes: Vec::new() }
    }
}

fn plain(text: String, touched: Vec<(Lane, u32)>) -> Done {
    Done { text, touched, lanes: Vec::new() }
}

// ---- schemas --------------------------------------------------------------

fn edit_schema() -> Value {
    let ops: Vec<&str> = OWN_OPS.iter().chain(FX_OPS.iter()).chain(SUB_OPS.iter()).copied().collect();
    json!({
        "type": "object",
        "properties": {
            "ops": {
                "type": "array",
                "minItems": 1,
                "description": "Ops applied in order; each is {op, ...} with the arguments listed in this tool's description.",
                "items": {
                    "type": "object",
                    "properties": { "op": { "type": "string", "enum": ops } },
                    "required": ["op"],
                    "additionalProperties": true
                }
            },
            "sequence": super::sequence::prop(),
            "dry_run": { "type": "boolean", "description": "Apply, report, then roll back. Undo and redo history are left as they were." },
            "label": { "type": "string", "maxLength": 60, "description": "A short name for the batch, e.g. \"Draft cut\"." }
        },
        "required": ["ops"]
    })
}

fn checkpoint_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "name": { "type": "string", "maxLength": 40, "description": "A checkpoint name: letters, digits, - and _." } },
        "required": ["name"]
    })
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

fn undo_schema() -> Value {
    // The type is an array, not "integer": rpc's argument check reads a single type name and would
    // refuse 1.5 in its own words. steps_arg refuses it instead, with the whole-number message.
    json!({
        "type": "object",
        "properties": { "steps": { "type": ["integer"], "minimum": 1, "description": "Batches to take back or redo (default 1)." } }
    })
}

/// Ops an agent tends to guess the argument name of: (op, the argument it takes, the guessed one).
const KEY_SWAPS: &[(&str, &str, &str)] = &[("move", "at", "to"), ("trim", "to", "at"), ("speed", "x", "speed")];

/// When an op is missing its argument and was given the guessed one from KEY_SWAPS, the
/// refusal names the right one: "missing `at` (move takes `at`, not `to`): give ...".
fn missing_key_hint(name: &str, op: &Value, err: String) -> String {
    let Some(rest) = err.strip_prefix("missing `") else { return err };
    let Some((key, tail)) = rest.split_once('`') else { return err };
    let Some(&(_, _, given)) = KEY_SWAPS.iter().find(|&&(o, k, _)| o == name && k == key) else { return err };
    if op.get(key).is_some_and(|v| !v.is_null()) || op.get(given).is_none() {
        return err;
    }
    format!("missing `{key}` ({name} takes `{key}`, not `{given}`){tail}")
}

/// The keys each op reads, its own `op` name aside. An op carrying any other key is refused
/// by name, so a misspelt one (`clipp`) cannot be silently ignored. Keep each list in step with
/// the keys its op function reads (`op.get`, `opt_time`, `lane_arg`, and the helpers they call).
/// None for a name no group answers.
fn accepted_keys(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "insert" | "overwrite" => &["source", "src", "in", "out", "at", "to", "length", "lanes"],
        "place" => &["source", "lane", "at", "in", "out", "length", "stretch", "audio", "audio_lane", "overwrite", "audio_track"],
        "append" => &["source", "in", "out", "length", "audio", "lane", "until", "audio_track"],
        "keep" => &["source", "ranges", "lane", "at", "audio", "audio_lane", "overwrite"],
        "assemble" => &["items", "lane", "at", "gap", "audio", "audio_lane", "overwrite"],
        "split" => &["at", "lanes"],
        "trim" => &["clip", "edge", "to", "scope", "ripple"],
        "roll" => &["clip", "by", "to", "edge"],
        "split_edit" => &["clip", "side", "edge", "to"],
        "slip" | "slide" => &["clip", "by"],
        "move" => &["clip", "at", "to_lane", "titles", "insert", "split_captions"],
        "copy_look" => &["from", "to", "to_lane", "what"],
        "replace" => &["clip", "source", "src", "in"],
        "nudge" => &["clips", "by"],
        "delete" | "ripple_delete" => &["clip", "lanes"],
        "lift" => &["clip"],
        "swap" => &["a", "b", "ripple"],
        "close_gaps" => &["lane"],
        "cut_regions" => &["regions", "lanes"],
        "insert_time" | "remove_time" => &["at", "duration", "lanes"],
        "freeze" => &["at", "duration", "lane", "insert"],
        "speed" => &["clip", "x", "ripple", "lanes"],
        "speed_ramp" => &["clip", "keys", "ripple", "lanes"],
        "reverse" => &["clip", "on"],
        "fade" => &["clip", "in", "out", "linked"],
        "dissolve" | "transition" => &["clip", "frames", "length", "kind", "direction", "linked"],
        "crossfade" => &["clip", "frames"],
        "visualizer" => &["clip", "on", "style", "fast"],
        "color" => &["clip", "brightness", "contrast", "saturation", "tint", "reset"],
        "lut" => &["clip", "path", "mix", "clear", "relink"],
        "wheels" => &["clip", "lift", "gamma", "gain", "reset"],
        "curves" => &["clip", "master", "red", "green", "blue", "reset"],
        "look" => &["clip", "name"],
        "auto_color" => &["clip"],
        "transform" => &["clip", "x", "y", "corner", "margin", "scale", "rotate", "crop", "reset", "opacity"],
        "transform_keys" => &["clip", "keys", "clear"],
        "opacity_keys" | "color_keys" => &["clip", "keys", "clear"],
        "mask_keys" => &["clip", "effect", "keys", "clear"],
        "fit" => &["clip", "policy"],
        "eq" => &["clip", "bands", "reset"],
        "gain" => &["clip", "db", "add_db"],
        "gain_keys" => &["clip", "keys"],
        "gain_key" => &["clip", "at", "db", "ease"],
        "gain_key_remove" => &["clip", "at"],
        "gain_ramp" => &["clip", "from", "to", "to_db", "from_db"],
        "gain_dip" => &["clip", "from", "to", "db", "ramp"],
        "lane_gain" => &["lane", "db"],
        "pan" => &["clip", "pan"],
        "pitch" => &["clip", "semitones"],
        "lane_pan" => &["lane", "pan"],
        "limiter" => &["on", "ceiling_db"],
        "duck" => &["lane", "under", "depth_db", "threshold_db", "attack_ms", "release_ms", "off"],
        "dynamics" => &["lane", "gate", "nr", "compressor", "deesser", "reset"],
        "stabilize" => &["clip", "on", "strength", "crop", "off", "cancel"],
        "reframe" => &["clip", "aspect", "smoothness"],
        "nest" => &["clips", "name"],
        "break_apart" => &["clips"],
        "angle" => &["clip", "at", "angle", "audio"],
        "place_sequence" => &["sequence", "at", "lane"],
        "lane_add" => &["kind"],
        "lane_remove" => &["lane"],
        "lane_move" => &["lane", "to"],
        "lane_flags" => &["lane", "hidden", "muted", "locked"],
        "lane_name" => &["lane", "name"],
        "deinterlace" => &["clip", "mode"],
        "generator_add" => &["kind", "color", "color2", "angle", "from", "fill", "stroke", "width", "radius", "hz", "level", "lane", "at", "duration"],
        "generator_set" => &["clip", "color", "color2", "angle", "from", "fill", "stroke", "width", "radius"],
        "adjustment_add" => &["lane", "at", "duration"],
        "label" => &["clip", "clips", "value"],
        "enable" => &["clip", "clips", "on"],
        "group" => &["clips"],
        "ungroup" => &["clip"],
        "subtitles_import" => &["path", "lang"],
        "title_add" => &["at", "duration", "text", "preset", "size", "outline", "boxed", "place", "hplace", "color", "colour", "weight", "align", "box_color", "box_colour", "box_opacity", "fade_in", "fade_out", "in_anim", "out_anim", "anim_secs"],
        "title_set" => &["title", "at", "duration", "text", "preset", "size", "outline", "boxed", "place", "hplace", "color", "colour", "weight", "align", "box_color", "box_colour", "box_opacity", "fade_in", "fade_out", "in_anim", "out_anim", "anim_secs"],
        "title_remove" => &["title"],
        "chapter_add" => &["at", "title"],
        "chapter_set" => &["chapter", "title", "at"],
        "chapter_remove" => &["chapter"],
        "marker_add" => &["at", "name", "note", "color", "length"],
        "marker_set" => &["marker", "name", "note", "color", "at", "length"],
        "marker_remove" => &["marker"],
        "beats" => &["clip", "every"],
        "captions_place" => &["track", "lane", "at"],
        "sub_move" => &["clip", "to_lane", "at"],
        "sub_trim" => &["clip", "edge", "to"],
        "sub_delete" => &["clip"],
        "cue_text" => &["track", "index", "text"],
        "cue_add" => &["track", "at", "to", "text", "overwrite", "track_clock", "extend"],
        "cue_replace" => &["track", "find", "replace", "case_sensitive"],
        "cue_delete" => &["track", "index", "at", "lane"],
        "cue_time" => &["track", "index", "at", "to"],
        "cue_merge" => &["track", "index"],
        "cue_split" => &["track", "index", "at"],
        "subtitles_lang" => &["track", "lang"],
        "captions_remove" => &["track", "force"],
        _ => return None,
    })
}

/// Refuses an op that carries a key it does not read, naming the keys it takes and the
/// nearest one when the key looks like a misspelling of it.
fn check_keys(name: &str, op: &Value) -> Result<(), String> {
    let (Some(accepted), Some(obj)) = (accepted_keys(name), op.as_object()) else {
        return Ok(());
    };
    for key in obj.keys().filter(|k| k.as_str() != "op") {
        if accepted.contains(&key.as_str()) {
            continue;
        }
        // A guessed name for an argument the op is missing (`to` for move's `at`): missing_key_hint
        // names the right one when the op fails for that argument.
        if KEY_SWAPS.iter().any(|&(o, real, given)| o == name && given == key && !obj.contains_key(real)) {
            continue;
        }
        let near = accepted
            .iter()
            .map(|&k| (edit_distance(key, k), k))
            .filter(|&(d, k)| d * 3 <= key.len().max(k.len()))
            .min();
        let hint = near.map_or(String::new(), |(_, k)| format!(" (did you mean `{k}`?)"));
        return Err(format!("unknown key `{key}`; {name} takes {}{hint}", accepted.join(", ")));
    }
    Ok(())
}

/// Levenshtein distance between two keys, for the "did you mean" hint.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool { name: "edit", description: EDIT_DOC, schema: edit_schema, run: run_edit },
        Tool { name: "undo", description: UNDO_DOC, schema: undo_schema, run: run_undo },
        Tool { name: "redo", description: REDO_DOC, schema: undo_schema, run: run_redo },
        Tool { name: "checkpoint", description: CHECKPOINT_DOC, schema: checkpoint_schema, run: run_checkpoint },
        Tool { name: "restore", description: RESTORE_DOC, schema: checkpoint_schema, run: run_restore },
        Tool { name: "checkpoints", description: CHECKPOINTS_DOC, schema: empty_schema, run: run_checkpoints },
    ]
}

// ---- argument helpers -----------------------------------------------------

pub(super) fn need(op: &Value, key: &str, fps: f64, end: u32) -> Result<u32, String> {
    opt_time(op, key, fps, end)?.ok_or_else(|| format!("missing `{key}`: give a time such as 450f, 15.0s or end"))
}

pub(super) fn opt_time(op: &Value, key: &str, fps: f64, end: u32) -> Result<Option<u32>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => parse::frame(v, fps, end).map(Some).map_err(|e| format!("`{key}`: {e}")),
    }
}

/// A length: an integer is frames, a string is a time (`12f`, `0.5s`).
pub(super) fn length(op: &Value, key: &str, fps: f64) -> Result<Option<u32>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_u64() => {
            let n = v.as_u64().unwrap_or(0);
            u32::try_from(n).map(Some).map_err(|_| format!("`{key}`: {n} frames is too many"))
        }
        Some(v) => parse::frame(v, fps, u32::MAX).map(Some).map_err(|e| format!("`{key}`: {e}")),
    }
}

fn lane_arg(op: &Value, key: &str) -> Result<Option<Lane>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => parse::lane(t).map(Some),
        Some(v) => Err(format!("`{key}` must be a lane name such as \"V1\" or \"A1\", got {}", parse::kind_of(v))),
    }
}

fn lane_list(op: &Value, key: &str) -> Result<Option<Vec<Lane>>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .ok_or_else(|| format!("`{key}` lists lane names such as V1"))
                    .and_then(parse::lane)
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("`{key}` must be a list of lane names such as [\"V1\", \"A1\"]")),
    }
}

/// An address (`V1@450`, `V1#0`) resolved against the timeline's lanes. A lane the project does not
/// have is named as such, with the lanes it has; `parse::clip` would call it an empty lane.
pub(super) fn clip_addr(s: &PlaybackSession, raw: &str) -> Result<(Lane, usize), String> {
    let a = raw.trim();
    if let Some(split) = a.find(['@', '#']) {
        let lane = parse::lane(&a[..split])?;
        if !s.lanes().contains(&lane) {
            let names: Vec<String> = s.lanes().into_iter().map(parse::lane_name).collect();
            return Err(format!(
                "{} is not a lane in this project; the lanes are {}",
                parse::lane_name(lane),
                names.join(", ")
            ));
        }
    }
    parse::clip(s, raw)
}

fn clip_arg(s: &PlaybackSession, op: &Value) -> Result<(Lane, usize), String> {
    let addr = parse::addr_str(op, "clip", "\"V1@0\"")?;
    let (lane, idx) = clip_addr(s, addr)?;
    if lane.kind == LaneKind::Subtitle {
        return Err(format!("{addr} is a caption lane; these ops edit media clips, not captions"));
    }
    Ok((lane, idx))
}

/// `V1@450` for the clip at `idx`, or `V1#idx` when there is no such clip.
fn addr_of(s: &PlaybackSession, lane: Lane, idx: usize) -> String {
    match s.lane_clips(lane).get(idx) {
        Some(c) => parse::addr(lane, c.start),
        None => format!("{}#{idx}", parse::lane_name(lane)),
    }
}

/// A source index from `src:N`, `N`, or a number. Must be on the timeline.
fn source_arg(s: &PlaybackSession, op: &Value) -> Result<usize, String> {
    let n = match op.get("source") {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|n| n as usize)
            .ok_or("`source` must be a whole number or src:N")?,
        Some(Value::String(t)) => {
            let t = t.trim();
            let t = t.strip_prefix("src:").unwrap_or(t).trim_start();
            let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
            digits
                .parse::<usize>()
                .map_err(|_| format!("`source` `{t}` is not a source: write src:N"))?
        }
        _ => return Err("missing `source`: give src:N, the number the timeline lists its media by".into()),
    };
    let count = s.sources().len();
    if n >= count {
        let have = if count == 0 {
            "no media is imported yet (media_import adds it)".to_string()
        } else {
            format!("media_list shows src:0–{}", count - 1)
        };
        return Err(format!("there is no src:{n} ({have})"));
    }
    Ok(n)
}

fn source_name(s: &PlaybackSession, n: usize) -> String {
    s.sources()
        .get(n)
        .and_then(|src| src.path.file_name())
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- time bases ----------------------------------------------------------

/// A source's own frames against the timeline's. The engine keeps a clip's
/// `in_frame`/`out_frame` in timeline-rate frames (a 30 fps file on a 25 fps
/// timeline is conformed), so source positions are converted here with the
/// pair `engine::project::Rate` uses: native frame `n` is first shown at
/// timeline frame `ceil(n * tl / src)`, and timeline frame `c` shows native
/// frame `floor(c * src / tl)`.
#[derive(Clone, Copy)]
struct Base {
    /// Frames per second in millionths, so 23.976 stays within a micro-frame of exact.
    src: u128,
    tl: u128,
    /// The file's real duration in seconds (`f64::MAX` when unknown): a frame that rounds past
    /// the last frame is shown at the end, not after it.
    dur: f64,
}

impl Base {
    fn of(s: &PlaybackSession, path: &Path) -> Self {
        let micro = |fps: f64| (fps * 1e6).round().max(1.0) as u128;
        let dur = engine::probe_media(path).duration_secs.filter(|d| *d > 0.0).unwrap_or(f64::MAX);
        Base { src: micro(s.file_fps(path)), tl: micro(parse::session_fps(s)), dur }
    }

    fn src_fps(self) -> f64 {
        self.src as f64 / 1e6
    }

    /// Timeline-rate frame to the source frame it shows.
    fn native(self, c: u32) -> u32 {
        (u128::from(c) * self.src / self.tl) as u32
    }

    /// Source frame to the first timeline-rate frame that shows it.
    fn conform(self, n: u32) -> u32 {
        (u128::from(n) * self.tl).div_ceil(self.src) as u32
    }

    /// A source frame as a source time, `2.000s`, capped at the file's real duration: the reply's
    /// source ranges are source time, so they are never printed as timeline frames.
    fn pos(self, native: u32) -> String {
        self.secs_text(f64::from(native) / self.src_fps())
    }

    /// A source time in seconds, `2.000s`, capped at the file's real duration.
    fn secs_text(self, secs: f64) -> String {
        format!("{:.3}s", secs.min(self.dur))
    }

    /// A source range `src 0.000s–2.000s`, in source time, for the timeline frames `a..z`. The same range
    /// the timeline prints (look.rs `src_range_secs`): it starts at the source frame the in shows, and the
    /// end is the source frame after the last one shown, over the file's own rate, so 48 frames of a
    /// 23.976 fps file read `0.0s–2.002s`.
    fn span(self, a: u32, z: u32) -> String {
        let start = f64::from(self.native(a)) / self.src_fps();
        let end = f64::from(self.native(z.saturating_sub(1)) + 1) / self.src_fps();
        format!("src {}–{}", self.secs_text(start), self.secs_text(end))
    }
}

/// A source-time argument (`in`, `out`, a range end, an item's bound): seconds
/// are the file's own, `450f` is the file's own frames. Returns the
/// timeline-rate frame the engine stores and the rounding note, if any.
///
/// The snap happens once. Seconds round to the nearest source frame (ties up:
/// `0.5s` at 25 fps is 12.5, so frame 13, 0.520s) and the first timeline frame
/// that shows that source frame is stored, so the stored frame reads back as the
/// source time the reply prints, with no second rounding. Same-rate sources keep
/// the frame-count rounding they always had.
fn source_time(v: &Value, b: Base, conformed_end: u32, key: &str) -> Result<(u32, String), String> {
    if v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("end")) {
        return Ok((conformed_end, String::new()));
    }
    if let (Some(secs), true) = (seconds_arg(v), b.src != b.tl) {
        // The +1e-6 keeps a tie that float arithmetic put just under .5 on the upper frame.
        let n = (secs * b.src_fps() + 0.5 + 1e-6).floor() as u32;
        let c = b.conform(n);
        // A timeline frame shows one source frame; when the source is faster the source
        // frame `n` may be skipped, and the frame that shows its neighbour is what is stored.
        let shown = b.native(c);
        if (f64::from(shown) / b.src_fps() - secs).abs() < 1e-6 {
            return Ok((c, String::new()));
        }
        let shown_secs = format!("{}s", trim_secs(secs));
        return Ok((c, format!(" ({shown_secs} → src {shown}f {})", b.pos(shown))));
    }
    let n = parse::frame(v, b.src_fps(), u32::MAX).map_err(|e| format!("`{key}`: {e}"))?;
    Ok((b.conform(n), exact_note(v, b.src_fps(), n, " src")))
}

/// The seconds a time argument names, when it is seconds (a bare number or `1.5s`);
/// None for frames, timecodes, `end` or anything else.
fn seconds_arg(v: &Value) -> Option<f64> {
    let secs = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(t) => {
            let t = t.trim();
            if t.ends_with(['f', 'F']) || t.contains(':') {
                return None;
            }
            t.strip_suffix(['s', 'S']).unwrap_or(t).trim().parse::<f64>().ok()?
        }
        _ => return None,
    };
    (secs.is_finite() && secs >= 0.0).then_some(secs)
}

/// A source time as seconds of the file, from seconds, frames or a timecode (at the file's rate).
fn source_secs(v: &Value, b: Base) -> Option<f64> {
    seconds_arg(v).or_else(|| parse::frame(v, b.src_fps(), u32::MAX).ok().map(|n| f64::from(n) / b.src_fps()))
}

/// The timeline frame a source range `[in, out)` ends at. Each end is still snapped to a source frame
/// on its own (`conformed` is the out's own snap, used for `end` and when a bound is not readable), but
/// the length is the range's duration in timeline frames: `round((out - in) × timeline fps)`. So 0s to
/// 12s is 360f at 30 fps from a 23.976 source, where the two snapped ends alone would give 361f.
fn range_end(in_v: Option<&Value>, out_v: &Value, b: Base, fps: f64, frames: u32, in_f: u32, conformed: u32) -> u32 {
    if out_v.as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case("end")) {
        return conformed;
    }
    let Some(out_s) = source_secs(out_v, b) else {
        return conformed;
    };
    let in_s = match in_v {
        None | Some(Value::Null) => 0.0,
        Some(v) => match source_secs(v, b) {
            Some(s) => s,
            None => return conformed,
        },
    };
    let len = ((out_s - in_s) * fps).round();
    if !(len.is_finite() && len >= 1.0) {
        return conformed;
    }
    // Past the file's end the snapped end stands, so the range is refused as it was before.
    let end = in_f.saturating_add(len as u32);
    if end > frames { conformed } else { end }
}

/// `0.5` as `0.5`, `2` as `2`, without trailing zeros.
fn trim_secs(secs: f64) -> String {
    let s = format!("{secs:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn opt_source_time(op: &Value, key: &str, b: Base, conformed_end: u32) -> Result<Option<(u32, String)>, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => source_time(v, b, conformed_end, key).map(Some),
    }
}

/// `" (1.53s → 38f)"` when a seconds value does not land on a frame of `fps`,
/// naming the frame it was rounded to. Empty for frames, timecodes and exact seconds.
fn exact_note(v: &Value, fps: f64, frame: u32, unit: &str) -> String {
    let (secs, shown) = match v {
        Value::Number(n) => (n.as_f64(), format!("{n}s")),
        Value::String(t) => {
            let t = t.trim();
            if t.ends_with(['f', 'F']) || t.contains(':') {
                return String::new();
            }
            let bare = t.strip_suffix(['s', 'S']).unwrap_or(t).trim();
            (bare.parse::<f64>().ok(), format!("{bare}s"))
        }
        _ => return String::new(),
    };
    let Some(secs) = secs.filter(|x| x.is_finite() && *x >= 0.0) else {
        return String::new();
    };
    let exact = secs * fps;
    if (exact - exact.round()).abs() < 1e-6 {
        return String::new();
    }
    format!(" ({shown} → {frame}f{unit})")
}

/// A trim's asked time, shown once with where it landed: `asked 6.070s → 182f (6.067s)` when the asked
/// seconds do not land on a frame, None when they do (the plain landing is then the whole reply).
fn asked_arrow(asked: Option<&Value>, fps: f64, landed: u32) -> Option<String> {
    let note = exact_note(asked?, fps, landed, "");
    if note.is_empty() {
        return None;
    }
    let inner = note.trim().trim_start_matches('(').trim_end_matches(')');
    Some(format!("asked {inner} ({:.3}s)", parse::secs(landed, fps)))
}

/// Rounding notes for the timeline times an op took (its `at`, `to`, `frames`,
/// `gap`; a fade's `in` and `out`), appended to the op's line.
fn time_notes(name: &str, op: &Value, fps: f64) -> String {
    // A trim words its asked time itself (`asked … → 182f (6.067s)`), so it gets no note here.
    if name == "trim" {
        return String::new();
    }
    let keys: &[&str] = if name == "fade" { &["in", "out"] } else { &["at", "to", "frames", "gap"] };
    let mut out = String::new();
    for key in keys {
        let Some(v) = op.get(*key) else { continue };
        // A whole JSON number is a frame count for the length keys, seconds for `at`/`to`.
        if v.is_u64() && matches!(*key, "frames" | "gap" | "in" | "out") {
            continue;
        }
        if let Ok(f) = parse::frame(v, fps, u32::MAX) {
            out.push_str(&exact_note(v, fps, f, ""));
        }
    }
    out
}

// ---- locks and overlaps ---------------------------------------------------

fn lock_msg(l: Lane) -> String {
    let n = parse::lane_name(l);
    format!("lane {n} is locked: unlock it first with lane_flags {{lane:\"{n}\", locked:false}}")
}

/// Refuses an edit that would touch a locked lane. The engine does not check
/// locks itself, so every edit that changes a lane is checked here first.
pub(super) fn check_unlocked(s: &PlaybackSession, lanes: &[Lane]) -> Result<(), String> {
    match lanes.iter().find(|&&l| s.lane_locked(l)) {
        Some(&l) => Err(lock_msg(l)),
        None => Ok(()),
    }
}

/// Every clip on `lane` that `start..end` runs into, in the words a refusal uses:
/// `V1@105 (3.500–4.000)`. `moving` holds the (lane, start) of clips that are travelling;
/// they do not count against their own new spot.
fn overlapping(s: &PlaybackSession, lane: Lane, start: u32, end: u32, moving: &[(Lane, u32)], fps: f64) -> Vec<String> {
    s.lane_clips(lane)
        .iter()
        .filter(|c| !moving.contains(&(lane, c.start)) && c.start < end && start < c.end())
        .map(|c| {
            format!(
                "{} ({:.3}–{:.3})",
                parse::addr(lane, c.start),
                parse::secs(c.start, fps),
                parse::secs(c.end(), fps)
            )
        })
        .collect()
}

/// The media clips of the take `(lane, idx)` belongs to, the clicked one first.
/// Just the clicked clip when it is not linked.
fn take_of(s: &PlaybackSession, lane: Lane, idx: usize) -> Vec<(Lane, usize)> {
    let mut out = vec![(lane, idx)];
    let Some(link) = s.lane_clips(lane).get(idx).and_then(|c| c.link) else {
        return out;
    };
    for l in s.lanes() {
        if l.kind == LaneKind::Subtitle {
            continue;
        }
        for (i, c) in s.lane_clips(l).iter().enumerate() {
            if c.link == Some(link) && (l, i) != (lane, idx) {
                out.push((l, i));
            }
        }
    }
    out
}

fn media_lanes(s: &PlaybackSession) -> Vec<Lane> {
    s.lanes().into_iter().filter(|l| l.kind != LaneKind::Subtitle).collect()
}

// ---- placement ------------------------------------------------------------

fn clip_of(source: usize, in_frame: u32, out_frame: u32) -> Clip {
    Clip {
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        start: 0,
        in_frame,
        out_frame,
        source,
        link: None,
        eq: None,
        color: None,
        transform: None,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
    }
}

/// A refusal for a lane the timeline does not have: lists the lanes it has and the
/// `lane_add` that makes the missing one.
fn missing_lane(s: &PlaybackSession, l: Lane) -> String {
    let have: Vec<String> = s.lanes().into_iter().map(parse::lane_name).collect();
    let kind = match l.kind {
        LaneKind::Video => "video",
        LaneKind::Audio => "audio",
        LaneKind::Subtitle => "subtitle",
    };
    format!(
        "lane {} is not on this timeline; the lanes are {}. Add it first with {{\"op\":\"lane_add\",\"kind\":\"{kind}\"}}",
        parse::lane_name(l),
        have.join(", ")
    )
}

/// Where a placed range lands: the timeline frame it starts at, the lane its picture takes, the
/// lane its sound takes, and whether it may overwrite what is there.
#[derive(Clone, Copy)]
struct Dest {
    at: u32,
    lane: Option<Lane>,
    audio: Option<Lane>,
    overwrite: bool,
}

/// Places source `n`'s `in_f..out_f` at timeline frame `at`. With no lane it
/// lands as a linked V1+A1 pair (whichever halves the file has), refused on
/// overlap. With a video lane it overwrites that lane, and when the source has
/// sound its sound half goes on `audio` (see `audio_arg`), grouped with it and
/// refused on overlap. With any other lane it overwrites that one lane.
/// Returns the new addresses. An occupied lane refuses unless `overwrite`; with it the clips
/// the placement lands on are named in `replaced`.
/// Places a range of source `n` at `dest.at` (see [`place_range_body`]), and says what an overwrite
/// did to what was there: each clip cut, trimmed or removed, with the ranges it is left with.
fn place_range(
    s: &mut PlaybackSession,
    n: usize,
    in_f: u32,
    out_f: u32,
    dest: Dest,
    speed: Speed,
    replaced: &mut Vec<String>,
) -> Result<Vec<(Lane, u32)>, String> {
    let Dest { at, .. } = dest;
    let before: Vec<(Lane, Vec<Clip>)> = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .map(|l| (l, s.lane_clips(l).to_vec()))
        .collect();
    let out = place_range_body(s, n, in_f, out_f, dest, speed)?;
    let fps = parse::session_fps(s);
    // Only the lanes the placement landed on are reported, each over the span its own clip covers:
    // a clip on another lane that the span merely overlaps is not touched, so it is not named.
    for &(l, _) in &out {
        let end = s.lane_clips(l).iter().filter(|c| c.start == at && c.source == n).map(|c| c.end()).max();
        if let Some(end) = end {
            let landed: Vec<(Lane, Vec<Clip>)> = before.iter().filter(|(b, _)| *b == l).cloned().collect();
            replaced.extend(overwrite_notes(&landed, at, end, fps));
        }
    }
    Ok(out)
}

/// What a placed span `[at, end)` did to the clips it covered, per lane, read from the lanes
/// before the placement: a clip cut at its head keeps its address, a tail kept past the span is
/// named by its new address, and a clip wholly inside the span is removed.
fn overwrite_notes(before: &[(Lane, Vec<Clip>)], at: u32, end: u32, fps: f64) -> Vec<String> {
    let mut out = Vec::new();
    for (lane, clips) in before {
        for c in clips.iter().filter(|c| c.start < end && at < c.end()) {
            let addr = parse::addr(*lane, c.start);
            let head = c.start < at;
            let tail = c.end() > end;
            let span = |a: u32, b: u32| format!("{:.3}–{:.3}", parse::secs(a, fps), parse::secs(b, fps));
            out.push(match (head, tail) {
                (true, true) => format!(
                    "{addr} cut to {}; {} kept as {}",
                    span(c.start, at),
                    span(end, c.end()),
                    parse::addr(*lane, end)
                ),
                (true, false) => format!("{addr} cut to {}", span(c.start, at)),
                (false, true) => format!("{addr} cut to {} (kept as {})", span(end, c.end()), parse::addr(*lane, end)),
                (false, false) => format!("{addr} removed ({})", span(c.start, c.end())),
            });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn place_range_body(
    s: &mut PlaybackSession,
    n: usize,
    in_f: u32,
    out_f: u32,
    dest: Dest,
    speed: Speed,
) -> Result<Vec<(Lane, u32)>, String> {
    let Dest { at, lane, audio, overwrite } = dest;
    let path: PathBuf = s.sources()[n].path.clone();
    let fps = parse::session_fps(s);
    let frames = source_frames(s, n, lane, fps);
    let b = Base::of(s, &path);
    if out_f <= in_f || out_f > frames {
        return Err(format!(
            "src:{n} {} plays {} to {}; the range {} to {} is not inside it",
            source_name(s, n),
            b.pos(0),
            b.pos(b.native(frames)),
            b.pos(b.native(in_f)),
            b.pos(b.native(out_f)),
        ));
    }
    // The picture is what the file's header says it has, not its extension: an mp4 may hold sound alone.
    let picture = engine::probe_media(&path).video.is_some() || engine::is_image(&path);
    let sound = !AudioSession::probe_streams(&path)
        .map_err(|e| format!("src:{n}: cannot read its sound: {e}"))?
        .is_empty();
    // A picture and its sound placed as one take run the same length. The sound's length is in the
    // same timeline frames as the picture's, and the longer half is cut to the shorter: left alone,
    // the engine would clamp only the sound to its own wall, leaving the two a frame or more apart.
    let take = picture
        && sound
        && match lane
    {
        None => true,
        Some(l) => l.kind == LaneKind::Video && audio.is_some(),
    };
    let out_f = match sound_frames(s, n, fps) {
        Some(wall) if take && wall < out_f => {
            if wall <= in_f {
                return Err(format!(
                    "src:{n} has {} of sound, so the range from {} starts past its end: give an earlier in, or place the picture alone with audio:false",
                    secs_text(wall, fps),
                    secs_text(in_f, fps)
                ));
            }
            wall
        }
        _ => out_f,
    };
    // The clip as it will sit: a stretched placement is placed already at its speed, so its
    // timeline span (what the overlap checks and the landed check measure) is the stretched one.
    let proto = Clip { speed, ..clip_of(n, in_f, out_f) };

    match lane {
        Some(l) => {
            match l.kind {
                LaneKind::Video if !picture => {
                    return Err(format!("src:{n} has no picture; place its sound on an audio lane (A1..)"))
                }
                LaneKind::Audio if !sound => {
                    return Err(format!("src:{n} has no sound; place it on a video lane (V1..)"))
                }
                LaneKind::Subtitle => return Err("a source cannot go on a caption lane (S1..)".into()),
                _ => {}
            }
            if !s.lanes().contains(&l) {
                return Err(missing_lane(s, l));
            }
            // A video lane with sound also takes the sound half on `audio`, grouped with the
            // picture as the no-lane path does, so the clip is not silent and can be ducked.
            let pair = audio.filter(|_| l.kind == LaneKind::Video && sound);
            let mut lanes = vec![l];
            if let Some(a) = pair {
                if !s.lanes().contains(&a) {
                    return Err(missing_lane(s, a));
                }
                lanes.push(a);
            }
            check_unlocked(s, &lanes)?;
            // An occupied lane refuses, naming the clip in the way, so an explicit `lane` never
            // silently replaces clips; overwrite:true replaces them and the reply lists them.
            let len = speed.frames(out_f - in_f);
            // Every collision is named, the picture's and the sound's, so one refusal shows them all.
            let pic_hit = overlapping(s, l, at, at + len, &[], fps);
            let snd_hit: Vec<String> = pair.map(|a| overlapping(s, a, at, at + len, &[], fps)).unwrap_or_default();
            if !overwrite && (!pic_hit.is_empty() || !snd_hit.is_empty()) {
                let fix = "give overwrite:true to replace what is there, put the picture alone with audio:false, or give another audio_lane";
                // The fix leads each refusal: the reader sees what to do before what is in the way.
                return Err(match (pic_hit.is_empty(), pair) {
                    // Only the sound collides.
                    (true, Some(a)) => format!(
                        "to place src:{n} here: put the picture alone with audio:false, give another audio_lane, or lift the clip in the way. Its sound on {} at {} would overlap {}",
                        parse::lane_name(a),
                        parse::tc(at, fps),
                        snd_hit.join(", ")
                    ),
                    // Only the picture collides.
                    (false, _) if snd_hit.is_empty() => format!(
                        "{} at {} is taken: src:{n} would overwrite {}. Give overwrite:true to replace what is there, or another at or lane",
                        parse::lane_name(l),
                        parse::tc(at, fps),
                        pic_hit.join(", ")
                    ),
                    // Both collide: every collision is named.
                    _ => {
                        let a = pair.map(parse::lane_name).unwrap_or_default();
                        format!(
                            "to place src:{n} here: {fix}. {} at {} would overlap {}; {a} at {} would overlap {}",
                            parse::lane_name(l),
                            parse::tc(at, fps),
                            pic_hit.join(", "),
                            parse::tc(at, fps),
                            snd_hit.join(", ")
                        )
                    }
                });
            }
            if !s.place_at(l, parse::secs(at, fps), proto.clone()) {
                return Err(format!("{} at {} refused: the engine would not place it there", parse::lane_name(l), parse::tc(at, fps)));
            }
            // The engine clamps a clip to the picture's length, so a sound longer than its picture
            // lands short. That is refused by name (the batch rolls back) rather than reported as placed.
            let landed = s.lane_clips(l).iter().find(|c| c.start == at && c.source == n).map(|c| c.frames());
            if landed != Some(len) {
                return Err(format!(
                    "{} at {}: src:{n} lands {} long, not the {} asked for: the engine reads this source's length from its picture ({}), so its sound cannot be placed {} long on {} yet; a shorter length or a video lane for it",
                    parse::lane_name(l),
                    parse::tc(at, fps),
                    secs_text(landed.unwrap_or(0), fps),
                    secs_text(len, fps),
                    secs_text(s.file_frames(&path), fps),
                    secs_text(len, fps),
                    parse::lane_name(l)
                ));
            }
            if let Some(a) = pair {
                if !s.place_at(a, parse::secs(at, fps), proto.clone()) {
                    return Err(format!("{} at {} refused: the engine would not place the sound there", parse::lane_name(a), parse::tc(at, fps)));
                }
                let picks: Vec<(Lane, usize)> = lanes
                    .iter()
                    .filter_map(|&x| s.lane_clips(x).iter().position(|c| c.start == at && c.source == n).map(|i| (x, i)))
                    .collect();
                if picks.len() > 1 {
                    s.group_all(&picks).map_err(|e| e.to_string())?;
                }
            }
            Ok(lanes.iter().map(|&x| (x, at)).collect())
        }
        None => {
            let mut items: Vec<(Lane, Clip)> = Vec::new();
            if picture {
                items.push((Lane::V1, proto.clone()));
            }
            if sound {
                items.push((Lane::A1, proto.clone()));
            }
            if items.is_empty() {
                return Err(format!("src:{n} has neither picture nor sound"));
            }
            let lanes: Vec<Lane> = items.iter().map(|(l, _)| *l).collect();
            check_unlocked(s, &lanes)?;
            // Each half goes down on its own lane at `at`, which a lane places exactly, even
            // past the end (the gap is kept). Refused before anything is written on overlap.
            let len = speed.frames(out_f - in_f);
            for &l in &lanes {
                if !s.lanes().contains(&l) {
                    return Err(missing_lane(s, l));
                }
                let hit = overlapping(s, l, at, at + len, &[], fps);
                if hit.is_empty() {
                    continue;
                }
                if !overwrite {
                    return Err(if l.kind == LaneKind::Audio && picture {
                        format!(
                            "src:{n}'s sound does not fit on {} at {}: it would overlap {}. Put the picture alone with audio:false, or give another at",
                            parse::lane_name(l),
                            parse::tc(at, fps),
                            hit.join(", ")
                        )
                    } else {
                        format!(
                            "src:{n} does not fit at {}: it would overlap {}; split or lift there first, give `lane`, or overwrite:true",
                            parse::tc(at, fps),
                            hit.join(", ")
                        )
                    });
                }
            }
            for (l, clip) in items {
                if !s.place_at(l, parse::secs(at, fps), clip) {
                    return Err(format!("src:{n} does not fit at {}: the engine refused the placement at that frame", parse::tc(at, fps)));
                }
            }
            // A pair placed without a lane is one take, as the GUI drops it: linked, so
            // trim, move and delete carry the picture and the sound together.
            let picks: Vec<(Lane, usize)> = lanes
                .iter()
                .filter_map(|&l| s.lane_clips(l).iter().position(|c| c.start == at && c.source == n).map(|i| (l, i)))
                .collect();
            if picks.len() > 1 {
                s.group_all(&picks).map_err(|e| e.to_string())?;
            }
            Ok(lanes.iter().map(|&l| (l, at)).collect())
        }
    }
}

/// The audio lane a placement's sound half goes on: `audio_lane` (default A1) when
/// the placement is on a video lane and `audio` is not false; None otherwise. The
/// audio options only mean something with a video `lane`, so they are refused elsewhere.
fn audio_arg(op: &Value, lane: Option<Lane>) -> Result<Option<Lane>, String> {
    let audio = match op.get("audio") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("`audio` must be true or false".into()),
    };
    let given = lane_arg(op, "audio_lane")?;
    if let Some(a) = given
        && a.kind != LaneKind::Audio
    {
        return Err(format!("`audio_lane` must be an audio lane such as A1, not {}", parse::lane_name(a)));
    }
    if !matches!(lane, Some(l) if l.kind == LaneKind::Video) {
        if audio.is_some() || given.is_some() {
            return Err("`audio` and `audio_lane` apply to the sound of a video lane placement: give `lane` a video lane such as V1, or drop them".into());
        }
        return Ok(None);
    }
    if audio == Some(false) {
        if given.is_some() {
            return Err("`audio_lane` and `audio:false` contradict each other: drop one".into());
        }
        return Ok(None);
    }
    Ok(Some(given.unwrap_or(Lane::A1)))
}

/// The audio tracks of a file as `(track, language)`, numbered the way `audio_track` takes them:
/// 0-based, file order among the audio tracks (the numbers media_probe lists). `und` where the file names none.
pub(crate) fn audio_tracks(path: &Path) -> Vec<(usize, String)> {
    engine::probe_media(path)
        .streams
        .iter()
        .filter(|st| st.kind == engine::demux::StreamKind::Audio)
        .map(|st| (st.index, st.language.clone().unwrap_or_else(|| "und".to_string())))
        .collect()
}

/// `audio_track` as the track number the op gave; `None` when it gives none.
fn audio_track_arg(op: &Value) -> Result<Option<usize>, String> {
    match op.get("audio_track") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_u64()
            .map(|t| Some(t as usize))
            .ok_or_else(|| "`audio_track` must be a whole number: the 0-based audio track media_probe lists".into()),
        Some(_) => Err("`audio_track` must be a whole number: the 0-based audio track media_probe lists, e.g. 1".into()),
    }
}

/// Checks that `src:n`'s file has audio track `track`, returning its language. The refusal names the
/// tracks the file has, so the fix is to read them off it.
fn track_language(path: &Path, n: usize, track: usize) -> Result<String, String> {
    let tracks = audio_tracks(path);
    if let Some((_, lang)) = tracks.iter().find(|(t, _)| *t == track) {
        return Ok(lang.clone());
    }
    if tracks.is_empty() {
        return Err(format!("audio_track:{track}: src:{n} has no audio track"));
    }
    // One track is named by its parts, so its parentheses do not nest in the list form.
    if let [(t, l)] = tracks.as_slice() {
        return Err(format!("audio_track:{track}: src:{n} has one audio track ({t}, {l})"));
    }
    let list: Vec<String> = tracks.iter().map(|(t, l)| format!("{t} ({l})")).collect();
    Err(format!("audio_track:{track}: src:{n} has audio tracks {}", list.join(", ")))
}

/// Takes `path`'s audio track `track` in as a source of its own, the way a library row is taken. The
/// engine can only place a track with `place_stream_at`, which pastes the picture where it lands and
/// ripples what follows, so the track goes down at the end of the timeline (nothing to ripple there)
/// and its scratch clips are deleted again, leaving the source. The placement that follows is then an
/// ordinary placement of that source.
fn register_track(s: &mut PlaybackSession, path: &Path, track: usize, fps: f64) -> Result<usize, String> {
    let end = parse::session_end(s);
    let placed = s
        .place_stream_at(parse::secs(end, fps), path, track, None)
        .map_err(|e| format!("audio_track:{track}: {e}"))?;
    if !placed {
        return Err(format!("audio_track:{track}: the track did not come in at {}", parse::tc(end, fps)));
    }
    let m = s
        .sources()
        .iter()
        .position(|x| x.path == path && x.audio_stream == track)
        .ok_or_else(|| format!("audio_track:{track}: the track did not become a source"))?;
    // A delete takes the clip's linked half with it, so the clips are found again after each one.
    loop {
        let mut next = None;
        for l in s.lanes() {
            if l.kind == LaneKind::Subtitle {
                continue;
            }
            if let Some(i) = s.lane_clips(l).iter().position(|c| c.source == m) {
                next = Some((l, i));
                break;
            }
        }
        let Some((l, i)) = next else { break };
        if !s.delete_clip(l, i) {
            return Err(format!("audio_track:{track}: could not clear the track's scratch clip on {}", parse::lane_name(l)));
        }
    }
    Ok(m)
}

#[allow(clippy::too_many_arguments)]
/// A track other than the source's own. The track is registered as a source of its own (`register_track`)
/// and the placement is the ordinary one for that source, so in, out, length, stretch, lanes, overwrite,
/// audio and the reply all behave as they do for any source.
fn place_track_op(s: &mut PlaybackSession, verb: &str, n: usize, op: &Value, dest: (u32, Option<Lane>), track: usize, fps: f64) -> Result<Done, String> {
    let (at, lane) = dest;
    let path: PathBuf = s.sources()[n].path.clone();
    let (m, added) = match s.sources().iter().position(|x| x.path == path && x.audio_stream == track) {
        Some(m) => (m, false),
        None => (register_track(s, &path, track, fps)?, true),
    };
    let mut done = place_op(s, verb, m, op, at, lane, fps)?;
    // A track taken in for the first time is a new source: the reply says so, with its file and track.
    if added {
        let file = source_name(s, m);
        done.text = format!("added src:{m} ({file} audio track {track}); {}", done.text);
    }
    Ok(done)
}

/// Stills are placed by the engine's own rule (their own length, no in/out).
fn place_op(
    s: &mut PlaybackSession,
    verb: &str,
    n: usize,
    op: &Value,
    at: u32,
    lane: Option<Lane>,
    fps: f64,
) -> Result<Done, String> {
    let path: PathBuf = s.sources()[n].path.clone();
    if engine::is_image(&path) {
        let still = s.still_secs();
        if op.get("audio_track").is_some() {
            return Err(format!("src:{n} is a still image; it has no audio track to choose"));
        }
        if op.get("in").is_some() || op.get("out").is_some() {
            return Err(format!(
                "src:{n} is a still image, which takes no in/out: give `length` for how long it shows (default {still:.1} s, the project's still length, up to {STILL_MAX_SECS:.0} s), or trim its out edge after placing it: trim {{clip, edge:\"out\", to}}"
            ));
        }
        if matches!(lane, Some(l) if l.kind == LaneKind::Audio) {
            return Err(format!("src:{n} is a still image; it goes on a video lane"));
        }
        // A lane the timeline does not have is named as missing, the way a clip's placement names it.
        if let Some(l) = lane.filter(|l| !s.lanes().contains(l)) {
            return Err(missing_lane(s, l));
        }
        // `length` sets how long the still shows; omitted, it is the project's still length.
        let asked = length(op, "length", fps)?;
        let len = match asked {
            None => (still * fps).ceil().max(1.0) as u32,
            Some(0) => return Err(format!("place src:{n}: a still's length must be more than 0")),
            Some(l) if f64::from(l) > STILL_MAX_SECS * fps => {
                return Err(format!(
                    "place src:{n}: length {} is past the {STILL_MAX_SECS:.0} s a still may run",
                    secs_text(l, fps)
                ))
            }
            Some(l) => l,
        };
        // A still takes the picture lane it is asked for (V1 when none), so that lane is what it can overwrite.
        let overwrite = overwrite_arg(op)?;
        let target = lane.unwrap_or(Lane::V1);
        let hit = if s.lanes().contains(&target) { overlapping(s, target, at, at + len, &[], fps) } else { Vec::new() };
        if !hit.is_empty() && !overwrite {
            return Err(format!(
                "{} at {} is taken: src:{n} would overwrite {}. Give overwrite:true to replace what is there, or another at or lane",
                parse::lane_name(target),
                parse::tc(at, fps),
                hit.join(", ")
            ));
        }
        let stream = s.sources()[n].audio_stream;
        // The clips of this still already at `at`, on any lane: the one this placement makes is the
        // one that is not among them (a second lane can hold the same still at the same start).
        let already = clips_of_source_at(s, n, at);
        let placed = s
            .place_stream_at(parse::secs(at, fps), &path, stream, lane)
            .map_err(|e| e.to_string())?;
        if !placed {
            return Err(format!("src:{n} did not place at {}", parse::tc(at, fps)));
        }
        // The engine places a still at its default length; a `length` trims the out edge to it.
        if asked.is_some() {
            let made: Vec<(Lane, u32)> = clips_of_source_at(s, n, at).into_iter().filter(|x| !already.contains(x)).collect();
            let idx = match made.as_slice() {
                [(l, _)] => s.lane_clips(*l).iter().position(|c| c.start == at && c.source == n).map(|i| (*l, i)),
                _ => None,
            };
            match idx {
                // Already that long (the default is five seconds): nothing to trim.
                Some((l, i)) if s.lane_clips(l)[i].end() == at + len => {}
                Some((l, i)) if s.trim_clip(l, i, Edge::End, at + len) => {}
                _ => return Err(format!("place src:{n}: the still could not be given length {} at {}", secs_text(len, fps), parse::tc(at, fps))),
            }
        }
        let touched = clips_of_source_at(s, n, at);
        let mut text = format!("{verb} src:{n} (still) @{at}f");
        if asked.is_some() {
            text.push_str(&format!(" for {}", secs_text(len, fps)));
        }
        if !hit.is_empty() {
            text.push_str(&format!(" — overwrote {}", hit.join(", ")));
        }
        return Ok(plain(text, touched));
    }
    // `audio_track` picks the sound of a multi-audio file. The source's own track is placed as any
    // other; a different one is a whole track of its own (place_track_op).
    let track = match audio_track_arg(op)? {
        Some(t) => Some((t, track_language(&path, n, t)?)),
        None => None,
    };
    if let Some((t, _)) = &track
        && *t != s.sources()[n].audio_stream
    {
        return place_track_op(s, verb, n, op, (at, lane), *t, fps);
    }
    let b = Base::of(s, &path);
    let frames = source_frames(s, n, lane, fps);
    let given_in = opt_source_time(op, "in", b, frames)?;
    let given_out = opt_source_time(op, "out", b, frames)?;
    let in_f = given_in.as_ref().map_or(0, |p| p.0);
    // `length` sets the out point from in, so a B-roll cut to a duration needs no speed math.
    let len_f = length(op, "length", fps)?;
    let stretch = stretch_arg(op)?;
    if stretch && len_f.is_none() {
        return Err(format!("place src:{n}: stretch:true needs length, the duration the source is fitted into"));
    }
    // `stretch` plays the whole source (from in, to out or its end) over `length`: the placement
    // takes the source's length and is then sped up or slowed to fill `length`.
    let (out_f, stretch_to) = match (len_f, &given_out) {
        (Some(len), _) if stretch => {
            let end_f = given_out.as_ref().map_or(frames, |p| p.0);
            if end_f <= in_f {
                return Err(format!("place src:{n}: out must be after in"));
            }
            let x = f64::from(end_f - in_f) / f64::from(len.max(1));
            if !(0.25..=4.0).contains(&x) {
                return Err(format!(
                    "place src:{n} stretch to {}: fitting {} of source into it needs speed ×{}, outside 0.25–4; a length nearer the source's, or a different in/out",
                    parse::tc(len, fps),
                    parse::tc(end_f - in_f, fps),
                    compact(x)
                ));
            }
            (end_f, Some((len, x)))
        }
        (Some(_), Some(_)) => {
            return Err(format!(
                "place src:{n}: give out or length, not both: length sets the out point from in"
            ))
        }
        (Some(len), None) => {
            let end_f = in_f.saturating_add(len);
            if end_f > frames {
                return Err(format!(
                    "place src:{n} length {}: from in {} it runs past the source end at {}; a shorter length, an earlier in, or stretch:true to fit the source into that length",
                    parse::tc(len, fps),
                    b.pos(b.native(in_f)),
                    b.pos(b.native(frames))
                ));
            }
            (end_f, None)
        }
        (None, Some(out)) => (range_end(op.get("in"), op.get("out").unwrap_or(&Value::Null), b, fps, frames, in_f, out.0), None),
        (None, None) => (frames, None),
    };
    let audio = audio_arg(op, lane)?;
    let overwrite = overwrite_arg(op)?;
    let mut replaced = Vec::new();
    // A stretch is placed at its speed, so the clip fills `length` and the overlap check sees that span.
    let speed = match stretch_to {
        Some((_, x)) => Speed::from_permille((x * 1000.0).round() as u16),
        None => Speed::NORMAL,
    };
    let mut touched = place_range(s, n, in_f, out_f, Dest { at, lane, audio, overwrite }, speed, &mut replaced)
        .map_err(|e| overlap_rounding_note(e, op, at, len_f, b.src_fps(), fps))?;
    // The reply names where the clip landed, which is `at` unless the engine moved it, and the source
    // range it landed with: a take is cut to its shorter half, so that can be shorter than asked.
    let landed = touched.first().map_or(at, |t| t.1);
    let (in_f, out_f) = landed_span(s, &touched, n).unwrap_or((in_f, out_f));
    let mut text = format!("{verb} src:{n} {} at timeline {landed}f", b.span(in_f, out_f));
    for (_, note) in [given_in, given_out].into_iter().flatten() {
        text.push_str(&note);
    }
    if let Some((len, x)) = stretch_to {
        // The clip was placed at its speed (a linked sound with it), so it already fills `length`.
        let &(l, st) = touched.first().ok_or_else(|| format!("place src:{n}: nothing placed to stretch"))?;
        let idx = s.lane_clips(l).iter().position(|c| c.start == st).ok_or_else(|| format!("place src:{n}: nothing placed to stretch"))?;
        let now = s.lane_clips(l).get(idx).map_or(len, |c| c.frames());
        touched = s.lane_clips(l).get(idx).map(|c| vec![(l, c.start)]).unwrap_or(touched);
        text.push_str(&format!("; stretched ×{} to {} ({}f){}", speed_text(x), secs_text(now, fps), now, speed_note(x)));
    }
    if let Some((t, lang)) = &track {
        text.push_str(&format!(" with audio track {t} ({lang})"));
    }
    text.push_str(&fit_note(s, n, &touched));
    if !replaced.is_empty() {
        text.push_str(&format!(" — overwrote {}", replaced.join(", ")));
    }
    Ok(plain(text, touched))
}

/// The note a placed picture owes when its aspect differs from the project canvas and it is
/// fitted (the default): the bars it gets, and the policy that crops instead. Empty for sound,
/// a still, a matching aspect, or a clip on another fit policy.
fn fit_note(s: &PlaybackSession, n: usize, touched: &[(Lane, u32)]) -> String {
    let Some(&(l, st)) = touched.first() else { return String::new() };
    if l.kind != LaneKind::Video {
        return String::new();
    }
    let Some(idx) = s.lane_clips(l).iter().position(|c| c.start == st) else { return String::new() };
    if !matches!(s.fit_of(l, idx), FitPolicy::Fit) {
        return String::new();
    }
    let Some(v) = engine::probe_media(&s.sources()[n].path).video else { return String::new() };
    let (cw, ch) = s.resolution();
    if cw == 0 || ch == 0 || v.width == 0 || v.height == 0 {
        return String::new();
    }
    let (src, canvas) = (f64::from(v.width) / f64::from(v.height), f64::from(cw) / f64::from(ch));
    if (src / canvas - 1.0).abs() < 0.01 {
        return String::new();
    }
    let bars = if src > canvas { "letterbox" } else { "pillarbox" };
    // The displayed size (a turn already applied), as the frame shows it: the bars sit at the sides for a
    // narrower picture and at the top and bottom for a wider one.
    format!("; fitted with {bars} (displayed {}×{} on {cw}×{ch}); fit {{clip, policy:\"fill\"}} crops to the frame instead", v.width, v.height)
}

/// `stretch` on a place: true fits the whole source into `length` by changing its speed.
fn stretch_arg(op: &Value) -> Result<bool, String> {
    match op.get("stretch") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("`stretch` must be true or false".into()),
    }
}

/// The length of source `n` a placement on `lane` reads, in timeline frames. An audio lane takes
/// the sound, so a video file's audio stream sets the length there (a 2 s picture with 60 s of
/// sound places 60 s); every other lane takes the picture, whose frame count is the file's length.
fn source_frames(s: &PlaybackSession, n: usize, lane: Option<Lane>, fps: f64) -> u32 {
    if lane.is_some_and(|l| l.kind == LaneKind::Audio)
        && let Some(frames) = sound_frames(s, n, fps)
    {
        return frames;
    }
    s.file_frames(&s.sources()[n].path)
}

/// The length of source `n`'s sound in timeline frames, rounded up: the wall an audio-lane clip of
/// it is held to (the engine's `wall_table`). None when it has no sound or its length cannot be read.
fn sound_frames(s: &PlaybackSession, n: usize, fps: f64) -> Option<u32> {
    let src = &s.sources()[n];
    let secs = engine::AudioSession::stream_duration_secs(&src.path, src.audio_stream).ok().flatten()?;
    let frames = (secs * fps).ceil();
    (frames.is_finite() && (1.0..=f64::from(u32::MAX)).contains(&frames)).then_some(frames as u32)
}

/// The source range `[in, out)` of the clip a placement landed with, read back from the lanes it
/// touched: the clip of source `n` at the start the placement reports. None when none is there.
fn landed_span(s: &PlaybackSession, got: &[(Lane, u32)], n: usize) -> Option<(u32, u32)> {
    got.iter().find_map(|&(l, st)| {
        s.lane_clips(l).iter().find(|c| c.start == st && c.source == n).map(|c| (c.in_frame, c.out_frame))
    })
}

/// `overwrite`: true lets a placement replace the clips it lands on. Off when not given.
fn overwrite_arg(op: &Value) -> Result<bool, String> {
    match op.get("overwrite") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("`overwrite` must be true or false".into()),
    }
}

/// Where an append, keep or assemble starts when it gives no `at`: the end of the lane it
/// writes (`lane`, or V1 where the picture goes), so an empty V1 starts at 0 whatever the sound holds.
fn tail_start(s: &PlaybackSession, lane: Option<Lane>) -> u32 {
    let l = lane.unwrap_or(Lane::V1);
    if !s.lanes().contains(&l) {
        return 0;
    }
    s.lane_clips(l).iter().map(|c| c.end()).max().unwrap_or(0)
}

fn op_place(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let n = source_arg(s, op)?;
    let at = need(op, "at", fps, end)?;
    let lane = lane_arg(op, "lane")?;
    place_op(s, "place", n, op, at, lane, fps)
}

/// `insert {source, in?, out?, at, to?, length?, lanes?}` and `overwrite {…}`: the three-point edit.
/// The source's `in`..`out` (the whole file when none) is put at timeline `at`. The length is the
/// source range, or `to` (the timeline out mark: duration = to − at, from `in`), or `length`.
/// `insert` opens room on the lanes the take lands on (a clip `at` falls inside is cut there, the
/// rest moves later); `overwrite` replaces what is there and moves nothing. The take lands on
/// `lanes` (one video lane for the picture, one audio lane for the sound), else the first
/// unlocked video and audio lanes. One engine edit, so one undo step.
fn op_three_point(s: &mut PlaybackSession, kind: SourceEdit, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let verb = if kind == SourceEdit::Insert { "insert" } else { "overwrite" };
    // `src` is the short spelling of `source`.
    let mut aliased = op.clone();
    if let Some(o) = aliased.as_object_mut()
        && let Some(v) = o.remove("src")
    {
        if o.contains_key("source") {
            return Err(format!("{verb}: give source or src, not both"));
        }
        o.insert("source".into(), v);
    }
    let op = &aliased;
    let n = source_arg(s, op)?;
    let at = need(op, "at", fps, end)?;
    let to = opt_time(op, "to", fps, end)?;
    let len_arg = length(op, "length", fps)?;
    let path: PathBuf = s.sources()[n].path.clone();
    let still = engine::is_image(&path);
    let picture = still || engine::probe_media(&path).video.is_some();
    let sound = !still
        && !AudioSession::probe_streams(&path)
            .map_err(|e| format!("src:{n}: cannot read its sound: {e}"))?
            .is_empty();
    if !picture && !sound {
        return Err(format!("src:{n} has neither picture nor sound"));
    }
    // The lanes the take lands on: those named, else the first unlocked of each kind it has.
    let first_free = |kind: LaneKind| s.lanes().into_iter().find(|l| l.kind == kind && !s.lane_locked(*l));
    let (vl, al) = match lane_list(op, "lanes")? {
        Some(list) => {
            if list.is_empty() {
                return Err(format!("{verb}: `lanes` is empty: name a video lane, an audio lane, or both, e.g. [\"V1\",\"A1\"]"));
            }
            for &l in &list {
                if l.kind == LaneKind::Subtitle {
                    return Err(format!("{verb}: a source cannot go on a caption lane ({})", parse::lane_name(l)));
                }
                if !s.lanes().contains(&l) {
                    return Err(missing_lane(s, l));
                }
            }
            let pick = |kind: LaneKind| -> Result<Option<Lane>, String> {
                let of: Vec<Lane> = list.iter().copied().filter(|l| l.kind == kind).collect();
                match of.as_slice() {
                    [] => Ok(None),
                    [l] => Ok(Some(*l)),
                    _ => Err(format!("{verb}: `lanes` names two lanes of one kind; a take lands on at most one video and one audio lane")),
                }
            };
            (pick(LaneKind::Video)?, pick(LaneKind::Audio)?)
        }
        None => {
            let v = if picture { Some(first_free(LaneKind::Video).ok_or("every video lane is locked: unlock one, or name lanes")?) } else { None };
            let a = if sound { Some(first_free(LaneKind::Audio).ok_or("every audio lane is locked: unlock one, or name lanes")?) } else { None };
            (v, a)
        }
    };
    if let Some(l) = vl.filter(|_| !picture) {
        return Err(format!("src:{n} has no picture; it cannot go on {}: name an audio lane (A1..)", parse::lane_name(l)));
    }
    if let Some(l) = al.filter(|_| !sound) {
        return Err(format!("src:{n} has no sound; it cannot go on {}: name a video lane (V1..)", parse::lane_name(l)));
    }
    let lanes: Vec<Lane> = vl.into_iter().chain(al).collect();
    check_unlocked(s, &lanes)?;

    // The duration a timeline out mark or a length asks for.
    let dur = match (to, len_arg) {
        (Some(_), Some(_)) => return Err(format!("{verb} src:{n}: give `to` or `length`, not both")),
        (Some(t), None) if t <= at => {
            return Err(format!("{verb} src:{n}: `to` ({}) must be after `at` ({})", parse::tc(t, fps), parse::tc(at, fps)))
        }
        (Some(t), None) => Some(t - at),
        (None, Some(0)) => return Err(format!("{verb} src:{n}: length must be more than 0")),
        (None, l) => l,
    };
    let (in_f, out_f, note) = if still {
        let still = s.still_secs();
        if op.get("in").is_some() || op.get("out").is_some() {
            return Err(format!(
                "src:{n} is a still image, which takes no in/out: give `to` or `length` for how long it shows (default {still:.1} s, the project's still length, up to {STILL_MAX_SECS:.0} s)"
            ));
        }
        let len = dur.unwrap_or_else(|| (still * fps).ceil().max(1.0) as u32);
        if f64::from(len) > STILL_MAX_SECS * fps {
            return Err(format!("{verb} src:{n}: {} is past the {STILL_MAX_SECS:.0} s a still may run", secs_text(len, fps)));
        }
        (0, len, String::new())
    } else {
        let b = Base::of(s, &path);
        let frames = source_frames(s, n, vl.is_none().then_some(al).flatten(), fps);
        let given_in = opt_source_time(op, "in", b, frames)?;
        let given_out = opt_source_time(op, "out", b, frames)?;
        let in_f = given_in.as_ref().map_or(0, |p| p.0);
        let out_f = match (dur, &given_out) {
            (Some(_), Some(_)) => {
                return Err(format!("{verb} src:{n}: give out, or the timeline length (`to`/`length`), not both: a duration sets the out point from in"))
            }
            (Some(d), None) => {
                let o = in_f.saturating_add(d);
                if o > frames {
                    return Err(format!(
                        "{verb} src:{n}: {} from in {} runs past the source end at {}; a shorter range or an earlier in",
                        secs_text(d, fps),
                        b.pos(b.native(in_f)),
                        b.pos(b.native(frames))
                    ));
                }
                o
            }
            (None, Some(o)) => o.0,
            (None, None) => frames,
        };
        if out_f <= in_f || out_f > frames {
            return Err(format!(
                "src:{n} {} plays {} to {}; the range {} to {} is not inside it",
                source_name(s, n),
                b.pos(0),
                b.pos(b.native(frames)),
                b.pos(b.native(in_f)),
                b.pos(b.native(out_f)),
            ));
        }
        let notes: String = [given_in, given_out].into_iter().flatten().map(|(_, note)| note).collect();
        (in_f, out_f, notes)
    };
    let items: Vec<(Lane, Clip)> = lanes.iter().map(|&l| (l, clip_of(n, in_f, out_f))).collect();
    let before: Vec<(Lane, Vec<Clip>)> = lanes.iter().map(|&l| (l, s.lane_clips(l).to_vec())).collect();
    s.edit_from_source_at(kind, parse::secs(at, fps), &items).map_err(|e| format!("{verb} src:{n} at {}: {e}", parse::tc(at, fps)))?;

    // What landed: the source range and length the take has (a take is cut to its shorter half).
    let touched: Vec<(Lane, u32)> = lanes.iter().map(|&l| (l, at)).collect();
    let (l_in, l_out) = landed_span(s, &touched, n).unwrap_or((in_f, out_f));
    let len = s.lane_clips(touched[0].0).iter().find(|c| c.start == at && c.source == n).map_or(out_f - in_f, |c| c.frames());
    let names: Vec<String> = lanes.iter().map(|&l| parse::lane_name(l)).collect();
    let shown = if still { "(still)".to_string() } else { Base::of(s, &path).span(l_in, l_out) };
    let mut text = format!(
        "{verb} src:{n} {shown} at timeline {at}f ({}) on {} for {}{note}",
        secs_text(at, fps),
        names.join("+"),
        secs_text(len, fps)
    );
    if kind == SourceEdit::Insert {
        let cut: Vec<String> = before
            .iter()
            .flat_map(|(l, cs)| cs.iter().filter(|c| c.start < at && at < c.end()).map(|c| parse::addr(*l, c.start)))
            .collect();
        let moved: usize = before.iter().map(|(_, cs)| cs.iter().filter(|c| c.start >= at || (c.start < at && at < c.end())).count()).sum();
        if !cut.is_empty() {
            text.push_str(&format!(" — split {}", cut.join(", ")));
        }
        if moved > 0 {
            text.push_str(&format!("{} rippled the later clips on {} {} later", if cut.is_empty() { " —" } else { ";" }, names.join("+"), secs_text(len, fps)));
        }
    } else {
        let end_f = at + len;
        let replaced: Vec<String> = overwrite_notes(&before, at, end_f, fps);
        if !replaced.is_empty() {
            text.push_str(&format!(" — overwrote {}", replaced.join(", ")));
        }
    }
    text.push_str(&fit_note(s, n, &touched));
    Ok(plain(text, touched))
}

fn op_append(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let n = source_arg(s, op)?;
    let lane = lane_arg(op, "lane")?;
    if let Some(until) = opt_time(op, "until", fps, end)? {
        if op.get("audio_track").is_some() {
            return Err(format!("append src:{n}: audio_track does not loop: place the track once with place, then copy it"));
        }
        if op.get("length").is_some() {
            return Err(format!("append src:{n}: until and length do not combine: give until, or a length for one copy"));
        }
        return append_loop(s, op, n, lane, until, fps);
    }
    let at = tail_start(s, lane);
    // Append pairs sound as a laneless place does. audio:false lays the picture alone on V1
    // (or `lane`) at the end; audio:true is the default, so it is taken out before audio_arg reads it.
    match op.get("audio") {
        Some(Value::Bool(false)) => place_op(s, "append", n, op, at, Some(lane.unwrap_or(Lane::V1)), fps),
        Some(Value::Bool(true)) => {
            let mut paired = op.clone();
            if let Some(o) = paired.as_object_mut() {
                o.remove("audio");
            }
            place_op(s, "append", n, &paired, at, lane, fps)
        }
        _ => place_op(s, "append", n, op, at, lane, fps),
    }
}

/// `append {source, lane?, until, in?, out?, audio?}`: the source's range (its in..out, or the
/// whole file) laid back to back from the end of `lane` (V1 when none) until timeline time
/// `until`; the last copy is trimmed to end there. Every copy goes through `place_range`, so
/// a video lane pairs sound and a busy lane refuses, as a plain append does.
fn append_loop(
    s: &mut PlaybackSession,
    op: &Value,
    n: usize,
    lane: Option<Lane>,
    until: u32,
    fps: f64,
) -> Result<Done, String> {
    let path: PathBuf = s.sources()[n].path.clone();
    if engine::is_image(&path) {
        return Err(format!("append src:{n} until: src:{n} is a still image; a still is placed with place, and loops are for media files"));
    }
    let b = Base::of(s, &path);
    let frames = s.file_frames(&path);
    let in_f = opt_source_time(op, "in", b, frames)?.map_or(0, |p| p.0);
    let out_f = opt_source_time(op, "out", b, frames)?.map_or(frames, |p| p.0);
    if out_f <= in_f {
        return Err(format!("append src:{n} until: the range {} is empty", b.span(in_f, out_f)));
    }
    let first = tail_start(s, lane);
    if until <= first {
        return Err(format!(
            "append src:{n} until {}: {} already runs to {}, past that time",
            parse::tc(until, fps),
            lane.map_or("V1".to_string(), parse::lane_name),
            parse::tc(first, fps)
        ));
    }
    let audio = audio_arg(op, lane)?;
    let copy = out_f - in_f;
    let mut at = first;
    let mut copies = 0usize;
    let mut touched = Vec::new();
    let mut replaced = Vec::new();
    while at < until {
        let seg = copy.min(until - at);
        let got = place_range(s, n, in_f, in_f + seg, Dest { at, lane, audio, overwrite: false }, Speed::NORMAL, &mut replaced)
            .map_err(|e| format!("append src:{n} copy {}: {e}", copies + 1))?;
        // The next copy starts where this one landed: a take is cut to its shorter half.
        let (pa, pz) = landed_span(s, &got, n).unwrap_or((in_f, in_f + seg));
        at += pz - pa;
        touched.extend(got);
        copies += 1;
    }
    let trimmed = if (until - first).is_multiple_of(copy) { String::new() } else { " (last copy trimmed)".to_string() };
    let mut text = format!(
        "loop src:{n} {copies}× on {} from {} to {}{trimmed}",
        lane.map_or("V1+A1".to_string(), parse::lane_name),
        parse::tc(first, fps),
        parse::tc(until, fps)
    );
    // A range shorter than the gap plays back to back: say how often, so a short file is not a surprise.
    if copies > 1 {
        text.push_str(&format!(
            "; the {:.1}s range looped {copies} times to fill it",
            parse::secs(copy, fps)
        ));
    }
    if !replaced.is_empty() {
        text.push_str(&format!(" — overwrote {}", replaced.join(", ")));
    }
    Ok(plain(text, touched))
}

fn op_keep(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let n = source_arg(s, op)?;
    let lane = lane_arg(op, "lane")?;
    let audio = audio_arg(op, lane)?;
    let overwrite = overwrite_arg(op)?;
    let mut replaced = Vec::new();
    let ranges = op
        .get("ranges")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
        .ok_or("missing `ranges`: give [[in,out], ...] in source times, e.g. [[\"0s\",\"4.5s\"]]")?;
    let path: PathBuf = s.sources()[n].path.clone();
    if engine::is_image(&path) {
        return Err(format!("src:{n} is a still image; keep takes ranges of a media file"));
    }
    let b = Base::of(s, &path);
    let frames = s.file_frames(&path);
    let first = opt_time(op, "at", fps, end)?.unwrap_or_else(|| tail_start(s, lane));
    let mut at = first;
    let mut touched = Vec::new();
    let mut notes = String::new();
    let mut shown = Vec::with_capacity(ranges.len());
    for (i, r) in ranges.iter().enumerate() {
        let pair = r
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or_else(|| format!("ranges[{i}] must be [in,out], two source times"))?;
        let (a, note_a) = source_time(&pair[0], b, frames, "in").map_err(|e| format!("ranges[{i}] {e}"))?;
        let (z, note_z) = source_time(&pair[1], b, frames, "out").map_err(|e| format!("ranges[{i}] {e}"))?;
        let z = range_end(Some(&pair[0]), &pair[1], b, fps, frames, a, z);
        let got = place_range(s, n, a, z, Dest { at, lane, audio, overwrite }, Speed::NORMAL, &mut replaced).map_err(|e| format!("ranges[{i}]: {e}"))?;
        // The span named is the one the clip landed with (frame-snapped), as clip_info reads it; the
        // next range starts where it ends.
        let (pa, pz) = landed_span(s, &got, n).unwrap_or((a, z));
        at += pz - pa;
        touched.extend(got);
        notes.push_str(&note_a);
        notes.push_str(&note_z);
        shown.push(b.span(pa, pz));
    }
    let mut text = format!("keep src:{n} {} at timeline {first}f{notes}", shown.join(", "));
    if !replaced.is_empty() {
        text.push_str(&format!(" — overwrote {}", replaced.join(", ")));
    }
    Ok(plain(text, touched))
}

/// `assemble {items:[{source, in?, out?}], lane?, at?, gap?}`: each item is a
/// source range, laid back to back from `at`; items may come from different sources.
fn op_assemble(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let items = op
        .get("items")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
        .ok_or("missing `items`: give [{source, in?, out?}, ...]; each item is one range, laid end to end")?;
    let lane = lane_arg(op, "lane")?;
    let audio = audio_arg(op, lane)?;
    let gap = length(op, "gap", fps)?.unwrap_or(0);
    if gap > 0 && lane.is_none() {
        return Err("assemble: a gap needs `lane` (V1.. or A1..); without one the items land at the timeline end and the gap cannot be kept".into());
    }
    let overwrite = overwrite_arg(op)?;
    let mut replaced = Vec::new();
    let first = opt_time(op, "at", fps, end)?.unwrap_or_else(|| tail_start(s, lane));
    let mut at = first;
    let mut touched = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if let Some(key) = item.as_object().and_then(|o| o.keys().find(|k| !matches!(k.as_str(), "source" | "in" | "out"))) {
            return Err(format!("items[{i}]: unknown key `{key}`; an item takes source, in, out"));
        }
        let n = source_arg(s, item).map_err(|e| format!("items[{i}]: {e}"))?;
        let path: PathBuf = s.sources()[n].path.clone();
        if engine::is_image(&path) {
            return Err(format!("items[{i}]: src:{n} is a still image; assemble takes ranges of media files"));
        }
        let base = Base::of(s, &path);
        let frames = s.file_frames(&path);
        let given_in = opt_source_time(item, "in", base, frames).map_err(|e| format!("items[{i}] {e}"))?;
        let given_out = opt_source_time(item, "out", base, frames).map_err(|e| format!("items[{i}] {e}"))?;
        let a = given_in.as_ref().map_or(0, |p| p.0);
        let z = given_out.as_ref().map_or(frames, |p| p.0);
        let got = place_range(s, n, a, z, Dest { at, lane, audio, overwrite }, Speed::NORMAL, &mut replaced).map_err(|e| format!("items[{i}]: {e}"))?;
        // place_range refuses an empty or backwards range, so z > a here. The next item starts where
        // this one landed, which a take cut to its shorter half makes shorter than the range asked.
        let (pa, pz) = landed_span(s, &got, n).unwrap_or((a, z));
        at += (pz - pa) + if i + 1 < items.len() { gap } else { 0 };
        touched.extend(got);
        notes.extend(given_in.into_iter().chain(given_out).map(|(_, note)| note));
    }
    let mut names: Vec<String> = Vec::new();
    for &(l, _) in &touched {
        let name = parse::lane_name(l);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let mut text = format!(
        "assembled {} clip(s) on {} from {} to {}{}",
        items.len(),
        names.join("+"),
        secs_text(first, fps),
        secs_text(at, fps),
        notes.concat()
    );
    if !replaced.is_empty() {
        text.push_str(&format!(" — overwrote {}", replaced.join(", ")));
    }
    Ok(plain(text, touched))
}

// ---- cuts and moves -------------------------------------------------------

fn op_split(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let asked = need(op, "at", fps, end)?;
    let all = s.lanes();
    let named = lane_list(op, "lanes")?;
    if named.as_ref().is_some_and(|v| v.is_empty()) {
        return Err(format!(
            "split {}: `lanes` is empty; name the lanes to cut, or leave `lanes` out to cut every lane",
            shown_time(op.get("at"), asked)
        ));
    }
    let explicit = named.is_some();
    let mut cut_on: Vec<Lane> = match &named {
        None => all.clone(),
        Some(v) => {
            if let Some(&l) = v.iter().find(|l| !all.contains(l)) {
                return Err(format!("lane {} is not on this timeline", parse::lane_name(l)));
            }
            v.clone()
        }
    };
    // A slow clip can be cut only where a source frame begins, so a frame between two showings of one
    // is moved to the nearest cut point that every clip the cut runs through can take (the same snap
    // insert_time, remove_time and freeze make). The reply says so.
    let scope_for_cut = if explicit { cut_on.clone() } else { cut_lanes(s, &cut_on, asked) };
    let at = cut_point(s, &scope_for_cut, asked);
    // Default: a clip the cut runs through takes its linked partners' lanes with it, so a take
    // stays whole. With explicit `lanes` only the named lanes are cut: a partner not named stays
    // whole, and the take is unlinked at the cut (reported below).
    let mut partners: Vec<String> = Vec::new();
    let mut unlink_at: Vec<String> = Vec::new();
    for &l in &cut_on.clone() {
        if l.kind == LaneKind::Subtitle {
            continue;
        }
        for (i, c) in s.lane_clips(l).iter().enumerate() {
            if c.start < at && at < c.end() {
                let take = take_of(s, l, i);
                if explicit && take.iter().any(|(m, _)| !cut_on.contains(m)) {
                    // The cut unlinks this take, which changes the link of every clip in it: a locked one refuses.
                    if let Some(&(m, _)) = take.iter().find(|(m, _)| s.lane_locked(*m)) {
                        return Err(format!(
                            "split {}: the cut would detach {} from its take at the cut, and {} is locked; {}, or cut its lanes too (leave `lanes` out)",
                            shown_time(op.get("at"), asked),
                            addr_of(s, l, i),
                            parse::lane_name(m),
                            lock_msg(m)
                        ));
                    }
                    let names: Vec<String> = take.iter().map(|&(m, j)| addr_of(s, m, j)).collect();
                    unlink_at.push(names.join(" + "));
                }
                for (m, _) in take {
                    if !cut_on.contains(&m) && !explicit {
                        cut_on.push(m);
                        partners.push(format!("{} (linked to {})", parse::lane_name(m), addr_of(s, l, i)));
                    }
                }
            }
        }
    }
    let media_on: Vec<Lane> = cut_on.iter().copied().filter(|l| l.kind != LaneKind::Subtitle).collect();
    check_unlocked(s, &media_on)?;
    // The clips the cut runs through, read before it: only these are cut, so only these are named.
    let straddled: Vec<(Lane, u32)> = media_on
        .iter()
        .flat_map(|&l| s.lane_clips(l).iter().filter(|c| c.start < at && at < c.end()).map(move |c| (l, c.start)).collect::<Vec<_>>())
        .collect();
    if !s.cut_at_lanes(parse::secs(at, fps), &cut_on) {
        return Err(format!(
            "nothing to split at {}: no clip spans that frame, or a cut is already there",
            shown_time(op.get("at"), asked)
        ));
    }
    // An explicit-lanes cut that leaves a partner whole unlinks the take at the cut: the engine does
    // that inside the split itself (one undo step), so the reply below only reports it.
    // Each clip cut leaves its left piece at its start and a right piece at the cut.
    let mut touched = Vec::new();
    for (l, start) in straddled {
        touched.push((l, start));
        touched.push((l, at));
    }
    let kept: Vec<String> = all.iter().filter(|l| !cut_on.contains(l)).map(|&l| parse::lane_name(l)).collect();
    let mut text = format!("split @{at}f");
    if at != asked {
        text.push_str(&cut_moved_note(asked, at));
    }
    if !partners.is_empty() {
        text.push_str(&format!("; also cut {}", partners.join(", ")));
    }
    if !kept.is_empty() {
        text.push_str(&format!("; kept whole: {}", kept.join(", ")));
    }
    if !unlink_at.is_empty() {
        text.push_str(&format!("; now detached at the cut: {}", unlink_at.join(", ")));
    }
    Ok(plain(text, touched))
}

/// A time as the caller wrote it, with its frame: `99s (2970f)`. A frame count
/// (`2970f`) is already the frame, so it is shown alone.
fn shown_time(v: Option<&Value>, frame: u32) -> String {
    match v {
        Some(Value::String(t)) if !t.trim().ends_with(['f', 'F']) => format!("{} ({frame}f)", t.trim()),
        Some(Value::Number(n)) => format!("{n}s ({frame}f)"),
        _ => format!("{frame}f"),
    }
}

/// A trim the engine refused after the room check passed: the edge is not where it asked, and
/// it did not move, so the reply names the room the edge has and says the move was refused.
fn trim_refusal(s: &PlaybackSession, lane: Lane, idx: usize, edge: Edge, shown: &str, addr: &str) -> String {
    let side = if edge == Edge::Start { "in" } else { "out" };
    match s.trim_room(lane, idx, edge) {
        Some((lo, hi)) => format!(
            "trim {addr} {side} to {shown} refused: the edge can travel {lo}f–{hi}f and the engine would not move it"
        ),
        None => format!("no clip at {addr}"),
    }
}

fn op_trim(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let (edge, side) = match op.get("edge").and_then(Value::as_str) {
        Some("in") => (Edge::Start, "in"),
        Some("out") => (Edge::End, "out"),
        _ => return Err("`edge` must be \"in\" or \"out\"".into()),
    };
    let to = need(op, "to", fps, end)?;
    let shown = shown_time(op.get("to"), to);
    let group = match op.get("scope").and_then(Value::as_str).unwrap_or("group") {
        "group" => true,
        "clip" => false,
        other => return Err(format!("scope `{other}` is not \"group\" or \"clip\"")),
    };
    // `ripple:true` closes (or opens) the hole the edge leaves, and what follows moves with it:
    // Premiere's ripple trim. The clip's linked partners move with it through the engine.
    let ripple = match op.get("ripple") {
        None => false,
        Some(v) => v.as_bool().ok_or("`ripple` must be true or false")?,
    };
    let verb = if ripple { "ripple trim" } else { "trim" };
    let take = take_of(s, lane, idx);
    let lanes: Vec<Lane> = take.iter().map(|&(l, _)| l).collect();
    if group {
        check_unlocked(s, &lanes)?;
    } else {
        // The clip alone changes only its own lane. Its partners keep their media, but leaving the take
        // changes their link, which a locked lane never takes, so a locked partner refuses with the way out.
        check_unlocked(s, &[lane])?;
        if let Some(&(pl, _)) = take.iter().skip(1).find(|(l, _)| s.lane_locked(*l)) {
            let pn = parse::lane_name(pl);
            return Err(format!(
                "trim {addr} scope:\"clip\" refused: trimming {addr} alone unlinks it from {pn}, which is locked; unlock {pn} or use scope:\"group\""
            ));
        }
    }
    if group && take.len() > 1 && !ripple {
        let at_clip = |s: &PlaybackSession| s.lane_clips(lane).get(idx).map(|c| if edge == Edge::Start { c.start } else { c.end() });
        let before = at_clip(s);
        let mut done = trim_take(s, &take, edge, to, fps, &addr, side, &shown, op.get("to"))?;
        if let Some(hint) = in_edge_hole_hint(edge, before, at_clip(s), &lanes.iter().map(|&l| parse::lane_name(l)).collect::<Vec<_>>()) {
            done.text.push_str(&format!("; {hint}"));
        }
        return Ok(done);
    }
    let edge_at = |s: &PlaybackSession, l: Lane, i: usize| {
        s.lane_clips(l).get(i).map(|c| if edge == Edge::Start { c.start } else { c.end() })
    };
    let before = edge_at(s, lane, idx);
    // Clamped to the room the edge has, here, so the reply can name the clamp and its wall. The
    // walls are named before anything moves: a clamp is worded from the bound it reached.
    let Some((lo, hi)) = s.trim_room(lane, idx, edge) else {
        return Err(format!("no clip at {addr}"));
    };
    let Some(bounds) = s.trim_bounds(lane, idx, edge) else {
        return Err(format!("no clip at {addr}"));
    };
    // The other edge bounds what this one may be: a clip that would end where it starts
    // (or start where it ends) has no picture left, so that is refused, not clamped.
    if let Some(c) = s.lane_clips(lane).get(idx).cloned() {
        let (other, other_name, ok) = match edge {
            Edge::End => (c.start, "in", to > c.start),
            Edge::Start => (c.end(), "out", to < c.end()),
        };
        if !ok {
            return Err(format!(
                "trim {addr} {side} to {shown} would leave no picture: its {side} edge must stay {} its {other_name} edge at {}",
                if edge == Edge::End { "after" } else { "before" },
                parse::tc(other, fps)
            ));
        }
    }
    let target = to.clamp(lo, hi);
    let (named_lo, named_hi) = (wall_named(s, lane, idx, edge, lo, bounds.0), wall_named(s, lane, idx, edge, hi, bounds.1));
    // The blockers are read before anything moves; the clicked clip is never its own blocker.
    let own: Vec<(Lane, u32)> = s.lane_clips(lane).get(idx).map(|c| (lane, c.start)).into_iter().collect();
    let words_lo = clamp_words(s, &own, named_lo, lane, edge, lo);
    let words_hi = clamp_words(s, &own, named_hi, lane, edge, hi);
    let reason = if target == hi { words_hi.clone() } else { words_lo.clone() };
    // An edge that is already where the op asks is a note, not a failed batch: nothing moves.
    if Some(to) == before {
        return Ok(plain(format!("{addr} {side} edge is already at {}: nothing to trim", parse::tc(to, fps)), Vec::new()));
    }
    if Some(target) == before {
        return Ok(plain(
            format!("{addr} {side} edge is already at {}: {reason}; it cannot go to {shown}", parse::tc(target, fps)),
            Vec::new(),
        ));
    }
    // Trimming one clip of a take alone leaves the take. Only this clip is unlinked: its partners are
    // regrouped among themselves, so they stay linked to each other (and their media does not move).
    // This comes after the no-op checks above: a trim that changes nothing must not detach anything.
    if !group && take.len() > 1 {
        if !s.ungroup(lane, idx) {
            return Err(format!("trim {addr} refused: its take could not be detached"));
        }
        let partners = &take[1..];
        for &(pl, pi) in &partners[1..] {
            s.group(partners[0].0, partners[0].1, pl, pi)
                .map_err(|e| format!("trim {addr} refused: its partners could not stay linked: {e}"))?;
        }
    }
    let trimmed = match ripple {
        true => s.ripple_trim_clip(lane, idx, edge, target),
        false => s.trim_clip(lane, idx, edge, target),
    };
    if !trimmed {
        return Err(trim_refusal(s, lane, idx, edge, &shown, &addr));
    }
    // A ripple start keeps the clip's start where it is and cuts its head, so the start alone does
    // not show the move: the cut point is where the edge landed.
    let ripple_head = ripple && edge == Edge::Start;
    let actual = if ripple_head { Some(target) } else { edge_at(s, lane, idx) };
    if actual == before {
        return Ok(plain(format!("{addr} {side} edge did not move: nothing to trim"), Vec::new()));
    }
    let touched: Vec<(Lane, u32)> = s.lane_clips(lane).get(idx).map(|c| (lane, c.start)).into_iter().collect();
    let landed = actual.unwrap_or(to);
    let mut text = match asked_arrow(op.get("to"), fps, landed) {
        Some(a) => format!("{verb} {addr} {side}: {a}"),
        None => format!("{verb} {addr} {side} → {}", parse::tc(landed, fps)),
    };
    if !group && take.len() > 1 {
        let rest = take.len() - 1;
        if rest == 1 {
            text.push_str("; now detached from its take (1 partner left where it is and unlinked)");
        } else {
            text.push_str(&format!("; now detached from its take ({rest} partners stay linked to each other)"));
        }
    }
    if let Some(a) = actual.filter(|&a| a != to) {
        let words = if a == hi { &words_hi } else { &words_lo };
        text.push_str(&format!("; clamped from {shown} to {}: {words}", parse::tc(a, fps)));
    }
    // An in edge that moved later leaves the clip where it was, so a hole opens before it. Name the
    // close for it: no ripple trim exists, and close_gaps pulls the later clips left.
    if let Some(hint) = in_edge_hole_hint(edge, before, actual, &[parse::lane_name(lane)]).filter(|_| !ripple) {
        text.push_str(&format!("; {hint}"));
    }
    Ok(plain(text, touched))
}

/// `split_edit {clip, side, edge, to}`: the split edit on the clip's take. `side` names the half
/// whose edge moves ("audio" or "video"), `edge` which edge of that half ("in" or "out"), and the
/// other half stays put. The pair is unlinked first, as a group trims together.
fn op_split_edit(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let (audio, word) = match op.get("side").and_then(Value::as_str) {
        Some("audio") => (true, "audio"),
        Some("video") => (false, "video"),
        _ => return Err("`side` must be \"audio\" or \"video\": the half of the take whose edge moves".into()),
    };
    let edge = match op.get("edge").and_then(Value::as_str) {
        Some("in") => Edge::Start,
        Some("out") => Edge::End,
        _ => return Err("`edge` must be \"in\" or \"out\"".into()),
    };
    let to = need(op, "to", fps, end)?;
    let shown = shown_time(op.get("to"), to);
    let take = take_of(s, lane, idx);
    check_unlocked(s, &take.iter().map(|&(l, _)| l).collect::<Vec<_>>())?;
    let (l, i, moved, landed) = engine::editkeys::split_edit_at(s, (lane, idx), audio, Some(edge), to)
        .map_err(|e| format!("split_edit {addr} {word} {shown} refused: {e}"))?;
    let ed = if moved == Edge::Start { "in" } else { "out" };
    let now = addr_of(s, l, i);
    let mut text = match asked_arrow(op.get("to"), fps, landed) {
        Some(a) => format!("split_edit {now} {word} {ed}: {a}"),
        None => format!("split_edit {now} {word} {ed} → {}", parse::tc(landed, fps)),
    };
    if take.len() > 1 {
        text.push_str("; the pair is unlinked");
    }
    let touched: Vec<(Lane, u32)> = s.lane_clips(l).get(i).map(|c| (l, c.start)).into_iter().collect();
    Ok(plain(text, touched))
}

/// The one-line offer after a trim of an in edge that moved later: the hole it leaves is closed by
/// close_gaps on the lane (no op trims and pulls later clips left in one step). None when the in
/// edge did not move later, so no hole opened.
fn in_edge_hole_hint(edge: Edge, before: Option<u32>, actual: Option<u32>, names: &[String]) -> Option<String> {
    let (before, actual) = (before?, actual?);
    if edge != Edge::Start || actual <= before {
        return None;
    }
    let closes: Vec<String> = names.iter().map(|n| format!("close_gaps {{lane:\"{n}\"}}")).collect();
    Some(format!(
        "the in edge leaves a hole before the clip; to pull later clips left over it, {} (each closes every gap on its lane)",
        closes.join(" and ")
    ))
}

/// The bound an edge's wall is named by, read against the clips on its lane. The engine calls a
/// source's end the binding wall when it meets a neighbour exactly (equal walls count as media),
/// so a wall that a neighbour sits on is named for the neighbour: that clip is what stops the edge.
fn wall_named(s: &PlaybackSession, lane: Lane, idx: usize, edge: Edge, at: u32, bound: WallBound) -> WallBound {
    if !matches!(bound, WallBound::Source | WallBound::Cap) {
        return bound;
    }
    let clips = s.lane_clips(lane);
    let meets = match edge {
        Edge::End => clips.get(idx + 1).is_some_and(|n| n.start == at),
        Edge::Start => idx.checked_sub(1).and_then(|p| clips.get(p)).is_some_and(|p| p.end() == at),
    };
    if meets { WallBound::Neighbour(at) } else { bound }
}

/// What stopped an edge at `at`, in words, from the bound the engine named for that wall
/// ([`engine::project::WallBound`]), asked before the trim.
fn wall_words(bound: WallBound, edge: Edge, lane: Lane, at: u32) -> String {
    match bound {
        WallBound::Source if edge == Edge::Start => format!("clamped at the source's start ({at} frames)"),
        WallBound::Source => format!("clamped at the source's end ({at} frames)"),
        WallBound::Neighbour(f) => format!("clamped where it meets {}", parse::addr(lane, f)),
        WallBound::Timeline => "clamped at the timeline start".to_string(),
        WallBound::MinLength => "clamped at the 1-frame minimum".to_string(),
        WallBound::Cap => "clamped at the still's length cap".to_string(),
    }
}

/// The clip that stops an edge at frame `at`, named by address: a clip that meets the edge there
/// (ends at `at` for an in edge, starts there for an out edge), looked for on the wall's own lane
/// first and then the other lanes of its kind. The take's own members (`members`, as (lane, start))
/// never count: a clip cannot be its own partner's blocker.
fn blocker_addr(s: &PlaybackSession, members: &[(Lane, u32)], lane: Lane, edge: Edge, at: u32) -> Option<String> {
    let mut order = vec![lane];
    order.extend(s.lanes().iter().copied().filter(|&l| l != lane && l.kind == lane.kind));
    for l in order {
        for c in s.lane_clips(l) {
            let meets = match edge {
                Edge::Start => c.end() == at,
                Edge::End => c.start == at,
            };
            if meets && !members.contains(&(l, c.start)) {
                return Some(parse::addr(l, c.start));
            }
        }
    }
    None
}

/// The words for the wall that clamps an edge at `at`: a neighbour wall is named by the clip that
/// actually meets the edge (see [`blocker_addr`]); the other walls read as [`wall_words`].
fn clamp_words(s: &PlaybackSession, members: &[(Lane, u32)], bound: WallBound, lane: Lane, edge: Edge, at: u32) -> String {
    match bound {
        WallBound::Neighbour(_) => match blocker_addr(s, members, lane, edge, at) {
            Some(addr) => format!("clamped where it meets {addr}"),
            None => wall_words(bound, edge, lane, at),
        },
        other => wall_words(other, edge, lane, at),
    }
}

/// The index of the take member on `lane` (the take holds one clip per lane).
fn idx_of(take: &[(Lane, usize)], lane: Lane) -> usize {
    take.iter().find(|&&(l, _)| l == lane).map_or(0, |&(_, i)| i)
}

/// Trims the whole take `take` (two or more linked clips) so the `edge` of every member
/// lands on one absolute time: `to`, clamped to the room every member has, so the shortest
/// source or the nearest neighbour sets the limit and the reply says which. The members are
/// unlinked for the move, each trimmed on its own, then grouped again.
#[allow(clippy::too_many_arguments)]
fn trim_take(
    s: &mut PlaybackSession,
    take: &[(Lane, usize)],
    edge: Edge,
    to: u32,
    fps: f64,
    addr: &str,
    side: &str,
    shown: &str,
    asked: Option<&Value>,
) -> Result<Done, String> {
    let mut lo = 0u32;
    let mut hi = u32::MAX;
    // The wall that set each limit, named by its member's bound before anything moves.
    let mut lo_wall: Option<(Lane, WallBound)> = None;
    let mut hi_wall: Option<(Lane, WallBound)> = None;
    for &(l, i) in take {
        let (Some((a, b)), Some((wl, wh))) = (s.trim_room(l, i, edge), s.trim_bounds(l, i, edge)) else {
            return Err(format!("trim {addr} refused: a member of its take has no {side} edge to trim"));
        };
        if a >= lo {
            lo = a;
            lo_wall = Some((l, wl));
        }
        if b <= hi {
            hi = b;
            hi_wall = Some((l, wh));
        }
    }
    if lo > hi {
        return Err(format!("trim {addr} {side} refused: the take has no room to move that edge"));
    }
    let target = to.clamp(lo, hi);
    let edge_at = |s: &PlaybackSession, l: Lane, i: usize| {
        s.lane_clips(l).get(i).map(|c| if edge == Edge::Start { c.start } else { c.end() })
    };
    // Zero length on the clicked clip (its other edge is what the in/out would cross): refused, not clamped.
    if let Some(c) = s.lane_clips(take[0].0).get(take[0].1).cloned() {
        let ok = if edge == Edge::End { to > c.start } else { to < c.end() };
        if !ok {
            return Err(format!("trim {addr} {side} to {shown} would leave no picture: its {side} edge must stay {} its other edge", if edge == Edge::End { "after" } else { "before" }));
        }
    }
    // Read before anything moves: the wall that set the limit the target reached.
    let members: Vec<(Lane, u32)> = take.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start))).collect();
    let reason = if target == to {
        None
    } else if target == hi {
        hi_wall.map(|(l, w)| clamp_words(s, &members, wall_named(s, l, idx_of(take, l), edge, hi, w), l, edge, hi))
    } else {
        lo_wall.map(|(l, w)| clamp_words(s, &members, wall_named(s, l, idx_of(take, l), edge, lo, w), l, edge, lo))
    };
    if take.iter().all(|&(l, i)| edge_at(s, l, i) == Some(target)) {
        let why = match &reason {
            Some(r) => format!(": {r}; it cannot go to {shown}"),
            None => ": nothing to trim".to_string(),
        };
        return Ok(plain(format!("{addr} {side} edge is already at {}{why}", parse::tc(target, fps)), Vec::new()));
    }
    for &(l, i) in take {
        s.ungroup(l, i);
    }
    for &(l, i) in take {
        if edge_at(s, l, i) == Some(target) {
            continue;
        }
        if !s.trim_clip(l, i, edge, target) {
            return Err(format!(
                "trim {addr} refused: {} would not take its edge at {}",
                parse::lane_name(l),
                parse::tc(target, fps)
            ));
        }
    }
    let regrouped = s.group_all(take).is_ok();
    let touched: Vec<(Lane, u32)> = take.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start))).collect();
    let own = touched.first().copied();
    let with: Vec<String> = touched.iter().skip(1).filter(|t| Some(**t) != own).map(|&(l, st)| parse::addr(l, st)).collect();
    let tail = if with.is_empty() { String::new() } else { format!(" (with {})", with.join(", ")) };
    let mut text = match asked_arrow(asked, fps, target) {
        Some(a) => format!("trim {addr} {side}: {a}{tail}"),
        None => format!("trim {addr} {side} → {}{tail}", parse::tc(target, fps)),
    };
    if let Some(r) = reason {
        text.push_str(&format!("; clamped from {shown} to {}: {r}", parse::tc(target, fps)));
    }
    if !regrouped {
        text.push_str("; take now detached: its members' spans no longer agree");
    }
    Ok(plain(text, touched))
}

/// Why a move refused. Checked in order: the lanes' locks, a kind mismatch,
/// then the first clip a member's requested span would run into (the group's
/// members at their shifted spans), and a fallback for the rest.
fn move_refusal(s: &PlaybackSession, lane: Lane, idx: usize, to: Lane, at: u32, at_shown: &str, take: &[(Lane, usize)]) -> String {
    let fps = parse::session_fps(s);
    let addr = addr_of(s, lane, idx);
    let target = parse::lane_name(to);
    if to.kind != lane.kind {
        return format!("move {addr} to {target}@{at_shown} refused: a clip moves only within its kind (V to V, A to A)");
    }
    let Some(clip) = s.lane_clips(lane).get(idx).cloned() else {
        return format!("no clip at {addr}");
    };
    let delta = i64::from(at) - i64::from(clip.start);
    let moving: Vec<(Lane, u32)> = take
        .iter()
        .filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start)))
        .collect();
    // Every collision is named: the clicked clip's, and each linked partner's at its own destination.
    let mut hits: Vec<String> = Vec::new();
    for &(l, i) in take {
        let Some(c) = s.lane_clips(l).get(i).cloned() else { continue };
        let dest = if (l, i) == (lane, idx) { to } else { l };
        let start = i64::from(c.start) + delta;
        if start < 0 {
            return format!("move {addr} to {target}@{at_shown} refused: the take would start before frame 0");
        }
        let start = start as u32;
        let who = if (l, i) == (lane, idx) { addr.to_string() } else { format!("its partner {}", addr_of(s, l, i)) };
        for o in overlapping(s, dest, start, start + c.frames(), &moving, fps) {
            hits.push(format!("{who} at {} would overlap {o}", parse::tc(start, fps)));
        }
    }
    if hits.is_empty() {
        return format!("move {addr} to {target}@{at_shown} refused: the engine would not place the take there (no clip is in its way)");
    }
    format!(
        "move {addr} to {target}@{at_shown} refused: {}; move or lift the clip(s) in the way first, pick another at, or insert:true to ripple the clip in (the later clips move to make room)",
        hits.join("; ")
    )
}

fn op_move(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let before = s
        .lane_clips(lane)
        .get(idx)
        .ok_or_else(|| format!("no clip at {addr}"))?.clone();
    let to = lane_arg(op, "to_lane")?.unwrap_or(lane);
    let at = need(op, "at", fps, end)?;
    // Already where it was asked to go: nothing moves, so there is nothing to refuse or undo.
    if to == lane && at == before.start {
        return Ok(plain(
            format!("move {addr}: already at {} — nothing to do", parse::tc(at, fps)),
            vec![(lane, before.start)],
        ));
    }
    // A move carries the whole take, so every lane the take is on (and the target) must be unlocked.
    let take = take_of(s, lane, idx);
    let mut lanes: Vec<Lane> = take.iter().map(|&(l, _)| l).collect();
    lanes.push(to);
    check_unlocked(s, &lanes)?;
    let at_shown = shown_time(op.get("at"), at);
    if insert_arg(op)? {
        return op_move_insert(s, op, fps, (lane, idx), (to, at), &before, &addr);
    }
    if !s.move_clip_to(lane, idx, to, at) {
        return Err(move_refusal(s, lane, idx, to, at, &at_shown, &take));
    }
    // Moves clamp `at` to the room the take has, so the new address is read
    // back, matching the clip by its source range (the one thing a move does not change).
    let touched: Vec<(Lane, u32)> = s
        .lane_clips(to)
        .iter()
        .find(|c| c.source == before.source && c.in_frame == before.in_frame && c.out_frame == before.out_frame)
        .map(|c| vec![(to, c.start)])
        .unwrap_or_default();
    // The batch line adds the new address after the arrow, so the text stops at the old one.
    let clamped = match touched.first() {
        Some(&(_, st)) if st != at => format!(" (asked for {}; the neighbours leave room only there)", parse::tc(at, fps)),
        _ => String::new(),
    };
    let mut text = format!("move {addr}{clamped}");
    // Titles follow the picture they sit over: one wholly over the clip travels with it by default,
    // `titles:true` carries the ones it only partly covers too, `titles:false` carries none. A title
    // that stays put is said so, with the title_set that moves it.
    if let (LaneKind::Video, Some(&(_, new_start))) = (lane.kind, touched.first()) {
        let carry_all = titles_arg(op)?;
        let delta = i64::from(new_start) - i64::from(before.start);
        let (old_start, old_end) = (i64::from(before.start), i64::from(before.end()));
        let mut moved = Vec::new();
        let mut plan = Vec::new();
        for (i, t) in s.titles().iter().enumerate() {
            let (ts, te) = (i64::from(t.start), i64::from(t.start) + i64::from(t.frames));
            if ts >= old_end || te <= old_start {
                continue;
            }
            let whole = ts >= old_start && te <= old_end;
            if carry_all.unwrap_or(whole) {
                let to_at = (ts + delta).max(0);
                moved.push(format!("T{} {}→{}", i + 1, secs_text(t.start, fps), secs_text(to_at as u32, fps)));
                let mut t2 = t.clone();
                t2.start = to_at as u32;
                plan.push((t.text.clone(), t.start, t2));
            } else {
                let to_at = (ts + delta).max(0);
                text.push_str(&format!(
                    "; T{} ({}) sat over {addr} and stays put — move it with the clip: title_set {{title:\"T{}\", at:\"{}f\"}}",
                    i + 1,
                    secs_text(t.frames, fps),
                    i + 1,
                    to_at
                ));
            }
        }
        for (text, from, t) in plan {
            retitle(s, &text, from, Some(t)).map_err(|e| format!("move {addr}: {e}"))?;
        }
        if !moved.is_empty() {
            text.push_str(&format!("; titles moved with the clip: {}", moved.join(", ")));
        }
    }
    Ok(plain(text, touched))
}

/// `insert` on a move: true for a ripple move, false (the default) for the slide.
fn insert_arg(op: &Value) -> Result<bool, String> {
    match op.get("insert") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("`insert` must be true or false".into()),
    }
}

/// The clip on `lane` that a move carried from `before`: found by its start and its source range.
fn find_moved(s: &PlaybackSession, lane: Lane, start: u32, before: &Clip) -> Option<usize> {
    s.lane_clips(lane)
        .iter()
        .position(|c| c.start == start && c.source == before.source && c.in_frame == before.in_frame && c.out_frame == before.out_frame)
}

#[allow(clippy::too_many_arguments)]
/// `move {clip, at, to_lane?, insert:true}`: a ripple move. The clip (its take with the linked
/// partners) leaves its slot, which closes on the ripple scope as `ripple_delete` closes one, and
/// lands at `at`, with the later clips on the scope lanes pushed to make room. Done in engine
/// steps that keep the clip whole: the room opens first, the clip moves into it, then the slot it
/// left closes. Titles over the clip carry with it, as a plain move carries them.
fn op_move_insert(
    s: &mut PlaybackSession,
    op: &Value,
    fps: f64,
    from: (Lane, usize),
    target: (Lane, u32),
    before: &Clip,
    addr: &str,
) -> Result<Done, String> {
    let (lane, idx) = from;
    let (to, at) = target;
    let (old, dur) = (before.start, before.frames());
    let target = parse::lane_name(to);
    let at_shown = shown_time(op.get("at"), at);
    if to == lane && old < at && at < old + dur {
        return Err(format!(
            "move {addr} insert to {target}@{at_shown} refused: the clip cannot be inserted inside its own span; pick a point outside {addr}"
        ));
    }
    let take = take_of(s, lane, idx);
    let mut sc = rip_scope(s, op, &take, old)?;
    if !sc.lanes.contains(&to) {
        sc.lanes.push(to);
    }
    check_unlocked(s, &sc.lanes)?;
    // The slot the clip leaves is cut out of the scope lanes once it has moved; a clip outside the take there is lost.
    let hit = unrelated_in_cut(s, &sc.lanes, &take, old, old + dur);
    if !hit.is_empty() {
        return Err(unrelated_refusal(
            &format!("move {addr} insert to {target}@{at_shown}"),
            addr,
            &hit,
            &format!("move {addr} without insert to leave the gap instead"),
        ));
    }
    let shown = (at_shown.clone(), format!("{:.1}s", parse::secs(dur, fps)));
    // Titles over the clip: the ones wholly over it travel, `titles:true` the partial ones too, `titles:false` none.
    let carry_all = titles_arg(op)?;
    let (old_i, old_e) = (i64::from(old), i64::from(old + dur));
    let mut carried: Vec<(usize, u32)> = Vec::new();
    let mut carried_text: Vec<String> = Vec::new();
    if lane.kind == LaneKind::Video {
        for (i, t) in s.titles().iter().enumerate() {
            let (ts, te) = (i64::from(t.start), i64::from(t.start) + i64::from(t.frames));
            if ts >= old_e || te <= old_i {
                continue;
            }
            let whole = ts >= old_i && te <= old_e;
            if carry_all.unwrap_or(whole) {
                let ns = (i64::from(at) + (ts - old_i)).max(0) as u32;
                carried.push((i, ns));
                carried_text.push(format!("T{} {}→{}", i + 1, secs_text(t.start, fps), secs_text(ns, fps)));
            }
        }
    }
    // The room opens, the clip moves into it, and the slot it left closes. Before `at` the room
    // opens at `at` and the slot is the one the clip shifted into; after it the room opens past
    // `at`, which is where the clip lands once the slot ahead of it has closed.
    // Captions over the picture travel with it: lifted out of the slot before it closes (the cut
    // would take them), and set down at the picture's new place once it has landed. A caption
    // A caption placement spanning one of the slot's edges would be split there, so by default
    // the move refuses it by name; `split_captions:true` splits it and reports both halves. Either
    // way each placement is then wholly over the picture or wholly beside it.
    let sub_lanes: Vec<Lane> = sc.lanes.iter().copied().filter(|l| l.kind == LaneKind::Subtitle).collect();
    // A caption grouped with some other take, lying over the slot, is refused before any cut: a
    // grouped caption moves its whole group when it is dragged, and cutting the slot would take its
    // cue. Captions grouped with the moved take itself travel with it.
    for &l in &sub_lanes {
        for c in s.sub_lane(l) {
            let Some(id) = c.link else { continue };
            if c.start >= old + dur || c.start + c.frames <= old {
                continue;
            }
            // A grouped caption wholly within the moved take's own span travels with it.
            if before.link == Some(id) && c.start >= old && c.start + c.frames <= old + dur {
                continue;
            }
            let p = c.start.max(old);
            let why = if before.link == Some(id) {
                "spans an edge of the moved take"
            } else {
                "is grouped with another take and lies over the slot the move closes"
            };
            return Err(format!(
                "move {addr} insert to {target}@{at_shown} refused: caption '{}' ({} {}–{}) is grouped and {why}; ungroup it or trim it first ({})",
                caption_words(s, c, p),
                parse::lane_name(l),
                secs_text(c.start, fps),
                secs_text(c.start + c.frames, fps),
                parse::addr(l, c.start)
            ));
        }
    }
    let split_caps = bool_arg(op, "split_captions")?;
    let edges = [old, old + dur];
    // Decided per cue: a placement may span an edge while no cue's own text does, and then the
    // placement is split between cues, which cuts no words. A cue whose text crosses an edge is
    // refused by name, unless `split_captions:true` allows the words to be cut.
    if !split_caps {
        for &p in &edges {
            for &l in &sub_lanes {
                for c in s.sub_lane(l) {
                    if !(c.start < p && p < c.start + c.frames) {
                        continue;
                    }
                    if let Some((words, a, b)) = crossing_cues(s, c, p).into_iter().next() {
                        return Err(format!(
                            "move {addr} insert to {target}@{at_shown} refused: cue '{words}' ({} {}–{}) crosses the edge of the moved clip; trim it, or pass split_captions:true to split it",
                            parse::lane_name(l),
                            secs_text(a, fps),
                            secs_text(b, fps)
                        ));
                    }
                }
            }
        }
    }
    let mut split_notes: Vec<String> = Vec::new();
    for p in edges {
        let spanning = spanning_captions(s, &sub_lanes, p);
        if !spanning.is_empty() {
            s.cut_at_lanes(parse::secs(p, fps), &sub_lanes);
            for c in &spanning {
                let lane = parse::lane_name(c.lane);
                let cut_cues = c.cues_cut.clone();
                if cut_cues.is_empty() {
                    split_notes.push(format!("{lane} split at {} between cues (no cue cut)", secs_text(p, fps)));
                    continue;
                }
                split_notes.push(format!(
                    "'{}' ({lane} {}–{}) split at {} into {lane} {}–{} and {lane} {}–{}",
                    c.words,
                    secs_text(c.start, fps),
                    secs_text(c.end, fps),
                    secs_text(p, fps),
                    secs_text(c.start, fps),
                    secs_text(p, fps),
                    secs_text(p, fps),
                    secs_text(c.end, fps)
                ));
            }
        }
    }
    let carry = carried_captions(s, &sc.lanes, old, dur);
    // The insert splits any clip it lands inside (the moved clip's own slot aside): named, not left to the effects.
    let p = if at <= old { at } else { at + dur };
    let split_at: Vec<String> = sc
        .lanes
        .iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .flat_map(|&l| {
            s.lane_clips(l)
                .iter()
                .filter(move |c| c.start < p && p < c.end() && !(l == lane && c.start == old))
                .map(move |c| format!("{} at {}", parse::addr(l, c.start), secs_text(p, fps)))
        })
        .collect();
    let cut_start = if at <= old {
        check_carry(addr, &carry, at)?;
        insert_span(s, at, dur, &sc, fps, shown)?;
        let i = find_moved(s, lane, old + dur, before)
            .ok_or_else(|| format!("move {addr} insert refused: the clip could not be followed through the ripple"))?;
        if !s.move_clip_to(lane, i, to, at) {
            return Err(format!("move {addr} to {target}@{at_shown} insert refused: the engine would not place the take there"));
        }
        carry_to(s, addr, &carry, at, dur, at as i64 - (old + dur) as i64)?;
        old + dur
    } else {
        // Once the old slot closes the timeline is `dur` shorter: a later `at` past that would leave a gap.
        let post_end = parse::session_end(s).saturating_sub(dur);
        if at > post_end {
            return Err(format!(
                "move {addr} insert to {target}@{at_shown} refused: past the timeline end once its slot closes ({:.1}s); use at {:.1}s or less, or append it",
                parse::secs(post_end, fps),
                parse::secs(post_end, fps)
            ));
        }
        check_carry(addr, &carry, at + dur)?;
        insert_span(s, at + dur, dur, &sc, fps, shown)?;
        let i = find_moved(s, lane, old, before)
            .ok_or_else(|| format!("move {addr} insert refused: the clip could not be followed through the ripple"))?;
        if !s.move_clip_to(lane, i, to, at + dur) {
            return Err(format!("move {addr} to {target}@{at_shown} insert refused: the engine would not place the take there"));
        }
        carry_to(s, addr, &carry, at + dur, dur, at as i64 + dur as i64 - old as i64)?;
        old
    };
    s.cut_regions(&[(cut_start, dur)], &sc.lanes)
        .map_err(|e| format!("move {addr} insert refused: {e}"))?;
    let mut moved = if has_picture(&sc.lanes) { move_titles_for_cuts(s, &[(cut_start, dur)], fps)? } else { Vec::new() };
    // A carried title is set where the clip put it, so the cut's move of it is not reported: one final position each.
    moved.retain(|m| !carried.iter().any(|&(i, _)| m.starts_with(&format!("T{} ", i + 1))));
    for &(i, ns) in &carried {
        if let Some(mut t) = s.titles().get(i).cloned() {
            t.start = ns;
            s.set_title(i, t);
        }
    }
    let pushed = if at <= old {
        format!("the clips from {at_shown} up to its old slot moved {:.1}s later", parse::secs(dur, fps))
    } else {
        format!("the clips between its old slot and {at_shown} moved {:.1}s earlier", parse::secs(dur, fps))
    };
    let mut text = format!("move {addr} insert to {target}@{at_shown}: its old slot closed and {pushed}");
    if !carried_text.is_empty() {
        text.push_str(&format!("; titles moved with the clip: {}", carried_text.join(", ")));
    }
    if !carry.is_empty() {
        text.push_str(&format!("; {} caption(s) moved with the clip", carry.len()));
    }
    if !split_at.is_empty() {
        text.push_str(&format!("; split {}", split_at.join(", ")));
    }
    if !split_notes.is_empty() {
        text.push_str(&format!("; captions split: {} (positions before the move)", split_notes.join("; ")));
    }
    text.push_str(&titles_note(&moved));
    text.push_str(&skipped_note(&sc.skipped));
    let mut touched = ripple_touched(s, &sc.lanes, at.min(old));
    if !touched.contains(&(to, at)) {
        touched.push((to, at));
    }
    Ok(Done { text, touched, lanes: sc.lanes })
}

/// A caption placement a ripple move lifts with the picture: its identity, and where it starts
/// before the ripple.
#[derive(Clone, Copy)]
struct Carry {
    lane: Lane,
    track: usize,
    win_in: i64,
    win_out: i64,
    frames: u32,
    start: u32,
}

/// The captions on `lanes` that overlap the slot `[old, old+dur)` a ripple move leaves: each
/// placement, whole, since a caption over any part of the picture is over the picture. Linked
/// captions are left to their take.
fn carried_captions(s: &PlaybackSession, lanes: &[Lane], old: u32, dur: u32) -> Vec<Carry> {
    let mut out = Vec::new();
    for &l in lanes.iter().filter(|l| l.kind == LaneKind::Subtitle) {
        for c in s.sub_lane(l) {
            if c.link.is_none() && c.start < old + dur && c.start + c.frames > old {
                out.push(Carry { lane: l, track: c.track, win_in: c.in_us, win_out: c.out_us, frames: c.frames, start: c.start });
            }
        }
    }
    out
}

/// Refuses before a ripple when a carried caption straddles `point`, where the ripple cuts:
/// the cut would split it, and it could not then be found to carry.
fn check_carry(addr: &str, carry: &[Carry], point: u32) -> Result<(), String> {
    match carry.iter().find(|c| c.start < point && point < c.start + c.frames) {
        Some(c) => Err(format!(
            "move {addr} insert refused: a caption over the picture spans {}, where the ripple opens; cut it there first",
            parse::addr(c.lane, c.start)
        )),
        None => Ok(()),
    }
}

/// Sets the carried captions down `delta` frames from where the ripple left them: each one at or
/// past `rippled_at` has moved `dur` later with the ripple, then all move by `delta` (the picture's
/// own shift), the ones moving away first so none lands on one not yet moved.
fn carry_to(s: &mut PlaybackSession, addr: &str, carry: &[Carry], rippled_at: u32, dur: u32, delta: i64) -> Result<(), String> {
    let mut now: Vec<(Carry, u32)> = carry
        .iter()
        .map(|c| (*c, if c.start >= rippled_at { c.start + dur } else { c.start }))
        .collect();
    now.sort_by_key(|&(_, st)| st);
    if delta > 0 {
        now.reverse();
    }
    for (c, st) in now {
        let idx = s
            .sub_lane(c.lane)
            .iter()
            .position(|x| x.start == st && x.frames == c.frames && x.track == c.track && x.in_us == c.win_in && x.out_us == c.win_out)
            .ok_or_else(|| format!("move {addr} insert refused: a caption over the picture could not be followed through the ripple"))?;
        let to = i64::from(st) + delta;
        let to = u32::try_from(to).map_err(|_| format!("move {addr} insert refused: a caption over the picture would start before the timeline"))?;
        s.move_sub(c.lane, idx, c.lane, to)
            .map_err(|e| format!("move {addr} insert refused: a caption over the picture would not move: {e}"))?;
    }
    Ok(())
}

/// A caption placement that spans a point a move cuts at: its lane, the words of the cue at the
/// point, and its span in frames.
struct Spanning {
    lane: Lane,
    words: String,
    start: u32,
    end: u32,
    /// The cues whose own text crosses the edge (their words, cut); empty when the placement
    /// spans the edge only between cues.
    cues_cut: Vec<String>,
}


/// The caption placements on `lanes` that span frame `p` strictly inside them, in lane order.
fn spanning_captions(s: &PlaybackSession, lanes: &[Lane], p: u32) -> Vec<Spanning> {
    let mut out = Vec::new();
    for &l in lanes {
        for c in s.sub_lane(l) {
            if c.start < p && p < c.start + c.frames {
                out.push(Spanning { lane: l, words: caption_words(s, c, p), start: c.start, end: c.start + c.frames, cues_cut: crossing_cues(s, c, p).into_iter().map(|x| x.0).collect() });
            }
        }
    }
    out
}

/// The cues a placement shows whose own timeline interval strictly contains frame `p`: their
/// words, and the frames their text starts and ends at (the placement's mapping of cue times, as
/// `cues_in_span` reads it). A placement that spans `p` between two cues gives none.
fn crossing_cues(s: &PlaybackSession, c: &engine::project::SubClip, p: u32) -> Vec<(String, u32, u32)> {
    let Some(track) = s.subtitles().get(c.track) else { return Vec::new() };
    let window = (c.out_us - c.in_us).max(1) as f64;
    let frames = f64::from(c.frames);
    let frame_of = |us: i64| c.start as f64 + (us - c.in_us) as f64 * frames / window;
    track
        .cues
        .iter()
        .filter(|q| q.start_us < c.out_us && q.end_us > c.in_us)
        .filter_map(|q| {
            let (a, b) = (frame_of(q.start_us), frame_of(q.end_us));
            // Frames, so 1e-3 is a thousandth of a frame: a cue that starts on
            // a cut maps to 59.999996 rather than 60 and must not read as crossing.
            if a + 1e-3 < f64::from(p) && f64::from(p) + 1e-3 < b {
                let text = q.text.replace('\n', " / ");
                let words: String = if text.chars().count() > 60 {
                    format!("{}…", text.chars().take(60).collect::<String>())
                } else {
                    text
                };
                Some((words, a.round() as u32, b.round() as u32))
            } else {
                None
            }
        })
        .collect()
}

/// The words of the cue a placement shows at frame `p` (or, between cues, the first cue it shows),
/// newlines as ` / `, cut to 60 characters.
fn caption_words(s: &PlaybackSession, c: &engine::project::SubClip, p: u32) -> String {
    let Some(track) = s.subtitles().get(c.track) else { return String::new() };
    let window = (c.out_us - c.in_us).max(1) as f64;
    let at_us = c.in_us + ((p - c.start) as f64 * window / f64::from(c.frames)) as i64;
    let cue = track
        .cues
        .iter()
        .find(|q| q.start_us <= at_us && at_us < q.end_us)
        .or_else(|| track.cues.iter().find(|q| q.start_us < c.out_us && q.end_us > c.in_us));
    match cue {
        Some(q) => {
            let text = q.text.replace('\n', " / ");
            if text.chars().count() > 60 {
                format!("{}…", text.chars().take(60).collect::<String>())
            } else {
                text
            }
        }
        None => String::new(),
    }
}

/// A true-or-false op key: false when omitted.
fn bool_arg(op: &Value, key: &str) -> Result<bool, String> {
    match op.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("`{key}` must be true or false")),
    }
}

/// `titles` on a move: true carries every title over the clip, false none; omitted, only the
/// titles wholly over it travel.
fn titles_arg(op: &Value) -> Result<Option<bool>, String> {
    match op.get("titles") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err("`titles` must be true or false".into()),
    }
}

/// The link of every item on `l` that starts at or after `from`, for the ripple ops.
fn links_from(s: &PlaybackSession, l: Lane, from: u32) -> Vec<Option<u32>> {
    if l.kind == LaneKind::Subtitle {
        s.sub_lane(l).iter().filter(|c| c.start >= from).map(|c| c.link).collect()
    } else {
        s.lane_clips(l).iter().filter(|c| c.start >= from).map(|c| c.link).collect()
    }
}

/// The lanes a ripple from `from` would move: the scope lanes holding anything at
/// or after it, and any other lane holding a link partner of such an item, since a
/// take moves whole. The lock check reads this set, so a locked partner refuses too.
fn ripple_lanes(s: &PlaybackSession, scope: &[Lane], from: u32) -> Vec<Lane> {
    let mut out: Vec<Lane> = Vec::new();
    let mut links: Vec<u32> = Vec::new();
    for &l in scope {
        let ls = links_from(s, l, from);
        if !ls.is_empty() && !out.contains(&l) {
            out.push(l);
        }
        links.extend(ls.into_iter().flatten());
    }
    for l in s.lanes() {
        if !out.contains(&l) && links_from(s, l, from).iter().flatten().any(|x| links.contains(x)) {
            out.push(l);
        }
    }
    out
}

/// The lanes a ripple moves for `scope`, closed over what it reaches: each cut point `cuts`
/// inside a clip brings that clip's take, and then `ripple_lanes` runs from `from` to a fixed
/// point. Refuses when a locked lane is among them, named or reached through a link: a take
/// moves whole, so its locked member would move too. Returns the closed lane list.
///
/// A locked caption lane that a cut point falls inside is not refused: it is left out and named
/// in the returned skipped list, as the default scope skips locked caption lanes. A locked picture
/// or sound lane is refused when a cut point falls inside one of its clips (the cut would have to
/// split it, and the ripple would leave its halves apart), and, with `default` (no `lanes` given),
/// when a clip on it ends after `from`: the default scope would otherwise leave it behind while the
/// lanes around it move. A locked partner that the take reaches is still refused, since a take
/// moves whole.
fn ripple_guard(s: &PlaybackSession, scope: &[Lane], from: u32, cuts: &[u32], default: bool) -> Result<(Vec<Lane>, Vec<Lane>), String> {
    let mut lanes: Vec<Lane> = scope.to_vec();
    let mut skipped: Vec<Lane> = Vec::new();
    let mut refused: Vec<Lane> = Vec::new();
    for &p in cuts {
        for l in s.lanes() {
            if l.kind == LaneKind::Subtitle {
                if s.sub_lane(l).iter().any(|c| c.start < p && p < c.start + c.frames) && !lanes.contains(&l) {
                    if s.lane_locked(l) {
                        if !skipped.contains(&l) {
                            skipped.push(l);
                        }
                    } else {
                        lanes.push(l);
                    }
                }
                continue;
            }
            for (i, c) in s.lane_clips(l).iter().enumerate() {
                if c.start < p && p < c.end() {
                    if s.lane_locked(l) && !lanes.contains(&l) {
                        if !refused.contains(&l) {
                            refused.push(l);
                        }
                        continue;
                    }
                    for (m, _) in take_of(s, l, i) {
                        if !lanes.contains(&m) {
                            lanes.push(m);
                        }
                    }
                }
            }
        }
    }
    if default {
        for l in s.lanes() {
            if l.kind != LaneKind::Subtitle && s.lane_locked(l) && !lanes.contains(&l) && s.lane_clips(l).iter().any(|c| c.end() > from) && !refused.contains(&l) {
                refused.push(l);
            }
        }
    }
    if let Some(&l) = refused.first() {
        return Err(format!("{}, or pass lanes without it: the ripple would move or cut it", lock_msg(l)));
    }
    loop {
        let before = lanes.len();
        for l in ripple_lanes(s, &lanes, from) {
            if !lanes.contains(&l) {
                lanes.push(l);
            }
        }
        if lanes.len() == before {
            break;
        }
    }
    check_unlocked(s, &lanes)?;
    Ok((lanes, skipped))
}

/// The clips and captions on lanes outside `scope` that start at or after `from`. A ripple
/// leaves them where they are, so the reply lists them (capped): empty when there are none.
pub(crate) fn left_behind(s: &PlaybackSession, scope: &[Lane], from: u32) -> String {
    const CAP: usize = 6;
    let mut items: Vec<String> = Vec::new();
    for l in s.lanes() {
        if scope.contains(&l) {
            continue;
        }
        if l.kind == LaneKind::Subtitle {
            items.extend(s.sub_lane(l).iter().filter(|c| c.start >= from).map(|c| parse::addr(l, c.start)));
        } else {
            items.extend(s.lane_clips(l).iter().filter(|c| c.start >= from).map(|c| parse::addr(l, c.start)));
        }
    }
    if items.is_empty() {
        return String::new();
    }
    let more = if items.len() > CAP { format!(" … {} more", items.len() - CAP) } else { String::new() };
    let shown: Vec<String> = items.iter().take(CAP).cloned().collect();
    format!("; left in place (not linked; pass lanes to move them): {}{more}", shown.join(", "))
}

/// The warning a ripple owes when it moves captions but leaves the sound under them where it was:
/// an audio lane outside `scope` with a clip that overlaps a caption stretch the edit moves keeps
/// its place while the words move, so the speech and its words part. The stretch of a caption in
/// `scope` that moves is the part from `to` (the end of what the edit removes, read before the cut)
/// on; a caption wholly before `to` does not move. Empty unless such a stretch and such a clip
/// both exist. `shift` is the move as the reply shows it, such as "−2.000s".
fn sound_drift(s: &PlaybackSession, scope: &[Lane], to: u32, shift: &str) -> String {
    let mut stretches: Vec<(u32, u32)> = Vec::new();
    let mut moved: Vec<String> = Vec::new();
    for l in s.subtitle_lanes() {
        if !scope.contains(&l) {
            continue;
        }
        let before = stretches.len();
        stretches.extend(s.sub_lane(l).iter().filter(|c| c.end() > to).map(|c| (c.start.max(to), c.end())));
        if stretches.len() > before {
            moved.push(parse::lane_name(l));
        }
    }
    let left: Vec<String> = s
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio && !scope.contains(l))
        .filter(|l| {
            s.lane_clips(*l)
                .iter()
                .any(|c| stretches.iter().any(|&(a, b)| c.start < b && a < c.end()))
        })
        .map(parse::lane_name)
        .collect();
    if moved.is_empty() || left.is_empty() {
        return String::new();
    }
    format!(
        "; captions on {} moved {shift} but {} (sound under them) stayed — they may no longer match the speech; add lanes:[…{}] or undo",
        moved.join(", "),
        left.join(", "),
        left.join(", ")
    )
}

/// The lanes a time falls strictly inside a clip or caption of: a cut there is needed first.
fn straddlers(s: &PlaybackSession, at: u32) -> Vec<Lane> {
    s.lanes()
        .into_iter()
        .filter(|&l| {
            if l.kind == LaneKind::Subtitle {
                s.sub_lane(l).iter().any(|c| c.start < at && at < c.start + c.frames)
            } else {
                s.lane_clips(l).iter().any(|c| c.start < at && at < c.end())
            }
        })
        .collect()
}

/// The lanes an insert or remove ripples: `lanes` as named (each must exist), else
/// every lane not locked.
fn ripple_scope(s: &PlaybackSession, op: &Value) -> Result<Vec<Lane>, String> {
    match lane_list(op, "lanes")? {
        Some(v) => {
            let all = s.lanes();
            match v.iter().find(|l| !all.contains(l)) {
                Some(&l) => Err(format!("lane {} is not on this timeline", parse::lane_name(l))),
                None => Ok(v),
            }
        }
        None => Ok(s.lanes().into_iter().filter(|&l| !s.lane_locked(l)).collect()),
    }
}

/// The default scope of insert_time and remove_time: every unlocked lane, with the locked caption
/// lanes listed as skipped (a note, never a refusal). `lanes` replaces it, with no skips.
fn default_scope(s: &PlaybackSession, op: &Value) -> Result<Scope, String> {
    let lanes = ripple_scope(s, op)?;
    let skipped = if lane_list(op, "lanes")?.is_some() {
        Vec::new()
    } else {
        s.lanes().into_iter().filter(|&l| l.kind == LaneKind::Subtitle && s.lane_locked(l)).collect()
    };
    let explicit = lane_list(op, "lanes")?.is_some();
    Ok(Scope { lanes, skipped, explicit })
}

/// What a rippling op moves: the lanes in scope, and the caption lanes it left
/// alone because they are locked and only came in by following the picture.
struct Scope {
    lanes: Vec<Lane>,
    skipped: Vec<Lane>,
    /// `lanes` was given by the caller, rather than being the default scope.
    explicit: bool,
}

pub(crate) fn has_picture(lanes: &[Lane]) -> bool {
    lanes.iter().any(|l| l.kind == LaneKind::Video)
}

/// The reply's note for the clips an insert cut in two, per lane: which clips were split, and the
/// silence (black on a picture lane) the inserted time leaves across the split. Empty when none.
fn split_note(splits: &[(Lane, Vec<String>)], at: u32, dur: u32, fps: f64) -> String {
    if splits.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = splits
        .iter()
        .map(|(l, names)| {
            let gap = if l.kind == LaneKind::Audio { "silence" } else { "black" };
            format!(
                "{} split {} at {:.3}s, leaving {:.3}s of {gap} at {:.3}–{:.3}s",
                parse::lane_name(*l),
                names.join(", "),
                parse::secs(at, fps),
                parse::secs(dur, fps),
                parse::secs(at, fps),
                parse::secs(at + dur, fps)
            )
        })
        .collect();
    format!("; {}", parts.join("; "))
}

/// The reply's note for caption lanes skipped as locked, empty when none were.
fn skipped_note(skipped: &[Lane]) -> String {
    if skipped.is_empty() {
        return String::new();
    }
    let names: Vec<String> = skipped.iter().map(|&l| parse::lane_name(l)).collect();
    let stays = if skipped.iter().all(|l| l.kind == LaneKind::Subtitle) { "its cues stay put" } else { "it stays put" };
    format!("; skipped locked {}: {stays}", names.join(", "))
}

/// The one ripple rule (EDIT_DOC, RIPPLE RULE). For an op that ripples from the clips
/// `edited` at `from`: the lanes of those clips, the lanes of any clip linked or grouped
/// to one of them, closed over what the ripple reaches (a take moves whole). On a picture
/// lane the caption lanes come too, so cues follow the picture; a locked one among them
/// is skipped with a note. `lanes` replaces all of it and must name the edited lanes.
fn rip_scope(s: &PlaybackSession, op: &Value, edited: &[(Lane, usize)], from: u32) -> Result<Scope, String> {
    let mut edit_lanes: Vec<Lane> = Vec::new();
    for &(l, _) in edited {
        if !edit_lanes.contains(&l) {
            edit_lanes.push(l);
        }
    }
    if let Some(v) = lane_list(op, "lanes")? {
        let all = s.lanes();
        if let Some(&l) = v.iter().find(|l| !all.contains(l)) {
            return Err(format!("lane {} is not on this timeline", parse::lane_name(l)));
        }
        if let Some(&l) = edit_lanes.iter().find(|l| !v.contains(l)) {
            return Err(format!("`lanes` must include {}, the lane being edited", parse::lane_name(l)));
        }
        return Ok(Scope { lanes: v, skipped: Vec::new(), explicit: true });
    }
    let mut lanes = edit_lanes.clone();
    for &(l, i) in edited {
        for (m, _) in take_of(s, l, i) {
            if !lanes.contains(&m) {
                lanes.push(m);
            }
        }
    }
    loop {
        let before = lanes.len();
        for l in ripple_lanes(s, &lanes, from) {
            if !lanes.contains(&l) {
                lanes.push(l);
            }
        }
        if lanes.len() == before {
            break;
        }
    }
    let mut skipped = Vec::new();
    if has_picture(&edit_lanes) {
        for l in s.lanes() {
            if l.kind != LaneKind::Subtitle || lanes.contains(&l) {
                continue;
            }
            if s.lane_locked(l) {
                skipped.push(l);
            } else {
                lanes.push(l);
            }
        }
    }
    Ok(Scope { lanes, skipped, explicit: false })
}

/// The clips a ripple cut over `[from, to)` would remove that the edit did not name: a clip on a
/// scope lane outside the take of the edited clips that overlaps the span. A ripple reaches such a
/// lane only through a later take's link, and the cut takes the unrelated clip with it, so the
/// picture under a deleted sound (or the reverse) would be lost. Returned as (lane, start).
fn unrelated_in_cut(s: &PlaybackSession, scope: &[Lane], edited: &[(Lane, usize)], from: u32, to: u32) -> Vec<(Lane, u32)> {
    let mut own: Vec<Lane> = Vec::new();
    for &(l, i) in edited {
        for (m, _) in take_of(s, l, i) {
            if !own.contains(&m) {
                own.push(m);
            }
        }
    }
    let mut out = Vec::new();
    for &l in scope {
        if l.kind == LaneKind::Subtitle || own.contains(&l) {
            continue;
        }
        out.extend(s.lane_clips(l).iter().filter(|c| c.start < to && c.end() > from).map(|c| (l, c.start)));
    }
    out
}

/// The refusal for a ripple whose cut would remove `hit` (from `unrelated_in_cut`). `head` is the
/// op as the reply names it, `addr` the clip the edit is about, and `first_way` the way out that
/// leaves the gap. The second way out lifts the clips in the way and closes the gaps they leave.
fn unrelated_refusal(head: &str, addr: &str, hit: &[(Lane, u32)], first_way: &str) -> String {
    let names: Vec<String> = hit.iter().map(|&(l, st)| parse::addr(l, st)).collect();
    let mut lanes: Vec<String> = Vec::new();
    for &(l, _) in hit {
        let n = parse::lane_name(l);
        if !lanes.contains(&n) {
            lanes.push(n);
        }
    }
    let closes: Vec<String> = lanes.iter().map(|n| format!("close_gaps {{lane:\"{n}\"}}")).collect();
    format!(
        "{head} refused: the ripple would close over {}, which is not part of {addr}'s take, and remove it; {first_way}, or lift {} too and {}",
        names.join(", "),
        names.join(", "),
        closes.join(" and ")
    )
}

/// The refusal for an insert whose cut at `at` would split a take: a clip on `scope` that `at` falls
/// inside has a take partner on a lane outside `scope`. The cut would split that partner too, and
/// the ripple would leave its half behind, so the op refuses and names the partner and the lanes to
/// add to `lanes`, as remove_time and cut_regions refuse it. A locked partner is left to the lock check.
fn split_take_refusal(s: &PlaybackSession, scope: &[Lane], at: u32, shown: &str) -> Option<String> {
    for &l in scope.iter().filter(|l| l.kind != LaneKind::Subtitle) {
        for (i, c) in s.lane_clips(l).iter().enumerate() {
            if !(c.start < at && at < c.end()) {
                continue;
            }
            let Some(&(pl, pi)) = take_of(s, l, i).iter().skip(1).find(|(m, _)| !scope.contains(m) && !s.lane_locked(*m)) else {
                continue;
            };
            let mut with: Vec<String> = scope.iter().map(|&x| format!("\"{}\"", parse::lane_name(x))).collect();
            with.push(format!("\"{}\"", parse::lane_name(pl)));
            return Some(format!(
                "insert_time {shown} refused: the {} clip at {} is one take with the {} clip at {}, and {} is not in lanes; cutting at {} would pull the take apart. Add {} to lanes (lanes:[{}]), or drop lanes to move the whole take",
                parse::lane_name(l),
                parse::addr(l, c.start),
                parse::lane_name(pl),
                parse::addr(pl, s.lane_clips(pl)[pi].start),
                parse::lane_name(pl),
                parse::tc(at, parse::session_fps(s)),
                parse::lane_name(pl),
                with.join(",")
            ));
        }
    }
    None
}

/// `insert_time {at, duration, lanes?}`: ripples empty time in at `at`, on the lanes
/// `lanes` names, else every unlocked lane. See `insert_span`.
fn op_insert_time(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let at = need(op, "at", fps, end)?;
    let dur = need(op, "duration", fps, u32::MAX)?;
    if dur == 0 {
        return Err("insert_time: duration must be more than 0".into());
    }
    // At or past the timeline end nothing lies after the cut, so nothing can move: say so, and say what lengthens it.
    if at >= end {
        let shown = shown_time(op.get("at"), at);
        // An empty timeline has no clip to hold a lead-in: the gap before a first clip exists only once a clip is placed after it.
        if end == 0 {
            let secs = format!("{:.3}s", f64::from(dur) / fps);
            return Ok(plain(
                format!(
                    "insert_time at {shown}: the timeline is empty, so no clip moved and nothing changed; a {secs} lead-in appears once a clip is placed after it, e.g. place {{source, lane:\"V1\", at:\"{secs}\"}}"
                ),
                Vec::new(),
            ));
        }
        return Ok(plain(
            format!(
                "insert_time at {shown}: nothing after the end to push; to lengthen the timeline add a clip with append {{source}} or place {{source, at:\"end\"}}, or a title past the end with title_add {{at, duration}}"
            ),
            Vec::new(),
        ));
    }
    let scope = default_scope(s, op)?;
    let shown = (shown_time(op.get("at"), at), shown_time(op.get("duration"), dur));
    let drift = caption_drift(s, op, &scope.lanes, at);
    let cut = cut_point(s, &cut_lanes(s, &scope.lanes, at), at);
    if let Some(msg) = split_take_refusal(s, &scope.lanes, cut, &shown.0) {
        return Err(msg);
    }
    let mut done = insert_span(s, cut, dur, &scope, fps, shown)?;
    if cut != at {
        done.text.push_str(&cut_moved_note(at, cut));
    }
    done.text.push_str(&drift);
    Ok(done)
}

/// The lanes a cut at `at` on `scope` makes: the scope, and the take partners of each clip the cut
/// runs through (a take is cut whole).
fn cut_lanes(s: &PlaybackSession, scope: &[Lane], at: u32) -> Vec<Lane> {
    let mut out: Vec<Lane> = scope.to_vec();
    for &l in scope.iter().filter(|l| l.kind != LaneKind::Subtitle) {
        for (i, c) in s.lane_clips(l).iter().enumerate() {
            if c.start < at && at < c.end() {
                for (m, _) in take_of(s, l, i) {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            }
        }
    }
    out
}

/// Whether a cut at frame `f` can be made on `lanes`: a slow clip plays each source frame over
/// several timeline frames, and the engine cuts it only where a source frame begins
/// ([`engine::Speed::split_at`]). At 0.5x only every second timeline frame is a cut point.
fn cuttable(s: &PlaybackSession, lanes: &[Lane], f: u32) -> bool {
    lanes.iter().filter(|l| l.kind != LaneKind::Subtitle).all(|&l| {
        s.lane_clips(l)
            .iter()
            .filter(|c| c.start < f && f < c.end())
            .all(|c| c.speed.split_at(c.len(), f - c.start).is_some())
    })
}

/// The frame a cut at `at` lands on across `lanes`: `at` itself when [`cuttable`], else the
/// nearest frame within 8 that is (the earlier one first on a tie), else `at`.
fn cut_point(s: &PlaybackSession, lanes: &[Lane], at: u32) -> u32 {
    if cuttable(s, lanes, at) {
        return at;
    }
    (1..=8u32)
        .flat_map(|d| [at.checked_sub(d), at.checked_add(d)])
        .flatten()
        .find(|&f| f > 0 && cuttable(s, lanes, f))
        .unwrap_or(at)
}

/// The start a span `at`..`at+dur` is cut from on `lanes`: `at` when both of its edges can be cut
/// there, else the nearest start within 8 frames whose two edges can (same length, so the amount
/// removed is the amount asked). `at` when no start does, and the sliver closing then owns it.
fn span_start(s: &PlaybackSession, lanes: &[Lane], at: u32, dur: u32) -> u32 {
    let both = |f: u32| cuttable(s, lanes, f) && cuttable(s, lanes, f + dur);
    if both(at) {
        return at;
    }
    (1..=8u32)
        .flat_map(|d| [at.checked_sub(d), at.checked_add(d)])
        .flatten()
        .find(|&f| f > 0 && both(f))
        .unwrap_or(at)
}

/// The reply's words for a cut that [`cut_point`] moved off the frame asked for.
fn cut_moved_note(asked: u32, cut: u32) -> String {
    format!("; the cut moved from {asked}f to {cut}f: a slow clip plays each source frame over several timeline frames, and {asked}f is between two showings of one")
}

/// The warning a ripple with explicit `lanes` owes when it leaves a caption lane out: the
/// captions on that lane at or after `at` stay put while the lanes they belong to move.
/// Empty unless `lanes` was given. Read before the edit, so the counts are the cues that
/// would have followed.
pub(crate) fn caption_drift(s: &PlaybackSession, op: &Value, lanes: &[Lane], at: u32) -> String {
    if !matches!(op.get("lanes"), Some(v) if !v.is_null()) {
        return String::new();
    }
    let anchor = lanes
        .iter()
        .find(|l| l.kind != LaneKind::Subtitle)
        .map_or_else(|| "the cut lanes".to_string(), |l| parse::lane_name(*l));
    let mut out = String::new();
    for l in s.subtitle_lanes() {
        if lanes.contains(&l) {
            continue;
        }
        // Cues, not placements: a placed track is one placement over many cues.
        let fps = parse::session_fps(s);
        let n = s
            .sub_lane_cues(l)
            .iter()
            .filter(|c| (c.start_us as f64 * fps / 1e6).round() as u32 >= at)
            .count();
        if n > 0 {
            let name = parse::lane_name(l);
            let word = if n == 1 { "cue" } else { "cues" };
            out.push_str(&format!("; {name} not in lanes: {n} {word} now out of sync with {anchor} (add {name} to lanes)"));
        }
    }
    out
}

/// Ripples `dur` frames of empty time in at `at` on the lanes of `scope`. A clip or
/// caption on those lanes that `at` falls inside is cut there first, with the partners
/// of what is cut (only those lanes are cut, so the others keep their clips whole);
/// then every scope item starting at or after `at` moves `dur` later, as one engine
/// move (a take's partners come with it), and titles starting at or after `at` move
/// too when the scope holds the picture.
fn insert_span(s: &mut PlaybackSession, at: u32, dur: u32, scope: &Scope, fps: f64, shown: (String, String)) -> Result<Done, String> {
    let scope_lanes = scope.lanes.clone();
    // The lanes that move: the scope closed over link partners. A locked one refuses, before any cut.
    let (closure, guard_skipped) = ripple_guard(s, &scope_lanes, at, &[at], !scope.explicit)?;
    let mut cut_on: Vec<Lane> = Vec::new();
    for &l in &scope_lanes {
        if straddlers(s, at).contains(&l) {
            if !cut_on.contains(&l) {
                cut_on.push(l);
            }
            let idxs: Vec<usize> = if l.kind == LaneKind::Subtitle {
                Vec::new()
            } else {
                s.lane_clips(l).iter().enumerate().filter(|(_, c)| c.start < at && at < c.end()).map(|(i, _)| i).collect()
            };
            for i in idxs {
                for (m, _) in take_of(s, l, i) {
                    if !cut_on.contains(&m) {
                        cut_on.push(m);
                    }
                }
            }
        }
    }
    // Every media clip the cut splits, per lane, read before the cut: the reply names them and the gap they leave.
    let mut split_lanes: Vec<(Lane, Vec<String>)> = Vec::new();
    for &l in cut_on.iter().filter(|l| l.kind != LaneKind::Subtitle) {
        let names: Vec<String> = s
            .lane_clips(l)
            .iter()
            .filter(|c| c.start < at && at < c.end())
            .map(|c| parse::addr(l, c.start))
            .collect();
        if !names.is_empty() {
            split_lanes.push((l, names));
        }
    }
    if !cut_on.is_empty() {
        check_unlocked(s, &cut_on)?;
        s.cut_at_lanes(parse::secs(at, fps), &cut_on);
        if scope_lanes.iter().any(|l| straddlers(s, at).contains(l)) {
            return Err(format!("insert_time: could not cut at {} to make room", parse::tc(at, fps)));
        }
    }
    check_unlocked(s, &scope_lanes)?;

    let mut media: Vec<(Lane, usize)> = Vec::new();
    let mut subs: Vec<(Lane, usize)> = Vec::new();
    for &l in &scope_lanes {
        if l.kind == LaneKind::Subtitle {
            subs.extend(s.sub_lane(l).iter().enumerate().filter(|(_, c)| c.start >= at).map(|(i, _)| (l, i)));
        } else {
            media.extend(s.lane_clips(l).iter().enumerate().filter(|(_, c)| c.start >= at).map(|(i, _)| (l, i)));
        }
    }
    let mut touched: Vec<(Lane, u32)> = media.iter().map(|&(l, i)| (l, s.lane_clips(l)[i].start + dur)).collect();
    touched.extend(subs.iter().map(|&(l, i)| (l, s.sub_lane(l)[i].start + dur)));

    match media.first().copied() {
        Some((dl, di)) => {
            let picks: Vec<(Lane, usize)> = media.iter().chain(subs.iter()).copied().collect();
            let start = s.lane_clips(dl)[di].start + dur;
            if !s.move_selection_to(&picks, dl, di, dl, start) {
                return Err(format!(
                    "insert_time {} refused: the items after it cannot all move {} later",
                    shown.0, shown.1
                ));
            }
        }
        None => {
            // Captions alone: the last of each lane moves first, so each index stays valid.
            for &(l, i) in subs.iter().rev() {
                let start = s.sub_lane(l)[i].start + dur;
                s.move_sub(l, i, l, start).map_err(|e| format!("insert_time: {e}"))?;
            }
        }
    }

    let tail: Vec<usize> = if has_picture(&scope_lanes) {
        s.titles().iter().enumerate().filter(|(_, t)| t.start >= at).map(|(i, _)| i).collect()
    } else {
        Vec::new()
    };
    for &i in tail.iter().rev() {
        let mut t = s.titles()[i].clone();
        t.start += dur;
        if !s.set_title(i, t) {
            return Err(format!("insert_time: title T{} would not move", i + 1));
        }
    }
    // Chapters at or after `at` move with the titles, by the same `dur`: a shift of everything past a
    // point keeps them one to a frame.
    if has_picture(&scope_lanes) {
        s.remap_chapters(|f| if f >= at { f + dur } else { f });
        s.remap_markers(|f| if f >= at { f + dur } else { f });
    }
    Ok(plain(
        format!(
            "insert_time {} for {}: {} clip(s), {} caption(s), {} title(s) moved{}{}{}",
            shown.0,
            shown.1,
            media.len(),
            subs.len(),
            tail.len(),
            split_note(&split_lanes, at, dur, fps),
            skipped_note(&[scope.skipped.clone(), guard_skipped].concat()),
            left_behind(s, &closure, at)
        ),
        touched,
    ))
}

/// `freeze {at, duration, lane?="V1", insert?=true}`: holds the picture at `at`.
/// The composed frame at `at` is saved as `<project>/freeze/<stem>-<frame>-<hash8>.png`,
/// the hash taken from its pixels (an identical frame reuses the file and its source),
/// imported as a still and placed at `duration`
/// on `lane`. `insert` ripples first, as `insert_time` does, so audio gets silence
/// and everything after moves `duration` later. Without `insert` the still
/// overwrites `lane`; with no `lane` it goes on a new top video lane over the picture.
/// The still is registered as a source: a failed batch or a no-op batch takes that source back (the
/// engine keeps sources out of undo, so an undo alone leaves it), and a dry run imports nothing.
/// FNV-1a over the frame's size and BGRA bytes, folded to 32 bits: the `<hash8>`
/// in a freeze file's name. Std has no stable hasher and this crate has no hash
/// dependency; the names only need to differ when the pixels do.
fn frame_hash(frame: &engine::decode::Frame) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in frame.width.to_le_bytes().into_iter().chain(frame.height.to_le_bytes()).chain(frame.bgra.iter().copied()) {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h >> 32) as u32 ^ h as u32
}

fn op_freeze(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32, project: Option<&Path>, dry: bool) -> Result<Done, String> {
    let at = need(op, "at", fps, end)?;
    let dur = need(op, "duration", fps, u32::MAX)?;
    if dur == 0 {
        return Err("freeze: duration must be more than 0".into());
    }
    let insert = match op.get("insert") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("freeze: `insert` must be true or false".into()),
    };
    let named = lane_arg(op, "lane")?;
    let mut lane = named.unwrap_or(Lane::V1);
    if lane.kind != LaneKind::Video {
        return Err(format!("freeze holds a picture: `lane` must be a video lane (V1..), not {}", parse::lane_name(lane)));
    }
    let project = project.ok_or("freeze needs a saved project to keep its still in: call project_save {path:\"/abs/file.edith\"} first")?;
    if at >= end {
        return Err(format!(
            "freeze at {} is past the end of the timeline ({}); pick a time before it",
            parse::tc(at, fps),
            parse::tc(end, fps)
        ));
    }
    let stem = project.file_stem().map_or_else(|| "untitled".to_string(), |f| f.to_string_lossy().into_owned());
    let dir = project.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).join("freeze");
    // Composed on every call (~100ms): the pixels decide the name, so a changed
    // timeline or grade can never be served a picture taken before it.
    let (proj, meta) = s.export_snapshot();
    let frame = engine::export::compose_at(&proj, &meta, at, meta.width, meta.height)
        .map_err(|e| format!("freeze: cannot compose frame {at}: {e}"))?;
    let file = format!("{stem}-{at}-{:08x}.png", frame_hash(&frame));
    let png = dir.join(&file);
    // A dry run writes no PNG and imports no source: a source outlives the rollback that a dry run
    // ends with, and the engine keeps sources out of undo, so it would stay in the project.
    let n = if dry {
        None
    } else {
        if !png.is_file() {
            let bytes = engine::decode::to_png_bytes(&frame)
                .map_err(|e| format!("freeze: cannot encode frame {at} as PNG: {e}"))?;
            // What this batch creates is noted, so a refused batch takes it back; a file that was there is never noted.
            if !dir.is_dir() {
                note_write(dir.clone());
            }
            std::fs::create_dir_all(&dir).map_err(|e| format!("freeze: cannot create {}: {e}", dir.display()))?;
            std::fs::write(&png, bytes).map_err(|e| format!("freeze: cannot write {}: {e}", png.display()))?;
            note_write(png.clone());
        }
        // The same pixels already imported as a source: place that one, no duplicate.
        let n = match s.sources().iter().position(|src| src.path == png) {
            Some(i) => i,
            None => s.import(&png).map_err(|e| format!("freeze: cannot import {}: {e}", png.display()))?,
        };
        if dur > s.file_frames(&png) {
            return Err(format!("freeze: {} is longer than a still can be held", parse::tc(dur, fps)));
        }
        Some(n)
    };
    // Named when a new lane was added for the still, so the reply says why a lane the caller did not name exists.
    let mut added_note = String::new();
    if named.is_none() && !insert {
        // Over the picture on a new top video lane: nothing underneath is touched.
        edit_fx::apply(s, &json!({ "op": "lane_add", "kind": "video" }), fps)
            .ok_or("freeze: lane_add is not available")?
            .map_err(|e| format!("freeze: {e}"))?;
        lane = s
            .lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Video)
            .max_by_key(|l| l.ord)
            .ok_or("freeze: no video lane after lane_add")?;
        added_note = format!(
            "; added lane {} for the still, since insert:false with no lane leaves the picture under it untouched",
            parse::lane_name(lane)
        );
    }
    check_unlocked(s, &[lane])?;
    // The still goes where the insert cuts nothing: at `at`, or at the clip's end when `at` is its last frame.
    // A slow clip the still's cut runs through can be cut only at some frames: the still follows the cut.
    let start = freeze_start(s, at);
    let pos = if insert { cut_point(s, &cut_lanes(s, &s.lanes(), start), start) } else { at };
    let mut touched = Vec::new();
    // An insert cuts any clip `pos` falls strictly inside, then leaves silence where the still
    // went. On an audio lane that is a hole in the sound, so name the clip it splits.
    let mut warning = String::new();
    if insert {
        for l in straddlers(s, pos).into_iter().filter(|l| l.kind == LaneKind::Audio) {
            if let Some(c) = s.lane_clips(l).iter().find(|c| c.start < pos && pos < c.end()) {
                warning = format!(
                    "\n   warning: the insert cut audio clip {} in two at {}, so its sound has a silent hole there; insert:false keeps the sound whole",
                    parse::addr(l, c.start),
                    parse::tc(pos, fps)
                );
            }
        }
        let pre = op_insert_time(
            s,
            &json!({ "at": format!("{pos}f"), "duration": format!("{dur}f") }),
            fps,
            end,
        )?;
        touched = pre.touched;
    }
    let held = if pos == at {
        String::new()
    } else if pos != start {
        cut_moved_note(start, pos)
    } else {
        format!(", holding frame {at}f: the clip ends at {pos}f, so nothing is cut")
    };
    let Some(n) = n else {
        // A dry run: the still is not placed, and the reply says so; the insert above is still reported.
        touched.push((lane, pos));
        return Ok(plain(
            format!(
                "freeze {:.3}s for {:.3}s on {} (dry run: the still is neither written nor placed){held}{added_note}{warning}",
                parse::secs(pos, fps),
                parse::secs(dur, fps),
                parse::lane_name(lane)
            ),
            touched,
        ));
    };
    if !s.place_at(lane, parse::secs(pos, fps), clip_of(n, 0, dur)) {
        return Err(format!(
            "freeze: {} at {} refused: the engine would not place the still there",
            parse::lane_name(lane),
            parse::tc(pos, fps)
        ));
    }
    touched.push((lane, pos));
    let text = format!(
        "freeze {:.3}s for {:.3}s on {} (src:{n} freeze/{file}){held}{added_note}{warning}",
        parse::secs(pos, fps),
        parse::secs(dur, fps),
        parse::lane_name(lane)
    );
    Ok(plain(text, touched))
}

/// Where an insert-freeze at `at` goes. An insert cuts each clip `at` falls inside; when `at`
/// is the last frame of every such clip, the cut would leave that frame alone after the still,
/// so the still goes at the clip's end instead and nothing is cut.
fn freeze_start(s: &PlaybackSession, at: u32) -> u32 {
    let ends: Vec<u32> = media_lanes(s)
        .into_iter()
        .flat_map(|l| s.lane_clips(l).iter().filter(|c| c.start < at && at < c.end()).map(|c| c.end()).collect::<Vec<_>>())
        .collect();
    if !ends.is_empty() && ends.iter().all(|&e| e == at + 1) {
        at + 1
    } else {
        at
    }
}

/// `remove_time {at, duration, lanes?}`: the `cut_regions` cut for one span, on the
/// scope lanes. Titles after the span move back by `duration`; a title that starts
/// inside it refuses the cut, since the cut would take it away.
fn op_remove_time(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let at = need(op, "at", fps, end)?;
    let dur = need(op, "duration", fps, u32::MAX)?;
    if dur == 0 {
        return Err("remove_time: duration must be more than 0".into());
    }
    if at >= parse::session_end(s) {
        return Ok(plain(
            format!("remove_time {} is past the timeline end: nothing to cut", shown_time(op.get("at"), at)),
            Vec::new(),
        ));
    }
    // The span starts where its edges can both be cut (a slow clip cannot be cut between two showings
    // of one source frame); the length does not change.
    let asked = at;
    let snap_lanes = ripple_scope(s, op)?;
    let at = span_start(s, &cut_lanes(s, &snap_lanes, asked), asked, dur);
    let to = at.saturating_add(dur);
    let (scope, guard_skipped) = if lane_list(op, "lanes")?.is_some() {
        let v = ripple_scope(s, op)?;
        check_unlocked(s, &v)?;
        (v, Vec::new())
    } else {
        ripple_guard(s, &ripple_scope(s, op)?, at, &[at, to], true)?
    };
    let drift = caption_drift(s, op, &scope, at);
    let picture = has_picture(&scope);
    // Titles follow the picture: one wholly inside the span goes with it, one that straddles an
    // edge is trimmed to what lies outside the span, and titles after it move back by `dur`.
    // (text, start as read, what it becomes: None removes it)
    let mut plan: Vec<(String, u32, Option<engine::project::Title>)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut trimmed: Vec<String> = Vec::new();
    let mut moved_back = 0;
    for (i, t) in s.titles().iter().enumerate().filter(|_| picture) {
        let (ts, te) = (t.start, t.start.saturating_add(t.frames));
        if te <= at {
            continue;
        }
        if ts >= to {
            let mut t2 = t.clone();
            t2.start -= dur;
            plan.push((t.text.clone(), ts, Some(t2)));
            moved_back += 1;
            continue;
        }
        // Overlaps the span: what is left is the part outside it, which closes up at `at`.
        let overlap = te.min(to) - ts.max(at);
        let kept = t.frames - overlap;
        if kept == 0 {
            removed.push(format!("T{} ({})", i + 1, secs_text(t.frames, fps)));
            plan.push((t.text.clone(), ts, None));
        } else {
            let mut t2 = t.clone();
            t2.start = ts.min(at);
            t2.frames = kept;
            trimmed.push(format!("T{} now {}–{}", i + 1, secs_text(t2.start, fps), secs_text(t2.start + kept, fps)));
            plan.push((t.text.clone(), ts, Some(t2)));
        }
    }
    // Clips on the scope lanes: one wholly inside the span is removed, one the span crosses is trimmed
    // to the part outside. Both are named before the cut, by the address the caller read.
    let mut clips_gone: Vec<String> = Vec::new();
    let mut clips_cut: Vec<String> = Vec::new();
    let mut clips_split: Vec<String> = Vec::new();
    for &l in scope.iter().filter(|l| l.kind != LaneKind::Subtitle) {
        for c in s.lane_clips(l) {
            let (cs, ce) = (c.start, c.end());
            if ce <= at || cs >= to {
                continue;
            }
            let who = format!("{} ({})", parse::addr(l, cs), source_name(s, c.source));
            if cs >= at && ce <= to {
                clips_gone.push(who);
            } else if cs < at && ce > to {
                // The span lies inside the clip: two pieces are left, the second one starting where the span was.
                let second = at + (ce - to);
                clips_split.push(format!(
                    "split {}: {} {}–{}, {} {}–{}",
                    parse::addr(l, cs),
                    parse::addr(l, cs),
                    secs_text(cs, fps),
                    secs_text(at, fps),
                    parse::addr(l, at),
                    secs_text(at, fps),
                    secs_text(second, fps)
                ));
            } else {
                let kept = c.frames() - (ce.min(to) - cs.max(at));
                let new_start = cs.min(at);
                clips_cut.push(format!("{who} now {}–{}", secs_text(new_start, fps), secs_text(new_start + kept, fps)));
            }
        }
    }
    let (cues_gone, cues_cut) = cues_in_span(s, &scope, at, to, fps);
    let sound = sound_drift(s, &scope, to, &format!("−{:.3}s", parse::secs(dur, fps)));
    s.cut_regions(&[(at, dur)], &scope).map_err(|e| e.to_string())?;
    let cues_cut = trimmed_cue_lines(s, &cues_cut);
    for (text, from, to_title) in plan {
        retitle(s, &text, from, to_title).map_err(|e| format!("remove_time: {e}"))?;
    }
    // Chapters follow the span as the titles do: one inside it lands at its start.
    if picture {
        s.remap_chapters(|f| title_shift(&[(at, dur)], f));
        s.remap_markers(|f| title_shift(&[(at, dur)], f));
    }
    // A split of a speed-changed clip rounds its piece to whole timeline frames, so the cut can leave
    // a frame or two between the piece and the clip after it: close those as the cut_regions op does.
    let mut sliver_moved: Vec<String> = Vec::new();
    let sliver = sliver_note(close_sliver_holes(s, &scope, fps, &mut sliver_moved));
    let mut titles_text = String::new();
    if !clips_gone.is_empty() {
        titles_text.push_str(&format!("; removed {}", clips_gone.join(", ")));
    }
    for split in &clips_split {
        titles_text.push_str(&format!("; {split}"));
    }
    if !clips_cut.is_empty() {
        titles_text.push_str(&format!("; trimmed {}", clips_cut.join(", ")));
    }
    if moved_back > 0 {
        titles_text.push_str(&format!("; {moved_back} title(s) moved back"));
    }
    if !trimmed.is_empty() {
        titles_text.push_str(&format!("; trimmed to the span's edge: {}", trimmed.join(", ")));
    }
    if !removed.is_empty() {
        titles_text.push_str(&format!("; removed with the span: {}", removed.join(", ")));
    }
    titles_text.push_str(&cue_cut_text(&cues_gone, &cues_cut));
    titles_text.push_str(&titles_note(&sliver_moved));
    let moved = if at != asked { cut_moved_note(asked, at) } else { String::new() };
    Ok(plain(
        format!(
            "remove_time {} for {}{titles_text}{drift}{}{}{sound}{sliver}{moved}",
            if at != asked { shown_time(Some(&json!(format!("{at}f"))), at) } else { shown_time(op.get("at"), at) },
            shown_time(op.get("duration"), dur),
            left_behind(s, &scope, at),
            skipped_note(&guard_skipped)
        ),
        ripple_touched(s, &scope, at),
    ))
}

/// A caption cue a cut trims, read before the cut: where its track time and its timeline span
/// sit, so `trimmed_cue_lines` can find the pieces the cut leaves.
struct CueCut {
    lane: Lane,
    track: usize,
    index: usize,
    words: String,
    track_start: i64,
    track_end: i64,
    old_start: i64,
    old_end: i64,
    new_start: i64,
    new_end: i64,
}

/// The caption cues a cut of `at..at+dur` on `lanes` would take: `(removed, trimmed)`. A cue is
/// removed only when every part of it on those lanes lies inside the span; a cue with any part
/// left outside (a placement cut in two counts as one cue) is trimmed. Removed cues name their
/// whole text and old times; trimmed cues are named after the cut, by `trimmed_cue_lines`. Read
/// before the cut. A cue's timeline place follows the placement that shows it, the way the engine
/// lays cues out (see `op_cue_add`).
fn cues_in_span(s: &PlaybackSession, lanes: &[Lane], at: u32, dur_end: u32, fps: f64) -> (Vec<String>, Vec<CueCut>) {
    let us = |f: u32| (f64::from(f) * 1e6 / fps).round() as i64;
    let (a, b) = (us(at), us(dur_end));
    // Where a timeline point lands once the span is cut out: before it stays, after it moves back, inside it is at its edge.
    let after_cut = |p: i64| -> i64 {
        if p <= a {
            p
        } else if p >= b {
            p - (b - a)
        } else {
            a
        }
    };
    // Per cue: its visible extent over every placement on these lanes, and whether any of it
    // lies outside the span or overlaps it. Keyed by (track, cue) so the reply lists them in order.
    struct Extent {
        lane: Lane,
        start: i64,
        end: i64,
        inside: bool,
        overlaps: bool,
    }
    let mut extents: std::collections::BTreeMap<(usize, usize), Extent> = std::collections::BTreeMap::new();
    for &l in lanes.iter().filter(|l| l.kind == LaneKind::Subtitle) {
        for c in s.sub_lane(l) {
            let Some(track) = s.subtitles().get(c.track) else { continue };
            let p0 = us(c.start);
            let window = (c.out_us - c.in_us).max(1) as f64;
            let rate = f64::from(c.frames) / fps * 1e6 / window;
            for (ci, cue) in track.cues.iter().enumerate() {
                // The part of the cue the placement shows: a cue reaching past its window's edge
                // is clamped to it, so the reply never names a time before the placement starts.
                let (a0, b0) = (cue.start_us.max(c.in_us), cue.end_us.min(c.out_us));
                if b0 <= a0 {
                    continue;
                }
                let ts = p0 + ((a0 - c.in_us) as f64 * rate).round() as i64;
                let te = p0 + ((b0 - c.in_us) as f64 * rate).round() as i64;
                let inside = ts >= a && te <= b;
                let overlaps = te > a && ts < b;
                let e = extents.entry((c.track, ci)).or_insert(Extent { lane: l, start: ts, end: te, inside: true, overlaps: false });
                e.start = e.start.min(ts);
                e.end = e.end.max(te);
                e.inside &= inside;
                e.overlaps |= overlaps;
            }
        }
    }
    let (mut gone, mut cut) = (Vec::new(), Vec::new());
    for ((track, ci), e) in extents.iter().filter(|(_, e)| e.overlaps) {
        let Some(cue) = s.subtitles().get(*track).and_then(|t| t.cues.get(*ci)) else { continue };
        let words = cue.text.replace('\n', " / ");
        let secs = |us: i64| format!("{:.3}s", us as f64 / 1e6);
        if e.inside {
            gone.push(format!("\"{words}\" ({}–{})", secs(e.start), secs(e.end)));
        } else {
            cut.push(CueCut {
                lane: e.lane,
                track: *track,
                index: *ci,
                words,
                track_start: cue.start_us,
                track_end: cue.end_us,
                old_start: e.start,
                old_end: e.end,
                new_start: after_cut(e.start),
                new_end: after_cut(e.end),
            });
        }
    }
    (gone, cut)
}

/// The caption lines a cut owes: the cues it removed and the cues it trimmed, each as its own `; ...` clause.
fn cue_cut_text(gone: &[String], cut: &[String]) -> String {
    let mut text = String::new();
    if !gone.is_empty() {
        text.push_str(&format!("; caption cue(s) removed: {}", gone.join(", ")));
    }
    if !cut.is_empty() {
        text.push_str(&format!("; caption cue(s) trimmed: {}", cut.join("; ")));
    }
    text
}

/// The trimmed cues of a cut, read after it: each as `S1 cue 89 'old words' → 'kept words' (was
/// old times, now new times)`. The kept words are the pieces the cut left of that cue: the track's cues inside its
/// track time that still sit under a placement's window (a piece the cut took out is none).
fn trimmed_cue_lines(s: &PlaybackSession, cut: &[CueCut]) -> Vec<String> {
    let secs = |us: i64| format!("{:.3}s", us as f64 / 1e6);
    cut.iter()
        .map(|c| {
            let windows: Vec<(i64, i64)> = s
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Subtitle)
                .flat_map(|l| s.sub_lane(l).iter().filter(|p| p.track == c.track).map(|p| (p.in_us, p.out_us)).collect::<Vec<_>>())
                .collect();
            let mut pieces: Vec<(i64, String)> = s
                .subtitles()
                .get(c.track)
                .map(|t| {
                    t.cues
                        .iter()
                        .filter(|q| q.image.is_none() && q.start_us >= c.track_start && q.end_us <= c.track_end)
                        .filter(|q| windows.iter().any(|&(a, b)| q.start_us < b && q.end_us > a))
                        .map(|q| (q.start_us, q.text.replace('\n', " / ")))
                        .collect()
                })
                .unwrap_or_default();
            pieces.sort_by_key(|p| p.0);
            let kept: Vec<String> = pieces.into_iter().map(|p| p.1).collect();
            format!(
                "{} cue {} '{}' → '{}' (was {}–{}, now {}–{})",
                parse::lane_name(c.lane),
                c.index + 1,
                c.words,
                kept.join(" "),
                secs(c.old_start),
                secs(c.old_end),
                secs(c.new_start),
                secs(c.new_end)
            )
        })
        .collect()
}

/// Every clip or caption at or after `from`, on the given lanes, after an edit
/// that ripples. These are the addresses a ripple changed.
fn ripple_touched(s: &PlaybackSession, lanes: &[Lane], from: u32) -> Vec<(Lane, u32)> {
    let mut out = Vec::new();
    for &l in lanes {
        if l.kind == LaneKind::Subtitle {
            out.extend(s.sub_lane(l).iter().filter(|c| c.start >= from).map(|c| (l, c.start)));
        } else {
            out.extend(s.lane_clips(l).iter().filter(|c| c.start >= from).map(|c| (l, c.start)));
        }
    }
    out
}

fn clips_of_source_at(s: &PlaybackSession, n: usize, at: u32) -> Vec<(Lane, u32)> {
    let mut out = Vec::new();
    for l in s.lanes() {
        if l.kind == LaneKind::Subtitle {
            continue;
        }
        if s.lane_clips(l).iter().any(|c| c.source == n && c.start == at) {
            out.push((l, at));
        }
    }
    out
}

/// The lanes of the take `(lane, idx)` belongs to: every lane a group-wide op touches.
fn take_lanes(s: &PlaybackSession, lane: Lane, idx: usize) -> Vec<Lane> {
    take_of(s, lane, idx).into_iter().map(|(l, _)| l).collect()
}

/// Where a title starting at `x` lands after `spans` (sorted, non-overlapping `(start, len)`
/// timeline spans) are cut out: `x` less the frames cut before it. A title inside a span lands
/// at the span's start, the cut point.
fn title_shift(spans: &[(u32, u32)], x: u32) -> u32 {
    let removed: u32 = spans.iter().map(|&(a, len)| x.min(a.saturating_add(len)).saturating_sub(a)).sum();
    x - removed
}

/// Moves every title for a cut of `spans` (see `title_shift`) and returns the moves as
/// `T2 6.500s→5.000s`. Titles follow the picture across a ripple, as the lanes do.
pub(crate) fn move_titles_for_cuts(s: &mut PlaybackSession, spans: &[(u32, u32)], fps: f64) -> Result<Vec<String>, String> {
    let mut moved = Vec::new();
    let mut sorted = spans.to_vec();
    sorted.sort_unstable();
    // Merged, so an overlap is not counted twice.
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for (a, len) in sorted {
        match spans.last_mut() {
            Some(last) if a <= last.0.saturating_add(last.1) => {
                last.1 = last.1.max(a.saturating_add(len) - last.0);
            }
            _ => spans.push((a, len)),
        }
    }
    // Read before any write. Each title is written by what it was (its text and start, as read): a write
    // can re-sort the list, so an index read before it may name another title after it.
    let titles: Vec<engine::project::Title> = s.titles().to_vec();
    for (i, t) in titles.iter().enumerate() {
        let (ts, te) = (t.start, t.start.saturating_add(t.frames));
        // A title inside a span is removed, named; one the span crosses is trimmed to the part outside it;
        // one after the span moves back. Titles keep their order (the shift is monotone).
        let inside: u32 = spans.iter().map(|&(a, len)| te.min(a.saturating_add(len)).saturating_sub(ts.max(a))).sum();
        let new = title_shift(&spans, ts);
        if inside == 0 {
            if new == ts {
                continue;
            }
            let mut t2 = t.clone();
            t2.start = new;
            retitle(s, &t.text, ts, Some(t2)).map_err(|e| format!("T{}: {e}", i + 1))?;
            moved.push(format!("T{} {:.3}s→{:.3}s", i + 1, parse::secs(ts, fps), parse::secs(new, fps)));
        } else if inside >= t.frames {
            retitle(s, &t.text, ts, None).map_err(|e| format!("T{}: {e}", i + 1))?;
            moved.push(format!("T{} {:.3}s removed with the cut", i + 1, parse::secs(ts, fps)));
        } else {
            let mut t2 = t.clone();
            t2.start = new;
            t2.frames = t.frames - inside;
            retitle(s, &t.text, ts, Some(t2)).map_err(|e| format!("T{}: {e}", i + 1))?;
            moved.push(format!("T{} trimmed to {:.3}–{:.3}s", i + 1, parse::secs(new, fps), parse::secs(new + t.frames - inside, fps)));
        }
    }
    // Chapters follow the cut as titles do: one after a span moves back, one inside it lands at the cut point.
    s.remap_markers(|f| title_shift(&spans, f));
    let dropped = s.remap_chapters(|f| title_shift(&spans, f));
    if dropped > 0 {
        moved.push(format!("{dropped} chapter(s) lost their frame: a chapter moved onto another's frame in the cut"));
    }
    Ok(moved)
}

/// Writes title `text` that starts at `from` (as the plan read it): `Some` sets it, `None` removes it.
/// Titles are kept in start order, so a title moved past another changes its index; each write
/// finds its title again by what it was, so the next one still lands on the right title.
pub(crate) fn retitle(s: &mut PlaybackSession, text: &str, from: u32, to: Option<engine::project::Title>) -> Result<(), String> {
    let idx = s
        .titles()
        .iter()
        .position(|t| t.text == text && t.start == from)
        .ok_or_else(|| format!("title {text:?} at {from}f is not there to change"))?;
    match to {
        Some(t) => match s.set_title(idx, t) {
            true => Ok(()),
            false => Err(format!("title {text:?} would not move")),
        },
        None => match s.remove_title(idx) {
            true => Ok(()),
            false => Err(format!("title {text:?} would not be removed")),
        },
    }
}

/// The reply's line for titles that a cut moved, empty when none did.
pub(crate) fn titles_note(moved: &[String]) -> String {
    if moved.is_empty() {
        String::new()
    } else {
        format!("; titles moved with the cut: {}", moved.join(", "))
    }
}

fn op_delete(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let clip = s.lane_clips(lane)[idx].clone();
    check_unlocked(s, &take_lanes(s, lane, idx))?;
    let span = [(clip.start, clip.frames())];
    // A take cut to one span on every member moves by the rule; a hand-built group whose
    // members differ keeps the engine's own law, each member's span out of its own lane.
    let take = take_of(s, lane, idx);
    let even = take.iter().all(|&(l, i)| {
        let c = &s.lane_clips(l)[i];
        c.start == clip.start && c.frames() == clip.frames()
    });
    if !even {
        let gone = vec![(lane, clip.start, clip.link)];
        if !s.delete_clip(lane, idx) {
            return Err(format!("delete {addr} refused: no such clip, or its lane is locked"));
        }
        let moved = move_titles_for_cuts(s, &span, fps)?;
        // The clips the ripple moved are named by the shifted list: no arrow list, since the address the
        // removed clip had is now another clip's.
        return Ok(plain(format!("delete {addr}{}{}", removed_words(&gone), titles_note(&moved)), Vec::new()));
    }
    let sc = rip_scope(s, op, &[(lane, idx)], clip.start)?;
    check_unlocked(s, &sc.lanes)?;
    let hit = unrelated_in_cut(s, &sc.lanes, &[(lane, idx)], clip.start, clip.end());
    if !hit.is_empty() {
        return Err(unrelated_refusal(&format!("delete {addr}"), &addr, &hit, &format!("lift {addr} to leave the gap instead")));
    }
    let gone = clips_within(s, &sc.lanes, clip.start, clip.end());
    let drift = caption_drift(s, op, &sc.lanes, clip.start);
    // Delete closes the gap the way ripple_delete does, so the caption cues it removes or trims are named the same way.
    let (cues_gone, cues_cut) = cues_in_span(s, &sc.lanes, clip.start, clip.end(), fps);
    s.cut_regions(&span, &sc.lanes).map_err(|e| format!("delete {addr} refused: {e}"))?;
    let cues_cut = trimmed_cue_lines(s, &cues_cut);
    let moved = if has_picture(&sc.lanes) { move_titles_for_cuts(s, &span, fps)? } else { Vec::new() };
    Ok(plain(
        format!("delete {addr}{}{}{}{drift}{}", removed_words(&gone), titles_note(&moved), skipped_note(&sc.skipped), cue_cut_text(&cues_gone, &cues_cut)),
        Vec::new(),
    ))
}

/// `+15f` / `-0.5s` / `00:00:01:00`: the signed distance an op's `by` asks, in frames.
fn need_by(op: &Value, fps: f64) -> Result<i64, String> {
    match op.get("by") {
        None | Some(Value::Null) => Err("missing `by`: give a signed distance such as 15f, -0.5s or +00:00:01:00".into()),
        Some(v) => parse::delta(v, fps).map_err(|e| format!("`by`: {e}")),
    }
}

/// A frame count with its sign, for `by` words: `+15f`, `-3f`.
fn signed(n: i64) -> String {
    format!("{n:+}f")
}

/// The addresses of the clips a roll/slide left at new starts, for the reply's `now at` list.
fn starts_of(s: &PlaybackSession, picks: &[(Lane, usize)]) -> Vec<(Lane, u32)> {
    picks.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start))).collect()
}

/// `roll {clip, by|to, edge?}`: the cut at the clip's out edge (`edge:"in"`: its in edge) moves, and
/// the two clips either side of it give and take the frames. Nothing else moves.
fn op_roll(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let at_in = match op.get("edge").and_then(Value::as_str) {
        None | Some("out") => false,
        Some("in") => true,
        Some(_) => return Err("`edge` must be \"in\" or \"out\": the cut at the clip's in edge or at its out edge".into()),
    };
    // The clip before the cut is the one the engine rolls by.
    let a_idx = if at_in {
        idx.checked_sub(1).ok_or_else(|| format!("roll {addr} edge:\"in\" refused: {addr} is the first clip on {}, so no cut lies at its in edge", parse::lane_name(lane)))?
    } else {
        idx
    };
    let clips = s.lane_clips(lane);
    let cut = match (clips.get(a_idx), clips.get(a_idx + 1)) {
        (Some(a), Some(b)) if a.end() == b.start => a.end(),
        (Some(a), Some(b)) => {
            return Err(format!(
                "roll {addr} refused: {} ends at {} and {} starts at {}; a gap, not a cut, lies between them (roll needs two clips butting at a cut; close_gaps first, or trim)",
                parse::addr(lane, a.start), parse::tc(a.end(), fps), parse::addr(lane, b.start), parse::tc(b.start, fps)
            ));
        }
        _ => {
            return Err(format!(
                "roll {addr} refused: there is no clip {} {addr} on {}, so there is no cut to move (roll needs two clips butting at a cut; trim moves a lone edge)",
                if at_in { "before" } else { "after" }, parse::lane_name(lane)
            ));
        }
    };
    let (want, shown, asked) = match (op.get("by").filter(|v| !v.is_null()), op.get("to").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => return Err("roll takes `by` or `to`, not both".into()),
        (None, None) => return Err("missing `by` or `to`: give the new cut place (to: 450f) or a signed move (by: -15f)".into()),
        (Some(_), None) => {
            let by = need_by(op, fps)?;
            ((i64::from(cut) + by).max(0), signed(by), None)
        }
        (None, Some(_)) => {
            let to = need(op, "to", fps, end)?;
            (i64::from(to), shown_time(op.get("to"), to), op.get("to"))
        }
    };
    let take = take_of(s, lane, a_idx);
    check_unlocked(s, &take.iter().map(|&(l, _)| l).collect::<Vec<_>>())?;
    let Some((lo, hi)) = s.roll_room(lane, a_idx) else {
        return Err(format!("roll {addr} refused: no cut there"));
    };
    let want = u32::try_from(want).unwrap_or(u32::MAX);
    if want == cut {
        return Ok(plain(format!("roll {addr}: the cut is already at {}: nothing to roll", parse::tc(cut, fps)), Vec::new()));
    }
    let target = want.clamp(lo, hi);
    let room = format!("the cut can travel {}–{} (the source media of the two clips, and a frame of each)", parse::tc(lo, fps), parse::tc(hi, fps));
    if target == cut {
        return Ok(plain(
            format!("roll {addr}: the cut is already at {}: clamped, it cannot go to {shown}: {room}", parse::tc(cut, fps)),
            Vec::new(),
        ));
    }
    let Some(landed) = s.roll_clip(lane, a_idx, target) else {
        return Err(format!("roll {addr} refused: the engine would not roll that cut"));
    };
    if landed == cut {
        return Ok(plain(
            format!("roll {addr}: the cut stayed at {}: no frame between it and {} is reachable by every clip of the take at its speed", parse::tc(cut, fps), parse::tc(target, fps)),
            Vec::new(),
        ));
    }
    let mut text = match asked.and_then(|a| asked_arrow(Some(a), fps, landed)) {
        Some(a) => format!("roll {addr} cut: {a}"),
        None => format!("roll {addr} cut {} → {}", parse::tc(cut, fps), parse::tc(landed, fps)),
    };
    if landed != target {
        text.push_str(&format!("; landed on {} (the nearest frame every clip of the take reaches at its speed)", parse::tc(landed, fps)));
    }
    if target != want {
        text.push_str(&format!("; clamped from {shown} to {}: {room}", parse::tc(target, fps)));
    }
    let mut picks = Vec::new();
    for &(l, i) in &take {
        let clips = s.lane_clips(l);
        if clips.get(i + 1).is_some_and(|b| clips[i].end() == b.start) {
            picks.extend([(l, i), (l, i + 1)]);
        }
    }
    let touched = starts_of(s, &picks);
    let with: Vec<String> = touched.iter().skip(2).map(|&(l, st)| parse::addr(l, st)).collect();
    if !with.is_empty() {
        text.push_str(&format!(" (with {})", with.join(", ")));
    }
    Ok(plain(text, touched))
}

/// `slip {clip, by}`: the clip keeps its place and length and plays footage `by` timeline frames
/// later (earlier for a negative `by`) in its source. A speed other than 1 makes that more or less
/// source: the reply says how much.
fn op_slip(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let by = need_by(op, fps)?;
    let take = take_of(s, lane, idx);
    check_unlocked(s, &take.iter().map(|&(l, _)| l).collect::<Vec<_>>())?;
    let Some((back, fwd)) = s.slip_room(lane, idx) else {
        return Err(format!("slip {addr} refused: a still image has no source frames to slip through"));
    };
    let before = s.lane_clips(lane)[idx].clone();
    if by == 0 {
        return Ok(plain(format!("slip {addr}: by 0f, nothing to slip"), Vec::new()));
    }
    let src = s.slip_source_delta(lane, idx, by).unwrap_or(by);
    // A ramp's mean rate: the source it reads over the timeline it fills.
    let speed = if before.speed_keys.is_empty() {
        before.speed.as_f64()
    } else {
        f64::from(before.len()) / f64::from(before.frames())
    };
    let conv = if src != by {
        format!(" ({} on the timeline is {} source frames at {}x)", signed(by), src, compact(speed))
    } else {
        String::new()
    };
    if src == 0 {
        return Ok(plain(format!("slip {addr}: {} is less than one source frame at {}x, nothing to slip", signed(by), compact(speed)), Vec::new()));
    }
    let Some(moved) = s.slip_clip(lane, idx, src) else {
        return Err(format!("slip {addr} refused: the engine would not slip that clip"));
    };
    let room = format!("the source allows {back}f back and +{fwd}f forward from here");
    if moved == 0 {
        return Ok(plain(format!("slip {addr}: the source window stayed at {}f–{}f: clamped, it cannot go {} source frames: {room}", before.in_frame, before.out_frame, src), Vec::new()));
    }
    let now = s.lane_clips(lane)[idx].clone();
    let mut text = format!(
        "slip {addr} by {}{conv}: source {}f–{}f → {}f–{}f, still {}–{} on the timeline",
        signed(by), before.in_frame, before.out_frame, now.in_frame, now.out_frame, parse::tc(now.start, fps), parse::tc(now.end(), fps)
    );
    if moved != src {
        text.push_str(&format!("; clamped from {src:+} to {moved:+} source frames: {room}"));
    }
    let touched = starts_of(s, &take);
    let with: Vec<String> = touched.iter().skip(1).map(|&(l, st)| parse::addr(l, st)).collect();
    if !with.is_empty() {
        text.push_str(&format!(" (with {})", with.join(", ")));
    }
    Ok(plain(text, touched))
}

/// `slide {clip, by}`: the clip moves `by` along its lane with its content and length unchanged; the
/// clip before it ends later or earlier and the clip after it starts later or earlier, so the
/// timeline's length stays. Needs a clip on each side.
fn op_slide(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let by = need_by(op, fps)?;
    let take = take_of(s, lane, idx);
    check_unlocked(s, &take.iter().map(|&(l, _)| l).collect::<Vec<_>>())?;
    let lane_name = parse::lane_name(lane);
    let n = s.lane_clips(lane).len();
    if idx == 0 || idx + 1 >= n {
        let side = if idx == 0 { "before" } else { "after" };
        return Err(format!(
            "slide {addr} refused: there is no clip {side} it on {lane_name}, so no neighbour can take up the move and the timeline would change length (slide needs a clip on each side; move places a clip freely)"
        ));
    }
    if let Some(&(l, i)) = take.iter().find(|&&(l, i)| i == 0 || i + 1 >= s.lane_clips(l).len()) {
        return Err(format!(
            "slide {addr} refused: its take member {} has no neighbour on each side of {}",
            addr_of(s, l, i), parse::lane_name(l)
        ));
    }
    let Some((lo, hi)) = s.slide_room(lane, idx) else {
        return Err(format!("slide {addr} refused: no clip there to slide"));
    };
    let start = s.lane_clips(lane)[idx].start;
    if by == 0 {
        return Ok(plain(format!("slide {addr}: by 0f, nothing to slide"), Vec::new()));
    }
    let want = u32::try_from((i64::from(start) + by).max(0)).unwrap_or(u32::MAX);
    let target = want.clamp(lo, hi);
    let room = format!("{addr} can start between {} and {} (the neighbours' source media, a frame of each, and any gap it must not enter)", parse::tc(lo, fps), parse::tc(hi, fps));
    if target == start {
        return Ok(plain(format!("slide {addr}: clamped, it cannot move {}: {room}", signed(by)), Vec::new()));
    }
    // Neighbours are named before the move; their indices do not change.
    let mut picks: Vec<(Lane, usize)> = Vec::new();
    for &(l, i) in &take {
        picks.extend([(l, i), (l, i - 1), (l, i + 1)]);
    }
    let Some(landed) = s.slide_clip(lane, idx, target) else {
        return Err(format!("slide {addr} refused: the engine would not slide that clip"));
    };
    if landed == start {
        return Ok(plain(
            format!("slide {addr}: stayed at {}: no start between it and {} is reachable by every neighbour at its speed", parse::tc(start, fps), parse::tc(target, fps)),
            Vec::new(),
        ));
    }
    let new_addr = parse::addr(lane, landed);
    let clips = s.lane_clips(lane);
    let mut text = format!(
        "slide {addr} by {}: now {new_addr}; {} now ends at {}, {} now starts at {}",
        signed(i64::from(landed) - i64::from(start)),
        parse::addr(lane, clips[idx - 1].start),
        parse::tc(clips[idx - 1].end(), fps),
        parse::addr(lane, clips[idx + 1].start),
        parse::tc(clips[idx + 1].start, fps)
    );
    if landed != target {
        text.push_str(&format!("; landed on {} (the nearest start every neighbour reaches at its speed)", parse::tc(landed, fps)));
    }
    if target != want {
        text.push_str(&format!("; clamped from {} to {}: {room}", signed(by), signed(i64::from(landed) - i64::from(start))));
    }
    let touched = starts_of(s, &picks);
    Ok(plain(text, touched))
}

/// `swap {a, b}`: two placements of one lane trade places. `b` takes `a`'s start, what lies
/// between them ripples by the difference in their lengths, and `a` lands where `b` was. The
/// span they cover keeps its length, so nothing after `b` moves. The reply lists every clip
/// that moved. One undo step.
fn op_swap(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    let a_raw = op.get("a").and_then(Value::as_str).ok_or("missing `a`: the first clip's address, such as V1@0")?;
    let b_raw = op.get("b").and_then(Value::as_str).ok_or("missing `b`: the second clip's address, such as V1@300f")?;
    let (la, ia) = clip_addr(s, a_raw)?;
    let (lb, ib) = clip_addr(s, b_raw)?;
    let who = format!("swap {a_raw} ↔ {b_raw}");
    if la != lb {
        return Err(format!(
            "{who}: the two clips are on {} and {}; swap trades two clips on one lane. Use move to carry one onto the other lane",
            parse::lane_name(la),
            parse::lane_name(lb)
        ));
    }
    if ia == ib {
        return Err(format!("{who}: the same clip twice; name two different clips"));
    }
    check_unlocked(s, &[la])?;
    let ripple = match op.get("ripple") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(format!("{who}: `ripple` must be true or false")),
    };
    if !ripple {
        return swap_in_slots(s, la, ia, ib, &who);
    }
    // A clip is in a take only when another clip shares its link; a lone link (what ungroup leaves) is not.
    if take_of(s, la, ia).len() > 1 || take_of(s, la, ib).len() > 1 {
        return Err(format!(
            "{who} refused: a clip in a take (grouped with another lane) is moved as a take, not swapped apart. Ungroup the take first"
        ));
    }
    let before = layout(s);
    let (la_len, lb_len) = (s.lane_clips(la)[ia].frames(), s.lane_clips(la)[ib].frames());
    if !s.swap_clips(la, ia, ib) {
        return Err(format!(
            "{who} refused: a clip in a take (grouped with another lane) is moved as a take, not swapped apart. Ungroup the take first"
        ));
    }
    // The earlier of the two now holds the later clip, and the later one holds the earlier.
    let (lo, hi) = (ia.min(ib), ia.max(ib));
    let now = s.lane_clips(la);
    let (at_lo, at_hi) = (parse::addr(la, now[lo].start), parse::addr(la, now[hi].start));
    let (later, earlier) = if ia > ib { (a_raw, b_raw) } else { (b_raw, a_raw) };
    let mut text = format!("{who}: {later} now at {at_lo}, {earlier} now at {at_hi}");
    // The clips that moved are listed once, on the batch's `shifted:` line.
    let moved_any = !shifted_pairs(&before, &layout(s)).is_empty();
    if !moved_any {
        text.push_str(" (nothing else moved)");
    }
    // A swap ripples what lies between the two by the difference in their lengths: say so, so a
    // shifted clip is explained. ripple:false keeps both slots where they were.
    // Said only when a clip moved: an unequal swap with nothing after it moves nothing.
    if la_len != lb_len && moved_any {
        text.push_str(&format!(
            " (the swapped clips differ by {}f, so later clips moved by {}f; use ripple:false to keep everyone else in place)",
            la_len.abs_diff(lb_len),
            la_len.abs_diff(lb_len)
        ));
    }
    Ok(plain(text, Vec::new()))
}

/// `swap {a, b, ripple:false}`: the two clips trade slots and nothing between them moves. The
/// earlier clip takes the later one's start and the later one the earlier's, so a shorter clip
/// leaves a gap in its slot and a longer one is refused where it would overlap what follows. The
/// engine checks the two clips at their final positions, so a clip may land on the slot the other
/// one leaves. Atomic: a refusal changes nothing, and the swap is one undo step.
fn swap_in_slots(s: &mut PlaybackSession, la: Lane, ia: usize, ib: usize, who: &str) -> Result<Done, String> {
    if take_of(s, la, ia).len() > 1 || take_of(s, la, ib).len() > 1 {
        return Err(format!(
            "{who} refused: a clip in a take (grouped with another lane) is moved as a take, not swapped apart. Ungroup the take first"
        ));
    }
    let (lo, hi) = if ia < ib { (ia, ib) } else { (ib, ia) };
    let (lo_start, hi_start) = (s.lane_clips(la)[lo].start, s.lane_clips(la)[hi].start);
    let (lo_len, hi_len) = (s.lane_clips(la)[lo].frames(), s.lane_clips(la)[hi].frames());
    s.swap_clips_in_place(la, ia, ib).map_err(|e| format!("{who} refused: {e}"))?;
    // The engine gives the earlier clip the later one's start and the later clip the earlier's.
    // Read from that rule rather than from a clip key: two clips with the same source range
    // would make a key lookup name the wrong one.
    // `a` and `b` are named as the caller gave them: the earlier one takes the later start.
    let (a_at, b_at) = if ia < ib { (hi_start, lo_start) } else { (lo_start, hi_start) };
    // A shorter clip leaves the rest of its slot empty: name each hole an unequal swap leaves.
    let mut holes = String::new();
    if lo_len > hi_len {
        let at = lo_start + hi_len;
        holes.push_str(&format!("; a gap of {}f is left after {} at {}", lo_len - hi_len, parse::addr(la, lo_start), parse::tc(at, parse::session_fps(s))));
    }
    if hi_len > lo_len {
        let at = hi_start + lo_len;
        holes.push_str(&format!("; a gap of {}f is left after {} at {}", hi_len - lo_len, parse::addr(la, hi_start), parse::tc(at, parse::session_fps(s))));
    }
    Ok(plain(
        format!(
            "{who}: first now at {}, second now at {}{holes}",
            secs_text(a_at, parse::session_fps(s)),
            secs_text(b_at, parse::session_fps(s))
        ),
        vec![(la, a_at), (la, b_at)],
    ))
}

fn op_lift(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    // Lifting a clip takes it out of its take: a locked partner would lose its link, so it refuses.
    check_unlocked(s, &take_lanes(s, lane, idx))?;
    check_unlocked(s, &[lane])?;
    if !s.lift_clip(lane, idx) {
        return Err(format!("lift {addr} refused: no such clip, or its lane is locked"));
    }
    Ok(Done { text: format!("lift {addr} (gap left)"), touched: Vec::new(), lanes: vec![lane] })
}

fn op_ripple_delete(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let clip = s.lane_clips(lane)[idx].clone();
    let sc = rip_scope(s, op, &[(lane, idx)], clip.start)?;
    check_unlocked(s, &sc.lanes)?;
    let hit = unrelated_in_cut(s, &sc.lanes, &[(lane, idx)], clip.start, clip.end());
    if !hit.is_empty() {
        return Err(unrelated_refusal(&format!("ripple_delete {addr}"), &addr, &hit, &format!("lift {addr} to leave the gap instead")));
    }
    let span = [(clip.start, clip.frames())];
    let gone = clips_within(s, &sc.lanes, clip.start, clip.end());
    let drift = caption_drift(s, op, &sc.lanes, clip.start);
    let sound =sound_drift(s, &sc.lanes, clip.end(), &format!("−{:.3}s", parse::secs(clip.frames(), fps)));
    // The caption cues inside the cut leave with it, so the reply names them (read before the cut, as remove_time does).
    let (cues_gone, cues_cut) = cues_in_span(s, &sc.lanes, clip.start, clip.end(), fps);
    s.cut_regions(&span, &sc.lanes).map_err(|e| format!("ripple_delete {addr} refused: {e}"))?;
    let cues_cut = trimmed_cue_lines(s, &cues_cut);
    let moved = if has_picture(&sc.lanes) { move_titles_for_cuts(s, &span, fps)? } else { Vec::new() };
    // The span it closed is named, so the reply says how much time went (the removed clips are side effects).
    let closed = format!(" closed {}–{}", secs_text(clip.start, fps), secs_text(clip.end(), fps));
    Ok(plain(
        format!("ripple_delete {addr}{closed}{}{}{}{}{sound}{drift}{}", removed_words(&gone), titles_note(&moved), skipped_note(&sc.skipped), left_behind(s, &sc.lanes, clip.start), cue_cut_text(&cues_gone, &cues_cut)),
        Vec::new(),
    ))
}

/// The gaps on a lane as (the clip after the gap, its identity, gap start, gap end). A gap is
/// closed when the clip after it no longer has one before it, which the identity tells.
fn gaps_of(s: &PlaybackSession, lane: Lane) -> Vec<(String, u32, u32)> {
    let mut out = Vec::new();
    let mut prev_end = 0u32;
    if lane.kind == LaneKind::Subtitle {
        for c in s.sub_lane(lane) {
            if c.start > prev_end {
                out.push((format!("caption {}:{}", c.track, c.in_us), prev_end, c.start));
            }
            prev_end = prev_end.max(c.start + c.frames);
        }
    } else {
        for c in s.lane_clips(lane) {
            if c.start > prev_end {
                out.push((format!("clip {}:{}", c.source, c.in_frame), prev_end, c.start));
            }
            prev_end = prev_end.max(c.end());
        }
    }
    out
}

/// Why close_gaps would leave the gap `start..start+frames` on `lane` open, in the words a fix needs.
/// None when it closes. The engine's own refusal (`gap_take_scope`) names the other take's gap
/// "there too" even when that lane has no gap there, so a partner with no gap of this span is named here.
fn take_gap_refusal(s: &PlaybackSession, lane: Lane, start: u32, frames: u32) -> Option<String> {
    let engine = match s.gap_take_scope(lane, start, frames) {
        Ok(_) => return None,
        Err(e) => e.to_string(),
    };
    let end = start + frames;
    let name = parse::lane_name(lane);
    for c in s.lane_clips(lane).iter().filter(|c| c.end() == start || c.start == end) {
        let Some(id) = c.link else { continue };
        for j in s.lanes() {
            if j == lane || j.kind == LaneKind::Subtitle {
                continue;
            }
            let Some(half) = s.lane_clips(j).iter().find(|o| o.link == Some(id)) else { continue };
            let other = parse::lane_name(j);
            if gaps_of(s, j).iter().any(|&(_, a, b)| a == start && b == end) {
                continue;
            }
            return Some(format!(
                "the {name} clip at {} is one take with the {other} clip at {}, and {other} has no gap of that length there: closing this one alone would pull the take out of sync, so the gap stays open (ungrouping would break the take)",
                parse::addr(lane, c.start),
                parse::addr(j, half.start)
            ));
        }
    }
    Some(engine)
}

fn op_close_gaps(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    let lane = lane_arg(op, "lane")?.ok_or("missing `lane`: give a lane such as A1")?;
    check_unlocked(s, &[lane])?;
    let fps = parse::session_fps(s);
    // An empty lane has no gap to close, and no end to measure against the timeline.
    let empty = if lane.kind == LaneKind::Subtitle { s.sub_lane(lane).is_empty() } else { s.lane_clips(lane).is_empty() };
    if empty {
        let name = parse::lane_name(lane);
        return Ok(Done { text: format!("close_gaps {name}: {name} is empty: nothing to close"), touched: Vec::new(), lanes: vec![lane] });
    }
    let before_gaps = gaps_of(s, lane);
    let before_layout = layout(s);
    // Read before the close, while each gap still has its place: the reason a take refuses it.
    let own_reasons: Vec<(u32, String)> = before_gaps
        .iter()
        .filter_map(|&(_, a, b)| take_gap_refusal(s, lane, a, b - a).map(|r| (a, r)))
        .collect();
    let report = s.close_all_gaps_on_lane(lane).map_err(|e| e.to_string())?;
    let after_gaps = gaps_of(s, lane);
    let span = |a: u32, b: u32| format!("{}–{}", secs_text(a, fps), secs_text(b, fps));
    let closed: Vec<String> = before_gaps
        .iter()
        .filter(|(who, _, _)| !after_gaps.iter().any(|(w, _, _)| w == who))
        .map(|&(_, a, b)| span(a, b))
        .collect();
    let open: Vec<String> = after_gaps.iter().map(|&(_, a, b)| span(a, b)).collect();
    let name = parse::lane_name(lane);
    // A lane with no gap at all says so, and its reply names no clip (nothing was closed or moved).
    let no_gaps = report.closed == 0 && report.skipped.is_empty() && open.is_empty();
    let mut text = if no_gaps {
        format!("close_gaps {name}: {name} has no gaps")
    } else {
        format!("close_gaps {name}: closed {}", report.closed)
    };
    if !closed.is_empty() {
        text.push_str(&format!(" ({})", closed.join(", ")));
    }
    if !report.skipped.is_empty() || !open.is_empty() {
        // Each gap says why the engine kept it open (its own refusal, a take that would split). The
        // skipped list runs left to right like the open spans, so each reason sits beside its span.
        let reasons: Vec<String> = report
            .skipped
            .iter()
            .map(|k| own_reasons.iter().find(|(a, _)| *a == k.start).map_or_else(|| k.reason.clone(), |(_, r)| r.clone()))
            .collect();
        let detail = if reasons.is_empty() {
            open.join(", ")
        } else if open.len() == reasons.len() {
            open.iter().zip(&reasons).map(|(span, why)| format!("{span}: {why}")).collect::<Vec<_>>().join("; ")
        } else {
            reasons.join("; ")
        };
        let detail = if detail.is_empty() { "times not read".to_string() } else { detail };
        text.push_str(&format!("; left open {} ({detail})", open.len().max(reasons.len())));
    }
    // Clips on other lanes that moved (a linked partner comes with the clip that closes the gap).
    let moved: Vec<String> = shifted_pairs(&before_layout, &layout(s))
        .into_iter()
        // Only the clips on other lanes: neither end of a pair may be on the lane that was closed.
        .filter(|(a, b)| !a.starts_with(&format!("{name}@")) && !b.starts_with(&format!("{name}@")))
        .map(|(a, b)| format!("{a}→{b}"))
        .collect();
    if !moved.is_empty() {
        text.push_str(&format!("; moved on other lanes: {}", moved.join(", ")));
    }
    // A gap the close leaves on another media lane (a music bed with no gap where this lane had one) is
    // named with its way out: its own close_gaps, or the take that keeps it open.
    let mut left: Vec<String> = Vec::new();
    for other in s.lanes().into_iter().filter(|&l| l != lane && l.kind != LaneKind::Subtitle).collect::<Vec<_>>() {
        let gaps = gaps_of(s, other);
        if gaps.is_empty() {
            continue;
        }
        let oname = parse::lane_name(other);
        let spans: Vec<String> = gaps.iter().map(|&(_, a, b)| span(a, b)).collect();
        let why = gaps
            .iter()
            .find_map(|&(_, a, b)| take_gap_refusal(s, other, a, b - a))
            .map(|r| format!(" (kept open: {r})"))
            .unwrap_or_else(|| format!(" (close with close_gaps {{lane:\"{oname}\"}})"));
        left.push(format!("{oname} {}{why}", spans.join(", ")));
    }
    if !left.is_empty() {
        text.push_str(&format!("; gaps left on other lanes: {}", left.join("; ")));
    }
    // A lane that ends before the timeline does not have a gap to close: its tail is empty time.
    let end = parse::session_end(s);
    let ends = lane_end(s, lane);
    if ends < end {
        // The item that sets the timeline end: a lane, or else a title (titles count toward the end).
        let longest = s
            .lanes()
            .into_iter()
            .find(|&l| lane_end(s, l) == end)
            .map(parse::lane_name)
            .or_else(|| s.titles().iter().position(|t| t.start + t.frames == end).map(|i| format!("T{}", i + 1)))
            .unwrap_or_else(|| "the timeline".to_string());
        text.push_str(&format!(
            "; {name} ends at {:.1}s, {:.1}s before the timeline end ({longest}); trim {longest} or extend {name} to reach it",
            parse::secs(ends, fps),
            parse::secs(end - ends, fps)
        ));
    }
    let touched = if no_gaps { Vec::new() } else { s.lane_clips(lane).iter().map(|c| (lane, c.start)).collect() };
    Ok(Done { text, touched, lanes: vec![lane] })
}

/// Where the last clip or caption on `lane` ends, in timeline frames (0 for an empty lane).
fn lane_end(s: &PlaybackSession, lane: Lane) -> u32 {
    if lane.kind == LaneKind::Subtitle {
        s.sub_lane(lane).iter().map(|c| c.start + c.frames).max().unwrap_or(0)
    } else {
        s.lane_clips(lane).iter().map(|c| c.end()).max().unwrap_or(0)
    }
}

/// The engine's CUT_SLIVER_FRAMES (crates/engine/src/project.rs): a kept piece shorter than this
/// between two cuts is a sliver no one can read, so the cuts either side of it are joined.
pub(crate) const CUT_SLIVER_FRAMES: u32 = 4;

/// Joins cut spans `(start, len)` that leave under [`CUT_SLIVER_FRAMES`] of kept picture between
/// them (or overlap). Returns the spans, sorted, and how many kept pieces were joined away.
pub(crate) fn join_slivers(spans: &[(u32, u32)]) -> (Vec<(u32, u32)>, usize) {
    let mut v: Vec<(u64, u64)> = spans.iter().map(|&(a, len)| (u64::from(a), u64::from(a) + u64::from(len))).collect();
    v.sort_unstable();
    let sliver = u64::from(CUT_SLIVER_FRAMES);
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    let mut joined = 0;
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a < last.1 + sliver => {
                if a > last.1 {
                    joined += 1;
                }
                last.1 = last.1.max(b);
            }
            _ => out.push((a, b)),
        }
    }
    (out.into_iter().map(|(a, b)| (a as u32, (b - a) as u32)).collect(), joined)
}

/// Closes the holes a cut left under [`CUT_SLIVER_FRAMES`] on the unlocked media lanes of `scope`.
/// A hole is closed by trimming a clip beside it (the clip after it starts earlier, or the clip
/// before it runs on), never by moving the clips after it: the cut then removes exactly the frames
/// it was asked for. A hole at the very start is no hole. `Ok(None)` when there is nothing to
/// close; otherwise the reply's note, or the error that kept the holes open (the holes it did
/// close stay closed).
pub(crate) fn close_sliver_holes(
    s: &mut PlaybackSession,
    scope: &[Lane],
    fps: f64,
    _moved: &mut Vec<String>,
) -> Result<Option<String>, String> {
    let mut lanes: Vec<String> = Vec::new();
    let mut closed = 0;
    let mut open: Vec<String> = Vec::new();
    for &l in scope {
        if l.kind == LaneKind::Subtitle || s.lane_locked(l) {
            continue;
        }
        let mut named = false;
        // Each pass closes the first hole left on the lane, then reads the lane again: one trim can
        // close a hole or move the next one, and a take partner's trim may have closed this one already.
        // A hole no trim can close is named and left, and the pass moves on to the next lane.
        let mut stuck: Vec<(u32, u32)> = Vec::new();
        while let Some((a, b)) = gaps_of(s, l)
            .into_iter()
            .filter(|&(_, a, b)| a > 0 && b - a < CUT_SLIVER_FRAMES)
            .map(|(_, a, b)| (a, b))
            .find(|h| !stuck.contains(h))
        {
            if !named {
                named = true;
                lanes.push(parse::lane_name(l));
            }
            let next = s.lane_clips(l).iter().position(|c| c.start == b);
            let prev = s.lane_clips(l).iter().position(|c| c.end() == a);
            if next.is_some_and(|i| s.trim_clip(l, i, Edge::Start, a)) || prev.is_some_and(|i| s.trim_clip(l, i, Edge::End, b)) {
                closed += 1;
            } else {
                stuck.push((a, b));
                open.push(format!("{} {}–{}", parse::addr(l, a), secs_text(a, fps), secs_text(b, fps)));
            }
        }
    }
    if lanes.is_empty() {
        return Ok(None);
    }
    if !open.is_empty() {
        return Err(format!("no clip beside {} can be trimmed into the hole", open.join(", ")));
    }
    Ok(Some(format!("; closed {closed} hole(s) under {CUT_SLIVER_FRAMES}f on {} by trimming the clip beside each", lanes.join(", "))))
}

/// The note for a [`close_sliver_holes`] that could not close: the holes stay, and the reason.
pub(crate) fn sliver_note(r: Result<Option<String>, String>) -> String {
    match r {
        Ok(note) => note.unwrap_or_default(),
        Err(e) => format!("; sliver holes left open: {e}"),
    }
}

/// Whether a clip or caption on one of `scope`'s lanes lies in any of the `spans` ((start, length) pairs).
fn scope_content_in(s: &PlaybackSession, scope: &[Lane], spans: &[(u32, u32)]) -> bool {
    spans.iter().any(|&(a, len)| {
        let b = a + len;
        scope.iter().any(|&l| {
            if l.kind == LaneKind::Subtitle {
                s.sub_lane(l).iter().any(|c| c.start < b && a < c.end())
            } else {
                s.lane_clips(l).iter().any(|c| c.start < b && a < c.end())
            }
        })
    })
}

fn op_cut_regions(s: &mut PlaybackSession, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    let list = op
        .get("regions")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())
        .ok_or("missing `regions`: give [[from,to], ...] times")?;
    let mut spans: Vec<(u32, u32)> = Vec::with_capacity(list.len());
    for (i, r) in list.iter().enumerate() {
        let pair = r
            .as_array()
            .filter(|p| p.len() == 2)
            .ok_or_else(|| format!("regions[{i}] must be [from,to]"))?;
        let a = parse::frame(&pair[0], fps, end).map_err(|e| format!("regions[{i}] from: {e}"))?;
        let b = parse::frame(&pair[1], fps, end).map_err(|e| format!("regions[{i}] to: {e}"))?;
        if b <= a {
            return Err(format!("regions[{i}]: to ({b}f) must be after from ({a}f)"));
        }
        spans.push((a, b - a));
    }
    let (spans, joined) = join_slivers(&spans);
    let from = spans.iter().map(|p| p.0).min().unwrap_or(0);
    let (scope, guard_skipped) = match lane_list(op, "lanes")? {
        Some(v) => {
            check_unlocked(s, &v)?;
            (v, Vec::new())
        }
        None => {
            let unlocked: Vec<Lane> = s.lanes().into_iter().filter(|&l| !s.lane_locked(l)).collect();
            let cuts: Vec<u32> = spans.iter().flat_map(|&(a, len)| [a, a + len]).collect();
            ripple_guard(s, &unlocked, from, &cuts, true)?
        }
    };
    let end_before = end;
    s.cut_regions(&spans, &scope).map_err(|e| e.to_string())?;
    let mut moved = if has_picture(&scope) { move_titles_for_cuts(s, &spans, fps)? } else { Vec::new() };
    let closed = sliver_note(close_sliver_holes(s, &scope, fps, &mut moved));
    // Nothing on the scope lanes lies in a span and nothing else moved: the reply says so, since the
    // batch then reads as no change (its reason is taken from this text).
    let mut text = if !scope_content_in(s, &scope, &spans) && moved.is_empty() && closed.is_empty() {
        let names: Vec<String> = scope.iter().map(|&l| parse::lane_name(l)).collect();
        format!("cut_regions {} span(s): nothing to cut, nothing on {} lies in the span(s)", spans.len(), names.join(", "))
    } else {
        format!("cut_regions {} span(s){}{closed}", spans.len(), titles_note(&moved))
    };
    text.push_str(&left_behind(s, &scope, from));
    text.push_str(&skipped_note(&guard_skipped));
    if joined > 0 {
        text.push_str(&format!(
            "; joined {joined} cut(s) across kept pieces under {CUT_SLIVER_FRAMES}f (a sliver that short cannot be read)"
        ));
    }
    // A title that runs past the cut keeps the timeline where it was: say which one.
    let new_end = parse::session_end(s);
    if new_end > 0 && spans.iter().any(|&(a, len)| a.saturating_add(len) >= end_before)
        && let Some((i, t)) = s.titles().iter().enumerate().find(|(_, t)| t.start.saturating_add(t.frames) >= new_end)
    {
        text.push_str(&format!(
            "; timeline still {}: T{} runs to {}",
            secs_text(new_end, fps),
            i + 1,
            secs_text(t.start.saturating_add(t.frames), fps)
        ));
    }
    Ok(Done { text, touched: ripple_touched(s, &scope, from), lanes: scope })
}

// ---- time and look (speed, fades, transitions) -----------------------------

/// `reverse {clip, on?}`: the clip and its take play backward, or forward again. `on` is explicit,
/// or absent to toggle. The length and place do not change, so nothing ripples.
fn op_reverse(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let on = match op.get("on") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(b)) => Some(*b),
        Some(_) => return Err("`on` must be true or false".into()),
    };
    let now = s.reversed_of(lane, idx);
    let want = on.unwrap_or(!now);
    check_unlocked(s, &take_lanes(s, lane, idx))?;
    if want == now {
        let state = if now { "reversed" } else { "forward" };
        return Ok(plain(format!("reverse {addr}: already {state}"), Vec::new()));
    }
    s.set_reverse(lane, idx, want).map_err(|e| format!("reverse {addr} refused: {e}"))?;
    let touched: Vec<(Lane, u32)> = take_of(s, lane, idx)
        .into_iter()
        .filter_map(|(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start)))
        .collect();
    let state = if want { "plays backward" } else { "plays forward again" };
    Ok(plain(format!("reverse {addr}: the clip {state}"), touched))
}

/// `beats {clip, every?}`: listens to a clip's sound for its beat, and marks every `every`th beat
/// (1 by default; 2 or 4 for bars) as a yellow comment marker noted "Beat". The clip's earlier beat
/// markers are taken off first, so a second run replaces the first rather than doubling it.
fn op_beats(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let every = match op.get("every") {
        None | Some(Value::Null) => 1,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=64).contains(n))
            .ok_or("beats: `every` is a whole number of beats, 1 for every beat, 2 or 4 for bars")? as usize,
    };
    let clip = s.lane_clips(lane).get(idx).cloned().ok_or_else(|| format!("beats {addr}: no such clip"))?;
    let src = s
        .sources()
        .get(clip.source)
        .ok_or_else(|| format!("beats {addr}: a clip uses src:{}, which is not on the timeline", clip.source))?;
    let range = (f64::from(clip.in_frame) / fps, f64::from(clip.out_frame) / fps);
    let found = beats::detect(&src.path, src.audio_stream, range)
        .map_err(|e| format!("beats {addr}: reading the sound of src:{}: {e}", clip.source))?;
    let (bpm, times) = match found {
        Found::Beats { bpm, times } => (bpm, times),
        Found::NoAudio => return Err(format!("beats {addr}: src:{} has no sound; pick a clip with sound", clip.source)),
        Found::Silent => return Err(format!("beats {addr}: the sound is silent, so there is no beat to mark")),
        Found::NoPulse => return Err(format!("beats {addr}: no steady beat in the sound to keep time to")),
    };
    let frames = beats::timeline_beats(&clip, fps, &beats::every_nth(&times, every));
    let old = beats::beat_marker_indices(s.markers(), clip.start, clip.end());
    for &i in old.iter().rev() {
        s.remove_marker(i);
    }
    for &f in &frames {
        s.add_marker(beats::beat_marker(f)).map_err(|e| format!("beats {addr}: {e}"))?;
    }
    let names: Vec<String> = frames.iter().map(|f| format!("M@{f}")).collect();
    let listed = match names.len() {
        0 => "none".to_string(),
        n if n <= 24 => names.join(", "),
        n => format!("{} … {} (and {} more)", names[..12].join(", "), names[n - 1], n - 12 - 1),
    };
    let text = format!(
        "beats {addr}: {bpm:.1} BPM; {} beat markers (every {every}) at {listed}, replacing {} earlier; markers: {}",
        frames.len(),
        old.len(),
        marker_names(s)
    );
    Ok(plain(text, vec![(lane, clip.start)]))
}

/// `speed {clip, x, ripple?}`. Without `ripple` a clip that would grow into its neighbour
/// refuses. With it the later items on the unlocked lanes move by the length change, as
/// `insert_time` (slower) or `cut_regions` (faster) would move them, and titles move with them.
fn op_speed(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let x = match op.get("x") {
        None | Some(Value::Null) => return Err("missing `x`: give a speed between 0.25 and 4".into()),
        Some(v) => v.as_f64().ok_or_else(|| format!("`x` must be a number between 0.25 and 4, not {v}"))?,
    };
    if !(0.25..=4.0).contains(&x) {
        return Err(format!("speed {} is outside 0.25–4", compact(x)));
    }
    let speed = Speed::from_permille((x * 1000.0).round() as u16);
    check_unlocked(s, &take_lanes(s, lane, idx))?;
    let new_frames = speed.frames(s.lane_clips(lane)[idx].len());
    let label = format!("speed {addr} ×{}", compact(x));
    let why = format!("at {x:.2}x");
    retime_clip(s, op, fps, lane, idx, &label, &why, new_frames, &|s: &mut PlaybackSession| {
        s.set_speed(lane, idx, speed).map_err(|e| e.to_string())
    })
}

/// `speed_ramp {clip, keys:[{at, speed}...], ripple?, lanes?}`: a speed that changes over the clip.
/// Each key is a speed (0.25–4) at a clip-relative time `at` ("0s", "2s", "end" for the clip's own
/// length, or frames); the speed is linear between keys and holds before the first and after the
/// last. Empty `keys` clears the ramp, and the clip takes its constant speed again. Its length
/// follows from the ramp, and `ripple` moves the later clips as `speed` does.
fn op_speed_ramp(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    check_unlocked(s, &take_lanes(s, lane, idx))?;
    let clip = s.lane_clips(lane)[idx].clone();
    let clip_frames = clip.frames();
    let raw = match op.get("keys") {
        None | Some(Value::Null) => return Err("missing `keys`: give a list of {at, speed}, or [] to clear the ramp".into()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err("`keys` must be a list of {at, speed} objects".into()),
    };
    if raw.len() > engine::project::MAX_SPEED_KEYS {
        return Err(format!("a speed ramp takes at most {} keys", engine::project::MAX_SPEED_KEYS));
    }
    let mut keys = Vec::with_capacity(raw.len());
    for k in raw {
        let at = k.get("at").ok_or_else(|| "each key needs `at`: a time such as 0s, 2s or end".to_string())?;
        let at = parse::frame(at, fps, clip_frames)?;
        let speed = match k.get("speed") {
            None | Some(Value::Null) => return Err("each key needs `speed`: a number between 0.25 and 4".into()),
            Some(v) => v.as_f64().ok_or_else(|| format!("`speed` must be a number between 0.25 and 4, not {v}"))?,
        };
        if !(0.25..=4.0).contains(&speed) {
            return Err(format!("speed {} is outside 0.25–4", compact(speed)));
        }
        keys.push(engine::project::SpeedKey { at, speed: Speed::from_permille((speed * 1000.0).round() as u16) });
    }
    if !keys.is_empty() && !engine::project::valid_speed_keys(&keys) {
        return Err("speed ramp keys must be in strictly increasing order of `at`, one per time".into());
    }
    let new_frames = if keys.is_empty() {
        clip.speed.frames(clip.len())
    } else {
        engine::project::ramp_frames(&keys, clip.len())
    };
    let label = format!("speed ramp {addr}");
    let why = "with that ramp".to_string();
    let shown = keys.clone();
    retime_clip(s, op, fps, lane, idx, &label, &why, new_frames, &|s: &mut PlaybackSession| {
        s.set_speed_ramp(lane, idx, &shown).map_err(|e| e.to_string())
    })
}

/// The shared body of a clip's re-timing ops (`speed`, `speed_ramp`): the length change
/// `new_frames` names, refused or rippled as the op asks, then `apply` makes the change.
/// `label` opens every reply and refusal, and `why` says what the clip would run at.
#[allow(clippy::too_many_arguments)]
fn retime_clip(
    s: &mut PlaybackSession,
    op: &Value,
    fps: f64,
    lane: Lane,
    idx: usize,
    label: &str,
    why: &str,
    new_frames: u32,
    apply: &dyn Fn(&mut PlaybackSession) -> Result<(), String>,
) -> Result<Done, String> {
    let ripple = match op.get("ripple") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("`ripple` must be true or false".into()),
    };
    let clip = s.lane_clips(lane)[idx].clone();
    let old_end = clip.end();
    let new_end = clip.start + new_frames;
    // The length change, named in the reply: "4.0s → 5.2s".
    let len_text = format!(
        " ({:.3}s → {:.3}s)",
        parse::secs(old_end - clip.start, fps),
        parse::secs(new_end - clip.start, fps)
    );
    if !ripple {
        if let Some(next) = s.lane_clips(lane).get(idx + 1).map(|c| c.start).filter(|&n| new_end > n) {
            return Err(format!(
                "{label} refused: {why} that clip would run to frame {new_end} and the next {} clip starts at {next}: add ripple:true to move the later clips along by the length change, or trim this one first",
                parse::lane_name(lane)
            ));
        }
        apply(s)?;
        let touched = s.lane_clips(lane).get(idx).map(|c| vec![(lane, c.start)]).unwrap_or_default();
        return Ok(plain(format!("{label}{len_text}"), touched));
    }
    // The change runs over [from, to): the frames the clip gives up when it is shortened, or the
    // point a slower clip opens room at. The edited clip and its take are retimed together, so only
    // other clips are checked.
    let (from, to) = if new_end > old_end { (old_end, old_end) } else { (new_end, old_end) };
    let take = take_of(s, lane, idx);
    let sc = rip_scope(s, op, &[(lane, idx)], old_end.min(new_end))?;
    let spanners = region_spanners(s, &take, &sc.lanes, from, to);
    if let Some(&(l, st, _)) = spanners.iter().find(|x| x.2) {
        return Err(format!(
            "{label} ripple refused: {} spans the change ({:.3}s–{:.3}s) and would be cut; trim or split it first, or take its lane out of lanes",
            parse::addr(l, st),
            parse::secs(old_end.min(new_end), fps),
            parse::secs(old_end.max(new_end), fps)
        ));
    }
    // A clip on a lane outside the scope spans the change: it is left in place, and the reply says so.
    let mut left: Vec<String> = spanners
        .iter()
        .filter(|x| !x.2)
        .map(|&(l, st, _)| format!("{} spans the change and was left in place", parse::addr(l, st)))
        .collect();
    // Whether any scope item starts at or after the change, so a move is said only when something moved.
    // The sped clip and its take (the clips starting where it starts, on its take's lanes) are retimed
    // in place, so they are never a later item.
    let take_lanes: Vec<Lane> = take.iter().map(|&(l, _)| l).collect();
    let own = |l: Lane, st: u32| st == clip.start && take_lanes.contains(&l);
    let later = sc.lanes.iter().any(|&l| {
        if l.kind == LaneKind::Subtitle {
            s.sub_lane(l).iter().any(|c| c.start >= from)
        } else {
            s.lane_clips(l).iter().any(|c| c.start >= from && !own(l, c.start))
        }
    });
    if new_end > old_end {
        // Slower: open the room at the old end on the scope lanes first (as insert_time does, titles
        // included), then retime, which now fits.
        let room = new_end - old_end;
        let shown = (format!("{old_end}f"), format!("{room}f"));
        let made = insert_span(s, old_end, room, &sc, fps, shown).map_err(|e| format!("{label} ripple: {e}"))?;
        apply(s)?;
        let mut touched = made.touched;
        touched.extend(s.lane_clips(lane).get(idx).map(|c| (lane, c.start)));
        let moved = if later { format!("; later items moved {room}f ({:.3}s) later", room as f64 / fps) } else { String::new() };
        let mut text = format!("{label}{len_text}{moved}{}", skipped_note(&sc.skipped));
        push_left(&mut text, &mut left);
        return Ok(Done { text, touched, lanes: made.lanes });
    }
    // Faster, or the same: retime, then close the gap it leaves in the ripple scope.
    apply(s)?;
    let gap = old_end - new_end;
    let mut text = format!("{label}{len_text}");
    let mut touched = Vec::new();
    if gap > 0 {
        check_unlocked(s, &sc.lanes)?;
        let span = [(new_end, gap)];
        let (cues_gone, cues_cut) = cues_in_span(s, &sc.lanes, new_end, old_end, fps);
        s.cut_regions(&span, &sc.lanes).map_err(|e| e.to_string())?;
        let cues_cut = trimmed_cue_lines(s, &cues_cut);
        let moved = if has_picture(&sc.lanes) { move_titles_for_cuts(s, &span, fps)? } else { Vec::new() };
        if later {
            text.push_str(&format!("; later items moved {gap}f ({:.3}s) earlier", gap as f64 / fps));
        }
        text.push_str(&titles_note(&moved));
        text.push_str(&cue_cut_text(&cues_gone, &cues_cut));
        text.push_str(&skipped_note(&sc.skipped));
        touched = ripple_touched(s, &sc.lanes, new_end);
    }
    push_left(&mut text, &mut left);
    touched.extend(s.lane_clips(lane).get(idx).map(|c| (lane, c.start)));
    Ok(plain(text, touched))
}

/// Appends the clips a ripple left in place, each as its own `; ...` clause.
fn push_left(text: &mut String, left: &mut Vec<String>) {
    for l in left.drain(..) {
        text.push_str(&format!("; {l}"));
    }
}

/// The clips a change over `[from, to)` runs across: each media clip starting before `to` and
/// ending after `from`, other than `skip` (the edited clip and its take). `(lane, start, in scope)`.
/// A clip wholly inside the span counts too, since the span would cut it away.
fn region_spanners(s: &PlaybackSession, skip: &[(Lane, usize)], scope: &[Lane], from: u32, to: u32) -> Vec<(Lane, u32, bool)> {
    let mut out = Vec::new();
    for l in s.lanes() {
        if l.kind == LaneKind::Subtitle {
            continue;
        }
        for (i, c) in s.lane_clips(l).iter().enumerate() {
            if !skip.contains(&(l, i)) && c.start < to && c.end() > from {
                out.push((l, c.start, scope.contains(&l)));
            }
        }
    }
    out
}

/// A timeline frame as seconds for a reply: `12.0s`, or `12.52s` when a tenth is not exact.
fn secs_text(frame: u32, fps: f64) -> String {
    let v = parse::secs(frame, fps);
    if (v * 10.0 - (v * 10.0).round()).abs() < 1e-6 {
        format!("{v:.1}s")
    } else {
        format!("{v:.2}s")
    }
}

/// A seconds time as the caller wrote it, for a reply: the string as given, or a number with `s`.
fn shown_secs(v: &Value) -> String {
    match v {
        Value::String(t) => t.trim().to_string(),
        _ => format!("{v}s"),
    }
}

/// Adds to an overlap refusal the seconds that rounded to whole frames. A seconds `at`, `in`, `out`
/// or `length` lands on a frame, so the clip can sit a frame off what the seconds say. Each value
/// that rounded is named with the math and the frame it became; the frame form is the exact way to
/// give it. A refusal where nothing rounded, and any other refusal, are returned as they are.
fn overlap_rounding_note(e: String, op: &Value, at: u32, len: Option<u32>, src_fps: f64, fps: f64) -> String {
    if !(e.contains("would overlap") || e.contains("would overwrite")) {
        return e;
    }
    let mut why: Vec<String> = Vec::new();
    let mut frames: Vec<String> = Vec::new();
    let mut fix: Vec<String> = Vec::new();
    // `at` in seconds: the timeline frame it lands on, read in timeline frames.
    if let Some(v) = op.get("at")
        && let Some(secs) = seconds_arg(v)
    {
        let exact = secs * fps;
        if (exact - exact.round()).abs() > 1e-6 {
            why.push(format!(
                "at {} is {exact:.3}f, which rounds to {at}f ({:.3}s)",
                shown_secs(v),
                parse::secs(at, fps)
            ));
            frames.push(format!("at \"{at}f\""));
        }
    }
    // `in` and `out` in seconds: the source frame they read, in the source's own rate.
    for key in ["in", "out"] {
        let Some(v) = op.get(key) else { continue };
        let Some(secs) = seconds_arg(v) else { continue };
        let exact = secs * src_fps;
        if (exact - exact.round()).abs() <= 1e-6 {
            continue;
        }
        // The same rounding source_time uses, so the frame named here is the one the op stored.
        let n = (exact + 0.5 + 1e-6).floor() as u32;
        why.push(format!("{key} {} is {exact:.3}f at {src_fps} fps, which rounds to {n}f", shown_secs(v)));
        frames.push(format!("{key} \"{n}f\""));
    }
    // `length` in seconds: rounded up or down to whole frames, so the clip ends a frame off.
    if let (Some(len), Some(given)) = (len, op.get("length").filter(|v| !v.is_u64()))
        && let Some(secs) = seconds_arg(given)
    {
        let gap = f64::from(len) - secs * fps;
        if gap.abs() > 1e-6 {
            let verb = if gap > 0.0 { "rounds up to" } else { "rounds down to" };
            why.push(format!(
                "length {} {verb} {len}f ({:.3}s) and ends at {}f",
                shown_secs(given),
                parse::secs(len, fps),
                at + len
            ));
            fix.push(if len > 1 {
                format!("give length in frames (e.g. \"{}f\") or start 1 frame later", len - 1)
            } else {
                "give length in frames or start 1 frame later".to_string()
            });
        }
    }
    if why.is_empty() {
        return e;
    }
    if !frames.is_empty() {
        fix.insert(0, format!("write {} in frames, or move it clear of the clip it would overlap", frames.join(" and ")));
    }
    format!("{e}. {}; {}", why.join("; "), fix.join("; "))
}

/// A speed factor for a reply, to three decimals with the zeros trimmed: `0.309`, `2`.
fn speed_text(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// What a speed factor means in words: slow motion below 1, sped up above; empty at 1.
fn speed_note(x: f64) -> &'static str {
    let rounded: f64 = speed_text(x).parse().unwrap_or(x);
    if rounded < 1.0 {
        " (slow motion)"
    } else if rounded > 1.0 {
        " (sped up)"
    } else {
        ""
    }
}

/// A number for a reply: plain, or in exponent form when it is huge or tiny, so 1e300 stays short.
fn compact(x: f64) -> String {
    if x != 0.0 && (x.abs() >= 1e6 || x.abs() < 1e-3) {
        format!("{x:e}")
    } else {
        x.to_string()
    }
}

/// Whether a fade or dissolve also reaches the clip's linked partners: true unless `linked` is false.
fn linked_arg(op: &Value) -> Result<bool, String> {
    match op.get("linked") {
        None | Some(Value::Null) => Ok(true),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err("`linked` must be true or false".into()),
    }
}

/// The clips a fade or dissolve on `lane`/`idx` reaches: its whole take, or the clip alone with `linked:false`.
fn reach(s: &PlaybackSession, op: &Value, lane: Lane, idx: usize) -> Result<Vec<(Lane, usize)>, String> {
    Ok(if linked_arg(op)? { take_of(s, lane, idx) } else { vec![(lane, idx)] })
}

/// Where each clip in `reached` starts, for the reply's touched list.
fn touched_of(s: &PlaybackSession, reached: &[(Lane, usize)]) -> Vec<(Lane, u32)> {
    reached.iter().filter_map(|&(l, i)| s.lane_clips(l).get(i).map(|c| (l, c.start))).collect()
}

/// Why the clip at `idx` of `lane` cannot join the clip after it, in the words a fix needs.
/// None when the next clip starts exactly where this one ends.
fn join_problem(s: &PlaybackSession, lane: Lane, idx: usize) -> Option<String> {
    let clips = s.lane_clips(lane);
    let addr = addr_of(s, lane, idx);
    let name = parse::lane_name(lane);
    let Some(cur) = clips.get(idx) else {
        return Some(format!("there is no clip at {addr}"));
    };
    let Some(next) = clips.get(idx + 1) else {
        return Some(format!("{addr} is the last clip on {name}, so there is no clip after it"));
    };
    let gap = next.start.saturating_sub(cur.end());
    if gap == 0 {
        return None;
    }
    let between = format!("a gap of {gap}f lies between {addr} and {} at {}", addr_of(s, lane, idx + 1), parse::tc(next.start, parse::session_fps(s)));
    // close_gaps is named only when it would close the gap; otherwise the take's reason is.
    Some(match take_gap_refusal(s, lane, cur.end(), gap) {
        None => format!("{between}: close it with close_gaps {{lane:\"{name}\"}}, or move the clip so they meet"),
        Some(why) => format!("{between}: close_gaps cannot close it, {why}; or move the clip so they meet"),
    })
}

fn op_fade(s: &mut PlaybackSession, op: &Value, fps: f64, _end: u32) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let fin = length(op, "in", fps)?;
    let fout = length(op, "out", fps)?;
    if fin.is_none() && fout.is_none() {
        return Err(format!("fade {addr} needs `in` and/or `out`: frames (12) or a time (\"0.5s\")"));
    }
    let reached = reach(s, op, lane, idx)?;
    let lanes: Vec<Lane> = reached.iter().map(|&(l, _)| l).collect();
    check_unlocked(s, &lanes)?;
    // In and out together longer than the clip would overlap: both are clamped to half of it, reported.
    let (mut fin, mut fout) = (fin, fout);
    let mut clamp_note = String::new();
    let len = s.lane_clips(lane).get(idx).map_or(0, |c| c.frames());
    if let (Some(a), Some(b)) = (fin, fout)
        && len > 0 && a.saturating_add(b) > len
    {
        let half = len / 2;
        clamp_note = format!(
            "; in {} + out {} are longer than the {} clip: both clamped to {}",
            secs_text(a, fps),
            secs_text(b, fps),
            secs_text(len, fps),
            secs_text(half, fps)
        );
        fin = Some(half);
        fout = Some(half);
    }
    let mut named = Vec::new();
    // What the engine kept, read back per edge: it clamps a ramp to its clip's length.
    let mut applied: Vec<(Option<u32>, Option<u32>)> = Vec::new();
    for &(l, i) in &reached {
        let a = addr_of(s, l, i);
        if let Some(f) = fin
            && !s.set_fade_in(l, i, f)
        {
            return Err(format!("fade in on {a} refused: no such clip"));
        }
        if let Some(f) = fout
            && !s.set_fade_out(l, i, f)
        {
            return Err(format!("fade out on {a} refused: no such clip"));
        }
        applied.push((fin.map(|_| s.fade_in_of(l, i)), fout.map(|_| s.fade_out_of(l, i))));
        named.push(a);
    }
    // The header names the frames the engine kept (the first clip speaks for the take), so a ramp
    // clamped to its clip's length reads as the length it got, with the asked time in the note.
    let (kept_in, kept_out) = applied.first().copied().unwrap_or((None, None));
    let mut parts = Vec::new();
    if let Some(f) = kept_in {
        parts.push(format!("in {f}f"));
    }
    if let Some(f) = kept_out {
        parts.push(format!("out {f}f"));
    }
    let mut text = format!("fade {} {}", named.join(" + "), parts.join(" "));
    // The first clip speaks for the note: a linked partner is the same length.
    if let Some(&(got_in, got_out)) = applied.first() {
        let clamp = |asked: Option<u32>, got: Option<u32>, edge: &str| match (asked, got) {
            (Some(asked), Some(got)) if got < asked => Some(format!(
                "{edge} clamped from {} to the clip's {}",
                secs_text(asked, fps),
                secs_text(got, fps)
            )),
            _ => None,
        };
        let notes: Vec<String> = [clamp(fin, got_in, "in"), clamp(fout, got_out, "out")]
            .into_iter()
            .flatten()
            .collect();
        if !notes.is_empty() {
            text.push_str(&format!(" ({})", notes.join("; ")));
        }
    }
    text.push_str(&clamp_note);
    let touched = touched_of(s, &reached);
    Ok(plain(text, touched))
}

/// The `kind`/`direction` of a transition op; a dissolve when `kind` is not given.
fn transition_kind_arg(op: &Value) -> Result<TransitionKind, String> {
    let text = |key: &str| match op.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.to_lowercase().replace('_', "-"))),
        Some(v) => Err(format!("`{key}` must be text, got {}", parse::kind_of(v))),
    };
    let shape = text("kind")?.unwrap_or_else(|| "dissolve".into());
    let dir = match text("direction")? {
        None => None,
        Some(d) => Some(Dir::parse(&d).ok_or_else(|| format!("`direction` {d:?} is not one of left, right, up, down"))?),
    };
    // A full name such as "wipe-left" is also accepted as the kind.
    if dir.is_none()
        && let Some(k) = TransitionKind::parse(&shape)
    {
        return Ok(k);
    }
    let kind = TransitionKind::from_parts(&shape, dir).ok_or_else(|| {
        format!("`kind` {shape:?} is not one of dissolve, dip-black, dip-white, wipe, slide, push, zoom")
    })?;
    if dir.is_some() && kind.dir().is_none() {
        return Err(format!("`direction` does not apply to {}: only wipe, slide and push move", kind.shape_name()));
    }
    Ok(kind)
}

fn op_dissolve(s: &mut PlaybackSession, op: &Value, fps: f64, keep_length: bool) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let kind = transition_kind_arg(op)?;
    let given = match (length(op, "frames", fps)?, length(op, "length", fps)?) {
        (Some(_), Some(_)) => return Err("give `frames` or `length`, not both".into()),
        (a, b) => a.or(b),
    };
    // `transition` without a length restyles the transition the clip has.
    let kept = if keep_length { Some(s.transition_out_of(lane, idx)).filter(|&n| n > 0) } else { None };
    let frames = given.or(kept).ok_or("missing `frames`: give the transition length in frames")?;
    if lane.kind != LaneKind::Video {
        return Err(format!(
            "dissolve on {addr} refused: {} is an audio lane; a dissolve is picture, so use crossfade for sound",
            parse::lane_name(lane)
        ));
    }
    // The lock comes before the join: a locked lane is refused as locked, whatever clip it is on.
    let reached = reach(s, op, lane, idx)?;
    let lanes: Vec<Lane> = reached.iter().map(|&(l, _)| l).collect();
    check_unlocked(s, &lanes)?;
    if let Some(why) = join_problem(s, lane, idx) {
        return Err(format!("dissolve on {addr} refused: {why}"));
    }
    if !s.set_transition(lane, idx, kind, frames) {
        return Err(format!("dissolve on {addr} refused: the engine would not set it"));
    }
    // The overlap is the frames the engine kept, which is less than asked when the shorter clip is.
    let applied = s.transition_out_of(lane, idx);
    let next = s.lane_clips(lane).get(idx + 1).map(|c| c.start).unwrap_or(0);
    let from = next.saturating_sub(applied);
    let mut text = format!(
        "{} {addr}→{} over {applied}f ({}–{}): {} blends in over the tail of {addr}",
        kind.name(),
        addr_of(s, lane, idx + 1),
        secs_text(from, fps),
        secs_text(next, fps),
        addr_of(s, lane, idx + 1)
    );
    if applied < frames {
        text.push_str(&format!(" (clamped from {frames}f by the shorter of the two clips)"));
    }
    text.push_str("; timeline length unchanged");
    // A linked sound partner crossfades into its own next clip, when that clip meets it.
    for &(l, i) in reached.iter().filter(|&&(l, i)| (l, i) != (lane, idx) && l.kind == LaneKind::Audio) {
        let a = addr_of(s, l, i);
        if s.crossfade(l, i, frames) {
            text.push_str(&format!("; sound crossfaded {a} {frames}f into {}", addr_of(s, l, i + 1)));
        } else if let Some(t) = fade_to_silence(s, l, i, frames) {
            text.push_str(&format!("; {t}"));
        } else {
            text.push_str(&format!("; the sound on {a} was not crossfaded: {}", sound_skip_reason(s, l, i)));
        }
    }
    let touched = touched_of(s, &reached);
    Ok(plain(text, touched))
}

/// Why a linked sound clip could not crossfade into its next clip, and what to do: one clause
/// for `dissolve`'s reply.
fn sound_skip_reason(s: &PlaybackSession, lane: Lane, idx: usize) -> String {
    let addr = addr_of(s, lane, idx);
    let name = parse::lane_name(lane);
    let clips = s.lane_clips(lane);
    match (clips.get(idx), clips.get(idx + 1)) {
        (Some(_), None) => format!(
            "{addr} is the last clip on {name}, so there is no clip after it to crossfade into; place the next sound clip after it, then crossfade {addr}"
        ),
        (Some(cur), Some(next)) if next.start > cur.end() => match take_gap_refusal(s, lane, cur.end(), next.start - cur.end()) {
            None => format!(
                "a gap of {}f lies before {} on {name}; close it with close_gaps {{lane:\"{name}\"}} and crossfade {addr} again",
                next.start - cur.end(),
                addr_of(s, lane, idx + 1)
            ),
            Some(why) => format!(
                "a gap of {}f lies before {} on {name}, and close_gaps cannot close it: {why}; crossfade {addr} once the gap is closed",
                next.start - cur.end(),
                addr_of(s, lane, idx + 1)
            ),
        },
        _ => format!("the engine would not crossfade {addr}; crossfade it on its own to see why"),
    }
}

/// `visualizer {clip, on?, style?, fast?}`: the clip's waveform look, as the flags byte the
/// engine stores: bit 0 on, bits 1-2 the style (fill, ribbon, ring, wave), bit 3 fast.
fn op_visualizer(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    const STYLES: [&str; 4] = ["fill", "ribbon", "ring", "wave"];
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let flag = |key: &str| -> Result<Option<bool>, String> {
        match op.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(format!("`{key}` must be true or false")),
        }
    };
    let on = flag("on")?;
    let fast = flag("fast")?;
    let style = match op.get("style") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(
            STYLES
                .iter()
                .position(|&n| n == t)
                .ok_or_else(|| format!("`style` {t} is not one of {}", STYLES.join(", ")))?,
        ),
        Some(_) => return Err(format!("`style` must be one of {}", STYLES.join(", "))),
    };
    if on.is_none() && fast.is_none() && style.is_none() {
        return Err(format!("visualizer {addr} needs `on`, `style` or `fast`"));
    }
    // On a sound clip, on:true places a waveform picture for it on a video lane. `style` and `fast`
    // set that picture's look (fill is the default), and the picture is grouped with the sound
    // clip, so moves, trims and deletes carry it.
    if lane.kind == LaneKind::Audio {
        // on:false turns off the waveform picture of the take, as it does when given on the picture's own clip.
        if on == Some(false) {
            if style.is_some() || fast.is_some() {
                return Err(format!("visualizer {addr}: on:false turns it off, so style and fast do not apply"));
            }
            let picture = take_of(s, lane, idx).into_iter().find(|&(l, i)| l.kind == LaneKind::Video && s.lane_clips(l)[i].visualizer != 0);
            let Some((pic_lane, pic_idx)) = picture else {
                return Ok(plain(format!("visualizer {addr} off already: its take has no waveform picture"), Vec::new()));
            };
            check_unlocked(s, &[pic_lane])?;
            if !s.set_visualizer(pic_lane, pic_idx, 0) {
                return Err(format!("visualizer {addr} refused: its waveform picture could not be turned off"));
            }
            let start = s.lane_clips(pic_lane)[pic_idx].start;
            let picture = parse::addr(pic_lane, start);
            return Ok(plain(format!("visualizer {addr} off: its waveform picture on {picture} is off"), vec![(pic_lane, start)]));
        }
        if on != Some(true) {
            return Err(format!(
                "visualizer {addr} refused: {} is an audio lane; the picture is drawn only from video clips, so set the visualizer on the video clip split from the same take, or give on:true for a waveform picture",
                parse::lane_name(lane)
            ));
        }
        check_unlocked(s, &[lane])?;
        // A take whose waveform picture is already there is restyled, not given a second one.
        let existing = take_of(s, lane, idx)
            .into_iter()
            .find(|&(l, i)| l.kind == LaneKind::Video && s.lane_clips(l)[i].visualizer != 0);
        let placed = existing.is_none();
        let (pic_lane, start) = match existing {
            Some((pl, pi)) => (pl, s.lane_clips(pl)[pi].start),
            None => super::edit_fx::place_waveform_for_audio(s, lane, idx)
                .map_err(|e| format!("visualizer {addr}: {e}"))?,
        };
        let pic_idx = s
            .lane_clips(pic_lane)
            .iter()
            .position(|c| c.start == start)
            .ok_or_else(|| format!("visualizer {addr}: the waveform picture was placed but not found"))?;
        let mut look = String::new();
        if style.is_some() || fast.is_some() {
            let cur = s.lane_clips(pic_lane)[pic_idx].visualizer;
            let mut f = (if cur == 0 { 1 } else { cur }) | 1;
            if let Some(st) = style {
                f = engine::decode::with_viz_style(f, st as u8);
            }
            match fast {
                Some(true) => f |= engine::decode::VIZ_FAST,
                Some(false) => f &= !engine::decode::VIZ_FAST,
                None => {}
            }
            if !s.set_visualizer(pic_lane, pic_idx, f) {
                return Err(format!("visualizer {addr}: the waveform picture could not take its look"));
            }
            let st = STYLES[usize::from(engine::decode::viz_style(f)) & 3];
            let fast_note = if f & engine::decode::VIZ_FAST != 0 { ", fast" } else { "" };
            look = format!(" (style {st}{fast_note})");
        }
        let picture = parse::addr(pic_lane, start);
        let text = if placed {
            s.group(lane, idx, pic_lane, pic_idx)
                .map_err(|e| format!("visualizer {addr}: the waveform picture could not be grouped with it: {e}"))?;
            // Read after the group: grouping re-keys the take's clips to one new link, so that link is the take's.
            let take_link = s.lane_clips(lane)[idx].link;
            let cover = covered_picture_note(s, pic_lane, start, s.lane_clips(pic_lane)[pic_idx].end(), take_link);
            format!("visualizer {addr}: placed a waveform picture{look} for {addr} on {picture}, grouped with it{cover}")
        } else if look.is_empty() {
            format!("visualizer {addr}: already on (its waveform picture is on {picture})")
        } else {
            format!("visualizer {addr}: restyled its waveform picture on {picture}{look}")
        };
        return Ok(plain(text, vec![(pic_lane, start), (lane, s.lane_clips(lane)[idx].start)]));
    }
    let cur = s.lane_clips(lane)[idx].visualizer;
    let flags = if on == Some(false) {
        if style.is_some() || fast.is_some() {
            return Err(format!("visualizer {addr}: on:false turns it off, so style and fast do not apply"));
        }
        0
    } else {
        if cur == 0 && on != Some(true) && (style.is_some() || fast.is_some()) {
            return Err(format!("visualizer {addr} is off: give on:true with style or fast"));
        }
        let base = if cur == 0 { 1 } else { cur };
        let mut f = base | 1;
        if let Some(st) = style {
            f = engine::decode::with_viz_style(f, st as u8);
        }
        match fast {
            Some(true) => f |= engine::decode::VIZ_FAST,
            Some(false) => f &= !engine::decode::VIZ_FAST,
            None => {}
        }
        f
    };
    check_unlocked(s, &[lane])?;
    if !s.set_visualizer(lane, idx, flags) {
        return Err(format!("visualizer {addr} refused: no such clip"));
    }
    let text = if flags == 0 {
        format!("visualizer {addr} off")
    } else {
        let st = STYLES[usize::from(engine::decode::viz_style(flags)) & 3];
        let fast_note = if flags & engine::decode::VIZ_FAST != 0 { ", fast" } else { "" };
        format!("visualizer {addr} on (style {st}{fast_note})")
    };
    Ok(plain(text, vec![(lane, s.lane_clips(lane)[idx].start)]))
}

/// The note for a picture placed on `pic` over `start..end` when picture lanes below it hold clips
/// there: the picture is opaque (its background is drawn, not keyed out), so it hides them. Clips of
/// the visualized sound's own take (`take_link`) are left out: that take's picture is the one the
/// visualizer is for, not a cover. Empty when nothing below is covered.
fn covered_picture_note(s: &PlaybackSession, pic: Lane, start: u32, end: u32, take_link: Option<u32>) -> String {
    let fps = parse::session_fps(s);
    let mut parts: Vec<String> = Vec::new();
    for l in s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video && l.ord < pic.ord) {
        for c in s.lane_clips(l) {
            let own_take = take_link.is_some() && c.link == take_link;
            let (a, b) = (c.start.max(start), c.end().min(end));
            if a < b && !own_take {
                parts.push(format!("{} ({}) during {}–{}", parse::lane_name(l), source_name(s, c.source), secs_text(a, fps), secs_text(b, fps)));
            }
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "; the visualizer on {} draws over {}; lane_move it below them: its picture is opaque, and no background option makes it transparent",
        parse::lane_name(pic),
        parts.join(", ")
    )
}

fn op_crossfade(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let frames = length(op, "frames", fps)?.ok_or("missing `frames`: give the crossfade length in frames")?;
    if lane.kind != LaneKind::Audio {
        return Err(format!(
            "crossfade on {addr} refused: {} is a picture lane; a crossfade joins sound, so use dissolve for picture",
            parse::lane_name(lane)
        ));
    }
    if let Some(why) = join_problem(s, lane, idx) {
        // No sound clip meets this one: fade it to and from silence instead of refusing.
        check_unlocked(s, &[lane])?;
        return match fade_to_silence(s, lane, idx, frames) {
            Some(t) => {
                let touched = s.lane_clips(lane).get(idx).map(|c| vec![(lane, c.start)]).unwrap_or_default();
                Ok(plain(format!("crossfade {addr} {frames}f: {t}"), touched))
            }
            None => Err(format!("crossfade on {addr} refused: {why}")),
        };
    }
    // A pair already crossfaded to this length (a dissolve does it for the sound of its take) is no change:
    // the reason is named, so the batch's no-change reply can say why.
    let (a, b) = (&s.lane_clips(lane)[idx], &s.lane_clips(lane)[idx + 1]);
    if s.fade_out_of(lane, idx) == frames.min(a.frames()) && s.fade_in_of(lane, idx + 1) == frames.min(b.frames()) {
        let from_dissolve = take_of(s, lane, idx).iter().any(|&(l, i)| {
            l.kind == LaneKind::Video && s.transition_out_of(l, i) > 0 && s.lane_clips(l).get(i).is_some_and(|c| c.end() == a.end())
        });
        let note = if from_dissolve { " (from the dissolve)" } else { "" };
        return Ok(plain(
            format!("{addr} and {} already crossfade over {frames}f{note}", addr_of(s, lane, idx + 1)),
            Vec::new(),
        ));
    }
    check_unlocked(s, &[lane])?;
    if !s.crossfade(lane, idx, frames) {
        return Err(format!("crossfade on {addr} refused: the engine would not set it"));
    }
    let touched = s.lane_clips(lane).get(idx).map(|c| vec![(lane, c.start)]).unwrap_or_default();
    // The reply names the clip the crossfade joins to, so the arrow is not a self-reference.
    Ok(plain(format!("crossfade {addr} {frames}f → {} (the next clip)", addr_of(s, lane, idx + 1)), touched))
}

// ---- transform warning ----------------------------------------------------

/// The share of a transformed clip's box that lands on the canvas, 0 to 1. The
/// box is `scale` canvas widths square, centred at (0.5 + x, 0.5 + y) -- taking
/// the fit size as the canvas's own shape, which is the common case.
fn on_canvas_share(t: &engine::transform::TransformParams) -> f32 {
    let (cx, cy) = (0.5 + t.pos_x, 0.5 + t.pos_y);
    let half = t.scale / 2.0;
    let across = ((cx + half).min(1.0) - (cx - half).max(0.0)).max(0.0);
    let down = ((cy + half).min(1.0) - (cy - half).max(0.0)).max(0.0);
    (across * down / (t.scale * t.scale)).clamp(0.0, 1.0)
}

/// The transform op's reply, with a warning when the clip it placed is entirely
/// or mostly off the canvas: that is seldom what was meant, and little or none
/// of the picture shows.
fn off_canvas_note(s: &PlaybackSession, mut applied: Applied) -> Applied {
    for &(lane, start) in &applied.touched {
        let Some(idx) = s.lane_clips(lane).iter().position(|c| c.start == start) else {
            continue;
        };
        let Some(t) = s.transform_of(lane, idx) else {
            continue;
        };
        let share = on_canvas_share(t);
        if share <= 0.0 {
            applied
                .text
                .push_str(" — WARNING: this clip is entirely off the canvas, so none of it shows");
        } else if share < 0.5 {
            applied.text.push_str(&format!(
                " — WARNING: only {:.0}% of this clip is on the canvas",
                share * 100.0
            ));
        }
    }
    applied
}

// ---- dispatch -------------------------------------------------------------

/// The note for a `rotate` that is not a quarter turn: the picture turns only in 90-degree steps,
/// so the angle is drawn at the nearest one (the engine's rule). Empty for a quarter turn.
fn rotate_note(op: &Value) -> String {
    let Some(deg) = op.get("rotate").and_then(Value::as_f64) else { return String::new() };
    if deg.rem_euclid(90.0) == 0.0 {
        return String::new();
    }
    let steps = ((deg.rem_euclid(360.0) / 90.0).round() as i64).rem_euclid(4);
    format!("; rotate {} is drawn at {}° (only quarter turns)", compact(deg), steps * 90)
}

/// One op, with the rounding note for each timeline time it took appended to its line.
/// An op aimed at the wrong kind of lane: picture ops on sound clips, sound ops on picture
/// clips, and lane gain on a video lane. None when the kind fits, or the address does not
/// resolve (the op's own check reports that).
fn kind_refusal(s: &PlaybackSession, name: &str, op: &Value) -> Option<String> {
    match name {
        "color" | "eq" | "visualizer" => {
            let addr = op.get("clip")?.as_str()?.trim();
            let (lane, idx) = parse::clip(s, addr).ok()?;
            let here = parse::lane_name(lane);
            match (name, lane.kind) {
                // A visualizer is a sound clip's picture: turning one on a video clip names the sound clip of its take.
                // on:false only clears a flag, so it is let through.
                ("visualizer", LaneKind::Video) if op.get("on") != Some(&Value::Bool(false)) => {
                    let sound = take_of(s, lane, idx).into_iter().find(|&(l, _)| l.kind == LaneKind::Audio);
                    Some(match sound {
                        Some((l, i)) => {
                            // The sound clip's waveform picture must match its length, so a sound clip off 1x refuses on:true.
                            let sound = addr_of(s, l, i);
                            let bounds = if s.speed_of(l, i) == engine::project::Speed::NORMAL {
                                format!("on:true there places its waveform picture over the span of {sound} on a free video lane")
                            } else {
                                format!("its waveform picture must match its length, and {sound} plays at {}x: set its speed to 1x first", compact(s.speed_of(l, i).as_f64()))
                            };
                            format!("visualizer {addr} refused: {here} is a video lane; the visualizer is set on the sound clip of its take, {sound} ({sound} is the sound of {addr}); {bounds}")
                        }
                        None => format!(
                            "visualizer {addr} refused: {here} is a video lane with no linked sound clip; the visualizer is set on a sound clip (on:true there places its waveform picture)"
                        ),
                    })
                }
                ("color", LaneKind::Audio) => Some(format!(
                    "color {addr} refused: {here} is an audio lane; colour is picture, so set it on the video clip split from the same take"
                )),
                ("eq", LaneKind::Video) => Some(format!(
                    "eq {addr} refused: {here} is a video lane; the equalizer works on sound, so set it on the audio clip split from the same take"
                )),
                _ => None,
            }
        }
        "lane_pan" => {
            let lane = lane_arg(op, "lane").ok()??;
            (lane.kind != LaneKind::Audio).then(|| {
                format!("lane_pan {} refused: pan is set on an audio lane (A1..)", parse::lane_name(lane))
            })
        }
        "lane_gain" => {
            let lane = lane_arg(op, "lane").ok()??;
            (lane.kind == LaneKind::Video).then(|| {
                format!(
                    "lane_gain {} refused: a video lane has no sound of its own; lane gain is set on an audio lane (A1..)",
                    parse::lane_name(lane)
                )
            })
        }
        _ => None,
    }
}

const PICTURE_PARTS: &[&str] = &["color", "transform", "fit", "effects"];
const SOUND_PARTS: &[&str] = &["eq", "gain"];

/// The parts `copy_look` copies: `what`, default ["color"]. Each is "color", "transform" or "fit"
/// (picture parts) or "eq" (the sound's equalizer).
fn copy_what(op: &Value) -> Result<Vec<&'static str>, String> {
    let list = match op.get("what") {
        None | Some(Value::Null) => return Ok(vec!["color"]),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("copy_look: `what` must be a list of color, transform, fit, effects, eq, gain or speed".into()),
    };
    let mut out: Vec<&'static str> = Vec::new();
    for item in list {
        let name = match item.as_str() {
            Some("color") => "color",
            Some("transform") => "transform",
            Some("fit") => "fit",
            Some("eq") => "eq",
            Some("effects") => "effects",
            Some("speed") => "speed",
            Some("gain") => "gain",
            _ => return Err(format!("copy_look: `what` takes color, transform, fit, effects, eq, gain or speed; {item} is not one of them")),
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        return Err("copy_look: `what` is empty; name what to copy, e.g. [\"color\"]".into());
    }
    Ok(out)
}

/// `copy_look {from, to:[addrs] | to_lane, what?}`: copies the look of the clip `from` onto the
/// clips `to` (or every clip on `to_lane`), as one batch step. Colour, transform and fit come from
/// a picture clip and land on picture clips; eq comes from a sound clip and lands on sound clips.
/// A target the part does not fit is skipped and named in the reply. Each target gets exactly the
/// source's setting, including none: a source with no colour clears its targets' colour.
fn op_copy_look(s: &mut PlaybackSession, op: &Value) -> Result<Done, String> {
    if op.get("from").is_none() {
        return Err("copy_look needs `from`: the clip whose look to copy, e.g. \"V1@0\"".into());
    }
    let (from_lane, from_idx) = clip_arg(s, &json!({ "clip": op.get("from").cloned().unwrap_or(Value::Null) }))
        .map_err(|e| format!("copy_look from: {e}"))?;
    let from_addr = addr_of(s, from_lane, from_idx);
    let what = copy_what(op)?;
    let picture_part = what.iter().any(|w| PICTURE_PARTS.contains(w));
    let sound_part = what.iter().any(|w| SOUND_PARTS.contains(w));
    if picture_part && from_lane.kind != LaneKind::Video {
        return Err(format!(
            "copy_look from {from_addr}: colour, transform, fit and effects come from a picture clip; {from_addr} is a sound clip (use what:[\"eq\"] or [\"gain\"] for its sound)"
        ));
    }
    if sound_part && from_lane.kind != LaneKind::Audio {
        return Err(format!(
            "copy_look from {from_addr}: eq comes from a sound clip, and so does gain; {from_addr} is a picture clip (its sound is on the audio lane of its take)"
        ));
    }
    // The targets: each `to` address, or every clip on `to_lane`; the source itself is never one.
    let mut targets: Vec<(Lane, usize)> = Vec::new();
    match (op.get("to"), lane_arg(op, "to_lane")?) {
        (Some(_), Some(_)) => return Err("copy_look: give `to` or `to_lane`, not both".into()),
        (Some(Value::Array(list)), None) => {
            if list.is_empty() {
                return Err("copy_look: `to` is empty; give clip addresses such as [\"V1@45\"]".into());
            }
            for v in list {
                targets.push(clip_arg(s, &json!({ "clip": v })).map_err(|e| format!("copy_look to: {e}"))?);
            }
        }
        (Some(_), None) => return Err("copy_look: `to` must be a list of clip addresses, e.g. [\"V1@45\"]".into()),
        (None, Some(l)) => {
            if !s.lanes().contains(&l) {
                return Err(missing_lane(s, l));
            }
            targets.extend((0..s.lane_clips(l).len()).map(|i| (l, i)));
        }
        (None, None) => return Err("copy_look needs `to` (clip addresses) or `to_lane`".into()),
    }
    targets.retain(|&t| t != (from_lane, from_idx));
    let mut seen: Vec<(Lane, usize)> = Vec::new();
    targets.retain(|t| {
        let new = !seen.contains(t);
        seen.push(*t);
        new
    });
    if targets.is_empty() {
        return Err(format!("copy_look {from_addr}: no clips to copy onto (the source is the only one named)"));
    }
    let mut unique: Vec<Lane> = Vec::new();
    for &(l, _) in &targets {
        if !unique.contains(&l) {
            unique.push(l);
        }
    }
    check_unlocked(s, &unique)?;

    // The source's settings, read before anything changes.
    let color = s.color_of(from_lane, from_idx).copied();
    let transform = s.transform_of(from_lane, from_idx).copied();
    let fit = s.fit_of(from_lane, from_idx);
    let eq = s.eq_of(from_lane, from_idx).cloned();
    let effects = s.effects_of(from_lane, from_idx).to_vec();
    let gain = s.clip_gain_of(from_lane, from_idx).unwrap_or(0.);
    let sp = s.lane_clips(from_lane).get(from_idx).map(|c| (c.speed, c.speed_keys.clone(), c.reversed)).ok_or("copy_look: the source clip is gone")?;
    let speed_differs = |s: &PlaybackSession, l: Lane, i: usize| {
        s.lane_clips(l).get(i).is_some_and(|c| (c.speed, &c.speed_keys, c.reversed) != (sp.0, &sp.1, sp.2))
    };

    let mut touched: Vec<(Lane, u32)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // Targets that already hold the source's look for every part asked: nothing is written to them.
    let mut already: Vec<String> = Vec::new();
    let parts = what.join(", ");
    for &(l, i) in &targets {
        let addr = addr_of(s, l, i);
        let start = s.lane_clips(l).get(i).map_or(0, |c| c.start);
        let differs = match l.kind {
            // A part the lane kind does not take is skipped below, so it counts as a difference here.
            LaneKind::Video => {
                !(picture_part || what.contains(&"speed"))
                    || sound_part
                    || (what.contains(&"color") && s.color_of(l, i).copied() != color)
                    || (what.contains(&"transform") && s.transform_of(l, i).copied() != transform)
                    || (what.contains(&"fit") && s.fit_of(l, i) != fit)
                    || (what.contains(&"effects") && s.effects_of(l, i) != effects.as_slice())
                    || (what.contains(&"speed") && speed_differs(s, l, i))
            }
            LaneKind::Audio => {
                picture_part
                    || !(sound_part || what.contains(&"speed"))
                    || (what.contains(&"eq") && s.eq_of(l, i).cloned() != eq)
                    || (what.contains(&"gain") && s.clip_gain_of(l, i).unwrap_or(0.) != gain)
                    || (what.contains(&"speed") && speed_differs(s, l, i))
            }
            LaneKind::Subtitle => true,
        };
        if !differs {
            already.push(addr);
            continue;
        }
        // `took`: the target received at least one part. A target that took nothing is skipped, not applied.
        let mut took = false;
        match l.kind {
            LaneKind::Video => {
                if what.contains(&"color") {
                    if !s.set_color(l, i, color) {
                        return Err(format!("copy_look: the colour would not go on {addr}"));
                    }
                    took = true;
                }
                if what.contains(&"transform") {
                    if !s.set_transform(l, i, transform) {
                        return Err(format!("copy_look: the transform would not go on {addr}"));
                    }
                    took = true;
                }
                if what.contains(&"fit") {
                    if !s.set_fit(l, i, fit) {
                        return Err(format!("copy_look: the fit would not go on {addr}"));
                    }
                    took = true;
                }
                if what.contains(&"effects") {
                    if !s.set_effects(l, i, effects.clone()) {
                        return Err(format!("copy_look: the effects would not go on {addr}"));
                    }
                    took = true;
                }
                if sound_part {
                    skipped.push(format!(
                        "{} on {addr} (a picture clip has no eq or gain; its sound does)",
                        what.iter().filter(|w| SOUND_PARTS.contains(w)).copied().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            LaneKind::Audio => {
                if what.contains(&"eq") {
                    if !s.set_eq(l, i, eq.clone()) {
                        return Err(format!("copy_look: the eq would not go on {addr}"));
                    }
                    took = true;
                }
                if what.contains(&"gain") {
                    if !s.set_clip_gain(l, i, gain) {
                        return Err(format!("copy_look: the gain would not go on {addr}"));
                    }
                    took = true;
                }
                if picture_part {
                    skipped.push(format!("{} on {addr} (a sound clip has no picture look)", what.iter().filter(|w| PICTURE_PARTS.contains(w)).copied().collect::<Vec<_>>().join(", ")));
                }
            }
            LaneKind::Subtitle => {
                skipped.push(format!("{addr} (a caption lane takes no look)"));
                continue;
            }
        }
        if what.contains(&"speed") {
            if speed_differs(s, l, i) {
                let mut ok = s.reversed_of(l, i) == sp.2 || s.set_reverse(l, i, sp.2).is_ok();
                ok &= s.speed_of(l, i) == sp.0 || s.set_speed(l, i, sp.0).is_ok();
                if !sp.1.is_empty() {
                    ok &= s.set_speed_ramp(l, i, &sp.1).is_ok();
                }
                if ok {
                    took = true;
                } else {
                    skipped.push(format!("speed on {addr} (no room to change its length there)"));
                }
            } else {
                took = true;
            }
        }
        if !took {
            skipped.push(format!("{addr} (no part of {parts} applies to it)"));
            continue;
        }
        names.push(addr);
        touched.push((l, start));
    }
    if names.is_empty() && skipped.is_empty() && !already.is_empty() {
        // Nothing changed: the reply says so, and the batch reads as a no-change one.
        let (verb, list) = if already.len() == 1 { ("has", already[0].clone()) } else { ("have", already.join(", ")) };
        return Ok(plain(format!("{list} already {verb} {from_addr}'s {parts}: nothing to copy"), Vec::new()));
    }
    let mut text = format!("copy_look {from_addr} → {}: {parts} copied", names.join(", "));
    if names.is_empty() {
        text = format!("copy_look {from_addr}: no target took {parts}");
    }
    if what.contains(&"color") && color.is_none() && !names.is_empty() {
        text.push_str(&format!("; {from_addr} has no colour, so the targets' colour is cleared"));
    }
    if !already.is_empty() {
        text.push_str(&format!("; already had {parts}: {}", already.join(", ")));
    }
    if !skipped.is_empty() {
        text.push_str(&format!("; skipped: {}", skipped.join("; ")));
    }
    Ok(plain(text, touched))
}

/// `replace {clip, source, in?}`: the clip (and the linked other half of its take, when it covers the
/// same frames and the source has that half) is overwritten by `source` from `in` (default its start),
/// keeping its place and length. Nothing else moves. One undo step.
fn op_replace(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let mut op = op.clone();
    if let Some(o) = op.as_object_mut()
        && let Some(v) = o.remove("src")
    {
        if o.contains_key("source") {
            return Err("replace: give source or src, not both".into());
        }
        o.insert("source".into(), v);
    }
    let op = &op;
    let (lane, idx) = clip_arg(s, op)?;
    let addr = addr_of(s, lane, idx);
    let n = source_arg(s, op)?;
    check_unlocked(s, &take_of(s, lane, idx).iter().map(|&(l, _)| l).collect::<Vec<_>>())?;
    let path: PathBuf = s.sources()[n].path.clone();
    let still = engine::is_image(&path);
    let picture = still || engine::probe_media(&path).video.is_some();
    let sound = !still
        && !AudioSession::probe_streams(&path).map_err(|e| format!("src:{n}: cannot read its sound: {e}"))?.is_empty();
    let total = source_frames(s, n, Some(lane), fps);
    let (in_f, note) = if still {
        if op.get("in").is_some() {
            return Err(format!("replace: src:{n} is a still image, which takes no `in`"));
        }
        (0, String::new())
    } else {
        let b = Base::of(s, &path);
        match opt_source_time(op, "in", b, total)? {
            Some((f, note)) => (f, note),
            None => (0, String::new()),
        }
    };
    let (at, len, items) = engine::editkeys::replace_plan(s, (lane, idx), n, in_f, total, (picture, sound, still))
        .map_err(|e| format!("replace {addr} with src:{n}: {e}"))?;
    s.edit_from_source_at(SourceEdit::Overwrite, parse::secs(at, fps), &items)
        .map_err(|e| format!("replace {addr} with src:{n}: {e}"))?;
    let touched: Vec<(Lane, u32)> = items.iter().map(|(l, _)| (*l, at)).collect();
    let names: Vec<String> = touched.iter().map(|&(l, st)| parse::addr(l, st)).collect();
    let shown = if still { "(still)".to_string() } else { Base::of(s, &path).span(in_f, in_f + len) };
    let text = format!(
        "replace {addr} with src:{n} {} {shown}: {} now play it for {}{note}",
        source_name(s, n),
        names.join(" + "),
        secs_text(len, fps)
    );
    Ok(plain(text, touched))
}

/// `nudge {clips:[addrs], by}`: the clips (and their groups) move `by` along their lane as one move,
/// refused when a neighbour blocks it outright, and stopped short at the room there is. One undo step.
fn op_nudge(s: &mut PlaybackSession, op: &Value, fps: f64) -> Result<Done, String> {
    let list = match op.get("clips") {
        Some(Value::Array(l)) if !l.is_empty() => l,
        Some(Value::String(_)) => return Err("nudge: `clips` must be a list of addresses, e.g. [\"V1@45\"]".into()),
        _ => return Err("missing `clips`: a list of clip addresses to move, e.g. [\"V1@45\", \"A1@45\"]".into()),
    };
    let by = need_by(op, fps)?;
    let mut picks: Vec<(Lane, usize)> = Vec::new();
    for v in list {
        let p = clip_arg(s, &json!({ "clip": v })).map_err(|e| format!("nudge clips: {e}"))?;
        if !picks.contains(&p) {
            picks.push(p);
        }
    }
    let lanes: Vec<Lane> = picks.iter().map(|p| p.0).collect();
    check_unlocked(s, &lanes)?;
    if by == 0 {
        return Ok(plain("nudge: by 0f, nothing to move".into(), Vec::new()));
    }
    let &(al, ai) = picks.last().ok_or("nudge: no clips")?;
    let anchor = s.lane_clips(al)[ai].clone();
    let from = anchor.start;
    let before: Vec<(Lane, u32)> = picks.iter().map(|&(l, i)| (l, s.lane_clips(l)[i].start)).collect();
    let names: Vec<String> = before.iter().map(|&(l, st)| parse::addr(l, st)).collect();
    engine::editkeys::nudge(s, &picks, by).map_err(|why| format!("nudge {} refused: {why}", names.join(", ")))?;
    // The move stops at the room there is, so the landing is read back from the anchor, not assumed.
    let landed = s
        .lane_clips(al)
        .iter()
        .find(|c| (c.source, c.in_frame, c.out_frame) == (anchor.source, anchor.in_frame, anchor.out_frame) && c.link == anchor.link)
        .map_or(from, |c| c.start);
    let moved = i64::from(landed) - i64::from(from);
    if moved == 0 {
        return Err(format!("nudge {} refused: a neighbouring clip is in the way", names.join(", ")));
    }
    let touched: Vec<(Lane, u32)> = before.iter().map(|&(l, st)| (l, (i64::from(st) + moved).max(0) as u32)).collect();
    let now: Vec<String> = touched.iter().map(|&(l, st)| parse::addr(l, st)).collect();
    let mut text = format!("nudge {} by {}: now {}", names.join(", "), signed(moved), now.join(", "));
    if moved != by {
        text.push_str(&format!("; stopped short of {} (the room there is, or the start of the timeline)", signed(by)));
    }
    Ok(plain(text, touched))
}

/// Refuses an op that names a lane the timeline does not have, before the op runs. The lanes an op
/// names (`lane`, `lanes`, `to_lane`, `audio_lane`, `under`) must be on the timeline, and the refusal
/// names the ones that are. Placements (which add a lane with `lane_add` in the fix they name) and
/// caption ops (which make a missing caption lane themselves) keep their own checks.
fn check_named_lanes(s: &PlaybackSession, name: &str, op: &Value) -> Result<(), String> {
    if SUB_OPS.contains(&name) || matches!(name, "place" | "append" | "keep" | "assemble") {
        return Ok(());
    }
    let mut raws: Vec<&str> = ["lane", "to_lane", "audio_lane", "under"].iter().filter_map(|k| op.get(*k)?.as_str()).collect();
    if let Some(list) = op.get("lanes").and_then(Value::as_array) {
        raws.extend(list.iter().filter_map(Value::as_str));
    }
    let lanes = s.lanes();
    for raw in raws {
        // A name that does not parse is the op's own refusal to give.
        let Ok(l) = parse::lane(raw) else { continue };
        if !lanes.contains(&l) {
            let names: Vec<String> = lanes.iter().copied().map(parse::lane_name).collect();
            return Err(format!("{raw} is not a lane on this timeline; the lanes are {}", names.join(", ")));
        }
    }
    Ok(())
}

fn run_op(s: &mut PlaybackSession, name: &str, op: &Value, fps: f64, project: Option<&Path>, tones: Option<&Path>, dry: bool) -> Result<Done, String> {
    check_keys(name, op)?;
    check_named_lanes(s, name, op)?;
    if let Some(msg) = kind_refusal(s, name, op) {
        return Err(msg);
    }
    let mut done = run_op_inner(s, name, op, fps, project, tones, dry).map_err(|e| missing_key_hint(name, op, e))?;
    done.text.push_str(&time_notes(name, op, fps));
    Ok(done)
}

fn run_op_inner(s: &mut PlaybackSession, name: &str, op: &Value, fps: f64, project: Option<&Path>, tones: Option<&Path>, dry: bool) -> Result<Done, String> {
    let end = parse::session_end(s);
    match name {
        "place" => op_place(s, op, fps, end),
        "insert" => op_three_point(s, SourceEdit::Insert, op, fps, end),
        "overwrite" => op_three_point(s, SourceEdit::Overwrite, op, fps, end),
        "append" => op_append(s, op, fps, end),
        "keep" => op_keep(s, op, fps, end),
        "assemble" => op_assemble(s, op, fps, end),
        "split" => op_split(s, op, fps, end),
        "trim" => op_trim(s, op, fps, end),
        "split_edit" => op_split_edit(s, op, fps, end),
        "roll" => op_roll(s, op, fps, end),
        "slip" => op_slip(s, op, fps),
        "slide" => op_slide(s, op, fps),
        "move" => op_move(s, op, fps, end),
        "copy_look" => op_copy_look(s, op),
        "replace" => op_replace(s, op, fps),
        "nudge" => op_nudge(s, op, fps),
        "delete" => op_delete(s, op, fps),
        "lift" => op_lift(s, op),
        "swap" => op_swap(s, op),
        "ripple_delete" => op_ripple_delete(s, op, fps),
        "close_gaps" => op_close_gaps(s, op),
        "cut_regions" => op_cut_regions(s, op, fps, end),
        "insert_time" => op_insert_time(s, op, fps, end),
        "freeze" => op_freeze(s, op, fps, end, project, dry),
        "remove_time" => op_remove_time(s, op, fps, end),
        "speed" => op_speed(s, op, fps),
        "speed_ramp" => op_speed_ramp(s, op, fps),
        "reverse" => op_reverse(s, op),
        "fade" => op_fade(s, op, fps, end),
        "dissolve" | "transition" => op_dissolve(s, op, fps, name == "transition"),
        "crossfade" => op_crossfade(s, op, fps),
        "visualizer" => op_visualizer(s, op),
        "chapter_add" | "chapter_set" | "chapter_remove" => op_chapter(s, name, op, fps, end),
        "marker_add" | "marker_set" | "marker_remove" => op_marker(s, name, op, fps, end),
        "beats" => op_beats(s, op, fps),
        "transform" => match edit_fx::apply(s, op, fps) {
            Some(r) => r.map(|applied| {
                let mut d = Done::from(off_canvas_note(s, applied));
                d.text.push_str(&rotate_note(op));
                d
            }),
            None => Err("transform is not answered here".to_owned()),
        },
        _ => {
            if let Some(r) = generated::apply(s, op, fps, tones) {
                return r.map(Done::from);
            }
            if let Some(r) = edit_fx::apply(s, op, fps) {
                return r.map(Done::from);
            }
            if let Some(r) = edit_subs::apply(s, op, fps) {
                return r.map(Done::from);
            }
            Err(format!(
                "unknown op `{name}`; known: {}",
                OWN_OPS.iter().chain(FX_OPS.iter()).chain(SUB_OPS.iter()).copied().collect::<Vec<_>>().join(", ")
            ))
        }
    }
}

// ---- reporting ------------------------------------------------------------

/// Every clip and caption on the timeline as (lane, identity, start). A clip's identity is what
/// it plays (its source range), which a ripple or a move does not change, so it finds the clip
/// again after the op.
type Layout = Vec<(Lane, String, u32)>;

fn layout(s: &PlaybackSession) -> Layout {
    let mut out = Vec::new();
    for l in s.lanes() {
        if l.kind == LaneKind::Subtitle {
            out.extend(s.sub_lane(l).iter().map(|c| (l, format!("caption {}:{}:{}", c.track, c.in_us, c.out_us), c.start)));
        } else {
            out.extend(s.lane_clips(l).iter().map(|c| (l, format!("clip {}:{}:{}", c.source, c.in_frame, c.out_frame), c.start)));
        }
    }
    out
}

/// The lane_move checks, before the op runs: `to` is the 1-based position of the lane in the lane
/// list, and the refusal shows that list so the position can be read off it.
fn lane_move_check(s: &PlaybackSession, op: &Value) -> Result<(), String> {
    let lanes = s.lanes();
    let n = lanes.len();
    let raw = op.get("lane").and_then(Value::as_str).unwrap_or("?");
    // A locked lane does not move: the lock is checked before the position, so the refusal names it.
    if let Ok(lane) = parse::lane(raw)
        && s.lane_locked(lane)
    {
        return Err(format!("lane_move {raw}: {}", lock_msg(lane)));
    }
    let list: Vec<String> = lanes.iter().enumerate().map(|(i, l)| format!("{} ({})", parse::lane_name(*l), i + 1)).collect();
    let list = list.join(", ");
    match op.get("to") {
        None | Some(Value::Null) => Err(format!(
            "lane_move {raw}: missing `to`; give the 1-based position of the lane in the lane list, 1–{n}. The lanes are: {list}"
        )),
        Some(v) => match v.as_f64() {
            Some(x) if x.fract() == 0.0 && x >= 1.0 && x <= n as f64 => Ok(()),
            Some(x) => Err(format!(
                "lane_move {raw}: to {x} is not a lane position; `to` is the 1-based position of the lane in the lane list, a whole number 1–{n}. The lanes are: {list}"
            )),
            None => Err(format!(
                "lane_move {raw}: `to` must be the 1-based position of the lane in the lane list (as `timeline` lists lanes), a whole number 1–{n}, not {}. The lanes are: {list}",
                parse::kind_of(v)
            )),
        },
    }
}

/// The clips a lane_move shifted, as (old address, new address). A move renumbers lanes: a label is
/// a position among the lanes of its kind, so the lanes between the two positions take the next
/// label over. Each lane is followed by its position (old position to new position), and each clip
/// is matched on its new lane by what it plays and where it starts; a clip whose address is the same is not listed.
fn lane_move_shifts(op: &Value, before_lanes: &[Lane], before: &Layout, after: &Layout, after_lanes: &[Lane]) -> Vec<(String, String)> {
    let Some(lane) = op.get("lane").and_then(Value::as_str).and_then(|t| parse::lane(t).ok()) else {
        return Vec::new();
    };
    let (Some(from), Some(to)) = (
        before_lanes.iter().position(|l| *l == lane),
        op.get("to").and_then(Value::as_f64).filter(|x| *x >= 1.0).map(|x| x as usize - 1),
    ) else {
        return Vec::new();
    };
    let new_pos = |p: usize| -> usize {
        if p == from {
            to
        } else if from < to && p > from && p <= to {
            p - 1
        } else if to < from && p >= to && p < from {
            p + 1
        } else {
            p
        }
    };
    let mut out = Vec::new();
    for x in before {
        let Some(p) = before_lanes.iter().position(|l| *l == x.0) else { continue };
        let Some(&nl) = after_lanes.get(new_pos(p)) else { continue };
        if let Some(y) = after.iter().find(|y| y.0 == nl && y.1 == x.1 && y.2 == x.2)
            && (y.0, y.2) != (x.0, x.2)
        {
            out.push((parse::addr(x.0, x.2), parse::addr(y.0, y.2)));
        }
    }
    out
}

/// The clips an op moved, as (old address, new address). An identity counts only when it occurs
/// once on each side, so a clip the op added or removed is never paired with another one: such a
/// clip is the same clip wherever it went (a move jumping over others keeps every lane's count).
/// The rest follow the lane rule: a lane that kept its item count keeps its items in order.
fn shifted_pairs(before: &Layout, after: &Layout) -> Vec<(String, String)> {
    shifted_pairs_cut(before, after, None)
}

/// The span a cutting op removes, as (start, frames), read before the op runs (the clip is gone
/// after it). None for an op that cuts nothing out of the timeline.
fn ripple_cut(s: &PlaybackSession, name: &str, op: &Value, fps: f64) -> Option<(u32, u32)> {
    match name {
        "delete" | "ripple_delete" => clip_arg(s, op).ok().map(|(l, i)| {
            let c = &s.lane_clips(l)[i];
            (c.start, c.frames())
        }),
        "remove_time" => Some((need(op, "at", fps, u32::MAX).ok()?, need(op, "duration", fps, u32::MAX).ok()?)),
        _ => None,
    }
}

/// `shifted_pairs` for an op that cuts `span` (the `(start, frames)` pair) out of the timeline. A
/// clip that plays the same source range twice on its lane (two takes cut from one place) cannot
/// be paired by its identity alone: the cut says where each one lands. A clip that starts at or
/// after the cut's end moves back by the span, so it is paired with the clip now at that start.
fn shifted_pairs_cut(before: &Layout, after: &Layout, cut: Option<(u32, u32)>) -> Vec<(String, String)> {
    if before == after {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut used = vec![false; after.len()];
    let mut taken = vec![false; before.len()];
    if let Some((from, span)) = cut {
        for (i, x) in before.iter().enumerate() {
            let twins = before.iter().filter(|y| y.0 == x.0 && y.1 == x.1).count();
            if twins < 2 || (from..from.saturating_add(span)).contains(&x.2) {
                continue;
            }
            let to = if x.2 >= from.saturating_add(span) { x.2 - span } else { x.2 };
            if let Some(j) = (0..after.len()).find(|&j| !used[j] && after[j].0 == x.0 && after[j].1 == x.1 && after[j].2 == to) {
                used[j] = true;
                taken[i] = true;
                if to != x.2 {
                    out.push((parse::addr(x.0, x.2), parse::addr(x.0, to)));
                }
            }
        }
    }
    let open_before: Layout = before.iter().enumerate().filter(|(i, _)| !taken[*i]).map(|(_, x)| x.clone()).collect();
    let mut pend_before: Layout = Vec::new();
    // Pass one pairs a clip with the one on its own lane, pass two (a clip sent to another lane)
    // with the only one of its identity anywhere; a take's partners share an identity, so they wait for pass one.
    for pass in 0..2 {
        let mut next_pending: Layout = Vec::new();
        for x in if pass == 0 { open_before.clone() } else { std::mem::take(&mut pend_before) } {
            let same = |y: &(Lane, String, u32)| y.1 == x.1 && (pass == 1 || y.0 == x.0);
            let was = open_before.iter().filter(|y| same(y)).count();
            let now: Vec<usize> = (0..after.len()).filter(|&j| same(&after[j]) && !used[j]).collect();
            let (pool_now, pool_was) = (now.len(), was);
            if pass == 1 && pool_was == 1 && pool_now == 1 || pass == 0 && pool_was == 1 && pool_now == 1 {
                let j = now[0];
                used[j] = true;
                let y = &after[j];
                if (y.0, y.2) != (x.0, x.2) {
                    out.push((parse::addr(x.0, x.2), parse::addr(y.0, y.2)));
                }
            } else {
                next_pending.push(x);
            }
        }
        pend_before = next_pending;
    }
    let pend_after: Layout = after.iter().enumerate().filter(|(j, _)| !used[*j]).map(|(_, y)| y.clone()).collect();
    let mut lanes_seen: Vec<Lane> = Vec::new();
    for x in &pend_before {
        if !lanes_seen.contains(&x.0) {
            lanes_seen.push(x.0);
        }
    }
    let mut paired: Vec<Lane> = Vec::new();
    for &l in &lanes_seen {
        let b: Vec<&(Lane, String, u32)> = pend_before.iter().filter(|x| x.0 == l).collect();
        let a: Vec<&(Lane, String, u32)> = pend_after.iter().filter(|x| x.0 == l).collect();
        if a.len() == b.len() {
            paired.push(l);
            for (x, y) in b.iter().zip(a.iter()) {
                if x.2 != y.2 {
                    out.push((parse::addr(l, x.2), parse::addr(l, y.2)));
                }
            }
        }
    }
    let rest_before: Layout = pend_before.iter().filter(|x| !paired.contains(&x.0)).cloned().collect();
    let rest_after: Layout = pend_after.iter().filter(|x| !paired.contains(&x.0)).cloned().collect();
    out.extend(identity_pairs(&rest_before, &rest_after));
    out
}

/// The clips of lanes whose count changed, paired by what they play (`identity_pairs`'s rule).
fn identity_pairs(before: &Layout, after: &Layout) -> Vec<(String, String)> {
    if before == after {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, (lane, key, start)) in before.iter().enumerate() {
        let was = before.iter().filter(|x| x.1 == *key).count();
        let now: Vec<&(Lane, String, u32)> = after.iter().filter(|x| x.1 == *key).collect();
        if now.len() != was {
            continue;
        }
        let nth = before[..i].iter().filter(|x| x.1 == *key).count();
        let (nl, _, ns) = now[nth];
        if (*nl, *ns) != (*lane, *start) {
            out.push((parse::addr(*lane, *start), parse::addr(*nl, *ns)));
        }
    }
    out
}

/// Lane lines: `V1: @0 0.000–15.000 src:0 · @450 15.000–30.000 src:0`.
fn digest(s: &PlaybackSession, lanes: &[Lane], fps: f64) -> String {
    const MAX: usize = 12;
    let mut out = Vec::new();
    for l in s.lanes() {
        if !lanes.contains(&l) {
            continue;
        }
        let items: Vec<String> = if l.kind == LaneKind::Subtitle {
            s.sub_lane(l)
                .iter()
                .map(|c| {
                    let end = c.start.saturating_add(c.frames);
                    format!("@{} {:.3}–{:.3} caption", c.start, parse::secs(c.start, fps), parse::secs(end, fps))
                })
                .collect()
        } else {
            s.lane_clips(l)
                .iter()
                .map(|c| {
                    format!(
                        "@{} {:.3}–{:.3} src:{}",
                        c.start,
                        parse::secs(c.start, fps),
                        parse::secs(c.end(), fps),
                        c.source
                    )
                })
                .collect()
        };
        let body = if items.is_empty() {
            "no clips".to_string()
        } else {
            let mut shown: Vec<String> = items.iter().take(MAX).cloned().collect();
            if items.len() > MAX {
                shown.push(format!("… {} more", items.len() - MAX));
            }
            shown.join(" · ")
        };
        out.push(format!("{}: {body}", parse::lane_name(l)));
    }
    out.join("\n")
}

/// One op as a short name for the undo report: `split @45`, `fade V1@0`, `place src:0`.
fn op_tag(op: &Value) -> String {
    let name = op.get("op").and_then(Value::as_str).unwrap_or("?");
    let text = |v: &Value| match v {
        Value::String(t) => t.clone(),
        other => other.to_string(),
    };
    let target = if let Some(c) = op.get("clip") {
        text(c)
    } else if let Some(t) = op.get("title") {
        text(t)
    } else if let Some(l) = op.get("lane") {
        text(l)
    } else if let Some(k) = op.get("kind") {
        text(k)
    } else if let Some(src) = op.get("source") {
        format!("src:{}", text(src).trim_start_matches("src:"))
    } else if let Some(a) = op.get("at") {
        format!("@{}", text(a))
    } else {
        String::new()
    };
    if target.is_empty() {
        name.to_string()
    } else {
        format!("{name} {target}")
    }
}

/// One clip op as the apply reply names it: the op and the clips it touched (`fade V1@0, A1@0`), so a
/// linked fade reads with its sound. None for an op with no `clip` or that touched nothing.
fn clip_tag(op: &Value, touched: &[String]) -> Option<String> {
    if op.get("clip").is_none() || touched.is_empty() {
        return None;
    }
    const NAMED: usize = 3;
    let mut text = touched.iter().take(NAMED).cloned().collect::<Vec<_>>().join(", ");
    if touched.len() > NAMED {
        text.push_str(&format!(" +{} more", touched.len() - NAMED));
    }
    let name = op.get("op").and_then(Value::as_str).unwrap_or("?");
    Some(format!("{name} {text}"))
}

/// A batch's default undo label: its first two ops, then how many more. A clip op is named by the
/// clips its apply reply names (`touched`, one list per op), so the label matches what was applied.
fn summarize(ops: &[Value], touched: &[Vec<String>]) -> String {
    let tags: Vec<String> = ops
        .iter()
        .enumerate()
        .map(|(k, op)| clip_tag(op, touched.get(k).map_or(&[][..], Vec::as_slice)).unwrap_or_else(|| op_tag(op)))
        .collect();
    match tags.len() {
        0 => "edit".to_string(),
        1 | 2 => tags.join(" + "),
        n => format!("{} + {} (+{} more)", tags[0], tags[1], n - 2),
    }
}

/// Takes `n` engine undo steps back. False if the engine would not take one.
/// "1 edit batch" or "3 edit batches", for the undo and redo refusals.
fn batches(n: usize) -> String {
    if n == 1 {
        "1 edit batch".to_string()
    } else {
        format!("{n} edit batches")
    }
}

fn undo_n(s: &mut PlaybackSession, n: usize) -> bool {
    (0..n).all(|_| s.undo())
}

fn redo_n(s: &mut PlaybackSession, n: usize) -> bool {
    (0..n).all(|_| s.redo())
}

thread_local! {
    /// What the batch being run wrote outside the project file: the freeze stills it saved and the
    /// `freeze` folder it made, in the order written. A refused batch takes these back.
    static BATCH_WRITES: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Notes a path the current batch wrote, so a refusal can take it back.
fn note_write(path: PathBuf) {
    BATCH_WRITES.with(|w| w.borrow_mut().push(path));
}

/// Takes back what the current batch wrote, newest first: each still, then the folder it made
/// (which goes only when empty). Files the batch did not create are never noted, so never removed.
fn take_back_writes() {
    let writes = BATCH_WRITES.with(|w| std::mem::take(&mut *w.borrow_mut()));
    for p in writes.iter().rev() {
        let _ = if p.is_dir() { std::fs::remove_dir(p) } else { std::fs::remove_file(p) };
    }
}

/// Where a batch starts: the engine's push count, the source count, and whether the project was
/// clean (saved and unchanged), which a rollback restores.
struct BatchMark {
    pushes: u64,
    sources: usize,
    clean: bool,
    /// The redo history the batch found, until it is put back or kept (see [`SavedRedo`]).
    redo: Option<SavedRedo>,
}

/// What a batch found before its ops ran: the engine's redo stack and the ledger's redo entries, and
/// the undo cap held open. The ops clear the redo stack and their pushes can evict the oldest undo
/// step, and the rollback fills the redo stack, so a batch that is rolled back puts all of it back
/// and one that is kept lets it go.
struct SavedRedo {
    stack: RedoCheckpoint,
    ledger: Vec<Entry>,
}

impl SavedRedo {
    fn take(s: &mut PlaybackSession, ledger: &mut Vec<Entry>) -> Self {
        s.hold_history(true);
        Self { stack: s.take_redo(), ledger: std::mem::take(ledger) }
    }
}

/// Puts back the redo history a rolled-back batch took out, if it has not been put back yet.
fn put_back_redo(mark: &mut BatchMark, s: &mut PlaybackSession, ledger: &mut Vec<Entry>) {
    if let Some(saved) = mark.redo.take() {
        s.restore_redo(saved.stack);
        *ledger = saved.ledger;
        s.hold_history(false);
    }
}

/// Lets go of the redo history a kept batch took out: its own edits cleared it, as they always did.
fn keep_redo(mark: &mut BatchMark, s: &mut PlaybackSession) {
    if mark.redo.take().is_some() {
        s.hold_history(false);
    }
}

/// Takes back the sources a rolled-back batch imported (a freeze's still), newest first, with
/// `remove_unused_source`, which keeps the undo and redo history: a source that an undo or redo step
/// still names stays, and the caller says so. Returns how many were taken back.
fn drop_batch_sources(s: &mut PlaybackSession, sources_before: usize) -> usize {
    let mut taken = 0;
    while s.sources().len() > sources_before {
        let Some(src) = s.sources().last().cloned() else { break };
        if s.remove_unused_source(&src.path, src.audio_stream).is_err() {
            break;
        }
        taken += 1;
    }
    taken
}

/// A failed batch: roll back what it pushed, take back the sources it imported, and say so.
fn refuse(srv: &mut Server, mark: BatchMark, k: usize, name: &str, msg: &str) -> Reply {
    let tail = roll_back(srv, mark);
    Reply::err(format!("op {k} ({name}) failed: {msg}; {tail}"))
}

/// An op that is not an object with an `op` name: the refusal says what the op must look like,
/// with no name to show, and the batch is rolled back the same way.
fn refuse_shape(srv: &mut Server, mark: BatchMark, k: usize, msg: &str) -> Reply {
    let tail = roll_back(srv, mark);
    Reply::err(format!("op {k} {msg}; {tail}"))
}

/// Rolls a refused batch back and returns the words that say so for the refusal.
fn roll_back(srv: &mut Server, mut mark: BatchMark) -> String {
    let Some(s) = srv.session.as_mut() else {
        return NO_PROJECT.to_string();
    };
    let n = pushed_since(s, mark.pushes);
    let note = if undo_n(s, n) {
        ""
    } else {
        " (the engine would not undo every step; check the project before going on)"
    };
    // The undo above left the rolled-back states on the redo history: they are not a branch to redo,
    // so the history the batch found goes back in their place.
    put_back_redo(&mut mark, s, &mut srv.redo_ledger);
    drop_batch_sources(s, mark.sources);
    take_back_writes();
    if mark.clean {
        // The rollback leaves the project as it was: a project that was clean reads clean again.
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
    }
    // A source a batch imported stays in the library when it could not be taken back unharmed.
    let kept_note = if s.sources().len() > mark.sources {
        " (the still stays in the media library; media_remove drops it)"
    } else {
        ""
    };
    srv.trim_ledger();
    format!("batch rolled back, nothing changed{note}{kept_note}")
}

/// Snapshots the engine pushed since `start` (a `history_pushes` reading). Exact
/// even at the cap, where the undo depth no longer grows.
fn pushed_since(s: &PlaybackSession, start: u64) -> usize {
    s.history_pushes().saturating_sub(start) as usize
}

/// The whole project as text: the engine's own project dump (sources, lanes with their gain,
/// flags, clips, eq, color, transform, captions, cues, titles, settings and the limiter). Two equal
/// texts are the same state, so a batch whose state matches the state before it changed nothing.
/// Only the undo bookkeeping is left out: `pushes` counts every snapshot ever taken and an undo
/// never restores it, and `revision` is minted by every snapshot (a no-op's too), so either would
/// make every batch look like a change.
fn state_of(s: &PlaybackSession) -> String {
    let (project, _meta) = s.raw_snapshot();
    without_tables(&without_counters(&format!("{project:?}")))
}

/// `dump` with the append-only effect tables (eq, color, transform) removed. An undo never takes
/// their entries back, so a grade undone would read as a change; each clip still names its own
/// entry (`color: Some(n)`), so a real change to a clip's grade is still seen.
fn without_tables(dump: &str) -> String {
    let mut out = dump.to_string();
    for key in ["eq: [", "color: [", "transform: ["] {
        // A field of its own name only: `box_color: [` (a title's box) is not the `color: [` table.
        let mut from = 0;
        while let Some(i) = out[from..].find(key).map(|i| i + from) {
            if out[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
                from = i + key.len();
                continue;
            }
            let open = i + key.len() - 1;
            let mut depth = 0usize;
            let mut end = None;
            for (k, ch) in out[open..].char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + k + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { break };
            let tail = if out[end..].starts_with(", ") { end + 2 } else { end };
            out.replace_range(i..tail, "");
        }
    }
    out
}

/// `dump` with the value of every bookkeeping id removed: `pushes: N` (the snapshot count) and
/// `revision: N` (the state id, which every snapshot mints afresh, a no-op batch's included, and an
/// undo restores). Neither is timeline content, so neither makes a batch a change.
fn without_counters(dump: &str) -> String {
    ["pushes: ", "revision: "].iter().fold(dump.to_string(), |text, key| without_value(&text, key))
}

/// `dump` with the digits after every `key` removed (the key itself stays).
fn without_value(dump: &str, key: &str) -> String {
    let mut out = String::with_capacity(dump.len());
    let mut rest = dump;
    while let Some(i) = rest.find(key) {
        out.push_str(&rest[..i + key.len()]);
        rest = &rest[i + key.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        rest = &rest[digits..];
    }
    out.push_str(rest);
    out
}

/// Notes on the timeline's ends for a batch reply: a video lane that stops short of the
/// timeline end, and picture that ends before sound (black after the picture). Empty when the
/// ends agree to within a frame.
fn tail_notes(s: &PlaybackSession, fps: f64) -> Vec<String> {
    let end = parse::session_end(s);
    let mut notes = Vec::new();
    let mut video_end: Option<u32> = None;
    let mut audio_end: Option<u32> = None;
    for l in s.lanes() {
        if l.kind == LaneKind::Subtitle {
            continue;
        }
        let Some(lane_end) = s.lane_clips(l).iter().map(|c| c.end()).max() else {
            continue;
        };
        if l.kind == LaneKind::Video {
            video_end = Some(video_end.map_or(lane_end, |v| v.max(lane_end)));
            if lane_end + 1 < end {
                notes.push(format!(
                    "{} ends {} before the timeline end",
                    parse::lane_name(l),
                    secs_text(end - lane_end, fps)
                ));
            }
        } else {
            audio_end = Some(audio_end.map_or(lane_end, |a| a.max(lane_end)));
        }
    }
    if let (Some(v), Some(a)) = (video_end, audio_end)
        && a > v + 1
    {
        notes.push(format!(
            "picture ends at {} but sound runs to {} — {} of black",
            secs_text(v, fps),
            secs_text(a, fps),
            secs_text(a - v, fps)
        ));
    }
    // A picture lane above the picture underneath that runs past its end shows over black there (a logo
    // left after the picture shrank): named by its lane. The picture underneath is the lowest video lane
    // with clips (V1 unless a lane_move put another lane below it), and only lanes above it are checked.
    let video: Vec<Lane> = s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).collect();
    if let Some(&base) = video.iter().find(|&&l| !s.lane_clips(l).is_empty()) {
        let base_end = s.lane_clips(base).iter().map(|c| c.end()).max().unwrap_or(0);
        for &l in video.iter().skip_while(|&&l| l != base).skip(1) {
            let Some(e) = s.lane_clips(l).iter().map(|c| c.end()).max() else {
                continue;
            };
            if e > base_end + 1 {
                notes.push(format!(
                    "{} runs {} past the picture's end at {}",
                    parse::lane_name(l),
                    secs_text(e - base_end, fps),
                    secs_text(base_end, fps)
                ));
            }
        }
    }
    // A caption or title that runs past the last picture frame shows over black: name it.
    if let Some(v) = video_end {
        for l in s.lanes().into_iter().filter(|l| l.kind == LaneKind::Subtitle) {
            let Some(e) = s.sub_lane(l).iter().map(|c| c.start + c.frames).max() else {
                continue;
            };
            if e > v + 1 {
                notes.push(format!(
                    "{} runs {} past the picture's end at {}",
                    parse::lane_name(l),
                    secs_text(e - v, fps),
                    secs_text(v, fps)
                ));
            }
        }
        for (i, t) in s.titles().iter().enumerate() {
            let e = t.start + t.frames;
            if e > v + 1 {
                notes.push(format!(
                    "T{} runs {} past the picture's end at {}",
                    i + 1,
                    secs_text(e - v, fps),
                    secs_text(v, fps)
                ));
            }
        }
    }
    notes
}

/// One clip or caption for an op's side effects: its lane, what it plays (its source and in
/// point, which a move, a ripple or an out-edge trim leaves alone), its start and its link.
type Item = (Lane, String, u32, Option<u32>);

/// The timeline's shape for one op's side effects: every clip and caption as an `Item`, and each
/// lane's clip count and gap count.
pub(crate) struct Shape {
    items: Vec<Item>,
    clips: Vec<(Lane, usize)>,
    holes: Vec<(Lane, usize)>,
    /// The empty spans between a lane's clips, in frames, so a hole the op left can be named by its range.
    gaps: Vec<(Lane, u32, u32)>,
    /// The gaps close_gaps would leave open, with the reason (see `take_gap_refusal`).
    stuck: Vec<(Lane, u32, u32, String)>,
    /// The timeline's frame rate, for naming those spans in seconds.
    fps: f64,
    /// Per media lane: its dissolves (count and total frames) and its clips with a fade.
    marks: Vec<(Lane, Marks)>,
    /// Every clip with a dissolve out, and the clip the engine blends it into (the one right after it,
    /// only while the two are adjacent: a gap makes it a hard cut).
    joins: Vec<Join>,
}

/// A dissolve out: the clip that owns it (`lane`, its identity, its start) and the clip it joins.
#[derive(Clone)]
struct Join {
    lane: Lane,
    ident: String,
    start: u32,
    next: Option<(String, u32)>,
}

/// What a lane's transitions and fades add up to: dissolves (clips and frames), fades (clips).
#[derive(Clone, Copy, Default)]
struct Marks {
    dissolves: usize,
    dissolve_frames: u32,
    fades: usize,
}

pub(crate) fn shape(s: &PlaybackSession) -> Shape {
    let mut items = Vec::new();
    let mut clips = Vec::new();
    let mut holes = Vec::new();
    let mut marks = Vec::new();
    let mut gaps = Vec::new();
    let mut stuck = Vec::new();
    let mut joins = Vec::new();
    for l in s.lanes() {
        holes.push((l, s.gap_count(l)));
        if l.kind != LaneKind::Subtitle {
            let mut prev_end: Option<u32> = None;
            for c in s.lane_clips(l) {
                if let Some(p) = prev_end.filter(|&p| c.start > p) {
                    gaps.push((l, p, c.start));
                    if let Some(why) = take_gap_refusal(s, l, p, c.start - p) {
                        stuck.push((l, p, c.start, why));
                    }
                }
                prev_end = Some(prev_end.map_or(c.end(), |p| p.max(c.end())));
            }
        }
        if l.kind == LaneKind::Subtitle {
            items.extend(s.sub_lane(l).iter().map(|c| (l, format!("caption {}:{}", c.track, c.in_us), c.start, c.link)));
        } else {
            let lane_clips = s.lane_clips(l);
            clips.push((l, lane_clips.len()));
            items.extend(lane_clips.iter().map(|c| (l, format!("clip {}:{}", c.source, c.in_frame), c.start, c.link)));
            for (i, c) in lane_clips.iter().enumerate().filter(|(_, c)| c.transition_out > 0) {
                let next = lane_clips.get(i + 1).filter(|b| b.start == c.end()).map(|b| (format!("clip {}:{}", b.source, b.in_frame), b.start));
                joins.push(Join { lane: l, ident: format!("clip {}:{}", c.source, c.in_frame), start: c.start, next });
            }
            let dissolving = lane_clips.iter().filter(|c| c.transition_out > 0);
            marks.push((
                l,
                Marks {
                    dissolves: dissolving.clone().count(),
                    dissolve_frames: dissolving.map(|c| c.transition_out).sum(),
                    fades: lane_clips.iter().filter(|c| c.fade_in > 0 || c.fade_out > 0).count(),
                },
            ));
        }
    }
    Shape { items, clips, holes, gaps, stuck, fps: parse::session_fps(s), marks, joins }
}

/// The item in `shape` that is the clip `lane`, `ident` starts at `start`. A clip's identity is what
/// it plays, so two clips of one source at one in-point on one lane share it (a copy placed earlier
/// on the lane, say): the one at `start` is taken, else the only one there is. Several with none at
/// `start` are ambiguous, and None: a clip is never matched to the wrong partner's state.
fn same_clip<'a>(shape: &'a Shape, lane: Lane, ident: &str, start: u32) -> Option<&'a Item> {
    let cands: Vec<&Item> = shape.items.iter().filter(|x| x.0 == lane && x.1 == ident).collect();
    match cands.iter().find(|x| x.2 == start) {
        Some(x) => Some(*x),
        None if cands.len() == 1 => Some(cands[0]),
        None => None,
    }
}

/// The link of the item in `shape` that is `a` (same lane, same source): `None` when there is no such item.
fn matched_link(shape: &Shape, a: &Item) -> Option<Option<u32>> {
    same_clip(shape, a.0, &a.1, a.2).map(|x| x.3)
}

/// Whether a lane that gained clips was split rather than given a clip from elsewhere. A split leaves
/// a new piece of a clip the lane already had (the same source, a new in-point); a clip moved or
/// copied in from another lane brings a source the lane did not hold, so it is not a split.
fn lane_split(before: &Shape, after: &Shape, l: Lane) -> bool {
    let source = |id: &str| id.split(':').next().unwrap_or("").to_string();
    let was: Vec<&Item> = before.items.iter().filter(|x| x.0 == l).collect();
    after.items.iter().filter(|x| x.0 == l).any(|a| {
        !was.iter().any(|b| b.1 == a.1) && was.iter().any(|b| source(&b.1) == source(&a.1))
    })
}

/// The lanes an op left a hole on, each with the gaps it added there: frames (start, end) and the reason
/// close_gaps would keep the gap open, None when it closes. A lane that was empty before has no hole
/// to leave: the silence before a first clip is the lane's own. A lane op leaves none.
pub(crate) fn holes_added(before: &Shape, after: &Shape, name: &str) -> Vec<(Lane, Gaps)> {
    if matches!(name, "lane_move" | "lane_add" | "lane_remove") {
        return Vec::new();
    }
    // Frames of empty time on a lane: a hole can grow without a new gap (an insert into a gap it left).
    let empty = |sh: &Shape, l: Lane| -> u32 { sh.gaps.iter().filter(|g| g.0 == l).map(|g| g.2 - g.1).sum() };
    let mut out = Vec::new();
    for &(l, _) in &after.holes {
        if !before.items.iter().any(|x| x.0 == l) {
            continue;
        }
        if empty(after, l) > empty(before, l) {
            // A gap the op only moved (a cut before it, an insert of a freeze) was there already: each
            // gap before the op answers for at most one gap after it, the same span first, else one of
            // the same length. What is left over is the hole the op made.
            let mut was_gaps: Vec<(u32, u32)> = before.gaps.iter().filter(|g| g.0 == l).map(|g| (g.1, g.2)).collect();
            let mut added: Gaps = Vec::new();
            for g in after.gaps.iter().filter(|g| g.0 == l) {
                let span = (g.1, g.2);
                let found = was_gaps
                    .iter()
                    .position(|x| *x == span)
                    .or_else(|| was_gaps.iter().position(|x| x.1 - x.0 == span.1 - span.0));
                match found {
                    Some(i) => {
                        was_gaps.remove(i);
                    }
                    None => {
                        let why = after.stuck.iter().find(|x| x.0 == g.0 && x.1 == g.1 && x.2 == g.2).map(|x| x.3.clone());
                        added.push((g.1, g.2, why));
                    }
                }
            }
            if !added.is_empty() {
                out.push((l, added));
            }
        }
    }
    out
}

/// The gaps one op left on a lane, as [`batch_hole_lines`] reads them: frames (start, end) and the
/// reason close_gaps keeps each open (None when it closes).
type Gaps = Vec<(u32, u32, Option<String>)>;

/// The gaps one op left on each lane (see [`holes_added`]).
type Holes = Vec<(Lane, Gaps)>;

/// What to tell the caller about closing the gaps a hole left on `lane`: the close_gaps call when it
/// would close them, else why it cannot (a take with a partner that has no gap there), so no reply
/// sends the caller to a close_gaps that reports nothing closed. `plural` for a line naming several.
fn close_clause(lane: Lane, gaps: &[(u32, u32, Option<String>)], plural: bool) -> String {
    let name = parse::lane_name(lane);
    let it = if plural { "them" } else { "it" };
    let mut reasons: Vec<&str> = Vec::new();
    for why in gaps.iter().filter_map(|g| g.2.as_deref()) {
        if !reasons.contains(&why) {
            reasons.push(why);
        }
    }
    let stuck = gaps.iter().filter(|g| g.2.is_some()).count();
    if stuck == 0 {
        format!("close_gaps {{lane:\"{name}\"}} closes {it}")
    } else if stuck == gaps.len() {
        format!("close_gaps cannot close {it}: {}", reasons.join("; "))
    } else {
        format!("close_gaps {{lane:\"{name}\"}} closes the others; {}", reasons.join("; "))
    }
}

/// The summary lines for a batch in which more than one op left a hole: one line per lane, naming the
/// gaps the batch left there in order (the first two, then `…`). Each line is paired with the index of
/// the op it prints under, the first op to leave a hole on that lane.
fn batch_hole_lines(holes: &[Holes], fps: f64) -> Vec<(usize, String)> {
    let mut lanes: Vec<(Lane, usize, Gaps)> = Vec::new();
    for (k, ops) in holes.iter().enumerate() {
        for (l, gaps) in ops {
            let i = match lanes.iter().position(|x| x.0 == *l) {
                Some(i) => i,
                None => {
                    lanes.push((*l, k, Vec::new()));
                    lanes.len() - 1
                }
            };
            // A gap that a repeated op leaves again starts at the same frame: it is named once.
            for g in gaps {
                if !lanes[i].2.iter().any(|x| x.0 == g.0) {
                    lanes[i].2.push(g.clone());
                }
            }
        }
    }
    lanes
        .into_iter()
        .map(|(l, k, gaps)| {
            let name = parse::lane_name(l);
            let at = if gaps.is_empty() {
                String::new()
            } else {
                let shown: Vec<String> = gaps.iter().take(2).map(|g| format!("{:.3}s", parse::secs(g.0, fps))).collect();
                let more = if gaps.len() > 2 { ", …" } else { "" };
                format!(" at {}{more}", shown.join(", "))
            };
            (k, format!("holes left on {name}{at} ({})", close_clause(l, &gaps, true)))
        })
        .collect()
}

/// The note for a placement that left a gap before the clip it put down: longer than 10 s, or longer
/// than the timeline was before the op (`end_then`). `touched` is the op's clips, the first one the
/// placed clip. None when the clip sits against what is before it.
fn placement_gap_note(s: &PlaybackSession, touched: &[(Lane, u32)], end_then: u32, fps: f64) -> Option<String> {
    let &(lane, start) = touched.iter().find(|(l, _)| l.kind != LaneKind::Subtitle)?;
    let prev_end = s.lane_clips(lane).iter().filter(|c| c.start < start).map(|c| c.end()).max().unwrap_or(0);
    let gap = start.saturating_sub(prev_end);
    let long = parse::secs(gap, fps) > 10.0;
    if gap == 0 || !(long || (end_then > 0 && gap > end_then)) {
        return None;
    }
    Some(format!(
        "left a {} gap before it on {} (the timeline was {})",
        secs_text(gap, fps),
        parse::lane_name(lane),
        secs_text(end_then, fps)
    ))
}

/// The side effects of one op beyond its own edit, diffed from the state before and after it:
/// clips split, holes left, takes unlinked (a take that lost members is still whole when the
/// members left share a link), and takes linked by a group.
pub(crate) fn side_effects(before: &Shape, after: &Shape, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    // A lane op renames lanes (lane_move reorders them), so per-lane counts say nothing there.
    if matches!(name, "lane_move" | "lane_add" | "lane_remove") {
        return out;
    }
    // A visualizer on a sound clip places its picture as a new clip, which is a placement, not a split.
    let placing = matches!(name, "place" | "append" | "keep" | "assemble" | "generator_add" | "adjustment_add" | "freeze" | "title_add" | "subtitles_import" | "lane_add" | "visualizer");
    let mut split = false;
    if !placing {
        for &(l, n) in &after.clips {
            let was = before.clips.iter().find(|x| x.0 == l).map_or(0, |x| x.1);
            if n > was && lane_split(before, after, l) {
                split = true;
                out.push(format!("split {} ×{}", parse::lane_name(l), n - was));
            }
        }
    }
    for (l, added) in holes_added(before, after, name) {
        // Named by the span it is: the gaps the op added on this lane, in seconds.
        let spans: Vec<String> = added
            .iter()
            .map(|&(a, b, _)| format!("{:.3}–{:.3}s", parse::secs(a, after.fps), parse::secs(b, after.fps)))
            .collect();
        let range = if spans.is_empty() { String::new() } else { format!(" at {}", spans.join(", ")) };
        out.push(format!("hole left on {}{range} by {name}; {}", parse::lane_name(l), close_clause(l, &added, false)));
    }
    // A transition or fade the op took away is named, so it is never dropped silently: a dissolve
    // whose clip or successor went (dropped), or one that kept its clip but lost frames (shortened).
    for &(l, now) in &after.marks {
        let was = before.marks.iter().find(|x| x.0 == l).map_or(Marks::default(), |x| x.1);
        let name = parse::lane_name(l);
        if now.dissolves < was.dissolves {
            // Named by the clip whose dissolve went, as it was before the op; a count the names do not
            // account for keeps the lane's count.
            let gone: Vec<String> = dropped_joins(before, after, l).iter().map(|j| parse::addr(l, j.start)).collect();
            if gone.len() == was.dissolves - now.dissolves {
                out.extend(gone.iter().map(|a| format!("dissolve dropped on {a}")));
            } else {
                out.push(format!("dissolve dropped on {name} ×{}", was.dissolves - now.dissolves));
            }
        } else if now.dissolve_frames < was.dissolve_frames {
            out.push(format!("dissolve shortened on {name} by {}f", was.dissolve_frames - now.dissolve_frames));
        }
        if now.fades < was.fades {
            out.push(format!("fade dropped on {name} ×{}", was.fades - now.fades));
        }
    }
    // A dissolve that survives may now blend into another clip: the one after it moved, went, or its gap
    // closed. The engine blends only into the next clip while they are adjacent, so that is what is named.
    for j in &before.joins {
        let Some(item) = same_clip(after, j.lane, &j.ident, j.start) else { continue };
        let Some(now) = after.joins.iter().find(|a| a.lane == j.lane && a.ident == item.1 && a.start == item.2) else { continue };
        let owner = parse::addr(j.lane, item.2);
        match (&j.next, &now.next) {
            (Some((bi, bs)), Some((ai, a_start))) if bi != ai => {
                let was = if same_clip(after, j.lane, bi, *bs).is_some() { parse::addr(j.lane, *bs) } else { "the deleted clip".to_string() };
                out.push(format!("{owner}'s dissolve now joins {} (was {was})", parse::addr(j.lane, *a_start)));
            }
            (Some(_), None) => out.push(format!("{owner}'s dissolve has no clip right after it now: it plays as a hard cut")),
            (None, Some((_, a_start))) => out.push(format!("{owner}'s dissolve now joins {}", parse::addr(j.lane, *a_start))),
            _ => {}
        }
    }
    // A trim keeps its take's links on purpose (its own reply says so).
    if !matches!(name, "trim" | "sub_trim") {
        let mut ids: Vec<u32> = before.items.iter().filter_map(|x| x.3).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let members: Vec<&Item> = before.items.iter().filter(|x| x.3 == Some(id)).collect();
            if members.len() < 2 {
                // An orphan link (one member) was already unlinked before the op.
                continue;
            }
            // Each member is found again by the clip it is, not by what it plays alone: a copy of the
            // same source at the same in-point elsewhere on its lane must not stand in for it.
            let found: Vec<(&Item, Option<&Item>)> = members.iter().map(|m| (*m, same_clip(after, m.0, &m.1, m.2))).collect();
            let survivors: Vec<&Item> = found.iter().filter(|(_, a)| a.is_some()).map(|(m, _)| *m).collect();
            if survivors.is_empty() {
                continue;
            }
            // `now` is one new link per survivor, in the survivors' order.
            let now: Vec<Option<u32>> = found.iter().filter_map(|(_, a)| a.map(|a| a.3)).collect();
            // A survivor is a take partner while another survivor shares its new link; only the rest detach.
            let shares = |i: usize| now[i].is_some() && now.iter().enumerate().any(|(j, x)| j != i && *x == now[i]);
            if name == "ungroup" {
                // Survivors that still share one link are one take: named once, with the partners in order.
                let mut links: Vec<u32> = now.iter().flatten().copied().collect();
                links.sort_unstable();
                links.dedup();
                for l in links {
                    let kept: Vec<String> = survivors.iter().zip(&now).filter(|(_, x)| **x == Some(l)).map(|(m, _)| parse::addr(m.0, m.2)).collect();
                    if kept.len() >= 2 {
                        out.push(format!("{} stay one take", kept.join("+")));
                    }
                }
            }
            // One line per detached survivor: the words `clip_info` and the timeline use.
            let why = if survivors.len() < members.len() { " (its partner was removed)" } else { "" };
            for (i, m) in survivors.iter().enumerate() {
                if shares(i) {
                    continue;
                }
                let addr = parse::addr(m.0, m.2);
                out.push(if split {
                    format!("{addr} is now detached at the cut")
                } else {
                    format!("{addr} is now detached{why}")
                });
            }
        }
    }
    // Clips that now share a link, which the op's own group made: members whose
    // earlier links differ, or that were unlinked before.
    let mut ids: Vec<u32> = after.items.iter().filter_map(|x| x.3).collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let members: Vec<&Item> = after.items.iter().filter(|x| x.3 == Some(id)).collect();
        if members.len() < 2 {
            continue;
        }
        let was: Vec<Option<u32>> = members.iter().filter_map(|m| matched_link(before, m)).collect();
        if was.is_empty() || was.iter().all(|x| *x == was[0]) && was[0].is_some() {
            continue;
        }
        let addrs: Vec<String> = members.iter().map(|m| parse::addr(m.0, m.2)).collect();
        out.push(format!("linked: {}", addrs.join(" + ")));
    }
    out
}

/// The clips on the lanes (not captions) that lie wholly inside `start..end` now, as `(lane, start, link)`:
/// what a delete of that span takes out. Read before the op.
fn clips_within(s: &PlaybackSession, lanes: &[Lane], start: u32, end: u32) -> Vec<(Lane, u32, Option<u32>)> {
    lanes
        .iter()
        .filter(|l| l.kind != LaneKind::Subtitle)
        .flat_map(|&l| s.lane_clips(l).iter().filter(move |c| c.start >= start && c.end() <= end).map(move |c| (l, c.start, c.link)))
        .collect()
}

/// `; removed V1@150 and A1@150 (its take)`: the clips a delete took out, named as they were. The take says
/// so when they were all one take. Empty when nothing was removed.
fn removed_words(gone: &[(Lane, u32, Option<u32>)]) -> String {
    if gone.is_empty() {
        return String::new();
    }
    let addrs: Vec<String> = gone.iter().map(|&(l, st, _)| parse::addr(l, st)).collect();
    let list = match addrs.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => addrs.join(", "),
    };
    let take = gone.len() >= 2 && gone[0].2.is_some() && gone.iter().all(|g| g.2 == gone[0].2);
    format!("; removed {list}{}", if take { " (its take)" } else { "" })
}

/// The dissolves on `lane` that the op took away, as they were before it: the clip that owned one is gone,
/// or no longer dissolves out.
fn dropped_joins<'a>(before: &'a Shape, after: &Shape, lane: Lane) -> Vec<&'a Join> {
    before
        .joins
        .iter()
        .filter(|j| j.lane == lane)
        .filter(|j| !same_clip(after, lane, &j.ident, j.start).is_some_and(|item| after.joins.iter().any(|a| a.lane == lane && a.ident == item.1 && a.start == item.2)))
        .collect()
}

/// A batch op that names a clip the earlier ops moved gets the reason and the op that moved it,
/// so the fix is to re-address or delete from the end. `msg` unchanged for other errors.
fn stale_address_hint(shifts: &[Vec<(String, String)>], msg: &str) -> String {
    let Some(rest) = msg.strip_prefix("no clip at ") else {
        return msg.to_string();
    };
    let addr = rest.split(';').next().unwrap_or("").trim();
    let lane: String = addr.chars().take_while(|c| *c != '@' && *c != '#').collect();
    let moved = shifts.iter().enumerate().find_map(|(j, v)| v.iter().find(|(a, _)| a == addr).map(|(_, b)| (j, b.clone())));
    match moved {
        Some((j, to)) => format!(
            "{msg}; ops run in order; op {} moved this clip (now {to}) — earlier ops in this batch shifted {lane} (see their shifted lists); delete from the end or use the new address",
            j + 1
        ),
        None if shifts.iter().flatten().any(|(a, _)| a.starts_with(&lane)) => format!(
            "{msg}; ops run in order; earlier ops in this batch shifted {lane} (see their shifted lists); delete from the end or use the new address"
        ),
        None => msg.to_string(),
    }
}

/// A sound clip with no sound clip meeting it at its end (a gap, or the lane's end) fades out to
/// silence over `frames`, and the next clip after a gap fades in from silence over the same. None
/// when the next clip meets it: then there is a crossfade to make, not a fade.
fn fade_to_silence(s: &mut PlaybackSession, lane: Lane, idx: usize, frames: u32) -> Option<String> {
    let (cur_end, cur_frames, next) = {
        let clips = s.lane_clips(lane);
        let cur = clips.get(idx)?;
        (cur.end(), cur.frames(), clips.get(idx + 1).map(|n| (n.start, n.frames())))
    };
    if matches!(next, Some((st, _)) if st == cur_end) {
        return None;
    }
    let addr = addr_of(s, lane, idx);
    let out_n = frames.min(cur_frames);
    if !s.set_fade_out(lane, idx, out_n) {
        return None;
    }
    let mut text = format!("sound on {addr} faded out {out_n}f to silence (no sound clip meets it)");
    if let Some((st, len)) = next {
        let gap = st.saturating_sub(cur_end);
        let in_n = frames.min(len);
        if s.set_fade_in(lane, idx + 1, in_n) {
            text.push_str(&format!(
                " and {} faded in from silence over {in_n}f after a {gap}f gap",
                addr_of(s, lane, idx + 1)
            ));
        }
    }
    Some(text)
}

pub(super) fn run_edit(srv: &mut Server, args: &Value) -> Reply {
    let Some(ops) = args.get("ops").and_then(Value::as_array).filter(|o| !o.is_empty()) else {
        return Reply::err(
            "missing argument `ops`: give a non-empty list of {op: \"...\", ...} objects; the edit tool description lists each op",
        );
    };
    let dry = opt_bool(args, "dry_run").unwrap_or(false);
    let label = opt_str(args, "label").map(str::to_string);
    BATCH_WRITES.with(|w| w.borrow_mut().clear());
    // A dry run rolls its edits back, but the engine's push count only grows, so the project
    // would read as dirty; when it was clean, its save marks are moved past the rolled-back steps.
    let was_clean = !super::project::dirty(srv);
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let fps = parse::session_fps(s);
    let start = s.history_pushes();
    let mut mark = BatchMark {
        pushes: start,
        sources: s.sources().len(),
        clean: was_clean,
        redo: Some(SavedRedo::take(s, &mut srv.redo_ledger)),
    };
    let state_before = state_of(s);
    let sources_before = s.sources().len();
    let end_before = parse::session_end(s);
    let project = srv.path.clone();
    let tones = crate::server::tone_folder(srv.path.as_deref(), srv.data_home.as_deref());

    // The notes that were already true before the batch are not repeated: only what it changed is.
    let notes_before = tail_notes(s, fps);
    let mut done: Vec<(String, Done)> = Vec::with_capacity(ops.len());
    let mut shifts: Vec<Vec<(String, String)>> = Vec::with_capacity(ops.len());
    let mut effects: Vec<Vec<String>> = Vec::with_capacity(ops.len());
    let mut holes: Vec<Holes> = Vec::with_capacity(ops.len());
    let mut gap_notes: Vec<String> = Vec::new();
    for (k, op) in ops.iter().enumerate() {
        let k1 = k + 1;
        if !op.is_object() {
            return refuse_shape(srv, mark, k1, "is not an op object: give {\"op\":\"<name>\", …}");
        }
        let name = op.get("op").and_then(Value::as_str).unwrap_or("").to_string();
        if name.is_empty() {
            return refuse_shape(
                srv,
                mark,
                k1,
                "has no \"op\" name (each op is flat: {\"op\":\"place\",\"source\":\"src:0\",\"at\":\"0s\"})",
            );
        }
        let before = layout(s);
        let lanes_before = s.lanes();
        let shape_before = shape(s);
        let end_then = parse::session_end(s);
        let mut op = op.clone();
        if let Err(msg) = resolve_title(s, &mut op, fps) {
            return refuse(srv, mark, k1, &name, &msg);
        }
        if name == "lane_move"
            && let Err(msg) = lane_move_check(s, &op)
        {
            return refuse(srv, mark, k1, &name, &msg);
        }
        let cut = ripple_cut(s, &name, &op, fps);
        match run_op(s, &name, &op, fps, project.as_deref(), tones.as_deref(), dry) {
            Ok(mut d) => {
                if matches!(name.as_str(), "title_add" | "title_set")
                    && let Some(t) = title_index_in(&d.text).and_then(|i| s.titles().get(i))
                {
                    d.text.push_str(&format!(" (T@{})", t.start));
                }
                // A placement never ripples, so a clip it "moved" is one it overwrote: the overwrite is named
                // as removed or cut in the op's own text, and pairing the removed clip with its replacement
                // would list a shift that never happened.
                let placing = matches!(name.as_str(), "place" | "append" | "keep" | "assemble" | "generator_add" | "adjustment_add");
                if placing && let Some(g) = placement_gap_note(s, &d.touched, end_then, fps) {
                    gap_notes.push(g);
                }
                shifts.push(if placing {
                    Vec::new()
                } else if name == "lane_move" {
                    lane_move_shifts(&op, &lanes_before, &before, &layout(s), &s.lanes())
                } else {
                    shifted_pairs_cut(&before, &layout(s), cut)
                });
                let shape_after = shape(s);
                effects.push(side_effects(&shape_before, &shape_after, &name));
                holes.push(holes_added(&shape_before, &shape_after, &name));
                done.push((name, d));
            }
            Err(msg) => {
                let msg = stale_address_hint(&shifts, &msg);
                return refuse(srv, mark, k1, &name, &msg);
            }
        }
    }

    let mut delta = pushed_since(s, start);
    // A batch that left the timeline exactly as it was is not an undo step: it is rolled back, and
    // the reply says why (an op that found nothing to do names it).
    let mut noop_why: Option<String> = None;
    if !dry && delta > 0 && state_of(s) == state_before {
        let rolled = undo_n(s, delta);
        put_back_redo(&mut mark, s, &mut srv.redo_ledger);
        drop_batch_sources(s, mark.sources);
        take_back_writes();
        if mark.clean {
            // The rollback leaves the project as it was: a project that was clean reads clean again.
            srv.saved_depth = Some(s.undo_depth());
            srv.saved_pushes = Some(s.history_pushes());
            srv.saved_sources = Some(s.sources().len());
        }
        noop_why = Some(
            done.iter()
                .map(|(_, d)| d.text.clone())
                .find(|t| t.contains("nothing to cut") || t.contains("nothing to copy") || t.contains("already crossfade"))
                .unwrap_or_else(|| "the batch leaves the timeline as it was".to_string())
                + if rolled { "" } else { " (could not roll every step back; check the project)" },
        );
        delta = 0;
    }
    let end_after = parse::session_end(s);
    let mut lanes_touched: Vec<Lane> = Vec::new();
    let mut lines = Vec::with_capacity(done.len());
    let mut applied = Vec::with_capacity(done.len());
    // Holes left by more than one op of the batch are one summary line per lane, under the first op
    // that left one there, in place of a line per op. A single op keeps its own hole lines.
    if holes.iter().filter(|h| !h.is_empty()).count() > 1 {
        for e in effects.iter_mut() {
            e.retain(|x| !x.starts_with("hole left on "));
        }
        for (k, line) in batch_hole_lines(&holes, fps) {
            effects[k].push(line);
        }
    }
    // How often each effect line occurs across the batch: a 200-op batch that leaves the same hole
    // 199 times prints it once, with the count.
    let mut effect_counts: Vec<(&str, usize)> = Vec::new();
    for e in effects.iter().flatten() {
        match effect_counts.iter_mut().find(|(x, _)| x == e) {
            Some(c) => c.1 += 1,
            None => effect_counts.push((e.as_str(), 1)),
        }
    }
    let mut printed_effects: Vec<&str> = Vec::new();
    for (k, (name, d)) in done.iter().enumerate() {
        let show = k < OP_LINES_CAP;
        let addrs: Vec<String> = d.touched.iter().map(|&(l, st)| parse::addr(l, st)).collect();
        let mut line = format!("{}. {}", k + 1, d.text);
        // The clips an op touched read as "→ …" after its text. When the text ends with a list of
        // clips left in place, that arrow would read as more of them, so the touched clips get their
        // own line there.
        let left = d.text.contains("; left in place");
        if !addrs.is_empty() && !left {
            line.push_str(" → ");
            line.push_str(&addrs.join(", "));
        }
        if show {
            lines.push(line);
            if !addrs.is_empty() && left {
                lines.push(format!("   now at: {}", addrs.join(", ")));
            }
        }
        let moved: Vec<String> = shifts[k].iter().map(|(a, b)| format!("{a}→{b}")).collect();
        if !moved.is_empty() && show && name == "lane_move" {
            // A lane move renames each lane's clips: one line per rename, so a swap reads as two lines.
            for pair in moved.iter().take(SHIFT_CAP) {
                lines.push(format!("   shifted: {pair}"));
            }
            if moved.len() > SHIFT_CAP {
                lines.push(format!("   shifted: +{} more", moved.len() - SHIFT_CAP));
            }
        } else if !moved.is_empty() && show {
            let more = if moved.len() > SHIFT_CAP { format!(", +{} more", moved.len() - SHIFT_CAP) } else { String::new() };
            let shown: Vec<&str> = moved.iter().take(SHIFT_CAP).map(String::as_str).collect();
            lines.push(format!("   shifted: {}{more}", shown.join(", ")));
        }
        // An effect printed under the first op that makes it; a repeat is only counted.
        let mut fresh: Vec<String> = Vec::new();
        for e in &effects[k] {
            if printed_effects.contains(&e.as_str()) {
                continue;
            }
            let n = effect_counts.iter().find(|(x, _)| *x == e.as_str()).map_or(1, |c| c.1);
            // The count is how many of the batch's ops make this effect, not a multiplier of it.
            fresh.push(if n > 1 { format!("{e} (in {n} ops)") } else { e.clone() });
            if show {
                printed_effects.push(e.as_str());
            }
        }
        if !fresh.is_empty() && show {
            lines.push(format!("   effects: {}", fresh.join("; ")));
        }
        applied.push(json!({ "op": name, "text": d.text, "touched": addrs, "shifted": moved, "effects": effects[k] }));
        for l in d.lanes.iter().copied().chain(d.touched.iter().map(|&(l, _)| l)) {
            if !lanes_touched.contains(&l) {
                lanes_touched.push(l);
            }
        }
    }
    if done.len() > OP_LINES_CAP {
        lines.push(format!("… and {} more ops", done.len() - OP_LINES_CAP));
    }
    // The digest is the state after the batch, so it is read before any rollback.
    let body = digest(s, &lanes_touched, fps);
    let mut notes: Vec<String> = tail_notes(s, fps).into_iter().filter(|n| !notes_before.contains(n)).collect();
    notes.extend(gap_notes);
    // Something other than the timeline changed (a caption track or a library row was added).
    let outside = s.sources().len() != sources_before || done.iter().any(|(_, d)| d.text.starts_with("imported"));

    // A dry run rolls back only now, after the digest read the state it reports. Then the redo
    // history goes back as it was, and a batch that changed nothing keeps it; only a kept batch lets go.
    let rolled = !dry || undo_n(s, delta);
    if dry || delta == 0 {
        put_back_redo(&mut mark, s, &mut srv.redo_ledger);
    } else {
        keep_redo(&mut mark, s);
    }
    let header = if dry {
        if was_clean {
            srv.saved_pushes = Some(s.history_pushes());
            srv.saved_sources = Some(s.sources().len());
        }
        let note = if rolled { "" } else { " (could not roll every step back; check the project)" };
        format!("dry run — would apply {} op(s); nothing changed{note}", done.len())
    } else if delta > 0 {
        let before = srv.ledger.len();
        let named: Vec<Vec<String>> = done.iter().map(|(_, d)| d.touched.iter().map(|&(l, st)| parse::addr(l, st)).collect()).collect();
        srv.ledger.push(Entry::new(delta, label.clone().unwrap_or_else(|| summarize(ops, &named))));
        srv.history_lost_on_open = false;
        // The step number is the batch's place in the history that remains: at the cap the oldest
        // step is dropped to make room, so the number stays at the cap and says so.
        srv.trim_ledger();
        let dropped = srv.ledger.len() <= before;
        let oldest = if dropped { " (oldest step dropped)" } else { "" };
        format!("applied {} op(s) as undo step {}{oldest}", done.len(), srv.ledger.len())
    } else if outside {
        format!("applied {} op(s); the timeline is unchanged", done.len())
    } else {
        let nothing_to_cut = done.iter().map(|(_, d)| d.text.clone()).find(|t| t.contains("nothing to cut") || t.contains("nothing to copy") || t.contains("already crossfade"));
        let why = noop_why
            .or(nothing_to_cut)
            .unwrap_or_else(|| format!("{} op(s) ran and left the timeline as it was", done.len()));
        format!("no change: {why}")
    };
    // A dry run at the cap evicted the oldest steps; a kept batch may have too.
    srv.trim_ledger();
    let header = match &label {
        Some(l) => format!("[{l}] {header}"),
        None => header,
    };
    let mut text = header;
    for l in &lines {
        text.push('\n');
        text.push_str(l);
    }
    if !body.is_empty() {
        text.push('\n');
        text.push_str(&body);
    }
    for note in &notes {
        text.push_str(&format!("\nnote: {note}"));
    }
    if end_after != end_before {
        text.push_str(&format!(
            "\ntimeline {:.3}s → {:.3}s",
            parse::secs(end_before, fps),
            parse::secs(end_after, fps)
        ));
    }
    Reply::ok(text).with(json!({ "applied": applied, "undo_steps": srv.ledger.len() }))
}

/// `T@<start>` (a title's frame, like a clip's address) names the title that starts there, so
/// the op can name it as `Tn`. Other title forms pass through for the title op to check.
fn resolve_title(s: &PlaybackSession, op: &mut Value, fps: f64) -> Result<(), String> {
    let Some(raw) = op.get("title").and_then(Value::as_str).map(|t| t.trim().to_string()) else {
        return Ok(());
    };
    let Some(rest) = raw.strip_prefix("T@").or_else(|| raw.strip_prefix("t@")) else {
        return Ok(());
    };
    // Bare digits name a start frame, as `T@1200` and `T@1200f` do in the reply; anything else
    // (a timecode, seconds with a unit) goes through the usual time parser.
    let digits = rest.trim().trim_end_matches(['f', 'F']);
    let start = match digits.parse::<u32>() {
        Ok(f) => f,
        Err(_) => parse::frame(&Value::String(rest.trim().to_string()), fps, parse::session_end(s))
            .map_err(|e| format!("title {raw}: {e}"))?,
    };
    match s.titles().iter().position(|t| t.start == start) {
        Some(i) => {
            op["title"] = json!(format!("T{}", i + 1));
            Ok(())
        }
        None => {
            let starts: Vec<String> = s.titles().iter().map(|t| format!("T@{}", t.start)).collect();
            Err(format!("title {raw} is not on this timeline; the titles are {}", starts.join(", ")))
        }
    }
}

/// The title number (0-based) a title op's reply names first, as `T2` gives 1.
fn title_index_in(text: &str) -> Option<usize> {
    let tok = text.split(|c: char| !c.is_ascii_alphanumeric()).find(|t| t.len() > 1 && t.starts_with('T') && t[1..].chars().all(|c| c.is_ascii_digit()))?;
    tok[1..].parse::<usize>().ok()?.checked_sub(1)
}

// ---- chapters ---------------------------------------------------------------

/// The chapter ops. A chapter is named `C@<frame>` (the frame it starts on, bare digits are frames
/// as `T@` is) or `C#n` (its place in the frame order, 1 first). Each reply names the chapters after
/// the op, so the next op can use the new addresses.
fn op_chapter(s: &mut PlaybackSession, name: &str, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    match name {
        "chapter_add" => {
            let at = op.get("at").ok_or("chapter_add needs `at`: the frame or time the chapter starts on")?;
            let frame = parse::frame(at, fps, end).map_err(|e| format!("chapter_add: {e}"))?;
            let title = chapter_text(op)?.ok_or("chapter_add needs `title`: the chapter's name")?;
            s.add_chapter(frame, &title).map_err(|e| format!("chapter_add: {e}"))?;
            let text = format!(
                "chapter_add \"{title}\" at {frame}f ({:.3}s) → C@{frame}; chapters: {}",
                parse::secs(frame, fps),
                chapter_names(s, fps)
            );
            Ok(plain(text, Vec::new()))
        }
        "chapter_set" => {
            let mut idx = chapter_index(s, op.get("chapter"), fps)?;
            let old = s.chapters()[idx].clone();
            let title = chapter_text(op)?;
            let at = match op.get("at") {
                None | Some(Value::Null) => None,
                Some(v) => Some(parse::frame(v, fps, end).map_err(|e| format!("chapter_set: {e}"))?),
            };
            if title.is_none() && at.is_none() {
                return Err("chapter_set changes nothing: give `title` and/or `at`".into());
            }
            if let Some(t) = &title
                && *t != old.title
                && !s.set_chapter_title(idx, t)
            {
                return Err(format!("chapter_set: the name {t:?} is refused: a name is 1 to 100 characters"));
            }
            if let Some(frame) = at
                && frame != old.frame
            {
                idx = s.move_chapter(idx, frame).map_err(|e| format!("chapter_set: {e}"))?;
            }
            let now = s.chapters()[idx].clone();
            let text = format!(
                "chapter_set C@{} → C@{} \"{}\"; chapters: {}",
                old.frame,
                now.frame,
                now.title,
                chapter_names(s, fps)
            );
            Ok(plain(text, Vec::new()))
        }
        _ => {
            let idx = chapter_index(s, op.get("chapter"), fps)?;
            let gone = s.chapters()[idx].clone();
            s.remove_chapter(idx);
            let text = format!("chapter_remove C@{} \"{}\"; chapters: {}", gone.frame, gone.title, chapter_names(s, fps));
            Ok(plain(text, Vec::new()))
        }
    }
}

/// The name a chapter op gives: `title` as text, trimmed. `None` when the op does not name one.
fn chapter_text(op: &Value) -> Result<Option<String>, String> {
    match op.get("title") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) => Ok(Some(t.trim().to_string())),
        Some(_) => Err("`title` must be the chapter's name, as text".into()),
    }
}

/// The index of the chapter `raw` names: `C@<frame>` (bare digits are frames, `f` is allowed, and a
/// time works too) or `C#n` (the n-th chapter, 1 first).
fn chapter_index(s: &PlaybackSession, raw: Option<&Value>, fps: f64) -> Result<usize, String> {
    let Some(raw) = raw.and_then(Value::as_str).map(|t| t.trim().to_string()) else {
        return Err("name the chapter as C@<frame> (C@150) or C#n (C#2, its place in the list)".into());
    };
    let count = s.chapters().len();
    if let Some(rest) = raw.strip_prefix("C#").or_else(|| raw.strip_prefix("c#")) {
        let n: usize = rest.trim().parse().map_err(|_| format!("chapter {raw}: C#n wants a number, 1 for the first chapter"))?;
        return match n.checked_sub(1) {
            Some(i) if i < count => Ok(i),
            _ => Err(format!("chapter {raw} is not on this timeline; the chapters are {}", chapter_names(s, fps))),
        };
    }
    if let Some(rest) = raw.strip_prefix("C@").or_else(|| raw.strip_prefix("c@")) {
        let digits = rest.trim().trim_end_matches(['f', 'F']);
        let frame = match digits.parse::<u32>() {
            Ok(f) => f,
            Err(_) => parse::frame(&Value::String(rest.trim().to_string()), fps, parse::session_end(s))
                .map_err(|e| format!("chapter {raw}: {e}"))?,
        };
        return s
            .chapters()
            .iter()
            .position(|c| c.frame == frame)
            .ok_or_else(|| format!("chapter {raw} is not on this timeline; the chapters are {}", chapter_names(s, fps)));
    }
    Err(format!("chapter {raw}: name a chapter as C@<frame> (C@150) or C#n (C#2)"))
}

/// The chapters as addresses and names, `C@0 Intro, C@4500 Setup`, or `none`.
fn chapter_names(s: &PlaybackSession, fps: f64) -> String {
    let _ = fps;
    if s.chapters().is_empty() {
        return "none".to_string();
    }
    s.chapters().iter().map(|c| format!("C@{} {}", c.frame, c.title)).collect::<Vec<_>>().join(", ")
}

// ---- markers ----------------------------------------------------------------

/// The marker ops. A marker is named `M@<frame>` (the first one on that frame) or `M#n` (its place
/// in the frame order, 1 first). Each reply names the markers after the op, so the next op can use
/// the new addresses.
fn op_marker(s: &mut PlaybackSession, name: &str, op: &Value, fps: f64, end: u32) -> Result<Done, String> {
    use engine::project::{Marker, MarkerColor};
    let text_arg = |key: &str| -> Result<Option<String>, String> {
        match op.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(t)) => Ok(Some(t.trim().to_string())),
            Some(_) => Err(format!("`{key}` must be text")),
        }
    };
    let color_arg = || -> Result<Option<MarkerColor>, String> {
        match text_arg("color")? {
            None => Ok(None),
            Some(c) => MarkerColor::from_name(&c)
                .map(Some)
                .ok_or_else(|| format!("color {c:?} is not red, orange, yellow, green, blue or purple")),
        }
    };
    let length_arg = || -> Result<Option<u32>, String> {
        match op.get("length") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => parse::frame(v, fps, u32::MAX).map(Some).map_err(|e| format!("length: {e}")),
        }
    };
    match name {
        "marker_add" => {
            let at = op.get("at").ok_or("marker_add needs `at`: the frame or time the marker sits on")?;
            let frame = parse::frame(at, fps, end).map_err(|e| format!("marker_add: {e}"))?;
            let name = text_arg("name")?.ok_or("marker_add needs `name`")?;
            let marker = Marker {
                frame,
                length: length_arg()?.unwrap_or(0),
                note: text_arg("note")?.unwrap_or_default(),
                color: color_arg()?.unwrap_or_default(),
                name,
            };
            s.add_marker(marker.clone()).map_err(|e| format!("marker_add: {e}"))?;
            let text = format!(
                "marker_add \"{}\" at {frame}f ({:.3}s) → M@{frame}; markers: {}",
                marker.name,
                parse::secs(frame, fps),
                marker_names(s)
            );
            Ok(plain(text, Vec::new()))
        }
        "marker_set" => {
            let idx = marker_index(s, op.get("marker"), fps)?;
            let old = s.markers()[idx].clone();
            let mut new = old.clone();
            if let Some(n) = text_arg("name")? {
                new.name = n;
            }
            if let Some(n) = text_arg("note")? {
                new.note = n;
            }
            if let Some(c) = color_arg()? {
                new.color = c;
            }
            if let Some(v) = op.get("at").filter(|v| !v.is_null()) {
                new.frame = parse::frame(v, fps, end).map_err(|e| format!("marker_set: {e}"))?;
            }
            if let Some(l) = length_arg()? {
                new.length = l;
            }
            let now_idx = s.set_marker(idx, new.clone()).map_err(|e| format!("marker_set: {e}"))?;
            let _ = now_idx;
            let text = format!("marker_set M@{} → M@{} \"{}\"; markers: {}", old.frame, new.frame, new.name, marker_names(s));
            Ok(plain(text, Vec::new()))
        }
        _ => {
            let idx = marker_index(s, op.get("marker"), fps)?;
            let gone = s.markers()[idx].clone();
            s.remove_marker(idx);
            let text = format!("marker_remove M@{} \"{}\"; markers: {}", gone.frame, gone.name, marker_names(s));
            Ok(plain(text, Vec::new()))
        }
    }
}

/// The index of the marker `raw` names: `M@<frame>` (the first marker on it; bare digits are frames,
/// `f` is allowed, and a time works too) or `M#n` (the n-th marker, 1 first).
fn marker_index(s: &PlaybackSession, raw: Option<&Value>, fps: f64) -> Result<usize, String> {
    let Some(raw) = raw.and_then(Value::as_str).map(|t| t.trim().to_string()) else {
        return Err("name the marker as M@<frame> (M@150) or M#n (M#2, its place in the list)".into());
    };
    let count = s.markers().len();
    if let Some(rest) = raw.strip_prefix("M#").or_else(|| raw.strip_prefix("m#")) {
        let n: usize = rest.trim().parse().map_err(|_| format!("marker {raw}: M#n wants a number, 1 for the first marker"))?;
        return match n.checked_sub(1) {
            Some(i) if i < count => Ok(i),
            _ => Err(format!("marker {raw} is not on this timeline; the markers are {}", marker_names(s))),
        };
    }
    if let Some(rest) = raw.strip_prefix("M@").or_else(|| raw.strip_prefix("m@")) {
        let digits = rest.trim().trim_end_matches(['f', 'F']);
        let frame = match digits.parse::<u32>() {
            Ok(f) => f,
            Err(_) => parse::frame(&Value::String(rest.trim().to_string()), fps, parse::session_end(s))
                .map_err(|e| format!("marker {raw}: {e}"))?,
        };
        return s
            .markers()
            .iter()
            .position(|m| m.frame == frame)
            .ok_or_else(|| format!("marker {raw} is not on this timeline; the markers are {}", marker_names(s)));
    }
    Err(format!("marker {raw}: name a marker as M@<frame> (M@150) or M#n (M#2)"))
}

/// The markers as addresses and names, `M@0 Intro, M@4500 Fix`, or `none`.
fn marker_names(s: &PlaybackSession) -> String {
    if s.markers().is_empty() {
        return "none".to_string();
    }
    s.markers().iter().map(|m| format!("M@{} {}", m.frame, m.name)).collect::<Vec<_>>().join(", ")
}

/// `steps` for undo and redo: a whole number of at least 1, and 1 when absent.
/// Anything else (zero, negative, fractional, text) is refused, the same for both.
fn steps_arg(args: &Value) -> Result<usize, Reply> {
    match args.get("steps") {
        None | Some(Value::Null) => Ok(1),
        Some(v) => match v.as_u64() {
            Some(n) if n >= 1 => Ok(n as usize),
            _ => Err(Reply::err(format!("`steps` must be a whole number of at least 1, not {v}"))),
        },
    }
}

pub(super) fn run_undo(srv: &mut Server, args: &Value) -> Reply {
    let want = tri!(steps_arg(args));
    if srv.session.is_none() {
        return Reply::err(NO_PROJECT_UNDO);
    }
    srv.trim_ledger();
    if srv.ledger.is_empty() {
        const CHECKPOINTS: &str = "checkpoints survive: checkpoint {name} before risky edits, restore {name} to go back";
        // Undone batches wait on the redo ledger: that is the history there is, not one that was lost.
        if !srv.redo_ledger.is_empty() {
            let capped = if srv.history_capped { "; older history was dropped by the 100-step cap" } else { "" };
            return Reply::err(format!(
                "nothing left to undo ({} steps undone; redo brings them back){capped}",
                srv.redo_ledger.len()
            ));
        }
        return Reply::err(if srv.history_capped {
            format!("nothing to undo: older history was dropped by the 100-step cap; {CHECKPOINTS}")
        } else {
            let lost = if srv.history_lost_on_open { " (history does not survive project_open)" } else { "" };
            format!("nothing to undo yet{lost}; {CHECKPOINTS}")
        });
    }
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let mut names: Vec<String> = Vec::new();
    while names.len() < want {
        let Some(entry) = srv.ledger.last().cloned() else { break };
        if !undo_n(s, entry.steps) {
            return Reply::err(format!(
                "undo stopped after {} batch(es): the engine would not undo the next batch in full; check the project before going on",
                names.len()
            ));
        }
        srv.ledger.pop();
        names.push(entry.label.clone());
        srv.redo_ledger.push(entry);
    }
    let fps = parse::session_fps(s);
    let lanes = s.lanes();
    let body = digest(s, &lanes, fps);
    let next = srv.ledger.last().map(|e| format!(" · next undo: {}", e.label)).unwrap_or_default();
    let dropped = if names.len() < want && srv.ledger.is_empty() && srv.history_capped {
        " · older history was dropped by the 100-step cap"
    } else {
        ""
    };
    let short = if names.len() < want {
        format!(" · asked for {want}, only {} can be undone", batches(names.len()))
    } else {
        String::new()
    };
    let text = format!(
        "undid: {} · undo {} · redo {}{short}{next}{dropped}\n{body}",
        names.join(" · "),
        srv.ledger.len(),
        srv.redo_ledger.len()
    );
    Reply::ok(text).with(json!({ "undone": names, "undo_steps": srv.ledger.len(), "redo_steps": srv.redo_ledger.len() }))
}

fn run_redo(srv: &mut Server, args: &Value) -> Reply {
    let want = tri!(steps_arg(args));
    if srv.session.is_none() {
        return Reply::err(NO_PROJECT_REDO);
    }
    if srv.redo_ledger.is_empty() {
        return Reply::err("nothing to redo (an edit clears redo history)");
    }
    let Some(s) = srv.session.as_mut() else {
        return Reply::err(NO_PROJECT);
    };
    let mut names: Vec<String> = Vec::new();
    while names.len() < want {
        let Some(entry) = srv.redo_ledger.last().cloned() else { break };
        if !redo_n(s, entry.steps) {
            return Reply::err(format!(
                "redo stopped after {} batch(es): the engine would not redo the next batch in full; check the project before going on",
                names.len()
            ));
        }
        srv.redo_ledger.pop();
        names.push(entry.label.clone());
        srv.ledger.push(entry);
        srv.history_lost_on_open = false;
    }
    let fps = parse::session_fps(s);
    let lanes = s.lanes();
    let body = digest(s, &lanes, fps);
    let next = srv.redo_ledger.last().map(|e| format!(" · next redo: {}", e.label)).unwrap_or_default();
    let short = if names.len() < want {
        format!(" · asked for {want}, only {} can be redone", batches(names.len()))
    } else {
        String::new()
    };
    let text = format!(
        "redid: {} · undo {} · redo {}{short}{next}\n{body}",
        names.join(" · "),
        srv.ledger.len(),
        srv.redo_ledger.len()
    );
    Reply::ok(text).with(json!({ "redone": names, "undo_steps": srv.ledger.len(), "redo_steps": srv.redo_ledger.len() }))
}

// ---- checkpoints ----------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// A test's own checkpoint folder, set by `own_checkpoints`, so no two tests share one.
    static TEST_CHECKPOINTS: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where the checkpoints of the open project are kept: `<project dir>/checkpoints/<stem>/`,
/// beside the project, so they outlive the server process (as freeze stills do). With no
/// project path yet, the temp-dir fallback of `temp_checkpoint_dir`.
fn checkpoint_dir(srv: &Server) -> PathBuf {
    match srv.path.as_deref() {
        Some(project) => engine::versions::dir_beside(project),
        None => temp_checkpoint_dir(),
    }
}

/// The fallback with no project path: a directory per process under the temp dir, so
/// two servers never read each other's copies. Lost on reboot or a temp cleanup.
fn temp_checkpoint_dir() -> PathBuf {
    #[cfg(test)]
    {
        if let Some(dir) = TEST_CHECKPOINTS.with(|d| d.borrow().clone()) {
            return dir;
        }
    }
    std::env::temp_dir().join(format!("edith_mcp-{}", std::process::id())).join("checkpoints")
}

fn checkpoint_file(srv: &Server, name: &str) -> PathBuf {
    engine::versions::file(&checkpoint_dir(srv), name)
}

/// A checkpoint name: 1 to 40 letters, digits, `-` or `_`, so it is a safe file name
/// ([`engine::versions::valid_name`], which the window's Save version uses too).
fn checkpoint_name(args: &Value) -> Result<String, Reply> {
    let name = arg_str(args, "name")?;
    if !engine::versions::valid_name(name) {
        return Err(Reply::err(format!(
            "checkpoint name `{name}` is not usable: 1 to 40 letters, digits, - or _"
        )));
    }
    Ok(name.to_string())
}

fn run_checkpoint(srv: &mut Server, args: &Value) -> Reply {
    let name = tri!(checkpoint_name(args));
    let dir = checkpoint_dir(srv);
    let Some(s) = srv.session.as_ref() else {
        return Reply::err(NO_PROJECT);
    };
    // An existing checkpoint of this name is overwritten; the reply says so and how old it was.
    let saved = match engine::versions::save(s, &dir, &name) {
        Ok(saved) => saved,
        Err(e) => return Reply::err(format!("checkpoint {name} failed: {e}")),
    };
    let fps = parse::session_fps(s);
    let head = match saved.replaced {
        Some(t) => format!("replaced checkpoint {name} (was from {})", age_text(t)),
        None => format!("checkpoint {name} saved"),
    };
    // Without a project path the copy is in the temp dir; say so, since it will not survive a restart.
    let kept = if srv.path.is_some() {
        format!("kept in {}", dir.display())
    } else {
        "kept in the temp directory only (no project path yet: save the project to keep checkpoints beside it)".to_string()
    };
    Reply::ok(format!(
        "{head}: {} placement(s), {}, {kept}. restore {name} returns to this.",
        saved.clips,
        parse::tc(parse::session_end(s), fps)
    ))
}

/// How long ago a file was written, as the checkpoint reply words it: `40 s ago`, `3 min ago`.
fn age_text(t: std::time::SystemTime) -> String {
    engine::versions::age_text(t)
}

/// Saves the open state as a checkpoint named `before-restore-<name>` (a suffix when that name is
/// taken, and cut to the 40 characters a checkpoint name may hold), so a restore never discards
/// unsaved edits. Returns the name it saved under; None when no project is open.
fn save_before_restore(srv: &Server, name: &str) -> Result<Option<String>, String> {
    let Some(s) = srv.session.as_ref() else {
        return Ok(None);
    };
    engine::versions::save_before_restore(s, &checkpoint_dir(srv), name).map(Some).map_err(|e| e.to_string())
}

fn run_restore(srv: &mut Server, args: &Value) -> Reply {
    let name = tri!(checkpoint_name(args));
    let path = checkpoint_file(srv, &name);
    if !path.is_file() {
        return Reply::err(format!(
            "no checkpoint named {name} in {}; checkpoints lists the ones there",
            path.parent().unwrap_or(&path).display()
        ));
    }
    // The open state is kept first, so the restore can be undone by restoring it back.
    let saved = match save_before_restore(srv, &name) {
        Ok(saved) => saved,
        Err(e) => return Reply::err(format!("restore {name} refused: could not save the open state first ({e}); nothing was restored")),
    };
    let session = match PlaybackSession::open_project(&path) {
        Ok(s) => s,
        Err(e) => return Reply::err(format!("restore {name} failed: {e}")),
    };
    // What the checkpoint needed fixing for, said as project_open says it (the same wording).
    let fixes: String = session
        .load_warnings()
        .iter()
        .map(|w| format!("warning: the file needed fixing to open: {w} — save to keep the fix\n"))
        .collect();
    // The file the project is saved to is not touched. Its mtime and path stay,
    // and `saved_depth` is cleared so the restored state reads as unsaved.
    srv.session = Some(session);
    srv.saved_depth = None;
    srv.saved_pushes = None;
    srv.history_lost_on_open = false;
    srv.ledger.clear();
    srv.redo_ledger.clear();
    srv.history_capped = false;
    let s = srv.session.as_ref().expect("session was just installed");
    let fps = parse::session_fps(s);
    let lanes = s.lanes();
    let body = digest(s, &lanes, fps);
    let kept = match saved {
        Some(auto) => format!(" (the state before the restore is kept as checkpoint {auto}: restore {auto} goes back to it)"),
        None => String::new(),
    };
    Reply::ok(format!(
        "restored checkpoint {name}; undo and redo history cleared, project is unsaved{kept}\n{fixes}{body}"
    ))
}

/// Lists the checkpoint files on disk in this project's checkpoint folder, by name.
fn run_checkpoints(srv: &mut Server, _args: &Value) -> Reply {
    let dir = checkpoint_dir(srv);
    let mut names: Vec<String> = engine::versions::list(&dir).into_iter().map(|v| v.name).collect();
    names.sort();
    if names.is_empty() {
        return Reply::ok(format!("no checkpoints in {}; `checkpoint {{name}}` saves one", dir.display()));
    }
    Reply::ok(format!("{} checkpoint(s) in {}:\n{}", names.len(), dir.display(), names.join("\n")))
        .with(json!({ "checkpoints": names, "dir": dir.display().to_string() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::path::Path;

    fn open() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let sess = PlaybackSession::open(p).expect("open test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    type Snap = Vec<(Lane, Vec<(u32, u32, u32, usize, Option<u32>)>)>;

    fn snapshot(srv: &Server) -> Snap {
        let s = srv.session.as_ref().expect("session");
        s.lanes()
            .into_iter()
            .filter(|l| l.kind != LaneKind::Subtitle)
            .map(|l| {
                let clips = s
                    .lane_clips(l)
                    .iter()
                    .map(|c| (c.start, c.in_frame, c.out_frame, c.source, c.link))
                    .collect();
                (l, clips)
            })
            .collect()
    }

    fn first_len(srv: &Server) -> u32 {
        srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames()
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// `beats` on a click track at 120 BPM (a 20 ms burst every half second, ten seconds): the tempo
    /// is read back, the beats are yellow "Beat" markers at `every`, and a second run replaces the
    /// first instead of doubling it.
    #[test]
    fn beats_marks_a_click_track_and_a_second_run_replaces_it() {
        let dir = crate::testutil::TestDir::new("beats-op");
        let wav = dir.join("click120.wav");
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("aevalsrc='0.8*sin(2*PI*1000*t)*lt(mod(t,0.5),0.02)':d=10:s=44100:c=mono")
            .args(["-c:a", "pcm_s16le"])
            .arg(&wav)
            .status()
            .expect("ffmpeg runs");
        assert!(status.success(), "ffmpeg made the click track");
        let mut srv = open();
        srv.session.as_mut().unwrap().import(&wav).expect("import click track");
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "A2", "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "beats", "clip": "A2@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("BPM") && r.text.contains("M@"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let beat_count = |s: &PlaybackSession| {
            s.markers().iter().filter(|m| m.note == engine::beats::BEAT_NOTE).count()
        };
        let first = beat_count(s);
        assert!((18..=21).contains(&first), "{first} beat markers: {}", r.text);
        assert!(s.markers().iter().filter(|m| m.note == "Beat").all(|m| m.color == engine::beats::BEAT_COLOR));
        // The reply reads "beats A2@0: 120.0 BPM; ...".
        let bpm: f64 = r.text.split(": ").nth(1).and_then(|t| t.split(" BPM").next()).and_then(|t| t.trim().parse().ok()).unwrap_or(0.);
        assert!((bpm - 120.).abs() <= 2., "bpm in the reply: {}", r.text);

        // Every 2nd beat, replacing the first run's markers: about half as many, none left over.
        let r = ed(&mut srv, json!([{ "op": "beats", "clip": "A2@0", "every": 2 }]));
        assert!(!r.is_error && r.text.contains("replacing"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let second = beat_count(s);
        assert!((9..=11).contains(&second), "{second} after every 2: {}", r.text);

        let bad = ed(&mut srv, json!([{ "op": "beats", "clip": "A2@0", "every": 0 }]));
        assert!(bad.is_error && bad.text.contains("every"), "{}", bad.text);
    }

    #[test]
    fn chapter_ops_add_set_remove_and_name_the_new_addresses() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "chapter_add", "at": "0s", "title": "Intro" }]));
        assert!(!r.is_error && r.text.contains("C@0"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "chapter_add", "at": "150f", "title": "Setup" }]));
        assert!(!r.is_error && r.text.contains("C@150") && r.text.contains("C@0 Intro, C@150 Setup"), "{}", r.text);
        let dup = ed(&mut srv, json!([{ "op": "chapter_add", "at": "150f", "title": "Again" }]));
        assert!(dup.is_error && dup.text.contains("already starts at frame 150"), "{}", dup.text);
        let r = ed(&mut srv, json!([{ "op": "chapter_set", "chapter": "C#2", "title": "Begin", "at": "160f" }]));
        assert!(!r.is_error && r.text.contains("C@150 → C@160") && r.text.contains("\"Begin\""), "{}", r.text);
        let clash = ed(&mut srv, json!([{ "op": "chapter_set", "chapter": "C@0", "at": "160f" }]));
        assert!(clash.is_error, "{}", clash.text);
        let missing = ed(&mut srv, json!([{ "op": "chapter_set", "chapter": "C@999", "title": "X" }]));
        assert!(missing.is_error && missing.text.contains("not on this timeline"), "{}", missing.text);
        let nothing = ed(&mut srv, json!([{ "op": "chapter_set", "chapter": "C#1" }]));
        assert!(nothing.is_error && nothing.text.contains("changes nothing"), "{}", nothing.text);
        let r = ed(&mut srv, json!([{ "op": "chapter_remove", "chapter": "C@0" }]));
        assert!(!r.is_error && r.text.contains("chapters: C@160 Begin"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().chapters().len(), 1);
    }

    #[test]
    fn marker_ops_add_set_remove_and_follow_a_ripple() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "marker_add", "at": "0s", "name": "Intro", "color": "red", "note": "hi" }]));
        assert!(!r.is_error && r.text.contains("M@0"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "marker_add", "at": "90f", "name": "Range", "length": "30f", "color": "purple" }]));
        assert!(!r.is_error, "{}", r.text);
        let bad = ed(&mut srv, json!([{ "op": "marker_add", "at": "5f", "name": "X", "color": "pink" }]));
        assert!(bad.is_error && bad.text.contains("not red, orange"), "{}", bad.text);
        let no = ed(&mut srv, json!([{ "op": "marker_add", "at": "5f" }]));
        assert!(no.is_error, "{}", no.text);
        let r = ed(&mut srv, json!([{ "op": "marker_set", "marker": "M#2", "name": "Fix", "color": "green", "at": "100f", "length": 0 }]));
        assert!(!r.is_error && r.text.contains("M@100 Fix"), "{}", r.text);
        let m = srv.session.as_ref().unwrap().markers().to_vec();
        assert_eq!((m[1].frame, m[1].length, m[1].color.name()), (100, 0, "green"));
        assert_eq!(m[0].note, "hi");
        let r = ed(&mut srv, json!([{ "op": "marker_set", "marker": "M@0", "note": "" }]));
        assert!(!r.is_error && srv.session.as_ref().unwrap().markers()[0].note.is_empty(), "{}", r.text);
        let nothing = ed(&mut srv, json!([{ "op": "marker_set", "marker": "M@0" }]));
        assert!(nothing.is_error, "{}", nothing.text);
        let missing = ed(&mut srv, json!([{ "op": "marker_remove", "marker": "M@999" }]));
        assert!(missing.is_error && missing.text.contains("markers are"), "{}", missing.text);
        // Inserting 30 frames at 50 pushes the marker at 100 on; the one at 0 stays.
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "50f", "duration": "30f" }]));
        assert!(!r.is_error, "{}", r.text);
        let frames: Vec<u32> = srv.session.as_ref().unwrap().markers().iter().map(|m| m.frame).collect();
        assert_eq!(frames, vec![0, 130]);
        let r = ed(&mut srv, json!([{ "op": "marker_remove", "marker": "M@130" }]));
        assert!(!r.is_error && r.text.contains("markers: M@0 Intro"), "{}", r.text);
    }

    #[test]
    fn chapters_move_with_a_cut_and_a_chapter_inside_lands_at_its_start() {
        let mut srv = open();
        ed(&mut srv, json!([{ "op": "chapter_add", "at": "0s", "title": "Intro" }]));
        ed(&mut srv, json!([{ "op": "chapter_add", "at": "45f", "title": "Inside" }]));
        ed(&mut srv, json!([{ "op": "chapter_add", "at": "90f", "title": "Late" }]));
        // A 30-frame span from frame 30: the chapter at 45 lands on its start, the one at 90 moves back 30.
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "30f", "duration": "30f" }]));
        assert!(!r.is_error, "{}", r.text);
        let frames: Vec<(u32, String)> =
            srv.session.as_ref().unwrap().chapters().iter().map(|c| (c.frame, c.title.clone())).collect();
        assert_eq!(frames, [(0, "Intro".into()), (30, "Inside".into()), (60, "Late".into())], "{:?}", frames);
        // Inserting 30 frames at 30 pushes the later ones on, and the chapter at 30 moves with it.
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "30f", "duration": "30f" }]));
        assert!(!r.is_error, "{}", r.text);
        let frames: Vec<u32> = srv.session.as_ref().unwrap().chapters().iter().map(|c| c.frame).collect();
        assert_eq!(frames, [0, 60, 90]);
    }

    #[test]
    fn split_move_fade_is_one_undo_step() {
        let mut srv = open();
        let len = first_len(&srv);
        let half = len / 2;
        let far = len + 60;
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": format!("{half}f") },
                { "op": "move", "clip": format!("V1@{half}"), "at": format!("{far}f") },
                { "op": "fade", "clip": format!("V1@{far}"), "out": "5f" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("applied 3 op(s) as undo step 1"), "{}", r.text);
        assert_eq!(srv.ledger.len(), 1);
        assert!(srv.redo_ledger.is_empty());
    }

    #[test]
    fn op_shape_refusals_name_the_fault() {
        let mut srv = open();
        let before = snapshot(&srv);
        let not_object = ed(&mut srv, json!([{ "op": "split", "at": "1s" }, null]));
        assert!(not_object.is_error);
        assert!(not_object.text.starts_with("op 2 is not an op object: give {\"op\":\"<name>\""), "{}", not_object.text);
        assert!(!not_object.text.contains("(?)"), "{}", not_object.text);
        assert!(not_object.text.contains("batch rolled back, nothing changed"), "{}", not_object.text);
        for bad in [json!([{}]), json!([{ "op": null }]), json!([{ "op": 7 }])] {
            let r = ed(&mut srv, bad);
            assert!(r.is_error);
            assert_eq!(
                r.text.split(';').next().unwrap(),
                "op 1 has no \"op\" name (each op is flat: {\"op\":\"place\",\"source\":\"src:0\",\"at\":\"0s\"})",
                "{}",
                r.text
            );
        }
        assert_eq!(snapshot(&srv), before, "a refused shape changes nothing");
    }

    #[test]
    fn undo_step_number_stops_at_the_cap() {
        let mut srv = open();
        let mut last = String::new();
        for i in 0..101 {
            let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "V1", "hidden": i % 2 == 0 }]));
            assert!(!r.is_error, "{}", r.text);
            last = r.text;
        }
        assert_eq!(srv.ledger.len(), 100, "the history keeps 100 steps");
        assert!(last.contains("as undo step 100 (oldest step dropped)"), "{last}");
    }

    #[test]
    fn long_batch_prints_capped_op_lines_and_counts_repeated_effects() {
        let mut srv = open();
        let ops: Vec<Value> = (0..45).map(|i| json!({ "op": "lane_flags", "lane": "V1", "hidden": i % 2 == 0 })).collect();
        let r = ed(&mut srv, Value::Array(ops));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("\n40. "), "{}", r.text);
        assert!(!r.text.contains("\n41. "), "{}", r.text);
        assert!(r.text.contains("… and 5 more ops"), "{}", r.text);

        // Four identical place-and-lift pairs leave the same hole four times: the batch names it once,
        // as a summary line for the lane, with the one gap start it has.
        let mut srv = open();
        let pair = [
            json!({ "op": "place", "source": 0, "lane": "V1", "at": "60s", "audio": false }),
            json!({ "op": "lift", "clip": "V1@60s" }),
        ];
        let ops: Vec<Value> = (0..4).flat_map(|_| pair.clone()).collect();
        let r = ed(&mut srv, Value::Array(ops));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.text.matches("holes left on V1").count(), 1, "{}", r.text);
        assert!(r.text.contains("holes left on V1 at 5.000s (close_gaps cannot close them: the V1 clip at V1@0 is one take with the A1 clip at A1@0"), "{}", r.text);
    }

    #[test]
    fn failing_second_op_rolls_the_batch_back() {
        let mut srv = open();
        let before = snapshot(&srv);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let half = first_len(&srv) / 2;
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": format!("{half}f") },
                { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }
            ]),
        );
        assert!(r.is_error);
        assert!(r.text.contains("op 2 (trim) failed"), "{}", r.text);
        assert!(r.text.contains("batch rolled back, nothing changed"), "{}", r.text);
        assert_eq!(snapshot(&srv), before);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
        assert!(srv.ledger.is_empty());
    }

    /// Fills the engine's history to the 100-step cap with single-op batches,
    /// each one split at its own frame, so each pushes exactly one snapshot.
    fn at_the_cap() -> Server {
        let mut srv = open();
        assert!(first_len(&srv) > 120, "the test asset is long enough for 115 splits");
        for f in 1..=105 {
            let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{f}f") }]));
            assert!(!r.is_error, "split {f}: {}", r.text);
        }
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.undo_depth(), 100, "the cap holds the depth at 100");
        assert_eq!(srv.ledger.len(), 100);
        srv
    }

    #[test]
    fn two_op_batch_at_the_cap_is_one_step_and_undo_restores_it() {
        let mut srv = at_the_cap();
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "split", "at": "110f" }, { "op": "split", "at": "115f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("as undo step 99 (oldest step dropped)"), "the batch counted both steps: {}", r.text);
        assert_eq!(srv.ledger.last().unwrap().steps, 2, "exact at the cap, not a zero delta");
        assert_eq!(srv.ledger.len(), 99, "the two oldest single-op entries were evicted with their steps");
        assert_ne!(snapshot(&srv), before);

        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(snapshot(&srv), before, "undo restores exactly the state before the batch");
        assert_eq!(srv.ledger.len(), 98);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.undo_depth(), 98, "the engine and the ledger agree");
        assert_eq!(srv.ledger.iter().map(|e| e.steps).sum::<usize>(), 98);
        assert_eq!(s.redo_depth(), 2, "the batch's two steps are on the redo stack");
        assert_eq!(srv.redo_ledger.len(), 1);
    }

    #[test]
    fn dry_run_at_the_cap_leaves_the_timeline_unchanged() {
        let mut srv = at_the_cap();
        let before = snapshot(&srv);
        let r = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "split", "at": "110f" }, { "op": "split", "at": "115f" }], "dry_run": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("dry run"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "rolled back exactly");
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.redo_depth(), 0, "the rolled-back states are not a branch to redo");
        assert!(srv.redo_ledger.is_empty());
        // The dry run's pushes evict nothing: the cap is held while it runs, so the
        // history and the ledger read exactly as they did before it.
        assert_eq!(s.undo_depth(), 100);
        assert_eq!(srv.ledger.len(), 100);
        assert_eq!(srv.ledger.iter().map(|e| e.steps).sum::<usize>(), s.undo_depth());
        assert!(srv.history_capped);
    }

    #[test]
    fn failing_batch_at_the_cap_rolls_back_and_keeps_the_ledger() {
        let mut srv = at_the_cap();
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "split", "at": "110f" }, { "op": "no_such_op" }]));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(snapshot(&srv), before);
        // The refused batch's push evicts nothing while it runs, so the rollback
        // leaves the history and the ledger as they were.
        let s = srv.session.as_ref().unwrap();
        assert_eq!(srv.ledger.len(), 100);
        assert_eq!(srv.ledger.iter().map(|e| e.steps).sum::<usize>(), s.undo_depth());
    }

    #[test]
    fn undo_past_the_cap_says_the_history_was_dropped() {
        let mut srv = at_the_cap();
        let r = run_undo(&mut srv, &json!({ "steps": 200 }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("older history was dropped by the 100-step cap"), "{}", r.text);
        assert!(srv.ledger.is_empty());
        let again = run_undo(&mut srv, &json!({}));
        assert!(again.is_error);
        assert!(again.text.contains("older history was dropped by the 100-step cap"), "{}", again.text);
    }

    /// Splits, undoes the split so the redo history holds it, and returns the server with the
    /// redo entry's serial, which a batch that leaves no trace must keep.
    fn undone_split() -> (Server, u64) {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        assert!(!run_undo(&mut srv, &json!({})).is_error);
        assert_eq!(srv.redo_ledger.len(), 1);
        let serial = srv.redo_ledger[0].serial;
        (srv, serial)
    }

    #[test]
    fn dry_run_keeps_the_redo_history_and_the_dirty_state() {
        let (mut srv, serial) = undone_split();
        let undone = snapshot(&srv);
        {
            let s = srv.session.as_ref().unwrap();
            srv.saved_depth = Some(s.undo_depth());
            srv.saved_pushes = Some(s.history_pushes());
            srv.saved_sources = Some(s.sources().len());
        }
        srv.saved_top = srv.ledger_top();
        assert!(!crate::tools::project::dirty(&srv), "the test starts clean");
        let half = first_len(&srv) / 2;
        let r = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "split", "at": format!("{}f", half / 2) }], "dry_run": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(snapshot(&srv), undone, "rolled back exactly");
        assert_eq!(srv.session.as_ref().unwrap().redo_depth(), 1, "the redo step survives the dry run");
        assert_eq!(srv.redo_ledger.len(), 1);
        assert_eq!(srv.redo_ledger[0].serial, serial, "the entry is the same one");
        assert!(!crate::tools::project::dirty(&srv), "a dry run leaves the project clean");
        let rd = run_redo(&mut srv, &json!({}));
        assert!(!rd.is_error, "{}", rd.text);
        assert!(srv.redo_ledger.is_empty());
    }

    #[test]
    fn failed_batch_keeps_the_redo_history() {
        let (mut srv, serial) = undone_split();
        let undone = snapshot(&srv);
        let half = first_len(&srv) / 2;
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{}f", half / 2) }, { "op": "no_such_op" }]));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(snapshot(&srv), undone);
        assert_eq!(srv.session.as_ref().unwrap().redo_depth(), 1);
        assert_eq!(srv.redo_ledger.len(), 1);
        assert_eq!(srv.redo_ledger[0].serial, serial);
        assert!(!run_redo(&mut srv, &json!({})).is_error);
    }

    #[test]
    fn batch_that_changes_nothing_keeps_the_redo_history() {
        let (mut srv, serial) = undone_split();
        let undone = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1000000f" }]));
        assert!(r.is_error || r.text.contains("left the timeline as it was"), "{}", r.text);
        assert_eq!(snapshot(&srv), undone);
        assert_eq!(srv.session.as_ref().unwrap().redo_depth(), 1);
        assert_eq!(srv.redo_ledger[0].serial, serial);
    }

    #[test]
    fn dry_run_keeps_the_redo_history_at_the_cap() {
        let mut srv = at_the_cap();
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": "120f" }])).is_error);
        assert!(!run_undo(&mut srv, &json!({})).is_error);
        let undone = snapshot(&srv);
        let r = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "split", "at": "110f" }, { "op": "split", "at": "115f" }], "dry_run": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(snapshot(&srv), undone);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.redo_depth(), 1);
        assert_eq!(s.undo_depth(), 99, "the undo left one step short of the cap, and the dry run keeps that depth");
        assert_eq!(srv.ledger.len(), 99);
    }

    #[test]
    fn dry_run_changes_nothing_and_keeps_no_entry() {
        let mut srv = open();
        let before = snapshot(&srv);
        let half = first_len(&srv) / 2;
        let r = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "split", "at": format!("{half}f") }], "dry_run": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("dry run"), "{}", r.text);
        assert_eq!(snapshot(&srv), before);
        assert!(srv.ledger.is_empty());
    }

    #[test]
    fn undo_then_redo_restores() {
        let mut srv = open();
        let before = snapshot(&srv);
        let half = first_len(&srv) / 2;
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        let after = snapshot(&srv);
        assert_ne!(after, before);

        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(snapshot(&srv), before);
        assert!(srv.ledger.is_empty());
        assert_eq!(srv.redo_ledger.len(), 1);

        let rd = run_redo(&mut srv, &json!({}));
        assert!(!rd.is_error, "{}", rd.text);
        assert_eq!(snapshot(&srv), after);
        assert_eq!(srv.ledger.len(), 1);
        assert!(srv.redo_ledger.is_empty());

        assert!(run_redo(&mut srv, &json!({})).is_error);
    }

    #[test]
    fn keep_three_ranges_lays_them_back_to_back() {
        let mut srv = open();
        let (fps, end, n_before) = {
            let s = srv.session.as_ref().unwrap();
            (parse::session_fps(s), parse::session_end(s), s.lane_clips(Lane::V1).len())
        };
        let r = ed(
            &mut srv,
            json!([{ "op": "keep", "source": 0, "ranges": [["0s", "1s"], ["2s", "3s"], ["4s", "5s"]] }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v1 = s.lane_clips(Lane::V1);
        assert_eq!(v1.len(), n_before + 3, "{}", r.text);
        let tail = &v1[n_before..];
        assert_eq!(tail[0].start, end);
        for w in tail.windows(2) {
            assert_eq!(w[0].end(), w[1].start);
        }
        assert_eq!(tail[0].frames(), fps.round() as u32);
        assert_eq!(srv.ledger.len(), 1);
    }

    #[test]
    fn assemble_lays_two_items_end_to_end() {
        let mut srv = open();
        let (end, n_before) = {
            let s = srv.session.as_ref().unwrap();
            (parse::session_end(s), s.lane_clips(Lane::V1).len())
        };
        let r = ed(
            &mut srv,
            json!([{ "op": "assemble", "items": [
                { "source": 0, "in": "0s", "out": "1s" },
                { "source": 0, "in": "2s", "out": "3s" }
            ] }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v1 = s.lane_clips(Lane::V1);
        assert_eq!(v1.len(), n_before + 2, "{}", r.text);
        let tail = &v1[n_before..];
        assert_eq!(tail[0].start, end);
        assert_eq!(tail[0].end(), tail[1].start);
        assert_eq!(srv.ledger.len(), 1);
    }

    #[test]
    fn assemble_refuses_gap_without_lane_and_bad_source() {
        let mut srv = open();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(
            &mut srv,
            json!([{ "op": "assemble", "gap": "1s", "items": [{ "source": 0, "in": "0s", "out": "1s" }] }]),
        );
        assert!(r.is_error);
        assert!(r.text.contains("gap needs `lane`"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "assemble", "items": [{ "source": 7 }] }]));
        assert!(r.is_error);
        assert!(r.text.contains("there is no src:7"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    /// Gives this test its own checkpoint folder inside `td`.
    fn own_checkpoints(td: &TestDir) {
        TEST_CHECKPOINTS.with(|d| *d.borrow_mut() = Some(td.join("checkpoints")));
    }

    #[test]
    fn checkpoint_edit_restore_returns_to_the_checkpoint() {
        let td = TestDir::new("checkpoint-edit-restore");
        own_checkpoints(&td);
        let mut srv = open();
        let half = first_len(&srv) / 2;
        let c = run_checkpoint(&mut srv, &json!({ "name": "ck_edit_restore" }));
        assert!(!c.is_error, "{}", c.text);
        let before = snapshot(&srv);

        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert_ne!(snapshot(&srv), before);

        let rs = run_restore(&mut srv, &json!({ "name": "ck_edit_restore" }));
        assert!(!rs.is_error, "{}", rs.text);
        assert_eq!(snapshot(&srv), before);
        assert!(srv.ledger.is_empty());
        assert!(srv.redo_ledger.is_empty());
        assert!(crate::tools::project::dirty(&srv), "a restored checkpoint reads as unsaved");

        let list = run_checkpoints(&mut srv, &json!({}));
        assert!(list.text.contains("ck_edit_restore"), "{}", list.text);
    }

    #[test]
    fn restore_refuses_an_unknown_name_and_bad_names() {
        let mut srv = open();
        assert!(run_restore(&mut srv, &json!({ "name": "never_made" })).is_error);
        assert!(run_checkpoint(&mut srv, &json!({ "name": "has space" })).is_error);
        assert!(run_checkpoint(&mut srv, &json!({ "name": "../escape" })).is_error);
    }

    /// A session with no clips yet and the 30 fps test asset imported as src:0.
    fn empty_with_av() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    #[test]
    fn digest_says_no_clips_for_an_empty_lane() {
        let srv = empty_with_av();
        let s = srv.session.as_ref().unwrap();
        let fps = parse::session_fps(s);
        let body = digest(s, &[Lane::V1], fps);
        assert_eq!(body, "V1: no clips", "{body}");
    }

    #[test]
    fn place_without_a_lane_honours_at() {
        let mut srv = empty_with_av();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let at = (20.0 * parse::session_fps(s)).round() as u32;
        assert_eq!(s.lane_clips(Lane::V1)[0].start, at, "the pair lands at 20s, not at 0");
        assert!(r.text.contains(&format!("at timeline {at}f")), "{}", r.text);
        assert!(s.lane_clips(Lane::A1).iter().any(|c| c.start == at), "the sound half lands there too");
    }

    #[test]
    fn place_on_a_video_lane_pairs_the_sound_on_a1_as_one_take() {
        let mut srv = empty_with_av();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let at = (1.0 * parse::session_fps(s)).round() as u32;
        let v = s.lane_clips(Lane::V1).iter().find(|c| c.start == at).expect("picture on V1");
        let a = s.lane_clips(Lane::A1).iter().find(|c| c.start == at).expect("sound on A1");
        assert!(v.link.is_some() && v.link == a.link, "the halves share one link");
        assert_eq!(v.frames(), a.frames());
        assert!(r.text.contains(&format!("at timeline {at}f")), "{}", r.text);
    }

    #[test]
    fn place_on_a_video_lane_with_audio_false_is_picture_only() {
        let mut srv = empty_with_av();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1).len(), 1);
        assert!(s.lane_clips(Lane::A1).is_empty(), "no sound half");
    }

    #[test]
    fn place_on_a_video_lane_sound_goes_to_audio_lane_when_given() {
        let mut srv = empty_with_av();
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio_lane": "A2" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let added: Vec<Lane> = s.lanes().into_iter().filter(|l| l.kind == LaneKind::Audio && *l != Lane::A1).collect();
        assert_eq!(added.len(), 1, "one new audio lane");
        assert_eq!(s.lane_clips(added[0]).len(), 1, "the sound went to A2");
        assert!(s.lane_clips(Lane::A1).is_empty());
        assert!(s.lane_clips(Lane::V1)[0].link.is_some());
    }

    #[test]
    fn place_on_a_video_lane_refuses_an_occupied_audio_lane_and_changes_nothing() {
        let mut srv = empty_with_av();
        let first = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "0s" }]));
        assert!(!first.is_error, "{}", first.text);
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("would overlap A1@0"), "{}", r.text);
        assert!(r.text.contains("audio:false") && r.text.contains("audio_lane"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refused placement changes nothing");
    }

    #[test]
    fn keep_on_a_video_lane_pairs_its_sound() {
        let mut srv = empty_with_av();
        let r = ed(&mut srv, json!([{ "op": "keep", "source": 0, "lane": "V1", "ranges": [["0s", "1s"], ["2s", "3s"]] }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1).len(), 2);
        assert_eq!(s.lane_clips(Lane::A1).len(), 2, "each range's sound is on A1");
    }

    #[test]
    fn audio_options_must_be_sane() {
        let mut srv = empty_with_av();
        let wrong_kind = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio_lane": "V2" }]));
        assert!(wrong_kind.text.contains("`audio_lane` must be an audio lane"), "{}", wrong_kind.text);
        let no_video = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "0s", "audio": false }]));
        assert!(no_video.text.contains("give `lane` a video lane"), "{}", no_video.text);
        let bad = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": "no" }]));
        assert!(bad.text.contains("`audio` must be true or false"), "{}", bad.text);
        let clash = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "audio_lane": "A1" }]));
        assert!(clash.text.contains("contradict"), "{}", clash.text);
        assert!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).is_empty(), "refusals change nothing");
    }

    #[test]
    fn move_reply_has_one_arrow() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let line = r.text.lines().find(|l| l.starts_with("1. ")).expect("op line");
        assert_eq!(line.matches('→').count(), 1, "{line}");
    }

    #[test]
    fn undo_and_redo_refuse_bad_steps_the_same_way() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        for bad in [json!(-1), json!(0), json!(1.5), json!("2")] {
            let u = run_undo(&mut srv, &json!({ "steps": bad }));
            assert!(u.is_error && u.text.contains("whole number of at least 1"), "{}", u.text);
            let d = run_redo(&mut srv, &json!({ "steps": bad }));
            assert!(d.is_error && d.text.contains("whole number of at least 1"), "{}", d.text);
        }
        assert_eq!(srv.ledger.len(), 1, "refused steps change nothing");
    }

    #[test]
    fn undo_names_a_short_ledger() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        let u = run_undo(&mut srv, &json!({ "steps": 5 }));
        assert!(!u.is_error, "{}", u.text);
        assert!(u.text.contains("asked for 5, only 1 edit batch can be undone"), "{}", u.text);
        assert!(srv.ledger.is_empty());
    }

    #[test]
    fn speed_with_a_text_x_says_it_must_be_a_number() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": "fast" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("`x` must be a number"), "{}", r.text);
        let missing = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0" }]));
        assert!(missing.text.contains("missing `x`"), "{}", missing.text);
    }

    #[test]
    fn remove_time_after_a_speed_change_leaves_no_hole_and_says_so() {
        // The 5 s test clip slowed to half speed is 300 frames; a cut at 55f splits it where
        // 27 source frames would give a 54-frame piece, one frame short of the cut.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "55f", "duration": "30f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("the cut moved from 55f to 54f"), "{}", r.text);
        assert!(!r.text.contains("hole left"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(s.gap_count(lane), 0, "{lane:?} keeps a hole: {}", r.text);
            let clips = s.lane_clips(lane);
            assert!(clips.windows(2).all(|w| w[0].end() == w[1].start), "{lane:?} clips do not abut: {}", r.text);
        }
    }

    #[test]
    fn a_hole_a_take_cannot_close_is_not_sent_to_close_gaps() {
        // A1 alone is trimmed, so its gap has a linked V1 clip with no gap there: close_gaps cannot close it.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "A1@0", "edge": "out", "to": "2s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("close_gaps cannot close it") && !r.text.contains("closes it"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "A1" }]));
        assert!(r.text.contains("closed 0") && r.text.contains("the gap stays open") && !r.text.contains("ungroup them"), "{}", r.text);
        assert!(!r.text.contains("gap there too"), "{}", r.text);
    }

    #[test]
    fn move_insert_names_the_clip_it_splits() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@150", "at": "60f", "insert": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("split V1@0 at 2.0s"), "{}", r.text);
    }

    #[test]
    fn split_edit_moves_one_half_of_the_take_and_unlinks_the_pair() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        let end_v = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].end();
        // An L-cut: the sound's out edge comes back to 1 s; the picture keeps its end.
        let r = ed(&mut srv, json!([{ "op": "split_edit", "clip": "V1@0", "side": "audio", "edge": "out", "to": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("split_edit A1@0 audio out") && r.text.contains("the pair is unlinked"), "{}", r.text);
        {
            let s = srv.session.as_ref().unwrap();
            assert_eq!(s.lane_clips(Lane::V1)[0].end(), end_v, "the picture moved: {}", r.text);
            let one_sec = s.meta().frame_rate.round() as u32;
            assert_eq!(s.lane_clips(Lane::A1)[0].end(), one_sec, "the sound is one second: {}", r.text);
            assert!(s.lane_clips(Lane::V1)[0].link.is_none() || s.lane_clips(Lane::V1)[0].link != s.lane_clips(Lane::A1)[0].link);
        }
        // one undo step takes the whole split back
        let u = super::super::edit::run_undo(&mut srv, &json!({ "steps": 1 }));
        assert!(!u.is_error, "{}", u.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::A1)[0].end(), end_v, "undo restores the sound");
        assert_eq!(s.lane_clips(Lane::V1)[0].link, s.lane_clips(Lane::A1)[0].link, "undo relinks the pair");
    }

    #[test]
    fn split_edit_on_the_sound_moves_the_picture_in_edge() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        // Anchored on A1, side video: the picture's in edge comes to 0.5 s and the sound stays at 0.
        let r = ed(&mut srv, json!([{ "op": "split_edit", "clip": "A1@0", "side": "video", "edge": "in", "to": "0.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("split_edit V1@") && r.text.contains("video in"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(Lane::V1)[0].start > 0, "the picture's in edge moved: {}", r.text);
        assert_eq!(s.lane_clips(Lane::A1)[0].start, 0, "the sound stays put: {}", r.text);
    }

    #[test]
    fn split_edit_refuses_a_bad_side_edge_or_frame_and_changes_nothing() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = snapshot(&srv);
        for (op, want) in [
            (json!({ "op": "split_edit", "clip": "V1@0", "edge": "out", "to": "1s" }), "`side` must be"),
            (json!({ "op": "split_edit", "clip": "V1@0", "side": "both", "edge": "out", "to": "1s" }), "`side` must be"),
            (json!({ "op": "split_edit", "clip": "V1@0", "side": "audio", "to": "1s" }), "`edge` must be"),
            (json!({ "op": "split_edit", "clip": "V1@0", "side": "audio", "edge": "in", "to": "end" }), "would pass the out edge"),
            (json!({ "op": "split_edit", "clip": "V1@0", "side": "audio", "edge": "out", "to": "0" }), "would pass the in edge"),
        ] {
            let r = ed(&mut srv, json!([op]));
            assert!(r.is_error && r.text.contains(want), "{want}: {}", r.text);
        }
        assert_eq!(snapshot(&srv), before, "a refused split_edit changes nothing");
    }

    #[test]
    fn insert_time_reports_only_the_hole_it_made() {
        // The gap 60f..150f was there before the insert: the insert moves it to 90f..180f and leaves its own.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": "src:0", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "2s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "30f", "duration": "30f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("hole left on V1 at 1.000–2.000s"), "{}", r.text);
        assert!(!r.text.contains("3.000–6.000s"), "the old gap moved, it was not made here: {}", r.text);
    }

    #[test]
    fn a_still_placed_on_a_missing_lane_names_the_lane() {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_av.mp4")).expect("open test asset");
        sess.import(&asset("test_still.png")).expect("import still");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "at": "1s", "lane": "V2" }]));
        assert!(r.is_error && r.text.contains("lane V2 is not on this timeline"), "{}", r.text);
        assert!(!r.text.contains("did not place"), "{}", r.text);
    }

    #[test]
    fn picture_end_note_follows_the_lowest_video_lane_with_clips() {
        // V1 is emptied, so the picture underneath is V2 and the logo on V3 runs past its end.
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lift", "clip": "V1@0" },
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": "src:0", "lane": "V2", "at": "0s", "audio": false },
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": "src:0", "lane": "V3", "at": "0s", "audio": false },
                { "op": "append", "source": "src:0", "lane": "V3", "audio": false }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V3 runs 5.0s past the picture's end at 5.0s"), "{}", r.text);
    }

    #[test]
    fn freeze_inserted_inside_audio_warns_and_names_the_clip() {
        let dir = TestDir::new("freeze-warn");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        // The test asset's take starts at 0, so 1s is strictly inside its audio.
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("warning: the insert cut audio clip A1@0"), "{}", r.text);
        assert!(r.text.contains("insert:false"), "{}", r.text);
    }

    #[test]
    fn checkpoints_live_beside_the_project_and_survive_a_restart() {
        let dir = TestDir::new("checkpoint-beside");
        let project = dir.join("cut.edith");
        let mut srv = open();
        srv.path = Some(project.clone());
        let half = first_len(&srv) / 2;
        let c = run_checkpoint(&mut srv, &json!({ "name": "before_split" }));
        assert!(!c.is_error, "{}", c.text);
        assert!(dir.join("checkpoints").join("cut").join("before_split.edith").is_file());
        let before = snapshot(&srv);

        // A fresh server, as after a restart: only the project path and the files on disk.
        let mut fresh = open();
        fresh.path = Some(project);
        let list = run_checkpoints(&mut fresh, &json!({}));
        assert!(list.text.contains("before_split"), "{}", list.text);
        let r = ed(&mut fresh, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        let rs = run_restore(&mut fresh, &json!({ "name": "before_split" }));
        assert!(!rs.is_error, "{}", rs.text);
        assert_eq!(snapshot(&fresh), before);
        assert!(run_restore(&mut fresh, &json!({ "name": "never_made" })).is_error);
    }

    #[test]
    fn checkpoint_without_a_project_path_says_it_is_temporary() {
        let td = TestDir::new("checkpoint-temp");
        own_checkpoints(&td);
        let mut srv = open();
        let c = run_checkpoint(&mut srv, &json!({ "name": "scratch_only" }));
        assert!(!c.is_error, "{}", c.text);
        assert!(c.text.contains("temp directory only"), "{}", c.text);
    }

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// A session opened on one asset from the assets folder, the way `open` opens test_av.mp4.
    fn open_asset(name: &str) -> Server {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(asset(name)).expect("open test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    /// A 25 fps timeline with the 30 fps test_av.mp4 imported as src:1.
    fn open_25_with_30() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_25fps.mp4")).expect("open 25 fps asset");
        sess.import(&asset("test_av.mp4")).expect("import 30 fps asset");
        Server { session: Some(sess), ..Server::default() }
    }

    #[test]
    fn source_seconds_use_the_source_fps_on_a_25_fps_timeline() {
        let mut srv = open_25_with_30();
        {
            let s = srv.session.as_ref().unwrap();
            assert_eq!(parse::session_fps(s), 25.0);
            assert!((s.file_fps(&asset("test_av.mp4")) - 30.0).abs() < 1e-3);
        }
        let r = ed(&mut srv, json!([{ "op": "keep", "source": 1, "ranges": [["0.5s", "2.0s"]] }]));
        assert!(!r.is_error, "{}", r.text);
        // The length is the duration in timeline frames: 1.5 s at 25 fps is 37.5, rounded up to 38f. The
        // in still snaps to native frame 15 (0.5 s), so the source end reads 2.033s, one source frame past 2.0s.
        assert!(r.text.contains("src 0.500s–2.033s"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let c = s.lane_clips(Lane::V1).last().expect("the keep placed a clip");
        assert_eq!(c.frames(), 38, "round(1.5 s x 25 fps)");
        assert_eq!((f64::from(c.in_frame) * 30.0 / 25.0).floor() as u32, 15, "in is 0.5s of the source");
    }

    #[test]
    fn source_range_errors_name_source_frames_and_seconds() {
        let mut srv = open_25_with_30();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "keep", "source": 1, "ranges": [["0s", "99s"]] }]));
        assert!(r.is_error);
        assert!(r.text.contains("plays 0.000s to 5.000s"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    #[test]
    fn exact_note_names_the_frame_a_time_rounded_to() {
        assert_eq!(exact_note(&json!("1.53s"), 25.0, 38, ""), " (1.53s → 38f)");
        assert_eq!(exact_note(&json!(1.53), 25.0, 38, " src"), " (1.53s → 38f src)");
        assert_eq!(exact_note(&json!("1.52s"), 25.0, 38, ""), "", "1.52s is exactly frame 38");
        assert_eq!(exact_note(&json!("450f"), 25.0, 450, ""), "", "frames are never rounded");
        assert_eq!(exact_note(&json!("00:00:01:12"), 25.0, 37, ""), "", "timecodes are exact");
    }

    #[test]
    fn off_frame_times_are_noted_in_the_op_line() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "split", "at": 1.53 }]));
        assert!(!r.is_error, "{}", r.text);
        let want = (1.53 * fps).round() as u32;
        assert!(r.text.contains(&format!("(1.53s → {want}f)")), "{}", r.text);
        let exact = ed(&mut srv, json!([{ "op": "split", "at": "1.0s" }]));
        assert!(!exact.is_error, "{}", exact.text);
        assert!(!exact.text.contains("s → "), "an exact time adds no note: {}", exact.text);
    }

    /// A slowed clip (0.333x) trimmed out past its source's end, with the next clip far off or
    /// close by: the reply names the wall that stopped it, from the bound the engine gave.
    #[test]
    fn trim_names_the_wall_that_stopped_it() {
        // Far neighbour: the source's end is the wall, and the reply says so.
        let mut srv = open();
        let r = ed(&mut srv, json!([
            { "op": "speed", "clip": "V1@0", "x": 0.333 },
            { "op": "place", "source": 0, "at": "60s", "lane": "V1", "audio": false }
        ]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "100s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("clamped at the source's end ("), "{}", r.text);
        assert!(!r.text.contains("neighbouring") && !r.text.contains("meets"), "{}", r.text);
        // The edge is now where the source ends: asking again says it cannot go further.
        let again = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "100s", "scope": "clip" }]));
        assert!(!again.is_error, "{}", again.text);
        assert!(again.text.contains("already at") && again.text.contains("clamped at the source's end"), "{}", again.text);

        // Close neighbour: the clip it meets is named by its address.
        // The slowed clip runs to 15 s, so it is shortened first to leave room for the neighbour at 8 s.
        let mut srv = open();
        let r = ed(&mut srv, json!([
            { "op": "speed", "clip": "V1@0", "x": 0.333 },
            { "op": "trim", "clip": "V1@0", "edge": "out", "to": "5s", "scope": "clip" },
            { "op": "place", "source": 0, "at": "8s", "lane": "V1", "audio": false }
        ]));
        assert!(!r.is_error, "{}", r.text);
        let next = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start;
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "100s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("clamped where it meets V1@{next}")), "{}", r.text);
        assert!(!r.text.contains("source's end"), "{}", r.text);
    }

    #[test]
    fn trim_follows_the_linked_take_by_default() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        let to = half / 2;
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": format!("{to}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(with A1@0)"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[0].end(), to);
        assert_eq!(s.lane_clips(Lane::A1)[0].end(), to, "the sound followed the picture");
    }

    #[test]
    fn trim_scope_clip_trims_only_that_clip() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        let to = half / 2;
        let r = ed(
            &mut srv,
            json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": format!("{to}f"), "scope": "clip" }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[0].end(), to);
        assert_eq!(s.lane_clips(Lane::A1)[0].end(), half, "the partner keeps its span");
        let bad = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "1f", "scope": "all" }]));
        assert!(bad.is_error && bad.text.contains("scope `all`"), "{}", bad.text);
    }

    /// `test_av.mp4` is 150 frames at 30 fps: cut at 30f and 90f into V1+A1 clips 0..30, 30..90, 90..150.
    fn three_clips() -> Server {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "30f" }, { "op": "split", "at": "90f" }]));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    /// `(start, end, in, out)` of every clip on a lane.
    fn spans(srv: &Server, lane: Lane) -> Vec<(u32, u32, u32, u32)> {
        let s = srv.session.as_ref().unwrap();
        s.lane_clips(lane).iter().map(|c| (c.start, c.end(), c.in_frame, c.out_frame)).collect()
    }

    #[test]
    fn roll_moves_the_cut_of_the_take_and_names_the_new_addresses() {
        let mut srv = three_clips();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@0", "by": "+10f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("roll V1@0 cut 30f (1.000s) → 40f (1.333s)"), "{}", r.text);
        assert!(r.text.contains("V1@40") && r.text.contains("A1@40"), "the new addresses: {}", r.text);
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(spans(&srv, lane), [(0, 40, 0, 40), (40, 90, 40, 90), (90, 150, 90, 150)], "{lane:?}");
        }
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth + 1, "one undo step");
        // `to` is the absolute cut place, and edge:"in" names the cut before the clip.
        let r = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@40", "edge": "in", "to": "1s" }]));
        assert!(!r.is_error && r.text.contains("cut 40f (1.333s) → 30f (1.000s)"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[..2], [(0, 30, 0, 30), (30, 90, 30, 90)]);
        let run = run_undo(&mut srv, &json!({ "steps": 2 }));
        assert!(!run.is_error, "{}", run.text);
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(spans(&srv, lane), [(0, 30, 0, 30), (30, 90, 30, 90), (90, 150, 90, 150)], "{lane:?}");
        }
    }

    #[test]
    fn roll_says_when_a_limit_clamped_and_refuses_without_a_cut() {
        let mut srv = three_clips();
        let r = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@0", "to": "end" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("clamped from") && r.text.contains("the cut can travel 1f"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[0].1, 89, "a frame is left for the clip after the cut");
        // The last clip has nothing after it; `by` and `to` together are one too many.
        let last = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@90", "by": "5f" }]));
        assert!(last.is_error && last.text.contains("no clip after V1@90"), "{}", last.text);
        let both = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@0", "by": "5f", "to": "9f" }]));
        assert!(both.is_error && both.text.contains("`by` or `to`, not both"), "{}", both.text);
        let none = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@0" }]));
        assert!(none.is_error && none.text.contains("missing `by` or `to`"), "{}", none.text);
        // A gap is not a cut.
        let mut gap = three_clips();
        assert!(!ed(&mut gap, json!([{ "op": "lift", "clip": "V1@30" }])).is_error);
        let r = ed(&mut gap, json!([{ "op": "roll", "clip": "V1@0", "by": "5f" }]));
        assert!(r.is_error && r.text.contains("a gap, not a cut"), "{}", r.text);
        let typo = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@0", "at": "5f" }]));
        assert!(typo.is_error && typo.text.contains("unknown key `at`; roll takes clip, by, to, edge"), "{}", typo.text);
    }

    #[test]
    fn roll_at_speed_two_moves_two_source_frames_per_frame() {
        let mut srv = three_clips();
        // V1@30 and A1@30 play 60 source frames in 30 timeline frames.
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@30", "x": 2.0, "ripple": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[1], (30, 60, 30, 90));
        // The gap after it (60..90) is not a cut; roll the cut before it instead.
        let r = ed(&mut srv, json!([{ "op": "roll", "clip": "V1@30", "edge": "in", "by": "-10f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[..2], [(0, 20, 0, 20), (20, 60, 10, 90)], "10 timeline frames of speed 2 are 20 source frames");
    }

    #[test]
    fn slip_shifts_the_window_keeps_the_slot_and_is_one_undo() {
        let mut srv = three_clips();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@30", "by": "10f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("source 30f–90f → 40f–100f") && r.text.contains("still 30f"), "{}", r.text);
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(spans(&srv, lane)[1], (30, 90, 40, 100), "{lane:?} the take slipped together");
        }
        // Negative, with a time form: -0.5 s is 15 frames back.
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@30", "by": "-0.5s" }]));
        assert!(!r.is_error && r.text.contains("by -15f"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[1], (30, 90, 25, 85));
        // Clamped to the file: the clip can only reach back 25 frames.
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@30", "by": "-100f" }]));
        assert!(!r.is_error && r.text.contains("clamped from -100 to -25 source frames"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[1], (30, 90, 0, 60));
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth + 3);
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@30", "by": "-5f" }]));
        assert!(!r.is_error && r.text.contains("clamped"), "pinned at the start: {}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth + 3, "a pinned slip costs no step");
        assert!(!run_undo(&mut srv, &json!({ "steps": 3 })).is_error);
        assert_eq!(spans(&srv, Lane::V1)[1], (30, 90, 30, 90));
    }

    #[test]
    fn slip_at_speed_converts_timeline_frames_to_source_frames() {
        let mut srv = three_clips();
        assert!(!ed(&mut srv, json!([{ "op": "speed", "clip": "V1@30", "x": 2.0, "ripple": false }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@30", "by": "3f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("+3f on the timeline is 6 source frames at 2x"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[1], (30, 60, 36, 96), "same slot, 6 source frames later");
        // A quarter of a source frame is nothing to slip.
        let mut srv = three_clips();
        assert!(!ed(&mut srv, json!([{ "op": "speed", "clip": "V1@90", "x": 0.25, "ripple": false }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "slip", "clip": "V1@90", "by": "1f" }]));
        assert!(!r.is_error && r.text.contains("less than one source frame"), "{}", r.text);
    }

    #[test]
    fn slide_moves_the_clip_between_its_neighbours_keeping_the_length() {
        let mut srv = three_clips();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "slide", "clip": "V1@30", "by": "+0.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("slide V1@30 by +15f: now V1@45") && r.text.contains("V1@0 now ends at 45f"), "{}", r.text);
        assert!(r.text.contains("V1@105 now starts at 105f"), "{}", r.text);
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(spans(&srv, lane), [(0, 45, 0, 45), (45, 105, 30, 90), (105, 150, 105, 150)], "{lane:?}");
        }
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth + 1);
        let r = ed(&mut srv, json!([{ "op": "slide", "clip": "V1@45", "by": "-99f" }]));
        assert!(!r.is_error && r.text.contains("clamped from -99f to -44f"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[0], (0, 1, 0, 1));
        assert!(!run_undo(&mut srv, &json!({ "steps": 2 })).is_error);
        for lane in [Lane::V1, Lane::A1] {
            assert_eq!(spans(&srv, lane), [(0, 30, 0, 30), (30, 90, 30, 90), (90, 150, 90, 150)], "{lane:?}");
        }
    }

    #[test]
    fn slide_needs_a_clip_on_each_side() {
        let mut srv = three_clips();
        let before = snapshot(&srv);
        let first = ed(&mut srv, json!([{ "op": "slide", "clip": "V1@0", "by": "5f" }]));
        assert!(first.is_error && first.text.contains("no clip before it on V1"), "{}", first.text);
        let last = ed(&mut srv, json!([{ "op": "slide", "clip": "A1@90", "by": "-5f" }]));
        assert!(last.is_error && last.text.contains("no clip after it on A1"), "{}", last.text);
        let missing = ed(&mut srv, json!([{ "op": "slide", "clip": "V1@30" }]));
        assert!(missing.is_error && missing.text.contains("missing `by`"), "{}", missing.text);
        assert_eq!(snapshot(&srv), before);
    }

    #[test]
    fn roll_slip_and_slide_refuse_a_locked_lane_of_the_take() {
        for op in [
            json!({ "op": "roll", "clip": "V1@0", "by": "5f" }),
            json!({ "op": "slip", "clip": "V1@30", "by": "5f" }),
            json!({ "op": "slide", "clip": "V1@30", "by": "5f" }),
        ] {
            let mut srv = three_clips();
            srv.session.as_mut().unwrap().set_lane_locked(Lane::A1, true);
            let before = snapshot(&srv);
            let r = ed(&mut srv, json!([op]));
            assert!(r.is_error && r.text.contains("lane A1 is locked"), "{}: {}", op, r.text);
            assert_eq!(snapshot(&srv), before);
        }
    }

    #[test]
    fn a_locked_lane_refuses_by_name() {
        let mut srv = open();
        srv.session.as_mut().unwrap().set_lane_locked(Lane::A1, true);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A1", "at": "0s" }]));
        assert!(r.is_error);
        assert!(r.text.contains("lane A1 is locked"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    #[test]
    fn a_move_refusal_names_the_clip_it_would_overlap() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        // Two clips and a third lane-mate: moving the first onto the second's start with no room must name it.
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": format!("{half}f") }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("would overlap V1@{half} (")), "{}", r.text);
        assert!(r.text.contains("move V1@0 to V1@"), "{}", r.text);
    }

    #[test]
    fn a_placed_pair_is_one_take_so_trim_moves_both() {
        let mut srv = open();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": format!("{end}f"), "in": "0s", "out": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let to = end + 10;
        let t = ed(&mut srv, json!([{ "op": "trim", "clip": format!("V1@{end}"), "edge": "out", "to": format!("{}f", end + 15) }]));
        assert!(!t.is_error, "{}", t.text);
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1).iter().find(|c| c.start == end).expect("placed picture");
        let a = s.lane_clips(Lane::A1).iter().find(|c| c.start == end).expect("placed sound");
        assert_eq!(v.end(), end + 15);
        assert_eq!(a.end(), end + 15, "the placed sound followed the picture (to {to})");
    }

    #[test]
    fn a_move_to_where_the_clip_already_is_is_a_no_op() {
        let mut srv = open();
        let before = snapshot(&srv);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("move V1@0: already at 0f (0.000s) — nothing to do"), "{}", r.text);
        assert!(r.text.contains("no change"), "{}", r.text);
        assert_eq!(snapshot(&srv), before);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth, "no engine snapshot was pushed");
        assert!(srv.ledger.is_empty(), "no ledger entry for a no-op");
    }

    #[test]
    fn a_trim_error_echoes_the_unit_the_caller_wrote() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // An out-of-room edge is clamped and the reply names the unit the caller wrote.
        let short = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "3s" }]));
        assert!(!short.is_error, "{}", short.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "99s" }]));
        let want = format!("clamped from 99s ({}f)", (99.0 * fps).round() as u32);
        assert!(!r.is_error && r.text.contains(&want), "{}", r.text);
        let mut fresh = open();
        ed(&mut fresh, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "3s" }]));
        let frames = ed(&mut fresh, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "2970f" }]));
        assert!(frames.text.contains("clamped from 2970f"), "a frame count is shown alone: {}", frames.text);
    }

    #[test]
    fn placing_on_a_missing_lane_lists_the_lanes_and_names_the_fix() {
        let mut srv = open();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A2", "at": end }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("lane A2 is not on this timeline; the lanes are "), "{}", r.text);
        assert!(r.text.contains("A1") && r.text.contains("V1"), "{}", r.text);
        assert!(r.text.contains(r#"{"op":"lane_add","kind":"audio"}"#), "{}", r.text);
    }

    #[test]
    fn undo_and_redo_name_the_batch_they_move() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        let a = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }]));
        assert!(!a.is_error, "{}", a.text);
        let b = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "fade", "clip": "V1@0", "out": "5f" }], "label": "Fade out" }),
        );
        assert!(!b.is_error, "{}", b.text);
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        let want = format!("undid: Fade out · undo 1 · redo 1 · next undo: split @{half}f");
        assert!(u.text.starts_with(&want), "{}", u.text);
        let r = run_redo(&mut srv, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("redid: Fade out · undo 2 · redo 0"), "{}", r.text);
    }

    #[test]
    fn an_unlabelled_batch_is_named_by_its_ops() {
        let ops = json!([
            { "op": "split", "at": "1f" },
            { "op": "fade", "clip": "V1@0", "out": "5f" },
            { "op": "speed", "clip": "V1@0", "x": 2 },
            { "op": "title_remove", "title": "T1" }
        ]);
        assert_eq!(summarize(ops.as_array().unwrap(), &[]), "split @1f + fade V1@0 (+2 more)");
        assert_eq!(summarize(&[json!({ "op": "title_remove", "title": "T1" })], &[]), "title_remove T1");
    }

    /// A clip op's label names the clips its apply reply touched, so a linked fade reads with its
    /// sound and a second fade on the sound is not labelled with the picture's address.
    #[test]
    fn a_clip_op_is_labelled_by_the_clips_it_touched() {
        let ops = [json!({ "op": "fade", "clip": "V1@0", "out": "5f" }), json!({ "op": "fade", "clip": "A1@0", "out": "5f" })];
        let touched = [vec!["V1@0".to_string(), "A1@0".to_string()], vec!["A1@0".to_string()]];
        assert_eq!(summarize(&ops, &touched), "fade V1@0, A1@0 + fade A1@0");
        let many: Vec<String> = ["V1@0", "A1@0", "V2@0", "A2@0"].iter().map(|s| s.to_string()).collect();
        assert_eq!(summarize(&ops[..1], &[many]), "fade V1@0, A1@0, V2@0 +1 more");
    }

    #[test]
    fn an_unlabelled_linked_batch_is_labelled_with_its_applied_clips() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "out": "1.0s" }, { "op": "fade", "clip": "A1@0", "out": "1.0s" }]));
        assert!(!r.is_error, "{}", r.text);
        let label = &srv.ledger.last().expect("one undo step").label;
        assert!(label.contains("V1@0, A1@0") && label.contains("fade A1@0"), "{label}");
        assert!(!label.contains("fade V1@0 + fade V1@0"), "{label}");
    }

    #[test]
    fn a_batch_that_changes_the_duration_reports_the_timeline() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "0s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let want = format!(
            "timeline {:.3}s → {:.3}s",
            parse::secs(before, fps),
            parse::secs(before, fps) + 1.0
        );
        assert!(r.text.contains(&want), "{}", r.text);
        let same = ed(&mut srv, json!([{ "op": "split", "at": "1.0s" }]));
        assert!(!same.text.contains("timeline "), "a batch that keeps the length adds no line: {}", same.text);
    }

    #[test]
    fn insert_time_opens_a_gap_on_every_lane_and_is_one_undo_step() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let at = (1.0 * fps).round() as u32;
        let dur = (2.0 * fps).round() as u32;
        let len = first_len(&srv);
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1s", "duration": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        {
            let s = srv.session.as_ref().unwrap();
            for lane in [Lane::V1, Lane::A1] {
                let clips = s.lane_clips(lane);
                assert_eq!(clips.len(), 2, "{lane:?}: the clip was cut at 1s");
                assert_eq!((clips[0].start, clips[0].end()), (0, at));
                assert_eq!(clips[1].start, at + dur, "{lane:?}: the rest moved two seconds later");
                assert_eq!(clips[1].frames(), len - at);
            }
        }
        assert_eq!(srv.ledger.len(), 1, "one batch, one undo entry");
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(snapshot(&srv), before);
    }

    #[test]
    fn insert_time_moves_titles_after_it() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let t = ed(&mut srv, json!([{ "op": "title_add", "at": "2s", "duration": "1s", "text": "Hello" }]));
        assert!(!t.is_error, "{}", t.text);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "0s", "duration": "3s" }]));
        assert!(!r.is_error, "{}", r.text);
        let title = srv.session.as_ref().unwrap().titles()[0].clone();
        assert_eq!(title.start, (5.0 * fps).round() as u32);
    }

    #[test]
    fn title_add_and_set_take_a_colour_weight_alignment_and_box() {
        use engine::text::{Align, HPlace, Weight};
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": "1s", "duration": "2s", "text": "Jane\nDirector", "place": "bottom",
            "hplace": "left", "align": "left", "weight": "bold", "color": "#ffcc00", "boxed": true, "box_color": "#112233", "box_opacity": 0.5 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("colour #ffcc00 bold left align left box #112233/50%"), "{}", r.text);
        let st = srv.session.as_ref().unwrap().titles()[0].style.clone();
        assert_eq!((st.hplace, st.align, st.weight, st.ink), (HPlace::Left, Align::Left, Weight::Bold, [255, 204, 0]));
        assert_eq!(st.box_color, [0x11, 0x22, 0x33, 128]);
        // Six digits keep the opacity, eight set it; the rest merges onto the title.
        let r = ed(&mut srv, json!([{ "op": "title_set", "title": "T1", "box_color": "000000c8", "align": "right" }]));
        assert!(!r.is_error, "{}", r.text);
        let st = srv.session.as_ref().unwrap().titles()[0].style.clone();
        assert_eq!((st.box_color, st.align, st.weight), ([0, 0, 0, 200], Align::Right, Weight::Bold));
        let r = ed(&mut srv, json!([{ "op": "title_set", "title": "T1", "box_color": "#ffffff" }]));
        assert_eq!(srv.session.as_ref().unwrap().titles()[0].style.box_color, [255, 255, 255, 200], "{}", r.text);
        for (key, val) in [("color", "red"), ("color", "#fff"), ("weight", "heavy"), ("hplace", "middle"), ("align", "justify"), ("box_color", "#12345"), ("box_opacity", "2")] {
            let r = ed(&mut srv, json!([{ "op": "title_set", "title": "T1", key: val }]));
            assert!(r.is_error, "{key} {val}: {}", r.text);
        }
        // Plain titles reply as they did.
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": "5s", "duration": "1s", "text": "Plain" }]));
        assert!(!r.text.contains("colour") && !r.text.contains(" box "), "{}", r.text);
    }

    #[test]
    fn insert_time_refuses_a_locked_lane_it_would_move() {
        let mut srv = open();
        srv.session.as_mut().unwrap().set_lane_locked(Lane::A1, true);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "0s", "duration": "1s", "lanes": ["A1"] }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("lane A1 is locked"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    /// An explicit `lanes` that holds a take's sound but not its picture refuses: the cut would split
    /// V1 and leave its half behind. The refusal names V1 and the lanes to add; adding it works.
    #[test]
    fn insert_time_refuses_to_split_a_take_partner_outside_lanes() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }, { "op": "place", "source": 0, "lane": "A2", "at": "0s", "in": "0s", "out": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = snapshot(&srv);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1s", "duration": "1s", "lanes": ["A1", "A2"] }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("one take with the V1 clip at V1@0"), "{}", r.text);
        assert!(r.text.contains("Add V1 to lanes (lanes:[\"A1\",\"A2\",\"V1\"])"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refusal changes nothing");
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1s", "duration": "1s", "lanes": ["V1", "A1", "A2"] }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let two_s = (2.0 * parse::session_fps(s)).round() as u32;
        assert!(s.lane_clips(Lane::V1).iter().any(|c| c.start == two_s), "the picture's second half moved with its sound");
    }

    /// cut_regions on lanes with nothing in the span says so, instead of only counting the span.
    #[test]
    fn cut_regions_on_an_empty_lane_says_nothing_lies_in_the_span() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cut_regions", "regions": [["1s", "2s"]], "lanes": ["A2"] }]));
        assert!(r.text.contains("nothing to cut, nothing on A2 lies in the span(s)"), "{}", r.text);
        assert!(r.text.contains("no change"), "{}", r.text);
    }

    /// A clip wholly inside the span is named as removed, with its linked sound; a clip the span
    /// crosses is named as trimmed, with the timeline span it now has.
    #[test]
    fn remove_time_names_the_clips_it_removes_and_trims() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let f = |s: f64| (s * fps).round() as u32;
        for at in ["1s", "2s"] {
            let r = ed(&mut srv, json!([{ "op": "split", "at": at }]));
            assert!(!r.is_error, "{}", r.text);
        }
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("removed V1@{} (test_av.mp4)", f(1.0))), "{}", r.text);
        assert!(r.text.contains(&format!("A1@{} (test_av.mp4)", f(1.0))), "the linked sound is named too: {}", r.text);

        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains(&format!("trimmed V1@{} (test_av.mp4) now 1.0s–", f(1.0))),
            "the clip the span crosses is named as trimmed with its new span: {}",
            r.text
        );
    }

    /// A split names only the clips it cuts: a lane whose clip merely ends at the cut is not one.
    #[test]
    fn split_names_only_the_clips_it_cut() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let f = |s: f64| (s * fps).round() as u32;
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s", "lanes": ["V1"] }]));
        assert!(!r.is_error, "{}", r.text);
        // V1 now has a cut at 1s, so its clips end there; A1 still runs through it.
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("→ A1@0, A1@{}", f(1.0))), "{}", r.text);
        assert!(!r.text.contains("V1@0"), "V1 was not cut here, so it is not named: {}", r.text);
    }

    #[test]
    fn remove_time_cuts_the_span_and_moves_the_rest_back() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let end0 = parse::session_end(srv.session.as_ref().unwrap());
        let ok = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!ok.is_error, "{}", ok.text);
        let end1 = parse::session_end(srv.session.as_ref().unwrap());
        assert_eq!(end0 - end1, (1.0 * fps).round() as u32, "one second came out: {}", ok.text);
    }

    /// A title wholly inside a removed span goes with it, one that straddles an edge is trimmed to
    /// the part outside, and a title after the span moves back. Each is named in the reply.
    #[test]
    fn remove_time_removes_a_title_inside_the_span_and_trims_a_straddler() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let f = |s: f64| (s * fps).round() as u32;
        for (at, dur, text) in [("1.2s", "0.5s", "Inside"), ("1.5s", "1s", "Straddles"), ("3s", "1s", "After")] {
            let t = ed(&mut srv, json!([{ "op": "title_add", "at": at, "duration": dur, "text": text }]));
            assert!(!t.is_error, "{}", t.text);
        }
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "a title inside the span is no refusal: {}", r.text);
        assert!(r.text.contains("removed with the span: T1") && r.text.contains("trimmed to the span's edge: T2 now 1.0s–1.5s"), "{}", r.text);
        let titles = srv.session.as_ref().unwrap().titles().to_vec();
        let texts: Vec<&str> = titles.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, ["Straddles", "After"], "the inside title is gone: {}", r.text);
        assert_eq!((titles[0].start, titles[0].frames), (f(1.0), f(0.5)), "the straddler keeps the part before the span's end");
        assert_eq!(titles[1].start, f(2.0), "the title after the span moves back by it");
    }

    /// A move carries a title wholly over the clip by default, says which title stays put when
    /// it only partly covers the clip, and `titles` true or false sets the choice.
    #[test]
    fn move_carries_the_titles_over_the_clip_and_says_which_stay() {
        fn setup() -> Server {
            // The 5 s take on V1 and A1: "Over" lies wholly on the picture, "Half" runs past its end.
            let mut srv = open();
            for (at, dur, text) in [("0.5s", "1s", "Over"), ("4.5s", "1s", "Half")] {
                let t = ed(&mut srv, json!([{ "op": "title_add", "at": at, "duration": dur, "text": text }]));
                assert!(!t.is_error, "{}", t.text);
            }
            srv
        }
        let starts = |srv: &Server| -> Vec<(String, u32)> {
            srv.session.as_ref().unwrap().titles().iter().map(|t| (t.text.clone(), t.start)).collect()
        };
        let fps = 30.0;
        let over = 15; // 0.5 s
        let half = 135; // 4.5 s
        let to10 = (10.0 * fps) as u32; // the clip moves from 0 to 10 s

        let mut srv = setup();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "10s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("titles moved with the clip: T1"), "{}", r.text);
        assert!(r.text.contains("T2 (1.0s) sat over V1@0 and stays put"), "the partly covered title stays: {}", r.text);
        assert_eq!(starts(&srv), vec![("Half".to_string(), half), ("Over".to_string(), to10 + over)], "{}", r.text);

        let mut srv = setup();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "10s", "titles": false }]));
        assert!(!r.is_error && !r.text.contains("titles moved"), "{}", r.text);
        assert_eq!(starts(&srv), vec![("Over".to_string(), over), ("Half".to_string(), half)], "titles:false carries none: {}", r.text);

        let mut srv = setup();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "10s", "titles": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(starts(&srv), vec![("Over".to_string(), to10 + over), ("Half".to_string(), to10 + half)], "titles:true carries both: {}", r.text);
    }

    /// A delete names the clips it moved, and no "left in place" list: the clips on other lanes
    /// it does not touch are not news in a plain delete (ripple_delete still lists them).
    #[test]
    fn delete_names_no_left_in_place_list() {
        let mut srv = open();
        let ops = json!([
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "place", "source": 0, "at": "4s", "in": "0s", "out": "1s", "lane": "V2", "audio": false },
            { "op": "delete", "clip": "V2@0", "lanes": ["V2"] }
        ]);
        let r = ed(&mut srv, ops);
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("left in place"), "{}", r.text);
        let clips = srv.session.as_ref().unwrap().lane_clips(Lane::new(LaneKind::Video, 1));
        assert_eq!(clips.last().map(|c| c.start), Some(60), "the clip after the deleted one moved back by its length: {}", r.text);
    }

    /// `place` with `stretch` fits the whole source into `length` and says the speed it used; a
    /// stretch outside the speed range is refused by name, and the refusal names stretch.
    #[test]
    fn place_stretch_fits_the_source_into_the_length() {
        let mut srv = open();
        let added = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }]));
        assert!(!added.is_error, "{}", added.text);
        // The 5 s asset into 3.2 s: the speed is 5 / 3.2, about 1.5625.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "0s", "length": "3.2s", "stretch": true, "lane": "V2", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("stretched ×1.56") && r.text.contains("(sped up)"), "{}", r.text);
        let c = srv.session.as_ref().unwrap().lane_clips(Lane::new(LaneKind::Video, 1))[0].clone();
        assert!((c.frames() as i64 - 96).abs() <= 1, "the clip fills 3.2 s, got {} frames", c.frames());
        // Without stretch the same length past the source end still refuses, and names stretch.
        let past = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "40s", "in": "4s", "length": "2s", "lane": "V1", "audio": false }]));
        assert!(past.is_error && past.text.contains("stretch:true"), "{}", past.text);
        let fast = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "40s", "in": "0s", "length": "0.5s", "stretch": true, "lane": "V1", "audio": false }]));
        assert!(fast.is_error && fast.text.contains("outside 0.25–4"), "{}", fast.text);
    }

    #[test]
    fn a_speed_factor_reads_to_three_decimals_and_names_its_direction() {
        assert_eq!(speed_text(5.0 / 16.2), "0.309");
        assert_eq!(speed_text(2.0), "2");
        assert_eq!(speed_note(5.0 / 16.2), " (slow motion)");
        assert_eq!(speed_note(1.5625), " (sped up)");
        assert_eq!(speed_note(1.0), "");
    }

    /// The source end is shown no later than the file's real duration: 6.000 s of frames on a
    /// 5.968 s file reads 5.968s.
    #[test]
    fn a_source_end_past_the_real_duration_reads_the_duration() {
        let b = Base { src: 25_000_000, tl: 30_000_000, dur: 5.968 };
        assert_eq!(b.pos(150), "5.968s", "a frame rounded past the file's end is the end");
        assert_eq!(b.pos(100), "4.000s", "inside the file, the frame's own time");
        let whole = Base { src: 25_000_000, tl: 30_000_000, dur: f64::MAX };
        assert_eq!(whole.pos(150), "6.000s", "no duration known: the frame's own time");
    }

    /// A seconds length that overlaps by a frame says why: the length rounds to whole frames and
    /// the clip ends a frame later than the seconds suggest, with the frame length to give instead.
    #[test]
    fn a_seconds_length_that_overlaps_explains_the_rounding() {
        let mut srv = open();
        // The V1 clip from 0 covers the first seconds; 2.0167 s is 60.5 frames at 30 fps, so it
        // rounds up to 61f and runs to frame 91 from timeline frame 30.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1s", "length": "2.0167s", "audio": false }]));
        assert!(r.is_error && r.text.contains(" is taken: src:0"), "{}", r.text);
        assert!(r.text.contains("length 2.0167s rounds up to 61f (2.033s) and ends at 91f"), "{}", r.text);
        assert!(r.text.contains("give length in frames (e.g. \"60f\") or start 1 frame later"), "{}", r.text);
        // A length in frames is not explained: it has no rounding to explain.
        let frames = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1s", "length": "61f", "audio": false }]));
        assert!(frames.is_error && !frames.text.contains("rounds"), "{}", frames.text);
    }

    /// A seconds `at` or `in` that rounds to a frame is named in the overlap refusal, with the math and
    /// the frame to write. A refusal where every value is already a frame has no note.
    #[test]
    fn a_seconds_start_or_in_that_rounds_is_named_in_an_overlap() {
        let mut srv = open();
        // 1.0167 s is 30.501 frames at 30 fps: it rounds to 31f (1.033 s), inside the clip from 0.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1.0167s", "length": "0.5s", "audio": false }]));
        assert!(r.is_error && r.text.contains(" is taken: src:0"), "{}", r.text);
        assert!(r.text.contains("at 1.0167s is 30.501f, which rounds to 31f (1.033s)"), "{}", r.text);
        assert!(r.text.contains("write at \"31f\" in frames"), "{}", r.text);
        // 0.5 s is exactly 15 frames, so the length is not explained.
        assert!(!r.text.contains("length 0.5s"), "{}", r.text);
        // 1.53 s is 45.9 source frames at 30 fps: the `in` rounds to 46f.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "1s", "in": "1.53s", "audio": false }]));
        assert!(r.is_error && r.text.contains("in 1.53s is 45.900f at 30 fps, which rounds to 46f"), "{}", r.text);
        assert!(r.text.contains("write in \"46f\" in frames"), "{}", r.text);
        // Frames all round: nothing is named.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "30f", "length": "61f", "audio": false }]));
        assert!(r.is_error && !r.text.contains("rounds") && !r.text.contains("in frames"), "{}", r.text);
    }

    /// Holes left by several ops of one batch are one summary line per lane, not a line per op. A single
    /// op keeps its own line.
    #[test]
    fn holes_from_several_ops_are_one_summary_line() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }, { "op": "split", "at": "2s" }, { "op": "split", "at": "3s" }, { "op": "split", "at": "4s" }]));
        assert!(!r.is_error, "{}", r.text);
        // Two clips apart: each lift leaves its own gap. (Adjacent lifts would merge into one gap.)
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "A1@30" }, { "op": "lift", "clip": "A1@90" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("holes left on A1 at 1.000s, 3.000s (close_gaps cannot close them: the A1 clip at A1@0"), "{}", r.text);
        assert_eq!(r.text.matches("holes left on A1").count(), 1, "{}", r.text);
        assert!(!r.text.contains("by lift"), "{}", r.text);
        // One op alone keeps its own line, named by the span it left.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }, { "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "A1@30" }]));
        assert!(r.text.contains("hole left on A1 at 1.000–2.000s by lift"), "{}", r.text);
    }

    /// A caption cue a delete cuts out is named in its reply, as ripple_delete names it.
    #[test]
    fn delete_names_the_caption_cues_it_removes() {
        let mut srv = linked_take();
        let dir = TestDir::new("delete-cues");
        let a = dir.path().join("a.srt");
        std::fs::write(&a, "1\n00:00:01,000 --> 00:00:02,000\nInside cue\n").unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" },
                { "op": "delete", "clip": "V1@0" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("caption cue(s) removed: \"Inside cue\" (1.000s–2.000s)"), "{}", r.text);
        assert!(srv.session.as_ref().unwrap().sub_lane_cues(Lane::S1).is_empty(), "the cue is gone");
    }

    /// A stretch slower than the source reads as slow motion, with the factor at three decimals:
    /// 5 s into 16.2 s is ×0.309 (5 / 16.2 = 0.30864...).
    #[test]
    fn place_stretch_slower_than_the_source_says_slow_motion() {
        let mut srv = open();
        let added = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }]));
        assert!(!added.is_error, "{}", added.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "0s", "length": "16.2s", "stretch": true, "lane": "V2", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("stretched ×0.309 to ") && r.text.contains("(485f) (slow motion)"), "{}", r.text);
        assert!(!r.text.contains("0.30927835"), "the factor is rounded, not printed in full: {}", r.text);
    }

    /// A dissolve or a fade whose clip a cut removes is named in the reply, not dropped silently.
    #[test]
    fn a_dropped_dissolve_or_fade_is_named_in_the_effects() {
        let mut srv = open();
        let r = ed(&mut srv, json!([
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "place", "source": 0, "at": "2s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "dissolve", "clip": "V2@0", "frames": 12, "linked": false },
            { "op": "fade", "clip": "V2@2s", "out": "5f", "linked": false },
            { "op": "delete", "clip": "V2@0", "lanes": ["V2"] }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("dissolve dropped on V2@0"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "delete", "clip": "V2@0", "lanes": ["V2"] }]));
        assert!(!r.is_error && r.text.contains("fade dropped on V2 ×1"), "{}", r.text);
    }

    /// A cut closes a hole it leaves under the sliver on the lanes it ripples, and says so.
    #[test]
    fn cut_regions_closes_a_sliver_hole_on_the_lanes_in_scope() {
        let mut srv = open();
        // Two picture clips with a single frame between them: a hole under CUT_SLIVER_FRAMES.
        let r = ed(&mut srv, json!([
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "0f", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "place", "source": 0, "at": "61f", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "cut_regions", "regions": [["100f", "102f"]], "lanes": ["V2"] }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("closed 1 hole(s) under 4f on V2"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().gap_count(Lane::new(LaneKind::Video, 1)), 0, "{}", r.text);
    }

    /// Mean absolute difference of two composed frames, per BGRA byte.
    fn mean_diff(a: &engine::decode::Frame, b: &engine::decode::Frame) -> f64 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let total: u64 = a.bgra.iter().zip(&b.bgra).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
        total as f64 / a.bgra.len() as f64
    }

    fn compose(srv: &Server, at: u32) -> engine::decode::Frame {
        let (proj, meta) = srv.session.as_ref().unwrap().export_snapshot();
        engine::export::compose_at(&proj, &meta, at, meta.width, meta.height).expect("compose")
    }

    #[test]
    fn freeze_inserts_a_still_and_shifts_the_rest() {
        let dir = TestDir::new("freeze-insert");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let at = (1.0 * fps).round() as u32;
        let dur = (2.0 * fps).round() as u32;
        let len = first_len(&srv);
        let end0 = parse::session_end(srv.session.as_ref().unwrap());
        let pre = compose(&srv, at);
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": "1s", "duration": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("freeze/cut-{at}-")), "{}", r.text);
        assert_eq!(std::fs::read_dir(dir.join("freeze")).unwrap().count(), 1);
        let post = compose(&srv, at + (fps.round() as u32));
        {
            let s = srv.session.as_ref().unwrap();
            assert_eq!(parse::session_end(s), end0 + dur, "the timeline grew by the duration");
            let clips = s.lane_clips(Lane::V1);
            let still = clips.iter().find(|c| c.start == at).expect("a still starts at `at`");
            assert_eq!(still.frames(), dur, "the still is held for the duration");
            let name = s.sources()[still.source].path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(&format!("cut-{at}-")) && name.ends_with(".png"), "{name}");
            let rest = clips.iter().find(|c| c.start == at + dur).expect("the rest moved by the duration");
            assert_eq!(rest.frames(), len - at);
        }
        assert!(mean_diff(&pre, &post) < 4.0, "the still shows the frame it was taken from");
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(snapshot(&srv), before);
    }

    /// The source index of the still on V1 that starts at `at`.
    fn still_source(srv: &Server, at: u32) -> usize {
        let s = srv.session.as_ref().unwrap();
        s.lane_clips(Lane::V1).iter().find(|c| c.start == at).expect("a still at `at`").source
    }

    #[test]
    fn freeze_same_frame_reuses_the_source_and_a_graded_clip_gets_a_new_one() {
        let dir = TestDir::new("freeze-reuse");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let at = (1.0 * parse::session_fps(srv.session.as_ref().unwrap())).round() as u32;
        let freeze = json!([{ "op": "freeze", "at": "1s", "duration": "1s" }]);

        let r = ed(&mut srv, freeze.clone());
        assert!(!r.is_error, "{}", r.text);
        let first = still_source(&srv, at);
        let sources0 = srv.session.as_ref().unwrap().sources().len();
        assert!(!run_undo(&mut srv, &json!({})).is_error);

        // Same frame, nothing changed: the file and its source are reused.
        let r = ed(&mut srv, freeze.clone());
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(still_source(&srv, at), first, "the same pixels reuse the same source index");
        assert_eq!(srv.session.as_ref().unwrap().sources().len(), sources0, "no duplicate import");
        assert_eq!(std::fs::read_dir(dir.join("freeze")).unwrap().count(), 1);
        assert!(!run_undo(&mut srv, &json!({})).is_error);

        // A grade on the clip changes the pixels: a new file and a new source.
        let g = ed(&mut srv, json!([{ "op": "color", "clip": "V1#0", "brightness": 0.3 }]));
        assert!(!g.is_error, "{}", g.text);
        let r = ed(&mut srv, freeze);
        assert!(!r.is_error, "{}", r.text);
        assert_ne!(still_source(&srv, at), first, "a changed frame gets a new source");
        assert_eq!(std::fs::read_dir(dir.join("freeze")).unwrap().count(), 2);
    }

    #[test]
    fn freeze_refuses_an_unsaved_project_and_changes_nothing() {
        let mut srv = open();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": "1s", "duration": "1s" }]));
        assert!(r.is_error && r.text.contains("project_save"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    #[test]
    fn freeze_without_insert_overlays_a_new_top_lane() {
        let dir = TestDir::new("freeze-overlay");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let end0 = parse::session_end(srv.session.as_ref().unwrap());
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": "1s", "duration": "1s", "insert": false }]));
        assert!(!r.is_error, "{}", r.text);
        // The reply says why the still is on a lane the caller did not name.
        assert!(r.text.contains("added lane V2 for the still, since insert:false with no lane"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(parse::session_end(s), end0, "an overlay keeps the length");
        let top = Lane::new(LaneKind::Video, 1);
        let fps = parse::session_fps(s);
        let dur = (fps).round() as u32;
        let still = s.lane_clips(top).iter().find(|c| c.start == (fps).round() as u32).expect("still on V2");
        assert_eq!(still.frames(), dur);
        let after = snapshot(&srv);
        assert_eq!(after.iter().find(|(l, _)| *l == Lane::V1), before.iter().find(|(l, _)| *l == Lane::V1));
    }

    /// The test asset opens as a linked V1+A1 take at 0.
    fn linked_take() -> Server {
        let srv = open();
        let s = srv.session.as_ref().unwrap();
        assert!(!s.lane_clips(Lane::A1).is_empty(), "the test asset has sound");
        assert_eq!(s.lane_clips(Lane::V1)[0].link, s.lane_clips(Lane::A1)[0].link, "one take");
        srv
    }

    #[test]
    fn fade_reaches_the_linked_sound_and_says_so() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "out": "1.0s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.fade_out_of(Lane::V1, 0) > 0, "picture faded");
        assert!(s.fade_out_of(Lane::A1, 0) > 0, "sound faded with it");
        assert!(r.text.contains("V1@0") && r.text.contains("A1@0"), "{}", r.text);
    }

    #[test]
    fn fade_says_when_the_engine_clamps_the_ramp() {
        let mut srv = linked_take();
        // The clip is 5.0 s; a 9.0 s ramp is kept at the clip's length, and the reply says so.
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "in": "9.0s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.fade_in_of(Lane::V1, 0), s.lane_clips(Lane::V1)[0].frames());
        assert!(r.text.contains("in clamped from 9.0s to the clip's 5.0s"), "{}", r.text);
        // A ramp that fits is named without a clamp note.
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "out": "1.0s", "linked": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("clamped"), "{}", r.text);
    }

    #[test]
    fn fade_with_linked_false_leaves_the_sound_alone() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "out": "1.0s", "linked": false }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.fade_out_of(Lane::V1, 0) > 0);
        assert_eq!(s.fade_out_of(Lane::A1, 0), 0, "sound untouched");
        assert!(!r.text.contains("A1@0"), "{}", r.text);
    }

    #[test]
    fn fade_linked_must_be_a_bool() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "out": "5f", "linked": "no" }]));
        assert!(r.is_error && r.text.contains("`linked` must be true or false"), "{}", r.text);
    }

    /// A read tool (`clip_info`, `timeline`) called the way a model calls it.
    fn read(srv: &mut Server, name: &str, args: Value) -> Reply {
        let t = crate::server::tools().into_iter().find(|t| t.name == name).expect("tool exists");
        (t.run)(srv, &args)
    }

    /// The link the engine holds for the clip at `at` on `lane`, against what the two reads a
    /// model makes say about it: `clip_info` names that id (or "link: none"), and the timeline
    /// shows a link on some row exactly when some clip holds one.
    fn assert_reads_agree(srv: &mut Server, addr: &str, lane: Lane, at: u32) {
        // How many placements (clips and captions, any lane) carry each id, read
        // off the engine in its own block so the session is not borrowed while
        // the reads below run.
        let (engine, shared, any_take, any_detached) = {
            let s = srv.session.as_ref().expect("session");
            let carried = |id: u32| {
                s.lanes()
                    .into_iter()
                    .map(|l| {
                        s.lane_clips(l).iter().filter(|c| c.link == Some(id)).count()
                            + s.sub_lane(l).iter().filter(|c| c.link == Some(id)).count()
                    })
                    .sum::<usize>()
            };
            let engine = s.lane_clips(lane).iter().find(|c| c.start == at).and_then(|c| c.link);
            let ids: Vec<u32> = s.lanes().into_iter().flat_map(|l| s.lane_clips(l).iter().filter_map(|c| c.link).collect::<Vec<_>>()).collect();
            let any_take = ids.iter().any(|&id| carried(id) >= 2);
            let any_detached = ids.iter().any(|&id| carried(id) == 1);
            (engine, engine.is_some_and(|id| carried(id) >= 2), any_take, any_detached)
        };
        let info = read(srv, "clip_info", json!({ "clip": addr }));
        assert!(!info.is_error, "{}", info.text);
        match engine {
            None => assert!(info.text.contains("link: none"), "{addr} holds no id: {}", info.text),
            Some(id) if shared => assert!(info.text.contains(&format!("link {id}")), "{addr} holds {id}: {}", info.text),
            Some(_) => assert!(info.text.contains("detached (ungrouped)"), "{addr} is detached: {}", info.text),
        }
        let tl = read(srv, "timeline", json!({}));
        assert!(!tl.is_error, "{}", tl.text);
        assert_eq!(tl.text.contains("  linked to "), any_take, "timeline disagrees about takes: {}", tl.text);
        assert_eq!(tl.text.contains("detached (ungrouped)"), any_detached, "timeline disagrees about detached clips: {}", tl.text);
    }

    #[test]
    fn a_lifted_partner_reads_unlinked_in_timeline_and_clip_info() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_reads_agree(&mut srv, "A1@0", Lane::A1, 0);
    }

    #[test]
    fn a_one_lane_ripple_delete_leaves_the_partner_unlinked_in_both_reads() {
        let mut srv = linked_take();
        // A one-lane ripple of a take is refused, so the take is detached first.
        let r = ed(
            &mut srv,
            json!([
                { "op": "ungroup", "clip": "V1@0" },
                { "op": "ripple_delete", "clip": "V1@0", "lanes": ["V1"] }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).is_empty(), "the picture is gone");
        assert_reads_agree(&mut srv, "A1@0", Lane::A1, 0);
    }

    #[test]
    fn a_copy_of_the_same_source_elsewhere_on_its_lane_detaches_nothing() {
        // The copy shares the picture's identity (source and in-point) but has no sound, so a lookup
        // by identity alone would find it in place of the linked clip and call the take detached.
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:0", "lane": "V1", "at": "10s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("detached") && !r.text.contains("linked:"), "{}", r.text);
        for op in [
            json!({ "op": "limiter", "on": true, "ceiling_db": -3 }),
            json!({ "op": "title_add", "at": "2s", "duration": "1s", "text": "Hi" }),
        ] {
            let r = ed(&mut srv, json!([op]));
            assert!(!r.is_error, "{}", r.text);
            assert!(!r.text.contains("detached") && !r.text.contains("linked:"), "{}", r.text);
        }
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1).iter().find(|c| c.start == 0).expect("the linked picture");
        let a = s.lane_clips(Lane::A1).iter().find(|c| c.start == 0).expect("the linked sound");
        assert!(v.link.is_some() && v.link == a.link, "the take is still one take");
    }

    #[test]
    fn an_ungroup_still_names_the_detached_partners() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "ungroup", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V1@0 is now detached") && r.text.contains("A1@0 is now detached"), "{}", r.text);
    }

    #[test]
    fn an_ungroup_that_leaves_a_take_names_the_detached_clip_and_the_take_that_stays() {
        // The loose V2 clip is its own take (it starts at 3s), grouped with the V1+A1 take.
        let mut srv = linked_take();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": 0, "lane": "V2", "at": "3s", "in": "0s", "out": "2s", "audio": false },
                { "op": "group", "clips": ["V1@0", "V2@3s"] }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "ungroup", "clip": "V2@3s" }]));
        assert!(!r.is_error, "{}", r.text);
        // The reply names clips by their frame address: 3s is V2@90 at 30 fps.
        assert!(r.text.contains("V2@90 is now detached"), "{}", r.text);
        assert!(r.text.contains("V1@0+A1@0 stay one take"), "{}", r.text);
        assert!(!r.text.contains("V1@0 is now detached") && !r.text.contains("A1@0 is now detached"), "{}", r.text);
    }

    /// A 30 fps timeline with the 23.976 fps test_23976fps.mp4 (2.002 s picture, 2.0 s sound) as src:1.
    fn open_30_with_23976() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_av.mp4")).expect("open 30 fps asset");
        sess.import(&asset("test_23976fps.mp4")).expect("import 23.976 fps asset");
        Server { session: Some(sess), ..Server::default() }
    }

    /// The picture and sound a placement landed at `start` are one span: same frames, same source range.
    fn assert_take_spans_agree(srv: &Server, start: u32) {
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1).iter().find(|c| c.start == start).expect("the picture");
        let a = s.lane_clips(Lane::A1).iter().find(|c| c.start == start).expect("the sound");
        assert_eq!(v.frames(), a.frames(), "picture {}f, sound {}f", v.frames(), a.frames());
        assert_eq!((v.in_frame, v.out_frame), (a.in_frame, a.out_frame), "picture and sound source ranges differ");
    }

    #[test]
    fn a_23976_take_places_picture_and_sound_over_one_span() {
        let mut srv = open_30_with_23976();
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "at": "47s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_take_spans_agree(&srv, 1410);
    }

    #[test]
    fn a_23976_take_placed_on_a_lane_places_one_span() {
        let mut srv = open_30_with_23976();
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "lane": "V1", "at": "47s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_take_spans_agree(&srv, 1410);
    }

    #[test]
    fn a_25_fps_take_places_picture_and_sound_over_one_span() {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_av.mp4")).expect("open 30 fps asset");
        sess.import(&asset("test_25fps.mp4")).expect("import 25 fps asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "at": "47s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_take_spans_agree(&srv, 1410);
    }

    #[test]
    fn an_explicit_lanes_split_unlinks_every_half_in_both_reads() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s", "lanes": ["V1"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("detached at the cut"), "{}", r.text);
        let right = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start;
        assert_reads_agree(&mut srv, "V1@0", Lane::V1, 0);
        assert_reads_agree(&mut srv, &format!("V1@{right}"), Lane::V1, right);
        assert_reads_agree(&mut srv, "A1@0", Lane::A1, 0);
    }

    #[test]
    fn undoing_a_lift_puts_the_link_back_in_both_reads() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        let back = read(&mut srv, "undo", json!({}));
        assert!(!back.is_error, "{}", back.text);
        assert_reads_agree(&mut srv, "V1@0", Lane::V1, 0);
        assert_reads_agree(&mut srv, "A1@0", Lane::A1, 0);
    }

    #[test]
    fn lane_move_takes_a_number_for_to_through_the_batch() {
        let mut srv = linked_take();
        let added = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }]));
        assert!(!added.is_error, "{}", added.text);
        let r = ed(&mut srv, json!([{ "op": "lane_move", "lane": "V2", "to": 1 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(position 1)"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lanes()[0].kind, LaneKind::Video);
    }

    #[test]
    fn lane_move_shifts_name_each_clips_new_lane_label() {
        let mut srv = linked_take();
        assert!(!ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }])).is_error);
        // The empty V2 moves to position 1: the picture's lane takes the label V2 (its label is its position among video lanes).
        let r = ed(&mut srv, json!([{ "op": "lane_move", "lane": "V2", "to": 1 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("shifted: V1@0→V2@0"), "{}", r.text);
        assert!(!r.text.contains("split"), "a lane move splits nothing: {}", r.text);
    }

    #[test]
    fn lane_move_refuses_a_name_for_to_and_lists_the_lanes() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "lane_move", "lane": "V1", "to": "V2" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("1-based position of the lane in the lane list"), "{}", r.text);
        assert!(r.text.contains("V1 (1), A1 (2)"), "the lane list is named: {}", r.text);
    }

    /// A take of three lanes: V2 picture with sound on A2 and A3, all 2 s from 0.
    fn three_lane_take() -> Server {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "lane_add", "kind": "audio" },
                { "op": "lane_add", "kind": "audio" },
                { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
                { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "A2" },
                { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "A3" },
                { "op": "group", "clips": ["V2@0", "A2@0", "A3@0"] }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    #[test]
    fn scope_clip_trim_leaves_the_partners_linked_to_each_other() {
        let mut srv = three_lane_take();
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V2@0", "edge": "out", "to": "1s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 partners stay linked to each other"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let (v, a2, a3) = (s.lane_clips(Lane::new(LaneKind::Video, 1))[0].link, s.lane_clips(Lane::new(LaneKind::Audio, 1))[0].link, s.lane_clips(Lane::new(LaneKind::Audio, 2))[0].link);
        assert!(v.is_some() && v != a2, "V2 left the take");
        assert!(a2.is_some() && a2 == a3, "A2 and A3 stay one take");
        assert_eq!(s.lane_clips(Lane::new(LaneKind::Audio, 1))[0].frames(), s.lane_clips(Lane::new(LaneKind::Audio, 2))[0].frames(), "the partners were not trimmed");
    }

    #[test]
    fn scope_clip_trim_with_a_locked_partner_refuses_and_names_the_way_out() {
        let mut srv = three_lane_take();
        assert!(!ed(&mut srv, json!([{ "op": "lane_flags", "lane": "A3", "locked": true }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V2@0", "edge": "out", "to": "1s", "scope": "clip" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("alone unlinks it from A3, which is locked; unlock A3 or use scope:\"group\""), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::new(LaneKind::Video, 1))[0].frames(), (2.0 * parse::session_fps(s)).round() as u32, "nothing moved (the batch rolled back)");
    }

    #[test]
    fn close_gaps_on_a_lane_with_no_gap_says_so() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V1 has no gaps"), "{}", r.text);
        assert!(!r.text.contains("not a gap"), "{}", r.text);
    }

    #[test]
    fn a_move_to_another_lane_is_not_reported_as_a_split() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
                { "op": "lane_add", "kind": "video" },
                { "op": "move", "clip": "V2@0", "at": "5s", "to_lane": "V3" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("split"), "nothing was split: {}", r.text);
    }

    #[test]
    fn ripple_delete_names_the_caption_cues_it_removes() {
        let mut srv = linked_take();
        let dir = TestDir::new("ripple-cues");
        let a = dir.path().join("a.srt");
        std::fs::write(&a, "1\n00:00:01,000 --> 00:00:02,000\nInside cue\n").unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" },
                { "op": "ripple_delete", "clip": "V1@0" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("caption cue(s) removed: \"Inside cue\" (1.000s–2.000s)"), "{}", r.text);
        assert!(!r.text.contains("trimmed"), "a cue wholly inside the span is not trimmed: {}", r.text);
        assert!(srv.session.as_ref().unwrap().sub_lane_cues(Lane::S1).is_empty(), "the cue is gone");
    }

    #[test]
    fn a_cut_that_only_trims_a_cue_says_trimmed_with_old_and_new_times() {
        let mut srv = linked_take();
        let dir = TestDir::new("cut-trims-cue");
        let a = dir.path().join("a.srt");
        std::fs::write(&a, "1\n00:00:01,000 --> 00:00:02,000\nStraddle\nline two\n").unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" },
                { "op": "remove_time", "at": "1.5s", "duration": "0.5s" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("removed"), "the cue keeps its first half, so nothing is removed: {}", r.text);
        assert!(
            r.text.contains("caption cue(s) trimmed: S1 cue 1 'Straddle / line two' → 'Straddle / line' (was 1.000s–2.000s, now 1.000s–1.500s)"),
            "{}",
            r.text
        );
    }

    #[test]
    fn an_unknown_lane_is_not_a_lane_and_an_empty_one_has_no_clips() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "delete", "clip": "V9@0" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("V9 is not a lane in this project; the lanes are V1, A1"), "{}", r.text);
        assert!(!r.text.contains("has no clips"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }, { "op": "delete", "clip": "V2@0" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("V2 has no clips"), "{}", r.text);
    }

    #[test]
    fn lane_move_names_every_lane_it_renamed_and_what_holds_it() {
        let mut srv = linked_take();
        // A locked lane does not move, so the batch is refused and nothing changes.
        let r = ed(
            &mut srv,
            json!([{ "op": "lane_add", "kind": "audio" }, { "op": "lane_flags", "lane": "A2", "locked": true }, { "op": "lane_move", "lane": "A2", "to": 2 }]),
        );
        assert!(r.is_error && r.text.contains("lane_move A2: lane A2 is locked"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lanes().len(), 2, "the refused batch added no lane");
        // The unlocked lane moves past the locked one: the locked lane is renamed by position, and says so.
        let r = ed(
            &mut srv,
            json!([{ "op": "lane_add", "kind": "audio" }, { "op": "lane_flags", "lane": "A2", "locked": true }, { "op": "lane_move", "lane": "A1", "to": 3 }]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains("lanes renamed by position: A1 → A2 (test_av.mp4), A2 → A1 (locked)"),
            "{}",
            r.text
        );
        assert!(r.text.contains("use the new names from now on"), "{}", r.text);
    }

    #[test]
    fn a_copy_look_onto_a_target_that_already_has_it_says_nothing_to_copy() {
        let mut srv = linked_take();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": 0, "lane": "V2", "at": "300f", "in": "0s", "out": "2s", "audio": false },
                { "op": "color", "clip": "V1@0", "brightness": 0.2 }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@300"] }]));
        assert!(!r.is_error && !r.text.contains("no change"), "a real copy is not a no-change: {}", r.text);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@300"] }]));
        assert!(r.text.starts_with("no change: V2@300 already has V1@0's color: nothing to copy"), "{}", r.text);
        assert!(!r.text.contains("copied"), "{}", r.text);
    }

    #[test]
    fn keep_names_the_frame_snapped_span_it_placed() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "keep", "source": 0, "ranges": [["1.2345s", "2.5"]], "lane": "V2", "at": "0s", "audio": false }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 1.233s–2.500s"), "{}", r.text);
    }

    #[test]
    fn a_dip_past_one_clip_names_the_clips_that_cover_the_span() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "audio" },
                { "op": "place", "source": 0, "lane": "A2", "at": "0s", "in": "0s", "out": "1s" },
                { "op": "place", "source": 0, "lane": "A2", "at": "1s", "in": "2s", "out": "3s" },
                { "op": "gain_dip", "clip": "A2@0", "from": "0.5s", "to": "2.5s", "db": -6 }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("the span 0.5s–2.5s covers A2@0…A2@30: dip each, or use lane gain keys"), "{}", r.text);
    }

    #[test]
    fn a_dip_at_the_clip_start_says_there_is_no_ramp_in() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "audio" },
                { "op": "place", "source": 0, "lane": "A2", "at": "0s", "in": "0s", "out": "2s" },
                { "op": "gain_dip", "clip": "A2@0", "from": "0s", "to": "1s", "db": -6, "ramp": "0.5s" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains("no ramp in: the dip starts at the clip's start, so there is no fade down into it; start it later than "),
            "{}",
            r.text
        );
        assert!(r.text.contains("for a ramp"), "{}", r.text);
        assert!(!r.text.contains("shortened from"), "{}", r.text);
    }

    #[test]
    fn lane_remove_names_the_clips_it_refuses_by_address_and_file() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "lane_remove", "lane": "A1" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("A1 still holds 1 clip(s): A1@0 (test_av.mp4)"), "{}", r.text);
        assert!(!r.text.contains("/home/"), "paths are shortened to file names: {}", r.text);
    }

    #[test]
    fn undo_and_redo_with_nothing_to_take_say_nothing_to_undo() {
        let mut srv = open();
        let r = run_undo(&mut srv, &json!({}));
        assert!(r.is_error && r.text.starts_with("nothing to undo yet; checkpoints survive"), "{}", r.text);
        assert!(!r.text.contains("project_open"), "a new project never opened from a file: {}", r.text);
        // A project opened from a file with no edits lost no history, so no reopen is blamed for it.
        srv.saved_depth = Some(0);
        let r = run_undo(&mut srv, &json!({}));
        assert!(r.is_error && r.text.starts_with("nothing to undo yet; checkpoints survive"), "{}", r.text);
        assert!(!r.text.contains("project_open"), "{}", r.text);
        let r = run_redo(&mut srv, &json!({}));
        assert!(r.is_error && r.text.starts_with("nothing to redo ("), "{}", r.text);
    }

    #[test]
    fn swap_trades_two_clips_on_one_lane_and_lists_the_moves() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // Two picture-only clips on V1, 2 s at 20 s and 3 s at 25 s, with a gap between.
        let p1 = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "0s", "out": "2s", "lane": "V1", "audio": false }]));
        assert!(!p1.is_error, "{}", p1.text);
        let p2 = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "25s", "in": "0s", "out": "3s", "lane": "V1", "audio": false }]));
        assert!(!p2.is_error, "{}", p2.text);
        let (a, b) = ((20.0 * fps).round() as u32, (25.0 * fps).round() as u32);
        let r = ed(&mut srv, json!([{ "op": "swap", "a": format!("V1@{a}f"), "b": format!("V1@{b}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("now at"), "{}", r.text);
        assert!(r.text.contains("moved"), "the moves are listed: {}", r.text);
        let v1 = srv.session.as_ref().unwrap().lane_clips(Lane::V1).to_vec();
        let mine: Vec<(u32, u32)> = v1.iter().filter(|c| c.start >= a).map(|c| (c.start, c.frames())).collect();
        // The 3 s clip now starts at 20 s; the 2 s clip lands where the 3 s one ended less 2 s.
        assert_eq!(mine, vec![(a, (3.0 * fps).round() as u32), (b + (fps).round() as u32, (2.0 * fps).round() as u32)]);
        // One undo puts both back.
        assert!(srv.session.as_mut().unwrap().undo());
        let back: Vec<u32> = srv.session.as_ref().unwrap().lane_clips(Lane::V1).iter().filter(|c| c.start >= a).map(|c| c.start).collect();
        assert_eq!(back, vec![a, b]);
        // A clip in a take is not swapped apart (V1@0 is linked to A1).
        let refused = ed(&mut srv, json!([{ "op": "swap", "a": "V1@0", "b": format!("V1@{a}f") }]));
        assert!(refused.is_error && refused.text.contains("refused"), "{}", refused.text);
    }

    /// A placement on one lane names only the clips on that lane: a clip on another lane that the
    /// span merely overlaps stays intact and is not in the reply.
    #[test]
    fn overwrite_reply_names_only_the_lane_the_placement_landed_on() {
        let mut srv = open_asset("test_25fps.mp4");
        srv.session.as_mut().unwrap().import(&asset("test_tone.wav")).expect("import tone");
        // The opened asset is already on V1 and A1 at 0 (a take), so only A2 is added to.
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "A2", "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("V1@") && !r.text.contains("A1@"), "other lanes are not named: {}", r.text);
        assert!(!r.text.contains("overwrote") && !r.text.contains("removed"), "nothing was overwritten: {}", r.text);
        // A second tone on A2 at the same start does overwrite what A2 holds, and A2 is what is named.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "A2", "at": "0s", "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("overwrote A2@0"), "{}", r.text);
        assert!(!r.text.contains("V1@") && !r.text.contains("A1@"), "{}", r.text);
        assert!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames() > 0, "V1 is intact");
    }

    /// An audio-only mp4 has no picture, whatever its extension says: a laneless place puts its
    /// sound on A1 alone, and a video lane request is refused as having no picture.
    #[test]
    fn audio_only_mp4_places_sound_alone_and_refuses_a_video_lane() {
        let mut srv = open_asset("test_av.mp4");
        srv.session.as_mut().unwrap().import(&asset("test_audio_only.mp4")).expect("import audio-only");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "at": "end" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A1@"), "the sound lands on A1: {}", r.text);
        assert!(!r.text.contains("V1@"), "no picture clip is made: {}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(Lane::V1).iter().all(|c| c.source != 1), "no video clip for the audio-only source");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "V1", "at": "end" }]));
        assert!(r.is_error && r.text.contains("has no picture"), "{}", r.text);
    }

    /// A clip whose link no other clip shares (what ungroup leaves) is not in a take, so it swaps.
    #[test]
    fn a_detached_clip_swaps_after_ungroup() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([{ "op": "ungroup", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let at = (20.0 * fps).round() as u32;
        let p = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "0s", "out": "2s", "lane": "V1", "audio": false }]));
        assert!(!p.is_error, "{}", p.text);
        let r = ed(&mut srv, json!([{ "op": "swap", "a": "V1@0", "b": format!("V1@{at}f") }]));
        assert!(!r.is_error, "a detached clip is not in a take: {}", r.text);
        assert!(!r.text.contains("in a take"), "{}", r.text);
    }

    /// `swap ripple:false` between a 4 s clip and a 1 s clip with a 1 s clip between them: the
    /// long clip's new slot is where the short one stood, so it fits only once the short one has
    /// left. The swap succeeds, moves nothing else, and one undo restores both.
    #[test]
    fn swap_without_ripple_fits_when_checked_on_the_final_positions() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let f = |sec: f64| (sec * fps).round() as u32;
        // Past the asset's own clip at 0s, so only these three are on the lane from 20s on.
        let place = |srv: &mut Server, at: &str, len: &str| {
            let p = ed(srv, json!([{ "op": "place", "source": 0, "at": at, "in": "0s", "length": len, "lane": "V1", "audio": false }]));
            assert!(!p.is_error, "{}", p.text);
        };
        place(&mut srv, "20s", "4s");
        place(&mut srv, "24s", "1s");
        place(&mut srv, "25s", "1s");
        let lane_from = |srv: &Server| -> Vec<(u32, u32)> {
            srv.session.as_ref().unwrap().lane_clips(Lane::V1).iter().filter(|c| c.start >= f(20.0)).map(|c| (c.start, c.frames())).collect()
        };
        let before = lane_from(&srv);
        // The 4 s clip would land on 25..29 s, where the 1 s clip stands until it has moved.
        let r = ed(&mut srv, json!([{ "op": "swap", "a": format!("V1@{}f", f(20.0)), "b": format!("V1@{}f", f(25.0)), "ripple": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("first now at 25.0s, second now at 20.0s"), "{}", r.text);
        // The 4 s clip now sits at 25 s, the 1 s clip at 20 s, and the middle clip has not moved.
        assert_eq!(lane_from(&srv), vec![(f(20.0), f(1.0)), (f(24.0), f(1.0)), (f(25.0), f(4.0))]);
        assert_ne!(before, lane_from(&srv));
        assert!(srv.session.as_mut().unwrap().undo());
        assert_eq!(lane_from(&srv), before, "one undo restores the lane");
    }

    #[test]
    fn place_length_sets_the_out_point_from_in() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        // 1 s into the 5 s asset, 2 s long, placed well past the existing clip.
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "1s", "length": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let last = srv.session.as_ref().unwrap().lane_clips(Lane::V1).last().unwrap().clone();
        assert_eq!(last.frames(), (2.0 * fps).round() as u32, "two seconds on the timeline");
        assert_eq!(last.in_frame, (1.0 * fps).round() as u32, "the in point is kept");
    }

    #[test]
    fn append_length_lays_one_copy_of_that_length() {
        let mut srv = open();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let before = srv.session.as_ref().unwrap().lane_clips(Lane::V1).len();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let v1 = srv.session.as_ref().unwrap().lane_clips(Lane::V1);
        assert_eq!(v1.len(), before + 1, "{}", r.text);
        assert_eq!(v1.last().unwrap().frames(), (1.0 * fps).round() as u32);
    }

    #[test]
    fn place_length_refuses_past_the_source_end_and_with_out() {
        let mut srv = open();
        // The 5 s asset: 4 s in plus 2 s runs past its end.
        let past = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "in": "4s", "length": "2s" }]));
        assert!(past.is_error, "{}", past.text);
        assert!(past.text.contains("runs past the source end"), "{}", past.text);
        let both = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "out": "2s", "length": "1s" }]));
        assert!(both.is_error && both.text.contains("give out or length, not both"), "{}", both.text);
        let loop_len = ed(&mut srv, json!([{ "op": "append", "source": 0, "until": "9s", "length": "1s" }]));
        assert!(loop_len.is_error && loop_len.text.contains("do not combine"), "{}", loop_len.text);
    }

    #[test]
    fn dissolve_crossfades_the_linked_sound() {
        let mut srv = linked_take();
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": format!("{len}f") }]));
        assert!(!r.is_error, "{}", r.text);
        let d = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(!d.is_error, "{}", d.text);
        assert!(d.text.contains("sound crossfaded A1@0 12f"), "{}", d.text);
        assert!(srv.session.as_ref().unwrap().fade_out_of(Lane::A1, 0) > 0);
    }

    #[test]
    fn dissolve_reply_says_the_overlap_and_the_length() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": format!("{len}f"), "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        let d = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(!d.is_error, "{}", d.text);
        assert!(d.text.contains(&format!("V1@{len} blends in over the tail of V1@0")), "{}", d.text);
        assert!(d.text.contains("timeline length unchanged"), "{}", d.text);
    }

    #[test]
    fn transition_sets_a_kind_with_a_direction_and_undoes() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": format!("{len}f"), "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        let t = ed(&mut srv, json!([{ "op": "transition", "clip": "V1@0", "kind": "push", "direction": "up", "length": "1s" }]));
        assert!(!t.is_error, "{}", t.text);
        assert!(t.text.contains("push-up V1@0→V1@"), "{}", t.text);
        let k = |srv: &Server| srv.session.as_ref().unwrap().transition_kind_of(Lane::V1, 0);
        assert_eq!(k(&srv), TransitionKind::Push(Dir::Up));
        let frames = srv.session.as_ref().unwrap().transition_out_of(Lane::V1, 0);
        assert_eq!(frames, 30, "1s at 30 fps");
        // Omitted length keeps the overlap; the full name is a kind too; a dissolve op resets the look.
        let t = ed(&mut srv, json!([{ "op": "transition", "clip": "V1@0", "kind": "wipe-right" }]));
        assert!(!t.is_error, "{}", t.text);
        assert_eq!(k(&srv), TransitionKind::Wipe(Dir::Right));
        assert_eq!(srv.session.as_ref().unwrap().transition_out_of(Lane::V1, 0), frames);
        let t = ed(&mut srv, json!([{ "op": "transition", "clip": "V1@0", "kind": "dip-black" }]));
        assert!(!t.is_error, "{}", t.text);
        assert_eq!(k(&srv), TransitionKind::DipBlack);
        for bad in [
            json!({ "op": "transition", "clip": "V1@0", "kind": "zoom", "direction": "left" }),
            json!({ "op": "transition", "clip": "V1@0", "kind": "spin" }),
            json!({ "op": "transition", "clip": "V1@0", "kind": "wipe", "direction": "sideways" }),
        ] {
            let r = ed(&mut srv, json!([bad]));
            assert!(r.is_error, "{}", r.text);
        }
        assert_eq!(k(&srv), TransitionKind::DipBlack, "refusals change nothing");
        let d = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(!d.is_error, "{}", d.text);
        assert_eq!(k(&srv), TransitionKind::Dissolve);
        assert!(srv.session.as_mut().unwrap().undo());
        assert_eq!(k(&srv), TransitionKind::DipBlack);
    }

    #[test]
    fn dissolve_names_a_gap_and_a_missing_next_clip() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        let len = first_len(&srv);
        let last = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(last.is_error && last.text.contains("last clip on V1"), "{}", last.text);
        let far = len + 30;
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": format!("{far}f"), "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let gap = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(gap.is_error && gap.text.contains("a gap of 30f lies between V1@0"), "{}", gap.text);
        assert!(srv.session.as_ref().unwrap().transition_out_of(Lane::V1, 0) == 0, "refusal changes nothing");
    }

    #[test]
    fn dissolve_on_an_audio_lane_points_at_crossfade() {
        let mut srv = linked_take();
        let d = ed(&mut srv, json!([{ "op": "dissolve", "clip": "A1@0", "frames": 12 }]));
        assert!(d.is_error && d.text.contains("use crossfade"), "{}", d.text);
    }

    #[test]
    fn append_audio_false_is_picture_only() {
        let mut srv = open();
        let (v, a) = (clip_count(&srv, Lane::V1), clip_count(&srv, Lane::A1));
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(clip_count(&srv, Lane::V1), v + 1, "picture appended on V1");
        assert_eq!(clip_count(&srv, Lane::A1), a, "no sound appended");
    }

    #[test]
    fn append_pairs_sound_by_default_and_audio_true_is_the_same() {
        let mut srv = open();
        let (v, a) = (clip_count(&srv, Lane::V1), clip_count(&srv, Lane::A1));
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(clip_count(&srv, Lane::V1), v + 1);
        assert_eq!(clip_count(&srv, Lane::A1), a + 1, "sound paired");
    }

    fn clip_count(srv: &Server, lane: Lane) -> usize {
        srv.session.as_ref().unwrap().lane_clips(lane).len()
    }

    #[test]
    fn move_with_to_names_the_right_argument() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "to": "-10s" }]));
        assert!(r.is_error && r.text.contains("missing `at` (move takes `at`, not `to`)"), "{}", r.text);
    }

    #[test]
    fn undo_steps_must_be_a_whole_number_in_words() {
        let mut srv = open();
        let u = run_undo(&mut srv, &json!({ "steps": 1.5 }));
        assert!(u.is_error && u.text.contains("a whole number of at least 1, not 1.5"), "{}", u.text);
    }

    #[test]
    fn undo_steps_fraction_is_refused_by_the_argument_check() {
        let t = tools().into_iter().find(|t| t.name == "undo").expect("undo tool");
        let err = crate::rpc::check_args(&t, &json!({ "steps": 1.5 })).expect_err("refused");
        assert_eq!(err, "argument `steps` for undo must be an integer, got 1.5; for example 1: Batches to take back or redo (default 1).");
    }

    #[test]
    fn speed_out_of_range_echoes_a_short_number() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 1e300 }]));
        assert!(r.is_error && r.text.contains("speed 1e300 is outside"), "{}", r.text);
    }

    #[test]
    fn every_op_in_the_dispatcher_has_a_key_list() {
        for name in OWN_OPS.iter().chain(FX_OPS).chain(SUB_OPS) {
            assert!(accepted_keys(name).is_some(), "{name} has no accepted-key list");
        }
    }

    #[test]
    fn an_unknown_key_is_refused_by_name_with_the_keys_the_op_takes() {
        let mut srv = empty_with_av();
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "0s", "clipp": "x" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(
            r.text.contains("unknown key `clipp`; place takes source, lane, at, in, out, length, stretch, audio, audio_lane, overwrite"),
            "{}",
            r.text
        );
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth, "a refused batch changes nothing");
    }

    #[test]
    fn an_unknown_key_suggests_the_close_name() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "tos": "1s" }]));
        assert!(r.is_error && r.text.contains("(did you mean `to`?)"), "{}", r.text);
    }

    #[test]
    fn a_guessed_key_still_gets_the_missing_argument_hint() {
        let mut srv = linked_take();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "to": "-10s" }]));
        assert!(r.is_error && r.text.contains("missing `at` (move takes `at`, not `to`)"), "{}", r.text);
    }

    #[test]
    fn an_occupied_explicit_video_lane_refuses_and_overwrite_lists_what_it_replaced() {
        let mut srv = open();
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("would overwrite V1@0") && r.text.contains("overwrite:true"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refused placement changes nothing");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "audio": false, "overwrite": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("overwrote V1@0"), "{}", r.text);
    }

    #[test]
    fn an_occupied_explicit_audio_lane_refuses_and_names_the_clip() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A1", "at": "0s" }]));
        assert!(r.is_error && r.text.contains("would overwrite A1@0"), "{}", r.text);
    }

    #[test]
    fn append_with_no_at_starts_at_the_end_of_v1_not_the_timeline() {
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A1", "at": "0s" }])).is_error);
        // V1 is empty, A1 runs to the end: the picture alone starts at 0 on V1.
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].start, 0, "{}", r.text);
    }

    #[test]
    fn append_refuses_a_busy_sound_lane_and_suggests_audio_false() {
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A1", "at": "0s" }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(r.is_error && r.text.contains("would overlap A1@0"), "{}", r.text);
        assert!(r.text.contains("audio:false"), "{}", r.text);
        assert!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).is_empty(), "a refused append changes nothing");
    }

    #[test]
    fn ripple_delete_lists_the_shifted_clips_old_to_new() {
        let mut srv = open();
        let half = first_len(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        let want = format!("shifted: V1@{half}→V1@0, A1@{half}→A1@0");
        assert!(r.text.contains(&want), "{}", r.text);
    }

    #[test]
    fn delete_of_a_detached_sound_refuses_when_the_ripple_would_take_the_picture_under_it() {
        // Two takes cut from the same source range. The first sound is detached, so its picture
        // V1@0 belongs to no take; the later take's picture V1@len would move onto it. The cut
        // would remove V1@0, so the delete refuses and changes nothing.
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "ungroup", "clip": "A1@0" }])).is_error);
        let before = snapshot(&srv);
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "delete", "clip": "A1@0" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("refused"), "{}", r.text);
        assert!(r.text.contains("V1@0"), "{}", r.text);
        assert!(r.text.contains("lift A1@0"), "{}", r.text);
        assert!(r.text.contains("close_gaps {lane:\"V1\"}"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refused delete changes nothing");
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].start, 0);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start, len);
    }

    #[test]
    fn ripple_delete_of_a_detached_sound_refuses_the_same_overwrite() {
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "ungroup", "clip": "A1@0" }])).is_error);
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "A1@0" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("ripple_delete A1@0 refused"), "{}", r.text);
        assert_eq!(snapshot(&srv), before);
    }

    #[test]
    fn lift_both_then_close_gaps_is_the_way_out_of_the_refusal() {
        // The refusal's own recipe: lift the sound and the picture in its way, then close the gap.
        // The later take rides along when its lane is empty over the stretch, so the take stays whole.
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "ungroup", "clip": "A1@0" }])).is_error);
        let len = first_len(&srv);
        let r = ed(
            &mut srv,
            json!([
                { "op": "lift", "clip": "A1@0" },
                { "op": "lift", "clip": "V1@0" },
                { "op": "close_gaps", "lane": "V1" },
                { "op": "close_gaps", "lane": "A1" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1).len(), 1, "{}", r.text);
        assert_eq!(s.lane_clips(Lane::V1)[0].start, 0, "{}", r.text);
        assert_eq!(s.lane_clips(Lane::A1)[0].start, 0, "{}", r.text);
        assert_eq!(s.lane_clips(Lane::V1)[0].frames(), len);
    }

    /// Two takes cut from the same source, the first one's sound detached: V1@0 is a picture that
    /// belongs to no take, A1@0 the sound beside it; the later take V1@len + A1@len stays whole.
    fn detached_pair_then_take() -> (Server, u32) {
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "ungroup", "clip": "A1@0" }])).is_error);
        let len = first_len(&srv);
        (srv, len)
    }

    #[test]
    fn move_insert_of_a_detached_sound_refuses_when_its_slot_would_take_the_picture_under_it() {
        // The insert moves the later take up into the slot the sound leaves, and the slot cut would
        // take V1@0 with it: refuse before anything moves.
        let (mut srv, len) = detached_pair_then_take();
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "A1@0", "at": format!("{len}f"), "to_lane": "A1", "insert": true }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("move A1@0 insert to A1@"), "{}", r.text);
        assert!(r.text.contains("refused: the ripple would close over V1@0"), "{}", r.text);
        assert!(r.text.contains("move A1@0 without insert to leave the gap instead"), "{}", r.text);
        assert!(r.text.contains("close_gaps {lane:\"V1\"}"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refused move changes nothing");
    }

    #[test]
    fn move_insert_of_a_detached_picture_refuses_when_its_slot_would_take_the_sound_under_it() {
        // The mirror: the picture leaves its slot, and the cut would take A1@0 under it.
        let (mut srv, len) = detached_pair_then_take();
        let before = snapshot(&srv);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": format!("{len}f"), "insert": true }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("refused: the ripple would close over A1@0"), "{}", r.text);
        assert!(r.text.contains("close_gaps {lane:\"A1\"}"), "{}", r.text);
        assert_eq!(snapshot(&srv), before, "a refused move changes nothing");
    }

    #[test]
    fn move_without_insert_moves_the_detached_sound_the_refusal_names() {
        // The refusal's first way out: the slide leaves the gap and ripples nothing, so the picture stays.
        let (mut srv, len) = detached_pair_then_take();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "A1@0", "at": format!("{}f", 2 * len) }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[0].start, 0, "{}", r.text);
        assert_eq!(s.lane_clips(Lane::V1).len(), 2, "{}", r.text);
    }

    #[test]
    fn ripple_delete_of_the_later_take_shifts_nothing() {
        // The later of two identical takes goes: the earlier one stays where it is, so nothing moves.
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": format!("V1@{len}") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("shifted:"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).len(), 1, "{}", r.text);
    }

    #[test]
    fn remove_time_lists_both_lanes_when_takes_share_a_source_range() {
        let mut srv = empty_with_av();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }])).is_error);
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "0f", "duration": format!("{len}f") }]));
        assert!(!r.is_error, "{}", r.text);
        let want = format!("shifted: V1@{len}→V1@0, A1@{len}→A1@0");
        assert!(r.text.contains(&want), "{}", r.text);
    }

    #[test]
    fn shifted_lists_cap_at_twenty_and_count_the_rest() {
        let mut srv = open();
        let len = first_len(&srv);
        let cuts: Vec<Value> = (1..=12).map(|i| i * 10).filter(|&f| f < len).map(|f| json!({ "op": "split", "at": format!("{f}f") })).collect();
        assert!(!ed(&mut srv, Value::Array(cuts.clone())).is_error);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        // Every cut piece after the first shifts on V1 and on A1.
        let total = 2 * cuts.len();
        let shown = r.text.lines().find(|l| l.starts_with("   shifted: ")).expect("shifted line");
        assert_eq!(shown.matches('→').count(), total.min(SHIFT_CAP), "{shown}");
        if total > SHIFT_CAP {
            assert!(shown.ends_with(&format!("+{} more", total - SHIFT_CAP)), "{shown}");
        }
    }

    #[test]
    fn freeze_at_a_clips_last_frame_goes_after_it_with_no_sliver() {
        let dir = TestDir::new("freeze-last-frame");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let len = first_len(&srv);
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": format!("{}f", len - 1), "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("holding frame"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v1 = s.lane_clips(Lane::V1);
        assert_eq!(v1.len(), 2, "{}", r.text);
        assert_eq!(v1[0].frames(), len, "the clip is not cut: {}", r.text);
        assert_eq!(v1[1].start, len, "the still starts where the clip ends");
    }

    #[test]
    fn a_still_with_in_out_names_its_length_and_how_to_change_it() {
        let mut srv = open();
        let still = asset("test_still.png");
        srv.session.as_mut().unwrap().import(&still).expect("import still");
        let n = srv.session.as_ref().unwrap().sources().len() - 1;
        let r = ed(&mut srv, json!([{ "op": "place", "source": n, "lane": "V1", "at": "0s", "in": "0s", "out": "1s" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("default 5.0 s") && r.text.contains("length"), "{}", r.text);
    }

    /// Two copies of the test asset back to back on V1 and A1: V1@0 and V1@(first_len).
    fn two_takes() -> Server {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    #[test]
    fn speed_without_ripple_refuses_and_names_ripple() {
        let mut srv = two_takes();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5 }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("ripple:true"), "{}", r.text);
    }

    #[test]
    fn speed_ripple_slower_pushes_the_later_takes_and_is_one_undo_step() {
        let mut srv = two_takes();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let s0 = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].clone();
        let next_before = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start;
        assert_eq!(next_before, s0.end());
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.ledger.len(), steps + 1, "one undo step for the batch");
        let s = srv.session.as_ref().unwrap();
        let slow = s.lane_clips(Lane::V1)[0].clone();
        assert_eq!(slow.frames(), 2 * s0.frames(), "half speed doubles the length");
        assert_eq!(s.lane_clips(Lane::V1)[1].start, slow.end(), "the next V1 take moved to the new end");
        assert_eq!(s.lane_clips(Lane::A1)[1].start, slow.end(), "its sound partner moved with it");
        assert!(fps > 0.0);
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[0].frames(), s0.frames(), "undo restores the speed");
        assert_eq!(s.lane_clips(Lane::V1)[1].start, next_before, "and the later take");
    }

    #[test]
    fn speed_ramp_sets_the_length_from_the_ramp_and_clearing_restores_the_constant() {
        let mut srv = two_takes();
        let s0 = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].clone();
        let len = s0.len();
        let keys = [engine::project::SpeedKey { at: 0, speed: Speed::NORMAL }, engine::project::SpeedKey { at: 60, speed: Speed::from_permille(2000) }];
        let want = engine::project::ramp_frames(&keys, len);
        assert!(want < len, "a speeding-up ramp shortens a {len}-frame clip, to {want}");
        let steps = srv.ledger.len();
        let r = ed(
            &mut srv,
            json!([{ "op": "speed_ramp", "clip": "V1@0", "keys": [
                {"at": "0f", "speed": 1.0}, {"at": "2s", "speed": 2.0}
            ], "ripple": true }]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("speed ramp V1@0 (5.000s → 3.000s)"), "{}", r.text);
        assert!(r.text.contains("→"), "the reply names the length change: {}", r.text);
        assert_eq!(srv.ledger.len(), steps + 1, "one undo step for the batch");
        let s = srv.session.as_ref().unwrap();
        let ramped = s.lane_clips(Lane::V1)[0].clone();
        assert_eq!(ramped.speed_keys, keys.to_vec(), "the keys are on the clip");
        assert_eq!(ramped.frames(), want, "the length is the ramp's footprint");
        assert_eq!(s.lane_clips(Lane::V1)[1].start, ramped.end(), "the later take closed up to the new end");
        // Clearing: the constant speed comes back, which was real time.
        let r = ed(&mut srv, json!([{ "op": "speed_ramp", "clip": "V1@0", "keys": [], "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let cleared = s.lane_clips(Lane::V1)[0].clone();
        assert!(cleared.speed_keys.is_empty());
        assert_eq!(cleared.frames(), s0.frames(), "a cleared ramp is the constant speed again");
    }

    #[test]
    fn speed_ramp_refuses_bad_keys_and_changes_nothing() {
        let mut srv = two_takes();
        let before = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].clone();
        for (keys, want) in [
            (json!([{"at": "0s", "speed": 9.0}]), "outside 0.25–4"),
            (json!([{"at": "2s", "speed": 1.0}, {"at": "1s", "speed": 2.0}]), "increasing order"),
            (json!([{"at": "0s"}]), "needs `speed`"),
            (json!("fast"), "must be a list"),
        ] {
            let r = ed(&mut srv, json!([{ "op": "speed_ramp", "clip": "V1@0", "keys": keys, "ripple": true }]));
            assert!(r.is_error, "{keys} should be refused");
            assert!(r.text.contains(want), "{keys}: {}", r.text);
        }
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0], before);
    }

    #[test]
    fn speed_ripple_faster_closes_the_gap() {
        let mut srv = two_takes();
        let s0 = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].clone();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 2, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let fast = s.lane_clips(Lane::V1)[0].clone();
        assert_eq!(fast.end(), s0.frames() / 2 + u32::from(s0.frames() % 2 == 1), "2x halves the length");
        assert_eq!(s.lane_clips(Lane::V1)[1].start, fast.end(), "the gap closed");
        assert_eq!(s.lane_clips(Lane::A1)[1].start, fast.end(), "the sound closed it too");
    }

    #[test]
    fn ripple_delete_moves_titles_after_the_cut_with_the_picture() {
        let mut srv = two_takes();
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": "6s", "duration": "1s", "text": "Porto" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = srv.session.as_ref().unwrap().titles()[0].start;
        let cut = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].clone();
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.titles()[0].start, before - cut.frames(), "the title moved with the picture");
        assert_eq!(s.titles()[0].start, (1.0 * fps).round() as u32);
        assert!(r.text.contains("titles moved"), "{}", r.text);
    }

    #[test]
    fn delete_moves_a_title_after_the_removed_clip() {
        let mut srv = two_takes();
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": "6s", "duration": "1s", "text": "Porto" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = srv.session.as_ref().unwrap().titles()[0].start;
        let len = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames();
        let r = ed(&mut srv, json!([{ "op": "delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().titles()[0].start, before - len);
    }

    fn s1() -> Lane {
        Lane::new(LaneKind::Subtitle, 0)
    }

    /// The text of each cue S1 shows, in timeline order.
    fn shown(srv: &Server, lane: Lane) -> Vec<String> {
        srv.session.as_ref().unwrap().sub_lane_cues(lane).into_iter().map(|c| c.text).collect()
    }

    /// One four-second sentence, "alpha beta gamma delta", placed on S1 at 0.
    fn sentence(tag: &str) -> (Server, TestDir) {
        let mut srv = open();
        let dir = TestDir::new(&format!("cues-{tag}"));
        let a = dir.path().join("a.srt");
        std::fs::write(&a, "1\n00:00:00,000 --> 00:00:04,000\nalpha beta gamma delta\n").unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        (srv, dir)
    }

    /// Cutting a span out of a sentence keeps the words either side and drops
    /// the ones inside: "beta" lies in 1 s to 2 s, so "alpha" and "gamma delta"
    /// are what is left. Undo brings the sentence back whole.
    #[test]
    fn remove_time_through_a_sentence_keeps_the_words_outside_the_span() {
        let (mut srv, _dir) = sentence("remove");
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(shown(&srv, s1()), ["alpha", "gamma delta"], "{}", r.text);

        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(shown(&srv, s1()), ["alpha beta gamma delta"], "undo restores the sentence");
    }

    /// Opening room inside a sentence divides its words once, with no "(part n/2)"
    /// copies of the whole sentence: "alpha" stays where it was and the rest moves on.
    #[test]
    fn insert_time_through_a_sentence_divides_its_words() {
        let (mut srv, _dir) = sentence("insert");
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1s", "duration": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(shown(&srv, s1()), ["alpha", "beta gamma delta"], "{}", r.text);
        assert!(shown(&srv, s1()).iter().all(|t| !t.contains("alpha beta")), "no copy of the whole sentence");
    }

    /// A cue that runs past its placement's window is named by the part the
    /// placement shows, not by times before it starts or after it ends: the
    /// sentence over a window trimmed to 0 s to 2 s is "0.000s–2.000s".
    #[test]
    fn a_cue_past_its_window_is_named_by_the_part_shown() {
        let (mut srv, _dir) = sentence("clamp");
        let r = ed(&mut srv, json!([{ "op": "sub_trim", "clip": "S1@0", "edge": "out", "to": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("trimmed: S1 cue 1 'alpha beta gamma delta' → 'alpha' (was 0.000s–2.000s, now 0.000s–1.000s)"), "{}", r.text);
        assert!(!r.text.contains("-"), "no negative time in the reply: {}", r.text);
    }

    /// A cue shown by two placements of one track is listed once in the reply.
    #[test]
    fn a_cue_shown_twice_is_listed_once() {
        let (mut srv, _dir) = sentence("twice");
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S2", "at": "0f" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(r.text.matches("'alpha beta gamma delta'").count(), 1, "{}", r.text);
    }

    /// A ripple that moves the captions but leaves the narration under them
    /// warns; the same ripple with the narration's lane in its scope does not.
    #[test]
    fn a_ripple_that_leaves_the_sound_under_moved_captions_warns() {
        let (mut srv, _dir) = sentence("drift");
        // Split at 2 s, then take the narration off its picture (both halves): a take can only move whole.
        let r = ed(
            &mut srv,
            json!([{ "op": "split", "at": "2s" }, { "op": "ungroup", "clip": "V1@0" }, { "op": "ungroup", "clip": "V1@60" }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0", "lanes": ["V1", "S1"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("left in place"), "{}", r.text);
        assert!(
            r.text.contains("captions on S1 moved −2.000s but A1 (sound under them) stayed"),
            "{}",
            r.text
        );
        assert!(r.text.contains("add lanes:[…A1] or undo"), "{}", r.text);

        let (mut srv, _dir) = sentence("linked");
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }, { "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("may no longer match"), "the linked narration moves with the picture: {}", r.text);
    }

    /// Captions placed on S1 from one SRT cue, `start` to `end` seconds, as `srt`'s text.
    fn captions_from(tag: &str, srt: &str) -> (Server, TestDir) {
        let mut srv = open();
        let dir = TestDir::new(&format!("cues-{tag}"));
        let a = dir.path().join("a.srt");
        std::fs::write(&a, srt).unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        (srv, dir)
    }

    /// Adds an audio lane A2 with the asset's sound looped back to back to six seconds: a clip at 0
    /// that runs to 5 s and one at 5 s to 6 s.
    fn sound_bed_on_a2(srv: &mut Server) {
        let r = ed(srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(srv, json!([{ "op": "append", "source": 0, "lane": "A2", "until": "6s" }]));
        assert!(!r.is_error, "{}", r.text);
    }

    /// A one-word cue that a cut at 3 s runs through (0 s to 4 s) is shown once, on the longer side
    /// (the head, 3 s), and an insert at 3 s leaves no copy on the far side.
    #[test]
    fn insert_time_through_a_one_word_caption_shows_the_word_once() {
        let (mut srv, _dir) = captions_from("one-word", "1\n00:00:00,000 --> 00:00:04,000\nOnewordcue\n");
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "3s", "duration": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(shown(&srv, s1()), ["Onewordcue"], "{}", r.text);
    }

    /// (a) V1 in two clips with A1 linked to them, an unlinked sound bed on A2 under captions over 2 s to 6 s.
    /// Rippling V1's first clip moves the captions 2 s and leaves A2 where it was: the warning names A2.
    #[test]
    fn a_ripple_warns_when_an_unlinked_sound_bed_under_moved_captions_stays() {
        let (mut srv, _dir) = captions_from("drift-a", "1\n00:00:02,000 --> 00:00:06,000\nalpha beta gamma delta\n");
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        sound_bed_on_a2(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains("captions on S1 moved −2.000s but A2 (sound under them) stayed"),
            "{}",
            r.text
        );
        assert!(!r.text.contains("A1 (sound under them)"), "the linked A1 moved with the picture: {}", r.text);
    }

    /// (b) The sound is A1 under V1's clips, and the take is detached (a take cannot move one track
    /// alone): the ripple names its lanes as V1 and S1, so A1 is left out, the captions move and the
    /// speech under them stays. The warning names A1.
    #[test]
    fn a_ripple_with_explicit_lanes_warns_when_the_sound_is_left_out() {
        let (mut srv, _dir) = captions_from("drift-b", "1\n00:00:02,000 --> 00:00:06,000\nalpha beta gamma delta\n");
        let r = ed(
            &mut srv,
            json!([{ "op": "split", "at": "2s" }, { "op": "ungroup", "clip": "V1@0" }, { "op": "ungroup", "clip": "V1@60" }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0", "lanes": ["V1", "S1"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(
            r.text.contains("captions on S1 moved −2.000s but A1 (sound under them) stayed"),
            "{}",
            r.text
        );
    }

    /// (c) A locked sound bed on A2 (a clip from 0 to 5 s, starting before the cut) under captions the
    /// remove moves: the warning names A2 even though its clip begins before the edit.
    #[test]
    fn a_remove_warns_when_a_locked_sound_bed_that_starts_before_the_cut_stays() {
        let (mut srv, _dir) = captions_from("drift-c", "1\n00:00:02,000 --> 00:00:06,000\nalpha beta gamma delta\n");
        sound_bed_on_a2(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "A2", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "1s" }]));
        assert!(r.is_error && r.text.contains("lane A2 is locked") && r.text.contains("pass lanes without it"), "{}", r.text);
    }

    /// Captions that end before the cut do not move, so the sound under them is not drift.
    #[test]
    fn a_ripple_over_captions_that_do_not_move_does_not_warn() {
        let (mut srv, _dir) = captions_from("drift-none", "1\n00:00:00,000 --> 00:00:01,000\nalpha\n");
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        sound_bed_on_a2(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@60" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("captions on"), "{}", r.text);
    }

    #[test]
    fn fade_clamp_is_reported_for_a_short_clip() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "10s", "audio": false, "in": "0s", "out": "0.88s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@10s", "out": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("out clamped from 1.0s to the clip's 0.87s"), "{}", r.text);
    }

    #[test]
    fn dry_run_leaves_a_clean_project_clean() {
        let mut srv = open();
        let s = srv.session.as_ref().unwrap();
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
        assert!(!crate::tools::project::dirty(&srv));
        let r = run_edit(&mut srv, &json!({ "ops": [{ "op": "split", "at": "1s" }], "dry_run": true }));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("dry run"), "{}", r.text);
        assert!(!crate::tools::project::dirty(&srv), "a dry run leaves the project clean");
        assert!(srv.ledger.is_empty(), "and keeps no ledger entry");
    }

    #[test]
    fn append_until_loops_the_source_and_trims_the_last_copy() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let until = (12.0 * fps).round() as u32;
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "lane": "A2", "until": "12s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("loop src:0"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let a2 = s.lane_clips(Lane::new(LaneKind::Audio, 1)).to_vec();
        assert!(a2.len() >= 2, "the copy repeats: {}", r.text);
        assert_eq!(a2[0].start, 0);
        for w in a2.windows(2) {
            assert_eq!(w[1].start, w[0].end(), "back to back");
        }
        assert_eq!(a2.last().unwrap().end(), until, "the last copy ends at the time asked");
        assert!(a2.last().unwrap().frames() <= a2[0].frames(), "the last copy is trimmed");
        assert!(r.text.contains("last copy trimmed"), "{}", r.text);
    }

    /// The flags of the waveform picture a visualizer on a sound clip placed (the first picture
    /// clip on a video lane above V1 that carries any).
    fn picture_flags(s: &PlaybackSession) -> u8 {
        s.lanes()
            .into_iter()
            .filter(|l| l.kind == LaneKind::Video && l.ord > 0)
            .flat_map(|l| s.lane_clips(l).iter().map(|c| c.visualizer).collect::<Vec<_>>())
            .find(|&v| v != 0)
            .unwrap_or(0)
    }

    #[test]
    fn visualizer_sets_on_style_and_fast_in_one_step() {
        // The visualizer is set on the sound clip; its waveform picture carries the flags.
        let mut srv = open();
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "ring", "fast": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("style ring, fast"), "{}", r.text);
        let v = picture_flags(srv.session.as_ref().unwrap());
        assert_eq!(v & 1, 1, "bit 0 on");
        assert_eq!(engine::decode::viz_style(v), 2, "ring");
        assert_eq!(v & engine::decode::VIZ_FAST, engine::decode::VIZ_FAST, "fast");
        assert_eq!(srv.ledger.len(), steps + 1, "one undo step");
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "wave" }]));
        assert!(!r.is_error, "{}", r.text);
        let v = picture_flags(srv.session.as_ref().unwrap());
        assert_eq!(engine::decode::viz_style(v), 3, "style changed, fast kept");
        assert_ne!(v & engine::decode::VIZ_FAST, 0);
    }

    #[test]
    fn visualizer_refuses_unknown_style_and_strays() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "spiral" }]));
        assert!(r.is_error && r.text.contains("fill, ribbon, ring, wave"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "style": "ring" }]));
        assert!(r.is_error && r.text.contains("on:true"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "colour": "red" }]));
        assert!(r.is_error && r.text.contains("unknown key"), "{}", r.text);
    }

    fn lane_clips_of(srv: &Server, l: Lane) -> Vec<(u32, u32, Option<u32>)> {
        srv.session.as_ref().unwrap().lane_clips(l).iter().map(|c| (c.start, c.frames(), c.link)).collect()
    }

    #[test]
    fn music_delete_leaves_picture_and_sound_alone() {
        let mut srv = open();
        let built = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "audio" },
                { "op": "append", "source": 0, "lane": "A2" },
                { "op": "append", "source": 0, "lane": "A2" }
            ]),
        );
        assert!(!built.is_error, "{}", built.text);
        let a2 = parse::lane("A2").unwrap();
        let picture = lane_clips_of(&srv, Lane::V1);
        let sound = lane_clips_of(&srv, Lane::A1);
        let music = lane_clips_of(&srv, a2);
        assert!(music.len() >= 2, "{:?}", music);
        let r = ed(&mut srv, json!([{ "op": "delete", "clip": "A2@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::V1), picture, "V1 moved: {}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::A1), sound, "A1 moved: {}", r.text);
        assert_eq!(lane_clips_of(&srv, a2).len(), music.len() - 1, "{}", r.text);
    }

    #[test]
    fn broll_speed_ripple_does_not_touch_sound() {
        let mut srv = open();
        let built = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "audio" },
                { "op": "append", "source": 0, "lane": "A2" },
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": 0, "lane": "V2", "at": "0s", "audio": false }
            ]),
        );
        assert!(!built.is_error, "{}", built.text);
        let picture = lane_clips_of(&srv, Lane::V1);
        let sound = lane_clips_of(&srv, Lane::A1);
        let music = lane_clips_of(&srv, parse::lane("A2").unwrap());
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V2@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::V1), picture, "{}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::A1), sound, "{}", r.text);
        assert_eq!(lane_clips_of(&srv, parse::lane("A2").unwrap()), music, "{}", r.text);
    }

    #[test]
    fn ripple_delete_of_a_picture_clip_keeps_the_take_linked() {
        let mut srv = open();
        let built = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": 0, "lane": "V2", "at": "0s", "audio": false }
            ]),
        );
        assert!(!built.is_error, "{}", built.text);
        let picture = lane_clips_of(&srv, Lane::V1);
        let sound = lane_clips_of(&srv, Lane::A1);
        let link = sound[0].2;
        assert!(link.is_some(), "the fixture's V1/A1 take is linked");
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V2@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("detached"), "{}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::V1), picture, "{}", r.text);
        assert_eq!(lane_clips_of(&srv, Lane::A1), sound, "{}", r.text);
    }

    #[test]
    fn linked_trim_moves_both_halves_to_one_time() {
        let mut srv = open();
        let len = first_len(&srv);
        let half = len / 2;
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "A1@0", "edge": "out", "to": format!("{half}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(with V1@0)"), "{}", r.text);
        assert!(!r.text.contains("shifted: V1@0→A1@0"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1)[0].end();
        let a = s.lane_clips(Lane::A1)[0].end();
        assert_eq!(v, a, "picture and sound ends agree: {}", r.text);
        assert_eq!(v, half, "{}", r.text);
    }

    #[test]
    fn split_on_named_lanes_cuts_only_them() {
        let mut srv = open();
        let len = first_len(&srv);
        let half = len / 2;
        let built = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }, { "op": "append", "source": 0, "lane": "A2" }]));
        assert!(!built.is_error, "{}", built.text);
        let music = lane_clips_of(&srv, parse::lane("A2").unwrap());
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f"), "lanes": ["V1"] }]));
        assert!(!r.is_error, "{}", r.text);
        // Explicit lanes are honoured exactly: A1 (linked to V1) stays whole, and the take is unlinked at the cut.
        assert!(r.text.contains("kept whole: A1, A2"), "{}", r.text);
        assert!(r.text.contains("detached at the cut"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).len(), 2);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::A1).len(), 1);
        assert_eq!(lane_clips_of(&srv, parse::lane("A2").unwrap()), music, "{}", r.text);
    }

    #[test]
    fn dissolve_fades_sound_to_silence_when_no_sound_meets_it() {
        let mut srv = open();
        let built = ed(&mut srv, json!([{ "op": "append", "source": 0, "lane": "V1", "audio": false }]));
        assert!(!built.is_error, "{}", built.text);
        let r = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("faded out 12f to silence"), "{}", r.text);
        assert!(!r.text.contains("was not crossfaded"), "{}", r.text);
    }

    #[test]
    fn crossfade_to_silence_is_applied_not_refused() {
        let mut srv = open();
        let built = ed(&mut srv, json!([{ "op": "append", "source": 0, "lane": "V1", "audio": false }]));
        assert!(!built.is_error, "{}", built.text);
        let r = ed(&mut srv, json!([{ "op": "crossfade", "clip": "A1@0", "frames": 12 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("faded out 12f to silence"), "{}", r.text);
    }
}

/// Unit tests for the ripple rule, the lock closure, no-op batches, explicit split lanes,
/// lane-kind refusals and the left-behind list. Each opens the test asset (V1 and A1 linked).
#[cfg(test)]
mod ripple_rule_tests {
    use super::*;
    use std::path::Path;

    fn open() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        let sess = PlaybackSession::open(p).expect("open test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    fn clip_count(srv: &Server, lane: Lane) -> usize {
        srv.session.as_ref().unwrap().lane_clips(lane).len()
    }

    fn len_of(srv: &Server) -> u32 {
        srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames()
    }

    fn ledger(srv: &Server) -> usize {
        srv.ledger.len()
    }

    #[test]
    fn locked_partner_refuses_a_ripple_and_moves_nothing() {
        // A1 is linked to V1, so a ripple from V1 reaches it: locked, the op refuses (finding 1).
        let mut srv = open();
        let half = len_of(&srv) / 2;
        assert!(!ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f") }])).is_error);
        let before = ledger(&srv);
        let r = ed(&mut srv, json!([
            { "op": "lane_flags", "lane": "A1", "locked": true },
            { "op": "insert_time", "at": "0.1s", "duration": "1s" }
        ]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("A1 is locked") || r.text.contains("lane A1 is locked"), "{}", r.text);
        assert_eq!(ledger(&srv), before);
    }

    #[test]
    fn locked_partner_refuses_a_cut_regions_default() {
        let mut srv = open();
        let r = ed(&mut srv, json!([
            { "op": "lane_flags", "lane": "A1", "locked": true },
            { "op": "cut_regions", "regions": [["0.2s", "0.4s"]] }
        ]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("locked"), "{}", r.text);
    }

    #[test]
    fn deleting_a_whole_take_reports_no_unlink() {
        // Finding 2: the deleted pair is gone; the reply must not say its link was broken.
        let mut srv = open();
        let half = len_of(&srv) / 2;
        let r = ed(&mut srv, json!([
            { "op": "split", "at": format!("{half}f") },
            { "op": "delete", "clip": format!("V1@{half}") }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("detached"), "{}", r.text);
    }

    #[test]
    fn past_the_end_remove_time_is_no_change_and_no_undo_step() {
        // Finding 3: the no-op batch is rolled back and the reply says why.
        let mut srv = open();
        let before = ledger(&srv);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1000s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("no change:"), "{}", r.text);
        assert_eq!(ledger(&srv), before);
    }

    #[test]
    fn empty_lanes_is_named_and_explicit_lanes_keep_partners_whole() {
        // Finding 8.
        let mut srv = open();
        let half = len_of(&srv) / 2;
        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f"), "lanes": [] }]));
        assert!(r.is_error && r.text.contains("`lanes` is empty"), "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "split", "at": format!("{half}f"), "lanes": ["V1"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("kept whole: A1"), "{}", r.text);
        assert!(r.text.contains("detached at the cut"), "{}", r.text);
        assert_eq!(clip_count(&srv, Lane::V1), 2, "{}", r.text);
        assert_eq!(clip_count(&srv, Lane::A1), 1, "{}", r.text);
    }

    #[test]
    fn kind_mismatches_are_refused() {
        // Finding 9.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "color", "clip": "A1@0", "brightness": 0.1 }]));
        assert!(r.is_error && r.text.contains("audio lane"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "eq", "clip": "V1@0", "bands": [{ "f": 1000, "db": 3, "q": 1, "kind": "peak" }] }]));
        assert!(r.is_error && r.text.contains("video lane"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": false }]));
        assert!(!r.is_error && r.text.contains("off already"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_gain", "lane": "V1", "db": -3 }]));
        assert!(r.is_error && r.text.contains("video lane"), "{}", r.text);
    }

    #[test]
    fn ripple_lists_clips_left_in_place_outside_the_scope() {
        // Finding 15: a V2 clip after the edit point stays put and is named.
        let mut srv = open();
        let far = len_of(&srv) + 30;
        let r = ed(&mut srv, json!([
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": format!("{far}f"), "lane": "V2", "audio": false },
            { "op": "ripple_delete", "clip": "V1@0" }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("left in place") && r.text.contains("V2@"), "{}", r.text);
    }
}

/// Round 9 fixes: the no-op check, place length, audio-lane length, the no-op notes, cut slivers,
/// swap slots, take links through a cut and the half-open remove_time span.
#[cfg(test)]
mod round9_tests {
    use super::*;

    fn open() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        Server { session: Some(PlaybackSession::open(p).expect("open test asset")), ..Server::default() }
    }

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    fn open_asset(name: &str) -> Server {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(asset(name)).expect("open test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// Runs one op, checks it applied and the project changed, then undoes it and checks the
    /// project is back where it was.
    fn sticks_and_undoes(ops: Value) {
        let mut srv = open_asset("test_av.mp4");
        let before = state_of(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, ops.clone());
        assert!(!r.is_error && !r.text.starts_with("no change"), "{ops}: {}", r.text);
        let after = state_of(srv.session.as_ref().unwrap());
        assert_ne!(before, after, "{ops} changed nothing the no-op check can see");
        // One ledger step, which may be several engine steps (a placed picture is one of them). The link
        // counter is not rolled back by an undo: a link id once handed out is simply not reused.
        assert!(!run_undo(&mut srv, &json!({})).is_error, "{ops}: an undo step was taken");
        let without_counter = |t: String| t.split("next_link: ").enumerate().map(|(i, p)| if i == 0 { p.to_string() } else { p.trim_start_matches(|c: char| c.is_ascii_digit()).to_string() }).collect::<String>();
        assert_eq!(without_counter(state_of(srv.session.as_ref().unwrap())), without_counter(before), "{ops}: undo restores it");
    }

    #[test]
    fn lane_settings_are_kept_by_the_no_op_check_and_undoable() {
        sticks_and_undoes(json!([{ "op": "lane_gain", "lane": "A1", "db": -6 }]));
        sticks_and_undoes(json!([{ "op": "limiter", "on": true, "ceiling_db": -3 }]));
        sticks_and_undoes(json!([{ "op": "lane_flags", "lane": "A1", "muted": true }]));
        sticks_and_undoes(json!([{ "op": "color", "clip": "V1@0", "brightness": 0.2 }]));
        sticks_and_undoes(json!([{ "op": "eq", "clip": "A1@0", "bands": [{ "f": 1000, "db": 3, "q": 1, "kind": "peak" }] }]));
        sticks_and_undoes(json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "wave" }]));
    }

    #[test]
    fn duck_sticks_and_is_undoable() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = state_of(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "duck", "lane": "A1", "under": "A2" }]));
        assert!(!r.is_error && !r.text.starts_with("no change"), "{}", r.text);
        assert_ne!(before, state_of(srv.session.as_ref().unwrap()));
        assert!(srv.session.as_mut().unwrap().undo());
        assert_eq!(state_of(srv.session.as_ref().unwrap()), before);
    }

    #[test]
    fn place_takes_length_and_append_does_too() {
        let mut srv = open_asset("test_av.mp4");
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "20s", "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let last = srv.session.as_ref().unwrap().lane_clips(Lane::V1).last().unwrap().clone();
        assert_eq!(last.frames(), (1.0 * fps).round() as u32);
        // The schema and the tool text both name length.
        assert!(EDIT_DOC.contains("place {source, at, lane?, in?, out?, length?"), "the place signature names length");
        assert!(EDIT_DOC.contains("append {source, in?, out?, length?"), "the append signature names length");
    }

    #[test]
    fn audio_lane_takes_the_sound_length_of_a_source_with_a_short_picture() {
        let mut srv = open_asset("test_short_video_long_audio.mp4");
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "A2", "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        let a2 = srv.session.as_ref().unwrap().lanes().into_iter().filter(|l| l.kind == LaneKind::Audio).nth(1).expect("the added audio lane");
        let placed = srv.session.as_ref().unwrap().lane_clips(a2)[0].frames();
        assert!(placed > (3.0 * fps) as u32, "the sound's length, not the 2 s picture: {placed}f");
    }

    #[test]
    fn insert_time_at_the_end_says_why_nothing_moved() {
        let mut srv = open_asset("test_av.mp4");
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": format!("{end}f"), "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("nothing after the end to push"), "{}", r.text);
        assert!(r.text.contains("no change"), "{}", r.text);
    }

    /// On an empty timeline insert_time moves nothing and the reply says so, and names the way a lead-in comes about.
    #[test]
    fn insert_time_on_an_empty_timeline_says_nothing_changed() {
        let mut srv = Server { session: Some(PlaybackSession::empty(1920, 1080, 30.0).unwrap()), ..Server::default() };
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "0s", "duration": "3s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("the timeline is empty, so no clip moved and nothing changed"), "{}", r.text);
        assert!(r.text.contains("a 3.000s lead-in appears once a clip is placed after it"), "{}", r.text);
        assert!(!r.text.contains("nothing after the end"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth, "no snapshot was pushed");
    }

    /// A 30 fps timeline with the 25 fps test_25fps.mp4 imported as src:1.
    fn open_30_with_25() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::open(asset("test_av.mp4")).expect("open 30 fps asset");
        sess.import(&asset("test_25fps.mp4")).expect("import 25 fps asset");
        Server { session: Some(sess), ..Server::default() }
    }

    /// The in-point is snapped once to the nearest source frame, ties up: 0.5s of the 25 fps
    /// source is 12.5, so frame 13 (0.520s), stored as the first timeline frame showing it (16f).
    /// The reply and the stored clip both read back 0.520s.
    #[test]
    fn place_in_on_a_tie_rounds_up_to_the_source_frame_once() {
        let mut srv = open_30_with_25();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "V1", "at": format!("{end}f"), "in": "0.5s", "length": "1.0s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.520s–1.520s"), "{}", r.text);
        assert!(r.text.contains("(0.5s → src 13f 0.520s)"), "{}", r.text);
        let c = srv.session.as_ref().unwrap().lane_clips(Lane::V1).iter().find(|c| c.start == end).expect("placed").clone();
        assert_eq!((c.in_frame, c.out_frame), (16, 46), "0.5s stored as 16f (shows 13), one second long, no second snap");
    }

    /// An out-point at 1.5s of the 25 fps source is 37.5, so frame 38 (1.520s): stored as 46f.
    #[test]
    fn place_out_on_a_tie_rounds_up_to_the_source_frame() {
        let mut srv = open_30_with_25();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "V1", "at": format!("{end}f"), "in": "0.5s", "out": "1.5s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.520s–1.520s"), "{}", r.text);
        let c = srv.session.as_ref().unwrap().lane_clips(Lane::V1).iter().find(|c| c.start == end).expect("placed").clone();
        assert_eq!(c.out_frame, 46, "1.5s is source frame 38, first shown at timeline 46");
        let back = (f64::from(c.out_frame) * 25.0 / 30.0).floor() as u32;
        assert_eq!(back, 38, "the stored out reads back as the source frame the reply names");
    }

    /// An in-point rounds once to the nearest source frame: 0.52s is
    /// source frame 13, stored as the first timeline frame that shows it (16f), and the reply,
    /// the clip and frame reading all say 0.520s, with no note because 0.52s is exact on the source.
    #[test]
    fn place_in_off_the_timeline_grid_rounds_to_the_source_frame_once() {
        let mut srv = open_30_with_25();
        let end = parse::session_end(srv.session.as_ref().unwrap());
        let r = ed(&mut srv, json!([{ "op": "place", "source": 1, "lane": "V1", "at": format!("{end}f"), "in": "0.52s", "length": "1.0s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.520s–1.520s"), "{}", r.text);
        assert!(!r.text.contains("→ src"), "exact on the source, no note: {}", r.text);
        let c = srv.session.as_ref().unwrap().lane_clips(Lane::V1).iter().find(|c| c.start == end).expect("placed").clone();
        assert_eq!(c.in_frame, 16, "the first timeline frame showing source frame 13");
        assert_eq!(c.out_frame, 46);
        let back = (f64::from(c.in_frame) * 25.0 / 30.0).floor() as u32;
        assert_eq!(back, 13, "the stored frame reads back as the source frame the reply names");
    }

    /// `keep` ranges go through the same snap: 0.5s to 2.0s of the 25 fps source, each once.
    #[test]
    fn keep_range_on_the_timeline_grid_names_the_frame_it_stores() {
        let mut srv = open_30_with_25();
        let r = ed(&mut srv, json!([{ "op": "keep", "source": 1, "ranges": [["0.5s", "2.0s"]] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.520s–2.000s"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let c = s.lane_clips(Lane::V1).last().expect("the keep placed a clip");
        assert_eq!((c.in_frame, c.out_frame), (16, 60), "0.5s stores as 16f (frame 13); 2.0s is on the grid");
    }

    #[test]
    fn a_trim_already_at_its_edge_is_a_note_not_a_failed_batch() {
        let mut srv = open_asset("test_av.mp4");
        let out = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].end();
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": format!("{out}f") }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("already at"), "{}", r.text);
    }

    #[test]
    fn join_slivers_merges_a_gap_under_the_sliver_and_leaves_a_wide_one() {
        let (merged, joined) = join_slivers(&[(100, 50), (153, 47)]);
        assert_eq!((merged.clone(), joined), (vec![(100, 100)], 1), "a 3-frame gap is joined");
        let (kept, joined) = join_slivers(&[(0, 10), (20, 5)]);
        assert_eq!((kept, joined), (vec![(0, 10), (20, 5)], 0), "a 10-frame gap stays");
        let (touching, joined) = join_slivers(&[(0, 10), (10, 5)]);
        assert_eq!((touching, joined), (vec![(0, 15)], 0), "touching cuts merge without a count");
    }

    #[test]
    fn a_linked_take_cut_across_its_lanes_stays_linked_on_both_sides() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "1s", "duration": "0.4s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("detached"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1);
        let a = s.lane_clips(Lane::A1);
        // The take's piece after the cut is linked to its sound piece, and the piece before it to its own.
        let after_v = v.iter().find(|c| c.start >= 30).expect("picture after the cut");
        let after_a = a.iter().find(|c| c.start >= 30).expect("sound after the cut");
        assert!(after_v.link.is_some() && after_v.link == after_a.link, "tail pieces linked");
        let before_v = v.iter().find(|c| c.start < 30).expect("picture before the cut");
        let before_a = a.iter().find(|c| c.start < 30).expect("sound before the cut");
        assert!(before_v.link.is_some() && before_v.link == before_a.link, "head pieces linked");
        assert_ne!(before_v.link, after_v.link, "the two sides are separate takes");
    }

    #[test]
    fn remove_time_ends_its_span_half_open() {
        let mut srv = open_asset("test_av.mp4");
        let fps = parse::session_fps(srv.session.as_ref().unwrap());
        let at = (1.0 * fps).round() as u32;
        let to = (2.0 * fps).round() as u32;
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": format!("{to}f"), "duration": "1s", "text": "T" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": format!("{at}f"), "duration": format!("{}f", to - at) }]));
        assert!(!r.is_error, "a title starting at the span's end is outside it: {}", r.text);
    }

    /// Splits the 5 s take at 2 s, so V1 and A1 each hold the clips 0–2 s (A) and 2–5 s (B).
    fn two_takes() -> Server {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    /// `move {insert:true}` ripples the clip into place: the clip before it is pushed later to
    /// make room, the slot the clip left closes, and the timeline keeps its length. Its sound
    /// on A1 moves with it.
    #[test]
    fn move_insert_ripples_a_clip_earlier_and_keeps_the_length() {
        let mut srv = two_takes();
        let before = srv.session.as_ref().unwrap();
        let total = parse::session_end(before);
        let (a_in, b_in) = (before.lane_clips(Lane::V1)[0].in_frame, before.lane_clips(Lane::V1)[1].in_frame);
        let b_len = before.lane_clips(Lane::V1)[1].frames();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60f", "at": "0s", "insert": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("insert to V1@"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let v = s.lane_clips(Lane::V1);
        assert_eq!(v.len(), 2, "{}", r.text);
        assert_eq!((v[0].start, v[0].frames(), v[0].in_frame), (0, b_len, b_in), "the moved clip leads: {}", r.text);
        assert_eq!((v[1].start, v[1].in_frame), (b_len, a_in), "the first clip follows: {}", r.text);
        assert_eq!(parse::session_end(s), total, "one clip out, one in: the length holds");
        let a1 = s.lane_clips(Lane::A1);
        assert_eq!(a1[0].start, 0, "the sound moved with its picture: {}", r.text);
        assert_eq!(srv.ledger.len(), 2, "split and move are one step each");
    }

    /// A plain move into occupied space still refuses, and names insert:true as the way in.
    #[test]
    fn move_into_occupied_space_refuses_and_suggests_insert() {
        let mut srv = two_takes();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60f", "at": "0s" }]));
        assert!(r.is_error && r.text.contains("insert:true"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start, 60, "a refusal changes nothing");
    }

    /// An insert later than the timeline's end once the slot closes would leave a gap: it refuses.
    #[test]
    fn move_insert_later_past_the_end_refuses() {
        let mut srv = two_takes();
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0f", "at": "5s", "insert": true }]));
        assert!(r.is_error && r.text.contains("past the timeline end"), "{}", r.text);
    }

    /// A title that runs past the last picture frame is named in the reply, with where the picture ends.
    #[test]
    fn a_title_past_the_picture_end_is_named_in_the_reply() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "title_add", "at": "4.5s", "duration": "2s", "text": "Late" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("T1 runs") && r.text.contains("past the picture's end at"), "{}", r.text);
    }

    /// insert_time reports each clip it split and the silence its time leaves on that lane.
    #[test]
    fn insert_time_names_the_clips_it_splits_and_the_silence_it_leaves() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1s", "duration": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A1 split"), "{}", r.text);
        // The same span the effects line names as the hole (3 decimals, one set of times).
        assert!(r.text.contains("2.000s of silence at 1.000–3.000s"), "{}", r.text);
        assert!(r.text.contains("hole left on A1 at 1.000–3.000s"), "{}", r.text);
    }

    /// A lane shorter than the timeline is not a gap: close_gaps says so and names the lane that ends it.
    #[test]
    fn close_gaps_says_when_a_lane_is_short_of_the_timeline() {
        let mut srv = two_takes();
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "V1@60f" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V1 has no gaps"), "{}", r.text);
        assert!(r.text.contains("V1 ends at 2.0s, 3.0s before the timeline end (A1)"), "{}", r.text);
        assert!(r.text.contains("trim A1 or extend V1 to reach it"), "{}", r.text);
    }

    /// A swap that moves later clips names how far, and says how to keep them still.
    #[test]
    fn a_default_swap_says_how_to_keep_the_others_still() {
        // Two picture-only clips (a take refuses a swap): 2 s at 10 s and 3 s at 15 s, after the test take.
        let mut srv = open();
        let p1 = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "10s", "in": "0s", "out": "2s", "lane": "V1", "audio": false }]));
        assert!(!p1.is_error, "{}", p1.text);
        let p2 = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "15s", "in": "0s", "out": "3s", "lane": "V1", "audio": false }]));
        assert!(!p2.is_error, "{}", p2.text);
        let r = ed(&mut srv, json!([{ "op": "swap", "a": "V1@300f", "b": "V1@450f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("later clips moved by 30f") && r.text.contains("use ripple:false to keep everyone else in place"), "{}", r.text);
        assert!(!r.text.contains("ripple:false keeps"), "{}", r.text);
    }

    /// `append until` on a source shorter than the gap says how many times it looped.
    #[test]
    fn append_until_says_how_many_times_it_looped() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "until": "12s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("looped 2 times"), "{}", r.text);
    }

    /// A 16:9 picture placed on a 9:16 canvas says it is letterboxed, and how to crop; a matching
    /// aspect says nothing.
    #[test]
    fn a_placement_on_another_aspect_names_the_fit() {
        let mut srv = open();
        srv.session.as_mut().unwrap().set_resolution(1080, 1920);
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("fitted with letterbox") && r.text.contains("policy:\"fill\""), "{}", r.text);
        let mut same = open();
        let r = ed(&mut same, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.text.contains("fitted with"), "{}", r.text);
    }
}

#[cfg(test)]
mod round12_tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::path::Path;

    fn open() -> Server {
        engine::ao::set_silent_output();
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_av.mp4");
        Server { session: Some(PlaybackSession::open(p).expect("open test asset")), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// The cues a caption lane shows: (text, start, end) in seconds, in start order.
    fn cues(srv: &Server, lane: Lane) -> Vec<(String, f64, f64)> {
        srv.session
            .as_ref()
            .unwrap()
            .sub_lane_cues(lane)
            .into_iter()
            .map(|c| (c.text, c.start_us as f64 / 1e6, c.end_us as f64 / 1e6))
            .collect()
    }

    /// A 5 s timeline of five 1 s clips on V1, and two caption lanes: S1 shows track 0 (four cues)
    /// and S2 shows track 1 (two cues), both placed at 0.
    fn two_caption_lanes(tag: &str) -> Server {
        let mut srv = open();
        let dir = TestDir::new(&format!("r12-two-lanes-{tag}"));
        let a = dir.path().join("a.srt");
        std::fs::write(
            &a,
            "1\n00:00:00,200 --> 00:00:00,800\na1\n\n2\n00:00:01,200 --> 00:00:01,800\na2\n\n\
             3\n00:00:02,500 --> 00:00:03,500\na3\n\n4\n00:00:04,200 --> 00:00:04,800\na4\n",
        )
        .unwrap();
        let b = dir.path().join("b.srt");
        std::fs::write(
            &b,
            "1\n00:00:01,300 --> 00:00:01,700\nb1\n\n2\n00:00:03,200 --> 00:00:03,600\nb2\n",
        )
        .unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": "30f" },
                { "op": "split", "at": "60f" },
                { "op": "split", "at": "90f" },
                { "op": "split", "at": "120f" },
                { "op": "subtitles_import", "path": a.to_string_lossy() },
                { "op": "subtitles_import", "path": b.to_string_lossy() },
                { "op": "captions_place", "track": 0, "lane": "S1" },
                { "op": "captions_place", "track": 1, "lane": "S2" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    fn s1() -> Lane {
        Lane::new(LaneKind::Subtitle, 0)
    }

    fn s2() -> Lane {
        Lane::new(LaneKind::Subtitle, 1)
    }

    fn expect(list: &[(&str, f64, f64)]) -> Vec<(String, f64, f64)> {
        list.iter().map(|&(t, a, b)| (t.to_string(), a, b)).collect()
    }

    /// A ripple move later (3 s) lifts the clip at 1 s: the captions over it travel with it, the
    /// others ripple with the slot, and undo restores every cue exactly.
    #[test]
    fn insert_move_later_carries_captions_over_the_picture_and_ripples_the_rest() {
        let mut srv = two_caption_lanes("later");
        let (before1, before2) = (cues(&srv, s1()), cues(&srv, s2()));
        assert_eq!(
            before1,
            expect(&[("a1", 0.2, 0.8), ("a2", 1.2, 1.8), ("a3", 2.5, 3.5), ("a4", 4.2, 4.8)])
        );
        assert_eq!(before2, expect(&[("b1", 1.3, 1.7), ("b2", 3.2, 3.6)]));
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@30", "at": "3s", "insert": true, "split_captions": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("2 caption(s) moved with the clip"), "{}", r.text);
        assert_eq!(
            cues(&srv, s1()),
            expect(&[("a1", 0.2, 0.8), ("a3", 1.5, 2.5), ("a2", 3.2, 3.8), ("a4", 4.2, 4.8)])
        );
        assert_eq!(cues(&srv, s2()), expect(&[("b2", 2.2, 2.6), ("b1", 3.3, 3.7)]));
        let r = run_undo(&mut srv, &json!({}));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(cues(&srv, s1()), before1);
        assert_eq!(cues(&srv, s2()), before2);
    }

    /// A ripple move earlier (to 1 s) lifts the last clip: its caption travels, the captions from
    /// 1 s up ripple later, and nothing is cut, duplicated or lost.
    #[test]
    fn insert_move_earlier_carries_captions_over_the_picture_and_ripples_the_rest() {
        let mut srv = two_caption_lanes("earlier");
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@120", "at": "1s", "insert": true, "split_captions": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1 caption(s) moved with the clip"), "{}", r.text);
        assert_eq!(
            cues(&srv, s1()),
            expect(&[("a1", 0.2, 0.8), ("a4", 1.2, 1.8), ("a2", 2.2, 2.8), ("a3", 3.5, 4.5)])
        );
        assert_eq!(cues(&srv, s2()), expect(&[("b1", 2.3, 2.7), ("b2", 4.2, 4.6)]));
    }

    /// Two-second V1 clip at 0 with its take, and a narration on A2 over 0–4 s (spanning the change).
    fn speed_setup(narration_out: &str) -> Server {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": "60f" },
                { "op": "lane_add", "kind": "audio" },
                { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": narration_out, "lane": "A2" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    /// Speed (bug 2): a shorter clip with ripple leaves a narration on an out-of-scope lane alone and
    /// says so; an explicit scope naming that lane refuses, naming it, and changes nothing.
    #[test]
    fn speed_ripple_leaves_a_spanning_clip_on_another_lane_and_says_so() {
        let mut srv = speed_setup("2s");
        let a2 = Lane::new(LaneKind::Audio, 1);
        let before = srv.session.as_ref().unwrap().lane_clips(a2).to_vec();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 2, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A2@0 spans the change and was left in place"), "{}", r.text);
        assert!(r.text.contains("(2.000s → 1.000s)"), "the length change is named: {}", r.text);
        assert!(r.text.contains("later items moved 30f (1.000s) earlier"), "{}", r.text);
        let after = srv.session.as_ref().unwrap().lane_clips(a2).to_vec();
        assert_eq!(
            after.iter().map(|c| (c.start, c.in_frame, c.out_frame)).collect::<Vec<_>>(),
            before.iter().map(|c| (c.start, c.in_frame, c.out_frame)).collect::<Vec<_>>(),
            "the narration is not truncated"
        );
        // The later V1 clip moved up by the 30f the change closed.
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].start, 30);
    }

    #[test]
    fn speed_ripple_refuses_a_spanning_clip_on_a_named_lane_and_changes_nothing() {
        let mut srv = speed_setup("2s");
        let depth = srv.ledger.len();
        let r = ed(
            &mut srv,
            json!([{ "op": "speed", "clip": "V1@0", "x": 2, "ripple": true, "lanes": ["V1", "A1", "A2"] }]),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("A2@0 spans the change"), "{}", r.text);
        assert_eq!(srv.ledger.len(), depth);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::new(LaneKind::Audio, 1))[0].frames(), 60);
    }

    /// Lengthening (bug 2, same rule): a narration over the point the room opens is left in place
    /// by default, and an explicit scope naming its lane refuses.
    #[test]
    fn speed_ripple_lengthening_follows_the_same_spanning_rule() {
        let mut srv = speed_setup("4s");
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A2@0 spans the change and was left in place"), "{}", r.text);
        let mut srv = speed_setup("4s");
        let r = ed(
            &mut srv,
            json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true, "lanes": ["V1", "A1", "A2"] }]),
        );
        assert!(r.is_error && r.text.contains("A2@0 spans the change"), "{}", r.text);
    }

    /// Locked lanes (bug 5): the default scope skips a locked caption lane with a note; an explicit
    /// `lanes` naming it refuses.
    #[test]
    fn default_scope_skips_a_locked_caption_lane_with_a_note() {
        let mut srv = two_caption_lanes("locked");
        srv.session.as_mut().unwrap().set_lane_locked(s1(), true);
        let before = cues(&srv, s1());
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1.5s", "duration": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("skipped locked S1"), "{}", r.text);
        assert_eq!(cues(&srv, s1()), before, "the locked lane's cues stay put");
        let r = ed(&mut srv, json!([{ "op": "cut_regions", "regions": [["1.5s", "2.5s"]] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("skipped locked S1"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "1.5s", "duration": "1s", "lanes": ["V1", "S1"] }]));
        assert!(r.is_error && r.text.contains("S1 is locked"), "{}", r.text);
    }

    /// Dirty state (bug 4): a failed batch leaves a clean project clean.
    #[test]
    fn a_failed_batch_leaves_a_clean_project_clean() {
        let mut srv = open();
        let s = srv.session.as_ref().unwrap();
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": "1s" },
                { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(!crate::tools::project::dirty(&srv), "a rolled-back batch is not a change");
    }

    /// Freeze (bug 3): a dry run writes no still and adds no source; a failed batch that froze takes
    /// the still's source back.
    #[test]
    fn freeze_dry_run_writes_nothing_and_a_failed_batch_takes_its_source_back() {
        let dir = TestDir::new("r12-freeze");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let s = srv.session.as_ref().unwrap();
        srv.saved_depth = Some(s.undo_depth());
        srv.saved_pushes = Some(s.history_pushes());
        srv.saved_sources = Some(s.sources().len());
        let sources = s.sources().len();
        let r = run_edit(
            &mut srv,
            &json!({ "ops": [{ "op": "freeze", "at": "1s", "duration": "1s" }], "dry_run": true }),
        );
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sources().len(), sources, "a dry run adds no source");
        assert!(!dir.join("freeze").exists(), "a dry run writes no still");
        assert!(!crate::tools::project::dirty(&srv));
        let r = ed(
            &mut srv,
            json!([
                { "op": "freeze", "at": "1s", "duration": "1s" },
                { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sources().len(), sources, "the failed batch took its still back");
        assert!(!crate::tools::project::dirty(&srv));
    }

    /// Trim (bug 7): a source end that meets the next clip exactly is named for that clip.
    #[test]
    fn trim_names_the_neighbour_a_source_end_meets() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        // The first clip's source ends at 150f, where the appended clip starts.
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "200f", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("clamped where it meets V1@150"), "{}", r.text);
        assert!(!r.text.contains("source's end"), "{}", r.text);
    }

    /// Placed source ranges are source time and timeline positions are timeline frames (bug 6).
    #[test]
    fn place_labels_source_time_and_timeline_frames() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "in": "0s", "out": "2s", "at": "5s", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.000s–2.000s at timeline 150f"), "{}", r.text);
    }

    /// A sliver hole closed by a cut names the clip that moved back (bug 6, find_words path).
    #[test]
    fn closing_a_sliver_hole_names_the_clip_that_moved() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": "60f" },
                { "op": "trim", "clip": "V1@0", "edge": "out", "to": "58f", "scope": "clip" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "cut_regions", "regions": [["100f", "110f"]] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("closed 1 hole(s)"), "{}", r.text);
        assert!(r.text.contains("by trimming the clip beside each"), "{}", r.text);
        assert!(!r.text.contains("moving V1@60"), "{}", r.text);
    }
}

#[cfg(test)]
mod round13_tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// The 5 s test_av.mp4 on a timeline of its own: V1 holds one 5 s clip, A1 its sound.
    fn open() -> Server {
        engine::ao::set_silent_output();
        Server { session: Some(PlaybackSession::open(asset("test_av.mp4")).expect("open test asset")), ..Server::default() }
    }

    /// An empty timeline with test_av.mp4 imported as src:0 (nothing placed yet).
    fn empty_av() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    fn s1() -> Lane {
        Lane::new(LaneKind::Subtitle, 0)
    }

    fn s2() -> Lane {
        Lane::new(LaneKind::Subtitle, 1)
    }

    /// The V1 clips as (start, frames), for asserting placements.
    fn v1_spans(srv: &Server, lane: Lane) -> Vec<(u32, u32)> {
        srv.session.as_ref().unwrap().lane_clips(lane).iter().map(|c| (c.start, c.frames())).collect()
    }

    /// Two tracks: track 0 is a cue 0.5–2.5 s ("x spans a b") and track 1 a cue 1.0–2.2 s ("b one").
    fn write_srts(dir: &TestDir) -> (PathBuf, PathBuf) {
        let a = dir.path().join("a.srt");
        std::fs::write(&a, "1\n00:00:00,500 --> 00:00:02,500\nx spans a b\n").unwrap();
        let b = dir.path().join("b.srt");
        std::fs::write(&b, "1\n00:00:01,000 --> 00:00:02,200\nb one\n").unwrap();
        (a, b)
    }

    /// Two 2 s clips on V1 (split at 2 s, 60f), the 5 s picture's second part 3 s long, and a cue
    /// over the 2 s edge on S1 (track 0), placed at 0.
    fn captioned(dir: &TestDir, two_lanes: bool) -> Server {
        let mut srv = open();
        let (a, b) = write_srts(dir);
        let mut ops = vec![
            json!({ "op": "split", "at": "60f" }),
            json!({ "op": "subtitles_import", "path": a.to_string_lossy() }),
            json!({ "op": "captions_place", "track": 0, "lane": "S1", "at": "0f" }),
        ];
        if two_lanes {
            ops.push(json!({ "op": "subtitles_import", "path": b.to_string_lossy() }));
            ops.push(json!({ "op": "captions_place", "track": 1, "lane": "S2", "at": "0f" }));
        }
        let r = ed(&mut srv, Value::Array(ops));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    fn sub_count(srv: &Server, lane: Lane) -> usize {
        srv.session.as_ref().unwrap().sub_lane(lane).len()
    }

    #[test]
    fn insert_move_refuses_a_caption_spanning_the_edge_by_name() {
        let dir = TestDir::new("r13-cap-refuse");
        let mut srv = captioned(&dir, false);
        let before = (v1_spans(&srv, Lane::V1), sub_count(&srv, s1()));
        // Moving the later clip earlier: its old edges are 2 s and 5 s, and the caption crosses 2 s.
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60", "at": "0f", "insert": true }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("cue 'x spans a b' (S1 0.5s–2.5s) crosses the edge of the moved clip"), "{}", r.text);
        assert!(r.text.contains("pass split_captions:true to split it"), "{}", r.text);
        // A refusal changes nothing, captions included.
        assert_eq!((v1_spans(&srv, Lane::V1), sub_count(&srv, s1())), before);
    }

    #[test]
    fn insert_move_split_captions_splits_and_reports_both_halves_then_undoes_exactly() {
        let dir = TestDir::new("r13-cap-split");
        let mut srv = captioned(&dir, false);
        let before = (v1_spans(&srv, Lane::V1), sub_count(&srv, s1()));
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60", "at": "0f", "insert": true, "split_captions": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions split:"), "{}", r.text);
        assert!(r.text.contains("split at 2.0s into S1 0.0s–2.0s and S1 2.0s–2.5s"), "{}", r.text);
        // The cue is in two placements now, not three: one split at the edge.
        assert_eq!(sub_count(&srv, s1()), 2);
        assert_eq!(v1_spans(&srv, Lane::V1), vec![(0, 90), (90, 60)]);
        // Undo returns the clips and the single caption exactly.
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!((v1_spans(&srv, Lane::V1), sub_count(&srv, s1())), before);
    }

    #[test]
    fn insert_move_later_refuses_then_splits_the_other_direction() {
        let dir = TestDir::new("r13-cap-later");
        let mut srv = captioned(&dir, false);
        // The first clip moved later: its old edge at 2 s is where the caption spans.
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "3s", "insert": true }]));
        assert!(r.is_error && r.text.contains("crosses the edge of the moved clip"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "3s", "insert": true, "split_captions": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("captions split:"), "{}", r.text);
        assert!(sub_count(&srv, s1()) >= 2);
    }

    #[test]
    fn insert_move_split_names_each_caption_lane_it_splits() {
        let dir = TestDir::new("r13-cap-two-lanes");
        let mut srv = captioned(&dir, true);
        let before = (v1_spans(&srv, Lane::V1), sub_count(&srv, s1()), sub_count(&srv, s2()));
        // Two caption lanes: refused by the first straddling placement, S1 here.
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60", "at": "0f", "insert": true }]));
        assert!(r.is_error && r.text.contains("(S1 "), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@60", "at": "0f", "insert": true, "split_captions": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("'b one' (S2 "), "{}", r.text);
        assert!(r.text.contains("'x spans a b' (S1 "), "{}", r.text);
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!((v1_spans(&srv, Lane::V1), sub_count(&srv, s1()), sub_count(&srv, s2())), before);
    }

    /// Clips of 1 s, 1 s and 2 s on V1 (audio off), with the middle one lifted: a 1 s hole at 1 s.
    fn holed() -> Server {
        let mut srv = empty_av();
        let r = ed(
            &mut srv,
            json!([
                { "op": "append", "source": "src:0", "in": "0s", "out": "1s", "lane": "V1", "audio": false },
                { "op": "append", "source": "src:0", "in": "1s", "out": "2s", "lane": "V1", "audio": false },
                { "op": "append", "source": "src:0", "in": "2s", "out": "4s", "lane": "V1", "audio": false },
                { "op": "lift", "clip": "V1@30" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    #[test]
    fn stretched_place_into_a_hole_checks_the_stretched_span() {
        let mut srv = holed();
        // A 2 s source fitted into 30f: it fills the 1 s hole and does not overlap the clip after it.
        let r = ed(
            &mut srv,
            json!([{ "op": "place", "source": "src:0", "lane": "V1", "at": "1s", "in": "0s", "out": "2s", "length": "30f", "stretch": true, "audio": false }]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("stretched ×2"), "{}", r.text);
        assert_eq!(v1_spans(&srv, Lane::V1), vec![(0, 30), (30, 30), (60, 60)]);
    }

    #[test]
    fn unstretched_place_into_the_same_hole_still_overlaps() {
        let mut srv = holed();
        let r = ed(
            &mut srv,
            json!([{ "op": "place", "source": "src:0", "lane": "V1", "at": "1s", "in": "0s", "out": "2s", "audio": false }]),
        );
        assert!(r.is_error && r.text.contains("would overwrite V1@60"), "{}", r.text);
    }

    #[test]
    fn a_still_takes_length_as_its_duration_and_a_missing_source_is_named() {
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import");
        sess.import(&asset("test_still.png")).expect("import still");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "lane": "V1", "at": "0s", "length": "5.267s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(v1_spans(&srv, Lane::V1), vec![(0, 158)]);
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:4", "at": "0s" }]));
        assert!(r.is_error && r.text.contains("there is no src:4 (media_list shows src:0–1)"), "{}", r.text);
    }

    #[test]
    fn trim_clamp_names_the_real_neighbour_not_the_linked_partner() {
        let mut srv = empty_av();
        let r = ed(
            &mut srv,
            json!([
                { "op": "append", "source": "src:0", "in": "0s", "out": "1.5s", "lane": "V1", "audio": false },
                // A pair at 1.5 s: V1@45 with A1@45 as its linked sound, its source reaching back 1 s.
                { "op": "place", "source": "src:0", "lane": "V1", "at": "1.5s", "in": "1s", "out": "3s" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@45", "edge": "in", "to": "0.5s" }]));
        assert!(r.text.contains("clamped where it meets V1@0"), "{}", r.text);
        assert!(!r.text.contains("A1@45") && !r.text.contains("meets V1@45"), "{}", r.text);
    }

    /// An in edge trimmed later leaves a hole before the clip: the reply offers the close in one line.
    /// An out edge trimmed shorter leaves none, so it gets no offer.
    #[test]
    fn an_in_edge_that_moves_later_offers_the_close_in_one_line() {
        let mut srv = empty_av();
        let r = ed(
            &mut srv,
            json!([{ "op": "append", "source": "src:0", "in": "0s", "out": "3s", "lane": "V1", "audio": false }]),
        );
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "in", "to": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("the in edge leaves a hole before the clip"), "{}", r.text);
        assert!(r.text.contains("close_gaps {lane:\"V1\"}"), "{}", r.text);

        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@30", "edge": "out", "to": "1.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("leaves a hole") && !r.text.contains("close_gaps"), "{}", r.text);
    }

    /// The hint is named only for an in edge that moved later, and names the lane it trims.
    #[test]
    fn the_in_edge_hint_needs_a_later_move() {
        let v1 = vec!["V1".to_string()];
        let both = vec!["V1".to_string(), "A1".to_string()];
        assert_eq!(
            in_edge_hole_hint(Edge::Start, Some(0), Some(30), &v1).as_deref(),
            Some("the in edge leaves a hole before the clip; to pull later clips left over it, close_gaps {lane:\"V1\"} (each closes every gap on its lane)")
        );
        // A take trimmed together names each of its lanes.
        let took = in_edge_hole_hint(Edge::Start, Some(0), Some(30), &both).unwrap_or_default();
        assert!(took.ends_with("close_gaps {lane:\"V1\"} and close_gaps {lane:\"A1\"} (each closes every gap on its lane)"), "{took}");
        assert_eq!(in_edge_hole_hint(Edge::Start, Some(30), Some(0), &v1), None);
        assert_eq!(in_edge_hole_hint(Edge::End, Some(30), Some(60), &v1), None);
        assert_eq!(in_edge_hole_hint(Edge::Start, None, Some(30), &v1), None);
    }

    #[test]
    fn a_failed_batch_with_a_freeze_leaves_no_still_behind() {
        let dir = TestDir::new("r13-freeze-rollback");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        // A still already on disk from an earlier batch is never removed by a refused one.
        std::fs::create_dir_all(dir.join("freeze")).unwrap();
        std::fs::write(dir.join("freeze").join("keep.png"), b"old").unwrap();
        let r = ed(
            &mut srv,
            json!([
                { "op": "freeze", "at": "1s", "duration": "1s" },
                { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        let names: Vec<String> = std::fs::read_dir(dir.join("freeze")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["keep.png".to_string()], "only the file that was there stays");
    }

    #[test]
    fn a_refused_freeze_in_a_fresh_folder_removes_the_folder_it_made() {
        let dir = TestDir::new("r13-freeze-fresh");
        let mut srv = open();
        srv.path = Some(dir.join("cut.edith"));
        let r = ed(
            &mut srv,
            json!([
                { "op": "freeze", "at": "1s", "duration": "1s" },
                { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }
            ]),
        );
        assert!(r.is_error, "{}", r.text);
        assert!(!dir.join("freeze").exists(), "a refused batch leaves no freeze folder it made");
    }

    #[test]
    fn a_picture_lane_past_the_picture_end_is_named_after_a_ripple() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": "30f" },
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": "src:0", "lane": "V2", "at": "0s", "in": "0s", "out": "5s", "audio": false },
                { "op": "ripple_delete", "clip": "V1@0" }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V2 runs 1.0s past the picture's end at 4.0s"), "{}", r.text);
    }

    #[test]
    fn copy_look_copies_colour_to_the_targets_as_one_undo_step() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": "src:0", "lane": "V2", "at": "0s", "in": "0s", "out": "5s", "audio": false },
                { "op": "color", "clip": "V1@0", "brightness": 0.3 }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let v2 = Lane::new(LaneKind::Video, 1);
        assert!(srv.session.as_ref().unwrap().color_of(v2, 0).is_none());
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@0"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("copy_look V1@0 → V2@0: color copied"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.color_of(v2, 0).copied(), s.color_of(Lane::V1, 0).copied());
        assert!(s.color_of(v2, 0).is_some());
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert!(srv.session.as_ref().unwrap().color_of(v2, 0).is_none(), "one undo takes the whole copy back");
    }

    #[test]
    fn copy_look_what_and_targets_are_checked() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }, { "op": "place", "source": "src:0", "lane": "V2", "at": "0s", "in": "0s", "out": "5s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        // Eq comes from a sound clip: a picture clip as the source is refused by name.
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@0"], "what": ["eq"] }]));
        assert!(r.is_error && r.text.contains("eq comes from a sound clip"), "{}", r.text);
        // A bad part is named; so is a missing target.
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@0"], "what": ["sparkle"] }]));
        assert!(r.is_error && r.text.contains("sparkle"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0" }]));
        assert!(r.is_error && r.text.contains("to_lane"), "{}", r.text);
        // Transform and to_lane: the source's placement goes to every clip on the lane but itself.
        let r = ed(&mut srv, json!([{ "op": "transform", "clip": "V1@0", "scale": 0.5 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to_lane": "V2", "what": ["transform"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V2@0"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.transform_of(Lane::new(LaneKind::Video, 1), 0).copied(), s.transform_of(Lane::V1, 0).copied());
    }

    #[test]
    fn copy_look_carries_effects_speed_and_gain() {
        let mut srv = open();
        let r = ed(
            &mut srv,
            json!([
                { "op": "lane_add", "kind": "video" },
                { "op": "place", "source": "src:0", "lane": "V2", "at": "0s", "in": "0s", "out": "2s", "audio": false },
                { "op": "lane_add", "kind": "audio" },
                { "op": "place", "source": "src:0", "lane": "A2", "at": "0s", "in": "0s", "out": "2s" },
                { "op": "speed", "clip": "V1@0", "x": 2, "ripple": false },
                { "op": "gain", "clip": "A1@0", "db": -6 }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        let fx = engine::effects::Effect::new(engine::effects::EffectKind::Vignette);
        assert!(srv.session.as_mut().unwrap().set_effects(Lane::V1, 0, vec![fx]));
        let v2 = Lane::new(LaneKind::Video, 1);
        let a2 = Lane::new(LaneKind::Audio, 1);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@0"], "what": ["effects", "speed"] }]));
        assert!(!r.is_error && r.text.contains("effects, speed copied"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.effects_of(v2, 0), s.effects_of(Lane::V1, 0));
        assert_eq!(s.speed_of(v2, 0), s.speed_of(Lane::V1, 0));
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "A1@0", "to": ["A2@0"], "what": ["gain"] }]));
        assert!(!r.is_error && r.text.contains("gain copied"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.clip_gain_of(a2, 0), s.clip_gain_of(Lane::A1, 0));
        assert_ne!(s.clip_gain_of(a2, 0), Some(0.));
        // The same copy again changes nothing; gain from a picture clip is refused by name.
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "A1@0", "to": ["A2@0"], "what": ["gain"] }]));
        assert!(r.text.contains("nothing to copy"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["V2@0"], "what": ["gain"] }]));
        assert!(r.is_error && r.text.contains("gain"), "{}", r.text);
    }

    #[test]
    fn replace_overwrites_the_clip_and_its_sound_keeping_place_and_length() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let (len, in_before) = {
            let c = &srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1];
            (c.frames(), c.in_frame)
        };
        let r = ed(&mut srv, json!([{ "op": "replace", "clip": "V1@60", "source": "src:0", "in": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("replace V1@60 with src:0") && r.text.contains("V1@60 + A1@60"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        for lane in [Lane::V1, Lane::A1] {
            let c = &s.lane_clips(lane)[1];
            assert_eq!((c.start, c.frames(), c.in_frame), (60, len, 0), "{lane:?}");
        }
        assert_ne!(in_before, 0);
        assert_eq!(s.lane_clips(Lane::V1).len(), 2);
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[1].in_frame, in_before);
        // A range that runs out of source is refused by name.
        let r = ed(&mut srv, json!([{ "op": "replace", "clip": "V1@0", "source": "src:0", "in": "4s" }]));
        assert!(r.is_error && r.text.contains("frames after its start"), "{}", r.text);
    }

    #[test]
    fn nudge_moves_a_take_whole_and_refuses_when_a_neighbour_is_in_the_way() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "nudge", "clips": ["V1@60"], "by": "+10f" }]));
        assert!(!r.is_error && r.text.contains("now V1@70"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.lane_clips(Lane::V1)[1].start, 70);
        assert_eq!(s.lane_clips(Lane::A1)[1].start, 70, "the linked sound moves with it");
        let r = ed(&mut srv, json!([{ "op": "nudge", "clips": ["V1@0"], "by": "+20f" }]));
        assert!(!r.is_error && r.text.contains("by +10f") && r.text.contains("stopped short of +20f"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "nudge", "clips": ["V1@10"], "by": "+5f" }]));
        assert!(r.is_error && r.text.contains("refused"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "nudge", "clips": ["V1@10"] }]));
        assert!(r.is_error && r.text.contains("`by`"), "{}", r.text);
    }

    #[test]
    fn a_swap_without_ripple_says_where_each_clip_now_starts() {
        // Two picture-only 2 s clips (a clip in a take is not swapped apart, so no sound here).
        let mut srv = empty_av();
        let r = ed(
            &mut srv,
            json!([
                { "op": "append", "source": "src:0", "in": "0s", "out": "2s", "lane": "V1", "audio": false },
                { "op": "append", "source": "src:0", "in": "2s", "out": "4s", "lane": "V1", "audio": false }
            ]),
        );
        assert!(!r.is_error, "{}", r.text);
        // Both clips are 2 s: swapping them is even, so the reply is exactly the two starts.
        let r = ed(&mut srv, json!([{ "op": "swap", "a": "V1@0", "b": "V1@60", "ripple": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("swap V1@0 ↔ V1@60: first now at 2.0s, second now at 0.0s"), "{}", r.text);
        assert!(!r.text.contains("nothing between them moved"), "{}", r.text);
    }

    #[test]
    fn a_speed_reply_gives_lengths_to_three_decimals() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 2.0 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("(5.000s → 2.500s)"), "{}", r.text);
    }

    #[test]
    fn a_trim_states_the_asked_time_once() {
        // Scope clip: the single-clip path.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "4.07s", "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("asked 4.07s → 122f (4.067s)"), "{}", r.text);
        assert!(!r.text.contains("(4.07s → 122f)"), "{}", r.text);
        // Default scope: the take path.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": "4.07s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("asked 4.07s → 122f (4.067s)"), "{}", r.text);
    }

    #[test]
    fn visualizer_on_an_audio_clip_places_a_ring_waveform_picture() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("placed a waveform picture for A1@0 on V"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lanes().iter().any(|l| l.kind == LaneKind::Video && s.lane_clips(*l).iter().any(|c| c.visualizer != 0)));
        // on:false on the sound clip turns its picture off.
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": false }]));
        assert!(!r.is_error && r.text.contains("its waveform picture on V2@0 is off"), "{}", r.text);
    }

    #[test]
    fn visualizer_on_a_locked_audio_clip_refuses_and_adds_no_lane() {
        let mut srv = open();
        let lanes = srv.session.as_ref().unwrap().lanes().len();
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "A1", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true }]));
        assert!(r.is_error, "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lanes().len(), lanes, "nothing was added");
    }

    #[test]
    fn placing_on_an_empty_lane_names_no_hole() {
        let mut srv = empty_av();
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:0", "lane": "V1", "at": "2s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("hole left"), "{}", r.text);
    }

    fn srt_path() -> String {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../engine/tests/data/test_subs.srt").to_string_lossy().into_owned()
    }

    /// Lanes and captions as (lane, start, end, link) rows, for before/after comparisons.
    fn full_state(srv: &Server) -> Vec<(Lane, u32, u32, Option<u32>)> {
        let s = srv.session.as_ref().expect("session");
        let mut out = Vec::new();
        for l in s.lanes() {
            if l.kind == LaneKind::Subtitle {
                out.extend(s.sub_lane(l).iter().map(|c| (l, c.start, c.start + c.frames, c.link)));
            } else {
                out.extend(s.lane_clips(l).iter().map(|c| (l, c.start, c.end(), c.link)));
            }
        }
        out
    }

    /// A 5 s test clip split at 2 s, with the srt placed as a caption track on S1 from 0.
    fn split_with_captions(srv: &mut Server) {
        let r = ed(srv, json!([{ "op": "split", "at": "2s" }, { "op": "subtitles_import", "path": srt_path() }, { "op": "captions_place", "track": 0, "lane": "S1", "at": "0" }]));
        assert!(!r.is_error, "{}", r.text);
    }

    #[test]
    fn grouped_caption_over_the_slot_is_refused_by_name_and_dry_matches_real() {
        let mut srv = open();
        split_with_captions(&mut srv);
        // The caption (0 s to 4.77 s) grouped with V1@60: it lies over the slot of V1@0.
        let r = ed(&mut srv, json!([{ "op": "group", "clips": ["S1@0", "V1@2s"] }]));
        assert!(!r.is_error, "{}", r.text);
        let before = full_state(&srv);
        let mv = json!([{ "op": "move", "clip": "V1@0", "at": "60f", "insert": true, "split_captions": true }]);
        let dry = run_edit(&mut srv, &json!({ "ops": mv, "dry_run": true }));
        let real = ed(&mut srv, mv);
        assert!(dry.is_error && real.is_error, "dry {} / real {}", dry.text, real.text);
        assert!(real.text.contains("grouped with another take") && real.text.contains("S1@0"), "{}", real.text);
        assert_eq!(dry.text.lines().next(), real.text.lines().next(), "a dry run names what the real run refuses");
        assert_eq!(full_state(&srv), before, "a refused move changes nothing");
    }

    #[test]
    fn grouped_caption_moves_with_its_take_and_no_audio_is_dropped() {
        let mut srv = open();
        split_with_captions(&mut srv);
        // Trim the caption to the take it is grouped with (V1@60, 2 s to 5 s): a consistent group.
        let r = ed(&mut srv, json!([{ "op": "captions_place", "track": 0, "lane": "S2", "at": "2s" }, { "op": "sub_trim", "clip": "S2@60", "edge": "out", "to": "5s" }, { "op": "group", "clips": ["S2@60", "V1@2s"] }]));
        assert!(!r.is_error, "{}", r.text);
        let mv = json!([{ "op": "move", "clip": "V1@60", "at": "0", "insert": true, "split_captions": true }]);
        let dry = run_edit(&mut srv, &json!({ "ops": mv, "dry_run": true }));
        assert!(!dry.is_error, "{}", dry.text);
        let before = full_state(&srv);
        let real = ed(&mut srv, mv);
        assert!(!real.is_error, "{}", real.text);
        let s = srv.session.as_ref().unwrap();
        assert!(!real.text.contains("detached"), "{}", real.text);
        let starts = |l: Lane| s.lane_clips(l).iter().map(|c| (c.start, c.frames())).collect::<Vec<_>>();
        assert_eq!(starts(Lane::V1), vec![(0, 90), (90, 60)], "{}", real.text);
        assert_eq!(starts(Lane::A1), vec![(0, 90), (90, 60)], "no audio is dropped: {}", real.text);
        let caps = s.sub_lane(Lane::new(LaneKind::Subtitle, 1));
        assert_eq!(caps.len(), 1, "the grouped caption is still one placement");
        assert_eq!(caps[0].start, 0, "the caption moved with its take to the take's new start");
        assert_ne!(before, full_state(&srv));
    }

    #[test]
    fn a_cut_between_cues_splits_with_no_refusal_and_a_cue_crossing_is_named() {
        // Cues sit at 0.5–1.5, 2.0–3.25 and 4.0–4.75 s. The 2 s edge falls between cues.
        let mut srv = open();
        split_with_captions(&mut srv);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "90f", "insert": true }]));
        assert!(!r.is_error, "a block spanning the edge with no cue across it moves: {}", r.text);
        assert!(r.text.contains("between cues (no cue cut)"), "{}", r.text);
        let cues = srv.session.as_ref().unwrap().sub_lane_cues(Lane::new(LaneKind::Subtitle, 0)).len();
        assert_eq!(cues, 3, "every cue is still listed once: {}", r.text);
        // Split at 1 s instead: "first line" (0.5–1.5 s) crosses the edge, so the move is refused by that cue.
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }, { "op": "subtitles_import", "path": srt_path() }, { "op": "captions_place", "track": 0, "lane": "S1", "at": "0" }]));
        assert!(!r.is_error, "{}", r.text);
        let before = full_state(&srv);
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "90f", "insert": true }]));
        assert!(r.is_error && r.text.contains("cue 'first line' (S1 0.5s–1.5s) crosses the edge"), "{}", r.text);
        assert_eq!(full_state(&srv), before);
    }

    #[test]
    fn a_sound_clip_visualizer_takes_style_and_fast_and_moves_with_its_clip() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "ring", "fast": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("style ring, fast") && r.text.contains("grouped with it"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let pic = s.lanes().into_iter().find(|l| l.kind == LaneKind::Video && s.lane_clips(*l).iter().any(|c| c.visualizer != 0)).expect("picture");
        let c = &s.lane_clips(pic)[0];
        assert!(c.visualizer & engine::decode::VIZ_FAST != 0, "fast is set");
        assert_eq!(engine::decode::viz_style(c.visualizer), 2, "ring");
        assert!(c.link.is_some() && c.link == s.lane_clips(Lane::A1)[0].link, "the picture is grouped with its sound clip");
        // A move of the sound clip's take carries the picture; a trim carries it too.
        let r = ed(&mut srv, json!([{ "op": "move", "clip": "V1@0", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(pic).iter().any(|c| c.start == 60), "the picture moved with the take");
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "A1@60", "edge": "out", "to": "4s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(pic).iter().any(|c| c.start == 60 && c.end() == 120), "the picture trimmed with the take");
    }

    #[test]
    fn a_still_at_the_same_start_on_a_second_lane_takes_its_length() {
        let mut srv = open();
        let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/test_still.png");
        srv.session.as_mut().unwrap().import(&png).expect("import still");
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "lane": "V1", "at": "32s", "length": "3s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "video" }, { "op": "place", "source": "src:1", "lane": "V2", "at": "32s", "length": "3s" }]));
        assert!(!r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let at = |l: Lane| s.lane_clips(l).iter().find(|c| c.start == 960).map(|c| c.frames());
        assert_eq!(at(Lane::new(LaneKind::Video, 1)), Some(90), "V2 takes the length");
        assert_eq!(at(Lane::V1), Some(90), "V1 keeps its own");
        // The default still length is five seconds: asking for it is not a trim.
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:1", "lane": "V1", "at": "60s", "length": "5s" }]));
        assert!(!r.is_error, "{}", r.text);
    }

    #[test]
    fn an_overwrite_says_what_it_cut_and_where_the_tail_went() {
        // A clip at 3 s, 5 s long (frames 90 to 240 at 30 fps), overwritten from 4 s to 5 s.
        let clip = |start: u32, frames: u32| Clip { start, ..clip_of(0, 0, frames) };
        let both = vec![(Lane::V1, vec![clip(90, 150)])];
        assert_eq!(
            overwrite_notes(&both, 120, 150, 30.0),
            vec!["V1@90 cut to 3.000–4.000; 5.000–8.000 kept as V1@150".to_string()]
        );
        // Overwritten from its head: the tail alone is kept, at its new address.
        let tail = vec![(Lane::V1, vec![clip(90, 150)])];
        assert_eq!(overwrite_notes(&tail, 90, 120, 30.0), vec!["V1@90 cut to 4.000–8.000 (kept as V1@120)".to_string()]);
        // Covered wholly: removed, with the range it had.
        let wholly = vec![(Lane::V1, vec![clip(90, 60)])];
        assert_eq!(overwrite_notes(&wholly, 80, 160, 30.0), vec!["V1@90 removed (3.000–5.000)".to_string()]);
    }

    #[test]
    fn a_hole_names_its_range_and_the_op_that_left_it() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }, { "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lift", "clip": "V1@30" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("hole left on V1 at 1.000–2.000s by lift"), "{}", r.text);
    }

    #[test]
    fn copy_look_lists_a_target_that_took_nothing_as_skipped() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "copy_look", "from": "V1@0", "to": ["A1@0"], "what": ["transform"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("→ A1@0"), "{}", r.text);
        assert!(r.text.contains("skipped:") && r.text.contains("A1@0"), "{}", r.text);
    }

    #[test]
    fn a_visualizer_on_a_video_clip_reports_no_split() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("split "), "{}", r.text);
    }

    #[test]
    fn a_failed_batch_with_a_freeze_keeps_the_undo_history() {
        let mut srv = open();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "freeze", "at": "2s", "duration": "1s" }, { "op": "trim", "clip": "V1@999999f", "edge": "out", "to": "1f" }]));
        assert!(r.is_error, "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(s.undo_depth(), depth, "the failed batch took no undo step away");
        assert_eq!(s.sources().len(), 1, "the still was taken back");
        assert_eq!(srv.ledger.len(), 1, "the ledger keeps its step");
    }
}

#[cfg(test)]
mod audio_track_tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// A timeline opened on a multi-audio file, so its tracks are `src:0`'s file's own.
    fn open_asset(name: &str) -> Server {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(asset(name)).expect("open multi-audio asset");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    #[test]
    fn audio_track_out_of_range_names_the_tracks_the_file_has() {
        let mut srv = open_asset("test_multiaudio.mp4");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 9, "at": "end" }]));
        assert!(r.text.contains("audio_track:9: src:0 has audio tracks 0 (und), 1 (fra), 2 (und)"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().sources().len(), 1, "the refusal changed nothing");
    }

    #[test]
    fn audio_track_takes_in_and_out_of_the_chosen_track() {
        let mut srv = open_asset("test_multiaudio.mp4");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 1, "at": "end", "in": "0f", "out": "10f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("audio track 1 (fra)"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        let clip = s.lane_clips(Lane::V1).last().expect("a clip on V1");
        assert_eq!(clip.frames(), 10, "the in/out range is placed");
        assert_eq!(s.sources()[clip.source].audio_stream, 1);
        assert_eq!(s.lane_clips(Lane::V1).len(), 2, "the scratch clips of the registration are gone");
    }

    #[test]
    fn audio_track_places_the_chosen_track_names_it_and_survives_save() {
        let mut srv = open_asset("test_multiaudio.mp4");
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 1, "at": "end" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("audio track 1 (fra)"), "the reply names the track: {}", r.text);
        let s = srv.session.as_ref().unwrap();
        let clip = s.lane_clips(Lane::V1).last().expect("a clip was placed on V1");
        assert_eq!(s.sources()[clip.source].audio_stream, 1, "the placed clip plays track 1");
        assert_eq!(s.sources()[0].audio_stream, 0, "src:0 keeps its own track");

        // The choice survives save and reopen.
        let dir = TestDir::new("audio-track-save");
        let proj = dir.join("multi.edith");
        s.save_project(&proj).expect("save");
        let back = PlaybackSession::open_project(&proj).expect("reopen");
        let back_clip = back.lane_clips(Lane::V1).last().expect("the clip is back").clone();
        assert_eq!(back.sources()[back_clip.source].audio_stream, 1, "track 1 survives reopen");
    }

    #[test]
    fn track_one_and_track_zero_sound_different() {
        let file = asset("test_multiaudio.mp4");
        let zero = engine::silence::levels(&file, 0, (0.0, f64::INFINITY)).expect("levels 0").expect("track 0 has sound");
        let one = engine::silence::levels(&file, 1, (0.0, f64::INFINITY)).expect("levels 1").expect("track 1 has sound");
        assert_ne!(zero, one, "the tracks differ, so placing one is not placing the other");
    }

    #[test]
    fn a_placement_that_overwrites_a_clip_names_it_removed_not_shifted() {
        let mut srv = open_asset("test_av.mp4");
        let len = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames();
        let half = len / 2;
        // X2 spans [half, len); the placement covers exactly that span, so X2 is removed outright.
        let r = ed(
            &mut srv,
            json!([
                { "op": "split", "at": format!("{half}f") },
                { "op": "place", "source": 0, "in": "0f", "length": format!("{}f", len - half), "at": format!("{half}f"), "lane": "V1", "overwrite": true }
            ]),
        );
        assert!(r.text.contains("removed"), "the removed clip is named: {}", r.text);
        assert!(!r.text.contains("shifted:"), "no clip shifted: {}", r.text);
    }

    /// A scope:"clip" trim to the edge the clip already has changes nothing: no detach, no undo step.
    #[test]
    fn scope_clip_trim_to_the_current_edge_is_a_true_no_op() {
        let mut srv = open_asset("test_av.mp4");
        let out = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].end();
        let (ledger_before, v1_link) = (srv.ledger.len(), srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].link);
        let r = ed(&mut srv, json!([{ "op": "trim", "clip": "V1@0", "edge": "out", "to": format!("{out}f"), "scope": "clip" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("nothing to trim"), "{}", r.text);
        assert!(!r.text.contains("detached"), "{}", r.text);
        assert_eq!(srv.ledger.len(), ledger_before, "no undo step");
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].link, v1_link, "the take is still linked");
    }

    /// Deleting one clip of a take names the whole take it removed, and a delete at the end of a lane
    /// has no arrow list: the clips it removed are not listed as if they stayed.
    #[test]
    fn delete_of_one_clip_of_a_take_names_the_take() {
        let mut srv = open_asset("test_av.mp4");
        let half = srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].frames() / 2;
        let r = ed(&mut srv, json!([
            { "op": "split", "at": format!("{half}f") },
            { "op": "delete", "clip": format!("A1@{half}") }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains(&format!("removed V1@{half} and A1@{half} (its take)")) || r.text.contains(&format!("removed A1@{half} and V1@{half} (its take)")), "{}", r.text);
        assert!(!r.text.contains(&format!("→ V1@{half}")), "a removed clip is not an arrow target: {}", r.text);
    }

    /// A ripple_delete names the span it closed, and its removed take is named as removed.
    #[test]
    fn ripple_delete_names_the_span_it_closed_and_the_removed_take() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("closed 0.0s–"), "{}", r.text);
        assert!(r.text.contains("removed") && r.text.contains("(its take)"), "{}", r.text);
        let first = r.text.lines().next().unwrap_or("");
        assert!(!first.contains("→"), "nothing is left after the cut to move: {}", r.text);
    }

    /// A cut inside one clip splits it: the reply names both pieces, not a trim of the first.
    #[test]
    fn remove_time_inside_a_clip_says_split_and_names_both_pieces() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([{ "op": "remove_time", "at": "2s", "duration": "0.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("split V1@0: V1@0 0.0s–2.0s, V1@60 2.0s–"), "{}", r.text);
        assert!(!r.text.contains("trimmed V1@0"), "{}", r.text);
    }

    /// A dissolve whose successor a delete removed and the next clip moved up into: the reply says the
    /// dissolve now joins that clip, and a dissolve that loses its successor for good says it is a cut.
    #[test]
    fn dissolve_names_the_clip_it_now_joins_after_a_ripple() {
        let mut srv = open_asset("test_av.mp4");
        let r = ed(&mut srv, json!([
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "0s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
            { "op": "place", "source": 0, "at": "2s", "in": "2s", "out": "4s", "lane": "V2", "audio": false },
            { "op": "place", "source": 0, "at": "4s", "in": "3s", "out": "5s", "lane": "V2", "audio": false },
            { "op": "dissolve", "clip": "V2@0", "frames": 12, "linked": false },
            { "op": "delete", "clip": "V2@60", "lanes": ["V2"] }
        ]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("V2@0's dissolve now joins V2@60 (was the deleted clip)"), "{}", r.text);
    }

}

#[cfg(test)]
mod round26_tests {
    use super::*;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// An empty 30 fps timeline with test_av.mp4 as src:0 and test_still.png as src:1.
    fn bank() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import test asset");
        sess.import(&asset("test_still.png")).expect("import still");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    #[test]
    fn visualizer_over_a_logo_names_the_picture_it_covers() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([
            { "op": "append", "source": 0 },
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "1s", "in": "0s", "out": "3s", "lane": "V2", "audio": false },
        ]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "ring" }]));
        assert!(!r.is_error, "{}", r.text);
        // V1 is the sound's own take, so only the logo on V2 is named.
        assert!(
            r.text.contains("the visualizer on V3 draws over V2 (test_av.mp4) during 1.0s–4.0s;"),
            "{}",
            r.text
        );
        assert!(!r.text.contains("draws over V1"), "{}", r.text);
        assert!(r.text.contains("lane_move it below them"), "{}", r.text);
        assert!(r.text.contains("no background option makes it transparent"), "{}", r.text);
    }

    #[test]
    fn visualizer_over_its_own_take_is_not_a_cover() {
        // The take's own picture on V1 sits below the visualizer's V2 picture, but it is the take the
        // visualizer is for: no note.
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true, "style": "ring" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("grouped with it"), "{}", r.text);
        assert!(!r.text.contains("draws over"), "{}", r.text);
    }

    #[test]
    fn visualizer_over_a_take_with_a_logo_below_names_only_the_logo() {
        // Own take (V1+A1) plus an unlinked logo on V2 under the new V3 picture: only the logo is named.
        let mut srv = bank();
        let r = ed(&mut srv, json!([
            { "op": "append", "source": 0 },
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "at": "2s", "in": "0s", "out": "2s", "lane": "V2", "audio": false },
        ]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("draws over V2 (test_av.mp4) during 2.0s–4.0s"), "{}", r.text);
        assert!(!r.text.contains("draws over V1"), "{}", r.text);
    }

    #[test]
    fn visualizer_on_a_video_clip_names_its_sound_clip_and_is_refused() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "V1@0", "on": true }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("the visualizer is set on the sound clip of its take, A1@0 (A1@0 is the sound of V1@0)"), "{}", r.text);
        // Turning a flag off is let through: nothing is refused on a video clip with on:false.
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "V1@0", "on": false }]));
        assert!(!r.text.contains("refused"), "{}", r.text);
    }

    #[test]
    fn visualizer_on_a_video_clip_with_no_sound_is_refused() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "V1@0", "on": true }]));
        assert!(r.is_error && r.text.contains("no linked sound clip"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1)[0].visualizer, 0);
    }

    #[test]
    fn undo_and_redo_with_no_project_say_no_project_is_open() {
        let mut srv = Server::default();
        let r = run_undo(&mut srv, &json!({}));
        assert!(r.is_error && r.text.starts_with("no project is open"), "{}", r.text);
        assert!(!r.text.contains("checkpoints survive"), "{}", r.text);
        let r = run_redo(&mut srv, &json!({}));
        assert!(r.is_error && r.text.starts_with("no project is open"), "{}", r.text);
    }

    #[test]
    fn a_placement_far_past_the_clips_names_the_gap_it_left() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": "src:0", "at": "999s", "lane": "V1", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("note: left a 994.0s gap before it on V1 (the timeline was 5.0s)"), "{}", r.text);
    }

    #[test]
    fn a_placement_against_the_clip_before_it_leaves_no_gap_note() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("left a "), "{}", r.text);
    }

    #[test]
    fn close_gaps_on_an_empty_lane_says_it_is_empty() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "A2" }]));
        assert!(r.is_error && r.text.contains("A2 is not a lane on this timeline; the lanes are V1, A1"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }, { "op": "close_gaps", "lane": "A2" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("A2 is empty: nothing to close"), "{}", r.text);
        assert!(!r.text.contains("ends at"), "{}", r.text);
    }

    #[test]
    fn crossfade_after_a_dissolve_says_the_pair_already_crosses() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }, { "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 15 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "crossfade", "clip": "A1@0", "frames": 15 }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.starts_with("no change: A1@0 and A1@150 already crossfade over 15f (from the dissolve)"), "{}", r.text);
    }

    #[test]
    fn crossfade_again_without_a_dissolve_names_the_crossfade_only() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0 }, { "op": "append", "source": 0 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "crossfade", "clip": "A1@0", "frames": 15 }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "crossfade", "clip": "A1@0", "frames": 15 }]));
        assert!(r.text.contains("already crossfade over 15f"), "{}", r.text);
        assert!(!r.text.contains("from the dissolve"), "{}", r.text);
    }
}

#[cfg(test)]
mod round27_tests {
    use super::*;
    use crate::testutil::TestDir;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// An empty 30 fps timeline with test_av.mp4 as src:0 and test_still.png as src:1.
    fn bank() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import test asset");
        sess.import(&asset("test_still.png")).expect("import still");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// `restore` keeps the open state as a checkpoint first, and the reply names it, so the
    /// restore can be undone by restoring that checkpoint back.
    #[test]
    fn restore_keeps_the_open_state_as_a_checkpoint() {
        let td = TestDir::new("r27-restore-keeps");
        TEST_CHECKPOINTS.with(|d| *d.borrow_mut() = Some(td.join("checkpoints")));
        let mut srv = bank();
        let c = run_checkpoint(&mut srv, &json!({ "name": "rs1" }));
        assert!(!c.is_error, "{}", c.text);
        let r = ed(&mut srv, json!([{ "op": "append", "source": 0, "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let rs = run_restore(&mut srv, &json!({ "name": "rs1" }));
        assert!(!rs.is_error, "{}", rs.text);
        assert!(rs.text.contains("the state before the restore is kept as checkpoint before-restore-rs1"), "{}", rs.text);
        assert!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).is_empty());
        let back = run_restore(&mut srv, &json!({ "name": "before-restore-rs1" }));
        assert!(!back.is_error, "{}", back.text);
        assert_eq!(srv.session.as_ref().unwrap().lane_clips(Lane::V1).len(), 1, "the unsaved edit came back");
    }

    /// A fade clamped to its clip reads the frames the clip kept in its header, with the asked time in the note.
    #[test]
    fn a_fade_clamped_to_its_clip_shows_the_frames_it_got() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "in": "0s", "out": "2s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "fade", "clip": "V1@0", "in": "999s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("in 60f (in clamped from 999.0s to the clip's 2.0s)"), "{}", r.text);
    }

    /// A locked lane refuses a dissolve on its last clip as locked, not as having no clip after it.
    #[test]
    fn a_dissolve_on_a_locked_last_clip_names_the_lock() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "in": "0s", "out": "2s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "V1", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "dissolve", "clip": "V1@0", "frames": 12 }]));
        assert!(r.is_error && r.text.contains("lane V1 is locked"), "{}", r.text);
        assert!(!r.text.contains("last clip"), "{}", r.text);
    }

    /// Taking an audio track in for the first time says it added a source, with its file and track.
    #[test]
    fn a_track_taken_in_for_the_first_time_says_it_added_a_source() {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(asset("test_multiaudio.mp4")).expect("open multi-audio asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 1, "at": "end" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("1. added src:1 (test_multiaudio.mp4 audio track 1); place src:1 "), "{}", r.text);
        let again = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 1, "at": "end" }]));
        assert!(!again.text.contains("added src:"), "{}", again.text);
    }

    /// A file with one audio track names it in parts, not as a one-item list.
    #[test]
    fn a_file_with_one_audio_track_names_it_in_parts() {
        engine::ao::set_silent_output();
        let sess = PlaybackSession::open(asset("test_av.mp4")).expect("open asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "audio_track": 9, "at": "end" }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("src:0 has one audio track ("), "{}", r.text);
        assert!(!r.text.contains("audio tracks"), "{}", r.text);
    }

    /// A group whose members differ in length says so, with each one's length.
    #[test]
    fn a_group_of_unequal_members_says_they_differ_in_length() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([
            { "op": "place", "source": 0, "lane": "V1", "at": "0s", "in": "0s", "out": "2s", "audio": false },
            { "op": "lane_add", "kind": "video" },
            { "op": "place", "source": 0, "lane": "V2", "at": "0s", "in": "0s", "out": "4s", "audio": false },
        ]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "group", "clips": ["V1@0", "V2@0"] }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("the members differ in length (V1@0 2.000s, V2@0 4.000s)"), "{}", r.text);
    }

    /// A gap a close_gaps sweep leaves open says the engine's own reason for it, with its span: here
    /// the V1 clip after the gap is one take with an A1 clip that starts elsewhere.
    #[test]
    fn close_gaps_names_the_reason_each_gap_stayed_open() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "in": "0s", "out": "2s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "3s", "in": "0s", "out": "2s", "audio": true }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "V1" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("left open 1 (2.0s–3.0s: the V1 clip at V1@90 is one take with the A1 clip at A1@90, and A1 has no gap of that length there"), "{}", r.text);
        assert!(!r.text.contains("a linked take would split"), "{}", r.text);
    }

    /// The source out of a placed range is the timeline's exclusive end, not the last frame's start.
    /// A 2 s range of the 2.002 s 23.976 fps file lands 61 timeline frames long, which the file's
    /// real end caps at `2.002s`, the same end the timeline prints.
    #[test]
    fn a_placed_source_range_ends_where_the_timeline_ends_it() {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_23976fps.mp4")).expect("import 23.976 fps asset");
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s", "in": "0s", "out": "2s", "audio": false }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("src 0.000s–2.002s"), "{}", r.text);
    }
}

#[cfg(test)]
mod round29_tests {
    use super::*;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// An empty 30 fps timeline with test_av.mp4 (5 s, picture and sound) as src:0.
    fn bank() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        sess.import(&asset("test_av.mp4")).expect("import test asset");
        Server { session: Some(sess), ..Server::default() }
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// Three 5 s clips on V1 and A1 (0 s, 5 s, 10 s), the first slowed to half speed with its ripple,
    /// so the timeline is 20 s and the slow clip ends at 10 s.
    fn slow_first() -> Server {
        let mut srv = bank();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "append", "source": 0 }, { "op": "append", "source": 0 }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    /// insert_time and remove_time cut a slow clip at a frame where it has no cut point: the cut moves to
    /// the nearest one, the reply says so, and the amount moved is what was asked.
    #[test]
    fn insert_and_remove_work_inside_a_slow_clip_at_an_odd_frame() {
        let mut srv = slow_first();
        let s = srv.session.as_ref().unwrap();
        // At half speed a cut is possible only at an even timeline frame of the clip: 151f is not one.
        assert!(!cuttable(s, &[Lane::V1], 151), "151f is between two showings of one source frame");
        assert_eq!(cut_point(s, &[Lane::V1], 151), 150, "the nearest earlier cut point");
        let ins = ed(&mut srv, json!([{ "op": "insert_time", "at": "151f", "duration": "15f" }]));
        assert!(!ins.is_error, "{}", ins.text);
        assert!(ins.text.contains("the cut moved from 151f to 150f"), "{}", ins.text);
        assert_eq!(parse::session_end(srv.session.as_ref().unwrap()), 615, "15f inserted exactly");

        let mut srv = slow_first();
        let rm = ed(&mut srv, json!([{ "op": "remove_time", "at": "151f", "duration": "30f" }]));
        assert!(!rm.is_error, "{}", rm.text);
        let s = srv.session.as_ref().unwrap();
        assert_eq!(parse::session_end(s), 570, "the 30f removed are the 30f asked, no more");
        assert_eq!(s.gap_count(Lane::V1), 0, "no hole left by the cut: {}", rm.text);
    }

    #[test]
    fn close_gaps_names_the_hole_it_leaves_on_another_lane() {
        // A hole on V1 and the matching one on A2 (a bed), with no take between them: closing V1 alone leaves
        // A2's, and the reply names it with its own close_gaps.
        let mut srv = bank();
        let r = ed(&mut srv, json!([
            { "op": "lane_add", "kind": "audio" },
            { "op": "append", "source": 0, "lane": "V1", "audio": false },
            { "op": "append", "source": 0, "lane": "V1", "audio": false },
            { "op": "append", "source": 0, "lane": "V1", "audio": false },
            { "op": "place", "source": 0, "lane": "A2", "at": "0s" },
            { "op": "place", "source": 0, "lane": "A2", "at": "5s" },
            { "op": "place", "source": 0, "lane": "A2", "at": "10s" },
            { "op": "lift", "clip": "V1@150" },
            { "op": "lift", "clip": "A2@150" }
        ]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "close_gaps", "lane": "V1" }]));
        assert!(!r.is_error && r.text.contains("closed 1"), "{}", r.text);
        assert!(r.text.contains("gaps left on other lanes: A2 5.0s–10.0s (close with close_gaps {lane:\"A2\"})"), "{}", r.text);
        assert_eq!(srv.session.as_ref().unwrap().gap_count(Lane::new(LaneKind::Audio, 1)), 1);
    }

    /// A speed change with ripple names only the items after it: the sped clip and its take are not
    /// among the later items, so a clip with nothing after it says no item moved.
    #[test]
    fn speed_ripple_does_not_name_the_sped_clip_as_a_later_item() {
        let mut srv = bank();
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 0.5, "ripple": true }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(!r.text.contains("later items moved"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "speed", "clip": "V1@0", "x": 1.0, "ripple": true }]));
        assert!(!r.is_error && !r.text.contains("later items moved"), "{}", r.text);
    }

    /// split at a frame between two showings of one source frame inside a slow clip cuts at the nearest
    /// cut point, and the reply says it moved, as insert_time does.
    #[test]
    fn split_works_inside_a_slow_clip_at_an_odd_frame() {
        let mut srv = slow_first();
        assert!(!cuttable(srv.session.as_ref().unwrap(), &[Lane::V1], 151));
        let r = ed(&mut srv, json!([{ "op": "split", "at": "151f" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("the cut moved from 151f to 150f"), "{}", r.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lane_clips(Lane::V1).iter().any(|c| c.start == 150), "a cut at 150f: {}", r.text);
        assert_eq!(parse::session_end(s), 600, "the cut removes nothing");
    }

    /// A cut that a hole closes must not pull the clips after it: the removed amount is the amount asked.
    #[test]
    fn remove_time_closes_a_sliver_without_removing_more() {
        let mut srv = slow_first();
        let before = parse::session_end(srv.session.as_ref().unwrap());
        let rm = ed(&mut srv, json!([{ "op": "remove_time", "at": "10.033s", "duration": "1s" }]));
        assert!(!rm.is_error, "{}", rm.text);
        assert_eq!(before - parse::session_end(srv.session.as_ref().unwrap()), 30, "{}", rm.text);
        assert!(!rm.text.contains("moving V1@"), "a sliver closing names no moved clip: {}", rm.text);
    }

    /// A locked lane that a ripple would move or cut refuses the op by name, and nothing changes.
    #[test]
    fn a_ripple_refuses_a_locked_lane_it_would_leave_behind() {
        let mut srv = bank();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0 }, { "op": "append", "source": 0 }, { "op": "append", "source": 0 }])).is_error);
        srv.session.as_mut().unwrap().set_lane_locked(Lane::A1, true);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        for op in [
            json!({ "op": "insert_time", "at": "18s", "duration": "0.5s" }),
            json!({ "op": "remove_time", "at": "18s", "duration": "0.5s" }),
            json!({ "op": "insert_time", "at": "18s", "duration": "0.5s", "lanes": ["V1"] }),
        ] {
            let r = ed(&mut srv, json!([op]));
            assert!(r.is_error, "{}", r.text);
            assert!(r.text.contains("lane A1 is locked"), "{}", r.text);
            assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
        }
    }

    /// A second insert into the first one's hole names the hole it grew, on every lane it grew on.
    #[test]
    fn a_second_insert_into_a_hole_names_each_lane_that_grew() {
        let mut srv = bank();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "0s" }, { "op": "append", "source": 0 }, { "op": "append", "source": 0 }, { "op": "append", "source": 0 }])).is_error);
        assert!(!ed(&mut srv, json!([{ "op": "insert_time", "at": "18s", "duration": "0.5s" }])).is_error);
        let r = ed(&mut srv, json!([{ "op": "insert_time", "at": "18s", "duration": "0.5s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("hole left on V1 at 18.000–19.000s by insert_time"), "{}", r.text);
        assert!(r.text.contains("hole left on A1 at 18.000–19.000s by insert_time"), "{}", r.text);
    }

    /// Splitting a take on its sound lane alone detaches a locked picture clip from the take: it refuses.
    #[test]
    fn split_and_lift_refuse_to_detach_a_locked_partner() {
        let mut srv = slow_first();
        srv.session.as_mut().unwrap().set_lane_locked(Lane::V1, true);
        let depth = srv.session.as_ref().unwrap().undo_depth();
        let r = ed(&mut srv, json!([{ "op": "split", "at": "7s", "lanes": ["A1"] }]));
        assert!(r.is_error && r.text.contains("lane V1 is locked"), "{}", r.text);
        let l = ed(&mut srv, json!([{ "op": "lift", "clip": "A1@0" }]));
        assert!(l.is_error && l.text.contains("lane V1 is locked"), "{}", l.text);
        assert_eq!(srv.session.as_ref().unwrap().undo_depth(), depth);
    }

    /// on:false on a sound clip turns off the waveform picture of its take, as it does on the picture.
    #[test]
    fn visualizer_off_from_the_sound_clip_turns_its_picture_off() {
        let mut srv = bank();
        assert!(!ed(&mut srv, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }])).is_error);
        let on = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": true }]));
        assert!(!on.is_error, "{}", on.text);
        let off = ed(&mut srv, json!([{ "op": "visualizer", "clip": "A1@0", "on": false }]));
        assert!(!off.is_error, "{}", off.text);
        assert!(off.text.contains("visualizer A1@0 off: its waveform picture"), "{}", off.text);
        let s = srv.session.as_ref().unwrap();
        assert!(s.lanes().into_iter().filter(|l| l.kind == LaneKind::Video).all(|l| s.lane_clips(l).iter().all(|c| c.visualizer == 0)));
    }

    /// The picture refusal states what the sound needs first: 1x speed for a waveform that matches it.
    #[test]
    fn a_video_visualizer_refusal_states_the_speed_its_sound_needs() {
        let mut srv = slow_first();
        let r = ed(&mut srv, json!([{ "op": "visualizer", "clip": "V1@0", "on": true }]));
        assert!(r.is_error, "{}", r.text);
        assert!(r.text.contains("A1@0 plays at 0.5x: set its speed to 1x first"), "{}", r.text);
    }

    /// A ripple delete moves titles with the picture: one inside the removed span goes (named, undoable),
    /// one the span crosses is trimmed to the part outside, and one after it moves back.
    #[test]
    fn ripple_delete_removes_trims_and_moves_titles() {
        let mut srv2 = bank();
        assert!(!ed(&mut srv2, json!([{ "op": "place", "source": 0, "lane": "V1", "at": "0s" }, { "op": "append", "source": 0, "lane": "V1" }, { "op": "append", "source": 0, "lane": "V1" }])).is_error);
        let t = ed(&mut srv2, json!([
            { "op": "title_add", "at": "3s", "duration": "1s", "text": "T-A" },
            { "op": "title_add", "at": "8s", "duration": "1s", "text": "T-B" },
            { "op": "title_add", "at": "4.5s", "duration": "1s", "text": "T-C" }
        ]));
        assert!(!t.is_error, "{}", t.text);
        let r = ed(&mut srv2, json!([{ "op": "ripple_delete", "clip": "V1@0" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("T1 3.000s removed with the cut"), "{}", r.text);
        assert!(r.text.contains("trimmed to 0.000–0.500s"), "{}", r.text);
        assert!(r.text.contains("T3 8.000s→3.000s"), "{}", r.text);
        let s = srv2.session.as_ref().unwrap();
        let mut titles: Vec<(String, u32, u32)> = s.titles().iter().map(|t| (t.text.clone(), t.start, t.frames)).collect();
        titles.sort();
        assert_eq!(titles, vec![("T-B".to_string(), 90, 30), ("T-C".to_string(), 0, 15)]);
    }

    /// A source range ends at the source frame after the last one a clip shows, at the file's own rate.
    #[test]
    fn a_source_range_ends_after_the_last_source_frame_shown() {
        let b = Base { src: 23_976_000, tl: 23_976_000, dur: f64::MAX };
        assert_eq!(b.span(0, 48), "src 0.000s–2.002s");
        let b = Base { src: 30_000_000, tl: 30_000_000, dur: f64::MAX };
        assert_eq!(b.span(0, 150), "src 0.000s–5.000s");
    }
}

#[cfg(test)]
mod range_length_tests {
    use super::*;
    use serde_json::json;

    /// A 23.976 source on a 30 fps timeline: each end snaps to a source frame on its own, but the length is
    /// the requested duration in timeline frames, so none of these comes out a frame long.
    #[test]
    fn a_range_from_a_23976_source_is_its_duration_at_30_fps() {
        let b = Base { src: 23_976_000, tl: 30_000_000, dur: f64::MAX };
        let snap = |secs: f64| b.conform((secs * 23.976 + 0.5).floor() as u32);
        let len = |i: f64, o: f64| {
            let in_f = snap(i);
            range_end(Some(&json!(format!("{i}s"))), &json!(format!("{o}s")), b, 30.0, 1200, in_f, snap(o)) - in_f
        };
        assert_eq!(len(0.0, 12.0), 360);
        assert_eq!(len(20.0, 30.0), 300);
        assert_eq!(len(0.0, 15.0), 450);
        assert_eq!(len(15.0, 35.0), 600);
        // `end` keeps the snapped end, and a range in frames is its own duration too.
        assert_eq!(range_end(Some(&json!("0s")), &json!("end"), b, 30.0, 1200, 0, 1200), 1200);
        assert_eq!(range_end(Some(&json!("0f")), &json!("288f"), b, 30.0, 1200, 0, 361), 360);
    }
}

#[cfg(test)]
mod three_point_tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn asset(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
    }

    /// An empty 30 fps timeline; the library holds src:0 test_av.mp4, src:1 test_av2.mp4,
    /// src:2 test_audio_only.mp4, src:3 test_still.png. Then src:0 is placed whole at 0 (V1+A1).
    fn bench() -> Server {
        engine::ao::set_silent_output();
        let mut sess = PlaybackSession::empty(1280, 720, 30.0).expect("empty session");
        for f in ["test_av.mp4", "test_av2.mp4", "test_audio_only.mp4", "test_still.png"] {
            sess.import(&asset(f)).expect("import");
        }
        let mut srv = Server { session: Some(sess), ..Server::default() };
        let r = ed(&mut srv, json!([{ "op": "place", "source": 0, "at": "0s" }]));
        assert!(!r.is_error, "{}", r.text);
        srv
    }

    fn ed(srv: &mut Server, ops: Value) -> Reply {
        run_edit(srv, &json!({ "ops": ops }))
    }

    /// `(start, end, in_frame, source)` of every clip on `lane`.
    fn spans(srv: &Server, lane: Lane) -> Vec<(u32, u32, u32, usize)> {
        srv.session.as_ref().unwrap().lane_clips(lane).iter().map(|c| (c.start, c.end(), c.in_frame, c.source)).collect()
    }

    fn link_of(srv: &Server, lane: Lane, idx: usize) -> Option<u32> {
        srv.session.as_ref().unwrap().lane_clips(lane)[idx].link
    }

    #[test]
    fn insert_into_the_middle_splits_ripples_and_undoes_in_one_step() {
        let mut srv = bench();
        let whole = spans(&srv, Lane::V1)[0].1;
        let steps = srv.ledger.len();
        let r = ed(&mut srv, json!([{ "op": "insert", "src": "src:1", "in": "1s", "out": "2s", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("split V1@0, A1@0") && r.text.contains("rippled"), "{}", r.text);
        let want = vec![(0, 60, 0, 0), (60, 90, 30, 1), (90, whole + 30, 60, 0)];
        assert_eq!(spans(&srv, Lane::V1), want);
        assert_eq!(spans(&srv, Lane::A1), want);
        assert!(link_of(&srv, Lane::V1, 1).is_some() && link_of(&srv, Lane::V1, 1) == link_of(&srv, Lane::A1, 1), "the new take is linked");
        assert_eq!(srv.ledger.len(), steps + 1, "one undo step");
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(spans(&srv, Lane::V1), vec![(0, whole, 0, 0)]);
        assert_eq!(spans(&srv, Lane::A1), vec![(0, whole, 0, 0)]);
    }

    #[test]
    fn src_and_source_are_the_same_key_and_not_both() {
        let mut srv = bench();
        let r = ed(&mut srv, json!([{ "op": "insert", "src": 1, "source": 1, "at": "0s", "length": "1s" }]));
        assert!(r.is_error && r.text.contains("not both"), "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "insert", "source": 1, "at": "0s", "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1)[0], (0, 30, 0, 1));
    }

    #[test]
    fn overwrite_across_two_clips_replaces_and_moves_nothing() {
        let mut srv = bench();
        let whole = spans(&srv, Lane::V1)[0].1;
        let r = ed(&mut srv, json!([{ "op": "split", "at": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        // 1s..3s of the timeline is 2s of src:1: it covers the end of the first clip and the head of the second.
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 1, "in": "0s", "at": "1s", "to": "3s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(r.text.contains("overwrote") && r.text.contains("V1@0 cut to") && r.text.contains("V1@60 cut to"), "{}", r.text);
        let want = vec![(0, 30, 0, 0), (30, 90, 0, 1), (90, whole, 90, 0)];
        assert_eq!(spans(&srv, Lane::V1), want);
        assert_eq!(spans(&srv, Lane::A1), want);
        let u = run_undo(&mut srv, &json!({}));
        assert!(!u.is_error, "{}", u.text);
        assert_eq!(spans(&srv, Lane::V1), vec![(0, 60, 0, 0), (60, whole, 60, 0)]);
    }

    #[test]
    fn overwrite_past_the_end_keeps_the_gap() {
        let mut srv = bench();
        let whole = spans(&srv, Lane::V1)[0].1;
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 1, "at": format!("{}f", whole + 30), "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1), vec![(0, whole, 0, 0), (whole + 30, whole + 60, 0, 1)]);
        assert_eq!(spans(&srv, Lane::A1).len(), 2);
    }

    #[test]
    fn an_audio_only_source_lands_on_an_audio_lane_and_a_partnerless_insert_refuses() {
        let mut srv = bench();
        // Into V1+A1 material an audio-only insert would pull the take's picture and sound apart.
        let r = ed(&mut srv, json!([{ "op": "insert", "src": 2, "at": "1s", "length": "1s" }]));
        assert!(r.is_error && r.text.contains("one take with the V1 clip"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::A1).len(), 1, "nothing changed");
        // Overwrite moves nothing, so it is fine, and only A1 holds the new clip.
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 2, "at": "1s", "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(spans(&srv, Lane::A1).iter().any(|c| c.3 == 2 && c.0 == 30));
        assert!(spans(&srv, Lane::V1).iter().all(|c| c.3 == 0));
        let v = ed(&mut srv, json!([{ "op": "overwrite", "src": 2, "at": "1s", "length": "1s", "lanes": ["V1"] }]));
        assert!(v.is_error && v.text.contains("no picture"), "{}", v.text);
    }

    #[test]
    fn a_still_takes_a_video_lane_and_a_length() {
        let mut srv = bench();
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 3, "at": "1s", "length": "2s" }]));
        assert!(!r.is_error, "{}", r.text);
        assert!(spans(&srv, Lane::V1).contains(&(30, 90, 0, 3)), "{:?}", spans(&srv, Lane::V1));
        assert!(spans(&srv, Lane::A1).iter().all(|c| c.3 == 0), "a still has no sound");
        let bad = ed(&mut srv, json!([{ "op": "overwrite", "src": 3, "in": "1s", "at": "0s" }]));
        assert!(bad.is_error && bad.text.contains("still"), "{}", bad.text);
    }

    #[test]
    fn a_locked_lane_refuses_and_the_default_skips_it() {
        let mut srv = bench();
        let r = ed(&mut srv, json!([{ "op": "lane_flags", "lane": "A1", "locked": true }]));
        assert!(!r.is_error, "{}", r.text);
        let before = spans(&srv, Lane::V1);
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 1, "at": "1s", "length": "1s", "lanes": ["V1", "A1"] }]));
        assert!(r.is_error && r.text.contains("locked"), "{}", r.text);
        assert_eq!(spans(&srv, Lane::V1), before);
        // With no lanes the default is the first unlocked audio lane: A2, added first.
        let r = ed(&mut srv, json!([{ "op": "lane_add", "kind": "audio" }]));
        assert!(!r.is_error, "{}", r.text);
        let r = ed(&mut srv, json!([{ "op": "overwrite", "src": 1, "at": "1s", "length": "1s" }]));
        assert!(!r.is_error, "{}", r.text);
        let a2 = spans(&srv, Lane::new(LaneKind::Audio, 1));
        assert_eq!(a2, vec![(30, 60, 0, 1)]);
        assert_eq!(spans(&srv, Lane::A1).len(), 1, "the locked lane is untouched");
    }

    #[test]
    fn bad_ranges_and_arguments_refuse_by_name() {
        let mut srv = bench();
        let steps = srv.ledger.len();
        for (op, want) in [
            (json!({ "op": "insert", "src": 1, "at": "1s", "out": "2s", "length": "1s" }), "not both"),
            (json!({ "op": "insert", "src": 1, "at": "2s", "to": "1s" }), "must be after"),
            (json!({ "op": "insert", "src": 1, "at": "1s", "to": "2s", "length": "1s" }), "not both"),
            (json!({ "op": "insert", "src": 1, "at": "1s", "in": "3s", "out": "2s" }), "not inside"),
            (json!({ "op": "insert", "src": 1, "at": "1s", "lanes": [] }), "empty"),
            (json!({ "op": "insert", "src": 1, "at": "1s", "lanes": ["V1", "V2"] }), "is not a lane on this timeline"),
            (json!({ "op": "insert", "src": 9, "at": "1s" }), "no src:9"),
            (json!({ "op": "insert", "src": 1 }), "missing `at`"),
        ] {
            let r = ed(&mut srv, json!([op]));
            assert!(r.is_error && r.text.contains(want), "{want}: {}", r.text);
        }
        assert_eq!(srv.ledger.len(), steps, "refusals are no undo steps");
    }
}
