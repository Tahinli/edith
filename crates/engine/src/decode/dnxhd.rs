//! The Avid DNxHD / DNxHR software seat (`ec-dnxhd`): all twenty compression ids, 8, 10 and
//! 12-bit (narrowed to 10) in 4:2:2 and 4:4:4, progressive and two-field. Every frame is a
//! sync sample, so a seek is one sample read. Interlaced frames come out woven and go
//! through the shared deinterlacer. See [`super::intra`] for the stream and the span loop.

use ec_core::frame::VideoFrame;
use ec_core::registry::{CodecId, CodecParameters};

use super::intra::{IntraFacts, Seat};

pub(crate) struct DnxhdSeat(ec_dnxhd::DnxhdDecoder);

impl Seat for DnxhdSeat {
    const NAME: &'static str = "DNxHD";
    fn open(_: IntraFacts, _: &[u8]) -> Result<Self, String> {
        ec_dnxhd::DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)).map(Self).map_err(|e| e.to_string())
    }
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String> {
        self.0.decode_bytes(sample).map_err(|e| e.to_string())
    }
}

pub(crate) type Stream = super::intra::Stream<DnxhdSeat>;
