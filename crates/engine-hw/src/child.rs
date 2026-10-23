//! The hardware child: the one process that opens a video driver.
//!
//! `hw-child` is started by `engine::hw` (decode and caps) and
//! `engine::hwproc` (encode) with a mode as its first argument and one end of a
//! socket pair as its standard input. It holds the socket until the parent
//! closes it, and then it exits; the parent's `PR_SET_PDEATHSIG` covers the
//! other way round, so neither side outlives the other.
//!
//! Modes:
//!
//! * `caps` -- answers the driver's capability query and exits.
//! * `decode <path> <target_sample>` -- opens the file for decode at the sample
//!   and then answers one request at a time (see the wire below).
//! * `encode <open argv...>` -- opens one encode seat and answers one request at
//!   a time, exactly as the encode half of [`engine::hwproc`] describes.
//!
//! Replies use [`engine::hwproc::send_reply`]: a tag byte that carries any
//! descriptors, then `u32` length and body for `R_AU` and `R_ERR`. A
//! decode session's pictures are packed I420 in a shared memfd (the staging
//! buffer) or, when the encoder can take them in place, DRM-PRIME buffers sent
//! as descriptors. Nothing else crosses the socket.
//!
//! Decode wire, after the open reply (`R_AU`, the 36-byte meta from
//! [`engine::hw::meta_to_wire`], with the staging memfd attached):
//!
//! * `T_NEXT` (no body) answers `R_NONE` at end of stream, `R_ERR` with the
//!   message on failure, or `R_AU` with `K_PIXELS` + `w` + `h` (u32 LE each),
//!   the picture in the staging buffer. The staging fd rides on the reply
//!   again only when the buffer had to grow.
//! * `T_NEXT_DMA` (body: coded width, coded height, width, height as u32 LE)
//!   answers the same, except that the picture may come back as `K_DMA` + the
//!   descriptor of [`engine::hwproc::DMA_BODY`], with one or two DRM-PRIME
//!   descriptors riding on the tag byte.
//! * `T_SEEK` (body: 1-based target sample, u32 LE) answers `R_AU` with an
//!   empty body or `R_ERR`.
//!
//! The child answers exactly one reply per request and never two at once. Any
//! request tag it does not know, or a socket that closes, ends the process.
//!
//! Three environment variables exist only for the tests that prove the
//! containment, and are read here and nowhere else (encode mode only):
//!
//! * `VE_HW_TEST_FAKE` -- open a stand-in seat that touches no driver. Refuses
//!   to open unless one of the two below is set with it.
//! * `VE_HW_TEST_ABORT=<init|drain|N>` -- `abort()` from a thread of our own at
//!   that point.
//! * `VE_HW_TEST_HANG=<init|drain|N>` -- stop answering at that point.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use engine::hw::{DmaFrame, VhDma, meta_to_wire};
use engine::hwproc::{
    DMA_BODY, K_DMA, K_PIXELS, Open, R_AU, R_ERR, R_NONE, Shm, T_DMA, T_DRAIN, T_ENCODE, T_GEOM,
    T_NEXT, T_NEXT_DMA, T_SEEK, die_with_parent, recv_tag, send_msg, send_reply,
};

use crate::enc_ec::EcEncSession;
use crate::{OpenSession, VA_FOURCC_NV12, query_caps};

/// The entry point of the `hw-child` binary.
pub fn main() {
    die_with_parent();
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().unwrap_or_default();
    let rest: Vec<OsString> = args.collect();
    // The socket is the child's standard input, as the parent made it. A second
    // descriptor for the same socket is taken, so standard input stays as is.
    let stdin = std::io::stdin();
    let Ok(sock_fd) = stdin.as_fd().try_clone_to_owned() else {
        eprintln!("hw-child: no socket on standard input");
        std::process::exit(2);
    };
    let mut sock = UnixStream::from(sock_fd);
    match mode.to_str() {
        Some("caps") => caps(&mut sock),
        Some("decode") => decode(&mut sock, &rest),
        Some("encode") => encode(&mut sock, &rest),
        _ => {
            eprintln!("hw-child: bad arguments");
            std::process::exit(2);
        }
    }
}

/// Answers the capability query: decode, encode and 10-bit decode, each a u32
/// LE, or `R_NONE` where there is no display to ask.
fn caps(sock: &mut UnixStream) {
    let _ = match query_caps() {
        Some(caps) => {
            let mut body = Vec::with_capacity(12);
            for word in [caps.decode, caps.encode, caps.decode_10bit] {
                body.extend_from_slice(&word.to_le_bytes());
            }
            send_reply(sock, R_AU, &body, &[])
        }
        None => send_reply(sock, R_NONE, &[], &[]),
    };
}

/// Bytes of one packed I420 picture of this size.
fn frame_len(width: u32, height: u32) -> usize {
    let (w, h) = (width as usize, height as usize);
    w * h + 2 * (w.div_ceil(2) * h.div_ceil(2))
}

