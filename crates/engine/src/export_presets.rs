//! The export preset table, one list for the MCP `export_start { preset }` and the GUI's preset
//! picker: [`presets`], [`by_id`], [`apply`]. Each [`Preset`] has a stable id, a label, a
//! one-line description and a [`Target`] (category, video or audio-only, size) to group by. A preset names a [`Format`], a bitrate, a sound rate, a canvas size and (for sound
//! alone) a loudness target; [`apply`] lays those over a caller's [`ExportSettings`] and leaves
//! everything else (chapters, subtitles, seat, range, burn, bit depth unless the preset names one)
//! as the caller had it.
//!
//! The video figures are YouTube's recommended SDR upload bitrates. The engine clamps an explicit
//! bitrate to 50 Mb/s, so the 4K60 row sits at that ceiling rather than YouTube's 53-68.
//! There is no standalone Opus file in this engine, so the second podcast row is Ogg Vorbis.

use crate::export::{ExportSettings, Format, HdrOutput, HdrTransfer, LoudnessTarget, ProResProfile};

/// Where a preset is meant to go: the group a picker lists it under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// Long-form web video (YouTube).
    Web,
    /// Short vertical video (Shorts, Reels, TikTok).
    Social,
    /// Sound-only delivery (podcasts).
    Audio,
    /// Masters kept for later: high quality, large files.
    Archive,
}

/// Whether the preset writes a picture or only sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Output {
    Video,
    AudioOnly,
}

/// What a preset aims at: for a UI to group and badge presets by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub category: Category,
    pub output: Output,
    /// The canvas the file is drawn at, `(width, height)`; `None` keeps the project's (and is
    /// always `None` for [`Output::AudioOnly`]).
    pub size: Option<(u32, u32)>,
}

/// One row of the table. The fields are the export settings the preset sets; [`apply`] lays them
/// over a caller's [`ExportSettings`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// Stable snake_case name: what `export_start { preset }` takes and a saved choice stores.
    pub id: &'static str,
    /// Short name for a list row.
    pub label: &'static str,
    /// One line saying what the file is and what it is for.
    pub description: &'static str,
    /// Group, output kind and target size.
    pub target: Target,
    pub format: Format,
    /// Bits per second of the picture; `None` is the automatic figure.
    pub bitrate: Option<u64>,
    /// The sound's rate in kbps.
    pub audio_kbps: u32,
    /// `Some` sets the AV1 sample depth; `None` leaves the caller's.
    pub video_bit_depth: Option<u8>,
    /// Integrated loudness and true-peak ceiling the sound is levelled to, if any.
    pub loudness: Option<LoudnessTarget>,
    /// `Some` sets the ProRes profile of a ProRes file; `None` leaves the caller's.
    pub prores_profile: Option<ProResProfile>,
    /// The HDR transfer of an HEVC file; [`HdrTransfer::Sdr`] is none, and [`apply`] clears any
    /// the caller had, since an HDR tag on another preset's file would be refused.
    pub hdr: HdrTransfer,
}

const PODCAST: Option<LoudnessTarget> = Some(LoudnessTarget { lufs: -16.0, true_peak_dbtp: -1.0 });

#[allow(clippy::too_many_arguments)]
const fn video(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    category: Category,
    format: Format,
    mbps: u64,
    size: (u32, u32),
    audio_kbps: u32,
) -> Preset {
    Preset {
        id,
        label,
        description,
        target: Target { category, output: Output::Video, size: Some(size) },
        format,
        bitrate: Some(mbps * 1_000_000),
        audio_kbps,
        video_bit_depth: None,
        loudness: None,
        prores_profile: None,
        hdr: HdrTransfer::Sdr,
    }
}

const fn sound(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    category: Category,
    format: Format,
    audio_kbps: u32,
    loudness: Option<LoudnessTarget>,
) -> Preset {
    Preset {
        id,
        label,
        description,
        target: Target { category, output: Output::AudioOnly, size: None },
        format,
        bitrate: None,
        audio_kbps,
        video_bit_depth: None,
        loudness,
        prores_profile: None,
        hdr: HdrTransfer::Sdr,
    }
}

const WEB: Category = Category::Web;
const HD: (u32, u32) = (1920, 1080);
const UHD: (u32, u32) = (3840, 2160);

