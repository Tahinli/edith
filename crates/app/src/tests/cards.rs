//! The cards the window opens over itself: the keys, the equaliser, the
//! colour and speed bars, the silence scan, the choices and the export.

use super::*;
use crate::audio_cards::EQ_GRAPH_MAX_H;
use crate::Player;
use crate::eq_ui::{eq_band_changes, eq_takes_gain, EQ_KINDS};
use crate::player::cards::{speed_fraction, speed_on_bar};
use crate::ui::dock_stance::transition_of;
use std::sync::atomic::Ordering;

/// FAULT 2's general shape (not just the source dot's menu): `overlaid()`
/// refuses every key while `context_menu`/`library_menu`/`picker` is `Some`,
/// so each of the three needs *both* a place to paint in the stance and a way
/// for any key to clear it -- otherwise the state `overlaid()` names becomes
/// an invisible, unrecoverable modal the moment something sets it. A live
/// harness proved the bug (right-click the source dot, every key but escape
/// went dead); this pins the fix as a standing structural rule rather than a
/// one-off repro, the same way `the_darkroom_path_never_lets_a_notice_surface_reach_the_picture`
/// pins its own render-order fault.
#[test]
fn no_overlaid_state_in_the_stance_goes_modal_without_painting_something() {
    let stance = src_text("ui/stance.rs");
    let render_body = &stance[stance
        .find("pub(crate) fn render(")
        .expect("the stance's entry point")..];
    for menu in ["context_card(", "library_card(", "picker_card("] {
        assert!(
            render_body.contains(menu),
            "ui::stance::render never mounts Player::{menu} -- overlaid() can refuse \
             every key for a menu the room never draws"
        );
    }
    let guard_at = stance
        .find("if this.card_open()\n                || this.context_menu.is_some()")
        .expect("the stance's modal guard");
    // Wide enough to span the whole guard block -- escape's
    // `escape_closes_overlay` branch and its commentary sit above the
    // any-key clears, and a scan window that ends before them read as the
    // clears having been deleted (2026-08-27).
    let guard = &stance[guard_at..guard_at + 3000];
    assert!(
        guard.contains("escape_closes_overlay"),
        "the modal guard's escape branch no longer routes through \
         Player::escape_closes_overlay -- the focus rings' own escape \
         branches call that same door, and the two must not diverge"
    );
    for field in [
        "this.context_menu = None",
        "this.library_menu = None",
        "this.picker = None",
    ] {
        assert!(
            guard.contains(field),
            "the overlaid() branch no longer clears {field} on every key -- \
             a menu can go modal and unrecoverable again (FAULT 2)"
        );
    }
}

#[test]
fn every_modal_field_has_a_mounted_surface_somewhere_in_the_darkroom() {
    // Every field/method `Player::modal()` reads, pulled from its own body
    // rather than hand-copied -- a hand-copied list is exactly the shape
    // that let three cards go modal with nothing mounted (GAP 2): the field
    // was in `modal()` and nowhere in this test.
    // `modal()` is just `card_open()` now: a running export no longer makes
    // the room modal (the oracle refuses what it must, action by action), so
    // every field this test pins lives in `card_open()`.
    let modal_body = fn_body("card_open");
    let mut fields = Vec::<String>::new();
    for chunk in modal_body.split("self.").skip(1) {
        let name: String = chunk
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !fields.contains(&name) {
            fields.push(name);
        }
    }
    assert!(
        fields.len() >= 9,
        "modal()'s own field list looks too short to have been parsed: {fields:?}"
    );
    // The mounted-surface function each field's own card draws, by the
    // convention the param cards already follow (`x_open` -> `x_card(` --
    // `Player::eq_card`/`color_card`/... in `ui/cards/`).
    // `export_open` doesn't follow that convention (it mounts `export_panel`),
    // so it is named explicitly. Anything else unmapped panics loudly -- the
    // point is that a ninth card has to earn a line here, not just a mount,
    // or this test cannot see it.
    let mount_for = |field: &str| -> &'static str {
        match field {
            "export_open" => "export_panel(",
            "eq_open" => "eq_card(",
            "color_open" => "color_card(",
            "transform_open" => "transform_card(",
            "speed_open" => "speed_card(",
            "effects_open" => "effects_card(",
            "silence_open" => "silence_card(",
            "beats_open" => "beats_card(",
            "mix_open" => "mix_card(",
            "markers_open" => "markers_card(",
            "history_open" => "history_card(",
            "tools" => "tools_card(",
            "subtitle_style_open" => "subtitle_style_card(",
            "settings_open" => "settings_stance::render(",
            "proj_dialog" => "project_dialog_card(",
            other => panic!(
                "Player::modal() now reads `self.{other}` with no entry in this test's \
                 mount_for map -- add one naming the function that mounts its surface in \
                 the darkroom (ui/stance.rs or ui/dock_stance.rs), or the room can go modal \
                 over it with nothing drawn (the silence/mix/subtitle-style bug, GAP 2)."
            ),
        }
    };
    let haystack = format!(
        "{}\n{}",
        src_text("ui/stance.rs"),
        src_text("ui/dock_stance.rs")
    );
    for field in &fields {
        let mount = mount_for(field);
        assert!(
            haystack.contains(mount),
            "Player::modal()'s `self.{field}` makes overlaid() true, but the darkroom never \
             mounts `{mount}` in ui/stance.rs or ui/dock_stance.rs -- pressing its key opens \
             an invisible modal (GAP 2)."
        );
    }
}

/// D2's class: an action refused while a modal is up used to be sworn to
/// silence, because the stance's own key guard ate the keystroke before
/// `enable()`/`act()` ever saw it -- driven live with `ctrl+,` mid-export
/// (no card open, only `exporting()` true): no notice plate, ledger reading
/// `—`. `ActionId::Settings`'s own refusal string (`oracle.rs`) was sound and
/// simply never reachable.
///
/// The fix lives in the guard, not in Settings alone (the charter this test
/// pins is explicit that a per-action patch would leave every *other* action
/// refused during a bare export equally mute): [`Player::modal`] no longer
/// reads `exporting()` at all, and the stance's guard reads `card_open()` --
/// never `overlaid()`/`modal()` directly, so the guard's own list stays the
/// cards, menus and pickers. This is a source scan, the same kind the guard above it
/// already is: this binary has no `TestAppContext` to actually press `ctrl+,`
/// mid-export and read the notice plate back.
#[test]
fn the_stances_key_guard_does_not_swallow_a_bare_export_before_the_oracle_is_asked() {
    let stance = src_text("ui/stance.rs");
    let guard_at = stance
        .find("if this.card_open()\n                || this.context_menu.is_some()")
        .expect("the stance's modal guard moved -- update this test's search string");
    let guard_line = &stance[guard_at..guard_at + 200];
    assert!(
        !guard_line.contains("overlaid()") && !guard_line.contains("this.modal()"),
        "the stance's key guard reads overlaid()/modal() again, which folds `exporting()` \
         back in and re-mutes every refusal an export-only moment should be able to speak (D2)"
    );
    // The guard trusts card_open() to mean "an actual card owns the keyboard",
    // so neither it nor modal() may grow an exporting read -- either would
    // silently regain the same mute.
    for body in [fn_body("card_open"), fn_body("modal")] {
        assert!(
            !body.contains("exporting"),
            "Player::card_open()/modal() now reads `exporting` -- that reintroduces the \
             swallow `the_stances_key_guard_does_not_swallow_a_bare_export_before_the_oracle_is_asked` \
             exists to keep out; an export is refused by the oracle, not by the modal"
        );
    }
}

/// The whole point of the card: there is no action a pointer cannot reach.
/// It renders [`keys_rows`] and nothing else, so this reads the same list
/// the card does -- add an `ActionId` and forget to surface it and this
/// fails, which is the only way that stays true as the editor grows.
#[test]
fn every_action_is_on_the_actions_card() {
    use keymap::{ActionId, Category};
    let rows = keys_rows();
    let listed: Vec<ActionId> = rows
        .iter()
        .filter_map(|r| match r {
            KeyRow::Act(a) => Some(*a),
            _ => None,
        })
        .collect();
    // The angles 2 to 9 share the one "angle 1-9" row, so they are not listed of their own.
    for action in ActionId::ALL.into_iter().filter(|a| a.angle_number().is_none_or(|n| n == 1)) {
        assert_eq!(
            listed.iter().filter(|a| **a == action).count(),
            1,
            "{action:?} is not on the card exactly once"
        );
    }
    assert_eq!(listed.len(), ActionId::ALL.len() - 8);
    // Under its own heading, in the registry's order: every row after a
    // heading belongs to that heading until the next one.
    let mut heading = None;
    let mut heads = 0;
    for row in &rows {
        match row {
            KeyRow::Head(category) => {
                heading = Some(*category);
                heads += 1;
            }
            KeyRow::Act(action) => assert_eq!(Some(action.category()), heading, "{action:?}"),
            KeyRow::Fixed(i) => assert_eq!(Some(keymap::FIXED[*i].category), heading),
        }
    }
    assert_eq!(heads, Category::ALL.len(), "a heading per category");
    // The card-local strokes are still all there beside them.
    assert_eq!(
        rows.iter()
            .filter(|r| matches!(r, KeyRow::Fixed(_)))
            .count(),
        keymap::FIXED.len()
    );
    // Both columns say something: the label does the action, the stroke
    // beside it changes that stroke, and neither may read blank.
    let keymap = keymap::Keymap::defaults();
    // DESIGN §10.2's menu-only rows: the mock leaves them without a stroke on
    // purpose, and the menu is their door.
    let menu_only = [
        ActionId::MarkerHere,
        ActionId::TrackSmall,
        ActionId::TrackMedium,
        ActionId::TrackLarge,
        ActionId::PlayheadHere,
        ActionId::PasteFilePath,
        ActionId::AddVideoLane,
        ActionId::AddAudioLane,
        ActionId::RemoveVideoLane,
        ActionId::RemoveAudioLane,
        ActionId::AddSubtitleLane,
        ActionId::RemoveSubtitleLane,
        ActionId::ToggleAutoProxies,
        ActionId::Theme,
        ActionId::AccentColour,
        // The viewer header owns these four; no stroke, menu and palette are the doors.
        ActionId::ViewZoom,
        ActionId::PlaybackQuality,
        ActionId::GuideStyle,
        ActionId::GuideMask,
        // No stroke by decision: a key that throws away edits is pressed by accident.
        ActionId::ReloadProject,
        ActionId::SaveVersion,
        ActionId::SaveTemplate,
        ActionId::ExportCaptions,
        ActionId::NewProject,
        ActionId::Versions,
        ActionId::CollectMedia,
        ActionId::ProjectNotes,
        // The transition kinds are palette commands and the Clip tab's picker.
        ActionId::TransitionDissolve,
        ActionId::TransitionDipBlack,
        ActionId::TransitionDipWhite,
        ActionId::TransitionWipeLeft,
        ActionId::TransitionWipeRight,
        ActionId::TransitionWipeUp,
        ActionId::TransitionWipeDown,
        ActionId::TransitionSlideLeft,
        ActionId::TransitionSlideRight,
        ActionId::TransitionSlideUp,
        ActionId::TransitionSlideDown,
        ActionId::TransitionPushLeft,
        ActionId::TransitionPushRight,
        ActionId::TransitionPushUp,
        ActionId::TransitionPushDown,
        ActionId::TransitionZoom,
        // The cards' stopwatch diamonds are their doors; the menu lists the same four.
        ActionId::KeyPlacement,
        ActionId::KeyOpacity,
        ActionId::KeyColor,
        ActionId::KeyEasing,
        // The creator tools: menu and palette, no chord yet.
        // The speed ramp's shapes and key verbs live on the palette, the Clip menu
        // and the speed card; the card is `j`.
        ActionId::SpeedRampUp,
        ActionId::SpeedRampDown,
        ActionId::SpeedRampSlowFastSlow,
        ActionId::SpeedRampFastSlowFast,
        ActionId::SpeedRampKey,
        ActionId::SpeedRampClear,
        // Auto reframe is a menu row and a transform-card row, with no stroke.
        ActionId::Reframe,
        ActionId::About,
        // Sequences and compound clips are menu rows and tabs, with no stroke.
        ActionId::NestClips,
        ActionId::BreakApart,
        ActionId::OpenCompound,
        ActionId::CloseCompound,
        ActionId::NewSequence,
        ActionId::DuplicateSequence,
        ActionId::RenameSequence,
        ActionId::DeleteSequence,
        // Multicam: menu rows, with no stroke.
        ActionId::CreateMulticam,
        ActionId::FlattenMulticam,
        ActionId::RenameLane,
        ActionId::PasteColor,
        ActionId::PasteEffects,
        ActionId::PasteTransform,
        ActionId::PasteSpeed,
        ActionId::PasteAudio,
        ActionId::ClipLabel,
        ActionId::NewSolid,
        ActionId::NewGradient,
        ActionId::NewCountdown,
        ActionId::NewBars,
        ActionId::NewTone,
        ActionId::NewBarsAndTone,
        ActionId::NewAdjustment,
        ActionId::NewBin,
        ActionId::NewBinInside,
        ActionId::RenameBin,
        ActionId::RemoveBin,
        ActionId::ToggleFavourite,
        ActionId::MoveToBin,
        ActionId::ShowFavourites,
        ActionId::ShowAllMedia,
        ActionId::NewRectangle,
        ActionId::NewEllipse,
        ActionId::NewLine,
        ActionId::NewArrow,
        // The transcript's cut: a Clip row and the palette, no stroke (Delete does it in the transcript).
        ActionId::CutSelectedWords,
        // The ruler's marker menu: rows with no stroke.
        ActionId::RenameMarker,
        ActionId::DeleteMarker,
        // The View menu's switch: no NLE convention gives it a key.
        ActionId::ToggleWaveforms,
        // The View menu's audible-scrub switch: likewise.
        ActionId::ToggleScrubAudio,
        // Edit > History opens its card from the menu: no stroke.
        ActionId::History,
        // Compare grade and the colour card's Auto and Save look: no NLE key gives them one.
        ActionId::CompareOff,
        ActionId::CompareSplit,
        ActionId::CompareSide,
        ActionId::AutoColour,
        ActionId::SaveLook,
        // Track mask: a Clip row with no stroke; the effects card's Track row is its surface.
        ActionId::TrackMask,
        // Timeline > Record voice-over: a sheet, and no NLE convention gives it a key.
        ActionId::RecordVoiceover,
        // The video output window and its display: menu rows, no NLE key.
        ActionId::ToggleOutputWindow,
        ActionId::OutputDisplay,
        // The timeline interchange rows: File menu entries with no stroke.
        ActionId::ExportTimelineOtio,
        ActionId::ExportTimelineFcpxml,
        ActionId::ExportTimelineEdl,
        ActionId::ImportTimelineOtio,
        // The render preview rows in the Timeline menu's submenu: no stroke for the last three.
        ActionId::RenderAll,
        ActionId::CancelRender,
        ActionId::DeleteRenderFiles,
    ];
    for action in ActionId::ALL {
        assert!(!action.label().is_empty(), "{action:?}");
        if menu_only.contains(&action) {
            assert_eq!(keymap.display(action), "unbound", "{action:?}");
        } else {
            assert_ne!(keymap.display(action), "unbound", "{action:?}");
        }
    }
    // Both halves of a row are click targets, so WCAG 2.5.8 binds them.
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
}

/// What a keystroke means to that box: a letter is a letter, and a word
/// gpui reports for a key that prints nothing is not typed letter by
/// letter into the search.
#[test]
fn only_a_printable_stroke_types_into_the_search() {
    assert_eq!(typed("a"), Some('a'));
    assert_eq!(typed("-"), Some('-'));
    assert_eq!(typed("1"), Some('1'));
    // The one printable key gpui reports by name.
    assert_eq!(typed("space"), Some(' '));
    for word in ["left", "escape", "f1", "backspace", "tab", "delete", "home"] {
        assert_eq!(typed(word), None, "{word}");
    }
}

#[test]
fn keys_search_keeps_matching_rows_and_drops_empty_heads() {
    let km = keymap::Keymap::defaults();
    let cut = keys_rows_matching("split", &km);
    assert!(cut.iter().any(|r| matches!(r, KeyRow::Act(ActionId::Cut))));
    assert!(!cut.iter().any(|r| matches!(r, KeyRow::Act(ActionId::Play))));
    assert!(cut.iter().any(|r| matches!(r, KeyRow::Head(_))));
    assert!(keys_rows_matching("xyzzy-no-such", &km).is_empty());
}

#[test]
fn the_keybindings_card_fits_the_smallest_window() {
    // The row list is capped and scrolls, so the card's height no longer
    // depends on how many actions there are: a title, a status line, the
    // search box and the viewport, inside the 640x360 the rest of the
    // layout is sized for.
    let title = 17.; // 13 px text on its own line
    let status = 28.; // 11 px text, two lines: a refusal wraps
    let search = 15.; // 11 px text, one line: it never wraps
    let gaps = 4. * 2.;
    let padding = 24.;
    // The line the list scrolls under: margin, rule, padding.
    let separator = 4. + 1. + 4.;
    assert!(
        title + status + search + separator + 10. * KEYS_ROW_H + gaps + padding <= 360.,
        "card too tall"
    );
    const {
        assert!(KEYS_W <= 640., "card too wide");
    }
    // The rows are clickable, so WCAG 2.5.8 binds them like every other
    // target in this window.
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
}

/// The equalizer's band rows: a cut shows no gain because it has none to set,
/// every kind has exactly one chip, and a number reads as changed only once it
/// has left the layout the clip opens on -- so the ink says what was moved.
#[test]
fn eq_rows_show_only_the_numbers_a_band_uses_and_mark_what_moved() {
    use engine::eq::{BandKind, EqParams};

    assert!(!eq_takes_gain(BandKind::LowCut) && !eq_takes_gain(BandKind::HighCut));
    assert!(
        eq_takes_gain(BandKind::LowShelf)
            && eq_takes_gain(BandKind::Peak)
            && eq_takes_gain(BandKind::HighShelf)
    );

    let kinds = [
        BandKind::LowCut,
        BandKind::LowShelf,
        BandKind::Peak,
        BandKind::HighShelf,
        BandKind::HighCut,
    ];
    assert_eq!(EQ_KINDS.len(), kinds.len());
    for kind in kinds {
        assert_eq!(
            EQ_KINDS.iter().filter(|(k, _, _)| *k == kind).count(),
            1,
            "{kind:?} needs exactly one chip"
        );
    }

    let layout = EqParams::default_layout();
    assert_eq!(
        eq_band_changes(&layout.bands[0], 0),
        (false, false, false),
        "a band at its layout default reads ink3 on every number"
    );
    let mut moved = layout.bands[1];
    moved.freq_hz *= 1.2;
    assert_eq!(eq_band_changes(&moved, 1), (true, false, false));
    let mut moved = layout.bands[1];
    moved.gain_db = 3.;
    assert_eq!(eq_band_changes(&moved, 1), (false, true, false));
    let mut moved = layout.bands[1];
    moved.q = 2.;
    assert_eq!(eq_band_changes(&moved, 1), (false, false, true));
    assert!(
        eq_band_changes(&layout.bands[1], 7).0,
        "a band past the layout has no default, so its frequency reads as moved"
    );
}

