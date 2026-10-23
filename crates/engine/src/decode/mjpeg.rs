//! The Motion JPEG software seat (`ec-mjpeg`): baseline and progressive JPEG per frame, 4:2:0,
//! 4:2:2, 4:4:4 and gray, always full range (`yuvj`; the probe states that as the stream's
//! colour so the conversion honours it). A packet of two JPEGs is one interlaced frame, woven
//! and then deinterlaced; the first image is the top field unless the container's `fiel` /
//! `FieldOrder` says bottom. Every frame is a sync sample. See [`super::intra`].

use ec_core::frame::VideoFrame;

use super::intra::{IntraFacts, Seat, mjpeg_decoder};

pub(crate) struct MjpegSeat(ec_mjpeg::MjpegDecoder);

impl Seat for MjpegSeat {
    const NAME: &'static str = "MJPEG";
    fn open(facts: IntraFacts, _: &[u8]) -> Result<Self, String> {
        mjpeg_decoder(facts.coded_height, facts.field_order()).map(Self)
    }
    fn decode(&mut self, sample: &[u8]) -> Result<VideoFrame, String> {
        self.0.decode(sample, None).map_err(|e| e.to_string())
    }
}

pub(crate) type Stream = super::intra::Stream<MjpegSeat>;
