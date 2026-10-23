//! The export queue: exports set up now and run later, one after another.
//! Each job owns the timeline as it was when it was added, so editing on
//! afterwards never changes a queued file. This module is only the state
//! machine; the player starts the workers and polls them
//! (`Player::poll_queue`), which keeps every rule here testable without a
//! window.

use engine::export::ExportSettings;
use engine::project::Project;
use engine::{ExportHandle, VideoMeta};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Where a queued job stands.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum JobState {
    Waiting,
    Running,
    Done { bytes: u64, wall_secs: f64 },
    Failed(String),
    Cancelled,
}

/// The timeline and settings a job writes from, taken when it was added.
pub(crate) struct Snapshot {
    pub project: Project,
    pub meta: VideoMeta,
    pub settings: ExportSettings,
    pub sample_rate: Option<u32>,
}

pub(crate) struct QueueJob {
    pub id: u64,
    pub out: PathBuf,
    /// Format, preset and length in plain words, for the row.
    pub summary: String,
    pub state: JobState,
    /// Dropped once the worker has it (or the job is cancelled).
    pub snapshot: Option<Snapshot>,
    pub handle: Option<ExportHandle>,
    pub started: Option<Instant>,
    /// Cancel was asked while the worker was running: it settles as Cancelled.
    pub cancelling: bool,
}

#[derive(Default)]
pub(crate) struct ExportQueue {
    pub jobs: Vec<QueueJob>,
    next_id: u64,
    /// "Start queue" was pressed: the next waiting job starts whenever nothing
    /// else is encoding. Cleared by "Pause after this job" and when the queue
    /// runs dry.
    go: bool,
}

impl ExportQueue {
    pub(crate) fn add(&mut self, out: PathBuf, summary: String, snapshot: Snapshot) -> u64 {
        self.next_id += 1;
        self.jobs.push(QueueJob {
            id: self.next_id,
            out,
            summary,
            state: JobState::Waiting,
            snapshot: Some(snapshot),
            handle: None,
            started: None,
            cancelling: false,
        });
        self.next_id
    }

    pub(crate) fn job(&self, id: u64) -> Option<&QueueJob> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut QueueJob> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub(crate) fn waiting(&self) -> usize {
        self.jobs.iter().filter(|j| j.state == JobState::Waiting).count()
    }

    pub(crate) fn running_id(&self) -> Option<u64> {
        self.jobs.iter().find(|j| j.state == JobState::Running).map(|j| j.id)
    }

    /// Jobs still to be written: waiting or running.
    pub(crate) fn unfinished(&self) -> usize {
        self.waiting() + usize::from(self.running_id().is_some())
    }

    pub(crate) fn finished(&self) -> usize {
        self.jobs.len() - self.unfinished()
    }

    /// Whether the queue is going: started and not yet dry. A direct Export
    /// then joins the end of the queue.
    pub(crate) fn is_going(&self) -> bool {
        self.go && self.unfinished() > 0
    }

    pub(crate) fn start(&mut self) {
        self.go = true;
    }

    /// The running job finishes; nothing after it starts.
    pub(crate) fn pause(&mut self) {
        self.go = false;
    }

    /// The job to start now, if any: the queue is going, no worker of the
    /// queue is writing and nothing else (`blocked`) is encoding.
    pub(crate) fn next_ready(&self, blocked: bool) -> Option<u64> {
        if !self.go || blocked || self.running_id().is_some() {
            return None;
        }
        self.jobs.iter().find(|j| j.state == JobState::Waiting).map(|j| j.id)
    }

    /// Takes the snapshot of a waiting job and marks it running.
    pub(crate) fn begin(&mut self, id: u64) -> Option<(Snapshot, PathBuf)> {
        let job = self.job_mut(id).filter(|j| j.state == JobState::Waiting)?;
        let snap = job.snapshot.take()?;
        job.state = JobState::Running;
        job.started = Some(Instant::now());
        Some((snap, job.out.clone()))
    }

    pub(crate) fn attach(&mut self, id: u64, handle: ExportHandle) {
        if let Some(job) = self.job_mut(id) {
            job.handle = Some(handle);
        }
    }

