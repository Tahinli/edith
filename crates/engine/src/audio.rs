//! Background audio decode worker: pulls a source's audio track and hands
//! interleaved f32 over a bounded channel, same shape as `decode`. Uses its own
//! reader so the video demuxer stays single-owner.
//!
//! Three readers, one output. An mp4 goes through `mp4` as it always has -- that
//! path also yields the raw access units the export copies, which is the only
//! reason it is kept separate -- and every AAC stream, from any container, is
//! decoded by `ec-aac`, HE-AAC (SBR) included. A standalone
//! audio file (`crate::is_audio`: mp3, wav, flac, ogg, ALAC, ADTS) goes through
//! symphonia's own format probe, as does the AAC track of a **Matroska** file:
//! same reader, `mkv` instead of `isomp4`, and a 5.1 film soundtrack folded to
//! stereo on the way out ([`SymDecoder`]). An ADTS file is read by [`crate::adts`],
//! since symphonia's ADTS reader sits behind the `aac` feature this project does
//! not link. An mp4's **AC-3** track is the third: the same
//! sample tables, decoded by `ec-ac3` and downmixed to stereo by the
//! decoder itself, which is what lets a 5.1 BluRay remux play on a stereo
//! timeline. Everything downstream of [`Track`] sees the
//! same samples either way; only the packet copy asks which reader it came from,
//! because there is nothing to copy out of an mp3 that an mp4 can hold.
//!
//! One output, however many lanes: a timeline's audio lanes are decoded side by
//! side and summed ([`AudioSession::open_mixed_streams`]), which is what both
//! the device and an audio-only export are handed. One lane skips the mixer
//! entirely and is the plain single-stream path it always was.
//!
//! Both readers are addressed as `(path, stream)`. An mp4 may carry several
//! audio tracks and the caller picks one; a standalone audio file carries
//! exactly one, so any stream above 0 there is an `Err` rather than a silent
//! fallback to the only track it has.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::mp4track;
use ec_core::CodecId;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions,
    well_known::{CODEC_ID_AAC, CODEC_ID_OPUS, CODEC_ID_VORBIS},
};
use symphonia_core::formats::probe::Hint;
use symphonia_core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo, TrackType as SymKind,
};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataOptions;
use symphonia_core::packet::Packet;
use symphonia_core::units;
use symphonia_core::units::{Time, TimeBase};

use crate::duck::{Duck, Ducker};
use crate::eq::{EqParams, EqState};
use crate::limiter::{Limiter, LimiterState};
use crate::stretch::TimeStretch;
use crate::voice::VoiceFx;

/// The rate a mix runs at when nothing picks one: the highest rate among the
/// sources it names, so a 22.05 kHz voiceover beside a 48 kHz bed is mixed at
/// 48 kHz and the bed is not resampled down to the voice. A mix whose sources
/// are all 44.1 kHz stays 44.1 kHz, and 48 kHz-only stays 48 kHz. Only when
/// every source is below 44.1 kHz (a 22.05 kHz voiceover alone, say) does the
/// mix go to 48 kHz, which every AAC output accepts and which a WebM or
/// Matroska Opus output needs. `None` when no source has a track.
fn mix_rate(rates: impl IntoIterator<Item = u32>) -> Option<u32> {
    rates
        .into_iter()
        .max()
        .map(|rate| if rate < 44_100 { 48_000 } else { rate })
}

/// ffmpeg's AAC-LC encoder delay, used when the file carries no edit list.
/// (HE-AAC/iTunes files use 2112, but those are rejected as unsupported here.)
const DEFAULT_PRIMING: u64 = 1024;

/// What the sound of a *Matroska* file is read with, named for a refusal:
/// symphonia's registry as this project enables it (see the feature list in
/// `Cargo.toml`) plus the families no symphonia version carries and this
/// project decodes anyway -- AAC through `ec-aac` ([`SymDecoder::Aac`]),
/// AC-3/E-AC-3 through `ec-ac3` ([`MkvAc3Track`]) and Opus through `ec-opus`
/// ([`SymDecoder::Opus`]).
///
/// Written down per container because capability is not one set: an mp4 video's
/// sound never reaches symphonia ([`Track::open`] answers before it), so it
/// reads fewer codecs than a Matroska file does. Both strings were "AAC only"
/// long after that stopped being true, which is how a user came to re-encode a
/// film whose FLAC track already played; `capability_matrix` opens one file per
/// codec named here.
const MKV_DECODED: &str = "AAC, AC-3/E-AC-3, ALAC, FLAC, MP3, Opus, PCM, TrueHD/MLP and Vorbis";

/// The name a person knows a sound codec by, from a Matroska `CodecID` (`A_WAVPACK4`, `A_MPEG/L2`)
/// or a bare codec tag (`WAVPACK4`, `MPC`). A tag nothing names comes back as its own text with the
/// `A_` prefix and any `/` suffix taken off, which is still better than the id.
pub fn codec_display_name(raw: &str) -> String {
    let id = raw.strip_prefix("A_").unwrap_or(raw);
    let upper = id.to_ascii_uppercase();
    let known = match upper.as_str() {
        "MPEG/L1" => "MP1",
        "MPEG/L2" => "MP2",
        "MPEG/L3" => "MP3",
        "WAVPACK4" | "WAVPACK" => "WavPack",
        "MPC" | "MPC8" | "MUSEPACK" => "Musepack",
        "TTA" | "TTA1" => "TTA",
        "ALAC" => "ALAC",
        "SPEEX" => "Speex",
        "AC3" | "AC-3" => "AC-3",
        "EAC3" | "E-AC-3" => "E-AC-3",
        "MS/ACM" => "ACM",
        "TRUEHD" | "MLP" => "TrueHD",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    let family = upper.split('/').next().unwrap_or(&upper);
    let named = match family {
        "WMA" | "WMAV1" | "WMAV2" | "WMAPRO" | "WMALOSSLESS" => "WMA",
        "AAC" => "AAC",
        "DTS" => "DTS",
        "VORBIS" => "Vorbis",
        "OPUS" => "Opus",
        "FLAC" => "FLAC",
        "REAL" => "RealAudio",
        "PCM" => "PCM",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_string();
    }
    id.split('/').next().unwrap_or(id).to_string()
}

/// ...and the mp4 half: [`AacTrack`] and [`Ac3Track`], which is where a video
/// file's sound is read and where it stops.
const MP4_DECODED: &str = "AAC-LC, HE-AAC v1/v2 and AC-3/E-AC-3";

/// Packets decoded and thrown away ahead of a seek target: one for the MDCT
/// overlap-add that reconstructs the target packet, one to warm the decoder.
const PRE_ROLL: u32 = 2;

/// Frames per channel one AAC-LC packet carries. Fixed by the codec.
pub(crate) const SAMPLES_PER_PACKET: u32 = 1024;

/// Syncframes decoded and thrown away ahead of an AC-3 seek target: one, for the
/// 256-sample overlap-add the target frame is reconstructed with.
const AC3_PRE_ROLL: u32 = 1;

/// Frames per channel a seek into a standalone audio file lands ahead of the
/// window, decoded and thrown away: enough to refill an mp3's bit reservoir
/// (a frame is 1152 samples and the reservoir reaches back two of them) at any
/// rate we accept.
const SYM_PRE_ROLL: u64 = 8192;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioMeta {
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames per channel the container claims, priming subtracted. `None` when
    /// the header does not say.
    pub total_samples: Option<u64>,
}

/// One decoder output block, ready to hand to an output device.
pub struct AudioChunk {
    /// Position of the first frame, in samples-per-channel counted from the
    /// first audible sample: priming is already dropped, so 0 is media time 0.
    pub start_sample: u64,
    /// Interleaved, `AudioMeta::channels` values per frame.
    pub samples: Vec<f32>,
}

/// What a writer needs to declare an AAC track that plays copied packets: the
/// esds fields verbatim from the source, no re-derivation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AacTrackParams {
    pub freq_index: u8,
    pub chan_conf: u8,
    pub sample_rate: u32,
}

/// One raw AAC access unit exactly as it sits in the source `mdat` — no ADTS
/// header, which is the form an `mp4a` sample table wants.
pub struct AacPacket {
    pub bytes: Vec<u8>,
    /// Frames per channel it decodes to, for the writer's stts.
    pub samples: u32,
}

/// Everything an import check needs from a candidate file's audio track. Two
/// files may share a timeline when their layouts agree (or both are `None`).
///
/// Rate and layout, of which only the **layout** is held to the timeline's: one
/// output device and one exported track carry one width, while a rate of its own
/// is resampled at the decoder's door ([`Resample`]). An mp3 that agrees on the
/// layout may join a timeline of mp4s even though nothing about it can be
/// *copied* into an export -- that is a separate refusal, at export time, in
/// [`AudioSession::copy_multi_segments`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioProbe {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A diagnostic line for stderr, printed only under `EDITH_DEBUG` and then once
/// per distinct line for the life of the process. The same file is opened many
/// times in one run (every probe, every open), and an unsupported track used to
/// print the same line each time. Anything a user must act on goes in a returned
/// warning instead ([`crate::PlaybackSession::import_with_warnings`]).
pub(crate) fn debug_once(line: String) {
    static SEEN: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    if std::env::var_os("EDITH_DEBUG").is_none() {
        return;
    }
    let seen = SEEN.get_or_init(Default::default);
    let fresh = seen.lock().map(|mut set| set.insert(line.clone())).unwrap_or(true);
    if fresh {
        eprintln!("{line}");
    }
}

/// One audio stream of a file as its header describes it. Every audio track is
/// described, decodable or not: a picker shows the ones we refuse greyed out
/// rather than pretending a BluRay remux has fewer streams than it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Position among this file's audio tracks in file order — the `stream`
    /// [`AudioSession::open_multi_streams`] takes.
    pub index: usize,
    /// Codec, named by its sample entry's fourcc read out of the file by hand
    /// (mp4 0.14 keeps none for a kind it does not parse). `"unknown"` only where
    /// the fourcc itself cannot be read.
    pub codec: String,
    /// `0` for a stream whose sample entry mp4 0.14 does not parse.
    pub channels: u16,
    /// `0` for a stream whose sample entry mp4 0.14 does not parse.
    pub sample_rate: u32,
    /// ISO-639-2 from the mdhd, `None` for the `und` most muxers write.
    pub lang: Option<String>,
    /// Whether opening this stream would decode -- a codec question only now
    /// that no width is refused. The grey-out rule, mirroring what `Track::open`
    /// answers one file at a time.
    pub decodable: bool,
    /// Bits per sample, for a PCM track whose sample entry states them. `None`
    /// for every compressed codec, and for a Matroska track (its `BitDepth` is
    /// not read here).
    pub bit_depth: Option<u8>,
    /// Bits per second this track's own samples take over the file's stated
    /// playing time -- the same measure as [`crate::MediaBitrate::audio`]. `None`
    /// where the header does not state the length, and for Matroska, whose
    /// per-track bytes need a cluster walk this listing does not do.
    pub bitrate: Option<u64>,
}

/// The mix settings a *running* mixer reads: what each lane is turned by on its
/// way into the sum and the ceiling the sum comes out through, both swappable
/// while the stream plays.
///
/// This is what keeps a fader off the reseek path. Everything else a timeline
/// says lives inside a decode worker -- a segment's window, its curve, its rate
/// -- and can only be changed by rebuilding it, which is a flush, a re-open and
/// a hole in the sound. These two live at the sum, where nothing is decoded, so
/// the mixer can pick them up between one block and the next: an arrow key held
/// down on a fader moves the level and nothing else moves at all.
///
/// One of these belongs to one mixer thread. [`is_live`](MixControls::is_live)
/// is what says whether a mixer is actually reading it -- a single lane at
/// unity with the limiter off is not mixed at all
/// ([`AudioSession::open_mixed_streams_master`]), and a caller that finds this
/// dead has to reopen the stream to be heard.
pub struct MixControls {
    /// Bumped by every change; the mixer reloads when the number it last saw
    /// is not this one, so an unchanged mix costs one relaxed load per block.
    version: AtomicU64,
    /// The lane amplitudes and the limiter, together: they are set together and
    /// a mixer that read half of one change and half of the next would be
    /// mixing a state nobody asked for.
    state: Mutex<(Vec<f32>, Vec<Vec<Duck>>, Limiter)>,
    /// A mixer thread has these; see the type docs.
    live: AtomicBool,
    /// The open these were handed to finished without a mixer: a silent
    /// timeline, or a source that would not open. Set by whoever ran that open
    /// ([`PlaybackSession::start_audio`] does it from the feeder), because
    /// [`Self::live`] alone cannot tell "no mixer will ever read this" from
    /// "the mixer is still being opened" -- and a caller that cannot tell those
    /// apart either rebuilds the sound under a cold open, once per fader nudge,
    /// or hands a mix to nobody and never notices.
    detached: AtomicBool,
    /// The highest level each lane has reached since it was last read, after
    /// its gain and before the sum. Written by the mixer once per block (never
    /// on the device's callback); read and cleared by [`Self::take_lane_peaks`].
    lane_peaks: Mutex<Vec<f32>>,
}

impl MixControls {
    pub fn new(gains: Vec<f32>, ducks: Vec<Vec<Duck>>, limiter: Limiter) -> Arc<Self> {
        Arc::new(Self {
            version: AtomicU64::new(0),
            lane_peaks: Mutex::new(vec![0.0; gains.len()]),
            state: Mutex::new((gains, ducks, limiter)),
            live: AtomicBool::new(false),
            detached: AtomicBool::new(false),
        })
    }

    /// Raises lane `lane`'s peak to `peak` if that is higher. A lane with no
    /// slot (a mix handed more lanes than it was built with) is ignored rather
    /// than grown here: it is a meter, not the mix.
    fn note_lane_peak(&self, lane: usize, peak: f32) {
        if let Some(held) = self.lane_peaks.lock().unwrap().get_mut(lane)
            && peak > *held
        {
            *held = peak;
        }
    }

    /// The per-lane peaks since the last call, and starts them over from zero.
    pub fn take_lane_peaks(&self) -> Vec<f32> {
        let mut held = self.lane_peaks.lock().unwrap();
        let taken = held.clone();
        held.iter_mut().for_each(|p| *p = 0.0);
        taken
    }

    /// Hands the running mixer a new mix. Cheap enough for a key repeat: a lock
    /// held for a `Vec` swap and an atomic.
    pub fn set(&self, gains: Vec<f32>, ducks: Vec<Vec<Duck>>, limiter: Limiter) {
        *self.state.lock().unwrap() = (gains, ducks, limiter);
        self.version.fetch_add(1, Ordering::Release);
    }

    /// Whether a mixer thread is reading this at all.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }

    /// Says the open is over and left no mixer behind; see [`Self::detached`].
    pub fn detach(&self) {
        self.detached.store(true, Ordering::Release);
    }

    /// Whether anything can still hear a [`set`](Self::set): a mixer reading
    /// these, or an open still on its way to being one.
    pub fn is_reachable(&self) -> bool {
        !self.detached.load(Ordering::Acquire)
    }

    /// The mix, if it has moved since `seen`. `None` -- the every-block case --
    /// takes no lock.
    #[allow(clippy::type_complexity)]
    fn changed(&self, seen: &mut u64) -> Option<(Vec<f32>, Vec<Vec<Duck>>, Limiter)> {
        let now = self.version.load(Ordering::Acquire);
        if now == *seen {
            return None;
        }
        *seen = now;
        Some(self.state.lock().unwrap().clone())
    }
}

pub struct AudioSession;

impl AudioSession {
    /// Opens the audio track of `path`. `Ok(None)` means the file simply has no
    /// audio, which is a valid session; `Err` is a real failure.
    pub fn open(
        path: impl AsRef<Path>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_at(path, 0.0)
    }

    /// Like [`open`](Self::open) but the first chunk starts at `start_secs` on
    /// the audible timeline; `start_sample` stays absolute from audible zero, so
    /// the output is the tail of a full run. Past the end yields no chunks.
    pub fn open_at(
        path: impl AsRef<Path>,
        start_secs: f64,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_segments(path, &[(start_secs, f64::INFINITY)])
    }

    /// Decodes `segs` — half-open `[start, end)` windows in *source* seconds —
    /// back to back as one continuous stream: the chunks carry no gap at a join,
    /// so pouring them into a ring is seamless. Each segment gets a fresh
    /// decoder and its own pre-roll, which makes a seeked segment perceptually
    /// but not bit-exactly equal to the same window of a full run (perceptual
    /// noise substitution reseeds; measured below 1e-3).
    ///
    /// `start_sample` counts from the *first* segment's own audible start, so a
    /// single segment reads as absolute media time (what [`open_at`](Self::open_at)
    /// promises) and a list reads as one timeline. Segment ends past the track
    /// are capped; an empty list is a valid session with no chunks.
    pub fn open_segments(
        path: impl AsRef<Path>,
        segs: &[(f64, f64)],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        let segs: Vec<_> = segs.iter().map(|&(s, e)| (Some(0), s, e)).collect();
        Self::open_multi_segments(&[path.as_ref().to_path_buf()], &segs)
    }

    /// [`open_segments`](Self::open_segments) over several files: a segment names
    /// its source by index into `sources`, and a source join behaves exactly like
    /// a cut inside one file — `start_sample` stays continuous, the decoder is
    /// fresh either way. Each source contributes *its own* priming, packet table
    /// and length, which is the whole reason the resolution is per source.
    ///
    /// A segment naming **no** source is a *gap*: that many seconds of silence
    /// are synthesised into the stream as ordinary chunks. Real chunks, not a
    /// pause — the master clock counts what the device has been fed, so a hole
    /// that fed nothing would stall the whole timeline on it.
    ///
    /// A segment naming a source that has **no track** is the same silence: a
    /// clip of a silent file is a hole with a picture over it, and the timeline
    /// let it in on exactly that footing ([`crate::PlaybackSession::import`]).
    ///
    /// Only the sources some segment names are opened. `Ok(None)` when `sources`
    /// is empty or its first entry has no AAC track; a later source that
    /// disagrees on *layout* is an `Err` — import refuses those up front (one
    /// timeline, one output device), this is the backstop. A source at another
    /// sample rate is not one of those: it is resampled, per segment.
    pub fn open_multi_segments(
        sources: &[PathBuf],
        segs: &[(Option<usize>, f64, f64)],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        let sources: Vec<_> = sources.iter().map(|p| (p.clone(), 0)).collect();
        Self::open_multi_streams(&sources, segs)
    }

    /// [`open_multi_segments`](Self::open_multi_segments) with each source
    /// naming *which* of its audio streams to decode — files carrying several
    /// (a remux with one track per language) are otherwise stuck on their first.
    /// The index counts audio tracks in file order, as
    /// [`probe_streams`](Self::probe_streams) lists them; a stream that does not
    /// exist or does not decode is an `Err` here, never a panic.
    pub fn open_multi_streams(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_multi_streams_eq(sources, segs, &[])
    }

    /// [`open_multi_streams`](Self::open_multi_streams) with an equalizer per
    /// segment: `eqs[i]` is what segment `i` plays through, `None` for one that
    /// plays flat. Filtering happens **per segment**, inside the worker, before
    /// anything is mixed -- two clips with different curves must not be blurred
    /// into one another, and a segment's filter memory starts clean and dies
    /// with it, which is also what makes a seek a reset for free.
    ///
    /// A parallel list, not a fourth tuple element: see
    /// [`crate::Project::audio_eqs_from`], which is what builds it. A list
    /// shorter than `segs` (the empty one every plain caller passes) means the
    /// rest play flat, so a mismatch cannot panic and cannot silently shift a
    /// curve onto the wrong clip.
    pub fn open_multi_streams_eq(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
        eqs: &[Option<EqParams>],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_multi_streams_speed(sources, segs, eqs, &[])
    }

    /// [`open_multi_streams_eq`](Self::open_multi_streams_eq) with a *rate* per
    /// segment as well: `speeds[i]` says how fast segment `i` is read and how
    /// much timeline it owes ([`crate::project::Stretch`]), `None` for one at
    /// real time. [`crate::Project::audio_speeds_from`] is what builds it, beside
    /// the very list the segments come from.
    ///
    /// Resampling happens **per segment**, inside the worker and before the mix,
    /// for the equalizer's reasons: two clips at two speeds must not be read
    /// into one another, and the interpolator's memory dies with the segment. A
    /// list shorter than `segs` (the empty one every plain caller passes) means
    /// the rest play at real time, so nothing here can shift a rate onto the
    /// wrong clip and a project with no speeds decodes exactly the samples it
    /// always did.
    pub fn open_multi_streams_speed(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
        eqs: &[Option<EqParams>],
        speeds: &[Option<crate::project::Stretch>],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_multi_streams_speed_at(sources, segs, eqs, speeds, None)
    }

    /// [`open_multi_streams_speed`] with the project's own rate, when one was
    /// picked ([`crate::edith::Document::sample_rate`]), overriding the source's
    /// derived one. The per-segment [`Rate`] chain below already conforms any
    /// segment whose own rate differs from `meta.sample_rate`, so naming a rate
    /// here costs nothing new -- it only changes which rate that chain conforms
    /// *to*.
    pub fn open_multi_streams_speed_at(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
        eqs: &[Option<EqParams>],
        speeds: &[Option<crate::project::Stretch>],
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        // No fades on this path, so the fps a `Fade` would be rescaled by is
        // never read; any finite value does.
        Self::open_multi_streams_speed_at_fade(sources, segs, eqs, speeds, &[], 1.0, sample_rate)
    }

    /// [`open_multi_streams_speed_at`](Self::open_multi_streams_speed_at) with a
    /// fade envelope per segment: `fades[i]` is what segment `i` plays through
    /// ([`emit`]'s last effect, after the rate and the equalizer), built the
    /// same way `eqs` and `speeds` are -- one entry per `segs`, a short list
    /// meaning flat from there on. `fps` is the timeline rate each `Fade` was
    /// built at ([`crate::Project::audio_fades_from`]) -- this is the one
    /// place that rate meets the audio one ([`crate::project::Fade::scaled`]).
    pub fn open_multi_streams_speed_at_fade(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
        eqs: &[Option<EqParams>],
        speeds: &[Option<crate::project::Stretch>],
        fades: &[Option<crate::project::Fade>],
        fps: f64,
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_lane(sources, segs, eqs, speeds, fades, fps, sample_rate, None)
    }

