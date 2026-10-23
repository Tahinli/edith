//! Fuzzing the `.edith` project format: whatever a session can build must
//! save, load back and save again byte for byte; every older dialect must load
//! and upgrade; and a damaged file must be refused by name, never a panic or a
//! hang.
//!
//! Deterministic: every case comes from a seed, so a failure names the seed
//! that reproduces it.
//!
//! ```text
//! cargo test --release -p engine --test project_fuzz -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use engine::color::{ColorKey, ColorParams};
use engine::duck::Duck;
use engine::eq::{Band, BandKind, EqParams};
use engine::export::EncoderSeat;
use engine::limiter::Limiter;
use engine::project::{
    Chapter, Clip, GainKey, LaneKind, Project, Source, Speed, SpeedKey, SubClip, Title, ramp_frames,
};
use engine::scale::FitPolicy;
use engine::scratch::Scratch;
use engine::subtitle;
use engine::text::{Outline, Place, TextStyle};
use engine::tonemap::Preset;
use engine::transform::{KeyEase, OpacityKey, TransformKey, TransformParams};

/// The number of random projects, and of corrupted files, each test builds.
const ITERATIONS: u64 = 2000;

/// xorshift64*: no dependency, and a seed replays it exactly.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero state would stay zero forever.
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + (self.next_u64() % u64::from(hi - lo + 1)) as u32
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next_u64() % 100 < percent
    }

    fn f32_in(&mut self, lo: f32, hi: f32) -> f32 {
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * unit as f32
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.range(0, items.len() as u32 - 1) as usize]
    }
}

/// Text that exercises every escape the format has, plus multi-byte characters.
fn some_text(rng: &mut Rng, max_pieces: u32) -> String {
    const PIECES: &[&str] = &[
        "a", "Z", " ", "\\", "\n", "\r", "%", "\t", "é", "Ω", "ж", "א", "ก", "漢", "😀",
    ];
    let n = rng.range(1, max_pieces);
    (0..n).map(|_| rng.pick(PIECES)).collect()
}

/// The scalars a save takes that a `Project` does not carry.
struct Scalars {
    resolution: (u32, u32),
    fps: Option<f64>,
    proxy: bool,
    auto_proxy: bool,
    encoder: EncoderSeat,
    sample_rate: Option<u32>,
    playhead: u32,
}

fn random_scalars(rng: &mut Rng) -> Scalars {
    Scalars {
        resolution: rng.pick(&[(1920, 1080), (640, 360), (1280, 720), (33, 17)]),
        fps: if rng.chance(80) {
            Some(rng.pick(&[24.0, 29.97, 30.0, 25.0, 23.976]))
        } else {
            None
        },
        proxy: rng.chance(30),
        auto_proxy: rng.chance(50),
        encoder: rng.pick(&[
            EncoderSeat::Auto,
            EncoderSeat::Hardware,
            EncoderSeat::Software,
        ]),
        sample_rate: rng.pick(&[None, Some(44_100), Some(48_000)]),
        playhead: rng.range(0, 400),
    }
}

/// What `PlaybackSession` writes, from the public accessors of a `Project`.
fn save_project(project: &Project, path: &Path, s: &Scalars) -> Vec<u8> {
    let (sources, lanes, eq, color, transform) = project.without_orphan_sources();
    engine::edith::save_flagged(
        path,
        &sources,
        &lanes,
        &project.lane_gains(),
        &project.lane_subs(),
        project.subtitles(),
        &eq,
        &color,
        &transform,
        s.resolution,
        s.fps,
        project.tone(),
        s.proxy,
        s.auto_proxy,
        s.encoder,
        project.limiter(),
        s.sample_rate,
        s.playhead,
        &project.lane_flags(),
        &project.lane_ducks(),
        project.cue_edits(),
        project.cue_adds(),
        project.titles(),
        project.chapters(),
        project.markers(),
        &project.caption_style(),
        &project.lane_voices(),
        &project.lane_names(),
    )
    .expect("save");
    std::fs::read(path).expect("read back the saved file")
}

/// The same save, from what a load gave back. A fixed point of the format is
/// exactly this: whatever `load` returns, `save` writes it the same way again.
fn save_document(doc: &engine::edith::Document, path: &Path) -> Vec<u8> {
    let mut tracks = subtitle::open_all(&doc.subtitles);
    for (track, code) in &doc.sub_langs {
        tracks[*track].language = code.clone();
    }
    engine::edith::save_flagged(
        path,
        &doc.sources,
        &doc.lanes,
        &doc.gains,
        &doc.subs,
        &tracks,
        &doc.eq,
        &doc.color,
        &doc.transform,
        doc.resolution.unwrap_or((1280, 720)),
        doc.fps,
        doc.tone,
        doc.proxy,
        doc.auto_proxy,
        doc.encoder,
        doc.limiter,
        doc.sample_rate,
        doc.playhead,
        &doc.flags,
        &doc.ducks,
        &doc.cue_edits,
        &doc.cue_adds,
        &doc.titles,
        &doc.chapters,
        &doc.markers,
        &doc.caption_style,
        &doc.voices,
        &doc.names,
    )
    .expect("re-save");
    std::fs::read(path).expect("read back the re-saved file")
}

