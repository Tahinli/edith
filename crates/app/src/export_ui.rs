//! What the export card offers and what it writes.

use crate::*;

/// The project resolutions the project-resolution list offers, largest area
/// first. A short list of the sizes people name, landscape and the two
/// phone-and-square shapes ([`NEW_SIZES`] offers the same ones on a new
/// project); the media's own is cycled in beside them, which is what makes the
/// trip round come back to where it started.
pub(crate) const RESOLUTIONS: [(u32, u32); 7] = [
    (3840, 2160),
    (2560, 1440),
    (1920, 1080),
    (1080, 1920),
    (1080, 1080),
    (1280, 720),
    (854, 480),
];

/// A size's name in the list, plain words: the height for a landscape frame
/// (`1080p`), the two numbers for a tall or square one (`1080×1920`), which a
/// `p` would misname.
pub(crate) fn size_name(w: u32, h: u32) -> String {
    match w > h {
        true => format!("{h}p"),
        false => format!("{w}\u{d7}{h}"),
    }
}

/// A size's row label in the list: [`size_name`], with the shape said for a
/// tall or square frame -- `1080×1920 (vertical 9:16)`, `1080×1080 (square)`.
pub(crate) fn size_row_label(w: u32, h: u32) -> String {
    match (w > h, w == h, w * 16 == h * 9) {
        (true, _, _) => size_name(w, h),
        (_, true, _) => format!("{w}\u{d7}{h} (square)"),
        (_, _, true) => format!("{w}\u{d7}{h} (vertical 9:16)"),
        _ => format!("{w}\u{d7}{h} (vertical)"),
    }
}

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
        "Match source".into(),
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
/// more), and HEVC stopped being one when this project gained a pure-Rust
/// H.265 encoder — an *intra-only* one, which the rows say rather than let a
/// user find out from the size of the file. VP9 and VP8 are the ones this program
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
pub(crate) const FORMATS: [(&[Format], &str, &str, &str); 12] = [
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
        "I+P GOPs in software — far smaller than intra-only",
    ),
    (&[Format::Wav], "w", "WAV", "16-bit PCM — audio only"),
    (&[Format::Flac], "f", "FLAC", "lossless — audio only"),
    (&[Format::Mp3], "p", "MP3", "MPEG-1 Layer III — audio only"),
    (
        &[Format::Ogg],
        "o",
        "Ogg",
        "Vorbis — variable quality, stereo",
    ),
    (
        &[Format::Opus],
        "u",
        "Opus",
        "Opus in an Ogg file — voice and podcasts, small",
    ),
    (&[Format::Still, Format::PngSequence], "s", "PNG", "lossless pictures · no sound"),
    (&[Format::Gif], "i", "GIF", "animated · 256 colours · no sound"),
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

/// Every fit policy, in the order the list offers them.
pub(crate) const FITS: [FitPolicy; 4] = [
    FitPolicy::Fit,
    FitPolicy::Fill,
    FitPolicy::Stretch,
    FitPolicy::Center,
];

/// What the easing list calls one: the words an editor uses for the curve.
pub(crate) fn ease_label(ease: engine::transform::KeyEase) -> &'static str {
    use engine::transform::KeyEase;
    match ease {
        KeyEase::Linear => "Linear",
        KeyEase::EaseIn => "Ease in",
        KeyEase::EaseOut => "Ease out",
        KeyEase::Ease => "Ease in-out",
        KeyEase::Hold => "Hold",
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
                size_row_label(w, h).into(),
                match (w > h, (w, h) == native) {
                    // Plain words (DESIGN §8): a landscape row's size is its
                    // detail, and the media's own size says what it matches. A
                    // tall or square row already names its shape in its label.
                    (true, true) => format!("{w}\u{d7}{h} (matches your media)"),
                    (true, false) => format!("{w}\u{d7}{h}"),
                    (false, true) => "matches your media".to_string(),
                    (false, false) => String::new(),
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
                size_row_label(w, h).into(),
                match w > h {
                    true => format!("{w}x{h}"),
                    false => String::new(),
                }
                .into(),
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
                fps_row_label(fps).into(),
                String::new().into(),
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
                fps_row_label(fps).into(),
                match fps == native {
                    true => "Match the media".to_string(),
                    false => String::new(),
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
                format!("{kbps} kb/s").into(),
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

/// The seats in the order every list and chip row shows them: Software first,
/// the GUI's default seat, then the two that may open the GPU encoder.
pub(crate) const ENCODER_SEATS: [EncoderSeat; 3] =
    [EncoderSeat::Software, EncoderSeat::Auto, EncoderSeat::Hardware];

/// How a seat is named where a person reads it: the export sheet's rows,
/// Settings' chips and the notice all say the same word
/// ([`engine::export::EncoderSeat`]), and [`encoder_note`] says what it does.
pub(crate) fn encoder_label(seat: EncoderSeat) -> &'static str {
    match seat {
        EncoderSeat::Auto => "Auto",
        EncoderSeat::Hardware => "Hardware",
        EncoderSeat::Software => "Software",
    }
}

/// What a seat does, in the fewest words: the same note beside the export
/// sheet's row and under Settings' chips.
pub(crate) fn encoder_note(seat: EncoderSeat) -> &'static str {
    match seat {
        EncoderSeat::Auto => "GPU encoder if any",
        EncoderSeat::Hardware => "GPU encoder or fail",
        EncoderSeat::Software => "software encoder, always",
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
                encoder_note(seat).into(),
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
        .then_some("AV1 on the GPU reset this machine's driver once — Software is the safe choice")
}

/// How a rendition is named where a person reads it: the panel button, the
/// notice and the list row all say the same word ([`engine::tonemap::Preset`]).
pub(crate) fn tone_label(preset: Preset) -> &'static str {
    match preset {
        Preset::Reference => "Faithful",
        Preset::Standard => "Balanced",
        Preset::Vivid => "Bright",
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
                    Preset::Reference => "true to the standard",
                    Preset::Standard => "like a normal player",
                    Preset::Vivid => "brightest, richer colour",
                }
                .into(),
                preset == current,
            )
        })
        .collect()
}

/// A frame rate as a person writes it: `30`, not `30.000`, and `23.976` for the
/// rate that is a ratio.
/// A rate's row label as a person names it: the number and "fps", and for the
/// NTSC ratios (23.976, 29.97, 59.94) what the rate is for -- "29.97 fps (NTSC
/// video)" -- so the ratio is never a bare number nobody can place.
pub(crate) fn fps_row_label(fps: f64) -> String {
    let base = format!("{} fps", fps_label(fps));
    match (fps - fps.round()).abs() < 0.001 {
        true => base,
        false => match fps < 25. {
            true => format!("{base} (NTSC film)"),
            false => format!("{base} (NTSC video)"),
        },
    }
}

