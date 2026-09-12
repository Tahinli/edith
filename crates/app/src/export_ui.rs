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
        // A state word, the resolution list's rule: the six-word phrase this
        // row wore ran past `MENU_W` and was cut mid-word like the size
        // list's was. Nothing is resampled -- that is what the row means.
        "unchanged".into(),
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
/// find out from the size of the file. VP9 and VP8 are the ones this program
/// still only *reads*: the plugin decodes them and there is no encoder for
/// either here, so they are rows for the reason the refusals are rows at all —
/// a codec that opens but never comes back out is exactly the gap a user
/// would otherwise go looking for. AAC is not a row at all: it is what both
/// containers' sound *is*, never a file of its own.
///
/// A codec is one row, and the boxes it can be written into are the row's
/// containers: the same AV1 picture and the same AAC track go into a Matroska
/// file or into an mp4, and which one a user needs is about what has to play
/// the file, not about the encode. So the container is asked *once*, in a row
/// of its own, and only where there is more than one to ask about -- five
/// picture rows to read past were four of them saying the same codec twice.
pub(crate) const FORMATS: [(&[Format], &str, &str, &str); 9] = [
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
    (&[], "", "VP8", "AV1 above replaces it"),
];

/// The boxes one codec may be written into, in the order its container row
/// cycles them. Empty for a codec this program cannot write at all.
/// Not on the export moment any more: the Settings surface is what shows
/// this (`ui::settings_stance`). Kept and named rather than deleted -- the
/// state and its setter are the same ones the moment used to drive.
#[allow(dead_code)]
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
/// Not on the export moment any more: the Settings surface is what shows
/// this (`ui::settings_stance`). Kept and named rather than deleted -- the
/// state and its setter are the same ones the moment used to drive.
#[allow(dead_code)]
pub(crate) fn same_box(row: &[Format], current: Format) -> Option<Format> {
    row.iter()
        .copied()
        .find(|f| f.ext() == current.ext())
        .or_else(|| row.first().copied())
}

