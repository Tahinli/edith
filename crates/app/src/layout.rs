//! The window's fixed geometry: what every region is measured in.

use crate::*;

/// ...and the narrowest it is drawn: a library row whose length the engine has
/// not measured yet has a landing place but no width, and a head marker says
/// where it goes where a zero-width box would say nothing.
pub(crate) const GHOST_MIN: f32 = 2.;

/// Fixed so the video region takes every pixel the window gains and the controls
/// never clip at 640x360.
pub(crate) const HEADER_H: f32 = 32.;
// 28 button row + 24 scrub strip + two 48 lanes + the timecode line + the gaps
// between them, with a few px of slack so a taller text line cannot push a lane
// off the bottom. The window measures itself from [`timeline_h`] now, so this
// figure is the guards' anchor for it and sits with them.
#[cfg(test)]
pub(crate) const PANEL_H: f32 = 220.;
/// How many lane rows are drawn before the lane column starts scrolling: past
/// this the panel would be taller than the picture it belongs under, and a
/// timeline that pushes the video off the window is not a timeline.
pub(crate) const LANES_MAX: usize = 6;
pub(crate) const LANE_H: f32 = 48.;
/// A caption lane's own height, thinner than a media one: a subtitle box
/// carries a name, a rate badge and the cue band under it
/// ([`Player::lane_row`]) and none of a clip's waveform or its full-height
/// plate, so the strip that used to draw the whole row's worth of nothing is
/// the regression this constant undoes.
///
/// Floored at [`HIT_MIN`] rather than the 20 px a first pass at "thin" landed
/// on: the header's show/hide eye is the row's one surviving click target
/// once the remove button moves off it onto the right button
/// ([`Player::lane_row`]), and that eye fills the row's full height -- so
/// anything under `HIT_MIN` here is a WCAG 2.5.8 target shrunk to chase a
/// pixel count, not a thinner lane. This is the whole gap between the two
/// harness variants the task asked for: 20 is not shippable at all, 24 is,
/// so 24 is the only variant landed.
pub(crate) const SUB_LANE_H: f32 = HIT_MIN;
/// The lane header column: wide enough for `V1`/`A1` and fixed, so both lanes
/// and the ruler above them start at the same pixel and are the same width --
/// one x-to-time mapping for the whole timeline. `HEADER_GAP` is part of that
/// offset and is therefore shared by all three rows.
pub(crate) const HEADER_W: f32 = 40.;
pub(crate) const HEADER_GAP: f32 = 4.;
/// The label row inside a clip; a waveform paints under it, never through it.
pub(crate) const LABEL_H: f32 = 15.;
/// A clip narrower than this shows no name: two characters and an ellipsis say
/// nothing that the tint has not already said, and cost the picture a smear.
pub(crate) const LABEL_MIN_W: f32 = 36.;
/// Peak buckets per second of source. Fixed and modest on purpose: `peaks`
/// allocates one bucket per window, so a rate taken from anything the user can
/// influence is an allocation bomb -- and 40 is already finer than the pixels a
/// clip is ever given.
///
/// corner-cut: "finer than the pixels" stops being true once the timeline is
/// zoomed -- past ~25 ms per bucket the envelope reads as steps rather than as
/// a shape. Ceiling: [`View::max_zoom`]'s 8 frames across the bed. The upgrade
/// is a second, finer pass over the visible span only, cached like `waves` is.
pub(crate) const WAVE_BPS: u32 = 40;
/// How many stand-ins may be in flight at once -- the engine's own number, not
/// a second one that happens to agree: the proxy cache sweeps with room left
/// for exactly this many encodes ([`engine::proxy::reserve`]), so a front-end
/// that started more would put the disk past the cap by whatever it added.
pub(crate) const PROXIES_AT_ONCE: usize = engine::proxy::AT_ONCE;
/// Pixels per envelope column. Coarser than a pixel: the eye reads the shape,
/// and a path with a point per pixel is a path per repaint.
pub(crate) const WAVE_COL: f32 = 2.;
/// The most columns one envelope is ever built from. A waveform is drawn into
/// the slice of its box that is on the bed ([`visible_slice`]), and a bed is a
/// screen wide, so nothing on screen ever reaches this -- it is the backstop for
/// a box laid out wider than any screen, whose path would otherwise cost a point
/// per two pixels of a width nobody can see and stall the repaint that was
/// meant to draw the wave.
pub(crate) const WAVE_COLS_MAX: usize = 4096;
/// WCAG 2.5.8: nothing clickable is smaller than this. The scrub bar stays 6 px
/// to look at -- `RULER_HIT_H` is the strip that has to be hit.
pub(crate) const HIT_MIN: f32 = 24.;
/// The media library's column: a share of the window rather than a fixed width,
/// so it yields on a narrow one, and never more than a third of it -- the
/// picture is what this program is for and keeps the majority at every size.
/// The floor is what a file name and a timecode need to be readable at all.
pub(crate) const LIBRARY_FRAC: f32 = 0.2;
pub(crate) const LIBRARY_MIN_W: f32 = 120.;
pub(crate) const LIBRARY_MAX_W: f32 = 220.;
/// A library row: a name over its duration, two lines and a click target, so
/// `HIT_MIN` binds it like every other one.
pub(crate) const ROW_H: f32 = 32.;
/// The tint swatch down the left of a row: the same colour that source's clips
/// wear in the lanes, which is the whole of the panel<->timeline association.
pub(crate) const SWATCH_W: f32 = 4.;
/// How far the encode has come, drawn under the stop square on a library row.
/// Inside a `HIT_MIN` cell with a little air each side, so the bar never reaches
/// the edge of the target it belongs to.
pub(crate) const PROXY_BAR_W: f32 = HIT_MIN - 10.;
pub(crate) const CONTROL_H: f32 = 28.;
/// The volume slider beside its button: a hundred steps across it, so a pixel
/// is finer than a step and the drag reads as continuous.
pub(crate) const VOLUME_W: f32 = 110.;
pub(crate) const RULER_HIT_H: f32 = HIT_MIN;
/// Wide enough for `HH:MM:SS:FF / HH:MM:SS:FF`, and fixed so changing digits
/// cannot push the layout around.
pub(crate) const TIME_W: f32 = 200.;
/// The keybindings card: a row per action, a title and a status line, inside a
/// 360 px tall window. The rows are click targets, so `HIT_MIN` binds them too.
/// Wider than the export card, and for the same reason that one is wider than
/// this used to be: at 320 the longest labels ("Remove the last video track (it
/// must be empty)") ran straight over the stroke printed at the other end of
/// their row. Every label in the registry fits beside its stroke here, and the
/// one that cannot -- a row waiting for a key to be pressed -- truncates rather
/// than overprinting. Still inside the 640 px floor.
pub(crate) const KEYS_W: f32 = 480.;
pub(crate) const KEYS_ROW_H: f32 = HIT_MIN;
/// How much of the row list is on screen at once; past this it scrolls. What
/// keeps the card inside the smallest window no matter how many actions the
/// editor grows -- ten rows fit here, and the eleventh is a scroll away.
pub(crate) const KEYS_ROWS_H: f32 = 10. * KEYS_ROW_H;
/// The same for the export card, which carries two summary lines and a button
/// under its list and so has less room: a section header, six codecs, the
/// container, five qualities and the destination are more rows than a 360 px
/// window holds. Eight on screen, which is the whole format section and its
/// header -- what a user picks first is never behind a scroll.
pub(crate) const EXPORT_ROWS_H: f32 = 8. * KEYS_ROW_H;
/// The export card is wider than the keybindings one: its rows carry a key, a
/// name *and* what the choice means, and the two summary lines under them state
/// the whole file. At `KEYS_W` every one of those wrapped to two lines, which is
/// the card the user called unfriendly. Still inside the 640 px floor with the
/// scrim showing either side of it.
pub(crate) const EXPORT_W: f32 = 420.;
/// The column the key of a row is printed in, wide enough for `0–9`: every row
/// in the export card says what picks it, so the card is drivable by keyboard
/// without a legend to memorise.
pub(crate) const EXPORT_KEY_W: f32 = 26.;
/// Everything in the export card that is *not* the row list: the title, the
/// status line, the head and tail of the summary, the button, the gaps between
/// them and the padding around the lot. What the list may be is the window
/// minus this -- and never less than [`EXPORT_ROWS_H`], which is the number
/// that makes the card fit the 360 px floor.
pub(crate) const EXPORT_FIXED_H: f32 = 17. + 28. + 15. + 30. + CONTROL_H + 4. + 10. + 24.;
/// The menu a right-click on a clip opens: wide enough for the longest label
/// beside the stroke that does the same thing, with the click targets `HIT_MIN`
/// binds like every other list here.
pub(crate) const MENU_W: f32 = 260.;
pub(crate) const MENU_ROW_H: f32 = HIT_MIN;
pub(crate) const MENU_PAD: f32 = 6.;