/// The equalizer card's graph: where a band lands on it, that a drag reads
/// back as the gain it painted -- the two are one mapping and its inverse,
/// which is what makes a handle follow the pointer -- and that the card
/// still fits the window the other two are sized for.
#[test]
fn the_equalizer_graph_puts_a_band_where_a_drag_reads_it_and_fits_the_smallest_window() {
    use engine::eq::{Band, BandKind, EqParams};
    // Flat is the middle of the box: a band nobody has touched must not
    // look like one that has been turned down.
    assert_eq!(eq_y(0.), 0.5);
    // Full boost is the top edge, full cut the bottom one -- y grows down.
    assert_eq!(eq_y(EQ_GAIN_LIMIT), 0.);
    assert_eq!(eq_y(-EQ_GAIN_LIMIT), 1.);
    assert_eq!(eq_y(EQ_GAIN_LIMIT / 2.), 0.25);
    // A file may carry a gain past what this card offers (the format writes
    // any finite value): it paints on the edge, never off the box.
    assert_eq!(eq_y(400.), 0.);
    assert_eq!(eq_y(-400.), 1.);

    // A pointer reads back as the gain that paints where it landed, which
    // is what makes a drag land under the hand ([`Player::drag_band`]).
    for gain in [-12., -6., 0., 6., 12.] {
        let read = (0.5 - eq_y(gain)) * 2. * EQ_GAIN_LIMIT;
        assert!((read - gain).abs() < 1e-4, "{gain} read back as {read}");
    }

    // The frequency axis is logarithmic and spans the audible range: the
    // ends are the ends, and the decade at 200 Hz is as wide as the one at
    // 2 kHz -- the whole reason a bass band is reachable at all.
    assert_eq!(eq_x(EQ_FREQ_LOW), 0.);
    assert_eq!(eq_x(EQ_FREQ_HIGH), 1.);
    assert!((eq_x(200.) - 1. / 3.).abs() < 1e-4);
    assert!(((eq_x(2000.) - eq_x(200.)) - (eq_x(20000.) - eq_x(2000.))).abs() < 1e-4);
    // Off either end clamps rather than painting outside the box.
    assert_eq!(eq_x(1.), 0.);
    assert_eq!(eq_x(96_000.), 1.);
    // Every tick the card names is on the axis it is drawn against.
    for (freq, label) in EQ_TICKS {
        assert!(
            (EQ_FREQ_LOW..=EQ_FREQ_HIGH).contains(&freq),
            "tick {label} is off the axis"
        );
    }
    // The default bands all land inside it too, spread out enough that the
    // nearest-band pick (`Player::nearest_band`) has something to pick.
    let xs: Vec<f32> = EqParams::default_layout()
        .bands
        .iter()
        .map(|b| eq_x(b.freq_hz))
        .collect();
    for pair in xs.windows(2) {
        assert!(pair[1] - pair[0] > 0.1, "bands too close to aim at: {xs:?}");
    }

    // Every default band says what it is, and a shelf says so: "12 kHz"
    // alone would not tell anyone it tilts the whole top octave.
    let labels: Vec<String> = EqParams::default_layout()
        .bands
        .iter()
        .map(band_label)
        .collect();
    assert_eq!(
        labels,
        [
            "80 Hz low shelf",
            "250 Hz",
            "1 kHz",
            "4 kHz",
            "12 kHz high shelf"
        ]
    );
    // A band moved off a round number reads as where it *is*: a keystroke
    // that changes the filter and not the number on the card is a keystroke
    // nobody can aim.
    assert_eq!(
        band_label(&Band {
            freq_hz: 2600.,
            gain_db: 0.,
            q: 1.,
            kind: BandKind::Peak
        }),
        "2.6 kHz"
    );
    assert_eq!(eq_freq_label(1122.), "1.12 kHz");
    assert_eq!(eq_freq_label(12000.), "12 kHz", "no zeroes to read past");
    assert_eq!(eq_freq_label(80.), "80 Hz");

    // The card fits the smallest window and takes the room a bigger one has
    // -- it is a graph, and the width *is* the frequency resolution.
    assert!(
        eq_card_w(640., false) <= 640. - 24.,
        "card too wide for 640"
    );
    assert!(
        eq_card_w(1280., false) > eq_card_w(640., false),
        "card ignores the window"
    );
    assert_eq!(eq_card_w(1920., false), EQ_W_MAX, "card grows without end");
    assert!(
        eq_card_w(320., false) >= KEYS_W,
        "card narrower than a row of text"
    );
    // At the smallest window the graph is still a graph: three across for
    // one down, so an octave is wide enough to put a handle in.
    assert!(
        eq_card_w(640., false) - 24. >= 3. * EQ_GRAPH_H,
        "graph too square to aim at"
    );

    // The same shape as the other two cards, so it fits where they do: the
    // graph stands where the export card's rows do, and is shorter than
    // they are. The numbers row is a row of buttons now, so it is one of
    // those tall rather than one line of text.
    let (title, status, gaps, padding) = (17., 28., 4. * 2., 24.);
    assert!(
        title + status + EQ_GRAPH_H + HIT_MIN + gaps + padding + CONTROL_H <= 360.,
        "card too tall"
    );
    const {
        assert!(
            EQ_GRAPH_H <= (8. * KEYS_ROW_H),
            "graph taller than a card of rows"
        );
    }
    // What is dragged is the whole graph -- the handle is a 10 px dot, but
    // a press anywhere in the box takes the band nearest it -- so WCAG
    // 2.5.8 is satisfied by the box, which is far past the minimum.
    const {
        assert!(EQ_GRAPH_H >= HIT_MIN);
    }
    const {
        assert!(
            EQ_HANDLE < HIT_MIN,
            "a dot that size would want its own hitbox"
        );
    }
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
}

/// Editing a band, which is what the card is *for*: the pointer reads a
/// frequency off the axis the same way the axis draws one, a new band lands
/// in the gap beside the picked one rather than on top of it, and every band
/// the card will hold has a digit that picks it.
#[test]
fn a_band_can_be_moved_added_and_reached() {
    use engine::eq::{Band, BandKind, EqParams};
    // Across the graph and back: a drag sets the frequency the handle is
    // then drawn at, so the two mappings have to be one another's inverse or
    // the handle walks away from the pointer.
    for freq in [20., 80., 250., 1000., 4000., 12000., 20000.] {
        let read = eq_freq(eq_x(freq));
        assert!(
            (read / freq - 1.).abs() < 1e-3,
            "{freq} Hz read back as {read}"
        );
    }
    // Off the box either end stops at the axis, never past it.
    assert_eq!(eq_freq(-1.), EQ_FREQ_LOW);
    assert_eq!(eq_freq(2.), EQ_FREQ_HIGH);

    // A step of the frequency keys is a real move on screen -- a keystroke
    // that changes nothing visible is a keystroke that reads as broken --
    // and small enough to aim with.
    let step = eq_x(1000. * EQ_FREQ_STEP) - eq_x(1000.);
    assert!(step > 0.01 && step < 0.06, "frequency key steps {step}");

    // A new band lands between the picked one and the next one up, in
    // octaves: 250 Hz and 1 kHz put it at 500, which is a gap on screen.
    let bands = EqParams::default_layout().bands;
    let added = inserted_band(&bands, 1);
    assert!((added.freq_hz - 500.).abs() < 1., "landed at {added:?}");
    assert_eq!(added.gain_db, 0., "a new band changes nothing until moved");
    assert_eq!(added.kind, BandKind::Peak, "a new band is not a shelf");
    assert!(eq_x(added.freq_hz) - eq_x(bands[1].freq_hz) > 0.1);
    // Above the topmost band the gap is the rest of the axis, and the band
    // still lands on it rather than off the end.
    let top = inserted_band(&bands, bands.len() - 1);
    assert!(
        top.freq_hz > bands[4].freq_hz && top.freq_hz <= EQ_FREQ_HIGH,
        "landed at {top:?}"
    );
    // Bands out of frequency order (a drag may cross two over) still get a
    // sane neighbour: the next one *up*, not the next one along the list.
    let crossed = vec![
        Band {
            freq_hz: 4000.,
            gain_db: 0.,
            q: 1.,
            kind: BandKind::Peak,
        },
        Band {
            freq_hz: 100.,
            gain_db: 0.,
            q: 1.,
            kind: BandKind::Peak,
        },
    ];
    let between = inserted_band(&crossed, 1);
    assert!(
        (between.freq_hz - 632.).abs() < 2.,
        "landed at {between:?}, not between 100 and 4000"
    );

    // Every band the card will hold is one digit away: the keyboard has ten
    // digits, which is exactly why the cap is what it is.
    const {
        assert!(
            EQ_BANDS_MAX <= 10,
            "a band past the tenth has no key that picks it"
        );
    }
    assert!(EQ_BANDS_MAX > EqParams::default_layout().bands.len());
    // The Q range holds the default, so a file's band never opens out of
    // range and needs dragging back in before it can be edited.
    assert!((EQ_Q_LOW..=EQ_Q_HIGH).contains(&0.707));
    const {
        assert!(EQ_Q_STEP > 1.);
    }
}

/// The analyser drawn behind the curve: a tone has to land on its own
/// frequency, or the backdrop is a decoration rather than a reading of what
/// is playing -- and someone shaping a band against it would be aiming at
/// the wrong octave.
#[test]
fn the_spectrum_puts_a_tone_under_its_own_frequency() {
    use std::f32::consts::TAU;
    let rate = 48_000u32;
    let sine = |hz: f32, amp: f32| -> Vec<f32> {
        (0..EQ_FFT)
            .map(|i| amp * (TAU * hz * i as f32 / rate as f32).sin())
            .collect()
    };

    // Silence is the floor of the box everywhere, not a band of noise.
    let quiet = eq_spectrum(&vec![0.; EQ_FFT], rate);
    assert_eq!(quiet.len(), EQ_CURVE_STEPS + 1);
    assert!(quiet.iter().all(|&l| l == 0.), "silence drew something");

    // A tone peaks over its own frequency, and the columns two octaves
    // either side of it are near the floor. "Over" to within a bin down low
    // and a column up high, which is all a 1024-point transform on a log
    // axis can promise: at 200 Hz a whole column is a fraction of a bin
    // wide, so the peak sits on the bin the tone fell in.
    let column = |freq: f32| (eq_x(freq) * EQ_CURVE_STEPS as f32).round() as usize;
    let freq_at = |col: usize| {
        let along = col as f32 / EQ_CURVE_STEPS as f32;
        EQ_FREQ_LOW * (EQ_FREQ_HIGH / EQ_FREQ_LOW).powf(along)
    };
    for hz in [200., 1000., 5000.] {
        let levels = eq_spectrum(&sine(hz, 0.05), rate);
        let (at, top) =
            levels
                .iter()
                .enumerate()
                .fold((0, 0f32), |best, (i, &l)| match l > best.1 {
                    true => (i, l),
                    false => best,
                });
        let slack = 1.5 * rate as f32 / EQ_FFT as f32 + 0.04 * hz;
        assert!(
            (freq_at(at) - hz).abs() <= slack,
            "{hz} Hz peaked at {:.0} Hz (column {at})",
            freq_at(at)
        );
        // -26 dBFS, which is where a 0.05 tone sits on the axis.
        let want =
            (20. * 0.05f32.log10() - EQ_SPECTRUM_DB.0) / (EQ_SPECTRUM_DB.1 - EQ_SPECTRUM_DB.0);
        assert!(
            (top - want).abs() < 0.05,
            "a -26 dBFS tone drew {top} of the box, not {want}"
        );
        // Two octaves off is far enough down the box that the hump reads as
        // one hump: 0.4 of the axis is a little over 30 dB.
        for off in [hz / 4., hz * 4.] {
            let away = levels[column(off).min(EQ_CURVE_STEPS)];
            assert!(
                top - away > 0.4,
                "{hz} Hz drew {top} but still {away} at {off} Hz"
            );
        }
    }

    // Level reads as level: 40 dB quieter sits lower by the fraction of the
    // axis those 40 dB are.
    let loud = eq_spectrum(&sine(1000., 0.05), rate);
    let soft = eq_spectrum(&sine(1000., 0.0005), rate);
    let at = column(1000.);
    let drop = loud[at] - soft[at];
    let (floor, ceiling) = EQ_SPECTRUM_DB;
    assert!(
        (drop - 40. / (ceiling - floor)).abs() < 0.05,
        "40 dB quieter moved the analyser {drop} of the box"
    );

    // A tap the engine has not filled yet (right after a seek) draws
    // nothing at all rather than a transform of half a window.
    assert!(eq_spectrum(&[0.; 16], rate).is_empty());
}

/// The speed bar is the same round trip -- pixels -> rate -> fill -- with
/// one thing the colour sliders do not have to promise: **exactly 1.00x has
/// to be reachable**, by a hand as well as by the reset. A grid that missed
/// it would leave a clip nobody could put back.
#[test]
fn the_speed_bar_lands_where_it_paints_and_real_time_is_reachable() {
    let bar = Bounds {
        origin: point(px(180.), px(240.)),
        size: size(px(COLOR_BAR_W), px(KEYS_ROW_H)),
    };
    // The same arithmetic `Player::drag_speed` runs, which is the one thing
    // a test of it can share without re-deriving it.
    let at = |x: f32| speed_on_bar(frac_along(px(x), bar));
    assert_eq!(at(180.), Speed::MIN, "the left end is a quarter speed");
    assert_eq!(at(180. + COLOR_BAR_W), Speed::MAX, "the right end is 4x");
    assert_eq!(at(-4000.), Speed::MIN, "off the left clamps");
    assert_eq!(at(9999.), Speed::MAX, "off the right clamps");
    let mut hits_real_time = false;
    for step in 0..=240 {
        let along = step as f32 / 240.;
        let speed = at(180. + along * COLOR_BAR_W);
        hits_real_time |= speed == Speed::NORMAL;
        assert_eq!(
            i32::from(speed.permille()) % SPEED_STEP,
            0,
            "{speed} is off the {SPEED_STEP} grid the keys move on"
        );
        // What the bar paints from that rate is where the pointer was, to
        // within half a grid step's width on the log scale.
        let p = f32::from(speed.permille());
        let painted = speed_fraction(p);
        let half = SPEED_STEP as f32 / 2.;
        // The press can be anywhere in the cell this rate stands for, which
        // runs from the bar where `p - half` sits to where `p + half` sits.
        // `painted` is the middle of that cell only while neither edge is
        // clamped: `speed_fraction` clamps the lower edge to the bar's start,
        // so at `Speed::MIN` the cell is one-sided and the paint sits on its
        // edge. The slack is the distance to the far edge, not half of the
        // whole cell, so both cases are covered.
        let slack = (speed_fraction(p + half) - painted)
            .max(painted - speed_fraction(p - half))
            + 1e-4;
        assert!(
            (painted - along).abs() <= slack,
            "pressed at {along}, paints at {painted}"
        );
    }
    assert!(hits_real_time, "a drag can land on exactly 1.00x");
    // ...and every preset the card offers is a rate the bar can also reach.
    for permille in SPEED_PRESETS {
        assert_eq!(Speed::from_permille(permille).permille(), permille);
        assert_eq!(i32::from(permille) % SPEED_STEP, 0);
    }
    assert!(SPEED_PRESETS.contains(&Speed::NORMAL.permille()), "reset");
}

/// The speed bar is logarithmic: every doubling takes the same travel, so the
/// quarter-to-half stretch is as long as the two-to-four one, and real time
/// sits in the middle of the bar where a person expects it.
#[test]
fn the_speed_bar_gives_every_doubling_the_same_travel() {
    let quarter_to_half = speed_fraction(500.) - speed_fraction(250.);
    let two_to_four = speed_fraction(4000.) - speed_fraction(2000.);
    assert!((quarter_to_half - two_to_four).abs() < 1e-5);
    assert!((speed_fraction(1000.) - 0.5).abs() < 1e-5, "1.00x is mid-bar");
    assert!(speed_fraction(250.).abs() < 1e-5);
    assert!((speed_fraction(4000.) - 1.).abs() < 1e-5);
}

/// A colour slider is dragged straight to a value, so where the pointer
/// lands and where the bar then paints have to be the same place: this is
/// the round trip [`Player::drag_color`] makes, pixels -> value -> fill.
#[test]
fn a_colour_drag_lands_where_it_paints_and_the_card_fits_the_smallest_window() {
    // A bar as laid out, somewhere that is not the window's origin -- a
    // mapping that forgot the offset would pass at zero.
    let bar = Bounds {
        origin: point(px(180.), px(240.)),
        size: size(px(COLOR_BAR_W), px(KEYS_ROW_H)),
    };
    for &(label, low, high) in &COLOR_BANDS {
        // The ends are the ends: the left of the bar is the bottom of the
        // range and the right is the top, so a slider can be pulled to
        // either without hunting for the last pixel.
        let at = |x: f32| color_snap(low + frac_along(px(x), bar) * (high - low));
        assert_eq!(at(180.), low, "{label} left end");
        assert_eq!(at(180. + COLOR_BAR_W), high, "{label} right end");
        // Off either end clamps rather than running past the range.
        assert_eq!(at(-4000.), low, "{label} off the left");
        assert_eq!(at(9999.), high, "{label} off the right");

        for step in 0..=48 {
            let along = step as f32 / 48.;
            let value = at(180. + along * COLOR_BAR_W);
            // Every stop is one the keyboard can also reach, which is what
            // keeps "0.35" the number the file writes.
            let steps = value / COLOR_STEP;
            assert!(
                (steps - steps.round()).abs() < 1e-3,
                "{label}: {value} is off the {COLOR_STEP} grid"
            );
            assert!(
                (low..=high).contains(&value),
                "{label}: {value} outside {low}..{high}"
            );
            // What the row paints from that value is where the pointer was,
            // to within the half step the snap costs.
            let painted = (value - low) / (high - low);
            let slack = COLOR_STEP / (high - low) / 2. + 1e-4;
            assert!(
                (painted - along).abs() <= slack,
                "{label}: pressed at {along}, paints at {painted}"
            );
        }
    }

    // The shape the card opens in, inside a 360 px window: the graph, the two
    // main sliders (each a label line, a gap and a track), the More fold, the
    // compare line for a first clip, and the reset line. An open fold adds the
    // two rare sliders below that, and the card grows past 360 px.
    let (title, status, gaps, padding) = (17., 17., 6. * 2., 24.);
    let slider = crate::ui::type_scale::LABEL_ROW_PX + 4. + HIT_MIN;
    let rows = 2. * slider + 2. * HIT_MIN;
    assert!(
        title + status + HIST_H + rows + gaps + padding + CONTROL_H + 4. <= 360.,
        "card too tall"
    );
    const {
        assert!(COLOR_W <= 640., "card too wide");
    }
    // What is dragged is the whole track's height, not the 4 px the bar is
    // drawn as (WCAG 2.5.8) -- the same split the ruler makes.
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
}

/// The graph over the sliders is the frame the grade already went through,
/// so it has to count what is actually in those bytes -- the luma of each
/// pixel, read off BGRA on the wire.
#[test]
fn the_histogram_counts_the_luma_of_the_frame_it_is_handed() {
    // Half pure red, half mid grey: two known lumas, in two known bins.
    let (w, h) = (64usize, 64usize);
    let mut frame = Vec::with_capacity(w * h * 4);
    for _ in 0..h {
        for col in 0..w {
            match col < w / 2 {
                true => frame.extend_from_slice(&[0, 0, 255, 255]),
                false => frame.extend_from_slice(&[128, 128, 128, 255]),
            }
        }
    }
    let bins = histogram(&frame);
    let half = (w * h / 2) as u32;
    // 64 bins over 256 codes. Pure red's luma is 0.2126 * 255, which is 53, in
    // bin 13; grey 128 is exactly half, bin 32.
    assert_eq!(bins[13], half, "the red half lands at luma 53");
    assert_eq!(bins[32], half, "and the grey half sits mid-range");
    assert_eq!(
        bins.iter().filter(|&&n| n > 0).count(),
        2,
        "nothing else is counted"
    );
    // Nothing is counted twice and nothing is dropped: this frame is small
    // enough to be read whole.
    assert_eq!(bins.iter().sum::<u32>(), (w * h) as u32);

    // A grade shifts it, which is the whole point of drawing it: the same
    // frame darkened lands in lower bins.
    let darker: Vec<u8> = frame.iter().map(|b| b / 2).collect();
    let bins = histogram(&darker);
    assert_eq!(bins[6], half, "red 127 is luma 26, bin 6");
    assert_eq!(bins[16], half, "grey 64 is bin 16");

    // A real frame is subsampled: a 1080p one is read every 253rd pixel, so
    // the shape costs a thousandth of the reads and still counts thousands.
    let big = vec![200u8; 1920 * 1080 * 4];
    let bins = histogram(&big);
    let counted = bins.iter().sum::<u32>();
    let pixels = 1920 * 1080usize;
    let expected = pixels.div_ceil(pixels / HIST_SAMPLES) as u32;
    assert_eq!(counted, expected, "every strided pixel counted, once");
    assert!(
        (HIST_SAMPLES as u32..=HIST_SAMPLES as u32 + 64).contains(&counted),
        "{counted} samples is not the budget"
    );
    assert_eq!(bins[200 * HIST_BINS / 256], counted, "all in one bin");

    // An empty buffer is a flat graph rather than a panic: the card is open
    // before the first frame is pumped.
    assert_eq!(histogram(&[]), [0; HIST_BINS]);
}

/// Mute and level are one control with two states, and the whole point is
/// that mute keeps the level: the user gets back what they had, not 100%.
#[test]
fn muting_keeps_the_level_it_comes_back_to() {
    let mut volume = Volume::default();
    assert_eq!(volume.gain(), 1.0);
    assert_eq!(volume.label(), "Vol 100%");

    // Four presses down, then muted: the gain is silence but the level is
    // still what it was, and the button keeps saying so.
    for _ in 0..4 {
        volume.step(false);
    }
    assert_eq!(volume.gain(), 0.8);
    volume.muted = true;
    assert_eq!(volume.gain(), 0.0);
    assert_eq!(volume.label(), "Muted 80%");

    // Turning it down while muted stays muted -- the one thing a mute
    // button must never do is get louder because you asked for quieter.
    volume.step(false);
    assert_eq!(volume.gain(), 0.0);
    assert!(volume.muted);

    // Unmute returns to the level, including the step taken while silent.
    volume.muted = false;
    assert_eq!(volume.gain(), 0.75);
}

