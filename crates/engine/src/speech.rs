//! Speech for the editor: what is said on the timeline (edith-stt, Whisper), captions cut from
//! it, and the fillers ("um", "uh") found in it.
//!
//! The pieces here are shared by the MCP server's `transcript`, `find_words` and
//! `auto_captions` and by the GUI's Captions and Fillers cards: locating the `edith-stt` binary
//! and its models, running it over a WAV, reading its JSON lines, and cutting timed words into
//! caption cues.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::project::Lane;
use crate::{AudioSession, PlaybackSession, subtitle};

/// The sample rate edith-stt is fed.
pub const RATE: u32 = 16_000;
/// The Whisper model sizes edith-stt knows.
pub const MODELS: &[&str] = &["tiny", "base", "small"];
/// Silence longer than this (seconds) between two words ends a caption cue.
pub const CAPTION_GAP_SECS: f64 = 0.7;
/// Whisper drops disfluencies unless its prompt shows them: this prompt, put before the
/// user's `hint`, makes it keep "um", "uh" and friends.
pub const FILLER_PROMPT: &str = "Um, uh, so, like, you know, I mean, er, hmm...";
/// Disfluencies that captions leave out. "like" and "you know" are real words too often to strip.
pub const CAPTION_FILLERS: &[&str] = &["um", "uhm", "uh", "ah", "hum", "hmm", "er", "erm"];
/// A caption cue shorter than this (seconds), or of one word, is folded into a neighbour when the
/// merged text still fits, and otherwise held on screen this long (or up to the next cue).
pub const CUE_MIN_SECS: f64 = 0.8;
/// A trailing caption cue whose mean word probability is below this is dropped.
pub const CUE_LOW_P: f64 = 0.5;
/// Whisper's stock sign-offs, which it makes up over silence. A trailing one that
/// follows another sign-off is dropped.
const SIGN_OFFS: &[&str] = &["thank you", "thanks", "thanks for watching", "thank you for watching", "bye", "bye bye", "see you", "see you next time"];

/// The word as compared: edge punctuation off, lower case.
pub fn norm(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from)
}

/// The edith-stt binary: `EDITH_STT_BIN`, else the first
/// `tools/edith-stt/target/release/edith-stt` above the running executable.
pub fn stt_bin() -> Result<PathBuf, String> {
    const NOT_BUILT: &str = "edith-stt is not built. Run `cd tools/edith-stt && cargo build --release` in the edith repo, or set EDITH_STT_BIN to the binary";
    if let Some(p) = std::env::var_os("EDITH_STT_BIN") {
        let p = PathBuf::from(p);
        return if p.is_file() { Ok(p) } else { Err(format!("EDITH_STT_BIN is set to {}, which is not a file. {NOT_BUILT}", p.display())) };
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors() {
            let c = dir.join("tools/edith-stt/target/release/edith-stt");
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(NOT_BUILT.into())
}

pub fn check_model(model: &str) -> Result<(), String> {
    if !MODELS.contains(&model) {
        return Err(format!("model `{model}` is not one of {}", MODELS.join(", ")));
    }
    let dir = home().join(".cache/edith/models").join(format!("whisper-{model}"));
    if !dir.is_dir() {
        return Err(format!("the {model} model is not installed at {}; edith-stt downloads it only when asked to", dir.display()));
    }
    Ok(())
}

pub fn check_lang(lang: &str) -> Result<(), String> {
    let ok = lang == "auto" || ((2..=3).contains(&lang.len()) && lang.chars().all(|c| c.is_ascii_lowercase()));
    if ok { Ok(()) } else { Err(format!("language `{lang}` must be auto or a language code such as en")) }
}

/// The model when none is named: the first installed of base, small, tiny.
pub fn pick_model(installed: impl Fn(&str) -> bool) -> Option<&'static str> {
    ["base", "small", "tiny"].into_iter().find(|m| installed(m))
}

#[derive(Clone, Debug)]
pub struct RawWord {
    pub w: String,
    pub start: f64,
    pub end: f64,
    pub p: f64,
}

#[derive(Clone, Debug)]
pub struct RawSeg {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<RawWord>,
    /// edith-stt's `approx`; a line without it counts as approximate.
    pub approx: bool,
    pub no_speech: Option<f64>,
    pub avg_logprob: Option<f64>,
    pub span_db: Option<f64>,
}


/// Linear resampling, the way edith-stt resamples its own input.
pub fn resample_linear(input: &[f32], src: f64, dst: f64) -> Vec<f32> {
    if input.is_empty() || src <= 0.0 || dst <= 0.0 {
        return Vec::new();
    }
    if (src - dst).abs() < 0.5 {
        return input.to_vec();
    }
    let n = (input.len() as f64 * dst / src).round() as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * src / dst;
            let idx = pos.floor() as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input.get(idx).copied().unwrap_or(0.0);
            let b = input.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac
        })
        .collect()
}

pub fn i16_bytes(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes())
        .collect()
}

/// A 16-bit mono PCM WAV at [`RATE`].
pub fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data = i16_bytes(samples);
    let len = data.len() as u32;
    let mut b = Vec::with_capacity(44 + data.len());
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&data);
    b
}

pub fn num(v: &serde_json::Value, k: &str) -> Result<f64, String> {
    v.get(k).and_then(serde_json::Value::as_f64).ok_or_else(|| format!("edith-stt line has no `{k}` number"))
}

