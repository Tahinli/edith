//! Subtitles: what is parsed, what is listed and what is drawn.

use crate::*;

/// One press of the size stepper, and how far it may be pushed: below
/// [`SUB_SIZE_RANGE`].0 a plate is unreadable, above its top it eats the
/// smallest picture the layout floor promises ([`the_subtitle_plate_and_lanes_fit_the_smallest_window`]).
pub(crate) const SUB_SIZE_STEP: f32 = 1.;
pub(crate) const SUB_SIZE_RANGE: (f32, f32) = (10., 28.);

/// The cue's line height at `size`, on the ratio the defaults draw at
/// ([`SUB_LINE_H`] over [`SUB_TEXT`]): a bigger cue wants a taller line, not
/// just bigger letters on the old one.
pub(crate) fn sub_line_h_for(size: f32) -> f32 {
    size * SUB_LINE_H / SUB_TEXT
}

/// Where the subtitle style survives: beside the keybindings and the theme,
/// same corner-cut persistence -- a torn write costs the style and nothing
/// else, and [`load_subtitle_style`] falls back to the defaults on one.
pub(crate) fn subtitle_style_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("subtitle-style")
}

/// Two lines: the family (empty for the system default) and the size.
/// Anything unreadable, missing or out of range leaves the defaults in
/// force -- a subtitle style file is not the user's work, so a bad one is
/// worth no message at startup, exactly as [`ui::theme::load`].
pub(crate) fn load_subtitle_style() -> (Option<String>, f32) {
    let Ok(text) = std::fs::read_to_string(subtitle_style_path()) else {
        return (None, SUB_TEXT);
    };
    let mut lines = text.lines();
    let family = lines.next().filter(|l| !l.is_empty()).map(str::to_string);
    // A size this format could not have written means the file is not this
    // format at all -- the family line is then noise too, not a font.
    let Some(size) = lines
        .next()
        .and_then(|l| l.trim().parse::<f32>().ok())
        .filter(|s| (SUB_SIZE_RANGE.0..=SUB_SIZE_RANGE.1).contains(s))
    else {
        return (None, SUB_TEXT);
    };
    (family, size)
}

/// Writes the style whole, the way [`ui::theme::save`] writes the palette.
pub(crate) fn save_subtitle_style(family: Option<&str>, size: f32) -> std::io::Result<()> {
    let path = subtitle_style_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{}\n{size}\n", family.unwrap_or("")))
}

/// Whether a dropped or named path is a subtitle file rather than media: the
/// formats `engine::subtitle` parses, lowercased, for [`engine::is_audio`]'s
/// reason -- the import door has to know which of the engine's two doors a file
/// goes through before anything is opened.
///
/// `.mks` is one of them, and it is the only Matroska extension that is: it is
/// the *subtitles alone*, so there is no source in it to import and a drop of
/// one used to be refused for having no video track -- while `+ S` on the same
/// bytes took it ([`PlaybackSession::parse_subtitles`] reads it as Matroska).
/// The other two are media and stay media: `.mka` is the sound alone, which
/// [`engine::is_audio`] already imports as a song, and `.mk3d` is a film. Both
/// may *carry* subtitles, which is [`carries_subtitles`] and not this.
pub(crate) fn is_subtitle(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "srt" | "vtt" | "webvtt" | "ass" | "ssa" | "mks"
        )
    })
}

/// Whether a media path is a container that can carry subtitle tracks *inside*
/// it -- every Matroska extension and the three ISO-BMFF ones, which is the list
/// [`PlaybackSession::parse_subtitles`] walks (Matroska blocks and mp4 `tx3g`
/// alike). Named for what it gates and not for one of the two families, because
/// an mp4 answers `true` here now.
///
/// Matroska's set is the standard's own and closed by it
/// (`engine::demux::is_matroska`), so it is copied whole rather than trimmed to
/// the two that carry a film: a `.mk3d` opened as media has its tracks walked
/// like the `.mkv` it is, and a `.mka` song can hold a lyric track. A suffix
/// wider than the engine's would be a file taken here and refused deeper down.
pub(crate) fn carries_subtitles(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mkv" | "mka" | "mks" | "mk3d" | "webm" | "mp4" | "m4v" | "mov"
        )
    })
}