    /// The worker's verdict. A job cancelled while it ran settles as
    /// Cancelled whatever the worker said. Returns the final state.
    pub(crate) fn settle(&mut self, id: u64, result: Result<(), String>, bytes: u64) -> Option<JobState> {
        let job = self.job_mut(id).filter(|j| j.state == JobState::Running)?;
        let wall_secs = job.started.map_or(0., |t| t.elapsed().as_secs_f64());
        job.handle = None;
        job.state = match (job.cancelling, result) {
            (true, _) => JobState::Cancelled,
            (false, Ok(())) => JobState::Done { bytes, wall_secs },
            (false, Err(e)) => JobState::Failed(e),
        };
        let state = job.state.clone();
        if self.waiting() == 0 {
            self.go = false;
        }
        Some(state)
    }

    /// Cancel one job: a waiting one never starts; a running one is asked to
    /// stop and settles as Cancelled once its worker has cleaned up.
    pub(crate) fn cancel(&mut self, id: u64) {
        let Some(job) = self.job_mut(id) else { return };
        match job.state {
            JobState::Waiting => {
                job.snapshot = None;
                job.state = JobState::Cancelled;
            }
            JobState::Running => {
                job.cancelling = true;
                if let Some(h) = &job.handle {
                    h.cancel();
                }
            }
            _ => {}
        }
        if self.waiting() == 0 && self.running_id().is_none() {
            self.go = false;
        }
    }

    /// Takes a job off the list. A running one cannot be removed, only cancelled.
    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id || j.state == JobState::Running);
        if self.unfinished() == 0 {
            self.go = false;
        }
        self.jobs.len() != before
    }

    pub(crate) fn clear_finished(&mut self) {
        self.jobs.retain(|j| matches!(j.state, JobState::Waiting | JobState::Running));
    }

    /// Cancels everything unfinished (the window is closing).
    pub(crate) fn cancel_all(&mut self) {
        let ids: Vec<u64> = self.jobs.iter().map(|j| j.id).collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Files the queue still means to write: a new job must not take the same name.
    pub(crate) fn claimed(&self, path: &Path) -> bool {
        self.jobs
            .iter()
            .any(|j| matches!(j.state, JobState::Waiting | JobState::Running) && j.out == path)
    }
}

