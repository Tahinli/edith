//! Text to speech with a local Piper voice: the voices on this machine, and a voiceover rendered
//! to a WAV. Shared by the MCP server's `voiceover` and the GUI's Voiceover card.
//!
//! Piper is an external binary (`EDITH_PIPER`, else `~/.local/share/piper-venv/bin/piper`)
//! fed the text on stdin. A voice is a `<name>.onnx` file with a `<name>.onnx.json`
//! beside it, read from `EDITH_PIPER_VOICES` or `~/.local/share/piper-voices`. Renders
//! are cached by a hash of (text, voice, speed, pause, model) in `voiceover/.renders/`:
//! a repeat reuses the render, and the named WAV beside the project is a copy of it.
//! Piper's phoneme-duration noise is pinned to 0, so one render has one length.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::speech::home;

pub const DEFAULT_VOICE: &str = "en_US-ryan-high";

pub const TIMEOUT: Duration = Duration::from_secs(120);

/// A WAV with no samples is its 44-byte header; anything bigger is a usable render.
pub const WAV_HEADER: u64 = 44;

/// The folder under `voiceover/` that holds one render per cache key.
pub const RENDER_DIR: &str = ".renders";

#[derive(Debug, Clone, PartialEq)]
pub struct Voice {
    pub name: String,
    pub language: Option<String>,
    pub quality: Option<String>,
    pub sample_rate: Option<u64>,
    pub speakers: u64,
    /// "female" or "male" from the name's tokens, else "unknown" (Piper's config has no gender).
    pub gender: &'static str,
}

/// The gender a voice name carries: a `female` or `male` token (split on `-` and `_`),
/// else "unknown". Matched by whole token, so "female" is never read as "male".
pub fn gender_of(name: &str) -> &'static str {
    let mut found = "unknown";
    for token in name.split(['-', '_']).map(str::to_ascii_lowercase) {
        match token.as_str() {
            "female" => return "female",
            "male" => found = "male",
            _ => {}
        }
    }
    found
}

pub fn voices_dir() -> PathBuf {
    std::env::var_os("EDITH_PIPER_VOICES")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share/piper-voices"))
}

/// The piper binary: `EDITH_PIPER` when set (it must be a file), else the venv's.
pub fn piper_bin() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("EDITH_PIPER") {
        let p = PathBuf::from(p);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!("EDITH_PIPER is set to {}, which is not a file", p.display()))
        };
    }
    let p = home().join(".local/share/piper-venv/bin/piper");
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("piper is not installed at {}; set EDITH_PIPER to the binary", p.display()))
    }
}

pub fn read_voice(name: &str, json_path: &Path) -> Voice {
    let meta: Option<serde_json::Value> = fs::read_to_string(json_path).ok().and_then(|s| serde_json::from_str(&s).ok());
    parse_voice(name, meta.as_ref())
}

/// A voice from its name and its parsed `.onnx.json` (None when the file is missing or
/// unreadable). Some voices carry no `language.code`, `audio.quality` or `dataset`
/// (tr_TR-eren-medium's config has only `espeak.voice: "tr"`), so each field falls back
/// to the file name `<lang>_<REGION>-<name>-<quality>` before it is left empty. A
/// field that is present but empty counts as missing.
pub fn parse_voice(name: &str, meta: Option<&serde_json::Value>) -> Voice {
    let text = |path: &str| {
        meta.and_then(|v| v.pointer(path))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let stem = name.split('-').next().filter(|s| s.contains('_')).map(str::to_string);
    let name_quality = (name.matches('-').count() >= 2).then(|| name.rsplit('-').next().unwrap_or_default().to_string());
    Voice {
        name: name.to_string(),
        language: text("/language/code").or(stem).or_else(|| text("/espeak/voice")),
        quality: text("/audio/quality").or(name_quality),
        sample_rate: meta.and_then(|v| v.pointer("/audio/sample_rate")).and_then(serde_json::Value::as_u64),
        speakers: meta.and_then(|v| v.pointer("/num_speakers")).and_then(serde_json::Value::as_u64).unwrap_or(1),
        gender: gender_of(name),
    }
}

/// Every `<name>.onnx` in `dir`, with its `.onnx.json` when present, sorted by name.
pub fn list_voices(dir: &Path) -> Result<Vec<Voice>, String> {
    let entries = fs::read_dir(dir).map_err(|e| {
        format!("cannot read the voices dir {}: {e}. Set EDITH_PIPER_VOICES, or install a Piper voice there", dir.display())
    })?;
    let mut voices: Vec<Voice> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let stem = file.strip_suffix(".onnx")?.to_string();
            let json_path = dir.join(format!("{file}.json"));
            Some(read_voice(&stem, &json_path))
        })
        .collect();
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(voices)
}

