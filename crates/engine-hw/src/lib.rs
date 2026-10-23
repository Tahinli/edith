//! Hardware decode and encode, held in a child process. This library is the
//! whole world of the `hw-child` binary ([`child`]), the one process a video
//! driver is allowed to kill: VA-API decode (H.264, HEVC, VP9 and AV1), VP8
//! decode on `ec-vp8` ([`vpx`], software, linked in), and the encode seats
//! (H.264, HEVC, and AV1 where the GPU has the entrypoint).
//!
//! It is a plain safe Rust crate: there is no `extern "C"` surface, no
//! `dlopen`, and no `unsafe` anywhere in it. The editor never links it; it
//! spawns the binary and speaks [`engine::hwproc`]'s wire to it, so an
//! `abort()` from a driver costs the caller a fallback and nothing more.

#![forbid(unsafe_code)]

pub mod child;
mod enc_ec;
mod vpx;

use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::rc::Rc;

use ec_hw::Frame;
use ec_hw::enc::EncCodec as EcCodec;
use ec_va::caps::Profile;
use enc_ec::{EcEncSession, EcOpen};
use engine::demux::{Codec, Demuxer, VideoMeta};
use engine::hw::{CAP_AV1, CAP_H264, CAP_HEVC, CAP_VP8, CAP_VP9, VhCaps, VhDma, VhMeta};

/// How many undecodable access units a seek may drop before the session gives
/// up and calls the stream broken. An open GOP's leading pictures are a handful
/// (a Blu-ray remux's are 8-16); a file that keeps failing past this is not one
/// we restarted in the middle of, so the error is real and the caller still
/// hears it.
/// `VA_FOURCC_NV12` and `VA_FOURCC_P010`: the four characters, little-endian.
const VA_FOURCC_NV12: u32 = u32::from_le_bytes(*b"NV12");
const VA_FOURCC_P010: u32 = u32::from_le_bytes(*b"P010");

const LEADING_LIMIT: u32 = 64;

/// How many pictures handed out as DRM-PRIME buffers stay un-decoded-over, and
/// therefore how many extra buffers the pool reserves the moment a caller asks
/// for one. The export's decode channel is two pictures deep and it encodes the
/// one it is holding, so at most four are ever outstanding; the rest is margin,
/// at ~3 MB of video memory each.
const DMA_HOLD: usize = 6;

/// How far ahead of the decode position [`Session::seek_to`] walks forward
/// rather than flushing, in seconds of the stream's own frames. A silence cut
/// lands the next span ahead of the decoder by the length of what it removed
/// -- tenths of a second -- and walking that forward costs only the decode of
/// the frames in between, dropped without the read-back ([`Session::pump`]'s
/// skip path) with the decoder's reference chain and pipeline depth left
/// standing. The flush it replaces costs a decoder reset, a demuxer seek to
/// the random access point, up to a group of skip-decoded pictures behind it,
/// and a full re-prime before the first frame is out again (~178 ms measured
/// at 4K, which is longer than the clips a silence cut leaves); past half a
/// second of walking, the skip-decode run alone approaches that bill and a
/// real seek wins.
const FORWARD_SECS: f64 = 0.5;

/// The forward reach in frames: [`FORWARD_SECS`] at the stream's own rate.
/// `0` for a rate the stream does not really state, which is "never forward"
/// -- exactly the flush every reposition did before this distinction existed.
fn forward_limit(frame_rate: f64) -> u32 {
    (FORWARD_SECS * frame_rate).round().max(0.) as u32
}

/// How a reposition to `target` (a 0-based display frame) is reached from
/// `position`, the frame the session would hand out next. Pure arithmetic,
/// so every shape of the answer is testable without a GPU.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Reposition {
    /// Decode forward from where the session stands, dropping this many
    /// pictures on the way: no flush, no demuxer seek, no re-prime.
    Forward(u32),
    /// Flush the decoder and seek the demuxer to the random access point at
    /// or before `target`.
    Flush,
}

fn reposition(position: i64, target: i64, frame_count: u32, limit: u32) -> Reposition {
    // A decoder cannot walk backwards, and a target at or past the last frame
    // is not one decoding forward can hand out -- both take the seek that has
    // always handled them, EOF included.
    if target < position || target >= i64::from(frame_count) {
        return Reposition::Flush;
    }
    match u32::try_from(target - position) {
        Ok(ahead) if ahead <= limit => Reposition::Forward(ahead),
        _ => Reposition::Flush,
    }
}

/// The skip a forward reposition leaves standing: `ahead` itself, replacing
/// whatever skip was undrained -- `_standing` is read by no one, on purpose.
/// [`Session::pump`] advances `position` for every handle it pops, dropped
/// and delivered alike, so the next picture delivered is always
/// `position + skip`: only a replacement keeps that on the target. An
/// accumulate would land `ahead` frames past it -- and silently, because the
/// pictures keep arriving (a stacked reseek before the first drains, or a
/// flush whose RAP skip is still standing when a forward span is asked).
fn forward_skip(_standing: u32, ahead: u32) -> u32 {
    ahead
}

