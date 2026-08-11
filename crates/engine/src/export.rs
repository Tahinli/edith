//! Export: the edit list rendered back out as one mp4 (H.264 or AV1) or one
//! AV1-in-Matroska — or, picture left behind, as one WAV, FLAC or MP3 of the
//! timeline's audio alone.
//!
//! Video is fully re-encoded — a cut lands mid-GOP, so stream-copying across it
//! is impossible — while audio is copied packet for packet wherever a copy can
//! say what the timeline says: a copy is exact, free and never a generation of
//! loss. Where it *cannot* — a second audio lane to mix in, a speeded clip, an
//! equalized one, a source that is not AAC inside an mp4's own sample table —
//! the sound is decoded, mixed and encoded again with `rusty_aac`
//! ([`encode_audio`]). The audio-only formats have no such split: `hound` writes
//! PCM, `flacenc` encodes FLAC and `rusty_mp3` encodes MP3, so those are always
//! *decoded* out of the timeline. The worker owns everything: the caller gets an
//! [`ExportHandle`] and polls it from its render loop.
//!
//! **Every video format here carries the timeline's sound**, in whichever
//! container: the mp4 muxer writes an AAC track and so does the Matroska one.
//! Nothing is written picture-only and nothing is left silent without saying so
//! — the only sound that does not come out is sound this engine cannot decode at
//! all (Opus and AC-3 inside a Matroska file), and that is an error by name.
//!
//! A **speeded** clip splits along the same line, and for the same reason. The
//! picture is re-encoded, so the frame walk honours a rate: it takes the source
//! frame each timeline frame shows ([`crate::project::Speed::source_at`], the
//! very frame preview holds there) and encodes a held one again rather than
//! decoding it twice. The sound is *copied* unless something forces a decode,
//! and a packet carries no rate — so a speeded lane is one of the things that
//! forces one ([`copy_audio`]) rather than being written out at 1.00x under a
//! re-timed picture.
//!
//! Nothing partial survives a failure: the worker writes to `<out>.part` and
//! renames it onto `out` only once the file is closed and complete, so the
//! output either does not exist or is finished — there is no window where a
//! half-written `.export.mp4` is sitting there looking playable. Cancel and
//! every error path delete the `.part`. A killed *process* leaves the `.part`
//! behind (only in-process cleanup is promised), which is an orphan a user can
//! delete rather than a file that plays for two seconds and stops.

use std::fs::File;
use std::io::{BufWriter, Write};
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

/// What an export writes. H.264-in-mp4 and AV1 -- in Matroska or in mp4, the
/// user's pick of container -- are the only *video* pairs with both an encoder
/// and a decoder under this project's no-install rule, and WAV, FLAC and MP3 are
/// the standalone audio formats with a pure-Rust encoder: `hound`, `flacenc` and
/// `rusty_mp3`. Vorbis and Opus have none. (AAC does -- `rusty_aac`, which is
/// what a re-encoded video track's sound leaves through -- but AAC is a
/// container's own audio, never a file of its own here.) HEVC and VP9 have no
/// encoder here at all (`hevc`/`vp9` import through the plugin and stop there).
/// A front-end says so rather than hiding the rows.
///
/// **Every video format carries sound.** Which way it carries it is the only
/// difference: copied packet for packet where a copy can say what the timeline
/// says, decoded and encoded again ([`encode_audio`]) where it cannot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Format {
    /// Picture and sound: video re-encoded, AAC copied -- or re-encoded too,
    /// where a copy could not carry the edit (a second audio lane to mix in, a
    /// speeded clip, an equalized one, a source no mp4 sample table holds).
    #[default]
    Mp4,
    /// AV1 in Matroska, with the timeline's AAC beside it: this engine's
    /// Matroska writer carries an audio track ([`crate::mux::MkvMuxer`]) and its
    /// reader reads one back, symphonia's `mkv` reader being what
    /// `audio::Track::open` opens it with.
    Av1,
    /// The same AV1 stream in an mp4, for everything that plays mp4 and not
    /// Matroska. The sample entry `mp4 0.14` cannot write is written by hand
    /// ([`crate::mux::Mp4Muxer::create_av1`]); the sound is the mp4 path's own,
    /// unchanged.
    Av1Mp4,
    /// The audio lanes alone, 16-bit PCM.
    Wav,
    /// The audio lanes alone, losslessly compressed.
    Flac,
    /// The audio lanes alone, MPEG-1 Layer III at 256 kbps CBR (`rusty_mp3`).
    Mp3,
}

