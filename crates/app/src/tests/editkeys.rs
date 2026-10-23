//! Lane A's timeline-editing verbs: close gap, freeze frame, paste attributes,
//! match frame, replace edit, select forward, split edits, nudge and the box
//! select. The window has no `TestAppContext`, so each is driven through the
//! session-level function the `Player` method calls, plus the oracle row, the
//! default stroke and the menu door.

use super::*;
use crate::player::editkeys::{
    Part, forward_picks, gap_under, look_of, marquee_picks, matched_source, nudge, paste_look, replace_plan,
    split_edit_in, still_name,
};
use crate::{Marquee, Player, Selection};
use engine::color::ColorParams;
use keymap::ActionId;

/// A one-take session: `test_av.mp4`, V1 and A1 linked, 150 frames at 30 fps.
fn av() -> PlaybackSession {
    let session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    session
}

/// The take cut in two at frame 60 and the second half moved 30 frames later,
/// leaving a gap `60..90` on both lanes.
fn av_with_gap() -> PlaybackSession {
    let mut session = av();
    assert!(session.cut_at(2.0));
    let (lane, idx) = (Lane::V1, 1);
    assert!(session.move_clip_to(lane, idx, lane, 90));
    session
}

fn live(action: ActionId, ctx: Ctx) -> Enable {
    enable(action, Ctx { timeline: true, playable: true, ..ctx })
}

fn key_of(action: ActionId) -> Vec<String> {
    keymap::Keymap::defaults()
        .entries()
        .iter()
        .filter(|b| b.action == action)
        .map(|b| b.chord.pretty())
        .collect()
}

#[test]
fn close_gap_closes_the_gap_under_the_playhead_in_one_undo_step() {
    let mut session = av_with_gap();
    assert_eq!(gap_under(&session, 30), None, "inside a clip is no gap");
    let (lane, start, frames) = gap_under(&session, 75).expect("the playhead stands in the hole");
    assert_eq!((lane, start, frames), (Lane::V1, 60, 30));
    let scope = session.gap_take_scope(lane, start, frames).expect("a scope");
    let depth = session.undo_depth();
    session.cut_regions(&[(start, frames)], &scope).expect("the hole closes");
    assert_eq!(session.undo_depth(), depth + 1, "one undo step");
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 60);
    assert_eq!(session.lane_clips(Lane::A1)[1].start, 60, "the linked sound closed with it");
    assert_eq!(gap_under(&session, 75), None);
    assert!(session.undo());
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 90);
}

