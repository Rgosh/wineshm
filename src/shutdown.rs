//! Being told to stop by something other than a person typing `exit`.
//!
//! **The pages outlive a killed bridge, and that is the one case the watchdog
//! cannot cover.** [`crate::watchdog`] handles the writer going away while
//! this program is still running: it notices the quiet and zeroes everything.
//! It cannot handle *this program* going away, because nothing of it is left
//! to notice. The window is closed, the machine logs out, a script reaps the
//! wrong process — and the files stay under `/dev/shm` holding the last frame
//! anybody wrote, for ever.
//!
//! [`crate::liveness`] answers that for readers who ask: the note stops being
//! touched, and a reader that checks its age sees `abandoned` rather than a
//! live session. That is the right backstop and it is not a substitute. A
//! reader that just opens the file — which is most of them, because "it is
//! just a file" is the whole appeal — gets a plausible session that ended
//! hours ago, with every number in it real and none of it now.
//!
//! So the bridge takes the chance it is given. Windows hands a program a few
//! seconds' warning before it is killed for closing, logging off or shutting
//! down, and that is enough to blank the pages and unlink them.
//!
//! # What this is not
//!
//! It is not a guarantee. `TerminateProcess`, `SIGKILL`, the machine losing
//! power — none of those are warnings, and nothing can be done about them.
//! The note's pulse is what covers those, and it is why both exist.

use core::time::Duration;

/// How long the handler may hold the process open while it tidies up.
///
/// Windows allows about five seconds after a close, a logoff or a shutdown
/// before it stops asking and kills the process. Four leaves a margin: being
/// killed halfway through unlinking is worse than not having started, because
/// it can leave some pages gone and some behind.
pub const GRACE: Duration = Duration::from_secs(4);

/// The console control events, named here rather than imported.
///
/// Written out so that the rule about which of them means "stop" is a
/// function of ordinary numbers and can be tested on a machine that has no
/// Win32 at all — the same reason [`crate::shadow`] and [`crate::pacing`]
/// exist apart from [`crate::win`].
pub mod event {
    /// Ctrl-C.
    pub const CTRL_C: u32 = 0;
    /// Ctrl-Break.
    pub const CTRL_BREAK: u32 = 1;
    /// The window was closed.
    pub const CLOSE: u32 = 2;
    /// The user is logging off.
    pub const LOGOFF: u32 = 5;
    /// The machine is shutting down.
    pub const SHUTDOWN: u32 = 6;
}

/// Why the program is being asked to stop, in words a log can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Somebody pressed Ctrl-C or Ctrl-Break.
    Interrupted,
    /// The window was closed.
    WindowClosed,
    /// The session is ending — a logoff or a shutdown.
    SessionEnding,
}

impl Reason {
    /// What to print, phrased as the thing that happened.
    pub fn told(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::WindowClosed => "the window was closed",
            Self::SessionEnding => "the session is ending",
        }
    }
}

/// What a console control event means, or `None` for one this does not know.
///
/// **Every event Windows defines means stop**, which makes the `None` arm look
/// pointless until you remember that the handler is called with whatever
/// number the system passes. Answering "yes, stop" to a number nobody has
/// documented is answering a question that was not asked; the system's own
/// default handling is the better answer for those.
pub fn reason(event: u32) -> Option<Reason> {
    match event {
        event::CTRL_C | event::CTRL_BREAK => Some(Reason::Interrupted),
        event::CLOSE => Some(Reason::WindowClosed),
        event::LOGOFF | event::SHUTDOWN => Some(Reason::SessionEnding),
        _ => None,
    }
}

#[cfg(windows)]
mod imp {
    use super::{GRACE, Reason, reason};
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Instant;
    use windows::Win32::Foundation::{BOOL, FALSE, TRUE};
    use windows::Win32::System::Console::SetConsoleCtrlHandler;

    static ASKED: AtomicBool = AtomicBool::new(false);
    static FINISHED: AtomicBool = AtomicBool::new(false);
    static WHY: AtomicU32 = AtomicU32::new(u32::MAX);

