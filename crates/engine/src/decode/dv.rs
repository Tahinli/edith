//! The DV software seat (`ec-dv`): DV25 (NTSC 4:1:1, PAL 4:2:0 and 4:1:1), DVCPRO25, DVCPRO50
//! (4:2:2) and DVCPRO HD (1080i / 720p, 4:2:2). Every frame is a sync sample holding the whole
//! picture, so a seek is one sample read. 4:1:1 comes out widened to 4:2:2 by exact chroma
//! duplication (`ec-core` has no 4:1:1) and then through the same chroma averaging every 4:2:2
//! source takes. DV is field-coded: a frame whose VAUX pack says so goes through the shared
//! deinterlacer. See [`super::intra`] for the stream and the span loop.

use ec_core::frame::VideoFrame;
use ec_core::registry::{CodecId, CodecParameters};

use super::intra::{IntraFacts, Seat};

pub(crate) struct DvSeat(ec_dv::DvDecoder);

impl Seat for DvSeat {
    const NAME: &'static str = "DV";
    fn open(_: IntraFacts, _: &[u8]) -> Result<Self, String> {
        ec_dv::DvDecoder::new(CodecParameters::new(CodecId::DvVideo)).map(Self).map_err(|e| e.to_string())
    }
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String> {
        self.0.decode(sample).map_err(|e| e.to_string())
    }
}

pub(crate) type Stream = super::intra::Stream<DvSeat>;
