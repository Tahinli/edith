//! The project file: a line of text per thing, and nothing else.
//!
//! ```text
//! edith 8
//! playhead 90
//! resolution 1920 1080
//! source 0 test_av.mp4
//! source 1 /elsewhere/test_av2.mp4
//! eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk
//! color 0.1:1.2:0.9:-0.3
//! video 1 0 0 120 0 0 0 0 fit 1000
//! audio 1 0 0 120 0 0 0 - fit 1000
//! video 2 120 0 120 1 - - - fill 2000
//! audio 2
//! ```
//!
//! The `resolution` line is the **project's** picture size, which is a
//! different thing from any source's: media of other sizes are placed on it.
//! It belongs once, beside the playhead, and a file without one (every dialect
//! before v7) means "source 0's own picture", which is what such a project
//! always was.
//!
//! A source line is `source <audio stream> <path>`: which of the file's audio
//! tracks that source plays, in the file order
//! [`crate::AudioSession::probe_streams`] numbers, and then the path -- the
//! stream first because a path runs to the end of the line (it may hold
//! spaces) and an optional *trailing* field could not be told from one.
//!
//! An eq line is `eq <band>...`, one band per field, each
//! `<frequency>:<gain dB>:<Q>:<shape>` with the shape spelled `ls`, `pk` or
//! `hs` ([`crate::eq::BandKind`]). Like a source it is named by *position* --
//! the first eq line is eq 0 -- and a clip names one, so twenty clips sharing a
//! curve write it once. `eq` on its own is the empty cascade, which is a
//! setting like any other. The numbers are printed to round trip bit-exactly;
//! anything not finite is refused, here and at [`crate::Project::set_eq`].
//!
//! A colour line is `color <brightness>:<contrast>:<saturation>:<tint>`
//! ([`crate::color::ColorParams`]), named by position exactly as an eq line is
//! -- the first colour line is color 0 -- printed and refused by the same rules.
//!
//! A lane line is `<kind> <lane> <start> <in> <out> <source> <link> <eq>
//! <color> <fit> <speed>`: which lane the clip is on -- its kind and its 1-based number
//! among the lanes of that kind, the [`crate::project::Lane::label`] a header
//! column shows -- then where the clip sits on the timeline, the half-open
//! source range it plays, the file it plays from, its group id, the eq line it
//! plays through, the colour line it is graded by (`-` for none of those three)
//! and how it meets a project canvas of another shape, spelled `fit`, `fill`,
//! `stretch` or `center` ([`crate::scale::FitPolicy`]), and how fast it plays,
//! in thousandths of real time ([`crate::project::Speed`], `1000` for a clip
//! nobody has speeded). Timeline placement is
//! explicit, so a *gap* is simply a stretch no line covers -- there is nothing
//! to write for one, and nothing that can disagree about its length. The `<in>`
//! and `<out>` are *source* frames whatever the speed says: how long the clip is
//! on the timeline is that range divided by the rate, and is written nowhere.
//!
//! A lane is declared by the clips on it, in the order the lanes are displayed
//! in; a lane holding *nothing* has a bare `<kind> <lane>` line instead, which
//! is the only way an empty lane could still be there on the way back. A lane
//! number may not skip one of its kind.
//!
//! **Version 7** was this without the clip's speed field -- every clip of such
//! a project plays at real time, which is the only rate there was.
//! **Version 6** was that without the `resolution` line and without the clip's
//! fit field -- such a project is the size of its first source and letterboxes
//! nothing, because nothing on it could differ in size. **Version 5** was that
//! without the colour lines and without the clip's colour
//! field. **Version 4** was that without the eq lines and without the clip's eq
//! field. **Version 3** was that without the lane number, and held one video
//! lane and one audio lane, no more. **Version 2** wrote `source <path>` as
//! well, which is this file's stream 0 -- one per file, whichever audio track
//! came first. **Version 1** wrote that and one lane, queued end to end: `clip
//! <in> <out> <source>`. All six still load -- a v1 file's clips are laid out
//! cumulatively and copied onto both lanes as one group each, which is exactly
//! what a v1 timeline meant, and an older file simply equalizes and grades
//! nothing, and an older one plays everything at real time -- and saving any of
//! them writes v8. An older reader refuses a newer
//! file by name.
//!
//! Text because an edit list is a few integers and a path, and a path is
//! *bytes* on this platform -- a JSON string would have to lossily decode one.
//! So the path field is byte-escaped (`%` -> `%25`, newline -> `%0A`, which is
//! everything a line-based format cannot carry) and survives round-trip
//! exactly. Paths under the project file's own directory are written relative
//! to it, so a folder holding the media and the `.edith` can be moved or
//! copied anywhere and still open; anything else is absolute.
//!
//! The parser is strict and every refusal names its 1-based line: a project
//! file is generated, so a line it did not generate is a corrupt file, not a
//! dialect. Structure is checked here (fields, ordering, empty clips,
//! out-of-range source indexes); whether the files on disk still match the
//! timeline is [`crate::PlaybackSession::open_project`]'s business.
//!
//! Writing goes through `<path>.part` and a rename, as an export does, so an
//! interrupted save cannot destroy the previous version of the project. The
//! bytes are fsynced before the rename and the directory after it, so what a
//! power loss can lose is a whole save, never half of one under the real name.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::color::ColorParams;
use crate::eq::{Band, BandKind, EqParams};
use crate::project::{Clip, Lane, LaneKind, Source, Speed};
use crate::scale::FitPolicy;

/// What [`save`] writes. Read support goes back to `edith 1`; see the module
/// docs for what those dialects looked like.
const MAGIC: &[u8] = b"edith 8";
const MAGIC_V7: &[u8] = b"edith 7";
const MAGIC_V6: &[u8] = b"edith 6";
const MAGIC_V5: &[u8] = b"edith 5";
const MAGIC_V4: &[u8] = b"edith 4";
const MAGIC_V3: &[u8] = b"edith 3";
const MAGIC_V2: &[u8] = b"edith 2";
const MAGIC_V1: &[u8] = b"edith 1";

/// What a project file says: an edit list plus where the playhead stood.
/// Structurally valid by construction -- see [`parse`].
#[derive(Debug)]
pub struct Document {
    /// Paths absolute, relative entries already joined to the file's own
    /// directory.
    pub sources: Vec<Source>,
    /// Every lane, in display order, which is the order
    /// [`crate::Project::from_parts`] takes them in. Exactly two -- `V1` then
    /// `A1` -- for every dialect before v4.
    pub lanes: Vec<(LaneKind, Vec<Clip>)>,
    /// The equalizer table [`Clip::eq`] indexes into, in file order. Empty for
    /// every dialect before v5.
    pub eq: Vec<EqParams>,
    /// The colour table [`Clip::color`] indexes into, in file order. Empty for
    /// every dialect before v6.
    pub color: Vec<ColorParams>,
    /// The project's own picture size. `None` for every dialect before v7 and
    /// for a v7 file that leaves it out, which both mean "source 0's picture".
    pub resolution: Option<(u32, u32)>,
    pub playhead: u32,
}

/// Writes the project to `path`, atomically. `sources`, `eq` and `color` should
/// already be orphan-free ([`crate::Project::without_orphan_sources`]).
pub fn save(
    path: &Path,
    sources: &[Source],
    lanes: &[(LaneKind, Vec<Clip>)],
    eq: &[EqParams],
    color: &[ColorParams],
    resolution: (u32, u32),
    playhead: u32,
) -> crate::Result<()> {
    let dir = project_dir(path);
    let mut part = path.to_path_buf().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    // The rename publishes the file under the caller's name in one step, on the
    // same directory; until it happens the old project file is still the whole
    // truth. `sync_all` puts the bytes on the disk before that name exists, and
    // the second one puts the name itself there.
    let result = std::fs::File::create(&part)
        .and_then(|mut f| {
            f.write_all(&emit(&dir, sources, lanes, eq, color, resolution, playhead))?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&part, path))
        .and_then(|()| std::fs::File::open(&dir)?.sync_all());
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map_err(Into::into)
}

