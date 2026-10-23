//! What a library row is and what it says about its file.

use crate::*;

/// What is left on the clipboard after the library row at `removed` was taken
/// out. A copied clip names its file by *index* into the source list
/// (`engine::Clip::source`) and a removal renumbers that list
/// (`engine::Project::remove_source`), so a clipboard kept as it was would paste
/// **a different file** -- the next one along -- over the range it was copied
/// from.
///
/// An entry whose own file is gone drops out of the clipboard rather than
/// pasting some other take down in its place; a set with even one such
/// member is trimmed, not emptied whole, the way a lane keeps every clip a
/// removal did not touch. Every index past `removed` moves down by one,
/// exactly as the lanes' clips do.
pub(crate) fn clipboard_after_remove(
    clipboard: Vec<(Lane, Clip)>,
    removed: usize,
) -> Vec<(Lane, Clip)> {
    clipboard
        .into_iter()
        .filter_map(|(lane, mut clip)| match clip.source.cmp(&removed) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => {
                clip.source -= 1;
                Some((lane, clip))
            }
            std::cmp::Ordering::Less => Some((lane, clip)),
        })
        .collect()
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
    decoders: &HashMap<PathBuf, Decoder>,
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
                    .map_or_else(|| Decoder::Pending.detail(), Decoder::detail),
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
    disambiguate(&mut rows);
    rows
}

/// Two files whose rows read the same word ("2026-09-09 18-05-50" twice, user
/// 2026-09-10 -- two screen recordings) are told apart on the metadata line
/// rather than left as a pair of identical rows: the folder they came out of
/// when that differs, and a `#n` in arrival order when even the folder is
/// shared. Compared on the stem, because the stem is what the row draws.
fn disambiguate(rows: &mut [Row]) {
    let named: Vec<(String, PathBuf)> = rows
        .iter()
        .map(|row| (crate::files::stem(&row.path), row.path.clone()))
        .collect();
    for i in 0..rows.len() {
        let mut twins: Vec<&PathBuf> = Vec::new();
        for (name, path) in &named {
            if *name == named[i].0 && !twins.contains(&path) {
                twins.push(path);
            }
        }
        if twins.len() < 2 {
            continue;
        }
        let folder = rows[i]
            .path
            .parent()
            .and_then(Path::file_name)
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        let alone_in_it = twins
            .iter()
            .filter(|p| p.parent() == rows[i].path.parent())
            .count()
            == 1;
        let mark = match alone_in_it && !folder.is_empty() {
            true => folder,
            false => format!(
                "#{}",
                twins
                    .iter()
                    .position(|p| **p == rows[i].path)
                    .expect("a row finds itself")
                    + 1
            ),
        };
        rows[i].detail = join_detail(&rows[i].detail, &mark);
    }
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

/// What the one-shot probe said about a file: the seat it will open on, or --
/// when the decoder itself refused -- the refusal, in the decoder's own words.
///
/// Three answers, not two, because "asked" and "refused" are not the same
/// answer. Both used to be a missing entry, and a missing entry drew an empty
/// second line -- the same line a song draws -- so a file this machine cannot
/// decode was indistinguishable from one that needs no word at all.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decoder {
    /// Asked and not yet answered: the probe is still walking the file. A
    /// real wait, and one that claims nothing while it lasts.
    Pending,
    /// The codec (`None` for a still, which has no coded stream) and the seat
    /// the engine picked for it.
    Seat(Option<Codec>, Backend),
    /// The decoder refused this file, and these are its words. Not a message
    /// of this program's own making: the probe is `engine::decode::probe`,
    /// and the refusal is the one opening the file would give.
    Refused(String),
}

impl From<engine::Result<(Option<Codec>, Backend)>> for Decoder {
    fn from(probed: engine::Result<(Option<Codec>, Backend)>) -> Self {
        match probed {
            Ok((codec, backend)) => Self::Seat(codec, backend),
            Err(why) => Self::Refused(why.to_string()),
        }
    }
}

