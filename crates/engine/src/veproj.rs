//! The project file: a line of text per thing, and nothing else.
//!
//! ```text
//! veproj 1
//! playhead 90
//! source test_av.mp4
//! source /elsewhere/test_av2.mp4
//! clip 0 120 0
//! clip 0 120 1
//! ```
//!
//! Text because an edit list is three integers and a path, and a path is
//! *bytes* on this platform -- a JSON string would have to lossily decode one.
//! So the path field is byte-escaped (`%` -> `%25`, newline -> `%0A`, which is
//! everything a line-based format cannot carry) and survives round-trip
//! exactly. Paths under the project file's own directory are written relative
//! to it, so a folder holding the media and the `.veproj` can be moved or
//! copied anywhere and still open; anything else is absolute.
//!
//! The parser is strict and every refusal names its 1-based line: a project
//! file is generated, so a line it did not generate is a corrupt file, not a
//! dialect. Structure is checked here (fields, ordering, empty clips,
//! out-of-range source indexes); whether the files on disk still match the
//! timeline is [`crate::PlaybackSession::open_project`]'s business.
//!
//! Writing goes through `<path>.part` and a rename, as an export does, so an
//! interrupted save cannot destroy the previous version of the project.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::project::Clip;

/// First line, exactly. The version is part of it: a v2 reader will accept a
/// `veproj 1` file, and a v1 reader refuses a `veproj 2` one by name.
const MAGIC: &[u8] = b"veproj 1";

/// What a project file says: an edit list plus where the playhead stood.
/// Structurally valid by construction -- see [`parse`].
#[derive(Debug)]
pub struct Document {
    /// Absolute, relative entries already joined to the file's own directory.
    pub sources: Vec<PathBuf>,
    pub clips: Vec<Clip>,
    pub playhead: u32,
}

