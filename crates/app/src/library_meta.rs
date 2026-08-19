//! What a library row is and what it says about its file.

use crate::*;

/// What is left on the clipboard after the library row at `removed` was taken
/// out. A copied clip names its file by *index* into the source list
/// (`engine::Clip::source`) and a removal renumbers that list
/// (`engine::Project::remove_source`), so a clipboard kept as it was would paste
/// **a different file** -- the next one along -- over the range it was copied
/// from.
///
/// The clip's own file gone means there is nothing to paste: `None`, and the
/// next paste says the clipboard is empty rather than putting some other take
/// down. Every index past it moves down by one, exactly as the lanes' clips do.
pub(crate) fn clipboard_after_remove(clip: Option<Clip>, removed: usize) -> Option<Clip> {
    let mut clip = clip?;
    match clip.source.cmp(&removed) {
        std::cmp::Ordering::Equal => None,
        std::cmp::Ordering::Greater => {
            clip.source -= 1;
            Some(clip)
        }
        std::cmp::Ordering::Less => Some(clip),
    }
}

/// One library row: a file and one of its audio streams. Plain data, planned
/// before anything is drawn -- which rows exist at all is the branchy part.
#[derive(Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) path: PathBuf,
    pub(crate) stream: usize,
    /// The file, plus which stream this is when the file has several.
    pub(crate) name: String,
    /// What the stream is, or blank for a file with a single one.
    pub(crate) detail: String,
    /// Why it cannot be put on this timeline, for a row that cannot: shown in
    /// place of the length and the only thing that greys a row out.
    pub(crate) unusable: Option<String>,
    /// Frames of the *file*, for the length line.
    pub(crate) frames: u32,
    /// Index into `SOURCE_TINTS`, shared by every stream of one file: the
    /// swatch says which file, and the lanes tint their clips the same way.
    pub(crate) tint: usize,
}

/// Every row the library shows: one per source entry, plus one for each further
/// audio stream those files have that no clip plays yet -- a remux with a track
/// per language lists them all, the ones this engine cannot use greyed out
/// rather than hidden. Streams a file has not been probed for yet simply are
/// not there; the row for what *is* on the timeline is always there.
///
/// `timeline_audio` is the rate and layout of source 0's stream, which every
/// other source must match: one output device and one copied AAC track for the
/// whole timeline (`PlaybackSession::place_stream_at`). `None` while unknown,
/// and then nothing is greyed for it -- the engine still refuses.
pub(crate) fn library_rows(
    sources: &[Source],
    streams: &HashMap<PathBuf, Vec<StreamInfo>>,
    decoders: &HashMap<PathBuf, Option<(Option<Codec>, Backend)>>,
    timeline_audio: Option<(u32, u16)>,
    frames: impl Fn(&Path) -> u32,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (i, source) in sources.iter().enumerate() {
        let of_file = streams.get(&source.path).map_or(&[][..], Vec::as_slice);
        let info = of_file.iter().find(|s| s.index == source.audio_stream);
        let tint = sources
            .iter()
            .position(|s| s.path == source.path)
            .expect("a source finds itself");
        rows.push(Row {
            path: source.path.clone(),
            stream: source.audio_stream,
            name: row_name(&source.path, source.audio_stream, of_file.len() > 1),
            // The decoder first: it is the same for every row of a file and
            // it is what a person opening the panel is asking about. The
            // stream half only where there is a choice to describe -- a file
            // with one audio track is the row it has always been, name and
            // length, and the length is what would be squeezed out at the
            // panel's least width.
            detail: join_detail(
                &decoders
                    .get(&source.path)
                    .copied()
                    .flatten()
                    .map_or_else(String::new, |(codec, backend)| decode_label(codec, backend)),
                &info
                    .filter(|_| of_file.len() > 1)
                    .map_or_else(String::new, stream_detail),
            ),
            // A stream already on the timeline is playing: whatever a probe
            // would say about it now, it is usable by demonstration.
            unusable: None,
            frames: frames(&source.path),
            tint,
        });
        // The file's other streams, listed once, right after the last entry
        // that names the file -- so a file's rows sit together.
        if sources[i + 1..].iter().any(|s| s.path == source.path) {
            continue;
        }
        for info in of_file {
            if sources
                .iter()
                .any(|s| s.path == source.path && s.audio_stream == info.index)
            {
                continue; // it has a row of its own above
            }
            rows.push(Row {
                path: source.path.clone(),
                stream: info.index,
                name: row_name(&source.path, info.index, true),
                detail: stream_detail(info),
                unusable: unusable(info, timeline_audio),
                frames: frames(&source.path),
                tint,
            });
        }
    }
    rows
}

