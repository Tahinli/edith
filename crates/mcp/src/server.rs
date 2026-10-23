use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use serde_json::Value;

use crate::tools;

#[derive(Default)]
#[allow(dead_code)]
pub struct Server {
    pub session: Option<engine::PlaybackSession>,
    pub path: Option<PathBuf>,
    pub saved_mtime: Option<SystemTime>,
    pub saved_depth: Option<usize>,
    /// The engine's `history_pushes` at the last save or open; with `saved_depth`
    /// it says whether the project differs from what is on disk.
    pub saved_pushes: Option<u64>,
    /// The serial of the undo ledger's top entry at the last save or open (0 when the ledger
    /// was empty). Undoing back to it is clean; a new edit after undoing past it is not, since
    /// it leaves the ledger without that entry and no undo or redo can reach it again.
    pub saved_top: u64,
    /// How many sources the project had at the last save or open. An import moves
    /// neither depth nor pushes, so only this says a library row was added.
    pub saved_sources: Option<usize>,
    /// The open project as the engine writes it (playhead line left out), taken when the
    /// file was opened or saved. An autosave is written only when the session differs from
    /// this, so a project nobody changed leaves nothing beside it.
    pub saved_body: Option<Vec<u8>>,
    /// The take in progress, from `record {action:"start"}` until its stop.
    pub recording: Option<tools::record::Active>,
    pub ledger: Vec<Entry>,
    pub redo_ledger: Vec<Entry>,
    /// Motion tracks running or finished but not yet written onto their masks (`track`).
    pub tracks: Vec<engine::playback::TrackJob>,
    /// Set once the ledger has lost an entry to the engine's undo cap, so `undo`
    /// can say why it has nothing left. Cleared wherever the ledger is.
    pub history_capped: bool,
    /// Set when opening a project dropped a ledger that still held steps, so `undo` can say
    /// the history did not survive the open. Cleared wherever a new edit lands.
    pub history_lost_on_open: bool,
    pub jobs: Vec<Job>,
    /// The render preview being made, and the frames it covers ([`engine::render_cache`]).
    pub render: Option<(engine::render_cache::Job, (u32, u32))>,
    /// Set when the open project came from a file with a newer autosave beside it that was
    /// not recovered. A save then keeps that autosave under a dated name instead of removing it.
    pub unrecovered_autosave: bool,
    /// The autosave this session recovered (`project_open {recover: true}`): its contents are
    /// the session's own, so the session's next autosave may replace it. Any other autosave
    /// beside the project holds edits this session never loaded and is moved aside first.
    pub recovered_autosave: Option<PathBuf>,
    /// Every project file this session opened or saved, canonical path and the mtime it was
    /// left with. A save to one of these is this project's own file, with no force needed.
    pub own_files: Vec<(PathBuf, Option<SystemTime>)>,
    /// Set while the open project is a media file started as a project and has no project
    /// file yet: (undo depth, history pushes, sources) as it opened. Its autosave goes to the
    /// cache, and only once an edit moves those counts, so a look at a file writes nothing.
    pub media_open: Option<(usize, u64, usize)>,
    /// What the open had to change to load (a clip past its source, clamped): the file on disk
    /// still has the problem, so the project counts as unsaved until a save writes the fix.
    pub load_fixes: Vec<String>,
    /// Each source's frame rate and playing length as its file read when last seen on disk, by
    /// path. A relink compares the new file with it when the old file is offline and cannot be read.
    pub source_shape: std::collections::HashMap<PathBuf, crate::tools::media::Shape>,
    /// The data folder templates and untitled projects' tones are kept under (`$XDG_DATA_HOME`
    /// or `~/.local/share`). `None` is the user's own; tests set a temporary folder here.
    pub data_home: Option<PathBuf>,
}

impl Server {
    /// The folder templates are kept in, under this server's data folder.
    pub fn templates_dir(&self) -> Option<PathBuf> {
        match &self.data_home {
            Some(data) => Some(engine::templates::dir_in(data)),
            None => engine::templates::dir(),
        }
    }
}

/// The folder a tone of the project at `project` is kept in: beside the project file when that
/// file exists (the project is saved), else under the data folder `data` (the user's when `None`).
pub fn tone_folder(project: Option<&std::path::Path>, data: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(p) = project
        && p.is_file()
        && let Some(parent) = p.parent()
        && !parent.as_os_str().is_empty()
    {
        return Some(parent.to_path_buf());
    }
    match data {
        Some(data) => Some(engine::generate::Tone::dir_in(data)),
        None => engine::generate::Tone::dir(),
    }
}

