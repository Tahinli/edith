//! What the export card offers and what it writes.

use crate::*;

/// The project resolutions [`Player::cycle_resolution`] offers, largest first.
/// A short list of the sizes people name; the media's own is cycled in beside
/// them, which is what makes the trip round come back to where it started.
pub(crate) const RESOLUTIONS: [(u32, u32); 5] = [
    (3840, 2160),
    (2560, 1440),
    (1920, 1080),
    (1280, 720),
    (854, 480),
];

/// The project frame rates the list offers, slowest first: the rates footage is
/// actually shot and delivered at, the NTSC ones written as the ratios they are
/// (`24000/1001`, not `23.976`) -- the engine conforms the timeline to the very
/// number it is handed, so a rate rounded here would be a rate no timescale can
/// name. The media's own is cycled in beside them
/// ([`frame_rate_ladder`]), which is what keeps the way back on the list.
pub(crate) const FRAME_RATES: [f64; 8] = [
    24_000. / 1001.,
    24.,
    25.,
    30_000. / 1001.,
    30.,
    50.,
    60_000. / 1001.,
    60.,
];

/// The project sound rates the list offers, slowest first. Unlike
/// [`RESOLUTIONS`] and [`FRAME_RATES`] there is no media rate to cycle in
/// beside them: a source's own rate is not a number this list has to name to
/// offer it, since [`Choice::SampleRate`]`(None)` -- "source" -- already means
/// exactly that, whatever the number turns out to be.
pub(crate) const SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 96_000];

/// The sound-rate list's rows: "source" first -- the derived rate, and the
/// row in force with nothing picked -- then every rate on offer, the one in
/// force marked. The same rows serve [`Pick::SampleRate`] with a session and
/// without one: `current` is [`PlaybackSession::sample_rate`] or
/// [`Player::pending_settings`]`.2`, and `None` means the same thing either
/// way -- nothing picked yet.
pub(crate) fn sample_rate_choices(current: Option<u32>) -> Vec<ChoiceRow> {
    let mut rows = vec![(
        Choice::SampleRate(None),
        "Source".into(),
        "the first audio source's own rate".into(),
        current.is_none(),
    )];
    rows.extend(SAMPLE_RATES.into_iter().map(|rate| {
        (
            Choice::SampleRate(Some(rate)),
            format!("{rate} Hz").into(),
            match rate {
                48_000 => "video standard".to_string(),
                44_100 => "CD/audio standard".to_string(),
                _ => "high-resolution".to_string(),
            }
            .into(),
            current == Some(rate),
        )
    }));
    rows
}

/// What the export card offers, top to bottom. Bitrate is the only thing the
/// encoder actually takes: the codec and the container are what this program
/// can write and nothing else, so the card states them rather than offering
/// them.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Quality {
    Auto,
    Low,
    Medium,
    High,
    Custom,
}

impl Quality {
    pub(crate) const ALL: [Quality; 5] = [
        Quality::Auto,
        Quality::Low,
        Quality::Medium,
        Quality::High,
        Quality::Custom,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Quality::Auto => "Auto",
            Quality::Low => "Low",
            Quality::Medium => "Medium",
            Quality::High => "High",
            Quality::Custom => "Custom",
        }
    }

    /// The figure the row stands for, said in the units the row is chosen by.
    pub(crate) fn detail(self, custom_mbps: u32) -> String {
        match self {
            Quality::Auto => "from the picture size and frame rate".to_string(),
            Quality::Custom => {
                format!("{custom_mbps} Mbps — wheel or ± steps, n types one, {MBPS_MIN}–{MBPS_MAX}")
            }
            other => format!(
                "{} Mbps",
                export_settings(other, 0, Format::Mp4, DEFAULT_AUDIO_KBPS, EncoderSeat::Auto)
                    .bitrate
                    .unwrap_or_default()
                    / 1_000_000
            ),
        }
    }
}

/// The primary pane's one row: the format-and-quality bundles most exports
/// actually are, named the way a person asks for one rather than by codec and
/// megabits apart. Every bundle is still exactly a [`Format`] and a
/// [`Quality`] the Advanced pane's own rows already know how to set -- this
/// is a shortcut to the pair, not a third setting kept beside them, so a
/// bundle picked here and a codec picked below never disagree about what is
/// in force. `Custom` sets nothing; it only opens the Advanced pane, for a
/// combination none of the bundles name.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ExportPreset {
    Web,
    Small,
    Master,
    AudioOnly,
    Custom,
}

