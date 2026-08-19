//! Small stand-ins for big files: a half-size, every-frame-a-keyframe H.264
//! copy of a source, kept in the user's cache and played instead of the
//! original while the cut is being made.
//!
//! Why they exist at all: a 4K HEVC film decodes at a fraction of realtime on
//! this machine, so scrubbing it is a slideshow and every seek is a wait. A
//! proxy is the same film at 1280 on its long edge, coded so that **every frame
//! is a random-access point** -- which is what makes a seek land on the frame
//! that was asked for instead of on the nearest one a decoder can start from,
//! and what takes the whole open-GOP class of seek faults off the table by
//! construction.
//!
//! Three rules, and they are the whole contract:
//!
//! - **Picture only.** No sound is coded into a proxy. The mix always comes off
//!   the original file ([`crate::PlaybackSession`] opens audio by its own path,
//!   which this module never touches), so nothing a listener hears has been
//!   through a second encoder.
//! - **Never exported.** [`crate::export`] reads the project's own sources and
//!   knows nothing of this module. Delivery is the original, always.
//! - **Derived, never authoritative.** A proxy is named after the source's
//!   path, length and modification time ([`path_for`]), so a re-encoded or
//!   re-cut original simply misses the cache and a stale proxy can never be
//!   found. Deleting the cache directory costs time and nothing else -- which
//!   is what lets this cap itself: over [`CACHE_CAP`] the least recently used
//!   stand-ins are deleted to make room ([`sweep`]), because a file written
//!   without being asked for must not be able to fill a disk.
//!
//! Generation is [`crate::export`]'s own decode -> scale -> encode walk, given
//! a one-clip project and a smaller canvas: the hardware seat where the GPU has
//! one, the software encoder where it does not, the same progress and the same
//! cancel a file export has ([`Job`]).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::demux::{Codec, Demuxer, NoVideoTrack, VideoMeta};
use crate::export::{ExportHandle, ExportSettings, Format};
use crate::project::{Clip, LaneKind, Project, Source, Speed};
use crate::scale::FitPolicy;

/// The long edge of a proxy picture, in samples: 1280, so a 4K source loses
/// nine tenths of its samples and a 1080p one a little over half. Chosen as
/// what a cut can be judged on at editing size, in the neighbourhood of every
/// shipped proxy preset (Resolve's half-res, kdenlive's 640/1000-wide
/// defaults).
pub const LONG_EDGE: u32 = 1280;

/// How much disk the stand-ins may hold between them, in bytes: 20 GB, which
/// is one of his 4K feature films and most of a second (a 2h20 one measures
/// ~11.5 GB at ~1.37 MB per second of film), or a dozen of an ordinary length.
///
/// A cap is not a nicety here. Proxies are written *without being asked for*,
/// on import, [`AT_ONCE`] at a time -- so without one the answer to "how much
/// of his disk does this take" is "all of it, eventually". Over the cap, the
/// least recently used are deleted until it fits ([`sweep`]).
///
/// **What this really bounds.** A sweep runs before each new stand-in and
/// leaves room for the ones about to be written ([`reserve`]), so what is
/// *finished* is held to `CACHE_CAP - reserve` and the encodes in flight have
/// the reserve to grow into. The directory therefore sits at about the cap
/// rather than at the cap plus a film per encode, which is what it did when the
/// sweep aimed at the bare number. It can still pass the cap where an encode
/// outruns its own estimate -- a stand-in cannot be deleted while it is being
/// written -- and the next sweep takes that back.
pub const CACHE_CAP: u64 = 20 * 1024 * 1024 * 1024;

/// How many stand-ins may be made at once. Two, not one per core: each is a
/// whole film through a decoder and an encoder, and the hardware seat they
/// queue on is a single one -- more of them at once is the same work done
/// later, with the editor slower while it happens.
///
/// It lives here rather than in the front-end that does the fanning out because
/// the cache's own arithmetic needs it ([`reserve`]): the number of encodes in
/// flight is exactly how much room the sweep has to leave, and two places
/// holding that number separately is one place for them to disagree.
pub const AT_ONCE: usize = 2;

/// What the hardware seat really writes against what it was asked for: 1.25,
/// measured 2026-08-13 (8.3 Mbit/s out of a 6.6 Mbit/s request on radeonsi).
/// Folded into [`reserve`] so the room left for an encode is the room it will
/// actually take, not the room it was told to.
const RATE_OVERSHOOT: f64 = 1.25;

