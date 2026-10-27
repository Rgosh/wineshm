//! Where the program's window is, published beside its pages.
//!
//! **Because nothing outside the prefix can ask.** A Linux program drawing
//! something over a game — a lap timer, a delta, a warning — has to know where
//! the game's window is and whether anybody can see it. Under Wine that is a
//! question about a Win32 window, and the only process that can put it to
//! Win32 is one inside the prefix. This one already is.
//!
//! With `--window NAME`, a thread looks for the top-level window of a process
//! by that executable name ten times a second and writes what it found into
//! one small file beside the pages: where the client area is, whether it is
//! minimised, and whether it is the window in front. A reader polls the file;
//! nothing is pushed, and a reader that never looks costs nothing.
//!
//! # The record
//!
//! Sixty-four bytes, little-endian, written whole on every change and once a
//! second besides, so its stamp says the writer is alive:
//!
//! | offset | type | what |
//! |---:|---|---|
//! | 0 | u32 | [`MAGIC`] |
//! | 4 | u32 | [`FORMAT`] |
//! | 8 | u32 | sequence, raised on every write |
//! | 12 | u32 | [`flags`] |
//! | 16 | i32 × 4 | client area: x, y, width, height, in Win32 screen pixels |
//! | 32 | i32 × 2 | the virtual screen's top-left, in the same pixels |
//! | 40 | u64 | when it was written, milliseconds since the Unix epoch |
//! | 48 | 12 bytes | reserved, zero |
//! | 60 | u32 | the sequence again |
//!
//! **The sequence is written twice**, first and last. A reader that copies the
//! record while a write is landing gets two different numbers and reads
//! again — the same tear check the game's own pages carry.
//!
//! The virtual screen's corner is there because Win32 puts the *primary*
//! monitor at the origin and X does not: with a monitor to the left of the
//! primary, Win32 coordinates there are negative. Subtracting the corner gives
//! a position on the X root window, which is what a Linux program places its
//! own windows by.

/// The file the record is written to, inside the publishing directory.
pub const FILE: &str = "wineshm.window";

/// The first four bytes: `WSWN`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"WSWN");

/// The record's shape. Raised when a field moves, not when the program changes.
pub const FORMAT: u32 = 1;

/// The record's length in bytes.
pub const SIZE: usize = 64;

/// What the record says about the window, one bit each.
pub mod flags {
    /// A window was found at all. Everything else is meaningless without it.
    pub const FOUND: u32 = 1;
    /// It is minimised.
    pub const MINIMISED: u32 = 1 << 1;
    /// It is the foreground window — the one the player is looking at.
    pub const FOREGROUND: u32 = 1 << 2;
}

/// What was found, as the record carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Seen {
    /// A window of the named program exists.
    pub found: bool,
    /// It is minimised.
    pub minimised: bool,
    /// It is in front.
    pub foreground: bool,
    /// The client area — what the program draws into — in Win32 screen pixels.
    pub x: i32,
    /// See [`Seen::x`].
    pub y: i32,
    /// See [`Seen::x`].
    pub width: i32,
    /// See [`Seen::x`].
    pub height: i32,
    /// The virtual screen's top-left corner, so a reader can translate.
    pub screen_x: i32,
    /// See [`Seen::screen_x`].
    pub screen_y: i32,
}

impl Seen {
    /// Nothing found.
    pub fn nothing() -> Self {
        Self::default()
    }

    /// Whether somebody could be looking at it right now.
    pub fn in_view(&self) -> bool {
        self.found && !self.minimised && self.foreground && self.width > 0 && self.height > 0
    }

    /// The client area's corner on the X root window.
    pub fn on_root(&self) -> (i32, i32) {
        (self.x - self.screen_x, self.y - self.screen_y)
    }

    fn flags(&self) -> u32 {
        let mut bits = 0;
        if self.found {
            bits |= flags::FOUND;
        }
        if self.minimised {
            bits |= flags::MINIMISED;
        }
        if self.foreground {
            bits |= flags::FOREGROUND;
        }
        bits
    }

    /// The record, with this sequence number and this stamp.
    pub fn encode(&self, sequence: u32, written_ms: u64) -> [u8; SIZE] {
        let mut out = [0u8; SIZE];
        let mut put = |at: usize, bytes: &[u8]| {
            out[at..at + bytes.len()].copy_from_slice(bytes);
        };
        put(0, &MAGIC.to_le_bytes());
        put(4, &FORMAT.to_le_bytes());
        put(8, &sequence.to_le_bytes());
        put(12, &self.flags().to_le_bytes());
        put(16, &self.x.to_le_bytes());
        put(20, &self.y.to_le_bytes());
        put(24, &self.width.to_le_bytes());
        put(28, &self.height.to_le_bytes());
        put(32, &self.screen_x.to_le_bytes());
        put(36, &self.screen_y.to_le_bytes());
        put(40, &written_ms.to_le_bytes());
        put(60, &sequence.to_le_bytes());
        out
    }
}

/// A record, read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// What it says.
    pub seen: Seen,
    /// Its sequence number.
    pub sequence: u32,
    /// When it was written, milliseconds since the Unix epoch.
    pub written_ms: u64,
}

/// Why a record was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Shorter than a record.
    Short,
    /// Not one of these at all.
    NotARecord,
    /// A shape this build does not know.
    Format(u32),
    /// Caught half-written; read again.
    Torn,
}