/// A cue's text, and the line it sits on. Fixed rather than a share of the
/// picture: the video region is 108 px tall at the 640x360 floor and a
/// proportional size there would be unreadable at exactly the size where it has
/// to be read.
pub(crate) const SUB_TEXT: f32 = 14.;
pub(crate) const SUB_LINE_H: f32 = 18.;
/// How far off the bottom of the picture the plate sits.
pub(crate) const SUB_BOTTOM: f32 = 8.;
/// The same, once the transient bars that hang off that bottom edge (import,
/// seek, notice) are `bars_h` tall: the cue steps up over them and comes back
/// down the frame they leave, which is what a player does with its OSD -- the
/// alternative, a notice drawn over the line being read, loses both of them.
///
/// `bars_h` is *measured* ([`height_probe`]) and never counted: a notice wraps
/// to as many lines as the window is narrow, and any constant here would be
/// right at one width. Nothing hanging there means nothing to step over, so the
/// no-notice position is [`SUB_BOTTOM`] exactly, to the pixel.
pub(crate) fn sub_bottom(bars_h: f32) -> f32 {
    // A box that was never painted measures zero, and a negative is not a
    // height -- either way there is nothing to clear.
    SUB_BOTTOM + bars_h.max(0.)
}
/// A mark narrower than this is still drawn this wide: a one-frame cue on a
/// zoomed-out bed is worth a fraction of a pixel, and a mark nobody can see says
/// the track is empty. The silence preview's marks are floored by it too -- they
/// are the same kind of thing, a picture of where something is and no target.
pub(crate) const SUB_CUE_MIN_W: f32 = 2.;
/// How much of the subtitle list in the library column is on screen at once,
/// past which it scrolls -- the media list above it is what keeps the height.
pub(crate) const SUB_ROWS_H: f32 = 3. * ROW_H;
/// The row naming the file a block of tracks came out of. Always drawn where
/// there is more than one file -- the list already scrolls past `SUB_ROWS_H`,
/// so a short window loses tracks under the fold rather than the name saying
/// whose they are. A click folds it, so it binds `HIT_MIN` like every other
/// target.
pub(crate) const SUB_HEAD_H: f32 = HIT_MIN;
/// How much of a subtitle row's width the file's name in front of the label may
/// take. Half: which file and which language are both worth reading, and a name
/// given the whole row is a row where the language is what gets truncated.
pub(crate) const SUB_STEM_SHARE: f32 = 0.5;
/// Roughly how wide one character of an 11 px list row is. Generous on purpose:
/// the element truncates for real, and a budget that overshoots would have the
/// element cut the tail off after [`clip_middle`] had already kept it.
pub(crate) const LIST_CHAR_W: f32 = 6.;
/// The fewest characters a clipped name is cut to, however narrow the column
/// gets: past this there is nothing on either side of the gap to read.
pub(crate) const LIST_CLIP_MIN: usize = 5;
/// How long the export card's Subtitles line may get before it counts tracks
/// instead of naming them ([`subtitle_plan`]). Three lines of that row's value
/// box, at [`LIST_CHAR_W`] to a character: [`EXPORT_W`] less the row's padding
/// and gap, its tick and [`EXPORT_KEY_W`] key column, and the word "Subtitles"
/// in front of the value. Not a track count -- what walks the Destination row
/// off the bottom of the card is the *wrapping*, and thirty-five one-word
/// labels wrap less than three long ones.
pub(crate) const SUB_PLAN_CHARS: usize = (3.
    * (EXPORT_W - 12. - 12. - (10. + 8. + EXPORT_KEY_W + 8. + 9. * LIST_CHAR_W))
    / LIST_CHAR_W) as usize;

