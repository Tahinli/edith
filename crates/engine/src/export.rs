//! Export: the edit list rendered back out as one mp4 or one AV1-in-Matroska —
//! or, picture left behind, as one WAV or FLAC of the timeline's audio alone.
//!
//! Video is fully re-encoded — a cut lands mid-GOP, so stream-copying across it
//! is impossible — while audio is copied packet for packet, because there is no
//! pure-Rust AAC encoder to re-encode with. The audio-only formats have no such
//! trouble: `hound` writes PCM and `flacenc` encodes FLAC, both pure Rust, so
//! those are *decoded* out of the timeline instead of copied. The worker owns
//! everything: the caller gets an [`ExportHandle`] and polls it from its render
//! loop.
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

use flacenc::component::BitRepr;
use flacenc::error::Verify;

use crate::audio::{AudioMeta, AudioSession};
use crate::demux::{Demuxer, VideoMeta};
use crate::hw::{HwEncoder, HwSession};
use crate::mux::{AudioParams, Av1Params, MkvMuxer, Mp4Muxer, VideoParams};
use crate::project::{LaneKind, Project};
use crate::scale::Composer;

/// Progress is reported in permille: an atomic integer the render loop can read
/// without a lock, fine enough for any progress bar.
const PROGRESS_SCALE: u32 = 1_000;

/// D2 rate control: bits per pixel per second, then a sane range. 720p30 lands
/// at 2.76 Mbps, which S1 measured the software encoder hitting within 1%.
const BITS_PER_PIXEL: f64 = 0.1;
const MIN_BITRATE: u64 = 1_000_000;
const MAX_BITRATE: u64 = 20_000_000;

/// `rav1e`'s fastest preset. Anything slower is minutes per second of timeline
/// on a build with no assembly, which is what this one is.
const AV1_SPEED: u8 = 10;

/// What an export writes. Four, not more: H.264-in-mp4 and AV1-in-Matroska are
/// the only *video* pairs with both an encoder and a decoder under this
/// project's no-install rule, and WAV and FLAC are the only audio formats with
/// a pure-Rust encoder at all. MP3 has one (`shine-rs`) under LGPL-2.0, which is
/// a licensing decision this project has not taken; Vorbis, Opus and AAC have
/// none. HEVC and VP9 have no encoder here at all (`hevc`/`vp9` import through
/// the plugin and stop there). A front-end says so rather than hiding the rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Picture and sound: video re-encoded, AAC copied. Copied, so it carries
    /// **one** audio lane: a timeline whose sound is spread over two is refused
    /// by name here and mixed by the other two formats, which decode.
    #[default]
    Mp4,
    /// Picture alone, AV1 in Matroska. Alone because Matroska carries no sound
    /// this project can write: there is no AAC or Opus encoder here, and an AAC
    /// track copied in would be one *this engine's own reader* leaves silent
    /// (`audio::AudioSession::open` refuses Matroska audio). The sound of an AV1
    /// export is a WAV or FLAC beside it, which a front-end says up front.
    Av1,
    /// The audio lane alone, 16-bit PCM.
    Wav,
    /// The audio lane alone, losslessly compressed.
    Flac,
}

impl Format {
    /// The extension a file of this format is named with -- a front-end builds
    /// the destination path from it, so the name never disagrees with the bytes.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Av1 => "mkv",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }

    /// Whether this format carries the picture. The bitrate settings are video
    /// settings and mean nothing to the audio two.
    pub fn has_video(self) -> bool {
        matches!(self, Self::Mp4 | Self::Av1)
    }

    /// What the format is called where a refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "an mp4",
            Self::Av1 => "an AV1 export",
            Self::Wav => "a WAV",
            Self::Flac => "a FLAC",
        }
    }
}