/// What a worker hands back, which is the fork [`arrival`] made when it was
/// started. An import is *read* and thrown away; the file argv named is opened
/// outright, because nothing else is going to open it afterwards.
pub(crate) enum Landed {
    /// An import into a timeline that is up: the container probed and the
    /// subtitle tracks walked, both *kept* -- they are the expensive halves and
    /// the worker is where they belong, so [`Player::take_import`] is left with
    /// two pushes ([`engine::PlaybackSession::import_probed`],
    /// [`subtitle_tail`]).
    Read(Subs, Probe),
    /// A whole timeline the worker opened, with the tail its subtitle tracks
    /// earn, ready to be hung off the window -- or the engine's refusal. `true`
    /// is the media argv named, which *becomes* the timeline; `false` is a file
    /// arriving at a window that had none, which fills the library and leaves
    /// the lanes empty for a drag ([`Player::install_media`]).
    Media(Result<(PlaybackSession, String), String>, bool),
    /// A `.edith`, restored: argv's, and the one a drop or the Import button
    /// brought ([`arrival`]).
    Project(Result<PlaybackSession, String>),
}

/// What the container walk found, for [`Player::take_import`] to register, or
/// the engine's refusal in the words it would have used on the UI thread.
///
/// `None` for the doors that never reach a demuxer: a song, a still and a
/// subtitle file, whose own small reads stay where they were
/// ([`engine::PlaybackSession::import`]).
pub(crate) type Probe = Option<engine::Result<engine::ImportProbe>>;

/// A media file opened as a session, with the subtitle tracks it carries inside
/// it taken in the same breath -- the two reads a file costs, in the one place
/// both doors ([`Player::open_media`] and the worker below) go through.
///
/// `place` is which door: a file *opened* is the timeline, one *imported* into
/// an empty window fills the library and leaves the lanes empty for a drag.
pub(crate) fn open_session(
    path: &std::path::Path,
    place: bool,
    subs: Subs,
) -> Result<(PlaybackSession, String), String> {
    let opened = match place {
        true => PlaybackSession::open(path),
        false => PlaybackSession::open_library(path),
    };
    let mut session = opened.map_err(|e| e.to_string())?;
    let tail = subtitle_tail(&mut session, subs).unwrap_or_default();
    Ok((session, tail))
}

/// The whole of what a queued file costs, off the UI thread. An import only
/// needs its pages warmed ([`read_ahead`]); the file argv named is *opened*
/// here instead, and that is the difference between a warm launch paying for
/// one header walk and paying for two -- the window is up either way.
pub(crate) fn open_ahead(
    what: Landing,
    path: &std::path::Path,
    stage: &std::sync::atomic::AtomicU8,
    gate: Option<engine::ImportGate>,
) -> Landed {
    match what {
        Landing::Import => read_ahead(path, stage, gate),
        Landing::Project => {
            let opened = PlaybackSession::open_project(path).map_err(|e| e.to_string());
            Landed::Project(opened)
        }
        Landing::Open => open_whole(path, true, stage),
    }
}

/// A whole timeline, opened here and handed over: the file argv named (`place`,
/// which *is* the timeline) and a file arriving at a window with no timeline to
/// import into, which fills the library instead. One function because they are
/// one read -- the engine's two doors differ by which lanes come up empty
/// ([`engine::PlaybackSession::open_library`]), not by what they walk.
pub(crate) fn open_whole(
    path: &std::path::Path,
    place: bool,
    stage: &std::sync::atomic::AtomicU8,
) -> Landed {
    use std::sync::atomic::Ordering::Relaxed;
    let opened = match place {
        true => PlaybackSession::open(path),
        false => PlaybackSession::open_library(path),
    };
    // The same two stages a read reports, because they are the same two
    // reads: the container, and then the tracks inside it.
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    Landed::Media(
        opened.map_err(|e| e.to_string()).map(|mut session| {
            let subs = subtitle_notice(&mut session, path).unwrap_or_default();
            (session, subs)
        }),
        place,
    )
}

