//! Export: the edit list rendered back out as one mp4 (H.264, AV1 or HEVC) or
//! one Matroska file (AV1 or HEVC) — or, picture left behind, as one WAV, FLAC,
//! MP3 or Ogg Vorbis of the timeline's audio alone.
//!
//! The HEVC pair is **intra-only**, and says so wherever it is offered: every
//! frame is a self-contained IDR from the vendored `oxideav-h265`, which is the
//! only shape of a pure-Rust HEVC encode fast enough to wait for (its inter
//! modes code 1080p at 0.81 fps across 12 cores against the intra path's 4.30).
//! That makes it an intraframe master in the family of ProRes and DNxHD — large
//! files, every frame a cut point — and not a delivery codec.
//!
//! Picture and sound are both **copied wherever a copy says exactly what the
//! timeline says** — a copy is exact, free and never a generation of loss. For
//! the sound that is packet for packet ([`copy_audio`]); for the picture it is
//! block for block out of a Matroska source into a Matroska file ([`CopyPlan`]),
//! which is what makes a cut in a two-hour film cost the reading of it rather
//! than the re-encoding of it. The picture's copy is all-or-nothing per export,
//! because a copied span and an encoded one cannot share one track: what it
//! needs of a cut is that it lands on a sync point, and everything else — a
//! grade, a speed, a gap, a source of another size or codec, an mp4 — goes
//! through the encoder exactly as it always did. Where the *sound* cannot be
//! copied — a second audio lane to mix in, a speeded clip, an
//! equalized one, a source that is not AAC inside an mp4's own sample table —
//! the sound is decoded, mixed and encoded again with `rusty_aac`
//! ([`encode_audio`]). The audio-only formats have no such split: `hound` writes
//! PCM, `flacenc` encodes FLAC, `rusty_mp3` encodes MP3 and `rusty_vorbis`
//! encodes Vorbis, so those are always *decoded* out of the timeline. The worker owns everything: the caller gets an
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
//! picture. The audio-only formats are decoded and resampled, so they honour a
//! rate like the picture does.
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
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use rusty_h264::{Decoder, Encoder, EncoderConfig, Preset, YuvFrame};

use flacenc::component::BitRepr;
use flacenc::error::Verify;
// The trait the Ogg muxer's `write_header` / `write_packet` / `write_trailer`
// live on -- `oxideav_ogg::mux::open_concrete` hands back the concrete muxer
// (the only way to reach `set_page_target_bytes`), so the trait has to be here.
use oxideav_core::Muxer as _;

use crate::audio::{AudioMeta, AudioSession};
use crate::colorspace::{ColorDescription, Matrix, Transfer};
use crate::demux::{Codec, Demuxer, MkvDemuxer, VideoMeta};
use crate::hw::{HwEncoder, HwSession};
use crate::mux::{
    AudioParams, Av1Params, CopyParams, HevcParams, MkvMuxer, Mp4Muxer, SubParams, VideoParams,
};
use crate::project::{LaneKind, Project, Rate};
use crate::scale::Composer;
use crate::subtitle::{Cue, SubtitleTrack};
use crate::tonemap::{self, ToneMapper};

/// Progress is reported in permille: an atomic integer the render loop can read
/// without a lock, fine enough for any progress bar.
const PROGRESS_SCALE: u32 = 1_000;

/// The head of the bar the *sound* owns, before a picture is written at all.
/// The mix is decoded, encoded and measured before the muxer exists ([`run`]
/// says why), and on a feature film that is a minute or two in which a bar
/// pinned at zero is the only thing on screen -- which reads as an export that
/// has hung. Small, because the picture is still nearly all of the work.
const AUDIO_BAND: u32 = PROGRESS_SCALE / 20;

/// Where the bar stands with `done` of `total` pictures written: the sound's
/// band is already behind it ([`AUDIO_BAND`]), so the picture fills what is
/// left rather than starting again from zero.
fn picture_progress(done: u32, total: u32) -> u32 {
    AUDIO_BAND + done.min(total) * (PROGRESS_SCALE - AUDIO_BAND) / total.max(1)
}

/// D2 rate control: bits per pixel per second, then a sane range. 720p30 lands
/// at 2.76 Mbps, which S1 measured the software encoder hitting within 1%.
const BITS_PER_PIXEL: f64 = 0.1;
const MIN_BITRATE: u64 = 1_000_000;
const MAX_BITRATE: u64 = 20_000_000;

/// The rates the sound may be coded at, smallest first -- what a front-end
/// offers and what [`ExportSettings::audio_kbps`] is clamped into. Every one of
/// them is a legal MPEG-1 Layer III value, so the MP3 path writes the figure it
/// was given rather than the nearest one `rusty_mp3` snaps to.
pub const AUDIO_KBPS: [u32; 4] = [128, 192, 256, 320];

/// What the sound is coded at when nobody picked: the rate this project wrote
/// before the choice existed. 256 and not the encoders' own 128, because an
/// export is a master a person edits from again and this is a second lossy
/// generation over sources that may already have been one -- measured on the
/// suite's fixtures, an untouched clip re-encoded moves 0.15 dB here against
/// 0.46 dB at 128, and 32 KB/s beside a 2.76 Mbps picture is not a size anyone
/// is counting.
pub const DEFAULT_AUDIO_KBPS: u32 = 256;

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
/// WAV, FLAC, MP3 and Ogg Vorbis are the standalone audio formats with a
/// pure-Rust encoder: `hound`, `flacenc`, `rusty_mp3` and `rusty_vorbis`. Opus
/// still has none -- `oxideav-opus` encodes CELT alone, which is half a codec.
/// (AAC has one too -- `rusty_aac`, which is what a re-encoded video track's
/// sound leaves through -- but AAC is a container's own audio, never a file of
/// its own here.) VP9 is the
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
    /// The audio lanes alone, MPEG-1 Layer III CBR (`rusty_mp3`) at the rate
    /// [`ExportSettings::audio_kbps`] asks for.
    Mp3,
    /// The audio lanes alone, Vorbis I in an Ogg container -- `rusty_vorbis`
    /// encoding, `oxideav-ogg` paging. Quality-coded and not rate-coded, which
    /// is why [`audio_rate_refusal`] takes the Sound row away here: see
    /// [`VORBIS_QUALITY`] for the operating point and how it was measured.
    Ogg,
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
            Self::Ogg => "ogg",
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

    /// Whether the file is a Matroska one. Only that container carries this
    /// project's Opus track ([`encode_opus`]): an mp4 may hold Opus by spec, but
    /// the crate that writes the mp4s here has no sample entry for it, so the
    /// mp4 formats stay on AAC and say so.
    fn is_mkv(self) -> bool {
        matches!(self, Self::Av1 | Self::Hevc)
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
            Self::Ogg => "an Ogg Vorbis",
        }
    }
}

/// What the caller gets to decide about the output. The codec is not among it:
/// [`Format`] names a container *and* what goes in it, because every pair we
/// can write is a pair we can also read back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// What the *sound* is coded at, in kbps, wherever an encoder writes it --
    /// the AAC of a video export as much as an MP3. `None` is
    /// [`DEFAULT_AUDIO_KBPS`], which is what every caller wrote before there
    /// was a choice, so a settings value nobody touched writes the same bytes
    /// it used to. Means nothing to WAV and FLAC, which code no rate, and
    /// nothing to a *copied* AAC track, which carries its source's.
    pub audio_kbps: Option<u32>,
    /// Which of the project's subtitle tracks travel with the file: indices into
    /// [`Project::subtitles`], which is the same list a front-end picks rows of.
    /// As many as the timeline holds, in the order given -- the file declares
    /// them in that order and a player's subtitle menu lists them in it. Empty
    /// writes none -- what every caller wrote before there was a choice, so an
    /// export of a timeline without subtitles is the file it always was, byte
    /// for byte.
    ///
    /// Every container that carries picture carries them: Matroska as
    /// `S_TEXT/UTF8` blocks, an mp4 as a `tx3g` timed-text track
    /// ([`crate::mux::Mp4Muxer::write_subtitles`]). A Matroska block names its
    /// track in one byte, so a Matroska file carries at most
    /// [`crate::mux::MAX_SUB_TRACKS`] of them and asking for more is refused by
    /// name rather than truncated; an mp4 numbers its tracks in 32 bits and has
    /// no such ceiling.
    pub subtitles: Vec<usize>,
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
    let settings = settings.clone();
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
            _ => run_audio(&project, &meta, &part, &worker, &settings),
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

/// Whether the *mix* is doing something -- a lane turned off unity, or a
/// limiter in circuit. [`equalized`]'s pair, and a copy cannot carry either of
/// them: both live where the lanes are summed
/// ([`AudioSession::open_mixed_streams_master`]), and a copied packet was never
/// summed at all. An mp4 whose timeline is mixed is decoded and re-encoded.
fn mastered(project: &Project) -> bool {
    project.limiter().is_active() || project.audio_gains().iter().any(|&g| g != 1.0)
}

/// The bitrate an export codes at: the caller's number through the same clamp
/// as the computed one, for the reason [`Enc::open`] states.
fn bitrate_of(meta: &VideoMeta, settings: &ExportSettings) -> u64 {
    settings
        .bitrate
        .map_or_else(|| bitrate_for(meta), |b| b.clamp(MIN_BITRATE, MAX_BITRATE))
}

/// The rate the sound codes at: the caller's pick clamped into the offered
/// range, [`DEFAULT_AUDIO_KBPS`] where there was none. Clamped and not refused,
/// for [`bitrate_of`]'s reason -- a number out of range is a caller's slip and
/// not a file worth failing.
fn audio_kbps_of(settings: &ExportSettings) -> u32 {
    settings.audio_kbps.map_or(DEFAULT_AUDIO_KBPS, |k| {
        k.clamp(AUDIO_KBPS[0], AUDIO_KBPS[3])
    })
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
        || mastered(project)
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
        Format::Ogg => "Vorbis · SW (rusty_vorbis)",
        _ => match copied.unwrap_or(!forces_encode(project)) {
            true => "AAC copy",
            // A Matroska file gets Opus where the mix allows it -- 48 kHz and
            // stereo, which is every Opus source and most of everything else --
            // and AAC where it does not ([`encode_audio`]). Which of the two it
            // will be cannot be known here: both answers need a source opened,
            // and this function may be asked per repaint. So the card names the
            // codec it will nearly always be, as a prediction like the copy above
            // it, and the line the export publishes while it runs is the measured
            // one ([`run`]) -- it says AAC when the fallback took it, so the
            // fallback is visible rather than silent.
            //
            // It is one line and 76 characters wide (`summary_head` in the app
            // has the test), which is why the condition is in this comment
            // instead of in the string.
            false if format.is_mkv() => "Opus · SW encode (opus-rs)",
            false => "AAC · SW encode (rusty_aac)",
        },
    }
}

/// What the export *did* to the sound, for the line it publishes while it runs:
/// [`audio_label`]'s words with the one thing no prediction can know filled in
/// -- whether the Opus seat took the track or handed it back
/// ([`encode_opus`]'s gate).
///
/// `sound` is `Some(copied)` for a track and `None` for a file with no sound.
/// Naming Opus for *every* Matroska encode is what made that fallback
/// invisible: a track the gate refused was written as AAC and the line still
/// said Opus, on the one path where the difference is audible.
fn measured_audio_label(
    project: &Project,
    format: Format,
    sound: Option<bool>,
    opus: bool,
) -> &'static str {
    match (sound, opus) {
        (_, true) => "Opus · SW encode (opus-rs)",
        // Encoded, into a container that would have carried Opus, and it is not
        // Opus: the fidelity gate sent it to AAC ([`OPUS_MIN_FIDELITY`]), or the
        // mix was not the 48 kHz stereo that seat is measured in.
        (Some(false), false) if format.is_mkv() => "AAC · SW encode (rusty_aac)",
        _ => audio_label(project, format, sound.is_some(), sound),
    }
}

/// What [`start`] would write this timeline's sound with, before one is
/// started. Pure: no probe and no file opened, so a card may ask it per repaint.
pub fn planned_audio(project: &Project, format: Format) -> &'static str {
    audio_label(project, format, has_sound(project), None)
}

