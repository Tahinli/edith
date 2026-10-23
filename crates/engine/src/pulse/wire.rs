//! The PulseAudio native protocol's wire format: packet framing and the tagged
//! structs every command, reply and event is written in.
//!
//! Everything is big-endian. A packet is a 20-byte header (payload length,
//! channel, offset, flags) and the payload. Control packets travel on channel
//! `u32::MAX`; a playback stream's samples go out as memblock packets on the
//! channel the server named in its CREATE reply. Values are tagged: `L` is a
//! u32, `r` an i64, `1`/`0` a bool, `t` a NUL-terminated string, `x` a
//! length-prefixed blob, and so on. The layouts here were read off a capture of
//! libpulse talking to pipewire-pulse at protocol version 35.

#![forbid(unsafe_code)]

/// Channel number of every control packet.
pub const CONTROL_CHANNEL: u32 = u32::MAX;
/// Bytes in a packet header.
pub const HEADER_LEN: usize = 20;
/// The protocol version this client speaks. Its CREATE_PLAYBACK_STREAM layout
/// is the one written by [`create_playback_stream_args`].
pub const PROTOCOL_VERSION: u32 = 35;
/// `PA_SAMPLE_FLOAT32LE`.
pub const SAMPLE_FORMAT_FLOAT32LE: u8 = 5;
/// `PA_VOLUME_NORM`: the stream's own volume, left at unity.
pub const VOLUME_NORM: u32 = 0x10000;
/// `PA_INVALID_INDEX`, and the "server's choice" value of the buffer attributes.
pub const DEFAULT: u32 = u32::MAX;

/// Command numbers (PulseAudio's `PA_COMMAND_*`), the ones this client uses.
pub mod command {
    pub const ERROR: u32 = 0;
    pub const REPLY: u32 = 2;
    pub const CREATE_PLAYBACK_STREAM: u32 = 3;
    pub const DELETE_PLAYBACK_STREAM: u32 = 4;
    pub const AUTH: u32 = 8;
    pub const SET_CLIENT_NAME: u32 = 9;
    pub const GET_PLAYBACK_LATENCY: u32 = 14;
    pub const CORK_PLAYBACK_STREAM: u32 = 41;
    pub const FLUSH_PLAYBACK_STREAM: u32 = 42;
    pub const CREATE_RECORD_STREAM: u32 = 5;
    pub const DELETE_RECORD_STREAM: u32 = 6;
    pub const GET_SOURCE_INFO_LIST: u32 = 24;
    pub const CORK_RECORD_STREAM: u32 = 58;
    /// Server to client: the stream has room for `length` more bytes.
    pub const REQUEST: u32 = 61;
    /// Server to client: the stream ran dry while it was playing.
    pub const UNDERFLOW: u32 = 63;
    /// Server to client: the server removed the stream.
    pub const PLAYBACK_STREAM_KILLED: u32 = 64;
    /// Server to client: the server removed the recording stream.
    pub const RECORD_STREAM_KILLED: u32 = 65;
}

/// The fixed fields of a packet header, as [`header`] writes them.
pub fn header(len: u32, channel: u32, offset: i64, flags: u32) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[0..4].copy_from_slice(&len.to_be_bytes());
    out[4..8].copy_from_slice(&channel.to_be_bytes());
    out[8..16].copy_from_slice(&offset.to_be_bytes());
    out[16..20].copy_from_slice(&flags.to_be_bytes());
    out
}

/// Splits a header into `(payload length, channel, offset, flags)`.
pub fn parse_header(bytes: &[u8; HEADER_LEN]) -> (u32, u32, i64, u32) {
    let be32 = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    let offset = i64::from_be_bytes(bytes[8..16].try_into().unwrap());
    (be32(0), be32(4), offset, be32(16))
}

/// A whole control packet: header, then the command and tag, then `args`.
pub fn control(command: u32, tag: u32, args: Tags) -> Vec<u8> {
    let payload = Tags::new().u32(command).u32(tag).extend(args).into_bytes();
    let mut packet = header(payload.len() as u32, CONTROL_CHANNEL, 0, 0).to_vec();
    packet.extend_from_slice(&payload);
    packet
}

