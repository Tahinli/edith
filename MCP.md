# edith_mcp — edith, driven by an agent

> Status: built, 2026-10-16. Phases 1–7 done, E7 (TTS) done, keyframed gain
> automation done. Details §8.
> Audience: the agent who edits (Claude), and the agents who maintain it.
> DESIGN.md governs the GUI. This file governs the agent door. Where they meet
> (§7, live follow), DESIGN.md wins on anything the user sees.

## 1. What it is for

The user will hand most video edits to an agent. The agent cannot see a window,
cannot hear, and pays for every byte it reads. So edith gets a second front
door: an MCP server that drives the same engine the GUI drives, writes the same
`.edith` file the GUI opens, and is shaped around what an agent is good and bad
at.

| An agent… | so the server… |
|---|---|
| reasons in text, sees pictures only when asked | answers in compact text; `frame` and `contact_sheet` return PNG images on demand |
| cannot hear | gives `levels`, `silence`, `loudness`, and `transcript` / `find_words` / `auto_captions` (segment times; word times DTW-aligned, `approx` where not; §6 E4) |
| cannot speak | renders narration with a local Piper voice (`voiceover`, §4.6; §6 E7) |
| loses track of shifting indices | addresses clips by lane and start (`V1@450`), never by a bare index (§3.2) |
| makes many small edits per idea | takes a **batch** of ops, applies it atomically, as one undo step (§4.5) |
| wants to try before committing | `dry_run` on `edit` and `cut_silence`; reports, then rolls back |
| acts on stale state if not told | every edit reply lists the new addresses it touched |
| burns context on noise | bounded replies, `range` filters, `timeline detail: brief` by default |
| must not take over the user's screen | is headless; never opens a window unless asked (§7) |
| sends bad arguments | an unknown argument is refused and the reply lists the accepted ones |

## 2. Shape

```
  Claude Code ──stdio JSON-RPC──▶ edith_mcp (crates/mcp, bin `edith_mcp`)
                                   │  one thread owns the session
                                   │  VE_NO_AO=1 at start: never touches audio
                                   │  help.rs: cheat sheet, built from the live tool table
                                   ▼
                          engine::PlaybackSession  ◀── same seam the GUI uses
                          ├─ edit wrappers (split, trim, move, delete, cut_regions,
                          │  place, keep, duck, gain, subtitles, titles, freeze, …)
                          ├─ save → edith::save_flagged → foo.edith
                          ├─ export::start → ExportHandle (threads, poll)
                          ├─ compose_at / compose_at_with → frame, contact_sheet, freeze
                          ├─ scenes (scenes.rs) → shot cuts, fades to black
                          └─ loudness (EBU R128), silence, waveform, probe, decode
                                   │
                                   ▼
                              foo.edith  ◀── the GUI opens, and follows (§7)

  listen tools ──stdin/stdout──▶ edith-stt (tools/edith-stt, own workspace)
                                 candle Whisper; models in ~/.cache/edith/models
                                 results cached in ~/.cache/edith/transcripts

  voiceover ──▶ piper (EDITH_PIPER, or ~/.local/share/piper-venv/bin/piper)
                voices: EDITH_PIPER_VOICES, or ~/.local/share/piper-voices
                renders cached in <project>/voiceover/.renders/ by (text, voice, speed, pause, model);
                the named WAV in <project>/voiceover/ is a copy of its render

  frame ──▶ PNGs in ~/.cache/edith/frames/<project>/
  freeze ──▶ PNGs in <project>/freeze/
```

Decisions:

- **Crate `crates/mcp`, depends on `engine` only.** No gpui, so it builds and
  runs anywhere the engine does.
- **Hand-rolled JSON-RPC over stdio, no `rmcp`, no tokio.** Methods served:
  `initialize`, `tools/list`, `tools/call`, `resources/list`, `resources/read`.
  Tool errors come back as `isError` text, not as JSON-RPC errors.