pub fn parse_raw(raw: &str) -> Result<Vec<RawSeg>, String> {
    let mut out = Vec::new();
    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("edith-stt printed a line that is not JSON: {e}"))?;
        let mut words = Vec::new();
        if let Some(arr) = v.get("words").and_then(serde_json::Value::as_array) {
            for w in arr {
                words.push(RawWord {
                    w: w.get("w").and_then(serde_json::Value::as_str).unwrap_or("").trim().to_string(),
                    start: num(w, "start")?,
                    end: num(w, "end")?,
                    p: w.get("p").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                });
            }
        }
        out.push(RawSeg {
            start: num(&v, "start")?,
            end: num(&v, "end")?,
            text: v.get("text").and_then(serde_json::Value::as_str).unwrap_or("").trim().to_string(),
            words,
            approx: v.get("approx").and_then(serde_json::Value::as_bool).unwrap_or(true),
            no_speech: v.get("no_speech").and_then(serde_json::Value::as_f64),
            avg_logprob: v.get("avg_logprob").and_then(serde_json::Value::as_f64),
            span_db: v.get("span_db").and_then(serde_json::Value::as_f64),
        });
    }
    Ok(out)
}

/// One caption cue while it is cleaned up: its words with their probabilities, and the speaker
/// (an index into a mix's speaker lanes; 0 for one lane) whose words they are.
pub struct Cap {
    pub start_us: i64,
    pub end_us: i64,
    pub words: Vec<(String, f64)>,
    pub text: String,
    pub owner: u32,
}

/// Caption cues for timed words (start us, end us, text, probability), each word's speaker in
/// `owners` (parallel to `words`). The words are partitioned into cues by [`partition`], which keeps
/// every cue at [`CUE_MIN_SECS`] where the limits allow it; then a trailing cue that is a
/// hallucination is dropped, cues shorter than the minimum fold into the nearer neighbour of the
/// same speaker, and no cue runs into the next (the earlier one is clipped). A cue never spans two
/// speakers. Returns the cues, for each dropped cue what it said and why, and the cues still under
/// the minimum because the limits leave no split that holds them.
pub fn caption_cues(
    words: &[(i64, i64, String, f64)],
    owners: &[u32],
    max_chars: usize,
    max_secs: f64,
    gap_secs: f64,
) -> (Vec<subtitle::Cue>, Vec<String>, Vec<String>) {
    let mut skipped: Vec<String> = Vec::new();
    let mut order: Vec<usize> = (0..words.len()).filter(|&i| !words[i].2.trim().is_empty()).collect();
    order.sort_by_key(|&i| words[i].0);
    let sorted: Vec<&(i64, i64, String, f64)> = order.iter().map(|&i| &words[i]).collect();
    let own: Vec<u32> = order.iter().map(|&i| owners.get(i).copied().unwrap_or(0)).collect();
    let min_us = (CUE_MIN_SECS * 1e6) as i64;
    // Each run of one speaker is partitioned on its own, so no cue spans two speakers.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut r0 = 0;
    for r1 in 1..=sorted.len() {
        if r1 == sorted.len() || own[r1] != own[r0] {
            for g in partition(&sorted[r0..r1], max_chars, max_secs, gap_secs, min_us) {
                groups.push(g.into_iter().map(|i| i + r0).collect());
            }
            r0 = r1;
        }
    }
    let mut caps: Vec<Cap> = groups
        .iter()
        .filter(|g| !g.is_empty())
        .map(|g| {
            let ws: Vec<(String, f64)> = g.iter().map(|&i| (sorted[i].2.trim().to_string(), sorted[i].3)).collect();
            let start_us = sorted[g[0]].0;
            let end_us = sorted[g[g.len() - 1]].1.max(start_us);
            Cap { start_us, end_us, text: caption_text(&ws, max_chars), words: ws, owner: own[g[0]] }
        })
        .collect();
    if caps.len() > 1 {
        let last = caps.len() - 1;
        if let Some(why) = hallucination(&caps[last], &caps[last - 1]) {
            let c = caps.pop().expect("a last cue");
            skipped.push(format!(
                "\"{}\" ({:.3}s–{:.3}s): {why}",
                c.text.replace('\n', " / "),
                c.start_us as f64 / 1e6,
                c.end_us as f64 / 1e6
            ));
        }
    }
    let mut i = 0;
    while caps.len() > 1 && i < caps.len() {
        if !is_short_cue(&caps[i], min_us) {
            i += 1;
            continue;
        }
        let before = (i > 0).then(|| caps[i].start_us - caps[i - 1].end_us);
        let after = (i + 1 < caps.len()).then(|| caps[i + 1].start_us - caps[i].end_us);
        let prev_nearer = match (before, after) {
            (Some(b), Some(a)) => b <= a,
            (Some(_), None) => true,
            _ => false,
        };
        let can_prev = i > 0 && fits_merged(&caps[i - 1], &caps[i], max_chars, max_secs);
        let can_next = i + 1 < caps.len() && fits_merged(&caps[i], &caps[i + 1], max_chars, max_secs);
        let into_prev = can_prev && (prev_nearer || !can_next);
        if into_prev || can_next {
            let cur = caps.remove(i);
            // Removing `i` leaves the next cue at `i`, so the loop goes on from there either way.
            if into_prev {
                let p = &mut caps[i - 1];
                p.end_us = p.end_us.max(cur.end_us);
                p.words.extend(cur.words);
                p.text = caption_text(&p.words, max_chars);
            } else {
                let n = &mut caps[i];
                n.start_us = n.start_us.min(cur.start_us);
                let mut ws = cur.words;
                ws.append(&mut n.words);
                n.words = ws;
                n.text = caption_text(&n.words, max_chars);
            }
        } else {
            // Nothing it can join: it stays up to min_us, or until the next cue starts.
            let next_start = caps.get(i + 1).map_or(i64::MAX, |c| c.start_us);
            let c = &mut caps[i];
            c.end_us = c.end_us.max((c.start_us + min_us).min(next_start));
            i += 1;
        }
    }
    for i in 0..caps.len().saturating_sub(1) {
        let next = caps[i + 1].start_us;
        let c = &mut caps[i];
        c.end_us = c.end_us.min(next).max(c.start_us);
    }
    // The cues still under the minimum after every move: the limits left no split that holds them.
    let mut short: Vec<String> = Vec::new();
    if caps.len() > 1 {
        for c in caps.iter().filter(|c| c.end_us - c.start_us < min_us) {
            short.push(format!(
                "\"{}\" ({:.3}s–{:.3}s, {:.2} s)",
                c.text.replace('\n', " / "),
                c.start_us as f64 / 1e6,
                c.end_us as f64 / 1e6,
                (c.end_us - c.start_us) as f64 / 1e6
            ));
        }
    }
    let cues = caps
        .into_iter()
        .map(|c| subtitle::Cue { start_us: c.start_us, end_us: c.end_us, text: c.text, image: None, words: Vec::new(), karaoke: Vec::new() })
        .collect();
    (cues, skipped, short)
}

