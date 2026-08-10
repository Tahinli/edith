//! Export: the edit list rendered back out as one mp4.
//!
//! Video is fully re-encoded — a cut lands mid-GOP, so stream-copying across it
//! is impossible — while audio is copied packet for packet, because there is no
//! pure-Rust AAC encoder to re-encode with. The worker owns everything: the
//! caller gets an [`ExportHandle`] and polls it from its render loop.
//!
//! Nothing partial survives a failure: the worker writes to `<out>.part` and
//! renames it onto `out` only once the file is closed and complete, so the
//! output either does not exist or is finished — there is no window where a
//! half-written `.export.mp4` is sitting there looking playable. Cancel and
//! every error path delete the `.part`. A killed *process* leaves the `.part`
//! behind (only in-process cleanup is promised), which is an orphan a user can
//! delete rather than a file that plays for two seconds and stops.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use rusty_h264::{Decoder, Encoder, EncoderConfig, Preset, YuvFrame};

use crate::audio::AudioSession;
use crate::demux::{Demuxer, VideoMeta};
use crate::hw::{HwEncoder, HwSession};
use crate::mux::{AudioParams, Mp4Muxer, VideoParams};
use crate::project::{Lane, Project};

/// Progress is reported in permille: an atomic integer the render loop can read
/// without a lock, fine enough for any progress bar.
const PROGRESS_SCALE: u32 = 1_000;

/// D2 rate control: bits per pixel per second, then a sane range. 720p30 lands
/// at 2.76 Mbps, which S1 measured the software encoder hitting within 1%.
const BITS_PER_PIXEL: f64 = 0.1;
const MIN_BITRATE: u64 = 1_000_000;
const MAX_BITRATE: u64 = 20_000_000;

/// What the caller gets to decide about the output. The codec and container are
/// not among it: H.264 in mp4 is the only pair with both an encoder and a
/// decoder under this project's no-install rule, so offering a choice would be
/// offering files we cannot read back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportSettings {
    /// Bits per second, clamped to the same sane range the automatic value uses.
    /// `None` picks it from the picture size and frame rate.
    pub bitrate: Option<u64>,
    /// Skip the hardware encoder even where it is available -- an escape hatch
    /// for a driver that encodes badly, matching the `VE_SW_ENC` env pin.
    pub force_sw: bool,
}

struct Shared {
    progress: AtomicU32,
    cancel: AtomicBool,
    finished: AtomicBool,
    outcome: Mutex<Option<crate::Result<()>>>,
}

/// A running export. Poll [`is_finished`](ExportHandle::is_finished) once per
/// rendered frame and take the [`result`](ExportHandle::result) when it flips;
/// dropping the handle does *not* stop the worker, [`cancel`](ExportHandle::cancel)
/// does.
pub struct ExportHandle {
    shared: Arc<Shared>,
}

impl ExportHandle {
    /// Fraction of the timeline written, `0.0..=1.0`.
    pub fn progress(&self) -> f32 {
        self.shared.progress.load(Ordering::Relaxed) as f32 / PROGRESS_SCALE as f32
    }

    /// Asks the worker to stop at its next checkpoint and delete the partial
    /// file. The outcome then reports the cancellation as an error. Checkpoints
    /// run to the last instant before the rename, so even a cancel at full
    /// progress leaves no output.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.shared.finished.load(Ordering::Acquire)
    }

    /// The outcome, once — taken out of the handle, so a caller that already
    /// reported it sees `None` afterwards. `None` while the export is running.
    pub fn result(&self) -> Option<crate::Result<()>> {
        self.shared.outcome.lock().unwrap().take()
    }
}

/// Starts the worker. Failures are reported through the handle rather than
/// returned, so a caller has exactly one place to look. The files to read are
/// the project's own [`sources`](Project::sources) -- every clip names one --
/// so nothing but the edit list decides what is decoded.
pub fn start(
    project: Project,
    meta: VideoMeta,
    out: &Path,
    settings: &ExportSettings,
) -> ExportHandle {
    let settings = *settings;
    let shared = Arc::new(Shared {
        progress: AtomicU32::new(0),
        cancel: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        outcome: Mutex::new(None),
    });
    let worker = Arc::clone(&shared);
    let out = out.to_path_buf();
    // `<out>.part`, appended rather than substituted: the temporary of
    // `a.export.mp4` is `a.export.mp4.part`, which no other export claims.
    let mut part = out.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let spawned = thread::Builder::new().name("export".into()).spawn(move || {
        // The rename is the last step and the only one that publishes a file
        // under the name the caller asked for; it stays on the same directory,
        // so it is atomic.
        let result = run(&project, &meta, &part, &worker, &settings)
            .and_then(|()| std::fs::rename(&part, &out).map_err(Into::into));
        if result.is_err() {
            // The muxer -- and with it the file handle -- died with `run`.
            let _ = std::fs::remove_file(&part);
        }
        settle(&worker, result);
    });
    if let Err(e) = spawned {
        settle(&shared, Err(e.into()));
    }
    ExportHandle { shared }
}

