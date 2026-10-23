//! The project file: a line of text per thing, and nothing else.
//!
//! ```text
//! edith 30
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
//! cue 0 3 Retyped\nby hand
//! eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk
//! color 0.1:1.2:0.9:-0.3
//! transform 0.1:-0.1:1.5:90.0:0.0:0.0:0.0:0.0
//! video 1 0 0 120 0 0 0 0 fit 1000 0 0 0 -
//! audio 1 0 0 120 0 0 0 - fit 1000 30 30 0 -
//! video 2 120 0 120 1 - - - fill 2000 0 0 0 0
//! audio 2
//! sub 1 0 60 0 500000 1500000
//! sub 2
//! gain audio 1 -3.0
//! flags audio 1 muted
//! ```
//!
//! The `fps` line is the rate the timeline was cut at -- the scaffolding
//! source's own unless a rate was picked
//! ([`crate::PlaybackSession::set_frame_rate`]), and the *only* thing that says
//! so once one was, since every clip number in the file is counted in it.
//! Printed so it reads back bit-exactly. A file without one (every dialect
//! before v9) means  0"whatever the scaffolding source runs at", which is what
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
//! A `sublang` line is `sublang <track> <code>`: the language of a subtitle
//! track, the track by its position in the `subtitle` lines and the code as the
//! rest of the line (escaped as a `cue`'s text is). Written after the `subtitle`
//! lines, one for each track that has a language: an import names a standalone
//! file's from its file name or the person's `lang`, and a retype replaces an
//! embedded track's container language. **Version 33** is the first dialect
//! with one; a project with none is written exactly as it always was.
//!
//! A `chapter` line is `chapter <frame> <name>`: a chapter marker at a timeline
//! frame, the name as the rest of the line, escaped as a `cue` line's is. Written
//! after the `title` lines in frame order, one per frame. **Version 34** is the
//! first dialect with one; a project with none is written exactly as a v33 one.
//!
//! A `speedkeys` line is `speedkeys <kind> <ord> <index> <at>:<permille> ...`: a
//! clip's speed ramp ([`crate::project::Clip::speed_keys`]), the keys in frame
//! order, each speed in thousandths like the clip's own `speed` field. Written after
//! the lanes it names, only for a clip with keys. **Version 35** is the first
//! dialect with one; a project with no ramp is written exactly as a v34 one.
//!
//! A `marker` line is `marker <frame> <length> <color> <name>`: a comment marker, a
//! point when the length in frames is 0 and a range otherwise, the colour one of
//! red, orange, yellow, green, blue or purple, the name the rest of the line (escaped
//! as a `cue` line's is). Written after the `chapter` lines in frame order; several may
//! share a frame. A `marknote <n> <text>` line gives the note of the n-th `marker` line
//! (the first is 0), only for a marker that has one. **Version 36** is the first
//! dialect with either; a project with no marker is written exactly as a v35 one.
//!
//! A `transition` line is `transition <kind> <ord> <index> <name>`: how a clip's
//! `transition_out` is drawn ([`crate::transitions::TransitionKind`]), by its
//! name -- `dip-black`, `wipe-left`, `zoom` and so on. Written after the lanes it
//! names, only for a clip whose kind is not the dissolve. **Version 37** is the
//! first dialect with one; a project with only dissolves is written exactly as
//! before, and a file without the line means dissolves.
//!
//! A `reverse` line is `reverse <kind> <ord> <index>`: the clip plays its source window
//! backwards ([`crate::project::Clip::reversed`]), named as a `speedkeys` line names its
//! clip. The clip's window, speed and footprint are the lines it already has; the
//! source-keyed `gainkeys` and `tkeys` are written as the clip plays them, so they
//! need no knowing of the direction to be read. Written after the lanes it names, only
//! for a reversed clip. **Version 38** is the first dialect with one; a project with
//! none is written exactly as a v35 (or older) one.
//!
//! An `okeys` line is `okeys video <ord> <index> <at>:<ease>:<opacity> ...` and a
//! `ckeys` line `ckeys video <ord> <index> <at>:<ease>:<b>,<c>,<s>,<t> ...`: a video
//! clip's opacity and colour animations, keys in source order, each with the ease of
//! the segment that leaves it (`lin`, `ease`, `in`, `out`, `hold`). **Version 39** is the
//! first dialect with either, and the first where a `gainkeys` or `tkeys` point may
//! carry an ease beyond linear and `ease`; a project with none is written as before.
//!
//! A `capstyle` line is `capstyle <animation> <rrggbb> <box 0|1> <uppercase 0|1> <words per
//! line>`: the project's caption animation, written only when it is not the default. A
//! `cuewords` line is `cuewords <track> <cue> <start_us> <end_us> <word> ...`: a cue's real
//! word times, only for a cue that has them. **Version 40** is the first dialect with
//! either; a project with neither is written under the header it needed before.
//!
//! A `cue` line is `cue <track> <cue> <text>`: an override of one cue's words,
//! the track by position in the `subtitle` lines (the first is track 0) and the
//! cue by its position in that track as its file parses -- so it needs the file
//! it names to be the one it was edited against, which a project holds it to by
//! not changing the file. The text is the rest of the line, escaped: `\\` for a
//! backslash, `\n` and `\r` for the two line breaks, and nothing else. Applied
//! on the way in, after the track is read, so the edited words come back where
//! the file's would have been. Written after the `subtitle` lines it names and
//! only when somebody has retyped a cue, so a project nobody has edited is the
//! bytes it was. Like the `flags` lines it is v24: a v23 file has none.
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
//! ([`crate::limiter`]): its true-peak ceiling in dBTP and whether it is in circuit,
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
//! A `voice` line is `voice audio <lane> <processor> <fields>...`: one audio lane's voice
//! cleanup ([`crate::voice`]), one line per processor that is not at its default --
//! `gate <on> <threshold> <attack> <release> <hold> <range>`, `nr <on> <strength> <floor>
//! <profile>` (the noise profile as 256 comma-separated dB, or `-`), `comp <on> <threshold>
//! <ratio> <attack> <release> <knee> <makeup> <auto>` and `deess <on> <threshold> <amount>
//! <lo> <hi>`. **Version 41** is the first dialect with one; a project with every lane at
//! the defaults is written under the header it needed before, and a v40 file loads with every
//! processor off.
//!
//! An `effect` line is `effect video <lane> <clip> <kind> <on> <value>...`: one effect of
//! a clip's effects stack ([`crate::effects`]), the lines of one clip in stack order. `kind`
//! is the effect's name (`blur`, `vignette`, `chroma_key`, ...), `on` is `1` or `0`, and the
//! values are its parameters in [`crate::effects::EffectKind::params`] order, each in its
//! range. **Version 42** is the first dialect with one; a project with no effect on any clip
//! is written under the header it needed before.
//!
//! A `stab` line is `stab video <ord> <index> <strength> <crop>`: a video clip's
//! stabilisation ([`crate::stabilise`]), strength `0..=1` and crop `zoom` or `full`. Only a
//! stabilised clip gets one. **Version 44** is the first dialect with one; a project with no
//! stabilised clip is written under the header it needed before.
//!
//! A `lanename` line is `lanename <kind> <lane> <name>`: the display label a lane was given
//! (the text escaped as a title's is); the lane keeps its address. A `deint` line is
//! `deint video <ord> <index> <mode>` with mode `field` or `off` (`frame` is the default and
//! written by no one): how a clip's interlaced source is deinterlaced. **Version 46** is the
//! first dialect with either; a project with no named lane and no such clip is written under
//! the header it needed before.
//!
//! A `flags` line is `flags <kind> <lane> <switch>...`: which lane, named as a
//! gain names one, and then the switches that are set on it -- any of `hidden`,
//! `muted` and `locked`, each at most once, in any order. `hidden` is for a
//! video or subtitle lane and `muted` for an audio one: a switch the kind of
//! lane cannot have is refused, as the setters refuse it. Like a gain it comes
//! after the lanes and only a lane somebody has switched gets one, so a
//! project with none is the bytes a v23 one was bar the version line.
//!
//! A `duck` line is `duck audio <lane> <under> <depth> <threshold> <attack> <release>`:
//! the audio lane that is ducked, the audio lane it is ducked under (its key),
//! both named as a gain names an audio lane (1-based, `audio 1` is `A1`), then
//! the depth in dB (`-40..=0`), the key threshold in dBFS (`-90..=0`), and the
//! attack and release in ms (attack `1..=2000`, release `1..=10000`). Like a
//! gain it comes after the lanes, a duck names its key lane, and only a lane
//! somebody has ducked gets one; a key may not be the lane it ducks. A project
//! with none is the bytes a v24 one was bar the version line. **Version 30** lets
//! a lane be ducked under several keys: one `duck` line per key, in the order
//! the lane holds them, and the same key twice is refused. Before it a lane has
//! one `duck` line, and a second for it is refused by name.
//!
//! A `title` line is one title card or text overlay:
//! `title <start> <frames> <fade_in> <fade_out> <size> <outline> <boxed> <place> <text>`.
//! The frames are timeline frames (the title shows on `start..start+frames`),
//! the fades are frames, `size` is the em size in permille of the frame height
//! (`10..=200`), `outline` is `none`, `thin` or `thick`, `boxed` is `0` or `1`,
//! and `place` is `bottom`, `top` or `centre`. The text is the rest of the line,
//! escaped as a `cue` line's is, so it may hold spaces and a line break as `\n`.
//! Titles are written in list order, after the `duck` lines; a project with none
//! is the bytes a v25 one was bar the version line.
//!
//! **Version 49** gives a title a look: a `titlestyle <title> <position> <align> <weight>
//! <rrggbb> <rrggbbaa>` line after the `title` lines, one per title that is not white, regular,
//! centred on a black 60% box, naming the title by its place in the list (0 is the first).
//! `position` is where the block sits across the frame and `align` how its lines line up in it
//! (both `centre`, `left` or `right`); `weight` is `regular` or `bold`; the first colour is the
//! letters, the second the box with its opacity last. A project with none is the bytes a v48 one
//! was bar the version line.
//!
//! **Version 54** gives a title an entrance and an exit animation: a `titleanim <title> <in> <out>
//! <frames>` line after the `titlestyle` lines, one per title that has either, naming the title by
//! its place in the list. `in` and `out` are the animation's name ([`TitleAnim::name`], `none` for
//! none) and `frames` how long each takes. A project with none is the bytes a v53 one was bar the
//! version line.
//!
//! A `cueadd` line is one cue added to a subtitle track, which no file has:
//! `cueadd <track> <start_us> <end_us> <text>`, the track by its position in
//! the `subtitle` lines, the times in microseconds on the track's own clock, and
//! the text escaped as a `cue` line's is. It comes after the `cue` lines, so an
//! added cue lands among the file's cues however they were retyped. A project
//! with none is the bytes a v28 one was bar the version line.
//!
//! A `pitch` line is `pitch audio <ord> <index> <semitones>`: an audio clip's pitch
//! shift in whole semitones, `-12` to `12`, written for each clip that is not at `0`,
//! after the lanes it names (as `clippan` is). **Version 52** is the first dialect
//! with one; a project with none is the bytes a v51 one was bar the version line.
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
//! `<frequency>:<gain dB>:<Q>:<shape>` with the shape spelled `ls`, `pk`, `hs`,
//! `lc` (low cut) or `hc` (high cut) ([`crate::eq::BandKind`]). Like a source it is named by *position* --
//! the first eq line is eq 0 -- and a clip names one, so twenty clips sharing a
//! curve write it once. `eq` on its own is the empty cascade, which is a
//! setting like any other. The numbers are printed to round trip bit-exactly;
//! anything not finite is refused, here and at [`crate::Project::set_eq`].
//!
//! A colour line is `color <brightness>:<contrast>:<saturation>:<tint>`
//! ([`crate::color::ColorParams`]), named by position exactly as an eq line is
//! -- the first colour line is color 0 -- printed and refused by the same rules.
//!
//! A colour line's look ([`crate::color::Look`]) rides on three line types
//! written at the end of the file and naming the colour line by position:
//! `wheels <color> <lift> <gamma> <gain>` (each `<master>:<red>:<green>:<blue>`,
//! -1..1), `curve <color> <channel> <x>:<y>,...` (channel `master`, `red`, `green`
//! or `blue`, up to 16 points in 0..1 with rising inputs) and `lut <color> <mix>
//! <path>` (mix 0..1, the path written as a source's is). **Version 43** is the
//! first dialect with them; a project whose grades have no look is written under
//! the header it needed before. A LUT file that is missing or malformed does not
//! stop the load: the clip plays without it and the project names it
//! ([`crate::Project::lut_problems`]).
//!
//! A transform line is `transform <pos_x>:<pos_y>:<scale>:<rotate>:<crop_l>:
//! <crop_r>:<crop_t>:<crop_b>` ([`crate::transform::TransformParams`]), named by
//! position exactly as an eq or a colour line is -- the first transform line is
//! transform 0 -- printed and refused by the same rules: how a clip's picture
//! sits, is sized, is turned and is trimmed on the canvas, on top of whatever
//! its fit policy already placed.
//!
//! A lane line is `<kind> <lane> <start> <in> <out> <source> <link> <eq>
//! <color> <fit> <speed> <fade_in> <fade_out> <transition_out> <transform> <visualizer> <viz_paint>`:
//! which lane the clip is on -- its kind and its 1-based number
//! among the lanes of that kind, the [`crate::project::Lane::label`] a header
//! column shows -- then where the clip sits on the timeline, the half-open
//! source range it plays, the file it plays from, its group id, the eq line it
//! plays through, the colour line it is graded by (`-` for none of those three)
//! and how it meets a project canvas of another shape, spelled `fit`, `fill`,
//! `stretch` or `center` ([`crate::scale::FitPolicy`]), how fast it plays,
//! in thousandths of real time ([`crate::project::Speed`], `1000` for a clip
//! nobody has speeded), and finally its ramp up from silence and ramp down to
//! it, in timeline frames ([`crate::project::Clip::fade_in`],
//! [`crate::project::Clip::fade_out`], `0 0` for a clip nobody has faded), its
//! cross-dissolve into whatever abuts its end
//! ([`crate::project::Clip::transition_out`], `0` for a hard cut), and last of
//! all the transform line it is placed, sized, turned and cropped by, on top
//! of everything its fit policy already did (`-` for a clip left at its fit
//! policy's own spot, [`crate::project::Clip::transform`]), a visualizer
//! flags byte (`0` for an ordinary picture, bit 0 for a waveform of the
//! source's sound, the rest outline/smooth/fast/tint) and that waveform's
//! packed look ([`crate::project::Clip::viz_paint`]): hue, sat, strands and
//! glow in the low four bytes, then the strand colour's own hue and sat,
//! with the top bit set once a clip has chosen one.
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
//! **Version 23** was this without the `flags` lines: such a project has no
//! lane hidden, muted or locked, which is what every lane of every dialect
//! before it is.
//! **Version 22** was this with a 32-bit packed look: such a project's clips
//! chose no strand colour, which is what the top bit still clear means, and
//! their value is the low half of the one v23 writes.
//! **Version 19** was this without the clip's transform field: such a project
//! places no clip anywhere but its fit policy's own spot, which is what `-`
//! still means. **Version 18** was that without the clip's transition field:
//! such a project dissolves no clip into its neighbor, which is what `0` still
//! means. **Version 17** was this without the clip's two fade fields: such a project
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
//! names, and an older one has no lane hidden, muted or locked -- and saving
//! any of them writes v24. An older
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
use crate::duck::{
    Duck, MAX_ATTACK_MS, MAX_DEPTH_DB, MAX_RELEASE_MS, MAX_THRESHOLD_DB, MIN_DEPTH_DB,
    MIN_THRESHOLD_DB, MIN_TIME_MS,
};
use crate::eq::{Band, BandKind, EqParams};
use crate::limiter::Limiter;
use crate::project::{
    Chapter, Clip, Deinterlace, GainKey, Marker, MarkerColor, Lane, LaneFlags, LaneKind, MAX_CLIP_GAIN_DB, MAX_GAIN_KEYS,
    MAX_SPEED_KEYS, MIN_CLIP_GAIN_DB, STILL_DEFAULT_SECS, STILL_MAX_SECS, STILL_MIN_SECS, Source, Speed, SpeedKey, SubClip,
    Title, check_chapter, check_marker, check_title,
};
use crate::scale::FitPolicy;
use crate::subtitle::SubtitleTrack;
use crate::captionanim::{CaptionAnim, CaptionStyle, MAX_WORDS_PER_LINE};
use crate::voice::{Compressor, DeEsser, Gate, NoiseReduction, VoiceFx};
use crate::text::{Align, HPlace, Outline, Place, TextStyle, Weight};
use crate::titleanim::TitleAnim;
use crate::color::{ColorKey, MAX_COLOR_KEYS};
use crate::transitions::TransitionKind;
use crate::transform::{KeyEase, MAX_TRANSFORM_KEYS, OpacityKey, TransformKey, TransformParams};

/// The newest project format version: the `N` in the `edith N` first line that
/// [`save`] writes for a project that uses the newest features. A reader refuses any
/// higher version by name.
pub const NEWEST_VERSION: u32 = 55;

/// What [`save`] writes. Read support goes back to `edith 1`; see the module
/// docs for what those dialects looked like. Kept at [`NEWEST_VERSION`].
const MAGIC: &[u8] = b"edith 55";
const MAGIC_V54: &[u8] = b"edith 54";
const MAGIC_V53: &[u8] = b"edith 53";
const MAGIC_V52: &[u8] = b"edith 52";
const MAGIC_V51: &[u8] = b"edith 51";
const MAGIC_V50: &[u8] = b"edith 50";
const MAGIC_V49: &[u8] = b"edith 49";
const MAGIC_V48: &[u8] = b"edith 48";
const MAGIC_V47: &[u8] = b"edith 47";
const MAGIC_V46: &[u8] = b"edith 46";
/// The most bands one equalizer line may declare. A hand-built cascade is a
/// handful; the cap keeps a crafted line from making every clip run a million
/// filters.
const MAX_EQ_BANDS: usize = 64;
/// The most words one `cuewords` line may carry (a cue is a line or two).
const MAX_CUE_WORDS: usize = 1_000;
/// The most sequences (tabs) a project may hold, each a parse of its own.
const MAX_SEQUENCES: usize = 256;
/// The most chapters, and the most markers, a project may hold.
const MAX_LIST_ENTRIES: usize = 100_000;

const MAGIC_V45: &[u8] = b"edith 45";
const MAGIC_V44: &[u8] = b"edith 44";
const MAGIC_V43: &[u8] = b"edith 43";
const MAGIC_V42: &[u8] = b"edith 42";
const MAGIC_V41: &[u8] = b"edith 41";
const MAGIC_V40: &[u8] = b"edith 40";
const MAGIC_V39: &[u8] = b"edith 39";
const MAGIC_V38: &[u8] = b"edith 38";
const MAGIC_V37: &[u8] = b"edith 37";
const MAGIC_V36: &[u8] = b"edith 36";
const MAGIC_V35: &[u8] = b"edith 35";
const MAGIC_V34: &[u8] = b"edith 34";
const MAGIC_V33: &[u8] = b"edith 33";
const MAGIC_V32: &[u8] = b"edith 32";
const MAGIC_V31: &[u8] = b"edith 31";
const MAGIC_V30: &[u8] = b"edith 30";
const MAGIC_V29: &[u8] = b"edith 29";
const MAGIC_V28: &[u8] = b"edith 28";
const MAGIC_V27: &[u8] = b"edith 27";
const MAGIC_V26: &[u8] = b"edith 26";
const MAGIC_V25: &[u8] = b"edith 25";
const MAGIC_V24: &[u8] = b"edith 24";
const MAGIC_V23: &[u8] = b"edith 23";
const MAGIC_V22: &[u8] = b"edith 22";
const MAGIC_V21: &[u8] = b"edith 21";
const MAGIC_V20: &[u8] = b"edith 20";
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
    /// Every lane's switches -- hidden, muted, locked -- in the order `lanes` is
    /// in and as long as it. All clear for every dialect before v24, which had
    /// no such switches, and for a lane a `flags` line does not name.
    pub flags: Vec<LaneFlags>,
    /// Every lane's auto-ducks, in the order `lanes` is in and as long as it;
    /// empty for every lane but an audio lane somebody has ducked. Each lane
    /// has at most one duck per key. Empty of every list for every dialect
    /// before v25, which had no ducks, and at most one per lane before v30.
    pub ducks: Vec<Vec<Duck>>,
    /// The title cards and text overlays, in the order the file has them.
    /// Empty for every dialect before v26, which had none.
    pub titles: Vec<Title>,
    /// The chapter markers, in frame order. Empty for every dialect before v34.
    pub chapters: Vec<Chapter>,
    /// The comment markers, in frame order. Empty for every dialect before v36.
    pub markers: Vec<Marker>,
    /// How burned captions animate, the `capstyle` line. The default (static cues) for every
    /// dialect before v40.
    pub caption_style: CaptionStyle,
    /// Every lane's voice processors, in the order `lanes` is in and as long as it.
    /// All off for every dialect before v41, and for a lane no `voice` line names.
    pub voices: Vec<VoiceFx>,
    /// One display name per lane (empty for none), from the `lanename` lines (v46).
    pub names: Vec<String>,
    /// The master limiter. Off for every dialect before v9, and for a v9 file
    /// that leaves the line out.
    pub limiter: Limiter,
    /// The subtitle tracks, as the file names them: the path the cues are in
    /// and which track of it, which is what [`crate::subtitle::open`] takes.
    /// Reading them is the loader's business, not this module's -- a `.edith`
    /// holds no cues. Empty for every dialect before v10.
    pub subtitles: Vec<(PathBuf, Option<u64>)>,
    /// The language of a subtitle track, `(track, code)`: the track by its
    /// position in `subtitles`, the code as the file holds it (ISO 639-2 for
    /// one set by a person, a container's own tag for an embedded track that
    /// was not retyped). Empty for every dialect before v33.
    pub sub_langs: Vec<(usize, String)>,
    /// The cue overrides, `(track, cue, text)`: the track by its position in
    /// `subtitles`, the cue by its position in that track. Empty for every
    /// dialect before v24.
    pub cue_edits: Vec<(usize, usize, String)>,
    /// The cues added to a track ([`crate::project::AddedCue`]), each on its
    /// track's own clock. Empty for every dialect before v29.
    pub cue_adds: Vec<crate::project::AddedCue>,
    /// The word times of cues, `(track, cue, words)` with the cue by its position in the track
    /// as it stands after the edits and additions: the `cuewords` lines (v40). Empty before v40.
    pub cue_words: Vec<crate::project::CueWordRow>,
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
    /// the sources' own: the highest rate among the sources that have sound,
    /// or 48 kHz when every one of them is below 44.1 kHz. Playback and export
    /// both resample every other source
    /// to whichever rate this resolves to, exactly as they already resample a
    /// source shot at another rate than the timeline's -- an explicit choice
    /// here only changes what that timeline's own rate *is*.
    pub sample_rate: Option<u32>,
    pub playhead: u32,
    /// The active sequence's id and name, from the `seqactive` line (v45). `None` for a file
    /// that holds one sequence, which is every dialect before v45.
    pub seq_active: Option<(u32, String)>,
    /// The tab order, from the `seqorder` line.
    pub seq_order: Vec<u32>,
    /// The other sequences, each a complete document of its own (`seq` ... `endseq`).
    pub sequences: Vec<SeqDoc>,
    /// The `mcam` lines (v47): a multicam source sequence's id and one angle's name each, in
    /// angle order. Empty for a file with no multicam clip.
    pub mcams: Vec<(u32, String)>,
    /// The library's bins, from the `bin` lines (v51). Empty for a file with none.
    pub bins: Vec<crate::project::Bin>,
    /// The project notes, from the `notes` line (v52). Empty for a file with none, and for
    /// every dialect before v52.
    pub notes: String,
    /// The length a still is placed at, from the `still` line (v52). `None` for a file that
    /// leaves it out, which means the default: every dialect before v52 placed stills at 5 s.
    pub still_secs: Option<f64>,
}

/// A parked sequence read off a v45 file.
#[derive(Debug)]
pub struct SeqDoc {
    pub id: u32,
    pub name: String,
    pub doc: Document,
}

/// Writes the project to `path`, atomically. `sources`, `eq` and `color` should
/// already be orphan-free ([`crate::Project::without_orphan_sources`]);
/// `gains` is one dB per lane in the same order the lanes are in
/// ([`crate::Project::lane_gains`]) and `subs` is one list of placements per
/// lane in that same order ([`crate::Project::lane_subs`]).
///
/// The same as [`save_flagged`] with every lane's switches clear, for the
/// callers that have none to write.
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
    save_flagged(
        path,
        sources,
        lanes,
        gains,
        subs,
        subtitles,
        eq,
        color,
        transform,
        resolution,
        fps,
        tone,
        proxy,
        auto_proxy,
        encoder,
        limiter,
        sample_rate,
        playhead,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &CaptionStyle::default(),
        &[],
        &[],
    )
}

