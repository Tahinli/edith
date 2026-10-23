//! The preview's title cards: the geometry that places them over the picture
//! and the frames they are up on. The drawing is `ui::preview::title_overlay`;
//! these pin what it is told.

use crate::titles::{Rect, active_titles, contain, em_px, opacity, outline_offsets, title_area};
use engine::Title;
use engine::text::{Outline, TextStyle};

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn title(start: u32, frames: u32, fade_in: u32, fade_out: u32) -> Title {
    Title {
        start,
        frames,
        text: "Chapter one".into(),
        style: TextStyle::default(),
        fade_in,
        fade_out,
        in_anim: Default::default(),
        out_anim: Default::default(),
        anim_frames: 0,
    }
}

/// A wide picture in a square region is as wide as the region and letterboxed
/// top and bottom, centred: the picture's own shape, not the region's.
#[test]
fn a_wide_picture_fits_the_width_and_centres_vertically() {
    let fitted = contain(rect(0., 0., 1000., 1000.), 1920., 1080.);
    assert!((fitted.w - 1000.).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.h - 562.5).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.x - 0.).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.y - 218.75).abs() < 1e-3, "{fitted:?}");
}

/// A tall picture in a wide region is as tall as the region and pillarboxed
/// left and right, offset by the region's own origin.
#[test]
fn a_square_picture_in_a_wide_region_is_pillarboxed_from_the_region_origin() {
    let fitted = contain(rect(10., 20., 1920., 1080.), 1080., 1080.);
    assert!((fitted.h - 1080.).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.w - 1080.).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.x - 430.).abs() < 1e-3, "{fitted:?}");
    assert!((fitted.y - 20.).abs() < 1e-3, "{fitted:?}");
}

/// Nothing to fit, nothing to place: an empty picture or region gives the
/// region back, and the drawing draws no title into it.
#[test]
fn an_empty_picture_or_region_fits_to_the_region() {
    let region = rect(0., 0., 800., 450.);
    assert_eq!(contain(region, 0., 1080.), region);
    assert_eq!(contain(rect(0., 0., 0., 450.), 1920., 1080.), rect(0., 0., 0., 450.));
}

/// A title's size is its permille of the displayed picture height, the share
/// the engine takes of the frame height. The default caption size (50) on a
/// 1080-high picture is 54 px, and a tiny size keeps the six-pixel floor.
#[test]
fn a_title_size_is_a_permille_of_the_displayed_height() {
    let style = TextStyle {
        size: 50,
        ..TextStyle::default()
    };
    assert!((em_px(&style, 1080.) - 54.).abs() < 1e-4);
    assert!((em_px(&style, 540.) - 27.).abs() < 1e-4);
    let tiny = TextStyle {
        size: 10,
        ..TextStyle::default()
    };
    assert_eq!(em_px(&tiny, 100.), 6.);
}

/// Words are set inside the title-safe margin, 5% of the picture height on all
/// four sides, which is the engine's margin (it measures both ways from the
/// height).
#[test]
fn title_words_sit_inside_the_five_percent_safe_margin() {
    let area = title_area(rect(100., 50., 1920., 1080.));
    assert!((area.x - 154.).abs() < 1e-3, "{area:?}");
    assert!((area.y - 104.).abs() < 1e-3, "{area:?}");
    assert!((area.w - 1812.).abs() < 1e-3, "{area:?}");
    assert!((area.h - 972.).abs() < 1e-3, "{area:?}");
}

/// The outline is the engine's stroke: em/24 for thin (four diagonals), em/12
/// for thick (eight around), each at least its floor.
#[test]
fn the_outline_copies_follow_the_engines_stroke_radius() {
    assert!(outline_offsets(Outline::None, 54.).is_empty());
    assert_eq!(
        outline_offsets(Outline::Thin, 54.),
        vec![(-2., -2.), (2., -2.), (-2., 2.), (2., 2.)]
    );
    let thick = outline_offsets(Outline::Thick, 54.);
    assert_eq!(thick.len(), 8);
    assert!(thick.iter().all(|&(dx, dy)| {
        [dx, dy].iter().all(|c| [-5., 0., 5.].contains(c))
    }));
    assert_eq!(outline_offsets(Outline::Thin, 6.), vec![(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)]);
}

/// The opacity is the alpha as a fraction: nothing at 0, all of it at 255.
#[test]
fn opacity_is_the_alpha_as_a_fraction() {
    assert_eq!(opacity(0), 0.);
    assert_eq!(opacity(255), 1.);
    assert!((opacity(128) - 128. / 255.).abs() < 1e-6);
}

