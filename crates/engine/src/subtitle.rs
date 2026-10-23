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
//! Parsing is `ec-subs` (SRT, WebVTT) and `ec-ass` (ASS/SSA) --
//! the markup, the override tags and the encoding quirks of a dozen dialects
//! are not this project's to re-derive. What comes back here is plain text: the
//! renderer is a later thing, and a cue nobody can draw yet is still a cue that
//! has to be listed, timed and saved.
//!
//! A cue is not always words. `S_HDMV/PGS` off a BluRay remux is *pictures* --
//! run-length bitmaps the disc composed against its own frame -- and one of
//! those is a [`Cue`] with a [`CueImage`] on it and no text. The decoding is
//! `ec-pgs`, for the reason above. Everything downstream carries the
//! picture the way it carries the string: [`crate::export::timeline_cues`] maps
//! it onto the timeline, a front-end draws it over the film.
//!
//! What is still *refused* is refused by name rather than by omission
//! ([`SubtitleTrack::refused`]): VobSub off a DVD is a picture format nothing
//! here reads, and a file carrying one opens with a row that says so instead of
//! opening with an empty list.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ec_subs::plain_text;

/// One line of subtitle: on screen from `start_us` to `end_us`, microseconds
/// from the start of the media, which is the unit every parser here speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_us: i64,
    pub end_us: i64,
    /// Plain text, `\n` between the lines of a multi-line cue. The markup is
    /// resolved and dropped: `<i>` from an SRT, `{\an8}` and `\N` from an ASS.
    ///
    /// corner-cut: bold/italic/colour and ASS positioning are parsed by the crate
    /// and thrown away here, because nothing draws a cue yet. The upgrade path
    /// is to keep `ec_subs::Segment` runs beside this string -- the parsers
    /// already hand them over -- and it belongs to whoever writes the renderer.
    pub text: String,
    /// The picture this cue *is*, for a track that is pictures rather than
    /// lines. `None` for every text cue -- an `.srt`, a `.vtt`, an `.ass`, an
    /// `S_TEXT/*` track -- and `Some` for every cue of a PGS one, which then
    /// has no [`text`](Self::text) at all.
    ///
    /// Shared rather than owned: [`crate::export::timeline_cues`] copies a whole
    /// track's cues, per repaint, and a display set is tens of kilobytes.
    pub image: Option<Arc<CueImage>>,
    /// When each word of [`text`](Self::text) is spoken, `(start_us, end_us, word)` on the same
    /// clock as the cue, one entry per whitespace-separated word. Empty for a cue from a file
    /// (an SRT has no word times) and for one whose words were retyped: use
    /// [`Self::real_words`], which checks them against the text, never the field.
    pub words: Vec<(i64, i64, String)>,
    /// The karaoke syllables of an ASS cue, on the same clock as the cue. Empty for every other
    /// cue; use [`Self::real_karaoke`], which checks them against the text, never the field.
    pub karaoke: Vec<Syllable>,
}

/// One karaoke beat of an ASS cue (`\k`, `\kf`, `\ko`): the text it sings and how it is lit.
/// Not stored in a project: it is read again from the imported file, with the cue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Syllable {
    /// Where the beat is lit from and to, microseconds on the cue's clock.
    pub start_us: i64,
    pub end_us: i64,
    /// Which non-whitespace character of the cue's text the syllable starts at, and the
    /// characters it covers (`text`, whitespace dropped). Counting only the letters keeps the
    /// place when the line wrapping re-flows the spaces.
    pub from: usize,
    pub text: String,
    pub mode: ec_subs::KaraokeMode,
    /// The colour before the syllable is sung (`\2c`, else the style's secondary).
    pub unsung: [u8; 3],
    /// The colour once sung: an enclosing colour override, else `None` for the text's own.
    pub sung: Option<[u8; 3]>,
}

/// The unsung colour of a style that names no secondary, libass's default.
const DEFAULT_SECONDARY: [u8; 3] = [255, 0, 0];

/// The syllables of `cue`'s body, grouped by their `(start, length)` as the parser documents.
fn syllables(cue: &ec_subs::SubtitleCue, track: &ec_subs::SubtitleTrack) -> Vec<Syllable> {
    struct Walk {
        out: Vec<Syllable>,
        seen: usize,
        base: i64,
        style: [u8; 3],
        /// The `(start_cs, cs)` of the syllable the last piece belonged to.
        last: Option<(u32, u32)>,
    }
    fn walk(segs: &[ec_subs::Segment], colour: Option<[u8; 3]>, beat: Option<(u32, u32, ec_subs::KaraokeMode, [u8; 3])>, w: &mut Walk) {
        use ec_subs::Segment as S;
        for seg in segs {
            match seg {
                S::Text(t) => {
                    let letters: String = t.chars().filter(|c| !c.is_whitespace()).collect();
                    if letters.is_empty() {
                        continue;
                    }
                    let n = letters.chars().count();
                    if let Some((start_cs, cs, mode, unsung)) = beat {
                        match w.out.last_mut() {
                            Some(l) if w.last == Some((start_cs, cs)) => l.text.push_str(&letters),
                            _ => w.out.push(Syllable {
                                start_us: w.base + i64::from(start_cs) * 10_000,
                                end_us: w.base + i64::from(start_cs + cs) * 10_000,
                                from: w.seen,
                                text: letters,
                                mode,
                                unsung,
                                sung: colour,
                            }),
                        }
                        w.last = Some((start_cs, cs));
                    }
                    w.seen += n;
                }
                S::Karaoke { cs, start_cs, mode, secondary, children } => {
                    let unsung = secondary.map_or(w.style, |(r, g, b)| [r, g, b]);
                    walk(children, colour, Some((*start_cs, *cs, *mode, unsung)), w);
                }
                S::Color { rgb, children } => walk(children, Some([rgb.0, rgb.1, rgb.2]), beat, w),
                S::Bold(c) | S::Italic(c) | S::Underline(c) | S::Strike(c) => walk(c, colour, beat, w),
                S::Font { children, .. } | S::Voice { children, .. } | S::Class { children, .. } => walk(children, colour, beat, w),
                S::LineBreak | S::Timestamp { .. } | S::Raw(_) => {}
            }
        }
    }
    let style = cue
        .style_ref
        .as_deref()
        .and_then(|n| track.style(n))
        .and_then(|s| s.secondary_color)
        .map_or(DEFAULT_SECONDARY, |(r, g, b, _)| [r, g, b]);
    let mut w = Walk { out: Vec::new(), seen: 0, base: cue.start_us, style, last: None };
    walk(&cue.segments, None, None, &mut w);
    w.out
}

/// `syllables` moved with the cue they belong to, as [`map_words`] moves words: the cue's `[a, b]`
/// became `[from, to]` on the timeline, and a syllable that starts at or past `b` is dropped.
pub fn map_karaoke(syl: &[Syllable], (a, b): (i64, i64), (from, to): (i64, i64)) -> Vec<Syllable> {
    let scale = if b > a { (to - from) as f64 / (b - a) as f64 } else { 1.0 };
    let onto = |t: i64| from + ((t.clamp(a, b) - a) as f64 * scale).round() as i64;
    syl.iter()
        .filter(|s| b <= a || s.start_us < b)
        .map(|s| Syllable { start_us: onto(s.start_us), end_us: onto(s.end_us), ..s.clone() })
        .collect()
}

impl Cue {
    /// The karaoke syllables, when they still describe the text: each one's letters are the
    /// letters of the text at its place (a retyped cue has lost its karaoke).
    pub fn real_karaoke(&self) -> Option<&[Syllable]> {
        let letters: Vec<char> = self.text.chars().filter(|c| !c.is_whitespace()).collect();
        let ok = !self.karaoke.is_empty()
            && self.karaoke.iter().all(|s| {
                let n = s.text.chars().count();
                letters.get(s.from..s.from + n).is_some_and(|l| l.iter().copied().eq(s.text.chars()))
            });
        ok.then_some(&self.karaoke[..])
    }
}

impl Cue {
    /// The word times, when they still describe the text: the same words in the same order
    /// (case aside, since a caption may capitalise a sentence start after it is timed).
    pub fn real_words(&self) -> Option<&[(i64, i64, String)]> {
        let mut text = self.text.split_whitespace();
        let ok = !self.words.is_empty()
            && self.words.iter().all(|w| text.next().is_some_and(|t| t.to_lowercase() == w.2.to_lowercase()))
            && text.next().is_none();
        ok.then_some(&self.words[..])
    }
}

/// `words` moved with the cue they belong to: the cue's `[a, b]` in its own clock became
/// `[from, to]` on the timeline (a linear map, as a placement or a speed change is), and the
/// words before `a` are held at its edge; a word that starts at or past `b` -- the cue was cut
/// there, by a range end or a trim -- is dropped, and the last word kept ends no later than `to`.
pub fn map_words(
    words: &[(i64, i64, String)],
    (a, b): (i64, i64),
    (from, to): (i64, i64),
) -> Vec<(i64, i64, String)> {
    let scale = if b > a { (to - from) as f64 / (b - a) as f64 } else { 1.0 };
    let onto = |t: i64| from + ((t.clamp(a, b) - a) as f64 * scale).round() as i64;
    words
        .iter()
        .filter(|(s, _, _)| b <= a || *s < b)
        .map(|(s, e, w)| (onto(*s), onto(*e), w.clone()))
        .collect()
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
    set: Source,
}

