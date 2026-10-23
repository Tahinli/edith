//! The agent's cheat sheet: the `edith://help` resource, the `help` tool, and the
//! first lines of the initialize `instructions`. The tool list and the edit op
//! list are read from the live tool table, so the sheet cannot drift from what
//! the server accepts. The edit op manual is the `edit` tool's description, so the
//! per-op entries are parsed from it rather than copied.

use serde_json::{Value, json};

use crate::server::{self, Reply, Server, Tool};

/// The fixed topics, besides the edit op names and the tool names.
const TOPICS: [&str; 20] = [
    "ops",
    "times",
    "recipes",
    "first_video",
    "podcast",
    "lanes",
    "ripple",
    "export",
    "subtitles",
    "captions",
    "project",
    "burn_captions",
    "visualizer",
    "looks",
    "overlays",
    "vertical",
    "sequences",
    "multicam",
    "stretch",
    "unsupported",
];

/// Tool groups in the order the sheet lists them. A registered tool that is in no
/// group lands under `other`, and a test fails if that happens.
const GROUPS: &[(&str, &[&str])] = &[
    ("project", &["project_new", "templates", "project_open", "project_save", "project_collect", "project_export_timeline", "project_import_timeline", "project_status", "project_settings"]),
    ("media", &["media_probe", "media_import", "media_list", "media_remove", "media_relink", "media_bin"]),
    ("look", &["timeline", "clip_info"]),
    ("see", &["frame", "contact_sheet", "scopes"]),
    ("scan", &["scenes", "stabilize_status", "track", "track_status"]),
    ("hear", &["levels", "silence", "cut_silence", "loudness", "normalize", "clean_voice"]),
    ("edit", &["edit", "sequence", "multicam_create", "multicam_flatten", "effect", "undo", "redo", "checkpoint", "restore", "checkpoints"]),
    ("export", &["export_start", "export_presets", "export_status", "export_cancel", "export_captions", "render_preview"]),
    ("listen", &["transcript", "find_words", "auto_captions"]),
    ("captions", &["captions"]),
    ("speak", &["voices", "voiceover"]),
    ("record", &["record"]),
    ("help", &["help"]),
];

/// The opening line of the sheet. Its topic list is `TOPICS`, so it cannot drift from what `section` answers.
fn header() -> String {
    format!(
        "edith help: one page for driving the editor over MCP. Tool descriptions carry the full argument lists. Topics: {}.",
        TOPICS.join(", ")
    )
}

const ADDRESSES: &str = "\
ADDRESSES
  V1@450       clip: the clip on lane V1 that starts at frame 450 (starts are unique in a lane)
  V1#0         clip by index, 0-based: V1#0 is the first clip on V1, V1#2 the third; resolved at once, so do not hold it across edits
  V1 A1 S1     lanes, named as the GUI names them (S1 holds captions)
  src:2        a source, as media_import reports it (src:2 interview_a.mp4)
  C@450 / C#1  chapter: C@450 the chapter that starts at frame 450, C#1 the first chapter in the list
  M@450 / M#1  comment marker: M@450 the first marker on frame 450, M#1 the first marker in the list
  Every edit reply lists the NEW addresses of what it touched. Use those, not old ones.
  Addresses in one batch refer to the timeline as the previous op left it.";

const TIMES: &str = "\
TIMES
  450f           frames (the model works in frames; replies show 450f (15.000s))
  15.0s or 15    seconds
  00:00:15:00    timecode at the project fps, HH:MM:SS:FF
  02:14.300      minutes, seconds, milliseconds (MM:SS.fff)
  end            the end of the timeline: the latest end of any lane, not the end of one lane
  Seconds round to the nearest frame: 2.5 s at 25 fps is frame 63 (62.5 rounds up).
  A distance (`by` of roll, slip and slide) takes these forms with a sign: -15f, +0.5s, -00:00:01:00. A bare number is seconds.";

const VALUES: &str = "\
VALUES
  color: brightness -1..1 (0 is none), contrast 0..2 (1 is none), saturation 0..2 (1 is none), tint -1..1 (0 is none)
  gain db -60..24 · lane_gain db -60..12 · speed 0.25..4
  A still image is placed 5 s long unless the project says otherwise (project_settings still_secs, 0.5 to 600 s), and a trim can take it up to 600 s.";

const GROUPS_AND_LINKS: &str = "\
GROUPS AND LINKS
  Clips that share a link (a take, such as V1+A1) are one unit in the GUI.
  The ops tagged [group] follow the whole take: trim, roll, slip, slide, move, delete, speed, sub_move. trim scope:\"clip\" unlinks the edge's clip first.
  fade, dissolve and transition reach the linked sound too (linked:false keeps them on the one clip).
  Every other op acts on the one clip alone: lift leaves a gap on that clip's lane only. group and ungroup set and clear a take.";

