//! The component gallery: every token, type role, icon and component of the
//! redesign kit painted in every state on one scrolling page. It is the P1
//! exit test (captured headless at 1920x1080 and 1280x720) and the place to
//! look when a later phase restyles a region and wants to know what a state
//! is *supposed* to look like.
//!
//! Reach it from the command palette (Ctrl+K), typing `component gallery`
//! and pressing Enter; Escape or the Close button leaves. It is not an
//! action: it has no menu entry, no key and no place in the keymap.

use crate::layout::redesign as r;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::kit::{
    self, Button, ButtonSize, ButtonStyle, Look, ScrubLook, Segment, keycap, scrub_field,
    section_head, segmented, switch,
};
use crate::ui::theme::{ClipKind, tokens};
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use gpui::{Hsla, SharedString};
use std::sync::atomic::AtomicUsize;

static OPEN: AtomicBool = AtomicBool::new(false);
static SEG: AtomicUsize = AtomicUsize::new(1);
static SWITCH: AtomicBool = AtomicBool::new(true);
static TOGGLE: AtomicBool = AtomicBool::new(true);

/// What the palette types to reach the gallery.
pub(crate) const PALETTE_PHRASE: &str = "component gallery";

pub(crate) fn is_open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

pub(crate) fn set_open(open: bool) {
    OPEN.store(open, Ordering::Relaxed);
}

fn c(x: u32) -> Hsla {
    rgb(x).into()
}

