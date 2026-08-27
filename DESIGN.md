# edith — Design Language: The Darkroom

This document is the binding design contract for edith's UI. It was distilled from the approved
concept mock ("Darkroom", 2026-08-20) and it outlives that mock: every existing surface is being
rebuilt to this language, and **every future feature must be designed inside it**. When a change
cannot be expressed in this language, the language gets amended here first — in the same diff —
never silently violated.

Visual reference (playable mock, v16): https://claude.ai/code/artifact/7781984a-a0cf-4af4-9c33-d38b5a9722a7

---

## 1. Thesis

**The film paints the tool. The tool itself is achromatic.**

edith is a darkroom: a dim, stable, monochrome room whose only colors are extracted from the
footage on the bench, and whose only pure white is the lamp — the playhead and the splices.
The editor sits in this room for eight to twelve hours; everything below exists to keep that
person in their groove.

Three laws derive everything else:

1. **Chrome is achromatic.** No panel, button, tab, slider, or scrollbar carries a hue. Loudness
   is luminance, never saturation.
2. **Film-extracted inks are the only color.** Each source contributes one ink, quantized from
   its own frames (hue kept; saturation/lightness clamped into the WCAG-passing band). Inks mark
   *identity* — spines, waveforms, traces, dots — never chrome.
3. **White is the lamp.** `#FFFFFF` is reserved for the playhead, its flag, and splice marks.
   Nothing else may be pure white.

## 2. Tokens

Surfaces (dark-only; no light theme, ever):

| Token | Value | Role |
|---|---|---|
| `canvas` | `#050607` | app ground, bench background |
| `panel` | `#0E1013` | spine, dock, ledger, plates' parent surfaces |
| `raised` | `#14171B` – `#17191D` | hover fill step, active verb fill |
| `hairline` | `#22262B` | 1px separations *within* a panel |
| seam | `rgba(0,0,0,.7)` | 1px panel-to-panel boundary |
| plate | `#050607` on `panel` | readout/notice/menu/cue body |

Ink (text) ladder — four levels, all WCAG ≥4.5:1 on their surfaces:

| Token | Value | Role |
|---|---|---|
| `ink1` | `#E6E9EC` | primary: values, active labels, timecode digits |
| `ink2` | `#9BA3AC` | secondary: labels, resting verbs, cue text |
| `ink3` | `#5E656D` | tertiary: chords at rest, metadata, section heads |
| `ink4` | `#3C4249` | faintest: decaying notices, dimmed-room hints |

Film inks: extracted per source at import (quantize dominant hues → clamp S/L into the contrast
band). Reference extraction from the standing test frame: azure `#64B5D1`, magenta `#D164B5`,
green `#64D19A`, violet `#7F64D1`, teal `#64D1D1`. The wheel caps at **12 hues**; beyond 12,
hues repeat and the name plate carries identity (the dot was always the secondary cue).
Selection uses the complement-leaning second ink (magenta against azure), not a chrome accent.

**Ink demotion rule:** extraction is ambient — it just happens. Re-inking a source is a
once-a-project act and lives behind right-click on the source's dot. Ink controls never occupy
the Sources tab, a toolbar, or any burst-use surface.

**Safelight:** the empty room (no footage) is warm monochrome (`#0A0908` family) with amber
`#FF9D57` on exactly one glyph. When footage lands, a 900ms skippable film-leader fade hands the
room to the film's inks. The 12 legacy palettes survive only as safelight hue choices.

Guard: the existing `no_colour_is_written_outside_the_theme` test remains the enforcement
mechanism — every value above is a theme token, never a literal at a call site.

## 3. Type

Two faces, fixed roles (bundled, no runtime deps):

- **Archivo** — the room's voice: labels, verbs, section heads. Weights 400/500/700.
- **Spline Sans Mono** — everything the film says: timecode, chords, readouts, metadata,
  cue text, the ledger. Mono = data; if a string is *about the footage or a key*, it is mono.