pub fn load(path: &Path) -> crate::Result<Document> {
    let data = std::fs::read(path)?;
    parse(&data, path.parent().unwrap_or(Path::new("")))
}

/// The directory a project file's relative source lines are measured from, in
/// the same canonical form [`crate::Project`] keeps its sources in -- without
/// that the prefix simply never matches and every line comes out absolute,
/// which is what `cd dir && edith clip.mp4` + save used to write (the parent of
/// a bare filename is `""`). Canonicalizing fails only if the directory does
/// not exist, and then the write is about to fail anyway.
fn project_dir(path: &Path) -> PathBuf {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

fn emit(
    dir: &Path,
    sources: &[Source],
    lanes: &[(LaneKind, Vec<Clip>)],
    eq: &[EqParams],
    color: &[ColorParams],
    resolution: (u32, u32),
    playhead: u32,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(b'\n');
    out.extend_from_slice(format!("playhead {playhead}\n").as_bytes());
    out.extend_from_slice(format!("resolution {} {}\n", resolution.0, resolution.1).as_bytes());
    for s in sources {
        out.extend_from_slice(format!("source {} ", s.audio_stream).as_bytes());
        escape(s.path.strip_prefix(dir).unwrap_or(&s.path), &mut out);
        out.push(b'\n');
    }
    // Before the clips, for the reason the sources are: a clip names one by the
    // position of its line, so the line has to be there first.
    for params in eq {
        out.extend_from_slice(b"eq");
        for b in &params.bands {
            // `{:?}` rather than `{}`: it is the formatting that promises the
            // shortest string parsing back to this exact f32, and it never
            // prints a bare integer, which would read as another field's shape.
            let kind = match b.kind {
                BandKind::LowShelf => "ls",
                BandKind::Peak => "pk",
                BandKind::HighShelf => "hs",
            };
            out.extend_from_slice(
                format!(" {:?}:{:?}:{:?}:{kind}", b.freq_hz, b.gain_db, b.q).as_bytes(),
            );
        }
        out.push(b'\n');
    }
    // ...and the colour table, for the same reason and by the same rules.
    for p in color {
        out.extend_from_slice(
            format!(
                "color {:?}:{:?}:{:?}:{:?}\n",
                p.brightness, p.contrast, p.saturation, p.tint
            )
            .as_bytes(),
        );
    }
    // Lane by lane rather than interleaved by time: a lane reads as a list, the
    // parser gets its sortedness check for free from the order it is in, and
    // the lanes come back out in the order they are displayed in.
    let (mut video, mut audio) = (0, 0);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
        };
        // A lane is declared by its clips; one holding nothing has to say so on
        // a line of its own, or the lane itself would not survive the round trip.
        if clips.is_empty() {
            out.extend_from_slice(format!("{keyword} {ord}\n").as_bytes());
        }
        for c in clips {
            let link = c.link.map_or("-".to_string(), |l| l.to_string());
            let eq = c.eq.map_or("-".to_string(), |e| e.to_string());
            let color = c.color.map_or("-".to_string(), |e| e.to_string());
            out.extend_from_slice(
                format!(
                    "{keyword} {ord} {} {} {} {} {link} {eq} {color} {} {}\n",
                    c.start,
                    c.in_frame,
                    c.out_frame,
                    c.source,
                    fit_name(c.fit),
                    c.speed.permille()
                )
                .as_bytes(),
            );
        }
    }
    out
}