fn panel(title: &str) -> Div {
    div()
        .flex_none()
        .w(px(400.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .rounded(px(r::R_CARD))
        .bg(c(tokens().e1))
        .child(section_head(title.to_string(), None))
}

/// Keeps a child at its own width inside a column panel (columns stretch).
fn left(el: impl IntoElement) -> Div {
    div().flex().child(el)
}

fn row() -> Div {
    div().flex().flex_wrap().items_center().gap(px(8.))
}

fn note(text: &str) -> Div {
    div()
        .type_style(type_scale::caption())
        .text_color(c(tokens().ink3))
        .child(text.to_string())
}

fn swatch(name: &str, fill: u32, ink: u32) -> Div {
    div()
        .flex_none()
        .w(px(86.))
        .h(px(40.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(r::R_CONTROL))
        .bg(c(fill))
        .border_1()
        .border_color(c(tokens().edge))
        .type_style(type_scale::caption())
        .text_color(c(ink))
        .child(name.to_string())
}

fn state_label(text: &str) -> Div {
    note(text).w(px(64.))
}

const LOOKS: [(&str, Option<Look>); 4] = [
    ("Rest", None),
    ("Hover", Some(Look::Hover)),
    ("Pressed", Some(Look::Pressed)),
    ("Focus", Some(Look::Focus)),
];

fn type_panel() -> Div {
    let t = tokens();
    let line = |style: type_scale::TypeStyle, text: &str, ink: u32| {
        div().type_style(style).text_color(c(ink)).child(text.to_string())
    };
    panel("Type")
        .child(line(type_scale::title(), "Title 16/22 Inter 600", t.ink1))
        .child(line(type_scale::emph(), "Emphasis 14/20 Inter 500", t.ink1))
        .child(line(type_scale::body(), "Body 13/18 Inter 400, H.264 \u{b7} 1280\u{d7}720 \u{b7} 25 fps", t.ink1))
        .child(line(type_scale::body_strong(), "Body that acts 13/18 Inter 500", t.ink1))
        .child(line(type_scale::caption(), "Caption 12/16 Inter 400 0123456789", t.ink2))
        .child(line(type_scale::caption_strong(), "Clip name 12/16 Inter 500", t.ink1))
        .child(line(type_scale::section(), "Section head 12/16 Inter 600", t.ink2))
        .child(line(type_scale::timecode(), "00:00:02:22", t.ink1))
        .child(line(type_scale::body(), "\u{414}\u{43e}\u{431}\u{440}\u{43e} \u{3b5}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac} \u{c5}ngstr\u{f6}m \u{15f}\u{11f}\u{131}", t.ink3))
}

fn surface_panel() -> Div {
    let t = tokens();
    panel("Surfaces and ink")
        .child(
            row()
                .child(swatch("e0 window", t.e0, t.ink1))
                .child(swatch("e1 panel", t.e1, t.ink1))
                .child(swatch("e2 raised", t.e2, t.ink1))
                .child(swatch("e3 hover", t.e3, t.ink1))
                .child(swatch("e4 pressed", t.e4, t.ink1))
                .child(swatch("well", t.well, t.ink1))
                .child(swatch("matte", t.matte, t.ink1))
                .child(swatch("edge", t.edge, t.e0)),
        )
        .child(
            row()
                .child(swatch("ink1", t.e1, t.ink1))
                .child(swatch("ink2", t.e1, t.ink2))
                .child(swatch("ink3", t.e1, t.ink3))
                .child(swatch("ink4 off", t.e1, t.ink4)),
        )
        .child(
            row()
                .child(swatch("accent", t.accent, t.e0))
                .child(swatch("hover", t.accent_hover, t.e0))
                .child(swatch("press", t.accent_press, t.on_accent))
                .child(swatch("fill", t.accent_fill, t.on_accent)),
        )
        .child(
            row()
                .child(swatch("err", t.e1, t.err))
                .child(swatch("warn", t.e1, t.warn))
                .child(swatch("ok", t.e1, t.ok)),
        )
}

fn clip_panel() -> Div {
    let t = tokens();
    let names = [
        ("Video", Icon::Film),
        ("Image", Icon::Image),
        ("Audio", Icon::AudioWaveform),
        ("Title", Icon::Type),
        ("Caption", Icon::Captions),
        ("Compound", Icon::Layers),
        ("Multicam", Icon::Clapperboard),
    ];
    let mut p = panel("Clip colours by media type");
    for (kind, (name, icon)) in ClipKind::ALL.into_iter().zip(names) {
        let tone = t.clip(kind);
        p = p.child(
            div()
                .h(px(32.))
                .flex()
                .items_center()
                .rounded(px(r::R_CLIP))
                .overflow_hidden()
                .bg(c(tone.body))
                .child(
                    div()
                        .h_full()
                        .w(px(3.))
                        .bg(c(t.clip(ClipKind::Video).ink)),
                )
                .child(
                    div()
                        .h_full()
                        .w(px(120.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .bg(c(tone.strip))
                        .type_style(type_scale::caption_strong())
                        .text_color(c(tone.label))
                        .child(icon.el(IconSize::Inline, tone.label))
                        .child(name.to_string()),
                )
                .child(
                    div()
                        .px(px(8.))
                        .type_style(type_scale::caption())
                        .text_color(c(tone.ink))
                        .child("waveform / glyph ink"),
                ),
        );
    }
    p
}

fn button_panel() -> Div {
    let mut p = panel("Buttons: primary, secondary, ghost");
    for (name, style) in [
        ("Primary", ButtonStyle::Primary),
        ("Secondary", ButtonStyle::Secondary),
        ("Ghost", ButtonStyle::Ghost),
    ] {
        let mut r_ = row().child(state_label(name));
        for (i, (_, look)) in LOOKS.iter().enumerate() {
            let mut b = Button::new(Box::leak(format!("g-btn-{name}-{i}").into_boxed_str()))
                .label("Export")
                .style(style);
            if let Some(look) = look {
                b = b.look(*look);
            }
            r_ = r_.child(b.build());
        }
        r_ = r_.child(
            Button::new(Box::leak(format!("g-btn-{name}-dis").into_boxed_str()))
                .label("Disabled")
                .style(style)
                .enabled(false)
                .build(),
        );
        p = p.child(r_);
    }
    let mut sizes = row().child(state_label("Sizes"));
    for (i, (label, size)) in [
        ("24", ButtonSize::Small),
        ("28", ButtonSize::Default),
        ("32", ButtonSize::Large),
        ("36", ButtonSize::Hero),
    ]
    .into_iter()
    .enumerate()
    {
        sizes = sizes.child(
            Button::new(Box::leak(format!("g-size-{i}").into_boxed_str()))
                .icon(Icon::Upload)
                .label(label)
                .size(size)
                .build(),
        );
    }
    p.child(sizes)
}

fn icon_button_panel(cx: &mut Context<Player>) -> Div {
    let on = TOGGLE.load(Ordering::Relaxed);
    let mut icons = row().child(state_label("Icon only"));
    for (i, (_, look)) in LOOKS.iter().enumerate() {
        let mut b = Button::new(Box::leak(format!("g-ib-{i}").into_boxed_str()))
            .icon(Icon::Play)
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Large);
        if let Some(look) = look {
            b = b.look(*look);
        }
        icons = icons.child(b.build());
    }
    icons = icons.child(
        Button::new("g-ib-dis")
            .icon(Icon::Play)
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Large)
            .enabled(false)
            .build(),
    );
    let toggles = row()
        .child(state_label("Toggle"))
        .child(
            Button::new("g-tg-off")
                .icon(Icon::Magnet)
                .label("Snap")
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Large)
                .build(),
        )
        .child(
            Button::new("g-tg-on")
                .icon(Icon::Magnet)
                .label("Snap")
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Large)
                .on(true)
                .build(),
        )
        .child(
            Button::new("g-tg-on-hover")
                .icon(Icon::Magnet)
                .label("Snap")
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Large)
                .on(true)
                .look(Look::Hover)
                .build(),
        );
    let live = row().child(state_label("Live")).child(
        Button::new("g-tg-live")
            .icon(if on { Icon::Volume2 } else { Icon::VolumeX })
            .label(if on { "Sound on" } else { "Muted" })
            .style(ButtonStyle::Ghost)
            .size(ButtonSize::Large)
            .min_w(112.)
            .on(on)
            .build()
            .on_click(cx.listener(|_, _, _, cx| {
                TOGGLE.fetch_xor(true, Ordering::Relaxed);
                cx.notify();
            })),
    );
    panel("Icon buttons and toggles")
        .child(icons)
        .child(toggles)
        .child(live)
        .child(note("A toggle swaps the icon shape and the tint; the box never moves."))
}

