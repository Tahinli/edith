//! The engine's integration tests, one binary for the 117 files that write no process-wide state.
//!
//! Each module below was its own `tests/*.rs` binary, and each binary is compiled and linked on
//! its own. The files left at the top of `tests/` are the ones that must not share a process:
//! they write process-wide env (`VE_SW`, `VE_HW_*`, `PIPEWIRE_REMOTE`, ...) or install a panic
//! hook, or re-run their own binary pinned to CPUs (`export_default_bytes`).
//!
//! The shared helpers under `tests/support/` are included once here and imported by the modules
//! that use them, so one `SEAT` lock serialises every seat holder in this binary.

#[path = "../support/hwchild.rs"]
#[allow(dead_code)]
mod hwchild;
#[path = "../support/reallib.rs"]
#[allow(dead_code)]
mod reallib;
#[path = "../support/seat.rs"]
#[allow(dead_code)]
mod seat;

mod aac_profile;
mod anim_keys;
mod ass_karaoke;
mod ass_subtitles;
mod audio_aac;
mod audio_aac_seek;
mod audio_aac_v2;
mod audio_ac3_film;
mod audio_bit_depth;
mod audio_copy;
mod audio_decode;
mod audio_export;
mod audio_ext;
mod audio_files;
mod audio_gaps;
mod audio_multi;
mod audio_only_mp4_reopen;
mod audio_seek;
mod audio_segments;
mod audio_streams;
mod audio_truehd;
mod audio_vorbis;
mod av1;
mod av1_export;
mod av1_software_export;
mod av_sync;
mod backends;
mod bitmap_subs;
mod bitrate;
mod capability_matrix;
mod clip_gain;
mod closed_captions;
mod color_assumed;
mod color_lut;
mod compose_at;
mod container_probe;
mod container_sniff;
mod cover_art;
mod creator_tools;
mod decode_baseline;
mod dependency_guard;
mod display_unit;
mod duck_audio;
mod effects;
mod eq_audio;
mod export_color;
mod export_deep_ten_bit;
mod export_m4a_range;
mod export_presets;
mod export_software;
mod export_ten_bit;
mod flac_export;
mod fps_round_trip;
mod generators;
mod grain_seek;
mod h264_mkv;
mod hevc;
mod hevc10_export;
mod hevc_export;
mod hevc_hvc1;
mod hevc_mkv;
mod hw_fallback;
mod hw_midgop_seek;
mod invalidation;
mod media_facts;
mod media_streams;
mod mixed_fps;
mod mkv_encodings;
mod mkv_unknown_cluster;
mod multicam;
mod offline;
mod ogg_opus_import;
mod opacity;
mod opus_file_export;
mod pan;
mod pgs_bitmaps;
mod pgs_sup_ffmpeg;
mod placement_walls;
mod playback;
mod pro_trims;
mod probe_duration;
mod probe_refusal;
mod project_fuzz;
mod prores_export;
mod r14_probe_undo_streams;
mod real_library_audio;
mod real_library_containers;
mod real_library_flac;
mod real_library_video;
mod real_library_zlib;
mod relative_sources;
mod relink_reach;
mod reverse;
mod rotation_parity;
mod saveload;
mod sequences;
mod silence;
mod silent_audio_import;
mod silent_first_source;
mod speed_frames;
mod speed_ramp_audio;
mod stills;
mod stream_layout;
mod subtitles;
mod text_subs_ffmpeg;
mod transform_keys;
mod transitions;
mod trim_walls;
mod undo_stress;
mod untrusted_header;
mod video_copy;
mod video_profile;
mod voice_audio;
mod voice_early_stop;
mod voiceover_record;
mod vp8;
mod vp9;