/// What the caller gets to decide about the output. The codec is not among it:
/// [`Format`] names a container *and* what goes in it, because every pair we
/// can write is a pair we can also read back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExportSettings {
    /// Bits per second, clamped to the same sane range the automatic value uses.
    /// `None` picks it from the picture size and frame rate. Video only.
    pub bitrate: Option<u64>,
    /// Skip the hardware encoder even where it is available -- an escape hatch
    /// for a driver that encodes badly, matching the `VE_SW_ENC` env pin.
    pub force_sw: bool,
    /// The file to write. Defaults to [`Format::Mp4`], which is what every
    /// caller wrote before there was a choice.
    pub format: Format,
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
        //
        // An emptied timeline is a legal project and an illegal file: no
        // picture, no sound, and a muxer that is never created because no coded
        // frame arrives. Refused by name here -- before the format is even
        // looked at, so an mp4 and a WAV refuse in the same words -- rather than
        // left to write a file of no frames that nothing opens. Every caller of
        // `start` comes through this, the app's own door
        // (`PlaybackSession::is_empty`) being only the first fence.
        //
        // An audio-only timeline is the other legal project no mp4 can be made
        // of: every frame of it is a gap, so the file would be black picture
        // over the sound -- minutes of encoding for a video of nothing. Refused
        // by name, with the two formats that *are* that timeline, rather than
        // written; the app says the same on the format row before a destination
        // is even picked (`mp4_refusal`).
        let written = match settings.format {
            _ if project.timeline_frames() == 0 => {
                Err("the timeline is empty: there is nothing to export".into())
            }
            format if format.has_video() && !has_picture(&project) => Err(format!(
                "the timeline has no picture: {} would be black. Export WAV or \
                 FLAC, which are the sound itself",
                format.name()
            )
            .into()),
            Format::Mp4 | Format::Av1 => run(&project, &meta, &part, &worker, &settings),
            format => run_audio(&project, &meta, &part, &worker, format),
        };
        let result = written.and_then(|()| std::fs::rename(&part, &out).map_err(Into::into));
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

/// Whether any video lane holds anything at all -- what tells an audio-only
/// timeline from one that merely opens on a gap. Asked of the *lanes* rather
/// than of the sources, because a project may name a video file no clip plays
/// from any more.
fn has_picture(project: &Project) -> bool {
    project
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Video && !project.lane(lane).is_empty())
}

fn settle(shared: &Shared, result: crate::Result<()>) {
    *shared.outcome.lock().unwrap() = Some(result);
    // Published last: a caller that sees the flag is guaranteed the outcome.
    shared.finished.store(true, Ordering::Release);
}

