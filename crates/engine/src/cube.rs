//! `.cube` look-up tables: the file format, the sampler, and the process-wide
//! table of loaded files a grade names its LUT through.
//!
//! The format is Adobe's Cube 1.0 as every grading tool writes it: optional
//! `TITLE`, one of `LUT_1D_SIZE N` / `LUT_3D_SIZE N`, optional `DOMAIN_MIN r g b`
//! and `DOMAIN_MAX r g b` (or Resolve's `LUT_1D_INPUT_RANGE lo hi` /
//! `LUT_3D_INPUT_RANGE lo hi`), `#` comments, then the table as one `r g b`
//! line per entry with red changing fastest. A 3D table is sampled
//! tetrahedrally (the interpolation `ffmpeg -vf lut3d=interp=tetrahedral` and the
//! grading suites use), a 1D table linearly per channel.
//!
//! A grade holds a [`LutRef`], not the table: a small `Copy` handle into the
//! registry below, so [`crate::color::ColorParams`] stays plain data that every
//! render path can copy per frame. The registry maps a handle to the path it was
//! made from and to the parsed table -- or to why there is none, which is how a
//! missing or broken file becomes a warning and an ungraded-by-LUT picture
//! instead of a failed render.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The largest 3D table edge a file may declare (65 is the biggest any tool writes).
pub const MAX_3D_SIZE: usize = 65;
/// The largest 1D table a file may declare.
pub const MAX_1D_SIZE: usize = 65_536;
/// The largest `.cube` file read: a 65-edge table is ~10 MB of text.
const MAX_FILE_BYTES: u64 = 64 << 20;

/// One parsed `.cube` file.
#[derive(Debug, Clone, PartialEq)]
pub struct Cube {
    /// The `TITLE`, if the file gave one.
    pub title: Option<String>,
    /// Whether this is a 3D table (`LUT_3D_SIZE`) rather than a 1D one.
    pub three_d: bool,
    /// Entries per axis (3D) or entries (1D).
    pub size: usize,
    /// The input value mapped to the first entry, per channel.
    pub domain_min: [f32; 3],
    /// The input value mapped to the last entry, per channel.
    pub domain_max: [f32; 3],
    /// The table, red fastest: `size` entries for 1D, `size^3` for 3D.
    pub data: Vec<[f32; 3]>,
}

fn nums<const N: usize>(rest: &[&str], line: usize, what: &str) -> Result<[f32; N], String> {
    if rest.len() != N {
        return Err(format!("line {line}: {what} wants {N} numbers, found {}", rest.len()));
    }
    let mut out = [0f32; N];
    for (o, t) in out.iter_mut().zip(rest) {
        *o = t
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("line {line}: {what}: '{t}' is not a number"))?;
    }
    Ok(out)
}

fn size_of(rest: &[&str], line: usize, what: &str, max: usize) -> Result<usize, String> {
    let [t] = rest else {
        return Err(format!("line {line}: {what} wants one size"));
    };
    let n: usize = t.parse().map_err(|_| format!("line {line}: {what}: '{t}' is not a size"))?;
    if !(2..=max).contains(&n) {
        return Err(format!("line {line}: {what} {n} is outside 2..={max}"));
    }
    Ok(n)
}

