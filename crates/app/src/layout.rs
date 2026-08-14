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
/// The row naming the file a block of tracks came out of. Shorter than a track
/// row because nothing on it is clicked -- `HIT_MIN` binds targets and a header
/// is a label, not a way in -- and drawn at all only where the column has the
/// height to show tracks under it ([`sub_headers_fit`]).
pub(crate) const SUB_HEAD_H: f32 = 18.;
/// How much of the subtitle list is on screen at the 640x360 floor: one row,
/// measured -- the section's own heading and the Add button under it take the
/// rest of the 84 px the column has there.
pub(crate) const SUB_ROWS_AT_FLOOR: f32 = ROW_H;
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

/// How many whole lane rows a box this tall shows. At least one: a box too
/// short for a single lane still shows part of one, and "0 shown" would count
/// every lane there is as hidden.
pub(crate) fn lanes_shown(box_h: f32) -> usize {
    (((box_h + 8.) / (LANE_H + 8.)).floor() as usize).max(1)
}

/// How many rows are still below the fold *now*: the count the affordance says
/// out loud, and the reason it is a function of the scroll rather than of the
/// lane count alone. A line that keeps saying "2 more tracks below" after the
/// user has scrolled to the last one is a line that has stopped being true, and
/// an affordance nobody can make go away is read as a bug rather than as an
/// instruction.
///
/// `scrolled` is how far down the column has been taken, in pixels. Rounded to
/// the nearest row: a half-scrolled row is showing, so it is not below.
pub(crate) fn rows_below(total: usize, box_h: f32, scrolled: f32) -> usize {
    let past = ((scrolled / (LANE_H + 8.)).round().max(0.)) as usize;
    total.saturating_sub(past + lanes_shown(box_h))
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
/// ruler and the gaps between them, plus a row per lane. Measured from its
/// parts rather than taken off `PANEL_H` -- the button row moved out of it
/// ([`Player::toolbar`]), and a height still carrying that row's pixels is a
/// height that cuts the last lane off the bottom of the window.
pub(crate) fn timeline_h(lanes: usize) -> f32 {
    TIMELINE_FIXED_H + lanes_h(lanes.clamp(2, LANES_MAX))
}

/// 8+8 padding, the timecode line, the 24 px ruler strip and the two 8 px gaps
/// between the three rows, with a couple of px of slack so a taller text line
/// cannot push a lane off the bottom.
pub(crate) const TIMELINE_FIXED_H: f32 = 16. + 18. + 8. + RULER_HIT_H + 8. + 4.;

/// The most of a short window the timeline may take. At the 640x360 floor that
/// is 151 px of the 360, which leaves the picture a region rather than a
/// letterbox stripe -- and the lanes scroll inside it.
pub(crate) const TIMELINE_SHARE: f32 = 0.42;

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

/// Which divider a hand has hold of. The three seams the main layout has --
/// library|picture, picture|inspector and the timeline's top edge -- and so the
/// only three sizes in this window a person sets rather than is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Split {
    Library,
    Inspector,
    Timeline,
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
}

impl Splits {
    pub(crate) fn get(&self, split: Split) -> Option<f32> {
        match split {
            Split::Library => self.library,
            Split::Inspector => self.inspector,
            Split::Timeline => self.timeline,
        }
    }

    pub(crate) fn set(&mut self, split: Split, size: f32) {
        *match split {
            Split::Library => &mut self.library,
            Split::Inspector => &mut self.inspector,
            Split::Timeline => &mut self.timeline,
        } = Some(size);
    }
}

/// How small a panel may be dragged and how large: the floor is the size it
/// still shows something at, the ceiling is what its neighbour needs to stay a
/// region rather than a sliver. Neither end is passable -- a panel dragged to
/// nothing is a panel nobody can get back, and a picture squeezed out by two
/// side columns is the same loss on the other side of the window.
pub(crate) fn split_bounds(split: Split, lanes: usize, window: Size<Pixels>) -> (f32, f32) {
    let (w, h) = (f32::from(window.width), f32::from(window.height));
    match split {
        Split::Library => (LIBRARY_MIN_W, w * SIDE_MAX_FRAC),
        Split::Inspector => (INSPECTOR_MIN_W, w * SIDE_MAX_FRAC),
        // One whole lane under the chrome: a timeline shorter than that is a
        // ruler with nothing beneath it. A second track pays for the line that
        // says the rest are below the fold as well -- at this height only one
        // row fits, so that line is *always* drawn there, and it is drawn
        // inside the region ([`Player::timeline`]). Left out of the floor it
        // comes off the lane's own pixels, and the one lane the floor promises
        // is a header cut in half.
        Split::Timeline => (
            TIMELINE_FIXED_H
                + LANE_H
                + match lanes > 1 {
                    true => LABEL_H + 8.,
                    false => 0.,
                },
            h * TIMELINE_MAX_SHARE,
        ),
    }
}

/// How big the panel at this seam is drawn: what the hand dragged it to, held
/// inside the window *being drawn now* -- the window is resizable, and a width
/// set at 1920 px is not a width at 640 -- or the share the untouched layout
/// gives it.
pub(crate) fn split_size(
    split: Split,
    set: Option<f32>,
    lanes: usize,
    window: Size<Pixels>,
) -> f32 {
    let (w, h) = (f32::from(window.width), f32::from(window.height));
    let (min, max) = split_bounds(split, lanes, window);
    let default = match split {
        Split::Library => library_w(w),
        Split::Inspector => inspector_w(w),
        Split::Timeline => timeline_h(lanes).min(h * TIMELINE_SHARE),
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
        split_size(split, self.splits.get(split), lanes, window)
    }
}
