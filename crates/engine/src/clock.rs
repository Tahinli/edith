//! Playback master clock: the one answer to "what time is it in this timeline?".
//!
//! Audio owns the clock whenever it plays -- the app polls the audio plugin and
//! feeds the sample position in with [`PlaybackClock::set_audio_position`]. With
//! no audio (muted, video-only, past audio EOF) the clock is anchored to an
//! [`Instant`] instead. Both modes are the same timeline, so switching mid-play
//! re-anchors at the current time and never jumps.
//!
//! Granularity contract: in audio mode `now()` steps at the audio quantum, not
//! smoothly. The reported position advances once per 256-1024 frame buffer
//! (5-23 ms at 48 kHz) while the app polls per rendered frame (~16 ms), so two
//! consecutive polls can return the same time and the next can jump by a whole
//! quantum. That is below the app's drift threshold, so it costs nothing today.
//! Smoothing (interpolating with the report's own timestamp) belongs on the
//! audio-plugin side where that timestamp exists, not here.
//!
//! Drift policy -- whether a late frame is dropped or an early one held -- is the
//! app's; this type only reports time.

use std::time::Instant;

/// What drives [`PlaybackClock::now`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockSource {
    /// Externally reported audio sample position.
    Audio,
    /// Elapsed wall time since the last anchor.
    Wall,
}

/// Monotonic-while-playing playback position, in seconds. `Send`, with no
/// threads or I/O of its own: every state change is an explicit call.
pub struct PlaybackClock {
    source: ClockSource,
    playing: bool,
    /// Timeline position of the current anchor. While paused this *is* `now()`.
    base: f64,
    /// Wall anchor; only meaningful while playing in [`ClockSource::Wall`].
    anchor: Instant,
    /// Audio position at the anchor, `None` until the first report after a
    /// re-anchor. Reports are relative to this, so a stream that restarts its
    /// sample counter after a seek re-syncs instead of jumping.
    audio_ref: Option<f64>,
    /// Last accepted report, to clamp a position that runs backwards.
    audio_last: f64,
}

impl PlaybackClock {
    /// A paused clock at t=0.
    pub fn new(source: ClockSource) -> Self {
        Self {
            source,
            playing: false,
            base: 0.0,
            anchor: Instant::now(),
            audio_ref: None,
            audio_last: 0.0,
        }
    }

