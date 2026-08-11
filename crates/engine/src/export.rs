//! Export: the edit list rendered back out as one mp4 (H.264, AV1 or HEVC) or
//! one Matroska file (AV1 or HEVC) — or, picture left behind, as one WAV, FLAC
//! or MP3 of the timeline's audio alone.
//!
//! The HEVC pair is **intra-only**, and says so wherever it is offered: every
//! frame is a self-contained IDR from the vendored `oxideav-h265`, which is the
//! only shape of a pure-Rust HEVC encode fast enough to wait for (its inter
//! modes code 1080p at 0.81 fps across 12 cores against the intra path's 4.30).
//! That makes it an intraframe master in the family of ProRes and DNxHD — large
//! files, every frame a cut point — and not a delivery codec.
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
//! decoding it twice. A clip whose *file* was shot at another rate than the
//! timeline goes through the same walk with one more conversion on the end
//! ([`crate::project::Rate`]), so it is encoded at the speed it was shot at.
//! The sound is *copied* unless something forces a decode, and a packet carries
//! no rate — so a speeded lane is one of the things that forces one
//! ([`copy_audio`]) rather than being written out at 1.00x under a re-timed
//! picture. WAV, FLAC and MP3 are decoded and resampled, so they honour a rate
//! like the picture does.
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
use crate::mux::{AudioParams, Av1Params, HevcParams, MkvMuxer, Mp4Muxer, VideoParams};
use crate::project::{LaneKind, Project, Rate};
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

/// The HEVC coding tree block this encoder is fixed at: the coded picture is a
/// whole number of them, which is what the conformance window crops back.
const CTB: usize = 16;
/// How many frames the intra HEVC encoder codes at once. Twelve is where the
/// 2026-08-11 bench stopped scaling on this box (4.30 fps at 1080p); more lanes
/// only holds more decoded frames in memory.
const HEVC_LANES: usize = 12;
/// Bits per pixel the intra encoder spends at QP 27, measured on the same bench:
/// 0.607 at 720p and 0.586 at 1080p. What [`hevc_qp`] maps a bitrate row through.
const HEVC_BPP_AT_27: f64 = 0.6;
/// The band [`hevc_qp`] is clamped into, for the reason stated there.
const HEVC_QP_MIN: i32 = 22;
const HEVC_QP_MAX: i32 = 40;

/// What an export writes. H.264-in-mp4, AV1 and HEVC -- each of the last two in
/// Matroska or in mp4, the user's pick of container -- are the *video* pairs
/// with both an encoder and a decoder under this project's no-install rule, and
/// WAV, FLAC and MP3 are the standalone audio formats with a pure-Rust encoder:
/// `hound`, `flacenc` and `rusty_mp3`. Vorbis and Opus have none. (AAC does --
/// `rusty_aac`, which is what a re-encoded video track's sound leaves through --
/// but AAC is a container's own audio, never a file of its own here.) VP9 is the
/// one codec left that comes in through the plugin and stops there, HEVC having
/// gained an encoder (an intra-only one, which the rows say). A front-end says
/// so rather than hiding the row.
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
    /// HEVC in Matroska, **intra-only**: every frame is a self-contained IDR
    /// picture ([`Enc::open_hevc`]), which is what makes a pure-Rust HEVC
    /// encoder fast enough to be an export at all. An intraframe master, like
    /// ProRes or DNxHD -- large files, and every frame a cut point.
    Hevc,
    /// The same HEVC stream in an mp4 (`hvc1`), for everything that plays mp4
    /// and not Matroska -- the AV1 pair's split, for the same reason.
    HevcMp4,
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
            Self::Mp4 | Self::Av1Mp4 | Self::HevcMp4 => "mp4",
            Self::Av1 | Self::Hevc => "mkv",
            Self::Wav => "wav",
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }

    /// Whether this format carries the picture. The bitrate settings are video
    /// settings and mean nothing to the audio-only ones.
    pub fn has_video(self) -> bool {
        matches!(
            self,
            Self::Mp4 | Self::Av1 | Self::Av1Mp4 | Self::Hevc | Self::HevcMp4
        )
    }

    /// Whether the picture in it is AV1 rather than H.264 -- which encoder runs
    /// and which of the two AV1 containers is being written are separate
    /// questions, and this is the first.
    fn is_av1(self) -> bool {
        matches!(self, Self::Av1 | Self::Av1Mp4)
    }

    /// The same question for HEVC, and the same answer for both its containers:
    /// one encoder, two boxes to put its stream in.
    fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc | Self::HevcMp4)
    }

    /// What the format is called where a refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mp4 => "an mp4",
            Self::Av1 => "an AV1 Matroska",
            Self::Av1Mp4 => "an AV1 mp4",
            Self::Hevc => "an HEVC Matroska",
            Self::HevcMp4 => "an HEVC mp4",
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
    /// The encoders this job really opened, published the moment they are
    /// chosen and never changed after -- one seat for the whole file, which is
    /// what [`Enc`] states. `None` until then.
    encoders: Mutex<Option<String>>,
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

    /// What this export is encoding with -- the video seat and what the sound
    /// is doing -- as the worker chose them, hardware fallback included.
    /// `None` for the first instants of a job, before the encoder is open.
    pub fn encoders(&self) -> Option<String> {
        self.shared.encoders.lock().unwrap().clone()
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
        encoders: Mutex::new(None),
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

/// Whether any audio lane holds anything, [`has_picture`]'s pair: what tells a
/// timeline whose export will carry sound from one whose file has none.
fn has_sound(project: &Project) -> bool {
    project
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Audio && !project.lane(lane).is_empty())
}

