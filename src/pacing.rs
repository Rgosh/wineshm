//! How often to look, when looking costs something.
//!
//! **This is the whole of the second optimisation.** When the bridge has to
//! copy a page rather than own it — see [`crate::Mode`] — something has to
//! decide how often. The obvious answer is a fixed interval, and the bridge
//! this replaces used four milliseconds: 250 wake-ups a second, for every
//! page, for as long as the program runs.
//!
//! Most of that is spent on nothing. A racing game rewrites its physics block
//! three hundred times a second *while the car is moving*, and not at all in
//! the menus, in the garage, on a loading screen, while the session is paused,
//! or while somebody is reading the results. Two of Assetto Corsa's four
//! blocks are written once a session and never again.
//!
//! So: keep the fast interval while the bytes are changing, and back off
//! geometrically when they are not. The cost of being wrong is one interval of
//! latency on the first change after a quiet spell, which is why the backoff
//! resets the instant anything moves rather than easing back down.

use core::time::Duration;

/// The pace to look at one page, and how it moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pacing {
    quick: Duration,
    slowest: Duration,
    current: Duration,
}

/// How long to wait when something is happening.
///
/// Four milliseconds, inherited rather than chosen: it is comfortably inside
/// both ends — a 333 Hz writer and a 60 Hz reader — and it is what the bridge
/// this replaces used, so a measurement against it is a measurement of the
/// backoff and not of a different interval.
pub const QUICK: Duration = Duration::from_millis(4);

/// How long to wait when nothing has happened for a while.
///
/// Sixty-four milliseconds is sixteen times less work, and it is still four
/// times faster than a human notices. It is the ceiling rather than the
/// resting place: a page that changes once goes straight back to [`QUICK`].
pub const SLOWEST: Duration = Duration::from_millis(64);

impl Default for Pacing {
    fn default() -> Self {
        Self::new(QUICK, SLOWEST)
    }
}

impl Pacing {
    /// A pace of your own, for a program that writes at another rate.
    ///
    /// A `slowest` below `quick` is raised to it rather than refused: the
    /// meaning of "never back off" is a ceiling equal to the floor, and that
    /// is a configuration somebody may want.
    pub fn new(quick: Duration, slowest: Duration) -> Self {
        let slowest = if slowest < quick { quick } else { slowest };
        Self {
            quick,
            slowest,
            current: quick,
        }
    }

    /// What this page's pace is right now.
    pub fn interval(&self) -> Duration {
        self.current
    }

    /// Tell it what the last look found, and get the next wait.
    ///
    /// **Asymmetric on purpose.** Backing off doubles; coming back does not
    /// halve, it jumps. Easing back would mean the first few frames after a
    /// car leaves the pits are sampled at the pace of the pit box, which is
    /// the one moment the data matters most.
    pub fn after(&mut self, changed: bool) -> Duration {
        self.current = if changed {
            self.quick
        } else {
            // Saturating, so a ceiling near `Duration::MAX` cannot wrap. The
            // doubling stops at `slowest` in either case.
            self.current.saturating_mul(2).min(self.slowest)
        };
        self.current
    }

    /// How much less often this is looking than the quick pace.
    ///
    /// For the line the program prints when it stops, so the saving is
    /// something a user can see rather than a claim in a readme.
    pub fn relief(&self) -> f64 {
        if self.quick.is_zero() {
            return 1.0;
        }
        self.current.as_secs_f64() / self.quick.as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_starts_quick() {
        let pacing = Pacing::default();
        assert_eq!(pacing.interval(), QUICK);
    }

    #[test]
    fn nothing_changing_backs_off_by_doubling() {
        let mut pacing = Pacing::new(Duration::from_millis(4), Duration::from_millis(64));
        assert_eq!(pacing.after(false), Duration::from_millis(8));
        assert_eq!(pacing.after(false), Duration::from_millis(16));
        assert_eq!(pacing.after(false), Duration::from_millis(32));
        assert_eq!(pacing.after(false), Duration::from_millis(64));
    }

    #[test]
    fn it_never_backs_off_past_the_ceiling() {
        let mut pacing = Pacing::new(Duration::from_millis(4), Duration::from_millis(64));
        for _ in 0..50 {
            pacing.after(false);
        }
        assert_eq!(pacing.interval(), Duration::from_millis(64));
    }

    /// The asymmetry is the point: a change goes straight back to quick.
    #[test]
    fn one_change_comes_all_the_way_back() {
        let mut pacing = Pacing::new(Duration::from_millis(4), Duration::from_millis(64));
        for _ in 0..10 {
            pacing.after(false);
        }
        assert_eq!(pacing.interval(), Duration::from_millis(64));
        assert_eq!(pacing.after(true), Duration::from_millis(4));
    }

    #[test]
    fn staying_busy_stays_quick() {
        let mut pacing = Pacing::default();
        for _ in 0..100 {
            assert_eq!(pacing.after(true), QUICK);
        }
    }

    /// A ceiling under the floor means "do not back off", not an error.
    #[test]
    fn a_ceiling_below_the_floor_becomes_the_floor() {
        let mut pacing = Pacing::new(Duration::from_millis(10), Duration::from_millis(1));
        assert_eq!(pacing.interval(), Duration::from_millis(10));
        assert_eq!(pacing.after(false), Duration::from_millis(10));
        assert_eq!(pacing.after(false), Duration::from_millis(10));
    }

    /// A ceiling near the top of the type must not wrap when doubled.
    #[test]
    fn a_huge_ceiling_does_not_overflow() {
        let mut pacing = Pacing::new(Duration::from_secs(1), Duration::MAX);
        let mut last = Duration::ZERO;
        for _ in 0..200 {
            let now = pacing.after(false);
            assert!(now >= last, "{now:?} went backwards from {last:?}");
            last = now;
        }
        assert!(last > Duration::from_secs(1));
    }

    #[test]
    fn relief_says_how_much_work_is_being_skipped() {
        let mut pacing = Pacing::new(Duration::from_millis(4), Duration::from_millis(64));
        assert!((pacing.relief() - 1.0).abs() < f64::EPSILON);
        for _ in 0..10 {
            pacing.after(false);
        }
        assert!(
            (pacing.relief() - 16.0).abs() < 0.001,
            "{}",
            pacing.relief()
        );
    }

    /// A zero quick pace is a spin loop, and dividing by it is the only way
    /// this can produce a number that is not a number.
    #[test]
    fn relief_with_no_interval_at_all_is_still_a_number() {
        let pacing = Pacing::new(Duration::ZERO, Duration::ZERO);
        assert!(pacing.relief().is_finite());
    }
}