impl Format {
    /// The extension a file of this format is named with -- a front-end builds
    /// the destination path from it, so the name never disagrees with the bytes.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mp4 | Self::Av1Mp4 => "mp4",
            Self::Av1 => "mkv",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }

    /// Whether this format carries the picture. The bitrate settings are video
    /// settings and mean nothing to the audio-only ones.
    pub fn has_video(self) -> bool {
        matches!(self, Self::Mp4 | Self::Av1 | Self::Av1Mp4)
    }

    /// Whether the picture in it is AV1 rather than H.264 -- which encoder runs
    /// and which of the two AV1 containers is being written are separate
    /// questions, and this is the first.
    fn is_av1(self) -> bool {
        matches!(self, Self::Av1 | Self::Av1Mp4)
    }

    /// What the format is called where a refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "an mp4",
            Self::Av1 => "an AV1 Matroska",
            Self::Av1Mp4 => "an AV1 mp4",
            Self::Wav => "a WAV",
            Self::Flac => "a FLAC",
            Self::Mp3 => "an MP3",
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
                "the timeline has no picture: {} would be black. Export WAV, \
                 FLAC or MP3, which are the sound itself",
                format.name()
            )
            .into()),
            format if format.has_video() => run(&project, &meta, &part, &worker, &settings),
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

/// The timeline's audio track for a file that carries picture: copied packet for
/// packet where a copy says exactly what the timeline says, and decoded, mixed
/// and encoded again ([`encode_audio`]) where it cannot -- a second audio lane,
/// a speeded clip, an equalizer, a source that is not AAC inside an mp4 sample
/// table. Nothing is refused for being uncopyable any more; the only refusals
/// left below the copy are about sound this engine cannot *decode* either, and
/// those come out of the decode path by name.
///
/// The copy is still the default and still bit-exact: a timeline nobody has
/// touched leaves as the very packets its source holds.
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
    // One lane is the only shape a *copy* has: summing two means decoding both,
    // and a sum is not a copy. Two or more go to [`encode_audio`], which is the
    // very mix the WAV path writes -- it used to be a refusal, and a refusal is
    // what a file missing half its sound deserves, not what a mix does.
    //
    // *Which* lane is the same question `audio_segments_from` answers for
    // playback, and it is asked here rather than assumed: the lane that holds
    // the sound need not be `A1` (a project may leave `A1` empty and place
    // everything on `A2`), and copying `A1`'s list in that case would write a
    // file with no audio track at all -- silently, which is the failure this
    // whole comment is about.
    let lanes = project.audio_segments_from(0, meta.frame_rate);
    let [segments] = &lanes[..] else {
        return encode_audio(project, meta);
    };
    // ...and the same list names *which* lane those clips sit on, which is the
    // only way to ask what has been done to them.
    let lane = project.audio_lanes()[0];
    // ...and the same list decides whether any clip this would copy plays at a
    // rate other than the one it was recorded at. A copy hands the muxer the
    // very AAC packets the source holds and there is no rate inside a packet to
    // change: the picture would come out re-timed (the walk in [`run`] honours
    // it) over sound at 1.00x, which is the drift a person finds only after the
    // file has gone somewhere. Decoded and resampled instead, by the same worker
    // playback feeds from.
    //
    // ...and whether any clip on it carries an equalizer, which is sample math a
    // copy never reaches: copying such a lane would write the clip *flat*,
    // silently.
    //
    // Only these: a timeline nobody has touched still leaves through the copy
    // below, packet for packet, so no passthrough quietly becomes a generation
    // of loss.
    let speeded = project.lane(lane).iter().any(|c| !c.speed.is_normal());
    let equalized = (0..project.lane(lane).len())
        .any(|idx| project.eq_of(lane, idx).is_some_and(|eq| !eq.is_identity()));
    if speeded || equalized {
        return encode_audio(project, meta);
    }
    // What is left is a copy the *sources* may still not be able to give: AAC
    // inside a Matroska file (readable, but not out of a sample table this walks
    // -- there is none), an mp3 or a wav on the timeline, an AC-3 track, two
    // sources whose AAC parameters disagree. Every one of them decodes, and what
    // decodes can be encoded again, so the copy's refusal is now a route: the
    // error a *decode* cannot get past -- Opus or AC-3 inside a Matroska file,
    // which symphonia has no decoder for at any version -- is the one that comes
    // back, by name, from [`encode_audio`].
    match AudioSession::copy_multi_streams(&project.audio_sources(), segments) {
        Ok(copied) => Ok(copied),
        Err(_) => encode_audio(project, meta),
    }
}