/// How many times the speaker changes as the words are read in time order.
pub fn speaker_changes(words: &[(i64, i64, String, f64)], owners: &[u32]) -> usize {
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by_key(|&i| words[i].0);
    order.windows(2).filter(|p| owners.get(p[0]) != owners.get(p[1])).count()
}

/// The cues for sorted words: contiguous runs, each one a cue the cut would make (see
/// [`cue_valid`]), chosen to leave the fewest cues under `min_us` and then the fewest cues. A
/// cue under the minimum costs as much as ten cues that are not, so words move to a neighbour
/// whenever that holds the minimum, and a short cue is kept only when no run of words around it
/// can hold it. Each cue is the indices of its words, in order.
pub fn partition(
    words: &[&(i64, i64, String, f64)],
    max_chars: usize,
    max_secs: f64,
    gap_secs: f64,
    min_us: i64,
) -> Vec<Vec<usize>> {
    /// The longest cue tried: a cue this long is past any caption a screen takes.
    const LOOK_BACK: usize = 40;
    /// The cost of a cue under the minimum, against 1 for every cue.
    const SHORT: u64 = 10;
    let n = words.len();
    // Best (cost, start of its last cue) for the first `j` words.
    let mut best: Vec<(u64, usize)> = vec![(u64::MAX, 0); n + 1];
    best[0] = (0, 0);
    for j in 1..=n {
        for i in (j.saturating_sub(LOOK_BACK)..j).rev() {
            // A run that is not a cue stays not one as it grows, so the search stops there.
            if i + 1 < j && !cue_valid(&words[i..j], max_chars, max_secs, gap_secs) {
                break;
            }
            if best[i].0 == u64::MAX {
                continue;
            }
            let dur = words[j - 1].1 - words[i].0;
            let cost = 1 + if dur < min_us { SHORT * 10 } else { 0 };
            let total = best[i].0 + cost;
            if total < best[j].0 {
                best[j] = (total, i);
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut j = n;
    while j > 0 {
        let i = best[j].1;
        groups.push((i..j).collect());
        j = i;
    }
    groups.reverse();
    groups
}

/// Whether a run of words is a cue the cut would make: no pause over `gap_secs` inside it, no
/// sentence ending before its last word, under `max_secs`, and two lines of `max_chars`.
pub fn cue_valid(ws: &[&(i64, i64, String, f64)], max_chars: usize, max_secs: f64, gap_secs: f64) -> bool {
    if ws.len() <= 1 {
        return true;
    }
    let gap_us = (gap_secs * 1e6) as i64;
    let max_us = (max_secs * 1e6) as i64;
    if ws.windows(2).any(|p| p[1].0 - p[0].1 > gap_us) {
        return false;
    }
    if ws[..ws.len() - 1].iter().any(|w| closes_sentence(&w.2)) {
        return false;
    }
    if ws[ws.len() - 1].1 - ws[0].0 > max_us {
        return false;
    }
    // Two lines of at most max_chars each, split anywhere between two words.
    let lens: Vec<usize> = ws.iter().map(|w| w.2.trim().chars().count()).collect();
    let total: usize = lens.iter().sum::<usize>() + lens.len() - 1;
    let mut head = 0usize;
    (1..ws.len()).any(|k| {
        head += lens[k - 1] + usize::from(k > 1);
        let tail = total - head - 1;
        head <= max_chars && tail <= max_chars
    })
}

/// Whether a word closes a sentence, looking past closing quotes and brackets (as the cut does).
pub fn closes_sentence(word: &str) -> bool {
    word.trim_end_matches(['"', '\'', ')', ']', '”', '’'])
        .ends_with(['.', '!', '?', '…'])
}

/// Why a trailing cue is a hallucination, or `None`: low confidence, made of words the cue
/// before already said, or a sign-off after a sign-off.
pub fn hallucination(cap: &Cap, prev: &Cap) -> Option<&'static str> {
    if cap.words.is_empty() {
        return None;
    }
    let mean = cap.words.iter().map(|w| w.1).sum::<f64>() / cap.words.len() as f64;
    if mean < CUE_LOW_P {
        return Some("low confidence, the model was unsure of these words");
    }
    let key = |ws: &[(String, f64)]| -> Vec<String> {
        ws.iter().map(|w| stem(&norm(&w.0))).filter(|w| !w.is_empty()).collect()
    };
    let (mine, theirs) = (key(&cap.words), key(&prev.words));
    // The same words in the same order, one after another: a common word scattered through the
    // cue before (the "the" or "us" of a different line) is not a repeat.
    if !mine.is_empty() && theirs.windows(mine.len()).any(|w| w == mine.as_slice()) {
        return Some("repeats the words of the cue before");
    }
    (sign_off(cap) && sign_off(prev)).then_some("a second sign-off after one")
}

/// A cue that is too short to read: under the minimum on screen, or one word.
pub fn is_short_cue(c: &Cap, min_us: i64) -> bool {
    c.end_us - c.start_us < min_us || c.words.len() == 1
}

/// Whether two neighbouring cues can be one: the merged text lays out in two lines of at most
/// `max_chars` each, and the merged cue is no longer than `max_secs`.
pub fn fits_merged(a: &Cap, b: &Cap, max_chars: usize, max_secs: f64) -> bool {
    if a.owner != b.owner {
        return false;
    }
    let span = (a.end_us.max(b.end_us) - a.start_us.min(b.start_us)) as f64 / 1e6;
    if span > max_secs {
        return false;
    }
    let words: Vec<(String, f64)> = a.words.iter().chain(b.words.iter()).cloned().collect();
    caption_text(&words, max_chars).lines().all(|l| l.chars().count() <= max_chars)
}

/// Plural `s` dropped, so "thanks" and "thank" count as one word for repeats.
pub fn stem(w: &str) -> String {
    w.strip_suffix('s').filter(|s| s.len() > 2).unwrap_or(w).to_string()
}

pub fn sign_off(cap: &Cap) -> bool {
    let text = cap.words.iter().map(|w| norm(&w.0)).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    SIGN_OFFS.contains(&text.as_str())
}

/// The text of a cue over at most two lines, split where the longer line is shortest
/// (as the engine lays out a cue).
pub fn caption_text(words: &[(String, f64)], max_chars: usize) -> String {
    let ws: Vec<&str> = words.iter().map(|w| w.0.as_str()).collect();
    let len = |s: &[&str]| s.iter().map(|w| w.chars().count()).sum::<usize>() + s.len().saturating_sub(1);
    let total = len(&ws);
    if total <= max_chars || ws.len() < 2 {
        return ws.join(" ");
    }
    let middle = total as i64 / 2;
    let mut best: Option<(usize, (usize, i64))> = None;
    for k in 1..ws.len() {
        let (a, b) = (len(&ws[..k]), len(&ws[k..]));
        let score = (a.max(b), (a as i64 - middle).abs());
        if best.is_none_or(|(_, s)| score < s) {
            best = Some((k, score));
        }
    }
    let k = best.map_or(1, |(k, _)| k);
    format!("{}\n{}", ws[..k].join(" "), ws[k..].join(" "))
}

/// Gives each cue the transcript's word times: every word belongs to the cue that starts last at
/// or before it. A cue whose words do not read back as its text (the cut and merge steps can
/// differ from the raw word list) keeps none and is drawn with evenly spread ones.
pub fn attach_word_times(cues: &mut [subtitle::Cue], words: &[(i64, i64, String, f64)]) {
    let mut sorted: Vec<&(i64, i64, String, f64)> = words.iter().filter(|w| !w.2.trim().is_empty()).collect();
    sorted.sort_by_key(|w| w.0);
    let mut by_cue: Vec<Vec<(i64, i64, String)>> = vec![Vec::new(); cues.len()];
    for w in sorted {
        if let Some(i) = cues.iter().rposition(|c| c.start_us <= w.0) {
            let end = w.1.clamp(w.0, cues[i].end_us.max(w.0));
            by_cue[i].push((w.0, end, w.2.trim().to_string()));
        }
    }
    for (c, ws) in cues.iter_mut().zip(by_cue) {
        c.words = ws;
        if c.real_words().is_none() {
            c.words.clear();
        }
    }
}

/// Capitalises the first letter of each cue that begins a sentence: the first cue, a cue that starts
/// where a transcript segment's first kept word does (`segment_starts`, in microseconds), and a cue
/// after one that ends in . ! or ?. A dropped filler leaves the word after it heading its cue in
/// lower case, so "Um, hello there." would read "hello there." without this.
pub fn capitalise_sentence_starts(cues: &mut [subtitle::Cue], segment_starts: &[i64]) {
    let mut begins = true;
    for c in cues.iter_mut() {
        if (begins || segment_starts.contains(&c.start_us))
            && let Some(first) = c.text.chars().next().filter(|f| f.is_lowercase())
        {
            let upper: String = first.to_uppercase().collect();
            c.text.replace_range(..first.len_utf8(), &upper);
        }
        begins = c.text.trim_end().ends_with(['.', '!', '?']);
    }
}

// ---- running edith-stt ------------------------------------------------------

/// A running speech job's shared state: how far it is, and whether anyone still wants it.
/// The GUI reads `fraction` for its progress line and sets `cancel` for its Cancel button.
#[derive(Debug, Default)]
pub struct Job {
    /// Progress in thousandths (0..=1000).
    pub permille: std::sync::atomic::AtomicU32,
    pub cancel: AtomicBool,
}

impl Job {
    /// Progress as a fraction, 0..=1.
    pub fn fraction(&self) -> f32 {
        self.permille.load(Ordering::Relaxed) as f32 / 1000.0
    }

    fn set(&self, f: f64) {
        self.permille.store((f.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// The error a cancelled job returns.
pub const CANCELLED: &str = "cancelled";

/// Runs edith-stt on one WAV and returns its stdout (one JSON line per segment). Its progress
/// lines feed `job`; setting `job.cancel` kills it. The last few other stderr lines are kept
/// for the error if the binary fails.
#[allow(clippy::too_many_arguments)]
pub fn run_stt(bin: &Path, wav: &Path, model: &str, lang: &str, hint: &str, offset: f64, label: &str, job: &Job) -> Result<String, String> {
    let hint_args: Vec<&str> = if hint.is_empty() { Vec::new() } else { vec!["--prompt", hint] };
    let mut child = Command::new(bin)
        .args(["--model", model, "--language", lang, "--offset"])
        .arg(format!("{offset:.3}"))
        .args(&hint_args)
        .arg(wav)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting {}: {e}", bin.display()))?;
    let stderr = child.stderr.take().ok_or("edith-stt: no stderr pipe")?;
    let mut stdout = child.stdout.take().ok_or("edith-stt: no stdout pipe")?;
    let progress = std::sync::Arc::new(Job::default());
    let seen = std::sync::Arc::clone(&progress);
    let err_reader = std::thread::spawn(move || {
        let mut tail: Vec<String> = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(p) = line.strip_prefix("progress ").and_then(|v| v.trim().parse::<f64>().ok()) {
                seen.set(p);
            } else if !line.trim().is_empty() {
                tail.push(line);
                if tail.len() > 6 {
                    tail.remove(0);
                }
            }
        }
        tail
    });
    let out_reader = std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).map(|_| out)
    });
    let status = loop {
        job.set(f64::from(progress.fraction()));
        if job.cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = err_reader.join();
            let _ = out_reader.join();
            return Err(CANCELLED.into());
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(40)),
            Err(e) => return Err(format!("waiting for edith-stt: {e}")),
        }
    };
    let tail = err_reader.join().unwrap_or_default();
    let out = out_reader
        .join()
        .map_err(|_| "reading edith-stt output: the reader panicked".to_string())?
        .map_err(|e| format!("reading edith-stt output: {e}"))?;
    if !status.success() {
        return Err(format!("edith-stt failed on {label}: {}", tail.join(" | ")));
    }
    job.set(1.0);
    Ok(out)
}

