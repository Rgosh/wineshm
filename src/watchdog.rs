//! Noticing that the writer has gone, and blanking what it left.
//!
//! **The pages outlive the program that filled them.** A game exits; the
//! section it was writing into stays, because this bridge is holding it; and
//! the file under `/dev/shm` goes on holding the last frame for ever. A reader
//! opening it finds a car at some speed on some circuit — real numbers, from a
//! session that ended. Start the game again and, for the moment before it
//! writes its first frame, that old frame is still what anybody reads.
//!
//! Zeroes are the honest answer. Every reader already knows how to wait
//! through them: they are the state between a mapping existing and the writer
//! filling it, which happens at the start of every session anyway.
//!
//! # What counts as gone
//!
//! This crate does not know what any particular program's blocks mean, so it
//! cannot look inside one and see a status field. What it can do is watch a
//! block the caller says changes constantly while the writer is alive — a
//! physics block at 333 Hz, a frame counter — and treat that going quiet as
//! the writer having stopped.
//!
//! Naming that block is the caller's job precisely because it is the caller
//! that knows. Nothing is blanked unless one is named.

use core::time::Duration;

/// How long a heartbeat block may go unchanged before the writer is presumed
/// gone.
///
/// Five seconds. Long enough to survive a loading screen, a shader compile or
/// a stutter; short enough that a driver who alt-tabs out and back does not
/// read a stale frame in between. It is a default, and `--blank-after` moves
/// it.
pub const BLANK_AFTER: Duration = Duration::from_secs(5);

/// What the watchdog wants done, having looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Carry on.
    Nothing,
    /// The writer has gone quiet: zero every block, once.
    Blank,
    /// It is writing again. Nothing to do but note it — the blocks refill
    /// themselves.
    Writing,
}

/// Watches one block for signs of life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watchdog {
    blank_after: Duration,
    quiet_for: Duration,
    blanked: bool,
}

impl Watchdog {
    /// A watchdog with its own patience.
    pub fn new(blank_after: Duration) -> Self {
        Self {
            blank_after,
            quiet_for: Duration::ZERO,
            blanked: false,
        }
    }

    /// Whether the blocks are currently blanked.
    pub fn is_blanked(&self) -> bool {
        self.blanked
    }

    /// How long the heartbeat has been quiet.
    pub fn quiet_for(&self) -> Duration {
        self.quiet_for
    }

    /// Tell it what the last look found, and how long since the one before.
    ///
    /// **`Blank` is returned once per silence, not every look.** Zeroing a
    /// page that is already zeroes is work nobody asked for, and — more to the
    /// point — a bridge that rewrites the blocks sixty times a second while
    /// nothing is happening is exactly the cost this program exists to avoid.
    pub fn saw(&mut self, changed: bool, since_last_look: Duration) -> Verdict {
        if changed {
            self.quiet_for = Duration::ZERO;
            if self.blanked {
                self.blanked = false;
                return Verdict::Writing;
            }
            return Verdict::Nothing;
        }

        self.quiet_for = self.quiet_for.saturating_add(since_last_look);
        if !self.blanked && self.quiet_for >= self.blank_after {
            self.blanked = true;
            return Verdict::Blank;
        }
        Verdict::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn a_writer_that_keeps_writing_is_left_alone() {
        let mut dog = Watchdog::new(BLANK_AFTER);
        for _ in 0..1000 {
            assert_eq!(dog.saw(true, MS(4)), Verdict::Nothing);
        }
        assert!(!dog.is_blanked());
    }

    #[test]
    fn silence_past_the_limit_asks_for_a_blanking() {
        let mut dog = Watchdog::new(Duration::from_secs(5));
        assert_eq!(dog.saw(false, Duration::from_secs(4)), Verdict::Nothing);
        assert_eq!(dog.saw(false, Duration::from_secs(1)), Verdict::Blank);
        assert!(dog.is_blanked());
    }

    /// Once, not on every look: a bridge that rewrites the blocks while
    /// nothing is happening is the cost this whole program is about avoiding.
    #[test]
    fn it_asks_to_blank_once_and_then_stops_asking() {
        let mut dog = Watchdog::new(Duration::from_secs(1));
        assert_eq!(dog.saw(false, Duration::from_secs(2)), Verdict::Blank);
        for _ in 0..100 {
            assert_eq!(dog.saw(false, Duration::from_secs(1)), Verdict::Nothing);
        }
    }

    #[test]
    fn a_writer_that_comes_back_is_noticed_once() {
        let mut dog = Watchdog::new(Duration::from_secs(1));
        assert_eq!(dog.saw(false, Duration::from_secs(2)), Verdict::Blank);
        assert_eq!(dog.saw(true, MS(4)), Verdict::Writing);
        assert!(!dog.is_blanked());
        // And the second frame is ordinary again.
        assert_eq!(dog.saw(true, MS(4)), Verdict::Nothing);
    }

    /// A game restarted has to be able to blank again, or the second session
    /// to end leaves its frame behind.
    #[test]
    fn it_can_blank_again_after_the_writer_comes_and_goes_twice() {
        let mut dog = Watchdog::new(Duration::from_secs(1));
        for _ in 0..3 {
            assert_eq!(dog.saw(false, Duration::from_secs(2)), Verdict::Blank);
            assert_eq!(dog.saw(true, MS(4)), Verdict::Writing);
        }
    }

    /// A stutter is not a writer that has gone.
    #[test]
    fn a_pause_shorter_than_the_limit_changes_nothing() {
        let mut dog = Watchdog::new(Duration::from_secs(5));
        for _ in 0..4 {
            assert_eq!(dog.saw(false, Duration::from_secs(1)), Verdict::Nothing);
        }
        assert_eq!(
            dog.saw(true, MS(4)),
            Verdict::Nothing,
            "not a Writing: it never blanked"
        );
        assert_eq!(dog.quiet_for(), Duration::ZERO);
    }

    #[test]
    fn the_quiet_is_measured_and_can_be_read_back() {
        let mut dog = Watchdog::new(Duration::from_secs(10));
        dog.saw(false, Duration::from_secs(3));
        assert_eq!(dog.quiet_for(), Duration::from_secs(3));
        dog.saw(false, Duration::from_secs(2));
        assert_eq!(dog.quiet_for(), Duration::from_secs(5));
        dog.saw(true, MS(1));
        assert_eq!(dog.quiet_for(), Duration::ZERO);
    }

    /// Exactly at the limit counts: a limit nothing can reach is not one.
    #[test]
    fn the_limit_is_inclusive() {
        let mut dog = Watchdog::new(Duration::from_secs(5));
        assert_eq!(dog.saw(false, Duration::from_secs(5)), Verdict::Blank);
    }

    /// A very long session must not wrap the counter.
    #[test]
    fn a_long_silence_does_not_overflow() {
        let mut dog = Watchdog::new(Duration::from_secs(5));
        let mut last = Duration::ZERO;
        for _ in 0..100 {
            dog.saw(false, Duration::MAX);
            assert!(
                dog.quiet_for() >= last,
                "{:?} went backwards from {last:?}",
                dog.quiet_for()
            );
            last = dog.quiet_for();
        }
        assert_eq!(
            dog.quiet_for(),
            Duration::MAX,
            "saturated rather than wrapped"
        );
    }

    /// A limit of zero means blank the instant it is quiet, which is a
    /// configuration somebody debugging may want.
    #[test]
    fn a_limit_of_nothing_blanks_at_once() {
        let mut dog = Watchdog::new(Duration::ZERO);
        assert_eq!(dog.saw(false, Duration::ZERO), Verdict::Blank);
    }
}
