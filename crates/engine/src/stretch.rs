//! Time-stretching that keeps the pitch: WSOLA, hand-rolled.
//!
//! A speeded clip used to be resampled -- the tape effect, its pitch moving
//! with its rate. The rate conversion (a source written at another sample
//! rate than the timeline's) is still a resample and still is, because
//! reading frames at the ratio of two rates is pitch-true by construction.
//! What [`crate::project::Speed`] asks for is *time* alone, and that is this
//! module's: the waveform is cut into overlapping windows and re-assembled at
//! a different hop, each window placed where it best continues the one before
//! it (Waveform Similarity Overlap-add, Verhelst & Roelands 1993), so the
//! timeline compresses or stretches while every period of the underlying
//! signal keeps its own length -- the words, the notes and the beeps stay at
//! the frequency they were recorded at.
//!
//! The one consumer is [`crate::audio`]'s emit path, after the rate
//! conversion and before the equalizer, exactly where the resample used to
//! sit; see [`TimeStretch`] for the buffering contract it keeps there.

/// Analysis window, in frames: long enough that a window holds several
/// periods of anything a person calls a pitch (at 48 kHz, 1024 frames is
/// 21 ms -- four periods of a 187 Hz tone), short enough that a placement
/// mistake cannot smear a consonant.
const N: usize = 1024;
/// Synthesis hop, in frames: half the window, which is what makes the Hann
/// windows of consecutive frames sum to one everywhere -- the overlap-add
/// neither adds gain nor leaves a seam.
const HS: usize = N / 2;
/// How far either side of the nominal position a window may be placed to find
/// the best continuation: half a window again, which is the classic WSOLA
/// compromise between finding the good overlap and keeping the search cheap
/// (it is O(N·SEARCH) per frame, and this keeps it under a million adds).
const SEARCH: usize = N / 2;

/// One speeded segment's time-stretch: interleaved f32 in, interleaved f32
/// out, at the timeline's own sample rate and channel count, paced to the
/// frames the timeline promised the segment (`owed` -- the A/V sync
/// invariant, which the device and an export both read).
///
/// WSOLA in one breath: windows of [`N`] frames are overlap-added at
/// [`HS`]-frame hops, read from the input at [`Ha`] = `HS · speed`-frame hops.
/// Each window after the first is shifted within ±[`SEARCH`] of its nominal
/// read position to the offset whose first `HS` frames best match the
/// *natural continuation* of the window before it -- the input that followed
/// that window's own read position -- measured by normalized cross-correlation
/// on the first channel, and the one offset is applied to every channel so a
/// stereo pair stays coherent. The output is the input at the timeline's rate
/// with its pitch kept: `speed` frames of source per frame of output, to the
/// frame.
///
/// The contract with the emit path is [`Resample`](crate::audio)'s own:
/// [`process`](Self::process) rewrites a decoded buffer as whatever output
/// frames are ready (an empty one is fine -- the first window only completes
/// once `N + SEARCH` frames are buffered, and a caller's buffers are packet
/// sized), and [`flush`](Self::flush) pays whatever the input's end left
/// owed, in silence, so a segment lands on its promise exactly.
pub(crate) struct TimeStretch {
    /// Analysis hop: how far the read position advances per synthesized
    /// window. `HS · speed`, rounded once here so it never rounds again --
    /// the speed itself is spent on this number and nowhere else.
    ha: usize,
    channels: usize,
    /// Output frames the timeline was promised and has not been paid. Paced
    /// to exactly, by truncation and by silence, for [`Resample`]'s reason:
    /// the picture's clip boundaries are exact, and this keeps the sound's
    /// exact with them.
    owed: u64,
    hann: Vec<f32>,
    /// Input not yet consumed, interleaved. `base` is the frame index of its
    /// first frame within the segment's whole stream, so the analysis
    /// positions below stay absolute -- and survive the buffer compaction
    /// that keeps this from growing without bound.
    input: Vec<f32>,
    base: usize,
    /// The nominal read position of the next window, in stream frames. Starts
    /// at 0 and advances by `ha`.
    anext: usize,
    /// The read position the last window actually used, for the natural
    /// continuation the next one is matched against. `None` until one window
    /// has been placed -- the first has nothing to match and takes its
    /// nominal position as-is.
    prev: Option<usize>,
    /// The overlap-add accumulator: the tail of the last window that the next
    /// one completes, interleaved, always `N - HS` frames.
    overlap: Vec<f32>,
    /// Whether any window has been synthesized -- a segment whose whole
    /// source is shorter than a window never primes, and is passed through
    /// as the decoder read it, padded to its promise in silence: the honest
    /// degenerate, rather than a stretched half-window of nothing.
    started: bool,
}

