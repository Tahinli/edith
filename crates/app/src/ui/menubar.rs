//! The menu bar across the top of the room (DESIGN §6.1): the wordmark, the six
//! titles and their dropdowns on the left, the project's save state on the
//! right. Every row is an [`ActionId`] run through [`Player::act`]; the bar owns
//! no verb of its own, and a refused row is the oracle's answer, not the bar's.

use gpui::deferred;

use crate::files::stem;
use crate::layout::MENU_BAR_H;
use crate::oracle::Enable;
use crate::ui::popup::{popup_plate, popup_row, popup_row_icon, popup_rule, popup_w, shortcut};
use crate::ui::type_scale::{self, Typeset};
use crate::*;

/// One line of a dropdown, in the order it draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Runs the action. A refused one shows its reason in place of the key.
    Act(ActionId),
    /// [`Entry::Act`] with a check mark while the switch is on.
    Switch(ActionId),
    /// The rule line between groups. Destructive verbs sit below one.
    Rule,
    /// A row that opens a flyout of more entries beside the plate (Premiere's menu groups).
    Sub(&'static str, &'static [Entry]),
    /// A setting with several values: a row that opens a flyout of them beside it, the one in
    /// force ticked, and picking one sets it outright ([`Player::menu_pick`]).
    Choice(ActionId),
}

impl Entry {
    pub(crate) fn action(self) -> Option<ActionId> {
        match self {
            Entry::Act(action) | Entry::Switch(action) | Entry::Choice(action) => Some(action),
            Entry::Rule | Entry::Sub(..) => None,
        }
    }
}

/// The six titles, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Title {
    File,
    Edit,
    Clip,
    Timeline,
    Playback,
    View,
    Help,
}

use Entry::{Act, Choice, Rule, Sub, Switch};

/// The File menu's Open recent flyout: a [`Sub`] by name whose rows are the recent projects
/// ([`recent_flyout`]), not a static list, so it is drawn even though it lists no action.
pub(crate) const RECENT_SUB: &str = "Open recent";
/// The File submenu that starts a new project from a template.
pub(crate) const TEMPLATE_SUB: &str = "New from template";