fn segmented_panel(cx: &mut Context<Player>) -> Div {
    let sel = SEG.load(Ordering::Relaxed);
    let items = |names: &[&str]| names.iter().map(|n| Segment::text(n.to_string())).collect::<Vec<_>>();
    let live = segmented(
        "g-seg-live",
        &items(&["Histogram", "Waveform", "Vector", "Parade"]),
        sel,
        true,
        None,
        {
            let entity = cx.entity();
            move |i, _, cx| {
                SEG.store(i, Ordering::Relaxed);
                entity.update(cx, |_, cx| cx.notify());
            }
        },
    );
    let live = left(live);
    let noop = |_: usize, _: &mut Window, _: &mut App| {};
    panel("Segmented control and switch")
        .child(note("Live (click)"))
        .child(live)
        .child(note("Hover on an unselected pill"))
        .child(left(segmented("g-seg-hover", &items(&["Off", "On"]), 1, true, Some((0, Look::Hover)), noop)))
        .child(note("Focus on the selected pill"))
        .child(left(segmented("g-seg-focus", &items(&["Off", "On"]), 1, true, Some((1, Look::Focus)), noop)))
        .child(note("Disabled"))
        .child(left(segmented("g-seg-dis", &items(&["Fit", "50%", "100%"]), 0, false, None, noop)))
        .child(
            row()
                .child(state_label("Switch"))
                .child(switch("g-sw-off", false, true, None))
                .child(switch("g-sw-on", true, true, None))
                .child(switch("g-sw-hover", false, true, Some(Look::Hover)))
                .child(switch("g-sw-onhover", true, true, Some(Look::Hover)))
                .child(switch("g-sw-focus", true, true, Some(Look::Focus)))
                .child(switch("g-sw-dis", true, false, None)),
        )
        .child(
            row().child(state_label("Live")).child(
                switch("g-sw-live", SWITCH.load(Ordering::Relaxed), true, None).on_click(cx.listener(
                    |_, _, _, cx| {
                        SWITCH.fetch_xor(true, Ordering::Relaxed);
                        cx.notify();
                    },
                )),
            ),
        )
}