    /// [`open_multi_streams_speed_at_fade`](Self::open_multi_streams_speed_at_fade)
    /// with the timeline's width given instead of derived from this lane alone.
    /// A mix has one width for every lane ([`mix_width`]): a lane whose own
    /// sources are all mono must still run at the mix's width, or its samples
    /// are summed as frames of the wrong size and it plays at half its length.
    #[allow(
        clippy::too_many_arguments,
        reason = "one private opener behind the public family; the parameters are the public ones plus the width"
    )]
    fn open_lane(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
        eqs: &[Option<EqParams>],
        speeds: &[Option<crate::project::Stretch>],
        fades: &[Option<crate::project::Fade>],
        fps: f64,
        sample_rate: Option<u32>,
        width: Option<u16>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        // Every source the segments name is opened here, once, before the
        // timeline's width is settled. The width is the *widest* of them: a mono
        // voiceover first on the lane with a stereo music bed under it used to
        // pin the whole mix to mono and fold the bed down with it. The rate and
        // the length are still the first opened source's, as they always were.
        // A source with no audio track at all is silence -- its segments are
        // silence, exactly as a gap's are, and the slot stays empty to say so --
        // and a source that cannot be opened is an error once a segment names it.
        let mut named = vec![false; sources.len()];
        for &(source, ..) in segs {
            if let Some(source) = source {
                *named
                    .get_mut(source)
                    .ok_or_else(|| format!("segment names source {source} of {}", sources.len()))? = true;
            }
        }
        // The *rate* is not held to the timeline's: a source written at another
        // one is read through its segment's [`Resample`], the very interpolator a
        // speeded clip already goes through, so it plays at the pitch it was
        // recorded at. The layout is held: see the width check below. A still
        // image has no track by definition.
        // The length is still the first non-image source's that opens, named
        // or not, as it always was: an unnamed source is opened for that alone, and
        // one that cannot be opened is skipped rather than refused. Only the named
        // ones take part in the width and the rate, and only they are played.
        let mut opened: Vec<Option<Track>> = sources.iter().map(|_| None).collect();
        for (at, (path, stream)) in sources.iter().enumerate() {
            if crate::is_image(path) {
                continue;
            }
            opened[at] = if named[at] {
                Track::open(path, *stream)?
            } else {
                Track::open(path, *stream).ok().flatten()
            };
        }
        let Some(first_at) = opened.iter().position(Option::is_some) else {
            return Ok(None);
        };
        let first = opened[first_at].as_ref().expect("found above");
        // The rate, when no caller picked one: the highest of the named sources'
        // (see [`mix_rate`]). A lane of only unnamed sources falls back to the
        // first one that opened.
        let rate = sample_rate.unwrap_or_else(|| {
            mix_rate(
                opened
                    .iter()
                    .zip(&named)
                    .filter(|(_, named)| **named)
                    .filter_map(|(track, _)| track.as_ref())
                    .map(|t| t.sample_rate()),
            )
            .unwrap_or_else(|| first.sample_rate())
        });
        // The length is counted in samples of the first source's own rate, so it
        // is restated at the mix's rate: the same seconds either way.
        let length = first.total_samples().map(|n| {
            n.saturating_mul(u64::from(rate)) / u64::from(first.sample_rate().max(1))
        });
        let meta = AudioMeta {
            sample_rate: rate,
            channels: width.unwrap_or_else(|| {
                opened
                    .iter()
                    .zip(&named)
                    .filter(|(_, named)| **named)
                    .filter_map(|(track, _)| track.as_ref())
                    .map(|t| t.channels())
                    .max()
                    .unwrap_or_else(|| first.channels())
            }),
            total_samples: length,
        };

        let mut tracks: Vec<Option<Track>> = sources.iter().map(|_| None).collect();
        for (source, track) in opened.into_iter().enumerate() {
            let Some(track) = track.filter(|_| named[source]) else {
                continue;
            };
            // One output device carries one width, and everything wider than a
            // pair is already folded to one. A mono source is the one exception
            // (DEBT #46): [`widen`] duplicates it into every channel of a stereo
            // timeline. The same gate as [`PlaybackSession`]'s own
            // ([`crate::playback::audio_matches_probed`]), so what plays exports.
            let mono_into_wider = track.channels() == 1 && meta.channels > 1;
            if track.channels() != meta.channels && !mono_into_wider {
                return Err(format!(
                    "source {source} is {} ch, the timeline is {} ch",
                    track.channels(),
                    meta.channels
                )
                .into());
            }
            track.check_decoder()?;
            tracks[source] = Some(track);
        }

        let owed_by_segment = owed_samples(segs, speeds, meta.sample_rate);
        // The segments played backwards, as the blocks they are decoded in
        // (`reverse_blocks`), beside the segment list the way the rate chains are.
        let mut reversed: Vec<Option<Vec<Segment>>> = segs.iter().map(|_| None).collect();
        let segments: Vec<Segment> = segs
            .iter()
            .enumerate()
            // A gap, and a source with no track to decode, are the same stretch
            // of nothing: as many frames of silence as the window is long.
            .map(|(i, &(source, start_secs, end_secs))| {
                match source.and_then(|source| Some((source, tracks[source].as_ref()?))) {
                    Some((source, track)) => {
                        let mut seg = track.segment(source, start_secs, end_secs);
                        // A plain window at the mix rate reads exactly what it owes:
                        // its end is set from the running count, not the source's own
                        // rounding, so the cuts never drift against the picture. A
                        // reversed one at real time is as plain as a forward one: it
                        // reads the same window, last to first.
                        let backwards = speeds.get(i).and_then(Option::as_ref).is_some_and(|s| s.reverse);
                        let plain = speeds.get(i).is_none_or(|s| s.as_ref().is_none_or(crate::project::Stretch::is_straight));
                        if let (true, true, Some(owed)) = (
                            plain,
                            track.sample_rate() == meta.sample_rate,
                            owed_by_segment[i],
                        ) {
                            seg.media_end = seg.media_target + owed;
                        }
                        if backwards {
                            reversed[i] = Some(reverse_blocks(track, source, start_secs, &seg));
                        }
                        seg
                    }
                    None => Segment::silence(owed_by_segment[i].unwrap_or(
                        ((end_secs - start_secs).max(0.0) * f64::from(meta.sample_rate)) as u64,
                    )),
                }
            })
            .collect();
        // Chunk numbering is continuous across every join, counted from the first
        // segment's audible start in its own source (see `open_segments`).
        let timeline = segments.first().map_or(0, |s| match s.source {
            Some(source) => {
                let track = tracks[source].as_ref().expect("opened above");
                // ...in *timeline* frames, which is not the source's own count
                // when it was written at another rate. Integer, so the equal-rate
                // case -- every project before this -- is the number it always was.
                let at = s.media_target.saturating_sub(track.priming());
                at.saturating_mul(u64::from(meta.sample_rate))
                    / u64::from(track.sample_rate().max(1))
            }
            None => 0,
        });

        // One AAC packet decodes to 1024 frames; at stereo f32 that is 8 KB, so
        // this bound is ~0.75 s of lookahead — enough to ride out decode jitter
        // without making a pause take a second to bite.
        // One filter per segment, built here where the rate and the layout are
        // known and off the thread that will run it. An identity curve costs a
        // branch and nothing else ([`EqState::process`]), and a segment with no
        // curve at all is not even that.
        let eqs: Vec<Option<EqState>> = segments
            .iter()
            .enumerate()
            .map(|(i, _)| {
                eqs.get(i)
                    .and_then(Option::as_ref)
                    .map(|p| EqState::new(p, meta.sample_rate, meta.channels))
            })
            .collect();

        // One rate chain per segment that needs one, built where the sample
        // rate is known: what the timeline owes it is seconds there and frames
        // here.
        //
        // Two things ask for one, and they are NOT the same thing: a source
        // written at a rate other than the timeline's is a *rate conversion*
        // -- reading the samples at the ratio of the two rates is what keeps
        // a 48 kHz file's pitch on a 44.1 kHz timeline, and is the resampler's
        // whole duty now. A clip whose own [`Speed`] is not real time is a
        // *time* change: the resample hands the stretch a pitch-true stream
        // and the stretch hands the timeline the clip's compressed or
        // stretched seconds with every period of the signal kept (the tape
        // effect this used to be -- one resample at the speed -- moved the
        // pitch with the rate). A segment with neither has no chain at all
        // and reaches the mixer with the decoder's own samples, byte for
        // byte, as it always did.
        let speeds: Vec<Option<Rate>> = segments
            .iter()
            .zip(segs)
            .enumerate()
            .map(|(i, (seg, _))| {
                let rate = seg
                    .source
                    .and_then(|source| tracks[source].as_ref())
                    .map_or(meta.sample_rate, Track::sample_rate);
                let ratio = f64::from(rate) / f64::from(meta.sample_rate.max(1));
                // A direction alone is no change of time: it needs no stretch.
                let stretch = speeds
                    .get(i)
                    .and_then(Option::as_ref)
                    .filter(|s| !s.is_straight());
                if stretch.is_none() && rate == meta.sample_rate {
                    return None;
                }
                // A pitch shift is a stretch and a resample together (see [`Stretch::pitch`]):
                // the resample reads `2^(pitch/12)` times as fast, so the pitch moves by that,
                // and the stretch takes back the same factor, so the length does not change.
                let pitch = stretch.map_or(1.0, crate::project::Stretch::pitch_ratio);
                let channels = usize::from(meta.channels);
                // A speeded clip states the timeline it owes; a plain one owes
                // the window it was placed for, and an open-ended window owes
                // whatever its source has ([`Resample::UNBOUNDED`]).
                let owed = match owed_by_segment[i] {
                    Some(frames) => frames.min(Resample::UNBOUNDED - 1),
                    None => Resample::UNBOUNDED,
                };
                Some(match stretch {
                    // A rate conversion alone: the resampler owns the
                    // segment's promise, as it always has.
                    None => Rate {
                        resample: Resample::new(ratio, owed),
                        stretch: None,
                    },
                    // A speeded clip: the resample is only the rate
                    // conversion -- unbounded, because the ratio is all it
                    // is asked for -- and the stretch lands the timeline's
                    // frames exactly.
                    Some(s) => Rate {
                        resample: Resample::new(ratio * pitch, Resample::UNBOUNDED),
                        stretch: Some(TimeStretch::new(
                            s.step / pitch,
                            owed,
                            channels,
                            // Samples per timeline frame: what turns the ramp's
                            // frames into the stretch's own samples.
                            s.ramp
                                .map(|r| (r.scaled(1.0 / pitch), f64::from(meta.sample_rate) / s.fps)),
                        )),
                    },
                })
            })
            .collect();

        // One [`crate::project::Fade`] per segment, exactly the eq and speed
        // lists' rule: a short `fades` (the empty one every plain caller
        // passes) means the rest play flat, and it is cloned rather than
        // built here because [`Project::audio_fades_from`] already resolved
        // each clip's own frame count against where this segment lands --
        // in timeline frames, though, which is `fps`-relative and not yet
        // the rate `Fade::apply` will count samples at; `.scaled` re-counts
        // it in frames of `meta.sample_rate` now that this is known.
        let fade_ratio = f64::from(meta.sample_rate) / fps;
        let fades: Vec<Option<crate::project::Fade>> = segments
            .iter()
            .enumerate()
            .map(|(i, _)| {
                fades
                    .get(i)
                    .cloned()
                    .flatten()
                    .map(|f| f.scaled(fade_ratio))
            })
            .collect();

        let (tx, rx) = sync_channel(32);
        thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                run(Worker {
                    tracks,
                    channels: meta.channels as usize,
                    segments,
                    eqs,
                    speeds,
                    fades,
                    reversed,
                    timeline,
                    tx,
                })
            })?;
        Ok(Some((meta, rx)))
    }

    /// Every audio lane of a timeline, summed into one stream: `lanes` is one
    /// play list per lane, each in the shape
    /// [`open_multi_streams`](Self::open_multi_streams) takes, and what comes
    /// out is what the device hears -- the lanes decoded side by side and added
    /// sample for sample, clamped to `[-1, 1]` so a loud pair cannot wrap the
    /// output.
    ///
    /// **One lane is not mixed at all**: it is `open_multi_streams` verbatim, so
    /// a timeline with a single audio lane decodes exactly the samples it always
    /// did, down to the bit and down to the thread count.
    ///
    /// Lengths need not agree: a lane that has run out contributes silence to
    /// what is left, which is what a shorter lane means. Each lane keeps its own
    /// gap rule -- a hole in it is real silence from its own worker, not a
    /// skipped chunk -- so the sum feeds the master clock a sample per sample of
    /// timeline however the lanes are arranged. `start_sample` numbers from the
    /// first lane's own start (see [`AudioChunk`]).
    pub fn open_mixed_streams(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_mixed_streams_eq(sources, lanes, &[])
    }

    /// [`open_mixed_streams`](Self::open_mixed_streams) with
    /// [`open_multi_streams_eq`](Self::open_multi_streams_eq)'s per-segment
    /// equalizers, one list per lane -- [`crate::Project::audio_eqs_from`] is
    /// what shapes them.
    ///
    /// Filtering is each lane's worker's own business and therefore happens
    /// **before** the sum: a clip's curve reaches that clip's samples and stops
    /// there, where filtering the mix would smear every lane's curve over
    /// everything playing at the same instant. A missing or short list plays
    /// flat, as it does one lane down.
    pub fn open_mixed_streams_eq(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_mixed_streams_speed(sources, lanes, eqs, &[])
    }

    /// [`open_mixed_streams_eq`](Self::open_mixed_streams_eq) with
    /// [`open_multi_streams_speed`](Self::open_multi_streams_speed)'s per-segment
    /// rates, one list per lane -- [`crate::Project::audio_speeds_from`] is what
    /// shapes them, off the same walk the segments come from.
    ///
    /// A resample is each lane's worker's own business and happens **before**
    /// the sum, exactly as the filtering does: every lane still hands the mixer
    /// as many samples as the timeline says, so the lanes stay sample-aligned by
    /// construction and a mix is still an add.
    pub fn open_mixed_streams_speed(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_mixed_streams_master(sources, lanes, eqs, speeds, &[], Limiter::default())
    }

    /// [`open_mixed_streams_speed`](Self::open_mixed_streams_speed) with the two
    /// settings that belong to the *mix* rather than to a clip: what each lane
    /// is turned up or down by on its way into the sum (`gains[i]`, an
    /// amplitude, [`crate::Project::audio_gains`] builds it) and the master
    /// limiter the sum comes out through.
    ///
    /// Both live at the sum and nowhere else, which is what makes a lane volume
    /// a *lane* volume and a limiter a master one: a gain applied per segment
    /// would have to agree with itself across a lane's cuts, and a limiter
    /// applied per lane would pump each lane against the others. A short or
    /// empty gain list means the rest play as they are, exactly as a short eq
    /// list means flat.
    ///
    /// Flat settings take the same path the mixer always took, down to the bit:
    /// one lane at unity with the limiter off is still `open_multi_streams`
    /// verbatim, with no mixer thread at all. A lane that is *not* at unity, or
    /// a limiter that is on, is what puts a single-lane timeline through the
    /// mixer -- a sum of one, which is where both settings live.
    pub fn open_mixed_streams_master(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
        gains: &[f32],
        limiter: Limiter,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_mixed_streams_master_at(sources, lanes, eqs, speeds, gains, limiter, None)
    }

    /// [`open_mixed_streams_master`] with the project's own rate
    /// ([`crate::edith::Document::sample_rate`]) overriding the derived one, the
    /// same rate every lane's meta agrees on ([`open_mixed_streams_live`]'s
    /// invariant).
    pub fn open_mixed_streams_master_at(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
        gains: &[f32],
        limiter: Limiter,
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        // No fades on this path -- see `open_multi_streams_speed_at`'s same
        // placeholder.
        Self::open_mixed_streams_master_at_fade(
            sources,
            lanes,
            eqs,
            speeds,
            &[],
            1.0,
            gains,
            &[],
            &[],
            limiter,
            sample_rate,
        )
    }

    /// [`open_mixed_streams_master_at`](Self::open_mixed_streams_master_at)
    /// with a fade envelope per segment; see
    /// [`open_mixed_streams_live_fade`](Self::open_mixed_streams_live_fade).
    /// `fps` is [`open_multi_streams_speed_at_fade`](Self::open_multi_streams_speed_at_fade)'s.
    #[allow(
        clippy::too_many_arguments,
        reason = "public mix openers form one family, each fixing one more argument of the next; a parameter struct would only rename the list at every step"
    )]
    pub fn open_mixed_streams_master_at_fade(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
        fades: &[Vec<Option<crate::project::Fade>>],
        fps: f64,
        gains: &[f32],
        ducks: &[Vec<Duck>],
        voices: &[VoiceFx],
        limiter: Limiter,
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        Self::open_mixed_streams_live_fade(
            sources,
            lanes,
            eqs,
            speeds,
            fades,
            fps,
            gains,
            ducks,
            voices,
            limiter,
            None,
            sample_rate,
        )
    }

    /// [`open_mixed_streams_master`](Self::open_mixed_streams_master) with the
    /// mix left *reachable*: `live` is the handle the mixer picks its gains and
    /// its limiter off between blocks, so a fader moved while the stream plays
    /// is heard without the stream being rebuilt ([`MixControls`]).
    ///
    /// `gains`/`limiter` still say what it starts as -- they are what decides
    /// whether there is a mixer at all -- and the handle is marked live only
    /// when there is one. An export passes `None`: nothing can move under it.
    #[allow(
        clippy::too_many_arguments,
        reason = "public mix openers form one family, each fixing one more argument of the next; a parameter struct would only rename the list at every step"
    )]
    pub fn open_mixed_streams_live(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
        gains: &[f32],
        limiter: Limiter,
        live: Option<&Arc<MixControls>>,
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        // No fades on this path -- see `open_multi_streams_speed_at`'s same
        // placeholder.
        Self::open_mixed_streams_live_fade(
            sources,
            lanes,
            eqs,
            speeds,
            &[],
            1.0,
            gains,
            &[],
            &[],
            limiter,
            live,
            sample_rate,
        )
    }

    /// [`open_mixed_streams_live`](Self::open_mixed_streams_live) with a fade
    /// envelope per segment as well: `fades[i]` is what segment `i` plays
    /// through -- [`crate::Project::audio_fades_from`] is what builds it, off
    /// the same walk the segments come from. A short or missing list means
    /// the rest play flat, the eq list's rule. `fps` is the timeline rate
    /// those fades were built at ([`crate::project::Fade::scaled`]).
    #[allow(
        clippy::too_many_arguments,
        reason = "public mix openers form one family, each fixing one more argument of the next; a parameter struct would only rename the list at every step"
    )]
    pub fn open_mixed_streams_live_fade(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
        eqs: &[Vec<Option<EqParams>>],
        speeds: &[Vec<Option<crate::project::Stretch>>],
        fades: &[Vec<Option<crate::project::Fade>>],
        fps: f64,
        gains: &[f32],
        ducks: &[Vec<Duck>],
        voices: &[VoiceFx],
        limiter: Limiter,
        live: Option<&Arc<MixControls>>,
        sample_rate: Option<u32>,
    ) -> crate::Result<Option<(AudioMeta, Receiver<AudioChunk>)>> {
        let [first, ..] = lanes else {
            return Ok(None);
        };
        if !Self::is_mixed(lanes.len(), gains, limiter, voices) {
            let flat = Vec::new();
            let plain = Vec::new();
            let flat_fade = Vec::new();
            return Self::open_multi_streams_speed_at_fade(
                sources,
                first,
                eqs.first().unwrap_or(&flat),
                speeds.first().unwrap_or(&plain),
                fades.first().unwrap_or(&flat_fade),
                fps,
                sample_rate,
            );
        }
        // Settled once for the whole mix, as the width is: every lane is opened
        // at the same rate, so no lane's own sources pick a rate of their own.
        let (width, mix_rate) = Self::mix_format(sources, lanes)?;
        let sample_rate = sample_rate.or(mix_rate);
        let mut meta = None;
        let mut rxs = Vec::with_capacity(lanes.len());
        for (i, segs) in lanes.iter().enumerate() {
            // Every lane takes its meta from the same source, so the metas
            // agree by construction; `None` from any of them is a silent
            // timeline.
            let flat = Vec::new();
            let flat_fade = Vec::new();
            let plain = Vec::new();
            let Some((lane_meta, rx)) = Self::open_lane(
                sources,
                segs,
                eqs.get(i).unwrap_or(&flat),
                speeds.get(i).unwrap_or(&plain),
                fades.get(i).unwrap_or(&flat_fade),
                fps,
                sample_rate,
                width,
            )?
            else {
                return Ok(None);
            };
            // A lane with voice processors on runs them on its own stream, before
            // anything is summed ([`crate::voice`]); one without is left as it was.
            let rx = match voices.get(i).filter(|v| v.is_active()) {
                Some(fx) => crate::voice::spawn_stage(rx, fx, lane_meta.sample_rate, usize::from(lane_meta.channels)),
                None => rx,
            };
            meta = Some(lane_meta);
            rxs.push(rx);
        }
        let meta = meta.expect("at least one lane opened above");
        let (tx, rx) = sync_channel(32);
        let channels = usize::from(meta.channels);
        // One per lane, in the lanes' own order, padded with unity: a short
        // list is "as it is from here on", the eq list's rule.
        let mut gains = gains.to_vec();
        gains.resize(lanes.len(), 1.0);
        // Built here, off the thread that will run it, where the rate is known.
        let state = limiter
            .is_active()
            .then(|| LimiterState::new(&limiter, meta.sample_rate, channels));
        // One list per lane, padded with no duck: a short list is "unducked from here on".
        let mut ducks = ducks.to_vec();
        ducks.resize(lanes.len(), Vec::new());
        // Marked before the spawn rather than inside it: a caller asks this the
        // moment the open returns, and a flag set on the far thread would still
        // read dead.
        let live = live.map(|live| {
            live.live.store(true, Ordering::Release);
            live.clone()
        });
        let rate = meta.sample_rate;
        thread::Builder::new()
            .name("audio-mix".into())
            .spawn(move || {
                mix(
                    &rxs,
                    channels,
                    MixState { gains, ducks, limiter: state },
                    rate,
                    live.as_deref(),
                    &tx,
                )
            })?;
        Ok(Some((meta, rx)))
    }

    /// The width and the rate every lane of a mix runs at, from the sources any
    /// lane names. The width is the widest of them (a mono source counts as
    /// mono, so a mono-only mix stays mono). Each lane used to derive its own
    /// from its own sources, which put a mono voice lane beside a stereo bed at
    /// one channel while the sum was two: the voice was then read as half as
    /// many frames, and ended at half its length. The rate is [`mix_rate`] over
    /// the same sources: a lane of its own would otherwise mix a 22.05 kHz
    /// voiceover at its own rate beside a 48 kHz bed. Each part is `None` when
    /// no lane names a source with an audio track.
    fn mix_format(
        sources: &[(PathBuf, usize)],
        lanes: &[Vec<(Option<usize>, f64, f64)>],
    ) -> crate::Result<(Option<u16>, Option<u32>)> {
        let mut width: Option<u16> = None;
        let mut rates = Vec::new();
        for &(source, ..) in lanes.iter().flatten() {
            let Some(source) = source else {
                continue;
            };
            let (path, stream) = sources
                .get(source)
                .ok_or_else(|| format!("segment names source {source} of {}", sources.len()))?;
            if crate::is_image(path) {
                continue;
            }
            if let Some(track) = Track::open(path, *stream)? {
                width = Some(width.map_or(track.channels(), |w| w.max(track.channels())));
                rates.push(track.sample_rate());
            }
        }
        Ok((width, mix_rate(rates)))
    }

    /// Whether these settings put the timeline through the **mixer**: more than
    /// one lane to sum, a lane off unity, or a limiter over the sum. Anything
    /// else is the single-lane flat path, which is `open_multi_streams`
    /// verbatim and has no mixer thread to talk to.
    ///
    /// Public to the crate because [`crate::PlaybackSession`] has to know the
    /// answer *before* the open now that it happens on another thread, and the
    /// answer must not be spelled out twice: what it decides is whether a fader
    /// is handed to a running mixer or rebuilds the stream.
    pub(crate) fn is_mixed(lanes: usize, gains: &[f32], limiter: Limiter, voices: &[VoiceFx]) -> bool {
        lanes > 1 || limiter.is_active() || gains.iter().any(|&g| g != 1.0) || voices.iter().any(VoiceFx::is_active)
    }

    /// Why [`open`](Self::open) came back silent for a file that *has* sound:
    /// `Some(reason)` when there is an audio track in a codec this does not
    /// decode (an Opus web rip, say). `None` when the file is simply silent
    /// -- nothing to tell the user there. Header only, and only worth calling
    /// once the open has already returned no audio.
    pub fn unsupported(path: impl AsRef<Path>) -> crate::Result<Option<String>> {
        // An Ogg Speex stream is named, not decoded: the reason is what the import warns with.
        if crate::demux::is_ogg_speex(path.as_ref()) {
            return Ok(Some(crate::demux::SPEEX_REFUSAL.to_string()));
        }
        // Matroska: what symphonia reads out of one ([`Track::open`]) plus the
        // AC-3 and E-AC-3 whose blocks are handed to the Dolby decoder
        // ([`MkvAc3Track`]) and the Opus its own decoder takes; everything else
        // -- DTS, and whatever else no decoder here has -- is named rather than
        // left playing silent with no reason given. A Matroska file with no
        // audio track at all returns `None` here like any other silent file.
        if crate::demux::is_matroska(path.as_ref()) {
            let Some(codec) = crate::demux::matroska_audio_codec(path.as_ref())? else {
                return Ok(None);
            };
            let name = codec_display_name(&codec);
            // An AC-3 track that came up silent all the same is one that could
            // not be read -- a laced block, an unreadable syncframe -- and the
            // reader's own words are the ones worth showing.
            // A TrueHD track its decoder opens is sound, not an unsupported one.
            if matches!(MkvTruehdTrack::open(path.as_ref()), Ok(Some(_))) {
                return Ok(None);
            }
            // ...and so is a DTS track `ec-dts` opens; one it refuses (XLL) says why.
            if (crate::extaudio::is_mkv_dts(&codec) || crate::extaudio::is_mkv_mpeg_layer(&codec))
                && let Some(first) = crate::demux::matroska_audio_tracks(path.as_ref())?.first()
            {
                match crate::extaudio::ExtFileTrack::open_native(path.as_ref(), first.number) {
                    Ok(Some(_)) => return Ok(None),
                    Err(e) => {
                        return Ok(Some(format!("the {name} track of this Matroska file cannot be decoded: {e}")));
                    }
                    Ok(None) => {}
                }
            }
            return Ok(Some(match MkvAc3Track::open(path.as_ref()) {
                Ok(Some(_)) => return Ok(None),
                // ...and the same question asked of the other reader, because a
                // refusal is a claim: a track symphonia's side really opens and
                // builds a decoder for is not one to tell the user about. This
                // is what stopped naming `A_OPUS` the moment Opus decoded.
                Ok(None) | Err(_)
                    if SymTrack::open(path.as_ref())
                        .ok()
                        .flatten()
                        .is_some_and(|t| t.decoder().is_ok()) =>
                {
                    return Ok(None);
                }
                Ok(None) => format!(
                    "the {name} track of this Matroska file cannot be decoded — {MKV_DECODED} are"
                ),
                Err(e) => format!("the {name} track of this Matroska file cannot be decoded: {e}"),
            }));
        }
        // A transport stream: the first sound track, if the build cannot decode it.
        if crate::demux::is_mpeg_system(path.as_ref()) {
            let file = crate::demux::ts::describe(path.as_ref())?;
            let flavor = crate::demux::ts::Flavor::of(path.as_ref());
            return Ok(file.audio.first().and_then(|a| match (&a.refusal, a.stream) {
                (Some(why), _) => Some(crate::demux::ts::refusal_text_for(why, false, flavor)),
                (None, None) => Some(crate::demux::ts::refusal_text_for(
                    &format!("the {} audio track cannot be decoded", a.codec),
                    false,
                    flavor,
                )),
                _ => None,
            }));
        }
        // File order, out of `moov.traks`: naming a codec off a `HashMap` walk
        // would tell the user about a different track from one run to the next
        // on a film with two.
        let reader = mp4track::open(path.as_ref())?;
        // Flagged only when the decoders `open` itself would try cannot take the
        // first track: AC-3 first, then the AAC reader, as `open_track` orders
        // them. A track either one opens is playable, so not a refusal.
        if matches!(Ac3Track::open(path.as_ref(), 0), Ok(Some(_)))
            || matches!(Track::open(path.as_ref(), 0), Ok(Some(_)))
        {
            return Ok(None);
        }
        Ok(mp4track::audio_tracks(&reader).first().map(|&(_, stream)| {
            match stream.and_then(|s| mp4track::codec(&reader, s)) {
                Some(codec) => {
                    format!("the {} audio track cannot be decoded — {MP4_DECODED} are", codec.name())
                }
                None => format!("the audio track is in a codec we cannot decode — {MP4_DECODED} are"),
            }
        }))
    }

    /// The audio parameters of `path`'s `stream`, for checking an import
    /// against the timeline's first source: header only, no decoder and no
    /// worker. `Ok(None)` means no audio track — a silent file, which import
    /// pairs only with other silent files. A *named* stream that is not there
    /// or does not decode is an `Err`, as everywhere else.
    pub fn probe(path: impl AsRef<Path>, stream: usize) -> crate::Result<Option<AudioProbe>> {
        let Some(track) = Track::open(path.as_ref(), stream)? else {
            return Ok(None);
        };
        Ok(Some(AudioProbe {
            sample_rate: track.sample_rate(),
            channels: track.channels(),
        }))
    }

    /// Every audio stream of `path`, in file order: an entry's `index` is the
    /// stream number [`open_multi_streams`](Self::open_multi_streams) takes.
    /// Header only, no decoder and no worker — and no filtering either, so a
    /// stream this engine cannot decode is listed with `decodable: false`
    /// instead of vanishing.
    ///
    /// An empty list means the file has no audio at all, which is a valid
    /// silent source.
    ///
    /// A standalone audio file ([`crate::is_audio`]) has exactly one stream by
    /// construction — one track, no picture beside it — so it is described from
    /// symphonia's own header rather than walked as an mp4, which an mp3 is not
    /// and an ALAC `.m4a` only half is.
    pub fn probe_streams(path: impl AsRef<Path>) -> crate::Result<Vec<StreamInfo>> {
        let path = path.as_ref();
        crate::demux::refuse_speex(path)?;
        // A Matroska file lists one row per `TrackEntry` of audio -- a
        // dual-audio remux has two, and the second one used to be invisible --
        // numbered by [`mkv_stream_order`]: the track that already plays first,
        // then the rest in file order. A file whose header will not walk at all
        // is an `Err` here, as it is in [`Track::open`]: a listing that quietly
        // came back empty for a broken file is how a lane ends up silent with
        // nothing to go on. No audio track at all is an empty list, which is
        // the silent source it says it is.
        if crate::demux::is_matroska(path) {
            let tracks = crate::demux::matroska_audio_tracks(path)?;
            let mut streams = Vec::new();
            for (index, entry) in mkv_stream_order(path, &tracks)
                .into_iter()
                .map(|at| &tracks[at])
                .enumerate()
            {
                // The routing [`Track::open`] does, one row at a time, so a row
                // describes the track that would really open: AC-3 and E-AC-3
                // through the Dolby decoder's own reader, everything else
                // through symphonia. The AC-3 row describes what the decoder
                // *emits*, which is the stereo the §7.8 downmix hands the
                // timeline, not the 5.1 the file carries -- exactly as the mp4
                // AC-3 rows do.
                let ac3 = matches!(entry.codec.as_str(), "A_AC3" | "A_EAC3")
                    .then(|| MkvAc3Track::open_track(path, entry.number).ok().flatten())
                    .flatten();
                // A track symphonia cannot even describe is listed undecodable
                // by the name the container gives it, rather than taking the
                // whole listing down with it: the other language still plays.
                // TrueHD and MLP through their own decoder, the same way.
                let truehd = matches!(entry.codec.as_str(), "A_TRUEHD" | "A_MLP")
                    .then(|| MkvTruehdTrack::open_track(path, entry.number).ok().flatten())
                    .flatten();
                let ext = (crate::extaudio::is_mkv_dts(&entry.codec) || crate::extaudio::is_mkv_mpeg_layer(&entry.codec))
                    .then(|| crate::extaudio::ExtFileTrack::open_native(path, entry.number).ok().flatten())
                    .flatten();
                let sym = match (&ac3, &truehd, &ext) {
                    (None, None, None) => SymTrack::open_track(path, entry.number).ok().flatten(),
                    _ => None,
                };
                // What the row describes: the Dolby decoders' own output where one
                // took the track, symphonia's where it did not.
                let (channels, sample_rate) = match (&ac3, &truehd) {
                    (Some(t), _) => (t.channels, t.sample_rate),
                    (_, Some(t)) => (t.channels, t.sample_rate),
                    _ if ext.is_some() => ext.as_ref().map_or((0, 0), |t| (t.channels(), t.sample_rate)),
                    _ => sym
                        .as_ref()
                        .map_or((0, 0), |t| (t.channels, t.sample_rate)),
                };
                streams.push(StreamInfo {
                    index,
                    bit_depth: None,
                    bitrate: None,
                    codec: mkv_codec_name(&entry.codec),
                    channels,
                    sample_rate,
                    lang: (entry.language != "und" && !entry.language.is_empty())
                        .then(|| entry.language.clone()),
                    // No width gate any more: `channels` is what reaches the
                    // timeline, which [`downmix`] makes a pair out of whatever
                    // the file carries. What greys a row out is a codec nothing
                    // here decodes, and nothing else.
                    decodable: ac3.is_some()
                        || truehd.is_some()
                        || ext.is_some()
                        || sym.is_some_and(|t| t.decoder().is_ok()),
                });
            }
            return Ok(streams);
        }
        // A transport stream lists every sound track of its program in PMT order, the ones
        // this build has no decoder for (LATM and the like) greyed out by name.
        if crate::demux::is_mpeg_system(path) {
            let file = crate::demux::ts::describe(path)?;
            return Ok(file
                .audio
                .iter()
                .enumerate()
                .map(|(index, a)| {
                    let track = a
                        .stream
                        .and_then(|_| crate::demux::ts::TsAudio::open(path, index).ok().flatten());
                    StreamInfo {
                        index,
                        bit_depth: crate::demux::ts::pcm_depth(a.codec_id),
                        bitrate: None,
                        codec: a.codec.clone(),
                        channels: track.as_ref().map_or(a.channels, |t| t.channels()),
                        sample_rate: track.as_ref().map_or(a.sample_rate, |t| t.sample_rate),
                        lang: a.language.clone().filter(|l| l != "und"),
                        decodable: track.is_some_and(|t| t.decoder().is_ok()),
                    }
                })
                .collect());
        }
        if crate::is_audio(path) {
            if let Some(track) = crate::extaudio::ExtFileTrack::open(path, 0)? {
                return Ok(vec![StreamInfo {
                    index: 0,
                    codec: track.codec_name().into(),
                    channels: track.channels(),
                    sample_rate: track.sample_rate,
                    lang: None,
                    decodable: track.decoder().is_ok(),
                    bit_depth: None,
                    bitrate: None,
                }]);
            }
            let track = SymTrack::open_required(path)?;
            return Ok(vec![StreamInfo {
                index: 0,
                codec: track.codec.into(),
                channels: track.channels,
                sample_rate: track.sample_rate,
                // Neither the mp3 nor the wav headers carry an ISO-639 tag the
                // way an mdhd does, and a lone track needs no telling apart.
                lang: None,
                decodable: track.decoder().is_ok(),
                bit_depth: None,
                bitrate: None,
            }]);
        }
        let reader = mp4track::open(path)?;
        // The playing time the header states, which is what a track's bytes are
        // divided by for its rate. A header that states none leaves the rate out.
        let secs = reader.duration_secs();
        Ok(mp4track::audio_tracks(&reader)
            .into_iter()
            .enumerate()
            .map(|(index, (track_id, stream))| {
                let codec = stream.and_then(|s| mp4track::codec(&reader, s));
                let bitrate = stream
                    .zip(secs)
                    .and_then(|(s, secs)| {
                        crate::demux::bits_per_second(mp4track::sample_bytes(&reader, s), secs)
                    });
                let aac = codec == Some(CodecId::Aac);
                // An AC-3 track is described through its own reader: what is
                // listed is the *decoded* shape -- stereo, because that is what
                // the §7.8 downmix hands the timeline (see [`Ac3Track`]).
                let ac3 = (!aac)
                    .then(|| Ac3Track::open(path, index).ok().flatten())
                    .flatten();
                // DTS, ADPCM, A-law / mu-law and AMR go through `ec-probe`'s reader.
                let ext = (!aac && ac3.is_none() && codec.is_some_and(crate::extaudio::is_ext_codec))
                    .then(|| crate::extaudio::ExtFileTrack::open(path, index).ok().flatten())
                    .flatten();
                let lang = mp4track::language(&reader, track_id).filter(|l| l != "und");
                let profile = stream
                    .and_then(|s| mp4track::extradata(&reader, s))
                    .and_then(|asc| asc_fields(&asc).ok())
                    .map(|(profile, _, _)| profile);
                StreamInfo {
                    index,
                    bit_depth: None,
                    bitrate,
                    codec: match (aac, &ac3) {
                        (true, _) => "aac",
                        (_, Some(_)) => "ac-3",
                        _ => match codec.filter(|c| crate::extaudio::is_ext_codec(*c)) {
                            Some(c) => c.name(),
                            None => "unknown",
                        },
                    }
                    .into(),
                    channels: match (&ac3, &ext) {
                        (Some(t), _) => t.channels,
                        (_, Some(t)) => t.channels(),
                        _ => stream.map_or(0, |s| mp4track::channels(&reader, s) as u16),
                    },
                    sample_rate: match (&ac3, &ext) {
                        (Some(t), _) => t.sample_rate,
                        (_, Some(t)) => t.sample_rate,
                        _ => stream.map_or(0, |s| mp4track::sample_rate(&reader, s)),
                    },
                    lang,
                    // The profile still gates -- the writer rebuilds an
                    // AudioSpecificConfig and calls it LC -- but the layout no
                    // longer does: a 5.1 or 7.1 AAC track decodes through
                    // `ec-aac` and folds to the pair the timeline carries
                    // ([`aac_decoder`]).
                    decodable: ac3.is_some()
                        || ext.is_some()
                        || aac && matches!(profile, Some(AAC_LC | AAC_SBR | AAC_PS)),
                }
            })
            .collect())
    }

    /// How long `path`'s audio plays, in seconds. `Ok(None)` for a silent file.
    ///
    /// A standalone audio file is a source with no frame count of its own, so
    /// this is what its length on the timeline is derived from. Taken from the
    /// header where there is one; a stream that does not say — a bare mp3 with
    /// no Xing header is the usual one — is *decoded* to find out, which is
    /// linear in its length (~0.5 ms per source second) and happens once, at
    /// import, rather than per repaint.
    ///
    /// Stream 0: this answers for a standalone audio file, whose only stream
    /// that is. A video source's length is its frame count, never this.
    pub fn duration_secs(path: impl AsRef<Path>) -> crate::Result<Option<f64>> {
        Self::stream_duration_secs(path, 0)
    }

    /// [`duration_secs`](Self::duration_secs) for audio stream `stream` of the file: its own
    /// playing time, which can be longer than the picture a video file carries.
    pub fn stream_duration_secs(path: impl AsRef<Path>, stream: usize) -> crate::Result<Option<f64>> {
        let path = path.as_ref();
        let timed = Self::decoded_secs(path, stream);
        if let Ok(Some(secs)) = timed {
            return Ok(Some(secs));
        }
        // A track nothing here opens or decodes cannot be timed by reading it, but the
        // file's own header states how long it plays: that length is what it is held to,
        // so its sound comes in silent rather than refused for want of a duration.
        match crate::demux::header_audio_secs(path, stream) {
            Some(secs) => Ok(Some(secs)),
            None => timed,
        }
    }

    /// [`stream_duration_secs`](Self::stream_duration_secs) by reading the track: its sample
    /// count, or a decode of it when the count is not stated.
    fn decoded_secs(path: &Path, stream: usize) -> crate::Result<Option<f64>> {
        let Some(track) = Track::open(path, stream)? else {
            return Ok(None);
        };
        let rate = f64::from(track.sample_rate().max(1));
        if let Some(total) = track.total_samples() {
            return Ok(Some(total as f64 / rate));
        }
        drop(track);
        let Some((meta, rx)) = Self::open(path)? else {
            return Ok(None);
        };
        let channels = usize::from(meta.channels.max(1));
        let last = rx.into_iter().fold(0, |_, chunk| {
            chunk.start_sample + (chunk.samples.len() / channels) as u64
        });
        Ok(Some(last as f64 / f64::from(meta.sample_rate.max(1))))
    }

    /// The raw AAC packets covering `segs` — the same half-open source-second
    /// windows [`open_segments`](Self::open_segments) decodes — copied out
    /// byte for byte. Nothing is decoded and nothing is re-encoded: the bytes go
    /// straight into an mp4 writer, which is how an export keeps its sound exact
    /// wherever the edit lets it (a lane carrying an equalizer cannot be copied
    /// and is re-encoded instead -- `export::encode_audio`).
    ///
    /// Cut points do not land on 1024-sample boundaries, so each segment is
    /// rounded to whole packets against an error carried across the joins (see
    /// [`packet_run`]): the copy stays within half a packet (~12 ms) of the
    /// asked-for length however many cuts there are, instead of drifting out of
    /// lip sync one rounding at a time.
    ///
    /// The run starts one packet before the first audible one, which a reader
    /// drops as encoder priming — for a segment list starting at 0.0 that is the
    /// source's own priming packet, so the copy is the whole track from sample 1.
    ///
    /// `Ok(None)` for a file with no AAC track, and for an empty segment list.
    ///
    /// corner-cut: the first packet after an *interior* join decodes without its
    /// MDCT overlap predecessor, so up to 23 ms there can alias. Upgrade path is
    /// re-encoding just the join packets, which needs an encoder we do not have.
    pub fn copy_segments(
        path: impl AsRef<Path>,
        segs: &[(f64, f64)],
    ) -> crate::Result<Option<(AacTrackParams, Vec<AacPacket>)>> {
        let segs: Vec<_> = segs.iter().map(|&(s, e)| (Some(0), s, e)).collect();
        Self::copy_multi_segments(&[path.as_ref().to_path_buf()], &segs)
    }

    /// [`copy_segments`](Self::copy_segments) over several files, segments naming
    /// their source by index into `sources`. The rounding debt is carried across
    /// a source join exactly as across a cut inside one file, so the bound stays
    /// half a packet over the whole list however many files it spans, and the
    /// extra priming packet is still the very first segment's alone.
    ///
    /// Every source that *has* a track must declare the same
    /// [`AacTrackParams`]: the copy becomes one output track with one esds.
    /// Import refuses a mismatch up front; the check here is the backstop.
    ///
    /// A segment naming **no** source is a gap, and is copied as that many
    /// packets of [`silent_packet`] silence — the rounding debt carried through
    /// it like any other, so the hole occupies its exact duration and the audio
    /// after it stays in sync with the picture. A gap the track *opens* on gets
    /// one silent packet more, which is the priming a reader drops. A segment
    /// naming a *silent* source is written the same way: a clip whose file has
    /// no audio track is a hole with a picture over it, and the timeline it
    /// joined already said so ([`crate::PlaybackSession::import`]).
    pub fn copy_multi_segments(
        sources: &[PathBuf],
        segs: &[(Option<usize>, f64, f64)],
    ) -> crate::Result<Option<(AacTrackParams, Vec<AacPacket>)>> {
        let sources: Vec<_> = sources.iter().map(|p| (p.clone(), 0)).collect();
        Self::copy_multi_streams(&sources, segs)
    }

    /// [`copy_multi_segments`](Self::copy_multi_segments) with each source
    /// naming *which* of its audio streams to copy — the same `(path, stream)`
    /// list [`open_multi_streams`](Self::open_multi_streams) plays. The two
    /// take it in the same shape on purpose: an export that copied a different
    /// stream from the one the timeline played would be a file that sounds
    /// nothing like what was edited, and nothing would say so.
    pub fn copy_multi_streams(
        sources: &[(PathBuf, usize)],
        segs: &[(Option<usize>, f64, f64)],
    ) -> crate::Result<Option<(AacTrackParams, Vec<AacPacket>)>> {
        // The output track is declared from the first source that has one --
        // not simply the first one named, which may be a silent clip. Such a
        // clip defines no rate and no layout; it is written as silence below
        // and the track goes on being the audible sources' track.
        let mut tracks: Vec<Option<Option<AacTrack>>> = sources.iter().map(|_| None).collect();
        let mut first = None;
        for &(source, ..) in segs {
            let Some(source) = source else { continue };
            if let Some(track) = source_at(&mut tracks, sources, source)? {
                first = Some(track.track_params());
                break;
            }
        }
        let Some(params) = first else {
            return Ok(None); // no segments, or nothing but silence
        };

        let sample_rate = params.sample_rate;
        let mut err = 0i64;
        let mut packets: Vec<AacPacket> = Vec::new();
        for (at, &(source, start_secs, end_secs)) in segs.iter().enumerate() {
            // The last segment is the only one no later join pays a rounding
            // debt in, so its run rounds up ([`packet_run_last`]).
            let last = at + 1 == segs.len();
            // A gap, and a clip whose source carries no track, are the same hole
            // to write: silence for as long as the window is.
            let named = match source {
                Some(source) => source_at(&mut tracks, sources, source)?.map(|t| (source, t)),
                None => None,
            };
            let Some((source, track)) = named else {
                let ideal = ((end_secs - start_secs).max(0.0) * f64::from(sample_rate)) as i64;
                let bytes = silent_packet(params.chan_conf)?;
                // A reader drops the first packet of an AAC track as the
                // encoder's priming. When the track opens on a hole, that has to
                // come out of one *extra* packet of silence: dropped out of the
                // hole itself it would shorten it, and everything after the hole
                // would play a packet early. It is not part of the run, so it
                // owes the rounding debt nothing.
                let priming = u32::from(packets.is_empty());
                let run = match last {
                    true => packet_run_last(&mut err, ideal, u32::MAX),
                    false => packet_run(&mut err, ideal, u32::MAX),
                };
                for _ in 0..run + priming {
                    packets.push(AacPacket {
                        bytes: bytes.clone(),
                        samples: SAMPLES_PER_PACKET,
                    });
                }
                continue;
            };
            if track.track_params() != params {
                return Err(format!(
                    "source {source} audio is {:?}, the timeline's is {params:?}",
                    track.track_params()
                )
                .into());
            }
            let target_ts = unscale(track.media(start_secs), track.sample_rate, track.timescale);
            let (start_id, _) = packet_at(track.stts.iter().copied(), target_ts, 0);
            let ideal = ((end_secs - start_secs).max(0.0) * f64::from(track.sample_rate)) as i64;
            let available = track.sample_count.saturating_sub(start_id - 1);
            // The head packet is the priming of the first segment only: a reader
            // drops exactly one, so an interior join must not add another --
            // and neither must a segment behind a leading gap, which already
            // paid the priming in silence.
            let head = (packets.is_empty() && start_id > 1).then(|| start_id - 1);
            let run = match last {
                true => packet_run_last(&mut err, ideal, available),
                false => packet_run(&mut err, ideal, available),
            };
            let ids = head
                .into_iter()
                .chain(start_id..start_id + run);
            for id in ids {
                let Some(sample) = mp4track::sample(&mut track.reader, track.stream, id)? else {
                    let count = track.sample_count;
                    return Err(format!("audio sample {id} of {count} is missing").into());
                };
                packets.push(AacPacket {
                    bytes: sample.data.to_vec(),
                    samples: SAMPLES_PER_PACKET,
                });
            }
        }
        Ok(Some((params, packets)))
    }
}

