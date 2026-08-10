//! The project file: a line of text per thing, and nothing else.
//!
//! ```text
//! edith 2
//! playhead 90
//! source test_av.mp4
//! source /elsewhere/test_av2.mp4
//! video 0 0 120 0 0
//! audio 0 0 120 0 0
//! video 120 0 120 1 -
//! ```
//!
//! A lane line is `<lane> <start> <in> <out> <source> <link>`: where the clip
//! sits on the timeline, the half-open source range it plays, the file it plays
//! from, and its group id (`-` for none). Timeline placement is explicit, so a
//! *gap* is simply a stretch no line covers -- there is nothing to write for
//! one, and nothing that can disagree about its length.
//!
//! **Version 1** wrote one lane, queued end to end: `clip <in> <out> <source>`.
//! Such a file still loads -- the clips are laid out cumulatively and copied
//! onto both lanes as one group each, which is exactly what a v1 timeline meant
//! -- and saving it again writes v2. A v1 reader refuses a v2 file by name.
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

use crate::project::Clip;

/// What [`save`] writes. Read support goes back to `edith 1`; see the module
/// docs for what that dialect looked like.
const MAGIC: &[u8] = b"edith 2";
const MAGIC_V1: &[u8] = b"edith 1";

/// What a project file says: an edit list plus where the playhead stood.
/// Structurally valid by construction -- see [`parse`].
#[derive(Debug)]
pub struct Document {
    /// Absolute, relative entries already joined to the file's own directory.
    pub sources: Vec<PathBuf>,
    pub video: Vec<Clip>,
    pub audio: Vec<Clip>,
    pub playhead: u32,
}

