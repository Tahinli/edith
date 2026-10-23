//! The audio lane's behaviour the GUI gestures rely on: the clip volume envelope's keys
//! (add, move, remove, one undo each) and the ruler's audible scrub gate.

use super::*;
use crate::player::transport::scrub_snippet_wanted;
use engine::GainKey;

/// Adding a key puts it in frame order, moving one keeps the others where they were, and
/// removing one leaves the rest; each is one undo step back to the envelope before it.
#[test]
fn envelope_keys_add_move_and_remove_through_the_session_one_undo_each() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let lane = Lane::A1;
    let clip = session.lane_clips(lane)[0].clone();
    let before = clip.gain_keys.clone();
    let (first, last) = (clip.in_frame + 10, clip.in_frame + 40);

    assert!(session.add_gain_key(lane, 0, GainKey::new(last, -12.)));
    assert!(session.add_gain_key(lane, 0, GainKey::new(first, -6.)));
    let added = session.gain_keys_of(lane, 0).to_vec();
    let ats: Vec<u32> = added.iter().map(|k| k.at).collect();
    assert!(ats.windows(2).all(|w| w[0] < w[1]), "keys sorted: {ats:?}");
    assert!(session.undo());
    let only_last = session.gain_keys_of(lane, 0).to_vec();
    assert!(session.redo());
    assert_eq!(session.gain_keys_of(lane, 0), added.as_slice());

    let mut moved = added.clone();
    let i = moved.iter().position(|k| k.at == first).unwrap();
    moved[i] = GainKey::new(first + 3, -9.);
    assert!(session.set_gain_keys(lane, 0, moved.clone()));
    assert!(session.gain_keys_of(lane, 0).iter().any(|k| k.at == first + 3 && k.db == -9.));

    assert!(session.remove_gain_key(lane, 0, first + 3));
    assert_eq!(session.gain_keys_of(lane, 0).len(), added.len() - 1);

    assert!(session.undo());
    assert_eq!(session.gain_keys_of(lane, 0), moved.as_slice());
    assert!(session.undo());
    assert_eq!(session.gain_keys_of(lane, 0), added.as_slice());
    assert!(session.undo());
    assert_eq!(session.gain_keys_of(lane, 0), only_last.as_slice());
    assert!(session.undo());
    assert_eq!(session.gain_keys_of(lane, 0), before.as_slice());
}

/// Dragging the ruler sounds a snippet only while paused and the switch is on.
#[test]
fn a_ruler_scrub_is_heard_only_while_paused_and_switched_on() {
    assert!(scrub_snippet_wanted(true, true));
    assert!(!scrub_snippet_wanted(true, false), "playing: the timeline is already sounding");
    assert!(!scrub_snippet_wanted(false, true), "switched off");
}
