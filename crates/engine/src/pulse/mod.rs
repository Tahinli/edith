//! A pure-Rust client for the PulseAudio native protocol, carrying the one
//! playback stream the editor's audio output needs.
//!
//! pipewire-pulse serves the protocol on `$XDG_RUNTIME_DIR/pulse/native`, or on
//! the socket named by `$PULSE_SERVER`, and PulseAudio does the same. The
//! client talks to it over std's `UnixStream`: no C library, no `-sys` crate,
//! no `unsafe`. Samples go out inline as memblock packets, paced by the
//! server's REQUEST credits, and a reader thread takes the replies and events.
//!
//! Only the calls the output uses are here: authenticate, name the client,
//! create a stream, write, cork and uncork, flush, read the playback position
//! (GET_PLAYBACK_LATENCY), count underflows, delete the stream. Volume is
//! applied to the samples before they leave, not through the server: a
//! server-side stream volume is saved by the session's restore state, and a mute
//! here would come back in the system mixer tomorrow.

#![forbid(unsafe_code)]

pub mod wire;

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use wire::{Reader, Tags, command};

/// The client name the server shows for the stream.
pub(crate) const APP_NAME: &str = "edith";
/// Longest wait for one reply. A healthy server answers in well under a
/// millisecond; this only fires when the server has stopped answering.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// How far ahead of the device the server is asked to keep the stream.
const LATENCY_MS: u64 = 100;
/// The smallest request the server should make of us.
const MINREQ_MS: u64 = 10;
/// Most a stream holds that the server has not yet taken, in seconds of audio.
/// Writes beyond it are refused, as a full ring refuses them.
const QUEUE_SECONDS: usize = 1;
/// Largest packet payload the reader accepts; anything bigger is a broken
/// stream, not a reply.
const MAX_PAYLOAD: usize = 16 << 20;
/// Most a recording holds that nobody has read, in seconds of audio. Past it the
/// oldest samples go.
const MAX_CAPTURE_SECONDS: usize = 2;
/// How much audio the server packs into one recording packet, in milliseconds.
const RECORD_FRAGMENT_MS: u64 = 10;

/// Where the server listens: `$PULSE_SERVER` when set (its `unix:` path, or a
/// bare absolute path), else `$XDG_RUNTIME_DIR/pulse/native`. `None` when the
/// server is somewhere this client cannot reach, such as TCP.
pub fn server_path() -> Option<PathBuf> {
    server_path_from(
        crate::envcfg::var("PULSE_SERVER").as_deref(),
        crate::envcfg::var_os("XDG_RUNTIME_DIR").as_deref(),
    )
}

fn server_path_from(server: Option<&str>, runtime_dir: Option<&OsStr>) -> Option<PathBuf> {
    match server {
        Some(server) => {
            let first = server
                .split(|c: char| c.is_whitespace() || c == ';')
                .find(|entry| !entry.is_empty())?;
            match first.strip_prefix("unix:") {
                Some(path) => Some(PathBuf::from(path)),
                None if first.starts_with('/') => Some(PathBuf::from(first)),
                None => None,
            }
        }
        None => Some(Path::new(runtime_dir?).join("pulse/native")),
    }
}

/// The authentication cookie: `$PULSE_COOKIE`, else `pulse/cookie` under the
/// config home. Zeros when there is none, which pipewire-pulse accepts from a
/// local client.
fn cookie() -> Vec<u8> {
    let path = crate::envcfg::var_os("PULSE_COOKIE").map(PathBuf::from).or_else(|| {
        let config = crate::envcfg::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| crate::envcfg::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
        Some(config.join("pulse/cookie"))
    });
    path.and_then(|path| std::fs::read(path).ok())
        .filter(|bytes| !bytes.is_empty())
        .unwrap_or_else(|| vec![0; 256])
}

type Reply = Result<Vec<u8>, u32>;

/// What the reader thread and the caller both touch. The socket and the state
/// have their own locks, and neither is held across a blocking socket write, so
/// the reader can always take the state to record what the server said.
struct Shared {
    socket: Mutex<UnixStream>,
    state: Mutex<State>,
    /// Output gain as f32 bits, applied to the samples as they are sent.
    gain: AtomicU32,
    /// Set once the connection is gone or the server removed the stream.
    dead: AtomicBool,
}

