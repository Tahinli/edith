//! Final Cut Pro XML 1.10 (`.fcpxml`), written for Final Cut Pro, DaVinci
//! Resolve and Premiere. The first video track is the spine: its clips and
//! gaps, back to back at their timeline offsets. Audio clips are connected
//! to the spine element they start inside, on lanes -1, -2 and so on, and
//! markers sit on the element they fall in.

use std::path::PathBuf;
use std::fmt::Write as _;

use super::{Item, Timeline};
use crate::project::LaneKind;

/// Rational seconds for `frames` at `fps`, reduced: `12/5s`, `3s`, `0s`.
fn time(frames: u32, fps: f64) -> String {
    let (scale, ticks) = crate::mux::frame_timing(fps).unwrap_or((fps.round() as u32, 1));
    let (mut n, mut d) = (u64::from(frames) * u64::from(ticks), u64::from(scale));
    if n == 0 {
        return "0s".to_string();
    }
    let g = gcd(n, d);
    n /= g;
    d /= g;
    match d {
        1 => format!("{n}s"),
        _ => format!("{n}/{d}s"),
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The format name Final Cut uses for a canvas: `FFVideoFormat1080p25`.
fn format_name(height: u32, fps: f64) -> String {
    let rate = match fps {
        f if (f - 29.97).abs() < 0.01 => "2997".to_string(),
        f if (f - 23.976).abs() < 0.01 => "2398".to_string(),
        f if (f - 59.94).abs() < 0.01 => "5994".to_string(),
        f => format!("{}", f.round()),
    };
    format!("FFVideoFormat{height}p{rate}")
}

/// One asset: a file the timeline uses, with its id and how far its clips reach.
struct Asset {
    path: PathBuf,
    id: String,
    reach: u32,
    video: bool,
    audio: bool,
}

/// A spine element: a clip or a gap, its timeline span, and its children.
struct Element {
    start: u32,
    end: u32,
    /// Source frame at the element's start, `0` for a gap.
    in_frame: u32,
    /// The opening tag without its closing `>` or `/>`.
    open: String,
    /// A time map, for a clip at a changed speed.
    time_map: String,
    /// Connected clips and markers, in their own lines.
    children: String,
    /// `</asset-clip>` or `</gap>`.
    close: &'static str,
}

impl Element {
    fn render(&self, out: &mut String) {
        if self.children.is_empty() && self.time_map.is_empty() && self.close == "</asset-clip>" {
            let _ = writeln!(out, "            {}/>", self.open);
        } else {
            let _ = writeln!(out, "            {}>{}{}\n            {}", self.open, self.time_map, self.children, self.close);
        }
    }
}

/// The FCPXML document for `t`.
pub fn write(t: &Timeline) -> String {
    let fps = t.fps;
    let (w, h) = t.resolution;
    let format = format_name(h, fps);
    let total = t.frames();

    // Assets, numbered from r2 (r1 is the format), in order of first use.
    let mut assets: Vec<Asset> = Vec::new();
    for track in &t.tracks {
        for item in &track.items {
            let Item::Clip(c) = item else { continue };
            let video = track.kind != LaneKind::Audio;
            let idx = match assets.iter().position(|a| a.path == c.path) {
                Some(i) => i,
                None => {
                    assets.push(Asset {
                        path: c.path.clone(),
                        id: format!("r{}", assets.len() + 2),
                        reach: 0,
                        video: false,
                        audio: false,
                    });
                    assets.len() - 1
                }
            };
            let a = &mut assets[idx];
            a.reach = a.reach.max(c.out_frame);
            a.video |= video;
            a.audio |= !video;
        }
    }
    let asset_id = |path: &PathBuf| assets.iter().find(|a| &a.path == path).map_or("r0", |a| a.id.as_str()).to_string();

    // The spine: the first video track, or one gap spanning everything when there is none.
    let spine_track = t.tracks.iter().find(|tr| tr.kind != LaneKind::Audio);
    let mut spine: Vec<Element> = Vec::new();
    match spine_track {
        Some(track) => {
            let mut at = 0;
            for item in &track.items {
                match item {
                    Item::Gap(n) => {
                        spine.push(gap(at, *n, fps));
                        at += n;
                    }
                    Item::Clip(c) => {
                        let open = format!(
                            "<asset-clip ref=\"{}\" offset=\"{}\" name=\"{}\" start=\"{}\" duration=\"{}\" format=\"r1\"",
                            asset_id(&c.path),
                            time(c.start, fps),
                            esc(&c.file_name()),
                            time(c.in_frame, fps),
                            time(c.frames, fps),
                        );
                        // A changed speed is a time map from the clip's first frame to its last.
                        let time_map = match c.speed.is_normal() {
                            true => String::new(),
                            false => format!(
                                "\n<timeMap><timept time=\"0s\" value=\"{}\" interp=\"linear\"/><timept time=\"{}\" value=\"{}\" interp=\"linear\"/></timeMap>",
                                time(c.in_frame, fps),
                                time(c.frames, fps),
                                time(c.out_frame, fps),
                            ),
                        };
                        spine.push(Element {
                            start: c.start,
                            end: c.start + c.frames,
                            in_frame: c.in_frame,
                            open,
                            time_map,
                            children: String::new(),
                            close: "</asset-clip>",
                        });
                        at = c.start + c.frames;
                    }
                }
            }
        }
        None => spine.push(gap(0, total, fps)),
    }

    // Connected audio: each audio clip hangs off the spine element its start falls in.
    let mut lane_no = 0;
    for track in t.tracks.iter().filter(|tr| tr.kind == LaneKind::Audio) {
        lane_no += 1;
        for item in &track.items {
            let Item::Clip(c) = item else { continue };
            let Some(el) = spine.iter_mut().find(|e| e.start <= c.start && c.start < e.end) else { continue };
            let local = c.start - el.start + el.in_frame;
            let _ = write!(
                el.children,
                "\n<asset-clip ref=\"{}\" lane=\"-{lane_no}\" offset=\"{}\" name=\"{}\" start=\"{}\" duration=\"{}\" audioRole=\"dialogue\"/>",
                asset_id(&c.path),
                time(local, fps),
                esc(&c.file_name()),
                time(c.in_frame, fps),
                time(c.frames, fps),
            );
        }
    }

    // Markers: on the spine element they fall in, in that element's own time.
    for m in &t.markers {
        if let Some(el) = spine.iter_mut().find(|e| e.start <= m.frame && m.frame < e.end) {
            let local = m.frame - el.start + el.in_frame;
            let _ = write!(
                el.children,
                "\n<marker start=\"{}\" duration=\"{}\" value=\"{}\"/>",
                time(local, fps),
                time(1, fps),
                esc(&m.name),
            );
        }
    }

    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\">\n  <resources>\n");
    let _ = writeln!(
        out,
        "    <format id=\"r1\" name=\"{format}\" frameDuration=\"{}\" width=\"{w}\" height=\"{h}\" colorSpace=\"1-1-1 (Rec. 709)\"/>",
        time(1, fps),
    );
    for a in &assets {
        let src = format!("file://{}", a.path.to_string_lossy().replace(' ', "%20"));
        let mut kinds = String::new();
        if a.video {
            kinds.push_str(" format=\"r1\" hasVideo=\"1\"");
        }
        if a.audio {
            kinds.push_str(" hasAudio=\"1\" audioSources=\"1\" audioChannels=\"2\" audioRate=\"48000\"");
        }
        let _ = writeln!(
            out,
            "    <asset id=\"{}\" name=\"{}\" src=\"{}\" start=\"0s\" duration=\"{}\"{kinds}/>",
            a.id,
            esc(&a.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())),
            esc(&src),
            time(a.reach, fps),
        );
    }
    out.push_str("  </resources>\n  <library>\n");
    let _ = writeln!(out, "    <event name=\"edith\">\n      <project name=\"{}\">", esc(&t.name));
    let _ = writeln!(
        out,
        "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">\n          <spine>",
        time(total, fps),
    );
    for el in &spine {
        el.render(&mut out);
    }
    out.push_str("          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");
    out
}

fn gap(at: u32, frames: u32, fps: f64) -> Element {
    Element {
        start: at,
        end: at + frames,
        in_frame: 0,
        open: format!("<gap name=\"Gap\" offset=\"{}\" start=\"0s\" duration=\"{}\"", time(at, fps), time(frames, fps)),
        time_map: String::new(),
        children: String::new(),
        close: "</gap>",
    }
}