impl Title {
    pub(crate) const ALL: [Title; 7] = [
        Title::File,
        Title::Edit,
        Title::Clip,
        Title::Timeline,
        Title::Playback,
        Title::View,
        Title::Help,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Title::File => "File",
            Title::Edit => "Edit",
            Title::Clip => "Clip",
            Title::Timeline => "Timeline",
            Title::Playback => "Playback",
            Title::View => "View",
            Title::Help => "Help",
        }
    }

    /// The rows, as DESIGN §6.1 lists them. Rows without an [`ActionId`] are
    /// left out rather than given a stub, and [`UNLISTED`] names what is not
    /// on any menu.
    /// Every entry of the menu with the flyouts' rows inlined after where they hang: what the
    /// door and wiring tests read.
    #[cfg(test)]
    pub(crate) fn entries(self) -> Vec<Entry> {
        fn flat(list: &[Entry], out: &mut Vec<Entry>) {
            for e in list {
                match e {
                    Sub(_, inner) => flat(inner, out),
                    e => out.push(*e),
                }
            }
        }
        let mut out = Vec::new();
        flat(self.layout(), &mut out);
        out
    }

    /// The menu as it draws: top-level rows, rules and flyout rows.
    pub(crate) fn layout(self) -> &'static [Entry] {
        match self {
            // New project and Open project (DESIGN §6.1) have no Player function
            // yet, so they have no row.
            Title::File => &[
                Act(ActionId::AddFiles),
                Act(ActionId::PasteFilePath),
                Act(ActionId::ImportSubtitles),
                Rule,
                Act(ActionId::OpenProject),
                Sub(RECENT_SUB, &[]),
                Sub(TEMPLATE_SUB, &[]),
                Sub(
                    "New",
                    &[
                        Act(ActionId::NewProject),
                        Rule,
                        Act(ActionId::NewBin),
                        Act(ActionId::NewSolid),
                        Act(ActionId::NewGradient),
                        Act(ActionId::NewCountdown),
                        Act(ActionId::NewBars),
                        Act(ActionId::NewTone),
                        Act(ActionId::NewBarsAndTone),
                        Act(ActionId::NewAdjustment),
                        Act(ActionId::NewRectangle),
                        Act(ActionId::NewEllipse),
                        Act(ActionId::NewLine),
                        Act(ActionId::NewArrow),
                    ],
                ),
                Rule,
                Act(ActionId::Save),
                Act(ActionId::SaveVersion),
                Act(ActionId::SaveTemplate),
                Act(ActionId::Versions),
                Act(ActionId::ProjectNotes),
                // Reads the file back over the window's edits, so it answers
                // to the same question a changed file does ([`Player::reload_request`]).
                Act(ActionId::ReloadProject),
                Act(ActionId::Export),
                // Shown only while an export runs (the oracle hides it otherwise).
                Act(ActionId::CancelExport),
                Act(ActionId::ExportCaptions),
                Act(ActionId::Screenshot),
                Act(ActionId::CollectMedia),
                Sub(
                    "Export timeline as",
                    &[
                        Act(ActionId::ExportTimelineOtio),
                        Act(ActionId::ExportTimelineFcpxml),
                        Act(ActionId::ExportTimelineEdl),
                    ],
                ),
                Act(ActionId::ImportTimelineOtio),
                Rule,
                Act(ActionId::Settings),
            ],
            Title::Edit => &[
                Act(ActionId::Undo),
                Act(ActionId::Redo),
                Act(ActionId::History),
                Rule,
                Act(ActionId::Copy),
                Act(ActionId::Paste),
                Sub(
                    "Paste attributes",
                    &[
                        Act(ActionId::PasteAttributes),
                        Rule,
                        Act(ActionId::PasteColor),
                        Act(ActionId::PasteEffects),
                        Act(ActionId::PasteTransform),
                        Act(ActionId::PasteSpeed),
                        Act(ActionId::PasteAudio),
                    ],
                ),
                Rule,
                Act(ActionId::InsertSource),
                Act(ActionId::OverwriteSource),
                Act(ActionId::ReplaceEdit),
                Act(ActionId::MatchFrame),
                Rule,
                Sub(
                    "Select",
                    &[
                        Act(ActionId::SelectAll),
                        Act(ActionId::Deselect),
                        Act(ActionId::Select),
                        Act(ActionId::SelectNext),
                        Act(ActionId::SelectPrev),
                        Act(ActionId::SelectForward),
                        Act(ActionId::SelectForwardAll),
                    ],
                ),
            ],
            Title::Clip => &[
                Act(ActionId::Cut),
                Act(ActionId::CutSelectedWords),
                Rule,
                Sub(
                    "Trim",
                    &[
                        Act(ActionId::TrimIn),
                        Act(ActionId::TrimOut),
                        Act(ActionId::RippleTrimInToPlayhead),
                        Act(ActionId::TrimInToPlayhead),
                        Act(ActionId::RippleTrimOutToPlayhead),
                        Act(ActionId::TrimOutToPlayhead),
                        Rule,
                        Act(ActionId::RollBack),
                        Act(ActionId::RollForward),
                        Act(ActionId::SlipBack),
                        Act(ActionId::SlipForward),
                        Act(ActionId::SlideBack),
                        Act(ActionId::SlideForward),
                        Rule,
                        Act(ActionId::LoopTrim),
                        Rule,
                        Act(ActionId::ExtendAudio),
                        Act(ActionId::ExtendVideo),
                    ],
                ),
                Sub(
                    "Transitions",
                    &[
                        Act(ActionId::Crossfade),
                        Act(ActionId::Dissolve),
                        Rule,
                        Act(ActionId::TransitionDipBlack),
                        Act(ActionId::TransitionDipWhite),
                        Act(ActionId::TransitionZoom),
                        Rule,
                        Act(ActionId::TransitionWipeLeft),
                        Act(ActionId::TransitionWipeRight),
                        Act(ActionId::TransitionWipeUp),
                        Act(ActionId::TransitionWipeDown),
                        Rule,
                        Act(ActionId::TransitionSlideLeft),
                        Act(ActionId::TransitionSlideRight),
                        Act(ActionId::TransitionSlideUp),
                        Act(ActionId::TransitionSlideDown),
                        Rule,
                        Act(ActionId::TransitionPushLeft),
                        Act(ActionId::TransitionPushRight),
                        Act(ActionId::TransitionPushUp),
                        Act(ActionId::TransitionPushDown),
                    ],
                ),
                Sub(
                    "Nudge",
                    &[
                        Act(ActionId::NudgeBack),
                        Act(ActionId::NudgeForward),
                        Act(ActionId::NudgeBack10),
                        Act(ActionId::NudgeForward10),
                    ],
                ),
                Sub(
                    "Speed",
                    &[
                        Act(ActionId::Speed),
                        Act(ActionId::Reverse),
                        Act(ActionId::Freeze),
                        Rule,
                        Act(ActionId::SpeedRampUp),
                        Act(ActionId::SpeedRampDown),
                        Act(ActionId::SpeedRampSlowFastSlow),
                        Act(ActionId::SpeedRampFastSlowFast),
                        Act(ActionId::SpeedRampKey),
                        Act(ActionId::SpeedRampClear),
                    ],
                ),
                Sub(
                    "Adjust",
                    &[
                        Act(ActionId::Color),
                        Act(ActionId::AutoColour),
                        Act(ActionId::SaveLook),
                        Act(ActionId::Transform),
                        Act(ActionId::Effects),
                        Act(ActionId::Reframe),
                        Act(ActionId::TrackMask),
                        Choice(ActionId::Fit),
                        Act(ActionId::SubtitleStyle),
                        Rule,
                        Act(ActionId::Equalizer),
                        Act(ActionId::Silence),
                        Act(ActionId::MarkBeats),
                        Act(ActionId::Mix),
                        Act(ActionId::CleanVoice),
                        Act(ActionId::VoiceDynamics),
                        Act(ActionId::Visualizer),
                    ],
                ),
                Sub(
                    "Keyframes",
                    &[
                        Act(ActionId::KeyPlacement),
                        Act(ActionId::KeyOpacity),
                        Act(ActionId::KeyColor),
                        Choice(ActionId::KeyEasing),
                    ],
                ),
                Choice(ActionId::ClipLabel),
                Switch(ActionId::ClipEnabled),
                Rule,
                Sub(
                    "Compound",
                    &[Act(ActionId::NestClips), Act(ActionId::BreakApart), Act(ActionId::OpenCompound)],
                ),
                Sub("Multicam", &[Act(ActionId::CreateMulticam), Act(ActionId::FlattenMulticam), Act(ActionId::Angle1)]),
                Rule,
                Act(ActionId::Group),
                Act(ActionId::Regroup),
                Act(ActionId::Detach),
                Rule,
                Act(ActionId::Delete),
                Act(ActionId::Lift),
            ],
            Title::Timeline => &[
                Sub(
                    "Sequence",
                    &[
                        Act(ActionId::NewSequence),
                        Act(ActionId::DuplicateSequence),
                        Act(ActionId::RenameSequence),
                        Act(ActionId::CloseCompound),
                        Rule,
                        Act(ActionId::DeleteSequence),
                    ],
                ),
                Act(ActionId::CloseGap),
                Act(ActionId::NewTitle),
                Act(ActionId::RecordVoiceover),
                Rule,
                Act(ActionId::SetIn),
                Act(ActionId::SetOut),
                Act(ActionId::ClearRange),
                Rule,
                Sub(
                    "Render preview",
                    &[
                        Act(ActionId::RenderInOut),
                        Act(ActionId::RenderAll),
                        Act(ActionId::CancelRender),
                        Act(ActionId::DeleteRenderFiles),
                    ],
                ),
                Rule,
                Act(ActionId::ZoomIn),
                Act(ActionId::ZoomOut),
                Act(ActionId::ZoomFit),
                Rule,
                Act(ActionId::TrackSmall),
                Act(ActionId::TrackMedium),
                Act(ActionId::TrackLarge),
                Act(ActionId::TracksSmaller),
                Act(ActionId::TracksLarger),
                Rule,
                Switch(ActionId::ToggleSnap),
                Rule,
                Sub(
                    "Go to",
                    &[
                        Act(ActionId::WalkCutPrev),
                        Act(ActionId::WalkCutNext),
                        Act(ActionId::WalkCutPrev10),
                        Act(ActionId::WalkCutNext10),
                        Rule,
                        Act(ActionId::PrevSyncPoint),
                        Act(ActionId::NextSyncPoint),
                    ],
                ),
                Sub(
                    "Markers",
                    &[
                        Act(ActionId::AddMarker),
                        Act(ActionId::PrevMarker),
                        Act(ActionId::NextMarker),
                        Act(ActionId::Markers),
                    ],
                ),
                Sub("Captions", &[Act(ActionId::Captions), Act(ActionId::Fillers)]),
                Sub("Audio", &[Act(ActionId::Normalize), Act(ActionId::Voiceover)]),
                Sub(
                    "Tracks",
                    &[
                        Act(ActionId::AddVideoLane),
                        Act(ActionId::AddAudioLane),
                        Act(ActionId::AddSubtitleLane),
                        Rule,
                        Act(ActionId::RemoveVideoLane),
                        Act(ActionId::RemoveAudioLane),
                        Act(ActionId::RemoveSubtitleLane),
                    ],
                ),
            ],
            Title::Playback => &[
                Act(ActionId::Play),
                Act(ActionId::ShuttleBack),
                Act(ActionId::ShuttleStop),
                Act(ActionId::ShuttleForward),
                Rule,
                Act(ActionId::StepBack),
                Act(ActionId::StepForward),
                Act(ActionId::JumpBack),
                Act(ActionId::JumpForward),
                Rule,
                Switch(ActionId::Loop),
                Act(ActionId::PlayAround),
                Sub(
                    "Go to",
                    &[
                        Act(ActionId::GoStart),
                        Act(ActionId::GoEnd),
                        Rule,
                        Act(ActionId::GoToTime),
                    ],
                ),
            ],
            Title::View => &[
                Switch(ActionId::ToggleSubtitles),
                Switch(ActionId::ToggleAngleMonitor),
                Switch(ActionId::ToggleWaveforms),
                Switch(ActionId::ToggleScrubAudio),
                Sub("Compare grade", &[Switch(ActionId::CompareOff), Switch(ActionId::CompareSplit), Switch(ActionId::CompareSide)]),
                Choice(ActionId::ViewZoom),
                Act(ActionId::ZoomToSelection),
                Choice(ActionId::PlaybackQuality),
                Choice(ActionId::GuideStyle),
                Choice(ActionId::GuideMask),
                Act(ActionId::Fullscreen),
                Switch(ActionId::ToggleOutputWindow),
                Choice(ActionId::OutputDisplay),
                Act(ActionId::FocusPanels),
            ],
            Title::Help => &[
                Act(ActionId::ShowActions),
                Act(ActionId::ShowKeys),
                Rule,
                Act(ActionId::About),
            ],
        }
    }
}

