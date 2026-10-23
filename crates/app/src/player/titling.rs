//! Titles on the window's side: Title in the File > New menu, the Title section of the Clip tab,
//! the timeline's title row and the picture's double-click all go through here. The model is the
//! engine's ([`engine::Title`], `PlaybackSession::{add,set,remove}_title`, the very calls MCP's
//! `title_add`/`title_set`/`title_remove` make); every change here is one undo step. The words
//! are typed into a draft the viewer shows live and are written when the field is left.

use crate::player::text_field::{FieldKey, TextField};
use crate::*;
use engine::Title;
use engine::project::{MAX_TITLE_CHARS, TitlePreset as Preset};
use engine::text::{DEFAULT_SIZE, SIZE_RANGE, TextStyle};

/// How long a new title is up, in seconds.
pub(crate) const NEW_SECS: f64 = 5.0;

/// The words a new title starts with, selected so typing replaces them.
pub(crate) const NEW_TEXT: &str = "Title";

/// Which number of the selected title a drag is turning, or the block itself being moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrubField {
    Size,
    Start,
    Length,
    /// How long the entrance and exit animations take, in frames.
    AnimLength,
    /// The box's opacity, in percent.
    BoxOpacity,
    /// The title's block dragged along the timeline: `Start` in timeline pixels.
    Move,
}

/// A title number being scrubbed: where the press was, the value it had, and the value now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TitleScrub {
    pub(crate) field: ScrubField,
    pub(crate) x0: f32,
    pub(crate) v0: i64,
    pub(crate) cur: i64,
}

/// Which colour of the selected title a hex field is typing: the letters' or the box's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HexTarget {
    Ink,
    Box,
}

/// `ffcc00` or `#ffcc00` as red, green, blue.
pub(crate) fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// A colour as six lower-case hex digits.
pub(crate) fn hex_of(c: [u8; 3]) -> String {
    format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// The name a preset's chip wears in the title card.
pub(crate) fn preset_label(preset: Preset) -> &'static str {
    match preset {
        Preset::LowerThird => "Lower third",
        Preset::Centered => "Centered title",
        Preset::CaptionBar => "Caption bar",
    }
}

/// The value a scrub of `field` shows after the pointer moved `dx` px from where it was pressed.
/// A size moves a unit per 3 px, timing a frame per 4 px; the block move is timeline pixels, so
/// `pps` (px per second) turns them into frames. Held to the engine's own limits.
pub(crate) fn scrub_value(field: ScrubField, v0: i64, dx: f32, fps: f64, pps: f64, frames: u32) -> i64 {
    let by = |per_unit: f32| (dx / per_unit).round() as i64;
    let max_end = i64::from(engine::project::MAX_TITLE_END);
    match field {
        ScrubField::Size => (v0 + by(3.)).clamp(i64::from(SIZE_RANGE.0), i64::from(SIZE_RANGE.1)),
        ScrubField::Start => (v0 + by(4.)).clamp(0, max_end - i64::from(frames)),
        ScrubField::Length => (v0 + by(4.)).clamp(1, max_end),
        ScrubField::AnimLength => (v0 + by(4.)).clamp(1, i64::from(frames.max(1))),
        ScrubField::BoxOpacity => (v0 + by(2.)).clamp(0, 100),
        ScrubField::Move => {
            let moved = if pps > 0. { (f64::from(dx) / pps * fps).round() as i64 } else { 0 };
            (v0 + moved).clamp(0, max_end - i64::from(frames))
        }
    }
}

/// The words a draft writes as the title's text: trailing spaces and breaks are dropped. Blank
/// words are `None`, and the title keeps the words it had.
/// How many frames an animation takes when none was chosen: half a second at the project's rate.
pub(crate) fn anim_default_frames(fps: f64) -> u32 {
    (engine::titleanim::DEFAULT_SECS * fps).round().max(1.) as u32
}

pub(crate) fn committed_title_words(draft: &str) -> Option<String> {
    let text = draft.trim_end();
    (!text.trim().is_empty()).then(|| text.to_string())
}

