//! What comes in and what goes out: the sources, the imports, the
//! subtitles, the project file and the lanes.

use crate::*;
use crate::follow::{Follow, follow_decision};
use crate::notices::{ToastAction, Tone, push_sticky_toast};
use crate::player::project_dialogs::PathFor;

/// Edits since the last autosave that make the next tick write without
/// waiting for the timer.
const AUTOSAVE_EDITS: u32 = 20;
/// The autosave periods Settings offers, in seconds: `None` is Off, which writes no
/// autosave at all. A missing or unreadable preference is one minute.
pub(crate) const AUTOSAVE_CHOICES: [Option<u64>; 4] = [None, Some(30), Some(60), Some(300)];
const AUTOSAVE_DEFAULT_SECS: u64 = 60;

/// The least time between two autosaves that a structural edit asked for: a burst of new
/// sequences or markers writes once, not once per click.
const AUTOSAVE_THROTTLE: Duration = Duration::from_secs(5);

/// The name of the one undo step a recovery takes: what Edit reads back as
/// "Undo Recover unsaved edits".
const RECOVER_STEP: &str = "Recover unsaved edits";

/// The session a recovery installs: the saved file as it stands, with the
/// autosave's timeline taken over on top of it as one undo step
/// ([`PlaybackSession::adopt_as_step`]), so undo takes the recovered edits back
/// to the saved timeline. With no saved file to stand on (`saved` is `None`, an
/// untitled session) the autosave is the project and undo has nothing earlier
/// to return to. Free of any `Player`, so a test can recover a file pair.
pub(crate) fn recovered_session(
    saved: Option<&Path>,
    autosave: &Path,
) -> Result<PlaybackSession, String> {
    let autosaved = PlaybackSession::open_project(autosave).map_err(|e| e.to_string())?;
    let Some(saved) = saved.filter(|p| p.is_file()) else {
        return Ok(autosaved);
    };
    let mut session = PlaybackSession::open_project(saved).map_err(|e| e.to_string())?;
    let mark = session.step_mark();
    if session.adopt_as_step(&autosaved) {
        session.name_step(mark, RECOVER_STEP);
        Ok(session)
    } else {
        Err("the autosave does not fit on the saved project as one step".into())
    }
}

/// Where the auto-proxies *default* survives, beside the keybindings and the
/// theme, same corner-cut persistence as [`crate::subs::subtitle_style_path`]:
/// a torn write costs this one preference and nothing else.
///
/// This is the window's own answer, not the project's: a project that was
/// saved with the switch set already carries its own line
/// ([`engine::edith::Document::auto_proxy`]) and always wins once one is
/// open ([`Player::install_project`]) -- this file is only ever read before
/// that, which is what makes it the difference between "the switch reverts to
/// On on every relaunch" and "the switch stayed how it was left".
/// The toasts for the clips a save had to shorten to their files' ends: one toast
/// each, since a toast is one line that the window would cut off past its width.
/// Past three, the rest are counted in one line.
pub(crate) fn shortened_notes(notes: &[String]) -> Vec<String> {
    const SHOWN: usize = 3;
    let mut out: Vec<String> = notes.iter().take(SHOWN).cloned().collect();
    if notes.len() > SHOWN {
        out.push(format!("{} more clips were shortened to fit their files.", notes.len() - SHOWN));
    }
    out
}

pub(crate) fn auto_proxies_pref_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("auto-proxies")
}

/// `Off` unless the file says `on`; missing, unreadable or garbled all read as
/// `Off`, the same as [`load_proxies_pref`] -- a preference file is not the
/// user's work, so a bad one is worth no message at startup.
pub(crate) fn load_auto_proxies_pref() -> bool {
    match std::fs::read_to_string(auto_proxies_pref_path()) {
        Ok(text) => text.trim() == "on",
        Err(_) => false,
    }
}

/// Writes the pick whole, the way [`crate::ui::theme::save`] writes the
/// palette.
pub(crate) fn save_auto_proxies_pref(on: bool) -> std::io::Result<()> {
    let path = auto_proxies_pref_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, if on { "on\n" } else { "off\n" })
}

/// Where the Proxies *default* survives -- Proxies' own sibling of
/// [`auto_proxies_pref_path`], same corner-cut persistence.
///
/// This is the window's own answer, not the project's: a project saved with
/// the switch set carries its own line and always wins once one is open
/// ([`Player::install_project`]) -- this file is only ever read before that.
/// The Settings word for one autosave period: `Off`, or how often it is written.
pub(crate) fn autosave_label(secs: Option<u64>) -> String {
    match secs {
        None => "Off".to_string(),
        Some(s) if s % 60 == 0 => format!("{} min", s / 60),
        Some(s) => format!("Every {s} s"),
    }
}

/// The small print beside one autosave period in its list.
pub(crate) fn autosave_detail(secs: Option<u64>) -> &'static str {
    match secs {
        None => "unsaved edits are lost if edith quits",
        Some(_) => "written beside the project while you work",
    }
}

pub(crate) fn autosave_pref_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("autosave")
}

/// The period the file names: `off` is `None`, a number of seconds is that period. Missing or
/// garbled reads as one minute, the default, since a preference file is not the user's work.
pub(crate) fn load_autosave_pref() -> Option<u64> {
    match std::fs::read_to_string(autosave_pref_path()) {
        Ok(text) => match text.trim() {
            "off" => None,
            n => Some(n.parse().unwrap_or(AUTOSAVE_DEFAULT_SECS)),
        },
        Err(_) => Some(AUTOSAVE_DEFAULT_SECS),
    }
}

/// Writes the pick whole, the way [`save_proxies_pref`] does.
pub(crate) fn save_autosave_pref(secs: Option<u64>) -> std::io::Result<()> {
    let path = autosave_pref_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, secs.map_or("off\n".to_string(), |n| format!("{n}\n")))
}

pub(crate) fn proxies_pref_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("proxies")
}

/// `Off` unless the file says `on`; missing, unreadable or garbled all read as
/// `Off`, the same default the field has always started at -- a preference
/// file is not the user's work, so a bad one is worth no message at startup.
pub(crate) fn load_proxies_pref() -> bool {
    match std::fs::read_to_string(proxies_pref_path()) {
        Ok(text) => text.trim() == "on",
        Err(_) => false,
    }
}

/// Writes the pick whole, the way [`save_auto_proxies_pref`] does.
pub(crate) fn save_proxies_pref(on: bool) -> std::io::Result<()> {
    let path = proxies_pref_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, if on { "on\n" } else { "off\n" })
}

/// How long ago an autosave was written, in plain words: the recovery question's
/// first half. Under a minute is "a moment ago"; a day back is "yesterday".
/// Pure, so the words are tested without a file or a clock.
pub(crate) fn age_words(secs: u64) -> String {
    let ago = |n: u64, one: &str, many: &str| format!("{} ago", crate::notices::count_noun(n as usize, one, many));
    match secs {
        0..60 => "a moment ago".to_string(),
        60..3600 => ago(secs / 60, "minute", "minutes"),
        3600..86_400 => ago(secs / 3600, "hour", "hours"),
        86_400..172_800 => "yesterday".to_string(),
        _ => ago(secs / 86_400, "day", "days"),
    }
}

/// The recovery question's words for an autosave: when its unsaved edits were
/// written. The buttons carry the rest of the question.
pub(crate) fn recovery_words(sidecar: &Path) -> String {
    match std::fs::metadata(sidecar).and_then(|m| m.modified()) {
        Ok(written) => recovery_words_at(written, std::time::SystemTime::now()),
        Err(_) => "Unsaved edits were found".to_string(),
    }
}

/// [`recovery_words`] for an autosave written at `written`, read at `now`: the
/// local clock time it was written and how long ago that was, so "from 14:32
/// (1 minute ago)" says both when and how recent. Pure but for the time zone.
pub(crate) fn recovery_words_at(written: std::time::SystemTime, now: std::time::SystemTime) -> String {
    let age = now.duration_since(written).map_or(0, |d| d.as_secs());
    let clock = chrono::DateTime::<chrono::Local>::from(written).format("%H:%M");
    format!("Unsaved edits from {clock} ({}) were found.", age_words(age))
}

/// The recovery toast's words once the window has edits of its own: recovering
/// would replace them, and says so before the button is pressed.
pub(crate) fn recovery_replaces_words() -> String {
    "Recovering will replace the current edits · undo takes them back".to_string()
}

impl Player {
    /// The lanes an insert of `path` writes, mirroring the choice
    /// [`PlaybackSession::place_stream_at`] makes: a still goes on a video lane
    /// alone, a song on an audio lane alone, and a picture takes its sound with
    /// it (the grouped take on `V1`/`A1`, or the same-numbered pair for a named
    /// further lane). With no lane named, a still and a picture land on `V1`
    /// and a song on `A1`.
    pub(crate) fn insert_lanes(path: &Path, onto: Option<Lane>) -> Vec<Lane> {
        if engine::is_image(path) {
            let lane = match onto {
                Some(lane) if lane.kind == LaneKind::Video => lane,
                _ => Lane::V1,
            };
            return vec![lane];
        }
        if engine::is_audio(path) {
            let lane = match onto {
                Some(lane) if lane.kind == LaneKind::Audio => lane,
                _ => Lane::A1,
            };
            return vec![lane];
        }
        match onto {
            Some(lane) if lane.ord > 0 => vec![lane, Lane::new(LaneKind::Audio, lane.ord)],
            _ => vec![Lane::V1, Lane::A1],
        }
    }

    /// The one way a library row reaches the timeline: the Add button and a row
    /// dragged onto a lane both come here, so there is a single answer to what
    /// "add this source" does. The whole source goes in as one grouped take at
    /// `at` -- the frame the pointer let it go on, or the playhead for the
    /// button, which names no place. It is the same insert a paste makes, so
    /// everything after it moves along rather than being painted over. Reseeks
    /// like every other edit, and drops the timeline's selection with it: the
    /// insert has just moved the indices it pointed at.
    /// Puts a source on the end of the timeline: the frame past the last clip on any lane, so
    /// nothing is pushed and nothing is overwritten.
    pub(crate) fn append_source(&mut self, path: &Path, stream: usize, cx: &mut Context<Self>) {
        let end = self.session.as_ref().map(|s| (s.timeline_duration() * self.fps).round() as u32);
        self.insert_source(path, stream, None, end, cx);
    }

