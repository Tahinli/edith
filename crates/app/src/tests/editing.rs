//! Importing, the library column, the menus, and every edit a clip or a
//! track can be handed.

use super::*;

/// What the file manager is handed: the parts a path keeps as they are, and
/// the ones the bus would otherwise read as something else.
#[test]
fn file_uri_encodes_what_a_path_carries() {
    assert_eq!(
        file_uri(std::path::Path::new("/a/b.mp4")),
        "file:///a/b.mp4"
    );
    assert_eq!(
        file_uri(std::path::Path::new("/home/x/out dir/my export.mp4")),
        "file:///home/x/out%20dir/my%20export.mp4"
    );
    assert_eq!(
        file_uri(std::path::Path::new("/tmp/ünlü#1?.mp4")),
        "file:///tmp/%C3%BCnl%C3%BC%231%3F.mp4"
    );
}

/// An import fills the *library* and nothing else -- the whole point of
/// this door: the row is there at the file's own length before any clip
/// plays it, wearing the tint the lanes will tint that clip with, and the
/// timeline is exactly as long as it was. Placing it is the drag, and the
/// drag is a separate act.
#[test]
fn an_import_adds_a_row_at_its_own_length_and_leaves_the_lanes_alone() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let before = session.timeline_duration();
    let clips: Vec<usize> = session
        .lanes()
        .into_iter()
        .map(|lane| session.lane_clips(lane).len())
        .collect();
    session.import(&asset("test_av2.mp4")).expect("av2 matches");
    assert_eq!(session.sources().len(), 2, "the library grew");
    assert_eq!(
        session.timeline_duration(),
        before,
        "an import must not place a clip"
    );
    assert_eq!(
        session
            .lanes()
            .into_iter()
            .map(|lane| session.lane_clips(lane).len())
            .collect::<Vec<_>>(),
        clips,
        "no lane moved"
    );
    // The rows the panel draws: one per source, each its file's own length
    // -- source 1 has no clip anywhere and is still 4 s at 30 fps.
    let rows = library_rows(
        session.sources(),
        &HashMap::new(),
        &HashMap::new(),
        None,
        |path| session.file_frames(path),
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].frames, 150, "5 s at 30 fps");
    assert_eq!(rows[1].frames, 120, "4 s at 30 fps, never placed");
    // The swatch is the clip colour, by the same index and the same
    // function -- what makes the panel and the lanes one association.
    for row in 0..rows.len() {
        assert_eq!(rows[row].tint, row);
        assert_eq!(source_tint(row), SOURCE_TINTS()[row % SOURCE_TINTS().len()]);
    }
}

/// The window opened on a song and nothing else -- the launch argument, the
/// drop on an empty window and the Import button all end in the same
/// `PlaybackSession::open`. The library lists it placeable, the lane door
/// the Add button and a drag share puts it on `A1`, and the one format that
/// needs a picture says so on its own row instead of failing at the end of
/// an export.
#[test]
fn a_song_opens_the_window_by_itself() {
    let mut session =
        PlaybackSession::open(asset("test_tone.mp3")).expect("a song is a timeline");
    session.set_gain(0.0);
    // The source's own path, which is the canonical one a row carries.
    let path = session.sources()[0].path.clone();
    assert!(session.lane_clips(Lane::V1).is_empty(), "no picture");
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);

    // The library row: probed like any other source, and not greyed --
    // `unusable` is what the panel dims a row with.
    let streams = HashMap::from([(
        path.clone(),
        engine::AudioSession::probe_streams(&path).expect("probe the song"),
    )]);
    let rate = streams[&path]
        .iter()
        .find(|s| s.index == session.sources()[0].audio_stream)
        .map(|s| (s.sample_rate, s.channels));
    let frames = session.file_frames(&path);
    let rows = library_rows(session.sources(), &streams, &HashMap::new(), rate, |_| {
        frames
    });
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "test_tone.mp3");
    assert_eq!(rows[0].unusable, None, "the row is placeable");
    assert_eq!(rows[0].frames, 90, "3 s at the audio-only 30 fps");

    // ...and it places, on the audio lane, through the door `insert_source`
    // uses: a second copy of the song at the playhead.
    session.seek(1.0);
    assert!(
        session
            .place_stream_at(1.0, &path, 0, Some(Lane::A1))
            .expect("its own file is on this timeline")
    );
    assert_eq!(session.lane_clips(Lane::A1).len(), 2);
    assert!(session.lane_clips(Lane::V1).is_empty(), "still no picture");

    // Both picture rows carry the reason rather than the format's detail
    // line, each naming itself; the audio formats are what this timeline is
    // and are never refused.
    assert_eq!(
        format_refusal(&session, Format::Mp4).as_deref(),
        Some("no picture — an mp4 would be black; export WAV, FLAC, MP3 or OGG")
    );
    assert_eq!(
        format_refusal(&session, Format::Av1).as_deref(),
        Some("no picture — an AV1 Matroska would be black; export WAV, FLAC, MP3 or OGG")
    );
    assert_eq!(
        format_refusal(&session, Format::Av1Mp4).as_deref(),
        Some("no picture — an AV1 mp4 would be black; export WAV, FLAC, MP3 or OGG")
    );
    assert_eq!(format_refusal(&session, Format::Wav), None);
    assert_eq!(format_refusal(&session, Format::Flac), None);
    assert_eq!(format_refusal(&session, Format::Mp3), None);
}

/// A file's every audio track gets a row: the ones the timeline can take
/// are placeable, the ones it cannot are listed greyed with the reason.
/// Which rows exist at all is the branchy part of the panel, so it is
/// planned as data and checked here rather than through the pointer.
#[test]
fn every_audio_stream_of_a_file_is_a_row_usable_or_not() {
    let multi = PathBuf::from("/m/movie.mp4");
    let sources = [source("/m/movie.mp4", 0)];
    let mut streams = HashMap::new();
    streams.insert(
        multi.clone(),
        vec![
            info(0, 44_100, 2, None, true),
            info(1, 44_100, 2, Some("fra"), true),
            info(2, 22_050, 1, Some("deu"), true),
            info(3, 0, 0, None, false),
        ],
    );
    let rows = library_rows(
        &sources,
        &streams,
        &HashMap::new(),
        Some((44_100, 2)),
        |_| 90,
    );
    assert_eq!(
        rows.iter().map(|r| r.stream).collect::<Vec<_>>(),
        [0, 1, 2, 3],
        "one row per audio stream, in file order"
    );
    assert!(
        rows.iter().all(|r| r.path == multi && r.tint == 0),
        "every stream of one file wears the file's own tint"
    );
    assert_eq!(rows[0].name, "movie.mp4 [audio 1]");
    assert_eq!(rows[1].name, "movie.mp4 [audio 2]");
    assert_eq!(rows[1].detail, "fra 44.1 kHz stereo");
    // Placeable: the one already on the timeline, and the one that matches
    // it. The mono track cannot join a stereo timeline -- one device and one
    // copied AAC track for the whole of it -- and the codec we cannot read
    // cannot join anything. Both say which. Its 22 kHz is *not* part of that
    // any more: a rate of its own is resampled at the decoder's door, and a
    // row greyed for one the engine accepts is this picker telling a lie.
    assert_eq!((&rows[0].unusable, &rows[1].unusable), (&None, &None));
    assert_eq!(rows[2].unusable.as_deref(), Some("the timeline is stereo"));
    assert_eq!(
        unusable(&info(9, 48_000, 2, None, true), Some((44_100, 2))),
        None,
        "48 kHz stereo joins a 44.1 kHz stereo timeline"
    );
    assert_eq!(rows[3].unusable.as_deref(), Some("unsupported codec"));
    assert_eq!(
        rows[3].detail, "",
        "a stream we cannot parse claims nothing"
    );
    // Every row is the same file, so every row is that file's length.
    assert!(rows.iter().all(|r| r.frames == 90));

    // The single-stream case is exactly one row and no stream tag: no
    // regression for the media everything else in the world is.
    let plain = [source("/m/plain.mp4", 0)];
    let mut one = HashMap::new();
    one.insert(
        PathBuf::from("/m/plain.mp4"),
        vec![info(0, 44_100, 2, None, true)],
    );
    let rows = library_rows(&plain, &one, &HashMap::new(), Some((44_100, 2)), |_| 90);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "plain.mp4");
    assert_eq!(
        rows[0].detail, "",
        "one audio track is the row it has always been: name and length"
    );
    // ...as is a silent file, and a file not probed yet.
    let mut silent = HashMap::new();
    silent.insert(PathBuf::from("/m/plain.mp4"), Vec::new());
    for probe in [silent, HashMap::new()] {
        let rows = library_rows(&plain, &probe, &HashMap::new(), None, |_| 90);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "plain.mp4");
        assert_eq!(rows[0].detail, "");
    }

    // A second stream *placed* on the timeline is a source entry of its
    // own: it keeps its row, no duplicate appears for it, and the rows of
    // one file stay together whatever order the entries came in.
    let placed = [
        source("/m/movie.mp4", 0),
        source("/m/other.mp4", 0),
        source("/m/movie.mp4", 2),
    ];
    streams.insert(
        PathBuf::from("/m/other.mp4"),
        vec![info(0, 44_100, 2, None, true)],
    );
    let rows = library_rows(
        &placed,
        &streams,
        &HashMap::new(),
        Some((44_100, 2)),
        |_| 90,
    );
    assert_eq!(
        rows.iter()
            .map(|r| (file_name(&r.path), r.stream))
            .collect::<Vec<_>>(),
        [
            ("movie.mp4".to_string(), 0),
            ("other.mp4".to_string(), 0),
            ("movie.mp4".to_string(), 2),
            ("movie.mp4".to_string(), 1),
            ("movie.mp4".to_string(), 3),
        ]
    );
    assert!(
        rows[2].unusable.is_none(),
        "a stream already on the timeline is playing, whatever a probe says"
    );
    assert_eq!(rows[1].tint, 1, "the other file is the other tint");
}