/// The one key name this file still spells out, and gpui's spelling of it: it
/// is the way out of a capture and out of the overlay, and both have to work
/// while the keymap itself is what is being changed -- so neither can go
/// through the keymap to find it.
pub(crate) const ESCAPE: &str = "escape";

/// How tall a column of `lanes` rows is, gaps included -- the panel's own gap
/// between them, since the rows sit in it.
pub(crate) fn lanes_h(lanes: usize) -> f32 {
    match lanes {
        0 => 0.,
        n => n as f32 * LANE_H + (n - 1) as f32 * 8.,
    }
}

/// How tall one lane row is: [`LANE_H`] for a media track, [`SUB_LANE_H`] for
/// a caption one -- the one place that answer is decided, since
/// [`lanes_h_mixed`], [`lanes_shown_mixed`] and [`rows_below_mixed`] all walk
/// off it and a track drawn at one height and measured at another is a fold
/// line that lands on the wrong row.
pub(crate) fn lane_h(kind: LaneKind) -> f32 {
    match kind {
        LaneKind::Subtitle => SUB_LANE_H,
        LaneKind::Video | LaneKind::Audio => LANE_H,
    }
}

/// [`lanes_h`] for a real stack of lanes rather than a bare count: a project
/// with a caption track in it is shorter than that many `LANE_H` rows, and
/// the column, the fold and the affordance below it all have to agree on
/// exactly how much shorter, or "N more below" is answering a box nothing
/// draws at.
pub(crate) fn lanes_h_mixed(kinds: &[LaneKind]) -> f32 {
    match kinds.len() {
        0 => 0.,
        n => kinds.iter().copied().map(lane_h).sum::<f32>() + (n - 1) as f32 * 8.,
    }
}