fn settle(shared: &Shared, result: crate::Result<()>) {
    *shared.outcome.lock().unwrap() = Some(result);
    // Published last: a caller that sees the flag is guaranteed the outcome.
    shared.finished.store(true, Ordering::Release);
}

fn run(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Shared,
    settings: &ExportSettings,
) -> crate::Result<()> {
    let total = project.timeline_frames();
    let sources = project.sources();
    // Audio first: a track has to be declared when the muxer is created, which
    // happens as soon as the first coded picture arrives.
    //
    // ponytail: this holds the whole exported AAC track in memory (~3 kB per
    // 23 ms packet, so ~500 MB for an hour). Upgrade path is a streaming
    // `copy_segments` that yields packets instead of collecting them.
    //
    // The segments name their source, and the copy carries its packet-rounding
    // debt across a source join exactly as across a cut, so a timeline spanning
    // files stays in sync. A source whose AAC parameters disagree with the
    // first one is an `Err` from there -- import refuses those up front, this
    // is the backstop -- and the caller deletes the `.part`.
    //
    // The stream each source is copied from is the one it plays: `audio_sources`
    // is the same list `PlaybackSession::seek` hands the decoder, so an export
    // of a timeline playing a file's second audio track carries *that* track.
    let audio = AudioSession::copy_multi_streams(
        &project.audio_sources(),
        &project.segments_from(0, meta.frame_rate),
    )?;
    let audio_params = audio.as_ref().map(|(track, _)| AudioParams {
        freq_index: track.freq_index,
        chan_conf: track.chan_conf,
    });

    let mut encoder = Enc::open(meta, settings)?;
    let mut muxer = None;
    let mut done = 0u32;
    let black = Black::new(meta);
    // Spans, not clips: a gap in the video lane is part of the timeline and
    // gets encoded too, as black frames. The picture count is therefore
    // `timeline_frames` however the lane is arranged.
    for span in project.spans_from(Lane::Video, 0) {
        // Every clip reopens its own source file at its own in point; the
        // encoder is *not* reopened, so the export is one continuous stream
        // whose GOP boundaries need not line up with the cuts -- nor with the
        // file boundaries, which are just cuts that change the path.
        let mut pictures = match span.from {
            Some((source, in_frame)) => {
                let entry = sources
                    .get(source)
                    .ok_or_else(|| format!("clip names source {source} of {}", sources.len()))?;
                Some(ClipDecoder::open(&entry.path, in_frame)?)
            }
            None => None,
        };
        for _ in 0..span.len {
            cancelled(shared)?;
            let picture = match &mut pictures {
                Some(pictures) => pictures.next()?,
                None => Some(black.picture()),
            };
            let Some((y, u, v, width, height)) = picture else {
                break; // source ran out early; the clip list outlives the file
            };
            if let Some(au) = encoder.encode(y, u, v, width, height)? {
                write_video(&mut muxer, out, meta, audio_params.as_ref(), au)?;
            }
            done += 1;
            shared
                .progress
                .store(done * PROGRESS_SCALE / total.max(1), Ordering::Relaxed);
        }
    }
    while let Some(au) = encoder.drain()? {
        write_video(&mut muxer, out, meta, audio_params.as_ref(), au)?;
    }
    // Progress reads 100% from here on, but nothing is published yet: draining,
    // the audio pass and `finish` are all still cancellable.
    cancelled(shared)?;

    let Some(mut muxer) = muxer else {
        return Err("export produced no coded pictures".into());
    };
    if let Some((_, packets)) = audio {
        for packet in packets {
            muxer.write_audio_packet(&packet.bytes)?;
        }
    }
    cancelled(shared)?;
    muxer.finish()?;
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// The I420 planes of one black picture, allocated once for a whole export: a
/// gap in the video lane is encoded, not skipped, or every frame after it would
/// arrive early. Limited-range black is `Y=16, U=V=128`, the same convention
/// [`crate::convert`] decodes with.
struct Black {
    y: Vec<u8>,
    uv: Vec<u8>,
    width: u32,
    height: u32,
}

impl Black {
    fn new(meta: &VideoMeta) -> Self {
        let (w, h) = (meta.width as usize, meta.height as usize);
        Self {
            y: vec![16; w * h],
            uv: vec![128; w.div_ceil(2) * h.div_ceil(2)],
            width: meta.width,
            height: meta.height,
        }
    }

    fn picture(&self) -> (&[u8], &[u8], &[u8], u32, u32) {
        (&self.y, &self.uv, &self.uv, self.width, self.height)
    }
}

/// `Err` once a cancel has been asked for. Called at every point where the work
/// left is more than an instant, so an `esc` at 99.9% still stops the export
/// instead of quietly completing it.
fn cancelled(shared: &Shared) -> crate::Result<()> {
    if shared.cancel.load(Ordering::Relaxed) {
        return Err("export cancelled".into());
    }
    Ok(())
}

/// Writes one access unit, creating the file on the first one -- the parameter
/// sets for `avcC` only exist once the encoder has coded something. Units with
/// no coded slice are skipped: a software encoder may hand back an empty buffer
/// while it buffers, and the muxer rejects a sample that would carry no picture.
fn write_video(
    muxer: &mut Option<Mp4Muxer>,
    out: &Path,
    meta: &VideoMeta,
    audio: Option<&AudioParams>,
    au: &[u8],
) -> crate::Result<()> {
    if !crate::mux::has_coded_slice(au) {
        return Ok(());
    }
    let muxer = match muxer {
        Some(muxer) => muxer,
        none => {
            let (sps, pps) = crate::mux::parameter_sets(au)
                .ok_or("the first coded picture carries no SPS/PPS")?;
            none.insert(Mp4Muxer::create(
                out,
                &VideoParams {
                    width: meta.width,
                    height: meta.height,
                    frame_rate: meta.frame_rate,
                    sps,
                    pps,
                },
                audio,
            )?)
        }
    };
    muxer.write_video_au(au)
}

fn bitrate_for(meta: &VideoMeta) -> u64 {
    let raw = f64::from(meta.width) * f64::from(meta.height) * meta.frame_rate * BITS_PER_PIXEL;
    (raw as u64).clamp(MIN_BITRATE, MAX_BITRATE)
}

fn forced(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|v| v == "1")
}

