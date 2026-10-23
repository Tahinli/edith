//! DTS and the legacy audio codecs the family decodes, seated in the engine.
//!
//! Two pieces live here, both over the family's own crates:
//!
//! - [`ExtDecoder`]: one packet in, interleaved f32 out, for DTS (`ec-dts`: core
//!   5.1, XCH 6.1, X96, 14-bit and little-endian words; a DTS-HD stream decodes
//!   its backward compatible core and an XLL layer is refused by name) and for
//!   what `ec-probe`'s decoder registry carries: IMA and MS ADPCM, QuickTime
//!   `ima4`, A-law, mu-law and AMR-NB/WB (`ec-pcm-ext`, `ec-amr`).
//!   The transport stream and AVI audio door ([`crate::demux::ts::TsDecoder`])
//!   feeds it packets out of its own demuxer.
//! - [`ExtFileTrack`]: one such audio stream read through `ec_probe::Reader`,
//!   for the containers whose audio no other reader here takes: a Matroska
//!   `A_DTS` track, a QuickTime / 3GP / mp4 track in one of these codecs, a
//!   WAVE file that is not plain PCM (or is DTS), a raw `.dts`, a raw `.amr`.
//!
//! The channel order every decoder here hands back is the film order the
//! engine's stereo fold expects (FL FR FC LFE BL BR SL SR); the one layout that
//! does not arrive in it, DTS 6.1 (L R C LFE Ls Rs Cs), is permuted to it.

use std::io::Read;
use std::path::{Path, PathBuf};

use ec_core::registry::SeekMode;
use ec_core::timebase::{Rounding, TimeBase, Timestamp};
use ec_core::{CodecId, CodecParameters, MediaParameters, MediaType, Packet};

/// Whether `codec` is one this module decodes.
pub(crate) fn is_ext_codec(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::Dts
            | CodecId::ImaAdpcmWav
            | CodecId::MsAdpcm
            | CodecId::ImaAdpcmQt
            | CodecId::PcmAlaw
            | CodecId::PcmMulaw
            | CodecId::AmrNb
            | CodecId::AmrWb
            // MPEG-1/2 audio layers I to III: one `ec-mp3` decoder reads all three.
            | CodecId::Mp1
            | CodecId::Mp2
            | CodecId::Mp3
    )
}

/// Whether a Matroska codec id names DTS (`A_DTS`, `A_DTS/EXPRESS`, `A_DTS/LOSSLESS`).
pub(crate) fn is_mkv_dts(id: &str) -> bool {
    id == "A_DTS" || id.starts_with("A_DTS/")
}

/// Whether a Matroska codec id names an MPEG-1/2 Layer I or II track (`A_MPEG/L1`, `A_MPEG/L2`),
/// which [`ExtFileTrack::open_native`] reads and decodes.
pub(crate) fn is_mkv_mpeg_layer(id: &str) -> bool {
    matches!(id, "A_MPEG/L1" | "A_MPEG/L2")
}

enum Inner {
    Dts(Box<ec_dts::DtsDecoder>),
    Probe(Box<ec_probe::AudioDecoder>),
}

/// A decoder for one of the codecs of [`is_ext_codec`], fed one packet at a time.
pub(crate) struct ExtDecoder {
    inner: Inner,
    scratch: Vec<f32>,
}

impl ExtDecoder {
    pub(crate) fn new(params: &CodecParameters) -> crate::Result<Self> {
        let inner = match params.codec {
            CodecId::Dts => Inner::Dts(Box::new(ec_dts::DtsDecoder::with_options(ec_dts::Options {
                // A lossless DTS-HD MA track is named, not silently played as
                // its lossy core.
                refuse_xll: true,
            }))),
            codec if is_ext_codec(codec) => Inner::Probe(Box::new(
                ec_probe::AudioDecoder::new(params).map_err(|e| format!("{}: {e}", codec.name()))?,
            )),
            codec => return Err(format!("{}: no decoder in the extended audio seat", codec.name()).into()),
        };
        Ok(Self {
            inner,
            scratch: Vec::new(),
        })
    }