pub(crate) fn fps_label(fps: f64) -> String {
    match (fps - fps.round()).abs() < 0.001 {
        true => format!("{fps:.0}"),
        false => format!("{fps:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string(),
    }
}

/// The budget itself, in Mbps to one decimal: the number the lever reads.
pub(crate) fn rate_label(bps: u64) -> String {
    format!("{:.1}", bps as f64 / 1e6)
}

/// How fast this computer last wrote a file, in media seconds per wall second.
/// Kept in memory for this session only. Zero until one export has landed.
static MEASURED_PACE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Remembers the pace of a finished export. A cancelled or failed one says
/// nothing about the machine, so the caller passes only the ones that landed.
pub(crate) fn note_pace(media_seconds: f64, wall_seconds: f64) {
    if media_seconds > 0. && wall_seconds > 0. {
        let pace = media_seconds / wall_seconds;
        MEASURED_PACE.store(pace.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// The pace the last finished export measured, if one has: the time estimate
/// never invents one (a guess of "real time" was off by 5x either way).
pub(crate) fn measured_pace() -> Option<f64> {
    match MEASURED_PACE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => None,
        bits => Some(f64::from_bits(bits)),
    }
}

/// How long writing `media_seconds` of film is expected to take at a measured
/// `pace`, in words a person plans around. Never "0 s": a job that rounds to
/// nothing says "about 1 s". `None` pace is "estimating…", not a number.
pub(crate) fn time_words(media_seconds: f64, pace: Option<f64>) -> String {
    let Some(pace) = pace.filter(|p| *p > 0. && p.is_finite()) else {
        return "estimating\u{2026}".to_string();
    };
    let secs = (media_seconds.max(0.) / pace).round().max(1.) as u64;
    match secs {
        0..90 => format!("about {secs} s"),
        90..600 => {
            let r = secs.div_ceil(5) * 5;
            format!("about {} min {} s", r / 60, r % 60)
        }
        _ => format!("about {} min", (secs as f64 / 60.).round() as u64),
    }
}

/// What a name that is a source of the project says, under the field and on the
/// Export button.
pub(crate) const SOURCE_NAME_TAKEN: &str = "A source file already has this name.";

/// Whether `target` is one of the project's own source files, by name or by
/// what it resolves to on disk. Writing an export over a source would destroy
/// the footage the timeline plays from, so the sheet refuses that outright
/// rather than asking to replace it.
pub(crate) fn is_a_source(target: &std::path::Path, sources: &[&std::path::Path]) -> bool {
    let same_file = |a: &std::path::Path| match (std::fs::canonicalize(a), std::fs::canonicalize(target)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == target,
    };
    sources.iter().any(|&source| same_file(source))
}

/// The row a format is picked by, which is what a refusal calls it: the codec,
/// since the container is a row of its own now -- `AV1`, not the `mkv` such a
/// file is named with.
pub(crate) fn format_label(format: Format) -> &'static str {
    FORMATS
        .iter()
        .find(|(row, ..)| row.contains(&format))
        .map_or("Export", |(_, _, label, _)| *label)
}

/// The file name a typed stem lands as under a format: `stem.ext`, or the bare folder name for a
/// numbered run, which is written into a folder named as the stem.
pub(crate) fn typed_name(stem: &str, format: Format) -> String {
    if format == Format::PngSequence {
        stem.to_string()
    } else {
        format!("{stem}.{}", format.ext())
    }
}

/// The destination under a format: `take.export.mp4` becomes `take.export.wav`.
/// The stem is untouched, so a name typed into the save dialog survives a
/// change of mind about the format -- only the extension is the format's to say.
pub(crate) fn retarget(path: &std::path::Path, format: Format) -> PathBuf {
    let mut path = path.to_path_buf();
    // A numbered run is written into a folder named as the destination, so it has no extension.
    if format == Format::PngSequence {
        path.set_extension("");
        return path;
    }
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
        // Filled by the session's export path from its chapter markers, which this
        // free function cannot see; an empty list means "use the project's chapters".
        chapters: Vec::new(),
        audio_kbps: Some(audio_kbps),
        audio_bit_depth: None,
        bitrate: Some(bitrate),
        // The copy path (the old `Quality::Exact`) is off this surface: the
        // moment is one budget, and a copy is the absence of one.
        // `engine::export` keeps the path and its own tests; nothing in the
        // app asks for it.
        exact: false,
        seat,
        subtitles: Vec::new(),
        no_soft_subtitles: false,
        intra_only: false,
        keep_source_colour: false,
        range: None,
        burn_subtitles: None,
        size: None,
        video_bit_depth: None,
        loudness: None,
        prores_profile: ProResProfile::default(),
        hdr: HdrOutput::default(),
        hevc_speed: None,
        gif: GifSettings::default(),
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
/// different one, so the lever and the field clamp to exactly these. The floor
/// is 100 kbps, not a megabit: a sub-Mbps delivery is an export too, and at a
/// megabit this row could not say `0.6` at all.
pub(crate) const BPS_MIN: u64 = 100_000;
pub(crate) const BPS_MAX: u64 = 50_000_000;

/// A wheel notch, and a notch with shift held: 0.1 and 1 Mbps. Five hundred
/// tenths is not a way across this range, and a tenth is the smallest step
/// the row's own readout can show.
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
    let text = text
        .trim_end_matches("bps")
        .trim_end_matches('b')
        .trim_end();
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
/// How long a marked span is, said so a sliver cannot pass for a range: a
/// single frame is "1 frame", under a second is frames, then seconds, then
/// minutes and seconds.
pub(crate) fn span_word(frames: u32, fps: f64) -> String {
    let secs = f64::from(frames) / fps;
    match frames {
        1 => "1 frame".to_string(),
        _ if secs < 1. => format!("{frames} frames"),
        _ if secs < 60. => format!("{} s", secs.round() as u64),
        _ => format!("{} min {} s", (secs / 60.) as u64, (secs % 60.).round() as u64),
    }
}

/// A point on the timeline in the same plain words as [`span_word`]: "2 s",
/// "1.5 s", "1 min 5 s" -- a timecode is for the transport, not a summary.
pub(crate) fn point_word(frame: u32, fps: f64) -> String {
    let secs = f64::from(frame) / fps;
    let tenths = (secs * 10.).round() / 10.;
    match secs {
        _ if secs < 60. && (tenths - tenths.round()).abs() < 1e-9 => format!("{} s", tenths as u64),
        _ if secs < 60. => format!("{tenths:.1} s"),
        _ => format!("{} min {} s", (secs / 60.) as u64, (secs % 60.).round() as u64),
    }
}

/// The seat a window opens a project with. `Auto` can resolve to VA-API
/// encode, which has crashed this machine's GPU, so the GUI starts on
/// software -- also for a project saved with `Auto`, which is what every
/// project saved before this carries. `Hardware` is only ever a pick.
pub(crate) fn gui_default_seat(session: &mut engine::PlaybackSession) {
    if session.encoder_seat() == EncoderSeat::Auto {
        // A default the load applies, not a pick: it leaves no undo step behind.
        session.default_encoder_seat(EncoderSeat::Software);
    }
}

/// A clip's share of the lane. A timeline with no length reads as one full-width
/// box rather than as NaN, which gpui would carry into layout.
/// Why this timeline cannot be written in `format`, if it cannot.
///
/// An audio-only timeline is one the picture formats refuse: every frame of it
/// is a gap, so a Matroska or WebM file would be a black picture over the sound.
/// The engine refuses those too (`export::start`); this is what greys the row
/// before a destination has been picked.
///
/// An mp4 of it is not refused: the engine writes it as the sound alone (AAC in
/// an mp4, `export::run_aac`), so the mp4 rows stay open.
///
/// It is the *only* reason left. A second audio lane, a speeded clip, a source
/// no mp4 sample table holds: each of those used to grey the MP4 row, because
/// the mp4 path could only *copy* an AAC track. It re-encodes where a copy
/// cannot say what the timeline says (`export::copy_audio`), so none of them is
/// a refusal any more -- and every video format carries the sound, so there is
/// nothing here that is one format's alone.
pub(crate) fn format_refusal(session: &PlaybackSession, format: Format) -> Option<String> {
    // A picture file is refused on a timeline with no picture too, as a video file is.
    if (!format.has_video() && !is_picture_set(format)) || format.ext() == "mp4" {
        return None;
    }
    let picture = session
        .lanes()
        .into_iter()
        .any(|lane| lane.kind == LaneKind::Video && !session.lane_clips(lane).is_empty());
    match picture {
        true => None,
        false => Some(format!(
            "no picture — {} would be black; export an mp4, M4A, WAV, FLAC, MP3 or Ogg",
            format.name()
        )),
    }
}

// ---------------------------------------------------------------------------
// The export sheet (DESIGN §9): the model behind the centred sheet, kept as
// plain values and functions so every behaviour on it is a test and none of
// them needs a window.
// ---------------------------------------------------------------------------

use engine::export::{GifSettings, LoudnessTarget};
use engine::export_presets as presets;
use std::collections::HashMap;

/// The sheet's open/closed state for the "More settings" disclosure, in the
/// same `folds` file every other fold lives in.
pub(crate) const EXPORT_MORE_FOLD: &str = "export-more";

/// Which of the two destination fields is being typed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SheetField {
    Name,
    Folder,
}

/// What a finished export left behind, for the sheet's "done" face.
#[derive(Debug, Clone)]
pub(crate) struct ExportDone {
    pub path: PathBuf,
    pub bytes: u64,
    pub wall_secs: f64,
}

/// A one-second trial write, run when the sheet opens so the time estimate is
/// a measurement of this machine and this format rather than a guess.
pub(crate) struct PaceBench {
    pub key: &'static str,
    pub handle: ExportHandle,
    pub started: std::time::Instant,
    pub media_secs: f64,
    pub out: PathBuf,
}

/// Everything the sheet keeps beside the settings the player already holds
/// (`format`, `audio_kbps`, `custom_bps`, the encoder seat).
#[derive(Default)]
pub(crate) struct ExportSheet {
    /// The user picked "whole timeline" although there are marks.
    pub whole: bool,
    /// The file's canvas; `None` follows the project.
    pub size: Option<(u32, u32)>,
    /// AV1 sample depth; `None` follows the source.
    pub bit_depth: Option<u8>,
    /// Sound levelling target; `None` leaves the mix as it is.
    pub loudness: Option<LoudnessTarget>,
    /// The ProRes profile of a ProRes file.
    pub prores: ProResProfile,
    /// The HDR transfer of an HEVC file; SDR is none.
    pub hdr: HdrTransfer,
    /// The software HEVC encoder's speed preset.
    pub hevc_speed: HevcSpeed,
    /// Bits per sample of a ProRes file's sound: `Some(16)`, else 24.
    pub sound_bits: Option<u8>,
    /// Captions are left out of the file (`false` keeps the tracks).
    pub no_captions: bool,
    /// The GIF's width, frame rate and looping ([`Format::Gif`]).
    pub gif: GifSettings,
    /// A Details row was changed since the last preset was picked: the preset
    /// box says "Custom (from X)" even if the settings happen to equal another preset.
    pub custom: bool,
    /// The preset tile last picked: the tie-break when two presets would
    /// describe the settings equally. Whether it still *applies* is derived
    /// ([`matching_preset`]), which is what makes "Custom" automatic.
    pub preset: Option<&'static str>,
    /// The destination field being typed into, and its text.
    pub field: Option<(SheetField, String)>,
    /// The destination field's text is selected whole: the next letter replaces it (Ctrl+A, or
    /// the field just opened on its name).
    pub field_all: bool,
    /// A plain sentence about the last field edit that was refused.
    pub note: Option<String>,
    pub done: Option<ExportDone>,
    pub failed: Option<String>,
    /// The export was cancelled and the cancel landed: the sheet says so.
    pub cancelled: bool,
    /// Measured pace per codec family; `None` is "tried and could not tell".
    pub paces: HashMap<&'static str, Option<f64>>,
    pub bench: Option<PaceBench>,
}

/// The settings a preset can change, as the player holds them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Picked {
    pub format: Format,
    /// The picture's budget; zero is "automatic".
    pub bps: u64,
    pub audio_kbps: u32,
    pub size: Option<(u32, u32)>,
    pub depth: Option<u8>,
    pub loudness: Option<LoudnessTarget>,
    pub prores: ProResProfile,
    pub hdr: HdrTransfer,
}

impl Picked {
    fn settings(&self) -> ExportSettings {
        ExportSettings {
            format: self.format,
            bitrate: (self.bps > 0).then_some(self.bps),
            audio_kbps: Some(self.audio_kbps),
            size: self.size,
            video_bit_depth: self.depth,
            loudness: self.loudness,
            prores_profile: self.prores,
            hdr: HdrOutput { transfer: self.hdr, ..HdrOutput::default() },
            ..ExportSettings::default()
        }
    }
}

/// `base` with a preset laid over it by the engine's own rule
/// ([`presets::apply`]): the GUI holds no copy of the table.
pub(crate) fn picked_with(base: Picked, preset: &presets::Preset) -> Picked {
    let s = presets::apply(preset, base.settings());
    Picked {
        format: s.format,
        bps: s.bitrate.unwrap_or(base.bps),
        audio_kbps: clamp_audio_kbps(s.audio_kbps.unwrap_or(base.audio_kbps)),
        size: s.size,
        depth: s.video_bit_depth,
        loudness: s.loudness,
        prores: s.prores_profile,
        hdr: s.hdr.transfer,
    }
}

/// The sound rates the engine writes are [`AUDIO_KBPS`]; a preset that asks for
/// more (AAC 384) gets the top of the list, which is what the engine clamps it to.
pub(crate) fn clamp_audio_kbps(kbps: u32) -> u32 {
    AUDIO_KBPS
        .iter()
        .copied()
        .filter(|&k| k <= kbps)
        .max()
        .unwrap_or(AUDIO_KBPS[0])
}

/// Whether laying `preset` over `picked` would change nothing.
pub(crate) fn preset_applies(picked: Picked, preset: &presets::Preset) -> bool {
    picked_with(picked, preset) == picked
}

/// The preset the settings are exactly, preferring `prefer` -- or `None`,
/// which the sheet calls Custom.
pub(crate) fn matching_preset(
    picked: Picked,
    prefer: Option<&str>,
) -> Option<&'static presets::Preset> {
    let table = presets::presets();
    prefer
        .and_then(presets::by_id)
        .filter(|p| preset_applies(picked, p))
        .or_else(|| table.iter().find(|p| preset_applies(picked, p)))
}

