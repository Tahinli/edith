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
//! [`Remote::timeout`] -- the parent sees a closed socket or a timed-out read,
//! reaps the child, and reports an error carrying [`HW_LOST`]. The export then
//! runs again on the software encoder ([`crate::export::start`]), or refuses in
//! words where a person picked the GPU by name.
//!
//! Two things cross between the processes and neither is a picture copied down a
//! pipe. Pictures that live in memory are written into a shared anonymous file
//! (`memfd`, mapped in both), so a frame costs one `memcpy` and a 10-byte
//! message. Pictures still on the GPU never move at all: the DRM-PRIME
//! descriptors travel as file descriptors over `SCM_RIGHTS`, so the zero-copy
//! export path stays zero-copy across the process boundary.

use std::ffi::c_void;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::colorspace::{ColorDescription, Matrix, Transfer};
use crate::hw::VhDma;

/// The words every "the child is gone" error carries, and the only thing
/// [`crate::export`] matches on to decide that the software encoder should have
/// another go at the file. A message, not an error kind, because
/// [`crate::Error`] is a boxed `dyn Error` and a caller two crates away reads
/// its text.
pub const HW_LOST: &str = "the hardware encoder process";

/// Whether an error is one: a driver that took its process down, rather than a
/// picture this engine got wrong.
pub fn is_lost(error: &crate::Error) -> bool {
    error.to_string().contains(HW_LOST)
}

/// The helper's file name; it is built into the same directory as the editor.
const BIN: &str = "hw-encode-child";

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

// Requests, parent to child.
pub const T_ENCODE: u8 = 1;
pub const T_DMA: u8 = 2;
pub const T_DRAIN: u8 = 3;
pub const T_GEOM: u8 = 4;
// Replies, child to parent.
pub const R_NONE: u8 = 0;
pub const R_AU: u8 = 1;
pub const R_ERR: u8 = 2;

/// A `memfd` mapped into this process: the staging buffer one picture is written
/// into and read out of without either side owning the other's memory.
pub struct Shm {
    ptr: *mut u8,
    len: usize,
}

impl Shm {
    /// Creates one of `len` bytes, returning it mapped plus the descriptor to
    /// hand the other process.
    pub fn create(len: usize) -> std::io::Result<(Self, OwnedFd)> {
        // SAFETY: a NUL-terminated name and a flag word, which is the whole of
        // this call's contract; the descriptor it returns is ours.
        let fd = unsafe { libc::memfd_create(c"edith-hwenc".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nothing else holds.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(len as u64)?;
        let shm = Self::map(fd, len)?;
        Ok((shm, OwnedFd::from(file)))
    }

    /// Maps a descriptor the other process created.
    pub fn map(fd: RawFd, len: usize) -> std::io::Result<Self> {
        // SAFETY: a shared mapping of a descriptor that is open for the call;
        // the kernel picks the address and the length is the file's own.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
        })
    }