    /// One packet in. `Ok(None)` when it decodes to nothing yet; otherwise the
    /// interleaved samples and the channel count they were decoded at.
    pub(crate) fn decode(&mut self, data: &[u8]) -> crate::Result<Option<(Vec<f32>, usize)>> {
        match &mut self.inner {
            Inner::Dts(d) => match d.decode_frame(data) {
                Ok(frame) => {
                    let channels = frame.layout.channel_count().max(1);
                    let mut samples: Vec<f32> = frame
                        .data
                        .first()
                        .into_iter()
                        .flat_map(|plane| plane.as_chunks::<4>().0.iter())
                        .map(|s| f32::from_le_bytes(*s))
                        .collect();
                    if channels == 7 {
                        // L R C LFE Ls Rs Cs -> L R C LFE Cs Ls Rs, the 6.1 order of the fold.
                        for f in samples.as_chunks_mut::<7>().0 {
                            f[4..].rotate_right(1);
                        }
                    }
                    Ok((!samples.is_empty()).then_some((samples, channels)))
                }
                Err(ec_core::Error::NeedMore) => Ok(None),
                Err(e) => Err(format!("DTS: {e}").into()),
            },
            Inner::Probe(d) => {
                let packet = Packet::new(0, TimeBase::new(1, 1000), data.to_vec());
                match d.decode(&packet, &mut self.scratch) {
                    Ok(()) => {}
                    Err(ec_core::Error::NeedMore) => return Ok(None),
                    Err(e) => return Err(format!("{}: {e}", d.codec().name()).into()),
                }
                let channels = d.channels().max(1);
                Ok((!self.scratch.is_empty()).then(|| (std::mem::take(&mut self.scratch), channels)))
            }
        }
    }
}

/// Which audio stream of a file to open.
#[derive(Clone, Copy)]
pub(crate) enum Pick {
    /// The n-th audio stream, in file order.
    Nth(usize),
    /// The stream with this container-native id (a Matroska track number).
    Native(u64),
}

/// One audio stream of a file, read through `ec_probe::Reader`.
pub(crate) struct ExtFileTrack {
    path: PathBuf,
    reader: ec_probe::Reader,
    stream: u32,
    pick: Pick,
    params: CodecParameters,
    time_base: TimeBase,
    /// What the decoder emits.
    pub(crate) sample_rate: u32,
    pub(crate) src_channels: usize,
    pub(crate) total_samples: Option<u64>,
    /// Samples at the head the container says are not audible (an edit list's
    /// start, an encoder delay): media position of the first audible sample.
    pub(crate) priming: u64,
    /// Seconds a decoder is run ahead of the first sample it is asked for.
    pub(crate) pre_roll_secs: f64,
}

/// The formats worth the cost of opening a reader for: the ones that carry
/// these codecs and that a standalone-audio or mp4 open would otherwise misread.
fn sniffs_as_ext(path: &Path) -> bool {
    let mut head = vec![0u8; 64 << 10];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut n = 0;
    while n < head.len() {
        match file.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(m) => n += m,
        }
    }
    matches!(
        ec_probe::sniff(&head[..n]),
        Some(
            ec_probe::Format::Wav
                | ec_probe::Format::Dts
                | ec_probe::Format::Amr
                | ec_probe::Format::Mp4
                | ec_probe::Format::Matroska
        )
    )
}

impl ExtFileTrack {
    /// The `index`-th audio stream of `path` when it is in one of the codecs
    /// here, `Ok(None)` when it is anything else (or not a file this reads).
    pub(crate) fn open(path: &Path, index: usize) -> crate::Result<Option<Self>> {
        if !sniffs_as_ext(path) {
            return Ok(None);
        }
        Self::open_pick(path, Pick::Nth(index))
    }

    /// The Matroska track numbered `number`, `Ok(None)` when it is not DTS.
    pub(crate) fn open_native(path: &Path, number: u64) -> crate::Result<Option<Self>> {
        Self::open_pick(path, Pick::Native(number))
    }