/// One AAC-LC access unit of silence, for the gaps in an exported timeline —
/// this is the *copy* path, which never encodes anything, so the only silence it
/// can write is one spelled out by hand. (The re-encode path next door has a
/// real encoder and needs none of this: its gaps are zero samples.)
///
/// It is the shortest legal `raw_data_block` there is: one element carrying
/// `max_sfb = 0`, which is "no scalefactor bands", which is a frame with no
/// spectral data at all and therefore 1024 samples of exact zero. Every field
/// is zero except the element id and the terminating `ID_END`, so the whole
/// thing is 4 bytes mono and 7 stereo. Verified against our own decoder in the
/// unit tests, which is the only claim worth making about hand-written
/// bitstream.
///
/// corner-cut: mono and stereo only, because `chan_conf` beyond 2 needs more
/// elements per block; the timeline refuses such a source at import today, so
/// this `Err` is a backstop. Upgrade path is emitting `chan_conf` elements.
fn silent_packet(chan_conf: u8) -> crate::Result<Vec<u8>> {
    match chan_conf {
        // ID_SCE(000) tag(0000) + 22 zero bits + ID_END(111), byte-aligned.
        1 => Ok(vec![0x00, 0x00, 0x00, 0x07]),
        // ID_CPE(001) tag(0000) common_window(0) + 2 x 22 zero bits + ID_END.
        2 => Ok(vec![0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0E]),
        n => Err(format!("cannot write silence for a {n}-channel AAC track").into()),
    }
}

/// The source at `index`, on the stream it names, opened on first use -- and
/// `None` for a source that has no track at all, which the caller writes as
/// silence exactly as it writes a gap. The outer `Option` is "has it been
/// opened", the inner one "was there anything in it", so a silent source is
/// opened once like any other. Sources a segment list never names are never
/// touched: a project's list only grows.
fn source_at<'a>(
    tracks: &'a mut [Option<Option<AacTrack>>],
    sources: &[(PathBuf, usize)],
    index: usize,
) -> crate::Result<Option<&'a mut AacTrack>> {
    let slot = tracks
        .get_mut(index)
        .ok_or_else(|| format!("segment names source {index} of {}", sources.len()))?;
    if slot.is_none() {
        let (path, stream) = &sources[index];
        *slot = Some(copy_track(path, *stream)?);
    }
    Ok(slot.as_mut().expect("just opened").as_mut())
}

/// A source's track for the *copy* path, which needs raw AAC access units.
///
/// An mp3, a wav, a flac — anything this cannot copy — is an `Err` here, by name
/// and by format. That is no longer the end of the road: `export::copy_audio`
/// takes it as the answer "not copyable" and decodes and re-encodes the timeline
/// instead, so these words are a fallback's reason rather than a user's refusal.
/// They are still worded for a person, because a caller with no encoder to fall
/// back on shows them verbatim.
///
/// `Ok(None)` for a file with no audio at all, which is a silent export.
fn copy_track(path: &Path, stream: usize) -> crate::Result<Option<AacTrack>> {
    match Track::open(path, stream)? {
        None => Ok(None),
        // An HE-AAC track has packets of 2048 frames and a config the copy
        // cannot rebuild, so it is re-encoded like every other uncopyable source.
        Some(Track::Aac(track)) if track.sbr => Err(format!(
            "a packet copy needs AAC-LC in an mp4; {} is HE-AAC",
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy(),
        )
        .into()),
        Some(Track::Aac(track)) => Ok(Some(track)),
        // A Matroska source gets its own words: its packets *are* AAC, so
        // "export needs AAC audio today" would read as a contradiction. What
        // they are not is packets of an mp4 sample table, which is what the
        // copy walks. The export decodes such a source and encodes it again
        // rather than showing this, so a film's sound reaches an mp4 either way;
        // the words stay for a caller that copies and cannot re-encode.
        //
        // corner-cut: the ceiling is the copy path being written against the mp4
        // reader, not anything about the bytes. Upgrade path is a
        // `copy_segments` that walks a Matroska track, which would make this a
        // copy again instead of a re-encode.
        Some(Track::Sym(_)) if crate::demux::is_matroska(path) => Err(format!(
            "{}'s sound is AAC inside a Matroska file, which no packet copy reaches: \
             a copy walks an mp4's own sample table. It is decoded and encoded again \
             instead",
            path.file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy(),
        )
        .into()),
        Some(Track::Sym(track)) => Err(uncopyable(path, track.codec)),
        // Decoded, never copied: an AC-3 syncframe is not something an `mp4a`
        // sample table can hold. The export decodes it and encodes AAC, exactly
        // as it does for every other source no copy reaches.
        Some(Track::Ac3(_) | Track::Mkv(_)) => Err(uncopyable(path, "AC-3")),
        // The same reason again: a TrueHD access unit is not an `mp4a` sample.
        Some(Track::Truehd(_)) => Err(uncopyable(path, "TrueHD")),
        Some(Track::Ext(_)) => Err(uncopyable(path, "DTS, ADPCM, A-law, mu-law or AMR")),
        // A transport stream's packets are not an mp4's sample table either.
        Some(Track::Ts(_)) => Err(uncopyable(path, "an MPEG system stream's sound")),
    }
}

/// The one wording for "this source's audio cannot be *copied* into an mp4",
/// shared by every format that cannot be. What the export does about it is
/// decode and re-encode; this is the reason it had to.
fn uncopyable(path: &Path, codec: &str) -> crate::Error {
    format!(
        "a packet copy needs AAC in an mp4; {} is {codec}",
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
    )
    .into()
}

/// One source's audio, however it is stored: the mp4 AAC track the export can
/// copy, or a standalone file read through symphonia's probe. The opener and
/// the worker speak to this, never to either reader directly.
enum Track {
    Aac(AacTrack),
    Ac3(Ac3Track),
    /// The same AC-3 decoder, fed out of a Matroska file's blocks instead of an
    /// mp4's sample table.
    Mkv(MkvAc3Track),
    /// Dolby TrueHD or MLP, out of a Matroska file's blocks, decoded by
    /// `ec-truehd`.
    Truehd(MkvTruehdTrack),
    /// DTS, ADPCM, A-law / mu-law and AMR out of a Matroska, mp4 / QuickTime /
    /// 3GP, WAVE or raw file, read by `ec-probe` ([`crate::extaudio`]).
    Ext(Box<crate::extaudio::ExtFileTrack>),
    Sym(SymTrack),
    /// One sound track of an MPEG transport stream, its own demuxer under it
    /// ([`crate::demux::ts`]): AAC in ADTS, AC-3, E-AC-3, MP3 and HDMV LPCM.
    Ts(Box<crate::demux::ts::TsAudio>),
}

impl Track {
    /// `Ok(None)` for a file with no audio, which is a valid silent source.
    ///
    /// The mp4 reader goes first and its verdict is final for anything that is
    /// not [`crate::is_audio`]: a video file's audio track is exactly what it
    /// always was, silent mp4s included (an mp4 whose only `soun` track is one
    /// we cannot decode has always been a silent source, not an error). Only a
    /// standalone audio file falls through to symphonia — which is also how an
    /// ALAC `.m4a` is reached, since it parses as an mp4 with no AAC track.
    ///
    /// `stream` counts audio tracks in file order, as
    /// [`AudioSession::probe_streams`] hands them out. A standalone audio file
    /// has exactly one, so any `stream` above 0 there is a promise the file
    /// cannot keep — an `Err`, the same as naming a stream an mp4 does not have.
    fn open(path: &Path, stream: usize) -> crate::Result<Option<Self>> {
        crate::demux::refuse_speex(path)?;
        crate::demux::refuse_asf(path)?;
        // A Matroska file goes to symphonia, whose `mkv` reader is the one thing
        // here that reads its packets -- neither mp4 reader can open the file at
        // all -- except for the two codecs symphonia has no decoder for at any
        // version and this project does: AC-3 and E-AC-3 are handed to the Dolby
        // decoder straight out of the blocks ([`MkvAc3Track`]). Sound neither
        // one can decode (Opus, DTS) leaves the source the silent one it always
        // was, which `AudioSession::unsupported` puts into words, rather than
        // failing the import of a file whose picture is fine. A track that will
        // not open at all is *not* in that group: see below.
        if crate::demux::is_matroska(path) {
            // The numbering `probe_streams` lists and a project saves, from the
            // one function that decides it ([`mkv_stream_order`]): the stream
            // index picks a `TrackEntry`, that entry's *track number* is what
            // both readers are then pointed at. Positions are never handed to a
            // reader -- symphonia's own order is its business -- because a
            // mapping that drifts plays the wrong language and saves it into
            // the project.
            let tracks = crate::demux::matroska_audio_tracks(path)?;
            if tracks.is_empty() {
                return Ok(None);
            }
            let order = mkv_stream_order(path, &tracks);
            let Some(entry) = order.get(stream).map(|at| &tracks[*at]) else {
                return Err(format!(
                    "{}: audio stream {stream} of {} stream{}",
                    path.display(),
                    tracks.len(),
                    if tracks.len() == 1 { "" } else { "s" }
                )
                .into());
            };
            // TrueHD and MLP go to their own decoder out of the same blocks. An
            // open failure is returned, not passed on: symphonia cannot read the
            // track either, and the silence it would give is the bug
            // [`MkvTruehdTrack`] exists to avoid -- an Atmos track's refusal is
            // the answer the user needs.
            if matches!(entry.codec.as_str(), "A_TRUEHD" | "A_MLP")
                && let Some(track) = MkvTruehdTrack::open_track(path, entry.number)?
            {
                return Ok(Some(Self::Truehd(track)));
            }
            match MkvAc3Track::open_track(path, entry.number) {
                Ok(Some(track)) => return Ok(Some(Self::Mkv(track))),
                Ok(None) => {}
                // Named on the way past rather than returned: symphonia gets the
                // same file next and may read the track perfectly well (a laced
                // block is [`MkvAc3Track`]'s limitation, not the file's), and
                // if it cannot either, *its* `Err` is the one that comes back.
                Err(e) => debug_once(format!("matroska audio: {}: {e}", path.display())),
            }
            // Two things are silent here and no more: a file with no audio
            // track at all, and a track whose codec nothing in this project
            // decodes (Opus, DTS -- [`AudioSession::unsupported`] is what puts
            // that into words). A track that will not *open* is an `Err`, the
            // same answer the mp4 arm gives: turned into silence it would play
            // and export as a hole with only an eprintln to say why, which is a
            // bug wearing a feature's clothes.
            // DTS (and its DTS-HD core) through `ec-dts`, out of the same blocks. An
            // open failure is returned: an XLL-only lossless track is refused by name.
            if (crate::extaudio::is_mkv_dts(&entry.codec) || crate::extaudio::is_mkv_mpeg_layer(&entry.codec))
                && let Some(track) = crate::extaudio::ExtFileTrack::open_native(path, entry.number)?
            {
                return Ok(Some(Self::Ext(Box::new(track))));
            }
            // Speex has no reader in this build (symphonia cannot open it): silent, and
            // `AudioSession::unsupported` names it for the warning.
            if codec_display_name(&entry.codec) == "Speex" {
                return Ok(None);
            }
            let Some(track) = SymTrack::open_track(path, entry.number)? else {
                return Ok(None);
            };
            if let Err(e) = track.decoder() {
                debug_once(format!("matroska audio: {}: {e}", path.display()));
                return Ok(None);
            }
            return Ok(Some(Self::Sym(track)));
        }
        // A transport stream is read by its own demuxer, whatever the sound in it is.
        if crate::demux::is_mpeg_system(path) {
            return Ok(crate::demux::ts::TsAudio::open(path, stream)?.map(|t| Self::Ts(Box::new(t))));
        }
        let audio_file = crate::is_audio(path);
        // Ahead of the AAC reader, because that one answers "not AAC" for a
        // *named* stream with an `Err` and an AC-3 track is not an error any
        // more. `Ok(None)` here means "not an AC-3 track", so every other file
        // falls through to exactly the reader it always used.
        if !audio_file && let Some(track) = Ac3Track::open(path, stream)? {
            return Ok(Some(Self::Ac3(track)));
        }
        match AacTrack::open(path, stream) {
            Ok(Some(track)) => return Ok(Some(Self::Aac(track))),
            Ok(None) if !audio_file => {
                // Not AAC: DTS, ADPCM, A-law / mu-law or AMR in an mp4 / QuickTime / 3GP file.
                return Ok(crate::extaudio::ExtFileTrack::open(path, stream)?.map(|t| Self::Ext(Box::new(t))));
            }
            Err(e) if !audio_file => {
                return match crate::extaudio::ExtFileTrack::open(path, stream) {
                    Ok(Some(t)) => Ok(Some(Self::Ext(Box::new(t)))),
                    _ => Err(e),
                };
            }
            _ => {}
        }
        // A WAVE file that is not plain PCM (ADPCM, A-law, mu-law, DTS), a raw `.dts`
        // or `.amr`: `ec-probe`'s reader, ahead of symphonia's.
        if stream == 0
            && let Some(track) = crate::extaudio::ExtFileTrack::open(path, 0)?
        {
            return Ok(Some(Self::Ext(Box::new(track))));
        }
        if stream > 0 {
            return Err(format!("{}: audio stream {stream} of 1 stream", path.display()).into());
        }
        SymTrack::open_required(path).map(|track| Some(Self::Sym(track)))
    }