static TABLE: [Preset; 15] = [
    video("youtube_1080p", "YouTube 1920×1080", "H.264 1080p30 at 8 Mb/s, AAC 384k: YouTube's recommended SDR upload.", WEB, Format::Mp4, 8, HD, 384),
    video("youtube_1080p60", "YouTube 1920×1080 60fps", "H.264 1080p60 at 12 Mb/s, AAC 384k.", WEB, Format::Mp4, 12, HD, 384),
    video("youtube_1080p_hevc", "YouTube 1920×1080 HEVC", "HEVC 1080p at 6 Mb/s in an mp4, AAC 384k: a smaller file at the same look.", WEB, Format::HevcMp4, 6, HD, 384),
    video("youtube_4k", "YouTube 3840×2160", "H.264 2160p30 at 40 Mb/s, AAC 384k.", WEB, Format::Mp4, 40, UHD, 384),
    video("youtube_4k60", "YouTube 3840×2160 60fps", "H.264 2160p60 at 50 Mb/s (the engine's ceiling; YouTube suggests 53-68), AAC 384k.", WEB, Format::Mp4, 50, UHD, 384),
    video("youtube_4k_hevc", "YouTube 3840×2160 HEVC", "HEVC 2160p at 28 Mb/s in an mp4, AAC 384k.", WEB, Format::HevcMp4, 28, UHD, 384),
    video("shorts_reels", "Shorts / Reels", "Vertical 9:16 1080x1920, H.264 at 8 Mb/s, AAC 192k.", Category::Social, Format::Mp4, 8, (1080, 1920), 192),
    sound("podcast", "Podcast m4a", "AAC 128k in an m4a, levelled to -16 LUFS with a -1 dBTP ceiling.", Category::Audio, Format::M4a, 128, PODCAST),
    sound("podcast_ogg", "Podcast ogg", "Vorbis in an ogg, -16 LUFS, -1 dBTP.", Category::Audio, Format::Ogg, 128, PODCAST),
    sound("podcast_opus", "Podcast opus", "Opus 96k stereo in an Ogg file, levelled to -16 LUFS with a -1 dBTP ceiling.", Category::Audio, Format::Opus, 96, PODCAST),
    Preset {
        id: "archive",
        label: "Archive 10-bit AV1",
        description: "10-bit AV1 in an mkv at 30 Mb/s, the project's size, audio at the top AAC rate.",
        target: Target { category: Category::Archive, output: Output::Video, size: None },
        format: Format::Av1,
        bitrate: Some(30_000_000),
        audio_kbps: 320,
        video_bit_depth: Some(10),
        loudness: None,
        prores_profile: None,
        hdr: HdrTransfer::Sdr,
    },
    Preset {
        id: "archive_prores",
        label: "ProRes 422 HQ master",
        description: "ProRes 422 HQ in a QuickTime .mov at the project's size, 24-bit PCM sound: an editing master.",
        target: Target { category: Category::Archive, output: Output::Video, size: None },
        format: Format::ProRes,
        bitrate: None,
        audio_kbps: 320,
        video_bit_depth: None,
        loudness: None,
        prores_profile: Some(ProResProfile::Hq),
        hdr: HdrTransfer::Sdr,
    },
    Preset {
        id: "hdr10_hevc",
        label: "HDR10 (HEVC 10-bit)",
        description: "10-bit HEVC in an mp4 tagged HDR10 (PQ, BT.2020) at 40 Mb/s, the project's size, AAC 384k.",
        target: Target { category: Category::Archive, output: Output::Video, size: None },
        format: Format::HevcMp4,
        bitrate: Some(40_000_000),
        audio_kbps: 384,
        video_bit_depth: Some(10),
        loudness: None,
        prores_profile: None,
        hdr: HdrTransfer::Pq,
    },
    sound("archive_flac", "Archive FLAC", "Lossless FLAC of the mix.", Category::Archive, Format::Flac, 320, None),
    sound("archive_wav", "Archive WAV", "16-bit PCM WAV of the mix.", Category::Archive, Format::Wav, 320, None),
];

/// Every preset, in the order a picker lists them (grouped by [`Target::category`]).
pub fn presets() -> &'static [Preset] {
    &TABLE
}

/// The preset whose [`Preset::id`] is `id`.
pub fn by_id(id: &str) -> Option<&'static Preset> {
    TABLE.iter().find(|p| p.id == id)
}

impl Preset {
    /// The settings this preset alone describes, over [`ExportSettings::default`].
    pub fn settings(&self) -> ExportSettings {
        apply(self, ExportSettings::default())
    }
}

/// `base` with the preset's format, bitrate, sound rate, size, loudness and HDR transfer laid over it.
/// Everything else stays as `base` had it -- chapters, subtitles, seat, range -- and the bit depth
/// changes only for a preset that names one.
pub fn apply(preset: &Preset, base: ExportSettings) -> ExportSettings {
    ExportSettings {
        format: preset.format,
        bitrate: preset.bitrate,
        audio_kbps: Some(preset.audio_kbps),
        size: preset.target.size,
        video_bit_depth: preset.video_bit_depth.or(base.video_bit_depth),
        loudness: preset.loudness,
        prores_profile: preset.prores_profile.unwrap_or(base.prores_profile),
        hdr: HdrOutput { transfer: preset.hdr, ..HdrOutput::default() },
        ..base
    }
}
