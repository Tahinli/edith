//! What this editor can really decode and encode, in one place and in two
//! halves.
//!
//! The *software* half is what this binary was built with: a table, because
//! "is `rav1e` linked into this build" is a question the build already answered
//! and probing for it would only be a slower way of reading the manifest.
//!
//! The *hardware* half is a different answer on every machine and is therefore
//! never a table: [`crate::hw::caps`] asks the plugin, the plugin asks the
//! driver, and what comes back is the intersection of the GPU's entrypoints
//! with the codecs the plugin implements. No plugin, an older plugin or no
//! driver all read the same way -- "software only" -- because that is what they
//! mean for the file a user is about to write.

use crate::hw::{CAP_AV1, CAP_H264, CAP_HEVC, CAP_VP9, VhCaps};

/// The codecs a hardware line can name, in the order it lists them.
const HW_CODECS: [(u32, &str); 4] = [
    (CAP_H264, "H.264"),
    (CAP_HEVC, "HEVC"),
    (CAP_VP9, "VP9"),
    (CAP_AV1, "AV1"),
];

/// The software half: the codec, the crate that carries it here, and which
/// halves of the job that crate does. Named crates rather than a bare "SW",
/// because the answer to "why is my HEVC file so large" is `oxideav-h265
/// intra`, and the row that says so is the row that answers it.
const SW_CODECS: [(&str, &str, bool, bool); 10] = [
    ("H.264", "rusty_h264", true, true),
    ("HEVC", "oxideav-h265 intra", false, true),
    ("AV1", "rav1e", false, true),
    ("AAC", "rusty_aac", true, true),
    ("MP3", "symphonia / rusty_mp3", true, true),
    ("FLAC", "symphonia / flacenc", true, true),
    ("PCM", "symphonia / hound", true, true),
    ("AC-3", "oxideav-ac3", true, false),
    ("Vorbis", "symphonia / rusty_vorbis", true, true),
    ("ALAC", "symphonia", true, false),
];

/// The hardware line as a front-end shows it: one measurement per process,
/// costing the plugin's VA-API init (~90 ms) the first time. Ask it off a
/// render thread.
pub fn hardware() -> String {
    hw_line(crate::hw::caps())
}

/// The software line. Pure and constant -- a caller may ask it per repaint.
pub fn software() -> String {
    SW_CODECS
        .iter()
        .map(|&(name, krate, decodes, encodes)| {
            format!("{name} {} ({krate})", seats(decodes, encodes))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What the plugin answered, in words. Separate from [`hardware`] so the
/// wording can be tested against a machine this test suite does not have.
fn hw_line(caps: Option<VhCaps>) -> String {
    let Some(caps) = caps else {
        return "none — no plugin, no driver, or a plugin too old to say".to_string();
    };
    let listed: Vec<String> = HW_CODECS
        .iter()
        .filter(|&&(bit, _)| caps.decode & bit != 0 || caps.encode & bit != 0)
        .map(|&(bit, name)| {
            let seats = seats(caps.decode & bit != 0, caps.encode & bit != 0);
            match caps.decode_10bit & bit != 0 {
                true => format!("{name} {seats} (10-bit)"),
                false => format!("{name} {seats}"),
            }
        })
        .collect();
    match listed.is_empty() {
        // A driver answered and takes none of the codecs this build carries:
        // not the same thing as no driver, and worth saying so.
        true => "none — the driver takes none of these codecs".to_string(),
        false => listed.join(" · "),
    }
}

/// Which halves of the job one seat does. Never both empty: a codec with
/// neither is not listed at all.
fn seats(decodes: bool, encodes: bool) -> &'static str {
    match (decodes, encodes) {
        (true, true) => "dec+enc",
        (true, false) => "dec",
        _ => "enc",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hardware_line_says_what_the_plugin_answered_and_nothing_more() {
        // No answer at all -- no plugin, no driver, or one older than the
        // symbol -- is one line, and never an empty listing that would read as
        // "still loading".
        assert!(hw_line(None).starts_with("none —"));
        // A driver that answers with nothing is its own sentence.
        assert!(hw_line(Some(VhCaps::default())).starts_with("none —"));
        // This project's own GPU as `vainfo` lists it: H.264 both ways, HEVC
        // and VP9 decode with their 10-bit profiles, AV1 both ways. HEVC has a
        // VA encode entrypoint here and is *not* an encode seat: the plugin has
        // no HEVC encoder, so the line must not offer one.
        let radeonsi = VhCaps {
            decode: CAP_H264 | CAP_HEVC | CAP_VP9 | CAP_AV1,
            encode: CAP_H264 | CAP_AV1,
            decode_10bit: CAP_HEVC | CAP_VP9 | CAP_AV1,
        };
        assert_eq!(
            hw_line(Some(radeonsi)),
            "H.264 dec+enc · HEVC dec (10-bit) · VP9 dec (10-bit) · AV1 dec+enc (10-bit)"
        );
        // A codec the driver has neither seat for is left out rather than
        // listed as a refusal: this line is what the machine *can* do.
        let h264_only = VhCaps {
            decode: CAP_H264,
            ..Default::default()
        };
        assert_eq!(hw_line(Some(h264_only)), "H.264 dec");
    }

    #[test]
    fn the_software_line_names_every_encoder_this_build_carries() {
        let line = software();
        for name in ["rusty_h264", "oxideav-h265 intra", "rav1e", "rusty_aac"] {
            assert!(line.contains(name), "{name} missing from {line}");
        }
        // Decode-only crates must not read as encoders, and the seat words are
        // the only thing saying which is which.
        assert!(line.contains("AC-3 dec (oxideav-ac3)"));
        assert!(line.contains("H.264 dec+enc (rusty_h264)"));
        assert!(line.contains("HEVC enc (oxideav-h265 intra)"));
    }
}