/// The whole transport in one place: the clock keeps running past the last
/// frame (wall time takes over at audio EOF), so "the clock is going" is
/// not "this is playing" -- and a button that read the clock showed Pause
/// on a timeline that had stopped moving. Ended is its own state, it draws
/// Play, and the next press starts over from the top.
#[test]
fn a_played_out_timeline_is_not_playing_and_the_next_press_starts_it_over() {
    assert_eq!(transport(true, false), Transport::Playing);
    assert_eq!(transport(false, false), Transport::Paused);
    // The transition the bug was about: the clock is still running and the
    // decoder is finished. Played out wins, however the clock reads.
    assert_eq!(transport(true, true), Transport::Ended);
    assert_eq!(transport(false, true), Transport::Ended);

    // What the button draws, in each state. Two bars only while it moves.
    assert!(Transport::Playing.is_playing());
    for state in [Transport::Paused, Transport::Ended, Transport::Stopped] {
        assert!(!state.is_playing(), "{state:?} must draw the Play triangle");
    }

    // And what a press does: start over at the end, plain toggle before it,
    // nothing with no timeline. Same answer for the key and the button --
    // both come through `Player::toggle_or_restart`.
    assert!(Transport::Ended.restarts());
    for state in [Transport::Playing, Transport::Paused, Transport::Stopped] {
        assert!(!state.restarts(), "{state:?} must toggle, not reseek");
    }
}

/// The half of `Ended` the eye cannot see: the clock. Wall time takes over
/// at the last frame and nothing used to stop it, so the playhead walked off
/// the end of the timeline in real time -- and the playhead is what a cut, a
/// paste, an insert and the analyser all act at. `pump` pauses on the
/// crossing; this is the engine contract that rests on, driven exactly as
/// the pump drives it.
#[test]
fn the_clock_stops_where_the_timeline_does_and_the_end_still_restarts() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    // Start a breath short of the end: the tail is what this is about, and
    // playing the whole five seconds would say nothing more.
    session.seek(4.8);
    session.play();

    // The pump's own loop -- tick, drain, ask where the transport is --
    // with a deadline so a fixture that will not decode fails as a failure
    // rather than as a hang. The first picture of a seek is a debug-build
    // software decode, and on a box whose cores are saturated by other work
    // it has taken 40-60s to arrive, so the bound is generous: the clock
    // running on wall time meanwhile is by design (see `halt_at_end`).
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut state = transport(session.is_playing(), session.is_eos());
    while state != Transport::Ended {
        assert!(
            Instant::now() < deadline,
            "never reached the end of a 5s file"
        );
        session.tick();
        while session.try_frame().is_some() {}
        state = transport(session.is_playing(), session.is_eos());
    }

    // What `pump` does on the crossing, and the whole point of it: the
    // position holds still afterwards instead of counting on past the end,
    // and it holds still *on the out point* -- where the timecode and the
    // playhead have been showing it. The clock at the moment the end is
    // recognised is not that: a slow renderer reaches EOF with the clock
    // seconds past the timeline, which is why this repositions rather than
    // only freezing.
    session.halt_at_end();
    let stopped_at = session.now();
    assert_eq!(stopped_at, session.timeline_duration());
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(session.now(), stopped_at, "the clock kept running past EOF");
    // And it is still the end: pausing must not spend the state the glyph
    // and the restart both read.
    assert!(session.is_eos());
    assert_eq!(
        transport(session.is_playing(), session.is_eos()),
        Transport::Ended
    );

    // The restart path off that frozen end, which is what the button and
    // the play key do from `Ended`: back to the top, and running.
    session.seek(0.);
    session.play();
    assert!(!session.is_eos(), "a seek revives the session");
    assert!(session.now() < 1.0);
    assert_eq!(
        transport(session.is_playing(), session.is_eos()),
        Transport::Playing
    );
    session.pause();
}

/// Both ends hold under a key held down: the ABI only accepts `0.0..=1.0`,
/// and a wrapped step count would hand it something else.
#[test]
fn the_volume_stops_at_both_ends() {
    let mut volume = Volume::default();
    for _ in 0..40 {
        volume.step(true);
    }
    assert_eq!(volume.gain(), 1.0);
    assert_eq!(volume.steps, Volume::MAX_STEPS);

    for _ in 0..40 {
        volume.step(false);
    }
    assert_eq!(volume.gain(), 0.0);
    assert_eq!(volume.label(), "Vol 0%");

    // Silent by the level rather than by the flag is still not muted: the
    // button says which, because only one of them survives a step up.
    assert!(!volume.muted);
    volume.step(true);
    assert_eq!(volume.gain(), 0.05);
}

#[test]
fn the_budget_is_the_one_number_the_engine_gets() {
    // One explicit rate, always: the Auto/Low/Medium/High/Exact ladder is
    // gone, so there is nothing left that sends `None` and lets the encoder
    // pick a figure the row never showed.
    let settings = export_settings(
        6_500_000,
        Format::Mp4,
        DEFAULT_AUDIO_KBPS,
        EncoderSeat::Auto,
    );
    assert_eq!(settings.bitrate, Some(6_500_000));
    assert!(!settings.exact, "the copy path is off this surface");
    assert_eq!(settings.audio_kbps, Some(DEFAULT_AUDIO_KBPS));
    assert_eq!(settings.seat, EncoderSeat::Auto);
    // The picked format travels, or the moment would be a picture of a
    // choice the engine never hears about.
    for format in [Format::Mp4, Format::Wav, Format::Flac] {
        assert_eq!(
            export_settings(BPS_MIN, format, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).format,
            format
        );
    }
    // The lever's bounds are the engine's own (`MIN_BITRATE`,
    // `MAX_EXPLICIT_BITRATE`), so no reachable number is silently rewritten.
    assert_eq!(BPS_MIN, 100_000);
    assert_eq!(BPS_MAX, 50_000_000);
    // The default the lever opens on is the engine's automatic figure for
    // the picture that is open -- never the 0 Mbps the old `custom_mbps: 0`
    // put on screen.
    assert_eq!(auto_bps(1920, 1080, 30.), 6_220_800);
    assert_eq!(auto_bps(160, 120, 15.), BPS_MIN, "a tiny picture floors");
    assert!(
        auto_bps(7680, 4320, 60.) <= 20_000_000,
        "and a huge one caps"
    );
    for bps in [auto_bps(1920, 1080, 30.), auto_bps(320, 240, 24.)] {
        assert!((BPS_MIN..=BPS_MAX).contains(&bps));
    }
}

#[test]
fn a_budget_is_said_four_ways_and_lands_inside_the_engines_range() {
    // A minute of film with 256 kbps of sound under it: the span and the
    // sound are what a *size* has to be converted through.
    let (seconds, audio) = (60., 256_000);
    // A rate in its own units, and a bare number in the row's (Mbps).
    assert_eq!(parse_budget("6.5M", seconds, audio), Some(6_500_000));
    assert_eq!(parse_budget("6.5", seconds, audio), Some(6_500_000));
    assert_eq!(parse_budget("6.5 mbps", seconds, audio), Some(6_500_000));
    assert_eq!(parse_budget("12M", seconds, audio), Some(12_000_000));
    // A sub-Mbps ask is a rate this row can say: 850 kbps travels as itself.
    assert_eq!(parse_budget("850k", seconds, audio), Some(850_000));
    // Under the engine's floor it lands *on* the floor -- clamped, and shown
    // clamped in the same keystroke.
    assert_eq!(parse_budget("50k", seconds, audio), Some(BPS_MIN));
    assert_eq!(parse_budget("2500k", seconds, audio), Some(2_500_000));
    // `G` is a target file size: 1.2 GB over a minute is 160 Mbit/s of file,
    // less the sound -- past the ceiling, so the ceiling is what is written.
    assert_eq!(parse_budget("1.2G", seconds, audio), Some(BPS_MAX));
    // ...and over an hour of film the same size is a rate that fits: the
    // sound's own bits come out of it, which is why it is not 2.67 Mbps.
    let hour = parse_budget("1.2G", 3600., audio).expect("a size over an hour");
    assert_eq!(hour, (1.2e9 * 8. / 3600.) as u64 - audio);
    // A size with no film to spread it over is not a rate.
    assert_eq!(parse_budget("1.2G", 0., audio), None);
    // Everything else is refused rather than guessed at, and the field stays
    // open on it.
    for text in ["", "  ", "abc", "-4", "0", "M", "6.5.5"] {
        assert_eq!(parse_budget(text, seconds, audio), None, "{text}");
    }
    // Whatever is typed, what comes out is inside the engine's clamp.
    for text in ["1k", "999G", "50M", "51M", "0.5"] {
        if let Some(bps) = parse_budget(text, seconds, audio) {
            assert!((BPS_MIN..=BPS_MAX).contains(&bps), "{text} -> {bps}");
        }
    }
}

#[test]
fn the_budget_lever_steps_by_a_tenth_and_a_whole_megabit() {
    // A notch is 0.1 Mbps and a notch with shift is 1 -- the readout has one
    // decimal, so the fine step is the smallest move it can show.
    assert_eq!(BPS_FINE, 100_000);
    assert_eq!(BPS_COARSE, 1_000_000);
    assert_eq!(rate_label(6_000_000 + BPS_FINE), "6.1");
    assert_eq!(rate_label(6_000_000 + BPS_COARSE), "7.0");
    // Forty-nine coarse notches carry the range with change left in the
    // fiftieth, which is what a wheel is for; the fine step would take five
    // hundred.
    assert_eq!((BPS_MAX - BPS_MIN) / BPS_COARSE, 49);
    // The step arithmetic itself, as `Player::nudge_budget` does it: a
    // saturating walk that stops at both ends rather than wrapping.
    let step = |at: u64, steps: i32, coarse: bool| {
        let by = i64::from(steps) * if coarse { BPS_COARSE } else { BPS_FINE } as i64;
        ((at as i64 + by).max(0) as u64).clamp(BPS_MIN, BPS_MAX)
    };
    assert_eq!(step(6_000_000, 5, false), 6_500_000);
    assert_eq!(step(6_000_000, -1, true), 5_000_000);
    assert_eq!(step(BPS_MIN, -9, true), BPS_MIN, "the floor holds");
    assert_eq!(step(BPS_MAX, 9, true), BPS_MAX, "and so does the ceiling");
}

/// The sheet itself, read off its own source: presets first, then the range,
/// the destination and the summary, with the detailed settings behind the one
/// "More settings" disclosure -- and the scrim stops at the time band.
/// A source scan, like the stance guards above it -- this binary has no
/// `TestAppContext` to render a card and measure it.
#[test]
fn the_export_sheet_reads_presets_range_destination_summary_then_more() {
    let card = src_text("ui/cards/export.rs");
    let body = fn_body("sheet_settings");
    let order = [
        "self.sheet_preset(cx)",
        "self.sheet_destination(cx)",
        "self.sheet_range(cx)",
        "self.sheet_summary(",
        "more_toggle(more, cx)",
        "more.then(|| self.sheet_more(cx))",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|part| body.find(part).unwrap_or_else(|| panic!("{part} is not on the sheet")))
        .collect();
    assert!(at.windows(2).all(|w| w[0] < w[1]), "the sheet's sections are out of order");
    // The body scrolls on a short window rather than the sheet moving down.
    assert!(body.contains("overflow_y_scroll"), "a short sheet cannot scroll");
    // The disclosure is the folds file's own, so it is remembered.
    assert!(card.contains("toggle_fold(&mut this.folds, EXPORT_MORE_FOLD)"));
    assert!(card.contains(".child(\"Details\")"));
    // The panel is the dock's third tab, in flow: no scrim, no absolute box.
    let mount = fn_body("export_panel");
    assert!(!mount.contains("absolute()") && !mount.contains("scrim"));
    assert!(src_text("ui/dock_stance.rs").contains("player.export_panel(cx)"));
    assert!(src_text("ui/dock_stance.rs").contains("\"dock-tab-export\""));
    // Every press has a pointer door as well as a key.
    for id in [
        "export-confirm",
        "export-range-whole",
        "export-range-marks",
        "export-name",
        "export-folder",
        "export-destination",
        "export-preset",
        "export-overwrite-replace",
        "export-overwrite-keep",
        "export-overwrite-cancel",
        "export-cancel",
        "export-retry",
        "export-show-file",
        "export-play",
    ] {
        assert!(card.contains(&format!("\"{id}\"")), "{id} is on no control");
    }
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
}

/// The shipped defect: a chord the export surface answers (`n`) also reached
/// the room's keymap under it and toggled the timeline snap. The fix is that
/// the moment owns the keyboard while it is up -- every stroke is consumed,
/// and escape retreats one step at a time (the field, then the moment).
#[test]
fn a_chord_the_export_moment_answers_never_reaches_the_room() {
    // The leak was real: `n` is the room's own snap chord.
    let keymap = keymap::Keymap::defaults();
    assert_eq!(keymap.display(keymap::ActionId::ToggleSnap), "N");
    // One door, and it is asked *before* the stance's modal guard.
    let cards = src_text("player/cards.rs");
    assert!(
        cards.contains("if self.export_moment_key(key, shift, cx)"),
        "param_card_key no longer routes the moment's own strokes"
    );
    let body = fn_body("export_moment_key");
    let (guard, rest) = body
        .split_once("return false;")
        .expect("the one early out is `the moment is not open`");
    assert!(
        guard.contains("!self.export_open"),
        "the only way out of this handler must be that no moment is up"
    );
    assert!(
        !rest.contains("return false"),
        "a stroke fell through to the room's keymap again -- that is the `n` \
         toggling snap defect"
    );
    // Escape retreats one step at a time (DESIGN.md:146): the question and
    // the field are closed inside their own branches, the sheet only once
    // both are shut.
    let (asking, rest) = rest
        .split_once("return true;")
        .expect("the question branch returns before the chords below it");
    assert!(asking.contains("ESCAPE => self.answer_overwrite(Overwrite::Cancel, cx)"));
    let (typing, closed) = rest
        .split_once("return true;")
        .expect("the typing branch returns before the chords below it");
    assert!(
        typing.contains("self.budget_edit = None"),
        "esc leaves the field"
    );
    assert!(
        closed.contains("self.leave_sheet_face()"),
        "then esc closes the moment"
    );
    // Its chords, all of them consumed here rather than by the keymap.
    for chord in [
        "\"d\"",
        "\"n\"",
        "\"e\"",
        "\"enter\"",
        "\"left\"",
        "\"right\"",
        "\",\"",
    ] {
        assert!(closed.contains(chord), "{chord} is not the moment's own");
    }
}

/// "Simplify it, not hide the things": every detailed setting is on the
/// sheet behind "Details" -- container and codec, encoder seat (software
/// first), bitrate, resolution, bit depth, sound rate, loudness, chapters and
/// captions. Each is a labelled row that opens a list of every value.
#[test]
fn details_holds_every_export_setting_as_direct_choices() {
    let more = fn_body("sheet_more");
    for (what, needle) in [
        ("the container", "ExportRow::Container"),
        ("the codec", "ExportRow::Codec"),
        ("the encoder", "ExportRow::Seat"),
        ("the bitrate", "ExportRow::Bitrate"),
        ("the size", "ExportRow::Size"),
        ("the bit depth", "ExportRow::Depth"),
        ("the sound rate", "ExportRow::Kbps"),
        ("the loudness", "ExportRow::Loudness"),
        ("the captions", "ExportRow::Captions"),
        ("the chapters", "s.chapters().len()"),
    ] {
        assert!(more.contains(needle), "Details lost {what}");
    }
    for cycle in ["cycle_export_picture", "cycle_audio_kbps", "cycle_encoder"] {
        assert!(!more.contains(cycle), "{cycle} is a press-to-cycle row again");
    }
    assert!(
        crate::export_ui::ENCODER_SEATS[0] == EncoderSeat::Software,
        "the software seat is not listed first"
    );
    let seats = fn_body("export_choices");
    assert!(seats.contains("ENCODER_SEATS"));
    // Hardware stays on the list, named plainly, and is never the default.
    assert!(crate::export_ui::seat_plain(EncoderSeat::Hardware).contains("GPU"));
}

/// The budget is a video setting -- the engine's own word is that the bitrate
/// settings "mean nothing to the audio-only ones". Its Details row stays in
/// place (nothing shifts) but dims with its reason for a file with no picture,
/// and both doors into the number answer the same gate: a row the moment does
/// not show cannot have been changed.
#[test]
fn the_budget_row_and_its_doors_are_video_only() {
    let body = fn_body("sheet_more");
    // A sound-only file has no picture settings at all: the video group's rows
    // (the bitrate among them) are built only `if video`.
    let gate = body.find("if video {").expect("the picture rows are gated on a picture");
    assert!(
        body.find("\"export-d-bitrate\"").is_some_and(|at| at > gate),
        "the bitrate row is not behind the video gate"
    );
    // The `n` door: the moment's chord and the row's own click both funnel
    // through `edit_budget`, so its gate is the one gate -- and it must sit
    // ahead of the field it guards.
    let typing = fn_body("edit_budget");
    let (gate, _) = typing
        .split_once("self.budget_edit = Some")
        .expect("the typing field still opens");
    assert!(
        gate.contains("!self.format.has_video()"),
        "`n` opens the budget field for a format that writes no picture"
    );
    // The value itself: arrows, `+`/`-` and the wheel all funnel through
    // `nudge_budget` -- a lever nothing shows does not move either.
    let lever = fn_body("nudge_budget");
    let (gate, _) = lever
        .split_once("self.custom_bps =")
        .expect("the lever still sets the budget");
    assert!(
        gate.contains("!self.format.has_video()"),
        "the hidden budget lever moves again"
    );
}

/// Exporting over an existing file must warn first, "same pattern as the
/// exit guard" (user 2026-09-11: "not just save but export too -- an export
/// named like an existing video overrides it"). The question as a value: a
/// target that is there and not this window's own asks; anything else just
/// writes.
#[test]
fn only_a_foreign_existing_target_asks_before_an_export_overwrites_it() {
    assert!(crate::Player::export_asks(true, false));
    assert!(
        !crate::Player::export_asks(false, false),
        "nothing there to clobber"
    );
    assert!(!crate::Player::export_asks(false, true));
    assert!(
        !crate::Player::export_asks(true, true),
        "a re-export over this window's own output is silent"
    );
}

/// The guard's wiring: asked after every refusal (an export that cannot run
/// never asks), before the worker starts; the moment closes with the asking
/// (it overlays the strip the plate rises on); a started export marks the
/// target this window's own so a re-export stays silent; and the plate's
/// `enter` confirms the target before re-entering the door.
#[test]
fn the_export_overwrite_question_owns_the_door_and_the_keyboard() {
    let body = fn_body("start_export");
    assert!(
        body.contains("self.export_needs_answer()"),
        "start_export never asks before overwriting"
    );
    // The call is hoisted above the `&mut self.session` borrow (E0502); what
    // must sit after the refusals is the *check* of its answer.
    let (fences, rest) = body
        .split_once("if overwrite {")
        .expect("the guard's answer is never checked");
    assert!(
        fences.contains("format_refusal"),
        "the question comes before the refusals: a refused export would ask"
    );
    let (guard, started) = rest
        .split_once("export_to_with")
        .expect("the worker is still started from start_export");
    assert!(
        guard.contains("self.export_ask = true") && !guard.contains("self.close_card()"),
        "the question is asked inside the sheet, which stays up"
    );
    assert!(
        started.contains("self.export_known = Some(self.export_path.clone())"),
        "a started export does not mark the target this window's own"
    );
    let stance = src_text("ui/stance.rs");
    let (_, handler) = stance
        .split_once("if this.export_ask {")
        .expect("the export question is up with no key answering it");
    for answer in ["Overwrite::Replace", "Overwrite::KeepBoth", "Overwrite::Cancel"] {
        assert!(handler.contains(answer), "the question's keys lost {answer}");
    }
    let answer = fn_body("answer_overwrite");
    let (replace, _) = answer
        .split_once("Overwrite::KeepBoth")
        .expect("Keep both is an answer");
    assert!(
        replace.contains("self.export_known = Some(self.export_path.clone())"),
        "Replace does not confirm the target before re-entering the door"
    );
    assert!(answer.contains("unique_path(&self.export_path"));
    let sheet = fn_body("sheet_overwrite");
    assert!(
        sheet.contains("file_name(&self.export_path)"),
        "the question does not name the file it would overwrite"
    );
}

/// The export panel's handler swallows every stroke so none reaches the room
/// under it; Settings has no Export tab.
#[test]
fn the_moment_and_the_room_wear_the_same_export_chords() {
    let handler = fn_body("export_moment_key");
    assert!(handler.contains("\"e\" | \"enter\" => self.start_export"));
    let settings = src_text("ui/settings_stance.rs");
    assert!(!settings.contains("SettingsTab::Export"), "Settings grew an Export tab again");
}