impl Decoder {
    /// What the row's second line says about the file's decoder: the seat for
    /// a file that opens, the refusal for one that does not, and nothing for a
    /// probe still running.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Pending => String::new(),
            Self::Seat(codec, backend) => decode_label(*codec, *backend),
            Self::Refused(why) => why.clone(),
        }
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

/// How big a file is, for a person: the largest unit that keeps the figure to
/// one decimal (`1.2 GB`, `840 MB`), bytes below a kilobyte.
pub(crate) fn size_human(bytes: u64) -> String {
    for (unit, per) in [("GB", 1e9), ("MB", 1e6), ("kB", 1e3)] {
        if bytes as f64 >= per {
            let n = bytes as f64 / per;
            return match n >= 10. {
                true => format!("{n:.0} {unit}"),
                false => format!("{n:.1} {unit}"),
            };
        }
    }
    format!("{bytes} B")
}

/// A picture's height as a person names it: `1080p`, and `4K` for the 2160
/// line, which is the one name people use for it.
pub(crate) fn resolution_label(height: u32) -> String {
    match height {
        2160 => "4K".to_string(),
        h => format!("{h}p"),
    }
}

/// One stream's rate in the unit its size asks for: bits a second below a
/// kilobit, kilobits below a megabit, megabits above. One track's own rate, so
/// there is no shared unit to keep across a line of them.
pub(crate) fn bits_label(bits: u64) -> String {
    match bits {
        0..1_000 => format!("{bits} b/s"),
        1_000..1_000_000 => format!("{} kb/s", scaled(bits, 1_000.)),
        _ => format!("{} Mb/s", scaled(bits, 1_000_000.)),
    }
}

/// A picture track as the properties plate says it: codec, size, rate, depth
/// and its own bitrate, each left out where the file does not state it.
pub(crate) fn video_detail(video: &engine::VideoFacts, bitrate: Option<u64>) -> String {
    let mut parts = vec![video.codec.name().to_string()];
    if video.width > 0 && video.height > 0 {
        parts.push(format!("{}×{}", video.width, video.height));
    }
    if let Some(fps) = video.frame_rate {
        parts.push(format!("{} fps", crate::export_ui::fps_label(fps)));
    }
    if let Some(depth) = video.bit_depth {
        parts.push(format!("{depth}-bit"));
    }
    if let Some(bits) = bitrate {
        parts.push(bits_label(bits));
    }
    parts.join(" · ")
}

/// A sound track as the properties plate says it: codec, rate, layout, depth
/// and its own bitrate. The codec is `"unknown"` only where the header will not
/// name it, which says nothing and is left out.
pub(crate) fn audio_detail(info: &StreamInfo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !info.codec.is_empty() && info.codec != "unknown" {
        parts.push(info.codec.clone());
    }
    if info.sample_rate > 0 {
        parts.push(format!("{} kHz", f64::from(info.sample_rate) / 1000.));
    }
    parts.extend(layout(info.channels));
    if let Some(depth) = info.bit_depth {
        parts.push(format!("{depth}-bit"));
    }
    if let Some(bits) = info.bitrate {
        parts.push(bits_label(bits));
    }
    parts.join(" · ")
}

/// What the properties plate says about a file, top to bottom: the file and its
/// path, its container and size, then one line per picture and sound track.
/// Rows the probe has not answered for are simply not there -- the plate fills
/// in as the header reads, and never says "unknown".
pub(crate) fn file_properties(
    path: &Path,
    facts: Option<&engine::MediaFacts>,
    streams: &[StreamInfo],
    rate: Option<MediaBitrate>,
) -> Vec<(String, String)> {
    let mut out = vec![
        ("File".to_string(), file_name(path)),
        ("Path".to_string(), path.display().to_string()),
    ];
    if let Some(container) = facts.and_then(|f| f.container.clone()) {
        out.push(("Container".to_string(), container));
    }
    if let Some(bytes) = facts.and_then(|f| f.bytes) {
        out.push((
            "Size".to_string(),
            match rate.and_then(|r| r.total) {
                Some(total) => format!("{} · {}", size_human(bytes), bits_label(total)),
                None => size_human(bytes),
            },
        ));
    }
    if let Some(video) = facts.and_then(|f| f.video.as_ref()) {
        out.push((
            "Video".to_string(),
            video_detail(video, rate.and_then(|r| r.video)),
        ));
    }
    for info in streams {
        let label = match streams.len() > 1 {
            true => format!("Audio {} of {}", info.index + 1, streams.len()),
            false => "Audio".to_string(),
        };
        out.push((label, audio_detail(info)));
    }
    out
}