/// The models installed, smallest first.
pub fn installed_models() -> Vec<&'static str> {
    MODELS.iter().copied().filter(|m| check_model(m).is_ok()).collect()
}

/// Where the speech models live: one folder per model, `whisper-<name>`, here.
pub fn models_dir() -> PathBuf {
    home().join(".cache/edith/models")
}

/// The files a model folder holds, and whether each is needed (the last is a nicety).
const MODEL_FILES: [(&str, bool); 4] = [
    ("config.json", true),
    ("model.safetensors", true),
    ("tokenizer.json", true),
    ("preprocessor_config.json", true),
];
const MODEL_OPTIONAL: &str = "generation_config.json";

/// About how many bytes a model's files add up to, for a progress bar before the sizes are known.
pub fn model_bytes(model: &str) -> u64 {
    match model {
        "tiny" => 152_000_000,
        "small" => 970_000_000,
        _ => 293_000_000,
    }
}

/// Downloads the `model` (one of [`MODELS`]) into [`models_dir`] with `curl`, from the same place
/// `edith-stt` fetches it (huggingface.co/openai/whisper-<model>). The folder appears only when
/// every file is in, so a half-finished download is never taken for an installed model; the files
/// already fetched are kept for the next try. `job` carries the progress and the cancel.
pub fn download_model(model: &str, job: &Job) -> Result<(), String> {
    if !MODELS.contains(&model) {
        return Err(format!("model `{model}` is not one of {}", MODELS.join(", ")));
    }
    let root = models_dir();
    let part = root.join(format!("whisper-{model}.part"));
    let done = root.join(format!("whisper-{model}"));
    std::fs::create_dir_all(&part).map_err(|e| format!("cannot create {}: {e}", part.display()))?;
    let base = format!("https://huggingface.co/openai/whisper-{model}/resolve/main");
    let total = model_bytes(model) as f64;
    let have = |dir: &Path| -> u64 {
        std::fs::read_dir(dir)
            .map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
            .unwrap_or(0)
    };
    let files = MODEL_FILES.iter().map(|&(f, need)| (f, need)).chain([(MODEL_OPTIONAL, false)]);
    for (file, needed) in files {
        let target = part.join(file);
        if target.exists() {
            continue;
        }
        let tmp = part.join(format!("{file}.tmp"));
        let mut child = Command::new("curl")
            .args(["-fsSL", "--retry", "2", "--connect-timeout", "20", "-o"])
            .arg(&tmp)
            .arg(format!("{base}/{file}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot run curl ({e}): install curl, or copy the model into {}", done.display()))?;
        let status = loop {
            if job.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&tmp);
                return Err(CANCELLED.into());
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            job.set((have(&part) as f64 / total).min(0.99));
            std::thread::sleep(std::time::Duration::from_millis(150));
        };
        if status.success() {
            std::fs::rename(&tmp, &target).map_err(|e| format!("cannot save {file}: {e}"))?;
        } else {
            let _ = std::fs::remove_file(&tmp);
            if needed {
                return Err(format!("could not download {file}: check the internet connection and try again"));
            }
        }
    }
    std::fs::rename(&part, &done).map_err(|e| format!("cannot finish the download: {e}"))?;
    job.set(1.0);
    Ok(())
}

