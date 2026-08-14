//! Where the transport is, how loud it is, and the seek rate limits.

use crate::*;

/// What the header says with no timeline open, and what the window title reads
/// as a program name rather than as a file name.
pub(crate) const NO_FILE: &str = "no file open";

/// What a press of play says when there is nothing to play: no timeline at all
/// and an emptied one are the same answer to the user, so they are one line.
pub(crate) const NOTHING_TO_PLAY: &str = "NOTHING TO PLAY — put a clip on the timeline first";

/// Whether a press of play would have anything to play. No timeline at all and
/// one every clip has been taken off are the same state to a transport, and the
/// button and the key both have to give the same answer to it -- so there is
/// one of them, and it is free of the window so it can be checked without one.
pub(crate) fn nothing_to_play(session: Option<&PlaybackSession>) -> bool {
    session.is_none_or(PlaybackSession::is_empty)
}

/// What the monitoring output is set to. Two things, not one: the level the
/// user picked, and whether it is being held silent -- so unmuting comes back
/// to the level rather than to a guess.
///
/// The level counts steps rather than carrying an `f32`, because 5% at a time
/// down and back up again through a float would not land on the number it
/// started from, and the label would eventually read `79%`.
///
/// Volume and mute stay independent on purpose: turning the level down while
/// muted must not be what makes sound come out. Only the mute key unmutes, and
/// the button says both things at once ("Muted 80%") so neither is a surprise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Volume {
    pub(crate) steps: u8,
    pub(crate) muted: bool,
}

impl Volume {
    /// One step is one percent: fine enough that a drag along the slider reads
    /// as continuous, and still a count rather than a float.
    pub(crate) const MAX_STEPS: u8 = 100;

    /// 5% a press: twenty presses across the range, which is what the keys have
    /// always moved. The slider is what the finer grid is for.
    pub(crate) const KEY_STEP: u8 = 5;

    /// What the device is set to: mute wins, and the level is what it returns
    /// to. `0.0..=1.0`, which is the range the plugin's ABI accepts.
    pub(crate) fn gain(self) -> f32 {
        if self.muted { 0. } else { self.along() }
    }

    /// One press up or down, clamped at both ends -- saturating, so the count
    /// cannot wrap past silence into full volume.
    pub(crate) fn step(&mut self, up: bool) {
        self.steps = if up {
            self.steps.saturating_add(Self::KEY_STEP).min(Self::MAX_STEPS)
        } else {
            self.steps.saturating_sub(Self::KEY_STEP)
        };
    }

    /// Where the hand let go along the slider, 0..1 from silence to full. The
    /// grid is the same one the keys land on, so a drag to the top and a key
    /// held up reach the very same number -- and a drag never touches mute:
    /// asking for a level while muted is not asking for sound.
    pub(crate) fn set_along(&mut self, frac: f32) {
        self.steps = (frac.clamp(0., 1.) * f32::from(Self::MAX_STEPS)).round() as u8;
    }

    /// How full the slider is drawn, 0..1. The level and not the gain: a muted
    /// slider still shows what unmuting comes back to, exactly as the label
    /// does.
    pub(crate) fn along(self) -> f32 {
        f32::from(self.steps) / f32::from(Self::MAX_STEPS)
    }

    /// The level as a whole number, for the button's fixed rect: muting swaps
    /// the glyph beside it, never the width of the box.
    pub(crate) fn percent(self) -> u32 {
        u32::from(self.steps) * 100 / u32::from(Self::MAX_STEPS)
    }

    /// What the button read before the mute state became a glyph and a colour
    /// ([`Player::toolbar`]): the guards still hold the wording to it, so it
    /// sits with them rather than in the binary.
    #[cfg(test)]
    pub(crate) fn label(self) -> String {
        let percent = u32::from(self.steps) * 100 / u32::from(Self::MAX_STEPS);
        if self.muted {
            format!("Muted {percent}%")
        } else {
            format!("Vol {percent}%")
        }
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            steps: Self::MAX_STEPS,
            muted: false,
        }
    }
}

/// Where the transport is. The one answer the button's glyph, its label, its
/// enablement, the play key and the repaint loop all read -- there is no play
/// flag anywhere else, because a second one is a second answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Transport {
    /// No timeline open. Nothing to play and the transport is dimmed.
    Stopped,
    Playing,
    Paused,
    /// Played out: the last frame is on screen, the decoder is finished, and the
    /// clock is still running past it -- which is exactly why "is the clock
    /// going" is not the same question as "is this playing".
    Ended,
}