#[test]
fn close_gap_is_dimmed_with_a_reason_when_no_gap_is_there() {
    assert_eq!(live(ActionId::CloseGap, Ctx::default()), Enable::No("no gap under the playhead"));
    assert_eq!(live(ActionId::CloseGap, Ctx { gap_here: true, ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::CloseGap), ["Shift+Delete"]);
}

#[test]
fn freeze_frame_holds_the_frame_and_pushes_the_rest_in_one_undo_step() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    let (proj, meta) = session.export_snapshot();
    let frame = engine::export::compose_at(&proj, &meta, 30, meta.width, meta.height).expect("a frame");
    let dir = std::env::temp_dir().join(format!("edith-freeze-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("hold.png");
    std::fs::write(&png, engine::decode::to_png_bytes(&frame).unwrap()).unwrap();
    let before = session.lane_clips(Lane::V1).to_vec();
    let depth = session.undo_depth();
    let (at, dur) = (30, 60);
    session.begin_batch();
    let moved = Player::freeze_edit(&mut session, at, dur, 30., Lane::V1, &png).expect("the freeze lands");
    assert!(moved, "the clips after the hold moved, and the toast says so");
    session.end_batch();
    assert_eq!(session.undo_depth(), depth + 1, "one undo step");
    let v1 = session.lane_clips(Lane::V1);
    assert_eq!(v1.len(), 4, "the first half is cut at the frame, the still sits between the halves");
    assert_eq!((v1[1].start, v1[1].frames()), (30, 60), "the still holds the frame for the duration");
    assert_eq!(v1[2].start, 90, "the cut-off half of the first clip moved later by the hold");
    assert_eq!(v1[3].start, before[1].start + dur, "everything after moves later");
    assert!(engine::is_image(&session.sources()[v1[1].source].path));
    assert!(session.undo());
    assert_eq!(session.lane_clips(Lane::V1).len(), before.len(), "undo takes the whole freeze back");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn freeze_frame_needs_a_clip_under_the_playhead_and_has_a_key() {
    assert_eq!(live(ActionId::Freeze, Ctx::default()), Enable::No("nothing under the playhead"));
    assert_eq!(live(ActionId::Freeze, Ctx { clip_here: true, ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::Freeze), ["Shift+R"]);
}

#[test]
fn paste_attributes_carries_colour_speed_and_audio_in_one_undo_step() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    let grade = ColorParams { brightness: 0.25, ..ColorParams::default() };
    assert!(session.set_color(Lane::V1, 0, Some(grade)));
    assert!(session.set_clip_gain(Lane::A1, 0, 6.));
    let look = look_of(&session, (Lane::V1, 0)).expect("a look");
    let depth = session.undo_depth();
    // Onto the second take, picked by its picture half: the sound half takes the audio part.
    let done = paste_look(&mut session, &look, &[(Lane::V1, 1)], &[Part::Color, Part::Audio]);
    assert_eq!(done.touched, 2);
    assert_eq!(done.words, ["colour", "audio"]);
    assert_eq!(session.undo_depth(), depth + 1, "colour and audio are one undo step");
    assert_eq!(session.color_of(Lane::V1, 1).copied(), Some(grade));
    assert_eq!(session.clip_gain_of(Lane::A1, 1), Some(6.));
    assert!(session.undo());
    assert_eq!(session.color_of(Lane::V1, 1), None);
    assert_eq!(session.clip_gain_of(Lane::A1, 1), Some(0.));
}

#[test]
fn paste_attributes_speed_reaches_the_pick_and_effects_say_when_none_apply() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    assert!(session.set_speed(Lane::V1, 0, Speed::from_permille(2000)).is_ok());
    let look = look_of(&session, (Lane::V1, 0)).expect("a look");
    let done = paste_look(&mut session, &look, &[(Lane::V1, 1)], &[Part::Speed]);
    assert_eq!(done.touched, 1);
    assert_eq!(session.speed_of(Lane::V1, 1), Speed::from_permille(2000));
}

#[test]
fn paste_attributes_rows_are_dimmed_until_something_is_copied_and_selected() {
    for action in [
        ActionId::PasteAttributes,
        ActionId::PasteColor,
        ActionId::PasteEffects,
        ActionId::PasteTransform,
        ActionId::PasteSpeed,
        ActionId::PasteAudio,
    ] {
        assert_eq!(live(action, Ctx::default()), Enable::No("nothing copied yet"), "{action:?}");
        assert_eq!(
            live(action, Ctx { clipboard: true, ..Ctx::default() }),
            Enable::No("no clips selected"),
            "{action:?}"
        );
        assert_eq!(live(action, Ctx { clipboard: true, picks: 1, ..Ctx::default() }), Enable::Yes, "{action:?}");
    }
    assert_eq!(key_of(ActionId::PasteAttributes), ["Ctrl+Alt+V"]);
}

#[test]
fn match_frame_finds_the_source_frame_under_the_playhead() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    // The second half plays the file from frame 60: the playhead 10 frames in shows frame 70.
    let (path, stream, secs) = matched_source(&session, (Lane::V1, 1), 70, 30.).expect("a source");
    assert_eq!(path, session.sources()[0].path);
    assert_eq!(stream, 0);
    assert!((secs - 70. / 30.).abs() < 1e-9, "{secs}");
    assert_eq!(live(ActionId::MatchFrame, Ctx::default()), Enable::No("nothing under the playhead"));
    assert_eq!(live(ActionId::MatchFrame, Ctx { clip_here: true, ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::MatchFrame), ["Shift+F"]);
}

#[test]
fn replace_edit_keeps_the_place_and_the_duration_in_one_undo_step() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    // The replacement is the take's own file from frame 10: same length, same place.
    let before = session.lane_clips(Lane::V1)[1].clone();
    let (at, len, items) =
        replace_plan(&session, (Lane::V1, 1), 0, 10, 150, (true, true, false)).expect("a plan");
    assert_eq!((at, len), (before.start, before.end() - before.start));
    assert_eq!(items.len(), 2, "the linked sound is replaced with it");
    let depth = session.undo_depth();
    session
        .edit_from_source_at(engine::SourceEdit::Overwrite, f64::from(at) / 30., &items)
        .expect("the overwrite lands");
    assert_eq!(session.undo_depth(), depth + 1);
    let now = session.lane_clips(Lane::V1)[1].clone();
    assert_eq!((now.start, now.end()), (before.start, before.end()), "same place, same duration");
    assert_eq!(now.in_frame, 10, "from the chosen in point");
    assert!(session.undo());
    assert_eq!(session.lane_clips(Lane::V1)[1].in_frame, before.in_frame);
    // A source shorter than the clip is refused in words.
    assert!(replace_plan(&session, (Lane::V1, 1), 0, 10, 40, (true, true, false)).is_err());
    // A picture-less item cannot replace a picture.
    assert!(replace_plan(&session, (Lane::V1, 1), 0, 0, 150, (false, true, false)).is_err());
    assert_eq!(live(ActionId::ReplaceEdit, Ctx { clip_here: true, ..Ctx::default() }), Enable::No("no library item selected"));
    assert_eq!(live(ActionId::ReplaceEdit, Ctx { clip_here: true, asset: true, ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::ReplaceEdit), ["F8"]);
}

#[test]
fn select_forward_marks_what_has_not_ended_by_the_playhead() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    assert!(session.cut_at(4.0));
    let lanes = session.lanes();
    let one = forward_picks(&session, 70, &[Lane::V1]);
    assert_eq!(one.picks(), [(Lane::V1, 1), (Lane::V1, 2)], "this track, from the clip the playhead is in");
    let all = forward_picks(&session, 70, &lanes);
    assert_eq!(all.len(), 4, "every track, V1 and A1");
    assert!(all.contains((Lane::A1, 2)));
    assert!(forward_picks(&session, 150, &lanes).is_empty(), "nothing past the end");
    assert_eq!(key_of(ActionId::SelectForward), ["Alt+Y"]);
    assert_eq!(key_of(ActionId::SelectForwardAll), ["Shift+A"]);
}

#[test]
fn split_edit_extends_the_sound_past_the_picture_in_one_undo_step() {
    let mut session = av();
    // Shorten the take (both halves trim together), then pull only the sound's end out to 100.
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, 60));
    assert_eq!(session.lane_clips(Lane::A1)[0].end(), 60);
    let depth = session.undo_depth();
    let (lane, idx, edge, now) = split_edit_in(&mut session, (Lane::V1, 0), true, 100).expect("an L-cut");
    assert_eq!((lane, idx, edge, now), (Lane::A1, 0, Edge::End, 100));
    assert_eq!(session.undo_depth(), depth + 1, "one undo step");
    assert_eq!(session.lane_clips(Lane::V1)[0].end(), 60, "the picture stays");
    assert_eq!(session.lane_clips(Lane::A1)[0].end(), 100, "the sound runs on");
    assert!(session.undo());
    assert_eq!(session.lane_clips(Lane::A1)[0].end(), 60);
    assert!(session.lane_clips(Lane::A1)[0].link.is_some(), "undo restores the link");
    // The J-cut: the picture's start is pulled earlier while the sound stays.
    assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 30));
    let (lane, _, edge, _) = split_edit_in(&mut session, (Lane::A1, 0), false, 10).expect("a J-cut");
    assert_eq!((lane, edge), (Lane::V1, Edge::Start));
    assert_eq!(session.lane_clips(Lane::V1)[0].start, 10);
    assert_eq!(session.lane_clips(Lane::A1)[0].start, 30);
}