/// The same lane, *decoded*: what a packet copy cannot carry comes out here as
/// AAC written by this project's own encoder.
///
/// The samples are the very ones [`run_audio`] would put in a WAV -- the same
/// opener with the same equalizers, the same rates and the same resize onto the
/// timeline's own length -- so an mp4 and a WAV of one timeline are one mix,
/// not two that could disagree.
///
/// AAC-LC's encoder delay is one 1024-frame block, which is exactly the priming
/// a reader drops when the file carries no edit list (`audio::DEFAULT_PRIMING`,
/// and [`crate::mux::Mp4Muxer`] writes none): the first packet is that delay, so
/// the sound starts at timeline frame 0 as a copied track's does.
///
/// ponytail: the mix sits in memory as f32 before it is encoded (~46 MB a
/// minute of 48 kHz stereo), for [`run_audio`]'s reason -- `rusty_aac` buffers
/// the whole stream anyway, since it encodes its frames in parallel. Upgrade
/// path is a chunked push once that encoder streams.
#[allow(clippy::type_complexity)]
fn encode_audio(
    project: &Project,
    meta: &VideoMeta,
) -> crate::Result<Option<(crate::AacTrackParams, Vec<crate::AacPacket>)>> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, meta.frame_rate);
    let eqs = project.audio_eqs_from(0, meta.frame_rate);
    let speeds = project.audio_speeds_from(0, meta.frame_rate);
    let Some((audio, chunks)) =
        AudioSession::open_mixed_streams_speed(&sources, &segs, &eqs, &speeds)?
    else {
        return Ok(None); // no audio to write, exactly as a copy of nothing is
    };
    let freq_index = rusty_aac::sf_index_for_rate(audio.sample_rate).ok_or_else(|| {
        format!(
            "{} Hz is not an AAC sample rate: export WAV, FLAC or MP3, which write it as \
             it is",
            audio.sample_rate
        )
    })?;
    let channels = usize::from(audio.channels.max(1));
    // The timeline's length, not the mix's: a segment resolves its window to
    // whole samples on its own, so the sum can miss by a sample or two and a
    // source that ran out early would leave the track short under a picture
    // that is not. One resize settles both, as [`run_audio`]'s does.
    let total = (f64::from(project.timeline_frames()) / meta.frame_rate
        * f64::from(audio.sample_rate))
    .round() as usize
        * channels;
    let mut samples: Vec<f32> = Vec::with_capacity(total);
    for chunk in chunks {
        samples.extend(chunk.samples);
    }
    samples.resize(total, 0.0);

    // 256 kbps, not the encoder's 128 default: this is a *second* generation of
    // lossy coding over a source that was already AAC, and the export is a
    // master a person edits from again, not a delivery file. Measured on the
    // suite's fixtures, an untouched clip re-encoded here moves 0.15 dB at this
    // rate against 0.46 dB at 128 -- and 32 KB/s beside a 2.76 Mbps picture is
    // not a size anyone is counting.
    let mut encoder = rusty_aac::AacEncoder::new(rusty_aac::AacEncoderConfig {
        bitrate_bps: 256_000,
        ..Default::default()
    });
    encoder.push_pcm(&samples, audio.channels.max(1), audio.sample_rate)?;
    encoder.finish();
    let mut packets = Vec::new();
    // `next_packet` after `finish` yields packets until `Eof` and nothing else,
    // so the loop ends on the only error it can see.
    while let Ok(packet) = encoder.next_packet() {
        packets.push(crate::AacPacket {
            bytes: packet.data,
            samples: packet.duration,
        });
    }
    Ok(Some((
        crate::AacTrackParams {
            freq_index,
            // 1 and 2 are the ISO channel configurations for mono and stereo,
            // and the opener refuses anything wider than stereo already.
            chan_conf: audio.channels.max(1) as u8,
            sample_rate: audio.sample_rate,
        },
        packets,
    )))
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
    // happens as soon as the first coded picture arrives -- and the Matroska
    // muxer wants the packets themselves that early too, because it interleaves
    // them into the clusters as it writes. Every video format gets the same
    // track: none of them is picture-only any more.
    let audio = copy_audio(project, meta)?;
    let audio_params = audio.as_ref().map(|(track, _)| AudioParams {
        freq_index: track.freq_index,
        chan_conf: track.chan_conf,
        sample_rate: track.sample_rate,
    });
    // Taken by the Matroska muxer at creation; the mp4 one writes its track
    // after the picture, so for that path this is still `Some` at the end.
    let mut packets = audio.map(|(_, packets)| packets);

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
        // Timeline frames, taken a *source* frame at a time: at a rate other
        // than real time the two are not the same count, and the span says which
        // source frame each timeline frame shows ([`Speed::source_at`]) -- the
        // very frame playback is holding there, because the two conversions are
        // one floor/ceil pair (`speed_maps_both_ways`). A speed-up decodes the
        // frames it skips and drops them, because the pictures after them
        // reference those; a slow-down encodes the picture it is holding again
        // rather than decoding it twice. At real time `repeats` is 1 and `want`
        // advances by one, which is the loop this always ran.
        let mut done_here = 0u32;
        // Source frames already taken out of this span's decoder.
        let mut taken = 0u32;
        while done_here < span.len {
            cancelled(shared)?;
            let want = span.speed.source_at(done_here);
            let repeats = span.speed.repeats(done_here, span.len);
            let picture = match &mut pictures {
                Some(pictures) => {
                    let mut ran_out = false;
                    while taken < want && !ran_out {
                        ran_out = pictures.next()?.is_none();
                        taken += 1;
                    }
                    match ran_out {
                        true => None,
                        false => {
                            taken += 1;
                            pictures.next()?
                        }
                    }
                }
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
            // Once per timeline frame this source frame covers: one at real time
            // and faster, more when the clip is slowed -- the picture is already
            // graded and placed, so a held frame costs an encode and no decode.
            for _ in 0..repeats {
                if let Some((au, key)) = encoder.encode(y, u, v, width, height)? {
                    write_video(
                        &mut muxer,
                        out,
                        meta,
                        settings,
                        audio_params.as_ref(),
                        &mut packets,
                        au,
                        key,
                    )?;
                }
                done += 1;
                shared
                    .progress
                    .store(done * PROGRESS_SCALE / total.max(1), Ordering::Relaxed);
            }
            done_here += repeats;
        }
    }
    while let Some((au, key)) = encoder.drain()? {
        write_video(
            &mut muxer,
            out,
            meta,
            settings,
            audio_params.as_ref(),
            &mut packets,
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
    // The mp4's audio track after its picture -- the Matroska one interleaved
    // its own as it went and left `packets` empty behind it.
    let muxer = match (muxer, packets) {
        (Muxer::Mp4(mut mp4), Some(packets)) => {
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
    // ...and the rates with them, for exactly the same reason: a speeded clip is
    // resampled inside the worker playback feeds from, so a WAV of a timeline at
    // 2x is half as long and holds the samples that were heard -- not a
    // second pass here that could disagree with what the ear got.
    let speeds = project.audio_speeds_from(0, meta.frame_rate);
    let Some((audio, chunks)) =
        AudioSession::open_mixed_streams_speed(&sources, &segs, &eqs, &speeds)?
    else {
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
        Format::Mp3 => write_mp3(out, &samples, &audio)?,
        Format::Mp4 | Format::Av1 | Format::Av1Mp4 => {
            unreachable!("the picture formats are `run`")
        }
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

/// The same samples as MPEG-1 Layer III, `rusty_mp3` doing the encoding -- pure
/// Rust like every other encoder here, and the reason this format is a row at
/// all: the LGPL `shine-rs` was the only one when it was not.
///
/// 256 kbps CBR, for [`encode_audio`]'s reason: an export is a master a person
/// edits from again, not a delivery file, and this is a lossy generation over
/// sources that may already have been one. The rate is snapped to a legal Layer
/// III value by the encoder, and a sample rate MPEG has no frame for (anything
/// but 8-48 kHz) is refused there by name rather than written as something else.
///
/// ponytail: CBR only, and no rate is offered to the caller -- the export card
/// has one bitrate control and it is the *picture's*. Upgrade path is
/// `Mp3EncoderConfig::vbr_quality` behind a setting of its own.
fn write_mp3(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    let mut encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
        bitrate_kbps: 256,
        vbr_quality: None,
    });
    // The samples are the 16-bit ones a WAV of this timeline holds, so the two
    // files are one mix -- and `push_pcm_s16` divides by 32768 exactly as this
    // engine's own decoders do.
    let pcm: Vec<i16> = samples.iter().map(|&s| s as i16).collect();
    encoder
        .push_pcm_s16(&pcm, audio.channels, audio.sample_rate)
        .map_err(|e| format!("mp3 encode: {e}"))?;
    encoder.finish();
    let mut file = BufWriter::new(File::create(out)?);
    // `next_packet` after `finish` yields frames until `Eof`, the Xing/Info
    // header first: the loop ends on the only error it can see.
    while let Ok(frame) = encoder.next_packet() {
        file.write_all(&frame)?;
    }
    file.flush()?;
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
    packets: &mut Option<Vec<crate::AacPacket>>,
    au: &[u8],
    key: bool,
) -> crate::Result<()> {
    if settings.format.is_av1() {
        // Every AV1 stream opens on a keyframe, and a keyframe carries the
        // sequence header the track has to declare: an encoder that handed back
        // neither has produced nothing a decoder can start. The same header goes
        // into a `CodecPrivate` and into an `av1C` -- one record, two containers.
        fn params<'a>(meta: &VideoMeta, au: &'a [u8]) -> crate::Result<Av1Params<'a>> {
            Ok(Av1Params {
                width: meta.width,
                height: meta.height,
                frame_rate: meta.frame_rate,
                config: crate::mux::av1_sequence_header(au)
                    .ok_or("the first coded picture carries no AV1 sequence header")?,
            })
        }
        if settings.format == Format::Av1 {
            let muxer = match muxer {
                Some(Muxer::Mkv(mkv)) => mkv,
                Some(Muxer::Mp4(_)) => unreachable!("the format picks the muxer once"),
                none => {
                    // The sound with it, all of it: the Matroska muxer writes the
                    // packets into the clusters of the pictures they play under.
                    let sound = audio.zip(packets.take());
                    let Muxer::Mkv(mkv) = none.insert(Muxer::Mkv(MkvMuxer::create(
                        out,
                        &params(meta, au)?,
                        sound,
                    )?)) else {
                        unreachable!("just inserted a Matroska muxer")
                    };
                    mkv
                }
            };
            return muxer.write_frame(au, key);
        }
        let muxer = match muxer {
            Some(Muxer::Mp4(mp4)) => mp4,
            Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
            none => {
                let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create_av1(
                    out,
                    &params(meta, au)?,
                    audio,
                )?)) else {
                    unreachable!("just inserted an mp4 muxer")
                };
                mp4
            }
        };
        return muxer.write_av1_frame(au, key);
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
        // Which *codec* the picture is, not which container it goes in: both AV1
        // formats run the same encoder.
        if settings.format.is_av1() {
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
    /// A still image: one picture, handed out for as long as the span runs.
    /// It is decoded here rather than in `run`'s loop for the same reason the
    /// other two are opened there -- the span's pictures come from one place.
    Still(crate::decode::Still),
}

impl ClipDecoder {
    fn open(path: &Path, start_frame: u32) -> crate::Result<Self> {
        // Before either decoder: an image is not a stream, so `start_frame`
        // means nothing to it -- every frame of a still span is the same
        // picture, which is what playback shows for it too.
        if crate::is_image(path) {
            return Ok(Self::Still(crate::decode::Still::open(path)?));
        }
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
            Self::Still(still) => Ok(Some(still.picture())),
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