/// Builds a tagged struct, one value at a time.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Tags(Vec<u8>);

impl Tags {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// Appends another struct's values after these.
    pub fn extend(mut self, other: Tags) -> Self {
        self.0.extend(other.0);
        self
    }

    pub fn u32(mut self, value: u32) -> Self {
        self.0.push(b'L');
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// A usec: the `U` tag, a u64.
    pub fn usec(mut self, value: u64) -> Self {
        self.0.push(b'U');
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    /// A plain u64, the `R` tag.
    pub fn u64(mut self, value: u64) -> Self {
        self.0.push(b'R');
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn i64(mut self, value: i64) -> Self {
        self.0.push(b'r');
        self.0.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn u8(mut self, value: u8) -> Self {
        self.0.push(b'B');
        self.0.push(value);
        self
    }

    pub fn bool(mut self, value: bool) -> Self {
        self.0.push(if value { b'1' } else { b'0' });
        self
    }

    /// A string, NUL-terminated on the wire.
    pub fn string(mut self, value: &str) -> Self {
        self.0.push(b't');
        self.0.extend_from_slice(value.as_bytes());
        self.0.push(0);
        self
    }

    /// The null string: "no name", the server's default.
    pub fn null(mut self) -> Self {
        self.0.push(b'N');
        self
    }

    /// A length-prefixed blob, such as the authentication cookie.
    pub fn arbitrary(mut self, value: &[u8]) -> Self {
        self.0.push(b'x');
        self.0.extend_from_slice(&(value.len() as u32).to_be_bytes());
        self.0.extend_from_slice(value);
        self
    }

    pub fn sample_spec(mut self, format: u8, channels: u8, rate: u32) -> Self {
        self.0.push(b'a');
        self.0.push(format);
        self.0.push(channels);
        self.0.extend_from_slice(&rate.to_be_bytes());
        self
    }

    pub fn channel_map(mut self, positions: &[u8]) -> Self {
        self.0.push(b'm');
        self.0.push(positions.len() as u8);
        self.0.extend_from_slice(positions);
        self
    }

    /// A channel volume: one u32 per channel.
    pub fn cvolume(mut self, volumes: &[u32]) -> Self {
        self.0.push(b'v');
        self.0.push(volumes.len() as u8);
        for volume in volumes {
            self.0.extend_from_slice(&volume.to_be_bytes());
        }
        self
    }

    /// A timeval: seconds and microseconds, two u32s.
    pub fn timeval(mut self, sec: u32, usec: u32) -> Self {
        self.0.push(b'T');
        self.0.extend_from_slice(&sec.to_be_bytes());
        self.0.extend_from_slice(&usec.to_be_bytes());
        self
    }

    /// A property list: each key with its value (NUL included in the length),
    /// closed by the null string.
    pub fn proplist(mut self, props: &[(&str, &str)]) -> Self {
        self.0.push(b'P');
        for (key, value) in props {
            self = self.string(key);
            let len = value.len() as u32 + 1;
            self = self.u32(len);
            self.0.push(b'x');
            self.0.extend_from_slice(&len.to_be_bytes());
            self.0.extend_from_slice(value.as_bytes());
            self.0.push(0);
        }
        self.0.push(b'N');
        self
    }
}

/// Reads a tagged struct back, value by value. Each reader returns `None` when
/// the next value is not the kind asked for, or the payload runs out.
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// The bytes not read yet.
    pub fn rest(&self) -> &'a [u8] {
        self.buf
    }

    fn take(&mut self, tag: u8, len: usize) -> Option<&'a [u8]> {
        let (&first, rest) = self.buf.split_first()?;
        if first != tag || rest.len() < len {
            return None;
        }
        let (value, after) = rest.split_at(len);
        self.buf = after;
        Some(value)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let value = self.take(b'L', 4)?;
        Some(u32::from_be_bytes(value.try_into().ok()?))
    }

    pub fn u64(&mut self) -> Option<u64> {
        let value = self.take(b'U', 8)?;
        Some(u64::from_be_bytes(value.try_into().ok()?))
    }

    pub fn i64(&mut self) -> Option<i64> {
        let value = self.take(b'r', 8)?;
        Some(i64::from_be_bytes(value.try_into().ok()?))
    }

    pub fn bool(&mut self) -> Option<bool> {
        match self.buf.first()? {
            b'1' => self.take(b'1', 0).map(|_| true),
            b'0' => self.take(b'0', 0).map(|_| false),
            _ => None,
        }
    }

    pub fn timeval(&mut self) -> Option<()> {
        self.take(b'T', 8).map(|_| ())
    }

    pub fn string(&mut self) -> Option<String> {
        let (&first, rest) = self.buf.split_first()?;
        if first != b't' {
            return None;
        }
        let end = rest.iter().position(|&b| b == 0)?;
        let text = String::from_utf8(rest[..end].to_vec()).ok()?;
        self.buf = &rest[end + 1..];
        Some(text)
    }

    /// A length-prefixed blob: the tag, a raw big-endian length, then the bytes.
    pub fn arbitrary(&mut self) -> Option<&'a [u8]> {
        let len = self.take(b'x', 4)?;
        let len = u32::from_be_bytes(len.try_into().ok()?) as usize;
        if self.buf.len() < len {
            return None;
        }
        let (value, after) = self.buf.split_at(len);
        self.buf = after;
        Some(value)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(b'B', 1).map(|v| v[0])
    }

