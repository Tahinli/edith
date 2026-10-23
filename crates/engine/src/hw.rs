//! Hardware decode and encode, held by a child process.
//!
//! The VA-API drivers live in `hw-child`, a binary of the `engine-hw` package
//! that is started next to the editor's own executable the same way the editor
//! finds its other helpers ([`crate::hwproc`]). This module is the parent side:
//! a session is a child plus a socket, and every call is one request and one
//! reply. Nothing here links a driver and nothing here is `unsafe`.
//!
//! Anything that goes wrong -- child missing, no VA-API runtime, no render node,
//! unsupported stream, a child that stops answering or dies -- leaves us with
//! `None` or an `Err`, and the caller falls back to the software codec exactly
//! as it always did. A dead child is reaped at once and the next open starts a
//! fresh one.
//!
//! Pictures in memory cross in a shared memfd, read out with one `pread`.
//! Pictures still on the GPU cross as DRM-PRIME descriptors over `SCM_RIGHTS`
//! and are never copied through the socket.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::process::Child;
use std::sync::OnceLock;

use crate::colorspace::ColorDescription;
use crate::hwproc::{
    DMA_BODY, HW_LOST, K_DMA, K_PIXELS, Lost, Open, R_AU, R_ERR, R_NONE, Remote, Seat, Shm, T_NEXT,
    T_NEXT_DMA, T_SEEK, recv_reply, send_msg, spawn_child,
};

#[cfg(test)]
thread_local! {
    /// How many times this thread asked for a decoder. The seam the tests read
    /// to show a file was never offered to the GPU, which stderr cannot show
    /// in-process.
    pub(crate) static OPEN_ATTEMPTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// One picture as the caller reads it: the Y, U and V planes, then the width
/// and height they are laid out at.
type Planes<'a> = (&'a [u8], &'a [u8], &'a [u8], u32, u32);

/// Stream metadata. The picture half of [`crate::VideoMeta`]: the four fields a
/// session was opened with, plus the coded pair and the sample aspect a display
/// size can differ from the coded one by.
///
/// `width`/`height` are the *displayed* shape, exactly as [`crate::VideoMeta`]
/// promises. `coded_width`/`coded_height` name the stored pair explicitly, and
/// `pixel_aspect_h`/`_v` carry the ratio one coded pixel is drawn at; zero and
/// zero reads square.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct VhMeta {
    pub width: u32,
    pub height: u32,
    pub frame_rate: f64,
    pub frame_count: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub pixel_aspect_h: u32,
    pub pixel_aspect_v: u32,
}

/// Bytes of [`VhMeta`] on the wire: eight 4-byte fields, the frame rate as its
/// bit pattern in 8 bytes.
pub const META_WIRE: usize = 36;

/// [`VhMeta`] as the child sends it at open.
pub fn meta_to_wire(meta: &VhMeta) -> [u8; META_WIRE] {
    let mut out = [0u8; META_WIRE];
    out[0..4].copy_from_slice(&meta.width.to_le_bytes());
    out[4..8].copy_from_slice(&meta.height.to_le_bytes());
    out[8..16].copy_from_slice(&meta.frame_rate.to_bits().to_le_bytes());
    out[16..20].copy_from_slice(&meta.frame_count.to_le_bytes());
    out[20..24].copy_from_slice(&meta.coded_width.to_le_bytes());
    out[24..28].copy_from_slice(&meta.coded_height.to_le_bytes());
    out[28..32].copy_from_slice(&meta.pixel_aspect_h.to_le_bytes());
    out[32..36].copy_from_slice(&meta.pixel_aspect_v.to_le_bytes());
    out
}

/// The other half of [`meta_to_wire`]; `None` for anything of the wrong size.
pub fn meta_from_wire(bytes: &[u8]) -> Option<VhMeta> {
    if bytes.len() != META_WIRE {
        return None;
    }
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    Some(VhMeta {
        width: u32_at(0)?,
        height: u32_at(4)?,
        frame_rate: f64::from_bits(u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?)),
        frame_count: u32_at(16)?,
        coded_width: u32_at(20)?,
        coded_height: u32_at(24)?,
        pixel_aspect_h: u32_at(28)?,
        pixel_aspect_v: u32_at(32)?,
    })
}