/// The codec in the words a person uses, and the key a measured pace is kept
/// under (every file of a codec is written at about the same pace).
pub(crate) fn codec_word(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "H.264",
        Format::Av1 | Format::Av1Mp4 | Format::Webm => "AV1",
        Format::Hevc | Format::HevcMp4 => "HEVC",
        Format::Wav => "WAV",
        Format::Flac => "FLAC",
        Format::Mp3 => "MP3",
        Format::Ogg => "Vorbis",
        Format::Opus => "Opus",
        Format::M4a => "AAC",
        Format::ProRes => "ProRes",
        Format::Still => "Still image",
        Format::PngSequence => "Numbered pictures",
        Format::Gif => "GIF",
    }
}

/// Every file the sheet's format list offers: the format, its name and a
/// plain-words reason to choose it. Video first, then sound alone.
pub(crate) const SHEET_FORMATS: [(Format, &str, &str); 15] = [
    (Format::Mp4, "MP4 \u{b7} H.264", "plays everywhere"),
    (Format::Av1Mp4, "MP4 \u{b7} AV1", "smaller files, newer players"),
    (Format::Av1, "MKV \u{b7} AV1", "smaller files, newer players"),
    (Format::HevcMp4, "MP4 \u{b7} HEVC", "small files, phones and TVs"),
    (Format::Hevc, "MKV \u{b7} HEVC", "small files, editing friendly"),
    (Format::Webm, "WebM \u{b7} AV1", "for web pages"),
    (Format::M4a, "M4A", "sound only, plays everywhere"),
    (Format::Mp3, "MP3", "sound only, plays on anything"),
    (Format::Ogg, "Ogg", "sound only, free format"),
    (Format::Opus, "Opus", "sound only, small, for voice"),
    (Format::Flac, "FLAC", "sound only, lossless"),
    (Format::Wav, "WAV", "sound only, uncompressed"),
    (Format::Still, "PNG \u{b7} still", "one picture, no sound"),
    (Format::PngSequence, "PNG \u{b7} sequence", "one picture per frame, no sound"),
    (Format::Gif, "GIF", "animated, no sound"),
];