impl ExportPreset {
    pub(crate) const ALL: [ExportPreset; 5] = [
        ExportPreset::Web,
        ExportPreset::Small,
        ExportPreset::Master,
        ExportPreset::AudioOnly,
        ExportPreset::Custom,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            ExportPreset::Web => "Web",
            ExportPreset::Small => "Small file",
            ExportPreset::Master => "Master",
            ExportPreset::AudioOnly => "Audio only",
            ExportPreset::Custom => "Custom",
        }
    }

    /// The key that picks this row, none of the sixteen the card's other rows
    /// already own (`n`, the digits, the codec row's `m/a/h/w/f/p/o`, and
    /// `c/q/b/e/d/g/r/s`): a letter out of the name itself where its initial is
    /// one of those (`smaLl`, `masTer`, `aUdio`), and a free one where every
    /// letter in the name already belongs to another row (`Web`'s w, e and b
    /// all are). `Custom` shares Advanced's own `s` -- the row does exactly what
    /// that key already does, not a second name for a third thing.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ExportPreset::Web => "v",
            ExportPreset::Small => "l",
            ExportPreset::Master => "t",
            ExportPreset::AudioOnly => "u",
            ExportPreset::Custom => "s",
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            ExportPreset::Web => "H.264 · MP4 · medium quality — plays everywhere",
            ExportPreset::Small => "AV1 · MP4 · low quality — smallest file",
            ExportPreset::Master => "HEVC · MP4 · high quality — intra-only, for re-editing later",
            ExportPreset::AudioOnly => "FLAC — lossless sound, no picture",
            ExportPreset::Custom => "pick a codec, quality and the rest below",
        }
    }

    /// The format and quality this bundle sets, or `None` for `Custom` --
    /// which changes nothing itself, it only opens the pane where the two are
    /// set apart.
    pub(crate) fn bundle(self) -> Option<(Format, Quality)> {
        match self {
            ExportPreset::Web => Some((Format::Mp4, Quality::Medium)),
            ExportPreset::Small => Some((Format::Av1Mp4, Quality::Low)),
            // HEVC, not H.264: a master is for cutting again later, and HEVC's
            // intra-only rows say why that is the one that keeps its promise --
            // every frame already a cut point, where H.264's are not.
            ExportPreset::Master => Some((Format::HevcMp4, Quality::High)),
            ExportPreset::AudioOnly => Some((Format::Flac, Quality::Auto)),
            ExportPreset::Custom => None,
        }
    }

    /// Which bundle the card's current format and quality already are, or
    /// `Custom` where they match none of them -- so the primary pane always
    /// says something true about what is actually going to be written rather
    /// than defaulting to a bundle nobody picked.
    pub(crate) fn from_state(format: Format, quality: Quality) -> ExportPreset {
        ExportPreset::ALL
            .into_iter()
            .find(|p| p.bundle() == Some((format, quality)))
            .unwrap_or(ExportPreset::Custom)
    }
}

/// The export card's format rows: the key that picks one, its name, and what it
/// writes -- or, where this program cannot write it, the reason it cannot. A
/// format with no entry at all would read as an oversight, and a menu of three
/// as a claim that nothing else exists -- so the refusals are rows too, dimmed
/// and unclickable.
///
/// `None` is exactly that kind of row, and there are two left. MP3 stopped
/// being one when `rusty_mp3` gave this project an Apache-2.0 encoder (the LGPL
/// `shine-rs` was the licence question, and it is not the only encoder any
/// more), and HEVC stopped being one when OxideAV's pure-Rust H.265 gave it an
/// encoder — an *intra-only* one, which the rows say rather than let a user
/// find out from the size of the file. VP9 is the one this program still only
/// *reads*: the plugin decodes it and there is no encoder for it here, so it is
/// a row for the reason the refusals are rows at all — a codec that opens but
/// never comes back out is exactly the gap a user would otherwise go looking
/// for. AAC is not a row at all: it is what both containers' sound *is*, never a
/// file of its own.
///
/// A codec is one row, and the boxes it can be written into are the row's
/// containers: the same AV1 picture and the same AAC track go into a Matroska
/// file or into an mp4, and which one a user needs is about what has to play
/// the file, not about the encode. So the container is asked *once*, in a row
/// of its own, and only where there is more than one to ask about -- five
/// picture rows to read past were four of them saying the same codec twice.
pub(crate) const FORMATS: [(&[Format], &str, &str, &str); 8] = [
    (
        &[Format::Mp4],
        "m",
        "H.264",
        "plays everywhere · AAC sound · MP4 only",
    ),
    (
        &[Format::Av1, Format::Av1Mp4],
        "a",
        "AV1",
        "smallest file for the picture · AAC sound",
    ),
    (
        &[Format::Hevc, Format::HevcMp4],
        "h",
        "HEVC",
        "intra-only, every frame a cut point — large files",
    ),
    (&[Format::Wav], "w", "WAV", "16-bit PCM — audio only"),
    (&[Format::Flac], "f", "FLAC", "lossless — audio only"),
    (&[Format::Mp3], "p", "MP3", "MPEG-1 Layer III — audio only"),
    (
        &[Format::Ogg],
        "o",
        "OGG",
        "Vorbis (rusty_vorbis) — quality-coded, stereo",
    ),
    (&[], "", "VP9", "AV1 above replaces it"),
];

/// The codec row a key picks, landed in the container already chosen: pressing
/// `a` after an mp4 gives AV1 *in mp4*, because a box decided once is not a
/// question again. A letter of the row's own name, spelled out in the table
/// rather than taken from the initial (`MP4` and `MP3` share one). Never a
/// digit -- those are the bitrate field's. `None` for every other stroke, the rows
/// that cannot be picked included.
pub(crate) fn format_key(key: &str, current: Format) -> Option<Format> {
    FORMATS
        .into_iter()
        .filter(|(_, stroke, ..)| !stroke.is_empty() && stroke.eq_ignore_ascii_case(key))
        .find_map(|(row, ..)| same_box(row, current))
}

