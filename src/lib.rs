//! Expose a Windows program's named shared memory to Linux.
//!
//! A Windows program running under Wine or Proton can publish a block of
//! shared memory — telemetry, a score, a state block — and nothing on the
//! Linux side can open it. The name lives in the prefix's own kernel object
//! namespace, and that namespace stops at the prefix.
//!
//! This bridges it. Run it *inside* the prefix, tell it which blocks to
//! expose, and each one appears as an ordinary file under `/dev/shm` that any
//! Linux program can open, map and read.
//!
//! # How it works
//!
//! ```text
//!   inside the Wine prefix          │         on Linux
//!                                   │
//!   ┌────────────┐   writes         │
//!   │ the game   │ ───────────┐     │
//!   └────────────┘            ▼     │
//!                     ┌───────────────────┐        ┌──────────────┐
//!                     │  named section    │ ═══════│ /dev/shm/... │
//!                     │  backed by a file │ backed └──────────────┘
//!                     └───────────────────┘   by          ▲
//!                             ▲                           │ reads
//!                             │ created by          ┌──────────────┐
//!                       ┌───────────┐               │ your program │
//!                       │  wineshm  │               └──────────────┘
//!                       └───────────┘
//! ```
//!
//! The trick is that a Win32 section can be backed by a *file*, and a file
//! under `/dev/shm` is shared memory on the Linux side. Create the section
//! with the name the Windows program expects, backed by that file, and the
//! writer's stores land in Linux shared memory with nothing copying anything.
//!
//! # The two modes
//!
//! Which one a page gets is not a setting. It is decided by who got there
//! first, and the program reports it per page.
//!
//! | | [`Mode::Owned`] | [`Mode::Mirrored`] |
//! |---|---|---|
//! | When | this started before the writer | the writer was already running |
//! | Cost | nothing, ever | a comparison per look, a copy per change |
//! | Latency | none — the same memory | up to one interval |
//!
//! `Owned` happens because `CreateFileMappingW` with a name nobody has taken
//! creates the section *backed by the file it was given*. `Mirrored` happens
//! because the same call with a name that **is** taken quietly hands back the
//! existing section and ignores the file — so if the writer started first,
//! there is nothing to do but open it and copy.
//!
//! Mirroring is what makes the start-up order stop mattering, which is the
//! thing people actually trip over.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod announce;
pub mod cli;
#[cfg(unix)]
pub mod launch;
pub mod liveness;
pub mod pacing;
pub mod page;
pub mod reader;
pub mod schedule;
pub mod shadow;
pub mod store;
pub mod watchdog;

#[cfg(windows)]
pub mod win;

pub use announce::{Mode, Note};
pub use liveness::Pulse;
pub use pacing::Pacing;
pub use page::Page;
pub use schedule::Schedule;
pub use shadow::Shadow;
pub use store::Store;
pub use watchdog::Watchdog;

/// This program's version, as published in the note it leaves.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version, findable inside the built `.exe` without running it.
///
/// A Linux program that has a `wineshm.exe` sitting next to it cannot ask it
/// anything: it is a Windows binary, and running it to find out is the thing
/// being decided. So the answer travels in the file, behind a prefix
/// distinctive enough that scanning for it cannot match anything else.
///
/// `#[used]` keeps it through dead-code elimination. Nothing reads this
/// static, which is exactly the point.
#[used]
static VERSION_MARKER: &[u8] =
    concat!("WINESHM-VERSION=", env!("CARGO_PKG_VERSION"), ";").as_bytes();

/// The marker's prefix, for whoever is doing the scanning.
pub const VERSION_MARKER_PREFIX: &str = "WINESHM-VERSION=";

/// Find the version inside the bytes of a `wineshm.exe`.
///
/// Given for the same reason the marker exists: so a program shipping this
/// alongside itself can say "the bridge beside me is 0.1.0 and I need 0.2.0"
/// rather than "something is wrong".
pub fn version_in_binary(bytes: &[u8]) -> Option<String> {
    let needle = VERSION_MARKER_PREFIX.as_bytes();
    let start = bytes
        .windows(needle.len())
        .position(|window| window == needle)?
        + needle.len();
    let rest = &bytes[start..];
    let end = rest.iter().position(|byte| *byte == b';')?;
    core::str::from_utf8(&rest[..end]).ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_can_be_found_in_a_blob_of_bytes() {
        let mut blob = b"....some other content....".to_vec();
        blob.extend_from_slice(VERSION_MARKER);
        blob.extend_from_slice(b"....trailing....");

        assert_eq!(version_in_binary(&blob).as_deref(), Some(VERSION));
    }

    /// The version must be the text up to the `;`, not the empty string
    /// before it and not everything after it.
    #[test]
    fn the_version_read_back_is_exactly_what_was_written() {
        let mut blob = b"prefix".to_vec();
        blob.extend_from_slice(b"WINESHM-VERSION=1.2.3;and then some trailing bytes");
        let found = version_in_binary(&blob).expect("a version");
        assert_eq!(found, "1.2.3");
        assert!(!found.is_empty());
        assert!(!found.contains(';'));
    }

    #[test]
    fn a_binary_without_the_marker_reports_nothing_rather_than_guessing() {
        assert_eq!(version_in_binary(b"no marker in here"), None);
        assert_eq!(version_in_binary(b""), None);
    }

    #[test]
    fn a_marker_that_never_ends_is_not_a_version() {
        let mut blob = VERSION_MARKER_PREFIX.as_bytes().to_vec();
        blob.extend_from_slice(b"0.1.0 and then nothing");
        assert_eq!(version_in_binary(&blob), None);
    }

    #[test]
    fn the_marker_says_the_same_thing_the_crate_does() {
        assert_eq!(
            version_in_binary(VERSION_MARKER).as_deref(),
            Some(VERSION),
            "the marker and the crate version have drifted"
        );
    }

    /// The first marker wins, which matters if the blob contains the string
    /// twice — a binary that embeds its own help text, for instance.
    #[test]
    fn the_first_marker_is_the_one_read() {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"WINESHM-VERSION=1.2.3;");
        blob.extend_from_slice(b"WINESHM-VERSION=9.9.9;");
        assert_eq!(version_in_binary(&blob).as_deref(), Some("1.2.3"));
    }
}