/// Whether any equalizer on the sound is doing something -- the one edit a
/// packet copy cannot carry, so an mp4 whose lane has one is decoded and
/// re-encoded ([`encode_audio`]) instead of copied. Asked of every audio lane:
/// the copy path has exactly one by the time it asks, and [`planned_audio`] is
/// asked before that is settled.
fn equalized(project: &Project) -> bool {
    project.audio_lanes().into_iter().any(|lane| {
        (0..project.lane(lane).len())
            .any(|idx| project.eq_of(lane, idx).is_some_and(|eq| !eq.is_identity()))
    })
}

/// The bitrate an export codes at: the caller's number through the same clamp
/// as the computed one, for the reason [`Enc::open`] states.
fn bitrate_of(meta: &VideoMeta, settings: &ExportSettings) -> u64 {
    settings
        .bitrate
        .map_or_else(|| bitrate_for(meta), |b| b.clamp(MIN_BITRATE, MAX_BITRATE))
}

/// The hardware seat for these settings, opened exactly as the export opens it
/// -- the same pins, the same AV1 opt-in, the same dimensions a driver may
/// refuse -- or `None` where the software encoder takes the file. The single
/// place that choice is made, which is what lets [`planned_video`] *measure* it
/// instead of promising it.
fn hw_seat(meta: &VideoMeta, settings: &ExportSettings) -> Option<HwEncoder> {
    if settings.force_sw || forced("VE_SW_ENC") {
        return None;
    }
    let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate).ok()?;
    let bitrate = bitrate_of(meta, settings);
    match settings.format {
        // The HEVC pair is software only, and not for want of a GPU: the plugin
        // has no HEVC *encode* entry point at all (`hw::HwEncoder` opens H.264
        // and AV1), so there is no seat to probe. Said here rather than left to
        // a failed open, which would read as a driver refusal.
        format if format.is_hevc() => None,
        // Opt-in only, for the reason [`Enc::open_av1`] states in full. Both
        // AV1 formats sit on it: the container is not the encoder's business.
        format if format.is_av1() && !forced("VE_HW_AV1") => None,
        format if format.is_av1() => {
            HwEncoder::open_av1(meta.width, meta.height, fps_num, fps_den, bitrate)
        }
        _ => HwEncoder::open(meta.width, meta.height, fps_num, fps_den, bitrate),
    }
}

/// How a front-end names a video seat: which of the two encoders has the file,
/// and which library that is. Not the codec -- a caller shows this beside the
/// format it picked, and `rav1e` names AV1 as `rusty_h264` names H.264. One
/// place, so what a card says before an export and what its progress line says
/// during one cannot drift apart.
fn video_label(format: Format, hw: bool) -> &'static str {
    match (format, hw) {
        (_, true) => "HW encode (VA-API)",
        (Format::Hevc | Format::HevcMp4, false) => "SW encode (oxideav-h265 intra)",
        (Format::Av1 | Format::Av1Mp4, false) => "SW encode (rav1e)",
        (_, false) => "SW encode (rusty_h264)",
    }
}

