//! Subtitles: a list of cues with a time on them, out of a file beside the
//! media (`.srt`, `.vtt`, `.ass`) or out of a track inside it.
//!
//! One model for both, because a timeline cannot care which it was: a
//! [`SubtitleTrack`] is where it came from -- a path, and the Matroska track
//! number when it is one of several inside that path -- plus the [`Cue`]s. That
//! pair is also all a `.edith` writes ([`crate::edith`]): the cues are read
//! back out of the file on the way in, exactly as a clip's *pictures* are, so a
//! project file stays an edit list and not a copy of the subtitles.
//!
//! Parsing is `oxideav-subtitle` (SRT, WebVTT) and `oxideav-ass` (ASS/SSA) --
//! the markup, the override tags and the encoding quirks of a dozen dialects
//! are not this project's to re-derive. What comes back here is plain text: the
//! renderer is a later thing, and a cue nobody can draw yet is still a cue that
//! has to be listed, timed and saved.
//!
//! A cue is not always words. `S_HDMV/PGS` off a BluRay remux is *pictures* --
//! run-length bitmaps the disc composed against its own frame -- and one of
//! those is a [`Cue`] with a [`CueImage`] on it and no text. The decoding is
//! `oxideav-sub-image`, for the reason above. Everything downstream carries the
//! picture the way it carries the string: [`crate::export::timeline_cues`] maps
//! it onto the timeline, a front-end draws it over the film.
//!
//! What is still *refused* is refused by name rather than by omission
//! ([`SubtitleTrack::refused`]): VobSub off a DVD is a picture format nothing
//! here reads, and a file carrying one opens with a row that says so instead of
//! opening with an empty list.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use oxideav_subtitle::ir::plain_text;

/// One line of subtitle: on screen from `start_us` to `end_us`, microseconds
/// from the start of the media, which is the unit every parser here speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_us: i64,
    pub end_us: i64,
    /// Plain text, `\n` between the lines of a multi-line cue. The markup is
    /// resolved and dropped: `<i>` from an SRT, `{\an8}` and `\N` from an ASS.
    ///
    /// ponytail: bold/italic/colour and ASS positioning are parsed by the crate
    /// and thrown away here, because nothing draws a cue yet. The upgrade path
    /// is to keep `oxideav_core::Segment` beside this string -- the parsers
    /// already hand it over -- and it belongs to whoever writes the renderer.
    pub text: String,
    /// The picture this cue *is*, for a track that is pictures rather than
    /// lines. `None` for every text cue -- an `.srt`, a `.vtt`, an `.ass`, an
    /// `S_TEXT/*` track -- and `Some` for every cue of a PGS one, which then
    /// has no [`text`](Self::text) at all.
    ///
    /// Shared rather than owned: [`crate::export::timeline_cues`] copies a whole
    /// track's cues, per repaint, and a display set is tens of kilobytes.
    pub image: Option<Arc<CueImage>>,
}

/// A cue that is a picture: one PGS display set, kept the way the Matroska
/// block held it -- run-length and palettised, tens of kilobytes -- and turned
/// into pixels only when something is about to draw it ([`Self::rgba`]).
///
/// Kept encoded because decoded is enormous: this film's canvas is 8 MB of
/// RGBA and its four PGS tracks carry about eleven thousand display sets
/// between them, so a track that decoded itself at import would cost gigabytes
/// where it now costs the thirty megabytes the file already spends on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueImage {
    /// The canvas the disc composed against: 1920x1080 for a BluRay, whatever
    /// the film's own frame was cropped to. The whole canvas is the picture --
    /// transparent everywhere the cue does not paint -- so a caller lays it
    /// over the film rather than placing it.
    pub width: u32,
    pub height: u32,
    /// The display set: `type`, big-endian size, body, segment after segment,
    /// which is the shape a Matroska block holds PGS in.
    set: Vec<u8>,
}