impl TimeStretch {
    /// `speed` is source frames per output frame, as [`crate::project::Stretch`]
    /// states it; `owed` is the segment's whole promise in output frames, the
    /// same number [`crate::audio::Resample`] paces to when it works alone.
    pub(crate) fn new(speed: f64, owed: u64, channels: usize) -> Self {
        // The classic Hann, in the half-open form the overlap-add wants: 0 at
        // either end, so a window never snaps the seam it is pasting over.
        let hann = (0..N)
            .map(|i| {
                (std::f64::consts::PI * i as f64 / N as f64).sin().powi(2) as f32
            })
            .collect();
        Self {
            ha: (HS as f64 * speed).round().max(1.) as usize,
            channels,
            owed,
            hann,
            input: Vec::new(),
            base: 0,
            anext: 0,
            prev: None,
            overlap: vec![0.; (N - HS) * channels],
            started: false,
        }
    }

    /// Frames of input the stretcher holds, stream-indexed -- every question
    /// below is asked in stream frames and answered against this.
    fn has(&self, frame: usize) -> bool {
        frame < self.base + self.input.len() / self.channels
    }

    /// The first channel's frames `[at, at + HS)` as the correlation target,
    /// zero-padded past the input's end -- the flush path reads past what was
    /// decoded, and a target of zeros simply stops favouring any offset.
    fn natural(&self, at: usize) -> Vec<f32> {
        (0..HS)
            .map(|i| {
                self.has(at + i)
                    .then(|| {
                        self.input[((at + i - self.base) * self.channels)..][0]
                    })
                    .unwrap_or(0.)
            })
            .collect()
    }

    /// Normalized cross-correlation of the first channel at `[at, at + HS)`
    /// against `target`: 1 for a perfect match, 0 for nothing in common,
    /// amplitude-free so a loud passage does not win on loudness alone.
    fn match_at(&self, at: usize, target: &[f32]) -> f32 {
        let (mut dot, mut ea, mut eb) = (0f32, 0f32, 0f32);
        for i in 0..HS {
            let a = self.has(at + i)
                .then(|| self.input[((at + i - self.base) * self.channels)..][0])
                .unwrap_or(0.);
            let b = target[i];
            dot += a * b;
            ea += a * a;
            eb += b * b;
        }
        dot / (ea.sqrt() * eb.sqrt() + 1e-9)
    }

    /// Overlap-adds one window read at `at` and hands back the `HS` output
    /// frames it completes: the accumulator's head, released, with the
    /// window's contribution added to what the last one left behind. The
    /// Hann hops at half its width, so the two contributions over any
    /// completed frame sum to one and the windowing leaves no trace of
    /// itself.
    fn place(&mut self, at: usize) -> Vec<f32> {
        let mut out = std::mem::take(&mut self.overlap);
        out.resize(N * self.channels, 0.);
        for i in 0..N {
            let w = self.hann[i];
            let src = if self.has(at + i) {
                &self.input[((at + i - self.base) * self.channels)..][..self.channels]
            } else {
                &[0., 0.][..self.channels.min(2)]
            };
            for c in 0..self.channels {
                out[i * self.channels + c] += src.get(c).copied().unwrap_or(0.) * w;
            }
        }
        let done = out[..HS * self.channels].to_vec();
        self.overlap = out[HS * self.channels..].to_vec();
        self.overlap
            .resize((N - HS) * self.channels, 0.);
        done
    }