/// Bits per pixel per second a proxy is coded at, [`crate::export`]'s own rate
/// rule at three times the number: every frame being an IDR means no frame
/// borrows anything from its neighbours, so the same picture at the inter rate
/// would be mush -- and mush is not something a cut can be judged on.
const BITS_PER_PIXEL: f64 = 0.3;

/// Whether this is a file worth standing in for: bigger than 1080p, or coded in
/// something this machine decodes slowly even at 1080p. H.264 at or under 1080p
/// plays natively and a proxy of it would cost time and buy nothing.
pub fn wanted(meta: &VideoMeta) -> bool {
    meta.width > 1920 || meta.height > 1080 || matches!(meta.codec, Codec::Hevc | Codec::Av1)
}

/// The picture a proxy of `meta` is coded at: aspect kept, long edge at most
/// [`LONG_EDGE`], both axes even because 4:2:0 has no half chroma samples. A
/// source already that small is coded at its own size (rounded down to even),
/// which is what makes [`generate`] safe to ask for on anything at all.
pub fn size_for(meta: &VideoMeta) -> (u32, u32) {
    let long = meta.width.max(meta.height);
    let scale = match long > LONG_EDGE {
        true => f64::from(LONG_EDGE) / f64::from(long),
        false => 1.0,
    };
    let even = |n: u32| (((f64::from(n) * scale).round() as u32) & !1).max(2);
    (even(meta.width), even(meta.height))
}

/// Where the proxy of `source` lives, whether or not it has been made yet.
///
/// The name is one hash of the source's canonical path, its length **and** its
/// modification time, so an original that changed at all is a different name
/// and its old proxy is simply never looked at again -- the staleness check is
/// the lookup. `None` where the file cannot be stat'd or the machine has no
/// cache directory at all, which is a source that gets no proxy rather than one
/// littering the working directory.
///
/// A re-encoded source therefore leaves its old stand-in behind, and a proxy of
/// a feature film is gigabytes: [`sweep`] is what keeps that from being the
/// whole disk, and it runs before every new one is written.
pub fn path_for(source: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(source).ok()?;
    let mtime = match meta.modified().ok()?.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs() as i128, d.subsec_nanos()),
        Err(e) => (-(e.duration().as_secs() as i128), e.duration().subsec_nanos()),
    };
    let canonical = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let mut bytes = canonical.as_os_str().as_encoded_bytes().to_vec();
    bytes.extend_from_slice(&meta.len().to_le_bytes());
    bytes.extend_from_slice(&mtime.0.to_le_bytes());
    bytes.extend_from_slice(&mtime.1.to_le_bytes());
    let dir = crate::demux::cache_dir(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::var_os("HOME"),
        "proxies",
    )?;
    Some(dir.join(format!("{:016x}.mp4", crate::demux::fnv1a(&bytes))))
}

/// How much of [`CACHE_CAP`] the sweep leaves free for the stand-ins that are
/// about to be written into the directory: [`AT_ONCE`] of them, each about the
/// size of the one being started now.
///
/// The estimate is the file's own arithmetic and not a guess: the picture is
/// coded at [`BITS_PER_PIXEL`] bits per pixel per second and the frame rate
/// cancels out, so it is `width * height * frames * bpp / 8` -- times
/// [`RATE_OVERSHOOT`], because the hardware seat writes over the rate it is
/// asked for.
///
/// Held to half the cap, whatever that comes to. A single 4K feature estimates
/// at over the whole cap, and reserving that would empty the cache to make room
/// for one film -- deleting stand-ins that are in use, for a bound that cannot
/// be met anyway while two of them are being written. Half is the compromise:
/// what is *finished* stays under half the cap, so the two in flight have the
/// other half to grow into before the disk carries more than [`CACHE_CAP`].
fn reserve(width: u32, height: u32, frames: u32) -> u64 {
    let bytes = f64::from(width) * f64::from(height) * f64::from(frames) * BITS_PER_PIXEL / 8.0;
    ((bytes * RATE_OVERSHOOT) as u64)
        .saturating_mul(AT_ONCE as u64)
        .min(CACHE_CAP / 2)
}