/// The shared buffer pictures are staged in, and how many bytes it holds.
struct Staging {
    shm: Shm,
    capacity: usize,
}

/// Decode mode: open, announce, then serve requests until the socket closes.
fn decode(sock: &mut UnixStream, args: &[OsString]) {
    let [path, target] = args else {
        eprintln!("hw-child: decode takes a path and a sample");
        std::process::exit(2);
    };
    let Some(target) = target.to_str().and_then(|t| t.parse::<u32>().ok()) else {
        eprintln!("hw-child: bad sample");
        std::process::exit(2);
    };
    let path = PathBuf::from(path);
    let Some(mut session) = OpenSession::open_at(&path, target) else {
        let _ = send_reply(sock, R_NONE, &[], &[]);
        return;
    };
    let meta = session.meta();
    let (shm, fd) = match Shm::create(frame_len(meta.width, meta.height)) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hw-child: no staging buffer: {e}");
            let _ = send_reply(sock, R_NONE, &[], &[]);
            return;
        }
    };
    let capacity = frame_len(meta.width, meta.height);
    if send_reply(sock, R_AU, &meta_to_wire(meta), &[fd.as_raw_fd()]).is_err() {
        return;
    }
    drop(fd);
    let mut staging = Staging { shm, capacity };
    loop {
        let Ok((tag, _)) = recv_tag(sock) else {
            // The parent closed the socket: nothing left to decode for.
            return;
        };
        let answered = match tag {
            T_NEXT | T_NEXT_DMA => {
                let want = match tag {
                    T_NEXT_DMA => {
                        let mut body = [0u8; 16];
                        if sock.read_exact(&mut body).is_err() {
                            return;
                        }
                        let word = |i: usize| {
                            u32::from_le_bytes(body[4 * i..4 * i + 4].try_into().expect("four bytes"))
                        };
                        Some(VhDma {
                            fourcc: VA_FOURCC_NV12,
                            coded_width: word(0),
                            coded_height: word(1),
                            width: word(2),
                            height: word(3),
                            ..VhDma::default()
                        })
                    }
                    _ => None,
                };
                frame(sock, &mut session, &mut staging, want)
            }
            T_SEEK => {
                let mut body = [0u8; 4];
                if sock.read_exact(&mut body).is_err() {
                    return;
                }
                match session.seek_to(u32::from_le_bytes(body)) {
                    Ok(()) => send_reply(sock, R_AU, &[], &[]),
                    Err(e) => send_reply(sock, R_ERR, e.as_bytes(), &[]),
                }
            }
            _ => return,
        };
        if answered.is_err() {
            return;
        }
    }
}

/// Pumps one picture and sends it: as a DRM-PRIME buffer when the session left
/// one on the GPU, otherwise as packed I420 in the staging buffer.
fn frame(
    sock: &mut UnixStream,
    session: &mut OpenSession,
    staging: &mut Staging,
    want: Option<VhDma>,
) -> std::io::Result<()> {
    match session.next_frame(want) {
        Ok(false) => send_reply(sock, R_NONE, &[], &[]),
        Err(e) => send_reply(sock, R_ERR, e.as_bytes(), &[]),
        Ok(true) => match session.take_dma() {
            Some((desc, fds)) => send_dma(sock, &desc, &fds),
            None => send_pixels(sock, session, staging),
        },
    }
}

/// `K_DMA`, the descriptor, and the buffer's descriptors on the tag byte.
fn send_dma(sock: &mut UnixStream, desc: &VhDma, fds: &[OwnedFd]) -> std::io::Result<()> {
    let mut body = Vec::with_capacity(1 + DMA_BODY);
    body.push(K_DMA);
    body.extend_from_slice(&desc.fourcc.to_le_bytes());
    body.extend_from_slice(&desc.modifier.to_le_bytes());
    for value in [
        desc.coded_width,
        desc.coded_height,
        desc.width,
        desc.height,
        desc.offset[0],
        desc.offset[1],
        desc.stride[0],
        desc.stride[1],
    ] {
        body.extend_from_slice(&value.to_le_bytes());
    }
    body.push(fds.len() as u8);
    let raw: Vec<RawFd> = fds.iter().map(AsRawFd::as_raw_fd).collect();
    send_reply(sock, R_AU, &body, &raw)
}

