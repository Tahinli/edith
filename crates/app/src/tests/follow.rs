//! Live follow: the decision on a project file another writer saved, the stamp
//! that says when a file is settled, and the menu action that reloads it.

use super::*;
use crate::follow::{Follow, Stamp, follow_decision, stamp};
use std::time::{Duration, SystemTime};

fn at(secs: u64, len: u64) -> Stamp {
    Stamp {
        mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
        len,
    }
}

/// The table in `follow.rs`, row for row.
#[test]
fn follow_decision_table() {
    let known = Some(at(10, 100));
    // Nothing settled to read: wait, whatever the window holds.
    assert_eq!(follow_decision(known, None, false), Follow::Wait);
    assert_eq!(follow_decision(known, None, true), Follow::Wait);
    assert_eq!(follow_decision(None, None, false), Follow::Wait);
    // No stamp yet: take the file as it is, silently.
    assert_eq!(
        follow_decision(None, Some(at(10, 100)), false),
        Follow::Adopt
    );
    assert_eq!(
        follow_decision(None, Some(at(10, 100)), true),
        Follow::Adopt
    );
    // Unchanged: nothing, edits or no edits.
    assert_eq!(
        follow_decision(known, Some(at(10, 100)), false),
        Follow::Idle
    );
    assert_eq!(
        follow_decision(known, Some(at(10, 100)), true),
        Follow::Idle
    );
    // Changed with no edits held: reload.
    assert_eq!(
        follow_decision(known, Some(at(11, 100)), false),
        Follow::Reload
    );
    assert_eq!(
        follow_decision(known, Some(at(10, 101)), false),
        Follow::Reload
    );
    // Changed under edits held: ask.
    assert_eq!(follow_decision(known, Some(at(11, 100)), true), Follow::Ask);
    assert_eq!(follow_decision(known, Some(at(10, 101)), true), Follow::Ask);
}

/// A same-length rewrite in the same second is still a change: the mtime is
/// what moves, and the length alone must not hide it.
#[test]
fn same_length_rewrite_in_the_same_second_is_a_change_only_by_mtime() {
    let known = Some(at(10, 100));
    let later = Stamp {
        mtime: at(10, 100).mtime + Duration::from_millis(250),
        len: 100,
    };
    assert_eq!(follow_decision(known, Some(later), false), Follow::Reload);
}

/// A settled file stamps; a file the engine is still writing, or that is
/// empty, or that is gone, does not.
#[test]
fn stamp_only_reads_a_settled_file() {
    let dir = std::env::temp_dir().join(format!("edith-follow-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let project = dir.join("cut.edith");

    assert_eq!(stamp(&project), None, "a missing file has no stamp");
    assert_eq!(stamp(std::path::Path::new("")), None, "no path, no stamp");

    std::fs::write(&project, b"edith-project").unwrap();
    let settled = stamp(&project).expect("a written file stamps");
    assert_eq!(settled.len, 13);

    // The engine writes `<path>.part` and renames it over the file: while the
    // sibling exists the file is not final, whatever it holds.
    let part = dir.join("cut.edith.part");
    std::fs::write(&part, b"partial").unwrap();
    assert_eq!(
        stamp(&project),
        None,
        "a file with a .part beside it is mid-write"
    );
    std::fs::remove_file(&part).unwrap();
    assert_eq!(stamp(&project), Some(settled));

    std::fs::write(&project, b"").unwrap();
    assert_eq!(stamp(&project), None, "an empty file is not settled");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The menu and palette action: registered, named, with no stroke, filed under
/// File. The keys card lists it from [`ActionId::ALL`].
#[test]
fn reload_project_is_a_menu_action_with_no_stroke() {
    assert!(ActionId::ALL.contains(&ActionId::ReloadProject));
    assert_eq!(ActionId::ReloadProject.label(), "Reload project from disk");
    let keys = crate::keymap::Keymap::defaults();
    assert_eq!(keys.chord(ActionId::ReloadProject), "--");
    assert_eq!(
        ActionId::ReloadProject.category(),
        crate::keymap::Category::File
    );
}