/// Whether speech can run at all here: the edith-stt binary is built and a model is installed.
pub fn available() -> Result<(), String> {
    stt_bin()?;
    if installed_models().is_empty() {
        return Err("The speech model is not installed yet.".into());
    }
    Ok(())
}

// ---- hearing the timeline ---------------------------------------------------

/// What to listen to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hear {
    /// Every unmuted lane, as an export writes it.
    Mix,
    /// One audio lane alone, as the engine plays it (lane gain and ducks not applied).
    Lane(Lane),
}

/// One word on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    pub text: String,
    pub start_f: u32,
    pub end_f: u32,
    /// The model's probability for the word.
    pub p: f64,
}

/// The audio of `hear` as mono 16 kHz samples of timeline time (a gap is silence), or `None`
/// when there is no sound at all.
fn timeline_16k(s: &PlaybackSession, hear: Hear) -> Result<Option<Vec<f32>>, String> {
    let fps = s.meta().frame_rate;
    let (rate, ch, chunks): (f64, usize, Box<dyn Iterator<Item = Vec<f32>>>) = match hear {
        Hear::Lane(lane) => {
            let Some((meta, rx)) = crate::normalize::open_lane(s, lane, 0, fps)? else {
                return Ok(None);
            };
            (f64::from(meta.sample_rate), usize::from(meta.channels).max(1), Box::new(rx.into_iter().map(|c| c.samples)))
        }
        Hear::Mix => {
            let wav = crate::normalize::render_mix(s)?;
            let sources = [(wav.0.clone(), 0usize)];
            let Some((meta, rx)) = AudioSession::open_multi_streams(&sources, &[(Some(0), 0.0, f64::INFINITY)])
                .map_err(|e| format!("reading the mix: {e}"))?
            else {
                return Ok(None);
            };
            // Drained before the temp file goes: the reader holds it open.
            let all: Vec<Vec<f32>> = rx.into_iter().map(|c| c.samples).collect();
            (f64::from(meta.sample_rate), usize::from(meta.channels).max(1), Box::new(all.into_iter()))
        }
    };
    let mut mono = Vec::new();
    for samples in chunks {
        for frame in samples.chunks(ch) {
            mono.push(frame.iter().sum::<f32>() / frame.len() as f32);
        }
    }
    Ok(Some(resample_linear(&mono, rate, f64::from(RATE))))
}