/// The timeline's one audio lane, copied packet for packet: the mp4 path's
/// sound, and the reason that path carries only one lane of it.
///
/// ponytail: this holds the whole exported AAC track in memory (~3 kB per
/// 23 ms packet, so ~500 MB for an hour). Upgrade path is a streaming
/// `copy_segments` that yields packets instead of collecting them.
#[allow(clippy::type_complexity)]
fn copy_audio(
    project: &Project,
    meta: &VideoMeta,
) -> crate::Result<Option<(crate::AacTrackParams, Vec<crate::AacPacket>)>> {
    // The segments name their source, and the copy carries its packet-rounding
    // debt across a source join exactly as across a cut, so a timeline spanning
    // files stays in sync. A source whose AAC parameters disagree with the
    // first one is an `Err` from there -- import refuses those up front, this
    // is the backstop -- and the caller deletes the `.part`.
    //
    // The stream each source is copied from is the one it plays: `audio_sources`
    // is the same list `PlaybackSession::seek` hands the decoder, so an export
    // of a timeline playing a file's second audio track carries *that* track.
    //
    // One lane, because this path *copies* AAC packets and a mix is not a copy:
    // summing two lanes means decoding both and encoding the result, and there
    // is no AAC encoder here to encode it with. Refused by name rather than
    // silently exporting the first lane -- a file missing half its sound is the
    // one failure a user would not notice.
    //
    // *Which* lane is the same question `audio_segments_from` answers for
    // playback, and it is asked here rather than assumed: the lane that holds
    // the sound need not be `A1` (a project may leave `A1` empty and place
    // everything on `A2`), and copying `A1`'s list in that case would write a
    // file with no audio track at all -- silently, which is the failure this
    // whole comment is about.
    let lanes = project.audio_segments_from(0, meta.frame_rate);
    let [segments] = &lanes[..] else {
        return Err(format!(
            "this timeline has {} audio lanes and an mp4 export copies one: \
             there is no AAC encoder here to mix them with. Export WAV or FLAC, \
             which are mixed, or put the sound on one lane",
            lanes.len()
        )
        .into());
    };
    // ...and the same list decides which clips this would be copying, which is
    // the only way to ask whether any of them carries an equalizer. An EQ is
    // sample math and a copy never decodes: exporting it here would write the
    // clip flat, silently -- the same failure a missing audio track is. Named by
    // where it sits on the timeline, because "some clip" is not a thing a user
    // can go and fix.
    let lane = project.audio_lanes()[0];
    if let Some((at, _)) = project
        .lane(lane)
        .iter()
        .enumerate()
        .find(|(idx, _)| {
            project
                .eq_of(lane, *idx)
                .is_some_and(|eq| !eq.is_identity())
        })
        .map(|(idx, clip)| (clip.start, idx))
    {
        return Err(format!(
            "the clip at {} has an equalizer and an mp4 export copies AAC packets, \
             which no filter can reach: export WAV or FLAC, which are decoded and \
             mixed, or clear the equalizer",
            timecode(at, meta.frame_rate)
        )
        .into());
    }
    AudioSession::copy_multi_streams(&project.audio_sources(), segments)
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
    // happens as soon as the first coded picture arrives. None of it for AV1 --
    // that one is picture only, for the reason [`Format::Av1`] states -- and no
    // refusal of it either: a timeline with sound is exported as the video it
    // also is, and the front-end says the sound is not in the file *before* the
    // export starts rather than failing it here.
    let audio = match settings.format {
        Format::Av1 => None,
        _ => copy_audio(project, meta)?,
    };
    let audio_params = audio.as_ref().map(|(track, _)| AudioParams {
        freq_index: track.freq_index,
        chan_conf: track.chan_conf,
    });

    let mut encoder = Enc::open(meta, settings)?;
    let mut muxer = None;
    let mut done = 0u32;
    let black = Black::new(meta);
    // Spans, not clips: a gap in the video is part of the timeline and gets
    // encoded too, as black frames. The picture count is therefore
    // `timeline_frames` however the lanes are arranged -- and *which* lane a
    // span comes from is `composite_spans_from`'s answer, the same one playback
    // shows, so an export is what was watched.
    for span in project.composite_spans_from(0) {
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
        // What this span is graded by -- the same answer playback's decoder
        // carries, so the export is the picture that was watched. `None` for a
        // gap and for an ungraded clip, and that is the path where not a byte
        // is touched. Scratch outside the frame loop: it is refilled per frame
        // and its allocation survives the whole span.
        let grade = project.composite_color_at(span.start).copied();
        let mut graded = (Vec::new(), Vec::new(), Vec::new());
        // ...and the canvas it is placed on, which is where a source of another
        // resolution becomes a picture at the project's. The same `Composer`
        // playback composes with, given the same policy, so an export is what
        // was watched down to the letterbox; a clip already the project's size
        // passes through it untouched.
        let mut canvas = Composer::new(
            meta.width,
            meta.height,
            project.composite_fit_at(span.start),
        );
        for _ in 0..span.len {
            cancelled(shared)?;
            let picture = match &mut pictures {
                Some(pictures) => pictures.next()?,
                None => Some(black.picture()),
            };
            let Some((y, u, v, width, height)) = picture else {
                break; // source ran out early; the clip list outlives the file
            };
            // The planes are borrowed from the decoder (and `Black::picture`
            // hands the same slice as both u and v), so a grade cannot be
            // applied in place: it goes onto a copy, which the encoder then
            // reads instead.
            let (y, u, v) = match grade.filter(|p| !p.is_identity()) {
                Some(params) => {
                    let (gy, gu, gv) = &mut graded;
                    gy.clear();
                    gy.extend_from_slice(y);
                    gu.clear();
                    gu.extend_from_slice(u);
                    gv.clear();
                    gv.extend_from_slice(v);
                    crate::color::apply_yuv(&params, gy, gu, gv);
                    (&gy[..], &gu[..], &gv[..])
                }
                None => (y, u, v),
            };
            // Grade first, place second: the grade is the clip's own pixels
            // and the bars around them are not the clip (see `scale::Composer`).
            let (y, u, v, width, height) = canvas.place(y, u, v, width, height);
            if let Some((au, key)) = encoder.encode(y, u, v, width, height)? {
                write_video(
                    &mut muxer,
                    out,
                    meta,
                    settings,
                    audio_params.as_ref(),
                    au,
                    key,
                )?;
            }
            done += 1;
            shared
                .progress
                .store(done * PROGRESS_SCALE / total.max(1), Ordering::Relaxed);
        }
    }
    while let Some((au, key)) = encoder.drain()? {
        write_video(
            &mut muxer,
            out,
            meta,
            settings,
            audio_params.as_ref(),
            au,
            key,
        )?;
    }
    // Progress reads 100% from here on, but nothing is published yet: draining,
    // the audio pass and `finish` are all still cancellable.
    cancelled(shared)?;

    let Some(muxer) = muxer else {
        return Err("export produced no coded pictures".into());
    };
    let muxer = match (muxer, audio) {
        (Muxer::Mp4(mut mp4), Some((_, packets))) => {
            for packet in packets {
                mp4.write_audio_packet(&packet.bytes)?;
            }
            Muxer::Mp4(mp4)
        }
        (muxer, _) => muxer,
    };
    cancelled(shared)?;
    muxer.finish()?;
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// Where a clip sits on the timeline as a person reads it: `mm:ss`, short
/// enough to sit inside a refusal and exact enough to go and find the clip.
/// Not the app's frame-accurate timecode -- an engine refusal is prose, and a
/// `00:00:12:07` in the middle of a sentence is a serial number.
fn timecode(frame: u32, fps: f64) -> String {
    let secs = match fps.is_finite() && fps > 0.0 {
        true => (f64::from(frame) / fps) as u32,
        false => 0,
    };
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// The audio lanes alone, summed, as a WAV or a FLAC.
///
/// Nothing here re-derives the edit: the play lists are
/// [`Project::audio_segments_from`] and they are handed to the very
/// [`AudioSession::open_mixed_streams`] playback feeds from, so what is written
/// is what was heard -- every lane, mixed the same way, gaps included, which
/// arrive as real silence from that choke point rather than as a hole this would
/// have to invent.
///
/// The length is the *timeline's*: each segment resolves its window to whole
/// samples on its own, so the sum can miss the timeline by a sample or two, and
/// a source that runs out early would leave the file short. Both are settled by
/// one `resize` at the end -- padding with silence, trimming what overshot --
/// so an exported file is exactly as long as what was edited.
fn run_audio(
    project: &Project,
    meta: &VideoMeta,
    out: &Path,
    shared: &Shared,
    format: Format,
) -> crate::Result<()> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, meta.frame_rate);
    // The equalizers with them, so what is written is what is heard down to the
    // filter: the worker applies them per segment before the lanes are summed
    // ([`AudioSession::open_mixed_streams_eq`]), which is the same choke point
    // playback's feeder reads from -- there is no second place here that could
    // apply them differently.
    let eqs = project.audio_eqs_from(0, meta.frame_rate);
    let Some((audio, chunks)) = AudioSession::open_mixed_streams_eq(&sources, &segs, &eqs)? else {
        return Err("this timeline has no audio to export".into());
    };
    let channels = usize::from(audio.channels);
    let frames = (f64::from(project.timeline_frames()) / meta.frame_rate
        * f64::from(audio.sample_rate))
    .round() as u64;
    let total = frames as usize * channels;
    // ponytail: the whole export sits in memory (4 bytes a sample, so ~23 MB
    // per minute of 48 kHz stereo). `flacenc::MemSource` wants it that way and
    // the mp4 path already collects its copied AAC track the same. Upgrade path
    // is hound's incremental writer plus flacenc's `Source` trait.
    let mut samples: Vec<i32> = Vec::with_capacity(total);
    for chunk in chunks {
        cancelled(shared)?;
        // Already equalized: the chunks come out of the per-lane workers, which
        // filter each segment before the mix (`open_mixed_streams_eq` above).
        samples.extend(
            chunk
                .samples
                .iter()
                .map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i32),
        );
        let done = samples.len().min(total) * PROGRESS_SCALE as usize / total.max(1);
        shared.progress.store(done as u32, Ordering::Relaxed);
    }
    samples.resize(total, 0);
    // Written in one go and still cancellable up to here, exactly as the mp4
    // path is up to its `finish`: the `.part` is what either gets renamed or
    // deleted.
    cancelled(shared)?;
    match format {
        Format::Wav => write_wav(out, &samples, &audio)?,
        Format::Flac => write_flac(out, &samples, &audio)?,
        Format::Mp4 | Format::Av1 => unreachable!("the picture formats are `run`"),
    }
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// 16-bit PCM at the timeline's own rate and layout: no resampling anywhere in
/// this engine, so what the decoder handed over is what the header says.
fn write_wav(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    let mut writer = hound::WavWriter::create(
        out,
        hound::WavSpec {
            channels: audio.channels,
            sample_rate: audio.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for &sample in samples {
        writer.write_sample(sample as i16)?;
    }
    // Not `drop`: this is what rewrites the RIFF sizes in the header, and its
    // failure is the difference between a finished file and a truncated one.
    writer.finalize()?;
    Ok(())
}

/// The same samples, losslessly compressed. `flacenc`'s errors are flattened to
/// text: they are not `Send + Sync`, and every one of them fails the export the
/// same way.
fn write_flac(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| format!("flac encoder config: {e}"))?;
    let source = flacenc::source::MemSource::from_samples(
        samples,
        usize::from(audio.channels),
        16,
        audio.sample_rate as usize,
    );
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| format!("flac encode: {e}"))?;
    // The block sizes are stated fixed, which is what the frames themselves say
    // (they are numbered, not sample-addressed). flacenc's single-threaded path
    // instead writes the *last* frame's length as the minimum, and a short final
    // frame is the normal case -- a stream whose `streaminfo` then disagrees
    // with its own blocking strategy. ffmpeg shrugs and plays it; symphonia,
    // which is this engine's own reader, rejects every frame header and reads
    // to EOF looking for a good one. Written this way the file reopens here,
    // which is the only test of an export that counts. (Same value flacenc's
    // own `par` path writes; the reference encoder does it too.)
    stream
        .stream_info_mut()
        .set_block_sizes(config.block_size, config.block_size)
        .map_err(|e| format!("flac block size: {e}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| format!("flac write: {e}"))?;
    std::fs::write(out, sink.as_slice())?;
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

/// The two containers an export writes, so the picture loop above is one loop:
/// H.264 goes into mp4, AV1 into Matroska, and nothing else is offered.
enum Muxer {
    Mp4(Mp4Muxer),
    Mkv(MkvMuxer),
}

impl Muxer {
    fn finish(self) -> crate::Result<()> {
        match self {
            Self::Mp4(mp4) => mp4.finish(),
            Self::Mkv(mkv) => mkv.finish(),
        }
    }
}

/// Writes one access unit, creating the file on the first one -- the parameter
/// sets for `avcC` (and the sequence header for `CodecPrivate`) only exist once
/// the encoder has coded something. Units with no coded slice are skipped: a
/// software encoder may hand back an empty buffer while it buffers, and the
/// muxer rejects a sample that would carry no picture.
fn write_video(
    muxer: &mut Option<Muxer>,
    out: &Path,
    meta: &VideoMeta,
    settings: &ExportSettings,
    audio: Option<&AudioParams>,
    au: &[u8],
    key: bool,
) -> crate::Result<()> {
    if settings.format == Format::Av1 {
        let muxer = match muxer {
            Some(Muxer::Mkv(mkv)) => mkv,
            Some(Muxer::Mp4(_)) => unreachable!("the format picks the muxer once"),
            none => {
                // Every AV1 stream opens on a keyframe, and a keyframe carries
                // the sequence header the track has to declare: an encoder that
                // handed back neither has produced nothing a decoder can start.
                let config = crate::mux::av1_sequence_header(au)
                    .ok_or("the first coded picture carries no AV1 sequence header")?;
                let Muxer::Mkv(mkv) = none.insert(Muxer::Mkv(MkvMuxer::create(
                    out,
                    &Av1Params {
                        width: meta.width,
                        height: meta.height,
                        frame_rate: meta.frame_rate,
                        config,
                    },
                )?)) else {
                    unreachable!("just inserted a Matroska muxer")
                };
                mkv
            }
        };
        return muxer.write_frame(au, key);
    }
    if !crate::mux::has_coded_slice(au) {
        return Ok(());
    }
    let muxer = match muxer {
        Some(Muxer::Mp4(mp4)) => mp4,
        Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
        none => {
            let (sps, pps) = crate::mux::parameter_sets(au)
                .ok_or("the first coded picture carries no SPS/PPS")?;
            let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create(
                out,
                &VideoParams {
                    width: meta.width,
                    height: meta.height,
                    frame_rate: meta.frame_rate,
                    sps,
                    pps,
                },
                audio,
            )?)) else {
                unreachable!("just inserted an mp4 muxer")
            };
            mp4
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
/// the whole export, so the stream never changes encoder mid-file. Both codecs
/// have both seats: an AV1 export runs on the plugin's VA-API encoder where the
/// GPU has one and on `rav1e` where it has not, which is the same pair H.264 has
/// had and the same silent fallback.
enum Enc {
    Hw(HwEncoder),
    Sw {
        encoder: Encoder,
        /// The last access unit; owned because `rusty_h264` hands back a `Vec`
        /// while the plugin lends a slice, and the two have to look alike here.
        au: Vec<u8>,
        flushed: bool,
    },
    /// AV1 on the GPU, through the same plugin the H.264 seat uses.
    Av1Hw(HwEncoder),
    Av1Sw {
        context: rav1e::Context<u8>,
        /// Temporal units the encoder has finished but the caller has not
        /// collected: `rav1e` reorders and may hand back several at once, while
        /// this interface is one picture in, one unit out (the plugin's own
        /// `ready` queue exists for the same reason).
        ready: std::collections::VecDeque<(Vec<u8>, bool)>,
        /// The unit currently lent out, for the same reason `Sw` owns one.
        au: Vec<u8>,
        flushed: bool,
    },
}

impl Enc {
    fn open(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Self> {
        if settings.format == Format::Av1 {
            return Self::open_av1(meta, settings);
        }
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

    /// The AV1 pair. The hardware seat is the plugin's, opened by a symbol of
    /// its own (`vh_enc_av1_open`) so a plugin built before AV1 encode existed
    /// simply has none and this falls through -- the same "no" a GPU without an
    /// AV1 encode entrypoint gives, and the same silent fallback either way.
    ///
    /// It is **opt-in**, which is the one place this pair does not mirror the
    /// H.264 one: the vendored cros-codecs AV1 encoder hung the GPU on this
    /// project's own radeonsi box -- `engine_hw: operation failed`, then an
    /// amdgpu hard recovery and a lost context, measured 2026-08-10 exporting
    /// the 720p fixture. A software encoder that takes half a minute is a worse
    /// export than a hardware one; a driver reset is not an export at all. So
    /// the plugin's AV1 seat is wired, kept and only entered when `VE_HW_AV1=1`
    /// asks for it by name.
    ///
    /// ponytail: the upgrade path is a driver this was reproduced against (or a
    /// cros-codecs release that fixes it) plus a probe encode of one frame at
    /// open, after which this can prefer hardware the way H.264 does.
    fn open_av1(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Self> {
        let bitrate = settings
            .bitrate
            .map_or_else(|| bitrate_for(meta), |b| b.clamp(MIN_BITRATE, MAX_BITRATE));
        let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate)?;
        // Two seconds between keyframes, as the H.264 seat does: a seek may only
        // land on one, and this is what a cluster of the Matroska file is.
        let gop = (meta.frame_rate * 2.0).round().max(1.0) as u64;
        if forced("VE_HW_AV1")
            && !settings.force_sw
            && !forced("VE_SW_ENC")
            && let Some(hw) =
                HwEncoder::open_av1(meta.width, meta.height, fps_num, fps_den, bitrate)
        {
            eprintln!("export encoder: hardware AV1 (VA-API plugin)");
            return Ok(Self::Av1Hw(hw));
        }
        eprintln!("export encoder: software AV1 (rav1e)");
        let mut cfg = rav1e::EncoderConfig::default();
        cfg.width = meta.width as usize;
        cfg.height = meta.height as usize;
        cfg.bit_depth = 8;
        cfg.chroma_sampling = rav1e::prelude::ChromaSampling::Cs420;
        // Seconds per frame, which is what `frame_timing` already has as an
        // exact rational -- 1001/24000 rather than a rounded 23.976.
        cfg.time_base = rav1e::prelude::Rational::new(u64::from(fps_den), u64::from(fps_num));
        cfg.min_key_frame_interval = gop;
        cfg.max_key_frame_interval = gop;
        cfg.bitrate = bitrate.min(i32::MAX as u64) as i32;
        // The fastest preset there is, and it is still slow: `rav1e` is built
        // here without its assembly (no `nasm` in this project's build), so an
        // export runs at a fraction of realtime. The export worker reports
        // progress the whole way, which is what makes that bearable rather than
        // hidden.
        cfg.speed_settings = rav1e::prelude::SpeedSettings::from_preset(AV1_SPEED);
        let config = rav1e::Config::new()
            .with_encoder_config(cfg)
            .with_threads(std::thread::available_parallelism().map_or(1, |n| n.get()));
        let context = config
            .new_context::<u8>()
            .map_err(|e| format!("software AV1 encoder: {e}"))?;
        Ok(Self::Av1Sw {
            context,
            ready: std::collections::VecDeque::new(),
            au: Vec::new(),
            flushed: false,
        })
    }

    /// One picture in, at most one access unit out, and whether that unit is one
    /// a decoder may be started from -- which only the Matroska muxer asks, the
    /// mp4 one reading its own sync flag off the IDR slice.
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
    ) -> crate::Result<Option<(&[u8], bool)>> {
        match self {
            Self::Av1Hw(hw) => {
                let au = hw.encode(y, u, v, width, height, false)?;
                // What the *bitstream* says, not what was asked for: an AV1
                // keyframe carries the sequence header, which is the same mark
                // `demux` reads a sync point off. A driver that emitted one
                // unasked is then still a seek target, and one that skipped a
                // requested keyframe cannot be mistaken for a decoder's start.
                Ok(au.map(|au| (au, crate::mux::av1_sequence_header(au).is_some())))
            }
            Self::Av1Sw {
                context, ready, au, ..
            } => {
                let mut frame = context.new_frame();
                let (w, h) = (width as usize, height as usize);
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                frame.planes[0].copy_from_raw_u8(&y[..w * h], w, 1);
                frame.planes[1].copy_from_raw_u8(&u[..cw * ch], cw, 1);
                frame.planes[2].copy_from_raw_u8(&v[..cw * ch], cw, 1);
                if let Err(e) = context.send_frame(frame) {
                    return Err(format!("software AV1 encode: {e}").into());
                }
                collect_av1(context, ready)?;
                Ok(pop_av1(ready, au))
            }
            Self::Hw(hw) => Ok(hw
                .encode(y, u, v, width, height, false)?
                .map(|au| (au, false))),
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
                Ok(Some((&au[..], false)).filter(|(au, _)| !au.is_empty()))
            }
        }
    }

    /// End of stream; call until it returns `None`.
    fn drain(&mut self) -> crate::Result<Option<(&[u8], bool)>> {
        match self {
            Self::Av1Hw(hw) => {
                let au = hw.drain()?;
                Ok(au.map(|au| (au, crate::mux::av1_sequence_header(au).is_some())))
            }
            Self::Av1Sw {
                context,
                ready,
                au,
                flushed,
            } => {
                if !*flushed {
                    *flushed = true;
                    context.flush();
                    collect_av1(context, ready)?;
                }
                Ok(pop_av1(ready, au))
            }
            Self::Hw(hw) => Ok(hw.drain()?.map(|au| (au, false))),
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
                Ok(Some((&au[..], false)).filter(|(au, _)| !au.is_empty()))
            }
        }
    }
}

/// Every temporal unit `rav1e` has finished, moved into the queue in display
/// order. `NeedMoreData` (send another picture) and `LimitReached` (the flush is
/// through) are the two ways it says there is nothing more; `Encoded` means a
/// picture was coded but not yet packetised, which is a *keep asking* -- taking
/// it for an end leaves the tail of the export inside the encoder.
fn collect_av1(
    context: &mut rav1e::Context<u8>,
    ready: &mut std::collections::VecDeque<(Vec<u8>, bool)>,
) -> crate::Result<()> {
    use rav1e::prelude::{EncoderStatus, FrameType};
    loop {
        match context.receive_packet() {
            Ok(packet) => ready.push_back((packet.data, packet.frame_type == FrameType::KEY)),
            Err(EncoderStatus::Encoded) => {}
            Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => return Ok(()),
            Err(e) => return Err(format!("software AV1 encode: {e}").into()),
        }
    }
}

/// The oldest finished unit, moved into the buffer that is lent out -- the same
/// "valid until the next call" contract the plugin's slice has.
fn pop_av1<'a>(
    ready: &mut std::collections::VecDeque<(Vec<u8>, bool)>,
    au: &'a mut Vec<u8>,
) -> Option<(&'a [u8], bool)> {
    let (unit, key) = ready.pop_front()?;
    *au = unit;
    Some((&au[..], key))
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
        let (meta, mut demuxer) = Demuxer::open(path)?;
        // The software decoder is H.264-only; an HEVC or VP9 source that got
        // this far means the plugin refused it, and the export says so instead
        // of handing those bytes to `rusty_h264`.
        if meta.codec != crate::demux::Codec::H264 {
            return Err(meta.codec.needs_plugin().into());
        }
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
            codec: crate::demux::Codec::H264,
        };
        // 1280 * 720 * 30 * 0.1
        assert_eq!(bitrate_for(&meta(1280, 720, 30.0)), 2_764_800);
        assert_eq!(bitrate_for(&meta(320, 240, 30.0)), MIN_BITRATE, "tiny");
        assert_eq!(bitrate_for(&meta(3840, 2160, 60.0)), MAX_BITRATE, "huge");
    }
}