/// Parses the text of a `.cube` file. Every refusal names the line and what is
/// wrong with it, so the message can be shown as it is.
pub fn parse(text: &str) -> Result<Cube, String> {
    let mut title = None;
    let (mut size1, mut size3) = (None, None);
    let (mut min, mut max) = ([0f32; 3], [1f32; 3]);
    let mut data: Vec<[f32; 3]> = Vec::new();
    let mut want = 0usize;
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let head = words[0];
        let starts_alpha = head.starts_with(|c: char| c.is_ascii_alphabetic());
        if !starts_alpha {
            if want == 0 {
                return Err(format!("line {n}: table data before LUT_1D_SIZE or LUT_3D_SIZE"));
            }
            if data.len() == want {
                return Err(format!("line {n}: more table entries than the declared {want}"));
            }
            data.push(nums::<3>(&words, n, "a table entry")?);
            continue;
        }
        if !data.is_empty() {
            return Err(format!("line {n}: keyword '{head}' after the table data began"));
        }
        let rest = &words[1..];
        match head {
            "TITLE" => {
                let t = line["TITLE".len()..].trim();
                title = Some(t.trim_matches('"').to_string());
            }
            "LUT_1D_SIZE" => size1 = Some(size_of(rest, n, head, MAX_1D_SIZE)?),
            "LUT_3D_SIZE" => size3 = Some(size_of(rest, n, head, MAX_3D_SIZE)?),
            "DOMAIN_MIN" => min = nums::<3>(rest, n, head)?,
            "DOMAIN_MAX" => max = nums::<3>(rest, n, head)?,
            "LUT_1D_INPUT_RANGE" | "LUT_3D_INPUT_RANGE" => {
                let [lo, hi] = nums::<2>(rest, n, head)?;
                min = [lo; 3];
                max = [hi; 3];
            }
            // Other tools' own keywords (Resolve's LUT_IN_VIDEO_RANGE and the
            // like) carry nothing this sampler reads.
            _ => {}
        }
        want = match (size1, size3) {
            (Some(_), Some(_)) => {
                return Err(format!("line {n}: both LUT_1D_SIZE and LUT_3D_SIZE; one table per file"));
            }
            (Some(s), None) => s,
            (None, Some(s)) => s * s * s,
            (None, None) => 0,
        };
    }
    if want == 0 {
        return Err("no LUT_1D_SIZE or LUT_3D_SIZE line: not a .cube table".into());
    }
    if data.len() != want {
        return Err(format!("the table has {} entries, the size line declares {want}", data.len()));
    }
    if (0..3).any(|c| min[c] >= max[c]) {
        return Err("DOMAIN_MIN must be below DOMAIN_MAX on every channel".into());
    }
    Ok(Cube {
        title,
        three_d: size3.is_some(),
        size: size3.or(size1).unwrap_or(0),
        domain_min: min,
        domain_max: max,
        data,
    })
}

/// Reads and parses a `.cube` file.
pub fn load(path: &Path) -> Result<Cube, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("{} is {} bytes, more than a .cube table can be", path.display(), meta.len()));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{} is not a text file", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

impl Cube {
    /// The table at `rgb` (non-linear R'G'B', 0..1 for the default domain).
    /// Inputs outside the domain are held at its edge.
    pub fn sample(&self, rgb: [f32; 3]) -> [f32; 3] {
        let edge = (self.size - 1) as f32;
        let mut p = [0f32; 3];
        for c in 0..3 {
            let t = (rgb[c] - self.domain_min[c]) / (self.domain_max[c] - self.domain_min[c]);
            p[c] = t.clamp(0.0, 1.0) * edge;
        }
        if !self.three_d {
            let mut out = [0f32; 3];
            for c in 0..3 {
                let i = (p[c] as usize).min(self.size - 2);
                let d = p[c] - i as f32;
                out[c] = self.data[i][c] * (1.0 - d) + self.data[i + 1][c] * d;
            }
            return out;
        }
        let s = self.size;
        let i = [0, 1, 2].map(|c| (p[c] as usize).min(s - 2));
        let d = [0, 1, 2].map(|c| p[c] - i[c] as f32);
        let at = |r: usize, g: usize, b: usize| self.data[(i[0] + r) + (i[1] + g) * s + (i[2] + b) * s * s];
        let c000 = at(0, 0, 0);
        let c111 = at(1, 1, 1);
        let (dr, dg, db) = (d[0], d[1], d[2]);
        // The six tetrahedra of the unit cube, picked by the order of the
        // fractions: each output is four corners weighted by their differences.
        let (w, a, b) = if dr > dg {
            if dg > db {
                ([1.0 - dr, dr - dg, dg - db, db], at(1, 0, 0), at(1, 1, 0))
            } else if dr > db {
                ([1.0 - dr, dr - db, db - dg, dg], at(1, 0, 0), at(1, 0, 1))
            } else {
                ([1.0 - db, db - dr, dr - dg, dg], at(0, 0, 1), at(1, 0, 1))
            }
        } else if db > dg {
            ([1.0 - db, db - dg, dg - dr, dr], at(0, 0, 1), at(0, 1, 1))
        } else if db > dr {
            ([1.0 - dg, dg - db, db - dr, dr], at(0, 1, 0), at(0, 1, 1))
        } else {
            ([1.0 - dg, dg - dr, dr - db, db], at(0, 1, 0), at(1, 1, 0))
        };
        [0, 1, 2].map(|c| w[0] * c000[c] + w[1] * a[c] + w[2] * b[c] + w[3] * c111[c])
    }
}

