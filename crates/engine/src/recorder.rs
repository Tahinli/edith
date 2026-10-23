//! Voice-over recording: the server's input written to a WAV in the project's
//! media folder as it arrives. A worker thread drains [`crate::pulse::Recording`]
//! into a 16-bit PCM file; the caller reads the meter and the clock, and stops
//! it to finalise the header.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::pulse::Recording;

/// Sample rate of every recording.
pub const RATE: u32 = 48_000;
/// Channels of every recording: the server remixes a mono input to both.
pub const CHANNELS: u16 = 2;
/// How often the worker takes what the server has captured.
const POLL: Duration = Duration::from_millis(10);
/// The most a WAV's data chunk can hold: its size is a u32.
const MAX_DATA_BYTES: u64 = u32::MAX as u64 - 36;
/// The meter's floor, in dBFS: silence reads as this.
pub const SILENCE_DB: f32 = -120.0;

/// Writes a 16-bit PCM WAV. The header goes first with zero sizes and `finish`
/// patches them in once the data is known.
pub struct WavWriter {
    file: BufWriter<File>,
    data_bytes: u64,
}

impl WavWriter {
    pub fn create(path: &Path, rate: u32, channels: u16) -> io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(rate, channels, 0))?;
        Ok(Self { file, data_bytes: 0 })
    }

    /// Appends interleaved f32 samples, clamped to -1..1, as 16-bit PCM.
    pub fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &sample in samples {
            let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        self.data_bytes += bytes.len() as u64;
        Ok(())
    }

    /// Bytes of audio written so far.
    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
    }

    /// Patches the sizes into the header and closes the file. Returns the sample
    /// frames written.
    pub fn finish(self, rate: u32, channels: u16) -> io::Result<u64> {
        let data_bytes = self.data_bytes;
        let mut file = self.file.into_inner().map_err(io::Error::other)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(rate, channels, data_bytes))?;
        file.flush()?;
        Ok(data_bytes / (2 * u64::from(channels)))
    }
}

/// The 44-byte RIFF header of a PCM WAV holding `data_bytes` of samples.
fn header(rate: u32, channels: u16, data_bytes: u64) -> [u8; 44] {
    let block = u32::from(channels) * 2;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block).to_le_bytes());
    h[32..34].copy_from_slice(&(block as u16).to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&(data_bytes as u32).to_le_bytes());
    h
}

/// What the meter and the clock show while a recording runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub path: PathBuf,
    /// Seconds of audio written so far.
    pub elapsed_secs: f64,
    /// The loudest sample since the last [`Recorder::level_db`], in dBFS.
    pub level_db: f32,
}

/// A finished recording, its header final.
#[derive(Clone, Debug, PartialEq)]
pub struct Take {
    pub path: PathBuf,
    pub frames: u64,
    pub duration_secs: f64,
}

/// The first `voiceover-<n>.wav` in `folder` that is not there yet.
fn next_take_path(folder: &Path) -> PathBuf {
    (1..)
        .map(|n| folder.join(format!("voiceover-{n}.wav")))
        .find(|path| !path.exists())
        .expect("a free take name")
}

/// A recording in progress. [`Recorder::stop`] finishes it; dropping one stops
/// it too, keeping what was written.
pub struct Recorder {
    path: PathBuf,
    frames: Arc<AtomicU64>,
    /// The loudest sample since the meter was last read, as f32 bits. Positive
    /// floats order the same as their bits, so `fetch_max` keeps the peak.
    peak: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<u64, String>>>,
}

