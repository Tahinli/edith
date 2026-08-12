//! Where the real films are, on the machine that has them.
//!
//! A handful of tests are worth nothing against a fixture: a 4K HDR10 grade, a
//! header-stripped BluRay remux, a disc's PGS tracks. None of those can live in
//! this repository -- they are tens of gigabytes of somebody's library -- and
//! their paths are nobody's business but that machine's, so they are not in the
//! tree either. `real_library.toml` at the workspace root maps a logical key to
//! an absolute path; it is gitignored, and `real_library.toml.example` shows the
//! shape. Without it every test that asks here says so and passes.
//!
//! The format is the `key = "path"` subset of TOML, one per line, `#` comments:
//! enough for a path table and no dependency to parse.

use std::path::PathBuf;

/// The manifest, beside the workspace `Cargo.toml`.
const MANIFEST: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../real_library.toml");

/// The file `key` names, if this machine is one that has it.
///
/// [`None`] -- with the reason on stderr, in the skip wording the rest of these
/// tests use -- when the manifest is absent, when it does not name the key, or
/// when what it names is not there. A test's whole gate is therefore one line:
///
/// ```no_run
/// let Some(film) = engine::real_library::film("hevc_4k_hdr") else {
///     return;
/// };
/// ```
pub fn film(key: &str) -> Option<PathBuf> {
    let Ok(text) = std::fs::read_to_string(MANIFEST) else {
        eprintln!(
            "skipped: no real_library.toml at the workspace root (copy real_library.toml.example)"
        );
        return None;
    };
    let Some(path) = text.lines().find_map(|line| entry(line, key)) else {
        eprintln!("skipped: real_library.toml does not name `{key}`");
        return None;
    };
    let path = PathBuf::from(path);
    if !path.exists() {
        eprintln!("skipped: `{key}` is not on this machine");
        return None;
    }
    Some(path)
}

/// One `key = "value"` line, if it is this key's: comments and blank lines
/// answer [`None`], and so does any line whose value is not quoted.
fn entry(line: &str, key: &str) -> Option<String> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let (name, value) = line.split_once('=')?;
    if name.trim() != key {
        return None;
    }
    let path = value.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::entry;

    /// The parser, against the lines a hand-written manifest really holds: the
    /// key it was asked for and no other, quotes off, whitespace either side of
    /// the `=`, and a commented-out line is not an entry however much it looks
    /// like one.
    #[test]
    fn a_manifest_line_answers_for_its_own_key_only() {
        assert_eq!(entry(r#"k = "/a/b.mkv""#, "k").as_deref(), Some("/a/b.mkv"));
        assert_eq!(
            entry(r#"  k="/a/b.mkv"  "#, "k").as_deref(),
            Some("/a/b.mkv")
        );
        // A path with an `=` in it survives: only the first one splits.
        assert_eq!(
            entry(r#"k = "/a/b=c.mkv""#, "k").as_deref(),
            Some("/a/b=c.mkv")
        );
        assert_eq!(entry(r#"other = "/a/b.mkv""#, "k"), None);
        assert_eq!(entry(r#"# k = "/a/b.mkv""#, "k"), None);
        assert_eq!(entry("k = /a/b.mkv", "k"), None);
        assert_eq!(entry("", "k"), None);
    }
}