/// What the clip menu offers, and where it hangs. The two items that act on
/// the playhead rather than on the clicked clip are the ones that can be
/// inapplicable, and a menu at the edge of the window has to come back
/// inside it or its last item cannot be clicked at all.
#[test]
fn the_clip_menu_dims_what_the_playhead_is_not_on_and_stays_in_the_window() {
    use keymap::{ActionId, Keymap};
    // Frames 30..90 of the timeline, taken from the head of its source.
    let clip = Clip {
        start: 30,
        in_frame: 0,
        out_frame: 60,
        source: 0,
        link: None,
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    assert_eq!(clip.end(), 90);
    let a1 = Lane::A1;
    let v1 = Lane::V1;
    // The question the clip menu asks: a timeline is open and the menu was
    // opened on this clip, at this playhead.
    let on = |clip: &Clip, lane, action, playhead| {
        enable(
            action,
            Ctx {
                clip: Some((*clip, lane)),
                playhead,
                timeline: true,
                ..Ctx::default()
            },
        )
    };
    let offered = |clip: &Clip, lane, action, playhead| on(clip, lane, action, playhead).yes();
    // Cut splits from inside only: neither edge has anything to split off.
    assert!(offered(&clip, v1, ActionId::Cut, 31));
    assert!(offered(&clip, v1, ActionId::Cut, 89));
    assert!(!offered(&clip, v1, ActionId::Cut, 30));
    assert!(!offered(&clip, v1, ActionId::Cut, 90));
    assert!(!offered(&clip, v1, ActionId::Cut, 200));
    // A slowed clip refuses more than the edges do, and it may not say the
    // edges' words while doing it: at a quarter speed each of its sixty
    // source frames is on screen four timeline frames, and only the first of
    // the four is a frame the file has. The three between are inside the
    // clip by any reading of the playhead, so "only from inside a clip"
    // there is a refusal claiming something the screen contradicts.
    let slow = Clip {
        speed: Speed::MIN,
        ..clip
    };
    assert_eq!(slow.frames(), 240, "a quarter speed is four times as long");
    assert!(offered(&slow, v1, ActionId::Cut, 34), "34 is a source frame");
    assert!(!offered(&slow, v1, ActionId::Cut, 35));
    assert_eq!(
        on(&slow, v1, ActionId::Cut, 35).why(),
        Some("this speed holds one frame here — step to the next"),
    );
    assert_eq!(
        on(&slow, v1, ActionId::Cut, 30).why(),
        Some("only from inside a clip"),
        "the head is still the edge's refusal, speed or no speed",
    );
    // Regroup is the other way round: only where this clip meets another.
    assert!(offered(&clip, v1, ActionId::Regroup, 30));
    assert!(offered(&clip, v1, ActionId::Regroup, 90));
    assert!(!offered(&clip, v1, ActionId::Regroup, 60));
    // Detach is the clip's own business: nothing to take apart in one that
    // names no group, and whether the group still has another half is the
    // engine's question. Group is offered on every clip, for that reason.
    assert!(!offered(&clip, v1, ActionId::Detach, 0));
    let grouped = Clip {
        link: Some(3),
        ..clip
    };
    assert!(offered(&grouped, v1, ActionId::Detach, 60));
    assert!(offered(&clip, a1, ActionId::Group, 0));
    // The equalizer is the one item the *lane* decides: it filters samples,
    // and a video clip has none of its own. Never the playhead's business.
    assert!(offered(&clip, a1, ActionId::Equalizer, 0));
    assert!(offered(&clip, a1, ActionId::Equalizer, 60));
    assert!(!offered(&clip, v1, ActionId::Equalizer, 60));
    // The rest act on the clip that was clicked, so they always mean
    // something -- the engine words its own refusals.
    for action in [ActionId::Delete, ActionId::Lift, ActionId::ToggleMute] {
        assert!(offered(&clip, v1, action, 0));
        assert!(offered(&clip, a1, action, 60));
    }
    // Except the grade, which is a picture setting: offered on a video
    // clip wherever the playhead is, dimmed on a waveform.
    assert!(offered(&clip, v1, ActionId::Color, 0));
    assert!(!offered(&clip, a1, ActionId::Color, 60));
    // The two kinds of no, which is what the row is told apart by: a grade
    // on a waveform is a *class* answer -- an audio clip has no picture and
    // never will, so the menu leaves the item out entirely -- where a cut at
    // the clip's edge is this moment's answer and the next click of the
    // playhead changes it, so that one is drawn, dimmed, and says why.
    //
    // What the menu draws for a clip of each kind, in order: the render
    // loop's `continue`, as a list.
    let menu = |lane, image, playhead| {
        MENU_ITEMS
            .into_iter()
            .filter(|&action| {
                enable(
                    action,
                    Ctx {
                        clip: Some((clip, lane)),
                        image,
                        playhead,
                        timeline: true,
                        ..Ctx::default()
                    },
                )
                .listed()
            })
            .collect::<Vec<_>>()
    };
    // Sound: no grade and no fit policy, and the equalizer that is the
    // whole reason an audio clip has a menu of its own.
    let sound = menu(a1, false, 60);
    assert!(!sound.contains(&ActionId::Color), "{sound:?}");
    assert!(!sound.contains(&ActionId::Fit), "{sound:?}");
    assert!(sound.contains(&ActionId::Equalizer));
    assert!(sound.contains(&ActionId::Silence));
    // Picture: the mirror of it. The sound of a take is the audio lane's,
    // clip for clip, so the equalizer is not this clip's business -- but the
    // silence scan is, because it opens on the half it is grouped with.
    let picture = menu(v1, false, 60);
    assert!(!picture.contains(&ActionId::Equalizer), "{picture:?}");
    assert!(picture.contains(&ActionId::Color));
    assert!(picture.contains(&ActionId::Fit));
    assert!(picture.contains(&ActionId::Silence));
    // A still: picture with no sound anywhere, ever. Graded, fitted and
    // re-timed like any other clip (a speed reaches a still through the same
    // rewrite), scanned like none.
    let still = menu(v1, true, 60);
    assert!(!still.contains(&ActionId::Silence), "{still:?}");
    assert!(!still.contains(&ActionId::Equalizer), "{still:?}");
    assert!(still.contains(&ActionId::Color));
    assert!(still.contains(&ActionId::Fit));
    assert!(still.contains(&ActionId::Speed));
    // ...and the state refusals are on all three, dimmed rather than gone:
    // at 30 the playhead is on this clip's head, where a cut has nothing to
    // split off -- a row the next click of the playhead lights.
    for rows in [menu(a1, false, 30), menu(v1, false, 30), menu(v1, true, 30)] {
        assert!(rows.contains(&ActionId::Cut), "{rows:?}");
        assert!(rows.contains(&ActionId::Detach), "{rows:?}");
    }
    // The actions card is the other half of the rule: it lists the whole
    // registry, so a class refusal is dimmed there with its reason and never
    // dropped -- an action missing from the one surface that lists
    // everything would read as an action that does not exist.
    let listed: Vec<ActionId> = keys_rows()
        .into_iter()
        .filter_map(|r| match r {
            KeyRow::Act(action) => Some(action),
            _ => None,
        })
        .collect();
    for (lane, action) in [
        (a1, ActionId::Color),
        (a1, ActionId::Fit),
        (v1, ActionId::Equalizer),
    ] {
        assert!(matches!(on(&clip, lane, action, 60), Enable::Hidden(_)));
        assert!(listed.contains(&action), "{action:?} left the actions card");
        assert!(on(&clip, lane, action, 60).why().is_some());
    }
    assert!(matches!(on(&clip, v1, ActionId::Cut, 30), Enable::No(_)));
    assert!(matches!(on(&clip, v1, ActionId::Regroup, 60), Enable::No(_)));
    assert!(matches!(on(&clip, v1, ActionId::Detach, 0), Enable::No(_)));
    // Every refusal says something, and says it short enough to sit in the
    // menu's right-hand column beside a label -- the still's included, which
    // the card dims with while the menu leaves the row out.
    for action in MENU_ITEMS {
        for (lane, image) in [(v1, false), (a1, false), (v1, true)] {
            for playhead in [0, 30, 60, 90] {
                let refusal = enable(
                    action,
                    Ctx {
                        clip: Some((clip, lane)),
                        image,
                        playhead,
                        timeline: true,
                        ..Ctx::default()
                    },
                );
                if let Some(why) = refusal.why() {
                    assert!(!why.is_empty() && why.len() <= 30, "{action:?}: {why:?}");
                }
            }
        }
    }
    assert!(matches!(
        enable(
            ActionId::Silence,
            Ctx {
                clip: Some((clip, v1)),
                image: true,
                playhead: 60,
                timeline: true,
                ..Ctx::default()
            }
        ),
        Enable::Hidden(_)
    ));
    // The editor as a whole, which is how the actions card asks: with no
    // timeline nothing is offered, an export leaves only its own cancel,
    // and the three that act on the marked clip say so when none is.
    let whole = |action, ctx| enable(action, ctx);
    assert_eq!(
        whole(ActionId::Play, Ctx::default()),
        Enable::No("no timeline open")
    );
    let live = Ctx {
        timeline: true,
        playable: true,
        ..Ctx::default()
    };
    assert!(whole(ActionId::Play, live).yes());
    // ...and an open timeline with nothing on its lanes refuses the
    // transport in the oracle rather than in the button, which is what
    // makes the key and the button say the same thing.
    assert_eq!(
        whole(
            ActionId::Play,
            Ctx {
                playable: false,
                ..live
            }
        ),
        Enable::No("put a clip on a lane first")
    );
    // The magnet and the monitoring level are the editor's own and answer
    // with no timeline at all -- the keyboard always fired them there, and
    // now so does the toolbar.
    for editor_wide in [
        ActionId::ToggleSnap,
        ActionId::ToggleMute,
        ActionId::VolumeUp,
        ActionId::VolumeDown,
    ] {
        assert!(
            whole(editor_wide, Ctx::default()).yes(),
            "{editor_wide:?} is dead with no file open while its key still fires"
        );
    }
    assert!(!whole(ActionId::Delete, live).yes());
    assert!(!whole(ActionId::Paste, live).yes());
    assert!(
        whole(
            ActionId::Paste,
            Ctx {
                clipboard: true,
                ..live
            }
        )
        .yes()
    );
    assert!(!whole(ActionId::CancelExport, live).yes());
    let busy = Ctx {
        exporting: true,
        ..live
    };
    assert!(whole(ActionId::CancelExport, busy).yes());
    for action in ActionId::ALL {
        // Nothing may touch the edit list while an export is reading it --
        // and the palette does not: it repaints this window and writes one
        // word to the user's own config file, which is why it is the one
        // action besides the cancel that stays live here.
        let allowed = action == ActionId::CancelExport || action == ActionId::Theme;
        assert_eq!(
            whole(action, busy).yes(),
            allowed,
            "{action:?} while an export reads the edit list"
        );
    }
    // The playhead frame is the engine's own rule, boundary included.
    assert_eq!(frame_at(1.0, 30.), 30);
    assert_eq!(frame_at(0.0, 30.), 0);
    assert_eq!(frame_at(-1.0, 30.), 0);
    // Where it hangs: at the pointer when it fits, back inside when not.
    let viewport = size(px(800.), px(400.));
    assert_eq!(menu_at(point(px(10.), px(10.)), viewport, 150.), (10., 10.));
    assert_eq!(
        menu_at(point(px(700.), px(380.)), viewport, 150.),
        (800. - MENU_W, 250.)
    );
    // A window smaller than the menu loses its bottom, never its top.
    assert_eq!(
        menu_at(point(px(90.), px(40.)), size(px(100.), px(50.)), 150.),
        (0., 0.)
    );
    // Every item is an action the registry knows, so the menu and the keys
    // menu say the same thing about it -- and none of them is unreachable
    // by keyboard, which is what makes the hint column worth drawing.
    let keymap = Keymap::defaults();
    for action in MENU_ITEMS {
        assert!(ActionId::ALL.contains(&action), "{action:?} is not listed");
        assert_ne!(keymap.display(action), "unbound", "{action:?}");
    }
    // ...and the whole card still fits the 640x360 floor, however many items
    // it grows to: the list is what scrolls where the window is too short
    // for it, never the card that grows.
    let items = MENU_ITEMS.len() + 1; // Properties
    let floor = size(px(640.), px(360.));
    assert!(MENU_PAD * 2. + menu_rows_h(items, floor) <= 360., "too tall");
    assert!(
        menu_rows_h(items, floor) / MENU_ROW_H >= 12.,
        "too few items visible to scan on the smallest window"
    );
    assert_eq!(
        menu_at(point(px(0.), px(0.)), floor, {
            MENU_PAD * 2. + menu_rows_h(items, floor)
        }),
        (0., 0.)
    );
    // The picker is the same card and the longest list in it is the palette
    // family list, which grows every time somebody asks for more colours.
    // Twelve of them still stand on screen *whole* at the floor -- so the
    // list's `overflow_y_scroll` is a safety net rather than something a
    // person has to find. The day a family makes this fail is the day the
    // card owes the affordance line the inspector and the cards carry
    // ("more below — scroll the …"): a row nobody knows is under the fold
    // is a row that is not there, and no scroll bar says it.
    let families = crate::ui::theme::PaletteId::ALL.len();
    assert_eq!(
        menu_rows_h(families, floor),
        families as f32 * MENU_ROW_H,
        "{families} palettes no longer fit 640x360 -- the picker now needs \
         the 'more below' line and a keyboard cursor that scrolls into view"
    );
    // ...and the detail beside each one is short enough to survive the
    // right-hand column at that width instead of being truncated mid-word.
    for id in crate::ui::theme::PaletteId::ALL {
        assert!(id.detail().len() <= 30, "{id:?}: {:?}", id.detail());
    }
}

/// Both menus, at the two things they can be opened on, and the box each
/// one is drawn in. Two rules, and the render obeys them by *calling* what
/// this calls -- [`menu_items`] and [`row_items`] are the only lists either
/// menu is built from, and [`menu_rows_h`] is the height each is both placed
/// by and drawn to:
///
/// 1. a row exists only where the oracle lists the action for the very thing
///    that was right-clicked, so an item can never offer a video action on a
///    waveform (the complaint this comes from) and a new action added to
///    `MENU_ITEMS` cannot appear where it does not apply;
/// 2. the whole card is inside the window, wherever it was opened and
///    however long the list -- a menu drawn past the bottom edge is a menu
///    whose last items nobody can click.
#[test]
fn a_menu_offers_only_what_applies_and_is_drawn_inside_the_window() {
    use keymap::ActionId;
    let clip = Clip {
        start: 30,
        in_frame: 0,
        out_frame: 60,
        source: 0,
        link: Some(1),
        eq: None,
        color: None,
        fit: FitPolicy::default(),
        speed: Speed::NORMAL,
    };
    let ctx = |lane, image| Ctx {
        clip: Some((clip, lane)),
        image,
        playhead: 60,
        timeline: true,
        ..Ctx::default()
    };
    // The oracle is the whole of what the menu draws: every item it lists is
    // one the oracle would list, and every item it leaves out is one the
    // oracle hides -- there is no third answer, and no hand-written list.
    for (lane, image) in [(Lane::V1, false), (Lane::A1, false), (Lane::V1, true)] {
        let ctx = ctx(lane, image);
        let rows = menu_items(ctx);
        for action in MENU_ITEMS {
            assert_eq!(
                rows.contains(&action),
                enable(action, ctx).listed(),
                "{action:?} on {lane:?}, image={image}"
            );
        }
    }
    // Sound has no picture settings; picture has no equalizer of its own; a
    // still has no sound to scan. The user's own words: an audio clip must
    // not be offered what only a picture can be given.
    let sound = menu_items(ctx(Lane::A1, false));
    assert!(!sound.contains(&ActionId::Color), "{sound:?}");
    assert!(!sound.contains(&ActionId::Fit), "{sound:?}");
    assert!(sound.contains(&ActionId::Equalizer));
    let picture = menu_items(ctx(Lane::V1, false));
    assert!(!picture.contains(&ActionId::Equalizer), "{picture:?}");
    assert!(picture.contains(&ActionId::Color));
    assert!(!menu_items(ctx(Lane::V1, true)).contains(&ActionId::Silence));
    // The library menu is the same rule on the other panel: its items come
    // off `row_items` and nothing else, and the two that change the timeline
    // say why rather than being clicked and refused afterwards.
    let live = RowCtx {
        timeline: true,
        usable: true,
        ..RowCtx::default()
    };
    for ctx in [
        live,
        RowCtx {
            usable: false,
            ..live
        },
        RowCtx { placed: 2, ..live },
        RowCtx {
            exporting: true,
            ..live
        },
        RowCtx::default(),
    ] {
        let rows = row_items(ctx);
        for item in ROW_ITEMS {
            assert_eq!(rows.contains(&item), row_enable(item, ctx).listed());
        }
        // Whatever the state, the two that need neither timeline nor edit
        // list are offered: a file is always describable and findable.
        assert!(row_enable(RowItem::Reveal, ctx).yes());
        assert!(row_enable(RowItem::Properties, ctx).yes());
    }
    assert!(row_enable(RowItem::Add, live).yes());
    assert!(
        !row_enable(
            RowItem::Add,
            RowCtx {
                usable: false,
                ..live
            }
        )
        .yes(),
        "a file that cannot join this timeline is not an Add anybody can ask for"
    );
    assert!(!row_enable(RowItem::Remove, RowCtx { placed: 1, ..live }).yes());
    assert!(!row_enable(RowItem::Add, RowCtx::default()).yes());
    // Every refusal is short enough to sit in the hint column beside its
    // label, the clip menu's rule.
    for item in ROW_ITEMS {
        for ctx in [live, RowCtx::default()] {
            if let Some(why) = row_enable(item, ctx).why() {
                assert!(!why.is_empty() && why.len() <= 30, "{why:?}");
            }
        }
    }
    // ...and the box. Every window from the floor up, every corner of it,
    // and every list length either menu can have: the card is placed by
    // `MENU_PAD * 2 + menu_rows_h` and drawn to it, so this is the card.
    for viewport in [
        size(px(640.), px(360.)),
        size(px(800.), px(600.)),
        size(px(1280.), px(690.)),
        // Smaller than the floor the layout is sized for: it still may not
        // draw outside the window it has.
        size(px(320.), px(200.)),
    ] {
        for rows in 1..=MENU_ITEMS.len() + 1 {
            let h = MENU_PAD * 2. + menu_rows_h(rows, viewport);
            for at in [
                point(px(0.), px(0.)),
                point(px(10.), px(10.)),
                // The click that started all this: low in the window, where
                // the menu used to hang off the bottom edge.
                point(px(0.), viewport.height - px(4.)),
                point(viewport.width - px(4.), viewport.height - px(4.)),
                point(viewport.width * 2., viewport.height * 2.),
            ] {
                let (x, y) = menu_at(at, viewport, h);
                assert!(x >= 0. && y >= 0., "{x},{y} outside {viewport:?}");
                assert!(
                    x + MENU_W <= f32::from(viewport.width) + 0.01,
                    "{rows} rows at {at:?} hang off the right of {viewport:?}"
                );
                assert!(
                    y + h <= f32::from(viewport.height) + 0.01,
                    "{rows} rows at {at:?} hang off the bottom of {viewport:?}"
                );
            }
        }
    }
    // On any window with the room, the whole list is drawn rather than
    // twelve rows of it and a scroll nobody is told about.
    let items = MENU_ITEMS.len() + 1;
    let real = size(px(1280.), px(690.));
    assert_eq!(menu_rows_h(items, real), items as f32 * MENU_ROW_H);
    assert!(menu_rows_h(items, size(px(640.), px(360.))) < items as f32 * MENU_ROW_H);
}

/// The other half of the keys menu's guarantee, and the audit this batch was
/// asked for kept as a test: no action may be a stroke and nothing else.
/// The actions card answers it for all of them at once -- its rows come off
/// [`ActionId::ALL`], so a fortieth action is on it the moment it exists and
/// there is no hand-written list here to fall behind.
#[test]
fn every_action_is_reachable_without_the_keyboard() {
    use keymap::ActionId;
    let source = ui_source();
    let source = source.as_str();
    let element = |id: &str| source.contains(&format!("\"{id}\""));
    let listed: Vec<ActionId> = keys_rows()
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
            "{action:?} is reachable by keyboard only"
        );
    }
    // The snap has a door of its own as well as its row on the card: the
    // button beside the zoom, which says which way it is set as well as
    // setting it.
    assert!(element("snap"), "no snap button beside the zoom");
    // And the card is a door the pointer can open: the panel's own button.
    assert!(element("keys"), "no way to open the actions card");
    // The card-local strokes have the same rule, and each of them is a thing
    // on its card: the graph and its two buttons, the colour bars and their
    // reset, the speed bar and its presets, and the silence card's rows --
    // whose steppers are the pointer's only way to a threshold.
    for id in [
        "eq-graph",
        "eq-reset",
        "eq-spectrum",
        "color-bar",
        "color-reset",
        "speed-bar",
        "speed-preset",
        "silence-row",
        "silence-step",
        "mix-row",
        "mix-step",
        "silence-apply",
        "export-confirm",
    ] {
        assert!(element(id), "{id} is not on any card");
    }
    // ...and each one is named by the row that carries its stroke, so a
    // card-local key added to `FIXED` with nothing to click fails here
    // instead of being noticed by whoever tries to use the card without a
    // keyboard. `KeyRow::Fixed` is "shown and never offered", which used to
    // mean the twenty-eight rows below were the one part of this editor no
    // reachability test read.
    for fixed in keymap::FIXED.iter() {
        match fixed.reach {
            keymap::Reach::Click(id) => assert!(
                element(id),
                "{:?} ({}) points at {id}, which is on no card",
                fixed.chord,
                fixed.label
            ),
            // Nothing to click by decision, not by omission: getting out of
            // a card, and the hold that repeats what a drag already does.
            keymap::Reach::Gesture => {}
        }
    }
}