/// [`lanes_shown`] for a real stack: the largest prefix of `kinds` whose
/// drawn height still fits `box_h`, on the same boundary [`lanes_shown`]
/// draws it at (`lanes_h(k) <= box_h`) -- so a uniform stack answers exactly
/// what [`lanes_shown`] already did, and a mixed one answers the row count
/// its own heights actually reach.
pub(crate) fn lanes_shown_mixed(kinds: &[LaneKind], box_h: f32) -> usize {
    (0..=kinds.len())
        .rev()
        .find(|&k| lanes_h_mixed(&kinds[..k]) <= box_h)
        .unwrap_or(0)
        .max(1)
}

/// [`rows_below`] for a real stack: `scrolled` read against each row's own
/// top rather than a uniform `LANE_H + 8` stride, so a column with a thin
/// caption track in it still says the true count once that track has
/// scrolled by.
pub(crate) fn rows_below_mixed(kinds: &[LaneKind], box_h: f32, scrolled: f32) -> usize {
    // Row `k`'s own top, in the same units `scrolled` is given in: nothing
    // for the first row, and every row before it plus the gap that follows
    // each one for the rest.
    let top = |k: usize| -> f32 {
        match k {
            0 => 0.,
            k => lanes_h_mixed(&kinds[..k]) + 8.,
        }
    };
    let past = (0..=kinds.len())
        .min_by(|&a, &b| {
            (scrolled - top(a))
                .abs()
                .partial_cmp(&(scrolled - top(b)).abs())
                .unwrap()
        })
        .unwrap_or(0);
    kinds
        .len()
        .saturating_sub(past + lanes_shown_mixed(kinds, box_h))
}

/// The same question for a column whose rows are not one height -- the
/// inspector's sections -- answered in pixels off what the scroll itself
/// reports: how far it may still be taken (`max_offset`) less how far it has
/// been (`offset`, which gpui keeps negative going down).
pub(crate) fn px_below(max_offset_h: f32, offset_y: f32) -> f32 {
    (max_offset_h + offset_y).max(0.)
}

/// How tall the panel is with `lanes` tracks in it: [`PANEL_H`] is sized for the
/// two a project starts with, and every further one adds its own row -- up to
/// [`LANES_MAX`], past which the lane column scrolls instead and the panel stops
/// growing. Only the guards ask, so it sits with them.
#[cfg(test)]
pub(crate) fn panel_h(lanes: usize) -> f32 {
    PANEL_H + lanes_h(lanes.clamp(2, LANES_MAX)) - lanes_h(2)
}

