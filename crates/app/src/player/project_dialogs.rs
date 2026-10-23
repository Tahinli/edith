//! The File menu's project dialogs: Save version, Versions and Collect media, plus Open recent.
//! The work is the engine's ([`engine::versions`], [`engine::collect`]); this is the state a dialog
//! holds, the keys it takes and the hand-over to the session. Drawn by `ui::project_dialogs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use engine::collect::{self, Plan, Stop};
use engine::versions::{self, Version};

use crate::player::text_field::{FieldKey, TextField};
use crate::*;

/// The longest name a version may have ([`versions::valid_name`]).
const NAME_MAX: usize = 40;

/// Which dialog is up, with what it shows.
pub(crate) enum Dialog {
    NewProject(NewProject),
    SaveTemplate(SaveTemplate),
    SaveVersion(SaveVersion),
    Captions(CaptionsExport),
    Versions(VersionList),
    Collect(Collect),
    PathEntry(PathEntry),
    Notes(Notes),
    Record(RecordVoiceover),
}

/// Record voice-over: the input and lane picked, and the take while it runs.
pub(crate) struct RecordVoiceover {
    /// The inputs the audio server lists, monitors included. Empty when none answers.
    pub(crate) inputs: Vec<engine::pulse::Source>,
    /// The input picked, an index into `inputs`; `None` is the server's default input.
    pub(crate) input: Option<usize>,
    /// The audio lane the take lands on; `None` is a new audio lane.
    pub(crate) lane: Option<engine::project::Lane>,
    /// Whether the timeline is heard while recording. Off, the output is muted so the mic
    /// does not pick up the speakers.
    pub(crate) hear: bool,
    /// The take in progress, from Record until Stop.
    pub(crate) take: Option<super::voice_record::InProgress>,
    pub(crate) error: Option<String>,
}

/// The longest note the project notes take.
pub(crate) const NOTES_MAX: usize = 20_000;

/// The project notes being edited: the text, with the caret at its end.
pub(crate) struct Notes {
    pub(crate) field: TextField,
}

/// What a typed path is for: the Import button, Open project, Import subtitles, a look file (a
/// new one, or the one a clip's look already holds) or Relink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathFor {
    Import,
    OpenProject,
    Subtitles,
    Lut { relink: bool },
    Relink,
}

/// The sheet a file picker's fallback opens: the path typed or pasted, and what it is for.
pub(crate) struct PathEntry {
    pub(crate) field: TextField,
    pub(crate) what: PathFor,
    /// The missing file a Relink is for; `None` for every other path.
    pub(crate) from: Option<PathBuf>,
    /// A refusal from the last try, shown under the field.
    pub(crate) error: Option<String>,
}

/// The sizes File > New > Project… offers, as direct choices: the label and the canvas.
pub(crate) const NEW_SIZES: [(&str, (u32, u32)); 4] =
    [("1080p", (1920, 1080)), ("4K", (3840, 2160)), ("Vertical", (1080, 1920)), ("Square", (1080, 1080))];
/// ...and the frame rates.
pub(crate) const NEW_RATES: [f64; 4] = [24., 25., 30., 60.];

/// The [`NEW_RATES`] entry a new project starts on: the one nearest the footage's
/// own rate when there is footage and it is within a frame per second of one
/// (25 fps footage starts on 25, 23.976 on 24), else 30, the rate most people
/// shoot and deliver at.
pub(crate) fn default_rate_index(footage: Option<f64>) -> usize {
    const DEFAULT: usize = 2;
    let Some(fps) = footage.filter(|f| f.is_finite() && *f > 0.) else {
        return DEFAULT;
    };
    NEW_RATES
        .iter()
        .enumerate()
        .map(|(i, &rate)| (i, (rate - fps).abs()))
        .filter(|&(_, gap)| gap <= 1.)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map_or(DEFAULT, |(i, _)| i)
}
/// The longest project name the field takes.
const PROJECT_NAME_MAX: usize = 60;
/// The longest path the path sheet takes.
const PATH_MAX: usize = 4096;
/// The longest time the Go to time field takes.
const TIME_MAX: usize = 24;

pub(crate) struct NewProject {
    pub(crate) field: TextField,
    pub(crate) dir: PathBuf,
    /// Index into [`NEW_SIZES`] and [`NEW_RATES`].
    pub(crate) size: usize,
    pub(crate) rate: usize,
    /// The template the project starts from, when it is File > New from template: its size, rate,
    /// lanes and caption style come with it, so the size and rate choices are not offered.
    pub(crate) template: Option<String>,
    /// A refusal from the last try, shown under the fields.
    pub(crate) error: Option<String>,
}

impl NewProject {
    /// Where the project file will be written, or `None` while the name is empty.
    pub(crate) fn target(&self) -> Option<PathBuf> {
        project_file_name(&self.field.text).map(|n| self.dir.join(n))
    }
}

