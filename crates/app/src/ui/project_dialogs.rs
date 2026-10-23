//! The File menu's project dialogs, drawn: Save version, Versions, Collect media. One plate
//! ([`kit::plate`]) centred over the picture, one scrim, kit buttons. The state and the keys
//! are `player::project_dialogs`.

use gpui::SharedString;

use crate::player::project_dialogs::{Collect, Dialog};
use crate::ui::kit::{self, ButtonSize, ButtonStyle};
use crate::ui::theme::tokens;
use crate::ui::type_scale::{self, Typeset};
use crate::*;

const W: f32 = 460.;
/// The record sheet's gap above the transport bar, in px.
const RECORD_CLEAR: f32 = 6.;
/// Below this much room over the viewer the record sheet drops its caption and hint.
const RECORD_ROOM: f32 = 380.;
/// Where the record sheet's room starts: under the menu bar, so it can cover the viewer's header too.
const RECORD_TOP: f32 = crate::layout::MENU_BAR_H;
/// Rows of the version list that show before it scrolls.
const ROWS: f32 = 6.;
const ROW_H: f32 = 40.;

/// How many characters the path sheet's field shows before it starts cutting from the front.
const PATH_SHOWN: usize = 56;

/// A face whose title is fixed text, as the shared plate wants it.
/// A source as the sheet names it: its description, else its name.
fn input_label(source: &engine::pulse::Source) -> String {
    if source.description.is_empty() { source.name.clone() } else { source.description.clone() }
}

