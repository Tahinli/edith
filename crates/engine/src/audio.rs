//! Background audio decode worker: pulls a source's audio track and hands
//! interleaved f32 over a bounded channel, same shape as `decode`. Uses its own
//! reader so the video demuxer stays single-owner.
//!
//! Three readers, one output. An mp4 goes through `mp4`+`symphonia-codec-aac` as
//! it always has -- that path also yields the raw access units the export
//! copies, which is the only reason it is kept separate -- and a standalone
//! audio file (`crate::is_audio`: mp3, wav, flac, ogg, ALAC, ADTS) goes through
//! symphonia's own format probe. An mp4's **AC-3** track is the third: the same
//! sample tables, decoded by `oxideav-ac3` and downmixed to stereo by the
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
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use mp4::{AudioObjectType, ChannelConfig, MediaType, Mp4Reader, Mp4Track, TrackType};
use oxideav_core::{CodecId, CodecParameters, CodecRegistry, Decoder, Frame, Packet as Ac3Packet};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
};
use symphonia_core::formats::probe::Hint;
// `TrackType` is the name both readers give their track kinds; the mp4 one is
// imported above and used far more often here, so symphonia's is the one aliased.
use symphonia_core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo, TrackType as SymKind,
};
use symphonia_core::io::MediaSourceStream;
use symphonia_core::meta::MetadataOptions;
use symphonia_core::packet::Packet;
use symphonia_core::units;
use symphonia_core::units::{Time, TimeBase};

use crate::eq::{EqParams, EqState};

/// ffmpeg's AAC-LC encoder delay, used when the file carries no edit list.
/// (HE-AAC/iTunes files use 2112, but those are rejected as unsupported here.)
const DEFAULT_PRIMING: u64 = 1024;

/// An edit list entry that only shifts the timeline, carrying no media.
const EMPTY_EDIT: u64 = u32::MAX as u64;

/// Packets decoded and thrown away ahead of a seek target: one for the MDCT
/// overlap-add that reconstructs the target packet, one to warm the decoder.
const PRE_ROLL: u32 = 2;

/// Frames per channel one AAC-LC packet carries. Fixed by the codec.
const SAMPLES_PER_PACKET: u32 = 1024;

/// Frames per channel one AC-3 syncframe carries. Fixed by the codec (6 blocks
/// of 256), the way 1024 is fixed for AAC-LC.
const AC3_SAMPLES_PER_FRAME: u32 = 1536;

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
/// files may share a timeline only when their probes are equal (or both `None`).
///
/// Rate and layout only: those are what one output device and one exported track
/// can carry, and an mp3 that agrees on both may join a timeline of mp4s even
/// though nothing about it can be *copied* into an export -- that is a separate
/// refusal, at export time, in [`AudioSession::copy_multi_segments`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioProbe {
    pub sample_rate: u32,
    pub channels: u16,
}