/// A format's name as the sheet's list writes it.
pub(crate) fn sheet_format_name(format: Format) -> &'static str {
    SHEET_FORMATS
        .iter()
        .find(|(f, ..)| *f == format)
        .map_or("File", |(_, name, _)| *name)
}

/// What a preset tile says it makes: the canvas and codec for a picture, the
/// file and rate for sound. `project` is the size a preset with none keeps.
pub(crate) fn preset_result(preset: &presets::Preset, project: Option<(u32, u32)>) -> String {
    match preset.target.output {
        presets::Output::AudioOnly => format!(
            "{} \u{b7} {}",
            sheet_format_name(preset.format),
            match preset.format {
                Format::Wav | Format::Flac => "lossless".to_string(),
                _ => format!("{} kbps", preset.audio_kbps),
            }
        ),
        presets::Output::Video => {
            let size = match preset.target.size.or(project) {
                Some((w, h)) => format!("{w}\u{d7}{h}"),
                None => "project size".to_string(),
            };
            format!("{size} \u{b7} {}", codec_word(preset.format))
        }
    }
}

/// The two radio rows of the range: the whole timeline and the marks, each
/// with its duration, and why the marks are off when they are.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RangeOptions {
    pub whole: String,
    pub marks: Option<String>,
    pub marks_off: Option<&'static str>,
}

/// `valid` is the marked span as the export would really use it
/// ([`Player::export_range`]); `raw` is whether any marks are set at all, so
/// a stale pair is told apart from none.
pub(crate) fn range_options(
    valid: Option<(u32, u32)>,
    raw: bool,
    timeline_frames: u32,
    fps: f64,
) -> RangeOptions {
    RangeOptions {
        // One decimal, so the whole timeline reads as a length beside the segments.
        whole: format!("{:.1} s long", f64::from(timeline_frames) / fps.max(1.)),
        marks: valid.map(|(start, end)| {
            format!(
                "{} \u{2013} {} \u{b7} {} long",
                point_word(start, fps),
                point_word(end, fps),
                span_word(end - start, fps)
            )
        }),
        marks_off: match (valid, raw) {
            (Some(_), _) => None,
            (None, true) => Some("marks are past the end"),
            (None, false) => Some("Set In and Out on the timeline first"),
        },
    }
}

/// How big the file may come out, in bytes: a single figure where the bits are
/// fixed by the settings, a span where a quality-driven encoder decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SizeEstimate {
    pub low: u64,
    pub high: u64,
}

/// What a file of `seconds` is expected to weigh. `video_fixed` is false for
/// an encoder that works to a quality and not to the budget (the GPU's AV1),
/// which makes the picture's part a span. Sound formats follow their own maths:
/// PCM is arithmetic, FLAC a typical 50-75 % of it, Vorbis is quality coded.
pub(crate) fn size_estimate(
    format: Format,
    video_bps: u64,
    audio_kbps: u32,
    sound: bool,
    seconds: f64,
    sample_rate: u32,
    video_fixed: bool,
) -> SizeEstimate {
    let secs = seconds.max(0.);
    let bytes = |bps: f64| (bps * secs / 8.).round() as u64;
    let audio = f64::from(audio_kbps) * 1_000.;
    let pcm = f64::from(sample_rate) * 2. * 16.;
    let (low, high) = match format {
        Format::Wav => (pcm * 0.995, pcm * 1.005),
        Format::Flac => (pcm * 0.5, pcm * 0.75),
        Format::Ogg => (80_000., 160_000.),
        Format::Mp3 | Format::M4a | Format::Opus => (audio * 0.98, audio * 1.04),
        _ => {
            let a = if sound { audio } else { 0. };
            let v = video_bps as f64;
            match video_fixed {
                // A budget is an average the encoder lands near, plus the
                // container's own bookkeeping.
                true => ((v + a) * 0.97, (v + a) * 1.04),
                false => (v * 0.35 + a, v * 1.1 + a),
            }
        }
    };
    let (low, high) = (bytes(low), bytes(high));
    // A job with something to write is never "0 MB".
    let floor = u64::from(secs > 0.) * 1_000;
    SizeEstimate {
        low: low.max(floor),
        high: high.max(floor),
    }
}

/// A byte count as a person reads it: KB under a megabyte, one decimal under
/// ten, whole megabytes, then gigabytes with two decimals.
pub(crate) fn bytes_words(bytes: u64) -> String {
    let b = bytes as f64;
    match bytes {
        0..1_000_000 => format!("{} KB", ((b / 1e3).round() as u64).max(1)),
        1_000_000..10_000_000 => format!("{:.1} MB", b / 1e6),
        10_000_000..1_000_000_000 => format!("{} MB", (b / 1e6).round() as u64),
        _ => format!("{:.2} GB", b / 1e9),
    }
}

/// The estimate as a sentence part: "about 38 MB" when it is tight, a span
/// ("about 20 to 60 MB") when it cannot be known closer.
pub(crate) fn size_words(est: SizeEstimate) -> String {
    let (low, high) = (bytes_words(est.low), bytes_words(est.high));
    match (est.high as f64) <= (est.low as f64) * 1.15 || low == high {
        true => format!("about {}", bytes_words((est.low + est.high) / 2)),
        false => {
            // "20 to 60 MB": the unit once when both ends share it.
            let (n_low, u_low) = low.split_once(' ').unwrap_or((&low, ""));
            let (_, u_high) = high.split_once(' ').unwrap_or((&high, ""));
            match u_low == u_high {
                true => format!("about {n_low} to {high}"),
                false => format!("about {low} to {high}"),
            }
        }
    }
}

/// What the warnings look at.
pub(crate) struct WarnCtx {
    pub format: Format,
    /// The canvas the file is drawn at.
    pub target: (u32, u32),
    /// The project's own canvas.
    pub project: (u32, u32),
    pub fps: f64,
    pub video_bps: u64,
    pub timeline_has_picture: bool,
    pub timeline_has_sound: bool,
    pub seconds: f64,
}

/// A picture rate below this many bits per pixel per frame looks blocky.
const LOW_BITS_PER_PIXEL: f64 = 0.03;