/// A grade's handle on a `.cube` file: an index into the process-wide table of
/// loaded files. Equal paths share a handle, so a grade compares equal to
/// another naming the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LutRef(u32);

struct Entry {
    path: PathBuf,
    table: Result<Arc<Cube>, String>,
}

static TABLE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn table() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

impl LutRef {
    /// The handle for `path`, reading the file the first time it is named and
    /// again whenever an earlier read failed (so a file that turns up later is
    /// picked up). Never fails: a file that cannot be read gets a handle whose
    /// [`table`](Self::table) says why.
    pub fn open(path: &Path) -> Self {
        let mut t = table();
        if let Some(i) = t.iter().position(|e| e.path == path) {
            if t[i].table.is_err() {
                t[i].table = load(path).map(Arc::new);
            }
            return Self(i as u32);
        }
        t.push(Entry {
            path: path.to_path_buf(),
            table: load(path).map(Arc::new),
        });
        Self((t.len() - 1) as u32)
    }

    /// [`open`](Self::open), re-reading the file even when it loaded before: what a
    /// relink to the same path (an edited file) asks for.
    pub fn reload(path: &Path) -> Self {
        let r = Self::open(path);
        let fresh = load(path).map(Arc::new);
        table()[r.0 as usize].table = fresh;
        r
    }

    /// The path this handle was made from.
    pub fn path(self) -> PathBuf {
        table().get(self.0 as usize).map(|e| e.path.clone()).unwrap_or_default()
    }

    /// The parsed table, or why there is none (missing file, malformed file).
    pub fn table(self) -> Result<Arc<Cube>, String> {
        match table().get(self.0 as usize) {
            Some(e) => e.table.clone(),
            None => Err("unknown LUT".into()),
        }
    }