/// Every [`ActionId`] that is on no menu, each with the one-line reason it is
/// not. Empty: every action has a row. A verb that only makes sense for one
/// kind of thing is still on its menu and drops out of the dropdown while the
/// oracle hides it ([`Enable::Hidden`]). A new action that is in neither place
/// fails [`every_action_has_a_door`] rather than going missing.
#[cfg(test)]
pub(crate) const UNLISTED: &[(ActionId, &str)] = &[
    (ActionId::MarkerHere, "the ruler's menu: a marker at the frame the pointer named"),
    (ActionId::RenameMarker, "the ruler's marker menu and a double-click on a ruler marker: the marker that was clicked"),
    (ActionId::DeleteMarker, "the ruler's marker menu: the marker that was clicked"),
    (ActionId::TransitionDissolve, "the same dissolve as Dissolve, at a cut: the Clip tab's own door"),
    (ActionId::PlayheadHere, "the ruler's and the empty-track menu: the playhead to the frame the pointer named"),
    (ActionId::Theme, "the command palette; a look-and-feel choice, not a view verb"),
    (ActionId::AccentColour, "the command palette; a look-and-feel choice, not a view verb"),
    (ActionId::VolumeUp, "the Up-arrow volume key, the viewer's volume control and the command palette"),
    (ActionId::VolumeDown, "the Down-arrow volume key, the viewer's volume control and the command palette"),
    (ActionId::ToggleMute, "the viewer's mute button, the M key and the command palette; it mutes the monitor, not the clip"),
    (ActionId::Resolution, "Settings' Project tab and the command palette"),
    (ActionId::ToggleProxies, "Settings' Project tab and the command palette"),
    (ActionId::ToggleAutoProxies, "Settings' Project tab and the command palette"),
    (ActionId::NewBinInside, "the bin row's right-click menu; it needs the bin that was clicked"),
    (ActionId::RenameBin, "the bin row's right-click menu or a double-click on its name; it needs the bin that was clicked"),
    (ActionId::RemoveBin, "the bin row's right-click menu; it needs the bin that was clicked"),
    (ActionId::ToggleFavourite, "the star on each media row and the row's right-click menu; it needs the media that was clicked"),
    (ActionId::MoveToBin, "the media row's right-click menu, or a drag onto a bin; it needs the media that was clicked"),
    (ActionId::ShowFavourites, "the All / Favourites segmented control in the Media tab"),
    (ActionId::ShowAllMedia, "the All / Favourites segmented control in the Media tab"),
    (ActionId::Angle2, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle3, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle4, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle5, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle6, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle7, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle8, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::Angle9, "the number keys: Clip > Multicam lists them as Switch to angle 1–9; the monitor's tiles are the pointer\'s way"),
    (ActionId::RenameLane, "set from the track head: its right-click menu, or a double-click on the name; it needs the track that was clicked"),
];

