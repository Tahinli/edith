//! Looks: a whole colour grade under one name. Eight built-in looks are colour
//! presets (sliders, wheels and curves, no LUT file), and the user's own looks
//! are saved as `<name>.look` text files in `$XDG_DATA_HOME/edith/looks`.
//!
//! A look is a plain [`ColorParams`]: applying one replaces the clip's grade,
//! and what is saved is the grade as it stands. A `.cube` file a grade names is
//! kept as its path and mix, so the look loads the same LUT again.
//!
//! The file format, one `key values` line each, in this order:
//!
//! ```text
//! edith look 1
//! brightness 0.04
//! contrast 0.85
//! saturation 0.75
//! tint 0.05
//! lift 0.1 0 0 0.03          (master, red, green, blue)
//! gamma 0 0 0 0
//! gain 0 0 0 0
//! curve master 0,0 0.25,0.2 0.75,0.8 1,1   ("-" for a straight curve)
//! curve red -
//! curve green -
//! curve blue -
//! lut 0.8 /home/me/luts/warm.cube          (only with a LUT: mix, then the path)
//! ```
//!
//! Unknown keys are skipped, so a later edith can add lines an older one ignores.

use std::path::{Path, PathBuf};

use crate::color::{ColorParams, Curve, Look, Wheels};
use crate::cube::LutRef;

/// The extension a saved look's file carries.
pub const EXT: &str = "look";
/// The first line of every look file.
const MAGIC: &str = "edith look 1";
/// Longest name a saved look may have, in characters.
const MAX_NAME: usize = 60;

/// The built-in looks, in the order the card lists them.
pub fn builtins() -> Vec<(&'static str, ColorParams)> {
    let curve = |pts: &[[f32; 2]]| Curve::new(pts).unwrap_or_default();
    let plain = |brightness, contrast, saturation, tint, wheels: Wheels, curves: [Curve; 4]| ColorParams {
        brightness,
        contrast,
        saturation,
        tint,
        look: Look { wheels, curves, ..Default::default() },
    };
    let gain = |master: f32, red: f32, green: f32, blue: f32| Wheels { gain: [master, red, green, blue], ..Default::default() };
    let lift = |master: f32, red: f32, green: f32, blue: f32| Wheels { lift: [master, red, green, blue], ..Default::default() };
    let none = [Curve::default(); 4];
    vec![
        ("Warm", plain(0.0, 1.0, 1.0, 0.3, gain(0.0, 0.06, 0.0, -0.06), none)),
        ("Cool", plain(0.0, 1.0, 1.0, -0.3, gain(0.0, -0.04, 0.0, 0.07), none)),
        (
            "Teal & orange",
            plain(0.0, 1.0, 1.15, 0.0, Wheels { lift: [0.0, -0.02, 0.05, 0.08], gain: [0.0, 0.07, 0.0, -0.05], ..Default::default() }, none),
        ),
        (
            "Bleach bypass",
            plain(
                0.0,
                1.2,
                0.35,
                0.0,
                Wheels::default(),
                [curve(&[[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]), Curve::default(), Curve::default(), Curve::default()],
            ),
        ),
        ("Black & white", plain(0.0, 1.0, 0.0, 0.0, Wheels::default(), none)),
        ("Faded film", plain(0.04, 0.85, 0.75, 0.05, lift(0.1, 0.0, 0.0, 0.03), none)),
        ("High contrast", plain(-0.02, 1.4, 1.1, 0.0, Wheels::default(), none)),
        ("Vintage", plain(0.0, 0.95, 0.6, 0.2, Wheels { lift: [0.06, 0.02, 0.02, 0.0], gain: [0.0, 0.05, 0.0, -0.08], ..Default::default() }, none)),
    ]
}

/// The built-in look called `name` (any case), if there is one.
pub fn builtin(name: &str) -> Option<ColorParams> {
    builtins().into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim())).map(|(_, p)| p)
}

/// Where saved looks live: `$XDG_DATA_HOME/edith/looks`, or `~/.local/share/edith/looks`.
pub fn saved_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("edith").join("looks"))
}

/// The name a look is saved under, trimmed and checked: non-empty, short, no path
/// separators or control characters, not a dot-file, and not a built-in's name.
pub fn valid_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a look needs a name".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!("a look name is at most {MAX_NAME} characters"));
    }
    if name.starts_with('.') || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err(format!("{name:?} has a character a file name cannot"));
    }
    if builtin(name).is_some() {
        return Err(format!("{name} is a built-in look; pick another name"));
    }
    Ok(name.to_string())
}

/// The names of the saved looks, A to Z.
pub fn saved_names() -> Vec<String> {
    let Some(dir) = saved_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == EXT))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// The look called `name`: a built-in first, then a saved one.
