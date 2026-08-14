//! The process that holds the video driver, and therefore the process a video
//! driver is allowed to kill.
//!
//! It is started by [`engine::hwproc::Remote::spawn`] with the encode session's
//! parameters as argv and one end of a socket pair as its standard input. It
//! opens the plugin's encoder in *this* address space, hands the parent a shared
//! anonymous file to stage pictures in, and then answers one request at a time
//! until the socket closes. Nothing else lives here: no project, no muxer, no
//! window -- so an `abort()` out of Mesa's own worker thread costs the editor a
//! reply and a fallback rather than the session.
//!
//! Three environment variables exist only for the tests that prove that, and
//! each is read here and nowhere else, so an editor without them set runs the
//! path it always ran:
//!
//! * `VE_HW_TEST_FAKE` -- open a stand-in seat that touches no driver, so the
//!   containment paths can be exercised on a machine whose GPU is wedged (which
//!   is the only machine where they matter). Refuses to open unless one of the
//!   two below is set with it, so it can never be a silently empty encoder.
//! * `VE_HW_TEST_ABORT=<init|drain|N>` -- call `abort()` from a worker thread of
//!   our own, before opening, at the first drain, or before coding picture `N`.
//!   A foreign C thread's `abort()` is what really happened on 2026-08-13; this
//!   is the same shape without a hung ring.
//! * `VE_HW_TEST_HANG=<init|drain|N>` -- stop answering at that same point,
//!   without dying, which is the other half of a wedged ring.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;

use engine::hw::{DmaFrame, LocalEncoder, VhDma};
use engine::hwproc::{Open, R_AU, R_ERR, R_NONE, Shm, T_DMA, T_DRAIN, T_ENCODE, T_GEOM, recv_tag, send_msg};

/// Where the encoder actually is: the plugin's, or the test stand-in that opens
/// no driver at all.
enum Seat {
    Real(LocalEncoder),
    Fake,
}

impl Seat {
    fn dma_geometry(&self) -> Option<(u32, u32)> {
        match self {
            Self::Real(encoder) => encoder.dma_geometry(),
            // Nothing to import into, so the parent stages pictures instead --
            // which is the path the containment tests want anyway.
            Self::Fake => None,
        }
    }
}

/// One of the three injection points, named the same way in all three variables.
#[derive(PartialEq, Eq)]
enum At {
    Init,
    Drain,
    Frame(u32),
}

fn injected(var: &str) -> Option<At> {
    let value = std::env::var(var).ok()?;
    Some(match value.as_str() {
        "init" => At::Init,
        "drain" => At::Drain,
        _ => At::Frame(value.parse().ok()?),
    })
}

/// The driver's own way of dying, reproduced: `abort()` called from a thread
/// that is not the one running the encode, because that is where
/// `amdgpu_ctx_set_sw_reset_status` calls it from and because a `catch_unwind`
/// around the encode call is exactly what cannot see it.
fn die() {
    std::thread::spawn(|| {
        // SAFETY: `abort` takes nothing, returns never, and is the whole point.
        unsafe { libc::abort() }
    });
    loop {
        std::thread::park();
    }
}

/// ...and the other half: alive, holding the socket open, answering nothing.
fn hang() -> ! {
    loop {
        std::thread::park();
    }
}