/// What the driver says this machine decodes and encodes: one bit per codec
/// ([`CAP_H264`] and its neighbours) in each mask.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhCaps {
    pub decode: u32,
    pub encode: u32,
    /// Codecs whose 10-bit profiles decode.
    pub decode_10bit: u32,
}

pub const CAP_H264: u32 = 1 << 0;
pub const CAP_HEVC: u32 = 1 << 1;
pub const CAP_VP9: u32 = 1 << 2;
pub const CAP_AV1: u32 = 1 << 3;

/// VP8 decodes on `ec-vp8` inside the child -- software, no GPU and no VA-API
/// profile anywhere in it -- so this bit is the child's seat, never the
/// driver's, and it lights up [`VhCaps::decode`] unconditionally. The hardware
/// line in [`crate::caps`] deliberately keeps it off.
pub const CAP_VP8: u32 = 1 << 4;

/// A picture's DRM-PRIME description, without its file descriptors (those
/// travel beside it in a [`DmaFrame`]): one object per plane, luma first, or one
/// object for both.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct VhDma {
    /// `NV12` and nothing else, today.
    pub fourcc: u32,
    /// DRM format modifier of the buffer, passed through to the import.
    pub modifier: u64,
    /// The surface's own size, rounded up to what the codec codes in.
    pub coded_width: u32,
    pub coded_height: u32,
    /// The picture inside it.
    pub width: u32,
    pub height: u32,
    /// Luma then chroma, each inside its own object.
    pub offset: [u32; 2],
    pub stride: [u32; 2],
}

/// One decoded picture left on the GPU, with the descriptors owned: dropping it
/// closes them. The decode child keeps the buffer out of its pool for a few
/// pictures only (see `DMA_HOLD` in engine-hw), so encode it before asking for
/// several more.
#[derive(Debug)]
pub struct DmaFrame {
    pub desc: VhDma,
    /// One per plane object, luma first.
    pub fds: Vec<OwnedFd>,
}

impl DmaFrame {
    pub fn width(&self) -> u32 {
        self.desc.width
    }

    pub fn height(&self) -> u32 {
        self.desc.height
    }
}

/// Bytes of one packed I420 picture of this size.
fn i420_bytes(width: u32, height: u32) -> usize {
    let (w, h) = (width as usize, height as usize);
    w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))
}

/// Reads a `u32` little-endian at `at`, or `None` past the end.
fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The one-time capability query, asked of a child that answers and exits.
/// `None` on every way there is no answer -- no child, no driver, a child that
/// refuses -- and a caller says "software only" for all of them.
///
/// VP8 sits outside that reading on a driverless machine: its decoder is inside
/// the child, so a VP8 file may open where this answers `None`.
///
/// Costs one VA-API init (~90 ms) the first time: ask it off a render thread.
pub fn caps() -> Option<VhCaps> {
    static CAPS: OnceLock<Option<VhCaps>> = OnceLock::new();
    *CAPS.get_or_init(query_caps)
}

fn query_caps() -> Option<VhCaps> {
    let (mut child, mut sock) = spawn_child(&[OsString::from("caps")])?;
    let mut body = Vec::new();
    let answer = match recv_reply(&mut sock, &mut body) {
        Ok((R_AU, _)) => Some(VhCaps {
            decode: le_u32(&body, 0)?,
            encode: le_u32(&body, 4)?,
            decode_10bit: le_u32(&body, 8)?,
        }),
        _ => None,
    };
    reap(&mut child);
    answer
}

/// Kills and reaps a child. Kills first: a child may be stuck in a driver
/// rather than dead, and a stuck child is never waited for.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Picture count at which new sessions inject a hardware error; negative for
/// none. Read once per [`HwSession::open_at`].
static FAIL_AFTER: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