struct State {
    /// The stream's channel, from the CREATE reply. It addresses the stream in
    /// the memblock headers and in the commands that name it (cork, flush,
    /// latency, delete), not the sink input the reply also carries.
    channel: u32,
    channels: usize,
    rate: u32,
    /// Bytes the server has asked for and we have not sent yet.
    credits: u64,
    /// Accepted samples not yet sent, interleaved, in whole frames.
    pending: VecDeque<f32>,
    capacity: usize,
    /// Set by the first accepted write, cleared by flush and by `stream_ended`:
    /// an underflow only counts once there is something to play
    /// ([`crate::ao`] documents the same rule for the old plugin).
    primed: bool,
    /// Replies still expected, by tag.
    waiters: HashMap<u32, mpsc::Sender<Reply>>,
    next_tag: u32,
    underruns: u64,
    /// Device position at the most recent underrun, -1 until one happens.
    last_underrun: i64,
    /// Device position last read, the one an underrun is stamped with.
    last_position: i64,
    /// Whether the server has reported the stream playing; the clock says
    /// nothing before that, as the plugin's did not.
    started: bool,
    /// Set by a recording: the samples the server pushes on the stream's
    /// channel queue in `capture` until they are read.
    capturing: bool,
    capture: VecDeque<f32>,
}

/// Locks, recovering the guard if a thread panicked while holding it: the state
/// is plain counters and queues, valid at any point.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Writes the packets that the credits allow out of `pending`, scaled by the
/// gain, and returns them ready for the socket. Empty when nothing can go.
fn take_packets(state: &mut State, gain: f32) -> Vec<u8> {
    let stride = 4 * state.channels;
    let by_credit = (state.credits as usize / stride) * state.channels;
    let count = by_credit.min(state.pending.len());
    if count == 0 {
        return Vec::new();
    }
    let mut data = Vec::with_capacity(count * 4);
    for sample in state.pending.drain(..count) {
        let value = if gain == 1.0 { sample } else { sample * gain };
        data.extend_from_slice(&value.to_le_bytes());
    }
    state.credits -= data.len() as u64;
    let mut packet = wire::header(data.len() as u32, state.channel, 0, 0).to_vec();
    packet.extend_from_slice(&data);
    packet
}

/// Sends bytes whole. A failed write means the connection is gone.
fn send(shared: &Shared, bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if shared.dead.load(Ordering::Acquire) {
        return false;
    }
    let sent = lock(&shared.socket).write_all(bytes).is_ok();
    if !sent {
        shared.dead.store(true, Ordering::Release);
    }
    sent
}

fn read_loop(mut socket: UnixStream, shared: Arc<Shared>) {
    let mut header = [0u8; wire::HEADER_LEN];
    loop {
        if socket.read_exact(&mut header).is_err() {
            break;
        }
        let (len, channel, _, _) = wire::parse_header(&header);
        if len as usize > MAX_PAYLOAD {
            break;
        }
        let mut payload = vec![0u8; len as usize];
        if socket.read_exact(&mut payload).is_err() {
            break;
        }
        if channel == wire::CONTROL_CHANNEL {
            dispatch(&shared, &payload);
        } else {
            capture(&shared, channel, &payload);
        }
    }
    shared.dead.store(true, Ordering::Release);
    // Dropping the senders wakes every caller still waiting for a reply.
    lock(&shared.state).waiters.clear();
}

/// Queues a recording's samples: the memblock packets the server pushes on the
/// stream's channel. Playback streams never set `capturing`, so their packets
/// (there are none) fall through.
fn capture(shared: &Shared, channel: u32, payload: &[u8]) {
    let mut state = lock(&shared.state);
    if !state.capturing || channel != state.channel {
        return;
    }
    let cap = state.rate as usize * state.channels * MAX_CAPTURE_SECONDS;
    let (samples, _) = payload.as_chunks::<4>();
    state.capture.extend(samples.iter().map(|b| f32::from_le_bytes(*b)));
    let excess = state.capture.len().saturating_sub(cap);
    state.capture.drain(..excess);
}

