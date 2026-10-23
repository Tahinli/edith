//! The hardware encoder, held in a **child process**.
//!
//! Every entry point of the plugin catches unwinds ([`crate::hw`]), and that
//! guarantee has a floor it cannot reach: Mesa's VA-API drivers run their own C
//! worker threads, and `radeonsi` calls libc `abort()` from
//! `amdgpu_ctx_set_sw_reset_status` when the video ring resets under it
//! (measured 2026-08-13, mesa 26.1.6, `vcn_unified_0` hung during an AV1 encode
//! submission). A foreign thread's `abort()` is not an unwind and no
//! `catch_unwind` in this address space can see it, so the editor died with the
//! driver and took the export -- and the session -- with it.
//!
//! So the driver is not in this address space any more. `HwEncoder` here is a
//! handle on a child running `hw-encode-child`, which is the one process that
//! `dlopen`s the plugin and therefore the only one a driver may kill. When it
//! dies -- by `abort()`, by any signal, or by going quiet past
//! [`Remote::deadline`] -- the parent sees a closed socket or a timed-out read,
//! reaps the child, and reports a [`Lost`]. The export then
//! runs again on the software encoder ([`crate::export::start`]), or refuses in
//! words where a person picked the GPU by name.
//!
//! Two things cross between the processes and neither is a picture copied down a
//! pipe. Pictures that live in memory are written into a shared anonymous file
//! (`memfd`, which both sides read and write through their own descriptor), so a
//! frame costs one write and a read and a 10-byte message. Pictures still on the
//! GPU never move at all: the DRM-PRIME descriptors travel as file descriptors
//! over `SCM_RIGHTS`, so the zero-copy export path stays zero-copy across the
//! process boundary.
//!
//! Nothing here is `unsafe`: the memfd and the descriptor passing go through
//! `rustix` and `nix`, and the staging buffer is file I/O rather than a mapping.

use std::ffi::OsString;
use std::fs::File;
use std::io::{IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use nix::sys::socket::{ControlMessage, MsgFlags, UnixAddr, sendmsg};
use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};
use rustix::process::{Signal, set_parent_process_death_signal};

use crate::colorspace::{ColorDescription, Matrix, Transfer};
use crate::hw::{DmaFrame, VhDma};

/// The words every "the child is gone" error carries, for the person reading
/// the log; what *decides* anything is [`Lost`], never this text.
pub const HW_LOST: &str = "the hardware encoder process";

/// The error a dead child carries.
///
/// A type and not a phrase. [`is_lost`] is what commits a whole export to being
/// written again from the first frame ([`crate::export::start`]), and matching
/// on words handed that decision to any future error whose message happened to
/// mention the child -- a silent re-encode of a feature film, caused by a
/// wording. Nothing outside this module can make one.
#[derive(Debug)]
pub struct Lost(pub(crate) String);

impl std::fmt::Display for Lost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Lost {}

/// Whether an error is one: a driver that took its process down, rather than a
/// picture this engine got wrong.
pub fn is_lost(error: &crate::Error) -> bool {
    error.downcast_ref::<Lost>().is_some()
}

/// The child's file name. It is a binary of the `engine-hw` package, built into
/// the same directory as the editor, and it is the only process that opens a
/// video driver.
const BIN: &str = "hw-child";

/// Which encoder the child opens. Sent as argv, so the child needs no protocol
/// before it has one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Seat {
    H264,
    Intra,
    Av1,
    Hevc,
}

impl Seat {
    pub fn code(self) -> u8 {
        match self {
            Self::H264 => 0,
            Self::Intra => 1,
            Self::Av1 => 2,
            Self::Hevc => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::H264,
            1 => Self::Intra,
            2 => Self::Av1,
            3 => Self::Hevc,
            _ => return None,
        })
    }
}

/// Everything an open needs, in one place because it travels as argv and comes
/// back as a struct on the other side.
#[derive(Clone, Copy)]
pub struct Open {
    pub seat: Seat,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub bitrate: u64,
    /// What the sequence header will say the samples mean. Only the HEVC seat
    /// takes one; the others ignore it.
    pub colour: ColorDescription,
}