/// Test hook: sessions opened from now on report a hardware error after
/// `pictures` pictures (counted from the open, or the last seek), as a hung or
/// reset GPU context would. `None` switches it off. `EC_HW_FAIL_AT=N` does the
/// same for a whole process.
#[doc(hidden)]
pub fn inject_failure_after(pictures: Option<u64>) {
    let value = pictures.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
    FAIL_AFTER.store(value, std::sync::atomic::Ordering::SeqCst);
}

fn injected_failure() -> Option<u64> {
    match FAIL_AFTER.load(std::sync::atomic::Ordering::SeqCst) {
        n if n >= 0 => Some(n as u64),
        _ => crate::envcfg::var("EC_HW_FAIL_AT")?.parse().ok(),
    }
}

/// What one decode request handed back.
enum Fetched {
    /// The picture is in the session's pixel buffer.
    Pixels,
    /// The picture is still on the GPU.
    Dma(DmaFrame),
}

/// An open hardware decode session, in a child process of its own. Dropping it
/// kills the child.
pub struct HwSession {
    child: Child,
    sock: std::os::unix::net::UnixStream,
    /// The staging buffer pictures in memory are read out of. Its capacity is
    /// the child's own, so a frame that does not fit comes with a new one.
    shm: Shm,
    meta: VhMeta,
    /// The last reply's body.
    body: Vec<u8>,
    /// The last picture read out of `shm`, packed I420.
    pixels: Vec<u8>,
    /// Set the moment the child is known gone, so every later call refuses in
    /// the same words instead of waiting on a socket nobody holds.
    lost: Option<String>,
    /// Test hook: the number of pictures after which `next_frame*` reports a
    /// hardware error. Set by [`inject_failure_after`] or `EC_HW_FAIL_AT`.
    fail_after: Option<u64>,
    delivered: u64,
}

impl HwSession {
    /// Probes the child against this very file: success means the driver, the
    /// render node and the stream's profile are all usable.
    pub fn open(path: &Path) -> Option<Self> {
        Self::open_at(path, 0)
    }

    /// As [`HwSession::open`], but the first [`HwSession::next_frame`] returns
    /// display index `start_frame`; earlier pictures are decoded and dropped
    /// inside the child, without the read-back copy.
    pub fn open_at(path: &Path, start_frame: u32) -> Option<Self> {
        #[cfg(test)]
        OPEN_ATTEMPTS.with(|n| n.set(n.get() + 1));
        // Sample ids are 1-based, frame indices 0-based.
        let target_sample = start_frame.saturating_add(1);
        let args = [
            OsString::from("decode"),
            path.as_os_str().to_owned(),
            OsString::from(target_sample.to_string()),
        ];
        let (mut child, mut sock) = spawn_child(&args)?;
        let mut body = Vec::new();
        let opened = match recv_reply(&mut sock, &mut body) {
            Ok((R_AU, mut fds)) if fds.len() == 1 => {
                meta_from_wire(&body).map(|meta| (meta, fds.remove(0)))
            }
            // A child that said no, or died opening the driver, is no session.
            _ => None,
        };
        let Some((meta, fd)) = opened else {
            reap(&mut child);
            return None;
        };
        Some(Self {
            child,
            sock,
            shm: Shm::from_fd(fd, i420_bytes(meta.width, meta.height)),
            meta,
            body,
            pixels: Vec::new(),
            lost: None,
            fail_after: injected_failure(),
            delivered: 0,
        })
    }

    /// Repositions this very session so the next [`HwSession::next_frame`]
    /// returns display index `start_frame`, without a new child and its driver
    /// initialisation. `false` when the child cannot do it, and the caller then
    /// opens a new session as it always did. The session is left usable either
    /// way.
    pub fn seek(&mut self, start_frame: u32) -> bool {
        // Sample ids are 1-based, frame indices 0-based -- as in `open_at`.
        let target_sample = start_frame.saturating_add(1);
        let moved = matches!(self.call(T_SEEK, &target_sample.to_le_bytes()), Ok((R_AU, _)));
        if moved {
            self.delivered = 0;
        }
        moved
    }