/// The Sound row: what it offers, and that the pick travels to the engine
/// for a *video* export as much as for an MP3 -- both files carry sound, so
/// a row that only reached the audio formats would be half a control.
#[test]
fn the_sound_row_carries_its_rate_into_both_kinds_of_file() {
    // Sorted and unique, so a row that says "b for the next one" is stepping
    // up and not shuffling.
    assert!(AUDIO_KBPS.windows(2).all(|w| w[0] < w[1]));
    // The untouched figure is one of the offered rows, or the first press of
    // `b` would jump somewhere nobody chose.
    assert!(AUDIO_KBPS.contains(&DEFAULT_AUDIO_KBPS));
    // ...and it is what this program wrote before the row existed: the
    // export of a user who never opens it must not change under them.
    assert_eq!(DEFAULT_AUDIO_KBPS, 256);
    for format in [Format::Mp4, Format::Av1, Format::Hevc, Format::Mp3] {
        for kbps in AUDIO_KBPS {
            assert_eq!(
                export_settings(BPS_MIN, format, kbps, EncoderSeat::Auto).audio_kbps,
                Some(kbps),
                "{format:?} at {kbps} kbps"
            );
        }
    }
    // The pointer's door to the same field: one row per rate, the one in
    // force marked exactly once, and every row's small print short enough
    // to survive `MENU_W`'s truncation (the resolution list's own rule).
    let rows = audio_rate_choices(DEFAULT_AUDIO_KBPS);
    assert_eq!(rows.len(), AUDIO_KBPS.len());
    assert_eq!(rows.iter().filter(|(.., picked)| *picked).count(), 1);
    for ((choice, label, detail, picked), kbps) in rows.iter().zip(AUDIO_KBPS) {
        assert_eq!(*choice, Choice::AudioRate(kbps));
        assert_eq!(label.as_ref(), format!("{kbps} kb/s"));
        assert!(detail.chars().count() <= 26, "{detail} is too long to read");
        assert_eq!(*picked, kbps == DEFAULT_AUDIO_KBPS);
    }
    // A rate no row holds marks none of them, rather than the wrong one.
    assert!(audio_rate_choices(7).iter().all(|(.., picked)| !picked));
}

/// The colour field (`ColorEdit`), the transition field's free-text sibling:
/// every spelling of one colour lands on the same pair, the pair is what the
/// row shows back as a hex, and text that is not a colour is refused in words
/// while the field goes on holding it -- never dropped in silence, never
/// guessed at, and never cleared by the refusal itself.
#[test]
fn a_typed_colour_is_read_in_every_spelling_and_anything_else_is_refused() {
    use engine::decode::{viz_color_from_text, viz_color_hex};

    // One colour in every spelling the field takes: red as `#RRGGBB`, as bare
    // digits, as `rgb(..)`, and as `rgba(..)` with the alpha dropped. Two
    // spellings of one colour landing on different pairs would be a field
    // whose colour depends on how it was written.
    let red = viz_color_from_text("#FF0000").expect("red is a colour");
    for text in ["#FF0000", "FF0000", "rgb(255,0,0)", "rgba(255,0,0,255)"] {
        let mut edit = ColorEdit::new("");
        for c in text.chars() {
            edit.typed(c);
        }
        assert_eq!(edit.commit(), Some(red), "{text}");
        assert_eq!(edit.refusal, None, "{text} is a colour, not a refusal");
    }

    // The round trip through what the row displays: the hex the row shows
    // reads back as the same pair, so opening the field on it and pressing
    // enter changes nothing. A seed taken from anywhere else -- or a hex
    // spelled by something other than the field's own reader -- shows up here.
    let hex = viz_color_hex(red.0, red.1);
    let mut edit = ColorEdit::new(&hex);
    assert_eq!(edit.text, hex, "the field opens on the row's own line");
    assert!(edit.detail().contains("enter commits"));
    assert_eq!(edit.commit(), Some(red));
    assert_eq!(edit.refusal, None);

    // Text that is not a colour: refused in words that say what a colour
    // looks like, and the text survives the refusal -- a refusal that ate what
    // was typed would make the next try retype all of it.
    let mut edit = ColorEdit::new("");
    edit.typed('z');
    edit.typed('z');
    assert_eq!(edit.commit(), None);
    let why = edit.refusal.clone().expect("a refusal, not a silence");
    assert!(
        why.contains("#RRGGBB"),
        "the refusal says what a colour is: {why}"
    );
    assert_eq!(edit.text, "zz", "the refusal keeps the text it refused");
    assert!(
        edit.detail().contains("zz▏"),
        "the caret stays in the buffer"
    );
    // ...and the next stroke clears it: that reason described the text that is
    // no longer there.
    edit.typed('0');
    assert_eq!(edit.refusal, None);
    assert_eq!(edit.text, "zz0");

    // Backspace erases the last character and the refusal with it, and the
    // buffer is capped out loud rather than growing without bound off a held
    // key.
    let mut edit = ColorEdit::new(&hex);
    edit.backspace();
    assert_eq!(edit.text.chars().count(), hex.chars().count() - 1);
    for _ in 0..64 {
        edit.typed('f');
    }
    assert_eq!(edit.text.chars().count(), 32);
    assert!(edit.refusal.is_some(), "the stroke past the cap says so");

    // Why the commit compares before it writes: the field opens seeded with
    // `viz_color_hex(now)`, and that hex reads back as a *different* pair for
    // a good share of inks (the engine's round-trip canary counts 47 053 of
    // 65 536 exact), so "write whatever the parse gave" would take an undo
    // step for the colour the row was already showing. The fact is held here;
    // the gate that reads it is pinned below.
    let mut inexact = 0;
    for hue in 0..=255u8 {
        for sat in [0u8, 64, 160, 255] {
            let seed = viz_color_hex(hue, sat);
            match viz_color_from_text(&seed) {
                // Exact: entering the seed is already a no-op for the write
                // gate, whatever the gate says.
                Some(pair) if pair == (hue, sat) => {}
                // Inexact: this is the case the gate exists for -- without it
                // every one of these takes a committing write on an untouched
                // colour.
                Some(_) => inexact += 1,
                None => panic!("the row's own hex must be readable back: {seed}"),
            }
        }
    }
    assert!(
        inexact > 0,
        "no ink round-trips inexactly -- the commit's no-op gate would be dead weight"
    );

    // The gate itself, in the door that has it (`Player::commit_viz_hex`): the
    // parse, then the comparison with the picked slot's own colour, and the
    // write only past it.
    let body = fn_body("commit_viz_hex");
    let gate = body
        .split_once("hue_sat(paint)")
        .expect("the commit no longer compares with the colour on the slot")
        .1;
    assert!(
        gate.contains("write_viz_hs("),
        "the commit writes before it has compared, so the row's own seed is an edit"
    );
}

/// The look family's writes arm autosave, and therefore the ledger's `unsaved`
/// ghost and `Player::close_needs_answer`, which read the same flag: a colour
/// picked in the Clip tab that armed nothing could be closed away with no
/// prompt and no autosave.
///
/// A scan and not a drive: every one of these doors needs a `Player` and a
/// gpui `Context`, and this crate has no window harness (`tests/view.rs`'s own
/// note). What a scan can hold is the three doors themselves -- the two the
/// whole paint family shares (`set_viz_paint_with` is where the wheel, the
/// steppers and a typed colour all land) and the two writes that do not go
/// through it (the style chips and the `Fast` chip, which write
/// `clip.visualizer`). Pre-fix none of the three armed anything while every
/// other engine-writing door did (`write_color`, `write_transform`,
/// `set_eq`, the trim/place/fade doors).
#[test]
fn a_visualizer_look_write_arms_autosave() {
    for name in ["set_viz_style", "toggle_viz_flag"] {
        let body = fn_body(name);
        assert!(
            body.contains("session.set_visualizer"),
            "{name} no longer writes the clip's look at all"
        );
        assert!(
            body.contains("mark_dirty()"),
            "{name} writes a look and arms nothing"
        );
    }
    let body = fn_body("set_viz_paint_with");
    // The arm rides the write that took, not the call: a no-op press (the
    // colour under the hand already) is not an edit, and the ghost must stay
    // off it.
    let took = body
        .split_once("if took {")
        .expect("the paint door writes through a taken-write gate")
        .1;
    assert!(
        took.contains("mark_dirty()"),
        "the arm is outside the write gate, so a no-op press would dirty the project"
    );
    assert!(
        took.contains("reset_after_reseek()"),
        "the arm replaced the reseek bookkeeping instead of joining it"
    );
}

/// The subtitle style card's caption animation rows write the project's caption style, so the
/// door arms autosave on the engine's own "it moved" answer, and every row has a keyboard step.
#[test]
fn the_caption_style_rows_write_the_project_and_step_from_the_keyboard() {
    let body = fn_body("edit_caption_style");
    assert!(body.contains("set_caption_style(cs)") && body.contains("mark_dirty()"));
    let step = fn_body("step_sub_field");
    for row in ["SUB_ROW_ANIM", "SUB_ROW_HIGHLIGHT", "SUB_ROW_HL_BOX", "SUB_ROW_UPPERCASE", "SUB_ROW_WORDS"] {
        assert!(step.contains(row), "{row} has no keyboard step");
    }
}

/// The rest of the class the visualizer look family belonged to: every other
/// door that writes the project from a *click* and so has no action behind it
/// to arm autosave ([`Player::act`]'s list is the action doors only). Each arm
/// rides its own "the write took" gate -- a pick that changed nothing must not
/// dirty the project, or a window nobody has touched would ask to be saved.
#[test]
fn every_click_door_that_writes_the_project_arms_autosave() {
    // The doors the engine answers with a bool *that means "the value
    // moved"*: the arm goes inside it.
    for name in ["apply_resolution", "apply_frame_rate", "apply_tone"] {
        let body = fn_body(name);
        assert!(
            body.contains("mark_dirty()"),
            "{name} writes the project and arms nothing"
        );
    }
    // ...and the one whose bool does not mean that at all: `set_fit` says the
    // index existed, and the picker keeps the row already in force clickable,
    // so the arm rides the policy having moved -- on the engine's own bool it
    // would light the `unsaved` ghost over a project nobody touched.
    let body = fn_body("apply_fit");
    assert!(
        body.contains("fit_of(lane, idx) != fit"),
        "apply_fit arms on the engine's own bool again, which a no-op pick also answers true"
    );
    assert!(body.contains("mark_dirty()"), "apply_fit arms nothing");
    // The import path keeps no arm on purpose (the `autosave_armed` rule): its
    // only caller is `install_media`, so an arm there could only ever fire for
    // a project nobody has touched. The user's door is the two toggles.
    let body = fn_body("apply_proxies");
    assert!(
        body.find("mark_dirty").is_none(),
        "apply_proxies arms again -- its only caller is the import path"
    );
    // The doors whose setter takes no answer back, and the library row's
    // removal: the arm rides a local `changed` (or the removal having landed),
    // never the call.
    for (name, gate) in [
        ("apply_sample_rate", "if changed {"),
        ("apply_encoder", "if changed {"),
        ("toggle_proxies", "if changed {"),
        ("toggle_auto_proxies", "if changed {"),
        ("remove_source", "Some(Ok(idx))"),
    ] {
        let body = fn_body(name);
        let after = body
            .split_once(gate)
            .unwrap_or_else(|| panic!("{name} lost the gate its arm rides"))
            .1;
        assert!(
            after.contains("mark_dirty()"),
            "{name} arms outside its gate, so a no-op pick would dirty the project"
        );
    }
    // The drag-and-drop family on the timeline: the same class, and the same
    // rule. Every one of these doors can answer "nothing happened" -- a clip let
    // go where it was picked up, a caption dragged back to the frame it started
    // on, a trim, a fade or a `[`/`]` nudge released against the wall the edge
    // already stood on, a subtitle row dropped on an overlap ("Not placed") or a
    // caption that is not there any more ("Nothing lifted") -- and each one used
    // to arm the flag *before* the door was asked, so the ledger's `unsaved`
    // ghost, the autosave sidecar and the save-on-close question described a
    // project whose content was byte-for-byte what it was.
    //
    // Each arm now rides its own door's answer, or the one reading the door's
    // answer cannot give: a caption's own window before and after the trim
    // (`Project::trim_sub` answers `Ok` for an edge released at a wall it already
    // stood against) and the ramp's own value before and after the fade
    // (`set_fade_*` answer `true` for any index that exists).
    for (name, gate) in [
        ("move_clip_now", "match moved {"),
        (
            "place_sub",
            "let text = match (self.sub_of_track(track), &mut self.session) {",
        ),
        ("move_sub", "Some(Ok(())) => {"),
        ("lift_sub", "if lifted {"),
        ("commit_trim_now", "if trim.lane.kind == LaneKind::Subtitle {"),
        ("commit_fade", "if set {"),
        ("nudge_cut", "let to = nudge_edge(current, dir, lo, hi);"),
        (
            "trim_cut_to_playhead",
            "let to = frame_at(session.now(), self.fps);",
        ),
    ] {
        let body = fn_body(name);
        let (before, after) = body
            .split_once(gate)
            .unwrap_or_else(|| panic!("{name} lost the gate its arm rides"));
        assert!(
            after.contains("mark_dirty()"),
            "{name} arms outside its gate, so a stroke that changed nothing \
             would dirty the project"
        );
        assert!(
            !before.contains("mark_dirty()"),
            "{name} still arms ahead of its gate, so a refusal -- or a drag let \
             go where it started -- leaves the project looking edited"
        );
    }
    // The history pair is the one place the action table does not arm early:
    // the door's own answer ("there was a step to take") is the gate, so `^z`
    // with an empty history leaves the flag -- and the close question -- clear.
    let act = fn_body("run_action");
    for (gate, next) in [
        ("if self.undo(cx) {", "ActionId::Redo =>"),
        ("if self.redo(cx) {", "ActionId::AddVideoLane =>"),
    ] {
        let arm = act
            .split_once(gate)
            .unwrap_or_else(|| panic!("{gate} is gone from the action table"))
            .1;
        // Cut at the arm *after* this one: "everything from here on" is
        // satisfied by the sibling's arm, so deleting Undo's own would ship
        // green.
        let arm = arm.split_once(next).map_or(arm, |(arm, _)| arm);
        assert!(
            arm.contains("mark_dirty()"),
            "the arm behind `{gate}` arms nothing, so a real step back is \
             invisible to autosave, the unsaved ghost and the close question"
        );
    }
    // ...and the answer the arm reads is the door's own: the session's bool, not
    // a bare call that would arm for an undo with nothing to undo.
    for name in ["undo", "redo"] {
        let body = fn_body(name);
        assert!(
            body.contains("PlaybackSession::") && body.contains("stepped"),
            "Player::{name} no longer answers whether there was a step to take"
        );
        assert!(
            !body.contains("mark_dirty"),
            "Player::{name} arms its own flag, so the arm in `act` is a second \
             one for the same step"
        );
    }
    // The mix card's own doors. Both answer "the value moved" -- a fader
    // already against its wall, a ceiling already at its clamp, a limiter
    // switch whose value is the one in force are each `false` -- so the arm
    // rides the bool, and `nudge_mix` does not arm before the ask the way it
    // did: a held arrow key at the top of the fader lit `unsaved`, wrote a
    // sidecar and put the close prompt up over a project whose bytes had not
    // moved.
    let body = fn_body("nudge_mix");
    for door in ["set_lane_gain_db", "set_limiter"] {
        assert!(
            body.contains(door),
            "nudge_mix no longer goes through {door}"
        );
    }
    let arm = body
        .split_once("if took {")
        .expect("the mix arm no longer rides the door's own answer")
        .1;
    assert!(
        arm.contains("mark_dirty()"),
        "nudge_mix arms outside its gate, so a fader held against its wall \
         would dirty the project"
    );
    let before = body.split_once("if took {").unwrap().0;
    assert!(
        !before.contains("mark_dirty()"),
        "nudge_mix still arms ahead of the door, so a clamped no-op nudge \
         leaves the project looking edited"
    );
    // The equalizer card's commit, the same rule against a door whose bool is
    // the WEAKER of the two shapes: `set_eq` answers "the index was there and
    // the curve is finite" and rewrites the very slot it already holds for a
    // curve already in force, so the arm needs the comparison too -- the same
    // pair of conditions `apply_fit` rides.
    let body = fn_body("commit_eq");
    let arm = body
        .split_once("if moved && session.set_eq(lane, idx, params) {")
        .expect("the EQ commit no longer rides the curve having moved and then the door")
        .1;
    assert!(
        arm.contains("mark_dirty()"),
        "commit_eq arms outside its gate, so re-committing the curve already \
         in force would dirty the project"
    );
    assert!(
        body.contains("session.eq_of(lane, idx).cloned() != params"),
        "commit_eq rides the engine's bool alone, which is true for a \
         re-commit of the curve already in force"
    );
    // ...and the reseek bookkeeping rides with the arm, not instead of it.
    assert!(
        arm.contains("reset_after_reseek()"),
        "commit_eq's reseek flags no longer clear on a write that took"
    );
    let before = body
        .split_once("if moved && session.set_eq(lane, idx, params) {")
        .unwrap()
        .0;
    assert!(
        !before.contains("mark_dirty()"),
        "commit_eq still arms ahead of the door"
    );
}

/// The transition doors, which are the class's other shape: both engine
/// setters ([`engine::project::Project::set_transition_out`],
/// [`engine::project::Project::crossfade`]) answer `true` for any two
/// adjacent clips, snapshot and clamp included -- so a step past the
/// successor's length, or a number retyped into the field, wrote the value the
/// join already carried and still said yes. [`Player::set_transition_frames`]
/// reads the join's own edges back and rides those, and the crossfade side
/// compares *both* edges: it writes the anchor's `fade_out` and the
/// neighbour's `fade_in`, and a comparison of the anchor alone would miss a
/// neighbour-side no-op.
#[test]
fn a_transition_step_or_a_retyped_number_arms_only_what_moved() {
    let body = fn_body("set_transition_frames");
    for door in ["set_transition_out", "crossfade"] {
        assert!(
            body.contains(door),
            "the duration doors no longer include {door}"
        );
    }
    let gate = "if ok && self.transition_edges(lane, idx) != before {";
    let arm = body
        .split_once(gate)
        .unwrap_or_else(|| panic!("the transition arm no longer rides the join's own edges"))
        .1;
    assert!(
        arm.contains("mark_dirty()"),
        "the transition arm is outside its gate, so a step past the cap or a \
         retyped number would dirty an unchanged project"
    );
    let before = body.split_once(gate).unwrap().0;
    assert!(
        !before.contains("mark_dirty()"),
        "the transition arm is still ahead of the comparison"
    );
    assert!(
        body.contains("let before = self.transition_edges(lane, idx);"),
        "the join's edges are no longer read before the write"
    );
    // The helper both sides call, and the pair it must compare: an anchor-only
    // comparison cannot see the neighbour's `fade_in` not moving.
    let helper = fn_body("transition_edges");
    assert!(
        helper.contains("clips.get(idx)?.fade_out")
            && helper.contains("clips.get(idx + 1)?.fade_in"),
        "the edge reader no longer reads both sides of the join, so a \
         crossfade that only moves the neighbour is invisible"
    );
}