/// The other half of "getting out of a card is a `Reach::Gesture`": the
/// gesture has to exist. Every card's scrim closes it on a press, so a hand
/// that never touches the keyboard can shut every one of them -- for seven
/// cards `esc` was the only exit, which is the same complaint as an action
/// reachable by stroke alone, said about the way out instead of the way in.
///
/// Read off [`Player::modal`] rather than off a list written here: a card
/// counted there and not closed by [`Player::close_card`] fails this test
/// the day it is added, which is the only way the two stay in step.
#[test]
fn every_card_closes_without_the_keyboard() {
    for card in [
        "keys_overlay",
        "export_card",
        "eq_card",
        "color_card",
        "speed_card",
        "mix_card",
        "silence_card",
    ] {
        let src = fn_body(card);
        assert!(
            src.contains("this.close_card()"),
            "{card}'s scrim swallows the press without closing the card: \
             escape is the only way out of it"
        );
        assert!(
            src.contains("MouseButton::Left, swallow"),
            "{card}'s body does not swallow its own presses, so a press on \
             one of its controls would close the card under itself"
        );
    }
    // ...and every state that makes the window modal is a state that press
    // clears. `exporting()` is not one of them: a running export is a job
    // with a cancel button, not a card with a scrim.
    let close = fn_body("close_card");
    for field in fn_body("modal").split("self.").skip(1).filter_map(|rest| {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!rest[name.len()..].starts_with('(')).then_some(name)
    }) {
        assert!(
            close.contains(&format!("self.{field}")),
            "{field} makes the window modal and `close_card` leaves it set: \
             that card cannot be closed by pointer"
        );
    }
}