    /// The handler itself.
    ///
    /// It runs on a thread of the system's making, so it does as little as it
    /// can: raise a flag, and then hold the process open while the main thread
    /// does the actual tidying. Doing the unlinking here instead would race
    /// the main thread's own shutdown for the same files.
    unsafe extern "system" fn handler(event: u32) -> BOOL {
        let Some(why) = reason(event) else {
            return FALSE;
        };
        WHY.store(event, Ordering::SeqCst);
        ASKED.store(true, Ordering::SeqCst);

        // Wait for the main thread to say it is done, but never for ever: the
        // system is going to kill this process shortly regardless, and a
        // handler that hangs turns a tidy exit into the untidy one it was
        // trying to avoid.
        let until = Instant::now() + GRACE;
        while !FINISHED.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(core::time::Duration::from_millis(10));
        }
        let _ = why;
        TRUE
    }

    /// Ask the system to tell this program before it is killed.
    ///
    /// Failing is not an error worth stopping for: the program still works,
    /// it just leaves its pages behind if the window is closed. Say so and
    /// carry on.
    pub fn listen() -> bool {
        // SAFETY: the routine is a `'static` function with the signature
        // Win32 requires, and it is registered once.
        unsafe { SetConsoleCtrlHandler(Some(handler), true) }.is_ok()
    }

    /// Whether something has asked this program to stop.
    pub fn asked_to_stop() -> bool {
        ASKED.load(Ordering::SeqCst)
    }

    /// Why, once [`asked_to_stop`] is true.
    pub fn why() -> Option<Reason> {
        reason(WHY.load(Ordering::SeqCst))
    }

    /// Tell the handler the tidying is finished, so it can stop holding the
    /// process open.
    pub fn finished() {
        FINISHED.store(true, Ordering::SeqCst);
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Reason;

    /// Nothing to listen to: this half of the program does not hold pages.
    pub fn listen() -> bool {
        false
    }
    /// Never, off Windows.
    pub fn asked_to_stop() -> bool {
        false
    }
    /// Never, off Windows.
    pub fn why() -> Option<Reason> {
        None
    }
    /// Nothing to tell.
    pub fn finished() {}
}

pub use imp::{asked_to_stop, finished, listen, why};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_windows_defines_means_stop() {
        for event in [
            event::CTRL_C,
            event::CTRL_BREAK,
            event::CLOSE,
            event::LOGOFF,
            event::SHUTDOWN,
        ] {
            assert!(reason(event).is_some(), "event {event} was not understood");
        }
    }

    /// Closing the window and logging off are not the same thing to a person
    /// reading the log, and the point of the message is the person.
    #[test]
    fn the_events_are_told_apart() {
        assert_eq!(reason(event::CTRL_C), Some(Reason::Interrupted));
        assert_eq!(reason(event::CTRL_BREAK), Some(Reason::Interrupted));
        assert_eq!(reason(event::CLOSE), Some(Reason::WindowClosed));
        assert_eq!(reason(event::LOGOFF), Some(Reason::SessionEnding));
        assert_eq!(reason(event::SHUTDOWN), Some(Reason::SessionEnding));
    }

    /// 3 and 4 are not events Win32 defines. Claiming to have handled one is
    /// worse than declining it, because declining lets the system do whatever
    /// it would have done.
    #[test]
    fn an_event_nobody_documented_is_declined_rather_than_claimed() {
        for event in [3, 4, 7, 99, u32::MAX] {
            assert_eq!(reason(event), None, "event {event} was claimed");
        }
    }

    #[test]
    fn every_reason_has_something_to_say() {
        for why in [
            Reason::Interrupted,
            Reason::WindowClosed,
            Reason::SessionEnding,
        ] {
            assert!(!why.told().is_empty());
        }
    }

    /// Long enough to unlink a handful of files, and comfortably inside the
    /// five seconds Windows allows before it stops waiting.
    #[test]
    fn the_grace_leaves_a_margin_under_what_windows_allows() {
        assert!(GRACE < Duration::from_secs(5));
        assert!(GRACE >= Duration::from_secs(2));
    }

    /// Off Windows there is nothing holding pages, so there is nothing to
    /// arrange — and the calls still have to exist for the rest of the program
    /// to compile against.
    #[cfg(not(windows))]
    #[test]
    fn the_unix_half_is_inert_but_present() {
        assert!(!listen());
        assert!(!asked_to_stop());
        assert_eq!(why(), None);
        finished();
    }
}
