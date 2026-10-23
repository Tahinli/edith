//! Syncing the cameras of a multicam clip by their sound.
//!
//! Every camera hears the same room, so the loudness curves of their tracks have the same shape,
//! shifted by the distance between the moments the cameras started rolling. [`best_lag`] finds
//! that shift by normalised cross-correlation of the two curves (the dB levels of
//! [`crate::silence::levels`], 50 a second, with the slow trend taken out), which is accurate to
//! one 20 ms window and needs no more than a clap, a voice or a song both cameras heard.

use crate::silence::WINDOWS_PER_SEC;

/// How much of each track's start is listened to, in seconds.
pub const SYNC_SECS: f64 = 240.0;

/// The furthest apart two cameras' starts may be, in seconds.
pub const MAX_LAG_SECS: f64 = 120.0;

/// A match weaker than this normalised correlation is not believed.
pub const MIN_SCORE: f32 = 0.2;

/// The shortest overlap, in seconds, a lag is judged on.
const MIN_OVERLAP_SECS: f64 = 3.0;

/// The lowest level that counts, in dB: below it a track is silence.
const FLOOR_DB: f32 = -70.0;

/// Where `other` sits against `reference`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lag {
    /// Seconds later than in the reference that the same sound happens in the other track: the
    /// other camera started this much earlier when it is positive.
    pub secs: f64,
    /// The normalised correlation at that lag, 0 to 1.
    pub score: f32,
}

/// The loudness curve with its trend (a one-second running mean) removed and silence clamped, so
/// that only the events are left to match.
fn events(levels: &[f32]) -> Vec<f32> {
    let v: Vec<f32> = levels.iter().map(|l| l.clamp(FLOOR_DB, 0.0)).collect();
    let half = (WINDOWS_PER_SEC / 2) as usize;
    let mut prefix = vec![0.0f64; v.len() + 1];
    for (i, x) in v.iter().enumerate() {
        prefix[i + 1] = prefix[i] + f64::from(*x);
    }
    (0..v.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(v.len()));
            v[i] - ((prefix[b] - prefix[a]) / (b - a) as f64) as f32
        })
        .collect()
}

/// The lag of `other` against `reference`, both loudness curves in dB at [`WINDOWS_PER_SEC`], within
/// `max_lag_secs`. `None` when either is too short or silent to judge.
pub fn best_lag(reference: &[f32], other: &[f32], max_lag_secs: f64) -> Option<Lag> {
    let (x, y) = (events(reference), events(other));
    let rate = f64::from(WINDOWS_PER_SEC);
    let min_overlap = ((MIN_OVERLAP_SECS * rate) as usize).min(x.len().min(y.len()) / 2).max(1);
    if x.len().min(y.len()) < 10 {
        return None;
    }
    let sq = |v: &[f32]| {
        let mut p = vec![0.0f64; v.len() + 1];
        for (i, a) in v.iter().enumerate() {
            p[i + 1] = p[i] + f64::from(*a) * f64::from(*a);
        }
        p
    };
    let (px, py) = (sq(&x), sq(&y));
    let max_lag = (max_lag_secs * rate) as i64;
    let mut best: Option<(i64, f64)> = None;
    for lag in -max_lag.min(x.len() as i64)..=max_lag.min(y.len() as i64) {
        // x[i] pairs with y[i + lag].
        let lo = (-lag).max(0) as usize;
        let hi = (x.len() as i64).min(y.len() as i64 - lag);
        if hi <= lo as i64 {
            continue;
        }
        let hi = hi as usize;
        if hi - lo < min_overlap {
            continue;
        }
        let dot: f64 = x[lo..hi].iter().zip(&y[(lo as i64 + lag) as usize..]).map(|(a, b)| f64::from(*a) * f64::from(*b)).sum();
        let ex = px[hi] - px[lo];
        let ey = py[(hi as i64 + lag) as usize] - py[(lo as i64 + lag) as usize];
        if ex <= 1e-9 || ey <= 1e-9 {
            continue;
        }
        let score = dot / (ex * ey).sqrt();
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((lag, score));
        }
    }
    best.map(|(lag, score)| Lag { secs: lag as f64 / rate, score: score.clamp(0.0, 1.0) as f32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A track of `secs` seconds: a quiet room and a burst of `len` seconds from `at` (dB levels).
    fn burst(secs: f64, at: f64, len: f64) -> Vec<f32> {
        let rate = f64::from(WINDOWS_PER_SEC);
        (0..(secs * rate) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                match t >= at && t < at + len {
                    true => -12.0,
                    false => -65.0 - ((i * 7) % 5) as f32,
                }
            })
            .collect()
    }

    #[test]
    fn a_burst_delayed_by_one_point_three_seconds_is_found() {
        let a = burst(30.0, 4.0, 0.6);
        let b = burst(30.0, 5.3, 0.6);
        let lag = best_lag(&a, &b, 20.0).expect("a lag");
        assert!((lag.secs - 1.3).abs() < 0.021, "{lag:?}");
        assert!(lag.score > 0.9, "{lag:?}");
        let back = best_lag(&b, &a, 20.0).expect("a lag");
        assert!((back.secs + 1.3).abs() < 0.021, "{back:?}");
    }

    #[test]
    fn a_louder_or_noisier_copy_still_lines_up() {
        let a = burst(40.0, 6.0, 0.4);
        let mut b: Vec<f32> = burst(40.0, 3.0, 0.4).iter().map(|l| l + 9.0).collect();
        b.truncate(30 * WINDOWS_PER_SEC as usize);
        let lag = best_lag(&a, &b, 30.0).expect("a lag");
        assert!((lag.secs + 3.0).abs() < 0.021, "{lag:?}");
    }

    #[test]
    fn silence_and_stubs_are_not_matched() {
        let quiet = vec![-120.0; 3000];
        assert!(best_lag(&quiet, &quiet, 10.0).is_none());
        assert!(best_lag(&[0.0; 4], &[0.0; 4], 10.0).is_none());
    }
}