/// The things worth saying before the button is pressed, in plain words.
pub(crate) fn warnings(ctx: &WarnCtx) -> Vec<String> {
    let mut out = Vec::new();
    if ctx.seconds <= 0. {
        out.push("Range is empty: nothing to export".to_string());
        return out;
    }
    let pixels = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    if ctx.format.has_video() {
        if pixels(ctx.target) > pixels(ctx.project) {
            out.push(format!(
                "Stretched up from {}\u{d7}{} \u{2014} won't look sharper",
                ctx.project.0, ctx.project.1
            ));
        }
        let per_pixel = ctx.video_bps as f64 / (pixels(ctx.target) as f64 * ctx.fps).max(1.);
        if per_pixel < LOW_BITS_PER_PIXEL {
            out.push(format!(
                "Low bitrate for {}\u{d7}{}: may look blocky",
                ctx.target.0,
                ctx.target.1
            ));
        }
        if !ctx.timeline_has_sound {
            out.push("No sound on the timeline: the file is silent".to_string());
        }
    } else if !is_picture_set(ctx.format) {
        // A picture set writes no sound, so the sound warnings do not apply to it.
        if ctx.timeline_has_picture {
            out.push("Sound only: the picture is left out".to_string());
        }
        if !ctx.timeline_has_sound {
            out.push("No sound on the timeline: nothing to write".to_string());
        }
    }
    out
}

/// The container a format is written into, as a person names the file kind.
pub(crate) fn container_word(format: Format) -> &'static str {
    match format {
        Format::Mp4 | Format::Av1Mp4 | Format::HevcMp4 => "MP4",
        Format::Av1 | Format::Hevc => "MKV",
        Format::Webm => "WebM",
        Format::M4a => "M4A",
        Format::Mp3 => "MP3",
        Format::Ogg => "Ogg",
        Format::Opus => "Opus",
        Format::Flac => "FLAC",
        Format::Wav => "WAV",
        Format::ProRes => "MOV",
        Format::Still | Format::PngSequence => "PNG",
        Format::Gif => "GIF",
    }
}

/// The sound codec a format writes beside (or instead of) its picture.
pub(crate) fn sound_codec_word(format: Format) -> &'static str {
    match format {
        Format::Webm | Format::Opus => "Opus",
        Format::Ogg => "Vorbis",
        Format::Mp3 => "MP3",
        Format::Flac => "FLAC",
        Format::Wav | Format::ProRes => "PCM",
        _ => "AAC",
    }
}

/// Every (container, codec) pair the export writes, in the order the two
/// Details lists show them. The format is the engine's one value for the pair.
pub(crate) const COMBOS: [(&str, &str, Format); 16] = [
    ("MP4", "H.264", Format::Mp4),
    ("MP4", "HEVC", Format::HevcMp4),
    ("MP4", "AV1", Format::Av1Mp4),
    ("MKV", "HEVC", Format::Hevc),
    ("MKV", "AV1", Format::Av1),
    ("WebM", "AV1", Format::Webm),
    ("M4A", "AAC", Format::M4a),
    ("MP3", "MP3", Format::Mp3),
    ("Ogg", "Vorbis", Format::Ogg),
    ("Opus", "Opus", Format::Opus),
    ("FLAC", "FLAC", Format::Flac),
    ("WAV", "PCM", Format::Wav),
    ("PNG", "Still image", Format::Still),
    ("PNG", "Numbered pictures", Format::PngSequence),
    ("GIF", "GIF", Format::Gif),
    ("MOV", "ProRes", Format::ProRes),
];

/// The format for picking `container`, keeping the codec in force where that
/// container has it (MP4 to MKV keeps AV1), else the container's first.
pub(crate) fn format_in_container(container: &str, current: Format) -> Option<Format> {
    let codec = codec_word(current);
    COMBOS
        .iter()
        .find(|(c, k, _)| *c == container && (*k == codec || (*k == "PCM" && codec == "WAV")))
        .or_else(|| COMBOS.iter().find(|(c, ..)| *c == container))
        .map(|(.., f)| *f)
}

/// What a seat is, in a plain half line: the Details list's note.
pub(crate) fn seat_plain(seat: EncoderSeat) -> &'static str {
    match seat {
        EncoderSeat::Software => "CPU; slower, works everywhere",
        EncoderSeat::Hardware => "GPU; faster, needs support",
        EncoderSeat::Auto => "Picks the best encoder on this computer",
    }
}

/// What the summary reads off the settings.
pub(crate) struct SummaryIn<'a> {
    pub format: Format,
    pub target: (u32, u32),
    pub fps: f64,
    pub bps: u64,
    pub audio_kbps: u32,
    pub sound: bool,
    pub seat: EncoderSeat,
    pub depth: Option<u8>,
    pub prores: ProResProfile,
    pub hdr: HdrTransfer,
    pub hevc_speed: HevcSpeed,
    pub sound_bits: Option<u8>,
    pub size: &'a str,
    /// What a GIF asks for ([`Format::Gif`]); the other formats ignore it.
    pub gif: GifSettings,
}

/// The three summary lines, each with its fixed label, read off the current
/// settings and short enough not to wrap in the dock:
/// `Video  H.264 . 1920x1080 . 25 fps . 12 Mb/s`,
/// `Audio  AAC stereo . 320 kb/s`,
/// `File   MP4 . Software . ~3.1 MB . ~1 s`.
/// `time` is the export-time words ("" for none).
pub(crate) fn summary_rows(i: &SummaryIn, time: &str) -> [(&'static str, String); 3] {
    let dot = " \u{b7} ";
    let (container, codec) = (container_word(i.format), codec_word(i.format));
    let nb = |n: String, unit: &str| format!("{n}\u{a0}{unit}");
    let (video, audio) = if is_picture_set(i.format) {
        // Pictures and no sound: the file's own size, and its rate for a GIF.
        let picture = match i.format {
            Format::Gif => {
                let w = i.gif.width.min(i.target.0.max(1));
                let h = (f64::from(i.target.1) * f64::from(w) / f64::from(i.target.0.max(1))).round() as u32;
                let loops = if i.gif.loop_forever { "loops" } else { "plays once" };
                format!("GIF{dot}{w}\u{d7}{h}{dot}{}{dot}{loops}", nb(i.gif.fps.to_string(), "fps"))
            }
            Format::PngSequence => format!("PNG{dot}{}\u{d7}{}{dot}one per frame", i.target.0, i.target.1),
            _ => format!("PNG{dot}{}\u{d7}{}", i.target.0, i.target.1),
        };
        (picture, "None".to_string())
    } else if i.format.has_video() {
        let prores = i.format == Format::ProRes;
        let hdr = i.format.is_hevc() && i.hdr != HdrTransfer::Sdr;
        let mut v = vec![match i.depth.filter(|_| matches!(i.format, Format::Av1 | Format::Av1Mp4 | Format::Webm) || (i.format.is_hevc() && !hdr)) {
            _ if prores => i.prores.name().to_string(),
            _ if hdr => hdr_word(i.hdr).to_string(),
            Some(d) => format!("{codec} {d}-bit"),
            None => codec.to_string(),
        }];
        v.push(format!("{}\u{d7}{}", i.target.0, i.target.1));
        v.push(nb(fps_label(i.fps), "fps"));
        // A ProRes profile is its own rate; no budget to quote.
        if !prores {
            v.push(nb(rate_label(i.bps).trim_end_matches(".0").to_string(), "Mb/s"));
        }

        let a = match (i.sound, prores) {
            (true, true) => format!("PCM stereo{dot}{}-bit", i.sound_bits.unwrap_or(24)),
            (true, false) => format!("{} stereo{dot}{}", sound_codec_word(i.format), nb(i.audio_kbps.to_string(), "kb/s")),
            (false, _) => "None".to_string(),
        };
        (v.join(dot), a)
    } else {
        let spec = match i.format {
            Format::Wav => "16-bit".to_string(),
            Format::Flac => "lossless".to_string(),
            Format::Ogg => "variable quality".to_string(),
            _ => nb(i.audio_kbps.to_string(), "kb/s"),
        };
        let a = match i.sound {
            true => format!("{} stereo{dot}{spec}", sound_codec_word(i.format)),
            false => "None".to_string(),
        };
        ("Sound only".to_string(), a)
    };
    let size = i.size.replacen("about ", "~", 1);
    let mut file = vec![container.to_string()];
    if i.format.has_video() {
        file.push(seat_short(i.seat).to_string());
        // The default search says nothing; a quicker preset is named. Only the
        // software encoder has one.
        if i.format.is_hevc() && i.seat != EncoderSeat::Hardware && i.hevc_speed != HevcSpeed::Default {
            file.push(hevc_speed_label(i.hevc_speed).to_string());
        }
    }
    // A picture set's size is not the bitrate's estimate, and one still is not a length of time.
    if !is_picture_set(i.format) {
        file.push(size);
    }
    if !time.is_empty() && i.format != Format::Still {
        file.push(time.to_string());
    }
    [("Video", video), ("Audio", audio), ("File", file.join(dot))]
}

/// The HDR choice as a person names it.
pub(crate) fn hdr_word(hdr: HdrTransfer) -> &'static str {
    match hdr {
        HdrTransfer::Sdr => "Off",
        HdrTransfer::Pq => "HDR10",
        HdrTransfer::Hlg => "HLG",
    }
}