/// Whether a switch is on, read off the state it names.
fn checked(player: &Player, action: ActionId) -> bool {
    match action {
        ActionId::ToggleSnap => player.snap,
        ActionId::Loop => player.loop_on,
        ActionId::ToggleAngleMonitor => player.angle_monitor,
        ActionId::ToggleOutputWindow => player.output.is_some(),
        ActionId::ToggleSubtitles => player.subs_on,
        ActionId::ToggleWaveforms => player.waveforms,
        ActionId::ToggleScrubAudio => player.scrub_audio,
        ActionId::CompareOff => player.compare == crate::viewer_view::Compare::Off,
        ActionId::CompareSplit => player.compare == crate::viewer_view::Compare::Split,
        ActionId::CompareSide => player.compare == crate::viewer_view::Compare::Side,
        ActionId::ToggleProxies => player.proxies_on,
        ActionId::ToggleAutoProxies => player.auto_proxies_on,
        ActionId::ToggleMute => player.volume.muted,
        ActionId::ClipEnabled => player
            .label_target()
            .and_then(|(lane, idx)| player.session.as_ref().map(|s| !s.mark_of(lane, idx).off))
            .unwrap_or(true),
        _ => false,
    }
}

/// The bar itself (REDESIGN §3.1): the room's first row, [`MENU_BAR_H`] tall on
/// `e0`, above the dock and the centre alike. Left, the `E` wordmark and the six
/// menus; centre, the command-search field that opens the palette; right, the
/// project's name with its unsaved dot, and Export, the one primary button.
pub(crate) fn bar(player: &Player, view_w: f32, view_h: f32, cx: &mut Context<Player>) -> impl IntoElement {
    use crate::ui::icons::{Icon, IconSize};
    let t = crate::ui::theme::tokens();
    let name = match (player.project_path.as_os_str().is_empty(), &player.new_project) {
        (_, Some(path)) => stem(path),
        // No project yet: the header says nothing rather than a name nobody has given.
        (true, None) => String::new(),
        // Saved or opened as a project: its own name. Not saved yet: "Untitled", never the
        // guessed file beside the media or the autosave the window keeps for it.
        (false, None) if player.autosave_armed => stem(&player.project_path),
        (false, None) => "Untitled".to_string(),
    };
    // An unsaved project shows a dot, a saved one nothing ("Saved" is not
    // displayed); the dot's slot is always there so the name never moves.
    let dot = div()
        .id("menubar-save")
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(16.))
        .when(player.edited_since_save, |d| {
            d.cursor_pointer()
                .tooltip(crate::ui::widgets::action_hover(player, ActionId::Save))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                    this.act(ActionId::Save, window, cx);
                    cx.notify();
                }))
                .child(div().size(px(6.)).rounded(px(3.)).bg(rgb(t.warn)))
        });
    let key = player.keymap.primary(ActionId::ShowActions);
    let wide = view_w >= 1100.;
    let search = div()
        .id("menubar-search")
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(24.))
        .rounded(px(4.))
        .bg(rgb(t.e2))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(t.e3)))
        .tooltip(crate::ui::widgets::action_hover(player, ActionId::ShowActions))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.act(ActionId::ShowActions, window, cx);
        }))
        .children(crate::ui::hitmap::action(ActionId::ShowActions, true))
        .child(Icon::Search.el(IconSize::Inline, t.ink3))
        .when(wide, |d| {
            d.w(px(280.))
                .px(px(8.))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .type_style(type_scale::caption())
                        .text_color(rgb(t.ink3))
                        .child("Search commands"),
                )
                .child(crate::ui::kit::keycap(key))
        })
        .when(!wide, |d| d.w(px(24.)).justify_center());
    let exporting = player.exporting().map(|h| match h.stage() {
        Some((_, f)) => (f.clamp(0., 1.) * 100.) as u32,
        None => (h.progress().clamp(0., 1.) * 100.) as u32,
    });
    let export = crate::ui::kit::Button::new("stance-export")
        .style(crate::ui::kit::ButtonStyle::Primary)
        .label(match exporting {
            Some(pct) => format!("Export {pct}%"),
            None => "Export".to_string(),
        })
        .min_w(100.)
        // Dimmed with the reason in its tooltip while the timeline has nothing to write out; an
        // export already running stays live, since its progress is what this button shows.
        .enabled(exporting.is_some() || player.enable(ActionId::Export, None).yes())
        .build()
        .h(px(24.))
        .tooltip(crate::ui::widgets::action_hover(player, ActionId::Export))
        .children(crate::ui::hitmap::action(ActionId::Export, true))
        // Always the sheet: while an export runs it is that export's progress
        // (with its own two-press cancel), so one stray click can never end an
        // hour of encoding.
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.open_export(cx);
            cx.notify();
        }));
    div()
        .id("menubar")
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .w_full()
        .h(px(MENU_BAR_H))
        .flex()
        .items_center()
        .px(px(8.))
        .bg(rgb(t.e0))
        .child(
            div()
                .flex_none()
                .pl(px(4.))
                .pr(px(12.))
                .type_style(type_scale::title())
                .text_color(rgb(t.ink1))
                .child("E"),
        )
        .children(Title::ALL.map(|title| title_button(player, title, view_h, cx).into_any_element()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .justify_center()
                .px(px(8.))
                .child(search),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id("menubar-project-name")
                        .max_w(px(160.))
                        .truncate()
                        .type_style(type_scale::body_strong())
                        .text_color(rgb(t.ink1))
                        .child(crate::ui::dock_stance::fit_name(&name, 156., type_scale::BODY_PX))
                        .tooltip(crate::ui::widgets::tip_hover(&name, "", None)),
                )
                .child(dot)
                .child(export),
        )
}

