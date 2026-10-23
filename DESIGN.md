# edith — Design

This is the binding design contract for edith, a desktop video editor. It is written for the agents who implement it. Every rule is checkable. Where this document is silent, the reference artifact is the answer.

**Reference artifact** (the approved visual reference). It has three tabs: "Main screen", "Panels" and "Keys".

**Precedence:** where this document and the mock disagree, this document wins. Where this document is silent, the mock is the reference.

Contents: 1 Thesis · 2 Tokens · 3 Identity elements · 4 Comfort rules · 5 Layout · 6 Regions · 7 Timeline behaviour · 8 Feedback · 9 Tool panels · 10 Keys · 11 Adding a feature · 12 Implementation plan

---

## 1. Thesis

edith is still the darkroom. Its chrome is colourless. The only colour comes from the footage, in the form of film inks. Pure white is the lamp: the playhead, its tab, and splice flashes.

**What changed.** The previous design ("Darkroom", 2026-08, deleted 2026-10-15) was optimised for an expert who already knew every key. Round after round it removed visible controls. The result:

- 48 of its 77 actions had no visible control.
- Text was dim 13px (3.4:1 and 2.0:1 contrast).
- Hit targets were 6–13px.
- The timeline was a cramped 200px.
- Feedback was invisible.

The user found it "hard to use… not comfortable".

**New thesis: keep the room, light the tools.** Every command is visible somewhere, readable, and big enough to hit. Keys are accelerators. They are never the only door.

**Three laws**

1. **Chrome is achromatic.** Film inks mark identity only: spines, waveforms, the overview strip, dots, meters and curves.
2. **Comfort rules (§4) are hard floors, not taste.**
3. **Identity is kept, not calmed.** The user explicitly chose the first draft over a "calmer" one ("first draft feels more original"). Do not tone down the identity elements listed in §3.

---

## 2. Tokens

### 2.1 Surfaces and inks (default palette)

These are the mock's values.

| Token | Hex | Use |
|---|---|---|
| canvas | `#0B0C0E` | Window background, viewer surround |
| panel | `#131518` | Dock, lane heads, status bar |
| raised | `#1B1E22` | Buttons, fields, the Play button |
| hover | `#24282D` | Hover fill |
| hairline | `#2A2E34` | Dividers only (decorative; need not reach 3:1) |
| edge | `#6E757D` | Boundary of a control that must read as a control (Export chip, fields, tracks). ≥3:1 on canvas, panel, raised and hover (WCAG 1.4.11) |
| seam | `#000000` | Gaps between regions |
| ink1 | `#E8EAED` | Primary text |
| ink2 | `#B4BAC1` | Secondary text |
| ink3 | `#8D959E` | Labels, keys, metadata. ≥4.5:1 on canvas and panel |
| ink4 | `#777F88` | Disabled controls **only**. Never for information. Spec: ≥3.6:1 on canvas, panel, raised and hover (3.7 on hover, 5.0 on canvas). Disabled is exempt from WCAG 1.4.3, and staying below ink3 (≥4.9) keeps disabled distinguishable from enabled |
| lamp | `#FFFFFF` | Playhead only |
| warn | `#D1B564` | Warnings |
| err | `#E07A7A` | Errors. ≥4.5:1 on every surface including hover (5.1) |

### 2.2 Film inks

- Extracted per source at import. The hue is kept. Saturation and lightness are clamped so each ink passes 3:1 on canvas.
- Reference inks: azure `#64B5D1`, magenta `#D164B5`, green `#64D19A`.
- The colour wheel caps at 12 hues.

### 2.3 Palettes

- Other palettes in Settings stay selectable.
- Every palette must pass the contrast numbers in §4. A palette that does not pass is a bug.
- The guard test `no_colour_is_written_outside_the_theme` stays. No colour literal may appear at a call site.

### 2.4 Type

- **Archivo** is the UI voice (weights 400, 500, 700).
- **Spline Sans Mono** is for data: timecode, keys, values and metadata.

| Role | Size | Face |
|---|---|---|
| Transport timecode | 20px | Mono 500 |
| Panel titles | 15px | Archivo 700 |
| Body, labels, buttons | 14px | Archivo 500 |
| Keys and values | 13px | Mono |
| Section heads (uppercase, +0.08em, ink3) and ruler labels | 12px | — |