/// Titles up at a frame come back in list order with their alpha, so a later
/// title draws over an earlier one; a title outside its span is not among them,
/// and one that has faded to nothing is not either.
#[test]
fn the_titles_up_at_a_frame_are_listed_in_order_with_their_alpha() {
    let early = title(0, 15, 5, 0);
    let late = title(10, 20, 0, 0);
    let titles = [early.clone(), late.clone()];

    let at_0 = active_titles(&titles, 0);
    assert_eq!(at_0.len(), 1);
    assert_eq!(at_0[0].0, &early);
    assert_eq!(at_0[0].1, 42);

    let at_12 = active_titles(&titles, 12);
    let order: Vec<&Title> = at_12.iter().map(|(t, _)| *t).collect();
    assert_eq!(order, vec![&early, &late]);
    assert_eq!(at_12[0].1, 255);

    let at_25 = active_titles(&titles, 25);
    assert_eq!(at_25.len(), 1);
    assert_eq!(at_25[0].0, &late);
    assert!(active_titles(&titles, 35).is_empty());
}

/// A title's end frame is not in it: it is up from `start` to `start + frames`
/// and no further, and a fade-out has it ramp down to nothing before that.
#[test]
fn a_title_is_up_for_exactly_its_frames_and_fades_out_within_them() {
    let faded = title(0, 20, 0, 5);
    let titles = [faded];
    assert!(active_titles(&titles, 19)[0].1 > 0);
    assert!(active_titles(&titles, 20).is_empty());
    let last = active_titles(&titles, 19)[0].1;
    let before = active_titles(&titles, 15)[0].1;
    assert!(last < before && last > 0, "fade-out ramps: {before} -> {last}");
}

// The title editor: where it is mounted and what owns the keyboard.
use super::src_text;

#[test]
fn new_title_is_a_file_new_row_with_alt_t_and_the_t_key_stays_subtitles() {
    use crate::ui::menubar::{Entry, Title as MenuTitle};
    fn walk(list: &[Entry], a: crate::keymap::ActionId) -> bool {
        list.iter().any(|e| match e {
            Entry::Sub(_, inner) => walk(inner, a),
            Entry::Act(x) | Entry::Switch(x) | Entry::Choice(x) => *x == a,
            Entry::Rule => false,
        })
    }
    assert!(walk(MenuTitle::Timeline.layout(), crate::keymap::ActionId::NewTitle));
    let k = crate::keymap::Keymap::defaults();
    assert_eq!(k.display(crate::keymap::ActionId::NewTitle), "Alt+T");
    assert_eq!(k.lookup("t", false, false, false), Some(crate::keymap::ActionId::ToggleSubtitles));
    assert!(src_text("player/actions.rs").contains("ActionId::NewTitle => self.new_title(cx)"));
}

#[test]
fn the_title_section_is_in_the_clip_tab_and_every_control_is_one_write() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains("title_card(room, cx)"));
    let card = src_text("ui/cards/title.rs");
    for needle in ["apply_title_preset", "style.place", "style.outline", "style.boxed", "ScrubField::AnimLength", "title_anim_row", "ScrubField::Size", "remove_title_selected"] {
        assert!(card.contains(needle), "{needle}");
    }
    assert!(!card.contains("set_title("), "the card writes through Player::write_title only");
}

/// A click away and enter write the same words: trailing spaces and breaks go, a line break inside
/// stays, and words that are blank keep the title's old ones.
#[test]
fn a_title_draft_commits_its_words_and_blank_words_keep_the_old_ones() {
    use crate::player::titling::committed_title_words;
    assert_eq!(committed_title_words("Intro hit  \n").as_deref(), Some("Intro hit"));
    assert_eq!(committed_title_words("Line one\nLine two").as_deref(), Some("Line one\nLine two"));
    assert_eq!(committed_title_words("   "), None);
    assert_eq!(committed_title_words(""), None);
}

#[test]
fn typing_in_the_title_field_owns_the_keyboard_ahead_of_the_chords_and_escape_leaves_it() {
    let stance = src_text("ui/stance.rs");
    let field = stance.find("this.text_field_key(event, window, cx)").expect("text field dispatch");
    let palette = stance.find("if this.palette_open {").expect("palette branch");
    assert!(field < palette, "the field is asked before the palette and the keymap");
    let blur = src_text("player/blur.rs");
    assert!(blur.contains("self.title_key(key, ctrl, shift, alt, key_char, cx)"));
    let titling = src_text("player/titling.rs");
    assert!(titling.contains("\"escape\" => self.title_draft = None"), "escape reverts the title");
    assert!(titling.contains("\"enter\" if shift =>"), "shift+enter inserts a newline");
    assert!(titling.contains("\"enter\" => self.commit_title_draft(cx)"), "enter commits");
}