    /// A mapping of nothing, for the moment between spawning a child and
    /// learning which buffer it made.
    fn empty() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    /// The mapping as bytes. Both processes see the same store, and the protocol
    /// -- one request, one reply, never overlapping -- is what keeps only one of
    /// them looking at a time.
    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is `len` bytes long and lives as long as `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        // SAFETY: the pair of the `mmap` above, unmapped exactly once.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

// The mapping is used from one thread at a time (the export worker), as the
// session it belongs to is.
unsafe impl Send for Shm {}

/// Sends one message with its file descriptors attached.
///
/// `MSG_NOSIGNAL` is the point of writing this by hand rather than
/// [`Write::write_all`]: a child that has just died leaves a socket whose next
/// write raises `SIGPIPE`, and a crash-containment path that kills the editor
/// with a signal of its own would be no containment at all.
pub fn send_msg(sock: &UnixStream, bytes: &[u8], fds: &[RawFd]) -> std::io::Result<()> {
    debug_assert!(!bytes.is_empty(), "ancillary data rides on a real byte");
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut c_void,
        iov_len: bytes.len(),
    };
    // Two descriptors is the most anything here sends (a two-object NV12
    // buffer); 64 bytes covers the header, the payload and the alignment.
    let mut control = [0u8; 64];
    // SAFETY: every pointer below is into a local that outlives the call, the
    // control buffer is larger than the `CMSG_SPACE` written into
    // `msg_controllen`, and the descriptors are open for the duration.
    let sent = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if !fds.is_empty() {
            let payload = (std::mem::size_of::<RawFd>() * fds.len()) as u32;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(payload) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(payload) as _;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(cmsg).cast::<RawFd>(),
                fds.len(),
            );
        }
        libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL)
    };
    match sent {
        n if n < 0 => Err(std::io::Error::last_os_error()),
        // Messages here are tens of bytes and a socket buffer is kilobytes, so a
        // short send means the peer is gone rather than that we should loop.
        n if n as usize != bytes.len() => Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short send",
        )),
        _ => Ok(()),
    }
}

/// Reads the one byte a message's descriptors ride on, and those descriptors.
///
/// Ancillary data attaches to a position in the byte stream, so the tag is read
/// with `recvmsg` and everything after it with an ordinary read.
pub fn recv_tag(sock: &UnixStream) -> std::io::Result<(u8, Vec<OwnedFd>)> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0u8; 64];
    let mut fds = Vec::new();
    // SAFETY: as `send` -- locals that outlive the call and a control buffer
    // whose length is what `msg_controllen` claims. Every descriptor the kernel
    // writes there is ours, and is taken by an `OwnedFd` exactly once.
    let got = unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len() as _;
        let got = libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC);
        if got > 0 {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let bytes = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                    for i in 0..bytes / std::mem::size_of::<RawFd>() {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        got
    };
    match got {
        n if n < 0 => Err(std::io::Error::last_os_error()),
        0 => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "peer closed",
        )),
        _ => Ok((byte[0], fds)),
    }
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

/// Where the helper lives: told outright, or beside the running executable --
/// which is `target/<profile>/` for the editor and one directory up from
/// `deps/` for a test binary.
fn child_bin() -> Option<PathBuf> {
    if let Some(pinned) = std::env::var_os("VE_HW_CHILD_BIN") {
        return Some(PathBuf::from(pinned));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    [dir.join(BIN), dir.parent()?.join(BIN)]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Whether an isolated encoder can be had at all -- a helper to run. `false`
/// leaves the caller on the in-process seat, which is what a build that did not
/// produce the helper has always had.
pub fn available() -> bool {
    child_bin().is_some()
}

/// The test-only stand-in seat: a child that opens no driver at all, so the
/// containment paths below can be exercised on a machine whose GPU is wedged --
/// which is the only machine where they matter and the only one where the real
/// trigger cannot be pulled.
pub fn faked() -> bool {
    std::env::var_os("VE_HW_TEST_FAKE").is_some()
}

/// A hardware encode session living in another process.
pub struct Remote {
    child: Child,
    sock: UnixStream,
    shm: Shm,
    open: Open,
    /// The last access unit, owned here: the child's buffer is in the child.
    au: Vec<u8>,
    /// Set the moment the child is known gone, so every later call refuses in
    /// the same words instead of blocking on a socket nobody holds.
    lost: Option<String>,
}

impl Remote {
    /// How long the child may take over one call before it is treated as hung.
    /// A wedged ring does not always `abort()` -- it can simply stop -- and a
    /// timeout is the only thing that tells those apart from a slow frame.
    fn timeout() -> Duration {
        let ms = std::env::var("VE_HW_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15_000);
        Duration::from_millis(ms)
    }

    /// Starts a child on this seat. `None` is the plugin's own "no" carried
    /// across the process boundary -- no driver, no entrypoint, a size refused
    /// -- and the caller takes the software encoder exactly as it always did.
    pub fn spawn(open: Open) -> Option<Self> {
        let bin = child_bin()?;
        let (mine, theirs) = UnixStream::pair().ok()?;
        mine.set_read_timeout(Some(Self::timeout())).ok()?;
        // The socket is the child's stdin, which is how it crosses `exec`
        // without a `pre_exec` of our own. Its stdout and stderr stay ours, so
        // whatever the driver prints is still in the editor's log.
        let child = Command::new(bin)
            .args(open.argv())
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .spawn()
            .ok()?;
        let mut remote = Self {
            child,
            sock: mine,
            // Replaced by the child's own mapping below; a zero-length one
            // allocates nothing and keeps the field non-optional.
            shm: Shm::empty(),
            open,
            au: Vec::new(),
            lost: None,
        };
        // The child answers once its encoder is open: a byte, and with it the
        // descriptor of the staging buffer it made for this session's size.
        match recv_tag(&remote.sock) {
            Ok((1, fds)) if fds.len() == 1 => {
                remote.shm = Shm::map(fds[0].as_raw_fd(), open.frame_bytes()).ok()?;
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
    pub fn dma_geometry(&mut self) -> Option<(u32, u32)> {
        self.request(&[T_GEOM], &[]).ok()?;
        let (tag, _) = recv_tag(&self.sock).ok()?;
        if tag != R_AU {
            return None;
        }
        let mut buf = [0u8; 8];
        self.sock.read_exact(&mut buf).ok()?;
        Some((
            u32::from_le_bytes(buf[..4].try_into().ok()?),
            u32::from_le_bytes(buf[4..].try_into().ok()?),
        ))
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
        if total > self.shm.len {
            return Err(format!(
                "a {width}x{height} picture does not fit the staging buffer opened for {}x{}",
                self.open.width, self.open.height
            )
            .into());
        }
        let bytes = self.shm.bytes();
        bytes[..y.len()].copy_from_slice(y);
        bytes[y.len()..y.len() + u.len()].copy_from_slice(u);
        bytes[y.len() + u.len()..total].copy_from_slice(v);
        let mut msg = vec![T_ENCODE];
        msg.extend_from_slice(&width.to_le_bytes());
        msg.extend_from_slice(&height.to_le_bytes());
        msg.push(u8::from(force_key));
        self.round_trip(&msg, &[])
    }

    /// Feeds one picture the decoder left on the GPU. The descriptors go over
    /// with the message, so the buffer itself never moves.
    pub fn encode_dma(&mut self, dma: &VhDma, force_key: bool) -> crate::Result<Option<&[u8]>> {
        let fds: Vec<RawFd> = dma.fd.iter().copied().filter(|&fd| fd >= 0).collect();
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
        self.request(msg, fds)?;
        let mut au = std::mem::take(&mut self.au);
        let read = read_reply(&mut self.sock, &mut au);
        self.au = au;
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
            return Err(gone.clone().into());
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
        let message =
            format!("{HW_LOST} {how}; the driver took it down and not the editor");
        self.lost = Some(message.clone());
        message.into()
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

// As the in-process session: one thread at a time (the export worker).
unsafe impl Send for Remote {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Descriptors really cross the socket, and the mapping really is shared:
    /// the two halves of the frame hand-off, checked without a driver.
    #[test]
    fn a_mapping_and_its_descriptor_cross_a_socket() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (mut mine, fd) = Shm::create(64).expect("memfd");
        mine.bytes()[..5].copy_from_slice(b"hello");
        send_msg(&a, &[7u8], &[fd.as_raw_fd()]).expect("send");
        let (tag, fds) = recv_tag(&b).expect("recv");
        assert_eq!(tag, 7);
        assert_eq!(fds.len(), 1, "the descriptor came over");
        let mut theirs = Shm::map(fds[0].as_raw_fd(), 64).expect("map the received descriptor");
        assert_eq!(&theirs.bytes()[..5], b"hello", "one store, two mappings");
        theirs.bytes()[0] = b'j';
        assert_eq!(&mine.bytes()[..5], b"jello", "and it is shared both ways");
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
