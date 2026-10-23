//! The picture: what is drawn over it and what is read off it.

use crate::titles;
use crate::ui::hitmap;
use crate::ui::icons::{Icon, IconSize};
use crate::ui::type_scale::{self, Typeset};
use crate::*;
use std::path::{Path, PathBuf};

/// What the preview's one cached cue picture is *of*: the lane, the moment,
/// the cue's own end, and the picture itself.
///
/// The lane and the start microsecond alone are not a cue. Two bitmap cues can
/// share both -- lift a caption off a lane and drop another track's caption at
/// the same frame -- and with those two as the whole key the cache's `up == key`
/// test matched, painting the picture of the cue that is gone over the one that
/// is up.
///
/// The picture's own allocation ([`Arc::as_ptr`]) is the identity: the cue list
/// a repaint is handed holds the same `Arc` for as long as that cue is in it, so
/// two repaints of one cue compare equal -- which is the whole cache hit -- while
/// two different cues never do. The bytes are never compared: a display set is
/// tens of kilobytes and this test runs on every frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SubImageKey {
    pub(crate) lane: Lane,
    pub(crate) start_us: i64,
    /// The cue's own end, so the same picture recut over the same start reads as
    /// a different cue here too.
    pub(crate) end_us: i64,
    /// Where that picture lives -- [`Arc::as_ptr`] of the cue's image.
    pub(crate) picture: usize,
}

/// [`SubImageKey`] for one cue's picture: the one place the key is built, so the
/// lookup that trusts a hit and the test that pins what a hit means cannot drift
/// apart. `picture` is where the cue's image lives -- the caller passes
/// `Arc::as_ptr` of it, and nothing about the bytes is ever read here.
pub(crate) fn sub_image_key(lane: Lane, start_us: i64, end_us: i64, picture: usize) -> SubImageKey {
    SubImageKey {
        lane,
        start_us,
        end_us,
        picture,
    }
}

/// The width of the welcome hero, and the most of it a project's name may take in a recent row.
const WELCOME_W: f32 = 460.;
const WELCOME_NAME_MAX_W: f32 = 190.;
/// A recent-project row, and the heading above the rows (with its gap): together they are the
/// block the hero reserves, so the hero does not move when the list fills in.
const WELCOME_ROW_H: f32 = 36.;
const WELCOME_HEAD_H: f32 = 28.;
/// The source strip's ruler: room for its labels over the ticks.
const SOURCE_RULER_H: f32 = 28.;