    /// A string that may be the null string (`N`), which reads as `None`.
    pub fn string_or_null(&mut self) -> Option<Option<String>> {
        match self.buf.first()? {
            b'N' => self.take(b'N', 0).map(|_| None),
            _ => self.string().map(Some),
        }
    }

    /// A sample spec: the tag, format, channels and rate. Skipped by the source list.
    pub fn sample_spec(&mut self) -> Option<()> {
        let (&first, rest) = self.buf.split_first()?;
        if first != b'a' || rest.len() < 6 {
            return None;
        }
        self.buf = &rest[6..];
        Some(())
    }

    /// A channel map: the tag, a count, then one position byte per channel.
    pub fn channel_map(&mut self) -> Option<()> {
        let n = self.take(b'm', 1)?[0] as usize;
        self.take_raw(n).map(|_| ())
    }

    /// A channel volume: the tag, a count, then one u32 per channel.
    pub fn cvolume(&mut self) -> Option<()> {
        let n = self.take(b'v', 1)?[0] as usize;
        self.take_raw(n * 4).map(|_| ())
    }

    /// A volume, the `V` tag and a u32.
    pub fn volume(&mut self) -> Option<u32> {
        let value = self.take(b'V', 4)?;
        Some(u32::from_be_bytes(value.try_into().ok()?))
    }

    /// A property list, read and dropped: its keys and values in turn, closed by
    /// the null string.
    pub fn skip_proplist(&mut self) -> Option<()> {
        self.take(b'P', 0)?;
        while self.string_or_null()?.is_some() {
            self.u32()?;
            self.arbitrary()?;
        }
        Some(())
    }

    /// A format info: the `f` tag, an encoding byte and a property list. Skipped
    /// by the source list.
    pub fn format_info(&mut self) -> Option<()> {
        self.take(b'f', 0)?;
        self.u8()?;
        self.skip_proplist()
    }

    /// Bytes with no tag of their own, of exactly `len`.
    fn take_raw(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.buf.len() < len {
            return None;
        }
        let (value, after) = self.buf.split_at(len);
        self.buf = after;
        Some(value)
    }
}

