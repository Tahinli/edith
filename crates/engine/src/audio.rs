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
            other => return Err(format!("unsupported channel layout: {other:?} (max stereo)").into()),
        };

        let track_id = track.track_id();
        let sample_rate = track.sample_freq_index()?.freq();
        let priming = priming_samples(track, sample_rate);
        let total_samples = scale(track.trak.mdia.mdhd.duration, sample_rate, track.timescale())
            .map(|d| d.saturating_sub(priming));

        let mut params = AudioCodecParameters::new();
        params
            .for_codec(CODEC_ID_AAC)
            .with_sample_rate(sample_rate)
            .with_extra_data(audio_specific_config(track)?);
        let decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default())?;

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
        thread::Builder::new().name("audio-decode".into()).spawn(move || {
            run(Worker {
                reader,
                decoder,
                track_id,
                sample_count,
                channels: channels as usize,
                priming,
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

struct Worker {
    reader: Mp4Reader<BufReader<File>>,
    decoder: AacDecoder,
    track_id: u32,
    sample_count: u32,
    channels: usize,
    priming: u64,
    tx: SyncSender<AudioChunk>,
}

fn run(mut w: Worker) {
    let mut interleaved = Vec::new();
    let mut to_skip = w.priming;
    let mut emitted = 0u64;

    for id in 1..=w.sample_count {
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

        // Priming frames are encoder ramp-up, not audio: drop them so the first
        // sample the caller sees is the first audible one.
        if to_skip > 0 {
            let drop = to_skip.min((interleaved.len() / w.channels) as u64);
            interleaved.drain(..drop as usize * w.channels);
            to_skip -= drop;
            if interleaved.is_empty() {
                continue;
            }
        }
        let chunk = AudioChunk {
            start_sample: emitted,
            samples: std::mem::take(&mut interleaved),
        };
        emitted += (chunk.samples.len() / w.channels) as u64;
        if w.tx.send(chunk).is_err() {
            break; // consumer went away
        }
    }
}

#[cfg(test)]
mod tests {
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