    fn open_pick(path: &Path, pick: Pick) -> crate::Result<Option<Self>> {
        let reader = match ec_probe::Reader::open(path) {
            Ok(reader) => reader,
            Err(_) => return Ok(None),
        };
        let audio: Vec<_> = reader
            .streams()
            .iter()
            .filter(|s| s.params.codec.media_type() == MediaType::Audio)
            .collect();
        let info = match pick {
            Pick::Nth(n) => audio.get(n).copied(),
            Pick::Native(id) => reader
                .stream_of_native(id)
                .and_then(|i| audio.iter().find(|s| s.index == i).copied()),
        };
        let Some(info) = info else {
            return Ok(None);
        };
        if !is_ext_codec(info.params.codec) {
            return Ok(None);
        }
        let reader_is_mp4 = reader.format() == ec_probe::Format::Mp4;
        let (stream, params, time_base) = (info.index, info.params.clone(), info.time_base);
        let duration = info.duration;
        let codec = info.params.codec;
        let padding = u64::from(info.initial_padding);
        let (mut rate, mut channels) = match &params.media {
            MediaParameters::Audio(a) => (a.sample_rate, a.layout.channel_count()),
            _ => (0, 0),
        };
        let mut track = Self {
            path: path.to_path_buf(),
            reader,
            stream,
            pick,
            params,
            time_base,
            sample_rate: rate,
            src_channels: channels.max(1),
            total_samples: None,
            priming: padding,
            pre_roll_secs: match codec {
                CodecId::Dts => 0.2,
                CodecId::AmrNb | CodecId::AmrWb => 0.5,
                _ => 0.0,
            },
        };
        // DTS states its rate and width in the stream, and a container's header
        // for it is often a default: the first frame that decodes is the truth.
        // Decoding a few packets also makes a refusal (DTS-HD XLL) the open's.
        // An mp4 / QuickTime edit list starts the presentation `media_time` into the
        // media: those samples are not heard (AMR's and ADPCM's muxers write one).
        if reader_is_mp4
            && let Pick::Nth(n) = pick
            && let Ok(mp4) = crate::mp4track::open(path)
            && let Some(&(track_id, Some(stream))) = crate::mp4track::audio_tracks(&mp4).get(n)
            && let Some(t) = crate::mp4track::media_time(&mp4, track_id)
        {
            let scale = u64::from(crate::mp4track::timescale(&mp4, stream).max(1));
            track.priming = t * u64::from(track.sample_rate.max(1)) / scale;
        }
        let mut decoder = track.decoder()?;
        for _ in 0..64 {
            let Some((_, bytes)) = track.next_packet()? else {
                break;
            };
            if let Some((_, c)) = decoder.decode(&bytes)? {
                channels = c;
                break;
            }
        }
        if let Inner::Dts(d) = &decoder.inner
            && let Some(info) = d.frame_info()
        {
            rate = info.sample_rate;
        }
        track.reopen()?;
        if rate == 0 {
            return Err(format!("{}: the audio track states no sample rate", path.display()).into());
        }
        track.sample_rate = rate;
        track.priming = track.priming.min(u64::MAX / 2);
        track.src_channels = channels.max(1);
        track.total_samples = duration
            .filter(|&d| d > 0)
            .map(|d| (time_base.rescale(d, TimeBase::from_rate(rate), Rounding::Down).max(0) as u64).saturating_sub(track.priming));
        Ok(Some(track))
    }

    /// What reaches the timeline: mono, or a pair for anything else.
    pub(crate) fn channels(&self) -> u16 {
        if self.src_channels == 1 { 1 } else { 2 }
    }

    pub(crate) fn codec_name(&self) -> &'static str {
        self.params.codec.name()
    }

    pub(crate) fn decoder(&self) -> crate::Result<ExtDecoder> {
        ExtDecoder::new(&self.params)
    }

    fn reopen(&mut self) -> crate::Result<()> {
        let reader = ec_probe::Reader::open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.stream = match self.pick {
            Pick::Nth(n) => reader
                .streams()
                .iter()
                .filter(|s| s.params.codec.media_type() == MediaType::Audio)
                .nth(n)
                .map(|s| s.index),
            Pick::Native(id) => reader.stream_of_native(id),
        }
        .ok_or("the audio stream vanished on reopen")?;
        self.reader = reader;
        Ok(())
    }

    /// Positions the track so the next packet starts at or before `secs`.
    pub(crate) fn seek(&mut self, secs: f64) -> crate::Result<()> {
        let secs = secs - self.pre_roll_secs;
        if secs <= 0.05 {
            return self.reopen();
        }
        let ticks = (secs * self.time_base.den() as f64 / self.time_base.num().max(1) as f64) as i64;
        match self
            .reader
            .seek(self.stream, Timestamp::new(ticks, self.time_base), SeekMode::Exact)
        {
            Ok(_) => Ok(()),
            Err(_) => self.reopen(),
        }
    }

    /// The next packet of the stream: its start in samples, when the container
    /// states one, and its bytes. `Ok(None)` at the end.
    pub(crate) fn next_packet(&mut self) -> crate::Result<Option<(Option<u64>, Vec<u8>)>> {
        loop {
            match self.reader.next_packet() {
                Ok(p) if p.stream == self.stream => {
                    let pos = p
                        .pts
                        .or(p.dts)
                        .map(|t| p.time_base.rescale(t, TimeBase::from_rate(self.sample_rate), Rounding::Nearest).max(0) as u64);
                    return Ok(Some((pos, p.data.as_ref().to_vec())));
                }
                Ok(_) => {}
                Err(ec_core::Error::Eof) => return Ok(None),
                Err(e) => return Err(format!("{}: {e}", self.path.display()).into()),
            }
        }
    }
}