    fn sample_rate(&self) -> u32 {
        match self {
            Self::Aac(t) => t.sample_rate,
            Self::Ac3(t) => t.sample_rate,
            Self::Mkv(t) => t.sample_rate,
            Self::Truehd(t) => t.sample_rate,
            Self::Ext(t) => t.sample_rate,
            Self::Sym(t) => t.sample_rate,
            Self::Ts(t) => t.sample_rate,
        }
    }

    /// What this track hands the timeline: mono, or stereo for everything else.
    /// Nothing is refused for its width any more -- a layout wider than a pair
    /// is folded to one at the decoder's door ([`downmix`]), because one output
    /// device is all there is, not because the file is unreadable.
    fn channels(&self) -> u16 {
        match self {
            Self::Aac(t) => t.channels(),
            // Downmixed to a pair by the AC-3 decoder itself (§7.8.2); a mono
            // track passes through as the one channel it is (see [`ac3_decoder`]),
            // so what `channels` measures is already the timeline width.
            Self::Ac3(t) => t.channels,
            // The same decoder, out of a Matroska file.
            Self::Mkv(t) => t.channels,
            // Folded to a pair by [`downmix`] in the worker; see [`MkvTruehdTrack`].
            Self::Truehd(t) => t.channels,
            Self::Ext(t) => t.channels(),
            Self::Sym(t) => t.channels,
            Self::Ts(t) => t.channels(),
        }
    }

    /// Frames per channel of *audible* audio, priming already subtracted.
    fn total_samples(&self) -> Option<u64> {
        match self {
            Self::Aac(t) => t.total_samples,
            Self::Ac3(t) => t.total_samples,
            Self::Mkv(t) => t.total_samples,
            Self::Truehd(t) => t.total_samples,
            Self::Ext(t) => t.total_samples,
            Self::Sym(t) => t.total_samples,
            Self::Ts(t) => t.total_samples,
        }
    }

    /// Encoder delay of this file. Sources are joined, priming is not shared.
    fn priming(&self) -> u64 {
        match self {
            Self::Aac(t) => t.priming,
            Self::Ac3(t) => t.priming,
            // Matroska writes no edit list and AC-3 has no encoder delay:
            // block 0 is sample 0.
            Self::Mkv(_) | Self::Truehd(_) | Self::Ts(_) => 0,
            Self::Ext(t) => t.priming,
            Self::Sym(t) => t.priming,
        }
    }

    /// One `[start, end)` window of this source's seconds, resolved to whatever
    /// its own reader needs to find it again.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        match self {
            Self::Aac(t) => t.segment(source, start_secs, end_secs),
            Self::Ac3(t) => t.segment(source, start_secs, end_secs),
            Self::Mkv(t) => t.segment(source, start_secs, end_secs),
            Self::Truehd(t) => t.segment(source, start_secs, end_secs),
            Self::Ext(t) => {
                let rate = f64::from(t.sample_rate);
                let media_target = (start_secs.max(0.0) * rate).round() as u64 + t.priming;
                Segment {
                    source: Some(source),
                    start_id: 0,
                    start_pos: 0,
                    media_target,
                    media_end: (((end_secs.max(0.0) * rate).round() as u64).saturating_add(t.priming)).max(media_target),
                }
            }
            Self::Ts(t) => {
                let rate = f64::from(t.sample_rate);
                let media_target = (start_secs.max(0.0) * rate).round() as u64;
                Segment {
                    source: Some(source),
                    start_id: 0,
                    start_pos: 0,
                    media_target,
                    media_end: ((end_secs.max(0.0) * rate).round() as u64).max(media_target),
                }
            }
            Self::Sym(t) => {
                let media_target = t.media(start_secs);
                Segment {
                    source: Some(source),
                    start_id: 0,
                    start_pos: 0,
                    media_target,
                    media_end: t.media(end_secs).max(media_target),
                }
            }
        }
    }

    /// Builds and throws away a decoder, so an undecodable stream is an `Err`
    /// from the opener rather than a silently empty channel.
    fn check_decoder(&self) -> crate::Result<()> {
        match self {
            Self::Aac(t) => {
                aac_decoder(&t.params, t.source_channels())?;
            }
            Self::Ac3(t) => {
                ac3_decoder(t.requested)?;
            }
            Self::Mkv(t) => {
                ac3_decoder(t.requested)?;
            }
            // Its first access unit was decoded by the opener already.
            Self::Truehd(_) => {}
            Self::Ext(t) => {
                t.decoder()?;
            }
            Self::Ts(t) => {
                t.decoder()?;
            }
            Self::Sym(t) => {
                t.decoder()?;
            }
        }
        Ok(())
    }
}

/// A standalone audio file — mp3, wav, flac, vorbis in ogg, ALAC, ADTS — read
/// through symphonia's own probe and codec registry. No packet table and no
/// sample ids: the reader is *seeked* per segment and the packets it then hands
/// out carry their own timestamps.
struct SymTrack {
    reader: Box<dyn FormatReader>,
    /// The file the reader reads, kept because a seek builds a *new* one
    /// ([`SymTrack::seek_to`]).
    path: PathBuf,
    track_id: u32,
    sample_rate: u32,
    /// What reaches the timeline: the file's own layout, or 2 for anything
    /// wider, which is folded down by [`SymDecoder`] exactly as the AC-3
    /// reader's decoder folds its own.
    channels: u16,
    /// What the decoder emits, which is what the fold reads.
    source_channels: u16,
    /// Frames per channel of audible audio, `None` when the header does not say.
    total_samples: Option<u64>,
    /// Frames withheld at the very end of the track and never played: the
    /// `DiscardPadding` of Vorbis in Matroska, whose blocks carry no granule to
    /// cut by. Zero for everything else (Ogg Vorbis is cut by its granules, per
    /// packet).
    tail_hold: u64,
    /// Every packet of a Matroska Vorbis track with its exact start in samples
    /// ([`VorbisStarts`]). Matroska stamps a block in whole milliseconds, up to
    /// half a millisecond (22 samples at 44.1 kHz) off the samples it holds, so
    /// a track with this index is read by it -- packet by packet, and sought by
    /// sample -- rather than through symphonia's stamp-based reader. `None` for
    /// everything else, which keeps its packets' own timestamps or granules.
    vorbis_starts: Option<VorbisStartsRef>,
    /// The file the indexed packets are read from.
    vorbis_file: Option<File>,
    /// The index of the next packet [`SymTrack::next_packet`] reads.
    vorbis_next: usize,
    /// Encoder delay, and **always zero here**: symphonia 0.6 drops the leading
    /// frames itself, so a reader that skipped `Track::delay` on top would skip
    /// them twice. Kept as a field because [`Track::priming`] asks every track
    /// for one and the AAC and AC-3 readers beside it do owe a real number.
    ///
    /// This used to carry `delay` and cost the double skip on the two formats
    /// that declare one: a `.mp3` lost its first 1105 samples (25 ms) and read
    /// 25 ms short, an `.ogg` its first 128. Measured 2026-08-11 against
    /// `test_tone.flac` -- lossless, and the only one of the three whose
    /// container declares no delay at all -- which the mp3 and the ogg of the
    /// same tone align with sample for sample once this is not subtracted.
    priming: u64,
    time_base: TimeBase,
    params: AudioCodecParameters,
    /// The format's short name, for the export refusal. `&'static` because it
    /// comes out of the codec registry, which outlives everything here.
    codec: &'static str,
}

impl SymTrack {
    /// `Ok(None)` for a container that opened and holds no audio track at all --
    /// a silent source, which is a thing a file is allowed to be. Everything
    /// else that goes wrong here is an `Err`: a file that will not open, will
    /// not parse, or carries a track it cannot describe is *broken*, and the
    /// difference matters because silence is played and exported without a word
    /// while an `Err` is shown ([`Track::open`]).
    fn open(path: &Path) -> crate::Result<Option<Self>> {
        Self::open_inner(path, None)
    }

    /// [`open`](Self::open) pointed at one **named** track: a Matroska
    /// `TrackNumber`, which symphonia keeps as the track's id
    /// (`symphonia-format-mkv` `demuxer.rs:301`). That is what makes the second
    /// language of a dual-audio file reachable, and it is matched by number
    /// rather than by position so this reader and
    /// [`crate::demux::matroska_audio_tracks`] cannot disagree about which
    /// track a stream index meant. `Ok(None)` for a number the reader does not
    /// carry.
    fn open_track(path: &Path, number: u64) -> crate::Result<Option<Self>> {
        Self::open_inner(path, Some(number))
    }

    fn open_inner(path: &Path, number: Option<u64>) -> crate::Result<Option<Self>> {
        let reader = sym_reader(path)?;
        let track = match number {
            Some(number) => reader.tracks().iter().find(|t| u64::from(t.id) == number),
            None => reader.default_track(SymKind::Audio),
        };
        let Some(track) = track else {
            return Ok(None);
        };
        // `track.delay` is deliberately not read: see the `priming` field.
        let (track_id, num_frames) = (track.id, track.num_frames);
        let time_base = track
            .time_base
            .ok_or_else(|| format!("{} declares no time base", path.display()))?;
        let Some(symphonia_core::codecs::CodecParameters::Audio(params)) =
            track.codec_params.clone()
        else {
            return Err(format!("{} declares no audio codec", path.display()).into());
        };
        // An AAC track's rate is the one `ec-aac` decodes at, which for HE-AAC
        // is the SBR extension rate (2x the core the container may state). An
        // ADTS stream carries no config to read that from, so its header's rate
        // is only the core's until the first frames are decoded (see below).
        let mut sample_rate = match (params.codec, params.extra_data.as_deref()) {
            (CODEC_ID_AAC, Some(asc)) => aac_output_rate(asc)?,
            _ => params.sample_rate,
        }
        .ok_or_else(|| format!("{} declares no sample rate", path.display()))?;
        let mut channels = params.channels.as_ref().map(|c| c.count() as u16);
        // A plain-LC config, or an ADTS stream with none, may still be HE-AAC by
        // implicit signalling: its rate only doubles once a frame has carried an
        // SBR payload, so the first frames are decoded to read the real rate.
        if params.codec == CODEC_ID_AAC
            && aac_may_be_implicit(params.extra_data.as_deref())?
            && let Some(primed) = sym_aac_prime(path, track_id, params.extra_data.as_deref())?
        {
            sample_rate = primed.sample_rate;
            channels = Some(primed.channels);
        }
        let channels = channels
            .ok_or_else(|| format!("{} declares no channel layout", path.display()))?;
        // Vorbis in Matroska states no length on the track and its blocks carry
        // no granule: the last block's padding is what its muxer says to drop
        // from the end ([`crate::demux::matroska_tail_padding_ns`]).
        let tail_hold = match params.codec {
            CODEC_ID_VORBIS => crate::demux::matroska_tail_padding_ns(path, u64::from(track_id))
                .map_or(0, |ns| (ns as f64 * f64::from(sample_rate) / 1e9).round() as u64),
            _ => 0,
        };
        let vorbis_starts = match params.codec {
            CODEC_ID_VORBIS if crate::demux::is_matroska(path) => {
                matroska_vorbis_starts(path, u64::from(track_id), &params)
            }
            _ => None,
        };
        let vorbis_file = match vorbis_starts {
            Some(_) => Some(File::open(path)?),
            None => None,
        };
        // AAC is decoded by `ec-aac`, which symphonia's registry does not list.
        let codec = match params.codec {
            CODEC_ID_AAC => "aac",
            _ => symphonia::default::get_codecs()
                .get_audio_decoder(params.codec)
                .map_or("an unsupported format", |d| d.codec.info.short_name),
        };
        // The one codec here that *does* owe a priming number: Opus states its
        // encoder delay in the stream itself (`OpusHead`'s pre-skip, 312 samples
        // on a film remux) and neither reader drops it, because symphonia trims
        // for its own decoders and the Opus one is this crate's. Measured against
        // ffmpeg's decode of the same file: 312 samples late in *both* an mkv and
        // an ogg until this is subtracted, sample-exact after (correlation
        // 1.00000). It is priming exactly as an AAC track's is, and it is read
        // off the stream rather than the container for that reason.
        let priming = match params.codec {
            CODEC_ID_OPUS => opus_pre_skip(params.extra_data.as_deref().unwrap_or_default()),
            _ => 0,
        };
        Ok(Some(Self {
            reader,
            path: path.to_path_buf(),
            track_id,
            sample_rate,
            // Anything wider than a pair arrives as a stereo source, whatever it
            // was stored as: 5.1, 7.1 and everything between are folded by
            // [`downmix`] at the decoder's door ([`SymDecoder`]).
            channels: channels.min(2),
            source_channels: channels,
            total_samples: num_frames.map(|n| n.saturating_sub(priming)),
            tail_hold,
            vorbis_starts,
            vorbis_file,
            vorbis_next: 0,
            priming,
            time_base,
            params,
            codec,
        }))
    }

    /// [`open`](Self::open) where having no audio track is itself the error:
    /// what a standalone audio file is held to, since a song with no sound in
    /// it is a broken file and not a silent source.
    fn open_required(path: &Path) -> crate::Result<Self> {
        Self::open(path)?.ok_or_else(|| format!("{} has no audio track", path.display()).into())
    }

    /// A fresh decoder: seeking mid-stream leaves state that belongs to the
    /// packets we skipped, exactly as on the AAC path.
    ///
    /// Which decoder is not the caller's business, and it is not always
    /// symphonia's: it has no AAC decoder in this build at all (its HE-AAC
    /// refusal, "aac: aac too complex", is why), so every AAC track goes to
    /// `ec-aac` instead ([`aac_decoder`]), and it has no Opus decoder either, so
    /// that one goes to `ec-opus` ([`SymDecoder::Opus`]).
    fn decoder(&self) -> crate::Result<SymDecoder> {
        let from = usize::from(self.source_channels);
        if self.params.codec == CODEC_ID_OPUS {
            let (streams, coupled, mapping) =
                opus_layout(self.params.extra_data.as_deref().unwrap_or_default(), from)?;
            // 48 kHz whatever the container says: that is the only rate Opus
            // decodes at, and the rate both readers declare for it.
            let decoder =
                ec_opus::MultistreamDecoder::with_rate(48_000, streams, coupled, &mapping);
            return Ok(SymDecoder::Opus(Box::new(decoder), from));
        }
        if self.params.codec == CODEC_ID_AAC {
            return aac_decoder(&self.params, from);
        }
        if self.params.codec == CODEC_ID_VORBIS {
            return vorbis_decoder(&self.params);
        }
        Ok(SymDecoder::Sym(
            symphonia::default::get_codecs()
                .make_audio_decoder(&self.params, &AudioDecoderOptions::default())?,
            from,
        ))
    }

    /// `secs` on this source's audible timeline into media samples (priming
    /// included, so it compares directly against a decoded position).
    fn media(&self, secs: f64) -> u64 {
        ((secs * f64::from(self.sample_rate)).round() as u64).saturating_add(self.priming)
    }

    /// Where the packet stamped `ts` begins, exactly, for a track with an index
    /// ([`VorbisStarts`]). `None` where there is none, or `ts` is not one of its
    /// packets' stamps.
    fn exact_start(&self, ts: units::Timestamp) -> Option<u64> {
        let starts = self.vorbis_starts.as_deref()?;
        Some(starts.packets[starts.at(ts.get())?].start)
    }

    /// The next packet of the track: from the index's file where the track has
    /// one, stamped with its own stamp, and from the reader otherwise.
    fn next_packet(&mut self) -> Result<Option<Packet>, String> {
        let (Some(starts), Some(file)) = (self.vorbis_starts.as_deref(), self.vorbis_file.as_mut())
        else {
            return self.reader.next_packet().map_err(|e| e.to_string());
        };
        let Some(block) = starts.packets.get(self.vorbis_next) else {
            return Ok(None);
        };
        self.vorbis_next += 1;
        crate::demux::check_extent(file, block.at, block.len).map_err(|e| e.to_string())?;
        let mut data = vec![0u8; block.len];
        file.seek(SeekFrom::Start(block.at))
            .and_then(|_| file.read_exact(&mut data))
            .map_err(|e| format!("reading a Vorbis block: {e}"))?;
        let stamp = units::Timestamp::new(block.stamp);
        Ok(Some(Packet::new(self.track_id, stamp, units::Duration::ZERO, data)))
    }

    /// A packet timestamp in the track's time base, as a media position: the
    /// scale [`media`](Self::media) hands out, so a landing compares directly
    /// against a segment's target.
    ///
    /// The priming comes *off* it because a Matroska Opus track's blocks are
    /// timestamped for the audible stream while the decoder hands its samples
    /// back with the pre-skip still in front of them. Measured against ffmpeg's
    /// decode of the same second of a real 5.1 Opus remux: without this term a seeked
    /// Opus track came out 6.5 ms -- one pre-skip, 312 samples -- ahead of it,
    /// with it, on the same sample.
    fn samples_at(&self, ts: symphonia_core::units::Timestamp) -> u64 {
        let secs = self.time_base.calc_time_saturating(ts).as_secs_f64();
        // Rounded, not truncated: a position is a whole sample, and the seconds
        // it was converted through sit a sample low when truncated (11840 came
        // back 11839), which the seek test's exact check caught.
        ((secs.max(0.0) * f64::from(self.sample_rate)).round() as u64).saturating_sub(self.priming)
    }

    /// Puts the reader **at or before** `media_target`, [`SYM_PRE_ROLL`] ahead
    /// of it where the container allows, so that everything the caller did not
    /// ask for is decoder warm-up that [`emit`] drops.
    ///
    /// At or before is the whole point, and it is not what one seek gives. A
    /// Matroska seek lands on the first frame whose timestamp is at or past the
    /// one asked for, and its cue table can put that a whole cluster further on
    /// still: measured on a real 5.1 Opus remux, asking for 599.83 s (600 s less the
    /// pre-roll) landed at 601.487 s and asking for 609.83 s at 613.407 s.
    /// Samples that far *behind* the reader can never be emitted, and the film
    /// played from there while the timeline said 600 s -- 1.5 s of sound
    /// against picture, which is the seek desync this exists to kill. So the
    /// landing is read back, and an overshoot is asked again from far enough
    /// before it to swallow the overshoot.
    ///
    /// Each attempt gets a **new reader**, because symphonia's mkv seek is
    /// reliable exactly once per reader: it neither clears the frames it has
    /// already queued (they come out ahead of the new position, carrying the
    /// old block's timestamp) nor recovers its element iterator, and a second
    /// seek measured on the same file landed the reader back at the first
    /// cluster, 0.094 s, whatever it was asked for. Opening again costs 2 ms on
    /// a 5 GB film, which is what a seek is allowed to cost.
    fn seek_to(&mut self, media_target: u64) {
        // With an index the packet is found by its sample, not by a seek of the
        // container's: the decoder starts at the packet before the first one
        // whose start is at or before the target, since a fresh decoder answers
        // nothing for the packet it starts on and the next one is the first
        // that answers. Nothing is read here; [`SymTrack::next_packet`] reads it.
        if let Some(starts) = self.vorbis_starts.as_deref() {
            let at_or_before = starts
                .packets
                .partition_point(|p| p.start <= media_target);
            self.vorbis_next = at_or_before.saturating_sub(2);
            return;
        }
        let rate = f64::from(self.sample_rate.max(1));
        let mut back = SYM_PRE_ROLL;
        // Enough for any overshoot a cue table can hold: each attempt asks from
        // strictly further back than the last, and the seek to 0 is the floor.
        for _ in 0..8 {
            let from = media_target.saturating_sub(back);
            match sym_reader(&self.path) {
                Ok(reader) => self.reader = reader,
                // Whatever the file did, the reader in hand still reads: fall
                // back to seeking that one, exactly as before this loop existed.
                Err(e) => eprintln!("audio reopen failed: {e}"),
            }
            // Asking for the beginning is asking for nothing: a fresh reader is
            // already there, and *seeking* there is how the first 167 ms of a
            // real 5.1 Opus remux went missing -- the cue table's first point put the
            // reader past them and no window rule can reach back.
            if from == 0 {
                return;
            }
            let secs = from as f64 / rate;
            let Some(time) = Time::try_from_secs_f64(secs) else {
                return;
            };
            let to = SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            };
            // A failed seek is not fatal: it leaves the reader where it was, and
            // the window rules still only emit what the segment asked for.
            let landed = match self.reader.seek(SeekMode::Accurate, to) {
                Ok(landed) => self.samples_at(landed.actual_ts),
                Err(e) => {
                    eprintln!("audio seek to {secs:.3}s failed: {e}");
                    return;
                }
            };
            if landed <= media_target {
                return;
            }
            back += (landed - media_target) + SYM_PRE_ROLL;
        }
    }
}

/// One symphonia reader over `path`, which is both how a track is opened and
/// how it is seeked ([`SymTrack::seek_to`]).
///
/// A raw ADTS stream is read by [`crate::adts`] rather than probed: symphonia's
/// own ADTS reader lives behind the `aac` feature this project does not link,
/// and an ADTS sync word is the one thing it would have answered to anyway.
fn sym_reader(path: &Path) -> crate::Result<Box<dyn FormatReader>> {
    let mut head = [0u8; 2];
    if File::open(path)?.read(&mut head)? == 2 && ec_aac::is_adts(&head) {
        let mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
        return Ok(Box::new(crate::adts::AdtsReader::try_new(mss)?));
    }
    let mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    // The extension is a hint, not a decision: the probe reads the magic and
    // is free to disagree, which is what makes a mislabelled file work.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    Ok(symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?)
}

/// One packet in, interleaved f32 out — whichever library did the decoding, and
/// already folded to the layout [`SymTrack::channels`] promised. The number
/// beside each is the width the decoder itself emits, which is what
/// [`downmix`] folds from; 1 and 2 fold to nothing.
enum SymDecoder {
    Sym(Box<dyn AudioDecoder>, usize),
    /// Every AAC track, mono to 7.1 and LC or HE-AAC: `ec-aac` takes the config
    /// the track carries, or the first ADTS frame's when it carries none. Boxed
    /// because the decoder carries its own IMDCT and SBR tables and this enum is
    /// moved into the worker.
    Aac(Box<ec_aac::AacDecoder>, usize),
    /// Every Opus track, mono to 7.1: one multistream decoder -- the streams of
    /// a packet, the channel mapping table and the routing between them are all
    /// its business -- and the same width.
    Opus(Box<ec_opus::MultistreamDecoder>, usize),
    /// Every Vorbis track, Ogg or Matroska, mono to 7.1: `ec-vorbis` decodes
    /// the blocks, in the stream's channel order, and the second field is the
    /// width. The third maps each of those channels to the family's order
    /// ([`ec_vorbis::channel_map`]).
    Vorbis(Box<ec_vorbis::VorbisDecoder>, usize, Vec<usize>),
}

impl SymDecoder {
    fn decode(&mut self, packet: &Packet, out: &mut Vec<f32>) -> crate::Result<()> {
        match self {
            Self::Sym(decoder, from) => {
                decoder.decode(packet)?.copy_to_vec_interleaved::<f32>(out);
                downmix(out, *from);
            }
            Self::Aac(decoder, from) => {
                let pcm = decoder
                    .decode(&packet.data, None)
                    .map_err(|e| format!("AAC decode failed: {e}"))?;
                // `pcm.sample_rate` is what this block is at; the track was
                // opened at the same rate ([`aac_output_rate`]), so the timeline
                // and the samples agree. The frame count comes from the block
                // itself, not from a fixed packet size, because HE-AAC carries
                // twice the frames of its core.
                *out = pcm.samples;
                downmix(out, *from);
            }
            Self::Vorbis(decoder, from, to_ec) => {
                let blocks = decoder
                    .decode_audio(&packet.data)
                    .map_err(|e| format!("Vorbis decode failed: {e}"))?;
                let mut planes = vec![Vec::new(); blocks.len()];
                for (channel, plane) in blocks.into_iter().enumerate() {
                    if let Some(&slot) = to_ec.get(channel) {
                        planes[slot] = plane;
                    }
                }
                interleave_planes(&planes, out);
                vorbis_packet_window(out, planes.len(), packet);
                downmix(out, *from);
            }
            Self::Opus(decoder, from) => {
                *out = decoder
                    .decode_packet(&packet.data)
                    .map_err(|e| format!("Opus decode failed: {e}"))?;
                // The encoder delay is not dropped here: it is this track's
                // priming ([`SymTrack::open_inner`]), which the window rules
                // subtract once for the whole segment rather than per packet.
                vorbis_to_film_order(out, *from);
                downmix(out, *from);
            }
        }
        Ok(())
    }
}

/// The decoder for a Vorbis track: `ec-vorbis`, from the identification and
/// setup headers symphonia states in the track's extra data (the same layout
/// in Ogg and in Matroska).
fn vorbis_decoder(params: &AudioCodecParameters) -> crate::Result<SymDecoder> {
    let decoder = vorbis_headers(params)?;
    let channels = usize::from(decoder.identification().channels);
    let (_, to_ec) = ec_vorbis::channel_map(channels);
    Ok(SymDecoder::Vorbis(Box::new(decoder), channels, to_ec))
}

/// `ec-vorbis` built from a Vorbis track's headers, without a decoding state.
fn vorbis_headers(params: &AudioCodecParameters) -> crate::Result<ec_vorbis::VorbisDecoder> {
    let extra = params
        .extra_data
        .as_deref()
        .ok_or("a Vorbis track carries no headers")?;
    // symphonia states the identification header (30 bytes, fixed by Vorbis I)
    // and then the setup header; `ec-vorbis` wants all three, so the comment
    // header is written here as an empty one.
    let (ident, setup) = extra
        .split_at_checked(VORBIS_IDENT_LEN)
        .ok_or("a Vorbis track's identification header is short")?;
    ec_vorbis::VorbisDecoder::new(&[ident, VORBIS_EMPTY_COMMENTS, setup])
        .map_err(|e| format!("Vorbis headers: {e}").into())
}

/// The starts of a Matroska Vorbis track's packets, in samples from its first.
///
/// Packet `n` answers the quarter of block `n-1` plus the quarter of block `n`
/// samples (`ec-vorbis` `packet_blocksize`), and the first packet answers none,
/// so the count of every packet before it is its exact position. The timestamp
/// alone cannot give that, since it is rounded to milliseconds, so the blocks
/// are counted: one byte each (their mode), in the cached block list of
/// [`crate::demux::matroska_track_blocks`]. The walk costs one read per block, and
/// it is kept per file, so a seek pays it once.
type VorbisStartsRef = Arc<VorbisStarts>;

/// The packets of one Matroska Vorbis track, in storage order.
struct VorbisStarts {
    packets: Vec<VorbisPacket>,
}

struct VorbisPacket {
    /// The block's own timestamp in the file, in `TimestampScale` ticks.
    stamp: i64,
    /// Where the packet's samples begin, in the track's own count from its start.
    start: u64,
    /// Where the block's bytes are, and how many.
    at: u64,
    len: usize,
}

impl VorbisStarts {
    /// The index of the packet stamped `stamp`.
    fn at(&self, stamp: i64) -> Option<usize> {
        self.packets.binary_search_by_key(&stamp, |p| p.stamp).ok()
    }
}

type StartsKey = (PathBuf, u64, u64, Option<std::time::SystemTime>);

/// The most files whose packet index is kept; a seek reuses one of them.
const VORBIS_STARTS_KEPT: usize = 8;

static VORBIS_STARTS: Mutex<Vec<(StartsKey, VorbisStartsRef)>> = Mutex::new(Vec::new());