const UNDO: &str = "\
UNDO
  One edit call is one undo step. Its ops run in order; if one fails, the earlier ones roll back and the error names it.
  undo {steps} and redo {steps} take back or re-apply whole batches. dry_run:true reports, then rolls back.
  A batch with no label is named after its first op (\"place V1\", \"trim V1@0\"); edit {label:\"…\", ops} names it.
  media_remove {source} of a source other than the last one renumbers the sources after it, which clears undo: refused unless force:true.
  checkpoint {name} saves the whole project under a name; restore {name} returns to it (it clears undo history).";

/// `help {topic:"first_video"}`: the order to build a first video in, saving as it goes.
const FIRST_VIDEO: &str = "\
FIRST VIDEO: a first cut from nothing, one stage at a time. Save after every stage (project_save), and before you end a run: nothing is on disk until you do.
  1. project_new {path:\"<dir>/talk.edith\", fps:30, resolution:\"1920x1080\"} — an empty project at a new path (nothing is written yet).
  2. media_import {paths:[\"<clip>.mp4\", \"<music>.mp3\"]} — the files become src:0, src:1 ...; nothing is placed. A file already imported says \"already imported as src:N\" and adds nothing. project_save.
  3. Each extra sound lane (music on A2, narration on A3) needs its own lane_add, one op per lane. For this recipe that is two: edit {ops:[{op:\"lane_add\", kind:\"audio\"}, {op:\"lane_add\", kind:\"audio\"}]} adds A2, then A3. A place or append to a lane that does not exist refuses; lane_add it first. project_save.
  4. edit {ops:[{op:\"place\", source:\"src:0\", at:\"0s\"}]} puts the picture on V1 and its sound on A1; edit {ops:[{op:\"append\", source:\"src:0\"}]} adds after the last clip. length:\"5s\" sets the duration from in instead of out. Music bed: edit {ops:[{op:\"append\", source:\"src:1\", lane:\"A2\", until:\"30s\"}]} (A2 comes from step 3; src:1 is a song of 30 s or more, so the bed is one clip; until LOOPS a shorter source into one clip per repeat, and trims a longer one to the span). project_save.
  5. voiceover {text:\"...\", pause:0.4, place:{lane:\"A3\", at:\"0s\"}} needs a project path (project_new or project_open); the WAV is written beside the .edith file in voiceover/, even before project_save. It imports the WAV as a source and places it when place is given. record {action:\"start\", lane:\"A3\"} then record {action:\"stop\"} is the microphone version: it records from the audio input and places the take the same way. voiceover speaks text with Piper and never records. The voiceover is unsaved until project_save: the project file does not name its WAV before then. project_save.
  6. auto_captions {target:\"A3\"} transcribes the narration lane and writes the words to S1. project_save. (A1 is the picture's own sound: it has no words to caption here.)
  7. export_start {out:\"<dir>/talk.mp4\"} returns job:1 at once; export_status {job:1} reports progress until it is done.
  Length: a picture shorter than the music bed leaves black at the end, since the bed plays on over nothing. Trim the bed to the picture: edit {ops:[{op:\"trim\", clip:\"A2@0\", edge:\"out\", to:\"<the picture's end>\"}]}. Or make the picture longer: for a still, edit {ops:[{op:\"trim\", clip:\"V1@0\", edge:\"out\", to:\"<the bed's end>\"}]}; for video, append more picture after it. timeline shows where each lane ends.
  Peaks: export_status warns when the job finishes if the file's true peak is above -1 dBTP (exactly -1 is within spec); the ceiling to aim at is -1 dBTP for lossless (wav, flac) and -2 dBTP for lossy (mp4, mkv, webm, mp3, ogg, m4a), since encoding can overshoot the mix; a true peak over 0 dBTP clips. Add the master limiter before export with project_settings {limiter:{on:true, ceiling_db:-1}} (ceiling_db:-2 for a lossy export); or normalize {target:\"mix\", allow_limiter:true, true_peak:-2} (-1 for lossless) sets the level in one undo step, and turns the limiter on at that ceiling only when the peak would otherwise hold the gain back (its reply says so).
  After each stage, read the result with timeline (or frame / contact_sheet for a picture check). A reply that says \"no change\" or names a refusal means that step did not happen: fix it, then go on.";

const PODCAST: &str = "\
PODCAST: two speakers on A1 and A2, a music bed on A3 ducked under both, each speaker levelled on its own, then captions from both. Save after each stage (project_save).
  1. project_new {path:\"<dir>/show.edith\", fps:30, resolution:\"1920x1080\"}, then media_import {paths:[\"<host>.wav\", \"<guest>.wav\", \"<bed>.mp3\"]}.
  2. Speakers: edit {ops:[{op:\"place\", source:\"src:0\", lane:\"A1\", at:\"0s\"}]} puts the host on A1. A2 does not exist yet, so add it first: edit {ops:[{op:\"lane_add\", kind:\"audio\"}]}; then edit {ops:[{op:\"place\", source:\"src:1\", lane:\"A2\", at:\"0s\"}]} puts the guest there. Both start at 0 s, because each was recorded on its own track for the whole conversation: the two files play together, not one after the other.
     Cut their silences together: cut_silence {lanes:[\"A1\",\"A2\"], dry_run:true}, then the same without dry_run. A joint cut (lanes with no target) removes a pause only where every listed lane is quiet at once, and a lane with no sound there (a gap between its clips, or no clip) counts as quiet. So while one speaker talks, the other lane is quiet but the talker's is not, and nothing is cut; only the stretches where both are quiet, the gaps between turns, are shortened, and the same span comes out of both lanes, so the turns stay in step. The reply's \"pauses now\" is measured on that joint quiet. Caption lanes ripple with the cut; locked caption lanes stay.
  3. Bed: edit {ops:[{op:\"lane_add\", kind:\"audio\"}]} adds A3, then edit {ops:[{op:\"append\", source:\"src:2\", lane:\"A3\", until:\"<end of the talk>\"}]} lays the bed there. Lay it after the cuts in step 2, so <end of the talk> is where the talk now ends (timeline shows each lane's end). A cut made after the bed is placed (the step 5 fillers, or a later cut_silence) leaves the bed running past the talk: trim it to the new end with edit {ops:[{op:\"trim\", clip:\"A3@0\", edge:\"out\", to:\"<the talk's new end>\"}]}. Duck it under each speaker: edit {ops:[{op:\"duck\", lane:\"A3\", under:\"A1\"}, {op:\"duck\", lane:\"A3\", under:\"A2\"}]}.
  4. Level each speaker alone: normalize {lanes:[\"A1\",\"A2\"], each:true, lufs:-16}. Its reply gives each lane's loudness before and after. A speaker with high peaks often cannot reach -16 LUFS under the -1 dBTP ceiling: the reply holds that lane to the true-peak ceiling (structuredContent carries it as capped_by: true_peak), and the lane stops short. This is expected for speech. A quiet speaker needs no manual clip gain: normalize takes the lane up to its +12 dB lane gain limit and puts the rest on that lane's clips (each up to +24 dB of clip gain), all in the same undo step. Only when even that is not enough does the reply say so with capped_by: clip_gain_limit; then raise that speaker's clip gain yourself (edit {ops:[{op:\"gain\", clip:\"A1@0\", add_db:3}]}) and normalize the lanes again. Then the whole mix: normalize {target:\"mix\", lufs:-16, allow_limiter:true, true_peak:-1} for a lossless or WAV export, true_peak:-2 for m4a or mp3 (lossy encoding overshoots the mix); the limiter is what lets the mix reach the target when the peak alone would hold it back. If the mix still falls short (its reply gives short_by_lu), the reply names the cap that holds it (capped_by): accept a target the peaks allow (-18 LUFS, say), or raise the quiet lane's clip gain as above. dry_run:true on either shows the plan first.
  5. Fillers (optional, before the captions): do not cut a filler on one speaker's lane with find_words {cut:true} alone. A ripple moves only that lane, so the other speaker's lane falls out of step. Use the no-ripple form instead: find_words {query:\"um\", target:\"A1\", cut:\"lift\"} removes each match and leaves a hole where it was, and nothing moves. Pass the step 6 hint here too (hint:\"Jane Doe, LUFS\"): names and jargon come out spelt as the talk spells them, so the search reads them right. Or cut both lanes together, only at a joint pause (cut_silence with no target, step 2).
  6. Captions from both speakers into one lane: auto_captions {target:\"A1\", lane:\"S1\", hint:\"Jane Doe, LUFS\"}, then auto_captions {target:\"A2\", lane:\"S1\", hint:\"Jane Doe, LUFS\"}. Each call transcribes its own lane and adds its cues to S1, so the two speakers' captions sit in one track. hint lists the names and words the talk uses; it may get their spelling and casing right, but that is not guaranteed, so check the names in the captions. A re-run on the same lane replaces the captions that lane made before; replace:true also removes overlapping captions that differ from what was heard now (a hand edit, or the other speaker's), so use it only when those are meant to go.
  7. Check with timeline. A project with no picture (only A lanes) exports an .mp4 as sound alone, and its captions are dropped: export_start {out:\"<dir>/show.m4a\"} for the sound, or, for a captioned video, place a still on V1 and trim its out edge to the end of the talk (the still recipe in help {topic:\"recipes\"}). Then export_start {out:\"<dir>/show.mp4\"} (or the .m4a) and export_status {job:1} until it is done.";

const RECIPES: &str = "\
RECIPES
  Rough cut from ranges: keep lays source ranges end to end (one undo step).
    edit {ops:[{op:\"keep\", source:\"src:0\", ranges:[[\"10.0s\",\"22.5s\"],[\"40.0s\",\"52.0s\"]], lane:\"V1\"}]}
    edit {ops:[{op:\"assemble\", items:[{source, in?, out?}]}]} does the same, one item per range, across several sources; each item is laid end to end after the last.
  Trim: edit {ops:[{op:\"trim\", clip, edge, to}]} moves that edge to a timeline time. `to` is absolute (where the edge lands), not a length: edit {ops:[{op:\"trim\", clip:\"V1@0\", edge:\"out\", to:\"2s\"}]} makes the clip 2 s long from 0. There is no delta form. Add ripple:true to close the hole the edge leaves and move what follows with it (Premiere's ripple trim, Q and W in the editor).
    edit {ops:[{op:\"trim\", clip, edge, to, scope:\"clip\"}]} on a take of three or more lanes: the other members stay linked to each other only if they have matching bounds. A 60 s picture with a 30 s bed cannot stay linked, so that trim is refused (\"its partners could not stay linked\") and nothing changes: give the members that stay together the same bounds first, or trim with the default scope (the whole take moves). A locked partner refuses (unlinking changes it).
  Append to a lane that does not exist yet: add the lane first, edit {ops:[{op:\"lane_add\", kind:\"audio\"}]}. An append op naming A2 does not make A2: edit {ops:[{op:\"append\", source:\"src:1\", lane:\"A2\"}]} refuses until A2 exists.
    assemble refuses a still image (src:N is a still): add a still with edit {ops:[{op:\"append\", source}]}, which puts it at the end of V1 with no hole, or edit {ops:[{op:\"place\", source, at:\"end\"}]}, which puts it at the timeline end (the latest end of any lane). They are not alternatives for one job: append extends V1, and place at:\"end\" goes to the timeline end, which leaves a hole on V1 when V1 ends sooner (a bed running on past it, say). Then trim its out edge (edit {ops:[{op:\"trim\", clip, edge:\"out\", to}]}) to change its length (5 s by default).
  Countdown leader: edit {ops:[{op:\"generator_add\", kind:\"countdown\", from:5, at:\"0s\"}]} puts a 5 s countdown on V1 at 0 (add `lane`, `at` to put it elsewhere); insert_time first if the programme should start after it.
  Tint everything under with an adjustment layer: edit {ops:[{op:\"adjustment_add\", at:\"0s\", duration:\"30s\"}, {op:\"color\", clip:\"V2@0\", tint:0.3, saturation:0.8}]} grades every lane below V2 for those 30 s; effect add on the layer does the same for blur or a vignette. enable {clip:\"V2@0\", on:false} switches it off without removing it. A transform on an adjustment layer moves the whole composite below it (uncovered area goes black); crop only limits where the layer reaches, so cropped-away areas keep the untouched picture. Adjacent adjustment layers hard-cut.
  Reverse a clip: edit {ops:[{op:\"reverse\", clip:\"V1@0\"}]} toggles it (on:true or on:false sets it), picture and sound together with its take. It keeps its place and length and composes with speed: reverse, then speed x:2 plays it backward at double rate. clip_info and timeline say reversed.
  Cut silence: read the regions, preview the cut, then cut.
    silence {target:\"A1\"}, or {target:\"src:2\"} to scan a source (threshold_db, min_silence, padding, min_keep)
    cut_silence {target:\"A1\", dry_run:true}, then cut_silence {target:\"A1\"}. It cuts only pauses of min_silence or longer. When none qualifies, the reply names the longest quiet it found and cuts nothing, so lower min_silence (or padding) and run it again.
    min_silence (default 0.5 s, judged before padding) is the shortest quiet run it cuts: shorter ones are ignored.
    keep:N is the total pause left in each silence, padding included (default 0.4). The padding at each end is never cut; under 2 × padding the pause is 2 × padding. The reply says \"pauses now X s\". A silence at or under keep is left alone, and when none is longer the reply says nothing was cut. lanes: with a target, it is the ripple scope, judged on the target alone (the target's lane is added if missing); by default the target's lane and its linked lanes, and a mix target cuts every lane. Without a target, two or more audio lanes are cut jointly where all are quiet: lanes:[\"A1\",\"A2\"] makes a joint cut with no target: the silence is where every listed lane is quiet at once, and the listed lanes are cut together, so their turns stay in step. Locked lanes: a locked lane named in lanes, or linked to the target, refuses the cut (unlock it with lane_flags); a mix's default scope (every lane) refuses the same way, naming each locked lane, and changes nothing. Unlisted audio lanes the cut runs through are reported as out of step.
    silence: judged inside clips only; the timeline gaps between a lane's clips are structure and are reported, never listed or cut. Default threshold: 30 dB under the target's median speech level (−90..−10 dBFS); the reply names the value used. min_silence is judged before padding.
    cut_silence: a silence that runs to the end of its clip keeps `keep` in all after the last speech, its end padding included. Gaps between clips are never cut.
    Caption lanes ripple with the cut; locked caption lanes stay (a locked caption lane in the scope is skipped with a note).
  Normalize loudness: measure the mix, then set it.
    loudness {target:\"mix\"}, then normalize {target:\"mix\", lufs:-14}; its reply leads with whether the target was reached.
    When the true-peak ceiling holds the gain back (the reply's capped_by is true_peak), add allow_limiter:true with true_peak set to the ceiling (-1 for lossless, -2 for lossy): the limiter then lets the mix reach the target, and the reply says it switched the limiter on.
    dry_run:true reports the planned gain per lane and changes nothing.
    Each lane alone: normalize {lanes:[\"A1\",\"A2\"], each:true, lufs:-16} sets each listed audio lane on its own, so each one alone measures lufs over its clips (two speakers at different levels come out even). Each lane's gain moves by itself, with the true-peak ceiling checked per lane; one undo step for all of them. Needs lanes, not target, and no allow_limiter.
    Targets (lufs): streaming, YouTube and Spotify -14; Apple -16; podcasts -16 (stereo); broadcast (EBU R128) -23; TikTok and Instagram about -14.
  Copy a look: edit {ops:[{op:\"copy_look\", from, to:[addrs] | to_lane, what?}]} copies the look of clip `from` onto each clip in `to` (or every clip on
    `to_lane`) in one undo step. what is a list of \"color\" (the default), \"transform\", \"fit\", \"effects\" (picture clips only), \"eq\", \"gain\" (sound
    clips only) or \"speed\" (rate, ramp and reverse, from and to any media clip); a target the part does not fit is named as skipped. from must carry a look: a plain clip has none, so copying it onto a plain clip replies \"nothing to copy\" (no change). e.g. edit {ops:[{op:\"copy_look\", from:\"V1@0\", to:[\"V1@30\"], what:[\"color\",\"transform\"]}]} Prerequisite: V1@0 is a picture clip with a look (a colour grade from the color op, or a transform) and V1@30 a second picture clip starting at frame 30; both must already exist.
  Effects: vignette plus soft blur. effect {op:\"add\", clip:\"V1#0\", kind:\"vignette\", params:{amount:0.6}}, then effect {op:\"add\", clip:\"V1#0\", kind:\"blur\", params:{radius:4}}; effect {op:\"list\", clip:\"V1#0\"} shows the stack, effect {op:\"set\", clip:\"V1#0\", index:1, on:false} switches the blur off. The stack runs in order on the placed picture, in preview and export; distances are pixels at 1080 lines. Each op is one undo step. Prerequisite: V1#0 is a picture clip.
  Green screen over a background: put the background on V1 and the green-screen take on V2, then effect {op:\"add\", clip:\"V2#0\", kind:\"chroma_key\", color:\"#00ff00\", params:{tolerance:0.35, softness:0.1, spill:0.5}}; the keyed-out part shows V1. effect {op:\"list\"} gives every kind with its ranges; masks (rect_mask, ellipse_mask) and blend {mode:\"screen\"} work the same way. pixelate {size} mosaics the picture (size is the block in pixels at 1080 lines); after a mask it censors only that shape, e.g. effect {op:\"add\", clip:\"V1#0\", kind:\"pixelate\", size:24} then an ellipse_mask. A mask moves with effect {op:\"mask_keys\"} (the ops topic lists its arguments).
  Grade a clip: wheels to balance, curves to shape, a LUT for the look, the sliders to trim; read the scopes between steps.
    edit {ops:[{op:\"wheels\", clip:\"V1@0\", lift:[0, 0.05, 0, -0.03]}, {op:\"curves\", clip:\"V1@0\", master:[[0.25, 0.2], [0.75, 0.8]]}, {op:\"lut\", clip:\"V1@0\", path:\"~/luts/warm.cube\", mix:0.7}]}
    scopes {at:\"2s\"} gives the waveform, the RGB parade and the vectorscope of that frame as numbers: crushed and clipped shares, each channel's spread, the cast.
  Check a cut before exporting it.
    timeline {range:{from:\"10s\", to:\"30s\"}}, contact_sheet {range:{from:\"10s\", to:\"30s\"}, count:12}, frame {at:\"450f\"}
  Cut by transcript: read the words, find the filler or phrase, then cut its spans. The filler list is um, uh, er, ah, hmm and
    like, but like is a real word too often: check each hit with its context before cutting it, or search a phrase instead.
    The transcript usually leaves fillers out but not always. A filler query runs with the prompt that keeps disfluencies, so a plain find_words {query:\"um\"} still finds the ones that remain, with no flag.
    transcript {target:\"A1\"}, then find_words {query:\"um\", target:\"A1\"} lists each match with its span; cut the spans with edit {ops:[{op:\"cut_regions\", regions:[[\"450f\",\"512f\"]]}]}
    or find_words {query:\"um\", target:\"A1\", cut:true} removes every match in one step (which:\"first\" or which:[0,2] picks some; dry_run:true reports only). keep:\"first\" or \"last\" keeps one word of a repeat such as \"I I\". The cut takes the target's lane and its linked lanes, or every lane for a mix or src target; help {topic:\"ripple\"} says which lanes move. On one speaker's lane of a two-speaker show, cut:true ripples only that lane and desyncs the other: use cut:\"lift\" instead, which leaves a hole and moves nothing (podcast has the joint-pause option). Caption lanes ripple with the cut; locked caption lanes stay (a locked caption lane in the scope is skipped with a note). A locked lane the cut would move refuses it, dry runs too (unlock it with lane_flags); a mix's default scope skips its locked lanes with a note. With cut, each edge snaps to a pause within 150 ms of Whisper's time, so a cut lands in quiet; snap:false cuts at Whisper's times.
  Auto captions: transcribe and place captions, then adjust the caption track.
    auto_captions {target:\"A1\"} writes the cues to S1; hint:\"Jane Doe, LUFS\" lists the words the talk uses; it may help their spelling and casing, and near-miss spellings of them are corrected after transcription (the reply names each one), but that is not guaranteed. help {topic:\"captions\"} covers the rest of the workflow.
  Shorts captions: animated, a few words at a time, the word being spoken highlighted.
    auto_captions {target:\"A1\"}, then project_settings {captions:{animation:\"karaoke\", words_per_line:3, uppercase:true, highlight:\"#ffd600\"}} (saved with the project; one undo step),
    check a frame mid-word: frame {at:\"2s\", burn_captions:{size:70, place:\"centre\", outline:\"thick\", boxed:false}}, then export_start {out, burn_captions:{size:70, place:\"centre\", outline:\"thick\", boxed:false}}.
    Or per call, without touching the project: burn_captions:{animation:\"karaoke\", words_per_line:3, box:true}. animation word is the one-big-word style; pop and bounce are the others. help {topic:\"burn_captions\"}.
  Cut to the beat: mark the music's beats, then cut on the markers (cuts snap to markers).
    edit {ops:[{op:\"beats\", clip:\"A2@0\"}]} finds the tempo of that clip's sound and marks every beat as a yellow comment marker noted \"Beat\"; every:2 or every:4 marks every second or fourth beat for bars. The reply gives the BPM, the count and the M@ addresses. Running it again replaces the earlier beat markers of that clip. A picture-only clip has no sound and is refused.
  Intro title and a lower third: a centred card over black, then a boxed caption over the picture.
    edit {ops:[{op:\"title_add\", at:\"0\", duration:3, text:\"Episode 17\", place:\"centre\", fade_in:12, fade_out:12}, {op:\"title_add\", at:\"5.0s\", duration:4, text:\"Jane Doe, director\", place:\"bottom\", boxed:true, fade_in:12, fade_out:12}]}
    A title past the last clip extends the timeline; edit {ops:[{op:\"title_set\", title:\"T2\", size:60}]} changes one, edit {ops:[{op:\"title_remove\", title:\"T1\"}]} takes it off (the titles after it renumber: T2 becomes T1), and timeline lists them as T lines.
  A title can start in one of the app's looks with preset (lower_third, centered or caption_bar): edit {ops:[{op:\"title_add\", at:\"5.0s\", duration:4, text:\"Jane Doe\", preset:\"lower_third\"}]}; fields given with it win. A title can carry a look: color (\"#ffcc00\"), weight (regular|bold; Latin letters, other scripts keep their one weight), hplace (centre|left|right: the side the block sits against, the margin from the edge), align (centre|left|right: how its lines line up inside the block), box_color (\"#000000\" keeps the opacity, \"#000000c8\" sets it) and box_opacity (0..1). A lower third: edit {ops:[{op:\"title_add\", at:\"5.0s\", duration:4, text:\"Jane Doe\\nDirector\", place:\"bottom\", hplace:\"left\", align:\"left\", weight:\"bold\", boxed:true, box_color:\"#112233\", box_opacity:0.8}]}. A project that uses one saves as format v49 (an older edith refuses it by name). A title can animate in and out: in_anim and out_anim are none, fade, typewriter (letters appear one by one), slide_up, slide_left, pop (grows from 60% with a slight overshoot) or word_by_word; anim_secs is how long each takes (a bare number is seconds, default 0.5). An exit is the entrance played backwards. Example: edit {ops:[{op:\"title_add\", at:\"0\", duration:4, text:\"Episode 17\", in_anim:\"typewriter\", out_anim:\"fade\", anim_secs:0.6}]}. An animated title saves as format v54 (a titleanim line; an older edith refuses it by name).
  Black lead-in for an intro title: edit {ops:[{op:\"insert_time\", at:\"0\", duration:\"3s\"}]} opens black before the picture, then title_add at 0 sits over that black.
    edit {ops:[{op:\"insert_time\", at:\"0\", duration:\"3s\"}, {op:\"title_add\", at:\"0\", duration:3, text:\"Episode 17\", place:\"centre\", fade_in:12, fade_out:12}]}
  Speed a clip that has narration over it (the examples need a picture clip at V1@0, and the narration lane A3 from first_video): narration on its own lane (A3, as in first_video) is outside the ripple by default, so edit {ops:[{op:\"speed\", clip:\"V1@0\", x:0.5, ripple:true}]} changes the picture's length and the narration keeps its times.
    The reply lists each clip left in place: a clip on another lane that spans the change (the music bed on A2, a narration clip on A3) stays where it is.
    To move the narration with the picture, name its lane (prerequisite: A3 exists and holds the narration clip): edit {ops:[{op:\"speed\", clip:\"V1@0\", x:0.5, ripple:true, lanes:[\"V1\",\"A1\",\"A3\"]}]}. A lane named in lanes refuses a clip that spans the change, so move or trim that narration clip first (edit {ops:[{op:\"move\", clip, at}]}, or a trim).
    Clips that start after the sped clip move by the length change on the lanes in scope. lanes:[\"V1\",\"A1\"] is the same as the default.
  Dip music under a voice line: a volume envelope on the music clip, over the line's span. The span must lie inside that one clip: a file shorter than the span
    loops into several clips under until, and a dip cannot cross them, so use a song at least as long as the span (the first_video bed is one clip).
    Prerequisite: A2@0 is the music clip and runs to at least 25 s.
    edit {ops:[{op:\"gain_dip\", clip:\"A2@0\", from:\"10s\", to:\"25s\", db:-18, ramp:\"0.5s\"}]}
    The ramps start from the level there now; clip_info lists the points as timeline times. gain_key and gain_key_remove adjust one point, gain_ramp draws a single ramp, gain_keys replaces the whole envelope. For a dip that follows the speech by itself, edit {ops:[{op:\"duck\", lane:\"A2\", under:\"A1\"}]} is the automatic alternative.
  Export: start, then poll until done.
    export_start {out:\"~/cut.mp4\"} returns job:1; export_status {job:1} until it reports the outcome.";

const LANES: &str = "\
LANES
  V1.. are picture lanes, A1.. sound lanes, S1.. caption lanes. Names are the GUI's.
  lane_add {kind: video|audio|subtitle} adds one; lane_remove {lane}; lane_move {lane, to}: to is the 1-based position in the lane list as timeline prints it, and the reply maps every moved clip old → new.
  Lanes are named by position, so a lane_move renumbers the lanes between its two places: lane_move {lane:\"V2\", to:1} makes the old V1 into V2. The reply lists each rename; use the new names after a move.
  place {lane} sets where the picture goes; its sound still goes to A1 (or audio_lane) and stays linked to it, as with no lane (V1 and A1). audio:false places the picture alone.
  place overwrite:true replaces only the span the new clip covers: a clip it partly covers keeps the part outside that span.
  insert {source, in?, out?, at, to?, length?, lanes?} and overwrite {…}: the three-point edit. The source range in..out (the whole file when none) goes at `at`; `to` (a timeline out mark) or `length` sets the duration from in instead of out. It lands on `lanes` (one video lane, one audio lane), else the first unlocked V and A lanes the source has, linked as one take (a still on a video lane, a song on an audio lane). insert cuts the clip `at` falls inside and moves what follows later on those lanes only; it refuses, by name, when that would pull a linked take apart (add the partner lane to `lanes`). overwrite replaces what is there, trimming or splitting it, and moves nothing. A locked lane refuses; each is one undo step.
  lane_name {lane, name}: the display label of a lane (like a track name), at most 40 characters; \"\" clears it. The address stays V1/A2: every op still takes V1, never the name. timeline shows the name on the lane's header line. One undo step; a locked lane refuses it.
  lane_flags {lane, hidden?, muted?, locked?}: hidden applies to picture lanes, muted to sound
    lanes (the other flag is refused there). A locked lane is never changed: an op that would change
    it refuses, except that the titles and captions a ripple carries past it skip it with a note.
  levels {target:\"A1\"} on a muted lane shows the lane's own sound, with a note that it is silent in the mix and export.
  A sound-only source (a song, a WAV) goes on an A lane: edit {ops:[{op:\"place\", source, lane:\"A2\", at}]} or edit {ops:[{op:\"append\", source, lane:\"A2\"}]}.
  An mp4 with no picture is sound-only too: placed with no lane, it goes to A1 alone.
  A source with more than two sound channels (5.1, say) is mixed to stereo on export. media_probe lists each audio track's
    channels and sample rate, and media_list names a chosen track that is not the first (\"audio 1 (fra)\").
  audio_track:N on place and append picks which of the file's sound tracks plays (0-based, the numbers media_probe lists as \"audio track N\"); the first track is the default. An N past the last track is refused, and the reply lists the tracks. edit {ops:[{op:\"place\", source:\"src:0\", at:\"0s\", audio_track:1}]}.
  lane_gain {lane, db} sets a lane's level, -60..12 dB. Each clip's own gain adds on top.
  pan {clip, pan} and lane_pan {lane, pan} place sound between the speakers, -1 (left) to 1 (right), 0 the middle; a balance, so
    the far side is turned down and the near one stays. A clip's pan and its lane's stack. Stereo mixes only. edit {ops:[{op:\"pan\", clip:\"A1@0\", pan:-0.5}]}
  pitch {clip, semitones (-12..12)} shifts an audio clip's pitch in whole semitones and keeps its length (12 an octave up). edit {ops:[{op:\"pitch\", clip:\"A1@0\", semitones:12}]}
  Ducks stack: a lane can carry one duck per key lane (`under`), so several keys can pull it down. edit {ops:[{op:\"duck\", lane, under, off:true}]}
    removes that one; edit {ops:[{op:\"duck\", lane, off:true}]} with no under clears every duck on the lane.
  Not supported (help {topic:\"unsupported\"}): custom transition shapes.
    Do not search the tools for them.
  close_gaps {lane} removes the gaps on a lane. The timeline's gaps line counts them per lane.
  timeline {lanes:[\"V1\",\"A2\"]} shows only those lanes; timeline {range:{from,to}} only the clips that overlap it.
  The brief timeline shows up to 40 clips per lane, within a text cap of about 4 KB, and says how many more there are; timeline {lanes:[\"V1\"], page:N} pages one lane at 40 clips per page, and a lane header counts its graded clips past the cut.
  Source times on a clip are the source's own clock: `src 0.0s–4.0s` is the part of the file it plays.";

const EXPORT: &str = "\
EXPORT
  export_start {out, preset?, loudness?, format?, range?, size?, fit?, bitrate_kbps?, audio_kbps?, bit_depth?, prores_profile?, hevc_speed?, hdr?, audio_bit_depth?, chapters?, intra_only?, seat?, subtitles?, burn_captions?, overwrite?}
    returns job:<n> at once. Poll it with export_status {job?}; export_cancel {job} stops it and deletes the partial file.
  out: the file to write; ~/ is expanded and its folder must exist. The extension picks the format:
    .mp4 mp4 · .mkv av1 · .webm · .wav · .flac · .mp3 · .ogg · .m4a · .opus · .mov prores. Refuses an existing file unless overwrite:true.
  format (overrides the extension): mp4 (H.264) · av1 (AV1 in mkv) · av1-mp4 · hevc (I+P GOPs, mkv) · hevc-mp4 ·
    webm · wav and flac (lossless audio) · mp3, ogg and m4a (audio only; m4a is AAC in an mp4 box) · opus (audio only; Opus in Ogg at 48 kHz) · prores (Apple ProRes in a QuickTime .mov with PCM sound) ·
    png_sequence (a folder of PNG pictures, one per frame, named <folder>_00001.png; out is the folder, which may not already hold files unless overwrite:true; the project's size, no sound) · still (one PNG: the frame at the start of range, the poster frame) · gif (animated GIF of range, at most 30 s and 720 px wide: gif_width (default 480), gif_fps 1-60 (default 15), gif_loop (default true); pictures are composed at the project size and the GIF is refused by name for a range or width past those).
  loudness: a number of LUFS from -30 to -5, the whole mix's integrated loudness target (-14 for streaming, -16 for podcasts). It takes one gain, cut back where the true peak would pass the preset's ceiling or -1 dBTP; never a limiter. Beside preset it replaces the preset's own loudness. export_captions {format:\"srt\"|\"vtt\", path?, overwrite?} writes the timeline's captions (every cue on the subtitle lanes, in start order) to a text file: SRT or WebVTT, with picture cues left out. Without path it is `<first source> edit.<format>` in the Videos folder.
  preset: an id from export_presets (youtube_1080p, youtube_1080p60, youtube_1080p_hevc, youtube_4k, youtube_4k60, youtube_4k_hevc, shorts_reels, podcast, podcast_ogg, podcast_opus, archive, archive_prores, hdr10_hevc, archive_flac, archive_wav). It fills format, bitrate_kbps, audio_kbps, size, bit_depth, prores_profile, hdr and, for the podcast rows, a -16 LUFS / -1 dBTP loudness target applied to the exported sound. Anything you pass beside it overrides the preset's value; out's extension must match the preset's format. export_presets {} lists the rows.
  range {from, to}: that part of the timeline, to excluded. Times as in TIMES.
  seat: auto (the GPU for H.264 and HEVC where this machine has a GPU encoder, else the CPU; AV1 is always the CPU) · hardware (GPU or refuse) · software.
  size: this export's picture size, WIDTHxHEIGHT (1280x720) or a height with p (720p, 1080p, 2160p, 16:9 at that height); 1080x1920 is vertical. Both sides even. Omitted: the project's resolution, which stays as it is.
  fit: how every picture clip meets this export's canvas: keep (the default: each clip's own fit), fit (whole picture, bars) or fill (the canvas covered, the picture cropped). It changes this export's copy only; the project's clips keep their fit. Captions and titles keep their place on the canvas.
    A 16:9 timeline as 9:16: export_start {out, size:\"1080x1920\", fit:\"fill\"}. The reply names the size and the fit, and the clip count they applied to.
  bitrate_kbps: video, chosen from the picture size and frame rate when omitted. audio_kbps: 128, 192, 256 or 320.
  intra_only: hevc and hevc-mp4: true codes every picture as an IDR (intra only, large files, every frame a cut point). false, the default, codes I+P GOPs (an IDR a second, P pictures between). h264 mp4 uses it the same way.
  Captions: burn_captions draws one track into the picture, so every player and WebM show it. subtitles adds soft
    tracks a player can switch off (WebM has none). Both together: burned and soft. burn_captions alone: no soft track.
  subtitles (off unless given): true for every subtitle track, placed or not, or an array of track indices (0-based, as the
    captions tool lists them) for just those, soft, in the order given. Omitted or [] adds none. A picked track that is not placed
    on a caption lane is still written whole (all its cues). In mp4, overlapping cues are merged into one line each; mkv keeps them as they are.
  Still and picture-sequence exports draw each frame as the export would (titles and burnt captions included). They write no sound, so a subtitle track or a sound setting does not apply to them.
  An .mp4 of a timeline with no picture is the sound alone (AAC, no picture track); it is not refused. burn_captions is refused for it, and for the audio-only formats (m4a, mp3, ogg, opus, wav, flac): it draws into the picture, so drop burn_captions or add a picture.
  chapters (default true): the project's chapter markers are written into the file, timed from the start of the range. Written to mp4, av1-mp4, hevc-mp4, m4a, webm, opus and Matroska (.mkv: av1, hevc); wav, flac, mp3 and ogg say they are not written. chapters:false leaves them out, and chapters:\"markers\" writes the comment markers as the chapter list instead. The reply counts them, e.g. \"3 chapters\".
  bit_depth: 8 or 10 for the picture. 10 writes a 10-bit AV1 or HEVC (Main 10) file: format av1 (.mkv), av1-mp4, hevc (.mkv) or hevc-mp4; H.264 and every other format refuse it. Omitted: a 10-bit source gives a 10-bit AV1 or HEVC file, an 8-bit source 8-bit. The reply says which (\"output 10-bit, from the source\").
  prores_profile (format prores): proxy, lt, standard, hq (the default) or 4444, 4444xq. The profile is the quality and sets the rate; bitrate_kbps means nothing to ProRes. proxy, lt, standard and hq are 4:2:2, 4444 and 4444xq are 4:4:4. ProRes is always 10-bit. A ProRes .mov carries no subtitles.
  hevc_speed (hevc and hevc-mp4): default (the full search), fast (about 1.2-1.6x quicker, 0-3% more bits) or faster (about 1.5-2x quicker, up to 27% more bits). Other formats refuse it.
  audio_bit_depth (format prores): 16 or 24 (the default), the bits per sample of the PCM sound.
  hdr (hevc and hevc-mp4): off (the default), pq (HDR10) or hlg. The file is 10-bit BT.2020 tagged with that transfer, HDR sources are not tone-mapped, and bit_depth 8 is refused. Other formats refuse it.
  Pictures are 8-bit 4:2:0 except in a 10-bit AV1 or HEVC export. (ProRes is always 10-bit.) A 10-bit export still composes edits (grades, titles, captions, transitions, layers) at 8-bit precision and shifts them up. A stream copy keeps the source's own bits.
  The sound is as wide as its widest clip: a mono clip alone exports mono, and a mono clip beside a stereo one is widened to stereo.
  A burn-only export reports \"subtitles: burned into the picture (no soft track)\".
  The export keeps running while you do other things. If the client closes the server's input, the server waits for running exports to finish before it exits. A stop signal (SIGTERM or SIGINT) cancels them and deletes their partial files. Poll export_status until it finishes; export_cancel {job} stops one.
  export_status reports progress, ETA, the encoder and the outcome; a done export reports its file size.
  Queue: export_start {queue:true} lines an export up. It keeps its own copy of the timeline as it is at that moment (later edits do not change it) and starts when every earlier export has finished, one encode at a time, with no polling needed; export_status shows it as `waiting` with the number of exports ahead. export_cancel {job} on a waiting job removes it from the queue. Without queue the export starts at once, beside any others.
  Render preview: render_preview {range?} pre-renders a stretch (default: the whole timeline) so playback over the heavy parts of a cut reads finished frames instead of compositing them. It runs in the background; project_status shows the progress and the cached ranges. An edit inside a cached range drops that render. render_preview {cancel:true} stops it.
  export_status {out} for a file this session did not write reports whether it is there, its size and its time; the progress
    of a job from an earlier session is not known.
  A finished file whose true peak is above -1 dBTP warns (exactly -1 is within spec, for every format); over 0 dBTP its samples clip, and that warning is the stronger one. The fix is the master limiter:
    project_settings {limiter:{on:true, ceiling_db:-1}} for lossless, ceiling_db:-2 for lossy. The ceiling is -1 dBTP for lossless (wav, flac) and -2 dBTP for lossy (mp4, mkv, webm, mp3, ogg, m4a), since a lossy encoder can add about a dB of overshoot above the mix. The warning fires above -1 dBTP for every format, lossy ones included, since a true peak there clips on some players; the -2 figure is the limiter target that keeps a lossy encode clear of it. The mix is held to the same ceiling by normalize {target:\"mix\", allow_limiter:true, true_peak:-1} (-2 for lossy).";

const SUBTITLES: &str = "\
SUBTITLES
  A subtitle track is a list of cues (text with a start and an end), kept in the project. Nothing shows until
  a track is placed on a caption lane (S1..) or exported. Tracks are numbered from 0; the captions tool's footer lists them.
  Import: edit {ops:[{op:\"subtitles_import\", path:\"talk.srt\"}]} adds the tracks of a subtitle file, or a video's
    own subtitle tracks. They wait off the timeline until edit {ops:[{op:\"captions_place\", track:0, lane?:\"S1\", at?:\"0\"}]}
    puts one on a caption lane (S1 by default, at frame 0 by default).
  Language: subtitles_import {path, lang?} takes an optional language: an ISO 639-2 code (fra, deu, eng; fr or French also work). Without one, it is guessed from the file name (talk.fr.srt, fra.srt, talk_en.srt, eng.srt); a name that says none leaves it unset, written as und. subtitles_lang {track, lang} sets it later. The mp4 and MKV exports carry the language, and an MKV track's title is the language name.
  Reading: in WebVTT and SRT, <br> is a line break; ASS is read as UTF-8. An unreadable or reversed cue, a wrong arrow (`->`) or a cue with no timing line is skipped, and the subtitles_import reply names it with its line number.
  Placed captions, by address: the edit ops sub_move {clip, at, to_lane?} (its group follows), sub_trim {clip, edge:\"in\"|\"out\", to}
    (moves its start or end: edge is \"in\" or \"out\") and sub_delete {clip} (a lone caption lifts and leaves a gap; a grouped one takes its group).
  Cue text, by track and cue number: edit {ops:[{op:\"cue_text\", track:0, index:3, text:\"Hello\\nthere\"}]} rewrites one
    (\\n is a line break; empty text removes it). edit {ops:[{op:\"cue_delete\", track:0, index:3}]} removes one and moves nothing.
    edit {ops:[{op:\"cue_replace\", find:\"Jon\", replace:\"John\"}]} changes words in one track, or in every track when
    track is left out; it is case-insensitive unless case_sensitive is true.
    edit {ops:[{op:\"cue_time\", track:0, index:3, at?:\"12s\", to?:\"14s\"}]} trims or moves one cue (timeline times; give at, to or both); it stays inside its neighbours and at least 0.1 s long.
    edit {ops:[{op:\"cue_merge\", track:0, index:3}]} joins a cue with the next one; edit {ops:[{op:\"cue_split\", track:0, index:3, at:\"12s\"}]} divides one in two at a time.
    edit {ops:[{op:\"cue_add\", track:0, at:\"12s\", to:\"14s\", text:\"Hello\"}]} adds a cue at timeline times (track_clock:true for the track's own); an overlap is refused unless overwrite is true.
  The captions tool lists the placed cues as \"track T cue C\": the track and cue numbers the cue ops take.";

const CAPTIONS: &str = "\
CAPTIONS
  Captions are the words drawn over the picture. Three steps make them.
  1. Put the words on a caption lane (S1..).
     auto_captions {target:\"A1\"} transcribes a sound lane (A1 by default) and writes its cues to S1 at frame 0, as one
       undo step. Cues are cut at pauses, sentence ends, max_chars and max_secs; the .srt is saved beside the project and
       imported as a track. lane names another caption lane. A path that already holds a track is refused.
       Words move between neighbouring cues to hold 0.8 s; cues the limits force shorter are named in the reply.
       A lane with no speech is refused, \"no speech to caption in A2: the transcript has no words\", and nothing is written.
     A lane that already holds captions takes the new run merged in; a mix splits cues at speaker changes.
     or edit {ops:[{op:\"subtitles_import\", path}]}, then edit {ops:[{op:\"captions_place\", track, lane?, at?}]} (see SUBTITLES).
     hint:\"Edith, LUFS\" may help spelling and vocabulary, but is not guaranteed: after transcription, a near-miss spelling of a hinted word is corrected to it, and the reply names each correction (\"hint corrected: 'Jane Dough' → 'Jane Doe'\"); numbers come out as digits (\"40 years\"). cue_replace fixes words after the fact.
  2. Fix them. The captions tool lists the cues as track T cue C [S1@F] (its range is from inclusive, to exclusive, in exact seconds).
     [S1@F] is the placement that shows the cue, the address sub_move, sub_trim and sub_delete take. Cues inside one placement share its
     address, and a cue that a cut split lists one line per part. The edit ops cue_text rewrites one, cue_delete removes one,
     and cue_replace fixes a word in every cue.
     Time edits (insert_time, remove_time, cut_regions and the other cuts) split a cue that a cut runs through, at the word boundary
     nearest the cut; words inside a removed span are dropped. A one-word cue keeps its word on the longer piece only (ties: the earlier one).
     Caption lanes ripple with the cut; locked caption lanes stay (cut_silence, find_words cut and delete all cut).
  3. Show them in the file. export_start with burn_captions draws the text into the picture, so every player and WebM
     shows it. With subtitles it adds soft tracks a player can switch off (WebM has none). Both is fine too.
     frame and contact_sheet take burn_captions as well, so a check frame shows what the export will draw.
  The burn options (track, size, outline, boxed, place, family, animation, highlight, box, uppercase, words_per_line) are in help {topic:\"burn_captions\"}; the export keys in help {topic:\"export\"}.";

const RIPPLE: &str = "\
RIPPLE
  A ripple moves the items after a cut or a length change, so the timeline closes up (or opens) without a gap.
  The ripple ops: delete, ripple_delete, speed with ripple:true, cut_regions, insert_time, remove_time, and the cut
    tools find_words with cut:true and cut_silence. Each takes lanes:[\"V1\",\"A2\"] to name the lanes it may move.
  Default scope (no lanes): the edited lane, plus the lanes of clips linked or grouped to it (a take moves whole),
    closed over what the ripple reaches. On a picture lane, captions and titles over the moved picture move too.
    find_words cut: the target's lane and its linked lanes, or every lane for a mix or src target.
  lanes replaces that default and must name the edited lane. cut_regions, insert_time and remove_time default to
    every unlocked lane, so lanes narrows them.
  Lanes outside the scope never move and are never split. Titles move only when a picture lane is in scope. A locked caption lane in the default scope is skipped with
    a note; any other locked lane the ripple must move refuses the op.
  Replies list the clips a ripple shifted, as old → new addresses. Addresses in one batch refer to the timeline as
    the previous op left it, so use the new address from that reply before naming the clip again.
  ripple_delete also lists the caption cues it removes or trims.
  lift is not a ripple: it removes one clip alone and leaves its gap, so nothing after it moves.
  A dissolve or crossfade next to a gap fades the sound to or from silence.
  A linked trim moves every member's edge to one time. Each reply carries an effects: line for side effects
    (clips split, holes left, takes unlinked).
  A separate narration lane (its own A lane, not linked to the picture) is outside the default scope: a ripple that
    lengthens, shortens or closes the picture leaves its clips at their times. Name it in lanes to move it too (see the
    speed recipe in help {topic:\"recipes\"}).
  split {at, lanes?} is a cut, not a ripple: it cuts the lanes it names at one time and moves nothing.";

const PROJECT: &str = "\
PROJECT
  project_new {path, template?, resolution?, fps?, overwrite?, discard?} starts an empty project at a new path, creating its folder if it is missing; nothing is written
    until project_save, and project_save needs at least one source in the project (media_import first). template:\"<name>\" starts from a template (see templates): the canvas, rate, lanes and caption style come with it, so resolution and fps are refused with it.
  templates {} lists the built-in templates (YouTube 1080p 30, Vertical 1080×1920 30, Square 1080 30) and the saved ones, each marked as keeping clips or not. Saved ones come from File > Save as template in the window. overwrite:true lets the later project_save replace a file already at path, since project_new makes that file this project's (without it a save refuses
    to replace one). discard:true drops the open project's unsaved edits to start this one (its autosave keeps them when
    one can be written); without it a dirty open project refuses.
    A path with an earlier session's unsaved autosave is refused before anything is created, unless overwrite:true: the reply names the autosave and offers project_open {recover:true} to continue from it; overwrite:true sets the autosave aside as a dated copy (the newest three dated copies and any from the last 7 days are kept; older ones are deleted).
  project_open {path, recover?, discard_autosave?, search?, discard?} opens a .edith file, or a media file as a new project.
    An autosave beside the project that holds an earlier session's unsaved edits refuses the open, and nothing is loaded: the refusal names the edits and both choices. recover:true opens the saved file with the autosave's edits on top as one undo step, so undo takes them back
    (with this session's own unsaved edits open it needs discard:true, which drops those edits; the recovered edits are unsaved until project_save). A never-saved project has no saved file under them, so undo has nothing earlier to return to.
    discard_autosave:true opens the saved file and sets the autosave aside (<stem>.autosave-<stamp>.edith; the newest three dated copies and any from the last 7 days are kept, older ones are deleted). recover:\"<file name or time stamp>\" loads one of the dated autosaves the reply
    lists (<stem>.autosave-<stamp>.edith; the stamp is local time, e.g. 20261016-223636), and the newest autosave stays unloaded.
    search:[folders] relinks offline sources by file name; the project is then unsaved until project_save writes the new paths.
  project_save {path?, force?, discard_autosave?} saves, or saves as a new path; it needs at least one source in the project (media_import first). It refuses to overwrite a file that changed on
    disk since the project was opened, unless force is true (force also replaces a file at path that this session did not open). project_save takes no overwrite key. It also refuses while an autosave that appeared after the open holds edits this
    project lacks (\"an autosave from <time> holds unsaved edits ...\"): recover them with project_open {recover:true}, or pass
    discard_autosave:true: this session's edits are saved, and that autosave is set aside as a dated copy (the newest three dated copies and any from the last 7 days are kept).
  project_collect {folder?} copies every file the project plays into one folder (default <project stem>_media beside the project file) and points the project at the copies,
    so the folder and the .edith can be moved or archived together. Generated sources (solids, bars, adjustment layers) are skipped; a source whose file is missing is skipped and named; a source already in the folder stays;
    a name taken by another file becomes name-2.ext; the same bytes already there are reused. One undo step; unsaved until project_save. A failed copy changes nothing.
  project_export_timeline {format, path?} writes the active sequence for another editor: format otio (OpenTimelineIO .otio), fcpxml (Final Cut Pro XML 1.10, for Final Cut Pro, Resolve and Premiere) or edl (CMX3600: V1 and A1/A2). Subtitles are left out and the reply says so. Default path: beside the project file. Only the file is written.
  project_import_timeline {path} reads an .otio file as a new sequence of the project and makes it the active one, relinking each clip by its path; a missing file becomes offline. One undo step.
  project_status reports the open project. project_settings sets resolution, fps, tonemap, proxies,
    auto_proxies, encoder, the master limiter, the notes (free text, several lines, shown by project_status when set) and the still length
    (still_secs, 0.5 to 600 s, 5 by default: stills placed after the change take it). Each change and each refusal is reported.
    The sample rate the mix runs at is not set here: project_status shows it. Unset, it is the highest rate among the sources, or 48 kHz when every source is below 44.1 kHz.
    An fps change keeps clips, captions and titles at the same seconds; their frame numbers, and so
    their addresses (V1@450), change with the rate. Positions are rounded to whole frames at the new rate,
    so a round trip (to another fps and back) can shift a cut by up to a frame; check the cut with timeline.
  Autosave: when the server's input closes (the client exits), a dirty project is written beside its file as
    <stem>.autosave.edith, or to ~/.cache/edith/autosave/ when untitled. It never overwrites the project's own file.
    project_open refuses over a newer autosave, and lists the dated autosaves beside the file (newest first) with their
    local time and what they change. Only the newest three dated copies and any from the last 7 days are kept: setting one aside deletes the older ones. An open removes only an autosave that matches the file.
  Voiceover sources are unsaved until project_save: the WAV is imported as a source, and the project file names it only after a save.
  frame and contact_sheet also save their PNGs to ~/.cache/edith/frames/<project>/ (<project> is the project file's name, or untitled;
    $XDG_CACHE_HOME/edith/frames/ when that is set to an absolute path) and print the path.
  A frame shows the file's own pixels: edith draws no stamp on it, so a source with a timecode burned into its picture
    (the test clips have one) shows it. A contact sheet labels each tile with its time in a band above the tile.
  Undo: one edit call is one step, 100 steps for the session; the history is lost when the server exits.
    media_relink is one undo step: undo puts the old file back and redo points the source at the new one.
    media_bin {action: create|rename|remove|move|favourite} files sources in bins (named folders, one level of nesting) and stars favourites; each call is one undo step. media_list shows each source's bin and star. Saved as format v51 lines (bin, srcbin, srcfav).
  Checkpoints last longer: checkpoint {name} saves a full copy to <project dir>/checkpoints/<stem>/<name>.edith
    (beside the project file, once the project has a path: project_new {path} or project_save; with no path at all
    the copy goes to the temp directory and is lost on reboot). The open project is untouched.
    checkpoints lists them, including ones from an earlier run.
    restore {name} first saves the open project as a checkpoint named before-restore-<name> (a suffix when that name is
    taken), so the state before the restore stays on disk and checkpoints lists it; then it replaces the open project with
    the named one. The undo history is cleared, and the restore itself cannot be undone, but the before-restore checkpoint can be restored.";

const BURN_CAPTIONS: &str = "\
BURN_CAPTIONS
  burn_captions {track?, size?, outline?, boxed?, place?, family?, animation?, highlight?, box?, uppercase?, words_per_line?} draws one subtitle track's text into the picture. Pass the object, true for the defaults, or false for none.
  Accepted by export_start, frame and contact_sheet (not with a source: those draw no timeline captions).
  family: the font family the caption is set in, by the name the GUI's font list shows (family:\"Noto Serif\"). Default: the bundled Archivo; a name the system does not have draws in Archivo.
  track: the track index, as in subtitles. Default: the track placed on the first caption lane with clips,
    else track 0. A bitmap track (PGS) is refused, and so is a track with no text.
  size: text height (the em size) as permille of the frame HEIGHT, not the width. 50 (the default) is 5% of the height; 10 to 200.
    In pixels: size × height / 1000. At 1920x1080 the default is 54 px; at 1080x1920 (9:16) it is 96 px, nearly 9% of the width,
    so a cue wraps at about 23 characters. For 9:16 use size 30 (58 px, about 5% of the width, about 38 characters a line on one line).
  outline: none, thin (default) or thick. boxed: a translucent black box behind the text (default true).
  place: bottom (default), top or centre. A lower third at the bottom collides with bottom captions: burn them at
    place:\"top\", or move the lower third (title_add with place:\"top\", or transform) when the bottom is wanted for captions.
    export_start and frame name a clash: a caption burned in the same place as a title on screen at the same time, as \"at 86.5s the caption overlaps title '...' (both at the bottom): burn_captions {place:\"top\"} or move the title\".
  animation: none (default, the static cue), karaoke (the whole chunk shown, the word being spoken in the highlight colour),
    pop (words appear as they are spoken, each popping from 125% to 100% in 120 ms), bounce (the word being spoken is raised and enlarged, easing down)
    or word (one big word at a time, popping in). The highlighted word is the one whose time has come: in karaoke, pop, bounce and word it is drawn in `highlight`.
  highlight: #rrggbb or #rgb (default #ffd600). box: true makes the highlight a rounded box of that colour behind white text (not the black `boxed`). uppercase: capitals.
  words_per_line: at most N words on screen at once (2 to 4 is the shorts style); a longer cue is re-chunked by its word times, and a pause of 0.7 s or more
    also starts a new chunk. 0 (default) keeps each cue whole, with its own line breaks. words_per_line and uppercase work with animation none too.
  Word times: a cue made by auto_captions keeps the transcript's real word times (saved in the project), so karaoke follows the speech. Editing the cue's text drops them;
    move and trim shift and clip them. A cue without them (SRT, VTT, typed) spreads its words over the span by letter count; a cue with no words draws nothing.
    Clip speed and speed ramps are already in the cue times, so a sped-up clip animates at its own pace.
  The defaults for animation, highlight, box, uppercase and words_per_line come from the project (project_settings {captions:{...}}, saved with it, undoable);
    a key in burn_captions overrides it for that call. Preview and export draw the same pixels: frame {burn_captions} shows what the export will.
  Lines keep the cue's own line breaks when they fit; a line too wide for the frame wraps, its words spread evenly over the lines.
  Text is drawn from bundled Noto faces for Latin, Greek, Cyrillic, Arabic, Hebrew, Devanagari and Thai, shaped, with right-to-left text in order. Colour emoji and CJK come from the system's Noto Color Emoji and Noto Sans CJK when those are installed; without them emoji draw monochrome and CJK draws as empty boxes here: use soft subtitles (export_start subtitles) for CJK then. A character no face has draws as a box, and the reply names it.
  Overlapping cues are all drawn, stacked upward from the bottom in start order.
  Burned captions are pixels, not a track: a re-imported export has no caption text to read back. Check them by eye with frame or contact_sheet.";

const OVERLAYS: &str = "\
OVERLAYS (a logo or still over the picture)
  A still is a picture with no sound. A lane higher up draws over the lanes below it, so a still on V2 sits over V1 wherever it is placed.
  1. Import the still: media_import {paths:[\"<dir>/logo.png\"]}. It becomes a source (src:N).
  2. Add the lane above the picture, then place the still on it: edit {ops:[{op:\"lane_add\", kind:\"video\"}]}, then edit {ops:[{op:\"place\", source:\"src:1\", lane:\"V2\", at:\"1s\"}]}. A still is placed 5 s long; set its length with edit {ops:[{op:\"trim\", clip:\"V2@30\", edge:\"out\", to:\"4s\"}]} (to is the timeline time the edge lands on; V2@30 is the still placed at 1s, which is frame 30 at 30 fps). Without a lane, place links V1 and A1, and a still has no sound to put there.
  3. Put it in a corner: edit {ops:[{op:\"transform\", clip:\"V2@30\", corner:\"bottom-right\", scale:0.3}]} puts a picture 30% of the canvas wide in the bottom-right corner, 32 px in from each edge. corner is centre, top-left, top-right, bottom-left or bottom-right; it keeps the scale you give (or the one already set), and margin:N sets the gap in pixels. The engine measures the picture itself, so a 16:9 still and a square logo both land flush. Giving x or y with corner is refused. Picture in picture is corner:\"bottom-right\" with scale:0.3.
     x and y still work for any spot off a corner: they move the centre off the canvas centre, as fractions of the canvas width (x) and height (y), positive right and down, negative left and up. x:0.6 puts the still past the right edge, and the reply warns when a clip is mostly off the canvas. scale multiplies the size the picture is fitted to (0.05 to 8).
  4. Choose how it fills its box: edit {ops:[{op:\"fit\", clip:\"V2@30\", policy:\"fit\"}]}. The policies are fit, fill, stretch and center.
  5. Make it translucent: edit {ops:[{op:\"transform\", clip:\"V2@30\", opacity:0.5}]} is a 50% logo over the picture below. opacity runs from 0 (clear) to 1 (opaque, the default), and it keeps the placement set in step 3.
  Check it with frame {at:\"2s\"}: the reply lists the lanes at that time and which one is on top.
";

const SEQUENCES: &str = "\
SEQUENCES AND COMPOUND CLIPS
  A project holds several named sequences (timelines). Size, frame rate and the look settings are the project's; lanes, clips, markers and chapters belong to each sequence. One is active, and every tool works on it.
  1. sequence {action:\"list\"} shows them (* is active); new {name?}, rename {sequence, new_name}, duplicate {sequence, name?}, delete {sequence} and switch {sequence} manage them. Switching is not an undo step; the others are.
  2. Group a stretch of clips into one: edit {ops:[{op:\"nest\", clips:[\"V1@0\",\"V1@90\"], name:\"Intro\"}]}. The picked clips (and their linked sound) move into a new sequence and a compound clip stands where they were; the picture and sound are the same. One undo step. break_apart {clips:[\"V1@0\"]} puts the contents back.
  3. Reuse a sequence: place_sequence {sequence:\"Intro\", at:\"30s\"}. A sequence that holds itself, directly or through others, is refused.
  4. Work inside one: sequence {action:\"switch\", sequence:\"Intro\"}, edit, switch back. Or give timeline, clip_info, edit or export_start a `sequence` argument: the read leaves the active sequence alone; edit leaves the named one active.
  5. export_start exports the active sequence, or `sequence`. Nested sound and picture render as the flat timeline would.
  A compound clip trims, splits, moves, fades and takes gain like any clip; it has no speed, ramp or reverse (refused), and its own colour, effects and transform are not applied (grade the clips inside). Captions and titles inside a sequence play only when that sequence is exported or active, not inside a compound. The frame rate cannot change once there are two sequences. Saved as format v45 only when a project has more than one sequence.
";

const MULTICAM: &str = "\
MULTICAM (several cameras of one event, cut by angle)
  1. media_import every camera's file (they should share a sound the cameras heard: a clap, a voice, a song).
  2. multicam_create {sources:[\"src:0\",\"src:1\",\"src:2\"], name?:\"Interview\", sync?:\"audio\"}: the cameras are lined up by the shape of their sound (accurate to 20 ms; sync:\"start\" lines them up at their starts instead) and one multicam clip goes on V1 (picture) + A1 (sound), linked. The reply lists every angle, how it was synced and a match score; an angle that did not match (or has no sound) starts with the others and says so. It plays angle 1.
  3. Cut between the cameras: edit {ops:[{op:\"angle\", clip:\"V1@0\", at:\"12s\", angle:2},{op:\"angle\", clip:\"V1@0\", at:\"20s\", angle:1}]}. Each op cuts the clip at `at` and shows the angle from there; the sound follows the picture. audio:2 keeps the sound on angle 2 (a lav mic, say) whatever the picture shows; audio:\"follow\" lets it follow again. Angles are numbers from 1 or the names the reply shows. Every segment is an ordinary clip: trim, move or delete it with the usual ops.
  4. frame {at} and contact_sheet show the cut; timeline names each segment's angle (multicam \"Interview\" angle 2 \"cam_b\").
  5. multicam_flatten {clip:\"V1@0\"} replaces all the segments with the plain camera clips they play (the picture does not change), for effects or colour on one camera's stretch; the multicam sequence stays until deleted.
  A multicam clip is a compound clip: it has no speed, ramp or reverse. One undo step each. Saved as format v47 (`mcam` and `angle` lines); a project without one keeps its older version.
";

const VERTICAL: &str = "\
VERTICAL (9:16 from a 16:9 cut)
  1. Start at the vertical size: project_new {path:\"<dir>/short.edith\", resolution:\"1080x1920\"}, or on an open project project_settings {resolution:\"1080x1920\"}. The canvas changes; the clips keep their times.
  2. Choose how a 16:9 clip fits the 9:16 canvas, with edit {ops:[{op:\"fit\", clip:\"V1@0\", policy:\"...\"}]} on each picture clip. The policies:
     fit (the default): the whole picture, aspect kept, with bars left and right. Nothing is cropped, but the sides are empty.
     fill: the canvas is covered, aspect kept, and the sides of the picture are cropped away. The usual choice for a talking head or footage with the subject in the middle.
     stretch: the whole picture is squeezed onto the canvas, aspect not kept. Avoid it for people.
     center: 1:1 pixels, centred, padded or cropped per axis. Use it for a graphic that must keep its pixel size.
     Choose fill when the subject is in the middle of the frame and cropping is fine; choose fit when the whole frame matters (a slide, a screen, a wide shot). Then check with frame.
     Text-heavy 16:9 art (slides, screens with words): fit letterboxes it with bars, and fill crops the words off the sides. For the best result, make 9:16 versions of the slides instead.
  3. Follow the subject automatically: edit {ops:[{op:\"reframe\", clip:\"V1@0\"}]} finds the subject in the footage (movement and contrast, no models), keeps the crop still while the subject stays put, and writes transform keys that fill the 9:16 canvas without ever showing an edge. It sets the clip's fit to fit, replaces the clip's transform keys, and is one undo step; smoothness 0..1 (default 0.5) sets how slowly the crop follows, and aspect:\"9:16\" checks the project is that shape. Refine the keys with transform_keys, or take them off with transform_keys {clip, clear:true}. A talking head that stays put needs only fill; reframe pays off when the subject moves across a wide shot.
  3b. Or reframe by hand: crop first, in fractions of the source, then move and zoom. A 16:9 picture keeps about a third of its width for 9:16, so crop [0.34,0.34,0,0] keeps the middle: edit {ops:[{op:\"transform\", clip:\"V1@0\", crop:[0.34,0.34,0,0], x:-0.1}]}.
  4. Titles and captions are laid out on the canvas, so they adapt to the new size; look at a frame to check where they sit. CJK draws as empty boxes in titles and burned captions unless the system has Noto Sans CJK: use soft subtitles (export_start subtitles) for CJK, as help {topic:\"unsupported\"} says.
  5. Or skip the project change: export_start {out:\"<dir>/short.mp4\", size:\"1080x1920\", fit:\"fill\"} draws this export at 9:16 and fills every picture clip to the canvas, with the project kept at 16:9. fit:\"fit\" keeps the whole picture with bars instead. Check the file with media_probe.
";

const STRETCH: &str = "\
STRETCH (a short shot made to fill a longer slot)
  place with stretch:true plays the whole source over `length`, sped up or slowed to fit: edit {ops:[{op:\"place\", source:\"src:0\", lane:\"V1\", at:\"6s\", length:\"6s\", stretch:true}]}. A 5 s shot in a 6 s slot runs at 0.83 speed. Prerequisite: src:0 is that 5 s shot, and stretch takes a slot only within 0.25..4 of the shot's length, so a 5 s shot fits slots from 1.25 s to 20 s. Without stretch, a length shorter than the source takes the source from its in point over that span, which cuts rather than slows; a length longer than the source is refused (\"runs past the source end\"), not cut short, so add stretch:true to fill the slot.
  A placed clip can be slowed afterwards with edit {ops:[{op:\"speed\", clip, x}]}: x 0.5 runs it twice as long, and the range is 0.25 to 4. x is the absolute rate, not a multiplier: sending 0.5 on a clip already at 0.5 changes nothing. Its end moves; ripple:true moves the clips after it too: edit {ops:[{op:\"speed\", clip:\"V1@12\", x:0.5, ripple:true}]}.
";

const UNSUPPORTED: &str = "\
NOT SUPPORTED (these are not in the tools; do not search for them)
  Transitions beyond the seven kinds (dissolve, dip-black, dip-white, wipe, slide, push, zoom): no custom shapes, no easing or keyframed progress, no transition between layered pictures (only the clips of one lane blend). A kind changes the picture only; the sound always crossfades.
  Reversed clips (reverse) have limits: they decode at 8 bits, so a 10-bit source loses its low 2 bits on a 10-bit export (banding is possible on HDR); hardware decode is off for them; the B side of a dissolve plays forward; waveforms and thumbnails draw forward.
  CJK text in the picture, unless the system has Noto Sans CJK: it draws as empty boxes in a title or in burned captions (burn_captions). Colour emoji need the system's Noto Color Emoji; without it emoji draw monochrome. A character no face has draws as a box either way; title_add names it in a warning, and frame names it in a caption. Soft subtitle tracks (export_start subtitles, in mp4 or mkv) keep that text, since the player draws it: use them for CJK when the font is missing.
  10-bit: AV1 and HEVC only (export_start bit_depth:10, or a 10-bit source); H.264 and the other formats are 8-bit and refuse bit_depth:10. Grades, titles, captions, transitions and layers are composed at 8-bit precision and shifted up, so their extra range is not used.
  Sources the decoder refuses: HEVC in 4:4:4, 4:0:0 (monochrome) or 12-bit, and AV1 in 4:4:0. AV1 in 4:2:2, 4:4:4 or 12-bit, and HEVC 4:2:2, decode.
  Source picture files that the decoder refuses (for example 12-bit HEVC, or a display matrix that is not a quarter turn):
    media_probe names the reason, and media_import refuses them.
  Sound codecs nothing here decodes (for example WavPack): media_probe names the codec and warns that its sound will be silent.";

/// The notes under the chapter ops in `help {topic:"ops"}`, after the op signatures.
const MARKERS: &str = "\
MARKERS
  A chapter is C@<frame> (the frame it starts on) or C#n (its place in the list, 1 first). chapter_add, chapter_set and chapter_remove take them.
  Cuts and inserts move chapters with the picture. timeline prints a chapters: line and a chapters_text YouTube description, with warnings.
  Comment markers are separate from chapters: marker_add {at, name, note?, color?, length?}, marker_set {marker, ...}, marker_remove {marker}. A marker is M@<frame> (the first on that frame) or M#n. color is red, orange, yellow, green, blue or purple; a length makes a range. They move with the picture as chapters do; timeline prints a markers: line and a markers list. export_start chapters:\"markers\" writes them as the file's chapters. beats {clip, every?} marks a sound clip's beats as yellow \"Beat\" markers (every:1 default, 2 or 4 for bars), replacing that clip's earlier beat markers.";

/// The `looks` topic: what a look is, the built-ins, and where saved ones live.
const LOOKS: &str = "looks: a look is a whole colour grade under one name. The look op applies it; the colour card's Looks row shows the same list.
  Built-in (colour presets, no LUT file needed):
    Warm              warmer temperature, red up and blue down in the wheels
    Cool              cooler temperature, blue up
    Teal & orange     teal shadows, orange highlights, a little more colour
    Bleach bypass     low colour, more contrast, an S curve on the master
    Black & white     no colour at all
    Faded film        lifted blacks, softer contrast, muted colour
    High contrast     more contrast and a little more colour
    Vintage           muted colour, a warm cast, lifted blacks
  Saved: 'Save current as look' on the colour card writes $XDG_DATA_HOME/edith/looks/<name>.look (or ~/.local/share/edith/looks). The file is plain text: the sliders, the wheels, the curves, and a LUT's path and mix if the grade has one.
  edit {ops:[{op:\"look\", clip:\"V1@0\", name:\"Warm\"}]} replaces the clip's grade with the look in one undo step. A name nobody has is refused with the list of looks there are.
  auto_color is not a look: it reads the clip and sets a correction from its own frame.";

const VISUALIZER: &str = "\
VISUALIZER
  clip_info says \"visualizer off\" for an ordinary clip. \"visualizer on (style S, fast)\" means the clip is a
    visualizer: its picture is drawn from the sound it plays, as a waveform, not from its source picture.
  Set it on a clip whose sound is drawn. On an AUDIO clip (a voiceover or music with no picture) on:true makes a linked
    waveform picture: the op places it on a free video lane at that clip's start, over the same span, grouped with the sound
    clip so moves, trims and deletes carry it. The waveform goes on a new top picture lane (V2 or V3, the next free one above
    the picture; the reply names it, e.g. V2@0), and it covers the picture under it where they overlap. style defaults to fill:
    edit {ops:[{op:\"visualizer\", clip:\"A2@0\", on:true, style:\"ring\", fast:true}]}.
    Position and size it with transform on that picture: edit {ops:[{op:\"transform\", clip:\"V2@0\", x:0, y:0.27, scale:0.4}]} puts it as a bottom band, 40% of the height, inside the canvas.
    (x and y move its centre by a share of the canvas, -1 to 1, y positive down; scale multiplies its size). opacity under 1 lets the picture show through.
    Keep it on the canvas: its centre sits at (0.5 + x, 0.5 + y) of the canvas, so a box of height scale (a 16:9 waveform on a 16:9 canvas) stays inside when y is at most 0.5 − scale / 2. With scale 0.4, y:0.27 puts its bottom edge at 0.97 of the height; y:0.7 would put the centre at 1.2 of the height, entirely off the canvas.
    Turn it off with on:false on the waveform picture (V2@0), or on the sound clip it draws (A2@0), which also turns the picture off.
    on: true or false (false turns it off). style: fill, ribbon, ring or wave, the look of the trace.
    fast: true paints the trace from a 0.2 s window instead of 2 s, so it reads as a line (an Audacity-style view).
    Omitted keys keep their setting. One undo step.";

/// Full text for ops whose manual line is cut short or leaves out the ranges. These
/// replace the manual's text in `help {topic: op}`; the signature still comes from the manual.
const OP_TEXTS: &[(&str, &str)] = &[
    (
        "replace",
        "replace {clip, source, in?} — the replace edit: the clip is overwritten by `source` from `in` (a source time, default the source's start; a still takes none), keeping its place and its length, and nothing else moves. The clip's linked other half (the sound of a picture clip, the picture of a sound clip) is replaced too when it covers the same frames and the new source has that half. Refused when the source has fewer frames after `in` than the clip is long, when the source lacks the clip's kind (a picture-less file onto V1), or when a lane is locked. The new clip starts plain: the old one's effects, colour and speed are not carried (copy_look first reads them, then copy_look back). e.g. edit {ops:[{op:\"replace\", clip:\"V1@0\", source:\"src:1\", in:\"2s\"}]} Prerequisite: V1@0 exists and src:1 is imported and at least as long as the clip after 2 s. One undo step.",
    ),
    (
        "nudge",
        "nudge {clips:[addrs], by} [group] — moves the clips and their link groups `by` along their lanes (-1f, +0.5s, -00:00:01:00), the nudge key's move. All or nothing: a neighbour in the way refuses the whole move and names nothing moved; the start clamps at 0. Unlike slide it needs no neighbours, unlike move it takes a distance, not a place. e.g. edit {ops:[{op:\"nudge\", clips:[\"V1@120\"], by:\"-5f\"}]} Prerequisite: V1@120 exists with at least 5 free frames before it. One undo step.",
    ),
    (
        "swap",
        "swap {a, b, ripple?=true} [clip] — two clips on one lane trade places: b takes a's start and a lands where b was. With ripple:true (the default) what lies between them ripples by the difference in their lengths, so the later clips move; the reply lists every clip that moved. With ripple:false nothing else moves: the two clips trade slots, a shorter clip leaves a gap in its slot, and a longer one is refused where it would overlap what follows. A clip in a take (linked to a clip on another lane, such as a picture with its sound) is refused, since swap does not move a take apart: the refusal names the clip, and the take must be ungrouped first. One undo step, and a refusal changes nothing.",
    ),
    (
        "move",
        "move {clip, at, to_lane?, titles?, insert?, split_captions?} [group] — slide the clip, with its take, to `at` on its own kind of lane (V to V, A to A; to_lane names another lane of that kind). A drop on empty frames lands exactly there, jumping over any clips between. Into occupied space it refuses and names the clip in the way; a head that would land inside a clip goes as far as the room allows, and the reply says what `at` was asked for. insert:true is a ripple move: the clip leaves its slot, the slot closes as ripple_delete closes one, and the clip lands at `at` with the later clips on those lanes pushed to make room. If the ripple would close over a clip outside the moved clip's take, insert:true refuses and names it: move without insert, or lift that clip and close_gaps. Titles: one wholly over the moved picture moves with it; titles:true also carries one it only partly covers; titles:false carries none, and the reply names each that stays. A caption spanning the clip's old edges is refused by name with insert:true unless split_captions:true, which splits it at the edge and reports both halves.",
    ),
    (
        "speed",
        "speed {clip, x, ripple?, lanes?} [group] — rate 0.25..4, absolute: x is the clip's new rate, not a multiplier, so re-sending 0.5 on a clip already at 0.5 changes nothing. The clip's length is divided by x: x 0.5 runs it twice as long and its end moves. A neighbour in its way refuses unless ripple:true, which moves the later items in the ripple scope by the change in length (slower pushes them later, faster closes the gap). Clip speed on audio keeps pitch (time-stretch). A voiceover's pace is set when it is rendered: voiceover's speed argument (0.5..2), not speed. The scope is the edited lane and the lanes of its linked clips; lanes:[...] changes it. A clip on another lane that spans the change is never cut: outside the scope it is left in place and the reply says so (\"A2@0 spans the change and was left in place\"); inside an explicit lanes it refuses by name. This holds for shortening and lengthening alike. help {topic:\"ripple\"} says which lanes move, and help {topic:\"recipes\"} has the narration case.",
    ),
    (
        "freeze",
        "freeze {at, duration, lane?, insert?=true} — holds the picture at `at` for `duration`. The composed frame is written to <project folder>/freeze/ when the op runs, so the project needs a path (project_new {path} gives one, saved or not) and that folder must exist; the project file itself need not be saved yet, and project_save writes the timeline. A project with no path is refused: project_save {path} first. insert=true ripples first, as insert_time: everything after moves `duration` later, and audio gets silence. insert:false overlays the still without pushing: with a lane it overwrites `lane` for `duration` (what is under it there is covered or cut, nothing after it moves); insert:false with no lane puts the still on a new top video lane over the picture, and nothing underneath is touched. The lane defaults to V1 only when insert is true. A dry run writes nothing. An undone freeze leaves its still in the media library (media_remove drops it).",
    ),
    (
        "ripple_delete",
        "ripple_delete {clip, lanes?} — cut the clip's span out of every lane in the ripple scope, so the later clips there move up to close the gap. Which lanes are in scope, and how lanes:[...] changes it: help {topic:\"ripple\"}. It is the same ripple as delete, and also reports the sound drift and what the ripple leaves behind.",
    ),
    (
        "delete",
        "delete {clip} [group] — remove the clip and RIPPLE: the clips after it move up to close the gap, the same cut as ripple_delete over the same ripple scope (its lane and the lanes of its linked or grouped clips; lanes:[...] changes it, help {topic:\"ripple\"}). The reply lists each clip that moved, as old → new. A take cut to one span on every member moves whole; a take whose members differ in length closes only each member's own lane. Titles and caption cues in the cut go with it, and the reply names them. If the ripple would cut a clip outside the deleted clip's take (a detached picture under a detached sound, say), delete refuses and names it: lift the clip, or lift that one too and close_gaps. One undo step.",
    ),
    (
        "lift",
        "lift {clip} [clip] — remove the clip alone and LEAVE A GAP where it was: nothing moves, on its lane or any other. Use delete or ripple_delete to close the gap. One undo step.",
    ),
    (
        "gain",
        "gain {clip, db (-60..24) | add_db} — the clip's own volume in dB, on top of its lane's. db is absolute, add_db relative to where the clip is now; the result is clamped to -60..24. Audio clips only: a video clip has no volume of its own here, so it is refused by name; set it on the audio clip split from the same take.",
    ),
    (
        "duck",
        "duck {lane, under, depth_db?=-12, threshold_db?=-40, attack_ms?=80, release_ms?=400, off?} — the audio lane `lane` drops by depth_db while the audio lane `under` (the key) is above threshold_db. off:true removes the duck and needs no `under`. A locked lane refuses it.",
    ),
    (
        "deinterlace",
        "deinterlace {clip, mode: frame|field|off} — how a video clip's interlaced source is made progressive; frame is the default. It only matters for an interlaced source (media_probe reports interlaced tff or bff); on a progressive one it does nothing. field (double rate) and off are stored and saved but the render acts on frame for now.",
    ),
    (
        "generator_add",
        "generator_add {kind: solid|gradient|countdown|bars|tone|rectangle|ellipse|line|arrow, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?, lane?=V1, at?, duration?=5s} — places a generated clip: a picture made from numbers, not a file. tone (a sine on an audio lane) takes hz?=1000 (20–20000), level?=-20 (RMS dBFS, −60 to −3), lane?=A1, at?, duration? in whole seconds (10 by default); it is written once as a 48 kHz mono WAV under the data folder solid takes color (\"#ff3366\" or \"ff3366\"); gradient takes color, color2 and angle (0..359 degrees, 0 left to right, 90 top to bottom); countdown takes from (whole seconds, 1..3600; its duration defaults to the count) and draws a big number with a sweep; bars is colour bars. rectangle, ellipse, line and arrow are shapes on a transparent canvas that composite over the lanes below (put them on V2 or higher): fill and stroke are \"#rrggbb\" or \"none\", width is the stroke in pixels (0..400; a line and an arrow are all stroke, so they need width above 0), radius rounds a rectangle (0..2000); a line runs left to right across the canvas and an arrow points right, turn them with the transform op. A shape lands at 40% scale; its size and place are the clip's transform. A param the kind does not take is refused by name, as is a colour that is not six hex digits. `lane` must be a video lane the timeline has (lane_add first); `at` defaults to the end of that lane; the clip overwrites what it lands on, as a still does. It is an ordinary clip afterwards: trim, move, split, color, transform and effects all work on it. The reply gives its address (V1@0). A project that uses one saves as format v48.",
    ),
    (
        "generator_set",
        "generator_set {clip, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?} — changes the params of a generator clip (V2@450), merged onto its current ones; the clip keeps its place and length. A clip that plays a file is refused by name, an adjustment layer has no params, and a param the kind does not take is refused. One undo step.",
    ),
    (
        "adjustment_add",
        "adjustment_add {lane?=V2, at?, duration?=5s} — places an adjustment layer: a clip with no picture whose color grade and effects (color, wheels, curves, lut, effect add) reach everything composed beneath it on the lanes under. Put it on a lane above what it should tint. With no lane it goes on V2, added when the timeline has only V1; a named lane must exist. at defaults to the end of the lane. Saved as format v48.",
    ),
    (
        "label",
        "label {clip | clips, value} — sets the colour label of one clip (clip:\"V1@0\") or several (clips:[\"V1@0\",\"A1@0\"]). value is a label name (Violet, Iris, Caribbean, …; any case), a number 1..8, or none to clear it. A label is for finding clips in the timeline and changes nothing in the render. Undoes in one step; the reply lists the addresses it touched. Saved as format v48.",
    ),
    (
        "enable",
        "enable {clip | clips, on} — on:false switches the clip off: it stays on the timeline, but playback, frames and export skip its picture and sound (an off adjustment layer stops adjusting); on:true brings it back. The timeline shows `disabled`. A single undo brings it back; the reply names the clips it changed. Saved as format v48.",
    ),
    (
        "lane_name",
        "lane_name {lane, name} — the lane's display label; \"\" clears it. See help {topic:\"lanes\"}.",
    ),
    ("stabilize", crate::tools::stabilise::STABILIZE_OP_DOC),
    ("reframe", crate::tools::reframe::REFRAME_OP_DOC),
    ("angle", crate::tools::multicam::ANGLE_OP_DOC),
    ("nest", crate::tools::sequence::NEST_OP_DOC),
    ("break_apart", crate::tools::sequence::BREAK_APART_OP_DOC),
    ("place_sequence", crate::tools::sequence::PLACE_SEQUENCE_OP_DOC),
    (
        "dynamics",
        "dynamics {lane, gate?, nr?, compressor?, deesser?, reset?} — the audio lane's voice processors, run in this order on the lane's mix: noise gate, spectral noise reduction, compressor, de-esser (after each clip's own EQ, before the limiter). Each of gate, nr, compressor, deesser is true (on, defaults), false (off) or an object of settings: gate {threshold_db, attack_ms, release_ms, hold_ms, range_db}; nr {strength 0..1, floor_db, from:{from,to} (the noise to learn, else the quietest half second is found), learn:false}; compressor {threshold_db, ratio, attack_ms, release_ms, knee_db, makeup_db | auto_makeup}; deesser {threshold_db, amount 0..1, lo_hz, hi_hz}. reset:true turns all four off. nr needs a noise profile: it is learned when nr is turned on. The reply and timeline show which are on. A locked or non-audio lane refuses it. One undo step. clean_voice sets the whole chain at once.",
    ),
    (
        "lane_gain",
        "lane_gain {lane, db (-60..12)} — the lane's own level, -60 to +12 dB. Each clip's own gain adds on top of it.",
    ),
    (
        "pan",
        "pan {clip, pan (-1..1)} — the audio clip's place between the speakers: -1 left, 0 the middle (the default), 1 right. A balance: the far side is turned down along a quarter sine, the near side is left alone. The lane's lane_pan stacks on it. Audio clips only.",
    ),
    (
        "pitch",
        "pitch {clip, semitones (-12..12)} — the audio clip's pitch in whole semitones: 12 an octave up, -12 an octave down, 0 the original (the default). The speed does not change, so the clip is the same length. Playback and export hear it. Audio clips only.",
    ),
    (
        "lane_pan",
        "lane_pan {lane, pan (-1..1)} — an audio lane's place between the speakers, on top of each clip's own pan.",
    ),
    (
        "gain_key_remove",
        "gain_key_remove {clip, at} — takes out the volume point within a frame of `at`. A miss refuses by name and lists the clip's points, so the time can be read off them.",
    ),
    (
        "assemble",
        "assemble {items:[{source,in?,out?}...], lane?, at?, gap?, audio?, audio_lane?, overwrite?} — keep, from several sources at once: each item is laid end to end after the last. Its start is the end of V1 (or of lane, when given) unless at is given, and 0 when that lane is empty or absent. With no lane the picture goes to V1 and its sound to A1, as place does. Stills are refused here (no in and out to cut): add one with place {source, at:\"end\"} or append {source}.",
    ),
    (
        "captions_place",
        "captions_place {track, lane?, at?} — puts a subtitle track (from subtitles_import) on a caption lane: lane (S1 by default) and at (frame 0 by default) are optional. Until a track is placed it shows nothing on the timeline or in an export.",
    ),
    (
        "subtitles_lang",
        crate::tools::edit_subs::SUBTITLES_LANG_DOC,
    ),
    (
        "split",
        "split {at, lanes?} — cut every lane at `at`, or only `lanes`. The lanes not named keep their clip across `at`. A linked partner of a clip cut is cut too, and the reply names it. A cut is not a ripple: nothing moves.",
        ),
    (
        "eq",
        "eq {clip, bands:[{f, db, q?, kind?}], reset?} — the clip's equalizer, up to 10 bands. f 20-20000 Hz; \
         db -12 to +12; q 0.3-12 (default 0.707); kind: peak (default), low_shelf, high_shelf, low_cut \
         (high-pass: rumble, mic handling), high_cut (low-pass: hiss, air). bands replaces the whole \
         equalizer; reset:true takes it off.",
    ),
    (
        "limiter",
        "limiter {on?, ceiling_db?} — the master limiter on the output: on true or false; ceiling_db -24 to 0 dBTP, \
         the highest TRUE PEAK (dBTP, between samples included) the output may reach. Give either or both; it merges onto the current setting. \
         project_settings takes a limiter object too.",
    ),
    (
        "transform",
        "transform {clip, x?, y?, scale?, rotate?, crop?:[l,r,t,b], opacity?, reset?} — the clip's place on the canvas. \
         Units: x moves its centre right by x × canvas width (centre at (0.5 + x) × width), y moves it down \
         by y × canvas height (positive down); both −1 to 1. scale multiplies the fit size, so with a fit that \
         fills the canvas width scale 0.3 is 30% of the canvas width (0.05–8). rotate: degrees clockwise, drawn \
         at the nearest 90. crop: [left, right, top, bottom], each a fraction of the source, 0 to 0.5. A clip \
         moved off the canvas is clipped; on a layer above another, the clip beneath shows wherever this one \
         does not cover. opacity: 0 (clear) to 1 (opaque, the default); below 1 the clip blends over the layers \
         beneath it. reset:true clears all of it. A move or zoom over time is transform_keys (keyed placement; a keyed clip refuses transform).",
    ),
    (
        "transform_keys",
        "transform_keys {clip, keys:[{at, x?, y?, scale?, rotate?, opacity?, ease?}], clear?} — the clip's placement over its own time: \
         a slow zoom (Ken Burns) on a still, or a move. Video clips only. Each key is the placement at one moment, with the \
         same units as transform. at is clip-relative: \"0s\", \"2.5s\" or a frame count from the clip's start, \"50%\" of the \
         clip, or \"end\". Between two keys every field moves in a straight line unless the key has an ease, which shapes \
         the stretch that leaves it (the last key has no stretch): linear (the default), ease_in (slow start), ease_out \
         (slow landing), ease_in_out (slow both ends; \"ease\" is the older name for it) or hold (keep this key's value until the next key, then jump). \
         The same ease works on opacity_keys, color_keys and the gain points. A key leaves a field it does not name held from the \
         previous key, and the first key from the clip's own transform. The reply lists the keys in timeline time. \
         clear:true takes the animation off, back to the clip's single transform. A keyed clip refuses transform: change \
         its keys, or clear them, first. Up to 256 keys. Keys sit on the footage: trimming either edge drops the keys \
         outside the clip and adds a key at the new edge holding the value there, so the picture on screen is unchanged; \
         split and move carry the keys with them. \
         Slow zoom, 20% over six seconds on a still (import it first, as in OVERLAYS): edit {ops:[{op:\"place\", source:\"src:1\", \
         lane:\"V1\", at:\"0s\", length:\"6s\"}, {op:\"transform_keys\", clip:\"V1@0\", keys:[{at:\"0s\", scale:1.0}, \
         {at:\"end\", scale:1.2, ease:\"ease_in_out\"}]}]}. The zoom holds at its first and last key; add {at:\"50%\", scale:1.1} \
         for a middle key. A pan is the same with x and y: {at:\"0s\", x:-0.05}, {at:\"end\", x:0.05}.",
    ),
    (
        "color",
        "color {clip, brightness?, contrast?, saturation?, tint?, reset?} — the clip's grade: brightness -1..1 (0 is none), contrast 0..2 (1 is none), saturation 0..2 (1 is none), tint -1..1 (0 is none). A grade that changes over the clip is color_keys; a clip with colour keys refuses color until they are cleared.",
    ),
    (
        "look",
        "look {clip, name} — the clip's whole grade becomes a look: a built-in (Warm, Cool, Teal & orange, Bleach bypass, Black & white, Faded film, High contrast, Vintage) or one saved from the colour card as \"Save current as look\". It replaces the colour sliders, wheels, curves and LUT in one undo step, and a keyed clip is refused until its keys are cleared. A name nobody has is refused with the list of looks there are. e.g. edit {ops:[{op:\"look\", clip:\"V1@0\", name:\"Teal & orange\"}]}. help {topic:\"looks\"} explains them.",
    ),
    (
        "auto_color",
        "auto_color {clip} — reads a correction off the clip's middle frame as shot and writes it onto the grade: grey-world white balance into the wheels' red, green and blue gains, then brightness and contrast so the 0.5 % and 99.5 % luma percentiles land near 0.02 and 0.98. Saturation, tint, curves and the LUT are kept. The reply gives the values it set. Absolute, so a second run lands where the first did; one undo step. A keyed clip is refused. e.g. edit {ops:[{op:\"auto_color\", clip:\"V1@0\"}]}.",
    ),
    (
        "wheels",
        "wheels {clip, lift?, gamma?, gain?, reset?} — the colour wheels of the clip's grade. Each wheel is [master, red, green, blue], every value -1..1 with 0 neutral, \
         or one number for the master alone; a channel's amount is its own plus the master's. lift moves the blacks and keeps white (1 puts black at half grey), \
         gamma bends the mid-tones and keeps both ends (positive is brighter), gain scales from black (1 doubles, -1 is black). A wheel named replaces that wheel; \
         the others stay. reset:true sets all three neutral. Order of the grade: wheels, curves, lut, then the color sliders. Colour keys animate the sliders only; \
         the wheels hold steady under them. Warm the shadows, cool the highlights: edit {ops:[{op:\"wheels\", clip:\"V1@0\", lift:[0, 0.06, 0, -0.04], gain:[0, -0.03, 0, 0.04]}]}.",
    ),
    (
        "curves",
        "curves {clip, master?, red?, green?, blue?, reset?} — RGB curves on the clip's grade, after the wheels. Each is a list of up to 16 [input, output] points, \
         both 0..1, inputs strictly rising, joined by a monotone cubic spline (no overshoot). An end the points leave open is pinned: (0,0) and (1,1). The master curve \
         runs on all three channels first, then each channel's own. [] straightens that curve; reset:true straightens all four. A gentle S-curve: \
         edit {ops:[{op:\"curves\", clip:\"V1@0\", master:[[0.25, 0.2], [0.75, 0.8]]}]}.",
    ),
    (
        "lut",
        "lut {clip, path?, mix?, clear?} or lut {relink, path} — a .cube LUT on the clip's grade, after the wheels and curves and before the color sliders. \
         path: a .cube file (Adobe/Resolve format: 1D up to 65536 entries or 3D up to 65 per side, DOMAIN_MIN/MAX or LUT_3D_INPUT_RANGE honoured, 3D sampled \
         tetrahedrally as ffmpeg's lut3d interp=tetrahedral is); the file is read now, and a missing or malformed one is refused with the line that is wrong. \
         mix: how much of the LUT shows, 0..1 (default 1 when a path is set). clear:true takes it off. The path is saved in the project (relative when inside \
         the project folder); if the file later goes missing the project still opens, the clip plays without its LUT, and project_status and clip_info name it: \
         relink with lut {relink:\"<old path>\", path:\"<new path>\"} (every clip that used it, one undo step). The LUT reads Rec. 709 R'G'B' 0..1, as LUTs made for \
         video expect. Recipe, a warm look at 80%: edit {ops:[{op:\"lut\", clip:\"V1@0\", path:\"~/luts/warm.cube\", mix:0.8}]}, then scopes {at:\"2s\"} to see the \
         shift (red above blue in the parade, the cast toward red-yellow), then frame {at:\"2s\"} to look.",
    ),
    (
        "mask_keys",
        "mask_keys {clip, effect, keys:[{at, x?, y?, w?, h?, feather?, ease?}], clear?} — a rect or ellipse mask's centre (x, y), size (w, h) \
         and soft edge (feather), all 0..1 of the frame, over the clip's own time. effect is the mask's stack position. A key that names \
         no field holds the previous key's, and the first the mask's own. Pixelate under a mask takes the mask's keys too, so a face \
         stays censored as it moves. clear:true takes the keys off. Up to 256 keys, sorted and clamped like opacity_keys. \
         edit {ops:[{op:\"mask_keys\", clip:\"V1@0\", effect:1, keys:[{at:\"0s\", x:0.3, y:0.5}, {at:\"2s\", x:0.7}]}]}.",
    ),
    (
        "opacity_keys",
        "opacity_keys {clip, keys:[{at, opacity, ease?}], clear?} — the clip's opacity over its own time, 0 (clear) to 1 (opaque): \
         a fade-in of an overlay, a pulse. Video clips only. at is clip-relative as in transform_keys (\"0s\", \"50%\", \"end\"). A key that \
         names no opacity holds the previous key's, and the first the clip's own. ease (linear, ease_in, ease_out, ease_in_out, hold) \
         shapes the stretch that leaves the key. While a clip has opacity keys they give its opacity (transform's opacity is refused); \
         the clip's fades multiply on top, so a fade-in over a half-opaque clip reaches half. clear:true takes them off. Up to 256 keys; \
         trims, splits and moves carry them as they do transform_keys. A layer above another shows the one beneath through it as it \
         fades; a lone clip fades toward black. Fade an overlay in over two seconds, easing: \
         edit {ops:[{op:\"opacity_keys\", clip:\"V2@0\", keys:[{at:\"0s\", opacity:0, ease:\"ease_in_out\"}, {at:\"2s\", opacity:1}]}]}.",
    ),
    (
        "color_keys",
        "color_keys {clip, keys:[{at, brightness?, contrast?, saturation?, tint?, ease?}], clear?} — the clip's grade over its own time, \
         with the value ranges of color (brightness -1..1, contrast 0..2, saturation 0..2, tint -1..1). Video clips only. at is \
         clip-relative as in transform_keys. A key leaves a field it does not name at the previous key's value, and the first key \
         at the clip's own grade. Each frame is graded exactly as a constant color of the interpolated numbers would be. ease \
         shapes the stretch that leaves a key. While a clip has colour keys they give its grade (color is refused); clear:true takes \
         them off. Up to 256 keys. A day-to-dusk grade: edit {ops:[{op:\"color_keys\", clip:\"V1@0\", keys:[{at:\"0s\", brightness:0, \
         saturation:1}, {at:\"end\", brightness:-0.3, saturation:0.6, tint:0.2, ease:\"ease_in_out\"}]}]}.",
    ),
    (
        "cut_regions",
        "cut_regions {regions:[[from,to]...], lanes?} — ripple-cut spans (silence scans); titles move with the cut too. A fade at a cut point is dropped, and the reply reports each one. A music bed cut mid-bed leaves a seam: add a crossfade {clip, frames} on the clip before the join.",
    ),
    (
        "transition",
        "transition {clip, kind?=dissolve, length?, direction?, linked?} — how the clip's tail meets the next clip on its lane (the two must abut). kind: dissolve (the default, what `dissolve` sets), dip-black, dip-white (through the colour at the midpoint), wipe (soft edge), slide (the next clip moves in over this one), push (the next clip pushes this one out), zoom (cross-zoom: this clip scales up and fades while the next scales from 1.2 to 1). direction left|right|up|down (default left) is the way things move: wipe-left reveals the next clip from the right edge, so at the midpoint the left half is still this clip. length (or frames) is the overlap, clamped to the shorter clip; omitted, it keeps the clip's current one. timeline and clip_info show the kind. Sound is the usual crossfade for every kind. One undo step. A 1 s push to the left: edit {ops:[{op:\"transition\", clip:\"V1@0\", kind:\"push\", direction:\"left\", length:\"1s\"}]}.",
    ),
    (
        "fit",
        "fit {clip, policy: fit|fill|stretch|center} — how the clip's picture meets a canvas of another shape. \
         fit (default): whole picture, aspect kept, centred; the bars are empty, so on a layer above another \
         the clip beneath shows through them. fill: whole canvas, aspect kept, the picture is cropped. stretch: \
         whole picture on the whole canvas, aspect not kept. center: no resampling, 1:1, padded or cropped.",
    ),
];

/// The tools the server answers: the registered table plus `help` itself.
pub fn catalogue() -> Vec<Tool> {
    let mut all = server::tools();
    all.push(tool());
    all
}

/// The whole cheat sheet, as the `edith://help` resource and the `help` tool give it.
pub fn sheet(tools: &[Tool]) -> String {
    let header = header();
    [header.as_str(), ADDRESSES, TIMES, VALUES, GROUPS_AND_LINKS, UNDO, &tool_list(tools), &ops_names(tools), RECIPES].join("\n\n")
}

/// The first 25 lines of the sheet, for the initialize `instructions`.
pub fn short(tools: &[Tool]) -> String {
    let head: Vec<String> = sheet(tools).lines().take(25).map(str::to_owned).collect();
    format!("{}\nThe full sheet is the edith://help resource or the help tool.", head.join("\n").trim_end())
}

/// One topic of the help, or the whole sheet when `name` is None. A name is a fixed
/// topic, an edit op name, or a tool name, in that order.
pub fn section(tools: &[Tool], name: Option<&str>) -> Result<String, String> {
    let Some(name) = name else {
        return Ok(sheet(tools));
    };
    match name {
        "ops" => Ok(ops_detail(tools)),
        "times" => Ok(TIMES.to_owned()),
        "recipes" => Ok(RECIPES.to_owned()),
        "first_video" => Ok(FIRST_VIDEO.to_owned()),
        "podcast" => Ok(PODCAST.to_owned()),
        "lanes" => Ok(LANES.to_owned()),
        "ripple" => Ok(RIPPLE.to_owned()),
        "export" => Ok(EXPORT.to_owned()),
        "subtitles" => Ok(SUBTITLES.to_owned()),
        "captions" => Ok(captions_topic(tools)),
        "project" => Ok(PROJECT.to_owned()),
        "burn_captions" => Ok(BURN_CAPTIONS.to_owned()),
        "overlays" => Ok(OVERLAYS.to_owned()),
        "vertical" => Ok(VERTICAL.to_owned()),
        "sequences" => Ok(SEQUENCES.to_owned()),
        "multicam" => Ok(MULTICAM.to_owned()),
        "stretch" => Ok(STRETCH.to_owned()),
        "unsupported" => Ok(UNSUPPORTED.to_owned()),
        "visualizer" => Ok(VISUALIZER.to_owned()),
        "looks" => Ok(LOOKS.to_owned()),
        other => {
            if let Some(text) = op_topic(tools, other) {
                return Ok(text);
            }
            if let Some(tool) = tools.iter().find(|t| t.name == other) {
                return Ok(tool_topic(tool));
            }
            Err(unknown_topic(tools, other))
        }
    }
}

pub fn tool() -> Tool {
    Tool {
        name: "help",
        description: "The edith cheat sheet: addresses, times, undo, the tool list, edit ops and recipes. Pass topic for one part: ops (every edit op with its arguments), times, recipes, lanes, ripple, export, subtitles, captions, project, burn_captions or visualizer, an edit op name (its manual entry), or a tool name (its arguments).",
        schema: help_schema,
        run: run_help,
    }
}

fn help_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": topic_description()
            }
        }
    })
}

/// The schema's description of `topic`: every fixed topic, from `TOPICS`, then the op and tool forms.
fn topic_description() -> String {
    format!(
        "Omit for the cheat sheet. Topics: {}. An edit op name gives its manual entry; a tool name gives its arguments. \"ops\" lists every edit op with its arguments.",
        TOPICS.join(", ")
    )
}

fn run_help(_server: &mut Server, args: &Value) -> Reply {
    let name = args.get("topic").and_then(Value::as_str);
    match section(&catalogue(), name) {
        Ok(text) => Reply::ok(text),
        Err(text) => Reply::err(text),
    }
}

/// The first sentence of a tool description, with its whitespace flattened. A sentence
/// ends at a full stop followed by a capital, so `"<n>. track"` inside a description
/// does not end it.
fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let end = flat
        .char_indices()
        .filter(|&(i, c)| c == '.' && flat[i + 1..].starts_with(' ') && flat[i + 2..].starts_with(|n: char| n.is_uppercase()))
        .map(|(i, _)| i)
        .next();
    match end {
        Some(i) => flat[..=i].to_owned(),
        None => flat,
    }
}

/// The note under the tool list: every tool takes its own arguments, and the two
/// shapes an agent most often gets wrong.
const TOOL_ARGS: &str = "  Every tool takes its own arguments: help {topic:\"<tool>\"} lists any tool's arguments (media_probe takes paths, a glob, or both).
  burn_captions {track:N} (on export_start, frame, contact_sheet) draws one caption track into the picture.
  N is 0-based: the track index as the captions tool lists it, so the first track is 0.";

fn tool_list(tools: &[Tool]) -> String {
    let mut lines = vec!["TOOLS".to_owned(), TOOL_ARGS.to_owned()];
    for (group, names) in GROUPS {
        let rows: Vec<&Tool> = names.iter().filter_map(|n| tools.iter().find(|t| t.name == *n)).collect();
        if rows.is_empty() {
            continue;
        }
        lines.push(format!("  {group}:"));
        lines.extend(rows.iter().map(|t| format!("    {:<18}{}", t.name, first_sentence(t.description))));
    }
    let grouped = |name: &str| GROUPS.iter().any(|(_, ns)| ns.contains(&name));
    let rest: Vec<&Tool> = tools.iter().filter(|t| !grouped(t.name)).collect();
    if !rest.is_empty() {
        lines.push("  other:".to_owned());
        lines.extend(rest.iter().map(|t| format!("    {:<18}{}", t.name, first_sentence(t.description))));
    }
    lines.join("\n")
}

/// The op names of the `edit` tool, read from the enum in its schema.
fn edit_ops(tools: &[Tool]) -> Vec<String> {
    let Some(edit) = tools.iter().find(|t| t.name == "edit") else {
        return Vec::new();
    };
    let schema = (edit.schema)();
    schema["properties"]["ops"]["items"]["properties"]["op"]["enum"]
        .as_array()
        .map(|ops| ops.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// The manual for the edit ops: the `edit` tool's description, or "" if it is not registered.
fn edit_manual(tools: &[Tool]) -> &'static str {
    tools.iter().find(|t| t.name == "edit").map_or("", |t| t.description)
}

/// The op names on one wrapped list, for the overview.
fn ops_names(tools: &[Tool]) -> String {
    let ops = edit_ops(tools);
    let mut lines = vec!["EDIT OPS  edit {ops:[{op, ...}]}".to_owned()];
    if ops.is_empty() {
        lines.push("  (the edit tool is not registered)".to_owned());
    }
    lines.extend(wrap(&ops, 78).into_iter().map(|l| format!("  {l}")));
    lines.push("  help {topic:\"ops\"} for every edit op with its arguments; help {topic:\"<op>\"} for one op.".to_owned());
    lines.push("  help {topic:\"ripple\"}: which lanes a ripple op moves, and lanes:[...].".to_owned());
    lines.join("\n")
}

/// One entry of the edit manual: the section it sits under, the op it describes, and
/// the entry as the manual writes it.
struct OpEntry {
    heading: String,
    op: String,
    text: String,
}

/// Every entry the manual gives an edit op, in manual order. An entry starts at an
/// op name followed by ` {` where the line starts or follows a `·` or a `.`; so
/// `title_set ... title_remove {title} ...` splits into two entries.
fn manual_entries(manual: &str, ops: &[String]) -> Vec<OpEntry> {
    let mut entries = Vec::new();
    let mut heading = String::new();
    for line in manual.lines() {
        if is_heading(line) {
            line.trim().clone_into(&mut heading);
            continue;
        }
        let mut starts: Vec<(usize, &str)> = Vec::new();
        for op in ops {
            let pattern = format!("{op} {{");
            for (i, _) in line.match_indices(&pattern) {
                if starts_entry(&line[..i]) {
                    starts.push((i, op.as_str()));
                }
            }
        }
        starts.sort_by_key(|(i, _)| *i);
        for (k, (start, op)) in starts.iter().enumerate() {
            let end = starts.get(k + 1).map_or(line.len(), |(next, _)| *next);
            let text = line[*start..end].trim().trim_end_matches('·').trim_end().to_owned();
            entries.push(OpEntry { heading: heading.clone(), op: (*op).to_owned(), text });
        }
    }
    entries
}

/// A manual section heading: a line of capitals and spaces only.
fn is_heading(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c.is_ascii_uppercase() || c == ' ')
}

/// Whether an op entry can begin right after `before` on its line.
fn starts_entry(before: &str) -> bool {
    let t = before.trim_end();
    t.is_empty() || t.ends_with('·') || t.ends_with('.')
}

/// The argument signature of an entry: its head before the dash, with a `[group]` or
/// `[clip]` tag where the entry gives one.
fn signature(text: &str) -> String {
    let head = text.split(" — ").next().unwrap_or(text).trim_end_matches('.');
    let mut sig = head.replace("[group]", "").replace("[clip]", "").trim().to_owned();
    for tag in ["[group]", "[clip]"] {
        if text.contains(tag) {
            sig.push(' ');
            sig.push_str(tag);
        }
    }
    sig
}

/// An op named inside another op's entry, such as `captions_place {track}` on the
/// subtitles_import line: the heading it sits under, its signature as written, and the line.
struct Mention {
    heading: String,
    sig: String,
    line: String,
}

/// The first manual line that names `op` as `op {...}` inside another entry. The
/// name must not be the tail of a longer one (`move {` is not `sub_move {`).
fn mention(manual: &str, op: &str) -> Option<Mention> {
    let pattern = format!("{op} {{");
    let mut heading = String::new();
    for line in manual.lines() {
        if is_heading(line) {
            line.trim().clone_into(&mut heading);
            continue;
        }
        for (i, _) in line.match_indices(&pattern) {
            let bounded = line[..i].chars().last().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            if !bounded {
                continue;
            }
            let rest = &line[i + op.len() + 1..];
            if let Some(close) = rest.find('}') {
                return Some(Mention {
                    heading: heading.clone(),
                    sig: format!("{op} {}", &rest[..=close]),
                    line: line.trim().to_owned(),
                });
            }
        }
    }
    None
}

/// Adds a signature under its heading, opening the heading the first time it is seen.
fn push_row(groups: &mut Vec<(String, Vec<String>)>, heading: &str, sig: String) {
    match groups.iter_mut().find(|(h, _)| h == heading) {
        Some((_, sigs)) => sigs.push(sig),
        None => groups.push((heading.to_owned(), vec![sig])),
    }
}

/// The signature of an op from its `OP_TEXTS` override, when it has one. The manual line can
/// be shorter than the op (the manual's transform line has no `opacity`), so the override's
/// head wins, as its entry does in `help {topic:"<op>"}`.
fn override_signature(op: &str) -> Option<String> {
    OP_TEXTS.iter().find(|(o, _)| *o == op).map(|(_, text)| signature(text))
}

/// `help {topic:"ops"}`: every edit op with its argument signature, one per line,
/// under the manual's headings. Each heading appears once, and an op the manual only
/// names inside another entry gets the signature written there.
/// A whole batch in the form `edit` takes: each op is a flat object, `{"op": name, ...args}`.
/// The op-list lines below show `place {source, at, …}`, which is the argument list, not the shape.
const OPS_EXAMPLE: &str = r#"  Example batch (each op is a flat object with "op" and its arguments): edit {"ops":[{"op":"place","source":"src:0","at":"0s"},{"op":"fade","clip":"V1@0","in":"15f"}]}"#;

fn ops_detail(tools: &[Tool]) -> String {
    let ops = edit_ops(tools);
    let mut lines = vec![
        "EDIT OPS, with arguments  (help {topic:\"<op>\"} for one op's full entry)".to_owned(),
        "  [group] = the op moves, trims or deletes the whole linked take; [clip] = it acts on that one clip alone.".to_owned(),
        "  swap is tagged [clip], but a clip in a linked take is refused until the take is ungrouped (ungroup first).".to_owned(),
        OPS_EXAMPLE.to_owned(),
    ];
    if ops.is_empty() {
        lines.push("  (the edit tool is not registered)".to_owned());
        return lines.join("\n");
    }
    let manual = edit_manual(tools);
    let entries = manual_entries(manual, &ops);
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut placed: Vec<&str> = Vec::new();
    for e in &entries {
        if placed.contains(&e.op.as_str()) {
            continue;
        }
        placed.push(&e.op);
        push_row(&mut groups, &e.heading, override_signature(&e.op).unwrap_or_else(|| signature(&e.text)));
    }
    for op in &ops {
        if placed.contains(&op.as_str()) {
            continue;
        }
        match mention(manual, op) {
            Some(m) => push_row(&mut groups, &m.heading, override_signature(op).unwrap_or(m.sig)),
            // An edit_subs op the manual does not mention still sits under CAPTIONS.
            None if crate::tools::edit_subs::OPS.contains(&op.as_str()) => {
                push_row(&mut groups, "CAPTIONS", override_signature(op).unwrap_or_else(|| op.clone()));
            }
            None => push_row(&mut groups, "OTHER", op.clone()),
        }
    }
    for (heading, sigs) in groups {
        lines.push(heading);
        lines.extend(sigs.into_iter().map(|s| format!("  {s}")));
    }
    lines.push(MARKERS.to_owned());
    lines.join("\n")
}

/// `help {topic:"<op>"}`: that op's manual entry, or None if `op` is not an edit op.
/// An `OP_TEXTS` override replaces the manual's text; the heading still comes from the manual.
fn op_topic(tools: &[Tool], op: &str) -> Option<String> {
    let ops = edit_ops(tools);
    if !ops.iter().any(|o| o == op) {
        return None;
    }
    let manual = edit_manual(tools);
    let text = OP_TEXTS.iter().find(|(o, _)| *o == op).map(|(_, t)| (*t).to_owned());
    if let Some(e) = manual_entries(manual, &ops).into_iter().find(|e| e.op == op) {
        let text = text.unwrap_or(e.text);
        return Some(format!("edit op `{op}` (under {}):\n  {text}", e.heading));
    }
    Some(match (mention(manual, op), text) {
        (Some(m), Some(text)) => format!("edit op `{op}` (under {}):\n  {text}", m.heading),
        (Some(m), None) => format!(
            "edit op `{op}` (under {}):\n  {}\n  the manual names it in another entry: {}",
            m.heading, m.sig, m.line
        ),
        (None, Some(text)) => format!("edit op `{op}`:\n  {text}"),
        (None, None) => format!("edit op `{op}`: the edit manual has no entry for it"),
    })
}

/// The refusal for a tool name nothing answers. An edit op name gets the shape it
/// goes in; any other name gets the nearest tool names.
pub fn unknown_tool(tools: &[Tool], name: &str) -> String {
    if edit_ops(tools).iter().any(|o| o == name) {
        return format!("{name} is an edit op: edit {{ops:[{{op:\"{name}\", …}}]}} — help {{topic:\"{name}\"}}");
    }
    let limit = (name.chars().count() / 3).max(2);
    let mut near: Vec<(usize, &str)> = tools
        .iter()
        .map(|t| (edit_distance(name, t.name), t.name))
        .filter(|(d, _)| *d <= limit)
        .collect();
    near.sort();
    let names: Vec<&str> = near.iter().take(3).map(|(_, n)| *n).collect();
    if names.is_empty() {
        format!("unknown tool `{name}`; help lists every tool")
    } else {
        format!("unknown tool `{name}`; did you mean {}? help lists every tool", names.join(", "))
    }
}

/// The Levenshtein distance between two names, in characters.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `help {topic:"<tool>"}`: the tool's description and its argument list from its schema.
fn tool_topic(tool: &Tool) -> String {
    let schema = (tool.schema)();
    let mut lines = vec![format!("tool `{}`", tool.name), tool.description.to_owned(), String::new(), "ARGUMENTS".to_owned()];
    let Some(props) = schema.get("properties").and_then(Value::as_object).filter(|p| !p.is_empty()) else {
        lines.push("  (none)".to_owned());
        return lines.join("\n");
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // The name column is as wide as the longest name, so no two names run together.
    let width = props.keys().map(|n| n.chars().count()).max().unwrap_or(0).max(12) + 2;
    for (name, prop) in props {
        let types = schema_types(prop);
        let kind = if types.is_empty() { "any".to_owned() } else { types.join("|") };
        let req = if required.contains(&name.as_str()) { "required" } else { "optional" };
        let mut desc = prop.get("description").and_then(Value::as_str).unwrap_or("").to_owned();
        if let Some(options) = prop.get("enum").and_then(Value::as_array) {
            let names: Vec<String> = options.iter().map(scalar_text).collect();
            if !names.is_empty() {
                desc = format!("{desc} one of: {}", names.join(", ")).trim_start().to_owned();
            }
        }
        let e_g = format!("(e.g. {})", example(name, prop));
        let desc = if desc.is_empty() { e_g } else { format!("{desc} {e_g}") };
        lines.push(format!("  {name:<width$}{kind:<16}{req:<10}{desc}"));
    }
    lines.join("\n")
}

/// The JSON types a schema property takes: its `type` (one name or a list), or, when it has
/// none, the types of its `anyOf` or `oneOf` branches. Empty when the property names no type.
fn schema_types(prop: &Value) -> Vec<String> {
    match prop.get("type") {
        Some(Value::String(t)) => return vec![t.clone()],
        Some(Value::Array(ts)) => return ts.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        _ => {}
    }
    let mut types: Vec<String> = Vec::new();
    for branch in prop.get("oneOf").or_else(|| prop.get("anyOf")).and_then(Value::as_array).into_iter().flatten() {
        for t in schema_types(branch) {
            if !types.contains(&t) {
                types.push(t);
            }
        }
    }
    types
}

/// One value of an argument's schema as JSON text, for the `e.g.` in help and in a refusal:
/// the schema's `examples` or `default` when it has one, else the first enum option, else a
/// value of its type. An array shows its item shape, `["clip.mp4"]`. Untyped parts show `…`.
pub fn example(key: &str, prop: &Value) -> String {
    let desc = prop.get("description").and_then(Value::as_str).unwrap_or("");
    shaped_example(key, prop, desc, false)
}

/// The example for a value of `prop`, named `key`, described by `desc`. `in_list` is set for the
/// item of a list, where a time description does not make the item a time.
fn shaped_example(key: &str, prop: &Value, desc: &str, in_list: bool) -> String {
    if let Some(first) = prop.get("examples").and_then(Value::as_array).and_then(|a| a.first()) {
        return first.to_string();
    }
    if let Some(options) = prop.get("enum").and_then(Value::as_array).filter(|o| !o.is_empty()) {
        return options[0].to_string();
    }
    if let Some(default) = prop.get("default") {
        return default.to_string();
    }
    // An anyOf or oneOf with no type of its own shows its first branch.
    if prop.get("type").is_none()
        && let Some(branch) = prop.get("oneOf").or_else(|| prop.get("anyOf")).and_then(Value::as_array).and_then(|b| b.first())
    {
        return shaped_example(key, branch, desc, in_list);
    }
    let types = schema_types(prop);
    // A type that allows a list shows the list: the usual shape, and the one a caller most needs.
    let Some(ty) = types.iter().find(|t| t.as_str() == "array").or(types.first()) else {
        return "…".to_owned();
    };
    match ty.as_str() {
        "string" => string_example(key, desc, in_list),
        // The schema's minimum is a valid value, so it is the example, except a bitrate, where 1 kbps is valid but useless.
        "integer" | "number" => match (key, prop.get("minimum")) {
            ("bitrate_kbps", _) => "8000".to_owned(),
            (_, Some(min)) => min.to_string(),
            _ => "1".to_owned(),
        },
        "boolean" => "true".to_owned(),
        "array" => {
            // The usual lists, by name: a list of lanes, and the edit batch.
            match key {
                "lanes" => return "[\"V1\",\"A1\"]".to_owned(),
                "sources" => return "[\"src:0\",\"src:1\"]".to_owned(),
                "speakers" => return "[\"A1\",\"A2\"]".to_owned(),
                "range" => return "[\"0s\",\"5s\"]".to_owned(),
                "search" => return "[\"~/Videos\"]".to_owned(),
                "ops" => return OPS_EXAMPLE_VALUE.to_owned(),
                _ => {}
            }
            let items = prop.get("items").cloned().unwrap_or_else(|| json!({"type": "string"}));
            format!("[{}]", shaped_example(key, &items, desc, true))
        }
        "object" => {
            if key == "range" {
                return "{\"from\": \"0s\", \"to\": \"5s\"}".to_owned();
            }
            let fields: Vec<String> = prop
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.iter().map(|(k, v)| format!("\"{k}\": {}", example(k, v))).collect())
                .unwrap_or_default();
            if fields.is_empty() { "{}".to_owned() } else { format!("{{{}}}", fields.join(", ")) }
        }
        _ => "…".to_owned(),
    }
}

/// The example for the `ops` argument: a list holding one place op, a flat object with "op" and its arguments.
const OPS_EXAMPLE_VALUE: &str = r#"[{"op": "place", "source": "src:0", "at": "0s"}]"#;

/// A string argument's example, by its name, then by its description. A file name shows the file it
/// takes (a project, a subtitle file, an output movie, or an input clip); an address, a source, a
/// lane or a time shows one of its own kind. A name it does not know shows "text".
fn string_example(key: &str, desc: &str, in_list: bool) -> String {
    let is_out = key == "out" || key.starts_with("out_") || key.ends_with("_out");
    let text = match key {
        _ if is_out => "cut.mp4",
        "save" => "cut.png",
        "glob" => "*.mp4",
        "folder" => "film_media",
        "clip" => "V1@0",
        "source" => "src:0",
        "lane" | "target" => "A1",
        "at" | "until" | "length" => "2s",
        "size" | "resolution" => "1280x720",
        "label" => "Draft cut",
        "name" | "new_name" | "sequence" => "Intro",
        "bin" | "parent" => "bin:1",
        "hint" => "Edith, LUFS",
        "language" => "en",
        "voice" => "en_US-ryan-high",
        "query" => "um",
        "text" => "Hello there",
        "notes" => "Rough cut, music to come",
        _ if key.contains("path") || key.contains("file") => {
            if desc.contains(".edith") {
                "talk.edith"
            } else if desc.contains(".srt") {
                "talk.srt"
            } else {
                "clip.mp4"
            }
        }
        _ if !in_list && desc.to_ascii_lowercase().contains("time") => "2s",
        _ if desc.contains("address") => "V1@0",
        _ => "text",
    };
    format!("\"{text}\"")
}

/// An enum option as the help shows it: a string as is, a number as its digits.
fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The captions workflow, then the `captions` tool's own arguments: the tool shares its
/// name with this topic, so the topic has to answer for both.
fn captions_topic(tools: &[Tool]) -> String {
    match tools.iter().find(|t| t.name == "captions") {
        Some(tool) => format!("{CAPTIONS}\n\n{}", tool_topic(tool)),
        None => CAPTIONS.to_owned(),
    }
}

fn unknown_topic(tools: &[Tool], name: &str) -> String {
    let ops = edit_ops(tools);
    let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
    format!(
        "unknown help topic `{name}`. Topics: {}. Edit ops: {}. Tools: {}.",
        TOPICS.join(", "),
        ops.join(", "),
        names.join(", ")
    )
}

fn wrap(words: &[String], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for w in words {
        if !cur.is_empty() && cur.len() + 2 + w.len() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str(", ");
        }
        cur.push_str(w);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ops_example_batch_uses_real_op_names_and_arguments() {
        let tools = catalogue();
        let text = section(&tools, Some("ops")).expect("ops topic");
        assert!(text.contains(OPS_EXAMPLE), "{text}");
        let json_text = &OPS_EXAMPLE[OPS_EXAMPLE.find("{\"ops\"").expect("the example has an ops list")..];
        let batch: Value = serde_json::from_str(json_text).expect("the example is valid JSON");
        let manual = edit_manual(&tools);
        let ops = edit_ops(&tools);
        for op in batch["ops"].as_array().expect("an ops array") {
            let name = op["op"].as_str().expect("each op is a flat object with an \"op\" name");
            assert!(ops.iter().any(|o| o == name), "`{name}` is not an edit op");
            // The manual line is `name {arg, arg?, …} — …`: every key of the example must be one of its arguments.
            let line = manual
                .lines()
                .map(str::trim_start)
                .find(|l| l.starts_with(&format!("{name} {{")))
                .unwrap_or_else(|| panic!("no manual line for `{name}`"));
            let args = line[name.len() + 2..].split('}').next().unwrap_or("");
            let known: Vec<&str> = args.split(',').map(|a| a.trim().trim_end_matches('?').split('=').next().unwrap_or("").trim()).collect();
            for key in op.as_object().expect("an object").keys().filter(|k| k.as_str() != "op") {
                assert!(known.contains(&key.as_str()), "`{name}` has no argument `{key}`: {line}");
            }
        }
    }

    #[test]
    fn a_tool_summary_stops_at_a_sentence_not_at_a_numbered_cue() {
        let captions = "List the cues on caption lanes (S1..) in timeline time, one line per cue: \"<n>. track T cue C  text\". Use the track and cue numbers with the cue_text op. Only cues that a placed caption shows.";
        assert_eq!(
            first_sentence(captions),
            "List the cues on caption lanes (S1..) in timeline time, one line per cue: \"<n>. track T cue C text\"."
        );
        assert_eq!(first_sentence("One sentence only."), "One sentence only.");
    }

    #[test]
    fn sheet_names_every_tool_and_groups_them_all() {
        let tools = catalogue();
        let text = sheet(&tools);
        for t in &tools {
            assert!(text.contains(t.name), "sheet does not name `{}`", t.name);
        }
        assert!(!text.contains("  other:"), "a registered tool is missing from GROUPS");
    }

    #[test]
    fn sheet_covers_addresses_and_undo() {
        let text = sheet(&catalogue());
        assert!(text.contains("V1@"));
        assert!(text.contains("keep"));
        assert!(text.contains("undo"));
    }

    #[test]
    fn overview_points_at_the_ops_and_op_topics_without_their_arguments() {
        let text = sheet(&catalogue());
        assert!(text.contains("help {topic:\"ops\"}"));
        assert!(text.contains("help {topic:\"<op>\"}"));
        assert!(!text.contains("rate 0.25..4"), "the overview should not carry op entries");
    }

    #[test]
    fn ops_come_from_the_edit_schema() {
        let ops = edit_ops(&catalogue());
        assert!(ops.iter().any(|o| o == "keep"));
        assert!(ops.iter().any(|o| o == "cut_regions"));
        let text = section(&catalogue(), Some("ops")).expect("ops topic");
        for op in &ops {
            assert!(text.contains(op.as_str()), "ops topic lacks `{op}`");
        }
    }

    #[test]
    fn ops_topic_gives_one_signature_per_line_under_headings() {
        let text = section(&catalogue(), Some("ops")).expect("ops topic");
        assert!(text.lines().any(|l| l.starts_with("  speed {clip, x, ripple?") && l.ends_with("[group]")), "speed takes x:\n{text}");
        assert!(!text.contains("factor"));
        assert!(text.lines().any(|l| l == "  lane_remove {lane}"), "lane_remove has its own line");
        assert!(text.lines().any(|l| l == "TIME"), "headings group the ops");
        assert!(text.lines().any(|l| l.starts_with("  captions_place")), "an op without its own entry is still listed");
        assert!(!text.lines().any(|l| l == "OTHER"), "named ops file under their heading");
        assert!(text.lines().any(|l| l == "  trim {clip, edge:in|out, to, scope?} [group]"));
        assert!(text.lines().any(|l| l == "  subtitles_lang {track, lang}"), "subtitles_lang is listed under CAPTIONS");
    }

    #[test]
    fn one_op_gives_its_manual_entry() {
        let text = section(&catalogue(), Some("speed")).expect("speed topic");
        assert!(text.contains("speed {clip, x, ripple?") && text.contains("— rate 0.25..4"), "{text}");
        assert!(text.contains("TIME"));
    }

    #[test]
    fn new_topics_answer_and_the_limiter_and_eq_entries_are_whole() {
        let tools = catalogue();
        for topic in ["lanes", "ripple", "export", "subtitles", "captions", "project", "burn_captions", "visualizer"] {
            let text = section(&tools, Some(topic)).unwrap_or_else(|e| panic!("{topic}: {e}"));
            assert!(text.lines().count() > 2, "{topic} is a real section:\n{text}");
        }
        let captions = section(&tools, Some("captions")).unwrap();
        assert!(captions.contains("tool `captions`"), "the captions topic also gives the tool's arguments");
        assert!(captions.contains("burn_captions") && captions.contains("auto_captions"), "{captions}");
        let subtitles = section(&tools, Some("subtitles")).unwrap();
        assert!(!subtitles.contains("auto_captions") && !subtitles.contains("export_start"), "{subtitles}");
        assert_ne!(captions.lines().nth(2), subtitles.lines().nth(2), "captions does not repeat subtitles");
        let limiter = section(&tools, Some("limiter")).unwrap();
        assert!(limiter.contains("ceiling_db -24 to 0 dB"), "{limiter}");
        assert!(!limiter.ends_with("— ceiling"), "{limiter}");
        let eq = section(&tools, Some("eq")).unwrap();
        assert!(eq.contains("low_cut") && eq.contains("f 20-20000 Hz"), "{eq}");
        let err = section(&tools, Some("nonsense")).unwrap_err();
        assert!(err.contains("lanes") && err.contains("visualizer"), "{err}");
    }

    #[test]
    fn two_ops_on_one_line_get_their_own_entries() {
        let remove = section(&catalogue(), Some("title_remove")).expect("title_remove topic");
        assert!(remove.contains("title_remove {title} — takes it off."), "{remove}");
        assert!(!remove.contains("merged onto the title"), "title_set text leaked: {remove}");
        let title_set = section(&catalogue(), Some("title_set")).expect("title_set topic");
        assert!(!title_set.contains("takes it off"), "title_remove text leaked: {title_set}");
    }

    #[test]
    fn tool_name_gives_its_description_and_arguments() {
        let text = section(&catalogue(), Some("edit")).expect("edit topic");
        assert!(text.starts_with("tool `edit`"));
        assert!(text.contains("Batched edit"));
        assert!(text.contains("ARGUMENTS"));
        assert!(text.contains("  ops"), "the ops argument is listed:\n{text}");
    }

    /// A name longer than the old fixed column keeps a gap before its type, and a type given as a
    /// list (`boolean` or `string`) is shown as `boolean|string`.
    #[test]
    fn long_argument_names_keep_a_gap_and_list_types_are_shown() {
        let text = section(&catalogue(), Some("project_open")).expect("project_open topic");
        assert!(text.contains("  discard_autosave  boolean"), "{text}");
        assert!(text.contains("  recover           boolean|string"), "{text}");
        assert!(!text.contains("discard_autosaveboolean"), "{text}");
    }

    #[test]
    fn unknown_topic_lists_the_valid_ones() {
        let err = section(&catalogue(), Some("nope")).unwrap_err();
        assert!(err.contains("ops, times, recipes"));
        assert!(err.contains("speed"));
        assert!(err.contains("help"));
    }

    #[test]
    fn recipes_mention_silence_on_a_source_and_a_black_lead_in() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains("src:2"));
        assert!(text.contains("Black lead-in"));
    }

    #[test]
    fn recipes_say_assemble_refuses_stills_and_how_to_add_one() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains("assemble refuses a still image"), "{text}");
        assert!(text.contains("{op:\"place\", source, at:\"end\"}"), "{text}");
        assert!(text.contains("{op:\"append\", source}"), "{text}");
        assert!(text.contains("items:[{source, in?, out?}]"), "{text}");
    }

    #[test]
    fn short_is_the_first_25_lines_plus_a_pointer() {
        let text = short(&catalogue());
        assert!(text.contains("V1@"));
        assert!(text.lines().count() <= 26);
        assert!(text.contains("edith://help"));
    }

    #[test]
    fn ops_topic_has_no_empty_one_of_and_no_repeated_heading() {
        let text = section(&catalogue(), Some("ops")).expect("ops topic");
        assert!(!text.contains("one of:\n") && !text.trim_end().ends_with("one of:"), "{text}");
        assert_eq!(text.lines().filter(|l| *l == "CAPTIONS").count(), 1, "CAPTIONS heading repeats:\n{text}");
        assert!(!text.contains("(named in another entry)"), "{text}");
        for heading in ["PLACE", "CUT", "TIME", "LOOK", "SOUND", "LANES", "CAPTIONS", "TITLES"] {
            assert_eq!(text.lines().filter(|l| *l == heading).count(), 1, "{heading} once:\n{text}");
        }
    }

    #[test]
    fn captions_place_lists_its_full_arguments_in_ops_and_its_topic() {
        let text = section(&catalogue(), Some("captions_place")).expect("captions_place topic");
        assert!(text.contains("edit op `captions_place` (under CAPTIONS):\n  captions_place {track, lane?, at?}"), "{text}");
        assert!(!text.contains("as the edit manual mentions it"), "{text}");
        let ops = section(&catalogue(), Some("ops")).expect("ops topic");
        assert!(ops.lines().any(|l| l == "  captions_place {track, lane?, at?}"), "{ops}");
        assert!(!ops.lines().any(|l| l == "  captions_place {track}"), "{ops}");
    }

    /// The facts the topics state about current behaviour: captions addresses and time edits,
    /// the assemble lane default, transform keys across trims, surround and sound-only sources,
    /// the export peak warning and the earlier-session status, and the autosave options.
    #[test]
    fn current_behaviour_is_named_in_its_topic() {
        let tools = catalogue();
        let text = |t: &str| section(&tools, Some(t)).unwrap_or_else(|e| panic!("{t}: {e}"));
        let captions = text("captions");
        assert!(captions.contains("track T cue C [S1@F]"), "{captions}");
        assert!(captions.contains("nearest the cut"), "{captions}");
        assert!(captions.contains("no speech to caption"), "{captions}");
        let assemble = section(&tools, Some("assemble")).expect("assemble topic");
        assert!(assemble.contains("With no lane the picture goes to V1 and its sound to A1"), "{assemble}");
        assert!(assemble.contains("0 when that lane is empty"), "{assemble}");
        let keys = section(&tools, Some("transform_keys")).expect("transform_keys topic");
        assert!(keys.contains("adds a key at the new edge"), "{keys}");
        assert!(keys.contains("split and move carry the keys"), "{keys}");
        let lanes = text("lanes");
        assert!(lanes.contains("mixed to stereo on export"), "{lanes}");
        assert!(lanes.contains("placed with no lane, it goes to A1 alone"), "{lanes}");
        let export = text("export");
        assert!(export.contains("project_settings {limiter:{on:true, ceiling_db:-1}}"), "{export}");
        assert!(export.contains("the progress\n    of a job from an earlier session is not known"), "{export}");
        let project = text("project");
        assert!(project.contains("discard_autosave:true: this session's edits are saved"), "{project}");
        assert!(project.contains("recover:true opens the saved file with the autosave's edits on top as one undo step, so undo takes them back\n    (with this session's own unsaved edits open it needs discard:true"), "{project}");
        assert!(project.contains("~/.cache/edith/frames/<project>/"), "{project}");
        // The cut, the captions, the export wait, freeze and burn, and the times: each states what the code does.
        let cut = text("recipes");
        assert!(cut.contains("keep:N is the total pause left in each silence, padding included"), "{cut}");
        assert!(cut.contains("makes a joint cut with no target"), "{cut}");
        assert!(cut.contains("reported as out of step"), "{cut}");
        assert!(cut.contains("keeps `keep` in all after the last speech"), "{cut}");
        assert!(captions.contains("keeps its word on the longer piece only (ties: the earlier one)"), "{captions}");
        assert!(captions.contains("numbers come out as digits"), "{captions}");
        assert!(export.contains("A stop signal (SIGTERM or SIGINT) cancels them"), "{export}");
        assert!(export.contains("m4a (audio only; m4a is AAC in an mp4 box)"), "{export}");
        assert!(export.contains("is the sound alone (AAC, no picture track)"), "{export}");
        assert!(export.contains("Pictures are 8-bit 4:2:0 except in a 10-bit AV1 or HEVC export"), "{export}");
        assert!(export.contains("chapters (default true)") && export.contains("chapters:false leaves them out"), "{export}");
        assert!(export.contains("H.264 and every other format refuse it"), "{export}");
        assert!(export.contains("Poll export_status until it finishes"), "{export}");
        let burn = text("burn_captions");
        assert!(burn.contains("false for none"), "{burn}");
        assert!(burn.contains("at 1080x1920"), "{burn}");
        assert!(burn.contains("Lines keep the cue's own line breaks"), "{burn}");
        assert!(text("times").contains("2.5 s at 25 fps is frame 63"), "{}", text("times"));
        assert!(text("freeze").contains("insert:false overlays the still without pushing"), "{}", text("freeze"));
    }

    #[test]
    fn mention_does_not_match_the_tail_of_a_longer_op() {
        let manual = "CUT\nsub_move {clip, at, to_lane?} [group]\nmove {clip, at, to_lane?} [group]";
        let m = mention(manual, "move").expect("move is named");
        assert_eq!(m.sig, "move {clip, at, to_lane?}");
        assert_eq!(m.heading, "CUT");
        assert!(mention("CUT\nsub_move {clip} [group]", "move").is_none());
    }

    #[test]
    fn enum_options_show_numbers_and_skip_an_empty_list() {
        let text = section(&catalogue(), Some("export_start")).expect("export_start topic");
        assert!(text.contains("one of: 128, 192, 256, 320"), "audio_kbps lists its values:\n{text}");
        assert!(!text.contains("one of: \n") && !text.trim_end().ends_with("one of:"), "{text}");
        fn empty_enum() -> Value {
            json!({"properties": {"x": {"type": "integer", "enum": []}}})
        }
        let tool = Tool { name: "fake", description: "d", schema: empty_enum, run: run_help };
        let shown = tool_topic(&tool);
        assert!(!shown.contains("one of"), "an empty enum shows no list: {shown}");
    }

    #[test]
    fn overview_says_tools_take_their_own_arguments_and_burn_captions_shape() {
        let text = sheet(&catalogue());
        assert!(text.contains("help {topic:\"<tool>\"} lists any tool's arguments"));
        assert!(text.contains("media_probe takes paths, a glob, or both"));
        assert!(text.contains("burn_captions {track:N}"));
        assert!(text.contains("N is 0-based"));
    }

    #[test]
    fn unknown_tool_hints_an_edit_op_and_suggests_near_names() {
        let tools = catalogue();
        assert_eq!(
            unknown_tool(&tools, "lane_add"),
            "lane_add is an edit op: edit {ops:[{op:\"lane_add\", …}]} — help {topic:\"lane_add\"}"
        );
        let near = unknown_tool(&tools, "project_sav");
        assert!(near.contains("did you mean") && near.contains("project_save"), "{near}");
        let far = unknown_tool(&tools, "zzzzzzzzzz");
        assert!(far.starts_with("unknown tool `zzzzzzzzzz`") && !far.contains("did you mean"), "{far}");
    }

    /// The body of a section: its text without the first line (the op heading or the topic title).
    fn body(text: &str) -> &str {
        text.split_once('\n').map_or("", |(_, rest)| rest).trim()
    }

    #[test]
    fn every_op_entry_ends_at_a_sentence_end() {
        let tools = catalogue();
        for op in edit_ops(&tools) {
            let text = section(&tools, Some(&op)).expect("op topic");
            let last = body(&text).trim_end_matches(" [group]").trim_end_matches(" [clip]");
            assert!(
                last.ends_with(['.', ')', '}', '"', '`']),
                "`{op}` ends mid-sentence: {last:?}"
            );
        }
    }

    #[test]
    fn every_topic_ends_at_a_sentence_end_or_a_list_line() {
        let tools = catalogue();
        for topic in ["times", "recipes", "lanes", "ripple", "export", "subtitles", "project", "burn_captions", "visualizer", "values"] {
            let Ok(text) = section(&tools, Some(topic)) else { continue };
            let last = text.lines().last().unwrap_or("").trim_end();
            assert!(
                last.ends_with(['.', ')', '}', '"', '`']) || last.contains("  "),
                "`{topic}` ends mid-sentence: {last:?}"
            );
        }
    }

    #[test]
    fn no_sentence_is_copied_into_two_op_entries() {
        let tools = catalogue();
        let mut seen: Vec<(String, String)> = Vec::new();
        for op in edit_ops(&tools) {
            let text = section(&tools, Some(&op)).expect("op topic");
            for sentence in body(&text).split(". ").map(str::trim).filter(|s| s.len() >= 40) {
                if let Some((other, _)) = seen.iter().find(|(_, s)| s == sentence) {
                    panic!("`{op}` repeats a sentence from `{other}`: {sentence}");
                }
                seen.push((op.clone(), sentence.to_owned()));
            }
        }
    }

    /// delete ripples, ripple_delete is the same cut with more reported, and lift is the one that leaves a gap.
    #[test]
    fn delete_ripples_and_lift_leaves_a_gap() {
        let tools = catalogue();
        let delete = section(&tools, Some("delete")).expect("delete topic");
        assert!(delete.contains("RIPPLE") && delete.contains("close the gap"), "{delete}");
        assert!(delete.contains("same cut as ripple_delete"), "{delete}");
        let lift = section(&tools, Some("lift")).expect("lift topic");
        assert!(lift.contains("LEAVE A GAP") && lift.contains("nothing moves"), "{lift}");
        assert!(!lift.contains("RIPPLE"), "{lift}");
        let ripple = section(&tools, Some("ripple")).expect("ripple topic");
        assert!(ripple.contains("lift is not a ripple"), "{ripple}");
    }

    #[test]
    fn ops_header_defines_the_group_and_clip_tags() {
        let text = section(&catalogue(), Some("ops")).expect("ops topic");
        assert!(text.contains("[group] = ") && text.contains("[clip] = "), "{text}");
        let lift = section(&catalogue(), Some("lift")).expect("lift topic");
        assert!(lift.contains("[clip]"), "lift is tagged [clip]: {lift}");
    }

    #[test]
    fn captions_and_subtitles_show_their_ops_as_edit_calls() {
        let tools = catalogue();
        for topic in ["subtitles", "captions"] {
            let text = section(&tools, Some(topic)).expect("topic");
            assert!(!text.contains("cue_text {track") && !text.contains("subtitles_import {path}"), "{topic} writes ops as tool calls:\n{text}");
        }
        let subtitles = section(&tools, Some("subtitles")).expect("subtitles topic");
        for op in ["subtitles_import", "captions_place", "cue_text", "cue_delete", "cue_replace"] {
            assert!(subtitles.contains(&format!("edit {{ops:[{{op:\"{op}\"")), "subtitles does not show `{op}` as an edit call:\n{subtitles}");
        }
    }

    #[test]
    fn locked_lanes_are_described_as_the_rule_has_it() {
        let text = section(&catalogue(), Some("lanes")).expect("lanes topic");
        assert!(!text.contains("refuses every edit"), "{text}");
        assert!(text.contains("never changed") && text.contains("skip it with a note"), "{text}");
    }

    #[test]
    fn cut_silence_lanes_say_what_the_scope_and_the_locks_do() {
        let recipe = RECIPES;
        assert!(recipe.contains("with a target, it is the ripple scope, judged on the target alone"), "{recipe}");
        assert!(recipe.contains("the target's lane is added if missing"), "{recipe}");
        assert!(recipe.contains("Without a target, two or more audio lanes are cut jointly where all are quiet"), "{recipe}");
        assert!(recipe.contains("a locked lane named in lanes, or linked to the target, refuses the cut (unlock it with lane_flags)"), "{recipe}");
        assert!(recipe.contains("a mix's default scope (every lane) refuses the same way, naming each locked lane, and changes nothing"), "{recipe}");
    }

    #[test]
    fn find_words_cut_says_what_the_locks_do() {
        let recipe = RECIPES;
        assert!(recipe.contains("A locked lane the cut would move refuses it, dry runs too (unlock it with lane_flags)"), "{recipe}");
        assert!(recipe.contains("a mix's default scope skips its locked lanes with a note"), "{recipe}");
    }

    #[test]
    fn the_true_peak_warning_names_the_ceiling_it_checks() {
        assert!(EXPORT.contains("-1 dBTP for lossless") && EXPORT.contains("-2 dBTP for lossy"), "export help names both ceilings");
        assert!(EXPORT.contains("above -1 dBTP warns") && !EXPORT.contains("over 0 dBTP warns"), "export help states the warning ceiling");
        assert!(EXPORT.contains("intra_only: hevc and hevc-mp4: true codes every picture as an IDR"), "export help names intra_only");
        assert!(EXPORT.contains("burn_captions is refused for it"), "export help says burn_captions is refused sound-only");
    }

    #[test]
    fn gain_and_ripple_delete_are_whole_entries() {
        let tools = catalogue();
        let gain = section(&tools, Some("gain")).expect("gain topic");
        assert!(gain.contains("Audio clips only") && gain.ends_with('.'), "{gain}");
        let ripple_delete = section(&tools, Some("ripple_delete")).expect("ripple_delete topic");
        assert!(!ripple_delete.contains("RIPPLE RULE above"), "{ripple_delete}");
        assert!(ripple_delete.contains("help {topic:\"ripple\"}"), "{ripple_delete}");
    }

    #[test]
    fn move_says_a_drop_on_empty_frames_may_jump_over_clips() {
        let text = section(&catalogue(), Some("move")).expect("move topic");
        assert!(text.contains("jumping over any clips between"), "{text}");
    }

    #[test]
    fn move_insert_says_it_refuses_a_ripple_over_a_clip_outside_the_take() {
        let text = section(&catalogue(), Some("move")).expect("move topic");
        assert!(text.contains("insert:true refuses and names it: move without insert, or lift that clip and close_gaps"), "{text}");
    }

    #[test]
    fn help_tool_answers_a_topic() {
        let mut server = Server::new();
        let reply = (tool().run)(&mut server, &json!({"topic": "ops"}));
        assert!(!reply.is_error);
        assert!(reply.text.contains("keep"));
        let op = (tool().run)(&mut server, &json!({"topic": "keep"}));
        assert!(!op.is_error);
        let bad = (tool().run)(&mut server, &json!({"topic": "nope"}));
        assert!(bad.is_error);
    }
}

#[cfg(test)]
mod topic_tests {
    use super::*;

    /// The top-level topic list names every topic the help answers, the new ones included.
    #[test]
    fn the_header_lists_every_topic() {
        for t in TOPICS {
            assert!(header().contains(t), "header misses topic {t}");
        }
    }

    /// The topic names after `Topics: ` in a sentence, in the order given.
    fn listed_topics(text: &str) -> Vec<String> {
        let after = text.split_once("Topics: ").expect("a Topics: list").1;
        after.split('.').next().unwrap_or("").split(", ").map(str::to_owned).collect()
    }

    /// The cheat sheet's topic list and the `help` schema's both name exactly the topics that
    /// `section` answers, in `TOPICS` order, and every one of them answers.
    #[test]
    fn schema_and_sheet_list_the_same_topics_as_section_answers() {
        let tools = catalogue();
        let expected: Vec<String> = TOPICS.iter().map(|t| (*t).to_owned()).collect();
        assert_eq!(listed_topics(&header()), expected, "the cheat sheet's topic list");
        assert_eq!(listed_topics(&topic_description()), expected, "the schema's topic list");
        assert_eq!(listed_topics(&sheet(&tools)), expected, "the sheet's own topic list");
        let schema = help_schema();
        let described = schema["properties"]["topic"]["description"].as_str().expect("topic description");
        assert!(described.contains("\"ops\""), "{described}");
        for t in TOPICS {
            assert!(section(&tools, Some(t)).is_ok(), "topic `{t}` is listed but does not answer");
        }
        for name in ["overlays", "vertical", "stretch", "unsupported"] {
            assert!(expected.iter().any(|t| t == name), "{name} is missing from TOPICS");
        }
    }

    /// `transform` documents `opacity` in its signature, as its help entry does.
    #[test]
    fn transform_signature_lists_opacity() {
        let ops = section(&catalogue(), Some("ops")).expect("ops topic");
        assert!(ops.lines().any(|l| l == "  transform {clip, x?, y?, scale?, rotate?, crop?:[l,r,t,b], opacity?, reset?}"), "{ops}");
    }

    /// The first_video recipe names the same lanes and sources at every step: the music is src:1
    /// (its second import), the narration is A3, and the captions read A3, not the picture's sound.
    #[test]
    fn first_video_recipe_is_self_consistent() {
        let text = section(&catalogue(), Some("first_video")).expect("first_video topic");
        assert!(text.contains(r#"{op:"append", source:"src:1", lane:"A2", until:"30s"}"#), "{text}");
        assert!(text.contains(r#"auto_captions {target:"A3"}"#), "{text}");
        assert!(!text.contains(r#"auto_captions {target:"A1"}"#), "{text}");
        assert!(text.contains(r#"voiceover {text:"...", pause:0.4, place:{lane:"A3", at:"0s"}}"#), "{text}");
        // Step 3 adds both sound lanes: one lane_add op per lane, so A2 (music) and then A3 (narration).
        assert!(text.contains(r#"{op:"lane_add", kind:"audio"}, {op:"lane_add", kind:"audio"}]} adds A2, then A3"#), "{text}");
    }

    /// The speed recipe leaves the music bed's lane out of its lanes, since a bed under the whole
    /// picture spans the change and a listed lane refuses such a clip.
    #[test]
    fn speed_recipe_leaves_the_music_bed_out_of_lanes() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains(r#"lanes:["V1","A1","A3"]"#), "{text}");
        assert!(!text.contains(r#"lanes:["V1","A1","A2"]"#), "{text}");
        assert!(text.contains("spans the change"), "{text}");
    }

    /// The dip recipe says its span must sit inside one clip, and that a short file loops into several.
    #[test]
    fn gain_dip_recipe_names_the_one_clip_span() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains("The span must lie inside that one clip"), "{text}");
        assert!(text.contains("a dip cannot cross them"), "{text}");
    }

    /// Dated autosave copies are capped, not kept forever: the project topic says which are kept.
    #[test]
    fn project_topic_says_which_dated_copies_are_kept() {
        let text = section(&catalogue(), Some("project")).expect("project topic");
        assert!(!text.contains("never deleted"), "{text}");
        assert!(!text.contains("kept, not deleted"), "{text}");
        assert!(!text.contains("only the newest three dated copies"), "{text}");
        assert!(text.contains("the newest three dated copies and any from the last 7 days are kept"), "{text}");
        assert!(text.contains("setting one aside deletes the older ones"), "{text}");
    }

    /// project_save takes no overwrite key: project_new's overwrite is what lets its save replace a file.
    #[test]
    fn project_topic_names_what_lets_a_save_replace_a_file() {
        let text = section(&catalogue(), Some("project")).expect("project topic");
        assert!(text.contains("overwrite:true lets the later project_save replace a file already at path"), "{text}");
        assert!(text.contains("project_save takes no overwrite key"), "{text}");
        assert!(text.contains("force also replaces a file at path"), "{text}");
    }

    #[test]
    fn swap_documents_ripple_and_the_take_refusal() {
        let tools = catalogue();
        let text = section(&tools, Some("swap")).expect("swap topic");
        assert!(text.contains("ripple:true (the default)"), "{text}");
        assert!(text.contains("ripple:false"), "{text}");
        assert!(text.contains("is refused"), "{text}");
        assert!(text.contains("ungrouped first"), "{text}");
        // swap refuses a take rather than moving it, so it is tagged [clip], not [group].
        assert!(text.starts_with("edit op `swap`") && text.contains("swap {a, b, ripple?=true} [clip]"), "{text}");
        let ops = section(&tools, Some("ops")).expect("ops topic");
        assert!(ops.lines().any(|l| l == "  swap {a, b, ripple?=true} [clip]"), "{ops}");
    }

    #[test]
    fn overlay_corner_values_are_the_verified_ones() {
        let text = section(&catalogue(), Some("overlays")).expect("overlays topic");
        assert!(text.contains("corner:\"bottom-right\", scale:0.3"), "{text}");
        assert!(!text.contains("x:0.6, y:-0.6"), "{text}");
    }

    /// The corner form is one step, and the overlays topic still says x and y work.
    #[test]
    fn overlays_give_the_corner_form_and_keep_x_and_y() {
        let text = section(&catalogue(), Some("overlays")).expect("overlays topic");
        assert!(text.contains("corner:\"top-right\"") || text.contains("corner is centre, top-left"), "{text}");
        assert!(text.contains("Giving x or y with corner is refused"), "{text}");
        assert!(text.contains("x and y still work"), "{text}");
        assert!(!text.contains("half-height = half-width"), "the hand-math recipe is gone:\n{text}");
    }

    /// The recipes and the topics that carry them write edit ops as `edit {ops:[{op:…}]}`, never as bare calls.
    #[test]
    fn recipe_topics_write_edit_ops_as_edit_calls() {
        const OPS: [&str; 16] = [
            "place", "append", "trim", "speed", "duck", "insert_time", "title_add", "title_set", "title_remove",
            "copy_look", "assemble", "transform", "fit", "lane_add", "keep", "gain_dip",
        ];
        let tools = catalogue();
        for topic in ["first_video", "podcast", "recipes", "overlays", "vertical", "stretch"] {
            let text = section(&tools, Some(topic)).unwrap_or_else(|e| panic!("{topic}: {e}"));
            for op in OPS {
                assert!(!text.contains(&format!("{op} {{")), "{topic} writes `{op}` as a bare call:\n{text}");
            }
        }
    }

    /// Each example that needs earlier steps or clips says so beside it.
    #[test]
    fn examples_state_their_prerequisites() {
        let tools = catalogue();
        let recipes = section(&tools, Some("recipes")).unwrap();
        assert!(recipes.contains("both must already exist"), "{recipes}");
        assert!(recipes.contains("Prerequisite: A2@0 is the music clip and runs to at least 25 s."), "{recipes}");
        assert!(recipes.contains("prerequisite: A3 exists and holds the narration clip"), "{recipes}");
        let stretch = section(&tools, Some("stretch")).unwrap();
        assert!(stretch.contains("a 5 s shot fits slots from 1.25 s to 20 s"), "{stretch}");
        let overlays = section(&tools, Some("overlays")).unwrap();
        assert!(overlays.contains("V2@30 is the still placed at 1s"), "{overlays}");
    }

    /// Non-Latin scripts draw as boxes in titles and burned captions, so the topics that place them point at soft subtitles.
    #[test]
    fn title_and_caption_topics_point_non_latin_text_at_soft_subtitles() {
        let tools = catalogue();
        for topic in ["vertical", "burn_captions"] {
            let text = section(&tools, Some(topic)).unwrap_or_else(|e| panic!("{topic}: {e}"));
            assert!(text.contains("CJK") && text.contains("empty boxes") && text.contains("soft subtitles"), "{topic}:\n{text}");
        }
        let titles = section(&tools, Some("recipes")).unwrap();
        assert!(titles.contains("T2 becomes T1"), "{titles}");
    }

    #[test]
    fn lane_move_says_to_is_a_position_and_names_renumber() {
        let lanes = section(&catalogue(), Some("lanes")).expect("lanes topic");
        assert!(lanes.contains("to is the 1-based position"), "{lanes}");
        assert!(lanes.contains("renumbers the lanes between its two places"), "{lanes}");
        assert!(lanes.contains("lane_move {lane:\"V2\", to:1}"), "{lanes}");
    }

    #[test]
    fn vertical_checks_the_file_with_media_probe() {
        let text = section(&catalogue(), Some("vertical")).expect("vertical topic");
        assert!(text.contains("Check the file with media_probe."), "{text}");
        assert!(!text.contains("with probe"), "{text}");
    }

    #[test]
    fn unsupported_names_cjk_and_the_open_gaps() {
        let text = section(&catalogue(), Some("unsupported")).expect("unsupported topic");
        assert!(text.contains("CJK text in the picture") && text.contains("Noto Sans CJK"), "{text}");
        assert!(text.contains("empty boxes in a title or in burned captions"), "{text}");
        assert!(text.contains("Soft subtitle tracks") && text.contains("mp4 or mkv"), "{text}");
        // Scripts the bundled faces draw are no longer listed as unsupported.
        assert!(!text.contains("Cyrillic"), "{text}");
        assert!(!text.contains("Speed ramps"), "speed ramps are supported now: {text}");
        assert!(!text.contains("Keyframes on colour"), "colour keys are supported now: {text}");
        let tools = catalogue();
        assert!(section(&tools, Some("color_keys")).expect("color_keys topic").contains("ease"));
        assert!(section(&tools, Some("opacity_keys")).expect("opacity_keys topic").contains("fades multiply"));
        for gap in ["10-bit: AV1 and HEVC only", "HEVC in 4:4:4", "AV1 in 4:4:0", "12-bit"] {
            assert!(text.contains(gap), "{gap} missing:\n{text}");
        }
        assert!(!text.contains("Chapter markers in the exported file"), "chapters are written now: {text}");
    }

    #[test]
    fn normalize_puts_the_boost_past_plus_twelve_on_the_clips() {
        let text = section(&catalogue(), Some("podcast")).expect("podcast topic");
        assert!(text.contains("puts the rest on that lane's clips (each up to +24 dB"), "{text}");
        assert!(text.contains("capped_by: clip_gain_limit"), "{text}");
        assert!(!text.contains("capped_by: lane_gain_limit"), "{text}");
    }

    #[test]
    fn find_words_cut_snaps_to_a_pause_and_names_the_switch() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains("snaps to a pause within 150 ms") && text.contains("snap:false"), "{text}");
    }

    #[test]
    fn hint_corrections_are_named_in_the_help() {
        let text = section(&catalogue(), Some("captions")).expect("captions topic");
        assert!(text.contains("near-miss") && text.contains("hint corrected"), "{text}");
    }

    #[test]
    fn visualizer_and_speed_and_export_say_what_the_code_does() {
        let vis = section(&catalogue(), Some("visualizer")).unwrap();
        assert!(vis.contains("new top picture lane") && vis.contains("covers the picture under it"), "{vis}");
        assert!(vis.contains("transform") && vis.contains("V2@0"), "{vis}");
        let speed = section(&catalogue(), Some("speed")).unwrap();
        assert!(speed.contains("absolute") && speed.contains("not a multiplier"), "{speed}");
        let export = section(&catalogue(), Some("export")).unwrap();
        assert!(export.contains("placed or not") && export.contains("written whole"), "{export}");
        assert!(export.contains("mp4, overlapping cues are merged") && export.contains("mkv keeps them"), "{export}");
        let subs = section(&catalogue(), Some("subtitles")).unwrap();
        assert!(subs.contains("edge:\"in\"|\"out\""), "{subs}");
    }

    #[test]
    fn podcast_warns_about_the_peak_and_the_one_lane_ripple() {
        let podcast = section(&catalogue(), Some("podcast")).unwrap();
        assert!(podcast.contains("holds that lane to the true-peak ceiling"), "{podcast}");
        assert!(podcast.contains("structuredContent carries it as capped_by: true_peak"), "{podcast}");
        assert!(podcast.contains("cut:\"lift\"") && podcast.contains("falls out of step"), "{podcast}");
        assert!(podcast.contains("normalize {target:\"mix\", lufs:-16, allow_limiter:true, true_peak:-1}"), "{podcast}");
        assert!(podcast.contains("true_peak:-2 for m4a or mp3"), "{podcast}");
        assert!(podcast.contains("trim it to the new end with edit {ops:[{op:\"trim\", clip:\"A3@0\", edge:\"out\""), "{podcast}");
        assert!(podcast.contains("hint:\"Jane Doe, LUFS\"") && podcast.contains("recorded on its own track"), "{podcast}");
    }

    #[test]
    fn move_names_split_captions() {
        let text = section(&catalogue(), Some("move")).expect("move topic");
        assert!(text.contains("split_captions?"), "{text}");
        assert!(text.contains("unless split_captions:true"), "{text}");
    }

    #[test]
    fn find_words_has_no_fillers_flag_and_a_plain_filler_query_keeps_them() {
        let recipes = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(!recipes.contains("fillers:true"), "{recipes}");
        assert!(recipes.contains("find_words {query:\"um\"}"), "{recipes}");
        assert!(recipes.contains("with no flag"), "{recipes}");
    }

    #[test]
    fn recipes_write_lane_and_append_as_edit_ops() {
        let recipes = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(!recipes.contains("lane_add {kind"), "{recipes}");
        assert!(recipes.contains("edit {ops:[{op:\"lane_add\", kind:\"audio\"}]}"), "{recipes}");
        assert!(recipes.contains("edit {ops:[{op:\"append\", source:\"src:1\", lane:\"A2\"}]}"), "{recipes}");
    }

    #[test]
    fn cut_silence_states_its_default_min_silence() {
        let recipes = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(recipes.contains("min_silence (default 0.5 s, judged before padding)"), "{recipes}");
        assert!(recipes.contains("It cuts only pauses of min_silence or longer"), "{recipes}");
        assert!(recipes.contains("the reply names the longest quiet it found and cuts nothing"), "{recipes}");
    }

    /// The copy_look example needs a source clip with a look; a plain clip replies "nothing to copy".
    #[test]
    fn copy_look_example_says_its_source_needs_a_look() {
        let recipes = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(recipes.contains("from must carry a look: a plain clip has none"), "{recipes}");
        assert!(recipes.contains("a colour grade from the color op, or a transform"), "{recipes}");
    }

    #[test]
    fn podcast_adds_speaker_lanes_as_edit_ops_and_explains_the_joint_cut() {
        let text = section(&catalogue(), Some("podcast")).expect("podcast topic");
        assert!(text.contains("A2 does not exist yet, so add it first: edit {ops:[{op:\"lane_add\", kind:\"audio\"}]}"), "{text}");
        assert!(!text.contains("lane_add {kind"), "{text}");
        assert!(!text.contains("append {source"), "{text}");
        assert!(text.contains("edit {ops:[{op:\"append\", source:\"src:2\", lane:\"A3\""), "{text}");
        assert!(text.contains("a lane with no sound there (a gap between its clips, or no clip) counts as quiet"), "{text}");
        assert!(text.contains("the turns stay in step"), "{text}");
        assert!(text.contains("hint:\"Jane Doe, LUFS\""), "{text}");
    }

    /// The speed recipe's example and its text agree: the default leaves narration in place, and
    /// naming its lane is the example that moves it.
    #[test]
    fn speed_recipe_example_and_text_agree_on_the_narration_lane() {
        let text = section(&catalogue(), Some("recipes")).expect("recipes topic");
        assert!(text.contains("edit {ops:[{op:\"speed\", clip:\"V1@0\", x:0.5, ripple:true}]} changes the picture's length and the narration keeps its times"), "{text}");
        assert!(text.contains("lanes:[\"V1\",\"A1\",\"A3\"]"), "{text}");
        assert!(text.contains("spans the change"), "{text}");
        assert!(text.contains("lanes:[\"V1\",\"A1\"]"), "{text}");
    }

    #[test]
    fn vertical_says_how_the_fit_policies_frame_a_16_9_clip() {
        let text = section(&catalogue(), Some("vertical")).expect("vertical topic");
        assert!(text.contains("fit (the default)"), "{text}");
        assert!(text.contains("fill:"), "{text}");
        assert!(text.contains("bars"), "{text}");
    }

    #[test]
    fn subtitles_say_skipped_cues_include_wrong_arrows_and_missing_timing() {
        let text = section(&catalogue(), Some("subtitles")).expect("subtitles topic");
        assert!(text.contains("a wrong arrow (`->`) or a cue with no timing line is skipped"), "{text}");
    }

    /// subtitles_import takes an optional language, guessed from the file name when left out; unset is und.
    #[test]
    fn subtitles_topic_says_how_a_track_gets_its_language() {
        let text = section(&catalogue(), Some("subtitles")).expect("subtitles topic");
        assert!(text.contains("subtitles_import {path, lang?} takes an optional language"), "{text}");
        assert!(text.contains("talk.fr.srt, fra.srt, talk_en.srt, eng.srt"), "{text}");
        assert!(text.contains("written as und"), "{text}");
        assert!(text.contains("subtitles_lang {track, lang}"), "{text}");
    }

    #[test]
    fn append_until_says_it_loops_a_short_source() {
        let text = section(&catalogue(), Some("first_video")).expect("first_video topic");
        assert!(text.contains("until LOOPS"), "{text}");
    }

    /// The argument row a tool's help gives for `name`: its line under ARGUMENTS.
    fn arg_row(tool: &str, name: &str) -> String {
        let tools = catalogue();
        let text = section(&tools, Some(tool)).unwrap_or_else(|e| panic!("{tool}: {e}"));
        let prefix = format!("  {name} ");
        text.lines().find(|l| l.starts_with(&prefix)).unwrap_or_else(|| panic!("{tool} has no `{name}` row:\n{text}")).to_owned()
    }

    #[test]
    fn media_import_help_says_paths_is_a_list_of_paths_with_an_example() {
        let row = arg_row("media_import", "paths");
        assert!(row.contains("array|string") && row.contains("(e.g. [\"clip.mp4\"])"), "{row}");
        assert!(row.contains("a list"), "{row}");
    }

    #[test]
    fn levels_help_says_per_sec_is_a_number_with_an_example() {
        let row = arg_row("levels", "per_sec");
        assert!(row.contains("number") && row.contains("(e.g. 10)"), "{row}");
        assert!(row.contains("Buckets per second: a number"), "{row}");
    }

    #[test]
    fn example_follows_the_schema_type() {
        assert_eq!(example("x", &json!({"type": "boolean"})), "true");
        assert_eq!(example("x", &json!({"type": "integer", "default": 5})), "5");
        assert_eq!(example("x", &json!({"type": "array", "items": {"type": "integer"}})), "[1]");
        assert_eq!(example("x", &json!({"type": "string", "enum": ["mp4", "mov"]})), "\"mp4\"");
        assert_eq!(example("x", &json!({"examples": ["A1@0"], "type": "string"})), "\"A1@0\"");
        assert_eq!(example("paths", &json!({"type": ["array", "string"], "items": {"type": "string"}})), "[\"clip.mp4\"]");
        assert_eq!(example("x", &json!({"type": ["array", "string"], "items": {"type": "string"}})), "[\"text\"]");
        assert_eq!(example("out_path", &json!({"type": "string"})), "\"cut.mp4\"");
        // anyOf and oneOf with no type of their own: the first branch's example, and the union of types.
        assert_eq!(example("x", &json!({"anyOf": [{"type": "boolean"}, {"type": "array", "items": {"type": "integer"}}]})), "true");
        assert_eq!(example("x", &json!({"oneOf": [{"type": "string", "enum": ["all", "first"]}]})), "\"all\"");
        assert_eq!(schema_types(&json!({"oneOf": [{"type": "string"}, {"type": "array"}, {"type": "string"}]})), ["string", "array"]);
        assert_eq!(example("range", &json!({"type": "object", "properties": {"from": {}, "to": {}}})), "{\"from\": \"0s\", \"to\": \"5s\"}");
    }

    /// A number's example is the schema's minimum when it has one, so burn_captions size shows 10, not 1.
    #[test]
    fn numeric_examples_use_the_schema_minimum() {
        assert_eq!(example("size", &json!({"type": "integer", "minimum": 10, "maximum": 200})), "10");
        assert_eq!(example("max_secs", &json!({"type": "number", "minimum": 0.5})), "0.5");
        assert_eq!(example("bitrate_kbps", &json!({"type": "integer", "minimum": 1})), "8000");
        assert_eq!(example("x", &json!({"type": "integer"})), "1");
    }

    /// Each name a tool's argument takes gets an example of its own kind, not `"text"`.
    #[test]
    fn named_arguments_get_examples_of_their_kind() {
        let string = |desc: &str| json!({"type": "string", "description": desc});
        let list = json!({"type": "array", "items": {"type": "string"}});
        assert_eq!(example("lanes", &list), "[\"V1\",\"A1\"]");
        assert_eq!(example("lane", &string("A caption lane.")), "\"A1\"");
        assert_eq!(example("glob", &string("Files to match.")), "\"*.mp4\"");
        assert_eq!(example("out", &string("The file to write.")), "\"cut.mp4\"");
        assert_eq!(example("clip", &string("A clip address.")), "\"V1@0\"");
        assert_eq!(example("source", &string("A source.")), "\"src:0\"");
        assert_eq!(example("at", &string("Timeline time, such as 5s or 150f.")), "\"2s\"");
        assert_eq!(example("time", &string("A time.")), "\"2s\"");
        assert_eq!(example("until", &string("Up to this time.")), "\"2s\"");
        assert_eq!(example("path", &string("Where the .edith file will be saved.")), "\"talk.edith\"");
        assert_eq!(example("path", &string("Where to write the .srt.")), "\"talk.srt\"");
        assert_eq!(example("path", &string("The file the source now is.")), "\"clip.mp4\"");
        let range = json!({"type": "object", "properties": {"from": {"description": "Start."}, "to": {"description": "End."}}});
        assert_eq!(example("range", &range), "{\"from\": \"0s\", \"to\": \"5s\"}");
        let ops = json!({"type": "array", "items": {"type": "object", "properties": {"op": {"type": "string", "enum": ["keep", "place"]}}}});
        assert_eq!(example("ops", &ops), "[{\"op\": \"place\", \"source\": \"src:0\", \"at\": \"0s\"}]");
        // A list of items takes the item's example, so a list of clip addresses reads as one.
        assert_eq!(example("clip", &json!({"type": "array", "items": {"type": "string"}})), "[\"V1@0\"]");
    }

    /// The generated help shows the new examples for real arguments, not the placeholder ones.
    #[test]
    fn generated_help_shows_real_examples_for_real_arguments() {
        let tools = catalogue();
        let row = |tool: &str, name: &str| {
            let text = section(&tools, Some(tool)).unwrap_or_else(|e| panic!("{tool}: {e}"));
            text.lines().find(|l| l.starts_with(&format!("  {name} "))).unwrap_or_else(|| panic!("{tool} has no `{name}` row:\n{text}")).to_owned()
        };
        assert!(row("edit", "ops").contains("(e.g. [{\"op\": \"place\", \"source\": \"src:0\", \"at\": \"0s\"}])"), "{}", row("edit", "ops"));
        let all = tools.iter().map(|t| section(&tools, Some(t.name)).unwrap_or_default()).collect::<Vec<_>>().join("\n");
        let placeholders = ["(e.g. [\"text\"])", "(e.g. \"text\")", "{\"from\": \"text\", \"to\": \"text\"}", "(e.g. [{\"op\": \"place\"}])"];
        let shown: Vec<&str> = all.lines().filter(|l| placeholders.iter().any(|p| l.contains(p))).collect();
        assert!(shown.is_empty(), "generated help rows still show a placeholder example:\n{}", shown.join("\n"));
    }

    /// Every argument of every tool has a type and an example in its help, so no row is left
    /// with a bare name. A refusal that names the expected type can reuse the same example.
    #[test]
    fn every_tool_argument_has_a_type_and_an_example() {
        let mut gaps = Vec::new();
        for tool in catalogue() {
            let text = section(&catalogue(), Some(tool.name)).expect("tool topic");
            let schema = (tool.schema)();
            let Some(props) = schema.get("properties").and_then(Value::as_object) else { continue };
            for (name, prop) in props {
                let row = text.lines().find(|l| l.starts_with(&format!("  {name} "))).unwrap_or("");
                let typed = !schema_types(prop).is_empty();
                if !typed || !row.contains("(e.g. ") || row.contains("(e.g. …)") {
                    gaps.push(format!("{}.{name}", tool.name));
                }
            }
        }
        assert!(gaps.is_empty(), "arguments without a type or an example: {gaps:?}");
    }

    /// The cut tools and the caption recipes say captions follow the cut, and locked caption lanes stay.
    #[test]
    fn cuts_and_caption_topics_say_caption_lanes_ripple_with_the_cut() {
        const PHRASE: &str = "caption lanes ripple with the cut; locked caption lanes stay";
        let tools = catalogue();
        for topic in ["recipes", "captions", "podcast"] {
            let text = section(&tools, Some(topic)).unwrap_or_else(|e| panic!("{topic}: {e}"));
            assert!(text.to_lowercase().contains(PHRASE), "{topic} lacks the caption ripple rule:\n{text}");
        }
        let recipes = section(&tools, Some("recipes")).unwrap();
        for tool in ["cut_silence:", "find_words"] {
            let line = recipes.lines().find(|l| l.contains(tool)).unwrap_or_else(|| panic!("no {tool} line"));
            assert!(line.contains("ripple") || recipes.to_lowercase().contains(PHRASE), "{tool}: {line}");
        }
    }
}