fn field_panel() -> Div {
    let t = tokens();
    let labelled = |name: &str, field: Stateful<Div>| {
        row()
            .child(div().w(px(64.)).type_style(type_scale::body()).text_color(c(t.ink2)).child(name.to_string()))
            .child(field)
    };
    panel("Scrub-number fields, section head, keycaps")
        .child(labelled("Rest", scrub_field("g-sf-rest", 88., "1.00", Some("\u{d7}"), true, None)))
        .child(labelled("Hover", scrub_field("g-sf-hover", 88., "1.00", Some("\u{d7}"), true, Some(ScrubLook::Hover))))
        .child(labelled("Typing", scrub_field("g-sf-type", 88., "0.75", Some("\u{d7}"), true, Some(ScrubLook::Typing))))
        .child(labelled("Disabled", scrub_field("g-sf-dis", 88., "1.00", Some("\u{d7}"), false, None)))
        .child(section_head(
            "Speed",
            Some(
                Button::new("g-reset")
                    .icon(Icon::RotateCcw)
                    .style(ButtonStyle::Ghost)
                    .size(ButtonSize::Small)
                    .build()
                    .into_any_element(),
            ),
        ))
        .child(
            row()
                .child(state_label("Keycaps"))
                .child(keycap("Space"))
                .child(keycap("Ctrl"))
                .child(keycap("K"))
                .child(keycap("\u{2190}")),
        )
}

static COLOUR: AtomicUsize = AtomicUsize::new(0x7c5cff);
static DROP: AtomicUsize = AtomicUsize::new(1);