fn matroska_vorbis_starts(
    path: &Path,
    number: u64,
    params: &AudioCodecParameters,
) -> Option<VorbisStartsRef> {
    let meta = std::fs::metadata(path).ok()?;
    let key: StartsKey = (path.to_path_buf(), number, meta.len(), meta.modified().ok());
    if let Ok(kept) = VORBIS_STARTS.lock()
        && let Some((_, hit)) = kept.iter().find(|(k, _)| *k == key)
    {
        return Some(Arc::clone(hit));
    }
    let starts = Arc::new(vorbis_starts_walk(path, number, params)?);
    if let Ok(mut kept) = VORBIS_STARTS.lock() {
        if kept.len() >= VORBIS_STARTS_KEPT {
            kept.remove(0);
        }
        kept.push((key, Arc::clone(&starts)));
    }
    Some(starts)
}

fn vorbis_starts_walk(
    path: &Path,
    number: u64,
    params: &AudioCodecParameters,
) -> Option<VorbisStarts> {
    let blocks = crate::demux::matroska_track_blocks(path, number)?;
    // The search by stamp needs them in order, which Matroska requires of a track.
    if blocks.windows(2).any(|w| w[0].0 >= w[1].0) {
        return None;
    }
    let headers = vorbis_headers(params).ok()?;
    let mut file = File::open(path).ok()?;
    let mut packets = Vec::with_capacity(blocks.len());
    let (mut samples, mut previous) = (0u64, None::<usize>);
    for (stamp, at, len) in blocks {
        packets.push(VorbisPacket {
            stamp,
            start: samples,
            at,
            len,
        });
        // The mode is in a packet's first byte, so the first bytes are all that is read.
        let mut head = [0u8; 8];
        let n = len.min(head.len());
        file.seek(SeekFrom::Start(at)).ok()?;
        file.read_exact(&mut head[..n]).ok()?;
        let size = headers.packet_blocksize(&head[..n]).ok()?;
        if let Some(previous) = previous {
            samples += (previous / 4 + size / 4) as u64;
        }
        previous = Some(size);
    }
    Some(VorbisStarts { packets })
}

/// The fixed length of a Vorbis identification header (Vorbis I, A.2).
const VORBIS_IDENT_LEN: usize = 30;

/// A Vorbis comment header with an empty vendor string and no comments, framed:
/// what `ec-vorbis` needs where symphonia's extra data carries none.
const VORBIS_EMPTY_COMMENTS: &[u8] = b"\x03vorbis\x00\x00\x00\x00\x00\x00\x00\x00\x01";

/// Interleaves per-channel planes, in the order they are given.
fn interleave_planes(planes: &[Vec<f32>], out: &mut Vec<f32>) {
    let frames = planes.first().map_or(0, Vec::len);
    out.clear();
    out.reserve(frames * planes.len());
    for frame in 0..frames {
        for plane in planes {
            out.push(plane.get(frame).copied().unwrap_or(0.0));
        }
    }
}

/// Cuts one Vorbis packet's decoded block down to the samples symphonia says
/// the packet owns: `dur` after its trims, from `trim_start`. Symphonia takes
/// both from the page granules (the stream's last block overhangs its end and
/// the first block has nothing before it), so the cut is the container's own.
/// A packet stating no duration keeps all of its block.
fn vorbis_packet_window(out: &mut Vec<f32>, channels: usize, packet: &Packet) {
    let channels = channels.max(1);
    let frames = out.len() / channels;
    let start = usize::try_from(packet.trim_start.get())
        .unwrap_or(usize::MAX)
        .min(frames);
    let end = match packet.dur.get() {
        0 => frames,
        dur => usize::try_from(dur).unwrap_or(usize::MAX).saturating_add(start).min(frames),
    };
    out.truncate(end * channels);
    out.drain(..start * channels);
}

/// Samples per channel of a full HE-AAC frame: the SBR output of a 1024-sample
/// core. A frame whose header is missing comes out as the core alone, half this.
const SBR_FULL_FRAME: usize = 2048;

/// The packet an SBR track's decoder starts on: the nearest one at or before
/// `from` whose own frame decodes with its SBR header, so every frame after it
/// is reconstructed. Headers come back every few packets (every 10 in libfdk's
/// streams), not on each one; a fresh decoder on a headerless frame gives its
/// core alone, which is how a frame without its header is told apart. Packet 1
/// always carries one.
fn sbr_start_packet(track: &mut AacTrack, from: u32) -> u32 {
    let mut id = from.max(1);
    while id > 1 {
        if sbr_packet_has_header(track, id) {
            return id;
        }
        id -= 1;
    }
    1
}

/// Whether packet `id`, decoded on its own by a fresh decoder, comes out as a
/// full SBR frame. A failed read or decode counts as no.
fn sbr_packet_has_header(track: &mut AacTrack, id: u32) -> bool {
    let Ok(Some(sample)) = mp4track::sample(&mut track.reader, track.stream, id) else {
        return false;
    };
    let Ok(mut decoder) = aac_decoder(&track.params, track.source_channels()) else {
        return false;
    };
    let packet = Packet::new(
        track.track_id,
        units::Timestamp::new(sample.dts.unwrap_or(0)),
        units::Duration::new(u64::from(sample.duration.unwrap_or(0).max(0) as u32)),
        &sample.data[..],
    );
    let mut out = Vec::new();
    if decoder.decode(&packet, &mut out).is_err() {
        return false;
    }
    out.len() >= SBR_FULL_FRAME * track.source_channels()
}

/// Where packet `id` starts on the track's timeline, from its time-to-sample
/// entries (`(count, delta)` runs, as [`packet_at`] reads them).
fn packet_start_ts(stts: &[(u32, u32)], id: u32) -> u64 {
    let mut first = 1u32;
    let mut ts = 0u64;
    for &(count, delta) in stts {
        if id < first + count {
            return ts + u64::from(id - first) * u64::from(delta);
        }
        first += count;
        ts += u64::from(count) * u64::from(delta);
    }
    ts
}

/// The decoder for an AAC track of `from` channels: `ec-aac`, whatever the
/// width and whether or not the stream is HE-AAC. symphonia's own refused SBR
/// outright ("aac: aac too complex", `aac/mod.rs:91-96`), so it is not asked.
///
/// The config is the track's AudioSpecificConfig when it has one (mp4 `esds`,
/// Matroska `CodecPrivate`), which is what arms SBR. An ADTS stream has none,
/// and `ec-aac` takes its parameters from the first frame it reads.
fn aac_decoder(params: &AudioCodecParameters, from: usize) -> crate::Result<SymDecoder> {
    let decoder = match params.extra_data.as_deref() {
        Some(asc) => ec_aac::AacDecoder::with_config_bytes(asc)
            .map_err(|e| format!("AAC decoder init failed: {e}"))?,
        None => ec_aac::AacDecoder::new(),
    };
    Ok(SymDecoder::Aac(Box::new(decoder), from))
}

/// The rate an AudioSpecificConfig decodes at: the extension rate for HE-AAC
/// (SBR reconstructed), the core rate otherwise. `Err` for a config `ec-aac`
/// cannot parse, which is a track that will not decode either.
fn aac_output_rate(asc: &[u8]) -> crate::Result<Option<u32>> {
    let decoder = ec_aac::AacDecoder::with_config_bytes(asc)
        .map_err(|e| format!("AAC config does not parse: {e}"))?;
    Ok(decoder.output_sample_rate())
}

/// How many access units the implicit-SBR probe decodes. An SBR payload shows
/// in the first frame that carries one, and every SBR frame carries one, so two
/// frames are enough and bound the cost of opening a track.
const AAC_PRIME_UNITS: usize = 2;

/// Whether an AAC track's rate can still change under implicit SBR: no config
/// (ADTS), or a plain AAC-LC one. An explicit SBR config states its rate, and a
/// PS one states its stereo output and rate, so neither is decoded to find out.
fn aac_may_be_implicit(asc: Option<&[u8]>) -> crate::Result<bool> {
    let Some(asc) = asc else {
        return Ok(true);
    };
    let decoder = ec_aac::AacDecoder::with_config_bytes(asc)
        .map_err(|e| format!("AAC config does not parse: {e}"))?;
    Ok(decoder.sbr_support() == ec_aac::SbrSupport::NotSignalled)
}

/// What an AAC track decodes at, as a throwaway decoder finds it after its first
/// access units.
struct AacPrimed {
    sample_rate: u32,
    channels: u16,
    /// Whether an implicit SBR payload switched the rate on.
    implicit: bool,
}

/// Decodes up to [`AAC_PRIME_UNITS`] of `units` through a fresh decoder, built
/// from `asc` or else from the first ADTS header, and reads back the rate it
/// now decodes at. `None` when no unit decodes: the caller keeps the rate its
/// container states, and the decode error surfaces on playback as it always has.
fn aac_prime(asc: Option<&[u8]>, units: &[&[u8]]) -> crate::Result<Option<AacPrimed>> {
    let mut decoder = match asc {
        Some(asc) => ec_aac::AacDecoder::with_config_bytes(asc)
            .map_err(|e| format!("AAC config does not parse: {e}"))?,
        None => ec_aac::AacDecoder::new(),
    };
    let mut channels = None;
    for unit in units.iter().take(AAC_PRIME_UNITS) {
        let Ok(pcm) = decoder.decode(unit, None) else {
            break;
        };
        channels = Some(pcm.channels);
        if decoder.sbr_implicit() {
            break;
        }
    }
    let (Some(channels), Some(sample_rate)) = (channels, decoder.output_sample_rate()) else {
        return Ok(None);
    };
    Ok(Some(AacPrimed {
        sample_rate,
        channels,
        implicit: decoder.sbr_implicit(),
    }))
}

/// The first `count` packets of track `track_id` (for Matroska, its `TrackNumber`),
/// read through a fresh reader. Empty where the file cannot be read or has none,
/// and then the caller keeps what the header states.
pub(crate) fn first_packets(path: &Path, track_id: u32, count: usize) -> Vec<Vec<u8>> {
    let Ok(mut reader) = sym_reader(path) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        match reader.next_packet() {
            Ok(Some(packet)) if packet.track_id == track_id => out.push(packet.data.to_vec()),
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    out
}

/// [`aac_prime`] over the first packets of `track_id`, read through a fresh
/// reader on `path`. A raw ADTS frame carries its own header, so it is decoded
/// as it stands.
fn sym_aac_prime(
    path: &Path,
    track_id: u32,
    asc: Option<&[u8]>,
) -> crate::Result<Option<AacPrimed>> {
    let mut reader = sym_reader(path)?;
    let mut units: Vec<Box<[u8]>> = Vec::with_capacity(AAC_PRIME_UNITS);
    while units.len() < AAC_PRIME_UNITS {
        match reader.next_packet()? {
            Some(packet) if packet.track_id == track_id => units.push(packet.data),
            Some(_) => {}
            None => break,
        }
    }
    let refs: Vec<&[u8]> = units.iter().map(|u| &u[..]).collect();
    aac_prime(asc, &refs)
}

/// [`aac_prime`] over the first samples of an mp4 track, read through a fresh
/// reader on `path`: the track's own reader is borrowed by the track it opens.
fn mp4_aac_prime(path: &Path, stream: u32, asc: &[u8]) -> crate::Result<Option<AacPrimed>> {
    let mut reader = mp4track::open(path)?;
    let mut units: Vec<Vec<u8>> = Vec::with_capacity(AAC_PRIME_UNITS);
    for id in 1..=AAC_PRIME_UNITS as u32 {
        match mp4track::sample(&mut reader, stream, id) {
            Ok(Some(sample)) => units.push(sample.data.to_vec()),
            _ => break,
        }
    }
    let refs: Vec<&[u8]> = units.iter().map(Vec::as_slice).collect();
    aac_prime(Some(asc), &refs)
}

/// The encoder delay of an Opus track, in samples at 48 kHz: bytes 10 and 11 of
/// `OpusHead`, little-endian. Zero for a track whose header is missing or too
/// short to hold one -- there is nothing to subtract that we can trust.
fn opus_pre_skip(head: &[u8]) -> u64 {
    match head {
        [
            b'O',
            b'p',
            b'u',
            b's',
            b'H',
            b'e',
            b'a',
            b'd',
            _,
            _,
            lo,
            hi,
            ..,
        ] => u64::from(u16::from_le_bytes([*lo, *hi])),
        _ => 0,
    }
}

/// The stream layout of an Opus track, off the file's own `OpusHead`: how many
/// elementary streams one packet carries, how many of those are coupled
/// (stereo), and which decoded channel each output channel comes from. Both
/// readers hand the header over as `extra_data` verbatim -- an mkv's
/// `CodecPrivate`, an ogg's identification packet.
///
/// Mapping family 0 carries no table at all and is mono or stereo by definition;
/// every other family writes one (RFC 7845 §5.1), and family 1 -- the Vorbis
/// surround layout a 5.1 film track is written with -- is four streams of which
/// two are coupled.
///
/// Everything here is checked rather than trusted: `MultistreamDecoder::with_rate`
/// *panics* on a layout that breaks the RFC's limits, and this header comes
/// straight out of a file, so a truncated or hostile one has to come back as an
/// `Err` and be refused like any other undecodable track.
fn opus_layout(head: &[u8], channels: usize) -> crate::Result<(usize, usize, Vec<u8>)> {
    if !(1..=255).contains(&channels) {
        return Err(format!("an Opus track of {channels} channels").into());
    }
    let table = match head {
        [b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', rest @ ..] if rest.len() > 10 => {
            // Byte 18 of the header is the mapping family; the table, when there
            // is one, is the two counts and one byte per output channel after it.
            match rest[10] {
                0 => None,
                _ => Some(rest.get(11..13 + channels).ok_or("a truncated OpusHead")?),
            }
        }
        // No header at all: the container declared the channel count and nothing
        // else, which only family 0 can honestly be.
        _ => None,
    };
    let Some(table) = table else {
        if channels > 2 {
            return Err(format!("an Opus track of {channels} channels with no mapping").into());
        }
        return Ok((1, channels - 1, (0..channels as u8).collect()));
    };
    let (streams, coupled) = (usize::from(table[0]), usize::from(table[1]));
    let mapping = table[2..].to_vec();
    let decoded = streams + coupled;
    if streams < 1 || coupled > streams || decoded > 255 {
        return Err(format!("an Opus track of {streams} streams, {coupled} coupled").into());
    }
    if !mapping
        .iter()
        .all(|&m| m == 255 || usize::from(m) < decoded)
    {
        return Err(format!("an Opus channel mapping outside its {decoded} streams").into());
    }
    Ok((streams, coupled, mapping))
}

/// Surround Opus is stored in Vorbis channel order (RFC 7845 §5.1.1.2) and
/// [`downmix`] reads the film order every other decoder here hands back. This is
/// that permutation, in place, per width: each entry says which *source* channel
/// the output slot of that index reads. Mono, stereo and quad agree already, and
/// a width the RFC does not lay out is left alone rather than shuffled by guess.
fn vorbis_to_film_order(samples: &mut [f32], from: usize) {
    let order: &[usize] = match from {
        3 => &[0, 2, 1],                // FL FC FR
        5 => &[0, 2, 1, 3, 4],          // FL FC FR SL SR
        6 => &[0, 2, 1, 5, 3, 4],       // FL FC FR SL SR LFE
        7 => &[0, 2, 1, 6, 5, 3, 4],    // FL FC FR SL SR BC LFE
        8 => &[0, 2, 1, 7, 5, 6, 3, 4], // FL FC FR SL SR BL BR LFE
        _ => return,
    };
    let mut frame = [0.0f32; 8];
    for chunk in samples.chunks_exact_mut(from) {
        frame[..from].copy_from_slice(chunk);
        for (out, &src) in chunk.iter_mut().zip(order) {
            *out = frame[src];
        }
    }
}

/// What each channel of a `from`-wide film-order layout contributes to the left
/// and the right of a stereo fold. ITU-R BS.775: a front channel keeps its own
/// side, the centre and the back centre go to both at -3 dB, a surround joins
/// its own side at -3 dB, and the LFE is dropped -- it carries no programme a
/// stereo pair could put anywhere, and neither ffmpeg's default downmix nor any
/// player's puts it there.
///
/// The film order is FL, FR, FC, LFE, BL, BR, SL, SR, which is what symphonia's
/// decoders and `ec-aac` both hand back (the latter reorders AAC's own
/// C, L, R, Ls, Rs, LFE element order into it: its `film_order`) and what the
/// AC-3 decoder's own passthrough uses.
///
/// corner-cut: a width past 7.1 (22.2, ambisonics) keeps its front pair and drops
/// the rest -- no table here places those, and inventing one silently would be
/// worse than a fold that is merely narrow. Upgrade path is one more arm.
fn fold_coeffs(from: usize) -> Vec<(f32, f32)> {
    const C: f32 = std::f32::consts::FRAC_1_SQRT_2;
    const L: (f32, f32) = (1.0, 0.0);
    const R: (f32, f32) = (0.0, 1.0);
    const M: (f32, f32) = (C, C);
    const SL: (f32, f32) = (C, 0.0);
    const SR: (f32, f32) = (0.0, C);
    const X: (f32, f32) = (0.0, 0.0);
    match from {
        3 => vec![L, R, X],                    // L R LFE (the 2.1 passthrough)
        4 => vec![L, R, SL, SR],               // FL FR BL BR
        5 => vec![L, R, M, SL, SR],            // FL FR FC BL BR
        6 => vec![L, R, M, X, SL, SR],         // 5.1
        7 => vec![L, R, M, X, M, SL, SR],      // FL FR FC LFE BC SL SR
        8 => vec![L, R, M, X, SL, SR, SL, SR], // 7.1
        n => (0..n).map(|c| [L, R][usize::from(c > 0)]).collect(),
    }
}

/// Anything wider than a stereo pair down to one, in place -- `from` is the
/// channel count the decoder actually emitted, and 1 or 2 is not touched at all.
/// This is what makes a 5.1 or 7.1 film an ordinary stereo source on the
/// timeline instead of an import refusal.
///
/// Both outputs are divided by the sum of their own coefficients
/// ([`fold_coeffs`]), so a source at full scale cannot come out clipped.
///
/// corner-cut: that normalisation costs 7.7 dB at 5.1 (9.9 at 7.1) against what
/// ffmpeg's `-ac 2` hands out, which does not normalise and can overload -- so a
/// film plays quieter here than in a player beside it. It is the loudness the
/// AC-3 5.1 path already has (measured within 1.3 dB), so the two agree; the
/// upgrade path is a limiter on the output, at which point the coefficients can
/// go back to 1.0.
fn downmix(samples: &mut Vec<f32>, from: usize) {
    if from < 3 {
        return;
    }
    let coeffs = fold_coeffs(from);
    let (left, right) = coeffs
        .iter()
        .fold((0.0f32, 0.0f32), |(l, r), c| (l + c.0, r + c.1));
    // Never a boost: a layout with nothing on one side stays where it is.
    let (left, right) = (1.0 / left.max(1.0), 1.0 / right.max(1.0));
    for frame in 0..samples.len() / from {
        let (mut l, mut r) = (0.0, 0.0);
        for (c, &(cl, cr)) in coeffs.iter().enumerate() {
            let sample = samples[frame * from + c];
            l += cl * sample;
            r += cr * sample;
        }
        samples[2 * frame] = l * left;
        samples[2 * frame + 1] = r * right;
    }
    samples.truncate(samples.len() / from * 2);
}

/// The inverse of [`downmix`]: a mono buffer duplicated into every channel of
/// a wider timeline, in place. `from` is the width the decoder actually
/// handed back (already folded to 1 or 2 by [`downmix`] at its own door) and
/// `to` is the timeline's own width ([`mix`]'s `channels`); anything that is
/// not exactly the mono-into-wider case is left untouched -- a source already
/// at `to`, and a source wider than `to` other than a stereo pair onto a mono
/// timeline. That one is the remix the other way: each frame's two channels
/// are averaged into the timeline's single one (half of each, so full scale
/// in is full scale out), the same fold ffmpeg's `-ac 1` makes. Without it a
/// stereo file joining a mono timeline would hand the mixer twice the samples
/// it expects and play at double speed.
fn widen(samples: &mut Vec<f32>, from: usize, to: usize) {
    if from == 2 && to == 1 {
        let mut out = Vec::with_capacity(samples.len() / 2);
        out.extend(
            samples
                .as_chunks::<2>()
                .0
                .iter()
                .map(|f| (f[0] + f[1]) * 0.5),
        );
        *samples = out;
        return;
    }
    if from != 1 || to <= 1 {
        return;
    }
    let mut out = Vec::with_capacity(samples.len() * to);
    for &s in samples.iter() {
        out.extend(std::iter::repeat_n(s, to));
    }
    *samples = out;
}

/// One source's AC-3 track: the same mp4 sample tables the AAC track is read
/// from, decoded through `ec-ac3` and **downmixed to stereo by the decoder
/// itself** (ATSC A/52 §7.8.2, [`ec_ac3::Downmix::Stereo`]), because one output
/// device and one timeline layout is all there is. A 5.1 BluRay track therefore
/// arrives here as an ordinary stereo source, and its rows in the picker say so.
///
/// `channels` is what the decoder actually emitted for the first frame rather
/// than an assumed 2: everything from stereo to 5.1 downmixes to stereo and a
/// mono track passes through as the one channel it is (`acmod` carries the
/// programme in the centre; duplicating it is the timeline's business, see
/// [`widen`]). The §7.8.2 fold is position-aware — every `acmod` from 1/0 to
/// 3/2 puts its centre and surrounds where the matrix says — so no width is
/// refused or mislabelled.
struct Ac3Track {
    reader: mp4track::Reader,
    stream: u32,
    sample_count: u32,
    sample_rate: u32,
    channels: u16,
    timescale: u32,
    priming: u64,
    total_samples: Option<u64>,
    stts: Vec<(u32, u32)>,
    /// What the decoder is asked to hand out, from this track's own layout:
    /// [`ec_ac3::Downmix::Stereo`] for anything with more than one front
    /// channel, [`ec_ac3::Downmix::Native`] — the coded channels untouched —
    /// for a mono track. See [`ac3_decoder`].
    requested: Option<u16>,
}

impl Ac3Track {
    /// The `stream`-th audio track of `path` when it is AC-3, `Ok(None)` when it
    /// is anything else (including out of range) — the caller then goes on to
    /// the AAC reader, which owns those verdicts and their wording.
    ///
    /// mp4 0.14 parses `mp4a` sample entries and drops every other kind without
    /// keeping the fourcc, so what the track *is* comes out of the `stsd` by
    /// hand ([`crate::demux::sample_entry`]); the rate and the channel count
    /// come out of the first syncframe, which is where AC-3 states them.
    fn open(path: &Path, stream: usize) -> crate::Result<Option<Self>> {
        let mut reader = mp4track::open(path)?;
        let Some(&(track_id, stream_id)) = mp4track::audio_tracks(&reader).get(stream) else {
            return Ok(None);
        };
        let Some(stream_id) = stream_id else {
            return Ok(None);
        };
        if &mp4track::sample_entry(&reader, track_id) != b"ac-3" {
            return Ok(None);
        }
        let timescale = mp4track::timescale(&reader, stream_id);
        let duration = mp4track::duration(&reader, stream_id).unwrap_or(0);
        let edit = mp4track::media_time(&reader, track_id);
        let stts = mp4track::stts(&reader, stream_id);
        let sample_count = mp4track::sample_count(&reader, stream_id);

        let first = mp4track::sample(&mut reader, stream_id, 1)?
            .ok_or("the AC-3 track has no samples")?;
        let sample_rate = ec_ac3::syncinfo::parse(&first.data)
            .map_err(|e| format!("not a readable AC-3 syncframe: {e:?}"))?
            .sample_rate;
        let nfchans = ec_ac3::bsi::parse(first.data.get(5..).unwrap_or_default())
            .map_err(|e| format!("not a readable AC-3 bit stream information: {e:?}"))?
            .nfchans;
        let requested = (nfchans > 1).then_some(2);
        let mut decoder = ac3_decoder(requested)?;
        let channels = decode_ac3(&mut decoder, &first.data)?
            .map(|(pcm, samples)| (pcm.len() as u64 / samples.max(1)) as u16)
            .filter(|&c| c > 0)
            .ok_or("the first AC-3 syncframe decoded to nothing")?;

        // No encoder delay in AC-3; a remux that writes an edit list is still
        // honoured, exactly as the AAC track honours it.
        let priming = edit
            .and_then(|t| scale(t, sample_rate, timescale))
            .unwrap_or(0);
        Ok(Some(Self {
            stream: stream_id,
            sample_count,
            sample_rate,
            channels,
            timescale,
            priming,
            total_samples: scale(duration, sample_rate, timescale)
                .map(|d| d.saturating_sub(priming)),
            stts,
            requested,
            reader,
        }))
    }

    /// `secs` on this source's audible timeline into media samples.
    fn media(&self, secs: f64) -> u64 {
        ((secs * f64::from(self.sample_rate)).round() as u64).saturating_add(self.priming)
    }

    /// One `[start, end)` window, resolved to syncframes and media samples —
    /// [`AacTrack::segment`] with AC-3's own frame length and pre-roll.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        let media_target = self.media(start_secs);
        let target_ts = unscale(media_target, self.sample_rate, self.timescale);
        let (start_id, start_ts) = packet_at(self.stts.iter().copied(), target_ts, AC3_PRE_ROLL);
        Segment {
            source: Some(source),
            start_id,
            start_pos: scale(start_ts, self.sample_rate, self.timescale).unwrap_or(0),
            media_target,
            media_end: self.media(end_secs).max(media_target),
        }
    }
}

/// One source's AC-3 or E-AC-3 track out of a **Matroska** file: the very same
/// decoder and the very same §7.8.2 stereo downmix [`Ac3Track`] runs, fed out of
/// the container's blocks instead of an mp4 sample table. A 5.1 E-AC-3 remux
/// therefore arrives on the timeline as an ordinary stereo source, exactly as
/// the mp4 path's does, and mono passes through exactly the same way.
///
/// Matroska indexes no samples, so what an `stts` walk answers on the mp4 side
/// is answered here by the blocks' own timestamps ([`crate::demux::MkvAudio`]):
/// one block is one packet, one packet is one syncframe, and a seek lands on
/// the block at or before the target with [`AC3_PRE_ROLL`] blocks of warm-up in
/// front of it.
struct MkvAc3Track {
    mkv: crate::demux::MkvAudio,
    sample_rate: u32,
    /// What comes *out* of the decoder, measured on the first frame — 2 for
    /// anything from stereo to 5.1, 1 for a mono track (see `requested`).
    channels: u16,
    total_samples: Option<u64>,
    /// What the decoder is asked to hand out, from this track's own layout:
    /// `Some(2)` for anything with more than one front channel, `None` for a
    /// mono track. See [`ac3_decoder`].
    requested: Option<u16>,
}

impl MkvAc3Track {
    /// `Ok(None)` when the file's first audio track is not AC-3 or E-AC-3 —
    /// that mkv is the silent source it has always been. An `Err` is a track
    /// that is one and could not be read, which
    /// [`AudioSession::unsupported`] quotes back to the user.
    fn open(path: &Path) -> crate::Result<Option<Self>> {
        Self::from_blocks(crate::demux::MkvAudio::open(path)?)
    }

    /// [`open`](Self::open) on one **named** track ([`MkvAudioTrack::number`]),
    /// which is how the AC-3 half of a dual-audio file is reached.
    ///
    /// [`MkvAudioTrack::number`]: crate::demux::MkvAudioTrack::number
    fn open_track(path: &Path, number: u64) -> crate::Result<Option<Self>> {
        Self::from_blocks(crate::demux::MkvAudio::open_track(path, number)?)
    }