/// Hardware where it is available, software everywhere else -- chosen once for
/// the whole export, so the stream never changes encoder mid-file.
enum Enc {
    Hw(HwEncoder),
    Sw {
        encoder: Encoder,
        /// The last access unit; owned because `rusty_h264` hands back a `Vec`
        /// while the plugin lends a slice, and the two have to look alike here.
        au: Vec<u8>,
        flushed: bool,
    },
}

impl Enc {
    fn open(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Self> {
        // A caller's number goes through the same clamp as the computed one: a
        // zero bitrate switches the software encoder's lookahead on, which would
        // break the one-picture-per-call contract `encode` documents below.
        let bitrate = settings
            .bitrate
            .map_or_else(|| bitrate_for(meta), |b| b.clamp(MIN_BITRATE, MAX_BITRATE));
        // The plugin wants an exact rational, and the muxer already picks one
        // that is exact at every rate we can read -- `fps * 1000 / 1000` would
        // hand 24000/1001 over as a rounded 23.976, which is the same
        // truncation the container timing had.
        let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate)?;
        if !settings.force_sw
            && !forced("VE_SW_ENC")
            && let Some(hw) = HwEncoder::open(meta.width, meta.height, fps_num, fps_den, bitrate)
        {
            eprintln!("export encoder: hardware (VA-API plugin)");
            return Ok(Self::Hw(hw));
        }
        eprintln!("export encoder: software (rusty_h264)");
        let mut cfg = EncoderConfig::new(meta.width as usize, meta.height as usize);
        cfg.framerate = meta.frame_rate as f32;
        cfg.bitrate = bitrate.min(u32::MAX as u64) as u32;
        // Two seconds between key frames, and no B-frames on either path: the
        // muxer times everything by duration alone, which reordering would break.
        cfg.gop_size = (meta.frame_rate * 2.0).round().max(1.0) as u32;
        cfg.bframes = 0;
        // S1 measured Fast at 1.30x realtime and Balanced at 0.46x for the same
        // bitrate, so Fast is what a fallback should be.
        cfg.preset = Preset::Fast;
        let encoder = Encoder::new(cfg).map_err(|e| format!("software encoder: {e}"))?;
        Ok(Self::Sw {
            encoder,
            au: Vec::new(),
            flushed: false,
        })
    }