/// Read a record back, refusing anything that is not one whole.
pub fn decode(bytes: &[u8]) -> Result<Record, Unread> {
    if bytes.len() < SIZE {
        return Err(Unread::Short);
    }
    let u32_at = |at: usize| {
        let mut word = [0u8; 4];
        word.copy_from_slice(&bytes[at..at + 4]);
        u32::from_le_bytes(word)
    };
    let i32_at = |at: usize| u32_at(at) as i32;
    if u32_at(0) != MAGIC {
        return Err(Unread::NotARecord);
    }
    if u32_at(4) != FORMAT {
        return Err(Unread::Format(u32_at(4)));
    }
    let sequence = u32_at(8);
    if u32_at(60) != sequence {
        return Err(Unread::Torn);
    }
    let bits = u32_at(12);
    let mut stamp = [0u8; 8];
    stamp.copy_from_slice(&bytes[40..48]);
    Ok(Record {
        seen: Seen {
            found: bits & flags::FOUND != 0,
            minimised: bits & flags::MINIMISED != 0,
            foreground: bits & flags::FOREGROUND != 0,
            x: i32_at(16),
            y: i32_at(20),
            width: i32_at(24),
            height: i32_at(28),
            screen_x: i32_at(32),
            screen_y: i32_at(36),
        },
        sequence,
        written_ms: u64::from_le_bytes(stamp),
    })
}

/// The executable names `--window` was given, split on commas.
///
/// **Several, because one game is often two programs.** Competizione is
/// started as `acc.exe` and draws as `AC2-Win64-Shipping.exe`; naming both
/// costs nothing and finds whichever owns the window.
pub fn names(list: &str) -> Vec<String> {
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether a process's image path is one of the names asked for.
///
/// Compared on the file name alone and without regard to case, because Win32
/// hands back a full path in whatever case the file system has, and nobody
/// types one.
pub fn is_named(image: &str, names: &[String]) -> bool {
    let file = image.rsplit(['\\', '/']).next().unwrap_or(image).trim();
    names.iter().any(|name| name.eq_ignore_ascii_case(file))
}

/// Of several windows a program owns, the one it plays in: the largest.
///
/// A game owns a launcher, a splash, a console and its real window, and the
/// one somebody is looking at is the big one. Hidden and zero-sized windows
/// are not candidates at all; a minimised one still is, because minimised is
/// one of the answers this exists to give.
pub fn pick(candidates: &[Seen]) -> Seen {
    candidates
        .iter()
        .filter(|seen| seen.found && (seen.minimised || (seen.width > 0 && seen.height > 0)))
        .max_by_key(|seen| {
            // A minimised window reports no size; prefer a visible one over it,
            // and among visible ones the largest.
            (
                !seen.minimised,
                i64::from(seen.width) * i64::from(seen.height),
            )
        })
        .copied()
        .unwrap_or_default()
}

/// The time now as the record stamps it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(width: i32, height: i32) -> Seen {
        Seen {
            found: true,
            minimised: false,
            foreground: true,
            x: 100,
            y: 50,
            width,
            height,
            screen_x: -1920,
            screen_y: 0,
        }
    }

    /// What is written is what is read.
    #[test]
    fn a_record_reads_back_as_it_was_written() {
        let seen = window(2560, 1440);
        let bytes = seen.encode(7, 1_700_000_000_000);
        assert_eq!(bytes.len(), SIZE);
        let read = decode(&bytes).expect("a whole record");
        assert_eq!(read.seen, seen);
        assert_eq!(read.sequence, 7);
        assert_eq!(read.written_ms, 1_700_000_000_000);
    }

    /// **A record caught mid-write is refused**, not read as half of two.
    #[test]
    fn a_torn_record_is_refused() {
        let mut bytes = window(1920, 1080).encode(7, 1);
        bytes[60..64].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(decode(&bytes), Err(Unread::Torn));
    }

    #[test]
    fn anything_else_is_refused_by_name() {
        assert_eq!(decode(&[0u8; 10]), Err(Unread::Short));
        assert_eq!(decode(&[0u8; SIZE]), Err(Unread::NotARecord));
        let mut later = window(10, 10).encode(1, 1);
        later[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode(&later), Err(Unread::Format(2)));
    }

    /// Win32 puts the primary monitor at the origin; X puts the leftmost one
    /// there. The corner of the virtual screen is the difference.
    #[test]
    fn a_position_translates_to_the_root_window() {
        let seen = window(1920, 1080);
        assert_eq!(seen.on_root(), (2020, 50));
    }

    /// In view means found, not minimised, in front, and with a size.
    #[test]
    fn in_view_needs_all_four() {
        assert!(window(10, 10).in_view());
        assert!(
            !Seen {
                minimised: true,
                ..window(10, 10)
            }
            .in_view()
        );
        assert!(
            !Seen {
                foreground: false,
                ..window(10, 10)
            }
            .in_view()
        );
        assert!(!window(0, 0).in_view());
        assert!(!Seen::nothing().in_view());
    }

    #[test]
    fn names_are_split_and_matched_on_the_file_alone() {
        let names = names(" acc.exe, AC2-Win64-Shipping.exe ,");
        assert_eq!(names, vec!["acc.exe", "AC2-Win64-Shipping.exe"]);
        assert!(is_named(
            r"C:\Games\ACC\AC2\Binaries\Win64\ac2-win64-shipping.EXE",
            &names
        ));
        assert!(!is_named(r"C:\Windows\explorer.exe", &names));
    }

    /// The window played in is the large one, and a visible one beats a
    /// minimised one that reports no size.
    #[test]
    fn the_largest_window_is_the_game() {
        let splash = window(640, 360);
        let game = window(2560, 1440);
        assert_eq!(pick(&[splash, game]), game);

        let tucked = Seen {
            minimised: true,
            width: 0,
            height: 0,
            ..window(0, 0)
        };
        assert_eq!(pick(&[tucked]), tucked, "minimised is still an answer");
        assert_eq!(pick(&[tucked, splash]), splash);
        assert_eq!(pick(&[]), Seen::nothing());
    }
}