/// A known silence hallucination: Whisper's stock sign-off over audio the model doubts is speech.
fn silence_hallucination(text: &str, no_speech: Option<f64>, avg_logprob: Option<f64>) -> bool {
    const STOCK: &[&str] = &["thank you", "thank you for watching", "thanks for watching", "you"];
    let t = text.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    STOCK.contains(&t.as_str()) && (no_speech.is_some_and(|p| p >= 0.5) || avg_logprob.is_some_and(|lp| lp < -1.0))
}

/// What is said in `hear`, word by word, in timeline frames. `fillers` asks Whisper to keep its
/// disfluencies ([`FILLER_PROMPT`]); without it the model tends to drop them. The model runs on
/// a worker thread of the caller's choosing: this blocks until it is done or `job` is cancelled.
pub fn transcribe(s: &PlaybackSession, hear: Hear, model: &str, lang: &str, fillers: bool, job: &Job) -> Result<Vec<Spoken>, String> {
    listen(prepare(s, hear)?, model, lang, fillers, job)
}

/// The sound of `hear` as the speech model takes it, read off the session. Split from
/// [`listen`] so a window can read the sound where the session is and run the model on a thread.
pub struct Heard {
    samples: Vec<f32>,
    fps: f64,
    label: String,
}

/// Reads what [`transcribe`] listens to: the timeline's sound at 16 kHz mono.
pub fn prepare(s: &PlaybackSession, hear: Hear) -> Result<Heard, String> {
    let samples = timeline_16k(s, hear)?.ok_or("there is no sound to listen to")?;
    if samples.iter().all(|x| x.abs() < 1e-4) {
        return Err("the sound is silent: there is nothing to transcribe".into());
    }
    let label = match hear {
        Hear::Mix => "the mix".to_string(),
        Hear::Lane(l) => l.label(),
    };
    Ok(Heard { samples, fps: s.meta().frame_rate, label })
}

