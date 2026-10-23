//! The Media tab's missing-file, menu and filter rules.

use crate::menus::RowItem;
use crate::oracle::{Enable, RowCtx, row_enable, row_items};

fn ctx() -> RowCtx {
    RowCtx { timeline: true, usable: true, ..RowCtx::default() }
}

#[test]
fn a_missing_file_can_only_be_relinked() {
    let gone = RowCtx { offline: true, ..ctx() };
    assert!(row_enable(RowItem::Relink, gone).yes());
    for item in [RowItem::Add, RowItem::Preview, RowItem::Proxy] {
        assert_eq!(row_enable(item, gone), Enable::No("the file is missing"));
    }
    assert!(row_items(gone).contains(&RowItem::Relink));
    assert!(!row_items(ctx()).contains(&RowItem::Relink));
}

#[test]
fn a_refused_remove_says_why_in_plain_words() {
    let used = RowCtx { placed: 1, ..ctx() };
    assert_eq!(row_enable(RowItem::Remove, used), Enable::No("It's used on the timeline"));
}

#[test]
fn the_type_filter_tells_video_audio_and_images_apart() {
    let ext = |name: &str| std::path::PathBuf::from(name);
    assert!(crate::ui::dock_stance::kind_matches(1, &ext("a.mp4")));
    assert!(crate::ui::dock_stance::kind_matches(2, &ext("a.wav")));
    assert!(crate::ui::dock_stance::kind_matches(3, &ext("a.png")));
    assert!(!crate::ui::dock_stance::kind_matches(1, &ext("a.png")));
}