/// How tall the timeline region is: its own padding, the timecode line, the
/// ruler, the scrollbar strip under the lanes when there is one to draw, and
/// the gaps between them, plus a row per lane. Measured from its parts rather
/// than taken off `PANEL_H` -- the button row moved out of it
/// ([`Player::toolbar`]), and a height still carrying that row's pixels is a
/// height that cuts the last lane off the bottom of the window.
pub(crate) fn timeline_h(lanes: usize, scroll: bool) -> f32 {
    timeline_fixed_h(scroll) + lanes_h(lanes.clamp(2, LANES_MAX))
}

/// 8+8 padding, the timecode line, the 24 px ruler strip and the two 8 px gaps
/// between the three rows, with a couple of px of slack so a taller text line
/// cannot push a lane off the bottom. The scrollbar strip's row is *not* here:
/// the strip comes and goes with the zoom, so its pixels are carried by
/// [`timeline_fixed_h`] alone, where every derivation that must agree with
/// what is drawn reads them together.
pub(crate) const TIMELINE_FIXED_H: f32 = 16. + 18. + 8. + RULER_HIT_H + 8. + 4.;

/// The panel's fixed furniture with its scrollbar strip while the timeline has
/// somewhere to scroll to. With nowhere to go the strip is not drawn -- a bar
/// for a view that cannot move is a bar teaching nothing -- and its row goes
/// with it, out of every budget at once: a height still carrying the row would
/// hand it to the lane column as a bonus at exactly the zoom where the hand is
/// least expecting the bed to move.
pub(crate) fn timeline_fixed_h(scroll: bool) -> f32 {
    TIMELINE_FIXED_H + match scroll {
        true => 8. + SCROLL_HIT,
        false => 0.,
    }
}

/// The most of a short window the timeline may take, sized so the seam's own
/// floor still fits inside it ([`split_bounds`]): at the 640x360 floor that is
/// 171 px of the 360 -- the chrome with the scrollbar strip in it, one whole
/// lane, and the line saying the rest are below. Left smaller than the floor
/// the floor would win the clamp anyway and this number would say the panel
/// takes less of a short window than it does. The lanes scroll inside whatever
/// is left, which leaves the picture a region rather than a letterbox stripe.
pub(crate) const TIMELINE_SHARE: f32 = 171. / 360.;

/// The edit toolbar directly above the timeline: one control's height in its
/// own padding, fixed so nothing in it can push the timeline down.
pub(crate) const TOOLBAR_H: f32 = CONTROL_H + 16.;

/// The top bar: the project's name on the left, the two file actions on the
/// right. Fixed for the reason [`HEADER_H`] is.
pub(crate) const TOPBAR_H: f32 = 36.;

/// The transport strip under the picture -- play, timecode, volume -- where a
/// player's own controls live in every consumer editor.
pub(crate) const TRANSPORT_H: f32 = CONTROL_H + 12.;

/// One press of a zoom key, or one notch of ctrl+wheel.
pub(crate) const ZOOM_STEP: f32 = 1.25;

/// How far one notch of a bare wheel slides the view along, as a share of what
/// is on the bed. A *share* rather than a number of pixels or of seconds: one
/// gesture then moves the same fraction of what is being looked at whether the
/// bed is showing five seconds or five hours, which is the only way a wheel is
/// usable at both ends of the zoom.
pub(crate) const SCROLL_NOTCH_SHARE: f32 = 0.1;

/// How few frames the bed may be narrowed down to. Past this there is nothing
/// left to aim at -- a single frame across a whole window is a wall of colour,
/// not an edit surface.
pub(crate) const ZOOM_MIN_FRAMES: f64 = 8.;

/// How thin a second of timeline may be drawn on a bed that is already showing
/// all of it: the far stop for a *short* project, which has no length of its own
/// worth widening to, so a five second import can still be zoomed out of.
pub(crate) const PPS_MIN: f64 = 1.;

/// How much bed the far stop leaves past the last frame, as a multiple of the
/// timeline's own length -- so a timeline zoomed all the way out ends a sliver
/// short of the window's edge rather than glued to it.
pub(crate) const ZOOM_OUT_MARGIN: f64 = 1.05;