/// One undo-ledger entry: a batch's engine steps, and the name undo and redo report for it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Engine undo steps the batch pushed; `undo` takes this many back.
    pub steps: usize,
    /// The batch's `label`, else a summary of its ops (`split @45 + fade V1@0 (+2 more)`).
    pub label: String,
    /// Unique per entry, never reused, and kept when undo or redo moves the entry between
    /// the two ledgers. The project's saved position is the serial of the entry on top.
    pub serial: u64,
}

impl Entry {
    pub fn new(steps: usize, label: impl Into<String>) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { steps, label: label.into(), serial }
    }
}

/// What an exported file measures, read back from the file: integrated loudness (LUFS, the
/// same EBU R128 measurement `loudness` reports) and the highest true peak (dBTP).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FileLevels {
    pub lufs: f64,
    pub true_peak_dbtp: f64,
}

/// What a queued export needs to start later: the timeline as it was when queued.
pub struct Pending {
    pub project: engine::Project,
    pub meta: engine::VideoMeta,
    pub settings: engine::export::ExportSettings,
    pub sample_rate: Option<u32>,
    pub length_secs: f64,
}

#[allow(dead_code)]
pub struct Job {
    pub out: PathBuf,
    /// The running worker; `None` while the job waits in the queue (`pending`) and for a
    /// queued job cancelled before it started.
    pub handle: Option<engine::ExportHandle>,
    /// A queued job's own copy of the timeline and settings, taken when it was queued; it starts
    /// when every job before it has finished (see `tools::export::advance`).
    pub pending: Option<Box<Pending>>,
    pub started: Instant,
    pub outcome: Option<Result<(), String>>,
    /// The picture encoder planned at start (`SW encode (rav1e)`, `copy (source packets)`, ...);
    /// empty for an audio-only export.
    pub picture: String,
    /// The format the export writes, for the codec and container words the replies use.
    pub format: engine::export::Format,
    /// The finished file's loudness and true peak, measured once from the file itself (the encoder
    /// can add overshoot above the mix). `None` until measured; `Some(None)` for a file with no sound.
    pub measured: Option<Option<FileLevels>>,
}

impl Job {
    /// Whether the job is waiting in the queue and has not started.
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }

    pub fn progress(&self) -> f32 {
        self.handle.as_ref().map_or(0.0, engine::ExportHandle::progress)
    }

    /// A job with no worker yet is not finished; one whose waiting was cancelled has an outcome.
    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_some_and(engine::ExportHandle::is_finished)
    }

    pub fn is_cancelling(&self) -> bool {
        self.handle.as_ref().is_some_and(engine::ExportHandle::is_cancelling)
    }

    pub fn encoders(&self) -> Option<String> {
        self.handle.as_ref().and_then(engine::ExportHandle::encoders)
    }

    pub fn stage(&self) -> Option<(&'static str, f32)> {
        self.handle.as_ref().and_then(engine::ExportHandle::stage)
    }

    pub fn length_secs(&self) -> f64 {
        match (&self.handle, &self.pending) {
            (Some(h), _) => h.length_secs(),
            (None, Some(p)) => p.length_secs,
            _ => 0.0,
        }
    }

    pub fn result(&self) -> Option<engine::Result<()>> {
        self.handle.as_ref().and_then(engine::ExportHandle::result)
    }

    /// Stops the job: a running one at its next checkpoint, a waiting one at once (it never starts).
    pub fn cancel(&mut self) {
        match &self.handle {
            Some(h) => h.cancel(),
            None => {
                if self.pending.take().is_some() {
                    self.outcome = Some(Err("cancelled".to_string()));
                }
            }
        }
    }
}

pub struct Reply {
    pub text: String,
    /// Boxed so `Reply` stays small: with serde_json's `preserve_order` (unified in by the
    /// workspace build) an inline `Value` is 72 bytes and `Reply` crosses clippy's `result_large_err`
    /// limit wherever it is the `Err` of a `Result`.
    pub structured: Option<Box<Value>>,
    pub is_error: bool,
    /// Image content blocks after the text: `(mime, base64)` pairs.
    pub images: Vec<(String, String)>,
}

