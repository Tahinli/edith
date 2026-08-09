//! MP4 demux: pulls the H.264 track out as Annex-B access units.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use mp4::{MediaType, Mp4Reader};

const START_CODE: [u8; 4] = [0, 0, 0, 1];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
}

pub struct Demuxer {
    reader: Mp4Reader<BufReader<File>>,
    track_id: u32,
    sample_count: u32,
    /// Annex-B SPS+PPS, re-injected ahead of every sync sample.
    parameter_sets: Vec<u8>,
    next_sample: u32,
}

impl Demuxer {
    pub fn open(path: &Path) -> crate::Result<(VideoMeta, Self)> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let reader = Mp4Reader::read_header(BufReader::new(file), size)?;

        let track = reader
            .tracks()
            .values()
            .find(|t| matches!(t.media_type(), Ok(MediaType::H264)))
            .ok_or("no H.264 video track in file")?;

        let track_id = track.track_id();
        let meta = VideoMeta {
            width: track.width() as u32,
            height: track.height() as u32,
            frame_rate: track.frame_rate(),
            frame_count: 0,
        };
        let mut parameter_sets = Vec::new();
        parameter_sets.extend_from_slice(&START_CODE);
        parameter_sets.extend_from_slice(track.sequence_parameter_set()?);
        parameter_sets.extend_from_slice(&START_CODE);
        parameter_sets.extend_from_slice(track.picture_parameter_set()?);

        let sample_count = reader.sample_count(track_id)?;
        Ok((
            VideoMeta {
                frame_count: sample_count,
                ..meta
            },
            Self {
                reader,
                track_id,
                sample_count,
                parameter_sets,
                next_sample: 1, // sample ids are 1-based
            },
        ))
    }

    /// Next access unit in decode order, Annex-B framed. `None` at end of track.
    pub fn next_access_unit(&mut self) -> crate::Result<Option<Vec<u8>>> {
        if self.next_sample > self.sample_count {
            return Ok(None);
        }
        let sample = self.reader.read_sample(self.track_id, self.next_sample)?;
        self.next_sample += 1;
        let Some(sample) = sample else {
            return Ok(None);
        };

        let mut au = Vec::with_capacity(self.parameter_sets.len() + sample.bytes.len() + 16);
        if sample.is_sync {
            au.extend_from_slice(&self.parameter_sets);
        }
        append_annex_b(&sample.bytes, &mut au)?;
        Ok(Some(au))
    }
}

/// avcC length-prefixed NALs -> Annex-B. Assumes a 4-byte length prefix, which
/// mp4 0.14 does not expose; a wrong guess misparses immediately, hence the check.
///
/// ponytail: hardcoded 4-byte NAL length prefix (lengthSizeMinusOne == 3). 1/2-byte
/// prefixes exist in the wild; upgrade path is parsing the avcC box from
/// `track.trak` instead of assuming.
fn append_annex_b(mut src: &[u8], out: &mut Vec<u8>) -> crate::Result<()> {
    while !src.is_empty() {
        if src.len() < 4 {
            return Err(format!("truncated NAL length prefix: {} bytes left", src.len()).into());
        }
        let len = u32::from_be_bytes([src[0], src[1], src[2], src[3]]) as usize;
        if len == 0 || len > src.len() - 4 {
            return Err(format!(
                "bad NAL length {len} with {} bytes remaining (not a 4-byte-prefixed avcC sample?)",
                src.len() - 4
            )
            .into());
        }
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&src[4..4 + len]);
        src = &src[4 + len..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_two_nals() {
        let mut out = Vec::new();
        append_annex_b(&[0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41], &mut out).unwrap();
        assert_eq!(out, [0, 0, 0, 1, 0x65, 0xAA, 0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn rejects_misparse() {
        assert!(append_annex_b(&[0, 0, 0, 9, 0x65], &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0], &mut Vec::new()).is_err());
        assert!(append_annex_b(&[0, 0, 0, 0], &mut Vec::new()).is_err());
    }
}