/// The three encoding speeds as the Details row names them.
pub(crate) const HEVC_SPEEDS: [HevcSpeed; 3] = [HevcSpeed::Default, HevcSpeed::Fast, HevcSpeed::Faster];

pub(crate) fn hevc_speed_label(s: HevcSpeed) -> &'static str {
    match s {
        HevcSpeed::Default => "Best quality",
        HevcSpeed::Fast => "Faster",
        HevcSpeed::Faster => "Fastest",
    }
}

/// What a speed costs, in plain words: the line under the row.
pub(crate) fn hevc_speed_plain(s: HevcSpeed) -> &'static str {
    match s {
        HevcSpeed::Default => "Thorough search: the smallest file for the quality.",
        HevcSpeed::Fast => "About 1.5\u{d7} quicker; the file is up to 3% bigger.",
        HevcSpeed::Faster => "About 2\u{d7} quicker; the file can be a quarter bigger.",
    }
}

fn hevc_speed_key(s: HevcSpeed) -> &'static str {
    match s {
        HevcSpeed::Default => "default",
        HevcSpeed::Fast => "fast",
        HevcSpeed::Faster => "faster",
    }
}

/// The HDR row's value in full words.
pub(crate) fn hdr_row_word(hdr: HdrTransfer) -> &'static str {
    match hdr {
        HdrTransfer::Sdr => "Off",
        HdrTransfer::Pq => "HDR10 (PQ)",
        HdrTransfer::Hlg => "HLG",
    }
}

/// The ProRes profile in a half line: what it is for.
pub(crate) fn profile_plain(profile: ProResProfile) -> &'static str {
    match profile {
        ProResProfile::Proxy => "smallest, for rough edits",
        ProResProfile::Lt => "light, small files",
        ProResProfile::Standard => "good for most work",
        ProResProfile::Hq => "high-quality master",
        ProResProfile::P4444 => "full colour, can carry alpha",
        ProResProfile::P4444Xq => "highest quality, huge files",
    }
}

/// The encoder seat in one short word for the summary's File line.
pub(crate) fn seat_short(seat: EncoderSeat) -> &'static str {
    match seat {
        EncoderSeat::Software => "Software",
        EncoderSeat::Hardware => "Hardware",
        EncoderSeat::Auto => "Auto",
    }
}

/// The export time in the few words the File line has room for.
pub(crate) fn time_short(words: &str) -> String {
    match words.strip_suffix(" to export") {
        Some(t) => t.replacen("about ", "~", 1),
        None if words.starts_with("estimating") => "estimating\u{2026}".to_string(),
        None => String::new(),
    }
}

/// The id of the UI's own first preset: H.264 MP4 at the project's size and rate.
pub(crate) const MATCH_PROJECT: &str = "match_project";

/// What a preset row says it makes, in one line: "1080p · H.264 · 8 Mb/s" for
/// a picture (the project's size when the preset keeps it), the file and rate
/// for sound.
pub(crate) fn preset_spec(preset: &presets::Preset, project: Option<(u32, u32)>) -> String {
    match preset.target.output {
        presets::Output::AudioOnly => preset_result(preset, project),
        presets::Output::Video => {
            let size = match preset.target.size.or(project) {
                Some((w, h)) => format!("{w}\u{d7}{h}"),
                None => "project size".to_string(),
            };
            let rate = match preset.bitrate {
                _ if preset.format == Format::ProRes => "10-bit".to_string(),
                Some(bps) if preset.hdr != HdrTransfer::Sdr => format!("{} Mb/s HDR", rate_label(bps).trim_end_matches(".0")),
                Some(bps) => format!("{} Mb/s", rate_label(bps).trim_end_matches(".0")),
                None => "auto rate".to_string(),
            };
            format!("{size} \u{b7} {} \u{b7} {rate}", codec_word(preset.format))
        }
    }
}

/// Whether a scrolled body has content under its bottom edge: `offset_y` is
/// gpui's (zero at the top, negative going down), `max_y` the furthest it
/// goes. Half a pixel of slack so a body scrolled to its end reads as ended.
pub(crate) fn more_below_shows(offset_y: f32, max_y: f32) -> bool {
    max_y > 0.5 && -offset_y < max_y - 0.5
}

/// The path with a number in its name until nothing is there: "Keep both".
pub(crate) fn unique_path(path: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if !exists(path) {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    (2..)
        .map(|n| {
            let name = match &ext {
                Some(ext) => format!("{stem} ({n}).{ext}"),
                None => format!("{stem} ({n})"),
            };
            path.with_file_name(name)
        })
        .find(|candidate| !exists(candidate))
        .unwrap_or_else(|| path.to_path_buf())
}

/// The three answers to "that file already exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Overwrite {
    Replace,
    KeepBoth,
    Cancel,
}

/// The folder field's text as a directory: `~` is home, and it has to exist.
pub(crate) fn parse_folder(text: &str, home: Option<&str>) -> Result<PathBuf, &'static str> {
    let text = text.trim();
    let path = match (text.strip_prefix('~'), home) {
        (Some(rest), Some(home)) => PathBuf::from(format!("{home}{rest}")),
        _ => PathBuf::from(text),
    };
    match (text.is_empty(), path.is_dir()) {
        (true, _) => Err("Type a folder to save into."),
        (_, false) => Err("That folder does not exist."),
        _ => Ok(path),
    }
}

/// The name field's text as a file name stem: no folders in it, not empty.
pub(crate) fn parse_stem(text: &str) -> Result<String, &'static str> {
    let stem = text.trim().trim_end_matches('.');
    match (stem.is_empty(), stem.contains(['/', '\\', '\0'])) {
        (true, _) => Err("Give the file a name."),
        (_, true) => Err("A file name cannot contain a slash."),
        _ => Ok(stem.to_string()),
    }
}

/// Opens or closes a fold in the folds set, returning whether it is now open.
pub(crate) fn toggle_fold(folds: &mut std::collections::BTreeSet<String>, id: &str) -> bool {
    match folds.remove(id) {
        true => false,
        false => {
            folds.insert(id.to_owned());
            true
        }
    }
}

/// The loudness targets the sheet offers beside "Off": name, LUFS, ceiling.
pub(crate) const LOUDNESS_CHOICES: [(&str, f64, f64); 3] = [
    ("Podcast (\u{2212}16)", -16., -1.),
    ("Streaming (\u{2212}14)", -14., -1.),
    ("Broadcast (\u{2212}23)", -23., -1.),
];