fn dispatch(shared: &Shared, payload: &[u8]) {
    let mut reader = Reader::new(payload);
    let (Some(cmd), Some(tag)) = (reader.u32(), reader.u32()) else {
        return;
    };
    match cmd {
        command::REPLY => {
            if let Some(waiter) = lock(&shared.state).waiters.remove(&tag) {
                let _ = waiter.send(Ok(reader.rest().to_vec()));
            }
        }
        command::ERROR => {
            let code = reader.u32().unwrap_or(0);
            if let Some(waiter) = lock(&shared.state).waiters.remove(&tag) {
                let _ = waiter.send(Err(code));
            }
        }
        command::REQUEST => {
            let (Some(channel), Some(len)) = (reader.u32(), reader.u32()) else {
                return;
            };
            let packet = {
                let mut state = lock(&shared.state);
                if channel != state.channel {
                    return;
                }
                state.credits += u64::from(len);
                take_packets(&mut state, f32::from_bits(shared.gain.load(Ordering::Relaxed)))
            };
            send(shared, &packet);
        }
        command::UNDERFLOW => {
            let mut state = lock(&shared.state);
            if state.primed {
                state.underruns += 1;
                state.last_underrun = state.last_position;
            }
        }
        command::PLAYBACK_STREAM_KILLED | command::RECORD_STREAM_KILLED => shared.dead.store(true, Ordering::Release),
        _ => {}
    }
}

/// Opens the socket at `path` and starts the reader thread. The stream is not
/// created yet: the caller greets the server and creates its stream.
fn connect(path: &Path, rate: u32, channels: u32) -> Option<(Arc<Shared>, JoinHandle<()>)> {
    if !(8_000..=768_000).contains(&rate) || !(1..=32).contains(&channels) {
        return None;
    }
    let socket = UnixStream::connect(path).ok()?;
    socket.set_write_timeout(Some(REPLY_TIMEOUT)).ok()?;
    let reader = socket.try_clone().ok()?;
    let shared = Arc::new(Shared {
        socket: Mutex::new(socket),
        state: Mutex::new(State {
            channel: 0,
            channels: channels as usize,
            rate,
            credits: 0,
            pending: VecDeque::new(),
            capacity: rate as usize * channels as usize * QUEUE_SECONDS,
            primed: false,
            waiters: HashMap::new(),
            next_tag: 1,
            underruns: 0,
            last_underrun: -1,
            last_position: -1,
            started: false,
            capturing: false,
            capture: VecDeque::new(),
        }),
        gain: AtomicU32::new(1.0f32.to_bits()),
        dead: AtomicBool::new(false),
    });
    let thread = {
        let shared = shared.clone();
        std::thread::spawn(move || read_loop(reader, shared))
    };
    Some((shared, thread))
}

/// Authenticates and names the client: what every stream starts with.
fn greet(shared: &Shared) -> bool {
    let Some(_) = call(shared, command::AUTH, Tags::new().u32(wire::PROTOCOL_VERSION).arbitrary(&cookie())) else {
        eprintln!("pulse: the server refused the connection");
        return false;
    };
    call(shared, command::SET_CLIENT_NAME, Tags::new().proplist(&[("application.name", APP_NAME)])).is_some()
}

/// Sends one command and waits for its reply. `None` on a refusal, a timeout,
/// or a dead connection.
fn call(shared: &Shared, cmd: u32, args: Tags) -> Option<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let tag = {
        let mut state = lock(&shared.state);
        let tag = state.next_tag;
        state.next_tag = state.next_tag.wrapping_add(1);
        state.waiters.insert(tag, tx);
        tag
    };
    if !send(shared, &wire::control(cmd, tag, args)) {
        lock(&shared.state).waiters.remove(&tag);
        return None;
    }
    match rx.recv_timeout(REPLY_TIMEOUT) {
        Ok(Ok(payload)) => Some(payload),
        Ok(Err(_)) | Err(_) => {
            lock(&shared.state).waiters.remove(&tag);
            None
        }
    }
}

/// An open playback stream. Dropping it deletes the stream on the server and
/// closes the connection.
pub struct Playback {
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
}

impl Playback {
    /// Connects to the server and creates an interleaved f32 stream at `rate`
    /// with `channels` channels. Starts running. `None` when there is no server,
    /// it refuses the client, or the format is not one it can carry.
    pub fn open(rate: u32, channels: u32) -> Option<Self> {
        Self::open_at(&server_path()?, rate, channels)
    }

    /// [`Playback::open`] against an explicit socket path.
    pub fn open_at(path: &Path, rate: u32, channels: u32) -> Option<Self> {
        let (shared, reader) = connect(path, rate, channels)?;
        let mut playback = Self {
            shared,
            reader: Some(reader),
        };
        playback.handshake(rate, channels).then_some(playback)
    }

