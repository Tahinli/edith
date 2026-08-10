//! Background audio decode worker: pulls the AAC track out of an MP4 and hands
//! interleaved f32 over a bounded channel, same shape as `decode`. Uses its own
//! `Mp4Reader` so the video demuxer stays single-owner.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use mp4::{AudioObjectType, ChannelConfig, MediaType, Mp4Reader, Mp4Track, TrackType};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
};
use symphonia_core::packet::Packet;
use symphonia_core::units;

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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioProbe {
    pub params: AacTrackParams,
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
        let Some((path, stream)) = sources.first() else {
            return Ok(None);
        };
        let Some(first) = Track::open(path, *stream)? else {
            return Ok(None);
        };
        // The timeline's meta is the first source's: policy makes every other
        // source match it, and the checks below hold them to that.
        let meta = AudioMeta {
            sample_rate: first.sample_rate,
            channels: first.channels()?,
            total_samples: first.total_samples,
        };
        // Built here only so an undecodable stream is an `Err` from the opener
        // rather than a silently empty channel; the worker makes its own.
        AacDecoder::try_new(&first.params, &AudioDecoderOptions::default())?;

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
            if (track.sample_rate, track.channels()?) != (meta.sample_rate, meta.channels) {
                return Err(format!(
                    "source {source} is {} Hz {} ch, the timeline is {} Hz {} ch",
                    track.sample_rate,
                    track.channels()?,
                    meta.sample_rate,
                    meta.channels
                )
                .into());
            }
            AacDecoder::try_new(&track.params, &AudioDecoderOptions::default())?;
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
                .saturating_sub(tracks[source].as_ref().expect("opened above").priming),
            None => 0,
        });

        // One AAC packet decodes to 1024 frames; at stereo f32 that is 8 KB, so
        // this bound is ~0.75 s of lookahead — enough to ride out decode jitter
        // without making a pause take a second to bite.
        let (tx, rx) = sync_channel(32);
        thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                run(Worker {
                    tracks,
                    channels: meta.channels as usize,
                    segments,
                    timeline,
                    tx,
                })
            })?;
        Ok(Some((meta, rx)))
    }

    /// Why [`open`](Self::open) came back silent for a file that *has* sound:
    /// `Some(reason)` when there is an audio track and it is not the AAC-LC
    /// this decodes (an AC-3 remux, say). `None` when the file is simply silent
    /// -- nothing to tell the user there. Header only, and only worth calling
    /// once the open has already returned no audio.
    pub fn unsupported(path: impl AsRef<Path>) -> crate::Result<Option<String>> {
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        Ok(reader
            .tracks()
            .values()
            .find(|t| matches!(t.track_type(), Ok(TrackType::Audio)))
            .map(|t| match t.media_type() {
                Ok(media) => format!("the {media} audio track cannot be decoded (AAC-LC only)"),
                Err(_) => "the audio track is in a codec we cannot decode (AAC-LC only)".to_string(),
            }))
    }

    /// The audio parameters of `path`, for checking an import against the
    /// timeline's first source: header only, no decoder and no worker.
    /// `Ok(None)` means no AAC track — an audio-less file, which import pairs
    /// only with other audio-less files.
    pub fn probe(path: impl AsRef<Path>) -> crate::Result<Option<AudioProbe>> {
        let Some(track) = Track::open(path.as_ref(), 0)? else {
            return Ok(None);
        };
        Ok(Some(AudioProbe {
            params: track.track_params(),
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
    pub fn probe_streams(path: impl AsRef<Path>) -> crate::Result<Vec<StreamInfo>> {
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;
        Ok(audio_track_ids(&reader)
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                let track = &reader.tracks()[&id];
                let aac = matches!(track.media_type(), Ok(MediaType::AAC));
                let lang = track.language();
                StreamInfo {
                    index,
                    codec: if aac { "aac" } else { "unknown" }.into(),
                    channels: track.channel_config().map_or(0, channel_count),
                    sample_rate: track.sample_freq_index().map_or(0, |f| f.freq()),
                    lang: (!lang.is_empty() && lang != "und").then(|| lang.to_string()),
                    decodable: aac
                        && matches!(track.audio_profile(), Ok(AudioObjectType::AacLowComplexity))
                        && matches!(
                            track.channel_config(),
                            Ok(ChannelConfig::Mono | ChannelConfig::Stereo)
                        ),
                }
            })
            .collect())
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
        let Some(&(Some(first), ..)) = segs.iter().find(|s| s.0.is_some()) else {
            return Ok(None); // no segments, or nothing but silence
        };
        let path = sources
            .get(first)
            .ok_or_else(|| format!("segment names source {first} of {}", sources.len()))?;
        let Some(track) = Track::open(path, 0)? else {
            return Ok(None); // silent source, silent export
        };
        let params = track.track_params();
        let mut tracks: Vec<Option<Track>> = sources.iter().map(|_| None).collect();
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

/// The source at `index`, opened on first use. Sources a segment list never
/// names are never touched: a project's list only grows.
///
/// ponytail: the copy path is still stream 0 of every source — an export of a
/// timeline playing stream 1 would carry stream 0's audio. Upgrade path is the
/// `(PathBuf, usize)` list [`AudioSession::open_multi_streams`] already takes,
/// which is S4b's job once a stream can be picked at all.
fn source_at<'a>(
    tracks: &'a mut [Option<Track>],
    sources: &[PathBuf],
    index: usize,
) -> crate::Result<&'a mut Track> {
    let slot = tracks
        .get_mut(index)
        .ok_or_else(|| format!("segment names source {index} of {}", sources.len()))?;
    if slot.is_none() {
        *slot = Some(
            Track::open(&sources[index], 0)?
                .ok_or_else(|| format!("source {index} has no audio track"))?,
        );
    }
    Ok(slot.as_mut().expect("just opened"))
}

/// One source's AAC track, resolved once: the reader plus everything the header
/// has to say about it. `stts` is copied out because every other field would
/// otherwise keep a borrow alive on the reader the worker needs mutably — a real
/// AAC track has one entry, so it is two `u32`s.
struct Track {
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

impl Track {
    /// The `stream`-th audio track of `path`, counted in file order over *all*
    /// audio tracks — the numbering [`AudioSession::probe_streams`] hands out.
    ///
    /// Stream 0 is best effort: `Ok(None)` when there is no audio there at all
    /// or the codec is one we do not decode, which is a valid silent source (a
    /// file whose audio is AC-3 still has a picture worth editing). A stream the
    /// caller *named* is a promise instead, so out of range or undecodable
    /// there is an `Err` rather than silence the caller did not ask for.
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
    /// `start_sample` of the first chunk.
    timeline: u64,
    tx: SyncSender<AudioChunk>,
}

fn run(mut w: Worker) {
    let mut interleaved = Vec::new();
    let channels = w.channels;
    // Chunk numbering is continuous across every join, source ones included.
    let mut timeline = w.timeline;
    let segments = std::mem::take(&mut w.segments);

    for seg in &segments {
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

            // Everything before the target is encoder priming and/or decoder
            // pre-roll, not audio the caller asked for: drop it, splitting the
            // packet that straddles the target.
            if next <= seg.media_target {
                pos = next;
                continue;
            }
            if pos < seg.media_target {
                interleaved.drain(..(seg.media_target - pos) as usize * channels);
                pos = seg.media_target;
            }
            // And the tail past the segment end goes too — on a short segment
            // that is this same buffer, trimmed at both ends.
            if next > seg.media_end {
                interleaved.truncate((seg.media_end - pos) as usize * channels);
            }
            pos = next;
            if interleaved.is_empty() {
                continue; // nothing left of it; the loop head ends the segment
            }
            let chunk = AudioChunk {
                start_sample: timeline,
                samples: std::mem::take(&mut interleaved),
            };
            timeline += (chunk.samples.len() / channels) as u64;
            if w.tx.send(chunk).is_err() {
                return; // consumer went away
            }
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
