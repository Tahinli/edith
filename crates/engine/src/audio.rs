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
        let decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default())?;

        // Where to land, in media samples (priming included, so it is directly
        // comparable to decoded position), and which packet holds it.
        let media_target = (start_secs * f64::from(sample_rate)) as u64 + priming;
        let target_ts = unscale(media_target, sample_rate, track.timescale());
        // Two packets of pre-roll: AAC-LC's MDCT overlap-add needs the previous
        // packet to cancel aliasing, plus one more to warm the decoder up. Their
        // output is discarded below, so this only costs decode time.
        let (start_id, start_ts) = packet_at(
            track
                .trak
                .mdia
                .minf
                .stbl
                .stts
                .entries
                .iter()
                .map(|e| (e.sample_count, e.sample_delta)),
            target_ts,
            PRE_ROLL,
        );
        let start_pos = scale(start_ts, sample_rate, track.timescale()).unwrap_or(0);

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
                    decoder,
                    track_id,
                    sample_count,
                    channels: channels as usize,
                    priming,
                    start_id,
                    start_pos,
                    media_target,
                    tx,
                })
            })?;
        Ok(Some((meta, rx)))
    }
}

/// The 2-byte AAC-LC AudioSpecificConfig, rebuilt from the esds fields; mp4 0.14
/// parses those out and drops the raw bytes, so this is the exact inverse of its
/// writer (`mp4box/mp4a.rs` `DecoderSpecificDescriptor::write_box`).
fn audio_specific_config(track: &Mp4Track) -> crate::Result<Box<[u8]>> {
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
    Ok(Box::new([
        (cfg.profile << 3) | (cfg.freq_index >> 1),
        (cfg.freq_index << 7) | (cfg.chan_conf << 3),
    ]))
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

struct Worker {
    reader: Mp4Reader<BufReader<File>>,
    decoder: AacDecoder,
    track_id: u32,
    sample_count: u32,
    channels: usize,
    priming: u64,
    /// First packet to feed the decoder, 1-based; includes the pre-roll.
    start_id: u32,
    /// Media position of `start_id`'s first frame, in samples-per-channel.
    start_pos: u64,
    /// Media position of the first frame to emit. Everything decoded before it
    /// is pre-roll, priming, or seek overshoot, and gets dropped.
    media_target: u64,
    tx: SyncSender<AudioChunk>,
}

fn run(mut w: Worker) {
    let mut interleaved = Vec::new();
    let mut pos = w.start_pos;

    for id in w.start_id..=w.sample_count {
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
        let buf = match w.decoder.decode(&packet) {
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
        if next <= w.media_target {
            pos = next;
            continue;
        }
        if pos < w.media_target {
            interleaved.drain(..(w.media_target - pos) as usize * w.channels);
            pos = w.media_target;
        }
        let chunk = AudioChunk {
            start_sample: pos - w.priming,
            samples: std::mem::take(&mut interleaved),
        };
        pos = next;
        if w.tx.send(chunk).is_err() {
            break; // consumer went away
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PRE_ROLL, packet_at};

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