/// What a click on `title` leaves open. The pointer reaches a title by
/// hovering it, which already switches the open menu to it; the click that
/// follows must keep that menu (a switch), not toggle it shut.
pub(crate) fn click_menu(open: Option<Title>, hopped: &mut Option<Title>, title: Title) -> Option<Title> {
    let switched = hopped.take() == Some(title) && open == Some(title);
    match open == Some(title) && !switched {
        true => None,
        false => Some(title),
    }
}

/// One title. Clicking it opens its dropdown or closes it; while one is open,
/// hovering another title switches to that one.
fn title_button(player: &Player, title: Title, view_h: f32, cx: &mut Context<Player>) -> impl IntoElement {
    let open = player.menu_open == Some(title);
    // Mounted inside the title, so its anchor is the title's own corner; the
    // plate is deferred so it paints over the room below the bar.
    let plate = open.then(|| deferred(dropdown(player, title, view_h, cx)));
    crate::ui::widgets::focus_ring(div().id(("menubar-title", title as usize)))
        .relative()
        .flex()
        .items_center()
        .h(px(MENU_BAR_H))
        .px(px(10.))
        .rounded(px(4.))
        .cursor_pointer()
        .type_style(type_scale::body_strong())
        .text_color(rgb(if open { INK1() } else { INK2() }))
        .when(open, |d| d.bg(rgb(crate::ui::theme::tokens().e3)))
        .hover(|s| s.bg(rgb(BG_HOVER())).text_color(rgb(INK1())))
        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menu_open = click_menu(this.menu_open, &mut this.menu_hopped, title);
            this.menu_sub = None;
            cx.notify();
        }))
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered && this.menu_open.is_some() && this.menu_open != Some(title) {
                this.menu_open = Some(title);
                this.menu_sub = None;
                this.menu_hopped = Some(title);
                cx.notify();
            }
        }))
        .child(title.label())
        .children(plate)
}

/// The entries of `list` that draw: a row the oracle hides for this context (a class refusal,
/// DESIGN §4.8) is left out, a flyout with nothing left in it goes too, and a rule line stays
/// only between two rows that do, so a group the context empties leaves no stray line.
fn drawn_entries(player: &Player, list: &[Entry]) -> Vec<Entry> {
    let mut drawn: Vec<Entry> = Vec::new();
    for entry in list {
        match entry {
            Sub(name, inner) => {
                if *name == RECENT_SUB || *name == TEMPLATE_SUB || drawn_entries(player, inner).iter().any(|e| e.action().is_some()) {
                    drawn.push(*entry);
                }
            }
            e => match e.action() {
                Some(action) if matches!(player.enable(action, None), Enable::Hidden(_)) => {}
                Some(_) => drawn.push(*e),
                None if drawn.last().is_some_and(|e| e.action().is_some() || matches!(e, Sub(..))) => {
                    drawn.push(*e)
                }
                None => {}
            },
        }
    }
    while drawn.last() == Some(&Rule) {
        drawn.pop();
    }
    drawn
}

/// The height of a plate holding `list`: its padding plus a row each and a rule's 7 px.
pub(crate) fn entries_h(list: &[Entry]) -> f32 {
    crate::layout::MENU_PAD * 2.
        + 2.
        + list.iter().map(|e| if *e == Rule { 7. } else { crate::layout::MENU_ROW_H }).sum::<f32>()
}