/// The boxes one codec may be written into, in the order its container row
/// cycles them. Empty for a codec this program cannot write at all.
pub(crate) fn containers(format: Format) -> &'static [Format] {
    FORMATS
        .into_iter()
        .map(|(row, ..)| row)
        .find(|row| row.contains(&format))
        .unwrap_or(&[])
}

/// This row's format under the container `current` is already in, or the row's
/// first when it has no such box: an AV1 picked from a WAV lands in Matroska,
/// and picked from an mp4 stays in the mp4.
pub(crate) fn same_box(row: &[Format], current: Format) -> Option<Format> {
    row.iter()
        .copied()
        .find(|f| f.ext() == current.ext())
        .or_else(|| row.first().copied())
}

/// The next box for the same codec, wrapping -- what the container row's key
/// does. The format itself for a codec with only one, so the stroke cannot
/// change what it is not offering.
pub(crate) fn next_container(format: Format) -> Format {
    let row = containers(format);
    let at = row.iter().position(|&f| f == format).unwrap_or(0);
    row.get((at + 1) % row.len().max(1))
        .copied()
        .unwrap_or(format)
}

/// The next rate the Sound row offers, wrapping -- what its key does, and what
/// the row itself names so the stroke is never a guess. [`next_container`]'s
/// shape, for the same reason: one place decides what "next" means.
pub(crate) fn next_audio_kbps(kbps: u32) -> u32 {
    let at = AUDIO_KBPS.iter().position(|&k| k == kbps).unwrap_or(0);
    AUDIO_KBPS[(at + 1) % AUDIO_KBPS.len()]
}

/// Why the quality rows say nothing about this format, or `None` where they
/// decide the picture. Only a picture encoder is given a bitrate here: the two
/// lossless audio formats have none to give and MP3 is written at one fixed
/// figure, so a live row over either would be a control that changes nothing.
pub(crate) fn bitrate_refusal(format: Format) -> Option<&'static str> {
    match format {
        Format::Wav | Format::Flac => Some("lossless audio — no bitrate to pick"),
        Format::Mp3 => Some("sound only — its rate is the Sound row"),
        // The guard is the question and not a list of names: an audio format
        // added without a line of its own above would otherwise show live
        // quality rows over a file that has no picture to spend a bitrate on.
        // OGG is exactly that format -- and it wants this sentence rather than
        // one of its own, because its *Sound* row already says the Vorbis half
        // ("quality-coded — Vorbis holds no rate to pick") and two rows saying
        // the same thing is one of them wasted.
        _ if !format.has_video() => Some("sound only — no picture to spend a bitrate on"),
        _ => None,
    }
}

/// What one of the colour card's own strokes does. Its keys are card-local --
/// they mean nothing outside it -- so they are a table here rather than keymap
/// bindings, exactly as the export card's format initials are. Listed in
/// `keymap::FIXED` all the same, which is how the keys menu still says so.
pub(crate) enum ColorKey {
    Close,
    /// Steps down the four sliders, wrapping.
    Band(usize),
    /// Moves the picked slider, in [`COLOR_STEP`]s.
    Nudge(f32),
    Reset,
}

/// Why the silence card has nothing to scan on that clip, in its own voice: the
/// lane and index the user picked, the file it is of, and which of the two
/// soundless things it is. One place, because a still and a silent video are
/// the same answer to the same question -- "a box with a larger size than it"
/// is what the *demuxer* would say about a png, and it is not an answer.
///
/// Costs nothing: the scan reads a file and writes marks, so a refusal here
/// leaves the project (and its undo history) exactly where it was.
pub(crate) fn unscannable(lane: Lane, idx: usize, path: &Path) -> String {
    let what = match engine::is_image(path) {
        true => "is a picture",
        false => "is silent",
    };
    format!(
        "{} clip {} has no audio to scan — {} {what}",
        lane.label(),
        idx + 1,
        file_name(path)
    )
}

/// The half of a take whose *sound* the silence card scans: a link is one span
/// on however many lanes, so a card opened on the picture opens on the sound it
/// is grouped with. That is the lane the waveform is drawn on, and so the lane
/// the marks have to land on to be read against it -- and the ranges agree,
/// because a group is one span.
///
/// The clip itself for one already on an audio lane, for a detached picture,
/// and for a take whose sound is not on any lane: there is nothing better to
/// open on, and the refusal for a source with no audio at all is `scan`'s.
pub(crate) fn audio_half(session: &PlaybackSession, (lane, idx): (Lane, usize)) -> (Lane, usize) {
    if lane.kind == LaneKind::Audio {
        return (lane, idx);
    }
    let Some(link) = session.lane_clips(lane).get(idx).and_then(|c| c.link) else {
        return (lane, idx);
    };
    session
        .lanes()
        .into_iter()
        .filter(|l| l.kind == LaneKind::Audio)
        .find_map(|l| {
            session
                .lane_clips(l)
                .iter()
                .position(|c| c.link == Some(link))
                .map(|i| (l, i))
        })
        .unwrap_or((lane, idx))
}

