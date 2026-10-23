//! Where a project's view was left -- the scale, the scroll and each lane's
//! height -- so a project reopens exactly as it was left (DESIGN §5).
//!
//! One small file per project, in a `views` directory beside the stance
//! splits, named by a hash of the project's canonical path. The project file
//! itself is never touched: a view that fails to write costs the next open its
//! place, and a view that fails to read is no view at all -- never an error.

use crate::*;
use std::io;
use std::path::{Path, PathBuf};

/// First line of every view file. A file that does not start with it is not
/// one of ours -- or is from a format this build does not know -- and loads as
/// no view at all.
const MAGIC: &str = "edith-view 1";

/// The most pixels per second a saved view may ask for. The real zoom stops
/// depend on the bed the timeline is drawn on ([`View::stops`]), which is not
/// known at load; [`View::settled`] applies them on the first frame. This bound
/// only keeps a corrupt number out of the arithmetic until then.
const PPS_MAX_SAVED: f64 = 100_000.;

/// What a project's view looked like when it was last saved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewState {
    pub(crate) pps: f64,
    pub(crate) start: f64,
    /// Lane key ("v1", "a2", "s1") to the height it was dragged to.
    pub(crate) lane_h: Vec<(String, f32)>,
    /// The viewer's zoom, playback quality, guides and mask.
    pub(crate) viewer: crate::viewer_view::Prefs,
    /// The media bins the list shows folded, by bin id (ascending).
    pub(crate) closed_bins: Vec<u32>,
    /// Which Clip-tab sections are open, per clip kind (`video`, `audio`, ...): kind then the
    /// sections' names, so the inspector comes back the way it was left on the next clip of that kind.
    pub(crate) sections: Vec<(String, Vec<String>)>,
    /// Whether audio clips draw their waveforms. On unless the file says `waveforms=off`.
    pub(crate) waveforms: bool,
    /// Whether a ruler scrub while paused plays a short snippet of sound ([`ActionId::ToggleScrubAudio`]).
    pub(crate) scrub_audio: bool,
}

/// What a saved view puts on the [`crate::Player`]. The flags are the part
/// that matters: a restored view must win over the first-import fit, so it
/// reads as the hand's (`panned`) and the sticky fit is cancelled
/// (`fitted_bed`, `fit_pending`). A project with no saved view never reaches
/// this and keeps today's behaviour.
#[derive(Debug, PartialEq)]
pub(crate) struct Restored {
    pub(crate) scale: Scale,
    pub(crate) lane_h: Vec<(String, f32)>,
    pub(crate) panned: bool,
    pub(crate) fitted_bed: Option<f32>,
    pub(crate) fit_pending: bool,
}

pub(crate) fn restored(state: &ViewState) -> Restored {
    Restored {
        scale: Scale {
            pps: state.pps,
            start: state.start,
        },
        lane_h: state.lane_h.clone(),
        panned: true,
        fitted_bed: None,
        fit_pending: false,
    }
}

/// The views directory: `views` beside the keybindings file, as the stance
/// splits file sits beside it.
pub(crate) fn views_dir() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("views")
}

/// The file a project's view lives in. The canonical path keeps one project
/// reached by two spellings on one file; a path that cannot be canonicalised
/// (a project not yet written) is hashed as given.
pub(crate) fn path_for(dir: &Path, project: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    dir.join(format!(
        "{:016x}",
        fnv1a(canon.to_string_lossy().as_bytes())
    ))
}

/// FNV-1a, 64 bit. A file name has to be the same from one launch to the next,
/// which rules out the std hasher (its seed is random per process); FNV is
/// small enough to write here rather than add a dependency for it.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

pub(crate) fn to_text(state: &ViewState) -> String {
    let mut out = format!("{MAGIC}\npps={}\nstart={}\n", state.pps, state.start);
    for (key, h) in &state.lane_h {
        out.push_str(&format!("lane.{key}={h}\n"));
    }
    let v = &state.viewer;
    out.push_str(&format!(
        "zoom={}\nquality={}\nguides={}\nmask={}\n",
        v.zoom.key(),
        v.quality.key(),
        v.guide.key(),
        v.mask.key()
    ));
    if !state.waveforms {
        out.push_str("waveforms=off\n");
    }
    if !state.scrub_audio {
        out.push_str("scrub_audio=off\n");
    }
    if !state.closed_bins.is_empty() {
        let ids: Vec<String> = state.closed_bins.iter().map(u32::to_string).collect();
        out.push_str(&format!("closed_bins={}\n", ids.join(",")));
    }
    for (kind, names) in &state.sections {
        if !names.is_empty() {
            out.push_str(&format!("sections.{kind}={}\n", names.join(",")));
        }
    }
    out
}