fn titled((head, body, foot): (&'static str, AnyElement, Vec<AnyElement>)) -> (SharedString, AnyElement, Vec<AnyElement>) {
    (head.into(), body, foot)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl Player {
    pub(crate) fn project_dialog_card(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.proj_dialog.as_ref()?;
        let busy = self.collect_running();
        let area_h = crate::ui::stance::below_picture_floor(f32::from(viewport.height), self.split_px(Split::Bench, viewport));
        // The record sheet's room: from the viewer's top (under the menu bar and the
        // viewer's title row) to just above the transport band, which is measured.
        let band_top = crate::ui::anchor::box_of("record-band").map_or(
            f32::from(viewport.height) - area_h - crate::layout::redesign::TRANSPORT_H,
            |b| f32::from(b.origin.y),
        );
        let over_viewer = band_top - RECORD_TOP - RECORD_CLEAR;
        let (head, body, foot): (SharedString, AnyElement, Vec<AnyElement>) = match dialog {
            Dialog::NewProject(d) => titled(self.new_project_face(d, cx)),
            Dialog::SaveTemplate(d) => titled(self.save_template_face(d, cx)),
            Dialog::SaveVersion(d) => titled(self.save_version_face(d, cx)),
            Dialog::Captions(d) => titled(self.captions_face(d, cx)),
            Dialog::Versions(d) => titled(self.versions_face(d, cx)),
            Dialog::Collect(c) => titled(self.collect_face(c, cx)),
            Dialog::PathEntry(d) => self.path_entry_face(d, cx),
            Dialog::Notes(d) => titled(self.notes_face(d, cx)),
            Dialog::Record(d) => titled(self.record_face(d, over_viewer < RECORD_ROOM, cx)),
        };
        let plate = kit::dialog("project-dialog", W)
            .child(kit::dialog_title(head))
            .child(body)
            .child(kit::dialog_actions().children(foot))
            .when(busy, |d| d);
        if let Dialog::Record(_) = dialog {
            // The record sheet hangs over the viewer, above the transport bar, with no scrim:
            // the timeline under it stays in view while the take runs. Clicks outside it do not
            // close it; Cancel, Stop, its X and Esc do.
            let right = self.split_px(Split::Inspector, viewport);
            // Top and height, not bottom: the window's content is not as tall as the viewport
            // size it is laid out against, so a bottom inset would land below the band.
            let room = (band_top - RECORD_CLEAR - RECORD_TOP).max(0.);
            return Some(
                div()
                    .absolute()
                    .top(px(RECORD_TOP))
                    .h(px(room))
                    .left_0()
                    .right(px(right))
                    .occlude()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_end()
                    .child(plate)
                    .into_any_element(),
            );
        }
        let bottom = (f32::from(viewport.height) - area_h).max(0.);
        Some(
            kit::dialog_layer(bottom)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        // A copy under way is ended by its Cancel, not by a stray click.
                        if !this.collect_running() {
                            this.close_project_dialog(cx);
                        }
                        cx.stop_propagation();
                    }),
                )
                .child(plate)
                .into_any_element(),
        )
    }

    fn caption(text: impl Into<SharedString>) -> Div {
        div().type_style(type_scale::caption()).text_color(rgb(tokens().ink3)).child(text.into())
    }

    fn new_project_face(&self, d: &crate::player::project_dialogs::NewProject, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        use crate::player::project_dialogs::{NEW_RATES, NEW_SIZES};
        use crate::ui::kit::Segment;
        let t = tokens();
        let target = d.target();
        // The folder as the whole path, cut in the middle to fit; the tooltip has all of it.
        let full_dir = d.dir.display().to_string();
        // One reserved line under the fields: a refusal, or where the file will land.
        let note = match (&d.error, &target) {
            (Some(e), _) => (e.clone(), t.err),
            (None, Some(path)) if d.template.is_some() => (format!("{} is created in this folder", crate::files::file_name(path)), t.ink3),
            (None, Some(path)) => {
                let (w, h) = NEW_SIZES[d.size].1;
                (format!("{w}\u{d7}{h} \u{b7} {} is created in this folder when you add footage", crate::files::file_name(path)), t.ink3)
            }
            _ => (String::new(), t.ink3),
        };
        let all = d.field.all && !d.field.text.is_empty();
        let shown: AnyElement = match all {
            true => div().bg(rgba(t.accent_wash)).child(d.field.text.clone()).into_any_element(),
            false => SharedString::from(format!("{}▏", d.field.text)).into_any_element(),
        };
        let row = |label: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_none().w(px(84.)).type_style(type_scale::body()).text_color(rgb(t.ink2)).child(label))
                .child(div().flex_1().min_w(px(0.)).flex().child(control))
        };
        let me = cx.entity();
        let sizes: Vec<Segment> = NEW_SIZES.iter().map(|(label, _)| Segment::text(*label)).collect();
        let rates: Vec<Segment> = NEW_RATES.iter().map(|r| Segment::text(format!("{r:.0}"))).collect();
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(row(
                "Name",
                div()
                    .id("new-project-name")
                    .flex_1()
                    .h(px(ButtonSize::Large.height()))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(shown)
                    .into_any_element(),
            ))
            .child(row(
                "Folder",
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("new-project-folder")
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .type_style(type_scale::body())
                            .text_color(rgb(t.ink1))
                            .tooltip(crate::ui::widgets::overlay_tip_hover(&full_dir, "", None))
                            .child(crate::recent::middle(&full_dir, 28)),
                    )
                    .child(
                        kit::Button::new("new-project-change")
                            .label("Change…")
                            .style(ButtonStyle::Secondary)
                            .build()
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_new_project_folder(cx))),
                    )
                    .into_any_element(),
            ))
            .when(d.template.is_none(), |b| b.child(row(
                "Size",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(kit::segmented("new-project-size", &sizes, d.size, true, None, {
                        let me = me.clone();
                        move |i, _, app| me.update(app, |this, cx| this.new_project_pick(Some(i), None, cx))
                    }))
                    .into_any_element(),
            ))
            .child(row(
                "Frame rate",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(kit::segmented("new-project-rate", &rates, d.rate, true, None, {
                        let me = me.clone();
                        move |i, _, app| me.update(app, |this, cx| this.new_project_pick(None, Some(i), cx))
                    }))
                    .child(Self::caption("frames per second"))
                    .into_any_element(),
            )))
            .when_some(d.template.clone(), |b, name| {
                b.child(row(
                    "Template",
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .type_style(type_scale::body())
                        .text_color(rgb(t.ink1))
                        .child(name)
                        .into_any_element(),
                ))
            })
            .child(div().h(px(16.)).type_style(type_scale::caption()).text_color(rgb(note.1)).child(note.0))
            .into_any_element();
        let foot = vec![
            kit::Button::new("new-project-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("new-project-create")
                .label("Create project")
                .style(ButtonStyle::Primary)
                .enabled(target.is_some())
                .build()
                .tooltip(crate::ui::widgets::overlay_tip_hover("Create project", "", target.is_none().then_some("Type a name first")))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_new_project(cx)))
                .into_any_element(),
        ];
        (if d.template.is_some() { "New from template" } else { "New project" }, body, foot)
    }

    /// Save as template: the name, the Keep clips switch and what it keeps.
    fn save_template_face(&self, d: &crate::player::project_dialogs::SaveTemplate, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        let t = tokens();
        let name = d.field.text.trim();
        let note = match &d.error {
            Some(e) => (e.clone(), t.err),
            None => (String::new(), t.ink3),
        };
        let all = d.field.all && !d.field.text.is_empty();
        let shown: AnyElement = match all {
            true => div().bg(rgba(t.accent_wash)).child(d.field.text.clone()).into_any_element(),
            false => SharedString::from(format!("{}▏", d.field.text)).into_any_element(),
        };
        let me = cx.entity();
        let on = d.keep_clips;
        let keep = kit::switch_row(
            "template-keep-clips",
            "Keep clips",
            Some("Off: the settings only. On: the clips and their media too".into()),
            on,
            None,
            move |_, app| me.update(app, |this, cx| this.set_template_keep_clips(!on, cx)),
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::caption("Keeps the frame rate, size, lanes, lane names, caption style and export preset for new projects."))
            .child(
                div()
                    .id("template-name")
                    .h(px(ButtonSize::Large.height()))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(shown),
            )
            .child(keep)
            .child(
                div()
                    .h(px(16.))
                    .type_style(type_scale::caption())
                    .text_color(rgb(note.1))
                    .child(note.0),
            )
            .into_any_element();
        let foot = vec![
            kit::Button::new("template-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("template-save")
                .label("Save template")
                .style(ButtonStyle::Primary)
                .enabled(!name.is_empty())
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_save_template(cx)))
                .into_any_element(),
        ];
        ("Save as template", body, foot)
    }

    /// File > Project notes: a box of several lines, the caret at its end, scrolling when the
    /// notes run past it. Enter starts a line; Save (or Ctrl+Enter) keeps them.
    fn notes_face(&self, d: &crate::player::project_dialogs::Notes, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        let t = tokens();
        let all = d.field.all && !d.field.text.is_empty();
        let caret = if all { "" } else { "▏" };
        let text = format!("{}{caret}", d.field.text);
        let lines: Vec<AnyElement> = text
            .split('\n')
            .map(|line| div().min_h(px(20.)).when(all, |l| l.bg(rgba(t.accent_wash))).child(line.to_string()).into_any_element())
            .collect();
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::caption("Saved with the project. Enter starts a new line."))
            .child(
                div()
                    .id("project-notes-field")
                    .h(px(180.))
                    .w_full()
                    .p(px(8.))
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .overflow_y_scroll()
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .children(lines),
            )
            .into_any_element();
        let foot = vec![
            kit::Button::new("notes-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("notes-save")
                .label("Save notes")
                .style(ButtonStyle::Primary)
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_project_notes(cx)))
                .into_any_element(),
        ];
        ("Project notes", body, foot)
    }

    /// The Record voice-over sheet: the input, the lane, whether the timeline is heard, a
    /// level meter and the clock while a take runs, and Record or Stop.
    fn record_face(&self, d: &crate::player::project_dialogs::RecordVoiceover, compact: bool, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        use crate::ui::kit::Segment;
        let t = tokens();
        let me = cx.entity();
        let running = d.take.is_some();
        let lanes = self.audio_lanes();
        let mut inputs = vec![Segment::text("Default")];
        inputs.extend(d.inputs.iter().map(|s| Segment::text(input_label(s))));
        let input_pick = d.input.map_or(0, |i| i + 1);
        let mut lane_names: Vec<Segment> = lanes.iter().map(|l| Segment::text(l.label())).collect();
        lane_names.push(Segment::text("New lane"));
        let lane_pick = d.lane.and_then(|l| lanes.iter().position(|x| *x == l)).unwrap_or(lanes.len());
        let (level_db, elapsed) = match &d.take {
            Some(take) => {
                let status = take.recorder.status();
                (status.level_db, status.elapsed_secs)
            }
            None => (crate::player::voice_record::SILENT_DB, 0.0),
        };
        let fill = ((level_db + 60.0) / 60.0).clamp(0.0, 1.0);
        let fill_color = match level_db {
            db if db > -1.0 => t.err,
            db if db > -9.0 => t.warn,
            _ => t.accent,
        };
        let readout = match level_db <= crate::player::voice_record::SILENT_DB {
            true => "silent".to_string(),
            false => format!("{level_db:.0} dBFS"),
        };
        let clock = format!("{}:{:04.1}", (elapsed / 60.0) as u32, elapsed % 60.0);
        let row = |label: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_none().w(px(84.)).type_style(type_scale::body()).text_color(rgb(t.ink2)).child(label))
                .child(div().flex_1().min_w(px(0.)).flex().items_center().child(control))
        };
        let meter = div()
            .relative()
            .h(px(10.))
            .w_full()
            .rounded(px(5.))
            .bg(rgb(t.e2))
            .border_1()
            .border_color(rgb(t.edge))
            .child(div().absolute().top_0().left_0().h_full().w(relative(fill)).rounded(px(5.)).bg(rgb(fill_color)));
        let note = match (&d.error, running) {
            (Some(e), _) => (e.clone(), t.err),
            _ if compact => (String::new(), t.ink3),
            (None, true) => ("Stop places the take on its lane at the start point.".to_string(), t.ink3),
            (None, false) => ("Recording starts at the playhead, and the timeline plays along.".to_string(), t.ink3),
        };
        let on_input = {
            let me = me.clone();
            move |i: usize, _: &mut Window, app: &mut App| me.update(app, |this, cx| this.record_pick_input(i, cx))
        };
        let on_lane = {
            let me = me.clone();
            move |i: usize, _: &mut Window, app: &mut App| me.update(app, |this, cx| this.record_pick_lane(i, cx))
        };
        // Short windows keep the switch in the footer, beside the buttons, to save a row.
        let hear = kit::switch_row(
            "record-hear",
            if compact { "Hear timeline" } else { "Hear the timeline while recording" },
            None,
            d.hear,
            None,
            {
                let me = me.clone();
                move |_: &mut Window, app: &mut App| me.update(app, |this, cx| this.record_toggle_hear(cx))
            },
        );
        // Short windows: the caption and hint go, and the clock joins the level row, so the
        // sheet fits over the viewer at 720 lines.
        let level = div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(div().flex_1().min_w(px(0.)).child(meter))
            .child(div().flex_none().w(px(72.)).type_style(type_scale::body()).text_color(rgb(t.ink2)).child(readout));
        let mut body = div().flex().flex_col().gap(px(if compact { 6. } else { 10. }));
        if !compact {
            body = body.child(Self::caption("A voice-over from the microphone, placed on an audio lane at the playhead."));
        }
        body = body
            .child(row("Input", kit::segmented_wrap("record-input", &inputs, input_pick, !running, None, on_input).into_any_element()))
            .child(row("Lane", kit::segmented_wrap("record-lane", &lane_names, lane_pick, !running, None, on_lane).into_any_element()));
        let hear_in_foot = if compact {
            Some(hear)
        } else {
            body = body.child(hear);
            None
        };
        body = match compact {
            true => body.child(row("Level", level.child(div().flex_none().w(px(44.)).type_style(type_scale::body()).text_color(rgb(t.ink1)).child(clock)).into_any_element())),
            false => body
                .child(row("Level", level.into_any_element()))
                .child(row("Time", div().type_style(type_scale::body()).text_color(rgb(t.ink1)).child(clock).into_any_element()))
                .child(div().h(px(16.)).type_style(type_scale::caption()).text_color(rgb(note.1)).child(note.0.clone())),
        };
        if compact && !note.0.is_empty() {
            body = body.child(div().type_style(type_scale::caption()).text_color(rgb(note.1)).child(note.0));
        }
        let body = body.into_any_element();
        let mut foot = match running {
            true => vec![
                kit::Button::new("record-stop")
                    .label("Stop")
                    .style(ButtonStyle::Primary)
                    .build()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.record_stop(cx)))
                    .into_any_element(),
            ],
            false => {
                let reason = if self.project_path.as_os_str().is_empty() { Some("Save the project first") } else { None };
                vec![
                    kit::Button::new("record-cancel")
                        .label("Cancel")
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                        .into_any_element(),
                    kit::Button::new("record-start")
                        .label("Record")
                        .style(ButtonStyle::Primary)
                        .enabled(reason.is_none())
                        .build()
                        .tooltip(crate::ui::widgets::overlay_tip_hover("Record", "", reason))
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.record_start(cx)))
                        .into_any_element(),
                ]
            }
        };
        if let Some(hear) = hear_in_foot {
            foot.insert(0, div().flex_none().w(px(170.)).child(hear).into_any_element());
        }
        ("Record voice-over", body, foot)
    }

    fn captions_face(&self, d: &crate::player::project_dialogs::CaptionsExport, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        use crate::player::project_dialogs::CaptionFormat;
        use crate::ui::kit::Segment;
        let t = tokens();
        let typed = d.field.text.trim().to_string();
        let name_ok = !typed.is_empty() && !typed.contains(['/', '\\']);
        let file = format!("{typed}.{}", d.format.ext());
        let folder = d.dir.display().to_string();
        let has_cues = self.session.as_ref().is_some_and(|s| !s.caption_cues().is_empty());
        // One reserved line under the fields: a refusal, where the file goes, or that it replaces one.
        let note = match (&d.error, name_ok) {
            (Some(e), _) => (e.clone(), t.err),
            (None, false) => ("Type a name for the file".to_string(), t.ink3),
            (None, true) if d.dir.join(&file).exists() => (format!("Replaces the file already in {}", crate::recent::middle(&folder, 28)), t.warn),
            (None, true) => (format!("Writes {file} in {}", crate::recent::middle(&folder, 28)), t.ink3),
        };
        let all = d.field.all && !d.field.text.is_empty();
        let shown: AnyElement = match all {
            true => div().bg(rgba(t.accent_wash)).child(d.field.text.clone()).into_any_element(),
            false => SharedString::from(format!("{}▏", d.field.text)).into_any_element(),
        };
        let formats: Vec<Segment> = CaptionFormat::ALL
            .iter()
            .map(|f| Segment::text(match f {
                CaptionFormat::Srt => "SRT",
                CaptionFormat::Vtt => "WebVTT",
            }))
            .collect();
        let selected = CaptionFormat::ALL.iter().position(|f| *f == d.format).unwrap_or(0);
        let me = cx.entity();
        let row = |label: &'static str, control: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_none().w(px(84.)).type_style(type_scale::body()).text_color(rgb(t.ink2)).child(label))
                .child(div().flex_1().min_w(px(0.)).flex().child(control))
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(Self::caption("The timeline's captions as a text file that players and video sites read."))
            .child(row(
                "Name",
                div()
                    .id("captions-name")
                    .flex_1()
                    .h(px(ButtonSize::Large.height()))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(shown)
                    .into_any_element(),
            ))
            .child(row("Format", kit::segmented("captions-format", &formats, selected, true, None, {
                let me = me.clone();
                move |i, _, app| me.update(app, |this, cx| this.captions_pick(i, cx))
            })
            .into_any_element()))
            .child(div().h(px(16.)).type_style(type_scale::caption()).text_color(rgb(note.1)).child(note.0))
            .into_any_element();
        let foot = vec![
            kit::Button::new("captions-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("captions-save")
                .label("Save captions")
                .style(ButtonStyle::Primary)
                .enabled(name_ok && has_cues)
                .build()
                .tooltip(crate::ui::widgets::overlay_tip_hover("Save captions", "", (!has_cues).then_some("No captions on the timeline yet")))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_export_captions(cx)))
                .into_any_element(),
        ];
        ("Export captions", body, foot)
    }

    fn save_version_face(&self, d: &crate::player::project_dialogs::SaveVersion, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        let t = tokens();
        let cleaned = engine::versions::clean_name(&d.field.text);
        // One reserved line under the field: an error, the name it will get, or what it replaces.
        let note = match (&d.error, &cleaned) {
            (Some(e), _) => Some((e.clone(), t.err)),
            (None, Some(name)) if *name != d.field.text => Some((format!("Saved as {name}: only letters, digits, - and _"), t.ink3)),
            (None, Some(name)) => std::fs::metadata(engine::versions::file(&d.dir, name))
                .and_then(|m| m.modified())
                .ok()
                .map(|at| (format!("Replaces the version saved {}", engine::versions::age_text(at)), t.warn)),
            _ => None,
        };
        let all = d.field.all && !d.field.text.is_empty();
        let shown: AnyElement = match all {
            true => div().bg(rgba(t.accent_wash)).child(d.field.text.clone()).into_any_element(),
            false => SharedString::from(format!("{}▏", d.field.text)).into_any_element(),
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::caption("Keeps a full copy of the project as it is now. Restore it later from File, Versions."))
            .child(
                div()
                    .id("version-name")
                    .h(px(ButtonSize::Large.height()))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(shown),
            )
            .child(
                div()
                    .h(px(16.))
                    .type_style(type_scale::caption())
                    .text_color(rgb(note.as_ref().map_or(t.ink3, |n| n.1)))
                    .child(note.map(|n| n.0).unwrap_or_default()),
            )
            .into_any_element();
        let foot = vec![
            kit::Button::new("version-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("version-save")
                .label("Save version")
                .style(ButtonStyle::Primary)
                .enabled(cleaned.is_some())
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_save_version(cx)))
                .into_any_element(),
        ];
        ("Save version", body, foot)
    }

    fn path_entry_face(&self, d: &crate::player::project_dialogs::PathEntry, cx: &mut Context<Self>) -> (SharedString, AnyElement, Vec<AnyElement>) {
        use crate::player::project_dialogs::PathFor;
        let t = tokens();
        let (title, action): (SharedString, &str) = match (d.what, &d.from) {
            (PathFor::Import, _) => ("Import media".into(), "Import"),
            (PathFor::OpenProject, _) => ("Open project".into(), "Open"),
            (PathFor::Subtitles, _) => ("Import subtitles".into(), "Import"),
            (PathFor::Lut { .. }, _) => ("Choose a look file".into(), "Choose"),
            (PathFor::Relink, from) => {
                let name = from.as_deref().map_or("file".to_string(), crate::files::file_name);
                (format!("Relink {name}").into(), "Relink")
            }
        };
        let typed = !d.field.text.trim().is_empty();
        // A long path shows its end, where the caret is, behind a mark that says the start is cut.
        let chars = d.field.text.chars().count();
        let seen = match chars > PATH_SHOWN {
            true => format!("…{}", d.field.text.chars().skip(chars - (PATH_SHOWN - 1)).collect::<String>()),
            false => d.field.text.clone(),
        };
        let shown: AnyElement = match d.field.all && typed {
            true => div().bg(rgba(t.accent_wash)).child(seen).into_any_element(),
            false => SharedString::from(format!("{seen}▏")).into_any_element(),
        };
        let body = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(Self::caption("Type or paste the file's path."))
            .child(
                div()
                    .id("path-entry")
                    .h(px(ButtonSize::Large.height()))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .bg(rgb(t.e2))
                    .border_1()
                    .border_color(rgb(t.accent))
                    .cursor(CursorStyle::IBeam)
                    .type_style(type_scale::body())
                    .text_color(rgb(t.ink1))
                    .child(shown),
            )
            .child(
                div()
                    .h(px(16.))
                    .type_style(type_scale::caption())
                    .text_color(rgb(if d.error.is_some() { t.err } else { t.ink3 }))
                    .child(d.error.clone().unwrap_or_default()),
            )
            .into_any_element();
        let foot = vec![
            kit::Button::new("path-cancel")
                .label("Cancel")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
            kit::Button::new("path-go")
                .label(action)
                .style(ButtonStyle::Primary)
                .enabled(typed)
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.commit_path_entry(cx)))
                .into_any_element(),
        ];
        (title, body, foot)
    }

    fn versions_face(&self, d: &crate::player::project_dialogs::VersionList, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        let t = tokens();
        let body: AnyElement = if d.rows.is_empty() {
            Self::caption("Use File \u{25b8} Save version to keep one.").into_any_element()
        } else {
            let rows = d.rows.iter().enumerate().map(|(n, v)| {
                let on = n == d.picked;
                div()
                    .id(("version-row", n))
                    .flex_none()
                    .h(px(ROW_H))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .rounded(px(crate::layout::redesign::R_CONTROL))
                    .cursor_pointer()
                    .when(on, |r| r.bg(rgba(t.accent_wash)))
                    .when(!on, |r| r.hover(|s| s.bg(rgb(t.e3))))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_version(n, cx)))
                    .child(div().min_w(px(0.)).truncate().type_style(type_scale::body_strong()).text_color(rgb(t.ink1)).child(v.name.clone()))
                    .child(
                        div()
                            .flex_none()
                            .type_style(type_scale::caption())
                            .text_color(rgb(t.ink3))
                            .child(format!("{} · {} clip{}", engine::versions::age_text(v.saved), v.clips, plural(v.clips))),
                    )
            });
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .id("version-rows")
                        .flex()
                        .flex_col()
                        .max_h(px(ROW_H * ROWS))
                        .overflow_y_scroll()
                        .children(rows),
                )
                .child(Self::caption(format!(
                    "Restoring is one undo step: {} brings back what you have now.",
                    self.keymap.display(ActionId::Undo)
                )))
                .into_any_element()
        };
        let mut foot = vec![
            kit::Button::new("versions-close")
                .label("Close")
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element(),
        ];
        if !d.rows.is_empty() {
            foot.push(
                kit::Button::new("versions-restore")
                    .label(if d.busy { "Restoring…" } else { "Restore" })
                    .style(ButtonStyle::Primary)
                    .enabled(!d.busy)
                    .min_w(96.)
                    .build()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.restore_picked_version(cx)))
                    .into_any_element(),
            );
        }
        ("Versions", body, foot)
    }

    fn collect_face(&self, c: &Collect, cx: &mut Context<Self>) -> (&'static str, AnyElement, Vec<AnyElement>) {
        let t = tokens();
        let close = |id: &'static str, label: &'static str, primary: bool, cx: &mut Context<Self>| {
            kit::Button::new(id)
                .label(label)
                .style(if primary { ButtonStyle::Primary } else { ButtonStyle::Secondary })
                .build()
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.close_project_dialog(cx)))
                .into_any_element()
        };
        let line = |text: String| div().type_style(type_scale::body()).text_color(rgb(t.ink2)).child(text).into_any_element();
        match c {
            Collect::Ask(plan) => {
                let files = plan.items.len();
                let mut lines = vec![if files == 0 {
                    line("Nothing to copy: every file the project uses is already in this folder, or is generated.".into())
                } else {
                    line(format!(
                        "Copies {files} file{} ({}) into one folder and plays the project from the copies.",
                        plural(files),
                        engine::collect::size_text(plan.bytes())
                    ))
                }];
                if plan.generated > 0 {
                    lines.push(Self::caption(format!("{} generated, nothing to copy", plan.generated)).into_any_element());
                }
                if !plan.offline.is_empty() {
                    let n = plan.offline.len();
                    lines.push(
                        Self::caption(format!("{n} file{} {} missing and will be skipped", plural(n), if n == 1 { "is" } else { "are" }))
                            .text_color(rgb(t.warn))
                            .into_any_element(),
                    );
                }
                if plan.already > 0 {
                    lines.push(Self::caption(format!("{} already in this folder", plan.already)).into_any_element());
                }
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(lines)
                    .child(
                        // The whole path: wrapped at its slashes would need a break rule gpui
                        // lacks, so it shows with the middle dropped and the full text on hover.
                        div()
                            .id("collect-folder-path")
                            .min_h(px(ButtonSize::Large.height()))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(crate::layout::redesign::R_CONTROL))
                            .bg(rgb(t.e2))
                            .border_1()
                            .border_color(rgba(t.line))
                            .type_style(type_scale::body())
                            .text_color(rgb(t.ink1))
                            .tooltip(crate::ui::widgets::overlay_tip_hover("Destination folder", "", Some(&plan.folder.display().to_string())))
                            .child(div().min_w(px(0.)).truncate().child(crate::recent::short_path(&plan.folder, 52))),
                    )
                    .into_any_element();
                let foot = vec![
                    kit::Button::new("collect-folder")
                        .label("Choose folder…")
                        .style(ButtonStyle::Secondary)
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.choose_collect_folder(cx)))
                        .into_any_element(),
                    div().flex_1().into_any_element(),
                    close("collect-cancel", "Cancel", false, cx),
                    kit::Button::new("collect-go")
                        .label("Collect")
                        .style(ButtonStyle::Primary)
                        .enabled(files > 0)
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start_collect(cx)))
                        .into_any_element(),
                ];
                ("Collect media", body, foot)
            }
            Collect::Run(run) => {
                let done = run.done.load(std::sync::atomic::Ordering::Relaxed);
                let cancelling = run.cancel.load(std::sync::atomic::Ordering::Relaxed);
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div().h(px(6.)).w_full().rounded(px(3.)).bg(rgb(t.e2)).child(
                            div().h_full().rounded(px(3.)).bg(rgb(t.accent)).w(relative(run.fraction())),
                        ),
                    )
                    .child(Self::caption(format!(
                        "{} of {}{}",
                        engine::collect::size_text(done),
                        engine::collect::size_text(run.total),
                        if cancelling { " · cancelling…" } else { "" }
                    )))
                    .into_any_element();
                let foot = vec![
                    kit::Button::new("collect-stop")
                        .label("Cancel")
                        .enabled(!cancelling)
                        .build()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_collect(cx)))
                        .into_any_element(),
                ];
                ("Collecting media", body, foot)
            }
            Collect::Done(lines) => {
                let body = div().flex().flex_col().gap(px(6.)).children(lines.iter().cloned().map(line)).into_any_element();
                ("Media collected", body, vec![close("collect-done", "Done", true, cx)])
            }
            Collect::Failed(why) => {
                let body = div().type_style(type_scale::body()).text_color(rgb(t.err)).child(format!("{why}. Nothing was changed.")).into_any_element();
                ("Collect failed", body, vec![close("collect-failed", "Close", true, cx)])
            }
        }
    }
}