    fn handshake(&mut self, rate: u32, channels: u32) -> bool {
        if !greet(&self.shared) {
            return false;
        }
        let bytes_per_second = u64::from(rate) * u64::from(channels) * 4;
        let tlength = (bytes_per_second * LATENCY_MS / 1000) as u32;
        let minreq = (bytes_per_second * MINREQ_MS / 1000) as u32;
        let args = wire::create_playback_stream_args(rate, channels as u8, tlength, minreq, wire::VOLUME_NORM);
        let Some(reply) = self.request(command::CREATE_PLAYBACK_STREAM, args) else {
            eprintln!("pulse: the server would not create a {rate} Hz, {channels}-channel stream");
            return false;
        };
        // The reply opens with the memblock channel, the stream's index and the
        // bytes the server wants straight away. The rest describes the buffer.
        let mut reader = Reader::new(&reply);
        let (Some(channel), Some(_sink_input), Some(requested)) = (reader.u32(), reader.u32(), reader.u32()) else {
            return false;
        };
        let mut state = lock(&self.shared.state);
        state.channel = channel;
        state.credits = u64::from(requested);
        true
    }

    /// Sends one command and waits for its reply. `None` on a refusal, a
    /// timeout, or a dead connection.
    fn request(&self, cmd: u32, args: Tags) -> Option<Vec<u8>> {
        call(&self.shared, cmd, args)
    }

    /// Sends a command that the server answers and nobody waits for.
    fn tell(&self, cmd: u32, args: Tags) -> bool {
        send(&self.shared, &wire::control(cmd, 0, args))
    }

    fn channel(&self) -> u32 {
        lock(&self.shared.state).channel
    }

    /// Queues interleaved samples and sends what the server has room for.
    /// Returns how many were taken, in whole frames: a short count means the
    /// queue is full and the caller should come back later. `None` once the
    /// output has died.
    pub fn write(&mut self, samples: &[f32]) -> Option<usize> {
        if self.shared.dead.load(Ordering::Acquire) {
            return None;
        }
        let gain = f32::from_bits(self.shared.gain.load(Ordering::Relaxed));
        let (taken, packet) = {
            let mut state = lock(&self.shared.state);
            let frame = state.channels;
            let room = state.capacity.saturating_sub(state.pending.len());
            let taken = samples.len().min(room) / frame * frame;
            state.pending.extend(&samples[..taken]);
            if taken > 0 {
                state.primed = true;
            }
            (taken, take_packets(&mut state, gain))
        };
        send(&self.shared, &packet).then_some(taken)
    }

    /// Samples per channel the device has actually played: what the server has
    /// read out of the stream, less what its sink still holds. `None` until the
    /// stream has started playing.
    pub fn position(&self) -> Option<i64> {
        let args = Tags::new().u32(self.channel()).timeval(0, 0);
        let reply = self.request(command::GET_PLAYBACK_LATENCY, args)?;
        let mut reader = Reader::new(&reply);
        let sink_usec = reader.u64()?;
        let _source_usec = reader.u64()?;
        let playing = reader.bool()?;
        reader.timeval()?;
        reader.timeval()?;
        let _write_index = reader.i64()?;
        let read_index = reader.i64()?;
        let mut state = lock(&self.shared.state);
        state.started |= playing;
        if !state.started || read_index < 0 {
            return None;
        }
        let stride = 4 * state.channels as i64;
        let frames = read_index / stride;
        let sink_frames = (sink_usec as i64).saturating_mul(i64::from(state.rate)) / 1_000_000;
        let played = (frames - sink_frames).max(0);
        state.last_position = played;
        Some(played)
    }

    /// Says the last sample of this stream has been queued: the silence after
    /// it stops counting as a late decoder. The next accepted write counts again.
    pub fn stream_ended(&self) {
        lock(&self.shared.state).primed = false;
    }

    /// Pauses (`false`) or resumes (`true`) playback. While paused the position
    /// stays put.
    pub fn set_active(&self, active: bool) -> bool {
        self.request(command::CORK_PLAYBACK_STREAM, Tags::new().u32(self.channel()).bool(!active))
            .is_some()
    }

    /// Sets the output gain, 0.0 (silent) to 1.0 (as written). It applies to
    /// the samples sent from now on, so the change lands within the server's
    /// latency. `false` for a gain outside the range.
    pub fn set_volume(&self, gain: f32) -> bool {
        if !(0.0..=1.0).contains(&gain) {
            return false;
        }
        self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
        true
    }