/// Reads, off the UI thread, everything the import that follows would have read
/// -- and hands all of it over. Nothing here is a warm-up any more: the
/// container is *probed* ([`engine::PlaybackSession::probe_import`]) and the
/// subtitle tracks are *walked*, and [`Player::take_import`] registers what came
/// back. Measured on the 24 GB 4K HEVC remux: 21.4 s of header cold, 429 ms
/// warm, plus 1-4 s of probing the timeline's own first source, plus the cue
/// walk -- all of it here, and the window keeps painting through it.
///
/// `gate` is the timeline the file is checked against
/// ([`engine::PlaybackSession::import_gate`]), taken before the worker started
/// because a worker cannot reach the session. `None` is a window with no
/// timeline to import into: then there is nothing to check against and nothing
/// to register, so the file is *opened* here instead, whole
/// ([`open_whole`]) -- which is the same twenty seconds, on the same thread,
/// rather than on the one that draws.
///
/// The header error is carried back now rather than dropped: it is the engine's
/// own refusal, from the only walk anybody makes, so it is worded once and shown
/// at the landing. The subtitle refusal travels beside it for the same reason.
///
/// `stage` is what the line above the panel is naming while this runs.
pub(crate) fn read_ahead(
    path: &std::path::Path,
    stage: &std::sync::atomic::AtomicU8,
    gate: Option<engine::ImportGate>,
) -> Landed {
    use std::sync::atomic::Ordering::Relaxed;
    stage.store(ImportStage::Header as u8, Relaxed);
    // Nothing to import into, and nothing a subtitle file needs opened: the
    // first opens the library itself, here; the second has no container at all.
    let Some(gate) = gate else {
        return match is_subtitle(path) {
            true => Landed::Read(walk_subtitles(path), None),
            false => open_whole(path, false, stage),
        };
    };
    // The three doors an import goes through: a song is measured by its
    // duration and a still by its header -- both the engine's own reads, warmed
    // here and paid again at the landing, which is a header apiece -- and
    // everything else is the container walk, which is handed over whole.
    let probe = if engine::is_audio(path) {
        engine::AudioSession::duration_secs(path).ok();
        None
    } else if engine::is_image(path) || is_subtitle(path) {
        None
    } else {
        Some(PlaybackSession::probe_import(gate, path))
    };
    stage.store(ImportStage::Subtitles as u8, Relaxed);
    // ...and the tracks inside it, kept.
    Landed::Read(walk_subtitles(path), probe)
}

/// The toast's own words for a standalone audio import -- rate, channel
/// count and rounded length -- appended after every other tail, so an
/// audio-only file's IMPORTED line reads the numbers a properties card would
/// otherwise take a click to open. `has_video` is the caller's probed answer
/// ([`Player::has_video`], or the import's own `NoVideoTrack` refusal): a
/// film's toast is unchanged, an audio-only container gets the numbers the
/// same as a song, and a song whose own small header read comes back empty
/// stays plain.
pub(crate) fn audio_import_tail(path: &std::path::Path, has_video: bool) -> String {
    if has_video {
        return String::new();
    }
    let probe = engine::AudioSession::probe(path, 0).ok().flatten();
    let secs = engine::AudioSession::duration_secs(path).ok().flatten();
    match (probe, secs) {
        (Some(p), Some(secs)) => {
            format!(
                " — {} Hz, {} ch, {}s",
                p.sample_rate,
                p.channels,
                secs.round() as u64
            )
        }
        _ => String::new(),
    }
}

/// Every subtitle track a file carries, cues and all -- the walk that costs, in
/// the one place every door that pays it goes through. `Ok` and empty for a file
/// with none to read, which is what a file that is neither a container we can
/// walk ([`carries_subtitles`]) nor a subtitle file is: the same answer
/// `add_subtitle_tracks` gives it, and nothing is opened to find that out.
///
/// Nothing in here is a session, on purpose ([`PlaybackSession::parse_subtitles`]
/// is an associated fn): no borrow crosses the await, so this runs whole on a
/// worker while the window keeps painting.
pub(crate) fn walk_subtitles(path: &std::path::Path) -> Subs {
    match carries_subtitles(path) || is_subtitle(path) {
        true => PlaybackSession::parse_subtitles(path),
        false => Ok(Vec::new()),
    }
}

