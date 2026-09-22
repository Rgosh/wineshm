//! When to look at which page, and when to stop.
//!
//! Pulled out of the program's loop so it can be tested: the loop itself is a
//! sleep, a Win32 call and a print, none of which a test can say anything
//! useful about, and all of the decisions are here.
//!
//! Time is a [`Duration`] since the program started rather than an `Instant`,
//! for the same reason — a test can hand this the fourth second directly.

use core::time::Duration;

/// Which pages are due to be looked at, and when the next one is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    due: Vec<Duration>,
}

impl Schedule {
    /// A schedule for `count` pages, all due immediately.
    pub fn new(count: usize) -> Self {
        Self {
            due: vec![Duration::ZERO; count],
        }
    }

    /// How many pages it is keeping.
    pub fn len(&self) -> usize {
        self.due.len()
    }

    /// Whether it is keeping none.
    pub fn is_empty(&self) -> bool {
        self.due.is_empty()
    }

    /// Whether this page wants looking at by `at`.
    pub fn is_due(&self, index: usize, at: Duration) -> bool {
        self.due.get(index).is_some_and(|when| *when <= at)
    }

    /// Say a page has just been looked at, and when it wants looking at next.
    pub fn looked_at(&mut self, index: usize, at: Duration, wait: Duration) {
        if let Some(when) = self.due.get_mut(index) {
            *when = at.saturating_add(wait);
        }
    }

    /// How long to sleep before anything is due again.
    ///
    /// `ceiling` caps it, so a schedule with nothing in it still wakes often
    /// enough to notice it has been asked to stop. Zero when something is
    /// already overdue, which the caller turns into "do not sleep".
    pub fn nap(&self, at: Duration, ceiling: Duration) -> Duration {
        self.due
            .iter()
            .map(|when| when.saturating_sub(at))
            .min()
            .unwrap_or(ceiling)
            .min(ceiling)
    }
}

/// How long an input has to last before its ending counts as somebody asking.
///
/// Two seconds: the only thing that can be mistaken for nobody is a parent
/// that dies within two seconds of starting this, and the cost of that mistake
/// is one program left running rather than one that never ran at all.
pub const GRACE: Duration = Duration::from_secs(2);

/// Whether input ending means nobody was ever there, rather than stop.
///
/// **The program stops when its input ends, and that is right twice and wrong
/// once.** A parent closing a pipe is asking it to stop; somebody pressing
/// Ctrl-D in a terminal is asking it to stop. Started from a file manager or a
/// desktop entry there is no input at all, the first read ends immediately, and
/// a program that obeys would unlink everything it had just published and exit
/// inside a second — which from the outside is "running it does nothing".
///
/// The handle cannot answer this under Wine: `GetFileType` says
/// `FILE_TYPE_CHAR` for a console, a pipe and `/dev/null` alike. A console can
/// be recognised on its own — see [`crate::win::stdin_is_a_console`] — and the
/// rest is answered by how quickly the input ended.
pub fn nobody_was_there(console: bool, ended_after: Duration, grace: Duration) -> bool {
    !console && ended_after < grace
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn everything_is_due_at_the_start() {
        let schedule = Schedule::new(3);
        assert_eq!(schedule.len(), 3);
        assert!(!schedule.is_empty());
        for index in 0..3 {
            assert!(schedule.is_due(index, Duration::ZERO));
        }
    }

    #[test]
    fn a_page_looked_at_is_not_due_again_until_its_wait_is_up() {
        let mut schedule = Schedule::new(2);
        schedule.looked_at(0, MS(100), MS(64));

        assert!(!schedule.is_due(0, MS(163)));
        assert!(schedule.is_due(0, MS(164)));
        assert!(schedule.is_due(0, MS(500)));
        // The other one was not touched.
        assert!(schedule.is_due(1, MS(100)));
    }

    #[test]
    fn the_nap_is_until_the_soonest_page_wants_looking_at() {
        let mut schedule = Schedule::new(3);
        schedule.looked_at(0, MS(0), MS(64));
        schedule.looked_at(1, MS(0), MS(4));
        schedule.looked_at(2, MS(0), MS(32));

        assert_eq!(schedule.nap(MS(0), MS(1000)), MS(4));
        // Once the quick one has been dealt with, the next is the 32.
        schedule.looked_at(1, MS(4), MS(64));
        assert_eq!(schedule.nap(MS(4), MS(1000)), MS(28));
    }

    #[test]
    fn something_overdue_means_do_not_sleep_at_all() {
        let mut schedule = Schedule::new(2);
        schedule.looked_at(0, MS(0), MS(4));
        assert_eq!(schedule.nap(MS(100), MS(1000)), Duration::ZERO);
    }

    /// Even with nothing to do, the loop has to come round often enough to
    /// notice it has been asked to stop.
    #[test]
    fn the_ceiling_caps_the_nap() {
        let mut schedule = Schedule::new(1);
        schedule.looked_at(0, MS(0), Duration::from_secs(60));
        assert_eq!(schedule.nap(MS(0), MS(100)), MS(100));
    }

    #[test]
    fn a_schedule_with_no_pages_sleeps_the_ceiling() {
        let schedule = Schedule::new(0);
        assert!(schedule.is_empty());
        assert_eq!(schedule.nap(MS(50), MS(100)), MS(100));
        // And asking about a page it has not got is not a panic.
        assert!(!schedule.is_due(7, MS(50)));
    }

    #[test]
    fn marking_a_page_that_is_not_there_is_ignored_rather_than_fatal() {
        let mut schedule = Schedule::new(1);
        schedule.looked_at(9, MS(0), MS(10));
        assert_eq!(schedule.len(), 1);
    }

    /// Late in a long session the numbers are large; nothing may wrap.
    #[test]
    fn a_long_running_schedule_does_not_overflow() {
        let mut schedule = Schedule::new(1);
        schedule.looked_at(0, Duration::MAX, MS(64));
        assert_eq!(schedule.nap(Duration::ZERO, MS(100)), MS(100));
    }

    #[test]
    fn input_that_ends_at_once_with_no_console_is_nobody_there() {
        assert!(nobody_was_there(false, MS(10), GRACE));
        assert!(nobody_was_there(false, MS(1999), GRACE));
    }

    #[test]
    fn input_that_lasted_is_somebody_asking_it_to_stop() {
        assert!(!nobody_was_there(false, MS(2000), GRACE));
        assert!(!nobody_was_there(false, Duration::from_secs(3600), GRACE));
    }

    /// A console is a person, and Ctrl-D the moment it starts still means
    /// stop.
    #[test]
    fn a_console_is_always_somebody() {
        assert!(!nobody_was_there(true, MS(1), GRACE));
        assert!(!nobody_was_there(true, Duration::ZERO, GRACE));
    }

    #[test]
    fn the_grace_is_the_two_seconds_it_is_documented_as() {
        assert_eq!(GRACE, Duration::from_secs(2));
    }
}