    fn from_blocks(mkv: Option<crate::demux::MkvAudio>) -> crate::Result<Option<Self>> {
        let Some(mut mkv) = mkv else {
            return Ok(None);
        };
        // A TrueHD or MLP track has blocks too, and its own decoder ([`MkvTruehdTrack`]).
        if !matches!(mkv.codec, "ac3" | "eac3") {
            return Ok(None);
        }
        let first = mkv
            .frame(0)?
            .ok_or("the AC-3 track of this Matroska file has no blocks")?;
        // Rate and layout come out of the bit stream, which is where AC-3
        // states them: Matroska's `Audio` element repeats them and muxers get
        // that wrong often enough that the frame is the better source.
        // `nfchans` excludes the LFE in both syntaxes, which is what the
        // downmix decision wants.
        let (sample_rate, nfchans) = if mkv.codec == "eac3" {
            // Annex E's BSI starts right after the 16-bit syncword.
            let bsi = ec_ac3::eac3::parse(first.get(2..).unwrap_or_default())
                .map_err(|e| format!("not a readable E-AC-3 bit stream information: {e:?}"))?;
            (bsi.sample_rate, bsi.nfchans)
        } else {
            let sync = ec_ac3::syncinfo::parse(&first)
                .map_err(|e| format!("not a readable AC-3 syncframe: {e:?}"))?;
            let bsi = ec_ac3::bsi::parse(first.get(5..).unwrap_or_default())
                .map_err(|e| format!("not a readable AC-3 bit stream information: {e:?}"))?;
            (sync.sample_rate, bsi.nfchans)
        };
        let requested = (nfchans > 1).then_some(2);
        let mut decoder = ac3_decoder(requested)?;
        let (pcm, samples) = decode_ac3(&mut decoder, &first)?
            .ok_or("the first AC-3 syncframe decoded to nothing")?;
        let channels = (pcm.len() as u64 / samples.max(1)) as u16;
        if channels == 0 {
            return Err("the first AC-3 syncframe decoded to nothing".into());
        }
        // The track runs to the last block's timestamp plus that block's own
        // frame -- a container that indexes nothing states no duration either,
        // and every frame of a syncframe stream is the same length.
        let last = mkv.secs(mkv.blocks().saturating_sub(1));
        Ok(Some(Self {
            total_samples: Some((last * f64::from(sample_rate)) as u64 + samples),
            sample_rate,
            channels,
            requested,
            mkv,
        }))
    }

    /// `secs` on this source's timeline into media samples. No priming: the
    /// container carries no edit list and AC-3 no encoder delay.
    fn media(&self, secs: f64) -> u64 {
        (secs.max(0.0) * f64::from(self.sample_rate)).round() as u64
    }

    /// One `[start, end)` window, resolved to blocks and media samples —
    /// [`Ac3Track::segment`] against block timestamps instead of an `stts`.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        let media_target = self.media(start_secs);
        let start_id = self.mkv.block_at(start_secs, AC3_PRE_ROLL as usize);
        Segment {
            source: Some(source),
            start_id: start_id as u32,
            start_pos: self.media(self.mkv.secs(start_id)),
            media_target,
            media_end: self.media(end_secs).max(media_target),
        }
    }
}

/// A fresh AC-3 / E-AC-3 decoder. `requested` is the downmix ask: `Some(2)`
/// hands back the library's own A/52 §7.8.2 stereo fold — per-position
/// coefficients from the frame's own `acmod` and mix levels, LFE dropped,
/// normalised so a full-scale mix cannot clip — which is the whole reason the
/// timeline can carry a 5.1 track at all; `None` passes the coded channels
/// through, which is what a mono track asks for (`acmod` 1/0 carries the
/// programme in the centre, and duplicating it is the timeline's business, see
/// [`widen`]). Fresh per segment for the same reason every other decoder here
/// is: a seek leaves overlap-add state belonging to the frames we skipped.
///
/// The syntax is not an argument: one decoder reads AC-3 and E-AC-3 alike,
/// dispatching per packet on `bsid`.
fn ac3_decoder(requested: Option<u16>) -> crate::Result<ec_ac3::Ac3Decoder> {
    Ok(ec_ac3::Ac3Decoder::with_options(ec_ac3::Options {
        downmix: requested
            .map(|_| ec_ac3::Downmix::Stereo)
            .unwrap_or(ec_ac3::Downmix::Native),
        ..ec_ac3::Options::default()
    }))
}

/// One syncframe in, one buffer of interleaved f32 out and the frames per
/// channel it holds. `Ok(None)` when the decoder wants more input before it can
/// hand a frame back, which is not an error. The library speaks interleaved
/// little-endian f32 — the same `[-1, 1)` every other reader here emits.
///
/// The frame count comes back beside the samples because an E-AC-3 frame is not
/// the fixed 1536 (6 blocks of 256) an AC-3 one is -- `numblkscod` may say 1, 2
/// or 3 blocks instead of 6 -- and dividing by an assumed length would put the
/// channel count and every media position after it out.
fn decode_ac3(
    decoder: &mut ec_ac3::Ac3Decoder,
    bytes: &[u8],
) -> crate::Result<Option<(Vec<f32>, u64)>> {
    match decoder.decode_frame(bytes) {
        Ok(frame) => Ok(Some((
            frame.data[0]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|s| f32::from_le_bytes(*s))
                .collect(),
            frame.samples as u64,
        ))),
        Err(ec_ac3::Error::NeedMore) => Ok(None),
        Err(e) => Err(format!("AC-3 decode failed: {e}").into()),
    }
}

/// One source's AAC track, resolved once: the reader plus everything the header
/// has to say about it. `stts` is copied out because every other field would
/// otherwise keep a borrow alive on the reader the worker needs mutably — a real
/// AAC track has one entry, so it is two `u32`s.
struct AacTrack {
    reader: mp4track::Reader,
    stream: u32,
    track_id: u32,
    sample_count: u32,
    sample_rate: u32,
    timescale: u32,
    /// Encoder delay of *this* file. Sources are joined, priming is not shared.
    priming: u64,
    total_samples: Option<u64>,
    stts: Vec<(u32, u32)>,
    /// Channels the decoder emits, from the config: 1 for mono.
    layout_channels: usize,
    freq_index: u8,
    chan_conf: u8,
    /// HE-AAC: the stream carries SBR, so [`sample_rate`](Self::sample_rate) is
    /// the extension rate and a packet holds twice the core's frames. Such a
    /// track decodes but cannot be packet-copied, because the copy writes an
    /// AAC-LC config back out from `freq_index`/`chan_conf`.
    sbr: bool,
    /// A fresh decoder is built from these per segment: seeking mid-stream
    /// leaves MDCT and PNS state that belongs to the packets we skipped.
    params: AudioCodecParameters,
}

impl AacTrack {
    /// The `stream`-th audio track of `path`, counted in file order over *all*
    /// audio tracks — the numbering [`AudioSession::probe_streams`] hands out.
    ///
    /// Stream 0 is best effort: `Ok(None)` when there is no AAC track there at
    /// all, which is a valid silent source (a file whose audio is AC-3 still has
    /// a picture worth editing) and, for a standalone audio file, the door
    /// through to symphonia ([`Track::open`]). A stream the caller *named* is a
    /// promise instead, so out of range or not AAC there is an `Err` rather than
    /// silence the caller did not ask for.
    fn open(path: &Path, stream: usize) -> crate::Result<Option<Self>> {
        let reader = mp4track::open(path)?;
        let audio = mp4track::audio_tracks(&reader);
        let Some(&(track_id, codec_stream)) = audio.get(stream) else {
            return match stream {
                0 => Ok(None),
                n => Err(format!(
                    "{}: audio stream {n} of {} streams",
                    path.display(),
                    audio.len()
                )
                .into()),
            };
        };
        let stream_id = match codec_stream {
            Some(s) if mp4track::codec(&reader, s) == Some(CodecId::Aac) => s,
            _ => {
                return match stream {
                    0 => Ok(None),
                    n => Err(format!("{}: audio stream {n} is not AAC", path.display()).into()),
                };
            }
        };
        // The config as the file states it, extension frequency and all: the
        // decoder needs the SBR part, which a bare rate and channel count do not
        // carry.
        let asc: Box<[u8]> = mp4track::extradata(&reader, stream_id)
            .ok_or_else(|| format!("{}: AAC track has no config", path.display()))?;
        // Refuse early with a message instead of failing packet by packet. LC,
        // HE-AAC v1 (SBR) and HE-AAC v2 (SBR with PS) all decode through `ec-aac`.
        let (mut sbr, ps) = match asc_fields(&asc)?.0 {
            AAC_LC => (false, false),
            AAC_SBR => (true, false),
            AAC_PS => (true, true),
            other => {
                return Err(format!(
                    "unsupported AAC profile: audio object type {other} (only AAC-LC, HE-AAC v1 and v2)"
                )
                .into());
            }
        };

        let mut layout_channels = mp4track::channels(&reader, stream_id);
        if layout_channels == 0 {
            return Err(format!("{}: AAC config declares no channels", path.display()).into());
        }
        // The rate `ec-aac` decodes at, which for HE-AAC is the extension rate:
        // the container's own rate is the core's, and a timeline counted in it
        // would run at half speed against the decoded samples.
        let mut sample_rate = aac_output_rate(&asc)?
            .ok_or_else(|| format!("{}: AAC config declares no sample rate", path.display()))?;
        // Plain LC may carry SBR by implicit signalling: the rate is the decoded
        // one, read off the first access units, as for an ADTS or mkv stream.
        if aac_may_be_implicit(Some(&asc[..]))?
            && let Some(primed) = mp4_aac_prime(path, stream_id, &asc)?
        {
            sample_rate = primed.sample_rate;
            sbr |= primed.implicit;
            // The decoder's own width: an implicit PS payload makes a mono core
            // stereo, and the container's channel count does not say so.
            layout_channels = usize::from(primed.channels);
        }
        // Explicit HE-AAC v2 is stereo whatever the core's channel count.
        if ps {
            layout_channels = 2;
        }
        let timescale = mp4track::timescale(&reader, stream_id);
        let priming = priming_samples(
            mp4track::media_time(&reader, track_id),
            sample_rate,
            timescale,
        );
        let (_, freq_index, chan_conf) = asc_fields(&asc)?;
        let duration = mp4track::duration(&reader, stream_id);
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_extra_data(asc);
        let this = Self {
            sample_count: mp4track::sample_count(&reader, stream_id),
            stts: mp4track::stts(&reader, stream_id),
            total_samples: duration
                .and_then(|d| scale(d, sample_rate, timescale))
                .map(|d| d.saturating_sub(priming)),
            stream: stream_id,
            track_id,
            sample_rate,
            timescale,
            priming,
            layout_channels,
            freq_index,
            chan_conf,
            sbr,
            params,
            reader,
        };
        Ok(Some(this))
    }

    /// What reaches the timeline: mono, or the stereo everything wider is folded
    /// to by [`downmix`] at the decoder's door.
    fn channels(&self) -> u16 {
        match self.source_channels() {
            1 => 1,
            _ => 2,
        }
    }

    /// What the decoder emits, which is what the fold reads.
    fn source_channels(&self) -> usize {
        self.layout_channels.max(1)
    }

    fn track_params(&self) -> AacTrackParams {
        AacTrackParams {
            freq_index: self.freq_index,
            chan_conf: self.chan_conf,
            sample_rate: self.sample_rate,
        }
    }

    /// `secs` on this source's audible timeline into media samples (priming
    /// included, so it compares directly against a decoded position).
    /// `f64::INFINITY` saturates.
    fn media(&self, secs: f64) -> u64 {
        ((secs * f64::from(self.sample_rate)).round() as u64).saturating_add(self.priming)
    }

    /// One `[start, end)` window of this source's seconds, resolved to its own
    /// packets and media samples.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        let media_target = self.media(start_secs);
        let target_ts = unscale(media_target, self.sample_rate, self.timescale);
        // Two packets of pre-roll: AAC-LC's MDCT overlap-add needs the previous
        // packet to cancel aliasing, plus one more to warm the decoder up. Their
        // output is discarded below, so this only costs decode time. An SBR track
        // is walked back further when it is decoded ([`sbr_start_packet`]).
        let (start_id, start_ts) = packet_at(self.stts.iter().copied(), target_ts, PRE_ROLL);
        Segment {
            source: Some(source),
            start_id,
            start_pos: scale(start_ts, self.sample_rate, self.timescale).unwrap_or(0),
            media_target,
            // An inverted segment is an empty one, never a backwards run.
            media_end: self.media(end_secs).max(media_target),
        }
    }
}

/// [`packet_run`] for the last segment of a copy. Rounded to nearest, the debt
/// that run leaves is never paid: `test_sync.mp4`'s 4.0 s is 172.27 packets, so
/// 172 were copied and the sound stopped 6 ms short of its picture. Nothing
/// follows the last segment to take the shortfall, so this rounds up instead --
/// at most one packet, under a frame, past the picture.
fn packet_run_last(err: &mut i64, ideal: i64, available: u32) -> u32 {
    let packet = i64::from(SAMPLES_PER_PACKET);
    let ideal = ideal.min(i64::from(available) * packet);
    let want = (ideal - *err) as f64 / packet as f64;
    let n = (want.ceil().max(0.0) as i64).min(i64::from(available)) as u32;
    *err += i64::from(n) * packet - ideal;
    n
}

/// How many whole packets to copy for a window of `ideal` samples per channel,
/// capped at the `available` packets left in the track.
///
/// `err` is "samples copied so far minus samples asked for", carried across the
/// segment joins: rounding to nearest *against the running debt* keeps the
/// cumulative error inside half a packet forever, where independent per-segment
/// rounding would random-walk away from sync (charter D1: < 1024 at every join).
fn packet_run(err: &mut i64, ideal: i64, available: u32) -> u32 {
    let packet = i64::from(SAMPLES_PER_PACKET);
    // Never carry a debt the track cannot pay: a segment running past the end
    // would otherwise inflate every later one. (Also tames `f64::INFINITY`,
    // which saturates to `i64::MAX` on the cast above.)
    let ideal = ideal.min(i64::from(available) * packet);
    let want = (ideal - *err) as f64 / packet as f64;
    let n = (want.round().max(0.0) as i64).min(i64::from(available)) as u32;
    *err += i64::from(n) * packet - ideal;
    n
}

/// The order a Matroska file's audio streams are numbered in, as positions into
/// `tracks` (which is file order): **stream 0 is the track this project has
/// always opened**, and the rest follow in file order behind it.
///
/// Not file order outright, and the difference is a saved project's sound. A
/// remux flags the language it wants heard with `FlagDefault` on whichever
/// track it likes -- mkvmerge writes it on the second one all the time -- and
/// that flag is what every reader here used to pick with. Numbering by file
/// position would have made stream 0 a *different* track on those files, so an
/// `.edith` project saved before this would quietly start playing the other
/// language. Nothing that already plays may change what it plays.
///
/// The old pick is not re-derived here, it is *run*: the AC-3 arm asks
/// [`MkvAc3Track`] the same question [`Track::open`] asked it (and only of the
/// file's first audio track, which is as far as [`crate::demux::MkvAudio`]
/// ever looked), and everything else asks symphonia for its default track,
/// which is `FlagDefault` where a muxer set one and the first readable track
/// where nobody did.
pub(crate) fn mkv_stream_order(path: &Path, tracks: &[crate::demux::MkvAudioTrack]) -> Vec<usize> {
    // One track is its own order, and it is the common case: no second probe
    // for the file that cannot have a second answer.
    if tracks.len() < 2 {
        return (0..tracks.len()).collect();
    }
    let head = match tracks.first() {
        Some(first) if dolby_track_opens(path, first) => 0,
        // A file symphonia will not open at all keeps file order: the open
        // itself is where that failure is reported, not the numbering.
        _ => SymTrack::open(path)
            .ok()
            .flatten()
            .and_then(|track| {
                tracks
                    .iter()
                    .position(|entry| entry.number == u64::from(track.track_id))
            })
            .unwrap_or(0),
    };
    std::iter::once(head)
        .chain((0..tracks.len()).filter(|i| *i != head))
        .collect()
}

/// Whether `entry` is an AC-3, E-AC-3, TrueHD or MLP track one of the Dolby
/// decoders reads straight out of the blocks, and that decoder opens. The
/// question [`mkv_stream_order`] asks of the file's first track.
fn dolby_track_opens(path: &Path, entry: &crate::demux::MkvAudioTrack) -> bool {
    match entry.codec.as_str() {
        "A_AC3" | "A_EAC3" => matches!(MkvAc3Track::open_track(path, entry.number), Ok(Some(_))),
        "A_TRUEHD" | "A_MLP" => {
            matches!(MkvTruehdTrack::open_track(path, entry.number), Ok(Some(_)))
        }
        id if crate::extaudio::is_mkv_dts(id) || crate::extaudio::is_mkv_mpeg_layer(id) => {
            matches!(crate::extaudio::ExtFileTrack::open_native(path, entry.number), Ok(Some(_)))
        }
        _ => false,
    }
}

/// The name a Matroska audio row wears: the container's own codec id, cut down
/// to what a picker shows — `A_AAC` is `aac`, `A_EAC3` is `e-ac-3`.
///
/// The codec id rather than symphonia's short name, because the rows that most
/// need naming are exactly the ones it has no decoder for: an Opus or DTS track
/// is greyed out with the codec in the reason (`unusable` in the app reads this
/// very string), where symphonia would call it "an unsupported format".
fn mkv_codec_name(id: &str) -> String {
    match id {
        "A_AC3" => "ac-3".into(),
        "A_EAC3" => "e-ac-3".into(),
        "A_MPEG/L3" => "mp3".into(),
        "A_MPEG/L1" | "A_MPEG/L2" => codec_display_name(id).to_ascii_lowercase(),
        _ => id.strip_prefix("A_").unwrap_or(id).to_ascii_lowercase(),
    }
}

/// The AAC `audioObjectType`s this reads: LC, HE-AAC v1 (LC with SBR) and
/// HE-AAC v2 (SBR with Parametric Stereo).
const AAC_LC: u8 = 2;
const AAC_SBR: u8 = 5;
const AAC_PS: u8 = 29;

/// `(profile, freq_index, chan_conf)` out of an `AudioSpecificConfig` (ISO
/// 14496-3 §1.6.2.1): the first five bits are the object type, the next four
/// the sampling-frequency index, the next four the channel configuration.
fn asc_fields(asc: &[u8]) -> crate::Result<(u8, u8, u8)> {
    match asc {
        [a, b, ..] => Ok((a >> 3, ((a & 0x7) << 1) | (b >> 7), (b >> 3) & 0xf)),
        _ => Err("AudioSpecificConfig shorter than its fixed header".into()),
    }
}

/// Encoder delay in samples-per-channel: the edit list's first real entry says
/// how far into the media the presentation starts.
///
/// corner-cut: an explicit `media_time` of 0 is taken at face value (no trim).
/// Muxers that write a zero edit list despite a primed stream would leak the
/// priming; upgrade path is preferring the codec delay when it disagrees.
fn priming_samples(media_time: Option<u64>, sample_rate: u32, timescale: u32) -> u64 {
    media_time
        .and_then(|t| scale(t, sample_rate, timescale))
        .unwrap_or(DEFAULT_PRIMING)
}

/// `value` from the track timescale into samples-per-channel. `None` on a zero
/// timescale, which would be a broken header.
fn scale(value: u64, sample_rate: u32, timescale: u32) -> Option<u64> {
    let timescale = u64::from(timescale);
    (timescale != 0).then(|| value * u64::from(sample_rate) / timescale)
}

/// Inverse of [`scale`]: samples-per-channel back into the track timescale.
fn unscale(samples: u64, sample_rate: u32, timescale: u32) -> u64 {
    match sample_rate {
        0 => 0,
        rate => samples * u64::from(timescale) / u64::from(rate),
    }
}

/// The 1-based packet holding media time `target_ts`, walked back by `pre_roll`
/// packets, plus that packet's own start time. Inverse of the mp4 crate's
/// private `sample_time` walk over stts (`track.rs:475`); its `SttsEntry` is
/// unnameable outside the crate, hence the `(sample_count, sample_delta)` pairs.
///
/// corner-cut: the walk-back is clamped to the start of the containing stts entry
/// rather than crossing into the previous one, so a target within `pre_roll`
/// packets of an entry boundary gets less pre-roll. AAC packets are uniformly
/// 1024 frames (one entry per track), so this cannot fire here; upgrade path is
/// carrying the previous entry's delta through the walk.
pub(crate) fn packet_at(
    entries: impl IntoIterator<Item = (u32, u32)>,
    target_ts: u64,
    pre_roll: u32,
) -> (u32, u64) {
    let mut first_id = 1u32;
    let mut elapsed = 0u64;
    let mut last = (1u32, 0u64);
    for (count, delta) in entries {
        let span = u64::from(count) * u64::from(delta);
        let index = if target_ts < elapsed + span && delta != 0 {
            ((target_ts - elapsed) / u64::from(delta)) as u32
        } else {
            // Past this entry: remember its last packet in case the target runs
            // past the end of the track entirely.
            last = (
                first_id + count.saturating_sub(1),
                elapsed + span.saturating_sub(u64::from(delta)),
            );
            first_id += count;
            elapsed += span;
            continue;
        };
        let index = index - index.min(pre_roll);
        return (
            first_id + index,
            elapsed + u64::from(index) * u64::from(delta),
        );
    }
    last
}

/// One window of a source to decode, resolved to media samples and packets —
/// or, with no source, a stretch of synthesised silence.
struct Segment {
    /// Index into the worker's tracks; the media positions below are that
    /// source's, not the timeline's. `None` for a gap, where only the length
    /// (`media_end`) means anything.
    source: Option<usize>,
    /// First packet to feed the decoder, 1-based; includes the pre-roll.
    start_id: u32,
    /// Media position of `start_id`'s first frame, in samples-per-channel.
    start_pos: u64,
    /// Media position of the first frame to emit. Everything decoded before it
    /// is pre-roll, priming, or seek overshoot, and gets dropped.
    media_target: u64,
    /// One past the last frame to emit; `u64::MAX` for "to the end of track".
    media_end: u64,
}

impl Segment {
    /// `frames` per channel of silence — a gap in the audio lane.
    fn silence(frames: u64) -> Self {
        Self {
            source: None,
            start_id: 0,
            start_pos: 0,
            media_target: 0,
            media_end: frames,
        }
    }
}

/// The sample of a mix at `rate` that `secs` of timeline (from the mix's start) ends on:
/// the one rounding [`owed_samples`] makes of its running total, so anything that
/// has to land on the mix's own samples (an export's warm-up) counts them the same way.
pub(crate) fn timeline_sample(secs: f64, rate: u32) -> u64 {
    (secs * f64::from(rate)).round() as u64
}

/// What each segment of a mix owes the timeline in samples, `None` for an
/// open-ended window. The sample each segment ends on is the running total of
/// the seconds it owes, rounded once against the mix rate, and a segment owes
/// the difference of two such roundings. Rounding each segment's own length
/// instead drifts: one frame at 59.94 fps is 800.8 samples, so every cut would
/// gain or lose up to a fifth of a sample, and a few thousand short cuts add up
/// to tens of samples.
fn owed_samples(
    segs: &[(Option<usize>, f64, f64)],
    speeds: &[Option<crate::project::Stretch>],
    rate: u32,
) -> Vec<Option<u64>> {
    let mut total_secs = 0.0f64;
    let mut at = 0u64;
    segs.iter()
        .enumerate()
        .map(|(i, &(_, start_secs, end_secs))| {
            let secs = speeds
                .get(i)
                .copied()
                .flatten()
                .map_or(end_secs - start_secs, |s| s.timeline_secs);
            if !secs.is_finite() {
                return None;
            }
            total_secs += secs.max(0.0);
            let end = timeline_sample(total_secs, rate);
            let owed = end.saturating_sub(at);
            at = end;
            Some(owed)
        })
        .collect()
}

/// One segment's rate conversion: the decoded samples read at the ratio of
/// the source's own sample rate to the timeline's, which is a pure rate
/// conversion -- the pitch is preserved because the frames are read at the
/// ratio of the rates rather than dropped, and 48 kHz onto a 44.1 kHz
/// timeline is one resampler, not two. Linear interpolation between the two
/// frames a fractional position falls between -- one multiply-add per
/// channel, which is nothing beside the decode that produced them, and for a
/// 48:44.1 ratio its worst-case image lands ~40 dB down; the alternative is
/// a windowed-sinc kernel and a new state machine for a conversion that is
/// not the audible risk in this path.
///
/// It lives across the whole segment: `pos` carries the fraction over a packet
/// boundary and `tail` keeps the frame before the buffer, so the interpolation
/// at the seam is the same one it would have been inside a buffer. A segment
/// starts one clean and drops it at the end, which is what makes a seek a
/// reset for free -- exactly as the equalizer's state does.
///
/// Time is no longer this struct's business: a clip whose own
/// [`Speed`](crate::project::Speed) is not real time is re-timed by the
/// [`TimeStretch`] behind it in its [`Rate`] chain, which compresses or
/// stretches the timeline while keeping every period of the signal its own
/// length -- this hands it a pitch-true stream at the timeline's rate and
/// that is the whole of the hand-off.
///
/// `owed` is what the timeline gives the segment, in frames per channel:
/// resampling resolves to whole samples and a clip's own window resolves to
/// whole *source* frames, so the two miss each other by a sample or two per
/// clip. Truncating and padding to what is owed keeps that from accumulating
/// into audible drift over a timeline of rate-mismatched cuts -- the picture's
/// clip boundaries are exact by construction, and this makes the sound's
/// exact too.
struct Resample {
    /// Source frames per output frame: the ratio of the two sample rates.
    step: f64,
    /// Where the next output frame reads, relative to the current buffer's
    /// first frame. Negative means it reads across the seam, into `tail`.
    pos: f64,
    /// The last frame of the buffer before this one, one sample per channel.
    tail: Vec<f32>,
    /// Output frames per channel still to be emitted for this segment, or
    /// [`Resample::UNBOUNDED`] for a window with no end of its own -- what
    /// [`AudioSession::open_at`] asks for, where the source running out is the
    /// end and there is no shortfall to pad.
    owed: u64,
}

impl Resample {
    /// "As many as the source has": a segment ending at `f64::INFINITY` owes the
    /// timeline nothing it did not decode.
    const UNBOUNDED: u64 = u64::MAX;

    fn new(step: f64, owed: u64) -> Self {
        Self {
            step,
            pos: 0.,
            tail: Vec::new(),
            owed,
        }
    }

    /// Rewrites `buf` in place as the frames this rate reads out of it. Never
    /// more than is owed, so the segment cannot overrun the room the timeline
    /// gave it.
    fn process(&mut self, buf: &mut Vec<f32>, channels: usize) {
        let frames = buf.len() / channels;
        if frames == 0 || channels == 0 {
            return;
        }
        let mut out: Vec<f32> = Vec::with_capacity(buf.len());
        while self.owed > (out.len() / channels) as u64 {
            let left = self.pos.floor();
            let right = left as i64 + 1;
            if right > frames as i64 - 1 {
                break; // the frame it would interpolate towards is not here yet
            }
            let frac = (self.pos - left) as f32;
            let (left, right) = (left as i64, right as usize);
            for c in 0..channels {
                // Before the buffer means the previous one's last frame; at the
                // very first buffer of a segment `pos` is 0, so this cannot be
                // asked for an empty tail.
                let a = match left < 0 {
                    true => self.tail.get(c).copied().unwrap_or(0.),
                    false => buf[left as usize * channels + c],
                };
                let b = buf[right * channels + c];
                out.push(a + (b - a) * frac);
            }
            self.pos += self.step;
        }
        self.tail.clear();
        self.tail.extend_from_slice(&buf[(frames - 1) * channels..]);
        self.pos -= frames as f64;
        if self.owed != Self::UNBOUNDED {
            self.owed -= (out.len() / channels) as u64;
        }
        *buf = out;
    }