impl Recorder {
    /// Starts recording from `source` (a source name, or the server's default
    /// input when `None`) into a new take in `folder`, which is created if it is
    /// not there. Fails when there is no audio server or the file cannot be made.
    pub fn start(folder: &Path, source: Option<&str>) -> Result<Self, String> {
        std::fs::create_dir_all(folder).map_err(|e| format!("cannot make {}: {e}", folder.display()))?;
        let path = next_take_path(folder);
        let recording = Recording::open(RATE, u32::from(CHANNELS), source)
            .ok_or_else(|| "no audio input: the audio server would not record".to_string())?;
        let writer = WavWriter::create(&path, RATE, CHANNELS).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let frames = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (frames, peak, stop) = (frames.clone(), peak.clone(), stop.clone());
            std::thread::spawn(move || work(recording, writer, &frames, &peak, &stop))
        };
        Ok(Self {
            path,
            frames,
            peak,
            stop,
            worker: Some(worker),
        })
    }

    /// The take's path, the clock and the level since the last call.
    pub fn status(&self) -> Status {
        Status {
            path: self.path.clone(),
            elapsed_secs: self.frames.load(Ordering::Relaxed) as f64 / f64::from(RATE),
            level_db: self.level_db(),
        }
    }

    /// The peak sample since the last call, in dBFS, and resets the peak.
    pub fn level_db(&self) -> f32 {
        let peak = f32::from_bits(self.peak.swap(0, Ordering::Relaxed));
        if peak <= 0.0 {
            return SILENCE_DB;
        }
        (20.0 * peak.log10()).max(SILENCE_DB)
    }

    /// Stops the recording, finalises the WAV and returns it.
    pub fn stop(mut self) -> Result<Take, String> {
        self.finish()
    }

    fn finish(&mut self) -> Result<Take, String> {
        self.stop.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return Err("the recording has already stopped".to_string());
        };
        let frames = worker.join().map_err(|_| "the recording thread panicked".to_string())??;
        Ok(Take {
            path: self.path.clone(),
            frames,
            duration_secs: frames as f64 / f64::from(RATE),
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// The worker: drains the server every [`POLL`] until asked to stop or the
/// input dies, then stops the stream, takes the rest and finalises the file.
fn work(
    recording: Recording,
    mut writer: WavWriter,
    frames: &AtomicU64,
    peak: &AtomicU32,
    stop: &AtomicBool,
) -> Result<u64, String> {
    let mut failure = None;
    let take = |writer: &mut WavWriter, samples: Vec<f32>| -> Result<(), String> {
        if samples.is_empty() {
            return Ok(());
        }
        let loudest = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        peak.fetch_max(loudest.to_bits(), Ordering::Relaxed);
        writer.write(&samples).map_err(|e| format!("cannot write the take: {e}"))?;
        frames.store(writer.data_bytes() / (2 * u64::from(CHANNELS)), Ordering::Relaxed);
        Ok(())
    };
    loop {
        if stop.load(Ordering::Acquire) || !recording.alive() {
            break;
        }
        if writer.data_bytes() + 4 * u64::from(CHANNELS) > MAX_DATA_BYTES {
            failure = Some("the take reached the longest a WAV can hold".to_string());
            break;
        }
        if let Err(e) = take(&mut writer, recording.read()) {
            failure = Some(e);
            break;
        }
        std::thread::sleep(POLL);
    }
    recording.set_active(false);
    if failure.is_none()
        && let Err(e) = take(&mut writer, recording.read())
    {
        failure = Some(e);
    }
    drop(recording);
    let frames_out = writer.finish(RATE, CHANNELS).map_err(|e| format!("cannot finish the take: {e}"))?;
    match failure {
        Some(e) => Err(e),
        None if !stop.load(Ordering::Acquire) => Err("the audio input stopped".to_string()),
        None => Ok(frames_out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header of a finished take is exact, and the samples come back as
    /// 16-bit PCM, clamped.
    #[test]
    fn a_wav_round_trips_with_its_header_final() {
        let dir = std::env::temp_dir().join(format!("edith-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.wav");
        let mut writer = WavWriter::create(&path, RATE, CHANNELS).unwrap();
        writer.write(&[0.5, -0.5, 2.0, -2.0]).unwrap();
        let frames = writer.finish(RATE, CHANNELS).unwrap();
        assert_eq!(frames, 2);

        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1, "PCM");
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2, "stereo");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), RATE);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16, "16 bits");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        let (pairs, _) = bytes[44..].as_chunks::<2>();
        let samples: Vec<i16> = pairs.iter().map(|b| i16::from_le_bytes(*b)).collect();
        assert_eq!(samples, vec![16384, -16384, 32767, -32767]);
    }
}
