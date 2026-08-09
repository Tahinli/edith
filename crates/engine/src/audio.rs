//! Background audio decode worker: pulls the AAC track out of an MP4 and hands
//! interleaved f32 over a bounded channel, same shape as `decode`. Uses its own
//! `Mp4Reader` so the video demuxer stays single-owner.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use mp4::{AudioObjectType, ChannelConfig, MediaType, Mp4Reader, Mp4Track};
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
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let Some(track) = reader
            .tracks()
            .values()
            .find(|t| matches!(t.media_type(), Ok(MediaType::AAC)))
        else {
            return Ok(None);
        };

        // AacDecoder only does plain LC, mono/stereo; refuse early with a
        // message instead of letting it fail packet by packet.
        match track.audio_profile()? {
            AudioObjectType::AacLowComplexity => {}
            other => return Err(format!("unsupported AAC profile: {other:?} (only AAC-LC)").into()),
        }
        let channels: u16 = match track.channel_config()? {
            ChannelConfig::Mono => 1,
            ChannelConfig::Stereo => 2,
            other => {
                return Err(format!("unsupported channel layout: {other:?} (max stereo)").into());
            }
        };

        let track_id = track.track_id();
        let sample_rate = track.sample_freq_index()?.freq();
        let priming = priming_samples(track, sample_rate);
        let total_samples = scale(
            track.trak.mdia.mdhd.duration,
            sample_rate,
            track.timescale(),
        )
        .map(|d| d.saturating_sub(priming));

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_extra_data(audio_specific_config(track)?);
        // Built here only so an undecodable stream is an `Err` from the opener
        // rather than a silently empty channel; the worker makes its own.
        AacDecoder::try_new(&params, &AudioDecoderOptions::default())?;

        // Segment bounds in media samples (priming included, so they compare
        // directly against the decoded position). `f64::INFINITY` saturates.
        let media = |secs: f64| ((secs * f64::from(sample_rate)) as u64).saturating_add(priming);
        let segments: Vec<Segment> = segs
            .iter()
            .map(|&(start_secs, end_secs)| {
                let media_target = media(start_secs);
                let target_ts = unscale(media_target, sample_rate, track.timescale());
                // Two packets of pre-roll: AAC-LC's MDCT overlap-add needs the
                // previous packet to cancel aliasing, plus one more to warm the
                // decoder up. Their output is discarded below, so this only
                // costs decode time.
                let (start_id, start_ts) = packet_at(stts_pairs(track), target_ts, PRE_ROLL);
                Segment {
                    start_id,
                    start_pos: scale(start_ts, sample_rate, track.timescale()).unwrap_or(0),
                    media_target,
                    // An inverted segment is an empty one, never a backwards run.
                    media_end: media(end_secs).max(media_target),
                }
            })
            .collect();

        let sample_count = reader.sample_count(track_id)?;
        let meta = AudioMeta {
            sample_rate,
            channels,
            total_samples,
        };
        // One AAC packet decodes to 1024 frames; at stereo f32 that is 8 KB, so
        // this bound is ~0.75 s of lookahead — enough to ride out decode jitter
        // without making a pause take a second to bite.
        let (tx, rx) = sync_channel(32);
        thread::Builder::new()
            .name("audio-decode".into())
            .spawn(move || {
                run(Worker {
                    reader,
                    params,
                    track_id,
                    sample_count,
                    channels: channels as usize,
                    priming,
                    segments,
                    tx,
                })
            })?;
        Ok(Some((meta, rx)))
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
        if segs.is_empty() {
            return Ok(None);
        }
        let file = File::open(path.as_ref())?;
        let size = file.metadata()?.len();
        let mut reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let Some(track) = reader
            .tracks()
            .values()
            .find(|t| matches!(t.media_type(), Ok(MediaType::AAC)))
        else {
            return Ok(None);
        };
        // The copy carries no profile of its own: the writer rebuilds the
        // AudioSpecificConfig from `freq_index`/`chan_conf` and calls it LC, so a
        // non-LC source would be mislabelled rather than merely unplayable here.
        match track.audio_profile()? {
            AudioObjectType::AacLowComplexity => {}
            other => return Err(format!("unsupported AAC profile: {other:?} (only AAC-LC)").into()),
        }

        let track_id = track.track_id();
        let sample_rate = track.sample_freq_index()?.freq();
        let timescale = track.timescale();
        let priming = priming_samples(track, sample_rate);
        let (_, freq_index, chan_conf) = asc_fields(track)?;
        let sample_count = reader.sample_count(track_id)?;

        // Resolve every packet id first: `read_sample` needs the reader mutably,
        // which ends the borrow `track` holds.
        let media = |secs: f64| ((secs * f64::from(sample_rate)) as u64).saturating_add(priming);
        let mut err = 0i64;
        let mut ids: Vec<u32> = Vec::new();
        for (i, &(start_secs, end_secs)) in segs.iter().enumerate() {
            let target_ts = unscale(media(start_secs), sample_rate, timescale);
            let (start_id, _) = packet_at(stts_pairs(track), target_ts, 0);
            let ideal = ((end_secs - start_secs).max(0.0) * f64::from(sample_rate)) as i64;
            let available = sample_count.saturating_sub(start_id - 1);
            if i == 0 && start_id > 1 {
                ids.push(start_id - 1); // priming, see the doc comment
            }
            ids.extend(start_id..start_id + packet_run(&mut err, ideal, available));
        }

        let packets = ids
            .into_iter()
            .map(|id| match reader.read_sample(track_id, id)? {
                Some(sample) => Ok(AacPacket {
                    bytes: sample.bytes.to_vec(),
                    samples: SAMPLES_PER_PACKET,
                }),
                None => Err(format!("audio sample {id} of {sample_count} is missing").into()),
            })
            .collect::<crate::Result<Vec<_>>>()?;

        Ok(Some((
            AacTrackParams {
                freq_index,
                chan_conf,
                sample_rate,
            },
            packets,
        )))
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
/// walks; the box's own entry type is `pub(crate)` in mp4 0.14.
fn stts_pairs(track: &Mp4Track) -> impl Iterator<Item = (u32, u32)> + '_ {
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
        .and_then(|t| scale(t, sample_rate, track.timescale()))
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
/// ponytail: the walk-back is clamped to the start of the containing stts entry
/// rather than crossing into the previous one, so a target within `pre_roll`
/// packets of an entry boundary gets less pre-roll. AAC packets are uniformly
/// 1024 frames (one entry per track), so this cannot fire here; upgrade path is
/// carrying the previous entry's delta through the walk.
fn packet_at(
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

/// One window of the source to decode, resolved to media samples and packets.
struct Segment {
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

struct Worker {
    reader: Mp4Reader<BufReader<File>>,
    /// A fresh decoder is built from these per segment: seeking mid-stream
    /// leaves MDCT and PNS state that belongs to the packets we skipped.
    params: AudioCodecParameters,
    track_id: u32,
    sample_count: u32,
    channels: usize,
    priming: u64,
    segments: Vec<Segment>,
    tx: SyncSender<AudioChunk>,
}

fn run(mut w: Worker) {
    let mut interleaved = Vec::new();
    // Chunk numbering is continuous across the segment joins, counted from the
    // first segment's audible start (see `open_segments`).
    let mut timeline = w
        .segments
        .first()
        .map_or(0, |s| s.media_target.saturating_sub(w.priming));

    for seg in &w.segments {
        let mut decoder = match AacDecoder::try_new(&w.params, &AudioDecoderOptions::default()) {
            Ok(decoder) => decoder,
            Err(e) => {
                eprintln!("audio decoder init failed: {e}");
                return;
            }
        };
        let mut pos = seg.start_pos;

        for id in seg.start_id..=w.sample_count {
            if pos >= seg.media_end {
                break; // segment done, on to the next one
            }
            let sample = match w.reader.read_sample(w.track_id, id) {
                Ok(Some(sample)) => sample,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("audio demux error at sample {id}: {e}");
                    break;
                }
            };
            let packet = Packet::new(
                w.track_id,
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
            let next = pos + (interleaved.len() / w.channels) as u64;

            // Everything before the target is encoder priming and/or decoder
            // pre-roll, not audio the caller asked for: drop it, splitting the
            // packet that straddles the target.
            if next <= seg.media_target {
                pos = next;
                continue;
            }
            if pos < seg.media_target {
                interleaved.drain(..(seg.media_target - pos) as usize * w.channels);
                pos = seg.media_target;
            }
            // And the tail past the segment end goes too — on a short segment
            // that is this same buffer, trimmed at both ends.
            if next > seg.media_end {
                interleaved.truncate((seg.media_end - pos) as usize * w.channels);
            }
            pos = next;
            if interleaved.is_empty() {
                continue; // nothing left of it; the loop head ends the segment
            }
            let chunk = AudioChunk {
                start_sample: timeline,
                samples: std::mem::take(&mut interleaved),
            };
            timeline += (chunk.samples.len() / w.channels) as u64;
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
}