/// What a [`CueImage`] keeps until something draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    /// A PGS display set: `type`, big-endian size, body, segment after segment,
    /// which is the shape a Matroska block holds PGS in.
    Pgs(Vec<u8>),
    /// A DVD subpicture unit, with the `.idx` text (palette, size) of its track.
    Dvd { idx: Arc<Vec<u8>>, unit: Vec<u8> },
    /// A DVB display set (PES payload or Matroska block).
    Dvb(Vec<u8>),
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
        let set = match &self.set {
            Source::Pgs(set) => set,
            Source::Dvd { idx, unit } => {
                let mut decoder = ec_dvdsub::DvdSubDecoder::new(Some(idx.as_slice()).filter(|i| !i.is_empty()));
                decoder.push_unit(unit, None).ok()?;
                return frame_rgba(decoder.take_cue()?.frame);
            }
            Source::Dvb(set) => {
                let mut decoder = ec_dvbsub::DvbSubDecoder::new();
                // A damaged segment is dropped by the decoder; what it did draw is kept.
                let _ = decoder.push_pes(set);
                return frame_rgba(decoder.take_cue()?.frame);
            }
        };
        // The decoder reads a `.sup`'s framing and a Matroska block is the same
        // segments with the `PG` magic and the timestamps taken off, so they go
        // back on. The times themselves are the block's and are already on the
        // `Cue`, so they are written as nothing.
        let mut sup = Vec::with_capacity(set.len() + 64);
        // Up to and including the `END` that closes the first display set which
        // composes an object, and no further: a block is normally one set, but
        // a muxer that packs a composition and the erase after it into one would
        // otherwise leave the *erase* as the last word and decode to an empty
        // canvas -- the cue drawn as nothing at all.
        let mut shows = false;
        for (kind, body) in segments(set)? {
            sup.extend_from_slice(b"PG");
            sup.extend_from_slice(&[0; 8]);
            sup.push(kind);
            sup.extend_from_slice(&(body.len() as u16).to_be_bytes());
            sup.extend_from_slice(body);
            match kind {
                ec_pgs::SEG_PCS => {
                    shows = body.get(10).is_some_and(|&n| n > 0);
                }
                ec_pgs::SEG_END if shows => break,
                _ => {}
            }
        }
        // One decoder per display set: the block carries its own epoch, so there
        // is no state from an earlier cue to carry in. The first frame out is the
        // one the set composes -- the segments past it were cut above.
        let mut decoder = ec_pgs::PgsDecoder::new();
        decoder.push(&sup).ok()?;
        let frame = decoder.take_frame()?;
        Some(frame.planes.into_iter().next()?.data.to_vec())
    }
}

/// The pixels of a decoded subtitle frame: its one RGBA plane.
fn frame_rgba(frame: ec_core::frame::VideoFrame) -> Option<Vec<u8>> {
    Some(frame.planes.into_iter().next()?.data.to_vec())
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
///
/// `PartialEq` because it is one of the things an edit can change and therefore
/// one of the things an undo has to bring back exactly
/// ([`crate::Project::parts`], the sweep's round-trip).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubtitleTrack {
    /// The file it was read out of: a standalone subtitle file, or the media
    /// file whose track number is below.
    pub path: PathBuf,
    /// Which track of `path`, for one embedded in a Matroska file. `None` when
    /// `path` *is* the subtitle file.
    pub track: Option<u64>,
    /// What language the track is in, as the container states it (`eng`, `fra`,
    /// `und` for a file that says nothing -- which is what both demuxers hand
    /// over rather than an empty string). Empty for a standalone file until an
    /// import or a retype names one ([`set_language`](crate::Project::set_subtitle_language));
    /// an export writes empty as `und`. A retype on an embedded track replaces
    /// the container's own, and the project file carries it (`sublang` lines).
    ///
    /// Kept apart from [`name`](Self::name) because a container keeps them
    /// apart: an export writes `TRACK_LANGUAGE` and `TRACK_NAME` as two fields
    /// ([`crate::mux::SubParams`]), and a French track whose language is folded
    /// into its title leaves as an English one.
    pub language: String,
    /// What the track is *called* beside its language (`Signs`, `Forced`). Empty
    /// where it has none -- a standalone file has none, its file name is only
    /// what [`label`](Self::label) shows.
    pub name: String,
    /// What a list shows: the [`language`](Self::language), the
    /// [`name`](Self::name), or the pair ([`label_of`]) -- flattened here
    /// because a row shows one string, and derived from the two rather than
    /// being what they are derived from.
    pub label: String,
    /// In start order. Empty when [`refused`](Self::refused) says why.
    pub cues: Vec<Cue>,
    /// This track is pictures rather than text (`S_HDMV/PGS`). Not a refusal:
    /// it is drawn over the picture like any other track, and what it cannot do
    /// is be *written* as text -- an SRT carries none of it.
    ///
    /// Read off the codec id at every open rather than off the cues, because a
    /// PGS track whose display sets are all erases -- or one with no cues at
    /// all -- is still a track of pictures, and calling it text would have an
    /// export promise to write words it does not have.
    pub bitmap: bool,
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
    ///
    /// No language and no name: what is refused is never *written* (an export
    /// drops it before the muxer, [`crate::export::planned_subtitles`]), and the
    /// row it leaves behind shows the `label` it was refused under.
    fn refused(path: &Path, track: Option<u64>, label: String, why: String) -> Self {
        Self {
            path: path.to_path_buf(),
            track,
            language: String::new(),
            name: String::new(),
            label,
            cues: Vec::new(),
            bitmap: false,
            refused: Some(why),
        }
    }

    /// Whether this track's cues are pictures rather than lines -- a PGS track
    /// off a remux. What it decides is where words are the only answer: an
    /// export writes a *text* track and cannot carry these
    /// ([`crate::export::planned_subtitles`] says so out loud).
    pub fn is_bitmap(&self) -> bool {
        self.bitmap
    }
}

/// The one door a saved subtitle line comes back in through: a standalone file
/// (`track` is `None`) or track `n` of a media file, Matroska or mp4
/// ([`of_media`]).
///
/// Never an error, whatever is wrong: a deleted `.srt`, a media file that is
/// not there any more, a track number the file no longer has, a codec of
/// pictures. All of them come back [`refused`](SubtitleTrack::refused) by name,
/// because a project that opened yesterday has to open today -- a subtitle is
/// not what a whole timeline should refuse over.
pub fn open(path: &Path, track: Option<u64>) -> SubtitleTrack {
    let mut one = open_all(std::slice::from_ref(&(path.to_path_buf(), track)));
    one.remove(0)
}

/// Every saved subtitle line of a project at once, back in the order they were
/// saved in -- which is the order they are picked by, so it is the row order
/// this hands back whatever it had to read.
///
/// The reason this exists beside [`open`]: a Matroska file is walked whole to
/// reach *one* of its tracks, and a film whose project names thirty-five of them
/// was thirty-five walks of a 25 GB file -- seven seconds with the window
/// stopped, since a project opens on the UI thread. Each distinct file is walked
/// once here and every row that named it is picked out of that one walk.
pub fn open_all(rows: &[(PathBuf, Option<u64>)]) -> Vec<SubtitleTrack> {
    // Keyed by path, so a second row naming the same file finds the walk rather
    // than doing it again. The whole walk is kept until the last row is served:
    // it is the tracks nobody asked for that are dropped at the end.
    //
    // corner-cut: a track is *moved* out of its walk, so two rows naming the same
    // file and the same track leave the second refused by name rather than
    // holding the cues twice. No project edith saves has one -- a row is added
    // by (file, track) and only once ([`crate::Project::add_subtitles`]) -- and
    // a hand-written `.edith` with a doubled row still opens, with that row
    // saying so. Clone the picked track instead if that ever has to hold.
    let mut walked: HashMap<&Path, crate::Result<Vec<SubtitleTrack>>> = HashMap::new();
    // The files with a caption row: the only ones whose picture is read for captions.
    let wants_captions: std::collections::HashSet<&Path> = rows
        .iter()
        .filter(|(_, t)| t.is_some_and(crate::demux::captions::is_caption_track))
        .map(|(p, _)| p.as_path())
        .collect();
    rows.iter()
        .map(|(path, track)| {
            let Some(number) = *track else {
                return external(path);
            };
            let label = format!("track {number}");
            let tracks = walked
                .entry(path.as_path())
                .or_insert_with(|| of_media_with(path, wants_captions.contains(path.as_path())));
            let tracks = match tracks {
                Ok(tracks) => tracks,
                Err(why) => return SubtitleTrack::refused(path, *track, label, why.to_string()),
            };
            match tracks.iter().position(|t| t.track == Some(number)) {
                Some(i) => tracks.swap_remove(i),
                None => SubtitleTrack::refused(
                    path,
                    *track,
                    label,
                    format!("no subtitle track {number} in {} any more", path.display()),
                ),
            }
        })
        .collect()
}

/// A standalone subtitle file, parsed by its extension.
fn external(path: &Path) -> SubtitleTrack {
    let label = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match parse_file(path) {
        Ok(cues) => SubtitleTrack {
            bitmap: cues.iter().any(|c| c.image.is_some()),
            path: path.to_path_buf(),
            track: None,
            // A file beside the media states no language of its own: it is set
            // by an import ([`guess_language`]) or by a retype, and empty until
            // then, which an export writes as `und`. Its file name is not a
            // title: an export titles it with its language's name or not at all.
            language: String::new(),
            name: String::new(),
            label,
            cues,
            refused: None,
        },
        Err(why) => SubtitleTrack::refused(path, None, label, why.to_string()),
    }
}

