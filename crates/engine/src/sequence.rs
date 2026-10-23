//! Numbered still runs: `shot_0001.png`, `shot_0002.png`, ... read as one picture track.
//!
//! Importing one file of a run (two or more frames, the same stem, digit width and extension)
//! makes ONE source whose "file" is a spelling that names the whole run, `shot_[0001-0010].png`,
//! in the run's folder. The spelling is not a file: it is parsed by [`Run::parse`] and never
//! opened, as [`crate::generate`] does for its generators. [`crate::decode::Still`] reads frame
//! `k` of the run when it is asked for it ([`Run::frame`]), so a run of a thousand frames costs
//! one decode per frame shown and nothing at import beyond a directory listing.
//!
//! A run's frames are taken at the timeline's rate: frame `k` is the picture for the `k`th frame
//! of the clip, so a run of ten frames is a clip of ten frames.

use std::path::{Path, PathBuf};

/// One numbered run of still frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The folder the frames live in.
    pub dir: PathBuf,
    /// The name before the number: `shot_` for `shot_0001.png`.
    pub stem: String,
    /// The extension as the files spell it (`png`, `JPG`).
    pub ext: String,
    /// How many digits the numbers are padded to.
    pub digits: usize,
    /// The first number of the run.
    pub first: u32,
    /// How many frames the run holds (two or more).
    pub count: u32,
}

/// The extensions a numbered run may be spelled with: the still formats this engine decodes.
fn is_still_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp")
}

/// Splits `shot_0001.png` into `("shot_", "0001", "png")`, for a name whose stem ends in digits.
fn split_name(path: &Path) -> Option<(String, String, String)> {
    let stem = path.file_stem()?.to_str()?;
    let ext = path.extension()?.to_str()?;
    if !is_still_ext(ext) {
        return None;
    }
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let (prefix, number) = stem.split_at(stem.len() - digits);
    Some((prefix.to_string(), number.to_string(), ext.to_string()))
}

impl Run {
    /// The run `path` belongs to, read from the folder beside it: the contiguous numbers that
    /// share its stem, digit width and extension. `None` for a file with no number in its name,
    /// an extension that is not a still format, or a number with no neighbour to make a run of
    /// two. The file itself need not be the first frame.
    pub fn detect(path: &Path) -> Option<Self> {
        if Self::parse(path).is_some() {
            return None; // already a spelled run: nothing to detect
        }
        let (stem, number, ext) = split_name(path)?;
        let digits = number.len();
        let at: u32 = number.parse().ok()?;
        let dir = path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut taken = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let other = entry.path();
            let Some((s, n, e)) = split_name(&other) else { continue };
            if s == stem && e.eq_ignore_ascii_case(&ext) && n.len() == digits && let Ok(v) = n.parse::<u32>() {
                taken.insert(v);
            }
        }
        if !taken.contains(&at) {
            return None;
        }
        let mut first = at;
        while first > 0 && taken.contains(&(first - 1)) {
            first -= 1;
        }
        let mut last = at;
        while taken.contains(&(last + 1)) {
            last += 1;
        }
        let count = last - first + 1;
        (count >= 2).then_some(Self { dir, stem, ext, digits, first, count })
    }

    /// The run a spelling names (`dir/shot_[0001-0010].png`), with no file read. `None` for any
    /// other path, so a real file is never mistaken for a run.
    pub fn parse(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str()?;
        let ext_at = name.rfind('.')?;
        let (head, ext) = (&name[..ext_at], &name[ext_at + 1..]);
        if !is_still_ext(ext) || !head.ends_with(']') {
            return None;
        }
        let open = head.rfind('[')?;
        let (stem, range) = (&head[..open], &head[open + 1..head.len() - 1]);
        let (lo, hi) = range.split_once('-')?;
        if lo.len() != hi.len() || lo.is_empty() || !lo.bytes().chain(hi.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (first, last): (u32, u32) = (lo.parse().ok()?, hi.parse().ok()?);
        if last < first || last - first >= u32::MAX - 1 {
            return None;
        }
        Some(Self {
            dir: path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            stem: stem.to_string(),
            ext: ext.to_string(),
            digits: lo.len(),
            first,
            count: last - first + 1,
        })
    }

    /// The spelling of this run: the one name a source carries for the whole run.
    pub fn spelled(&self) -> PathBuf {
        let last = self.first + self.count - 1;
        self.dir.join(format!(
            "{}[{:0w$}-{:0w$}].{}",
            self.stem,
            self.first,
            last,
            self.ext,
            w = self.digits
        ))
    }

    /// The file of frame `k` of the run. A `k` past the last frame is the last frame: a clip
    /// held longer than its run holds its final picture rather than reading a file that is not
    /// there.
    pub fn frame(&self, k: u32) -> PathBuf {
        let n = self.first + k.min(self.count - 1);
        self.dir.join(format!("{}{:0w$}.{}", self.stem, n, self.ext, w = self.digits))
    }
}

/// The file a path reads its picture from: frame 0 of a run's spelling, the path itself otherwise.
/// For the header reads (size, EXIF turn) that want one picture and not the whole run.
pub fn first_frame(path: &Path) -> PathBuf {
    Run::parse(path).map_or_else(|| path.to_path_buf(), |r| r.frame(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spelling names its whole run and parses back to it, a real file is never read as a
    /// spelling, and one frame of a run on disk finds the run around it and nothing else.
    #[test]
    fn a_run_is_spelled_back_and_found_from_one_of_its_frames() {
        let run = Run { dir: "/clips".into(), stem: "shot_".into(), ext: "png".into(), digits: 4, first: 1, count: 10 };
        let spelled = run.spelled();
        assert_eq!(spelled, PathBuf::from("/clips/shot_[0001-0010].png"));
        assert_eq!(Run::parse(&spelled), Some(run.clone()));
        assert_eq!(run.frame(9), PathBuf::from("/clips/shot_0010.png"));
        assert_eq!(run.frame(40), PathBuf::from("/clips/shot_0010.png"), "past the end holds the last frame");
        for name in ["shot_0001.png", "shot_[0001-0010].mp4", "shot_[1-10].png", "shot_[0001-0010]", "a[x-y].png"] {
            assert!(Run::parse(Path::new(&format!("/clips/{name}"))).is_none(), "{name}");
        }

        let dir = std::env::temp_dir().join(format!("edith-seq-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 1..=3 are a run; 5 stands alone; a different stem and a different width do not join it.
        for n in [1, 2, 3, 5] {
            std::fs::write(dir.join(format!("f_{n:04}.png")), b"x").unwrap();
        }
        std::fs::write(dir.join("g_0004.png"), b"x").unwrap();
        std::fs::write(dir.join("f_4.png"), b"x").unwrap();
        let found = Run::detect(&dir.join("f_0002.png")).expect("a run of three");
        assert_eq!((found.first, found.count, found.digits), (1, 3, 4));
        assert!(Run::detect(&dir.join("f_0005.png")).is_none(), "a lone frame is not a run");
        assert!(Run::detect(&dir.join("g_0004.png")).is_none());
        assert!(Run::detect(&dir.join("f_4.png")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