/// The bytes [`save_flagged`] writes, for a caller that assembles a file of several parts
/// ([`sequenced`]). `dir` is the project's directory, which relative paths are written against.
#[allow(clippy::too_many_arguments)]
pub fn encode_flagged(
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
    flags: &[LaneFlags],
    ducks: &[Vec<Duck>],
    cue_edits: &[(usize, usize, String)],
    cue_adds: &[crate::project::AddedCue],
    titles: &[Title],
    chapters: &[Chapter],
    markers: &[Marker],
    caption_style: &CaptionStyle,
    voices: &[VoiceFx],
    names: &[String],
) -> Vec<u8> {
    let mut bytes = emit(
        dir,
        sources,
        lanes,
        gains,
        subs,
        subtitles,
        eq,
        color,
        transform,
        resolution,
        fps,
        tone,
        proxy,
        auto_proxy,
        encoder,
        limiter,
        sample_rate,
        playhead,
    );
    // The cue overrides after the subtitle lines `emit` wrote, which
    // they name; then the switches, which name lanes, as the gains do.
    bytes.extend_from_slice(&cue_lines(cue_edits));
    bytes.extend_from_slice(&cue_add_lines(cue_adds));
    let cue_words = cue_word_lines(subtitles);
    bytes.extend_from_slice(&flag_lines(lanes, flags));
    bytes.extend_from_slice(&duck_lines(lanes, ducks));
    bytes.extend_from_slice(&title_lines(titles));
    // The chapters after the titles, and a file that has any is v34 whatever
    // `emit` picked, and a file with a speed ramp is v35, one with a marker v36, one with a transition kind v37, one with a voice processor v41: the header line is
    // the one line `emit` wrote that these change, so it is swapped here,
    // where the lines they need are.
    let ramped = lanes
        .iter()
        .any(|(_, clips)| clips.iter().any(|c| !c.speed_keys.is_empty()));
    let transitioned = lanes.iter().any(|(_, clips)| {
        clips.iter().any(|c| c.transition_kind != TransitionKind::Dissolve)
    });
    let reversed = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.reversed));
    // ...and one with an opacity or colour animation, or a key easing beyond
    // linear and `ease`, is v39.
    let animated = lanes.iter().any(|(_, clips)| clips.iter().any(needs_v39));
    // ...and one with a project caption style or cue word times is v40.
    let captioned = *caption_style != CaptionStyle::default() || !cue_words.is_empty();
    // ...and one with a lane's voice processor is v41.
    let voiced = voices.iter().any(|v| !v.is_default());
    // ...and one with a clip's effect is v42.
    let effected = lanes.iter().any(|(_, clips)| clips.iter().any(|c| !c.effects.is_empty()));
    // ...and one with a grade's wheels, curves or LUT is v43.
    let looked = color.iter().any(|p| p.look != crate::color::Look::default());
    // ...and one with a stabilised clip is v44.
    let steadied = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.stabilise.is_some()));
    // ...and one with a named lane or a clip's deinterlace choice is v46.
    let named = names.iter().any(|n| !n.is_empty());
    let deinterlaced = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.deinterlace != Deinterlace::Frame));
    // ...and one with a multicam clip's angle is v47.
    let angled = lanes.iter().any(|(_, clips)| clips.iter().any(|c| !c.angle.is_default()));
    // ...and one with a generated source (a solid, a gradient, a countdown, bars, an adjustment
    // layer), a clip's colour label or a switched-off clip is v48.
    // ...and one with a title's colour, weight, alignment, position or box colour is v49.
    let styled = titles.iter().any(|t| !title_look_is_default(&t.style));
    // ...and one with a title's entrance or exit animation is v54.
    let titled = titles.iter().any(|t| t.in_anim != TitleAnim::None || t.out_anim != TitleAnim::None);
    let marked = sources.iter().any(|s| crate::generate::is_generated(&s.path))
        || lanes.iter().any(|(_, clips)| clips.iter().any(|c| !c.mark.is_default()));
    // ...and one with a clip pan or a lane pan is v50.
    let panned = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.pan != 0)) || flags.iter().any(|f| f.pan != 0.0);
    // ...and one with a mask's keys is v53.
    let masked = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.effects.iter().any(|e| !e.keys.is_empty())));
    // ...and one with a clip's pitch shift is v55.
    let pitched = lanes.iter().any(|(_, clips)| clips.iter().any(|c| c.pitch != 0));
    if pitched || titled || masked || panned || styled || marked || angled || named || deinterlaced || steadied || looked || effected || voiced || captioned || animated || reversed || transitioned || ramped || !chapters.is_empty() || !markers.is_empty() {
        let first_end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len());
        let header = if pitched {
            MAGIC
        } else if titled {
            MAGIC_V54
        } else if masked {
            MAGIC_V53
        } else if panned {
            MAGIC_V50
        } else if styled {
            MAGIC_V49
        } else if marked {
            MAGIC_V48
        } else if angled {
            MAGIC_V47
        } else if named || deinterlaced {
            MAGIC_V46
        } else if steadied {
            MAGIC_V44
        } else if looked {
            MAGIC_V43
        } else if effected {
            MAGIC_V42
        } else if voiced {
            MAGIC_V41
        } else if captioned {
            MAGIC_V40
        } else if animated {
            MAGIC_V39
        } else if reversed {
            MAGIC_V38
        } else if transitioned {
            MAGIC_V37
        } else if !markers.is_empty() {
            MAGIC_V36
        } else if ramped {
            MAGIC_V35
        } else {
            MAGIC_V34
        };
        bytes.splice(..first_end, header.iter().copied());
    }
    bytes.extend_from_slice(&chapter_lines(chapters));
    bytes.extend_from_slice(&marker_lines(markers));
    bytes.extend_from_slice(&capstyle_line(caption_style));
    bytes.extend_from_slice(&cue_words);
    bytes.extend_from_slice(&voice_lines(lanes, voices));
    bytes.extend_from_slice(&clip_gain_lines(lanes));
    bytes.extend_from_slice(&gain_key_lines(lanes));
    bytes.extend_from_slice(&tkey_lines(lanes));
    bytes.extend_from_slice(&speed_key_lines(lanes));
    bytes.extend_from_slice(&transition_lines(lanes));
    bytes.extend_from_slice(&reverse_lines(lanes));
    bytes.extend_from_slice(&opacity_key_lines(lanes));
    bytes.extend_from_slice(&color_key_lines(lanes));
    bytes.extend_from_slice(&effect_lines(lanes));
    bytes.extend_from_slice(&mask_key_lines(lanes));
    bytes.extend_from_slice(&look_lines(color, dir));
    bytes.extend_from_slice(&stab_lines(lanes));
    bytes.extend_from_slice(&deint_lines(lanes));
    bytes.extend_from_slice(&lane_name_lines(lanes, names));
    bytes.extend_from_slice(&angle_lines(lanes));
    bytes.extend_from_slice(&mark_lines(lanes));
    bytes.extend_from_slice(&pan_lines(lanes, flags));
    bytes.extend_from_slice(&pitch_lines(lanes));
    bytes
}

/// `main` (a document as [`encode_flagged`] wrote it) with the library's lines after it (v51):
/// `bin <id> <parent> <name>` for each bin, `srcbin <source> <bin>` for each filed source and
/// `srcfav <source>` for each starred one. A library with none of these is `main` untouched, so
/// such a project is the bytes it was before v51.
/// `current` (a file's first line, without its newline) with `candidate` in its place when
/// `candidate` is a newer dialect, else `current` untouched. The library and notes lines raise
/// the header to what they need and never lower it, so a file with mask keys (v53) keeps v53.
fn raise_header(current: &[u8], candidate: &[u8]) -> Vec<u8> {
    let version = |h: &[u8]| {
        std::str::from_utf8(h).ok().and_then(|l| l.strip_prefix("edith ")).and_then(|n| n.parse::<u32>().ok()).unwrap_or(0)
    };
    if version(candidate) > version(current) { candidate.to_vec() } else { current.to_vec() }
}

pub fn binned(mut main: Vec<u8>, sources: &[Source], bins: &[crate::project::Bin]) -> Vec<u8> {
    let mut lines = Vec::new();
    for b in bins {
        lines.extend_from_slice(format!("bin {} {} ", b.id, b.parent).as_bytes());
        escape_text(&b.name, &mut lines);
        lines.push(b'\n');
    }
    for (i, s) in sources.iter().enumerate() {
        if s.bin != 0 {
            lines.extend_from_slice(format!("srcbin {i} {}\n", s.bin).as_bytes());
        }
        if s.fav {
            lines.extend_from_slice(format!("srcfav {i}\n").as_bytes());
        }
    }
    if lines.is_empty() {
        return main;
    }
    let first_end = main.iter().position(|&b| b == b'\n').unwrap_or(main.len());
    let header = raise_header(&main[..first_end], MAGIC_V51);
    main.splice(..first_end, header);
    if main.last() != Some(&b'\n') {
        main.push(b'\n');
    }
    main.extend_from_slice(&lines);
    main
}

/// `main` with the project notes and the still length after it (v52): a `notes` line when there
/// are notes, a `still` line when the length is not the default. A project with neither is `main`
/// untouched, so it keeps the bytes it had before v52.
pub fn noted(mut main: Vec<u8>, notes: &str, still_secs: f64) -> Vec<u8> {
    let mut lines = Vec::new();
    if !notes.is_empty() {
        lines.extend_from_slice(b"notes ");
        escape_text(notes, &mut lines);
        lines.push(b'\n');
    }
    if still_secs != STILL_DEFAULT_SECS {
        lines.extend_from_slice(format!("still {still_secs:?}\n").as_bytes());
    }
    if lines.is_empty() {
        return main;
    }
    let first_end = main.iter().position(|&b| b == b'\n').unwrap_or(main.len());
    let header = raise_header(&main[..first_end], MAGIC_V52);
    main.splice(..first_end, header);
    if main.last() != Some(&b'\n') {
        main.push(b'\n');
    }
    main.extend_from_slice(&lines);
    main
}

/// [`save`], with each lane's switches -- `flags` is one entry per lane in the
/// same order as `lanes`, as `gains` is ([`crate::Project::lane_flags`]).
#[allow(clippy::too_many_arguments)]
pub fn save_flagged(
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
    flags: &[LaneFlags],
    ducks: &[Vec<Duck>],
    cue_edits: &[(usize, usize, String)],
    cue_adds: &[crate::project::AddedCue],
    titles: &[Title],
    chapters: &[Chapter],
    markers: &[Marker],
    caption_style: &CaptionStyle,
    voices: &[VoiceFx],
    names: &[String],
) -> crate::Result<()> {
    let dir = project_dir(path);
    let bytes = encode_flagged(
        &dir,
        sources,
        lanes,
        gains,
        subs,
        subtitles,
        eq,
        color,
        transform,
        resolution,
        fps,
        tone,
        proxy,
        auto_proxy,
        encoder,
        limiter,
        sample_rate,
        playhead,
        flags,
        ducks,
        cue_edits,
        cue_adds,
        titles,
        chapters,
        markers,
        caption_style,
        voices,
        names,
    );
    write_atomic(path, &bytes)
}

/// One parked sequence as a file holds it: a whole standalone document of its own.
pub struct SeqBlock {
    pub id: u32,
    pub name: String,
    pub bytes: Vec<u8>,
}

/// `main` (the active sequence, as [`encode_flagged`] wrote it) with the other sequences after
/// it, as a v45 file: the `seqactive` and `seqorder` lines, then each parked sequence between
/// `seq <id> <name>` and `endseq`. The body of each block is a complete document, so its lines
/// read exactly as a project file's do.
pub fn sequenced(main: Vec<u8>, active: (u32, &str), order: &[u32], blocks: &[SeqBlock], mcam: &[(u32, Vec<String>)]) -> Vec<u8> {
    let mut bytes = main;
    let first_end = bytes.iter().position(|&b| b == b'\n').unwrap_or(bytes.len());
    // v47 only when something in the file needs it (a multicam source or an angle somewhere), v46
    // when a part is v46; a newer-only line under an older header is refused.
    let is = |b: &[u8], m: &[u8]| b.starts_with(m) && b.get(m.len()) == Some(&b'\n');
    let v55 = is(&bytes, MAGIC) || blocks.iter().any(|b| is(&b.bytes, MAGIC));
    let v54 = v55 || is(&bytes, MAGIC_V54) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V54));
    let v53 = v54 || is(&bytes, MAGIC_V53) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V53));
    let v52 = v53 || is(&bytes, MAGIC_V52) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V52));
    let v51 = v52 || is(&bytes, MAGIC_V51) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V51));
    let v50 = v51 || is(&bytes, MAGIC_V50) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V50));
    let v49 = v50 || is(&bytes, MAGIC_V49) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V49));
    let v48 = v49 || is(&bytes, MAGIC_V48) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V48));
    let v47 = !mcam.is_empty() || is(&bytes, MAGIC_V47) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V47));
    let v46 = is(&bytes, MAGIC_V46) || blocks.iter().any(|b| is(&b.bytes, MAGIC_V46));
    let header = if v55 { MAGIC } else if v54 { MAGIC_V54 } else if v53 { MAGIC_V53 } else if v52 { MAGIC_V52 } else if v51 { MAGIC_V51 } else if v50 { MAGIC_V50 } else if v49 { MAGIC_V49 } else if v48 { MAGIC_V48 } else if v47 { MAGIC_V47 } else if v46 { MAGIC_V46 } else { MAGIC_V45 };
    bytes.splice(..first_end, header.iter().copied());
    bytes.extend_from_slice(format!("seqactive {} ", active.0).as_bytes());
    escape_text(active.1, &mut bytes);
    bytes.push(b'\n');
    let ids: Vec<String> = order.iter().map(u32::to_string).collect();
    bytes.extend_from_slice(format!("seqorder {}\n", ids.join(" ")).as_bytes());
    for (id, names) in mcam {
        for name in names {
            bytes.extend_from_slice(format!("mcam {id} ").as_bytes());
            escape_text(name, &mut bytes);
            bytes.push(b'\n');
        }
    }
    for b in blocks {
        bytes.extend_from_slice(format!("seq {} ", b.id).as_bytes());
        escape_text(&b.name, &mut bytes);
        bytes.push(b'\n');
        bytes.extend_from_slice(&b.bytes);
        if bytes.last() != Some(&b'\n') {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(b"endseq\n");
    }
    bytes
}

/// Writes `bytes` to `path` atomically: a `.part` file beside it, synced, renamed over it, and the
/// directory synced, so until the rename the old file is still the whole truth.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> crate::Result<()> {
    let dir = project_dir(path);
    let mut part = path.to_path_buf().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let result = std::fs::File::create(&part)
        .and_then(|mut f| {
            f.write_all(bytes)?;
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
    // Relative lines are joined to the directory the project was opened
    // through, so the file is reached the way the project was opened; the
    // save side compares canonically (`written_path`), so whichever way it
    // was opened, a saved source is written relative.
    parse(&data, path.parent().unwrap_or(Path::new("")))
}

/// The form a source or subtitle path is written in: relative to the project's
/// directory when the file is inside it, however the path was spelled -- a
/// `..` or a symlink on the way in is resolved before the comparison -- and as
/// it is otherwise. This is the one rule every save follows.
fn written_path(path: &Path, dir: &Path) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(dir) {
        return rel.to_path_buf();
    }
    if let Ok(real) = path.canonicalize()
        && let Ok(rel) = real.strip_prefix(dir)
    {
        return rel.to_path_buf();
    }
    path.to_path_buf()
}

/// The directory a project file's relative source lines are measured from, in
/// the same canonical form [`crate::Project`] keeps its sources in -- without
/// that the prefix simply never matches and every line comes out absolute,
/// which is what `cd dir && edith clip.mp4` + save used to write (the parent of
/// a bare filename is `""`). Canonicalizing fails only if the directory does
/// not exist, and then the write is about to fail anyway.
pub(crate) fn project_dir(path: &Path) -> PathBuf {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// The `cue` lines: one per retyped cue, after the `subtitle` lines that name
/// their tracks. Only a cue somebody has retyped has one, so an unedited project
/// is the bytes it was.
fn cue_lines(edits: &[(usize, usize, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (track, cue, text) in edits {
        out.extend_from_slice(format!("cue {track} {cue} ").as_bytes());
        escape_text(text, &mut out);
        out.push(b'\n');
    }
    out
}

/// The `cuewords` lines (v40): `cuewords <track> <cue> <start_us> <end_us> <word> ...` for each
/// cue that has word times, the cue by its position in the track as it stands when saved. The
/// times are on the track's own clock; a word has no whitespace, so a space separates fields.
fn cue_word_lines(subtitles: &[SubtitleTrack]) -> Vec<u8> {
    let mut out = Vec::new();
    for (t, track) in subtitles.iter().enumerate() {
        for (c, cue) in track.cues.iter().enumerate().filter(|(_, c)| !c.words.is_empty()) {
            out.extend_from_slice(format!("cuewords {t} {c}").as_bytes());
            for (start, end, word) in &cue.words {
                out.extend_from_slice(format!(" {start} {end} ").as_bytes());
                escape_text(word, &mut out);
            }
            out.push(b'\n');
        }
    }
    out
}

/// The `cueadd` lines: one per cue added to a track, after the `cue` lines, as
/// `cueadd <track> <start_us> <end_us> <text>`, the text escaped as a `cue` line's is.
fn cue_add_lines(adds: &[crate::project::AddedCue]) -> Vec<u8> {
    let mut out = Vec::new();
    for a in adds {
        out.extend_from_slice(format!("cueadd {} {} {} ", a.track, a.start_us, a.end_us).as_bytes());
        escape_text(&a.text, &mut out);
        out.push(b'\n');
    }
    out
}

/// The `duck` lines: one per duck, in lane order and then in the order the lane
/// holds them, the key named as the lanes are (1-based), so a project with none
/// is the bytes a v24 one was bar the version line.
fn duck_lines(lanes: &[(LaneKind, Vec<Clip>)], ducks: &[Vec<Duck>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut audio = 0u32;
    for ((kind, _), lane_ducks) in lanes.iter().zip(ducks) {
        if *kind != LaneKind::Audio {
            continue;
        }
        audio += 1;
        for d in lane_ducks {
            out.extend_from_slice(
                format!(
                    "duck audio {audio} {} {:?} {:?} {:?} {:?}\n",
                    d.under + 1,
                    d.depth_db,
                    d.threshold_db,
                    d.attack_ms,
                    d.release_ms
                )
                .as_bytes(),
            );
        }
    }
    out
}

/// The `voice` lines: `voice audio <ord> <processor> <fields>...`, one per processor of an
/// audio lane that is not at its default (so a lane nobody has cleaned writes none, and a
/// project with none is the bytes a v36 one was bar the version line). The processors:
/// `gate <on> <threshold> <attack> <release> <hold> <range>`, `nr <on> <strength> <floor>
/// <profile>` (the profile a comma-separated list of dB, or `-`), `comp <on> <threshold>
/// <ratio> <attack> <release> <knee> <makeup> <auto>` and `deess <on> <threshold> <amount>
/// <lo> <hi>`; `<on>` and `<auto>` are `0` or `1`.
fn voice_lines(lanes: &[(LaneKind, Vec<Clip>)], voices: &[VoiceFx]) -> Vec<u8> {
    let mut out = String::new();
    let mut audio = 0u32;
    for ((kind, _), v) in lanes.iter().zip(voices) {
        if *kind != LaneKind::Audio {
            continue;
        }
        audio += 1;
        let b = |x: bool| u8::from(x);
        if v.gate != Gate::DEFAULT {
            let g = &v.gate;
            out += &format!(
                "voice audio {audio} gate {} {:?} {:?} {:?} {:?} {:?}\n",
                b(g.on), g.threshold_db, g.attack_ms, g.release_ms, g.hold_ms, g.range_db
            );
        }
        if v.nr != NoiseReduction::default() {
            let n = &v.nr;
            let profile = if n.profile.is_empty() {
                "-".to_string()
            } else {
                n.profile.iter().map(|d| format!("{d:?}")).collect::<Vec<_>>().join(",")
            };
            out += &format!("voice audio {audio} nr {} {:?} {:?} {profile}\n", b(n.on), n.strength, n.floor_db);
        }
        if v.comp != Compressor::DEFAULT {
            let c = &v.comp;
            out += &format!(
                "voice audio {audio} comp {} {:?} {:?} {:?} {:?} {:?} {:?} {}\n",
                b(c.on), c.threshold_db, c.ratio, c.attack_ms, c.release_ms, c.knee_db, c.makeup_db, b(c.auto_makeup)
            );
        }
        if v.deess != DeEsser::DEFAULT {
            let d = &v.deess;
            out += &format!(
                "voice audio {audio} deess {} {:?} {:?} {:?} {:?}\n",
                b(d.on), d.threshold_db, d.amount, d.lo_hz, d.hi_hz
            );
        }
    }
    out.into_bytes()
}

/// The pan lines (v50): `clippan audio <ord> <index> <pan>` for each audio clip that is not in the
/// middle, and `lanepan audio <ord> <pan>` for each audio lane that is not.
fn pan_lines(lanes: &[(LaneKind, Vec<Clip>)], flags: &[LaneFlags]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut audio = 0u32;
    for (n, (kind, clips)) in lanes.iter().enumerate() {
        if *kind != LaneKind::Audio {
            continue;
        }
        audio += 1;
        if let Some(f) = flags.get(n).filter(|f| f.pan != 0.0) {
            out.extend_from_slice(format!("lanepan audio {audio} {}\n", f.pan).as_bytes());
        }
        for (idx, c) in clips.iter().enumerate() {
            if c.pan != 0 {
                out.extend_from_slice(format!("clippan audio {audio} {idx} {}\n", c.pan_value()).as_bytes());
            }
        }
    }
    out
}

/// The pitch lines (v55): `pitch audio <ord> <index> <semitones>` for each audio clip whose
/// pitch shift is not `0`.
fn pitch_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut audio = 0u32;
    for (kind, clips) in lanes {
        if *kind != LaneKind::Audio {
            continue;
        }
        audio += 1;
        for (idx, c) in clips.iter().enumerate() {
            if c.pitch != 0 {
                out.extend_from_slice(format!("pitch audio {audio} {idx} {}\n", c.pitch).as_bytes());
            }
        }
    }
    out
}

/// The `clipgain` lines: one per clip whose own gain is not unity, as
/// `clipgain <kind> <ord> <index> <db>`. `index` counts the lane's clips from
/// `0`, in lane order. Written after the lanes they name, and only for a clip
/// somebody has turned, so a project with none is the bytes a v26 one was bar
/// the version line.
fn clip_gain_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate() {
            if c.gain_db != 0.0 {
                out.extend_from_slice(
                    format!("clipgain {keyword} {ord} {idx} {}\n", c.gain_db).as_bytes(),
                );
            }
        }
    }
    out
}

/// The `gainkeys` lines: one per clip with a volume envelope, as
/// `gainkeys <kind> <ord> <index> <at>:<db> ...`, the points in source order.
/// `kind` and `ord` name the lane the way [`clip_gain_lines`] does; `index`
/// counts the lane's clips from `0`. Written after the lanes they name, and
/// only for a clip with points, so a project with none is the bytes a v27 one
/// was bar the version line.
fn gain_key_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate() {
            if c.gain_keys.is_empty() {
                continue;
            }
            let points: String = c
                .gain_keys
                .iter()
                .map(|k| match k.ease {
                    KeyEase::Linear => format!(" {}:{}", k.at, k.db),
                    ease => format!(" {}:{}:{}", k.at, k.db, ease.token()),
                })
                .collect();
            out.extend_from_slice(format!("gainkeys {keyword} {ord} {idx}{points}\n").as_bytes());
        }
    }
    out
}

/// The `tkeys` lines: one per clip with a placement animation, as
/// `speedkeys <kind> <ord> <index> <at>:<permille> ...`, the keys in frame order. The
/// speeds are thousandths, as a clip's own `speed` field is, so they read back exactly.
/// Only a clip with keys gets a line, and such a project is a v35 one.
fn speed_key_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate() {
            if c.speed_keys.is_empty() {
                continue;
            }
            let points: String = c
                .speed_keys
                .iter()
                .map(|k| format!(" {}:{}", k.at, k.speed.permille()))
                .collect();
            out.extend_from_slice(format!("speedkeys {keyword} {ord} {idx}{points}\n").as_bytes());
        }
    }
    out
}

/// The `transition` lines: one per clip whose transition is not a dissolve, as
/// `transition <kind> <ord> <index> <name>`. Such a project is a v37 one.
fn transition_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate() {
            if c.transition_kind != TransitionKind::Dissolve {
                out.extend_from_slice(
                    format!("transition {keyword} {ord} {idx} {}\n", c.transition_kind.name()).as_bytes(),
                );
            }
        }
    }
    out
}

/// Whether a clip needs the v39 dialect: an opacity or colour animation, or a
/// key easing that v35 has no word for (`in`, `out`, `hold` on a placement key,
/// anything but linear on a volume point).
fn needs_v39(c: &Clip) -> bool {
    !c.opacity_keys.is_empty()
        || !c.color_keys.is_empty()
        || c.gain_keys.iter().any(|k| k.ease != KeyEase::Linear)
        || c.transform_keys
            .iter()
            .any(|k| matches!(k.ease, KeyEase::EaseIn | KeyEase::EaseOut | KeyEase::Hold))
}

/// One line per video clip that `points` returns something for, as
/// `<name> video <ord> <index>{points}`: the shape of the `okeys` and `ckeys` lines.
fn video_key_lines(
    lanes: &[(LaneKind, Vec<Clip>)],
    name: &str,
    points: impl Fn(&Clip) -> String,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut video = 0u32;
    for (kind, clips) in lanes {
        if *kind != LaneKind::Video {
            continue;
        }
        video += 1;
        for (idx, c) in clips.iter().enumerate() {
            let points = points(c);
            if !points.is_empty() {
                out.extend_from_slice(format!("{name} video {video} {idx}{points}\n").as_bytes());
            }
        }
    }
    out
}

/// The `reverse` lines: one per clip that plays backwards, as `reverse <kind> <ord>
/// <index>`. Only a reversed clip gets a line, and such a project is a v38 one.
fn reverse_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, _) in clips.iter().enumerate().filter(|(_, c)| c.reversed) {
            out.extend_from_slice(format!("reverse {keyword} {ord} {idx}\n").as_bytes());
        }
    }
    out
}

/// `deint video <ord> <index> <mode>`: one per clip whose mode is not `frame`, and such a
/// project is a v46 one.
fn deint_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    video_key_lines(lanes, "deint", |c| match c.deinterlace {
        Deinterlace::Frame => String::new(),
        d => format!(" {}", d.name()),
    })
}

/// `lanename <kind> <ord> <name>`: one per lane that has a name.
fn lane_name_lines(lanes: &[(LaneKind, Vec<Clip>)], names: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio, mut sub) = (0u32, 0u32, 0u32);
    for ((kind, _), name) in lanes.iter().zip(names) {
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
                sub += 1;
                ("sub", sub)
            }
        };
        if !name.is_empty() {
            out.extend_from_slice(format!("lanename {keyword} {ord} ").as_bytes());
            escape_text(name, &mut out);
            out.push(b'\n');
        }
    }
    out
}

/// `stab video <ord> <index> <strength> <crop>`: one per stabilised clip, and such a
/// project is a v44 one.
fn stab_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    video_key_lines(lanes, "stab", |c| {
        c.stabilise.map_or(String::new(), |st| format!(" {} {}", st.amount(), st.crop().name()))
    })
}

/// The `mark` lines: one per clip that has a colour label or is switched off, as `mark <kind>
/// <ord> <index> <label> <on|off>` with the label 0 to 8. Only such a clip gets a line, and such a
/// project is a v48 one.
fn mark_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate().filter(|(_, c)| !c.mark.is_default()) {
            let state = if c.mark.off { "off" } else { "on" };
            out.extend_from_slice(format!("mark {keyword} {ord} {idx} {} {state}\n", c.mark.label).as_bytes());
        }
    }
    out
}

/// The `angle` lines: one per clip that shows another angle than the first, as `angle <kind> <ord>
/// <index> <picture> <sound>` with angles counted from 1 and `-` for a sound that follows the
/// picture. Only such a clip gets a line, and such a project is a v47 one.
fn angle_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate().filter(|(_, c)| !c.angle.is_default()) {
            let sound = c.angle.audio.map_or("-".to_string(), |a| (u32::from(a) + 1).to_string());
            out.extend_from_slice(format!("angle {keyword} {ord} {idx} {} {sound}\n", u32::from(c.angle.video) + 1).as_bytes());
        }
    }
    out
}

/// `okeys video <ord> <index> <at>:<ease>:<opacity> ...`, the opacity keys in
/// source order. Only a clip with keys gets a line, and such a project is a v39 one.
fn opacity_key_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    video_key_lines(lanes, "okeys", |c| {
        c.opacity_keys
            .iter()
            .map(|k| format!(" {}:{}:{}", k.at, k.ease.token(), k.opacity))
            .collect()
    })
}

/// `ckeys video <ord> <index> <at>:<ease>:<brightness>,<contrast>,<saturation>,<tint> ...`,
/// the colour keys in source order. Only a clip with keys gets a line, and such
/// a project is a v39 one.
fn color_key_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    video_key_lines(lanes, "ckeys", |c| {
        c.color_keys
            .iter()
            .map(|k| {
                let p = &k.params;
                format!(
                    " {}:{}:{},{},{},{}",
                    k.at,
                    k.ease.token(),
                    p.brightness,
                    p.contrast,
                    p.saturation,
                    p.tint
                )
            })
            .collect()
    })
}

/// The `effect` lines (v42): `effect video <ord> <index> <kind> <on> <value>...`, one per
/// effect of a clip's stack, in stack order. Only a clip with effects gets any.
fn effect_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = String::new();
    let mut video = 0u32;
    for (kind, clips) in lanes {
        if *kind != LaneKind::Video {
            continue;
        }
        video += 1;
        for (idx, c) in clips.iter().enumerate() {
            for e in &c.effects {
                out += &format!("effect video {video} {idx} {} {}", e.kind.name(), u8::from(e.on));
                for v in &e.values[..e.kind.params().len()] {
                    out += &format!(" {v}");
                }
                out.push('\n');
            }
        }
    }
    out.into_bytes()
}

/// The `mkeys` lines (v53): `mkeys video <ord> <index> <slot> <at>:<ease>:<x>,<y>,<w>,<h>,<feather> ...`,
/// the keys of the rect or ellipse mask at stack position `slot`, in source order. Only a
/// mask with keys gets a line.
fn mask_key_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = String::new();
    let mut video = 0u32;
    for (kind, clips) in lanes {
        if *kind != LaneKind::Video {
            continue;
        }
        video += 1;
        for (idx, c) in clips.iter().enumerate() {
            for (slot, e) in c.effects.iter().enumerate() {
                if e.keys.is_empty() {
                    continue;
                }
                out += &format!("mkeys video {video} {idx} {slot}");
                for k in &e.keys {
                    out += &format!(" {}:{}:{},{},{},{},{}", k.at, k.ease.token(), k.x, k.y, k.width, k.height, k.feather);
                }
                out.push('\n');
            }
        }
    }
    out.into_bytes()
}

