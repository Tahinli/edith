//! The project file: a line of text per thing, and nothing else.
//!
//! ```text
//! edith 15
//! playhead 90
//! resolution 1920 1080
//! fps 30.0
//! tonemap vivid
//! proxy on
//! autoproxy off
//! encoder hardware
//! limiter -1.0 on
//! source 0 test_av.mp4
//! source 1 /elsewhere/test_av2.mp4
//! subtitle - subs.srt
//! subtitle 3 test_av.mp4
//! eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk
//! color 0.1:1.2:0.9:-0.3
//! video 1 0 0 120 0 0 0 0 fit 1000 0 0
//! audio 1 0 0 120 0 0 0 - fit 1000 30 30
//! video 2 120 0 120 1 - - - fill 2000 0 0
//! audio 2
//! sub 1 0 60 0 500000 1500000
//! sub 2
//! gain audio 1 -3.0
//! ```
//!
//! The `fps` line is the rate the timeline was cut at -- the scaffolding
//! source's own unless a rate was picked
//! ([`crate::PlaybackSession::set_frame_rate`]), and the *only* thing that says
//! so once one was, since every clip number in the file is counted in it.
//! Printed so it reads back bit-exactly. A file without one (every dialect
//! before v9) means "whatever the scaffolding source runs at", which is what
//! such a project always was and is an answer only a project holding a video has
//! ([`crate::PlaybackSession::open_project`]).
//!
//! A `subtitle` line is `subtitle <track> <path>`: which track of that file the
//! cues are in -- a Matroska track number, or `-` when the path *is* a subtitle
//! file ([`crate::subtitle`]) -- and then the path, last and escaped for the
//! reason a source's is. Only the reference is written: the cues themselves are
//! read back out of the file on the way in, exactly as a clip's pictures are, so
//! a project file stays an edit list. A subtitle file that has gone missing
//! since the save comes back listed and refused by name rather than dropped,
//! which is what keeps a re-save from losing it.
//!
//! The `tonemap` line is which HDR-to-SDR rendition the project is watched and
//! exported in ([`crate::tonemap::Preset`]), spelled `reference`, `standard` or
//! `vivid`. Left out when it is the default, which is `reference` -- so a
//! project holding no HDR media, and one whose owner never picked, are the bytes
//! they were.
//!
//! The `proxy` line is whether this project is cut on the stand-ins
//! ([`crate::proxy`]) rather than on the films themselves, spelled `on` or
//! `off`. Left out when it is off, which is what every dialect before v12 was.
//! *Which* sources have a stand-in is not written: a proxy is found by the
//! film's own path, length and modification time, so the cache is the only
//! answer and a project file cannot go stale about it.
//!
//! The `autoproxy` line is whether a film this machine cuts slowly gets a
//! stand-in made for it the moment it is imported ([`crate::proxy::wanted`]),
//! spelled `on` or `off`. Left out when it is on, which is what every dialect
//! before v13 did and could not be told not to. With it off an import makes
//! nothing at all, and turning the `proxy` switch on is what asks for the
//! stand-ins this project is missing -- which is the only way to ask, so a
//! project that never wants them never spends a minute of encode on one.
//!
//! The `encoder` line is which seat an export of this project writes its
//! picture with ([`crate::export::EncoderSeat`]), spelled `auto`, `hardware` or
//! `software`. Left out when it is `auto` -- hardware where this machine has a
//! seat and software everywhere else -- which is what every dialect before v14
//! did and could not be told otherwise. It is the project's and not the
//! machine's for the reason the tone map is: a person who picked the software
//! encoder for *this* delivery picked it for every export of it, and a pick
//! that vanished on a reload is a pick nobody could keep.
//!
//! The `limiter` line is the master limiter over the whole mix
//! ([`crate::limiter`]): its ceiling in dBFS and whether it is in circuit,
//! spelled `on` or `off`. Left out when it is the default, which is off.
//!
//! A `gain` line is `gain <kind> <lane> <dB>`: how loud that whole lane plays,
//! everything on it and every frequency of it -- a different thing from a
//! clip's `eq` line, which is one take and one band. It names its lane the way
//! a clip line does and comes *after* the lanes, because a lane is declared by
//! its clips and a gain declares nothing. Only a lane somebody has turned gets
//! one; the rest are at 0 dB, which is where every lane of every dialect before
//! v9 is.
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
//! <color> <fit> <speed> <fade_in> <fade_out> <transition_out>`: which lane the clip is on -- its kind and its 1-based number
//! among the lanes of that kind, the [`crate::project::Lane::label`] a header
//! column shows -- then where the clip sits on the timeline, the half-open
//! source range it plays, the file it plays from, its group id, the eq line it
//! plays through, the colour line it is graded by (`-` for none of those three)
//! and how it meets a project canvas of another shape, spelled `fit`, `fill`,
//! `stretch` or `center` ([`crate::scale::FitPolicy`]), how fast it plays,
//! in thousandths of real time ([`crate::project::Speed`], `1000` for a clip
//! nobody has speeded), and finally its ramp up from silence and ramp down to
//! it, in timeline frames ([`crate::project::Clip::fade_in`],
//! [`crate::project::Clip::fade_out`], `0 0` for a clip nobody has faded), and
//! finally its cross-dissolve into whatever abuts its end
//! ([`crate::project::Clip::transition_out`], `0` for a hard cut).
//! Timeline placement is
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
//! A subtitle lane is a lane like the other two and is written in its place
//! among them, `sub` for its kind: `sub <lane> <start> <frames> <track>
//! <in> <out>` ([`crate::project::SubClip`]) -- where the caption sits on the
//! timeline and how many frames it covers, then which `subtitle` line's words
//! it shows (by position, the first one is track 0) and the half-open window of
//! that track it shows, in *microseconds*, which is the clock the cues
//! themselves are timed in. Two clocks on one line because a subtitle has two:
//! a placement is in timeline frames like everything else, and a window is in
//! the file's own time whatever the timeline runs at. A lane holding nothing
//! has the bare `sub <lane>` line for the reason a video one does, and the
//! track a caption names has to be one the file declared -- a `sub` line naming
//! track 3 of a two-track palette is refused here, as a clip naming a source
//! that is not there is.
//!
//! **Version 17** was this without the clip's two fade fields: such a project
//! holds no clip anyone has faded, which is what a `0 0` pair still means.
//! **Version 15** was this without the caption's group field: such a project
//! holds captions no hand ever grouped, which is every caption there could be.
//! **Version 14** was that without the `sub` lines: such a project holds no
//! subtitle lane at all -- its `subtitle` lines are the palette, and where the
//! words of it were shown was nowhere in the file.
//! **Version 13** was that without the `encoder` line: such a project takes the
//! hardware seat where there is one and the software encoder
//! everywhere else, which is the only choice those projects had.
//! **Version 12** was that without the `autoproxy` line: such a project makes a
//! stand-in for every film that wants one the moment it arrives, which is all
//! any project could do.
//! **Version 11** was that without the `proxy` line: such a project is cut on
//! its films themselves, which is all any project could do.
//! **Version 10** was that without the `tonemap` line: such a project is shown
//! in the reference rendition, which is the only one there was.
//! **Version 9** was that without the `subtitle` lines: such a project shows no
//! subtitles, which is all any project could do.
//! **Version 8** was that without the `fps` and `limiter` lines and without
//! the `gain` ones: such a project mixes every lane at unity, limits nothing,
//! and comes back at the rate its scaffolding source runs at.
//! **Version 7** was that without the clip's speed field -- every clip of such
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
//! nothing, and an older one plays everything at real time, and an older one
//! mixes flat, and an older one shows no subtitles, and an older one is shown
//! in the reference rendition, and an older one places none of the words it
//! names -- and saving any of them writes v18. An older
//! reader refuses a newer file by name.
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
use crate::limiter::Limiter;
use crate::project::{Clip, Lane, LaneKind, Source, Speed, SubClip};
use crate::scale::FitPolicy;
use crate::subtitle::SubtitleTrack;
use crate::transform::TransformParams;