/// A clip too short to hit is still a clip: at a far zoom-out its box is
/// drawn [`HIT_MIN`] wide rather than the fraction of a pixel it is worth,
/// because a box nobody can put a pointer on cannot be selected, dragged,
/// trimmed or given a menu -- and an invisible unselectable take is strictly
/// worse than one drawn a few pixels too wide.
#[test]
fn a_clip_box_is_never_narrower_than_its_target() {
    // Two seconds on a bed showing an hour: a fifth of a pixel.
    let far = Scale {
        pps: 0.1,
        start: 0.,
    };
    assert!(far.width_px(2.) < 1., "the fixture is not zoomed out enough");
    assert_eq!(clip_width(far.width_px(2.)), HIT_MIN);
    // Zero is the floor's own case: a clip trimmed to nothing, and the
    // width a lane draws before its first frame is measured.
    assert_eq!(clip_width(0.), HIT_MIN);
    // What is already wide enough keeps its own width, to the pixel: the
    // floor is a floor and never a resize.
    let near = Scale::default();
    assert_eq!(clip_width(near.width_px(5.)), near.width_px(5.));
    // ...and the padding is not trimmable: the strips are asked of the
    // clip's own width, so a box drawn wider than its length keeps all of
    // that box as a body to select and drag by ([`trims`]).
    assert!(!trims(far.width_px(2.)));
    assert!(clip_width(far.width_px(2.)) >= HIT_MIN);
}