/// How wide a second of timeline is drawn before anyone zooms: a five second
/// import is 200 px of a bed several times that, so a short clip reads as
/// short -- the thing a bed scaled to the content's own length cannot say.
/// [`View::fit`] is the one way back to "the whole timeline across the bed".
pub(crate) const PPS_DEFAULT: f64 = 40.;

// -- the seams between the regions, and where a hand has dragged them ---------

/// Which divider a hand has hold of. The legacy tree's three seams --
/// library|picture, picture|inspector and the timeline's top edge -- plus the
/// darkroom's own two, dock|centre and time-band|bench: `stance.rs`'s own
/// `BENCH_H`/`DOCK_W` doc comments named this exact fold as the fix for
/// "fields are not stretchable" before this diff existed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Split {
    Library,
    Inspector,
    Timeline,
    Dock,
    Bench,
}

impl Split {
    /// The two persisted seams -- library/inspector/timeline stay
    /// unpersisted, out of this diff's scope, same as before it. Read by
    /// [`load_stance_splits`] and [`save_stance_splits`] together so the
    /// two can never learn a different set of keys from each other, the
    /// gap a sixth region would otherwise fall through silently.
    const PERSISTED: [Split; 2] = [Split::Dock, Split::Bench];

    fn key(self) -> &'static str {
        match self {
            Split::Library => "library",
            Split::Inspector => "inspector",
            Split::Timeline => "timeline",
            Split::Dock => "dock",
            Split::Bench => "bench",
        }
    }
}

/// How wide a divider is drawn *and* hit. Wider than the 1 px stroke it stands
/// in for: a seam nobody can put a pointer on is a seam nobody can drag, which
/// is why every editor that ships this draws a strip rather than the hairline
/// it looks like.
pub(crate) const SPLIT_W: f32 = 6.;

/// The most of the window one side column may be dragged to. A third each, so
/// the picture keeps the middle at every size -- the rule [`library_w`] and
/// [`inspector_w`] already lay an untouched window out by, kept once the hand
/// takes over.
pub(crate) const SIDE_MAX_FRAC: f32 = 1. / 3.;

/// The most of the window the timeline may be dragged to. Past
/// [`TIMELINE_SHARE`], which is what it is *given*: a hand asking for a tall
/// timeline is asking on purpose, and the picture still keeps most of a third.
pub(crate) const TIMELINE_MAX_SHARE: f32 = 0.7;

/// The least the darkroom's bench may be dragged to: derived, not measured,
/// from the same stack `ui::stance::bench` and `ui::bench_stance::render`
/// actually build, top to bottom --
/// [`crate::ui::stance::BENCH_CHROME_H`] (the "bench" section head and its
/// padding) + [`crate::ui::bench_stance::RULER_H`] (the pinned ruler) +
/// [`crate::ui::bench_stance::ROW_GAP`] (the gap `bench-content`'s flex
/// column puts between the ruler and the lane column) + two lane rows at
/// [`crate::ui::bench_stance::LANE_MIN_H`] each (the darkroom's own floor of
/// two lanes -- V1, A1, `bench_stance::render`'s own no-session fallback)
/// + one more `ROW_GAP` between those two rows (`bench-lanes`'s own flex
/// gap). Below this the `bench-lanes` column asks for more height than
/// `bench-content`'s flex_1 gives it, and since that column scrolls rather
/// than clips visibly, the shortfall comes off the bottom row's own pixels
/// unscrolled -- A1's clip-bar border and status dot, first. At the old
/// literal `80.` the shortfall was `82. - 80. == 2.`, the exact ~2px clip
/// this floor now closes.
pub(crate) const BENCH_MIN_H: f32 = crate::ui::stance::BENCH_CHROME_H
    + crate::ui::bench_stance::RULER_H
    + crate::ui::bench_stance::ROW_GAP
    + 2. * crate::ui::bench_stance::LANE_MIN_H
    + crate::ui::bench_stance::ROW_GAP;

/// What a hand has done to the three seams: the size it dragged each panel to,
/// or `None` where nobody has touched one and the window's own share still
/// answers. Held in the model and not recomputed, so a size outlives the
/// release -- and read through [`split_size`], so it cannot outlive the window
/// it was set in.
#[derive(Clone, Copy, Default)]
pub(crate) struct Splits {
    library: Option<f32>,
    inspector: Option<f32>,
    timeline: Option<f32>,
    dock: Option<f32>,
    bench: Option<f32>,
}

