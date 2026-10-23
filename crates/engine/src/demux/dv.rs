//! Raw DV (`.dv`, `.dif`) over the family's own [`ec_dv::DvDemuxer`], read by the very readers a
//! transport stream is ([`super::ts`]): [`ts::TsDemuxer`](super::ts::TsDemuxer) for the picture,
//! [`ts::TsAudio`](super::ts::TsAudio) for each sound track -- the way [`super::avi`] is.
//!
//! [`DvSrc`] is the adapter: it presents the file as the 90 kHz packet stream the readers expect
//! (every stream's time base rescaled to ticks, a seek rescaled back), and its streams as the
//! [`ec_mpegts::TrackInfo`] list `ts::describe_open` walks. Source time 0 is frame 0.
//!
//! **Picture.** One DV frame per packet (DV25 NTSC and PAL, DVCPRO25/50, DVCPRO HD), every frame a
//! sync sample. **Sound.** The PCM in the audio DIF blocks, one 16-bit stereo stream per pair
//! the first frame's audio source pack announces (12-bit non-linear already expanded).

use std::io::BufReader;

use ec_core::registry::SeekMode;
use ec_core::timebase::{Rounding, TimeBase, Timestamp};
use ec_core::{Packet, StreamInfo};
use ec_dv::DvDemuxer;
use ec_mpegts::{StreamKind as TsKind, TrackInfo};

use super::*;

type Reader = BufReader<File>;

/// The ticks a second is cut into, as [`super::ts`] counts.
fn tb() -> TimeBase {
    TimeBase::new(1, 90_000)
}

/// Whether `path` is a raw DV file: its extension (`.dv`, `.dif`), or -- for a name that says
/// nothing -- the DIF section headers in its first bytes. A container whose magic is Matroska's
/// or an mp4's is never one, nor is a RIFF file (an AVI holds DV *inside*), a standalone sound
/// file or a still.
pub fn is_dv(path: &Path) -> bool {
    if sniff_container(path).is_some() {
        return false;
    }
    let named = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "dv" | "dif"));
    if !named && (crate::is_audio(path) || crate::is_image(path)) {
        return false;
    }
    let mut head = vec![0u8; 64 << 10];
    let read = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = &head[..read];
    if head.starts_with(b"RIFF") {
        return false;
    }
    ec_dv::is_dv(head)
}

pub(crate) fn dv_error(e: ec_core::Error) -> crate::Error {
    format!("DV: {e}").into()
}

/// A raw DV file as the packet stream [`super::ts::Src`] speaks.
pub(crate) struct DvSrc {
    d: DvDemuxer<Reader>,
    /// The streams in 90 kHz ticks.
    streams: Vec<StreamInfo>,
    /// Each stream's own time base, by stream index.
    native: Vec<TimeBase>,
}

impl DvSrc {
    pub(crate) fn open(path: &Path) -> crate::Result<Self> {
        let file = File::open(path)?;
        let d = DvDemuxer::new(BufReader::with_capacity(256 << 10, file)).map_err(dv_error)?;
        let mut native = Vec::new();
        let streams = ec_core::Demuxer::streams(&d)
            .iter()
            .map(|s| {
                native.push(s.time_base);
                let mut s = s.clone();
                let to = |t: i64| Timestamp::new(t, s.time_base).rescale(tb(), Rounding::Nearest).ticks;
                s.start_time = s.start_time.map(to);
                s.duration = s.duration.map(to);
                s.time_base = tb();
                s
            })
            .collect();
        Ok(Self { d, streams, native })
    }

    pub(crate) fn streams(&self) -> &[StreamInfo] {
        &self.streams
    }

    pub(crate) fn tracks(&self) -> Vec<TrackInfo> {
        self.streams
            .iter()
            .map(|s| TrackInfo {
                pid: s.index as u16,
                stream_type: 0,
                kind: TsKind::Other(if s.index == 0 { "dvvideo".into() } else { "pcm_s16le".into() }),
                stream: Some(s.index),
                refusal: None,
                language: None,
            })
            .collect()
    }

    pub(crate) fn next_packet(&mut self) -> ec_core::Result<Packet> {
        let mut p = ec_core::Demuxer::next_packet(&mut self.d)?;
        let from = self.native.get(p.stream as usize).copied().unwrap_or(p.time_base);
        let to = |t: i64| Timestamp::new(t, from).rescale(tb(), Rounding::Nearest).ticks;
        p.pts = p.pts.map(to);
        p.dts = p.dts.map(to);
        p.duration = p.duration.map(to);
        p.time_base = tb();
        Ok(p)
    }

    pub(crate) fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> ec_core::Result<()> {
        let at = to.rescale(self.native.get(stream as usize).copied().unwrap_or(to.base), Rounding::Nearest);
        ec_core::Demuxer::seek(&mut self.d, stream, at, mode)
    }

    pub(crate) fn duration_secs(&self) -> Option<f64> {
        self.streams
            .iter()
            .filter_map(|s| s.duration)
            .max()
            .map(|t| t as f64 / 90_000.0)
    }
}