/// What a loudness target is for, in a plain half line; the LUFS figure is the detail, not the name.
pub(crate) fn loudness_plain(lufs: f64) -> &'static str {
    match lufs {
        -14. => "YouTube, Spotify \u{b7} LUFS",
        -16. => "podcasts and voice \u{b7} LUFS",
        _ => "TV and radio \u{b7} LUFS",
    }
}

/// An audio-rate refusal from the engine, said as the sound row's plain caption.
pub(crate) fn audio_rate_words(refusal: &str) -> String {
    match refusal {
        r if r.contains("copied") => "Sound is copied as it is, so its rate can't change",
        r if r.contains("lossless") => "Lossless sound, so its rate can't change",
        r if r.contains("quality-coded") => "Ogg sound is set by quality, not by a rate",
        r if r.contains("no sound") => "There is no sound to write",
        _ => "Sound has no rate to pick here",
    }
    .to_string()
}

/// A failed export in plain words, from the raw error and the folder it was
/// writing to. The raw text stays for the tooltip; the sheet reads this.
pub(crate) fn failure_words(raw: &str, folder: &str) -> String {
    let low = raw.to_ascii_lowercase();
    if low.contains("permission denied") || low.contains("os error 13") {
        format!("edith can't write to {folder}. Pick another folder.")
    } else if low.contains("no space left") || low.contains("os error 28") {
        "The disk is full.".to_string()
    } else if let Some((_, names)) = raw.split_once(" \u{2014} missing: ") {
        format!("A file is missing: {names}.")
    } else if low.contains("no such file") || low.contains("os error 2") {
        format!("The folder {folder} no longer exists.")
    } else {
        "Something went wrong while writing the file.".to_string()
    }
}

/// The folder field's text: the destination's directory, `~` for home.
pub(crate) fn dest_dir_text(path: &Path) -> String {
    let dir = path.parent().unwrap_or(path).to_string_lossy().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && dir.starts_with(&home) => format!("~{}", &dir[home.len()..]),
        _ => dir,
    }
}

/// The time part of the summary. A measured pace prices it ("about 1 min 40 s
/// to export"); with none there is no time at all, rather than a guess or a
/// word that waits on a measurement that may never come.
pub(crate) fn export_time_words(media_seconds: f64, pace: Option<f64>, _pending: bool) -> String {
    // No measured pace, no time: a trial write still running is not a number
    // yet, and "estimating" waiting on it would stick when the trial fails.
    match pace {
        Some(_) => format!("{} to export", time_words(media_seconds, pace)),
        None => String::new(),
    }
}

/// The settings of the sheet's one-second trial write. Always the software
/// seat, whatever the sheet's encoder says: a GPU encode once took this
/// machine down, and a trial nobody asked for must never open one. The range
/// is the first `frames` frames only.
pub(crate) fn pace_bench_settings(
    bps: u64,
    format: Format,
    audio_kbps: u32,
    size: Option<(u32, u32)>,
    depth: Option<u8>,
    frames: u32,
) -> ExportSettings {
    ExportSettings {
        range: Some((0, frames)),
        size,
        video_bit_depth: depth,
        ..export_settings(bps, format, audio_kbps, EncoderSeat::Software)
    }
}

/// Where the trial write goes: the system's temporary folder, never the
/// user's export folder. Deleted as soon as it lands or is cancelled.
pub(crate) fn pace_bench_path(format: Format) -> PathBuf {
    std::env::temp_dir().join(format!("edith-pace-{}.{}", std::process::id(), format.ext()))
}

/// The Details rows that open a list ([`crate::Pick::Export`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExportRow {
    Preset,
    Profile,
    Hdr,
    SoundDepth,
    Container,
    Codec,
    Seat,
    Bitrate,
    Size,
    Depth,
    Kbps,
    Loudness,
    Captions,
    GifWidth,
    GifFps,
    GifLoop,
}

/// One answer from those lists. A pair is picked as the engine's one [`Format`].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ExportPick {
    Preset(&'static str),
    /// "Custom": opens Details, where the settings are.
    Custom,
    Format(Format),
    Size(Option<(u32, u32)>),
    /// `None` types a value in.
    Bitrate(Option<u64>),
    Depth(Option<u8>),
    Profile(ProResProfile),
    Hdr(HdrTransfer),
    HevcSpeed(HevcSpeed),
    /// `Some(16)`; `None` is the 24-bit default.
    SoundBits(Option<u8>),
    /// An index into [`LOUDNESS_CHOICES`]; `None` is off.
    Loudness(Option<usize>),
    /// `true` keeps the timeline's caption tracks in the file.
    Captions(bool),
    /// A GIF's width in pixels.
    GifWidth(u32),
    /// A GIF's frame rate.
    GifFps(u32),
    /// `true` loops the GIF forever.
    GifLoop(bool),
}

/// The widths a GIF is offered at, in pixels.
pub(crate) const GIF_WIDTHS: [u32; 4] = [320, 480, 640, 720];
/// The frame rates a GIF is offered at.
pub(crate) const GIF_FPS: [u32; 4] = [10, 15, 20, 30];

/// The bitrates the Details list offers, in Mb/s.
pub(crate) const BITRATE_STEPS: [u64; 11] = [1, 2, 4, 6, 8, 12, 16, 20, 30, 40, 50];

/// The preset box's text: its name, or "Custom" once a detail moved.
pub(crate) fn preset_box_text(current: Option<&str>, _last: Option<&str>) -> String {
    let label = |id: &str| match id {
        MATCH_PROJECT => Some("Match project"),
        _ => presets::by_id(id).map(|p| p.label),
    };
    current.and_then(label).map_or_else(|| "Custom".to_string(), str::to_string)
}

// ---------------------------------------------------------------------------
// What the Export tab remembers on this computer.
// ---------------------------------------------------------------------------

/// Every format the file can name. Kept beside [`format_key`], whose match is
/// exhaustive, so a new format cannot be written without a name.
const ALL_FORMATS: [Format; 16] = [
    Format::Mp4,
    Format::Av1Mp4,
    Format::Av1,
    Format::Hevc,
    Format::HevcMp4,
    Format::Webm,
    Format::ProRes,
    Format::Wav,
    Format::Flac,
    Format::Mp3,
    Format::Ogg,
    Format::M4a,
    Format::Opus,
    Format::Still,
    Format::PngSequence,
    Format::Gif,
];

/// The name a format is saved under. Stable words, not the labels the page
/// shows -- a label may be reworded, and a saved file must keep reading.
fn format_key(format: Format) -> &'static str {
    match format {
        Format::Mp4 => "mp4",
        Format::Av1Mp4 => "av1-mp4",
        Format::Av1 => "av1",
        Format::Hevc => "hevc",
        Format::HevcMp4 => "hevc-mp4",
        Format::Webm => "webm",
        Format::Wav => "wav",
        Format::Flac => "flac",
        Format::Mp3 => "mp3",
        Format::Ogg => "ogg",
        Format::M4a => "m4a",
        Format::Opus => "opus",
        Format::ProRes => "prores",
        Format::Still => "still",
        Format::PngSequence => "png-sequence",
        Format::Gif => "gif",
    }
}

/// Whether a format writes pictures and no sound, and no encoder runs: a PNG, a run of them, or a
/// GIF. Asked wherever a video format and a sound-only one would be told apart.
pub(crate) fn is_picture_set(format: Format) -> bool {
    matches!(format, Format::Still | Format::PngSequence | Format::Gif)
}