    /// The injected error, once `fail_after` pictures have been delivered.
    fn inject(&mut self) -> crate::Result<()> {
        if let Some(limit) = self.fail_after {
            if self.delivered >= limit {
                return Err(format!("injected hardware error after {limit} pictures").into());
            }
            self.delivered += 1;
        }
        Ok(())
    }

    pub fn meta(&self) -> Option<VhMeta> {
        Some(self.meta)
    }

    /// Next picture in display order as tightly packed I420 `(y, u, v)`.
    /// `Ok(None)` is clean end of stream.
    pub fn next_frame(&mut self) -> crate::Result<Option<Planes<'_>>> {
        self.inject()?;
        match self.fetch(T_NEXT, &[])? {
            None => Ok(None),
            Some(Fetched::Pixels) => Ok(Some(self.planes())),
            Some(Fetched::Dma(_)) => Err("the decoder handed over a GPU buffer nobody asked for".into()),
        }
    }

    /// The next picture *without reading it back* where the caller's encoder can
    /// take the buffer the decoder wrote into -- which is what `want` describes.
    /// Anything the child cannot hand over that way comes back as pixels
    /// instead ([`HwPicture::Pixels`]), so a mismatch costs the read-back it
    /// always cost and never a frame.
    ///
    /// The buffer behind a [`DmaFrame`] is the decoder's, held out of its pool
    /// for a few pictures only: encode it before asking this for a few more.
    pub fn next_frame_dma(&mut self, want: DmaWant) -> crate::Result<Option<HwPicture<'_>>> {
        self.inject()?;
        let mut body = Vec::with_capacity(16);
        for value in [want.coded_width, want.coded_height, want.width, want.height] {
            body.extend_from_slice(&value.to_le_bytes());
        }
        match self.fetch(T_NEXT_DMA, &body)? {
            None => Ok(None),
            Some(Fetched::Dma(frame)) => Ok(Some(HwPicture::Dma(frame))),
            Some(Fetched::Pixels) => {
                let (y, u, v, width, height) = self.planes();
                Ok(Some(HwPicture::Pixels(y, u, v, width, height)))
            }
        }
    }

    /// One decode request and its reply: `None` at end of stream, otherwise
    /// where the picture is.
    fn fetch(&mut self, request: u8, body: &[u8]) -> crate::Result<Option<Fetched>> {
        let (tag, mut fds) = self.call(request, body)?;
        if tag == R_NONE {
            return Ok(None);
        }
        let reply = self.body.clone();
        match reply.first().copied() {
            Some(K_PIXELS) if tag == R_AU => {
                let bad = || -> crate::Error { "the decoder sent a short picture header".into() };
                let width = le_u32(&reply, 1).ok_or_else(bad)?;
                let height = le_u32(&reply, 5).ok_or_else(bad)?;
                let needed = i420_bytes(width, height);
                if let Some(fd) = fds.pop() {
                    // The child made a bigger staging buffer for this frame.
                    self.shm = Shm::from_fd(fd, needed);
                }
                if needed > self.shm.capacity() {
                    return Err(format!("a {width}x{height} picture does not fit the staging buffer").into());
                }
                self.shm
                    .read_into(&mut self.pixels, needed)
                    .map_err(|e| format!("cannot read the staging buffer: {e}"))?;
                self.meta.width = width;
                self.meta.height = height;
                Ok(Some(Fetched::Pixels))
            }
            Some(K_DMA) if tag == R_AU => {
                let desc = dma_from_wire(&reply).ok_or("the decoder sent a short GPU buffer header")?;
                let count = usize::from(reply[DMA_BODY - 1]);
                if fds.len() != count || !(1..=2).contains(&count) {
                    return Err("the decoder's GPU buffer came without its descriptors".into());
                }
                self.meta.width = desc.width;
                self.meta.height = desc.height;
                Ok(Some(Fetched::Dma(DmaFrame { desc, fds })))
            }
            _ => Err(format!("unexpected reply {tag} from the decoder").into()),
        }
    }

    /// The picture last read into `pixels`, as planes of the current size.
    fn planes(&self) -> Planes<'_> {
        let (w, h) = (self.meta.width as usize, self.meta.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (y, rest) = self.pixels.split_at(w * h);
        let (u, rest) = rest.split_at(cw * ch);
        (y, u, &rest[..cw * ch], self.meta.width, self.meta.height)
    }

    /// One request and its reply on this session's socket. A child that does
    /// not answer, or is gone, is reaped here and every later call refuses.
    fn call(&mut self, tag: u8, body: &[u8]) -> crate::Result<(u8, Vec<OwnedFd>)> {
        if let Some(gone) = &self.lost {
            return Err(Lost(gone.clone()).into());
        }
        let mut msg = Vec::with_capacity(1 + body.len());
        msg.push(tag);
        msg.extend_from_slice(body);
        if let Err(e) = send_msg(&self.sock, &msg, &[]) {
            return Err(self.bury(&e.to_string()));
        }
        match recv_reply(&mut self.sock, &mut self.body) {
            Ok((R_ERR, _)) => Err(String::from_utf8_lossy(&self.body).into_owned().into()),
            Ok(answer) => Ok(answer),
            Err(e) => Err(self.bury(&e.to_string())),
        }
    }

    fn bury(&mut self, why: &str) -> crate::Error {
        reap(&mut self.child);
        let message = format!("{HW_LOST} stopped answering ({why}) and was killed; the driver took it down and not the editor");
        self.lost = Some(message.clone());
        Lost(message).into()
    }
}