#[test]
fn a_title_is_reachable_by_double_click_on_the_timeline_row_and_on_the_picture() {
    assert!(src_text("ui/bench_stance/titles_row.rs").contains("click_count() >= 2"));
    assert!(src_text("ui/preview.rs").contains("this.open_title(i, event.click_count() >= 2, cx)"));
    assert!(src_text("ui/bench_stance/mod.rs").contains("titles_row::titles_row"));
}

/// The source of the function whose signature starts with `sig`, up to its closing brace at the
/// method's indent.
fn fn_body(src: &str, sig: &str) -> String {
    let at = src.find(sig).unwrap_or_else(|| panic!("no `{sig}` in the source"));
    let rest = &src[at..];
    let end = rest.find("\n    }\n").map_or(rest.len(), |e| e + 6);
    rest[..end].to_string()
}

/// One selection model: a bare-bed press, a gap, a caption and Escape let go of the other kinds of
/// pick, so Backspace can never remove a title nothing shows as picked. Pinned on the bodies that
/// do it, since the bug was a title pick outliving the click that should have ended it.
#[test]
fn one_pick_at_a_time_and_bare_bed_lets_go_of_the_rest() {
    let edit = src_text("player/editkeys.rs");
    let begin = fn_body(&edit, "pub(crate) fn begin_marquee");
    assert!(!begin.contains("title_sel = None"), "the release, not the press, lets the title go");
    let finish = fn_body(&edit, "pub(crate) fn finish_marquee");
    assert!(finish.contains("self.drop_title_selection(cx);") && finish.contains("self.drop_sel_cue();"), "{finish}");
    assert!(finish.contains("self.selected = Selection::new();"), "a plain click on bare bed picks nothing");

    let view = src_text("player/view.rs");
    let clear = fn_body(&view, "pub(crate) fn clear_picks");
    for needle in ["self.selected = Selection::new();", "self.gap = None;", "self.drop_title_selection(cx);", "self.drop_sel_cue();"] {
        assert!(clear.contains(needle), "clear_picks misses {needle}");
    }
    let actions = src_text("player/actions.rs");
    assert!(actions.contains("ActionId::Deselect => self.clear_picks(cx),"));

    let timeline = src_text("player/timeline_edit.rs");
    let gap = fn_body(&timeline, "pub(crate) fn select_gap(");
    assert!(gap.contains("self.drop_title_selection(cx);") && gap.contains("self.drop_sel_cue();"), "{gap}");

    let titling = src_text("player/titling.rs");
    let target = fn_body(&titling, "pub(crate) fn title_target");
    assert!(target.contains("self.sel_cue.is_some()") && target.contains("self.gap_pick().is_some()"), "{target}");
    let open = fn_body(&titling, "pub(crate) fn open_title(");
    assert!(open.contains("self.drop_sel_cue();"), "a title opened over a caption leaves the caption picked");

    let cues = src_text("player/caption_cues.rs");
    let menu = fn_body(&cues, "pub(crate) fn open_cue_menu");
    assert!(menu.contains("self.drop_title_selection(cx);") && menu.contains("self.gap = None;"), "{menu}");
}

/// The clip panel starts at the top for every change of pick (clip, caption or title), and the
/// caption style body draws only under its own head.
#[test]
fn the_panel_scrolls_to_top_on_any_pick_and_the_style_body_needs_its_head() {
    let dock = src_text("ui/dock_stance.rs");
    assert!(dock.contains("let picked: PanelPick = (anchor, player.sel_cue, player.title_sel);"));
    assert!(dock.contains("Cell<PanelPick> = const { std::cell::Cell::new((None, None, None)) }"));
    assert!(dock.contains("on_subtitle.then(|| section_body(player.subtitle_style_card(room, cx))).flatten()"));
}

/// Escape in a caption's words gives them up and says so, as a title's words are given up.
#[test]
fn escape_in_a_caption_says_the_edit_was_cancelled() {
    let cues = src_text("player/caption_cues.rs");
    let esc = fn_body(&cues, "pub(crate) fn escape_cue_edit");
    assert!(esc.contains("Caption edit cancelled"), "{esc}");
    let key = fn_body(&cues, "pub(crate) fn cue_edit_key");
    assert!(key.contains("self.escape_cue_edit()"), "{key}");
}

/// A Normalize result is drawn only for the pick and the scope it was read under, and a scope
/// switch drops it.
#[test]
fn a_normalize_result_goes_when_its_scope_or_its_pick_goes() {
    let card = src_text("ui/cards/tools.rs");
    assert!(card.contains("t.set_said(Tool::Normalize, Said::default());"));
    assert!(card.contains(".when(self.result_shown(t), |d|"));
    let shown = fn_body(&card, "fn result_shown(");
    assert!(shown.contains("self.selected.anchor(), self.sel_cue, self.title_sel, t.each"), "{shown}");
}