    /// The gain in force.
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.shared.gain.load(Ordering::Relaxed))
    }

    /// Drops everything queued, here and on the server, so the next samples
    /// written play right away. The played position keeps counting.
    pub fn flush(&self) -> bool {
        {
            let mut state = lock(&self.shared.state);
            state.pending.clear();
            state.primed = false;
        }
        self.request(command::FLUSH_PLAYBACK_STREAM, Tags::new().u32(self.channel())).is_some()
    }

    /// Underflows so far, and the device position the newest was seen at.
    pub fn underruns(&self) -> (u64, Option<i64>) {
        let state = lock(&self.shared.state);
        (state.underruns, (state.last_underrun >= 0).then_some(state.last_underrun))
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        if !self.shared.dead.load(Ordering::Acquire) {
            self.tell(command::DELETE_PLAYBACK_STREAM, Tags::new().u32(self.channel()));
        }
        let underruns = lock(&self.shared.state).underruns;
        if underruns > 0 {
            eprintln!("pulse: {underruns} underruns");
        }
        let _ = lock(&self.shared.socket).shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// An open recording stream: the server's input pushed in as interleaved f32
/// samples. Dropping it deletes the stream on the server.
pub struct Recording {
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
}

impl Recording {
    /// Connects and starts recording `channels` channels at `rate`, from the
    /// source named, or the server's default. `None` when there is no server,
    /// it refuses the client, or it cannot record that format from that source.
    pub fn open(rate: u32, channels: u32, source: Option<&str>) -> Option<Self> {
        Self::open_at(&server_path()?, rate, channels, source)
    }

    /// [`Recording::open`] against an explicit socket path.
    pub fn open_at(path: &Path, rate: u32, channels: u32, source: Option<&str>) -> Option<Self> {
        let (shared, reader) = connect(path, rate, channels)?;
        let mut recording = Self {
            shared,
            reader: Some(reader),
        };
        recording.handshake(rate, channels, source).then_some(recording)
    }

    fn handshake(&mut self, rate: u32, channels: u32, source: Option<&str>) -> bool {
        if !greet(&self.shared) {
            return false;
        }
        let fragsize = (u64::from(rate) * u64::from(channels) * 4 * RECORD_FRAGMENT_MS / 1000) as u32;
        let args = wire::create_record_stream_args(rate, channels as u8, source, fragsize);
        let Some(reply) = call(&self.shared, command::CREATE_RECORD_STREAM, args) else {
            eprintln!("pulse: the server would not record {rate} Hz, {channels} channels");
            return false;
        };
        // The reply opens with the memblock channel the samples arrive on.
        let Some(channel) = Reader::new(&reply).u32() else {
            return false;
        };
        let mut state = lock(&self.shared.state);
        state.channel = channel;
        state.capturing = true;
        true
    }

    /// Takes the samples captured since the last call: interleaved f32, in
    /// whole frames.
    pub fn read(&self) -> Vec<f32> {
        lock(&self.shared.state).capture.drain(..).collect()
    }

    /// `false` once the connection or the stream is gone.
    pub fn alive(&self) -> bool {
        !self.shared.dead.load(Ordering::Acquire)
    }

    /// Starts (`true`) or stops (`false`) the stream. Stopping before the
    /// delete keeps the server from sending more samples.
    pub fn set_active(&self, active: bool) -> bool {
        let channel = lock(&self.shared.state).channel;
        call(&self.shared, command::CORK_RECORD_STREAM, Tags::new().u32(channel).bool(!active)).is_some()
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if !self.shared.dead.load(Ordering::Acquire) {
            let channel = lock(&self.shared.state).channel;
            send(&self.shared, &wire::control(command::DELETE_RECORD_STREAM, 0, Tags::new().u32(channel)));
        }
        let _ = lock(&self.shared.socket).shutdown(std::net::Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A capture source, as the server lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    /// The name to open it by, such as `edtest.monitor`.
    pub name: String,
    pub description: String,
    /// Whether it is a monitor: the output of a sink, not a microphone.
    pub monitor: bool,
}

/// The server's sources, monitors included, in its order. `None` when there is
/// no server or it does not answer.
pub fn list_sources() -> Option<Vec<Source>> {
    list_sources_at(&server_path()?)
}

/// [`list_sources`] against an explicit socket path.
pub fn list_sources_at(path: &Path) -> Option<Vec<Source>> {
    let (shared, reader) = connect(path, 48_000, 2)?;
    let sources = greet(&shared)
        .then(|| call(&shared, command::GET_SOURCE_INFO_LIST, Tags::new()))
        .flatten()
        .and_then(|reply| parse_sources(&reply));
    let _ = lock(&shared.socket).shutdown(std::net::Shutdown::Both);
    let _ = reader.join();
    sources
}

/// Reads the source list reply, the fields of `source_fill_tagstruct` at
/// protocol version 35 (`pulsecore/protocol-native.c`).
fn parse_sources(reply: &[u8]) -> Option<Vec<Source>> {
    let mut r = Reader::new(reply);
    let mut sources = Vec::new();
    while !r.rest().is_empty() {
        r.u32()?; // index
        let name = r.string_or_null()?.unwrap_or_default();
        let description = r.string_or_null()?.unwrap_or_default();
        r.sample_spec()?;
        r.channel_map()?;
        r.u32()?; // module
        r.cvolume()?;
        r.bool()?; // muted
        let monitor_of = r.u32()?;
        r.string_or_null()?; // the sink it monitors
        r.u64()?; // latency
        r.string_or_null()?; // driver
        r.u32()?; // flags
        r.skip_proplist()?;
        r.u64()?; // requested latency
        r.volume()?; // base volume
        r.u32()?; // state
        r.u32()?; // volume steps
        r.u32()?; // card
        for _ in 0..r.u32()? {
            r.string_or_null()?; // port name
            r.string_or_null()?; // port description
            r.u32()?; // priority
            r.u32()?; // available
            r.string_or_null()?; // availability group
            r.u32()?; // port type
        }
        r.string_or_null()?; // active port
        for _ in 0..r.u8()? {
            r.format_info()?;
        }
        sources.push(Source {
            name,
            description,
            monitor: monitor_of != wire::DEFAULT,
        });
    }
    Some(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// One packet the client sent, split into its parts.
    struct Sent {
        channel: u32,
        /// For control packets: the command, its tag, and the arguments after.
        command: u32,
        tag: u32,
        args: Vec<u8>,
        /// For memblock packets: the samples' bytes.
        data: Vec<u8>,
    }

    /// The server's end of the socket, driven by the test: every packet the
    /// client sends arrives on `rx`, and the test answers by hand.
    struct Mock {
        /// The server's end, taken from `accepted` on first use: the client
        /// connects after `bind` returns.
        socket: Option<UnixStream>,
        accepted: mpsc::Receiver<UnixStream>,
        rx: mpsc::Receiver<Sent>,
        dir: PathBuf,
    }

    impl Mock {
        fn bind(name: &str) -> (Self, PathBuf) {
            let dir = std::env::temp_dir().join(format!("edith-pulse-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("native");
            let listener = UnixListener::bind(&path).unwrap();
            let (tx, rx) = mpsc::channel();
            let (accepted_tx, accepted_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                let reader = socket.try_clone().unwrap();
                accepted_tx.send(socket).unwrap();
                let mut reader = reader;
                let mut header = [0u8; wire::HEADER_LEN];
                while reader.read_exact(&mut header).is_ok() {
                    let (len, channel, _, _) = wire::parse_header(&header);
                    let mut payload = vec![0u8; len as usize];
                    if reader.read_exact(&mut payload).is_err() {
                        break;
                    }
                    let sent = if channel == wire::CONTROL_CHANNEL {
                        let mut r = Reader::new(&payload);
                        let command = r.u32().unwrap();
                        let tag = r.u32().unwrap();
                        Sent { channel, command, tag, args: r.rest().to_vec(), data: Vec::new() }
                    } else {
                        Sent { channel, command: u32::MAX, tag: 0, args: Vec::new(), data: payload }
                    };
                    if tx.send(sent).is_err() {
                        break;
                    }
                }
            });
            (
                Self {
                    socket: None,
                    accepted: accepted_rx,
                    rx,
                    dir: dir.clone(),
                },
                path,
            )
        }

        fn expect(&self, command: u32) -> Sent {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let sent = self.rx.recv_timeout(left).expect("the client sent nothing in time");
                if sent.command == command {
                    return sent;
                }
            }
        }

        /// Whatever memblock data arrives within `wait`, concatenated.
        fn memblock_within(&self, wait: Duration) -> Vec<u8> {
            let deadline = Instant::now() + wait;
            let mut data = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(sent) if sent.channel != wire::CONTROL_CHANNEL => data.extend(sent.data),
                    Ok(_) => {}
                    Err(_) => return data,
                }
            }
        }

        fn send(&mut self, packet: &[u8]) {
            if self.socket.is_none() {
                self.socket = Some(self.accepted.recv().unwrap());
            }
            self.socket.as_mut().unwrap().write_all(packet).unwrap();
        }

        /// The server goes away: the client sees the end of the socket.
        fn close(&mut self) {
            if let Some(socket) = &self.socket {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }

        fn reply(&mut self, tag: u32, args: Tags) {
            let packet = wire::control(command::REPLY, tag, args);
            self.send(&packet);
        }

        fn request_bytes(&mut self, channel: u32, len: u32) {
            let packet = wire::control(command::REQUEST, u32::MAX, Tags::new().u32(channel).u32(len));
            self.send(&packet);
        }

        fn underflow(&mut self, channel: u32) {
            let packet = wire::control(command::UNDERFLOW, u32::MAX, Tags::new().u32(channel).i64(0));
            self.send(&packet);
        }

        /// Answers the three handshake commands; the stream starts with
        /// `requested` bytes of credit.
        fn handshake(&mut self, requested: u32) -> Sent {
            let auth = self.expect(command::AUTH);
            let mut r = Reader::new(&auth.args);
            assert_eq!(r.u32(), Some(wire::PROTOCOL_VERSION));
            assert_eq!(r.arbitrary().map(<[u8]>::len), Some(256), "the cookie is sent");
            self.reply(auth.tag, Tags::new().u32(wire::PROTOCOL_VERSION));
            let name = self.expect(command::SET_CLIENT_NAME);
            self.reply(name.tag, Tags::new().u32(1));
            let create = self.expect(command::CREATE_PLAYBACK_STREAM);
            self.reply(
                create.tag,
                Tags::new()
                    .u32(0)
                    .u32(7)
                    .u32(requested)
                    .u32(wire::DEFAULT)
                    .u32(38_400)
                    .u32(38_400)
                    .u32(1_920),
            );
            create
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.close();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Runs a call that waits for one reply, answers that reply with an empty
    /// success, and gives back the call's result, the playback, and the command's
    /// arguments. The call runs on its own thread, since it blocks until answered.
    fn answered<T: Send + 'static>(
        mock: &mut Mock,
        playback: Playback,
        cmd: u32,
        call: impl FnOnce(&Playback) -> T + Send + 'static,
    ) -> (T, Playback, Vec<u8>) {
        let asking = std::thread::spawn(move || {
            let out = call(&playback);
            (out, playback)
        });
        let sent = mock.expect(cmd);
        mock.reply(sent.tag, Tags::new());
        let (out, playback) = asking.join().unwrap();
        (out, playback, sent.args)
    }

    fn open(path: PathBuf) -> JoinHandle<Option<Playback>> {
        std::thread::spawn(move || Playback::open_at(&path, 48_000, 2))
    }

    fn stereo_samples(frames: usize, value: f32) -> Vec<f32> {
        vec![value; frames * 2]
    }

    fn as_f32s(bytes: &[u8]) -> Vec<f32> {
        bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    }

    #[test]
    fn handshake_stream_credits_and_clock() {
        let (mut mock, path) = Mock::bind("handshake");
        let handle = open(path);
        let create = mock.handshake(4_096);
        let mut playback = handle.join().unwrap().expect("the handshake completes");
        // The create arguments open with the sample spec, and the stream starts running.
        assert_eq!(&create.args[..7], &[b'a', 5, 2, 0, 0, 0xbb, 0x80]);
        assert_eq!(create.args[22], b'0');
        assert_eq!(playback.channel(), 0);

        // 4096 bytes of credit releases 1024 samples; the rest waits for a REQUEST.
        let samples: Vec<f32> = (0..4096).map(|i| i as f32).collect();
        assert_eq!(playback.write(&samples), Some(4096));
        let first = mock.memblock_within(Duration::from_millis(200));
        assert_eq!(as_f32s(&first), samples[..1024].to_vec());
        mock.request_bytes(0, 100_000);
        let rest = mock.memblock_within(Duration::from_millis(200));
        assert_eq!(as_f32s(&rest), samples[1024..].to_vec());

        // The clock: nothing until the server reports playing, then the read
        // index less the sink's latency.
        let (position, playback) = ask_position(playback, &mut mock, 20_000, false, 0);
        assert_eq!(position, None);
        let (position, playback) = ask_position(playback, &mut mock, 20_000, true, 48_000 * 8);
        assert_eq!(position, Some(48_000 - 960));

        // A lost server stops the writes.
        mock.close();
        std::thread::sleep(Duration::from_millis(100));
        let mut playback = playback;
        assert_eq!(playback.write(&stereo_samples(4, 0.0)), None);
    }

    #[test]
    fn pause_volume_flush_underflow_and_close() {
        let (mut mock, path) = Mock::bind("controls");
        let handle = open(path);
        mock.handshake(1 << 20);
        let playback = handle.join().unwrap().unwrap();

        // A dry stream before anything was written is the head of the start.
        mock.underflow(0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(playback.underruns(), (0, None));

        let (ok, playback, args) = answered(&mut mock, playback, command::CORK_PLAYBACK_STREAM, |p| p.set_active(true));
        assert!(ok);
        assert_eq!(args, Tags::new().u32(0).bool(false).into_bytes());
        let (ok, mut playback, args) = answered(&mut mock, playback, command::CORK_PLAYBACK_STREAM, |p| p.set_active(false));
        assert!(ok);
        assert_eq!(args, Tags::new().u32(0).bool(true).into_bytes());

        // Gain scales what is sent; out of range is refused.
        assert!(playback.set_volume(0.5));
        assert!(!playback.set_volume(1.5));
        assert_eq!(playback.write(&stereo_samples(2, 1.0)), Some(4));
        assert_eq!(as_f32s(&mock.memblock_within(Duration::from_millis(100))), vec![0.5; 4]);

        // Fed, a dry stream is late and counted.
        mock.underflow(0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(playback.underruns().0, 1);

        // A flush drops what is queued, here and on the server.
        let (ok, playback, _) = answered(&mut mock, playback, command::FLUSH_PLAYBACK_STREAM, |p| p.flush());
        assert!(ok);
        mock.request_bytes(0, 100_000);
        assert!(mock.memblock_within(Duration::from_millis(100)).is_empty());

        // Dropping the stream deletes it on the server.
        drop(playback);
        let delete = mock.expect(command::DELETE_PLAYBACK_STREAM);
        assert_eq!(delete.args, Tags::new().u32(0).into_bytes());
    }

    #[test]
    fn no_server_means_no_stream_and_the_path_follows_the_environment() {
        let runtime = OsStr::new("/run/user/1000");
        assert_eq!(
            server_path_from(None, Some(runtime)),
            Some(PathBuf::from("/run/user/1000/pulse/native"))
        );
        assert_eq!(
            server_path_from(Some("unix:/tmp/x/native"), Some(runtime)),
            Some(PathBuf::from("/tmp/x/native"))
        );
        assert_eq!(server_path_from(Some("tcp:127.0.0.1:4713"), Some(runtime)), None);
        let path = std::env::temp_dir().join(format!("edith-pulse-absent-{}", std::process::id()));
        assert!(Playback::open_at(&path, 48_000, 2).is_none());
    }

    /// Asks the playback for its position on another thread while the test
    /// answers the GET_PLAYBACK_LATENCY it sends.
    fn ask_position(
        playback: Playback,
        mock: &mut Mock,
        sink_usec: u64,
        playing: bool,
        read_index: i64,
    ) -> (Option<i64>, Playback) {
        let asking = std::thread::spawn(move || {
            let position = playback.position();
            (position, playback)
        });
        let latency = mock.expect(command::GET_PLAYBACK_LATENCY);
        mock.reply(latency.tag, latency_reply(sink_usec, playing, read_index));
        asking.join().unwrap()
    }

    /// The GET_PLAYBACK_LATENCY reply: sink and source latency, playing, the two
    /// timevals, the write and read indexes, then the two underrun counters.
    fn latency_reply(sink_usec: u64, playing: bool, read_index: i64) -> Tags {
        Tags::new()
            .usec(sink_usec)
            .usec(0)
            .bool(playing)
            .timeval(0, 0)
            .timeval(0, 0)
            .i64(read_index)
            .i64(read_index)
            .u64(0)
            .u64(0)
    }

}
