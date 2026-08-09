//! Video engine: demux -> decode -> pixel convert. No UI dependencies.

pub mod ao;
pub mod audio;
pub mod clock;
pub mod convert;
pub mod decode;
pub mod demux;
pub mod hw;
pub mod playback;

pub use audio::{AudioChunk, AudioMeta, AudioSession};
pub use clock::PlaybackClock;
pub use playback::PlaybackSession;
pub use decode::{DecodeSession, Frame};
pub use demux::VideoMeta;

/// Boxed error; the engine has few failure modes and every one of them is fatal
/// to the session, so a message is all a caller can act on.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