impl Transport {
    /// The timeline is in motion: two bars on the button, and a repaint owed
    /// every vsync.
    pub(crate) fn is_playing(self) -> bool {
        matches!(self, Transport::Playing)
    }

    /// The play key and the transport button start over from the top rather
    /// than toggling -- the end of a timeline is where every NLE does this, and
    /// the button does it because the key already did.
    pub(crate) fn restarts(self) -> bool {
        matches!(self, Transport::Ended)
    }
}

/// What a session's own two answers mean: the clock, unless the timeline has
/// been played out -- `played_out` is the engine's end of stream with no frame
/// still waiting on the pump, and it wins, because past the end a running clock
/// is measuring wall time and not a picture.
pub(crate) fn transport(playing: bool, played_out: bool) -> Transport {
    match (played_out, playing) {
        (true, _) => Transport::Ended,
        (_, true) => Transport::Playing,
        (_, false) => Transport::Paused,
    }
}

/// Rate limit for scrub seeks: a video worker reopen costs 72-87 ms on the
/// hardware path for the small files it was measured on (215 ms in software), so
/// one seek per mouse move would only queue workers that are cancelled before
/// they decode anything.
///
/// It is a *floor*, not a bound: the reopen is a demux open
/// ([`engine::decode::open_worker`]), and on a 25 GB film that is 550-750 ms --
/// five to seven times this gap, which therefore gates nothing there. Where the
/// cost had to be bounded rather than thinned -- the colour and speed drags --
/// the gate is the frame the worker delivers ([`Player::flush_drag`]) and no
/// timer at all. The ruler keeps this one: a scrub has no value to hold back,
/// only a position that the next mouse move replaces anyway.
pub(crate) const SCRUB_GAP: Duration = Duration::from_millis(100);

pub(crate) fn scrub_due(target: u32, last_target: u32, since: Duration) -> bool {
    target != last_target && since >= SCRUB_GAP
}

/// How far the picture may fall behind the sound before it is restarted at the
/// clock instead of left to crawl after it. Past what an eye reads as lip sync
/// (a tenth of a second or so) and above what a single reopen costs to fix, so a
/// picture that is merely a reopen behind is not answered with another one.
pub(crate) const LATE_RESYNC: f64 = 0.4;
/// The least time between two such restarts: the decoder that cannot keep up is
/// the one this fires for, and it will still be behind straight afterwards.
pub(crate) const RESYNC_GAP: Duration = Duration::from_secs(2);

/// Whether a picture `late` seconds behind the master clock is restarted at it,
/// given when the last restart was ([`Player::pump`]).
pub(crate) fn should_resync(late: f64, last: Option<Instant>) -> bool {
    late > LATE_RESYNC && last.is_none_or(|t| t.elapsed() >= RESYNC_GAP)
}

/// The gate a live drag sample goes through. With the worker still owing a
/// frame (`busy`), writing now would only cancel the open the picture is already
/// waiting for -- the sample is held in `stash` instead, and the frame that
/// lands writes it ([`Player::flush_drag`]). Returns what to write, if anything.
///
/// The press (`first`) never waits: it is the undo step the whole gesture rolls
/// back to, so it has to be taken against the state the hand picked up.
pub(crate) fn stash_or_write<T: Copy>(stash: &mut Option<T>, value: T, first: bool, busy: bool) -> Option<T> {
    match busy && !first {
        true => {
            *stash = Some(value);
            None
        }
        false => Some(value),
    }
}

/// How long an open may stand before the window says so in words. Well past an
/// ordinary seek (a warm reopen is under a tenth of this) and well under what a
/// cold read of a big film takes, which is the only case worth a line.
pub(crate) const SEEK_STALL: Duration = Duration::from_secs(2);

/// What a seek that has stood past [`SEEK_STALL`] says, and nothing at all
/// before that: a line on every click of the ruler would be a flicker, and the
/// picture holding still for a tenth of a second is not something to explain.
/// The import bar's words, for the import bar's reason -- a window that cannot
/// move and a window that has hung look identical, so this one says which.
pub(crate) fn seek_line(standing: Option<Duration>) -> Option<String> {
    let since = standing.filter(|d| *d >= SEEK_STALL)?;
    Some(format!(
        "still opening the picture — a cold read of a big file is seconds of it, and the window \
         is not frozen · {} elapsed",
        clock(since.as_secs_f32())
    ))
}