/// The row's second line: what the stream is and then either how long it is or
/// why it cannot be used, with the separator only where both halves exist (a
/// single-stream file says nothing about its stream).
pub(crate) fn join_detail(detail: &str, tail: &str) -> String {
    match (detail.is_empty(), tail.is_empty()) {
        (true, _) => tail.to_string(),
        (false, true) => detail.to_string(),
        (false, false) => format!("{detail} · {tail}"),
    }
}

/// How a source's decoder reads: the codec and which seat has it, or the seat
/// alone for a still, which has no coded stream to name. The one place either
/// answer is spelled, so a row, a transport line and a card cannot disagree.
pub(crate) fn decode_label(codec: Option<Codec>, backend: Backend) -> String {
    match codec {
        Some(codec) => format!("{} · {}", codec.name(), backend.label()),
        None => backend.label().to_string(),
    }
}

/// How a row names its file: the file alone when it has one audio stream or
/// none, and the file plus which stream this row is when it has several --
/// counted from 1, the way a player numbers tracks.
pub(crate) fn row_name(path: &Path, stream: usize, several: bool) -> String {
    match several {
        false => file_name(path),
        true => format!("{} [audio {}]", file_name(path), stream + 1),
    }
}

/// What a stream is, for the row's second line: the language if the file says
/// one, then rate and layout. A field the header does not give is left out
/// rather than shown as a zero -- a stream we cannot parse says nothing about
/// itself, and saying "0 Hz" would be saying something.
pub(crate) fn stream_detail(info: &StreamInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(lang) = &info.lang {
        parts.push(lang_human(lang).to_string());
    }
    if info.sample_rate > 0 {
        parts.push(format!("{} kHz", f64::from(info.sample_rate) / 1000.));
    }
    parts.extend(layout(info.channels));
    parts.join(" ")
}

/// What a file is coded at, for the properties cards: the rate of the whole
/// file, then each track's own beside it. A component the container does not
/// state is left out for `stream_detail`'s reason -- a fabricated `0` would be
/// saying something -- and a file that states none of the three says so rather
/// than showing an empty row. `None` is the probe still running, which is a
/// real wait: it walks a Matroska's clusters.
///
/// The whole file's rate is the one that carries the unit and no word: named
/// "total" as well, the line loses its last component to `MENU_W`'s truncation
/// on an ordinary 1080p file -- and a rate cut to "0.13 soun" is a number the
/// card did not give.
///
/// `tracks` is how many sound tracks the file has, from `probe_streams`. The
/// rate is the one track this engine plays -- the first, neither their sum nor
/// the biggest -- so a file with more says which of how many: a bare "0.16
/// sound" on a file whose name says AC3.5.1 names a track without saying it is
/// one of two, and the card's Audio row above it may be describing the other.
///
/// The marker costs the word "sound", the way the whole file's rate costs the
/// word "total" above, and for the same reason. Measured in Noto Sans 11 px
/// against the 186 px this value has beside a "Bitrate" label: "0.16 sound 1/2"
/// wants 192 px on a 4.7 Mb/s film and 205 on the 39.8 Mb/s remux --
/// the marker is what gets cut, which is the one part of the line that is new.
/// Nothing that keeps the word fits the wide files -- the shortest, "snd 1/2",
/// still wants 188 px on that remux -- so the word goes and the number keeps
/// the answer: "0.16 1 of 2" wants 172 px, and 185 on the widest line his
/// library can produce (the 10.9 Mb/s three-track film).
pub(crate) fn bitrate_detail(rate: Option<MediaBitrate>, tracks: usize) -> String {
    let Some(rate) = rate else {
        return "…".to_string();
    };
    let (per, unit) = rate_scale(rate);
    let mut parts: Vec<String> = Vec::new();
    if let Some(total) = rate.total {
        // One unit for the three numbers, carried once, on the whole file's
        // rate: the components are read against it.
        parts.push(format!("{} {unit}", scaled(total, per)));
    }
    if let Some(video) = rate.video {
        parts.push(format!("{} video", scaled(video, per)));
    }
    if let Some(audio) = rate.audio {
        parts.push(match tracks > 1 {
            true => format!("{} 1 of {tracks}", scaled(audio, per)),
            false => format!("{} sound", scaled(audio, per)),
        });
    }
    match parts.is_empty() {
        true => "not stated".to_string(),
        false => parts.join(" · "),
    }
}