fn parse(data: &[u8], dir: &Path) -> crate::Result<Document> {
    // One trailing newline is the line terminator of the last line, not an
    // empty line -- any further one is, and is refused below.
    let body = data.strip_suffix(b"\n").unwrap_or(data);
    let mut lines = body.split(|&b| b == b'\n').enumerate();
    let (_, first) = lines.next().unwrap_or((0, &[]));
    let v1 = first == MAGIC_V1;
    // The dialects that wrote a source line without its stream field. Reading
    // one is the whole of what "an old project still opens" means here.
    let streamless = v1 || first == MAGIC_V2;
    // The one that carries a per-clip speed...
    let v8 = first == MAGIC;
    // ...the ones that carry a project resolution and per-clip fit policies...
    let v7 = v8 || first == MAGIC_V7;
    // ...the ones that carry colour grades...
    let v6 = v7 || first == MAGIC_V6;
    // ...the ones that carry equalizer settings...
    let v5 = v6 || first == MAGIC_V5;
    // ...and the ones that number their lanes. Every older one held two.
    let numbered = v5 || first == MAGIC_V4;
    if !numbered && !streamless && first != MAGIC_V3 {
        return Err(match first.strip_prefix(b"edith ") {
            Some(v) => format!("line 1: unsupported version {}", String::from_utf8_lossy(v)),
            None => "line 1: not a edith file".to_string(),
        }
        .into());
    }

    let mut doc = Document {
        sources: Vec::new(),
        // v4 on declares its lanes as they come; every dialect before it held
        // exactly `V1` and `A1`, either of which could be empty.
        lanes: if numbered {
            Vec::new()
        } else {
            vec![(LaneKind::Video, Vec::new()), (LaneKind::Audio, Vec::new())]
        },
        // Nothing before v5 equalizes anything, and nothing before v6 grades.
        eq: Vec::new(),
        color: Vec::new(),
        // Nothing before v7 has a resolution of its own: source 0's picture is
        // what those projects were, and `None` is how the loader is told so.
        resolution: None,
        playhead: 0,
    };
    let mut playhead_seen = false;
    // Where the next v1 clip is placed: that dialect had no `start` field, the
    // clips simply queued up.
    let mut queued = 0u32;
    for (i, line) in lines {
        let n = i + 1;
        let (keyword, rest) = match line.iter().position(|&b| b == b' ') {
            Some(at) => (&line[..at], &line[at + 1..]),
            None => (line, &line[line.len()..]),
        };
        match keyword {
            b"playhead" => {
                if playhead_seen || !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: playhead belongs once, before the sources").into(),
                    );
                }
                doc.playhead = number(rest, n)?;
                playhead_seen = true;
            }
            b"resolution" if v7 => {
                if doc.resolution.is_some() || !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: resolution belongs once, before the sources").into(),
                    );
                }
                let f = fields(rest, 2, "resolution", n)?;
                let (width, height) = (number(f[0], n)?, number(f[1], n)?);
                // The same bound the keystroke has (`crate::is_resolution`):
                // unbounded here, this line reached `open_black` and panicked
                // the open with a capacity overflow.
                if !crate::is_resolution(width, height) {
                    return Err(format!("line {n}: {width}x{height} is not a picture").into());
                }
                doc.resolution = Some((width, height));
            }
            b"source" => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: source after a clip").into());
                }
                // The stream comes first and the path is everything after it,
                // spaces and all; an older dialect wrote no stream and meant 0.
                let (audio_stream, rest) = if streamless {
                    (0, rest)
                } else {
                    // No space at all still has to reach `number`, so a v2 line
                    // in a v3 file is refused as the missing field it is rather
                    // than as a missing path.
                    let at = rest.iter().position(|&b| b == b' ');
                    (
                        number(&rest[..at.unwrap_or(rest.len())], n)? as usize,
                        &rest[at.map_or(rest.len(), |at| at + 1)..],
                    )
                };
                if rest.is_empty() {
                    return Err(format!("line {n}: source without a path").into());
                }
                let path = unescape(rest, n)?;
                // Relative means "next to the project file", which is what
                // makes a whole folder relocatable.
                doc.sources.push(Source {
                    path: if path.is_absolute() {
                        path
                    } else {
                        dir.join(path)
                    },
                    audio_stream,
                });
            }
            b"eq" if v5 => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: eq after a clip").into());
                }
                // A bare `eq` is the empty cascade -- a setting that moves
                // nothing, which is not the same thing as no setting at all.
                let bands = match rest.is_empty() {
                    true => Vec::new(),
                    false => rest
                        .split(|&b| b == b' ')
                        .map(|field| band(field, n))
                        .collect::<crate::Result<Vec<Band>>>()?,
                };
                doc.eq.push(EqParams { bands });
            }
            b"color" if v6 => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: color after a clip").into());
                }
                let f = fields(rest, 1, "color", n)?;
                let parts: Vec<&[u8]> = f[0].split(|&b| b == b':').collect();
                if parts.len() != 4 {
                    return Err(
                        format!("line {n}: color wants 4 fields, found {}", parts.len()).into(),
                    );
                }
                doc.color.push(ColorParams {
                    brightness: float(parts[0], n)?,
                    contrast: float(parts[1], n)?,
                    saturation: float(parts[2], n)?,
                    tint: float(parts[3], n)?,
                });
            }
            // v1: one lane, no placement, no groups. Every clip becomes one
            // grouped video+audio pair laid where the queue reached, which is
            // what the file always meant.
            b"clip" if v1 => {
                let f = fields(rest, 3, "clip", n)?;
                let clip = check(
                    Clip {
                        start: queued,
                        in_frame: number(f[0], n)?,
                        out_frame: number(f[1], n)?,
                        source: number(f[2], n)? as usize,
                        link: Some(doc.lanes[0].1.len() as u32),
                        eq: None,
                        color: None,
                        fit: FitPolicy::default(),
                        speed: Speed::NORMAL,
                    },
                    &doc,
                    n,
                )?;
                queued = clip.end();
                doc.lanes[0].1.push(clip);
                doc.lanes[1].1.push(clip);
            }
            b"video" | b"audio" if !v1 => {
                let kind = match keyword {
                    b"video" => LaneKind::Video,
                    _ => LaneKind::Audio,
                };
                // v4 on names the lane, and a line naming nothing else is an
                // empty lane's whole existence. Older dialects had one lane per
                // kind.
                let (ord, rest) = if numbered {
                    let at = rest.iter().position(|&b| b == b' ');
                    (
                        number(&rest[..at.unwrap_or(rest.len())], n)?,
                        &rest[at.map_or(rest.len(), |at| at + 1)..],
                    )
                } else {
                    (1, rest)
                };
                let at = lane_of(&mut doc.lanes, kind, ord, n)?;
                if numbered && rest.is_empty() {
                    continue;
                }
                // v8 grew the speed field, v7 the fit one, v6 the colour one and
                // v5 the eq one; every older dialect ends at the link.
                let want =
                    5 + usize::from(v5) + usize::from(v6) + usize::from(v7) + usize::from(v8);
                let f = fields(rest, want, "clip", n)?;
                let clip = check(
                    Clip {
                        start: number(f[0], n)?,
                        in_frame: number(f[1], n)?,
                        out_frame: number(f[2], n)?,
                        source: number(f[3], n)? as usize,
                        link: match f[4] {
                            b"-" => None,
                            field => Some(number(field, n)?),
                        },
                        eq: table_index(f.get(5).copied(), doc.eq.len(), "eq", n)?,
                        color: table_index(f.get(6).copied(), doc.color.len(), "color", n)?,
                        fit: fit_policy(f.get(7).copied(), n)?,
                        speed: speed_of(f.get(8).copied(), n)?,
                    },
                    &doc,
                    n,
                )?;
                let lane = &mut doc.lanes[at].1;
                // The sorted, non-overlapping placement invariant, checked at
                // the only door untrusted clips come in through.
                if lane.last().is_some_and(|prev| prev.end() > clip.start) {
                    return Err(format!(
                        "line {n}: clip at {} overlaps the one before it, or comes before it",
                        clip.start
                    )
                    .into());
                }
                lane.push(clip);
            }
            // Empty lines land here too: nothing in this format is optional
            // whitespace. A lane keyword in a v1 file (or `clip` in a v2 one)
            // is a file mixing dialects, and is refused the same way.
            _ => {
                return Err(format!(
                    "line {n}: unknown keyword {:?}",
                    String::from_utf8_lossy(keyword)
                )
                .into());
            }
        }
    }
    // A file whose lanes are all empty *is* a project -- the emptied timeline,
    // saved. What it may not be is laneless; [`crate::Project::from_parts`]
    // refuses that one, by the name it has there.
    Ok(doc)
}

/// Where the lane a v4 line names sits, appending it if this is its first line.
/// Lanes arrive in display order, so a number that skips one of its kind is a
/// lane that was never declared, and is refused by the name it would have had.
fn lane_of(
    lanes: &mut Vec<(LaneKind, Vec<Clip>)>,
    kind: LaneKind,
    ord: u32,
    line: usize,
) -> crate::Result<usize> {
    let mut seen = 0;
    for (at, (k, _)) in lanes.iter().enumerate() {
        if *k == kind {
            seen += 1;
            if seen == ord {
                return Ok(at);
            }
        }
    }
    if ord != seen + 1 {
        return Err(match ord.checked_sub(1) {
            Some(prev) => format!(
                "line {line}: lane {} comes before {}",
                Lane::new(kind, prev as usize).label(),
                Lane::new(kind, seen as usize).label()
            ),
            None => format!("line {line}: lane numbers start at 1"),
        }
        .into());
    }
    lanes.push((kind, Vec::new()));
    Ok(lanes.len() - 1)
}

/// Splits a line's fields, refusing the wrong count by name.
fn fields<'a>(
    rest: &'a [u8],
    want: usize,
    what: &str,
    line: usize,
) -> crate::Result<Vec<&'a [u8]>> {
    let fields: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
    if fields.len() != want {
        return Err(format!(
            "line {line}: {what} wants {want} fields, found {}",
            fields.len()
        )
        .into());
    }
    Ok(fields)
}

/// The per-clip structure checks both dialects share.
fn check(clip: Clip, doc: &Document, line: usize) -> crate::Result<Clip> {
    if clip.out_frame <= clip.in_frame {
        return Err(format!(
            "line {line}: clip [{}, {}) is empty",
            clip.in_frame, clip.out_frame
        )
        .into());
    }
    if clip.source >= doc.sources.len() {
        return Err(format!(
            "line {line}: clip names source {} of {}",
            clip.source,
            doc.sources.len()
        )
        .into());
    }
    // Everything downstream -- `Clip::end`, the overlap check, the v1 queue --
    // adds `start + len` as plain `u32`, which panics in debug and wraps in
    // release on a crafted file. Bound it here, at the door.
    if clip.start.checked_add(clip.frames()).is_none() {
        return Err(format!(
            "line {line}: clip at {} runs past the last frame there is",
            clip.start
        )
        .into());
    }
    Ok(clip)
}