fn prores_key(p: ProResProfile) -> &'static str {
    match p {
        ProResProfile::Proxy => "proxy",
        ProResProfile::Lt => "lt",
        ProResProfile::Standard => "standard",
        ProResProfile::Hq => "hq",
        ProResProfile::P4444 => "4444",
        ProResProfile::P4444Xq => "4444xq",
    }
}

fn hdr_key(h: HdrTransfer) -> &'static str {
    match h {
        HdrTransfer::Sdr => "sdr",
        HdrTransfer::Pq => "pq",
        HdrTransfer::Hlg => "hlg",
    }
}

/// The export choices this computer keeps between launches. The encoder is
/// not among them: it is the project's, saved in its `.edith`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ExportPrefs {
    pub format: Format,
    pub audio_kbps: u32,
    /// The picture's budget; zero is automatic.
    pub bps: u64,
    pub size: Option<(u32, u32)>,
    pub depth: Option<u8>,
    pub prores: ProResProfile,
    pub hdr: HdrTransfer,
    pub hevc_speed: HevcSpeed,
    pub sound_bits: Option<u8>,
}

impl Default for ExportPrefs {
    fn default() -> Self {
        ExportPrefs {
            format: Format::default(),
            audio_kbps: DEFAULT_AUDIO_KBPS,
            bps: 0,
            size: None,
            depth: None,
            prores: ProResProfile::default(),
            hdr: HdrTransfer::Sdr,
            hevc_speed: HevcSpeed::Default,
            sound_bits: None,
        }
    }
}

/// Beside the keybindings, like the theme and the proxy switches.
fn export_defaults_path() -> PathBuf {
    crate::keymap::Keymap::config_path().with_file_name("export-defaults")
}

/// What the last session left, or the defaults: missing or garbled is not
/// worth a message, since a preference file is not the user's work.
pub(crate) fn load_export_prefs() -> ExportPrefs {
    match std::fs::read_to_string(export_defaults_path()) {
        Ok(text) => parse_export_prefs(&text),
        Err(_) => ExportPrefs::default(),
    }
}

/// Line one is the format's name, line two the audio kbps (the file's first
/// shape, still read), then `key value` lines. Each line is read on its own:
/// one that does not parse leaves that value at its default.
pub(crate) fn parse_export_prefs(text: &str) -> ExportPrefs {
    let mut prefs = ExportPrefs::default();
    let mut lines = text.lines();
    if let Some(f) = lines
        .next()
        .and_then(|line| ALL_FORMATS.into_iter().find(|&f| format_key(f) == line.trim()))
    {
        prefs.format = f;
    }
    if let Some(k) = lines
        .next()
        .and_then(|line| line.trim().parse::<u32>().ok())
        .filter(|k| AUDIO_KBPS.contains(k))
    {
        prefs.audio_kbps = k;
    }
    for line in lines {
        let Some((key, value)) = line.trim().split_once(' ') else { continue };
        let value = value.trim();
        match key {
            "bitrate" => {
                if let Ok(b) = value.parse::<u64>()
                    && (b == 0 || (BPS_MIN..=BPS_MAX).contains(&b))
                {
                    prefs.bps = b;
                }
            }
            "size" => {
                if let Some((w, h)) = value.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
                    && (16..=16_384).contains(&w)
                    && (16..=16_384).contains(&h)
                {
                    prefs.size = Some((w, h));
                }
            }
            "depth" => {
                prefs.depth = match value {
                    "8" => Some(8),
                    "10" => Some(10),
                    _ => None,
                }
            }
            "prores" => {
                if let Some(p) = ProResProfile::ALL.into_iter().find(|&p| prores_key(p) == value) {
                    prefs.prores = p;
                }
            }
            "hdr" => {
                if let Some(h) = [HdrTransfer::Sdr, HdrTransfer::Pq, HdrTransfer::Hlg]
                    .into_iter()
                    .find(|&h| hdr_key(h) == value)
                {
                    prefs.hdr = h;
                }
            }
            "hevc-speed" => {
                if let Some(s) = HEVC_SPEEDS.into_iter().find(|&s| hevc_speed_key(s) == value) {
                    prefs.hevc_speed = s;
                }
            }
            "sound-bits" => {
                prefs.sound_bits = match value {
                    "16" => Some(16),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    // An HDR tag lives on an HEVC file only; the engine refuses it elsewhere.
    if !prefs.format.is_hevc() {
        prefs.hdr = HdrTransfer::Sdr;
    }
    prefs
}

pub(crate) fn render_export_prefs(p: &ExportPrefs) -> String {
    let mut out = format!("{}\n{}\n", format_key(p.format), p.audio_kbps);
    out.push_str(&format!("bitrate {}\n", p.bps));
    if let Some((w, h)) = p.size {
        out.push_str(&format!("size {w}x{h}\n"));
    }
    if let Some(d) = p.depth {
        out.push_str(&format!("depth {d}\n"));
    }
    out.push_str(&format!("prores {}\nhdr {}\n", prores_key(p.prores), hdr_key(p.hdr)));
    if p.hevc_speed != HevcSpeed::Default {
        out.push_str(&format!("hevc-speed {}\n", hevc_speed_key(p.hevc_speed)));
    }
    if let Some(b) = p.sound_bits {
        out.push_str(&format!("sound-bits {b}\n"));
    }
    out
}

/// Writes the choices whole, the way the proxy switches' files are.
pub(crate) fn save_export_prefs(prefs: &ExportPrefs) -> std::io::Result<()> {
    let path = export_defaults_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, render_export_prefs(prefs))
}

#[cfg(test)]
mod prefs_tests {
    use super::*;

    #[test]
    fn every_format_is_saved_under_a_name_that_reads_back_as_itself() {
        let mut names = Vec::new();
        for f in ALL_FORMATS {
            let key = format_key(f);
            assert!(!names.contains(&key), "{key} names two formats");
            names.push(key);
            let p = ExportPrefs { format: f, ..ExportPrefs::default() };
            assert_eq!(parse_export_prefs(&render_export_prefs(&p)).format, f);
        }
    }

    #[test]
    fn all_the_choices_round_trip() {
        let p = ExportPrefs {
            format: Format::HevcMp4,
            audio_kbps: 256,
            bps: 8_000_000,
            size: Some((1280, 720)),
            depth: Some(10),
            prores: ProResProfile::P4444,
            hdr: HdrTransfer::Pq,
            hevc_speed: HevcSpeed::Fast,
            sound_bits: Some(16),
        };
        assert_eq!(parse_export_prefs(&render_export_prefs(&p)), p);
    }

    #[test]
    fn old_and_garbled_files_read_as_defaults_line_by_line() {
        // The first shape: two lines only.
        let old = parse_export_prefs("av1-mp4\n128\n");
        assert_eq!((old.format, old.audio_kbps), (Format::Av1Mp4, 128));
        assert_eq!(old.bps, 0);
        assert_eq!(parse_export_prefs(""), ExportPrefs::default());
        // A rate the encoder does not offer is not a rate: the default stands.
        assert_eq!(parse_export_prefs("wav\n999\n").audio_kbps, DEFAULT_AUDIO_KBPS);
        assert_eq!(parse_export_prefs("wav\n999\n").format, Format::Wav);
        assert_eq!(parse_export_prefs("no-such\n999\nbitrate x\nsize 1\nhdr what\n"), ExportPrefs::default());
        // HDR on a non-HEVC file is dropped.
        assert_eq!(parse_export_prefs("mp4\n192\nhdr pq\n").hdr, HdrTransfer::Sdr);
    }
}