/// What the Detach and Group items do to a real timeline: a music video's
/// sound comes off its picture, Delete on the loose half takes that half
/// only, undo puts both back, and Group makes the two one take again --
/// whole-take delete and all.
#[test]
fn a_detached_half_is_removed_alone_and_groups_again() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    assert!(whole_take(&session, Lane::V1, 0), "one take to start with");
    assert!(whole_take(&session, Lane::A1, 0));
    let frames = session.lane_clips(Lane::V1)[0].len();

    // Detach audio: neither half is a whole take any more, so Delete on
    // either leaves the other exactly where it is.
    assert!(session.ungroup(Lane::V1, 0));
    assert!(
        !whole_take(&session, Lane::A1, 0),
        "the sound is a half now"
    );
    assert!(!whole_take(&session, Lane::V1, 0), "and so is the picture");
    assert!(session.lift_clip(Lane::A1, 0));
    assert!(session.lane_clips(Lane::A1).is_empty(), "the sound went");
    assert_eq!(session.lane_clips(Lane::V1).len(), 1, "the picture stayed");
    assert_eq!(session.lane_clips(Lane::V1)[0].len(), frames, "untrimmed");

    // One undo per edit, the removal then the detach.
    assert!(session.undo());
    assert_eq!(session.lane_clips(Lane::A1).len(), 1, "the sound is back");
    assert!(session.undo());
    assert!(whole_take(&session, Lane::A1, 0), "one take again");

    // Group: the partner is the clip covering these very frames on the
    // other track, which is what the item hands the engine.
    assert!(session.ungroup(Lane::V1, 0));
    assert_eq!(span_partner(&session, Lane::V1, 0), Some((Lane::A1, 0)));
    session
        .group(Lane::V1, 0, Lane::A1, 0)
        .expect("both halves still cover the same frames");
    assert!(
        whole_take(&session, Lane::A1, 0),
        "a take that ripples again"
    );
    assert_eq!(
        span_partner(&session, Lane::V1, 0),
        None,
        "and nothing left on another track to group with"
    );
}

/// Which clip Group reaches when more than one covers the span: the sound,
/// whatever order the lanes are stored in. A project file may hold them in
/// any order -- a video layer *before* the audio lane among them -- and
/// "group this" means the other half of the take, never the layer above it.
#[test]
fn group_reaches_the_sound_before_a_video_layer_over_it() {
    use engine::project::LaneKind;

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let v2 = session.add_lane(LaneKind::Video);
    // A bare layer, through the one-lane door: a *drop* on V2 brings the
    // file's sound down with it and would put a third clip over this span.
    let whole = session.lane_clips(Lane::V1)[0];
    assert!(
        session.place_at(v2, 0.0, whole),
        "a layer covering the same frames as the take"
    );

    // Saved and loaded back with the lanes in the order a hand-written
    // project may hold them: the sound last, behind the layer.
    let dir = engine::scratch::Scratch::dir("ve_group");
    let file = dir.join("lanes.edith");
    session.save_project(&file).expect("save the project");
    let text = std::fs::read_to_string(&file).expect("read it back");
    let (sound, rest): (Vec<&str>, Vec<&str>) =
        text.lines().partition(|l| l.starts_with("audio "));
    std::fs::write(
        &file,
        format!("{}\n{}\n", rest.join("\n"), sound.join("\n")),
    )
    .expect("write the reordered project");
    let mut session = PlaybackSession::open_project(&file).expect("it loads as it stands");
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(
        session.lanes(),
        vec![Lane::V1, v2, Lane::A1],
        "the sound is the last lane there"
    );

    // Detached, so Group has a choice to get wrong: the layer covers these
    // frames too, and it is the lane the walk meets first.
    assert!(session.ungroup(Lane::V1, 0));
    // Group on the picture reaches the sound, not that layer.
    assert_eq!(span_partner(&session, Lane::V1, 0), Some((Lane::A1, 0)));
    session
        .group(Lane::V1, 0, Lane::A1, 0)
        .expect("the two halves cover the same frames");
    // ...and a lane of its own kind is still groupable, once the sound is
    // spoken for: two video lanes may be one take.
    assert_eq!(
        span_partner(&session, Lane::V1, 0),
        Some((v2, 0)),
        "the layer is what is left to group with"
    );
}

/// The refusal path, end to end against the real files: an incompatible
/// import changes nothing, and the library mirrors `sources()` 1:1, so a
/// refused file cannot leave a row behind.
#[test]
fn a_refused_import_leaves_no_row_and_an_accepted_one_is_whole() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    // Silent like the engine suite: this opens the real device.
    session.set_gain(0.0);
    assert_eq!(session.sources().len(), 1);
    // 640x360 joins now (the project canvas places it), and so does a file
    // with no sound (it plays silence over its span). What is left is one
    // output device: a mono track cannot join a stereo timeline.
    let refusal = session
        .import(&asset("test_ac3.mp4"))
        .expect_err("a mono track must not join a stereo timeline")
        .to_string();
    assert!(refusal.contains("audio"), "refusal must name it: {refusal}");
    assert_eq!(session.sources().len(), 1, "a refusal added a row");
    // An accepted one does add a row, and it reads as the whole file: 4 s
    // at 30 fps, its own length and not one taken off the lanes.
    session.import(&asset("test_av2.mp4")).expect("av2 matches");
    assert_eq!(session.sources().len(), 2);
    let second = session.sources()[1].path.clone();
    assert_eq!(session.file_frames(&second), 120);
    assert_eq!(timecode(120. / 30., 30.), "00:00:04:00");
}