/// Why the sound's rate is nothing this timeline can be asked about in this
/// format, or `None` where it is a real choice: there is no sound to write, the
/// format codes the samples themselves, or the packets are the source's own and
/// carry the rate *it* was coded at. Pure, like [`planned_audio`], and a
/// prediction in the same way -- a copy that turns out impossible (a source no
/// mp4 sample table holds) falls through to [`encode_audio`], which codes at
/// [`ExportSettings::audio_kbps`] like every other encode. So the value travels
/// even where this refuses; what it cannot do is silently write a rate nobody
/// picked.
pub fn audio_rate_refusal(project: &Project, format: Format) -> Option<&'static str> {
    match format {
        _ if !has_sound(project) => Some("no sound to write"),
        Format::Wav | Format::Flac => Some("lossless — the samples themselves, at no rate"),
        // Vorbis has a quality knob and no rate knob, and the two are not the
        // same control wearing different words: measured across this suite's
        // fixtures, the whole usable quality band lands between 55 and 175 kbps
        // ([`VORBIS_QUALITY`]), so two of the four rates on offer here are not
        // rates it can reach at all. A live row would print a figure the file
        // does not hold, which is the one thing this card is built not to do.
        Format::Ogg => Some("quality-coded — Vorbis holds no rate to pick"),
        Format::Mp3 => None,
        _ if forces_encode(project) => None,
        _ => Some("the source's own packets are copied — at the rate they hold"),
    }
}

/// The tracks an export carries, in the order they were picked, and empty where
/// it carries none: no pick, a pick nothing answers, a track that could not be
/// read, one with no cue left on the exported timeline -- and every format that
/// is the sound alone, which has nowhere to put them. Filtered *per track*, so
/// one unusable pick costs its own row and not the others.
///
/// Both containers, since the mp4 muxer gained its `tx3g` track: what the two
/// do with the very same [`SubParams`] is theirs to say
/// ([`crate::mux::Mp4Muxer::write_subtitles`], [`MkvMuxer`]), and nothing here
/// asks which one is being written.
fn export_subtitles(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
) -> Vec<SubParams> {
    if !settings.format.has_video() {
        return Vec::new();
    }
    settings
        .subtitles
        .iter()
        .filter_map(|&pick| {
            let track = project.subtitles().get(pick)?;
            // A picture is not a line: the muxer writes an `S_TEXT/UTF8` track
            // and a PGS cue has no words to put in one. `planned_subtitles` says
            // so before the button is pressed rather than the file coming out
            // short.
            if track.refused.is_some() || track.is_bitmap() {
                return None;
            }
            let cues = timeline_cues(project, track, meta.frame_rate);
            // The container's own two fields, carried as two
            // ([`SubtitleTrack::language`]): a `TrackEntry` with no `Language`
            // means `eng` by spec, so a French track that arrives here as one
            // flattened string leaves as an English one.
            (!cues.is_empty()).then(|| SubParams {
                language: track.language.clone(),
                name: track.name.clone(),
                cues,
            })
        })
        .collect()
}

/// The track's cues where the *exported* timeline puts them, which is the only
/// clock the file has: the export writes timeline frame 0 onwards and nothing
/// else, exactly as the sound is mixed from frame 0 and resized to the
/// timeline's own length ([`encode_audio`]).
///
/// Which clock the cues arrive in is what the track itself says:
///
/// * a track out of a *file* (`SubtitleTrack::track`) is timed against that
///   file, so each cue is carried through the spans that play it -- a cue over
///   a stretch that was cut out is gone, one straddling a cut keeps the half
///   that is still there, and one after a rippled delete moves back with the
///   picture it belongs to. The *edges* a cut clips to are frames -- that is
///   what a cut is -- but a cue nothing clipped keeps its own microseconds, so
///   an untouched timeline exports the times the source states.
/// * a *standalone* file (`subs.srt`) is timed against nothing else -- there is
///   no clip to hang it on and no offset in the project to shift it by -- so it
///   is the timeline's own clock, which is where it was drawn while editing
///   (`Player::subtitle_overlay`). Clipped to the exported length and no more.
///
/// This is also what the *preview* draws: the plate over the picture and the
/// timeline strip both ask it through
/// [`PlaybackSession::timeline_cues`](crate::PlaybackSession::timeline_cues), so
/// what a rippled timeline shows and what its export writes are one answer by
/// construction rather than two maps kept in step by hand.
///
/// Pure and cheap enough to ask per repaint (a walk of the spans and of the
/// cues, no file opened), which is how a front-end asks it.
pub fn timeline_cues(project: &Project, track: &SubtitleTrack, fps: f64) -> Vec<Cue> {
    let frames = project.timeline_frames();
    let us = |frame: u32| (f64::from(frame) / fps * 1e6).round() as i64;
    let end = us(frames);
    // The file the cues are timed against, when there is one on this timeline.
    // A track whose media has since been dropped from the project has no span
    // to be carried through and falls back to the timeline's clock, which is
    // what it was drawn on.
    //
    // Through the same `canonical` a source is indexed by, or the two spellings
    // of one file (an argv `../assets/film.mkv`, a symlinked folder) would read
    // as two files and the cues would quietly fall back to the timeline's clock.
    let source = track.track.and_then(|_| {
        let path = crate::project::canonical(&track.path);
        project.sources().iter().position(|s| s.path == path)
    });
    let Some(source) = source else {
        return track
            .cues
            .iter()
            .filter_map(|cue| {
                let (start_us, end_us) = (cue.start_us.max(0), cue.end_us.min(end));
                (end_us > start_us).then(|| Cue {
                    start_us,
                    end_us,
                    text: cue.text.clone(),
                    image: cue.image.clone(),
                })
            })
            .collect();
    };
    let mut out = Vec::new();
    for span in project.composite_spans_from(0) {
        let Some((_, in_frame)) = span.from.filter(|&(from, _)| from == source) else {
            continue;
        };
        // What of the file this span plays, as *time in the file*: a clip counts
        // the timeline's frames whatever its file was shot at ([`Rate`]), which
        // is what makes those frames the same seconds a cue is timed in.
        let (first, last) = (us(in_frame), us(in_frame + span.source_len()));
        // Where that lands, and how fast: a cue is carried by the same factor
        // the pictures under it are, so a slowed clip stretches its subtitles
        // with it instead of leaving them behind.
        let start = us(span.start);
        let onto = |t: i64| start + ((t - first) as f64 / span.speed.as_f64()).round() as i64;
        for cue in &track.cues {
            let (a, b) = (cue.start_us.max(first), cue.end_us.min(last));
            if b <= a {
                continue; // wholly outside what this span kept
            }
            out.push(Cue {
                start_us: onto(a),
                end_us: onto(b).min(end),
                text: cue.text.clone(),
                image: cue.image.clone(),
            });
        }
    }
    // The spans come in timeline order but a cue's *own* order across them does
    // not: a Matroska muxer writes blocks as they come up.
    out.sort_by_key(|cue| cue.start_us);
    out
}

/// What [`start`] would do about the subtitles, in the words a card shows: what
/// travels, and the reason beside every pick that does not. Pure, like
/// [`planned_audio`]: no file is opened, so it may be asked per repaint.
///
/// `picks` is any run of indices into [`Project::subtitles`] -- an
/// [`ExportSettings::subtitles`] list (`picks.iter().copied()`) as much as a
/// single `Some(row)`, which is what a front-end holding one row still hands it.
pub fn planned_subtitles(
    project: &Project,
    format: Format,
    picks: impl IntoIterator<Item = usize>,
) -> String {
    let mut embedded: Vec<&str> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    let picks: Vec<usize> = picks.into_iter().collect();
    // One statement about the *file*, said before any about a track: a format
    // that is the sound alone has nowhere to put a single one of them, and the
    // reason a pick would not have travelled anyway (a picture track, no cues)
    // is not the reason it does not travel here. Nothing picked is "none" --
    // there is nothing to say a container cannot carry.
    if !picks.is_empty() && !format.has_video() {
        return "none — this format is the sound alone".into();
    }
    for pick in picks {
        // A pick the project has no row for: a caller holding an index the list
        // no longer has (a removed row shifts every later one down). Every other
        // reason a pick does not travel is named on this card, so this one is
        // too -- a bug that says nothing is a bug found in the finished file.
        let Some(track) = project.subtitles().get(pick) else {
            dropped.push(format!("#{pick} — no such track"));
            continue;
        };
        if let Some(why) = &track.refused {
            dropped.push(format!("{} — {why}", track.label));
        } else if track.is_bitmap() {
            // Drawn over the picture, written into no file: the exported track
            // is text and these cues are bitmaps. Said whatever the format,
            // because it is the track and not the container that cannot be
            // carried.
            dropped.push(format!("{} — pictures; drawn, not written", track.label));
        } else if track.cues.is_empty() {
            dropped.push(format!("{} — no cues", track.label));
        } else {
            // Whichever container: a Matroska carries the track as
            // `S_TEXT/UTF8` blocks and an mp4 as a `tx3g` timed-text track, and
            // a pick travels either way. There is no container refusal left on
            // this card -- the sound-alone one above is about the *file* and the
            // ones beside it about the *track*.
            embedded.push(&track.label);
        }
    }
    let mut parts = match embedded.len() {
        0 => Vec::new(),
        1 => vec![format!("{} → embedded", embedded[0])],
        n => vec![format!("{n} tracks → embedded ({})", embedded.join(", "))],
    };
    parts.extend(dropped);
    match parts.is_empty() {
        true => "none".into(),
        false => parts.join("; "),
    }
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

/// What [`start`] would open for *this* timeline: the picture's seat -- or the
/// copy that means there is no encoder at all -- and the sound's, asked of the
/// very gates the export asks ([`CopyPlan::of`] and [`copy_audio`]'s) rather
/// than of the format alone.
///
/// This exists because the card and the running export used to disagree in both
/// directions on the same file: the card named a software HEVC encode for a cut
/// the engine copied, and named an AAC copy for a Matroska source whose sound
/// no sample table holds and which therefore always re-encodes. A prediction
/// that is not the decision is worse than no prediction, because a person plans
/// their evening around it.
///
/// Costs what [`planned_video`] costs (an encoder opened and closed) plus a
/// header read per source and, once per file, its cluster walk: ask it off a
/// render thread and keep the answer until the timeline or the settings change.
pub fn planned_seats(
    project: &Project,
    meta: &VideoMeta,
    settings: &ExportSettings,
) -> (Option<&'static str>, &'static str) {
    let video = settings.format.has_video().then(|| {
        match CopyPlan::of(project, meta, settings).is_some() {
            true => "copy (source packets)",
            false => video_label(settings.format, hw_seat(meta, settings).is_some()),
        }
    });
    // The half [`audio_label`] cannot know from the format: a copy is a *sample
    // table's* packets ([`copy_audio`] hands them to `copy_multi_streams`), and
    // a Matroska file has none -- so every mkv source re-encodes, however
    // untouched its lane is. Extension only, which is what `is_matroska`
    // reads, so this costs nothing beyond what is already open.
    let copyable = !forces_encode(project)
        && !project
            .sources()
            .iter()
            .any(|source| crate::demux::is_matroska(&source.path));
    (
        video,
        audio_label(project, settings.format, has_sound(project), Some(copyable)),
    )
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
    /// The track's pre-skip in samples where those packets are **Opus** and not
    /// AAC ([`encode_opus`]), which is what the Matroska track entry states in
    /// its `OpusHead` and its `CodecDelay`. `None` is an AAC track, which is
    /// every copy and every mp4.
    pub opus_pre_skip: Option<u16>,
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
fn copy_audio(
    project: &Project,
    meta: &VideoMeta,
    kbps: u32,
    mkv: bool,
    shared: &Shared,
) -> crate::Result<Option<ExportAudio>> {
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
        return encode_audio(project, meta, kbps, mkv, shared);
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
    // ...and whether the mix itself does anything a packet never went through:
    // a lane volume is applied at the sum and a limiter over it, so copying
    // such a lane would write it at unity and unlimited, silently.
    let speeded = project.lane(lane).iter().any(|c| !c.speed.is_normal());
    if speeded || equalized(project) || mastered(project) {
        return encode_audio(project, meta, kbps, mkv, shared);
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
            // A copy is the source's own AAC packets: there is no Opus copy
            // path here, since the packets an mkv source holds never reach this
            // (they are not in a sample table) and every one of them re-encodes.
            opus_pre_skip: None,
        })),
        Err(_) => encode_audio(project, meta, kbps, mkv, shared),
    }
}

