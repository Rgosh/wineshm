//! Telling a bridge that is running from one that was killed.
//!
//! **The failure this exists for is silence that looks like health.** A bridge
//! that exits cleanly takes its pages and its note away; one that is killed —
//! the window closed, the machine suspended, a benchmark script that reaped
//! the wrong process — leaves both behind. The pages then hold the last bytes
//! anybody wrote, for ever, and a reader opening them finds a plausible
//! session that ended hours ago. Every number in it is real and none of it is
//! now.
//!
//! The bridge writes its note again every [`BEAT`], so the file's modified
//! time is a pulse. A reader that finds one older than [`STALE`] is looking at
//! something abandoned, and can say so instead of publishing a ghost.
//!
//! Nothing here reads a clock the caller cannot supply, so all of it is
//! testable without waiting.

use core::time::Duration;

/// How often a running bridge touches its note.
///
/// Two seconds. It is one small write, so the cost is nothing measurable, and
/// it is short enough that a reader notices an abandoned bridge inside the
/// time it takes somebody to look at the screen and wonder.
pub const BEAT: Duration = Duration::from_secs(2);

/// How old a note has to be before the bridge behind it is presumed gone.
///
/// Three beats. One missed beat is a machine that was busy; three is a process
/// that is not there. Erring long on purpose: calling a live bridge dead sends
/// somebody to restart something that was working.
pub const STALE: Duration = Duration::from_secs(6);

/// What a note's age says about the bridge that wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pulse {
    /// Touched recently. Something is running.
    Beating,
    /// Old enough that nothing is maintaining it — the pages beside it are
    /// whatever was last written into them, and are not a live session.
    Abandoned,
    /// In the future by more than a beat, which a clock change can do. Treated
    /// as alive: a reader that calls a bridge dead because the machine's clock
    /// moved is worse than one that waits a moment longer.
    Ahead,
}

impl Pulse {
    /// Whether a reader should trust the pages beside this note.
    pub fn is_worth_reading(self) -> bool {
        matches!(self, Self::Beating | Self::Ahead)
    }

    /// One word, for a machine.
    pub fn word(self) -> &'static str {
        match self {
            Self::Beating => "beating",
            Self::Abandoned => "abandoned",
            Self::Ahead => "ahead",
        }
    }

    /// A sentence for a person.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Beating => "a bridge is running",
            Self::Abandoned => {
                "the bridge that published this is gone — the pages are whatever it left"
            }
            Self::Ahead => "the note is dated ahead of this clock, which is not a fault",
        }
    }
}

/// Read a pulse from how long ago the note was touched.
///
/// `age` is negative-free by construction — the caller works it out from two
/// times it holds — so "in the future" arrives as `ahead`.
pub fn pulse(age: Duration, ahead: bool, stale_after: Duration) -> Pulse {
    if ahead {
        return Pulse::Ahead;
    }
    if age > stale_after {
        return Pulse::Abandoned;
    }
    Pulse::Beating
}

/// Whether it is time to touch the note again.
///
/// Called on every turn of the loop, which is why it takes the times rather
/// than reading a clock: the loop already knows both.
pub fn due_to_beat(since_last: Duration, every: Duration) -> bool {
    since_last >= every
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn a_note_touched_a_moment_ago_is_a_running_bridge() {
        assert_eq!(pulse(Duration::ZERO, false, STALE), Pulse::Beating);
        assert_eq!(pulse(S(1), false, STALE), Pulse::Beating);
        assert_eq!(
            pulse(S(6), false, STALE),
            Pulse::Beating,
            "exactly at the edge"
        );
    }

    #[test]
    fn a_note_nobody_has_touched_is_an_abandoned_bridge() {
        assert_eq!(pulse(S(7), false, STALE), Pulse::Abandoned);
        assert_eq!(pulse(S(3600), false, STALE), Pulse::Abandoned);
    }

    /// A clock that moved must not condemn a working bridge.
    #[test]
    fn a_note_dated_ahead_is_not_a_dead_bridge() {
        assert_eq!(pulse(S(3600), true, STALE), Pulse::Ahead);
        assert!(Pulse::Ahead.is_worth_reading());
    }

    #[test]
    fn only_an_abandoned_bridge_is_not_worth_reading() {
        assert!(Pulse::Beating.is_worth_reading());
        assert!(!Pulse::Abandoned.is_worth_reading());
    }

    #[test]
    fn every_pulse_says_something_a_person_can_act_on() {
        for pulse in [Pulse::Beating, Pulse::Abandoned, Pulse::Ahead] {
            let said = pulse.describe();
            assert!(said.len() > 12, "{pulse:?} said {said:?}");
            assert!(!said.contains("error"), "{said:?}");
        }
    }

    #[test]
    fn the_beat_is_due_when_a_beat_has_passed_and_not_before() {
        assert!(!due_to_beat(Duration::ZERO, BEAT));
        assert!(!due_to_beat(Duration::from_millis(1999), BEAT));
        assert!(due_to_beat(S(2), BEAT));
        assert!(due_to_beat(S(30), BEAT));
    }

    /// The window has to be several beats wide, or one busy moment on the
    /// machine reads as a bridge that died.
    #[test]
    fn the_stale_window_is_wider_than_one_beat() {
        assert!(
            STALE >= BEAT * 3,
            "a single missed beat must not condemn it: {BEAT:?} against {STALE:?}"
        );
    }

    /// A caller may choose its own patience.
    #[test]
    fn the_window_can_be_narrowed_by_a_caller_that_wants_to_know_sooner() {
        assert_eq!(pulse(S(3), false, S(2)), Pulse::Abandoned);
        assert_eq!(pulse(S(3), false, S(10)), Pulse::Beating);
    }
}