    /// The silence the segment still owes when its source ran out before the
    /// timeline did -- a rounding frame or two, or a clip whose file is shorter
    /// than the edit says. `false` means the consumer went away.
    fn flush(&mut self, channels: usize, timeline: &mut u64, tx: &SyncSender<AudioChunk>) -> bool {
        if self.owed == Self::UNBOUNDED {
            return true; // nothing was promised, so nothing is short
        }
        while self.owed > 0 {
            let frames = self.owed.min(u64::from(SAMPLES_PER_PACKET));
            let chunk = AudioChunk {
                start_sample: *timeline,
                samples: vec![0.0; frames as usize * channels],
            };
            if tx.send(chunk).is_err() {
                return false;
            }
            *timeline += frames;
            self.owed -= frames;
        }
        true
    }
}

/// One speeded-or-converted segment's whole rate chain: the resample that
/// turns the source's own rate into the timeline's, and -- only for a clip
/// whose [`Speed`](crate::project::Speed) is not real time -- the stretch
/// that changes its *time* without touching its pitch, after the resample
/// and before the equalizer sees a sample (the filter's bands are set in the
/// frequency a listener hears, and after a stretch that is still the
/// frequency the file was written at).
///
/// Which half owns the segment's promise depends on which halves there are:
/// a rate conversion alone is paced by the resample, as it always was, and a
/// speeded clip's resample runs unbounded (the ratio is all it is asked for)
/// while the stretch lands the timeline's frames exactly.
struct Rate {
    resample: Resample,
    stretch: Option<TimeStretch>,
}

impl Rate {
    fn process(&mut self, buf: &mut Vec<f32>, channels: usize) {
        self.resample.process(buf, channels);
        if let Some(stretch) = self.stretch.as_mut() {
            stretch.process(buf, channels);
        }
    }

    fn flush(&mut self, channels: usize, timeline: &mut u64, tx: &SyncSender<AudioChunk>) -> bool {
        match self.stretch.as_mut() {
            Some(stretch) => stretch.flush(channels, timeline, tx),
            None => self.resample.flush(channels, timeline, tx),
        }
    }
}

struct Worker {
    /// Indexed by [`Segment::source`]; only the sources some segment names are
    /// `Some`, the rest were never opened.
    tracks: Vec<Option<Track>>,
    channels: usize,
    segments: Vec<Segment>,
    /// One per entry of `segments`, in the same order: the filter that segment's
    /// samples pass through on the way out, or `None` for one that plays flat.
    /// Beside the segments rather than inside them so the emit path can hold the
    /// window rules and the filter memory at once without splitting a borrow.
    eqs: Vec<Option<EqState>>,
    /// One per entry of `segments` again: that segment's rate chain, or
    /// `None` for one at real time on the timeline's own rate -- which is
    /// every segment of a project nobody has speeded or rate-mismatched, and
    /// the path that emits the decoder's own samples untouched.
    speeds: Vec<Option<Rate>>,
    /// One per entry of `segments` again: that segment's gain envelope, or
    /// `None` for one that plays flat -- [`emit`]'s last effect, run after
    /// the rate and the equalizer.
    fades: Vec<Option<crate::project::Fade>>,
    /// One per entry of `segments` again: the blocks a segment played backwards is
    /// decoded in ([`reverse_blocks`]), or `None` for one that plays forwards.
    reversed: Vec<Option<Vec<Segment>>>,
    /// `start_sample` of the first chunk.
    timeline: u64,
    tx: SyncSender<AudioChunk>,
}

/// Sums one worker per lane into one stream (see
/// [`AudioSession::open_mixed_streams`]). Every lane starts at the same timeline
/// position and synthesises its own gaps, so the lanes are sample-aligned by
/// construction and a mix is an add: no resampling, no positioning, no drift to
/// correct. What is emitted at each turn is the shortest block every still-live
/// lane can supply, which keeps a slow lane from being outrun and never buffers
/// more than the chunks already in flight.
///
/// A lane whose channel has closed has ended and simply stops being added --
/// that is what "shorter lane" means here. The mix ends when all of them have.
/// The accumulator is reused across turns; the one allocation per chunk is the
/// buffer the channel takes ownership of, and none of this is the RT path (the
/// device callback reads the ring the feeder has already filled).
/// The gains, ducks and limiter a mix starts from, carried as one value.
struct MixState {
    gains: Vec<f32>,
    ducks: Vec<Vec<Duck>>,
    limiter: Option<LimiterState>,
}

fn mix(
    rxs: &[Receiver<AudioChunk>],
    channels: usize,
    state: MixState,
    rate: u32,
    controls: Option<&MixControls>,
    tx: &SyncSender<AudioChunk>,
) {
    let MixState { mut gains, mut ducks, mut limiter } = state;
    // The version of the mix these gains and this limiter came from; see the
    // reload at the head of the loop.
    let mut seen = 0;
    let mut pending: Vec<Vec<f32>> = rxs.iter().map(|_| Vec::new()).collect();
    let mut live: Vec<bool> = rxs.iter().map(|_| true).collect();
    // Numbering follows lane 0, whose first chunk says where it starts.
    let mut base = 0;
    let mut emitted = 0;
    let mut out: Vec<f32> = Vec::new();
    // Per lane: the block after its gain and its duck, whether the lane had
    // samples in this block, and the key a duck reads. Kept between blocks so
    // the steady path allocates nothing beyond the chunk it sends.
    let mut scaled: Vec<Vec<f32>> = rxs.iter().map(|_| Vec::new()).collect();
    let mut filled: Vec<bool> = rxs.iter().map(|_| false).collect();
    // Per lane, per duck: the key that duck reads this block.
    let mut keys: Vec<Vec<Vec<f32>>> = rxs.iter().map(|_| Vec::new()).collect();
    // Per lane, one detector per duck, each with the key it reads: a lane ducked
    // under several keys is reduced by each of them in turn.
    let mut duckers: Vec<Vec<(usize, Ducker)>> = ducks
        .iter()
        .map(|lane| {
            lane.iter()
                .map(|d| (d.under, Ducker::new(d, rate)))
                .collect()
        })
        .collect();
    loop {
        // A mix moved while the stream plays, picked up between two blocks:
        // the levels change at a block boundary and nothing else changes at
        // all -- no flush, no re-open, no hole. The limiter is *retuned* rather
        // than rebuilt, so its delay line keeps running and the sample count
        // the master clock is made of does not move ([`LimiterState::retune`]).
        if let Some((next, next_ducks, params)) = controls.and_then(|c| c.changed(&mut seen)) {
            gains = next;
            gains.resize(rxs.len(), 1.0);
            ducks = next_ducks;
            ducks.resize(rxs.len(), Vec::new());
            // A duck that keeps its lane and its key keeps its detector and its
            // reduction; a new one starts at rest; one that was taken away is gone.
            for (slot, lane_ducks) in duckers.iter_mut().zip(&ducks) {
                let mut old = std::mem::take(slot);
                *slot = lane_ducks
                    .iter()
                    .map(|duck| match old.iter().position(|(under, _)| *under == duck.under) {
                        Some(at) => {
                            let (_, mut ducker) = old.swap_remove(at);
                            ducker.retune(duck, rate);
                            (duck.under, ducker)
                        }
                        None => (duck.under, Ducker::new(duck, rate)),
                    })
                    .collect();
            }
            match limiter.as_mut() {
                Some(state) => state.retune(&params),
                // First time it has ever been switched in on this stream: the
                // one that swallows nothing, for the count's sake.
                None if params.is_active() => {
                    limiter = Some(LimiterState::engaged(&params, rate, channels));
                }
                None => {}
            }
        }
        for (i, rx) in rxs.iter().enumerate() {
            while live[i] && pending[i].is_empty() {
                match rx.recv() {
                    Ok(chunk) => {
                        if i == 0 && emitted == 0 {
                            base = chunk.start_sample;
                        }
                        pending[i] = chunk.samples;
                    }
                    Err(_) => live[i] = false,
                }
            }
        }
        // The block every lane still playing can cover; `None` once they are all
        // done, which is the only way out of this loop besides a gone consumer.
        let Some(frames) = pending
            .iter()
            .filter(|p| !p.is_empty())
            .map(|p| p.len() / channels)
            .min()
            .filter(|&f| f > 0)
        else {
            // The mix has run out: the limiter still holds the stream's last frames, which go
            // out limited rather than being dropped (the export pads a shortfall with silence,
            // and that cut reads as an overshoot).
            if let Some(limiter) = limiter.as_mut() {
                let tail = limiter.drain();
                if !tail.is_empty() {
                    let samples = tail.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
                    let _ = tx.send(AudioChunk { start_sample: base + emitted, samples });
                }
            }
            return;
        };
        let block = frames * channels;
        // Each lane's own volume, applied on its way into the sum: this is the
        // one place every lane's samples pass through, so a lane that is turned
        // down is turned down in an export exactly as it is in the room. Unity
        // is a multiply by 1.0, which f32 leaves alone.
        for (i, lane) in pending.iter_mut().enumerate() {
            filled[i] = !lane.is_empty();
            if !filled[i] {
                continue;
            }
            let gain = gains.get(i).copied().unwrap_or(1.0);
            let dst = &mut scaled[i];
            dst.clear();
            dst.extend(lane.drain(..block).map(|sample| sample * gain));
        }
        // Auto-ducking. A ducked lane is reduced by each of its keys' signals as
        // they stood after gain and before any duck, so no duck keys off another
        // duck's output: every key is read first, then the ducks are applied in
        // turn. A key that had no samples this block is silence, and lets its
        // duck go. A duck naming its own lane, or none, does nothing.
        for i in 0..rxs.len() {
            keys[i].resize_with(duckers[i].len(), Vec::new);
            for (j, (under, _)) in duckers[i].iter().enumerate() {
                let under = *under;
                if under == i || under >= rxs.len() {
                    continue;
                }
                let key = &mut keys[i][j];
                key.clear();
                if filled[under] {
                    key.extend_from_slice(&scaled[under]);
                } else {
                    key.resize(block, 0.0);
                }
            }
        }
        for i in 0..rxs.len() {
            if !filled[i] {
                continue;
            }
            for (j, (under, ducker)) in duckers[i].iter_mut().enumerate() {
                if *under == i || *under >= rxs.len() {
                    continue;
                }
                ducker.process(&keys[i][j], &mut scaled[i], channels);
            }
        }
        out.clear();
        out.resize(block, 0.0);
        for (i, lane) in scaled.iter().enumerate() {
            if !filled[i] {
                continue;
            }
            let mut peak = 0.0f32;
            for (sum, &level) in out.iter_mut().zip(lane) {
                peak = peak.max(level.abs());
                *sum += level;
            }
            // Read by the level meters ([`MixControls::take_lane_peaks`]); the
            // sum above is the same arithmetic it always was.
            if let Some(controls) = controls {
                controls.note_lane_peak(i, peak);
            }
        }
        // ...and the master limiter over the sum, before the clamp: what the
        // clamp used to catch by squaring it off is held under the ceiling
        // here instead ([`crate::limiter`]). Off, this is not even a branch
        // per sample.
        if let Some(limiter) = limiter.as_mut() {
            limiter.process(&mut out);
        }
        let frames = out.len() / channels;
        if frames == 0 {
            continue; // the limiter's delay line is still filling
        }
        let chunk = AudioChunk {
            start_sample: base + emitted,
            // Clamped, not wrapped: two lanes at full scale are what a mix has
            // to survive, and the device takes `[-1, 1]`.
            samples: out.iter().map(|s| s.clamp(-1.0, 1.0)).collect(),
        };
        if tx.send(chunk).is_err() {
            return; // caller moved on
        }
        emitted += frames as u64;
    }
}

fn run(mut w: Worker) {
    let channels = w.channels;
    // Chunk numbering is continuous across every join, source ones included.
    let mut timeline = w.timeline;
    let segments = std::mem::take(&mut w.segments);
    let mut eqs = std::mem::take(&mut w.eqs);
    // A short list is "flat from here on" (`open_multi_streams_eq`), which this
    // pads out so the zip below still walks every segment.
    eqs.resize_with(segments.len(), || None);
    let mut speeds = std::mem::take(&mut w.speeds);
    // ...and a short rate list is "real time from here on", for the same reason.
    speeds.resize_with(segments.len(), || None);
    let mut fades = std::mem::take(&mut w.fades);
    // ...and a short fade list is "flat from here on", the same rule again.
    fades.resize_with(segments.len(), || None);
    let mut reversed = std::mem::take(&mut w.reversed);
    // ...and a short reversal list is "forwards from here on".
    reversed.resize_with(segments.len(), || None);

    for ((((seg, eq), speed), fade), backwards) in segments
        .iter()
        .zip(eqs.iter_mut())
        .zip(speeds.iter_mut())
        .zip(fades.iter_mut())
        .zip(reversed.iter())
    {
        let Some(source) = seg.source else {
            // A gap: hand the device real silence rather than nothing at all,
            // in the same packet-sized chunks decoding produces, so `fed` and
            // the master clock keep counting straight through the hole.
            let mut left = seg.media_end;
            while left > 0 {
                let frames = left.min(u64::from(SAMPLES_PER_PACKET));
                let chunk = AudioChunk {
                    start_sample: timeline,
                    samples: vec![0.0; frames as usize * channels],
                };
                if w.tx.send(chunk).is_err() {
                    return; // caller moved on
                }
                timeline += frames;
                left -= frames;
            }
            continue;
        };
        let Some(track) = w.tracks[source].as_mut() else {
            continue; // opener fills every named source; nothing to decode
        };
        let chain = Chain {
            eq: eq.as_mut(),
            speed: &mut *speed,
            fade: &mut *fade,
            channels,
            timeline: &mut timeline,
            tx: &w.tx,
        };
        let more = match backwards {
            Some(blocks) => run_backwards(track, blocks, chain),
            None => run_segment(track, seg, chain),
        };
        if !(more && settle(speed, channels, &mut timeline, &w.tx)) {
            return; // consumer went away
        }
    }
}

/// One segment of one source, decoded forwards through `chain`. `false` means the
/// consumer went away, or the decoder could not be built and the rest of the mix
/// is not worth decoding.
fn run_segment(track: &mut Track, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let track = match track {
        Track::Sym(track) => return run_sym(track, seg, Chain { eq, speed, fade, channels, timeline, tx }),
        Track::Ts(track) => return run_ts(track, seg, Chain { eq, speed, fade, channels, timeline, tx }),
        Track::Truehd(track) => {
            return run_mkv_truehd(track, seg, Chain { eq, speed, fade, channels, timeline, tx });
        }
        Track::Ext(track) => return run_ext(track, seg, Chain { eq, speed, fade, channels, timeline, tx }),
        Track::Mkv(track) => return run_mkv_ac3(track, seg, Chain { eq, speed, fade, channels, timeline, tx }),
        Track::Ac3(track) => return run_ac3(track, seg, Chain { eq, speed, fade, channels, timeline, tx }),
        Track::Aac(track) => track,
    };
    let mut interleaved = Vec::new();
    // An SBR track starts on a packet that carries its own header, which
    // may be ahead of the segment's pre-roll; its position is that packet's.
    let (start_id, mut pos) = if track.sbr {
        let id = sbr_start_packet(track, seg.start_id);
        let ts = packet_start_ts(&track.stts, id);
        (id, scale(ts, track.sample_rate, track.timescale).unwrap_or(0))
    } else {
        (seg.start_id, seg.start_pos)
    };
    let mut decoder = match aac_decoder(&track.params, track.source_channels()) {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return false;
        }
    };

    for id in start_id..=track.sample_count {
        if pos >= seg.media_end {
            break; // segment done, on to the next one
        }
        let sample = match mp4track::sample(&mut track.reader, track.stream, id) {
            Ok(Some(sample)) => sample,
            Ok(None) => break,
            Err(e) => {
                eprintln!("audio demux error at sample {id}: {e}");
                break;
            }
        };
        let packet = Packet::new(
            track.track_id,
            units::Timestamp::new(sample.dts.unwrap_or(0)),
            units::Duration::new(u64::from(sample.duration.unwrap_or(0).max(0) as u32)),
            &sample.data[..],
        );
        if let Err(e) = decoder.decode(&packet, &mut interleaved) {
            eprintln!("audio decode error at sample {id}: {e}");
            break;
        }
        // decoder.decode already folded to track.channels(); widen the mono
        // case up to the timeline's own width before dividing by it below.
        widen(&mut interleaved, usize::from(track.channels()), channels);
        let next = pos + (interleaved.len() / channels) as u64;
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed: &mut *speed,
                fade: &mut *fade,
                channels,
                timeline: &mut *timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false; // consumer went away
        }
        pos = next;
    }
    true
}

/// How long one block of a reversed segment is, in seconds of the source: the most
/// that is ever held decoded at once, whatever the clip's length -- a stereo pair at
/// 48 kHz is 750 KB of it.
const REVERSE_BLOCK_SECS: u64 = 2;

/// The blocks a reversed `seg` (the window `[media_target, media_end)` of `track`,
/// opened for the seconds from `start_secs`) is decoded in, in the order they are
/// *played*: the last stretch of the window first. Each is a segment of its own,
/// resolved against the packet table like any other, and trimmed to the exact
/// samples it owns so the blocks tile the window with nothing lost between.
fn reverse_blocks(track: &Track, source: usize, start_secs: f64, seg: &Segment) -> Vec<Segment> {
    let rate = u64::from(track.sample_rate().max(1));
    let step = REVERSE_BLOCK_SECS * rate;
    let mut blocks = Vec::new();
    let mut end = seg.media_end;
    while end > seg.media_target {
        let from = end.saturating_sub(step).max(seg.media_target);
        let (a, b) = (from - seg.media_target, end - seg.media_target);
        let mut block = track.segment(
            source,
            start_secs + a as f64 / rate as f64,
            start_secs + b as f64 / rate as f64,
        );
        // The seconds only find the packets; the samples are exact.
        block.media_target = from;
        block.media_end = end;
        blocks.push(block);
        end = from;
    }
    blocks
}

/// A reversed segment: each block decoded forwards into memory, turned end for end
/// and handed to the rate, the equalizer and the fade as if it were the decoder's
/// own -- so a reversed clip is filtered, speeded and faded exactly as a forward one,
/// from the sound's last sample down. `false` as for [`run_segment`].
fn run_backwards(track: &mut Track, blocks: &[Segment], chain: Chain<'_>) -> bool {
    let Chain {
        eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    // A block's chunks are collected here on the way out of the decoder: a block is
    // seconds of packets, never more than this many.
    let (hold, held) = sync_channel(1 << 14);
    let mut eq = eq;
    for block in blocks {
        let mut at = 0u64;
        let mut none = None;
        let mut flat = None;
        let ok = run_segment(
            track,
            block,
            Chain {
                eq: None,
                speed: &mut none,
                fade: &mut flat,
                channels,
                timeline: &mut at,
                tx: &hold,
            },
        );
        let mut samples: Vec<f32> = Vec::new();
        while let Ok(chunk) = held.try_recv() {
            samples.extend_from_slice(&chunk.samples);
        }
        // Frame order reversed, the channels of a frame left as they are.
        let frames = samples.len() / channels.max(1);
        for f in 0..frames / 2 {
            let (a, b) = (f * channels, (frames - 1 - f) * channels);
            for c in 0..channels {
                samples.swap(a + c, b + c);
            }
        }
        if !samples.is_empty()
            && !deliver(
                &mut samples,
                Chain {
                    eq: eq.as_deref_mut(),
                    speed: &mut *speed,
                    fade: &mut *fade,
                    channels,
                    timeline: &mut *timeline,
                    tx,
                },
            )
        {
            return false;
        }
        if !ok {
            return false;
        }
    }
    true
}

/// Pays out whatever silence a speeded segment still owes the timeline (see
/// [`Resample::flush`]). Nothing at all at real time, where a segment owes the
/// timeline only the samples it decoded.
fn settle(
    speed: &mut Option<Rate>,
    channels: usize,
    timeline: &mut u64,
    tx: &SyncSender<AudioChunk>,
) -> bool {
    match speed {
        Some(speed) => speed.flush(channels, timeline, tx),
        None => true,
    }
}

/// Trims one decoded buffer to the segment window and sends what survives.
/// `pos` is the media position of its first frame and `next` of the one after
/// it; both readers hand the same thing here, which is why the window rules
/// live in one place. `false` means the consumer went away.
///
/// Everything before the target is encoder priming and/or decoder pre-roll, not
/// audio the caller asked for: it is dropped, splitting the buffer that
/// straddles the target. The tail past the segment end goes too — on a short
/// segment that is this same buffer, trimmed at both ends.
///
/// The segment's equalizer runs here, on the trimmed buffer and nowhere else:
/// this is the one place both readers hand their samples over, so playback and
/// an audio export get the same filtered stream by construction rather than by
/// two call sites agreeing. Trimmed first, so the pre-roll a listener never
/// hears cannot ring the filter either. Nothing is allocated -- the device
/// callback is two hand-offs downstream of here in any case.
/// What one segment writes through: its equalizer, rate and fade, each carried
/// from one decoded buffer to the next, and the timeline and channel count the
/// samples land on. Passed whole so `emit` and the per-codec loops share one
/// shape instead of threading the same ten values through every call.
struct Chain<'a> {
    eq: Option<&'a mut EqState>,
    speed: &'a mut Option<Rate>,
    fade: &'a mut Option<crate::project::Fade>,
    channels: usize,
    timeline: &'a mut u64,
    tx: &'a SyncSender<AudioChunk>,
}

fn emit(interleaved: &mut Vec<f32>, seg: &Segment, chain: Chain<'_>, pos: u64, next: u64) -> bool {
    let Chain {
        eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    if next <= seg.media_target {
        return true;
    }
    let mut pos = pos;
    if pos < seg.media_target {
        interleaved.drain(..(seg.media_target - pos) as usize * channels);
        pos = seg.media_target;
    }
    if next > seg.media_end {
        interleaved.truncate((seg.media_end - pos) as usize * channels);
    }
    if interleaved.is_empty() {
        return true; // nothing left of it; the loop head ends the segment
    }
    deliver(
        interleaved,
        Chain {
            eq,
            speed,
            fade,
            channels,
            timeline,
            tx,
        },
    )
}

/// Sends the samples of one decoded, window-trimmed buffer on their way: through the
/// rate, the equalizer and the fade, in that order, and out as the next chunk.
/// `false` means the consumer went away.
fn deliver(interleaved: &mut Vec<f32>, chain: Chain<'_>) -> bool {
    let Chain {
        eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    // The rate first, the filter second: what the equalizer's bands are set in
    // is the frequency a listener hears, and after a resample that is not the
    // frequency the file was written at. A segment at real time has no
    // resampler at all and reaches the filter with the decoder's own samples.
    if let Some(speed) = speed {
        speed.process(interleaved, channels);
        if interleaved.is_empty() {
            return true; // this buffer resolved to no output frame of its own
        }
    }
    if let Some(eq) = eq {
        eq.process(interleaved);
    }
    // The fade last, on the same output-rate samples the mix will hear: its
    // frame counts are timeline frames ([`crate::project::Clip::fade_in`]),
    // which is what a resample has already turned these into. The state
    // walks forward with the segment across as many buffers as decoding
    // takes, exactly as the equalizer's does.
    if let Some(fade) = fade {
        fade.apply(interleaved, channels);
        fade.elapsed = fade
            .elapsed
            .saturating_add((interleaved.len() / channels) as u32);
    }
    let chunk = AudioChunk {
        start_sample: *timeline,
        samples: std::mem::take(interleaved),
    };
    *timeline += (chunk.samples.len() / channels) as u64;
    tx.send(chunk).is_ok()
}

/// One segment of an AC-3 track: the AAC loop with AC-3's decoder. Sample ids
/// come out of the same mp4 tables, the window rules are [`emit`]'s as always,
/// and what reaches the mixer is already the stereo downmix. `false` means the
/// consumer went away.
fn run_ac3(track: &mut Ac3Track, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let mut decoder = match ac3_decoder(track.requested) {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let mut pos = seg.start_pos;
    for id in seg.start_id..=track.sample_count {
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let sample = match mp4track::sample(&mut track.reader, track.stream, id) {
            Ok(Some(sample)) => sample,
            Ok(None) => return true,
            Err(e) => {
                eprintln!("audio demux error at sample {id}: {e}");
                return true;
            }
        };
        let (mut interleaved, samples) = match decode_ac3(&mut decoder, &sample.data) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => continue, // the decoder wants another frame first
            Err(e) => {
                eprintln!("audio decode error at sample {id}: {e}");
                return true;
            }
        };
        // §7.8.2 has already folded anything wider than a pair; only a mono
        // track arrives here in its own width, and [`widen`]'s is the last word.
        widen(&mut interleaved, usize::from(track.channels), channels);
        let next = pos + samples;
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed: &mut *speed,
                fade: &mut *fade,
                channels,
                timeline: &mut *timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false;
        }
        pos = next;
    }
    true
}

/// One segment of a Matroska AC-3 / E-AC-3 track: [`run_ac3`] reading blocks
/// instead of mp4 samples. The decoder, the window rules ([`emit`]) and the
/// stereo downmix are the same ones; only where a packet comes from differs.
/// `false` means the consumer went away.
fn run_mkv_ac3(track: &mut MkvAc3Track, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let mut decoder = match ac3_decoder(track.requested) {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let mut pos = seg.start_pos;
    for id in seg.start_id as usize.. {
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let block = match track.mkv.frame(id) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return true, // end of the track
            Err(e) => {
                eprintln!("audio demux error at block {id}: {e}");
                return true;
            }
        };
        let (mut interleaved, samples) = match decode_ac3(&mut decoder, &block) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => continue, // the decoder wants another frame first
            Err(e) => {
                eprintln!("audio decode error at block {id}: {e}");
                return true;
            }
        };
        // ...and widened here, as on the mp4 side.
        widen(&mut interleaved, usize::from(track.channels), channels);
        let next = pos + samples;
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed: &mut *speed,
                fade: &mut *fade,
                channels,
                timeline: &mut *timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false;
        }
        pos = next;
    }
    true
}

/// The Matroska TrueHD / MLP track: every block is one access unit, decoded by
/// `ec-truehd`, which is bit-exact against ffmpeg. The decoder hands back the
/// highest presentation the unit carries (7.1 where the stream has it, else 5.1,
/// else stereo), and [`downmix`] folds that to the pair the timeline carries, as
/// it does for every multichannel source. An Atmos track's 4th substream is
/// refused by `ec-truehd` by name, and so is the open.
struct MkvTruehdTrack {
    mkv: crate::demux::MkvAudio,
    sample_rate: u32,
    /// Samples per channel in every access unit. Constant across a stream, so a
    /// block's first sample is `id * au_samples` exactly, with no rounding from
    /// Matroska's millisecond timestamps.
    au_samples: u64,
    /// What the decoder emits: 2, 6 or 8 channels, before [`downmix`].
    source_channels: usize,
    /// What reaches the timeline: the source's width, or a pair for anything wider.
    channels: u16,
    total_samples: Option<u64>,
}

/// Blocks scanned for the first decodable access unit when a track is opened. A
/// decoder emits nothing until it has seen a restart header, which a stream
/// carries every few access units.
const TRUEHD_PROBE_BLOCKS: usize = 256;

/// Blocks decoded before a seek target so the decoder has seen its restart
/// header by the time the window starts (about 0.8 s at 48 kHz).
const TRUEHD_PRE_ROLL: u32 = 40;

impl MkvTruehdTrack {
    /// `Ok(None)` when the file's first audio track is not TrueHD or MLP.
    fn open(path: &Path) -> crate::Result<Option<Self>> {
        Self::from_blocks(crate::demux::MkvAudio::open(path)?)
    }

