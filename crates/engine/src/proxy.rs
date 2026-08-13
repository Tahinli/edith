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
//!   found. Deleting the cache directory costs time and nothing else.
//!
//! Generation is [`crate::export`]'s own decode -> scale -> encode walk, given
//! a one-clip project and a smaller canvas: the hardware seat where the GPU has
//! one, the software encoder where it does not, the same progress and the same
//! cancel a file export has ([`Job`]).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::demux::{Codec, Demuxer, VideoMeta};
use crate::export::{ExportHandle, ExportSettings, Format};
use crate::project::{Clip, LaneKind, Project, Source, Speed};
use crate::scale::FitPolicy;

/// The long edge of a proxy picture, in samples: 1280, so a 4K source loses
/// nine tenths of its samples and a 1080p one a little over half. Chosen as
/// what a cut can be judged on at editing size, in the neighbourhood of every
/// shipped proxy preset (Resolve's half-res, kdenlive's 640/1000-wide
/// defaults).
pub const LONG_EDGE: u32 = 1280;

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
/// corner-cut: nothing evicts, exactly as the Matroska index cache beside it
/// states. A proxy of a feature film is hundreds of megabytes and a re-encoded
/// source leaves its old one behind; the upgrade path is a size-capped sweep of
/// the directory at open. It is the user's own cache to delete meanwhile.
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

/// The proxy of `source` if one has already been made, `None` otherwise. The
/// answer a session asks at open: a hit costs one `stat`.
pub fn cached(source: &Path) -> Option<PathBuf> {
    path_for(source).filter(|p| p.is_file())
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
pub fn generate_if_wanted(source: &Path) -> crate::Result<Option<Job>> {
    started(source, wanted)
}

/// Both doors: the cache is looked at, the header is read once, and `only_if`
/// decides off that header whether there is anything to do.
fn started(source: &Path, only_if: fn(&VideoMeta) -> bool) -> crate::Result<Option<Job>> {
    let out = path_for(source).ok_or("no cache directory to keep a proxy in")?;
    if out.is_file() {
        return Ok(Some(Job { out, handle: None }));
    }
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
        start: 0,
        in_frame: 0,
        out_frame: meta.frame_count.max(1),
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let project = Project::from_parts(
        vec![Source::new(source, 0)],
        vec![(LaneKind::Video, vec![clip])],
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
    );
    Ok(Some(Job {
        out,
        handle: Some(handle),
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