/// Deletes least-recently-used stand-ins until the directory holds at most
/// `cap` bytes, and reports what it holds afterwards.
///
/// Run before a new one is written, which is the only moment this directory
/// grows. Nothing else evicts, and nothing else can: a proxy is derived, so
/// deleting one costs the time to make it again and no more -- which is exactly
/// what makes a cap safe to apply behind the user's back where an eviction of
/// anything he *authored* would not be.
///
/// Least recently *used* is read off the access time, with the modification
/// time where a filesystem reports none. On a `relatime` mount -- which is the
/// default, and is what this machine's `/home` is -- an access time moves at
/// most once a day, so this is a daily-granularity LRU rather than an exact
/// one. That is the right accuracy for a thing whose eviction costs a
/// re-encode, and it is why the order is by *time* and not by a hit counter
/// nothing would persist.
///
/// A `.part` is never deleted -- it is being written right now, by this process
/// or another -- but its bytes *are* counted: they are on the disk, and a
/// half-written 11 GB proxy is exactly when the finished ones should make room.
fn sweep(dir: &Path, cap: u64) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    let mut evictable: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        total += meta.len();
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "mp4") {
            let used = meta.accessed().or_else(|_| meta.modified());
            evictable.push((used.unwrap_or(UNIX_EPOCH), meta.len(), path));
        }
    }
    if total <= cap {
        return total;
    }
    evictable.sort_by_key(|(used, ..)| *used);
    for (_, len, path) in evictable {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            eprintln!("proxy cache: evicted {}", path.display());
        }
    }
    total
}

/// The proxy of `source` if one has already been made, `None` otherwise. The
/// answer a session asks at open: a hit costs one `stat`.
pub fn cached(source: &Path) -> Option<PathBuf> {
    path_for(source).filter(|p| p.is_file())
}