/// The subtitle tracks a media file carries, taken into the session as it is
/// opened, and the tail the notice grows for them: an mkv or an mp4 with
/// subtitles in it arrives with its subtitles, because a track nobody imported
/// is a track nobody knows is there. Every other container answers `None`
/// without being read.
///
/// A refusal is a tail too, never a failure of the open: the picture and the
/// sound of a film whose subtitle tracks cannot be walked are still the film.
///
/// Both halves at once, which only a *worker* may do: the walk reads the whole
/// file for its cues (`engine::subtitle::of_matroska`) -- ~200 ms on a two-hour
/// 4K remux, 9.7 s on a cold 25 GB one. The one caller is the open beside which
/// this runs on the worker ([`open_ahead`]), never the render thread; an
/// import splits the two halves across the hop instead ([`read_ahead`] walks,
/// [`subtitle_tail`] pushes).
pub(crate) fn subtitle_notice(
    session: &mut PlaybackSession,
    path: &std::path::Path,
) -> Option<String> {
    subtitle_tail(session, walk_subtitles(path))
}

/// What a subtitle walk ([`walk_subtitles`]) gave, on its way from whichever
/// thread paid for it to the timeline. `Send` all the way down (`sendable()`,
/// `engine/tests/subtitles.rs`), which is what lets the walk be a worker's.
pub(crate) type Subs = engine::Result<Vec<engine::subtitle::SubtitleTrack>>;

/// The tail a file's own subtitle tracks earn on the notice that names it, and
/// the push that puts them on the timeline -- the second half of the walk, the
/// cheap one ([`PlaybackSession::add_subtitle_tracks`]: no open, no seek, no
/// decode). Every door that arrives with a *file* words it here, once: the file
/// argv named, an import, and an import into an empty window cannot say the same
/// thing differently, whichever thread read the cues.
///
/// Worded for where they actually land: a *list* beside the media and nothing
/// over the picture -- a track shows when it is dragged onto a subtitle lane
/// ([`Player::subtitle_overlay`]), so the tail says the next move rather than
/// letting a count read as "and there they are on screen".
///
/// `None` for a file that gave none and for one whose tracks are on the timeline
/// already -- an import that adds nothing says nothing about subtitles. A
/// refusal is a tail too, never a failure of the import: the picture and the
/// sound of a film whose subtitle tracks cannot be walked are still the film.
pub(crate) fn subtitle_tail(session: &mut PlaybackSession, subs: Subs) -> Option<String> {
    match subs {
        Ok(tracks) => match session.add_subtitle_tracks(tracks) {
            0 => None,
            n => Some(format!(
                " — {n} subtitle track(s) in the file, in the subtitle list: drag one onto a \
                 subtitle track to show it"
            )),
        },
        Err(e) => Some(format!(" — SUBTITLES UNREAD: {e}")),
    }
}

/// What the subtitles toggle says it just did ([`Player::toggle_subtitles`]),
/// worded off the lanes: `placed` is every caption on every subtitle lane
/// ([`Player::placed_captions`]), because that -- and never the palette row the
/// list marks -- is what the toggle covers and uncovers.
///
/// Nothing placed is its own sentence: "SUBTITLES SHOWN — 0 caption(s)" over an
/// unchanged picture reads as a broken toggle, so it says the move that would
/// put words there instead.
pub(crate) fn subtitle_toggle_notice(on: bool, placed: usize) -> String {
    match (on, placed) {
        (true, 0) => "SUBTITLES SHOWN — nothing placed yet".to_string(),
        (true, n) => format!("SUBTITLES SHOWN — {n} caption(s) on the timeline"),
        (false, n) => format!("SUBTITLES HIDDEN — {n} caption(s) still placed"),
    }
}

/// The push both deliberate add-subtitles doors end on -- `+ S` and its key
/// ([`Player::add_subtitles`]) and a dropped or argv'd subtitle file
/// ([`Player::take_subtitles`]) -- so the two cannot come to word the same file
/// differently.
///
/// The walk having found *nothing* is a refusal and not a count: a container
/// with no subtitle track in it and a file whose tracks are all on the timeline
/// already both push zero rows, and they are opposite answers -- one says look
/// somewhere else, the other says you already have them. The engine's own door
/// draws the same line in the same words
/// ([`PlaybackSession::no_subtitles_in`], asked here because this route splits
/// the walk from the push to keep the walk off the render thread).
///
/// The file itself joins nothing either way: no library row, no lane, no clip.
/// Subtitles are a list the timeline carries, and this is the only thing that
/// touches it.
pub(crate) fn pushed(
    session: &mut PlaybackSession,
    path: &std::path::Path,
    tracks: Vec<engine::subtitle::SubtitleTrack>,
) -> engine::Result<usize> {
    match tracks.is_empty() {
        true => Err(PlaybackSession::no_subtitles_in(path)),
        false => Ok(session.add_subtitle_tracks(tracks)),
    }
}

