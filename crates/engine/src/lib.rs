//! Video engine: demux -> decode -> pixel convert. No UI dependencies.

pub mod ao;
pub mod audio;
pub mod caps;
pub mod clock;
pub mod color;
pub mod colorspace;
pub mod convert;
pub mod decode;
pub mod demux;
pub mod edith;
pub mod eq;
pub mod export;
pub mod hw;
pub mod hwproc;
pub mod limiter;
pub mod mux;
pub mod playback;
pub mod project;
pub mod proxy;
pub mod real_library;
pub mod scale;
pub mod scratch;
pub mod silence;
pub mod subtitle;
pub(crate) mod map;
#[cfg(test)]
mod sweep;
pub(crate) mod stretch;
pub mod tonemap;
pub mod transform;
pub mod waveform;

pub use audio::{
    AacPacket, AacTrackParams, AudioChunk, AudioMeta, AudioProbe, AudioSession, StreamInfo,
};
pub use clock::PlaybackClock;
pub use decode::{DecodeSession, Frame, image_size};
pub use demux::{Codec, MediaBitrate, Rotation, UnsupportedRotation, VideoMeta, probe_bitrate};
pub use export::ExportHandle;
pub use mux::{AudioParams, Mp4Muxer, VideoParams};
pub use playback::{ImportGate, ImportProbe, PlaybackSession};
pub use project::{Clip, Project, Rate, Speed};

/// Whether a path names a standalone audio file -- a source with no picture,
/// which belongs on the audio lane and nowhere else. Extension only, lowercased:
/// the decoder is what really decides, but a front-end has to know which lane a
/// dropped file may land on *before* anything is opened, and so does
/// [`PlaybackSession::import`](playback::PlaybackSession::import).
///
/// Exactly the containers the engine's audio path reads (`audio::AudioSession`),
/// Opus included: `.opus` is an Ogg file symphonia's probe already reads and
/// `ec-opus` decodes, and `.mka` is the Matroska whose sound is all it
/// has. A `.dts` or a bare `.ac3` is refused by the same door that refuses a
/// `.txt` -- there is no reader here that opens an elementary stream -- which is
/// the honest answer.
///
/// The `.mpeg` and `.mpg` spellings are on the list because an MPEG audio
/// stream has no container to name it and the file is whatever a recorder
/// called it: those two are the *standard's* name -- what a voice note or a
/// tagged download off the web wears -- where `.mp3` is the codec's. Symphonia's
/// `mp3` reader is what reads them, the same reader an `.mp3` goes through, and
/// it skips the ID3v2 tag and any cover picture muxed into one. A `.mpeg` that
/// is really an MPEG *program stream* with a picture in it is still no picture
/// here -- nothing demuxes one, and it is symphonia's refusal that says so
/// rather than the mp4 crate's. (MPEG's other two layers, `.mp1`/`.mp2`, are
/// deliberately absent: this build compiles the mp3 bundle without its `mp1`/
/// `mp2` features, so a layer II stream is a codec nothing here decodes, and
/// admitting the spelling would be a promise the reader cannot keep.)
pub fn is_audio(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "mp3" | "mpeg" | "mpg" | "wav" | "flac" | "ogg" | "oga" | "opus" | "mka" | "m4a" | "aac"
        )
    })
}

/// Whether a path names a still image -- a source with a picture and no sound
/// and no length of its own, which belongs on a video lane and nowhere else.
/// Extension only, lowercased, for [`is_audio`]'s reason: a front-end has to
/// know which lane a dropped file may land on before anything is opened.
///
/// Exactly the formats `image` is built with here (see `Cargo.toml`): PNG,
/// JPEG and WebP. A `.gif` or a `.tif` is refused by the same door that refuses
/// a `.txt`, which is the honest answer until a decoder for it is compiled in.
pub fn is_image(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp"
        )
    })
}

/// The largest picture this engine composes onto, either way: 8K, which is where
/// a per-frame buffer stops being a sane thing to allocate.
pub const MAX_RESOLUTION: u32 = 7680;

/// Whether a project resolution is a picture at all: not zero either way, and
/// not past [`MAX_RESOLUTION`]. Both doors ask -- the keystroke
/// ([`PlaybackSession::set_resolution`](playback::PlaybackSession::set_resolution))
/// and the file ([`edith::load`]) -- so a hand-written `resolution` line cannot
/// reach an allocation a keypress could not, which is how `4294967295` used to
/// panic the open with a capacity overflow.
pub fn is_resolution(width: u32, height: u32) -> bool {
    (1..=MAX_RESOLUTION).contains(&width) && (1..=MAX_RESOLUTION).contains(&height)
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
            "song.mp3",
            "a.WAV",
            "b.flac",
            "c.ogg",
            "d.oga",
            "e.m4a",
            "f.aac",
            "g.opus",
            "h.OPUS",
            "i.mka",
            "j.MPEG",
            "k.mpg",
        ] {
            assert!(super::is_audio(Path::new(name)), "{name}");
        }
        // Video, projects, the elementary streams no reader here opens, and a
        // file with no extension at all: all of them go elsewhere.
        for name in [
            "clip.mp4",
            "take.MP4",
            "cut.edith",
            "y.ac3",
            "z.dts",
            "notes",
            "mp3",
        ] {
            assert!(!super::is_audio(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn is_image_admits_the_formats_the_decoder_is_built_with() {
        for name in ["shot.png", "a.JPG", "b.jpeg", "c.webp"] {
            assert!(super::is_image(Path::new(name)), "{name}");
        }
        // Media, projects, the still formats no decoder is compiled in for,
        // and a file with no extension: all of them go elsewhere. The two
        // predicates never both hold -- which lane a file may land on is one
        // answer, not two.
        for name in ["clip.mp4", "song.mp3", "cut.edith", "d.gif", "e.tif", "png"] {
            assert!(!super::is_image(Path::new(name)), "{name}");
            assert!(
                !(super::is_image(Path::new(name)) && super::is_audio(Path::new(name))),
                "{name}"
            );
        }
    }
}