/// `out` if nothing is there and the queue has not claimed it; otherwise the
/// same name with " 2", " 3" ... before the extension. A queued job never
/// asks about overwriting (the question belongs to the moment of the click),
/// so it picks a free name instead of replacing anything.
pub(crate) fn free_name(out: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    if !taken(out) {
        return out.to_path_buf();
    }
    let stem = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = out.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| out.with_file_name(format!("{stem} {n}{ext}")))
        .find(|p| !taken(p))
        .unwrap_or_else(|| out.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::PlaybackSession;

    fn session() -> PlaybackSession {
        let path = format!("{}/../../assets/test_av.mp4", env!("CARGO_MANIFEST_DIR"));
        PlaybackSession::open(path).expect("open the fixture")
    }

    fn snap(s: &PlaybackSession) -> Snapshot {
        let (project, meta) = s.export_snapshot();
        Snapshot { project, meta, settings: ExportSettings::default(), sample_rate: s.sample_rate() }
    }

    fn add(q: &mut ExportQueue, s: &PlaybackSession, name: &str) -> u64 {
        q.add(PathBuf::from(name), "MP4".into(), snap(s))
    }

    #[test]
    fn nothing_starts_until_the_queue_is_started() {
        let s = session();
        let mut q = ExportQueue::default();
        let a = add(&mut q, &s, "a.mp4");
        assert_eq!(q.next_ready(false), None, "added jobs wait for Start queue");
        assert!(!q.is_going());
        q.start();
        assert_eq!(q.next_ready(false), Some(a));
        assert_eq!(q.next_ready(true), None, "a direct export encoding holds the queue off");
    }

    #[test]
    fn jobs_run_one_at_a_time_in_order_and_settle() {
        let s = session();
        let mut q = ExportQueue::default();
        let (a, b) = (add(&mut q, &s, "a.mp4"), add(&mut q, &s, "b.mp4"));
        q.start();
        assert_eq!(q.next_ready(false), Some(a));
        assert!(q.begin(a).is_some());
        assert_eq!(q.running_id(), Some(a));
        assert_eq!(q.next_ready(false), None, "never two at once");
        assert!(matches!(q.settle(a, Ok(()), 10), Some(JobState::Done { bytes: 10, .. })));
        assert_eq!(q.next_ready(false), Some(b));
        assert!(q.begin(b).is_some());
        assert!(matches!(q.settle(b, Err("disk full".into()), 0), Some(JobState::Failed(e)) if e == "disk full"));
        assert!(!q.is_going(), "a dry queue stops going");
        assert_eq!(q.finished(), 2);
    }

    #[test]
    fn a_failed_job_does_not_stop_the_next() {
        let s = session();
        let mut q = ExportQueue::default();
        let (a, b) = (add(&mut q, &s, "a.mp4"), add(&mut q, &s, "b.mp4"));
        q.start();
        q.begin(a);
        q.settle(a, Err("bad".into()), 0);
        assert!(q.is_going());
        assert_eq!(q.next_ready(false), Some(b));
    }

    #[test]
    fn cancel_a_waiting_job_it_never_starts() {
        let s = session();
        let mut q = ExportQueue::default();
        let (a, b) = (add(&mut q, &s, "a.mp4"), add(&mut q, &s, "b.mp4"));
        q.start();
        q.cancel(a);
        assert_eq!(q.job(a).unwrap().state, JobState::Cancelled);
        assert!(q.job(a).unwrap().snapshot.is_none(), "the copy is let go");
        assert_eq!(q.next_ready(false), Some(b), "the queue moves past it");
        assert!(q.begin(a).is_none());
    }

    #[test]
    fn cancel_a_running_job_settles_cancelled_whatever_the_worker_says() {
        let s = session();
        let mut q = ExportQueue::default();
        let a = add(&mut q, &s, "a.mp4");
        q.start();
        q.begin(a);
        q.cancel(a);
        assert_eq!(q.job(a).unwrap().state, JobState::Running, "until its worker has cleaned up");
        assert!(!q.remove(a), "a running job cannot be removed");
        assert_eq!(q.settle(a, Err("export cancelled".into()), 0), Some(JobState::Cancelled));
    }

    #[test]
    fn pause_lets_the_running_job_finish_and_holds_the_rest() {
        let s = session();
        let mut q = ExportQueue::default();
        let (a, b) = (add(&mut q, &s, "a.mp4"), add(&mut q, &s, "b.mp4"));
        q.start();
        q.begin(a);
        q.pause();
        q.settle(a, Ok(()), 1);
        assert_eq!(q.next_ready(false), None, "paused after the job");
        q.start();
        assert_eq!(q.next_ready(false), Some(b));
    }

    #[test]
    fn remove_and_clear_finished_keep_what_is_still_to_do() {
        let s = session();
        let mut q = ExportQueue::default();
        let (a, b, c) = (add(&mut q, &s, "a.mp4"), add(&mut q, &s, "b.mp4"), add(&mut q, &s, "c.mp4"));
        q.start();
        q.begin(a);
        q.settle(a, Ok(()), 1);
        assert!(q.remove(c));
        assert_eq!(q.jobs.len(), 2);
        q.clear_finished();
        assert_eq!(q.jobs.iter().map(|j| j.id).collect::<Vec<_>>(), vec![b]);
    }

    #[test]
    fn a_job_keeps_the_timeline_it_was_added_with() {
        let mut s = session();
        let mut q = ExportQueue::default();
        let before = s.export_snapshot().0.media_frames();
        assert!(before > 4);
        let a = add(&mut q, &s, "a.mp4");
        // Edit the live timeline after queuing: delete every clip.
        for lane in s.lanes() {
            while !s.lane_clips(lane).is_empty() {
                assert!(s.delete_clip(lane, 0));
            }
        }
        let after = s.export_snapshot().0.media_frames();
        let queued = q.job(a).unwrap().snapshot.as_ref().unwrap().project.media_frames();
        assert_eq!(queued, before, "the queued copy is the timeline as added");
        assert_ne!(after, before, "the live timeline did change");
    }

    #[test]
    fn a_queued_name_is_never_one_already_there_or_claimed() {
        let s = session();
        let mut q = ExportQueue::default();
        add(&mut q, &s, "/x/a.mp4");
        let taken = |p: &Path| q.claimed(p) || p == Path::new("/x/a 2.mp4");
        assert_eq!(free_name(Path::new("/x/a.mp4"), taken), PathBuf::from("/x/a 3.mp4"));
        assert_eq!(free_name(Path::new("/x/b.mp4"), taken), PathBuf::from("/x/b.mp4"));
    }
}