/// The plate's Copy button: the full path on the clipboard, and a toast that
/// says so. Shared by both properties plates so the two cannot word it apart.
pub(crate) fn copy_path_button(path: &Path, cx: &mut Context<Player>) -> AnyElement {
    let text = path.display().to_string();
    control(
        "properties-copy-path",
        0.,
        BG_RAISED(),
        None,
        "Copy",
        "copies the full path of this file".to_string(),
        true,
        cx.listener(move |this, _: &ClickEvent, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
            this.notify_user("Path copied".into());
            cx.notify();
        }),
    )
    .into_any_element()
}

impl Player {
    /// Read each source's header once for the properties plates: container,
    /// size, picture and track. Off the render thread like the bitrate probe
    /// beside it, and filled the same way -- presence means "asked", and the
    /// inner `None` is "asked, not answered yet".
    pub(crate) fn cache_facts(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        for path in unseen_paths(session.sources(), &self.facts) {
            self.facts.insert(path.clone(), None);
            let probed = cx.background_executor().spawn({
                let path = path.clone();
                async move { engine::probe_media(&path) }
            });
            cx.spawn(async move |this, cx| {
                let probed = probed.await;
                this.update(cx, |this, cx| {
                    this.facts.insert(path, Some(probed));
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }
}

/// A rate in the line's unit as a person reads it: one decimal above 1 of it,
/// two below -- a 128 kbps song rounded to one decimal is "0.1", which reads as
/// a guess where "0.13" reads as a measurement.
///
/// Never `0.00`: [`bits_label`] only asks for this in a unit where the number
/// is at least 0.001 of it, so two decimals always reach it. Every rate that
/// gets here is one the container really stated (the probe leaves out what it
/// does not state, it never zeroes it), and a "0.00 sound" would be this card
/// saying a track that plays is silent.
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
pub(crate) fn eq_card_w(window_w: f32, maximized: bool) -> f32 {
    let raw = window_w - EQ_W_MARGIN;
    if maximized {
        // Maximized drops the [`EQ_W_MAX`] ceiling: every pixel across is
        // frequency resolution (the doc comment above), and a maximized card
        // is asking for exactly the room a big window has to give it.
        raw.max(KEYS_W)
    } else {
        raw.clamp(KEYS_W, EQ_W_MAX)
    }
}

/// How wide a non-EQ param card is drawn: its usual fixed width, or (when
/// maximized) all of the window bar the same margin the EQ card gives
/// itself -- never narrower than its own floor, so a small window's
/// maximize is a no-op rather than a shrink.
pub(crate) fn card_max_w(base_w: f32, maximized: bool, window_w: f32) -> f32 {
    if maximized {
        (window_w - EQ_W_MARGIN).max(base_w)
    } else {
        base_w
    }
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

/// What the window is called once a project is in play: the same name the
/// header bar shows (`ui::menubar::bar`), so the title and the bar never
/// disagree. A project started or saved answers with its own name; an unsaved
/// one is "Untitled"; with no project at all the window keeps `name`, which is
/// the media's.
pub(crate) fn shown_title(name: &str, project: &Path, new_project: Option<&Path>, armed: bool) -> String {
    let project_name = match (project.as_os_str().is_empty(), new_project) {
        (_, Some(path)) => Some(crate::files::stem(path)),
        (true, None) => None,
        (false, None) if armed => Some(crate::files::stem(project)),
        (false, None) => Some("Untitled".to_string()),
    };
    window_title(&project_name.unwrap_or_else(|| name.to_string()))
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
