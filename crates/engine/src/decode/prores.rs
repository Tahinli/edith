//! The Apple ProRes software seat (`ec-prores`): 422 Proxy, LT, Standard, HQ (10-bit 4:2:2)
//! and 4444, 4444 XQ (12-bit 4:4:4, narrowed to 10). Every frame is a sync sample, so a seek
//! is one sample read. Interlaced frames come out woven and go through the shared
//! deinterlacer; an alpha plane is dropped (the engine has no alpha path for a video clip).
//! See [`super::intra`] for the stream and the span loop.

use ec_core::frame::VideoFrame;
use ec_core::registry::{CodecId, CodecParameters};

use super::intra::{IntraFacts, Seat};

pub(crate) struct ProResSeat(ec_prores::ProResDecoder);

impl Seat for ProResSeat {
    const NAME: &'static str = "ProRes";
    fn open(_: IntraFacts, _: &[u8]) -> Result<Self, String> {
        ec_prores::ProResDecoder::new(CodecParameters::new(CodecId::ProRes)).map(Self).map_err(|e| e.to_string())
    }
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String> {
        self.0.decode(sample).map_err(|e| e.to_string())
    }
}

pub(crate) type Stream = super::intra::Stream<ProResSeat>;
