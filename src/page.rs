//! Which blocks of memory to expose, and how big each one is.
//!
//! **The list is data, not code.** The bridge this replaces had its pages
//! written into a `const` array, so using it for anything but one pair of
//! racing games meant forking it. Here a page is a name and a size, and a
//! caller — a command line, a config file, another crate — hands over as many
//! as it likes.

use core::fmt;

/// One named block of shared memory to expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The Win32 object name the Windows program uses.
    pub name: String,
    /// How many bytes to map.
    pub bytes: usize,
}

/// The largest page this will agree to map, 64 MiB.
///
/// Not a technical limit. A typo in a size is otherwise a request to allocate
/// whatever number was typed, and `/dev/shm` is RAM: `--page x:999999999999`
/// should be refused rather than attempted.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// The longest name, matching Win32's own limit for a kernel object name.
pub const MAX_NAME: usize = 255;

/// Why a `name:size` could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadPage {
    /// No `:` at all.
    NoSize,
    /// The name was empty.
    EmptyName,
    /// The name is longer than Win32 allows.
    NameTooLong(usize),
    /// A `\` or a NUL, neither of which can be in an object name.
    NameHasSeparator(char),
    /// The size was not a number, or carried a suffix that is not `K` or `M`.
    SizeNotANumber(String),
    /// A size of zero maps nothing.
    SizeIsZero,
    /// Bigger than [`MAX_BYTES`].
    SizeTooBig(usize),
}

impl fmt::Display for BadPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSize => write!(f, "expected NAME:SIZE, with a colon between them"),
            Self::EmptyName => write!(f, "the name is empty"),
            Self::NameTooLong(was) => {
                write!(f, "the name is {was} characters; Win32 allows {MAX_NAME}")
            }
            Self::NameHasSeparator(what) => {
                write!(f, "a shared memory name cannot contain {what:?}")
            }
            Self::SizeNotANumber(what) => {
                write!(f, "{what:?} is not a size — try 2048, 16K or 1M")
            }
            Self::SizeIsZero => write!(f, "a size of zero would map nothing"),
            Self::SizeTooBig(was) => {
                write!(f, "{was} bytes is past the {MAX_BYTES} this will map")
            }
        }
    }
}

impl std::error::Error for BadPage {}

impl Page {
    /// Read one `NAME:SIZE`, where the size may carry a `K` or `M` suffix.
    ///
    /// The name is everything before the **last** colon, because Win32 object
    /// names may contain one — `Global\Something` is rare but `Local\x` is
    /// not, and a name with a colon in it is legal. Splitting on the first
    /// colon would quietly mangle those.
    pub fn parse(text: &str) -> Result<Self, BadPage> {
        let (name, size) = text.rsplit_once(':').ok_or(BadPage::NoSize)?;
        Ok(Self {
            name: check_name(name)?,
            bytes: parse_size(size)?,
        })
    }
}

fn check_name(name: &str) -> Result<String, BadPage> {
    if name.is_empty() {
        return Err(BadPage::EmptyName);
    }
    // Counted in characters rather than bytes: the limit Win32 states is on
    // the name, and a name is UTF-16 by the time it gets there.
    let length = name.chars().count();
    if length > MAX_NAME {
        return Err(BadPage::NameTooLong(length));
    }
    if let Some(bad) = name.chars().find(|c| *c == '\\' || *c == '\0') {
        return Err(BadPage::NameHasSeparator(bad));
    }
    Ok(name.to_string())
}

fn parse_size(text: &str) -> Result<usize, BadPage> {
    let text = text.trim();
    let (digits, scale) = match text.as_bytes().last() {
        Some(b'K' | b'k') => (&text[..text.len() - 1], 1024),
        Some(b'M' | b'm') => (&text[..text.len() - 1], 1024 * 1024),
        _ => (text, 1),
    };

    let value: usize = digits
        .parse()
        .map_err(|_| BadPage::SizeNotANumber(text.to_string()))?;
    let bytes = value
        .checked_mul(scale)
        .ok_or(BadPage::SizeTooBig(usize::MAX))?;

    if bytes == 0 {
        return Err(BadPage::SizeIsZero);
    }
    if bytes > MAX_BYTES {
        return Err(BadPage::SizeTooBig(bytes));
    }
    Ok(bytes)
}