Scale: 18px hero timecode (700, colons in `ink3`) · 15px labels/rows (500) · 13–14px chords
and metadata (500) · 12px section heads (Archivo 700, uppercase, +0.14em letter-spacing, `ink3`).
Nothing below 10px. Every size is a whole pixel (no half-pixel sizes -- they put glyph baselines
off the pixel grid and blurred cosmic-text's AA). No italics. Tabular figures wherever digits
align.

## 4. Ghost grammar — the control language

- **Flow controls are ghosts:** borderless glyph + dim chord (`ink2` glyph, `ink3` chord).
  Hover = one fill step (`raised`) + ink brighten. Active = `ink1` + fill. No borders, no boxes,
  no icon assets — glyphs are hand-drawn/typographic.
- **Boxes are commitments:** the single bordered chip is reserved for commit-class actions
  (Export). If a second boxed control ever appears on one surface, one of them is wrong.
- **Readouts are plates:** dark (`canvas`-on-`panel`) rectangles carrying mono text. Notices,
  menus, chips, cues, and tooltips are all the same plate.
- **Every command wears its chord**, everywhere it appears — spine, menus, verbs, the `?`
  overlay. Nothing lives only in a menu; nothing lives only on a key. (This kills the
  shortcut-sync defect class permanently: the UI *is* the shortcut sheet.)
- **The most-read element anchors its region** — timecode leads the time band; the film leads
  the room.
- Radii: 0 for lanes/clips/room chrome · 2px plates · 3px verbs/controls · 4–6px floating
  (menus, sheets). No pills, no circles except source dots.
- Hover never adds hue. Focus/selection ring = 1px `ink1` (lamp-adjacent, not colored).

## 5. The stance — layout

Fixed geography; nothing moves, nothing occludes the picture:

```
┌──┬──────────────────────────────┬──────┐
│  │           SCREEN             │      │
│sp│    (picture — top, never     │ dock │
│in│         occluded)            │ Src/ │
│e │                              │ Clip │
│  ├──────────────────────────────┤ tabs │
│56│ TIME BAND: timecode → ghost  │      │
│px│  transport → cut readout →   │      │
│  │  contact strip → [Export]    │      │
│  ├──────────────────────────────┤      │
│  │ BENCH (lanes V/A/S)          │      │
│  ├──────────────────────────────┤      │
│  │ LEDGER (project · state)     │      │
└──┴──────────────────────────────┴──────┘
```

- **Spine** (56px, left): every command as ghost glyph + chord, grouped by task frequency.
- **Screen**: the picture. Never covered — not by drawers, notices, drags, or menus. Two-up
  OUT|IN judging renders *in* the screen at rest on a cut.
- **Time band**: timecode leads; ghost transport; cut readout (`14/37`); the **contact strip**
  (whole-film motion-trace minimap, `FILM` label, viewport bracket with grip notches — click
  jumps, drag pans); boxed **Export** at the end.
- **Bench**: lanes. Clip anatomy = ink spine (3px left edge) + real thumbnails/waveform in the
  source's ink + name plate + splice gaps in lamp white.
- **Ledger**: project identity (`name.edith · saved`), last action, export progress, position.
  State lives here, notices rise from here.
- **Dock** (right, the only side panel): Sources tab (assembly: filter, usage chips, import) and
  Clip tab (verbs: Speed / Colour / Transform / EQ as ghost verbs over param rows).

Session continuity: a room reopens exactly as left — playhead, subject cut, viewport, dock tab.

## 6. Cut machinery — the interaction core

The cut is the subject. Grammar (implement before any cosmetics):

- `,` `.` — walk cuts (shift = stride 10). The cut readout is the odometer.
- `[` `]` — no-aim trim of the subject cut, frame detents, works at any zoom (even 4px clips).
- `ctrl+{` `ctrl+}` — trim-to-playhead (debt #42): snaps the subject cut's in/out edge straight to
  the playhead, one press, one undo step -- the keyboard's own version of dragging the edge to a
  spot instead of nudging it a detent at a time. Needs a subject cut with the playhead resting on
  it, same as the pair above.
- `/` — loop-trim: loop around the subject cut while trimming (the modernized Avid trim mode).
- Two-up OUT|IN on the screen when resting on a cut.
- `j k l` — shuttle; `s` — split; `↵` context-dependent commit; `esc` always retreats.
- Param rows: drag while playing; value ghosts on the frame; `r` resets; live row takes the ink.
- Colour verb: `v` holds previous shot beside current (matching is comparative).
- Subtitle cues: cue edge is a cut — the same `,` `.` `[` `]` grammar applies; text edits on the
  picture where subtitles live.

## 7. Scale — the degradation ladder

Clips degrade **by width, one layer per threshold**, never all at once:
full anatomy (thumbnails, trace, plates, chips) → drop chips → drop labels (<48px; labels never
truncate into soup) → spine + trace only → sliver fill + splice gaps (film scale). Navigation
never depends on pixels: `,` `.` + contact strip work identically at 100 clips. Lanes compress
evenly to 5, then scroll behind the pinned ruler and track heads.

## 8. Notices — how the room speaks

No full-width bars, no covering the picture, ever. A notice is a plate rising above the ledger,
one at a time; severity is a **3px left spine**, not a color flood:

- grey `#5E656D` — *told you* (self-fades)
- amber `#D1B564` — *worth a look* (carries a jump action: `go to cut ,31`)
- red `#C85050` — *needs a decision* (the decision is on the plate: `relink ↵ / later esc`)

Every failure names its remedy, and the wound stays visible where it lives (dark clips in the
lane). **A refusal string is a claim:** shipping "cannot be decoded" requires a test proving the
capability is genuinely absent — otherwise it is a bug, not a message. A verb the state refuses
(in menus) greys out with its reason on hover; it never disappears.

**No instructional copy (rejected pattern, user 2026-08-27).** The room never explains itself in
prose: no "pick a verb above", no "type to filter — name, codec, unused…", no "Import, or drop a
file on the window". Empty states are empty (or a single noun); placeholders are one word or
absent; a control's use is taught by its chord and its geography (§4, §9), never by a sentence.
Telling the editor what to do is the keys overlay's job on request — not ambient text. Any new
string over ~4 words that instructs rather than reports state is a defect.

## 9. Menus and the keys overlay

- **Right-click = verbs of the thing under the cursor** (clip menu: clip verbs; lane head: lane
  verbs; source dot: ink acts). Plate styling, chords on every row, destructive verbs below a
  rule line. Never a junk drawer.
- **`?` held** dims the room one fill step and surfaces chords *in place* beside their controls;
  release restores, and a click on the scrim also dismisses it. Hold-to-peek, not a latch;
  learning stays geographic. The film keeps playing.

  **Amendment (2026-08-20), explicit per this section's own rule:** the geographic reading above
  is only partly built and edith currently ships the fallback instead — named here rather than
  silently violated.

  Fifteen of the keymap's 71 actions already have a geographic home in the darkroom stance today,
  and every one of those wears its chord **permanently**, not gated on `?` (spine glyphs, the
  time band's transport and Export chip, the dock's four Clip verbs) — a stronger reading of "the
  UI *is* the shortcut sheet" (§4) than "surfaces on hold" asks for, so nothing further was owed
  those fifteen. The other 56 actions (`,` `.` `[` `]` shuttle strides, marks, clipboard, delete,
  lift/detach/group/regroup, fit/resolution/zoom, subtitle add/style, silence, project cards,
  etc.) have **no widget anywhere in the room** — burst-use surfaces (screen, bench, time band)
  are the checklist's most expensive land (§11.1), and giving each of the 56 a geographic
  placement is that many individual placement decisions across the bench, time band and dock, not
  one fix. Until that lands, `?` held keeps opening `ui::stance::keys_overlay`: a single scrolling
  plate (plate styling per §4, positioned over the bench+ledger footprint so it never reaches the
  screen — §11.6's occlusion check holds) listing every action by [`keymap::Category`]. It is a
  cheat-sheet, and it is modal for as long as the key is held; both are accepted here rather than
  hidden.

  Ceiling to close this amendment: walk the 56 down to zero by giving each a home next to the
  control it already has no visual presence beside (marks and clipboard on the time band's cut
  readout row; lift/detach/group/regroup as bench lane-head ghosts; fit/resolution/zoom on the
  screen's own corner; subtitle/silence/project verbs into the dock next to the tab they already
  belong to) — at which point the plate deletes and `?` held becomes pure dim, no list.

## 10. The joy layer

Game-feel, not gamification. All specs ≤150ms, ease-out, tied to real actions, honor
`prefers-reduced-motion`; all sound behind one toggle, −30dB, and **always silent while the
film's own audio plays**.

- Split: 80ms white splice flash + dry mechanical tick.
- Trim nudge: 60ms detent per frame (rotary-encoder feel); held key = ratchet.
- Cut jump: 2-frame snap flash on arrival.
- Drop: 80ms magnetic settle + 1-frame squash. Never a teleport.
- JKL: optional pitch cue per speed step; blade leans 1px at 4×.
- Export: breathing ribbon on the boxed chip; film-leader countdown on open.

**Never-list:** no badges, streaks, confetti, or celebration copy; nothing moves during playback
except the blade; no sound over film audio; no animation as decoration (every motion reports a
state change).

## 11. Adding a feature — the checklist

Before any new surface merges, it answers:

1. **Frequency row:** which editor task, done how many times per hour, justifies its pixels?
   Burst-use surfaces (screen, bench, time band) are the most expensive land.
2. **Grammar fit:** flow → ghost; commitment → the box (is it really commit-class?);
   readout → plate; identity → ink; alarm → notice spine.
3. **Chord:** it has one, wears it everywhere it appears, and joins the `?` overlay.
4. **Scale:** define its degradation ladder step-by-width before merge.
5. **Achromatic check:** zero new hues in chrome; film inks only for identity; white only if it
   is literally the lamp.
6. **Occlusion check:** the picture stays visible; nothing floats over the screen.
7. **States:** empty (safelight-composed, never bare), refusing (reason named), failing
   (notice plate with remedy).
8. **Feel:** one ≤150ms motion spec from §10's vocabulary, or a stated reason for none.
9. **Continuity:** its state survives room close/reopen.

If a feature fails the checklist, either the feature changes or this document does — explicitly.

## 12. Implementation order

1. Token substrate in `crates/app/src/ui/theme.rs` (extend the 34-role system; keep the
   `no_colour_is_written_outside_the_theme` guard green).
2. The stance behind a flag (spine / screen / time band / bench / ledger / dock skeleton).
3. Cut machinery (`,` `.` `[` `]` `/`, two-up, readout) — before cosmetics.
4. Dock (Sources assembly + Clip verbs).
5. Ink extraction + grading pipeline; source dots; selection ink.
6. Traces + contact strip.
7. Notices, session continuity, safelight empty room, `?` overlay.
8. Joy layer last — it polishes verbs that already work.
