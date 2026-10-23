//! The subtitle style card: the app-wide look of the cue plate in the preview.

use super::common::*;
use crate::player::generators::GenPaint;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{self, ScrubLook, Segment};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::{SliderTyped, typed_readout};
use crate::*;
use gpui::Entity;

/// The label column's width: every row's label starts at the same left edge and every control at
/// the same one, as in Settings.
/// The font list never shrinks below a few rows, however short the window.
const LABEL_W: f32 = 112.;
/// The animation list's width, hung from the field's right edge.
const ANIM_LIST_W: f32 = 264.;
/// About the animation list's height: five names, each with its line.
const ANIM_LIST_H: f32 = 220.;

thread_local! {
    /// Where the Animation field was last painted, so the list can open the side with room.
    static ANIM_FIELD_BOUNDS: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
}

/// One row of the card, built like a Settings row: the label on the left, the control on the
/// right. `picked` is the row the arrow keys are on.
fn style_row(id: &'static str, label: &'static str, picked: bool, control: impl IntoElement) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_start()
        .gap(px(8.))
        .min_h(px(crate::layout::redesign::CONTROL_M))
        .py(px(2.))
        .px(px(6.))
        .rounded(px(3.))
        .when(picked, |d| d.bg(rgb(DARK_RAISED())))
        .child(
            div()
                .flex_none()
                .w(px(LABEL_W))
                .h(px(crate::layout::redesign::CONTROL_M))
                .flex()
                .items_center()
                .child(dark_row_label(label, picked)),
        )
        .child(div().flex_1().min_w(px(0.)).flex().flex_col().justify_center().child(control))
}

/// A row that is a switch: the label left, the switch at the right edge, the whole row the click.
fn switch_row(
    id: &'static str,
    label: &'static str,
    picked: bool,
    on: bool,
    toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    style_row(
        id,
        label,
        picked,
        div().flex().justify_end().child(kit::switch((id, 1usize), on, true, None)),
    )
    .cursor_pointer()
    .on_click(move |_, window, cx| toggle(window, cx))
}

/// A write on the player, run from a control's handler: the card's own write, so a click and
/// the keyboard reach the same switch.
fn write(me: &Entity<Player>, set: impl FnOnce(&mut Player, &mut Context<Player>)) -> impl FnOnce(&mut App) {
    let me = me.clone();
    move |app| {
        me.update(app, |this, cx| {
            this.sub_keys_used = false;
            set(this, cx);
            cx.notify();
        })
    }
}