/// Parses a view file. Missing or unparsable scale values are corruption and
/// give `None`; a lane line that does not parse is dropped on its own, and an
/// unknown key is skipped so a newer file's extra lines do not lose the rest.
/// The scale is clamped to what a view may hold ([`PPS_MIN`], [`PPS_MAX_SAVED`]
/// and a start at or after the head).
pub(crate) fn from_text(text: &str) -> Option<ViewState> {
    let mut lines = text.lines();
    if lines.next()? != MAGIC {
        return None;
    }
    let (mut pps, mut start) = (None, None);
    let mut lane_h = Vec::new();
    let mut viewer = crate::viewer_view::Prefs::default();
    let mut closed_bins = Vec::new();
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut waveforms = true;
    let mut scrub_audio = true;
    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "pps" => pps = Some(value.parse::<f64>().ok().filter(|v| v.is_finite())?),
            "start" => start = Some(value.parse::<f64>().ok().filter(|v| v.is_finite())?),
            "zoom" => viewer.zoom = crate::viewer_view::Zoom::from_key(value).unwrap_or_default(),
            "quality" => viewer.quality = crate::viewer_view::Quality::from_key(value).unwrap_or_default(),
            "guides" => viewer.guide = crate::viewer_view::Guide::from_key(value).unwrap_or_default(),
            "mask" => viewer.mask = crate::viewer_view::Mask::from_key(value).unwrap_or_default(),
            "waveforms" => waveforms = value != "off",
            "scrub_audio" => scrub_audio = value != "off",
            "closed_bins" => closed_bins = value.split(',').filter_map(|v| v.parse::<u32>().ok()).collect(),
            _ if key.starts_with("sections.") => {
                let kind = &key["sections.".len()..];
                if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphanumeric()) {
                    let names = value
                        .split(',')
                        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))
                        .map(str::to_string)
                        .collect();
                    sections.push((kind.to_string(), names));
                }
            }
            _ => {
                let Some(name) = key.strip_prefix("lane.") else {
                    continue;
                };
                let valid_name =
                    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric());
                if let Ok(h) = value.parse::<f32>()
                    && h.is_finite()
                    && h > 0.
                    && valid_name
                {
                    lane_h.push((name.to_string(), h));
                }
            }
        }
    }
    Some(ViewState {
        pps: pps?.clamp(PPS_MIN, PPS_MAX_SAVED),
        start: start?.max(0.),
        lane_h,
        viewer,
        closed_bins,
        sections,
        waveforms,
        scrub_audio,
    })
}

pub(crate) fn load_in(dir: &Path, project: &Path) -> Option<ViewState> {
    from_text(&std::fs::read_to_string(path_for(dir, project)).ok()?)
}

/// Written to a temporary file and renamed over the old one, so a quit in the
/// middle of the write leaves the previous view or none, never a torn one.
pub(crate) fn save_in(dir: &Path, project: &Path, state: &ViewState) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = path_for(dir, project);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, to_text(state))?;
    std::fs::rename(&tmp, &path)
}

pub(crate) fn load(project: &Path) -> Option<ViewState> {
    load_in(&views_dir(), project)
}

