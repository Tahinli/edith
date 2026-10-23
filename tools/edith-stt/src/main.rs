//! edith-stt: offline Whisper speech-to-text on CPU (candle), JSON-lines output.
//!
//! usage: edith-stt --model base|small|tiny|<dir> [--language en|auto] [--offset SECS] [--no-align] [--keep-all] <input.wav>
//!
//! stdout: one JSON object per segment
//!   {"start":s,"end":s,"text":"...","words":[{"w":"Hello","start":s,"end":s,"p":prob}],"approx":false,
//!    "no_speech":p,"avg_logprob":lp}
//!   `approx` is true when the segment's words were spread proportionally (--no-align, or the
//!   cross-attention alignment failed for it); false when words come from DTW over cross-attention.
//!   `no_speech` is the window's `<|nocaptions|>` probability at the first decode step; `avg_logprob`
//!   is the mean token log-prob of the segment; `span_db` is the segment's level over the window's
//!   noise floor (null when unknown). Segments that look like hallucinated silence are
//!   dropped unless `--keep-all` is given (see `silence_reason`). With an English transcript the
//!   decoder never emits CJK, kana, Hangul, Cyrillic, Arabic or Indic tokens (see `Suppress`).
//! stderr: progress lines `progress <0..1>` and diagnostics.

mod whisper_model;

use anyhow::{anyhow, bail, Context, Result};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self as m, Config};
use whisper_model::Whisper;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const SR: usize = 16_000;
/// 30 s window in samples (480000).
const WINDOW: usize = m::N_SAMPLES;
/// Samples per mel frame (10 ms).
const FRAME: usize = m::HOP_LENGTH;
/// Whisper timestamp token resolution.
const SEC_PER_TS: f64 = 0.02;
/// Temperature fallback schedule, as Whisper's reference: a window whose output is rejected is
/// decoded again at the next temperature.
const TEMPS: [f64; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
/// A window whose text compresses above this ratio is a repetition: rejected.
const MAX_COMPRESSION_RATIO: f64 = 2.4;
/// A window whose mean token log-prob is below this is rejected.
const MIN_AVG_LOGPROB: f64 = -1.0;
const MAX_REPEAT: usize = 20;
/// A text phrase (2 to `LOOP_MAX_PERIOD` tokens, timestamps skipped) repeated this many times in
/// a row is a loop: the window ends after its first copy. Whisper loops on silence and fillers.
const LOOP_REPEATS: usize = 4;
const LOOP_MAX_PERIOD: usize = 24;
/// Seek step after a looped window that has no timestamp before the loop. Short, so the speech
/// that follows the loop is decoded next instead of being skipped with the whole window.
const LOOP_STEP_S: f64 = 5.0;
/// Longest `--prompt` kept, in tokens (Whisper's prompt budget).
const MAX_PREV_TOKENS: usize = 223;
/// Silence filter thresholds (see `silence_reason`).
const NO_SPEECH_PROB: f64 = 0.6;
const NO_SPEECH_LOGPROB: f64 = -1.0;
const KNOWN_SIZES: &[&str] = &["tiny", "base", "small", "medium", "large-v2", "large-v3"];
const REQUIRED: &[&str] = &[
    "config.json",
    "model.safetensors",
    "tokenizer.json",
    "preprocessor_config.json",
];
const OPTIONAL: &[&str] = &["generation_config.json"];

fn main() {
    if let Err(e) = run() {
        eprintln!("edith-stt: error: {e:#}");
        std::process::exit(1);
    }
}

struct Cli {
    model: String,
    language: String,
    offset: f64,
    /// Word times from cross-attention DTW (default). `--no-align` gives proportional times.
    align: bool,
    /// `--keep-all`: emit every segment, including ones the silence filter would drop.
    keep_all: bool,
    /// `--prompt`: text Whisper sees before the audio, as OpenAI's `initial_prompt`. Names and
    /// jargon spelled the way they should come out ("Edith, LUFS") steer the decoder.
    prompt: Option<String>,
    input: PathBuf,
}

fn usage() -> &'static str {
    "usage: edith-stt --model base|small|tiny|<dir> [--language en|auto] [--offset SECS] [--no-align] [--keep-all] [--prompt TEXT] <input.wav>"
}

fn parse_cli() -> Result<Cli> {
    let mut model = "base".to_string();
    let mut language = "en".to_string();
    let mut offset = 0.0f64;
    let mut align = true;
    let mut keep_all = false;
    let mut prompt: Option<String> = None;
    let mut input: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => model = args.next().context("--model needs a value")?,
            "--language" => language = args.next().context("--language needs a value")?,
            "--offset" => {
                offset = args
                    .next()
                    .context("--offset needs a value")?
                    .parse()
                    .context("--offset must be a number of seconds")?
            }
            "--no-align" => align = false,
            "--keep-all" => keep_all = true,
            "--prompt" => prompt = Some(args.next().context("--prompt needs a value")?),
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            s if s.starts_with("--") => bail!("unknown flag {s}\n{}", usage()),
            s => {
                if input.is_some() {
                    bail!("only one input file allowed\n{}", usage());
                }
                input = Some(PathBuf::from(s));
            }
        }
    }
    let input = input.with_context(|| usage().to_string())?;
    Ok(Cli { model, language, offset, align, keep_all, prompt, input })
}

fn run() -> Result<()> {
    let cli = parse_cli()?;
    let dir = resolve_model(&cli.model)?;
    let t_start = Instant::now();

    let cfg: Config = serde_json::from_reader(BufReader::new(
        File::open(dir.join("config.json")).context("open config.json")?,
    ))?;
    let pc: serde_json::Value = serde_json::from_reader(BufReader::new(
        File::open(dir.join("preprocessor_config.json")).context("open preprocessor_config.json")?,
    ))?;
    let filters = load_mel_filters(&pc, cfg.num_mel_bins)?;

    let dev = Device::Cpu;
    // SAFETY: the safetensors file is read-only and we do not modify it while mapped.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &dev)?
    };
    let mut model = Whisper::load(&vb, cfg.clone())?;
    let vocab = Vocab::load(&dir.join("tokenizer.json"))?;
    let ids = Ids::new(&vocab)?;
    let heads = alignment_heads(&dir, &cfg)?;
    eprintln!("model loaded from {} in {:.2}s", dir.display(), t_start.elapsed().as_secs_f64());

    let pcm = read_wav_16k_mono(&cli.input)?;
    let audio_s = pcm.len() as f64 / SR as f64;
    let total_frames = pcm.len().div_ceil(FRAME);

    let mut lang: Option<u32> = if cli.language.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(vocab.id(&format!("<|{}|>", cli.language.to_ascii_lowercase()))?)
    };

    let t_decode = Instant::now();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // Previous-text prompt: <|startofprev|> then the hint's tokens, the newest 223 at most
    // (half of Whisper's 448-token context, less one, as in OpenAI's `initial_prompt`).
    let mut prev: Vec<u32> = Vec::new();
    if let Some(text) = cli.prompt.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let mut toks = vocab.encode(&format!(" {text}"));
        if toks.len() > MAX_PREV_TOKENS {
            toks.drain(..toks.len() - MAX_PREV_TOKENS);
        }
        if !toks.is_empty() {
            prev.push(ids.startofprev);
            prev.extend(toks);
        }
        eprintln!("prompt: {} token(s)", prev.len().saturating_sub(1));
    }
    let en_id = vocab.id("<|en|>").ok();
    let mut sup: Option<Suppress> = None;
    let mut seek = 0usize; // in mel frames (10 ms)
    let mut n_segs = 0usize;
    let mut align_s = 0f64;
    let mut align_fail = 0usize;
    // Absolute end of the last word emitted: the gap before the next segment starts there.
    let mut prev_end_abs = 0f64;
    while seek < total_frames {
        let start_sample = seek * FRAME;
        let end_sample = (start_sample + WINDOW).min(pcm.len());
        let content_frames = (end_sample - start_sample).div_ceil(FRAME);
        let content_s = content_frames as f64 * 0.01;
        let seek_s = seek as f64 * 0.01;

        let mut win = vec![0f32; WINDOW];
        win[..end_sample - start_sample].copy_from_slice(&pcm[start_sample..end_sample]);
        let mel = m::audio::pcm_to_mel(&cfg, &win, &filters);
        let n_frames = mel.len() / cfg.num_mel_bins;
        let mel_t = Tensor::from_vec(mel, (1, cfg.num_mel_bins, n_frames), &dev)?
            .narrow(2, 0, WINDOW / FRAME)?;
        let enc = model.encoder.forward(&mel_t, true)?;
        // Levels and noise floor of the window's real audio, not its zero padding.
        let lv = levels(&pcm[start_sample..end_sample]);
        let speech = speech_frames(&lv);
        let n_content = (end_sample - start_sample).div_ceil(ENC_HOP);

        if lang.is_none() {
            let (id, code) = detect_language(&mut model, &enc, &vocab, &ids, &dev)?;
            eprintln!("detected language {code}");
            lang = Some(id);
        }
        let mut prompt = prev.clone();
        prompt.extend([ids.sot, lang.unwrap(), ids.transcribe]);

        // English-only suppression follows the language in use: decided once, on the first window.
        let english = lang == en_id;
        let sup = sup.get_or_insert_with(|| Suppress::new(&vocab, english, ids.ts0));
        let mut attempt = 0;
        let (toks, no_speech, looped) = loop {
            let temp = TEMPS[attempt];
            let (toks, no_speech, looped) =
                decode_window(&mut model, &enc, &prompt, temp, &ids, &cfg, sup, &mut rng, &dev)?;
            let ids_only: Vec<u32> = toks.iter().map(|t| t.0).collect();
            let text = vocab.decode(&ids_only);
            let ratio = compression_ratio(&text)?;
            let avg = avg_logprob(&toks);
            attempt += 1;
            let bad = window_rejected(ratio, avg, looped && !has_text(&toks, ids.ts0));
            if !bad || attempt == TEMPS.len() {
                break (toks, no_speech, looped);
            }
            eprintln!(
                "window at {seek_s:.2}s: temp {temp} rejected (ratio {ratio:.2}, avg_logprob {avg:.2}, looped {looped}), retrying"
            );
        };

        if std::env::var_os("EDITH_STT_DEBUG").is_some() {
            let ids_dbg: Vec<u32> = toks.iter().map(|t| t.0).collect();
            eprintln!("debug window {seek_s:.2}s: {} tokens: {:?}", toks.len(), &ids_dbg[..ids_dbg.len().min(60)]);
            eprintln!("debug text: {:?}", vocab.decode(&ids_dbg));
        }
        // Split into segments and find where to seek next.
        let ends_with_ts = toks.last().is_some_and(|t| t.0 >= ids.ts0);
        let last_ts_time = toks
            .iter()
            .rev()
            .find(|t| t.0 >= ids.ts0)
            .map(|t| (t.0 - ids.ts0) as f64 * SEC_PER_TS);
        let segs = split_segments(&toks, ids.ts0, content_s);

        for seg in &segs {
            // The model can place timestamps past the real audio in a short tail window.
            if seg.start >= content_s || starts_past_end(seek_s + seg.start, audio_s) {
                eprintln!("dropped segment at {:.2}s: starts at or past the audio end", seek_s + seg.start);
                continue;
            }
            let abs_start = seek_s + seg.start + cli.offset;
            let abs_end = (seek_s + seg.end.min(content_s) + cli.offset).max(abs_start);
            let text_ids: Vec<u32> = seg.toks.iter().map(|t| t.0).collect();
            let text = vocab.decode(&text_ids).trim().to_string();
            if text.is_empty() {
                continue;
            }
            let avg = avg_logprob(&seg.toks);
            // Span level against the window's noise floor, from the window start.
            let span = span_rel_db(&lv, seg.start, seg.end.min(content_s));
            if let Some(reason) = silence_reason(no_speech, avg, span) {
                if !cli.keep_all {
                    eprintln!(
                        "skipped segment {abs_start:.2}-{abs_end:.2}s ({reason}, no_speech {no_speech:.2}, avg_logprob {avg:.2}): {text:?}"
                    );
                    continue;
                }
            }
            let aligned = if cli.align {
                let t_al = Instant::now();
                let crop = trim_crop_to_speech(
                    crop_frames(seg.start, seg.end.min(content_s), n_content, ALIGN_PAD_LEFT_S, ALIGN_PAD_RIGHT_S),
                    &speech,
                );
                let r = aligned_words(
                    &mut model,
                    &enc,
                    &prompt,
                    &seg.toks,
                    &vocab,
                    &ids,
                    &heads,
                    crop,
                    n_content,
                    seek_s + cli.offset,
                    &speech,
                    &dev,
                );
                align_s += t_al.elapsed().as_secs_f64();
                match r {
                    Ok(w) => Some(w),
                    Err(e) => {
                        align_fail += 1;
                        eprintln!("word alignment failed at {abs_start:.2}s, using proportional times: {e:#}");
                        None
                    }
                }
            } else {
                None
            };
            let (mut words, approx) = match aligned {
                Some(w) => (w, false),
                None => (word_times(&vocab, &seg.toks, abs_start, abs_end), true),
            };
            // Words that start at or past the audio end are cut, the rest of the segment stays;
            // a word that runs past the end has its end clamped to it.
            let n_words = words.len();
            let cut = fit_words_to_audio(&mut words, cli.offset, audio_s);
            if n_words > 0 && words.is_empty() {
                eprintln!("dropped segment at {abs_start:.2}s: every word starts at or past the audio end: {text:?}");
                continue;
            }
            let text = if cut > 0 {
                eprintln!("cut {cut} word(s) at {abs_start:.2}s that start at or past the audio end");
                words.iter().filter_map(|w| w["w"].as_str()).collect::<Vec<_>>().join(" ")
            } else {
                text
            };
            // A leading filler the window's decode dropped (Whisper drops a leading "Um" depending
            // on the window it is given). When speech sounds for over ONSET_MIN_S before the first
            // word, after the previous word, that gap is decoded again on its own and the filler
            // words it holds go in front of the segment.
            let win_abs = seek_s + cli.offset;
            let (words, text) = match (cli.align, words.first().and_then(|w| w["start"].as_f64())) {
                (true, Some(first)) => {
                    let first_rel = first - win_abs;
                    let from = ((prev_end_abs - win_abs).max(0.0) / ENC_SPF) as usize;
                    let word_frame = (first_rel.max(0.0) / ENC_SPF) as usize;
                    match leading_onset(&speech, from, word_frame) {
                        Some(onset) => {
                            let gap_lo = (onset as f64 * ENC_SPF - RECOVER_MARGIN_S).max(prev_end_abs - win_abs).max(0.0);
                            let gap_hi = first_rel + RECOVER_MARGIN_S;
                            match decode_gap_fillers(
                                &mut model,
                                &pcm,
                                start_sample,
                                (gap_lo, gap_hi),
                                &prompt,
                                &vocab,
                                &ids,
                                &cfg,
                                &filters,
                                &heads,
                                sup,
                                &mut rng,
                                win_abs,
                                &dev,
                            ) {
                                Ok(found) => {
                                    let fillers = recovered_fillers(&found, prev_end_abs, first);
                                    if fillers.is_empty() {
                                        (words, text)
                                    } else {
                                        let lead: Vec<&str> = fillers.iter().filter_map(|w| w["w"].as_str()).collect();
                                        eprintln!("recovered leading filler(s) {lead:?} at {first:.2}s");
                                        let text = format!("{} {}", lead.join(" "), text);
                                        let words = fillers.into_iter().chain(words).collect();
                                        (words, text)
                                    }
                                }
                                Err(e) => {
                                    eprintln!("filler recovery failed at {first:.2}s: {e:#}");
                                    (words, text)
                                }
                            }
                        }
                        None => (words, text),
                    }
                }
                _ => (words, text),
            };
            // Whisper's segment stamps can start before the speech or stop after it. Once the
            // words are timed, the segment spans exactly its first word through its last.
            let (abs_start, abs_end) = match (words.first(), words.last()) {
                (Some(f), Some(l)) => (
                    f["start"].as_f64().unwrap_or(abs_start),
                    l["end"].as_f64().unwrap_or(abs_end),
                ),
                _ => (abs_start, abs_end),
            };
            if let Some(e) = words.last().and_then(|w| w["end"].as_f64()) {
                prev_end_abs = e;
            }
            let line = serde_json::json!({
                "start": r3(abs_start),
                "end": r3(abs_end),
                "text": text,
                "words": words,
                "approx": approx,
                "no_speech": r3(no_speech),
                "avg_logprob": r3(avg),
                "span_db": span.map(r3),
            });
            println!("{line}");
            n_segs += 1;
        }
        std::io::stdout().flush()?;

        let advance = seek_advance_frames(ends_with_ts, last_ts_time, looped, content_frames);
        seek += advance.max(1);
        let p = (seek as f64 / total_frames as f64).min(1.0);
        eprintln!("progress {p:.4}");
    }

    let elapsed = t_decode.elapsed().as_secs_f64();
    eprintln!(
        "done: {n_segs} segments, audio {audio_s:.2}s, decode {elapsed:.2}s, RTF {:.3}",
        elapsed / audio_s.max(1e-9)
    );
    if cli.align {
        eprintln!(
            "alignment {align_s:.2}s ({:.1}% of decode), {align_fail} segment(s) fell back to proportional times",
            100.0 * align_s / elapsed.max(1e-9)
        );
    }
    Ok(())
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------- model files