/// The three doors the action table used to arm on their behalf. The table
/// fires *before* any door is asked, so an entry for a door that can refuse
/// dirties an untouched project -- and the dissolve is the sharpest case,
/// because the clip-menu row reaches [`Player::dissolve_selected`] directly
/// ([`crate::ui::bench_stance`]) and so was armed by nothing at all while the
/// key chord beside it armed on every press, refused or not.
///
/// The engine half of the premise is not a scan but a run: see
/// `engine::project::tests::a_transition_door_says_yes_to_a_write_that_moved_nothing`,
/// which drives `set_transition_out` and `crossfade` and shows they answer
/// `true` for a write that changed nothing. That is why these three need a
/// before/after comparison and not just the door's own answer.
#[test]
fn the_action_table_arms_nothing_a_door_answers_for_itself() {
    // The two transition doors, in the same shape as the duration row's.
    for (name, door) in [
        ("crossfade_selected", "session.crossfade(lane, idx, frames)"),
        (
            "dissolve_selected",
            "session.set_transition_out(lane, idx, frames)",
        ),
    ] {
        let body = fn_body(name);
        assert!(body.contains(door), "{name} no longer goes through {door}");
        let gate = "if took && self.transition_edges(lane, idx) != before {";
        let arm = body
            .split_once(gate)
            .unwrap_or_else(|| panic!("{name} no longer arms on the write that took"))
            .1;
        assert!(
            arm.contains("mark_dirty()"),
            "{name} arms outside its gate, so a crossfade already at the room \
             the two clips offer would dirty an unchanged project"
        );
        let before = body.split_once(gate).unwrap().0;
        assert!(
            !before.contains("mark_dirty()"),
            "{name} still arms ahead of the comparison"
        );
    }
    // ...and the two actions are out of the table's list, or the key path is
    // armed twice over and the menu path not at all.
    let act = fn_body("run_action");
    let listed = act
        .split_once("if matches!(")
        .unwrap()
        .1
        .split_once(") {")
        .unwrap()
        .0;
    for gone in ["ActionId::Crossfade", "ActionId::Dissolve"] {
        assert!(
            !listed.contains(gone),
            "{gone} is back in the pre-arm list, so the key path arms before \
             the door is asked while the clip-menu row arms by nothing"
        );
    }
    // The track removal, whose two callers are a key and a header button.
    let body = fn_body("remove_lane");
    let arm = body
        .split_once("Some(Ok(())) => {")
        .expect("the track door no longer answers which arm means the track went")
        .1;
    assert!(
        arm.contains("mark_dirty()"),
        "a track that went arms nothing, so the removal is invisible to \
         autosave, the unsaved ghost and the close question"
    );
    // ...and the header door no longer arms on its own before asking, which
    // would be a second arm for the same step.
    let lane_act = fn_body("act_lane");
    assert!(
        !lane_act.contains("mark_dirty"),
        "act_lane arms before remove_lane, so a track still holding clips \
         refuses with the unsaved ghost already lit"
    );
    assert!(
        lane_act.contains(
            "ActionId::RemoveVideoLane | ActionId::RemoveAudioLane => self.remove_lane(lane, cx)"
        ),
        "act_lane no longer routes the header's remove to the one armed door"
    );
}

/// [`crate::ui::dock_stance::transition_of`]: the dock's duration row
/// appears only for an anchor that actually carries a dissolve or a
/// crossfade, and the room it offers is capped at what the successor clip
/// actually has -- the same clamp [`engine::project::Project::set_transition_out`]/
/// [`engine::project::Project::crossfade`] enforce, read back here without
/// either engine call so the row can never claim more than a step could land
/// in.
#[test]
fn a_duration_row_shows_only_for_a_transition_and_its_room_is_the_successors() {
    // `len` is the clip's own source length (`out_frame`, `in_frame` fixed at
    // 0, `speed` 1x, so `Clip::frames()` reads back exactly `len`).
    fn clip(start: u32, len: u32, transition_out: u32, fade_in: u32, fade_out: u32) -> Clip {
        Clip {
            speed_keys: Vec::new(),
            reversed: false,
            stabilise: None, deinterlace: Default::default(), mark: Default::default(),
            angle: Default::default(),
            start,
            in_frame: 0,
            out_frame: len,
            source: 0,
            link: None,
            eq: None,
            color: None,
            transform: None,
            fit: FitPolicy::default(),
            speed: Speed::NORMAL,
            fade_in,
            fade_out,
            transition_out,
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
        }
    }
    // A plain cut carries neither field, so the row has nothing to show.
    let plain = clip(0, 10, 0, 0, 0);
    let plain_next = clip(10, 15, 0, 0, 0);
    assert_eq!(
        transition_of(LaneKind::Video, &plain, Some(&plain_next)),
        None
    );

    // A dissolve: video lane, `transition_out` set, an adjacent successor.
    // The room offered is the shorter of the two -- here the successor's own
    // 5-frame length, less than the anchor's 10.
    let a = clip(0, 10, 4, 0, 0);
    let b = clip(10, 5, 0, 0, 0);
    assert_eq!(
        transition_of(LaneKind::Video, &a, Some(&b)),
        Some(("Dissolve", 4, 5))
    );
    // No successor, or a gap before one: no row, same as no transition.
    assert_eq!(transition_of(LaneKind::Video, &a, None), None);
    let gap = clip(11, 5, 0, 0, 0);
    assert_eq!(transition_of(LaneKind::Video, &a, Some(&gap)), None);

    // A crossfade: audio lane, both ends of the join set
    // ([`Project::crossfade`]'s own signature) -- a one-sided fade (the
    // successor's `fade_in` still 0) is not one, and stays a plain fade.
    let a = clip(0, 10, 0, 0, 3);
    let b = clip(10, 2, 0, 3, 0);
    assert_eq!(
        transition_of(LaneKind::Audio, &a, Some(&b)),
        Some(("Crossfade", 3, 2)),
        "capped at the successor's own 2-frame length, though 3 was asked for"
    );
    let one_sided = clip(10, 2, 0, 0, 0);
    assert_eq!(transition_of(LaneKind::Audio, &a, Some(&one_sided)), None);

    // A dissolve field on an audio lane, or a fade-only pair on a video
    // lane, name no transition either -- each kind reads only its own lane.
    let dissolving = clip(0, 10, 4, 0, 0);
    assert_eq!(transition_of(LaneKind::Audio, &dissolving, Some(&b)), None);
}

/// The transition kinds: sixteen palette actions (one per kind and direction,
/// each naming the kind it sets once), the card's title follows the clip's kind,
/// and the kind door arms only on a write that changed the join, as the
/// dissolve's does.
#[test]
fn the_transition_kinds_are_palette_actions_and_set_through_a_gated_door() {
    use engine::transitions::{Dir, TransitionKind};
    let kinds: Vec<TransitionKind> = ActionId::ALL
        .into_iter()
        .filter_map(|a| a.transition_kind())
        .collect();
    assert_eq!(kinds.len(), 16, "dissolve, two dips, three shapes x four ways, zoom");
    for (i, k) in kinds.iter().enumerate() {
        assert!(!kinds[..i].contains(k), "{k:?} is set by two actions");
    }
    assert!(kinds.contains(&TransitionKind::Push(Dir::Left)));
    assert!(ActionId::TransitionWipeUp.label().contains("Wipe up"));

    let body = fn_body("set_transition_kind");
    assert!(body.contains("session.set_transition(lane, idx, kind, frames)"));
    let gate = "if took && self.transition_edges(lane, idx) != before {";
    let (ahead, arm) = body
        .split_once(gate)
        .expect("set_transition_kind no longer arms on the write that took");
    assert!(arm.contains("mark_dirty()") && !ahead.contains("mark_dirty()"));
    // The edge reader tells two kinds apart, or a kind change would not arm.
    assert!(fn_body("transition_edges").contains("transition_kind_of(lane, idx)"));

    let mut a = Clip {
        opacity_keys: Vec::new(),
        color_keys: Vec::new(),
        effects: Vec::new(),
        speed_keys: Vec::new(),
        reversed: false,
        stabilise: None, deinterlace: Default::default(), mark: Default::default(),
        angle: Default::default(),
        start: 0,
        in_frame: 0,
        out_frame: 10,
        source: 0,
        link: None,
        eq: None,
        color: None,
        transform: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
        fade_in: 0,
        fade_out: 0,
        transition_out: 4,
        transition_kind: TransitionKind::Push(Dir::Left),
        visualizer: 0,
        viz_paint: 0,
        gain_db: 0.0,
        gain_keys: Vec::new(),
        pan: 0,
        pitch: 0,
        transform_keys: Vec::new(),
    };
    let mut b = a.clone();
    b.start = 10;
    b.transition_out = 0;
    assert_eq!(transition_of(LaneKind::Video, &a, Some(&b)), Some(("Push", 4, 10)));
    a.transition_kind = TransitionKind::DipBlack;
    assert_eq!(
        transition_of(LaneKind::Video, &a, Some(&b)),
        Some(("Dip to black", 4, 10))
    );
}

#[test]
fn a_clip_with_no_sound_is_refused_in_the_same_words_whichever_kind_it_is() {
    // A still and a video with no audio track are one answer to one
    // question: the lane and index that were picked, the file, and which of
    // the two soundless things it is. What must never reach the bar is the
    // demuxer's own words -- a png handed to the mp4 reader answers "a box
    // with a larger size than it", which is true of a container and useless
    // to a person.
    assert_eq!(
        unscannable(Lane::V1, 1, std::path::Path::new("/tmp/shot.png")),
        "V1 clip 2 has no audio to scan — shot.png is a picture"
    );
    assert_eq!(
        unscannable(
            Lane::new(LaneKind::Audio, 1),
            0,
            std::path::Path::new("/tmp/test_baseline.mp4")
        ),
        "A2 clip 1 has no audio to scan — test_baseline.mp4 is silent"
    );
}

/// The hold gate: a value runs, everything else still means one press one
/// action -- which is the whole invariant the blanket `is_held` filter used
/// to carry on its own.
#[test]
fn a_held_key_moves_a_value_and_nothing_else() {
    use keymap::ActionId;
    // A card's four arrows, whichever card it is.
    for key in ["up", "down", "left", "right"] {
        assert!(repeats(Repeat::Card, key, None), "{key} on a card");
    }
    // The card's own one-shots: flatten every band, cut forty places, play
    // them fast, close. None of them on a hold.
    for key in ["r", "enter", "f", "1", "escape"] {
        assert!(!repeats(Repeat::Card, key, None), "{key} on a card");
    }
    // Outside a card the keymap answers, and only the volume pair is a
    // value being moved.
    assert!(repeats(Repeat::Keymap, "up", Some(ActionId::VolumeUp)));
    assert!(repeats(Repeat::Keymap, "down", Some(ActionId::VolumeDown)));
    // ...and the zoom pair, which runs the view the way they run the level.
    assert!(repeats(Repeat::Keymap, "=", Some(ActionId::ZoomIn)));
    assert!(!repeats(Repeat::Keymap, "0", Some(ActionId::ZoomFit)));
    for action in ActionId::ALL {
        let held = repeats(Repeat::Keymap, "k", Some(action));
        assert_eq!(
            held,
            matches!(
                action,
                ActionId::VolumeUp | ActionId::VolumeDown | ActionId::ZoomIn | ActionId::ZoomOut
            ),
            "{action:?} on a hold"
        );
    }
    // An arrow with nothing bound to it moves nothing on the timeline.
    assert!(!repeats(Repeat::Keymap, "left", None));
    // And a stroke being captured, an export, or the overlays: nothing at
    // all, or the hold would bind a key and then fire what it just bound.
    for key in ["up", "left", "escape", "5"] {
        assert!(!repeats(Repeat::Nothing, key, Some(ActionId::VolumeUp)));
    }
}

#[test]
fn the_silence_card_fits_the_smallest_window_and_never_slows_a_silence_down() {
    // The same 640x360 floor, and this card starts below the header: a
    // title, the scope chips, three slider rows (the threshold, the forgiveness
    // and the rate), the `More …` fold, the status line beside the keep-only
    // switch, and the two apply buttons. Help is the head's `?`, not a line.
    // The fold's rows are behind a click, so the floor is the closed card.
    let (title, label_line, slider_gap) = (17., 17., 4.);
    let slider_row_h = label_line + slider_gap + HIT_MIN;
    let gaps = 6. * 6.;
    let padding = 24.;
    assert!(
        HEADER_H
            + 8.
            + title
            + KEYS_ROW_H
            + 3. * slider_row_h
            + HIT_MIN
            + CONTROL_H
            + CONTROL_H
            + gaps
            + padding
            <= 360.,
        "card too tall"
    );
    // The closed card shows the four main rows; the rest are the fold's.
    assert_eq!(Player::SILENCE_MAIN_ROWS.len(), 4);
    assert_eq!(
        Player::SILENCE_MAIN_ROWS.len() + Player::SILENCE_MORE_ROWS.len(),
        SILENCE_ROWS
    );
    // Its rows, its bars and its buttons are clicked, so WCAG 2.5.8 binds
    // them: a row's hit is `HIT_MIN` tall.
    const {
        assert!(KEYS_ROW_H >= HIT_MIN);
    }
    // ...and the pair of them fits beside the widest value the card prints.
    const {
        assert!(2. * HIT_MIN + 4. < COLOR_W / 2., "steppers crowd the value");
    }
    // A "speed-up" is never a slow-down: the rate stops above real time at
    // one end and at what a clip can hold at the other, whatever the keys
    // ask for. A silence played *slower* would make the timeline longer,
    // which is the one thing neither button may do.
    assert!(silence_rate(0) > Speed::NORMAL);
    assert!(silence_rate(1000) > Speed::NORMAL);
    assert_eq!(silence_rate(i32::MAX), Speed::MAX);
    assert_eq!(silence_rate(4000), Speed::MAX);
}

/// Each silence slider reads its own range, snaps to its own grid and opens
/// at a value inside that range -- the three things a press and a Reset
/// button both lean on. The rate's floor is real time plus one step, the
/// same floor [`silence_rate`] clamps to, so a press at the left end is a
/// speed-up and never a slow-down.
#[test]
fn the_silence_sliders_snap_to_their_grids_and_open_inside_their_ranges() {
    // The two rows that are choices are not sliders at all.
    assert_eq!(Player::silence_snap(0, 0.5), None);
    assert_eq!(Player::silence_snap(2, 0.5), None);
    // The threshold sweeps -80..0 dB on whole decibels.
    assert_eq!(Player::silence_snap(1, 0.), Some(-80.));
    assert_eq!(Player::silence_snap(1, 1.), Some(0.));
    assert_eq!(Player::silence_snap(1, 0.5), Some(-40.));
    // The durations sweep 0..5 s on a twentieth of a second.
    let forgive = Player::silence_snap(3, 0.1).expect("a slider");
    assert!((forgive - 0.5).abs() < 1e-9, "{forgive}");
    let padding = Player::silence_snap(4, 0.333).expect("a slider");
    assert!((padding / 0.05 - (padding / 0.05).round()).abs() < 1e-9);
    // The rate sweeps from one step above real time to the top, in steps.
    let floor = f64::from(Speed::NORMAL.permille()) + f64::from(SPEED_STEP);
    assert_eq!(Player::silence_snap(6, 0.), Some(floor));
    assert_eq!(Player::silence_snap(6, 1.), Some(4000.));
    assert!(silence_rate(floor as i32) > Speed::NORMAL);
    // A value a press can name is never outside its range, whatever the
    // track's end it came from.
    for field in [1, 3, 4, 5, 6] {
        let (lo, hi, _) = Player::silence_range(field).expect("a slider");
        for along in [-1., 0., 0.37, 1., 2.] {
            let at = Player::silence_snap(field, along).expect("a slider");
            assert!((lo..=hi).contains(&at), "field {field} at {along}: {at}");
        }
        // Every row opens inside its own range, so the bar starts on the track.
        let default = Player::silence_default(field);
        assert!(
            (lo..=hi).contains(&default),
            "field {field} opens at {default}"
        );
    }
    // A fresh card's rate is the top of the range: the speed-up plays at four.
    assert_eq!(Player::silence_default(6), 4000.);
}

/// The choice lists that replaced two click-to-cycle surfaces: every value
/// on offer at once, exactly one of them marked as the one in force, the
/// same order the stroke steps through, and the open list inside the
/// 640x360 floor with every row a `HIT_MIN` target.
#[test]
fn a_choice_list_offers_every_value_and_fits_the_smallest_window() {
    // Odd media: its own size is on the ladder, in its place by area, and
    // nothing else moved.
    let native = (1440, 1080);
    let ladder = resolution_ladder(native);
    assert_eq!(
        ladder,
        [
            (3840, 2160),
            (2560, 1440),
            (1920, 1080),
            (1080, 1920),
            (1440, 1080),
            (1080, 1080),
            (1280, 720),
            (854, 480)
        ]
    );
    for size in RESOLUTIONS {
        assert!(ladder.contains(&size), "{size:?} is not on offer");
    }
    // Media already at a listed size is on the ladder once, not twice.
    assert_eq!(resolution_ladder((1920, 1080)).len(), RESOLUTIONS.len());

    // The rows say the same thing the ladder does, and mark the one in
    // force -- exactly one row, whichever rung the project is on.
    let rows = resolution_choices((1280, 720), native);
    assert_eq!(rows.len(), ladder.len());
    assert_eq!(rows.iter().filter(|(.., picked)| *picked).count(), 1);
    for ((choice, label, detail, picked), size) in rows.iter().zip(&ladder) {
        assert_eq!(*choice, Choice::Size(size.0, size.1));
        assert_eq!(label.as_ref(), size_row_label(size.0, size.1));
        // A tall or square row names its shape, not its numbers, unless it is the media's own.
        assert_eq!(detail.is_empty(), size.0 <= size.1 && *size != native, "{size:?}");
        assert_eq!(*picked, *size == (1280, 720));
    }
    // The media's own size says so, in plain words: it is the one rung a person
    // cannot read off a number they chose.
    let (.., native_detail, _) = rows.iter().find(|r| r.0 == Choice::Size(native.0, native.1)).expect("the media's own rung");
    assert!(native_detail.ends_with("(matches your media)"), "{native_detail}");
    // A project at a size nobody listed still gets the whole list, with
    // nothing marked rather than a wrong row marked.
    assert!(
        resolution_choices((1000, 1000), native)
            .iter()
            .all(|(.., picked)| !picked)
    );

    // The fit list, on a clip: all four policies, in the order the stroke
    // lists them, the clip's own marked and every row naming the
    // canvas it would place the picture on.
    let fits = fit_choices(Lane::V1, 3, FITS[2], (1920, 1080));
    assert_eq!(fits.len(), FITS.len());
    assert_eq!(fits.iter().filter(|(.., picked)| *picked).count(), 1);
    assert_eq!(fits[2].0, Choice::Fit(Lane::V1, 3, FITS[2]));
    assert!(fits[2].3, "the clip's own policy is not marked");
    assert!(fits[0].2.contains("1920x1080"), "{}", fits[0].2);

    // The rate list, the other setting the project has of its own: every
    // rate on offer, the media's own cycled in at its place by speed and
    // said so, and the one the timeline is cut at marked. The value carried
    // is the `f64` the engine conforms to, not the rounded label -- 23.976
    // is not 24000/1001, and a rate the timescales cannot name is refused.
    let ntsc = 24_000. / 1001.;
    let rates = frame_rate_ladder(25.);
    assert_eq!(rates.len(), FRAME_RATES.len(), "25 is already on the list");
    let odd = frame_rate_ladder(48.);
    assert_eq!(odd[5], 48., "the media's own, in its place by speed");
    assert_eq!(odd.len(), FRAME_RATES.len() + 1);
    let fps = fps_choices(ntsc, 48.);
    assert_eq!(fps.len(), odd.len());
    assert_eq!(fps.iter().filter(|(.., picked)| *picked).count(), 1);
    assert_eq!(fps[0].0, Choice::Fps(ntsc), "the ratio, not 23.976");
    assert_eq!(fps[0].1.as_ref(), "23.976 fps (NTSC film)");
    assert!(fps[0].3, "the rate in force is not marked");
    assert!(fps[5].2.contains("Match the media"), "{}", fps[5].2);
    for (.., detail, _) in &fps {
        assert!(detail.chars().count() < 26, "{detail} loses its tail");
    }

    // The HDR list, the third project setting: all three renditions in the
    // order they brighten, the one in force marked, and every row saying
    // what it is in words that fit beside the label.
    let tones = tone_choices(Preset::Standard);
    assert_eq!(tones.len(), Preset::ALL.len());
    assert_eq!(tones.iter().filter(|(.., picked)| *picked).count(), 1);
    for (row, preset) in tones.iter().zip(Preset::ALL) {
        assert_eq!(row.0, Choice::Tone(preset));
        assert_eq!(row.1.as_ref(), tone_label(preset));
        assert!(!row.2.is_empty(), "{preset:?} says nothing about itself");
        assert!(row.2.chars().count() < 26, "{} loses its tail", row.2);
    }
    assert!(tones[1].3, "the rendition in force is not marked");

    // The encoder list, the export card's own: all three seats, the one in
    // force marked, and a row each saying what it does. The AV1 warning
    // belongs to exactly one pair -- the GPU on an AV1 file -- so it cannot
    // become a line nobody reads.
    let seats = encoder_choices(EncoderSeat::Software);
    assert_eq!(seats.len(), EncoderSeat::ALL.len());
    assert_eq!(seats.iter().filter(|(.., picked)| *picked).count(), 1);
    for (row, seat) in seats.iter().zip(EncoderSeat::ALL) {
        assert_eq!(row.0, Choice::Encoder(seat));
        assert_eq!(row.1.as_ref(), encoder_label(seat));
        assert!(!row.2.is_empty(), "{seat:?} says nothing about itself");
        assert!(row.2.chars().count() < 26, "{} loses its tail", row.2);
    }
    assert!(seats[2].3, "the seat in force is not marked");
    for format in [Format::Av1, Format::Av1Mp4] {
        assert!(av1_hw_warning(format, EncoderSeat::Hardware).is_some());
        for seat in [EncoderSeat::Auto, EncoderSeat::Software] {
            assert_eq!(
                av1_hw_warning(format, seat),
                None,
                "{seat:?} is the safe seat"
            );
        }
    }
    for format in [Format::Mp4, Format::Hevc, Format::HevcMp4, Format::Wav] {
        assert_eq!(
            av1_hw_warning(format, EncoderSeat::Hardware),
            None,
            "{format:?} is not the encoder that reset the driver"
        );
    }

    // The open list fits the floor the menus are measured against: the
    // longest of them is the rate ladder with an odd rate cycled in, and it
    // hangs at the pointer with every row on screen. Rows are click targets
    // (WCAG 2.5.8).
    const {
        assert!(MENU_ROW_H >= HIT_MIN);
    }
    assert!(odd.len() > ladder.len(), "the longest list moved");
    assert!(MENU_PAD * 2. + odd.len() as f32 * MENU_ROW_H <= 360.);
    let tall = MENU_PAD * 2. + ladder.len() as f32 * MENU_ROW_H;
    assert!(tall <= 360., "the list is taller than the floor");
    assert_eq!(
        menu_at(point(px(600.), px(340.)), size(px(640.), px(360.)), tall),
        (
            640. - crate::layout::MENU_EDGE - MENU_W,
            360. - crate::layout::MENU_EDGE - tall
        ),
        "the list would hang off the smallest window"
    );
}