#[test]
fn split_edit_with_a_named_edge_moves_that_edge_and_refuses_the_other_side() {
    use engine::editkeys::split_edit_at;
    let mut session = av();
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, 60));
    // Named out edge on the sound: moves to 40 whatever the nearer edge would have been.
    let (lane, _, edge, now) = split_edit_at(&mut session, (Lane::V1, 0), true, Some(Edge::End), 40).expect("an out edge");
    assert_eq!((lane, edge, now), (Lane::A1, Edge::End, 40));
    assert_eq!(session.lane_clips(Lane::V1)[0].end(), 60, "the picture stays");
    // An in edge given a frame past its out edge is refused, and nothing moves.
    assert!(session.undo());
    assert!(split_edit_at(&mut session, (Lane::V1, 0), true, Some(Edge::Start), 61).is_err());
    assert!(split_edit_at(&mut session, (Lane::V1, 0), true, Some(Edge::End), 0).is_err());
    assert_eq!(session.lane_clips(Lane::A1)[0].end(), 60, "a refused split changes nothing");
    assert!(session.lane_clips(Lane::A1)[0].link.is_some(), "a refused split keeps the link");
}

#[test]
fn split_edit_rows_say_what_to_select() {
    assert_eq!(live(ActionId::ExtendAudio, Ctx::default()), Enable::No("no clip selected"));
    let video = (Clip::source_range(0, 0, 50), Lane::V1);
    let unlinked = Ctx { clip: Some(video.clone()), ..Ctx::default() };
    assert!(matches!(live(ActionId::ExtendAudio, unlinked.clone()), Enable::No(_)));
    assert_eq!(live(ActionId::ExtendVideo, unlinked), Enable::Yes, "the picture's own edge needs no partner");
    let mut linked = video.0.clone();
    linked.link = Some(3);
    assert_eq!(live(ActionId::ExtendAudio, Ctx { clip: Some((linked, Lane::V1)), ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::ExtendAudio), ["Shift+Alt+L"]);
    assert_eq!(key_of(ActionId::ExtendVideo), ["Shift+Alt+J"]);
}

#[test]
fn nudge_moves_a_take_by_one_frame_and_stops_at_a_neighbour() {
    let mut session = av_with_gap();
    let depth = session.undo_depth();
    nudge(&mut session, &[(Lane::V1, 1)], 1).expect("room to the right");
    assert_eq!(session.undo_depth(), depth + 1, "one undo step");
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 91);
    assert_eq!(session.lane_clips(Lane::A1)[1].start, 91, "the linked sound moves with it");
    nudge(&mut session, &[(Lane::V1, 1)], -10).expect("ten frames back");
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 81);
    nudge(&mut session, &[(Lane::V1, 1)], -50).expect("a long nudge stops flush against the neighbour");
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 60);
    assert!(nudge(&mut session, &[(Lane::V1, 1)], -1).is_err(), "the first take is in the way");
    assert_eq!(session.lane_clips(Lane::V1)[1].start, 60, "a refusal changes nothing");
    assert!(nudge(&mut session, &[(Lane::V1, 0)], -1).is_err(), "already at the start");
    assert_eq!(live(ActionId::NudgeForward, Ctx::default()), Enable::No("no clips selected"));
    assert_eq!(live(ActionId::NudgeForward10, Ctx { picks: 2, ..Ctx::default() }), Enable::Yes);
    assert_eq!(key_of(ActionId::NudgeBack), ["Ctrl+Alt+←"]);
    assert_eq!(key_of(ActionId::NudgeForward10), ["Ctrl+Shift+Alt+→"]);
}