/// Writes the picture into the staging buffer, growing it first when it must,
/// and answers `K_PIXELS` with its size. A grown buffer's descriptor rides on
/// the reply so the parent maps the new one.
fn send_pixels(
    sock: &mut UnixStream,
    session: &OpenSession,
    staging: &mut Staging,
) -> std::io::Result<()> {
    let (picture, w, h) = session.picture();
    let needed = picture.len();
    let grown = if needed > staging.capacity {
        let (shm, fd) = Shm::create(needed)?;
        staging.shm = shm;
        staging.capacity = needed;
        Some(fd)
    } else {
        None
    };
    staging.shm.write_picture(&[picture])?;
    let mut body = Vec::with_capacity(9);
    body.push(K_PIXELS);
    body.extend_from_slice(&w.to_le_bytes());
    body.extend_from_slice(&h.to_le_bytes());
    let attach: Vec<RawFd> = grown.iter().map(AsRawFd::as_raw_fd).collect();
    send_reply(sock, R_AU, &body, &attach)
}

/// Encode mode: open the seat, hand back the staging buffer, then serve encode
/// requests until the socket closes.
fn encode(sock: &mut UnixStream, args: &[OsString]) {
    let strings: Option<Vec<String>> = args.iter().map(|a| a.to_str().map(str::to_owned)).collect();
    let Some(open) = strings.as_deref().and_then(Open::from_argv) else {
        eprintln!("hw-child: bad arguments");
        std::process::exit(2);
    };

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
        false => crate::open_encoder(&open).map(|encoder| Seat::Real(Box::new(encoder))),
    };
    let Some(mut seat) = seat else {
        // The driver's own "no", carried back as a byte: the parent takes the
        // software encoder.
        let _ = std::io::Write::write_all(sock, &[0u8]);
        std::process::exit(1);
    };

    // The staging buffer for pictures that live in memory: made here because
    // this side knows the session's size, and handed over with the "yes".
    let (shm, fd) = match Shm::create(open.frame_bytes()) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hw-child: no staging buffer: {e}");
            let _ = std::io::Write::write_all(sock, &[0u8]);
            std::process::exit(1);
        }
    };
    if send_msg(sock, &[1u8], &[fd.as_raw_fd()]).is_err() {
        std::process::exit(1);
    }
    drop(fd);

    let mut coded = 0u32;
    loop {
        let Ok((tag, fds)) = recv_tag(sock) else {
            // The parent is gone: there is nothing left to encode for.
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
                    Seat::Real(encoder) => match shm.read_bytes(w * h + 2 * chroma) {
                        Ok(frame) => {
                            let (y, rest) = frame.split_at(w * h);
                            let (u, rest) = rest.split_at(chroma);
                            encoder
                                .encode(y, u, &rest[..chroma], force_key)
                                .map(|()| encoder.next_au())
                        }
                        Err(e) => Err(format!("cannot read the staging buffer: {e}")),
                    },
                    Seat::Fake => Ok(None),
                };
                if !reply(sock, answer) {
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
                let desc = VhDma {
                    fourcc: u32_at(0),
                    modifier: u64::from_le_bytes(head[4..12].try_into().expect("eight bytes")),
                    coded_width: u32_at(12),
                    coded_height: u32_at(16),
                    width: u32_at(20),
                    height: u32_at(24),
                    offset: [u32_at(28), u32_at(32)],
                    stride: [u32_at(36), u32_at(40)],
                };
                if fds.len() != head[44] as usize || fds.is_empty() {
                    if send_reply(sock, R_ERR, b"a buffer arrived without its descriptors", &[])
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                // The descriptors are ours from here: the parent kept its own.
                let frame = DmaFrame { desc, fds };
                let answer = match &mut seat {
                    Seat::Real(encoder) => encoder
                        .encode_dma(&frame, head[45] != 0)
                        .map(|()| encoder.next_au()),
                    Seat::Fake => Ok(None),
                };
                if !reply(sock, answer) {
                    return;
                }
            }
            T_DRAIN => {
                reached(&At::Drain);
                let answer = match &mut seat {
                    Seat::Real(encoder) => Ok(encoder.next_au()),
                    Seat::Fake => Ok(None),
                };
                if !reply(sock, answer) {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Where the encoder is: the driver's, or the test stand-in that opens none.
enum Seat {
    Real(Box<EcEncSession>),
    Fake,
}

impl Seat {
    fn dma_geometry(&self) -> Option<(u32, u32)> {
        match self {
            Self::Real(encoder) => Some(encoder.coded_size()),
            // Nothing to import into, so the parent stages pictures instead.
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

/// The driver's own way of dying, reproduced: `abort()` from a thread that is
/// not the one running the encode, which is where Mesa calls it from.
fn die() {
    std::thread::spawn(|| {
        std::process::abort();
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

/// Writes one encode answer. `false` means the parent is gone.
fn reply(sock: &mut UnixStream, answer: Result<Option<&[u8]>, String>) -> bool {
    let sent = match answer {
        Ok(None) => send_reply(sock, R_NONE, &[], &[]),
        Ok(Some(au)) => send_reply(sock, R_AU, au, &[]),
        // The encoder's own refusal, not a death: the parent raises it as the
        // export error it is and this process stays open.
        Err(e) => send_reply(sock, R_ERR, e.as_bytes(), &[]),
    };
    sent.is_ok()
}
