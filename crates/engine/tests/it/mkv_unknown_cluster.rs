//! A WebM whose first Cluster was written with no length -- what a live
//! recorder leaves -- and holds more than the eight-megabyte window the demuxer
//! reads such a cluster through. Every block of it, and the Cluster after it,
//! must come out. Before the fix the demuxer took the next Cluster for a child
//! of the open one and skipped it, so a real 2193-frame VP8 WebM stopped at 270.

use std::path::PathBuf;

use engine::demux::Demuxer;

/// An EBML element with its size written in eight bytes.
fn ebml_el(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.push(0x01);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    out
}

/// An EBML element whose size was never written: the all-ones size.
fn ebml_unknown(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&[0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    out.extend_from_slice(body);
    out
}

#[test]
fn an_unknown_length_cluster_wider_than_its_window_keeps_every_block() {
    const BLOCKS: usize = 150;
    const PAYLOAD: usize = 64 << 10;
    let block = |payload: &[u8]| {
        let mut body = vec![0x81, 0x00, 0x00, 0x80];
        body.extend_from_slice(payload);
        ebml_el(&[0xA3], &body)
    };
    let header = ebml_el(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_el(&[0x42, 0x82], b"webm"));
    let info = ebml_el(
        &[0x15, 0x49, 0xA9, 0x66],
        &ebml_el(&[0x2A, 0xD7, 0xB1], &[0x0F, 0x42, 0x40]),
    );
    let video = [
        ebml_el(&[0xB0], &[0x01, 0x40]),
        ebml_el(&[0xBA], &[0x00, 0xF0]),
    ]
    .concat();
    let track = ebml_el(
        &[0xAE],
        &[
            ebml_el(&[0xD7], &[1]),
            ebml_el(&[0x83], &[1]),
            ebml_el(&[0x86], b"V_VP8"),
            ebml_el(&[0xE0], &video),
        ]
        .concat(),
    );
    let tracks = ebml_el(&[0x16, 0x54, 0xAE, 0x6B], &track);
    let mut open = ebml_el(&[0xE7], &[0]);
    for _ in 0..BLOCKS {
        open.extend(block(&vec![0x5A; PAYLOAD]));
    }
    let closed = [ebml_el(&[0xE7], &[BLOCKS as u8]), block(&[1, 2, 3])].concat();
    let segment = ebml_unknown(
        &[0x18, 0x53, 0x80, 0x67],
        &[
            info,
            tracks,
            ebml_unknown(&[0x1F, 0x43, 0xB6, 0x75], &open),
            ebml_el(&[0x1F, 0x43, 0xB6, 0x75], &closed),
        ]
        .concat(),
    );
    let file = [header, segment].concat();

    let dir = PathBuf::from(std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into()));
    let path = dir.join(format!("mkv-unknown-cluster-{}.webm", std::process::id()));
    std::fs::write(&path, &file).expect("write the fixture");
    let (_, mut demuxer) = Demuxer::open(&path).expect("demuxes");
    let mut units = Vec::new();
    while let Some(au) = demuxer.next_access_unit().expect("reads") {
        units.push(au);
    }
    std::fs::remove_file(&path).ok();

    assert_eq!(
        units.len(),
        BLOCKS + 1,
        "every block of both clusters comes out"
    );
    assert!(units[..BLOCKS].iter().all(|u| u.len() == PAYLOAD));
    assert_eq!(
        units[BLOCKS],
        vec![1, 2, 3],
        "the Cluster after the open one"
    );
}