/// The rows for `drawn`, with the text the plate's width is fitted to. `level` keeps the ids of a
/// flyout's rows apart from the plate's; only the plate's own plain rows close an open flyout
/// when the pointer comes onto them.
fn menu_rows(
    player: &Player,
    title: Title,
    drawn: &[Entry],
    level: usize,
    cx: &mut Context<Player>,
) -> (Vec<AnyElement>, f32) {
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut measure: Vec<(String, String)> = Vec::new();
    // The check column exists only on a menu that has a switch in it.
    // REDESIGN §3.1: every bar menu keeps the 16px icon column, blank or not.
    let checks = true;
    for (idx, entry) in drawn.iter().enumerate() {
        let id = ("menubar-row", level * 10_000 + title as usize * 100 + idx);
        if let Choice(action) = *entry {
            let name = action.label();
            let shown = name.trim_end_matches('…');
            let refusal = player.enable(action, None);
            let usable = refusal.yes() && player.menu_pick(action).is_some();
            let open = match level {
                0 => player.menu_sub == Some((title, name)),
                _ => player.menu_sub2 == Some((title, name)),
            };
            measure.push((shown.to_string(), "▸".to_string()));
            let row = popup_row_icon(id, shown, "▸", Some(false), crate::menus::icon(action), refusal.why());
            rows.push(match usable {
                true => row
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .when(open, |d| d.bg(rgb(BG_HOVER())))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        if *on {
                            match level {
                                0 => this.menu_sub = Some((title, name)),
                                _ => this.menu_sub2 = Some((title, name)),
                            }
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        match level {
                            0 => this.menu_sub = Some((title, name)),
                            _ => this.menu_sub2 = Some((title, name)),
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
                false => row.into_any_element(),
            });
            continue;
        }
        if let Sub(name, _) = *entry {
            measure.push((name.to_string(), "▸".to_string()));
            rows.push(
                popup_row_icon(id, name, "▸", Some(false), crate::menus::icon_sub(name), None)
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(BG_HOVER())))
                    .when(player.menu_sub == Some((title, name)), |d| d.bg(rgb(BG_HOVER())))
                    .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                        if *on && this.menu_sub != Some((title, name)) {
                            this.menu_sub = Some((title, name));
                            this.menu_sub2 = None;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.menu_sub = Some((title, name));
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            continue;
        }
        let Some(action) = entry.action() else {
            rows.push(popup_rule());
            continue;
        };
        let refusal = player.enable(action, None);
        let label = player.action_name(action);
        // A switch that cannot be flipped shows no tick (nothing to be on), and says why on the row.
        let on = matches!(entry, Switch(_)) && refusal.yes() && checked(player, action);
        let hint = match refusal.why() {
            Some(why) if matches!(entry, Switch(_)) && why.len() <= 24 => {
                let mut c = why.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
            _ => shortcut(&player.keymap, action),
        };
        measure.push((label.clone(), hint.clone()));
        // A switch wears its icon while off and a tick while on, so its state stays visible.
        let icon = crate::menus::icon(action);
        let row = popup_row_icon(id, &label, hint, Some(on), icon, refusal.why());
        // A row whose name says little carries its one-line description on hover.
        let row = match (refusal.yes(), crate::menus::note(action)) {
            (true, Some(note)) => row.tooltip(crate::ui::widgets::overlay_tip_hover(&label, "", Some(note))),
            _ => row,
        };
        let row = row
            .when(level == 0, |d| {
                d.on_hover(cx.listener(|this, on: &bool, _, cx| {
                    if *on && this.menu_sub.is_some() {
                        this.menu_sub = None;
                        this.menu_sub2 = None;
                        cx.notify();
                    }
                }))
            })
            .when(level == 1, |d| {
                d.on_hover(cx.listener(|this, on: &bool, _, cx| {
                    if *on && this.menu_sub2.is_some() {
                        this.menu_sub2 = None;
                        cx.notify();
                    }
                }))
            });
        rows.push(match refusal.yes() {
            true => row
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    // The row sits inside its title, so the title's own
                    // click (which toggles the menu) must not hear it too.
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.menu_sub = None;
                    this.menu_sub2 = None;
                    this.act(action, window, cx);
                    cx.notify();
                }))
                .into_any_element(),
            false => row.into_any_element(),
        });
    }
    let w = popup_w(measure.iter().map(|(l, h)| (l.as_str(), h.as_str())), checks);
    (rows, w)
}