impl CueImage {
    /// The picture: straight (not premultiplied) RGBA, `width * height * 4`
    /// bytes, transparent wherever the cue paints nothing. `None` for a display
    /// set the decoder will not have, which is a corrupt block.
    ///
    /// Decoded on the spot: the run-length walk and one canvas-sized buffer,
    /// which is a thing to do when the cue on screen changes and not sixty
    /// times a second. A caller that draws these keeps the answer for as long
    /// as the cue is up.
    pub fn rgba(&self) -> Option<Vec<u8>> {
        use oxideav_core::{CodecId, CodecParameters, Frame, Packet, TimeBase};
        let params = CodecParameters::subtitle(CodecId::new(oxideav_sub_image::PGS_CODEC_ID));
        let mut decoder = oxideav_sub_image::pgs::make_decoder(&params).ok()?;
        // The decoder reads a `.sup`'s framing and a Matroska block is the same
        // segments with the `PG` magic and the timestamps taken off, so they go
        // back on. The times themselves are the block's and are already on the
        // `Cue`, so they are written as nothing.
        let mut sup = Vec::with_capacity(self.set.len() + 64);
        for (kind, body) in segments(&self.set)? {
            sup.extend_from_slice(b"PG");
            sup.extend_from_slice(&[0; 8]);
            sup.push(kind);
            sup.extend_from_slice(&(body.len() as u16).to_be_bytes());
            sup.extend_from_slice(body);
        }
        decoder
            .send_packet(&Packet::new(0, TimeBase::new(1, 90_000), sup))
            .ok()?;
        match decoder.receive_frame().ok()? {
            Frame::Video(frame) => Some(frame.planes.into_iter().next()?.data),
            _ => None,
        }
    }
}

/// The segments of a display set as a Matroska block holds them: a type byte, a
/// big-endian size, that many bytes of body, and again until the block is out.
/// `None` for bytes that do not walk -- a truncated or mislabelled block.
fn segments(set: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < set.len() {
        let size = u16::from_be_bytes([*set.get(at + 1)?, *set.get(at + 2)?]) as usize;
        out.push((set[at], set.get(at + 3..at + 3 + size)?));
        at += 3 + size;
    }
    Some(out)
}

/// A subtitle track: where it came from and what it says.
#[derive(Clone, Debug)]
pub struct SubtitleTrack {
    /// The file it was read out of: a standalone subtitle file, or the media
    /// file whose track number is below.
    pub path: PathBuf,
    /// Which track of `path`, for one embedded in a Matroska file. `None` when
    /// `path` *is* the subtitle file.
    pub track: Option<u64>,
    /// What a list shows: a language, a muxer's name for the track, or the
    /// file's own name for a standalone one.
    pub label: String,
    /// In start order. Empty when [`refused`](Self::refused) says why.
    pub cues: Vec<Cue>,
    /// Why this track has no cues, when it has none: a codec that is pictures
    /// rather than text, a file that has gone missing since the project was
    /// saved. `None` for one that parsed -- which a track with genuinely no
    /// cues in it also is.
    pub refused: Option<String>,
}

impl SubtitleTrack {
    /// A track that is there and cannot be read, named and kept: a project
    /// re-saved after one of these still lists it, which is the difference
    /// between "your subtitles are missing" and losing them for good.
    fn refused(path: &Path, track: Option<u64>, label: String, why: String) -> Self {
        Self {
            path: path.to_path_buf(),
            track,
            label,
            cues: Vec::new(),
            refused: Some(why),
        }
    }

    /// Whether this track's cues are pictures rather than lines -- a PGS track
    /// off a remux. What it decides is where words are the only answer: an
    /// export writes a *text* track and cannot carry these
    /// ([`crate::export::planned_subtitles`] says so out loud).
    ///
    /// Read off the first cue rather than all of them, because a track is one
    /// codec: PGS blocks are pictures to the last one.
    pub fn is_bitmap(&self) -> bool {
        self.cues.first().is_some_and(|c| c.image.is_some())
    }
}