/// Below this a megabit's two decimals cannot state a rate, so the line is read
/// in kilobits instead.
pub(crate) const MB_FLOOR: u64 = 10_000;

/// What the line counts in, and the name of it: the largest unit that can state
/// its *smallest* component, because that is the one a bigger unit rounds away.
/// Megabits for everything a real file produces -- the smallest component over
/// an 18-file sweep of his library was 0.13 Mb/s -- kilobits for the sub-32x32
/// encodes below that.
///
/// corner-cut: one unit for the whole line, so a file mixing a multi-megabit
/// picture with a sub-10 kb/s sound track prints the picture as four or five
/// digits of kilobits and loses the line's tail to `MENU_W`. No such file
/// exists in his library, and both units at once ("0.01 Mb/s · 1.2 kb/s video ·
/// 9.5 kb/s sound") wants 215 px of the 186 the row has. Upgrade path is a
/// suffix per component ("1.2k video"), which measures 177 px.
pub(crate) fn rate_scale(rate: MediaBitrate) -> (f64, &'static str) {
    match [rate.total, rate.video, rate.audio]
        .into_iter()
        .flatten()
        .min()
    {
        // A rate this small is a broken header rather than a track, but it is
        // still a number the file stated, and bits state it.
        Some(bits) if bits < 10 => (1., "b/s"),
        Some(bits) if bits < MB_FLOOR => (1_000., "kb/s"),
        _ => (1_000_000., "Mb/s"),
    }
}

/// A rate in the line's unit as a person reads it: one decimal above 1 of it,
/// two below -- a 128 kbps song rounded to one decimal is "0.1", which reads as
/// a guess where "0.13" reads as a measurement.
///
/// Never `0.00`: [`rate_scale`] picks the unit off the smallest component, so
/// two decimals always reach it. Every rate that gets here is one the container
/// really stated (the probe leaves out what it does not state, it never zeroes
/// it), and a "0.00 sound" would be this card saying a track that plays is
/// silent.
pub(crate) fn scaled(bits: u64, per: f64) -> String {
    let n = bits as f64 / per;
    match n >= 1. {
        true => format!("{n:.1}"),
        false => format!("{n:.2}"),
    }
}

/// A language tag as a person reads it. Everything a file writes is passed
/// through untouched bar the one tag that is not a language: "und" is what a
/// muxer writes when nobody said, and a row showing it verbatim names a
/// language nobody speaks.
pub(crate) fn lang_human(lang: &str) -> &str {
    match lang {
        "und" => "unknown language",
        lang => lang,
    }
}

pub(crate) fn layout(channels: u16) -> Option<String> {
    match channels {
        0 => None,
        1 => Some("mono".to_string()),
        2 => Some("stereo".to_string()),
        n => Some(format!("{n} ch")),
    }
}

/// Why a stream cannot go on this timeline, or `None` if it can. Both answers
/// are shown: a stream nothing can be done with is listed greyed with the
/// reason, never dropped from the list -- a file has the tracks it has, and a
/// picker that hides them is a picker that lies.
pub(crate) fn unusable(info: &StreamInfo, timeline_audio: Option<(u32, u16)>) -> Option<String> {
    if !info.decodable {
        // AAC and AC-3 name themselves (`StreamInfo::codec`, which reads the
        // stsd fourcc by hand); anything else mp4 0.14 does not parse has no
        // name to give, and a row with no name still says why it is greyed.
        return Some(match info.codec.as_str() {
            "unknown" => "unsupported codec".to_string(),
            codec => format!("{codec} is not supported"),
        });
    }
    // The **layout** and not the rate, which is what the engine's own gate now
    // asks (`PlaybackSession::import`): a stream written at another sample rate
    // is resampled onto the timeline's at the decoder's door, so greying its row
    // would be this picker refusing what the timeline accepts.
    let (_, channels) = timeline_audio?;
    (info.channels != channels).then(|| {
        format!(
            "the timeline is {}",
            layout(channels).unwrap_or_else(|| "silent".to_string())
        )
    })
}