/// en_US-ryan-high if present, else the first en_US voice, else the first voice.
/// `voices` must be sorted by name, as `list_voices` returns it.
pub fn default_voice(voices: &[Voice]) -> Option<&Voice> {
    voices
        .iter()
        .find(|v| v.name == DEFAULT_VOICE)
        .or_else(|| voices.iter().find(|v| v.name.starts_with("en_US-")))
        .or_else(|| voices.first())
}

/// piper's `--length-scale` for a speed: twice as fast is half the length.
pub fn length_scale(speed: f64) -> f64 {
    1.0 / speed
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The hash of everything that changes the audio: text, voice, speed, pause and
/// the model file. The output name is not part of it. Speed and pause are written
/// at fixed precision so float noise cannot split one render into two files.
pub fn cache_hash(text: &str, voice: &str, speed: f64, pause: f64, model: &Path) -> u32 {
    let key = format!("v3|{text}|{voice}|{speed:.4}|{pause:.3}|{}", model.display());
    (fnv1a(key.as_bytes()) & 0xffff_ffff) as u32
}

/// A lowercase ASCII slug of at most 40 characters, `voiceover` when nothing is left.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.truncate(40);
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() { "voiceover".to_string() } else { out }
}

/// `<name or slug of text>-<hash8>.wav`.
pub fn wav_name(name: Option<&str>, text: &str, hash: u32) -> String {
    let base = slug(name.unwrap_or(text));
    format!("{base}-{hash:08x}.wav")
}

/// The last few non-empty lines of piper's stderr, for an error message.
pub fn tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return "no output".to_string();
    }
    lines[lines.len().saturating_sub(5)..].join(" | ")
}

/// Runs piper with `text` on stdin, stopping it after `TIMEOUT`.
pub fn run_piper(bin: &Path, args: &[String], text: &str) -> Result<(), String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start piper ({}): {e}", bin.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Closing stdin (on drop) tells piper the text is complete. A write that fails
        // means piper quit early, which its exit status reports.
        let _ = stdin.write_all(text.as_bytes());
    }
    let mut stderr = child.stderr.take().ok_or("piper: no stderr pipe")?;
    let reader = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("piper did not finish within {}s and was stopped", TIMEOUT.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("waiting for piper: {e}")),
        }
    };
    let log = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        Err(format!("piper failed ({status}): {}", tail(&log)))
    }
}