/// The v43 look lines, one group per colour line that has one, named by the
/// colour line's position:
///
/// * `wheels <color> <lift> <gamma> <gain>`, each `<master>:<red>:<green>:<blue>`,
/// * `curve <color> <channel> <x>:<y>,<x>:<y>...`, one per channel (`master`,
///   `red`, `green`, `blue`) that holds points,
/// * `lut <color> <mix> <path>`, the path written as a source's is (relative
///   when inside the project's folder) and running to the end of the line.
fn look_lines(color: &[ColorParams], dir: &Path) -> Vec<u8> {
    let quad = |q: &[f32; 4]| format!("{:?}:{:?}:{:?}:{:?}", q[0], q[1], q[2], q[3]);
    let mut out = Vec::new();
    for (i, p) in color.iter().enumerate() {
        let look = &p.look;
        if look.wheels != crate::color::Wheels::default() {
            let w = &look.wheels;
            out.extend_from_slice(
                format!("wheels {i} {} {} {}\n", quad(&w.lift), quad(&w.gamma), quad(&w.gain)).as_bytes(),
            );
        }
        for (c, curve) in look.curves.iter().enumerate() {
            if curve.points().is_empty() {
                continue;
            }
            let pts: Vec<String> = curve.points().iter().map(|q| format!("{:?}:{:?}", q[0], q[1])).collect();
            out.extend_from_slice(
                format!("curve {i} {} {}\n", crate::color::CHANNELS[c], pts.join(",")).as_bytes(),
            );
        }
        if let Some(lut) = look.lut {
            out.extend_from_slice(format!("lut {i} {:?} ", look.lut_mix).as_bytes());
            escape(&written_path(&lut.path(), dir), &mut out);
            out.push(b'\n');
        }
    }
    out
}

/// A `wheels`, `curve` or `lut` line (v43): see [`look_lines`]. A LUT file that is
/// missing or broken still loads -- the clip plays without it and
/// [`crate::Project::lut_problems`] names it -- but a malformed line is refused.
fn parse_look(head: &[u8], rest: &[u8], n: usize, dir: &Path, doc: &mut Document) -> crate::Result<()> {
    let sp = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
    let at = number(&rest[..sp], n)? as usize;
    let rest = rest.get(sp + 1..).unwrap_or(&[]);
    let what = String::from_utf8_lossy(head).into_owned();
    if at >= doc.color.len() {
        return Err(format!("line {n}: {what} names color {at}, and there are {}", doc.color.len()).into());
    }
    let look = &mut doc.color[at].look;
    match head {
        b"wheels" => {
            let f = fields(rest, 3, "wheels", n)?;
            let mut q = [[0f32; 4]; 3];
            for (k, field) in f.iter().enumerate() {
                let parts: Vec<&[u8]> = field.split(|&b| b == b':').collect();
                if parts.len() != 4 {
                    return Err(format!("line {n}: a wheel wants 4 values").into());
                }
                for (c, part) in parts.iter().enumerate() {
                    let v = float(part, n)?;
                    if !(-1.0..=1.0).contains(&v) {
                        return Err(format!("line {n}: wheels has a value outside -1..1").into());
                    }
                    q[k][c] = v;
                }
            }
            look.wheels = crate::color::Wheels { lift: q[0], gamma: q[1], gain: q[2] };
        }
        b"curve" => {
            let f = fields(rest, 2, "curve", n)?;
            let channel = crate::color::CHANNELS
                .iter()
                .position(|c| c.as_bytes() == f[0])
                .ok_or_else(|| format!("line {n}: curve channel {:?} is not master, red, green or blue", String::from_utf8_lossy(f[0])))?;
            let mut pts = Vec::new();
            for pair in f[1].split(|&b| b == b',') {
                let xy: Vec<&[u8]> = pair.split(|&b| b == b':').collect();
                if xy.len() != 2 {
                    return Err(format!("line {n}: a curve point is <x>:<y>").into());
                }
                pts.push([float(xy[0], n)?, float(xy[1], n)?]);
            }
            look.curves[channel] = crate::color::Curve::new(&pts).map_err(|e| format!("line {n}: {e}"))?;
        }
        _ => {
            let sp = rest.iter().position(|&b| b == b' ').ok_or_else(|| format!("line {n}: lut wants <mix> <path>"))?;
            let mix = float(&rest[..sp], n)?;
            if !(0.0..=1.0).contains(&mix) {
                return Err(format!("line {n}: lut mix must be 0..=1").into());
            }
            let path = unescape(&rest[sp + 1..], n)?;
            if path.as_os_str().is_empty() {
                return Err(format!("line {n}: lut without a path").into());
            }
            let path = if path.is_absolute() { path } else { dir.join(path) };
            look.lut = Some(crate::cube::LutRef::open(&path));
            look.lut_mix = mix;
        }
    }
    Ok(())
}