/// The languages a file name can name, as `(ISO 639-1, ISO 639-2, the 639-2
/// bibliographic code when it differs, English name)`. Only the common ones, and
/// no two-letter word that would be a name in a file name (`is`, `no`, `to`).
const LANGUAGES: &[(&str, &str, &str, &str)] = &[
    ("en", "eng", "eng", "English"),
    ("fr", "fra", "fre", "French"),
    ("de", "deu", "ger", "German"),
    ("es", "spa", "spa", "Spanish"),
    ("it", "ita", "ita", "Italian"),
    ("pt", "por", "por", "Portuguese"),
    ("nl", "nld", "dut", "Dutch"),
    ("sv", "swe", "swe", "Swedish"),
    ("da", "dan", "dan", "Danish"),
    ("nb", "nob", "nob", "Norwegian"),
    ("fi", "fin", "fin", "Finnish"),
    ("pl", "pol", "pol", "Polish"),
    ("cs", "ces", "cze", "Czech"),
    ("sk", "slk", "slo", "Slovak"),
    ("hu", "hun", "hun", "Hungarian"),
    ("ro", "ron", "rum", "Romanian"),
    ("bg", "bul", "bul", "Bulgarian"),
    ("el", "ell", "gre", "Greek"),
    ("tr", "tur", "tur", "Turkish"),
    ("ru", "rus", "rus", "Russian"),
    ("uk", "ukr", "ukr", "Ukrainian"),
    ("ar", "ara", "ara", "Arabic"),
    ("he", "heb", "heb", "Hebrew"),
    ("hi", "hin", "hin", "Hindi"),
    ("ja", "jpn", "jpn", "Japanese"),
    ("ko", "kor", "kor", "Korean"),
    ("zh", "zho", "chi", "Chinese"),
    ("th", "tha", "tha", "Thai"),
    ("vi", "vie", "vie", "Vietnamese"),
    ("id", "ind", "ind", "Indonesian"),
    ("ms", "msa", "may", "Malay"),
    ("fa", "fas", "per", "Persian"),
    ("ca", "cat", "cat", "Catalan"),
    ("hr", "hrv", "hrv", "Croatian"),
    ("sr", "srp", "srp", "Serbian"),
    ("sl", "slv", "slv", "Slovenian"),
    ("et", "est", "est", "Estonian"),
    ("lv", "lav", "lav", "Latvian"),
    ("lt", "lit", "lit", "Lithuanian"),
];

/// The ISO 639-2 code a file name's language token names, when it names one
/// (`fr` and `fra` are both French, `fre` is French too).
fn code_of_token(token: &str) -> Option<&'static str> {
    let token = token.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(two, three, bib, _)| match token.len() {
            2 => *two == token,
            3 => *three == token || *bib == token,
            _ => false,
        })
        .map(|(_, three, _, _)| *three)
}

/// The language a subtitle file's name says it is, as the ISO 639-2 code an
/// import sets: the last token of the name that is a language (`talk.fr.srt`,
/// `talk.fra.srt`, `fra.srt`, `talk_en.srt`, `eng.srt` are French, French,
/// French, English, English). `None` when no token of the name is one -- the
/// track is then left unset, not guessed.
pub fn guess_language(path: &Path) -> Option<&'static str> {
    let stem = path.file_stem()?.to_str()?;
    stem.rsplit(['.', '_', '-', ' '])
        .find_map(code_of_token)
}

/// The ISO 639-2 code a person typed for a language: a 639-1 two-letter code
/// (`fr`), an English name (`French`), or a three-letter code as it is written
/// (`fra`, `und`), which is taken as it is. `None` for anything else.
pub fn language_code(input: &str) -> Option<String> {
    let input = input.trim().to_ascii_lowercase();
    match input.len() {
        2 => LANGUAGES
            .iter()
            .find(|(two, _, _, _)| *two == input)
            .map(|(_, three, _, _)| three.to_string()),
        3 if input.bytes().all(|b| b.is_ascii_lowercase()) => {
            Some(code_of_token(&input).map_or_else(|| input.clone(), str::to_string))
        }
        _ => LANGUAGES
            .iter()
            .find(|(_, _, _, name)| name.eq_ignore_ascii_case(&input))
            .map(|(_, three, _, _)| three.to_string()),
    }
}

/// The English name of an ISO 639-2 code, as an export titles its track
/// (`fra` is `French`). `None` for a code the list does not name.
pub fn language_name(code: &str) -> Option<&'static str> {
    let code = code.to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(_, three, bib, _)| *three == code || *bib == code)
        .map(|(_, _, _, name)| *name)
}

/// The language an import gives a track: the `lang` a person typed, or else the
/// one the file name says, with where it came from for the reply (`as given`,
/// `from the file name`). `None` when neither names one. Refused, with the words
/// to say so, for a `lang` that is not a language.
pub fn import_language(path: &Path, lang: Option<&str>) -> Result<Option<(String, &'static str)>, String> {
    match lang {
        Some(raw) => language_code(raw)
            .map(|code| Some((code, "as given")))
            .ok_or_else(|| format!("`{raw}` is not a language: give its code (fra or fr) or its English name (French)")),
        None => Ok(guess_language(path).map(|code| (code.to_string(), "from the file name"))),
    }
}

/// Names the language of `track` (an ISO 639-2 code, or empty for unset) and
/// keeps its row's label in step: an embedded track's label shows its language,
/// and a standalone file's label is its file name, which does not change.
pub fn set_language(track: &mut SubtitleTrack, code: &str) {
    track.language = code.to_string();
    if track.track.is_some() && track.refused.is_none() {
        track.label = label_of(&track.language, &track.name);
    }
}

/// Every subtitle track *inside* a media file, whichever container it is: a
/// Matroska's ([`of_matroska`]) or an mp4's timed text ([`of_mp4`]).
///
/// The one door for it, so a project row, an import and an export's read-back
/// cannot disagree about what a file carries -- and so an mp4 this project
/// wrote comes back into it, which is the whole reason the mp4 reader exists:
/// mp4 is the default export format, and a file edith writes and cannot open
/// is not a round trip.
pub fn of_media(path: &Path) -> crate::Result<Vec<SubtitleTrack>> {
    of_media_with(path, true)
}

/// [`of_media`], with the embedded closed captions ([`of_captions`]) listed after the container's
/// own tracks only when `captions` says so: they cost a read of every picture packet, so a
/// project re-open asks for them only for a file that has a caption row.
fn of_media_with(path: &Path, captions: bool) -> crate::Result<Vec<SubtitleTrack>> {
    let mut tracks = match crate::demux::is_matroska(path) {
        true => of_matroska(path),
        false if crate::demux::is_mpeg_system(path) => of_mpegts(path),
        false => of_mp4(path),
    }?;
    if captions {
        tracks.extend(of_captions(path));
    }
    Ok(tracks)
}

/// The CEA-608 (CC1-CC4) and CEA-708 (service 1) captions carried inside the picture of `path`
/// (H.264/HEVC SEI, MPEG-2 user data), one track per channel that has cues, named `CC1 (608)` or
/// `Service 1 (708)`. Timed on the picture's own clock. Found by one cached read of every
/// picture packet ([`crate::demux::captions`]); empty for a file without captions.
pub fn of_captions(path: &Path) -> Vec<SubtitleTrack> {
    crate::demux::captions::tracks(path)
        .iter()
        .map(|t| SubtitleTrack {
            path: path.to_path_buf(),
            track: Some(t.number),
            language: "und".to_string(),
            name: t.name.clone(),
            label: label_of("und", &t.name),
            cues: t
                .cues
                .iter()
                .map(|c| Cue {
                    start_us: c.cue.start_us,
                    end_us: c.cue.end_us,
                    text: plain_text(&c.cue.segments),
                    image: None,
                    words: Vec::new(),
                    karaoke: Vec::new(),
                })
                .collect(),
            bitmap: false,
            refused: None,
        })
        .collect()
}

/// Every PGS subtitle track of an MPEG transport stream (an AVCHD or Blu-ray `.m2ts`), in
/// PMT order: the same display sets a Matroska file carries, so they are read the same way
/// ([`pgs_cues`]). `track` is the demuxer's stream index.
pub fn of_mpegts(path: &Path) -> crate::Result<Vec<SubtitleTrack>> {
    Ok(crate::demux::ts::subtitles(path)?
        .into_iter()
        .map(|mut t| {
            let label = label_of(&t.language, &t.name);
            let cues = match t.codec.as_str() {
                crate::demux::VOBSUB => dvd_cues(&mut t),
                crate::demux::DVBSUB => dvb_cues(&mut t),
                _ => pgs_cues(&mut t),
            };
            SubtitleTrack {
                path: path.to_path_buf(),
                track: Some(t.number),
                language: t.language,
                name: t.name,
                label,
                cues,
                bitmap: true,
                refused: None,
            }
        })
        .collect())
}

/// Every timed-text (`tx3g`) subtitle track of an mp4, in track order, with its
/// cues -- the mp4 twin of [`of_matroska`].
///
/// A `tx3g` track is *continuous*: it states one sample per instant of the film
/// and the stretches with nothing on screen are samples of no text, so those are
/// dropped here rather than kept as cues with an empty line
/// ([`crate::demux::Mp4TextSample::payload`]). What is left is the cues, timed
/// on the file's own clock, and a track whose samples are all empty comes back
/// with none rather than with one per instant.
///
/// Never a bitmap: an mp4 carries no PGS track this reads, so the refusal
/// [`of_matroska`] has for one has no counterpart here.
pub fn of_mp4(path: &Path) -> crate::Result<Vec<SubtitleTrack>> {
    Ok(crate::demux::mp4_subtitles(path)?
        .into_iter()
        .map(|t| SubtitleTrack {
            path: path.to_path_buf(),
            track: Some(t.number),
            label: label_of(&t.language, &t.name),
            language: t.language,
            name: t.name,
            cues: t.samples.iter().filter_map(tx3g_cue).collect(),
            bitmap: false,
            refused: None,
        })
        .collect())
}