    /// One picture in, at most one access unit out.
    ///
    /// ponytail: `rusty_h264` buffers a whole GOP and returns it in one buffer
    /// when its lookahead is active, which would make this "one access unit"
    /// a lie and every sample duration with it. It is inactive here because
    /// lookahead needs a zero bitrate and this path is always CBR; a future
    /// constant-QP mode has to split the buffer per access unit first.
    fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
    ) -> crate::Result<Option<&[u8]>> {
        match self {
            Self::Hw(hw) => hw.encode(y, u, v, width, height, false),
            Self::Sw { encoder, au, .. } => {
                let frame = YuvFrame {
                    width: width as usize,
                    height: height as usize,
                    y: y.to_vec(),
                    u: u.to_vec(),
                    v: v.to_vec(),
                };
                *au = encoder
                    .try_encode(&frame)
                    .map_err(|e| format!("software encode: {e}"))?;
                Ok(Some(&au[..]).filter(|au| !au.is_empty()))
            }
        }
    }

    /// End of stream; call until it returns `None`.
    fn drain(&mut self) -> crate::Result<Option<&[u8]>> {
        match self {
            Self::Hw(hw) => hw.drain(),
            Self::Sw {
                encoder,
                au,
                flushed,
            } => {
                if *flushed {
                    return Ok(None);
                }
                *flushed = true;
                *au = encoder
                    .try_flush()
                    .map_err(|e| format!("software encoder flush: {e}"))?;
                Ok(Some(&au[..]).filter(|au| !au.is_empty()))
            }
        }
    }
}

/// I420 straight out of the decoder: the export never converts to BGRA and back,
/// which would cost two conversions and a generation of colour precision.
///
/// This mirrors `decode`'s two worker loops rather than reusing `DecodeSession`,
/// which only speaks BGRA. Unlike playback there is no mid-clip fallback to
/// software: a hardware decode that fails after the first picture fails the
/// export, which then deletes the half-written file.
enum ClipDecoder {
    Hw(HwSession),
    Sw(SwDecoder),
}

impl ClipDecoder {
    fn open(path: &Path, start_frame: u32) -> crate::Result<Self> {
        if !forced("VE_SW")
            && let Some(hw) = HwSession::open_at(path, start_frame)
        {
            return Ok(Self::Hw(hw));
        }
        Ok(Self::Sw(SwDecoder::open(path, start_frame)?))
    }

    /// The next picture as tightly packed I420, borrowed until the call after.
    fn next(&mut self) -> crate::Result<Option<(&[u8], &[u8], &[u8], u32, u32)>> {
        match self {
            Self::Hw(hw) => hw.next_frame(),
            Self::Sw(sw) => {
                if !sw.advance()? {
                    return Ok(None);
                }
                let frame = sw.frame.as_ref().expect("advance stored a picture");
                Ok(Some((
                    &frame.y,
                    &frame.u,
                    &frame.v,
                    frame.width as u32,
                    frame.height as u32,
                )))
            }
        }
    }
}

struct SwDecoder {
    demuxer: Demuxer,
    decoder: Decoder,
    /// Display index of the next picture the decoder will produce. Signed: a
    /// sync sample inside what the edit list trims is before frame 0.
    index: i64,
    start: u32,
    frame: Option<YuvFrame>,
}

impl SwDecoder {
    fn open(path: &Path, start_frame: u32) -> crate::Result<Self> {
        let (_, mut demuxer) = Demuxer::open(path)?;
        // Decoding restarts at a sync sample, so pictures between it and the in
        // point are decoded (the target references them) and then dropped.
        let index = demuxer.seek_to_sync_at_or_before(start_frame);
        Ok(Self {
            demuxer,
            decoder: Decoder::new(),
            index,
            start: start_frame,
            frame: None,
        })
    }

    /// Decodes up to and including the next picture at or after the in point,
    /// leaving it in `frame`. `false` at end of stream.
    fn advance(&mut self) -> crate::Result<bool> {
        loop {
            let Some(au) = self.demuxer.next_access_unit()? else {
                return Ok(false);
            };
            let decoded = self
                .decoder
                .decode(&au)
                .map_err(|e| format!("decode at picture {}: {e}", self.index))?;
            let Some(yuv) = decoded else { continue };
            let wanted = self.index >= i64::from(self.start);
            self.index += 1;
            if wanted {
                self.frame = Some(yuv);
                return Ok(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrate_clamps_at_both_ends() {
        let meta = |width, height, frame_rate| VideoMeta {
            width,
            height,
            frame_rate,
            frame_count: 1,
        };
        // 1280 * 720 * 30 * 0.1
        assert_eq!(bitrate_for(&meta(1280, 720, 30.0)), 2_764_800);
        assert_eq!(bitrate_for(&meta(320, 240, 30.0)), MIN_BITRATE, "tiny");
        assert_eq!(bitrate_for(&meta(3840, 2160, 60.0)), MAX_BITRATE, "huge");
    }
}