impl Open {
    fn argv(&self) -> Vec<String> {
        let (matrix, transfer) = (
            match self.colour.matrix {
                Matrix::Bt709 => 0,
                Matrix::Bt601 => 1,
                Matrix::Bt2020Ncl => 2,
            },
            match self.colour.transfer {
                Transfer::Sdr => 0,
                Transfer::Pq => 1,
                Transfer::Hlg => 2,
            },
        );
        [
            u32::from(self.seat.code()),
            self.width,
            self.height,
            self.fps_num,
            self.fps_den,
            matrix,
            transfer,
            u32::from(self.colour.full_range),
        ]
        .iter()
        .map(u32::to_string)
        .chain(std::iter::once(self.bitrate.to_string()))
        .collect()
    }

    /// The other half of [`Open::argv`], in the child.
    pub fn from_argv(args: &[String]) -> Option<Self> {
        let n = |i: usize| args.get(i)?.parse::<u32>().ok();
        Some(Self {
            seat: Seat::from_code(u8::try_from(n(0)?).ok()?)?,
            width: n(1)?,
            height: n(2)?,
            fps_num: n(3)?,
            fps_den: n(4)?,
            bitrate: args.get(8)?.parse().ok()?,
            colour: ColorDescription {
                matrix: match n(5)? {
                    0 => Matrix::Bt709,
                    1 => Matrix::Bt601,
                    _ => Matrix::Bt2020Ncl,
                },
                transfer: match n(6)? {
                    0 => Transfer::Sdr,
                    1 => Transfer::Pq,
                    _ => Transfer::Hlg,
                },
                full_range: n(7)? != 0,
            },
        })
    }

    /// How many bytes one packed I420 picture of this size takes -- the size of
    /// the shared mapping, decided once at the open because the export feeds one
    /// canvas size for the whole file.
    pub fn frame_bytes(&self) -> usize {
        let (w, h) = (self.width as usize, self.height as usize);
        w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))
    }
}

// Requests, parent to child (encode seat).
pub const T_ENCODE: u8 = 1;
pub const T_DMA: u8 = 2;
pub const T_DRAIN: u8 = 3;
pub const T_GEOM: u8 = 4;
// Requests, parent to child (decode seat). `T_NEXT` has no body, `T_NEXT_DMA`
// carries `coded_width, coded_height, width, height` as u32 LE, `T_SEEK` the
// 1-based target sample as u32 LE.
pub const T_NEXT: u8 = 5;
pub const T_NEXT_DMA: u8 = 6;
pub const T_SEEK: u8 = 7;
// Replies, child to parent. `R_NONE` carries no body; `R_AU` and `R_ERR` carry
// a u32 LE length and that many bytes. Descriptors ride on the tag byte.
pub const R_NONE: u8 = 0;
pub const R_AU: u8 = 1;
pub const R_ERR: u8 = 2;
// First byte of a decode frame's `R_AU` body: the picture is in the staging
// buffer as packed I420 (`K_PIXELS`: then `w` and `h` as u32 LE follow), or it
// is still on the GPU (`K_DMA`: the descriptor follows, see `DMA_BODY`, and the
// descriptors ride on the tag byte, one per plane object).
pub const K_PIXELS: u8 = 0;
pub const K_DMA: u8 = 1;
/// The `K_DMA` descriptor after its kind byte: fourcc u32, modifier u64, coded
/// width and height, picture width and height, two offsets and two strides as
/// u32 each, then the number of descriptors (1 or 2) as one byte.
pub const DMA_BODY: usize = 4 + 8 + 4 * 8 + 1;

/// A `memfd` this process reads and writes through its own descriptor: the
/// staging buffer one picture is written into and read out of, shared with the
/// other process. The protocol -- one request, one reply, never overlapping -- is
/// what keeps only one of the two touching it at a time.
///
/// File I/O rather than a mapping: an `mmap` is the one thing here that needs
/// `unsafe` at every turn, and a picture is copied in and out once either way.
pub struct Shm {
    file: File,
    len: usize,
}

