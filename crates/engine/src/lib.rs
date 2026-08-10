//! Video engine: demux -> decode -> pixel convert. No UI dependencies.

pub mod ao;
pub mod audio;
pub mod clock;
pub mod convert;
pub mod decode;
pub mod demux;
pub mod edith;
pub mod eq;
pub mod export;
pub mod hw;
pub mod mux;
pub mod playback;
pub mod project;
pub mod waveform;

pub use audio::{
    AacPacket, AacTrackParams, AudioChunk, AudioMeta, AudioProbe, AudioSession, StreamInfo,
};
pub use clock::PlaybackClock;
pub use decode::{DecodeSession, Frame};
pub use demux::{Codec, VideoMeta};
pub use export::ExportHandle;
pub use mux::{AudioParams, Mp4Muxer, VideoParams};
pub use playback::PlaybackSession;
pub use project::{Clip, Project};

/// Boxed error; the engine has few failure modes and every one of them is fatal
/// to the session, so a message is all a caller can act on.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