    pub(crate) fn insert_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        self.place_source(path, stream, onto, at, false, cx);
    }

    /// A library item let go on the bed: it **overwrites** what is under it for its length, and
    /// nothing is pushed along, unless `insert` (Ctrl held) asks for the push
    /// ([`insert_source`](Self::insert_source)). Refusals say why in a toast, as every placement does.
    pub(crate) fn drop_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        self.place_source(path, stream, onto, at, !insert, cx);
    }

    /// A library item let go below the last lane, where there is no row to aim at: the
    /// track it needs is made first, in the same undo step as the placement. A file with a
    /// picture needs its video lane *and* the audio lane beside it, so both are made (`V2`
    /// with `A2`, say), and a placement that is refused takes the new lanes back again, so no
    /// empty track is ever left behind.
    pub(crate) fn drop_source_below_lanes(
        &mut self,
        path: &Path,
        stream: usize,
        at: Option<u32>,
        insert: bool,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.begin_batch();
        let lane = add_drop_lanes(session, path);
        self.drop_source(path, stream, Some(lane), at, insert, cx);
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let placed = !session.lane_clips(lane).is_empty();
        session.end_batch();
        if !placed {
            // Refused: the batch holds nothing but the lanes just made, so one undo takes them back.
            session.undo();
            self.reset_after_reseek();
        }
    }

    /// The one placement behind [`insert_source`](Self::insert_source) and
    /// [`drop_source`](Self::drop_source): `overwrite` picks the engine door.
    fn place_source(
        &mut self,
        path: &Path,
        stream: usize,
        onto: Option<Lane>,
        at: Option<u32>,
        overwrite: bool,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The drop ends the hover: nothing is below the stack any more.
        self.drag_below = false;
        // The source monitor's own item, dragged by its picture or its row
        // with marks set: the marked part goes, not the whole file.
        if self.source_has_marks(path, stream) {
            let kind = match overwrite {
                true => engine::SourceEdit::Overwrite,
                false => engine::SourceEdit::Insert,
            };
            self.source_edit_at(kind, at, onto, cx);
            return;
        }
        // The lane the pointer named cannot hold this kind of file: refused by
        // name, in the same words the ghost was tinted by on the way down
        // ([`lane_refuses`]). The Add button names no lane and so is never
        // refused here -- where a file goes when nobody says is the engine's
        // choice, in `place_stream_at`, not one made twice here.
        if let Some(why) = onto.and_then(|lane| lane_refuses(path, lane, self.has_video(path))) {
            self.notify_user(why.into());
            cx.notify();
            return;
        }
        // A locked lane takes nothing, named or chosen: the Add button's pick
        // and a drop onto a lane both write the lanes [`Self::insert_lanes`]
        // names, and the one that is locked refuses the whole insert with its
        // own toast before anything moves.
        for lane in Self::insert_lanes(path, onto) {
            if self.lane_locked_refusal(lane) {
                cx.notify();
                return;
            }
        }
        // The engine's own length for the file, noted when the import took it
        // in: a row that has never been on a lane is placeable at its full
        // length, which is the whole point of an import that only fills the
        // library.
        let fps = self.fps;
        let placed = match &mut self.session {
            // Seconds, because that is what the engine's own door takes: the
            // frame the pointer named goes back through the same rate every box
            // on the bed is drawn at, so it lands on the frame it was let go on
            // rather than a neighbouring one.
            Some(session) => {
                let at = at.map_or_else(|| session.now(), |frame| f64::from(frame) / fps);
                match overwrite {
                    true => session.place_stream_overwrite_at(at, path, stream, onto),
                    false => session.place_stream_at(at, path, stream, onto),
                }
            }
            None => Ok(false),
        };
        match placed {
            Ok(true) => {
                self.selected.clear();
                self.reset_after_reseek();
            }
            // The engine's own words: a stream that cannot join this timeline
            // says which property disagrees, exactly as a refused import does.
            Err(e) => self.notify_user(format!("Nothing added: {e}").into()),
            Ok(false) => {
                self.notify_user("Nothing added: that file could not be placed here".into())
            }
        }
        cx.notify();
    }

    /// `I`/`O` while a library item is previewed: the preview's playhead becomes
    /// that item's in or out mark (the source monitor's marks), kept in order
    /// the way the timeline's pair is.
    pub(crate) fn mark_source(&mut self, is_in: bool, cx: &mut Context<Self>) {
        let Some(preview) = &self.preview_session else {
            return;
        };
        let Some(src) = preview.sources().first() else {
            return;
        };
        let key = (src.path.clone(), src.audio_stream);
        let at = preview.now();
        let (mut a, mut b) = self.source_marks.get(&key).copied().unwrap_or((None, None));
        if is_in {
            a = Some(at);
        } else {
            b = Some(at);
        }
        if let (Some(x), Some(y)) = (a, b)
            && x > y
        {
            (a, b) = (Some(y), Some(x));
        }
        self.source_marks.insert(key, (a, b));
        cx.notify();
    }

    /// `ctrl+u` while a library item is previewed: lets go of that item's marks.
    pub(crate) fn clear_source_marks(&mut self, cx: &mut Context<Self>) {
        if let Some(src) = self.preview_session.as_ref().and_then(|p| p.sources().first()) {
            self.source_marks.remove(&(src.path.clone(), src.audio_stream));
        }
        cx.notify();
    }

    /// The three-point edit (`F9` insert, `F10` overwrite): the library item
    /// being previewed, or the last one that was, goes onto the timeline at the
    /// playhead (the timeline's in mark when set) for its marked in..out -- the
    /// whole item with no marks -- onto the first unlocked video and audio
    /// lanes it has. Insert cuts the clip there and ripples the later ones on
    /// those lanes; overwrite replaces what is there and moves nothing. One
    /// undo step ([`PlaybackSession::edit_from_source_at`]); a locked lane or a
    /// take it would pull apart refuses with the engine's words. On success the
    /// preview closes (the timeline shows what was done) and the playhead
    /// moves to the end of the new take, so the next press follows on.
    pub(crate) fn source_edit(&mut self, kind: engine::SourceEdit, cx: &mut Context<Self>) {
        self.source_edit_at(kind, None, None, cx);
    }

    /// Whether `(path, stream)` is the source monitor's item and has a mark.
    fn source_has_marks(&self, path: &Path, stream: usize) -> bool {
        self.preview_session.is_some()
            && self.source_target.as_ref().is_some_and(|(p, s)| p == path && *s == stream)
            && self
                .source_marks
                .get(&(path.to_path_buf(), stream))
                .is_some_and(|(a, b)| a.is_some() || b.is_some())
    }

    /// [`Self::source_edit`] at a given frame and onto a given lane (a drop),
    /// else at the playhead on the first free lanes.
    pub(crate) fn source_edit_at(
        &mut self,
        kind: engine::SourceEdit,
        drop_at: Option<u32>,
        onto: Option<Lane>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let verb = match kind {
            engine::SourceEdit::Insert => "Insert",
            engine::SourceEdit::Overwrite => "Overwrite",
        };
        let target = self
            .preview_session
            .as_ref()
            .and_then(|p| p.sources().first().map(|s| (s.path.clone(), s.audio_stream)))
            .or_else(|| self.source_target.clone());
        let Some((path, stream)) = target else {
            self.notify_user(
                format!("{verb}: no library item has been previewed").into(),
            );
            cx.notify();
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let want = canon(&path);
        let Some(source) = session
            .sources()
            .iter()
            .position(|s| s.audio_stream == stream && (s.path == path || canon(&s.path) == want))
        else {
            self.notify_user(format!("{verb}: {} is not in this project", file_name(&path)).into());
            cx.notify();
            return;
        };
        let fps = self.fps;
        let still = engine::is_image(&path);
        let picture = still || (!engine::is_audio(&path) && engine::probe_media(&path).video.is_some());
        let sound = !still
            && engine::AudioSession::probe_streams(&path).is_ok_and(|streams| streams.len() > stream);
        let total = if picture {
            session.file_frames(&path)
        } else {
            engine::AudioSession::stream_duration_secs(&path, stream)
                .ok()
                .flatten()
                .map_or(0, |secs| (secs * fps).ceil() as u32)
        };
        let lanes: Vec<(Lane, bool)> = session
            .lanes()
            .into_iter()
            .filter(|l| l.kind != LaneKind::Subtitle)
            .map(|l| (l, session.lane_locked(l)))
            .collect();
        let ask = SourceEditAsk {
            fps,
            lanes: &lanes,
            playhead: drop_at.unwrap_or_else(|| frame_at(session.now(), fps)),
            range: self.range.filter(|_| drop_at.is_none()),
            marks: self.source_marks.get(&(path.clone(), stream)).copied().unwrap_or((None, None)),
            source,
            total,
            picture: picture && self.source_video,
            sound: sound && self.source_audio,
            still,
            video_patch: Some(self.patch_video),
            audio_patch: Some(self.patch_audio),
        };
        let (at, items) = match plan_source_edit(&ask) {
            Ok((at, mut items)) => {
                // A drop names its lane: the half of that kind goes there.
                if let Some(lane) = onto
                    && !lanes.iter().any(|(l, locked)| *l == lane && *locked)
                    && let Some(item) = items.iter_mut().find(|(l, _)| l.kind == lane.kind)
                {
                    item.0 = lane;
                }
                (at, items)
            }
            Err(why) => {
                self.notify_user(format!("{verb}: {why}").into());
                cx.notify();
                return;
            }
        };
        let len = items.iter().map(|(_, c)| c.frames()).max().unwrap_or(0);
        let done = match self.session.as_mut() {
            Some(session) => session.edit_from_source_at(kind, f64::from(at) / fps, &items),
            None => return,
        };
        match done {
            Ok(()) => {
                self.selected.clear();
                self.reset_after_reseek();
                self.mark_dirty();
                // The source monitor stays up, ready for the next edit; only
                // the timeline's playhead moves, to the end of the new take.
                if let Some(session) = self.session.as_mut() {
                    session.seek(f64::from(at + len) / fps);
                }
                self.reset_after_reseek();
                let (word, where_) = match kind {
                    engine::SourceEdit::Insert => ("Inserted", "rippling what follows"),
                    engine::SourceEdit::Overwrite => ("Overwrote", "nothing moved"),
                };
                self.notify_user(
                    format!(
                        "{word} {} at {} for {}: {where_}; ctrl+z undoes it",
                        file_name(&path),
                        timecode(f64::from(at) / fps, fps),
                        timecode(f64::from(len) / fps, fps),
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(format!("{verb} refused: {e}").into()),
        }
        cx.notify();
    }

    /// Paints a sliding waveform of the marked audio clip onto a video lane.
    /// Same implementation the clip menu and the `h` stroke both reach.
    pub(crate) fn create_visualizer(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some((lane, idx)) = self.selected.anchor() else {
            self.notify_user("No visualizer: select a clip with sound".into());
            cx.notify();
            return;
        };
        let placed = match &mut self.session {
            None => {
                self.notify_user("No visualizer: no file open".into());
                cx.notify();
                return;
            }
            Some(session) => {
                let Some(clip) = session.lane_clips(lane).get(idx).cloned() else {
                    self.notify_user("No visualizer: select a clip with sound".into());
                    cx.notify();
                    return;
                };
                let Some(src) = session.sources().get(clip.source).cloned() else {
                    self.notify_user("No visualizer: that clip has no source".into());
                    cx.notify();
                    return;
                };
                session.place_visualizer(
                    clip.start,
                    &src.path,
                    src.audio_stream,
                    clip.in_frame,
                    clip.out_frame,
                    None,
                )
            }
        };
        match placed {
            Ok(true) => {
                self.notify_user("Visualizer added".into());
                self.reset_after_reseek();
            }
            Err(e) => self.notify_user(format!("No visualizer: {e}").into()),
            Ok(false) => self.notify_user("No visualizer: that clip could not be placed".into()),
        }
        cx.notify();
    }

    pub(crate) fn toggle_viz_flag(&mut self, bit: u8, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(clip) = session.lane_clips(lane).get(idx).cloned() else {
            return;
        };
        if clip.visualizer & 1 == 0 {
            return;
        }
        if session.set_visualizer(lane, idx, clip.visualizer ^ bit) {
            // A look is a project field, so the write arms autosave and the
            // close prompt the same way a grade or a trim does -- these chips
            // are clicked, not dispatched through `act`, so nothing upstream
            // has armed it for them.
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    pub(crate) fn set_viz_style(&mut self, style: u8, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(clip) = session.lane_clips(lane).get(idx).cloned() else {
            return;
        };
        if clip.visualizer & 1 == 0 {
            return;
        }
        let next = engine::decode::with_viz_style(clip.visualizer, style);
        if next != clip.visualizer && session.set_visualizer(lane, idx, next) {
            // [`Self::toggle_viz_flag`]'s own reason: a clicked look arms
            // autosave here, because nothing upstream has.
            self.mark_dirty();
            self.reset_after_reseek();
        }
        cx.notify();
    }

    pub(crate) fn viz_paint(&self) -> u64 {
        self.selected
            .anchor()
            .and_then(|(lane, idx)| {
                self.session
                    .as_ref()?
                    .lane_clips(lane)
                    .get(idx)
                    .map(|c| c.viz_paint)
            })
            .unwrap_or(0)
    }

    /// Which of the two inks the look section's wheel and colour field act on
    /// ([`VizSlot`]). An open colour field is given up with the change: it is
    /// seeded with the picked slot's colour, so it would otherwise edit a
    /// colour the row is no longer showing.
    pub(crate) fn set_viz_slot(&mut self, slot: VizSlot, cx: &mut Context<Self>) {
        self.viz_slot = slot;
        self.viz_hex_edit = None;
        cx.notify();
    }

    /// Opens the colour type-in on `seed`, the picked slot's colour as the row
    /// already shows it, so a backspace edits that rather than a field opening
    /// empty over a colour still in force (the number field's own rule). The
    /// transition row's field is given up: one inline editor at a time.
    pub(crate) fn edit_viz_hex(&mut self, seed: &str) {
        self.transition_edit = None;
        self.viz_hex_edit = Some(ColorEdit::new(seed));
    }

    /// The colour field's enter. A colour it can read is written on the picked
    /// slot -- one undo step, a typed colour being one edit -- and the field
    /// closes. Anything else is refused *in words* with the field left open and
    /// the colour where it was, so the next try is a backspace away.
    pub(crate) fn commit_viz_hex(&mut self, cx: &mut Context<Self>) {
        let Some((hue, sat)) = self.viz_hex_edit.as_mut().and_then(|edit| edit.commit()) else {
            // Refused in words: the field stays open holding what it could
            // not read.
            cx.notify();
            return;
        };
        self.viz_hex_edit = None;
        // Nothing changed is not an edit. The field opens seeded with the
        // row's own `viz_color_hex`, and that hex reads back as a *different*
        // (hue, sat) for a large share of inks (the engine's round-trip
        // canary counts 47 053 of 65 536 exact), so a commit that wrote
        // whatever it parsed would take an undo step -- and arm autosave --
        // for the colour the row was already showing, on an enter that
        // changed nothing.
        let paint = self.viz_paint();
        let slot = self.viz_slot.picked(paint);
        if slot.hue_sat(paint) == (hue, sat) {
            cx.notify();
            return;
        }
        self.write_viz_hs(hue, sat, false, cx);
    }

    /// Puts hue and saturation on the picked slot ([`VizSlot::picked`]): the
    /// wheel's sample and the colour field's commit both land here, so the two
    /// doors cannot disagree about which ink they write.
    ///
    /// `live` is [`Player::write_color`]'s own flag, for the colour card's
    /// reason: the false one takes the undo step the whole gesture rolls back
    /// to, the true one is every sample *inside* a drag
    /// (`PlaybackSession::set_viz_paint_live`). A wheel sample is not a table
    /// entry but the painter's own input, and it is the write that rebuilds the
    /// picture at the playhead -- so one committed entry per pointer sample
    /// would stamp a sixty-entry gesture into a hundred-step history and leave
    /// the user a hundred-step history of one drag.
    pub(crate) fn write_viz_hs(&mut self, hue: u8, sat: u8, live: bool, cx: &mut Context<Self>) {
        // A write supersedes whatever a drag was holding
        // ([`Player::write_color`]'s rule).
        self.pending_viz = None;
        let slot = self.viz_slot;
        self.set_viz_paint_with(live, move |p| slot.picked(p).set_hue_sat(p, hue, sat), cx);
    }

    /// One pointer sample on the wheel, as the hue and saturation under it.
    /// `first` is the press, which is never held: it is the pick the gesture
    /// rolls back to, so it has to be taken against the ink the hand picked up.
    /// Every sample after it goes through [`stash_or_write`] -- a wheel sample
    /// is not a table entry but the painter's input, so the write is what
    /// rebuilds the whole picture at the playhead, and a sweep that wrote each
    /// one would queue a reopen per pixel, cancel all but one and freeze the
    /// window for the sum.
    pub(crate) fn drag_viz_hs(
        &mut self,
        at: gpui::Point<Pixels>,
        first: bool,
        cx: &mut Context<Self>,
    ) {
        let b = self.viz_wheel.get();
        let w = f32::from(b.size.width).max(1.);
        let h = f32::from(b.size.height).max(1.);
        let dx = f32::from(at.x - b.origin.x) - w / 2.;
        let dy = h / 2. - f32::from(at.y - b.origin.y);
        let radius = w.min(h) / 2.;
        let r = (dx * dx + dy * dy).sqrt() / radius;
        let sat = (r.clamp(0., 1.) * 255.).round() as u8;
        let ang = dx.atan2(dy).to_degrees();
        let hue_deg = (ang + 360.) % 360.;
        let hue = (hue_deg / 360. * 256.).round() as u16 as u8;
        // The slot is read once, at the sample that carries it: whichever ink
        // the hand picked up is the one the whole gesture writes, and the slot
        // is what the held sample draws with while the worker catches up.
        let slot = self.viz_slot.picked(self.viz_paint());
        let busy = self.seek_since.is_some();
        match stash_or_write(&mut self.pending_viz, (slot, hue, sat), first, busy) {
            Some((_, hue, sat)) => self.write_viz_hs(hue, sat, !first, cx),
            // The wheel draws off the held sample, so the mark goes on
            // following the hand while the picture catches up.
            None => cx.notify(),
        }
    }

    pub(crate) fn set_viz_strands(&mut self, n: u8, cx: &mut Context<Self>) {
        self.set_viz_paint_with(false, |p| engine::decode::with_viz_paint_strands(p, n), cx);
    }

    pub(crate) fn set_viz_glow(&mut self, n: u8, cx: &mut Context<Self>) {
        self.set_viz_paint_with(false, |p| engine::decode::with_viz_paint_glow(p, n), cx);
    }

    /// `live` picks the door, as [`Player::write_viz_hs`] does: the strands and
    /// glow steppers are presses and take their own undo step, and the two are
    /// on the packed bytes rather than on either ink, so they are unaffected by
    /// which slot the wheel is on.
    fn set_viz_paint_with(
        &mut self,
        live: bool,
        f: impl FnOnce(u64) -> u64,
        cx: &mut Context<Self>,
    ) {
        let Some((lane, idx)) = self.selected.anchor() else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(clip) = session.lane_clips(lane).get(idx).cloned() else {
            return;
        };
        if clip.visualizer & 1 == 0 {
            return;
        }
        let next = f(clip.viz_paint);
        if next != clip.viz_paint {
            let took = match live {
                true => session.set_viz_paint_live(lane, idx, next),
                false => session.set_viz_paint(lane, idx, next),
            };
            if took {
                // Every look write arms autosave and the close gate
                // (`write_color`'s own rule, and these doors have no keymap
                // action behind them to arm it the way `act` does for the
                // verbs): the wheel's press, a strands/glow stepper and a
                // typed colour all land here. The drag's live samples land
                // here too, which is why the arm is on the write and not on
                // the gesture -- `mark_dirty` is idempotent, so the press, the
                // samples and the release are one armed edit rather than
                // sixty.
                self.mark_dirty();
                self.reset_after_reseek();
            }
        }
        cx.notify();
    }

    /// Takes a library row's file out of the list, which is the one thing a row
    /// can lose. Refused in the engine's own words while clips still play from
    /// it -- and those words name the lanes holding them, so the refusal says
    /// what to delete first. The list itself is the report that it worked: the
    /// row is gone from it.
    pub(crate) fn remove_source(&mut self, path: &Path, stream: usize, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_source(path, stream));
        let text = match removed {
            Some(Ok(idx)) => {
                // Tells a running background scan of this source nobody wants
                // its levels any more, and drops the entry that was standing
                // in the way of a re-add starting a fresh one
                // ([`Player::cache_media`]'s dedupe reads presence here).
                if let Some(scan) = self.silence_bg.remove(&(path.to_path_buf(), stream)) {
                    scan.cancel
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                // The picked row may be the one that just went, and the engine
                // reseeks, so this owes the flag reset like every other edit.
                if self.selected_asset.as_ref() == Some(&(path.to_path_buf(), stream)) {
                    self.selected_asset = None;
                }
                self.asset_picks.retain(|r| *r != (path.to_path_buf(), stream));
                // A copied clip names its source by *index*, and every index
                // past the one that went has just moved down: without this the
                // next paste puts some other file on the timeline.
                self.clipboard = clipboard_after_remove(std::mem::take(&mut self.clipboard), idx);
                // The library is project state, and this door is the row's
                // own menu click -- nothing in [`Player::act`]'s list arms
                // it. Behind the `Ok`, so a refusal (the engine's own words
                // while clips still play the file) is not an edit.
                self.mark_dirty();
                self.reset_after_reseek();
                // The last row leaves a session naming no file: nothing to
                // play, nothing to save and nothing to show, which is the empty
                // window the editor launches as. The next import scaffolds a
                // fresh timeline from whatever file it is, at that file's own
                // rate -- which is why the session goes rather than lingering
                // on with the gone file's parameters.
                match self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.sources().is_empty())
                {
                    true => {
                        self.close_session();
                        format!("Removed {}: the library is empty", file_name(path))
                    }
                    // The undo stack goes with it (`Project::remove_source`):
                    // said here, because a `z` that does nothing afterwards
                    // would otherwise read as a bug.
                    false => format!("Removed {}: there is nothing left to undo", file_name(path)),
                }
            }
            Some(Err(e)) => format!("Not removed: {e}"),
            None => "No timeline: no file open".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// Removes a library row *and* every clip playing it, in that order --
    /// the plain [`Player::remove_source`] is refused by the engine while any
    /// clip still plays a source, which left a placed file with no reachable
    /// way out of the library at all (user 2026-08-21: "can not remove a
    /// media from library"). Destructive, and named so on the plate it is
    /// picked from; one undo entry per deleted clip, exactly as deleting them
    /// by hand would leave, and then the source itself goes the same door
    /// every other removal takes.
    pub(crate) fn remove_source_and_clips(
        &mut self,
        path: &Path,
        stream: usize,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            self.notify_user("No timeline: no file open".into());
            return;
        };
        let Some(idx) = session
            .sources()
            .iter()
            .position(|s| s.path == path && s.audio_stream == stream)
        else {
            return;
        };
        // One clip at a time, re-scanning after each: deleting a clip takes
        // its group partners (a V1 picture and its A1 sound) with it, so a
        // list of indices collected up front goes stale the moment the first
        // one lands -- and a stale index deletes some other film's clip.
        // Bounded by the count that was there when we started, so a
        // `delete_clip` that ever answers false cannot spin here.
        let count_of = |session: &engine::PlaybackSession| {
            session
                .lanes()
                .into_iter()
                .flat_map(|lane| session.lane_clips(lane))
                .filter(|c| c.source == idx)
                .count()
        };
        let started_with = count_of(session);
        let mut budget = session
            .lanes()
            .into_iter()
            .flat_map(|lane| session.lane_clips(lane))
            .filter(|c| c.source == idx)
            .count();
        while budget > 0 {
            let Some((lane, at)) = session.lanes().into_iter().find_map(|lane| {
                session
                    .lane_clips(lane)
                    .iter()
                    .position(|c| c.source == idx)
                    .map(|at| (lane, at))
            }) else {
                break;
            };
            if !session.delete_clip(lane, at) {
                break;
            }
            budget -= 1;
        }
        // What actually left the bench, not how many calls it took: one
        // `delete_clip` on a grouped clip takes its partner with it.
        let deleted = started_with.saturating_sub(count_of(session));
        self.reset_after_reseek();
        self.remove_source(path, stream, cx);
        // `remove_source` already wrote its own line; this one says what went
        // with it, which is the half that line cannot know about.
        if deleted > 0 {
            self.notify_user(
                format!(
                    "{deleted} clip{} deleted with {}",
                    if deleted == 1 { "" } else { "s" },
                    file_name(path)
                )
                .into(),
            );
        }
        cx.notify();
    }

    /// Back to the window the editor launches as: no timeline, no library, no
    /// picture -- and the hint that says to open a file. What removing the last
    /// library row leaves, since a session whose library is empty has nothing
    /// left to be ([`Player::remove_source`]).
    ///
    /// Everything a *loaded project* resets goes here for its reasons (an index
    /// into a timeline that is gone names nothing), plus the three per-file
    /// caches: they are keyed by path, and the next file to arrive fills them
    /// again.
    pub(crate) fn close_session(&mut self) {
        self.session = None;
        self.session_swapped();
        // The picture goes with it, or the empty window would keep showing the
        // last frame of a timeline that no longer exists.
        //
        // corner-cut: its atlas tile is not released -- `window.drop_image` wants
        // a `&mut Window` this door has no other reason to take. One tile per
        // emptied library, against one per displayed frame in `pump`; the
        // upgrade path is threading the window through `act_on_row`.
        self.image = None;
        // The drawn cue with it, and its tile for the same reason as above.
        self.sub_image = None;
        self.clipboard.clear();
        self.selected.clear();
        self.selected_asset = None;
        self.asset_picks.clear();
        // The subtitle rows go with the timeline they were on, and so does the
        // lane that was shown: S1 of one project is not S1 of the next.
        self.sub_track = 0;
        self.sub_lane = None;
        self.context_menu = None;
        self.library_menu = None;
        self.eq_open = None;
        self.color_open = None;
        self.transform_open = None;
        self.speed_open = None;
        self.effects_open = None;
        self.close_silence();
        self.waves.clear();
        self.thumbs.clear();
        self.streams.clear();
        self.bitrates.clear();
        self.facts.clear();
        self.sizes.clear();
        self.syncs.clear();
        self.has_video.clear();
        // Scanned off sources that are not in the library any more.
        self.silence_levels.clear();
        // ...and every background read still chasing one of them: cancelled
        // rather than merely dropped, so a landing closure over a gone window
        // does not write into a `silence_levels` the next session starts
        // empty (`Player::cache_media`'s spawn checks this flag before it
        // inserts).
        for scan in self.silence_bg.values() {
            scan.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.silence_bg.clear();
        // Every gesture in flight, dropped for `reset_after_reseek`'s reason
        // (it drops the trim below): a drag holds a bar, a clip or a band of a
        // timeline that has just stopped existing.
        self.scrubbing = false;
        self.volume_dragging = false;
        self.preview_scrubbing = false;
        self.eq_dragging = false;
        self.speed_dragging = false;
        self.effects_drag = None;
        self.effects_draft = None;
        self.color_dragging = false;
        self.color_wheel_drag = None;
        self.viz_hs_dragging = false;
        self.pending_color = None;
        self.pending_viz = None;
        self.transform_dragging = false;
        self.pending_transform = None;
        self.pending_speed = None;
        self.displayed = 0;
        self.dropped = 0;
        self.started = None;
        // The empty window's own: no name in the titlebar, nowhere chosen to
        // export or save to yet, and a rate that only keeps the timecode
        // reading in frames until a file brings its own (`main`).
        self.name = NO_FILE.into();
        self.export_path = PathBuf::new();
        self.project_path = PathBuf::new();
        self.fps = 30.;
        // No decoder to wait for a frame from: the hint is what shows. The
        // transport reads `Stopped` from the session being gone, so there is no
        // end-of-stream state left to clear here.
        self.reset_after_reseek();
        self.seek_since = None;
    }

    /// The row's own play button: opens `path` as a session of its own and
    /// shows it in place of the timeline's picture, without moving the
    /// timeline's playhead or touching its undo stack -- what a library
    /// preview is. [`PlaybackSession::open_with_audio_stream`] builds exactly
    /// this (a one-clip session, ungraded, passthrough) bound to `stream`, the
    /// row's own audio track rather than always the file's first.
    pub(crate) fn open_preview(
        &mut self,
        path: &std::path::Path,
        stream: usize,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The timeline would otherwise keep playing under the preview -- two
        // soundtracks at once -- so it is paused here and resumed on the way
        // out only if this is what paused it ([`Player::close_preview`]).
        self.preview_playing = self
            .session
            .as_ref()
            .is_some_and(PlaybackSession::is_playing);
        if let Some(session) = self.session.as_mut() {
            session.pause();
        }
        match PlaybackSession::open_with_audio_stream(path, stream) {
            Ok(mut session) => {
                self.source_target = Some((path.to_path_buf(), stream));
                self.source_video = true;
                self.source_audio = true;
                session.drop_late_pictures(true);
                // Parked on its first frame, paused: opening a source is looking at it,
                // and play is one press away.
                self.preview_session = Some(session);
                // The one place the window's level reaches a session
                // ([`Player::apply_volume`]) -- and it goes at the timeline
                // under the preview too, which is what makes closing the
                // preview hand back a session already at the shown level
                // rather than the one it had before the preview opened.
                self.apply_volume();
                self.session_swapped();
                self.notify_user(
                    format!(
                        "Previewing {}. Drag it onto the timeline to use it.",
                        stem(path)
                    )
                    .into(),
                );
            }
            Err(e) => self.notify_user(format!("Preview failed: {e}").into()),
        }
        self.reset_after_reseek();
        cx.notify();
    }

    /// The way out of a preview: `esc`, or the banner's own button. The
    /// timeline was never pumped while the preview played, so its picture is
    /// stale where it left off -- a reseek to the position it is already at
    /// is what brings a fresh frame back, exactly as loading a project does.
    pub(crate) fn close_preview(&mut self, cx: &mut Context<Self>) {
        if self.preview_session.take().is_none() {
            return;
        }
        self.notify_user("Back to the timeline".into());
        match self.session.as_ref().map(PlaybackSession::now) {
            Some(now) => {
                self.seek(now, cx);
                if self.preview_playing
                    && let Some(session) = self.session.as_mut()
                {
                    session.play();
                }
            }
            None => {
                self.image = None;
                cx.notify();
            }
        }
        // The session on screen is the timeline's again, and it is told the
        // window's level here rather than being trusted to have kept it: a
        // level changed while the preview was up reaches every session
        // ([`Player::apply_volume`]), and this is the belt to that pair of
        // braces -- the session that takes the picture is the one that must be
        // heard at the level the control shows.
        self.apply_volume();
        // The picture swaps back to the timeline's, so everything keyed to a
        // session hears it (`Player::session_swapped`) -- the sound watch among
        // them, or the timeline's own unchanged reason would read as a device
        // that came back the moment the preview closed.
        self.session_swapped();
        self.preview_playing = false;
    }

    /// The rate and layout the whole timeline's audio is, taken from the stream
    /// of the first source that could have one: what a library row has to match
    /// to be placeable. `None` until that file has been probed, and then nothing
    /// is greyed for it.
    ///
    /// The first source that is *not a still*, which is the rule the engine
    /// holds every import to (`playback::audio_source_of`) -- a picture at the
    /// head of the list (a removal moves indexes) has no stream to describe
    /// anything with.
    pub(crate) fn timeline_audio(&self) -> Option<(u32, u16)> {
        let first = self
            .session
            .as_ref()?
            .sources()
            .iter()
            .find(|s| !engine::is_image(&s.path))?;
        let info = self
            .streams
            .get(&first.path)?
            .iter()
            .find(|s| s.index == first.audio_stream)?;
        Some((info.sample_rate, info.channels))
    }

    /// Queues a file for the library. Nothing is read here: the reading is
    /// [`read_ahead`] on a worker, and [`Player::take_import`] is what finally
    /// touches the timeline, one repaint later and with the pages warm. A drop
    /// is not a key press, so the export guard on the key handler does not
    /// cover it and this checks for itself.
    ///
    /// One file at a time, in arrival order: a drop can carry six and argv can
    /// name more, and six header walks racing over one disk finish no sooner
    /// than six in a row -- while the line above the panel has exactly one file
    /// to name.
    pub(crate) fn import(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.imports.push_back(path.to_path_buf());
        self.start_import(cx);
    }

    /// Starts the worker for the next queued file, if no worker is running.
    /// Called again as each import lands, which is what drains the queue.
    pub(crate) fn start_import(&mut self, cx: &mut Context<Self>) {
        if self.importing.is_some() {
            return;
        }
        let Some(path) = self.imports.pop_front() else {
            return;
        };
        // An OpenTimelineIO file is read here, whole, and is no media to probe: it becomes a sequence.
        if crate::importing::is_timeline(&path) {
            self.import_timeline_file(&path, cx);
            self.start_import(cx);
            return;
        }
        let stage = Arc::new(std::sync::atomic::AtomicU8::new(ImportStage::Header as u8));
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The fork is made here, once, and carried to the landing: an import is
        // *probed* on the worker and registered from what came back, while the
        // file argv named -- and a file arriving at a window with no timeline to
        // import into -- is *opened* on the worker and handed over whole. None
        // of the three leaves the UI thread anything to read: a cold 24 GB
        // header walk is twenty seconds, and the window keeps painting through
        // all of them.
        let what = arrival(self.opening.as_deref(), &path);
        // The timeline the file will be checked against, taken here because the
        // worker cannot reach the session: two clones and no disk
        // ([`PlaybackSession::import_gate`]). `None` is a window with nothing to
        // import into, which is the fork that opens the file outright.
        let gate = self.session.as_ref().map(PlaybackSession::import_gate);
        let read = cx.background_executor().spawn({
            let (path, stage) = (path.clone(), Arc::clone(&stage));
            let cancelled = Arc::clone(&cancelled);
            // The Cancel beside the line reaches the *walk* this way: every
            // cluster the reads below step over checks this flag, so a stopped
            // import stops the disk rather than only the landing
            // ([`engine::demux::with_cancel`]).
            async move {
                engine::demux::with_cancel(&cancelled, || open_ahead(what, &path, &stage, gate))
            }
        });
        let now = Instant::now();
        self.importing = Some(Import {
            path: path.clone(),
            started: now,
            stage,
            seen: ImportStage::Header,
            since: now,
            cancelled: Arc::clone(&cancelled),
        });
        cx.spawn(async move |this, cx| {
            let landed = read.await;
            this.update(cx, |this, cx| {
                this.importing = None;
                // Cancelled while it read: the window was given back at the
                // click and said so then, so what the worker carried is dropped
                // without a second word ([`Player::cancel_import`]).
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                this.take_import(&path, landed, cx);
                // The next one is started by the repaint this notified, which
                // is also what starts the files argv named ([`poll_import`]).
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Keeps the import line's two clocks honest: the elapsed one runs from the
    /// worker, and the stall one from the last time the stage it is naming
    /// actually changed. Sampled here rather than while drawing, for
    /// [`Player::poll_export`]'s reason.
    ///
    /// ...and starts whatever is queued behind it, which is the one place the
    /// files argv named can begin: they are put in the queue before there is a
    /// context to spawn a worker from.
    /// Takes each finished stand-in's verdict, once. Sampled here rather than
    /// while drawing, for [`Player::poll_export`]'s reason -- and it is the one
    /// place a proxy job's outcome is read, so "ready" on a row and the file in
    /// the cache are the same fact.
    ///
    /// Nothing waits for it: until a proxy is ready the film itself is what
    /// plays, switch or no switch ([`engine::PlaybackSession::picture_path`]),
    /// so a failure costs a line and no more.
    ///
    /// A stopped one settles here too ([`Player::cancel_proxy`]), which is what
    /// makes the stop honest: the worker's own answer is what says the encode
    /// is over and its half-written file gone -- and where the encode *beat*
    /// the cancel, the answer is a finished stand-in and it is reported as one
    /// rather than thrown away.
    pub(crate) fn poll_proxies(&mut self, cx: &mut Context<Self>) {
        let done: Vec<(PathBuf, bool, engine::Result<PathBuf>)> = self
            .proxies
            .iter()
            .filter_map(|(path, state)| match state {
                Proxy::Making(job) => Some((path.clone(), false, job.outcome()?)),
                Proxy::Cancelling(job) => Some((path.clone(), true, job.outcome()?)),
                _ => None,
            })
            .collect();
        for (path, stopped, outcome) in done {
            let text = match (&outcome, stopped) {
                (Ok(_), _) => format!(
                    "Editing copy ready for {}: Editing copies on cuts on it",
                    file_name(&path)
                ),
                // Not a failure and not worded as one: the worker gave up
                // because it was asked to, and it took its half-written file
                // with it.
                (Err(_), true) => format!(
                    "Editing copy stopped for {}: nothing of it was kept, and the original is what \
                     plays",
                    file_name(&path)
                ),
                (Err(e), false) => format!(
                    "Editing copy failed for {}: {e} — the original is what plays",
                    file_name(&path)
                ),
            };
            eprintln!("{text}");
            self.notify_user(text.into());
            let state = match (outcome, stopped) {
                (Ok(_), _) => Proxy::Ready,
                (Err(_), true) => Proxy::Cancelled,
                (Err(_), false) => Proxy::Failed,
            };
            self.proxies.insert(path, state);
            cx.notify();
        }
    }

    /// Starts the stand-in for one file: the slot is taken the moment the ask
    /// goes out, so a repaint mid-encode cannot start a second one, and what
    /// comes back off the worker settles the slot.
    ///
    /// One body, two doors: the sweep [`Player::cache_media`] runs over
    /// everything the session has not seen, and the switch on a library row
    /// ([`Player::toggle_proxy`]) for the one file under the pointer. The cap on
    /// how many run at once belongs to the sweep -- a person who asks for *this*
    /// film's stand-in has asked, and is told by the row how far it has got.
    pub(crate) fn start_proxy_for(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let path = path.to_path_buf();
        // The same flag [`engine::proxy::generate_if_wanted`] polls at the door
        // and around the header read: a click on the row while the header is
        // still being walked ([`Player::toggle_proxy`]'s `Asked` arm) sets it,
        // so the walk unwinds mid-read instead of running to completion.
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.proxies
            .insert(path.clone(), Proxy::Asked(cancel.clone()));
        let started = cx.background_executor().spawn({
            let path = path.clone();
            async move { engine::proxy::generate_if_wanted(&path, &cancel) }
        });
        cx.spawn(async move |this, cx| {
            let started = started.await;
            this.update(cx, |this, cx| {
                // Switched off while the header was being read
                // ([`Player::toggle_proxy`]): the slot is no longer the `Asked`
                // this put there, so what came back is *stopped as it lands*
                // rather than installed over the stop. Without this the second
                // half of a click made during the ask is an encoder nothing in
                // the map can reach -- and it finishes minutes later and puts
                // back the stand-in somebody switched off.
                let stopped = !matches!(this.proxies.get(&path), Some(Proxy::Asked(_)));
                let state = match started {
                    // Cancelled and *held*, exactly as a stop of a running one
                    // is: the worker's own answer is what settles it, so an
                    // encode that beat the stop is reported as the finished
                    // stand-in it is ([`Player::poll_proxies`]).
                    Ok(Some(job)) if stopped => {
                        job.cancel();
                        Proxy::Cancelling(job)
                    }
                    // ...and a stop of something that was never going to be
                    // encoded is simply the film itself, which is what it was.
                    Ok(None) | Err(_) if stopped => Proxy::Cancelled,
                    Ok(Some(job)) => Proxy::Making(job),
                    // A film that needs none keeps the state it was inserted
                    // with, which is what it is.
                    Ok(None) => Proxy::Native,
                    Err(e) => {
                        let text = format!(
                            "No editing copy for {}: {e} — the original is what plays",
                            file_name(&path)
                        );
                        eprintln!("{text}");
                        this.notify_user(text.into());
                        Proxy::Failed
                    }
                };
                this.proxies.insert(path, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The switch on a library row: this file's stand-in on, or off. One
    /// control for the whole life of the thing, because a row with a separate
    /// start, stop and delete is three buttons for one fact.
    ///
    /// * being made -- the ask is to *stop*, which is
    ///   [`Player::cancel_proxy`]'s half-written-file-and-all path and never a
    ///   deletion;
    /// * asked for, and the worker has not answered yet -- the same stop, made
    ///   before there is a job to stop: the slot is marked and the answer is
    ///   cancelled as it lands ([`Player::start_proxy_for`]). Never a second
    ///   start, which is what two encoders on one `.part` path used to be;
    /// * there is one -- the file in the cache goes ([`engine::proxy::delete`]),
    ///   and the slot is left [`Proxy::Cancelled`] rather than emptied: an empty
    ///   slot is what the next repaint's sweep starts an encode from
    ///   ([`proxies_to_start`] passes on everything the map has not seen), so
    ///   emptying it would rebuild the stand-in a second after it was turned
    ///   off. A delete that could not happen leaves the row *on*, because the
    ///   file is still there and still what plays;
    /// * anything else -- start one.
    ///
    /// `showed_stop` is what the row was drawing when the pointer went down:
    /// a stand-in that finished between that paint and this click is a click on
    /// a **stop**, and a stop settles as the ready stand-in it is rather than
    /// deleting a file nobody asked to lose ([`Player::cancel_proxy`]'s own
    /// promise).
    ///
    /// The off is remembered past the window ([`engine::proxy::set_off`]): the
    /// sweep runs again at every launch, and a switch that forgets is one that
    /// re-encodes tomorrow what was turned off today.
    pub(crate) fn toggle_proxy(
        &mut self,
        path: &std::path::Path,
        showed_stop: bool,
        cx: &mut Context<Self>,
    ) {
        match self.proxies.get(path) {
            Some(Proxy::Making(_)) => self.cancel_proxy(path, cx),
            // Already winding down: the second click on a stop is nothing.
            Some(Proxy::Cancelling(_)) => {}
            Some(Proxy::Asked(cancel)) => {
                // Wakes the header read mid-walk ([`engine::proxy::started`]):
                // without this the stop only ever landed once the read had
                // already run to completion on its own (DEBT #91).
                cancel.store(true, std::sync::atomic::Ordering::Release);
                engine::proxy::set_off(path, true);
                self.proxies.insert(path.to_path_buf(), Proxy::Cancelled);
                let text = format!("Stopping the stand-in for {}…", file_name(path));
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            // The encode beat the click: what is under the pointer is a stop,
            // and the film has a stand-in now.
            Some(Proxy::Ready) if showed_stop => {
                let text = format!(
                    "Editing copy ready for {}: it finished before the stop",
                    file_name(path)
                );
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            Some(Proxy::Ready) => {
                let text = match engine::proxy::delete(path) {
                    Ok(gone) => {
                        engine::proxy::set_off(path, true);
                        self.proxies.insert(path.to_path_buf(), Proxy::Cancelled);
                        match gone {
                            true => format!(
                                "Editing copy off for {}: the original is what plays",
                                file_name(path)
                            ),
                            false => {
                                format!("No editing copy for {}: nothing to delete", file_name(path))
                            }
                        }
                    }
                    // The file is still on the disk and [`engine::proxy::cached`]
                    // is still handing it to playback, so the row stays on: the
                    // one thing it must never do is say off over a proxy that is
                    // playing.
                    Err(e) => format!(
                        "Editing copy kept for {}: {e} — it could not be deleted and is still what plays",
                        file_name(path)
                    ),
                };
                eprintln!("{text}");
                self.notify_user(text.into());
                cx.notify();
            }
            _ => {
                engine::proxy::set_off(path, false);
                self.start_proxy_for(path, cx);
            }
        }
    }

    /// The × on a library row while its stand-in is being made: the worker is
    /// asked to give up at its next frame and delete the half-written file it
    /// was writing ([`engine::proxy::Job::cancel`]), and the row says so until
    /// it has. A whole feature film re-encoded is minutes of a machine the cut
    /// is being made on, and there was no way to take it back.
    ///
    /// Nothing is deleted from here and nothing waits: the film itself is what
    /// plays throughout ([`engine::PlaybackSession::picture_path`] stats the
    /// cache and finds nothing), so a stop costs a line and no more. Asking
    /// twice is asking once -- the second click finds a state that is not
    /// [`Proxy::Making`] and answers nothing -- and a cancel that arrives after
    /// the encode has already written its file settles as the *ready* stand-in
    /// it is ([`Player::poll_proxies`]), never as a deletion of one in use.
    pub(crate) fn cancel_proxy(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Taken out of its slot and put back one state on, rather than removed:
        // a path that left this map is a path the next repaint starts an encode
        // for all over again ([`Player::cache_media`]).
        let Some(slot) = self.proxies.get_mut(path) else {
            return;
        };
        if !matches!(slot, Proxy::Making(_)) {
            return;
        }
        let Proxy::Making(job) = std::mem::replace(slot, Proxy::Cancelled) else {
            unreachable!("the slot was just read as one being made")
        };
        job.cancel();
        *slot = Proxy::Cancelling(job);
        // Remembered past this window, for [`Player::toggle_proxy`]'s reason:
        // the launch sweep would otherwise start again what was just stopped.
        engine::proxy::set_off(path, true);
        let text = format!("Stopping the stand-in for {}…", file_name(path));
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// What the Proxies button says after the state: how far the stand-ins
    /// have got, since "on" with nothing made yet and "on" with the whole
    /// library standing in are two different things to be looking at. Empty
    /// where no film here wants one, which is the ordinary case.
    pub(crate) fn proxy_tail(&self) -> String {
        // No session, no library row could be making anything: a prior
        // session's jobs (never cleared out of `self.proxies` on close, since
        // a proxy is derived and re-checked from disk on next open rather
        // than tracked per session) must not print as this one's progress.
        if self.session.is_none() {
            return String::new();
        }
        let making = self.proxies.values().filter_map(|p| match p {
            Proxy::Making(job) => Some(job.progress()),
            _ => None,
        });
        // The one furthest along would read as done while three others sit at
        // nothing: the *least* finished is what the button reports.
        match making.fold(f32::INFINITY, f32::min) {
            least if least.is_finite() => format!(" · {}%", (least * 100.) as u32),
            _ => String::new(),
        }
    }

    /// Pushes the window's two stand-in switches at the session, which is the
    /// only place they are ever written -- [`Player::apply_volume`]'s contract,
    /// and for a sharper reason: a fresh session comes up making stand-ins for
    /// every big film, so a person who turned that off before importing
    /// anything would watch the very import they were waiting to prevent start
    /// an encode.
    pub(crate) fn apply_proxies(&mut self) {
        let (on, auto) = (self.proxies_on, self.auto_proxies_on);
        if let Some(session) = &mut self.session {
            // Deliberately not armed: the only caller is
            // [`Self::install_media`], the import/load path, where arming is
            // the one thing the `autosave_armed` rule forbids -- a project
            // nobody has written must not start asking to be saved. The
            // user's own door is the switch ([`Self::toggle_proxies`] /
            // [`Self::toggle_auto_proxies`]), and that is where the arm is.
            session.set_proxies(on);
            session.set_auto_proxies(auto);
        }
    }

    /// Whether any stand-in is still being made -- what keeps the frame loop
    /// alive while one is, so its percentage moves on screen.
    ///
    /// One being stopped counts: its worker is still running until it reaches
    /// the frame it gives up at, and the repaint that notices *that* is this
    /// same loop -- without it a stopped row would sit on "stopping…" until
    /// something else happened to repaint the window.
    pub(crate) fn making_proxies(&self) -> bool {
        self.proxies
            .values()
            .any(|p| matches!(p, Proxy::Making(_) | Proxy::Cancelling(_)))
    }

    /// How many films are between "shall we?" and "done": the number the start
    /// of another is held against ([`PROXIES_AT_ONCE`]). A header being read
    /// counts, because the answer may be an encode -- and so does one still
    /// winding down from a stop, which is an encoder that has not let go of the
    /// seat yet.
    pub(crate) fn in_flight_proxies(&self) -> usize {
        self.proxies
            .values()
            .filter(|p| matches!(p, Proxy::Asked(_) | Proxy::Making(_) | Proxy::Cancelling(_)))
            .count()
    }

    /// Cuts on the stand-ins, or on the films themselves. The picture and only
    /// the picture: the sound is the film's either way, and so is every frame
    /// an export writes.
    ///
    /// Set on the window and pushed at the session, not the other way about:
    /// this is an import option and a person picks it before the first film
    /// arrives ([`Player::apply_proxies`]). With nothing open it is the pick
    /// the next session comes up with.
    pub(crate) fn toggle_proxies(&mut self, cx: &mut Context<Self>) {
        let on = !self.proxies_on;
        self.proxies_on = on;
        if let Some(session) = &mut self.session {
            let changed = session.proxies() != on;
            session.set_proxies(on);
            if changed {
                self.mark_dirty();
            }
        }
        // What is really under the switch, said in the same breath: a project
        // whose films have no stand-in yet would otherwise read as a switch
        // that does nothing.
        let ready = self
            .proxies
            .values()
            .filter(|p| matches!(p, Proxy::Ready))
            .count();
        let making = self
            .proxies
            .values()
            .filter(|p| matches!(p, Proxy::Making(_)))
            .count();
        let text = match (on, ready, making) {
            (false, ..) => "Editing copies off: the originals are what play".to_string(),
            (true, 0, 0) => "Editing copies on: no video here has one".to_string(),
            (true, 0, n) => format!("Editing copies on: {n} still being made"),
            (true, n, 0) => format!("Editing copies on: cutting on {n}"),
            (true, n, m) => format!("Editing copies on: cutting on {n}, {m} still being made"),
        };
        eprintln!("{text}");
        // Kept for the next launch, not only the next project -- the same
        // reason auto-proxies keeps one ([`save_auto_proxies_pref`]).
        if let Err(e) = save_proxies_pref(on) {
            let path = proxies_pref_path();
            self.notify_user(
                format!(
                    "Editing copies default could not be kept: {} — {e}",
                    path.display()
                )
                .into(),
            );
        }
        self.notify_user(text.into());
        // The switch reseeks the session, and the wait for that picture is what
        // this clears -- with nothing open there is no open to wait for, and a
        // clock started against one would be a seek line about nothing.
        if self.session.is_some() {
            self.reset_after_reseek();
        }
        cx.notify();
    }

    /// Makes the stand-ins as the films arrive, or makes none until they are
    /// asked for. The other half of the switch above: this one decides what is
    /// *encoded*, that one what is *watched*.
    ///
    /// With it off nothing is started by an import at all -- turning Proxies on
    /// is what asks for the ones this project needs ([`Player::cache_media`]),
    /// which is said here because a switch whose only effect is elsewhere is a
    /// switch that reads as doing nothing.
    ///
    /// The window's own, like the switch above and for its reason: this one
    /// decides what an *import* does, so it is answerable before the import.
    pub(crate) fn toggle_auto_proxies(&mut self, cx: &mut Context<Self>) {
        let on = !self.auto_proxies_on;
        self.auto_proxies_on = on;
        if let Some(session) = &mut self.session {
            // [`Self::toggle_proxies`]'s own gate: the switch is a click and
            // the session is where the project hears it.
            let changed = session.auto_proxies() != on;
            session.set_auto_proxies(on);
            if changed {
                self.mark_dirty();
            }
        }
        let text = match on {
            true => "Editing copies on import: every import gets one",
            false => "Editing copies on import off: no import makes one",
        };
        eprintln!("{text}");
        // Kept for the next launch, not only the next project -- a flip made
        // with nothing open, or a project never saved after it, used to
        // vanish the moment the window closed ([`load_auto_proxies_pref`]).
        if let Err(e) = save_auto_proxies_pref(on) {
            let path = auto_proxies_pref_path();
            self.notify_user(
                format!(
                    "Import editing copies default could not be kept: {} — {e}",
                    path.display()
                )
                .into(),
            );
        }
        self.notify_user(text.into());
        cx.notify();
    }

    pub(crate) fn poll_import(&mut self, cx: &mut Context<Self>) {
        match &mut self.importing {
            Some(import) => {
                import.poll();
            }
            None => self.start_import(cx),
        }
    }

    /// The Cancel beside the import line: the window is given back at once and
    /// the file does not land. Everything queued behind it goes too -- a person
    /// who has stopped an import of six dropped files has stopped the six, and
    /// leaving five to start themselves would be the same wait under another
    /// name.
    ///
    /// The read in flight is stopped too, where the container lets it be: a
    /// Matroska walk checks this flag between clusters and gives up there
    /// ([`engine::demux::with_cancel`]). See [`Import::cancelled`] for what an
    /// mp4 header still costs.
    pub(crate) fn cancel_import(&mut self, cx: &mut Context<Self>) {
        let Some(import) = self.importing.take() else {
            return;
        };
        import
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let waiting = self.imports.len();
        self.imports.clear();
        let tail = match waiting {
            0 => String::new(),
            n => format!(" — {n} more dropped from the queue"),
        };
        let text = format!(
            "Import cancelled: {}{tail} — the read stops where the container lets it",
            file_name(&import.path)
        );
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Takes a read-ahead file into the library and nowhere else: the timeline
    /// is not touched, and the row is dragged onto a lane when it is wanted
    /// there. Nothing moves, so nothing reseeks; a refusal is shown as the
    /// engine worded it and changes nothing.
    ///
    /// The export guard again, and not for the caller's sake: an export can
    /// have started during the seconds the worker was reading, and a drop
    /// during an export has always been a silent no-op.
    pub(crate) fn take_import(
        &mut self,
        path: &std::path::Path,
        landed: Landed,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        // The file argv named is the one file in the queue that is not an
        // import: it *is* the timeline, and the worker has already opened it.
        // All that is left here is to hang everything derived from it off the
        // window -- the clock, the title, where an export and a save go -- and
        // that is arithmetic, not a read.
        let (subs, probe) = match landed {
            Landed::Read(subs, probe) => (subs, probe),
            what => {
                // Only when *this* is the file argv named: a project dropped
                // while that one is still being read must not make it land as
                // an import.
                if self.opening.as_deref() == Some(path) {
                    self.opening = None;
                }
                match what {
                    Landed::Project(opened) => self.install_project(path, opened, cx),
                    Landed::Media(opened, place) => {
                        let text = self.install_media(path, opened, place);
                        eprintln!("{text}");
                        self.notify_user(text.into());
                        // The file argv named becomes the timeline, and a
                        // timeline that arrives fits, as a first import does
                        // (DESIGN §7). Opened without a place it is a library
                        // row and the view stays where it is.
                        if place && self.session.as_ref().is_some_and(|s| !s.is_empty()) {
                            self.zoom_fit(cx);
                        }
                        cx.notify();
                    }
                    Landed::Read(..) => unreachable!("matched above"),
                }
                // The line a launch has always printed, now printed when the
                // file actually arrives: it is the mark that says the timeline
                // is up, as the window's own appearance is the other one.
                if let Some(meta) = self.session.as_ref().map(PlaybackSession::meta) {
                    println!(
                        "{}: {}x{} @ {:.2} fps, {} samples",
                        path.display(),
                        meta.width,
                        meta.height,
                        meta.frame_rate,
                        meta.frame_count
                    );
                }
                // A new project's first footage is in: the file it was named for is written now.
                if std::mem::take(&mut self.new_project_fresh) {
                    self.save_project(cx);
                }
                return;
            }
        };
        // An empty window has no library to add to yet: the file opens one. An
        // import is an import whether or not a session was already up, so it
        // fills the library either way; the one thing that differs is that the
        // first file into an empty timeline is also placed on it, and the zoom
        // fits it (DESIGN §7). A file *named at launch* is the other fork --
        // that one is an open, and it does become the timeline (`main`).
        // A subtitle file is not a source and lands on no lane: it joins the
        // timeline's own list of them, which is what the library's subtitle
        // section shows and what the overlay draws. With no timeline open there
        // is nothing for the cues to be timed against, and it says so.
        if is_subtitle(path) {
            self.take_subtitles(path, subs, cx);
            return;
        }
        // The container was read on the worker and what came back is registered
        // here ([`engine::PlaybackSession::import_probed`]): no header walk, no
        // decoder open, no probe of the timeline's own first source -- the three
        // reads that used to be spent on this thread. A song and a still fork
        // before the demuxer and pay their own small read
        // ([`engine::PlaybackSession::import`]); a window whose timeline went
        // away while the worker read falls to the slow door below, which is the
        // one that can still open one.
        // A refusal that only says "no picture" is not a refusal here: the
        // slow door below re-reads the file and lands it as audio, the same
        // fallback every other door in makes.
        let no_video = matches!(&probe, Some(Err(e)) if engine::demux::NoVideoTrack::is_it(e));
        let registered = match (self.session.as_mut(), probe) {
            (Some(session), Some(Ok(probe))) => Some(session.import_probed(path, probe)),
            (Some(session), Some(Err(_))) if no_video => Some(session.import(path)),
            (Some(_), Some(Err(refused))) => Some(Err(refused)),
            (Some(session), None) => Some(session.import(path)),
            (None, _) => None,
        };
        // Whether this import is the first thing into an empty timeline, so it
        // is also placed on it once the library line below has been said.
        let first = match &self.session {
            Some(session) => session.is_empty(),
            None => true,
        };
        let joins: bool;
        let text = match registered {
            Some(Ok(_)) => {
                // The file's own subtitle tracks with it, exactly as an open
                // takes them: an import is the other door the same file arrives
                // through. The cues were read on the worker
                // ([`read_ahead`]); what happens here is the push.
                let tail = self
                    .session
                    .as_mut()
                    .and_then(|session| subtitle_tail(session, subs))
                    .unwrap_or_default();
                // A file with sound joining a session that opened silent is the
                // one import that *arms* the audio device
                // ([`engine::PlaybackSession::arm_audio`]); it comes up at full
                // volume, so the player's own level and mute are pushed at it
                // again here or a muted window would play.
                self.apply_volume();
                joins = first;
                let landing = match first {
                    true => " and the timeline",
                    false => "",
                };
                format!(
                    "Imported {} to the library{landing}{tail}{}",
                    file_name(path),
                    audio_import_tail(path, self.has_video(path) && !no_video)
                )
            }
            // Named, because two files can fail in one launch and the queue now
            // shows both: "No such file or directory" twice over, with nothing
            // saying which file, is two messages that answer nothing.
            // The engine's words stay on stderr; the window says what the user can do about it.
            Some(Err(e)) => {
                joins = false;
                eprintln!("import of {} failed: {e}", path.display());
                let name = file_name(path);
                match path.exists() {
                    false => format!("Couldn't import {name} \u{2014} the file isn't there."),
                    true => format!("{name} isn't a video, audio or image file edith can read."),
                }
            }
            None => {
                let text = self.open_media(path, false, subs);
                // A window with no session that opened one has an empty
                // timeline under it, which is the case the first file fills.
                joins = self.session.is_some();
                text
            }
        };
        eprintln!("{text}");
        self.notify_loaded(text);
        cx.notify();
        // Placed after the line above, so a refusal from the insert is the last
        // word on the notice. The insert is one undo step (`place` snapshots
        // the project); the import that stands behind it is not.
        if joins {
            self.insert_source(path, 0, None, None, cx);
            if self
                .session
                .as_ref()
                .is_some_and(|session| !session.is_empty())
            {
                self.zoom_fit(cx);
            }
        }
        // A new project's first footage is in: the file it was named for is written now.
        if std::mem::take(&mut self.new_project_fresh) {
            self.save_project(cx);
        }
    }

    /// Takes a file as the session an empty window is waiting for. Everything
    /// derived from the media -- the clock, the title, where an export and a
    /// save go -- is set here, exactly as a launch with a file argument sets
    /// it. Paused with its first frame showing, like every other way a timeline
    /// arrives.
    ///
    /// `place` is the difference between the two doors that come here: a file
    /// *opened* is the timeline, one *imported* into an empty window fills the
    /// library and leaves the lanes empty for a drag.
    pub(crate) fn open_media(&mut self, path: &std::path::Path, place: bool, subs: Subs) -> String {
        self.install_media(path, open_session(path, place, subs), place)
    }

    /// The second half of it: everything the window derives from a session that
    /// has already been opened. Split from the open itself because the file
    /// argv named is opened on a worker ([`open_ahead`]) -- the twelve seconds
    /// of a cold header walk are not the UI thread's to spend -- and lands
    /// here, where nothing is read and nothing blocks.
    pub(crate) fn install_media(
        &mut self,
        path: &std::path::Path,
        opened: Result<(PlaybackSession, String), String>,
        place: bool,
    ) -> String {
        match opened {
            Ok((mut session, subs)) => {
                // A size and/or a rate picked before this file existed
                // ([`Player::apply_resolution`], [`Player::apply_frame_rate`])
                // are the project's from its very first frame -- the same
                // precedence a `.edith`'s own saved settings already have over
                // the scaffold ([`engine::PlaybackSession::open_project`]).
                // Consumed once: a later file replacing this session finds
                // nothing left pending.
                if let Some((w, h)) = self.pending_settings.0.take() {
                    session.set_resolution(w, h);
                }
                if let Some(fps) = self.pending_settings.1.take() {
                    session.set_frame_rate(fps);
                }
                if let Some(rate) = self.pending_settings.2.take() {
                    session.set_sample_rate(Some(rate));
                }
                // A project started from a template without clips takes that template's lanes,
                // names and caption style now, and its export preset once the window is set up.
                let template_export = match self.pending_template.take() {
                    Some(name) => match engine::templates::dir().map(|dir| engine::templates::apply(&dir, &name, &mut session)) {
                        Some(Ok(export)) => Some(export),
                        _ => {
                            self.notify_user(format!("The template {name} could not be read").into());
                            None
                        }
                    },
                    None => None,
                };
                self.fps = session.meta().frame_rate;
                // This window *is* the real-time viewer: a picture already past
                // the clock is one `pump` takes off the channel and throws away,
                // so the worker stops paying for it instead
                // ([`PlaybackSession::drop_late_pictures`]).
                session.drop_late_pictures(true);
                // Read before the session moves: a file that plays silent says
                // so here or nowhere.
                let silent = audio_notice(&session);
                // A file replaces the one that was open, and track 3 of that one
                // is not track 3 of this.
                self.sub_track = 0;
                crate::export_ui::gui_default_seat(&mut session);
                self.session = Some(session);
                self.saved_revision = None;
                self.session_swapped();
                // A fresh session comes up at full volume; the player's own
                // setting outlives the file, so it is pushed at every new one.
                self.apply_volume();
                // ...and so do the stand-in switches, which were answerable
                // before this file existed and decide what its own import does.
                self.apply_proxies();
                self.project_path = match self.new_project.take() {
                    // A project started from File > New > Project…: the footage arriving is its
                    // first, and the file the person named is written as soon as it is placed.
                    Some(target) => {
                        self.new_project_fresh = true;
                        target
                    }
                    None => project_path(path),
                };
                // Derived, not opened: `project_path` here is a guess at where
                // a save would land, not a file this window has any business
                // writing an autosave beside until a manual save earns it.
                self.autosave_armed = false;
                // An autosave left by an earlier, unsaved session of this media is
                // unsaved work too: offered like a project's newer autosave.
                self.recovery_sidecar = Self::recovery_offer(&self.project_path, false);
                self.name = match self.new_project_fresh {
                    true => file_name(&self.project_path).trim_end_matches(".edith").to_string().into(),
                    false => "Untitled".into(),
                };
                self.reset_export_default();
                if let Some(export) = template_export {
                    self.apply_template_export(export.as_deref());
                }
                self.reset_after_reseek();
                let name = file_name(path);
                // The library is filled and the timeline is empty; the only
                // thing that says so is this line, so it says what to do next.
                let what = match place {
                    true => format!("Opened {name}"),
                    false => {
                        format!("Imported {name} to the library")
                    }
                };
                let audio = match place {
                    true => String::new(),
                    false => audio_import_tail(path, self.has_video(path)),
                };
                format!("{what}{}{subs}{audio}", silent.unwrap_or_default())
            }
            Err(_) => self.open_refused(path),
        }
    }

    /// What a refused open says, in plain words, and the window's name put back:
    /// a launch that named a file the window never got to open must not stay
    /// titled with it.
    fn open_refused(&mut self, path: &std::path::Path) -> String {
        if self.session.is_none() {
            self.name = NO_FILE.into();
        }
        let name = file_name(path);
        match (path.exists(), path.extension().is_some_and(|e| e.eq_ignore_ascii_case("edith"))) {
            (false, _) => format!("Couldn't open {name} \u{2014} the file isn't there."),
            // A project that will not parse is a damaged one; any other file is media edith cannot read.
            (true, true) => format!("{name} is damaged or incomplete and can't be opened."),
            (true, false) => format!("{name} isn't video, audio or an image edith can read."),
        }
    }

    /// File > Export timeline as: writes the active sequence in `ext` ("otio", "fcpxml" or
    /// "edl") to the default export folder, named like an export of this project. No chooser;
    /// the toast says where it went.
    pub(crate) fn export_timeline(&mut self, ext: &'static str, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            self.notify_user("No timeline to export: no project is open".into());
            cx.notify();
            return;
        };
        let timeline = session.timeline_snapshot();
        let text = match ext {
            "otio" => engine::interchange::otio::write(&timeline),
            "fcpxml" => engine::interchange::fcpxml::write(&timeline),
            _ => engine::interchange::edl::write(&timeline),
        };
        let sources: Vec<&Path> = session.sources().iter().map(|s| s.path.as_path()).collect();
        let first_clip = sources.first().map(|path| crate::files::stem(path));
        let path = default_path_with_ext(
            &self.project_path,
            self.autosave_armed || self.new_project_fresh,
            first_clip.as_deref(),
            &sources,
            ext,
        );
        let text = match std::fs::write(&path, text) {
            Ok(()) => format!("Timeline exported to {}", path.display()),
            Err(e) => format!("Couldn't export the timeline to {}: {e}", path.display()),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// Reads an OpenTimelineIO file and makes it a new sequence of the open project, which
    /// becomes the active one. Each file it names is taken in as a library file is. One undo step.
    pub(crate) fn import_timeline_file(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let text = match self.session.as_ref() {
            None => "No timeline imported: no project is open".to_string(),
            Some(session) => {
                let fps = session.timeline_frame_rate();
                let read = std::fs::read_to_string(path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| engine::interchange::otio::read(&t, fps).map_err(|e| e.to_string()));
                match (read, self.session.as_mut()) {
                    (Ok(timeline), Some(session)) => match session.import_timeline(&timeline) {
                        Ok(_) => format!("Imported {} as a new sequence", timeline.name),
                        Err(e) => format!("Couldn't import {}: {e}", file_name(path)),
                    },
                    (Err(e), _) => format!("Couldn't import {}: {e}", file_name(path)),
                    (Ok(_), None) => "No timeline imported: no project is open".to_string(),
                }
            }
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// The Import button: asks the desktop for a path and takes it the same way
    /// a drop would. The chooser is another process and the user may sit in it,
    /// so it runs on a background thread -- blocking here would freeze the
    /// window behind the dialog.
    pub(crate) fn pick_and_import(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("Import media") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                // One queue, and the fork is made when its worker starts
                // ([`arrival`]): a project replaces the timeline, media joins
                // the library, and neither is read on this thread.
                Ok(Some(path)) => this.import(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                // No chooser here: the same question, typed.
                Err(_) => this.open_path_entry(PathFor::Import, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The dock's "Paste path" verb (MOCK-SPEC "Dock" IMPORT section): reads
    /// whatever text sits on the OS clipboard and imports it the same way a
    /// drop or the Import button would, if it names a file that exists.
    /// `gpui::App::read_from_clipboard` already covers this -- no new
    /// dependency for what is otherwise a clipboard read and the existing
    /// [`Player::import`] door.
    pub(crate) fn paste_file_path(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.notify_user("Copy a file's path first".into());
            cx.notify();
            return;
        };
        let text = text.trim().trim_matches(['"', '\'']);
        let path = std::path::PathBuf::from(text);
        if !path.is_file() {
            self.notify_user(format!("No file at {text}").into());
            cx.notify();
            return;
        }
        self.import(&path, cx);
    }

    /// The `+ S` button and its key: asks the desktop for a file and takes the
    /// subtitle tracks out of it -- a standalone `.srt`/`.vtt`/`.ass` is one of
    /// them, a Matroska however many are inside. Only the subtitles: the file
    /// itself does not join the library, which is what the Import button beside
    /// this one is for.
    ///
    /// The chooser is another process and the user may sit in it, so it runs on
    /// a background thread, exactly as [`Player::pick_and_import`] does.
    pub(crate) fn pick_and_add_subtitles(&mut self, cx: &mut Context<Self>) {
        // What dims the `+ S` button, asked here as well so the key answers the
        // same question -- and *before* the chooser rather than after it: a door
        // that opens a dialog, waits for a file and only then says the timeline
        // was never there is the second door disagreeing with the first.
        if let Some(why) = self.enable(ActionId::ImportSubtitles, None).why() {
            let text = format!("No subtitles added: {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        let picked = cx
            .background_executor()
            .spawn(async { pick_file("edith — subtitles to add") });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.add_subtitles(&path, cx),
                // Cancelled: the user already knows what happened.
                Ok(None) => {}
                Err(_) => this.open_path_entry(PathFor::Subtitles, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Takes a file's subtitle tracks onto the timeline, off the render thread.
    /// The walk reads the whole container for its cues
    /// (`engine::PlaybackSession::parse_subtitles`) -- ~200 ms on a two-hour 4K
    /// remux and 1.3 s on a cold 3 GB one -- and a button that costs the window
    /// that many frames is a button that freezes it. So the *parse* is the
    /// worker's, whole, and the UI thread only pushes what came back
    /// ([`PlaybackSession::add_subtitle_tracks`]): no borrow crosses the await,
    /// because the parse is an associated fn that owns nothing.
    pub(crate) fn add_subtitles(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        // Nothing to time the cues against: said now rather than after a walk
        // of a 25 GB file that was never going to be kept.
        if self.session.is_none() {
            self.landed_subtitles(path, None, cx);
            return;
        }
        self.notify_user(format!("Reading {} for subtitles…", file_name(path)).into());
        let parsed = cx.background_executor().spawn({
            let path = path.to_path_buf();
            async move { engine::PlaybackSession::parse_subtitles(&path) }
        });
        let path = path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let parsed = parsed.await;
            this.update(cx, |this, cx| {
                // The dedupe lives inside the push, so a second `+ S` on the
                // same file still answers 0 and still says so below.
                let added = this
                    .session
                    .as_mut()
                    .map(|session| parsed.and_then(|tracks| pushed(session, &path, tracks)));
                this.landed_subtitles(&path, added, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Every subtitle track a `.srt`/`.vtt`/`.ass` carries, onto the timeline
    /// and nowhere else: they are not clips and land on no lane. The cues came
    /// off the worker that read the file ([`read_ahead`]), like every other
    /// door's do, and what is left here is the push. The engine dedupes by
    /// (file, track), so the same `.srt` twice is one row and says so.
    pub(crate) fn take_subtitles(
        &mut self,
        path: &std::path::Path,
        subs: Subs,
        cx: &mut Context<Self>,
    ) {
        let added = self
            .session
            .as_mut()
            .map(|session| subs.and_then(|tracks| pushed(session, path, tracks)));
        self.landed_subtitles(path, added, cx);
    }

    /// What the timeline says once the tracks are on it, whichever worker did
    /// the reading: the `+ S` button and its key ([`Self::add_subtitles`]), a
    /// dropped or imported subtitle file ([`Self::take_subtitles`]), and a
    /// window with nothing to time cues against all word the outcome here,
    /// once, so no two doors can drift apart.
    pub(crate) fn landed_subtitles(
        &mut self,
        path: &std::path::Path,
        added: Option<engine::Result<usize>>,
        cx: &mut Context<Self>,
    ) {
        let text = match added {
            Some(Ok(0)) => format!("{}'s subtitles are in the palette already", file_name(path)),
            // Where they are and what to do with them: the tracks are in the
            // palette and nothing is placed until one is dragged onto an S
            // track, which is the whole of what this door does.
            Some(Ok(n)) => format!(
                "Subtitles {}: {} in the palette — drag one onto an S track ({} adds \
                 one), showing over the picture meanwhile, {} hides them",
                file_name(path),
                count_noun(n, "track", "tracks"),
                self.keymap.display(ActionId::AddSubtitleLane),
                self.keymap.display(ActionId::ToggleSubtitles)
            ),
            Some(Err(e)) => format!("Subtitle import failed: {e}"),
            None => "No subtitles added: no file open".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// The × on a subtitle row, and the stroke that takes the picked one off:
    /// the track leaves the timeline and the pick moves with it. Every index
    /// past the one that went moves down
    /// ([`engine::Project::remove_subtitles`]), so a pick left where it was
    /// would name a *different* track -- and the pick is what an export writes
    /// into the file.
    ///
    /// One undo step, and the palette comes back with the reindex
    /// ([`engine::Project::remove_subtitles`]), so `^z` is a way back that
    /// works -- [`Player::step_history`] moves the pick back with it and drops
    /// the plate the old index drew. The second way back, for a caller that
    /// wants the *file's* tracks again, is putting them on
    /// ([`Player::pick_and_add_subtitles`]): a door of its own that reads the
    /// subtitles alone, never the media, which is what the notice names.
    pub(crate) fn remove_subtitle_track(&mut self, track: usize, cx: &mut Context<Self>) {
        // The one availability oracle, for the same reason the × on a row and
        // the stroke are one call: an empty list is not a failure, it is an
        // action with nothing to act on, and the engine's "there is no subtitle
        // track 0" is an index nobody typed. A real removal that fails still
        // says what the engine said, below.
        // The palette's list is what this takes a row off, and an empty list is
        // the very fact the toggle is refused by -- so the same arm answers
        // both (oracle.rs) rather than a second reading of the same state.
        if let Some(why) = self.enable(ActionId::ToggleSubtitles, None).why() {
            let text = format!("No subtitles removed: {why}");
            eprintln!("{text}");
            self.notify_user(text.into());
            cx.notify();
            return;
        }
        // Read before it goes: a notice naming an index names nothing.
        let name = self
            .session
            .as_ref()
            .and_then(|session| sub_pick_name(session.subtitles(), track))
            .unwrap_or_else(|| format!("subtitle track {track}"));
        let text = match self
            .session
            .as_mut()
            .map(|session| session.remove_subtitles(track))
        {
            Some(Ok(())) => {
                let left = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.subtitles().len());
                self.sub_track = sub_pick_after_removal(self.sub_track, track, left);
                // The cue over the picture goes with the pick: its tile is
                // keyed by *where* the cue sits ([`Player::sub_picture`]'s
                // `(lane, start_us)`) and never by the track it shows, so the
                // index that moved here is not something that cache can be
                // asked about. Conservative, and the same drop a history step
                // makes for the same reason ([`Player::step_history`]).
                //
                // corner-cut: its atlas tile is not released -- `close_session`'s
                // note, for its reason and with its upgrade path.
                self.sub_image = None;
                format!(
                    "{name} removed: {} puts a file's subtitles back in the palette, the file \
                     itself stays off",
                    self.keymap.display(ActionId::ImportSubtitles)
                )
            }
            Some(Err(e)) => format!("No subtitles removed: {e}"),
            None => "No subtitles removed: no file open".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    /// Swaps the whole timeline for one restored from a `.edith`, for
    /// [`Player::install_media`]'s reason: the open is a worker's -- a project
    /// naming a 24 GB film opens that film, which is the same twenty seconds
    /// ([`arrival`] sends every `.edith` through the one queue) -- and this is
    /// what is left once it lands. Nothing is replaced until the new session is
    /// in hand, so a refusal is shown as the engine worded it and leaves what is
    /// playing alone.
    pub(crate) fn install_project(
        &mut self,
        path: &std::path::Path,
        opened: Result<PlaybackSession, String>,
        cx: &mut Context<Self>,
    ) {
        if self.exporting().is_some() {
            return;
        }
        let text = match opened {
            Ok(mut session) => {
                // A `.edith` carries its own saved size and rate, read at
                // [`engine::PlaybackSession::open_project`] -- a pick made
                // before this project loaded is not this project's.
                self.pending_settings = (None, None, None);
                self.pending_template = None;
                // Another project was opened: a new one still waiting for footage is given up.
                self.new_project = None;
                self.new_project_fresh = false;
                self.fps = session.meta().frame_rate;
                // As at the file door above: the window watches in real time.
                session.drop_late_pictures(true);
                let silent = audio_notice(&session);
                crate::export_ui::gui_default_seat(&mut session);
                self.session = Some(session);
                self.session_swapped();
                self.apply_volume();
                // The other way round from every other door: a project carries
                // its own two switches ([`engine::edith`]) and they are what it
                // was saved with, so the window takes *them* rather than
                // pushing what it had -- and the buttons say what was loaded.
                if let Some(session) = &self.session {
                    self.proxies_on = session.proxies();
                    self.auto_proxies_on = session.auto_proxies();
                }
                self.project_path = path.to_path_buf();
                crate::recent::push(path);
                // Opened from the `.edith` itself: the file this window will
                // autosave beside is the one the user actually chose.
                self.autosave_armed = true;
                self.disk_known = crate::follow::stamp(path);
                self.edited_since_save = false;
                self.saved_revision = self.revision_now();
                self.name = file_name(path).into();
                self.reset_export_default();
                // A copied clip names its source by index, which means a
                // different file -- or none -- in another project.
                self.clipboard.clear();
                self.selected.clear();
                // A menu can be up while a project is dropped on the window --
                // the scrim swallows clicks, never a drop -- and its index
                // would name some other timeline's clip. The two clip cards
                // hold a (lane, idx) of the old timeline for the same reason.
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                self.transform_open = None;
                // Marks are timeline frames of the timeline that was.
                self.close_silence();
                // A different set of sources: the row that was picked is not
                // the file that index names any more -- and neither is the
                // subtitle track that was showing.
                self.selected_asset = None;
                self.asset_picks.clear();
                self.sub_track = 0;
                // The counters describe one timeline; the eof line must not
                // report the old one's frames against the new one.
                self.displayed = 0;
                self.dropped = 0;
                self.started = None;
                // Loaded paused at its saved playhead, so the still it owes
                // reaches the screen the way a seek's does. The old picture is
                // released by the swap in `pump`, as after any other seek.
                self.reset_after_reseek();
                // A saved view is the project's own place: it is set after the
                // reseek (which resets the hand's flag) and wins over the
                // first-import fit, so a project with none keeps today's view.
                if let Some(state) = crate::view_state::load(path) {
                    let restored = crate::view_state::restored(&state);
                    self.scale = restored.scale;
                    self.lane_h = restored.lane_h;
                    self.panned = restored.panned;
                    self.fitted_bed = restored.fitted_bed;
                    self.fit_pending = restored.fit_pending;
                    self.viewer = state.viewer;
                    self.viewer_pan = (0., 0.);
                    self.collapsed_bins = state.closed_bins.iter().copied().collect();
                    self.sections_mem = state.sections.clone();
                    self.sections_anchor = None;
                    self.waveforms = state.waveforms;
                    self.scrub_audio = state.scrub_audio;
                } else {
                    self.viewer = crate::viewer_view::Prefs::default();
                    self.viewer_pan = (0., 0.);
                    self.collapsed_bins.clear();
                    self.sections_mem.clear();
                    self.sections_anchor = None;
                    self.waveforms = true;
                    self.scrub_audio = true;
                }
                // Offered right after the load it is about: `notify_loaded` puts the
                // question on its own sticky toast, under the "Loaded" line.
                self.recovery_sidecar = Self::recovery_offer(path, true);
                format!("Loaded {}{}", file_name(path), silent.unwrap_or_default())
            }
            Err(_) => self.open_refused(path),
        };
        eprintln!("{text}");
        self.notify_loaded(text);
        cx.notify();
    }

    /// Says what a load did. A recovery on offer is its own sticky toast with
    /// Recover and Discard: it stays on the bar until one of them is pressed, so
    /// the question cannot time out under the person's nose.
    fn notify_loaded(&mut self, text: String) {
        self.notify_user(text.into());
        if let Some(sidecar) = self.recovery_sidecar.clone() {
            push_sticky_toast(
                &mut self.notices,
                recovery_words(&sidecar).into(),
                Tone::Info,
                vec![ToastAction::Recover, ToastAction::Discard],
            );
        }
    }

    /// Takes the recovery question off the bar: the one the recovery answers
    /// have just been given to, or that a save made moot.
    fn drop_recovery_toasts(&mut self) {
        self.notices
            .retain(|toast| !toast.actions.contains(&ToastAction::Recover));
    }

    /// Whether a close has to be answered instead of obeyed: unsaved edits on
    /// the timeline, or an export that has not finished.
    pub(crate) fn close_needs_answer(&self) -> bool {
        self.close_dirty() || self.queue.unfinished() > 0
    }

    /// Unsaved work on the timeline: the save half of the close question. It
    /// reads `edited_since_save` (a real save clears it), never the autosave's
    /// flag: an autosave is crash recovery, not saving, so an edit the autosave
    /// has already written still asks on close.
    pub(crate) fn close_dirty(&self) -> bool {
        let clips = self.session.as_ref().map_or(0, |session| {
            session
                .lanes()
                .into_iter()
                .map(|lane| session.lane_clips(lane).len())
                .sum()
        });
        Self::close_asks(clips, self.edited_since_save)
    }

    /// The close question as a value: clips on the timeline and an edit since
    /// the last real save. Split out of [`Player::close_dirty`] so the rule can
    /// be read and tested without a window or a session. An empty timeline
    /// closes the way it always did.
    pub(crate) fn close_asks(clips: usize, edited: bool) -> bool {
        clips > 0 && edited
    }

    /// Whether a save has to be answered instead of obeyed: `project_path`
    /// names a file on disk that this window itself did not put there --
    /// neither loaded ([`Player::install_project`]) nor written
    /// ([`Player::save_project`]) -- so the first ^s would clobber a
    /// `.edith` this timeline only derives its name from beside an
    /// imported media file (user 2026-09-11). `autosave_armed` is the
    /// whole difference: once this window's own save has confirmed the
    /// overwrite, later saves on the file are silent again.
    pub(crate) fn save_needs_answer(&self) -> bool {
        Self::save_asks(self.project_path.exists(), self.autosave_armed)
    }

    /// The overwrite question as a value: a target on disk and a window
    /// that has not earned the write. Split out of
    /// [`Player::save_needs_answer`] so the rule can be read and tested
    /// without a window or a session, the way [`Player::close_asks`] is.
    pub(crate) fn save_asks(target_exists: bool, armed: bool) -> bool {
        target_exists && !armed
    }

    /// Writes the timeline back to its project file -- the confirmed write.
    /// The overwrite question, when one is owed at all, was asked by Save's
    /// door ([`Player::save_guarded`]) or already answered at the quit
    /// plate; the notice here is the confirmation, not the permission.
    /// Returns whether the file was written, so a close that is answered with
    /// Save only goes once the work is actually on disk.
    pub(crate) fn save_project(&mut self, cx: &mut Context<Self>) -> bool {
        let saved = self
            .session
            .as_ref()
            .map(|session| session.save_project_noted(&self.project_path));
        let written = matches!(saved, Some(Ok(_)));
        let mut shortened = Vec::new();
        let text = match saved {
            Some(Ok(notes)) => {
                shortened = notes;
                // The manual save is now the newest copy of the timeline, so
                // the sidecar's job is done -- left behind it would outlive
                // its use and, if it later grew younger than a *later* manual
                // save through some clock oddity, could be offered as
                // "recovery" over a file that already holds everything it did.
                for armed in [true, false] {
                    if let Some(autosave) = Self::autosave_target(&self.project_path, armed) {
                        let _ = std::fs::remove_file(autosave);
                    }
                }
                self.autosave_dirty = false;
                self.autosave_edits = 0;
                // The file now holds everything the offered autosave did: the
                // question is answered by the save itself.
                self.recovery_sidecar = None;
                self.drop_recovery_toasts();
                self.save_view_state();
                // A manual save is exactly the event this flag exists to
                // require: whatever `project_path` was derived from, it now
                // names a file the user just chose to write.
                // The export's default follows the project to where it was just saved, unless
                // the person had already chosen one.
                let was_default = self.export_path == self.default_export_target();
                self.autosave_armed = true;
                if was_default || self.export_path.as_os_str().is_empty() {
                    self.reset_export_default();
                }
                // Our own write: the stamp it left is what the file is now, so
                // the follow tick reads it as unchanged and not as the agent's.
                self.disk_known = crate::follow::stamp(&self.project_path);
                self.edited_since_save = false;
                self.saved_revision = self.revision_now();
                crate::recent::push(&self.project_path);
                format!("Saved {}", file_name(&self.project_path))
            }
            Some(Err(e)) => format!("Save failed: {e}"),
            None => "Nothing to save: no file open".to_string(),
        };
        eprintln!("{text}");
        self.notify_user(text.into());
        for note in shortened_notes(&shortened) {
            eprintln!("{note}");
            self.notify_user(note.into());
        }
        cx.notify();
        written
    }

    /// Drops the unsaved edits for good, as the close question's Don't save
    /// does: the autosave goes on disk and in memory, so the next open does not
    /// offer the edits back as recovery. Nothing else is touched -- the project
    /// file keeps what it had.
    pub(crate) fn discard_unsaved_edits(&mut self) {
        for armed in [true, false] {
            if let Some(autosave) = Self::autosave_target(&self.project_path, armed) {
                Self::discard_sidecar(&autosave);
            }
        }
        if let Some(sidecar) = self.recovery_sidecar.take() {
            Self::discard_sidecar(&sidecar);
        }
        self.drop_recovery_toasts();
        self.autosave_dirty = false;
        self.autosave_edits = 0;
        self.edited_since_save = false;
        self.saved_revision = self.revision_now();
    }

    /// Keeps the view the project is left in, beside the project's hash
    /// ([`crate::view_state`]). Best effort: a view that fails to write costs
    /// the next open its place and nothing else. A project with no file yet
    /// (its first import has not been saved) has nothing to key on.
    pub(crate) fn save_view_state(&self) {
        if self.session.is_none() || !self.project_path.is_file() {
            return;
        }
        let state = crate::view_state::ViewState {
            pps: self.scale.pps,
            start: self.scale.start,
            lane_h: self.lane_h.clone(),
            viewer: self.viewer,
            closed_bins: {
                let mut ids: Vec<u32> = self.collapsed_bins.iter().copied().collect();
                ids.sort_unstable();
                ids
            },
            sections: self.sections_mem.clone(),
            waveforms: self.waveforms,
            scrub_audio: self.scrub_audio,
        };
        let _ = crate::view_state::save(&self.project_path, &state);
    }

    /// Save's door: asks before the first overwrite of a file this window
    /// never put there, saves straight through once the file is the
    /// window's own. Raising the question does not save -- the overwrite
    /// plate (the root `on_key_down` in `ui::stance`) owns the keyboard
    /// until it is answered, and only its `enter` comes back to
    /// [`Player::save_project`], which is what arms `autosave_armed` and
    /// makes every later save on the file silent.
    pub(crate) fn save_guarded(&mut self, cx: &mut Context<Self>) {
        if self.save_needs_answer() {
            self.save_ask = true;
            cx.notify();
            return;
        }
        self.save_project(cx);
    }

    /// Marks that an edit has landed since the sidecar last caught up, and
    /// when: the door every edit that funnels through [`Player::act`] or a
    /// timeline gesture calls on its way through. Cheap and safe to call on a
    /// refusal too -- an autosave a beat early costs nothing a real edit
    /// would not have earned anyway.
    pub(crate) fn mark_dirty(&mut self) {
        // An edit can change the picture as shot at the frame on screen, so the compare view reads it again.
        self.before_at = None;
        self.autosave_dirty = true;
        self.edited_since_save = true;
        self.autosave_edits = self.autosave_edits.saturating_add(1);
        // An edit made while the recovery is on offer: the offer now says what
        // recovering costs, since the edits it would replace are real. `act`
        // calls this before an edit lands, so the revision is not read here.
        if self.recovery_sidecar.is_some() {
            self.word_recovery_replaces();
        }
    }

    /// An edit that changes what the window is (a sequence made, copied, opened or nested, a
    /// marker dropped): the autosave is written soon after it, not at the next 20 edits, so a
    /// crash right after it loses nothing. Throttled by [`AUTOSAVE_THROTTLE`].
    pub(crate) fn mark_structural(&mut self) {
        self.mark_dirty();
        self.autosave_urgent = true;
    }

    /// The engine revision of the timeline as it stands, `None` with no session.
    pub(crate) fn revision_now(&self) -> Option<u64> {
        self.session.as_ref().map(PlaybackSession::revision)
    }

    /// The mark an undo or a redo leaves, after [`Player::mark_dirty`]: the
    /// window is edited unless the timeline it landed on is the saved one. Undo
    /// and redo are the only steps that can walk back to the saved timeline, so
    /// this is the one place the flag is cleared by an edit's door.
    pub(crate) fn settle_saved_point(&mut self) {
        self.edited_since_save = self.revision_now() != self.saved_revision;
    }

    /// Puts the recovery's cost on its own toast, in place of the age-only line,
    /// once the timeline has moved off the saved one.
    fn word_recovery_replaces(&mut self) {
        if self.recovery_sidecar.is_none() {
            return;
        }
        let text: SharedString = recovery_replaces_words().into();
        if let Some(toast) = self
            .notices
            .iter_mut()
            .find(|toast| toast.actions.contains(&ToastAction::Recover))
        {
            toast.text = text;
        }
    }

    /// Where the autosave for `project_path` goes. Beside a project this window
    /// opened or saved (`armed`), as `<stem>.autosave.edith`, the name
    /// `edith_mcp` reads too. An untitled session (`armed` false) has a
    /// `project_path` that is only a guess at where a save would land, and
    /// that file may be somebody's real project, so its autosave goes to the
    /// state directory instead, one file per guessed path. `None` when there is
    /// no path at all.
    pub(crate) fn autosave_target(project_path: &Path, armed: bool) -> Option<PathBuf> {
        if project_path.as_os_str().is_empty() {
            return None;
        }
        let stem = project_path
            .file_stem()
            .map_or_else(|| "untitled".to_string(), |s| s.to_string_lossy().into_owned());
        if armed {
            return Some(project_path.with_file_name(format!("{stem}.autosave.edith")));
        }
        // FNV-1a over the path's bytes: a name that stays the same across builds,
        // which `DefaultHasher` does not promise.
        let hash = project_path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3));
        Some(Self::autosave_state_dir()?.join(format!("{stem}-{hash:016x}.autosave.edith")))
    }

    /// `$XDG_STATE_HOME/edith`, or `~/.local/state/edith` when that is unset or
    /// not an absolute path, as the XDG base directory spec says.
    pub(crate) fn autosave_state_dir() -> Option<PathBuf> {
        let state = std::env::var_os("XDG_STATE_HOME")
            .filter(|v| !v.is_empty() && Path::new(v).is_absolute())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
            })?;
        Some(state.join("edith"))
    }

    /// The Recover answer to the recovery notice: the autosave's unsaved edits
    /// go on top of the saved project ([`recovered_session`]), and the window's
    /// own timeline as it stands is kept one undo step back, so the recovery
    /// replaces the edits made since the file was opened without losing them:
    /// undo brings them back ([`PlaybackSession::adopt_as_step`]). Nothing here
    /// writes the project file: the next thing to touch it is still only a
    /// manual save.
    pub(crate) fn recover_from_sidecar(&mut self, cx: &mut Context<Self>) {
        let Some(sidecar) = self.recovery_sidecar.clone() else {
            return;
        };
        let project_path = self.project_path.clone();
        // An armed window's saved file is the base the autosave goes on top of;
        // an untitled one has no saved file to be the base.
        let armed = self.autosave_armed;
        let opened = match recovered_session(armed.then_some(project_path.as_path()), &sidecar) {
            Ok(recovered) => match self.session.take() {
                // The window's own timeline becomes the step back: its history
                // stays, and its current state is one undo step away.
                Some(mut current) => {
                    let mark = current.step_mark();
                    if current.adopt_as_step(&recovered) {
                        current.name_step(mark, RECOVER_STEP);
                        Ok(current)
                    } else {
                        self.session = Some(current);
                        Err("the recovered edits do not fit on the edits made here as one step".to_string())
                    }
                }
                None => Ok(recovered),
            },
            Err(e) => Err(e),
        };
        if let Err(e) = opened {
            // Nothing was taken or dropped: the window is as it was, and the
            // autosave stays on disk for another try or for Discard. Said in a
            // dialog, since the reason can be a sentence long.
            let kept = if self.edited_since_save {
                "Your edits in this window are still here"
            } else {
                "Your saved version is open"
            };
            self.recovery_failure = Some(format!("Reason: {e}. {kept}. Nothing was deleted."));
            cx.notify();
            return;
        }
        // The saved timeline is the file's, not the one recovery stands on:
        // install_project would take the new session's as saved, so keep ours.
        let saved_revision = self.saved_revision;
        // The export's own default is the project's, so a path the person picked
        // after opening is theirs to keep through the recovery.
        let export_path = self.export_path.clone();
        self.install_project(&project_path, opened, cx);
        self.export_path = export_path;
        self.saved_revision = saved_revision;
        // `install_project` arms the window, as any opened `.edith` does. An
        // untitled session stays unarmed: its `project_path` is still a guess.
        self.autosave_armed = armed;
        // install_project re-ran the offer on its way in and found the same
        // autosave still younger; offering it back here would be the one loop.
        self.recovery_sidecar = None;
        self.drop_recovery_toasts();
        // The recovered edits are not in the file yet, so the next autosave
        // must write them out.
        self.mark_dirty();
        self.notify_user(
            format!(
                "Recovered unsaved edits · undo {}",
                self.keymap.display(ActionId::Undo)
            )
            .into(),
        );
        cx.notify();
    }

    /// The Discard answer to the recovery notice: the autosave goes, on disk as
    /// well as in memory, so the offer does not come back on the next open.
    pub(crate) fn discard_recovery(&mut self) {
        if let Some(sidecar) = self.recovery_sidecar.take() {
            Self::discard_sidecar(&sidecar);
        }
        self.drop_recovery_toasts();
    }

    /// Deletes an autosave the person declined to recover: on disk as well as in
    /// memory. Left behind, an unread
    /// autosave from a scratch session would keep outdating the real project
    /// it sits beside and reappear as "recovery" on every open of that file
    /// (reference-editor behaviour: a declined backup is discarded, not kept
    /// around to ask again). Pure at the path level so it needs no `Player`
    /// to test -- a missing sidecar is not an error, only a no-op.
    pub(crate) fn discard_sidecar(sidecar: &Path) {
        let _ = std::fs::remove_file(sidecar);
    }

    /// Whether opening `project_path` should offer a recovery, and the autosave
    /// it would recover. An armed project (a `.edith` this window opened or
    /// saved) is offered only when its autosave is strictly the younger of the
    /// two. An untitled session has no saved file to be newer than, so any
    /// autosave left for it is unsaved work. Reads only the filesystem: no
    /// session, no `Player`, so a test can ask it with nothing but paths.
    pub(crate) fn recovery_offer(project_path: &Path, armed: bool) -> Option<PathBuf> {
        let sidecar = Self::autosave_target(project_path, armed)?;
        if !armed {
            return sidecar.is_file().then_some(sidecar);
        }
        let project_mtime = std::fs::metadata(project_path)
            .and_then(|m| m.modified())
            .ok()?;
        let sidecar_mtime = std::fs::metadata(&sidecar)
            .and_then(|m| m.modified())
            .ok()?;
        (sidecar_mtime > project_mtime).then_some(sidecar)
    }

    /// Starts the one background loop that ticks the autosave: every second,
    /// forever, for as long as the window is up. Cheap to run idle --
    /// a tick with nothing dirty does nothing but reschedule itself -- so one
    /// loop for the window's whole life is simpler than arming and disarming
    /// a timer around every edit.
    pub(crate) fn start_autosave(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this.update(cx, |this, cx| this.autosave_tick(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Starts the loop that watches the open project file for a write from
    /// outside (the agent's, through `edith_mcp`): every half second, for as
    /// long as the window is up. A stat each tick, nothing more.
    pub(crate) fn start_follow(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this.update(cx, |this, cx| this.follow_tick(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// One look at the project file. Only a project this window opened or
    /// saved is followed: a derived path names a file the window never
    /// claimed. Acting on what [`follow_decision`] says is all this does.
    fn follow_tick(&mut self, cx: &mut Context<Self>) {
        if !self.autosave_armed || self.exporting().is_some() {
            return;
        }
        let now = crate::follow::stamp(&self.project_path);
        match follow_decision(self.disk_known, now, self.edited_since_save) {
            Follow::Wait | Follow::Idle => {}
            Follow::Adopt => self.disk_known = now,
            Follow::Reload => self.reload_from_disk(cx),
            Follow::Ask => {
                // Recorded as answered before the question is asked, so the
                // same change does not ask again on the next tick.
                self.disk_known = now;
                self.notify_user_actions(
                    "Project changed on disk".into(),
                    vec![ToastAction::Reload, ToastAction::KeepMine],
                );
                cx.notify();
            }
        }
    }

    /// Reloads the open project from its file, in place. The timeline becomes
    /// the file's; the playhead stays where it was (clamped to the file's own
    /// length); the view (zoom, scroll, lane heights) stays as it was. Play
    /// stays paused, as a load always leaves it. Undo history and the selection
    /// belong to the old timeline and go with it.
    pub(crate) fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() || !self.autosave_armed || self.session.is_none() {
            return;
        }
        let path = self.project_path.clone();
        let at = self.playhead(self.drawn_duration());
        let scale = self.scale;
        let lane_h = self.lane_h.clone();
        let (panned, fitted_bed, fit_pending) =
            (self.panned, self.fitted_bed, self.fit_pending);
        let before = self.notices.back().map(|toast| toast.id);
        let opened = PlaybackSession::open_project(&path).map_err(|e| e.to_string());
        let ok = opened.is_ok();
        self.install_project(&path, opened, cx);
        // Recorded whether or not the open took: a file that will not open is
        // answered once, not retried on every tick.
        self.disk_known = crate::follow::stamp(&path);
        if !ok {
            return;
        }
        // install_project's own "Loaded" line is replaced by the one that says
        // what happened here. Taken off only if install pushed one.
        if self.notices.back().map(|toast| toast.id) != before {
            self.notices.pop_back();
        }
        self.recovery_sidecar = None;
        self.scale = scale;
        self.lane_h = lane_h;
        // A seek takes the view back to the playhead; the view is put back
        // after it, so the reload keeps where the person was looking.
        let dur = self.drawn_duration();
        self.seek(at.clamp(0., dur), cx);
        (self.panned, self.fitted_bed, self.fit_pending) = (panned, fitted_bed, fit_pending);
        self.notify_user("Reloaded — changed on disk".into());
        cx.notify();
    }

    /// The reload asked for by the menu or the palette. Unsaved edits are not
    /// dropped on a click: they get the same question a changed file does.
    pub(crate) fn reload_request(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() || !self.autosave_armed || self.session.is_none() {
            return;
        }
        if self.edited_since_save {
            self.notify_user_actions(
                "Reload would drop unsaved edits".into(),
                vec![ToastAction::Reload, ToastAction::KeepMine],
            );
            cx.notify();
            return;
        }
        self.reload_from_disk(cx);
    }

    /// Whether a tick is allowed to write at all: dirty and not mid-export.
    /// Pure, so a test can ask it with two bools and no `Player` to build.
    fn autosave_gate(dirty: bool, exporting: bool) -> bool {
        dirty && !exporting
    }

    /// One tick of the loop above. Writes the autosave when [`AUTOSAVE_EDITS`]
    /// edits have landed since the last write, or the session has been dirty
    /// for [`AUTOSAVE_PERIOD`] without one. Never while an export is running:
    /// the two would fight the same disk and worker for nothing an export
    /// needs. The write is [`PlaybackSession::save_autosave`]: atomic (a
    /// `.part` file, fsynced, renamed over), and it cannot refuse over a
    /// session's state the way a manual save may. Failure is a stderr line,
    /// never a notice: a modal over a lost autosave would be worse than the risk.
    fn autosave_tick(&mut self, cx: &mut Context<Self>) {
        // An autosave on offer is the unsaved work the person has not answered
        // for yet: writing this session over it would destroy that work silently.
        if self.recovery_sidecar.is_some() {
            return;
        }
        // Off writes no autosave at all: the Settings row says so.
        let Some(period) = self.autosave_secs.map(Duration::from_secs) else {
            return;
        };
        if !Self::autosave_gate(self.autosave_dirty, self.exporting().is_some()) {
            return;
        }
        let now = Instant::now();
        let overdue = self
            .autosave_last_run
            .is_none_or(|t| now.duration_since(t) >= period);
        // A structural edit is written once the throttle has let the last write go by.
        let urgent = self.autosave_urgent
            && self
                .autosave_last_run
                .is_none_or(|t| now.duration_since(t) >= AUTOSAVE_THROTTLE);
        if self.autosave_edits < AUTOSAVE_EDITS && !overdue && !urgent {
            return;
        }
        let Some(target) = Self::autosave_target(&self.project_path, self.autosave_armed) else {
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Some(dir) = target.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match session.save_autosave(&target) {
            Ok(()) => {
                self.autosave_dirty = false;
                self.autosave_edits = 0;
                self.autosave_urgent = false;
                self.autosave_last_run = Some(now);
            }
            Err(e) => eprintln!("AUTOSAVE FAILED: {e}"),
        }
        cx.notify();
    }

    /// A new empty track under the ones already there. One undo step in the
    /// engine, so the stroke that takes back an edit takes back a track too, and
    /// no reseek: nothing plays differently until something is dropped on it.
    /// The selection stays -- the lanes it indexes into have not moved.
    pub(crate) fn add_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        match &mut self.session {
            Some(session) => {
                let lane = session.add_lane(kind);
                self.notify_user(
                    format!(
                        "{} added · undo {}",
                        lane.label(),
                        self.keymap.display(ActionId::Undo)
                    )
                    .into(),
                );
            }
            None => self.notify_user("No track added: no file open".into()),
        }
        cx.notify();
    }

    /// The × in a track's header: the add taken back, one undo step, and the
    /// engine's own words when it refuses -- those name the clips still on the
    /// track, so the notice says what to delete first. A removal never deletes a
    /// clip.
    ///
    /// Everything holding a `(lane, idx)` is dropped, because the tracks below
    /// the one that went have just moved up an `ord`
    /// ([`engine::Project::remove_lane`]): a selection or an open card kept
    /// across it would be pointing at the *next* track's clip.
    pub(crate) fn remove_lane(&mut self, lane: Lane, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let removed = self
            .session
            .as_mut()
            .map(|session| session.remove_lane(lane));
        let text = match removed {
            Some(Ok(())) => {
                // The arm lives here, on the one answer that means the track
                // went: `remove_lane` refuses a track still holding clips or
                // captions, and the last of its kind, and both used to arrive
                // with the `unsaved` ghost already lit behind them.
                self.mark_dirty();
                self.selected.clear();
                self.context_menu = None;
                self.eq_open = None;
                self.color_open = None;
                self.transform_open = None;
                self.speed_open = None;
                self.effects_open = None;
                self.close_silence();
                // The shown lane with them: a `Lane` is a position among
                // its kind, so a pick now names another track -- the same
                // reason the selection and the cards go. The first lane
                // left is drawn ([`subs::active_lane`]).
                self.sub_lane = None;
                format!(
                    "{} removed — {} brings it back",
                    lane.label(),
                    self.keymap.display(ActionId::Undo)
                )
            }
            Some(Err(e)) => format!("No track removed: {e}"),
            None => "No track removed: no file open".to_string(),
        };
        self.notify_user(text.into());
        cx.notify();
    }

    /// A header let go over another header: the track in the hand takes that
    /// one's place in the stack, clips and all
    /// ([`engine::Project::move_lane`]), one undo step. The gesture every
    /// editor reorders tracks with, and the only way the order is ever changed
    /// -- there is no second list of it to keep in step.
    ///
    /// Display order is the stack, so moving a video track past another video
    /// track changes which picture wins, here and in an export alike; audio is
    /// summed and does not care, which is what makes `A1` above `V1` a purely
    /// visual arrangement. A label is a position among the tracks of its kind,
    /// so a track that crossed one of its own kind comes back under a different
    /// name -- and everything holding a `(lane, idx)` is dropped exactly then,
    /// for [`Player::remove_lane`]'s reason: those handles now name another
    /// track's clip. A move that crossed only the other kind renames nothing
    /// and keeps the selection.
    pub(crate) fn reorder_lane(&mut self, lane: Lane, onto: Lane, cx: &mut Context<Self>) {
        self.lane_drop = None;
        if self.exporting().is_some() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(to) = session.lanes().iter().position(|&l| l == onto) else {
            return;
        };
        // Picked up and put back down where it was is a click, and a click says
        // nothing -- `move_lane` refuses it and every other no-op.
        let Some(moved) = session.move_lane(lane, to) else {
            cx.notify();
            return;
        };
        self.mark_dirty();
        if moved != lane {
            self.selected.clear();
            self.context_menu = None;
            self.eq_open = None;
            self.color_open = None;
            self.transform_open = None;
            self.speed_open = None;
            self.effects_open = None;
            self.close_silence();
            // The shown lane with them: a `Lane` is a position among its
            // kind, so a pick now names another track -- the same reason
            // the selection and the cards go.
            self.sub_lane = None;
        }
        self.notify_user(
            format!(
                "{} is track {} now · undo {}",
                moved.label(),
                to + 1,
                self.keymap.display(ActionId::Undo)
            )
            .into(),
        );
        cx.notify();
    }

    /// What the remove keys act on: the last track of that kind, which is the
    /// one the matching add key appended. Nothing at all before a file is open,
    /// where the timeline drawn is a placeholder pair.
    pub(crate) fn remove_last_lane(&mut self, kind: LaneKind, cx: &mut Context<Self>) {
        let last = self
            .session
            .as_ref()
            .and_then(|session| session.lanes().into_iter().rfind(|l| l.kind == kind));
        match last {
            Some(lane) => self.remove_lane(lane, cx),
            // Two different nothings: no timeline at all, and a timeline whose
            // lanes of this kind have all gone -- which is the ordinary state of
            // the subtitle ones, since a project starts with none. "Open a file
            // first" over an open file is a notice that reads as a bug.
            None => {
                self.notify_user(
                    match self.session.is_some() {
                        true => format!(
                            "No track removed: there is no {} track to remove",
                            match kind {
                                LaneKind::Video => "video",
                                LaneKind::Audio => "audio",
                                LaneKind::Subtitle => "subtitle",
                            }
                        ),
                        false => "No track removed: no file open".to_string(),
                    }
                    .into(),
                );
                cx.notify();
            }
        }
    }
}

impl Player {
    /// Where an export goes until the person says otherwise: [`default_export_target`](Self::default_export_target),
    /// which is the format the card is set to, so a new file keeps that choice.
    pub(crate) fn reset_export_default(&mut self) {
        self.export_path = self.default_export_target();
    }

    /// The export's default target right now: [`default_export_path`] over this
    /// session's sources, the first of which names an untitled project.
    pub(crate) fn default_export_target(&self) -> PathBuf {
        let sources: Vec<&Path> = self
            .session
            .as_ref()
            .map_or_else(Vec::new, |s| s.sources().iter().map(|s| s.path.as_path()).collect());
        let first_clip = sources.first().map(|path| crate::files::stem(path));
        default_export_path(&self.project_path, self.autosave_armed || self.new_project_fresh, first_clip.as_deref(), &sources, self.format)
    }
}

/// [`Player::reset_export_default`]'s rule. A saved project is named after itself,
/// an untitled one after its first clip (`<clip> edit`), and nothing with neither is
/// "Untitled". The name never equals one of `sources`: a taken one gets " (2)", " (3)"
/// and so on until it is free.
pub(crate) fn default_export_path(
    project: &Path,
    saved: bool,
    first_clip: Option<&str>,
    sources: &[&Path],
    format: Format,
) -> PathBuf {
    default_path_with_ext(project, saved, first_clip, sources, format.ext())
}

/// [`default_export_path`] for any extension: a timeline export ([`Player::export_timeline`])
/// lands beside an export of the same project, under the same name rule.
pub(crate) fn default_path_with_ext(
    project: &Path,
    saved: bool,
    first_clip: Option<&str>,
    sources: &[&Path],
    ext: &str,
) -> PathBuf {
    let stem = match (saved, project.file_stem(), first_clip) {
        (true, Some(stem), _) => stem.to_string_lossy().into_owned(),
        (_, _, Some(clip)) => format!("{clip} edit"),
        _ => "Untitled".to_string(),
    };
    let folder = engine::export::default_export_folder(project);
    let mut n = 1;
    loop {
        let name = match n {
            1 => stem.clone(),
            n => format!("{stem} ({n})"),
        };
        let path = folder.join(format!("{name}.{ext}"));
        if !is_a_source(&path, sources) {
            return path;
        }
        n += 1;
    }
}

/// The lanes a drop below the stack needs, made in the open batch: a picture gets a video
/// lane and an audio lane of the same number, so [`PlaybackSession::place_take`] finds its
/// partner, and a sound gets an audio lane. Returns the lane the file lands on.
fn add_drop_lanes(session: &mut PlaybackSession, path: &Path) -> Lane {
    if engine::is_audio(path) {
        return session.add_lane(LaneKind::Audio);
    }
    let count = |session: &PlaybackSession, kind: LaneKind| {
        session.lanes().into_iter().filter(|l| l.kind == kind).count()
    };
    let ord = count(session, LaneKind::Video).max(count(session, LaneKind::Audio));
    while count(session, LaneKind::Video) <= ord {
        session.add_lane(LaneKind::Video);
    }
    while count(session, LaneKind::Audio) <= ord {
        session.add_lane(LaneKind::Audio);
    }
    Lane::new(LaneKind::Video, ord)
}

#[cfg(test)]
mod autosave_tests {
    use super::*;

    /// The three cases the user named: an empty timeline closes, a saved one
    /// closes, and only work that is both there and unsaved asks first --
    /// the rule the window's close request routes through.
    #[test]
    fn only_unsaved_clips_ask_before_the_window_closes() {
        assert!(!Player::close_asks(0, false));
        assert!(
            !Player::close_asks(0, true),
            "an empty timeline has nothing to lose"
        );
        assert!(
            !Player::close_asks(3, false),
            "saved work closes without a question"
        );
        assert!(Player::close_asks(1, true));
    }

    /// The recovery question reads the sidecar's own mtime, not the clock of the
    /// open: a sidecar written three minutes ago says so.
    #[test]
    fn the_recovery_question_reads_the_sidecars_real_age() {
        let dir = std::env::temp_dir().join(format!("edith-recovery-age-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("p.autosave.edith");
        std::fs::write(&sidecar, b"autosave").unwrap();
        let written = std::time::SystemTime::now() - Duration::from_secs(3 * 60 + 5);
        std::fs::File::options()
            .write(true)
            .open(&sidecar)
            .unwrap()
            .set_modified(written)
            .unwrap();
        // The clock time is the local time the file was written, so the test
        // reads it back the same way rather than naming a zone's hour.
        let clock = chrono::DateTime::<chrono::Local>::from(written).format("%H:%M").to_string();
        assert_eq!(
            recovery_words(&sidecar),
            format!("Unsaved edits from {clock} (3 minutes ago) were found.")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The recovery question names the local clock time the autosave was written
    /// and how long ago it was: "from 14:32 (1 minute ago)".
    #[test]
    fn the_recovery_question_names_the_clock_time_and_the_age() {
        let now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let words = recovery_words_at(now - Duration::from_secs(70), now);
        let clock = chrono::DateTime::<chrono::Local>::from(now - Duration::from_secs(70)).format("%H:%M").to_string();
        assert_eq!(clock.len(), 5, "HH:MM");
        assert_eq!(
            words,
            format!("Unsaved edits from {clock} (1 minute ago) were found.")
        );
        let just = recovery_words_at(now, now);
        assert!(just.contains("(a moment ago) were found"), "{just}");
    }

    /// The recovery question's age, in plain words at each step.
    #[test]
    fn the_recovery_age_reads_in_plain_words() {
        assert_eq!(age_words(0), "a moment ago");
        assert_eq!(age_words(59), "a moment ago");
        assert_eq!(age_words(60), "1 minute ago");
        assert_eq!(age_words(7 * 60), "7 minutes ago");
        assert_eq!(age_words(3600), "1 hour ago");
        assert_eq!(age_words(5 * 3600), "5 hours ago");
        assert_eq!(age_words(86_400), "yesterday");
        assert_eq!(age_words(2 * 86_400 - 1), "yesterday");
        assert_eq!(age_words(3 * 86_400), "3 days ago");
    }

    /// The overwrite question Save's door asks: a target that exists on
    /// disk but was never this window's (an import-derived `project_path`,
    /// `autosave_armed` still false) warns before the first clobber; a
    /// fresh path writes silently, and a file the window itself already
    /// saved keeps every later ^s silent. The close rule's sibling.
    #[test]
    fn only_a_foreign_existing_file_asks_before_a_save_overwrites_it() {
        assert!(Player::save_asks(true, false));
        assert!(
            !Player::save_asks(false, false),
            "nothing on disk yet -- a first save needs no warning"
        );
        assert!(
            !Player::save_asks(true, true),
            "the window's own file -- later saves stay silent"
        );
    }

    /// The three answers [`Player::recovery_offer`] gives, off nothing but a
    /// project file and a sidecar's mtimes: younger sidecar offers it, an
    /// older one or none at all offers nothing. Nowhere near a `Player` or a
    /// `PlaybackSession` -- the helper reads only the filesystem, which is
    /// what makes it askable from a test at all ([`Player`] needs a
    /// `gpui::TestAppContext` this repo has no way to build).
    #[test]
    fn recovery_offer_reads_only_the_newer_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "edith-autosave-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("p.edith");
        let sidecar = dir.join("p.autosave.edith");

        // Neither file exists yet.
        assert_eq!(Player::recovery_offer(&project, true), None);

        std::fs::write(&project, b"project").unwrap();
        // A project with no sidecar beside it: nothing to offer.
        assert_eq!(Player::recovery_offer(&project, true), None);

        std::fs::write(&sidecar, b"sidecar").unwrap();
        let now = std::time::SystemTime::now();
        // Sidecar written after the project (the ordinary case: an edit
        // landed since the last manual save) -- offered.
        std::fs::File::open(&project)
            .unwrap()
            .set_modified(now - Duration::from_secs(60))
            .unwrap();
        std::fs::File::open(&sidecar)
            .unwrap()
            .set_modified(now)
            .unwrap();
        assert_eq!(Player::recovery_offer(&project, true), Some(sidecar.clone()));

        // The sidecar left over from *before* the last manual save (the save
        // that answered it, per `Player::save_project`, deletes it -- but an
        // older one left by some other means must not be offered as newer
        // than a file it predates).
        std::fs::File::open(&project)
            .unwrap()
            .set_modified(now)
            .unwrap();
        std::fs::File::open(&sidecar)
            .unwrap()
            .set_modified(now - Duration::from_secs(60))
            .unwrap();
        assert_eq!(Player::recovery_offer(&project, true), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An untitled session's autosave never lands beside the guessed `.edith`
    /// (which may be somebody's real project): it goes to the state directory,
    /// one file per guessed path. A saved window's goes beside it, by the name
    /// `edith_mcp` reads.
    #[test]
    fn untitled_autosave_goes_to_the_state_dir_never_beside_the_guess() {
        let guessed = Path::new("/media/clips/take1.edith");
        let untitled = Player::autosave_target(guessed, false).expect("a path to autosave for");
        let state = Player::autosave_state_dir().expect("a state dir");
        assert!(untitled.starts_with(&state), "{untitled:?} is not under {state:?}");
        assert_ne!(untitled.parent(), guessed.parent());
        // The same guess names the same file every time, and another guess another one.
        assert_eq!(Player::autosave_target(guessed, false), Some(untitled.clone()));
        assert_ne!(
            Player::autosave_target(Path::new("/media/clips/take2.edith"), false),
            Some(untitled)
        );
        assert_eq!(
            Player::autosave_target(guessed, true),
            Some(PathBuf::from("/media/clips/take1.autosave.edith"))
        );
        assert_eq!(Player::autosave_target(Path::new(""), true), None);
    }

    /// Autosave writes on a dirty session and nowhere else: a clean one, or
    /// one mid-export, is left alone.
    #[test]
    fn only_a_dirty_session_outside_an_export_writes() {
        assert!(Player::autosave_gate(true, false));
        assert!(!Player::autosave_gate(true, true), "an export owns the disk");
        assert!(!Player::autosave_gate(false, false), "nothing changed");
    }

    /// Discard on the recovery notice deletes the sidecar on disk, not just the
    /// in-memory offer -- otherwise the same stale sidecar outdates the project
    /// again on the very next open.
    #[test]
    fn declining_recovery_deletes_the_sidecar() {
        let dir = std::env::temp_dir().join(format!(
            "edith-autosave-decline-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("p.autosave.edith");
        std::fs::write(&sidecar, b"stale").unwrap();

        Player::discard_sidecar(&sidecar);
        assert!(!sidecar.exists());

        // A missing sidecar (already recovered, or declined twice) is a
        // no-op, not an error.
        Player::discard_sidecar(&sidecar);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
