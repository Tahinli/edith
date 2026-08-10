//! Annex-B -> MP4, the mirror of `demux`: encoders emit start-code framed NALs,
//! an mp4 sample wants 4-byte length prefixes with SPS/PPS living in `avcC`.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use mp4::{
    AacConfig, AudioObjectType, AvcConfig, Bytes, ChannelConfig, FourCC, MediaConfig, Mp4Config,
    Mp4Sample, Mp4Writer, SampleFreqIndex, TrackConfig, TrackType,
};

/// 90 kHz is the H.264 convention; frame durations round to well under a
/// microsecond of error at every fps we care about.
const VIDEO_TIMESCALE: u32 = 90_000;
/// The same as the video's, deliberately: `mp4` converts every sample duration
/// into the movie timescale with an integer division of its own (track.rs:752),
/// so a coarser movie clock loses a fraction of a millisecond *per frame* --
/// 1 kHz makes a 4.000 s export announce itself as 3.960 s. Equal timescales
/// make the video conversion exact.
const MOVIE_TIMESCALE: u32 = VIDEO_TIMESCALE;
/// An AAC-LC packet is always 1024 samples, so with timescale == sample rate the
/// per-packet duration is this constant.
const AAC_PACKET_SAMPLES: u32 = 1024;

const NAL_IDR: u8 = 5;
const NAL_SEI: u8 = 6;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;

pub struct VideoParams<'a> {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub sps: &'a [u8],
    pub pps: &'a [u8],
}

/// Read straight off the source file's `esds`: export copies AAC packets, it
/// never re-encodes, so the decoder config is passed through unchanged.
pub struct AudioParams {
    pub freq_index: u8,
    pub chan_conf: u8,
}

/// Writes one H.264 track plus an optional AAC track. mp4 0.14 ignores
/// `Mp4Sample::start_time` and builds all timing by accumulating `duration`, so
/// every sample handed over must carry an exact duration or the tracks drift.
pub struct Mp4Muxer {
    writer: Mp4Writer<BufWriter<File>>,
    frame_duration: u32,
    has_audio: bool,
}

const VIDEO_TRACK: u32 = 1;
const AUDIO_TRACK: u32 = 2;

impl Mp4Muxer {
    pub fn create(
        path: &Path,
        video: &VideoParams,
        audio: Option<&AudioParams>,
    ) -> crate::Result<Self> {
        if !video.frame_rate.is_finite() || video.frame_rate <= 0.0 {
            return Err(format!("bad frame rate {}", video.frame_rate).into());
        }
        if video.width == 0
            || video.height == 0
            || video.width > u16::MAX as u32
            || video.height > u16::MAX as u32
        {
            return Err(format!("bad dimensions {}x{}", video.width, video.height).into());
        }
        // AvcCBox::new indexes sps[1..=3] for profile/level, so a short SPS panics
        // inside the mp4 crate; refuse it here where the message still means something.
        if video.sps.len() < 4 || video.pps.is_empty() {
            return Err("no usable SPS/PPS for the video track".into());
        }

        let mut writer = Mp4Writer::write_start(
            BufWriter::new(File::create(path)?),
            &Mp4Config {
                major_brand: FourCC::from(*b"isom"),
                minor_version: 512,
                compatible_brands: vec![
                    FourCC::from(*b"isom"),
                    FourCC::from(*b"iso2"),
                    FourCC::from(*b"avc1"),
                    FourCC::from(*b"mp41"),
                ],
                timescale: MOVIE_TIMESCALE,
            },
        )?;
        writer.add_track(&TrackConfig {
            track_type: TrackType::Video,
            timescale: VIDEO_TIMESCALE,
            language: "und".to_string(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: video.width as u16,
                height: video.height as u16,
                seq_param_set: video.sps.to_vec(),
                pic_param_set: video.pps.to_vec(),
            }),
        })?;
        if let Some(audio) = audio {
            let freq_index = SampleFreqIndex::try_from(audio.freq_index)?;
            let sample_rate = freq_index.freq();
            writer.add_track(&TrackConfig {
                track_type: TrackType::Audio,
                timescale: sample_rate,
                language: "und".to_string(),
                media_conf: MediaConfig::AacConfig(AacConfig {
                    bitrate: 0,
                    profile: AudioObjectType::AacLowComplexity,
                    freq_index,
                    chan_conf: ChannelConfig::try_from(audio.chan_conf)?,
                }),
            })?;
        }