/// The one door a saved subtitle line comes back in through: a standalone file
/// (`track` is `None`) or track `n` of a Matroska file.
///
/// Never an error, whatever is wrong: a deleted `.srt`, a media file that is
/// not there any more, a track number the file no longer has, a codec of
/// pictures. All of them come back [`refused`](SubtitleTrack::refused) by name,
/// because a project that opened yesterday has to open today -- a subtitle is
/// not what a whole timeline should refuse over.
pub fn open(path: &Path, track: Option<u64>) -> SubtitleTrack {
    let Some(number) = track else {
        return external(path);
    };
    let label = format!("track {number}");
    let mut tracks = match of_matroska(path) {
        Ok(tracks) => tracks,
        Err(why) => return SubtitleTrack::refused(path, track, label, why.to_string()),
    };
    match tracks.iter().position(|t| t.track == Some(number)) {
        Some(i) => tracks.swap_remove(i),
        None => SubtitleTrack::refused(
            path,
            track,
            label,
            format!("no subtitle track {number} in {} any more", path.display()),
        ),
    }
}

/// A standalone subtitle file, parsed by its extension.
fn external(path: &Path) -> SubtitleTrack {
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match parse_file(path) {
        Ok(cues) => SubtitleTrack {
            path: path.to_path_buf(),
            track: None,
            label,
            cues,
            refused: None,
        },
        Err(why) => SubtitleTrack::refused(path, None, label, why.to_string()),
    }
}

/// Every subtitle track of a Matroska file, in file order -- the text ones with
/// their cues, the bitmap ones refused by name (see the module docs).
pub fn of_matroska(path: &Path) -> crate::Result<Vec<SubtitleTrack>> {
    Ok(crate::demux::matroska_subtitles(path)?
        .into_iter()
        .map(|mut t| {
            let label = match (t.name.is_empty(), t.language.as_str()) {
                (true, lang) => lang.to_owned(),
                (false, "und") => t.name.clone(),
                (false, lang) => format!("{lang} — {}", t.name),
            };
            match cues_of(&mut t) {
                Ok(cues) => SubtitleTrack {
                    path: path.to_path_buf(),
                    track: Some(t.number),
                    label,
                    cues,
                    refused: None,
                },
                Err(why) => SubtitleTrack::refused(path, Some(t.number), label, why),
            }
        })
        .collect())
}

/// The cues of one Matroska subtitle track, or why it has none.
///
/// A Matroska text track carries the *timing* on the block and the *text* in
/// it, so neither codec is parsed as its own file would be: `S_TEXT/UTF8` is
/// one SRT cue's body per block, `S_TEXT/ASS` is one `Dialogue` line per block
/// with the `Start` and `End` fields cut out and the `[Script Info]` header
/// filed away in `CodecPrivate`. Both are put back into the shape their parser
/// reads -- rather than the markup being re-derived here -- which is also what
/// makes an embedded track and the standalone file it was muxed from come back
/// as the same cues.
///
/// A PGS track is neither: its blocks are pictures, and they are moved out of
/// `track` rather than copied ([`pgs_cues`]) -- thirty megabytes a track is not
/// a thing to hold twice.
fn cues_of(track: &mut crate::demux::MkvSubtitle) -> Result<Vec<Cue>, String> {
    // A track compressed or encrypted with something the demuxer cannot undo is
    // refused in those words: the row stays, and it says why.
    if let Some(why) = &track.unsupported {
        return Err(why.clone());
    }
    let cues = match track.codec.as_str() {
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => track
            .cues
            .iter()
            .map(|c| Cue {
                start_us: c.start_us,
                end_us: end_of(track, c),
                text: srt_body(&String::from_utf8_lossy(&c.payload)),
                image: None,
            })
            .collect(),
        "S_TEXT/ASS" | "S_TEXT/SSA" => ass_cues(track)?,
        crate::demux::PGS => pgs_cues(track),
        codec => {
            return Err(format!(
                "{codec} subtitles are pictures, not text — this track is listed, not read"
            ));
        }
    };
    Ok(cues)
}