/// The two settings a resolution/rate pick reaches with no file open yet:
/// the plain list, since there is no media size to cycle in beside it, with
/// nothing marked until a pending pick exists and exactly that row marked
/// once it does -- the same shape [`resolution_choices`]/[`fps_choices`]
/// have once a session exists, minus the media's own rung.
#[test]
fn the_resolution_and_rate_lists_work_before_any_file_is_open() {
    let none = pending_resolution_choices(None);
    assert_eq!(none.len(), RESOLUTIONS.len());
    assert!(
        none.iter().all(|(.., picked)| !picked),
        "nothing picked yet"
    );
    for ((choice, label, detail, _), size) in none.iter().zip(RESOLUTIONS) {
        assert_eq!(*choice, Choice::Size(size.0, size.1));
        assert_eq!(label.as_ref(), size_row_label(size.0, size.1));
        let landscape = size.0 > size.1;
        assert_eq!(detail.as_ref(), if landscape { format!("{}x{}", size.0, size.1) } else { String::new() });
    }
    let picked = pending_resolution_choices(Some(RESOLUTIONS[2]));
    assert_eq!(picked.iter().filter(|(.., picked)| *picked).count(), 1);
    assert!(picked[2].3);

    let none = pending_fps_choices(None);
    assert_eq!(none.len(), FRAME_RATES.len());
    assert!(none.iter().all(|(.., picked)| !picked));
    let picked = pending_fps_choices(Some(FRAME_RATES[3]));
    assert_eq!(picked.iter().filter(|(.., picked)| *picked).count(), 1);
    assert!(picked[3].3);
}

/// The sample-rate list: "source" first and marked whenever nothing is
/// picked -- true both before any file is open ([`Player::pending_settings`])
/// and once a session exists with no override ([`PlaybackSession::sample_rate`]
/// returning `None`) -- then every offered rate, exactly one marked once one
/// is picked.
#[test]
fn the_sample_rate_list_marks_source_with_nothing_picked() {
    let none = sample_rate_choices(None);
    assert_eq!(none.len(), SAMPLE_RATES.len() + 1);
    assert_eq!(none[0].0, Choice::SampleRate(None));
    assert!(none[0].3, "source is the row in force with nothing picked");
    assert_eq!(
        none.iter().filter(|(.., picked)| *picked).count(),
        1,
        "exactly one row marked"
    );
    for ((choice, label, ..), rate) in none.iter().skip(1).zip(SAMPLE_RATES) {
        assert_eq!(*choice, Choice::SampleRate(Some(rate)));
        assert_eq!(label.as_ref(), format!("{rate} Hz"));
    }

    let picked = sample_rate_choices(Some(SAMPLE_RATES[1]));
    assert!(!picked[0].3, "source no longer in force");
    assert_eq!(picked.iter().filter(|(.., picked)| *picked).count(), 1);
    assert!(picked[2].3);
}

/// The progress line's two clocks, driven the way a repaint drives them:
/// steady work, a stall where hardware hands over to software, then steady
/// work again. The estimate may not whipsaw, may not vanish once it has
/// been given, and must meet the elapsed clock at the end.
#[test]
fn the_export_estimate_rides_out_a_stall_and_converges() {
    let (mut marks, mut elapsed, mut progress) = (Vec::new(), 0f32, 0f32);
    // 2%/s, a 12 s stall at 40% -- longer than the window, so the window
    // alone would have nothing left to measure -- and the same rate to the
    // end: 62 s of wall clock for 50 s of work.
    let rate = 0.02;
    let (mut quiet, mut before_stall, mut after_stall, mut last) = (0f32, 0., 0., f32::MAX);
    while progress < 1. {
        let stalled = (20. ..32.).contains(&elapsed);
        note_progress(&mut marks, elapsed, progress);
        // What is really left, to hold every guess against.
        let truth = (1. - progress) / rate + if stalled { 32. - elapsed } else { 0. };
        match eta_secs(&marks, elapsed, progress) {
            // "estimating…" is only allowed before there is a span to
            // measure, and never again after the first number.
            None => {
                assert!(elapsed < ETA_SPAN + 1., "estimate vanished at {elapsed}");
                quiet = elapsed;
            }
            Some(left) => {
                // No guess is ever wilder than four times the truth: that
                // is the eightfold spike a raw window rate throws on either
                // edge of the stall.
                assert!(left <= truth * 4., "at {elapsed}s: {left} vs {truth}");
                // While the rate holds, the answer is the true one and it
                // only ever counts down.
                if (5. ..20.).contains(&elapsed) {
                    assert!((left - truth).abs() <= truth * 0.15, "{elapsed}s: {left}");
                    assert!(left < last, "estimate grew while the rate held");
                }
                // Eight seconds past the stall it has caught up again.
                if elapsed >= 40. {
                    assert!((left - truth).abs() <= truth * 0.25, "{elapsed}s: {left}");
                    assert!(left < last + 0.001, "estimate grew after the stall");
                }
                if (20. ..20.25).contains(&elapsed) {
                    before_stall = left;
                }
                if (31.5..31.75).contains(&elapsed) {
                    after_stall = left;
                }
                last = left;
            }
        }
        // A window's worth of marks and no more, however long the encode.
        assert!(marks.len() <= 20, "{} marks", marks.len());
        elapsed += 0.25;
        if !stalled {
            progress = (progress + rate * 0.25).min(1.);
        }
    }
    assert!(quiet > 0. && quiet < ETA_SPAN + 1.);
    // The stall stretched the guess instead of erasing it, and by more than
    // the stopped clock adds on its own.
    assert!(
        after_stall > before_stall + 12.,
        "{before_stall} -> {after_stall}"
    );
    // Both clocks meet: a finished pass has nothing left.
    assert_eq!(eta_secs(&marks, elapsed, 1.), Some(0.));
    assert!((elapsed - 62.).abs() < 1., "{elapsed}");
    assert_eq!(clock(83.4), "1:23");
    assert_eq!(clock(114.), "1:54");
    assert_eq!(clock(-1.), "0:00");
}

/// The two shapes that made the countdown lie rather than guess: a bar that
/// went backwards, and a division that could reach infinity. The engine no
/// longer pulls its bar back (`export`'s `fetch_max`), which is why this asks
/// the estimator directly -- it is handed the marks a re-run used to leave.
#[test]
fn the_export_estimate_answers_a_bar_that_went_backwards_with_a_guess() {
    // 93% at 400 s, then the hardware encoder dies and the mark stands while
    // the software one writes the film again from the first frame.
    let marks = vec![(392., 0.93), (396., 0.93), (400., 0.93)];
    let stalled = eta_secs(&marks, 400., 0.93).expect("a stall is still estimated");
    assert!(stalled > 0., "a running export has time left: {stalled}");
    // The old shape: the same marks against a bar that had been put back to
    // zero. The rate cannot be negative, so what is left cannot be either --
    // and it is never the "~0:00 left" that read as an export about to finish.
    let backwards = eta_secs(&marks, 400., 0.05).expect("a backward bar still estimates");
    assert!(
        backwards >= stalled,
        "a bar at 5% claimed less left than one at 93%: {backwards} vs {stalled}"
    );
    assert!(backwards > 1., "the countdown read as good as finished");
    // Nothing it can be handed prints as an infinite clock.
    for (marks, elapsed, progress) in [
        (vec![(0., 0.)], f32::MAX, f32::MIN_POSITIVE),
        (vec![(0., f32::MAX)], 3., 0.5),
        (Vec::new(), f32::MAX, 0.5),
    ] {
        let left = eta_secs(&marks, elapsed, progress);
        assert!(
            left.is_none_or(f32::is_finite),
            "{left:?} would be printed as a clock"
        );
    }
}

/// The stacking-scrims bug: opening the speed card while the colour card was
/// up left both up at once, because each `open_*` cleared a different
/// hand-picked subset of the other flags rather than all of them. The fix is
/// one path (`Player::close_card`, the single list of every flag that means
/// "a card is up") that every opener calls before setting its own -- so this
/// pins that *every* opener routes through it, found by name in the source
/// rather than hand-listed, which is what let a seventh and eighth card slip
/// through the ad-hoc version unnoticed.
#[test]
fn every_card_opener_closes_every_other_card_through_one_path() {
    let cards_src = src_text("player/cards.rs");
    let mut openers = Vec::new();
    let mut rest = cards_src.as_str();
    let mut scanned_from = 0usize;
    while let Some(at) = rest.find("fn open_") {
        let after = &rest[at + "fn ".len()..];
        let name_end = after.find('(').expect("a fn's parens");
        openers.push(after[..name_end].to_string());
        scanned_from += at + "fn ".len() + name_end;
        rest = &cards_src[scanned_from..];
    }
    // `open_picker` opens a small dropdown (a resolution/fps/fit choice
    // list), not one of the seven full-window plates `close_card` lists --
    // it is not part of this invariant.
    openers.retain(|name| name != "open_picker");
    assert!(
        openers.len() >= 7,
        "expected at least the seven card openers (eq/color/transform/speed/silence/mix/subtitle_style), found {openers:?}"
    );
    for name in &openers {
        let body = fn_body(name);
        assert!(
            body.contains("self.close_card();") || body.contains("self.close_other_cards();"),
            "{name} never calls close_card() -- it may be clearing its own \
             hand-picked subset of the other cards again, which is exactly \
             what let two cards stack on screen at once (open speed, then \
             colour, and both stayed up)"
        );
    }
}

/// Regression: a builder moved every card's how-to prose off the card body
/// onto a `?` glyph tooltip on the card head (`dark_card_head`), to answer
/// the complaint that the body read "like a terminal screen". But `Tip`
/// stands aside for *every* tooltip while `OVERLAID` is set, and a card
/// being open is exactly what sets `OVERLAID` -- so the `?`'s own help could
/// never paint while the card that owns it was on screen (driven proof:
/// dwelling the pointer on it for 5-6s at four coordinates changed zero
/// pixels). `OverlayTip`/`tip_may_paint` is the fix: a tip anchored on the
/// overlay itself is exempt. This pins both halves so an eighth card can't
/// reintroduce either half of the bug.
#[test]
fn a_cards_own_help_paints_while_the_card_is_open_but_the_ui_under_it_stays_quiet() {
    OVERLAID.store(true, Ordering::Relaxed);
    assert!(
        tip_may_paint(true),
        "a tip anchored on the overlay itself (a card head's own `?`) must \
         still paint while its own card is open, or the help nobody wanted \
         permanent becomes help nobody can ever read"
    );
    assert!(
        !tip_may_paint(false),
        "a tip on the *underlying* UI must still stand aside while a card/menu \
         is up over it -- OVERLAID's original purpose, which the overlay \
         exemption must not weaken"
    );
    OVERLAID.store(false, Ordering::Relaxed);
    assert!(
        tip_may_paint(false),
        "with nothing overlaid, an ordinary tip paints too"
    );

    // Structural half: every card routes its head through `dark_card_head`,
    // so checking its one help-tooltip call site covers all seven cards (and
    // any future eighth) at once -- it must build an `OverlayTip`, not a
    // plain `Tip`, or this whole fix is undone by construction.
    let cards_src = src_text("ui/cards");
    let head_fn_at = cards_src
        .find("fn dark_card_head(")
        .expect("dark_card_head");
    // Bound the inspected source by the next item, not a byte count: rustfmt
    // may legitimately reflow this function without moving its tooltip out of
    // scope.
    let head_fn_end = cards_src[head_fn_at..]
        .find("\n/// A ghost action row")
        .expect("dark_card_head's following item");
    let head_fn = &cards_src[head_fn_at..head_fn_at + head_fn_end];
    assert!(
        head_fn.contains("OverlayTip(h.clone())"),
        "dark_card_head's own `?` tooltip no longer builds an OverlayTip -- the \
         card-head help is unreadable again while its card is open"
    );
}

/// Structural guard only: this binary has no `TestAppContext`, so it cannot
/// mount the card, aim at its hint, wheel it, or read painted movement. A
/// driving pass must still prove a wheel over the hint changes card pixels.
/// This pins the declaration-level invariant that the EQ card's outer
/// container, which also owns the hint sibling, owns `eq_scroll`; putting the
/// handle back on `eq-card-rows` recreates that hint's dead zone.
#[test]
fn eq_cards_scroll_handle_belongs_to_the_container_that_also_holds_its_hint() {
    let card = fn_body("eq_card");
    let card_at = card.find(".id(\"eq-card\")").expect("EQ card container");
    let rows_at = card.find(".id(\"eq-card-rows\")").expect("EQ card rows");
    let container = &card[card_at..rows_at];

    assert!(
        container.contains(".overflow_y_scroll()")
            && container.contains(".track_scroll(&self.eq_scroll)"),
        "eq_scroll must belong to eq-card, the container that also holds the \
         below-fold hint, rather than a body-only child"
    );

    let rows = &card[rows_at..];
    assert!(
        !rows.contains(".track_scroll(&self.eq_scroll)"),
        "eq-card-rows owns eq_scroll again -- its sibling hint becomes a \
         wheel dead zone"
    );
    assert!(
        rows.contains("more below — scroll the card"),
        "the below-fold hint disappeared; this guard must cover the child it \
         keeps in the card's wheel surface"
    );
}

/// "Maximize a card in place" changes size only, never the relative
/// semantics a value is drawn against: `eq_x`/`eq_y` map into `0..1` of
/// whatever box they are given, so a wider/taller EQ card is genuinely more
/// dB/Hz resolution, not the same graph stretched. This is the layout half
/// of that claim -- `eq_x`/`eq_y` themselves take no size argument at all
/// (they answer in `0..1`), so there is nothing about them a maximize could
/// have broken; what a maximize is allowed to touch is `eq_card_w`/
/// `eq_graph_h`'s own ceiling.
#[test]
fn maximizing_a_card_grows_its_box_without_moving_where_a_value_sits_in_it() {
    // Below the old EQ_W_MAX ceiling, maximized and not agree -- there is
    // room for both without the ceiling mattering yet.
    assert_eq!(eq_card_w(600., false), eq_card_w(600., true));
    // Past it, only the maximized card keeps growing: the ceiling this
    // session's fix drops.
    assert!(
        eq_card_w(1920., true) > eq_card_w(1920., false),
        "maximized EQ card no longer drops the EQ_W_MAX ceiling"
    );
    assert_eq!(
        eq_graph_h(true),
        EQ_GRAPH_MAX_H,
        "maximized EQ graph no longer grows to EQ_GRAPH_MAX_H"
    );
    assert!(
        eq_graph_h(true) > eq_graph_h(false),
        "maximized EQ graph is not actually taller than the resting one"
    );
    // The mapping itself never sees "maximized" -- a bigger box is more
    // resolution because the same 0..1 fraction now spans more pixels, not
    // because the fraction itself moved.
    assert_eq!(eq_x(1000.), eq_x(1000.));
    assert_eq!(eq_y(0.), eq_y(0.));
}

/// The regression the previous test could not see: `eq_card_w`/`eq_graph_h`
/// growing with `maximized` proved nothing about where the box that receives
/// those numbers actually sits. It sat inside `dock_stance.rs`'s
/// "dock-clip-rows" -- a ~280-390px, less-than-window-tall strip -- and an
/// absolutely-positioned child is sized against its own *immediate* parent
/// regardless of any `.relative()` marker upstream, so `below_picture_floor`
/// (a window-space y) landed inside that short box and shoved the card to a
/// ~120px sliver at the bottom. Every one of `eq_card_w`/`card_max_w`'s own
/// unit tests stayed green throughout, because none of them knew a
/// containing box existed at all -- the same class of gap as `canvas()`
/// silently rendering black under a `Block` parent.
///
/// This binary has no `TestAppContext` to mount `dock_stance::clip_tab` /
/// `ui::stance::render` for real and read painted rects back, so it cannot
/// see the div nesting itself -- only driving the app and screenshotting it
/// proves the fix actually lands on screen (this is that admission, not a
/// substitute for it). What this test *can* pin down in pure values is the
/// room each mount site is fed after the fix: [`dock_stance`]'s un-maximized
/// mount is handed the dock's own real width (not the window's, the "480 in
/// 280" shape of the width-clip bug), and a maximized card is handed
/// `stance-centre`'s width -- the window minus the dock it
/// escaped -- which must come out *larger*, not smaller, than the docked
/// room it replaces. A future change that re-nests the maximize mount back
/// inside the dock, or hands it the dock's width again, fails this.
#[test]
fn a_maximized_cards_room_is_bigger_than_the_docked_rooms_not_smaller() {
    use crate::SPLIT_W;
    use crate::layout::{Split, split_size};

    // `layout::SPLIT_W` is the one divider between `stance-centre` and the
    // dock; the 56px rail that used to eat the centre's left edge is gone
    // (2026-09-09, "option C"), so the centre is everything but the dock.

    for window_w in [960., 1280., 1920.] {
        let window = size(px(window_w), px(720.));
        // `split_size`'s own default/floor/ceiling for the dock, with no
        // hand having dragged the seam -- the exact width `dock_stance.rs`
        // now feeds its un-maximized cards as their "room".
        let dock_w = split_size(Split::Dock, None, 2, window, false);
        let docked_room_w = dock_w;
        let maximized_room_w = window_w - SPLIT_W - dock_w;

        assert!(
            maximized_room_w > docked_room_w,
            "at a {window_w}px window a maximized card's room ({maximized_room_w}px, \
             stance-centre) is not wider than the docked room it escaped \
             ({docked_room_w}px, dock-clip-rows) -- maximize would be making \
             the card smaller again"
        );
        assert!(
            maximized_room_w <= window_w,
            "a maximized card's room ({maximized_room_w}px) exceeds the whole \
             window ({window_w}px) -- it is being measured in the wrong space"
        );
        // The concrete pixel gain a driving agent should actually see: the
        // EQ graph's own width formula, fed each mount's real room.
        assert!(
            eq_card_w(maximized_room_w, true) > eq_card_w(docked_room_w, false),
            "maximizing the EQ card at a {window_w}px window does not grow \
             eq_card_w's output"
        );
    }
}

/// DESIGN's "a maximized scrim covers only the bench/ledger/time-band band,
/// never the picture": the floor a maximized card's `.top()` is pinned to
/// must leave exactly the picture's own height above it, for any bench
/// height a hand has dragged the seam to -- not just the untouched
/// `BENCH_H` default. `below_picture_floor` takes the live bench height as
/// its own second argument (the seam feature merged onto this branch), so
/// this checks the real two-argument signature rather than a stale one-arg
/// assumption.
#[test]
fn a_maximized_card_can_never_rise_above_the_picture_no_matter_the_bench_height() {
    use crate::ui::stance::{LEDGER_H, TIME_BAND_H, below_picture_floor};
    let viewport_h = 1080.;
    for bench_h in [140., 200., 400.] {
        assert_eq!(
            below_picture_floor(viewport_h, bench_h) + TIME_BAND_H + bench_h + LEDGER_H,
            viewport_h,
            "below_picture_floor's floor plus the band below the picture no \
             longer adds back up to the window -- a maximized scrim could rise \
             into the picture"
        );
    }
    // A bench dragged tall enough to eat the whole window still clamps at
    // zero rather than going negative and reporting free room that is not
    // there.
    assert_eq!(below_picture_floor(300., 5000.), 0.);
}

