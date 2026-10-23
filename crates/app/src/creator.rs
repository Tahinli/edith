//! The creator tools the MCP server had first and the window now shares: loudness
//! normalizing, ducking, captions from speech, filler removal and a voiceover from text.
//!
//! Everything here takes the session and says what happened; none of it touches the
//! window, so the same calls are what the tests drive. The work is the engine's
//! ([`engine::normalize`], [`engine::speech`], [`engine::tts`], [`engine::duck`]) -- the
//! MCP tools call those very functions, so a mix normalized from either door lands on the
//! same numbers.

use std::path::{Path, PathBuf};

use engine::PlaybackSession;
use engine::duck::{self, Duck};
use engine::normalize::{self, Report, Scope};
use engine::project::{Lane, LaneKind};
use engine::speech::{self, Filler, Hear, Spoken};

/// The loudness targets the Normalize card offers, in LUFS: online video, streaming speech
/// and broadcast (EBU R128). The first is the default.
pub(crate) const LUFS_PRESETS: [(&str, f64); 3] = normalize::PRESETS;

/// How far the target and the ceiling move per press, in LU and dB.
pub(crate) const LUFS_STEP: f64 = 1.0;
pub(crate) const PEAK_STEP: f64 = 0.5;
/// The target's and the ceiling's ranges, as the engine takes them.
pub(crate) const LUFS_RANGE: (f64, f64) = (-40.0, -5.0);
pub(crate) const PEAK_RANGE: (f64, f64) = (-9.0, 0.0);