impl Player {
    /// The subtitle style card: the size slider on top, the outline, the box
    /// and the place as chip rows, then the platform's own font list -- each
    /// name drawn in its own face, a picker's rows and not a cycle, so a
    /// hundred-odd families are each one click. Nothing here is a clip's or the
    /// project's, the mix card's shape for the same reason: app-global, kept in
    /// a file beside the theme, and drawn straight off the fields so a change
    /// is on the cue underneath before the card is closed.
    pub(crate) fn subtitle_style_card(
        &self,
        viewport: Size<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.subtitle_style_open {
            return None;
        }
        let help_text = "Use the arrow keys to pick a row and change it.";
        let head = dark_card_head(
            "Subtitle style",
            None,
            Some(help_text.into()),
            Some(self.card_maximized),
            cx,
        );
        // The pick wash shows only once the keyboard has moved a row; a mouse press clears it.
        let picked = |row: usize| self.sub_keys_used && self.subtitle_style_field == row;

        // The slider's track is measured at paint and read back by the drag,
        // so the press and the move share one cell.
        let bar = Rc::new(Cell::new(Bounds::default()));
        let (lo, hi) = SUB_SIZE_RANGE;
        let press_bar = bar.clone();
        let me = cx.entity();
        let size_changed = self.sub_text != SUB_TEXT;
        let size_picked = picked(SUB_ROW_SIZE);
        let size_row = div()
            .id("subtitle-size-row")
            .flex()
            .flex_col()
            .flex_none()
            .px(px(6.))
            .py(px(2.))
            .rounded(px(3.))
            .when(size_picked, |d| d.bg(rgb(DARK_RAISED())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .min_h(px(crate::layout::redesign::CONTROL_M))
                    .child(dark_row_label("Text size", size_picked))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(typed_readout(
                                "subtitle-size-value",
                                &format!("{:.0} px", self.sub_text),
                                None,
                                40.,
                                if size_changed { INK1() } else { INK2() },
                                // Typed in pixels, clamped to the size range.
                                Some(SliderTyped {
                                    lo: f64::from(lo),
                                    hi: f64::from(hi),
                                    set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                                        this.set_sub_size(v as f32, cx);
                                    }),
                                }),
                                None,
                                cx,
                            ))
                            .child(kit::reset_slot("subtitle-size-reset", size_changed, {
                                let me = me.clone();
                                move |_, app| write(&me, |this, cx| this.set_sub_size(SUB_TEXT, cx))(app)
                            })),
                    ),
            )
            .child(super::tools::slider_track(
                "subtitle-size",
                slider_fraction(self.sub_text, lo, hi),
                bar.clone(),
                move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                    this.subtitle_style_field = SUB_ROW_SIZE;
                    this.sub_keys_used = false;
                    this.sub_dragging = true;
                    this.drag_sub_size(x, press_bar.get(), cx);
                },
                cx,
            ));
        let outline_row = style_row(
            "subtitle-outline-row",
            "Outline",
            picked(SUB_ROW_OUTLINE),
            kit::segmented_equal(
                "subtitle-outline",
                &["None", "Thin", "Thick"].map(Segment::text),
                [SubOutline::None, SubOutline::Thin, SubOutline::Thick].iter().position(|o| *o == self.sub_outline).unwrap_or(0),
                true,
                None,
                {
                    let me = me.clone();
                    move |i, _, app| {
                        let o = [SubOutline::None, SubOutline::Thin, SubOutline::Thick][i];
                        write(&me, move |this, cx| this.set_sub_outline(o, cx))(app)
                    }
                },
            ),
        );
        let box_row = switch_row("subtitle-box-row", "Box behind text", picked(SUB_ROW_BOX), self.sub_boxed, {
            let me = me.clone();
            let on = self.sub_boxed;
            move |_, app| write(&me, move |this, cx| this.set_sub_boxed(!on, cx))(app)
        });
        let place_row = style_row(
            "subtitle-place-row",
            "Position",
            picked(SUB_ROW_PLACE),
            kit::segmented_equal(
                "subtitle-place",
                &["Bottom", "Top"].map(Segment::text),
                usize::from(self.sub_place == SubPlace::Top),
                true,
                None,
                {
                    let me = me.clone();
                    move |i, _, app| {
                        let p = if i == 0 { SubPlace::Bottom } else { SubPlace::Top };
                        write(&me, move |this, cx| this.set_sub_place(p, cx))(app)
                    }
                },
            ),
        );

        // The caption animation rows: the project's caption style, as a burn draws it.
        let cs = self
            .session
            .as_ref()
            .map(|s| s.caption_style())
            .unwrap_or_default();
        use engine::captionanim::CaptionAnim;
        let anims = [
            ("None", CaptionAnim::None, "Plain captions"),
            ("Karaoke", CaptionAnim::Karaoke, "Each word lights up as it is spoken"),
            ("Pop", CaptionAnim::Pop, "Each word pops in"),
            ("Bounce", CaptionAnim::Bounce, "Each word bounces in"),
            ("Word by word", CaptionAnim::Word, "Words appear one at a time"),
        ];
        // The animation is a dropdown: the field shows the caption's own, and the list hangs from it
        // with one plain line under each name.
        let anim_at = anims.iter().position(|(_, a, _)| *a == cs.anim).unwrap_or(0);
        // The field's place on the window, measured at paint and kept by the frame after: the list
        // opens upward when the window has no room under the field for all five names.
        let anim_bar = ANIM_FIELD_BOUNDS.with(|b| b.clone());
        let anim_probe = anim_bar.clone();
        let anim_field = kit::dropdown_field("subtitle-anim", anims[anim_at].0, true)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.sub_keys_used = false;
                this.sub_anim_open = !this.sub_anim_open;
                cx.notify();
            }));
        let anim_list = self.sub_anim_open.then(|| {
            let t = crate::ui::theme::tokens();
            let below = f32::from(anim_bar.get().bottom()) + ANIM_LIST_H <= f32::from(viewport.height);
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.sub_anim_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let mut plate = kit::plate(6.)
                .id("subtitle-anim-list")
                .w_full()
                .p(px(crate::layout::MENU_PAD))
                .flex()
                .flex_col()
                .occlude();
            for (k, (name, anim, why)) in anims.iter().enumerate() {
                let anim = *anim;
                plate = plate.child(
                    div()
                        .id(("subtitle-anim-row", k))
                        .flex_none()
                        .px(px(8.))
                        .py(px(4.))
                        .flex()
                        .items_start()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(crate::ui::theme::WASH_HOVER)))
                        .child(div().flex_none().size(px(16.)).children((k == anim_at).then(|| Icon::Check.el(IconSize::Inline, t.accent))))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(div().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(*name))
                                .child(dark_help(*why)),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.sub_anim_open = false;
                            this.sub_keys_used = false;
                            this.edit_caption_style(|c| c.anim = anim, cx);
                        })),
                );
            }
            // Hangs from the field's foot, or stands over it when the window has no room below; its
            // right edge is the field's, so the sidebar holds it.
            let hang = div().absolute().right(px(0.)).w(px(ANIM_LIST_W)).child(plate);
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .child(backdrop)
                    .child(match below {
                        true => hang.top(px(crate::layout::redesign::CONTROL_M + 2.)),
                        false => hang.bottom(px(2.)),
                    }),
            )
        });
        let anim_row = style_row(
            "subtitle-anim-row",
            "Animation",
            picked(SUB_ROW_ANIM),
            div().relative().w_full().child(anim_field).child(bounds_probe(anim_probe)).children(anim_list),
        );
        // The highlight is any colour: the swatches are shortcuts and the hex field takes any.
        let current = (u32::from(cs.highlight[0]) << 16) | (u32::from(cs.highlight[1]) << 8) | u32::from(cs.highlight[2]);
        let swatches = HIGHLIGHT_SWATCHES.iter().enumerate().map(|(i, (name, [r, g, b]))| {
            let colour = (u32::from(*r) << 16) | (u32::from(*g) << 8) | u32::from(*b);
            let rgb_now = [*r, *g, *b];
            let on = colour == current;
            let me = me.clone();
            div()
                .id(("subtitle-hl", i))
                .flex_none()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if on { rgb(tokens().ink1).into() } else { gpui::transparent_black() })
                .cursor_pointer()
                .tooltip(crate::ui::widgets::tip_hover(name, "", None))
                .child(div().size(px(13.)).rounded_full().bg(rgb(colour)).border_1().border_color(rgba(tokens().line)))
                .on_click(move |_, _, app| {
                    write(&me, move |this, cx| {
                        this.gen_hex = None;
                        this.edit_caption_style(|c| c.highlight = rgb_now, cx)
                    })(app)
                })
        });
        let highlight_row = style_row(
            "subtitle-highlight-row",
            "Highlight colour",
            picked(SUB_ROW_HIGHLIGHT),
            div().flex().gap(px(6.)).children(swatches),
        );
        // The hex sits on its own row under the swatches, the way the title card's colour rows do.
        let typing = self.gen_hex.as_ref().filter(|(p, _)| *p == GenPaint::SubHighlight).map(|(_, f)| f.text.clone());
        let hex_value = typing.clone().unwrap_or_else(|| format!("#{current:06X}"));
        let hex_field = kit::scrub_field(
            "subtitle-hl-hex",
            76.,
            hex_value,
            None,
            true,
            typing.as_ref().map(|_| ScrubLook::Typing),
        )
        .cursor(CursorStyle::IBeam)
        .tooltip(crate::ui::widgets::tip_hover("Highlight colour", "type hex", Some("Six hex digits such as ffd600; enter to apply")))
        .on_click({
            let me = me.clone();
            move |_, _, app| write(&me, |this, cx| this.begin_sub_highlight_hex(cx))(app)
        });
        let hex_row = style_row("subtitle-hl-hex-row", "Hex", false, div().flex().child(hex_field));
        let hl_box_row = switch_row("subtitle-hlbox-row", "Box behind word", picked(SUB_ROW_HL_BOX), cs.hl_box, {
            let me = me.clone();
            let on = cs.hl_box;
            move |_, app| write(&me, move |this, cx| this.edit_caption_style(|c| c.hl_box = !on, cx))(app)
        });
        let upper_row = switch_row("subtitle-upper-row", "All capitals", picked(SUB_ROW_UPPERCASE), cs.uppercase, {
            let me = me.clone();
            let on = cs.uppercase;
            move |_, app| write(&me, move |this, cx| this.edit_caption_style(|c| c.uppercase = !on, cx))(app)
        });
        let words_row = style_row(
            "subtitle-words-row",
            "Words per line",
            picked(SUB_ROW_WORDS),
            kit::segmented_equal(
                "subtitle-words",
                &["Any", "1", "2", "3", "4"].map(Segment::text),
                usize::from(cs.words_per_line.min(4)),
                true,
                None,
                {
                    let me = me.clone();
                    move |i, _, app| {
                        let n = i as u8;
                        write(&me, move |this, cx| this.edit_caption_style(|c| c.words_per_line = n, cx))(app)
                    }
                },
            ),
        );

        // The family is a dropdown, as the other card pickers are: the closed field shows the
        // family in its own face, and the open list shows every family in its own face too.
        let families = self.sub_font_rows();
        let current = self.sub_family.clone();
        let shown = current.clone().unwrap_or_else(|| "System default".to_string());
        let font_open = self.sub_font_open;
        let query = self.sub_font_query.clone();
        let field = kit::dropdown_field("subtitle-font", shown.clone(), true)
            .font_family(shown)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.sub_font_open = !this.sub_font_open;
                this.sub_font_query.clear();
                cx.notify();
            }));
        let font_list = font_open.then(|| {
            let backdrop = div().absolute().top(px(-4000.)).left(px(-4000.)).size(px(8000.)).on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.sub_font_open = false;
                    this.sub_font_query.clear();
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
            let text = cx.text_system().clone();
            let labels: Vec<SharedString> = families
                .iter()
                .map(|f| f.clone().unwrap_or_else(|| "System default".to_string()).into())
                .collect();
            // A row is drawn in its own face only when that face spells the name (see `family_face_ok`).
            let faces: Vec<Option<SharedString>> = families
                .iter()
                .map(|f| f.as_ref().filter(|name| family_face_ok(&text, name)).map(|name| name.clone().into()))
                .collect();
            let current_ix = families.iter().position(|f| *f == current).unwrap_or(0);
            let me = cx.entity();
            let pick = move |i: usize, _: &mut Window, app: &mut App| {
                me.update(app, |this, cx| {
                    this.sub_keys_used = false;
                    this.sub_font_open = false;
                    this.sub_font_query.clear();
                    this.set_sub_family(families[i].clone(), cx);
                })
            };
            let list = kit::choice_list_faced(SUB_FONT_LIST, 300., &labels, &faces, current_ix, pick);
            // The search line sits on the list, and both are drawn in the one plate look.
            let search = kit::plate(6.)
                .id("subtitle-font-search")
                .w(px(300.))
                .h(px(crate::layout::redesign::CONTROL_M))
                .mb(px(4.))
                .px(px(10.))
                .flex()
                .items_center()
                .type_style(type_scale::body())
                .text_color(rgb(if query.is_empty() { tokens().ink3 } else { tokens().ink1 }))
                .child(if query.is_empty() { "Search fonts".to_string() } else { format!("{query}\u{258f}") });
            gpui::deferred(
                div()
                    .absolute()
                    .top(px(0.))
                    .right(px(0.))
                    .child(backdrop)
                    // Opens upward: the card sits at the foot of the inspector, where a list below would run off the window.
                    .child(
                        div()
                            .absolute()
                            .bottom(px(crate::layout::redesign::CONTROL_M + 2.))
                            .right(px(0.))
                            .w(px(300.))
                            .flex()
                            .flex_col()
                            .child(search)
                            .child(list),
                    ),
            )
        });
        let font_drop = div().relative().w_full().child(field).children(font_list);

        let move_bar = bar.clone();
        Some(
            self.card_outer(viewport, cx)
                // A held slider follows the pointer over the whole window, and
                // the button coming up anywhere keeps what it was left at.
                .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                    if this.sub_dragging {
                        this.drag_sub_size(event.position.x, move_bar.get(), cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        this.end_sub_drag(cx);
                    }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(card_max_w(
                            COLOR_W,
                            self.card_maximized,
                            f32::from(viewport.width),
                        )))
                        .on_mouse_down(MouseButton::Left, swallow)
                        .max_h(px((f32::from(viewport.height) - 120.).clamp(360., 640.)))
                        .flex()
                        .flex_col()
                        .gap(px(CARD_GAP))
                        .map(|d| self.card_surface(d))
                        .map(move |d| d.child(head))
                        .child(size_row)
                        .child(outline_row)
                        .child(box_row)
                        .child(place_row)
                        .child(anim_row)
                        .child(highlight_row)
                        .child(hex_row)
                        .child(hl_box_row)
                        .child(upper_row)
                        .child(words_row)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .child(div().px(px(6.)).child(dark_row_label("Font", false)))
                                .child(font_drop),
                        )
                        .child(dark_help("Size, outline, box, position and font are kept for every project. Animation, highlight, capitals and words per line are saved with this project.")),
                ),
        )
    }
}