    /// Whether another window can be placed: its whole search range and its
    /// whole window must be buffered, and so must the natural continuation
    /// the *next* window is matched against -- one look-ahead window of
    /// latency, which is what the caller's empty emits ride out.
    fn ready(&self) -> bool {
        let have = self.base + self.input.len() / self.channels;
        self.anext + SEARCH + N + HS <= have
    }

    /// Rewrites `buf` as the output frames this stretch has ready for it --
    /// possibly none (see the struct doc). Consumes the input, compacts what
    /// is left, and never returns more than the segment is owed.
    pub(crate) fn process(&mut self, buf: &mut Vec<f32>, channels: usize) {
        debug_assert_eq!(channels, self.channels, "the layout is fixed at open");
        self.input.append(buf);
        let mut out: Vec<f32> = Vec::new();
        while self.owed > 0 && self.ready() {
            let want = self
                .anext
                .saturating_sub(SEARCH)
                .max(self.prev.map_or(0, |p| p + HS - SEARCH));
            // The best continuation in the search range: every offset is
            let at = match self.prev {
                None => self.anext,
                Some(p) => {
                    let target = self.natural(p + HS);
                    // A silent continuation says nothing about where the next
                    // window belongs: every offset matches it equally (which
                    // is to say not at all), and the search would pick one on
                    // rounding noise. The nominal position is the honest
                    // answer there -- silence continued is silence, and the
                    // burst that follows starts where the timeline says.
                    let loud = target.iter().fold(0f32, |m, x| m + x * x);
                    if loud < 1e-4 {
                        self.anext
                    } else {
                        // Scored once each and argmax'd after: a comparator
                        // that re-measured both sides of every comparison is
                        // a search squared, and this search already is the
                        // module's cost.
                        (want..=self.anext + SEARCH)
                            .map(|at| (self.match_at(at, &target), at))
                            .max_by(|(a, _), (b, _)| a.total_cmp(b))
                            .map_or(self.anext, |(_, at)| at)
                    }
                }
            };
            // Never past the promise: the last window a segment can pay for
            // may be a partial one, and the frames it still owes -- not the
            // hop a full window would have completed -- are what leaves here.
            let frames = self.place(at);
            let pays = frames.len().min(self.owed as usize * self.channels);
            out.extend_from_slice(&frames[..pays]);
            self.owed -= (pays / self.channels) as u64;
            self.prev = Some(at);
            self.anext += self.ha;
            self.started = true;
        }
        // Consumed input is everything before the next window's search range
        // and before the continuation it is matched against -- either may be
        // the earlier one.
        let keep = self
            .anext
            .saturating_sub(SEARCH)
            .min(self.prev.map_or(usize::MAX, |p| p + HS));
        let drop = (keep.saturating_sub(self.base)).min(self.input.len() / self.channels);
        self.input.drain(..drop * self.channels);
        self.base += drop;
        *buf = out;
    }