/// What the Add button and a dropped row both do, minus the pointer: the
/// clip [`Player::insert_source`] builds, put in where the playhead is. One
/// call, so what a drop does cannot drift from what the button does.
#[test]
fn adding_a_row_drops_the_whole_source_in_at_the_playhead() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&asset("test_av2.mp4")).expect("av2 matches");
    // In the library only: the 5 s the fixture is, still.
    assert_eq!(session.timeline_duration(), 5.0);
    // Two seconds in, which is inside the first take: the insert splits it.
    session.seek(2.0);
    let second = session.sources()[1].path.clone();
    let frames = session.file_frames(&second);
    // Through the engine door `insert_source` uses, with the row's own
    // stream: the button, the drop and this are one call.
    assert!(
        session
            .place_stream_at(2.0, &second, 0, None)
            .expect("av2 is already on the timeline")
    );
    // The whole of source 1 went in and nothing was painted over: the
    // timeline is longer by exactly that file.
    assert_eq!(session.timeline_duration(), 9.0);
    let (video, audio) = (session.lane_clips(Lane::V1), session.lane_clips(Lane::A1));
    // One take, not a video clip with no sound under it: both lanes hold
    // the same clip at the same place, in the same group.
    let at = |lane: &[Clip]| {
        *lane
            .iter()
            .find(|c| c.start == 60)
            .expect("inserted at 2 s")
    };
    assert_eq!(at(video), at(audio));
    assert_eq!(at(video).source, 1);
    assert_eq!(at(video).len(), frames);
    assert!(at(video).link.is_some());
    assert_eq!(video.len(), audio.len());

    // The same door with a *second audio stream* of a file already there:
    // a new source entry, the same picture, and the row that was dragged
    // is what plays. This is the whole user-facing point of the slice.
    let mut session =
        PlaybackSession::open(asset("test_multilang.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let path = session.sources()[0].path.clone();
    let frames = session.file_frames(&path);
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &path, 1, None)
            .expect("the French track shares the timeline's parameters")
    );
    assert_eq!(session.sources()[1].audio_stream, 1);
    assert_eq!(session.timeline_duration(), end * 2.0);
    // Both rows are the same file, so both rows are that file's length --
    // the second one before any clip of its own existed.
    assert_eq!(session.file_frames(&path), frames);
}

/// The drop a hand actually makes: a library row let go on the *empty* bed
/// past the last clip, which is most of the bed on any timeline shorter
/// than the window. The whole chain the release runs, minus gpui's own
/// pointer read -- [`Player::place_frame`]'s [`landing`], the frame back
/// through the rate as [`Player::insert_source`] hands it over, and the one
/// engine door a row goes through -- and the head lands on the frame the
/// ghost was drawn on, black in front of it. It used to be swallowed by the
/// clipboard's clamp inside `Project::paste` and appended after the last
/// clip wherever it was let go, which is the bug this pins.
#[test]
fn a_row_dropped_on_the_open_bed_lands_under_the_pointer() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    session.import(&asset("test_av2.mp4")).expect("av2 matches");
    let second = session.sources()[1].path.clone();
    let fps = session.meta().frame_rate;
    assert_eq!(session.timeline_duration(), 5.0);
    // 5 s of timeline on a bed 30 s wide: the open bed past the last clip
    // is five sixths of it.
    let bed: Bounds<Pixels> = Bounds {
        origin: point(px(12.), px(400.)),
        size: size(px(600.), px(LANE_H)),
    };
    let scale = Scale {
        pps: 20.,
        start: 0.,
    };

    // What `Player::place_frame` answers for a pointer 200 px along the
    // bed: ten seconds in, five seconds past the end of the timeline, and
    // no mark anywhere near enough to pull it back.
    let clips = [session.lane_clips(Lane::V1), session.lane_clips(Lane::A1)];
    let marks = snap_marks(&clips, None, frame_at(session.now(), fps));
    let under = frame_at(scale.time_at(px_along(px(212.), bed)), fps);
    let (at, cue) = landing(under, 0, 0, true, scale.snap_frames(fps), &marks);
    assert_eq!((at, cue), (300, None), "the pointer is on frame 300");

    // ...and what the release does with it: the frame back through the same
    // rate every box is drawn at, into the door the Add button uses too.
    assert!(
        session
            .place_stream_at(f64::from(at) / fps, &second, 0, None)
            .expect("av2 is already on this timeline")
    );
    let head = |lane| {
        session
            .lane_clips(lane)
            .last()
            .copied()
            .expect("the dropped clip")
    };
    assert_eq!(
        head(Lane::V1).start,
        at,
        "the drop landed somewhere other than under the pointer"
    );
    assert_eq!(head(Lane::A1).start, at, "...and its sound with it");
    assert!(head(Lane::V1).link.is_some(), "one grouped take");
    assert_eq!(head(Lane::V1).link, head(Lane::A1).link);
    // The bed in front of it stays black: nothing was stretched to reach
    // it, and the 4 s file is the whole of what was added.
    assert_eq!(session.timeline_duration(), 14.0);
}

/// Remove from library, through the door the menu item uses
/// ([`Player::remove_source`] calls exactly this): refused by name while
/// clips play from the file, and once they do not the row leaves the list.
#[test]
fn removing_a_row_is_refused_while_it_plays_and_takes_the_row_away() {
    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    // Imported, then dragged onto the end: an import alone fills the
    // library, and a row with no clip is removable without any refusal to
    // test. This one has to have a take on the timeline.
    session.import(&asset("test_av2.mp4")).expect("av2 matches");
    let second = session.sources()[1].path.clone();
    let end = session.timeline_duration();
    assert!(
        session
            .place_stream_at(end, &second, 0, None)
            .expect("a file just imported is on this timeline")
    );
    let streams = HashMap::new();
    let rows = |session: &PlaybackSession| {
        library_rows(session.sources(), &streams, &HashMap::new(), None, |_| 0).len()
    };
    assert_eq!(rows(&session), 2, "a row per source");

    let refusal = session
        .remove_source(&second, 0)
        .expect_err("its take is still on the timeline")
        .to_string();
    assert!(refusal.contains("still plays"), "{refusal}");
    assert!(
        refusal.contains("V1 (1 clip)") && refusal.contains("A1 (1 clip)"),
        "the refusal names the lanes to clear: {refusal}"
    );
    assert_eq!(rows(&session), 2, "and the row is still there");

    // Delete that take -- the whole group, from either half -- and the file
    // can go. The list is what says so.
    let last = session.lane_clips(Lane::V1).len() - 1;
    assert!(session.delete_clip(Lane::V1, last));
    session
        .remove_source(&second, 0)
        .expect("nothing plays av2 any more");
    assert_eq!(rows(&session), 1, "the row went with it");
    assert_eq!(session.sources().len(), 1);
    // The one file left is held to the same rule and no other: its own take
    // is still on the lanes, so it stays...
    let first = session.sources()[0].path.clone();
    let refusal = session
        .remove_source(&first, 0)
        .expect_err("its take is still on the timeline")
        .to_string();
    assert!(refusal.contains("still plays"), "{refusal}");
    // ...and once nothing plays it, the *last* row goes too. What is left
    // is the empty library `Player::close_session` turns back into the
    // window the editor launches as -- the user-reported bug was that this
    // very removal was refused, leaving a row that could never be taken
    // out.
    assert!(session.delete_clip(Lane::V1, 0));
    session
        .remove_source(&first, 0)
        .expect("the only row goes like any other");
    assert_eq!(rows(&session), 0, "an empty library");
    assert!(session.sources().is_empty());
    // And it is still a session: silent, empty, and asked for its length
    // rather than panicking on a source list that is not there.
    assert_eq!(session.timeline_duration(), 0.0);
    assert!(
        session.save_project(&asset("nothing.edith")).is_err(),
        "a project naming no file could never be opened again, so it is not written"
    );
    // A row this timeline never had is refused, not panicked on.
    assert!(session.remove_source(&second, 0).is_err());
}