fn reached(point: &At) {
    if injected("VE_HW_TEST_ABORT").as_ref() == Some(point) {
        die();
    }
    if injected("VE_HW_TEST_HANG").as_ref() == Some(point) {
        hang();
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(open) = Open::from_argv(&args) else {
        eprintln!("hw-encode-child: bad arguments");
        std::process::exit(2);
    };
    // SAFETY: the parent handed this process one end of a socket pair as its
    // standard input and holds the other; nothing else in this process reads
    // descriptor 0.
    let mut sock = unsafe { UnixStream::from_raw_fd(0) };

    reached(&At::Init);
    let seat = match std::env::var_os("VE_HW_TEST_FAKE").is_some() {
        // A stand-in that no test forgot to arm: without an injection point it
        // would be an encoder producing nothing, and that is a hang of its own
        // kind rather than a test.
        true => match injected("VE_HW_TEST_ABORT").is_some() || injected("VE_HW_TEST_HANG").is_some()
        {
            true => Some(Seat::Fake),
            false => None,
        },
        false => LocalEncoder::open_seat(open).map(Seat::Real),
    };
    let Some(mut seat) = seat else {
        // The plugin's own "no", carried back as a byte: no driver, no
        // entrypoint, a size refused. The parent takes the software encoder.
        let _ = sock.write_all(&[0u8]);
        std::process::exit(1);
    };

    // The staging buffer for pictures that live in memory: made here because
    // this side knows the session's size, and handed over with the "yes".
    let (mut shm, fd) = match Shm::create(open.frame_bytes()) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hw-encode-child: no staging buffer: {e}");
            let _ = sock.write_all(&[0u8]);
            std::process::exit(1);
        }
    };
    if send_msg(&sock, &[1u8], &[fd.as_raw_fd()]).is_err() {
        std::process::exit(1);
    }
    drop(fd);

    let mut coded = 0u32;
    loop {
        let Ok((tag, fds)) = recv_tag(&sock) else {
            // The parent is gone (it closed, or it died): there is nothing left
            // to encode for.
            return;
        };
        match tag {
            T_GEOM => match seat.dma_geometry() {
                Some((width, height)) => {
                    let mut reply = vec![R_AU];
                    reply.extend_from_slice(&width.to_le_bytes());
                    reply.extend_from_slice(&height.to_le_bytes());
                    let _ = sock.write_all(&reply);
                }
                None => {
                    let _ = sock.write_all(&[R_NONE]);
                }
            },
            T_ENCODE => {
                let mut head = [0u8; 9];
                if sock.read_exact(&mut head).is_err() {
                    return;
                }
                reached(&At::Frame(coded));
                coded += 1;
                let width = u32::from_le_bytes(head[0..4].try_into().expect("four bytes"));
                let height = u32::from_le_bytes(head[4..8].try_into().expect("four bytes"));
                let force_key = head[8] != 0;
                let (w, h) = (width as usize, height as usize);
                let chroma = w.div_ceil(2) * h.div_ceil(2);
                let answer = match &mut seat {
                    Seat::Real(encoder) => {
                        let bytes = shm.bytes();
                        let (y, rest) = bytes.split_at(w * h);
                        let (u, rest) = rest.split_at(chroma);
                        encoder.encode(y, u, &rest[..chroma], width, height, force_key)
                    }
                    Seat::Fake => Ok(None),
                };
                if !reply(&mut sock, answer) {
                    return;
                }
            }
            T_DMA => {
                let mut head = [0u8; 46];
                if sock.read_exact(&mut head).is_err() {
                    return;
                }
                reached(&At::Frame(coded));
                coded += 1;
                let u32_at = |offset: usize| {
                    u32::from_le_bytes(head[offset..offset + 4].try_into().expect("four bytes"))
                };
                let mut desc = VhDma {
                    fourcc: u32_at(0),
                    modifier: u64::from_le_bytes(head[4..12].try_into().expect("eight bytes")),
                    coded_width: u32_at(12),
                    coded_height: u32_at(16),
                    width: u32_at(20),
                    height: u32_at(24),
                    offset: [u32_at(28), u32_at(32)],
                    stride: [u32_at(36), u32_at(40)],
                    fd: [-1, -1],
                };
                if fds.len() != head[44] as usize || fds.is_empty() {
                    let _ = write_error(&mut sock, "a buffer arrived without its descriptors");
                    continue;
                }
                // Ownership passes to the frame, which closes them: the parent
                // kept its own copies and these are ours.
                for (slot, fd) in desc.fd.iter_mut().zip(fds) {
                    *slot = fd.into_raw_fd();
                }
                let frame = DmaFrame::from_desc(desc);
                let answer = match &mut seat {
                    Seat::Real(encoder) => encoder.encode_dma(&frame, head[45] != 0),
                    Seat::Fake => Ok(None),
                };
                if !reply(&mut sock, answer) {
                    return;
                }
            }
            T_DRAIN => {
                reached(&At::Drain);
                let answer = match &mut seat {
                    Seat::Real(encoder) => encoder.drain(),
                    Seat::Fake => Ok(None),
                };
                if !reply(&mut sock, answer) {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Writes one answer back. `false` means the parent is gone and this process has
/// nothing left to do.
fn reply(sock: &mut UnixStream, answer: engine::Result<Option<&[u8]>>) -> bool {
    match answer {
        Ok(None) => sock.write_all(&[R_NONE]).is_ok(),
        Ok(Some(au)) => {
            let mut head = vec![R_AU];
            head.extend_from_slice(&(au.len() as u32).to_le_bytes());
            sock.write_all(&head).is_ok() && sock.write_all(au).is_ok()
        }
        // The encoder's own refusal, not a death: the parent raises it as the
        // export error it is and this process stays open.
        Err(e) => write_error(sock, &e.to_string()),
    }
}

fn write_error(sock: &mut UnixStream, message: &str) -> bool {
    let mut head = vec![R_ERR];
    head.extend_from_slice(&(message.len() as u32).to_le_bytes());
    sock.write_all(&head).is_ok() && sock.write_all(message.as_bytes()).is_ok()
}