- **12px is the floor.**
- Whole pixels only.
- Use tabular figures wherever digits align.

### 2.5 Radii and shadow

- 0px: clips, lanes, regions.
- 4px: buttons, chips, fields.
- 6px: menus, sheets, toasts.
- Shadow appears only on floating plates: menus, the command palette, sheets and toasts.

---

## 3. Identity elements

Keep these exactly. Do not "clean them up".

- **Overview strip.** A 14px band above the ruler. Each clip is drawn in its film ink across its span. Audio appears as thin translucent slivers. A white playhead tick sits on it. A draggable 1px ink1 window box marks the visible area.
- **Words on the toolbar.** Every edit-toolbar button shows glyph, word and key, for example `✂ Split S`. Only zoom − and + are glyph-only.
- **Typographic lane glyphs.** ◉ hides (video and subtitle), ♪ mutes (audio), ▣ locks. They are ink3 at rest. When off, the glyph is struck through in ink1. These are not SVG icons. The user chose them.
- **Clip anatomy.**
  - A 3px film-ink spine on the left edge.
  - Film-tinted thumbnails for video, or a waveform in the clip's ink for audio.
  - View > Audio waveforms (saved with the project's view) off: audio clips are a flat bar in their colour with the name only.
  - A name plate top-left: mono 12px on a dim plate.
  - 1px splice gaps between clips.
- **One box per surface.** The bordered strong button is reserved for the commit action: Export, or Apply and Remove inside a tool. Small bordered `+ Add` buttons in the Media list are the one accepted secondary box. They are per-row, small and ink2.
- Glyphs are hand-drawn or typographic. No icon asset packs.

---

## 4. Comfort rules

These are hard floors. Each one is testable.

1. **Contrast.** Any text that carries information is ≥4.5:1 against its surface. ink4 is used only for disabled controls.
   Non-text: a control's boundary is `edge` (≥3:1). The focus ring is 2px `ink1`, drawn outside the control (no layout shift) on every control that holds keyboard focus. The playhead is 2px at rest, 3px playing, with a 1px `seam` halo either side so it reads on every clip colour. The overview strip's window is a 2px `ink1` box with the film outside it dimmed by `seam`. The guard test `theme::tests::darkroom_token_pairs_meet_their_contrast_spec` fails if any pair drops below spec.
2. **Size.** Nothing is below 12px. Body text and labels are 14px.
3. **Targets.**
   - Every pointer target is ≥24×24px. The hit area may exceed the drawn shape.
   - Clip trim zones: a 24px invisible hit zone centred on every clip edge (12px inside, 12px outside), with an `ew-resize` cursor. Where two clips meet, the clip on the pointer's side wins. On a clip narrower than 48px, the inside half shrinks so the middle third of the clip stays a move target.
   - The selected clip also shows solid 12px trim handles.
   - Fade handles, grips and the playhead tab each have a ≥24px hit area.
4. **Every action has a visible door.** It appears in the menu bar (§6) with its key. It can also be run from command search (Ctrl+K). The most frequent ~12 actions also sit on the edit toolbar or the transport. Right-click menus are a shortcut to the same verbs. They are never the only home.
5. **Keys are shown.** Every visible command shows its key, in mono ink3, wherever it appears: toolbar, menus, tooltips and panels.
6. **No hover-only controls.** Hover may brighten or fill. It never reveals a control that was not drawn.
7. **Tooltips** appear after 500ms (gpui 0.2.2's fixed delay) and show the name and key. For a disabled control, the tooltip gives the reason it cannot run. Example: Split shows "Put the playhead over a clip".
8. **Refused actions grey out.** The control is drawn in ink4; its reason is written in ink3, so the reason stays readable. They never vanish. Actions a media kind can never use (EQ on a picture, for example) are hidden in that context. That is a class refusal, not a momentary one.
9. **Feedback for every action.** Each action produces a toast (§8). There are no silent no-ops.
10. **Selection and focus.**
    - A selected clip has a 2px ink1 outline and visible trim handles.
    - Linked audio selects together with its picture.
    - A focused region has a 1px ink2 top rule.
11. **Words.** Use sentence case and plain language. No ALL-CAPS messages. Short hints of ≤6 words are allowed in empty states and empty lanes, for example "Drop a video here" or "Drag media here to layer it". Paragraphs of instruction are not allowed.
12. **Occlusion.** Nothing covers the picture except menus and dropdowns, the command palette, and the centred Settings sheet. All of these are transient and close on Esc or on a click away. Nothing ever covers the timeline. Export is a dock tab, not a sheet.

---

## 5. Layout

The default reference size is 1440×860. Proportions scale with the window.

```
+----------------------------------------------------------------------------+
| menu bar 32 (full width)                                                   |
+-------------------------------------------------------+--------------------+
| viewer (flex)                                         | dock 320           |
|                                                       | Media | Clip tabs  |
+-------------------------------------------------------+                    |
| transport 44                                          |                    |
+-------------------------------------------------------+                    |
| edit toolbar 44                                       |                    |
+-------------------------------------------------------+                    |
| overview strip 14                                     |                    |
+-------------------------------------------------------+                    |
| ruler 26                                              |                    |
+-------------------------------------------------------+                    |
| lanes                                                 |                    |
+-------------------------------------------------------+--------------------+
| status bar 30 (full width)                                                 |
+----------------------------------------------------------------------------+
```

- **Timeline block.** The block from the edit toolbar down to the lanes defaults to about 42% of window height. The seam between viewer and timeline is draggable, and its position is remembered.
- **Dock.** It sits on the right. The default width is 320px. It is resizable from 280px up to ⅓ of the window width, and the width is remembered.
- **Lane heads** are 156px wide. Each shows a 16px source-patch box first (V on video lanes, A on audio lanes; shown only while the source viewer has an item loaded, the slot is reserved otherwise), then a film dot, the name (14px ink1) and the glyph toggles. A lit box sends F9/F10 source edits to that lane.
- **Lane heights.** Video 64px, audio 52px, subtitle 32px, empty layer 40px. Each is resizable from 28px to 120px by dragging its bottom edge. Double-click resets it.
- **Scrolling.** Lanes beyond the visible area scroll under a pinned ruler and pinned lane heads.
- **Clip degradation by width.**
  - Under 60px: drop the name plate.
  - Under 24px: drop thumbnails or waveform, leaving a solid ink fill and the spine.
  - Names ellipsize. They are never cut mid-glyph.
- **Continuity.** A project reopens exactly as it was left: playhead, selection, zoom, scroll, dock tab, lane heights and splits.

---

## 6. Regions

### 6.1 Menu bar

- Left: the `edith` wordmark, then File · Edit · Clip · Timeline · View · Help.
- Right: the project name, then `Saved` or `Unsaved — Save Ctrl+S`. The latter is a button.
- Menu rows are 28px tall: label on the left, key right-aligned. A disabled row shows its reason in place of the key. Toggles show checkmarks: Snap, Loop, Subtitles.

Menu contents, exactly as in the mock:

| Menu | Entries |
|---|---|
| File | New project · New from template ▸ (the built-ins and saved templates) · Open project… · Import media… Ctrl+I · Paste path · Import subtitles… Ctrl+Shift+I · Save · Save as template… · Export… E · Export captions… (a sheet: name and SRT or WebVTT, no picker) · Export timeline as ▸ (OpenTimelineIO… · Final Cut Pro XML… · EDL…; written to the export folder, the path in a toast) · Import OpenTimelineIO… (a new sequence) · Settings… Ctrl+, |
| Edit | Undo · Redo · Cut / Copy / Paste · Insert / Overwrite source F9 / F10 · Select all · Deselect |
| Clip | Split S · Delete · Lift ; · Ripple start to playhead Q · Ripple end to playhead W · Trim start / end to playhead Alt+Q / Alt+W · Speed… · Colour… · Transform… · Equalizer… · Silence… · Fit policy… (Alt+1…) · Group · Detach audio |
| Timeline | Mark in / out · Clear marks · Record voice-over… (a sheet: input, lane, level meter, Record and Stop) · Zoom in / out / Fit · Snap · Add track / Remove track |
| View | Play/Pause · J/K/L · Loop · Go to start / end · Subtitles · Audio scrubbing · Full screen |
| Help | Command search Ctrl+K · Keyboard shortcuts ? |

A template keeps the frame rate, size, lanes, lane names, caption style and export preset of a project; "Keep clips" (off by default) keeps the clips too. The welcome screen's From template opens the same list as a dropdown beside New project.

### 6.2 Viewer

- The picture is letterboxed and centred, with 12px padding.
- Compare grade (View menu, or the Compare row on the Colour card) shows the picture as shot beside the graded one. Split is a vertical wipe at the centre of the picture: as shot on the left, graded on the right, with a light divider the picture's height. Side by side fits each half in its own side of the viewer. Each view has a "Before" and an "After" caption inside the picture, at the bottom left of its half, and the picture shows no selection box or handles while it is on. Compare is for looking only and is never exported.
- Nothing is drawn over the picture and nothing appears under it: no info chip, no OUT | IN strip, so selecting a clip never resizes or crops the picture. the timecode is the transport's, and the clip's name is the status bar's.

### 6.3 Transport

- Left: the big timecode. Its colons are ink3.
- Centre: Go to start · Back one frame · Play/Pause (raised button) · Forward one frame · Go to end.
- Right: Loop, and a volume slider.
- There is no viewer "Fit" control. It duplicated the timeline Fit.

### 6.4 Edit toolbar

Groups, left to right:

- [✂ Split S · ⌫ Delete · ⇤ Lift ;]
- [↶ Undo Ctrl+Z · ↷ Redo Ctrl+Y]
- [⧉ Snap N (toggle) · [ Mark in I · ] Mark out O]
- then, pushed right: [− zoom slider + · Fit Ctrl+0]
- then [Export E], the commit box.

### 6.5 Overview strip and ruler

- The overview strip is described in §3.
- The ruler uses adaptive ticks. Labels are at least 80px apart. Steps run from 1 frame to 1 minute. Labels are mono 12px ink3.
- The playhead's lamp tab on the ruler is grabbable, with a 24px hit area.
- The playhead line is 1px at rest and 2px while playing.
- Render bar: a 3px strip along the ruler's top edge, laid over it so no height changes. Green where a render preview matches the timeline; amber where a clip with effects, colour, transform, speed, fades or transitions has none yet. Nothing is drawn elsewhere. Timeline menu: Render preview (in to out) (Enter), Render preview (whole timeline), Cancel render preview, Delete render files. While a render runs, its stretch fills green from the left as it goes, and Cancel render preview is enabled; the result comes back as a notice.

### 6.6 Lanes

Described in §5 and §7.

### 6.7 Dock: Media tab

- A search field at the top.
- Rows are 56px tall. Each row holds: a dot, the name without its extension, a mono meta line such as `H.264 · 00:09:15 · 1080p`, and a `+ Add` button.
- Double-click adds the item at the playhead.
- Right-click opens the source menu: Add to timeline ↵ · Add at playhead · Preview Space · Make proxy · Reveal in files · Properties · — · Remove from library · Remove with its clips (shown in err on hover).
- Footer: `+ Import media… Ctrl+I`.
- Empty state: "No media yet", followed by the import button.

### 6.8 Dock: Clip tab

- Nothing selected: "Select a clip to adjust it", with "or press Tab" in ink3.
- Selected: a header (name, then the mono span), then tool rows.
  - Each tool row is 44px tall. The name is on the left and the current value on the right. The value is ink3 at default and ink1 when changed.
  - Rows expand in place. Several may be open at once. Open state is remembered.
- Bottom: `Delete clip`.
- Cards (Speed, Colour, Transform, EQ, Silence, Mix, Markers, History, Tools): History lists the undo steps newest first, the current step marked with a dot; clicking a step jumps to it.
- Cards (Speed, Colour, Transform, EQ, Silence, Mix, Markers, Tools): the verb list is the tab's home. Opening a card replaces the list in the same place, always. One shared frame: a head row (`←` back to the list, the title with the clip name, `Reset all` where it applies, `✕` close), then the body at the same padding, 6px section gap, label column and type scale. No card floats over the viewer or the timeline; Esc and `←` return to the list. A tooltip is shown only where it adds something (a chord, a reason).
- Effects card: the stack runs top to bottom, one row per effect with its on switch, move arrows and remove. A rect or ellipse mask's position and size rows (Centre across, Centre down, Width, Height) sit under a keyframe diamond: clicking it keys the mask at the playhead, and the diamond shows green while the playhead is on a key. Keyed masks show their keys as marks on the clip's ruler. A mask's last row is a Track row: "Track backward" and "Track forward" follow the picture under the mask from the playhead; while it runs the row reads "Tracking N%" with a Cancel, and the finished path lands as keys.

### 6.9 Status bar

- Left: the selected clip or gap readout, such as `city_dusk · 00:00:00:00 → 00:00:09:15 · 9.5s`. During an export, `Exporting N%` shows on the left too.
- Right: `1920×1080 · 30 fps` · `Ctrl+K Commands` · `? Shortcuts`.
- The project name and Saved or Unsaved are in the menu bar (§6.1), not here.

---

## 7. Timeline behaviour

- **Zoom.** A new project opens at 60 px/s. Fit (Ctrl+0 or the button) fits the whole edit with about a 2% margin. The zoom keys are = and -. Ctrl+wheel zooms about the pointer. A plain wheel scrolls horizontally. A thin 8px scrollbar sits under the lanes.
- **Imports.** The first import into an empty timeline lands on it and fits. Later imports go to Media.
- **Insert.** Double-click, `+ Add` and ↵ insert at the playhead. Dragging and dropping onto a lane inserts at the drop point.
- **Trim.** Drag an edge. A plate shows the delta and the new length, for example `+00:12 · 9.5s → 10.0s`. Trimming stops at the source's media limit, with a 120ms warn flash.
- **Move.** While dragging, a translucent ghost shows the target and the original is dimmed. While snapping is active, a 1px ink1 guide runs through all lanes.
- **Gaps.** Gaps are drawn hatched in ink4. Clicking a gap selects it (1px ink2 outline) and shows an inline `Close gap` button. Delete closes the gap.
- **Right-click menus** exist for clips, gaps, lane heads, the ruler and Media rows. They use the same plate style, show keys, and place destructive verbs below a rule.
- **Lanes.**
  - Hide, mute and lock use the glyphs in §3.
  - Hidden and muted lanes dim to 40%.
  - Locked lanes refuse selection and edits. The refusal comes with a toast that offers Unlock.
  - Tracks are added and removed from the lane-head menu or the Timeline menu.
- **Subtitle cues.** They use the same trim and move behaviour as clips. Double-click edits the text inline. Enter commits and Esc cancels.
- **Undo.** Undo covers every timeline edit, one step per gesture.
- **Clip volume line.** On a selected audio clip the line is editable. Ctrl+click on the line adds a key where it is. Drag a key to move it in time and dB (a plate shows the dB while dragging). Drag the line between two keys to move both. Double-click a key, or pick it and press Delete, to remove it. Each gesture is one undo step. Keys are dots when the audio lane is tall. The cursor changes over the line and the keys.
- **Audible scrub.** While the ruler is dragged and playback is paused, a short snippet plays at each new position. View > Audio scrubbing switches it, on by default, and the choice is saved with the view.

---

## 8. Feedback

- **Toasts** rise above the status bar at the bottom-left of the timeline. They never cover the picture. At most 3 are stacked. Each has a 3px left stripe: ink3 for info, warn or err. The text is 14px ink1. An optional action may appear: Undo, Unlock, or Show in folder.
- **Auto-hide.** Info and warn toasts hide after 5s. Errors stay until dismissed. Errors always name the remedy.
- **Refusal strings are claims.** "cannot be decoded" may ship only together with a test that proves the capability is absent.
- **Long work shows progress where it happens.**
  - Import: status bar, with cancel.
  - Export: sheet or status bar, with ETA and cancel. Cancel asks for an inline confirmation.
  - Silence scan: the count updates live.
  - Proxies: row meta reads "proxy 42%".
- **Dropped frames.** While playing, the status bar's right edge says `Dropped N frames · try Half quality` once the picture has skipped frames (at full quality only). The slot is fixed-width and the words stay for 3 s after a stop.

---

## 9. Tool panels

The Panels tab of the reference artifact is the reference.

### 9.1 Shared anatomy

1. Header: clip name, plus `Reset all`.
2. A picture of what the tool does: preview, histogram, curve, waveform or meters.
3. At most 3–4 main controls.
4. The rest folded under a `More …` section. It is closed by default, and its state is remembered.
5. Commit actions: a box for the commit, a ghost button for the alternative.

**Slider row.** The label is on the left and the value on the right, in mono. The value is ink3 at default and ink1 when changed. The track is 4px. The thumb is 14px, with a 24px hit area. A `Reset` ghost button appears only when the value is changed. Double-clicking the value resets it. Arrow keys step the value.

### 9.2 Per tool

- **Colour.** Live preview and a luma histogram. Controls: Brightness, Contrast, Saturation, Tint. An Auto colour button reads a correction off the clip as shot (grey-world white balance and a luma stretch to 2 % and 98 %) and writes it as one undo step; saturation, tint, lift, gamma, curves and the LUT are left as they are. A Look dropdown lists the eight built-in looks (Warm, Cool, Teal & orange, Bleach bypass, Black & white, Faded film, High contrast, Vintage) and the user's saved looks; picking one replaces the clip's colour as one undo step. Save current as look… opens a name field under the dropdown with Save and Cancel; saved looks are kept as plain text files in the user's data folder. A Compare row (Off, Split, Side by side) sets the viewer's compare view, which is preview only and never exported.
- **Transform.** Position X and Y (px), Scale (%), Rotation (°) with −90, 0, +90 and 180 chips. A CROP section holds a live crop diagram and four sliders.
- **Speed.** Chips for 0.25× to 4×. A log slider from 0.25× to 4×, which is the engine's real range. A readout shows the resulting length. Speed keeps pitch: the engine time-stretches the audio (WSOLA, `stretch.rs`), so a faster or slower clip keeps the voice and the notes at their original pitch.
- **Silence.** A waveform with silences shaded and counted live. A draggable threshold line. Controls: Apply to (take, track or every track), Threshold, Minimum silence. A `More timing` section holds Keep around speech, Ignore kept bits under, Level unit and Speed-up rate. Buttons: `Remove silences ↵` (the box) and `Speed them up F`. An `Invert` switch.
- **EQ.** A log-frequency graph (20 Hz to 20 kHz, ±18 dB view) with a live spectrum and draggable numbered bands.
  - Drag adjusts frequency and gain. Wheel adjusts Q. Double-click resets a band. Clicking empty space adds a band.
  - A band strip has type chips (Bell, Low shelf, High shelf, Low cut, High cut) and Freq, Gain and Q steppers.
  - Buttons: Add A, Remove X, Flatten R, Spectrum S, Bypass B.
- **Mix.** Channel strips: one per audio lane, plus Master. Each has a vertical fader (double-click sets 0 dB) and a meter with peak hold (warn above −6 dBFS, err above −1 dBFS). Each has Mute and Solo. Mute is saved with the project and exported. Solo is for listening only: it is not saved, and export ignores it. Master also holds the limiter toggle and the ceiling.
  - On a selected audio clip, the Mix card also has a **Pitch** row: a typed value in semitones (`+3 semitones`, `0 semitones`) and a centred slider from −12 to +12 with a notch at 0. Pitch keeps the clip's length.
- **Visualizer.** A live preview. Style chips: Fill, Ribbon, Ring, Waveform, and Fast. Colour and Strand swatches, each with an inline hue ring and a hex value. Strands and Glow sliders. Visualizer colours may be any hue, because they are footage content, not chrome.
- **Subtitle style.** A live preview. A font list, where each name is shown in its own face. Size. Outline (None, Thin, Thick). Background box. Position (Bottom, Top). A note reads "Saved for all projects. The preview shows the look only. Export writes soft subtitles (tx3g in MP4, S_TEXT/UTF8 in MKV), and the viewer's player draws them, so the look may differ slightly."
- **Title animation.** On the Title card, two dropdowns below the presets: Entrance and Exit (None, Fade, Typewriter, Slide up, Slide left, Pop, Word by word), then Animation length in seconds (a typed or dragged number, greyed until an animation is picked). Picking one moves the playhead to the title's start so the animation shows in the viewer.
- **Export (dock tab).** The dock's third tab, Media | Clip | Export. Opened by the Export key (E), File › Export…, the toolbar Export button or the tab itself. Leaving it is picking another tab (or Esc). There is no centred sheet and no scrim: the panel is in flow in the dock, so nothing covers the viewer or the timeline.
  - Layout: a scrolling body (wheel, and Up/Down/PageUp/PageDown/Home/End) over a fixed footer holding one full-width `Export` button. Every control is reachable by scrolling; a short window only scrolls more.
  - Settings state, top to bottom:
    - Presets, as compact two-line rows grouped Web video, Phones and social, Sound only, Archive, then Custom. Each row shows the name, a one-line spec (`1080p · H.264 · 8 Mb/s`, `M4A · 128 kbps`) and a ✓ plus a raised fill on the chosen one. The rows come from the engine's preset table (`export_presets`); the GUI keeps no copy of it. Custom is chosen whenever the settings match no preset.
    - Range: Whole film or In–Out, each with its duration. In–Out is greyed with the reason when there are no valid marks.
    - Save as: Name and Folder fields, typed in place; the extension follows the format. `Choose… d` opens the desktop save dialog.
    - Summary in plain words (duration, size, time), for example `1920×1080 · 30 fps · H.264 · 256 kbps sound · about 180 MB · about 1 min 40 s to export`. Warnings follow: upscaling, a very low bitrate, a sound-only file that drops the picture, no sound, WebM without subtitles.
    - `▸ More settings`, closed by default and remembered, disclosed inline. It holds every detailed setting as visible lists or segmented choices: format, picture size, picture bitrate, bit depth (AV1), sound quality, loudness, the encoder seat, and what travels with the file.
    - Picture formats (PNG still, numbered PNG pictures, GIF) have no sound and no video rows. The summary says `PNG · 1280×720`, `PNG · 1280×720 · one per frame` or `GIF · 480×270 · 15 fps · loops`, with `Audio  None`. A GIF adds Width (320, 480, 640 or 720 px), Frame rate (10, 15, 20 or 30 fps) and Loop as segmented or list choices. A numbered run's Name is the folder name, with no extension.
    - The footer `Export ↵` is the one primary button. A refused format greys it and says why.
  - Honest estimates. The size is "about X", or a span where a quality-driven encoder decides (GPU AV1). The time comes from a measured pace (a one-second software-seat trial write when the tab opens, deleted afterwards); until then it reads "estimating export time…", never a made-up number.
  - Encoder seat: Software is the default and is listed first. Auto and Hardware are named "GPU encoder…" and are used only when picked.
  - Overwrite state: when the file exists and this window did not write it, the footer becomes "name already exists in this folder" with `Cancel Esc`, `Keep both k` and `Replace ↵`, inline in the panel.
  - Progress state, in the same panel: the file name, a bar, `N% · elapsed · about M left`, the encoders in use, and `Cancel export` with an inline confirmation. Switching tab leaves the export running; the status bar keeps `Exporting N%`.
  - Done state: the file, its folder, size and time taken, with `Open folder`, `Play` and `Done ↵`.
  - Failed state: the engine's reason, with `Retry`, `Back to settings` and `Close`.
- **Settings (centred sheet).**
  - Left-hand tabs: Project, Editor, Export defaults, Keys.
  - Each section states where its values are saved: "Saved in this project" or "Saved on this computer".
  - Any non-obvious setting has a one-line description beneath it.
  - The Keys tab lists shortcuts and lets the user rebind them.
- **Properties (plate).** FILE: name, path with a Copy button, size, container. STREAMS: codec, size, rate, bit depth and bitrate for each stream. For a timeline clip, also THIS CLIP: source range and speed.

---

## 10. Keys

### 10.1 Principles

- Cross-editor conventions come first. Premiere, Resolve, Shotcut and FCP mostly agree.
- Single letters are used only for frequent, non-destructive verbs.
- Nothing destructive sits on a bare letter. Delete and Backspace delete. `x` no longer does.
- No chord may mean something else on a Linux desktop. Ctrl+Q (quit), Ctrl+W (close), Ctrl+T, Ctrl+L, Ctrl+M and Ctrl+H stay free or keep their usual meaning.
- No two actions share a chord.
- Keys are rebindable in Settings → Keys.

### 10.2 Key table

| Action | Key |
|---|---|
| Play/Pause | Space |
| Shuttle reverse / pause / forward | J / K / L |
| Frame back / forward | ← / → |
| Previous / next cut | ↑ / ↓ (also `,` / `.`) |
| Start / End | Home / End |
| Mark in / out | I / O (on a previewed library item: the source's marks) |
| Insert / Overwrite the previewed source at the playhead | F9 / F10 |
| Clear marks | Ctrl+U |
| Split | S |
| Delete | Delete / Backspace |
| Lift | ; |
| Trim start / end to playhead | Q / W |
| Nudge trim | [ / ] |
| Undo | Ctrl+Z |
| Redo | Ctrl+Y (Ctrl+Shift+Z once Shift chords exist) |
| Snap | N |
| Zoom in / out | = / - (also Ctrl+= / Ctrl+-) |
| Fit | \ or Ctrl+0 |
| Copy / Paste | Ctrl+C / Ctrl+V |
| Select all | Ctrl+A |
| Deselect | Esc |
| Speed / Colour / Transform / EQ / Silence / Visualizer / Fit policy (clip tools) | Alt+1 / Alt+2 / Alt+3 / Alt+4 / Alt+5 / Alt+6 / Alt+7 |
| Mix | F |
| Mute | M |
| Volume down / up | PageDown / PageUp |
| Subtitles | T |
| Subtitle style | Y |
| Import media | Ctrl+I |
| Import subtitles | Ctrl+Shift+I |
| Save | Ctrl+S |
| Export | E |
| Command search | Ctrl+K |
| Shortcuts | ? / F1 |
| Settings | Ctrl+, |
| Full screen | F11 |
| Screenshot | F12 |

Keys printed inside a tool panel (EQ's A/X/R/S/B, Silence's ↵ and F) act only while that panel has focus, and override the global key there.

The full 80-row mapping (today's 77 actions plus the three new J/K/L shuttle actions), from today's keys to the proposed ones with reasons, is the Keys tab of the reference artifact. Implementation updates `crates/app/src/keymap.rs` from it.

### 10.3 Engineering prerequisites

1. `Chord` must support Shift with letters. Today gpui drops Shift, and the parser refuses uppercase. This must land before Ctrl+Shift+I or Ctrl+Shift+Z can work.
2. J/K/L shuttle is a new action.
   - L plays forward. Each further press speeds it up: 1×, 2×, 4×, 8×.
   - J plays in reverse at 1×. A second press makes it 2×.
   - K stops.
   - Sound is muted above 1× and in reverse.
   - While reversing, L slows the reverse down.

---

## 11. Adding a feature

Before a new surface merges, it must answer each of these:

1. Which task is it, and how often is it used? That decides where it lives. It goes on the toolbar only if it is used many times an hour.
2. Its menu-bar entry and key (§4.4 and §4.5).
3. Contrast, size and target floors (§4.1 to §4.3).
4. Achromatic chrome. Film ink only for identity.
5. Empty, refused and failed states, with words (§4.8 and §8).
6. Degradation by width, if it lives on the timeline (§5).
7. A feedback toast (§8).
8. Continuity across reopen (§5).
9. A screenshot from the headless display, placed next to the reference artifact in the PR or commit description.

If a feature cannot pass, change the feature, or amend this document in the same diff. Either way, state the change explicitly and give the reason.

---

## 12. Implementation plan

Phases run from cheapest and biggest-win first. Each phase ends with headless screenshots compared against the reference artifact.

- **A — Comfort floors.** Tokens and contrast (§2, §4.1 and §4.2). Hit targets (§4.3). Toasts (§8). Selection and focus (§4.10). Import lands on the timeline. Double-click adds.
- **B — Doors.** Menu bar with every action. Edit toolbar. Command search (Ctrl+K). Tooltips with keys and reasons. Keymap update (§10), including Shift chords.
- **C — Timeline.** Lane heads with glyphs, and lock, mute and hide. Lane resize. Adaptive ruler. Playhead tab. Trim plate and media limit. Move ghost and snap guides. Gaps. Right-click menus. Inline cue edit. Overview strip window box. Layout proportions.
- **D — Panels.** The §9 redesigns, plus new capabilities: Mix mute and solo, EQ low-cut and high-cut bands, subtitle outline, box and position, and J/K/L shuttle.

Code comments that cite the old DESIGN.md's section numbers (49 of them) are updated to this document's sections as each phase touches their files.

## Video output window (View menu)

View > Video output window opens a second window that shows only the program picture, full-bleed on black and letterboxed, with no chrome. It reuses the viewer's own frame (`Player::image`), so there is no second decode, and it repaints with the main window. Esc or a double-click closes it. The window is session state only and is not saved in the project. View > Output display (a ✓ list of screens, shown only when there are two or more) moves it to a screen. On Wayland, gpui now fullscreens a window that names a display on that output (third_party gpui patch).