/// The file name a typed project name makes: trimmed, path separators turned into `-`, and
/// `.edith` added unless the person typed it. `None` for a name with nothing in it.
pub(crate) fn project_file_name(typed: &str) -> Option<String> {
    let name: String = typed.trim().chars().map(|c| if matches!(c, '/' | '\\' | '\0') || c.is_control() { '-' } else { c }).collect();
    let name = name.strip_suffix(".edith").unwrap_or(&name).trim().trim_matches('.').to_string();
    (!name.is_empty()).then(|| format!("{name}.edith"))
}

/// The folder a new project is offered in: the one the newest recent project sits in, else
/// `~/Videos`, else the home folder.
pub(crate) fn default_project_dir() -> PathBuf {
    if let Some(dir) = crate::recent::list().iter().find_map(|r| r.path.parent().filter(|d| d.is_dir()).map(Path::to_path_buf)) {
        return dir;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let videos = home.join("Videos");
    if videos.is_dir() { videos } else { home }
}

/// Save as template: the name, and whether the clips are kept with the settings.
pub(crate) struct SaveTemplate {
    pub(crate) field: TextField,
    pub(crate) keep_clips: bool,
    /// A refusal from the last try, shown under the field.
    pub(crate) error: Option<String>,
}

pub(crate) struct SaveVersion {
    pub(crate) field: TextField,
    pub(crate) dir: PathBuf,
    /// A refusal from the last try, shown under the field.
    pub(crate) error: Option<String>,
}

/// The caption file a File > Export captions writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptionFormat {
    Srt,
    Vtt,
}

impl CaptionFormat {
    pub(crate) const ALL: [CaptionFormat; 2] = [CaptionFormat::Srt, CaptionFormat::Vtt];

    pub(crate) fn ext(self) -> &'static str {
        match self {
            CaptionFormat::Srt => "srt",
            CaptionFormat::Vtt => "vtt",
        }
    }
}

/// The sheet File > Export captions opens: the name of the file (its folder is the default
/// export folder), and whether it is SRT or WebVTT.
pub(crate) struct CaptionsExport {
    pub(crate) field: TextField,
    pub(crate) format: CaptionFormat,
    pub(crate) dir: PathBuf,
    /// A refusal from the last try, shown under the field.
    pub(crate) error: Option<String>,
}

pub(crate) struct VersionList {
    pub(crate) rows: Vec<Version>,
    pub(crate) picked: usize,
    pub(crate) dir: PathBuf,
    /// A restore is being read in.
    pub(crate) busy: bool,
}

pub(crate) enum Collect {
    /// What will be copied, asked before anything is.
    Ask(Plan),
    /// Copying on a worker thread.
    Run(Run),
    /// What happened, one line each.
    Done(Vec<String>),
    Failed(String),
}

pub(crate) struct Run {
    pub(crate) plan: Plan,
    pub(crate) total: u64,
    pub(crate) done: Arc<AtomicU64>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) result: Arc<Mutex<Option<Result<collect::Done, Stop>>>>,
}

impl Run {
    pub(crate) fn fraction(&self) -> f32 {
        match self.total {
            0 => 0.,
            t => (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.) as f32,
        }
    }
}

impl Player {
    /// Whether a dialog is up that has not been dealt with by `Esc`.
    pub(crate) fn collect_running(&self) -> bool {
        matches!(&self.proj_dialog, Some(Dialog::Collect(Collect::Run(_))))
    }

    /// Whether a modal dialog (a question, the project dialogs, About, Settings) is up: it owns
    /// the keyboard, and Tab walks its controls.
    pub(crate) fn dialog_up(&self) -> bool {
        self.quit_ask || self.save_ask || self.export_ask || self.proj_dialog.is_some() || self.about_open || self.settings_open
    }

    /// The dialog shell's X: cancels the topmost dialog, the way Esc does.
    pub(crate) fn dialog_cancel(&mut self, cx: &mut Context<Self>) {
        if self.quit_ask {
            self.quit_ask = false;
        } else if self.save_ask {
            self.save_ask = false;
        } else if self.export_ask {
            self.answer_overwrite(Overwrite::Cancel, cx);
        } else if self.proj_dialog.is_some() {
            if self.collect_running() {
                self.cancel_collect(cx);
            } else {
                self.close_project_dialog(cx);
            }
        } else if self.about_open {
            self.about_open = false;
        } else if self.settings_open {
            self.settings_open = false;
            self.keys_filter.clear();
        } else if self.keys_open {
            self.keys_open = false;
            self.keys_filter.clear();
        }
        cx.notify();
    }

    /// Dialog keyboard navigation, ahead of every dialog's own keys: Tab and Shift+Tab move the
    /// focus ring through the dialog's controls; Enter and Space on a focused control are left to
    /// its own (key-up) click, so they do not also fire the dialog's primary answer.
    pub(crate) fn dialog_nav_key(&mut self, key: &str, shift: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match key {
            "tab" => {
                crate::ui::kit::set_kbd_ring(true);
                if shift {
                    window.focus_prev();
                } else {
                    window.focus_next();
                }
                cx.notify();
                true
            }
            "enter" | "space" => self.dlg_pool.iter().any(|h| h.is_focused(window)),
            _ => false,
        }
    }