        Ok(Self {
            writer,
            frame_duration: (VIDEO_TIMESCALE as f64 / video.frame_rate).round() as u32,
            has_audio: audio.is_some(),
        })
    }

    /// One coded picture, Annex-B framed. Parameter sets inside it are dropped
    /// (they are already in `avcC`); an IDR slice marks the sample as a sync point.
    pub fn write_video_au(&mut self, annex_b: &[u8]) -> crate::Result<()> {
        let (bytes, is_sync) = annex_b_to_avcc(annex_b)?;
        self.writer.write_sample(
            VIDEO_TRACK,
            &Mp4Sample {
                start_time: 0, // ignored by the writer; timing is duration-accumulated
                duration: self.frame_duration,
                rendering_offset: 0, // no B-frames on either encode path
                is_sync,
                bytes: Bytes::from(bytes),
            },
        )?;
        Ok(())
    }

    /// One raw AAC packet (no ADTS header), copied verbatim from the source --
    /// or one of the hand-written silent ones a gap is filled with. Every AAC-LC
    /// access unit is [`AAC_PACKET_SAMPLES`] frames, gap or not.
    pub fn write_audio_packet(&mut self, bytes: &[u8]) -> crate::Result<()> {
        if !self.has_audio {
            return Err("audio packet written to a video-only file".into());
        }
        self.writer.write_sample(
            AUDIO_TRACK,
            &Mp4Sample {
                start_time: 0,
                duration: AAC_PACKET_SAMPLES,
                rendering_offset: 0,
                // Every AAC packet is a sync point; saying so per-sample would emit
                // an `stss` listing all of them, while no `stss` means the same thing.
                is_sync: false,
                bytes: Bytes::copy_from_slice(bytes),
            },
        )?;
        Ok(())
    }

    pub fn finish(mut self) -> crate::Result<()> {
        self.writer.write_end()?;
        Ok(())
    }
}

/// SPS and PPS of an Annex-B access unit, for the `avcC` box. `None` when the
/// unit does not carry both, which the first exported unit always must.
pub fn parameter_sets(annex_b: &[u8]) -> Option<(&[u8], &[u8])> {
    let nals = split_annex_b(annex_b);
    let sps = nals.iter().find(|n| nal_type(n) == NAL_SPS)?;
    let pps = nals.iter().find(|n| nal_type(n) == NAL_PPS)?;
    Some((sps, pps))
}

/// Whether the unit carries a coded picture (NAL types 1..=5). An encoder that
/// buffered instead of coding hands back an empty buffer, and one that emitted
/// only parameter sets has no sample to write -- both are for the caller to skip
/// rather than for [`Mp4Muxer::write_video_au`] to reject.
pub(crate) fn has_coded_slice(annex_b: &[u8]) -> bool {
    split_annex_b(annex_b)
        .iter()
        .any(|nal| (1..=NAL_IDR).contains(&nal_type(nal)))
}