/// The member of a caption's group on a media lane of the wanted kind: what a
/// card that is about media opens on when the hand is on the caption pinned to
/// it. `None` for a caption in no group with clips -- there are no pictures or
/// sound behind the words to be reaching at, and the card that opened anyway
/// would be a card of settings nothing plays.
pub(crate) fn caption_media_half(
    session: &PlaybackSession,
    (lane, idx): (Lane, usize),
    kind: LaneKind,
) -> Option<(Lane, usize)> {
    let link = session.sub_lane(lane).get(idx).and_then(|s| s.link)?;
    session
        .lanes()
        .into_iter()
        .filter(|l| l.kind == kind)
        .find_map(|l| {
            session
                .lane_clips(l)
                .iter()
                .position(|c| c.link == Some(link))
                .map(|i| (l, i))
        })
}

pub(crate) fn color_key(key: &str) -> Option<ColorKey> {
    Some(match key {
        ESCAPE => ColorKey::Close,
        "down" => ColorKey::Band(1),
        "up" => ColorKey::Band(COLOR_BANDS.len() - 1),
        "right" => ColorKey::Nudge(1.),
        "left" => ColorKey::Nudge(-1.),
        "r" => ColorKey::Reset,
        _ => return None,
    })
}

/// The band'th control of a grade, to read or to write. The order is
/// [`COLOR_BANDS`]', which is the order the card lists them in.
pub(crate) fn band_mut(params: &mut ColorParams, band: usize) -> &mut f32 {
    match band {
        0 => &mut params.brightness,
        1 => &mut params.contrast,
        2 => &mut params.saturation,
        _ => &mut params.tint,
    }
}

/// The line under the rows: what the picked format really writes, in the terms
/// a file is judged by afterwards.
/// The next policy round the cycle, in the order the action's label reads.
/// Every fit policy, in the order the list offers them -- which is the order
/// [`next_fit`] steps through them, pinned by the test below: a list and a
/// stroke that disagreed about what comes next would be two settings.
pub(crate) const FITS: [FitPolicy; 4] = [
    FitPolicy::Fit,
    FitPolicy::Fill,
    FitPolicy::Stretch,
    FitPolicy::Center,
];

pub(crate) fn next_fit(fit: FitPolicy) -> FitPolicy {
    match fit {
        FitPolicy::Fit => FitPolicy::Fill,
        FitPolicy::Fill => FitPolicy::Stretch,
        FitPolicy::Stretch => FitPolicy::Center,
        FitPolicy::Center => FitPolicy::Fit,
    }
}

/// What a person calls one, said as what it does to the picture.
pub(crate) fn fit_label(fit: FitPolicy) -> &'static str {
    match fit {
        FitPolicy::Fit => "fit (whole picture, bars)",
        FitPolicy::Fill => "fill (cropped, no bars)",
        FitPolicy::Stretch => "stretch (aspect broken)",
        FitPolicy::Center => "centre (1:1, no resample)",
    }
}

/// Every project resolution on offer, largest first: [`RESOLUTIONS`] with the
/// media's own size cycled in at its place by size -- so a project already at a
/// listed size does not see it twice, and the media's own shape, whatever it is,
/// is always on the list. The one order both the stroke and the list use.
pub(crate) fn resolution_ladder(native: (u32, u32)) -> Vec<(u32, u32)> {
    let mut sizes: Vec<(u32, u32)> = RESOLUTIONS.to_vec();
    if !sizes.contains(&native) {
        // By area, descending, like the list itself: the cycle then reads as one
        // ladder rather than a list with a stray rung at the end.
        let at = sizes
            .iter()
            .position(|&(w, h)| {
                u64::from(w) * u64::from(h) < u64::from(native.0) * u64::from(native.1)
            })
            .unwrap_or(sizes.len());
        sizes.insert(at, native);
    }
    sizes
}

/// The resolution list's rows: every rung of the ladder, the media's own said
/// so, and the one in force marked. A size is named by its height the way the
/// button that opens the list names it, with the full figure beside it.
pub(crate) fn resolution_choices(current: (u32, u32), native: (u32, u32)) -> Vec<ChoiceRow> {
    resolution_ladder(native)
        .into_iter()
        .map(|(w, h)| {
            (
                Choice::Size(w, h),
                format!("{h}p").into(),
                match (w, h) == native {
                    // Short enough to sit beside the label inside `MENU_W`:
                    // the longer phrase lost its last word to the truncation.
                    true => format!("{w}x{h} · the media's own"),
                    false => format!("{w}x{h}"),
                }
                .into(),
                (w, h) == current,
            )
        })
        .collect()
}

/// The resolution list's rows before any file is open: [`RESOLUTIONS`] plain,
/// with nothing marked unless a pick is already held
/// ([`Player::pending_settings`]) -- there is no media size to cycle in
/// beside them yet ([`resolution_choices`]'s `native`).
pub(crate) fn pending_resolution_choices(pending: Option<(u32, u32)>) -> Vec<ChoiceRow> {
    RESOLUTIONS
        .into_iter()
        .map(|(w, h)| {
            (
                Choice::Size(w, h),
                format!("{h}p").into(),
                format!("{w}x{h}").into(),
                Some((w, h)) == pending,
            )
        })
        .collect()
}