    pub fn source(&self) -> ClockSource {
        self.source
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Current timeline position in seconds. Frozen exactly while paused, never
    /// decreasing while playing.
    pub fn now(&self) -> f64 {
        if !self.playing {
            return self.base;
        }
        match self.source {
            ClockSource::Wall => self.base + self.anchor.elapsed().as_secs_f64(),
            ClockSource::Audio => match self.audio_ref {
                Some(r) => self.base + (self.audio_last - r),
                None => self.base,
            },
        }
    }

    /// Resumes from wherever `now()` was frozen. No-op if already playing.
    pub fn play(&mut self) {
        if !self.playing {
            self.playing = true;
            self.reanchor(self.base);
        }
    }

    pub fn pause(&mut self) {
        if self.playing {
            self.base = self.now();
            self.playing = false;
        }
    }

    /// Repositions the timeline to `secs`, playing or paused.
    pub fn seek_to(&mut self, secs: f64) {
        self.reanchor(secs);
    }

    /// Audio EOF: keeps the clock running off wall time from the current
    /// position, so `now()` is continuous across the switch.
    pub fn switch_to_wall(&mut self) {
        self.reanchor(self.now());
        self.source = ClockSource::Wall;
    }

    /// Hands the clock back to audio from the current position. The next
    /// reported sample position becomes the new reference whatever its value.
    pub fn switch_to_audio(&mut self) {
        self.reanchor(self.now());
        self.source = ClockSource::Audio;
    }

    /// The app's audio position poll. Ignored in wall mode, and ignored when it
    /// runs backwards (a device reset restarts the counter) so playback time
    /// never rewinds under the renderer; the app re-syncs with `seek_to` or
    /// `switch_to_wall`.
    pub fn set_audio_position(&mut self, samples_played: u64, sample_rate: u32) {
        if sample_rate == 0 {
            return;
        }
        let secs = samples_played as f64 / sample_rate as f64;
        match self.audio_ref {
            None => {
                self.audio_ref = Some(secs);
                self.audio_last = secs;
            }
            Some(_) if secs > self.audio_last => self.audio_last = secs,
            Some(_) => {}
        }
    }

    /// Makes `base` the current position and drops both source references, so
    /// the next wall tick or audio report continues from there.
    fn reanchor(&mut self, secs: f64) {
        self.base = secs;
        self.anchor = Instant::now();
        self.audio_ref = None;
        self.audio_last = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    const RATE: u32 = 48_000;

    /// The app moves the clock between threads/closures.
    fn _assert_send<T: Send>() {}
    const _: fn() = || _assert_send::<PlaybackClock>();

    fn audio_playing() -> PlaybackClock {
        let mut c = PlaybackClock::new(ClockSource::Audio);
        c.play();
        c
    }

    #[test]
    fn audio_follows_reports_exactly() {
        let mut c = audio_playing();
        assert_eq!(c.now(), 0.0);
        c.set_audio_position(0, RATE); // first report sets the reference
        assert_eq!(c.now(), 0.0);
        c.set_audio_position(48_000, RATE);
        assert_eq!(c.now(), 1.0);
        c.set_audio_position(72_000, RATE);
        assert_eq!(c.now(), 1.5);
    }

    #[test]
    fn audio_reference_is_relative_not_absolute() {
        // A stream already 10 s in still starts this clock's timeline at 0.
        let mut c = audio_playing();
        c.set_audio_position(480_000, RATE);
        assert_eq!(c.now(), 0.0);
        c.set_audio_position(504_000, RATE);
        assert_eq!(c.now(), 0.5);
    }

    #[test]
    fn audio_regression_clamps() {
        let mut c = audio_playing();
        c.set_audio_position(0, RATE);
        c.set_audio_position(96_000, RATE);
        assert_eq!(c.now(), 2.0);
        c.set_audio_position(24_000, RATE); // device reset
        assert_eq!(c.now(), 2.0, "clock must not rewind");
        c.set_audio_position(0, RATE);
        assert_eq!(c.now(), 2.0);
        c.set_audio_position(120_000, RATE); // recovered, forward again
        assert_eq!(c.now(), 2.5);
    }

    #[test]
    fn zero_sample_rate_ignored() {
        let mut c = audio_playing();
        c.set_audio_position(1_000, 0);
        assert_eq!(c.now(), 0.0);
    }

    #[test]
    fn pause_freezes_and_play_resumes() {
        let mut c = audio_playing();
        c.set_audio_position(0, RATE);
        c.set_audio_position(48_000, RATE);
        c.pause();
        assert!(!c.is_playing());
        assert_eq!(c.now(), 1.0);
        // Audio keeps running for a moment after a pause request; ignored.
        c.set_audio_position(60_000, RATE);
        assert_eq!(c.now(), 1.0);
        c.play();
        assert_eq!(c.now(), 1.0, "no time gained across the pause");
        c.set_audio_position(72_000, RATE); // stream restarted mid-buffer
        assert_eq!(c.now(), 1.0);
        c.set_audio_position(96_000, RATE);
        assert_eq!(c.now(), 1.5);
    }

    #[test]
    fn wall_advances_with_real_time() {
        let mut c = PlaybackClock::new(ClockSource::Wall);
        c.play();
        sleep(Duration::from_millis(120));
        let t = c.now();
        assert!((0.05..0.60).contains(&t), "wall clock at {t}");
        c.pause();
        let frozen = c.now();
        sleep(Duration::from_millis(120));
        assert_eq!(c.now(), frozen, "paused wall clock must not move");
        c.play();
        assert!(c.now() - frozen < 0.05);
        sleep(Duration::from_millis(120));
        assert!(c.now() > frozen + 0.05);
    }

    #[test]
    fn switch_to_wall_is_continuous() {
        let mut c = audio_playing();
        c.set_audio_position(0, RATE);
        c.set_audio_position(96_000, RATE);
        let x = c.now();
        c.switch_to_wall();
        assert_eq!(c.source(), ClockSource::Wall);
        assert!((c.now() - x).abs() < 0.01, "jumped from {x} to {}", c.now());
        sleep(Duration::from_millis(120));
        assert!(c.now() > x + 0.05, "wall clock did not take over");
    }

    #[test]
    fn switch_to_audio_is_continuous() {
        let mut c = PlaybackClock::new(ClockSource::Wall);
        c.play();
        sleep(Duration::from_millis(50));
        let before = c.now();
        c.switch_to_audio();
        let x = c.now();
        assert!((x - before).abs() < 0.01, "jumped from {before} to {x}");
        assert_eq!(c.now(), x, "audio mode holds until the first report");
        c.set_audio_position(240_000, RATE);
        assert_eq!(c.now(), x);
        c.set_audio_position(264_000, RATE);
        assert_eq!(c.now(), x + 0.5);
    }

    #[test]
    fn seek_while_paused() {
        let mut c = PlaybackClock::new(ClockSource::Wall);
        c.seek_to(12.0);
        assert_eq!(c.now(), 12.0);
        sleep(Duration::from_millis(50));
        assert_eq!(c.now(), 12.0, "seek must not start the clock");
        c.play();
        sleep(Duration::from_millis(120));
        let t = c.now();
        assert!((12.05..12.60).contains(&t), "resumed at {t}");
    }

    #[test]
    fn seek_while_playing_audio() {
        let mut c = audio_playing();
        c.set_audio_position(0, RATE);
        c.set_audio_position(48_000, RATE);
        c.seek_to(30.0);
        assert_eq!(c.now(), 30.0);
        // The app restarts the stream, so the counter comes back from zero.
        c.set_audio_position(0, RATE);
        assert_eq!(c.now(), 30.0);
        c.set_audio_position(24_000, RATE);
        assert_eq!(c.now(), 30.5);
        assert!(c.is_playing());
    }

    #[test]
    fn seek_while_playing_wall() {
        let mut c = PlaybackClock::new(ClockSource::Wall);
        c.play();
        sleep(Duration::from_millis(50));
        c.seek_to(5.0);
        assert!((c.now() - 5.0).abs() < 0.01);
        sleep(Duration::from_millis(120));
        assert!(c.now() > 5.05);
    }
}