/// The cheap half of "does this timeline's sound have to be re-encoded": what
/// the *edit* says, with no file opened. Two lanes are a mix and a mix is not a
/// copy; a speeded lane is resampled; an equalizer is sample math. The other
/// half -- whether the sources can be copied out of at all -- costs a file open
/// each and belongs to [`copy_audio`], which may re-encode where this says copy.
fn forces_encode(project: &Project) -> bool {
    let lanes: Vec<_> = project
        .audio_lanes()
        .into_iter()
        .filter(|&lane| !project.lane(lane).is_empty())
        .collect();
    lanes.len() > 1
        || lanes
            .iter()
            .any(|&lane| project.lane(lane).iter().any(|c| !c.speed.is_normal()))
        || equalized(project)
}

/// What the sound is written by. Every audio encoder here is software -- there
/// is no hardware AAC, PCM, FLAC or MP3 seat -- so this names *which* one, and
/// whether a container escapes encoding altogether by copying the packets it was
/// given. Both AV1 formats are in it like the mp4: an AV1 export carries the
/// timeline's sound too, so leaving it unnamed would be the old lie.
///
/// A prediction, and it says so: `copied` is what [`copy_audio`] *did*, which a
/// card asking before the export has no way to know (a source that cannot be
/// copied out of is only found by opening it). `None` predicts from the edit
/// alone.
fn audio_label(
    project: &Project,
    format: Format,
    sound: bool,
    copied: Option<bool>,
) -> &'static str {
    if !sound {
        return "no sound to write";
    }
    match format {
        Format::Wav => "PCM · SW (hound)",
        Format::Flac => "FLAC · SW (flacenc)",
        Format::Mp3 => "MP3 · SW (rusty_mp3)",
        _ => match copied.unwrap_or(!forces_encode(project)) {
            true => "AAC copy",
            false => "AAC · SW encode (rusty_aac)",
        },
    }
}

/// What [`start`] would write this timeline's sound with, before one is
/// started. Pure: no probe and no file opened, so a card may ask it per repaint.
pub fn planned_audio(project: &Project, format: Format) -> &'static str {
    audio_label(project, format, has_sound(project), None)
}

/// What [`start`] would encode the picture with, probed the way the export
/// probes it -- the very encoder is opened and closed again -- so this is a
/// measurement and not a promise. `None` for a format that carries no picture.
///
/// Costs that open (~100 ms here): ask it off a render thread and keep the
/// answer until the format, the resolution or the bitrate changes.
pub fn planned_video(meta: &VideoMeta, settings: &ExportSettings) -> Option<&'static str> {
    settings
        .format
        .has_video()
        .then(|| video_label(settings.format, hw_seat(meta, settings).is_some()))
}

fn settle(shared: &Shared, result: crate::Result<()>) {
    *shared.outcome.lock().unwrap() = Some(result);
    // Published last: a caller that sees the flag is guaranteed the outcome.
    shared.finished.store(true, Ordering::Release);
}

