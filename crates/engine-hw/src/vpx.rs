//! The software VP8 decoder: `ec-vp8`, this project's own implementation of
//! RFC 6386 -- the reason a VP8 file decodes on a machine whose GPU has no
//! VP8 profile, or whose GPU is absent altogether.
//!
//! The seat was `libvpx`, resolved at runtime behind a hand-written FFI; the
//! native crate replaces it outright, and with it the whole dlopen layer
//! goes: the plugin links the decoder as ordinary Rust, so there is no
//! library to look for, no ABI to pin, and no machine where the codec is
//! absent from a build that carries this plugin. What survives from the FFI
//! days is the seat's shape -- the five verbs the session above lives on:
//!
//! - `init` -- a fresh [`Decoder`], infallible now that there is no ABI to
//!   negotiate;
//! - `decode` -- one access unit in, the picture (if any) held for the next
//!   `get_frame`;
//! - `get_frame` -- the picture the last `decode` produced, taken once;
//! - `flush` -- a formality: VP8 reorders nothing and holds nothing back, so
//!   end of stream has nothing to release (libvpx wanted the empty
//!   `vpx_codec_decode`; the native decoder has no equivalent need, and the
//!   verb stays because the session's pump is written around it);
//! - `destroy` -- now simply the drop of an owned value, which is why a
//!   seek is `self.decoder = Decoder::new()` where the FFI seat paid a
//!   destroy + re-init round trip.

use ec_vp8::decode;

/// The VP8 decoder: [`decode::Decoder`] plus the one-picture slot between
/// `decode` and `get_frame`.
pub(super) struct Decoder {
    inner: decode::Decoder,
    pending: Option<Picture>,
}

/// One decoded picture: the visible frame's planes, 8-bit 4:2:0, strided
/// (`stride` may exceed `width`) and owned by whoever holds it -- what
/// [`crate::VhFrame`] hands on is the packed copy the session makes.
pub(super) use decode::Picture;

impl Decoder {
    /// `init`: a decoder holding no references yet; the first key frame
    /// sets the size.
    pub(super) fn new() -> Self {
        Self {
            inner: decode::Decoder::new(),
            pending: None,
        }
    }

    /// `decode`: one access unit in -- one complete VP8 frame payload,
    /// exactly what a container demuxer hands over for `V_VP8`/`vp08`. A
    /// hidden frame (an alternate reference, `show_frame == 0`) updates the
    /// reference buffers and produces no picture: the same nothing libvpx
    /// emitted for one, so the seat's picture accounting does not move.
    pub(super) fn decode(&mut self, au: &[u8]) -> Result<(), String> {
        if au.is_empty() {
            return Ok(());
        }
        match self.inner.decode(au) {
            Ok(picture) => self.pending = picture,
            Err(e) => return Err(format!("vp8 decode failed: {e}")),
        }
        Ok(())
    }

    /// `get_frame`: the picture the last `decode` produced, if that frame
    /// was shown. Taken, not peeked -- the slot is empty again afterwards,
    /// which is what lets the pump loop feed the next unit.
    pub(super) fn get_frame(&mut self) -> Option<Picture> {
        self.pending.take()
    }

    /// `flush`: end of the track. VP8 queues nothing -- one unit in, at
    /// most one picture out, feed order -- so there is nothing held back to
    /// drain; the answer is always ok, immediately.
    pub(super) fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}