/// The clipboard after a library removal. A copied clip names its file by
/// index and a removal renumbers the list, so this is the difference
/// between pasting the take that was copied and pasting **another file**
/// over the same range ([`clipboard_after_remove`], called by
/// [`Player::remove_source`]).
#[test]
fn a_copied_clip_is_renumbered_or_dropped_when_a_row_leaves_the_library() {
    let clip = |source: usize| Clip {
        start: 0,
        in_frame: 0,
        out_frame: 30,
        source,
        link: None,
        eq: None,
        color: None,
        fit: Default::default(),
        speed: Default::default(),
    };
    // Copied from source 2, source 0 removed: the same file is source 1 now.
    assert_eq!(
        clipboard_after_remove(Some(clip(2)), 0).map(|c| c.source),
        Some(1),
        "the clipboard follows its file down the list"
    );
    // Copied from a source *before* the one that went: untouched.
    assert_eq!(
        clipboard_after_remove(Some(clip(0)), 2).map(|c| c.source),
        Some(0)
    );
    // Copied from the row that was just removed: there is nothing left to
    // paste, and pasting the next file along would be a lie.
    assert!(clipboard_after_remove(Some(clip(1)), 1).is_none());
    assert!(clipboard_after_remove(None, 0).is_none());
}

/// The trim-a-clip path through the doors the edge drag uses:
/// [`Player::trim_to`] clamps the pointer with `trim_room` and
/// [`Player::commit_trim`] writes it with `trim_clip`. The clip plays less
/// of its file, the sound linked to it follows, the head trim moves the
/// in-point, and one undo takes a whole gesture back.
///
/// The routing *to* these doors -- the 6 px edge strip claiming the press
/// the clip's own body-drag would otherwise take -- is gpui hitbox
/// behaviour (`occlude`) and is not reachable without a window.
#[test]
fn a_clip_trimmed_by_its_edge_plays_less_of_its_file() {
    use engine::project::Edge;

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let whole = session.lane_clips(Lane::V1)[0];
    let full = session.timeline_duration();
    assert_eq!(
        session.trim_room(Lane::V1, 0, Edge::End),
        Some((whole.start + 1, whole.end())),
        "the file's own last frame is how far the tail goes"
    );
    assert!(
        !session.trim_clip(Lane::V1, 0, Edge::End, 9_999),
        "it already plays all of it"
    );

    // Pulled in by a third: the timeline ends earlier and the sound with it.
    let shorter = whole.end() - whole.len() / 3;
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, shorter));
    assert_eq!(session.lane_clips(Lane::V1)[0].end(), shorter);
    assert_eq!(
        session.lane_clips(Lane::A1)[0].end(),
        shorter,
        "the linked sound was trimmed with the picture"
    );
    assert!(session.timeline_duration() < full, "and plays out earlier");

    // ...and dragged back out, as far as the file goes and no further.
    assert!(session.trim_clip(Lane::V1, 0, Edge::End, 9_999));
    assert_eq!(
        session.lane_clips(Lane::V1)[0],
        whole,
        "the whole take back"
    );

    // The head takes the in-point with it, so what plays at the new start
    // is source frame 10 rather than source frame 0.
    assert!(session.trim_clip(Lane::V1, 0, Edge::Start, 10));
    let head = session.lane_clips(Lane::V1)[0];
    assert_eq!((head.start, head.in_frame), (10, 10));
    assert_eq!(session.lane_clips(Lane::A1)[0].in_frame, 10, "sound too");
    assert_eq!(
        session.trim_room(Lane::V1, 0, Edge::Start).map(|r| r.0),
        Some(0),
        "and it may be pulled back out to the file's first frame"
    );

    assert!(session.undo(), "one step for the whole drag");
    assert_eq!(session.lane_clips(Lane::V1)[0], whole);
}

/// The move-a-clip-between-tracks path through the door the drop uses
/// ([`Player::move_clip`] calls exactly this): the clip changes row, the
/// *picture* comes from the new row afterwards -- which is what "it plays
/// from there" means -- and one undo puts it back.
#[test]
fn a_clip_dragged_onto_another_track_plays_from_it() {
    use engine::project::LaneKind;

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    let v2 = session.add_lane(LaneKind::Video);
    assert_eq!(session.video_clip_at(0.0), Some((Lane::V1, 0)));

    assert!(session.move_clip_to(Lane::V1, 0, v2, 0), "V1 -> V2");
    assert!(session.lane_clips(Lane::V1).is_empty(), "it left V1");
    assert_eq!(session.lane_clips(v2).len(), 1, "and landed on V2");
    assert_eq!(
        session.video_clip_at(0.0),
        Some((v2, 0)),
        "the picture now comes from V2"
    );
    assert_eq!(session.lane_clips(Lane::A1).len(), 1, "its sound stayed");

    // Dropped on a lane of the other kind it is refused and nothing moves --
    // the notice the front-end shows for it says which kind of lane to use.
    // (The other refusal, landing on another clip, is the engine's own test
    // `move_clip_keeps_the_frames_and_refuses_the_rest`.)
    assert!(!session.move_clip_to(v2, 0, Lane::A1, 0), "picture on A1");
    assert_eq!(session.lane_clips(v2).len(), 1, "and it stayed on V2");
    assert!(
        session.move_clip_to(v2, 0, Lane::V1, 0),
        "dragged back down"
    );

    // One undo per move, and each is a single step.
    assert!(session.undo(), "the drag back");
    assert_eq!(session.video_clip_at(0.0), Some((v2, 0)));
    assert!(session.undo(), "the drag up");
    assert_eq!(session.video_clip_at(0.0), Some((Lane::V1, 0)));
    assert!(session.lane_clips(v2).is_empty());
}

/// The add-a-track path end to end through the doors the buttons and the
/// drop use: `+ V` adds a row, a library row let go over it lands there and
/// nowhere else, Delete on it leaves the lanes under it where they are, and
/// undo takes the whole thing back one step at a time.
#[test]
fn a_track_can_be_added_dropped_on_edited_and_taken_back() {
    use engine::project::LaneKind;

    let mut session = PlaybackSession::open(asset("test_av.mp4")).expect("open the fixture");
    session.set_gain(0.0);
    assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1]);
    // What the `+ V` button asks for, and what the row it draws is called.
    let v2 = session.add_lane(LaneKind::Video);
    assert_eq!(v2.label(), "V2");
    assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1, v2]);
    assert!(session.lane_clips(v2).is_empty());
    // A library row let go over that row: the same door the Add button
    // uses, told which lane it was let go over.
    let path = session.sources()[0].path.clone();
    assert!(
        session
            .place_stream_at(1.0, &path, 0, Some(v2))
            .expect("its own file is on this timeline")
    );
    assert_eq!(session.lane_clips(v2).len(), 1, "the drop landed on V2");
    assert_eq!(session.lane_clips(v2)[0].start, 30, "at the playhead");
    // ...with the sound it came with, on the row of its own the drop added:
    // a file with audio dropped on a layer used to land silent.
    let a2 = Lane::new(LaneKind::Audio, 1);
    assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1, v2, a2]);
    assert_eq!(session.lane_clips(a2), session.lane_clips(v2), "same take");
    assert_eq!(
        session.lane_clips(v2)[0].link,
        session.lane_clips(a2)[0].link,
        "grouped, so a drag moves both"
    );
    // And nowhere else: the first pair is exactly as it was, one take each.
    assert_eq!(session.lane_clips(Lane::V1).len(), 1);
    assert_eq!(session.lane_clips(Lane::A1).len(), 1);
    // ...and it is a project that saves and opens again as it stands: a
    // grouped pair on a further row is what the file has to carry.
    let dir = engine::scratch::Scratch::dir("ve_layer");
    let file = dir.join("layer.edith");
    session.save_project(&file).expect("save the project");
    let back = PlaybackSession::open_project(&file).expect("it loads as it stands");
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(back.lanes(), session.lanes());
    for lane in back.lanes() {
        assert_eq!(back.lane_clips(lane), session.lane_clips(lane), "{lane:?}");
    }
    // Delete on the layer is a lift: it is laid over the timeline, so
    // closing a hole under it would drag the take beneath out of step with
    // it. The take on the first pair is still a take, and still ripples.
    assert!(!whole_take(&session, v2, 0));
    assert!(whole_take(&session, Lane::V1, 0));
    assert!(whole_take(&session, Lane::A1, 0));
    assert_eq!(a2.label(), "A2");
    assert!(session.lift_clip(v2, 0));
    assert!(session.lane_clips(v2).is_empty());
    assert_eq!(session.lane_clips(Lane::V1).len(), 1, "V1 stayed put");
    assert_eq!(
        session.lane_clips(a2).len(),
        1,
        "a lift takes the half it names: the sound is still there to lift"
    );
    assert!(session.lift_clip(a2, 0));

    // A second audio lane used to be what an mp4 export could not write --
    // it copied one AAC track and a mix is not a copy. It mixes now
    // (`export::copy_audio`), so no row is greyed by a count of lanes.
    for format in [Format::Mp4, Format::Av1, Format::Av1Mp4] {
        assert_eq!(
            format_refusal(&session, format),
            None,
            "two audio lanes are a mix, not a refusal"
        );
    }

    // Undo, one edit at a time and backwards: the lift of the sound, the
    // lift of the picture, the drop on V2 with the lane A2 it added, and
    // last the lane V2 -- an added track is one step like every other edit.
    for lanes in [4, 4, 3, 2] {
        assert!(session.undo());
        assert_eq!(session.lanes().len(), lanes);
    }
    assert_eq!(session.lanes(), vec![Lane::V1, Lane::A1]);
    assert_eq!(session.lane_clips(Lane::V1).len(), 1);
    assert_eq!(format_refusal(&session, Format::Mp4), None);
}