impl Player {
    /// The title the Clip tab is about: the selected one, while it is still there and no clip is
    /// selected (picking a clip ends the title selection, `pick`/`select` clear it).
    pub(crate) fn title_target(&self) -> Option<(usize, Title)> {
        let idx = self.title_sel?;
        if !self.selected.is_empty() || self.sel_cue.is_some() || self.gap_pick().is_some() {
            return None;
        }
        let t = self.session.as_ref()?.titles().get(idx)?.clone();
        Some((idx, t))
    }

    /// `title` as it is on screen right now: with the words being typed and the number being
    /// scrubbed laid over what the project holds.
    pub(crate) fn shown_title(&self, title: &Title) -> Title {
        let mut t = title.clone();
        if let Some(field) = &self.title_draft
            && !field.text.trim().is_empty()
        {
            t.text = field.text.clone();
        }
        if let Some(s) = self.title_scrub {
            set_field(&mut t, s.field, s.cur);
        }
        t
    }

    /// Adds a title at the playhead, centred and five seconds long, selects it and opens its
    /// words for typing.
    pub(crate) fn new_title(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.commit_title_draft(cx);
        let Some(session) = &mut self.session else {
            self.notify_user("Nothing to add to — no file open".into());
            cx.notify();
            return;
        };
        let fps = session.meta().frame_rate;
        let want = frame_at(session.now(), fps);
        let (start, frames) = free_title_slot(session.titles(), want, ((NEW_SECS * fps).round() as u32).max(1), fps);
        let moved = start != want;
        let mut title = Title {
            start,
            frames,
            text: NEW_TEXT.into(),
            style: TextStyle { size: DEFAULT_SIZE, ..TextStyle::default() },
            fade_in: 0,
            fade_out: 0,
            in_anim: Default::default(),
            out_anim: Default::default(),
            anim_frames: 0,
        };
        title.apply_preset(Preset::Centered, fps);
        // Up at once: a fade-in would have the new words nearly invisible where the playhead is.
        (title.fade_in, title.fade_out) = (0, 0);
        match session.add_title(title) {
            Ok(idx) => {
                self.mark_dirty();
                self.open_title(idx, true, cx);
                let undo = self.keymap.display(ActionId::Undo);
                let at = match moved {
                    true => " after the one already on the playhead",
                    false => "",
                };
                self.notify_user(format!("Title added{at} · undo {undo}").into());
            }
            Err(e) => self.notify_user(e.to_string().into()),
        }
        cx.notify();
    }

    /// Selects title `idx`, shows its section in the Clip tab and, with `typing`, gives its words
    /// the keyboard with all of them selected.
    pub(crate) fn open_title(&mut self, idx: usize, typing: bool, cx: &mut Context<Self>) {
        self.commit_title_draft(cx);
        let Some(text) = self.session.as_ref().and_then(|s| s.titles().get(idx)).map(|t| t.text.clone()) else {
            return;
        };
        self.selected.clear();
        self.gap = None;
        self.drop_sel_cue();
        self.context_menu = None;
        self.close_card();
        self.keys_open = false;
        self.dock_src_active = false;
        if self.exporting().is_none() {
            self.export_open = false;
        }
        self.title_sel = Some(idx);
        self.title_draft = typing.then(|| TextField::selected(text));
        self.title_hex = None;
        cx.notify();
    }

    /// Ends the title selection (a clip was picked, the project changed). The words being typed
    /// are written first, so a click away never loses them.
    pub(crate) fn drop_title_selection(&mut self, cx: &mut Context<Self>) {
        self.commit_title_draft(cx);
        self.title_sel = None;
        self.title_scrub = None;
    }

    /// Writes the draft words as the title's text (one undo step) and gives the keyboard back.
    /// Empty words keep the old ones: a title needs some.
    pub(crate) fn commit_title_draft(&mut self, cx: &mut Context<Self>) {
        self.commit_title_hex(cx);
        let Some(field) = self.title_draft.take() else {
            return;
        };
        match committed_title_words(&field.text) {
            Some(text) => {
                self.write_title(|t| t.text = text, cx);
            }
            None => self.notify_user("A title needs words — kept the old ones".into()),
        }
        cx.notify();
    }