/// [`pending_resolution_choices`]'s sibling for the rate list.
pub(crate) fn pending_fps_choices(pending: Option<f64>) -> Vec<ChoiceRow> {
    FRAME_RATES
        .into_iter()
        .map(|fps| {
            (
                Choice::Fps(fps),
                format!("{} fps", fps_label(fps)).into(),
                match (fps - fps.round()).abs() < 0.001 {
                    true => String::new(),
                    false => "NTSC".to_string(),
                }
                .into(),
                Some(fps) == pending,
            )
        })
        .collect()
}

/// Every project frame rate on offer, slowest first: [`FRAME_RATES`] with the
/// media's own cycled in at its place by speed, so a project already cut at a
/// listed rate does not see it twice and the media's own rate -- the one a
/// project moved off it has no other way back to -- is always there.
/// [`resolution_ladder`]'s rule, for the other setting the project has of its
/// own.
pub(crate) fn frame_rate_ladder(native: f64) -> Vec<f64> {
    let mut rates = FRAME_RATES.to_vec();
    // Bit for bit: 23.976023976... is not 23.976, and a rate that read as
    // "already listed" when it is not would take the media's own off the list.
    if !rates.contains(&native) {
        let at = rates
            .iter()
            .position(|&fps| fps > native)
            .unwrap_or(rates.len());
        rates.insert(at, native);
    }
    rates
}

/// The rate list's rows: every rung of the ladder, the media's own said so, and
/// the one in force marked. Named as a person writes a rate ([`fps_label`]),
/// with what it is for beside it -- short, or the row loses its tail to the
/// truncation the resolution list already met.
pub(crate) fn fps_choices(current: f64, native: f64) -> Vec<ChoiceRow> {
    frame_rate_ladder(native)
        .into_iter()
        .map(|fps| {
            (
                Choice::Fps(fps),
                format!("{} fps", fps_label(fps)).into(),
                match fps == native {
                    true => "the media's own".to_string(),
                    // The rates that are a ratio are the ones nobody can tell
                    // from their neighbour by the label alone.
                    false => match (fps - fps.round()).abs() < 0.001 {
                        true => String::new(),
                        false => "NTSC".to_string(),
                    },
                }
                .into(),
                fps == current,
            )
        })
        .collect()
}

/// The fit list's rows: all four policies against the canvas they place a
/// picture on, since the word alone ("fill") says nothing about the size it is
/// filling -- which is the very thing the notice says after a stroke.
pub(crate) fn fit_choices(lane: Lane, idx: usize, current: FitPolicy, (w, h): (u32, u32)) -> Vec<ChoiceRow> {
    FITS.into_iter()
        .map(|fit| {
            (
                Choice::Fit(lane, idx, fit),
                fit_label(fit).into(),
                // The canvas alone, worded as the resolution list words a size:
                // the policy names are long and anything wordier here loses its
                // tail to the truncation.
                format!("{w}x{h}").into(),
                fit == current,
            )
        })
        .collect()
}

/// The sound-rate list's rows: every offered rate, the one in force marked, and
/// what each buys said in the fewest words that fit beside the label (a longer
/// phrase loses its tail to `MENU_W`'s truncation, as the two lists above say).
pub(crate) fn audio_rate_choices(current: u32) -> Vec<ChoiceRow> {
    AUDIO_KBPS
        .into_iter()
        .enumerate()
        .map(|(n, kbps)| {
            (
                Choice::AudioRate(kbps),
                format!("{kbps} kbps").into(),
                match (kbps, n) {
                    (DEFAULT_AUDIO_KBPS, _) => "the default",
                    (_, 0) => "smallest file",
                    (k, _) if k < DEFAULT_AUDIO_KBPS => "smaller file",
                    _ => "better sound",
                }
                .into(),
                kbps == current,
            )
        })
        .collect()
}

/// How a seat is named where a person reads it: the card row, the notice and
/// the list row all say the same word ([`engine::export::EncoderSeat`]).
pub(crate) fn encoder_label(seat: EncoderSeat) -> &'static str {
    match seat {
        EncoderSeat::Auto => "Auto",
        EncoderSeat::Hardware => "Hardware",
        EncoderSeat::Software => "Software",
    }
}

/// The encoder list's rows: all three seats, the one in force marked, and what
/// each one *does* beside it -- in the fewest words that fit inside `MENU_W`,
/// the truncation every list above already met. Always offered, whatever this
/// machine has: a row that vanished with the plugin would be a setting nobody
/// could find, and the answer to "is there a seat here?" is the planned line
/// under the rows ([`engine::export::planned_video`]), which is measured.
pub(crate) fn encoder_choices(current: EncoderSeat) -> Vec<ChoiceRow> {
    EncoderSeat::ALL
        .into_iter()
        .map(|seat| {
            (
                Choice::Encoder(seat),
                encoder_label(seat).into(),
                match seat {
                    EncoderSeat::Auto => "the GPU if there is one",
                    EncoderSeat::Hardware => "the GPU or a refusal",
                    EncoderSeat::Software => "the CPU, always",
                }
                .into(),
                seat == current,
            )
        })
        .collect()
}