/// Renders `text` to `out` through a `.part` file, so a failed run leaves no WAV behind.
pub fn synth(bin: &Path, model: &Path, text: &str, speed: f64, pause: f64, out: &Path) -> Result<(), String> {
    let part = out.with_extension("part");
    let args: Vec<String> = [
        "-m".to_string(),
        model.display().to_string(),
        "-f".to_string(),
        part.display().to_string(),
        "--length-scale".to_string(),
        format!("{:.4}", length_scale(speed)),
        // Duration noise off: the same text renders to the same length every time.
        "--noise-w-scale".to_string(),
        "0".to_string(),
        "--sentence-silence".to_string(),
        format!("{pause:.3}"),
    ]
    .into();
    let result = run_piper(bin, &args, text).and_then(|()| {
        let len = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        if len <= WAV_HEADER {
            return Err("piper wrote no audio".to_string());
        }
        // Piper's sentence silence goes between sentences only, so the last one gets its pause here.
        pad_tail(&part, pause)?;
        fs::rename(&part, out).map_err(|e| format!("could not save {}: {e}", out.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Appends `secs` of silence to a 16-bit PCM WAV that piper wrote, fixing its RIFF and data sizes.
/// A no-op for a zero pause.
pub fn pad_tail(path: &Path, secs: f64) -> Result<(), String> {
    if secs <= 0.0 {
        return Ok(());
    }
    let mut bytes = fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let canonical = bytes.len() >= 44 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" && &bytes[36..40] == b"data";
    if !canonical {
        return Err("piper wrote a WAV this server cannot pad".to_string());
    }
    let channels = u64::from(u16::from_le_bytes([bytes[22], bytes[23]]));
    let rate = u64::from(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]));
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    if bits != 16 || channels == 0 || rate == 0 {
        return Err(format!("piper wrote {bits}-bit audio at {rate} Hz; only 16-bit PCM can take a pause"));
    }
    let extra = (secs * rate as f64).round() as u64 * channels * 2;
    let data = (bytes.len() - 44) as u64 + extra;
    let riff = (bytes.len() - 8) as u64 + extra;
    if data > u64::from(u32::MAX) || riff > u64::from(u32::MAX) {
        return Err("the voiceover is too long to write a WAV for".to_string());
    }
    bytes.resize(bytes.len() + extra as usize, 0);
    bytes[4..8].copy_from_slice(&(riff as u32).to_le_bytes());
    bytes[40..44].copy_from_slice(&(data as u32).to_le_bytes());
    fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// The longest text a voiceover takes, in characters.
pub const TEXT_LIMIT: usize = 5000;
/// The speed range a voiceover takes (1.0 is the voice's own pace).
pub const SPEED_MIN: f64 = 0.5;
pub const SPEED_MAX: f64 = 2.0;
/// The longest pause between sentences, in seconds.
pub const PAUSE_MAX: f64 = 10.0;

/// A voiceover on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// The WAV: `<dir>/<name or slug of text>-<hash8>.wav`.
    pub file: PathBuf,
    pub voice: String,
    /// The render was reused rather than synthesized.
    pub cached: bool,
}

/// Speaks `text` with `voice` (the default voice when `None`) into `dir`, reusing an identical
/// earlier render. Checked as the server checks it: text length, speed and pause ranges, the voice
/// being installed.
pub fn render(dir: &Path, text: &str, name: Option<&str>, voice: Option<&str>, speed: f64, pause: f64) -> Result<Rendered, String> {
    if text.trim().is_empty() {
        return Err("missing argument `text`: give the words to say".into());
    }
    let chars = text.chars().count();
    if chars > TEXT_LIMIT {
        return Err(format!("text is {chars} characters; the limit is {TEXT_LIMIT}: split it into several voiceovers"));
    }
    if !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
        return Err(format!("speed {speed} is outside {SPEED_MIN}..{SPEED_MAX}"));
    }
    if !(0.0..=PAUSE_MAX).contains(&pause) {
        return Err(format!("pause {pause} is outside 0..{PAUSE_MAX} seconds"));
    }
    let vdir = voices_dir();
    let voices = list_voices(&vdir)?;
    let voice = match voice {
        Some(want) => voices.iter().find(|v| v.name == want).ok_or_else(|| {
            let names: Vec<&str> = voices.iter().map(|v| v.name.as_str()).collect();
            format!("no voice `{want}` in {}; voices: {}", vdir.display(), names.join(", "))
        })?,
        None => default_voice(&voices).ok_or_else(|| format!("no voices in {}; install a Piper voice", vdir.display()))?,
    };
    let model = vdir.join(format!("{}.onnx", voice.name));
    let hash = cache_hash(text, &voice.name, speed, pause, &model);
    let renders = dir.join(RENDER_DIR);
    let render = renders.join(format!("{hash:08x}.wav"));
    let file = dir.join(wav_name(name, text, hash));
    let cached = fs::metadata(&render).is_ok_and(|m| m.len() > WAV_HEADER);
    if !cached {
        let bin = piper_bin()?;
        fs::create_dir_all(&renders).map_err(|e| format!("could not create {}: {e}", renders.display()))?;
        synth(&bin, &model, text, speed, pause, &render)?;
    }
    if file != render {
        // The named WAV is a copy of the render; the copy is refreshed when the render differs.
        let same = fs::metadata(&file).is_ok_and(|m| m.len() == fs::metadata(&render).map_or(0, |r| r.len()));
        if !same {
            fs::create_dir_all(dir)
                .and_then(|()| fs::copy(&render, &file).map(|_| ()))
                .map_err(|e| format!("could not save {}: {e}", file.display()))?;
        }
    }
    Ok(Rendered { file, voice: voice.name.clone(), cached })
}