    /// [`open`](Self::open) on one named track ([`crate::demux::MkvAudioTrack::number`]).
    fn open_track(path: &Path, number: u64) -> crate::Result<Option<Self>> {
        Self::from_blocks(crate::demux::MkvAudio::open_track(path, number)?)
    }

    fn from_blocks(mkv: Option<crate::demux::MkvAudio>) -> crate::Result<Option<Self>> {
        let Some(mut mkv) = mkv else {
            return Ok(None);
        };
        if !matches!(mkv.codec, "truehd" | "mlp") {
            return Ok(None);
        }
        // Rate, layout and access-unit length all come out of the stream's own
        // major sync, as the decoder reads them: the first frame it can emit says
        // them. A first block the decoder cannot yet decode is not an error.
        let mut decoder = ec_truehd::TrueHdDecoder::new();
        let mut first = None;
        for id in 0..mkv.blocks().min(TRUEHD_PROBE_BLOCKS) {
            let Some(bytes) = mkv.frame(id)? else {
                break;
            };
            match decoder.decode_access_unit(&bytes) {
                Ok(Some(frame)) => {
                    first = Some(frame);
                    break;
                }
                Ok(None) | Err(ec_core::Error::NeedMore) => {}
                Err(e) => return Err(format!("TrueHD: {e}").into()),
            }
        }
        let frame = first.ok_or(
            "the TrueHD track of this Matroska file has no decodable access unit in its first blocks",
        )?;
        if frame.rate == 0 || frame.samples == 0 {
            return Err("the TrueHD track of this Matroska file states no sample rate".into());
        }
        let source_channels = frame.layout.channel_count();
        let au_samples = frame.samples as u64;
        Ok(Some(Self {
            sample_rate: frame.rate,
            au_samples,
            source_channels,
            channels: source_channels.min(2) as u16,
            total_samples: Some(mkv.blocks() as u64 * au_samples),
            mkv,
        }))
    }

    /// `secs` on this source's timeline into media samples. No priming: a
    /// Matroska file carries no edit list and TrueHD no encoder delay.
    fn media(&self, secs: f64) -> u64 {
        (secs.max(0.0) * f64::from(self.sample_rate)).round() as u64
    }

    /// One `[start, end)` window, resolved to blocks and media samples. The
    /// decoder is started [`TRUEHD_PRE_ROLL`] blocks early; what it decodes before
    /// the target is dropped by [`emit`].
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        let media_target = self.media(start_secs);
        let start_id = self.mkv.block_at(start_secs, TRUEHD_PRE_ROLL as usize);
        Segment {
            source: Some(source),
            start_id: start_id as u32,
            start_pos: start_id as u64 * self.au_samples,
            media_target,
            media_end: self.media(end_secs).max(media_target),
        }
    }
}

/// One segment of a Matroska TrueHD / MLP track: [`run_mkv_ac3`]'s shape, with
/// the access unit's position computed from its block index rather than summed,
/// so a decoded unit that is not emitted (the decoder still warming up) cannot
/// shift every sample after it.
fn run_mkv_truehd(track: &mut MkvTruehdTrack, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let mut decoder = ec_truehd::TrueHdDecoder::new();
    for id in seg.start_id as usize.. {
        let pos = id as u64 * track.au_samples;
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let block = match track.mkv.frame(id) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return true, // end of the track
            Err(e) => {
                eprintln!("audio demux error at block {id}: {e}");
                return true;
            }
        };
        let frame = match decoder.decode_access_unit(&block) {
            Ok(Some(frame)) => frame,
            // No output yet (no restart header seen): nothing to emit for this unit.
            Ok(None) | Err(ec_core::Error::NeedMore) => continue,
            Err(e) => {
                eprintln!("audio decode error at block {id}: {e}");
                return true;
            }
        };
        let mut interleaved = truehd_f32(&frame);
        downmix(&mut interleaved, track.source_channels);
        widen(&mut interleaved, usize::from(track.channels), channels);
        let next = pos + frame.samples as u64;
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed,
                fade,
                channels,
                timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false;
        }
    }
    true
}

/// One segment of a DTS / ADPCM / A-law / mu-law / AMR track read by `ec-probe`
/// ([`crate::extaudio`]): the reader is seeked a pre-roll ahead of the window, a
/// fresh decoder runs from there, and each packet lands at the media position its
/// timestamp states (summed from the last one when it states none). `false` means
/// the consumer went away.
fn run_ext(track: &mut crate::extaudio::ExtFileTrack, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let mut decoder = match track.decoder() {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let rate = f64::from(track.sample_rate.max(1));
    if let Err(e) = track.seek(seg.media_target as f64 / rate) {
        eprintln!("audio seek error: {e}");
        return true;
    }
    let mut next_pos = 0u64;
    loop {
        let (stated, block) = match track.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => return true, // end of the track
            Err(e) => {
                eprintln!("audio demux error: {e}");
                return true;
            }
        };
        let pos = stated.unwrap_or(next_pos);
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let (mut interleaved, source_channels) = match decoder.decode(&block) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => {
                next_pos = pos;
                continue;
            }
            Err(e) => {
                eprintln!("audio decode error: {e}");
                return true;
            }
        };
        let frames = (interleaved.len() / source_channels.max(1)) as u64;
        downmix(&mut interleaved, source_channels);
        widen(&mut interleaved, usize::from(track.channels()), channels);
        let next = pos + frames;
        next_pos = next;
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed,
                fade,
                channels,
                timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false;
        }
    }
}

/// The decoder's S32 output as f32 in `[-1, 1)`, interleaved. Its samples carry
/// 24 significant bits (left-justified in 32), so both the widening to f32 and
/// the division by 2^31 are exact.
pub(crate) fn truehd_f32(frame: &ec_core::frame::AudioFrame) -> Vec<f32> {
    frame
        .data
        .first()
        .into_iter()
        .flat_map(|plane| plane.as_chunks::<4>().0.iter())
        .map(|s| i32::from_ne_bytes(*s) as f32 / 2_147_483_648.0)
        .collect()
}

/// One segment of a standalone audio file: seek the reader to just before the
/// window, then decode forward until the window is filled.
///
/// The seek goes [`SYM_PRE_ROLL`] ahead of the target and everything before the
/// target is thrown away by [`emit`], which is what gives a codec with decoder
/// state — mp3's bit reservoir above all — something to warm up on, exactly as
/// the AAC path's two-packet pre-roll does. A reader that cannot seek at all
/// simply decodes from wherever it is; the window rules still hold, it is only
/// slower. `false` means the consumer went away.
/// One segment of a transport stream's sound: the track's own demuxer is sought a
/// pre-roll ahead of the window, a fresh decoder runs from there, and each packet lands at
/// the media position its timestamp states (the sound that starts after the picture leaves
/// silence in front of it; sound before the picture is trimmed away). `false` means the
/// consumer went away.
fn run_ts(track: &mut crate::demux::ts::TsAudio, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let rate = i128::from(track.sample_rate);
    let mut decoder = match track.decoder() {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let target_secs = seg.media_target as f64 / track.sample_rate as f64;
    if let Err(e) = track.seek((target_secs - track.pre_roll_secs()).max(0.0)) {
        eprintln!("audio demux seek failed: {e}");
        return true;
    }
    let mut at: Option<i128> = None;
    let mut first = true;
    loop {
        let (stamp, bytes) = match track.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return true, // end of the file, and of the segment
            Err(e) => {
                eprintln!("audio demux error: {e}");
                return true;
            }
        };
        // The count of samples decoded since the last stamp is the position, until a stamp
        // disagrees with it by more than a fiftieth of a second (a dropped packet, a splice).
        let pos = match at {
            Some(running) if (running - stamp).abs() <= rate / 50 => running,
            _ => stamp,
        };
        if pos >= seg.media_end as i128 {
            return true;
        }
        let decoded = match decoder.decode(&bytes) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => continue,
            Err(e) => {
                eprintln!("audio decode error at sample {pos}: {e}");
                return true;
            }
        };
        let mut interleaved = decoded.samples;
        let width = decoded.channels.max(1);
        at = Some(pos + i128::from(decoded.frames));
        downmix(&mut interleaved, width);
        widen(&mut interleaved, width.min(2), channels);
        let frames = (interleaved.len() / channels) as i128;
        // A packet that starts before the clip does: the frames in front of 0 are dropped.
        let (mut pos, mut frames) = (pos, frames);
        if pos < 0 {
            let cut = ((-pos).min(frames)) as usize;
            interleaved.drain(..cut * channels);
            pos += cut as i128;
            frames -= cut as i128;
        }
        if frames <= 0 {
            continue;
        }
        // The first sound of the run begins after the window does: the gap is silence,
        // so the sound keeps its offset from the picture.
        if first {
            first = false;
            let target = seg.media_target as i128;
            if pos > target {
                let gap = (pos.min(seg.media_end as i128) - target) as usize;
                let mut silence = vec![0.0f32; gap * channels];
                if !emit(
                    &mut silence,
                    seg,
                    Chain {
                        eq: eq.as_deref_mut(),
                        speed: &mut *speed,
                        fade: &mut *fade,
                        channels,
                        timeline: &mut *timeline,
                        tx,
                    },
                    target as u64,
                    pos.min(seg.media_end as i128) as u64,
                ) {
                    return false;
                }
            }
        }
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed: &mut *speed,
                fade: &mut *fade,
                channels,
                timeline: &mut *timeline,
                tx,
            },
            pos as u64,
            (pos + frames) as u64,
        ) {
            return false;
        }
    }
}

fn run_sym(track: &mut SymTrack, seg: &Segment, chain: Chain<'_>) -> bool {
    let Chain {
        mut eq,
        speed,
        fade,
        channels,
        timeline,
        tx,
    } = chain;
    let mut decoder = match track.decoder() {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    track.seek_to(seg.media_target);
    let mut interleaved = Vec::new();
    // Media position of the next frame to decode: the landing for the first
    // packet, and the decoder's own count from there on. A packet's timestamp
    // cannot carry it, because the frames of one laced block all share the
    // block's -- accumulating is what both mp4 readers here already do.
    let mut at: Option<u64> = None;
    // The withheld tail (see `SymTrack::tail_hold`) and the position of its first frame.
    let mut held: Vec<f32> = Vec::new();
    let mut held_pos = 0u64;
    loop {
        let packet = match track.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => return true, // end of the file, and of the segment
            Err(e) => {
                eprintln!("audio demux error: {e}");
                return true;
            }
        };
        if packet.track_id != track.track_id {
            continue; // another track of the same container
        }
        // A Matroska Vorbis packet's own start is exact ([`SymTrack::exact_start`]);
        // the running count and the stamp only answer where there is no index.
        let pos = track.exact_start(packet.pts).or(at).unwrap_or_else(|| match track.params.codec {
            // A Vorbis run from the top starts at the top. Matroska stamps its
            // blocks a few milliseconds past the samples they hold (ffmpeg
            // starts the same file at 0), so the first block's stamp is not the
            // start of the run.
            CODEC_ID_VORBIS if seg.media_target == 0 => 0,
            _ => track.samples_at(packet.pts),
        });
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        if let Err(e) = decoder.decode(&packet, &mut interleaved) {
            eprintln!("audio decode error at sample {pos}: {e}");
            return true;
        }
        // decoder.decode already folded to track.channels ([`SymDecoder::decode`],
        // [`downmix`]); widen the mono case up to the timeline's own width
        // before anything below divides by it.
        widen(&mut interleaved, usize::from(track.channels), channels);
        if interleaved.is_empty() {
            // A packet that answers nothing (a fresh decoder's first block
            // only primes the overlap) is not a position: the packet that
            // does answer carries its own timestamp, and counting on from this
            // one's is what put a seeked Vorbis file 1025 samples off its timeline.
            at = None;
            continue;
        }
        let next = pos + (interleaved.len() / channels) as u64;
        at = Some(next);
        // The last `tail_hold` frames of the track are not played: they wait
        // here, and whatever is still held when the file ends is dropped.
        let (pos, next) = if track.tail_hold > 0 {
            if held.is_empty() {
                held_pos = pos;
            }
            held.append(&mut interleaved);
            let keep = usize::try_from(track.tail_hold)
                .unwrap_or(usize::MAX)
                .saturating_mul(channels);
            let release = held.len().saturating_sub(keep);
            interleaved.extend(held.drain(..release));
            let released = (interleaved.len() / channels) as u64;
            let from = held_pos;
            held_pos += released;
            (from, from + released)
        } else {
            (pos, next)
        };
        // Reborrowed per packet: the filter memory has to carry across the
        // packet boundary, so this is one `EqState` for the whole segment.
        if !emit(
            &mut interleaved,
            seg,
            Chain {
                eq: eq.as_deref_mut(),
                speed: &mut *speed,
                fade: &mut *fade,
                channels,
                timeline: &mut *timeline,
                tx,
            },
            pos,
            next,
        ) {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MixControls, PRE_ROLL, SAMPLES_PER_PACKET, downmix, packet_at, packet_run, packet_run_last,
        vorbis_to_film_order,
    };
    use crate::limiter::Limiter;

    const C: f32 = std::f32::consts::FRAC_1_SQRT_2;

    /// The fold, channel by channel and width by width: each side keeps its own
    /// front channel, takes the centre and its own surrounds at -3 dB, drops the
    /// LFE, and comes out unable to clip a full-scale source. The channel order
    /// is FL, FR, FC, LFE, BL, BR, SL, SR.
    #[test]
    fn a_surround_frame_folds_to_stereo_without_clipping() {
        // Per width: what each channel at full scale, alone, must land as
        // *before* the normalisation that width carries.
        let layouts: [(usize, &[(f32, f32)]); 6] = [
            (3, &[(1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]),
            (4, &[(1.0, 0.0), (0.0, 1.0), (C, 0.0), (0.0, C)]),
            (5, &[(1.0, 0.0), (0.0, 1.0), (C, C), (C, 0.0), (0.0, C)]),
            (
                6,
                &[
                    (1.0, 0.0),
                    (0.0, 1.0),
                    (C, C),
                    (0.0, 0.0),
                    (C, 0.0),
                    (0.0, C),
                ],
            ),
            (
                7,
                &[
                    (1.0, 0.0),
                    (0.0, 1.0),
                    (C, C),
                    (0.0, 0.0),
                    (C, C),
                    (C, 0.0),
                    (0.0, C),
                ],
            ),
            (
                8,
                &[
                    (1.0, 0.0),
                    (0.0, 1.0),
                    (C, C),
                    (0.0, 0.0),
                    (C, 0.0),
                    (0.0, C),
                    (C, 0.0),
                    (0.0, C),
                ],
            ),
        ];
        for (from, want) in layouts {
            let (nl, nr) = want
                .iter()
                .fold((0.0f32, 0.0f32), |(l, r), c| (l + c.0, r + c.1));
            for (channel, &(left, right)) in want.iter().enumerate() {
                let mut frame = vec![0.0f32; from];
                frame[channel] = 1.0;
                downmix(&mut frame, from);
                assert_eq!(frame.len(), 2, "{from} ch: one frame is one stereo frame");
                assert!(
                    (frame[0] - left / nl).abs() < 1e-6 && (frame[1] - right / nr).abs() < 1e-6,
                    "{from} ch{channel}: {frame:?}, want {left}/{nl} {right}/{nr}"
                );
            }
            // Every channel at once is what a normalised fold exists for.
            let mut loud = vec![1.0f32; from * 2];
            downmix(&mut loud, from);
            assert_eq!(loud.len(), 4, "{from} ch: two frames out of two");
            assert!(
                loud.iter().all(|s| (*s - 1.0).abs() < 1e-6),
                "{from} ch: full scale in, full scale out, never past it: {loud:?}"
            );
        }
        // Mono and stereo are handed over untouched.
        for from in [1, 2] {
            let mut flat = vec![0.5f32, -0.5];
            downmix(&mut flat, from);
            assert_eq!(flat, [0.5, -0.5], "{from} ch is not a fold");
        }
    }

    /// The Vorbis order an Opus surround stream is stored in, into the film
    /// order the fold reads. Each channel is its own index, so a permutation
    /// that drifts is the value that comes out in the wrong slot.
    #[test]
    fn vorbis_order_becomes_film_order() {
        // (width, film slot -> the vorbis channel it must hold)
        for (from, want) in [
            (3usize, &[0, 2, 1][..]),
            (5, &[0, 2, 1, 3, 4]),
            (6, &[0, 2, 1, 5, 3, 4]),
            (7, &[0, 2, 1, 6, 5, 3, 4]),
            (8, &[0, 2, 1, 7, 5, 6, 3, 4]),
        ] {
            // Two frames, so a permutation that reads across the frame boundary
            // shows up as the wrong frame's value.
            let mut samples: Vec<f32> = (0..from * 2).map(|i| i as f32).collect();
            vorbis_to_film_order(&mut samples, from);
            let got: Vec<usize> = samples[..from].iter().map(|&s| s as usize).collect();
            assert_eq!(got, want, "{from} ch, first frame");
            let second: Vec<usize> = samples[from..].iter().map(|&s| s as usize - from).collect();
            assert_eq!(second, want, "{from} ch, second frame");
        }
        // 7.1's LFE is the last vorbis channel and the fourth film one.
        let mut lfe = vec![0.0f32; 8];
        lfe[7] = 1.0;
        vorbis_to_film_order(&mut lfe, 8);
        assert_eq!(
            lfe[3], 1.0,
            "the LFE lands where the fold drops it: {lfe:?}"
        );
    }

    /// A real AAC track: one stts entry, N packets of 1024.
    fn aac(count: u32) -> impl IntoIterator<Item = (u32, u32)> {
        [(count, 1024)]
    }

    #[test]
    fn packet_at_walks_back_by_the_pre_roll() {
        // Packet 1 covers [0, 1024); the target at 2 s is packet 87 (89088..).
        assert_eq!(packet_at(aac(200), 88_200, 0), (87, 88_064));
        assert_eq!(packet_at(aac(200), 88_200, PRE_ROLL), (85, 86_016));
        // Exactly on a packet boundary is the start of that packet, not the end
        // of the previous one.
        assert_eq!(packet_at(aac(200), 2048, 0), (3, 2048));
    }

    #[test]
    fn packet_at_clamps_both_ends() {
        // Head: no room for the walk-back, and packet ids are 1-based.
        assert_eq!(packet_at(aac(200), 0, PRE_ROLL), (1, 0));
        assert_eq!(packet_at(aac(200), 1500, PRE_ROLL), (1, 0));
        // Past the end: the last packet, so the worker decodes ~nothing.
        assert_eq!(packet_at(aac(200), 10_000_000, PRE_ROLL), (200, 203_776));
        // Degenerate boxes must not panic or wrap.
        assert_eq!(packet_at([], 5000, PRE_ROLL), (1, 0));
        assert_eq!(packet_at([(4u32, 0u32)], 5000, PRE_ROLL), (4, 0));
    }

    #[test]
    fn packet_at_crosses_stts_entries() {
        // 10 packets of 1024 then 10 of 512, so the second entry starts at 10240
        // with packet 11.
        let entries = [(10u32, 1024u32), (10, 512)];
        assert_eq!(packet_at(entries, 12_500, 0), (15, 12_288));
        assert_eq!(packet_at(entries, 12_500, PRE_ROLL), (13, 11_264));
        // The corner-cut clamp: on the first packet of the second entry the
        // walk-back stops there instead of stepping into the first entry.
        assert_eq!(packet_at(entries, 10_500, PRE_ROLL), (11, 10_240));
    }

    /// xorshift64: adversarial segment lists do not deserve a dependency.
    fn rng(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn packet_run_keeps_cumulative_error_under_a_packet() {
        let packet = i64::from(SAMPLES_PER_PACKET);
        let mut state = 0x5eed_1234_9876_abcd;
        for rate in [44100i64, 48000] {
            for list in 0..100 {
                let (mut err, mut asked, mut copied) = (0i64, 0i64, 0i64);
                // 3..43 joins of anything from one video frame to four seconds.
                for join in 0..3 + rng(&mut state) % 40 {
                    let ideal = rate / 30 + (rng(&mut state) % (4 * rate as u64)) as i64;
                    copied += i64::from(packet_run(&mut err, ideal, u32::MAX));
                    asked += ideal;
                    assert!(
                        err.abs() < packet,
                        "rate {rate} list {list} join {join}: err {err}"
                    );
                }
                // The accumulator is the whole story: what it says is the drift.
                assert_eq!(copied * packet - asked, err);
            }
        }
    }

    #[test]
    fn packet_run_rounds_against_the_running_debt() {
        // One second at 44100 is 43.07 packets: copying 43 leaves 68 samples
        // owed, and the debt is repaid by a 44-packet second once it crosses a
        // half packet — never by letting each second round on its own.
        let mut err = 0;
        assert_eq!(packet_run(&mut err, 44100, u32::MAX), 43);
        assert_eq!(err, -68);
        let mut seconds = 1;
        while packet_run(&mut err, 44100, u32::MAX) == 43 {
            seconds += 1;
            assert!(seconds < 20, "debt never repaid, err {err}");
        }
        assert_eq!(seconds, 7, "68 samples a second, half a packet is 512");
        assert!(err > 0, "the repaying second overshoots, err {err}");
    }

    #[test]
    fn the_last_run_rounds_up_so_the_copy_reaches_its_picture() {
        // test_sync.mp4: 4.0 s at 44100 is 172.27 packets. Nearest copies 172 and
        // leaves 6 ms unwritten; the last run takes the 173rd, which runs 16 ms
        // past the picture -- under a frame.
        let ideal = 176_400;
        let mut err = 0;
        assert_eq!(packet_run(&mut err, ideal, u32::MAX), 172);
        let mut err = 0;
        assert_eq!(packet_run_last(&mut err, ideal, u32::MAX), 173);
        assert_eq!(err, 173 * 1024 - ideal);
        // A whole number of packets is not rounded up, and the track's end caps it.
        let mut err = 0;
        assert_eq!(packet_run_last(&mut err, 1024 * 10, u32::MAX), 10);
        assert_eq!(packet_run_last(&mut err, 1024 * 10, 4), 4);
    }

    #[test]
    fn packet_run_forgives_a_debt_the_track_cannot_pay() {
        // Asking a second of a track with ten packets left: it delivers ten and
        // does not make the next segment copy the shortfall from elsewhere.
        let mut err = 0;
        assert_eq!(packet_run(&mut err, 44100, 10), 10);
        assert_eq!(err, 0);
        // Degenerate windows are empty, not negative.
        assert_eq!(packet_run(&mut err, 0, 10), 0);
        assert_eq!(packet_run(&mut err, i64::MAX, 7), 7);
    }

    #[test]
    fn asc_round_trips_aac_lc_44100_stereo() {
        // profile 2 (LC), freq_index 4 (44100), chan_conf 2 (stereo).
        let (profile, freq_index, chan_conf) = (2u8, 4u8, 2u8);
        let asc = [
            (profile << 3) | (freq_index >> 1),
            (freq_index << 7) | (chan_conf << 3),
        ];
        assert_eq!(asc, [0x12, 0x10]);
    }

    /// The one claim worth making about a hand-written bitstream: our own
    /// decoder turns it into exactly one packet of exactly zero.
    #[test]
    fn a_silent_packet_decodes_to_a_packet_of_zero() {
        use super::silent_packet;

        for (chan_conf, channels) in [(1u8, 1usize), (2, 2)] {
            // AAC-LC (profile 2), 44100 (freq_index 4), `chan_conf` channels.
            // `4 << 7` in a u8 is zero: freq_index 4 has no low bits to put in byte 1.
            let asc = [(2 << 3) | (4 >> 1), chan_conf << 3];
            let mut decoder = ec_aac::AacDecoder::with_config_bytes(&asc).expect("decoder");
            let bytes = silent_packet(chan_conf).expect("silence");
            let pcm = decoder.decode(&bytes, None).expect("decodes as AAC-LC");
            assert_eq!(pcm.channels as usize, channels);
            assert_eq!(pcm.samples.len(), SAMPLES_PER_PACKET as usize * channels);
            assert!(
                pcm.samples.iter().all(|&s| s == 0.0),
                "{chan_conf} ch is not silent"
            );
        }
        assert!(
            silent_packet(6).is_err(),
            "beyond stereo is refused, not faked"
        );
    }

    #[test]
    fn lane_peaks_keep_the_highest_level_until_read() {
        let controls = MixControls::new(vec![1.0, 1.0], Vec::new(), Limiter::default());
        controls.note_lane_peak(0, 0.25);
        controls.note_lane_peak(0, 0.5);
        controls.note_lane_peak(0, 0.125);
        controls.note_lane_peak(1, 0.75);
        controls.note_lane_peak(7, 1.0);
        assert_eq!(
            controls.take_lane_peaks(),
            vec![0.5, 0.75],
            "the highest level since the last read, per lane; a lane with no slot is ignored"
        );
        assert_eq!(
            controls.take_lane_peaks(),
            vec![0.0, 0.0],
            "a read starts the meters over"
        );
    }

    /// A stereo clip on a mono timeline is no longer refused: the export folds
    /// its pair to the one channel the timeline carries ([`super::widen`]), the
    /// same as playback does. The file comes back mono, and the stereo clip's
    /// span is audible in it -- silent there would mean the fold dropped it.
    #[test]
    fn a_mono_clip_then_a_stereo_clip_exports_stereo() {
        use crate::export::{ExportSettings, Format};
        use crate::project::{Clip, LaneKind, Project, Source, Speed};
        use crate::scale::FitPolicy;

        let assets = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let mono = assets.join("test_seek_chirp.mkv");
        let stereo = assets.join("test_av1_grain.mkv");
        let meta = crate::demux::Demuxer::open(&assets.join("test_av.mp4"))
            .expect("open test_av")
            .0;
        let clip = |start, source| Clip {
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start,
            in_frame: 0,
            out_frame: 30,
            source,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            transform: None,
            pitch: 0,
        };
        // One second of the mono source, then one second of the stereo one.
        let project = Project::from_parts(
            vec![Source::new(&mono, 0), Source::new(&stereo, 0)],
            vec![(LaneKind::Audio, vec![clip(0, 0), clip(30, 1)])],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .expect("a mono clip with a stereo clip after it");

        let out = crate::scratch::Scratch::file("ve_stereo_on_mono", "wav");
        let handle = crate::export::start(
            project,
            meta,
            &out,
            &ExportSettings {
                format: Format::Wav,
                ..Default::default()
            },
            None,
        );
        let started = std::time::Instant::now();
        while !handle.is_finished() {
            assert!(started.elapsed().as_secs() < 120, "export did not finish in 120s");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        handle
            .result()
            .expect("a finished export has an outcome")
            .expect("a mono clip then a stereo clip exports");

        let (audio, chunks) = crate::AudioSession::open(&out)
            .expect("reopen the export")
            .expect("the export has audio");
        let mut samples = Vec::new();
        for chunk in chunks {
            samples.extend(chunk.samples);
        }
        // The timeline is as wide as its widest clip: the mono clip is widened
        // into both channels, and the stereo one is kept rather than folded.
        assert_eq!(audio.channels, 2, "a stereo clip makes the timeline stereo");
        let rate = f64::from(audio.sample_rate);
        let rms = |from: f64, to: f64| {
            let at = |secs: f64| (secs * rate) as usize;
            let window = &samples[at(from).min(samples.len())..at(to).min(samples.len())];
            assert!(!window.is_empty(), "no samples in [{from}, {to})");
            (window.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / window.len() as f64)
                .sqrt()
        };
        let mono_span = rms(0.05, 0.95);
        let stereo_span = rms(1.05, 1.95);
        println!("rms: mono clip {mono_span:.4}  stereo clip {stereo_span:.4}");
        assert!(mono_span > 0.01, "the mono clip is audible");
        assert!(stereo_span > 0.01, "the stereo clip is audible on the stereo timeline");
        std::fs::remove_file(&out).ok();
    }
}