/// Exact inverse of `demux::append_annex_b`: 4-byte length prefixes (the only
/// prefix size `mp4` writes, avc1.rs hardcodes `length_size_minus_one = 3`).
/// Parameter sets, access unit delimiters and SEI are stripped — the first two
/// are redundant with `avcC`, SEI is not worth carrying. Returns the sample
/// bytes and whether the unit is an IDR.
fn annex_b_to_avcc(annex_b: &[u8]) -> crate::Result<(Vec<u8>, bool)> {
    let mut out = Vec::with_capacity(annex_b.len());
    let mut is_idr = false;
    for nal in split_annex_b(annex_b) {
        match nal_type(nal) {
            NAL_SPS | NAL_PPS | NAL_AUD | NAL_SEI => continue,
            NAL_IDR => is_idr = true,
            _ => {}
        }
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    if out.is_empty() {
        return Err("access unit has no coded slice".into());
    }
    Ok((out, is_idr))
}

/// NAL payloads of an Annex-B buffer, both 3- and 4-byte start codes, trailing
/// zero bytes trimmed (they belong to the next start code, or are padding).
fn split_annex_b(src: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut start = None;
    let mut i = 0;
    while i + 2 < src.len() {
        if src[i] == 0 && src[i + 1] == 0 && src[i + 2] == 1 {
            if let Some(s) = start {
                push_nal(&mut nals, &src[s..i]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    if let Some(s) = start {
        push_nal(&mut nals, &src[s..]);
    }
    nals
}

fn push_nal<'a>(nals: &mut Vec<&'a [u8]>, nal: &'a [u8]) {
    let end = nal.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    if end > 0 {
        nals.push(&nal[..end]);
    }
}

fn nal_type(nal: &[u8]) -> u8 {
    nal[0] & 0x1F
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1E, 0xAB];
    const PPS: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

    fn au(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for nal in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(nal);
        }
        v
    }

    #[test]
    fn splits_both_start_code_lengths() {
        let src = [
            0, 0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB, 0, 0, 0, 1, 0x65, 0xCC,
        ];
        let nals = split_annex_b(&src);
        assert_eq!(nals, [&[0x67, 0xAA][..], &[0x68, 0xBB], &[0x65, 0xCC]]);
    }

    #[test]
    fn trims_trailing_zero_padding() {
        let src = [0, 0, 0, 1, 0x65, 0xCC, 0, 0, 0];
        assert_eq!(split_annex_b(&src), [&[0x65, 0xCC][..]]);
    }

    #[test]
    fn repacks_multi_nal_unit_and_drops_parameter_sets() {
        let slice = [0x65, 0x88, 0x99];
        let (bytes, is_idr) = annex_b_to_avcc(&au(&[SPS, PPS, &[0x09, 0x10], &slice])).unwrap();
        assert!(is_idr);
        assert_eq!(
            bytes,
            [0, 0, 0, 3, 0x65, 0x88, 0x99],
            "4-byte prefix, slice only"
        );

        let (bytes, is_idr) = annex_b_to_avcc(&au(&[&[0x41, 0x9A], &[0x41, 0x9B]])).unwrap();
        assert!(!is_idr, "no IDR slice in the unit");
        assert_eq!(bytes, [0, 0, 0, 2, 0x41, 0x9A, 0, 0, 0, 2, 0x41, 0x9B]);
    }

    #[test]
    fn rejects_units_with_no_slice() {
        assert!(annex_b_to_avcc(&[]).is_err(), "empty");
        assert!(annex_b_to_avcc(&[0x65, 0x88]).is_err(), "no start code");
        assert!(
            annex_b_to_avcc(&au(&[SPS, PPS])).is_err(),
            "parameter sets only"
        );
    }

    #[test]
    fn finds_parameter_sets() {
        assert_eq!(
            parameter_sets(&au(&[SPS, PPS, &[0x65, 0x11]])),
            Some((SPS, PPS))
        );
        assert!(
            parameter_sets(&au(&[SPS, &[0x65, 0x11]])).is_none(),
            "no PPS"
        );
    }

    #[test]
    fn create_rejects_unusable_config() {
        let out = std::env::temp_dir().join("ve_mux_never_written.mp4");
        let ok = VideoParams {
            width: 64,
            height: 64,
            frame_rate: 30.0,
            sps: SPS,
            pps: PPS,
        };
        assert!(
            Mp4Muxer::create(
                &out,
                &VideoParams {
                    sps: &SPS[..3],
                    ..ok
                },
                None
            )
            .is_err(),
            "SPS too short for avcC"
        );
        assert!(
            Mp4Muxer::create(
                &out,
                &VideoParams {
                    frame_rate: 0.0,
                    ..ok
                },
                None
            )
            .is_err()
        );
        assert!(
            Mp4Muxer::create(&out, &VideoParams { width: 0, ..ok }, None).is_err(),
            "zero width"
        );
        let _ = std::fs::remove_file(&out);
    }

    /// Three encoder-shaped access units out, then back in through both `mp4`'s
    /// own reader and our `Demuxer` — the round trip is what proves the repack is
    /// the exact inverse of `demux::append_annex_b`.
    #[test]
    fn round_trips_through_both_readers() {
        let units = [
            au(&[SPS, PPS, &[0x65, 0x01, 0x02, 0x03]]),
            au(&[&[0x41, 0x04, 0x05]]),
            au(&[&[0x41, 0x06]]),
        ];
        let out = std::env::temp_dir().join(format!("ve_mux_{}.mp4", std::process::id()));

        let mut muxer = Mp4Muxer::create(
            &out,
            &VideoParams {
                width: 1280,
                height: 720,
                frame_rate: 30.0,
                sps: SPS,
                pps: PPS,
            },
            Some(&AudioParams {
                freq_index: 4, // 44100
                chan_conf: 2,
            }),
        )
        .unwrap();
        for unit in &units {
            muxer.write_video_au(unit).unwrap();
        }
        muxer.write_audio_packet(&[0x21, 0x22, 0x23]).unwrap();
        muxer.write_audio_packet(&[0x24, 0x25]).unwrap();
        muxer.finish().unwrap();

        let file = File::open(&out).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = mp4::Mp4Reader::read_header(BufReader::new(file), size).unwrap();
        let (video_id, audio_id) = (VIDEO_TRACK, AUDIO_TRACK);
        assert_eq!(reader.sample_count(video_id).unwrap(), 3);
        assert_eq!(reader.sample_count(audio_id).unwrap(), 2);
        {
            let track = &reader.tracks()[&video_id];
            assert_eq!((track.width(), track.height()), (1280, 720));
            assert_eq!(track.frame_rate().round(), 30.0);
            assert_eq!(track.sequence_parameter_set().unwrap(), SPS);
            assert_eq!(track.picture_parameter_set().unwrap(), PPS);
        }
        let first = reader.read_sample(video_id, 1).unwrap().unwrap();
        assert!(first.is_sync, "IDR unit is a sync sample");
        assert_eq!(first.duration, 3000, "90000 / 30 fps");
        assert_eq!(&first.bytes[..], [0, 0, 0, 4, 0x65, 0x01, 0x02, 0x03]);
        assert!(!reader.read_sample(video_id, 2).unwrap().unwrap().is_sync);
        assert_eq!(
            &reader.read_sample(audio_id, 1).unwrap().unwrap().bytes[..],
            [0x21, 0x22, 0x23],
            "audio packets copied verbatim"
        );
        let second = reader.read_sample(audio_id, 2).unwrap().unwrap();
        assert_eq!(&second.bytes[..], [0x24, 0x25], "verbatim");
        assert_eq!(
            second.duration, AAC_PACKET_SAMPLES,
            "every AAC-LC packet is one 1024-frame stts entry"
        );

        let (meta, mut demuxer) = crate::demux::Demuxer::open(&out).unwrap();
        assert_eq!(meta.frame_count, 3);
        assert_eq!((meta.width, meta.height), (1280, 720));
        assert_eq!(meta.frame_rate.round(), 30.0);
        for expected in &units {
            assert_eq!(
                &demuxer.next_access_unit().unwrap().unwrap(),
                expected,
                "demux returns the exact bytes that went in"
            );
        }
        assert!(demuxer.next_access_unit().unwrap().is_none());

        std::fs::remove_file(&out).unwrap();
    }
}