/// What [`save`] writes. Read support goes back to `edith 1`; see the module
/// docs for what those dialects looked like.
const MAGIC: &[u8] = b"edith 20";
const MAGIC_V19: &[u8] = b"edith 19";
const MAGIC_V18: &[u8] = b"edith 18";
const MAGIC_V17: &[u8] = b"edith 17";
const MAGIC_V16: &[u8] = b"edith 16";
const MAGIC_V15: &[u8] = b"edith 15";
const MAGIC_V14: &[u8] = b"edith 14";
const MAGIC_V13: &[u8] = b"edith 13";
const MAGIC_V12: &[u8] = b"edith 12";
const MAGIC_V11: &[u8] = b"edith 11";
const MAGIC_V10: &[u8] = b"edith 10";
const MAGIC_V9: &[u8] = b"edith 9";
const MAGIC_V8: &[u8] = b"edith 8";
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
    /// What is placed on each lane, in the order `lanes` is in and as long as
    /// it -- the `sub` lines. Empty for every video and audio lane, which hold
    /// no [`SubClip`], and for every lane of every dialect before v15, which
    /// placed no words anywhere ([`crate::Project::with_subs`] takes it back).
    pub subs: Vec<Vec<SubClip>>,
    /// The equalizer table [`Clip::eq`] indexes into, in file order. Empty for
    /// every dialect before v5.
    pub eq: Vec<EqParams>,
    /// The colour table [`Clip::color`] indexes into, in file order. Empty for
    /// every dialect before v6.
    pub color: Vec<ColorParams>,
    /// The transform table [`Clip::transform`] indexes into, in file order.
    /// Empty for every dialect before v20.
    pub transform: Vec<TransformParams>,
    /// The project's own picture size. `None` for every dialect before v7 and
    /// for a v7 file that leaves it out, which both mean "source 0's picture".
    pub resolution: Option<(u32, u32)>,
    /// Every lane's own volume in dB, in the order `lanes` is in and as long as
    /// it; `0.0` for a lane nobody has turned, which is every lane of every
    /// dialect before v9.
    pub gains: Vec<f32>,
    /// The master limiter. Off for every dialect before v9, and for a v9 file
    /// that leaves the line out.
    pub limiter: Limiter,
    /// The subtitle tracks, as the file names them: the path the cues are in
    /// and which track of it, which is what [`crate::subtitle::open`] takes.
    /// Reading them is the loader's business, not this module's -- a `.edith`
    /// holds no cues. Empty for every dialect before v10.
    pub subtitles: Vec<(PathBuf, Option<u64>)>,
    /// The timeline's frame rate. `None` for every dialect before v9 and for a
    /// v9 file that leaves it out, which both mean "whatever the scaffolding
    /// source runs at" -- the inference a project of nothing but stills and
    /// songs has no answer for (see
    /// [`crate::PlaybackSession::open_project`]).
    pub fps: Option<f64>,
    /// The HDR-to-SDR rendition every clip on an HDR curve is shown in.
    /// [`crate::tonemap::Preset::Reference`] for every dialect before v11 and
    /// for a v11 file that leaves the line out, which both mean the published
    /// conversion -- the only one those projects had.
    pub tone: crate::tonemap::Preset,
    /// Whether the films are cut on their stand-ins ([`crate::proxy`]). `false`
    /// for every dialect before v12 and for a v12 file that leaves the line
    /// out, which both mean the films themselves -- the only thing those
    /// projects could do.
    pub proxy: bool,
    /// Whether an import makes a stand-in for a film that wants one by itself
    /// ([`crate::proxy`]). `true` for every dialect before v13 and for a v13
    /// file that leaves the line out, which both mean by itself -- the only
    /// thing those projects did.
    pub auto_proxy: bool,
    /// Which encoder an export writes the picture with
    /// ([`crate::export::EncoderSeat`]). `Auto` for every dialect before v14 and
    /// for a v14 file that leaves the line out, which both mean the seat this
    /// machine has -- the only choice those projects offered.
    pub encoder: crate::export::EncoderSeat,
    /// The rate the project's *sound* is mixed and exported at, chosen rather
    /// than probed. `None` for every dialect before v17 and for a v17 file that
    /// leaves the line out, which both mean what they always did: the rate of
    /// the first source that has any ([`crate::PlaybackSession::open_project`]'s
    /// `first_audio_of`). Playback and export both resample every other source
    /// to whichever rate this resolves to, exactly as they already resample a
    /// source shot at another rate than the timeline's -- an explicit choice
    /// here only changes what that timeline's own rate *is*.
    pub sample_rate: Option<u32>,
    pub playhead: u32,
}