/// The row under the encoder one where -- and only where -- a person has asked
/// for the GPU on an AV1 export: this project's own driver reset the GPU on the
/// vendored AV1 encoder (2026-08-10), so the pick is theirs to make and the
/// risk is theirs to be told about. `None` for every other pair, which is what
/// keeps this from becoming a row nobody reads.
pub(crate) fn av1_hw_warning(format: Format, seat: EncoderSeat) -> Option<&'static str> {
    (seat == EncoderSeat::Hardware && matches!(format, Format::Av1 | Format::Av1Mp4)).then_some(
        "AV1 on the GPU reset this machine's driver once — Software is the safe seat",
    )
}

/// How a rendition is named where a person reads it: the panel button, the
/// notice and the list row all say the same word ([`engine::tonemap::Preset`]).
pub(crate) fn tone_label(preset: Preset) -> &'static str {
    match preset {
        Preset::Reference => "Reference",
        Preset::Standard => "Standard",
        Preset::Vivid => "Vivid",
    }
}

/// The HDR list's rows: all three renditions, the one in force marked, and what
/// each one *is* beside it -- in the fewest words that fit inside `MENU_W`, the
/// truncation the three lists above already met. Always offered, whatever is on
/// the timeline: a setting that appeared and vanished with the media would be a
/// setting nobody could find, and the row says who it acts on instead.
pub(crate) fn tone_choices(current: Preset) -> Vec<ChoiceRow> {
    Preset::ALL
        .into_iter()
        .map(|preset| {
            (
                Choice::Tone(preset),
                tone_label(preset).into(),
                match preset {
                    Preset::Reference => "BT.2446-A, as published",
                    Preset::Standard => "brighter, player-like",
                    Preset::Vivid => "brightest, richer colour",
                }
                .into(),
                preset == current,
            )
        })
        .collect()
}

/// The next project resolution after `current`, over [`RESOLUTIONS`] with the
/// media's own size cycled in at its place by size -- so the trip round always
/// comes back to the media, whatever odd shape it is, and a project already at a
/// listed size does not see it twice.
pub(crate) fn next_resolution(current: (u32, u32), native: (u32, u32)) -> (u32, u32) {
    let sizes = resolution_ladder(native);
    let at = sizes.iter().position(|&s| s == current);
    // A project at a size nobody listed (a hand-edited file) joins the cycle at
    // the top rather than being stuck.
    sizes[at.map_or(0, |at| (at + 1) % sizes.len())]
}

/// The first of the two lines the card keeps above its button: what will be
/// *inside* the file. [`format_line`]'s codec and box, then the project's own
/// picture size and rate -- which is what a video export is written at however
/// many sizes and rates the media on the timeline are -- and last what the
/// sound will be, or that there is none. Every field here is one `ffprobe`
/// reads back off the finished file, so the line is checkable rather than a
/// promise.
pub(crate) fn summary_head(format: Format, picture: Option<((u32, u32), f64)>, audio: &str) -> String {
    let line = match picture.filter(|_| format.has_video()) {
        Some(((w, h), fps)) => {
            format!("{} · {w}x{h} · {} fps", format_line(format), fps_label(fps))
        }
        None => format_line(format).to_string(),
    };
    join_detail(&line, audio)
}

/// The second: where it lands, roughly how big, and what will encode the
/// picture -- the seat as the probe found it (`…` until it lands, never a
/// guess), which is what the running export then names on its progress line.
pub(crate) fn summary_tail(path: &Path, bytes: Option<u64>, seat: Option<&'static str>, video: bool) -> String {
    let size = bytes.map_or_else(String::new, |bytes| format!("≈ {}", size_label(bytes)));
    let seat = match (video, seat) {
        (true, Some(seat)) => seat,
        (true, None) => "encoder …",
        (false, _) => "",
    };
    join_detail(&join_detail(&file_name(path), &size), seat)
}

/// A frame rate as a person writes it: `30`, not `30.000`, and `23.976` for the
/// rate that is a ratio.
pub(crate) fn fps_label(fps: f64) -> String {
    match (fps - fps.round()).abs() < 0.001 {
        true => format!("{fps:.0}"),
        false => format!("{fps:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string(),
    }
}

/// About how big a *chosen* bitrate makes the file: the picture's bits over the
/// timeline's length, in bytes -- the unit the line is written in is
/// [`size_label`]'s to pick. `None` for `Auto`, whose figure is the encoder's to
/// decide, and for a format with no bitrate at all -- a number nobody picked is
/// not an estimate. The sound and the container's own overhead are not in it,
/// which is why the card says "≈".
pub(crate) fn estimated_bytes(bitrate: Option<u64>, duration: f64) -> Option<u64> {
    let bitrate = bitrate.filter(|&b| b > 0 && duration > 0.)?;
    Some((bitrate as f64 * duration / 8.).round() as u64)
}

/// A size in the largest unit that can state it, [`rate_scale`]'s rule:
/// megabytes for an export of any length, kilobytes below the one a whole
/// megabyte rounds away. A three second clip at the floor bitrate really is
/// 375 kB, and "≈ 0 MB" would be this line saying the file it is about to write
/// is empty -- the one thing the size field is there to deny. Never "0 kB"
/// either: an estimate that exists is at least a kilobyte of file.
pub(crate) fn size_label(bytes: u64) -> String {
    match (bytes as f64 / 1e6).round() as u64 {
        0 => format!("{} kB", (bytes as f64 / 1e3).round().max(1.) as u64),
        mb => format!("{mb} MB"),
    }
}

/// What is in the file and what box it is in, which is the head of the summary.
/// Terse on purpose: the fields after it (size, rate, sound) are what the line
/// is *for*, and a head that spent its width on prose used to push the whole
/// summary onto a second line -- what each codec means is on its row.
pub(crate) fn format_line(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "H.264 · MP4",
        Format::Av1 => "AV1 · MKV",
        Format::Av1Mp4 => "AV1 · MP4",
        Format::Hevc => "HEVC intra · MKV",
        Format::HevcMp4 => "HEVC intra · MP4",
        // The three whose codec *is* their box: naming it twice would be the
        // only field on this line that says nothing.
        Format::Wav => "16-bit PCM · WAV",
        Format::Flac => "FLAC · lossless",
        Format::Mp3 => "MP3 · lossy",
        Format::Ogg => "Vorbis · OGG",
    }
}