impl Player {
    /// What a window with no project shows in the picture (REDESIGN §3.11): the welcome hero,
    /// the one empty state of the app. New project and Open… are the two ways in, the recent
    /// projects sit under them, and the window itself is the drop target (so the hint says so).
    pub(crate) fn empty_viewer(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
        use crate::ui::theme::tokens;
        let t = tokens();
        // The viewer is only ~240px tall at 1280x720: the hero sheds its icon, then its recent
        // rows, rather than clip (the probe reads 0 until the first frame has been laid out).
        let avail = match f32::from(self.picture_box.get().size.height) {
            h if h > 1. => h - 24.,
            _ => 400.,
        };
        let pending = self.new_project.is_some();
        let show_icon = avail >= 300.;
        let base = if show_icon { 168. } else { 114. };
        let rows = (((avail - base - 52.) / WELCOME_ROW_H).floor().max(0.) as usize).min(WELCOME_RECENT_MAX);
        let all_recent = crate::recent::list();
        let have_recent = !all_recent.is_empty();
        let recent: Vec<crate::recent::Recent> = all_recent.into_iter().take(rows).collect();
        let template_rows_now = if self.welcome_templates { Some(crate::ui::menubar::template_rows(cx, false)) } else { None };
        let row = |i: usize, r: crate::recent::Recent| {
            let open = r.missing.is_none();
            let path = r.path.clone();
            let reason = r.missing.map(crate::recent::Missing::reason);
            // What is left of the row once the icon, the name and the gaps are paid for.
            let name_w = crate::ui::popup::text_w(&r.name, type_scale::BODY_PX, false).min(WELCOME_NAME_MAX_W);
            let left = WELCOME_W - 20. - 16. - 20. - name_w - 6.;
            let folder_chars = (left / crate::ui::popup::text_w("n", type_scale::CAPTION_PX, false)).floor().max(4.) as usize;
            div()
                .id(("welcome-recent", i))
                .h(px(WELCOME_ROW_H))
                .w_full()
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(10.))
                .rounded(px(4.))
                .when(open, |d| {
                    d.cursor_pointer().hover(move |s| s.bg(rgb(t.e2))).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.open_recent(&path, cx);
                    }))
                })
                .child(Icon::Film.el(IconSize::Inline, if open { t.ink2 } else { t.ink4 }))
                .child(
                    div()
                        .flex_none()
                        .max_w(px(WELCOME_NAME_MAX_W))
                        .truncate()
                        .type_style(type_scale::body_strong())
                        .text_color(rgb(if open { t.ink1 } else { t.ink3 }))
                        .child(r.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .type_style(type_scale::caption())
                        .text_color(rgb(t.ink2))
                        .child(reason.map_or_else(|| crate::recent::middle(&r.folder, folder_chars), str::to_string)),
                )
        };
        div()
            .id("viewer-welcome")
            .flex_none()
            .w(px(WELCOME_W))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .when(show_icon, |d| d.child(Icon::Clapperboard.el(IconSize::Hero, t.ink3)))
            .child(
                div()
                    .mt(px(if show_icon { 8. } else { 0. }))
                    .type_style(type_scale::title())
                    .text_color(rgb(t.ink1))
                    .child(match &self.new_project {
                        Some(_) => self.name.to_string(),
                        None => EMPTY_VIEWER_TITLE.to_string(),
                    }),
            )
            .child(div().type_style(type_scale::body()).text_color(rgb(t.ink2)).child(match &self.new_project {
                Some(_) => {
                    let (w, h) = self.pending_settings.0.unwrap_or((1920, 1080));
                    format!("{w}\u{d7}{h} at {:.0} fps: drop footage here or import media", self.pending_settings.1.unwrap_or(30.))
                }
                None => EMPTY_VIEWER_LINE.to_string(),
            }))
            .child(
                div()
                    .mt(px(14.))
                    .flex()
                    .gap(px(8.))
                    .child(
                        // A project waiting for its footage: the import is the way forward, so it takes
                        // the primary style and New project says why it is not offered again.
                        Button::new("welcome-new")
                            .label("New project")
                            .icon(Icon::Plus)
                            .style(if pending { ButtonStyle::Secondary } else { ButtonStyle::Primary })
                            .size(ButtonSize::Hero)
                            .enabled(!pending)
                            .build()
                            .tooltip(match pending {
                                true => crate::ui::widgets::tip_hover("New project", "", Some("A project is waiting for footage: import media to start it")),
                                false => crate::ui::widgets::tip_hover("New project", "Name it, pick a size and rate", None),
                            })
                            .children(hitmap::control("welcome.new", "New project", true))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.open_new_project(cx))),
                    )
                    .child({
                        // From template: the same list as the File menu's, as a dropdown under the button
                        // (absolute, so the buttons beside it do not move when it opens).
                        let mut slot = div().relative().child(
                            Button::new("welcome-template")
                                .label("From template")
                                .icon(Icon::Layers)
                                .style(ButtonStyle::Secondary)
                                .size(ButtonSize::Hero)
                                .build()
                                .tooltip(crate::ui::widgets::tip_hover("From template", "Start from a saved or built-in template", None))
                                .children(hitmap::control("welcome.template", "From template", true))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.welcome_templates = !this.welcome_templates;
                                    cx.notify();
                                })),
                        );
                        if let Some(rows) = template_rows_now {
                            // Deferred, so the viewer's clipping edge does not cut the list short.
                            slot = slot.child(gpui::deferred(crate::ui::popup::popup_plate("welcome-templates", 0., ButtonSize::Hero.height() + 6., 260.).children(rows)));
                        }
                        slot
                    })
                    .child(
                        Button::new("welcome-open")
                            .label("Open project…")
                            .icon(Icon::FolderOpen)
                            .style(ButtonStyle::Secondary)
                            .size(ButtonSize::Hero)
                            .build()
                            .tooltip(crate::ui::widgets::tip_hover("Open project", "", None))
                            .children(hitmap::control("welcome.open", "Open project", true))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_open_project(cx))),
                    )
                    .child(
                        Button::new("welcome-import")
                            .label("Import media…")
                            .icon(Icon::Download)
                            .style(if pending { ButtonStyle::Primary } else { ButtonStyle::Secondary })
                            .size(ButtonSize::Hero)
                            .build()
                            .tooltip(crate::ui::widgets::tip_hover("Import media", "Video, audio or images", None))
                            .children(hitmap::control("welcome.import", "Import media", true))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.pick_and_import(cx))),
                    ),
            )
            // The recent block is always as tall as `rows` rows and its heading, whether the list
            // is long, short or not loaded, so the buttons above it never move.
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .h(px(if have_recent && rows > 0 { WELCOME_HEAD_H + 8. + rows as f32 * WELCOME_ROW_H } else { 0. }))
                    .mt(px(20.))
                    .flex()
                    .flex_col()
                    .when(!recent.is_empty(), |d| {
                        d.child(
                            div()
                                .h(px(WELCOME_HEAD_H))
                                .w_full()
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .type_style(type_scale::section())
                                .text_color(rgb(t.ink2))
                                .child("Recent projects"),
                        )
                        .children(recent.into_iter().enumerate().map(|(i, r)| row(i, r)))
                    }),
            )
    }

    /// The note over the picture when the clip under the playhead reads from a file that is no
    /// longer on the disk: a compact banner along the top of the picture, a warning, the words,
    /// and Relink… beside them, the same relink the media list offers ([`Player::relink_pick`]).
    /// Along the top, not the middle, so the picture and its titles stay in view. `None` otherwise.
    pub(crate) fn missing_clip_note(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        use crate::ui::kit::{Button, ButtonStyle};
        let session = self.session.as_ref()?;
        let offline = session.offline_sources();
        let (lane, idx) = self.under_playhead().into_iter().next()?;
        let source = session.lane_clips(lane).get(idx)?.source;
        let (_, path) = offline.into_iter().find(|(i, _)| *i == source)?;
        let t = crate::ui::theme::tokens();
        Some(
            div()
                .absolute()
                .top(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .pl(px(10.))
                        .pr(px(6.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .bg(rgba(crate::ui::theme::with_alpha(t.e0, 0xD9)))
                        .border_1()
                        .border_color(rgba(crate::ui::theme::with_alpha(t.warn, 0x80)))
                        .type_style(type_scale::body())
                        .text_color(rgb(t.ink1))
                        .child(crate::ui::icons::Icon::TriangleAlert.el_px(px(14.), t.warn))
                        .child(div().whitespace_nowrap().child("This clip's file is missing"))
                        .child(
                            Button::new("viewer-relink")
                                .label("Relink…")
                                .style(ButtonStyle::Secondary)
                                .build()
                                .tooltip(crate::ui::widgets::tip_hover("Relink", "", None))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.relink_pick(path.clone(), cx))),
                        ),
                ),
        )
    }

    /// The source monitor's strip (REDESIGN: the pro-NLE source viewer), a flex
    /// sibling under the picture so nothing is drawn over it. Its mini ruler
    /// spans the source's whole length: click or drag to scrub, the marked
    /// part in..out is the shaded band between two brackets, the playhead is
    /// the white line. Under it the marking and placing controls: In, Out,
    /// clear, which halves go (picture, sound), and Insert / Overwrite at the
    /// timeline's playhead. Every cell keeps its size whatever the state, so
    /// marking or toggling moves nothing.
    pub(crate) fn source_strip(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        use crate::ui::kit::{Button, ButtonSize, ButtonStyle};
        let session = self.preview_session.as_ref()?;
        let t = crate::ui::theme::tokens();
        let short = f32::from(window.viewport_size().height) < 800.;
        let dur = session.timeline_duration().max(0.001);
        let fps = self.active_fps();
        let now = session.now().clamp(0., dur);
        let (a, b) = self
            .source_target
            .as_ref()
            .and_then(|k| self.source_marks.get(k))
            .copied()
            .unwrap_or((None, None));
        let frac = |s: f64| crate::transport::preview_progress_along(s, dur);
        let marked = match (a, b) {
            (None, None) => dur,
            (a, b) => (b.unwrap_or(dur) - a.unwrap_or(0.)).max(0.),
        };
        let path = self.source_target.as_ref().map(|(p, _)| p.clone());
        let has_picture = path.as_ref().is_some_and(|p| !engine::is_audio(p));
        let has_sound = path.as_ref().is_some_and(|p| !engine::is_image(p));
        let can_edit = self.session.is_some() && self.exporting().is_none();
        let cell = |text: String, w: f32| {
            div()
                .flex_none()
                .w(px(w))
                .type_style(type_scale::label_tabular(type_scale::BODY_PX, gpui::FontWeight::MEDIUM))
                .text_color(rgb(t.ink1))
                .whitespace_nowrap()
                .child(text)
        };
        // The ruler's ticks: the timeline ruler's own step rule at this bar's width, a label on
        // a tick only where the labels clear each other.
        let bar_w = match f32::from(self.preview_bar.get().size.width) {
            w if w > 1. => w,
            _ => 700.,
        };
        let step = ruler_step(f64::from(bar_w) / dur, fps, 56.);
        let step_secs = step.secs(fps).max(0.001);
        let ticks: Vec<(f32, Option<String>)> = (0..)
            .map(|i| (i, i as f64 * step_secs))
            .take_while(|(i, at)| *at <= dur && *i < 400)
            .map(|(i, at)| {
                let labelled = f64::from(bar_w) * step_secs / dur >= 56. || i == 0;
                (frac(at), labelled.then(|| ruler_label(step, i, fps)))
            })
            .collect();
        let track = div()
            .id("source-ruler")
            .flex_1()
            .min_w(px(0.))
            .h(px(SOURCE_RULER_H))
            .relative()
            .rounded(px(3.))
            .overflow_hidden()
            .bg(rgb(t.e2))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.preview_scrubbing = true;
                    this.preview_scrub_to(event.position.x, true, cx);
                }),
            )
            .tooltip(crate::ui::widgets::tip_hover("Source ruler", "Click or drag", None))
            .children(hitmap::control("source.ruler", "Source ruler", true))
            .child(bounds_probe(self.preview_bar.clone()))
            .when(a.is_some() || b.is_some(), |d| {
                let (l, r) = (frac(a.unwrap_or(0.)), frac(b.unwrap_or(dur)));
                d.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(relative(l))
                        .w(relative((r - l).max(0.)))
                        .bg(rgb(crate::ui::theme::mix(t.e2, t.accent, 0.35))),
                )
            })
            .children(ticks.into_iter().map(|(at, label)| {
                // A label is skipped where an in or out marker would cover it.
                let label = label.filter(|text| {
                    let x = at * bar_w + 3.;
                    let w = text.chars().count() as f32 * 7. + 2.;
                    [a, b].into_iter().flatten().all(|m| {
                        let mx = frac(m) * bar_w;
                        mx + 3. < x - 4. || mx - 3. > x + w + 2.
                    })
                });
                div()
                    .absolute()
                    .bottom_0()
                    .left(relative(at))
                    .w(px(1.))
                    .h(px(if label.is_some() { 9. } else { 5. }))
                    .bg(rgb(t.ink4))
                    .children(label.map(|text| {
                        div()
                            .absolute()
                            .bottom(px(7.))
                            .left(px(3.))
                            .whitespace_nowrap()
                            .type_style(type_scale::caption())
                            .text_color(rgb(t.ink3))
                            .child(text)
                    }))
            }))
            .children(a.map(|x| {
                div().absolute().top_0().bottom_0().left(relative(frac(x))).w(px(3.)).bg(rgb(t.accent))
            }))
            .children(b.map(|x| {
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(frac(x)))
                    .ml(px(-3.))
                    .w(px(3.))
                    .bg(rgb(t.accent))
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(frac(now)))
                    .w(px(2.))
                    .bg(rgb(t.ink1)),
            );
        let icon_toggle = |id: &'static str, icon: Icon, label: &'static str, on: bool, usable: bool| {
            Button::new(id)
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .icon(icon)
                .label(label)
                .on(on && usable)
                .enabled(usable)
                .build()
        };
        // A mark is its icon and where it is set, one control: a press sets it at the playhead.
        // The word says which mark it is; the value is the moment, or "Not set" in ink3 while unset.
        let mark_btn = |id: &'static str, icon: Icon, word: &'static str, at: Option<f64>, action: ActionId, cx: &mut Context<Self>| {
            let value = match at {
                Some(s) => div().child(timecode(s, fps)),
                None => div().text_color(rgb(t.ink3)).child("Not set"),
            };
            Button::new(id)
                .style(ButtonStyle::Ghost)
                .size(ButtonSize::Small)
                .icon(icon)
                .label(word)
                .on(at.is_some())
                .build()
                .child(value)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.act(action, window, cx)))
                .tooltip(crate::ui::widgets::action_hover(self, action))
                .children(hitmap::action(action, true))
        };
        let place_btn = |id: &'static str, icon: Icon, label: &'static str, action: ActionId, cx: &mut Context<Self>| {
            Button::new(id)
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Small)
                .icon(icon)
                .label(label)
                .enabled(can_edit)
                .build()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.act(action, window, cx)))
                .tooltip(crate::ui::widgets::action_hover(self, action))
                .children(hitmap::action(action, can_edit))
        };
        let marked_any = a.is_some() || b.is_some();
        Some(
            div()
                .id("source-strip")
                .flex_none()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(px(12.))
                .pt(px(4.))
                .pb(px(if short { 2. } else { 6. }))
                .bg(rgb(t.matte))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(cell(timecode(now, fps), 96.))
                        .child(track)
                        .child(cell(timecode(marked, fps), 96.).text_color(rgb(t.ink3)).text_right()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(mark_btn("src-in", Icon::SquareArrowDownRight, "In", a, ActionId::SetIn, cx))
                        .child(mark_btn("src-out", Icon::SquareArrowDownLeft, "Out", b, ActionId::SetOut, cx))
                        .child(
                            Button::new("src-clear")
                                .style(ButtonStyle::Ghost)
                                .size(ButtonSize::Small)
                                .icon(Icon::Eraser)
                                .label("Clear")
                                .enabled(marked_any)
                                .build()
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.act(ActionId::ClearRange, window, cx)
                                }))
                                .tooltip(crate::ui::widgets::tip_hover(
                                    "Clear",
                                    &self.keymap.display(ActionId::ClearRange),
                                    if marked_any { None } else { Some("No In or Out set") },
                                ))
                                .children(hitmap::action(ActionId::ClearRange, marked_any)),
                        )
                        .child(div().flex_1().min_w(px(0.)))
                        .child(
                            icon_toggle("src-video", Icon::Film, "Video", self.source_video, has_picture)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.source_video = !this.source_video;
                                    cx.notify();
                                }))
                                .tooltip(crate::ui::widgets::tip_hover(
                                    "Video",
                                    "",
                                    Some(if has_picture {
                                        "Insert and Overwrite place the picture when this is on"
                                    } else {
                                        "This clip has no picture"
                                    }),
                                ))
                                .children(hitmap::control("source.video", "Source picture", has_picture)),
                        )
                        .child(
                            icon_toggle("src-audio", Icon::AudioWaveform, "Audio", self.source_audio, has_sound)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                    this.source_audio = !this.source_audio;
                                    cx.notify();
                                }))
                                .tooltip(crate::ui::widgets::tip_hover(
                                    "Audio",
                                    "",
                                    Some(if has_sound {
                                        "Insert and Overwrite place the sound when this is on"
                                    } else {
                                        "This clip has no sound"
                                    }),
                                ))
                                .children(hitmap::control("source.audio", "Source sound", has_sound)),
                        )
                        .child(div().w(px(8.)))
                        .child(place_btn("src-insert", Icon::ArrowRightToLine, "Insert", ActionId::InsertSource, cx))
                        .child(place_btn("src-overwrite", Icon::Layers, "Overwrite", ActionId::OverwriteSource, cx)),
                ),
        )
    }

    /// The frame on screen right now, written to a PNG -- at the size the
    /// **project** is, not at the size the preview shows it.
    ///
    /// The picture area asks the session for frames the size of its own box
    /// ([`PlaybackSession::set_view_size`]), so the atlas holds a preview-sized
    /// picture: saving that would cost the user the resolution they asked for
    /// (a still of a 1080p project, an 11th of it). So the seat is asked for
    /// one picture at the project's own size and the file is written when it
    /// lands ([`Player::flush_shot`], off `pump`'s frame-owed signal). Every
    /// other part of the gesture is what it always was: the same directory,
    /// the same name, the same notice.
    ///
    /// A library preview is the same rule about *its* file: the seat paints at
    /// the file's own resolution for it, which is the full size a still of that
    /// file wants.
    pub(crate) fn take_screenshot(&mut self, cx: &mut Context<Self>) {
        if self.shot_pending.is_some() {
            return; // one at a time: the picture is on its way
        }
        let Some(session) = self.active_session() else {
            self.notify_user("No frame to save: nothing is showing yet".into());
            cx.notify();
            return;
        };
        let full = (session.meta().width, session.meta().height);
        let stem = session
            .sources()
            .first()
            .map_or_else(|| self.name.to_string(), |s| file_name(&s.path));
        let stem = std::path::Path::new(&stem)
            .file_stem()
            .map_or(stem.clone(), |s| s.to_string_lossy().into_owned());
        let tc = timecode(session.now(), self.active_fps()).replace(':', "-");
        // The *exact* setter, not the deadbanded push: a view within 8 px of
        // the project is a still of the wrong size. `false` says the view is
        // already at it -- and then the frame in hand *is* the project's frame,
        // so waiting for a picture that would never come (nothing moved, so
        // nothing reseeks) is exactly the trap this avoids.
        let moved = self
            .active_session_mut()
            .is_some_and(|s| s.set_view_size_exact(full.0, full.1));
        if moved {
            // One frame at the project's own size, which is a reseek like any
            // other edit: the frame it owes is what the flush waits on.
            if let Some(session) = self.active_session_mut() {
                session.resync_picture();
            }
            self.shot_pending = Some((self.session_gen, stem, tc));
            self.reset_after_reseek();
        } else if !self.write_shot(&stem, &tc) {
            self.notify_user("No frame to save: nothing is showing yet".into());
        }
        cx.notify();
    }

    /// Writes the picture the atlas is holding, at the size it holds it, and
    /// says whether there was one. The tail [`Player::take_screenshot`] and
    /// [`Player::flush_shot`] share, so the two doors cannot write differently.
    fn write_shot(&mut self, stem: &str, tc: &str) -> bool {
        let Some(image) = self.image.clone() else {
            return false;
        };
        let Some(bytes) = image.as_bytes(0) else {
            return false;
        };
        let size = image.size(0);
        match save_screenshot(bytes, size.width.0 as u32, size.height.0 as u32, stem, tc) {
            Ok(path) => self.notify_user_action(
                format!("Frame saved as {}", file_name(&path)).into(),
                ToastAction::ShowInFolder(path),
            ),
            Err(e) => self.notify_user(format!("Couldn't save the frame: {e}").into()),
        }
        true
    }

    /// The screenshot a preview-sized picture had to wait for: written once the
    /// seat's project-sized frame has landed, then the preview takes its own
    /// size back ([`Player::push_view_size`] pushes it on the next repaint).
    ///
    /// A picture that never lands -- the timeline emptied, a source that died
    /// -- gives up rather than leaving the preview stuck at the project's size:
    /// the wait is bounded, and the notice names what happened.
    pub(crate) fn flush_shot(&mut self, cx: &mut Context<Self>) {
        let Some((asked, stem, tc)) = self.shot_pending.clone() else {
            return;
        };
        if asked != self.session_gen {
            // The session that asked is gone (a preview opened, another file
            // opened, a session closed): its picture is not coming, and the new
            // session's frame must not be written under its name.
            self.shot_pending = None;
            self.notify_user("No frame to save: the session changed meanwhile".into());
            cx.notify();
            return;
        }
        if self
            .seek_since
            .is_some_and(|since| since.elapsed() < std::time::Duration::from_secs(2))
        {
            return; // the frame is still being painted
        }
        self.shot_pending = None;
        // The seat's own clamp is the truth about the size it can paint, which
        // is what the ask settled on ([`PlaybackSession::set_view_size_exact`]),
        // and with the push held while the shot is in flight
        // ([`Player::push_view_size`]) it is still holding it here.
        let want = self.active_session().map(PlaybackSession::view_size);
        let landed = self
            .image
            .as_ref()
            .map(|i| (i.size(0).width.0 as u32, i.size(0).height.0 as u32))
            == want;
        if !landed || !self.write_shot(&stem, &tc) {
            self.notify_user("No frame to save: the picture could not be redrawn".into());
        }
        cx.notify();
    }

    /// The active session has been swapped (`session` or `preview_session`):
    /// what anything keyed to a session has to hear. A screenshot in flight is
    /// the one such thing ([`Player::shot_pending`]), and it is *keyed* rather
    /// than cleared here because a door that forgot to call this would still be
    /// caught by the key -- the picture it is holding came from a session that
    /// is no longer the one on screen.
    pub(crate) fn session_swapped(&mut self) {
        self.session_gen += 1;
        self.title_sel = None;
        self.title_draft = None;
        self.title_hex = None;
        self.gen_hex = None;
        self.title_scrub = None;
        self.pan_scrub = None;
    }

    /// The title cards up at the playhead, over the picture and under the
    /// captions ([`Player::picture_area`] mounts this before the cue plate).
    /// Each title is set in its own style the way the engine burns it: its size
    /// is a share of the *displayed* picture's height, the words sit in the
    /// title-safe margins at the style's place, a box is the engine's translucent
    /// black, and the opacity is the title's alpha at the playhead's frame.
    ///
    /// Only the timeline's own titles, and only with no preview open: a preview
    /// is another file, as the captions are. Not an exact match of the export:
    /// the words are the app's family, not the engine's bundled face, and the
    /// wrap is the browser's line break.
    pub(crate) fn title_overlay(&self, at: f64, dpr: f32, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.preview_session.is_some() {
            return None;
        }
        let session = self.session.as_ref()?;
        if session.titles().is_empty() {
            return None;
        }
        // The picture is contained in the region the way it is drawn, so the
        // titles are placed against the picture and not the letterbox around it.
        let region = self.picture_box.get();
        let region = titles::Rect {
            x: 0.,
            y: 0.,
            w: f32::from(region.size.width),
            h: f32::from(region.size.height),
        };
        let meta = session.meta();
        let picture = match self.viewer.zoom {
            crate::viewer_view::Zoom::Fit => titles::contain(region, meta.width as f32, meta.height as f32),
            // Zoomed and panned: against the picture as drawn, which may be
            // bigger than the region (the region clips) or smaller than it.
            _ => self.viewer_frame_local(dpr).map_or(region, |(x, y, w, h)| titles::Rect { x, y, w, h }),
        };
        if picture.w <= 0. || picture.h <= 0. {
            return None;
        }
        let frame = frame_at(at, self.active_fps());
        // The selected title is drawn as it is being edited: the words typed so far and the
        // number being scrubbed laid over what the project holds.
        let picked = self.title_target().map(|(i, _)| i);
        let shown: Vec<engine::Title> = session
            .titles()
            .iter()
            .enumerate()
            .map(|(i, t)| if picked == Some(i) { self.shown_title(t) } else { t.clone() })
            .collect();
        // Each title's motion at this frame ([`engine::Title::motion_at`]), the same one the export draws.
        let up: Vec<(usize, &engine::Title, u8, engine::titleanim::Motion)> = titles::active_titles(&shown, frame)
            .into_iter()
            .filter_map(|(t, a)| Some((shown.iter().position(|s| std::ptr::eq(s, t))?, t, a, t.motion_at(frame)?)))
            .collect();
        if up.is_empty() {
            return None;
        }
        let family = self.sub_family.clone();
        // The font a title's text is set in before its family and weight: the window's own.
        let base_font = window.text_style().font();
        // Each title is laid in the picture's own coordinates, so the layer
        // below is picture-local and the whole stack sits at the picture's box.
        let local = titles::Rect {
            x: 0.,
            y: 0.,
            w: picture.w,
            h: picture.h,
        };
        let area = titles::title_area(local);
        // The transient bars along the bottom edge are over the picture, and a
        // title that sits at the bottom steps up over them the way a caption
        // does, so the words are not drawn under a notice.
        let bars = f32::from(self.notice_h.get());
        let layers = up.into_iter().map(|(i, title, alpha, motion)| {
            // The size the animation has now, and the words it shows; the box and outline follow the size.
            let mut style = title.style.clamped();
            style.size = ((style.size as f32 * motion.scale).round() as u32).clamp(engine::text::SIZE_RANGE.0, engine::text::SIZE_RANGE.1);
            // The letters the reveal has let through; the rest of the text is laid out, not drawn.
            let shown = motion.revealed(&title.text).map(|s| s.chars);
            let em = titles::em_px(&style, picture.h);
            let (pad_x, pad_y) = (titles::BOX_PAD_X_EM * em, titles::BOX_PAD_Y_EM * em);
            // How its lines line up, and whether the letters are bold: the engine's Align and Weight.
            let look = (
                match style.align {
                    engine::text::Align::Left => TextAlign::Left,
                    engine::text::Align::Centre => TextAlign::Center,
                    engine::text::Align::Right => TextAlign::Right,
                },
                style.weight == engine::text::Weight::Bold,
            );
            let [ir, ig, ib] = style.ink;
            let [br, bg, bb, ba] = style.box_color;
            let outline = titles::outline_offsets(style.outline, em)
                .into_iter()
                .map(|(dx, dy)| {
                    title_lines(&title.text, em, family.as_deref(), rgba(SUB_SHADE()), look, shown, &base_font)
                        .absolute()
                        .top(px(dy))
                        .left(px(dx))
                        .right(px(-dx))
                });
            // A press selects the title and a double-click puts the keyboard in its words.
            let face = div()
                .id(("preview-title", i))
                .relative()
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.open_title(i, event.click_count() >= 2, cx);
                }))
                .px(px(pad_x))
                .py(px(pad_y))
                .when(style.boxed, |el| {
                    // The default box is the subtitle plate's own; a chosen one is what was chosen.
                    el.bg(if style.box_color == engine::text::TextStyle::default().box_color {
                        rgba(SUB_SHADE())
                    } else {
                        rgba(u32::from_be_bytes([br, bg, bb, ba]))
                    })
                })
                .children(outline)
                .child(title_lines(
                    &title.text,
                    em,
                    family.as_deref(),
                    if style.ink == engine::text::TextStyle::default().ink {
                        rgb(SUB_FG())
                    } else {
                        rgb(u32::from_be_bytes([0, ir, ig, ib]))
                    },
                    look,
                    shown,
                    &base_font,
                ));
            // The block hangs from the top, sits in the middle, or stands off
            // the bottom of the area, as its place says: how tall it is depends
            // on how its words wrap, so the flex column does the placing.
            let lift = match style.place {
                engine::text::Place::Bottom => bars,
                _ => 0.,
            };
            let column = div()
                .absolute()
                .left(px(area.x))
                .top(px(area.y))
                .w(px(area.w))
                .h(px(area.h - lift).max(px(0.)))
                .flex()
                .flex_col();
            let column = match style.hplace {
                engine::text::HPlace::Left => column.items_start(),
                engine::text::HPlace::Centre => column.items_center(),
                engine::text::HPlace::Right => column.items_end(),
            };
            let column = match style.place {
                engine::text::Place::Top => column.justify_start(),
                engine::text::Place::Centre => column.justify_center(),
                engine::text::Place::Bottom => column.justify_end(),
            };
            // The slide is an offset of the whole column.
            column
                .left(px(area.x + motion.dx * picture.w))
                .top(px(area.y + motion.dy * picture.h))
                .opacity(titles::opacity(alpha))
                .child(face)
        });
        Some(
            div()
                .absolute()
                .left(px(picture.x))
                .top(px(picture.y))
                .w(px(picture.w))
                .h(px(picture.h))
                .overflow_hidden()
                .children(layers.collect::<Vec<_>>()),
        )
    }

    /// The cues the *subtitle lanes* put on screen at `at`, over the picture and
    /// nothing else: bottom-centred where every player puts them, white on a
    /// plate so the film underneath cannot swallow them, and each cue its own
    /// plate so two at one moment stack rather than run together.
    ///
    /// What is drawn is what was *placed*: a track walked out of a file lands in
    /// the palette and nothing more, exactly as a picture or a song does, and it
    /// reaches the screen when a placement of it sits under the playhead
    /// ([`PlaybackSession::sub_lane_cues`], the same map the export writes the
    /// file with -- so what is read here is what the file will say). **One**
    /// lane draws, the one that is shown ([`Player::active_sub_lane`]): a
    /// picture carrying every lane's words at once is unreadable at three lanes
    /// and nonsense at two hundred, which is the same one-track-at-a-time every
    /// player offers. What an export writes is untouched by this -- every lane
    /// on the timeline becomes a subtitle track of the file.
    ///
    /// `None` -- no element at all -- while the toggle is off, with nothing
    /// placed, and between cues: the picture is what this window is for, and a
    /// permanent empty band across it would be in the way of exactly that.
    ///
    /// A cue off a PGS track is a *picture* and not a line
    /// ([`engine::subtitle::CueImage`]), and is drawn as one: the disc's whole
    /// canvas fitted over the picture region exactly as the picture itself is,
    /// which puts every cue where the disc put it relative to its own frame.
    ///
    /// corner-cut: exact only while the canvas and the encode are the same shape
    /// -- a 16:9 canvas over a 2.39:1 encode fits to the region's height and
    /// the film to its width, so a cue sits a little low on a scope film. The
    /// upgrade path is the picture's own rect, which wants `VideoMeta`'s aspect
    /// and the measured bounds rather than the shared `Contain`.
    pub(crate) fn subtitle_overlay(
        &mut self,
        at: f64,
        window: &mut Window,
    ) -> Option<impl IntoElement + use<>> {
        // One way out, and it lets the drawn picture go on the way: the toggle
        // going off, the file closing and the gap between two cues are the same
        // "nothing on screen", and an 8 MB atlas tile may not survive any of
        // them (an early return above this leaked one per toggle-off).
        // One lane's cues and never a walk of the lanes: the map is asked of the
        // shown lane alone, so two hundred subtitle lanes cost this frame what
        // one does.
        let shown: Option<(Lane, Vec<engine::subtitle::Cue>)> = self
            .active_sub_lane()
            .filter(|_| self.subs_on)
            // A preview is another file: the timeline's words over its
            // picture would caption the wrong film.
            .filter(|_| self.preview_session.is_none())
            .and_then(|lane| {
                let session = self.session.as_ref()?;
                let cues = session.sub_lane_cues(lane);
                let mut now: Vec<_> = cues_at(&cues, at).into_iter().cloned().collect();
                // The words being typed into a caption show on the picture as they are typed,
                // as a title's do: the cue being edited is drawn with the edit's words.
                if let Some(edit) = self.cue_edit.as_ref() {
                    let refs = session.sub_lane_cue_refs(lane);
                    for cue in now.iter_mut() {
                        let is_edited = refs.iter().any(|&(start, end, track, idx)| {
                            start == cue.start_us && end == cue.end_us && (track, idx) == (edit.track, edit.cue)
                        });
                        if is_edited {
                            cue.text = edit.text.clone();
                        }
                    }
                }
                (!now.is_empty()).then_some((lane, now))
            });
        let Some((lane, cues)) = shown else {
            self.drop_sub_image(window);
            return None;
        };
        // The first picture cue up, decoded once and kept: two bitmap cues at
        // one moment is a thing PGS composes into one display set, so there is
        // never a second picture to stack under the first on one lane -- and one
        // lane is all there is here.
        let picture = cues
            .iter()
            .find_map(|cue| Some((cue.start_us, cue.end_us, cue.image.as_ref()?)))
            .and_then(|(start_us, end_us, image)| {
                self.sub_picture(lane, start_us, end_us, image, window)
            });
        // A picture is fitted onto the whole region and a plate hangs off the
        // bottom of it, and a track is one or the other -- so they are two
        // shapes and not one with the parts switched off.
        // What the transient bars along the bottom edge are taking up right now:
        // both shapes step up over it rather than be drawn under it.
        let bars = f32::from(self.notice_h.get());
        if let Some(image) = picture {
            // A *flex* box with the canvas as its one growing item: a percentage
            // size (`size_full`) inside an absolutely placed box has nothing to
            // be a percentage of and lays the picture out to nothing, while a
            // flex item is sized by the box itself. Fitted the way the picture
            // above it is -- `Contain` over the same box -- so a canvas of the
            // picture's own shape lands exactly on it.
            // Slid up by the bars' height and not *shrunk* by it: the top and
            // bottom insets cancel, so the box keeps the region's height and the
            // canvas keeps the fit (and so the scale) it had with nothing
            // hanging there -- a shorter box would refit the disc's canvas and
            // move every cue on it, not just the one over the bar. The picture
            // region clips (`overflow_hidden`), and what leaves the top of a
            // subtitle canvas is transparent.
            return Some(
                div()
                    .absolute()
                    .top(px(-bars))
                    .bottom(px(bars))
                    .left_0()
                    .right_0()
                    .flex()
                    .child(letterboxed_image(image)),
            );
        }
        // An animated (or re-chunked, or upper-cased) project caption is the
        // engine's own drawing, bitmap and all: what the export burns is what
        // this plate shows, and the word being spoken moves with the playhead.
        if let Some(image) = self.animated_caption_image(lane, &cues, at, window) {
            return Some(
                div()
                    .absolute()
                    .top(px(-bars))
                    .bottom(px(bars))
                    .left_0()
                    .right_0()
                    .flex()
                    .child(letterboxed_image(image)),
            );
        }
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .when(self.sub_place == SubPlace::Bottom, |d| {
                    d.bottom(px(sub_bottom(bars)))
                })
                .when(self.sub_place == SubPlace::Top, |d| d.top(px(SUB_BOTTOM)))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                // The plate takes no click: the picture behind it is still the
                // drop target the whole window is.
                //
                // In the lane's own order: two cues at one moment -- a sign and
                // a line of dialogue -- stack, the first on the bottom line
                // where a single cue sits.
                .children(cues.into_iter().filter(|c| c.image.is_none()).map(|cue| {
                    let family = self.sub_family.as_deref();
                    // The outline is the same lines, in the box's own ink,
                    // drawn behind the text at each offset. Pinned to the box's
                    // edges with the offset taken out of both sides, so each
                    // copy wraps and centres exactly as the text does.
                    let outline = self.sub_outline.offsets().iter().map(|&(dx, dy)| {
                        cue_lines(&cue.text, self.sub_text, family, rgba(SUB_SHADE()))
                            .absolute()
                            .top(px(dy))
                            .left(px(dx))
                            .right(px(-dx))
                    });
                    div()
                        .relative()
                        .max_w(relative(0.9))
                        .px(px(6.))
                        .rounded(px(3.))
                        .when(self.sub_boxed, |el| el.bg(rgba(SUB_SHADE())))
                        .children(outline)
                        // A line of the cue is a line on screen: the break the
                        // parser kept is not whitespace to be re-flowed. What a
                        // *long* line does is wrap inside its own div, which is
                        // what the width cap above is for.
                        .child(cue_lines(&cue.text, self.sub_text, family, rgb(SUB_FG())))
                })),
        )
    }

    /// The project caption style's drawing of `cues` at `at`, as the one cached
    /// picture ([`Player::sub_image`]); `None` while the style is plain (the
    /// text plate above draws that), with no session, or where nothing is up.
    /// The cache key is the drawn words and poses
    /// ([`subs::animated_caption_key`]), taken before any text is shaped, so a
    /// held cue costs a hash per repaint and a moving word one drawing per pose.
    pub(crate) fn animated_caption_image(
        &mut self,
        lane: Lane,
        cues: &[engine::subtitle::Cue],
        at: f64,
        window: &mut Window,
    ) -> Option<Arc<RenderImage>> {
        let session = self.session.as_ref()?;
        let caption = session.caption_style();
        if caption.is_plain() && !cues.iter().any(|c| c.real_karaoke().is_some()) {
            return None;
        }
        let style = engine::text::TextStyle {
            outline: match self.sub_outline {
                SubOutline::None => engine::text::Outline::None,
                SubOutline::Thin => engine::text::Outline::Thin,
                SubOutline::Thick => engine::text::Outline::Thick,
            },
            boxed: self.sub_boxed,
            place: match self.sub_place {
                SubPlace::Bottom => engine::text::Place::Bottom,
                SubPlace::Top => engine::text::Place::Top,
            },
            caption,
            // The family the viewer draws the plain caption in, so an animated one matches it.
            family: self.sub_family.clone(),
            ..engine::text::TextStyle::default()
        };
        let resolution = session.resolution();
        let (key, blocks) = animated_caption_key(&style, cues, frame_at(at, self.fps), self.fps)?;
        let key = sub_image_key(lane, i64::from(resolution.0), i64::from(resolution.1), key as usize);
        if let Some((up, ready)) = &self.sub_image
            && *up == key
        {
            return Some(ready.clone());
        }
        let (w, h, bgra) = animated_caption_bitmap(&style, &blocks, resolution)?;
        let buf = image::RgbaImage::from_raw(w, h, bgra)?;
        let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        self.drop_sub_image(window);
        self.sub_image = Some((key, next.clone()));
        Some(next)
    }

    /// The cue `lane` plays from `start_us` to `end_us` as a drawable picture,
    /// decoded on the first repaint it is up for and kept until another cue
    /// takes its place ([`Player::sub_image`]). `None` for a display set the
    /// decoder refuses, which draws nothing rather than failing the frame.
    ///
    /// Its atlas tile is released as the video's is: every [`RenderImage`] gets
    /// a fresh id and its own tile, so a film's worth of cues would grow the
    /// sprite atlas by the whole film.
    pub(crate) fn sub_picture(
        &mut self,
        lane: Lane,
        start_us: i64,
        end_us: i64,
        image: &Arc<engine::subtitle::CueImage>,
        window: &mut Window,
    ) -> Option<Arc<RenderImage>> {
        let key = sub_image_key(lane, start_us, end_us, Arc::as_ptr(image) as usize);
        if let Some((up, ready)) = &self.sub_image
            && *up == key
        {
            return Some(ready.clone());
        }
        let mut rgba = image.rgba()?;
        // gpui's atlas is BGRA with straight alpha; PGS decodes to RGBA with
        // straight alpha. The same swap the video frames get.
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let buf = image::RgbaImage::from_raw(image.width, image.height, rgba)?;
        let next = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        self.drop_sub_image(window);
        self.sub_image = Some((key, next.clone()));
        Some(next)
    }

    /// Lets go of the drawn cue and its atlas tile. Called where the cue stops
    /// being on screen, which is every gap between two of them: an 8 MB tile
    /// per cue is not a thing to leave behind a film.
    pub(crate) fn drop_sub_image(&mut self, window: &mut Window) {
        if let Some((_, old)) = self.sub_image.take() {
            let _ = window.drop_image(old);
        }
    }
}