/// The `K_DMA` header after its kind byte.
fn dma_from_wire(body: &[u8]) -> Option<VhDma> {
    let at = 1;
    Some(VhDma {
        fourcc: le_u32(body, at)?,
        modifier: u64::from_le_bytes(body.get(at + 4..at + 12)?.try_into().ok()?),
        coded_width: le_u32(body, at + 12)?,
        coded_height: le_u32(body, at + 16)?,
        width: le_u32(body, at + 20)?,
        height: le_u32(body, at + 24)?,
        offset: [le_u32(body, at + 28)?, le_u32(body, at + 32)?],
        stride: [le_u32(body, at + 36)?, le_u32(body, at + 40)?],
    })
}

impl Drop for HwSession {
    fn drop(&mut self) {
        reap(&mut self.child);
    }
}

/// What an encoder needs a decoded buffer to look like before it can read it
/// without a copy: the surface's coded size and the picture inside it.
#[derive(Clone, Copy)]
pub struct DmaWant {
    pub coded_width: u32,
    pub coded_height: u32,
    pub width: u32,
    pub height: u32,
}

/// One picture out of [`HwSession::next_frame_dma`]: still on the GPU, or read
/// back like every other frame.
pub enum HwPicture<'a> {
    Pixels(&'a [u8], &'a [u8], &'a [u8], u32, u32),
    Dma(DmaFrame),
}

/// An open hardware encode session, in a child process of its own. See
/// [`crate::hwproc`] for why the driver is never in the editor's address space.
///
/// The API is the one it always was, call for call, so nothing in the export
/// loop knows which side of a process boundary the encoder is on.
pub struct HwEncoder(Remote);

impl HwEncoder {
    /// H.264 at `width`x`height`, `fps_num / fps_den` frames per second, coded
    /// at a constant `bitrate` bits per second. `None` whenever hardware encode
    /// is unavailable -- no child binary, no driver, dimensions the driver
    /// refuses, *or a child that died opening any of it* -- and the caller falls
    /// back to the software encoder.
    pub fn open(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        Self::seated(
            Seat::H264,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            ColorDescription::default(),
        )
    }