/// Deletes the stand-in of `source`, if there is one, and says whether a file
/// went. The one *by hand* eviction: [`sweep`] takes the oldest to stay under a
/// cap, this takes the one that was asked for.
///
/// The source itself is never touched -- what is deleted is [`path_for`]'s
/// answer, a name in the cache directory and nothing else -- and deleting one
/// under a session that is playing off it costs a re-open: [`cached`] stats,
/// finds nothing, and the film itself is what plays from the next span on
/// ([`crate::PlaybackSession::picture_path`]).
/// Three answers and not two, because they are three different rows: `Ok(true)`
/// is a stand-in that went, `Ok(false)` is one that was never there (a no-op,
/// and the switch is honestly off afterwards), and `Err` is a file that is
/// **still on the disk** -- a read-only cache directory, a filesystem gone
/// away. A caller that folded the last two together drew the row as OFF over a
/// proxy [`cached`] went on handing to playback.
pub fn delete(source: &Path) -> std::io::Result<bool> {
    let Some(path) = path_for(source) else {
        return Ok(false);
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {
            eprintln!("proxy cache: deleted {}", path.display());
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The mark beside a cache entry that says its film was switched *off* by hand,
/// and the one thing about a stand-in that outlives the window: without it the
/// next launch's sweep re-encodes the very proxy somebody just turned off.
///
/// Keyed exactly as the stand-in itself is ([`path_for`]) and living beside it,
/// which is what makes it answer for a film that is in no project at all -- the
/// `.edith` file knows only the sources of one timeline, and the switch is on a
/// library row that may belong to none. Derived and disposable like everything
/// else in this directory: a re-encoded source names another key and is asked
/// again, and deleting the cache forgets the choice, which costs one click.
///
/// corner-cut: the choice therefore does not survive a cache wipe or an edit to
/// the source file. Upgrade path is a list in the `.edith`, which needs the
/// project format to carry per-source flags.
fn off_marker(source: &Path) -> Option<PathBuf> {
    Some(path_for(source)?.with_extension("off"))
}

/// Whether this film's stand-in was switched off by hand ([`off_marker`]).
pub fn is_off(source: &Path) -> bool {
    off_marker(source).is_some_and(|p| p.exists())
}

/// Writes or clears that mark. Best effort: a cache directory that cannot be
/// written is a choice that does not survive the session, never a refused
/// click.
pub fn set_off(source: &Path, off: bool) {
    let Some(marker) = off_marker(source) else {
        return;
    };
    match off {
        true => {
            if let Some(dir) = marker.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&marker, b"");
        }
        false => {
            let _ = std::fs::remove_file(&marker);
        }
    }
}

/// The stand-ins being written *right now*, one entry per live encoder, held
/// for the life of the [`Job`].
///
/// The one-encoder-per-film rule lives here rather than only in the window that
/// draws the switch: two starts for one film race over one `.part` path, and
/// the second unlinks the first's file and leaves it writing into an orphaned
/// inode with nothing left holding its handle -- an encode nobody can stop,
/// which finishes minutes later and puts back a proxy somebody had switched
/// off. A guarantee that lives in the caller is a guarantee every future caller
/// has to re-make.
static IN_FLIGHT: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// A held seat in [`IN_FLIGHT`], given up when the job that owns it is dropped.
struct Claim(PathBuf);

impl Drop for Claim {
    fn drop(&mut self) {
        let mut held = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = held.iter().position(|p| *p == self.0) {
            held.swap_remove(i);
        }
    }
}

/// Takes the seat for `out`, or `None` where an encoder already has it.
fn claim(out: &Path) -> Option<Claim> {
    let mut held = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
    match held.iter().any(|p| p == out) {
        true => None,
        false => {
            held.push(out.to_path_buf());
            Some(Claim(out.to_path_buf()))
        }
    }
}

/// A proxy being made -- or one that was already there, which is the same thing
/// to whoever is waiting for a path.
///
/// Poll [`progress`](Job::progress) while [`is_finished`](Job::is_finished) is
/// false and take the [`outcome`](Job::outcome) when it flips, exactly as an
/// export is polled. Dropping the job does not stop the work;
/// [`cancel`](Job::cancel) does.
pub struct Job {
    out: PathBuf,
    /// `None` for a cache hit: there was nothing to run.
    handle: Option<ExportHandle>,
    /// The seat this encoder holds ([`IN_FLIGHT`]), given back when the job
    /// goes. `None` for a cache hit, which codes nothing.
    _claim: Option<Claim>,
}

impl Job {
    /// Fraction of the proxy's pictures written, `0.0..=1.0`. A cache hit is
    /// done.
    ///
    /// The band an export's bar reserves for its sound is taken back out: a
    /// proxy has no sound, so that band is crossed in the first instant and
    /// showing it would be five percent no frame earned.
    pub fn progress(&self) -> f32 {
        let Some(handle) = &self.handle else {
            return 1.0;
        };
        let band = crate::export::AUDIO_BAND as f32 / crate::export::PROGRESS_SCALE as f32;
        ((handle.progress() - band) / (1.0 - band)).clamp(0.0, 1.0)
    }

    /// What is coding this proxy -- the hardware seat or the software one, as
    /// the worker really opened it. `None` before the encoder is open, and for
    /// a cache hit, which coded nothing.
    pub fn encoder(&self) -> Option<String> {
        self.handle.as_ref()?.encoders()
    }

    /// Asks the worker to stop at its next frame and leave no file behind. The
    /// outcome then reports the cancellation as an error.
    pub fn cancel(&self) {
        if let Some(handle) = &self.handle {
            handle.cancel();
        }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(ExportHandle::is_finished)
    }

    /// The proxy's path once it is written, once -- taken out of the job, so a
    /// caller that already took it sees `None`. `None` while it is still being
    /// made.
    pub fn outcome(&self) -> Option<crate::Result<PathBuf>> {
        let Some(handle) = &self.handle else {
            return Some(Ok(self.out.clone()));
        };
        handle.result().map(|r| r.map(|()| self.out.clone()))
    }

    /// Where the proxy will be, whether or not it is there yet -- what a caller
    /// names in a log line while the job runs.
    pub fn path(&self) -> &Path {
        &self.out
    }
}

/// Starts making the proxy of `source`, or hands back the one already in the
/// cache. Errors are the ones that happen before a worker exists -- a file with
/// no picture in it, nowhere to put a cache -- and everything after them comes
/// back through [`Job::outcome`], so a caller has two places to look and not
/// twenty.
pub fn generate(source: &Path) -> crate::Result<Job> {
    Ok(started(source, |_| true)?.expect("nothing was filtered out"))
}

/// The same, for a file that is worth standing in for and nobody else
/// ([`wanted`]) -- `Ok(None)` is a film this machine already cuts at speed.
///
/// The header is read *once* for both questions: on a Matroska that read is the
/// cluster walk, so asking `wanted` and then `generate` separately would pay
/// for it twice.
///
/// A file with no picture in it is not a film and is answered `Ok(None)`, the
/// same answer a 1080p H.264 film gets -- it is not a failure, and reporting it
/// as one put "NO PROXY for tone.wav" in front of somebody who had imported a
/// song.
///
/// Twice, because there are two ways to know. A name this engine already reads
/// as sound or as a still is answered before anything is opened, which is the
/// cheap half. The other half is the honest one: **an audio-only `.mp4`, `.mkv`
/// or `.mov` is a video container with no video track in it**, and only its
/// header says so -- so the demuxer's own [`NoVideoTrack`] answer is taken here
/// as the same quiet `None`. A film that *has* a picture and will not open is a
/// different thing and stays loud.
pub fn generate_if_wanted(source: &Path) -> crate::Result<Option<Job>> {
    if crate::is_audio(source) || crate::is_image(source) {
        return Ok(None);
    }
    // Switched off by hand, in this session or in one last week ([`set_off`]):
    // the sweep that runs at every launch comes through here, and without this
    // it re-encodes the stand-in somebody just turned off. Answered before the
    // header is read, so a library of switched-off films costs no disk at all.
    if is_off(source) {
        return Ok(None);
    }
    match started(source, wanted) {
        Err(e) if NoVideoTrack::is_it(&e) => Ok(None),
        other => other,
    }
}

/// Both doors: the cache is looked at, the header is read once, and `only_if`
/// decides off that header whether there is anything to do.
fn started(source: &Path, only_if: fn(&VideoMeta) -> bool) -> crate::Result<Option<Job>> {
    let out = path_for(source).ok_or("no cache directory to keep a proxy in")?;
    if out.is_file() {
        return Ok(Some(Job {
            out,
            handle: None,
            _claim: None,
        }));
    }
    // One encoder per film, taken before the header is read and given back when
    // the job is dropped ([`IN_FLIGHT`]): the second start would unlink the
    // first's `.part` below and orphan an encode nothing can stop.
    let claim = claim(&out).ok_or("a stand-in for this film is already being made")?;
    // The source's own header: its rate and length are the proxy's (a stand-in
    // that ran at another rate or stopped early would put every cut on another
    // frame), and only its picture size changes.
    let (meta, _) = Demuxer::open(source)?;
    if !only_if(&meta) {
        return Ok(None);
    }
    let (width, height) = size_for(&meta);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
        // Before the write, which is the only thing that makes this directory
        // bigger -- and with room left for what is *about* to be written into
        // it: sweeping to the bare cap left the finished ones filling it and
        // then [`AT_ONCE`] encodes adding gigabytes on top, which is a cap that
        // is over by a whole film's worth by construction ([`reserve`]).
        sweep(dir, CACHE_CAP.saturating_sub(reserve(width, height, meta.frame_count)));
    }
    // What a killed editor left behind: the export worker deletes its own part
    // file when it is cancelled or fails, but nothing can delete it for a
    // process that was killed -- and a proxy of a feature film is hundreds of
    // megabytes of cache nothing would ever look at again. The key is the same,
    // so this is that same film's own leftover and not somebody else's.
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let _ = std::fs::remove_file(PathBuf::from(part));
    // One video lane, one clip, the whole file -- and *no* audio lane, which is
    // what makes the proxy picture-only: the export walk codes the lanes it is
    // given and there is no sound among them.
    let clip = Clip {
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        start: 0,
        in_frame: 0,
        out_frame: meta.frame_count.max(1),
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let project = Project::from_parts(
        vec![Source::new(source, 0)],
        vec![(LaneKind::Video, vec![clip])],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )?;
    let bitrate =
        (f64::from(width) * f64::from(height) * meta.frame_rate * BITS_PER_PIXEL) as u64;
    let handle = crate::export::start(
        project,
        VideoMeta {
            width,
            height,
            ..meta
        },
        &out,
        &ExportSettings {
            format: Format::Mp4,
            // The whole point: every frame its own random-access point.
            intra_only: true,
            // ...and the film's own colour, carried rather than converted: the
            // screen tone-maps the stand-in exactly as it tone-maps the film,
            // live and at preview size.
            keep_source_colour: true,
            bitrate: Some(bitrate),
            ..Default::default()
        },
        // A stand-in keeps its film's own rate: the override is the
        // timeline's affair, not the proxy's.
        None,
    );
    Ok(Some(Job {
        out,
        handle: Some(handle),
        _claim: Some(claim),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::colorspace::ColorDescription;

    fn meta(width: u32, height: u32, codec: Codec) -> VideoMeta {
        VideoMeta {
            width,
            height,
            frame_rate: 24.0,
            frame_count: 240,
            codec,
            color: ColorDescription::default(),
        }
    }

    /// What a proxy is *for*: the files this machine cannot play at speed.
    #[test]
    fn only_the_files_that_need_one_get_one() {
        assert!(wanted(&meta(3840, 2160, Codec::Hevc)), "4K HEVC");
        assert!(wanted(&meta(3840, 2160, Codec::H264)), "4K H.264");
        assert!(wanted(&meta(1920, 1080, Codec::Hevc)), "1080p HEVC");
        assert!(wanted(&meta(1920, 1080, Codec::Av1)), "1080p AV1");
        assert!(
            !wanted(&meta(1920, 1080, Codec::H264)),
            "1080p H.264 plays natively"
        );
        assert!(!wanted(&meta(1280, 720, Codec::H264)), "720p H.264");
    }

    /// Aspect kept, long edge capped, both axes even -- and a small source left
    /// alone rather than blown up.
    #[test]
    fn a_proxy_is_the_same_shape_at_a_smaller_size() {
        assert_eq!(size_for(&meta(3840, 2160, Codec::Hevc)), (1280, 720));
        assert_eq!(size_for(&meta(1920, 1080, Codec::Hevc)), (1280, 720));
        // A portrait source is capped on *its* long edge, which is the height.
        assert_eq!(size_for(&meta(1080, 1920, Codec::Hevc)), (720, 1280));
        // 1916x1080 (his own film's cropped width): even both ways, and the
        // aspect it really has rather than the 16:9 it nearly has.
        let (w, h) = size_for(&meta(1916, 1080, Codec::H264));
        assert_eq!((w, h), (1280, 722), "cropped-width 1080p");
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
        assert!(
            (f64::from(w) / f64::from(h) - 1916. / 1080.).abs() < 0.005,
            "{w}x{h} is the source's shape"
        );
        assert_eq!(
            size_for(&meta(640, 360, Codec::H264)),
            (640, 360),
            "already small: its own size, never upscaled"
        );
        assert_eq!(size_for(&meta(1, 1, Codec::H264)), (2, 2), "never zero");
    }

    /// What the sweep leaves free for the encodes that are about to run: two
    /// of them, at what this one is really going to cost. Without it the cap
    /// was soft by a whole film per encode -- 20 GB of finished stand-ins and
    /// then two 4K ones writing gigabytes on top of it.
    #[test]
    fn the_cap_keeps_room_for_what_is_about_to_be_written() {
        // A 2h20 4K film, at the size a stand-in of it is coded: 1280x536 for
        // 201536 frames. Its own arithmetic says ~2.6 GB, and two of them is
        // over the cap's half, so the reserve is that half.
        let big = reserve(1280, 536, 201_536);
        assert_eq!(big, CACHE_CAP / 2, "a feature film asks for more than half");
        // A short clip asks for what it costs and no more: 1280x720 for 300
        // frames is 1280*720*300*0.3/8 * 1.25 * 2 encodes.
        let small = reserve(1280, 720, 300);
        let one = 1280.0 * 720.0 * 300.0 * BITS_PER_PIXEL / 8.0 * RATE_OVERSHOOT;
        assert_eq!(small, one as u64 * AT_ONCE as u64);
        assert!(small < CACHE_CAP / 2, "and it is nowhere near the ceiling");
        // The reserve is what a sweep aims below, so what is *finished* is held
        // under the cap by at least the room the encodes will take.
        assert!(CACHE_CAP.saturating_sub(big) <= CACHE_CAP / 2);
        // Nothing overflows on a picture nobody would code.
        assert_eq!(reserve(0, 0, 0), 0);
        assert_eq!(reserve(u32::MAX, u32::MAX, u32::MAX), CACHE_CAP / 2);
    }

    /// The cap keeps the directory a cache and not a second copy of his
    /// library: over it, the least recently used go until it fits, and a
    /// half-written one is never taken (it is being written *now*) though its
    /// bytes count towards what has to be freed.
    #[test]
    fn the_cache_evicts_its_oldest_until_it_fits() {
        let dir = crate::scratch::Scratch::dir("proxy-sweep");
        let write = |name: &str, len: usize, ago: u64| {
            let path = dir.join(name);
            std::fs::write(&path, vec![0u8; len]).expect("write");
            // Stamped by hand: three files written in one millisecond have no
            // order of their own, and the order is what is under test. `atime`
            // is what `sweep` reads and `set_times` is the one door to it
            // without a dependency.
            let file = std::fs::File::options().write(true).open(&path).expect("open");
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(ago);
            file.set_times(std::fs::FileTimes::new().set_accessed(when).set_modified(when))
                .expect("stamp");
            path
        };
        let old = write("old.mp4", 400, 300);
        let middle = write("middle.mp4", 400, 200);
        let fresh = write("fresh.mp4", 400, 100);
        let part = write("busy.mp4.part", 400, 400);

        // 1600 bytes there, 900 allowed: the oldest two `.mp4`s go, and that is
        // as far as it gets -- the part file is 400 of the remaining bytes and
        // is not the sweep's to take.
        let left = sweep(&dir, 900);
        assert!(!old.exists(), "the oldest stand-in survived the sweep");
        assert!(!middle.exists(), "the next oldest survived it");
        assert!(fresh.exists(), "the freshest was taken");
        assert!(part.exists(), "a half-written proxy was deleted under its writer");
        assert_eq!(left, 800, "what is left is the fresh one and the part file");

        // Under the cap, nothing is touched at all.
        assert_eq!(sweep(&dir, 900), 800);
        assert!(fresh.exists());
    }

    /// A song and a still are not films: they are answered `None` -- the same
    /// answer a 1080p H.264 film gets -- rather than opened, failed on and
    /// reported. The bug this pins said "NO PROXY for tone.wav" to a person who
    /// had imported a song, and put "no proxy" on its library row.
    ///
    /// Real files, because the point is that nothing reads them: a demuxer
    /// opened on a WAV is exactly what used to raise the refusal.
    ///
    /// ...and one **audio-only mp4**, which is the same class arriving the
    /// other way: its name says video, so no extension can answer it and the
    /// header is what tells. That one is a real, well-formed mp4 with an AAC
    /// track and no video track at all -- a thing a person really does hand an
    /// editor, and what a name-shaped gate walks straight past.
    #[test]
    fn a_song_and_a_still_are_never_stood_in_for() {
        let dir = crate::scratch::Scratch::dir("proxy-not-a-film");
        for name in ["tone.wav", "song.mp3", "track.mka", "still.png", "shot.jpg"] {
            let file = dir.join(name);
            std::fs::write(&file, b"not a film, and never read").expect("write");
            let answer = generate_if_wanted(&file);
            assert!(
                matches!(answer, Ok(None)),
                "{name} is not a film to stand in for: {:?}",
                answer.err().map(|e| e.to_string())
            );
            assert_eq!(cached(&file), None, "{name} left nothing in the cache");
        }

        let silent_film = dir.join("audio-only.mp4");
        write_audio_only_mp4(&silent_film);
        // It really is an mp4 nothing here finds a picture in -- the demuxer
        // says so by name, which is what makes the answer below a *class* and
        // not a guess about the extension.
        let opened = Demuxer::open(&silent_film);
        let refusal = opened.err().expect("an mp4 with no picture will not open");
        assert!(
            crate::demux::NoVideoTrack::is_it(&refusal),
            "not the no-picture answer: {refusal}"
        );
        let answer = generate_if_wanted(&silent_film);
        assert!(
            matches!(answer, Ok(None)),
            "an audio-only mp4 is not a film to stand in for: {:?}",
            answer.err().map(|e| e.to_string())
        );
        assert_eq!(cached(&silent_film), None);
    }

    /// A well-formed mp4 carrying one AAC track and no video track: what an
    /// `ffmpeg -vn` or a music file in the wrong box looks like. Written with
    /// the same crate the exporter's muxer uses, so it is the file that reader
    /// really parses and not a hand-made approximation.
    fn write_audio_only_mp4(path: &Path) {
        use mp4::{
            AacConfig, AudioObjectType, ChannelConfig, FourCC, MediaConfig, Mp4Config, Mp4Writer,
            SampleFreqIndex, TrackConfig, TrackType,
        };
        let mut writer = Mp4Writer::write_start(
            std::io::BufWriter::new(std::fs::File::create(path).expect("create")),
            &Mp4Config {
                major_brand: FourCC::from(*b"isom"),
                minor_version: 512,
                compatible_brands: vec![FourCC::from(*b"isom"), FourCC::from(*b"mp41")],
                timescale: 44_100,
            },
        )
        .expect("start an mp4");
        writer
            .add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: 44_100,
                language: "und".to_string(),
                media_conf: MediaConfig::AacConfig(AacConfig {
                    bitrate: 0,
                    profile: AudioObjectType::AacLowComplexity,
                    freq_index: SampleFreqIndex::Freq44100,
                    chan_conf: ChannelConfig::Stereo,
                }),
            })
            .expect("an audio track");
        writer.write_end().expect("finish the mp4");
    }

    /// The staleness rule *is* the name: touch the source and the old proxy is
    /// unreachable rather than wrong.
    #[test]
    fn a_changed_source_names_another_proxy() {
        let dir = crate::scratch::Scratch::dir("proxy-key");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"one").expect("write");
        let first = path_for(&file).expect("a key");
        assert_eq!(path_for(&file).as_ref(), Some(&first), "stable for a file");
        // Same length, later time: a re-encode that happened to land on the
        // same byte count is still another file.
        std::fs::write(&file, b"two").expect("rewrite");
        filetime_bump(&file);
        assert_ne!(path_for(&file), Some(first), "touched: another name");
        assert_eq!(cached(&file), None, "nothing written, nothing cached");
    }

    /// Turning one off deletes the stand-in and *only* the stand-in: the film
    /// it was made from is what plays afterwards, so it had better still be
    /// there.
    #[test]
    fn delete_takes_the_proxy_and_never_the_source() {
        let dir = crate::scratch::Scratch::dir("proxy-delete");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"the film itself").expect("write");
        let proxy = path_for(&file).expect("a key");
        std::fs::create_dir_all(proxy.parent().expect("a cache dir")).expect("cache dir");
        std::fs::write(&proxy, b"a stand-in").expect("write the proxy");
        assert_eq!(cached(&file).as_ref(), Some(&proxy), "one in the cache");
        assert_eq!(delete(&file).ok(), Some(true), "the stand-in went");
        assert_eq!(cached(&file), None, "and is gone from the cache");
        assert!(file.is_file(), "the film itself is untouched");
        assert_eq!(delete(&file).ok(), Some(false), "nothing left to take");
        assert!(file.is_file(), "still untouched");
    }

    /// A delete that *failed* is not a delete: the row above it draws itself
    /// from this answer, and folding "could not" into "was not there" drew a
    /// switch as OFF over a proxy still on the disk and still being played.
    #[test]
    fn a_delete_that_could_not_happen_says_so() {
        let dir = crate::scratch::Scratch::dir("proxy-delete-refused");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"the film itself").expect("write");
        let proxy = path_for(&file).expect("a key");
        std::fs::create_dir_all(&proxy).expect("something an unlink will refuse");
        // An unlink that fails for anything but "it was not there" -- a
        // read-only cache directory in the field, a directory in this test --
        // is a stand-in still on the disk, and it is answered as one.
        let answer = delete(&file);
        assert!(answer.is_err(), "a refused unlink reported as a delete");
        assert!(proxy.is_dir(), "and what could not be deleted is still there");
        std::fs::remove_dir(&proxy).expect("clean up");
        // ...against the honest no-op, which *is* an off switch: nothing there.
        assert_eq!(delete(&file).ok(), Some(false), "nothing to take");
        assert!(file.is_file(), "the film itself, throughout");
    }

    /// The switch turned off outlives the window: the mark sits beside the
    /// cache entry, so the sweep at the next launch answers `None` for that
    /// film instead of re-encoding the stand-in somebody just deleted.
    #[test]
    fn a_film_switched_off_by_hand_stays_off() {
        let dir = crate::scratch::Scratch::dir("proxy-off-mark");
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"the film itself").expect("write");
        assert!(!is_off(&file), "nothing has been switched off yet");
        set_off(&file, true);
        assert!(is_off(&file), "the mark is beside the cache entry");
        // Which is what the sweep asks -- and it does not even read the header.
        assert!(
            matches!(generate_if_wanted(&file), Ok(None)),
            "a switched-off film was started again"
        );
        set_off(&file, false);
        assert!(!is_off(&file), "switched back on, the mark is gone");
        // The mark is keyed like the stand-in: a source that changed is a
        // different film and is asked again.
        set_off(&file, true);
        // Taken back by hand before the key moves: the scratch directory takes
        // the film with it, and a mark left in the real cache would outlive the
        // test that wrote it.
        let stale = off_marker(&file).expect("a key");
        std::fs::write(&file, b"re-encoded").expect("rewrite");
        filetime_bump(&file);
        assert!(!is_off(&file), "a changed source kept the old answer");
        std::fs::remove_file(stale).expect("the old mark");
    }

    /// One encoder per film, in the engine and not only in the window that
    /// draws the switch: the seat is taken for as long as the job lives, and a
    /// second start for the same stand-in is refused rather than allowed to
    /// unlink the first one's half-written file.
    #[test]
    fn one_encoder_per_stand_in() {
        let dir = crate::scratch::Scratch::dir("proxy-one-encoder");
        let out = dir.join("f00d.mp4");
        let first = claim(&out).expect("the seat was free");
        assert!(claim(&out).is_none(), "two encoders on one path");
        // Another film is another seat.
        let other = claim(&dir.join("beef.mp4")).expect("a different stand-in");
        drop(first);
        let again = claim(&out).expect("the seat came back with the job");
        drop((again, other));
        assert!(claim(&out).is_some(), "the seat was never given back");
    }

    /// `std::fs` has no set-mtime, and a rewrite inside the same nanosecond is
    /// possible on a fast machine -- so the stamp is moved by hand through the
    /// one door that does it without a dependency.
    fn filetime_bump(path: &Path) {
        let now = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open");
        file.set_modified(now).expect("set mtime");
    }
}