/// The dropdown under a title: a menu plate, one row per entry, and the flyout of the group the
/// pointer is on beside it.
fn dropdown(player: &Player, title: Title, view_h: f32, cx: &mut Context<Player>) -> impl IntoElement {
    let drawn = drawn_entries(player, title.layout());
    let (rows, w) = menu_rows(player, title, &drawn, 0, cx);
    // A menu longer than the window scrolls inside the plate rather than hanging off the bottom
    // edge (the menus are grouped into flyouts so none is: `every_menu_fits_a_720_window`).
    let room = (view_h - MENU_BAR_H - crate::layout::MENU_EDGE - crate::layout::MENU_PAD * 2.).max(crate::layout::MENU_ROW_H);
    // The open flyout: its rows, hung at the right of the plate level with its row.
    let flyout = drawn.iter().enumerate().find_map(|(idx, e)| match *e {
        Sub(name, inner) if player.menu_sub == Some((title, name)) => Some((idx, name, inner)),
        _ => None,
    });
    let flyout = flyout.map(|(idx, name, inner)| {
        if name == RECENT_SUB {
            return recent_flyout(entries_h(&drawn[..idx]) - crate::layout::MENU_PAD - 2., w - 2., cx);
        }
        if name == TEMPLATE_SUB {
            let top = entries_h(&drawn[..idx]) - crate::layout::MENU_PAD - 2.;
            return popup_plate("menubar-flyout", w - 2., top, 300.).children(template_rows(cx, true));
        }
        let inner = drawn_entries(player, inner);
        let (frows, fw) = menu_rows(player, title, &inner, 1, cx);
        let top = entries_h(&drawn[..idx]) - crate::layout::MENU_PAD - 2.;
        // A list-row inside the group opens its own flyout beside this one.
        let second = inner.iter().enumerate().find_map(|(i, e)| match *e {
            Choice(action) if player.menu_sub2 == Some((title, action.label())) => Some((i, action)),
            _ => None,
        });
        let second = second.and_then(|(i, action)| {
            let top2 = entries_h(&inner[..i]) - crate::layout::MENU_PAD - 2.;
            choice_plate(player, "menubar-flyout2", action, fw - 2., top2, cx)
        });
        popup_plate("menubar-flyout", w - 2., top, fw).children(frows).children(second)
    });
    // A list-row on the plate itself opens its flyout where a group's would be.
    let choice_flyout = drawn.iter().enumerate().find_map(|(idx, e)| match *e {
        Choice(action) if player.menu_sub == Some((title, action.label())) => {
            let top = entries_h(&drawn[..idx]) - crate::layout::MENU_PAD - 2.;
            choice_plate(player, "menubar-flyout", action, w - 2., top, cx)
        }
        _ => None,
    });
    popup_plate("menubar-plate", 0., MENU_BAR_H, w)
        .child(
            div()
                .id("menubar-rows")
                .flex()
                .flex_col()
                .max_h(px(room))
                .overflow_y_scroll()
                .children(rows),
        )
        .children(flyout)
        .children(choice_flyout)
}

/// The Open recent flyout at the right of the File plate, level with its row: one two-line row per
/// recent project (name, then its folder), a missing one dimmed with the reason in place of the
/// folder, and Clear list under a rule.
fn recent_flyout(top: f32, x: f32, cx: &mut Context<Player>) -> gpui::Stateful<gpui::Div> {
    use crate::ui::theme::tokens;
    let t = tokens();
    let list = crate::recent::list();
    let mut rows: Vec<AnyElement> = Vec::new();
    for (n, r) in list.iter().enumerate() {
        let path = r.path.clone();
        let missing = r.missing;
        let row = div()
            .id(("menubar-recent", n))
            .flex_none()
            .flex()
            .flex_col()
            .justify_center()
            .h(px(crate::layout::MENU_ROW_H + 12.))
            .px(px(10.))
            .rounded(px(3.))
            .overflow_hidden()
            .child(
                div()
                    .truncate()
                    .type_style(type_scale::body_strong())
                    .text_color(rgb(if missing.is_some() { t.ink4 } else { t.ink1 }))
                    .child(r.name.clone()),
            )
            .child(
                div()
                    .truncate()
                    .type_style(type_scale::caption())
                    .text_color(rgb(if missing.is_some() { t.ink4 } else { t.ink3 }))
                    .child(match missing {
                        Some(why) => format!("{} · {}", why.reason(), crate::recent::tail(&r.folder, 22)),
                        None => crate::recent::tail(&r.folder, 44),
                    }),
            );
        rows.push(match missing {
            None => row
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.menu_sub = None;
                    this.open_recent(&path, cx);
                }))
                .into_any_element(),
            Some(_) => row.into_any_element(),
        });
    }
    if list.is_empty() {
        rows.push(popup_row("menubar-recent-none", "No recent projects", "", None, Some("Projects you open or save appear here")).into_any_element());
        // The list is empty, so the way out is the one the menu offers beside it.
        rows.push(
            popup_row("menubar-recent-open", "Open project…", "", None, None)
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.menu_sub = None;
                    this.pick_open_project(cx);
                }))
                .into_any_element(),
        );
    } else {
        rows.push(popup_rule());
        rows.push(
            popup_row("menubar-recent-clear", "Clear list", "", None, None)
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.menu_sub = None;
                    crate::recent::clear();
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    popup_plate("menubar-flyout", x, top, 340.).children(rows)
}

/// One row per template for File > New from template and the welcome screen's From template list:
/// the built-ins, then the saved ones by name. A click starts a new project from the template. With
/// `close_menu` the File menu it hangs from closes too.
pub(crate) fn template_rows(cx: &mut Context<Player>, close_menu: bool) -> Vec<AnyElement> {
    let dir = engine::templates::dir().unwrap_or_default();
    let mut rows: Vec<AnyElement> = Vec::new();
    for (n, info) in engine::templates::list(&dir).into_iter().enumerate() {
        let name = info.name.clone();
        let hint = if info.clips { "keeps clips" } else { "" };
        rows.push(
            popup_row(("menubar-template-row", n), info.name.clone(), hint, None, None)
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    if close_menu {
                        this.menu_open = None;
                        this.menu_sub = None;
                    }
                    this.open_new_project_from(Some(name.clone()), cx);
                }))
                .into_any_element(),
        );
    }
    rows
}