impl Player {
    /// Which cut the pointer or the keys are working on right now, as the
    /// outgoing clip's index (the incoming one is the next): a trim drag on an
    /// edge, a roll, slip or slide drag, or the `[`/`]` nudge mode
    /// ([`Player::loop_trim`]) on the selected clip. `None` at all other times,
    /// which is the only time the viewer is left whole.
    pub(crate) fn editing_cut(&self) -> Option<(Lane, Option<usize>)> {
        if let Some(t) = self.trim {
            return Some((t.lane, if t.edge == Edge::Start { t.idx.checked_sub(1) } else { Some(t.idx) }));
        }
        if let Some(p) = self.pro_drag {
            return Some((p.lane, if p.kind == ProKind::Roll { Some(p.idx) } else { p.idx.checked_sub(1) }));
        }
        self.loop_trim?;
        let (lane, idx) = self.selected.anchor()?;
        let session = self.session.as_ref()?;
        let clip = session.lane_clips(lane).get(idx)?;
        let now = frame_at(session.now(), self.fps);
        Some((lane, if now >= clip.end() { Some(idx) } else { idx.checked_sub(1) }))
    }

    /// The two-up OUT|IN trim view (DESIGN §6): while an edit point is being
    /// trimmed, rolled, slipped, slid or nudged, the viewer's picture area is
    /// split side by side into the outgoing clip's side and the incoming one's.
    /// It is an `absolute()` fill of the screen, so it takes no room from
    /// anything and the picture's box is exactly as it was; once the gesture
    /// ends it is gone, and `None` is what every other frame draws.
    ///
    /// corner-cut: each half names its clip and edge frame rather than
    /// decoding that frame (a second decode off the neighbour's span).
    pub(crate) fn trim_two_up(&self) -> Option<impl IntoElement> {
        let (lane, cut) = self.editing_cut()?;
        let session = self.session.as_ref()?;
        let clips = session.lane_clips(lane);
        let name_of = |source: usize| {
            session
                .sources()
                .get(source)
                .map_or_else(|| "?".to_string(), |s| file_name(&s.path))
        };
        let out = cut.and_then(|i| clips.get(i)).map_or("Nothing before the cut".to_string(), |c| {
            let end = self.trim.filter(|t| t.edge == Edge::End && Some(t.idx) == cut).map_or(c.end(), |t| t.to);
            format!("{} · ends {}", name_of(c.source), frames_timecode(end, self.fps))
        });
        let inn = cut.map_or(0, |i| i + 1);
        let inn = clips.get(inn).map_or("Nothing after the cut".to_string(), |c| {
            let start = self.trim.filter(|t| t.edge == Edge::Start && t.idx == inn).map_or(c.start, |t| t.to);
            format!("{} · starts {}", name_of(c.source), frames_timecode(start, self.fps))
        });
        let half = |tag: &'static str, text: String| {
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(4.))
                .bg(rgb(DARK_CANVAS()))
                .child(
                    div()
                        .text_size(px(type_scale::FLOOR_PX))
                        .text_color(rgb(INK3()))
                        .child(tag),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(rgb(INK1()))
                        .child(text),
                )
        };
        Some(
            div()
                .id("trim-two-up")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .gap(px(1.))
                .bg(rgba(DARK_SEAM()))
                .child(half("OUT", out))
                .child(half("IN", inn)),
        )
    }
}