/// The row a format is picked by, which is what a refusal calls it: the codec,
/// since the container is a row of its own now -- `AV1`, not the `mkv` such a
/// file is named with.
pub(crate) fn format_label(format: Format) -> &'static str {
    FORMATS
        .iter()
        .find(|(row, ..)| row.contains(&format))
        .map_or("EXPORT", |(_, _, label, _)| *label)
}

/// The destination under a format: `take.export.mp4` becomes `take.export.wav`.
/// The stem is untouched, so a name typed into the save dialog survives a
/// change of mind about the format -- only the extension is the format's to say.
pub(crate) fn retarget(path: &std::path::Path, format: Format) -> PathBuf {
    let mut path = path.to_path_buf();
    path.set_extension(format.ext());
    path
}

/// The card's rows as the engine takes them. `Auto` leaves the bitrate to the
/// exporter, which derives it from the picture; the fixed rows are figures that
/// hold from 720p to 1080p, and a typed one is passed exactly as typed -- the
/// engine clamps every explicit bitrate to 1..50 Mbps (`MAX_EXPLICIT_BITRATE`), so this
/// must not clamp it a second time and disagree about where the edge is.
///
/// The bitrate travels even for an audio format, where the engine ignores it:
/// one settings value, and a row the card has dimmed cannot have been changed.
pub(crate) fn export_settings(
    quality: Quality,
    custom_mbps: u32,
    format: Format,
    audio_kbps: u32,
    seat: EncoderSeat,
) -> ExportSettings {
    ExportSettings {
        format,
        // Always travels, exactly as the picture's bitrate does above: the
        // engine ignores it where nothing encodes the sound, and a row the card
        // has dimmed cannot have been changed.
        audio_kbps: Some(audio_kbps),
        bitrate: match quality {
            Quality::Auto => None,
            Quality::Low => Some(2_000_000),
            Quality::Medium => Some(6_000_000),
            Quality::High => Some(12_000_000),
            Quality::Custom => Some(u64::from(custom_mbps) * 1_000_000),
        },
        // The card's own row now ([`encoder_choices`]), kept with the project:
        // it was a `VE_SW_ENC` env pin and nothing else, which is a switch
        // nobody exporting a film would ever find.
        seat,
        // The picked track is put on by `start_export`, which is the only
        // caller that writes a file; the rest of them are asking about the
        // bitrate and the format.
        subtitles: Vec::new(),
        // Neither is a delivery setting: both belong to the stand-ins
        // [`engine::proxy`] writes -- every frame a key frame costs bits
        // nobody delivering wants, and a file kept in its source's colour
        // space is one for this editor to read back rather than to hand
        // anybody.
        intra_only: false,
        keep_source_colour: false,
        // Set by `start_export`, which knows the player's mark; this helper is
        // also asked for the estimate alone, which has no range to give.
        range: None,
    }
}

/// What the engine will code an explicit bitrate at, in whole Mbps: outside
/// this it clamps (`export.rs` `MIN_BITRATE`/`MAX_EXPLICIT_BITRATE`), so a
/// number typed past either end would be written as a different one. The field
/// refuses it instead of clamping quietly -- a card that changes the user's
/// number without saying so is the one thing a field like this must never do.
///
/// The ceiling was 20, which was never a limit of any encoder here: it was the
/// top of the range the exporter *derives* an automatic bitrate in, borrowed as
/// the cap on a typed one. A 1080p master or a 4K edit wants more than that, so
/// the asked-for rate has its own ceiling now and this is it.
pub(crate) const MBPS_MIN: u32 = 1;
pub(crate) const MBPS_MAX: u32 = 50;

/// How many digits the field takes. Two reach the ceiling; the third is there so
/// a number *past* it can be typed whole and refused in its own words, rather
/// than being dropped keystroke by keystroke.
pub(crate) const MBPS_DIGITS: usize = 3;

/// A number being typed into a card row: the digits so far and, once a commit
/// has been refused, why. Text-field semantics on a card that has no text field
/// -- typing, backspace, arrows that step, enter that commits and escape that
/// gives up -- held as state and driven by the root's key handler, since
/// nothing in these cards takes gpui focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NumberEdit {
    pub(crate) text: String,
    pub(crate) refusal: Option<String>,
}