    /// Why the table cannot be used, or `None` when it can.
    pub fn problem(self) -> Option<String> {
        self.table().err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3D table of `size` whose entries are `f` of the grid point.
    pub(crate) fn cube_text(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
        let mut s = format!("TITLE \"t\"\nLUT_3D_SIZE {size}\n");
        let e = (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let o = f([r as f32 / e, g as f32 / e, b as f32 / e]);
                    s.push_str(&format!("{} {} {}\n", o[0], o[1], o[2]));
                }
            }
        }
        s
    }

    #[test]
    fn identity_3d_samples_back_its_input() {
        for size in [2, 17, 33] {
            let c = parse(&cube_text(size, |p| p)).expect("parses");
            assert!(c.three_d && c.size == size);
            for p in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.3, 0.71, 0.02], [0.5, 0.25, 0.9]] {
                let o = c.sample(p);
                for k in 0..3 {
                    assert!((o[k] - p[k]).abs() < 1e-5, "{size} {p:?} -> {o:?}");
                }
            }
        }
    }

    /// Tetrahedral interpolation is exact for any function linear in r, g and b,
    /// whatever the cell -- a channel swap and an affine mix are both.
    #[test]
    fn tetrahedral_is_exact_for_linear_tables() {
        let f = |p: [f32; 3]| [p[2], 0.5 * p[0] + 0.25 * p[1], 1.0 - p[1]];
        let c = parse(&cube_text(9, f)).expect("parses");
        for p in [[0.13, 0.77, 0.5], [0.9, 0.1, 0.45], [0.33, 0.33, 0.34]] {
            let (o, w) = (c.sample(p), f(p));
            for k in 0..3 {
                assert!((o[k] - w[k]).abs() < 1e-5, "{p:?}: {o:?} vs {w:?}");
            }
        }
    }

    #[test]
    fn one_d_and_domain() {
        let c = parse("LUT_1D_SIZE 3\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n0 0 0\n0.5 0.25 1\n1 1 1\n").expect("1D");
        assert!(!c.three_d);
        // 1.0 is the middle entry of a 0..2 domain.
        assert_eq!(c.sample([1.0, 1.0, 1.0]), [0.5, 0.25, 1.0]);
        let o = c.sample([0.5, 0.5, 0.5]);
        assert!((o[0] - 0.25).abs() < 1e-6 && (o[2] - 0.5).abs() < 1e-6);
        // Outside the domain is held at its edge.
        assert_eq!(c.sample([-3.0, 9.0, 2.0]), [0.0, 1.0, 1.0]);
        let r = parse("LUT_3D_INPUT_RANGE -0.5 1.5\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").expect("range");
        assert_eq!((r.domain_min, r.domain_max), ([-0.5; 3], [1.5; 3]));
    }

    /// The fuzz corpus: every malformed file is a named refusal, never a panic.
    #[test]
    fn malformed_files_are_refused_by_name() {
        let ok = cube_text(2, |p| p);
        let cases: Vec<(String, &str)> = vec![
            (String::new(), "no LUT_1D_SIZE"),
            ("# only a comment\n".into(), "no LUT_1D_SIZE"),
            ("0 0 0\nLUT_3D_SIZE 2\n".into(), "before LUT_1D_SIZE"),
            ("LUT_3D_SIZE 66\n".into(), "outside 2..=65"),
            ("LUT_3D_SIZE 1\n0 0 0\n".into(), "outside 2..=65"),
            ("LUT_3D_SIZE x\n".into(), "not a size"),
            ("LUT_3D_SIZE 2 3\n".into(), "one size"),
            ("LUT_3D_SIZE 99999999999999999999\n".into(), "not a size"),
            ("LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n".into(), "both"),
            ("LUT_1D_SIZE 2\n0 0 0\n".into(), "has 1 entries"),
            ("LUT_1D_SIZE 2\n0 0 0\n1 1 1\n1 1 1\n".into(), "more table entries"),
            ("LUT_1D_SIZE 2\n0 0\n1 1 1\n".into(), "wants 3 numbers"),
            ("LUT_1D_SIZE 2\n0 nan 0\n1 1 1\n".into(), "not a number"),
            ("LUT_1D_SIZE 2\n0 inf 0\n1 1 1\n".into(), "not a number"),
            ("LUT_1D_SIZE 2\n0 0 0\nTITLE late\n1 1 1\n".into(), "after the table"),
            ("DOMAIN_MIN 1 0 0\nDOMAIN_MAX 0 1 1\nLUT_1D_SIZE 2\n0 0 0\n1 1 1\n".into(), "below DOMAIN_MAX"),
            ("DOMAIN_MIN 0 0\nLUT_1D_SIZE 2\n".into(), "wants 3 numbers"),
            (ok[..ok.len() - 7].to_string(), "entries"),
            ("\u{0}\u{1}\u{2}garbage\n".into(), "before LUT_1D_SIZE"),
        ];
        for (text, want) in cases {
            let e = parse(&text).expect_err(&text);
            assert!(e.contains(want), "{text:?}: {e}");
        }
        // Random bytes never panic.
        let mut seed = 0x9e37_79b9u32;
        for _ in 0..300 {
            let mut s = String::from("LUT_3D_SIZE 2\n");
            for _ in 0..40 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                s.push(b"0123456789 .-e\nLUT_ #x"[(seed % 22) as usize] as char);
            }
            let _ = parse(&s);
        }
    }

    #[test]
    fn a_missing_file_is_a_handle_with_a_reason() {
        let r = LutRef::open(Path::new("/nonexistent/edith/look.cube"));
        assert!(r.problem().expect("missing").contains("cannot read"));
        assert_eq!(r, LutRef::open(Path::new("/nonexistent/edith/look.cube")), "one handle per path");
    }
}