/// Writes the project to `path`, atomically. `sources` should already be
/// orphan-free ([`crate::Project::without_orphan_sources`]).
pub fn save(path: &Path, sources: &[PathBuf], clips: &[Clip], playhead: u32) -> crate::Result<()> {
    let dir = path.parent().unwrap_or(Path::new(""));
    let mut part = path.to_path_buf().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    // The rename publishes the file under the caller's name in one step, on the
    // same directory; until it happens the old project file is still the whole
    // truth.
    let result = std::fs::write(&part, emit(dir, sources, clips, playhead))
        .and_then(|()| std::fs::rename(&part, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map_err(Into::into)
}

pub fn load(path: &Path) -> crate::Result<Document> {
    let data = std::fs::read(path)?;
    parse(&data, path.parent().unwrap_or(Path::new("")))
}

fn emit(dir: &Path, sources: &[PathBuf], clips: &[Clip], playhead: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(b'\n');
    out.extend_from_slice(format!("playhead {playhead}\n").as_bytes());
    for s in sources {
        out.extend_from_slice(b"source ");
        escape(s.strip_prefix(dir).unwrap_or(s), &mut out);
        out.push(b'\n');
    }
    for c in clips {
        out.extend_from_slice(
            format!("clip {} {} {}\n", c.in_frame, c.out_frame, c.source).as_bytes(),
        );
    }
    out
}

fn parse(data: &[u8], dir: &Path) -> crate::Result<Document> {
    // One trailing newline is the line terminator of the last line, not an
    // empty line -- any further one is, and is refused below.
    let body = data.strip_suffix(b"\n").unwrap_or(data);
    let mut lines = body.split(|&b| b == b'\n').enumerate();
    let (_, first) = lines.next().unwrap_or((0, &[]));
    if first != MAGIC {
        return Err(match first.strip_prefix(b"veproj ") {
            Some(v) => format!("line 1: unsupported version {}", String::from_utf8_lossy(v)),
            None => "line 1: not a veproj file".to_string(),
        }
        .into());
    }

    let mut doc = Document {
        sources: Vec::new(),
        clips: Vec::new(),
        playhead: 0,
    };
    let mut playhead_seen = false;
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
            b"source" => {
                if !doc.clips.is_empty() {
                    return Err(format!("line {n}: source after a clip").into());
                }
                if rest.is_empty() {
                    return Err(format!("line {n}: source without a path").into());
                }
                let path = unescape(rest, n)?;
                // Relative means "next to the project file", which is what
                // makes a whole folder relocatable.
                doc.sources.push(if path.is_absolute() {
                    path
                } else {
                    dir.join(path)
                });
            }
            b"clip" => {
                let fields: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if fields.len() != 3 {
                    return Err(
                        format!("line {n}: clip wants 3 fields, found {}", fields.len()).into(),
                    );
                }
                let clip = Clip {
                    in_frame: number(fields[0], n)?,
                    out_frame: number(fields[1], n)?,
                    source: number(fields[2], n)? as usize,
                };
                if clip.out_frame <= clip.in_frame {
                    return Err(format!(
                        "line {n}: clip [{}, {}) is empty",
                        clip.in_frame, clip.out_frame
                    )
                    .into());
                }
                if clip.source >= doc.sources.len() {
                    return Err(format!(
                        "line {n}: clip names source {} of {}",
                        clip.source,
                        doc.sources.len()
                    )
                    .into());
                }
                doc.clips.push(clip);
            }
            // Empty lines land here too: nothing in this format is optional
            // whitespace.
            _ => {
                return Err(format!(
                    "line {n}: unknown keyword {:?}",
                    String::from_utf8_lossy(keyword)
                )
                .into());
            }
        }
    }
    if doc.clips.is_empty() {
        return Err("no clips: an empty timeline is not a project".into());
    }
    Ok(doc)
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

    fn clip(in_frame: u32, out_frame: u32, source: usize) -> Clip {
        Clip {
            in_frame,
            out_frame,
            source,
        }
    }

    /// Two sources, one under the project's own directory and one not, and a
    /// clip from each -- the shape every case below starts from.
    fn doc() -> (PathBuf, Vec<PathBuf>, Vec<Clip>) {
        (
            PathBuf::from("/proj"),
            vec![
                PathBuf::from("/proj/a.mp4"),
                PathBuf::from("/elsewhere/b.mp4"),
            ],
            vec![clip(0, 30, 0), clip(10, 20, 1)],
        )
    }

    #[test]
    fn relative_and_absolute_paths_round_trip() {
        let (dir, sources, clips) = doc();
        let bytes = emit(&dir, &sources, &clips, 12);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "veproj 1\nplayhead 12\nsource a.mp4\nsource /elsewhere/b.mp4\nclip 0 30 0\nclip 10 20 1\n",
            "the file under the project directory is written relative to it"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.sources, sources, "relative entries rejoin the dir");
        assert_eq!(back.clips, clips);
        assert_eq!(back.playhead, 12);
        // ...and emitting the parsed document reproduces the same bytes.
        assert_eq!(emit(&dir, &back.sources, &back.clips, back.playhead), bytes);
    }

    #[test]
    fn escapes_survive_the_bytes_json_would_not() {
        let dir = PathBuf::from("/proj");
        // A newline and a percent in a filename, plus a non-UTF-8 byte.
        let nasty = PathBuf::from(OsString::from_vec(b"/proj/we\nird 100%\xff.mp4".to_vec()));
        let bytes = emit(&dir, &[nasty.clone()], &[clip(0, 5, 0)], 0);
        assert_eq!(
            bytes.iter().filter(|&&b| b == b'\n').count(),
            4,
            "the escaped newline must not become a line break"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.sources, vec![nasty]);
    }

    #[test]
    fn a_wrong_first_line_is_refused_by_name() {
        let dir = PathBuf::from("/proj");
        let err = parse(b"veproj 2\nsource a.mp4\nclip 0 5 0\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "line 1: unsupported version 2");
        for junk in [&b""[..], b"{}\n", b"source a.mp4\n"] {
            assert_eq!(
                parse(junk, &dir).unwrap_err().to_string(),
                "line 1: not a veproj file"
            );
        }
    }

    #[test]
    fn malformed_lines_name_their_line_number() {
        let dir = PathBuf::from("/proj");
        let cases: [(&[u8], &str); 8] = [
            (
                b"veproj 1\nsource a.mp4\nclip 0 5\n",
                "line 3: clip wants 3 fields, found 2",
            ),
            (
                b"veproj 1\nsource a.mp4\nclip 0 five 0\n",
                "line 3: \"five\" is not a number",
            ),
            (
                b"veproj 1\nsource a.mp4\nclip 5 5 0\n",
                "line 3: clip [5, 5) is empty",
            ),
            (
                b"veproj 1\nsource a.mp4\nclip 7 5 0\n",
                "line 3: clip [7, 5) is empty",
            ),
            (
                b"veproj 1\nsource a.mp4\nclip 0 5 1\n",
                "line 3: clip names source 1 of 1",
            ),
            (
                b"veproj 1\nsource a.mp4\nclip 0 5 0\nsource b.mp4\n",
                "line 4: source after a clip",
            ),
            (
                b"veproj 1\nsource a.mp4\n\nclip 0 5 0\n",
                "line 3: unknown keyword \"\"",
            ),
            (
                b"veproj 1\nsource a.mp4\nplayhead 3\nclip 0 5 0\n",
                "line 3: playhead belongs once, before the sources",
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(parse(bytes, &dir).unwrap_err().to_string(), want);
        }
    }

    #[test]
    fn a_project_without_clips_is_not_a_project() {
        let dir = PathBuf::from("/proj");
        assert_eq!(
            parse(b"veproj 1\nsource a.mp4\n", &dir)
                .unwrap_err()
                .to_string(),
            "no clips: an empty timeline is not a project"
        );
        assert_eq!(
            parse(b"veproj 1\n", &dir).unwrap_err().to_string(),
            "no clips: an empty timeline is not a project"
        );
    }

    /// Every truncation of a good file either parses or refuses; none panics.
    #[test]
    fn truncations_never_panic() {
        let (dir, sources, clips) = doc();
        let bytes = emit(&dir, &sources, &clips, 12);
        for cut in 0..bytes.len() {
            let _ = parse(&bytes[..cut], &dir);
            // ...and the same file with a byte lopped off the front, which
            // shifts every field into the wrong place.
            let _ = parse(&bytes[cut..], &dir);
        }
        // A % escape at the very end of a line has nothing to consume.
        assert_eq!(
            parse(b"veproj 1\nsource a%\nclip 0 5 0\n", &dir)
                .unwrap_err()
                .to_string(),
            "line 2: truncated % escape in the path"
        );
    }
}
