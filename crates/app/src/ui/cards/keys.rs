//! The keyframe rows the transform and colour cards share: the stopwatch
//! diamond that puts a key on the playhead (or takes the one there off), and --
//! with a key on the playhead -- the chips that pick its easing.

use crate::ui::icons::Icon;
use crate::ui::kit::{self, Segment};
use crate::ui::type_scale::{self, Typeset};
use crate::ui::widgets::*;
use crate::*;
use engine::project::KeyKind;
use engine::transform::KeyEase;

/// What a key of `kind` is called in a row and its tooltips.
fn kind_label(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Placement => "Position and size",
        KeyKind::Opacity => "Opacity",
        KeyKind::Color => "Colour",
    }
}

/// The ease as the segmented row names it: whole words, the row has its own line.
fn ease_text(ease: KeyEase) -> &'static str {
    match ease {
        KeyEase::Linear => "Linear",
        KeyEase::EaseIn => "Ease in",
        KeyEase::EaseOut => "Ease out",
        KeyEase::Ease => "Smooth",
        KeyEase::Hold => "Hold",
    }
}

/// The row's own label: the colour card's row sits among sliders, so it says what it animates.
fn row_label(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Color => "Colour keyframes",
        other => kind_label(other),
    }
}

fn kind_id(kind: KeyKind) -> usize {
    match kind {
        KeyKind::Placement => 0,
        KeyKind::Opacity => 1,
        KeyKind::Color => 2,
    }
}

/// A diamond of `size` px: filled (a key is on the playhead) or an outline.
pub(super) fn diamond(size: f32, colour: u32, filled: bool) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let (o, s) = (bounds.origin, bounds.size);
            let (cx_, cy_) = (o.x + s.width / 2., o.y + s.height / 2.);
            let half = px(size / 2. - if filled { 0. } else { 0.75 });
            let mut path = if filled { gpui::PathBuilder::fill() } else { gpui::PathBuilder::stroke(px(1.5)) };
            path.move_to(gpui::point(cx_, cy_ - half));
            path.line_to(gpui::point(cx_ + half, cy_));
            path.line_to(gpui::point(cx_, cy_ + half));
            path.line_to(gpui::point(cx_ - half, cy_));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, rgb(colour));
            }
        },
    )
    .size(px(size))
}

impl Player {
    /// Puts the playhead on the next (`forward`) or the previous key of `kind` of the open
    /// card's clip, and says so when there is none that way.
    pub(super) fn jump_key(&mut self, kind: KeyKind, forward: bool, cx: &mut Context<Self>) {
        let Some((lane, idx)) = self.key_clip(kind) else { return };
        let Some(session) = self.session.as_ref() else { return };
        let ats: Vec<u32> = match kind {
            KeyKind::Placement => session.transform_keys_of(lane, idx).iter().map(|k| k.at).collect(),
            KeyKind::Opacity => session.opacity_keys_of(lane, idx).iter().map(|k| k.at).collect(),
            KeyKind::Color => session.color_keys_of(lane, idx).iter().map(|k| k.at).collect(),
        };
        let Some(clip) = session.lane_clips(lane).get(idx) else { return };
        let (start, end, now) = (clip.start, clip.end(), self.key_frame());
        let on_key = |t: &u32| session.key_source_frame(lane, idx, *t).is_some_and(|s| ats.contains(&s));
        let hit = match forward {
            true => (now.saturating_add(1)..end).find(on_key),
            false => (start..now.min(end)).rev().find(on_key),
        };
        match hit {
            Some(t) => {
                let at = f64::from(t) / self.active_fps();
                self.seek(at, cx);
            }
            None => {
                self.notify_user(if forward { "No later key on this clip" } else { "No earlier key on this clip" }.into());
                cx.notify();
            }
        }
    }