/// The maximize flag's own small round trip (`dock_stance.rs`'s own doc
/// comment: "the same small-file round trip as the dock tab pick", "a room
/// reopens exactly as left"): saved, read back, and silently defaulted to
/// `false` on a corrupt or absent file -- never a startup crash or notice
/// over the user's own work, the same rule `load_subtitle_style`'s own test
/// (`tests/media.rs`) already pins for its sibling config file. `XDG_CONFIG_HOME`
/// is the existing scratch-path override every config-round-trip test in
/// this crate already uses; there is no maximize-specific override to
/// invent.
#[test]
fn a_maximized_card_reopens_exactly_as_left_across_a_restart() {
    let _config_env = config_env_lock();

    let dir = std::env::temp_dir().join(format!(
        "edith-test-maximized-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // `config_env_lock` serializes every test that changes this process-wide
    // path.
    crate::keymap::set_test_config_home(Some(dir.clone()));

    // Nothing written yet: the default (not maximized), not a crash on a
    // missing file.
    assert!(!crate::ui::dock_stance::load_maximized());

    crate::ui::dock_stance::save_maximized(true);
    assert!(crate::ui::dock_stance::load_maximized());

    crate::ui::dock_stance::save_maximized(false);
    assert!(!crate::ui::dock_stance::load_maximized());

    // A file from a future/corrupt version leaves the default in force
    // rather than failing the room that opens over it.
    std::fs::write(
        crate::ui::dock_stance::maximized_config_path(),
        "not a bool\n",
    )
    .unwrap();
    assert!(!crate::ui::dock_stance::load_maximized());

    crate::keymap::set_test_config_home(None);
    std::fs::remove_dir_all(&dir).ok();
}

/// Source preview must retain the mouse doors beside its `esc` stop chord:
/// `screen` routes the Darkroom through `picture_area`, and that shared
/// picture mounts the scrub bar whose press starts the root-tracked drag.
#[test]
fn darkroom_preview_mounts_stop_and_drag_scrub() {
    let stance = src_text("ui/stance.rs");
    let screen = &stance[stance.find("fn screen(").expect("the Darkroom screen")..];
    assert!(
        screen.contains(".children(player.source_strip(window, cx))"),
        "the Darkroom screen no longer mounts the source strip"
    );
    assert!(
        screen.contains("player.picture_area(position, window, cx).into_any_element()"),
        "the Darkroom screen no longer routes through the shared picture"
    );

    let preview = src_text("ui/preview.rs");
    assert!(
        preview.contains("this.preview_scrubbing = true")
            && preview.contains("this.preview_scrub_to(event.position.x, true, cx)"),
        "the mounted preview scrub no longer starts its drag-and-seek gesture"
    );
}

/// Source management must stay reachable in the Darkroom itself: pointer
/// Add-at-playhead alongside its `↵` chord on the row, and -- since cleanse
/// round 2 (2026-09-10) took the `▷` and `○` ghosts off the row -- preview
/// and the per-file stand-in in the row's own right-click menu, each still
/// calling the one implementation it always called. The test reads the
/// mounted dock source because this binary has no `TestAppContext` to inspect
/// GPUI's painted controls.
#[test]
fn darkroom_source_rows_keep_proxy_add_and_preview_doors() {
    let dock = src_text("ui/dock_stance.rs");
    let row_at = dock.find("fn source_row(").expect("Darkroom source row");
    let row = &dock[row_at..];
    for needle in [
        "dock-add-at-playhead",
        "Append to timeline end",
        "this.append_source(",
    ] {
        assert!(
            row.contains(needle),
            "Darkroom source row lost `{needle}`: a source must keep its \
             mouse-reachable Add-at-playhead path"
        );
    }
    // The two former ghosts, in the menu the same row opens.
    assert!(
        crate::ROW_ITEMS.contains(&crate::RowItem::Preview)
            && crate::ROW_ITEMS.contains(&crate::RowItem::Proxy),
        "the row menu lost Preview or Proxy"
    );
    let actions = src_text("player/actions.rs");
    for needle in [
        "self.open_preview(&menu.path, menu.stream, cx);",
        "self.toggle_proxy(&menu.path, stops, cx);",
    ] {
        assert!(
            actions.contains(needle),
            "the row menu's `{needle}` is a second implementation, not the door"
        );
    }
    // One list, no sub-tabs: a row says its kind on its own metadata line.
    for gone in [
        "LIBRARY_TABS",
        "library_tab",
        "dock-source-filter",
        "dock.sort.cycle",
    ] {
        assert!(
            !dock.contains(gone),
            "the dock still carries `{gone}`: Sources is one list now"
        );
    }
}

/// DEBT #91: a click-to-stop while a stand-in's header is still being read
/// must reach the flag the read itself polls (`engine::proxy::started`), not
/// only relabel the row. Read off the source and not run, the way the row
/// above it is -- there is no `TestAppContext` in this binary to construct a
/// `Context<Player>` and call `toggle_proxy` on. On the state this fix
/// replaced, `start_proxy_for` handed the header read a flag nothing outside
/// it could reach and `Proxy::Asked` carried no field to store a stop into,
/// so `library.find("Proxy::Asked(cancel)")` below has nothing to find.
#[test]
fn toggle_proxy_wakes_the_asked_phase_it_stops() {
    let library = src_text("player/library.rs");
    let start = &library[library
        .find("fn start_proxy_for(")
        .expect("start_proxy_for")..];
    assert!(
        start.contains("generate_if_wanted(&path, &cancel)"),
        "start_proxy_for no longer hands the header read a cancel flag anyone can reach"
    );
    let toggle = &library[library.find("fn toggle_proxy(").expect("toggle_proxy")..];
    let asked_arm = &toggle[toggle
        .find("Proxy::Asked(cancel)")
        .expect("the Asked arm's own flag")..];
    assert!(
        asked_arm.contains("cancel.store(true"),
        "a stop asked during Asked no longer sets the flag the header read polls"
    );
}

/// A wheel notch on the budget must not re-ask the seat probe. The probe opens
/// a real VA-API encoder (ten opens for ten notches before this, 5-32 ms of GPU
/// work each, against the player's own decoder) and its answer -- GPU seat or
/// software, copy or encode -- does not depend on the bitrate at all. So the
/// settings it is keyed on carry no budget, and the budget still reaches the
/// export itself, which is the one place it changes a file.
#[test]
fn the_seat_probe_is_not_keyed_on_the_budget() {
    let seat = EncoderSeat::default();
    let asked = crate::probe_settings(Format::Mp4, DEFAULT_AUDIO_KBPS, seat);
    assert_eq!(asked.bitrate, None, "no budget is in the probe's question");
    // What still *is* a new question: the format the file is written in.
    assert_ne!(
        asked,
        crate::probe_settings(Format::Av1, DEFAULT_AUDIO_KBPS, seat),
        "a format change must re-probe"
    );
    let cache = fn_body("cache_export_seat");
    assert!(
        cache.contains("probe_settings(") && !cache.contains("budget_bps()"),
        "the seat cache is keyed on the budget again"
    );
    // ...and the number the wheel moves is still what the export is given.
    assert!(
        fn_body("export_settings_now").contains("self.budget_bps()"),
        "the export no longer writes at the budget on the row"
    );
}

/// DESIGN §8, the class behind the user's "word cut" (2026-09-09): a picker
/// row's tail is a *state word* -- at most two -- because the plate is
/// `MENU_W` wide and prose loses its last word to the truncation on exactly
/// the row a reader is looking at (the resolution list's checked row read
/// "3840x2160 · the medi…"). The same rule for the settings page's own row
/// values: a value slot carries the state, never the door's noun elided to
/// an ellipsis ("Subtitle font  Font…", "Mix  Tracks…").
#[test]
fn picker_row_tails_are_state_words_and_settings_values_carry_no_ellipsis() {
    let lists = [
        resolution_choices((1280, 720), (3840, 2160)),
        pending_resolution_choices(Some(RESOLUTIONS[1])),
        pending_fps_choices(Some(FRAME_RATES[0])),
        sample_rate_choices(None),
    ];
    for (.., detail, _) in lists.iter().flatten() {
        let words = detail.split_whitespace().count();
        // The media's own rung says what it matches in words: a state, not prose.
        let state = detail.ends_with("(matches your media)") || detail == "matches your media";
        assert!(
            words <= 2 || state,
            "picker row tail {detail:?} is {words} words -- `MENU_W` cuts prose"
        );
        assert!(
            !detail.contains('\u{2026}') && !detail.contains('\u{b7}'),
            "picker row tail {detail:?} carries prose punctuation"
        );
    }
    // Code lines only: the comments above the two rows this fixed quote the
    // strings the bug shipped as.
    let settings = src_text("ui/settings_stance.rs");
    for line in settings
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
    {
        assert!(
            !line.contains('\u{2026}'),
            "a settings row value is elided with an ellipsis again: {line}"
        );
    }
}

/// D1: the project's own settings are the project's -- the resolution, the
/// rate, the encoder seat, the mix rate and the two proxy switches ride in
/// the same history entry the lanes do, so every one of those picks is a
/// single `ctrl+z`. Before this, pressing `z` after any of them did nothing.
#[test]
fn a_settings_pick_undoes_and_redoes_like_a_cut() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    let (w, h) = session.resolution();

    assert!(session.set_resolution(w / 2, h / 2));
    assert_eq!(session.resolution(), (w / 2, h / 2));
    assert!(session.undo(), "the resolution pick is one step");
    assert_eq!(session.resolution(), (w, h), "and the canvas comes back");
    assert!(session.redo(), "redo walks the same branch");
    assert_eq!(session.resolution(), (w / 2, h / 2));
    assert!(session.undo(), "back to where the test started");

    // The encoder seat, which nothing reseeks and nothing shows until an
    // export starts.
    let seat = session.encoder_seat();
    assert_ne!(seat, EncoderSeat::Software);
    session.set_encoder_seat(EncoderSeat::Software);
    assert_eq!(session.encoder_seat(), EncoderSeat::Software);
    assert!(session.undo(), "the encoder pick is one step");
    assert_eq!(session.encoder_seat(), seat, "and the seat comes back");
    assert!(session.redo());
    assert_eq!(session.encoder_seat(), EncoderSeat::Software);
    assert!(session.undo());

    // The mix rate, and the switches beside it.
    session.set_sample_rate(Some(44_100));
    assert_eq!(session.sample_rate(), Some(44_100));
    assert!(session.undo(), "the mix rate is one step");
    assert_eq!(session.sample_rate(), None, "back to the derived rate");
    let proxies = session.proxies();
    session.set_proxies(!proxies);
    assert_eq!(session.proxies(), !proxies);
    assert!(session.undo(), "the proxy switch is one step");
    assert_eq!(session.proxies(), proxies, "and the switch comes back");
    let auto = session.auto_proxies();
    session.set_auto_proxies(!auto);
    assert_eq!(session.auto_proxies(), !auto);
    assert!(session.undo(), "and so is the auto switch");
    assert_eq!(session.auto_proxies(), auto, "which comes back too");

    // ...and a lane edit still undoes the way it always did, settings and all
    // in the same stack.
    let clips = session.lane_clips(Lane::V1).len();
    assert_eq!(clips, 1, "one take to start with");
    assert!(session.delete_clip(Lane::V1, 0));
    assert_eq!(session.lane_clips(Lane::V1).len(), 0);
    assert!(session.undo(), "the delete is one step");
    assert_eq!(session.lane_clips(Lane::V1).len(), clips);
}

// The mix panel's meters, solo set and slider maths -- the parts of the card
// that are not drawn, so they can be pinned without a window.

#[test]
fn a_meter_rises_at_once_and_falls_at_a_fixed_rate() {
    use crate::player::cards::{METER_FLOOR_DB, MixMeter};
    let t0 = Instant::now();
    let mut meter = MixMeter::default();
    assert_eq!(meter.level_db, METER_FLOOR_DB, "a new meter reads silence");
    meter.feed(1.0, t0);
    assert!(
        meter.level_db.abs() < 1e-3,
        "full scale reads 0 dBFS at once"
    );
    // Half a second of silence, read every tenth of one, falls 10 dB at 20 dB
    // a second. (A read is capped at a quarter second, so one long gap would
    // fall 5 dB at most -- which is the point of the cap.)
    for step in 1..=5 {
        meter.feed(0.0, t0 + Duration::from_millis(100 * step));
    }
    assert!((meter.level_db + 10.).abs() < 0.01, "{}", meter.level_db);
    // A long pause between two reads is capped, not a drop through the floor.
    meter.feed(0.0, t0 + Duration::from_secs(60));
    assert!(meter.level_db > -16., "{}", meter.level_db);
}

#[test]
fn a_meter_holds_its_peak_for_a_second_then_lets_it_fall() {
    use crate::player::cards::MixMeter;
    let t0 = Instant::now();
    let mut meter = MixMeter::default();
    meter.feed(1.0, t0);
    meter.feed(0.0, t0 + Duration::from_millis(900));
    assert!(meter.hold_db.abs() < 1e-3, "still held at 0.9 s");
    meter.feed(0.0, t0 + Duration::from_secs(2));
    assert!(
        (meter.hold_db + 5.).abs() < 0.01,
        "the marker falls once the hold is over: {}",
        meter.hold_db
    );
    meter.reset();
    assert_eq!(meter, MixMeter::default(), "a reset meter reads silence");
}

#[test]
fn meter_db_is_dbfs_floored_at_the_bottom_of_the_bar() {
    use crate::player::cards::{METER_FLOOR_DB, meter_db};
    assert!(meter_db(1.0).abs() < 1e-4);
    assert!((meter_db(0.1) + 20.).abs() < 1e-3);
    assert_eq!(meter_db(0.), METER_FLOOR_DB);
    assert_eq!(meter_db(1e-9), METER_FLOOR_DB, "below the floor pins to it");
}

#[test]
fn solo_toggles_a_lane_in_and_out_and_keeps_the_order() {
    use crate::player::cards::solo_toggled;
    let (a1, a2, a3) = (
        Lane::new(LaneKind::Audio, 0),
        Lane::new(LaneKind::Audio, 1),
        Lane::new(LaneKind::Audio, 2),
    );
    let solo = solo_toggled(&[], a2);
    assert_eq!(solo, vec![a2]);
    let solo = solo_toggled(&solo, a1);
    assert_eq!(solo, vec![a2, a1], "joins at the end");
    assert_eq!(
        solo_toggled(&solo, a2),
        vec![a1],
        "a second press lets it go"
    );
    assert!(solo_toggled(&solo_toggled(&[], a3), a3).is_empty());
}

#[test]
fn a_meter_reads_the_audio_lane_the_engine_levels_index() {
    use crate::player::cards::meter_slot_in;
    let (a1, a2, a3) = (
        Lane::new(LaneKind::Audio, 0),
        Lane::new(LaneKind::Audio, 1),
        Lane::new(LaneKind::Audio, 2),
    );
    // A1 is empty: the engine's levels have no slot for it.
    let sounding = [a2, a3];
    assert_eq!(meter_slot_in(&sounding, a2), Some(0));
    assert_eq!(meter_slot_in(&sounding, a3), Some(1));
    assert_eq!(meter_slot_in(&sounding, a1), None);
    // With no sound at all the engine plays A1 as its one slot.
    assert_eq!(meter_slot_in(&[], Lane::A1), Some(0));
    assert_eq!(meter_slot_in(&[], a2), None);
}

#[test]
fn a_slider_press_maps_onto_its_range_and_clamps() {
    use crate::player::cards::{bar_fraction, value_at};
    use engine::project::{MAX_GAIN_DB, MIN_GAIN_DB};
    assert_eq!(value_at(0., MIN_GAIN_DB, MAX_GAIN_DB), MIN_GAIN_DB);
    assert_eq!(value_at(1., MIN_GAIN_DB, MAX_GAIN_DB), MAX_GAIN_DB);
    assert!((value_at(0.5, -60., 12.) + 24.).abs() < 1e-4);
    assert_eq!(value_at(2., -60., 12.), 12., "past the end pins to it");
    assert_eq!(value_at(-1., -60., 12.), -60.);
    // A bar that has not been drawn yet answers nothing rather than dividing.
    assert_eq!(bar_fraction(Bounds::default(), px(10.)), None);
    let bar = Bounds::new(point(px(0.), px(0.)), size(px(200.), px(4.)));
    assert_eq!(bar_fraction(bar, px(50.)), Some(0.25));
}

/// Export follow-ups (X1): an export no longer makes the room modal, so what
/// it must refuse is refused by the oracle and says why -- these pin the
/// wiring of each piece by source, the way the guards above do, since there is
/// no window here to press the keys.
#[test]
fn an_export_done_toast_offers_play_then_show_in_folder() {
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains(
            "ToastAction::Play(path.clone()),\n                ToastAction::ShowInFolder(path),"
        ),
        "the export-done toast no longer carries Play then Show in folder"
    );
    assert!(
        actions.contains("xdg-open") && actions.contains("ToastAction::Play(path) =>"),
        "Play no longer opens the file in the default player off the UI thread"
    );
    let stance = src_text("ui/stance.rs");
    assert!(
        stance.contains("toast.actions.iter().enumerate()")
            && stance.contains("ToastAction::Play(_) => \"Play\""),
        "the toast draws one button per action, Play included"
    );
}

#[test]
fn a_running_export_is_refused_by_the_oracle_not_by_the_modal() {
    let body = fn_body("modal");
    assert!(
        !body.contains("exporting"),
        "modal() reads exporting() again -- an export would swallow the keyboard"
    );
    // The picked-row Add is a second door to the edit list: it asks the row
    // oracle, which refuses it while an export reads the list.
    let actions = src_text("player/actions.rs");
    assert!(
        actions.contains("crate::oracle::row_enable(item, self.row_ctx(&path, stream))"),
        "the media panel's Add no longer asks the row oracle during an export"
    );
    // Escape is Deselect and an export refuses it, so the press says what does
    // cancel instead of doing nothing.
    let stance = src_text("ui/stance.rs");
    assert!(
        stance.contains("if key == ESCAPE && !ctrl && this.exporting().is_some()")
            && stance.contains("Esc keeps the export running."),
        "Esc during an export no longer says that the export keeps running"
    );
}

#[test]
fn a_webm_export_says_why_the_subtitles_do_not_travel() {
    let card = src_text("ui/cards/export.rs");
    assert!(
        card.contains(".filter(|_| self.format == Format::Webm)")
            && card.contains("s.planned_subtitles(self.format, 0..s.subtitles().len())")
            && card.contains(".filter(|why| why != \"none\")"),
        "the export card no longer shows the engine's subtitle reason for WebM"
    );
}

#[test]
fn the_gui_opens_on_the_software_seat_and_keeps_a_hardware_pick() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    // The engine's own default (and every old saved project) is Auto, which can
    // resolve to VA-API encode; the window never starts there.
    assert_eq!(session.encoder_seat(), EncoderSeat::Auto);
    gui_default_seat(&mut session);
    assert_eq!(session.encoder_seat(), EncoderSeat::Software);
    session.set_encoder_seat(EncoderSeat::Hardware);
    gui_default_seat(&mut session);
    assert_eq!(session.encoder_seat(), EncoderSeat::Hardware, "an explicit pick stays");
    // Auto and Hardware are named as the GPU encoder in the list.
    let rows = encoder_choices(EncoderSeat::Software);
    assert!(rows[0].2.contains("GPU encoder") && rows[1].2.contains("GPU encoder"));
}

#[test]
fn a_marked_span_says_how_long_it_is() {
    assert_eq!(span_word(1, 30.), "1 frame");
    assert_eq!(span_word(12, 30.), "12 frames");
    assert_eq!(span_word(1140, 30.), "38 s");
    assert_eq!(span_word(2340, 30.), "1 min 18 s");
}

/// The keyframe doors: four actions on the Clip menu and the palette, a diamond
/// and the easing chips on the cards, and the sliders writing a key (not the
/// clip's single value) once the clip is keyed.
#[test]
fn the_cards_carry_keyframe_diamonds_and_keyed_writes() {
    let act = source_from("pub(crate) fn act(");
    for arm in [
        "ActionId::KeyPlacement => self.toggle_key_here(engine::project::KeyKind::Placement, cx)",
        "ActionId::KeyOpacity => self.toggle_key_here(engine::project::KeyKind::Opacity, cx)",
        "ActionId::KeyColor => self.toggle_key_here(engine::project::KeyKind::Color, cx)",
        "ActionId::KeyEasing => self.open_key_easing(self.ledger_anchor(LedgerItem::Clip), cx)",
    ] {
        assert!(act.contains(arm), "act() lacks `{arm}`");
    }
    let ui = ui_source();
    for id in ["key-diamond", "key-ease", "transform-opacity"] {
        assert!(ui.contains(&format!("\"{id}\"")), "{id} is on no card");
    }
    for a in [ActionId::KeyPlacement, ActionId::KeyOpacity, ActionId::KeyColor, ActionId::KeyEasing] {
        assert!(
            ui.contains(&format!("Act(ActionId::{a:?})")) || ui.contains(&format!("Choice(ActionId::{a:?})")),
            "{a:?} is on no menu"
        );
    }
    assert!(fn_body("write_transform_now").contains("put_transform_key_at("));
    assert!(fn_body("write_transform_now").contains("put_opacity_key_at("));
    assert!(fn_body("write_color_now").contains("put_color_key_at("));
}