    pub(crate) fn close_project_dialog(&mut self, cx: &mut Context<Self>) {
        // A take is never dropped by a click away: closing the sheet places it, as Stop does.
        if self.record_running() {
            self.record_stop(cx);
            return;
        }
        if let Some(Dialog::Collect(Collect::Run(run))) = &self.proj_dialog {
            run.cancel.store(true, Ordering::Relaxed);
        }
        self.proj_dialog = None;
        cx.notify();
    }

    /// The keys a project dialog takes; every other key is the dialog's to ignore.
    pub(crate) fn project_dialog_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        if matches!((ctrl && !alt, key), (true, "v")) && matches!(self.proj_dialog, Some(Dialog::PathEntry(_))) {
            return self.path_entry_paste(cx);
        }
        match &mut self.proj_dialog {
            None => {}
            Some(Dialog::NewProject(d)) => match d.field.key(key, ctrl, alt, key_char, PROJECT_NAME_MAX) {
                FieldKey::Cancel => self.close_project_dialog(cx),
                FieldKey::Commit => self.commit_new_project(cx),
                FieldKey::Edited => {
                    d.error = None;
                    cx.notify();
                }
            },
            Some(Dialog::SaveTemplate(d)) => match d.field.key(key, ctrl, alt, key_char, NAME_MAX) {
                FieldKey::Cancel => self.close_project_dialog(cx),
                FieldKey::Commit => self.commit_save_template(cx),
                FieldKey::Edited => {
                    d.error = None;
                    cx.notify();
                }
            },
            Some(Dialog::SaveVersion(d)) => match d.field.key(key, ctrl, alt, key_char, NAME_MAX) {
                FieldKey::Cancel => self.close_project_dialog(cx),
                FieldKey::Commit => self.commit_save_version(cx),
                FieldKey::Edited => {
                    d.error = None;
                    cx.notify();
                }
            },
            Some(Dialog::PathEntry(d)) => match d.field.key(key, ctrl, alt, key_char, PATH_MAX) {
                FieldKey::Cancel => self.close_project_dialog(cx),
                FieldKey::Commit => self.commit_path_entry(cx),
                FieldKey::Edited => {
                    d.error = None;
                    cx.notify();
                }
            },
            // Several lines: Enter starts a new one, Ctrl+Enter saves, Escape drops the edit.
            Some(Dialog::Notes(d)) => match (key, ctrl && !alt) {
                ("escape", _) => self.close_project_dialog(cx),
                ("enter", true) => self.commit_project_notes(cx),
                ("enter", false) => {
                    if d.field.all {
                        d.field.text.clear();
                        d.field.all = false;
                    }
                    if d.field.text.chars().count() < NOTES_MAX {
                        d.field.text.push('\n');
                    }
                    cx.notify();
                }
                _ => match d.field.key(key, ctrl, alt, key_char, NOTES_MAX) {
                    FieldKey::Cancel => self.close_project_dialog(cx),
                    FieldKey::Commit => self.commit_project_notes(cx),
                    FieldKey::Edited => cx.notify(),
                },
            },
            // Escape closes the sheet; a take still running is placed, as Stop places it.
            Some(Dialog::Record(_)) => {
                if key == ESCAPE {
                    self.close_project_dialog(cx);
                }
            }
            Some(Dialog::Captions(d)) => match d.field.key(key, ctrl, alt, key_char, NAME_MAX) {
                FieldKey::Cancel => self.close_project_dialog(cx),
                FieldKey::Commit => self.commit_export_captions(cx),
                FieldKey::Edited => {
                    d.error = None;
                    cx.notify();
                }
            },
            Some(Dialog::Versions(d)) => match key {
                "escape" => self.close_project_dialog(cx),
                "enter" => self.restore_picked_version(cx),
                "up" if d.picked > 0 && !d.busy => {
                    d.picked -= 1;
                    cx.notify();
                }
                "down" if d.picked + 1 < d.rows.len() && !d.busy => {
                    d.picked += 1;
                    cx.notify();
                }
                _ => {}
            },
            Some(Dialog::Collect(c)) => match (&*c, key) {
                (Collect::Run(_), "escape") => self.cancel_collect(cx),
                (Collect::Ask(_), "enter") => self.start_collect(cx),
                (Collect::Ask(_), "escape") | (Collect::Done(_) | Collect::Failed(_), "enter" | "escape") => self.close_project_dialog(cx),
                _ => {}
            },
        }
    }

    // ---- New project -------------------------------------------------------------------

    /// File > New > Project…, the welcome screen's New project: asks for a name, a folder, a
    /// size and a rate, all direct choices.
    pub(crate) fn open_new_project(&mut self, cx: &mut Context<Self>) {
        self.open_new_project_from(None, cx);
    }

    /// File > New project, or, with a template, File > New from template and the welcome screen's
    /// From template: the same dialog, with the template's own settings in place of the size and rate.
    pub(crate) fn open_new_project_from(&mut self, template: Option<String>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let size = 0;
        let rate = default_rate_index(match self.session {
            Some(_) => Some(self.fps),
            None => self.pending_settings.1,
        });
        self.close_card();
        self.welcome_templates = false;
        self.proj_dialog = Some(Dialog::NewProject(NewProject {
            field: TextField::selected("Untitled"),
            dir: default_project_dir(),
            size,
            rate,
            template,
            error: None,
        }));
        cx.notify();
    }

    /// File > Save as template…: the open project's settings, and optionally its clips, as a named
    /// template in the user's data folder.
    pub(crate) fn open_save_template(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() || self.exporting().is_some() {
            return;
        }
        self.close_card();
        self.proj_dialog = Some(Dialog::SaveTemplate(SaveTemplate {
            field: TextField::selected("My template"),
            keep_clips: false,
            error: None,
        }));
        cx.notify();
    }

    /// The Keep clips switch of the Save as template dialog.
    pub(crate) fn set_template_keep_clips(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(Dialog::SaveTemplate(d)) = &mut self.proj_dialog {
            d.keep_clips = on;
            cx.notify();
        }
    }

    /// Writes the open project as the template the dialog names. The export preset the export sheet
    /// shows is kept with it, unless the sheet is on Custom.
    pub(crate) fn commit_save_template(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::SaveTemplate(d)) = &mut self.proj_dialog else {
            return;
        };
        let name = d.field.text.trim().to_string();
        if !engine::templates::valid_name(&name) {
            d.error = Some(if engine::templates::BUILTINS.iter().any(|b| b.name.eq_ignore_ascii_case(&name)) {
                format!("{name} is a built-in template: pick another name")
            } else {
                "Type a name of up to 40 characters, without slashes".into()
            });
            cx.notify();
            return;
        }
        let keep = d.keep_clips;
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let export = self.preset_now();
        let Some(dir) = engine::templates::dir() else {
            if let Some(Dialog::SaveTemplate(d)) = &mut self.proj_dialog {
                d.error = Some("There is no data folder to keep templates in".into());
            }
            cx.notify();
            return;
        };
        match engine::templates::save(&dir, session, &name, keep, export) {
            Ok(replaced) => {
                self.proj_dialog = None;
                let verb = if replaced { "Replaced" } else { "Saved" };
                self.notify_user(format!("{verb} template {name}").into());
            }
            Err(e) => {
                if let Some(Dialog::SaveTemplate(d)) = &mut self.proj_dialog {
                    d.error = Some(format!("Not saved: {e}"));
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn new_project_pick(&mut self, size: Option<usize>, rate: Option<usize>, cx: &mut Context<Self>) {
        if let Some(Dialog::NewProject(d)) = &mut self.proj_dialog {
            d.size = size.unwrap_or(d.size).min(NEW_SIZES.len() - 1);
            d.rate = rate.unwrap_or(d.rate).min(NEW_RATES.len() - 1);
            cx.notify();
        }
    }

    pub(crate) fn choose_new_project_folder(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NewProject(d)) = &self.proj_dialog else {
            return;
        };
        let start = d.dir.clone();
        let picked = cx.background_executor().spawn(async move { crate::files::pick_folder("edith — new project folder", &start) });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(folder)) => {
                    if let Some(Dialog::NewProject(d)) = &mut this.proj_dialog {
                        d.dir = folder;
                        d.error = None;
                        cx.notify();
                    }
                }
                Ok(None) => {}
                Err(text) => {
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Starts the project. Nothing is written yet: a project file names at least one source, so
    /// the file appears with the first footage, which is saved to it at once. Until then the
    /// size and rate wait as the pending settings the first file is conformed to.
    pub(crate) fn commit_new_project(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::NewProject(d)) = &mut self.proj_dialog else {
            return;
        };
        let Some(target) = d.target() else {
            d.error = Some("Type a name for the project".into());
            cx.notify();
            return;
        };
        if target.exists() {
            d.error = Some(format!("{} is already there: pick another name", file_name(&target)));
            cx.notify();
            return;
        }
        let (size, rate) = (NEW_SIZES[d.size].1, NEW_RATES[d.rate]);
        let dir = d.dir.clone();
        let clips = self.session.as_ref().map_or(0, engine::versions::clip_count);
        if self.session.is_some() && self.edited_since_save && clips > 0 {
            if let Some(Dialog::NewProject(d)) = &mut self.proj_dialog {
                d.error = Some(format!("{} has unsaved changes: save it first", file_name(&self.project_path)));
            }
            cx.notify();
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            if let Some(Dialog::NewProject(d)) = &mut self.proj_dialog {
                d.error = Some(format!("Could not use that folder: {e}"));
            }
            cx.notify();
            return;
        }
        if let Some(template) = d.template.clone() {
            return self.start_from_template(&template, target, cx);
        }
        self.proj_dialog = None;
        if self.session.is_some() {
            self.close_session();
        }
        self.pending_settings = (Some(size), Some(rate), None);
        self.name = file_name(&target).trim_end_matches(".edith").to_string().into();
        self.new_project = Some(target);
        cx.notify();
    }

    /// Starts the project File > New from template makes. A template with clips writes its project
    /// file to `target` (its sources come with it) and opens it. One without clips is started as a
    /// new project: its canvas and rate are pending, and its lanes, names, caption style and export
    /// preset are put on when the first footage arrives, as the file appears then.
    fn start_from_template(&mut self, template: &str, target: PathBuf, cx: &mut Context<Self>) {
        let Some(dir) = engine::templates::dir() else {
            if let Some(Dialog::NewProject(d)) = &mut self.proj_dialog {
                d.error = Some("There is no data folder to read templates from".into());
            }
            cx.notify();
            return;
        };
        let info = engine::templates::list(&dir).into_iter().find(|i| i.name == template);
        let Some(info) = info else {
            self.proj_dialog = None;
            self.notify_user(format!("The template {template} is gone").into());
            cx.notify();
            return;
        };
        self.proj_dialog = None;
        if info.clips {
            return match engine::templates::write_project(&dir, template, &target) {
                Ok(export) => {
                    self.apply_template_export(export.as_deref());
                    self.open_recent(&target, cx);
                }
                Err(e) => {
                    self.notify_user(format!("Could not start from {template}: {e}").into());
                    cx.notify();
                }
            };
        }
        let Ok(opened) = engine::templates::open(&dir, template) else {
            self.notify_user(format!("Could not start from {template}").into());
            cx.notify();
            return;
        };
        let session = opened.session;
        let size = session.resolution();
        let fps = session.meta().frame_rate;
        if self.session.is_some() {
            self.close_session();
        }
        self.pending_settings = (Some(size), Some(fps), None);
        self.pending_template = Some(template.to_string());
        self.name = file_name(&target).trim_end_matches(".edith").to_string().into();
        self.new_project = Some(target);
        cx.notify();
    }

    /// Sets the export sheet to the preset a template names (`None`, or the match-project tile, when
    /// the template names no preset of edith's own).
    pub(crate) fn apply_template_export(&mut self, export: Option<&str>) {
        if let Some(id) = export {
            self.choose_preset(id);
        }
    }

    // ---- Save version ------------------------------------------------------------------

    /// The folder this project's versions live in: beside the project file.
    fn versions_dir(&self) -> PathBuf {
        versions::dir_beside(&self.project_path)
    }

    /// File > Project notes: the notes the project holds, open for editing with the caret at the end.
    pub(crate) fn open_project_notes(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let text = session.notes().to_string();
        self.close_card();
        self.proj_dialog = Some(Dialog::Notes(Notes { field: TextField { text, all: false } }));
        cx.notify();
    }

    /// Save on the notes dialog: one undo step in the project when the text changed, and the
    /// project reads unsaved until it is written.
    pub(crate) fn commit_project_notes(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Notes(d)) = &self.proj_dialog else {
            return;
        };
        let text = d.field.text.clone();
        if let Some(session) = &mut self.session
            && session.set_notes(&text)
        {
            self.mark_dirty();
            self.notify_user("Project notes saved".into());
        }
        self.proj_dialog = None;
        cx.notify();
    }

    pub(crate) fn open_save_version(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        let dir = self.versions_dir();
        // A name that is free, ready to be typed over.
        let taken: Vec<String> = versions::list(&dir).into_iter().map(|v| v.name).collect();
        let name = (1..).map(|n| format!("version-{n}")).find(|n| !taken.contains(n)).unwrap_or_default();
        self.close_card();
        self.proj_dialog = Some(Dialog::SaveVersion(SaveVersion { field: TextField::selected(name), dir, error: None }));
        cx.notify();
    }

    pub(crate) fn commit_save_version(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::SaveVersion(d)) = &mut self.proj_dialog else {
            return;
        };
        let Some(name) = versions::clean_name(&d.field.text) else {
            d.error = Some("Type a name for this version".into());
            cx.notify();
            return;
        };
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let text = match versions::save(session, &d.dir, &name) {
            Ok(saved) => {
                let replaced = if saved.replaced.is_some() { "Replaced" } else { "Saved" };
                format!("{replaced} version {name} · {} clip{}", saved.clips, if saved.clips == 1 { "" } else { "s" })
            }
            Err(e) => {
                d.error = Some(format!("Not saved: {e}"));
                cx.notify();
                return;
            }
        };
        eprintln!("{text}");
        self.proj_dialog = None;
        self.notify_user(text.into());
        cx.notify();
    }

    // ---- Export captions ---------------------------------------------------------------

    /// File > Export captions: the sheet opens on the default export name and folder, as an
    /// export does, with SRT chosen. No picker opens.
    pub(crate) fn open_export_captions(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let sources: Vec<&Path> = session.sources().iter().map(|s| s.path.as_path()).collect();
        let first_clip = sources.first().map(|path| crate::files::stem(path));
        let saved = self.autosave_armed || self.new_project_fresh;
        let path = crate::player::library::default_export_path(&self.project_path, saved, first_clip.as_deref(), &sources, engine::export::Format::Mp4);
        let stem = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.close_card();
        self.proj_dialog = Some(Dialog::Captions(CaptionsExport { field: TextField::selected(stem), format: CaptionFormat::Srt, dir, error: None }));
        cx.notify();
    }

    /// The format row: SRT or WebVTT, a direct choice.
    pub(crate) fn captions_pick(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(Dialog::Captions(d)) = &mut self.proj_dialog {
            d.format = CaptionFormat::ALL[at.min(CaptionFormat::ALL.len() - 1)];
            d.error = None;
            cx.notify();
        }
    }

    /// Writes the timeline's captions as the sheet names them. A name with a separator in it,
    /// and a timeline with no captions, stay on the sheet with the reason under the field.
    pub(crate) fn commit_export_captions(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Captions(d)) = &mut self.proj_dialog else {
            return;
        };
        let name = d.field.text.trim().to_string();
        if name.is_empty() || name.contains(['/', '\\']) {
            d.error = Some("Type a name for the file, without a slash".into());
            cx.notify();
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let cues = session.caption_cues();
        if cues.is_empty() {
            d.error = Some("No captions on the timeline yet".into());
            cx.notify();
            return;
        }
        let path = d.dir.join(format!("{name}.{}", d.format.ext()));
        let text = match d.format {
            CaptionFormat::Srt => engine::subtitle::write_srt(&cues),
            CaptionFormat::Vtt => engine::subtitle::write_vtt(&cues),
        };
        if let Err(e) = std::fs::write(&path, text) {
            d.error = Some(format!("Not saved: {e}"));
            cx.notify();
            return;
        }
        self.proj_dialog = None;
        self.notify_user(format!("Captions saved to {}", path.display()).into());
        cx.notify();
    }

    // ---- Path entry --------------------------------------------------------------------

    /// The sheet that stands in for a file picker when no chooser can open here: the person
    /// types or pastes the path and the same door takes it.
    pub(crate) fn open_path_entry(&mut self, what: PathFor, cx: &mut Context<Self>) {
        self.open_path_entry_from(what, None, cx);
    }

    /// The same sheet for a Relink, which keeps the missing file it is for.
    pub(crate) fn open_path_entry_from(&mut self, what: PathFor, from: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        self.close_card();
        self.proj_dialog = Some(Dialog::PathEntry(PathEntry { field: TextField::clicked(""), what, from, error: None }));
        cx.notify();
    }

    /// The Paste key on the sheet: the clipboard's text goes in at the caret, as typed.
    pub(crate) fn path_entry_paste(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::PathEntry(d)) = &mut self.proj_dialog else {
            return;
        };
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if d.field.all {
            d.field.text.clear();
            d.field.all = false;
        }
        d.field.text.extend(text.trim().chars().filter(|c| !c.is_control()).take(PATH_MAX.saturating_sub(d.field.text.len())));
        d.error = None;
        cx.notify();
    }

    /// Takes the typed path to what it was for. A path that is not a file stays on the sheet
    /// with the reason under the field.
    pub(crate) fn commit_path_entry(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::PathEntry(d)) = &mut self.proj_dialog else {
            return;
        };
        let text = d.field.text.trim().trim_matches(['"', '\'']).to_string();
        let path = PathBuf::from(&text);
        if text.is_empty() {
            d.error = Some("Type the file's path first".into());
            cx.notify();
            return;
        }
        if !path.is_file() {
            d.error = Some(format!("No file at {text}"));
            cx.notify();
            return;
        }
        let (what, from) = (d.what, d.from.clone());
        self.proj_dialog = None;
        match what {
            PathFor::Import => self.import(&path, cx),
            PathFor::OpenProject => self.open_recent(&path, cx),
            PathFor::Subtitles => self.add_subtitles(&path, cx),
            PathFor::Lut { relink } => self.set_lut(&path, relink, cx),
            PathFor::Relink => {
                if let Some(from) = from {
                    self.relink_to(from, path, cx);
                }
            }
        }
        cx.notify();
    }

    // ---- Go to time --------------------------------------------------------------------

    /// The timecode on the transport, clicked: it becomes a field with the time it shows all
    /// selected, so the first key replaces it. Keys then go to [`Player::time_edit_key`].
    pub(crate) fn open_time_entry(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() || self.session.is_none() {
            return;
        }
        let now = self.session.as_ref().map_or(0., PlaybackSession::now);
        let shown = crate::viewport::timecode(self.hero_position(now), self.active_fps());
        self.close_card();
        self.time_edit = Some(TextField::selected(shown));
        cx.notify();
    }

    /// The keys the Go to time field takes: enter jumps, escape cancels, the rest is text.
    pub(crate) fn time_edit_key(&mut self, key: &str, ctrl: bool, alt: bool, key_char: Option<&str>, cx: &mut Context<Self>) {
        let Some(field) = self.time_edit.as_mut() else {
            return;
        };
        match field.key(key, ctrl, alt, key_char, TIME_MAX) {
            FieldKey::Cancel => self.time_edit = None,
            FieldKey::Commit => self.commit_time_entry(cx),
            FieldKey::Edited => {}
        }
        cx.notify();
    }

    /// Enter on the time field: the typed time is read and the playhead goes there. A time that
    /// does not read is said in a note, and the field stays open with what was typed.
    pub(crate) fn commit_time_entry(&mut self, cx: &mut Context<Self>) {
        let Some(field) = &self.time_edit else {
            return;
        };
        let jump = match crate::ui::timeband_stance::parse_jump(&field.text, self.active_fps()) {
            Ok(jump) => jump,
            Err(why) => {
                self.notify_user(why.into());
                cx.notify();
                return;
            }
        };
        self.time_edit = None;
        match jump {
            crate::ui::timeband_stance::Jump::By(frames) => self.step(frames, cx),
            crate::ui::timeband_stance::Jump::At(frame) => {
                let fps = self.active_fps();
                let last = self
                    .active_session()
                    .map_or(0, |s| ((s.timeline_duration() * fps).round() as i64 - 1).max(0));
                self.seek(frame.clamp(0, last) as f64 / fps, cx);
            }
        }
        cx.notify();
    }

    // ---- Versions ----------------------------------------------------------------------

    pub(crate) fn open_versions(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() {
            return;
        }
        let dir = self.versions_dir();
        let rows = versions::list(&dir);
        self.close_card();
        self.proj_dialog = Some(Dialog::Versions(VersionList { rows, picked: 0, dir, busy: false }));
        cx.notify();
    }

    pub(crate) fn pick_version(&mut self, at: usize, cx: &mut Context<Self>) {
        if let Some(Dialog::Versions(d)) = &mut self.proj_dialog
            && !d.busy
            && at < d.rows.len()
        {
            d.picked = at;
            cx.notify();
        }
    }

    /// Reads the picked version in on a worker (a project names films; opening one is not
    /// instant) and takes it over as one undo step ([`PlaybackSession::adopt_as_step`]).
    pub(crate) fn restore_picked_version(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Versions(d)) = &mut self.proj_dialog else {
            return;
        };
        let Some(row) = d.rows.get(d.picked).filter(|_| !d.busy) else {
            return;
        };
        let (name, file) = (row.name.clone(), versions::file(&d.dir, &row.name));
        d.busy = true;
        cx.notify();
        let opened = cx.background_executor().spawn(async move { PlaybackSession::open_project(&file).map_err(|e| e.to_string()) });
        cx.spawn(async move |this, cx| {
            let opened = opened.await;
            this.update(cx, |this, cx| this.finish_restore(&name, opened, cx)).ok();
        })
        .detach();
    }

    fn finish_restore(&mut self, name: &str, opened: Result<PlaybackSession, String>, cx: &mut Context<Self>) {
        let text = match opened {
            Err(e) => {
                if let Some(Dialog::Versions(d)) = &mut self.proj_dialog {
                    d.busy = false;
                }
                format!("Version {name} not restored: {e}")
            }
            Ok(other) => {
                if self.exporting().is_some() {
                    format!("Version {name} not restored: an export is running")
                } else if self.step_history(|s| s.adopt_as_step(&other)) {
                    self.selected.clear();
                    self.mark_dirty();
                    self.proj_dialog = None;
                    format!("Restored version {name} · {} takes it back", self.keymap.display(ActionId::Undo))
                } else {
                    format!("Version {name} not restored: it does not fit on this project")
                }
            }
        };
        if let Some(Dialog::Versions(d)) = &mut self.proj_dialog {
            d.busy = false;
        }
        eprintln!("{text}");
        self.notify_user(text.into());
        cx.notify();
    }

    // ---- Collect media -----------------------------------------------------------------

    pub(crate) fn open_collect(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let folder = collect::default_folder(&self.project_path);
        let plan = collect::plan(session.sources(), &folder);
        self.close_card();
        self.proj_dialog = Some(Dialog::Collect(Collect::Ask(plan)));
        cx.notify();
    }

    /// Asks the desktop for the folder, then plans again for it.
    pub(crate) fn choose_collect_folder(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Collect(Collect::Ask(plan))) = &self.proj_dialog else {
            return;
        };
        let start = plan.folder.parent().map(Path::to_path_buf).unwrap_or_default();
        let picked = cx.background_executor().spawn(async move { crate::files::pick_folder("edith — collect media into", &start) });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(folder)) => this.replan_collect(folder, cx),
                Ok(None) => {}
                Err(text) => {
                    this.notify_user(text.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn replan_collect(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let (Some(session), Some(Dialog::Collect(c @ Collect::Ask(_)))) = (self.session.as_ref(), &mut self.proj_dialog) {
            *c = Collect::Ask(collect::plan(session.sources(), &folder));
            cx.notify();
        }
    }

    pub(crate) fn start_collect(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Collect(c)) = &mut self.proj_dialog else {
            return;
        };
        let Collect::Ask(plan) = &*c else {
            return;
        };
        if plan.items.is_empty() {
            return;
        }
        let plan = plan.clone();
        let (done, cancel, result) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)), Arc::new(Mutex::new(None)));
        let run = Run { total: plan.bytes(), plan: plan.clone(), done: done.clone(), cancel: cancel.clone(), result: result.clone() };
        *c = Collect::Run(run);
        std::thread::spawn(move || {
            let r = collect::copy_all(&plan, &cancel, &mut |d, _| done.store(d, Ordering::Relaxed));
            *result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
        });
        cx.notify();
    }

    pub(crate) fn cancel_collect(&mut self, cx: &mut Context<Self>) {
        if let Some(Dialog::Collect(Collect::Run(run))) = &self.proj_dialog {
            run.cancel.store(true, Ordering::Relaxed);
            cx.notify();
        }
    }

    /// Called every frame: when the worker has finished, relinks and shows the outcome.
    pub(crate) fn poll_collect(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Collect(Collect::Run(run))) = &self.proj_dialog else {
            return;
        };
        let Some(result) = run.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else {
            return;
        };
        let plan = run.plan.clone();
        match result {
            Err(Stop::Cancelled) => {
                self.proj_dialog = None;
                self.notify_user("Collect cancelled — nothing was copied".into());
            }
            Err(Stop::Failed(why)) => self.proj_dialog = Some(Dialog::Collect(Collect::Failed(why))),
            Ok(done) => {
                let moved = self.session.as_mut().map_or(0, |s| s.relink_all(&done.pairs));
                if moved > 0 {
                    self.mark_dirty();
                    self.reset_after_reseek();
                }
                self.proj_dialog = Some(Dialog::Collect(Collect::Done(collect_report(&plan, &done, moved, self.keymap.display(ActionId::Undo)))));
            }
        }
        cx.notify();
    }

    // ---- Open recent -------------------------------------------------------------------

    /// Opens a project from the Open recent list. Unsaved edits are not dropped on a click.
    pub(crate) fn open_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let clips = self.session.as_ref().map_or(0, engine::versions::clip_count);
        if self.edited_since_save && clips > 0 {
            self.notify_user_actions(
                format!("Save changes to {} before opening {}?", file_name(&self.project_path), file_name(path)).into(),
                vec![ToastAction::SaveThenOpen(path.to_path_buf()), ToastAction::OpenDiscarding(path.to_path_buf())],
            );
            cx.notify();
            return;
        }
        self.import(path, cx);
    }

    /// File > Open project… and the welcome screen's Open project button: a chooser that shows
    /// only `.edith` files, and the file it picks goes through the same door a recent project
    /// does (so unsaved edits are asked about, not dropped). The chooser runs on a background
    /// thread, as the import one does, so the window keeps painting while it is up.
    pub(crate) fn pick_open_project(&mut self, cx: &mut Context<Self>) {
        if self.exporting().is_some() {
            return;
        }
        let picked = cx.background_executor().spawn(async { pick_project_file() });
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            this.update(cx, |this, cx| match picked {
                Ok(Some(path)) => this.open_recent(&path, cx),
                // Cancelled: nothing to say.
                Ok(None) => {}
                // No chooser here: the same question, typed.
                Err(_) => this.open_path_entry(PathFor::OpenProject, cx),
            })
            .ok();
        })
        .detach();
    }
}