- **Speech is a separate process.** `edith-stt` is built from `tools/edith-stt`
  (its own workspace, so the engine's dependency tree stays small). The server
  finds it via `EDITH_STT_BIN`, else `tools/edith-stt/target/release/edith-stt`
  above the running binary. It is called with a model name, a language, and an
  offset per audio stretch. Word times are DTW-aligned; a segment that could not
  be aligned is marked `approx` (§6 E4). Synthesis (`voiceover`) is Piper, a
  separate process the same way.
- **One session per server.** One open project at a time, like the GUI. Opening
  another one saves or refuses (`dirty` guard), never discards silently.
- **The `.edith` file is the hand-off.** No IPC with the GUI. The GUI reloads
  the file when it changes on disk (§7).
- **Autosave on exit.** At stdin EOF a project is written beside its file
  as `<stem>.autosave.edith` (or to `~/.cache/edith/autosave/` when untitled),
  but only when the session differs from the file as it was opened or saved,
  compared as the whole project (settings included, not only clips). Never over
  the project's own file. `project_open` says when a newer autosave exists and
  names what differs: clips, lanes, sources, and settings such as a lane's gain
  or a clip's colour. Every dated autosave beside the file is listed on open,
  newest first, with its local time and what it changes; `project_open
  {recover:"<file name or stamp>"}` loads one of them.
  `project_save` refuses while an unrecovered autosave holds edits the project
  lacks; `discard_autosave:true` sets that autosave aside as a dated copy.
  `project_open {discard_autosave:true}` does the same and says so: its edits are
  not loaded, and the copy is kept under its dated name, not deleted.
  A session that made no edit writes no autosave at exit. A load fix or a
  recovered autosave is not an edit; an edit (a source added, a relink, an engine snapshot
  pushed, the undo depth moved) is what makes the exit write one.
  An autosave is never overwritten: an older one this session did not recover
  is moved aside to `<stem>.autosave-<local stamp>.edith` first, and the reply says so.
  Stamps and listed times are local time.
  A recovered autosave is replaced in place. A write that fails (the engine
  refuses the project, or a folder cannot be made) is logged as `autosave FAILED`
  with its reason; a discard names it in its reply.

## 3. Conventions every tool shares

### 3.1 Time

Every time argument accepts any of these:
`450f` (frames) · `15.0s` / `15` (seconds) · `00:00:15:00` (timecode at project
fps) · `02:14.300` (MM:SS.fff) · `end`.

**Distances** (`by` of `roll`, `slip` and `slide`) are signed: the same forms
with a leading `-` or `+` (`-15f`, `+0.5s`, `-00:00:01:00`). A bare number is
seconds, and `end` is not a distance. `roll` also takes `to`, an absolute
timeline time like every other `to`. `slip`'s `by` is timeline time too: at a
speed other than 1 it is worth `by x speed` source frames, and the reply says so.

Timeline times round to the nearest frame. A reply that rounded says so:
`(1.53s → 38f)`. Every time in a response is given as frames and seconds:
`450f (15.000s)`. Frames are the truth; seconds are for the reader.

**Source times** (`in`, `out`, `keep` ranges, `cut_regions` against a source)
are seconds or `450f` in the **file's own fps**, not the project's. A 60 fps
clip in a 30 fps project keeps its own frame numbers.

### 3.2 Addressing

Clips live in a lane's sorted, disjoint vector, and indices shift on every edit.
So:

- **Clip address = `<lane>@<start frame>`**, e.g. `V1@450`, `A2@0`, `S1@900`.
  Clips in a lane never overlap, so the start is unique. It survives edits
  elsewhere in the lane.
- `V1#0` (index, 0-based: `V1#0` is the first clip on V1) is accepted and resolved at once. Do not hold it across edits.
- Every edit reply lists the **new** addresses of what it touched.
- Sources are `src:<n>`, as `media_import` and `media_list` report them, e.g.
  `src:2 interview_a.mp4`. Indices are kept across save and reopen.
- Lanes are `V1..`, `A1..`, `S1..`, the names the GUI shows. `S` lanes hold captions.

### 3.3 Groups and scope

Clips that share a `link` (a take: V1 + A1) move, trim and delete together, as
in the GUI. Ops take `scope: "group" | "clip"` where it matters.

| op | default scope | notes |
|---|---|---|
| `trim` | group | `ripple:true` closes (start edge) or moves what follows (end edge) so the hole the trim leaves closes, as Premiere's ripple trim; a linked partner moves with it. Plain trim is the default. `scope:"clip"` unlinks the take first; the members that stay linked must have matching bounds on a take of three or more (a 60 s picture with a 30 s bed cannot stay linked: the trim is refused, "its partners could not stay linked") |
| `split_edit` | clip | `split_edit {clip, side: audio\|video, edge: in\|out, to}`: the split edit (J or L cut). The `edge` of the `side` half of the take moves to `to`; the other half stays put. The pair is unlinked first, so `clip` may name either half. Refused when `to` would pass the other edge of that half, or a locked lane is in the take |
| `roll` | group | `roll {clip, by\|to, edge?}`: the cut at `clip`'s out edge (`edge:"in"`: its in edge) moves; the clip before it loses what the clip after it gains. A take rolls its members' cuts together; the length of the timeline does not change. Refused with no clip abutting at the cut |
| `slip` | group | `slip {clip, by}`: same slot, same length, the clip plays source `by` timeline frames later (negative: earlier). Clamped to the file; a still refuses |
| `slide` | group | `slide {clip, by}`: the clip moves along its lane with its content; the clip before it ends later or earlier, the clip after it starts later or earlier. Needs a clip on each side (the first and last of a lane refuse); the length of the timeline does not change |
| `move` | group | `to_lane` must be the same kind (V to V). A drop on empty frames lands exactly there, jumping over the clips between; a head that would land inside a clip stops short, and the reply says what was asked |
| `delete` | group | closes the gap; a ripple (below) |
| `speed` | group | |
| `speed_ramp` | group | `keys:[{at, speed}…]` (`[]` clears it); a ramp, not a constant rate |
| `reverse` | group | `on?` (absent toggles); picture and sound play backward, length and place unchanged |
| `sub_move` | group | captions |
| `group` / `ungroup` | — | `group {clips:[…]}`, `ungroup {clip}` |
| `lift`, `fade`, `dissolve`, `transition`, `crossfade`, `gain`, `color`, `transform`, `fit`, `eq`, `sub_trim`, `sub_delete`, `cue_text` | clip | act on the one clip named |
| `split` | all lanes (or `lanes`) | a cut, not a ripple: nothing moves. `lanes` limits which lanes are cut; a linked partner follows its clip |
| `close_gaps` | one lane | closes the gaps on that lane only |
| `delete`, `ripple_delete`, `cut_regions`, `insert_time`, `remove_time`, `speed` with `ripple:true`, `find_words` with `cut:true`, `cut_silence` | the ripple scope (below) | ripple ops: later items move |
| `freeze` | one lane | `lane` defaults to V1; `insert` defaults to true |
| `title_add`, `title_set`, `title_remove` | — | titles are not clips: no link, no scope. Named `T1..` in start order |

**Ripple scope.** `delete`, `ripple_delete`, `speed` with `ripple:true`, `cut_regions`, `insert_time` and `remove_time`
move the items after the cut or length change. Titles move only when a picture lane is in scope. Each reply carries an `effects:` line for side effects (clips split, holes left, takes unlinked). The default scope is the edited lane, plus the lanes of clips linked or
grouped to it (a take moves whole), closed over what the ripple reaches. On a picture lane, captions and titles over the
moved picture move too. `lanes:[…]` replaces that default and must name the edited lane; `cut_regions`, `insert_time`
and `remove_time` default to every unlocked lane, so `lanes` narrows them. `find_words` with `cut` defaults
to the target's lane and its linked lanes (every lane for a mix or `src` target). Lanes outside the scope never move or split.
A locked caption lane in the default scope is skipped with a note; any other locked lane the ripple must move refuses
the op. Addresses in one batch refer to the timeline as the previous op left it.

A group-wide op checks every lane of the take before it moves anything.

### 3.4 Lock rule

A locked lane is never changed. An op that would change it refuses, and the refusal
names the lane and the fix: `lane A2 is locked: unlock it first with lane_flags {lane:"A2", locked:false}`.
The one exception is a ripple: a locked caption lane in the default scope is skipped
with a note (the titles and captions a ripple carries past it stay put), and any other
locked lane a ripple must move refuses the op. Inside a batch a refusal rolls the whole
batch back (§4.5). A group-wide op that reaches a locked lane is refused the same way.
`lane_flags` changes the flag.

### 3.5 Responses

- Plain, terse text first. Errors are sentences that say what to do next, in
  the voice of `oracle.rs`, not `false`.
- Replies are bounded: `timeline` is `brief` unless `detail: "full"`; `captions`
  is capped at 4 KB of text; `transcript` at ~6 KB, with a `from` footer for the next page.
- Unknown arguments are refused, not ignored. The reply reads
  `unknown argument \`x\` for <tool>; accepted: …`.

### 3.6 Safety

- Never deletes or rewrites source media. Never writes outside the paths it is given.
- `export_start` takes `format: "png_sequence"` (a folder of numbered PNGs, `out` is the folder), `"still"` (the frame at the start of `range` as one PNG, the poster frame) and `"gif"` (animated GIF, with `gif_width`, `gif_fps` and `gif_loop`; at most 30 s and 720 px wide). None of these has sound. The `media_import` of one frame of a numbered PNG run (`shot_0001.png`, two or more frames, same stem and extension) imports the whole run as one clip of its frames, reported as `image sequence, N frames`.

`export_start` refuses an existing output unless `overwrite: true`. It writes
  to a `.part` file and renames on success.
- `project_save` to a different path is "save as". It refuses to overwrite a
  file that changed since this project opened it, unless `force: true`. A file
  this session opened or saved (any of them, so a save back to the original after
  a save-as) is this project's own and needs no force.
- `checkpoint` writes a copy to `<project dir>/checkpoints/<stem>/` and leaves the open
  project file alone. Only a project with no path at all (`project_new` without one, never
  saved) gets the temp directory, which is lost on reboot. A project with a path, saved or not, does not refuse.

## 4. Tools

35 tools, plus `help` and three resources. Every editing verb goes through one `edit` tool
so the schema stays small and every change is batchable. Required arguments are
in **bold**; optional ones follow the name.

### 4.1 Project

| tool | args | does |
|---|---|---|
| `project_new` | **path**, template, fps=30, resolution=1920x1080, discard, overwrite | `template` starts from a saved or built-in template by name (`templates` lists them) and takes the canvas, rate, lanes and caption style from it, so `resolution` and `fps` are refused with it | empty project at a new path (its folder is created if missing), unsaved until `project_save` (which needs at least one source). A video import does not change the canvas or fps: the project keeps 1920x1080 @30 until `project_settings` changes them. Refused when an earlier session's autosave sits at the path, unless `overwrite` |
| `project_open` | **path**, discard, recover, discard_autosave, search | opens `.edith`, or a media file as a new project (`<stem>.edith` beside it, unsaved). Reports a newer autosave and lists dated ones; `recover` is true (newest) or a dated autosave's name or stamp, its edits going on the saved file as one undo step; `discard_autosave` sets the newer autosave aside as a dated copy (kept); `search` relinks offline sources, leaving the project unsaved |
| `project_save` | path, force, discard_autosave | save, or save-as when `path` differs; needs at least one source in the project (`media_import` first). Refuses over a file changed on disk (`force` overrides), and over an autosave this session did not load (`discard_autosave` sets that autosave aside as a dated copy) |
| `project_collect` | folder | copies every file the project plays into `folder` (default `<project stem>_media` beside the project file, made if missing) and points the project at the copies; one undo step, unsaved until `project_save`. Skips generated sources, names and skips missing files, leaves sources already in the folder, renames clashes to `name-2.ext`, reuses identical bytes already there. A failed copy changes nothing. The window's File > Collect media does the same, with progress and cancel |
| `project_export_timeline` | format, path? | writes the active sequence for another editor: `otio` (OpenTimelineIO: tracks, clips with absolute file paths, gaps, transitions, markers), `fcpxml` (Final Cut Pro XML 1.10, read by Final Cut Pro, Resolve and Premiere) or `edl` (CMX3600: V1 and audio on A1/A2, dissolves as `D`, `FROM CLIP NAME` comments). Default path: beside the project file. Subtitles are left out and the reply says so. Only the file is written; the project does not change. The window's File > Export timeline as does the same into the default export folder |
| `project_import_timeline` | path | reads an `.otio` file as a new sequence of the project and makes it the active one; each clip is relinked by its path, and a missing file becomes an offline clip. One undo step, unsaved until `project_save`. The window takes the same file from File > Import timeline (OTIO) or from the command line |
| `project_status` | — | path, saved or dirty, canvas size, fps, duration, lanes with clip counts, sources, undo and redo depth, running exports, settings (and the notes, when there are any); an `OFFLINE: src:N <path>` line per offline source, and the fixes an open made to load (dirty until saved) |
| `project_settings` | resolution, fps, tonemap, notes, still_secs, proxies, auto_proxies, encoder, limiter, captions | the `ProjectSettings` knobs. An fps change keeps clips, captions and titles at the same seconds; their frame numbers and addresses change. `notes` is free text (several lines; `""` clears it), saved with the project as format v52; `still_secs` is how long a still is placed for, 0.5 to 600 s, 5 s by default, and only stills placed after the change take it. Both are one undo step each |
| `templates` | (none) | lists the project templates: the built-ins (YouTube 1080p 30, Vertical 1080×1920 30, Square 1080 30) and the ones saved from the window with File > Save as template, each marked built-in or saved and whether it keeps clips |
| `checkpoint` | **name** | save the whole project under a name (letters, digits, `-`, `_`), as a copy in `<project dir>/checkpoints/<stem>/` (the project's path, from `project_new {path}` or `project_save`; with no path at all, the temp dir, lost on reboot) |
| `restore` | **name** | replace the open project with a checkpoint, from this run or an earlier one; first saves the open project as `before-restore-<name>` (a suffix when that name is taken), so the state before the restore stays on disk; the project is dirty afterwards and its undo and redo history is cleared |
| `checkpoints` | — | the checkpoints on disk in this project's checkpoint folder, by name, including those made by earlier server runs |

### 4.2 Media

| tool | args | does |
|---|---|---|
| `media_probe` | glob, paths | header only, no import: container, codec, size, fps, duration, bitrate, and the audio and subtitle streams (Matroska: codec, language, title; mp4 subtitles: track, language, title). A picture the decoder refuses shows its reason and its streams together (`refused: …` then `streams: video #0 V_AV1 · …`). A sound codec nothing decodes gets a `warning: audio: WavPack is not supported; this file's sound will be silent` line. Many files per call |
| `media_import` | **paths** | adds sources; returns `src:<n>` for each; does not place them. Importing a file twice registers it once. Refusals and warnings are listed in full however many paths there are; only the plain successes are folded (`…and N more imported`) past 20 |
| `media_list` | kind (`video`, `audio` or `image`; omitted: every source; offline sources are kept only with no kind) | bins (`bin:<n>`, path, source count) then sources: `src:<n>`, name, duration, video size, codec, fps, the bin each is filed in and `favourite` if starred (JSON: `bin`, `bin_path`, `favourite`, and a `bins` array) |
| `media_bin` | **action** (`create` \| `rename` \| `remove` \| `move` \| `favourite`), name, parent, bin, sources, on | the media library's bins (named folders, one level of nesting) and favourites. `create {name, parent?}`; `rename {bin, name}`; `remove {bin}` (its sources go up one level, bins inside it come out to the top, nothing is lost); `move {sources, bin?}` (no `bin` takes them out of every bin); `favourite {sources, on?=true}`. A bin is `bin:<n>` or its name (`Parent/Child` for a nested one); sources are `src:<n>`, a list, or `"all"`. Names are trimmed, 1..60 characters, unique among siblings (any case). One undo step each; unsaved until `project_save`. Format v51 (older formats still load): `bin <id> <parent> <name>`, `srcbin <source> <bin>` and `srcfav <source>` lines, written only when a project has a bin, a filed source or a starred one, so other projects keep their older version |
| `media_remove` | **source**, force | drops an unused source (`src:<n>`). Refused, naming the clips, while any clip plays from it. Removing the last source keeps the undo history; any other renumbers the sources after it, which clears undo, so that is refused unless `force`. The project is left unsaved until `project_save` |
| `media_relink` | **source**, **path** | points an offline source (`src:<n>`, its file moved or deleted) at the file that now holds it. The new file must be the same kind (picture, sound or still) and long enough for its clips; clips are unchanged. One undo step: undo puts the old file back, redo points it at the new one. Unsaved until `project_save` |

Channel layouts: a mono file joins a stereo timeline (widened); a stereo file
joins a mono timeline (narrowed). A layout wider than stereo is folded to stereo
when decoded. A wider file onto a mono timeline is refused.

### 4.3 Look (text)

| tool | args | does |
|---|---|---|
| `timeline` | detail (brief\|full), lanes, range, page | the timeline as text (brief lists at most 40 clips per lane, within about 4 KB of text, and says how many more there are; `page` pages one lane 40 clips at a time), one line per clip with its address, span, source, source span (`src 3.0s–15.4s`, the part of the file it plays) and effect flags |
| `clip_info` | **clip** | everything about one clip: lane, span, source range, speed, fades, transition (kind and length), fit, visualizer, EQ bands, colour, transform, group members, and how far each edge can trim; with `at` (a time inside the clip) also match frame: the source (src:N, file) and source time shown there |
| `captions` | lane, range, page | cues on caption lanes, in timeline time, one line per cue, 40 to a page. `range` is `{from, to}`: from inclusive, to exclusive, seconds exact (a cue starting at `to` is not listed) |

`timeline` example (illustrative; real output has the same shape):

```
foo.edith · 1920x1080 @30 · 02:14.300 (4029f) · dirty · undo 12
V1  @0      0.000–12.400   src:0 interview_a.mp4  src 3.0s–15.4s  link 1
    @372   12.400–31.000   src:0 interview_a.mp4  src 20.333s–38.933s  link 2  dissolve 15f  (a wipe shows as `wipe-left 15f`)
    @930   31.000–40.000   src:3 broll_city.mp4   src 0.0s–9.0s  speed 1.5 color
A1  @0      0.000–12.400   src:0 interview_a.mp4  src 3.0s–15.4s  link 1  eq fade_in 6f
    @372   12.400–31.000   src:0 interview_a.mp4  link 2  gain -3dB
A2  @0      0.000–40.000   src:4 music.flac  gain -14dB  duck under A1
S1  @0      0.000–40.000   track 0 interview_a.srt (38 cues)
T  T1 0.000–3.000 "Episode 17" centre
gaps: V1 none · A2 none
```

Source spans are source time, not timeline frames: `src 3.0s–15.4s` is the part of
the file the clip plays. The clip's in and out frames are stored at the timeline's
rate, so the seconds are `in / fps` and `out / fps`. `clip_info` gives the same span
with timecodes and the source's own frame count.

### 4.4 See and hear

| tool | args | does |
|---|---|---|
| `frame` | **at**, source, width=640, save, burn_captions | one frame of the composed timeline (or of a source) as an image block; `compose_at`, so the frame is exact, not "whatever arrived". The PNG is also kept in `~/.cache/edith/frames/<project>/` (200 files or 200 MB across projects, oldest removed first). A name carries a hash of the picture, so the same time in a different project state is a new file, not an overwrite; the reply prints the path. `save` writes a copy to a path. The lane list marks a hidden picture lane `(hidden: not in the picture)` and never takes it as the picture on top. The reply also lists the layers at that frame: a picture stacked over another is named (`V2@100 over V1@50`), and a dissolve in progress shows its two clips and B's share, e.g. `V1 dissolving V1@75→V1@90, 40%`, with `layers` and `dissolve` in the structured content |
| `contact_sheet` | range, count=12, cols=4, source, width=1280, burn_captions, save | one PNG grid of evenly spaced frames, each with its timecode in a band above it (not over the picture, so a lower-third title stays readable). `cuts:` lists the picture (V) lane cuts in range; audio cuts are counted (`(38 audio cuts not shown)`), not listed. A tile's timecode stamp is left off when it does not fit the tile (narrow widths); the text lists every time. The cheapest way to "watch" a minute. `save` writes the PNG to a path, as `frame` does |
| `scopes` | **at**, bands=4 (1..16) | the video scopes of one composed frame as numbers, read from the graded picture at 320 px wide (`engine::scopes`, the same counts the GUI's Scopes view draws): luma min/p5/median/p95/max and mean with the crushed (≤2 %) and clipped (≥98 %) shares; the waveform's p5/median/p95 per band left to right; the RGB parade per channel and which sits highest; the vectorscope's mean saturation and cast (how far off grey, toward which hue). Levels are display percent. A LUT a clip names that cannot be read is listed as not applied |
| `scenes` | **source**, range (`{from, to}`, or the array form `[from, to]`), threshold=0.35, min_shot=0.6, sheet | shot list in source frames as `{from, to}` pairs, ready for `keep`: hard cuts and fades to black. Each boundary is the first frame of the new shot, exact to the frame; a fade through black is placed at the first picture frame after the dark run. A range gives the same boundaries as the whole file inside it. A clip address (`V1@450`) scans that clip's source within its span and adds timeline times. A source with no picture is refused. Cached per file and settings; `sheet:true` adds one contact-sheet image, saved in `~/.cache/edith/frames/<project>/` and named in the reply |
| `levels` | **target**, range, per_sec=2 (a number: buckets per second, default 2, max 50) | RMS dBFS in 20 ms windows, grouped per second; text rows and a summary. Clip gain, its volume envelope and lane gain are included; fades and EQ are not |
| `silence` | target, threshold_db, min_silence, padding, min_keep | quiet stretches as timeline frames, the same shape `cut_regions` takes. Clip gain, its envelope and lane gain are applied before judging; fades and EQ are not |
| `cut_silence` | target, threshold_db, min_silence, padding, min_keep, keep=0.4, lanes, force, dry_run | runs `silence`, then shortens every silence to `keep` seconds of pause inside its padded edges (a natural beat survives). The padding at each end is never cut, so the pause left is max(`keep`, 2 × `padding`), and the reply measures it. A silence at or under `keep` is left alone; when none is longer the reply says nothing was cut. A listed silence is at least `min_silence` with its padding counted; `min_keep` defaults to 0. One undo step. `lanes` default: the target's lane and its linked lanes; a mix target cuts every lane. A cut that would remove more than half the target's sound is refused unless `force`. With no target and two or more audio `lanes`, it is a joint cut: the silence is where every listed lane is quiet at once, so their turns stay in step. `src:N` is refused |
| `loudness` | **target**, range | EBU R128: integrated LUFS, loudness range, true peak, max momentary and short-term. Clip and lane gain, clip speed, fades and EQ are included as played |
| `clean_voice` | lane=A1, strength=medium (light, medium, strong), noise_from | gate + spectral noise reduction + compressor + de-esser on one audio lane, as one undo step. The noise profile is learned from `noise_from` (`{from, to}`) or, by default, the quietest half second of the lane; if none is found noise reduction is left off and the reply says so. The timeline, `clip_info` and `project_status` list the processors that are on |
| `track` | clip, effect_index, from?, to?, direction?=forward\|backward, apply?=mask, cancel? | follows a rect or ellipse mask's box through a clip in the background (sub-pixel normalised cross-correlation, the template learned slowly); the mask's centre takes the feature's path, size and soft edge kept. from/to are clip-relative times, default the whole clip. Stops where the feature is lost (flat or line-only patch, or a rival match nearly as good) and says at which frame. Titles cannot be tracked (no keyable position). The path follows the clip's placement (position, scale, rotation, crop and transform keys). Not yet: a source stored turned (a phone's portrait video) or with non-square pixels is tracked as stored. `cancel:true` stops the clip's tracks |
| `track_status` | wait=false | where each running track stands (percent); a finished track's keys are written onto its mask and the reply says how far it got; `wait:true` blocks until they finish |
| `stabilize_status` | clip, wait=false | where each stabilised clip's shake measurement stands: ready (with the zoom it chose), running with its percent, failed with why, or not started; `wait:true` blocks until they finish |
| `sequence` | action=list (list, new, rename, duplicate, delete, switch), sequence (name or number), name, new_name | the project's sequences: `list` marks the active one with `*`; `new`/`duplicate` do not switch; `delete` refuses the last sequence and one a compound clip plays; `switch` is not an undo step, the others are one each. `timeline`, `clip_info`, `edit` and `export_start` take an optional `sequence` to act on another (reads go back to the active one after; `edit` leaves it active). `engine::project::sequence` |
| `multicam_create` | sources, sync=audio (audio, start), name, at=0, lane=V1 | makes a multicam clip from 2 to 9 imported cameras and places it (picture on the video lane, sound on the audio lane of the same number, linked, overwriting what is there): a compound clip of a multicam sequence with one video lane and one audio lane per camera. `sync:audio` lines each camera up with the first one that has sound by cross-correlating the loudness curves (`silence::levels`, 50 per second, one-second trend removed) of the first four minutes, within ±2 minutes, to 20 ms; a camera with no sound or a match below 0.2 starts with the others and the reply says so; `sync:start` starts all together. The reply lists each angle, its start, how it was synced and the match score. One undo step. `engine::multisync`, `engine::project::multicam` |
| `multicam_flatten` | clip | replaces a multicam clip, every other piece of the same multicam on the timeline and their linked sound with the plain camera clips they play (the render does not change); the multicam sequence stays. One undo step |
| `normalize` | **target**, lufs=-14, true_peak=-1, allow_limiter, dry_run, lanes, each | sets gain so the sound measures `lufs`; cut back if the peak would pass `true_peak`. The reply leads with whether the target was reached; it also reports `achieved_lufs`, `capped_by` (true_peak, limiter, clip_gain_limit, lane_gain_limit) and the limiter state. A boost past a lane's +12 dB lane gain goes on that lane's clips as clip gain (each up to +24 dB), in the same undo step, so a quiet speaker needs no manual `gain`. `capped_by: clip_gain_limit` means even that is not enough: raise the clip gain yourself (`gain`, `add_db`, which adds on top of the lane) and normalize again. `dry_run` only measures. With `lanes` and `each:true`, each listed lane alone is set to `lufs`, in one undo step |

`target` for the hearing tools is a clip (`A1@0`, reads its own sound over its
in..out span), a lane (`A1`, its clips in timeline time, gaps as -120 dBFS),
`src:N` (the whole source; `range` is then in source time), or `mix` (the whole
timeline as an export renders it).

`normalize` on `mix` moves every unmuted audio lane by the same dB. `allow_limiter`
(mix only) turns the master limiter on at the `true_peak` ceiling when the peak is
what holds the gain back, in the same undo step. The limiter stays on afterwards.

### 4.5 Edit

`edit {ops: [...], dry_run?: false, label?}`

- Ops apply **in order**, each against the state the previous one left.
- **Atomic:** if op k fails, ops 0..k-1 roll back and the error names op k.
- **One undo step:** the batch is one entry in the undo ledger. `undo` pops the whole batch.
  `label` (≤60 chars) names it; `undo` and `redo` report it by that label. A batch with no `label` is named after its first op (e.g. `place V1`), not "edit".
- **`dry_run: true`** applies, reports, then rolls back. Undo and redo history are left as they were.
- The reply lists each op's result, the new addresses, and a timeline digest of
  the affected range.

Ops (each `{op: "…", …}`). The `op` enum is the source of truth; `help {topic:"ops"}`
prints it live, and `help {topic:"<op>"}` gives one op's entry. The recipes and the help topics write every op as `edit {ops:[{op:"…", …}]}`; tools (`normalize`, `find_words`, `cut_silence`, …) keep their own names as calls.

| group | ops |
|---|---|
| place | `place {source, at, lane?, in?, out?, length?, stretch?, audio?, audio_lane?, audio_track?, overwrite?}` (no lane: linked V1+A1; with lane: overwrites it; `at:"end"` is the timeline end, not the lane's, so it can leave a hole on a lane that ends sooner; a `length` past the source is refused ("runs past the source end") unless `stretch:true`; `audio_track:N` picks the sound track of a multi-audio file, 0-based as `media_probe` numbers them, and a track other than the source's own is placed as a source of its own, `src:N` with the same file name) · `append {source, in?, out?}` (to the end of its lane; not the timeline end) · `keep {source, ranges:[[in,out]…], lane?, at?}` (ranges back to back, the rough-cut workhorse) · `assemble {items:[{source,in?,out?}…], lane?, at?, gap?}` (keep across sources) |
| three-point | `insert {source, in?, out?, at, to?, length?, lanes?}` · `overwrite {source, in?, out?, at, to?, length?, lanes?}` (`src` also spells `source`; the source range `in..out`, else the whole file, goes at `at`; `to` is a timeline out mark and `length` a duration, either sets the length from `in` instead of `out`; the take lands on `lanes`, at most one V and one A, else the first unlocked V and A lanes the source has, linked as one take; a still takes a V lane and `length` (default 5 s), a song an A lane. `insert` cuts the clip `at` falls inside and ripples what follows on those lanes only; it never moves or splits a lane outside the take, and refuses by name when that would pull a linked take apart. `overwrite` replaces what is there, trimming or splitting it, and nothing moves. Captions and titles keep their time; a locked lane refuses; one undo step each) |
| cut | `split {at, lanes?}` · `trim {clip, edge: in\|out, to, scope?}` [group] · `roll {clip, by\|to, edge?}` [group] (move the cut between two clips) · `slip {clip, by}` [group] (shift the source in/out, same slot) · `slide {clip, by}` [group] (move the clip between its neighbours) · `move {clip, at, to_lane?, insert?, titles?, split_captions?}` [group] (`insert:true` is a ripple move) · `delete {clip}` [group] · `lift {clip}` · `ripple_delete {clip}` · `close_gaps {lane}` · `cut_regions {regions:[[from,to]…], lanes?}` (accepts `silence` output verbatim) · `insert_time {at, duration, lanes?}` (ripples empty time in; a clip `at` falls inside is cut first) · `remove_time {at, duration, lanes?}` (the `cut_regions` cut for one span; a title wholly inside the span is removed, and one straddling an edge is trimmed to the part outside; the reply names both) · `swap {a, b}` (two clips on one lane trade places; a clip in a linked take is refused until the take is ungrouped) |
| time | `speed {clip, x}` 0.25–4 [group] (x is the absolute rate, not a multiplier: re-sending 0.5 on a clip already at 0.5 changes nothing) · `reverse {clip, on?}` [group] (plays the clip backward, picture and sound, or forward again; `on` omitted toggles; composes with speed and ramps; a still refuses; `clip_info` and `timeline` say `reversed`) · `speed_ramp {clip, keys:[{at, speed}…], ripple?, lanes?}` [group] (up to 8 keys at clip-relative times, speed 0.25–4 linear between them and held outside them; `keys:[]` clears the ramp) · `fade {clip, in?, out?}` (int = frames, `"0.5s"` = time) · `dissolve {clip, frames}` (video transition into the next clip) · `transition {clip, kind?=dissolve, length?, direction?}` (the same overlap with a look: `dissolve`, `dip-black`, `dip-white`, `wipe`, `slide`, `push` or `zoom`; `wipe`/`slide`/`push` move `left` (default), `right`, `up` or `down`, the way things move, so `wipe-left` shows the next clip from the right edge; `length` or `frames` is the overlap, kept when omitted; `dissolve` accepts `kind` too; the picture changes with the kind, the sound is always the crossfade; `timeline` flags and `clip_info.transition_kind` show it) · `crossfade {clip, frames}` (audio) · `freeze {at, duration, lane?=V1, insert?=true}` (holds the composed frame at `at` as a still; needs a project path, saved or not; see below) |
| look | `color {clip, brightness?, contrast?, saturation?, tint?, reset?}` · `transform {clip, x?, y?, corner?, margin?, scale?, rotate?, crop?:[l,r,t,b], opacity?, reset?}` (`corner` is centre, top-left, top-right, bottom-left or bottom-right: the engine sets x and y from the picture's own size (after crop and quarter turn), the scale given and a `margin` in pixels (default 32) from the two edges it touches; giving x or y with corner is refused, and so is margin without corner; picture in picture is `corner:"bottom-right", scale:0.3`) · `transform_keys {clip, keys?:[{at, x?, y?, scale?, rotate?, opacity?, ease?}…], clear?}` (animates the transform over the clip's own time) · `opacity_keys {clip, keys?:[{at, opacity, ease?}…], clear?}` (opacity 0..1 over the clip's own time, fades multiply on top) · `color_keys {clip, keys?:[{at, brightness?, contrast?, saturation?, tint?, ease?}…], clear?}` (the grade over the clip's own time, interpolated per frame) · `look {clip, name}` (the whole grade becomes a look, one undo step: a built-in `Warm`, `Cool`, `Teal & orange`, `Bleach bypass`, `Black & white`, `Faded film`, `High contrast` or `Vintage`, or a saved one from the colour card's "Save current as look" (`$XDG_DATA_HOME/edith/looks/<name>.look`); an unknown name is refused with the list; `help {topic:"looks"}` lists them) · `auto_color {clip}` (reads the clip's middle frame as shot: grey-world white balance into the wheels' channel gains, then brightness and contrast so the 0.5 % and 99.5 % luma percentiles land near 0.02 and 0.98; saturation, tint, curves and LUT kept; the reply gives the values set; one undo step) · · `wheels {clip, lift?, gamma?, gain?, reset?}` (each `[master, red, green, blue]`, -1..1, or one number for the master) · `curves {clip, master?, red?, green?, blue?, reset?}` (each up to 16 `[input, output]` points in 0..1, monotone cubic; `[]` straightens one) · `lut {clip, path?, mix?, clear?}` / `lut {relink, path}` (a `.cube` file, 1D or 3D up to 65, tetrahedral, mix 0..1; a broken file is refused; relink moves every clip naming a file, one undo step). The grade runs wheels → curves → LUT → the four sliders, in Rec. 709 R'G'B' for the first three; colour keys animate the sliders and the look holds under them; saved as v43 `wheels`/`curve`/`lut` lines. Recipe: `edit {ops:[{op:"lut", clip:"V1@0", path:"~/luts/warm.cube", mix:0.8}]}`, then `scopes {at:"2s"}` to read the shift · `copy_look {from, to | to_lane, what?}` (copies colour, transform, fit, effects, EQ, gain or speed onto other clips, one undo step) · `replace {clip, source, in?}` (the replace edit: overwrite the clip and its linked half with another source, same place and length) · `nudge {clips, by}` [group] (move clips by a signed distance, all or nothing) · `fit {clip, policy: fit\|fill\|stretch\|center}` · `deinterlace {clip, mode: frame\|field\|off\|ivtc-auto\|ivtc}` (how a video clip's interlaced source is made progressive, frame by default; only matters for an interlaced source, which `media_probe` reports; field and off are stored, the render acts on frame for now; `ivtc` removes 3:2 pulldown from telecined 29.97i H.264 and MPEG-2, giving the 23.976p film frames (best cadence found in the first 60 pictures, one cadence for the whole file), `ivtc-auto` does so only when that cadence is clear and otherwise deinterlaces as frame; put the project on 23.976 (`set_frame_rate`) for one timeline frame per film frame; soft pulldown (MPEG-2 repeat_first_field, H.264 pic_struct) in a transport stream is always presented as 23.976p; saved as v46 `deint` lines) · `visualizer {clip, on?, style?, fast?}` (the clip becomes a sound picture: style fill\|ribbon\|ring\|wave, fill by default; `fast` paints from a 0.2 s window; `on:false` turns it off, from the waveform picture or from the sound clip). On a sound clip, `on:true` places a waveform picture on the next free top picture lane (V2 or V3), grouped with the sound and covering the picture under it where they overlap; place it with `transform` (x, y move its centre, -1 to 1; scale; opacity). A picture address (`V1@…`) is mapped to its linked sound clip, which gets the picture; a picture with no linked sound is refused · `reframe {clip, aspect?=fit, smoothness?=0.5}` (auto reframe: a cheap saliency per sampled frame -- what moved since the last sample, local contrast, a pull to the middle, no models -- gives the subject's path, which is smoothed (`0.25 + 1.75 × smoothness` s), held still inside a dead zone and thinned to the keys where it turns; the keys scale the clip to cover the canvas and pan the subject to the middle, clamped so no edge ever shows; sets `fit` to fit and replaces the clip's `transform_keys` in one undo step, so no format change; it analyses before it answers; `aspect` is a guard that the project canvas has that shape; `engine::reframe`) · `stabilize {clip, on?, strength?=0.5, crop?=zoom, off?, cancel?}` (takes the camera shake out of a video clip: corners tracked frame to frame by pyramidal Lucas-Kanade, a shift-turn-zoom fitted by RANSAC, the camera path smoothed by a Gaussian of strength × 2 s (a steady pan is kept), each frame moved to the smoothed path before the clip's transform; `crop` `zoom` enlarges just enough that no frame shows an edge (≤1.5×), `full` keeps the frame and shows black edges; the analysis runs in the background, is cached per source and file-frame range under `~/.cache/edith/stabilise`, follows the footage through trims, speed, ramps and reverse; an export measures a missing analysis itself, which `export_status` shows as its `Measuring shake` stage with its own percent, while `frame` and `contact_sheet` never wait: they start it, draw the clip unstabilised and say the analysis is pending; `engine::stabilise`, format v44) |
| generated | `generator_add {kind: solid\|gradient\|countdown\|bars\|rectangle\|ellipse\|line\|arrow, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?, lane?=V1, at?, duration?=5s}` (shapes `rectangle`, `ellipse`, `line` and `arrow` draw on a transparent canvas and composite over the lanes below: `fill` and `stroke` are `"#rrggbb"` or `"none"`, `width` the stroke in pixels 0..400, `radius` rounds a rectangle 0..2000; a line and an arrow are all stroke, a line runs left to right and an arrow points right; a shape lands at 40% scale and its size and place are the clip's `transform`; a clip made from numbers instead of a file: `color` is `"#ff3366"`, `angle` 0..359 degrees with 0 left to right and 90 top to bottom, `from` the countdown's whole seconds 1..3600, which is also its default duration; `at` defaults to the end of the lane; the lane must exist; a param the kind does not take, an unknown kind or a bad colour is refused by name; it overwrites what it lands on like a still, and is an ordinary clip afterwards, so trim, move, split, `color`, `transform` and effects all work) · `generator_set {clip, color?, color2?, angle?, from?, fill?, stroke?, width?, radius?}` (params merged onto a generator clip, place and length kept; a clip that plays a file is refused) · `adjustment_add {lane?=V2, at?, duration?=5s}` (a clip with no picture whose grade and effects reach everything composed beneath it; with no lane, V2, added when there is only V1; a transform on it moves the whole composite below, so uncovered area goes black, while crop only limits where it reaches and cropped-away areas keep the untouched picture; adjacent layers hard-cut) · `label {clip \| clips, value}` (a name from the label palette in any case, 1..8, or `none`) · `enable {clip \| clips, on}` (`on:false` keeps the clip but skips its picture and sound; an off adjustment layer stops adjusting). One undo step each; `timeline` and `clip_info` show `generator solid #ff3366`, `adjustment layer`, `label Rose` and `disabled`, and `media_list` does not call a generator an offline file. A generator's source is a pseudo-path `/edith-gen/<spec>.gen` (`engine::generate`), so the library, trims and saves treat it as a still. Format v48 (older formats still load): written only when a project has a generator, a label or a switched-off clip, so other projects keep their older version |
| sequences | `nest {clips:[addr…], name?}` (folds the picked clips and their linked partners into a new sequence and leaves one compound clip, on the lowest picked video and sound lanes, linked; refuses when an unpicked clip sits between picked ones; one undo step) · `break_apart {clips}` (puts a compound clip's contents back, trimmed to the stretch it played, one undo step) · `place_sequence {sequence, at, lane?=V1}` (a sequence as a compound clip; a cycle is refused). A compound clip's source is `1<<30 + sequence id`; the render inlines it into the flat timeline, so a nested render equals the flat one; it has no speed, ramp or reverse (refused) and its own colour/effects/transform are not applied. Format v45 (`MAGIC_V44` still loads): the active sequence is the body as before, then `seqactive <id> <name>`, `seqorder <ids>` and one `seq <id> <name>` … `endseq` block per parked sequence holding a complete document; only written when there is more than one sequence or a compound clip, so other projects stay v44 or older. The frame rate is locked then |
| multicam | `angle {clip, at?, angle?, audio?}` (switches a multicam clip's angle from `at` on: the clip is cut there when `at` is inside it, and the right part shows `angle`, a number from 1 or a name; the sound follows the picture unless `audio` is `"follow"` or an angle, which keeps it there; `clip` may be the picture or its linked sound clip; one undo step; refused for a clip that is not multicam, an angle that does not exist, or no change). A multicam clip is a compound clip (`source = 1<<30 + id`) with an `Angle {video, audio?}` on the clip; the sequence it plays is flagged multicam and holds angle `n` as its `n`th video lane and `n`th audio lane, so the render inlines exactly that angle's lane (`expand`), `break_apart` and `multicam_flatten` do the same. `timeline` rows read `multicam "Name" angle 2 "cam_b"`. Format v47 (`MAGIC_V46` and older still load; the version lives in `edith.rs` `MAGIC`/`NEWEST_VERSION`): `mcam <sequence id> <angle name>` lines in angle order after `seqorder`, and `angle <video|audio> <ord> <index> <picture> <sound|->` per clip that is not on angle 1 (angles counted from 1); written only when a project has a multicam source or a switched clip, so other projects keep their older version |
| sound | `eq {clip, bands:[{f,db,q,kind}] (max 10), reset?}` (f 20–20000 Hz, db ±12, q 0.3–12 default 0.707; kind peak (default), low_shelf, high_shelf, low_cut, high_cut) · `gain {clip, db (-60..24) \| add_db}` (one audio clip's own volume, on top of its lane's; relative steps clamp to the range) · `gain_keys {clip, keys:[[time, db]…]}` (the clip's volume envelope, audio only: replaces all points, `[]` clears, max 256; times are timeline times inside the clip) · `gain_key {clip, at, db}` (one point; the same frame is replaced) · `gain_key_remove {clip, at}` (the point within a frame of `at`; a miss lists the points) · `gain_ramp {clip, from, to, to_db, from_db?}` (straight dB ramp; `from_db` defaults to the envelope's level at `from`) · `gain_dip {clip, from, to, db, ramp?="0.5s"}` (dips over `from..to` with ramps in and out, starting from the level there now and clamped to the clip). The ramp and dip ops replace the points inside their span. · `lane_gain {lane, db}` (-60..12) · `pan {clip, pan}` (-1 left .. 1 right, 0 the middle; an audio clip's balance, format v50 `clippan` line) · `pitch {clip, semitones}` (-12..12 whole semitones, 0 the original; an audio clip's pitch with its speed kept, format v52 `pitch` line) · `lane_pan {lane, pan}` (the same for an audio lane, on top of the clips' `lanepan` line) · `limiter {on?, ceiling_db?}` (ceiling -24..0 dB) · `duck {lane, under, depth_db?=-12, threshold_db?=-40, attack_ms?=80, release_ms?=400, off?}` (the lane dips while the key lane speaks) |· `dynamics {lane, gate?, nr?, compressor?, deesser?, reset?}` (an audio lane's voice processors, run gate, noise reduction, compressor, de-esser after the clips' own EQ and before the limiter; each is true, false (bypass) or an object of settings; `engine::voice`, format v41) |
| lanes | `lane_add {kind}` · `lane_remove {lane}` · `lane_move {lane, to}` · `lane_flags {lane, hidden?, muted?, locked?}` · `lane_name {lane, name}` (the lane's display label, like a track name, at most 40 characters, `""` clears it; the address V1/A2 is unchanged and still what every op takes; `timeline` shows the name on the lane header and as `name` in its structured lanes; one undo step; saved as v46 `lanename` lines) |
| group | `group {clips:[addr…]}` · `ungroup {clip}` |
| captions | edit ops, not tools: `subtitles_import {path}` (tracks, no lane yet) · `captions_place {track, lane?, at?}` · `sub_move {clip, at, to_lane?}` [group] · `sub_trim {clip, edge:in|out, to}` · `sub_delete {clip}` · `captions_remove {track, force?}` · `cue_add {track, at, to, text, overwrite?}` · `cue_text {track, index, text}` (`\n` = line break) · `cue_delete {track, index}` · `cue_time {track, index, at?, to?}` (timeline times: trim or move one cue inside its neighbours, 0.1 s minimum) · `cue_merge {track, index}` (with the next cue) · `cue_split {track, index, at}` · `cue_replace {track?, find, replace, case_sensitive?}` (every cue of one track, or all tracks; case-insensitive unless `case_sensitive`; reports occurrences and cues). Written as `edit {ops:[{op:"cue_text", …}]}`; `captions` (the tool) lists the cues as `track T cue C` |
| markers | `chapter_add {at, title}` · `chapter_set {chapter, title?, at?}` · `chapter_remove {chapter}` (a chapter is `C@<frame>` or `C#n`, its place in the list, 1 first; cuts and inserts move chapters with the picture; `timeline` prints them as a YouTube description, with warnings) |
| comment markers | `marker_add {at, name, note?, color?, length?}` · `marker_set {marker, name?, note?, color?, at?, length?}` · `marker_remove {marker}` · `beats {clip, every?=1}` (finds the tempo of a sound clip and marks each `every`th beat, 1, 2 or 4 for bars, as a yellow marker noted "Beat"; the reply gives the BPM, the count and the `M@` addresses; the clip's earlier beat markers are replaced in the same undo step; a clip with no sound, a silent one or one with no steady pulse is refused; `engine::beats`) (a marker is `M@<frame>`, the first on that frame, or `M#n`; color is red, orange, yellow, green, blue or purple, default blue; a `length` makes a range and `length: 0` turns it back into a point; several markers may share a frame; they move with the picture as chapters do, a range growing or shrinking with an insert or cut; `timeline` prints a `markers:` line and a `markers` list; `export_start chapters: "markers"` writes them as the file's chapters) |
| titles | `title_add {at, duration, text, preset?=lower_third\|centered\|caption_bar, size?=80, outline?, boxed?, place?=centre\|top\|bottom, hplace?=centre\|left\|right, color?=#ffffff, weight?=regular\|bold, align?=centre\|left\|right, box_color?, box_opacity?, fade_in?, fade_out?, in_anim?=none\|fade\|typewriter\|slide_up\|slide_left\|pop\|word_by_word, out_anim?=(same), anim_secs?=0.5}` · `title_set {title, …}` · `title_remove {title}`. The title and burned-caption text draws Latin, Greek, Cyrillic, Arabic, Hebrew, Devanagari and Thai from bundled Noto faces, shaped, right-to-left in order. Colour emoji and CJK come from the system's Noto Color Emoji and Noto Sans CJK when installed; otherwise emoji draw monochrome and CJK draws as empty boxes (soft subtitle tracks keep that text). Only a character no face has draws as a box. `title_remove` renumbers the titles after it (T2 becomes T1). `preset` is the app's one-click look (the same values as the Title menu's styles): the fields given with it win, and `title_set {preset}` applies it to an existing title keeping its words and timing. `hplace` is the side the block sits against (a lower third is `place:bottom, hplace:left`), `align` how its lines line up in it, `color` the letters (`#rrggbb`), `weight` Archivo Medium or Bold (Latin only), `box_color` `#rrggbb` (keeps the opacity) or `#rrggbbaa`, `box_opacity` 0..1. A title with any of these off the default saves as format v49 (`titlestyle` lines); older projects load with the defaults. `in_anim` and `out_anim` are the entrance and exit animations, each of the seven names above; an exit is the entrance played backwards, eased in on the way out and out on the way in. `anim_secs` is how long each takes (a bare number is seconds, unlike the frame fades), `title_set` takes all three too. Typewriter and word_by_word reveal the text over the animation. A title with an animation saves as format v54 (`titleanim` lines) |

`freeze` writes the composed frame at `at` to `<project>/freeze/<stem>-<frame>-<hash8>.png`
and places it as a still on `lane`. The hash is of the pixels, so an identical
frame reuses its file. `insert: true` ripples first, as `insert_time`, with silence
under the audio. `insert: false` overwrites `lane` for `duration`; with no `lane`
the still goes on a new top video lane over the picture.

Also: `undo {steps?=1}`, `redo {steps?=1}` (an edit clears redo history).

`effect {op, clip, ...}` is a video clip's effects stack (blur, sharpen, vignette, glow, shake, chroma_key, blend, pixelate, rect_mask, ellipse_mask), at most 16 per clip, video lanes only. Every mutating op is one undo step (an entry in the `undo` ledger) and refuses on a locked lane.

| op | args | does |
|---|---|---|
| `add` | **clip**, **kind**, at=end, params, color, mode, size | a new effect at stack position `at`, at its defaults unless `params` (`{name: number}`) say otherwise; `color:"#rrggbb"` sets a chroma_key's red/green/blue, `mode:"screen"` a blend's mode (normal, multiply, screen, overlay, add, darken, lighten), `size` a pixelate's block size in pixels (2..200 at 1080 lines; same as `params:{size}`) |
| `set` | **clip**, **index**, params, on, color, mode | changes the effect at `index`; unnamed values keep theirs; `on:false` switches it off and keeps its settings. Out-of-range numbers are clamped and the reply says so |
| `remove` | **clip**, **index** | takes the effect out |
| `move` | **clip**, **from**, **to** | moves an effect within the stack |
| `mask_keys` | **clip**, **effect**, **keys**, clear | sets the whole key list of a rect_mask or ellipse_mask at stack position `effect`. Each key is `{at, x?, y?, w?, h?, feather?, ease?}`: `at` a time, `x`/`y` the centre and `w`/`h` the size as fractions of the frame, `feather` the soft edge, `ease` the move into the next key (linear, ease_in, ease_out, ease_in_out, hold). Keys sort by time; a key that names no field holds the previous key's values, and the first key the mask's own. Between keys the mask moves smoothly; before the first and after the last it holds. Up to 256 keys. `clear:true` takes the keys off and the mask goes back to its static values. Refused on any other kind |
| `list` | clip | the stack with each effect's position, kind, on/off and named values, and `keys=N` for a keyed mask; with no `clip`, every kind with its parameters and ranges |

Effects are static, one value for the whole clip, except a mask with `mask_keys`, which moves between its keys; Ken Burns stays in `transform_keys`. A pixelate is limited by the first rect_mask or ellipse_mask after it (that mask only shapes the pixelation and does not cut the clip), so an ellipse after a pixelate censors a region. Distances (blur radius, shake amount) are pixels at 1080 lines and scale with the frame height. The stack applies in order on the clip's placed picture in preview and export; chroma_key and the masks reveal the lanes below, `blend` picks how the clip lies over them. An empty or all-off stack changes nothing. Setting what is already there is no undo step. Errors are specific: an unknown kind lists the kinds, an unknown parameter lists the parameters, a bad position gives the valid range, an audio lane is refused, a 17th effect is refused.

Recipes: *Vignette plus soft blur* -- `effect {op:"add", clip:"V1#0", kind:"vignette", params:{amount:0.6}}`, then `effect {op:"add", clip:"V1#0", kind:"blur", params:{radius:4}}`. *Green screen over a background lane* -- background on V1, the green-screen take on V2: `effect {op:"add", clip:"V2#0", kind:"chroma_key", color:"#00ff00", params:{tolerance:0.35, softness:0.1, spill:0.5}}`; the keyed-out part shows V1.

### 4.6 Listen and speak

Listening needs `edith-stt` built and the model installed. The model is downloaded
only when asked for (the first call that needs it says so and stops if it is absent).

| tool | args | does |
|---|---|---|
| `transcript` | target, from, language, model=base\|tiny\|small, range, words, hint, fillers | what is said, as timeline frames; segment lines, or word times with `words:true`. Segment times are reliable; word times are DTW-aligned, and a segment whose words were only spread evenly is `approx:true` |
| `find_words` | **query**, target, language, model, hint, cut, keep (first\|last), which, pad, dry_run, lanes | a phrase, case-insensitive, punctuation ignored; returns the matching spans to cut or caption. `keep` with `cut` keeps one word of a repeat (`"I I"`). `cut` lanes default to the target's lane and its linked lanes (every lane for a mix or src target). Filler spellings (uh, um, and their variants) and hallucinated "Thank you"-type segments are handled. `cut:"lift"` removes the matches without ripple: a hole is left where each was and no other lane moves (use it on one speaker's lane, so the other speaker stays in step). With `cut`, each edge snaps to the quietest 10 ms within ±150 ms of Whisper's time (a pause, not a word's sound), never closer than 30 ms to the kept word on its side; `snap:false` cuts at Whisper's times. A match in an `approx:true` segment is a hint: pad it or check it with `frame` |
| `auto_captions` | target (default lane A1), lane (default S1), language, model, max_chars, max_secs, path, speakers, hint, replace | transcribes, cuts cues at pauses, sentence ends and the length limits, writes `<stem>.captions.srt` (`-2`, `-3` … when that name is taken; a temp file if the project is untitled), imports it as a subtitle track and places it on the caption lane at frame 0. One undo step. A re-run writes a new file and keeps the old one, so undo can bring the old track back. A lane that already holds captions takes the new run merged in, as one track in time order; a re-run on the same lane replaces the captions that lane made before. `replace:true` also removes overlapping captions that differ from what was heard now (hand edits included); undo brings them back. `speakers` (mix target) sets which audio lanes split the cues. `hint` also corrects near-miss spellings of its words after transcription, and the reply names each correction (`hint corrected: …`). An explicit `path` that a track already holds is refused |
| `voices` | language | lists the local Piper voices (name, language, quality, sample rate, speakers). The default is marked `*`: `en_US-ryan-high` when installed, else the first `en_US`, else the first one |
| `voiceover` | **text**, voice, speed=1, pause=0, name, place | speaks `text` into `<project>/voiceover/<name or slug>-<hash8>.wav` and imports it as a source (`src:N`), reporting its duration. Needs a project path (`project_new` or `project_open`): the WAV goes beside the `.edith` file even before `project_save`, and the reply says the source is unsaved until then. Renders are cached by (text, voice, speed, pause, model) in `voiceover/.renders/`: the same text with another `name` reuses the render and only copies it. Piper's duration noise is pinned off, so one text always renders to one length (the waveform keeps piper's usual small variation). `place {lane, at}` places it on a lane as one undo step; the lane must be an existing audio lane, and both are checked before anything is rendered. Text is capped at 5000 characters; `speed` 0.5–2; `pause` is seconds of silence after each sentence (max 10) |
| `record` | **action** (start\|stop\|status), lane, at=playhead, source | records a voice-over from the audio input into `<project stem>_media/voiceover-<n>.wav` beside the project (16-bit stereo WAV, 48 kHz; the folder `project_collect` uses). `start` begins a take at `at` (default the playhead) and needs a project path; `lane` is the audio lane it lands on, or a new one. `stop` ends the take, imports it as `src:N` and places it at the start point as one undo step. `status` reports the clock and the level in dBFS while recording, and the input list (`source` names, monitors marked) when idle. This is the microphone; `voiceover` is text-to-speech (Piper) and never records. Only one take runs at a time |

`hint` names words as they should come out (`"Edith, LUFS"`). It is handed to
Whisper as its initial prompt, and steers spelling only; after transcription a
near-miss spelling of a hinted word is corrected to it. A wrong hint can mislead
the transcriber, so leave it out when unsure.

Transcripts are cached in `~/.cache/edith/transcripts`. A second call on the same
target does not run the model again. The cache key carries a version
(`CACHE_VERSION`, now 5: segment-cropped DTW, band-energy onset, tail drop, the
`hint` prompt in the key), so results from before an edith-stt change are not reused.

edith-stt drops segments that look like hallucinated silence (high `no_speech`
probability with low average logprob, or a span under -50 dBFS). Its `--keep-all`
flag turns that off; the server does not pass it.

Voice needs piper (`EDITH_PIPER`, or `~/.local/share/piper-venv/bin/piper`).
Piper's stderr is reported on failure.

### 4.7 Export

| tool | args | does |
|---|---|---|
| `export_start` | **out**, preset (an `export_presets` id; explicit args override it), loudness (-30 to -5 LUFS, the integrated target; the true peak holds at the preset's ceiling or -1 dBTP, the gain is cut back and never a limiter), format (mp4, av1, av1-mp4, hevc, hevc-mp4, webm, wav, flac, mp3, ogg, m4a, opus, prores, png_sequence, still, gif), gif_width (320/480/640/720, default 480), gif_fps (1-60, default 15), gif_loop (default true), range, size, fit, bitrate_kbps, audio_kbps (128/192/256/320), bit_depth (8\|10; 10 is AV1 and HEVC only), prores_profile (proxy\|lt\|standard\|hq\|4444\|4444xq; format `prores`, a `.mov`), hdr (off\|pq\|hlg; HEVC only, implies 10-bit), hevc_speed (default\|fast\|faster; HEVC only: fast is about 1.2-1.6x quicker for 0-3% more bits, faster about 1.5-2x quicker for up to 27% more), audio_bit_depth (16\|24; ProRes sound), chapters (default true; `"markers"` writes the comment markers as the chapters), intra_only, seat (auto\|hardware\|software), subtitles, burn_captions, overwrite | starts an export; returns `job:<n>` at once. The reply counts the chapters written (`3 chapters`) and, for an AV1 or HEVC file, its depth (`output 10-bit, from the source`) · `queue` (true: line the export up behind the others -- it keeps its own copy of the timeline as it is now, so later edits do not change it, and starts when every earlier export has finished, one encode at a time, polled or not; `export_status` shows it as `waiting`; default false starts at once beside any others) |
| `export_presets` | – | lists the export presets (id, label, format, bitrate_kbps, audio_kbps, size, bit_depth, prores_profile, hdr, loudness): youtube_1080p/1080p60/1080p_hevc/4k/4k60/4k_hevc, shorts_reels, podcast, podcast_ogg and podcast_opus (-16 LUFS, -1 dBTP), archive (10-bit AV1 mkv), archive_prores (ProRes 422 HQ .mov), hdr10_hevc (10-bit HEVC mp4 tagged PQ), archive_flac, archive_wav |
| `export_captions` | format (`srt` or `vtt`, required), path, overwrite | writes the timeline's captions (every cue on the subtitle lanes, in start order) as SRT or WebVTT; picture and empty cues are left out. Without `path`: `<first source> edit.<format>` in the Videos folder. Refuses an existing file unless `overwrite` is true, and writes nothing when there are no captions |
| `export_status` | job, out | a queued job (`export_start {queue:true}`) reads `waiting` with the number of exports ahead of it; progress, elapsed, ETA, encoder, outcome of one job; while a stabilised clip is measured before it can encode, the line and `stage`/`stage_progress` name that stage (`Measuring shake`) with its own percent; `out` reports a file this session did not start; omitted, every export so far, with its size when done (decimal: 12,139,083 bytes is 12.1 MB) and the integrated loudness (LUFS) and true peak of the written file (`integrated_lufs`, `true_peak_dbtp`, measured once from the file with the `loudness` measurement), or its error when failed. When a job finishes, a peak over the format's ceiling gets a warning line: -1 dBTP for lossless (wav, flac), -2 dBTP for lossy (the rest, since encoding can overshoot the mix) |
| `export_cancel` | **job** | cancels a running job; a waiting (queued) job is removed from the queue and never starts |
| `render_preview` | range (`"all"`, the default, or `{from, to}`), cancel | pre-renders a stretch of the timeline at preview size into the render cache, so playback over it reads finished frames instead of compositing them (heavy sections: stacked effects, titles over video, colour, speed, multicam). One render at a time, in the background; `project_status` shows the progress and the cached ranges. A render is keyed by a hash of everything that changes its pixels (the clips overlapping it and their neighbours, their sources, titles, captions, tone, rate), so an edit inside a cached range drops it and playback composes live there again. `cancel: true` stops the running render and removes its partial file. The sound is not rendered; the live mix always plays |

`format` is one of `mp4`, `av1`, `av1-mp4`, `hevc`, `hevc-mp4`, `webm`, `wav`,
`flac`, `mp3`, `ogg`, `m4a` (AAC in an mp4 box, audio only), `opus` (Opus in Ogg, audio only, 48 kHz). Without `format` the extension of `out` decides, `.m4a` included. An
unknown extension is refused, and the reply lists the ones that exist. An `.mp4` of a timeline with no picture is the sound alone (AAC, no picture track) and carries no captions: export `.m4a` for sound, or add a still on V1 for a captioned video.

A relative `out` resolves against the project's folder, and the reply says so.
With no project open it is refused. An `out` inside a git working tree, outside
the project folder, is written with a warning that it will show in `git status`.

Two ways to carry captions. `burn_captions` draws one track's text into the
picture (`{track, size, outline, boxed, place, family, animation, highlight, box, uppercase, words_per_line}`), so every player shows it. CJK draws as empty boxes in burned captions unless the system has Noto Sans CJK, so put CJK in `subtitles`.
`subtitles` adds soft tracks a player can switch off (WebM carries none): `true`
for every subtitle track, placed or not, or an array of 0-based track indices, in the order
given. A picked track that is not placed on a caption lane is written whole (all its cues).
In mp4, overlapping cues are merged into one line each; mkv keeps them as they are. It is opt-in: omitted or `[]` adds none. With `burn_captions` and no
`subtitles`, the file carries no soft track, so the captions are not shown twice.
A burn-only export reports `subtitles: burned into the picture (no soft track)`. An export
that is no longer polled still finishes in the background; `export_cancel {job}` stops it.
Burn-in settings are per export; they are not saved in the project.

There are no progress notifications. Poll `export_status`.

### 4.8 Help and resources

- `help {topic?}`: the cheat sheet. Topics: `ops` (every edit op with its
  arguments), an edit op name (its manual entry), a tool name (its arguments),
  `times`, `recipes`, and these topics: `lanes` (lane names, lane_add, lane_flags,
  gaps), `ripple` (which lanes a ripple op moves, and `lanes:[…]`), `export`
  (export_start arguments, formats, range, seat, captions), `subtitles` (subtitle
  tracks, import, placed captions, cues), `captions` (the workflow: auto_captions,
  subtitles_import, burn_captions and export, plus the `captions` tool's
  arguments), `project` (new, open, save, autosave, undo, checkpoints),
  `burn_captions` (the picture-burn arguments), `visualizer` (the visualizer op and
  the clip flag `clip_info` shows), and the topics `first_video`, `podcast`, `overlays`, `vertical`, `stretch` and `unsupported`. An unknown topic lists the valid ones. No topic
  gives the whole sheet. Built from the live tool table, so it cannot drift.
- `edith://help`: the same sheet as a resource. Read it once.
- `edith://project`: the `project_status` text.
- `edith://timeline`: the `timeline` text.

## 5. A session, as the agent would run it

Illustrative; names and argument shapes are the real ones.

```
media_probe {glob: "~/Videos/vlog-17/*.mp4"}             → 6 files, 41 min total
project_new {path: "~/Videos/vlog-17/cut.edith"}
media_import {paths: [...]}                               → src:0..5
contact_sheet {source: "src:0", count: 16}                → what was shot
scenes {source: "src:0", sheet: true}                     → shots in source frames, sheet
silence {target: "src:0"}                                 → 212 quiet runs
cut_silence {target: "src:0", dry_run: true}              → would cut 212 runs, 6.1 min
edit {ops:[{op:"keep", source:"src:0", ranges:[…]}], dry_run:true}
edit {ops:[{op:"keep", source:"src:0", ranges:[…]}], label:"Rough cut"}  → V1/A1 rough cut, undo 1
transcript {target: "A1", hint: "Edith"}                  → segments, in frames
find_words {query: "um", target: "A1"}                    → 9 spans
edit {ops:[{op:"cut_regions", regions:[…the 9 spans…]}]}  → one undo step
frame {at: "01:12.0"}                                     → check a cut point
loudness {target: "mix"}                                  → -17.9 LUFS, true peak -2.1
normalize {target: "mix", lufs: -14}
auto_captions {target: "A1"}                              → 38 cues on S1
edit {ops:[{op:"duck", lane:"A2", under:"A1"}]}           → music dips under speech
voiceover {text: "Welcome back.", place: {lane:"A2", at:"0"}} → src:6, placed on A2
edit {ops:[{op:"title_add", at:"0", duration:3, text:"Episode 17", place:"centre"}]}  → T1
project_save {}
export_start {out: "vlog-17.mp4", burn_captions: {track: 0}}  → relative: the project folder
export_status {job: 1}                                    → done
```

## 6. Engine gaps (status)

| id | gap | status | notes |
|---|---|---|---|
| E1 | No deterministic "render timeline frame N" | **done** | `compose_at` / `compose_at_with` (`export.rs`), shared with export's compositor; `frame`, `contact_sheet`, `freeze` use it |
| E2 | `MediaFacts` carried no duration | **done** | `duration_secs` on the probe facts; `media_probe` reports length |
| E3 | Loudness was RMS only | **done** | `loudness.rs`: EBU R128 integrated, LRA, true peak, momentary, short-term; `normalize` uses it |
| E4 | No speech-to-text | **done** | `edith-stt` (candle Whisper). DTW word alignment over cross-attention, segment-cropped, energy onset, tail drop, `approx:true` where alignment failed, hallucinated-silence filter, `hint` as initial prompt. Vendored `zip` (`tools/edith-stt/vendor`) |
| E5 | No SRT/VTT writer, no burn-in | **done** | `subtitle.rs`: `write_srt`, `write_vtt`, `save_cues`, `cues_from_words`; VTT entity decoding; burn-in via `ExportSettings::burn_subtitles`, drawn by `text.rs` (ab_glyph rasterizer) |
| E6 | No title cards or text overlays | **done** | `title_add` / `title_set` / `title_remove` (`edit_fx.rs`); format v26 `title` line; drawn in export, `compose_at` and the GUI preview. Past the last clip the timeline extends over black |
| E7 | No text-to-speech | **done** | `voices`, `voiceover` (`tools/speak.rs`), Piper as a separate process like E4. Renders are cached by hash; `place` lands the clip as one undo step |
| G1 | GUI never reloads a project changed on disk | **done** | §7 |

Format is v27. `duck` is a line of its own (v25, `duck.rs`); titles are v26
`title` lines; per-clip gain is a v27 `clipgain <kind> <ord> <index> <db>` line,
written for each clip whose gain is not unity.

### Known limits

- **Envelopes are per clip, not per lane.** `gain_keys` and its ops shape one
  audio clip's volume over its span; `lane_gain` stays one static value per lane.
  Points are stored on source frames, so a move, trim or split keeps each point on
  the same audio. Points add to the clip's `gain` and run inside its fades.
- **Clip gain is audio only.** `gain` on a video clip is refused; set it on the
  audio clip split from the same take. It applies through the clip's fade
  envelope.
- **Keys animate transform, opacity, colour and gain.** `transform_keys`,
  `opacity_keys`, `color_keys` and the gain points take `ease` per key (`linear`,
  `ease_in`, `ease_out`, `ease_in_out`, `hold`; linear by default, and a file with no
  eases saves as before). Opacity keys replace the placement's opacity while they exist
  and the fades multiply on top; colour keys replace the single grade, and `color`
  refuses while they exist. Keys sit in source frames, so they follow trims, splits and
  ripple. Speed keys have no ease yet. A source plays its first sound
  track; `place {audio_track:N}` picks another sound track (§4.5).
- **Reversed clips.** `reverse` decodes at 8 bits, so a 10-bit source loses its low 2 bits on a 10-bit export (banding is possible on HDR). Hardware decode is off for reversed clips. The B side of a dissolve plays forward, and waveforms and thumbnails draw forward.
- **Chapters and bit depth.** Chapter markers are written into mp4, av1-mp4, hevc-mp4, m4a, webm, opus (as `CHAPTERxxx` comments) and Matroska (av1, hevc) files, timed from the start of the range; `chapters: false` leaves them out, and wav, flac, mp3 and ogg say they are not written. 10-bit export is AV1 and HEVC only (`format: av1`, `av1-mp4`, `hevc` or `hevc-mp4`; HEVC is written as Main 10): a 10-bit source writes a 10-bit file on its own, and `bit_depth: 10` asks for it (`bit_depth: 10` on H.264 or any other format is refused, naming the formats that can). Edits that are composed (grades, titles, captions, transitions, layers) still run at 8-bit precision and shift up.
- **Transitions** are the seven kinds above, one per join, on one lane's abutting clips (no transition between layered pictures, no eased or keyframed progress). Preview and export draw the same picture; project files that use a kind are format 37, older files load as dissolves.
- **Not supported yet:** HEVC sources in 4:4:4, 4:0:0 or 12-bit and AV1 sources in 4:4:0 (refused by the decoder; AV1 4:2:2, 4:4:4 and 12-bit, and HEVC 4:2:2, decode). `help {topic:"unsupported"}` lists them too.
- **Caption style is per export, except the animation.** `burn_captions` settings
  (track, size, outline, boxed, place) go with the export, not the project. The
  animation keys (`animation`: none, karaoke, pop, bounce, word; `highlight`; `box`;
  `uppercase`; `words_per_line`) default to the project's caption style, which
  `project_settings {captions: {...}}` sets (saved in the project from format v40,
  one undo step) and which a `burn_captions` key overrides for that call. `frame` and
  `export_start` draw the same pixels from the same function. A cue made by
  `auto_captions` keeps the transcript's real word times (saved in the project; a
  text edit drops them, move and trim shift and clip them). A cue without them
  (SRT, VTT, typed) spreads its words over the span by letter count. Clip speed and ramps
  are already in the cue times. Re-export to change the look.
- **`transcript`/`find_words` on `mix` is costly.** It renders the whole mix and
  runs the model over it. Transcribe a lane or a clip instead, and cache hits
  only help on the same target.
- **Export speed in debug builds is poor.** Use `cargo build --release` for real
  exports and for any timing.
- **Locked lanes refuse, they do not skip.** One refused op rolls the whole batch
  back. Unlock first, or leave that lane out of the batch. The one exception: a
  locked caption lane in a ripple's default scope is skipped with a note.
- **Approximate word times, only where marked.** Words in a segment with
  `approx:true` were spread evenly, not aligned. Check them with `frame` or
  `contact_sheet` before a cut.
- **Scene detection is heuristic.** `threshold` and `min_shot` tune it. A dissolve
  that never goes dark is not reported as a fade.
- **No progress notifications.** Poll `export_status`.

## 7. Live follow (the user watching)

Implemented in `crates/app/src/follow.rs`. The GUI watches the `.edith` file it
has open. It stores the file's write time and length when it last read it. A
poll compares those stamps, and asks whether the window has unsaved changes.

- **Clean window, file changed on disk:** the GUI reloads the project in place.
  The playhead and view state stay (view state is per project, in
  `~/.config/edith/views/`).
- **Dirty window, file changed on disk:** one toast, "changed on disk — reload /
  keep mine". Nothing is lost silently.
- **Manual reload:** the `reload-project` action does the same
  reload on demand.

The server never opens a window on its own. `edith_mcp` writes the file; the
window, if one is open, follows it. The decision is a pure function of the two
stamps and the dirty flag, so it is unit-tested without a window.

## 8. Building it

Phases, in agent time, run back to back:

1. **Skeleton, look, edit core. Done.** JSON-RPC loop, §3 parsing, `project_*`,
   `media_*`, `timeline`, `clip_info`, `edit` with the cut and place ops, undo and
   redo, save, export. E2. Tests are JSON-RPC transcripts against
   `assets/test_av.mp4`.
2. **See and hear. Done.** E1, `frame`, `contact_sheet`, `levels`, `silence`,
   `cut_silence`, `loudness`, `normalize`, checkpoints and `restore`, `dry_run`,
   resources, `help`. Progress notifications were dropped; `export_status` polls.
3. **Register and dogfood. Done.** `claude mcp add edith <path>/target/release/edith_mcp`.
4. **Live follow (G1). Done.** §7.
5. **Hearing and delivery. Done.** E3 LUFS, E5 SRT, VTT and burn-in. E4:
   `transcript`, `find_words`, `auto_captions` with DTW word alignment, energy
   edge snapping and the silence filter. `scenes` (hard cuts and fades to black,
   optional contact sheet) done.
6. **Titles and voice. Done.** E6 titles (format v26), E7 `voices` and `voiceover`
   (Piper, cached, `place` as one undo step).

7. **Keyframed gain. Done.** A clip's volume envelope (format v28 `gainkeys`
   line): `gain_keys`, `gain_key`, `gain_key_remove`, `gain_ramp` and `gain_dip`,
   with times in timeline time and points stored on source frames. `clip_info`
   lists the points, the timeline marks clips that have one with `env`, and
   `levels` and `silence` read the envelope per 20 ms window. `loudness` and
   `export` already hear it through the fade envelope.

Remaining: per-project caption style is not scheduled.
