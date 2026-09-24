//! Letting the game hold the section, and leaving.
//!
//! **Measured, then built.** A named section in Wine is a counted object. This
//! program creates it backed by a file under `/dev/shm`; the game then asks for
//! the same name, is handed the same object, and maps it. From that moment two
//! processes hold it — and if this one lets go, the game's handle keeps the
//! object alive and the game's stores keep landing in the file. Verified under
//! Proton: the bridge was killed and a writer's counter went on advancing in
//! `/dev/shm` with no bridge process in existence.
//!
//! That is worth doing because of what the bridge costs while it sits there.
//! It is not processor time — an owned page needs no copying, and the measured
//! idle cost is a quarter of one percent of a core. It is a Wine process, a
//! wineserver client and about fifty megabytes, resident for the whole session
//! for the sake of a handle nobody needs any more.
//!
//! # Why it cannot simply exit
//!
//! The same experiment showed the other half. When the game exits it releases
//! the name, and the **file stays**, holding the last frame for ever. Start the
//! game again and it finds no section of that name, makes its own — backed by
//! nothing this side can see — and writes into that. Measured: the second run
//! reported `already existed = false` while the file sat frozen on the previous
//! session's last frame.
//!
//! That is the failure this whole program is built to prevent, so handing over
//! is only allowed when somebody is left watching: the Linux half, which
//! outlives the prefix, keeps the note's pulse beating and takes the pages away
//! the moment the writer stops.

/// What this program's exit code means when it has handed the section over.
///
/// A distinct code rather than a line of output, because the Linux half reads
/// it from `wait` and has to be sure: treating an ordinary exit as a handover
/// would leave pages nobody is watching, and treating a handover as a crash
/// would take away pages a game is still writing to.
pub const HANDED_OVER: u8 = 3;

/// Whether the section may be left in the game's hands.
///
/// Every condition is a way of being certain the game is actually holding it.
///
/// - **Every page owned.** A mirrored page is one this program is copying by
///   hand; letting go of that stops the copying and freezes the file.
/// - **A heartbeat named.** Without one there is nothing that can later notice
///   the game leaving, and handing over would be handing over to nobody.
/// - **It has stirred.** Bytes changing in a page this program created and
///   never writes to means one thing: the game opened it and wrote. That is
///   the proof of a second handle, and there is no other way to ask.
pub fn may_hand_over(all_owned: bool, heartbeat_named: bool, has_stirred: bool) -> bool {
    all_owned && heartbeat_named && has_stirred
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_that_is_writing_to_owned_pages_may_have_them() {
        assert!(may_hand_over(true, true, true));
    }

    /// Letting go of a mirrored page stops the copying, and the file freezes
    /// on whatever was last copied into it.
    #[test]
    fn a_mirrored_page_is_never_handed_over() {
        assert!(!may_hand_over(false, true, true));
    }

    /// Nothing would be left that could notice the game leaving, so the pages
    /// would outlive the session with nobody to take them away.
    #[test]
    fn without_a_heartbeat_there_is_nobody_to_hand_over_to() {
        assert!(!may_hand_over(true, false, true));
    }

    /// The whole proof that a second handle exists is that something wrote.
    /// Handing over before that releases the name and the game then makes its
    /// own section, which this side cannot see at all.
    #[test]
    fn nothing_is_handed_over_before_the_game_has_written() {
        assert!(!may_hand_over(true, true, false));
    }

    #[test]
    fn every_condition_is_required() {
        for (owned, beat, stirred) in [
            (false, false, false),
            (false, false, true),
            (false, true, false),
            (true, false, false),
        ] {
            assert!(!may_hand_over(owned, beat, stirred));
        }
    }

    /// 0 is success and 1 is the ordinary failure, so neither can be confused
    /// with this. 2 is already what a bad command line exits with.
    #[test]
    fn the_exit_code_cannot_be_confused_with_the_ordinary_ones() {
        assert_ne!(HANDED_OVER, 0);
        assert_ne!(HANDED_OVER, 1);
        assert_ne!(HANDED_OVER, 2);
    }
}