/// The cues of an `S_HDMV/PGS` track: one display set per block, and a display
/// set is a picture the disc composes onto its own canvas.
///
/// Only the blocks that compose *something* are cues. The other half of them
/// are the disc's "take it off again" -- a composition with no object on it --
/// and they are what says when the picture before them goes away, which is why
/// they are walked and not skipped. A muxer writes `BlockDuration` zero for all
/// of them, so [`end_of`] is what pairs the two.
fn pgs_cues(track: &mut crate::demux::MkvSubtitle) -> Vec<Cue> {
    // Before the blocks are emptied: `end_of` reads the ones after this one.
    let ends: Vec<i64> = track.cues.iter().map(|c| end_of(track, c)).collect();
    track
        .cues
        .iter_mut()
        .zip(ends)
        .filter_map(|(block, end_us)| {
            let (width, height) = pgs_canvas(&block.payload)?;
            Some(Cue {
                start_us: block.start_us,
                end_us,
                text: String::new(),
                image: Some(Arc::new(CueImage {
                    width,
                    height,
                    set: std::mem::take(&mut block.payload),
                })),
            })
        })
        .collect()
}

/// The canvas of a display set that paints something -- its `PCS` says how big
/// the disc's picture is, and how many objects are composed onto it.
///
/// `None` when nothing is composed, which is the disc clearing the screen, and
/// `None` for bytes that are not a display set at all.
fn pgs_canvas(set: &[u8]) -> Option<(u32, u32)> {
    /// `PresentationCompositionSegment`, which every display set opens with.
    const PCS: u8 = 0x16;
    let (_, pcs) = segments(set)?.into_iter().find(|&(kind, _)| kind == PCS)?;
    // Width and height, then the frame rate, the composition number and state,
    // the palette-update flag and the palette id -- and then how many objects
    // this composition puts on the canvas.
    let width = u16::from_be_bytes([*pcs.first()?, *pcs.get(1)?]);
    let height = u16::from_be_bytes([*pcs.get(2)?, *pcs.get(3)?]);
    (*pcs.get(10)? > 0).then_some((u32::from(width), u32::from(height)))
}

/// When a cue goes away: its `BlockDuration`, or -- for the muxer that writes
/// none, and for the PGS block that writes a zero, which says the same thing --
/// when the next one arrives, and [`NO_DURATION_US`] for the last.
fn end_of(track: &crate::demux::MkvSubtitle, cue: &crate::demux::MkvCue) -> i64 {
    /// How long a cue with nothing to say about it stays up: two seconds, a
    /// read of one line.
    const NO_DURATION_US: i64 = 2_000_000;
    match cue.duration_us.filter(|&d| d > 0) {
        Some(d) => cue.start_us + d,
        None => track
            .cues
            .iter()
            .map(|c| c.start_us)
            .find(|&s| s > cue.start_us)
            .unwrap_or(cue.start_us + NO_DURATION_US),
    }
}

/// The text of one `S_TEXT/UTF8` block with its markup resolved: through the
/// SRT parser, around a timing line this then throws away, because SRT markup
/// is exactly what such a block holds and a second implementation of it here
/// would be a second set of bugs.
fn srt_body(text: &str) -> String {
    let doc = format!("1\n00:00:00,000 --> 00:00:01,000\n{}\n", text.trim_end());
    match oxideav_subtitle::srt::parse(doc.as_bytes()) {
        Ok(track) => track
            .cues
            .first()
            .map(|c| plain_text(&c.segments))
            .unwrap_or_default(),
        // The parser refuses nothing an SRT body can hold; if it ever does,
        // the bytes themselves are a better answer than an empty cue.
        Err(_) => text.trim_end().to_owned(),
    }
}