/// A ready-made list of pages, for a program somebody else has to support.
///
/// Presets exist so that the common case is one word rather than five flags,
/// and so that the sizes — which are facts about somebody else's program, and
/// wrong in a way nobody notices — are written down once.
pub fn preset(name: &str) -> Option<Vec<Page>> {
    let pages: &[(&str, usize)] = match name {
        // Assetto Corsa and Assetto Corsa Competizione publish the same four
        // names. The sizes are Competizione's, which are the larger of the
        // two, so one list serves both.
        "assetto-corsa" | "ac" | "acc" => &[
            ("acpmf_physics", 2048),
            ("acpmf_graphics", 2048),
            ("acpmf_static", 2048),
            ("acpmf_crewchief", 15660),
        ],
        // rFactor 2's plugin-published block.
        "rfactor2" | "rf2" => &[("$rFactor2SMMP_Telemetry$", 1024 * 1024)],
        _ => return None,
    };
    Some(
        pages
            .iter()
            .map(|(name, bytes)| Page {
                name: (*name).to_string(),
                bytes: *bytes,
            })
            .collect(),
    )
}

/// Which of a preset's blocks changes while the program is alive.
///
/// **The knowledge belongs with the preset, not with the caller.** A
/// `--heartbeat` has to name a block that moves constantly, and picking the
/// wrong one is not an error anybody sees: name a block that is written once a
/// session and every other block gets blanked in the middle of a live one.
/// Whoever wrote the preset knows which is which, so it is answered here and
/// applied automatically — see [`crate::cli::parse`].
///
/// `None` for a program whose blocks nobody has checked. Nothing is blanked
/// then, which is the old behaviour and the safe one.
pub fn preset_heartbeat(name: &str) -> Option<&'static str> {
    match name {
        // Assetto Corsa and Competizione rewrite the physics block on every
        // physics tick — three hundred times a second — and stop entirely
        // when the session ends. `acpmf_static` is the trap: it carries the
        // car and the track, is written once, and would have the bridge blank
        // a session that is still running.
        "assetto-corsa" | "ac" | "acc" => Some("acpmf_physics"),
        // rFactor 2 publishes one block, so it is its own heartbeat.
        "rfactor2" | "rf2" => Some("$rFactor2SMMP_Telemetry$"),
        _ => None,
    }
}