/// Arguments of CREATE_RECORD_STREAM at protocol version 35, in the order the
/// server reads them (`command_create_record_stream` in protocol-native.c). The
/// server's default source is used, and the stream starts running.
pub fn create_record_stream_args(rate: u32, channels: u8, source: Option<&str>, fragsize: u32) -> Tags {
    let positions: Vec<u8> = match channels {
        1 => vec![0],
        2 => vec![1, 2],
        n => (0..n).map(|i| 12 + i).collect(),
    };
    let source = match source {
        Some(name) => Tags::new().string(name),
        None => Tags::new().null(),
    };
    let mut tags = Tags::new()
        .sample_spec(SAMPLE_FORMAT_FLOAT32LE, channels, rate)
        .channel_map(&positions)
        .u32(DEFAULT)
        .extend(source)
        .u32(DEFAULT)
        .bool(false)
        .u32(fragsize);
    // no_remap, no_remix, fix_format, fix_rate, fix_channels, no_move, variable_rate
    for _ in 0..7 {
        tags = tags.bool(false);
    }
    // peak_detect, adjust_latency, then the proplist, then direct_on_input.
    tags = tags.bool(false).bool(false).proplist(&[
        ("application.name", crate::pulse::APP_NAME),
        ("media.name", "edith voice-over"),
    ]);
    tags = tags.u32(DEFAULT).bool(false).bool(false).bool(false);
    // No formats, then the volume (unset), muted, volume_set, muted_set,
    // relative_volume and passthrough.
    tags = tags.u8(0).cvolume(&vec![VOLUME_NORM; channels as usize]);
    for _ in 0..5 {
        tags = tags.bool(false);
    }
    tags
}

/// Arguments of CREATE_PLAYBACK_STREAM at protocol version 35, in the order the
/// server reads them. Flags are all off, as libpulse sends them by default, and
/// the stream starts running, as the old plugin's did: the caller pauses it.
pub fn create_playback_stream_args(
    rate: u32,
    channels: u8,
    tlength: u32,
    minreq: u32,
    volume: u32,
) -> Tags {
    let positions: Vec<u8> = match channels {
        1 => vec![0],
        2 => vec![1, 2],
        n => (0..n).map(|i| 12 + i).collect(),
    };
    let mut tags = Tags::new()
        .sample_spec(SAMPLE_FORMAT_FLOAT32LE, channels, rate)
        .channel_map(&positions)
        .u32(DEFAULT)
        .null()
        .u32(DEFAULT)
        .bool(false)
        .u32(tlength)
        .u32(tlength)
        .u32(minreq)
        .u32(0)
        .cvolume(&vec![volume; channels as usize]);
    for _ in 0..9 {
        tags = tags.bool(false);
    }
    tags = tags.proplist(&[
        ("application.name", crate::pulse::APP_NAME),
        ("media.name", "edith playback"),
    ]);
    for _ in 0..7 {
        tags = tags.bool(false);
    }
    tags.u8(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trips_and_matches_the_capture() {
        let bytes = header(0x0102, 7, -3, 9);
        assert_eq!(&bytes[0..4], &[0, 0, 1, 2]);
        assert_eq!(parse_header(&bytes), (0x0102, 7, -3, 9));

        let bytes = Tags::new()
            .u32(7)
            .string("edith")
            .bool(true)
            .i64(-5)
            .arbitrary(&[1, 2, 3])
            .timeval(1, 2)
            .into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.u32(), Some(7));
        assert_eq!(r.string().as_deref(), Some("edith"));
        assert_eq!(r.bool(), Some(true));
        assert_eq!(r.i64(), Some(-5));
        assert_eq!(r.arbitrary(), Some(&[1u8, 2, 3][..]));
        assert_eq!(r.timeval(), Some(()));
        assert_eq!(r.u32(), None, "a wrong tag is refused");

        // The create arguments match the layout captured from libpulse.
        let args = create_playback_stream_args(48_000, 2, 38_400, 1_920, VOLUME_NORM).into_bytes();
        assert_eq!(&args[..7], &[b'a', 5, 2, 0, 0, 0xbb, 0x80]);
        assert_eq!(&args[7..11], &[b'm', 2, 1, 2]);
        assert_eq!(args[22], b'0');
        assert_eq!(args.last(), Some(&0));
    }
}