impl Shm {
    /// Creates one of `len` bytes, returning it plus the descriptor to hand the
    /// other process.
    pub fn create(len: usize) -> std::io::Result<(Self, OwnedFd)> {
        let fd = rustix::fs::memfd_create("edith-hwenc", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(std::io::Error::from)?;
        let file = File::from(fd);
        file.set_len(len as u64)?;
        let shared = OwnedFd::from(file.try_clone()?);
        Ok((Self { file, len }, shared))
    }

    /// Takes the other process's descriptor of a buffer it created, `len` bytes long.
    pub fn from_fd(fd: OwnedFd, len: usize) -> Self {
        Self {
            file: File::from(fd),
            len,
        }
    }

    /// How many bytes the buffer holds.
    pub fn capacity(&self) -> usize {
        self.len
    }

    /// Writes `parts` back to back from the start of the buffer.
    pub fn write_picture(&self, parts: &[&[u8]]) -> std::io::Result<()> {
        let mut at = 0u64;
        for part in parts {
            self.file.write_all_at(part, at)?;
            at += part.len() as u64;
        }
        Ok(())
    }

    /// Reads the first `len` bytes of the buffer into `into`, reusing its
    /// allocation.
    pub fn read_into(&self, into: &mut Vec<u8>, len: usize) -> std::io::Result<()> {
        if len > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a read past the end of the staging buffer",
            ));
        }
        into.clear();
        into.resize(len, 0);
        self.file.read_exact_at(into, 0)
    }

    /// Reads the first `len` bytes of the buffer into an owned one.
    pub fn read_bytes(&self, len: usize) -> std::io::Result<Vec<u8>> {
        if len > self.len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a read past the end of the staging buffer",
            ));
        }
        let mut bytes = vec![0u8; len];
        self.file.read_exact_at(&mut bytes, 0)?;
        Ok(bytes)
    }
}

/// Sends one message with its file descriptors attached.
///
/// `MSG_NOSIGNAL` is the point of writing this by hand rather than
/// [`Write::write_all`]: a child that has just died leaves a socket whose next
/// write raises `SIGPIPE`, and a crash-containment path that kills the editor
/// with a signal of its own would be no containment at all.
pub fn send_msg(sock: &UnixStream, bytes: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    debug_assert!(!bytes.is_empty(), "ancillary data rides on a real byte");
    let rights = [ControlMessage::ScmRights(fds)];
    let control: &[ControlMessage] = if fds.is_empty() { &[] } else { &rights };
    let sent = sendmsg::<UnixAddr>(
        sock.as_raw_fd(),
        &[IoSlice::new(bytes)],
        control,
        MsgFlags::MSG_NOSIGNAL,
        None,
    )
    .map_err(std::io::Error::from)?;
    // Messages here are tens of bytes and a socket buffer is kilobytes, so a
    // short send means the peer is gone rather than that we should loop.
    if sent != bytes.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short send",
        ));
    }
    Ok(())
}

/// Reads the one byte a message's descriptors ride on, and those descriptors.
///
/// Ancillary data attaches to a position in the byte stream, so the tag is read
/// with `recvmsg` and everything after it with an ordinary read.
pub fn recv_tag(sock: &UnixStream) -> std::io::Result<(u8, Vec<OwnedFd>)> {
    let mut byte = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut byte)];
    // Room for four descriptors; the most anything here sends is two.
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let received = rustix::net::recvmsg(sock, &mut iov, &mut control, RecvFlags::CMSG_CLOEXEC)
        .map_err(std::io::Error::from)?;
    if received.bytes == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "peer closed",
        ));
    }
    let mut fds = Vec::new();
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    Ok((byte[0], fds))
}

/// One access unit, or the child's own error, read off the socket.
fn read_reply(sock: &mut UnixStream, au: &mut Vec<u8>) -> std::io::Result<Option<Result<(), String>>> {
    let (tag, _) = recv_tag(sock)?;
    if tag == R_NONE {
        return Ok(None);
    }
    let mut len = [0u8; 4];
    sock.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    au.clear();
    au.resize(len, 0);
    sock.read_exact(au)?;
    Ok(Some(match tag {
        R_AU => Ok(()),
        _ => Err(String::from_utf8_lossy(au).into_owned()),
    }))
}