/// Every preset name, for the help text and for the test that keeps them in
/// step with it.
pub const PRESETS: &[&str] = &["assetto-corsa", "rfactor2"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_a_name_and_a_size() {
        assert_eq!(
            Page::parse("acpmf_physics:2048"),
            Ok(Page {
                name: "acpmf_physics".to_string(),
                bytes: 2048
            })
        );
    }

    #[test]
    fn sizes_may_be_written_in_kilobytes_or_megabytes() {
        assert_eq!(Page::parse("a:16K").map(|p| p.bytes), Ok(16 * 1024));
        assert_eq!(Page::parse("a:16k").map(|p| p.bytes), Ok(16 * 1024));
        assert_eq!(Page::parse("a:1M").map(|p| p.bytes), Ok(1024 * 1024));
        assert_eq!(Page::parse("a:1m").map(|p| p.bytes), Ok(1024 * 1024));
        // And a bare number is still bytes.
        assert_eq!(Page::parse("a:2048").map(|p| p.bytes), Ok(2048));
    }

    #[test]
    fn whitespace_around_a_size_is_forgiven() {
        assert_eq!(Page::parse("a: 2048 ").map(|p| p.bytes), Ok(2048));
    }

    /// **The last colon, not the first.** `Local\x:16` has a name with no
    /// colon, but a name may contain one, and splitting at the front would
    /// take half of it.
    #[test]
    fn the_size_is_after_the_last_colon() {
        assert_eq!(
            Page::parse("Session:12:2048"),
            Ok(Page {
                name: "Session:12".to_string(),
                bytes: 2048
            })
        );
    }

    #[test]
    fn a_page_without_a_size_is_refused() {
        assert_eq!(Page::parse("acpmf_physics"), Err(BadPage::NoSize));
    }

    #[test]
    fn an_empty_name_is_refused() {
        assert_eq!(Page::parse(":2048"), Err(BadPage::EmptyName));
    }

    #[test]
    fn a_name_longer_than_win32_allows_is_refused() {
        let long = "x".repeat(MAX_NAME + 1);
        assert_eq!(
            Page::parse(&format!("{long}:2048")),
            Err(BadPage::NameTooLong(MAX_NAME + 1))
        );
        // And exactly at the limit is allowed, which is the edge the check
        // above is one past.
        let edge = "x".repeat(MAX_NAME);
        assert_eq!(
            Page::parse(&format!("{edge}:2048")).map(|p| p.name.chars().count()),
            Ok(MAX_NAME)
        );
    }

    #[test]
    fn a_name_with_a_backslash_or_a_nul_is_refused() {
        assert_eq!(
            Page::parse("Global\\thing:2048"),
            Err(BadPage::NameHasSeparator('\\'))
        );
        assert_eq!(
            Page::parse("thing\0:2048"),
            Err(BadPage::NameHasSeparator('\0'))
        );
    }

    #[test]
    fn a_size_that_is_not_a_number_is_refused() {
        assert_eq!(
            Page::parse("a:lots"),
            Err(BadPage::SizeNotANumber("lots".to_string()))
        );
        assert_eq!(
            Page::parse("a:-1"),
            Err(BadPage::SizeNotANumber("-1".to_string()))
        );
        // A suffix on its own is not a size either.
        assert_eq!(
            Page::parse("a:K"),
            Err(BadPage::SizeNotANumber("K".to_string()))
        );
    }

    #[test]
    fn a_size_of_zero_is_refused() {
        assert_eq!(Page::parse("a:0"), Err(BadPage::SizeIsZero));
        assert_eq!(Page::parse("a:0K"), Err(BadPage::SizeIsZero));
    }

    /// `/dev/shm` is RAM, so a typo in a size is a request to eat it.
    #[test]
    fn a_size_past_the_ceiling_is_refused() {
        assert_eq!(
            Page::parse(&format!("a:{}", MAX_BYTES + 1)),
            Err(BadPage::SizeTooBig(MAX_BYTES + 1))
        );
        // Exactly at the ceiling is allowed.
        assert_eq!(
            Page::parse(&format!("a:{MAX_BYTES}")).map(|p| p.bytes),
            Ok(MAX_BYTES)
        );
        // And a suffix cannot be used to get past it by overflowing.
        assert!(matches!(
            Page::parse(&format!("a:{}M", usize::MAX)),
            Err(BadPage::SizeTooBig(_))
        ));
    }

    #[test]
    fn every_preset_in_the_list_can_actually_be_asked_for() {
        for name in PRESETS {
            let pages = preset(name).unwrap_or_default();
            assert!(!pages.is_empty(), "{name} is listed and answers nothing");
            for page in pages {
                assert!(page.bytes > 0 && page.bytes <= MAX_BYTES, "{page:?}");
                assert!(!page.name.is_empty(), "{page:?}");
            }
        }
    }

    /// The ceiling and the preset sizes are facts, and a fact written as
    /// arithmetic can be wrong by a factor of a thousand without looking it.
    #[test]
    fn the_sizes_are_the_numbers_they_are_meant_to_be() {
        assert_eq!(MAX_BYTES, 67_108_864, "64 MiB");
        assert_eq!(MAX_NAME, 255);

        let ac = preset("assetto-corsa").unwrap_or_default();
        let sizes: Vec<usize> = ac.iter().map(|page| page.bytes).collect();
        assert_eq!(sizes, vec![2048, 2048, 2048, 15660]);

        let rf2 = preset("rfactor2").unwrap_or_default();
        assert_eq!(rf2.first().map(|page| page.bytes), Some(1_048_576), "1 MiB");
    }

    /// **A heartbeat that names the wrong block is a silent fault.** It has
    /// to be one of the preset's own blocks, and it has to be one that moves
    /// — a static block would have the bridge blank a live session.
    #[test]
    fn every_preset_names_a_heartbeat_among_its_own_blocks() {
        for name in PRESETS {
            let pages = preset(name).unwrap_or_default();
            let beat = preset_heartbeat(name).unwrap_or_else(|| panic!("{name} has no heartbeat"));
            assert!(
                pages.iter().any(|page| page.name == beat),
                "{name}: heartbeat {beat} is not one of its pages"
            );
            assert_ne!(
                beat, "acpmf_static",
                "the static block is written once a session and is the one trap here"
            );
        }
    }

    #[test]
    fn the_short_names_answer_the_same_heartbeat() {
        assert_eq!(preset_heartbeat("ac"), preset_heartbeat("assetto-corsa"));
        assert_eq!(preset_heartbeat("rf2"), preset_heartbeat("rfactor2"));
        assert_eq!(preset_heartbeat("gran-turismo"), None);
    }

    #[test]
    fn a_preset_nobody_has_written_is_not_invented() {
        assert_eq!(preset("gran-turismo"), None);
    }

    #[test]
    fn the_short_names_reach_the_same_lists() {
        assert_eq!(preset("ac"), preset("assetto-corsa"));
        assert_eq!(preset("rf2"), preset("rfactor2"));
    }

    /// Every fault says which one it was, in words, rather than "invalid".
    #[test]
    fn each_refusal_explains_itself() {
        for bad in [
            BadPage::NoSize,
            BadPage::EmptyName,
            BadPage::NameTooLong(300),
            BadPage::NameHasSeparator('\\'),
            BadPage::SizeNotANumber("x".to_string()),
            BadPage::SizeIsZero,
            BadPage::SizeTooBig(99),
        ] {
            let said = bad.to_string();
            assert!(said.len() > 10, "{bad:?} said {said:?}");
            assert!(!said.contains("invalid"), "{said:?}");
        }
    }
}