/// Writes the project to `path`, atomically. `sources` should already be
/// orphan-free ([`crate::Project::without_orphan_sources`]).
pub fn save(
    path: &Path,
    sources: &[PathBuf],
    video: &[Clip],
    audio: &[Clip],
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
            f.write_all(&emit(&dir, sources, video, audio, playhead))?;
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

fn emit(dir: &Path, sources: &[PathBuf], video: &[Clip], audio: &[Clip], playhead: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(b'\n');
    out.extend_from_slice(format!("playhead {playhead}\n").as_bytes());
    for s in sources {
        out.extend_from_slice(b"source ");
        escape(s.strip_prefix(dir).unwrap_or(s), &mut out);
        out.push(b'\n');
    }
    // Lane by lane rather than interleaved by time: a lane reads as a list, and
    // the parser gets its sortedness check for free from the order it is in.
    for (keyword, clips) in [("video", video), ("audio", audio)] {
        for c in clips {
            let link = c.link.map_or("-".to_string(), |l| l.to_string());
            out.extend_from_slice(
                format!(
                    "{keyword} {} {} {} {} {link}\n",
                    c.start, c.in_frame, c.out_frame, c.source
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
    if first != MAGIC && !v1 {
        return Err(match first.strip_prefix(b"edith ") {
            Some(v) => format!("line 1: unsupported version {}", String::from_utf8_lossy(v)),
            None => "line 1: not a edith file".to_string(),
        }
        .into());
    }

    let mut doc = Document {
        sources: Vec::new(),
        video: Vec::new(),
        audio: Vec::new(),
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
            b"source" => {
                if !doc.video.is_empty() || !doc.audio.is_empty() {
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
                        link: Some(doc.video.len() as u32),
                    },
                    &doc,
                    n,
                )?;
                queued = clip.end();
                doc.video.push(clip);
                doc.audio.push(clip);
            }
            b"video" | b"audio" if !v1 => {
                let f = fields(rest, 5, "clip", n)?;
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
                    },
                    &doc,
                    n,
                )?;
                let lane = match keyword {
                    b"video" => &mut doc.video,
                    _ => &mut doc.audio,
                };
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
    if doc.video.is_empty() && doc.audio.is_empty() {
        return Err("no clips: an empty timeline is not a project".into());
    }
    Ok(doc)
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
    Ok(clip)
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
        }
    }

    /// Two sources, one under the project's own directory and one not, a clip
    /// from each on the video lane, and an audio lane with a gap in the middle
    /// -- the shape every case below starts from.
    fn doc() -> (PathBuf, Vec<PathBuf>, Vec<Clip>, Vec<Clip>) {
        (
            PathBuf::from("/proj"),
            vec![
                PathBuf::from("/proj/a.mp4"),
                PathBuf::from("/elsewhere/b.mp4"),
            ],
            vec![clip(0, 0, 30, 0, Some(0)), clip(30, 10, 20, 1, Some(1))],
            vec![clip(0, 0, 30, 0, Some(0))],
        )
    }

    #[test]
    fn relative_and_absolute_paths_round_trip() {
        let (dir, sources, video, audio) = doc();
        let bytes = emit(&dir, &sources, &video, &audio, 12);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 2\nplayhead 12\nsource a.mp4\nsource /elsewhere/b.mp4\n\
             video 0 0 30 0 0\nvideo 30 10 20 1 1\naudio 0 0 30 0 0\n",
            "the file under the project directory is written relative to it"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.sources, sources, "relative entries rejoin the dir");
        assert_eq!(back.video, video);
        assert_eq!(
            back.audio, audio,
            "the trailing gap needs no line of its own"
        );
        assert_eq!(back.playhead, 12);
        // ...and emitting the parsed document reproduces the same bytes.
        assert_eq!(
            emit(&dir, &back.sources, &back.video, &back.audio, back.playhead),
            bytes
        );
    }

    /// The compatibility promise: a v1 file is a fully-grouped, gapless pair of
    /// lanes, and saving it again writes v2.
    #[test]
    fn a_v1_file_loads_as_two_grouped_lanes() {
        let dir = PathBuf::from("/proj");
        let v1 = b"edith 1\nplayhead 5\nsource a.mp4\nclip 0 30 0\nclip 40 60 0\n";
        let back = parse(v1, &dir).expect("v1 parses");
        assert_eq!(
            back.video,
            vec![clip(0, 0, 30, 0, Some(0)), clip(30, 40, 60, 0, Some(1))],
            "v1 clips queue up: the second starts where the first ended"
        );
        assert_eq!(back.audio, back.video, "one take per clip, on both lanes");
        assert_eq!(back.playhead, 5);
        // Saved again it is v2, and that round-trips to the same document.
        let v2 = emit(&dir, &back.sources, &back.video, &back.audio, back.playhead);
        assert!(v2.starts_with(b"edith 2\n"));
        let again = parse(&v2, &dir).expect("v2 parses");
        assert_eq!((again.video, again.audio), (back.video, back.audio));
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
        assert!(only_audio.video.is_empty());
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
        save(&path, &[source], &one, &one, 0).expect("save");
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 2\nplayhead 0\nsource a.mp4\nvideo 0 0 30 0 -\naudio 0 0 30 0 -\n"
        );
        // Loading rejoins the *given* directory, so the file is reached by the
        // way the project was opened -- the same file, through the link.
        assert_eq!(
            load(&path).expect("load").sources,
            vec![dir.join("link/a.mp4")]
        );
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn escapes_survive_the_bytes_json_would_not() {
        let dir = PathBuf::from("/proj");
        // A newline and a percent in a filename, plus a non-UTF-8 byte.
        let nasty = PathBuf::from(OsString::from_vec(b"/proj/we\nird 100%\xff.mp4".to_vec()));
        let bytes = emit(&dir, &[nasty.clone()], &[clip(0, 0, 5, 0, None)], &[], 0);
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
        let err = parse(b"edith 3\nsource a.mp4\nvideo 0 0 5 0 -\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "line 1: unsupported version 3");
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
        let cases: [(&[u8], &str); 8] = [
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

    #[test]
    fn a_project_without_clips_is_not_a_project() {
        let dir = PathBuf::from("/proj");
        assert_eq!(
            parse(b"edith 1\nsource a.mp4\n", &dir)
                .unwrap_err()
                .to_string(),
            "no clips: an empty timeline is not a project"
        );
        assert_eq!(
            parse(b"edith 1\n", &dir).unwrap_err().to_string(),
            "no clips: an empty timeline is not a project"
        );
    }

    /// Every truncation of a good file either parses or refuses; none panics.
    #[test]
    fn truncations_never_panic() {
        let (dir, sources, video, audio) = doc();
        let bytes = emit(&dir, &sources, &video, &audio, 12);
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