/// Runs the speech model over what [`prepare`] read. Needs no session, so it can run anywhere.
pub fn listen(heard: Heard, model: &str, lang: &str, fillers: bool, job: &Job) -> Result<Vec<Spoken>, String> {
    let bin = stt_bin()?;
    check_model(model)?;
    check_lang(lang)?;
    let fps = heard.fps;
    if job.cancelled() {
        return Err(CANCELLED.into());
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let wav = std::env::temp_dir().join(format!("edith-speech-{}-{nanos}.wav", std::process::id()));
    std::fs::write(&wav, wav_bytes(&heard.samples)).map_err(|e| format!("writing {}: {e}", wav.display()))?;
    let guard = TempFile(wav.clone());
    let hint = if fillers { FILLER_PROMPT } else { "" };
    let raw = run_stt(&bin, &wav, model, lang, hint, 0.0, &heard.label, job);
    drop(guard);
    let mut words = Vec::new();
    for seg in parse_raw(&raw?)? {
        if seg.text.is_empty() || silence_hallucination(&seg.text, seg.no_speech, seg.avg_logprob) {
            continue;
        }
        for w in seg.words.iter().filter(|w| !w.w.is_empty()) {
            let start_f = (w.start * fps).round().max(0.0) as u32;
            words.push(Spoken { text: w.w.clone(), start_f, end_f: ((w.end * fps).round() as u32).max(start_f), p: w.p });
        }
    }
    Ok(words)
}

struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

// ---- captions ---------------------------------------------------------------

/// The characters per line and seconds per cue captions default to.
pub const CAPTION_MAX_CHARS: usize = 42;
pub const CAPTION_MAX_SECS: f64 = 6.0;

/// Caption cues for `words`: disfluencies left out, cut at pauses and at the length limits, a
/// trailing hallucination dropped, short cues folded, sentence starts capitalised, and each cue
/// given its words' real times.
pub fn captions(words: &[Spoken], fps: f64) -> Vec<subtitle::Cue> {
    let us = |f: u32| (f64::from(f) / fps * 1e6).round() as i64;
    let timed: Vec<(i64, i64, String, f64)> = words
        .iter()
        .filter(|w| !CAPTION_FILLERS.contains(&norm(&w.text).as_str()))
        .map(|w| (us(w.start_f), us(w.end_f), w.text.clone(), w.p))
        .collect();
    if timed.is_empty() {
        return Vec::new();
    }
    let owners = vec![0; timed.len()];
    let (mut cues, _, _) = caption_cues(&timed, &owners, CAPTION_MAX_CHARS, CAPTION_MAX_SECS, CAPTION_GAP_SECS);
    capitalise_sentence_starts(&mut cues, &[]);
    attach_word_times(&mut cues, &timed);
    cues
}

/// Puts `cues` on caption lane `lane` as a new subtitle track, written to `srt` first (the track
/// is a file, as every subtitle track is). One undo step. Returns the track's index.
pub fn place_captions(s: &mut PlaybackSession, cues: &[subtitle::Cue], lane: Lane, srt: &Path) -> Result<usize, String> {
    use crate::project::{LaneKind, SubClip};
    if lane.kind != LaneKind::Subtitle {
        return Err(format!("{} is not a caption lane; captions go on S1..", lane.label()));
    }
    if s.lane_locked(lane) {
        return Err(format!("lane {} is locked: unlock it first", lane.label()));
    }
    let end_us = cues.iter().map(|c| c.end_us).max().unwrap_or(0);
    if cues.is_empty() || end_us <= 0 {
        return Err("there are no cues to place".into());
    }
    if let Some(dir) = srt.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    subtitle::save_cues(srt, cues).map_err(|e| format!("writing {}: {e}", srt.display()))?;
    let fps = s.meta().frame_rate;
    let pushed = s.history_pushes();
    s.begin_batch();
    let placed = (|| -> Result<usize, String> {
        let added = s.import_subtitles(srt).map_err(|e| format!("importing {}: {e}", srt.display()))?;
        if added == 0 {
            return Err(format!("importing {} added no subtitle track", srt.display()));
        }
        let want = std::fs::canonicalize(srt).unwrap_or_else(|_| srt.to_path_buf());
        let track = s
            .subtitles()
            .iter()
            .rposition(|t| std::fs::canonicalize(&t.path).unwrap_or_else(|_| t.path.clone()) == want)
            .ok_or_else(|| format!("{} was imported but no track for it was found", srt.display()))?;
        // The transcript's word times ride along: the SRT cannot hold them.
        s.attach_cue_words(track, cues);
        let mut guard = 0;
        while !s.subtitle_lanes().contains(&lane) {
            guard += 1;
            if guard > lane.ord + 1 {
                return Err(format!("{} could not be made", lane.label()));
            }
            s.add_lane(LaneKind::Subtitle);
        }
        s.place_sub(lane, 0, SubClip::whole_track(track, end_us, fps)).map_err(|e| e.to_string())?;
        Ok(track)
    })();
    s.end_batch();
    if placed.is_err() && s.history_pushes() > pushed {
        // A failed attempt leaves nothing behind: take back what it pushed.
        for _ in pushed..s.history_pushes() {
            if !s.undo() {
                break;
            }
        }
        s.clear_redo();
    }
    placed
}

// ---- fillers ----------------------------------------------------------------

/// One filler word found in speech: "um", "uh" and the like.
#[derive(Clone, Debug, PartialEq)]
pub struct Filler {
    pub text: String,
    pub start_f: u32,
    pub end_f: u32,
    /// The span a cut takes: the word padded a little each side, never into its neighbours.
    pub cut: (u32, u32),
}

/// The filler words among `words`, in order. A cut span pads the word by `pad` frames each side
/// but stops at the neighbouring words, and at the timeline `end`.
pub fn find_fillers(words: &[Spoken], pad: u32, end: u32) -> Vec<Filler> {
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if !CAPTION_FILLERS.contains(&norm(&w.text).as_str()) {
            continue;
        }
        let prev_end = if i == 0 { 0 } else { words[i - 1].end_f };
        let next_start = words.get(i + 1).map_or(u32::MAX, |n| n.start_f);
        let a = w.start_f.saturating_sub(pad).max(prev_end.min(w.start_f));
        let b = w.end_f.saturating_add(pad).min(next_start.max(w.end_f)).min(end.max(w.end_f));
        out.push(Filler { text: w.text.trim_matches(|c: char| !c.is_alphanumeric()).to_string(), start_f: w.start_f, end_f: w.end_f, cut: (a, b) });
    }
    out
}