impl NumberEdit {
    /// Starts on the number the row already carries, so backspace edits it
    /// rather than the field opening empty over a value that is still in force.
    /// Zero is no number at all -- it is what the card opens at, before anyone
    /// has typed one.
    pub(crate) fn new(value: u32) -> Self {
        NumberEdit {
            text: match value {
                0 => String::new(),
                v => v.to_string(),
            },
            refusal: None,
        }
    }

    /// A digit against what is there. The one past [`MBPS_DIGITS`] is refused
    /// *out loud*: a keystroke dropped in silence is how the old digit capture
    /// left the card showing a number the user had already typed past.
    pub(crate) fn digit(&mut self, digit: u32) {
        if self.text.chars().count() >= MBPS_DIGITS {
            self.refusal = Some(format!("{MBPS_DIGITS} digits is already past the ceiling"));
            return;
        }
        match char::from_digit(digit, 10) {
            Some(c) => {
                self.text.push(c);
                self.refusal = None;
            }
            None => self.refusal = Some("digits only".into()),
        }
    }

    /// Erases the last digit, and the refusal with it: the number on screen has
    /// changed, so the reason the old one was refused no longer describes it.
    pub(crate) fn backspace(&mut self) {
        self.text.pop();
        self.refusal = None;
    }

    /// The arrows, which is how a number gets picked rather than typed. Steps
    /// from what is in the field -- an empty one starts at the floor, so the
    /// first press up is `MBPS_MIN` and not a jump to some remembered value --
    /// and stays inside the range, because a step is a walk through the legal
    /// numbers rather than a way out of them.
    pub(crate) fn step(&mut self, by: i32) {
        let at = self.text.parse::<i32>().unwrap_or(MBPS_MIN as i32 - by.signum());
        self.text = (at + by)
            .clamp(MBPS_MIN as i32, MBPS_MAX as i32)
            .to_string();
        self.refusal = None;
    }

    /// The number, or `None` with the reason recorded where the row will read
    /// it. Never clamped: 45 committed as 20 is a number the user did not type.
    pub(crate) fn commit(&mut self) -> Option<u32> {
        match commit_mbps(&self.text) {
            Ok(mbps) => Some(mbps),
            Err(why) => {
                self.refusal = Some(why);
                None
            }
        }
    }

    /// What the row shows while it is being typed into: the digits, the caret
    /// that says they are landing *here*, and either the refusal or the two
    /// keys that end the edit.
    pub(crate) fn detail(&self) -> String {
        format!(
            "{}▏ Mbps — {}",
            self.text,
            match &self.refusal {
                Some(why) => why.as_str(),
                None => "enter commits · esc cancels",
            }
        )
    }
}

/// A typed bitrate as the card takes it, or the reason it is not one. The words
/// are the row's: they are what the field shows in place of its hint.
pub(crate) fn commit_mbps(text: &str) -> Result<u32, String> {
    match text.parse::<u32>() {
        Ok(mbps) if (MBPS_MIN..=MBPS_MAX).contains(&mbps) => Ok(mbps),
        Ok(0) => Err(format!("0 is not a rate — {MBPS_MIN}–{MBPS_MAX} Mbps")),
        Ok(mbps) => Err(format!("{mbps} is past the {MBPS_MAX} Mbps ceiling")),
        Err(_) => Err(format!("type a number — {MBPS_MIN}–{MBPS_MAX} Mbps")),
    }
}

/// Whether a stroke gets out of a running export. `ctrl+escape` does -- a chord
/// and not the bare key, which used to end an hour of encoding on the stroke a
/// hand reaches for to shut a menu it has already shut. Bare escape does nothing
/// at all here: the progress card is not dismissable, so there is nothing left
/// for it to mean. Whatever the keymap has on cancel works too, so rebinding it
/// adds a way rather than replacing this one -- and that binding is what the
/// card shows.
pub(crate) fn cancels_export(key: &str, ctrl: bool, action: Option<ActionId>) -> bool {
    (ctrl && key == ESCAPE) || action == Some(ActionId::CancelExport)
}

/// A clip's share of the lane. A timeline with no length reads as one full-width
/// box rather than as NaN, which gpui would carry into layout.
/// Why this timeline cannot be written in `format`, if it cannot.
///
/// An audio-only timeline is the one both picture formats refuse: every frame of
/// it is a gap, so the file would be a black picture over the sound. The engine
/// refuses it too (`export::start`); this is what greys the row before a
/// destination has been picked.
///
/// It is the *only* reason left. A second audio lane, a speeded clip, a source
/// no mp4 sample table holds: each of those used to grey the MP4 row, because
/// the mp4 path could only *copy* an AAC track. It re-encodes where a copy
/// cannot say what the timeline says (`export::copy_audio`), so none of them is
/// a refusal any more -- and every video format carries the sound, so there is
/// nothing here that is one format's alone.
pub(crate) fn format_refusal(session: &PlaybackSession, format: Format) -> Option<String> {
    if !format.has_video() {
        return None;
    }
    let picture = session
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Video && !session.lane_clips(lane).is_empty());
    match picture {
        true => None,
        false => Some(format!(
            "no picture — {} would be black; export WAV, FLAC, MP3 or OGG",
            format.name()
        )),
    }
}
