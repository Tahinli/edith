//! The cards the window opens over itself: the keys, the equaliser, the
//! colour and speed bars, the silence scan, the choices and the export.

use super::*;
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
    let render_body = &stance[stance.find("pub(crate) fn render(").expect("the stance's entry point")..];
    for menu in ["context_card(", "library_card(", "picker_card("] {
        assert!(
            render_body.contains(menu),
            "ui::stance::render never mounts Player::{menu} -- overlaid() can refuse \
             every key for a menu the room never draws"
        );
    }
    let guard_at = stance
        .find("if this.rebinding.is_some() || this.overlaid() {")
        .expect("the stance's modal guard");
    let guard = &stance[guard_at..guard_at + 1600];
    for field in ["this.context_menu = None", "this.library_menu = None", "this.picker = None"] {
        assert!(
            guard.contains(field),
            "the overlaid()/rebinding branch no longer clears {field} on every key -- \
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
    let modal_body = fn_body("modal");
    let mut fields = Vec::new();
    for chunk in modal_body.split("self.").skip(1) {
        let name: String = chunk
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !fields.contains(&name) {
            fields.push(name);
        }
    }
    assert!(fields.len() >= 9, "modal()'s own field list looks too short to have been parsed: {fields:?}");
    // The mounted-surface function each field's own card draws, by the
    // convention the param cards already follow (`x_open` -> `x_card(` --
    // `Player::eq_card`/`color_card`/... in `ui/cards.rs`). `keys_open` and
    // `export_open`/`exporting` don't follow that convention (the keys
    // overlay isn't a "card", export pairs with the running-export sheet),
    // so they're named explicitly. Anything else unmapped panics loudly --
    // the point is that a ninth card has to earn a line here, not just a
    // mount, or this test cannot see it.
    let mount_for = |field: &str| -> &'static str {
        match field {
            "keys_open" => "keys_overlay(",
            "export_open" => "export_card(",
            "exporting" => "export_progress_card(",
            "eq_open" => "eq_card(",
            "color_open" => "color_card(",
            "transform_open" => "transform_card(",
            "speed_open" => "speed_card(",
            "silence_open" => "silence_card(",
            "mix_open" => "mix_card(",
            "subtitle_style_open" => "subtitle_style_card(",
            other => panic!(
                "Player::modal() now reads `self.{other}` with no entry in this test's \
                 mount_for map -- add one naming the function that mounts its surface in \
                 the darkroom (ui/stance.rs or ui/dock_stance.rs), or the room can go modal \
                 over it with nothing drawn (the silence/mix/subtitle-style bug, GAP 2)."
            ),
        }
    };
    let haystack = format!("{}\n{}", src_text("ui/stance.rs"), src_text("ui/dock_stance.rs"));
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

#[test]
fn a_bare_escape_leaves_player_fullscreen_before_anything_under_it() {
    // Fires while the picture-only layout is up, on the same bare escape
    // every menu and preview close on -- and only that key: a chord, or an
    // escape while not fullscreen, means nothing to it.
    assert!(escape_leaves_player_fullscreen("escape", false, true));
    assert!(!escape_leaves_player_fullscreen("escape", false, false));
    assert!(!escape_leaves_player_fullscreen("escape", true, true));
    assert!(!escape_leaves_player_fullscreen("q", false, true));
}

#[test]
fn only_a_chord_gets_out_of_an_export() {
    use keymap::ActionId;
    // What this guards: bare escape is the stroke a hand throws at anything on
    // screen, and while an export ran it deleted the encode. The way out is the
    // same key with control on it, and the card says so.
    assert!(cancels_export("escape", true, Some(ActionId::CancelExport)));
    assert!(!cancels_export("escape", false, None));
    // Even if something else were ever bound to bare escape, it is not this.
    assert!(!cancels_export("escape", false, Some(ActionId::Play)));
    // A rebound cancel works as well -- it adds a way out, never replaces the
    // chord, and the keymap is what decides whether that one carries control.
    assert!(cancels_export("q", false, Some(ActionId::CancelExport)));
    // Nothing else does, whatever it means outside an export.
    assert!(!cancels_export("e", false, Some(ActionId::Export)));
    assert!(!cancels_export("space", false, Some(ActionId::Play)));
    assert!(!cancels_export("q", false, None));
    // The default keymap is what the handler feeds this: the chord reaches the
    // action, the bare key reaches nothing.
    let k = keymap::Keymap::defaults();
    assert!(cancels_export("escape", true, k.lookup("escape", true)));
    assert!(!cancels_export("escape", false, k.lookup("escape", false)));
}

#[test]
fn a_capture_waits_through_a_lone_modifier() {
    // gpui delivers these on their own; taking one as a binding would make
    // the action fire on the way to every chord that uses it.
    for key in [
        "control", "shift", "alt", "super", "platform", "function", "fn", "meta", "command",
    ] {
        assert!(is_bare_modifier(key), "{key}");
    }
    // Everything a binding is actually made of, escape included -- the
    // capture branch turns that one away itself.
    for key in ["c", "x", "space", "escape", "delete", "f1", "z"] {
        assert!(!is_bare_modifier(key), "{key}");
    }
}

/// The whole point of the card: there is no action a pointer cannot reach.
/// It renders [`keys_rows`] and nothing else, so this reads the same list
/// the card does -- add an `ActionId` and forget to surface it and this
/// fails, which is the only way that stays true as the editor grows.
#[test]
fn every_action_is_on_the_actions_card() {
    use keymap::{ActionId, Category, Keymap};
    let rows = keys_rows();
    let listed: Vec<ActionId> = rows
        .iter()
        .filter_map(|r| match r {
            KeyRow::Act(a) => Some(*a),
            _ => None,
        })
        .collect();
    for action in ActionId::ALL {
        assert_eq!(
            listed.iter().filter(|a| **a == action).count(),
            1,
            "{action:?} is not on the card exactly once"
        );
    }
    assert_eq!(listed.len(), ActionId::ALL.len());
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
    let keymap = Keymap::defaults();
    for action in ActionId::ALL {
        assert!(!action.label().is_empty(), "{action:?}");
        assert_ne!(keymap.display(action), "unbound", "{action:?}");
    }
    // The list scrolls inside a card the smallest window holds, so a
    // thirty-fourth action costs no height at all.
    assert!(rows.len() as f32 * KEYS_ROW_H > KEYS_ROWS_H, "no cap needed?");
    // Both halves of a row are click targets, so WCAG 2.5.8 binds them.
    assert!(KEYS_ROW_H >= HIT_MIN);
}

/// The search box: what a typed word leaves standing. Forty actions is more
/// than a 360 px window shows at once, so this is the card's answer to
/// "where is the one I want" -- and a heading with nothing under it would
/// be worse than no filter at all.
#[test]
fn a_search_leaves_the_rows_it_names_under_their_own_headings() {
    use keymap::{ActionId, Category, Keymap};
    let keymap = Keymap::defaults();
    let acts = |found: &[(usize, KeyRow)]| -> Vec<ActionId> {
        found
            .iter()
            .filter_map(|(_, r)| match r {
                KeyRow::Act(a) => Some(*a),
                _ => None,
            })
            .collect()
    };
    let heads = |found: &[(usize, KeyRow)]| -> Vec<Category> {
        found
            .iter()
            .filter_map(|(_, r)| match r {
                KeyRow::Head(c) => Some(*c),
                _ => None,
            })
            .collect()
    };
    // Nothing typed hides nothing, and every row keeps its place in the
    // unfiltered list: an element id must not move under a keystroke.
    let all = keys_filter("", &keymap);
    assert_eq!(all.len(), keys_rows().len());
    assert!(all.iter().enumerate().all(|(n, (i, _))| n == *i));
    // The word the user types is the word on the row. The mix card names
    // the track volumes, so it is an honest hit; the fixed row about the
    // volume keys is another, and each comes with its own heading only.
    let vol = keys_filter("vol", &keymap);
    assert_eq!(
        acts(&vol),
        vec![ActionId::VolumeUp, ActionId::VolumeDown, ActionId::Mix]
    );
    assert_eq!(heads(&vol), vec![Category::Audio, Category::View]);
    // Case is not part of the question, in either direction.
    assert_eq!(keys_filter("VoL", &keymap).len(), vol.len());
    // The stroke column is searched too -- "what did ctrl do again" -- and
    // an unbound-looking word finds nothing rather than everything.
    let ctrl = keys_filter("ctrl+", &keymap);
    assert!(acts(&ctrl).contains(&ActionId::Save));
    assert!(!acts(&ctrl).contains(&ActionId::Play));
    assert!(
        keys_filter("qzx", &keymap).is_empty(),
        "headings left behind"
    );
    // The card's own door is on the card, by name and by stroke.
    assert_eq!(
        acts(&keys_filter("?", &keymap)),
        vec![ActionId::ShowActions]
    );
    assert_eq!(
        acts(&keys_filter("all actions", &keymap)),
        vec![ActionId::ShowActions]
    );
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
        title + status + search + separator + KEYS_ROWS_H + gaps + padding <= 360.,
        "card too tall"
    );
    // ...and the list is the only part that grows with the editor, so the
    // rows past the fold are reached by scrolling that viewport (and, with
    // forty of them, by the search box above it) rather than by a card
    // taller than the window.
    assert!(
        keys_rows().len() as f32 * KEYS_ROW_H > KEYS_ROWS_H,
        "the list outgrew the viewport long ago; the cap must still scroll"
    );
    // The cap is only honest if it is the taller list that scrolls, not the
    // card that grows: every action must be reachable by scrolling, and
    // enough of them visible that the list reads as a list.
    assert!(
        KEYS_ROWS_H / KEYS_ROW_H >= 8.,
        "too few rows visible to scan"
    );
    assert!(KEYS_W <= 640., "card too wide");
    // The rows are clickable, so WCAG 2.5.8 binds them like every other
    // target in this window.
    assert!(KEYS_ROW_H >= HIT_MIN);
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
    assert!(eq_card_w(640.) <= 640. - 24., "card too wide for 640");
    assert!(eq_card_w(1280.) > eq_card_w(640.), "card ignores the window");
    assert_eq!(eq_card_w(1920.), EQ_W_MAX, "card grows without end");
    assert!(eq_card_w(320.) >= KEYS_W, "card narrower than a row of text");
    // At the smallest window the graph is still a graph: three across for
    // one down, so an octave is wide enough to put a handle in.
    assert!(
        eq_card_w(640.) - 24. >= 3. * EQ_GRAPH_H,
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
    assert!(
        EQ_GRAPH_H <= EXPORT_ROWS_H,
        "graph taller than a card of rows"
    );
    // What is dragged is the whole graph -- the handle is a 10 px dot, but
    // a press anywhere in the box takes the band nearest it -- so WCAG
    // 2.5.8 is satisfied by the box, which is far past the minimum.
    assert!(EQ_GRAPH_H >= HIT_MIN);
    assert!(
        EQ_HANDLE < HIT_MIN,
        "a dot that size would want its own hitbox"
    );
    assert!(KEYS_ROW_H >= HIT_MIN);
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
    assert!(
        EQ_BANDS_MAX <= 10,
        "a band past the tenth has no key that picks it"
    );
    assert!(EQ_BANDS_MAX > EqParams::default_layout().bands.len());
    // The Q range holds the default, so a file's band never opens out of
    // range and needs dragging back in before it can be edited.
    assert!((EQ_Q_LOW..=EQ_Q_HIGH).contains(&0.707));
    assert!(EQ_Q_STEP > 1.);
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
    let (lo, hi) = (
        f32::from(Speed::MIN.permille()),
        f32::from(Speed::MAX.permille()),
    );
    // The same arithmetic `Player::drag_speed` runs, which is the one thing
    // a test of it can share without re-deriving it.
    let at = |x: f32| {
        let raw = lo + frac_along(px(x), bar) * (hi - lo);
        speed_at((raw / SPEED_STEP as f32).round() as i32 * SPEED_STEP)
    };
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
        // within the half step the snap costs.
        let painted = (f32::from(speed.permille()) - lo) / (hi - lo);
        let slack = SPEED_STEP as f32 / (hi - lo) / 2. + 1e-4;
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

    // The same shape as the other two cards, so it fits where they do: the
    // graph, four rows and the reset button inside a 360 px window.
    let (title, status, gaps, padding) = (17., 17., 6. * 2., 24.);
    let rows = COLOR_BANDS.len() as f32 * KEYS_ROW_H;
    assert!(
        title + status + HIST_H + rows + gaps + padding + CONTROL_H + 4. <= 360.,
        "card too tall"
    );
    assert!(COLOR_W <= 640., "card too wide");
    // The label still has room beside the bar and the readout, which is
    // what the buttons coming off the row bought.
    let row = COLOR_W - padding - 12. - 2. * 8. - COLOR_BAR_W - 44.;
    assert!(row >= LABEL_MIN_W, "no room left for a label: {row}px");
    // What is dragged is the whole row's height, not the 4 px the bar is
    // drawn as (WCAG 2.5.8) -- the same split the ruler makes.
    assert!(KEYS_ROW_H >= HIT_MIN);
}

/// The graph over the sliders is the frame the grade already went through,
/// so it has to count what is actually in those bytes -- BGRA on the wire,
/// red-green-blue in the bins.
#[test]
fn the_histogram_counts_the_frame_it_is_handed() {
    // Half pure red, half mid grey: two known values, in two known bins.
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
    // 64 bins over 256 codes: 255 is the last bin, 128 the middle one, 0 the
    // first.
    assert_eq!(bins[0][63], half, "the red half tops the red channel");
    assert_eq!(bins[0][32], half, "and the grey half sits mid red");
    for channel in [1, 2] {
        assert_eq!(bins[channel][0], half, "no green or blue in the red half");
        assert_eq!(bins[channel][32], half);
        assert_eq!(bins[channel][63], 0);
    }
    // Nothing is counted twice and nothing is dropped: this frame is small
    // enough to be read whole.
    for channel in bins {
        assert_eq!(channel.iter().sum::<u32>(), (w * h) as u32);
    }

    // A grade shifts it, which is the whole point of drawing it: the same
    // frame darkened lands in lower bins.
    let darker: Vec<u8> = frame.iter().map(|b| b / 2).collect();
    let bins = histogram(&darker);
    assert_eq!(bins[0][31], half, "255 -> 127");
    assert_eq!(bins[0][16], half, "128 -> 64");

    // A real frame is subsampled: a 1080p one is read every 253rd pixel, so
    // the shape costs a thousandth of the reads and still counts thousands.
    let big = vec![200u8; 1920 * 1080 * 4];
    let bins = histogram(&big);
    let counted = bins[0].iter().sum::<u32>();
    let pixels = 1920 * 1080usize;
    let expected = pixels.div_ceil(pixels / HIST_SAMPLES) as u32;
    assert_eq!(counted, expected, "every strided pixel counted, once");
    assert!(
        (HIST_SAMPLES as u32..=HIST_SAMPLES as u32 + 64).contains(&counted),
        "{counted} samples is not the budget"
    );
    assert_eq!(bins[0][200 * HIST_BINS / 256], counted, "all in one bin");

    // An empty buffer is a flat graph rather than a panic: the card is open
    // before the first frame is pumped.
    assert_eq!(histogram(&[]), [[0; HIST_BINS]; 3]);
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
    // rather than as a hang.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut state = transport(session.is_playing(), session.is_eos());
    while state != Transport::Ended {
        assert!(Instant::now() < deadline, "never reached the end of a 5s file");
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
fn a_quality_row_is_the_bitrate_it_promises() {
    // Auto is the one row that says nothing: the exporter derives it, and
    // a number typed against the custom row must not leak into it.
    let mp4 = Format::Mp4;
    assert_eq!(
        export_settings(Quality::Auto, 7, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).bitrate,
        None
    );
    assert_eq!(
        export_settings(Quality::Low, 0, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).bitrate,
        Some(2_000_000)
    );
    assert_eq!(
        export_settings(Quality::Medium, 0, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).bitrate,
        Some(6_000_000)
    );
    assert_eq!(
        export_settings(Quality::High, 0, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).bitrate,
        Some(12_000_000)
    );
    // Megabits as typed, and as the row says it back.
    assert_eq!(
        export_settings(Quality::Custom, 7, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).bitrate,
        Some(7_000_000)
    );
    assert_eq!(Quality::Low.detail(0), "2 Mbps");
    // The picked format travels, or the card's rows would be a picture of a
    // choice the engine never hears about.
    for format in [Format::Mp4, Format::Wav, Format::Flac] {
        assert_eq!(
            export_settings(Quality::Auto, 0, format, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto).format,
            format
        );
    }
    // Every fixed row sits inside the engine's clamp
    // (`MAX_EXPLICIT_BITRATE`), so no row can promise a bitrate the exporter
    // silently changes -- the ceiling row included, which is the one a
    // raised cap could have walked out past.
    for quality in Quality::ALL {
        let settings = export_settings(quality, MBPS_MAX, mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto);
        if let Some(bitrate) = settings.bitrate {
            assert!(
                (u64::from(MBPS_MIN) * 1_000_000..=u64::from(MBPS_MAX) * 1_000_000)
                    .contains(&bitrate),
                "{quality:?} outside the engine clamp"
            );
        }
        // The seat travels exactly as it was picked, and an unpicked project
        // exports on the seat this machine has.
        assert_eq!(settings.seat, EncoderSeat::Auto);
    }
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
                export_settings(Quality::Auto, 0, format, kbps, EncoderSeat::Auto).audio_kbps,
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
        assert_eq!(label.as_ref(), format!("{kbps} kbps"));
        assert!(detail.chars().count() <= 26, "{detail} is too long to read");
        assert_eq!(*picked, kbps == DEFAULT_AUDIO_KBPS);
    }
    // A rate no row holds marks none of them, rather than the wrong one.
    assert!(audio_rate_choices(7).iter().all(|(.., picked)| !picked));

    // The wrap the row's key does, which is the row's own step function.
    assert_eq!(
        next_audio_kbps(AUDIO_KBPS[AUDIO_KBPS.len() - 1]),
        AUDIO_KBPS[0]
    );
    assert_eq!(next_audio_kbps(DEFAULT_AUDIO_KBPS), 320);
    // A rate no list holds (a stale one, say) lands back on the first row
    // rather than nowhere.
    assert_eq!(next_audio_kbps(7), AUDIO_KBPS[1]);
}

#[test]
fn a_typed_bitrate_is_a_field_and_not_a_key_capture() {
    // It opens on the number in force, so backspace edits that number
    // rather than the field starting empty over a bitrate still being used.
    let mut edit = NumberEdit::new(12);
    assert_eq!(edit.text, "12");
    edit.backspace();
    edit.digit(8);
    assert_eq!(edit.text, "18");
    assert_eq!(edit.commit(), Some(18));
    // A card nobody has typed a number into opens empty: zero is not a
    // bitrate anyone chose.
    assert_eq!(NumberEdit::new(0).text, "");

    // Out of range is refused *in words* and the digits stay put: clamping
    // 55 to 50 would write a bitrate the user never typed.
    let mut edit = NumberEdit::new(0);
    for digit in [5, 5] {
        edit.digit(digit);
    }
    assert_eq!(edit.commit(), None);
    assert_eq!(edit.text, "55", "a refusal keeps what was typed");
    let refusal = edit.refusal.clone().expect("a refusal says why");
    assert!(refusal.contains(&MBPS_MAX.to_string()), "{refusal}");
    assert!(edit.detail().starts_with("55▏"), "{}", edit.detail());
    assert!(edit.detail().contains(&refusal));
    // And is fixable in place, which is the whole point of a field.
    edit.backspace();
    assert_eq!(edit.refusal, None, "the reason went with the digit");
    assert_eq!(edit.commit(), Some(5));

    // Empty, zero, and past the digit cap: each its own reason, none of
    // them silent.
    assert!(commit_mbps("").is_err());
    assert!(commit_mbps("0").unwrap_err().contains("not a rate"));
    assert_eq!(commit_mbps("1"), Ok(MBPS_MIN));
    assert_eq!(commit_mbps("50"), Ok(MBPS_MAX));
    assert!(commit_mbps("51").is_err());
    let mut edit = NumberEdit::new(0);
    for digit in [9, 9, 9, 9] {
        edit.digit(digit);
    }
    assert_eq!(edit.text, "999", "the cap holds");
    assert!(edit.refusal.is_some(), "and says it is holding");
    // Never past what a u64 bitrate can be built from -- the committed
    // number is the only one that reaches the engine, and it is bounded.
    assert!(u64::from(MBPS_MAX) * 1_000_000 < u64::from(u32::MAX));
    assert_eq!(MBPS_DIGITS, 3);

    // The arrows step inside the range and stop at both ends: a walk
    // through the legal numbers, never a way out of them.
    let mut edit = NumberEdit::new(0);
    edit.step(1);
    assert_eq!(edit.text, MBPS_MIN.to_string(), "empty starts at the floor");
    edit.step(-1);
    assert_eq!(edit.text, MBPS_MIN.to_string());
    let mut edit = NumberEdit::new(MBPS_MAX);
    edit.step(1);
    assert_eq!(edit.text, MBPS_MAX.to_string());
    edit.step(-1);
    assert_eq!(edit.text, (MBPS_MAX - 1).to_string());
    // A step past a refused number clears the refusal with it.
    let mut edit = NumberEdit::new(0);
    edit.digit(5);
    edit.digit(5);
    assert_eq!(edit.commit(), None);
    edit.step(-1);
    assert_eq!(edit.refusal, None);
    assert_eq!(edit.text, MBPS_MAX.to_string(), "back inside the range");

    // The hint the field shows when there is nothing to refuse names both
    // ways out of it.
    let detail = NumberEdit::new(6).detail();
    assert!(detail.contains("enter") && detail.contains("esc"), "{detail}");
    assert!(detail.starts_with("6▏"), "{detail}");
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
                ActionId::VolumeUp
                    | ActionId::VolumeDown
                    | ActionId::ZoomIn
                    | ActionId::ZoomOut
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
    // title and a hint over its [`SILENCE_ROWS`] rows, the count line and
    // the two buttons.
    let (title, hint, count) = (17., 17., 17.);
    let gaps = 6. * 5.;
    let padding = 24.;
    assert!(
        HEADER_H
            + 8.
            + title
            + hint
            + SILENCE_ROWS as f32 * KEYS_ROW_H
            + count
            + KEYS_ROW_H
            + gaps
            + padding
            <= 360.,
        "card too tall"
    );
    // Its rows, its steppers and its buttons are clicked, so WCAG 2.5.8
    // binds them: a stepper is `HIT_MIN` square inside a row of that height.
    assert!(KEYS_ROW_H >= HIT_MIN);
    // ...and the pair of them fits beside the widest value the card prints.
    assert!(2. * HIT_MIN + 4. < COLOR_W / 2., "steppers crowd the value");
    // A "speed-up" is never a slow-down: the rate stops above real time at
    // one end and at what a clip can hold at the other, whatever the keys
    // ask for. A silence played *slower* would make the timeline longer,
    // which is the one thing neither button may do.
    assert!(silence_rate(0) > Speed::NORMAL);
    assert!(silence_rate(1000) > Speed::NORMAL);
    assert_eq!(silence_rate(i32::MAX), Speed::MAX);
    assert_eq!(silence_rate(4000), Speed::MAX);
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
            (1440, 1080),
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
        assert_eq!(label.as_ref(), format!("{}p", size.1));
        assert!(detail.contains(&format!("{}x{}", size.0, size.1)));
        assert_eq!(*picked, *size == (1280, 720));
    }
    // The media's own size says so: it is the one rung a person cannot read
    // off a number they chose.
    let (.., native_detail, _) = &rows[3];
    assert!(
        native_detail.contains("the media's own"),
        "{native_detail}"
    );
    // A project at a size nobody listed still gets the whole list, with
    // nothing marked rather than a wrong row marked.
    assert!(
        resolution_choices((1000, 1000), native)
            .iter()
            .all(|(.., picked)| !picked)
    );
    // Picking a row means that row, and stepping means the next one: the
    // list and the stroke read the same ladder.
    assert_eq!(next_resolution(ladder[1], native), ladder[2]);

    // The fit list, on a clip: all four policies, in the order the stroke
    // steps through them, the clip's own marked and every row naming the
    // canvas it would place the picture on.
    let mut fit = FITS[0];
    for next in FITS.into_iter().skip(1).chain([FITS[0]]) {
        assert_eq!(next_fit(fit), next, "the stroke skips a policy");
        fit = next;
    }
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
    assert_eq!(fps[0].1.as_ref(), "23.976 fps");
    assert!(fps[0].3, "the rate in force is not marked");
    assert!(fps[5].2.contains("the media's own"), "{}", fps[5].2);
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
            assert_eq!(av1_hw_warning(format, seat), None, "{seat:?} is the safe seat");
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
    assert!(MENU_ROW_H >= HIT_MIN);
    assert!(odd.len() > ladder.len(), "the longest list moved");
    assert!(MENU_PAD * 2. + odd.len() as f32 * MENU_ROW_H <= 360.);
    let tall = MENU_PAD * 2. + ladder.len() as f32 * MENU_ROW_H;
    assert!(tall <= 360., "the list is taller than the floor");
    assert_eq!(
        menu_at(point(px(600.), px(340.)), size(px(640.), px(360.)), tall),
        (640. - MENU_W, 360. - tall),
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
    assert!(none.iter().all(|(.., picked)| !picked), "nothing picked yet");
    for ((choice, label, detail, _), size) in none.iter().zip(RESOLUTIONS) {
        assert_eq!(*choice, Choice::Size(size.0, size.1));
        assert_eq!(label.as_ref(), format!("{}p", size.1));
        assert_eq!(detail.as_ref(), format!("{}x{}", size.0, size.1));
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

#[test]
fn the_export_card_fits_the_smallest_window() {
    // Same 640x360 floor the keybindings card is measured against: the
    // capped row list, the two summary lines and the confirm button, under
    // a title and a status line.
    let title = 17.;
    let status = 28.;
    // The head is one line of 11 px at this width -- every field of it,
    // worst case, is 71 characters against the 76 that fit. The tail is
    // budgeted for two: the destination's name is the user's and a long one
    // wraps.
    let summary = 15. + 30.;
    // Six children in the column, so five gaps.
    let gaps = 5. * 2.;
    let padding = 24.;
    assert_eq!(
        EXPORT_FIXED_H,
        title + status + summary + CONTROL_H + 4. + gaps + padding
    );
    assert!(EXPORT_FIXED_H + EXPORT_ROWS_H <= 360., "card too tall");
    // The list grows with a window that has the room -- and never shrinks
    // below the cap that made the floor fit, whatever arithmetic the window
    // hands it.
    let cap = |h: f32| (h - EXPORT_FIXED_H - 24.).max(EXPORT_ROWS_H);
    assert_eq!(cap(360.), EXPORT_ROWS_H);
    assert_eq!(cap(0.), EXPORT_ROWS_H);
    assert!(cap(720.) > EXPORT_ROWS_H);
    assert!(EXPORT_FIXED_H + cap(720.) <= 720.);
    // ...and inside the 640 px floor with the scrim showing either side.
    assert!(EXPORT_W + 2. * 12. <= 640.);
    // The cap is only honest if enough of the list is on screen to read as
    // one -- and the whole format section is: its header and every codec
    // row, so nothing that is picked *first* is behind a scroll.
    let codecs = FORMATS.iter().filter(|(row, ..)| !row.is_empty()).count();
    assert!(EXPORT_ROWS_H / KEYS_ROW_H >= 1. + codecs as f32);
    // Clickable rows, so WCAG 2.5.8 binds them as it binds the panel's --
    // and the bitrate steppers are `HIT_MIN` squares sitting inside a row,
    // which only fits while the row is at least as tall as one.
    assert!(KEYS_ROW_H >= HIT_MIN);
    assert!(CONTROL_H >= HIT_MIN);
    // The dimmed text on the card -- every refusal, every detail, every key
    // in its column -- is body text on `BG_RAISED` and WCAG 1.4.3 binds it.
    // A dimmed row is drawn in this ink rather than at an opacity, which is
    // what a refusal used to be readable through.
    // Every palette, not the one in force: a family is picked at runtime
    // now (`ui::theme`), so a floor met by one of them and missed by the
    // other is a window somebody is looking at.
    for id in crate::ui::theme::PaletteId::ALL {
        let p = id.palette();
        assert!(
            contrast(p.FG_SECONDARY, p.BG_RAISED) >= 4.5,
            "{id:?}: refusal ink {:.2}",
            contrast(p.FG_SECONDARY, p.BG_RAISED)
        );
    // ...and on the picked row, where the highlight is the accent at
    // surface brightness. Both inks clear 4.5:1 on it now -- the row still
    // lifts its key and detail to `FG_PRIMARY`, as emphasis rather than as
    // the rescue it used to be.
        assert!(contrast(p.FG_PRIMARY, p.BG_SELECTED) >= 4.5, "{id:?}");
        assert!(
            contrast(p.FG_SECONDARY, p.BG_SELECTED) >= 4.5,
            "{id:?}: refusal ink on the picked row {:.2}",
            contrast(p.FG_SECONDARY, p.BG_SELECTED)
        );
    }
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
                if elapsed >= 5. && elapsed < 20. {
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

/// The primary pane's own round trip: every bundle's format-and-quality lands
/// back on the preset that named it, so the row a person picked is the row
/// the card shows picked. `Custom` is the one preset `bundle` names nothing
/// for, and the one every pair outside the other four's must fall back to.
#[test]
fn every_preset_bundle_round_trips_through_from_state() {
    for preset in ExportPreset::ALL {
        match preset.bundle() {
            Some((format, quality)) => {
                assert_eq!(ExportPreset::from_state(format, quality), preset);
            }
            None => assert_eq!(preset, ExportPreset::Custom),
        }
    }
    // A pair no bundle names -- the launch default before this batch's fix --
    // reads as `Custom`, not as whichever preset happens to be first.
    assert_eq!(
        ExportPreset::from_state(Format::Mp4, Quality::Auto),
        ExportPreset::Custom
    );
    // Master's bundle is HEVC/High, not H.264: an intra-only master is the one
    // that keeps its own "for re-editing later" detail true.
    assert_eq!(
        ExportPreset::Master.bundle(),
        Some((Format::HevcMp4, Quality::High))
    );
}

/// The primary pane's own refusal: a bundle whose format this timeline cannot
/// write reads exactly as a codec row does -- the reason in place of the
/// detail, and no format-and-quality pair to land wrong once a click on a
/// dimmed row is ignored.
#[test]
fn a_preset_over_a_picture_this_timeline_has_none_of_carries_the_codec_refusal() {
    let mut session =
        PlaybackSession::open(asset("test_tone.mp3")).expect("a song is a timeline");
    session.set_gain(0.0);
    let path = session.sources()[0].path.clone();
    session.seek(1.0);
    session
        .place_stream_at(1.0, &path, 0, Some(Lane::A1))
        .expect("its own file is on this timeline");
    assert!(session.lane_clips(Lane::V1).is_empty(), "still no picture");

    for preset in [ExportPreset::Web, ExportPreset::Small, ExportPreset::Master] {
        let (format, _) = preset.bundle().expect("a real bundle");
        assert!(
            format_refusal(&session, format).is_some(),
            "{preset:?}'s bundle is a picture format this audio-only timeline must refuse"
        );
    }
    // Audio only's bundle is FLAC, which has no picture to refuse.
    let (audio_format, _) = ExportPreset::AudioOnly.bundle().expect("a real bundle");
    assert_eq!(format_refusal(&session, audio_format), None);
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
            body.contains("self.close_card();"),
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
    assert!(tip_may_paint(false), "with nothing overlaid, an ordinary tip paints too");

    // Structural half: every card routes its head through `dark_card_head`,
    // so checking its one help-tooltip call site covers all seven cards (and
    // any future eighth) at once -- it must build an `OverlayTip`, not a
    // plain `Tip`, or this whole fix is undone by construction.
    let cards_src = src_text("ui/cards.rs");
    let head_fn_at = cards_src.find("fn dark_card_head(").expect("dark_card_head");
    let head_fn = &cards_src[head_fn_at..head_fn_at + 1200];
    assert!(
        head_fn.contains("OverlayTip(h.clone())"),
        "dark_card_head's own `?` tooltip no longer builds an OverlayTip -- the \
         card-head help is unreadable again while its card is open"
    );
}