/// How wide the equalizer card is drawn in a window this wide: all of it bar a
/// margin, up to [`EQ_W_MAX`]. The card is a graph, and a graph of twenty
/// thousand hertz on 320 px spends four pixels on the octave below middle C --
/// so it takes the room a big window has and stays inside a small one. Floored
/// at the other cards' width, which is the last size the rows still read at.
pub(crate) fn eq_card_w(window_w: f32) -> f32 {
    (window_w - EQ_W_MARGIN).clamp(KEYS_W, EQ_W_MAX)
}

/// What the media list is given of the window. A share of it, so a narrow
/// window gives the panel less rather than giving the picture nothing, floored
/// where a name stops being readable and capped at a third of the window --
/// the picture is what the program is for and keeps the majority at every size.
pub(crate) fn library_w(window_w: f32) -> f32 {
    (window_w * LIBRARY_FRAC)
        .clamp(LIBRARY_MIN_W, LIBRARY_MAX_W)
        .min(window_w / 3.)
}

/// What is left of a library column this wide for a row's *words*: the panel's
/// padding on both sides, the tint bar, the gap after it and the row's own right
/// inset. Every row in the column -- media and subtitle -- is built to this
/// shape, so one number answers for both.
pub(crate) fn row_text_w(width: f32) -> f32 {
    // 8 px of panel padding each side, then the bar, the gap after it and the
    // row's own right inset -- the numbers the rows are built with.
    width - 16. - SWATCH_W - 6. - 6.
}

/// A name cut to what a column this wide can hold, out of the *middle*. Two
/// files off one release differ in their last characters and nowhere else --
/// "…Episode 01" against "…02" -- so a name cut from the right is the same
/// name twice and the list stops naming anything. The width decides how much
/// survives, not a number of characters somebody guessed: a wider window spells
/// more of the file out, and the floor still keeps both ends.
///
/// The element truncates for real; this only decides where what is lost comes
/// out of.
pub(crate) fn clip_middle(name: &str, width: f32) -> String {
    let budget = ((width / LIST_CHAR_W) as usize).max(LIST_CLIP_MIN);
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= budget {
        return name.to_string();
    }
    // The gap costs a character, and what is left of the odd one goes to the
    // tail: the tail is the half that tells two of them apart.
    let head = (budget - 1) / 2;
    let tail = budget - 1 - head;
    format!(
        "{}…{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

/// What the window is called: the program, and what is open in it. The name is
/// what the header shows, so an empty window says the program alone rather than
/// "no file open — edith".
pub(crate) fn window_title(name: &str) -> String {
    if name == NO_FILE {
        "edith".to_string()
    } else {
        format!("{name} — edith")
    }
}

/// The tint a clip from source `n` wears. Cycled rather than extended: past the
/// palette two sources share a colour, which is a smaller lie than a fifth tint
/// bright enough to leave the family.
pub(crate) fn source_tint(source: usize) -> u32 {
    SOURCE_TINTS()[source % SOURCE_TINTS().len()]
}

/// The tint of a *file*, which is what a library row is named by: the first
/// source entry naming it, since two audio streams of one file are two sources
/// and one colour.
///
/// `None` for a path no source names -- a standalone `.srt` is on nobody's
/// timeline, and painting it with the first file's colour would say it came out
/// of that file. No swatch says what is true: it belongs to itself.
pub(crate) fn file_tint(sources: &[Source], path: &Path) -> Option<u32> {
    sources
        .iter()
        .position(|s| s.path == path)
        .or_else(|| {
            // A source entry is stored symlink-resolved (`Source::new`) and a
            // path from anywhere else -- a subtitle track, a file being
            // dragged -- is stored as it was spelled. `edith assets/film.mkv`
            // is one file under two spellings, and matching by spelling alone
            // said the film had no colour. Only asked when the spellings
            // differ, so the common paint costs no syscall.
            let path = std::fs::canonicalize(path).ok()?;
            sources.iter().position(|s| s.path == path)
        })
        .map(source_tint)
}