/// A clip's `<speed>` field: thousandths of real time, as
/// [`crate::project::Speed`] holds it -- `1000` is the rate it was shot at. A
/// dialect with no such field (everything before v8) means real time, which is
/// what such a project always played at. A number outside the range the editor
/// can set is *refused* rather than clamped: a project file is generated, so
/// that is a corrupt line and not a dialect.
fn speed_of(field: Option<&[u8]>, line: usize) -> crate::Result<Speed> {
    let Some(field) = field else {
        return Ok(Speed::NORMAL);
    };
    let permille = number(field, line)?;
    let speed = Speed::from_permille(permille.min(u32::from(u16::MAX)) as u16);
    if u32::from(speed.permille()) != permille {
        return Err(format!(
            "line {line}: speed {permille} is outside {}-{} thousandths",
            Speed::MIN.permille(),
            Speed::MAX.permille()
        )
        .into());
    }
    Ok(speed)
}

/// How a fit policy is spelled in the file. One word each, so a clip line stays
/// readable, and the same word the parser takes back.
fn fit_name(fit: FitPolicy) -> &'static str {
    match fit {
        FitPolicy::Fit => "fit",
        FitPolicy::Fill => "fill",
        FitPolicy::Stretch => "stretch",
        FitPolicy::Center => "center",
    }
}

/// A clip's `<fit>` field. A dialect that has no such field at all means the
/// default -- those projects held one resolution, so no clip of theirs was ever
/// placed on anything.
fn fit_policy(field: Option<&[u8]>, line: usize) -> crate::Result<FitPolicy> {
    let Some(field) = field else {
        return Ok(FitPolicy::default());
    };
    match field {
        b"fit" => Ok(FitPolicy::Fit),
        b"fill" => Ok(FitPolicy::Fill),
        b"stretch" => Ok(FitPolicy::Stretch),
        b"center" => Ok(FitPolicy::Center),
        other => Err(format!(
            "line {line}: {:?} is not a fit policy",
            String::from_utf8_lossy(other)
        )
        .into()),
    }
}

/// A clip's `<eq>` or `<color>` field: `-` (or a dialect that has no such field
/// at all), or a line of a table that is already declared. Refused by the name
/// of what it points at, so the message reads as the file does.
fn table_index(
    field: Option<&[u8]>,
    len: usize,
    what: &str,
    line: usize,
) -> crate::Result<Option<u16>> {
    let field = match field {
        None | Some(b"-") => return Ok(None),
        Some(field) => field,
    };
    let i = number(field, line)?;
    if i as usize >= len || i > u32::from(u16::MAX) {
        return Err(format!("line {line}: clip names {what} {i} of {len}").into());
    }
    Ok(Some(i as u16))
}

/// One `<frequency>:<gain>:<Q>:<shape>` field of an eq line.
fn band(field: &[u8], line: usize) -> crate::Result<Band> {
    let parts: Vec<&[u8]> = field.split(|&b| b == b':').collect();
    if parts.len() != 4 {
        return Err(format!("line {line}: band wants 4 fields, found {}", parts.len()).into());
    }
    Ok(Band {
        freq_hz: float(parts[0], line)?,
        gain_db: float(parts[1], line)?,
        q: float(parts[2], line)?,
        kind: match parts[3] {
            b"ls" => BandKind::LowShelf,
            b"pk" => BandKind::Peak,
            b"hs" => BandKind::HighShelf,
            other => {
                return Err(format!(
                    "line {line}: {:?} is not a band shape",
                    String::from_utf8_lossy(other)
                )
                .into());
            }
        },
    })
}

/// A band's number: whatever `{:?}` wrote, read back to the same bits. Infinity
/// and NaN are refused rather than carried -- they are the values a coefficient
/// cannot be built from, and the ones that would not compare equal to
/// themselves on the way back.
fn float(field: &[u8], line: usize) -> crate::Result<f32> {
    match std::str::from_utf8(field)
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|f| f.is_finite())
    {
        Some(f) => Ok(f),
        None => Err(format!(
            "line {line}: {:?} is not a number",
            String::from_utf8_lossy(field)
        )
        .into()),
    }
}

fn number(field: &[u8], line: usize) -> crate::Result<u32> {
    match std::str::from_utf8(field).ok().and_then(|s| s.parse().ok()) {
        Some(n) => Ok(n),
        None => Err(format!(
            "line {line}: {:?} is not a number",
            String::from_utf8_lossy(field)
        )
        .into()),
    }
}

/// The two bytes a line-based format cannot hold, and the escape byte itself.
fn escape(path: &Path, out: &mut Vec<u8>) {
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'%' => out.extend_from_slice(b"%25"),
            b'\n' => out.extend_from_slice(b"%0A"),
            _ => out.push(b),
        }
    }
}