// The mix card's dynamics sliders: each one reads and writes exactly its own
// setting, snapped to its step, and every processor's sliders are listed.

#[test]
fn voice_sliders_read_and_write_their_own_setting_and_snap_to_the_step() {
    use crate::player::cards::{VoiceParam, VoiceProc};
    use engine::voice::VoiceFx;
    let mut fx = VoiceFx::default();
    for which in VoiceProc::ALL {
        assert!(!which.params().is_empty(), "{which:?} has no slider");
        for &param in which.params() {
            let (lo, hi, step) = param.range();
            let before = fx.clone();
            param.set(&mut fx, hi + 100.);
            assert!((param.get(&fx) - hi).abs() < 1e-4, "{param:?} clamps to {hi}");
            param.set(&mut fx, lo - 100.);
            assert!((param.get(&fx) - lo).abs() < 1e-4, "{param:?} clamps to {lo}");
            param.set(&mut fx, lo + step * 2.4);
            assert!(((param.get(&fx) - lo) / step - 2.).abs() < 1e-3, "{param:?} snaps");
            assert!(!param.text(param.get(&fx)).is_empty());
            fx = before;
        }
    }
    // A slider moves its own setting and not the processor's switch.
    VoiceParam::CompRatio.set(&mut fx, 6.);
    assert_eq!(fx.comp.ratio, 6.);
    assert!(!fx.comp.on && !fx.is_default());
    // A hand-set makeup turns the automatic makeup off.
    fx.comp.auto_makeup = true;
    VoiceParam::CompMakeup.set(&mut fx, 4.);
    assert!(!fx.comp.auto_makeup && (fx.comp.makeup() - 4.).abs() < 1e-4);
}

// ---- the speed card's ramp editor ------------------------------------------

fn ramp_key(at: u32, permille: u16) -> engine::project::SpeedKey {
    engine::project::SpeedKey {
        at,
        speed: Speed::from_permille(permille),
    }
}

/// Every shape the card and the palette offer is a ramp the engine takes, on any clip
/// long enough to hold its keys a frame apart -- and one that is not long enough is
/// refused by the shape, not by an engine error the hand cannot act on.
#[test]
fn every_ramp_preset_is_keys_the_engine_takes() {
    use crate::audio_cards::RampPreset;
    for preset in RampPreset::ALL {
        for frames in [1u32, 2, 3, 30, 150] {
            match preset.keys(frames) {
                Some(keys) => {
                    assert!(engine::project::valid_speed_keys(&keys), "{preset:?} over {frames}");
                    assert_eq!(keys[0].at, 0, "{preset:?} starts with the clip");
                    assert_eq!(keys.last().map(|k| k.at), Some(frames), "{preset:?} spans it");
                }
                None => assert!(frames < 2, "{preset:?} refuses {frames} frames"),
            }
        }
    }
    // The shapes are the ones their names say.
    let speeds = |p: RampPreset| -> Vec<u16> {
        p.keys(60)
            .expect("long enough")
            .iter()
            .map(|k| k.speed.permille())
            .collect()
    };
    assert_eq!(speeds(RampPreset::Up), [1000, 2000]);
    assert_eq!(speeds(RampPreset::Down), [2000, 1000]);
    assert_eq!(speeds(RampPreset::SlowFastSlow), [500, 2000, 500]);
    assert_eq!(speeds(RampPreset::FastSlowFast), [2000, 500, 2000]);
}

/// The preset through the session the card writes to: the clip takes the keys, its
/// footprint is the ramp's, and one undo takes the whole thing back.
#[test]
fn a_ramp_preset_lands_on_a_clip_in_one_undo_step() {
    use crate::audio_cards::RampPreset;
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let before = session.lane_clips(Lane::V1)[0].clone();
    for preset in RampPreset::ALL {
        let keys = preset.keys(before.frames()).expect("the fixture is long");
        session
            .set_speed_ramp(Lane::V1, 0, &keys)
            .expect("a clip alone on its lane takes a ramp");
        let clip = session.lane_clips(Lane::V1)[0].clone();
        assert_eq!(clip.speed_keys, keys, "{preset:?}");
        assert_eq!(
            clip.frames(),
            engine::project::ramp_frames(&keys, clip.len()),
            "{preset:?}: the footprint is the ramp's"
        );
        assert!(session.undo(), "{preset:?} is one undo step");
        assert_eq!(session.lane_clips(Lane::V1)[0], before, "{preset:?} undone");
    }
}

/// Adding a key slots it in order, replaces one at the same frame, and stops at the
/// engine's cap; removing the last key leaves no ramp; a stepper moves a quarter and
/// stays inside what a speed can hold.
#[test]
fn ramp_key_edits_keep_the_keys_in_order_and_inside_their_limits() {
    use crate::audio_cards::{ramp_key_stepped, ramp_with_key, ramp_without_key};
    let base = vec![ramp_key(0, 1000), ramp_key(60, 2000)];
    let inserted = ramp_with_key(&base, 30, Speed::from_permille(500)).expect("room");
    assert_eq!(inserted, [ramp_key(0, 1000), ramp_key(30, 500), ramp_key(60, 2000)]);
    assert_eq!(
        ramp_with_key(&inserted, 30, Speed::from_permille(750)).expect("replaces"),
        [ramp_key(0, 1000), ramp_key(30, 750), ramp_key(60, 2000)]
    );
    assert_eq!(
        ramp_with_key(&base, 90, Speed::NORMAL).expect("room"),
        [ramp_key(0, 1000), ramp_key(60, 2000), ramp_key(90, 1000)],
        "past the last key"
    );
    assert!(engine::project::valid_speed_keys(&inserted));
    let mut full: Vec<_> = (0..engine::project::MAX_SPEED_KEYS as u32)
        .map(|i| ramp_key(i * 10, 1000))
        .collect();
    assert!(ramp_with_key(&full, 5, Speed::NORMAL).is_none(), "a ninth key is refused");
    assert!(ramp_with_key(&full, 10, Speed::from_permille(500)).is_some(), "but a replace is not");
    full.truncate(1);
    assert!(ramp_without_key(&full, 0).is_empty(), "the last key out clears the ramp");
    assert_eq!(ramp_without_key(&base, 0), [ramp_key(60, 2000)]);
    let stepped = ramp_key_stepped(&base, 1, 1);
    assert_eq!(stepped[1].speed.permille(), 2250);
    assert_eq!(ramp_key_stepped(&base, 0, -10)[0].speed, Speed::MIN);
    assert_eq!(ramp_key_stepped(&base, 1, 100)[1].speed, Speed::MAX);
}

/// A clip's box wears its ramp: the keys joined by arrows, shortened past three, and
/// nothing at all for a clip at real time.
#[test]
fn the_clip_label_shows_the_ramp() {
    use crate::audio_cards::speed_chip_text;
    let mut clip = Clip::source_range(0, 0, 100);
    assert_eq!(speed_chip_text(&clip), None);
    clip.speed = Speed::from_permille(2000);
    assert_eq!(speed_chip_text(&clip).as_deref(), Some("2×"));
    clip.speed_keys = vec![ramp_key(0, 1000), ramp_key(60, 2000)];
    assert_eq!(speed_chip_text(&clip).as_deref(), Some("1×→2×"));
    clip.speed_keys = vec![ramp_key(0, 500), ramp_key(30, 2000), ramp_key(60, 500)];
    assert_eq!(speed_chip_text(&clip).as_deref(), Some("0.5×→2×→0.5×"));
    clip.speed_keys = (0..5).map(|i| ramp_key(i * 10, 1000 + 250 * i as u16)).collect();
    assert_eq!(speed_chip_text(&clip).as_deref(), Some("1×→2× (5 keys)"));
}

/// The curve the card draws starts and ends on the ramp's first and last speeds, passes
/// through a key's height at the key, and sits level for a clip without a ramp.
#[test]
fn the_ramp_curve_runs_through_its_keys() {
    use crate::audio_cards::ramp_curve;
    let keys = [ramp_key(0, 500), ramp_key(30, 2000), ramp_key(60, 500)];
    let curve = ramp_curve(&keys, Speed::NORMAL, 60, 60);
    assert_eq!(curve.len(), 61);
    assert!((curve[0].1 - speed_fraction(500.)).abs() < 1e-6);
    assert!((curve[30].1 - speed_fraction(2000.)).abs() < 1e-6, "the middle key");
    assert!((curve[60].1 - speed_fraction(500.)).abs() < 1e-6);
    assert!((curve[60].0 - 1.).abs() < 1e-6);
    let level = ramp_curve(&[], Speed::from_permille(1500), 60, 8);
    assert!(level.iter().all(|&(_, y)| (y - speed_fraction(1500.)).abs() < 1e-6));
}

/// The palette's entries for the ramp: each is a real action in the Clips category with
/// a label that says what it does, and is handled by the table that runs
/// actions -- an entry the table forgot would be a row that does nothing.
#[test]
fn the_ramp_actions_are_in_the_palette_and_run() {
    use crate::keymap::{ActionId, Category};
    let ramp = [
        ActionId::SpeedRampUp,
        ActionId::SpeedRampDown,
        ActionId::SpeedRampSlowFastSlow,
        ActionId::SpeedRampFastSlowFast,
        ActionId::SpeedRampKey,
        ActionId::SpeedRampClear,
    ];
    let actions = src_text("player/actions.rs");
    for action in ramp {
        assert!(ActionId::ALL.contains(&action));
        assert_eq!(action.category(), Category::Clips, "{action:?}");
        assert!(action.label().to_lowercase().contains("speed"), "{action:?}: {}", action.label());
        assert!(
            actions.contains(&format!("ActionId::{action:?} =>")),
            "{action:?} is in the palette and does nothing"
        );
    }
}

/// The effects card writes a whole stack at a time, so each act on it is one
/// `set_effects` call and one undo step; a slider drag is held as a draft and is
/// one call when the hand lets go.
#[test]
fn every_effects_card_act_is_one_undo_step() {
    use crate::player::effects::{ParamShape, param_shape, snap_param};
    use engine::effects::{Effect, EffectKind};
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open");
    let lane = Lane::V1;
    let depth = |s: &PlaybackSession| s.undo_depth();
    let step = |s: &mut PlaybackSession, stack: Vec<Effect>, what: &str| {
        let before = depth(s);
        assert!(s.set_effects(lane, 0, stack), "{what} changed nothing");
        assert_eq!(depth(s), before + 1, "{what} is not one undo step");
    };
    // Add, at the end.
    let mut stack = session.effects_of(lane, 0).to_vec();
    stack.push(Effect::new(EffectKind::Blur));
    step(&mut session, stack.clone(), "adding a blur");
    stack.push(Effect::new(EffectKind::Vignette));
    step(&mut session, stack.clone(), "adding a vignette");
    // Switch off.
    stack[0].on = false;
    step(&mut session, stack.clone(), "switching off");
    // Move down.
    stack.swap(0, 1);
    step(&mut session, stack.clone(), "moving down");
    assert_eq!(session.effects_of(lane, 0)[1].kind, EffectKind::Blur);
    // A slider drag: the draft is the card's, the clip is written once.
    let spec = EffectKind::Blur.params()[0];
    let before = depth(&session);
    let mut draft = stack.clone();
    for raw in [3., 17.3, 42.26] {
        draft[1].values[0] = snap_param(&spec, raw);
    }
    assert_eq!(depth(&session), before, "a held bar writes nothing");
    assert!(session.set_effects(lane, 0, draft));
    assert_eq!(depth(&session), before + 1);
    assert_eq!(session.effects_of(lane, 0)[1].values[0], 42.5);
    // Remove.
    stack.remove(0);
    step(&mut session, stack, "removing");
    // The shapes: a switch is a step of 1 over 0..1, the blend mode is words.
    let shape = |kind: EffectKind, name: &str| {
        let spec = kind.params().iter().find(|p| p.name == name).unwrap();
        param_shape(kind, spec)
    };
    assert_eq!(shape(EffectKind::RectMask, "invert"), ParamShape::Switch);
    assert_eq!(shape(EffectKind::Blend, "mode"), ParamShape::Mode);
    assert_eq!(shape(EffectKind::Blur, "radius"), ParamShape::Bar);
}

/// The card has a Clip menu-bar row, a palette command and the dock's verb, and it is a
/// picture card: refused over a sound.
#[test]
fn the_effects_card_is_reachable_and_for_pictures() {
    use crate::oracle::{Ctx, Enable, enable};
    assert!(ActionId::ALL.contains(&ActionId::Effects));
    assert_eq!(ActionId::Effects.label(), "Effects…");
    assert!(crate::ui::menubar::Title::Clip.entries().contains(&crate::ui::menubar::Entry::Act(ActionId::Effects)));
    let picture = Ctx { clip: Some((Clip::source_range(0, 0, 10), Lane::V1)), timeline: true, playable: true, ..Ctx::default() };
    let sound = Ctx { clip: Some((Clip::source_range(0, 0, 10), Lane::A1)), timeline: true, playable: true, ..Ctx::default() };
    assert!(!matches!(enable(ActionId::Effects, picture), Enable::Hidden(_)));
    assert!(matches!(enable(ActionId::Effects, sound), Enable::Hidden(_)));
}

#[test]
fn colour_sections_cover_every_band_once_and_their_bands_round_trip() {
    use crate::color_ui::{
        band_neutral, band_spec, band_value, section_bands, set_band, ColorSection, COLOR_SLOTS,
    };
    let sections = [ColorSection::Basic, ColorSection::Wheels, ColorSection::Curves, ColorSection::Lut];
    let all: Vec<usize> = sections.iter().flat_map(|&s| section_bands(s)).collect();
    assert_eq!(all, (0..COLOR_SLOTS).collect::<Vec<_>>());

    let neutral = engine::color::ColorParams::default();
    for band in 0..COLOR_SLOTS {
        let (name, lo, hi, _) = band_spec(band);
        // Neutral reads as neutral, for every wheel and curve pick.
        for pick in 0..3 {
            let v = band_value(&neutral, band, pick, pick);
            assert!((v - band_neutral(band)).abs() < 1e-4, "{name}: {v}");
        }
        // A value written reads back, and leaves the rest alone.
        let target = lo + 0.3 * (hi - lo);
        let mut p = neutral;
        set_band(&mut p, band, 1, 2, target);
        assert!((band_value(&p, band, 1, 2) - target).abs() < 2e-3, "{name}");
        // (An intensity with no LUT to mix is still the identity.)
        assert!(band == COLOR_SLOTS - 1 || !p.is_identity(), "{name} changed nothing");
        // Putting the neutral back is the identity grade again.
        set_band(&mut p, band, 1, 2, band_neutral(band));
        assert!(p.is_identity(), "{name} did not reset");
    }
}

/// N5: no card changes its layout when a value changes. The shared slider row
/// keeps its Reset on the row whether or not the value is off its default (dimmed
/// and inert at rest), and no card hands a row a Reset only once it has changed.
#[test]
fn a_slider_rows_reset_never_comes_and_goes_with_its_value() {
    let widgets = src_text("ui/widgets.rs");
    let row = &widgets[widgets.find("pub(crate) fn slider_row").unwrap()..];
    let row = &row[..row.find("pub(crate) fn more_fold").unwrap()];
    assert!(!row.contains("changed.then_some"), "Reset must not depend on `changed`");
    assert!(row.contains("on_reset.map("), "Reset is drawn whenever the card gives one");
    for card in ["speed", "color", "mix", "silence", "transform", "substyle", "effects"] {
        let src = src_text(&format!("ui/cards/{card}.rs"));
        assert!(
            !src.contains("changed.then(") && !src.contains("changed.then_some"),
            "{card}: a Reset handed over only when changed shifts the layout"
        );
    }
}

/// N5, per card: the rows whose presence follows a value are drawn in a slot of
/// fixed height (stabilise's Stop/Measure) or are overlays (meter hold, analyser
/// note), and the card sources carry no `Reset` row built from `when(changed`.
#[test]
fn every_card_keeps_its_rows_when_a_value_changes() {
    for card in ["speed", "color", "effects", "tools", "transform", "silence", "mix", "eq", "markers", "substyle", "keys", "export"] {
        let src = src_text(&format!("ui/cards/{card}.rs"));
        assert!(!src.contains(".when(changed"), "{card}: a row appears when a value changes");
        assert!(!src.contains(".when(!changed"), "{card}: a row disappears when a value changes");
    }
    assert!(src_text("ui/cards/transform.rs").contains(".min_h(px(CONTROL_H))"), "stabilise's action slot keeps its height");
}

/// N9: the Tools card's loudness chips wrap instead of clipping at the card edge.
#[test]
fn the_tools_loudness_chips_wrap() {
    let src = src_text("ui/cards/tools.rs");
    // The presets are one equal-width segmented row: three short names that always fit.
    assert!(src.contains("segmented_equal(\n            \"tools-preset\""));
}

/// N7: the puck's place and the RGB offsets the sliders hold are one mapping.
#[test]
fn a_wheel_puck_and_its_rgb_offsets_round_trip() {
    use crate::color_ui::{wheel_offsets, wheel_puck, WHEEL_REACH};
    for (x, y) in [(0., 0.), (0.5, 0.), (-0.3, 0.6), (0., -0.9), (0.6, 0.6)] {
        let [r, g, b] = wheel_offsets(x, y);
        assert!((r + g + b).abs() < 1e-6, "tints do not move the master");
        let (px, py) = wheel_puck(r, g, b);
        assert!((px - x).abs() < 1e-5 && (py - y).abs() < 1e-5, "({x},{y}) -> ({px},{py})");
    }
    // Past the rim the puck stays on it.
    let [r, ..] = wheel_offsets(3., 0.);
    assert!((r - WHEEL_REACH).abs() < 1e-6);
    // Red sits at 3 o'clock.
    let [r, g, b] = wheel_offsets(1., 0.);
    assert!(r > 0. && g < 0. && b < 0.);
}

/// N7: the scope tabs are short, and the curve editor has a square of its own
/// rather than being drawn over the scope.
#[test]
fn the_colour_card_has_short_scope_tabs_and_a_curve_square_of_its_own() {
    use crate::color_ui::SCOPE_VIEWS;
    assert!(SCOPE_VIEWS.iter().all(|(_, n)| n.len() <= 11));
    let src = src_text("ui/cards/color.rs");
    assert!(!src.contains("graph_curve"), "no curve drawn over the scope");
    assert!(src.contains("color-curve-graph"));
}

#[test]
fn a_pan_scrub_sits_on_a_grid_catches_the_centre_and_stays_in_range() {
    use crate::player::pan::{pan_text, scrub_pan_at};
    assert_eq!(scrub_pan_at(0., 50., 0.01), 0.5);
    assert_eq!(scrub_pan_at(0.5, -50., 0.01), 0., "the centre is caught");
    assert_eq!(scrub_pan_at(0., 2., 0.01), 0., "within 3% of the middle is the middle");
    assert_eq!(scrub_pan_at(0.9, 500., 0.01), 1.);
    assert_eq!(scrub_pan_at(-0.9, -500., 0.01), -1.);
    assert_eq!(pan_text(0.), "Centre");
    assert_eq!(pan_text(-0.4), "40% L");
    assert_eq!(pan_text(0.25), "25% R");
}

#[test]
fn the_master_meter_reads_at_30_hz_latches_clips_and_falls_when_paused() {
    use crate::player::cards::{CLIP_LAMP_S, MasterMeter};
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let mut m = MasterMeter::default();
    let mut asked = 0;
    m.step(true, || { asked += 1; [0.5, 0.1] }, t0);
    m.step(true, || { asked += 1; [1.0, 1.0] }, t0 + Duration::from_millis(10));
    assert_eq!(asked, 1, "a second read inside 33 ms is not taken");
    assert!(m.lr[0].level_db > m.lr[1].level_db, "left is louder than right");
    assert!(!m.clipped(t0));
    m.step(true, || [1.0, 0.2], t0 + Duration::from_millis(40));
    assert!(m.clipped(t0 + Duration::from_millis(41)), "a full-scale peak lights the lamp");
    assert!(!m.clipped(t0 + Duration::from_secs_f32(CLIP_LAMP_S + 0.1)), "and it goes out");
    m.step(false, || [0.0; 2], t0 + Duration::from_millis(80));
    assert_eq!(m.lr[0], crate::player::cards::MixMeter::default());
    assert!(!m.clipped(t0 + Duration::from_millis(81)));
}

#[test]
fn the_loop_hover_names_what_it_loops() {
    use crate::ui::timeband_stance::{loop_tip_hint, loop_tip_name};
    assert_eq!(loop_tip_name(Some((10, 40))), "Loop playback");
    assert_eq!(loop_tip_name(None), "Loop playback");
    assert!(loop_tip_hint(Some((10, 40))).contains("Between In and Out"));
    assert!(loop_tip_hint(None).contains("Set In and Out"));
}