/// The id of the caption font list, which the arrow keys and the search line name.
const SUB_FONT_LIST: &str = "subtitle-font-list";

impl Player {
    /// The rows of the caption font list: the platform's own choice first, then every family
    /// whose name holds the search words (see [`font_matches`]).
    pub(crate) fn sub_font_rows(&self) -> Vec<Option<String>> {
        std::iter::once(None)
            .chain(
                self.subtitle_fonts
                    .iter()
                    .filter(|name| !name.starts_with('.') && font_matches(name, &self.sub_font_query))
                    .cloned()
                    .map(Some),
            )
            .collect()
    }

    /// The open font list's keys: letters, digits and spaces narrow it, Backspace takes a letter
    /// back, and Escape clears the search and then closes the list. Up, Down and Enter are the
    /// kit list's ([`kit::list_nav`]), so they are not taken here. `true` when the key was taken.
    pub(crate) fn sub_font_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) -> bool {
        if !self.sub_font_open || ctrl || alt {
            return false;
        }
        match key {
            "up" | "down" | "enter" => return false,
            "escape" if self.sub_font_query.is_empty() => self.sub_font_open = false,
            "escape" => self.sub_font_query.clear(),
            "backspace" => {
                self.sub_font_query.pop();
            }
            _ => {
                let Some(chars) = key_char else { return false };
                self.sub_font_query.extend(chars.chars().filter(|c| !c.is_control()));
            }
        }
        // A search puts the highlight on its first family (row 1: row 0 is the platform's own
        // choice); an empty search puts it back on the family in force.
        let rows = self.sub_font_rows();
        let ix = if self.sub_font_query.is_empty() {
            rows.iter().position(|f| *f == self.sub_family).unwrap_or(0)
        } else {
            usize::from(rows.len() > 1)
        };
        kit::set_list_highlight(SUB_FONT_LIST, ix);
        cx.notify();
        true
    }
}