/// The project chooser: zenity or kdialog, filtered to `.edith` files and titled for what it is
/// for. `Err` is a machine with no chooser at all.
fn pick_project_file() -> Result<Option<PathBuf>, &'static str> {
    crate::files::run_picker([
        (
            "zenity",
            vec!["--file-selection".into(), "--title=Open project".into(), "--file-filter=edith projects | *.edith".into()],
        ),
        ("kdialog", vec!["--title=Open project".into(), "--getopenfilename".into(), ".".into(), "*.edith".into()]),
    ])
    .ok_or(crate::files::NO_DIALOG)
}

/// The lines the Done face shows: what was copied, what was relinked, what was left alone.
pub(crate) fn collect_report(plan: &Plan, done: &collect::Done, moved: usize, undo: String) -> Vec<String> {
    let mut lines = vec![
        format!("Copied {} file{} ({}) to", done.files, plural(done.files), collect::size_text(done.bytes)),
        crate::recent::short_path(&plan.folder, 58),
        format!("Relinked {moved} source{} to the copies. Save the project to keep it; {undo} takes it back.", plural(moved)),
    ];
    let reused = plan.items.iter().filter(|i| i.present).count();
    if reused > 0 {
        lines.push(format!("{reused} file{} were already there and were reused", plural(reused)));
    }
    if plan.generated > 0 {
        lines.push(format!("{} generated source{} skipped (not files)", plan.generated, plural(plan.generated)));
    }
    if !plan.offline.is_empty() {
        lines.push(format!("{} missing file{} skipped: {}", plan.offline.len(), plural(plan.offline.len()), names(&plan.offline)));
    }
    lines
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn names(paths: &[PathBuf]) -> String {
    let mut shown: Vec<String> = paths.iter().take(3).map(|p| file_name(p)).collect();
    if paths.len() > 3 {
        shown.push(format!("and {} more", paths.len() - 3));
    }
    shown.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_what_was_left_alone() {
        let plan = Plan {
            folder: PathBuf::from("/p/x_media"),
            generated: 2,
            offline: vec![PathBuf::from("/a/gone.mp4")],
            ..Plan::default()
        };
        let mut done = collect::Done::default();
        done.files = 1;
        done.bytes = 3_400_000;
        let lines = collect_report(&plan, &done, 1, "Ctrl+Z".into());
        assert_eq!(lines[0], "Copied 1 file (3.4 MB) to", "{lines:?}");
        assert_eq!(lines[1], "/p/x_media", "{lines:?}");
        assert!(lines[2].starts_with("Relinked 1 source to"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("2 generated sources skipped")));
        assert!(lines.iter().any(|l| l.contains("1 missing file skipped: gone.mp4")));
    }
}
