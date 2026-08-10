//! Video engine: demux -> decode -> pixel convert. No UI dependencies.

pub mod ao;
pub mod audio;
pub mod clock;
pub mod convert;
pub mod decode;
pub mod demux;
pub mod edith;
pub mod export;
pub mod hw;
pub mod mux;
pub mod playback;
pub mod project;
pub mod waveform;

pub use audio::{AacPacket, AacTrackParams, AudioChunk, AudioMeta, AudioProbe, AudioSession};
pub use clock::PlaybackClock;
pub use decode::{DecodeSession, Frame};
pub use demux::VideoMeta;
pub use export::ExportHandle;
pub use mux::{AudioParams, Mp4Muxer, VideoParams};
pub use playback::PlaybackSession;
pub use project::{Clip, Project};

/// Whether a path names a standalone audio file -- a source with no picture,
/// which belongs on the audio lane and nowhere else. Extension only, lowercased:
/// the decoder is what really decides, but a front-end has to know which lane a
/// dropped file may land on *before* anything is opened, and so does
/// [`PlaybackSession::import`](playback::PlaybackSession::import).
///
/// Exactly the containers the engine's audio path reads (`audio::AudioSession`):
/// no opus, ac3 or dts, for which no pure-Rust decoder exists -- those are
/// refused by the same door that refuses a `.txt`, which is the honest answer.
pub fn is_audio(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac"
        )
    })
}

/// Boxed error; the engine has few failure modes and every one of them is fatal
/// to the session, so a message is all a caller can act on.
pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn is_audio_admits_the_containers_the_decoder_reads() {
        for name in [
            "song.mp3", "a.WAV", "b.flac", "c.ogg", "d.oga", "e.m4a", "f.aac",
        ] {
            assert!(super::is_audio(Path::new(name)), "{name}");
        }
        // Video, projects, the formats with no pure-Rust decoder, and a file
        // with no extension at all: all of them go elsewhere.
        for name in [
            "clip.mp4",
            "take.MP4",
            "cut.edith",
            "x.opus",
            "y.ac3",
            "notes",
            "mp3",
        ] {
            assert!(!super::is_audio(Path::new(name)), "{name}");
        }
    }
}