/// The card components: equal and wrapping segmented rows, switch rows, value
/// pills with the reserved reset slot, number fields, swatches with a custom
/// colour, a card dropdown, ghost icon buttons and keycaps.
fn card_panel(cx: &mut Context<Player>) -> Div {
    let items = |names: &[&str]| names.iter().map(|n| Segment::text(n.to_string())).collect::<Vec<_>>();
    let noop = |_: usize, _: &mut Window, _: &mut App| {};
    let nop = |_: &mut Window, _: &mut App| {};
    let me = cx.entity();
    let colour = COLOUR.load(Ordering::Relaxed) as u32;
    let drop_items: Vec<SharedString> = ["Fit", "Fill", "Stretch"].iter().map(|s| SharedString::from(*s)).collect();
    panel("Card components")
        .child(note("Equal-width segmented"))
        .child(kit::segmented_equal("g-seg-eq", &items(&["Left", "Centre", "Right"]), 1, true, None, noop))
        .child(note("Wrapping segmented (never clips a label)"))
        .child(div().w(px(220.)).child(kit::segmented_wrap(
            "g-seg-wrap",
            &items(&["Cinematic", "Documentary", "Vivid", "Muted", "Black and white"]),
            2,
            true,
            None,
            noop,
        )))
        .child(note("Switch rows: on, with a description, disabled with a reason"))
        .child(kit::switch_row("g-swr-1", "Loop playback", None, true, None, nop))
        .child(kit::switch_row("g-swr-2", "Burn captions", Some("Draw the captions into the picture".into()), false, None, nop))
        .child(kit::switch_row("g-swr-3", "Hardware encoding", Some("Faster export".into()), false, Some("No supported graphics card found"), nop))
        .child(note("Value pills: fixed width, reset slot always reserved"))
        .child(row().child(kit::value_pill("g-vp-1", 64., "1.00", Some("\u{d7}"), false, nop)).child(kit::value_pill("g-vp-2", 64., "1.25", Some("\u{d7}"), true, nop)))
        .child(note("Number field: rest, typing (drag scrubs, click types, Enter commits, Esc cancels)"))
        .child(row()
            .child(kit::number_field("g-nf-1", 88., "1.00", Some("\u{d7}"), None, true))
            .child(kit::number_field("g-nf-2", 88., "", Some("\u{d7}"), Some(&kit::NumberEdit::begin("0.75")), true))
            .child(kit::reset_slot("g-nf-reset", true, nop)))
        .child(note("Swatches: ring on the current colour; a custom one is added to the row"))
        .child(kit::swatch_row(
            "g-sw-row",
            &[0xff5c5c, 0xffb84d, 0x4dd47c, 0x4d9fff],
            colour,
            None,
            {
                let me = me.clone();
                move |rgb, _, cx| {
                    COLOUR.store(rgb as usize, Ordering::Relaxed);
                    me.update(cx, |_, cx| cx.notify());
                }
            },
            nop,
        ))
        .child(note("Card dropdown (click)"))
        .child(div().w(px(220.)).child(kit::dropdown_field("g-dd", drop_items[DROP.load(Ordering::Relaxed)].clone(), true).on_click(cx.listener(|_, _, _, cx| {
            DROP.store((DROP.load(Ordering::Relaxed) + 1) % 3, Ordering::Relaxed);
            cx.notify();
        }))))
        .child(div().w(px(220.)).child(kit::choice_list("g-dd-list", 220., &drop_items, DROP.load(Ordering::Relaxed), nop_pick)))
        .child(note("Ghost icon buttons: move up, move down (disabled, with reason), remove"))
        .child(row()
            .child(kit::icon_ghost("g-ig-up", Icon::ChevronUp, "Move up", None, nop))
            .child(kit::icon_ghost("g-ig-down", Icon::ChevronDown, "Move down", Some("Already last"), nop))
            .child(kit::icon_ghost("g-ig-rm", Icon::Trash2, "Remove", None, nop)))
        .child(note("Keycaps from a chord"))
        .child(row().child(kit::keycaps("Ctrl+Shift+R")).child(kit::keycaps("Ctrl++")).child(kit::keycaps("Space")))
}

fn nop_pick(_: usize, _: &mut Window, _: &mut App) {}

/// The dialog shell, drawn inline (the real one is a scrim over the window).
fn dialog_panel() -> Div {
    panel("Dialog shell")
        .child(
            kit::dialog("g-dialog", 360.)
                .child(kit::dialog_title("Save changes?"))
                .child(kit::dialog_text("Body text sits at ink2 and wraps inside the plate."))
                .child(
                    kit::dialog_actions()
                        .child(Button::new("g-dlg-no").label("Cancel").build())
                        .child(Button::new("g-dlg-yes").label("Save").style(ButtonStyle::Primary).build()),
                ),
        )
}