#[test]
fn box_select_marks_what_the_rectangle_touches_on_the_lanes_it_crosses() {
    let mut session = av();
    assert!(session.cut_at(2.0));
    let beds = [(Lane::V1, 0., 48.), (Lane::A1, 50., 98.)];
    let v1 = marquee_picks(&session, &beds, (10., 30.), (50, 80), Selection::new());
    assert_eq!(v1.picks(), [(Lane::V1, 0), (Lane::V1, 1)], "only the lane the box crosses, both clips it touches");
    let both = marquee_picks(&session, &beds, (10., 70.), (100, 120), Selection::new());
    assert_eq!(both.picks(), [(Lane::V1, 1), (Lane::A1, 1)]);
    let mut base = Selection::new();
    base.add((Lane::A1, 0));
    let added = marquee_picks(&session, &beds, (10., 30.), (0, 10), base);
    assert_eq!(added.picks(), [(Lane::A1, 0), (Lane::V1, 0)], "a ctrl-box adds to what is selected");
    assert!(marquee_picks(&session, &beds, (200., 300.), (0, 150), Selection::new()).is_empty());
    let box_ = Marquee { from: point(px(10.), px(10.)), to: point(px(12.), px(11.)) };
    assert!(!box_.live(), "under the slop it is still a click");
    assert!(Marquee { to: point(px(40.), px(11.)), ..box_ }.live());
}

#[test]
fn every_new_verb_is_on_a_menu_and_the_menus_still_fit() {
    use crate::ui::menubar::{Entry, Title};
    let door = |a: ActionId| {
        Title::ALL.iter().any(|t| {
            t.entries().iter().any(|e| matches!(e, Entry::Act(x) | Entry::Switch(x) | Entry::Choice(x) if *x == a))
        })
    };
    for a in [
        ActionId::CloseGap,
        ActionId::Freeze,
        ActionId::PasteAttributes,
        ActionId::PasteColor,
        ActionId::PasteEffects,
        ActionId::PasteTransform,
        ActionId::PasteSpeed,
        ActionId::PasteAudio,
        ActionId::MatchFrame,
        ActionId::ReplaceEdit,
        ActionId::SelectForward,
        ActionId::SelectForwardAll,
        ActionId::ExtendAudio,
        ActionId::ExtendVideo,
        ActionId::NudgeBack,
        ActionId::NudgeForward,
        ActionId::NudgeBack10,
        ActionId::NudgeForward10,
    ] {
        assert!(door(a), "{a:?} has a menu row");
    }
    // `every_menubar_menu_fits_1280_by_720` walks every list, flyouts included.
}

/// A freeze's still is named for its clip, and a second one of the same clip gets a number,
/// never the first one's name: the timeline reads "Freeze – <clip>" for the first.
#[test]
fn a_freeze_still_is_named_for_its_clip_and_numbered_after_that() {
    assert_eq!(still_name("Freeze \u{2013} test_av", 1), "Freeze \u{2013} test_av.png");
    assert_eq!(still_name("Freeze \u{2013} test_av", 2), "Freeze \u{2013} test_av 2.png");
}