/// The audio track an export writes, and how it was made -- a copy is the
/// source's own packets, bit for bit, and anything else is this project's
/// encoder over the decoded mix. [`run`] publishes which of the two really
/// happened, so the encoder line a user reads is measured and not promised.
pub(crate) struct ExportAudio {
    pub params: crate::AacTrackParams,
    pub packets: Vec<crate::AacPacket>,
    /// Whether those packets are the source's, untouched.
    pub copied: bool,
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
fn copy_audio(project: &Project, meta: &VideoMeta) -> crate::Result<Option<ExportAudio>> {
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
    // A frame *rate* of its own is not one of those, and that is deliberate: do
    // not "fix" this into a re-encode. A clip counts the **timeline's** frames
    // whatever its file was shot at ([`crate::project::Rate`]), so the window
    // this copy trims a source's sound to is `start..end` in whole timeline
    // frames -- exactly the seconds the picture covers -- and the file is read
    // at the pitch it was recorded at, which is what playing at the rate it was
    // shot at means. There is no per-clip stretch to express, so the packets are
    // already the right sound in the right place; re-encoding a conformed lane
    // would buy nothing and cost a generation of loss on every mixed-rate export
    // (`tests/mixed_fps.rs`, `an_export_of_two_rates_runs_at_the_speed_it_was_shot`
    // measures the copied sound against the picture). A *speed* is different
    // because it resamples, which is why it leaves through the re-encode above.
    //
    // Only these: a timeline nobody has touched still leaves through the copy
    // below, packet for packet, so no passthrough quietly becomes a generation
    // of loss.
    let speeded = project.lane(lane).iter().any(|c| !c.speed.is_normal());
    if speeded || equalized(project) {
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
        Ok(copied) => Ok(copied.map(|(params, packets)| ExportAudio {
            params,
            packets,
            copied: true,
        })),
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
fn encode_audio(project: &Project, meta: &VideoMeta) -> crate::Result<Option<ExportAudio>> {
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
    Ok(Some(ExportAudio {
        params: crate::AacTrackParams {
            freq_index,
            // 1 and 2 are the ISO channel configurations for mono and stereo,
            // and the opener refuses anything wider than stereo already.
            chan_conf: audio.channels.max(1) as u8,
            sample_rate: audio.sample_rate,
        },
        packets,
        copied: false,
    }))
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
    let audio_params = audio.as_ref().map(|track| AudioParams {
        freq_index: track.params.freq_index,
        chan_conf: track.params.chan_conf,
        sample_rate: track.params.sample_rate,
    });
    // What the track *is*, kept before the packets are handed to a muxer: the
    // encoder line names a copy or an encode and only `copy_audio` knows which.
    let sound = audio.as_ref().map(|track| track.copied);
    // Taken by the Matroska muxer at creation; the mp4 one writes its track
    // after the picture, so for that path this is still `Some` at the end.
    let mut packets = audio.map(|track| track.packets);

    // One header parse per source, before any of them is decoded: what rate
    // each file was shot at against the timeline's ([`Rate`]), which is how the
    // clip's timeline frames below become frames of the file. Real time for a
    // still, a song and a single-rate project, where every conversion below is
    // the identity.
    let rates: Vec<Rate> = sources
        .iter()
        .map(|source| source_rate(&source.path, meta.frame_rate))
        .collect();

    let mut encoder = Enc::open(meta, settings)?;
    // What this file is really being written by, for a progress line to name.
    // Published *after* the seat is open, so a hardware encoder the driver
    // refused reads as the software one that took over rather than as the hope
    // it replaced -- and the sound says whether it was copied or encoded, which
    // `copy_audio` decided a few lines up.
    *shared.encoders.lock().unwrap() = Some(format!(
        "{} · {}",
        encoder.label(),
        audio_label(project, settings.format, sound.is_some(), sound)
    ));
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
        let (mut pictures, rate, in_frame) = match span.from {
            Some((source, in_frame)) => {
                let entry = sources
                    .get(source)
                    .ok_or_else(|| format!("clip names source {source} of {}", sources.len()))?;
                let rate = rates[source];
                // Opened at the file's own frame, which is the only place the
                // file's numbering is used -- the span's are the timeline's.
                let pictures = ClipDecoder::open(&entry.path, rate.source_at(in_frame))?;
                (Some(pictures), rate, in_frame)
            }
            None => (None, Rate::REAL_TIME, 0),
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
        // The whole of the mapping, in the order it composes: the speed says
        // which of the clip's (timeline-rate) frames a timeline frame shows,
        // and the rate says which frame of the *file* that is -- counted from
        // the one the decoder was opened at. At real time and at the timeline's
        // own rate this is `Speed::source_at` and nothing else, which is the
        // walk a single-rate project has always taken.
        let opened_at = rate.source_at(in_frame);
        let source_at =
            |offset: u32| rate.source_at(in_frame + span.speed.source_at(offset)) - opened_at;
        while done_here < span.len {
            cancelled(shared)?;
            let want = source_at(done_here);
            // How many timeline frames from here show that same picture: one at
            // real time, more when the clip is slowed or its file is slower
            // than the timeline. Encoded again rather than decoded twice.
            let repeats = (done_here..span.len)
                .take_while(|&offset| source_at(offset) == want)
                .count() as u32;
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

/// What rate `path` was shot at against a timeline at `timeline_fps`
/// ([`Rate`]). [`Rate::REAL_TIME`] for the files that have no rate of their own
/// -- a still, a song -- and for one that will not open here: the decoder is
/// opened a few lines later and fails the export by name, which is a better
/// error than this one could raise.
fn source_rate(path: &Path, timeline_fps: f64) -> Rate {
    if crate::is_image(path) || crate::is_audio(path) {
        return Rate::REAL_TIME;
    }
    match Demuxer::open(path) {
        // ...and one whose rate cannot be named against the timeline's, which
        // `matches_timeline` refuses at import, so nothing on a timeline is on
        // this arm either.
        Ok((meta, _)) => Rate::from_fps(meta.frame_rate, timeline_fps).unwrap_or(Rate::REAL_TIME),
        Err(_) => Rate::REAL_TIME,
    }
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
    // The picture path's line, for a file that is sound alone: there is sound
    // by the line above, so this names the encoder writing it.
    *shared.encoders.lock().unwrap() = Some(audio_label(project, format, true, None).to_string());
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
        _ => unreachable!("the picture formats are `run`"),
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
    if settings.format.is_hevc() {
        // The parameter sets are in the access unit -- every intra AU carries
        // VPS, SPS and PPS -- and the record built from them is what *both*
        // containers declare the track with: an mp4's `hvcC` box and a
        // Matroska `CodecPrivate` are the same bytes, so the two files are one
        // stream written twice rather than two.
        let sample =
            crate::mux::annex_b_to_hvcc(au).ok_or("a coded HEVC unit with no slice in it")?;
        if settings.format == Format::Hevc {
            let muxer = match muxer {
                Some(Muxer::Mkv(mkv)) => mkv,
                Some(Muxer::Mp4(_)) => unreachable!("the format picks the muxer once"),
                none => {
                    let hvcc = hevc_params(au)?;
                    let sound = audio.zip(packets.take());
                    let Muxer::Mkv(mkv) = none.insert(Muxer::Mkv(MkvMuxer::create_hevc(
                        out,
                        &HevcParams {
                            width: meta.width,
                            height: meta.height,
                            frame_rate: meta.frame_rate,
                            hvcc: &hvcc,
                        },
                        sound,
                    )?)) else {
                        unreachable!("just inserted a Matroska muxer")
                    };
                    mkv
                }
            };
            return muxer.write_frame(&sample, key);
        }
        let muxer = match muxer {
            Some(Muxer::Mp4(mp4)) => mp4,
            Some(Muxer::Mkv(_)) => unreachable!("the format picks the muxer once"),
            none => {
                let hvcc = hevc_params(au)?;
                let Muxer::Mp4(mp4) = none.insert(Muxer::Mp4(Mp4Muxer::create_hevc(
                    out,
                    &HevcParams {
                        width: meta.width,
                        height: meta.height,
                        frame_rate: meta.frame_rate,
                        hvcc: &hvcc,
                    },
                    audio,
                )?)) else {
                    unreachable!("just inserted an mp4 muxer")
                };
                mp4
            }
        };
        return muxer.write_coded_sample(&sample, key);
    }
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
        return muxer.write_coded_sample(au, key);
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

/// The `hvcC` record of the first coded unit, or the refusal by name. Split out
/// because both HEVC containers ask it in the same words.
fn hevc_params(au: &[u8]) -> crate::Result<Vec<u8>> {
    crate::mux::hvcc_record(au)
        .ok_or_else(|| crate::Error::from("the first coded picture carries no HEVC VPS/SPS/PPS"))
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
    /// HEVC, intra-only and software only: `encode_idr_intra_au_cropped` is a
    /// *stateless* function of one picture, so a batch of frames is coded on as
    /// many cores as there are lanes and collected back in order.
    Hevc(HevcEnc),
}

/// The intra HEVC seat. Frames arrive one at a time (the export walk is one
/// picture per timeline frame) and are held until there are [`lanes`] of them,
/// then coded in parallel and queued in **display order** -- fanning out is the
/// only thing that makes a pure-Rust HEVC export bearable (4.30 fps at 1080p on
/// 12 lanes against 0.55 fps on one, measured 2026-08-11), and an export whose
/// frames came back shuffled would be no export at all.
///
/// ponytail: a batch of padded planes sits in memory (~3 MB a frame at 1080p,
/// so ~37 MB at 12 lanes) and the tail of a timeline is coded on fewer lanes
/// than the middle. Upgrade path is a pipeline that keeps every lane fed from a
/// decoder running ahead of the encoder rather than a batch barrier.
struct HevcEnc {
    /// Padded I420 planes waiting for a lane, in display order.
    pending: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
    /// Coded access units not yet collected, in the same order.
    ready: std::collections::VecDeque<Vec<u8>>,
    /// The unit currently lent out, for the reason `Sw` owns one.
    au: Vec<u8>,
    /// The *coded* size: the picture's, rounded up to the 16-sample CTB grid.
    width: usize,
    height: usize,
    /// Luma samples the conformance window crops off the right and the bottom,
    /// so a decoder outputs the picture's own size ([`pad_to_ctb`]).
    crop: (usize, usize),
    qp: i32,
    lanes: usize,
}

impl Enc {
    /// Which seat this *is*, said the way [`video_label`] says it -- so the
    /// name a running export shows is the name the card showed, or the honest
    /// difference where the probe and the open disagreed.
    fn label(&self) -> &'static str {
        match self {
            Self::Hw(_) => video_label(Format::Mp4, true),
            Self::Sw { .. } => video_label(Format::Mp4, false),
            Self::Av1Hw(_) => video_label(Format::Av1, true),
            Self::Av1Sw { .. } => video_label(Format::Av1, false),
            Self::Hevc(_) => video_label(Format::Hevc, false),
        }
    }

    fn open(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Self> {
        // Which *codec* the picture is, not which container it goes in: both AV1
        // formats run the same encoder, and so do both HEVC ones.
        if settings.format.is_av1() {
            return Self::open_av1(meta, settings);
        }
        if settings.format.is_hevc() {
            return Self::open_hevc(meta, settings);
        }
        // A caller's number goes through the same clamp as the computed one: a
        // zero bitrate switches the software encoder's lookahead on, which would
        // break the one-picture-per-call contract `encode` documents below.
        let bitrate = bitrate_of(meta, settings);
        // The plugin wants an exact rational, and the muxer already picks one
        // that is exact at every rate we can read -- `fps * 1000 / 1000` would
        // hand 24000/1001 over as a rounded 23.976, which is the same
        // truncation the container timing had.
        crate::mux::frame_timing(meta.frame_rate)?;
        if let Some(hw) = hw_seat(meta, settings) {
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
        let bitrate = bitrate_of(meta, settings);
        let (fps_num, fps_den) = crate::mux::frame_timing(meta.frame_rate)?;
        // Two seconds between keyframes, as the H.264 seat does: a seek may only
        // land on one, and this is what a cluster of the Matroska file is.
        let gop = (meta.frame_rate * 2.0).round().max(1.0) as u64;
        if let Some(hw) = hw_seat(meta, settings) {
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

    /// The intra HEVC seat, software and frame-parallel. There is no hardware
    /// half: the plugin encodes H.264 and AV1 and nothing else, which
    /// [`hw_seat`] says by refusing to probe one.
    ///
    /// **Intra-only, deliberately.** The vendored encoder's inter modes code
    /// 1080p at 0.81 fps across 12 cores (measured 2026-08-11) -- a minute of
    /// timeline in half an hour, which is not an export anybody waits for --
    /// while the intra path does 4.30 fps on the same picture and the same
    /// cores. So every frame is an IDR, the file is an intraframe master in the
    /// shape of ProRes or DNxHD, and the card says so where a user picks it.
    ///
    /// The picture is coded **padded** to the 16-sample CTB grid and the SPS
    /// crops it back ([`pad_to_ctb`], the vendored conformance-window patch), so
    /// 1920x1080 is a legal export rather than a refusal.
    fn open_hevc(meta: &VideoMeta, settings: &ExportSettings) -> crate::Result<Self> {
        // The muxers time by it and the mp4 one wants an exact rational: asked
        // here so an impossible rate fails before a frame is coded.
        crate::mux::frame_timing(meta.frame_rate)?;
        // 4:2:0 addresses its conformance window in *chroma* samples, so an odd
        // picture cannot be cropped back to itself -- and an odd dimension has
        // no chroma plane of its own to begin with. Named rather than padded to
        // something a decoder would then show a column of.
        if meta.width % 2 != 0 || meta.height % 2 != 0 {
            return Err(format!(
                "{}x{} cannot be written as HEVC: 4:2:0 needs even dimensions",
                meta.width, meta.height
            )
            .into());
        }
        let (width, height) = (
            (meta.width as usize).next_multiple_of(CTB),
            (meta.height as usize).next_multiple_of(CTB),
        );
        let lanes = std::thread::available_parallelism().map_or(1, |n| n.get());
        eprintln!("export encoder: software HEVC intra (oxideav-h265)");
        Ok(Self::Hevc(HevcEnc {
            pending: Vec::new(),
            ready: std::collections::VecDeque::new(),
            au: Vec::new(),
            width,
            height,
            crop: (width - meta.width as usize, height - meta.height as usize),
            qp: hevc_qp(meta, settings),
            lanes: lanes.min(HEVC_LANES),
        }))
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
            Self::Hevc(hevc) => {
                hevc.pending.push(pad_to_ctb(
                    y,
                    u,
                    v,
                    width as usize,
                    height as usize,
                    hevc.width,
                    hevc.height,
                ));
                if hevc.pending.len() >= hevc.lanes {
                    hevc.code_batch()?;
                }
                // Every intra AU is an IDR, so every one of them is a key
                // frame -- which is what an intraframe master means.
                Ok(pop_hevc(hevc))
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
            Self::Hevc(hevc) => {
                // The tail batch: fewer frames than lanes, coded on as many
                // cores as there are frames left.
                if !hevc.pending.is_empty() {
                    hevc.code_batch()?;
                }
                Ok(pop_hevc(hevc))
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

impl HevcEnc {
    /// The whole pending batch, one frame per lane, collected back in the order
    /// it went out -- `std::thread::scope`, because the encode borrows the
    /// planes rather than copying them again. A panic inside a lane comes back
    /// as an error rather than unwinding the export thread.
    fn code_batch(&mut self) -> crate::Result<()> {
        let (width, height, qp, crop) = (self.width, self.height, self.qp, self.crop);
        let coded: Vec<crate::Result<Vec<u8>>> = std::thread::scope(|scope| {
            let lanes: Vec<_> = self
                .pending
                .iter()
                .map(|(y, cb, cr)| {
                    scope.spawn(move || {
                        oxideav_h265::encoder::intra::encode_idr_intra_au_cropped(
                            y, cb, cr, width, height, qp, crop.0, crop.1,
                        )
                        .map(|coded| coded.au)
                        .map_err(|e| crate::Error::from(format!("HEVC intra encode: {e:?}")))
                    })
                })
                .collect();
            lanes
                .into_iter()
                .map(|lane| {
                    lane.join()
                        .unwrap_or_else(|_| Err("an HEVC encode lane panicked".into()))
                })
                .collect()
        });
        for au in coded {
            self.ready.push_back(au?);
        }
        self.pending.clear();
        Ok(())
    }
}

/// The oldest coded intra AU, lent out under the same "valid until the next
/// call" contract [`pop_av1`] has. Every one of them is an IDR, so the key flag
/// is not a guess.
fn pop_hevc(hevc: &mut HevcEnc) -> Option<(&[u8], bool)> {
    hevc.au = hevc.ready.pop_front()?;
    Some((&hevc.au[..], true))
}

/// One I420 picture copied onto the padded plane sizes the CTB grid needs, the
/// added rows and columns filled by **replicating the edge** rather than with
/// black: the padding is coded and then cropped away, and a black border would
/// bleed into the last real column through the intra prediction and the
/// transform that straddles it.
fn pad_to_ctb(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
    padded_w: usize,
    padded_h: usize,
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let plane = |src: &[u8], w: usize, h: usize, pw: usize, ph: usize| {
        let mut out = vec![0u8; pw * ph];
        for row in 0..ph {
            let src_row = &src[row.min(h - 1) * w..][..w];
            let dst = &mut out[row * pw..][..pw];
            dst[..w].copy_from_slice(src_row);
            dst[w..].fill(src_row[w - 1]);
        }
        out
    };
    (
        plane(y, width, height, padded_w, padded_h),
        plane(u, width / 2, height / 2, padded_w / 2, padded_h / 2),
        plane(v, width / 2, height / 2, padded_w / 2, padded_h / 2),
    )
}

/// The quantiser an intra HEVC export codes at, mapped from the *bitrate* row a
/// user picked -- the card has no QP control and this codec has no rate control,
/// so the two are joined by measurement rather than promised to each other.
///
/// The encoder codes ~0.6 bits per pixel at QP 27 ([`HEVC_BPP_AT_27`], measured
/// on the 720p and 1080p benches to within 4% of each other), and a step of 6 QP
/// is a factor of two in rate, which is the classic HEVC relation. So the QP
/// that would land on a target is `27 + 6 * log2(0.6 / target)` -- and it is
/// clamped, because this is a *quality* dial and not a rate controller: below QP
/// 22 the file grows without a picture to show for it, and past QP 40 an
/// intra-only frame goes blocky in a way no bitrate row asked for. A 1080p30
/// timeline at the automatic 6.2 Mbps therefore codes at QP 40 and comes out
/// bigger than that, which is what "intra-only, large files" on the format row
/// means: the row buys quality, not a size.
fn hevc_qp(meta: &VideoMeta, settings: &ExportSettings) -> i32 {
    let pixels = f64::from(meta.width) * f64::from(meta.height) * meta.frame_rate;
    let target = bitrate_of(meta, settings) as f64 / pixels.max(1.0);
    let qp = 27.0 + 6.0 * (HEVC_BPP_AT_27 / target.max(f64::MIN_POSITIVE)).log2();
    (qp.round() as i32).clamp(HEVC_QP_MIN, HEVC_QP_MAX)
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

    fn meta(width: u32, height: u32) -> VideoMeta {
        VideoMeta {
            width,
            height,
            frame_rate: 30.0,
            frame_count: 1,
            codec: crate::demux::Codec::H264,
        }
    }

    /// The bitrate row a user picked, read as the quality tier it is: a higher
    /// row is a lower QP, the band is honoured at both ends, and the same
    /// picture at the same rate gets the same number whatever its size (the
    /// mapping is bits per *pixel*, which is what makes 720p and 1080p one
    /// table).
    #[test]
    fn the_quality_rows_map_onto_quantisers_that_only_go_one_way() {
        let at = |width, height, mbps: Option<u64>| {
            hevc_qp(
                &meta(width, height),
                &ExportSettings {
                    bitrate: mbps.map(|m| m * 1_000_000),
                    ..Default::default()
                },
            )
        };
        // 20 Mbps at 1080p30 is 0.32 bits a pixel against the 0.6 measured at
        // QP 27, which is just over half: five QP up, and QP is not an
        // arithmetic scale, so this is the number the formula gives.
        assert_eq!(at(1920, 1080, Some(20)), 32);
        // Every step down the card is a step up in QP, never sideways.
        let rows: Vec<i32> = [20, 12, 8, 4, 2, 1]
            .into_iter()
            .map(|mbps| at(1920, 1080, Some(mbps)))
            .collect();
        assert!(
            rows.windows(2).all(|w| w[0] <= w[1]),
            "a lower bitrate row must never code at a lower QP: {rows:?}"
        );
        assert_eq!(*rows.last().unwrap(), HEVC_QP_MAX, "the floor is clamped");
        // ...and the top of the range is the clamp, not an unbounded number: 20
        // Mbps at 320x240 is 8.7 bits a pixel, which no picture needs.
        assert_eq!(at(320, 240, Some(20)), HEVC_QP_MIN);
        // The same bits per pixel is the same quantiser at any size: 1080p at
        // 20 Mbps and 720p at 8.9 Mbps are one picture quality.
        assert_eq!(at(1920, 1080, Some(20)), at(1280, 720, Some(9)));
        // The automatic bitrate is 0.1 bpp at every size, so it is one QP
        // everywhere -- the honest end of "intra-only, large files": the file
        // will be bigger than the row says, and the row buys quality.
        assert_eq!(at(1920, 1080, None), at(1280, 720, None));
    }

    /// The padding a non-%16 picture is coded with replicates the edge instead
    /// of filling black: the coded rows past the picture carry its last row, so
    /// nothing bleeds into the last real line through the intra prediction.
    #[test]
    fn the_ctb_padding_replicates_the_edge() {
        // 6x2 luma, values 1..=12, padded to 8x4 (CTB is 16, so this checks the
        // copy itself with numbers small enough to read).
        let y: Vec<u8> = (1..=12).collect();
        let u: Vec<u8> = vec![40, 41, 42];
        let v: Vec<u8> = vec![50, 51, 52];
        let (py, pu, pv) = pad_to_ctb(&y, &u, &v, 6, 2, 8, 4);
        assert_eq!(
            py,
            vec![
                1, 2, 3, 4, 5, 6, 6, 6, // the row, then its last sample twice
                7, 8, 9, 10, 11, 12, 12, 12, //
                7, 8, 9, 10, 11, 12, 12, 12, // the last row, replicated
                7, 8, 9, 10, 11, 12, 12, 12,
            ]
        );
        // Chroma is half of everything, padded the same way.
        assert_eq!(pu, vec![40, 41, 42, 42, 40, 41, 42, 42]);
        assert_eq!(pv, vec![50, 51, 52, 52, 50, 51, 52, 52]);
    }

    /// An odd picture cannot be cropped back to itself in 4:2:0 -- the
    /// conformance window is stated in chroma samples -- so it is refused by
    /// name rather than written a column wider than the timeline.
    #[test]
    fn an_odd_picture_is_refused_an_hevc_export_by_name() {
        let settings = ExportSettings {
            format: Format::Hevc,
            ..Default::default()
        };
        let refused = Enc::open(&meta(1919, 1080), &settings)
            .err()
            .expect("an odd width has no chroma plane")
            .to_string();
        assert!(refused.contains("1919x1080"), "{refused}");
        assert!(refused.contains("even"), "{refused}");
        // ...and the even one it neighbours opens.
        assert!(Enc::open(&meta(1920, 1080), &settings).is_ok());
    }
}