    /// The end of the segment: whatever windows the buffered input still
    /// completes are placed at their nominal positions -- no search, because
    /// the offsets the search would pick from are half padding -- and what
    /// the promise still lacks is paid in silence. Pads or truncates to
    /// `owed` exactly, for the reason every flush in this path does: the
    /// device's seconds are the timeline's seconds, and a segment that lands
    /// one frame long is a segment every later segment is one frame late
    /// against. `false` means the consumer went away.
    pub(crate) fn flush(
        &mut self,
        channels: usize,
        timeline: &mut u64,
        tx: &std::sync::mpsc::SyncSender<crate::audio::AudioChunk>,
    ) -> bool {
        let mut send = |frames: Vec<f32>| -> bool {
            if frames.is_empty() {
                return true;
            }
            let chunk = crate::audio::AudioChunk {
                start_sample: *timeline,
                samples: frames,
            };
            *timeline += (chunk.samples.len() / channels) as u64;
            tx.send(chunk).is_ok()
        };
        // A segment too short to ever prime: the decoder's own frames, as
        // they came -- the honest degenerate -- and the padding below pays
        // the rest of the promise.
        if !self.started && self.input.len() / self.channels < N + SEARCH {
            let mut rest = std::mem::take(&mut self.input);
            let owed = (self.owed as usize).min(usize::MAX / channels) * channels;
            if rest.len() > owed {
                rest.truncate(owed);
            }
            self.owed -= (rest.len() / channels) as u64;
            if !send(rest) {
                return false;
            }
        } else {
            // The windows the buffer still completes, placed nominally; the
            // zero-padding in `place` finishes the last of them.
            while self.owed > 0 && self.anext < self.base + self.input.len() / self.channels + N {
                let frames = self.place(self.anext);
                self.anext += self.ha;
                let pays = (self.owed as usize).min(frames.len() / channels) * channels;
                self.owed -= (pays / channels) as u64;
                if !send(frames[..pays].to_vec()) {
                    return false;
                }
            }
        }
        // The promise, in silence, to the frame.
        while self.owed > 0 {
            let frames = (self.owed as usize).min(crate::audio::SAMPLES_PER_PACKET as usize);
            if !send(vec![0.; frames * channels]) {
                return false;
            }
            self.owed -= frames as u64;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a stretcher over a whole signal the way the emit path does: one
    /// packet-shaped buffer at a time, whatever comes out collected, then the
    fn stretched(speed: f64, owed: u64, samples: &[f32]) -> Vec<f32> {
        let mut s = TimeStretch::new(speed, owed, 1);
        let mut out = Vec::new();
        for chunk in samples.chunks(1024) {
            let mut buf = chunk.to_vec();
            s.process(&mut buf, 1);
            out.extend_from_slice(&buf);
        }
        let before = out.len();
        // Drained on its own thread, the way the device drains the worker in
        // the real path: the channel is four chunks deep, and a flush that
        // owes more silence than that would block on a receiver nobody is
        // reading yet.
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let drained = std::thread::spawn(move || {
            rx.into_iter()
                .map(|c: crate::audio::AudioChunk| (c.start_sample, c.samples))
                .collect::<Vec<_>>()
        });
        let mut timeline = 0;
        s.flush(1, &mut timeline, &tx);
        drop(tx);
        let mut paid = 0;
        for (_, frames) in drained.join().unwrap() {
            paid += frames.len();
            out.extend_from_slice(&frames);
        }
        assert_eq!(
            before + paid,
            owed as usize,
            "the promise is paid to the frame"
        );
        // The promise is what the two halves paid together: the flush's own
        // count is only its tail, and a segment that primed early paid most
        // of its way on the process path.
        assert_eq!(
            before + timeline as usize,
            owed as usize,
            "the promise is paid to the frame"
        );
        out
    }

    /// The amplitude of `hz` inside `samples`, by Goertzel at 48 kHz: the
    /// dominant-frequency meter these tests assert pitch with, rather than
    /// trusting that a stretch *sounds* unchanged.
    fn amplitude(samples: &[f32], hz: f64) -> f64 {
        let n = samples.len() as f64;
        let w = 2. * std::f64::consts::PI * hz / 48_000.;
        let coeff = 2. * w.cos();
        let (mut s1, mut s2) = (0., 0.);
        for &x in samples {
            let s0 = f64::from(x) + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        2. * (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt() / n
    }

    /// Two seconds of a 440 Hz sine at 48 kHz, one channel: the plain,
    /// pitch-carrying signal every assertion below stretches.
    fn sine(hz: f64, secs: f64) -> Vec<f32> {
        (0..(secs * 48_000.) as usize)
            .map(|i| (2. * std::f64::consts::PI * hz * i as f64 / 48_000.).sin() as f32)
            .collect()
    }


    #[test]
    fn a_2x_stretch_halves_the_time_and_keeps_the_pitch() {
        let input = sine(440., 2.);
        let out = stretched(2.0, 48_000, &input);
        // The promise is the timeline's: one second out for two in.
        assert_eq!(out.len(), 48_000, "owed exactly");
        // Pitch, measured: 440 Hz is the loudest thing in the output by far,
        // and the octave the tape effect would have put there (880) is not.
        let keep = amplitude(&out, 440.);
        let octave = amplitude(&out, 880.);
        assert!(keep > 0.4, "440 Hz survives at {keep}");
        assert!(octave < keep / 4., "the tape-effect octave is gone: {octave} vs {keep}");
    }

    #[test]
    fn a_half_speed_stretch_doubles_the_time_and_keeps_the_pitch() {
        let input = sine(440., 1.);
        let out = stretched(0.5, 96_000, &input);
        assert_eq!(out.len(), 96_000, "owed exactly");
        let keep = amplitude(&out, 440.);
        let sub = amplitude(&out, 220.);
        assert!(keep > 0.4, "440 Hz survives at {keep}");
        assert!(sub < keep / 4., "the tape-effect sub-octave is gone: {sub} vs {keep}");
    }

    #[test]
    fn a_stereo_pair_keeps_its_channels_in_step() {
        // Left is the 440 Hz sine, right the same one panned down: a stereo
        // pair whose channels differ only in level, which is what a real
        // image is made of. The stretch's one search offset for both
        // channels is what keeps the pair from smearing, and the two halves
        // staying in step -- the cross-correlation of right against left
        // peaking at lag zero, both still the tone they went in as -- is what
        // says it did.
        let left = sine(440., 2.);
        let right: Vec<f32> = left.iter().map(|&x| x * 0.7).collect();
        let input: Vec<f32> = left
            .iter()
            .zip(&right)
            .flat_map(|(&l, &r)| [l, r])
            .collect();
        let mut s = TimeStretch::new(2.0, 48_000, 2);
        let mut out = Vec::new();
        for chunk in input.chunks(2048) {
            let mut buf = chunk.to_vec();
            s.process(&mut buf, 2);
            out.extend_from_slice(&buf);
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let drained = std::thread::spawn(move || {
            rx.into_iter()
                .flat_map(|c: crate::audio::AudioChunk| c.samples)
                .collect::<Vec<_>>()
        });
        let mut timeline = 0;
        s.flush(2, &mut timeline, &tx);
        drop(tx);
        out.extend(drained.join().unwrap());
        assert_eq!(out.len(), 96_000);
        let l: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        let r: Vec<f32> = out.chunks(2).map(|f| f[1]).collect();
        assert!(amplitude(&l, 440.) > 0.4, "left keeps its tone");
        assert!(amplitude(&r, 440.) > 0.25, "right keeps the same one, panned");
        // In step: the correlation of right against left is greatest at lag
        // zero -- a channel smeared to its own timings peaks somewhere else.
        let at = |lag: i32| -> f32 {
            let n = l.len() as i32;
            let (mut dot, mut ea, mut eb) = (0f32, 0f32, 0f32);
            for i in 0..n {
                let j = i + lag;
                if j < 0 || j >= n {
                    continue;
                }
                dot += l[i as usize] * r[j as usize];
                ea += l[i as usize] * l[i as usize];
                eb += r[j as usize] * r[j as usize];
            }
            dot / (ea.sqrt() * eb.sqrt() + 1e-9)
        };
        let zero = at(0);
        let worst = (-256..=256)
            .filter(|&lag| lag != 0)
            .map(at)
            .fold(0f32, f32::max);
        assert!(
            zero > worst,
            "the pair is in step: lag 0 at {zero}, best other {worst}"
        );
    }

    #[test]
    fn a_segment_shorter_than_a_window_passes_through_honestly() {
        // 600 frames is under the prime (N + SEARCH): no window ever
        // completes, and the decoder's own frames come out padded to the
        // promise in silence -- degenerate, and exactly what it owes.
        let input = sine(440., 600. / 48_000.);
        let out = stretched(2.0, 400, &input);
        assert_eq!(out.len(), 400);
        assert_eq!(out[..600.min(400)], input[..600.min(400)], "the frames as they were");
    }

    #[test]
    fn the_promise_is_padded_when_the_source_runs_short() {
        // A window's worth of input for two seconds of promise: whatever the
        // stretch makes of it, the rest is silence, to the frame.
        let input = sine(440., 0.02);
        let out = stretched(2.0, 96_000, &input);
        assert_eq!(out.len(), 96_000, "owed exactly");
        let tail = &out[48_000..];
        assert!(tail.iter().all(|&x| x == 0.), "the shortfall is silence");
    }
}