struct Session {
    decoder: ec_hw::Decoder,
    /// The VA-API display the decoder's context and surfaces live on: kept
    /// alive for as long as the session is.
    _display: Rc<ec_va::Display>,
    demuxer: Demuxer,
    meta: VhMeta,
    timestamp: i64,
    /// `flush` has been called: the next empty `pump` is end of stream.
    flushed: bool,
    /// Pictures still to be decoded and thrown away to land on a seek target.
    skip: u32,
    /// The display frame the next `pump` would hand out, however it gets
    /// there: one further along for every frame taken from the decoder, skipped
    /// past and delivered alike. The one thing that can tell a reposition
    /// *ahead* of the decoder from one behind it ([`Session::seek_to`]).
    position: i64,
    /// Access units dropped because they referenced pictures this session never
    /// decoded -- see [`Session::pump`]. Counted rather than flagged so the
    /// tolerance ends somewhere, and raised to [`LEADING_LIMIT`] the moment a
    /// picture is handed out, which is where the window closes.
    leading: u32,
    /// Tightly packed I420 handed out as the picture; reused every frame,
    /// which is exactly why a picture is only valid until the next pump.
    out: Vec<u8>,
    luma: usize,
    chroma: usize,
    /// What the caller's encoder can read straight off the GPU, asked once per
    /// [`vh_next_frame_dma`] call and `None` for every read-back caller.
    want: Option<VhDma>,
    /// The descriptor the last such call produced, its file descriptor already
    /// belonging to the caller. `None` says that picture was read back instead.
    dma: Option<(VhDma, Vec<OwnedFd>)>,
    /// Pictures handed out as DRM-PRIME buffers, newest last. Holding a frame
    /// holds its surface out of the decoder's pool, so the decoder would write
    /// the next picture into the one an encoder is still reading unless this
    /// keeps the last [`DMA_HOLD`] of them alive.
    held: VecDeque<Frame>,
}

/// The VA-API display the decoders run on: the first render node that opens.
fn open_decode_display() -> Option<Rc<ec_va::Display>> {
    ec_va::open().ok()
}

impl Session {
    /// Assembles the VA-API half around a container that is already read --
    /// [`OpenSession::open_at`] owns the read and routes VP8 away from here
    /// before any device is opened.
    fn opened(meta: VideoMeta, demuxer: Demuxer) -> Option<Self> {
        let display = open_decode_display()?;
        let codec = match meta.codec {
            Codec::H264 => ec_hw::Codec::H264,
            Codec::Hevc => ec_hw::Codec::H265,
            Codec::Vp9 => ec_hw::Codec::Vp9,
            Codec::Av1 => ec_hw::Codec::Av1,
            // Routed to [`VpxSession`] by [`OpenSession::open_at`] before
            // this half is ever built.
            Codec::Vp8 => return None,
            // Never offered to VA-API: `decode::hw_decodes` refuses it first.
            Codec::Mpeg2 | Codec::ProRes | Codec::Dnxhd | Codec::Mjpeg | Codec::Dv | Codec::Mpeg4 | Codec::MsMpeg4 | Codec::Raw => return None,
        };
        let decoder = ec_hw::Decoder::new(&display, codec).ok()?;
        Some(Self {
            decoder,
            _display: display,
            demuxer,
            meta: VhMeta {
                width: meta.width,
                height: meta.height,
                frame_rate: meta.frame_rate,
                frame_count: meta.frame_count,
                coded_width: meta.coded_width,
                coded_height: meta.coded_height,
                pixel_aspect_h: meta.pixel_aspect.h,
                pixel_aspect_v: meta.pixel_aspect.v,
            },
            timestamp: 0,
            flushed: false,
            skip: 0,
            position: 0,
            // A stream read from its own beginning has no missing references
            // and therefore no leading pictures to forgive: the window opens
            // only for a session positioned somewhere else ([`OpenSession::open_at`]),
            // so a file whose first picture will not decode is still refused at
            // the door rather than opening onto nothing.
            leading: LEADING_LIMIT,
            out: Vec::new(),
            luma: 0,
            chroma: 0,
            want: None,
            dma: None,
            held: VecDeque::new(),
        })
    }

    /// Repositions an **open** session, exactly as [`OpenSession::open_at`]
    /// positions a new one: the next [`Session::pump`] hands back sample
    /// `target_sample`.
    ///
    /// A target *ahead* of where the decoder stands, within [`FORWARD_SECS`]
    /// of it, is reached by decoding on and dropping the pictures in between
    /// ([`reposition`]) -- no reset, no demuxer seek, no re-prime. Everything
    /// else pays the demuxer seek and the decoder reset.
    fn seek_to(&mut self, target_sample: u32) -> Result<(), String> {
        // The same arithmetic `open_at` does, for the same reason.
        let target_frame = target_sample.saturating_sub(1);
        if let Reposition::Forward(ahead) = reposition(
            self.position,
            i64::from(target_frame),
            self.meta.frame_count,
            forward_limit(self.meta.frame_rate),
        ) {
            // Nothing about the session moves but the skip: it *replaces*
            // whatever skip was standing, never adds to it ([`forward_skip`]).
            self.skip = forward_skip(self.skip, ahead);
            return Ok(());
        }
        // Discard all picture state, and whatever the decoder had completed
        // but not yet handed out: those pictures belong to where the session
        // *was*.
        self.decoder.reset();
        self.flushed = false;
        let first = self.demuxer.seek_to_sync_at_or_before(target_frame);
        self.skip = (i64::from(target_frame) - first).max(0) as u32;
        self.position = first;
        // ...and the same open-GOP window: a session positioned anywhere but the
        // start may meet leading pictures that reference what it never decoded.
        self.leading = if target_frame > 0 { 0 } else { LEADING_LIMIT };
        Ok(())
    }