    /// A press on a colour field: its hex opens for typing with all of it selected.
    pub(crate) fn begin_title_hex(&mut self, target: HexTarget, cx: &mut Context<Self>) {
        self.commit_title_draft(cx);
        let Some((_, t)) = self.title_target() else {
            return;
        };
        let now = match target {
            HexTarget::Ink => t.style.ink,
            HexTarget::Box => [t.style.box_color[0], t.style.box_color[1], t.style.box_color[2]],
        };
        self.title_hex = Some((target, TextField::selected(hex_of(now))));
        cx.notify();
    }

    /// Writes the typed hex as the title's colour (one undo step) and gives the keyboard back. Not
    /// six hex digits keeps the old colour and says so.
    pub(crate) fn commit_title_hex(&mut self, cx: &mut Context<Self>) {
        let Some((target, field)) = self.title_hex.take() else {
            return;
        };
        match parse_hex(&field.text) {
            Some(rgb) => {
                self.write_title(
                    |t| match target {
                        HexTarget::Ink => t.style.ink = rgb,
                        HexTarget::Box => t.style.box_color = [rgb[0], rgb[1], rgb[2], t.style.box_color[3]],
                    },
                    cx,
                );
            }
            None => self.notify_user("A colour is six hex digits, such as ffcc00 — kept the old one".into()),
        }
        cx.notify();
    }

    /// Changes the selected title with `edit` (one undo step), following it to wherever the
    /// engine's start order puts it. `false` when nothing changed.
    pub(crate) fn write_title(&mut self, edit: impl FnOnce(&mut Title), cx: &mut Context<Self>) -> bool {
        self.named_step("Title text", |this| this.write_title_now(edit, cx))
    }

    fn write_title_now(&mut self, edit: impl FnOnce(&mut Title), cx: &mut Context<Self>) -> bool {
        let Some(idx) = self.title_sel else {
            return false;
        };
        let Some(session) = &mut self.session else {
            return false;
        };
        let Some(mut title) = session.titles().get(idx).cloned() else {
            return false;
        };
        edit(&mut title);
        if !session.set_title(idx, title.clone()) {
            return false;
        }
        self.title_sel = session.titles().iter().rposition(|t| *t == title).or(Some(idx));
        self.mark_dirty();
        cx.notify();
        true
    }

    /// Takes the selected title off the timeline (one undo step).
    pub(crate) fn remove_title_selected(&mut self, cx: &mut Context<Self>) {
        let Some(idx) = self.title_sel else {
            return;
        };
        self.title_draft = None;
        self.title_hex = None;
        self.title_scrub = None;
        if self.session.as_mut().is_some_and(|s| s.remove_title(idx)) {
            self.title_sel = None;
            self.mark_dirty();
            let undo = self.keymap.display(ActionId::Undo);
            self.notify_user(format!("Title removed · undo {undo}").into());
        }
        cx.notify();
    }