/// The picture's half of [`copy_audio`]: a Matroska export whose every span is
/// its source's own coded blocks, written back out without a decoder or an
/// encoder anywhere in the path.
///
/// **When.** A copy is entered only where it says *exactly* what the timeline
/// says, which is the audio gate's rule and not a looser one: the file this
/// export writes is Matroska, the source's codec is the one that file would
/// carry ([`Format::Hevc`] over HEVC, [`Format::Av1`] over AV1), and nothing on
/// the way has touched a sample -- no grade, no speed, no rate conversion, no
/// gap to fill with black, no source of another size to place on the canvas,
/// no `ContentEncodings` packing to undo. Anything else re-encodes exactly as
/// it always did; there is no half-copied file.
///
/// **All or nothing, per export.** A copied span and an encoded one cannot
/// share one track: this project's encoders write 8-bit SDR streams whose
/// parameter sets are their own, so splicing them into (say) a Main 10 HDR
/// source would change bit depth and colour mid-track, and a container states
/// its configuration record once. So the head and tail fragments of a cut that
/// lands mid-GOP are *not* re-encoded into the copy -- they are what makes the
/// whole export fall back to the encoder. What a copy needs instead is that
/// every span begins on a sync point and ends where the next one begins, which
/// is what a cut placed on a keyframe gives it.
///
/// **What a cut costs.** An open-GOP source (x265's default) writes leading
/// pictures after a sync point that are shown *before* it and reference the
/// group before it. At the start of a copied region those references are not in
/// the file, so they are dropped -- the picture before the cut is held for those
/// few frames (four at this film's B-depth, a sixth of a second) rather than
/// being written as pictures that cannot decode. The region still starts at the
/// timeline frame it owns, so the sound never drifts.
///
/// ponytail: mp4 is not copied (its `hvc1` sample entry forbids in-band
/// parameter sets, and the mp4 muxer here writes its sample table from a frame
/// counter), and neither is H.264, which this project only writes into mp4. The
/// upgrade path for both is a sample-table writer that takes explicit
/// timestamps, which is what [`MkvMuxer::write_block`] already is for Matroska.
struct CopyPlan {
    /// Every source a region reads, by the index the project names it with;
    /// `None` for a source no clip plays from.
    sources: Vec<Option<MkvDemuxer>>,
    regions: Vec<CopyRegion>,
    codec_id: &'static [u8],
    /// The `CodecPrivate` every region's source agrees on -- a track is declared
    /// once, so two sources that disagree about their parameter sets are not one
    /// copy.
    private: Vec<u8>,
    colour: ColorDescription,
    width: u32,
    height: u32,
}

/// A run of source blocks copied in one piece: what a cut leaves behind, and
/// what two spans that continue each other in the same file merge back into.
struct CopyRegion {
    source: usize,
    /// Blocks of that source, in the file's own decode order.
    blocks: std::ops::Range<usize>,
    /// The timeline frame the region's first picture is shown at.
    start: u32,
}

impl CopyPlan {
    /// The plan for this timeline, or `None` where anything at all makes a copy
    /// say something other than what the timeline says -- in which case the
    /// caller encodes, which is what it always did.
    fn of(project: &Project, meta: &VideoMeta, settings: &ExportSettings) -> Option<Self> {
        let (codec_id, codec): (&'static [u8], Codec) = match settings.format {
            Format::Hevc => (b"V_MPEGH/ISO/HEVC", Codec::Hevc),
            Format::Av1 => (b"V_AV1", Codec::Av1),
            _ => return None,
        };
        let entries = project.sources();
        let mut sources: Vec<Option<MkvDemuxer>> = (0..entries.len()).map(|_| None).collect();
        let mut declared: Option<(Vec<u8>, ColorDescription)> = None;
        let mut regions: Vec<CopyRegion> = Vec::new();
        for span in project.composite_spans_from(0) {
            // A gap is black frames, which only an encoder makes.
            let (source, in_frame) = span.from?;
            if !span.speed.is_normal() {
                return None;
            }
            // The grade playback shows, which a copied packet never went
            // through. `None` and the identity are the same untouched picture.
            if project
                .composite_color_at(span.start)
                .is_some_and(|params| !params.is_identity())
            {
                return None;
            }
            let path = &entries.get(source)?.path;
            if sources.get(source)?.is_none() {
                if !crate::demux::is_matroska(path) {
                    return None;
                }
                let (source_meta, demuxer) = Demuxer::open(path).ok()?;
                let Demuxer::Mkv(demuxer) = demuxer else {
                    return None;
                };
                // The stream this file would carry, at the size and the rate the
                // timeline is in: anything else is a picture that has to be
                // coded again to become this file's.
                if source_meta.codec != codec
                    || source_meta.width != meta.width
                    || source_meta.height != meta.height
                    || Rate::from_fps(source_meta.frame_rate, meta.frame_rate).ok()?
                        != Rate::REAL_TIME
                    || !demuxer.plain_blocks()
                {
                    return None;
                }
                // One track, one configuration record and one `Colour`: two
                // sources that disagree about either are two streams, and this
                // writes one.
                let declares = (demuxer.codec_private().to_vec(), source_meta.color);
                if declared.get_or_insert(declares.clone()) != &declares {
                    return None;
                }
                sources[source] = Some(demuxer);
            }
            let demuxer = sources.get_mut(source)?.as_mut()?;
            // The window this span reads, in the blocks the whole engine counts
            // a source's frames in. It has to *be* whole groups of pictures: a
            // copy that began between two sync points would hand a decoder
            // pictures whose references are not in the file, and one that ended
            // between them would drop a reference the pictures before it need.
            let (start, end) = (in_frame as usize, in_frame as usize + span.len as usize);
            if end > demuxer.block_count()
                || !demuxer.is_sync(start)
                || (end < demuxer.block_count() && !demuxer.is_sync(end))
            {
                return None;
            }
            // Two spans that continue each other in one file are one region:
            // nothing was cut between them, so the leading pictures across that
            // boundary still have every reference they need.
            match regions.last_mut() {
                Some(last)
                    if last.source == source
                        && last.blocks.end == start
                        && last.start + (last.blocks.end - last.blocks.start) as u32
                            == span.start =>
                {
                    last.blocks.end = end;
                }
                _ => regions.push(CopyRegion {
                    source,
                    blocks: start..end,
                    start: span.start,
                }),
            }
        }
        let (private, colour) = declared?;
        Some(Self {
            sources,
            regions,
            codec_id,
            private,
            colour,
            width: meta.width,
            height: meta.height,
        })
    }