    /// Pumps the decoder until one picture is ready. `Ok(false)` is clean EOF.
    fn pump(&mut self) -> Result<bool, String> {
        loop {
            if let Some(frame) = self.decoder.next_frame() {
                // One picture further along, shown or skipped past alike:
                // this is the only place [`Session::position`] walks.
                self.position += 1;
                // Seek discards: dropping the frame skips the read-back and
                // frees its pool surface.
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                self.deliver(frame)?;
                // Past the random access point's leading pictures: a decode
                // error from here on is a real one again.
                self.leading = LEADING_LIMIT;
                return Ok(true);
            }
            if self.flushed {
                return Ok(false);
            }
            match self.demuxer.next_access_unit() {
                Ok(Some(au)) => {
                    match self.decoder.decode(&au, self.timestamp) {
                        Ok(()) => {}
                        // A picture that references pictures we never decoded,
                        // which is what an open GOP's leading pictures are after
                        // a seek: they display *before* the random access point
                        // we restarted at, so they are not ours to show and the
                        // stream is not broken. Drop the access unit and walk on.
                        Err(e) if self.leading < LEADING_LIMIT => {
                            if self.leading == 0 {
                                eprintln!("engine_hw: skipping leading pictures after seek ({e})");
                            }
                            self.leading += 1;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                    self.timestamp += 1;
                }
                Ok(None) => {
                    self.decoder.flush().map_err(|e| e.to_string())?;
                    self.flushed = true;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Hands one decoded picture to the caller: as the buffer it was decoded
    /// into where that is what the caller asked for and the encoder on the
    /// other side can read it, and as read-back I420 everywhere else.
    fn deliver(&mut self, frame: Frame) -> Result<(), String> {
        self.dma = None;
        if self.want.is_some()
            && let Some(dma) = self.export_dma(&frame)
        {
            self.meta.width = dma.0.width;
            self.meta.height = dma.0.height;
            self.dma = Some(dma);
            // Kept out of the pool until the caller is [`DMA_HOLD`] pictures
            // past it, by which time nothing is reading it any more.
            self.held.push_back(frame);
            while self.held.len() > DMA_HOLD {
                self.held.pop_front();
            }
            return Ok(());
        }
        self.emit(&frame)
    }

    /// This picture's surface as a DRM-PRIME buffer, if it *is* the shape the
    /// caller's encoder takes -- one composed two-plane layer in at most two
    /// objects, the fourcc and both sizes it asked for. `None` for every other
    /// answer, and the caller then gets the pixels it always got rather than a
    /// failure.
    fn export_dma(&self, frame: &Frame) -> Option<(VhDma, Vec<OwnedFd>)> {
        let want = self.want?;
        let (w, h) = frame.display_size;
        if (w, h) != (want.width, want.height) {
            return None;
        }
        let prime = frame.export_prime().ok()?;
        // The driver describes the NV12 buffer as one layer per plane (R8 and
        // GR88, each one plane into the same object), or as one composed layer
        // holding both: either is the same two planes, in that order.
        let planes: Vec<(u32, u32, u32)> = match prime.layers.as_slice() {
            [layer] => layer.planes.clone(),
            [luma, chroma] => {
                let ([y], [uv]) = (luma.planes.as_slice(), chroma.planes.as_slice()) else {
                    return None;
                };
                vec![*y, *uv]
            }
            _ => return None,
        };
        let [(obj0, off0, pitch0), (obj1, off1, pitch1)] = planes.as_slice() else {
            return None;
        };
        if prime.fourcc != want.fourcc
            || prime.width != want.coded_width
            || prime.height != want.coded_height
            || *obj0 != 0
            || *obj1 > 1
            || prime.objects.len() != *obj1 as usize + 1
        {
            return None;
        }
        let planes = [*obj0, *obj1];
        let (offset, stride) = ([*off0, *off1], [*pitch0, *pitch1]);
        // Whatever the driver says the two planes live in, said back to it
        // unchanged on the import: radeonsi answers with an object per plane,
        // other drivers with one for both, and neither is ours to reinterpret.
        let modifier = prime.objects[0].modifier;
        let mut objects = prime.objects.into_iter();
        // Owned by the caller from here: they travel to the encoder with the
        // descriptor, one per plane object.
        let mut fds = vec![objects.next()?.fd];
        if planes[1] == 1 {
            fds.push(objects.next()?.fd);
        }
        Some((
            VhDma {
                fourcc: prime.fourcc,
                modifier,
                coded_width: prime.width,
                coded_height: prime.height,
                width: w,
                height: h,
                offset,
                stride,
            },
            fds,
        ))
    }

    /// Copies a decoded picture into `self.out` as packed I420.
    fn emit(&mut self, frame: &Frame) -> Result<(), String> {
        // The read-back goes through the driver's image copy (`vaGetImage`)
        // rather than a CPU map of the surface: on a discrete GPU that buffer
        // lives in VRAM, and a CPU read of it runs over the PCIe BAR.
        let i420 = frame.to_i420().map_err(|e| e.to_string())?;
        self.luma = i420.y.len();
        self.chroma = i420.u.len();
        self.out.clear();
        self.out.extend_from_slice(&i420.y);
        self.out.extend_from_slice(&i420.u);
        self.out.extend_from_slice(&i420.v);
        self.meta.width = i420.width;
        self.meta.height = i420.height;
        Ok(())
    }

    /// The picture [`Session::pump`] or [`VpxSession::pump`] just delivered, as
    /// packed I420 (Y, then U, then V), with its width and height.
    fn picture(&self) -> (&[u8], u32, u32) {
        (&self.out, self.meta.width, self.meta.height)
    }
}

/// What the plugin is decoding a file on: the VA-API stateless decoders, or
/// -- for VP8 alone -- libvpx in this plugin's own address space. The split
/// is the codec's, decided once at [`OpenSession::open_at`]: VP8 opens no VA
/// display (this GPU has no VP8 profile to ask it for, and a machine with no
/// render node at all still decodes VP8), and the other four never touch
/// libvpx.
///
/// Everything past the split is written twice -- [`Session`] for the VA-API
/// decoders and [`VpxSession`] for libvpx -- rather than folded into one
/// shape: the halves share the *contract* (open at a sample, pump one
/// picture, read it back) and nothing underneath it, and a trait over
/// "a `vaGetImage`" versus "a memcpy" is the kind of seam that hides which
/// path a measurement ran on.
enum OpenSession {
    Vaapi(Box<Session>),
    Vpx(Box<VpxSession>),
}

impl OpenSession {
    /// Opens `path` for decode, positioned so the first picture handed out is
    /// sample `target_sample` (1-based): decode restarts at the sync sample at
    /// or before it and the pictures in between are dropped unread. The same
    /// landing for both halves; the codec's half is chosen here, where the
    /// container has been read and no device has been opened yet.
    fn open_at(path: &Path, target_sample: u32) -> Option<Self> {
        let (meta, demuxer) = Demuxer::open(path).ok()?;
        let session = match meta.codec {
            Codec::Vp8 => Self::Vpx(Box::new(VpxSession::opened(meta, demuxer)?)),
            _ => {
                // The GPU was reset moments ago (a hung ring: every VA context
                // alive then was reset with it, and Mesa aborts the process on
                // the next submit to one). A session opened now would be one of
                // them, so it is refused and the caller takes the software seat
                // until the grace period is over.
                if ec_hw::gpu_recently_reset() {
                    return None;
                }
                Self::Vaapi(Box::new(Session::opened(meta, demuxer)?))
            }
        };
        // The ABI still speaks 1-based sample ids, the demuxer speaks 0-based
        // display frames -- and answers with a signed one, since a sync sample
        // inside what the edit list trims sits before frame 0.
        let target_frame = target_sample.saturating_sub(1);
        match session {
            Self::Vaapi(mut session) => {
                let first = session.demuxer.seek_to_sync_at_or_before(target_frame);
                session.skip = (i64::from(target_frame) - first).max(0) as u32;
                session.position = first;
                if target_frame > 0 {
                    session.leading = 0;
                }
                Some(Self::Vaapi(session))
            }
            Self::Vpx(mut session) => {
                let first = session.demuxer.seek_to_sync_at_or_before(target_frame);
                session.skip = (i64::from(target_frame) - first).max(0) as u32;
                session.position = first;
                Some(Self::Vpx(session))
            }
        }
    }

    fn meta(&self) -> &VhMeta {
        match self {
            Self::Vaapi(session) => &session.meta,
            Self::Vpx(session) => &session.meta,
        }
    }

    fn seek_to(&mut self, target_sample: u32) -> Result<(), String> {
        match self {
            Self::Vaapi(session) => session.seek_to(target_sample),
            Self::Vpx(session) => session.seek_to(target_sample),
        }
    }

    /// The picture the last successful [`OpenSession::next_frame`] delivered as
    /// read-back I420, with its width and height.
    fn picture(&self) -> (&[u8], u32, u32) {
        match self {
            Self::Vaapi(session) => session.picture(),
            Self::Vpx(session) => session.picture(),
        }
    }

    /// Pumps the next picture in display order. `want` describes what the
    /// caller's encoder can read straight off the GPU; it is only ever honoured
    /// by the VA-API half, and it is cleared again before this returns.
    fn next_frame(&mut self, want: Option<VhDma>) -> Result<bool, String> {
        match self {
            Self::Vaapi(session) => {
                session.want = want;
                let pumped = session.pump();
                session.want = None;
                pumped
            }
            Self::Vpx(session) => session.pump(),
        }
    }

    /// The DRM-PRIME buffer the last [`OpenSession::next_frame`] left on the
    /// GPU, if it did: its descriptor and the descriptors that go with it.
    /// `None` means that picture is in [`OpenSession::picture`] instead.
    fn take_dma(&mut self) -> Option<(VhDma, Vec<OwnedFd>)> {
        match self {
            Self::Vaapi(session) => session.dma.take(),
            Self::Vpx(_) => None,
        }
    }
}

/// One VP8 decode session: the VP8 half of the plugin, the whole of the
/// same job [`Session`] does for the VA-API codecs. No display, no GBM, no
/// surface pool -- `ec-vp8` hands the planes back in memory, and the only
/// shapes worth carrying over from the other half are the ones the caller
/// sees: [`VhMeta`], one picture per `pump`, the same packed layout, the
/// same skip/position arithmetic a seek is.
///
/// VP8 reorders nothing: every access unit fed is *at most* one picture out,
/// in feed order (a hidden altref frame produces none), which is why
/// `position` advances exactly as the VA-API half's does without any of its
/// drain machinery.
struct VpxSession {
    dec: vpx::Decoder,
    demuxer: Demuxer,
    meta: VhMeta,
    flushed: bool,
    /// Pictures still to be decoded and thrown away to land on a seek target,
    /// exactly [`Session`]'s field of the same name.
    skip: u32,
    /// The display frame the next `pump` would hand out -- [`Session`]'s
    /// field, again: one further along for every picture pulled, skipped past
    /// and delivered alike.
    position: i64,
    /// Tightly packed I420 handed out as the picture; reused every
    /// frame, which is exactly why a picture is only valid until the next
    /// pump.
    out: Vec<u8>,
    luma: usize,
    chroma: usize,
}

impl VpxSession {
    fn opened(meta: VideoMeta, demuxer: Demuxer) -> Option<Self> {
        // The decoder cannot refuse to exist any more: it is linked in. The
        // `Option` is the shape [`OpenSession::open_at`] routes on, shared
        // with [`Session::opened`], whose failures are real.
        Some(Self {
            dec: vpx::Decoder::new(),
            demuxer,
            meta: VhMeta {
                width: meta.width,
                height: meta.height,
                frame_rate: meta.frame_rate,
                frame_count: meta.frame_count,
                coded_width: meta.coded_width,
                coded_height: meta.coded_height,
                pixel_aspect_h: meta.pixel_aspect.h,
                pixel_aspect_v: meta.pixel_aspect.v,
            },
            flushed: false,
            skip: 0,
            position: 0,
            out: Vec::new(),
            luma: 0,
            chroma: 0,
        })
    }

    /// Feeds one access unit. VP8 emits at most one picture per unit, so the
    /// pull belongs to the `pump` loop, not here.
    fn decode(&mut self, au: &[u8]) -> Result<(), String> {
        self.dec.decode(au)
    }

    fn pump(&mut self) -> Result<bool, String> {
        loop {
            if self.flushed {
                return Ok(false);
            }
            let au = match self.demuxer.next_access_unit() {
                Ok(Some(au)) => au,
                // End of the track. VP8 holds nothing back -- the flush is the
                // formality [`vpx::Decoder::flush`] documents -- so the next
                // pass finds nothing and ends the stream.
                Ok(None) => {
                    self.dec.flush()?;
                    self.flushed = true;
                    continue;
                }
                Err(e) => return Err(e.to_string()),
            };
            self.decode(&au)?;
            // Every fed unit is one container slot, shown or hidden: a hidden
            // alt-ref frame takes a slot and sends no picture, exactly as the
            // software seat's index counts it. The slot is taken, not peeked --
            // the next fed unit overwrites it, because VP8 queues nothing.
            let shown = self.dec.get_frame();
            self.position += 1;
            // Both the skip and the walk count slots, so a seek that lands
            // past a hidden frame drops the right number of pictures.
            if self.skip > 0 {
                self.skip -= 1;
                continue;
            }
            if let Some(pic) = shown {
                self.emit(&pic);
                return Ok(true);
            }
        }
    }

    /// Copies one decoded I420 picture into `self.out` -- [`Session::emit`]
    /// for a decoder that hands the planes back in memory rather than behind
    /// a `vaGetImage`. VP8 output is always 8-bit 4:2:0, so there is no
    /// 10-bit branch to mirror.
    fn emit(&mut self, pic: &vpx::Picture) {
        let (w, h) = (usize::from(pic.width), usize::from(pic.height));
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if self.luma != w * h || self.chroma != cw * ch {
            self.luma = w * h;
            self.chroma = cw * ch;
            self.out = vec![0u8; self.luma + 2 * self.chroma];
        }
        self.meta.width = u32::from(pic.width);
        self.meta.height = u32::from(pic.height);
        let (dst_y, rest) = self.out.split_at_mut(self.luma);
        let (dst_u, dst_v) = rest.split_at_mut(self.chroma);
        // The planes stride-cover the visible frame by the `Picture`
        // contract (4:2:0, so the chroma planes cover cw x ch), and they are
        // owned by this session -- no lifetime to out-think, no unsafe.
        copy_plane(&pic.y, pic.stride, dst_y, w, h);
        copy_plane(&pic.u, pic.uv_stride, dst_u, cw, ch);
        copy_plane(&pic.v, pic.uv_stride, dst_v, cw, ch);
    }

    /// The picture [`Session::pump`] or [`VpxSession::pump`] just delivered, as
    /// packed I420 (Y, then U, then V), with its width and height.
    fn picture(&self) -> (&[u8], u32, u32) {
        (&self.out, self.meta.width, self.meta.height)
    }

    /// Repositions the session, exactly [`Session::seek_to`]: the same
    /// forward walk within [`FORWARD_SECS`], the same demuxer seek to a sync
    /// sample otherwise -- and for the decoder, a fresh [`vpx::Decoder`]
    /// where the VA-API half pays a flush, because VP8 is stateful to its
    /// last inter frame.
    fn seek_to(&mut self, target_sample: u32) -> Result<(), String> {
        let target_frame = target_sample.saturating_sub(1);
        if let Reposition::Forward(ahead) = reposition(
            self.position,
            i64::from(target_frame),
            self.meta.frame_count,
            forward_limit(self.meta.frame_rate),
        ) {
            self.skip = forward_skip(self.skip, ahead);
            return Ok(());
        }
        // Dropping every reference the decoder holds: the whole of what the
        // FFI seat paid as destroy + re-init is now the assignment of an
        // owned value. What makes it safe is unchanged -- the demuxer's
        // sync-sample seek restarts the feed where the reference chain does.
        self.dec = vpx::Decoder::new();
        self.flushed = false;
        let first = self.demuxer.seek_to_sync_at_or_before(target_frame);
        self.skip = (i64::from(target_frame) - first).max(0) as u32;
        self.position = first;
        Ok(())
    }
}

/// One strided plane down to tightly packed rows: the whole of the read-back
/// a VP8 picture costs, where the VA-API half pays `vaGetImage` for the same.
fn copy_plane(src: &[u8], src_stride: usize, dst: &mut [u8], w: usize, h: usize) {
    for row in 0..h {
        let s = &src[row * src_stride..row * src_stride + w];
        dst[row * w..(row + 1) * w].copy_from_slice(s);
    }
}

/// One row of [`CAP_TABLE`]: the capability bit, its 8-bit and 10-bit decode profiles, and its encode profile.
type CapRow = (u32, &'static [Profile], &'static [Profile], Option<Profile>);

/// What each codec needs of the driver before this plugin can claim it: the
/// 8-bit decode profiles, the 10-bit ones, and the profile the *encoder* here
/// would open -- `None` for the one this plugin has no encoder for at all, so a
/// GPU with a VP9 encode entrypoint is never reported as one edith can
/// reach. The encode profiles are the very ones the seats in [`enc_ec`] and
/// [`EncSession::open_seat`] ask for, which is what keeps this a description of the plugin rather than a
/// second opinion about it.
const CAP_TABLE: [CapRow; 4] = [
    (
        CAP_H264,
        &[
            Profile::H264ConstrainedBaseline,
            Profile::H264Main,
            Profile::H264High,
        ],
        &[],
        Some(Profile::H264Main),
    ),
    (
        CAP_HEVC,
        &[Profile::HEVCMain],
        &[Profile::HEVCMain10],
        // 8-bit Main only: the encoder here opens Main whatever the source was,
        // so a Main 10 entrypoint is a *decode* claim on this line and nothing
        // more. Main 10 encode would be a second profile here and a 10-bit input
        // buffer in `EncSession`, neither of which exists yet.
        Some(Profile::HEVCMain),
    ),
    (
        CAP_VP9,
        &[Profile::VP9Profile0],
        &[Profile::VP9Profile2],
        None,
    ),
    (
        CAP_AV1,
        &[Profile::AV1Profile0],
        // AV1 carries 8- and 10-bit in the same profile 0, so the profile bit
        // is the same one and the P010 read-back is what decides.
        &[Profile::AV1Profile0],
        Some(Profile::AV1Profile0),
    ),
];

/// This machine's decode and encode seats, asked of the driver rather than
/// assumed: which profiles it has, which entrypoints each of those carries, and
/// whether a P010 image exists to read a 10-bit picture back through. Only
/// codecs this plugin actually implements can light up -- `CAP_TABLE` is that
/// half of the intersection.
///
/// VP8 is the one bit asked of nobody: it is this plugin's own native seat
/// (`vpx`, `ec-vp8` linked in), lit unconditionally -- no profile, no
/// entrypoint, no display, no library to load. It rides along only when the
/// display *is* answerable, because this symbol's shape says a displayless
/// machine has nothing to report at all; the decode path itself needs no
/// display for it ([`OpenSession::open_at`] routes VP8 before any device
/// opens).
///
/// `None` when there is no display to ask at all, which is the same "software
/// only" a missing plugin means.
fn query_caps() -> Option<VhCaps> {
    use ec_va::caps::Entrypoint;
    let display = open_decode_display()?;
    let report = ec_va::CapReport::probe(&display).ok()?;
    let p010 = report.image_formats.contains(&VA_FOURCC_P010);
    let decodes = |list: &[Profile]| list.iter().any(|&p| report.supports(p, Entrypoint::VLD));
    let mut caps = VhCaps::default();
    for (bit, eight, ten, encode) in CAP_TABLE {
        if decodes(eight) {
            caps.decode |= bit;
        }
        if p010 && decodes(ten) {
            caps.decode_10bit |= bit;
        }
        // Both entrypoints, exactly as the encoder's own open takes either.
        if encode.is_some_and(|p| {
            report.supports(p, Entrypoint::EncSlice) || report.supports(p, Entrypoint::EncSliceLP)
        }) {
            caps.encode |= bit;
        }
    }
    // The plugin's own software decoder, on its own account rather than the
    // driver's -- the plugin's capability, never a GPU feat. Native now, so
    // the bit has no "when it loads" condition left: it lights on every
    // machine this plugin runs on.
    caps.decode |= CAP_VP8;
    Some(caps)
}

/// How many pictures a group of them holds -- the low-delay predictor's
/// `limit`, which is the *only* thing that decides whether a picture is an IDR.
///
/// One for an intra-only seat, and that is not a tuning: the predictor counts
/// `counter % limit`, so a limit of one pins the counter at zero, every picture
/// takes the keyframe branch with `idr` set, and every access unit carries a
/// fresh SPS and PPS
/// (`cros-codecs/encoder/stateless/predictor.rs:107`, `h264/predictor.rs:139`).
/// Two seconds otherwise, which is what a seek may land on in an ordinary file.
///
/// Forcing a key frame per picture is a different thing and does *not* make an
/// intra file: that path passes `idr = false`, writes no parameter sets, and
/// the slice is an ordinary I picture -- how a 60-picture "all-intra" hardware
/// proxy came out with one sync point in it.
fn gop_limit(framerate: u32, intra_only: bool) -> u16 {
    match intra_only {
        true => 1,
        // Saturating and clamped: the caller pins the rate to 1..240 before it
        // gets here, and a limit that wrapped to zero would divide by zero in
        // the predictor -- a guard costs nothing next to that.
        false => framerate.saturating_mul(2).clamp(1, u16::MAX as u32) as u16,
    }
}

/// Opens the encode seat `open` names, or `None` where the driver has no such
/// entrypoint or refuses the size -- the caller then takes the software encoder.
pub(crate) fn open_encoder(open: &engine::hwproc::Open) -> Option<EcEncSession> {
    use engine::hwproc::Seat;
    let (codec, intra_only, colour) = match open.seat {
        Seat::H264 => (EcCodec::H264, false, None),
        Seat::Intra => (EcCodec::H264, true, None),
        Seat::Av1 => (EcCodec::Av1, false, None),
        Seat::Hevc => {
            let (p, t, m) = open.colour.codes();
            let narrow = |code: u16| u8::try_from(code).unwrap_or(u8::MAX);
            (
                EcCodec::H265,
                true,
                Some((narrow(p), narrow(t), narrow(m), open.colour.full_range)),
            )
        }
    };
    EcEncSession::open(EcOpen {
        codec,
        width: open.width,
        height: open.height,
        fps_num: open.fps_num,
        fps_den: open.fps_den,
        bitrate: open.bitrate,
        intra_only,
        colour,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one number that decides whether a hardware file is intra-only, and
    /// the one this seat got wrong: a proxy asks for a group of one picture, so
    /// every picture is an IDR with its own parameter sets. Needs no GPU --
    /// which is the point, since the seat itself cannot be opened without one.
    #[test]
    fn an_intra_seat_asks_for_a_group_of_one_picture() {
        assert_eq!(gop_limit(30, true), 1, "a proxy is every frame a start");
        assert_eq!(gop_limit(24, true), 1);
        // ...and an ordinary export keeps its two seconds between them.
        assert_eq!(gop_limit(30, false), 60);
        assert_eq!(gop_limit(24, false), 48);
        // A rate no file has, and the clamp that keeps the limit a `u16`
        // rather than a wrapped one: `0` would divide by zero in the predictor.
        assert_eq!(gop_limit(0, false), 1);
        assert_eq!(gop_limit(u32::MAX, false), u16::MAX);
    }

    /// What that group of one writes into the sequence header, which is the
    /// half that shipped broken: the predictor hands the limit straight to
    /// `SpsBuilder::max_frame_num` (`h264/predictor.rs:89`), and `1.ilog2()`
    /// is 0, so the old `0u8 - 4u8` wrapped to 252 in release. Every intra-only
    /// hardware file then carried `log2_max_frame_num_minus4 = 252` -- outside
    /// the spec's 0..=12 -- and no decoder would parse it. Zero is the right
    /// answer, not merely a legal one: `frame_num` never leaves 0 when every
    /// picture is an IDR. No GPU: this is the bitstream side of the seat.
    #[test]
    fn an_intra_seat_writes_a_sequence_header_a_decoder_can_read() {
        // The limit the intra seat asks for is one picture, and the SPS the
        // encoder writes for it must declare the smallest frame_num the spec
        // allows (`minus4 = 0`). A wrap would read as 252 here.
        assert_eq!(gop_limit(30, true), 1);
        let open = EcOpen {
            codec: EcCodec::H264,
            width: 320,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            bitrate: 2_000_000,
            intra_only: true,
            colour: None,
        };
        // No VA-API on this machine: nothing to encode, nothing to claim.
        let Some(mut session) = EcEncSession::open(open) else {
            return;
        };
        let luma = vec![128u8; 320 * 240];
        let chroma = vec![128u8; 160 * 120];
        session
            .encode(&luma, &chroma, &chroma, false)
            .expect("the intra seat encodes a picture");
        let au = session.next_au().expect("one access unit").to_vec();
        let au = au.as_slice();
        assert_eq!(
            sps_log2_max_frame_num_minus4(au),
            Some(0),
            "log2_max_frame_num_minus4 = 0; 252 is what a wrap looks like"
        );
    }

    /// `log2_max_frame_num_minus4` from the first SPS in an Annex-B access unit.
    /// Main profile only (no chroma-format fields between the level and the
    /// sequence parameter set id), which is the only profile this seat writes.
    fn sps_log2_max_frame_num_minus4(au: &[u8]) -> Option<u32> {
        let at = au.windows(4).position(|w| w == [0, 0, 1, 0x67])?;
        // Undo emulation prevention: 0x00 0x00 0x03 -> 0x00 0x00.
        let mut rbsp = Vec::new();
        let mut zeros = 0;
        for &b in &au[at + 4..] {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            if zeros > 2 && b == 1 {
                break;
            }
            rbsp.push(b);
        }
        let bits: Vec<u8> = rbsp
            .iter()
            .flat_map(|b| (0..8).rev().map(move |i| (b >> i) & 1))
            .collect();
        fn read(bits: &[u8], pos: &mut usize, n: usize) -> Option<u32> {
            let mut v = 0;
            for _ in 0..n {
                v = v << 1 | u32::from(*bits.get(*pos)?);
                *pos += 1;
            }
            Some(v)
        }
        fn ue(bits: &[u8], pos: &mut usize) -> Option<u32> {
            let mut zeros = 0;
            while read(bits, pos, 1)? == 0 {
                zeros += 1;
            }
            Some((1u32 << zeros) - 1 + read(bits, pos, zeros)?)
        }
        let mut pos = 0;
        if read(&bits, &mut pos, 8)? != 77 {
            return None;
        }
        read(&bits, &mut pos, 16)?; // constraint flags, level_idc
        ue(&bits, &mut pos)?; // seq_parameter_set_id
        ue(&bits, &mut pos)
    }

    /// The other half of the same fault: every picture of an intra-only seat
    /// is an IDR, and each access unit carries its own SPS, so no decoder can
    /// read a later picture as a non-sync one. Two pictures, each checked for a
    /// parameter set and an IDR slice and no non-IDR slice. Needs a GPU.
    #[test]
    fn every_intra_only_access_unit_is_an_idr_with_parameter_sets() {
        let open = EcOpen {
            codec: EcCodec::H264,
            width: 320,
            height: 240,
            fps_num: 30,
            fps_den: 1,
            bitrate: 2_000_000,
            intra_only: true,
            colour: None,
        };
        let Some(mut session) = EcEncSession::open(open) else {
            return;
        };
        let luma = vec![128u8; 320 * 240];
        let chroma = vec![128u8; 160 * 120];
        for picture in 0..2 {
            session
                .encode(&luma, &chroma, &chroma, false)
                .expect("the intra seat encodes a picture");
            let au = session.next_au().expect("one access unit").to_vec();
            let au = au.as_slice();
            let kinds: Vec<u8> = au
                .windows(4)
                .filter(|w| w[..3] == [0, 0, 1])
                .map(|w| w[3] & 0x1f)
                .collect();
            assert!(
                kinds.contains(&7),
                "picture {picture} carries an SPS: {kinds:?}"
            );
            assert!(kinds.contains(&5), "picture {picture} is an IDR: {kinds:?}");
            assert!(
                !kinds.contains(&1),
                "picture {picture} has no non-IDR slice: {kinds:?}"
            );
        }
    }

    /// Runs against whatever GPU this machine has -- and passes on one with no
    /// VA-API at all, which is exactly what `None` means here. What it pins is
    /// the *shape* of the answer, the contents being the driver's: nothing may
    /// be claimed that this plugin cannot actually reach.
    #[test]
    fn the_caps_query_never_claims_more_than_this_plugin_implements() {
        let Some(caps) = query_caps() else {
            return;
        };
        eprintln!("{caps:?}");
        // Encode is this plugin's three seats and no others, whatever
        // entrypoints the driver has: a VP9 EncSlice is not an encoder here.
        assert_eq!(
            caps.encode & !(CAP_H264 | CAP_HEVC | CAP_AV1),
            0,
            "{caps:?}"
        );
        // A 10-bit claim is a decode claim -- same profile family, read back
        // through P010 -- and there is no 10-bit encode path at all.
        assert_eq!(caps.decode_10bit & !caps.decode, 0, "{caps:?}");
        // H.264 here is 8-bit 4:2:0; the table has no 10-bit profile for it.
        assert_eq!(caps.decode_10bit & CAP_H264, 0, "{caps:?}");
    }

    /// Every shape of the forward-or-flush decision a reposition makes, with
    /// no GPU in sight: the boundary a silence cut leaves is a target a few
    /// frames *ahead* of the decoder and that is the walk; behind it, past the
    /// reach, and past the end of the stream are the flush.
    #[test]
    fn a_target_ahead_within_the_reach_walks_forward() {
        use Reposition::{Flush, Forward};
        // A 30 fps stream, so the reach is [`FORWARD_SECS`] of it.
        let (count, limit) = (10_000u32, forward_limit(30.0));
        assert_eq!(forward_limit(30.0), 15);
        // The boundary a 0.3 s silence cut leaves: 9 frames ahead, dropped
        // unread on the way to exactly the target.
        assert_eq!(reposition(120, 129, count, limit), Forward(9));
        // Exactly at the reach, and a reseek to exactly where the decoder
        // stands (a superseded span's landing): nothing to do but not flush.
        assert_eq!(reposition(120, 135, count, limit), Forward(15));
        assert_eq!(reposition(120, 120, count, limit), Forward(0));
        // One frame past either end of the walk is the flush's.
        assert_eq!(reposition(120, 136, count, limit), Flush);
        assert_eq!(reposition(120, 119, count, limit), Flush);
        assert_eq!(reposition(120, 0, count, limit), Flush);
        // The last frame of the stream is a frame like any other...
        assert_eq!(reposition(9_995, 9_999, count, limit), Forward(4));
        // ...while a target at or past the end is not one decoding forward
        // can hand out: EOF falls back to the seek that always handled it.
        assert_eq!(reposition(9_995, 10_000, count, limit), Flush);
        assert_eq!(reposition(9_995, 60_000, count, limit), Flush);
        // From the end itself -- everything delivered, `pump` at clean EOF --
        // even a target inside the stream is behind.
        assert_eq!(reposition(10_000, 9_999, count, limit), Flush);
    }

    /// The reach is time, carried at the stream's own rate, and a stream that
    /// states no rate gets none of it: such a session keeps the behaviour
    /// every reposition had, rather than a skip of nonsense frames.
    #[test]
    fn the_reach_is_half_a_second_at_the_streams_own_rate() {
        assert_eq!(forward_limit(23.976), 12);
        assert_eq!(forward_limit(24.0), 12);
        assert_eq!(forward_limit(60.0), 30);
        assert_eq!(forward_limit(0.0), 0);
        assert_eq!(forward_limit(f64::NAN), 0);
        assert_eq!(forward_limit(-30.0), 0);
    }

    /// The drain machine's one law, held across *stacked* repositions: the
    /// picture delivered is `position + skip`, `position` advancing one per
    /// handle popped ([`Session::pump`]). A second forward reseek before the
    /// first's skip has drained must land on its own target -- the skip is
    /// replaced ([`forward_skip`]), never added to, or every reseek inside
    /// another's window walks its predecessor's frames past the target and
    /// the pictures keep arriving, only wrong.
    #[test]
    fn stacked_forward_reseeks_land_exactly_on_their_target() {
        use Reposition::Forward;
        let (count, limit) = (10_000u32, forward_limit(30.0));
        let land = |position: i64, skip: u32| position + i64::from(skip);
        let (mut position, mut skip) = (100i64, 0u32);
        let Forward(ahead) = reposition(position, 110, count, limit) else {
            panic!("110 is within the reach of 100")
        };
        skip = forward_skip(skip, ahead);
        let Forward(ahead) = reposition(position, 114, count, limit) else {
            panic!("114 is within the reach of 100")
        };
        skip = forward_skip(skip, ahead);
        assert_eq!(land(position, skip), 114, "the second target, exactly");

        // A flush whose RAP skip is still undrained when a forward span is
        // asked: the flush stood at its RAP with `skip` toward its own
        // target, and the forward reseek through that window replaces it.
        position = 200;
        skip = 12;
        let Forward(ahead) = reposition(position, 208, count, limit) else {
            panic!("208 is within the reach of 200")
        };
        skip = forward_skip(skip, ahead);
        assert_eq!(
            land(position, skip),
            208,
            "a flush's undrained skip does not compound"
        );

        // ...and a reseek to exactly where the decoder stands cancels
        // whatever was standing: the next handle is shown, not skipped past.
        let Forward(ahead) = reposition(position, position, count, limit) else {
            panic!("a session's own position is within its reach")
        };
        skip = forward_skip(skip, ahead);
        assert_eq!((ahead, land(position, skip)), (0, position));
    }
}