#[test]
fn a_source_is_only_ever_asked_for_its_peaks_once() {
    let (a, b) = (asset("test_av.mp4"), asset("test_av2.mp4"));
    // Two files, and two streams of the first: three envelopes, because
    // one file's two audio tracks are two different waveforms.
    let sources = [
        Source {
            path: a.clone(),
            audio_stream: 0,
        },
        Source {
            path: a.clone(),
            audio_stream: 1,
        },
        Source {
            path: b.clone(),
            audio_stream: 0,
        },
    ];
    let keys = |s: &[Source]| {
        s.iter()
            .map(|s| (s.path.clone(), s.audio_stream))
            .collect::<Vec<_>>()
    };
    let mut waves: HashMap<(PathBuf, usize), Wave> = HashMap::new();
    assert_eq!(unseen_sources(&sources, &waves), keys(&sources));
    // The entry goes in when the decode *starts*, so the sixty repaints a
    // second that happen while it runs must not start it again -- which is
    // what this asserts about a key whose value is not an answer yet.
    waves.insert((a.clone(), 0), Wave::Loading);
    assert_eq!(
        unseen_sources(&sources, &waves),
        keys(&sources[1..]),
        "the file's other stream is a key of its own"
    );
    // A file with no audio is an answer like any other: never re-asked.
    waves.insert((a, 1), Wave::Silent);
    waves.insert((b.clone(), 0), Wave::Silent);
    assert!(unseen_sources(&sources, &waves).is_empty());
    // The stream probe is per *file*: the two entries of `a` ask once.
    let mut streams: HashMap<PathBuf, Vec<StreamInfo>> = HashMap::new();
    assert_eq!(
        unseen_paths(&sources, &streams),
        vec![asset("test_av.mp4"), b]
    );
    streams.insert(asset("test_av.mp4"), Vec::new());
    assert_eq!(unseen_paths(&sources, &streams).len(), 1);
}

/// The auto switch, which is the whole of what an import asks: with it off no
/// film is handed to [`engine::proxy`] at all, and turning Proxies on is what
/// asks for the ones the project is missing -- there is no other door, so a
/// project that never wants a stand-in never spends a minute encoding one.
#[test]
fn a_project_that_makes_no_proxies_by_itself_starts_none_until_it_is_cut_on_them() {
    let sources = [Source {
        path: asset("test_av.mp4"),
        audio_stream: 0,
    }];
    let none: HashMap<PathBuf, ()> = HashMap::new();
    let one = vec![asset("test_av.mp4")];
    // Today's behaviour, both ways round: auto on starts them whether or not
    // the picture is cut on them -- the cache is warmed for later.
    assert_eq!(proxies_to_start(true, false, &sources, &none), one);
    assert_eq!(proxies_to_start(true, true, &sources, &none), one);
    // Auto off and not cut on them: nothing is started at all.
    assert!(
        proxies_to_start(false, false, &sources, &none).is_empty(),
        "an import started a stand-in the project asked it not to"
    );
    // ...and the switch is the ask: the film comes through the moment Proxies
    // goes on, because it was left unseen rather than marked done.
    assert_eq!(proxies_to_start(false, true, &sources, &none), one);
    // One start per film either way: a film already in the map is one already
    // asked about, sixty repaints a second notwithstanding.
    let seen: HashMap<PathBuf, ()> = HashMap::from([(asset("test_av.mp4"), ())]);
    assert!(proxies_to_start(true, true, &sources, &seen).is_empty());
}

#[test]
fn the_add_button_is_dead_unless_it_would_do_something() {
    let picked = (PathBuf::from("/m/0.mp4"), 0);
    assert!(can_add(Some(&picked), true, false));
    // Nothing picked, nothing to put it on, or an export reading the very
    // edit list this would change.
    assert!(!can_add(None, true, false));
    assert!(!can_add(Some(&picked), false, false));
    assert!(!can_add(Some(&picked), true, true));
}

#[test]
fn the_media_column_never_takes_the_picture_over() {
    for window in [360., 640., 1280., 1920., 3840.] {
        let w = crate::library_w(window);
        // The whole point of the budget: the picture keeps the majority of
        // the row at every size a window can be.
        assert!(w <= window / 3., "{window}px window gave the list {w}px");
        assert!(w >= LIBRARY_MIN_W.min(window / 3.), "{window}px: {w}px");
        assert!(w <= LIBRARY_MAX_W);
    }
    // It yields: a narrower window gives the list less, never the same.
    assert!(crate::library_w(640.) < crate::library_w(1280.));
    // Rows are clickable, so WCAG 2.5.8 binds them like every other target,
    // and a name over a timecode has to fit inside one.
    assert!(ROW_H >= HIT_MIN);
    assert!(SWATCH_W < LIBRARY_MIN_W);
}

#[test]
fn the_window_is_named_after_the_program_and_what_is_open() {
    assert_eq!(window_title("test_av.mp4"), "test_av.mp4 — edith");
    // An empty window is the program, not "no file open — edith".
    assert_eq!(window_title(NO_FILE), "edith");
}

#[test]
fn frac_along_measures_from_the_elements_own_left_edge() {
    // A ruler inset by the panel's 12 px padding: window x is not bar x.
    let bar: Bounds<Pixels> = Bounds {
        origin: point(px(12.), px(400.)),
        size: size(px(200.), px(6.)),
    };
    assert_eq!(frac_along(px(12.), bar), 0.);
    assert_eq!(frac_along(px(112.), bar), 0.5);
    assert_eq!(frac_along(px(212.), bar), 1.);
    // Outside the bar (a click that slid off) clamps, never extrapolates.
    assert_eq!(frac_along(px(0.), bar), 0.);
    assert_eq!(frac_along(px(9999.), bar), 1.);
    // Never painted: no division by zero, no NaN reaching seek().
    assert_eq!(frac_along(px(50.), Bounds::default()), 0.);

    // The equalizer's axis is the other one, and reads the same way.
    let graph: Bounds<Pixels> = Bounds {
        origin: point(px(12.), px(60.)),
        size: size(px(296.), px(EQ_GRAPH_H)),
    };
    assert_eq!(frac_down(px(60.), graph), 0.);
    assert_eq!(frac_down(px(60. + EQ_GRAPH_H / 2.), graph), 0.5);
    assert_eq!(frac_down(px(9999.), graph), 1.);
    // Never painted reads as flat: an unpainted graph must not slam a band
    // to +12 dB on the first press.
    assert_eq!(frac_down(px(50.), Bounds::default()), 0.5);
}