impl Splits {
    pub(crate) fn get(&self, split: Split) -> Option<f32> {
        match split {
            Split::Library => self.library,
            Split::Inspector => self.inspector,
            Split::Timeline => self.timeline,
            Split::Dock => self.dock,
            Split::Bench => self.bench,
        }
    }

    pub(crate) fn set(&mut self, split: Split, size: f32) {
        *match split {
            Split::Library => &mut self.library,
            Split::Inspector => &mut self.inspector,
            Split::Timeline => &mut self.timeline,
            Split::Dock => &mut self.dock,
            Split::Bench => &mut self.bench,
        } = Some(size);
    }
}

/// Where the darkroom's own two dragged sizes live -- dock and bench, the
/// only pair with a hand on them so far. Same small, silent round trip as
/// [`crate::ui::dock_stance`]'s tab pick: unreadable or absent leaves both
/// `None`, which [`split_size`] already reads as "give it the window's own
/// share".
pub(crate) fn stance_splits_config_path() -> std::path::PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("stance-splits")
}

pub(crate) fn load_stance_splits() -> Splits {
    load_stance_splits_from(&stance_splits_config_path())
}

/// One `split=pixels` line per touched seam, read from `path` -- factored out
/// of [`load_stance_splits`] so a test can round-trip a scratch file instead
/// of the real config.
pub(crate) fn load_stance_splits_from(path: &std::path::Path) -> Splits {
    let mut splits = Splits::default();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let Ok(px) = value.trim().parse::<f32>() else { continue };
            let Some(split) = Split::PERSISTED.into_iter().find(|s| s.key() == key) else {
                continue;
            };
            splits.set(split, px);
        }
    }
    splits
}

/// Writes the sizes a hand has actually dragged -- the legacy three stay
/// unpersisted, as they already were before this diff (out of its scope).
/// Walks [`Split::PERSISTED`] rather than a hand-written `if let` per field,
/// so a region added there needs no second, separately-maintained list here.
pub(crate) fn save_stance_splits(splits: &Splits) {
    save_stance_splits_to(splits, &stance_splits_config_path());
}

pub(crate) fn save_stance_splits_to(splits: &Splits, path: &std::path::Path) {
    let text: String = Split::PERSISTED
        .into_iter()
        .filter_map(|split| splits.get(split).map(|px| format!("{}={px}\n", split.key())))
        .collect();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, text);
}

/// How small a panel may be dragged and how large: the floor is the size it
/// still shows something at, the ceiling is what its neighbour needs to stay a
/// region rather than a sliver. Neither end is passable -- a panel dragged to
/// nothing is a panel nobody can get back, and a picture squeezed out by two
/// side columns is the same loss on the other side of the window.
pub(crate) fn split_bounds(
    split: Split,
    lanes: usize,
    window: Size<Pixels>,
    scroll: bool,
) -> (f32, f32) {
    let (w, h) = (f32::from(window.width), f32::from(window.height));
    match split {
        Split::Library => (LIBRARY_MIN_W, w * SIDE_MAX_FRAC),
        Split::Inspector => (INSPECTOR_MIN_W, w * SIDE_MAX_FRAC),
        // The dock is the darkroom's one side column -- same shape as the
        // legacy inspector's, so it takes the legacy inspector's own floor
        // and ceiling rather than a second pair that would only ever agree.
        Split::Dock => (INSPECTOR_MIN_W, w * SIDE_MAX_FRAC),
        // The bench is the darkroom's own timeline, but its ceiling is not
        // [`Split::Timeline`]'s window-share one: a bench that swallows
        // `TIMELINE_MAX_SHARE` of a 720p window leaves the screen and its
        // scale plate only 100px, which is not a picture any more. The
        // ceiling instead leaves the screen and time band their own fixed
        // 160px -- room for the scale plate, the picture and a sliver of
        // letterbox -- and `BENCH_MIN_H` still wins the clamp on anything
        // shorter than that.
        Split::Bench => (
            BENCH_MIN_H,
            (h - crate::ui::stance::TIME_BAND_H - crate::ui::stance::LEDGER_H - 160.)
                .max(BENCH_MIN_H),
        ),
        // One whole lane under the chrome, the scrollbar strip's row included
        // while there is one to draw ([`timeline_fixed_h`]): a timeline
        // shorter than that is a ruler with nothing beneath it. A second
        // track pays for the line that says the rest are below the fold as
        // well -- at this height only one row fits, so that line is *always*
        // drawn there, and it is drawn inside the region
        // ([`Player::timeline`]). Left out of the floor it comes off the
        // lane's own pixels, and the one lane the floor promises is a header
        // cut in half.
        Split::Timeline => (
            // `LANE_H` even though the first lane could be a thinner caption
            // one: a floor is a *minimum*, so the only failure mode of
            // reading it off the tallest kind is a seam that refuses to go
            // quite as small as it technically could -- never a lane clipped
            // short of its own row. corner-cut: exact per-kind floor would
            // read `session.lanes()[0].kind`, which `split_bounds` is not
            // handed; ceiling is one drawn-too-tall pixel at the very floor
            // of a caption-only project, upgrade is threading the real lane
            // list through here the way [`lanes_h_mixed`] already takes it.
            timeline_fixed_h(scroll)
                + LANE_H
                + match lanes > 1 {
                    true => LABEL_H + 8.,
                    false => 0.,
                },
            h * TIMELINE_MAX_SHARE,
        ),
    }
}