/// Writes one reply: the tag with `fds` riding on it, then the body when the tag
/// carries one (`R_AU` and `R_ERR` do; `R_NONE` is the tag alone).
pub fn send_reply(sock: &mut UnixStream, tag: u8, body: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    send_msg(sock, &[tag], fds)?;
    if tag == R_AU || tag == R_ERR {
        sock.write_all(&(body.len() as u32).to_le_bytes())?;
        sock.write_all(body)?;
    }
    Ok(())
}

/// Reads one reply: its tag and the descriptors that rode on it, and its body
/// into `body` when the tag carries one.
pub fn recv_reply(sock: &mut UnixStream, body: &mut Vec<u8>) -> std::io::Result<(u8, Vec<OwnedFd>)> {
    let (tag, fds) = recv_tag(sock)?;
    body.clear();
    if tag == R_AU || tag == R_ERR {
        let mut len = [0u8; 4];
        sock.read_exact(&mut len)?;
        body.resize(u32::from_le_bytes(len) as usize, 0);
        sock.read_exact(body)?;
    }
    Ok((tag, fds))
}

/// Where the helper lives: told outright, or beside the running executable --
/// which is `target/<profile>/` for the editor and one directory up from
/// `deps/` for a test binary.
fn locate_child() -> Option<PathBuf> {
    if let Some(pinned) = crate::envcfg::var_os("VE_HW_CHILD_BIN") {
        let pinned = PathBuf::from(pinned);
        return pinned.is_file().then_some(pinned);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    [dir.join(BIN), dir.parent()?.join(BIN)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// The command that builds the child, as the warning and the test panics say it.
pub const BUILD_CHILD: &str = "cargo build -p engine-hw --bin hw-child";

/// Whether this machine has a DRM render node, the thing the hardware seats
/// need. `false` on a machine with no GPU, where a missing child is expected.
pub fn render_node_present() -> bool {
    std::fs::read_dir("/dev/dri").is_ok_and(|dir| {
        dir.flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
    })
}

/// Where the child binary is, or `None` when it is not built next to the
/// editor (and `VE_HW_CHILD_BIN` does not name a file). A missing child on a
/// machine with a render node is warned about once, on stderr.
pub fn child_path() -> Option<PathBuf> {
    static WARNED: std::sync::Once = std::sync::Once::new();
    let found = locate_child();
    if found.is_none() {
        WARNED.call_once(|| {
            if render_node_present() {
                eprintln!(
                    "edith: hardware decode/encode is off: this machine has a GPU but hw-child is not built. \
                     Run `{BUILD_CHILD}` (or set VE_HW_CHILD_BIN to a built hw-child)."
                );
            }
        });
    }
    found
}

/// Whether an isolated child can be had at all -- a binary to run. `false`
/// means every hardware seat answers "no" and the software codec takes over.
pub fn available() -> bool {
    child_path().is_some()
}

/// Starts the child with `args` (its mode first, then the mode's own arguments)
/// and hands it one end of a socket pair as its standard input, which is the
/// only channel the child has. Its stdout goes to our stderr: an MCP client
/// reads our stdout as JSON-RPC only. `None` when there is no child binary or
/// it will not start.
pub fn spawn_child(args: &[OsString]) -> Option<(Child, UnixStream)> {
    let bin = child_path()?;
    let (mine, theirs) = UnixStream::pair().ok()?;
    mine.set_read_timeout(Some(timeout())).ok()?;
    let mut command = Command::new(bin);
    crate::envcfg::apply(&mut command);
    let child = command
        .args(args)
        .stdin(Stdio::from(OwnedFd::from(theirs)))
        .stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned().ok()?))
        .spawn()
        .ok()?;
    Some((child, mine))
}

/// The child's side of the containment: it dies with the parent, so an editor
/// that is killed or aborts does not leave a driver holder behind. Called
/// first thing in the child.
pub fn die_with_parent() {
    let _ = set_parent_process_death_signal(Some(Signal::KILL));
}

/// How long one call to the child may take before the child is treated as hung
/// (`VE_HW_TIMEOUT_MS`, 15 s by default).
pub fn timeout() -> Duration {
    let ms = crate::envcfg::var("VE_HW_TIMEOUT_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(15_000);
    Duration::from_millis(ms)
}

/// The test-only stand-in seat: a child that opens no driver at all, so the
/// containment paths below can be exercised on a machine whose GPU is wedged --
/// which is the only machine where they matter and the only one where the real
/// trigger cannot be pulled.
pub fn faked() -> bool {
    crate::envcfg::var_os("VE_HW_TEST_FAKE").is_some()
}

/// A hardware encode session living in another process.
pub struct Remote {
    child: Child,
    sock: UnixStream,
    /// The staging buffer, once the child has said it is open. `None` only while
    /// [`Remote::spawn`] is still reading that answer.
    shm: Option<Shm>,
    open: Open,
    /// The last access unit, owned here: the child's buffer is in the child.
    au: Vec<u8>,
    /// Set the moment the child is known gone, so every later call refuses in
    /// the same words instead of blocking on a socket nobody holds.
    lost: Option<String>,
    /// The slowest call this child has answered, and how many it has answered
    /// at all: what [`Remote::deadline`] measures its patience against.
    slowest: Duration,
    answered: u32,
}

/// How many answered calls it takes before the wait stops being the configured
/// ceiling and becomes this encoder's own measure, how much slower than its
/// slowest answer a call may be before it is a wedge, and the least it is ever
/// given.
///
/// The ceiling has to be generous -- an encoder opening on a 4K frame is not a
/// dead one -- and the export spends every second of it on a child that will
/// never answer again: the file is owed the software encoder and the wait is
/// pure delay before it starts. The window is what shrinks, not the ceiling.
/// Once a child has answered [`CALIBRATE`] calls, the machine has said how long
/// its own frames take, and eight times the slowest of them (never under
/// [`FLOOR`]) contains a wedge in two or three seconds rather than fifteen,
/// while a slow open still gets the whole ceiling and is never killed for
/// being slow. The editor is live throughout either way -- the wait is on the
/// export's own thread and the card goes on repainting -- so what this buys is
/// the delay, not the window.
const CALIBRATE: u32 = 8;
const SLACK: u32 = 8;
const FLOOR: Duration = Duration::from_secs(2);

impl Remote {
    /// How long the child may take over one call before it is treated as hung.
    /// A wedged ring does not always `abort()` -- it can simply stop -- and a
    /// timeout is the only thing that tells those apart from a slow frame.
    fn timeout() -> Duration {
        timeout()
    }

    /// How long the *next* call may take: the ceiling until this child has
    /// shown what its frames cost, then its own slowest answer with room to
    /// spare ([`CALIBRATE`]).
    fn deadline(&self) -> Duration {
        let ceiling = Self::timeout();
        match self.answered >= CALIBRATE {
            true => ceiling.min((self.slowest * SLACK).max(FLOOR)),
            false => ceiling,
        }
    }

    /// Starts a child on this seat. `None` is the plugin's own "no" carried
    /// across the process boundary -- no driver, no entrypoint, a size refused
    /// -- and the caller takes the software encoder exactly as it always did.
    pub fn spawn(open: Open) -> Option<Self> {
        let mut args = vec![OsString::from("encode")];
        args.extend(open.argv().into_iter().map(OsString::from));
        let (child, mine) = spawn_child(&args)?;
        let mut remote = Self {
            child,
            sock: mine,
            shm: None,
            open,
            au: Vec::new(),
            lost: None,
            slowest: Duration::ZERO,
            answered: 0,
        };
        // The child answers once its encoder is open: a byte, and with it the
        // descriptor of the staging buffer it made for this session's size.
        match recv_tag(&remote.sock) {
            Ok((1, mut fds)) if fds.len() == 1 => {
                remote.shm = Some(Shm::from_fd(fds.remove(0), open.frame_bytes()));
                Some(remote)
            }
            // Anything else is "no seat here", including a child that died
            // opening the driver -- which is the crash this whole module exists
            // for, and it costs an export nothing but the software encoder.
            _ => None,
        }
    }

    /// What the child's encoder codes in, or `None` where it has no zero-copy
    /// door.
    ///
    /// The two `None`s are not the same thing and only one of them is an
    /// answer. A child that *says* it has no door costs the caller a read-back
    /// and nothing else; a child that does not answer at all is gone, and is
    /// reaped right here -- otherwise the next picture would spend a whole
    /// second timeout discovering what this call already knows.
    pub fn dma_geometry(&mut self) -> Option<(u32, u32)> {
        self.request(&[T_GEOM], &[]).ok()?;
        let mut buf = [0u8; 8];
        let answered = match recv_tag(&self.sock) {
            Ok((R_AU, _)) => self.sock.read_exact(&mut buf).map(|()| true),
            Ok(_) => Ok(false),
            Err(e) => Err(e),
        };
        match answered {
            Ok(true) => Some((
                u32::from_le_bytes(buf[..4].try_into().ok()?),
                u32::from_le_bytes(buf[4..].try_into().ok()?),
            )),
            Ok(false) => None,
            Err(e) => {
                self.bury(&e.to_string());
                None
            }
        }
    }

    /// Feeds one packed I420 picture through the shared mapping.
    pub fn encode(
        &mut self,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        width: u32,
        height: u32,
        force_key: bool,
    ) -> crate::Result<Option<&[u8]>> {
        let total = y.len() + u.len() + v.len();
        let Some(shm) = self.shm.as_ref() else {
            return Err("the staging buffer was never opened".to_owned().into());
        };
        if total > shm.capacity() {
            return Err(format!(
                "a {width}x{height} picture does not fit the staging buffer opened for {}x{}",
                self.open.width, self.open.height
            )
            .into());
        }
        if let Err(e) = shm.write_picture(&[y, u, v]) {
            return Err(format!("cannot stage the picture: {e}").into());
        }
        let mut msg = vec![T_ENCODE];
        msg.extend_from_slice(&width.to_le_bytes());
        msg.extend_from_slice(&height.to_le_bytes());
        msg.push(u8::from(force_key));
        self.round_trip(&msg, &[])
    }

    /// Feeds one picture the decoder left on the GPU. The descriptors go over
    /// with the message, so the buffer itself never moves.
    pub fn encode_dma(&mut self, frame: &DmaFrame, force_key: bool) -> crate::Result<Option<&[u8]>> {
        let dma: &VhDma = &frame.desc;
        let fds: Vec<RawFd> = frame.fds.iter().map(AsRawFd::as_raw_fd).collect();
        let mut msg = vec![T_DMA];
        msg.extend_from_slice(&dma.fourcc.to_le_bytes());
        msg.extend_from_slice(&dma.modifier.to_le_bytes());
        for value in [
            dma.coded_width,
            dma.coded_height,
            dma.width,
            dma.height,
            dma.offset[0],
            dma.offset[1],
            dma.stride[0],
            dma.stride[1],
        ] {
            msg.extend_from_slice(&value.to_le_bytes());
        }
        msg.push(fds.len() as u8);
        msg.push(u8::from(force_key));
        self.round_trip(&msg, &fds)
    }

    /// Flushes the child's encoder; call until it answers `None`.
    pub fn drain(&mut self) -> crate::Result<Option<&[u8]>> {
        self.round_trip(&[T_DRAIN], &[])
    }

    /// One request and its reply, which is every call above: the child is fed,
    /// then read, and anything that is not an answer is the child being gone.
    fn round_trip(&mut self, msg: &[u8], fds: &[RawFd]) -> crate::Result<Option<&[u8]>> {
        // Set per call, because what it may be is measured from the calls
        // before it ([`deadline`]).
        let _ = self.sock.set_read_timeout(Some(self.deadline()));
        let started = std::time::Instant::now();
        self.request(msg, fds)?;
        let mut au = std::mem::take(&mut self.au);
        let read = read_reply(&mut self.sock, &mut au);
        self.au = au;
        if read.is_ok() {
            self.slowest = self.slowest.max(started.elapsed());
            self.answered += 1;
        }
        // Every "no access unit" leaves through a `return`, so the borrow of the
        // buffer below is the only one alive when it is taken.
        match read {
            Ok(None) => return Ok(None),
            Ok(Some(Ok(()))) => {}
            // The child's *own* refusal: it is alive and said no, so this is an
            // ordinary encode error and the process stays as it is.
            Ok(Some(Err(message))) => return Err(message.into()),
            Err(e) => return Err(self.bury(&e.to_string())),
        }
        Ok(Some(&self.au))
    }

    fn request(&mut self, msg: &[u8], fds: &[RawFd]) -> crate::Result<()> {
        if let Some(gone) = &self.lost {
            return Err(Lost(gone.clone()).into());
        }
        match send_msg(&self.sock, msg, fds) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.bury(&e.to_string())),
        }
    }

    /// Reaps the child and remembers why, so no later call waits on it. Kills
    /// first: the child may be *stuck* rather than dead, which is the hang this
    /// timeout exists to contain, and a stuck child is never waited for.
    fn bury(&mut self, why: &str) -> crate::Error {
        let _ = self.child.kill();
        let status = self.child.wait().ok();
        // The two shapes a wedged ring takes, told apart by who did the
        // killing: a driver that called `abort()` leaves its own signal, while
        // a child that merely stopped answering is still holding `SIGKILL` from
        // the line above and the read's own timeout is the real reason.
        let how = match status.and_then(|s| std::os::unix::process::ExitStatusExt::signal(&s)) {
            Some(libc::SIGKILL) | None => format!("stopped answering ({why}) and was killed"),
            Some(signal) => format!("was killed by signal {signal}"),
        };
        let message = format!("{HW_LOST} {how}; the driver took it down and not the editor");
        self.lost = Some(message.clone());
        Lost(message).into()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Nothing is pending -- the export drains before it drops -- and a
        // signal is the one exit a wedged child is guaranteed to take.
        if self.lost.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descriptors really cross the socket, and the buffer really is shared: the
    /// two halves of the frame hand-off, checked without a driver.
    #[test]
    fn a_buffer_and_its_descriptor_cross_a_socket() {
        use std::os::fd::AsRawFd;
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (mine, fd) = Shm::create(64).expect("memfd");
        mine.write_picture(&[b"hello"]).expect("stage");
        send_msg(&a, &[7u8], &[fd.as_raw_fd()]).expect("send");
        let (tag, mut fds) = recv_tag(&b).expect("recv");
        assert_eq!(tag, 7);
        assert_eq!(fds.len(), 1, "the descriptor came over");
        let theirs = Shm::from_fd(fds.remove(0), 64);
        assert_eq!(theirs.read_bytes(5).expect("read"), b"hello", "one store, two handles");
        theirs.write_picture(&[b"j"]).expect("write back");
        assert_eq!(mine.read_bytes(5).expect("read"), b"jello", "and it is shared both ways");
    }

    /// A dead peer must come back as an error, never as a `SIGPIPE` that kills
    /// the process doing the sending -- the whole point of `MSG_NOSIGNAL`.
    #[test]
    fn sending_to_a_closed_socket_errors_instead_of_signalling() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        drop(b);
        assert!(send_msg(&a, &[1u8, 2, 3], &[]).is_err());
    }

    /// The open crosses to the child as text and comes back the same struct.
    #[test]
    fn an_open_round_trips_through_argv() {
        let open = Open {
            seat: Seat::Hevc,
            width: 1920,
            height: 1080,
            fps_num: 30000,
            fps_den: 1001,
            bitrate: 12_345_678,
            colour: ColorDescription {
                matrix: Matrix::Bt2020Ncl,
                transfer: Transfer::Pq,
                full_range: true,
            },
        };
        let back = Open::from_argv(&open.argv()).expect("parse");
        assert!(back.seat == Seat::Hevc);
        assert_eq!(
            (back.width, back.height, back.fps_num, back.fps_den, back.bitrate),
            (1920, 1080, 30000, 1001, 12_345_678)
        );
        assert_eq!(back.colour, open.colour);
        assert_eq!(back.frame_bytes(), 1920 * 1080 * 3 / 2);
    }
}