/// `tkeys <kind> <ord> <index> <at>:<ease>:<pos_x>,<pos_y>,<scale>,<rotate>,<crop_l>,<crop_r>,<crop_t>,<crop_b>,<opacity> ...`,
/// the keys in source order. `ease` is `lin` or `ease`. Floats are written the
/// way the `transform` table writes them, `{}`, which reads back exactly. Only a
/// clip with keys gets a line, and such a project is a v32 one.
fn tkey_lines(lanes: &[(LaneKind, Vec<Clip>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio) = (0u32, 0u32);
    for (kind, clips) in lanes {
        let (keyword, ord) = match kind {
            LaneKind::Video => {
                video += 1;
                ("video", video)
            }
            LaneKind::Audio => {
                audio += 1;
                ("audio", audio)
            }
            LaneKind::Subtitle => continue,
        };
        for (idx, c) in clips.iter().enumerate() {
            if c.transform_keys.is_empty() {
                continue;
            }
            let points: String = c
                .transform_keys
                .iter()
                .map(|k| {
                    let p = &k.params;
                    let ease = k.ease.token();
                    format!(
                        " {}:{ease}:{},{},{},{},{},{},{},{},{}",
                        k.at,
                        p.pos_x,
                        p.pos_y,
                        p.scale,
                        p.rotate,
                        p.crop_l,
                        p.crop_r,
                        p.crop_t,
                        p.crop_b,
                        p.opacity
                    )
                })
                .collect();
            out.extend_from_slice(format!("tkeys {keyword} {ord} {idx}{points}\n").as_bytes());
        }
    }
    out
}

/// The `chapter` lines: one per chapter, in frame order, as [`save_flagged`]
/// writes them after the titles. A project with none writes nothing.
fn chapter_lines(chapters: &[Chapter]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in chapters {
        out.extend_from_slice(format!("chapter {} ", c.frame).as_bytes());
        escape_text(&c.title, &mut out);
        out.push(b'\n');
    }
    out
}

/// The `marker` lines, one per marker in frame order, then a `marknote` line for each one
/// that has a note, as [`save_flagged`] writes them after the chapters. None for a project with none.
/// The `capstyle` line, `capstyle <animation> <rrggbb> <box 0|1> <uppercase 0|1> <words per
/// line>`: how burned captions animate. None for the default style, so a project that never
/// touched it is the bytes it was before v40, bar the version line.
fn capstyle_line(cs: &CaptionStyle) -> Vec<u8> {
    if *cs == CaptionStyle::default() {
        return Vec::new();
    }
    let [r, g, b] = cs.highlight;
    format!(
        "capstyle {} {r:02x}{g:02x}{b:02x} {} {} {}\n",
        cs.anim.name(),
        u8::from(cs.hl_box),
        u8::from(cs.uppercase),
        cs.words_per_line
    )
    .into_bytes()
}

fn marker_lines(markers: &[Marker]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in markers {
        out.extend_from_slice(format!("marker {} {} {} ", m.frame, m.length, m.color.name()).as_bytes());
        escape_text(&m.name, &mut out);
        out.push(b'\n');
    }
    for (i, m) in markers.iter().enumerate().filter(|(_, m)| !m.note.is_empty()) {
        out.extend_from_slice(format!("marknote {i} ").as_bytes());
        escape_text(&m.note, &mut out);
        out.push(b'\n');
    }
    out
}

/// The `title` lines: one per title, in list order, as [`save_flagged`] writes
/// them. A project with none is the bytes a v25 one was bar the version line.
fn title_lines(titles: &[Title]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in titles {
        let outline = match t.style.outline {
            Outline::None => "none",
            Outline::Thin => "thin",
            Outline::Thick => "thick",
        };
        let place = match t.style.place {
            Place::Bottom => "bottom",
            Place::Top => "top",
            Place::Centre => "centre",
        };
        out.extend_from_slice(
            format!(
                "title {} {} {} {} {} {outline} {} {place} ",
                t.start,
                t.frames,
                t.fade_in,
                t.fade_out,
                t.style.size,
                u8::from(t.style.boxed)
            )
            .as_bytes(),
        );
        escape_text(&t.text, &mut out);
        out.push(b'\n');
    }
    // A title's colour, weight, alignment, position and box colour, one line per title that has
    // any of them off the default, so a project without them is the bytes it was.
    for (i, t) in titles.iter().enumerate() {
        let st = &t.style;
        if title_look_is_default(st) {
            continue;
        }
        let hplace = match st.hplace {
            HPlace::Centre => "centre",
            HPlace::Left => "left",
            HPlace::Right => "right",
        };
        let align = match st.align {
            Align::Centre => "centre",
            Align::Left => "left",
            Align::Right => "right",
        };
        let weight = match st.weight {
            Weight::Regular => "regular",
            Weight::Bold => "bold",
        };
        let [r, g, b] = st.ink;
        let [br, bg, bb, ba] = st.box_color;
        out.extend_from_slice(format!("titlestyle {i} {hplace} {align} {weight} {r:02x}{g:02x}{b:02x} {br:02x}{bg:02x}{bb:02x}{ba:02x}\n").as_bytes());
    }
    // A title's entrance and exit, one line per title that has either.
    for (i, t) in titles.iter().enumerate() {
        if t.in_anim == TitleAnim::None && t.out_anim == TitleAnim::None {
            continue;
        }
        out.extend_from_slice(format!("titleanim {i} {} {} {}\n", t.in_anim.name(), t.out_anim.name(), t.anim_frames).as_bytes());
    }
    out
}

/// Whether a style has none of the v49 title looks: white, regular, centred, on the black box.
fn title_look_is_default(st: &TextStyle) -> bool {
    let d = TextStyle::default();
    st.hplace == d.hplace && st.align == d.align && st.weight == d.weight && st.ink == d.ink && st.box_color == d.box_color
}

/// The `flags` lines: one per lane that has a switch set, in lane order. Only a
/// lane somebody has switched gets one, so a project with none is the bytes a
/// v23 one was bar the version line.
fn flag_lines(lanes: &[(LaneKind, Vec<Clip>)], flags: &[LaneFlags]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut video, mut audio, mut sub) = (0u32, 0u32, 0u32);
    for ((kind, _), f) in lanes.iter().zip(flags) {
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
                sub += 1;
                ("sub", sub)
            }
        };
        let words: Vec<&str> = [
            (f.hidden, "hidden"),
            (f.muted, "muted"),
            (f.locked, "locked"),
        ]
        .into_iter()
        .filter_map(|(on, word)| on.then_some(word))
        .collect();
        if !words.is_empty() {
            out.extend_from_slice(
                format!("flags {keyword} {ord} {}\n", words.join(" ")).as_bytes(),
            );
        }
    }
    out
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
    // The file says which dialect it is by whether it needs the newest one:
    // a transform's opacity is the only thing v31 adds, so a project without
    // one is the bytes a v30 project always was, bar none, and an older reader
    // still opens it.
    // The newest dialect is only for a project that has placement keys
    // (`tkeys` lines, [`tkeys_lines`]); one with opacity alone is still v31.
    let needs_v33 = subtitles.iter().any(|t| !t.language.is_empty());
    let needs_v32 = lanes
        .iter()
        .any(|(_, clips)| clips.iter().any(|c| !c.transform_keys.is_empty()));
    let needs_v31 = transform.iter().any(|p| p.opacity != 1.0);
    out.extend_from_slice(if needs_v33 {
        MAGIC_V33
    } else if needs_v32 {
        MAGIC_V32
    } else if needs_v31 {
        MAGIC_V31
    } else {
        MAGIC_V30
    });
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
        escape(&written_path(&s.path, dir), &mut out);
        out.push(b'\n');
    }
    // The subtitle tracks, by reference: which track of which file, and never
    // the cues (see the module docs). Beside the sources because that is what
    // they are -- a file this project reads from -- although nothing indexes
    // them and their order is only the order they were added in.
    for t in subtitles {
        let track = t.track.map_or("-".to_string(), |n| n.to_string());
        out.extend_from_slice(format!("subtitle {track} ").as_bytes());
        escape(&written_path(&t.path, dir), &mut out);
        out.push(b'\n');
    }
    // The language of each track that has one, as `sublang` lines after the
    // subtitle lines they name: a standalone file's is set by an import or a
    // retype, and an embedded track's is its container's until a retype
    // replaces it. Only a track with one is written, and the header only says
    // v33 when one is, so a project with none is written exactly as before.
    for (track, t) in subtitles.iter().enumerate() {
        if t.language.is_empty() {
            continue;
        }
        out.extend_from_slice(format!("sublang {track} ").as_bytes());
        escape_text(&t.language, &mut out);
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
                BandKind::LowCut => "lc",
                BandKind::HighCut => "hc",
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
        let mut line = format!(
            "transform {:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}",
            p.pos_x, p.pos_y, p.scale, p.rotate, p.crop_l, p.crop_r, p.crop_t, p.crop_b
        );
        // The ninth field only where it says something: an opacity of 1 is the
        // eight-field line every dialect since v20 wrote.
        if p.opacity != 1.0 {
            line.push_str(&format!(":{:?}", p.opacity));
        }
        line.push('\n');
        out.extend_from_slice(line.as_bytes());
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
                    "{keyword} {ord} {} {} {} {} {link} {eq} {color} {} {} {} {} {} {transform} {} {}\n",
                    c.start,
                    c.in_frame,
                    c.out_frame,
                    c.source,
                    fit_name(c.fit),
                    c.speed.permille(),
                    c.fade_in,
                    c.fade_out,
                    c.transition_out,
                    c.visualizer,
                    c.viz_paint,
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
    // The one that carries a clip's volume envelope, as `gainkeys` lines...
    // The one that lets a lane be ducked under several keys, as several `duck`
    // lines naming one lane; a v29 reader refuses it by name.
    // The one that carries a transform's opacity, as a ninth field on a
    // `transform` line; a v30 reader refuses it by its field count.
    // The one that carries a clip's placement animation, as `tkeys` lines...
    // The one that carries a clip's stabilisation, as `stab` lines...
    // The one that carries a clip's pitch shift, as `pitch` lines...
    let v55 = first == MAGIC;
    let v54 = v55 || first == MAGIC_V54;
    let v53 = v54 || first == MAGIC_V53;
    let v52 = v53 || first == MAGIC_V52;
    let v51 = v52 || first == MAGIC_V51;
    let v50 = v51 || first == MAGIC_V50;
    let v49 = v50 || first == MAGIC_V49;
    let v48 = v49 || first == MAGIC_V48;
    let v47 = v48 || first == MAGIC_V47;
    let v46 = v47 || first == MAGIC_V46;
    let v45 = v46 || first == MAGIC_V45;
    let v44 = v45 || first == MAGIC_V44;
    let v43 = v44 || first == MAGIC_V43;
    let v42 = v43 || first == MAGIC_V42;
    let v41 = v42 || first == MAGIC_V41;
    let v40 = v41 || first == MAGIC_V40;
    let v39 = v40 || first == MAGIC_V39;
    let v38 = v39 || first == MAGIC_V38;
    let v37 = v38 || first == MAGIC_V37;
    let v36 = v37 || first == MAGIC_V36;
    let v35 = v36 || first == MAGIC_V35;
    let v34 = v35 || first == MAGIC_V34;
    let v33 = v34 || first == MAGIC_V33;
    let v32 = v33 || first == MAGIC_V32;
    let v31 = v32 || first == MAGIC_V31;
    let v30 = v31 || first == MAGIC_V30;
    let v29 = v30 || first == MAGIC_V29;
    let v28 = v29 || first == MAGIC_V28;
    // The one that carries a clip's own gain in dB, as `clipgain` lines...
    let v27 = v28 || first == MAGIC_V27;
    // The one that carries the title cards and text overlays...
    let v26 = v27 || first == MAGIC_V26;
    // The one that says which lanes are ducked under which...
    let v25 = v26 || first == MAGIC_V25;
    // The one that says which lanes are hidden, muted or locked...
    let v24 = v25 || first == MAGIC_V24;
    // The one that carries a strand colour of its own above the packed
    // visualizer paint (hue/sat, with the top bit as the flag)...
    let v23 = v24 || first == MAGIC_V23;
    // The one that carries packed visualizer paint (hue/sat/strands/glow)...
    let v22 = v23 || first == MAGIC_V22;
    // The one that carries per-clip visualizer flags...
    let v21 = v22 || first == MAGIC_V21;
    // The one that carries a per-clip transform (position/scale/rotate/crop)...
    let v20 = v21 || first == MAGIC_V20;
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
            None => "line 1: not an edith file".to_string(),
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
        // Nothing before v24 could hide, mute or lock a lane: every lane is
        // showing, audible and unlocked, which is what leaving a line out means.
        flags: Vec::new(),
        // Nothing before v25 ducks a lane under another: every lane plays at
        // its own level, which is what leaving a line out means.
        ducks: Vec::new(),
        titles: Vec::new(),
        chapters: Vec::new(),
        markers: Vec::new(),
        caption_style: CaptionStyle::default(),
        voices: Vec::new(),
        names: Vec::new(),
        // Nothing before v10 shows a subtitle, and nothing before v11 was shown
        // in any rendition but the published one.
        subtitles: Vec::new(),
        sub_langs: Vec::new(),
        cue_edits: Vec::new(),
        cue_adds: Vec::new(),
        cue_words: Vec::new(),
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
        seq_active: None,
        seq_order: Vec::new(),
        sequences: Vec::new(),
        mcams: Vec::new(),
        bins: Vec::new(),
        notes: String::new(),
        still_secs: None,
    };
    // The block of a parked sequence being read: its id, name and the lines so far.
    let mut block: Option<(u32, String, Vec<u8>)> = None;
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
        if let Some((id, name, buf)) = block.as_mut() {
            if line == b"endseq" {
                if doc.sequences.len() >= MAX_SEQUENCES {
                    return Err(format!("line {n}: a project holds at most {MAX_SEQUENCES} sequences").into());
                }
                let sub = parse(buf, dir).map_err(|e| format!("sequence \"{name}\": {e}"))?;
                doc.sequences.push(SeqDoc { id: *id, name: std::mem::take(name), doc: sub });
                block = None;
            } else {
                buf.extend_from_slice(line);
                buf.push(b'\n');
            }
            continue;
        }
        match keyword {
            b"seqactive" if v45 => {
                let (id, name) = rest.split_at(rest.iter().position(|&b| b == b' ').unwrap_or(rest.len()));
                let id = number(id, n)?;
                let name = unescape_text(name.strip_prefix(b" ").unwrap_or(name), n)?;
                doc.seq_active = Some((id, name));
            }
            b"seqorder" if v45 => {
                for f in rest.split(|&b| b == b' ').filter(|f| !f.is_empty()) {
                    if doc.seq_order.len() >= MAX_SEQUENCES {
                        return Err(format!("line {n}: seqorder names more than {MAX_SEQUENCES} sequences").into());
                    }
                    doc.seq_order.push(number(f, n)?);
                }
            }
            b"seq" if v45 => {
                let (id, name) = rest.split_at(rest.iter().position(|&b| b == b' ').unwrap_or(rest.len()));
                let id = number(id, n)?;
                let name = unescape_text(name.strip_prefix(b" ").unwrap_or(name), n)?;
                block = Some((id, name, Vec::new()));
            }
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
                    return Err(
                        format!("line {n}: samplerate belongs once, before the sources").into(),
                    );
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
                    return Err(format!("line {n}: proxy belongs once, before the sources").into());
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
                        format!("line {n}: encoder belongs once, before the sources").into(),
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
            // After the lanes, for the reason a gain is: it names one. A line
            // carries the switches that are set, in any order and each once, and
            // a switch the kind of lane cannot have is refused by name.
            // After the lanes, for the reason a gain is: it names one, and the key
            // it names is checked once the whole file is read.
            b"duck" if v25 => {
                let f = fields(rest, 7, "duck", n)?;
                if f[0] != b"audio" {
                    return Err(format!(
                        "line {n}: duck names {:?}, not an audio lane",
                        String::from_utf8_lossy(f[0])
                    )
                    .into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Audio)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: duck names a lane that is not there"))?;
                // 1-based in the file, as a gain's lane is; 0-based in the struct.
                let under = number(f[2], n)?;
                let duck = Duck {
                    under: under.checked_sub(1).ok_or_else(|| {
                        format!("line {n}: duck names audio lane 0, and lanes count from 1")
                    })? as usize,
                    depth_db: float(f[3], n)?,
                    threshold_db: float(f[4], n)?,
                    attack_ms: float(f[5], n)?,
                    release_ms: float(f[6], n)?,
                };
                let in_range = (MIN_DEPTH_DB..=MAX_DEPTH_DB).contains(&duck.depth_db)
                    && (MIN_THRESHOLD_DB..=MAX_THRESHOLD_DB).contains(&duck.threshold_db)
                    && duck.attack_ms >= MIN_TIME_MS
                    && duck.attack_ms <= MAX_ATTACK_MS
                    && duck.release_ms >= MIN_TIME_MS
                    && duck.release_ms <= MAX_RELEASE_MS;
                if !in_range {
                    return Err(format!("line {n}: duck has a number out of its range").into());
                }
                doc.ducks.resize(doc.lanes.len(), Vec::new());
                // Several keys for one lane are a v30 dialect; before it a lane
                // had one duck, and a second line for it is a file that says
                // something its dialect cannot.
                if !v30 && !doc.ducks[at].is_empty() {
                    return Err(format!("line {n}: duck names a lane twice").into());
                }
                if doc.ducks[at].iter().any(|d| d.under == duck.under) {
                    return Err(format!("line {n}: duck names the same key twice").into());
                }
                doc.ducks[at].push(duck);
            }
            b"voice" if v41 => parse_voice(rest, n, &mut doc)?,
            b"effect" if v42 => parse_effect(rest, n, &mut doc)?,
            b"wheels" | b"curve" | b"lut" if v43 => parse_look(keyword, rest, n, dir, &mut doc)?,
            // After the lanes, which is where `clip_gain_lines` writes it: a line
            // names a clip, and a clip is declared by its lane's lines. Only audio
            // clips take one, the setter's own rule.
            b"lanepan" | b"clippan" if v50 => {
                let clip = keyword == b"clippan";
                let f = fields(rest, if clip { 4 } else { 3 }, if clip { "clippan" } else { "lanepan" }, n)?;
                if f[0] != b"audio" {
                    return Err(format!("line {n}: pan names {:?}, not an audio lane", String::from_utf8_lossy(f[0])).into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Audio)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: pan names a lane that is not there"))?;
                let pan = float(f[if clip { 3 } else { 2 }], n)?;
                if !(-1.0..=1.0).contains(&pan) {
                    return Err(format!("line {n}: pan has a number out of its range").into());
                }
                if clip {
                    let idx = number(f[2], n)? as usize;
                    doc.lanes[at].1.get_mut(idx).ok_or_else(|| format!("line {n}: pan names clip {idx}, which the lane does not have"))?.pan = (pan * 100.0).round() as i8;
                } else {
                    doc.flags.resize(doc.lanes.len(), LaneFlags::default());
                    doc.flags[at].pan = pan;
                }
            }
            // After the lanes, which is where `pitch_lines` writes it, as `clippan` is.
            b"pitch" if v55 => {
                let f = fields(rest, 4, "pitch", n)?;
                if f[0] != b"audio" {
                    return Err(format!("line {n}: pitch names {:?}, not an audio lane", String::from_utf8_lossy(f[0])).into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Audio)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: pitch names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let semitones = std::str::from_utf8(f[3]).ok().and_then(|s| s.parse::<i8>().ok());
                let semitones = match semitones {
                    Some(v) if (crate::project::MIN_CLIP_PITCH..=crate::project::MAX_CLIP_PITCH).contains(&v) => v,
                    _ => return Err(format!("line {n}: pitch has a number out of its range").into()),
                };
                doc.lanes[at]
                    .1
                    .get_mut(idx)
                    .ok_or_else(|| format!("line {n}: pitch names clip {idx}, which the lane does not have"))?
                    .pitch = semitones;
            }
            b"clipgain" if v27 => {
                let f = fields(rest, 4, "clipgain", n)?;
                if f[0] != b"audio" {
                    return Err(format!(
                        "line {n}: clipgain names {:?}, not an audio lane",
                        String::from_utf8_lossy(f[0])
                    )
                    .into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Audio)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: clipgain names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let db = float(f[3], n)?;
                if !(MIN_CLIP_GAIN_DB..=MAX_CLIP_GAIN_DB).contains(&db) {
                    return Err(format!("line {n}: clipgain has a number out of its range").into());
                }
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: clipgain names clip {idx}, which the lane does not have")
                })?;
                clip.gain_db = db;
            }
            // After the lanes, which is where `tkey_lines` writes it: a placement
            // key is source-ordered and unique by source frame, at most
            // `MAX_TRANSFORM_KEYS` of them, each a finite placement with an opacity
            // in `0..=1`. Only a video clip can carry them.
            b"tkeys" if v32 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').filter(|f| !f.is_empty()).collect();
                if f.len() < 4 {
                    return Err(format!("line {n}: tkeys wants a clip and a key").into());
                }
                let lane_kind = match f[0] {
                    b"video" => LaneKind::Video,
                    other => {
                        return Err(format!(
                            "line {n}: tkeys names {:?}, not a video lane",
                            String::from_utf8_lossy(other)
                        )
                        .into())
                    }
                };
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == lane_kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: tkeys names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let points = &f[3..];
                if points.len() > MAX_TRANSFORM_KEYS {
                    return Err(
                        format!("line {n}: tkeys has more than {MAX_TRANSFORM_KEYS} keys").into(),
                    );
                }
                let mut keys: Vec<TransformKey> = Vec::with_capacity(points.len());
                for point in points {
                    let parts: Vec<&[u8]> = point.splitn(3, |&b| b == b':').collect();
                    if parts.len() != 3 {
                        return Err(format!("line {n}: tkeys key wants at:ease:values").into());
                    }
                    let ease = KeyEase::from_token(parts[1])
                        .filter(|e| v39 || matches!(e, KeyEase::Linear | KeyEase::Ease))
                        .ok_or_else(|| format!("line {n}: tkeys ease is lin, in, out, ease or hold"))?;
                    let v: Vec<&[u8]> = parts[2].split(|&b| b == b',').collect();
                    if v.len() != 9 {
                        return Err(format!("line {n}: tkeys key wants 9 values").into());
                    }
                    let params = TransformParams {
                        pos_x: float(v[0], n)?,
                        pos_y: float(v[1], n)?,
                        scale: float(v[2], n)?,
                        rotate: float(v[3], n)?,
                        crop_l: float(v[4], n)?,
                        crop_r: float(v[5], n)?,
                        crop_t: float(v[6], n)?,
                        crop_b: float(v[7], n)?,
                        opacity: float(v[8], n)?,
                    };
                    if !(0.0..=1.0).contains(&params.opacity) {
                        return Err(format!("line {n}: tkeys opacity must be 0..=1").into());
                    }
                    let key = TransformKey {
                        at: number(parts[0], n)?,
                        params,
                        ease,
                    };
                    if keys.last().is_some_and(|k| k.at >= key.at) {
                        return Err(
                            format!("line {n}: tkeys keys are not in source order").into()
                        );
                    }
                    keys.push(key);
                }
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: tkeys names clip {idx}, which the lane does not have")
                })?;
                clip.transform_keys = keys;
            }
            // After the lanes, which is where `opacity_key_lines` and `color_key_lines`
            // write them: a video clip's opacity (`0..=1`) and colour animation,
            // source-ordered and unique by source frame, at most `MAX_TRANSFORM_KEYS`
            // and `MAX_COLOR_KEYS` of them, each in range.
            // A stabilised video clip, named as an `okeys` line names its clip.
            b"deint" if v46 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').filter(|f| !f.is_empty()).collect();
                if f.len() != 4 || f[0] != b"video" {
                    return Err(format!("line {n}: deint wants a video clip and a mode").into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Video)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: deint names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let mode = std::str::from_utf8(f[3])
                    .ok()
                    .and_then(Deinterlace::from_name)
                    .ok_or_else(|| format!("line {n}: deint mode must be frame, field or off"))?;
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: deint names clip {idx}, which the lane does not have")
                })?;
                clip.deinterlace = mode;
            }
            b"lanename" if v46 => {
                let mut parts = rest.splitn(3, |&b| b == b' ');
                let kind = match parts.next().unwrap_or_default() {
                    b"video" => LaneKind::Video,
                    b"audio" => LaneKind::Audio,
                    b"sub" => LaneKind::Subtitle,
                    other => {
                        return Err(format!("line {n}: lanename names {:?}, not a lane kind", String::from_utf8_lossy(other)).into());
                    }
                };
                let ord = number(parts.next().unwrap_or_default(), n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: lanename names a lane that is not there"))?;
                let name = unescape_text(parts.next().unwrap_or_default(), n)?;
                if name.is_empty() || name.chars().count() > crate::project::MAX_LANE_NAME_CHARS || name.chars().any(char::is_control) {
                    return Err(format!("line {n}: a lane name is 1 to {} characters", crate::project::MAX_LANE_NAME_CHARS).into());
                }
                doc.names.resize(doc.lanes.len(), String::new());
                doc.names[at] = name;
            }
            b"stab" if v44 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').filter(|f| !f.is_empty()).collect();
                if f.len() != 5 || f[0] != b"video" {
                    return Err(format!("line {n}: stab wants a video clip, a strength and a crop").into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Video)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: stab names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let strength = std::str::from_utf8(f[3])
                    .ok()
                    .and_then(|v| v.parse::<f32>().ok())
                    .filter(|v| (0.0..=1.0).contains(v))
                    .ok_or_else(|| format!("line {n}: stab strength must be a number from 0 to 1"))?;
                let crop = std::str::from_utf8(f[4])
                    .ok()
                    .and_then(crate::stabilise::Crop::from_name)
                    .ok_or_else(|| format!("line {n}: stab crop must be zoom or full"))?;
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: stab names clip {idx}, which the lane does not have")
                })?;
                clip.stabilise = Some(crate::stabilise::Stabilise::new(strength, crop));
            }
            b"mkeys" if v53 => {
                use crate::effects::{EffectKind, MaskKey};
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').filter(|f| !f.is_empty()).collect();
                if f.len() < 5 || f[0] != b"video" {
                    return Err(format!("line {n}: mkeys wants a video clip, a slot and a key").into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Video)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: mkeys names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let slot = number(f[3], n)? as usize;
                let points = &f[4..];
                if points.len() > MAX_TRANSFORM_KEYS {
                    return Err(format!("line {n}: mkeys has more than {MAX_TRANSFORM_KEYS} keys").into());
                }
                let r = EffectKind::RectMask.params();
                let mut keys: Vec<MaskKey> = Vec::new();
                for point in points {
                    let parts: Vec<&[u8]> = point.splitn(3, |&b| b == b':').collect();
                    if parts.len() != 3 {
                        return Err(format!("line {n}: mkeys key wants at:ease:values").into());
                    }
                    let at_frame = number(parts[0], n)?;
                    let ease = KeyEase::from_token(parts[1])
                        .ok_or_else(|| format!("line {n}: mkeys ease is lin, in, out, ease or hold"))?;
                    if keys.last().is_some_and(|k| k.at >= at_frame) {
                        return Err(format!("line {n}: mkeys keys are not in source order").into());
                    }
                    let v: Vec<&[u8]> = parts[2].split(|&b| b == b',').collect();
                    if v.len() != 5 {
                        return Err(format!("line {n}: mkeys key wants 5 values").into());
                    }
                    let mut val = [0.0f32; 5];
                    for (i, (x, field)) in val.iter_mut().zip(&v).enumerate() {
                        *x = float(field, n)?;
                        if !(r[i].min..=r[i].max).contains(x) {
                            return Err(format!("line {n}: mkeys {} is out of its range", r[i].name).into());
                        }
                    }
                    let [x, y, width, height, feather] = val;
                    keys.push(MaskKey { at: at_frame, x, y, width, height, feather, ease });
                }
                let clip = doc.lanes[at]
                    .1
                    .get_mut(idx)
                    .ok_or_else(|| format!("line {n}: mkeys names clip {idx}, which the lane does not have"))?;
                let effect = clip
                    .effects
                    .get_mut(slot)
                    .filter(|e| matches!(e.kind, EffectKind::RectMask | EffectKind::EllipseMask))
                    .ok_or_else(|| format!("line {n}: mkeys names an effect that is not a mask"))?;
                effect.keys = keys;
            }
            b"okeys" | b"ckeys" if v39 => {
                let colour = keyword == b"ckeys";
                let name = if colour { "ckeys" } else { "okeys" };
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').filter(|f| !f.is_empty()).collect();
                if f.len() < 4 || f[0] != b"video" {
                    return Err(format!("line {n}: {name} wants a video clip and a key").into());
                }
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == LaneKind::Video)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: {name} names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let points = &f[3..];
                let cap = if colour { MAX_COLOR_KEYS } else { MAX_TRANSFORM_KEYS };
                if points.len() > cap {
                    return Err(format!("line {n}: {name} has more than {cap} keys").into());
                }
                let mut okeys: Vec<OpacityKey> = Vec::new();
                let mut ckeys: Vec<ColorKey> = Vec::new();
                for point in points {
                    let parts: Vec<&[u8]> = point.splitn(3, |&b| b == b':').collect();
                    if parts.len() != 3 {
                        return Err(format!("line {n}: {name} key wants at:ease:values").into());
                    }
                    let at_frame = number(parts[0], n)?;
                    let ease = KeyEase::from_token(parts[1])
                        .ok_or_else(|| format!("line {n}: {name} ease is lin, in, out, ease or hold"))?;
                    let last = if colour { ckeys.last().map(|k| k.at) } else { okeys.last().map(|k| k.at) };
                    if last.is_some_and(|l| l >= at_frame) {
                        return Err(format!("line {n}: {name} keys are not in source order").into());
                    }
                    if colour {
                        let v: Vec<&[u8]> = parts[2].split(|&b| b == b',').collect();
                        if v.len() != 4 {
                            return Err(format!("line {n}: ckeys key wants 4 values").into());
                        }
                        let params = ColorParams {
                            brightness: float(v[0], n)?,
                            contrast: float(v[1], n)?,
                            saturation: float(v[2], n)?,
                            tint: float(v[3], n)?,
                            ..Default::default()
                        };
                        if !((-1.0..=1.0).contains(&params.brightness)
                            && (0.0..=2.0).contains(&params.contrast)
                            && (0.0..=2.0).contains(&params.saturation)
                            && (-1.0..=1.0).contains(&params.tint))
                        {
                            return Err(format!("line {n}: ckeys has a value out of its range").into());
                        }
                        ckeys.push(ColorKey { at: at_frame, params, ease });
                    } else {
                        let opacity = float(parts[2], n)?;
                        if !(0.0..=1.0).contains(&opacity) {
                            return Err(format!("line {n}: okeys opacity must be 0..=1").into());
                        }
                        okeys.push(OpacityKey { at: at_frame, opacity, ease });
                    }
                }
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: {name} names clip {idx}, which the lane does not have")
                })?;
                if colour {
                    clip.color_keys = ckeys;
                } else {
                    clip.opacity_keys = okeys;
                }
            }
            // After the lanes, which is where `gain_key_lines` writes it: a line
            // names a clip, and a clip is declared by its lane's lines. Its points
            // are source-ordered and unique by source frame, as the setters keep
            // them, and at most `MAX_GAIN_KEYS` of them, each in range.
            b"gainkeys" if v28 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() < 4 {
                    return Err(format!("line {n}: gainkeys wants a clip and a point").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    other => {
                        return Err(format!(
                            "line {n}: gainkeys names {:?}, not a lane kind",
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
                    .ok_or_else(|| format!("line {n}: gainkeys names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let points = &f[3..];
                if points.len() > MAX_GAIN_KEYS {
                    return Err(
                        format!("line {n}: gainkeys has more than {MAX_GAIN_KEYS} points").into(),
                    );
                }
                let mut keys = Vec::with_capacity(points.len());
                for point in points {
                    let colon = point
                        .iter()
                        .position(|&b| b == b':')
                        .ok_or_else(|| format!("line {n}: gainkeys point wants at:db"))?;
                    let (a, d) = (&point[..colon], &point[colon + 1..]);
                    // A v39 point may end in the ease of the segment that leaves it.
                    let (d, ease) = match d.iter().position(|&b| b == b':') {
                        Some(c) if v39 => (
                            &d[..c],
                            KeyEase::from_token(&d[c + 1..])
                                .ok_or_else(|| format!("line {n}: gainkeys ease is lin, in, out, ease or hold"))?,
                        ),
                        _ => (d, KeyEase::Linear),
                    };
                    let key = GainKey {
                        at: number(a, n)?,
                        db: float(d, n)?,
                        ease,
                    };
                    if !(MIN_CLIP_GAIN_DB..=MAX_CLIP_GAIN_DB).contains(&key.db) {
                        return Err(format!("line {n}: gainkeys has a dB out of its range").into());
                    }
                    if keys.last().is_some_and(|k: &GainKey| k.at >= key.at) {
                        return Err(
                            format!("line {n}: gainkeys points are not in source order").into()
                        );
                    }
                    keys.push(key);
                }
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: gainkeys names clip {idx}, which the lane does not have")
                })?;
                clip.gain_keys = keys;
            }
            // After the lanes and the subtitle palette: a title names neither,
            // and its text is the rest of the line, so it is the ninth field.
            b"title" if v26 => {
                let f: Vec<&[u8]> = rest.splitn(9, |&b| b == b' ').collect();
                if f.len() != 9 {
                    return Err(format!("line {n}: title wants 9 fields, found {}", f.len()).into());
                }
                let outline = match f[5] {
                    b"none" => Outline::None,
                    b"thin" => Outline::Thin,
                    b"thick" => Outline::Thick,
                    other => {
                        return Err(format!(
                            "line {n}: title outline {:?} is none, thin or thick",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                let boxed = match f[6] {
                    b"0" => false,
                    b"1" => true,
                    other => {
                        return Err(format!(
                            "line {n}: title boxed {:?} is 0 or 1",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                let place = match f[7] {
                    b"bottom" => Place::Bottom,
                    b"top" => Place::Top,
                    b"centre" => Place::Centre,
                    other => {
                        return Err(format!(
                            "line {n}: title place {:?} is bottom, top or centre",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                let title = Title {
                    start: number(f[0], n)?,
                    frames: number(f[1], n)?,
                    text: unescape_text(f[8], n)?,
                    style: TextStyle {
                        size: number(f[4], n)?,
                        outline,
                        boxed,
                        place,
                        ..TextStyle::default()
                    },
                    fade_in: number(f[2], n)?,
                    fade_out: number(f[3], n)?,
                    in_anim: TitleAnim::None,
                    out_anim: TitleAnim::None,
                    anim_frames: 0,
                };
                check_title(&title).map_err(|e| format!("line {n}: {e}"))?;
                doc.titles.push(title);
            }
            // A chapter is a frame and a name: the name is the rest of the line, escaped
            // as a `cue` line's is. Written in frame order, one per frame.
            // A clip's speed ramp, as `speedkeys` lines, after the lanes they name.
            // Its keys are clip-relative and sorted, at most `MAX_SPEED_KEYS` of them, each
            // speed in range; the clip's footprint grows with the ramp, so the lane is
            // checked again here, where the ramp lands.
            b"speedkeys" if v35 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() < 4 {
                    return Err(format!("line {n}: speedkeys wants a clip and a point").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    other => {
                        return Err(format!(
                            "line {n}: speedkeys names {:?}, not a lane kind",
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
                    .ok_or_else(|| format!("line {n}: speedkeys names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let points = &f[3..];
                if points.len() > MAX_SPEED_KEYS {
                    return Err(format!("line {n}: speedkeys has more than {MAX_SPEED_KEYS} points").into());
                }
                let mut keys = Vec::with_capacity(points.len());
                for point in points {
                    let colon = point
                        .iter()
                        .position(|&b| b == b':')
                        .ok_or_else(|| format!("line {n}: speedkeys point wants at:permille"))?;
                    let (a, p) = (&point[..colon], &point[colon + 1..]);
                    let permille = number(p, n)?;
                    if permille < u32::from(Speed::MIN.permille())
                        || permille > u32::from(Speed::MAX.permille())
                    {
                        return Err(format!("line {n}: speedkeys speed {permille} is out of range").into());
                    }
                    let key = SpeedKey {
                        at: number(a, n)?,
                        speed: Speed::from_permille(permille as u16),
                    };
                    if keys.last().is_some_and(|k: &SpeedKey| k.at >= key.at) {
                        return Err(format!("line {n}: speedkeys points are not in frame order").into());
                    }
                    keys.push(key);
                }
                let lane = &mut doc.lanes[at].1;
                let clip = lane.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: speedkeys names clip {idx}, which the lane does not have")
                })?;
                clip.speed_keys = keys;
                let end = clip.start.checked_add(clip.frames()).ok_or_else(|| {
                    format!("line {n}: speed ramp runs past the last frame there is")
                })?;
                if lane.get(idx + 1).is_some_and(|next| next.start < end) {
                    return Err(format!("line {n}: speed ramp runs into the next clip on its lane").into());
                }
            }
            // How a clip's transition is drawn, after the lanes it names.
            b"transition" if v37 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 4 {
                    return Err(format!("line {n}: transition wants a clip and a kind").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    other => {
                        return Err(format!(
                            "line {n}: transition names {:?}, not a lane kind",
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
                    .ok_or_else(|| format!("line {n}: transition names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let name = String::from_utf8_lossy(f[3]);
                let kind = TransitionKind::parse(&name)
                    .ok_or_else(|| format!("line {n}: transition kind {name:?} is not one"))?;
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: transition names clip {idx}, which the lane does not have")
                })?;
                clip.transition_kind = kind;
            }
            // A reversed clip: named like a `speedkeys` line names its clip. The
            // window, speed and ramp are the clip's own lines; this is the direction.
            b"reverse" if v38 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 3 {
                    return Err(format!("line {n}: reverse wants a lane and a clip").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    other => {
                        return Err(format!(
                            "line {n}: reverse names {:?}, not a lane kind",
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
                    .ok_or_else(|| format!("line {n}: reverse names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: reverse names clip {idx}, which the lane does not have")
                })?;
                clip.reversed = true;
            }
            b"angle" if v47 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 5 {
                    return Err(format!("line {n}: angle wants a lane, a clip, a picture angle and a sound angle").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    _ => return Err(format!("line {n}: angle names a lane kind that is not video or audio").into()),
                };
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: angle names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let one_based = |f: &[u8]| -> crate::Result<u8> {
                    let v = number(f, n)?;
                    u8::try_from(v.wrapping_sub(1))
                        .ok()
                        .filter(|a| usize::from(*a) < crate::project::MAX_ANGLES)
                        .ok_or_else(|| format!("line {n}: an angle is a number from 1 to {}", crate::project::MAX_ANGLES).into())
                };
                let video = one_based(f[3])?;
                let audio = if f[4] == b"-" { None } else { Some(one_based(f[4])?) };
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: angle names clip {idx}, which the lane does not have")
                })?;
                clip.angle = crate::project::Angle { video, audio };
            }
            // A title's look: which title (by its position in the list), where its block sits,
            // how its lines line up, its weight, its colour and its box colour.
            b"titlestyle" if v49 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 6 {
                    return Err(format!("line {n}: titlestyle wants a title, a position, an alignment, a weight and two colours").into());
                }
                let idx = number(f[0], n)? as usize;
                let side = |b: &[u8], what: &str| -> crate::Result<u8> {
                    match b {
                        b"centre" => Ok(0),
                        b"left" => Ok(1),
                        b"right" => Ok(2),
                        other => Err(format!("line {n}: title {what} {:?} is centre, left or right", String::from_utf8_lossy(other)).into()),
                    }
                };
                let hplace = match side(f[1], "position")? {
                    1 => HPlace::Left,
                    2 => HPlace::Right,
                    _ => HPlace::Centre,
                };
                let align = match side(f[2], "alignment")? {
                    1 => Align::Left,
                    2 => Align::Right,
                    _ => Align::Centre,
                };
                let weight = match f[3] {
                    b"regular" => Weight::Regular,
                    b"bold" => Weight::Bold,
                    other => return Err(format!("line {n}: title weight {:?} is regular or bold", String::from_utf8_lossy(other)).into()),
                };
                let hex = |b: &[u8], len: usize| -> crate::Result<Vec<u8>> {
                    let ok = b.len() == len * 2 && b.iter().all(u8::is_ascii_hexdigit);
                    if !ok {
                        return Err(format!("line {n}: a title colour is {} hex digits", len * 2).into());
                    }
                    Ok((0..len).map(|i| u8::from_str_radix(std::str::from_utf8(&b[i * 2..i * 2 + 2]).unwrap_or("0"), 16).unwrap_or(0)).collect())
                };
                let c = hex(f[4], 3)?;
                let bc = hex(f[5], 4)?;
                let title = doc
                    .titles
                    .get_mut(idx)
                    .ok_or_else(|| format!("line {n}: titlestyle names title {idx}, which the file does not have"))?;
                title.style.hplace = hplace;
                title.style.align = align;
                title.style.weight = weight;
                title.style.ink = [c[0], c[1], c[2]];
                title.style.box_color = [bc[0], bc[1], bc[2], bc[3]];
            }
            // A title's entrance and exit: which title (by its place in the list), the two animations
            // and how many frames each takes.
            b"titleanim" if v54 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 4 {
                    return Err(format!("line {n}: titleanim wants a title, an entrance, an exit and a length").into());
                }
                let idx = number(f[0], n)? as usize;
                let anim = |b: &[u8]| {
                    let name = String::from_utf8_lossy(b).into_owned();
                    TitleAnim::from_name(&name).ok_or_else(|| format!("line {n}: title animation {name:?} is not one of the known ones"))
                };
                let (in_anim, out_anim) = (anim(f[1])?, anim(f[2])?);
                let anim_frames = number(f[3], n)?;
                let title = doc
                    .titles
                    .get_mut(idx)
                    .ok_or_else(|| format!("line {n}: titleanim names title {idx}, which the file does not have"))?;
                title.in_anim = in_anim;
                title.out_anim = out_anim;
                title.anim_frames = anim_frames;
                check_title(title).map_err(|e| format!("line {n}: {e}"))?;
            }
            b"mark" if v48 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() != 5 {
                    return Err(format!("line {n}: mark wants a lane, a clip, a label and on or off").into());
                }
                let kind = match f[0] {
                    b"audio" => LaneKind::Audio,
                    b"video" => LaneKind::Video,
                    _ => return Err(format!("line {n}: mark names a lane kind that is not video or audio").into()),
                };
                let ord = number(f[1], n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: mark names a lane that is not there"))?;
                let idx = number(f[2], n)? as usize;
                let label = u8::try_from(number(f[3], n)?)
                    .ok()
                    .filter(|l| *l <= crate::project::LABEL_COUNT)
                    .ok_or_else(|| format!("line {n}: a label is a number from 0 to {}", crate::project::LABEL_COUNT))?;
                let off = match f[4] {
                    b"on" => false,
                    b"off" => true,
                    _ => return Err(format!("line {n}: mark ends with on or off").into()),
                };
                let clip = doc.lanes[at].1.get_mut(idx).ok_or_else(|| {
                    format!("line {n}: mark names clip {idx}, which the lane does not have")
                })?;
                clip.mark = crate::project::ClipMark { label, off };
            }
            // The library (v51): `bin <id> <parent> <name>`, then `srcbin <source> <bin>` and
            // `srcfav <source>` for the sources that are filed or starred.
            // The project notes (v52): one `notes` line, its text escaped as a cue's is, so a
            // note of several lines is one line in the file.
            b"notes" if v52 => {
                if !doc.notes.is_empty() {
                    return Err(format!("line {n}: notes is given twice").into());
                }
                doc.notes = unescape_text(rest, n)?;
            }
            // The length a still is placed at (v52), in seconds, within the range the editor allows.
            b"still" if v52 => {
                let f = fields(rest, 1, "still", n)?;
                let secs: f64 = std::str::from_utf8(f[0]).ok().and_then(|t| t.parse().ok()).ok_or_else(|| format!("line {n}: still wants a length in seconds"))?;
                if !(STILL_MIN_SECS..=STILL_MAX_SECS).contains(&secs) {
                    return Err(format!("line {n}: a still is {STILL_MIN_SECS} to {STILL_MAX_SECS} s long, not {secs}").into());
                }
                doc.still_secs = Some(secs);
            }
            b"bin" if v51 => {
                let f: Vec<&[u8]> = rest.splitn(3, |&b| b == b' ').collect();
                if f.len() != 3 {
                    return Err(format!("line {n}: bin wants an id, a parent and a name").into());
                }
                let id = number(f[0], n)?;
                let parent = number(f[1], n)?;
                let name = unescape_text(f[2], n)?;
                if id == 0 || doc.bins.iter().any(|b| b.id == id) {
                    return Err(format!("line {n}: bin {id} is not a new id").into());
                }
                if parent != 0 && !doc.bins.iter().any(|b| b.id == parent && b.parent == 0) {
                    return Err(format!("line {n}: bin {id} is inside bin {parent}, which is not a top-level bin before it").into());
                }
                let name = crate::project::clean_bin_name(&name).map_err(|e| format!("line {n}: {e}"))?;
                if doc.bins.iter().any(|b| b.parent == parent && b.name.eq_ignore_ascii_case(&name)) {
                    return Err(format!("line {n}: two bins named {name} in one place").into());
                }
                if doc.bins.len() >= crate::project::MAX_BINS {
                    return Err(format!("line {n}: a project holds at most {} bins", crate::project::MAX_BINS).into());
                }
                doc.bins.push(crate::project::Bin { id, parent, name });
            }
            b"srcbin" if v51 => {
                let f = fields(rest, 2, "srcbin", n)?;
                let (src, bin) = (number(f[0], n)? as usize, number(f[1], n)?);
                if !doc.bins.iter().any(|b| b.id == bin) {
                    return Err(format!("line {n}: srcbin names bin {bin}, which is not defined before it").into());
                }
                doc.sources
                    .get_mut(src)
                    .ok_or_else(|| format!("line {n}: srcbin names source {src}, which the file does not have"))?
                    .bin = bin;
            }
            b"srcfav" if v51 => {
                let f = fields(rest, 1, "srcfav", n)?;
                let src = number(f[0], n)? as usize;
                doc.sources
                    .get_mut(src)
                    .ok_or_else(|| format!("line {n}: srcfav names source {src}, which the file does not have"))?
                    .fav = true;
            }
            b"mcam" if v47 => {
                let (id, name) = rest.split_at(rest.iter().position(|&b| b == b' ').unwrap_or(rest.len()));
                let id = number(id, n)?;
                let name = unescape_text(name.strip_prefix(b" ").unwrap_or(name), n)?;
                doc.mcams.push((id, name));
            }
            b"chapter" if v34 => {
                let mut fields = rest.splitn(2, |&b| b == b' ');
                let frame = number(fields.next().unwrap_or_default(), n)?;
                let title = unescape_text(fields.next().unwrap_or_default(), n)?;
                let chapter = Chapter { frame, title };
                check_chapter(&chapter).map_err(|e| format!("line {n}: {e}"))?;
                if doc.chapters.last().is_some_and(|c| c.frame >= frame) {
                    return Err(format!("line {n}: chapter at frame {frame} is not after the one before it").into());
                }
                if doc.chapters.len() >= MAX_LIST_ENTRIES {
                    return Err(format!("line {n}: a project holds at most {MAX_LIST_ENTRIES} chapters").into());
                }
                doc.chapters.push(chapter);
            }
            b"marker" if v36 => {
                let f: Vec<&[u8]> = rest.splitn(4, |&b| b == b' ').collect();
                if f.len() < 4 {
                    return Err(format!("line {n}: marker wants a frame, a length, a colour and a name").into());
                }
                let color = std::str::from_utf8(f[2])
                    .ok()
                    .and_then(MarkerColor::from_name)
                    .ok_or_else(|| format!("line {n}: marker names {:?}, not a colour", String::from_utf8_lossy(f[2])))?;
                let marker = Marker {
                    frame: number(f[0], n)?,
                    length: number(f[1], n)?,
                    name: unescape_text(f[3], n)?,
                    note: String::new(),
                    color,
                };
                check_marker(&marker).map_err(|e| format!("line {n}: {e}"))?;
                if doc.markers.last().is_some_and(|m| m.frame > marker.frame) {
                    return Err(format!("line {n}: marker at frame {} is before the one above it", marker.frame).into());
                }
                if doc.markers.len() >= MAX_LIST_ENTRIES {
                    return Err(format!("line {n}: a project holds at most {MAX_LIST_ENTRIES} markers").into());
                }
                doc.markers.push(marker);
            }
            b"cuewords" if v40 => {
                let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
                if f.len() < 5 || !(f.len() - 2).is_multiple_of(3) {
                    return Err(format!("line {n}: cuewords wants a track, a cue and whole (start, end, word) triples").into());
                }
                if (f.len() - 2) / 3 > MAX_CUE_WORDS {
                    return Err(format!("line {n}: cuewords carries more than {MAX_CUE_WORDS} words").into());
                }
                let track = number(f[0], n)? as usize;
                if track >= doc.subtitles.len() {
                    return Err(format!("line {n}: cuewords names subtitle track {track} of {}", doc.subtitles.len()).into());
                }
                let mut words = Vec::new();
                for w in f[2..].chunks(3) {
                    let time = |b: &[u8]| -> crate::Result<i64> {
                        std::str::from_utf8(b)
                            .ok()
                            .and_then(|t| t.parse::<i64>().ok())
                            .ok_or_else(|| format!("line {n}: cuewords time is not a number").into())
                    };
                    words.push((time(w[0])?, time(w[1])?, unescape_text(w[2], n)?));
                }
                doc.cue_words.push((track, number(f[1], n)? as usize, words));
            }
            b"capstyle" if v40 => {
                let f: Vec<&str> = fields(rest, 5, "capstyle", n)?
                    .into_iter()
                    .map(|b| std::str::from_utf8(b).unwrap_or(""))
                    .collect();
                let anim = CaptionAnim::from_name(f[0])
                    .ok_or_else(|| format!("line {n}: capstyle names {:?}, not an animation", f[0]))?;
                let hex = f[1];
                let rgb = (hex.len() == 6)
                    .then(|| u32::from_str_radix(hex, 16).ok())
                    .flatten()
                    .ok_or_else(|| format!("line {n}: capstyle colour {hex:?} is not six hex digits"))?;
                let flag = |s: &str| match s {
                    "0" => Ok(false),
                    "1" => Ok(true),
                    other => Err(format!("line {n}: capstyle flag {other:?} is not 0 or 1")),
                };
                let words_per_line: u8 = f[4]
                    .parse()
                    .ok()
                    .filter(|&w| w <= MAX_WORDS_PER_LINE)
                    .ok_or_else(|| format!("line {n}: capstyle words per line {:?} is not 0 to {MAX_WORDS_PER_LINE}", f[4]))?;
                doc.caption_style = CaptionStyle {
                    anim,
                    highlight: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8],
                    hl_box: flag(f[2])?,
                    uppercase: flag(f[3])?,
                    words_per_line,
                };
            }
            b"marknote" if v36 => {
                let mut fields = rest.splitn(2, |&b| b == b' ');
                let idx = number(fields.next().unwrap_or_default(), n)? as usize;
                let note = unescape_text(fields.next().unwrap_or_default(), n)?;
                let marker = doc
                    .markers
                    .get_mut(idx)
                    .ok_or_else(|| format!("line {n}: marknote names marker {idx}, which the file does not have"))?;
                marker.note = note;
                check_marker(marker).map_err(|e| format!("line {n}: {e}"))?;
            }
            b"flags" if v24 => {
                let mut words = rest.split(|&b| b == b' ');
                let kind = match words.next().unwrap_or_default() {
                    b"video" => LaneKind::Video,
                    b"audio" => LaneKind::Audio,
                    b"sub" => LaneKind::Subtitle,
                    other => {
                        return Err(format!(
                            "line {n}: flags names {:?}, not a lane kind",
                            String::from_utf8_lossy(other)
                        )
                        .into());
                    }
                };
                let ord = number(words.next().unwrap_or_default(), n)?;
                let at = doc
                    .lanes
                    .iter()
                    .enumerate()
                    .filter(|(_, (k, _))| *k == kind)
                    .nth(ord.saturating_sub(1) as usize)
                    .map(|(at, _)| at)
                    .ok_or_else(|| format!("line {n}: flags names a lane that is not there"))?;
                let mut set = LaneFlags::default();
                let mut any = false;
                for word in words {
                    let (slot, name) = match word {
                        b"hidden" if kind != LaneKind::Audio => (&mut set.hidden, "hidden"),
                        b"muted" if kind == LaneKind::Audio => (&mut set.muted, "muted"),
                        b"locked" => (&mut set.locked, "locked"),
                        other => {
                            return Err(format!(
                                "line {n}: flags {:?} cannot be set on that lane",
                                String::from_utf8_lossy(other)
                            )
                            .into());
                        }
                    };
                    if std::mem::replace(slot, true) {
                        return Err(format!("line {n}: flags names {name} twice").into());
                    }
                    any = true;
                }
                if !any {
                    return Err(format!("line {n}: flags names no switch").into());
                }
                doc.flags.resize(doc.lanes.len(), LaneFlags::default());
                set.pan = doc.flags[at].pan;
                doc.flags[at] = set;
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
            // After the subtitle lines, which declare the tracks it names. The
            // text is the rest of the line, so it may hold spaces: the track and
            // the cue are the two fields before it.
            b"cue" if v24 => {
                let mut fields = rest.splitn(3, |&b| b == b' ');
                let track = number(fields.next().unwrap_or_default(), n)? as usize;
                let cue = number(
                    fields
                        .next()
                        .ok_or_else(|| format!("line {n}: cue without a cue index"))?,
                    n,
                )? as usize;
                let text = fields
                    .next()
                    .ok_or_else(|| format!("line {n}: cue without its text"))?;
                if track >= doc.subtitles.len() {
                    return Err(format!(
                        "line {n}: cue names subtitle track {track} of {}",
                        doc.subtitles.len()
                    )
                    .into());
                }
                doc.cue_edits.push((track, cue, unescape_text(text, n)?));
            }
            // After the subtitle lines, which declare the tracks it names: the
            // track, then the language as the rest of the line, escaped as a cue's
            // text is. A language is one field to a reader, not a list of words.
            b"sublang" if v33 => {
                let mut fields = rest.splitn(2, |&b| b == b' ');
                let track = number(fields.next().unwrap_or_default(), n)? as usize;
                let lang = fields
                    .next()
                    .ok_or_else(|| format!("line {n}: sublang without a language"))?;
                if track >= doc.subtitles.len() {
                    return Err(format!(
                        "line {n}: sublang names subtitle track {track} of {}",
                        doc.subtitles.len()
                    )
                    .into());
                }
                let lang = unescape_text(lang, n)?;
                if lang.is_empty() {
                    return Err(format!("line {n}: sublang without a language").into());
                }
                doc.sub_langs.push((track, lang));
            }
            // After the subtitle lines, which declare the tracks it names, and
            // the `cue` lines, so an added cue lands after the file's own cues.
            // The text is the rest of the line, as a `cue` line's is.
            b"cueadd" if v29 => {
                let mut fields = rest.splitn(4, |&b| b == b' ');
                let mut field = |what: &str| fields.next().ok_or_else(|| format!("line {n}: cueadd without {what}"));
                let track = number(field("a track")?, n)? as usize;
                let start_us = number64(field("a start")?, n)? as i64;
                let end_us = number64(field("an end")?, n)? as i64;
                let text = field("its text")?;
                if track >= doc.subtitles.len() {
                    return Err(format!(
                        "line {n}: cueadd names subtitle track {track} of {}",
                        doc.subtitles.len()
                    )
                    .into());
                }
                if end_us <= start_us {
                    return Err(format!("line {n}: cueadd ends at {end_us} µs, not after its start {start_us} µs").into());
                }
                doc.cue_adds.push(crate::project::AddedCue {
                    track,
                    start_us,
                    end_us,
                    text: unescape_text(text, n)?,
                });
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
                    ..Default::default()
                });
            }
            b"eq" if v5 => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: eq after a clip").into());
                }
                // A bare `eq` is the empty cascade -- a setting that moves
                // nothing, which is not the same thing as no setting at all.
                if rest.split(|&b| b == b' ').count() > MAX_EQ_BANDS {
                    return Err(format!("line {n}: an equalizer holds at most {MAX_EQ_BANDS} bands").into());
                }
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
                    ..Default::default()
                });
            }
            b"transform" if v20 => {
                if doc.lanes.iter().any(|(_, clips)| !clips.is_empty()) {
                    return Err(format!("line {n}: transform after a clip").into());
                }
                let f = fields(rest, 1, "transform", n)?;
                let parts: Vec<&[u8]> = f[0].split(|&b| b == b':').collect();
                // v31 may add the opacity as a ninth field; an earlier dialect
                // has eight, and a ninth there is a line it never wrote.
                let want = if v31 { 8..=9 } else { 8..=8 };
                if !want.contains(&parts.len()) {
                    return Err(format!(
                        "line {n}: transform wants 8 fields, found {}",
                        parts.len()
                    )
                    .into());
                }
                let opacity = match parts.get(8) {
                    Some(p) => float(p, n)?,
                    None => 1.0,
                };
                if !(0.0..=1.0).contains(&opacity) {
                    return Err(format!("line {n}: transform opacity must be 0..=1").into());
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
                    opacity,
                });
            }
            // v1: one lane, no placement, no groups. Every clip becomes one
            // grouped video+audio pair laid where the queue reached, which is
            // what the file always meant.
            b"clip" if v1 => {
                let f = fields(rest, 3, "clip", n)?;
                let clip = check(
                    Clip {
                        speed_keys: Vec::new(),
                        reversed: false,
                        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
                        angle: Default::default(),
                        fade_in: 0,
                        fade_out: 0,
                        transition_out: 0,
                        transition_kind: Default::default(),
                        visualizer: 0,
                        viz_paint: 0,
                        gain_db: 0.0,
                        gain_keys: Vec::new(),
                        pan: 0,
                        transform_keys: Vec::new(),
                        opacity_keys: Vec::new(),
                        color_keys: Vec::new(),
                        effects: Vec::new(),
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
                        pitch: 0,
                    },
                    &doc,
                    n,
                )?;
                queued = clip.end();
                doc.lanes[0].1.push(clip.clone());
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
                // The width of that window, which the check above cannot see:
                // it asks whether the window is *open*, never how wide it is,
                // and a `sub` line is the one number in this format a hand (or
                // a hostile actor) can set to `i64::MAX` and have accepted.
                // Every cut, split, ripple and group trim re-derives the two
                // halves of the window by proportion
                // ([`crate::project::SubClip::window_at`]), a multiply that
                // overflowed on the first frame inside the caption -- a panic
                // on the UI thread the first time a hand ripple-deleted across
                // it, and in a release build a silently wrapped clock. The
                // same bound [`crate::is_resolution`] holds a `resolution`
                // line to, and for the same reason: refused by name at the
                // door rather than clamped into something nobody wrote.
                if sub.out_us - sub.in_us > crate::project::MAX_WINDOW_US {
                    return Err(format!(
                        "line {n}: caption at {} is a {}us window, past the {}us a caption can hold",
                        sub.start,
                        sub.out_us - sub.in_us,
                        crate::project::MAX_WINDOW_US
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
                    + usize::from(v20)
                    + usize::from(v21)
                    + usize::from(v22);
                let f = fields(rest, want, "clip", n)?;
                let in_frame = number(f[1], n)?;
                let out_frame = number(f[2], n)?;
                let speed = speed_of(f.get(8).copied(), n)?;
                // Clamped to the clip's own timeline length,
                // [`Project::set_fade_in`]'s rule: a crafted or hand-edited
                // file asking for more ramp than the clip has frames gets the
                // clip's whole length rather than a refusal.
                // A v35 clip's ramp comes on a later line and may lengthen it, so its
                // fades are clamped once the ramps are in (after the last line).
                let frames = if v35 {
                    u32::MAX
                } else {
                    speed.frames(out_frame.saturating_sub(in_frame))
                };
                let clip = check(
                    Clip {
                        speed_keys: Vec::new(),
                        reversed: false,
                        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
                        angle: Default::default(),
                        fade_in: number(f.get(9).copied().unwrap_or(b"0"), n)?.min(frames),
                        fade_out: number(f.get(10).copied().unwrap_or(b"0"), n)?.min(frames),
                        transition_out: number(f.get(11).copied().unwrap_or(b"0"), n)?.min(frames),
                        transition_kind: Default::default(),
                        visualizer: flags_byte(f.get(13).copied().unwrap_or(b"0"), n)?,
                        viz_paint: number64(f.get(14).copied().unwrap_or(b"0"), n)?,
                        gain_db: 0.0,
                        gain_keys: Vec::new(),
                        pan: 0,
                        transform_keys: Vec::new(),
                        opacity_keys: Vec::new(),
                        color_keys: Vec::new(),
                        effects: Vec::new(),
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
                        pitch: 0,
                    },
                    &doc,
                    n,
                )?;
                let lane = &mut doc.lanes[at].1;
                // The sorted, non-overlapping placement invariant, checked at
                // the only door untrusted clips come in through.
                // A v35 file's `speedkeys` lines follow the clips, so a ramped clip still
                // has its constant footprint here, which a shorter ramp undercuts: only
                // the order is checked at this door, and the footprints after the last line.
                let behind = |prev: &Clip| {
                    if v35 {
                        prev.start >= clip.start
                    } else {
                        prev.end() > clip.start
                    }
                };
                if lane.last().is_some_and(behind) {
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
    // The ramps are in now: every clip ends before the next one starts.
    if v35 {
        for (_, clips) in &mut doc.lanes {
            for c in clips.iter_mut() {
                let frames = c.frames();
                c.fade_in = c.fade_in.min(frames);
                c.fade_out = c.fade_out.min(frames);
                c.transition_out = c.transition_out.min(frames);
            }
            if let Some(w) = clips.windows(2).find(|w| w[0].end() > w[1].start) {
                return Err(format!("clip at {} overlaps the one before it", w[1].start).into());
            }
        }
    }
    if block.is_some() {
        return Err("a sequence block never ends: the file is cut short".into());
    }
    // One per lane whatever the file said, so a caller may zip the two -- and
    // the same for the placements, whose lanes are the very same list.
    doc.gains.resize(doc.lanes.len(), 0.0);
    doc.flags.resize(doc.lanes.len(), LaneFlags::default());
    doc.ducks.resize(doc.lanes.len(), Vec::new());
    doc.voices.resize(doc.lanes.len(), VoiceFx::default());
    doc.names.resize(doc.lanes.len(), String::new());
    doc.subs.resize(doc.lanes.len(), Vec::new());
    // A key is another audio lane of this file, named by its position among
    // them, and never the lane it ducks: a file that says otherwise would ask
    // the mixer to duck under nothing.
    let audio_lanes = doc
        .lanes
        .iter()
        .filter(|(k, _)| *k == LaneKind::Audio)
        .count();
    for (i, lane_ducks) in doc.ducks.iter().enumerate() {
        let own_ord = doc.lanes[..i]
            .iter()
            .filter(|(k, _)| *k == LaneKind::Audio)
            .count();
        for duck in lane_ducks {
            if duck.under >= audio_lanes || duck.under == own_ord {
                return Err(format!(
                    "duck names audio lane {}, which is not another audio lane of this project",
                    duck.under + 1
                )
                .into());
            }
        }
    }
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
    if clip.source >= doc.sources.len() && !crate::project::is_sequence_source(clip.source) {
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

/// An `effect` line (v42): see [`effect_lines`]. The kind must be one, the switch `0` or
/// `1`, every value in its range and a clip's stack no longer than
/// [`crate::effects::MAX_EFFECTS`].
fn parse_effect(rest: &[u8], n: usize, doc: &mut Document) -> crate::Result<()> {
    use crate::effects::{Effect, EffectKind, MAX_EFFECTS, MAX_PARAMS};
    let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
    if f.len() < 5 || f[0] != b"video" {
        return Err(format!("line {n}: effect wants a video clip, a kind and a switch").into());
    }
    let ord = number(f[1], n)?;
    let at = doc
        .lanes
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| *k == LaneKind::Video)
        .nth(ord.saturating_sub(1) as usize)
        .map(|(at, _)| at)
        .ok_or_else(|| format!("line {n}: effect names a lane that is not there"))?;
    let idx = number(f[2], n)? as usize;
    let name = String::from_utf8_lossy(f[3]);
    let kind = EffectKind::from_name(&name).ok_or_else(|| format!("line {n}: effect {name:?} is not one"))?;
    let on = match f[4] {
        b"1" => true,
        b"0" => false,
        _ => return Err(format!("line {n}: effect switch is not 0 or 1").into()),
    };
    let specs = kind.params();
    if f.len() - 5 != specs.len() {
        return Err(format!("line {n}: effect {name} wants {} values, found {}", specs.len(), f.len() - 5).into());
    }
    let mut values = [0.0; MAX_PARAMS];
    for ((v, field), spec) in values.iter_mut().zip(&f[5..]).zip(specs) {
        *v = float(field, n)?;
        if !(spec.min..=spec.max).contains(v) {
            return Err(format!("line {n}: effect {name} {} is out of its range", spec.name).into());
        }
    }
    let clip = doc.lanes[at]
        .1
        .get_mut(idx)
        .ok_or_else(|| format!("line {n}: effect names clip {idx}, which the lane does not have"))?;
    if clip.effects.len() >= MAX_EFFECTS {
        return Err(format!("line {n}: a clip holds at most {MAX_EFFECTS} effects").into());
    }
    clip.effects.push(Effect { kind, on, values, keys: Vec::new() });
    Ok(())
}

/// A `voice` line (v41): see [`voice_lines`]. Every number is range-checked the way the
/// setter clamps it, and a noise reduction that is on must carry its profile.
fn parse_voice(rest: &[u8], n: usize, doc: &mut Document) -> crate::Result<()> {
    let f: Vec<&[u8]> = rest.split(|&b| b == b' ').collect();
    if f.len() < 3 {
        return Err(format!("line {n}: voice wants a lane, a number and a processor").into());
    }
    if f[0] != b"audio" {
        return Err(format!("line {n}: voice names {:?}, not an audio lane", String::from_utf8_lossy(f[0])).into());
    }
    let ord = number(f[1], n)?;
    let at = doc
        .lanes
        .iter()
        .enumerate()
        .filter(|(_, (k, _))| *k == LaneKind::Audio)
        .nth(ord.saturating_sub(1) as usize)
        .map(|(at, _)| at)
        .ok_or_else(|| format!("line {n}: voice names a lane that is not there"))?;
    doc.voices.resize(doc.lanes.len(), VoiceFx::default());
    let flag = |field: &[u8]| match field {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err(format!("line {n}: {:?} is not 0 or 1", String::from_utf8_lossy(field))),
    };
    let want = |count: usize, what: &str| {
        if f.len() == count + 3 {
            Ok(())
        } else {
            Err(format!("line {n}: voice {what} wants {count} fields, found {}", f.len() - 3))
        }
    };
    let range = || format!("line {n}: voice has a number out of its range");
    let voice = &mut doc.voices[at];
    match f[2] {
        b"gate" => {
            want(6, "gate")?;
            let g = Gate {
                on: flag(f[3])?,
                threshold_db: float(f[4], n)?,
                attack_ms: float(f[5], n)?,
                release_ms: float(f[6], n)?,
                hold_ms: float(f[7], n)?,
                range_db: float(f[8], n)?,
            };
            if g.clamped() != Some(g) {
                return Err(range().into());
            }
            voice.gate = g;
        }
        b"nr" => {
            want(4, "nr")?;
            let profile = match f[6] {
                b"-" => Vec::new(),
                list => list.split(|&b| b == b',').map(|v| float(v, n)).collect::<crate::Result<Vec<f32>>>()?,
            };
            let nr = NoiseReduction { on: flag(f[3])?, strength: float(f[4], n)?, floor_db: float(f[5], n)?, profile };
            if nr.clamped().as_ref() != Some(&nr) {
                return Err(range().into());
            }
            if nr.on && nr.profile.is_empty() {
                return Err(format!("line {n}: voice nr is on but has no noise profile").into());
            }
            voice.nr = nr;
        }
        b"comp" => {
            want(8, "comp")?;
            let c = Compressor {
                on: flag(f[3])?,
                threshold_db: float(f[4], n)?,
                ratio: float(f[5], n)?,
                attack_ms: float(f[6], n)?,
                release_ms: float(f[7], n)?,
                knee_db: float(f[8], n)?,
                makeup_db: float(f[9], n)?,
                auto_makeup: flag(f[10])?,
            };
            if c.clamped() != Some(c) {
                return Err(range().into());
            }
            voice.comp = c;
        }
        b"deess" => {
            want(5, "deess")?;
            let d = DeEsser {
                on: flag(f[3])?,
                threshold_db: float(f[4], n)?,
                amount: float(f[5], n)?,
                lo_hz: float(f[6], n)?,
                hi_hz: float(f[7], n)?,
            };
            if d.clamped() != Some(d) {
                return Err(range().into());
            }
            voice.deess = d;
        }
        other => {
            return Err(format!("line {n}: voice names {:?}, not a processor", String::from_utf8_lossy(other)).into());
        }
    }
    Ok(())
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
            b"lc" => BandKind::LowCut,
            b"hc" => BandKind::HighCut,
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

/// A clip's visualizer field: every bit of the byte is a flag
/// ([`crate::decode::VIZ_STYLE_FILL`]'s low bits, the fast bit, the ink
/// nibble), so a line naming one past what a byte holds is a look this
/// program does not have rather than a 300 that quietly loads as a 44.
fn flags_byte(field: &[u8], line: usize) -> crate::Result<u8> {
    let n = number(field, line)?;
    u8::try_from(n).map_err(|_| format!("line {line}: {n} is not a set of visualizer flags").into())
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

/// [`number`] for the one field wider than a `u32`: the clip's packed
/// visualizer look, whose strand-colour half needs the width (v23).
fn number64(field: &[u8], line: usize) -> crate::Result<u64> {
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

/// A `cue` line's text: the backslash that escapes it and the two line breaks,
/// which are the only bytes a line cannot hold (and `\r` is spelled too, so a
/// text ending in one does not look like a CRLF file to a hand-editor).
fn escape_text(text: &str, out: &mut Vec<u8>) {
    for c in text.chars() {
        match c {
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            c => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
}

/// Undoes [`escape_text`]. Anything but the three escapes is refused by name,
/// so a hand-edited typo does not quietly become a different word.
fn unescape_text(field: &[u8], line: usize) -> crate::Result<String> {
    let mut out = Vec::with_capacity(field.len());
    let mut bytes = field.iter().copied();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'\\') => out.push(b'\\'),
            Some(b'n') => out.push(b'\n'),
            Some(b'r') => out.push(b'\r'),
            _ => return Err(format!("line {line}: cue text has a bad escape").into()),
        }
    }
    String::from_utf8(out)
        .map_err(|_| -> crate::Error { format!("line {line}: cue text is not UTF-8").into() })
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
            &[],
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

    /// The v24 `cue` line: a retyped cue's words escape the backslash and the
    /// two line breaks, come back exactly, and a line that names no track it
    /// has, or spells an escape it does not know, is refused by name.
    #[test]
    fn cue_lines_round_trip_their_text_and_refuse_what_they_cannot_name() {
        let dir = PathBuf::from("/proj");
        let doc = parse(
            b"edith 24\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\nsubtitle - subs.srt\n\
              cue 0 2 a \\\\ b\\nc\\r\\nd\ncue 0 0 \n",
            &dir,
        )
        .expect("two cue lines parse");
        assert_eq!(
            doc.cue_edits,
            [(0, 2, "a \\ b\nc\r\nd".to_string()), (0, 0, String::new())]
        );

        // The writer's own spelling reads back as the same text, whatever is in it.
        let text = "back\\slash\nnew\rline \\n is not a newline";
        let mut out = Vec::new();
        escape_text(text, &mut out);
        assert_eq!(unescape_text(&out, 1).expect("its own escapes"), text);

        let refuse = |body: &[u8]| parse(body, &dir).unwrap_err().to_string();
        let head = b"edith 24\nsubtitle - subs.srt\n";
        let with = |line: &[u8]| [head.as_slice(), line].concat();
        assert!(refuse(&with(b"cue 1 0 x\n")).contains("names subtitle track 1 of 1"));
        assert!(refuse(&with(b"cue 0 0 \\q\n")).contains("bad escape"));
        assert!(refuse(&with(b"cue 0 0\n")).contains("without its text"));
        assert!(refuse(&with(b"cue 0\n")).contains("without a cue index"));
        assert!(
            refuse(b"edith 23\nsubtitle - subs.srt\ncue 0 0 x\n").contains("unknown keyword"),
            "a cue line before v24 is not a dialect"
        );
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
        assert!(flat(&dir, &old.sources, &old.lanes, old.playhead).starts_with(b"edith 30\n"));
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

    /// The v33 `sublang` line: a track's language is written after the subtitle
    /// lines and comes back on that track. A project with no language writes no
    /// line and keeps its old header, and a line that names no track, has no
    /// language, or sits in a dialect before v33 is refused.
    #[test]
    fn sublang_lines_round_trip_and_a_project_with_none_is_unchanged() {
        let dir = PathBuf::from("/proj");
        let (_, sources, lanes) = doc();
        let save = |tracks: &[SubtitleTrack]| {
            super::emit(
                &dir,
                &sources,
                &lanes,
                &[],
                &[],
                tracks,
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
        };
        let mut eng = subtitle("/proj/subs.srt", None);
        eng.language = "eng".into();
        let mut fra = subtitle("/proj/a.mp4", Some(3));
        fra.language = "fra".into();
        let bytes = save(&[eng, fra]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("edith 33\n"), "a language makes it v33: {text}");
        assert!(
            text.contains("subtitle - subs.srt\nsubtitle 3 a.mp4\nsublang 0 eng\nsublang 1 fra\n"),
            "{text}"
        );
        let back = parse(&bytes, &dir).expect("v33 parses");
        assert_eq!(back.sub_langs, [(0, "eng".to_string()), (1, "fra".to_string())]);

        // No language anywhere: no line, and the header is the one it was.
        let bare = save(&[subtitle("/proj/subs.srt", None)]);
        let text = String::from_utf8_lossy(&bare);
        assert!(!text.contains("sublang") && !text.starts_with("edith 33"), "{text}");
        assert!(parse(&bare, &dir).expect("parses").sub_langs.is_empty());

        // A v32 file has no languages and loads as it did.
        let v32 = b"edith 32\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\nsubtitle - subs.srt\n";
        assert!(parse(v32, &dir).expect("v32 loads").sub_langs.is_empty());

        // The refusals: a track that is not there, an empty language, and a
        // line in a dialect before v33.
        let head = "edith 33\nsource 0 a.mp4\nsubtitle - subs.srt\n";
        for line in ["sublang 1 eng\n", "sublang 0\n", "sublang x eng\n"] {
            assert!(
                parse(format!("{head}{line}").as_bytes(), &dir).is_err(),
                "{line:?} is not a sublang line"
            );
        }
        let old = b"edith 32\nsource 0 a.mp4\nsubtitle - subs.srt\nsublang 0 eng\n";
        assert_eq!(
            parse(old, &dir).unwrap_err().to_string(),
            "line 4: unknown keyword \"sublang\""
        );
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
        let subs = vec![
            vec![],
            vec![],
            vec![caption(0, 30, Some(7)), caption(40, 10, None)],
        ];
        let bytes = super::emit(
            &dir,
            &doc().1,
            &lanes,
            &[],
            &subs,
            &[subtitle("/proj/subs.srt", None)],
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
            assert!(
                text.contains(&format!("encoder {}\n", seat.name())),
                "{text}"
            );
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
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start,
            in_frame,
            out_frame,
            source,
            link,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            pitch: 0,
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
            ..Default::default()
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
            "edith 30\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\n",
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
            "edith 30\nplayhead 7\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 4 - - fit 1000 0 0 0 - 0 0\naudio 1\n\
             video 2 40 0 10 0 - - - fit 1000 0 0 0 - 0 0\naudio 2 0 0 30 0 4 - - fit 1000 0 0 0 - 0 0\n",
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
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    eq: Some(1),
                    ..clip(30, 10, 20, 0, None)
                },
            ],
            vec![
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    eq: Some(0),
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    eq: Some(2),
                    ..clip(30, 0, 10, 0, None)
                },
            ],
        );
        let bytes = emit(&dir, &sources, &lanes, &eq, &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls 1000.0:4.5:1.0:pk\n\
             eq 16777215.0:-0.1:3.918315e-39:hs\n\
             eq\n\
             video 1 0 0 30 0 0 0 - fit 1000 0 0 0 - 0 0\nvideo 1 30 10 20 0 - 1 - fit 1000 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 0 0 - fit 1000 0 0 0 - 0 0\naudio 1 30 0 10 0 - 2 - fit 1000 0 0 0 - 0 0\n",
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
                ..Default::default()
            },
            // The awkward ones again: all 24 mantissa bits, an exponential
            // spelling, a subnormal and a negative zero -- and the identity,
            // which is a setting like any other.
            ColorParams {
                brightness: -0.000_000_1,
                contrast: 16_777_215.0,
                saturation: f32::MIN_POSITIVE / 3.0,
                tint: -0.0,
                ..Default::default()
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
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
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
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    color: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
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
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             color 0.1:1.2:0.9:-0.3\n\
             color -1e-7:16777215.0:3.918315e-39:-0.0\n\
             color 0.0:1.0:1.0:0.0\n\
             video 1 0 0 30 0 0 - 0 fit 1000 0 0 0 - 0 0\nvideo 1 30 10 20 0 - - 1 fit 1000 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 0 - 0 fit 1000 0 0 0 - 0 0\naudio 1 30 0 10 0 - - 2 fit 1000 0 0 0 - 0 0\n",
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
            "edith 30\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             eq 80.0:-3.0:0.707:ls\n\
             video 1 0 0 30 0 0 0 - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\n"
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
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start,
            in_frame,
            out_frame,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::from_permille(permille),
            pitch: 0,
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
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 2000 0 0 0 - 0 0\nvideo 1 15 30 40 0 - - - fit 250 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 - - - fit 2000 0 0 0 - 0 0\n",
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
            "edith 30\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\n"
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
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 5,
            fade_out: 7,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            pitch: 0,
        };
        let lanes = vec![(LaneKind::Video, vec![faded])];
        let bytes = emit(&dir, &sources, &lanes, &[], &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 5 7 0 - 0 0\n",
            "the fades are the clip line's last two fields"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(
            back.lanes, lanes,
            "the fades round trip as the very numbers"
        );
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
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            fade_in: 0,
            fade_out: 0,
            transition_out: 9,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db: 0.0,
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            pitch: 0,
        };
        let lanes = vec![(LaneKind::Video, vec![dissolving])];
        let bytes = emit(&dir, &sources, &lanes, &[], &[], (1280, 720), 0);
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 9 - 0 0\n",
            "the dissolve is the clip line's own last field"
        );
        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(
            back.lanes, lanes,
            "the dissolve round trips as the very number"
        );
    }

    /// The clip's packed look (v23): a chosen strand colour rides in the
    /// field's own bytes under the top bit and comes back bit-exact, a v22
    /// file's 32-bit value is the low half of that field with no strand
    /// colour, and a file older than the field itself is the zero it always
    /// was.
    #[test]
    fn a_strand_colour_round_trips_and_a_v22_look_stays_32_bit() {
        use crate::decode::{
            VIZ_PAINT_AZURE, viz_hue_ui, viz_sat_ui, viz_strand_set, with_viz_paint_strand_hue,
            with_viz_paint_strand_sat,
        };
        let (dir, sources, lanes) = doc();
        let mut painted = lanes.clone();
        // Azure, its dual half a chosen green rather than the automatic
        // complement of the clip's own hue.
        let chosen = with_viz_paint_strand_sat(with_viz_paint_strand_hue(VIZ_PAINT_AZURE, 64), 200);
        assert!(viz_strand_set(chosen));
        painted[0].1[0].viz_paint = chosen;
        let bytes = flat(&dir, &sources, &painted, 0);
        assert!(
            bytes.starts_with(b"edith 30\n"),
            "a project with a strand colour saves as v23: {}",
            String::from_utf8_lossy(&bytes)
        );
        let back = parse(&bytes, &dir).expect("v23 parses");
        assert_eq!(back.lanes, painted, "the look round-trips bit-exactly");

        // A v22 file's field is a u32: the value lands in the low half
        // untouched, and a strand colour it cannot carry reads as auto.
        let look = VIZ_PAINT_AZURE as u32;
        let v22 = format!(
            "edith 22\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 {look}\n"
        );
        let old = parse(v22.as_bytes(), &dir).expect("v22 still loads");
        let paint = old.lanes[0].1[0].viz_paint;
        assert_eq!(paint, u64::from(look), "the low half is the v22 value");
        assert!(!viz_strand_set(paint), "a v22 look chose no strand colour");
        assert_eq!(viz_hue_ui(paint), 150);
        assert_eq!(viz_sat_ui(paint), 160);
        // ...and a file older than the field is the zero that means the
        // azure default, as it was before the field was widened.
        let v21 = b"edith 21\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0\n";
        let older = parse(v21, &dir).expect("v21 still loads");
        assert_eq!(older.lanes[0].1[0].viz_paint, 0);
    }

    /// A v19 file is a v20 one that places no clip anywhere but its fit
    /// policy's own spot: it loads with every clip's [`Clip::transform`] at
    /// `None`, which is what a v19 file's missing field still means.
    #[test]
    fn a_v19_file_loads_untransformed() {
        let dir = PathBuf::from("/proj");
        // The v19 dialect: twelve clip fields, the last the transition -- no
        // transform field after it at all, not even a `-`.
        let v19 = b"edith 19\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
                    video 1 0 0 30 0 - - - fit 1000 0 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0\n";
        let old = parse(v19, &dir).expect("v19 parses");
        assert!(old.transform.is_empty(), "nothing before v20 has a table");
        assert!(
            old.lanes
                .iter()
                .flat_map(|(_, clips)| clips)
                .all(|c| c.transform.is_none()),
            "nothing before v20 had a placement to name"
        );
    }

    /// [`colours_round_trip_bit_exactly_and_are_shared`]'s twin: two clips
    /// sharing one transform table entry write it once, and both bytes and
    /// values come back bit-exact.
    #[test]
    fn transforms_round_trip_bit_exactly_and_are_shared() {
        let dir = PathBuf::from("/proj");
        let sources = vec![source("/proj/a.mp4", 0)];
        let transform = vec![
            TransformParams {
                pos_x: 0.1,
                pos_y: -0.2,
                scale: 1.5,
                rotate: 90.0,
                crop_l: 0.1,
                crop_r: 0.0,
                crop_t: 0.0,
                crop_b: 0.05,
                opacity: 0.25,
            },
            // The awkward ones again: all 24 mantissa bits, an exponential
            // spelling, a subnormal and a negative zero -- and the identity,
            // which is a setting like any other.
            TransformParams {
                pos_x: -0.000_000_1,
                pos_y: 16_777_215.0,
                scale: f32::MIN_POSITIVE / 3.0,
                rotate: -0.0,
                crop_l: 0.0,
                crop_r: 0.0,
                crop_t: 0.0,
                crop_b: 0.0,
                opacity: 1.0,
            },
            TransformParams::default(),
        ];
        // Two clips share entry 0, so it is written once; one clip is untransformed.
        let lanes = two(
            vec![
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    transform: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    transform: Some(1),
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
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    transform: Some(0),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(0, 0, 30, 0, Some(0))
                },
                Clip {
                    fade_in: 0,
                    fade_out: 0,
                    transition_out: 0,
                    transition_kind: Default::default(),
                    visualizer: 0,
                    viz_paint: 0,
                    gain_db: 0.0,
                    transform: Some(2),
                    fit: FitPolicy::default(),
                    speed: Speed::NORMAL,
                    ..clip(30, 0, 10, 0, None)
                },
            ],
        );
        let bytes = super::emit(
            &dir,
            &sources,
            &lanes,
            &[],
            &[],
            &[],
            &[],
            &[],
            &transform,
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
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "edith 31\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             transform 0.1:-0.2:1.5:90.0:0.1:0.0:0.0:0.05:0.25\n\
             transform -1e-7:16777215.0:3.918315e-39:-0.0:0.0:0.0:0.0:0.0\n\
             transform 0.0:0.0:1.0:0.0:0.0:0.0:0.0:0.0\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 0 0 0\nvideo 1 30 10 20 0 - - - fit 1000 0 0 0 1 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0 0 0 0\naudio 1 30 0 10 0 - - - fit 1000 0 0 0 2 0 0\n",
            "the table comes before the clips, and a clip names a line of it"
        );

        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.lanes, lanes, "every clip keeps the transform it named");
        for (i, (got, want)) in back.transform.iter().zip(&transform).enumerate() {
            assert_eq!(
                (
                    got.pos_x.to_bits(),
                    got.pos_y.to_bits(),
                    got.scale.to_bits(),
                    got.rotate.to_bits(),
                    got.crop_l.to_bits(),
                    got.crop_r.to_bits(),
                    got.crop_t.to_bits(),
                    got.crop_b.to_bits(),
                    got.opacity.to_bits(),
                ),
                (
                    want.pos_x.to_bits(),
                    want.pos_y.to_bits(),
                    want.scale.to_bits(),
                    want.rotate.to_bits(),
                    want.crop_l.to_bits(),
                    want.crop_r.to_bits(),
                    want.crop_t.to_bits(),
                    want.crop_b.to_bits(),
                    want.opacity.to_bits(),
                ),
                "transform {i} came back as other bits"
            );
        }
        assert_eq!(back.transform.len(), 3);
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
            "edith 30\nplayhead 0\nresolution 1280 720\nfps 23.976023976023978\n\
             limiter -1.5 on\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
             audio 2 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
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
            "edith 30\nplayhead 3\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n"
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

    /// The cut shapes are two more spellings of the same band grammar: they
    /// round-trip bit-exactly like the others, and a cut's gain is written as
    /// it was given even though the filter ignores it.
    #[test]
    fn cut_bands_round_trip_as_lc_and_hc() {
        let dir = PathBuf::from("/proj");
        let sources = vec![source("/proj/a.mp4", 0)];
        let eq = vec![EqParams {
            bands: vec![
                band_of(80.0, 0.0, 0.707, BandKind::LowCut),
                band_of(12_000.0, -6.5, 0.5, BandKind::HighCut),
            ],
        }];
        let lanes = two(vec![clip(0, 0, 30, 0, Some(0))], Vec::new());
        let bytes = emit(&dir, &sources, &lanes, &eq, &[], (1280, 720), 0);
        assert!(
            String::from_utf8_lossy(&bytes).contains("eq 80.0:0.0:0.707:lc 12000.0:-6.5:0.5:hc\n"),
            "the cut shapes are spelled lc and hc"
        );

        let back = parse(&bytes, &dir).expect("parse");
        assert_eq!(back.eq, eq, "both cut bands come back as they went in");
        assert_eq!(back.lanes, lanes);
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
            "edith 30\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 2 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\n"
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
            "edith 30\nplayhead 12\nresolution 1280 720\nsource 0 a.mp4\n\
             source 0 /elsewhere/b.mp4\n\
             video 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\nvideo 1 30 10 20 1 1 - - fit 1000 0 0 0 - 0 0\n\
             audio 1 0 0 30 0 0 - - fit 1000 0 0 0 - 0 0\n",
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
        assert!(v5.starts_with(b"edith 30\n"));
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
            audio_stream: 1, ..Default::default()
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
            "edith 30\nplayhead 0\nresolution 1280 720\nsource 1 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n"
        );
        // Loading rejoins the *given* directory, so the file is reached by the
        // way the project was opened -- the same file, through the link, still
        // playing the stream it was saved on.
        assert_eq!(
            load(&path).expect("load").sources,
            vec![Source {
                path: dir.join("link/a.mp4"),
                audio_stream: 1, ..Default::default()
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
            audio_stream: 0, ..Default::default()
        };
        let bytes = flat(
            &dir,
            std::slice::from_ref(&nasty),
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

    /// A `flags` line names its lane the way a gain does and carries only the
    /// switches its kind of lane can have. A lane with none has no line, so a
    /// v23 file -- which has no such line -- parses with every switch clear, and
    /// one that carries the line before v24 is an unknown keyword.
    #[test]
    fn lane_switches_parse_by_kind_and_a_v23_file_has_none() {
        let dir = PathBuf::from("/proj");
        let doc = parse(
            b"edith 24\nvideo 1\naudio 1\nsub 1\nflags video 1 hidden locked\nflags audio 1 muted\nflags sub 1 hidden\n",
            &dir,
        )
        .expect("a v24 file with switches parses");
        assert_eq!(
            doc.flags,
            vec![
                LaneFlags {
                    hidden: true,
                    muted: false,
                    locked: true,
                    pan: 0.0,
                },
                LaneFlags {
                    hidden: false,
                    muted: true,
                    locked: false,
                    pan: 0.0,
                },
                LaneFlags {
                    hidden: true,
                    muted: false,
                    locked: false,
                    pan: 0.0,
                },
            ]
        );

        let plain = parse(b"edith 24\nvideo 1\naudio 1\n", &dir).expect("no switches");
        assert_eq!(plain.flags, vec![LaneFlags::default(); 2]);
        let old = parse(b"edith 23\nvideo 1\naudio 1\n", &dir).expect("a v23 file loads");
        assert_eq!(
            old.flags,
            vec![LaneFlags::default(); 2],
            "every switch clear"
        );

        let refuse = |body: &[u8]| parse(body, &dir).unwrap_err().to_string();
        assert!(
            refuse(b"edith 24\nvideo 1\nflags video 1 hidden hidden\n").contains("twice"),
            "a switch named twice"
        );
        assert!(
            refuse(b"edith 24\naudio 1\nflags audio 1 hidden\n").contains("cannot be set"),
            "a hidden audio lane"
        );
        assert!(
            refuse(b"edith 24\nvideo 1\nflags video 1 muted\n").contains("cannot be set"),
            "a muted video lane"
        );
        assert!(
            refuse(b"edith 24\nvideo 1\nflags video 2 hidden\n").contains("not there"),
            "a lane that is not there"
        );
        assert!(
            refuse(b"edith 24\nvideo 1\nflags video 1\n").contains("no switch"),
            "a line with no switch"
        );
        assert!(
            refuse(b"edith 23\nvideo 1\nflags video 1 hidden\n").contains("unknown keyword"),
            "a flags line before v24 is not a dialect"
        );
    }

    #[test]
    fn a_duck_line_round_trips_and_is_bounded_by_name() {
        let dir = PathBuf::from("/proj");
        let head = "edith 25\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 2 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let ok = parse(
            format!("{head}duck audio 2 1 -12.0 -40.0 80.0 400.0\n").as_bytes(),
            &dir,
        )
        .expect("a duck of A2 under A1 parses");
        assert_eq!(
            ok.ducks,
            vec![
                vec![],
                vec![],
                vec![crate::duck::Duck {
                    under: 0,
                    depth_db: -12.0,
                    threshold_db: -40.0,
                    attack_ms: 80.0,
                    release_ms: 400.0,
                }]
            ],
            "one list per lane, the key named 0-based in the struct"
        );
        let refuse = |line: &str| {
            parse(format!("{head}{line}\n").as_bytes(), &dir)
                .unwrap_err()
                .to_string()
        };
        assert!(refuse("duck audio 2 2 -12.0 -40.0 80.0 400.0").contains("not another audio lane"));
        assert!(refuse("duck audio 2 3 -12.0 -40.0 80.0 400.0").contains("not another audio lane"));
        assert!(refuse("duck audio 2 0 -12.0 -40.0 80.0 400.0").contains("lane 0"));
        assert!(refuse("duck audio 2 1 -41.0 -40.0 80.0 400.0").contains("out of its range"));
        assert!(refuse("duck audio 2 1 -12.0 -40.0 0.0 400.0").contains("out of its range"));
        assert!(refuse("duck video 1 1 -12.0 -40.0 80.0 400.0").contains("not an audio lane"));
        // A dialect before v25 has no ducks, so the same line is an unknown one.
        let v24 = format!(
            "{}duck audio 2 1 -12.0 -40.0 80.0 400.0\n",
            head.replacen("edith 25", "edith 24", 1)
        );
        assert!(
            parse(v24.as_bytes(), &dir)
                .unwrap_err()
                .to_string()
                .contains("unknown keyword"),
            "a v24 file cannot carry a duck"
        );
        // ...and a v24 file without one still loads, with every lane unducked.
        let old = parse(head.replacen("edith 25", "edith 24", 1).as_bytes(), &dir)
            .expect("v24 still loads");
        assert!(old.ducks.iter().all(Vec::is_empty));
    }

    /// The v42 `effect` lines: a clip's whole stack round-trips in order, switch and
    /// every value included, through the same `save_flagged` every save takes; a project
    /// with no effect keeps its older header; a v41 file refuses the line; a bad line is
    /// refused by name.
    #[test]
    fn effect_lines_round_trip_in_v42_and_older_files_refuse_them() {
        use crate::effects::{Effect, EffectKind};
        let dir = std::env::temp_dir().join(format!("edith-effects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fx.edith");
        let src = vec![Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() }];
        let save = |lanes: &[(LaneKind, Vec<Clip>)]| {
            save_flagged(
                &path, &src, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let plain = two(vec![clip(0, 0, 30, 0, Some(0))], vec![clip(0, 0, 30, 0, Some(0))]);
        let before = save(&plain);
        assert!(!before.starts_with("edith 42") && !before.contains("effect"), "{before}");
        let mut lanes = plain.clone();
        let mut blur = Effect::new(EffectKind::Blur);
        blur.set("radius", 12.25).unwrap();
        let mut key = Effect::new(EffectKind::ChromaKey);
        key.on = false;
        key.set("tolerance", 0.123_456_7).unwrap();
        let stack = vec![Effect::new(EffectKind::Vignette), blur, key, Effect::new(EffectKind::EllipseMask)];
        lanes[0].1[0].effects = stack.clone();
        let text = save(&lanes);
        assert!(text.starts_with("edith 42\n"), "an effect makes it v42: {text}");
        assert!(text.contains("\neffect video 1 0 blur 1 12.25\n"), "{text}");
        assert!(text.contains("\neffect video 1 0 chroma_key 0 0 255 0 0.1234567 0.1 0.5\n"), "{text}");
        let back = load(&path).expect("v42 loads");
        assert_eq!(back.lanes[0].1[0].effects, stack, "order, switches and values survive");
        assert_eq!(save(&back.lanes), text, "saved again: the same bytes");
        lanes[0].1[0].effects.clear();
        assert_eq!(save(&lanes), before, "no effect: the bytes it was");
        std::fs::remove_dir_all(&dir).ok();

        let dir = PathBuf::from("/proj");
        let video_head = "edith 42\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let doc = parse(format!("{video_head}effect video 1 0 sharpen 1 0.5\n").as_bytes(), &dir).expect("parses");
        assert_eq!(doc.lanes[0].1[0].effects[0].get("amount"), Some(0.5));
        let refuse = |line: &str| parse(format!("{video_head}{line}\n").as_bytes(), &dir).unwrap_err().to_string();
        assert!(refuse("effect video 1 0 sparkle 1 0.5").contains("not one"));
        assert!(refuse("effect video 1 0 sharpen 2 0.5").contains("not 0 or 1"));
        assert!(refuse("effect video 1 0 sharpen 1 9").contains("out of its range"));
        assert!(refuse("effect video 1 0 vignette 1 0.5").contains("wants 2 values"));
        assert!(refuse("effect video 1 4 sharpen 1 0.5").contains("does not have"));
        assert!(refuse("effect audio 1 0 sharpen 1 0.5").contains("wants a video clip"));
        assert!(refuse("effect video 2 0 sharpen 1 0.5").contains("not there"));
        let v41 = video_head.replacen("edith 42", "edith 41", 1);
        let err = parse(format!("{v41}effect video 1 0 sharpen 1 0.5\n").as_bytes(), &dir).unwrap_err().to_string();
        assert!(err.contains("unknown keyword \"effect\""), "{err}");
    }

    /// The v50 pan lines: a clip pan and a lane pan round-trip, a project with neither keeps its
    /// header and its bytes, and an out-of-range or unknown-lane pan is refused by name.
    #[test]
    fn pan_lines_round_trip_in_v50() {
        let dir = std::env::temp_dir().join(format!("edith-pan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pan.edith");
        let src = vec![Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() }];
        let save = |lanes: &[(LaneKind, Vec<Clip>)], flags: &[LaneFlags]| {
            save_flagged(
                &path, &src, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, flags, &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let plain = two(vec![clip(0, 0, 30, 0, Some(0))], vec![clip(0, 0, 30, 0, Some(0)), clip(30, 0, 30, 0, Some(0))]);
        let before = save(&plain, &[]);
        assert!(!before.contains("pan"), "{before}");
        let mut lanes = plain.clone();
        lanes[1].1[1].pan = -25;
        let flags = [LaneFlags::default(), LaneFlags { pan: 0.5, locked: true, ..LaneFlags::default() }];
        let text = save(&lanes, &flags);
        assert!(text.starts_with("edith 50\n"), "{text}");
        assert!(text.contains("\nlanepan audio 1 0.5\n") && text.contains("\nclippan audio 1 1 -0.25\n"), "{text}");
        let back = load(&path).expect("v50 loads");
        assert_eq!(back.lanes[1].1[1].pan, -25);
        assert_eq!(back.lanes[1].1[0].pan, 0);
        assert_eq!(back.flags[1].pan, 0.5);
        assert!(back.flags[1].locked, "the switches stay");
        assert_eq!(save(&back.lanes, &back.flags), text, "saved again: the same bytes");
        let dirp = PathBuf::from("/proj");
        let head = "edith 50\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let refuse = |line: &str| parse(format!("{head}{line}\n").as_bytes(), &dirp).unwrap_err().to_string();
        assert!(refuse("clippan audio 1 0 2").contains("out of its range"));
        assert!(refuse("clippan audio 1 4 0.5").contains("does not have"));
        assert!(refuse("lanepan audio 3 0.5").contains("not there"));
        assert!(refuse("lanepan video 1 0.5").contains("not an audio lane"));
        let old = head.replacen("edith 50", "edith 49", 1);
        let err = parse(format!("{old}lanepan audio 1 0.5\n").as_bytes(), &dirp).unwrap_err().to_string();
        assert!(err.contains("unknown keyword"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }


    /// The v55 pitch lines: a clip's semitones round-trip and a project with none is the bytes a
    /// v51 one was; an out-of-range, unknown-lane or unknown-clip pitch is refused by name, and a
    /// pitch line under an older header is refused as unknown.
    #[test]
    fn pitch_lines_round_trip_in_v55() {
        let dir = std::env::temp_dir().join(format!("edith-pitch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pitch.edith");
        let src = vec![Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() }];
        let save = |lanes: &[(LaneKind, Vec<Clip>)]| {
            save_flagged(
                &path, &src, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let plain = two(vec![clip(0, 0, 30, 0, Some(0))], vec![clip(0, 0, 30, 0, Some(0)), clip(30, 0, 30, 0, Some(0))]);
        let before = save(&plain);
        assert!(!before.contains("pitch"), "{before}");
        assert!(!before.starts_with("edith 55"), "{before}");
        let mut lanes = plain.clone();
        lanes[1].1[1].pitch = -7;
        let text = save(&lanes);
        assert!(text.starts_with("edith 55\n"), "{text}");
        assert!(text.contains("\npitch audio 1 1 -7\n"), "{text}");
        let back = load(&path).expect("v55 loads");
        assert_eq!(back.lanes[1].1[1].pitch, -7);
        assert_eq!(back.lanes[1].1[0].pitch, 0);
        assert_eq!(save(&back.lanes), text, "saved again: the same bytes");
        let dirp = PathBuf::from("/proj");
        let head = "edith 55\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let refuse = |line: &str| parse(format!("{head}{line}\n").as_bytes(), &dirp).unwrap_err().to_string();
        assert!(refuse("pitch audio 1 0 13").contains("out of its range"));
        assert!(refuse("pitch audio 1 0 1.5").contains("out of its range"));
        assert!(refuse("pitch audio 1 4 3").contains("does not have"));
        assert!(refuse("pitch audio 3 0 3").contains("not there"));
        assert!(refuse("pitch video 1 0 3").contains("not an audio lane"));
        let old = head.replacen("edith 55", "edith 51", 1);
        let err = parse(format!("{old}pitch audio 1 0 3\n").as_bytes(), &dirp).unwrap_err().to_string();
        assert!(err.contains("unknown keyword"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The v51 library lines: bins, filed sources and starred ones round-trip; a project with
    /// none keeps its v50 bytes; the lines are refused in v50 and bad ones by name.
    #[test]
    fn library_lines_round_trip_with_mask_keys_in_v53() {
        use crate::project::Bin;
        let dir = std::env::temp_dir().join(format!("edith-bins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bins.edith");
        let mut src = vec![
            Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() },
            Source { path: dir.join("b.mp4"), audio_stream: 0, ..Default::default() },
        ];
        let mut lanes = two(vec![clip(0, 0, 30, 0, Some(0))], vec![clip(0, 0, 30, 0, Some(0))]);
        let mut mask = crate::effects::Effect::new(crate::effects::EffectKind::EllipseMask);
        mask.keys = vec![crate::effects::MaskKey {
            at: 0, x: 0.5, y: 0.5, width: 0.4, height: 0.4, feather: 0.02, ease: KeyEase::Linear,
        }];
        lanes[0].1[0].effects = vec![mask];
        let save = |src: &[Source], bins: &[Bin]| {
            let main = encode_flagged(
                &dir, src, &lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            );
            let bytes = noted(binned(main, src, bins), "Two cuts", STILL_DEFAULT_SECS);
            std::fs::write(&path, &bytes).unwrap();
            String::from_utf8(bytes).unwrap()
        };
        let plain = save(&src, &[]);
        assert!(plain.starts_with("edith 45\n") || !plain.starts_with("edith 51"), "{plain}");
        assert!(!plain.contains("bin") && !plain.contains("srcfav"), "{plain}");
        let bins = vec![
            Bin { id: 1, parent: 0, name: "Interviews \"A\"".into() },
            Bin { id: 3, parent: 1, name: "B roll".into() },
        ];
        src[0].bin = 3;
        src[1].fav = true;
        src[1].bin = 1;
        let text = save(&src, &bins);
        // A mask key is v53, and the library and the notes lines keep it there.
        assert!(text.starts_with("edith 53\n"), "{text}");
        assert!(text.contains("\nsrcbin 0 3\n") && text.contains("\nsrcfav 1\n"), "{text}");
        assert!(text.contains("\nmkeys video ") && text.contains("\nnotes Two cuts\n"), "{text}");
        let back = load(&path).expect("v53 loads");
        assert_eq!(back.notes, "Two cuts");
        assert_eq!(back.bins, bins);
        assert_eq!((back.sources[0].bin, back.sources[0].fav), (3, false));
        assert_eq!((back.sources[1].bin, back.sources[1].fav), (1, true));
        assert_eq!(save(&back.sources, &back.bins), text, "saved again: the same bytes");
        let dirp = PathBuf::from("/proj");
        let head = "edith 51\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let refuse = |line: &str| parse(format!("{head}{line}\n").as_bytes(), &dirp).unwrap_err().to_string();
        assert!(refuse("bin 1 0").contains("wants an id"));
        assert!(refuse("bin 0 0 x").contains("not a new id"));
        assert!(refuse("bin 1 0 x\nbin 1 0 y").contains("not a new id"));
        assert!(refuse("bin 2 1 x").contains("not a top-level bin"));
        assert!(refuse("bin 1 0 x\nbin 2 0 X").contains("two bins named"));
        assert!(refuse("bin 1 0 x\nbin 2 1 y\nbin 3 2 z").contains("not a top-level bin"));
        assert!(refuse("srcbin 0 9").contains("not defined"));
        assert!(refuse("bin 1 0 x\nsrcbin 4 1").contains("does not have"));
        assert!(refuse("srcfav 7").contains("does not have"));
        let old = head.replacen("edith 51", "edith 50", 1);
        for line in ["bin 1 0 x", "srcfav 0"] {
            let err = parse(format!("{old}{line}\n").as_bytes(), &dirp).unwrap_err().to_string();
            assert!(err.contains("unknown keyword"), "{err}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The v41 `voice` lines: every processor of an audio lane (the noise profile included)
    /// round-trips bit for bit; a lane with no voice setting writes none, so a project nobody
    /// has cleaned keeps its older header and its exact bytes; a v40 file loads with every
    /// processor off and refuses the lines; a number out of range is refused by name.
    /// The v43 look lines: wheels, curves and a LUT path (relative inside the
    /// project folder) round trip bit-exactly; a grade without a look writes none
    /// and stays under its old header; a missing LUT still loads; bad lines are
    /// refused by name.
    #[test]
    fn look_lines_round_trip_in_v43() {
        use crate::color::{Curve, Look, Wheels};
        let dir = std::env::temp_dir().join(format!("edith-look-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("luts")).unwrap();
        let cube = dir.join("luts/warm look.cube");
        std::fs::write(&cube, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
        let path = dir.join("look.edith");
        let src = vec![Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() }];
        let lanes = two(
            vec![Clip { color: Some(0), ..clip(0, 0, 30, 0, Some(0)) }, Clip { color: Some(1), ..clip(30, 0, 30, 0, Some(0)) }],
            vec![clip(0, 0, 60, 0, Some(0))],
        );
        let look = Look {
            wheels: Wheels { lift: [0.1, -0.000_000_1, 0.0, 0.25], gamma: [0.0, 0.0, 0.3, 0.0], gain: [-0.5, 0.0, 0.0, 1.0] },
            curves: [
                Curve::new(&[[0.25, 0.2], [0.75, 0.8]]).unwrap(),
                Curve::default(),
                Curve::new(&[[0.0, 0.1], [0.5, 0.5], [1.0, 0.95]]).unwrap(),
                Curve::default(),
            ],
            lut: Some(crate::cube::LutRef::open(&cube)),
            lut_mix: 0.7,
        };
        let color = vec![ColorParams { contrast: 1.1, look, ..Default::default() }, ColorParams { tint: 0.2, ..Default::default() }];
        let save = |color: &[ColorParams]| {
            save_flagged(
                &path, &src, &lanes, &[], &[], &[], &[], color, &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let text = save(&color);
        assert!(text.starts_with("edith 43\n"), "a look makes it v43: {text}");
        assert!(text.contains("\nwheels 0 0.1:-1e-7:0.0:0.25 0.0:0.0:0.3:0.0 -0.5:0.0:0.0:1.0\n"), "{text}");
        assert!(text.contains("\ncurve 0 master 0.25:0.2,0.75:0.8\n") && text.contains("\ncurve 0 green 0.0:0.1,0.5:0.5,1.0:0.95\n"), "{text}");
        assert!(text.contains("\nlut 0 0.7 luts/warm look.cube\n"), "relative inside the folder: {text}");
        assert!(!text.contains("curve 1") && !text.contains("lut 1"), "no look, no lines");
        let back = load(&path).expect("v43 loads");
        assert_eq!(back.color, color, "every number survives");
        assert_eq!(save(&back.color), text, "a re-save is the same file");
        // No look: the old header and no look line.
        let plain = save(&[color[1], color[1]]);
        assert!(plain.starts_with("edith 3") && !plain.contains("wheels") && !plain.contains("\nlut "), "{plain}");
        // A missing LUT loads, and says why it cannot be used.
        let gone = text.replace("luts/warm look.cube", "luts/gone.cube");
        std::fs::write(&path, &gone).unwrap();
        let doc = load(&path).expect("a missing LUT still loads");
        let lut = doc.color[0].look.lut.expect("named");
        assert!(lut.problem().expect("missing").contains("cannot read"));
        std::fs::remove_dir_all(&dir).ok();

        let head = "edith 43\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\ncolor 0.0:1.0:1.0:0.0\n\
            video 1 0 0 30 0 - - 0 fit 1000 0 0 0 - 0 0\n";
        let refuse = |line: &str| parse(format!("{head}{line}\n").as_bytes(), &dir).unwrap_err().to_string();
        assert!(refuse("wheels 1 0:0:0:0 0:0:0:0 0:0:0:0").contains("names color 1"));
        assert!(refuse("wheels 0 2:0:0:0 0:0:0:0 0:0:0:0").contains("outside -1..1"));
        assert!(refuse("wheels 0 0:0:0 0:0:0:0 0:0:0:0").contains("4 values"));
        assert!(refuse("curve 0 cyan 0.1:0.2").contains("not master"));
        assert!(refuse("curve 0 red 0.5:0.2,0.4:0.3").contains("strictly increase"));
        assert!(refuse("curve 0 red 0.5").contains("<x>:<y>"));
        assert!(refuse("lut 0 1.5 a.cube").contains("0..=1"));
        assert!(refuse("lut 0 1.0").contains("<mix> <path>"));
        for old in ["edith 42", "edith 41"] {
            let older = head.replacen("edith 43", old, 1);
            assert!(parse(format!("{older}lut 0 1.0 a.cube\n").as_bytes(), &dir).is_err(), "a {old} file has no lut line");
        }
    }

    #[test]
    fn voice_lines_round_trip_in_v41_and_older_files_load_with_them_off() {
        use crate::voice::{PROFILE_BANDS, Strength};
        let dir = std::env::temp_dir().join(format!("edith-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("voice.edith");
        let src = vec![Source { path: dir.join("a.mp4"), audio_stream: 0, ..Default::default() }];
        let lanes = two(vec![clip(0, 0, 30, 0, Some(0))], vec![clip(0, 0, 30, 0, Some(0))]);
        let profile: Vec<f32> = (0..PROFILE_BANDS).map(|b| -80.0 + b as f32 * 0.1).collect();
        let fx = VoiceFx::clean_voice(Strength::Strong, profile, Some(-52.0));
        let save = |voices: &[VoiceFx]| {
            save_flagged(
                &path, &src, &lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(30.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), voices, &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let text = save(&[VoiceFx::default(), fx.clone()]);
        assert!(text.starts_with("edith 41\n"), "a voice makes it v41: {text}");
        for kind in ["gate", "nr", "comp", "deess"] {
            assert!(text.contains(&format!("\nvoice audio 1 {kind} 1 ")), "{kind}: {text}");
        }
        let back = load(&path).expect("v41 loads");
        assert_eq!(back.voices, vec![VoiceFx::default(), fx.clone()], "every number and the profile survive");
        // Saved again from what was loaded: the same bytes.
        let again = save(&back.voices);
        assert_eq!(again, text);
        // Only what differs from the default is written, and a lane with nothing writes no line.
        let comp_only = VoiceFx { comp: Compressor { on: true, ..Compressor::DEFAULT }, ..Default::default() };
        let text = save(&[VoiceFx::default(), comp_only.clone()]);
        assert_eq!(text.matches("\nvoice ").count(), 1, "{text}");
        assert_eq!(load(&path).unwrap().voices[1], comp_only);
        // Nothing set: not v41 and no voice line -- the bytes a v40 save wrote.
        let plain = save(&[VoiceFx::default(), VoiceFx::default()]);
        assert!(!plain.contains("voice") && plain.starts_with("edith 3") && !plain.starts_with("edith 41"), "{plain}");
        assert_eq!(save(&[]), plain, "no list is the same as a list of defaults");
        std::fs::remove_file(&path).ok();

        let head = "edith 41\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\naudio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let refuse = |line: &str| parse(format!("{head}{line}\n").as_bytes(), &dir).unwrap_err().to_string();
        assert!(refuse("voice audio 1 gate 1 -45.0 2.0 150.0 60.0 5.0").contains("out of its range"));
        assert!(refuse("voice audio 1 comp 1 -24.0 0.5 10.0 100.0 6.0 0.0 1").contains("out of its range"));
        assert!(refuse("voice audio 1 nr 1 0.6 -18.0 -").contains("no noise profile"));
        assert!(refuse("voice audio 1 nr 0 0.6 -18.0 1.0,2.0").contains("out of its range"), "a profile is 256 bands");
        assert!(refuse("voice audio 1 gate 2 -45.0 2.0 150.0 60.0 -18.0").contains("not 0 or 1"));
        assert!(refuse("voice audio 1 gate 1 -45.0 2.0").contains("wants 6 fields"));
        assert!(refuse("voice audio 1 reverb 1").contains("not a processor"));
        assert!(refuse("voice audio 2 gate 1 -45.0 2.0 150.0 60.0 -18.0").contains("not there"));
        assert!(refuse("voice video 1 gate 1 -45.0 2.0 150.0 60.0 -18.0").contains("not an audio lane"));
        // A v40 file carries none, and the same line in one is an unknown keyword.
        let v40 = head.replacen("edith 41", "edith 40", 1);
        let err = parse(format!("{v40}voice audio 1 gate 1 -45.0 2.0 150.0 60.0 -18.0\n").as_bytes(), &dir)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown keyword"), "{err}");
        let old = parse(v40.as_bytes(), &dir).expect("v40 still loads");
        assert!(old.voices.iter().all(|v| !v.is_active()) && old.voices.len() == 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_lane_ducked_under_two_keys_round_trips_in_v30() {
        let dir = PathBuf::from("/proj");
        let head = "edith 30\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 2 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 3 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let two = format!(
            "{head}duck audio 3 1 -12.0 -40.0 80.0 400.0\nduck audio 3 2 -6.0 -30.0 50.0 300.0\n"
        );
        let ok = parse(two.as_bytes(), &dir).expect("two keys for A3 parse");
        assert_eq!(ok.ducks[3].len(), 2, "both ducks kept, in file order");
        assert_eq!(ok.ducks[3][0].under, 0);
        assert_eq!(ok.ducks[3][1].under, 1);
        let refuse = |text: String| parse(text.as_bytes(), &dir).unwrap_err().to_string();
        assert!(
            refuse(format!("{head}duck audio 3 1 -12.0 -40.0 80.0 400.0\nduck audio 3 1 -6.0 -30.0 50.0 300.0\n"))
                .contains("same key twice")
        );
        // A v29 file cannot hold two for one lane: refused by name, not dropped.
        let v29 = format!(
            "{}duck audio 3 1 -12.0 -40.0 80.0 400.0\nduck audio 3 2 -6.0 -30.0 50.0 300.0\n",
            head.replacen("edith 30", "edith 29", 1)
        );
        assert!(refuse(v29).contains("names a lane twice"));
    }

    #[test]
    fn a_wrong_first_line_is_refused_by_name() {
        let dir = PathBuf::from("/proj");
        let err = parse(b"edith 56\nsource 0 a.mp4\nvideo 0 0 5 0 -\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "line 1: unsupported version 56");
        for junk in [&b""[..], b"{}\n", b"source a.mp4\n"] {
            assert_eq!(
                parse(junk, &dir).unwrap_err().to_string(),
                "line 1: not an edith file"
            );
        }
    }

    /// The v34 `chapter` line: a project's chapters are saved in frame order and
    /// come back as they were, and a project with none is still a v33-or-older
    /// file. A v33 file loads with no chapters, and a chapter line in one is refused.
    #[test]
    fn chapters_round_trip_and_a_v33_file_loads_without_them() {
        use crate::project::Chapter;
        let dir = std::env::temp_dir().join(format!("edith-chapters-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chapters.edith");
        let chapters = vec![
            Chapter { frame: 0, title: "Intro".into() },
            Chapter { frame: 1500, title: "Setup: 100% \\ done".into() },
        ];
        save_flagged(
            &path, &[], &[], &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
            crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
            Limiter::default(), None, 0, &[], &[], &[], &[], &[], &chapters, &[], &CaptionStyle::default(), &[], &[],
        )
        .expect("a project with chapters saves");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("edith 34\n"), "chapters make it v34: {text}");
        assert!(text.contains("\nchapter 0 Intro\nchapter 1500 "), "frame order, one line each: {text}");
        let back = load(&path).expect("v34 loads");
        assert_eq!(back.chapters, chapters);
        std::fs::remove_file(&path).ok();

        // A project with no chapters keeps the header it always had.
        let bare = b"edith 33\nplayhead 0\nsource 0 a.mp4\n";
        let old = parse(bare, &dir).expect("a v33 file loads");
        assert!(old.chapters.is_empty());
        let mixed = parse(b"edith 33\nsource 0 a.mp4\nchapter 0 Intro\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"chapter\"");
        // Out of order, a name with none, and a frame that is not a frame are refused.
        for line in ["chapter 10 B\nchapter 5 A\n", "chapter 5 \n", "chapter x Intro\n"] {
            let file = format!("edith 34\nsource 0 a.mp4\n{line}");
            assert!(parse(file.as_bytes(), &dir).is_err(), "{line:?} is refused");
        }
        std::fs::remove_dir(&dir).ok();
    }

    /// The v36 `marker` and `marknote` lines: markers (point and range, with and without a
    /// note, two on one frame) round-trip and make the file v36; a v35 file loads with none
    /// and refuses the lines; a project with no marker keeps its older header.
    #[test]
    fn markers_round_trip_and_a_v35_file_loads_without_them() {
        use crate::project::{Marker, MarkerColor};
        let dir = std::env::temp_dir().join(format!("edith-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("markers.edith");
        let mk = |frame, length, name: &str, note: &str, color| Marker { frame, length, name: name.into(), note: note.into(), color };
        let markers = vec![
            mk(0, 0, "Intro", "", MarkerColor::Blue),
            mk(100, 50, "Fix: 100% \\ the cut", "line one\nline two", MarkerColor::Red),
            mk(100, 0, "Same frame", "", MarkerColor::Purple),
            mk(900, 0, "Late", "has a note", MarkerColor::Green),
        ];
        let save = |markers: &[Marker]| {
            save_flagged(
                &path, &[], &[], &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], markers, &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
        };
        save(&markers);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("edith 36\n"), "markers make it v36: {text}");
        assert!(text.contains("\nmarker 0 0 blue Intro\nmarker 100 50 red "), "{text}");
        assert!(text.contains("\nmarknote 1 line one\\nline two\nmarknote 3 has a note\n"), "{text}");
        assert_eq!(load(&path).expect("v36 loads").markers, markers);
        save(&[]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.starts_with("edith 36") && !text.contains("marker"), "no markers, an older file: {text}");
        std::fs::remove_file(&path).ok();

        let old = parse(b"edith 35\nplayhead 0\nsource 0 a.mp4\n", &dir).expect("a v35 file loads");
        assert!(old.markers.is_empty());
        let mixed = parse(b"edith 35\nsource 0 a.mp4\nmarker 0 0 red A\n", &dir).unwrap_err().to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"marker\"");
        for line in [
            "marker 10 0 red B\nmarker 5 0 red A\n",
            "marker 5 0 red \n",
            "marker 5 0 pink A\n",
            "marker x 0 red A\n",
            "marknote 0 hi\n",
        ] {
            let file = format!("edith 36\nsource 0 a.mp4\n{line}");
            assert!(parse(file.as_bytes(), &dir).is_err(), "{line:?} is refused");
        }
        std::fs::remove_dir(&dir).ok();
    }

    /// The v40 `capstyle` line: an animated caption style round-trips and makes the file v40;
    /// the default style writes no line and keeps the older header; a v36 file loads as the
    /// default and a v39 file refuses the line; a bad line is refused by name.
    /// The v40 `cuewords` lines: word times round-trip through the text, a cue without any
    /// writes no line, and a v39 file refuses the line.
    #[test]
    fn cue_word_times_round_trip_through_the_lines() {
        let dir = PathBuf::from("/proj");
        let cue = |text: &str, words: Vec<(i64, i64, String)>| crate::subtitle::Cue {
            start_us: 0,
            end_us: 3_000_000,
            text: text.into(),
            image: None,
            words,
            karaoke: Vec::new(),
        };
        let mut track = crate::subtitle::open(Path::new("/nonexistent.srt"), None);
        track.cues = vec![
            cue("plain", vec![]),
            cue("two words", vec![(0, 1_000_000, "two".into()), (1_000_000, 2_500_000, "words".into())]),
        ];
        let lines = String::from_utf8(cue_word_lines(&[track])).unwrap();
        assert_eq!(lines, "cuewords 0 1 0 1000000 two 1000000 2500000 words\n");
        let file = format!("edith 40\nsource 0 a.mp4\nsubtitle - x.srt\n{lines}");
        let doc = parse(file.as_bytes(), &dir).expect("v40 parses");
        assert_eq!(
            doc.cue_words,
            vec![(0, 1, vec![(0, 1_000_000, "two".to_string()), (1_000_000, 2_500_000, "words".to_string())])]
        );
        let old = file.replacen("edith 40", "edith 39", 1);
        assert!(parse(old.as_bytes(), &dir).unwrap_err().to_string().contains("unknown keyword"));
        for bad in ["cuewords 0 1 0 5\n", "cuewords 9 1 0 5 a\n", "cuewords 0 1 x 5 a\n"] {
            let f = format!("edith 40\nsource 0 a.mp4\nsubtitle - x.srt\n{bad}");
            assert!(parse(f.as_bytes(), &dir).is_err(), "{bad:?} is refused");
        }
    }

    /// The whole project loop for word times: a track with uneven word times is written as
    /// `cuewords` lines, read back and attached to a freshly loaded project, and the cue then
    /// answers with the same uneven times -- until its text is retyped, which drops them.
    #[test]
    fn a_projects_cue_word_times_survive_the_file_and_die_with_a_text_edit() {
        let words = vec![(0, 200_000, "so".to_string()), (200_000, 2_600_000, "extraordinary".to_string())];
        let make = |words: Vec<(i64, i64, String)>| {
            let mut track = crate::subtitle::open(Path::new("/nonexistent.srt"), None);
            track.cues = vec![crate::subtitle::Cue {
                start_us: 0,
                end_us: 3_000_000,
                text: "So extraordinary".into(),
                image: None,
                words,
                karaoke: Vec::new(),
            }];
            track
        };
        let mut project = crate::Project::single("/proj/a.mp4", 150);
        assert!(project.add_subtitles(&make(words.clone())));
        let lines = String::from_utf8(cue_word_lines(project.subtitles())).unwrap();
        let file = format!("edith 40\nsource 0 a.mp4\nsubtitle - x.srt\n{lines}");
        let doc = parse(file.as_bytes(), &PathBuf::from("/proj")).expect("the file parses");

        let mut loaded = crate::Project::single("/proj/a.mp4", 150);
        assert!(loaded.add_subtitles(&make(Vec::new())));
        assert!(loaded.subtitles()[0].cues[0].real_words().is_none(), "the SRT alone has no word times");
        let mut loaded = loaded.with_cue_words(&doc.cue_words);
        assert_eq!(loaded.subtitles()[0].cues[0].real_words(), Some(&words[..]), "the uneven times are back");
        assert!(loaded.set_cue_text(0, 0, "Something else"));
        assert!(loaded.subtitles()[0].cues[0].real_words().is_none(), "retyped words have no times");
    }

    #[test]
    fn the_caption_style_round_trips_and_a_v36_file_loads_as_static() {
        use crate::captionanim::CaptionAnim;
        let dir = std::env::temp_dir().join(format!("edith-capstyle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capstyle.edith");
        let save = |cs: &CaptionStyle| {
            save_flagged(
                &path, &[], &[], &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], cs, &[], &[],
            )
            .expect("saves");
        };
        let style = CaptionStyle {
            anim: CaptionAnim::Karaoke,
            highlight: [0x12, 0xab, 0xff],
            hl_box: true,
            uppercase: true,
            words_per_line: 3,
        };
        save(&style);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("edith 40\n"), "{text}");
        assert!(text.contains("\ncapstyle karaoke 12abff 1 1 3\n"), "{text}");
        assert_eq!(load(&path).expect("v40 loads").caption_style, style);
        // The default is no line and an older header: the bytes an older build wrote.
        save(&CaptionStyle::default());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("capstyle") && !text.starts_with("edith 40"), "{text}");
        assert_eq!(load(&path).unwrap().caption_style, CaptionStyle::default());
        let old = parse(b"edith 36\nplayhead 0\nsource 0 a.mp4\n", &dir).expect("a v36 file loads");
        assert_eq!(old.caption_style, CaptionStyle::default());
        let mixed = parse(b"edith 39\nsource 0 a.mp4\ncapstyle pop ffd600 0 0 0\n", &dir)
            .unwrap_err()
            .to_string();
        assert_eq!(mixed, "line 3: unknown keyword \"capstyle\"");
        for line in [
            "capstyle spin ffd600 0 0 0",
            "capstyle pop ffd60 0 0 0",
            "capstyle pop ffd600 2 0 0",
            "capstyle pop ffd600 0 0 99",
            "capstyle pop ffd600 0 0",
        ] {
            let file = format!("edith 40\nsource 0 a.mp4\n{line}\n");
            assert!(parse(file.as_bytes(), &dir).is_err(), "{line:?} is refused");
        }
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    /// Crafted files that declare more than any editor writes get a clear error,
    /// never an allocation or a hang.
    #[test]
    fn crafted_tables_over_the_caps_are_refused() {
        let dir = PathBuf::from("/proj");
        let head = "edith 40\nsource 0 a.mp4\n";
        let bands = vec!["1000.0:0.0:1.0:pk"; MAX_EQ_BANDS + 1].join(" ");
        let words = "0 1 w ".repeat(MAX_CUE_WORDS + 1);
        let order = (0..=MAX_SEQUENCES).map(|i| i.to_string()).collect::<Vec<_>>().join(" ");
        for (file, what) in [
            (format!("{head}eq {bands}\n"), "equalizer"),
            (format!("{head}cuewords 0 0 {}\n", words.trim_end()), "cuewords"),
            (format!("edith 45\nsource 0 a.mp4\nseqorder {order}\n"), "seqorder"),
        ] {
            let err = parse(file.as_bytes(), &dir).expect_err(what).to_string();
            assert!(err.contains("at most") || err.contains("more than"), "{what}: {err}");
        }
        // And a table of exactly the cap still opens.
        let ok = vec!["1000.0:0.0:1.0:pk"; MAX_EQ_BANDS].join(" ");
        assert!(parse(format!("{head}eq {ok}\n").as_bytes(), &dir).is_ok());
        // A project whose lists run past the cap is refused as a whole.
        let mut many = String::from("edith 40\nsource 0 a.mp4\n");
        for i in 0..=MAX_LIST_ENTRIES {
            many.push_str(&format!("chapter {i} c\n"));
        }
        let err = parse(many.as_bytes(), &dir).unwrap_err().to_string();
        assert!(err.contains("at most"), "{err}");
    }

    #[test]
    fn the_newest_version_is_the_magic_line_the_writer_emits() {
        assert_eq!(MAGIC, format!("edith {NEWEST_VERSION}").as_bytes());
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
            audio_stream: 0, ..Default::default()
        };
        let bytes = flat(
            &dir,
            std::slice::from_ref(&source),
            &two(Vec::new(), Vec::new()),
            0,
        );
        let back = parse(&bytes, &dir).expect("an emptied timeline is a project");
        assert_eq!(back.sources, vec![source]);
        assert_eq!(back.lanes, two(Vec::new(), Vec::new()));
        assert_eq!(back.playhead, 0);
        // Older dialects, where the two lanes are implied rather than declared.
        assert_eq!(
            parse(b"edith 1\nsource a.mp4\n", &dir).expect("v1").lanes,
            two(Vec::new(), Vec::new())
        );
        assert!(
            parse(b"edith 1\n", &dir)
                .expect("a project of nothing")
                .sources
                .is_empty()
        );
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

    /// A clip's visualizer field is a whole byte of flags, and a line naming one
    /// past it used to be truncated into a look nobody chose (`300` loaded as
    /// `44`, with the fast bit and the ink nibble both wrong). It is refused by
    /// name now, like the resolution's bound, the sample rate's range, the tone
    /// map and the encoder seat beside it.
    #[test]
    fn a_visualizer_byte_past_a_byte_is_refused_and_the_legal_ones_still_load() {
        // The door the parser reads that field through: the low bits are the
        // style, bit 3 the fast flag and the high nibble the ink, so every value a
        // byte holds is a look and 256 and up is not one.
        assert_eq!(flags_byte(b"0", 1).expect("zero is the plain look"), 0);
        assert_eq!(flags_byte(b"255", 1).expect("the whole byte is flags"), 255);
        let refused = flags_byte(b"300", 9).expect_err("300 is not a byte of flags");
        assert_eq!(
            refused.to_string(),
            "line 9: 300 is not a set of visualizer flags"
        );
    }

    /// ...and the same field through the parser, on a document of the newest
    /// dialect: the legal byte is what lands in the clip, the illegal one is the
    /// refusal above rather than a truncation.
    #[test]
    fn a_clip_line_whose_visualizer_is_past_a_byte_loses_the_whole_load() {
        let dir = PathBuf::from("/proj");
        let line = |flags: &str| {
            format!(
                "edith 22\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
             video 1 0 0 30 0 - - - fit 1000 0 0 0 - {flags} 0\n"
            )
        };
        let doc = parse(line("12").as_bytes(), &dir).expect("12 is a look");
        // The keyword is the saver's own for this dialect, so a mismatch here would
        // be this test's line and not the field it is about.
        assert_eq!(doc.lanes[0].1[0].visualizer, 12);
        let refused = parse(line("300").as_bytes(), &dir).expect_err("300 is not a look");
        assert!(
            refused
                .to_string()
                .contains("is not a set of visualizer flags"),
            "{refused}"
        );
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    fn sample() -> Vec<Title> {
        vec![
            Title {
                start: 0,
                frames: 90,
                text: "Episode 17\n100% \\ done".into(),
                style: TextStyle {
                    size: 80,
                    outline: Outline::Thick,
                    boxed: false,
                    place: Place::Centre,
                    ..TextStyle::default()
                },
                fade_in: 10,
                fade_out: 5,
                in_anim: TitleAnim::None,
                out_anim: TitleAnim::None,
                anim_frames: 0,
            },
            Title {
                start: 60,
                frames: 120,
                text: "Jane Doe \u{2014} Engineer".into(),
                style: TextStyle::default(),
                fade_in: 0,
                fade_out: 0,
                in_anim: Default::default(),
                out_anim: Default::default(),
                anim_frames: 0,
            },
        ]
    }

    fn styled() -> Vec<Title> {
        let mut t = sample();
        t[1].style = TextStyle {
            hplace: HPlace::Left,
            align: Align::Right,
            weight: Weight::Bold,
            ink: [255, 204, 0],
            box_color: [10, 20, 30, 200],
            ..TextStyle::default()
        };
        t
    }

    #[test]
    fn a_title_look_round_trips_and_only_a_styled_one_writes_a_line() {
        // Default looks write no `titlestyle` line at all.
        assert!(!String::from_utf8(title_lines(&sample())).unwrap().contains("titlestyle"));
        let titles = styled();
        let body = title_lines(&titles);
        let text = String::from_utf8(body.clone()).unwrap();
        assert_eq!(text.matches("titlestyle").count(), 1);
        assert!(text.contains("titlestyle 1 left right bold ffcc00 0a141ec8\n"), "{text}");
        let mut bytes = b"edith 49\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n".to_vec();
        bytes.extend_from_slice(&body);
        let doc = parse(&bytes, Path::new(".")).expect("a v49 file with a title look parses");
        assert_eq!(doc.titles, titles);
        let mut again = b"edith 49\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n".to_vec();
        again.extend_from_slice(&title_lines(&doc.titles));
        assert_eq!(again, bytes);
    }

    #[test]
    fn a_title_look_is_a_v49_line_and_a_bad_one_is_refused_by_name() {
        let head = |v: u32| format!("edith {v}\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\ntitle 0 10 0 0 50 none 1 bottom hi\n");
        let ok = format!("{}titlestyle 0 left left bold ffffff 000000ff\n", head(49));
        assert!(parse(ok.as_bytes(), Path::new(".")).is_ok());
        let old = format!("{}titlestyle 0 left left bold ffffff 000000ff\n", head(48));
        assert!(parse(old.as_bytes(), Path::new(".")).is_err(), "titlestyle is not a v48 line");
        // A v48 file with plain titles still loads, with the default look.
        let doc = parse(head(48).as_bytes(), Path::new(".")).unwrap();
        assert_eq!(doc.titles[0].style, TextStyle { boxed: true, ..TextStyle::default() });
        for line in [
            "titlestyle 0 middle left bold ffffff 000000ff",
            "titlestyle 0 left up bold ffffff 000000ff",
            "titlestyle 0 left left heavy ffffff 000000ff",
            "titlestyle 0 left left bold fff 000000ff",
            "titlestyle 0 left left bold ffffff 000000",
            "titlestyle 5 left left bold ffffff 000000ff",
        ] {
            let text = format!("{}{line}\n", head(49));
            assert!(parse(text.as_bytes(), Path::new(".")).is_err(), "{line}");
        }
    }

    #[test]
    fn title_lines_round_trip_through_the_loader_with_escapes() {
        let titles = sample();
        let mut bytes = b"edith 28\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n".to_vec();
        bytes.extend_from_slice(&title_lines(&titles));
        let doc = parse(&bytes, Path::new(".")).expect("a v26 file with titles parses");
        assert_eq!(doc.titles, titles);
        // Written and read again, the same bytes come back.
        let mut again = b"edith 28\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n".to_vec();
        again.extend_from_slice(&title_lines(&doc.titles));
        assert_eq!(again, bytes);
    }

    #[test]
    fn a_v25_file_has_no_titles_and_loads() {
        let v25 = b"edith 25\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n";
        let doc = parse(v25, Path::new(".")).expect("v25 still loads");
        assert!(doc.titles.is_empty());
        let as_v25 = b"edith 25\nplayhead 0\nsource 0 a.mp4\ntitle 0 10 0 0 50 none 0 bottom hi\n";
        assert!(
            parse(as_v25, Path::new(".")).is_err(),
            "title is not a v25 line"
        );
    }

    #[test]
    fn a_title_line_outside_its_range_is_refused_by_name() {
        let head = "edith 28\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n";
        for line in [
            "title 0 0 0 0 50 none 0 bottom hi",
            "title 0 10 0 0 9 none 0 bottom hi",
            "title 0 10 0 0 50 wavy 0 bottom hi",
            "title 0 10 0 0 50 none 2 bottom hi",
            "title 0 10 0 0 50 none 0 middle hi",
            "title 0 10 11 0 50 none 0 bottom hi",
            "title 0 10 0 0 50 none 0 bottom",
        ] {
            let body = format!("{head}{line}\n");
            assert!(
                parse(body.as_bytes(), Path::new(".")).is_err(),
                "{line} was accepted"
            );
        }
    }
}

#[cfg(test)]
mod clip_gain_tests {
    use super::*;

    /// A v26 file loads, every clip at unity; a `clipgain` line in one is a
    /// line the dialect does not have, so it is refused by number.
    #[test]
    fn a_v26_file_loads_at_unity_and_refuses_a_clipgain_line() {
        let dir = PathBuf::from("/proj");
        let old = b"edith 26\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";
        let doc = parse(old, &dir).expect("a v26 file still loads");
        assert_eq!(doc.lanes[0].1[0].gain_db, 0.0);
        let with = b"edith 26\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\nclipgain audio 1 0 -6\n";
        let err = parse(with, &dir).unwrap_err().to_string();
        assert!(err.contains("line 6"), "{err}");
    }

    /// The writer's own bytes: a gained clip gets a `clipgain` line after its
    /// lanes, unity clips get none, and the value comes back exactly.
    #[test]
    fn clip_gain_lines_are_written_only_off_unity_and_round_trip() {
        let dir = PathBuf::from("/proj");
        let clip = |gain_db: f32| Clip {
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            gain_keys: Vec::new(),
            pan: 0,
            transform_keys: Vec::new(),
            opacity_keys: Vec::new(),
            color_keys: Vec::new(),
            effects: Vec::new(),
            start: 0,
            in_frame: 0,
            out_frame: 30,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            fade_in: 0,
            fade_out: 0,
            transition_out: 0,
            transition_kind: Default::default(),
            visualizer: 0,
            viz_paint: 0,
            gain_db,
            pitch: 0,
        };
        let lanes = vec![
            (LaneKind::Video, vec![clip(0.0)]),
            (LaneKind::Audio, vec![clip(-6.5), clip(0.0)]),
        ];
        let bytes = clip_gain_lines(&lanes);
        assert_eq!(bytes, b"clipgain audio 1 0 -6.5\n".to_vec());
        let mut text = b"edith 27\nplayhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
            audio 1 30 0 30 0 - - - fit 1000 0 0 0 - 0 0\n"
            .to_vec();
        text.extend_from_slice(&bytes);
        let doc = parse(&text, &dir).expect("a v27 file with a clip gain parses");
        assert_eq!(doc.lanes[1].1[0].gain_db, -6.5);
        assert_eq!(doc.lanes[1].1[1].gain_db, 0.0);
    }
}

#[cfg(test)]
mod gain_key_tests {
    use super::*;

    const HEAD: &str = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
        audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";

    /// A v27 file loads with every clip's envelope empty, and a `gainkeys` line
    /// in one is a line the dialect does not have, so it is refused by number.
    #[test]
    fn a_v27_file_loads_flat_and_refuses_a_gainkeys_line() {
        let dir = PathBuf::from("/proj");
        let old = format!("edith 27\n{HEAD}");
        let doc = parse(old.as_bytes(), &dir).expect("a v27 file still loads");
        assert!(doc.lanes[0].1[0].gain_keys.is_empty());
        let with = format!("edith 27\n{HEAD}gainkeys audio 1 0 0:-6\n");
        let err = parse(with.as_bytes(), &dir).unwrap_err().to_string();
        assert!(err.contains("line 6"), "{err}");
    }

    /// The writer's own bytes: a clip with points gets one `gainkeys` line after
    /// its lanes, a clip without gets none, and the points come back exactly.
    #[test]
    fn gain_key_lines_are_written_only_with_points_and_round_trip() {
        let dir = PathBuf::from("/proj");
        let mut text = format!("edith 28\n{HEAD}").into_bytes();
        text.extend_from_slice(b"gainkeys audio 1 0 0:0 30:-20 45:-6.5\n");
        let doc = parse(&text, &dir).expect("a v28 file with an envelope parses");
        let keys = &doc.lanes[0].1[0].gain_keys;
        assert_eq!(
            keys,
            &[
                GainKey::new(0, 0.0),
                GainKey::new(30, -20.0),
                GainKey::new(45, -6.5),
            ]
        );
        let lanes: Vec<(LaneKind, Vec<Clip>)> =
            doc.lanes.iter().map(|(k, c)| (*k, c.clone())).collect();
        let bytes = gain_key_lines(&lanes);
        assert_eq!(bytes, b"gainkeys audio 1 0 0:0 30:-20 45:-6.5\n".to_vec());
        let again = parse(
            &[format!("edith 28\n{HEAD}").as_bytes(), &bytes].concat(),
            &dir,
        )
        .expect("the writer's own line parses");
        assert_eq!(again.lanes[0].1[0].gain_keys, *keys);
    }

    /// A ramp can make a clip shorter than its constant footprint, which the lines before
    /// its `speedkeys` still show: the next clip, placed against the ramp's length, loads;
    /// one that really overlaps the ramped footprint is refused.
    #[test]
    fn a_clip_placed_against_a_shorter_ramp_loads_and_a_real_overlap_does_not() {
        let dir = PathBuf::from("/proj");
        let next = |start: u32| {
            format!("audio 1 {start} 0 30 0 - - - fit 1000 0 0 0 - 0 0\n")
        };
        let ok = format!("edith 35\n{HEAD}{}speedkeys audio 1 0 0:1000 30:2000\n", next(22));
        let doc = parse(ok.as_bytes(), &dir).expect("the ramp is 22 frames, the next clip starts at 22");
        assert_eq!(doc.lanes[0].1.len(), 2);
        let bad = format!("edith 35\n{HEAD}{}speedkeys audio 1 0 0:1000 30:2000\n", next(21));
        assert!(parse(bad.as_bytes(), &dir).is_err(), "21 is inside the ramped clip");
    }

    /// A `speedkeys` line is a v35 clip's ramp: it reads back as the keys, its footprint
    /// is the ramp's, and the writer's own line reads back to the same keys. A clip with no
    /// keys writes no line.
    #[test]
    fn speedkeys_lines_round_trip_and_a_clip_without_a_ramp_writes_none() {
        let dir = PathBuf::from("/proj");
        let text = format!("edith 35\n{HEAD}speedkeys audio 1 0 0:1000 30:2000\n");
        let doc = parse(text.as_bytes(), &dir).expect("a v35 file with a ramp parses");
        let clip = &doc.lanes[0].1[0];
        assert_eq!(
            clip.speed_keys,
            vec![
                SpeedKey { at: 0, speed: Speed::NORMAL },
                SpeedKey { at: 30, speed: Speed::from_permille(2000) },
            ]
        );
        // 30 source frames at a rate that climbs from 1x to 2x: 22 timeline frames.
        assert_eq!(clip.frames(), 22);
        let lanes: Vec<(LaneKind, Vec<Clip>)> =
            doc.lanes.iter().map(|(k, c)| (*k, c.clone())).collect();
        assert_eq!(speed_key_lines(&lanes), b"speedkeys audio 1 0 0:1000 30:2000\n".to_vec());
        let mut plain = lanes.clone();
        plain[0].1[0].speed_keys.clear();
        assert!(speed_key_lines(&plain).is_empty(), "no ramp, no line");
        let again = parse(
            &[format!("edith 35\n{HEAD}").as_bytes(), &speed_key_lines(&lanes)].concat(),
            &dir,
        )
        .expect("the writer's own line parses");
        assert_eq!(again.lanes[0].1[0].speed_keys, clip.speed_keys);
    }

    /// A `transition` line (v37) names how a clip's dissolve is drawn: every kind's name
    /// reads back as the kind, the writer's own lines parse, a clip with the default writes
    /// none, and an older file (no line) means a dissolve.
    #[test]
    fn transition_lines_round_trip_every_kind_and_old_files_dissolve() {
        let dir = PathBuf::from("/proj");
        let head = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
            video 1 0 0 30 0 - - - fit 1000 0 0 9 - 0 0\n";
        let old = parse(format!("edith 35\n{head}").as_bytes(), &dir).expect("v35 parses");
        assert_eq!(old.lanes[0].1[0].transition_kind, TransitionKind::Dissolve);
        let mut kinds = Vec::new();
        for shape in TransitionKind::SHAPES {
            match shape.dir() {
                Some(_) => kinds.extend(crate::transitions::Dir::ALL.map(|d| shape.with_dir(d))),
                None => kinds.push(shape),
            }
        }
        for kind in kinds {
            let mut lanes: Vec<(LaneKind, Vec<Clip>)> = old.lanes.clone();
            lanes[0].1[0].transition_kind = kind;
            let lines = transition_lines(&lanes);
            if kind == TransitionKind::Dissolve {
                assert!(lines.is_empty(), "the default writes no line");
            }
            let text = [format!("edith 37\n{head}").as_bytes(), &lines].concat();
            let back = parse(&text, &dir).unwrap_or_else(|e| panic!("{}: {e}", kind.name()));
            assert_eq!(back.lanes[0].1[0].transition_kind, kind);
        }
        for bad in ["transition video 1 0 swirl\n", "transition video 1 4 zoom\n", "transition video 2 0 zoom\n"] {
            let err = parse(format!("edith 37\n{head}{bad}").as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains("line 6"), "{bad}: {err}");
        }
        for old in [35, 36] {
            let file = format!("edith {old}\n{head}transition video 1 0 zoom\n");
            assert!(parse(file.as_bytes(), &dir).is_err(), "a v{old} reader has no such line");
        }
    }

    /// The writer emits the highest version a project needs: markers alone are v36, a
    /// transition kind makes it v37 (markers and all, which round-trip together), only
    /// dissolves and no markers fall back to the older header, and a v36 file still loads.
    #[test]
    fn the_header_is_the_highest_version_needed_and_v36_and_v37_round_trip() {
        use crate::project::{Marker, MarkerColor};
        let dir = std::env::temp_dir().join(format!("edith-v37-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v37.edith");
        let head = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 1000 0 0 9 - 0 0\n";
        let doc = parse(format!("edith 35\n{head}").as_bytes(), &dir).expect("parses");
        let sources = doc.sources;
        let mut lanes = doc.lanes;
        let markers = vec![Marker { frame: 12, length: 0, name: "Beat".into(), note: "a note".into(), color: MarkerColor::Blue }];
        let save_styled = |lanes: &[(LaneKind, Vec<Clip>)], markers: &[Marker], cs: &CaptionStyle| {
            save_flagged(
                &path, &sources, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], markers, cs, &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let save = |lanes: &[(LaneKind, Vec<Clip>)], markers: &[Marker]| {
            save_styled(lanes, markers, &CaptionStyle::default())
        };
        assert!(save(&lanes, &markers).starts_with("edith 36\n"), "markers alone are v36");
        lanes[0].1[0].transition_kind = TransitionKind::Zoom;
        let text = save(&lanes, &markers);
        assert!(text.starts_with("edith 37\n"), "a transition makes it v37: {text}");
        let back = load(&path).expect("v37 loads");
        assert_eq!(back.markers, markers);
        assert_eq!(back.lanes[0].1[0].transition_kind, TransitionKind::Zoom);
        let text = save(&lanes, &[]);
        assert!(text.starts_with("edith 37\n") && text.contains("transition "), "{text}");
        lanes[0].1[0].reversed = true;
        let text = save(&lanes, &markers);
        assert!(text.starts_with("edith 38\n") && text.contains("reverse video 1 0"), "a reversed clip makes it v38: {text}");
        let back = load(&path).expect("v38 loads");
        assert!(back.lanes[0].1[0].reversed && back.markers == markers);
        assert_eq!(back.lanes[0].1[0].transition_kind, TransitionKind::Zoom);
        lanes[0].1[0].opacity_keys = vec![OpacityKey { at: 0, opacity: 0.5, ease: KeyEase::Hold }];
        let text = save(&lanes, &markers);
        assert!(text.starts_with("edith 39\n") && text.contains("okeys video 1 0"), "an animation makes it v39: {text}");
        let back = load(&path).expect("v39 loads");
        assert!(back.lanes[0].1[0].reversed && back.markers == markers);
        assert_eq!(back.lanes[0].1[0].opacity_keys.len(), 1);
        // ...and a project caption style on top of all of it makes it v40.
        let style = CaptionStyle { anim: crate::captionanim::CaptionAnim::Pop, ..CaptionStyle::default() };
        let text = save_styled(&lanes, &markers, &style);
        assert!(text.starts_with("edith 40\n") && text.contains("\ncapstyle pop "), "a caption style makes it v40: {text}");
        let back = load(&path).expect("v40 loads");
        assert!(back.lanes[0].1[0].reversed && back.markers == markers && back.caption_style == style);
        assert_eq!(back.lanes[0].1[0].opacity_keys.len(), 1);
        lanes[0].1[0].opacity_keys.clear();
        lanes[0].1[0].reversed = false;
        lanes[0].1[0].transition_kind = TransitionKind::Dissolve;
        let text = save(&lanes, &[]);
        assert!(!text.starts_with("edith 40") && !text.starts_with("edith 39") && !text.starts_with("edith 38") && !text.starts_with("edith 37") && !text.starts_with("edith 36"), "{text}");
        let v36 = parse(b"edith 36\nplayhead 0\nsource 0 a.mp4\nmarker 0 0 red A\n", &dir).expect("a v36 file loads");
        assert_eq!(v36.markers.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `lanename` and `deint` lines are v46: both round-trip, a project with neither keeps
    /// its older header and exact bytes, an older header refuses them and bad ones are
    /// refused by line.
    #[test]
    fn lane_names_and_deinterlace_round_trip_in_v46_and_without_them_the_bytes_are_unchanged() {
        let dir = std::env::temp_dir().join(format!("edith-v46-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v46.edith");
        let head = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 1000 0 0 9 - 0 0\n";
        let doc = parse(format!("edith 35\n{head}").as_bytes(), &dir).expect("parses");
        let sources = doc.sources;
        let mut lanes = doc.lanes;
        let n = lanes.len();
        let save = |lanes: &[(LaneKind, Vec<Clip>)], names: &[String]| {
            save_flagged(
                &path, &sources, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], names,
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let blank = vec![String::new(); n];
        let plain = save(&lanes, &blank);
        assert!(!plain.contains("lanename") && !plain.contains("deint") && !plain.starts_with("edith 46"), "{plain}");
        let mut names = blank.clone();
        names[0] = "Main \\ cam".into();
        let text = save(&lanes, &names);
        assert!(text.starts_with("edith 46\n") && text.contains("\nlanename video 1 Main \\\\ cam\n"), "{text}");
        assert_eq!(load(&path).expect("v46 loads").names[0], "Main \\ cam");
        assert_eq!(save(&lanes, &blank), plain, "no names is the bytes it was");
        lanes[0].1[0].deinterlace = Deinterlace::Field;
        let text = save(&lanes, &blank);
        assert!(text.starts_with("edith 46\n") && text.contains("\ndeint video 1 0 field\n"), "{text}");
        assert_eq!(load(&path).unwrap().lanes[0].1[0].deinterlace, Deinterlace::Field);
        for older in ["edith 45", "edith 44"] {
            for line in ["lanename video 1 X\n", "deint video 1 0 off\n"] {
                let old = format!("{older}\n{head}{line}");
                assert!(parse(old.as_bytes(), &dir).is_err(), "{older} refuses {line:?}");
            }
        }
        for (line, why) in [
            ("lanename video 1 \n", "1 to"),
            ("lanename video 3 X\n", "not there"),
            ("lanename tape 1 X\n", "not a lane kind"),
            ("deint video 1 0 weave\n", "frame, field or off"),
            ("deint video 1 4 off\n", "does not have"),
        ] {
            let err = parse(format!("edith 46\n{head}{line}").as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains(why) && err.contains("line"), "{line:?}: {err}");
        }
        let long = "x".repeat(crate::project::MAX_LANE_NAME_CHARS + 1);
        assert!(parse(format!("edith 46\n{head}lanename video 1 {long}\n").as_bytes(), &dir).is_err());
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    /// A `stab` line is a v44 clip's stabilisation: it round-trips through a save, a
    /// project with no stabilised clip keeps its older header and its exact bytes, an
    /// older file refuses the line and a bad one is refused by its line number.
    #[test]
    fn stab_lines_round_trip_in_v44_and_without_one_the_bytes_are_unchanged() {
        let dir = std::env::temp_dir().join(format!("edith-stab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v44.edith");
        let head = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\nvideo 1 0 0 30 0 - - - fit 1000 0 0 9 - 0 0\n";
        let doc = parse(format!("edith 35\n{head}").as_bytes(), &dir).expect("parses");
        let sources = doc.sources;
        let mut lanes = doc.lanes;
        let save = |lanes: &[(LaneKind, Vec<Clip>)]| {
            save_flagged(
                &path, &sources, lanes, &[], &[], &[], &[], &[], &[], (1280, 720), Some(25.0),
                crate::tonemap::Preset::default(), false, true, crate::export::EncoderSeat::default(),
                Limiter::default(), None, 0, &[], &[], &[], &[], &[], &[], &[], &CaptionStyle::default(), &[], &[],
            )
            .expect("saves");
            std::fs::read_to_string(&path).unwrap()
        };
        let plain = save(&lanes);
        assert!(!plain.contains("stab") && !plain.starts_with("edith 44"), "{plain}");
        let st = crate::stabilise::Stabilise::new(0.75, crate::stabilise::Crop::Full);
        lanes[0].1[0].stabilise = Some(st);
        let text = save(&lanes);
        assert!(text.starts_with("edith 44\n") && text.contains("\nstab video 1 0 0.75 full\n"), "{text}");
        assert_eq!(load(&path).expect("v44 loads").lanes[0].1[0].stabilise, Some(st));
        lanes[0].1[0].stabilise = None;
        assert_eq!(save(&lanes), plain, "off again is the bytes it was");
        for older in ["edith 43", "edith 42", "edith 41"] {
            let old = format!("{older}\n{head}stab video 1 0 0.5 zoom\n");
            assert!(parse(old.as_bytes(), &dir).unwrap_err().to_string().contains("stab"), "{older} has no stab line");
        }
        for (line, why) in [
            ("stab video 1 0 0.5\n", "wants a video clip"),
            ("stab video 1 0 1.5 zoom\n", "from 0 to 1"),
            ("stab video 1 0 0.5 tilt\n", "zoom or full"),
            ("stab video 1 4 0.5 zoom\n", "which the lane does not have"),
        ] {
            let err = parse(format!("edith 44\n{head}{line}").as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains(why) && err.starts_with("line "), "{line:?}: {err}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `reverse` line is a v38 clip's direction: it reads back as `reversed`, the
    /// writer's own line reads back the same, a project with none is written under
    /// the v35 header it always was, and an older file or a bad line is refused by
    /// its line number.
    #[test]
    fn reverse_lines_round_trip_and_older_files_and_bad_lines_are_refused() {
        let dir = PathBuf::from("/proj");
        let text = format!("edith 38\n{HEAD}reverse audio 1 0\n");
        let doc = parse(text.as_bytes(), &dir).expect("a v38 file with a reversed clip parses");
        assert!(doc.lanes[0].1[0].reversed);
        let lanes: Vec<(LaneKind, Vec<Clip>)> =
            doc.lanes.iter().map(|(k, c)| (*k, c.clone())).collect();
        assert_eq!(reverse_lines(&lanes), b"reverse audio 1 0\n".to_vec());
        let mut plain = lanes.clone();
        plain[0].1[0].reversed = false;
        assert!(reverse_lines(&plain).is_empty(), "forward, no line");
        let again = parse(
            &[format!("edith 38\n{HEAD}").as_bytes(), &reverse_lines(&lanes)].concat(),
            &dir,
        )
        .expect("the writer's own line parses");
        assert!(again.lanes[0].1[0].reversed);
        // A v37 file (or older) has no such line.
        for old in [35, 37] {
            let file = format!("edith {old}\n{HEAD}reverse audio 1 0\n");
            let err = parse(file.as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains("reverse"), "v{old}: {err}");
        }
        let head = format!("edith 38\n{HEAD}");
        for (line, why) in [
            ("reverse audio 1\n", "wants a lane and a clip"),
            ("reverse subtitle 1 0\n", "not a lane kind"),
            ("reverse audio 9 0\n", "a lane that is not there"),
            ("reverse audio 1 5\n", "which the lane does not have"),
        ] {
            let err = parse(format!("{head}{line}").as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains(why) && err.starts_with("line "), "{line:?}: {err}");
        }
    }

    /// A v34 file has no `speedkeys` line, and a bad ramp is refused by its line: a speed
    /// out of range, keys out of frame order, a key without a speed, or more than the cap.
    #[test]
    fn a_v34_file_refuses_a_speedkeys_line_and_a_bad_ramp_is_refused() {
        let dir = PathBuf::from("/proj");
        let v34 = format!("edith 34\n{HEAD}speedkeys audio 1 0 0:1000 30:2000\n");
        let err = parse(v34.as_bytes(), &dir).unwrap_err().to_string();
        assert!(err.contains("speedkeys"), "{err}");
        let head = format!("edith 35\n{HEAD}");
        for line in [
            "speedkeys audio 1 0 0:9000\n",
            "speedkeys audio 1 0 30:1000 0:1000\n",
            "speedkeys audio 1 0 0\n",
            "speedkeys audio 1 0 0:1000 1:1000 2:1000 3:1000 4:1000 5:1000 6:1000 7:1000 8:1000\n",
        ] {
            let err = parse(format!("{head}{line}").as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains("line 6"), "{line}: {err}");
        }
    }

    /// A point out of source order, one out of range, or a clip with more than
    /// the cap are all refused by line; the dialect keeps the invariants.
    #[test]
    fn a_bad_gainkeys_line_is_refused() {
        let dir = PathBuf::from("/proj");
        let head = format!("edith 28\n{HEAD}");
        for line in [
            "gainkeys audio 1 0 30:-6 0:0\n",
            "gainkeys audio 1 0 0:-90\n",
            "gainkeys audio 1 0 0\n",
            "gainkeys audio 1 0 0 0x\n",
        ] {
            let text = format!("{head}{line}");
            let err = parse(text.as_bytes(), &dir).unwrap_err().to_string();
            assert!(err.contains("line 6"), "{line}: {err}");
        }
        let many: String = (0..=MAX_GAIN_KEYS as u32)
            .map(|i| format!(" {i}:0"))
            .collect();
        let text = format!("{head}gainkeys audio 1 0 0{many}\n");
        let err = parse(text.as_bytes(), &dir).unwrap_err().to_string();
        assert!(err.contains("more than"), "{err}");
    }
}

#[cfg(test)]
mod animation_key_tests {
    use super::*;

    const HEAD: &str = "playhead 0\nresolution 1280 720\nsource 0 a.mp4\n\
        video 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n\
        audio 1 0 0 30 0 - - - fit 1000 0 0 0 - 0 0\n";

    /// A v35 file written before the easings existed still loads, every key linear
    /// (or the `ease` it said), and resaving it does not change a key.
    #[test]
    fn a_v35_file_upgrades_with_its_keys_linear_and_resaves_as_v32_bytes() {
        let dir = PathBuf::from("/proj");
        let text = format!(
            "edith 35\n{HEAD}gainkeys audio 1 0 0:0 20:-6.5\n\
             tkeys video 1 0 0:lin:0,0,1,0,0,0,0,0,1 20:ease:0,0,2,0,0,0,0,0,0.5\n"
        );
        let doc = parse(text.as_bytes(), &dir).expect("a v35 file loads");
        let clips = &doc.lanes[1].1;
        assert_eq!(clips[0].gain_keys, [GainKey::new(0, 0.0), GainKey::new(20, -6.5)]);
        assert_eq!(doc.lanes[0].1[0].transform_keys[1].ease, KeyEase::Ease);
        assert!(doc.lanes[0].1[0].opacity_keys.is_empty() && doc.lanes[0].1[0].color_keys.is_empty());
        // The writers' lines for those clips are the bytes the file had.
        assert_eq!(gain_key_lines(&doc.lanes), b"gainkeys audio 1 0 0:0 20:-6.5\n".to_vec());
        assert_eq!(
            tkey_lines(&doc.lanes),
            b"tkeys video 1 0 0:lin:0,0,1,0,0,0,0,0,1 20:ease:0,0,2,0,0,0,0,0,0.5\n".to_vec()
        );
        assert!(doc.lanes.iter().all(|(_, clips)| !clips.iter().any(needs_v39)), "still not a v39 project");
        assert!(opacity_key_lines(&doc.lanes).is_empty() && color_key_lines(&doc.lanes).is_empty());
    }

    /// The v39 lines read back, in range and in order, and an older dialect has
    /// no such lines (nor the new easings) and says so by line number.
    #[test]
    fn v39_reads_okeys_ckeys_and_eased_points_and_older_dialects_refuse_them() {
        let dir = PathBuf::from("/proj");
        let new = format!(
            "edith 39\n{HEAD}okeys video 1 0 0:in:0 10:hold:0.5 20:lin:1\n\
             ckeys video 1 0 0:out:-0.5,1,1,0 20:ease:0.5,2,0,0.25\n\
             gainkeys audio 1 0 0:0:out 20:-6.5:hold 30:-3\n\
             tkeys video 1 0 0:in:0,0,1,0,0,0,0,0,1 20:hold:0,0,2,0,0,0,0,0,1\n"
        );
        let doc = parse(new.as_bytes(), &dir).expect("a v39 file loads");
        let v = &doc.lanes[0].1[0];
        assert_eq!(v.opacity_keys.len(), 3);
        assert_eq!(v.opacity_keys[0].ease, KeyEase::EaseIn);
        assert_eq!(v.opacity_keys[1], OpacityKey { at: 10, opacity: 0.5, ease: KeyEase::Hold });
        assert_eq!(v.color_keys[0].ease, KeyEase::EaseOut);
        assert_eq!(v.color_keys[1].params.contrast, 2.0);
        assert_eq!(v.transform_keys[1].ease, KeyEase::Hold);
        let g = &doc.lanes[1].1[0].gain_keys;
        assert_eq!((g[0].ease, g[1].ease, g[2].ease), (KeyEase::EaseOut, KeyEase::Hold, KeyEase::Linear));
        // ...and writing them gives the same lines back.
        assert_eq!(
            opacity_key_lines(&doc.lanes),
            b"okeys video 1 0 0:in:0 10:hold:0.5 20:lin:1\n".to_vec()
        );
        assert_eq!(
            color_key_lines(&doc.lanes),
            b"ckeys video 1 0 0:out:-0.5,1,1,0 20:ease:0.5,2,0,0.25\n".to_vec()
        );
        assert_eq!(gain_key_lines(&doc.lanes), b"gainkeys audio 1 0 0:0:out 20:-6.5:hold 30:-3\n".to_vec());

        // A v35 reader has none of it.
        let old = format!("edith 35\n{HEAD}okeys video 1 0 0:lin:0\n");
        assert!(parse(old.as_bytes(), &dir).unwrap_err().to_string().contains("line 7"));
        // ...and so has a v38 one.
        let old = format!("edith 38\n{HEAD}okeys video 1 0 0:lin:0\n");
        assert!(parse(old.as_bytes(), &dir).is_err(), "a v38 reader has no okeys line");
        let old = format!("edith 35\n{HEAD}tkeys video 1 0 0:hold:0,0,1,0,0,0,0,0,1\n");
        assert!(parse(old.as_bytes(), &dir).is_err(), "a hold ease is not a v35 word");
    }

    #[test]
    fn bad_okeys_and_ckeys_lines_are_refused() {
        let dir = PathBuf::from("/proj");
        for bad in [
            "okeys video 1 0 0:lin:1.5",             // opacity out of range
            "okeys video 1 0 5:lin:0 5:lin:1",       // two at one frame
            "okeys video 1 0 9:lin:0 5:lin:1",       // not in source order
            "okeys video 1 0 0:wobble:1",            // unknown ease
            "okeys audio 1 0 0:lin:1",               // not a video lane
            "okeys video 1 3 0:lin:1",               // no such clip
            "ckeys video 1 0 0:lin:2,1,1,0",         // brightness out of range
            "ckeys video 1 0 0:lin:0,1,1",           // three values
            "ckeys video 1 0 0:lin:0,3,1,0",         // contrast out of range
        ] {
            let text = format!("edith 39\n{HEAD}{bad}\n");
            assert!(parse(text.as_bytes(), &dir).is_err(), "{bad}");
        }
    }
}