pub(crate) fn split_size(
    split: Split,
    set: Option<f32>,
    lanes: usize,
    window: Size<Pixels>,
    scroll: bool,
) -> f32 {
    let (w, h) = (f32::from(window.width), f32::from(window.height));
    let (min, max) = split_bounds(split, lanes, window, scroll);
    let default = match split {
        Split::Library => library_w(w),
        Split::Inspector => inspector_w(w),
        Split::Timeline => timeline_h(lanes, scroll).min(h * TIMELINE_SHARE),
        Split::Dock => crate::ui::stance::DOCK_W,
        Split::Bench => crate::ui::stance::BENCH_H,
    };
    // The floor wins a window too small to honour both ends: a panel at its
    // floor is still a panel, and `clamp` panics outright on a ceiling under
    // its floor.
    set.unwrap_or(default).clamp(min, max.max(min))
}

/// Where a divider drag puts the panel it belongs to: the pointer's position
/// turned into that panel's size. Half a strip is taken off each answer because
/// the strip is grabbed by its middle -- without it the panel jumps a strip's
/// width on the press and then trails the pointer for the rest of the gesture.
pub(crate) fn split_drag_size(split: Split, at: Point<Pixels>, window: Size<Pixels>) -> f32 {
    let (x, y) = (f32::from(at.x), f32::from(at.y));
    let (w, h) = (f32::from(window.width), f32::from(window.height));
    match split {
        Split::Library => x - SPLIT_W / 2.,
        Split::Inspector => w - x - SPLIT_W / 2.,
        // This seam sits above the edit toolbar, which is a fixed height: what
        // is under the pointer is the toolbar and the timeline together.
        Split::Timeline => h - y - TOOLBAR_H - SPLIT_W / 2.,
        // The dock sits on the same right edge the inspector does.
        Split::Dock => w - x - SPLIT_W / 2.,
        // The bench sits above the fixed ledger strip, the bench's own
        // reason the timeline's formula reads the toolbar.
        Split::Bench => h - y - crate::ui::stance::LEDGER_H - SPLIT_W / 2.,
    }
}

impl Player {
    /// How big the panel on this seam is this frame -- the one door every
    /// region measures itself through, so a dragged size and a drawn one cannot
    /// disagree.
    pub(crate) fn split_px(&self, split: Split, window: Size<Pixels>) -> f32 {
        // The pair a fresh project starts with where there is no session yet,
        // which is what the timeline itself draws (`Player::timeline`).
        let lanes = self
            .session
            .as_ref()
            .map_or(2, |session| session.lanes().len());
        // Whether the time axis has anywhere to go: the scrollbar strip is
        // drawn only while it does, and the region's furniture -- and its
        // floor -- carry the strip's row only while it is. One door
        // ([`timeline_fixed_h`]) answers the region, the fold and the floor
        // together, so the three cannot disagree about whether the strip is
        // there.
        let view = self.view();
        let scroll = view.duration > view.span();
        split_size(split, self.splits.get(split), lanes, window, scroll)
    }
}