/// A cue's lines in one ink: a line for each break the parser kept, each one
/// line high at `size` ([`sub_line_h_for`]). The text and each outline copy
/// are this, so they share every metric and differ only in where and in what.
fn cue_lines(text: &str, size: f32, family: Option<&str>, ink: gpui::Rgba) -> Div {
    div()
        .text_size(px(size))
        .text_color(ink)
        .text_align(TextAlign::Center)
        .when_some(family, |el, fam| el.font_family(fam.to_string()))
        .children(text.split('\n').map(|line| {
            div()
                .min_h(px(sub_line_h_for(size)))
                .child(line.to_string())
        }))
}

/// A title's lines in one ink, each one line of the engine's pitch at `em` px
/// and centred, so a blank line keeps its height the way the rasteriser keeps it.
fn title_lines(
    text: &str,
    em: f32,
    family: Option<&str>,
    ink: gpui::Rgba,
    look: (TextAlign, bool),
    shown: Option<usize>,
    base: &gpui::Font,
) -> Div {
    let line = px(titles::line_px(em));
    let mut font = base.clone();
    if let Some(fam) = family {
        font.family = fam.to_string().into();
    }
    if look.1 {
        font.weight = gpui::FontWeight::BOLD;
    }
    let ink: gpui::Hsla = ink.into();
    // Characters before the line being laid, against `shown` (the reveal's count, `None` for all).
    let mut before = 0usize;
    let mut rows = Vec::new();
    for l in text.split('\n') {
        let n = l.chars().count();
        let child: gpui::AnyElement = match shown {
            Some(s) if s < before + n => {
                // The part of the line the reveal has let through keeps its ink; the rest is
                // in the same place and the same font, drawn in nothing.
                let local = s.saturating_sub(before);
                let cut = l.char_indices().nth(local).map_or(l.len(), |(i, _)| i);
                let hidden = gpui::transparent_black();
                let runs: Vec<gpui::TextRun> = [(cut, ink), (l.len() - cut, hidden)]
                    .into_iter()
                    .filter(|(len, _)| *len > 0)
                    .map(|(len, color)| gpui::TextRun {
                        len,
                        font: font.clone(),
                        color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    })
                    .collect();
                gpui::StyledText::new(l.to_string()).with_runs(runs).into_any_element()
            }
            _ => l.to_string().into_any_element(),
        };
        rows.push(div().min_h(line).child(child));
        before += n + 1;
    }
    div()
        .text_size(px(em))
        .line_height(line)
        .text_color(ink)
        .text_align(look.0)
        .when(look.1, |el| el.font_weight(gpui::FontWeight::BOLD))
        .when_some(family, |el, fam| el.font_family(fam.to_string()))
        .children(rows)
}