/// Accepts any `%XX`, not just the two [`escape`] writes -- a hand-edited file
/// may spell a space or a quote that way, and there is no reason to refuse it.
fn unescape(field: &[u8], line: usize) -> crate::Result<PathBuf> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] != b'%' {
            out.push(field[i]);
            i += 1;
            continue;
        }
        let byte = field
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .ok_or_else(|| -> crate::Error {
                format!("line {line}: truncated % escape in the path").into()
            })?;
        out.push(byte);
        i += 3;
    }
    Ok(PathBuf::from(OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(start: u32, in_frame: u32, out_frame: u32, source: usize, link: Option<u32>) -> Clip {
        Clip {
            start,
            in_frame,
            out_frame,
            source,
            link,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        }
    }

    /// [`emit`] for a timeline that equalizes nothing, which is every case that
    /// predates the eq lines.
    fn flat(
        dir: &Path,
        sources: &[Source],
        lanes: &[(LaneKind, Vec<Clip>)],
        playhead: u32,
    ) -> Vec<u8> {
        emit(dir, sources, lanes, &[], &[], (1280, 720), playhead)
    }

    /// Two sources, one under the project's own directory and one not, a clip
    /// from each on the video lane, and an audio lane with a gap in the middle
    /// -- the shape every case below starts from.
    fn source(path: &str, audio_stream: usize) -> Source {
        Source {
            path: PathBuf::from(path),
            audio_stream,
        }
    }

    /// Lanes in display order, as [`Document::lanes`] holds them.
    type Lanes = Vec<(LaneKind, Vec<Clip>)>;

    /// `V1` then `A1`, the shape every dialect before v4 held.
    fn two(video: Vec<Clip>, audio: Vec<Clip>) -> Lanes {
        vec![(LaneKind::Video, video), (LaneKind::Audio, audio)]
    }

    fn doc() -> (PathBuf, Vec<Source>, Lanes) {
        (
            PathBuf::from("/proj"),
            vec![source("/proj/a.mp4", 0), source("/elsewhere/b.mp4", 2)],
            two(
                vec![clip(0, 0, 30, 0, Some(0)), clip(30, 10, 20, 1, Some(1))],
                vec![clip(0, 0, 30, 0, Some(0))],
            ),
        )
    }

    #[test]
    fn relative_and_absolute_paths_round_trip() {
        let (dir, sources, lanes) = doc();
        let bytes = flat(&dir, &sources, &lanes, 12);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000\nvideo 1 30 10 20 1 1 - - fit 1000\n\
             audio 1 0 0 30 0 0 - - fit 1000\n",
            "the file under the project directory is written relative to it, \
             each with the audio stream it plays"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.sources, sources, "relative entries rejoin the dir");
        assert_eq!(
            back.lanes, lanes,
            "both lanes, in display order; the trailing gap needs no line"
        );
        assert_eq!(back.playhead, 12);
        // ...and emitting the parsed document reproduces the same bytes.
        assert_eq!(flat(&dir, &back.sources, &back.lanes, back.playhead), bytes);
    }

    /// The whole of the bump: a project of any number of lanes writes and reads
    /// back, in display order, empty lanes and cross-lane groups included.
    #[test]
    fn any_number_of_lanes_round_trips() {
        let dir = PathBuf::from("/proj");
        let sources = vec![source("/proj/a.mp4", 0)];
        // V1, A1, V2, A2 in *display* order -- the interleaving a front-end
        // shows, not video-then-audio -- with one take grouped across V1 and A2
        // (no paired ord, the lanes between them empty of it) and an A1 holding
        // nothing at all.
        let lanes = vec![
            (LaneKind::Video, vec![clip(0, 0, 30, 0, Some(4))]),
            (LaneKind::Audio, Vec::new()),
            (LaneKind::Video, vec![clip(40, 0, 10, 0, None)]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0, Some(4))]),
        ];
        let bytes = flat(&dir, &sources, &lanes, 7);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 7\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 4 - - fit 1000\naudio 1\n\
             video 2 40 0 10 0 - - - fit 1000\naudio 2 0 0 30 0 4 - - fit 1000\n",
            "an empty lane is a line of its own; everything else is its clips"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "lane list, order and links all survive");
        assert_eq!(back.playhead, 7);
        assert_eq!(flat(&dir, &back.sources, &back.lanes, back.playhead), bytes);

        // A lane number may not skip one of its kind: that lane was declared by
        // nothing, and the refusal names it.
        for (file, want) in [
            (
                "edith 4\nsource 0 a.mp4\nvideo 2 0 0 30 0 -\n",
                "line 3: lane V2 comes before V1",
            ),
            (
                "edith 4\nsource 0 a.mp4\nvideo 1 0 0 30 0 -\naudio 3\n",
                "line 4: lane A3 comes before A1",
            ),
            (
                "edith 4\nsource 0 a.mp4\nvideo 0 0 0 30 0 -\n",
                "line 3: lane numbers start at 1",
            ),
            // The clip fields are still the clip fields, one lane number on.
            (
                "edith 4\nsource 0 a.mp4\nvideo 1 0 0 30 0\n",
                "line 3: clip wants 5 fields, found 4",
            ),
            // ...and a v4 line in a v3 file reads as the garbled clip it is.
            (
                "edith 3\nsource 0 a.mp4\nvideo 1 0 0 30 0 -\n",
                "line 3: clip wants 5 fields, found 6",
            ),
        ] {
            assert_eq!(parse(file.as_bytes(), &dir).unwrap_err().to_string(), want);
        }
        // Lanes and nothing on them is the emptied timeline, saved: a project.
        let empty = parse(b"edith 4\nsource 0 a.mp4\nvideo 1\naudio 1\n", &dir).expect("parse");
        assert!(empty.lanes.iter().all(|(_, clips)| clips.is_empty()));
        assert_eq!(empty.lanes.len(), 2, "and its lanes survive the round trip");
    }

    /// One band, spelled the way an eq line spells it.
    fn band_of(freq_hz: f32, gain_db: f32, q: f32, kind: BandKind) -> Band {
        Band {
            freq_hz,
            gain_db,
            q,
            kind,
        }
    }

    /// The whole of the v5 bump: the clips name a shared equalizer table, the
    /// bytes are the ones documented at the top of this file, and every f32 in
    /// them comes back *bit* for bit -- which is what makes a re-save of a
    /// loaded project produce the same file rather than a drifting one.
    #[test]
    fn equalizers_round_trip_bit_exactly_and_are_shared() {
        let dir = PathBuf::from("/proj");
        let sources = vec![source("/proj/a.mp4", 0)];
        let eq = vec![
            EqParams {
                bands: vec![
                    band_of(80.0, -3.0, 0.707, BandKind::LowShelf),
                    band_of(1000.0, 4.5, 1.0, BandKind::Peak),
                ],
            },
            // The awkward ones: a value needing all 24 mantissa bits, one whose
            // shortest spelling is exponential, a subnormal, and a negative
            // zero -- plus the empty cascade, which is a setting like any other.
            EqParams {
                bands: vec![band_of(
                    16_777_215.0,
                    -0.1,
                    f32::MIN_POSITIVE / 3.0,
                    BandKind::HighShelf,
                )],
            },
            EqParams { bands: Vec::new() },
        ];
        // Two clips share entry 0, so it is written once; one clip plays flat.
        let lanes = two(
            vec![
                Clip {
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    eq: Some(1),
                    ..clip(30, 10, 20, 0, None)
                },
            ],
            vec![
                Clip {
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    eq: Some(2),
                    ..clip(30, 0, 10, 0, None)
                },
            ],
        );
        let bytes = emit(&dir, &sources, &lanes, &eq, &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk\n\
             eq 16777215.0:-0.1:3.918315e-39:hs\n\
             eq\n\
             video 1 0 0 30 0 0 0 - fit 1000\nvideo 1 30 10 20 0 - 1 - fit 1000\n\
             audio 1 0 0 30 0 0 0 - fit 1000\naudio 1 30 0 10 0 - 2 - fit 1000\n",
            "the table comes before the clips, and a clip names a line of it"
        );

        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "every clip keeps the eq it named");
        for (i, (got, want)) in back.eq.iter().zip(&eq).enumerate() {
            for (b, (got, want)) in got.bands.iter().zip(&want.bands).enumerate() {
                assert_eq!(
                    (
                        got.freq_hz.to_bits(),
                        got.gain_db.to_bits(),
                        got.q.to_bits(),
                        got.kind
                    ),
                    (
                        want.freq_hz.to_bits(),
                        want.gain_db.to_bits(),
                        want.q.to_bits(),
                        want.kind
                    ),
                    "eq {i} band {b} came back as other bits"
                );
            }
        }
        assert_eq!(back.eq.len(), 3, "the empty cascade is an entry of its own");
        assert_eq!(
            emit(
                &dir,
                &back.sources,
                &back.lanes,
                &back.eq,
                &back.color,
                (1280, 720),
                0
            ),
            bytes
        );
    }

    /// The whole of the v6 bump, the eq test's twin: the clips name a shared
    /// colour table, the bytes are the ones documented at the top of this file,
    /// and every f32 comes back *bit* for bit, so a re-save is the same file.
    #[test]
    fn colours_round_trip_bit_exactly_and_are_shared() {
        let dir = PathBuf::from("/proj");
        let sources = vec![source("/proj/a.mp4", 0)];
        let color = vec![
            ColorParams {
                brightness: 0.1,
                contrast: 1.2,
                saturation: 0.9,
                tint: -0.3,
            },
            // The awkward ones again: all 24 mantissa bits, an exponential
            // spelling, a subnormal and a negative zero -- and the identity,
            // which is a setting like any other.
            ColorParams {
                brightness: -0.000_000_1,
                contrast: 16_777_215.0,
                saturation: f32::MIN_POSITIVE / 3.0,
                tint: -0.0,
            },
            ColorParams::default(),
        ];
        // Two clips share entry 0, so it is written once; one clip is ungraded.
        let lanes = two(
            vec![
                Clip {
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    color: Some(1),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(30, 10, 20, 0, None)
                },
            ],
            vec![
                Clip {
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    color: Some(2),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(30, 0, 10, 0, None)
                },
            ],
        );
        let bytes = emit(&dir, &sources, &lanes, &[], &color, (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             color 0.1:1.2:0.9:-0.3\n\
             color -1e-7:16777215.0:3.918315e-39:-0.0\n\
             color 0.0:1.0:1.0:0.0\n\
             video 1 0 0 30 0 0 - 0 fit 1000\nvideo 1 30 10 20 0 - - 1 fit 1000\n\
             audio 1 0 0 30 0 0 - 0 fit 1000\naudio 1 30 0 10 0 - - 2 fit 1000\n",
            "the table comes before the clips, and a clip names a line of it"
        );

        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "every clip keeps the colour it named");
        for (i, (got, want)) in back.color.iter().zip(&color).enumerate() {
            assert_eq!(
                (
                    got.brightness.to_bits(),
                    got.contrast.to_bits(),
                    got.saturation.to_bits(),
                    got.tint.to_bits()
                ),
                (
                    want.brightness.to_bits(),
                    want.contrast.to_bits(),
                    want.saturation.to_bits(),
                    want.tint.to_bits()
                ),
                "color {i} came back as other bits"
            );
        }
        assert_eq!(back.color.len(), 3);
        assert_eq!(
            emit(
                &dir,
                &back.sources,
                &back.lanes,
                &back.eq,
                &back.color,
                (1280, 720),
                0
            ),
            bytes
        );
    }

    /// A v5 file is a v6 one that grades nothing: it loads with its equalizers
    /// intact, and re-saving it writes the new magic and a `-` colour field.
    #[test]
    fn a_v5_file_grades_nothing() {
        let dir = PathBuf::from("/proj");
        let v5 = b"edith 5\nplayhead 3\nsource 0 a.mp4\neq 80.0:-3.0:0.707:ls\n\
                   video 1 0 0 30 0 0 0\naudio 1 0 0 30 0 0 -\n";
        let old = parse(v5, &dir).expect("v5 parses");
        assert!(old.color.is_empty(), "nothing before v6 grades anything");
        assert_eq!(old.eq.len(), 1, "...and its equalizers are untouched");
        assert_eq!(old.lanes[0].1[0].eq, Some(0));
        assert!(old.lanes[0].1[0].color.is_none());
        assert_eq!(
            String::from_utf8_lossy(&emit(
                &dir,
                &old.sources,
                &old.lanes,
                &old.eq,
                &old.color,
                (1280, 720),
                old.playhead
            )),
            "edith 8\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls\n\
             video 1 0 0 30 0 0 0 - fit 1000\naudio 1 0 0 30 0 0 - - fit 1000\n"
        );
    }

    /// The whole of the v8 bump, the eq and colour tests' twin: a clip's rate is
    /// written on its own line, comes back as the very number that was set, and
    /// a v7 file -- which has no such field -- loads at real time and re-saves
    /// saying so.
    #[test]
    fn speeds_round_trip_and_a_v7_file_plays_at_real_time() {
        let dir = PathBuf::from("/proj");
        let sources = vec![Source::new("/proj/a.mp4", 0)];
        let speeded = |start, in_frame, out_frame, permille| Clip {
            start,
            in_frame,
            out_frame,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::from_permille(permille),
        };
        // 30 source frames at 2x is 15 timeline frames, which is where the
        // second clip starts -- the file writes the source range and the rate,
        // and the length on the timeline is derived from the two.
        let lanes = vec![
            (
                LaneKind::Video,
                vec![speeded(0, 0, 30, 2000), speeded(15, 30, 40, 250)],
            ),
            (LaneKind::Audio, vec![speeded(0, 0, 30, 2000)]),
        ];
        let bytes = emit(&dir, &sources, &lanes, &[], &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 2000\nvideo 1 15 30 40 0 - - - fit 250\n\
             audio 1 0 0 30 0 - - - fit 2000\n",
            "the rate is the clip line's last field, in thousandths"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "every clip keeps the rate it named");

        // ...and the dialect before it, which had no rate to name.
        let v7 = b"edith 7\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
                   video 1 0 0 30 0 0 - - fit\naudio 1 0 0 30 0 0 - - fit\n";
        let old = parse(v7, &dir).expect("v7 parses");
        assert!(
            old.lanes
                .iter()
                .flat_map(|(_, clips)| clips)
                .all(|c| c.speed == Speed::NORMAL),
            "nothing before v8 plays at anything but real time"
        );
        assert_eq!(
            String::from_utf8_lossy(&emit(
                &dir,
                &old.sources,
                &old.lanes,
                &old.eq,
                &old.color,
                (1280, 720),
                old.playhead
            )),
            "edith 8\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000\naudio 1 0 0 30 0 0 - - fit 1000\n"
        );
        // A rate outside what the editor can set is a corrupt line, by name.
        let bad = b"edith 8\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 9000\n";
        assert_eq!(
            parse(bad, &dir).unwrap_err().to_string(),
            "line 3: speed 9000 is outside 250-4000 thousandths"
        );
    }

    /// The refusals the colour grammar adds, each naming its line.
    #[test]
    fn a_malformed_colour_names_its_line() {
        let dir = PathBuf::from("/proj");
        let head = "edith 6\nsource 0 a.mp4\n";
        // v6, deliberately: the colour grammar is the same one v7 inherits, and
        // a v6 clip line is one field shorter.
        for (file, want) in [
            (
                format!("{head}color 0.0:1.0:1.0\nvideo 1 0 0 5 0 - - 0\n"),
                "line 3: color wants 4 fields, found 3",
            ),
            (
                format!("{head}color 0.0:1.0:1.0:0.0 0.0:1.0:1.0:0.0\nvideo 1 0 0 5 0 - - 0\n"),
                "line 3: color wants 1 fields, found 2",
            ),
            (
                format!("{head}color 0.0:warm:1.0:0.0\nvideo 1 0 0 5 0 - - 0\n"),
                "line 3: \"warm\" is not a number",
            ),
            // The two values the format cannot write and read back as itself.
            (
                format!("{head}color inf:1.0:1.0:0.0\nvideo 1 0 0 5 0 - - 0\n"),
                "line 3: \"inf\" is not a number",
            ),
            (
                format!("{head}color 0.0:1.0:1.0:NaN\nvideo 1 0 0 5 0 - - 0\n"),
                "line 3: \"NaN\" is not a number",
            ),
            (
                format!("{head}color 0.0:1.0:1.0:0.0\nvideo 1 0 0 5 0 - - 1\n"),
                "line 4: clip names color 1 of 1",
            ),
            (
                format!("{head}video 1 0 0 5 0 - - 0\n"),
                "line 3: clip names color 0 of 0",
            ),
            // A clip's colour field is not optional in v6, and the table has to
            // be declared before the clip that names it.
            (
                format!("{head}video 1 0 0 5 0 - -\n"),
                "line 3: clip wants 7 fields, found 6",
            ),
            (
                format!("{head}video 1 0 0 5 0 - - -\ncolor 0.0:1.0:1.0:0.0\n"),
                "line 4: color after a clip",
            ),
            // ...and a colour line in a file that predates them is a corrupt one.
            (
                "edith 5\nsource 0 a.mp4\ncolor 0.0:1.0:1.0:0.0\nvideo 1 0 0 5 0 - -\n".to_string(),
                "line 3: unknown keyword \"color\"",
            ),
        ] {
            assert_eq!(parse(file.as_bytes(), &dir).unwrap_err().to_string(), want);
        }
    }

    /// The refusals the eq grammar adds, each naming its line.
    #[test]
    fn a_malformed_equalizer_names_its_line() {
        let dir = PathBuf::from("/proj");
        let head = "edith 5\nsource 0 a.mp4\n";
        for (file, want) in [
            (
                format!("{head}eq 80.0:0.0:0.707\nvideo 1 0 0 5 0 - 0\n"),
                "line 3: band wants 4 fields, found 3",
            ),
            (
                format!("{head}eq 80.0:0.0:0.707:notch\nvideo 1 0 0 5 0 - 0\n"),
                "line 3: \"notch\" is not a band shape",
            ),
            (
                format!("{head}eq 80.0:0.0:wide:pk\nvideo 1 0 0 5 0 - 0\n"),
                "line 3: \"wide\" is not a number",
            ),
            // Neither of the two values a coefficient cannot be built from, and
            // NaN would not even compare equal to itself on the way back.
            (
                format!("{head}eq 80.0:inf:0.707:pk\nvideo 1 0 0 5 0 - 0\n"),
                "line 3: \"inf\" is not a number",
            ),
            (
                format!("{head}eq NaN:0.0:0.707:pk\nvideo 1 0 0 5 0 - 0\n"),
                "line 3: \"NaN\" is not a number",
            ),
            (
                format!("{head}eq 80.0:0.0:0.707:pk\nvideo 1 0 0 5 0 - 1\n"),
                "line 4: clip names eq 1 of 1",
            ),
            (
                format!("{head}video 1 0 0 5 0 - 0\n"),
                "line 3: clip names eq 0 of 0",
            ),
            // A clip's eq field is not optional in v5, and the table has to be
            // declared before the clip that names it.
            (
                format!("{head}video 1 0 0 5 0 -\n"),
                "line 3: clip wants 6 fields, found 5",
            ),
            (
                format!("{head}video 1 0 0 5 0 - -\neq 80.0:0.0:0.707:pk\n"),
                "line 4: eq after a clip",
            ),
            // ...and an eq line in a file that predates them is a corrupt file.
            (
                "edith 4\nsource 0 a.mp4\neq 80.0:0.0:0.707:pk\nvideo 1 0 0 5 0 -\n".to_string(),
                "line 3: unknown keyword \"eq\"",
            ),
        ] {
            assert_eq!(parse(file.as_bytes(), &dir).unwrap_err().to_string(), want);
        }
    }

    /// A v4 file is a v5 one that equalizes nothing: it loads as it always did,
    /// and re-saving it writes the new magic, the `-` in every clip's eq field,
    /// and not one byte else.
    #[test]
    fn a_v4_file_equalizes_nothing() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let v4 = b"edith 4\nplayhead 12\nsource 0 a.mp4\nsource 2 /elsewhere/b.mp4\n\
                   video 1 0 0 30 0 0\nvideo 1 30 10 20 1 1\naudio 1 0 0 30 0 0\n";
        let old = parse(v4, &dir).expect("v4 parses");
        assert_eq!(
            (&old.sources, &old.lanes, old.playhead),
            (&sources, &lanes, 12)
        );
        assert!(old.eq.is_empty(), "nothing before v5 equalizes anything");
        assert_eq!(
            String::from_utf8_lossy(&flat(&dir, &old.sources, &old.lanes, old.playhead)),
            "edith 8\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000\nvideo 1 30 10 20 1 1 - - fit 1000\n\
             audio 1 0 0 30 0 0 - - fit 1000\n"
        );
        // An empty lane's line is still the whole of that lane, eq or no eq.
        let empty = parse(
            b"edith 5\nsource 0 a.mp4\nvideo 1 0 0 5 0 - -\naudio 1\n",
            &dir,
        )
        .expect("parse");
        assert!(empty.lanes[1].1.is_empty());
    }

    /// The other half of the compatibility promise: a v3 file is a v4 one
    /// without the lane numbers, and a v2 file names no audio stream and means
    /// stream 0 -- what the whole dialect could play. Both load as the two lanes
    /// they always were, and saving either writes v5 with nothing else changed.
    #[test]
    fn a_v3_file_is_two_lanes_and_a_v2_one_is_stream_0() {
        let dir = PathBuf::from("/proj");
        let (_, _, lanes) = doc();
        let v3 = b"edith 3\nplayhead 12\nsource 0 a.mp4\nsource 2 /elsewhere/b.mp4\n\
                   video 0 0 30 0 0\nvideo 30 10 20 1 1\naudio 0 0 30 0 0\n";
        let old = parse(v3, &dir).expect("v3 parses");
        assert_eq!(
            (&old.sources, &old.lanes, old.playhead),
            (
                &vec![source("/proj/a.mp4", 0), source("/elsewhere/b.mp4", 2)],
                &lanes,
                12
            ),
            "a v3 file loads as exactly the state its v5 twin does"
        );
        // ...and re-saved it *is* that twin: the magic and a lane number per
        // clip line, and not one byte else.
        assert_eq!(
            flat(&dir, &old.sources, &old.lanes, old.playhead),
            flat(&dir, &old.sources, &lanes, 12),
        );

        let v2 = b"edith 2\nplayhead 12\nsource a.mp4\nsource /elsewhere/b.mp4\n\
                   video 0 0 30 0 0\nvideo 30 10 20 1 1\naudio 0 0 30 0 0\n";
        let back = parse(v2, &dir).expect("v2 parses");
        assert_eq!(
            back.sources,
            vec![source("/proj/a.mp4", 0), source("/elsewhere/b.mp4", 0)],
        );
        assert_eq!(back.lanes, lanes);

        let v5 = flat(&dir, &back.sources, &back.lanes, back.playhead);
        assert_eq!(
            String::from_utf8_lossy(&v5),
            "edith 8\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 0 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000\nvideo 1 30 10 20 1 1 - - fit 1000\n\
             audio 1 0 0 30 0 0 - - fit 1000\n",
            "a re-saved v2 project differs only by its version, the lane \
             numbers, the streams it always meant and an equalizer it has none of"
        );
        let again = parse(&v5, &dir).expect("v5 parses");
        assert_eq!(again.sources, back.sources);
    }

    /// The compatibility promise: a v1 file is a fully-grouped, gapless pair of
    /// lanes, and saving it again writes v5.
    #[test]
    fn a_v1_file_loads_as_two_grouped_lanes() {
        let dir = PathBuf::from("/proj");
        let v1 = b"edith 1\nplayhead 5\nsource a.mp4\nclip 0 30 0\nclip 40 60 0\n";
        let back = parse(v1, &dir).expect("v1 parses");
        assert_eq!(
            back.lanes[0].1,
            vec![clip(0, 0, 30, 0, Some(0)), clip(30, 40, 60, 0, Some(1))],
            "v1 clips queue up: the second starts where the first ended"
        );
        assert_eq!(
            back.lanes,
            two(back.lanes[0].1.clone(), back.lanes[0].1.clone()),
            "one take per clip, on both lanes"
        );
        assert_eq!(back.playhead, 5);
        // Saved again it is the current version, which round-trips to the
        // same document.
        let v5 = flat(&dir, &back.sources, &back.lanes, back.playhead);
        assert!(v5.starts_with(b"edith 8\n"));
        let again = parse(&v5, &dir).expect("v5 parses");
        assert_eq!(again.lanes, back.lanes);
        // A dialect may not be mixed: lane lines under v1, `clip` under v2.
        for (bytes, want) in [
            (
                &b"edith 1\nsource a.mp4\nvideo 0 0 30 0 -\n"[..],
                "line 3: unknown keyword \"video\"",
            ),
            (
                b"edith 2\nsource a.mp4\nclip 0 30 0\n",
                "line 3: unknown keyword \"clip\"",
            ),
        ] {
            assert_eq!(parse(bytes, &dir).unwrap_err().to_string(), want);
        }
    }

    /// A gap is the absence of a line, so a lane's placements must arrive in
    /// order and never overlap -- the invariant the whole model rests on.
    #[test]
    fn a_lane_out_of_order_or_overlapping_is_refused() {
        let dir = PathBuf::from("/proj");
        for bytes in [
            &b"edith 2\nsource a.mp4\nvideo 30 0 30 0 -\nvideo 0 0 30 0 -\n"[..],
            b"edith 2\nsource a.mp4\nvideo 0 0 30 0 -\nvideo 10 0 30 0 -\n",
        ] {
            let err = parse(bytes, &dir).unwrap_err().to_string();
            assert!(err.starts_with("line 4: clip at "), "{err}");
        }
        // The lanes are independent, so a video line never crowds an audio one.
        assert!(
            parse(
                b"edith 2\nsource a.mp4\nvideo 0 0 30 0 -\naudio 0 0 30 0 -\n",
                &dir
            )
            .is_ok()
        );
        // ...and a lane may be empty as long as the other one is not.
        let only_audio = parse(b"edith 2\nsource a.mp4\naudio 0 0 30 0 -\n", &dir).expect("parse");
        assert!(only_audio.lanes[0].1.is_empty(), "V1 is there and empty");
    }

    /// A `start` near the top of `u32` used to make `Clip::end` panic in debug
    /// and wrap in release, and a wrapped end passes the overlap check --
    /// crafted numbers must be refused by name, in both dialects.
    #[test]
    fn a_clip_whose_end_overflows_is_refused() {
        let dir = PathBuf::from("/proj");
        for bytes in [
            &b"edith 2\nsource a.mp4\nvideo 4294967290 0 30 0 -\naudio 0 0 30 0 -\n"[..],
            // Two of them: the second would land *before* the first once wrapped.
            b"edith 2\nsource a.mp4\nvideo 4294967290 0 30 0 -\nvideo 4294967295 0 30 0 -\n",
            // v1 has no `start` field, but its queue reaches the same ceiling.
            b"edith 1\nsource a.mp4\nclip 0 4294967295 0\nclip 0 4294967295 0\n",
        ] {
            let err = parse(bytes, &dir).unwrap_err().to_string();
            assert!(
                err.contains("runs past the last frame there is"),
                "not refused by name: {err}"
            );
        }
    }

    /// The relocatability promise, from the two directions a project path
    /// reaches [`save`] non-canonical: a bare filename (`cd dir && edith
    /// clip.mp4`, whose parent is `""`) and a directory reached through a
    /// symlink. Both used to emit absolute source lines.
    #[test]
    fn a_non_canonical_project_path_still_writes_relative_sources() {
        assert_eq!(
            project_dir(Path::new("a.edith")),
            std::env::current_dir().expect("cwd"),
            "a bare filename is saved into the working directory"
        );

        let dir = std::env::temp_dir().join(format!("ve_edith_link_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).expect("scratch dir");
        let dir = std::fs::canonicalize(&dir).expect("canonical scratch dir");
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).expect("symlink");
        let source = dir.join("real/a.mp4");
        let path = dir.join("link/p.edith");

        let one = [clip(0, 0, 30, 0, None)];
        let entry = Source {
            path: source,
            audio_stream: 1,
        };
        save(
            &path,
            &[entry],
            &two(one.to_vec(), one.to_vec()),
            &[],
            &[],
            (1280, 720),
            0,
        )
        .expect("save");
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 8\nplayhead 0\nresolution 1280 720\nsource 1 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000\naudio 1 0 0 30 0 - - - fit 1000\n"
        );
        // Loading rejoins the *given* directory, so the file is reached by the
        // way the project was opened -- the same file, through the link, still
        // playing the stream it was saved on.
        assert_eq!(
            load(&path).expect("load").sources,
            vec![Source {
                path: dir.join("link/a.mp4"),
                audio_stream: 1,
            }]
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn escapes_survive_the_bytes_json_would_not() {
        let dir = PathBuf::from("/proj");
        // A newline and a percent in a filename, plus a non-UTF-8 byte.
        let nasty = Source {
            path: PathBuf::from(OsString::from_vec(b"/proj/we\nird 100%\xff.mp4".to_vec())),
            audio_stream: 0,
        };
        let bytes = flat(
            &dir,
            &[nasty.clone()],
            &two(vec![clip(0, 0, 5, 0, None)], Vec::new()),
            0,
        );
        assert_eq!(
            bytes.iter().filter(|&&b| b == b'\n').count(),
            6,
            "the escaped newline must not become a line break: magic, playhead, \
             resolution, source, the clip, and the empty audio lane's own line"
        );
        let back = parse(&bytes, &dir).expect("parse");
        // The spaces in it are the reason the stream field leads rather than
        // trails: everything after the first one is path.
        assert_eq!(back.sources, vec![nasty]);
    }

    #[test]
    fn a_wrong_first_line_is_refused_by_name() {
        let dir = PathBuf::from("/proj");
        let err = parse(b"edith 9\nsource 0 a.mp4\nvideo 0 0 5 0 -\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "line 1: unsupported version 9");
        for junk in [&b""[..], b"{}\n", b"source a.mp4\n"] {
            assert_eq!(
                parse(junk, &dir).unwrap_err().to_string(),
                "line 1: not a edith file"
            );
        }
    }

    #[test]
    fn malformed_lines_name_their_line_number() {
        let dir = PathBuf::from("/proj");
        let cases: [(&[u8], &str); 10] = [
            // v3 wants the stream before the path, and a path after it.
            (
                b"edith 3\nsource a.mp4\nvideo 0 0 5 0 -\n",
                "line 2: \"a.mp4\" is not a number",
            ),
            (
                b"edith 3\nsource 0\nvideo 0 0 5 0 -\n",
                "line 2: source without a path",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 0 5\n",
                "line 3: clip wants 3 fields, found 2",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 0 five 0\n",
                "line 3: \"five\" is not a number",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 5 5 0\n",
                "line 3: clip [5, 5) is empty",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 7 5 0\n",
                "line 3: clip [7, 5) is empty",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 0 5 1\n",
                "line 3: clip names source 1 of 1",
            ),
            (
                b"edith 1\nsource a.mp4\nclip 0 5 0\nsource b.mp4\n",
                "line 4: source after a clip",
            ),
            (
                b"edith 1\nsource a.mp4\n\nclip 0 5 0\n",
                "line 3: unknown keyword \"\"",
            ),
            (
                b"edith 1\nsource a.mp4\nplayhead 3\nclip 0 5 0\n",
                "line 3: playhead belongs once, before the sources",
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(parse(bytes, &dir).unwrap_err().to_string(), want);
        }
    }

    /// The emptied timeline writes and reads back: every lane declared, no clip
    /// on any of them, and the source that gave the project its frame rate still
    /// named. Nothing about the format changes for it -- an empty lane already
    /// had a line of its own.
    #[test]
    fn a_project_without_clips_round_trips() {
        let dir = PathBuf::from("/proj");
        let source = Source {
            path: PathBuf::from("/proj/a.mp4"),
            audio_stream: 0,
        };
        let bytes = flat(&dir, &[source.clone()], &two(Vec::new(), Vec::new()), 0);
        let back = parse(&bytes, &dir).expect("an emptied timeline is a project");
        assert_eq!(back.sources, vec![source]);
        assert_eq!(back.lanes, two(Vec::new(), Vec::new()));
        assert_eq!(back.playhead, 0);
        // Older dialects, where the two lanes are implied rather than declared.
        assert_eq!(
            parse(b"edith 1\nsource a.mp4\n", &dir).expect("v1").lanes,
            two(Vec::new(), Vec::new())
        );
        assert!(parse(b"edith 1\n", &dir).expect("a project of nothing").sources.is_empty());
    }

    /// Every truncation of a good file either parses or refuses; none panics.
    #[test]
    fn truncations_never_panic() {
        let (dir, sources, lanes) = doc();
        let bytes = flat(&dir, &sources, &lanes, 12);
        for cut in 0..bytes.len() {
            let _ = parse(&bytes[..cut], &dir);
            // ...and the same file with a byte lopped off the front, which
            // shifts every field into the wrong place.
            let _ = parse(&bytes[cut..], &dir);
        }
        // A % escape at the very end of a line has nothing to consume.
        assert_eq!(
            parse(b"edith 1\nsource a%\nclip 0 5 0\n", &dir)
                .unwrap_err()
                .to_string(),
            "line 2: truncated % escape in the path"
        );
    }
}