/// One `tx3g` sample as a cue, or `None` for the sample that is not one: the
/// empty one that says nothing is on screen between here and the next line, and
/// the truncated one whose declared length runs past its own bytes.
fn tx3g_cue(sample: &crate::demux::Mp4TextSample) -> Option<Cue> {
    let len = usize::from(u16::from_be_bytes([
        *sample.payload.first()?,
        *sample.payload.get(1)?,
    ]));
    let text = String::from_utf8_lossy(sample.payload.get(2..2 + len)?);
    (!text.is_empty()).then(|| Cue {
        start_us: sample.start_us,
        end_us: sample.end_us,
        text: text.into_owned(),
        image: None,
        words: Vec::new(),
        karaoke: Vec::new(),
    })
}

/// What a list row shows for a track a container declares: the language, the
/// muxer's title, or the pair.
///
/// A row shows one string, so the two are flattened for it -- and *only* for it.
/// Both are kept on the track beside this
/// ([`SubtitleTrack::language`], [`SubtitleTrack::name`]), so an export writes
/// the container's own two fields rather than guessing them back out of this
/// string, which is what used to leave every French track marked English.
fn label_of(language: &str, name: &str) -> String {
    match (name.is_empty(), language) {
        (true, lang) => lang.to_owned(),
        (false, "und") => name.to_owned(),
        (false, lang) => format!("{lang} — {name}"),
    }
}