/// What a subtitle row says under its name: how many cues it holds and whether
/// they are pictures ([`engine::subtitle::SubtitleTrack::is_bitmap`]) -- which
/// is the difference between a track an export writes into the file and one it
/// can only draw -- or, for a track that could not be read, the engine's own
/// reason verbatim. A refusal is still what a row can be *for*: a VobSub track
/// dropped from the list would say the film has no subtitles at all.
pub(crate) fn subtitle_detail(track: &engine::subtitle::SubtitleTrack) -> String {
    match (&track.refused, track.is_bitmap()) {
        (Some(why), _) => why.clone(),
        (None, true) => format!("{} cues — pictures", track.cues.len()),
        (None, false) => format!("{} cues", track.cues.len()),
    }
}

/// What the export card's Subtitles row says: the engine's own words for the
/// tracks an export carries (`plan`, [`engine::export::planned_subtitles`] asked
/// about `picks`) and, beside them, the rows it is carrying nothing of.
///
/// `picks` is [`Player::export_subs`] -- every track with a cue left in the
/// exported range -- so a track sitting in the list with eighty-three cues
/// nowhere near a trimmed timeline never reaches the engine at all, and the card
/// said nothing about it while the list went on showing it. Which tracks have
/// cues *here* is this side's answer and nobody else's, so this side words it.
///
/// Past [`SUB_PLAN_CHARS`] the line counts rather than names: every name is more
/// words in a value box `MENU_W` wide, and at 35 tracks the row wrapped to ten
/// lines and pushed the Destination row under the fold of the card. The names
/// are still one row each in the Subtitles list, with the cue count and the
/// reason under them ([`subtitle_detail`]) -- more than this line ever said.
///
/// The counted split follows the engine's own order of reasons (`refused`, then
/// pictures, then no cues), with the last asked of the timeline instead of the
/// track.
///
/// corner-cut: that split reads the same public fields the list rows read
/// (`refused`, [`engine::subtitle::SubtitleTrack::is_bitmap`]) rather than the
/// engine's decision, so a *new* reason to drop a track would be counted here as
/// embedded until this follows it -- the named line, which is the engine's
/// string verbatim, would say it correctly meanwhile. Upgrade path: reasons out
/// of `planned_subtitles` as data rather than as one sentence, which is an
/// engine change.
pub(crate) fn subtitle_plan(
    plan: String,
    tracks: &[engine::subtitle::SubtitleTrack],
    picks: &[usize],
) -> String {
    let mut parts: Vec<String> = Vec::new();
    // "none" is the engine's word for having nothing to say and would read as a
    // verdict on the tracks named after it. Anything else is the sentence the
    // export is actually planned from -- the lanes' own, wherever the timeline
    // places words ([`engine::export::planned_subtitles`]) -- and it is kept
    // whatever the palette picks are: dropping it on an empty pick list is how a
    // card showing "S1 eng — past the last picture" said nothing at all.
    if plan != "none" {
        parts.push(plan.clone());
    }
    parts.extend(
        tracks
            .iter()
            .enumerate()
            .filter(|(i, track)| !picks.contains(i) && !spoken_for(&plan, track))
            // Not "no cues here": a row the export leaves out is a row nothing
            // on the timeline placed, and what puts that right is the drag --
            // the same sentence the import notice ends with.
            .map(|(_, track)| format!("{} — in the palette, on no track", track.label)),
    );
    let named = parts.join("; ");
    if named.chars().count() <= SUB_PLAN_CHARS {
        return match named.is_empty() {
            true => "none".to_string(),
            false => named,
        };
    }
    let (mut embedded, mut unread, mut pictures, mut off) = (0, 0, 0, 0);
    for (i, track) in tracks.iter().enumerate() {
        match (
            track.refused.is_some(),
            track.is_bitmap(),
            picks.contains(&i) || spoken_for(&plan, track),
        ) {
            (true, _, _) => unread += 1,
            (_, true, _) => pictures += 1,
            (_, _, false) => off += 1,
            _ => embedded += 1,
        }
    }
    let mut counted = vec![format!("{embedded} of {} → embedded", tracks.len())];
    counted.extend(
        [
            (pictures, "pictures"),
            (unread, "unread"),
            (off, "in the palette, on no track"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}")),
    );
    counted.join("; ")
}

/// Whether the engine's sentence already speaks for this row: with words placed
/// on lanes, every clause of it names the palette row its lane shows
/// ([`engine::export::planned_subtitles`]) -- and a line that embeds a track and
/// then says the same track is on no track is a line contradicting itself.
///
/// corner-cut: the match is the label found in the sentence, because the engine
/// hands back one string and not the rows it spoke for. Ceiling: two rows whose
/// labels are one a prefix of the other ("eng", "eng 2") -- with the longer one
/// placed, the shorter loses its clause; it never gains a wrong one. Upgrade
/// path: `planned_subtitles` handing its clauses back as data, the same upgrade
/// [`subtitle_plan`]'s own note names.
fn spoken_for(plan: &str, track: &engine::subtitle::SubtitleTrack) -> bool {
    !track.label.is_empty() && plan.contains(track.label.as_str())
}

/// Every subtitle track one file gave, kept together. Plain data, planned
/// before anything is drawn -- the grouping is the branchy part and it is the
/// same answer whatever a styling puts around it.
///
/// [`Player::subtitle_section`] draws one block per group of these, and
/// [`sub_pick_name`] names the pick out of the same answer -- so what a heading
/// says and what a row a person clicked says cannot disagree.
#[derive(Debug, PartialEq)]
pub(crate) struct SubGroup {
    /// What the group is called: the file without its extension, which is what
    /// a person named the film even when the subtitles came out of a remux.
    pub(crate) name: String,
    /// What the swatch is asked by ([`file_tint`]) -- the file, so a subtitle
    /// row and the media rows of the same file wear one colour. `None` from
    /// that lookup for a standalone `.srt`, which is nobody's stream.
    pub(crate) path: PathBuf,
    pub(crate) rows: Vec<SubRow>,
}

/// One subtitle track, as a row shows it.
#[derive(Debug, PartialEq)]
pub(crate) struct SubRow {
    /// Which track of the session's list this row is: the *flat* index into the
    /// add-order Vec `PlaybackSession::subtitles` hands back, which is what a
    /// click sets `sub_track` to and what a caption placed on a subtitle lane
    /// names, so it is what a save writes into the `.edith` for that caption.
    /// Grouping moves rows around on screen and never touches this number.
    pub(crate) track: usize,
    /// Which of *this file's* tracks it is, counted from 1 -- the numbering
    /// [`row_name`] gives audio streams, for the same reason: two tracks off one
    /// remux that both say "eng" are told apart by nothing else.
    pub(crate) number: usize,
    pub(crate) label: String,
    /// The row's second line ([`subtitle_detail`]).
    pub(crate) detail: String,
    /// Why it cannot be shown, for a track that cannot: the only thing that
    /// greys a row out. Listed all the same -- a picker that hides them is a
    /// picker that lies.
    pub(crate) refused: Option<String>,
    /// Pictures rather than lines ([`engine::subtitle::SubtitleTrack::bitmap`]).
    pub(crate) bitmap: bool,
}

/// What a subtitle row is called, off the two fields the container really
/// stated rather than out of the flattened
/// [`label`](engine::subtitle::SubtitleTrack::label). The pair is what an export
/// writes (`TRACK_LANGUAGE` and `TRACK_NAME` are two fields), and reading the
/// display string back apart is the very heuristic that sent every French track
/// out as English: `lang_human` on a whole "und — Signs" matches nothing and
/// names a language nobody speaks.
///
/// The three shapes a track arrives in, all three of them real: a standalone
/// file states no language and is its own name, an embedded one states a
/// language and sometimes a title beside it, and a refused one states neither
/// and keeps the label it was refused under (`SubtitleTrack::refused`).
pub(crate) fn sub_title(sub: &engine::subtitle::SubtitleTrack) -> String {
    match (sub.language.as_str(), sub.name.as_str()) {
        ("", "") => sub.label.clone(),
        ("", name) => name.to_string(),
        // A track whose only name is the tag for "nobody said" says that in
        // words rather than showing the tag itself.
        (lang, "") => lang_human(lang).to_string(),
        // ...and one that says "nobody said" *and* gives a title is the title:
        // "unknown language — Signs" pads it with a word the file never said.
        ("und", name) => name.to_string(),
        (lang, name) => format!("{lang} — {name}"),
    }
}

/// The subtitle list as rows under the file each came out of: one group per
/// distinct path, in the order the files first appear, and each file's tracks
/// in the order they were added. Two remuxes' tracks arriving interleaved --
/// which is what importing a second film does -- still read as two films.
pub(crate) fn subtitle_rows(tracks: &[engine::subtitle::SubtitleTrack]) -> Vec<SubGroup> {
    let mut groups: Vec<SubGroup> = Vec::new();
    for (track, sub) in tracks.iter().enumerate() {
        let group = match groups.iter().position(|g| g.path == sub.path) {
            Some(i) => &mut groups[i],
            None => {
                groups.push(SubGroup {
                    name: sub
                        .path
                        .file_stem()
                        .map_or_else(|| file_name(&sub.path), |s| s.to_string_lossy().into()),
                    path: sub.path.clone(),
                    rows: Vec::new(),
                });
                groups.last_mut().expect("just pushed")
            }
        };
        group.rows.push(SubRow {
            track,
            number: group.rows.len() + 1,
            label: sub_title(sub),
            detail: subtitle_detail(sub),
            refused: sub.refused.clone(),
            bitmap: sub.is_bitmap(),
        });
    }
    groups
}

/// How the picked track is named wherever the pick is echoed -- the strip
/// header, the section heading, the toggle's own notice. What the track is
/// *and* which file it came out of: two remuxes each carrying an "eng" track
/// give the same word twice, and the file is the only thing that tells them
/// apart. A file that gave several of them numbers them within itself, the way
/// [`row_name`] numbers audio streams, since "eng" twice off one remux is the
/// same problem one file down.
///
/// Goes through [`subtitle_rows`], so the name a header says and the row a
/// person clicked cannot disagree -- and the label is humanised there, so an
/// "und" track is named in words here without being passed through
/// [`lang_human`] twice.
///
/// `None` for an index no track answers to, which is the silence
/// [`Player::subtitle_track`] gives at the same moment.
/// Where the picked subtitle row lands once `removed` has been taken off a list
/// that is `left` long afterwards. The pick follows the list: the same *track*
/// while it is still there -- every index past the one that went moves down
/// ([`engine::Project::remove_subtitles`]) -- the row that slid into the empty
/// place when the picked one is what went, and the last row when that was the
/// last. Zero on an emptied list, which is the index the section is not drawn at
/// all.
///
/// Its own function because the pick is what the overlay draws: left where it
/// was it would name a different track, and the plate over the picture would
/// change language on its own the moment a row above it went. What an export
/// writes is *not* this pick and cannot be desynced by a removal -- it is worked
/// out from the cues on the timeline each time ([`Player::export_subs`]).
pub(crate) fn sub_pick_after_removal(picked: usize, removed: usize, left: usize) -> usize {
    let picked = match removed < picked {
        true => picked - 1,
        false => picked,
    };
    picked.min(left.saturating_sub(1))
}

pub(crate) fn sub_pick_name(
    tracks: &[engine::subtitle::SubtitleTrack],
    track: usize,
) -> Option<String> {
    subtitle_rows(tracks).into_iter().find_map(|group| {
        let row = group.rows.iter().find(|row| row.track == track)?;
        let track = match group.rows.len() > 1 {
            false => row.label.clone(),
            true => format!("{} {}", row.label, row.number),
        };
        // A standalone `.srt` is already named after its own file: "sub.srt —
        // sub" says the one thing twice, and the film is only worth naming
        // where it is not in the label yet.
        Some(match row.label.starts_with(&group.name) {
            true => track,
            false => format!("{track} — {}", group.name),
        })
    })
}

/// Where a cue is drawn on the bed: left edge and width in pixels, through the
/// same [`Scale`] every clip box goes through -- so a cue and the take it is
/// spoken over line up at every zoom, which is the only reason the strip is
/// worth drawing. Microseconds are the cue's unit and seconds are the scale's,
/// and this is the one place they meet.
///
/// Never narrower than [`SUB_CUE_MIN_W`]: zoomed out, a one-second cue is a
/// fraction of a pixel, and a mark that rounds away reads as a track with
/// nothing in it.
pub(crate) fn cue_box(scale: Scale, cue: &engine::subtitle::Cue) -> (f32, f32) {
    let (start, end) = (cue.start_us as f64 / 1e6, cue.end_us as f64 / 1e6);
    (
        scale.px_at(start),
        scale.width_px(end - start).max(SUB_CUE_MIN_W),
    )
}

/// Which subtitle lane draws over the picture, out of the pick somebody made
/// and the lanes there are *now* ([`Player::active_sub_lane`]): the picked one
/// while it is still on the timeline, and the first lane otherwise -- which is
/// the whole of "one lane needs no picking", "the first lane added draws" and
/// "a removed lane's pick promotes its neighbour rather than showing nothing".
///
/// `None` only with no subtitle lane at all, which is a timeline with nowhere
/// for words to be.
///
/// A pick against the live list rather than a pointer kept in step with it: a
/// [`Lane`] is a position among its kind, so an add, a removal, a reorder and
/// every undo of those move what a stored handle names -- and each of them is
/// answered here, at the read, by a list that is never stale.
pub(crate) fn active_lane(picked: Option<Lane>, lanes: &[Lane]) -> Option<Lane> {
    picked
        .filter(|lane| lanes.contains(lane))
        .or_else(|| lanes.first().copied())
}

/// Which cues of a track are on screen at `at` seconds. Half-open, as the cue
/// itself is: one that ends exactly where the next begins hands over rather than
/// overlapping it for a frame, and several that genuinely overlap all come back
/// -- a sign and a line of dialogue are two cues at one moment.
pub(crate) fn cues_at(cues: &[engine::subtitle::Cue], at: f64) -> Vec<&engine::subtitle::Cue> {
    let us = (at * 1e6) as i64;
    cues.iter()
        .filter(|cue| cue.start_us <= us && us < cue.end_us)
        .collect()
}

/// The sources a repaint has not asked about yet. A key that is already there
/// means "asked", whatever state it is in, which is what stops a decode already
/// running from being started again by the next of sixty repaints a second.
pub(crate) fn unseen_sources(
    sources: &[Source],
    waves: &HashMap<(PathBuf, usize), Wave>,
) -> Vec<(PathBuf, usize)> {
    sources
        .iter()
        .map(|s| (s.path.clone(), s.audio_stream))
        .filter(|key| !waves.contains_key(key))
        .collect()
}

/// The same, for the per-file caches: one entry per *file*, however many of its
/// streams the timeline plays. Generic in the value, because the stream probe
/// and the still's own size are both asked once per file and answered by
/// presence in a map.
pub(crate) fn unseen_paths<V>(sources: &[Source], seen: &HashMap<PathBuf, V>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for s in sources {
        if !seen.contains_key(&s.path) && !out.contains(&s.path) {
            out.push(s.path.clone());
        }
    }
    out
}

/// The films a repaint may start a stand-in for ([`engine::proxy`]): the unseen
/// ones while this project makes them by itself, and **none at all** while it
/// does not -- until the Proxies switch is thrown, which is the ask, since
/// nothing else offers to make one.
///
/// A film left out here stays out of the map it was checked against, which is
/// what brings it back the moment either switch says yes: an import made while
/// both were off is not a film that missed its turn.
pub(crate) fn proxies_to_start<V>(
    auto: bool,
    cut_on_them: bool,
    sources: &[Source],
    seen: &HashMap<PathBuf, V>,
) -> Vec<PathBuf> {
    match auto || cut_on_them {
        true => unseen_paths(sources, seen),
        false => Vec::new(),
    }
}

/// Which timeline frame the playhead is on, by the rule the engine's own edits
/// use (playback.rs `secs_to_frame`): the frame that has started, with the
/// epsilon that keeps a clock sitting exactly on a boundary from reading as the
/// frame before it. Only ever a hint here -- what an edit does is still the
/// engine's answer, taken from the same seconds.
pub(crate) fn frame_at(secs: f64, fps: f64) -> u32 {
    (secs * fps + 1e-6).floor().max(0.) as u32
}