/// The cues of an `S_TEXT/ASS` track: the script header out of `CodecPrivate`
/// with every block put back as a `Dialogue` line under it, parsed once as the
/// `.ass` file it came from.
fn ass_cues(track: &crate::demux::MkvSubtitle) -> Result<Vec<Cue>, String> {
    let mut doc = String::from_utf8_lossy(&track.private).into_owned();
    if !doc.ends_with('\n') {
        doc.push('\n');
    }
    for cue in &track.cues {
        let fields = String::from_utf8_lossy(&cue.payload);
        // `ReadOrder,Layer,<the Format fields from Style on>`: the two the
        // block adds go, and the timing the block *is* takes their place. The
        // field order is the one the header's `Format:` line declares, which
        // is the order a demuxer strips them out of.
        let mut rest = fields.trim_end().splitn(3, ',');
        let (_read_order, layer, rest) = match (rest.next(), rest.next(), rest.next()) {
            (Some(r), Some(l), Some(t)) => (r, l, t),
            _ => continue,
        };
        doc.push_str(&format!(
            "Dialogue: {layer},{},{},{rest}\n",
            timestamp(cue.start_us),
            timestamp(end_of(track, cue))
        ));
    }
    let parsed = oxideav_ass::parse(doc.as_bytes()).map_err(|e| e.to_string())?;
    // The timings above are centiseconds, which is all an ASS line can say;
    // the blocks are microseconds. So the text comes from the parse and the
    // timing from the block -- unless a line was dropped as unparsable, when
    // the pairing is no longer sound and the parse answers for both.
    let exact = parsed.cues.len() == track.cues.len();
    Ok(parsed
        .cues
        .iter()
        .enumerate()
        .map(|(i, c)| Cue {
            start_us: if exact {
                track.cues[i].start_us
            } else {
                c.start_us
            },
            end_us: if exact {
                end_of(track, &track.cues[i])
            } else {
                c.end_us
            },
            text: plain_text(&c.segments),
            image: None,
        })
        .collect())
}

/// Microseconds as ASS writes a time: `H:MM:SS.cc`.
fn timestamp(us: i64) -> String {
    let cs = (us.max(0) + 5_000) / 10_000;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6_000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

/// A standalone subtitle file, by extension: the formats this engine reads at
/// all. Anything else is refused by name, as an unknown media file is.
fn parse_file(path: &Path) -> crate::Result<Vec<Cue>> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // The extension before the read: a `.txt` is refused for what it is, not
    // for whether it happens to be there.
    type Parse = fn(&[u8]) -> oxideav_core::Result<oxideav_subtitle::ir::SubtitleTrack>;
    let parse: Parse = match ext.as_str() {
        "srt" => oxideav_subtitle::srt::parse,
        "vtt" | "webvtt" => oxideav_subtitle::webvtt::parse,
        "ass" | "ssa" => oxideav_ass::parse,
        other => {
            return Err(format!(
                "{other:?} is not a subtitle format this reads — .srt, .vtt, .ass and .ssa are"
            )
            .into());
        }
    };
    let track = parse(&std::fs::read(path)?);
    let track = track.map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(track
        .cues
        .iter()
        .map(|c| Cue {
            start_us: c.start_us,
            end_us: c.end_us,
            text: plain_text(&c.segments),
            image: None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ass_timestamps_are_written_the_way_ass_reads_them() {
        assert_eq!(timestamp(0), "0:00:00.00");
        assert_eq!(timestamp(500_000), "0:00:00.50");
        assert_eq!(timestamp(3_250_000), "0:00:03.25");
        // An hour, a minute and a second, and a negative time is not one.
        assert_eq!(timestamp(3_661_000_000), "1:01:01.00");
        assert_eq!(timestamp(-1), "0:00:00.00");
    }

    #[test]
    fn srt_markup_is_resolved_not_carried() {
        assert_eq!(srt_body("<i>tilted</i> and plain"), "tilted and plain");
        assert_eq!(srt_body("two\nlines\n"), "two\nlines");
    }

    #[test]
    fn an_unknown_extension_is_refused_by_name_and_a_missing_file_is_kept() {
        let err = parse_file(Path::new("/nonexistent/notes.txt"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("txt"), "{err}");
        // ...and a `.srt` that is not there comes back as a track that says so
        // rather than as no track at all, so a re-save does not lose it.
        let gone = external(Path::new("/nonexistent/subs.srt"));
        assert!(gone.refused.is_some());
        assert_eq!(gone.label, "subs.srt");
    }
}