/// Writes the project to `path`, atomically. `sources`, `eq` and `color` should
/// already be orphan-free ([`crate::Project::without_orphan_sources`]);
/// `gains` is one dB per lane in the same order the lanes are in
/// ([`crate::Project::lane_gains`]) and `subs` is one list of placements per
/// lane in that same order ([`crate::Project::lane_subs`]).
#[allow(clippy::too_many_arguments)]
pub fn save(
    path: &Path,
    sources: &[Source],
    lanes: &[(LaneKind, Vec<Clip>)],
    gains: &[f32],
    subs: &[Vec<SubClip>],
    subtitles: &[SubtitleTrack],
    eq: &[EqParams],
    color: &[ColorParams],
    transform: &[TransformParams],
    resolution: (u32, u32),
    fps: Option<f64>,
    tone: crate::tonemap::Preset,
    proxy: bool,
    auto_proxy: bool,
    encoder: crate::export::EncoderSeat,
    limiter: Limiter,
    sample_rate: Option<u32>,
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
            f.write_all(&emit(
                &dir, sources, lanes, gains, subs, subtitles, eq, color, transform, resolution,
                fps, tone, proxy, auto_proxy, encoder, limiter, sample_rate, playhead,
            ))?;
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

#[allow(clippy::too_many_arguments)]
fn emit(
    dir: &Path,
    sources: &[Source],
    lanes: &[(LaneKind, Vec<Clip>)],
    gains: &[f32],
    subs: &[Vec<SubClip>],
    subtitles: &[SubtitleTrack],
    eq: &[EqParams],
    color: &[ColorParams],
    transform: &[TransformParams],
    resolution: (u32, u32),
    fps: Option<f64>,
    tone: crate::tonemap::Preset,
    proxy: bool,
    auto_proxy: bool,
    encoder: crate::export::EncoderSeat,
    limiter: Limiter,
    sample_rate: Option<u32>,
    playhead: u32,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(b'\n');
    out.extend_from_slice(format!("playhead {playhead}\n").as_bytes());
    out.extend_from_slice(format!("resolution {} {}\n", resolution.0, resolution.1).as_bytes());
    // `{:?}`, the eq table's rule: the rate a timeline was cut at has to read
    // back as the very number, or every clip on it lands on another frame.
    if let Some(fps) = fps.filter(|f| f.is_finite() && *f > 0.0) {
        out.extend_from_slice(format!("fps {fps:?}\n").as_bytes());
    }
    // Beside it, and by the same rule: a project nobody chose a sound rate for
    // is the mix the source's own probe always gave it, which is what leaving
    // this line out still means.
    if let Some(rate) = sample_rate.filter(|&r| (1_000..=384_000).contains(&r)) {
        out.extend_from_slice(format!("samplerate {rate}\n").as_bytes());
    }
    // Beside the rate, and written only when it is not the default: a project
    // holding no HDR media has nothing to say here, and saying nothing is what
    // a v10 file already said.
    if tone != crate::tonemap::Preset::default() {
        out.extend_from_slice(format!("tonemap {}\n", tone.name()).as_bytes());
    }
    // Beside it and by the same rule: a project cut on the films themselves --
    // every project before there were stand-ins -- says nothing here.
    if proxy {
        out.extend_from_slice(b"proxy on\n");
    }
    // Beside it and by the same rule the other way up: making them by itself is
    // what every project before this line did, so only a project told to stop
    // has anything to say here.
    if !auto_proxy {
        out.extend_from_slice(b"autoproxy off\n");
    }
    // ...and which encoder writes the picture, by the same rule again: `auto`
    // is what every project before this line did, so only a project whose owner
    // picked a seat says anything here.
    if encoder != crate::export::EncoderSeat::default() {
        out.extend_from_slice(format!("encoder {}\n", encoder.name()).as_bytes());
    }
    // Written only when it is not the default, so a project nobody has limited
    // is the same bytes it was in v8 bar the version line.
    if limiter != Limiter::default() {
        out.extend_from_slice(
            format!(
                "limiter {:?} {}\n",
                limiter.ceiling_db,
                match limiter.on {
                    true => "on",
                    false => "off",
                }
            )
            .as_bytes(),
        );
    }
    for s in sources {
        out.extend_from_slice(format!("source {} ", s.audio_stream).as_bytes());
        escape(s.path.strip_prefix(dir).unwrap_or(&s.path), &mut out);
        out.push(b'\n');
    }
    // The subtitle tracks, by reference: which track of which file, and never
    // the cues (see the module docs). Beside the sources because that is what
    // they are -- a file this project reads from -- although nothing indexes
    // them and their order is only the order they were added in.
    for t in subtitles {
        let track = t.track.map_or("-".to_string(), |n| n.to_string());
        out.extend_from_slice(format!("subtitle {track} ").as_bytes());
        escape(t.path.strip_prefix(dir).unwrap_or(&t.path), &mut out);
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
    // ...and the transform table, for the same reason and by the same rules.
    for p in transform {
        out.extend_from_slice(
            format!(
                "transform {:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}\n",
                p.pos_x, p.pos_y, p.scale, p.rotate, p.crop_l, p.crop_r, p.crop_t, p.crop_b
            )
            .as_bytes(),
        );
    }
    // Lane by lane rather than interleaved by time: a lane reads as a list, the
    // parser gets its sortedness check for free from the order it is in, and
    // the lanes come back out in the order they are displayed in.
    let (mut video, mut audio, mut subtitle) = (0, 0, 0);
    for (at, (kind, clips)) in lanes.iter().enumerate() {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => {
                subtitle += 1;
                ("sub", subtitle)
            }
        };
        // A subtitle lane holds no `Clip` at all ([`LaneKind::Subtitle`]): what
        // is on it is in `subs`, one list per lane and in step with this one,
        // and it is written here so the lane comes back in its place among the
        // others.
        if *kind == LaneKind::Subtitle {
            let placed = subs.get(at).map_or(&[][..], Vec::as_slice);
            if placed.is_empty() {
                out.extend_from_slice(format!("{keyword} {ord}\n").as_bytes());
            }
            for s in placed {
                // The group field only when the caption is in one, for the
                // clip line's reason: a project nobody grouped a caption in is
                // the same bytes it was in v15.
                let link = s.link.map_or(String::new(), |l| format!(" {l}"));
                out.extend_from_slice(
                    format!(
                        "{keyword} {ord} {} {} {} {} {}{link}\n",
                        s.start, s.frames, s.track, s.in_us, s.out_us
                    )
                    .as_bytes(),
                );
            }
            continue;
        }
        // A lane is declared by its clips; one holding nothing has to say so on
        // a line of its own, or the lane itself would not survive the round trip.
        if clips.is_empty() {
            out.extend_from_slice(format!("{keyword} {ord}\n").as_bytes());
        }
        for c in clips {
            let link = c.link.map_or("-".to_string(), |l| l.to_string());
            let eq = c.eq.map_or("-".to_string(), |e| e.to_string());
            let color = c.color.map_or("-".to_string(), |e| e.to_string());
            let transform = c.transform.map_or("-".to_string(), |e| e.to_string());
            out.extend_from_slice(
                format!(
                    "{keyword} {ord} {} {} {} {} {link} {eq} {color} {} {} {} {} {} {transform}\n",
                    c.start,
                    c.in_frame,
                    c.out_frame,
                    c.source,
                    fit_name(c.fit),
                    c.speed.permille(),
                    c.fade_in,
                    c.fade_out,
                    c.transition_out,
                )
                .as_bytes(),
            );
        }
    }
    // After the lanes, not before them: a gain line names a lane that has to be
    // there already, and a lane is declared by its own lines. Only the lanes
    // somebody has turned get one.
    let (mut video, mut audio) = (0, 0);
    for ((kind, _), &db) in lanes.iter().zip(gains) {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            // In the list -- the two are zipped, so it has to be -- and never
            // written: a subtitle lane has no volume at all
            // (`Project::set_lane_gain_db` refuses one), so its entry is the
            // `0.0` no lane writes a line for.
            LaneKind::Subtitle => continue,
        };
        if db != 0.0 {
            out.extend_from_slice(format!("gain {keyword} {ord} {db:?}\n").as_bytes());
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
    // The one that carries a per-clip transform (position/scale/rotate/crop)...
    let v20 = first == MAGIC;
    // ...the one that carries a per-clip cross-dissolve into its successor...
    let v19 = v20 || first == MAGIC_V19;
    // ...the one that carries a per-clip fade envelope...
    let v18 = v19 || first == MAGIC_V18;
    // ...the one that carries an explicit sound rate...
    let v17 = v18 || first == MAGIC_V17;
    // ...the one whose captions may carry a group id...
    let v16 = v17 || first == MAGIC_V16;
    // ...the one that places the words it names...
    let v15 = v16 || first == MAGIC_V15;
    // ...the ones that say which encoder an export takes...
    let v14 = v15 || first == MAGIC_V14;
    // ...the ones that say whether the stand-ins are made by themselves...
    let v13 = v14 || first == MAGIC_V13;
    // ...the ones that say whether it is cut on stand-ins...
    let v12 = v13 || first == MAGIC_V12;
    // ...the ones that carry an HDR rendition...
    let v11 = v12 || first == MAGIC_V11;
    // ...the ones that carry subtitle tracks...
    let v10 = v11 || first == MAGIC_V10;
    // ...the ones that carry the mix -- lane volumes, the master limiter and
    // the rate the timeline was cut at...
    let v9 = v10 || first == MAGIC_V9;
    // ...the ones that carry a per-clip speed...
    let v8 = v9 || first == MAGIC_V8;
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
        // Resized to the lanes as they arrive, and once more at the end, so a
        // caller may zip the two exactly as it zips the gains. Nothing before
        // v15 places a word anywhere: those projects have the palette and no
        // subtitle lane to put any of it on.
        subs: Vec::new(),
        // Nothing before v5 equalizes anything, and nothing before v6 grades.
        eq: Vec::new(),
        color: Vec::new(),
        // Nothing before v20 places any clip anywhere but its fit policy's own
        // spot: the identity transform is what leaving a clip's index out
        // still means.
        transform: Vec::new(),
        // Nothing before v7 has a resolution of its own: source 0's picture is
        // what those projects were, and `None` is how the loader is told so.
        resolution: None,
        // ...and nothing before v9 carries a mix or a rate: every lane at unity,
        // no limiter, and the rate inferred from the scaffold as it always was.
        gains: Vec::new(),
        // Nothing before v10 shows a subtitle, and nothing before v11 was shown
        // in any rendition but the published one.
        subtitles: Vec::new(),
        limiter: Limiter::default(),
        tone: crate::tonemap::Preset::default(),
        proxy: false,
        // ...and nothing before v13 could be told *not* to make one: an import
        // that wanted a stand-in got one, which is what leaving the line out
        // still means.
        auto_proxy: true,
        // ...and nothing before v14 could be told *which* encoder: an export
        // took the seat this machine had, which is what leaving the line out
        // still means.
        encoder: crate::export::EncoderSeat::default(),
        fps: None,
        // Nothing before v17 chose a sound rate: the source's own probe is
        // still what the mix is cut at, which is what `None` means everywhere
        // else it reaches (`PlaybackSession::open_project`).
        sample_rate: None,
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
            b"fps" if v9 => {
                if doc.fps.is_some() || !doc.sources.is_empty() {
                    return Err(format!("line {n}: fps belongs once, before the sources").into());
                }
                let f = fields(rest, 1, "fps", n)?;
                let fps = std::str::from_utf8(f[0])
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                    .filter(|f| f.is_finite() && *f > 0.0 && *f <= 1000.0)
                    .ok_or_else(|| {
                        format!(
                            "line {n}: {:?} is not a frame rate",
                            String::from_utf8_lossy(f[0])
                        )
                    })?;
                doc.fps = Some(fps);
            }
            b"samplerate" if v17 => {
                if doc.sample_rate.is_some() || !doc.sources.is_empty() {
                    return Err(format!(
                        "line {n}: samplerate belongs once, before the sources"
                    )
                    .into());
                }
                let f = fields(rest, 1, "samplerate", n)?;
                let rate = number(f[0], n)?;
                if !(1_000..=384_000).contains(&rate) {
                    return Err(format!("line {n}: {rate} Hz is not a sound rate").into());
                }
                doc.sample_rate = Some(rate);
            }
            b"tonemap" if v11 => {
                if !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: tonemap belongs once, before the sources").into(),
                    );
                }
                let f = fields(rest, 1, "tonemap", n)?;
                doc.tone = crate::tonemap::Preset::from_name(f[0]).ok_or_else(|| {
                    format!(
                        "line {n}: {:?} is not a tone map",
                        String::from_utf8_lossy(f[0])
                    )
                })?;
            }
            b"proxy" if v12 => {
                if !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: proxy belongs once, before the sources").into()
                    );
                }
                let f = fields(rest, 1, "proxy", n)?;
                doc.proxy = match f[0] {
                    b"on" => true,
                    b"off" => false,
                    other => {
                        return Err(format!(
                            "line {n}: proxy is on or off, not {:?}",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
            }
            b"autoproxy" if v13 => {
                if !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: autoproxy belongs once, before the sources").into(),
                    );
                }
                let f = fields(rest, 1, "autoproxy", n)?;
                doc.auto_proxy = match f[0] {
                    b"on" => true,
                    b"off" => false,
                    other => {
                        return Err(format!(
                            "line {n}: autoproxy is on or off, not {:?}",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
            }
            b"encoder" if v14 => {
                if !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: encoder belongs once, before the sources").into()
                    );
                }
                let f = fields(rest, 1, "encoder", n)?;
                doc.encoder = crate::export::EncoderSeat::from_name(f[0]).ok_or_else(|| {
                    format!(
                        "line {n}: encoder is auto, hardware or software, not {:?}",
                        String::from_utf8_lossy(f[0])
                    )
                })?;
            }
            b"limiter" if v9 => {
                if !doc.sources.is_empty() {
                    return Err(
                        format!("line {n}: limiter belongs once, before the sources").into(),
                    );
                }
                let f = fields(rest, 2, "limiter", n)?;
                let on = match f[1] {
                    b"on" => true,
                    b"off" => false,
                    other => {
                        return Err(format!(
                            "line {n}: limiter is on or off, not {:?}",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                // Through the same clamp a nudge goes through: a hand-written
                // ceiling above full scale is a ceiling that limits nothing.
                doc.limiter = Limiter {
                    on,
                    ..Limiter::default()
                }
                .with_ceiling(float(f[0], n)?);
            }
            // After the lanes it names, which is where `emit` writes it: a lane
            // is declared by its own lines, and a gain declares nothing.
            b"gain" if v9 => {
                let f = fields(rest, 3, "gain", n)?;
                let kind = match f[0] {
                    b"video" => LaneKind::Video,
                    b"audio" => LaneKind::Audio,
                    other => {
                        return Err(format!(
                            "line {n}: gain names {:?}, not a lane kind",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: gain names a lane that is not there"))?;
                doc.gains.resize(doc.lanes.len(), 0.0);
                doc.gains[at] = float(f[2], n)?;
            }
            // Beside the source lines and read the same way: fields first, the
            // path last, because a path runs to the end of the line.
            b"subtitle" if v10 => {
                let at = rest
                    .iter()
                    .position(|&b| b == b' ')
                    .ok_or_else(|| format!("line {n}: subtitle without a path"))?;
                let track = match &rest[..at] {
                    b"-" => None,
                    field => Some(u64::from(number(field, n)?)),
                };
                let path = unescape(&rest[at + 1..], n)?;
                if path.as_os_str().is_empty() {
                    return Err(format!("line {n}: subtitle without a path").into());
                }
                doc.subtitles.push((
                    // Relative to the project file, as a source path is.
                    if path.is_absolute() {
                        path
                    } else {
                        dir.join(path)
                    },
                    track,
                ));
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
            b"transform" if v20 => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: transform after a clip").into());
                }
                let f = fields(rest, 1, "transform", n)?;
                let parts: Vec<&[u8]> = f[0].split(|&b| b == b':').collect();
                if parts.len() != 8 {
                    return Err(format!(
                        "line {n}: transform wants 8 fields, found {}",
                        parts.len()
                    )
                    .into());
                }
                doc.transform.push(TransformParams {
                    pos_x: float(parts[0], n)?,
                    pos_y: float(parts[1], n)?,
                    scale: float(parts[2], n)?,
                    rotate: float(parts[3], n)?,
                    crop_l: float(parts[4], n)?,
                    crop_r: float(parts[5], n)?,
                    crop_t: float(parts[6], n)?,
                    crop_b: float(parts[7], n)?,
                });
            }
            // v1: one lane, no placement, no groups. Every clip becomes one
            // grouped video+audio pair laid where the queue reached, which is
            // what the file always meant.
            b"clip" if v1 => {
                let f = fields(rest, 3, "clip", n)?;
                let clip = check(
                    Clip {
                        fade_in: 0,
                        fade_out: 0,
                        transition_out: 0,
                        start: queued,
                        in_frame: number(f[0], n)?,
                        out_frame: number(f[1], n)?,
                        source: number(f[2], n)? as usize,
                        link: Some(doc.lanes[0].1.len() as u32),
                        eq: None,
                        color: None,
                        transform: None,
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
            // A lane line like the two below it, in its place among them, and
            // read into `subs` rather than into the clips: the lane holds
            // words, and no media path may ever meet one
            // ([`crate::project::LaneKind::Subtitle`]).
            b"sub" if v15 => {
                let at = rest.iter().position(|&b| b == b' ');
                let ord = number(&rest[..at.unwrap_or(rest.len())], n)?;
                let rest = &rest[at.map_or(rest.len(), |at| at + 1)..];
                let at = lane_of(&mut doc.lanes, LaneKind::Subtitle, ord, n)?;
                doc.subs.resize(doc.lanes.len(), Vec::new());
                // The bare line is the empty lane's whole existence, exactly as
                // it is for a video one.
                if rest.is_empty() {
                    continue;
                }
                // v16 on may carry a sixth field -- the caption's group id, the
                // same spelling a clip's link takes (`-` for none). A v15 line
                // stops at five, and a sixth field in one is a file this parser
                // never wrote.
                let all: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                let ok = all.len() == 5 || (v16 && all.len() == 6);
                if !ok {
                    return Err(format!(
                        "line {n}: sub wants {} fields, found {}",
                        if v16 { "5 or 6" } else { "5" },
                        all.len()
                    )
                    .into());
                }
                let link = match all.get(5) {
                    None => None,
                    Some(f) if *f == b"-" => None,
                    Some(f) => Some(number(f, n)?),
                };
                let f = &all;
                let sub = SubClip {
                    start: number(f[0], n)?,
                    frames: number(f[1], n)?,
                    track: number(f[2], n)? as usize,
                    in_us: micros(f[3], n)?,
                    out_us: micros(f[4], n)?,
                    link,
                };
                // The never-empty invariant at both clocks, which is what
                // [`crate::Project::place_sub`] refuses on the live timeline.
                if sub.frames == 0 || sub.out_us <= sub.in_us || sub.in_us < 0 {
                    return Err(format!(
                        "line {n}: caption at {} is empty: {} frames of [{}, {})",
                        sub.start, sub.frames, sub.in_us, sub.out_us
                    )
                    .into());
                }
                // The palette bound, which is a clip's source bound for words:
                // the `subtitle` lines are written before these and a caption
                // names one of them by position.
                if sub.track >= doc.subtitles.len() {
                    return Err(format!(
                        "line {n}: caption names subtitle track {} of {}",
                        sub.track,
                        doc.subtitles.len()
                    )
                    .into());
                }
                if sub.start.checked_add(sub.frames).is_none() {
                    return Err(format!(
                        "line {n}: caption at {} runs past the last frame there is",
                        sub.start
                    )
                    .into());
                }
                let lane = &mut doc.subs[at];
                if lane.last().is_some_and(|prev| prev.end() > sub.start) {
                    return Err(format!(
                        "line {n}: caption at {} overlaps the one before it, or comes before it",
                        sub.start
                    )
                    .into());
                }
                lane.push(sub);
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
                // v20 grew the transform field, v18 the two fade fields, v19
                // the transition one, v8 the speed one, v7 the fit one, v6
                // the colour one and v5 the eq one; every older dialect ends
                // at the link.
                let want = 5
                    + usize::from(v5)
                    + usize::from(v6)
                    + usize::from(v7)
                    + usize::from(v8)
                    + 2 * usize::from(v18)
                    + usize::from(v19)
                    + usize::from(v20);
                let f = fields(rest, want, "clip", n)?;
                let in_frame = number(f[1], n)?;
                let out_frame = number(f[2], n)?;
                let speed = speed_of(f.get(8).copied(), n)?;
                // Clamped to the clip's own timeline length,
                // [`Project::set_fade_in`]'s rule: a crafted or hand-edited
                // file asking for more ramp than the clip has frames gets the
                // clip's whole length rather than a refusal.
                let frames = speed.frames(out_frame.saturating_sub(in_frame));
                let clip = check(
                    Clip {
                        fade_in: number(f.get(9).copied().unwrap_or(b"0"), n)?.min(frames),
                        fade_out: number(f.get(10).copied().unwrap_or(b"0"), n)?.min(frames),
                        transition_out: number(f.get(11).copied().unwrap_or(b"0"), n)?.min(frames),
                        start: number(f[0], n)?,
                        in_frame,
                        out_frame,
                        source: number(f[3], n)? as usize,
                        link: match f[4] {
                            b"-" => None,
                            field => Some(number(field, n)?),
                        },
                        eq: table_index(f.get(5).copied(), doc.eq.len(), "eq", n)?,
                        color: table_index(f.get(6).copied(), doc.color.len(), "color", n)?,
                        transform: table_index(
                            f.get(12).copied(),
                            doc.transform.len(),
                            "transform",
                            n,
                        )?,
                        fit: fit_policy(f.get(7).copied(), n)?,
                        speed,
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
    // One per lane whatever the file said, so a caller may zip the two -- and
    // the same for the placements, whose lanes are the very same list.
    doc.gains.resize(doc.lanes.len(), 0.0);
    doc.subs.resize(doc.lanes.len(), Vec::new());
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

/// A `sub` line's window field: microseconds, which is the clock a cue is timed
/// in ([`crate::subtitle::Cue`]) and the one thing in this format counted in
/// something other than frames. Signed as the cues are, and the negative half
/// is refused by the never-empty check at the line, not here.
fn micros(field: &[u8], line: usize) -> crate::Result<i64> {
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

    /// [`super::emit`] for a project whose mix is flat: every lane at unity, no
    /// limiter, no rate written. Shadows the real one for the tests that were
    /// written before there was a mix to write -- the dialect they are about is
    /// the one this emits, and a v9 file with flat settings is a v8 file bar
    /// its version line, which is exactly what they assert.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        dir: &Path,
        sources: &[Source],
        lanes: &[(LaneKind, Vec<Clip>)],
        eq: &[EqParams],
        color: &[ColorParams],
        resolution: (u32, u32),
        playhead: u32,
    ) -> Vec<u8> {
        super::emit(
            dir,
            sources,
            lanes,
            &[],
            &[],
            &[],
            eq,
            color,
            resolution,
            None,
            crate::tonemap::Preset::default(),
            false,
            true,
            crate::export::EncoderSeat::default(),
            Limiter::default(),
            None,
            playhead,
        )
    }

    /// A subtitle track as a save hands one over: the cues are not written and
    /// so are not here either.
    fn subtitle(path: &str, track: Option<u64>) -> SubtitleTrack {
        SubtitleTrack {
            path: PathBuf::from(path),
            track,
            language: String::new(),
            name: String::new(),
            label: String::new(),
            cues: Vec::new(),
            bitmap: false,
            refused: None,
        }
    }

    /// The v10 line: which track of which file, written beside the sources and
    /// read back as the pair a load opens. And what a v9 file is -- one with no
    /// subtitles at all, which still opens and re-saves as v10.
    #[test]
    fn subtitles_round_trip_as_references_and_a_v9_file_has_none() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let tracks = [
            subtitle("/proj/subs.srt", None),
            subtitle("/proj/a.mp4", Some(3)),
            subtitle("/elsewhere/od d name.ass", None),
        ];
        let bytes = super::emit(
            &dir,
            &sources,
            &lanes,
            &[],
            &[],
            &tracks,
            &[],
            &[],
            (1280, 720),
            None,
            crate::tonemap::Preset::default(),
            false,
            true,
            crate::export::EncoderSeat::default(),
            Limiter::default(),
            None,
            0,
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains(
                "subtitle - subs.srt\n\
                 subtitle 3 a.mp4\n\
                 subtitle - /elsewhere/od d name.ass\n"
            ),
            "the path is relative where it can be, and runs to the end of its \
             line, spaces and all: {text}"
        );
        let back = parse(&bytes, &dir).expect("v10 parses");
        assert_eq!(
            back.subtitles,
            vec![
                (PathBuf::from("/proj/subs.srt"), None),
                (PathBuf::from("/proj/a.mp4"), Some(3)),
                (PathBuf::from("/elsewhere/od d name.ass"), None),
            ]
        );

        // A v9 file is a project with no subtitles, and re-saving it writes v10.
        let v9 = b"edith 9\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                   video 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v9, &dir).expect("v9 still loads");
        assert_eq!(old.subtitles, Vec::new());
        assert!(flat(&dir, &old.sources, &old.lanes, old.playhead).starts_with(b"edith 19\n"));
        // ...and the line itself is not a v9 line: a dialect may not be mixed.
        let mixed = parse(b"edith 9\nsource 0 a.mp4\nsubtitle - subs.srt\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"subtitle\"");
        // A subtitle line with no path is refused, as a source with none is.
        for line in ["subtitle -\n", "subtitle - \n", "subtitle x subs.srt\n"] {
            let file = format!("edith 10\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not a subtitle line"
            );
        }
    }

    /// The caption's group field (v16): written only when the caption is in a
    /// group -- so a project nobody grouped a caption in is the same bytes it
    /// was in v15 -- read back as the id, and refused in an older dialect,
    /// which never wrote one.
    #[test]
    fn a_caption_group_round_trips_and_an_old_dialect_refuses_the_field() {
        let dir = PathBuf::from("/proj");
        let caption = |start: u32, frames: u32, link| SubClip {
            start,
            frames,
            track: 0,
            in_us: 0,
            out_us: i64::from(frames) * 1_000_000,
            link,
        };
        let lanes = vec![
            (LaneKind::Video, vec![clip(0, 0, 30, 0, Some(7))]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0, Some(7))]),
            (LaneKind::Subtitle, Vec::new()),
        ];
        let subs = vec![vec![], vec![], vec![caption(0, 30, Some(7)), caption(40, 10, None)]];
        let bytes = super::emit(
            &dir,
            &doc().1,
            &lanes,
            &[],
            &subs,
            &[subtitle("/proj/subs.srt", None)],
            &[],
            &[],
            (1280, 720),
            None,
            crate::tonemap::Preset::default(),
            false,
            true,
            crate::export::EncoderSeat::default(),
            Limiter::default(),
            None,
            0,
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("sub 1 0 30 0 0 30000000 7\n"),
            "the grouped caption carries its id: {text}"
        );
        assert!(
            text.contains("sub 1 40 10 0 0 10000000\n"),
            "a caption in no group writes no field: {text}"
        );
        let back = parse(&bytes, &dir).expect("v16 parses");
        assert_eq!(back.subs[2][0].link, Some(7));
        assert_eq!(back.subs[2][1].link, None);

        // The older dialect never wrote a sixth field, and one claiming to be
        // it is a file this parser refuses by name.
        let v15 = parse(
            b"edith 15\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\nsubtitle - subs.srt\n\
               sub 1 0 30 0 0 30000000 7\n",
            &dir,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(v15, "line 6: sub wants 5 fields, found 6");
    }

    /// The v12 line: whether the project is cut on the stand-ins, written only
    /// when it is -- and the dialect before it, which had no stand-ins and
    /// whose bytes are therefore unchanged bar the version.
    #[test]
    fn the_proxy_switch_round_trips_and_a_v11_file_is_cut_on_its_films() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let bytes = |proxy| {
            super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                &[],
                &[],
                &[],
                (1280, 720),
                None,
                crate::tonemap::Preset::default(),
                proxy,
                true,
                crate::export::EncoderSeat::default(),
                Limiter::default(),
                None,
                0,
            )
        };
        let on = bytes(true);
        assert!(
            String::from_utf8_lossy(&on).contains("proxy on\n"),
            "{}",
            String::from_utf8_lossy(&on)
        );
        assert!(parse(&on, &dir).expect("v12 parses").proxy);
        let off = bytes(false);
        assert!(
            !String::from_utf8_lossy(&off).contains("proxy"),
            "off is the line left out"
        );
        assert!(!parse(&off, &dir).expect("v12 parses").proxy);

        // A v11 project is one cut on the films themselves, and it still loads.
        let v11 = b"edith 11\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v11, &dir).expect("v11 still loads");
        assert!(!old.proxy);
        assert_eq!(old.sources.len(), 1, "the rest of it came back too");
        // ...and the line is not a v11 line: a dialect may not be mixed.
        let mixed = parse(b"edith 11\nsource 0 a.mp4\nproxy on\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"proxy\"");
        // Anything but on or off is a corrupt line, by name.
        for line in ["proxy\n", "proxy yes\n", "proxy On\n"] {
            let file = format!("edith 13\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not a proxy line"
            );
        }
    }

    /// The v13 line: whether an import makes a stand-in by itself. Written the
    /// other way up from every other switch here -- on is the default and the
    /// line left out -- because that is what every dialect before it did, and a
    /// v12 file must come back doing it.
    #[test]
    fn the_auto_proxy_switch_round_trips_and_a_v12_file_makes_them_by_itself() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let bytes = |proxy, auto| {
            super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                &[],
                &[],
                &[],
                (1280, 720),
                None,
                crate::tonemap::Preset::default(),
                proxy,
                auto,
                crate::export::EncoderSeat::default(),
                Limiter::default(),
                None,
                0,
            )
        };
        // The default is the line left out, and the two switches are two lines:
        // a project cut on stand-ins it does not make writes both.
        let on = bytes(false, true);
        assert!(
            !String::from_utf8_lossy(&on).contains("autoproxy"),
            "on is the line left out: {}",
            String::from_utf8_lossy(&on)
        );
        assert!(parse(&on, &dir).expect("v13 parses").auto_proxy);
        let off = bytes(true, false);
        let text = String::from_utf8_lossy(&off).to_string();
        assert!(text.contains("proxy on\nautoproxy off\n"), "{text}");
        let back = parse(&off, &dir).expect("v13 parses");
        assert!(back.proxy, "the other switch came back too");
        assert!(!back.auto_proxy);

        // A v12 project made them by itself and could not be told otherwise.
        let v12 = b"edith 12\nplayhead 0\nresolution 1280 720\nproxy on\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v12, &dir).expect("v12 still loads");
        assert!(old.auto_proxy, "a v12 project made its stand-ins by itself");
        assert!(old.proxy, "the rest of it came back too");
        // ...and the line is not a v12 line: a dialect may not be mixed.
        let mixed = parse(b"edith 12\nsource 0 a.mp4\nautoproxy off\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"autoproxy\"");
        // Anything but on or off is a corrupt line, by name.
        for line in ["autoproxy\n", "autoproxy no\n", "autoproxy Off\n"] {
            let file = format!("edith 13\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not an autoproxy line"
            );
        }
    }

    /// The v14 line: which encoder an export writes the picture with, by name,
    /// read back as the very seat -- and the dialect before it, which had no
    /// way to say one and took whatever this machine had.
    #[test]
    fn the_encoder_seat_round_trips_by_name_and_a_v13_file_takes_the_machines() {
        use crate::export::EncoderSeat;
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let bytes = |seat| {
            super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                &[],
                &[],
                &[],
                (1280, 720),
                None,
                crate::tonemap::Preset::default(),
                false,
                true,
                seat,
                Limiter::default(),
                None,
                0,
            )
        };
        // The default is the line left out, so a project nobody has picked a
        // seat for is the bytes a v13 file was bar the version line.
        let auto = bytes(EncoderSeat::Auto);
        assert!(
            !String::from_utf8_lossy(&auto).contains("encoder"),
            "auto is the line left out: {}",
            String::from_utf8_lossy(&auto)
        );
        assert_eq!(
            parse(&auto, &dir).expect("v14 parses").encoder,
            EncoderSeat::Auto
        );
        for seat in [EncoderSeat::Hardware, EncoderSeat::Software] {
            let written = bytes(seat);
            let text = String::from_utf8_lossy(&written).to_string();
            assert!(text.contains(&format!("encoder {}\n", seat.name())), "{text}");
            assert_eq!(parse(&written, &dir).expect("v14 parses").encoder, seat);
        }

        // A v13 project took the seat this machine had and could not be told
        // otherwise.
        let v13 = b"edith 13\nplayhead 0\nresolution 1280 720\nautoproxy off\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v13, &dir).expect("v13 still loads");
        assert_eq!(
            old.encoder,
            EncoderSeat::Auto,
            "a v13 project exported on whatever seat there was"
        );
        assert!(!old.auto_proxy, "the rest of it came back too");
        // ...and the line is not a v13 line: a dialect may not be mixed.
        let mixed = parse(b"edith 13\nsource 0 a.mp4\nencoder software\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"encoder\"");
        // Anything but the three names is a corrupt line, by name.
        for line in ["encoder\n", "encoder gpu\n", "encoder Hardware\n"] {
            let file = format!("edith 15\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not an encoder line"
            );
        }
    }

    /// The v17 line: an explicit sound rate, written only when picked and read
    /// back as the very number -- and a v16 file, which had no way to say one
    /// and meant the source's own probe, still opens with `None`.
    #[test]
    fn the_sample_rate_round_trips_and_a_v16_file_leaves_it_derived() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let bytes = |rate| {
            super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                &[],
                &[],
                &[],
                (1280, 720),
                None,
                crate::tonemap::Preset::default(),
                false,
                true,
                crate::export::EncoderSeat::default(),
                Limiter::default(),
                rate,
                0,
            )
        };
        // Unset is the line left out, so a project nobody has chosen a rate for
        // is the bytes a v16 file was bar the version line.
        let unset = bytes(None);
        assert!(
            !String::from_utf8_lossy(&unset).contains("samplerate"),
            "no pick is the line left out: {}",
            String::from_utf8_lossy(&unset)
        );
        assert_eq!(parse(&unset, &dir).expect("v17 parses").sample_rate, None);
        for rate in [44_100, 48_000, 96_000] {
            let written = bytes(Some(rate));
            let text = String::from_utf8_lossy(&written).to_string();
            assert!(text.contains(&format!("samplerate {rate}\n")), "{text}");
            assert_eq!(
                parse(&written, &dir).expect("v17 parses").sample_rate,
                Some(rate)
            );
        }
        // A v16 project had no rate of its own to say: the field comes back
        // `None`, which is what "derive it from the source" already meant.
        let v16 = b"edith 16\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\n";
        assert_eq!(parse(v16, &dir).expect("v16 still loads").sample_rate, None);
        // ...and the line is not a v16 line: a dialect may not be mixed.
        let mixed = parse(b"edith 16\nsource 0 a.mp4\nsamplerate 48000\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"samplerate\"");
        // A number out of a sound rate's range is a corrupt line, by name.
        for line in ["samplerate 0\n", "samplerate 500000\n", "samplerate nope\n"] {
            let file = format!("edith 18\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not a sound rate line"
            );
        }
    }

    /// The v11 line: the HDR rendition, written by name, read back as the very
    /// preset -- and the dialect before it, which had no way to say one and
    /// meant the published conversion.
    #[test]
    fn the_tone_map_round_trips_by_name_and_a_v10_file_is_the_reference_one() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        for preset in crate::tonemap::Preset::ALL {
            let bytes = super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                &[],
                &[],
                &[],
                (1280, 720),
                None,
                preset,
                false,
                true,
                crate::export::EncoderSeat::default(),
                Limiter::default(),
                None,
                0,
            );
            let text = String::from_utf8_lossy(&bytes).to_string();
            assert_eq!(
                text.contains(&format!("tonemap {}\n", preset.name())),
                preset != crate::tonemap::Preset::default(),
                "the default is the line left out: {text}"
            );
            assert_eq!(parse(&bytes, &dir).expect("v11 parses").tone, preset);
        }

        // A v10 file is a project shown the published way, and re-saving it
        // writes v11 with nothing more in it.
        let v10 = b"edith 10\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v10, &dir).expect("v10 still loads");
        assert_eq!(old.tone, crate::tonemap::Preset::Reference);
        // ...and the line itself is not a v10 line: a dialect may not be mixed.
        let mixed = parse(b"edith 10\nsource 0 a.mp4\ntonemap vivid\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"tonemap\"");
        // A rendition nobody wrote is a corrupt line, by name -- not a silent
        // fall back to another picture.
        for line in ["tonemap\n", "tonemap bright\n", "tonemap Vivid\n"] {
            let file = format!("edith 11\nsource 0 a.mp4\n{line}");
            assert!(
                parse(file.as_bytes(), &dir).is_err(),
                "{line:?} is not a tonemap line"
            );
        }
    }

    fn clip(start: u32, in_frame: u32, out_frame: u32, source: usize, link: Option<u32>) -> Clip {
        Clip {
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
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
            "edith 19\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0\n",
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
            "edith 19\nplayhead 7\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 4 - - fit 1000 0 0 0\naudio 1\n\
             video 2 40 0 10 0 - - - fit 1000 0 0 0\naudio 2 0 0 30 0 4 - - fit 1000 0 0 0\n",
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
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    eq: Some(1),
                    ..clip(30, 10, 20, 0, None)
                },
            ],
            vec![
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    eq: Some(2),
                    ..clip(30, 0, 10, 0, None)
                },
            ],
        );
        let bytes = emit(&dir, &sources, &lanes, &eq, &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk\n\
             eq 16777215.0:-0.1:3.918315e-39:hs\n\
             eq\n\
             video 1 0 0 30 0 0 0 - fit 1000 0 0 0\nvideo 1 30 10 20 0 - 1 - fit 1000 0 0 0\n\
             audio 1 0 0 30 0 0 0 - fit 1000 0 0 0\naudio 1 30 0 10 0 - 2 - fit 1000 0 0 0\n",
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
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    color: Some(1),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(30, 10, 20, 0, None)
                },
            ],
            vec![
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
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
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             color 0.1:1.2:0.9:-0.3\n\
             color -1e-7:16777215.0:3.918315e-39:-0.0\n\
             color 0.0:1.0:1.0:0.0\n\
             video 1 0 0 30 0 0 - 0 fit 1000 0 0 0\nvideo 1 30 10 20 0 - - 1 fit 1000 0 0 0\n\
             audio 1 0 0 30 0 0 - 0 fit 1000 0 0 0\naudio 1 30 0 10 0 - - 2 fit 1000 0 0 0\n",
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
            "edith 19\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls\n\
             video 1 0 0 30 0 0 0 - fit 1000 0 0 0\naudio 1 0 0 30 0 0 - - fit 1000 0 0 0\n"
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
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
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
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 2000 0 0 0\nvideo 1 15 30 40 0 - - - fit 250 0 0 0\n\
             audio 1 0 0 30 0 - - - fit 2000 0 0 0\n",
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
            "edith 19\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0\naudio 1 0 0 30 0 0 - - fit 1000 0 0 0\n"
        );
        // A rate outside what the editor can set is a corrupt line, by name.
        let bad = b"edith 8\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 9000\n";
        assert_eq!(
            parse(bad, &dir).unwrap_err().to_string(),
            "line 3: speed 9000 is outside 250-4000 thousandths"
        );
    }

    #[test]
    fn a_v17_file_loads_with_zero_fades_and_fades_round_trip_bit_exactly() {
        let dir = PathBuf::from("/proj");
        // The v17 dialect: nine clip fields, the last the speed -- no fade
        // pair after it at all, not even zeroes.
        let v17 = b"edith 17\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000\naudio 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v17, &dir).expect("v17 parses");
        assert!(
            old.lanes
                .iter()
                .flat_map(|(_, clips)| clips)
                .all(|c| c.fade_in == 0 && c.fade_out == 0),
            "nothing before v18 had a fade to name"
        );

        // ...and the dialect that does: nonzero fades on both edges, written
        // and read back as the very numbers.
        let sources = vec![Source::new("/proj/a.mp4", 0)];
        let faded = Clip {
            fade_in: 5,
            fade_out: 7,
            transition_out: 0,
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        let lanes = vec![(LaneKind::Video, vec![faded])];
        let bytes = emit(&dir, &sources, &lanes, &[], &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 5 7 0\n",
            "the fades are the clip line's last two fields"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "the fades round trip as the very numbers");
    }

    /// The whole of the v19 bump, the fade test's twin: a clip's dissolve into
    /// its successor is written as the clip line's own last field, comes back
    /// as the very number that was set, and a v18 file -- which has no such
    /// field -- loads with every clip's [`Clip::transition_out`] at zero and
    /// re-saves saying so.
    #[test]
    fn a_v18_file_loads_with_no_transition_and_transitions_round_trip_bit_exactly() {
        let dir = PathBuf::from("/proj");
        // The v18 dialect: eleven clip fields, the last the fade-out -- no
        // transition field after it at all, not even a zero.
        let v18 = b"edith 18\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0\n";
        let old = parse(v18, &dir).expect("v18 parses");
        assert!(
            old.lanes
                .iter()
                .flat_map(|(_, clips)| clips)
                .all(|c| c.transition_out == 0),
            "nothing before v19 had a dissolve to name"
        );

        // ...and the dialect that does: a nonzero dissolve, written and read
        // back as the very number.
        let sources = vec![Source::new("/proj/a.mp4", 0)];
        let dissolving = Clip {
            fade_in: 0,
            fade_out: 0,
            transition_out: 9,
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
        };
        let lanes = vec![(LaneKind::Video, vec![dissolving])];
        let bytes = emit(&dir, &sources, &lanes, &[], &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 9\n",
            "the dissolve is the clip line's own last field"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(
            back.lanes, lanes,
            "the dissolve round trips as the very number"
        );
    }

    /// The whole of the v9 bump, the speed test's twin: the mix a project was
    /// left at -- every track's own volume and the master limiter -- plus the
    /// rate it was cut at, all written, all read back as the very numbers, and
    /// a v8 file (which has none of them) loading flat and unlimited.
    #[test]
    fn the_mix_and_the_rate_round_trip_and_a_v8_file_is_flat() {
        let dir = PathBuf::from("/proj");
        let sources = vec![Source::new("/proj/a.mp4", 0)];
        let lanes = vec![
            (LaneKind::Video, vec![clip(0, 0, 30, 0, None)]),
            (LaneKind::Audio, vec![clip(0, 0, 30, 0, None)]),
            // A second audio track, turned down: the gain lines are per lane,
            // so the two must not run into one another.
            (LaneKind::Audio, vec![clip(0, 0, 30, 0, None)]),
        ];
        let limiter = Limiter {
            ceiling_db: -1.5,
            on: true,
        };
        let bytes = super::emit(
            &dir,
            &sources,
            &lanes,
            &[0.0, 3.0, -6.5],
            &[],
            &[],
            &[],
            &[],
            (1280, 720),
            Some(23.976023976023978),
            crate::tonemap::Preset::default(),
            false,
            true,
            crate::export::EncoderSeat::default(),
            limiter,
            None,
            0,
        );
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 19\nplayhead 0\nresolution 1280 720\nfps 23.976023976023978\n\
             limiter -1.5 on\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0\n\
             audio 2 0 0 30 0 - - - fit 1000 0 0 0\n\
             gain audio 1 3.0\ngain audio 2 -6.5\n",
            "the mix is lines of its own, the gains after the lanes they name"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes);
        assert_eq!(back.gains, vec![0.0, 3.0, -6.5], "one per lane, in order");
        assert_eq!(back.limiter, limiter);
        assert_eq!(back.fps, Some(23.976023976023978), "bit-exact, not 23.976");

        // ...and the dialect before it, which carried none of the three: a v8
        // file mixes flat, limits nothing, and says nothing about its rate.
        let v8 = b"edith 8\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
                   video 1 0 0 30 0 - - - fit 1000\naudio 1 0 0 30 0 - - - fit 1000\n";
        let old = parse(v8, &dir).expect("v8 parses");
        assert_eq!(old.gains, vec![0.0, 0.0], "one per lane, all at unity");
        assert_eq!(old.limiter, Limiter::default());
        assert_eq!(old.fps, None);
        // A v9 file may leave all three out too, and means the same thing.
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
            "edith 19\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0\n"
        );

        // Each of the three refuses by name, on its own line.
        for (text, want) in [
            (
                "edith 9\nfps nope\nsource 0 a.mp4\n",
                "line 2: \"nope\" is not a frame rate",
            ),
            (
                "edith 9\nlimiter -1.0 maybe\nsource 0 a.mp4\n",
                "line 2: limiter is on or off, not \"maybe\"",
            ),
            (
                "edith 9\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 1000\ngain audio 1 -3.0\n",
                "line 4: gain names a lane that is not there",
            ),
            (
                "edith 9\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 1000\ngain lane 1 -3.0\n",
                "line 4: gain names \"lane\", not a lane kind",
            ),
        ] {
            assert_eq!(
                parse(text.as_bytes(), &dir).unwrap_err().to_string(),
                want,
                "{text:?}"
            );
        }
        // A hand-written ceiling nothing could limit at is clamped, not taken.
        let loud = parse(b"edith 9\nlimiter 12.0 on\nsource 0 a.mp4\n", &dir).expect("parses");
        assert_eq!(loud.limiter.ceiling_db, Limiter::MAX_DB);
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
            "edith 19\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0\n"
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
            "edith 19\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 0 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0\n",
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
        assert!(v5.starts_with(b"edith 19\n"));
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

        let dir = crate::scratch::Scratch::dir("ve_edith_link");
        std::fs::create_dir_all(dir.join("real")).expect("scratch dir");
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
            &[],
            &[],
            &[],
            (1280, 720),
            None,
            crate::tonemap::Preset::default(),
            false,
            true,
            crate::export::EncoderSeat::default(),
            Limiter::default(),
            None,
            0,
        )
        .expect("save");
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 19\nplayhead 0\nresolution 1280 720\nsource 1 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0\n"
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
        let err = parse(b"edith 20\nsource 0 a.mp4\nvideo 0 0 5 0 -\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "line 1: unsupported version 20");
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