/// The languages the captions and fillers cards offer; `auto` lets Whisper decide.
pub(crate) const LANGUAGES: [(&str, &str); 12] = [
    ("auto", "Auto"),
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("tr", "Turkish"),
    ("ru", "Russian"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
];

/// How the cards name a speech model: by what it is for. `tiny` is the quick one, `base` the
/// middle, `small` the careful one; a model with no such name keeps its own, capitalised.
pub(crate) fn model_label(model: &str) -> String {
    match model {
        "tiny" => "Fast".to_string(),
        "base" => "Balanced".to_string(),
        "small" => "Accurate".to_string(),
        other => {
            let mut c = other.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
    }
}

/// What a model's tooltip says: what it trades off, and its name on disk so the plain word is
/// never a mystery.
pub(crate) fn model_tip(model: &str) -> String {
    let what = match model {
        "tiny" => "quickest, least accurate",
        "base" => "a middle ground",
        "small" => "slowest, most accurate",
        _ => "a speech model",
    };
    format!("{}: {what} ({model} model)", model_label(model))
}

// ---- normalize ----------------------------------------------------------------------

/// A minus sign the way the cards print it: U+2212, never a hyphen.
pub(crate) fn minus(s: String) -> String {
    s.replace('-', "\u{2212}")
}

/// A level for the card: one decimal, `silent` for no energy.
pub(crate) fn lufs_text(x: f64) -> String {
    if x.is_finite() {
        minus(format!("{x:.1}"))
    } else {
        "silent".to_string()
    }
}

/// `m:ss` for a time in seconds, or `h:mm:ss` past an hour: the plain clock the cards show.
pub(crate) fn clock(secs: f64) -> String {
    let whole = secs.max(0.0).round() as u64;
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    match h {
        0 => format!("{m}:{s:02}"),
        _ => format!("{h}:{m:02}:{s:02}"),
    }
}

/// What a tool says on its card and in the toast: one plain sentence, and for an error that was
/// cut short the whole text (the card's tooltip). `offline` marks the sentence a Relink answers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Said {
    pub(crate) line: String,
    pub(crate) detail: Option<String>,
    pub(crate) offline: bool,
}

impl Said {
    pub(crate) fn plain(line: impl Into<String>) -> Self {
        Said { line: line.into(), ..Said::default() }
    }
}

/// The number the engine gives when sources are offline: `7` in "... 7 source(s) offline ...".
fn offline_count(e: &str) -> Option<usize> {
    let at = e.find(" source(s) offline")?;
    e[..at].rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// An error in the cards' words. A refusal for offline sources becomes a count and a sentence the
/// Relink button answers; anything else keeps its first sentence, with the rest in the tooltip.
pub(crate) fn plain_error(e: &str) -> Said {
    if let Some(n) = offline_count(e) {
        let line = match n {
            1 => "1 file is offline. Relink it in Media.".to_string(),
            n => format!("{n} files are offline. Relink them in Media."),
        };
        return Said { line, detail: Some(e.to_string()), offline: true };
    }
    let first = e.split(". ").next().unwrap_or(e).split(';').next().unwrap_or(e).trim();
    let first = match first.chars().next() {
        Some(c) => c.to_uppercase().chain(first.chars().skip(1)).collect::<String>(),
        None => String::new(),
    };
    let detail = (first.len() != e.trim().len()).then(|| e.to_string());
    Said { line: first, detail, offline: false }
}

/// The audio tracks a normalize reaches: unmuted, with clips on them.
pub(crate) fn normalizable_lanes(session: &PlaybackSession) -> Vec<Lane> {
    normalize::mix_lanes(session)
}

/// Normalizes the whole mix, or each track on its own, to `lufs` with the true peak held
/// under `true_peak` dBTP. One undo step.
pub(crate) fn normalize(
    session: &mut PlaybackSession,
    each: bool,
    lufs: f64,
    true_peak: f64,
) -> Result<Report, String> {
    let scope = match each {
        true => Scope::Each(normalizable_lanes(session)),
        false => Scope::Mix,
    };
    normalize::normalize(session, &scope, lufs, true_peak)
}

/// Joins names as a sentence reads: `A1, A2 and A3`.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// What a normalize says on the card and in the toast: where the level landed, and what held it
/// short of the target if something did.
pub(crate) fn normalize_line(report: &Report, each: bool) -> String {
    let names: Vec<String> = report.lanes.iter().map(|l| l.lane.label()).collect();
    let (subject, verb) = match (each, names.as_slice()) {
        (true, lanes) if lanes.len() > 1 => (join_names(lanes), "are each"),
        (true, [one]) => (one.clone(), "is"),
        // The whole mix says which tracks it moved, so a one-track mix still names the track.
        (_, []) => ("The whole mix".to_string(), "is"),
        (_, lanes) => (format!("The whole mix ({})", join_names(lanes)), "is"),
    };
    let (before, after) = (report.before.integrated_lufs, report.after.integrated_lufs);
    let target = minus(format!("{:.0}", report.lufs));
    let head = match (before - after).abs() < 0.05 && before.is_finite() {
        true => format!("Already at {} LUFS.", lufs_text(after)),
        false => format!("{subject} {verb} now {} LUFS.", lufs_text(after)),
    };
    let tail = match (report.reached, report.peak_held) {
        (true, _) => String::new(),
        (false, true) => format!(
            " The {} dB peak limit stops it from getting louder, so it can't reach {target} LUFS.",
            lufs_text(report.true_peak)
        ),
        (false, false) => format!(" It is short of the {target} LUFS target."),
    };
    format!("{head}{tail}")
}

// ---- ducking ------------------------------------------------------------------------

/// The depth a new duck starts at, in dB: the MCP tool's default.
pub(crate) const DUCK_DEPTH_DEFAULT: f32 = -12.0;
/// One press of a duck stepper: a dB of depth, ten ms of attack, fifty of release.
pub(crate) const DUCK_DEPTH_STEP: f32 = 1.0;
pub(crate) const DUCK_ATTACK_STEP: f32 = 10.0;
pub(crate) const DUCK_RELEASE_STEP: f32 = 50.0;

/// Which of a duck's numbers a stepper moves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DuckField {
    Depth,
    Attack,
    Release,
}

/// Ducks `lane` under `key` (the voice), keeping the numbers a duck under that key already
/// has. A duck under a different key is added beside it, as the MCP `duck` op does.
pub(crate) fn duck_under(session: &mut PlaybackSession, lane: Lane, key: Lane) -> Result<(), String> {
    check_duck(session, lane, key)?;
    let duck = session
        .lane_duck_list(lane)
        .into_iter()
        .find(|d| d.under == key.ord)
        .unwrap_or_else(|| Duck::new(key.ord, DUCK_DEPTH_DEFAULT));
    set_duck(session, lane, duck)
}

/// Moves one number of the duck `lane` has under `key` by `steps` presses.
pub(crate) fn duck_step(
    session: &mut PlaybackSession,
    lane: Lane,
    key: Lane,
    field: DuckField,
    steps: i32,
) -> Result<(), String> {
    let mut duck = session
        .lane_duck_list(lane)
        .into_iter()
        .find(|d| d.under == key.ord)
        .ok_or_else(|| format!("{} is not ducked under {}", lane.label(), key.label()))?;
    let by = steps as f32;
    match field {
        DuckField::Depth => {
            duck.depth_db = (duck.depth_db + by * DUCK_DEPTH_STEP).clamp(duck::MIN_DEPTH_DB, duck::MAX_DEPTH_DB)
        }
        DuckField::Attack => {
            duck.attack_ms = (duck.attack_ms + by * DUCK_ATTACK_STEP).clamp(duck::MIN_TIME_MS, duck::MAX_ATTACK_MS)
        }
        DuckField::Release => {
            duck.release_ms =
                (duck.release_ms + by * DUCK_RELEASE_STEP).clamp(duck::MIN_TIME_MS, duck::MAX_RELEASE_MS)
        }
    }
    set_duck(session, lane, duck)
}

/// Takes the duck `lane` has under `key` off, keeping its others.
pub(crate) fn duck_off(session: &mut PlaybackSession, lane: Lane, key: Lane) -> Result<(), String> {
    if session.lane_locked(lane) {
        return Err(format!("{} is locked", lane.label()));
    }
    match session.clear_lane_duck(lane, key.ord) {
        true => Ok(()),
        false => Err(format!("{} is not ducked under {}", lane.label(), key.label())),
    }
}

fn check_duck(session: &PlaybackSession, lane: Lane, key: Lane) -> Result<(), String> {
    if lane.kind != LaneKind::Audio || key.kind != LaneKind::Audio {
        return Err("only an audio track ducks, and only under another audio track".into());
    }
    if lane == key {
        return Err(format!("{} cannot duck under itself", lane.label()));
    }
    if session.lane_locked(lane) {
        return Err(format!("{} is locked", lane.label()));
    }
    Ok(())
}

fn set_duck(session: &mut PlaybackSession, lane: Lane, duck: Duck) -> Result<(), String> {
    let duck = duck.clamped().ok_or("a duck takes finite numbers")?;
    // Already so: no second undo step for a press that changed nothing.
    if session.lane_duck_list(lane).contains(&duck) {
        return Ok(());
    }
    match session.set_lane_duck(lane, Some(duck)) {
        true => Ok(()),
        false => Err(format!("{} could not take that duck", lane.label())),
    }
}

// ---- captions and fillers -----------------------------------------------------------

/// What to listen to for captions and fillers: the whole mix, or one audio track.
pub(crate) fn hear_choices(session: &PlaybackSession) -> Vec<Hear> {
    std::iter::once(Hear::Mix)
        .chain(
            session
                .lanes()
                .into_iter()
                .filter(|l| l.kind == LaneKind::Audio && !session.lane_clips(*l).is_empty())
                .map(Hear::Lane),
        )
        .collect()
}

/// How the card names a [`Hear`].
pub(crate) fn hear_label(hear: Hear) -> String {
    match hear {
        Hear::Mix => "Whole mix".to_string(),
        Hear::Lane(l) => l.label(),
    }
}

/// The caption lane new captions go on: the first with nothing on it, else the next one
/// (made when they are placed).
pub(crate) fn caption_lane(session: &PlaybackSession) -> Lane {
    let lanes = session.subtitle_lanes();
    lanes
        .iter()
        .copied()
        .find(|l| session.sub_lane(*l).is_empty() && !session.lane_locked(*l))
        .unwrap_or_else(|| Lane::new(LaneKind::Subtitle, lanes.len()))
}

/// Where the caption file of a generated track is kept: beside the project when it has a
/// path, else in the editor's cache; never over a file that is there.
pub(crate) fn caption_file(project: Option<&Path>) -> PathBuf {
    let (dir, stem) = match project {
        Some(p) => (
            p.parent().map(Path::to_path_buf).unwrap_or_default(),
            p.file_stem().map_or_else(|| "project".to_string(), |s| s.to_string_lossy().into_owned()),
        ),
        None => (speech::home().join(".cache/edith/captions"), format!("captions-{}", std::process::id())),
    };
    let mut path = dir.join(format!("{stem}.captions.srt"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}.captions-{n}.srt"));
        n += 1;
    }
    path
}

/// Cuts `words` into caption cues and puts them on the next free caption lane, as one undo
/// step. Returns how many cues and the lane they are on.
/// Puts the transcript's captions on the caption lane, as one undo step. Captions made by an
/// earlier run are replaced in place (their lane is cleared first), so a second run never stacks
/// a second copy. Returns the cue count, the lane, and whether an earlier run was replaced.
pub(crate) fn place_captions(
    session: &mut PlaybackSession,
    words: &[Spoken],
    project: Option<&Path>,
) -> Result<(usize, Lane, bool), String> {
    let fps = session.meta().frame_rate;
    let cues = speech::captions(words, fps);
    if cues.is_empty() {
        return Err("no speech to caption: the transcript has no words".into());
    }
    let earlier = generated_caption_lane(session);
    if let Some(lane) = earlier
        && session.lane_locked(lane)
    {
        return Err(format!("{} is locked: unlock it to replace its captions", lane.label()));
    }
    let lane = earlier.unwrap_or_else(|| caption_lane(session));
    let pushed = session.history_pushes();
    // One batch: the clearing and the new track are a single undo step (the engine's own
    // batch nests inside this one).
    session.begin_batch();
    if earlier.is_some() {
        clear_generated_captions(session, lane);
    }
    let placed = speech::place_captions(session, &cues, lane, &caption_file(project));
    session.end_batch();
    if let Err(e) = placed {
        // A failed attempt leaves nothing behind: take back what it pushed.
        for _ in pushed..session.history_pushes() {
            if !session.undo() {
                break;
            }
        }
        session.clear_redo();
        return Err(e);
    }
    Ok((cues.len(), lane, earlier.is_some()))
}

/// Whether a subtitle track is a caption track this editor generated (its file is named for
/// the project and `.captions`, see [`caption_file`]).
fn is_generated_caption(track: &engine::subtitle::SubtitleTrack) -> bool {
    track.path.file_name().is_some_and(|n| n.to_string_lossy().contains(".captions"))
}

/// The first subtitle lane that holds generated captions, if any.
fn generated_caption_lane(session: &PlaybackSession) -> Option<Lane> {
    let tracks = session.subtitles();
    session
        .subtitle_lanes()
        .into_iter()
        .find(|&l| session.sub_lane(l).iter().any(|c| tracks.get(c.track).is_some_and(is_generated_caption)))
}

/// Takes every caption off `lane` and drops the generated tracks they came from, when nothing
/// else places them. Inside the caller's batch.
fn clear_generated_captions(session: &mut PlaybackSession, lane: Lane) {
    let mut tracks: Vec<usize> = session.sub_lane(lane).iter().map(|c| c.track).collect();
    while !session.sub_lane(lane).is_empty() {
        if !session.delete_sub(lane, 0) {
            break;
        }
    }
    tracks.sort_unstable();
    tracks.dedup();
    // Highest first, so each removal leaves the lower indices where they are.
    for t in tracks.into_iter().rev() {
        if session.subtitles().get(t).is_some_and(is_generated_caption) {
            let _ = session.remove_subtitles(t);
        }
    }
}

/// The fillers among `words`, with the padding a cut takes either side (a few frames, never
/// into the next word).
pub(crate) fn fillers_in(session: &PlaybackSession, words: &[Spoken]) -> Vec<Filler> {
    let fps = session.meta().frame_rate;
    let end = (session.timeline_duration() * fps).round() as u32;
    let pad = (0.06 * fps).round() as u32;
    speech::find_fillers(words, pad, end)
}

/// Cuts the picked fillers out of every track, closing each hole, as one undo step.
pub(crate) fn cut_fillers(session: &mut PlaybackSession, fillers: &[Filler]) -> Result<usize, String> {
    cut_fillers_regions(session, fillers).map(|_| fillers.len())
}

/// [`cut_fillers`], returning the regions cut (`(start, length)` frames) so a transcript can
/// follow the edit.
pub(crate) fn cut_fillers_regions(session: &mut PlaybackSession, fillers: &[Filler]) -> Result<Vec<(u32, u32)>, String> {
    let lanes = session.lanes();
    speech::cut_fillers(session, fillers, &lanes)
}

// ---- voiceover ----------------------------------------------------------------------

/// The folder a voiceover is written to: `voiceover/` beside the project, or the editor's
/// cache when the project has no path yet.
pub(crate) fn voiceover_dir(project: Option<&Path>) -> PathBuf {
    match project.and_then(Path::parent) {
        Some(dir) => dir.join("voiceover"),
        None => speech::home().join(".cache/edith/voiceover"),
    }
}

/// Takes the voiceover `file` in as a source and puts it at `at_secs` on the first audio
/// track with room for it (a new track when none has), as one undo step. Returns the lane.
pub(crate) fn place_voiceover(session: &mut PlaybackSession, file: &Path, at_secs: f64) -> Result<Lane, String> {
    let secs = engine::probe_media(file).duration_secs.unwrap_or(0.0);
    let fps = session.meta().frame_rate;
    let (from, to) = ((at_secs * fps).round() as u32, ((at_secs + secs) * fps).ceil() as u32);
    let free = |l: &Lane| {
        l.kind == LaneKind::Audio
            && !session.lane_locked(*l)
            && session.lane_clips(*l).iter().all(|c| c.end() <= from || c.start >= to)
    };
    let found = session.lanes().into_iter().find(free);
    session.begin_batch();
    let placed = (|| {
        let lane = match found {
            Some(l) => l,
            None => session.add_lane(LaneKind::Audio),
        };
        session.import(file).map_err(|e| format!("{}: refused: {e}", file.display()))?;
        match session.place_stream_at(at_secs, file, 0, Some(lane)) {
            Ok(true) => Ok(lane),
            Ok(false) => Err(format!("the voiceover did not place at {at_secs:.2} s")),
            Err(e) => Err(e.to_string()),
        }
    })();
    session.end_batch();
    placed
}