#[allow(dead_code)]
impl Reply {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), structured: None, is_error: false, images: Vec::new() }
    }

    pub fn err(text: impl Into<String>) -> Self {
        Self { text: text.into(), structured: None, is_error: true, images: Vec::new() }
    }

    pub fn with(mut self, v: Value) -> Self {
        self.structured = Some(Box::new(v));
        self
    }

    /// Adds one PNG-or-other image block after the text. `b64` is standard base64.
    pub fn image(mut self, mime: impl Into<String>, b64: impl Into<String>) -> Self {
        self.images.push((mime.into(), b64.into()));
        self
    }
}

pub type ToolFn = fn(&mut Server, &Value) -> Reply;

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
    pub run: ToolFn,
}

pub fn tools() -> Vec<Tool> {
    let mut all = Vec::new();
    all.extend(tools::project::tools());
    all.extend(tools::media::tools());
    all.extend(tools::look::tools());
    all.extend(tools::edit::tools());
    all.extend(tools::edit_subs::tools());
    all.extend(tools::export::tools());
    all.extend(tools::render::tools());
    all.extend(tools::see::tools());
    all.extend(tools::hear::tools());
    all.extend(tools::listen::tools());
    all.extend(tools::scan::tools());
    all.extend(tools::speak::tools());
    all.extend(tools::record::tools());
    all.extend(tools::voice::tools());
    all.extend(tools::effects::tools());
    all.extend(tools::stabilise::tools());
    all.extend(tools::track::tools());
    all.extend(tools::sequence::tools());
    all.extend(tools::multicam::tools());
    all
}

/// Early-returns the `Err(Reply)` of a `Result<_, Reply>` from a tool fn.
#[allow(unused_macros)]
macro_rules! tri {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(r) => return r,
        }
    };
}
#[allow(unused_imports)]
pub(crate) use tri;

#[allow(dead_code)]
const NO_PROJECT: &str = "no project open — call project_open or project_new first";

#[allow(dead_code)]
impl Server {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session(&mut self) -> Result<&mut engine::PlaybackSession, Reply> {
        self.session.as_mut().ok_or_else(|| Reply::err(NO_PROJECT))
    }

    pub fn session_ref(&self) -> Result<&engine::PlaybackSession, Reply> {
        self.session.as_ref().ok_or_else(|| Reply::err(NO_PROJECT))
    }

    /// Drops the oldest ledger entries until their steps fit in the engine's undo
    /// history. The engine keeps 100 steps and drops the oldest past that, so a
    /// batch with a step gone cannot be undone whole, and undoing it would take
    /// steps off the batches after it. Sets `history_capped` when it drops one.
    pub fn trim_ledger(&mut self) {
        let Some(depth) = self.session.as_ref().map(|s| s.undo_depth()) else {
            return;
        };
        let mut total: usize = self.ledger.iter().map(|e| e.steps).sum();
        while total > depth {
            total -= self.ledger.remove(0).steps;
            self.history_capped = true;
        }
    }

    /// The serial of the undo ledger's top entry, 0 when nothing can be undone.
    pub fn ledger_top(&self) -> u64 {
        self.ledger.last().map_or(0, |e| e.serial)
    }
}

#[allow(dead_code)]
pub fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, Reply> {
    opt_str(args, name)
        .ok_or_else(|| Reply::err(format!("missing/invalid argument `{name}`: expected a string")))
}

#[allow(dead_code)]
pub fn opt_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name)?.as_str()
}

#[allow(dead_code)]
pub fn opt_bool(args: &Value, name: &str) -> Option<bool> {
    args.get(name)?.as_bool()
}

#[allow(dead_code)]
pub fn opt_f64(args: &Value, name: &str) -> Option<f64> {
    args.get(name)?.as_f64()
}

#[allow(dead_code)]
pub fn opt_u64(args: &Value, name: &str) -> Option<u64> {
    args.get(name)?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reply_stays_under_clippy_large_err_limit() {
        // clippy::result_large_err fires at 128 bytes; `Reply` is the Err of most tool helpers.
        assert!(std::mem::size_of::<Reply>() < 128, "Reply is {} bytes", std::mem::size_of::<Reply>());
    }

    #[test]
    fn with_keeps_the_structured_value() {
        let r = Reply::ok("done").with(json!({"placed": true}));
        assert_eq!(r.structured.as_deref(), Some(&json!({"placed": true})));
        assert!(Reply::err("no").structured.is_none());
    }
}