/// The values of a list-row as a flyout plate at (`x`, `y`): one row each, the one in force
/// ticked, a click sets it and closes the menu. `None` while the setting has nothing to offer.
fn choice_plate(
    player: &Player,
    id: &'static str,
    action: ActionId,
    x: f32,
    y: f32,
    cx: &mut Context<Player>,
) -> Option<gpui::Stateful<gpui::Div>> {
    let choices = player.choices(player.menu_pick(action)?);
    let w = popup_w(choices.iter().map(|(_, l, d, _)| (l.as_ref(), d.as_ref())), true);
    let rows: Vec<AnyElement> = choices
        .into_iter()
        .enumerate()
        .map(|(n, (choice, label, detail, picked))| {
            popup_row(("menubar-choice", n), label, detail, Some(picked), None)
                .cursor_pointer()
                .hover(|s| s.bg(rgb(BG_HOVER())))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu_open = None;
                    this.menu_sub = None;
                    this.menu_sub2 = None;
                    this.choose(choice, cx);
                }))
                .into_any_element()
        })
        .collect();
    Some(popup_plate(id, x, y, w).children(rows))
}

/// The click-away for an open dropdown: a press anywhere below the bar closes
/// it. The bar itself stays live, so a press on another title still switches.
/// Mounted after the room, so it sits over it and under the dropdown plate.
pub(crate) fn backdrop(player: &Player, cx: &mut Context<Player>) -> Option<impl IntoElement> {
    player.menu_open?;
    Some(
        div()
            .id("menubar-away")
            .absolute()
            .top(px(MENU_BAR_H))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.menu_open = None;
                    this.menu_sub = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.menu_open = None;
                    this.menu_sub = None;
                    cx.notify();
                }),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menus::destructive;

    #[test]
    fn clicking_another_title_with_a_menu_open_switches_to_it() {
        let mut hopped = None;
        // File open; the pointer hovers Edit (switch), then clicks it: stays open.
        hopped.replace(Title::Edit);
        assert_eq!(click_menu(Some(Title::Edit), &mut hopped, Title::Edit), Some(Title::Edit));
        // A second click on the open title closes it.
        assert_eq!(click_menu(Some(Title::Edit), &mut hopped, Title::Edit), None);
        // A click on a title when another is open (no hover seen) opens it.
        assert_eq!(click_menu(Some(Title::File), &mut hopped, Title::Edit), Some(Title::Edit));
        // Closed -> opens.
        assert_eq!(click_menu(None, &mut hopped, Title::File), Some(Title::File));
    }

    /// Every action is on exactly one menu, or on [`UNLISTED`] with a reason,
    /// and never both. A new `ActionId` that is in neither place fails here.
    #[test]
    fn every_action_has_a_door() {
        for action in ActionId::ALL {
            let on_menus = Title::ALL
                .iter()
                .flat_map(|t| t.entries())
                .filter(|e| e.action() == Some(action))
                .count();
            let unlisted = UNLISTED.iter().filter(|(a, _)| *a == action).count();
            assert_eq!(
                on_menus + unlisted,
                1,
                "{action:?} is on {on_menus} menu row(s) and {unlisted} allowlist entries; want exactly one door"
            );
        }
    }

    /// File carries the project rows this phase added, Open recent as a flyout.
    #[test]
    fn the_file_menu_has_versions_collect_and_open_recent() {
        let layout = Title::File.layout();
        assert!(layout.iter().any(|e| matches!(e, Sub(name, _) if *name == RECENT_SUB)));
        let all = Title::File.entries();
        for action in [ActionId::SaveVersion, ActionId::Versions, ActionId::CollectMedia, ActionId::ProjectNotes] {
            assert!(all.contains(&Act(action)), "{action:?}");
            assert!(action.label().ends_with('…'), "{action:?} opens a dialog");
        }
    }

    #[test]
    fn unlisted_entries_have_a_reason() {
        for (action, why) in UNLISTED {
            assert!(!why.trim().is_empty(), "{action:?} has no reason");
        }
    }

    /// DESIGN §6.1 and §9: a verb that takes something away sits below a rule
    /// line in its menu.
    #[test]
    fn destructive_rows_sit_below_a_rule() {
        for title in Title::ALL {
            let entries = title.entries();
            for (idx, entry) in entries.iter().enumerate() {
                let Some(action) = entry.action() else {
                    continue;
                };
                if !destructive(action) {
                    continue;
                }
                assert!(
                    entries[..idx].contains(&Rule),
                    "{action:?} in {} has no rule line above it",
                    title.label()
                );
            }
        }
    }

    /// Every menu, with all its rows listed, fits a 1280x720 window without scrolling, and so
    /// does every flyout.
    #[test]
    fn every_menu_fits_a_720_window() {
        let room = 720. - MENU_BAR_H - crate::layout::MENU_EDGE;
        fn lists(list: &'static [Entry], out: &mut Vec<&'static [Entry]>) {
            out.push(list);
            for e in list {
                if let Sub(_, inner) = e {
                    lists(inner, out);
                }
            }
        }
        for title in Title::ALL {
            let mut all = Vec::new();
            lists(title.layout(), &mut all);
            for list in all {
                let h = entries_h(list);
                assert!(h <= room, "{} menu list is {h} px, over the {room} px a 720 window leaves", title.label());
            }
        }
    }
}