    /// The same H.264 seat, **intra-only**: every picture comes back an IDR, so
    /// every frame of the file is one a decoder may be started from. What
    /// [`crate::proxy`] codes a stand-in with. `None` on a child that cannot
    /// make that seat, and the caller must then take the software encoder rather
    /// than the ordinary hardware seat.
    pub fn open_intra(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        Self::seated(
            Seat::Intra,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            ColorDescription::default(),
        )
    }

    /// The same, coding AV1 instead. `None` on a child or a GPU with no AV1
    /// encoder, and the caller's software AV1 encoder takes the export without
    /// saying anything about it.
    pub fn open_av1(width: u32, height: u32, fps_num: u32, fps_den: u32, bitrate: u64) -> Option<Self> {
        Self::seated(
            Seat::Av1,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            ColorDescription::default(),
        )
    }

    /// The same, coding HEVC. `colour` is what the SPS will say the samples
    /// mean, and it is taken at the open because the parameter sets are written
    /// when the sequence starts.
    pub fn open_hevc(
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
        colour: ColorDescription,
    ) -> Option<Self> {
        Self::seated(Seat::Hevc, width, height, fps_num, fps_den, bitrate, colour)
    }

    fn seated(
        seat: Seat,
        width: u32,
        height: u32,
        fps_num: u32,
        fps_den: u32,
        bitrate: u64,
        colour: ColorDescription,
    ) -> Option<Self> {
        let open = Open {
            seat,
            width,
            height,
            fps_num,
            fps_den,
            bitrate,
            colour,
        };
        // No child binary beside the executable means no hardware seat at all:
        // `Remote::spawn` answers `None` before a process is forked.
        Remote::spawn(open).map(Self)
    }

    /// Feeds one tightly packed I420 picture. `Ok(None)` means the encoder has
    /// not finished an access unit yet; the returned bytes are Annex-B and stay
    /// valid until the next call on this session.
    pub fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
        force_key: bool,
    ) -> crate::Result<Option<&[u8]>> {
        // Checked on this side of the boundary: a caller's mistake is a caller's
        // mistake, and the child must not turn one into a message about a dead
        // child.
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
            return Err("encode input is not a packed I420 frame".into());
        }
        self.0.encode(y, u, v, width, height, force_key)
    }

    /// What a decoded buffer must look like to reach this encoder without a
    /// copy, or `None` where the child has no zero-copy door at all.
    pub fn dma_want(&mut self, width: u32, height: u32) -> Option<DmaWant> {
        let (coded_width, coded_height) = self.0.dma_geometry()?;
        Some(DmaWant {
            coded_width,
            coded_height,
            width,
            height,
        })
    }

    /// Feeds one picture the decoder left on the GPU. The descriptors go over
    /// with the message, so the buffer itself never moves.
    pub fn encode_dma(&mut self, dma: &DmaFrame, force_key: bool) -> crate::Result<Option<&[u8]>> {
        self.0.encode_dma(dma, force_key)
    }

    /// Flushes the encoder; call until it returns `Ok(None)`.
    pub fn drain(&mut self) -> crate::Result<Option<&[u8]>> {
        self.0.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The meta block survives the wire, frame rate's bit pattern included.
    #[test]
    fn meta_round_trips_through_the_wire() {
        let meta = VhMeta {
            width: 1920,
            height: 1080,
            frame_rate: 30000.0 / 1001.0,
            frame_count: 4321,
            coded_width: 1920,
            coded_height: 1088,
            pixel_aspect_h: 4,
            pixel_aspect_v: 3,
        };
        assert_eq!(meta_from_wire(&meta_to_wire(&meta)), Some(meta));
        assert_eq!(meta_from_wire(&[0u8; 35]), None);
    }

    #[test]
    fn i420_size_rounds_chroma_up() {
        assert_eq!(i420_bytes(3, 3), 9 + 2 * 4);
        assert_eq!(i420_bytes(1920, 1080), 1920 * 1080 * 3 / 2);
    }
}