fn popup_panel() -> Div {
    let t = tokens();
    let menu_row = |icon: Option<Icon>, label: &str, key: &str, look: Option<Look>, enabled: bool| {
        let ink = if enabled { t.ink1 } else { t.ink4 };
        let mut d = div()
            .id(Box::leak(format!("g-mr-{label}").into_boxed_str()) as &'static str)
            .h(px(r::MENU_ROW_H))
            .px(px(12.))
            .flex()
            .items_center()
            .gap(px(8.))
            .type_style(type_scale::body_strong())
            .text_color(c(ink))
            .child(div().w(px(16.)).flex_none().children(icon.map(|i| i.el(IconSize::Inline, if enabled { t.ink2 } else { t.ink4 }))))
            .child(div().flex_1().child(label.to_string()))
            .child(div().type_style(type_scale::caption()).text_color(c(t.ink3)).child(key.to_string()));
        d = kit::states(d, kit::Fills::ghost(), enabled, look);
        d
    };
    panel("One popup look: plate and menu rows")
        .child(
            kit::plate(r::R_CARD)
                .w(px(280.))
                .py(px(4.))
                .flex()
                .flex_col()
                .child(menu_row(Some(Icon::Scissors), "Split", "S", None, true))
                .child(menu_row(Some(Icon::Trash2), "Delete", "X", Some(Look::Hover), true))
                .child(menu_row(Some(Icon::Eraser), "Lift", "Shift+X", Some(Look::Pressed), true))
                .child(div().h(px(1.)).mx(px(8.)).my(px(4.)).bg(Hsla::from(rgba(t.line))))
                .child(menu_row(None, "Paste", "Ctrl+V", None, false)),
        )
        .child(left(
            kit::plate(r::R_CARD)
                .px(px(8.))
                .py(px(6.))
                .flex()
                .items_center()
                .gap(px(8.))
                .type_style(type_scale::caption())
                .text_color(c(t.ink1))
                .child("Play / Pause")
                .child(keycap("Space")),
        ))
}

fn icon_panel() -> Div {
    let t = tokens();
    let mut grid = row().gap(px(10.));
    for icon in Icon::ALL {
        grid = grid.child(icon.el(IconSize::Inline, t.ink2));
    }
    let mut big = row().gap(px(12.));
    for icon in [Icon::Play, Icon::Scissors, Icon::Magnet, Icon::Upload, Icon::Settings] {
        big = big.child(icon.el(IconSize::Toolbar, t.ink1));
    }
    big = big.child(Icon::Clapperboard.el(IconSize::Hero, t.ink3));
    panel(&format!("Icons: {} Lucide at 16, 20 and 40", Icon::ALL.len()))
        .child(grid)
        .child(big)
        .child(
            row()
                .child(Icon::Eye.stated(IconSize::Toolbar, theme_state(0)))
                .child(Icon::Eye.stated(IconSize::Toolbar, theme_state(1)))
                .child(Icon::Eye.stated(IconSize::Toolbar, theme_state(2)))
                .child(Icon::EyeOff.stated(IconSize::Toolbar, theme_state(3)))
                .child(note("rest, hover, on, disabled")),
        )
}

fn theme_state(i: usize) -> crate::ui::icons::IconState {
    use crate::ui::icons::IconState::*;
    [Rest, Hover, On, Disabled][i]
}

/// The gallery as a window-sized overlay, or nothing while it is closed.
/// Mounted last in the stance so it covers everything, menus included.
pub(crate) fn overlay(cx: &mut Context<Player>) -> Option<AnyElement> {
    if !is_open() {
        return None;
    }
    let t = tokens();
    let head = div()
        .flex_none()
        .h(px(r::MENU_BAR_H + 8.))
        .px(px(16.))
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .type_style(type_scale::title())
                .text_color(c(t.ink1))
                .child(SharedString::from("Component gallery")),
        )
        .child(
            Button::new("g-close")
                .label("Close")
                .size(ButtonSize::Default)
                .build()
                .on_click(cx.listener(|_, _, _, cx| {
                    set_open(false);
                    cx.notify();
                })),
        );
    let body = div()
        .id("gallery-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(16.))
        .pb(px(16.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_start()
                .gap(px(12.))
                .child(type_panel())
                .child(surface_panel())
                .child(clip_panel())
                .child(button_panel())
                .child(icon_button_panel(cx))
                .child(segmented_panel(cx))
                .child(field_panel())
                .child(card_panel(cx))
                .child(dialog_panel())
                .child(popup_panel())
                .child(icon_panel()),
        );
    Some(
        div()
            .id("component-gallery")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(c(t.e0))
            .child(head)
            .child(body)
            .into_any_element(),
    )
}