    /// One keyframe row: the name, then previous key, the diamond (filled with a key on the
    /// playhead, an outline without; lit once the clip is keyed at all) and next key; under it,
    /// the easing of the key on the playhead.
    pub(super) fn key_row(&self, kind: KeyKind, cx: &mut Context<Self>) -> AnyElement {
        let here = self.key_here(kind);
        let keyed = self.key_active(kind);
        let name = kind_label(kind);
        let t = crate::ui::theme::tokens();
        let ink = match (here.is_some(), keyed) {
            (true, _) => t.accent,
            (false, true) => t.ink1,
            _ => t.ink3,
        };
        let diamond = div()
            .id(("key-diamond", kind_id(kind)))
            .flex_none()
            .size(px(crate::layout::redesign::CONTROL_M))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(crate::layout::redesign::R_CONTROL))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(crate::ui::theme::tokens().e3)))
            .tooltip(overlay_tip_hover(
                &if here.is_some() {
                    format!("Remove the {name} key at the playhead")
                } else {
                    format!("Key {name} at the playhead")
                },
                "",
                None,
            ))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.toggle_key_here(kind, cx);
            }))
            .child(diamond(12., ink, here.is_some()));
        let (m1, m2) = (cx.entity(), cx.entity());
        let none = (!keyed).then_some("No keys yet");
        let me = cx.entity();
        // The ease of the key on the playhead gets a line of its own, whole words. The line is
        // always there: without a key on the playhead it is disabled (and says why), so setting
        // the first key never moves the rows under it.
        let eases = kit::segmented_equal(
            "key-ease",
            &KeyEase::ALL.map(|e| Segment::text(ease_text(e))),
            here.and_then(|current| KeyEase::ALL.iter().position(|e| *e == current)).unwrap_or(usize::MAX),
            here.is_some(),
            None,
            move |pick, _, app| {
                me.update(app, |this, cx| this.set_key_ease_here(kind, KeyEase::ALL[pick], cx));
            },
        );
        // With no key on the playhead the row is disabled, and its reason rides a clear layer
        // over the chips: a tooltip on the row itself did not reach the pointer past the chips.
        let eases = div()
            .id(("key-ease-row", kind_id(kind)))
            .relative()
            .w_full()
            .child(eases)
            .when(here.is_none(), |d| {
                d.child(
                    div()
                        .id(("key-ease-why", kind_id(kind)))
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .size_full()
                        .tooltip(overlay_tip_hover("Add a key first", "", None)),
                )
            });
        // The colour card's rows run to its edge; the transform card's sit 6px in.
        let inset = if kind == KeyKind::Color { 0. } else { 6. };
        div()
            .id(("key-row", kind_id(kind)))
            .w_full()
            .px(px(inset))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .type_style(type_scale::body())
                            .text_color(rgb(if keyed { t.ink1 } else { t.ink2 }))
                            .child(row_label(kind)),
                    )
                    .child(kit::icon_ghost("key-prev", Icon::ChevronLeft, "Previous key", none, move |_, app| {
                        m1.update(app, |this, cx| this.jump_key(kind, false, cx));
                    }))
                    .child(diamond)
                    .child(kit::icon_ghost("key-next", Icon::ChevronRight, "Next key", none, move |_, app| {
                        m2.update(app, |this, cx| this.jump_key(kind, true, cx));
                    })),
            )
            .child(eases)
            .into_any_element()
    }

    /// The opacity row: the same slider and typed pill as Scale and Rotation, 0 to 100 percent.
    pub(super) fn opacity_row(&self, opacity: f32, cx: &mut Context<Self>) -> AnyElement {
        let row = SliderRow {
            id: "transform-opacity-slider",
            label: "Opacity".into(),
            value: format!("{:.0}%", opacity * 100.).into(),
            fraction: slider_fraction(opacity, 0., 1.),
            changed: (opacity - 1.).abs() > 0.005,
            bar: self.opacity_bar.clone(),
            on_press: Box::new(move |this: &mut Player, x: Pixels, cx: &mut Context<Player>| {
                this.opacity_drag = true;
                this.transform_dragging = true;
                this.drag_opacity(x, true, cx);
            }),
            on_reset: Some(Rc::new(move |this: &mut Player, cx: &mut Context<Player>| {
                this.set_opacity(1., cx);
            })),
            // Typed in percent: "40" is 40% opacity.
            typed: Some(SliderTyped {
                lo: 0.,
                hi: 100.,
                set: Rc::new(|this: &mut Player, v: f64, cx: &mut Context<Player>| {
                    this.set_opacity((v / 100.) as f32, cx);
                }),
            }),
        };
        div()
            .id("transform-opacity")
            .w_full()
            .flex()
            .flex_col()
            .px(px(6.))
            .rounded(px(3.))
            .tooltip(overlay_tip_hover("Opacity", "", None))
            .child(slider_row(row, cx))
            .into_any_element()
    }
}