fn resolve_model(arg: &str) -> Result<PathBuf> {
    let p = Path::new(arg);
    if p.is_dir() {
        return Ok(p.to_path_buf());
    }
    if !KNOWN_SIZES.contains(&arg) {
        bail!("model '{arg}' is neither a directory nor one of {KNOWN_SIZES:?}");
    }
    let home = std::env::var("HOME").context("HOME not set")?;
    let dir = PathBuf::from(home).join(".cache/edith/models").join(format!("whisper-{arg}"));
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let base = format!("https://huggingface.co/openai/whisper-{arg}/resolve/main");
    for f in REQUIRED {
        if !dir.join(f).exists() {
            eprintln!("downloading {base}/{f}");
            if let Err(e) = fetch(&format!("{base}/{f}"), &dir.join(f), false) {
                bail!(
                    "{e:#}\n\nfetch the files manually:\n  mkdir -p {d}\n{cmds}",
                    d = dir.display(),
                    cmds = REQUIRED
                        .iter()
                        .chain(OPTIONAL.iter())
                        .map(|f| format!("  curl -fL -o {d}/{f} {base}/{f}", d = dir.display()))
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
    }
    for f in OPTIONAL {
        if !dir.join(f).exists() {
            let _ = fetch(&format!("{base}/{f}"), &dir.join(f), true);
        }
    }
    Ok(dir)
}

/// Download to a .part file and rename on success. Returns Ok(false) for a missing optional file.
fn fetch(url: &str, dest: &Path, optional: bool) -> Result<bool> {
    let name = dest.file_name().context("bad dest")?.to_string_lossy().into_owned();
    let tmp = dest.with_file_name(format!("{name}.part"));
    match ureq::get(url).call() {
        Ok(resp) => {
            let mut reader = resp.into_reader();
            let mut f = File::create(&tmp)?;
            std::io::copy(&mut reader, &mut f).with_context(|| format!("write {}", tmp.display()))?;
            f.flush()?;
            drop(f);
            fs::rename(&tmp, dest)?;
            Ok(true)
        }
        Err(ureq::Error::Status(404, _)) if optional => Ok(false),
        Err(e) => Err(anyhow!("GET {url}: {e}")),
    }
}

fn load_mel_filters(pc: &serde_json::Value, n_mels: usize) -> Result<Vec<f32>> {
    // HF stores mel_filters as [n_mels][n_freq] (201 = N_FFT/2+1); candle wants the same flat layout.
    let rows = pc["mel_filters"].as_array().context("preprocessor_config has no mel_filters")?;
    if rows.len() != n_mels {
        bail!("mel_filters has {} rows, expected {n_mels}", rows.len());
    }
    let n_freq = m::N_FFT / 2 + 1;
    let mut out = vec![0f32; n_mels * n_freq];
    for (mi, row) in rows.iter().enumerate() {
        let row = row.as_array().context("mel_filters row is not an array")?;
        for (f, v) in row.iter().take(n_freq).enumerate() {
            out[mi * n_freq + f] = v.as_f64().unwrap_or(0.0) as f32;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- audio input

/// Reads WAV (int 8..32 or float 32, any channel count and rate) and returns 16 kHz mono f32.
/// Channels are averaged; non-16 kHz input is resampled by linear interpolation.
fn read_wav_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int if spec.bits_per_sample <= 16 => {
            let scale = 1.0 / 32768.0;
            r.samples::<i16>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()?
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = interleaved
        .chunks(ch)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let src = spec.sample_rate as f64;
    if spec.sample_rate as usize == SR {
        return Ok(mono);
    }
    eprintln!("resampling {} Hz -> {SR} Hz (linear)", spec.sample_rate);
    let out_len = (mono.len() as f64 * SR as f64 / src).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * src / SR as f64;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        let a = mono.get(idx).copied().unwrap_or(0.0);
        let b = mono.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    Ok(out)
}

// ---------------------------------------------------------------- tokenizer

/// Byte-level BPE vocabulary read straight from tokenizer.json (no `tokenizers` crate).
struct Vocab {
    ids: HashMap<String, u32>,
    pieces: Vec<String>,
    byte_of: HashMap<char, u8>,
    /// Inverse of `byte_of`: the byte-level character for each byte value.
    char_of: Vec<char>,
    /// BPE merge ranks: (left, right) symbol pair -> rank, lower merges first.
    merges: HashMap<(String, String), usize>,
}

impl Vocab {
    fn load(path: &Path) -> Result<Self> {
        let v: serde_json::Value = serde_json::from_reader(BufReader::new(
            File::open(path).with_context(|| format!("open {}", path.display()))?,
        ))?;
        let mut ids = HashMap::new();
        for (k, id) in v["model"]["vocab"].as_object().context("tokenizer.json: no model.vocab")? {
            ids.insert(k.clone(), id.as_u64().context("bad vocab id")? as u32);
        }
        for t in v["added_tokens"].as_array().context("tokenizer.json: no added_tokens")? {
            let content = t["content"].as_str().context("bad added token")?;
            ids.insert(content.to_string(), t["id"].as_u64().context("bad added id")? as u32);
        }
        let n = ids.values().max().map_or(0, |m| *m as usize + 1);
        let mut pieces = vec![String::new(); n];
        for (k, &id) in &ids {
            pieces[id as usize] = k.clone();
        }
        // GPT-2 bytes_to_unicode, inverted.
        let mut bs: Vec<u32> = (33..=126).chain(161..=172).chain(174..=255).collect();
        let mut cs = bs.clone();
        let mut extra = 0u32;
        for b in 0..256u32 {
            if !bs.contains(&b) {
                bs.push(b);
                cs.push(256 + extra);
                extra += 1;
            }
        }
        let pairs: Vec<(char, u8)> =
            cs.iter().zip(bs.iter()).map(|(c, b)| (char::from_u32(*c).unwrap(), *b as u8)).collect();
        let byte_of: HashMap<char, u8> = pairs.iter().copied().collect();
        let mut char_of = vec!['?'; 256];
        for (c, b) in &pairs {
            char_of[*b as usize] = *c;
        }
        let mut merges = HashMap::new();
        for (rank, m) in v["model"]["merges"].as_array().context("tokenizer.json: no merges")?.iter().enumerate() {
            let (a, b) = match m {
                serde_json::Value::String(line) => {
                    let mut it = line.splitn(2, ' ');
                    (it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string())
                }
                serde_json::Value::Array(pair) if pair.len() == 2 => {
                    (pair[0].as_str().unwrap_or("").to_string(), pair[1].as_str().unwrap_or("").to_string())
                }
                _ => bail!("tokenizer.json: bad merge entry"),
            };
            merges.entry((a, b)).or_insert(rank);
        }
        Ok(Vocab { ids, pieces, byte_of, char_of, merges })
    }

    fn id(&self, s: &str) -> Result<u32> {
        self.ids.get(s).copied().with_context(|| format!("token {s} not in tokenizer"))
    }

    /// Byte-level BPE encoding of plain text (no special tokens). The pre-tokenizer keeps each
    /// word with its leading space and splits runs of letters/digits from punctuation, which
    /// is close enough to Whisper's GPT-2 regex for a decoding hint.
    fn encode(&self, text: &str) -> Vec<u32> {
        let mut chunks: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_kind = 0u8; // 1 = alphanumeric run, 2 = punctuation run
        for c in text.chars() {
            if c.is_whitespace() {
                if !cur.is_empty() {
                    chunks.push(std::mem::take(&mut cur));
                }
                cur_kind = 0;
                cur.push(' ');
                continue;
            }
            let kind = if c.is_alphanumeric() { 1 } else { 2 };
            if kind != cur_kind && cur_kind != 0 {
                chunks.push(std::mem::take(&mut cur));
            }
            cur.push(c);
            cur_kind = kind;
        }
        if !cur.is_empty() {
            chunks.push(cur);
        }
        let mut out = Vec::new();
        for chunk in chunks {
            let mut syms: Vec<String> = chunk.bytes().map(|b| self.char_of[b as usize].to_string()).collect();
            loop {
                let best = (0..syms.len().saturating_sub(1))
                    .filter_map(|i| {
                        self.merges.get(&(syms[i].clone(), syms[i + 1].clone())).map(|&r| (r, i))
                    })
                    .min();
                let Some((_, i)) = best else { break };
                let merged = format!("{}{}", syms[i], syms[i + 1]);
                syms[i] = merged;
                syms.remove(i + 1);
            }
            out.extend(syms.iter().filter_map(|t| self.ids.get(t).copied()));
        }
        out
    }

    fn piece_bytes(&self, id: u32) -> Vec<u8> {
        match self.pieces.get(id as usize) {
            Some(p) if !p.starts_with("<|") => {
                p.chars().filter_map(|c| self.byte_of.get(&c).copied()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// Text of the given tokens, special/timestamp tokens skipped.
    fn decode(&self, ids: &[u32]) -> String {
        let bytes: Vec<u8> = ids.iter().flat_map(|&i| self.piece_bytes(i)).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// (code, id) for every `<|xx|>` / `<|xxx|>` language token.
    fn lang_tokens(&self) -> Vec<(String, u32)> {
        let mut out: Vec<(String, u32)> = self
            .ids
            .iter()
            .filter_map(|(k, &id)| {
                let inner = k.strip_prefix("<|")?.strip_suffix("|>")?;
                let ok = (2..=3).contains(&inner.len())
                    && inner.chars().all(|c| c.is_ascii_lowercase());
                ok.then(|| (inner.to_string(), id))
            })
            .collect();
        out.sort_by_key(|(_, id)| *id);
        out
    }
}

struct Ids {
    /// `<|startofprev|>`, which opens the previous-text prompt.
    startofprev: u32,
    sot: u32,
    eot: u32,
    ts0: u32,
    transcribe: u32,
    nt: u32,
    /// `<|nospeech|>` in some vocabularies, `<|nocaptions|>` in the multilingual ones.
    no_speech: u32,
}

impl Ids {
    fn new(v: &Vocab) -> Result<Self> {
        let no_speech = v.id("<|nospeech|>").or_else(|_| v.id("<|nocaptions|>"))?;
        Ok(Ids {
            startofprev: v.id("<|startofprev|>")?,
            sot: v.id("<|startoftranscript|>")?,
            eot: v.id("<|endoftext|>")?,
            ts0: v.id("<|0.00|>")?,
            transcribe: v.id("<|transcribe|>")?,
            nt: v.id("<|notimestamps|>")?,
            no_speech,
        })
    }
}

/// Tokens the decoder may not emit. `any` is cleared at every step: for an English transcript,
/// the tokens whose bytes start a CJK, kana, Hangul, Cyrillic, Arabic or Indic character (Whisper
/// makes these up over quiet or musical audio). `first` is cleared at the first step only: the
/// blank tokens, so a window never opens on a space (OpenAI's `suppress_blank`).
struct Suppress {
    any: Vec<u32>,
    first: Vec<u32>,
}

impl Suppress {
    fn new(v: &Vocab, english: bool, ts0: u32) -> Self {
        let mut any = Vec::new();
        let mut first = Vec::new();
        for id in 0..ts0 {
            let b = v.piece_bytes(id);
            if b.is_empty() {
                continue;
            }
            if english && foreign_script_bytes(&b) {
                any.push(id);
            }
            if blank_bytes(&b) {
                first.push(id);
            }
        }
        Suppress { any, first }
    }
}

/// True when a token's bytes start a character outside the Latin script that English uses:
/// lead bytes of Cyrillic, Arabic, the Indic and Thai blocks (U+0400 to U+0FFF), CJK, kana,
/// Hangul and Yi (U+3000 to U+ABFF). Punctuation (U+2000 to U+2BFF, lead byte 0xE2) and the
/// Latin letters with accents (0xC3) are not foreign.
fn foreign_script_bytes(b: &[u8]) -> bool {
    b.iter().any(|&x| matches!(x, 0xD0..=0xD3 | 0xD8..=0xDB | 0xE0 | 0xE3..=0xED))
}

/// True for a token that is only ASCII whitespace (a bare space or newline).
fn blank_bytes(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(u8::is_ascii_whitespace)
}

// ---------------------------------------------------------------- decoding

struct Rng(u64);

impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn detect_language(
    model: &mut Whisper,
    enc: &Tensor,
    vocab: &Vocab,
    ids: &Ids,
    dev: &Device,
) -> Result<(u32, String)> {
    let t = Tensor::new(&[ids.sot][..], dev)?.unsqueeze(0)?;
    let ys = model.decoder.forward(&t, enc, true)?;
    let n = ys.dim(1)?;
    let last = ys.narrow(1, n - 1, 1)?;
    let l: Vec<f32> = model.decoder.final_linear(&last)?.flatten_all()?.to_vec1::<f32>()?;
    let langs = vocab.lang_tokens();
    let (code, id) = langs
        .iter()
        .max_by(|a, b| l[a.1 as usize].total_cmp(&l[b.1 as usize]))
        .context("no language tokens in vocabulary")?;
    Ok((*id, code.clone()))
}

/// Greedy (temp 0) or sampled decoding of one 30 s window with timestamp tokens enabled.
/// Returns generated (token, logprob) pairs, excluding the prompt and the final EOT, and the
/// window's no-speech probability: softmax over the full vocabulary at the start-of-transcript
/// position of the first decode step, before any suppression (as OpenAI whisper computes it).
/// What one decode window hands back: its tokens with their log-probs, the average
/// log-prob, and whether it stopped on the end-of-text token.
type DecodeWindow = (Vec<(u32, f32)>, f64, bool);

#[allow(clippy::too_many_arguments)]
fn decode_window(
    model: &mut Whisper,
    enc: &Tensor,
    prompt: &[u32],
    temp: f64,
    ids: &Ids,
    cfg: &Config,
    sup: &Suppress,
    rng: &mut Rng,
    dev: &Device,
) -> Result<DecodeWindow> {
    const NEG: f32 = f32::NEG_INFINITY;
    let ts0 = ids.ts0 as usize;
    let eot = ids.eot as usize;
    let mut seq: Vec<u32> = prompt.to_vec();
    let mut out: Vec<(u32, f32)> = Vec::new();
    let max_new = cfg.max_target_positions.saturating_sub(prompt.len() + 1);
    let mut run = 0usize;
    let mut no_speech = 0.0f64;
    let mut looped = false;

    for step in 0..max_new {
        // The decoder keeps the keys and values of the tokens so far: after the prompt, a step
        // feeds only the newest token.
        let feed: &[u32] = if step == 0 { seq.as_slice() } else { &seq[seq.len() - 1..] };
        let t = Tensor::new(feed, dev)?.unsqueeze(0)?;
        let ys = model.decoder.forward(&t, enc, step == 0)?;
        let n = ys.dim(1)?;
        let last = ys.narrow(1, n - 1, 1)?;
        let mut l: Vec<f32> = model.decoder.final_linear(&last)?.flatten_all()?.to_vec1::<f32>()?;
        if step == 0 {
            let sot_row = ys.narrow(1, 0, 1)?;
            let raw: Vec<f32> = model.decoder.final_linear(&sot_row)?.flatten_all()?.to_vec1::<f32>()?;
            no_speech = softmax_prob(&raw, ids.no_speech as usize);
        }

        for &s in cfg.suppress_tokens.iter().chain(&sup.any) {
            if let Some(v) = l.get_mut(s as usize) {
                *v = NEG;
            }
        }
        if step == 0 {
            l[eot] = NEG;
            for &s in &sup.first {
                l[s as usize] = NEG;
            }
        }
        // No special/language/task tokens after the prompt, only text, EOT and timestamps.
        for v in &mut l[(eot + 1)..ts0] {
            *v = NEG;
        }
        // Timestamp grammar: pairs of timestamps must be followed by text; a lone
        // timestamp must be followed by a timestamp or EOT.
        let last_is_ts = out.last().is_some_and(|t| t.0 >= ids.ts0);
        // Like Whisper: a lone leading timestamp counts as "penultimate was timestamp".
        let pen_is_ts = out.len() < 2 || out[out.len() - 2].0 >= ids.ts0;
        if last_is_ts {
            if pen_is_ts {
                for v in &mut l[ts0..] {
                    *v = NEG;
                }
            } else {
                for v in &mut l[..eot] {
                    *v = NEG;
                }
            }
        }
        // Timestamps never go backwards.
        if let Some(lt) = out.iter().rev().find(|t| t.0 >= ids.ts0).map(|t| t.0 as usize) {
            for v in &mut l[ts0..lt] {
                *v = NEG;
            }
        }
        // First timestamp at most 1.0 s into the window.
        if out.is_empty() {
            let len = l.len();
            for v in &mut l[(ts0 + 51).min(len)..] {
                *v = NEG;
            }
        }

        let mx = l.iter().copied().filter(|v| v.is_finite()).fold(NEG, f32::max);
        let z: f32 = l.iter().filter(|v| v.is_finite()).map(|v| (v - mx).exp()).sum();
        let lse = mx + z.ln();
        let next: usize = if temp <= 0.0 {
            l.iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i)
                .context("no finite logits")?
        } else {
            let probs: Vec<f32> = l.iter().map(|v| ((v - mx) / temp as f32).exp()).collect();
            let total: f32 = probs.iter().sum();
            let mut r = rng.next_f32() * total;
            let mut pick = probs.len() - 1;
            for (i, p) in probs.iter().enumerate() {
                r -= p;
                if r <= 0.0 {
                    pick = i;
                    break;
                }
            }
            pick
        };
        if next == eot {
            break;
        }
        let lp = l[next] - lse;
        out.push((next as u32, lp));
        seq.push(next as u32);

        run = if out.len() >= 2 && out[out.len() - 2].0 == next as u32 { run + 1 } else { 1 };
        if run >= MAX_REPEAT {
            eprintln!("repetition guard: token {next} repeated {MAX_REPEAT}x, ending window");
            break;
        }
        if let Some(p) = text_loop_period(&out, ids.ts0) {
            eprintln!("repetition guard: a {p}-token phrase repeated {LOOP_REPEATS}x, ending window");
            cut_loop(&mut out, ids.ts0, p);
            looped = true;
            break;
        }
    }
    Ok((out, no_speech, looped))
}

/// True when the window's output must be decoded again at the next temperature: its text
/// compresses too well (a repetition), its tokens are too unsure, or a loop cut left no text.
fn window_rejected(ratio: f64, avg_logprob: f64, looped_without_text: bool) -> bool {
    ratio > MAX_COMPRESSION_RATIO || avg_logprob < MIN_AVG_LOGPROB || looped_without_text
}

/// Whether any text token (not a timestamp) is in `toks`.
fn has_text(toks: &[(u32, f32)], ts0: u32) -> bool {
    toks.iter().any(|t| t.0 < ts0)
}

/// Frames to seek past a decoded window (10 ms frames). Standard long-form seek: jump to the last
/// timestamp when the window ended on one, else consume the whole window. A looped window whose
/// timestamps stop at the start (none after `0.00`) advances only `LOOP_STEP_S`, so the audio after
/// the loop is decoded instead of skipped.
fn seek_advance_frames(ends_with_ts: bool, last_ts_s: Option<f64>, looped: bool, content_frames: usize) -> usize {
    let to_last_ts = match (ends_with_ts, last_ts_s) {
        (true, Some(t)) if t > 0.0 => Some(((t * 100.0).round() as usize).min(content_frames)),
        _ => None,
    };
    match (to_last_ts, looped) {
        (Some(f), _) => f,
        (None, true) => ((LOOP_STEP_S * 100.0).round() as usize).min(content_frames),
        (None, false) => content_frames,
    }
}

/// The period of a phrase that the text tokens of `out` end with, repeated `LOOP_REPEATS` times
/// in a row (timestamps are skipped, so a loop whose copies get new times is still one).
fn text_loop_period(out: &[(u32, f32)], ts0: u32) -> Option<usize> {
    let text: Vec<u32> = out.iter().filter(|t| t.0 < ts0).map(|t| t.0).collect();
    for p in 2..=LOOP_MAX_PERIOD {
        let n = LOOP_REPEATS * p;
        if text.len() < n {
            break;
        }
        let tail = &text[text.len() - n..];
        if (1..LOOP_REPEATS).all(|r| tail[r * p..(r + 1) * p] == tail[..p]) {
            return Some(p);
        }
    }
    None
}

/// Drops the repeats of a loop of period `p` from the end of `out` and ends the window on the last
/// timestamp at or before the loop, so the next window resumes there and the audio after the loop
/// is not skipped. The first copy is kept only when a timestamp follows it. With no timestamp
/// before the loop, the window's text is gone: the caller retries it, then steps forward briefly.
fn cut_loop(out: &mut Vec<(u32, f32)>, ts0: u32, p: usize) {
    let text_at: Vec<usize> = out.iter().enumerate().filter(|(_, t)| t.0 < ts0).map(|(i, _)| i).collect();
    let first_repeat = text_at[text_at.len() - (LOOP_REPEATS - 1) * p];
    out.truncate(first_repeat);
    if let Some(i) = out.iter().rposition(|t| t.0 >= ts0) {
        out.truncate(i + 1);
    }
}

/// Probability of `idx` under a softmax over `logits`. Returns 0 for an out-of-range index.
fn softmax_prob(logits: &[f32], idx: usize) -> f64 {
    let Some(&target) = logits.get(idx) else { return 0.0 };
    let mx = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max) as f64;
    let z: f64 = logits.iter().map(|&v| (v as f64 - mx).exp()).sum();
    (target as f64 - mx).exp() / z
}

fn compression_ratio(text: &str) -> Result<f64> {
    use flate2::{write::ZlibEncoder, Compression};
    if text.is_empty() {
        return Ok(0.0);
    }
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(text.as_bytes())?;
    let packed = enc.finish()?;
    Ok(text.len() as f64 / packed.len() as f64)
}

fn avg_logprob(toks: &[(u32, f32)]) -> f64 {
    if toks.is_empty() {
        return 0.0;
    }
    toks.iter().map(|t| t.1 as f64).sum::<f64>() / toks.len() as f64
}

struct Seg {
    /// Seconds from the window start.
    start: f64,
    end: f64,
    toks: Vec<(u32, f32)>,
}

/// Splits a window's tokens into segments on timestamp tokens. A segment is the text
/// between two timestamps; text left after the last timestamp ends at `content_s`.
fn split_segments(toks: &[(u32, f32)], ts0: u32, content_s: f64) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut start: Option<f64> = None;
    let mut buf: Vec<(u32, f32)> = Vec::new();
    for &(id, lp) in toks {
        if id >= ts0 {
            let t = (id - ts0) as f64 * SEC_PER_TS;
            match start {
                None => {
                    if !buf.is_empty() {
                        segs.push(Seg { start: 0.0, end: t, toks: std::mem::take(&mut buf) });
                    }
                    start = Some(t);
                }
                Some(s) => {
                    if !buf.is_empty() {
                        segs.push(Seg { start: s, end: t, toks: std::mem::take(&mut buf) });
                    }
                    start = Some(t);
                }
            }
        } else {
            buf.push((id, lp));
        }
    }
    if !buf.is_empty() {
        let s = start.unwrap_or(0.0);
        segs.push(Seg { start: s, end: content_s.max(s), toks: buf });
    }
    segs
}

/// Word timing approximation: the segment span divided across its words in proportion to
/// character count. Word probability is the geometric mean of its token probabilities.
fn word_times(vocab: &Vocab, toks: &[(u32, f32)], start: f64, end: f64) -> Vec<serde_json::Value> {
    // Group tokens into words: a token whose bytes start with a space opens a new word.
    let mut words: Vec<(Vec<u8>, Vec<f32>)> = Vec::new();
    for &(id, lp) in toks {
        let b = vocab.piece_bytes(id);
        if b.is_empty() {
            continue;
        }
        if b[0] == b' ' && words.last().is_some_and(|w| !w.0.is_empty()) {
            words.push((Vec::new(), Vec::new()));
        }
        if words.is_empty() {
            words.push((Vec::new(), Vec::new()));
        }
        let w = words.last_mut().unwrap();
        w.0.extend_from_slice(&b);
        w.1.push(lp);
    }
    let named: Vec<(String, f64)> = words
        .iter()
        .map(|(b, lps)| {
            let w = String::from_utf8_lossy(b).trim().to_string();
            let p = if lps.is_empty() {
                0.0
            } else {
                (lps.iter().map(|&x| x as f64).sum::<f64>() / lps.len() as f64).exp()
            };
            (w, p)
        })
        .filter(|(w, _)| !w.is_empty())
        .collect();
    let total_chars: usize = named.iter().map(|(w, _)| w.chars().count()).sum();
    let span = (end - start).max(0.0);
    let mut t = start;
    named
        .iter()
        .map(|(w, p)| {
            let chars = w.chars().count();
            let dur = if total_chars > 0 { span * chars as f64 / total_chars as f64 } else { 0.0 };
            let (ws, we) = (t, t + dur);
            t = we;
            serde_json::json!({"w": w, "start": r3(ws), "end": r3(we), "p": r3(*p)})
        })
        .collect()
}

// ---------------------------------------------------------------- word alignment

/// Audio (encoder) frame duration: 20 ms, twice the mel hop.
const ENC_SPF: f64 = 0.02;
/// Median filter width over audio frames (whisper's `medfilt_width`).
const MEDFILT: usize = 7;

/// Heads whose cross-attention is used for alignment: `generation_config.json`'s
/// `alignment_heads`, or every head of the last half of the decoder layers when absent.
fn alignment_heads(dir: &Path, cfg: &Config) -> Result<Vec<(usize, usize)>> {
    let p = dir.join("generation_config.json");
    if p.exists() {
        let v: serde_json::Value = serde_json::from_reader(BufReader::new(File::open(&p)?))?;
        if let Some(arr) = v["alignment_heads"].as_array() {
            let mut out = Vec::with_capacity(arr.len());
            for h in arr {
                let l = h[0].as_u64().context("alignment_heads: bad layer")? as usize;
                let hd = h[1].as_u64().context("alignment_heads: bad head")? as usize;
                out.push((l, hd));
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }
    let first = cfg.decoder_layers / 2;
    Ok((first..cfg.decoder_layers)
        .flat_map(|l| (0..cfg.decoder_attention_heads).map(move |h| (l, h)))
        .collect())
}

#[derive(Default)]
struct WordG {
    text: Vec<u8>,
    /// Indices into the segment's token list.
    toks: Vec<usize>,
    lps: Vec<f32>,
}

fn is_punct_piece(s: &str) -> bool {
    let t = s.trim();
    let mut cs = t.chars();
    matches!((cs.next(), cs.next()), (Some(c), None) if c.is_ascii_punctuation())
}

fn is_sentence_end(text: &[u8]) -> bool {
    let t = String::from_utf8_lossy(text);
    let t = t.trim();
    t.chars().count() == 1 && ".。!！?？".contains(t)
}

/// Whisper's `split_tokens_on_spaces` + `merge_punctuations`: a word opens at a token that
/// starts with a space or is a lone punctuation mark; prepend-punctuation joins the following
/// word and append-punctuation joins the previous one.
fn group_words(vocab: &Vocab, toks: &[(u32, f32)]) -> Vec<WordG> {
    const PREPEND: &str = "\"'“¿([{-";
    const APPEND: &str = "\"'.。,，!！?？:：”)]}、";
    let mut words: Vec<WordG> = Vec::new();
    for (i, &(id, lp)) in toks.iter().enumerate() {
        let b = vocab.piece_bytes(id);
        let opens = match b.first() {
            Some(&c) => c == b' ' || is_punct_piece(&String::from_utf8_lossy(&b)),
            None => false,
        };
        if words.is_empty() || opens {
            words.push(WordG::default());
        }
        let w = words.last_mut().unwrap();
        w.text.extend_from_slice(&b);
        w.toks.push(i);
        w.lps.push(lp);
    }
    let n = words.len();
    // Prepended punctuation: merge into the following word (walking backwards).
    if n >= 2 {
        let mut j = n - 1;
        for i in (0..n - 1).rev() {
            let prev = String::from_utf8_lossy(&words[i].text).into_owned();
            if prev.starts_with(' ') && !prev.trim().is_empty() && PREPEND.contains(prev.trim()) {
                let moved = std::mem::take(&mut words[i]);
                let f = &mut words[j];
                let mut t = moved.text;
                t.extend_from_slice(&f.text);
                f.text = t;
                let mut tk = moved.toks;
                tk.extend_from_slice(&f.toks);
                f.toks = tk;
                let mut lp = moved.lps;
                lp.extend_from_slice(&f.lps);
                f.lps = lp;
            } else {
                j = i;
            }
        }
    }
    // Appended punctuation: merge into the previous word (walking forwards).
    let mut i = 0usize;
    for j in 1..words.len() {
        let foll = String::from_utf8_lossy(&words[j].text).into_owned();
        let prev_ends_space = words[i].text.last() == Some(&b' ');
        if !words[j].text.is_empty() && !prev_ends_space && APPEND.contains(foll.as_str()) {
            let moved = std::mem::take(&mut words[j]);
            let p = &mut words[i];
            p.text.extend_from_slice(&moved.text);
            p.toks.extend_from_slice(&moved.toks);
            p.lps.extend_from_slice(&moved.lps);
        } else {
            i = j;
        }
    }
    words.retain(|w| !w.text.is_empty());
    words
}

/// Whisper's `dtw` (cost matrix rows x cols, row-major). Returns the monotone path as
/// parallel (row, col) index lists from (0,0) to (rows-1, cols-1).
fn dtw_path(cost: &[f32], rows: usize, cols: usize) -> (Vec<usize>, Vec<usize>) {
    let w = cols + 1;
    let mut c = vec![f64::INFINITY; (rows + 1) * w];
    let mut tr = vec![0u8; (rows + 1) * w];
    c[0] = 0.0;
    for j in 1..=cols {
        for i in 1..=rows {
            let c0 = c[(i - 1) * w + j - 1];
            let c1 = c[(i - 1) * w + j];
            let c2 = c[i * w + j - 1];
            let (best, t) = if c0 < c1 && c0 < c2 {
                (c0, 0u8)
            } else if c1 < c0 && c1 < c2 {
                (c1, 1)
            } else {
                (c2, 2)
            };
            c[i * w + j] = cost[(i - 1) * cols + (j - 1)] as f64 + best;
            tr[i * w + j] = t;
        }
    }
    tr[..=cols].fill(2);
    for i in 0..=rows {
        tr[i * w] = 1;
    }
    let (mut i, mut j) = (rows, cols);
    let mut path = Vec::with_capacity(rows + cols);
    while i > 0 || j > 0 {
        path.push((i - 1, j - 1));
        match tr[i * w + j] {
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
    }
    path.reverse();
    path.into_iter().unzip()
}

/// Median over a reflect-padded window (whisper's `median_filter`).
fn median_filter(x: &[f32], width: usize) -> Vec<f32> {
    let n = x.len();
    let pad = (width / 2) as isize;
    let refl = |i: isize| -> usize {
        if n == 1 {
            return 0;
        }
        let period = 2 * (n as isize - 1);
        let mut j = i.rem_euclid(period);
        if j >= n as isize {
            j = period - j;
        }
        j as usize
    };
    (0..n)
        .map(|i| {
            let mut win: Vec<f32> = (0..width)
                .map(|k| x[refl(i as isize + k as isize - pad)])
                .collect();
            win.sort_by(f32::total_cmp);
            win[width / 2]
        })
        .collect()
}

fn median_f64(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Speech band edges in Hz: narration energy lives here, room tone and most beds sit below.
const BAND_LO_HZ: f64 = 300.0;
const BAND_HI_HZ: f64 = 3400.0;
/// A frame is speech when its band energy is this far above the rolling noise floor.
const SPEECH_MARGIN_DB: f64 = 10.0;
/// A segment span is silent when its band energy is less than this above the noise floor.
const SILENT_REL_DB: f64 = 6.0;
/// The noise floor is the mean of the quietest `FLOOR_SHARE` of the frames within
/// `FLOOR_HALF_FRAMES` either side of a frame (a 3 s rolling window).
const FLOOR_SHARE: f64 = 0.10;
const FLOOR_HALF_FRAMES: usize = 75;
/// Attention context added to a segment before and after it for DTW, in seconds. The left margin
/// is zero: the DTW start is pinned to the crop's first frame, and a margin there reaches into the
/// previous segment's words. The crop is then trimmed to the speech it holds.
const ALIGN_PAD_LEFT_S: f64 = 0.0;
const ALIGN_PAD_RIGHT_S: f64 = 0.5;
/// Samples per 20 ms audio frame.
const ENC_HOP: usize = 320;
/// Segments and words starting this close to the audio end, or later, are dropped.
const END_GUARD_S: f64 = 0.1;

/// Second-order section (RBJ cookbook), direct form I.
#[derive(Clone, Copy)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
}

impl Biquad {
    fn new(fc: f64, q: f64, high: bool) -> Self {
        let w = 2.0 * std::f64::consts::PI * fc / SR as f64;
        let (s, c) = (w.sin(), w.cos());
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b = if high {
            [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
        } else {
            [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
        };
        Biquad { b: [b[0] / a0, b[1] / a0, b[2] / a0], a: [-2.0 * c / a0, (1.0 - alpha) / a0] }
    }

    fn apply(&self, x: &mut [f64]) {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in x.iter_mut() {
            let y = self.b[0] * *v + self.b[1] * x1 + self.b[2] * x2 - self.a[0] * y1 - self.a[1] * y2;
            x2 = x1;
            x1 = *v;
            y2 = y1;
            y1 = y;
            *v = y;
        }
    }
}

/// The window's samples band-limited to the speech band: a 4th-order Butterworth band run
/// forward and then backward, so the band has no filter delay.
fn speech_band(pcm: &[f32]) -> Vec<f64> {
    const Q: [f64; 2] = [0.5412, 1.3066];
    let sections: Vec<Biquad> = Q
        .iter()
        .map(|&q| Biquad::new(BAND_LO_HZ, q, true))
        .chain(Q.iter().map(|&q| Biquad::new(BAND_HI_HZ, q, false)))
        .collect();
    let mut y: Vec<f64> = pcm.iter().map(|&v| v as f64).collect();
    for _ in 0..2 {
        for bq in &sections {
            bq.apply(&mut y);
        }
        y.reverse();
    }
    y
}

/// Band-limited level of each 20 ms frame of a window, and its rolling noise floor. Both in dB.
struct Levels {
    db: Vec<f64>,
    floor: Vec<f64>,
}

fn levels(pcm: &[f32]) -> Levels {
    let y = speech_band(pcm);
    let db: Vec<f64> = y.chunks(ENC_HOP).map(power_db).collect();
    let floor = (0..db.len())
        .map(|j| {
            let a = j.saturating_sub(FLOOR_HALF_FRAMES);
            let b = (j + FLOOR_HALF_FRAMES + 1).min(db.len());
            quiet_floor_db(&db[a..b])
        })
        .collect();
    Levels { db, floor }
}

/// Mean power of a block of samples in dB (full scale = 1.0).
fn power_db(c: &[f64]) -> f64 {
    let ms = c.iter().map(|x| x * x).sum::<f64>() / c.len().max(1) as f64;
    10.0 * (ms + 1e-12).log10()
}

/// Noise floor of a stretch of frame levels: the mean of its quietest `FLOOR_SHARE`, in dB.
fn quiet_floor_db(levels_db: &[f64]) -> f64 {
    if levels_db.is_empty() {
        return -120.0;
    }
    let mut s = levels_db.to_vec();
    s.sort_by(f64::total_cmp);
    let k = ((s.len() as f64 * FLOOR_SHARE).ceil() as usize).clamp(1, s.len());
    s[..k].iter().sum::<f64>() / k as f64
}

/// One flag per 20 ms frame of a window: true when the frame is speech-band louder than the
/// rolling noise floor by `SPEECH_MARGIN_DB`. A bed that is quiet in the speech band never counts.
fn speech_frames(lv: &Levels) -> Vec<bool> {
    lv.db.iter().zip(&lv.floor).map(|(d, f)| d - f > SPEECH_MARGIN_DB).collect()
}

/// Level of a span in dB above the noise floor (mean power over the span against the mean floor
/// over the same frames), or None when the span holds no frames. Span times are window-relative.
fn span_rel_db(lv: &Levels, start_s: f64, end_s: f64) -> Option<f64> {
    let a = ((start_s.max(0.0) / ENC_SPF).floor() as usize).min(lv.db.len());
    let b = ((end_s.max(0.0) / ENC_SPF).ceil() as usize).min(lv.db.len());
    if b <= a {
        return None;
    }
    let n = (b - a) as f64;
    let mean_power = lv.db[a..b].iter().map(|d| 10f64.powf(d / 10.0)).sum::<f64>() / n;
    let floor = lv.floor[a..b].iter().sum::<f64>() / n;
    Some(10.0 * mean_power.max(1e-30).log10() - floor)
}

/// Segment-level hallucination gate, after OpenAI whisper's no-speech rule plus an energy gate.
/// Returns the reason to drop the segment, or None to keep it.
/// - `no_speech > 0.6` and `avg_logprob < -1.0`: the model says silence and is not confident.
/// - the segment's span is not `SILENT_REL_DB` above the noise floor: nothing audible to transcribe.
fn silence_reason(no_speech: f64, avg_logprob: f64, span_rel: Option<f64>) -> Option<&'static str> {
    if no_speech > NO_SPEECH_PROB && avg_logprob < NO_SPEECH_LOGPROB {
        return Some("no_speech");
    }
    if span_rel.is_some_and(|d| d < SILENT_REL_DB) {
        return Some("silent_span");
    }
    None
}

/// True when a time (seconds in the audio file, no CLI offset) is at or past the audio end.
fn starts_past_end(t: f64, audio_s: f64) -> bool {
    t >= audio_s - END_GUARD_S
}

/// Cuts the words of a segment that start at or past the audio end (`starts_past_end`), keeping
/// the rest, and clamps the ends of the kept words to the audio end. `offset` is the CLI offset
/// the word times carry. Returns how many words were cut.
fn fit_words_to_audio(words: &mut Vec<serde_json::Value>, offset: f64, audio_s: f64) -> usize {
    let before = words.len();
    words.retain(|w| w["start"].as_f64().is_none_or(|s| !starts_past_end(s - offset, audio_s)));
    let audio_end_abs = audio_s + offset;
    for w in words.iter_mut() {
        if w["end"].as_f64().is_some_and(|e| e > audio_end_abs) {
            w["end"] = serde_json::json!(r3(audio_end_abs));
        }
    }
    before - words.len()
}

/// The 20 ms frames `[f0, f1)` of a window that a segment's DTW may use: the segment's span plus
/// `left_s` before and `right_s` after, clamped to the `n` frames of real audio. Always at least one frame.
fn crop_frames(start_s: f64, end_s: f64, n: usize, left_s: f64, right_s: f64) -> (usize, usize) {
    let n = n.max(1);
    let f0 = ((((start_s - left_s).max(0.0)) / ENC_SPF).floor() as usize).min(n - 1);
    let f1 = (((end_s + right_s) / ENC_SPF).ceil() as usize).clamp(f0 + 1, n);
    (f0, f1)
}

/// Trims a crop to the first and last speech frame inside it, so DTW is pinned to speech rather
/// than to the silence the crop margin adds. A crop with no speech is returned unchanged.
fn trim_crop_to_speech(crop: (usize, usize), speech: &[bool]) -> (usize, usize) {
    let (f0, f1) = crop;
    let inside: Vec<usize> = (f0..f1.min(speech.len())).filter(|&j| speech[j]).collect();
    match (inside.first(), inside.last()) {
        (Some(&a), Some(&b)) => (a, b + 1),
        _ => crop,
    }
}

/// DTW pins the first token row to the crop's first frame, and a row whose attention sits at that
/// frame can tie with its neighbour, which makes a word of zero length. Each run of tied starts
/// is spread evenly between its left anchor (the start before it, or the pinned first start) and
/// the next start (or `end_s` for a run at the end). The first entry of a run at index 0 is kept.
fn spread_tied_starts(starts: &mut [f64], end_s: f64) {
    let n = starts.len();
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && starts[j + 1] <= starts[i] + 1e-9 {
            j += 1;
        }
        if j > i {
            let m = j - i + 1;
            let v = starts[i];
            if i == 0 {
                let right = if j + 1 < n { starts[j + 1] } else { end_s.max(v) };
                for (k, s) in starts[1..=j].iter_mut().enumerate() {
                    *s = v + (right - v) * (k + 1) as f64 / m as f64;
                }
            } else {
                let left = starts[i - 1];
                let right = if j + 1 < n { starts[j + 1] } else { end_s.max(v) };
                for (k, s) in starts[i..=j].iter_mut().enumerate() {
                    *s = left + (right - left) * (k + 1) as f64 / (m + 1) as f64;
                }
            }
        }
        i = j + 1;
    }
}

/// Z-scores each column of `m` over its rows, as OpenAI whisper does over the token axis.
fn zscore_columns(m: &mut [Vec<f32>]) {
    let rows = m.len();
    if rows == 0 {
        return;
    }
    for f in 0..m[0].len() {
        let mean = m.iter().map(|r| r[f] as f64).sum::<f64>() / rows as f64;
        let var = m.iter().map(|r| (r[f] as f64 - mean).powi(2)).sum::<f64>() / rows as f64;
        let sd = var.sqrt().max(1e-12);
        for r in m.iter_mut() {
            r[f] = ((r[f] as f64 - mean) / sd) as f32;
        }
    }
}

/// Word timestamps for one segment, following OpenAI whisper's `find_alignment` and
/// `add_word_timestamps`. One teacher-forced decoder pass over
/// [prompt, <|notimestamps|>, text, <|endoftext|>] gives the cross-attention of the alignment
/// heads. DTW runs over the crop `crop = [f0, f1)` of the window (the segment plus a margin),
/// not the whole window: the path is pinned to the crop's first and last frame, so a DTW over
/// the whole window stretches a later segment's words across the audio before it and they
/// collapse. `n_content` is the number of 20 ms frames of real audio in the window, `base` the
/// absolute time of the window start (with the CLI offset). Returned times are absolute.
#[allow(clippy::too_many_arguments)]
fn aligned_words(
    model: &mut Whisper,
    enc: &Tensor,
    prompt: &[u32],
    toks: &[(u32, f32)],
    vocab: &Vocab,
    ids: &Ids,
    heads: &[(usize, usize)],
    crop: (usize, usize),
    n_content: usize,
    base: f64,
    speech: &[bool],
    dev: &Device,
) -> Result<Vec<serde_json::Value>> {
    let n = toks.len();
    let (f0, f1) = crop;
    if n == 0 || heads.is_empty() || f0 >= f1 || f1 > n_content {
        bail!("nothing to align");
    }
    let width = f1 - f0;
    let mut seq: Vec<u32> = prompt.to_vec();
    seq.push(ids.nt);
    seq.extend(toks.iter().map(|t| t.0));
    seq.push(ids.eot);
    let (p, l) = (prompt.len(), seq.len());
    let t = Tensor::new(seq.as_slice(), dev)?.unsqueeze(0)?;
    let (_, caps) = model.decoder.forward_with_cross_attn(&t, enc, true)?;

    // Rows [p, l-1): the <|notimestamps|> row, then one per text token. Row j is the start of
    // token j and row n the end of the segment. Each head's weights are softmaxed over the
    // window's real frames, cropped to [f0, f1), z-scored over the rows and median-filtered over
    // time; the heads are then averaged.
    let rows = l - 1 - p;
    let mut acc = vec![0f32; rows * width];
    for &(layer, head) in heads {
        let c = caps.get(layer).context("alignment head: layer out of range")?;
        let (_, nh, _, nf) = c.dims4()?;
        if head >= nh || n_content > nf {
            bail!("alignment head ({layer},{head}) out of range");
        }
        let full: Vec<Vec<f32>> = c.i((0, head, p..l - 1, ..n_content))?.to_vec2::<f32>()?;
        let mut m: Vec<Vec<f32>> = full
            .iter()
            .map(|row| {
                let s = row.iter().sum::<f32>().max(1e-12);
                row[f0..f1].iter().map(|v| v / s).collect()
            })
            .collect();
        zscore_columns(&mut m);
        for (r, row) in m.iter().enumerate() {
            let med = median_filter(row, MEDFILT);
            for f in 0..width {
                acc[r * width + f] += med[f] / heads.len() as f32;
            }
        }
    }

    let cost: Vec<f32> = acc.iter().map(|v| -v).collect();
    let (ti, tj) = dtw_path(&cost, rows, width);
    let mut first: Vec<Option<usize>> = vec![None; rows];
    for (&r, &f) in ti.iter().zip(tj.iter()) {
        if first[r].is_none() {
            first[r] = Some(f);
        }
    }
    // Seconds from the window start: crop column j is window frame f0 + j.
    let mut starts: Vec<f64> = first
        .iter()
        .map(|f| f.map(|f| (f0 + f) as f64 * ENC_SPF))
        .collect::<Option<Vec<f64>>>()
        .context("DTW path skipped a token")?;
    spread_tied_starts(&mut starts, f1 as f64 * ENC_SPF);

    let words = group_words(vocab, toks);
    let k = words.len();
    if k == 0 {
        bail!("no words in segment");
    }
    let mut ws: Vec<f64> = words.iter().map(|w| starts[w.toks[0]]).collect();
    let mut we: Vec<f64> = (0..k)
        .map(|i| if i + 1 < k { starts[words[i + 1].toks[0]] } else { starts[n] })
        .collect();

    // Whisper's long-word heuristic: cap at 2x the median word duration (median capped at 0.7 s).
    let mut durs: Vec<f64> = (0..k).map(|i| we[i] - ws[i]).filter(|d| *d > 0.0).collect();
    if !durs.is_empty() {
        let max_d = median_f64(&mut durs).min(0.7) * 2.0;
        for i in 1..k {
            if we[i] - ws[i] > max_d {
                if is_sentence_end(&words[i].text) {
                    we[i] = ws[i] + max_d;
                } else if is_sentence_end(&words[i - 1].text) {
                    ws[i] = we[i] - max_d;
                }
            }
        }
        // The DTW path is pinned to the crop's first and last frame, so edge words absorb
        // silence: cap them too.
        if we[0] - ws[0] > max_d {
            ws[0] = we[0] - max_d;
        }
        if we[k - 1] - ws[k - 1] > max_d {
            we[k - 1] = ws[k - 1] + max_d;
        }
    }

    // Words stay inside the crop, which lies inside the audio.
    let lo = base + f0 as f64 * ENC_SPF;
    let hi = base + f1 as f64 * ENC_SPF;
    let mut s: Vec<f64> = ws.iter().map(|&x| (base + x).clamp(lo, hi)).collect();
    let mut e: Vec<f64> = we.iter().map(|&x| (base + x).clamp(lo, hi)).collect();
    // Snap edges onto speech frames. DTW spreads a word over the silence before it, so a start
    // moves to the first speech frame inside the word's span and an end to the end of the last
    // speech frame inside it.
    let frame_of = |t: f64| ((t - base) / ENC_SPF).round().max(0.0) as usize;
    let audible = |j: usize| speech.get(j).copied().unwrap_or(false);
    for i in 0..k {
        let (fs, fe) = (frame_of(s[i]), frame_of(e[i]));
        if let Some(j) = (fs..=fe).find(|&j| audible(j)) {
            if j > fs {
                s[i] = (base + j as f64 * ENC_SPF).min(e[i]);
            }
        }
        let fs = frame_of(s[i]);
        if let Some(j) = (fs..=fe).rev().find(|&j| audible(j)) {
            if j + 1 < fe {
                e[i] = (base + (j + 1) as f64 * ENC_SPF).max(s[i]);
            }
        }
    }
    if repair_spans(&mut s, &mut e, lo, hi) {
        bail!("degenerate word spans: DTW collapsed words onto one another");
    }

    let out: Vec<serde_json::Value> = words
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let text = String::from_utf8_lossy(&w.text).trim().to_string();
            if text.is_empty() {
                return None;
            }
            let p = (w.lps.iter().map(|&x| x as f64).sum::<f64>() / w.lps.len() as f64).exp();
            Some(serde_json::json!({"w": text, "start": r3(s[i]), "end": r3(e[i]), "p": r3(p)}))
        })
        .collect();
    if out.is_empty() {
        bail!("alignment produced no words");
    }
    Ok(out)
}

/// Shortest word the output may carry: two encoder frames (40 ms).
const MIN_WORD_S: f64 = 2.0 * ENC_SPF;

/// Makes word spans monotone, non-overlapping and at least `MIN_WORD_S` long, inside
/// `[lo, hi]` where the audio allows. Returns true when the alignment is degenerate, i.e. a
/// word had collapsed to a point or overlapped its neighbour by more than a frame, or the words
/// could not be fitted into the segment. The spans are still repaired in that case, but the
/// caller should not trust them.
fn repair_spans(s: &mut [f64], e: &mut [f64], lo: f64, hi: f64) -> bool {
    let n = s.len();
    let mut degenerate = false;
    let mut prev_end = lo;
    for i in 0..n {
        if e[i] - s[i] < 1e-3 {
            degenerate = true;
        }
        if s[i] < prev_end - ENC_SPF {
            degenerate = true;
        }
        s[i] = s[i].max(prev_end);
        e[i] = e[i].max(s[i] + MIN_WORD_S);
        prev_end = e[i];
    }
    if n > 0 && e[n - 1] > hi {
        // The words ran past the segment: pull them back from the end, keeping the minimum.
        let mut limit = hi;
        for i in (0..n).rev() {
            e[i] = e[i].min(limit);
            s[i] = s[i].min(e[i] - MIN_WORD_S);
            limit = s[i];
        }
        if s[0] < lo - ENC_SPF {
            degenerate = true;
        }
    }
    degenerate
}

/// Speech that must come before a segment's first word, after the previous word, for the gap to
/// be re-decoded for a leading filler. Shorter gaps are breaths and onsets, not a filler.
const ONSET_MIN_S: f64 = 0.25;
/// Audio kept on each side of a re-decoded gap, in seconds: the margin Whisper needs to hear the
/// gap's edges as edges.
const RECOVER_MARGIN_S: f64 = 0.3;
/// Words a leading gap may carry and still be a filler: the spellings Whisper gives those sounds.
const FILLER_WORDS: &[&str] = &["um", "uh", "er", "ah", "hmm", "hm", "mm", "uhm", "umm", "uhh", "erm"];

/// The first speech frame in `[from, word)` (frames of 20 ms, window-relative) when the speech
/// before the word, from that frame on, runs longer than `ONSET_MIN_S`. None when the gap before
/// the word holds no speech that long: the word is the first thing said since `from`.
fn leading_onset(speech: &[bool], from: usize, word: usize) -> Option<usize> {
    let end = word.min(speech.len());
    let first = (from..end).find(|&j| speech[j])?;
    ((end - first) as f64 * ENC_SPF > ONSET_MIN_S).then_some(first)
}

/// True when a word is a filler sound, ignoring case and punctuation ("Um," is "um").
fn is_filler_word(w: &str) -> bool {
    let norm: String = w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase();
    FILLER_WORDS.contains(&norm.as_str())
}

/// The filler words a re-decoded gap found that belong in front of a segment: fillers that start
/// at or after `min_s` (the previous word's end, so nothing is taken twice) and clearly before the
/// segment's first word `first_s`. A filler at the word's own start is the same sound, not a
/// second one, and is left to the segment.
fn recovered_fillers(words: &[serde_json::Value], min_s: f64, first_s: f64) -> Vec<serde_json::Value> {
    words
        .iter()
        .filter(|w| {
            let (Some(s), Some(t)) = (w["start"].as_f64(), w["w"].as_str()) else { return false };
            is_filler_word(t) && s >= min_s && s < first_s - 0.05
        })
        .cloned()
        .collect()
}

/// Re-decodes the gap `[gap_s.0, gap_s.1]` (window-relative seconds, margins included) on its own
/// and returns the filler words it holds, timed with DTW. `base` is the absolute time of the
/// window start (with the CLI offset). The gap is decoded with the same `prompt` as the window,
/// so the filler hint is in force, but only the filler words are kept.
#[allow(clippy::too_many_arguments)]
fn decode_gap_fillers(
    model: &mut Whisper,
    pcm: &[f32],
    win_start_sample: usize,
    gap_s: (f64, f64),
    prompt: &[u32],
    vocab: &Vocab,
    ids: &Ids,
    cfg: &Config,
    filters: &[f32],
    heads: &[(usize, usize)],
    sup: &Suppress,
    rng: &mut Rng,
    base: f64,
    dev: &Device,
) -> Result<Vec<serde_json::Value>> {
    let a = win_start_sample + (gap_s.0 * SR as f64) as usize;
    let b = (win_start_sample + (gap_s.1 * SR as f64) as usize).min(pcm.len());
    if b <= a + SR / 10 {
        return Ok(Vec::new());
    }
    let stretch = &pcm[a..b];
    let mut win = vec![0f32; WINDOW];
    win[..stretch.len()].copy_from_slice(stretch);
    let mel = m::audio::pcm_to_mel(cfg, &win, filters);
    let n_frames = mel.len() / cfg.num_mel_bins;
    let mel_t = Tensor::from_vec(mel, (1, cfg.num_mel_bins, n_frames), dev)?.narrow(2, 0, WINDOW / FRAME)?;
    let enc = model.encoder.forward(&mel_t, true)?;
    let n_content = stretch.len().div_ceil(ENC_HOP);
    let speech = speech_frames(&levels(stretch));
    let crop = trim_crop_to_speech((0, n_content), &speech);
    let base = base + (a - win_start_sample) as f64 / SR as f64;

    let (toks, _, _) = decode_window(model, &enc, prompt, 0.0, ids, cfg, sup, rng, dev)?;
    let content_s = stretch.len() as f64 / SR as f64;
    let mut out = Vec::new();
    for seg in split_segments(&toks, ids.ts0, content_s) {
        // A gap that holds no alignable words, or no DTW path, has no fillers to keep.
        if let Ok(words) = aligned_words(model, &enc, prompt, &seg.toks, vocab, ids, heads, crop, n_content, base, &speech, dev) {
            out.extend(words.into_iter().filter(|w| w["w"].as_str().is_some_and(is_filler_word)));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_split_on_timestamp_pairs() {
        let ts0 = 1000;
        // ts 0.00, text(10,11), ts 1.00, ts 1.00, text(12), ts 2.00
        let toks = vec![(ts0, 0.0), (10, -0.1), (11, -0.1), (ts0 + 50, 0.0), (ts0 + 50, 0.0), (12, -0.2), (ts0 + 100, 0.0)];
        let segs = split_segments(&toks, ts0, 30.0);
        assert_eq!(segs.len(), 2);
        assert!((segs[0].start - 0.0).abs() < 1e-9 && (segs[0].end - 1.0).abs() < 1e-9);
        assert!((segs[1].start - 1.0).abs() < 1e-9 && (segs[1].end - 2.0).abs() < 1e-9);
    }

    #[test]
    fn leftover_text_ends_at_content() {
        let ts0 = 1000;
        let toks = vec![(ts0, 0.0), (10, -0.1), (11, -0.1)];
        let segs = split_segments(&toks, ts0, 12.5);
        assert_eq!(segs.len(), 1);
        assert!((segs[0].end - 12.5).abs() < 1e-9);
    }

    #[test]
    fn repaired_spans_are_monotone_and_never_zero_length() {
        // The shape the DTW produced on the dog3 narration: "You" collapsed onto the segment
        // start, "describe" overlapping it.
        let mut s = vec![5.24, 5.24, 5.62];
        let mut e = vec![5.24, 5.62, 6.0];
        assert!(repair_spans(&mut s, &mut e, 5.24, 6.0), "a collapsed word is degenerate");
        for i in 0..3 {
            assert!(e[i] - s[i] >= MIN_WORD_S - 1e-9, "word {i}: {} to {}", s[i], e[i]);
            if i > 0 {
                assert!(s[i] >= e[i - 1] - 1e-9, "word {i} overlaps the one before");
            }
        }
    }

    #[test]
    fn clean_spans_are_not_degenerate_and_stay_put() {
        let mut s = vec![0.0, 0.14, 0.2];
        let mut e = vec![0.14, 0.2, 0.6];
        assert!(!repair_spans(&mut s, &mut e, 0.0, 0.6));
        assert_eq!(s, vec![0.0, 0.14, 0.2]);
        assert_eq!(e, vec![0.14, 0.2, 0.6]);
    }

    #[test]
    fn words_that_overrun_the_segment_are_pulled_back_or_flagged() {
        let mut s = vec![0.0, 0.5, 0.9];
        let mut e = vec![0.5, 0.9, 1.4];
        // Fits: the last word is pulled back to the segment end.
        assert!(!repair_spans(&mut s, &mut e, 0.0, 1.2));
        assert!(e[2] <= 1.2 + 1e-9 && e[2] > s[2]);
        // Cannot fit three words of minimum length into 0.05 s.
        let mut s = vec![0.0, 0.01, 0.02];
        let mut e = vec![0.01, 0.02, 0.03];
        assert!(repair_spans(&mut s, &mut e, 0.0, 0.05));
        for i in 0..3 {
            assert!(e[i] > s[i]);
        }
    }

    #[test]
    fn prompt_encodes_to_tokens_that_decode_back() {
        // Needs the base model on disk; the vocabulary is the only input.
        let home = std::env::var("HOME").unwrap_or_default();
        let path = Path::new(&home).join(".cache/edith/models/whisper-base/tokenizer.json");
        let Ok(vocab) = Vocab::load(&path) else { return };
        for text in [" Edith, LUFS", " Edith turns a folder of raw footage."] {
            let toks = vocab.encode(text);
            assert!(!toks.is_empty());
            assert_eq!(vocab.decode(&toks), text);
        }
        // Merges are applied: a common word is one token, not one per byte.
        assert_eq!(vocab.encode(" turns").len(), 1);
    }

    #[test]
    fn dtw_follows_the_cheap_diagonal() {
        // 3 rows x 5 frames; cells (0,0) (1,2) (2,4) are cheap, so the path must cross them.
        let (rows, cols) = (3, 5);
        let mut cost = vec![1.0f32; rows * cols];
        cost[0] = -1.0;
        cost[cols + 2] = -1.0;
        cost[2 * cols + 4] = -1.0;
        let (ti, tj) = dtw_path(&cost, rows, cols);
        assert_eq!((ti[0], tj[0]), (0, 0));
        assert_eq!((*ti.last().unwrap(), *tj.last().unwrap()), (2, 4));
        let path: Vec<(usize, usize)> = ti.iter().copied().zip(tj.iter().copied()).collect();
        for cell in [(0, 0), (1, 2), (2, 4)] {
            assert!(path.contains(&cell), "path misses {cell:?}: {path:?}");
        }
    }

    #[test]
    fn median_filter_reflects_edges() {
        let x = [1.0f32, 1.0, 9.0, 1.0, 1.0];
        assert_eq!(median_filter(&x, 3), vec![1.0, 1.0, 1.0, 1.0, 1.0]);
        assert_eq!(median_filter(&[5.0], 7), vec![5.0]);
    }

    #[test]
    fn silence_filter_needs_both_no_speech_signals() {
        // Hallucination: model says no speech and is unsure of its own text.
        assert_eq!(silence_reason(0.9, -1.5, None), Some("no_speech"));
        // Confident text is kept even when no_speech is high.
        assert_eq!(silence_reason(0.9, -0.2, None), None);
        // Unsure text is kept when the model does not think it is silence, if the span is audible.
        assert_eq!(silence_reason(0.1, -1.5, Some(-30.0)), Some("silent_span"));
        assert_eq!(silence_reason(0.1, -1.5, Some(20.0)), None);
        // Thresholds are strict: exactly 0.6 and exactly -1.0 are kept.
        assert_eq!(silence_reason(0.6, -1.5, None), None);
        assert_eq!(silence_reason(0.9, -1.0, None), None);
    }

    #[test]
    fn silence_filter_drops_spans_at_the_noise_floor_regardless_of_text() {
        assert_eq!(silence_reason(0.0, -0.1, Some(1.0)), Some("silent_span"));
        assert_eq!(silence_reason(0.0, -0.1, Some(SILENT_REL_DB - 0.1)), Some("silent_span"));
        assert_eq!(silence_reason(0.0, -0.1, Some(SILENT_REL_DB)), None);
        // No frames in the span: no energy evidence either way.
        assert_eq!(silence_reason(0.0, -0.1, None), None);
    }

    #[test]
    fn span_level_is_relative_to_the_noise_floor() {
        // 2 s of a 220 Hz bed at -20 dBFS, then 2 s of 1 kHz speech at -10 dBFS: the speech band
        // drops the bed, so the speech span sits far above the floor and the bed span does not.
        let sr = SR as f32;
        let bed: Vec<f32> = (0..2 * SR).map(|i| 0.1 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr).sin()).collect();
        let speech: Vec<f32> = (0..2 * SR).map(|i| 0.316 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin()).collect();
        let pcm: Vec<f32> = bed.iter().chain(speech.iter()).copied().collect();
        let lv = levels(&pcm);
        let speech_rel = span_rel_db(&lv, 2.5, 3.5).unwrap();
        let bed_rel = span_rel_db(&lv, 0.5, 1.5).unwrap();
        assert!(speech_rel > 20.0, "speech span {speech_rel:.1} dB above floor");
        assert!(bed_rel.abs() < SILENT_REL_DB, "bed span {bed_rel:.1} dB above floor");
        assert_eq!(span_rel_db(&lv, 0.5, 0.5), None);
    }

    #[test]
    fn speech_band_drops_the_bed_and_keeps_speech() {
        let sr = SR as f32;
        let bed: Vec<f32> = (0..SR).map(|i| 0.1 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr).sin()).collect();
        let voice: Vec<f32> = (0..SR).map(|i| 0.1 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr).sin()).collect();
        let rms = |v: &[f64]| (v[SR / 4..3 * SR / 4].iter().map(|x| x * x).sum::<f64>() / (SR / 2) as f64).sqrt();
        let bed_out = rms(&speech_band(&bed));
        let voice_out = rms(&speech_band(&voice));
        assert!(bed_out < 0.1 * 0.1, "220 Hz bed leaks through: {bed_out}");
        assert!(voice_out > 0.9 * 0.1 / 2f64.sqrt(), "1 kHz speech attenuated: {voice_out}");
    }

    #[test]
    fn speech_frames_flag_only_frames_above_the_floor() {
        // 1 s of bed (220 Hz), 1 s of speech-band tone, 1 s of bed again.
        let sr = SR as f32;
        let mut pcm = Vec::new();
        for i in 0..3 * SR {
            let f = if (SR..2 * SR).contains(&i) { 1000.0 } else { 220.0 };
            let a = if f > 500.0 { 0.2 } else { 0.1 };
            pcm.push(a * (2.0 * std::f32::consts::PI * f * i as f32 / sr).sin());
        }
        let flags = speech_frames(&levels(&pcm));
        let frame = |t: f64| (t / ENC_SPF) as usize;
        assert!(!flags[frame(0.3)] && !flags[frame(2.7)], "bed frames flagged");
        assert!(flags[frame(1.2)] && flags[frame(1.8)], "speech frames missed");
    }

    #[test]
    fn quiet_floor_is_the_mean_of_the_quietest_tenth() {
        let v: Vec<f64> = (1..=20).map(|x| x as f64).collect();
        assert_eq!(quiet_floor_db(&v), 1.5); // quietest 2 of 20: 1 and 2
        assert_eq!(quiet_floor_db(&[7.0]), 7.0);
    }

    #[test]
    fn crop_covers_the_segment_plus_margin_and_is_clamped() {
        // 5 s segment inside 20 s of audio, 0.5 s margin: 6 s of 20 ms frames.
        assert_eq!(crop_frames(5.0, 10.0, 1000, 0.5, 0.5), (225, 525));
        // Margins past the audio edges are clamped to it.
        assert_eq!(crop_frames(0.2, 1.0, 1000, 0.5, 0.5), (0, 75));
        assert_eq!(crop_frames(14.9, 15.0, 760, 0.5, 0.5), (720, 760));
        // Never empty, even for a zero-length span.
        let (f0, f1) = crop_frames(3.0, 3.0, 100, 0.0, 0.0);
        assert!(f1 > f0 && f1 <= 100);
    }

    #[test]
    fn tied_starts_are_spread_between_their_neighbours() {
        // Tokens 0 and 1 both pinned at 14.16 s, the next token at 14.44 s, the end at 15.0 s.
        let mut s = vec![14.16, 14.16, 14.44, 14.74];
        spread_tied_starts(&mut s, 15.0);
        assert_eq!(s[0], 14.16);
        assert!((s[1] - 14.30).abs() < 1e-9, "{}", s[1]);
        assert_eq!(s[2], 14.44);
        // A tie in the middle is spread from the start before it.
        let mut m = vec![1.0, 2.0, 2.0, 2.0, 5.0];
        spread_tied_starts(&mut m, 6.0);
        assert!((m[2] - 3.0).abs() < 1e-9 && (m[3] - 4.0).abs() < 1e-9, "{m:?}");
        // Strictly increasing starts are left alone.
        let mut t = vec![0.0, 0.5, 1.0];
        spread_tied_starts(&mut t, 2.0);
        assert_eq!(t, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn crop_trims_to_the_speech_it_contains() {
        let mut speech = vec![false; 100];
        speech[40..60].fill(true);
        assert_eq!(trim_crop_to_speech((30, 80), &speech), (40, 60));
        // Crops with no speech keep their span.
        assert_eq!(trim_crop_to_speech((0, 30), &speech), (0, 30));
        // A single speech frame gives a one-frame crop.
        let mut one = vec![false; 10];
        one[4] = true;
        assert_eq!(trim_crop_to_speech((2, 8), &one), (4, 5));
    }

    #[test]
    fn segments_starting_at_the_audio_end_are_dropped() {
        // 15.28 s of audio: a start at 15.26 s is the spurious tail segment.
        assert!(starts_past_end(15.26, 15.28));
        assert!(starts_past_end(15.18, 15.28));
        assert!(!starts_past_end(15.0, 15.28));
        assert!(!starts_past_end(0.0, 15.28));
    }

    #[test]
    fn words_past_the_audio_end_are_cut_and_the_rest_of_the_segment_stays() {
        let word = |w: &str, s: f64, e: f64| serde_json::json!({"w": w, "start": s, "end": e, "p": 0.9});
        // 2.0 s of audio, offset 0: "Edith makes editing fast." ends before it, "We can." starts after it.
        let mut words = vec![
            word("Edith", 0.04, 0.4),
            word("makes", 0.4, 0.6),
            word("editing", 0.6, 1.08),
            word("fast.", 1.08, 1.92),
            word("We", 1.95, 2.2),
            word("can.", 2.2, 2.4),
        ];
        assert_eq!(fit_words_to_audio(&mut words, 0.0, 2.0), 2);
        let kept: Vec<&str> = words.iter().filter_map(|w| w["w"].as_str()).collect();
        assert_eq!(kept, ["Edith", "makes", "editing", "fast."]);
        assert_eq!(words[3]["end"].as_f64(), Some(1.92));

        // A word starting inside the audio but running past its end keeps its start and gets its end clamped.
        let mut words = vec![word("cut", 1.5, 2.3)];
        assert_eq!(fit_words_to_audio(&mut words, 0.0, 2.0), 0);
        assert_eq!(words[0]["start"].as_f64(), Some(1.5));
        assert_eq!(words[0]["end"].as_f64(), Some(2.0));

        // Offset: word times are absolute, so the cut is judged on audio-local times.
        let mut words = vec![word("a", 10.5, 10.9), word("b", 11.95, 12.2)];
        assert_eq!(fit_words_to_audio(&mut words, 10.0, 2.0), 1);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0]["end"].as_f64(), Some(10.9));

        // Every word past the end: nothing is kept.
        let mut words = vec![word("x", 2.0, 2.3)];
        assert_eq!(fit_words_to_audio(&mut words, 0.0, 2.0), 1);
        assert!(words.is_empty());
    }

    #[test]
    fn zscore_normalises_each_frame_over_the_token_rows() {
        let mut m = vec![vec![1.0f32, 5.0], vec![3.0, 5.0], vec![5.0, 5.0]];
        zscore_columns(&mut m);
        // Column 0: mean 3, population sd sqrt(8/3). Column 1 is constant: all zeros.
        let sd = (8.0f64 / 3.0).sqrt();
        assert!((m[0][0] as f64 + 2.0 / sd).abs() < 1e-4);
        assert!((m[2][0] as f64 - 2.0 / sd).abs() < 1e-4);
        assert!(m.iter().all(|r| r[1].abs() < 1e-6));
    }

    #[test]
    fn softmax_prob_is_a_probability() {
        let logits = [0.0f32, 1.0, 2.0];
        let total: f64 = (0..3).map(|i| softmax_prob(&logits, i)).sum();
        assert!((total - 1.0).abs() < 1e-9);
        assert!(softmax_prob(&logits, 2) > softmax_prob(&logits, 0));
        assert_eq!(softmax_prob(&logits, 9), 0.0);
    }

    #[test]
    fn english_suppression_catches_foreign_scripts_not_punctuation() {
        // "锈" (U+9508) and "拜" (U+62DC) start with 0xE9 and 0xE6; Cyrillic "д" is D0 B4.
        assert!(foreign_script_bytes("锈".as_bytes()));
        assert!(foreign_script_bytes("拜".as_bytes()));
        assert!(foreign_script_bytes("д".as_bytes()));
        assert!(foreign_script_bytes("、".as_bytes())); // U+3001, ideographic comma
        assert!(foreign_script_bytes("ひ".as_bytes()));
        assert!(foreign_script_bytes("한".as_bytes()));
        // Plain English, accented Latin and general punctuation are kept.
        for ok in ["the", " harbour", "café", "naïve", "\u{2018}quoted\u{2019}", "a\u{2014}b", "…", "$5"] {
            assert!(!foreign_script_bytes(ok.as_bytes()), "{ok:?} must not be suppressed");
        }
    }

    #[test]
    fn blank_tokens_are_whitespace_only() {
        assert!(blank_bytes(b" "));
        assert!(blank_bytes(b"\n"));
        assert!(!blank_bytes(b""));
        assert!(!blank_bytes(b" The"));
    }

    #[test]
    fn resample_keeps_length_ratio() {
        let mono = vec![0.0f32; 22050];
        let src = 22050.0f64;
        let out_len = (mono.len() as f64 * SR as f64 / src).round() as usize;
        assert_eq!(out_len, 16000);
    }

    #[test]
    fn loops_are_found_on_text_tokens_and_cut_to_one_copy() {
        let ts0 = 1000u32;
        // Text 1, then the phrase "7 8" said four times, each copy after its own timestamp.
        let mut out: Vec<(u32, f32)> = vec![(ts0 + 5, 0.0), (1, 0.0), (ts0 + 10, 0.0)];
        for copy in 0..4u32 {
            out.push((7, 0.0));
            out.push((8, 0.0));
            out.push((ts0 + 14 + copy * 4, 0.0));
        }
        let p = text_loop_period(&out, ts0).expect("a loop");
        assert_eq!(p, 2);
        cut_loop(&mut out, ts0, p);
        // The first copy stays, and the window ends on the timestamp before the repeats.
        let kept: Vec<u32> = out.iter().map(|t| t.0).collect();
        assert_eq!(kept, vec![ts0 + 5, 1, ts0 + 10, 7, 8, ts0 + 14]);
    }

    #[test]
    fn loop_right_after_the_first_timestamp_leaves_no_text() {
        // The window-0 case: `<|0.00|>`, then the phrase said four times with no timestamp
        // between the copies. The cut keeps only the timestamp, so the window has no text.
        let ts0 = 1000u32;
        let mut out: Vec<(u32, f32)> = vec![(ts0, 0.0)];
        for _ in 0..4u32 {
            out.push((7, 0.0));
            out.push((8, 0.0));
        }
        let p = text_loop_period(&out, ts0).expect("a loop");
        cut_loop(&mut out, ts0, p);
        assert!(!has_text(&out, ts0));
        assert!(window_rejected(0.3, -0.2, true), "an empty loop cut must be retried");
    }

    #[test]
    fn window_rejection_follows_whisper_thresholds() {
        assert!(!window_rejected(1.5, -0.4, false));
        assert!(window_rejected(2.5, -0.4, false), "repetitive text");
        assert!(window_rejected(1.5, -1.2, false), "unsure tokens");
        assert!(window_rejected(1.5, -0.4, true), "loop without text");
    }

    #[test]
    fn temperature_schedule_is_whisper_fallback() {
        assert_eq!(TEMPS.first(), Some(&0.0));
        assert_eq!(TEMPS.last(), Some(&1.0));
        assert!(TEMPS.windows(2).all(|w| (w[1] - w[0] - 0.2).abs() < 1e-9));
    }

    #[test]
    fn seek_goes_to_the_last_timestamp_when_the_window_ends_on_one() {
        // Ends on a timestamp at 5.0 s: seek there (500 frames), whether or not it looped.
        assert_eq!(seek_advance_frames(true, Some(5.0), false, 3000), 500);
        assert_eq!(seek_advance_frames(true, Some(5.0), true, 3000), 500);
        // Ends on text: the whole window is consumed.
        assert_eq!(seek_advance_frames(false, Some(5.0), false, 3000), 3000);
    }

    #[test]
    fn looped_window_without_a_timestamp_steps_briefly_not_thirty_seconds() {
        // Window 0 looped right after `<|0.00|>`: the cut leaves only that timestamp (t = 0).
        assert_eq!(seek_advance_frames(true, Some(0.0), true, 3000), 500);
        // Nothing left at all after the cut.
        assert_eq!(seek_advance_frames(false, None, true, 3000), 500);
        // Near the end of the audio the step is clamped to what is left.
        assert_eq!(seek_advance_frames(false, None, true, 200), 200);
        // A loop after a timestamp at 12 s seeks to it.
        assert_eq!(seek_advance_frames(true, Some(12.0), true, 3000), 1200);
    }

    #[test]
    fn ordinary_speech_is_not_a_loop() {
        let ts0 = 1000u32;
        let out: Vec<(u32, f32)> = [1u32, 2, 3, 4, 5, 6, 7, 8, 9, 10].iter().map(|&t| (t, 0.0)).collect();
        assert_eq!(text_loop_period(&out, ts0), None);
        let twice: Vec<(u32, f32)> = [1u32, 2, 1, 2, 3, 4].iter().map(|&t| (t, 0.0)).collect();
        assert_eq!(text_loop_period(&twice, ts0), None);
    }

    /// Speech flags with `[a, b)` (frames) set.
    fn speech_at(n: usize, a: usize, b: usize) -> Vec<bool> {
        (0..n).map(|j| (a..b).contains(&j)).collect()
    }

    #[test]
    fn onset_needs_more_than_a_quarter_second_of_speech_before_the_word() {
        // 1 s of speech (50 frames) from frame 10, the word at frame 60: an onset at 10.
        let s = speech_at(100, 10, 60);
        assert_eq!(leading_onset(&s, 0, 60), Some(10));
        // 0.2 s (10 frames) is a breath, not a filler: no onset.
        let short = speech_at(100, 50, 60);
        assert_eq!(leading_onset(&short, 0, 60), None);
        // 0.24 s (12 frames) is still short of the 0.25 s that must come before the word.
        let just_short = speech_at(100, 48, 60);
        assert_eq!(leading_onset(&just_short, 0, 60), None);
    }

    #[test]
    fn onset_search_starts_after_the_previous_word() {
        // Speech in [0, 30) is the previous word's; the gap [30, 60) is silent: no onset.
        let s = speech_at(100, 0, 30);
        assert_eq!(leading_onset(&s, 30, 60), None);
        // With speech after the previous word's end the onset is found from `from`.
        let s = speech_at(100, 0, 60);
        assert_eq!(leading_onset(&s, 30, 60), Some(30));
    }

    #[test]
    fn filler_words_match_regardless_of_case_and_punctuation() {
        assert!(is_filler_word("Um,"));
        assert!(is_filler_word("uh"));
        assert!(is_filler_word("Hmm..."));
        assert!(is_filler_word("Uhm,"));
        assert!(!is_filler_word("yes,"));
        assert!(!is_filler_word("like"), "like is a real word too often to recover");
    }

    #[test]
    fn recovered_fillers_stop_at_the_previous_word_and_the_segment_word() {
        let w = |t: &str, s: f64| serde_json::json!({"w": t, "start": s, "end": s + 0.3});
        let found = vec![
            w("Um,", 0.98),     // in the gap: kept
            w("Yes,", 2.6),     // not a filler: dropped
            w("uh", 0.1),       // before the previous word's end: already taken
            w("Hmm", 2.02),     // at the segment's first word (2.0): the same sound, dropped
        ];
        let got = recovered_fillers(&found, 0.5, 2.0);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0]["w"], "Um,");
    }
}