pub(crate) fn save(project: &Path, state: &ViewState) -> io::Result<()> {
    save_in(&views_dir(), project, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory per test, so parallel tests never share a view file.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("edith-view-{}-{name}", std::process::id()))
    }

    fn sample() -> ViewState {
        ViewState {
            pps: 75.5,
            start: 12.25,
            lane_h: vec![("v1".to_string(), 120.), ("a2".to_string(), 64.5)],
            viewer: crate::viewer_view::Prefs {
                zoom: crate::viewer_view::Zoom::P50,
                quality: crate::viewer_view::Quality::Half,
                guide: crate::viewer_view::Guide::Safe,
                mask: crate::viewer_view::Mask::Vertical,
            },
            closed_bins: vec![2, 7],
            sections: vec![("video".to_string(), vec!["color".to_string(), "speed".to_string()])],
            waveforms: false,
            scrub_audio: false,
        }
    }

    #[test]
    fn a_saved_view_loads_back_as_it_was_written() {
        let dir = scratch("round-trip");
        let project = dir.join("cut.edith");
        save_in(&dir, &project, &sample()).expect("save");
        assert_eq!(load_in(&dir, &project), Some(sample()));
        // Written in the one place a later launch will look for it.
        assert!(path_for(&dir, &project).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_view_is_none_and_so_is_a_corrupt_one() {
        let dir = scratch("corrupt");
        let project = dir.join("cut.edith");
        assert_eq!(load_in(&dir, &project), None, "no file, no view");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(path_for(&dir, &project), "edith-view 1\npps=ninety\n").expect("write");
        assert_eq!(load_in(&dir, &project), None, "a bad number is corruption");
        let _ = std::fs::remove_dir_all(&dir);

        // Each of these is a file this build must not read as a view.
        for text in [
            "",
            "edith-view 2\npps=40\nstart=0\n",
            "pps=40\nstart=0\n",
            "edith-view 1\npps=40\n",
            "edith-view 1\nstart=0\n",
            "edith-view 1\npps=NaN\nstart=0\n",
            "edith-view 1\npps=40\nstart=inf\n",
        ] {
            assert_eq!(from_text(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_waveform_switch_is_on_unless_the_view_says_off() {
        let on = from_text("edith-view 1\npps=40\nstart=0\n").expect("parses");
        assert!(on.waveforms, "a view with no line keeps the waveforms on");
        let off = from_text("edith-view 1\npps=40\nstart=0\nwaveforms=off\n").expect("parses");
        assert!(!off.waveforms);
        assert!(on.scrub_audio, "a view with no line scrubs with sound");
        let quiet = from_text("edith-view 1\npps=40\nstart=0\nscrub_audio=off\n").expect("parses");
        assert!(!quiet.scrub_audio);
        assert!(to_text(&off).contains("waveforms=off"));
        assert!(!to_text(&on).contains("waveforms"), "an on view writes nothing for it");
    }

    #[test]
    fn a_damaged_lane_line_costs_only_that_lane() {
        let text =
            "edith-view 1\npps=40\nstart=0\nlane.v1=oops\nlane.a2=50\nlane.x y=3\nfuture=1\n";
        let state = from_text(text).expect("the scale is intact");
        assert_eq!(state.lane_h, vec![("a2".to_string(), 50.)]);
    }

    #[test]
    fn a_loaded_scale_is_clamped_to_what_a_view_may_hold() {
        let state = from_text("edith-view 1\npps=0.001\nstart=-5\n").expect("parses");
        assert_eq!((state.pps, state.start), (PPS_MIN, 0.));
        let state = from_text("edith-view 1\npps=1e300\nstart=3\n").expect("parses");
        assert_eq!(state.pps, PPS_MAX_SAVED);
    }

    #[test]
    fn restoring_a_view_takes_the_hand_and_cancels_the_first_import_fit() {
        let restored = restored(&sample());
        assert_eq!(
            restored.scale,
            Scale {
                pps: 75.5,
                start: 12.25
            }
        );
        assert!(restored.panned, "the restored view is the hand's");
        assert_eq!(restored.fitted_bed, None, "no sticky fit to refit against");
        assert!(
            !restored.fit_pending,
            "a fit waiting for a bed is cancelled"
        );
        assert_eq!(restored.lane_h, sample().lane_h);
    }

    #[test]
    fn the_file_name_is_stable_and_follows_the_project() {
        let dir = Path::new("/nonexistent-views");
        // FNV-1a of the empty string and of "a", the published test vectors.
        assert_eq!(fnv1a(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
        let one = path_for(dir, Path::new("/nonexistent-views/one.edith"));
        assert_eq!(
            one,
            path_for(dir, Path::new("/nonexistent-views/one.edith"))
        );
        assert_ne!(
            one,
            path_for(dir, Path::new("/nonexistent-views/two.edith"))
        );
    }
}