/// A random but well-formed project, built the way a session builds one: a
/// `from_parts` for the edit list, then the public setters for everything else.
/// A setter that refuses a value leaves the project as it was, so the random
/// draws are allowed to be refused.
fn random_project(rng: &mut Rng, dir: &Path) -> Project {
    // Sources: relative names most of the time, the odd spaced, escaped or
    // absolute one.
    let nsrc = rng.range(1, 3);
    let sources: Vec<Source> = (0..nsrc)
        .map(|i| {
            let name = match rng.range(0, 9) {
                0 => format!("/elsewhere/{i} media%25.mov"),
                1 => format!("sub dir/clip {i}\n2.mp4"),
                _ => format!("media_{i}.mp4"),
            };
            Source::new(name, rng.range(0, 2) as usize)
        })
        .collect();

    let eq: Vec<EqParams> = (0..rng.range(0, 3))
        .map(|_| EqParams {
            bands: (0..rng.range(0, 4))
                .map(|_| Band {
                    freq_hz: rng.f32_in(20.0, 20_000.0),
                    gain_db: rng.f32_in(-12.0, 12.0),
                    q: rng.f32_in(0.1, 10.0),
                    kind: rng.pick(&[
                        BandKind::LowShelf,
                        BandKind::Peak,
                        BandKind::HighShelf,
                        BandKind::LowCut,
                        BandKind::HighCut,
                    ]),
                })
                .collect(),
        })
        .collect();
    let color: Vec<ColorParams> = (0..rng.range(0, 3))
        .map(|_| ColorParams {
            brightness: rng.f32_in(-1.0, 1.0),
            contrast: rng.f32_in(0.0, 2.0),
            saturation: rng.f32_in(0.0, 2.0),
            tint: rng.f32_in(-1.0, 1.0),
            ..Default::default()
        })
        .collect();
    let transform: Vec<TransformParams> = (0..rng.range(0, 3))
        .map(|_| random_transform(rng))
        .collect();

    // Lanes, each a run of clips with gaps, sorted and never overlapping.
    let nlanes = rng.range(1, 4);
    let mut next_link = 0;
    let mut lanes: Vec<(LaneKind, Vec<Clip>)> = Vec::new();
    for _ in 0..nlanes {
        let kind = rng.pick(&[LaneKind::Video, LaneKind::Audio]);
        let mut clips = Vec::new();
        // A link is a group id, unique to one lane's clips: the setters refuse
        // two clips of a lane that name the same one.
        let mut cursor = rng.range(0, 20);
        for _ in 0..rng.range(0, 4) {
            let start = cursor + rng.range(0, 15);
            let in_frame = rng.range(0, 200);
            let out_frame = in_frame + rng.range(1, 120);
            let speed = Speed::from_permille(rng.pick(&[1000, 500, 250, 2000, 4000, 1500]));
            // One clip in four carries a speed ramp (format v35): valid keys only,
            // strictly increasing, and the clip's footprint is the ramp's.
            let speed_keys = if rng.chance(25) {
                let mut at = 0;
                (0..rng.range(1, 4))
                    .map(|_| {
                        let key = SpeedKey {
                            at,
                            speed: Speed::from_permille(rng.pick(&[250, 500, 1000, 1500, 2000, 4000])),
                        };
                        at += rng.range(1, 40);
                        key
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let frames = if speed_keys.is_empty() {
                speed.frames(out_frame - in_frame).max(1)
            } else {
                ramp_frames(&speed_keys, out_frame - in_frame)
            };
            let fade_in = rng.range(0, frames);
            clips.push(Clip {
                start,
                in_frame,
                out_frame,
                source: rng.range(0, nsrc - 1) as usize,
                link: if rng.chance(50) {
                    next_link += 1;
                    Some(next_link)
                } else {
                    None
                },
                eq: pick_index(rng, eq.len()),
                color: pick_index(rng, color.len()),
                transform: pick_index(rng, transform.len()),
                fit: rng.pick(&[
                    FitPolicy::Fit,
                    FitPolicy::Fill,
                    FitPolicy::Stretch,
                    FitPolicy::Center,
                ]),
                speed,
                fade_in,
                fade_out: rng.range(0, frames),
                transition_out: rng.range(0, frames),
                transition_kind: Default::default(),
                visualizer: rng.range(0, 255) as u8,
                viz_paint: rng.next_u64(),
                // A picture has no volume of its own, and only a picture is placed
                // by keys: each kind takes the fields its lane can hold.
                gain_db: if kind == LaneKind::Video {
                    0.0
                } else {
                    rng.f32_in(-60.0, 24.0)
                },
                gain_keys: random_gain_keys(rng, frames),
                pan: 0,
                pitch: 0,
                transform_keys: if kind == LaneKind::Video {
                    random_transform_keys(rng, frames)
                } else {
                    Vec::new()
                },
                opacity_keys: if kind == LaneKind::Video {
                    random_opacity_keys(rng, frames)
                } else {
                    Vec::new()
                },
                color_keys: if kind == LaneKind::Video {
                    random_color_keys(rng, frames)
                } else {
                    Vec::new()
                },
                speed_keys,
                reversed: rng.chance(25),
                effects: if kind == LaneKind::Video {
                    random_effects(rng)
                } else {
                    Vec::new()
                },
                stabilise: None, deinterlace: Default::default(), mark: Default::default(),
                angle: Default::default(),
            });
            cursor = start + frames;
        }
        lanes.push((kind, clips));
    }

    let mut project = Project::from_parts(sources, lanes, eq, color, transform)
        .expect("a generated project is well-formed");

    // Lane-level state, through the setters.
    let laned = project.lanes();
    for lane in &laned {
        project.set_lane_gain_db(*lane, rng.f32_in(-60.0, 12.0));
        match lane.kind {
            LaneKind::Video => {
                project.set_lane_hidden(*lane, rng.chance(30));
            }
            LaneKind::Audio => {
                project.set_lane_muted(*lane, rng.chance(30));
                if rng.chance(40) {
                    let under = rng.range(0, 2) as usize;
                    project.set_lane_duck(*lane, Some(Duck::new(under, rng.f32_in(-40.0, 0.0))));
                }
            }
            LaneKind::Subtitle => {}
        }
        project.set_lane_locked(*lane, rng.chance(20));
    }
    project.set_tone(rng.pick(&[Preset::Reference, Preset::Standard, Preset::Vivid]));
    project.set_limiter(Limiter {
        ceiling_db: rng.f32_in(-6.0, 0.0),
        on: rng.chance(50),
    });

    // Titles and chapters, in the frame range the project has.
    let total = project.timeline_frames().max(1);
    for _ in 0..rng.range(0, 3) {
        let frames = rng.range(1, 90);
        let fade_in = rng.range(0, frames);
        let _ = project.add_title(Title {
            start: rng.range(0, total),
            frames,
            text: some_text(rng, 12),
            style: TextStyle {
                size: rng.range(10, 200),
                outline: rng.pick(&[Outline::None, Outline::Thin, Outline::Thick]),
                boxed: rng.chance(50),
                place: rng.pick(&[Place::Bottom, Place::Top, Place::Centre]),
                ..TextStyle::default()
            },
            fade_in,
            fade_out: rng.range(0, frames - fade_in),
            in_anim: Default::default(),
            out_anim: Default::default(),
            anim_frames: 0,
        });
    }
    for _ in 0..rng.range(0, 3) {
        let _ = project.add_chapter(Chapter {
            frame: rng.range(0, total),
            title: some_text(rng, 8),
        });
    }

    // Subtitles: a real SRT on disk, so the cues are the file's own.
    let srt = dir.join("subs.srt");
    std::fs::write(
        &srt,
        "1\n00:00:01,000 --> 00:00:02,500\nhello there\n\n2\n00:00:03,000 --> 00:00:04,000\nsecond cue\n\n",
    )
    .expect("write the srt");
    if rng.chance(70) {
        project = project.with_subtitles(vec![subtitle::open(&srt, None)]);
        project.set_subtitle_language(0, "eng");
        for cue in 0..rng.range(0, 1) as usize {
            project.set_cue_text(0, cue, &some_text(rng, 6));
        }
        if rng.chance(50) {
            let _ = project.add_cue(0, 5_000_000, 6_000_000, &some_text(rng, 4), false);
        }
        if rng.chance(60) {
            let sub_lane = project.add_lane(LaneKind::Subtitle);
            let _ = project.place_sub(
                sub_lane,
                rng.range(0, 100),
                SubClip::whole_track(0, 2_000_000, 25.0),
            );
        }
    }
    project
}

/// `Option<u16>` index into a table of `len`, or none.
/// A random effects stack: up to four effects of any kind, each switched on or off,
/// every value somewhere in its range.
fn random_effects(rng: &mut Rng) -> Vec<engine::effects::Effect> {
    use engine::effects::{Effect, EffectKind};
    let n = rng.range(0, 4) as usize;
    (0..n)
        .map(|_| {
            let kind = rng.pick(&EffectKind::ALL);
            let mut e = Effect::new(kind);
            e.on = rng.chance(70);
            for spec in kind.params() {
                e.set(spec.name, rng.f32_in(spec.min, spec.max)).unwrap();
            }
            e
        })
        .collect()
}

fn pick_index(rng: &mut Rng, len: usize) -> Option<u16> {
    if len == 0 || rng.chance(40) {
        None
    } else {
        Some(rng.range(0, len as u32 - 1) as u16)
    }
}

fn random_transform(rng: &mut Rng) -> TransformParams {
    TransformParams {
        pos_x: rng.f32_in(-1.0, 1.0),
        pos_y: rng.f32_in(-1.0, 1.0),
        scale: rng.f32_in(0.1, 4.0),
        rotate: rng.f32_in(-360.0, 360.0),
        crop_l: rng.f32_in(0.0, 0.5),
        crop_r: rng.f32_in(0.0, 0.5),
        crop_t: rng.f32_in(0.0, 0.5),
        crop_b: rng.f32_in(0.0, 0.5),
        opacity: rng.f32_in(0.0, 1.0),
    }
}

/// Sorted, distinct keys inside a clip of `frames` timeline frames.
fn random_gain_keys(rng: &mut Rng, frames: u32) -> Vec<GainKey> {
    let mut at: Vec<u32> = (0..rng.range(0, 3))
        .map(|_| rng.range(0, frames.saturating_sub(1)))
        .collect();
    at.sort_unstable();
    at.dedup();
    at.into_iter()
        .map(|at| GainKey {
            at,
            db: rng.f32_in(-60.0, 24.0),
            ease: rng.pick(&KeyEase::ALL),
        })
        .collect()
}

fn random_opacity_keys(rng: &mut Rng, frames: u32) -> Vec<OpacityKey> {
    let mut at: Vec<u32> = (0..rng.range(0, 3))
        .map(|_| rng.range(0, frames.saturating_sub(1)))
        .collect();
    at.sort_unstable();
    at.dedup();
    at.into_iter()
        .map(|at| OpacityKey {
            at,
            opacity: rng.f32_in(0.0, 1.0),
            ease: rng.pick(&KeyEase::ALL),
        })
        .collect()
}

fn random_color_keys(rng: &mut Rng, frames: u32) -> Vec<ColorKey> {
    let mut at: Vec<u32> = (0..rng.range(0, 3))
        .map(|_| rng.range(0, frames.saturating_sub(1)))
        .collect();
    at.sort_unstable();
    at.dedup();
    at.into_iter()
        .map(|at| ColorKey {
            at,
            params: ColorParams {
                brightness: rng.f32_in(-1.0, 1.0),
                contrast: rng.f32_in(0.0, 2.0),
                saturation: rng.f32_in(0.0, 2.0),
                tint: rng.f32_in(-1.0, 1.0),
                ..Default::default()
            },
            ease: rng.pick(&KeyEase::ALL),
        })
        .collect()
}

fn random_transform_keys(rng: &mut Rng, frames: u32) -> Vec<TransformKey> {
    let mut at: Vec<u32> = (0..rng.range(0, 2))
        .map(|_| rng.range(0, frames.saturating_sub(1)))
        .collect();
    at.sort_unstable();
    at.dedup();
    at.into_iter()
        .map(|at| TransformKey {
            at,
            params: random_transform(rng),
            ease: rng.pick(&KeyEase::ALL),
        })
        .collect()
}

#[test]
fn random_projects_save_load_and_save_again_byte_for_byte() {
    let scratch = Scratch::dir("ve_project_fuzz_round_trip");
    // How many projects carried each feature, so a generator that stopped
    // drawing one shows up here rather than as a green run that tests nothing.
    let mut seen = [0u32; 9];
    for seed in 0..ITERATIONS {
        let mut rng = Rng::new(seed);
        let project = random_project(&mut rng, &scratch);
        let scalars = random_scalars(&mut rng);
        let path = scratch.join("fuzz.edith");

        let first = save_project(&project, &path, &scalars);
        let doc = engine::edith::load(&path)
            .unwrap_or_else(|e| panic!("seed {seed}: a saved project refused to load: {e}"));

        // Every part of the edit list comes back as it went in. What is saved is
        // what `without_orphan_sources` hands over: its tables hold only what the
        // clips name, so a clip's table index may be renumbered on the way out.
        let (_, saved, saved_eq, saved_color, saved_transform) = project.without_orphan_sources();
        let lanes = project.lanes();
        assert_eq!(doc.lanes.len(), lanes.len(), "seed {seed}: lane count");
        for ((lane, (kind, clips)), (_, saved_clips)) in lanes.iter().zip(&doc.lanes).zip(&saved) {
            assert_eq!(*kind, lane.kind, "seed {seed}: lane kind");
            if lane.kind != LaneKind::Subtitle {
                assert_eq!(
                    format!("{saved_clips:?}"),
                    format!("{clips:?}"),
                    "seed {seed}: the clips of {}",
                    lane.label()
                );
            }
        }
        assert_eq!(
            format!("{saved_eq:?}"),
            format!("{:?}", doc.eq),
            "seed {seed}: equalizers"
        );
        assert_eq!(
            format!("{saved_color:?}"),
            format!("{:?}", doc.color),
            "seed {seed}: colours"
        );
        assert_eq!(
            format!("{saved_transform:?}"),
            format!("{:?}", doc.transform),
            "seed {seed}: transforms"
        );
        assert_eq!(
            format!("{:?}", project.lane_gains()),
            format!("{:?}", doc.gains),
            "seed {seed}: gains"
        );
        assert_eq!(
            format!("{:?}", project.lane_flags()),
            format!("{:?}", doc.flags),
            "seed {seed}: flags"
        );
        assert_eq!(
            format!("{:?}", project.lane_ducks()),
            format!("{:?}", doc.ducks),
            "seed {seed}: ducks"
        );
        assert_eq!(
            format!("{:?}", project.titles()),
            format!("{:?}", doc.titles),
            "seed {seed}: titles"
        );
        assert_eq!(
            format!("{:?}", project.chapters()),
            format!("{:?}", doc.chapters),
            "seed {seed}: chapters"
        );
        assert_eq!(
            format!("{:?}", project.cue_edits()),
            format!("{:?}", doc.cue_edits),
            "seed {seed}: cue edits"
        );
        assert_eq!(
            format!("{:?}", project.cue_adds()),
            format!("{:?}", doc.cue_adds),
            "seed {seed}: cue adds"
        );
        assert_eq!(
            format!("{:?}", project.limiter()),
            format!("{:?}", doc.limiter),
            "seed {seed}: limiter"
        );
        assert_eq!(project.tone(), doc.tone, "seed {seed}: tone");
        assert_eq!(doc.playhead, scalars.playhead, "seed {seed}: playhead");

        for (count, has) in seen.iter_mut().zip([
            !doc.titles.is_empty(),
            !doc.chapters.is_empty(),
            !doc.subtitles.is_empty(),
            !doc.cue_edits.is_empty(),
            !doc.cue_adds.is_empty(),
            doc.ducks.iter().any(|d| !d.is_empty()),
            doc.flags.iter().any(|f| f.hidden || f.muted || f.locked),
            doc.subs.iter().any(|l| !l.is_empty()),
            doc.lanes.iter().any(|(_, c)| {
                c.iter()
                    .any(|c| !c.gain_keys.is_empty() || c.speed.permille() != 1000)
            }),
        ]) {
            *count += u32::from(has);
        }

        // Saving what came back writes the same bytes: a fixed point.
        let second = save_document(&doc, &scratch.join("fuzz-again.edith"));
        assert_eq!(
            String::from_utf8_lossy(&first),
            String::from_utf8_lossy(&second),
            "seed {seed}: the re-save differs from the first save"
        );
    }
    let names = [
        "titles",
        "chapters",
        "subtitles",
        "cue edits",
        "cue adds",
        "ducks",
        "switches",
        "captions",
        "keyed or sped clips",
    ];
    for (name, count) in names.iter().zip(seen) {
        assert!(count > 0, "no generated project had {name}");
        println!("{name}: {count} of {ITERATIONS} projects");
    }
}

/// The lines a version may write, one clip line per lane, for each dialect from
/// v1 up. Built from the same field rules the loader reads, so it is the file
/// that version would have written, not a hand-picked one.
fn synthesize(version: u32) -> String {
    let mut out = format!("edith {version}\nplayhead 3\n");
    if version >= 7 {
        out.push_str("resolution 1280 720\n");
    }
    if version >= 9 {
        out.push_str("fps 30.0\n");
    }
    // Versions 1 and 2 wrote the source without its audio stream.
    if version <= 2 {
        out.push_str("source media.mp4\n");
    } else {
        out.push_str("source 0 media.mp4\n");
    }
    if version == 1 {
        out.push_str("clip 0 30 0\n");
        return out;
    }
    // The clip fields after the first five, in the order the loader reads them,
    // each present from the version that introduced it.
    let mut fields: Vec<&str> = Vec::new();
    if version >= 5 {
        fields.push("-"); // eq
    }
    if version >= 6 {
        fields.push("-"); // colour
    }
    if version >= 7 {
        fields.push("fit");
    }
    if version >= 8 {
        fields.push("1000"); // speed
    }
    if version >= 18 {
        fields.extend(["0", "0"]); // fades
    }
    if version >= 19 {
        fields.push("0"); // transition
    }
    if version >= 20 {
        fields.push("-"); // transform
    }
    if version >= 21 {
        fields.push("0"); // visualizer flags
    }
    if version >= 22 {
        fields.push("0"); // visualizer paint
    }
    // A lane line is `<kind> [<lane>] <start> <in> <out> <source> <link>` and
    // then the fields above; v4 on numbers the lane, and every older one holds
    // one lane of each kind.
    let numbered = if version >= 4 { "1 " } else { "" };
    for kind in ["video", "audio"] {
        let mut line = format!("{kind} {numbered}0 0 30 0 -");
        for field in &fields {
            line.push(' ');
            line.push_str(field);
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[test]
fn every_older_version_loads_and_upgrades() {
    let scratch = Scratch::dir("ve_project_fuzz_versions");
    for version in 1..=engine::edith::NEWEST_VERSION {
        let text = synthesize(version);
        let path = scratch.join(format!("v{version}.edith"));
        std::fs::write(&path, &text).expect("write the old file");

        let doc = engine::edith::load(&path)
            .unwrap_or_else(|e| panic!("v{version} refused to load: {e}\n{text}"));
        assert!(!doc.sources.is_empty(), "v{version}: its source came back");
        assert_eq!(doc.playhead, 3, "v{version}: the playhead came back");
        assert!(
            doc.lanes.iter().any(|(_, clips)| !clips.is_empty()),
            "v{version}: its clip came back"
        );
        // Every clip of an old file plays at real time and holds no grade it did
        // not write, which is what leaving a field out has always meant.
        for (_, clips) in &doc.lanes {
            for clip in clips {
                assert_eq!(clip.in_frame, 0, "v{version}");
                assert_eq!(clip.out_frame, 30, "v{version}");
                if version < 8 {
                    assert_eq!(
                        clip.speed,
                        Speed::from_permille(1000),
                        "v{version}: real time"
                    );
                }
            }
        }

        // An upgraded file is written again as the newest dialect, and it loads
        // to the same document.
        let again_path = scratch.join(format!("v{version}-up.edith"));
        let upgraded = save_document(&doc, &again_path);
        assert!(
            String::from_utf8_lossy(&upgraded).starts_with("edith "),
            "v{version}: the upgrade writes a header"
        );
        let reloaded = engine::edith::load(&again_path)
            .unwrap_or_else(|e| panic!("v{version} upgraded and then refused: {e}"));
        assert_eq!(
            format!("{:?}", doc.lanes),
            format!("{:?}", reloaded.lanes),
            "v{version}: the upgrade keeps the clips"
        );
    }
}

#[test]
fn a_newer_version_is_refused_by_name() {
    let scratch = Scratch::dir("ve_project_fuzz_newer");
    let path = scratch.join("future.edith");
    let future = engine::edith::NEWEST_VERSION + 1;
    std::fs::write(&path, format!("edith {future}\nsource 0 a.mp4\n")).unwrap();
    let err = engine::edith::load(&path).expect_err("a newer file must be refused");
    assert!(
        err.to_string()
            .contains(&format!("unsupported version {future}")),
        "named: {err}"
    );
}

/// Damage that a seed picks: each returns the bytes it leaves behind.
fn corrupt(rng: &mut Rng, good: &[u8]) -> Vec<u8> {
    let mut bytes = good.to_vec();
    match rng.range(0, 6) {
        // Truncate at a random byte offset.
        0 => {
            let at = rng.range(0, bytes.len() as u32) as usize;
            bytes.truncate(at);
        }
        // Flip a few random bytes.
        1 => {
            for _ in 0..rng.range(1, 4) {
                let at = rng.range(0, bytes.len() as u32 - 1) as usize;
                bytes[at] ^= rng.range(1, 255) as u8;
            }
        }
        // Duplicate or delete a random line.
        2 | 3 => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let mut lines: Vec<&str> = text.lines().collect();
            if !lines.is_empty() {
                let at = rng.range(0, lines.len() as u32 - 1) as usize;
                if rng.chance(50) {
                    let dup = lines[at];
                    lines.insert(at, dup);
                } else {
                    lines.remove(at);
                }
            }
            bytes = (lines.join("\n") + "\n").into_bytes();
        }
        // Replace a number with one that is huge, negative, NaN or infinite.
        _ => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let mut spans: Vec<(usize, usize)> = Vec::new();
            let mut start = None;
            for (i, c) in text.char_indices() {
                match (c.is_ascii_digit() || c == '.' || c == '-', start) {
                    (true, None) => start = Some(i),
                    (false, Some(s)) => {
                        spans.push((s, i));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                spans.push((s, text.len()));
            }
            if spans.is_empty() {
                return bytes;
            }
            let (s, e) = spans[rng.range(0, spans.len() as u32 - 1) as usize];
            let junk = rng.pick(&[
                "99999999999999999999999999",
                "-99999999999999999999",
                "-1",
                "NaN",
                "inf",
                "-inf",
                "1e308",
                "-0",
                "4294967296",
                "18446744073709551616",
                "4000000000",
                "4294967295",
                "2147483647",
                "0",
                "1e-45",
                "1.5e38",
                "-3.4e38",
                "0.0000000000000000000000000000001",
                "9223372036854775807",
            ]);
            let mut out = String::new();
            out.push_str(&text[..s]);
            out.push_str(junk);
            out.push_str(&text[e..]);
            bytes = out.into_bytes();
        }
    }
    bytes
}

/// Loads `path` on a worker thread: `Ok(Ok(()))` for a load, `Ok(Err(why))` for
/// a refusal, and `Err(what)` for a panic or a load that does not return in time.
fn load_guarded(path: PathBuf) -> Result<Result<(), String>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(|| engine::edith::load(&path).map(|_| ()));
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(loaded)) => Ok(loaded.map_err(|e| e.to_string())),
        Ok(Err(panic)) => Err(format!(
            "panicked: {}",
            panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "no message".to_string())
        )),
        Err(_) => Err("did not return within 10 s".to_string()),
    }
}

#[test]
fn damaged_files_are_refused_or_loaded_never_panic_or_hang() {
    let scratch = Scratch::dir("ve_project_fuzz_corrupt");
    let path = scratch.join("damaged.edith");
    let mut failures = Vec::new();
    for seed in 0..ITERATIONS {
        let mut rng = Rng::new(seed ^ 0xC0FF_EE00);
        let project = random_project(&mut rng, &scratch);
        let good = save_project(&project, &path, &random_scalars(&mut rng));
        let bytes = corrupt(&mut rng, &good);
        std::fs::write(&path, &bytes).expect("write the damaged file");

        match load_guarded(path.clone()) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                if message.is_empty() {
                    failures.push(format!("seed {seed}: an empty refusal"));
                }
            }
            Err(what) => failures.push(format!("seed {seed}: {what}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A value the setters may be handed by a caller that is not careful: out of
/// every range, huge, negative or not a number at all.
fn wild_f32(rng: &mut Rng) -> f32 {
    match rng.range(0, 9) {
        0 => f32::NAN,
        1 => f32::INFINITY,
        2 => -1.0e30,
        3 => 1.0e30,
        4 => 0.0,
        _ => rng.f32_in(-400.0, 400.0),
    }
}

/// Whatever the setters accept must save to a file that loads: a setter that
/// takes a value the loader would refuse is a project that cannot be saved
/// without losing the work it holds.
#[test]
fn wild_values_the_setters_accept_still_save_and_load() {
    let scratch = Scratch::dir("ve_project_fuzz_wild");
    for seed in 0..ITERATIONS {
        let mut rng = Rng::new(seed ^ 0x5EED_0F51);
        let mut project = random_project(&mut rng, &scratch);
        let lanes = project.lanes();
        for lane in lanes.iter().copied() {
            let len = project.lane(lane).len();
            if len == 0 {
                continue;
            }
            let idx = rng.range(0, len as u32 - 1) as usize;
            match lane.kind {
                LaneKind::Audio => {
                    project.set_clip_gain(lane, idx, wild_f32(&mut rng));
                    project.set_lane_duck(
                        lane,
                        Some(Duck {
                            under: rng.range(0, 4) as usize,
                            depth_db: wild_f32(&mut rng),
                            threshold_db: wild_f32(&mut rng),
                            attack_ms: wild_f32(&mut rng),
                            release_ms: wild_f32(&mut rng),
                        }),
                    );
                }
                LaneKind::Video => {
                    let keys: Vec<TransformKey> = (0..rng.range(0, 300))
                        .map(|_| TransformKey {
                            at: rng.range(0, 500),
                            params: TransformParams {
                                opacity: wild_f32(&mut rng),
                                ..random_transform(&mut rng)
                            },
                            ease: KeyEase::Linear,
                        })
                        .collect();
                    project.set_transform_keys(lane, idx, keys);
                    project.set_transform(
                        lane,
                        idx,
                        Some(TransformParams {
                            scale: wild_f32(&mut rng),
                            ..random_transform(&mut rng)
                        }),
                    );
                }
                LaneKind::Subtitle => {}
            }
            let keys: Vec<GainKey> = (0..rng.range(0, 300))
                .map(|_| GainKey {
                    at: rng.range(0, 500),
                    db: wild_f32(&mut rng),
                    ease: KeyEase::Linear,
                })
                .collect();
            project.set_gain_keys(lane, idx, keys);
            project.set_eq(
                lane,
                idx,
                Some(EqParams {
                    bands: vec![Band {
                        freq_hz: wild_f32(&mut rng),
                        gain_db: wild_f32(&mut rng),
                        q: wild_f32(&mut rng),
                        kind: BandKind::Peak,
                    }],
                }),
            );
            project.set_color(
                lane,
                idx,
                Some(ColorParams {
                    brightness: wild_f32(&mut rng),
                    contrast: wild_f32(&mut rng),
                    saturation: wild_f32(&mut rng),
                    tint: wild_f32(&mut rng),
                    ..Default::default()
                }),
            );
            project.set_lane_gain_db(lane, wild_f32(&mut rng));
        }
        let long: String = std::iter::repeat_n("x", rng.range(0, 600) as usize).collect();
        let _ = project.add_title(Title {
            start: rng.range(0, 1000),
            frames: rng.range(0, 200_000_000),
            text: long.clone(),
            style: TextStyle {
                size: rng.range(0, 400),
                outline: Outline::Thick,
                boxed: true,
                place: Place::Top,
                ..TextStyle::default()
            },
            fade_in: rng.range(0, 1000),
            fade_out: rng.range(0, 1000),
            in_anim: Default::default(),
            out_anim: Default::default(),
            anim_frames: 0,
        });
        let _ = project.add_chapter(Chapter {
            frame: rng.range(0, 1000),
            title: long,
        });
        project.set_limiter(Limiter {
            ceiling_db: wild_f32(&mut rng),
            on: true,
        });

        let scalars = random_scalars(&mut rng);
        let path = scratch.join("wild.edith");
        save_project(&project, &path, &scalars);
        if let Err(e) = engine::edith::load(&path) {
            panic!("seed {seed}: a project the setters accepted refuses to load: {e}");
        }
    }
}

/// A plain clip at `start` of source 0, for the `from_parts` refusals below.
fn plain_clip(start: u32) -> Clip {
    Clip {
        start,
        in_frame: 0,
        out_frame: 30,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::from_permille(1000),
        fade_in: 0,
        fade_out: 0,
        transition_out: 0,
        transition_kind: Default::default(),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
    }
}

/// Regression: `Project::from_parts` used to take a clip the setters refuse --
/// a volume on a picture, placement keys on a sound, a gain outside its range --
/// and the save of it then wrote a file the loader refused. Each is refused at
/// the door now, by the same rule the setter applies.
#[test]
fn from_parts_refuses_what_the_setters_and_the_loader_refuse() {
    let source = || vec![Source::new("a.mp4", 0)];
    let key = TransformKey {
        at: 0,
        params: random_transform(&mut Rng::new(1)),
        ease: KeyEase::Linear,
    };

    let mut video_gain = plain_clip(0);
    video_gain.gain_db = -6.0;
    let err = Project::from_parts(
        source(),
        vec![(LaneKind::Video, vec![video_gain])],
        vec![],
        vec![],
        vec![],
    )
    .expect_err("a picture with a volume must be refused");
    assert!(err.to_string().contains("no volume"), "named: {err}");

    let mut audio_keys = plain_clip(0);
    audio_keys.transform_keys = vec![key];
    let err = Project::from_parts(
        source(),
        vec![(LaneKind::Audio, vec![audio_keys])],
        vec![],
        vec![],
        vec![],
    )
    .expect_err("a sound with placement keys must be refused");
    assert!(err.to_string().contains("placement keys"), "named: {err}");

    let mut loud = plain_clip(0);
    loud.gain_db = 100.0;
    let err = Project::from_parts(
        source(),
        vec![(LaneKind::Audio, vec![loud])],
        vec![],
        vec![],
        vec![],
    )
    .expect_err("a gain past the range must be refused");
    assert!(
        err.to_string().contains("outside its range"),
        "named: {err}"
    );

    // And what they accept, the loader reads back: an audio clip at -6 dB.
    let mut quiet = plain_clip(0);
    quiet.gain_db = -6.0;
    let project = Project::from_parts(
        source(),
        vec![(LaneKind::Audio, vec![quiet])],
        vec![],
        vec![],
        vec![],
    )
    .expect("an audio clip at -6 dB is well-formed");
    let scratch = Scratch::dir("ve_project_fuzz_from_parts");
    let path = scratch.join("quiet.edith");
    save_project(&project, &path, &random_scalars(&mut Rng::new(2)));
    let doc = engine::edith::load(&path).expect("a saved -6 dB clip loads");
    assert_eq!(doc.lanes[0].1[0].gain_db, -6.0);
}