    /// The file itself: one Matroska muxer, every region's blocks through it in
    /// the order and at the timing their source states.
    fn run(
        mut self,
        out: &Path,
        meta: &VideoMeta,
        shared: &Shared,
        audio: Option<(&AudioParams, Vec<crate::AacPacket>)>,
        subs: Vec<SubParams>,
        total: u32,
    ) -> crate::Result<()> {
        let mut muxer = MkvMuxer::create_copy(
            out,
            &CopyParams {
                width: self.width,
                height: self.height,
                frame_rate: meta.frame_rate,
                codec_id: self.codec_id,
                codec_private: &self.private,
                colour: self.colour,
            },
            audio,
            subs,
        )?;
        // Nanoseconds a timeline frame lasts, which is what a region's own
        // position on the timeline is measured in.
        let frame_ns = 1e9 / meta.frame_rate;
        let mut done = 0u32;
        for region in &self.regions {
            let demuxer = self.sources[region.source]
                .as_mut()
                .ok_or("a copied region names a source that was never opened")?;
            let base = (f64::from(region.start) * frame_ns).round() as i64;
            // When the region's first picture is shown on its source's clock;
            // everything after it is written that far along from `base`.
            let mut origin: Option<i64> = None;
            for index in region.blocks.clone() {
                cancelled(shared)?;
                let Some(block) = demuxer.coded_block(index)? else {
                    break;
                };
                let origin = *origin.get_or_insert(block.ts_ns);
                // The leading pictures of the group this region opens on: shown
                // before it, coded against the group before it, and that group
                // is not in this file. Dropped rather than written as pictures
                // no decoder can put back together.
                if block.ts_ns < origin {
                    continue;
                }
                muxer.write_block(block.bytes, block.key, base + block.ts_ns - origin)?;
                done += 1;
                shared
                    .progress
                    .store(picture_progress(done, total), Ordering::Relaxed);
            }
        }
        // Everything up to here is still cancellable, exactly as the encoded
        // path is up to its own `finish`.
        cancelled(shared)?;
        muxer.finish()?;
        shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
        Ok(())
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
fn encode_audio(
    project: &Project,
    meta: &VideoMeta,
    kbps: u32,
    mkv: bool,
    shared: &Shared,
) -> crate::Result<Option<ExportAudio>> {
    let sources = project.audio_sources();
    let segs = project.audio_segments_from(0, meta.frame_rate);
    let eqs = project.audio_eqs_from(0, meta.frame_rate);
    let speeds = project.audio_speeds_from(0, meta.frame_rate);
    // Coarse stage timers, in the same voice as `export video: copy` below: an
    // export of a feature film spends minutes in here before a byte of picture
    // is written, and which minutes went where is the first question asked of a
    // slow export. Three lines an export, so they stay on.
    let mixed = std::time::Instant::now();
    let Some((audio, chunks)) = AudioSession::open_mixed_streams_master(
        &sources,
        &segs,
        &eqs,
        &speeds,
        &project.audio_gains(),
        project.limiter(),
    )?
    else {
        return Ok(None); // no audio to write, exactly as a copy of nothing is
    };
    let freq_index = rusty_aac::sf_index_for_rate(audio.sample_rate).ok_or_else(|| {
        format!(
            "{} Hz is not an AAC sample rate: export WAV, FLAC, MP3 or OGG, which write it as \
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
        // The bar's first third of [`AUDIO_BAND`]: the mix is the longest of
        // the three stages and the only one that can be counted as it goes.
        shared.progress.store(
            (samples.len().min(total) * (AUDIO_BAND / 3) as usize / total.max(1)) as u32,
            Ordering::Relaxed,
        );
    }
    samples.resize(total, 0.0);
    eprintln!(
        "export audio: mixed {:.1} s of sound in {:.1} s",
        total as f64 / f64::from(audio.sample_rate.max(1)) / channels as f64,
        mixed.elapsed().as_secs_f64()
    );

    // Opus where the container carries it and the mix is inside the envelope
    // this encoder was *measured* correct in ([`encode_opus`]): a Matroska file,
    // 48 kHz, stereo. An Opus source edited into an mkv export therefore leaves
    // as Opus instead of being turned into AAC on the way out, which is the one
    // generation of loss an all-Opus timeline used to pay for being cut.
    //
    // Everything else falls through to the AAC below and is unchanged by this:
    // every mp4, a mono mix, a 44.1 kHz timeline. The rate is the mix's own --
    // there is no resampler on this path, and inventing one to reach 48 kHz
    // would resample a whole timeline to satisfy a codec, which is a bigger
    // change to the sound than the codec is.
    if mkv
        && audio.sample_rate == OPUS_RATE
        && channels == 2
        && let Some((packets, pre_skip)) = encode_opus(&samples, kbps)?
    {
        return Ok(Some(ExportAudio {
            params: crate::AacTrackParams {
                freq_index,
                chan_conf: 2,
                sample_rate: OPUS_RATE,
            },
            packets,
            copied: false,
            opus_pre_skip: Some(pre_skip),
        }));
    }

    // The caller's rate, never the encoder's own 128 default -- see
    // [`DEFAULT_AUDIO_KBPS`] for why the untouched figure is 256.
    let mut encoder = rusty_aac::AacEncoder::new(rusty_aac::AacEncoderConfig {
        bitrate_bps: kbps * 1_000,
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
            // and what the opener hands the timeline is one of those whatever
            // the source carried ([`crate::audio::downmix`]).
            chan_conf: audio.channels.max(1) as u8,
            sample_rate: audio.sample_rate,
        },
        packets,
        copied: false,
        opus_pre_skip: None,
    }))
}

/// The one rate Opus codes at. The container may state another one in its
/// `OpusHead` and every decoder still runs at 48 kHz (RFC 7845 §5.1), so a
/// timeline at any other rate is not "encoded at its own rate" here -- it is
/// resampled or it is not Opus, and [`encode_audio`] picks the second.
pub(crate) const OPUS_RATE: u32 = 48_000;

/// 20 ms at [`OPUS_RATE`], the frame every Opus muxer in the wild writes:
/// long enough that the per-packet overhead is nothing, short enough that a
/// Matroska block timestamp in whole milliseconds is exact.
const OPUS_FRAME: usize = 960;

/// The most this project asks of `opus-rs`, in kbps, and why it is a cap rather
/// than the four rates the Sound row offers: this encoder falls off a cliff at a
/// rate that depends on the *content*, and above it a conformant decoder gets
/// noise back. Measured on three one-second spans of a real 5.1 Opus film,
/// encoded here and decoded with `ruopus` -- which is not the suspect, since it
/// decodes ffmpeg's own libopus at 256 kbps to correlation 1.00000 on the same
/// material:
///
/// ```text
/// kbps     120     128     136     144     152     160
/// span A  0.945   0.937   0.452   0.206   0.223   0.163
/// span B  0.986   0.989   0.697   0.153   0.114   0.149
/// span C  0.999   0.994   0.951   0.335   0.266   0.326
/// libopus 0.997   0.998     --      --      --    0.996   (same spans, control)
/// ```
///
/// So: usable to 128, ruined by 136 on real film sound, and the pure-tone
/// fixtures that first suggested a 165 kbps ceiling were simply easy material.
/// 128 kbps stereo Opus is around transparency anyway (libopus scores 0.998
/// there on the worst of those spans), so the cap costs nothing a listener has.
/// It is still not *trusted*: every track is decoded again and measured before
/// it is written ([`opus_fidelity`]), because a cliff whose edge moves with the
/// material is not something a constant can be safe against on its own.
///
/// ponytail: the ceiling is this encoder's, not the format's. Upgrade path is a
/// released `opus-rs` that survives its own high rates -- the unit test pins the
/// failure, so it fails the day the bug is fixed and this can be raised.
pub(crate) const OPUS_MAX_KBPS: u32 = 128;

/// How well a track has to survive its own round trip to be written as Opus:
/// the correlation between the mix and the decode of what was encoded from it.
/// A good encoder scores 0.98-0.999 on this material (libopus, measured above);
/// the failure mode is not subtle at all, scoring 0.1-0.7. 0.95 sits in the gap
/// with room on both sides, and what fails it is written as AAC -- the export's
/// own encoder line then says AAC, so the fallback is visible rather than
/// silent.
const OPUS_MIN_FIDELITY: f64 = 0.95;

/// The encoder's lookahead at [`OPUS_RATE`] in the mode this envelope always
/// selects (CELT-only: 2.5 ms). The pre-skip a file states is this *plus* the
/// warm-up frame [`encode_opus`] throws away. Measured by
/// cross-correlating the decode against the source, not taken from the crate --
/// it exposes no lookahead getter -- and asserted at both ends of the rate band
/// in `tests/opus_export.rs`, which is what catches a version that changes it.
const OPUS_PRE_SKIP: u16 = 120;

/// The mix as Opus packets, one per [`OPUS_FRAME`], plus the pre-skip that has
/// to be declared in front of them. Stereo and [`OPUS_RATE`] only -- the caller
/// checks both, because outside that pair this encoder is not one this project
/// is willing to write files with ([`OPUS_MAX_KBPS`]).
///
/// The tail frame is padded with silence rather than dropped: Opus codes whole
/// frames and a track a few milliseconds short under a picture that is not is
/// the drift every other path here is written to avoid. The pre-skip trims the
/// front; the container's duration trims the back.
fn encode_opus(samples: &[f32], kbps: u32) -> crate::Result<Option<(Vec<crate::AacPacket>, u16)>> {
    let mut encoder = opus_rs::OpusEncoder::new(OPUS_RATE as i32, 2, opus_rs::Application::Audio)
        .map_err(|e| format!("the Opus encoder refused 48 kHz stereo: {e}"))?;
    encoder.bitrate_bps = (kbps.min(OPUS_MAX_KBPS) * 1_000) as i32;
    encoder.complexity = 10;
    let mut packets = Vec::new();
    let mut frame = vec![0f32; OPUS_FRAME * 2];
    // 1275 bytes is the most one Opus frame may weigh (RFC 6716 §3.2); this is
    // that with room for the TOC and a padded frame, so `encode` never has to
    // refuse for want of a buffer.
    let mut out = vec![0u8; 1500];
    // The head, coded **twice**. An encoder's very first frame has no history
    // behind it and comes out ramped and out of phase -- measured on a 440 Hz
    // fixture, the first 20 ms of the export correlated 0.09 with the source
    // where every later window correlated 0.9997 -- and 20 ms of wrong sound at
    // the head of a file is the kind of thing nobody hears in a test and
    // everybody hears in the first frame of a cut. So the first frame is fed in
    // once to warm the encoder, and thrown away again by the pre-skip: exactly
    // what pre-skip is for (RFC 7845 §4.2), at the cost of one packet.
    let coded = std::time::Instant::now();
    let warm = samples.len().min(OPUS_FRAME * 2);
    // ...and one silent frame after the sound, for the same delay seen from the
    // other end: the encoder is [`OPUS_PRE_SKIP`] samples behind its input, so
    // without a frame to push them out the last 2.5 ms of the timeline stay
    // inside it and the track ends short under a picture that does not.
    let blocks = std::iter::once(&samples[..warm])
        .chain(samples.chunks(OPUS_FRAME * 2))
        .chain(std::iter::once(&samples[..0]));
    for block in blocks {
        frame[..block.len()].copy_from_slice(block);
        frame[block.len()..].fill(0.0);
        let len = encoder
            .encode(&frame, OPUS_FRAME, &mut out)
            .map_err(|e| format!("Opus encode failed: {e}"))?;
        packets.push(crate::AacPacket {
            bytes: out[..len].to_vec(),
            samples: OPUS_FRAME as u32,
        });
    }
    // ...and now the part that is not optimism: what was just written is
    // decoded again and measured against what went in, and a track that does
    // not come back is not written at all ([`OPUS_MIN_FIDELITY`]). The caller
    // falls through to AAC, which is exactly where it stood before this seat
    // existed -- a worse codec is a fair trade for a track that is the sound.
    //
    // ponytail: this costs one Opus decode of the whole track (a minute of
    // sound in a few hundred milliseconds, against a video encode that is
    // minutes). Upgrade path is deleting it, the day this encoder can be
    // trusted at the rate it is asked for.
    let encoded = coded.elapsed().as_secs_f64();
    let pre_skip = OPUS_PRE_SKIP + OPUS_FRAME as u16;
    let measured = std::time::Instant::now();
    let fidelity = opus_fidelity(&packets, samples, usize::from(pre_skip));
    eprintln!(
        "export audio: Opus encode {encoded:.1} s, fidelity {fidelity:.4} measured in {:.1} s",
        measured.elapsed().as_secs_f64()
    );
    Ok((fidelity >= OPUS_MIN_FIDELITY).then_some((packets, pre_skip)))
}

/// Packets one sampled window listens to: 5 s at [`OPUS_FRAME`].
const FIDELITY_WINDOW: usize = 250;

/// How many of them a long track is judged on -- a minute of sound, wherever
/// the track's length puts them, and a constant cost rather than one that grows
/// with the film ([`opus_fidelity`]).
const FIDELITY_WINDOWS: usize = 12;

/// Packets decoded into a window and thrown away before it is measured. CELT
/// predicts across frames, so a decoder started cold mid-stream is behind its
/// own history for a few frames; 100 ms is far more than it needs and is not
/// worth counting.
const FIDELITY_WARMUP: usize = 5;

/// How much sound a window must carry before its correlation means anything:
/// mean square per sample. A window of digital silence -- the head of a film,
/// the gap between a feature and its credits -- correlates with nothing, and
/// failing a track for it would send perfectly good sound to AAC.
const FIDELITY_FLOOR: f64 = 1e-9;

/// The correlation between `samples` and the decode of `packets` with the first
/// `pre_skip` frames dropped: 1.0 is the same waveform, and this codec's failure
/// mode lands near 0.1. Streamed packet by packet -- the decode is never held,
/// only three running sums -- because the mix behind it is already the biggest
/// thing in an export ([`encode_audio`]'s ponytail).
///
/// A packet the decoder refuses is 0.0 and no argument: a track this project
/// cannot read is exactly what this is here to catch.
///
/// **Sampled above a minute of sound, and the median window is the answer.**
/// This decode is the whole cost of exporting a feature film: `ruopus` runs its
/// inverse MDCT at 0.2x real time here (measured, 24.2 s for 121.5 s of stereo),
/// so judging every packet of a two-and-a-half-hour film costs half an hour
/// before a byte of picture is written -- which is what it did, and what made a
/// copy export that has no encoding to do at all sit at zero for twenty minutes.
/// [`FIDELITY_WINDOWS`] windows spread evenly across the track cost the same
/// minute of decode whatever the film's length, and what they are looking for
/// survives sampling: [`OPUS_MAX_KBPS`]'s cliff is a *rate* the encoder falls
/// off everywhere that rate is used, not a defect of one bar of music.
///
/// The **median** and not the worst, and that is measured rather than cautious.
/// Forty five-second windows of a real 5.1 film's mix, encoded here at 128 kbps
/// and decoded back, in order:
///
/// ```text
/// 0.803  0.835  0.938  0.940  0.960  0.987  0.994  ... median 0.998 ... 0.999
/// ```
///
/// Five of the thirty-nine that carried sound sit under [`OPUS_MIN_FIDELITY`] on
/// a track that is *good* -- which is the same thing the table in
/// [`OPUS_MAX_KBPS`] shows, where a span scoring 0.937 at 128 kbps is inside the
/// usable band. A worst-window rule at 0.95 therefore refuses films this encoder
/// handles perfectly well, while the failure it exists for is the opposite
/// shape: at 136 kbps the same three measured spans read 0.452, 0.697, 0.951,
/// and at 256 kbps the whole track correlates 0.06. Half a track cannot be
/// ruined without the middle of the list moving, so the median separates the two
/// by a margin no single window can (0.998 against under 0.7) -- and a track
/// under a minute is one window, where median and whole-track correlation are
/// the same number the gate always used.
fn opus_fidelity(packets: &[crate::AacPacket], samples: &[f32], pre_skip: usize) -> f64 {
    // Short enough to hear all of: every fixture, every unit test, and any
    // track under a minute. There is nothing to sample from.
    let all = FIDELITY_WINDOW * FIDELITY_WINDOWS;
    if packets.len() <= all {
        return window_fidelity(packets, samples, pre_skip, 0, 0, packets.len()).unwrap_or(1.0);
    }
    // Evenly spread, first window at the head: the head is where an encoder's
    // own warm-up would show, and the last starts a window short of the end so
    // no window runs past the packets.
    let step = (packets.len() - FIDELITY_WINDOW) / (FIDELITY_WINDOWS - 1);
    let mut scores: Vec<f64> = Vec::with_capacity(FIDELITY_WINDOWS);
    for w in 0..FIDELITY_WINDOWS {
        let at = w * step;
        let from = at.saturating_sub(FIDELITY_WARMUP);
        let Some(fidelity) =
            window_fidelity(packets, samples, pre_skip, from, at, at + FIDELITY_WINDOW)
        else {
            continue; // silence: nothing to correlate, and not a failure
        };
        scores.push(fidelity);
    }
    // Every window silent is a silent track, which this codec writes as silence.
    if scores.is_empty() {
        return 1.0;
    }
    scores.sort_by(f64::total_cmp);
    scores[scores.len() / 2]
}

/// [`opus_fidelity`] over `packets[measure..to]`, with `packets[from..measure]`
/// -- the warm-up -- decoded for its state and thrown away. `None` where the
/// source window carries no sound worth correlating ([`FIDELITY_FLOOR`]).
fn window_fidelity(
    packets: &[crate::AacPacket],
    samples: &[f32],
    pre_skip: usize,
    from: usize,
    measure: usize,
    to: usize,
) -> Option<f64> {
    let mut decoder = ruopus::MultistreamDecoder::with_rate(OPUS_RATE, 1, 1, &[0, 1]);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    // Where in the mix this window's first decoded sample lands: every packet
    // is [`OPUS_FRAME`] long, and the pre-skip is what the front of the stream
    // owes before the first of them is the timeline's frame 0.
    let mut at = (from * OPUS_FRAME * 2).saturating_sub(pre_skip * 2);
    let mut drop = (pre_skip * 2).saturating_sub(from * OPUS_FRAME * 2);
    let mut counted = 0usize;
    for (index, packet) in packets.iter().enumerate().take(to).skip(from) {
        let Ok(pcm) = decoder.decode_packet(&packet.bytes) else {
            return Some(0.0);
        };
        let pcm = &pcm[drop.min(pcm.len())..];
        drop = drop.saturating_sub(packet.samples as usize * 2);
        // The warm-up is decoded for its state and not for its numbers.
        if index < measure {
            at += pcm.len();
            continue;
        }
        for (want, got) in samples[at.min(samples.len())..].iter().zip(pcm) {
            let (x, y) = (f64::from(*want), f64::from(*got));
            num += x * y;
            da += x * x;
            db += y * y;
            at += 1;
            counted += 1;
        }
    }
    (da / counted.max(1) as f64 > FIDELITY_FLOOR).then(|| num / (da.sqrt() * db.sqrt()).max(1e-12))
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
    // The picture's decision first, before a sample of sound is touched: it is
    // the answer that decides what the whole export *is*, it costs one header
    // read per source, and it is what the line on screen has been missing --
    // an export that copies its picture used to say nothing at all for as long
    // as its sound took, which on a feature film was minutes. Half a line now,
    // completed by the measured codec once the sound is known below: what is
    // published is never a codec that did not run.
    let plan = CopyPlan::of(project, meta, settings);
    if plan.is_some() {
        eprintln!("export video: copy (source packets)");
        *shared.encoders.lock().unwrap() = Some("copy · working out the sound…".into());
    }
    // ...then the audio: a track has to be declared when the muxer is created,
    // which happens as soon as the first coded picture arrives -- and the
    // Matroska muxer wants the packets themselves that early too, because it
    // interleaves them into the clusters as it writes. Every video format gets
    // the same track: none of them is picture-only any more.
    let audio = copy_audio(
        project,
        meta,
        audio_kbps_of(settings),
        settings.format.is_mkv(),
        shared,
    )?;
    let audio_params = audio.as_ref().map(|track| AudioParams {
        freq_index: track.params.freq_index,
        chan_conf: track.params.chan_conf,
        sample_rate: track.params.sample_rate,
        opus_pre_skip: track.opus_pre_skip,
    });
    // What the track *is*, kept before the packets are handed to a muxer: the
    // encoder line names a copy or an encode and only `copy_audio` knows which.
    let sound = audio.as_ref().map(|track| track.copied);
    // ...and *which* encoder, for the same line: the card predicts Opus from the
    // format alone and cannot know the mix's rate or width ([`audio_label`]),
    // while this is after the mix was opened, so it is the measured answer.
    let opus = audio
        .as_ref()
        .is_some_and(|track| track.opus_pre_skip.is_some());
    // Taken by the Matroska muxer at creation; the mp4 one writes its track
    // after the picture, so for that path this is still `Some` at the end.
    let mut packets = audio.map(|track| track.packets);
    // ...and the cues with them, for the same reason: the subtitle track is
    // declared in the header and its blocks are interleaved into the clusters,
    // so the muxer needs the lot before the first picture is written.
    let mut subs = export_subtitles(project, meta, settings);
    // The sound is done, whichever way it went: the bar stands at the head of
    // the picture's own share ([`AUDIO_BAND`]).
    shared.progress.store(AUDIO_BAND, Ordering::Relaxed);

    // Before an encoder is opened at all: a timeline nobody has touched, whose
    // sources this file can hold as they are, is written by copying their coded
    // blocks -- the picture's half of what `copy_audio` just did for the sound,
    // and the difference between an export bounded by the *edited* spans and one
    // that re-encodes a whole film to change a cut. [`CopyPlan`] states the
    // whole rule; anything at all outside it falls through to the walk below.
    if let Some(plan) = plan {
        *shared.encoders.lock().unwrap() = Some(format!(
            "copy · {}",
            // The measured codec, exactly as the encoder path below publishes
            // it: the mix has been opened by now, so a line that fell back to
            // AAC says AAC. Naming the prediction here would make the fallback
            // invisible on precisely the exports that are over in seconds.
            measured_audio_label(project, settings.format, sound, opus)
        ));
        return plan.run(
            out,
            meta,
            shared,
            audio_params.as_ref().zip(packets.take()),
            std::mem::take(&mut subs),
            total,
        );
    }

    // One header parse per source, before any of them is decoded: what rate
    // each file was shot at against the timeline's ([`Rate`]), which is how the
    // clip's timeline frames below become frames of the file, and what its
    // samples *mean* ([`ColorDescription`]), which is what decides whether they
    // are remapped on the way in. Real time for a still, a song and a
    // single-rate project, where every conversion below is the identity.
    let rates: Vec<(Rate, ColorDescription, Option<f32>)> = sources
        .iter()
        .map(|source| source_rate(&source.path, meta.frame_rate))
        .collect();
    // ...and, for a source on an HDR curve, the tone map that brings its samples
    // down to the SDR this file is written in ([`crate::tonemap`]). Once per
    // source stream: the table costs a couple of milliseconds to build and a
    // span reopens its file, so building it here rather than in the loop below
    // is the difference between paying that once and paying it per cut. An SDR
    // source -- the whole of an ordinary project -- builds no table at all.
    //
    // Through the project's own preset ([`tonemap::Preset`]) and each source's
    // own declared peak, which is the pair playback's decode funnel builds its
    // tables with: preview and export are the same rendition of the same film
    // because they are told the same two things, not because two numbers happen
    // to agree.
    let preset = project.tone();
    let tone: Vec<Option<ToneMapper>> = rates
        .iter()
        .map(|(_, color, peak)| match color.transfer {
            Transfer::Sdr => None,
            Transfer::Pq => Some(ToneMapper::new(tonemap::Transfer::Pq, preset, *peak)),
            Transfer::Hlg => Some(ToneMapper::new(tonemap::Transfer::Hlg, preset, *peak)),
        })
        .collect();
    // ...and the one space they are all written in, the same rule a reader with
    // no tags to read would apply to this file's height.
    let out_color = ColorDescription::output(meta.height);

    let mut encoder = Enc::open(meta, settings)?;
    // What this file is really being written by, for a progress line to name.
    // Published *after* the seat is open, so a hardware encoder the driver
    // refused reads as the software one that took over rather than as the hope
    // it replaced -- and the sound says whether it was copied or encoded, which
    // `copy_audio` decided a few lines up.
    *shared.encoders.lock().unwrap() = Some(format!(
        "{} · {}",
        encoder.label(),
        measured_audio_label(project, settings.format, sound, opus)
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
        let (mut pictures, rate, in_frame, color, mapper) = match span.from {
            Some((source, in_frame)) => {
                let entry = sources
                    .get(source)
                    .ok_or_else(|| format!("clip names source {source} of {}", sources.len()))?;
                let (rate, color, _peak) = rates[source];
                // Opened at the file's own frame, which is the only place the
                // file's numbering is used -- the span's are the timeline's.
                let pictures = ClipDecoder::open(&entry.path, rate.source_at(in_frame))?;
                (
                    Some(pictures),
                    rate,
                    in_frame,
                    Some(color),
                    tone[source].as_ref(),
                )
            }
            // A gap's black is 16/128/128, which is black in every matrix here
            // and on every curve: nothing to remap and nothing to tone-map,
            // which is what `None` says.
            None => (None, Rate::REAL_TIME, 0, None, None),
        };
        // Mixed spaces on one timeline: a clip coded against another matrix than
        // the one this file declares is rewritten into it, *after* the grade --
        // playback grades in the source's own space and converts for the screen
        // ([`crate::decode`]), so a grade applied to remapped samples is a
        // different picture than the one the canvas showed. Same-space clips --
        // the whole of an ordinary project -- take `None` and not a byte is
        // touched.
        //
        // ...and what the samples mean when that remap sees them is not always
        // what the file said, which is [`remap_into`]'s whole question.
        let remap = remap_into(color, mapper.is_some(), out_color.matrix);
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
            let params = grade.filter(|p| !p.is_identity());
            let (y, u, v) = match (params, remap, mapper) {
                (None, None, None) => (y, u, v),
                (params, remap, mapper) => {
                    let (gy, gu, gv) = &mut graded;
                    gy.clear();
                    gy.extend_from_slice(y);
                    gu.clear();
                    gu.extend_from_slice(u);
                    gv.clear();
                    gv.extend_from_slice(v);
                    // Tone-map first, so the grade is applied to the SDR picture
                    // the canvas showed and not to HDR codes; then remap the
                    // graded result. The order playback renders in, and the
                    // only order in which preview and export agree.
                    if let Some(mapper) = mapper {
                        mapper.map(gy, gu, gv, width as usize, height as usize);
                    }
                    if let Some(params) = params {
                        crate::color::apply_yuv(&params, gy, gu, gv);
                    }
                    if let Some((from, to)) = remap {
                        crate::colorspace::remap(from, to, gy, gu, gv, width as usize);
                    }
                    (&gy[..], &gu[..], &gv[..])
                }
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
                        &mut subs,
                        au,
                        key,
                    )?;
                }
                done += 1;
                shared
                    .progress
                    .store(picture_progress(done, total), Ordering::Relaxed);
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
            &mut subs,
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
    // The mp4's audio and subtitle tracks after its picture -- the Matroska one
    // interleaved its own as it went and left `packets` and `subs` empty behind
    // it. The text last, so its samples are written knowing how long the picture
    // turned out to be.
    let muxer = match muxer {
        Muxer::Mp4(mut mp4) => {
            for packet in packets.into_iter().flatten() {
                mp4.write_audio_packet(&packet.bytes)?;
            }
            mp4.write_subtitles(&subs)?;
            Muxer::Mp4(mp4)
        }
        muxer => muxer,
    };
    cancelled(shared)?;
    muxer.finish()?;
    shared.progress.store(PROGRESS_SCALE, Ordering::Relaxed);
    Ok(())
}

/// Which matrix pair a span's samples are rewritten by on the way into a file
/// whose own matrix is `out` -- `None` when they are already coded against it,
/// and for a gap, whose 16/128/128 black is black in every matrix here.
///
/// `toned` says the tone map ran on this span: it hands back BT.709 SDR whatever
/// the source was coded as, so an HDR clip is never *also* given the matrix
/// treatment its BT.2020 tag would ask for -- one conversion or the other, never
/// both on one frame -- while a tone-mapped clip on a canvas below 720 lines
/// still takes its 709 -> 601 step.
fn remap_into(
    color: Option<ColorDescription>,
    toned: bool,
    out: Matrix,
) -> Option<(Matrix, Matrix)> {
    let from = match toned {
        true => Matrix::Bt709,
        false => color?.matrix,
    };
    (from != out).then_some((from, out))
}

/// What rate `path` was shot at against a timeline at `timeline_fps`
/// ([`Rate`]). [`Rate::REAL_TIME`] for the files that have no rate of their own
/// -- a still, a song -- and for one that will not open here: the decoder is
/// opened a few lines later and fails the export by name, which is a better
/// error than this one could raise.
fn source_rate(path: &Path, timeline_fps: f64) -> (Rate, ColorDescription, Option<f32>) {
    if crate::is_image(path) || crate::is_audio(path) {
        // A still is BT.601 by construction whatever it was authored as:
        // `decode::rgb_to_i420` is the one matrix that turns its pixels into
        // planes. A song has no picture and the answer is never read.
        return (Rate::REAL_TIME, ColorDescription::default(), None);
    }
    match Demuxer::open(path) {
        // ...and one whose rate cannot be named against the timeline's, which
        // `matches_timeline` refuses at import, so nothing on a timeline is on
        // this arm either.
        //
        // The declared peak comes off the same open the space does, which is
        // what keeps export on the number playback's decode funnel read: one
        // header parse, both answers.
        Ok((meta, demuxer)) => (
            Rate::from_fps(meta.frame_rate, timeline_fps).unwrap_or(Rate::REAL_TIME),
            meta.color,
            demuxer.light().peak(),
        ),
        // Unreadable here means unreadable below too, where the span dies with a
        // real message; the file's own space is the least of that.
        Err(_) => (Rate::REAL_TIME, ColorDescription::default(), None),
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
    settings: &ExportSettings,
) -> crate::Result<()> {
    let format = settings.format;
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
    // ...and the mix over both: each lane's volume into the sum, the master
    // limiter out of it. The same opener playback's feeder reads from, so a
    // file is written at the levels it was heard at -- there is no second
    // place here that could mix it differently.
    let Some((audio, chunks)) = AudioSession::open_mixed_streams_master(
        &sources,
        &segs,
        &eqs,
        &speeds,
        &project.audio_gains(),
        project.limiter(),
    )?
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
        Format::Mp3 => write_mp3(out, &samples, &audio, audio_kbps_of(settings))?,
        Format::Ogg => write_ogg(out, &samples, &audio)?,
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
/// CBR at the caller's rate, [`DEFAULT_AUDIO_KBPS`] where there was none. Every
/// offered rate is already a legal Layer III value, so the encoder's snap is a
/// backstop and not a silent substitution; a sample rate MPEG has no frame for
/// (anything but 8-48 kHz) is refused there by name rather than written as
/// something else.
///
/// ponytail: CBR only -- the card offers rates and not a quality index. Upgrade
/// path is `Mp3EncoderConfig::vbr_quality` behind a setting of its own.
fn write_mp3(out: &Path, samples: &[i32], audio: &AudioMeta, kbps: u32) -> crate::Result<()> {
    let mut encoder = rusty_mp3::Mp3Encoder::new(rusty_mp3::Mp3EncoderConfig {
        bitrate_kbps: kbps,
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

/// The Vorbis quality every `.ogg` this program writes is coded at, on
/// `rusty_vorbis`'s normalised `[0, 1]` scale -- about `-q 8.35` on the scale
/// Vorbis itself is spoken in (`quality01_from_vorbis_q`).
///
/// A measured number, not a taste. The 2026-08-11 bench encoded three of this
/// suite's fixtures (`test_tone_48k.wav`, `test_av.mp4`, `test_tone.mp3`) at
/// every quality from 0.40 to 0.95 and decoded each back with **symphonia** --
/// an independent decoder, and the very one [`crate::audio`] reopens an export
/// with. Two things came out of it:
///
/// * **Below 0.50 the encoder is quiet, not just coarse.** The decoded RMS
///   against the source's runs 0.83-0.94 at quality 0.40 (a 1 to 1.6 dB level
///   error), and at 0.10 and below the file decodes to near-silence -- the same
///   class of failure the AC-3 mono downmix has in `crate::audio`. From 0.55 up
///   the level sits at 1.000-1.009, which is what a lossy codec should do.
/// * **The band tops out well under the rates the Sound row offers.** Quality
///   0.85 measured 176, 144 and 148 kbps on the three fixtures; 0.95 only
///   reaches ~200. That is why [`audio_rate_refusal`] takes the row away rather
///   than mapping 128/192/256/320 onto quality steps: the top two are numbers
///   this encoder cannot produce, and a card that printed them would be lying.
///
/// 0.85 is the top of the band where every fixture still decoded to *exactly*
/// its own length (0.95 overshot one by 820 samples), which is the property
/// [`write_ogg`] is built around.
const VORBIS_QUALITY: f32 = 0.85;

/// The Vorbis block hop, and the size of the pre-roll [`write_ogg`] feeds the
/// encoder. `rusty_vorbis` emits long blocks only (`N = 2048`, hop `N/2`).
const VORBIS_HOP: usize = 1024;

/// The same samples as Vorbis I in an Ogg container: `rusty_vorbis` encodes,
/// `oxideav-ogg` pages. Pure Rust on both halves, like every other encoder here.
///
/// Two corrections stand between that library pair and a file this project would
/// ship, and both are measured rather than guessed (2026-08-11, decoded back
/// with symphonia every time):
///
/// 1. **One hop of pre-roll.** `rusty_vorbis` advances the granule by a full hop
///    on its *first* audio packet, which its own doc comment says decodes to
///    zero samples. Written straight through, every sample lands ~1024 early:
///    a marker signal whose only energy was its first and last 1024 samples came
///    back with the head at 36% of its amplitude and a full-scale burst 1500
///    samples *before* the real one, in what should have been silence. Feeding
///    [`VORBIS_HOP`] samples of silence ahead of the mix and subtracting the same
///    from every granule puts it back: head 0.86 against an input peak of 0.80,
///    the phantom burst down to 0.04, the real burst at its own place. The
///    decoder drops the pre-roll itself -- a first page whose granule is under
///    what its packets decode to is exactly how Ogg says "skip this much".
/// 2. **The tail is declared, not encoded.** The block grid overshoots the last
///    sample, so the stream is padded out and the *last* page's granule is set
///    to the timeline's own frame count. A decoder trims to it, which is what
///    makes an `.ogg` here exactly as long as the WAV of the same timeline --
///    the promise [`run_audio`] makes for every other audio format.
///
/// **Stereo, always.** The embedded setup header `rusty_vorbis` ships is a
/// stereo profile: a mono push is refused outright ("bad coupling channels"),
/// and so is anything wider. A mono mix is therefore written as dual mono, both
/// channels bit-identical (measured: `max|L-R|` is exactly 0, and Vorbis's own
/// channel coupling codes the empty side channel for almost nothing). Wider than
/// stereo cannot reach here -- [`crate::audio`] folds 5.1 to stereo when the
/// source is opened -- but it is refused by name rather than left to the
/// library's own message.
///
/// ponytail: one quality, [`VORBIS_QUALITY`], with no user control -- the Sound
/// row is refused for this format because the rates it offers are not rates
/// Vorbis reaches. Upgrade path is a quality picker of its own, worth building
/// the day the card has a control that speaks in quality rather than kbps.
fn write_ogg(out: &Path, samples: &[i32], audio: &AudioMeta) -> crate::Result<()> {
    if audio.channels > 2 {
        return Err(format!(
            "Ogg Vorbis is written in stereo here and this mix has {} channels",
            audio.channels
        )
        .into());
    }
    // The mp3 path's conversion, so the two files are one mix, plus the pre-roll
    // and (for a mono mix) the dual-mono widening: the same sample in both
    // channels is the same signal, not a widening of it, so what a player folds
    // back down is what was mixed.
    let mut pcm: Vec<i16> = vec![0; VORBIS_HOP * 2];
    match audio.channels < 2 {
        true => pcm.extend(samples.iter().flat_map(|&s| [s as i16, s as i16])),
        false => pcm.extend(samples.iter().map(|&s| s as i16)),
    }
    // The block grid has to run past the last real sample or the tail is never
    // coded at all; the granule below is what trims the padding back off.
    let frames = samples.len() / usize::from(audio.channels).max(1);
    pcm.resize(pcm.len() + VORBIS_HOP * 4 * 2, 0);

    let mut encoder = rusty_vorbis::VorbisEncoder::new(rusty_vorbis::VorbisEncoderConfig {
        bitrate_bps: rusty_vorbis::BITRATE_NOMINAL,
        quality: VORBIS_QUALITY,
    });
    encoder
        .push_pcm_s16(&pcm, 2, audio.sample_rate)
        .map_err(|e| format!("vorbis encode: {e}"))?;
    encoder.finish();
    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(rusty_vorbis::Error::Eof) => break,
            Err(e) => return Err(format!("vorbis encode: {e}").into()),
        }
    }
    // Identification, comment and setup, then at least one audio packet: fewer
    // than four and there is no stream to page.
    if packets.len() < 4 {
        return Err(format!("vorbis encode: {} packets is not a stream", packets.len()).into());
    }
    let headers = [
        &packets[0].data[..],
        &packets[1].data[..],
        &packets[2].data[..],
    ];
    let extradata = oxideav_ogg::mux::xiph_lace(&headers)
        .ok_or("vorbis encode: the three headers would not lace")?;

    let time_base = oxideav_core::TimeBase::new(1, i64::from(audio.sample_rate));
    let mut params = oxideav_core::CodecParameters::audio(oxideav_core::CodecId::new("vorbis"));
    params.sample_rate = Some(audio.sample_rate);
    params.channels = Some(2);
    params.extradata = extradata;
    let stream = oxideav_core::StreamInfo {
        index: 0,
        time_base,
        duration: Some(frames as i64),
        start_time: Some(0),
        params,
    };
    let mut muxer = oxideav_ogg::mux::open_concrete(Box::new(File::create(out)?), &[stream])
        .map_err(|e| format!("ogg mux: {e:?}"))?;
    // Without this every audio packet of a short export lands on one page, and
    // the muxer's own note records what that costs: a first-audio-page-is-also-
    // EOS stream decodes short by half a small block in a reference decoder.
    muxer.set_page_target_bytes(Some(OGG_PAGE_BYTES));
    muxer
        .write_header()
        .map_err(|e| format!("ogg mux: {e:?}"))?;
    let last = packets.len() - 1;
    for (i, packet) in packets.iter().enumerate().skip(3) {
        // Correction 1 on every packet, correction 2 on the last one.
        let granule = match i == last {
            true => frames as i64,
            false => (packet.pts - VORBIS_HOP as i64).max(0),
        };
        muxer
            .write_packet(
                &oxideav_core::Packet::new(0, time_base, packet.data.clone())
                    .with_pts(granule)
                    .with_duration(packet.duration),
            )
            .map_err(|e| format!("ogg mux: {e:?}"))?;
    }
    muxer
        .write_trailer()
        .map_err(|e| format!("ogg mux: {e:?}"))?;
    Ok(())
}

/// The page size [`write_ogg`] asks the Ogg muxer to aim at, in bytes. RFC 3533
/// describes pages as "usually 4-8 kB", which is also the muxer's own suggested
/// general-purpose value.
const OGG_PAGE_BYTES: usize = 4096;

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
    subs: &mut Vec<SubParams>,
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
                        std::mem::take(subs),
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
                        std::mem::take(subs),
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
    /// What the SPS says the samples mean. A decoder reads the bitstream before
    /// it reads the container, so an HEVC stream without this renders BT.601 in
    /// libavcodec however the file is tagged -- the container's `Colour` element
    /// and `colr` box are not enough on their own.
    signal: oxideav_h265::vui::VideoSignalType,
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
        // What the sequence header says the samples mean, for the reason
        // [`video_signal_type`] gives: a container tag is the second answer a
        // decoder looks at, not the first. Written as rav1e's own enums rather
        // than the H.273 numbers -- the crate has no `from` for the code points,
        // and the output rule only ever picks one of these two spaces.
        let out = ColorDescription::output(meta.height);
        cfg.pixel_range = rav1e::prelude::PixelRange::Limited;
        cfg.color_description = Some(match out.matrix {
            Matrix::Bt709 => rav1e::prelude::ColorDescription {
                color_primaries: rav1e::prelude::ColorPrimaries::BT709,
                transfer_characteristics: rav1e::prelude::TransferCharacteristics::BT709,
                matrix_coefficients: rav1e::prelude::MatrixCoefficients::BT709,
            },
            _ => rav1e::prelude::ColorDescription {
                color_primaries: rav1e::prelude::ColorPrimaries::BT601,
                transfer_characteristics: rav1e::prelude::TransferCharacteristics::BT601,
                matrix_coefficients: rav1e::prelude::MatrixCoefficients::BT601,
            },
        });
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
            signal: video_signal_type(meta.height),
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
        let signal = self.signal;
        let coded: Vec<crate::Result<Vec<u8>>> = std::thread::scope(|scope| {
            let lanes: Vec<_> = self
                .pending
                .iter()
                .map(|(y, cb, cr)| {
                    scope.spawn(move || {
                        oxideav_h265::encoder::intra::encode_idr_intra_au_cropped(
                            y,
                            cb,
                            cr,
                            width,
                            height,
                            qp,
                            crop.0,
                            crop.1,
                            Some(signal),
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

/// What the *bitstream* says the samples mean, in the words a sequence header
/// spells them -- the same H.273 code points [`crate::mux`] writes into the
/// container, from the same [`ColorDescription::output`] rule, so the two halves
/// of a file cannot drift apart.
///
/// This exists because a container tag is not enough: libavcodec takes the
/// bitstream's answer first, and a stream that says nothing is "unspecified",
/// which renders BT.601 -- a visible shift on a 709 export that a player reads
/// from the wrong matrix. Every export is limited range, which is what
/// `video_full_range_flag = false` says here.
fn video_signal_type(height: u32) -> oxideav_h265::vui::VideoSignalType {
    let (primaries, transfer, matrix) = ColorDescription::output(height).codes();
    oxideav_h265::vui::VideoSignalType {
        // "Unspecified", which is what every encoder here writes: the field is
        // the analogue system a picture came off, and none of these did.
        video_format: 5,
        video_full_range_flag: false,
        colour_description: Some(oxideav_h265::vui::ColourDescription {
            colour_primaries: primaries as u8,
            transfer_characteristics: transfer as u8,
            matrix_coeffs: matrix as u8,
        }),
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
    /// A stream, decoded on a thread of its own: the encode of picture *n* and
    /// the decode of picture *n + 1* are the two halves an export used to do in
    /// turns, each idle while the other worked. The channel is two pictures
    /// deep, so the decoder runs no further ahead than that (a 1080p I420
    /// picture is 3.1 MB) and a slow encoder still stops it.
    ///
    /// The thread is also where the plugin is *opened*: a VA-API session is not
    /// ours to hand between threads ([`crate::decode::open_worker_deferred`]
    /// keeps the same rule), which is why the open's error comes back down the
    /// channel rather than out of [`ClipDecoder::open`].
    Streamed(Streamed),
    /// A still image: one picture, handed out for as long as the span runs.
    /// It is decoded here rather than in `run`'s loop for the same reason the
    /// other two are opened there -- the span's pictures come from one place,
    /// and no thread can make one picture arrive sooner.
    Still(crate::decode::Still),
}

/// The receiving half of a decode thread, plus the picture it last handed out
/// -- which is what lets `next` keep lending planes rather than moving them.
///
/// The first two fields are in this order on purpose, and it is the order
/// [`crate::decode::FrameStream`] states for the same pair: Rust drops fields as
/// they are declared, so the receiver disconnects *first* -- the only thing that
/// wakes a decoder parked in a full `send` -- and the join inside
/// [`DecodeThread::drop`] runs second. The other way round is a hang and not a
/// slow path.
struct Streamed {
    frames: Receiver<crate::Result<Option<Yuv>>>,
    /// Held only to be dropped: joining is all it does, and it must happen
    /// after the receiver above (`_lib` in `crate::hw` is kept the same way).
    _thread: DecodeThread,
    current: Option<Yuv>,
    /// Whether the thread has said "no more pictures" in as many words. What
    /// separates a stream that ended from a thread that vanished: after this,
    /// the channel is disconnected because the decoder is *done*, and a further
    /// ask is end of stream rather than a failure.
    ended: bool,
}

/// A decode thread, joined when its span's decoder is dropped -- the rule
/// [`crate::decode::Worker`] is written for: a thread abandoned mid
/// `vaInitialize` outlives the process, and Mesa's `atexit` handlers then free
/// the state it is still reading, which is a SIGSEGV at exit long after the
/// export succeeded.
struct DecodeThread(Option<thread::JoinHandle<()>>);

impl Drop for DecodeThread {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // The receiver is already gone (see `Streamed`'s field order), so a
            // decoder blocked on `send` has been woken and this waits only for
            // the thread to unwind its VA-API session.
            let _ = handle.join();
        }
    }
}

/// One picture, owned, on its way from a decode thread to the encoder.
struct Yuv {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    width: u32,
    height: u32,
}

impl ClipDecoder {
    fn open(path: &Path, start_frame: u32) -> crate::Result<Self> {
        // Before either decoder: an image is not a stream, so `start_frame`
        // means nothing to it -- every frame of a still span is the same
        // picture, which is what playback shows for it too.
        if crate::is_image(path) {
            return Ok(Self::Still(crate::decode::Still::open(path)?));
        }
        let path = path.to_path_buf();
        let (tx, frames) = sync_channel(2);
        let handle = thread::Builder::new()
            .name("export-decode".into())
            .spawn(move || decode_stream(&path, start_frame, &tx))?;
        Ok(Self::Streamed(Streamed {
            frames,
            _thread: DecodeThread(Some(handle)),
            current: None,
            ended: false,
        }))
    }

    /// The next picture as tightly packed I420, borrowed until the call after.
    fn next(&mut self) -> crate::Result<Option<(&[u8], &[u8], &[u8], u32, u32)>> {
        match self {
            Self::Still(still) => Ok(Some(still.picture())),
            Self::Streamed(stream) if stream.ended => Ok(None),
            Self::Streamed(stream) => match stream.frames.recv() {
                Ok(Ok(Some(frame))) => {
                    let frame = stream.current.insert(frame);
                    Ok(Some((
                        &frame.y,
                        &frame.u,
                        &frame.v,
                        frame.width,
                        frame.height,
                    )))
                }
                // The decoder said end of stream in as many words -- a source
                // shorter than the clip that names it, which the span loop ends
                // gracefully on.
                Ok(Ok(None)) => {
                    stream.ended = true;
                    Ok(None)
                }
                Ok(Err(e)) => Err(e),
                // The channel went quiet without either: the thread panicked.
                // Read as end of stream that would truncate the export at this
                // frame and *report success*, which is the one outcome an
                // export must never have -- before the decode moved onto a
                // thread the same panic took the whole export down, and it
                // still does.
                Err(_) => Err("the decoder stopped without a picture or a reason".into()),
            },
        }
    }
}

/// One span's decode, on its own thread: open the file (hardware if the plugin
/// will have it, software otherwise) and send every picture until the encoder
/// stops taking them.
///
/// Unlike playback there is no mid-clip fallback to software: a hardware decode
/// that fails after the first picture fails the export, which then deletes the
/// half-written file.
///
/// Every way this returns says which way it was, down the channel: a picture, an
/// error, or the `Ok(None)` that *is* end of stream. Closing the channel is not
/// one of them -- a silent close is a panicked thread, and the receiver treats
/// it as the failure it is rather than as a source that ran out.
fn decode_stream(path: &Path, start_frame: u32, tx: &SyncSender<crate::Result<Option<Yuv>>>) {
    let opened = match forced("VE_SW") {
        false => HwSession::open_at(path, start_frame).map(Pictures::Hw),
        true => None,
    };
    let mut decoder = match opened {
        Some(hw) => hw,
        None => match SwDecoder::open(path, start_frame) {
            Ok(sw) => Pictures::Sw(sw),
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        },
    };
    loop {
        // Copied out of the decoder's own buffer, which it reuses for the next
        // picture: one 3 MB memcpy per frame against the decode and the encode
        // it lets overlap.
        let frame = match decoder.next() {
            Ok(Some((y, u, v, width, height))) => Yuv {
                y: y.to_vec(),
                u: u.to_vec(),
                v: v.to_vec(),
                width,
                height,
            },
            Ok(None) => {
                let _ = tx.send(Ok(None));
                return;
            }
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        // The consumer went away: the span ended early, or the export was
        // cancelled. Either way there is nothing left to decode for.
        if tx.send(Ok(Some(frame))).is_err() {
            return;
        }
    }
}

/// The two stream decoders, on the thread that opened them.
enum Pictures {
    Hw(HwSession),
    Sw(SwDecoder),
}

impl Pictures {
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

    /// How a decode thread ended is never guessed from the channel being shut.
    /// A source that genuinely ran out says so, and the span ends on it; a
    /// thread that died without a word is a failure, because reading it as end
    /// of stream would truncate the picture and *report the export finished*.
    #[test]
    fn a_decoder_that_dies_without_a_word_is_not_end_of_stream() {
        let streamed = |handle, frames| {
            ClipDecoder::Streamed(Streamed {
                frames,
                _thread: DecodeThread(Some(handle)),
                current: None,
                ended: false,
            })
        };

        let (tx, frames) = sync_channel::<crate::Result<Option<Yuv>>>(2);
        // Said its piece, then finished: the span walks on to its next clip.
        let handle = thread::spawn(move || {
            tx.send(Ok(None)).expect("the receiver is alive");
        });
        let mut ended = streamed(handle, frames);
        assert!(ended.next().expect("a clean end is not an error").is_none());
        // ...and asking again is the same answer and not a disconnect error.
        assert!(ended.next().expect("still not an error").is_none());

        let (tx, frames) = sync_channel::<crate::Result<Option<Yuv>>>(2);
        // Died holding the sender: the channel shuts with nothing said.
        let handle = thread::spawn(move || {
            let _tx = tx;
            panic!("decoder died");
        });
        let mut died = streamed(handle, frames);
        let e = died
            .next()
            .expect_err("a silent death must fail the export, not end the clip");
        assert!(
            e.to_string().contains("without a picture or a reason"),
            "{e}"
        );
    }

    /// Three seconds of two tones, one per channel, interleaved: enough shape
    /// that a correlation says something and enough length that the encoder
    /// settles into a mode.
    fn two_tones(secs: usize) -> Vec<f32> {
        (0..OPUS_RATE as usize * secs)
            .flat_map(|i| {
                let t = i as f32 / OPUS_RATE as f32;
                let tau = std::f32::consts::TAU;
                [
                    0.3 * (tau * 440.0 * t).sin() + 0.2 * (tau * 1234.0 * t).sin(),
                    0.25 * (tau * 880.0 * t).sin(),
                ]
            })
            .collect()
    }

    /// The best correlation of one channel of `got` against `want`, and the
    /// offset it sat at.
    fn align(want: &[f32], got: &[f32]) -> (f64, usize) {
        let (mut best, mut at) = (-2.0, 0);
        let window = OPUS_RATE as usize;
        for lag in 0..1400 {
            let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
            for (x, y) in want[..window]
                .iter()
                .step_by(2)
                .zip(got[lag * 2..].iter().step_by(2))
            {
                num += f64::from(*x) * f64::from(*y);
                da += f64::from(*x) * f64::from(*x);
                db += f64::from(*y) * f64::from(*y);
            }
            let c = num / (da.sqrt() * db.sqrt()).max(1e-12);
            if c > best {
                (best, at) = (c, lag);
            }
        }
        (best, at)
    }

    fn ruopus_decode(packets: &[crate::AacPacket]) -> Vec<f32> {
        let mut decoder = ruopus::MultistreamDecoder::with_rate(OPUS_RATE, 1, 1, &[0, 1]);
        let mut pcm = Vec::new();
        for packet in packets {
            pcm.extend(
                decoder
                    .decode_packet(&packet.bytes)
                    .expect("a valid packet"),
            );
        }
        pcm
    }

    /// Why [`OPUS_MAX_KBPS`] is a cap, why [`OPUS_PRE_SKIP`] is a constant and
    /// why [`opus_fidelity`] exists at all -- all three measured here against
    /// `ruopus`, the decoder this project reads every Opus file back with, and
    /// which matches ffmpeg's libopus decode of a libopus file to correlation
    /// 1.00000 on this very signal.
    ///
    /// The last third **asserts the bug**: at 256 kbps stereo `opus-rs 0.1.26`
    /// writes packets that only its own decoder reads back, and pinning that
    /// here is what makes a version bump that fixes it *fail* -- which is the
    /// signal to raise the cap rather than a reason to widen it quietly.
    #[test]
    fn the_opus_encoder_is_pinned_to_the_band_it_was_measured_in() {
        let pcm = two_tones(3);
        for kbps in [96, OPUS_MAX_KBPS] {
            let (packets, pre_skip) = encode_opus(&pcm, kbps)
                .expect("the encoder opens")
                .expect("and inside the band it passes its own round trip");
            assert_eq!(pre_skip, OPUS_PRE_SKIP + OPUS_FRAME as u16);
            assert_eq!(
                packets.len(),
                pcm.len().div_ceil(OPUS_FRAME * 2) + 2,
                "one packet per 20 ms frame, plus the warm-up and the flush"
            );
            let decoded = ruopus_decode(&packets);
            let (c, lag) = align(&pcm, &decoded);
            // From the first audible sample, head included -- and the head is
            // why this is 0.98 and not the 0.999 below: the encoder's first
            // audible 20 ms still comes out about 6 dB down whatever is fed in
            // ahead of it (a second warm-up frame changes the numbers here in
            // no decimal place), so that ramp is a property of `opus-rs 0.1.26`
            // and is stated rather than hidden behind a loose threshold.
            assert!(c >= 0.98, "{kbps} kbps came back at correlation {c:.4}");
            let (settled, _) = align(&pcm[OPUS_FRAME * 2..], &decoded[OPUS_FRAME * 2..]);
            assert!(
                settled >= 0.999,
                "{kbps} kbps past the first frame: correlation {settled:.4}"
            );
            // The whole point of the number in the header: drop exactly that
            // many samples and the sound starts where the timeline says. A lag
            // that is not it is a file that plays late or eats its own head.
            assert_eq!(
                lag,
                usize::from(pre_skip),
                "{kbps} kbps: the declared pre-skip is not where the sound starts"
            );
            // ...and the tail is all there: the flush frame pushed the last
            // samples out, so what is left after the pre-skip covers the input.
            assert!(
                decoded.len() - usize::from(pre_skip) * 2 >= pcm.len(),
                "{kbps} kbps: the track ends {} samples short of the timeline",
                (pcm.len() + usize::from(pre_skip) * 2 - decoded.len()) / 2
            );
            // The gate the export runs before it writes anything, on a track
            // that deserves to pass it.
            let fidelity = opus_fidelity(&packets, &pcm, usize::from(pre_skip));
            assert!(
                fidelity >= OPUS_MIN_FIDELITY,
                "{kbps} kbps scored {fidelity:.4} on its own round trip"
            );
        }

        // Above the band, straight at the crate, since `encode_opus` clamps: the
        // packets come back as noise, and the gate says so. This is the check
        // that the guard actually catches the failure it is built for -- and the
        // one that fails, loudly, the day `opus-rs` fixes it.
        let mut encoder =
            opus_rs::OpusEncoder::new(OPUS_RATE as i32, 2, opus_rs::Application::Audio).unwrap();
        encoder.bitrate_bps = 256_000;
        encoder.complexity = 10;
        let mut out = vec![0u8; 1500];
        let packets: Vec<crate::AacPacket> = pcm
            .chunks_exact(OPUS_FRAME * 2)
            .map(|block| {
                let len = encoder.encode(block, OPUS_FRAME, &mut out).unwrap();
                crate::AacPacket {
                    bytes: out[..len].to_vec(),
                    samples: OPUS_FRAME as u32,
                }
            })
            .collect();
        let fidelity = opus_fidelity(&packets, &pcm, usize::from(OPUS_PRE_SKIP));
        assert!(
            fidelity < OPUS_MIN_FIDELITY,
            "opus-rs now encodes 256 kbps stereo that a conformant decoder reads \
             back (fidelity {fidelity:.4}): raise OPUS_MAX_KBPS and delete this half"
        );
    }

    /// The gate on a track too long to listen to whole: it samples, and the
    /// middle of what it hears is the answer. A track whose sound is *not* what
    /// was encoded fails it; one bad five-second window does not, which is
    /// deliberate and measured ([`opus_fidelity`] carries the distribution:
    /// five windows of a good film's mix score under 0.95, the lowest 0.803).
    /// A silent track is not a failure either, which is what it used to be
    /// scored as -- 0.0 against a denominator of nothing, and an all-quiet
    /// timeline fell back to AAC for it.
    ///
    /// 61 s of sound: one second past the minute the gate stops listening whole,
    /// which is what puts this on the sampled path at all.
    #[test]
    fn a_long_track_is_judged_by_the_middle_of_what_it_samples() {
        let pcm = two_tones(61);
        let (packets, pre_skip) = encode_opus(&pcm, 96)
            .expect("the encoder opens")
            .expect("and passes its own round trip");
        assert!(
            packets.len() > FIDELITY_WINDOW * FIDELITY_WINDOWS,
            "{} packets is not the sampled path",
            packets.len()
        );
        let pre_skip = usize::from(pre_skip);
        let whole = opus_fidelity(&packets, &pcm, pre_skip);
        assert!(whole >= OPUS_MIN_FIDELITY, "a good track scored {whole:.4}");

        // One window's worth of the *mix* replaced by its own phase inverse:
        // the packets still decode, they are simply no longer this sound. Placed
        // on a window the gate samples -- window 6 of the twelve, by the same
        // arithmetic the gate spreads them with. The track still passes, and
        // that is the measured decision, not an oversight: this codec really
        // does score 0.80 on five seconds of film it otherwise handles at 0.998.
        let step = (packets.len() - FIDELITY_WINDOW) / (FIDELITY_WINDOWS - 1);
        let from = (6 * step * OPUS_FRAME).saturating_sub(pre_skip) * 2;
        let mut one_bad = pcm.clone();
        for sample in &mut one_bad[from..(from + FIDELITY_WINDOW * OPUS_FRAME * 2).min(pcm.len())] {
            *sample = -*sample;
        }
        let dip = opus_fidelity(&packets, &one_bad, pre_skip);
        assert!(
            dip >= OPUS_MIN_FIDELITY,
            "one ruined window sank the whole track ({dip:.4})"
        );

        // ...and the shape the gate is actually for, which is what a rate the
        // encoder cannot hold does: the track is not the mix any more, not one
        // window of it. The middle of the list moves with it.
        let mut ruined = pcm.clone();
        for sample in &mut ruined[pcm.len() / 4..] {
            *sample = -*sample;
        }
        let ruined = opus_fidelity(&packets, &ruined, pre_skip);
        assert!(
            ruined < OPUS_MIN_FIDELITY,
            "a track that is no longer the mix scored {ruined:.4}"
        );

        // ...and silence is silence: nothing to correlate is not a round trip
        // that failed. Short, because the floor is the same on either path.
        let quiet = vec![0f32; OPUS_RATE as usize * 2 * 2];
        let (packets, pre_skip) = encode_opus(&quiet, 96)
            .expect("the encoder opens")
            .expect("silence is written as Opus, not sent to AAC");
        let fidelity = opus_fidelity(&packets, &quiet, usize::from(pre_skip));
        assert!(
            fidelity >= OPUS_MIN_FIDELITY,
            "a silent track scored {fidelity:.4} and would fall back to AAC"
        );
    }

    /// The one rule that keeps a picture from being converted twice: a frame the
    /// tone map has already brought to BT.709 is never handed the 2020 matrix as
    /// well, on either canvas -- and everything else about the remap is
    /// unchanged by the tone map existing.
    #[test]
    fn a_tone_mapped_span_is_never_matrix_remapped_too() {
        let hdr = ColorDescription {
            matrix: Matrix::Bt2020Ncl,
            transfer: Transfer::Pq,
            full_range: false,
        };
        let sdr = ColorDescription::default(); // BT.601
        for out in [Matrix::Bt709, Matrix::Bt601] {
            let toned = remap_into(Some(hdr), true, out);
            assert!(
                toned.is_none_or(|(from, _)| from == Matrix::Bt709),
                "tone-mapped onto a {out:?} canvas asked for {toned:?}"
            );
            // Untouched by any of this: the 2020 clip that was *not* tone-mapped
            // (an unreachable state today, and still the matrix answer), the
            // ordinary 601-on-709 reconcile, and a gap.
            assert_eq!(
                remap_into(Some(hdr), false, out),
                (out != Matrix::Bt2020Ncl).then_some((Matrix::Bt2020Ncl, out))
            );
            assert_eq!(
                remap_into(Some(sdr), false, out),
                (out != Matrix::Bt601).then_some((Matrix::Bt601, out))
            );
            assert_eq!(remap_into(None, false, out), None);
        }
        assert_eq!(
            remap_into(Some(hdr), true, Matrix::Bt601),
            Some((Matrix::Bt709, Matrix::Bt601)),
            "a tone-mapped clip below 720 lines still takes its 709 -> 601 step"
        );
    }

    /// A track of three cues, in whichever clock `track` says: `Some(n)` is a
    /// track of the media file, `None` a `.srt` beside it.
    fn subtitles(path: &str, track: Option<u64>) -> SubtitleTrack {
        SubtitleTrack {
            path: path.into(),
            track,
            language: "eng".into(),
            name: String::new(),
            label: "eng".into(),
            bitmap: false,
            cues: vec![
                Cue {
                    start_us: 500_000,
                    end_us: 1_500_000,
                    text: "first".into(),
                    image: None,
                },
                Cue {
                    start_us: 2_000_000,
                    end_us: 3_250_000,
                    text: "second".into(),
                    image: None,
                },
                Cue {
                    start_us: 4_000_000,
                    end_us: 4_750_000,
                    text: "third".into(),
                    image: None,
                },
            ],
            refused: None,
        }
    }

    /// A standalone subtitle file is timed against the timeline itself -- there
    /// is no clip to hang it on -- so a cut moves the pictures and not it. What
    /// it does get is the exported length: a cue past the end of the file would
    /// otherwise be a block after the last picture.
    ///
    /// The pair of this, the embedded clock, is measured against a real exported
    /// file in `tests/export_subs.rs`; both live in one function and this is the
    /// half that needs no encoder.
    #[test]
    fn a_subtitle_file_beside_the_media_keeps_the_timelines_own_clock() {
        // 5 seconds at 30, with `[0.5s, 2.5s)` cut out of it: 3 seconds left.
        let mut project = Project::single("/nonexistent/film.mp4", 150);
        let track = subtitles("/nonexistent/film.srt", None);
        assert!(project.ripple_delete(15, 60));
        assert_eq!(project.timeline_frames(), 90);
        let cues = timeline_cues(&project, &track, 30.0);
        assert_eq!(
            cues,
            vec![
                Cue {
                    start_us: 500_000,
                    end_us: 1_500_000,
                    text: "first".into(),
                    image: None,
                },
                // Clipped to the three seconds the export writes...
                Cue {
                    start_us: 2_000_000,
                    end_us: 3_000_000,
                    text: "second".into(),
                    image: None,
                },
                // ...and the one wholly past the end is not written at all.
            ]
        );
        // A track *of a file* the project no longer plays has no span to be
        // carried through either, so it falls back to the same clock rather
        // than exporting nothing.
        let orphan = subtitles("/nonexistent/other.mkv", Some(2));
        assert_eq!(timeline_cues(&project, &orphan, 30.0).len(), 2);
    }

    /// The embedded clock, without an encoder in it: the cut moves the cues with
    /// the pictures, and a cue nothing clipped keeps its own microseconds.
    #[test]
    fn a_track_of_the_media_is_carried_through_the_cut() {
        let mut project = Project::single("/nonexistent/film.mkv", 150);
        let track = subtitles("/nonexistent/film.mkv", Some(2));
        assert!(project.ripple_delete(15, 60), "cut 0.5s..2.5s out");
        let cues = timeline_cues(&project, &track, 30.0);
        assert_eq!(
            cues,
            vec![
                // The first cue was wholly inside the cut. The second straddled
                // its far edge and kept the 0.75s that is still there...
                Cue {
                    start_us: 500_000,
                    end_us: 1_250_000,
                    text: "second".into(),
                    image: None,
                },
                // ...and the third simply moves back the two seconds that went.
                Cue {
                    start_us: 2_000_000,
                    end_us: 2_750_000,
                    text: "third".into(),
                    image: None,
                },
            ]
        );
    }

    #[test]
    fn bitrate_clamps_at_both_ends() {
        let meta = |width, height, frame_rate| VideoMeta {
            width,
            height,
            frame_rate,
            frame_count: 1,
            codec: crate::demux::Codec::H264,
            color: Default::default(),
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
            color: Default::default(),
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
