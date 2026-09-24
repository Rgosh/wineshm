//! Whether anything is actually moving.
//!
//! **The question this program could not answer about itself.** `--verify`
//! says what is published and how big it is; nothing said whether a single
//! byte had changed since the game loaded. That is the question somebody
//! actually has when their telemetry is wrong, and without an answer the
//! candidates are: the game is not writing, the bridge is not copying, the
//! reader is looking at the wrong block, or the numbers are fine and the
//! reader's arithmetic is not.
//!
//! Watching needs nothing from Win32. The pages are ordinary files under
//! `/dev/shm`, so this runs on the Linux side, outside the prefix, while the
//! game is running — which is also the only place somebody can comfortably
//! look at a terminal mid-session.
//!
//! # Why a proportion rather than a rate
//!
//! A physics block written at 333 Hz cannot be counted by sampling: counting
//! events needs samples faster than the events, and this deliberately looks
//! slowly enough to cost nothing. What can be said honestly is what proportion
//! of looks found something different, and that separates the cases that
//! matter — a block being written constantly, a block written occasionally,
//! and a block not being written at all.

use core::time::Duration;

/// How often to look while watching.
///
/// Twenty milliseconds. Fast enough that a block written every frame reads as
/// moving at any frame rate a game runs at, slow enough that watching a
/// handful of pages is fifty reads a second and not a core.
pub const LOOK_EVERY: Duration = Duration::from_millis(20);

/// How long a block may be unchanged before it is called still.
///
/// A second. Below a rendered frame's worth of slack it would call a 30 Hz
/// block still between writes; above a second it stops being a live answer.
pub const STILL_AFTER: Duration = Duration::from_secs(1);

/// What a block is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doing {
    /// Changing right now.
    Moving,
    /// Not changing, but it has at some point.
    Still,
    /// Has never changed since watching began.
    Never,
}

impl Doing {
    /// One word for the table.
    pub fn word(self) -> &'static str {
        match self {
            Self::Moving => "moving",
            Self::Still => "still",
            Self::Never => "never",
        }
    }
}

/// What has been seen of one block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    looks: u64,
    changes: u64,
    since_change: Option<Duration>,
}

impl Activity {
    /// Record one look.
    pub fn saw(&mut self, changed: bool, since_last_look: Duration) {
        self.looks = self.looks.saturating_add(1);
        if changed {
            self.changes = self.changes.saturating_add(1);
            self.since_change = Some(Duration::ZERO);
        } else if let Some(since) = self.since_change.as_mut() {
            *since = since.saturating_add(since_last_look);
        }
    }

    /// How many looks have been taken.
    pub fn looks(&self) -> u64 {
        self.looks
    }

    /// What it is doing, given how long a block may rest.
    pub fn doing(&self, still_after: Duration) -> Doing {
        match self.since_change {
            None => Doing::Never,
            Some(since) if since < still_after => Doing::Moving,
            Some(_) => Doing::Still,
        }
    }

    /// The proportion of looks that found a change, 0 to 100.
    ///
    /// Rounded rather than truncated, so a block that changed on every look
    /// but one does not read as 99 when it is 99.6 — and, more to the point,
    /// so a block that changed once in two hundred looks reads as 1 and not 0.
    pub fn percent(&self) -> u8 {
        if self.looks == 0 {
            return 0;
        }
        let scaled = self.changes.saturating_mul(200) / self.looks;
        let rounded = scaled.div_ceil(2);
        rounded.min(100) as u8
    }

    /// How long since it last changed, if it ever has.
    pub fn since_change(&self) -> Option<Duration> {
        self.since_change
    }

    /// Forget the counts but not the silence.
    ///
    /// The proportion is per reporting period — otherwise a block that was
    /// busy for a minute and has since stopped goes on reporting a high
    /// percentage for ever. How long it has been quiet is not per period and
    /// must survive.
    pub fn new_period(&mut self) {
        self.looks = 0;
        self.changes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn a_block_nobody_has_written_to_is_never_rather_than_still() {
        let mut seen = Activity::default();
        for _ in 0..100 {
            seen.saw(false, MS(20));
        }
        assert_eq!(seen.doing(STILL_AFTER), Doing::Never);
        assert_eq!(seen.percent(), 0);
        assert_eq!(seen.since_change(), None);
    }

    /// The distinction that matters: "it has never written" is a different
    /// problem from "it stopped writing", and they look identical in a number.
    #[test]
    fn a_block_that_has_stopped_is_still_rather_than_never() {
        let mut seen = Activity::default();
        seen.saw(true, MS(20));
        for _ in 0..100 {
            seen.saw(false, MS(20));
        }
        assert_eq!(seen.doing(STILL_AFTER), Doing::Still);
        assert!(
            seen.since_change()
                .is_some_and(|d| d >= Duration::from_secs(1))
        );
    }

    #[test]
    fn a_block_being_written_constantly_is_moving_at_a_hundred_percent() {
        let mut seen = Activity::default();
        for _ in 0..50 {
            seen.saw(true, MS(20));
        }
        assert_eq!(seen.doing(STILL_AFTER), Doing::Moving);
        assert_eq!(seen.percent(), 100);
    }

    /// A block written every tenth look is written, and reporting 0 would say
    /// the opposite.
    #[test]
    fn a_rarely_written_block_does_not_round_down_to_nothing() {
        let mut seen = Activity::default();
        for at in 0..200 {
            seen.saw(at % 200 == 0, MS(20));
        }
        assert_eq!(seen.percent(), 1, "one change in two hundred looks");
    }

    #[test]
    fn a_block_written_half_the_time_reads_as_half() {
        let mut seen = Activity::default();
        for at in 0..100 {
            seen.saw(at % 2 == 0, MS(20));
        }
        assert_eq!(seen.percent(), 50);
    }

    /// A pause shorter than the limit is a block between frames, not a block
    /// that has stopped.
    #[test]
    fn a_gap_shorter_than_the_limit_is_still_moving() {
        let mut seen = Activity::default();
        seen.saw(true, MS(20));
        seen.saw(false, MS(500));
        assert_eq!(seen.doing(STILL_AFTER), Doing::Moving);
    }

    /// Otherwise a block that was busy and has stopped reports a high
    /// percentage for ever, which is the opposite of what happened.
    #[test]
    fn a_new_period_forgets_the_counts_and_keeps_the_silence() {
        let mut seen = Activity::default();
        for _ in 0..50 {
            seen.saw(true, MS(20));
        }
        seen.new_period();
        assert_eq!(seen.percent(), 0);
        assert_eq!(seen.looks(), 0);

        for _ in 0..100 {
            seen.saw(false, MS(20));
        }
        assert_eq!(
            seen.doing(STILL_AFTER),
            Doing::Still,
            "the silence survived the period"
        );
    }

    #[test]
    fn nothing_looked_at_is_not_a_division_by_zero() {
        let seen = Activity::default();
        assert_eq!(seen.percent(), 0);
        assert_eq!(seen.looks(), 0);
        assert_eq!(seen.doing(STILL_AFTER), Doing::Never);
    }

    #[test]
    fn a_very_long_watch_does_not_overflow() {
        let mut seen = Activity {
            looks: u64::MAX,
            changes: u64::MAX,
            ..Default::default()
        };
        seen.saw(true, Duration::MAX);
        assert_eq!(seen.looks(), u64::MAX);
        assert!(seen.percent() <= 100);
    }

    #[test]
    fn every_verdict_has_a_word() {
        for doing in [Doing::Moving, Doing::Still, Doing::Never] {
            assert!(!doing.word().is_empty());
        }
    }
}