/// The next box for the same codec, wrapping -- what the container row's key
/// does. The format itself for a codec with only one, so the stroke cannot
/// change what it is not offering.
/// Not on the export moment any more: the Settings surface is what shows
/// this (`ui::settings_stance`). Kept and named rather than deleted -- the
/// state and its setter are the same ones the moment used to drive.
#[allow(dead_code)]
pub(crate) fn next_container(format: Format) -> Format {
    let row = containers(format);
    let at = row.iter().position(|&f| f == format).unwrap_or(0);
    row.get((at + 1) % row.len().max(1))
        .copied()
        .unwrap_or(format)
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
                    // A state word, not prose (DESIGN §8): "· the media's own"
                    // lost its last word to `MENU_W` on the row a user read
                    // ("word cut"). `source` is the word the sample-rate list
                    // already says the same fact in.
                    true => format!("{w}x{h} source"),
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
pub(crate) fn fit_choices(
    lane: Lane,
    idx: usize,
    current: FitPolicy,
    (w, h): (u32, u32),
) -> Vec<ChoiceRow> {
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
/// Not on the export moment any more: the Settings surface is what shows
/// this (`ui::settings_stance`). Kept and named rather than deleted -- the
/// state and its setter are the same ones the moment used to drive.
#[allow(dead_code)]
pub(crate) fn av1_hw_warning(format: Format, seat: EncoderSeat) -> Option<&'static str> {
    (seat == EncoderSeat::Hardware && matches!(format, Format::Av1 | Format::Av1Mp4))
        .then_some("AV1 on the GPU reset this machine's driver once — Software is the safe seat")
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

/// About how big the budget makes the file: every bit that is written --
/// the picture's rate and the sound's -- over the exported range, in bytes.
/// The container's own overhead is not in it, which is why the row says "≈".
pub(crate) fn estimated_bytes(bps: u64, seconds: f64) -> u64 {
    ((bps as f64) * seconds.max(0.) / 8.).round() as u64
}

/// A size as the budget row says it: gigabytes with two decimals once the
/// file is one, whole megabytes under that. Never a decimal megabyte -- the
/// estimate is an "≈" and a tenth of a megabyte is precision it does not have.
pub(crate) fn size_label(bytes: u64) -> String {
    match bytes >= 1_000_000_000 {
        true => format!("{:.2} GB", bytes as f64 / 1e9),
        false => format!("{} MB", (bytes as f64 / 1e6).round() as u64),
    }
}

/// The budget itself, in Mbps to one decimal: the number the lever reads.
pub(crate) fn rate_label(bps: u64) -> String {
    format!("{:.1}", bps as f64 / 1e6)
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

/// The moment's one number as the engine takes it. The budget is always
/// explicit now -- the lever opens on the engine's own automatic figure
/// ([`auto_bps`]) rather than on nothing, so there is no "Auto" left to send
/// as `None` -- and the engine clamps it to `MIN_BITRATE..MAX_EXPLICIT_BITRATE`
/// exactly where [`BPS_MIN`]/[`BPS_MAX`] do, so neither can disagree about the
/// edges.
///
/// The bitrate travels even for an audio format, where the engine ignores it:
/// one settings value, and a row the moment does not show cannot have been
/// changed.
pub(crate) fn export_settings(
    bitrate: u64,
    format: Format,
    audio_kbps: u32,
    seat: EncoderSeat,
) -> ExportSettings {
    ExportSettings {
        format,
        audio_kbps: Some(audio_kbps),
        bitrate: Some(bitrate),
        // The copy path (the old `Quality::Exact`) is off this surface: the
        // moment is one budget, and a copy is the absence of one.
        // `engine::export` keeps the path and its own tests; nothing in the
        // app asks for it.
        exact: false,
        seat,
        subtitles: Vec::new(),
        intra_only: false,
        keep_source_colour: false,
        range: None,
    }
}

/// The same settings with the budget taken out -- what the seat probe is asked
/// and keyed on ([`crate::Player::cache_export_seat`]). A bitrate does not
/// decide whether this machine has a VA-API seat or whether the picture's
/// packets can be copied; leaving it in the key meant every wheel notch on the
/// budget row invalidated the answer and opened a real VA-API encoder to ask it
/// again -- measured at ten opens for ten notches, 5-32 ms of GPU work each,
/// against the player's own decoder, which is the "freezes a little" this row
/// was reported for.
pub(crate) fn probe_settings(format: Format, audio_kbps: u32, seat: EncoderSeat) -> ExportSettings {
    ExportSettings {
        bitrate: None,
        ..export_settings(0, format, audio_kbps, seat)
    }
}

/// The budget's bounds, the engine's own (`engine::export`'s `MIN_BITRATE`
/// and `MAX_EXPLICIT_BITRATE`): a number outside them would be written as a
/// different one, so the lever and the field clamp to exactly these.
pub(crate) const BPS_MIN: u64 = 1_000_000;
pub(crate) const BPS_MAX: u64 = 50_000_000;

/// A wheel notch, and a notch with shift held: 0.1 and 1 Mbps. Fifty presses
/// is not a way across this range, and a tenth is the smallest step the row's
/// own readout can show.
pub(crate) const BPS_FINE: u64 = 100_000;
pub(crate) const BPS_COARSE: u64 = 1_000_000;

/// The engine's automatic figure for a picture of this size and rate
/// (`engine::export`'s `BITS_PER_PIXEL` rule and the range it clamps the
/// derived number into), which is what the lever opens on before anybody has
/// chosen: a budget row reading 0 Mbps -- the old `custom_mbps: 0` default --
/// was a promise to write an empty file.
const BITS_PER_PIXEL: f64 = 0.1;
const BPS_AUTO_MAX: u64 = 20_000_000;

pub(crate) fn auto_bps(width: u32, height: u32, fps: f64) -> u64 {
    let raw = f64::from(width) * f64::from(height) * fps * BITS_PER_PIXEL;
    (raw as u64).clamp(BPS_MIN, BPS_AUTO_MAX)
}

/// What was typed into the budget field, as a rate. Four ways to say one:
/// `850k` and `6.5M` are rates in their own units, a bare `6.5` is Mbps (the
/// unit the row reads in), and `1.2G` is a *file size* -- the one thing a
/// person actually wants to hit -- converted through the exported range and
/// the sound's own rate into the picture rate that lands there.
///
/// Clamped to the engine's bounds rather than refused: the moment shows the
/// parsed number immediately, so a clamp is visible in the same keystroke.
/// `None` is only ever "that is not a number".
pub(crate) fn parse_budget(text: &str, seconds: f64, audio_bps: u64) -> Option<u64> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.trim_end_matches("bps").trim_end_matches('b').trim_end();
    let (digits, unit) = match text.chars().last()? {
        c @ ('k' | 'm' | 'g') => (&text[..text.len() - c.len_utf8()], c),
        _ => (text, 'm'),
    };
    let n: f64 = digits.trim().parse().ok()?;
    if !n.is_finite() || n <= 0. {
        return None;
    }
    let bps = match unit {
        'k' => n * 1e3,
        'm' => n * 1e6,
        _ => {
            if seconds <= 0. {
                return None;
            }
            (n * 1e9 * 8. / seconds) - audio_bps as f64
        }
    };
    Some((bps.max(0.) as u64).clamp(BPS_MIN, BPS_MAX))
}

/// The faintest line on the moment: what the export resolved to on its own --
/// the codec, the sound, whether it is the whole film or the marked span, and
/// which seat writes the picture. A readout, never a control: everything in it
/// The span an export writes, in the words row 3 says it in: the marked
/// range where there is one, the whole timeline otherwise -- and "sound only"
/// for a file that carries no picture at all.
pub(crate) fn range_word(marks: Option<&str>, has_video: bool) -> String {
    match (marks, has_video) {
        (Some(marks), _) => format!("marks {marks}"),
        (None, true) => "whole film".to_string(),
        (None, false) => "sound only".to_string(),
    }
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