    /// Applies a one-click style to the selected title.
    pub(crate) fn apply_title_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        let fps = self.session.as_ref().map_or(25., |s| s.meta().frame_rate);
        self.write_title(|t| t.apply_preset(preset, fps), cx);
    }

    /// The keyboard while the title's words are being typed (ahead of the palette and every
    /// chord): enter commits, shift+enter breaks the line (a title can run to several), escape
    /// throws the typed words away and keeps the title's own, the rest is text.
    pub(crate) fn title_key(&mut self, key: &str, ctrl: bool, shift: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        if let Some((_, field)) = self.title_hex.as_mut() {
            let result = field.key(key, ctrl, alt, key_char, 7);
            field.text.retain(|c| c.is_ascii_hexdigit() || c == '#');
            match result {
                FieldKey::Commit => self.commit_title_hex(cx),
                FieldKey::Cancel => self.title_hex = None,
                FieldKey::Edited => {}
            }
            cx.notify();
            return;
        }
        let Some(field) = self.title_draft.as_mut() else {
            return;
        };
        match key {
            // The words go back to the title's own: nothing typed is written.
            "escape" => self.title_draft = None,
            "enter" if ctrl => self.commit_title_draft(cx),
            "enter" if shift => {
                if field.all {
                    field.text.clear();
                    field.all = false;
                }
                if field.text.chars().count() < MAX_TITLE_CHARS {
                    field.text.push('\n');
                }
            }
            "enter" => self.commit_title_draft(cx),
            _ => {
                field.key(key, ctrl, alt, key_char, MAX_TITLE_CHARS);
            }
        }
        cx.notify();
    }

    /// A press on one of the selected title's numbers (or on its block): the drag that follows
    /// turns it.
    pub(crate) fn begin_title_scrub(&mut self, field: ScrubField, x: f32, cx: &mut Context<Self>) {
        self.commit_title_draft(cx);
        let Some((_, t)) = self.title_target() else {
            return;
        };
        let v0 = get_field(&t, field);
        self.title_scrub = Some(TitleScrub { field, x0: x, v0, cur: v0 });
    }

    /// The pointer moved with a title scrub in flight: the number follows, shown live.
    pub(crate) fn title_scrub_move(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(s) = self.title_scrub else {
            return;
        };
        let (fps, frames) = self
            .title_target()
            .map_or((25., 1), |(_, t)| (self.session.as_ref().map_or(25., |s| s.meta().frame_rate), t.frames));
        let cur = scrub_value(s.field, s.v0, x - s.x0, fps, self.scale.pps, frames);
        if cur != s.cur {
            self.title_scrub = Some(TitleScrub { cur, ..s });
            cx.notify();
        }
    }

    /// The button came up: the scrubbed number is written as one undo step.
    pub(crate) fn title_scrub_end(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(s) = self.title_scrub.take() else {
            return false;
        };
        if s.cur != s.v0 {
            self.write_title(|t| set_field(t, s.field, s.cur), cx);
        }
        cx.notify();
        true
    }
}

/// One scrubbable number of a title.
fn get_field(t: &Title, field: ScrubField) -> i64 {
    match field {
        ScrubField::Size => i64::from(t.style.size),
        ScrubField::Start | ScrubField::Move => i64::from(t.start),
        ScrubField::Length => i64::from(t.frames),
        ScrubField::AnimLength => i64::from(t.anim_frames),
        ScrubField::BoxOpacity => (i64::from(t.style.box_color[3]) * 100 + 127) / 255,
    }
}

fn set_field(t: &mut Title, field: ScrubField, v: i64) {
    let v = v.max(0) as u32;
    match field {
        ScrubField::Size => t.style.size = v,
        ScrubField::Start | ScrubField::Move => t.start = v,
        ScrubField::Length => {
            t.frames = v.max(1);
            t.fade_in = t.fade_in.min(t.frames);
            t.fade_out = t.fade_out.min(t.frames);
        }
        ScrubField::AnimLength => t.anim_frames = v.clamp(1, t.frames.max(1)),
        ScrubField::BoxOpacity => t.style.box_color[3] = ((v.min(100) * 255 + 50) / 100) as u8,
    }
}