/// Every subtitle track of a Matroska file, in file order -- the text ones with
/// their cues, the bitmap ones refused by name (see the module docs).
pub fn of_matroska(path: &Path) -> crate::Result<Vec<SubtitleTrack>> {
    Ok(crate::demux::matroska_subtitles(path)?
        .into_iter()
        .map(|mut t| {
            let label = label_of(&t.language, &t.name);
            let bitmap = matches!(t.codec.as_str(), crate::demux::PGS | crate::demux::VOBSUB | crate::demux::DVBSUB);
            match cues_of(&mut t) {
                Ok(cues) => SubtitleTrack {
                    path: path.to_path_buf(),
                    track: Some(t.number),
                    language: t.language,
                    name: t.name,
                    label,
                    cues,
                    bitmap,
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
        "S_TEXT/UTF8" | "S_TEXT/ASCII" => text_cues(track, srt_body),
        // WebVTT in Matroska is `D_WEBVTT/<kind>` (ffmpeg and mkvmerge both
        // write it so); the older `S_TEXT/WEBVTT` is the same payload.
        "D_WEBVTT/SUBTITLES"
        | "D_WEBVTT/CAPTIONS"
        | "D_WEBVTT/DESCRIPTIONS"
        | "D_WEBVTT/METADATA"
        | "S_TEXT/WEBVTT" => text_cues(track, webvtt_body),
        "S_TEXT/ASS" | "S_TEXT/SSA" => ass_cues(track)?,
        crate::demux::PGS => pgs_cues(track),
        crate::demux::VOBSUB => dvd_cues(track),
        crate::demux::DVBSUB => dvb_cues(track),
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
                words: Vec::new(),
                karaoke: Vec::new(),
                image: Some(Arc::new(CueImage {
                    width,
                    height,
                    set: Source::Pgs(std::mem::take(&mut block.payload)),
                })),
            })
        })
        .collect()
}

/// How long a bitmap cue with no stated end stays up: two seconds.
const NO_END_US: i64 = 2_000_000;

/// Gives every cue whose end is unstated the start of the next cue (or two seconds
/// for the last), and never lets a stated end run past the next cue's start.
fn settle(spans: &mut [(i64, Option<i64>)]) {
    for i in 0..spans.len() {
        let next = spans.get(i + 1).map(|n| n.0);
        let (start, end) = spans[i];
        let end = match (end, next) {
            (Some(e), Some(n)) => e.min(n.max(start)),
            (Some(e), None) => e,
            (None, Some(n)) => n,
            (None, None) => start + NO_END_US,
        };
        spans[i].1 = Some(end);
    }
}

/// The cues of an `S_VOBSUB` track (or a `.idx`/`.sub` pair): one subpicture unit per
/// block, which carries its own delay and end after the block's time. The `.idx` text
/// is the palette and canvas.
fn dvd_cues(track: &mut crate::demux::MkvSubtitle) -> Vec<Cue> {
    let units = std::mem::take(&mut track.cues)
        .into_iter()
        .map(|b| (b.start_us, b.duration_us.filter(|&d| d > 0), b.payload))
        .collect();
    dvd_units(units, std::mem::take(&mut track.private))
}

/// [`dvd_cues`] on `(time in microseconds, stated duration, unit)` triples.
fn dvd_units(units: Vec<(i64, Option<i64>, Vec<u8>)>, idx: Vec<u8>) -> Vec<Cue> {
    let parsed = ec_dvdsub::parse_idx(&idx);
    let palette = parsed.palette.unwrap_or(ec_dvdsub::DEFAULT_PALETTE);
    let canvas = parsed.size.unwrap_or((720, 576));
    let idx = Arc::new(idx);
    let us = |t: i64| t * 1_000_000 / 90_000;
    let mut spans = Vec::new();
    let mut made = Vec::new();
    for (at, duration, unit) in units {
        let Ok(cue) = ec_dvdsub::decode_spu(&unit, &palette, canvas) else {
            continue;
        };
        let start = at + us(cue.start_90k);
        let end = cue.end_90k.map(|e| at + us(e)).or(duration.map(|d| at + d));
        spans.push((start, end));
        made.push((cue.frame.width, cue.frame.height, unit));
    }
    settle(&mut spans);
    spans
        .into_iter()
        .zip(made)
        .map(|((start_us, end), (width, height, unit))| Cue {
            start_us,
            end_us: end.unwrap_or(start_us + NO_END_US),
            text: String::new(),
            words: Vec::new(),
            karaoke: Vec::new(),
            image: Some(Arc::new(CueImage {
                width,
                height,
                set: Source::Dvd { idx: idx.clone(), unit },
            })),
        })
        .collect()
}

/// The cues of an `S_DVBSUB` track (or a transport stream's DVB PID): one display set
/// per block. A set with no regions is the clear that ends the one before it; a set
/// states how long it stays up (`timeout`), cut short by the next set.
fn dvb_cues(track: &mut crate::demux::MkvSubtitle) -> Vec<Cue> {
    // (start, end, canvas, empty, set)
    let mut sets = Vec::new();
    for block in std::mem::take(&mut track.cues) {
        let mut decoder = ec_dvbsub::DvbSubDecoder::new();
        let _ = decoder.push_pes(&block.payload);
        let Some(cue) = decoder.take_cue() else {
            continue;
        };
        let stated = block
            .duration_us
            .filter(|&d| d > 0)
            .or((cue.timeout_ms > 0).then(|| i64::from(cue.timeout_ms) * 1000));
        sets.push((
            block.start_us,
            stated.map(|d| block.start_us + d),
            (cue.frame.width, cue.frame.height),
            cue.empty,
            block.payload,
        ));
    }
    let mut spans: Vec<(i64, Option<i64>)> = sets.iter().map(|s| (s.0, s.1)).collect();
    settle(&mut spans);
    spans
        .into_iter()
        .zip(sets)
        .filter(|(_, s)| !s.3)
        .map(|((start_us, end), (_, _, (width, height), _, set))| Cue {
            start_us,
            end_us: end.unwrap_or(start_us + NO_END_US),
            text: String::new(),
            words: Vec::new(),
            karaoke: Vec::new(),
            image: Some(Arc::new(CueImage {
                width,
                height,
                set: Source::Dvb(set),
            })),
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

/// The cues of a Matroska text track whose blocks are one cue body each: the
/// timing is the block's, and `body` resolves the markup of one block's text.
fn text_cues(
    track: &crate::demux::MkvSubtitle,
    body: fn(&str) -> String,
) -> Vec<Cue> {
    track
        .cues
        .iter()
        .map(|c| Cue {
            start_us: c.start_us,
            end_us: end_of(track, c),
            text: body(&String::from_utf8_lossy(&c.payload)),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        })
        .collect()
}

/// The text of one WebVTT block with its markup resolved: the WebVTT parser
/// reads a one-cue file built around it, the same way [`srt_body`] does for an
/// SRT block. The block's own timing wins, so the timing line here is a
/// placeholder that is thrown away.
fn webvtt_body(text: &str) -> String {
    // The block's bytes can open with blank lines (the cue identifier slot), and
    // a blank line would end the timing block before the text.
    let doc = format!(
        "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n{}\n",
        text.trim_matches(['\n', '\r']).trim_end()
    );
    match ec_subs::webvtt::parse(doc.as_bytes()) {
        Ok(track) => track
            .cues
            .first()
            .map(|c| plain_text(&c.segments))
            .unwrap_or_default(),
        Err(_) => text.trim_end().to_owned(),
    }
}

/// The text of one `S_TEXT/UTF8` block with its markup resolved: through the
/// SRT parser, around a timing line this then throws away, because SRT markup
/// is exactly what such a block holds and a second implementation of it here
/// would be a second set of bugs.
fn srt_body(text: &str) -> String {
    let doc = format!("1\n00:00:00,000 --> 00:00:01,000\n{}\n", text.trim_end());
    match ec_subs::srt::parse(doc.as_bytes()) {
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
    let parsed = ec_ass::parse(doc.as_bytes()).map_err(|e| e.to_string())?;
    // The timings above are centiseconds, which is all an ASS line can say;
    // the blocks are microseconds. So the text comes from the parse and the
    // timing from the block -- unless a line was dropped as unparsable, when
    // the pairing is no longer sound and the parse answers for both.
    let exact = parsed.cues.len() == track.cues.len();
    Ok(parsed
        .cues
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let start_us = if exact { track.cues[i].start_us } else { c.start_us };
            // The syllables are timed from the line's own start, which the block may differ from.
            let mut karaoke = syllables(c, &parsed);
            for s in &mut karaoke {
                s.start_us += start_us - c.start_us;
                s.end_us += start_us - c.start_us;
            }
            Cue {
                start_us,
                end_us: if exact {
                    end_of(track, &track.cues[i])
                } else {
                    c.end_us
                },
                text: plain_text(&c.segments),
                image: None,
                words: Vec::new(),
                karaoke,
            }
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
    Ok(read_file(path)?.0)
}

/// The cues of a VobSub pair: `name.idx` (palette, canvas) beside `name.sub` (the
/// subpicture units in an MPEG program stream). Either file may be the one named.
fn vobsub_file_cues(path: &Path) -> crate::Result<Vec<Cue>> {
    let (idx_path, sub_path) = (path.with_extension("idx"), path.with_extension("sub"));
    let (Ok(idx), Ok(sub)) = (std::fs::read(&idx_path), std::fs::read(&sub_path)) else {
        return Err(format!(
            "a VobSub subtitle is a pair, {} and {}, and one of them is missing or unreadable",
            idx_path.display(),
            sub_path.display()
        )
        .into());
    };
    // The first subpicture stream of the program (substream 0x20); `stream` is its index.
    let units = ec_dvdsub::sub_file_units(&sub, 0)
        .into_iter()
        .map(|(unit, pts_90k)| (pts_90k * 1_000_000 / 90_000, None, unit))
        .collect();
    let cues = dvd_units(units, idx);
    if cues.is_empty() {
        return Err(format!("no subtitle pictures in {}", sub_path.display()).into());
    }
    Ok(cues)
}

/// The lines of a text subtitle file that were not imported, each with its
/// line number and why (`line 34: end before start`). Empty for a file that
/// read whole, and for an ASS script, whose cues have no such lines to skip.
pub fn skipped_cues(path: &Path) -> Vec<String> {
    read_file(path).map(|(_, skipped)| skipped).unwrap_or_default()
}

/// The cues of a text subtitle file, and the timing lines that were not made
/// into one. See [`timing_lines`] for why the file is read twice.
fn read_file(path: &Path) -> crate::Result<(Vec<Cue>, Vec<String>)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(ext.as_str(), "idx" | "sub") {
        return Ok((vobsub_file_cues(path)?, Vec::new()));
    }
    // The extension before the read: a `.txt` is refused for what it is, not
    // for whether it happens to be there.
    type Parse = fn(&[u8]) -> ec_core::Result<ec_subs::SubtitleTrack>;
    let parse: Parse = match ext.as_str() {
        "srt" => ec_subs::srt::parse,
        "vtt" | "webvtt" => ec_subs::webvtt::parse,
        "ass" | "ssa" => ec_ass::parse,
        other => {
            // What is *genuinely* accepted, all of it: the subtitle files this
            // parses and the containers [`of_media`] walks for the tracks
            // inside them (`.mkv`, `.mka`, `.mks`, `.mk3d`, `.webm`, `.mp4`,
            // `.m4v`, `.mov`). The old sentence named the four text formats
            // alone and so refused a `.mks` this very binary reads.
            return Err(format!(
                "{other:?} is not a subtitle format this reads — .srt, .vtt, .ass and .ssa are, \
                 as are the subtitle tracks inside a Matroska or an mp4"
            )
            .into());
        }
    };
    let vtt = matches!(ext.as_str(), "vtt" | "webvtt");
    let ass = matches!(ext.as_str(), "ass" | "ssa");
    let bytes = std::fs::read(path)?;
    let marked;
    let input: &[u8] = if ass {
        &bytes
    } else {
        marked = mark_breaks(&bytes);
        &marked
    };
    let track = parse(input);
    let track = track.map_err(|e| format!("{}: {e}", path.display()))?;
    let cue_of = |c: &ec_subs::SubtitleCue| {
        // ec-subs strips the markup and resolves the character references in
        // one pass, so a `&lt;` written in a WebVTT file is text, not a tag,
        // and is not decoded a second time here.
        let text = plain_text(&c.segments).replace(char::from(BREAK_MARK), "\n");
        Cue {
            start_us: c.start_us,
            end_us: c.end_us,
            text,
            image: None,
            words: Vec::new(),
            karaoke: syllables(c, &track),
        }
    };
    if ass {
        return Ok((track.cues.iter().map(cue_of).collect(), Vec::new()));
    }
    // The parser keeps a cue whose end is before its start and drops one whose
    // timing it cannot read, both without a word. Each timing line is matched
    // to the parser's next cue: a line it is a cue for is kept or, if its
    // timing is wrong, named as skipped; a line with no cue is named too.
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    let mut next = 0;
    for line in timing_lines(text, vtt) {
        let matched = match (line.span, track.cues.get(next)) {
            // ec-subs clamps a backwards pair to a zero-length cue, so the
            // cue to match is the clamped one; the line is still skipped.
            (Some((s, e)), Some(c)) if (c.start_us, c.end_us) == (s, e.max(s)) => {
                next += 1;
                true
            }
            _ => false,
        };
        match line.why {
            Some(why) => skipped.push(format!("line {}: {why}", line.line)),
            None if matched => kept.push(cue_of(&track.cues[next - 1])),
            None => skipped.push(format!("line {}: the parser did not read this cue", line.line)),
        }
    }
    // A cue no timing line claimed is kept as the parser gave it.
    kept.extend(track.cues[next..].iter().map(cue_of));
    kept.sort_by_key(|c| c.start_us);
    Ok((kept, skipped))
}

/// The byte a `<br>` line break is marked with on its way through the parser,
/// which drops the tag and the break with it. A control character, so no cue's
/// own text carries one; `cue_of` turns it back into a newline.
const BREAK_MARK: u8 = 0x01;

/// The file's bytes with each `<br>`, `<br/>` or `<br />` replaced by
/// [`BREAK_MARK`]. A byte-level pass, so a timing line is untouched and its
/// line number still counts.
fn mark_breaks(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'<' {
            // The tag's own name, with its case, spaces and slash set aside.
            if let Some(close) = bytes[at..].iter().take(9).position(|&b| b == b'>') {
                let name: Vec<u8> = bytes[at + 1..at + close]
                    .iter()
                    .filter(|b| !b.is_ascii_whitespace() && **b != b'/')
                    .map(u8::to_ascii_lowercase)
                    .collect();
                if name == b"br" {
                    out.push(BREAK_MARK);
                    at += close + 1;
                    continue;
                }
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    out
}

/// One timing line of a text subtitle file, `start --> end`: where it is, and
/// either the span it gives or why it gives none.
struct TimingLine {
    /// Its number in the file, from 1.
    line: usize,
    /// Its start and end in microseconds, when both are timestamps.
    span: Option<(i64, i64)>,
    /// Why the cue it begins is not imported, when it is not: an end before
    /// its start, or a time that is not one.
    why: Option<String>,
}

/// Every timing line of an SRT or WebVTT text, and every cue block that has
/// none, in file order. The parser reads the same lines itself, but drops a cue
/// it cannot time without a word and keeps one that runs backwards, so the reply
/// needs them read again here.
///
/// A line counts when its left side is a time-like run (a digit, then a `:`),
/// so a cue whose words happen to hold an arrow is not one. A time-like pair
/// with a wrong arrow (`->`) is named too, and so is a block that opens like a
/// cue (see below) and holds no timing line at all.
fn timing_lines(text: &str, vtt: bool) -> Vec<TimingLine> {
    // The blocks of the file: what runs between blank lines, each with the
    // number and text of its lines.
    let mut blocks: Vec<Vec<(usize, &str)>> = vec![Vec::new()];
    for (at, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            blocks.push(Vec::new());
        } else if let Some(block) = blocks.last_mut() {
            block.push((at + 1, line));
        }
    }
    let mut out = Vec::new();
    for block in blocks.iter().filter(|b| !b.is_empty()) {
        let mut timed = false;
        for &(n, line) in block {
            if let Some(t) = cue_timing(n, line) {
                timed = true;
                out.push(t);
            } else if !line.contains("-->") && is_bad_arrow(line) {
                timed = true;
                let why = "timing arrow must be -->".to_string();
                out.push(TimingLine { line: n, span: None, why: Some(why) });
            }
        }
        // A block that opens like a cue but has no timing line is named by its
        // first line. In SRT that is an index number; in WebVTT, any block that
        // is not the header, a NOTE, a STYLE or a REGION.
        let (first_no, first) = block[0];
        let first = first.trim();
        let opens = if vtt {
            !matches!(first.split_whitespace().next(), Some("WEBVTT" | "NOTE" | "STYLE" | "REGION"))
        } else {
            first.bytes().all(|b| b.is_ascii_digit())
        };
        if !timed && opens {
            let why = "cue has no timing line".to_string();
            out.push(TimingLine { line: first_no, span: None, why: Some(why) });
        }
    }
    out.sort_by_key(|t| t.line);
    out
}

/// The timing line on one line of text, when it is one: `start --> end`, with
/// a time-like start. `None` for any other line.
fn cue_timing(line_no: usize, line: &str) -> Option<TimingLine> {
    let (start, rest) = line.split_once("-->")?;
    let start = start.trim();
    if !start.starts_with(|c: char| c.is_ascii_digit()) || !start.contains(':') {
        return None;
    }
    // A WebVTT cue setting (`align:start`) follows the end time.
    let end = rest.split_whitespace().next().unwrap_or("");
    let (from, to) = (clock_us(start), clock_us(end));
    let (span, why) = match (from, to) {
        (Some(s), Some(e)) if e < s => (Some((s, e)), Some("end before start".to_string())),
        (Some(s), Some(e)) if e == s => {
            (Some((s, e)), Some("start and end are the same time".to_string()))
        }
        (Some(s), Some(e)) => (Some((s, e)), None),
        (None, _) => (None, Some(format!("start time `{start}` is not a timestamp"))),
        (Some(_), None) => (None, Some(format!("end time `{end}` is not a timestamp"))),
    };
    Some(TimingLine { line: line_no, span, why })
}

/// Whether a line without `-->` reads as a timing line with a wrong arrow: a
/// time-like run, then one or more dashes and a `>`, then a time-like run
/// (`00:00:02,000 -> 00:00:03,000`).
fn is_bad_arrow(line: &str) -> bool {
    let Some(gt) = line.find('>') else {
        return false;
    };
    let before = &line[..gt];
    let head = before.trim_end_matches('-');
    if head.len() == before.len() {
        return false;
    }
    let (start, end) = (head.trim(), &line[gt + 1..]);
    start.starts_with(|c: char| c.is_ascii_digit()) && start.contains(':') && end.contains(':')
}

/// A subtitle timestamp, `[H:]MM:SS` then `.` or `,` and up to three digits of
/// fraction, as microseconds. `None` for anything else, including minutes or
/// seconds of sixty and more.
fn clock_us(text: &str) -> Option<i64> {
    let (hms, frac) = text.split_once(['.', ','])?;
    if frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = hms.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut secs: i64 = 0;
    for (k, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 9 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let v: i64 = part.parse().ok()?;
        // Only the hours are unbounded; minutes and seconds are below sixty.
        if k > 0 && v >= 60 {
            return None;
        }
        secs = secs * 60 + v;
    }
    let millis: i64 = format!("{frac:0<3}").parse().ok()?;
    Some((secs * 1000 + millis) * 1000)
}

/// One cue as a writer states it: whole milliseconds, and lines that are
/// neither empty nor carry a stray carriage return. Shared by [`write_srt`] and
/// [`write_vtt`] so the two cannot disagree about which cues exist.
///
/// Picture cues are dropped (a text file cannot carry them), as are cues with
/// no words and cues whose millisecond span is empty or negative once rounded.
/// Negative starts are clamped to zero; the result is sorted by start.
fn written_cues(cues: &[Cue]) -> Vec<(i64, i64, Vec<String>)> {
    let mut out: Vec<(i64, i64, Vec<String>)> = cues
        .iter()
        .filter(|c| c.image.is_none())
        .filter_map(|c| {
            let start = ms_of(c.start_us.max(0));
            let end = ms_of(c.end_us.max(0));
            if end <= start {
                return None;
            }
            let lines: Vec<String> = c
                .text
                .lines()
                .map(|l| l.trim_end().to_owned())
                .filter(|l| !l.is_empty())
                .collect();
            (!lines.is_empty()).then_some((start, end, lines))
        })
        .collect();
    out.sort_by_key(|&(start, end, _)| (start, end));
    out
}

/// Microseconds to whole milliseconds, rounding half up. The input is never
/// negative here: [`written_cues`] clamps first.
fn ms_of(us: i64) -> i64 {
    (us + 500) / 1000
}

/// `HH:MM:SS<sep>mmm`, where `sep` is `,` for SRT and `.` for WebVTT.
fn clock(ms: i64, sep: char) -> String {
    format!(
        "{:02}:{:02}:{:02}{sep}{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// The cues as a SubRip file: numbered from 1, in start order, `HH:MM:SS,mmm`
/// times, lines as the cue has them. SRT has no escaping, so the text goes out
/// as it is. Picture cues and empty cues are left out (see [`written_cues`]).
pub fn write_srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, (start, end, lines)) in written_cues(cues).iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n",
            i + 1,
            clock(*start, ','),
            clock(*end, ',')
        ));
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// The cues as a WebVTT file: the `WEBVTT` header, then numbered cues in start
/// order with `HH:MM:SS.mmm` times. Cue text is escaped as WebVTT reads it:
/// `&` becomes `&amp;`, `<` becomes `&lt;` and `>` becomes `&gt;`. Escaping `>`
/// is also what keeps a `-->` in the words from reading as a timing line.
/// Picture cues and empty cues are left out (see [`written_cues`]).
pub fn write_vtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for (i, (start, end, lines)) in written_cues(cues).iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n",
            i + 1,
            clock(*start, '.'),
            clock(*end, '.')
        ));
        for line in lines {
            out.push_str(&escape_vtt(line));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

/// The three characters WebVTT cue text cannot carry as they are.
fn escape_vtt(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Save cues to `path`, as SRT for `.srt` and WebVTT for `.vtt` (any case). The
/// extension is checked before anything is written, so a `.txt` is refused and
/// leaves nothing behind.
///
/// Written through `<path>.part` and a rename, as [`crate::edith::save`] does:
/// the bytes are fsynced before the name exists, the directory after, and a
/// failed write removes the part file.
pub fn save_cues(path: &Path, cues: &[Cue]) -> crate::Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let text = match ext.as_str() {
        "srt" => write_srt(cues),
        "vtt" => write_vtt(cues),
        other => {
            return Err(format!(
                "{other:?} is not a caption format this writes -- .srt and .vtt are"
            )
            .into());
        }
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut part = path.to_path_buf().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = std::fs::File::create(&part)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(text.as_bytes())?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&part, path))
        .and_then(|()| std::fs::File::open(dir)?.sync_all());
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map_err(Into::into)
}

/// Group timed words into caption cues. `words` is `(start_us, end_us, word)`,
/// sorted by start here whatever order it arrives in; words that are empty
/// after trimming are skipped.
///
/// A cue is closed before a word that would take it past any of these:
/// - a pause longer than `gap_secs` since the previous word ended;
/// - `max_secs` from the cue's first word to this word's end;
/// - two lines of `max_chars` each: the words joined with spaces may not exceed
///   `2 * max_chars` characters.
///
/// A cue is also closed after a word that ends a sentence (`.`, `!`, `?`, `…`,
/// optionally followed by closing quotes or brackets). A cue of more than
/// `max_chars` characters is split over two lines at the word boundary that
/// makes the longer line shortest, so the lines come out as close to even as the
/// words allow. Text is the lines joined with `\n`; no picture.
///
/// A single word longer than the limits is still a cue of its own rather than
/// being dropped or cut.
pub fn cues_from_words(
    words: &[(i64, i64, String)],
    max_chars: usize,
    max_secs: f64,
    gap_secs: f64,
) -> Vec<Cue> {
    let mut sorted: Vec<&(i64, i64, String)> =
        words.iter().filter(|w| !w.2.trim().is_empty()).collect();
    sorted.sort_by_key(|w| w.0);
    let max_us = (max_secs * 1e6) as i64;
    let gap_us = (gap_secs * 1e6) as i64;
    let joined_len = |cur: &[&(i64, i64, String)]| -> usize {
        cur.iter()
            .map(|w| w.2.trim().chars().count())
            .sum::<usize>()
            + cur.len().saturating_sub(1)
    };
    let mut out = Vec::new();
    let mut cur: Vec<&(i64, i64, String)> = Vec::new();
    for &word in &sorted {
        if let Some(&last) = cur.last() {
            let pause = word.0 - last.1 > gap_us;
            let too_long = word.1 - cur[0].0 > max_us;
            let too_wide = joined_len(&cur) + 1 + word.2.trim().chars().count() > 2 * max_chars;
            if pause || too_long || too_wide {
                out.push(words_cue(&cur, max_chars));
                cur.clear();
            }
        }
        cur.push(word);
        if ends_sentence(&word.2) {
            out.push(words_cue(&cur, max_chars));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(words_cue(&cur, max_chars));
    }
    out
}

/// One cue out of its words: timed from the first word's start to the last
/// word's end, text laid out over at most two lines.
fn words_cue(cur: &[&(i64, i64, String)], max_chars: usize) -> Cue {
    let words: Vec<&str> = cur.iter().map(|w| w.2.trim()).collect();
    let len = |ws: &[&str]| {
        ws.iter().map(|w| w.chars().count()).sum::<usize>() + ws.len().saturating_sub(1)
    };
    let total = len(&words);
    let text = if total <= max_chars || words.len() < 2 {
        words.join(" ")
    } else {
        // The split that keeps the longer of the two lines shortest; ties go to
        // the one nearest the middle.
        let middle = total as i64 / 2;
        let mut best: Option<(usize, (usize, i64))> = None;
        for k in 1..words.len() {
            let (a, b) = (len(&words[..k]), len(&words[k..]));
            let score = (a.max(b), (a as i64 - middle).abs());
            if best.is_none_or(|(_, s)| score < s) {
                best = Some((k, score));
            }
        }
        let k = best.map_or(1, |(k, _)| k);
        format!("{}\n{}", words[..k].join(" "), words[k..].join(" "))
    };
    let start = cur[0].0;
    let end = cur.last().map_or(start, |w| w.1).max(start);
    Cue {
        start_us: start,
        end_us: end,
        text,
        image: None,
        words: Vec::new(),
        karaoke: Vec::new(),
    }
}

/// Whether a word closes a sentence: final `.`, `!`, `?` or `…`, looking past
/// closing quotes and brackets.
fn ends_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', ']', '”', '’'])
        .ends_with(['.', '!', '?', '…'])
}

/// The decoding path checked against a snapshot of the oxideav one it replaced.
#[cfg(test)]
mod parity;

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

    /// The sample that is not a cue: a `tx3g` track states one sample per
    /// instant, so the stretches with nothing on screen are samples of no text
    /// -- a track of nothing but those is a track with no cues, not one blank
    /// cue per instant.
    #[test]
    fn an_empty_timed_text_sample_is_not_a_cue() {
        let sample = |payload: Vec<u8>| crate::demux::Mp4TextSample {
            start_us: 500_000,
            end_us: 1_500_000,
            payload,
        };
        let mut text = vec![0, 5];
        text.extend_from_slice(b"lines");
        let cue = tx3g_cue(&sample(text)).expect("a sample with words is a cue");
        assert_eq!(
            (cue.start_us, cue.end_us, &cue.text[..]),
            (500_000, 1_500_000, "lines")
        );
        // The empty sample, the one an export writes between two cues and past
        // the last of them...
        assert!(tx3g_cue(&sample(vec![0, 0])).is_none());
        // ...and the bytes that are not a sample: a length past the payload,
        // and no length at all.
        assert!(tx3g_cue(&sample(vec![0, 9, b'h', b'i'])).is_none());
        assert!(tx3g_cue(&sample(Vec::new())).is_none());
    }

    /// An ASS script's text is UTF-8 whether or not it starts with a BOM: the
    /// parser this reads it with would read each byte as Latin-1 otherwise.
    /// `\h` is still a hard space, and nothing else is touched.
    #[test]
    fn ass_text_is_read_as_utf8_with_or_without_a_bom() {
        let dir = crate::scratch::Scratch::dir("subtitle_ass_utf8");
        let body = "[Script Info]\nScriptType: v4.00+\n\n[Events]\n\
            Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
            Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,{\\b1}Kalın{\\b0} ve ş Ğ 🎵\n\
            Dialogue: 0,0:00:02.00,0:00:04.00,Default,,0,0,0,,Alt\\hkesme\n";
        for (name, bytes) in [
            ("plain.ass", body.as_bytes().to_vec()),
            ("bom.ass", [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, bytes).expect("write");
            let cues = parse_file(&path).expect("parse");
            let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(texts, ["Kalın ve ş Ğ 🎵", "Alt\u{a0}kesme"], "{name}");
        }
    }

    /// A cue the parser would silently drop or keep backwards is named by its
    /// line, kept out of the track, and the cues around it are read as before.
    #[test]
    fn a_bad_timing_line_is_skipped_and_named_with_its_line() {
        let dir = crate::scratch::Scratch::dir("subtitle_bad_timing");
        let path = dir.join("bad.srt");
        let body = "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n\
            2\n00:00:03,000 --> 00:00:04,000\nTwo\n\n\
            3\n00:00:09,000 --> 00:00:0x,500\nBad digit\n\n\
            4\n00:00:08,500 --> 00:00:08,000\nReversed\n\n\
            5\n00:00:06,000 --> 00:00:07,000\nFive\n";
        std::fs::write(&path, body).expect("write");
        let (cues, skipped) = read_file(&path).expect("read");
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["One", "Two", "Five"]);
        assert_eq!(
            skipped,
            [
                "line 10: end time `00:00:0x,500` is not a timestamp",
                "line 14: end before start",
            ]
        );
        assert_eq!(skipped_cues(&path), skipped);
    }

    /// A timing line with a `->` for its arrow is named by its line, and a cue
    /// block with no timing line at all is named by its first line. Neither
    /// adds a cue, and the cues around them are read as before.
    #[test]
    fn a_wrong_arrow_and_a_cue_without_timing_are_named_by_line() {
        let dir = crate::scratch::Scratch::dir("subtitle_arrow_no_timing");
        let path = dir.join("malformed.srt");
        let body = "1\n00:00:01,000 --> 00:00:02,000\nOne\n\n\
            2\n00:00:02,000 -> 00:00:03,000\nArrow\n\n\
            3\n00:00:04,000 --> 00:00:05,000\nThree\n\n\
            4\nNo timing here\nstill text\n\n\
            5\n00:00:06,000 --> 00:00:07,000\nFive\n";
        std::fs::write(&path, body).expect("write");
        let (cues, skipped) = read_file(&path).expect("read");
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["One", "Three", "Five"]);
        assert_eq!(
            skipped,
            [
                "line 6: timing arrow must be -->",
                "line 13: cue has no timing line",
            ]
        );
        assert_eq!(skipped_cues(&path), skipped);

        let vtt = dir.join("malformed.vtt");
        let body = "WEBVTT\n\nNOTE a comment\nmore\n\nintro\n00:00.000 --> 00:01.000\nHi\n\n\
            lost\nno timing\n\nSTYLE\n::cue { color: red }\n";
        std::fs::write(&vtt, body).expect("write");
        let (cues, skipped) = read_file(&vtt).expect("read");
        assert_eq!(cues.len(), 1);
        assert_eq!(skipped, ["line 10: cue has no timing line"]);
    }

    /// A file that reads whole names nothing: a cue text may hold a number or
    /// a NOTE line of its own, and a WebVTT header, NOTE, STYLE and REGION
    /// block is not a cue.
    #[test]
    fn a_valid_file_names_nothing() {
        let dir = crate::scratch::Scratch::dir("subtitle_valid_names_nothing");
        let srt = dir.join("valid.srt");
        let body = "1\n00:00:01,000 --> 00:00:02,000\nIn the year\n2024\n\n\
            2\n00:00:03,000 --> 00:00:04,000\nSecond\n";
        std::fs::write(&srt, body).expect("write");
        let (cues, skipped) = read_file(&srt).expect("read");
        assert_eq!(cues.len(), 2);
        assert!(skipped.is_empty(), "{skipped:?}");

        let vtt = dir.join("valid.vtt");
        let body = "WEBVTT\n\nNOTE a comment\nmore\n\nREGION\nid:r1\n\n\
            intro\n00:00.000 --> 00:01.000 align:start\nHi\n\n\
            00:01.000 --> 00:02.000\nNOTE is text here\n";
        std::fs::write(&vtt, body).expect("write");
        let (cues, skipped) = read_file(&vtt).expect("read");
        assert_eq!(cues.len(), 2);
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// `<br>` is a line break in a WebVTT cue, in any of its spellings, and the
    /// other tags still go. A break at a line's end is one more than the
    /// line's own, as a browser shows it, and it does not end the cue.
    #[test]
    fn a_br_tag_is_a_line_break_in_a_cue() {
        let dir = crate::scratch::Scratch::dir("subtitle_br_tag");
        let path = dir.join("breaks.vtt");
        let body = "WEBVTT\n\n00:00.000 --> 00:01.500\nTwo lines<br>second <i>it</i>\n\n\
            00:01.500 --> 00:03.000\nthree<br/>four<BR />five<br>\nsix\n\n\
            00:03.000 --> 00:04.000\nPlain\n";
        std::fs::write(&path, body).expect("write");
        let texts: Vec<String> = read_file(&path).expect("read").0.into_iter().map(|c| c.text).collect();
        assert_eq!(texts, ["Two lines\nsecond it", "three\nfour\nfive\n\nsix", "Plain"]);
        // An SRT cue has the same break.
        let srt = dir.join("breaks.srt");
        std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:01,000\nA<br>B\n").expect("write");
        assert_eq!(read_file(&srt).expect("read").0[0].text, "A\nB");
    }

    /// The timestamps a subtitle file writes: hours optional, `.` or `,` before
    /// up to three fraction digits, and minutes and seconds below sixty.
    #[test]
    fn subtitle_clocks_read_the_forms_files_write() {
        assert_eq!(clock_us("00:01.500"), Some(1_500_000));
        assert_eq!(clock_us("1:02:03,5"), Some(3_723_500_000));
        assert_eq!(clock_us("00:00:00,000"), Some(0));
        assert_eq!(clock_us("00:60:00,000"), None);
        assert_eq!(clock_us("00:00:0x,500"), None);
        assert_eq!(clock_us("00:00:01"), None);
        assert_eq!(clock_us("00:00:01,"), None);
    }

    /// What a row shows, and what an export has to split back apart: the pair a
    /// container states, flattened the one way both readers flatten it.
    #[test]
    fn a_label_is_the_language_the_title_or_the_two() {
        assert_eq!(label_of("eng", ""), "eng");
        assert_eq!(label_of("fra", "Signs"), "fra — Signs");
        // An undetermined language is no language: the title stands alone.
        assert_eq!(label_of("und", "Commentary"), "Commentary");
        assert_eq!(label_of("und", ""), "und");
    }

    /// The language a file name names, from its last token that is one: the
    /// two-letter and three-letter codes, and a name with none names nothing.
    #[test]
    fn a_file_name_names_its_language_or_nothing() {
        for (name, code) in [
            ("talk.fr.srt", "fra"),
            ("talk.fra.srt", "fra"),
            ("fra.srt", "fra"),
            ("talk_en.srt", "eng"),
            ("eng.srt", "eng"),
            ("talk.de.forced.srt", "deu"),
            ("talk_fre.srt", "fra"),
            ("/media/talk.en-US.srt", "eng"),
        ] {
            assert_eq!(guess_language(Path::new(name)), Some(code), "{name}");
        }
        for name in ["talk.srt", "the_end.srt", "talk_is.srt", "talk.no.srt"] {
            assert_eq!(guess_language(Path::new(name)), None, "{name} names no language");
        }
    }

    /// What a person may type for a language, and the name an export titles it by.
    #[test]
    fn a_typed_language_is_an_iso_639_2_code() {
        assert_eq!(language_code("fr").as_deref(), Some("fra"));
        assert_eq!(language_code(" French ").as_deref(), Some("fra"));
        assert_eq!(language_code("ENG").as_deref(), Some("eng"));
        assert_eq!(language_code("fre").as_deref(), Some("fra"));
        assert_eq!(language_code("und").as_deref(), Some("und"));
        assert_eq!(language_code("zz9"), None);
        assert_eq!(language_code("x"), None);
        assert_eq!(language_name("fra"), Some("French"));
        assert_eq!(language_name("und"), None);
    }

    /// A standalone file is untitled: its name is its label, not its title, and a
    /// retype names its language. An embedded track's label follows its language.
    #[test]
    fn an_import_takes_the_given_language_or_the_file_names() {
        let path = Path::new("/media/talk.fr.srt");
        assert_eq!(import_language(path, None), Ok(Some(("fra".to_string(), "from the file name"))));
        assert_eq!(import_language(path, Some("eng")), Ok(Some(("eng".to_string(), "as given"))));
        assert_eq!(import_language(Path::new("/media/talk.srt"), None), Ok(None));
        assert!(import_language(path, Some("klingon")).unwrap_err().contains("not a language"));
    }

    #[test]
    fn a_retype_relabels_an_embedded_track_and_never_titles_a_file() {
        let mut file = external(Path::new("/nonexistent/subs.srt"));
        assert_eq!(file.name, "");
        assert_eq!(file.label, "subs.srt");
        set_language(&mut file, "fra");
        assert_eq!((file.language.as_str(), file.label.as_str()), ("fra", "subs.srt"));

        let mut embedded = SubtitleTrack {
            path: PathBuf::from("/media/a.mkv"),
            track: Some(3),
            language: "eng".into(),
            name: "Signs".into(),
            label: "eng — Signs".into(),
            cues: Vec::new(),
            bitmap: false,
            refused: None,
        };
        set_language(&mut embedded, "fra");
        assert_eq!(embedded.label, "fra — Signs");
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

    fn cue(start_ms: i64, end_ms: i64, text: &str) -> Cue {
        Cue {
            start_us: start_ms * 1000,
            end_us: end_ms * 1000,
            text: text.to_owned(),
            image: None,
            words: Vec::new(),
            karaoke: Vec::new(),
        }
    }

    #[test]
    fn srt_is_numbered_timed_and_sorted() {
        let cues = [cue(2_000, 3_500, "second"), cue(0, 1_250, "first\nlined")];
        assert_eq!(
            write_srt(&cues),
            "1\n00:00:00,000 --> 00:00:01,250\nfirst\nlined\n\n\
             2\n00:00:02,000 --> 00:00:03,500\nsecond\n\n"
        );
    }

    #[test]
    fn srt_roundtrips_through_the_parser() {
        let dir = crate::scratch::Scratch::dir("subtitle_write_srt");
        let path = dir.join("out.srt");
        let cues = [
            cue(500, 2_250, "hello there"),
            cue(3_661_004, 3_662_000, "an hour, and a line\nunder it"),
        ];
        save_cues(&path, &cues).expect("save");
        let back = parse_file(&path).expect("parse");
        assert_eq!(back.len(), 2);
        for (got, want) in back.iter().zip(&cues) {
            assert_eq!(got.start_us / 1000, want.start_us / 1000);
            assert_eq!(got.end_us / 1000, want.end_us / 1000);
            assert_eq!(got.text, want.text);
        }
        assert!(!dir.join("out.srt.part").exists());
    }

    #[test]
    fn vtt_has_its_header_and_dotted_times() {
        let out = write_vtt(&[cue(1_000, 2_000, "hi")]);
        assert_eq!(out, "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nhi\n\n");
        assert_eq!(write_vtt(&[]), "WEBVTT\n\n");
    }

    #[test]
    fn vtt_escapes_what_it_must_and_roundtrips() {
        let text = "tom & jerry <i>x</i> --> done";
        let out = write_vtt(&[cue(0, 1_000, text)]);
        assert!(
            out.contains("tom &amp; jerry &lt;i&gt;x&lt;/i&gt; --&gt; done\n"),
            "{out}"
        );
        // Only the timing line carries `-->`.
        assert_eq!(out.matches("-->").count(), 1, "{out}");
        // The file still parses as one cue with its timing, and the text reads
        // back as it was written: `&`, `<`, `>` and `-->` included.
        let dir = crate::scratch::Scratch::dir("subtitle_write_vtt");
        let path = dir.join("out.vtt");
        save_cues(&path, &[cue(0, 1_000, text)]).expect("save");
        let back = parse_file(&path).expect("parse");
        assert_eq!(back.len(), 1);
        assert_eq!((back[0].start_us, back[0].end_us), (0, 1_000_000));
        assert_eq!(back[0].text, text);

        // One pass only: `&amp;lt;` is the text `&lt;`, and a reference the
        // decoder does not know is kept as written.
        let raw = dir.join("raw.vtt");
        std::fs::write(
            &raw,
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n\
             A &amp;lt; B &#x3C; C &#62; D&nbsp;E &bogus; F\n\n",
        )
        .expect("write");
        let back = parse_file(&raw).expect("parse");
        assert_eq!(back[0].text, "A &lt; B < C > D\u{a0}E &bogus; F");
    }

    #[test]
    fn writers_drop_pictures_and_empty_or_backwards_cues() {
        let picture = Cue {
            start_us: 0,
            end_us: 1_000_000,
            text: String::new(),
            image: Some(Arc::new(CueImage {
                width: 1,
                height: 1,
                set: Source::Pgs(Vec::new()),
            })),
            words: Vec::new(),
            karaoke: Vec::new(),
        };
        let cues = [
            picture,
            cue(0, 1_000, "   "),
            cue(5_000, 4_000, "backwards"),
            cue(6_000, 6_000, "instant"),
            cue(-500, 1_000, "clamped"),
        ];
        assert_eq!(
            write_srt(&cues),
            "1\n00:00:00,000 --> 00:00:01,000\nclamped\n\n"
        );
    }

    #[test]
    fn save_refuses_other_extensions_and_leaves_nothing() {
        let dir = crate::scratch::Scratch::dir("subtitle_write_refuse");
        let path = dir.join("out.txt");
        assert!(save_cues(&path, &[cue(0, 1_000, "x")]).is_err());
        assert!(!path.exists());
        assert!(!dir.join("out.txt.part").exists());
    }

    fn word(start_ms: i64, end_ms: i64, w: &str) -> (i64, i64, String) {
        (start_ms * 1000, end_ms * 1000, w.to_owned())
    }

    #[test]
    fn words_split_on_a_pause() {
        let words = [
            word(0, 300, "one"),
            word(350, 600, "two"),
            word(2_000, 2_300, "three"),
        ];
        let cues = cues_from_words(&words, 42, 10.0, 0.5);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["one two", "three"]);
        assert_eq!((cues[1].start_us, cues[1].end_us), (2_000_000, 2_300_000));
    }

    #[test]
    fn words_split_on_length_and_on_duration() {
        // Four words of 4 letters: 19 characters joined. With max_chars 9 the
        // cue holds at most 2 * 9 = 18, so the fourth word starts a new cue; the
        // first three are 14 characters, over one line, so they split 2 + 1.
        let words = [
            word(0, 100, "aaaa"),
            word(100, 200, "bbbb"),
            word(200, 300, "cccc"),
            word(300, 400, "dddd"),
        ];
        let cues = cues_from_words(&words, 9, 10.0, 5.0);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["aaaa bbbb\ncccc", "dddd"]);
        // Duration: a cue may run 1 s from its first word's start.
        let timed = [
            word(0, 400, "a"),
            word(400, 800, "b"),
            word(800, 1_200, "c"),
        ];
        let cues = cues_from_words(&timed, 100, 1.0, 5.0);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "a b");
        assert_eq!(cues[1].text, "c");
    }

    #[test]
    fn words_split_on_sentence_end_and_lay_out_two_lines() {
        let words = [
            word(0, 100, "Hello"),
            word(100, 200, "there."),
            word(200, 300, "\"Again?\""),
            word(300, 400, "Yes"),
        ];
        let cues = cues_from_words(&words, 42, 10.0, 5.0);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["Hello there.", "\"Again?\"", "Yes"]);

        // Over one line's worth: split near the middle, at a word boundary.
        let long = [
            word(0, 100, "alpha"),
            word(100, 200, "bravo"),
            word(200, 300, "charlie"),
            word(300, 400, "delta"),
        ];
        let cues = cues_from_words(&long, 13, 10.0, 5.0);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "alpha bravo\ncharlie delta");
    }
}
