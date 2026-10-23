//! The container is what the bytes say, not what the extension says; and an
//! HEVC profile the VA-API path cannot decode is refused by name, not panicked on.
//!
//! ```text
//! cargo test -p engine --release --test container_sniff -- --nocapture
//! ```

use std::path::PathBuf;

use engine::PlaybackSession;
use engine::demux::{Demuxer, is_matroska};
use engine::scratch::Scratch;

fn asset(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(name)
}

#[test]
fn a_matroska_file_named_mp4_is_read_as_matroska() {
    let dir = Scratch::dir("ve_sniff_mkv_as_mp4");
    let renamed = dir.join("renamed.mp4");
    std::fs::copy(asset("test_hevc.mkv"), &renamed).expect("copy the fixture");
    assert!(is_matroska(&renamed), "the EBML magic decides, not the extension");
    let (meta, _) = Demuxer::open(&renamed).expect("opens by content");
    assert!(meta.width > 0 && meta.height > 0);
}

#[test]
fn an_mp4_named_mkv_is_read_as_mp4() {
    let dir = Scratch::dir("ve_sniff_mp4_as_mkv");
    let renamed = dir.join("renamed.mkv");
    std::fs::copy(asset("test_hevc.mp4"), &renamed).expect("copy the fixture");
    assert!(!is_matroska(&renamed), "the ftyp box decides, not the extension");
    Demuxer::open(&renamed).expect("opens by content");
}

#[test]
fn a_4_2_2_hevc_opens_in_the_demuxer_and_in_playback() {
    // 4:2:2 is carried at 8 and 10 bits: the hvcC record names it, and it opens
    // at open and at playback alike, with no refusal left to raise.
    let (meta, _) = Demuxer::open(&asset("test_hevcsw_422.mp4")).expect("a 4:2:2 HEVC file opens");
    assert_eq!(meta.codec, engine::demux::Codec::Hevc);
    PlaybackSession::open(asset("test_hevcsw_422.mp4")).expect("and plays");
}

#[test]
fn a_main_profile_hevc_reports_profile_one() {
    let (_, demuxer) = Demuxer::open(&asset("test_hevc.mp4")).expect("opens");
    assert_eq!(demuxer.hevc_profile(), Some(1));
}