/// Whether a family's name holds the search words: every word of `query`, ignoring case, is in
/// `name`. An empty query holds every name.
pub(crate) fn font_matches(name: &str, query: &str) -> bool {
    let name = name.to_lowercase();
    query.split_whitespace().all(|word| name.contains(&word.to_lowercase()))
}

/// Name parts of the symbol, emoji, icon and math fonts that a name cannot be read in (the fallback
/// for [`family_face_ok`]), and [`symbolic_name`]'s list.
const SYMBOL_FACES: [&str; 10] = ["dingbat", "d050000l", "symbol", "wingding", "webding", "emoji", "zapf", "awesome", "math", "ornament"];

thread_local! {
    /// Whether each family can draw its own name in its own face, checked once per name.
    static FACE_OK: std::cell::RefCell<std::collections::HashMap<String, bool>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Whether a family's name says it is a symbol, emoji or math face (see [`SYMBOL_FACES`]).
pub(crate) fn symbolic_name(family: &str) -> bool {
    let lower = family.to_ascii_lowercase();
    SYMBOL_FACES.iter().any(|w| lower.contains(w))
}

/// Whether `family` may draw its own name in its own face: it must have every Latin letter, and
/// it must not be a mono face (the UI keeps mono to the timecode). A symbol font has none of the
/// letters, so its name is drawn in the UI font instead. Both are read off the font's own glyphs.
pub(crate) fn family_face_ok(text: &gpui::TextSystem, family: &str) -> bool {
    FACE_OK.with(|cache| {
        if let Some(ok) = cache.borrow().get(family) {
            return *ok;
        }
        const SIZE: f32 = 12.;
        let id = text.resolve_font(&gpui::font(family.to_string()));
        let latin = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
            .chars()
            .all(|ch| text.typographic_bounds(id, px(SIZE), ch).is_ok());
        // In a mono face every glyph has one advance: an i is as wide as an M.
        let mono = match (text.advance(id, px(SIZE), 'i'), text.advance(id, px(SIZE), 'M')) {
            (Ok(i), Ok(m)) => (f32::from(i.width) - f32::from(m.width)).abs() < 0.01,
            _ => true,
        };
        // Some symbol fonts map the Latin letters onto their own pictures, so the coverage check
        // passes for them: the names of the known ones are the fallback.
        let ok = latin && !mono && !symbolic_name(family);
        cache.borrow_mut().insert(family.to_string(), ok);
        ok
    })
}

#[cfg(test)]
mod font_list_tests {
    use super::*;

    #[test]
    fn a_search_holds_every_word_whatever_the_case() {
        assert!(font_matches("Noto Serif", ""), "an empty search holds every name");
        assert!(font_matches("Noto Serif", "serif"));
        assert!(font_matches("Noto Serif", "NOTO ser"));
        assert!(!font_matches("Noto Serif", "sans"));
        assert!(!font_matches("Noto Serif", "serif mono"), "every word must be in the name");
    }

    #[test]
    fn symbol_emoji_math_and_icon_faces_are_not_caption_faces() {
        for name in ["Font Awesome 6 Free", "Noto Emoji", "MnSymbol", "Latin Modern Math", "Zapf Dingbats"] {
            assert!(symbolic_name(name), "{name} is a symbol face");
        }
        for name in ["Noto Serif", "DejaVu Sans", "Liberation Serif"] {
            assert!(!symbolic_name(name), "{name} is a text face");
        }
    }
}