/// Cuts the spans of `fillers` out of `lanes`, closing each hole, as one undo step. Returns the
/// regions cut, `(start, length)` in frames, for [`remap_words`].
pub fn cut_fillers(s: &mut PlaybackSession, fillers: &[Filler], lanes: &[Lane]) -> Result<Vec<(u32, u32)>, String> {
    let spans: Vec<(u32, u32)> = fillers.iter().map(|f| f.cut).collect();
    cut_spans(s, &spans, lanes)
}

/// Cuts `spans` (`(start, end)` frames, any order, overlaps merged) out of `lanes` and closes
/// each hole, as one undo step: the one door filler cuts and transcript word cuts share. Returns
/// the regions cut as `(start, length)`.
pub fn cut_spans(s: &mut PlaybackSession, spans: &[(u32, u32)], lanes: &[Lane]) -> Result<Vec<(u32, u32)>, String> {
    let mut spans: Vec<(u32, u32)> = spans.iter().copied().filter(|(a, b)| b > a).collect();
    spans.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    if merged.is_empty() {
        return Err("there is nothing to cut".into());
    }
    // cut_regions takes (start, length).
    let regions: Vec<(u32, u32)> = merged.iter().map(|&(a, b)| (a, b - a)).collect();
    s.cut_regions(&regions, lanes).map_err(|e| e.to_string())?;
    Ok(regions)
}

/// The span a cut of words `first..=last` takes: from the first word's start to the last word's
/// end, padded by `pad` frames each side but stopped at the neighbouring words and at the
/// timeline `end`, so no other speech goes with it.
pub fn word_span(words: &[Spoken], first: usize, last: usize, pad: u32, end: u32) -> Option<(u32, u32)> {
    let (a, b) = (words.get(first)?, words.get(last)?);
    let prev_end = if first == 0 { 0 } else { words[first - 1].end_f };
    let next_start = words.get(last + 1).map_or(u32::MAX, |n| n.start_f);
    let from = a.start_f.saturating_sub(pad).max(prev_end.min(a.start_f));
    let to = b.end_f.saturating_add(pad).min(next_start.max(b.end_f)).min(end.max(b.end_f));
    Some((from, to))
}

/// `words` as they stand after `regions` (`(start, length)`, sorted, as [`cut_spans`] returns
/// them) were cut and closed: a word the cut covers is gone, the rest move up by the time cut
/// before them.
pub fn remap_words(words: &[Spoken], regions: &[(u32, u32)]) -> Vec<Spoken> {
    let moved = |f: u32| -> u32 { f - regions.iter().map(|&(a, len)| f.saturating_sub(a).min(len)).sum::<u32>() };
    words
        .iter()
        .filter(|w| !regions.iter().any(|&(a, len)| w.start_f >= a && w.end_f <= a + len && (w.end_f > w.start_f || w.start_f > a)))
        .map(|w| Spoken { text: w.text.clone(), start_f: moved(w.start_f), end_f: moved(w.end_f), p: w.p })
        .collect()
}

/// Whether `text` is a disfluency ("um", "uh"...): what a transcript view dims.
pub fn is_filler(text: &str) -> bool {
    CAPTION_FILLERS.contains(&norm(text).as_str())
}

#[cfg(test)]
mod transcript_cut_tests {
    use super::*;

    fn w(t: &str, a: u32, b: u32) -> Spoken {
        Spoken { text: t.into(), start_f: a, end_f: b, p: 0.9 }
    }

    #[test]
    fn a_word_span_pads_but_stops_at_neighbours() {
        let ws = [w("so", 10, 20), w("um", 22, 30), w("yes", 31, 40)];
        assert_eq!(word_span(&ws, 1, 1, 3, 100), Some((20, 31)));
        assert_eq!(word_span(&ws, 0, 1, 3, 100), Some((7, 31)));
        assert_eq!(word_span(&ws, 2, 2, 5, 42), Some((30, 42)));
        assert_eq!(word_span(&ws, 3, 3, 3, 100), None);
    }

    #[test]
    fn remapping_drops_cut_words_and_closes_the_hole() {
        let ws = [w("a", 0, 10), w("b", 12, 20), w("c", 22, 30), w("d", 32, 40)];
        let out = remap_words(&ws, &[(11, 10)]);
        assert_eq!(out.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["a", "c", "d"]);
        assert_eq!((out[1].start_f, out[1].end_f), (12, 20));
        assert_eq!((out[2].start_f, out[2].end_f), (22, 30));
        let two = remap_words(&ws, &[(11, 10), (31, 9)]);
        assert_eq!(two.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(two[1].start_f, 12);
    }

    #[test]
    fn fillers_are_told_by_spelling() {
        assert!(is_filler("Um,") && is_filler("uh") && !is_filler("umbrella"));
    }
}