/// Where a new title goes so it never hides, or is hidden by, one already there: the playhead
/// when that is free, else just after the title it would sit on; and no longer than the room
/// before the next one (a gap under a second is skipped over).
pub(crate) fn free_title_slot(titles: &[engine::project::Title], want: u32, frames: u32, fps: f64) -> (u32, u32) {
    let min = (fps.round() as u32).max(1);
    let mut start = want;
    loop {
        if let Some(t) = titles.iter().filter(|t| t.start <= start && start < t.end()).max_by_key(|t| t.end()) {
            start = t.end();
            continue;
        }
        match titles.iter().filter(|t| t.start > start).map(|t| t.start).min() {
            Some(next) if next - start < min => start = titles.iter().filter(|t| t.start == next).map(|t| t.end()).max().unwrap_or(next),
            Some(next) => return (start, frames.min(next - start)),
            None => return (start, frames),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::text::{Align, HPlace, Outline, Place, Weight};

    fn title() -> Title {
        Title { start: 100, frames: 125, text: "Hi".into(), style: TextStyle::default(), fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 }
    }

    #[test]
    fn a_preset_keeps_words_and_timing_and_holds_fades_inside_the_title() {
        let mut t = title();
        t.apply_preset(Preset::LowerThird, 25.);
        assert_eq!((t.text.as_str(), t.start, t.frames), ("Hi", 100, 125));
        assert_eq!((t.style.place, t.style.boxed, t.fade_in, t.fade_out), (Place::Bottom, true, 13, 13));
        t.apply_preset(Preset::CaptionBar, 25.);
        assert_eq!((t.fade_in, t.fade_out, t.style.size), (0, 0, 40));
        t.frames = 10;
        t.apply_preset(Preset::Centered, 25.);
        assert_eq!((t.style.place, t.style.outline, t.fade_in), (Place::Centre, Outline::Thin, 5));
    }

    #[test]
    fn a_scrub_follows_the_pointer_and_stays_inside_the_engines_limits() {
        assert_eq!(scrub_value(ScrubField::Size, 50, 30., 25., 60., 125), 60);
        assert_eq!(scrub_value(ScrubField::Size, 50, -3000., 25., 60., 125), i64::from(SIZE_RANGE.0));
        assert_eq!(scrub_value(ScrubField::Size, 50, 3000., 25., 60., 125), i64::from(SIZE_RANGE.1));
        assert_eq!(scrub_value(ScrubField::Length, 5, -400., 25., 60., 125), 1);
        assert_eq!(scrub_value(ScrubField::AnimLength, 0, 4000., 25., 60., 125), 125);
        // 60 px/s at 25 fps: 60 px is one second, 25 frames.
        assert_eq!(scrub_value(ScrubField::Move, 100, 60., 25., 60., 125), 125);
        assert_eq!(scrub_value(ScrubField::Move, 100, -6000., 25., 60., 125), 0);
    }

    #[test]
    fn shortening_a_title_pulls_its_fades_in_with_it() {
        let mut t = title();
        t.fade_in = 40;
        t.fade_out = 60;
        set_field(&mut t, ScrubField::Length, 30);
        assert_eq!((t.frames, t.fade_in, t.fade_out), (30, 30, 30));
    }

    #[test]
    fn the_lower_third_is_bold_bottom_left_and_a_hex_colour_parses_or_is_refused() {
        let mut t = title();
        t.apply_preset(Preset::LowerThird, 25.);
        let st = &t.style;
        assert_eq!((st.place, st.hplace, st.align, st.weight, st.boxed), (Place::Bottom, HPlace::Left, Align::Left, Weight::Bold, true));
        assert_eq!(parse_hex("#FFcc00"), Some([255, 204, 0]));
        assert_eq!(hex_of([1, 2, 255]), "0102ff");
        for bad in ["fff", "ggg000", "", "ffcc000"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        let mut t = title();
        set_field(&mut t, ScrubField::BoxOpacity, 50);
        assert_eq!(t.style.box_color[3], 128);
        assert_eq!(get_field(&t, ScrubField::BoxOpacity), 50);
    }

    #[test]
    fn a_new_title_goes_after_the_one_on_the_playhead_and_fits_before_the_next() {
        let t = |start, frames| Title { start, frames, text: "t".into(), style: TextStyle::default(), fade_in: 0, fade_out: 0, in_anim: Default::default(), out_anim: Default::default(), anim_frames: 0 };
        let titles = [t(0, 150), t(200, 150)];
        // Free: where it was asked, and cut to the room before the next title.
        assert_eq!(free_title_slot(&[], 10, 150, 30.), (10, 150));
        // On a title: just after it, which leaves 50 frames before the next.
        assert_eq!(free_title_slot(&titles, 0, 150, 30.), (150, 50));
        // A gap under a second is skipped over to the room after the next title.
        let tight = [t(0, 150), t(160, 150)];
        assert_eq!(free_title_slot(&tight, 0, 150, 30.), (310, 150));
    }
}