/// One audio stream of a file as its header describes it. Every audio track is
/// described, decodable or not: a picker shows the ones we refuse greyed out
/// rather than pretending a BluRay remux has fewer streams than it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    /// Position among this file's audio tracks in file order — the `stream`
    /// [`AudioSession::open_multi_streams`] takes.
    pub index: usize,
    /// Container codec.
    ///
    /// ponytail: mp4 0.14 parses `mp4a` sample entries and silently drops every
    /// other kind (`stsd.rs` `_ => {}`), keeping no fourcc, so an AC-3/DTS/PCM
    /// stream can only be `"unknown"` here — enough to grey a row out, not
    /// enough to name the codec. Upgrade path is reading the sample entry's
    /// fourcc out of the file ourselves.
    pub codec: String,
    /// `0` for a stream whose sample entry mp4 0.14 does not parse.
    pub channels: u16,
    /// `0` for a stream whose sample entry mp4 0.14 does not parse.
    pub sample_rate: u32,
    /// ISO-639-2 from the mdhd, `None` for the `und` most muxers write.
    pub lang: Option<String>,
    /// Whether opening this stream would decode: AAC-LC, mono or stereo. The
    /// grey-out rule, mirroring the refusals `Track::open` and `Track::channels`
    /// make one file at a time.
    pub decodable: bool,
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
    /// Only the sources some segment names are opened. `Ok(None)` when `sources`
    /// is empty or its first entry has no AAC track; a later source that is
    /// missing audio or disagrees on rate/layout is an `Err` — import refuses
    /// those up front (one timeline, one output device), this is the backstop.
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
        let Some((path, stream)) = sources.first() else {
            return Ok(None);
        };
        let Some(first) = Track::open(path, *stream)? else {
            return Ok(None);
        };
        // The timeline's meta is the first source's: policy makes every other
        // source match it, and the checks below hold them to that.
        let meta = AudioMeta {
            sample_rate: first.sample_rate(),
            channels: first.channels()?,
            total_samples: first.total_samples(),
        };
        // Built here only so an undecodable stream is an `Err` from the opener
        // rather than a silently empty channel; the worker makes its own.
        first.check_decoder()?;

        let mut tracks: Vec<Option<Track>> = sources.iter().map(|_| None).collect();
        tracks[0] = Some(first);
        for &(source, ..) in segs {
            let Some(source) = source else {
                continue; // a gap opens no file
            };
            let slot = tracks
                .get_mut(source)
                .ok_or_else(|| format!("segment names source {source} of {}", sources.len()))?;
            if slot.is_some() {
                continue; // already opened, and the checks below already ran
            }
            let (path, stream) = &sources[source];
            let track = Track::open(path, *stream)?
                .ok_or_else(|| format!("source {source} has no audio track"))?;
            if (track.sample_rate(), track.channels()?) != (meta.sample_rate, meta.channels) {
                return Err(format!(
                    "source {source} is {} Hz {} ch, the timeline is {} Hz {} ch",
                    track.sample_rate(),
                    track.channels()?,
                    meta.sample_rate,
                    meta.channels
                )
                .into());
            }
            track.check_decoder()?;
            *slot = Some(track);
        }

        let segments: Vec<Segment> = segs
            .iter()
            .map(|&(source, start_secs, end_secs)| match source {
                Some(source) => tracks[source]
                    .as_ref()
                    .expect("opened above")
                    .segment(source, start_secs, end_secs),
                None => Segment::silence(
                    ((end_secs - start_secs).max(0.0) * f64::from(meta.sample_rate)) as u64,
                ),
            })
            .collect();
        // Chunk numbering is continuous across every join, counted from the first
        // segment's audible start in its own source (see `open_segments`).
        let timeline = segments.first().map_or(0, |s| match s.source {
            Some(source) => s
                .media_target
                .saturating_sub(tracks[source].as_ref().expect("opened above").priming()),
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

        let (tx, rx) = sync_channel(32);
        thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                run(Worker {
                    tracks,
                    channels: meta.channels as usize,
                    segments,
                    eqs,
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
        let [first, rest @ ..] = lanes else {
            return Ok(None);
        };
        if rest.is_empty() {
            let flat = Vec::new();
            return Self::open_multi_streams_eq(sources, first, eqs.first().unwrap_or(&flat));
        }
        let mut meta = None;
        let mut rxs = Vec::with_capacity(lanes.len());
        for (i, segs) in lanes.iter().enumerate() {
            // Every lane probes the same source 0, so the metas agree by
            // construction; `None` from any of them is a silent timeline.
            let flat = Vec::new();
            let Some((lane_meta, rx)) =
                Self::open_multi_streams_eq(sources, segs, eqs.get(i).unwrap_or(&flat))?
            else {
                return Ok(None);
            };
            meta = Some(lane_meta);
            rxs.push(rx);
        }
        let meta = meta.expect("at least two lanes opened above");
        let (tx, rx) = sync_channel(32);
        let channels = usize::from(meta.channels);
        thread::Builder::new()
            .name("audio-mix".into())
            .spawn(move || mix(&rxs, channels, &tx))?;
        Ok(Some((meta, rx)))
    }

    /// Why [`open`](Self::open) came back silent for a file that *has* sound:
    /// `Some(reason)` when there is an audio track and it is not the AAC-LC
    /// this decodes (an AC-3 remux, say). `None` when the file is simply silent
    /// -- nothing to tell the user there. Header only, and only worth calling
    /// once the open has already returned no audio.
    pub fn unsupported(path: impl AsRef<Path>) -> crate::Result<Option<String>> {
        // Matroska: the demuxer reads its picture and nothing else yet, so a
        // file that has sound is told so by name rather than playing silent
        // with no reason given. A Matroska file with no audio track at all
        // returns `None` here like any other silent file.
        if crate::demux::is_matroska(path.as_ref()) {
            return Ok(
                crate::demux::matroska_audio_codec(path.as_ref())?.map(|codec| {
                    format!("{codec} audio in a Matroska file is not wired to the decoder yet")
                }),
            );
        }
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        Ok(reader
            .tracks()
            .values()
            .find(|t| matches!(t.track_type(), Ok(TrackType::Audio)))
            .map(|t| match t.media_type() {
                Ok(media) => format!("the {media} audio track cannot be decoded (AAC-LC only)"),
                Err(_) => {
                    "the audio track is in a codec we cannot decode (AAC-LC only)".to_string()
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
            channels: track.channels()?,
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
        // A Matroska file's audio tracks are not readable here yet ([`Track`]),
        // and a stream nothing can open is not one to offer: an mkv lists as
        // the silent source it currently is.
        if crate::demux::is_matroska(path) {
            return Ok(Vec::new());
        }
        if crate::is_audio(path) {
            let track = SymTrack::open(path)?;
            return Ok(vec![StreamInfo {
                index: 0,
                codec: track.codec.into(),
                channels: track.channels,
                sample_rate: track.sample_rate,
                // Neither the mp3 nor the wav headers carry an ISO-639 tag the
                // way an mdhd does, and a lone track needs no telling apart.
                lang: None,
                decodable: matches!(track.channels, 1 | 2) && track.decoder().is_ok(),
            }]);
        }
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        Ok(audio_track_ids(&reader)
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let track = &reader.tracks()[&id];
                let aac = matches!(track.media_type(), Ok(MediaType::AAC));
                // An AC-3 track describes itself through its own reader: mp4
                // 0.14 parses no sample entry for it, so `channel_config` and
                // `sample_freq_index` have nothing to say. What is listed is
                // the *decoded* shape -- stereo, because that is what the §7.8
                // downmix hands the timeline (see [`Ac3Track`]).
                let ac3 = (!aac)
                    .then(|| Ac3Track::open(path, index).ok().flatten())
                    .flatten();
                let lang = track.language();
                StreamInfo {
                    index,
                    codec: match (aac, &ac3) {
                        (true, _) => "aac",
                        (_, Some(_)) => "ac-3",
                        _ => "unknown",
                    }
                    .into(),
                    channels: ac3
                        .as_ref()
                        .map_or_else(|| track.channel_config().map_or(0, channel_count), |t| {
                            t.channels
                        }),
                    sample_rate: ac3.as_ref().map_or_else(
                        || track.sample_freq_index().map_or(0, |f| f.freq()),
                        |t| t.sample_rate,
                    ),
                    lang: (!lang.is_empty() && lang != "und").then(|| lang.to_string()),
                    decodable: ac3.as_ref().is_some_and(|t| matches!(t.channels, 1 | 2))
                        || aac
                        && matches!(track.audio_profile(), Ok(AudioObjectType::AacLowComplexity))
                        && matches!(
                            track.channel_config(),
                            Ok(ChannelConfig::Mono | ChannelConfig::Stereo)
                        ),
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
        let path = path.as_ref();
        let Some(track) = Track::open(path, 0)? else {
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
    /// straight into an mp4 writer, which is the only way to keep audio in an
    /// export at all (no pure-Rust AAC encoder exists).
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
    /// ponytail: the first packet after an *interior* join decodes without its
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
    /// Every source must declare the same [`AacTrackParams`]: the copy becomes
    /// one output track with one esds. Import refuses a mismatch up front; the
    /// check here is the backstop, as is the missing-track `Err`.
    ///
    /// A segment naming **no** source is a gap, and is copied as that many
    /// packets of [`silent_packet`] silence — the rounding debt carried through
    /// it like any other, so the hole occupies its exact duration and the audio
    /// after it stays in sync with the picture. A gap the track *opens* on gets
    /// one silent packet more, which is the priming a reader drops.
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
        let Some(&(Some(first), ..)) = segs.iter().find(|s| s.0.is_some()) else {
            return Ok(None); // no segments, or nothing but silence
        };
        let (path, stream) = sources
            .get(first)
            .ok_or_else(|| format!("segment names source {first} of {}", sources.len()))?;
        let Some(track) = copy_track(path, *stream)? else {
            return Ok(None); // silent source, silent export
        };
        let params = track.track_params();
        let mut tracks: Vec<Option<AacTrack>> = sources.iter().map(|_| None).collect();
        tracks[first] = Some(track);

        let sample_rate = params.sample_rate;
        let mut err = 0i64;
        let mut packets: Vec<AacPacket> = Vec::new();
        for &(source, start_secs, end_secs) in segs {
            let Some(source) = source else {
                let ideal = ((end_secs - start_secs).max(0.0) * f64::from(sample_rate)) as i64;
                let bytes = silent_packet(params.chan_conf)?;
                // A reader drops the first packet of an AAC track as the
                // encoder's priming. When the track opens on a hole, that has to
                // come out of one *extra* packet of silence: dropped out of the
                // hole itself it would shorten it, and everything after the hole
                // would play a packet early. It is not part of the run, so it
                // owes the rounding debt nothing.
                let priming = u32::from(packets.is_empty());
                for _ in 0..packet_run(&mut err, ideal, u32::MAX) + priming {
                    packets.push(AacPacket {
                        bytes: bytes.clone(),
                        samples: SAMPLES_PER_PACKET,
                    });
                }
                continue;
            };
            let track = source_at(&mut tracks, sources, source)?;
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
            let ids = head
                .into_iter()
                .chain(start_id..start_id + packet_run(&mut err, ideal, available));
            for id in ids {
                let Some(sample) = track.reader.read_sample(track.track_id, id)? else {
                    let count = track.sample_count;
                    return Err(format!("audio sample {id} of {count} is missing").into());
                };
                packets.push(AacPacket {
                    bytes: sample.bytes.to_vec(),
                    samples: SAMPLES_PER_PACKET,
                });
            }
        }
        Ok(Some((params, packets)))
    }
}

/// One AAC-LC access unit of silence, for the gaps in an exported timeline —
/// the export copies packets and has no AAC encoder, so the only silence it can
/// write is one spelled out by hand.
///
/// It is the shortest legal `raw_data_block` there is: one element carrying
/// `max_sfb = 0`, which is "no scalefactor bands", which is a frame with no
/// spectral data at all and therefore 1024 samples of exact zero. Every field
/// is zero except the element id and the terminating `ID_END`, so the whole
/// thing is 4 bytes mono and 7 stereo. Verified against our own decoder in the
/// unit tests, which is the only claim worth making about hand-written
/// bitstream.
///
/// ponytail: mono and stereo only, because `chan_conf` beyond 2 needs more
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

/// The source at `index`, on the stream it names, opened on first use. Sources
/// a segment list never names are never touched: a project's list only grows.
fn source_at<'a>(
    tracks: &'a mut [Option<AacTrack>],
    sources: &[(PathBuf, usize)],
    index: usize,
) -> crate::Result<&'a mut AacTrack> {
    let slot = tracks
        .get_mut(index)
        .ok_or_else(|| format!("segment names source {index} of {}", sources.len()))?;
    if slot.is_none() {
        let (path, stream) = &sources[index];
        *slot = Some(
            copy_track(path, *stream)?
                .ok_or_else(|| format!("source {index} has no audio track"))?,
        );
    }
    Ok(slot.as_mut().expect("just opened"))
}

/// A source's track for the *copy* path, which needs raw AAC access units.
///
/// An mp3, a wav, a flac — anything the export cannot copy — is refused here by
/// name and by format rather than exported silent or, worse, exported as noise:
/// there is no AAC encoder in this project (see [`AudioSession::copy_segments`]),
/// so a timeline carrying one of those simply cannot become an mp4 today. The
/// wording is what a front-end shows.
///
/// `Ok(None)` for a file with no audio at all, which is a silent export.
fn copy_track(path: &Path, stream: usize) -> crate::Result<Option<AacTrack>> {
    match Track::open(path, stream)? {
        None => Ok(None),
        Some(Track::Aac(track)) => Ok(Some(track)),
        Some(Track::Sym(track)) => Err(uncopyable(path, track.codec)),
        // Decoded, never copied: an AC-3 syncframe is not something an `mp4a`
        // sample table can hold, and there is no AAC encoder here to turn it
        // into one. A WAV/FLAC export of the same timeline decodes fine.
        Some(Track::Ac3(_)) => Err(uncopyable(path, "AC-3")),
    }
}

/// The one wording for "this source's audio cannot be copied into an mp4",
/// shared by every format that cannot be: a front-end shows it verbatim.
fn uncopyable(path: &Path, codec: &str) -> crate::Error {
    format!(
        "export needs AAC audio today; {} is {codec}",
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
    Sym(SymTrack),
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
        // A Matroska file's sound is not wired to either reader yet: its picture
        // is what this slice delivers, and the source counts as a silent one --
        // which `AudioSession::unsupported` puts into words for the user.
        if crate::demux::is_matroska(path) {
            return Ok(None);
        }
        let audio_file = crate::is_audio(path);
        // Ahead of the AAC reader, because that one answers "not AAC" for a
        // *named* stream with an `Err` and an AC-3 track is not an error any
        // more. `Ok(None)` here means "not an AC-3 track", so every other file
        // falls through to exactly the reader it always used.
        if !audio_file
            && let Some(track) = Ac3Track::open(path, stream)?
        {
            return Ok(Some(Self::Ac3(track)));
        }
        match AacTrack::open(path, stream) {
            Ok(Some(track)) => return Ok(Some(Self::Aac(track))),
            Ok(None) if !audio_file => return Ok(None),
            Err(e) if !audio_file => return Err(e),
            _ => {}
        }
        if stream > 0 {
            return Err(format!("{}: audio stream {stream} of 1 stream", path.display()).into());
        }
        SymTrack::open(path).map(|track| Some(Self::Sym(track)))
    }

    fn sample_rate(&self) -> u32 {
        match self {
            Self::Aac(t) => t.sample_rate,
            Self::Ac3(t) => t.sample_rate,
            Self::Sym(t) => t.sample_rate,
        }
    }

    /// Mono or stereo; anything wider is refused here rather than one packet at
    /// a time, because one output device and one copied track is all there is.
    fn channels(&self) -> crate::Result<u16> {
        match self {
            Self::Aac(t) => t.channels(),
            // Already downmixed: `channels` is what comes *out* of the decoder,
            // which is 2 for everything from mono to 5.1 (see [`Ac3Track`]).
            Self::Ac3(t) => match t.channels {
                1 | 2 => Ok(t.channels),
                n => Err(format!("unsupported channel layout: {n} channels (max stereo)").into()),
            },
            Self::Sym(t) => match t.channels {
                1 | 2 => Ok(t.channels),
                n => Err(format!("unsupported channel layout: {n} channels (max stereo)").into()),
            },
        }
    }

    /// Frames per channel of *audible* audio, priming already subtracted.
    fn total_samples(&self) -> Option<u64> {
        match self {
            Self::Aac(t) => t.total_samples,
            Self::Ac3(t) => t.total_samples,
            Self::Sym(t) => t.total_samples,
        }
    }

    /// Encoder delay of this file. Sources are joined, priming is not shared.
    fn priming(&self) -> u64 {
        match self {
            Self::Aac(t) => t.priming,
            Self::Ac3(t) => t.priming,
            Self::Sym(t) => t.priming,
        }
    }

    /// One `[start, end)` window of this source's seconds, resolved to whatever
    /// its own reader needs to find it again.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        match self {
            Self::Aac(t) => t.segment(source, start_secs, end_secs),
            Self::Ac3(t) => t.segment(source, start_secs, end_secs),
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
                AacDecoder::try_new(&t.params, &AudioDecoderOptions::default())?;
            }
            Self::Ac3(t) => {
                ac3_decoder(t.requested)?;
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
    track_id: u32,
    sample_rate: u32,
    channels: u16,
    /// Frames per channel of audible audio, `None` when the header does not say.
    total_samples: Option<u64>,
    /// Encoder delay: the same role [`AacTrack::priming`] plays, taken from the
    /// container where it declares one (mp3's Xing/LAME tag, ALAC's edit list).
    priming: u64,
    time_base: TimeBase,
    params: AudioCodecParameters,
    /// The format's short name, for the export refusal. `&'static` because it
    /// comes out of the codec registry, which outlives everything here.
    codec: &'static str,
}

impl SymTrack {
    fn open(path: &Path) -> crate::Result<Self> {
        let mss = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
        // The extension is a hint, not a decision: the probe reads the magic and
        // is free to disagree, which is what makes a mislabelled file work.
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let reader = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;
        let track = reader
            .default_track(SymKind::Audio)
            .ok_or_else(|| format!("{} has no audio track", path.display()))?;
        let (track_id, num_frames, delay) = (track.id, track.num_frames, track.delay);
        let time_base = track
            .time_base
            .ok_or_else(|| format!("{} declares no time base", path.display()))?;
        let Some(symphonia_core::codecs::CodecParameters::Audio(params)) =
            track.codec_params.clone()
        else {
            return Err(format!("{} declares no audio codec", path.display()).into());
        };
        let sample_rate = params
            .sample_rate
            .ok_or_else(|| format!("{} declares no sample rate", path.display()))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .ok_or_else(|| format!("{} declares no channel layout", path.display()))?;
        let codec = symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .map_or("an unsupported format", |d| d.codec.info.short_name);
        Ok(Self {
            reader,
            track_id,
            sample_rate,
            channels,
            total_samples: num_frames,
            priming: u64::from(delay.unwrap_or(0)),
            time_base,
            params,
            codec,
        })
    }

    /// A fresh decoder: seeking mid-stream leaves state that belongs to the
    /// packets we skipped, exactly as on the AAC path.
    fn decoder(&self) -> crate::Result<Box<dyn AudioDecoder>> {
        Ok(symphonia::default::get_codecs()
            .make_audio_decoder(&self.params, &AudioDecoderOptions::default())?)
    }

    /// `secs` on this source's audible timeline into media samples (priming
    /// included, so it compares directly against a decoded position).
    fn media(&self, secs: f64) -> u64 {
        ((secs * f64::from(self.sample_rate)) as u64).saturating_add(self.priming)
    }

    /// A packet timestamp in the track's time base, as samples per channel.
    fn samples_at(&self, ts: symphonia_core::units::Timestamp) -> u64 {
        let secs = self.time_base.calc_time_saturating(ts).as_secs_f64();
        (secs.max(0.0) * f64::from(self.sample_rate)) as u64
    }
}

/// One source's AC-3 track: the same mp4 sample tables the AAC track is read
/// from, decoded through `oxideav-ac3` and **downmixed to stereo by the decoder
/// itself** (ATSC A/52 §7.8, `channels: Some(2)`), because one output device and
/// one timeline layout is all there is. A 5.1 BluRay track therefore arrives
/// here as an ordinary stereo source, and its rows in the picker say so.
///
/// `channels` is what the decoder actually emitted for the first frame rather
/// than an assumed 2: everything from mono to 5.1 downmixes to stereo, but a 2.1
/// stream is a passthrough the library leaves at 3, and that is refused by name
/// in [`Track::channels`] instead of being mislabelled.
struct Ac3Track {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    sample_rate: u32,
    channels: u16,
    timescale: u32,
    priming: u64,
    total_samples: Option<u64>,
    stts: Vec<(u32, u32)>,
    /// What the decoder is asked to hand out, from this track's own layout:
    /// `Some(2)` for anything with more than one front channel, `None` — the
    /// library's passthrough — for a mono track. See [`ac3_decoder`].
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
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        let ids = audio_track_ids(&reader);
        let Some(&track_id) = ids.get(stream) else {
            return Ok(None);
        };
        if !matches!(crate::demux::sample_entry(path, track_id), Ok((kind, _)) if &kind == b"ac-3")
        {
            return Ok(None);
        }
        let track = &reader.tracks()[&track_id];
        let timescale = track.timescale();
        let duration = track.trak.mdia.mdhd.duration;
        let edit = edit_media_time(track);
        let stts: Vec<(u32, u32)> = stts_pairs(track).collect();
        let sample_count = reader.sample_count(track_id)?;

        let first = reader
            .read_sample(track_id, 1)?
            .ok_or("the AC-3 track has no samples")?;
        let sample_rate = oxideav_ac3::syncinfo::parse(&first.bytes)
            .map_err(|e| format!("not a readable AC-3 syncframe: {e:?}"))?
            .sample_rate;
        let nfchans = oxideav_ac3::bsi::parse(first.bytes.get(5..).unwrap_or_default())
            .map_err(|e| format!("not a readable AC-3 bit stream information: {e:?}"))?
            .nfchans;
        let requested = (nfchans > 1).then_some(2);
        let mut decoder = ac3_decoder(requested)?;
        let channels = decode_ac3(&mut decoder, &first.bytes)?
            .map(|pcm| (pcm.len() / AC3_SAMPLES_PER_FRAME as usize) as u16)
            .filter(|&c| c > 0)
            .ok_or("the first AC-3 syncframe decoded to nothing")?;

        // No encoder delay in AC-3; a remux that writes an edit list is still
        // honoured, exactly as the AAC track honours it.
        let priming = edit
            .and_then(|t| scale(t, sample_rate, timescale))
            .unwrap_or(0);
        Ok(Some(Self {
            track_id,
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
        ((secs * f64::from(self.sample_rate)) as u64).saturating_add(self.priming)
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

/// A fresh AC-3 decoder handing out `channels`: `Some(2)` is the library's own
/// A/52 §7.8 stereo downmix, which is the whole reason the timeline can carry a
/// 5.1 track at all. Fresh per segment for the same reason every other decoder
/// here is: a seek leaves overlap-add state belonging to the frames we skipped.
///
/// ponytail: `Some(2)` on a **mono** (`acmod` 1/0) source decodes to digital
/// silence in oxideav-ac3 0.0.10 — measured, 5.1 and stereo are correct — so a
/// mono track asks for no downmix at all and stays the mono source it is. The
/// upgrade path is `Some(2)` unconditionally once the library duplicates the
/// centre channel; the caller ([`Ac3Track::open`]) is the only thing to change.
fn ac3_decoder(channels: Option<u16>) -> crate::Result<Box<dyn Decoder>> {
    let mut registry = CodecRegistry::new();
    oxideav_ac3::register_codecs(&mut registry);
    let mut params = CodecParameters::audio(CodecId::new("ac3"));
    params.channels = channels;
    registry
        .first_decoder(&params)
        .map_err(|e| format!("no AC-3 decoder: {e:?}").into())
}

/// One syncframe in, one buffer of interleaved f32 out. `Ok(None)` when the
/// decoder wants more input before it can hand a frame back, which is not an
/// error. The library speaks S16 little-endian; `/32768` is the whole
/// conversion, and it lands in the `[-1, 1)` every other reader here emits.
fn decode_ac3(decoder: &mut Box<dyn Decoder>, bytes: &[u8]) -> crate::Result<Option<Vec<f32>>> {
    let packet = Ac3Packet::new(0, oxideav_core::TimeBase::new(1, 48_000), bytes.to_vec());
    decoder
        .send_packet(&packet)
        .map_err(|e| format!("AC-3 decode failed: {e:?}"))?;
    match decoder.receive_frame() {
        Ok(Frame::Audio(audio)) => Ok(Some(
            audio.data[0]
                .chunks_exact(2)
                .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
                .collect(),
        )),
        Ok(other) => Err(format!("AC-3 decoder handed back {other:?}").into()),
        Err(oxideav_core::Error::NeedMore) => Ok(None),
        Err(e) => Err(format!("AC-3 decode failed: {e:?}").into()),
    }
}

/// One source's AAC track, resolved once: the reader plus everything the header
/// has to say about it. `stts` is copied out because every other field would
/// otherwise keep a borrow alive on the reader the worker needs mutably — a real
/// AAC track has one entry, so it is two `u32`s.
struct AacTrack {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    sample_rate: u32,
    timescale: u32,
    /// Encoder delay of *this* file. Sources are joined, priming is not shared.
    priming: u64,
    total_samples: Option<u64>,
    stts: Vec<(u32, u32)>,
    config: ChannelConfig,
    freq_index: u8,
    chan_conf: u8,
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
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let ids = audio_track_ids(&reader);
        let Some(track) = ids.get(stream).map(|id| &reader.tracks()[id]) else {
            return match stream {
                0 => Ok(None),
                n => Err(format!(
                    "{}: audio stream {n} of {} streams",
                    path.display(),
                    ids.len()
                )
                .into()),
            };
        };
        if !matches!(track.media_type(), Ok(MediaType::AAC)) {
            return match stream {
                0 => Ok(None),
                n => Err(format!("{}: audio stream {n} is not AAC", path.display()).into()),
            };
        }
        // Refuse early with a message instead of failing packet by packet — and
        // a copy carries no profile of its own: the writer rebuilds the
        // AudioSpecificConfig from `freq_index`/`chan_conf` and calls it LC, so a
        // non-LC source would be mislabelled rather than merely unplayable.
        match track.audio_profile()? {
            AudioObjectType::AacLowComplexity => {}
            other => return Err(format!("unsupported AAC profile: {other:?} (only AAC-LC)").into()),
        }

        let track_id = track.track_id();
        let sample_rate = track.sample_freq_index()?.freq();
        let timescale = track.timescale();
        let priming = priming_samples(track, sample_rate);
        let (_, freq_index, chan_conf) = asc_fields(track)?;
        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_extra_data(audio_specific_config(track)?);
        let this = Self {
            track_id,
            sample_count: reader.sample_count(track_id)?,
            sample_rate,
            timescale,
            priming,
            total_samples: scale(track.trak.mdia.mdhd.duration, sample_rate, timescale)
                .map(|d| d.saturating_sub(priming)),
            stts: stts_pairs(track).collect(),
            config: track.channel_config()?,
            freq_index,
            chan_conf,
            params,
            reader,
        };
        Ok(Some(this))
    }

    /// Channel count. `AacDecoder` only does mono and stereo, so anything else
    /// is refused here rather than one packet at a time.
    fn channels(&self) -> crate::Result<u16> {
        match self.config {
            ChannelConfig::Mono => Ok(1),
            ChannelConfig::Stereo => Ok(2),
            other => Err(format!("unsupported channel layout: {other:?} (max stereo)").into()),
        }
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
        ((secs * f64::from(self.sample_rate)) as u64).saturating_add(self.priming)
    }

    /// One `[start, end)` window of this source's seconds, resolved to its own
    /// packets and media samples.
    fn segment(&self, source: usize, start_secs: f64, end_secs: f64) -> Segment {
        let media_target = self.media(start_secs);
        let target_ts = unscale(media_target, self.sample_rate, self.timescale);
        // Two packets of pre-roll: AAC-LC's MDCT overlap-add needs the previous
        // packet to cancel aliasing, plus one more to warm the decoder up. Their
        // output is discarded below, so this only costs decode time.
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

/// This file's audio tracks in file order — the order a stream index counts in.
/// It comes out of `moov.traks` and not `Mp4Reader::tracks`, which is a
/// `HashMap`: iterating that would make "stream 0" a different track from one
/// run to the next on any file carrying more than one.
fn audio_track_ids<R>(reader: &Mp4Reader<R>) -> Vec<u32> {
    reader
        .moov
        .traks
        .iter()
        .filter(|trak| {
            matches!(
                TrackType::try_from(&trak.mdia.hdlr.handler_type),
                Ok(TrackType::Audio)
            )
        })
        .map(|trak| trak.tkhd.track_id)
        .collect()
}

/// Channels in an AAC channel configuration. The config number *is* the channel
/// count up to 5.1; 7.1 is the one that is not.
fn channel_count(config: ChannelConfig) -> u16 {
    match config {
        ChannelConfig::SevenOne => 8,
        other => other as u16,
    }
}

/// `(profile, freq_index, chan_conf)` out of the esds descriptor. Returned as a
/// tuple because mp4 0.14's `DecoderSpecificDescriptor` is unnameable outside
/// the crate (same trap as `SttsEntry`, see [`packet_at`]).
fn asc_fields(track: &Mp4Track) -> crate::Result<(u8, u8, u8)> {
    let esds = track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .mp4a
        .as_ref()
        .and_then(|mp4a| mp4a.esds.as_ref())
        .ok_or("AAC track has no esds descriptor")?;
    let cfg = &esds.es_desc.dec_config.dec_specific;
    Ok((cfg.profile, cfg.freq_index, cfg.chan_conf))
}

/// The 2-byte AAC-LC AudioSpecificConfig, rebuilt from the esds fields; mp4 0.14
/// parses those out and drops the raw bytes, so this is the exact inverse of its
/// writer (`mp4box/mp4a.rs` `DecoderSpecificDescriptor::write_box`).
fn audio_specific_config(track: &Mp4Track) -> crate::Result<Box<[u8]>> {
    let (profile, freq_index, chan_conf) = asc_fields(track)?;
    Ok(Box::new([
        (profile << 3) | (freq_index >> 1),
        (freq_index << 7) | (chan_conf << 3),
    ]))
}

/// The stts entries as the `(sample_count, sample_delta)` pairs [`packet_at`]
/// walks; the box's own entry type is `pub(crate)` in mp4 0.14. Shared with
/// `demux`, which walks a video trak's table with the same two helpers.
pub(crate) fn stts_pairs(track: &Mp4Track) -> impl Iterator<Item = (u32, u32)> + '_ {
    track
        .trak
        .mdia
        .minf
        .stbl
        .stts
        .entries
        .iter()
        .map(|e| (e.sample_count, e.sample_delta))
}

/// Encoder delay in samples-per-channel: the edit list's first real entry says
/// how far into the media the presentation starts.
///
/// ponytail: an explicit `media_time` of 0 is taken at face value (no trim).
/// Muxers that write a zero edit list despite a primed stream would leak the
/// priming; upgrade path is preferring the codec delay when it disagrees.
fn priming_samples(track: &Mp4Track, sample_rate: u32) -> u64 {
    edit_media_time(track)
        .and_then(|t| scale(t, sample_rate, track.timescale()))
        .unwrap_or(DEFAULT_PRIMING)
}

/// `media_time` of the edit list's first real entry, in the track's own
/// timescale: where the presentation starts inside the media. `None` when there
/// is no edit list, or only empty entries -- those shift the timeline rather
/// than trim the media, and neither track honours that shift (see
/// [`priming_samples`]'s ponytail). Shared with `demux`, which owes the video
/// track the same trim.
pub(crate) fn edit_media_time(track: &Mp4Track) -> Option<u64> {
    track
        .trak
        .edts
        .as_ref()
        .and_then(|edts| edts.elst.as_ref())
        .and_then(|elst| {
            elst.entries
                .iter()
                .map(|e| e.media_time)
                .find(|&t| t != EMPTY_EDIT && t != u64::MAX)
        })
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
/// ponytail: the walk-back is clamped to the start of the containing stts entry
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
fn mix(rxs: &[Receiver<AudioChunk>], channels: usize, tx: &SyncSender<AudioChunk>) {
    let mut pending: Vec<Vec<f32>> = rxs.iter().map(|_| Vec::new()).collect();
    let mut live: Vec<bool> = rxs.iter().map(|_| true).collect();
    // Numbering follows lane 0, whose first chunk says where it starts.
    let mut base = 0;
    let mut emitted = 0;
    let mut out: Vec<f32> = Vec::new();
    loop {
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
            return;
        };
        let block = frames * channels;
        out.clear();
        out.resize(block, 0.0);
        for lane in pending.iter_mut().filter(|p| !p.is_empty()) {
            for (sum, sample) in out.iter_mut().zip(lane.drain(..block)) {
                *sum += sample;
            }
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
    let mut interleaved = Vec::new();
    let channels = w.channels;
    // Chunk numbering is continuous across every join, source ones included.
    let mut timeline = w.timeline;
    let segments = std::mem::take(&mut w.segments);
    let mut eqs = std::mem::take(&mut w.eqs);
    // A short list is "flat from here on" (`open_multi_streams_eq`), which this
    // pads out so the zip below still walks every segment.
    eqs.resize_with(segments.len(), || None);

    for (seg, eq) in segments.iter().zip(eqs.iter_mut()) {
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
        let track = match track {
            Track::Sym(track) => {
                if !run_sym(track, seg, eq.as_mut(), channels, &mut timeline, &w.tx) {
                    return; // consumer went away
                }
                continue;
            }
            Track::Ac3(track) => {
                if !run_ac3(track, seg, eq.as_mut(), channels, &mut timeline, &w.tx) {
                    return; // consumer went away
                }
                continue;
            }
            Track::Aac(track) => track,
        };
        let mut decoder = match AacDecoder::try_new(&track.params, &AudioDecoderOptions::default())
        {
            Ok(decoder) => decoder,
            Err(e) => {
                eprintln!("audio decoder init failed: {e}");
                return;
            }
        };
        let mut pos = seg.start_pos;

        for id in seg.start_id..=track.sample_count {
            if pos >= seg.media_end {
                break; // segment done, on to the next one
            }
            let sample = match track.reader.read_sample(track.track_id, id) {
                Ok(Some(sample)) => sample,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("audio demux error at sample {id}: {e}");
                    break;
                }
            };
            let packet = Packet::new(
                track.track_id,
                units::Timestamp::new(sample.start_time as i64),
                units::Duration::new(u64::from(sample.duration)),
                &sample.bytes[..],
            );
            let buf = match decoder.decode(&packet) {
                Ok(buf) => buf,
                Err(e) => {
                    eprintln!("audio decode error at sample {id}: {e}");
                    break;
                }
            };
            buf.copy_to_vec_interleaved::<f32>(&mut interleaved);
            let next = pos + (interleaved.len() / channels) as u64;
            if !emit(
                &mut interleaved,
                channels,
                seg,
                eq.as_mut(),
                pos,
                next,
                &mut timeline,
                &w.tx,
            ) {
                return; // consumer went away
            }
            pos = next;
        }
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
fn emit(
    interleaved: &mut Vec<f32>,
    channels: usize,
    seg: &Segment,
    eq: Option<&mut EqState>,
    pos: u64,
    next: u64,
    timeline: &mut u64,
    tx: &SyncSender<AudioChunk>,
) -> bool {
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
    if let Some(eq) = eq {
        eq.process(interleaved);
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
fn run_ac3(
    track: &mut Ac3Track,
    seg: &Segment,
    mut eq: Option<&mut EqState>,
    channels: usize,
    timeline: &mut u64,
    tx: &SyncSender<AudioChunk>,
) -> bool {
    let mut decoder = match ac3_decoder(track.requested) {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let mut pos = seg.start_pos;
    for id in seg.start_id..=track.sample_count {
        let mut interleaved;
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let sample = match track.reader.read_sample(track.track_id, id) {
            Ok(Some(sample)) => sample,
            Ok(None) => return true,
            Err(e) => {
                eprintln!("audio demux error at sample {id}: {e}");
                return true;
            }
        };
        match decode_ac3(&mut decoder, &sample.bytes) {
            Ok(Some(pcm)) => interleaved = pcm,
            Ok(None) => continue, // the decoder wants another frame first
            Err(e) => {
                eprintln!("audio decode error at sample {id}: {e}");
                return true;
            }
        }
        let next = pos + (interleaved.len() / channels) as u64;
        if !emit(
            &mut interleaved,
            channels,
            seg,
            eq.as_deref_mut(),
            pos,
            next,
            timeline,
            tx,
        ) {
            return false;
        }
        pos = next;
    }
    true
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
fn run_sym(
    track: &mut SymTrack,
    seg: &Segment,
    mut eq: Option<&mut EqState>,
    channels: usize,
    timeline: &mut u64,
    tx: &SyncSender<AudioChunk>,
) -> bool {
    let mut decoder = match track.decoder() {
        Ok(decoder) => decoder,
        Err(e) => {
            eprintln!("audio decoder init failed: {e}");
            return true;
        }
    };
    let rate = f64::from(track.sample_rate.max(1));
    let from = seg.media_target.saturating_sub(SYM_PRE_ROLL) as f64 / rate;
    if let Some(time) = Time::try_from_secs_f64(from) {
        let to = SeekTo::Time {
            time,
            track_id: Some(track.track_id),
        };
        // A failed seek is not fatal: it leaves the reader where it was, and
        // the window rules below still only emit what the segment asked for.
        if let Err(e) = track.reader.seek(SeekMode::Accurate, to) {
            eprintln!("audio seek to {from:.3}s failed: {e}");
        }
    }
    let mut interleaved = Vec::new();
    loop {
        let packet = match track.reader.next_packet() {
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
        let pos = track.samples_at(packet.pts);
        if pos >= seg.media_end {
            return true; // segment done, on to the next one
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(e) => {
                eprintln!("audio decode error at sample {pos}: {e}");
                return true;
            }
        };
        buf.copy_to_vec_interleaved::<f32>(&mut interleaved);
        let next = pos + (interleaved.len() / channels) as u64;
        // Reborrowed per packet: the filter memory has to carry across the
        // packet boundary, so this is one `EqState` for the whole segment.
        if !emit(
            &mut interleaved,
            channels,
            seg,
            eq.as_deref_mut(),
            pos,
            next,
            timeline,
            tx,
        ) {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PRE_ROLL, SAMPLES_PER_PACKET, packet_at, packet_run};

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
        // The ponytail clamp: on the first packet of the second entry the
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
        use super::{AacDecoder, AudioDecoder, AudioDecoderOptions, silent_packet};
        use symphonia_core::codecs::audio::{AudioCodecParameters, well_known::CODEC_ID_AAC};
        use symphonia_core::{packet::Packet, units};

        for (chan_conf, channels) in [(1u8, 1usize), (2, 2)] {
            // AAC-LC (profile 2), 44100 (freq_index 4), `chan_conf` channels.
            let asc: Box<[u8]> = Box::new([(2 << 3) | (4 >> 1), (4 << 7) | (chan_conf << 3)]);
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(CODEC_ID_AAC)
                .with_sample_rate(44100)
                .with_extra_data(asc);
            let mut decoder =
                AacDecoder::try_new(&params, &AudioDecoderOptions::default()).expect("decoder");
            let bytes = silent_packet(chan_conf).expect("silence");
            let packet = Packet::new(
                0,
                units::Timestamp::new(0),
                units::Duration::new(u64::from(SAMPLES_PER_PACKET)),
                &bytes[..],
            );
            let buf = decoder.decode(&packet).expect("decodes as AAC-LC");
            let mut out = Vec::new();
            buf.copy_to_vec_interleaved::<f32>(&mut out);
            assert_eq!(out.len(), SAMPLES_PER_PACKET as usize * channels);
            assert!(
                out.iter().all(|&s| s == 0.0),
                "{chan_conf} ch is not silent"
            );
        }
        assert!(
            silent_packet(6).is_err(),
            "beyond stereo is refused, not faked"
        );
    }
}