/// `~/Pictures/edith`, or `.` with no `HOME` -- [`keymap::config_path_in`]'s
/// own fallback, for the same reason: a screenshot has to land somewhere even
/// on a machine with no desktop environment set up.
pub(crate) fn screenshots_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("Pictures")
        .join("edith")
}

/// Where a screenshot goes: `stem-tc.png` in `dir`, or the same name with a
/// numeric suffix if that one is already taken -- a screenshot never
/// overwrites another the way an export overwrites its own path.
pub(crate) fn screenshot_path(dir: &Path, stem: &str, tc: &str) -> PathBuf {
    let base = format!("{stem}-{tc}");
    let first = dir.join(format!("{base}.png"));
    if !first.exists() {
        return first;
    }
    let mut n = 2;
    loop {
        let path = dir.join(format!("{base}-{n}.png"));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

/// The swap every BGRA frame needs to become the RGBA a PNG writes: gpui's
/// atlas is BGRA with straight alpha ([`Player::sub_picture`]'s own comment),
/// and the PNG encoder writes RGBA.
pub(crate) fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut out = bgra.to_vec();
    for pixel in out.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    out
}

/// Writes `bgra` (`w`x`h`, gpui's own layout) to a fresh PNG under
/// [`screenshots_dir`], creating the directory if this is the first one.
/// Never called on the render thread's own budget for more than a channel
/// swap and one PNG encode -- there is no decode here, only a copy already
/// in memory.
pub(crate) fn save_screenshot(
    bgra: &[u8],
    w: u32,
    h: u32,
    stem: &str,
    tc: &str,
) -> Result<PathBuf, String> {
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = screenshot_path(&dir, stem, tc);
    let rgba = bgra_to_rgba(bgra);
    let png = ec_image::png_encode::encode_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
    std::fs::write(&path, png).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The welcome hero's headline and its second line.
pub(crate) const EMPTY_VIEWER_TITLE: &str = "Drop video, audio or images here";
pub(crate) const EMPTY_VIEWER_LINE: &str = "or start a project from your computer";
/// How many recent projects the hero lists.
pub(crate) const WELCOME_RECENT_MAX: usize = 4;