pub fn load(name: &str) -> Result<ColorParams, String> {
    if let Some(p) = builtin(name) {
        return Ok(p);
    }
    let path = saved_path(name)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("no look called {name}: {e}"))?;
    from_text(&text).map_err(|e| format!("look {name}: {e}"))
}

/// Saves `params` as the look `name`, replacing a saved look of that name. Returns the file written.
pub fn save(name: &str, params: &ColorParams) -> Result<PathBuf, String> {
    let path = saved_path(name)?;
    let dir = path.parent().ok_or("no folder for looks")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    std::fs::write(&path, to_text(params)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn saved_path(name: &str) -> Result<PathBuf, String> {
    let name = valid_name(name)?;
    let dir = saved_dir().ok_or("no home folder to keep looks in")?;
    Ok(dir.join(format!("{name}.{EXT}")))
}

/// The look file's text for `p`.
pub fn to_text(p: &ColorParams) -> String {
    let w = &p.look.wheels;
    let four = |v: &[f32; 4]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ");
    let mut out = format!(
        "{MAGIC}\nbrightness {}\ncontrast {}\nsaturation {}\ntint {}\nlift {}\ngamma {}\ngain {}\n",
        p.brightness,
        p.contrast,
        p.saturation,
        p.tint,
        four(&w.lift),
        four(&w.gamma),
        four(&w.gain)
    );
    for (name, c) in crate::color::CHANNELS.iter().zip(&p.look.curves) {
        let pts = c.points();
        let body = if pts.is_empty() {
            "-".to_string()
        } else {
            pts.iter().map(|q| format!("{},{}", q[0], q[1])).collect::<Vec<_>>().join(" ")
        };
        out.push_str(&format!("curve {name} {body}\n"));
    }
    if let Some(lut) = p.look.lut {
        out.push_str(&format!("lut {} {}\n", p.look.lut_mix, lut.path().display()));
    }
    out
}

/// The grade a look file's text holds. A bad number, a bad curve or a LUT that
/// no longer loads is refused by name.
pub fn from_text(text: &str) -> Result<ColorParams, String> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some(MAGIC) {
        return Err("not an edith look file".into());
    }
    let mut p = ColorParams::default();
    for line in lines {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        let nums = |n: usize| -> Result<Vec<f32>, String> {
            let v: Vec<f32> = rest
                .split_whitespace()
                .map(|t| t.parse::<f32>().map_err(|_| format!("{key}: {t:?} is not a number")))
                .collect::<Result<_, _>>()?;
            if v.len() != n {
                return Err(format!("{key} takes {n} numbers, not {}", v.len()));
            }
            Ok(v)
        };
        let four = |v: Vec<f32>| [v[0], v[1], v[2], v[3]];
        match key {
            "brightness" => p.brightness = nums(1)?[0],
            "contrast" => p.contrast = nums(1)?[0],
            "saturation" => p.saturation = nums(1)?[0],
            "tint" => p.tint = nums(1)?[0],
            "lift" => p.look.wheels.lift = four(nums(4)?),
            "gamma" => p.look.wheels.gamma = four(nums(4)?),
            "gain" => p.look.wheels.gain = four(nums(4)?),
            "curve" => {
                let (channel, body) = rest.split_once(' ').unwrap_or((rest, "-"));
                let ix = crate::color::CHANNELS
                    .iter()
                    .position(|c| *c == channel)
                    .ok_or_else(|| format!("curve {channel:?} is not master, red, green or blue"))?;
                let pts: Vec<[f32; 2]> = if body.trim() == "-" {
                    Vec::new()
                } else {
                    body.split_whitespace()
                        .map(|pair| {
                            let (x, y) = pair.split_once(',').ok_or_else(|| format!("curve point {pair:?} is not x,y"))?;
                            let f = |s: &str| s.parse::<f32>().map_err(|_| format!("curve point {pair:?} is not x,y"));
                            Ok([f(x)?, f(y)?])
                        })
                        .collect::<Result<_, String>>()?
                };
                p.look.curves[ix] = Curve::new(&pts)?;
            }
            "lut" => {
                let (mix, path) = rest.split_once(' ').ok_or("lut takes a mix and a path")?;
                let mix: f32 = mix.parse().map_err(|_| format!("lut mix {mix:?} is not a number"))?;
                let lut = LutRef::reload(Path::new(path));
                if let Some(why) = lut.problem() {
                    return Err(format!("the LUT {path} does not load: {why}"));
                }
                p.look.lut = Some(lut);
                p.look.lut_mix = mix;
            }
            _ => {}
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_round_trips_through_its_text() {
        let (_, mut p) = builtins().into_iter().find(|(n, _)| *n == "Bleach bypass").expect("a look");
        p.look.wheels.lift = [0.1, -0.05, 0.0, 0.25];
        p.tint = -0.123_456_79;
        let back = from_text(&to_text(&p)).expect("parses");
        assert_eq!(back, p);
    }

}
