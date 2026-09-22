//! The note a running bridge leaves, so the Linux side knows what it is
//! talking to.
//!
//! **Because there is no other channel.** The bridge is a Windows process
//! inside a Wine prefix; the program reading the pages is a Linux process
//! outside it. They share exactly one thing — a directory — so the note goes
//! in the directory.
//!
//! Every failure that costs somebody an evening is a version mismatch that
//! looks like a hang: a reader waiting for a page a bridge of another vintage
//! is publishing at another size. A note that says which bridge, which pages,
//! and how each one is being served turns that into a sentence.

use crate::page::Page;
use core::fmt;

/// The file the note is written to, inside the same directory as the pages.
pub const FILE: &str = "wineshm.info";

/// The note's own shape. Raised when a key is added or removed, not when the
/// program changes — a reader keys off this, not off the version.
pub const FORMAT: u32 = 1;

/// How one page is being served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// This bridge created the Win32 section, backed by the file. The writer
    /// writes straight into it and nothing is copied, ever.
    Owned,
    /// Somebody else had already created the section, so it is opened and
    /// copied. Costs a comparison per look and a copy per change.
    Mirrored,
}

impl Mode {
    /// The word used in the note and in the program's own output.
    pub fn word(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Mirrored => "mirrored",
        }
    }

    /// Read one back.
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "owned" => Some(Self::Owned),
            "mirrored" => Some(Self::Mirrored),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `pad` rather than `write_str`, so `{:<9}` means what it says: a
        // custom `Display` that writes straight to the formatter silently
        // ignores width, and the table came out ragged.
        f.pad(self.word())
    }
}

/// What a running bridge is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The version of the program that wrote it.
    pub version: String,
    /// [`FORMAT`], so a reader can refuse a note it does not understand.
    pub format: u32,
    /// The Windows process id, for somebody trying to find it.
    pub pid: u32,
    /// Every page, with the size published and how it is served.
    pub pages: Vec<(Page, Mode)>,
}

impl Note {
    /// Render it. One `key=value` per line, values never containing a newline.
    ///
    /// Hand-rolled rather than a serialisation crate: it is nine lines, it has
    /// to be readable with `cat` by somebody debugging at midnight, and a
    /// dependency here is paid for in a binary that runs under Wine.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("format={}\n", self.format));
        out.push_str(&format!("version={}\n", self.version));
        out.push_str(&format!("pid={}\n", self.pid));
        for (page, mode) in &self.pages {
            out.push_str(&format!("page={}:{}:{}\n", page.name, page.bytes, mode));
        }
        out
    }

    /// Read one back, or `None` if it is not a note this understands.
    ///
    /// Unknown keys are skipped rather than refused, so a later version adding
    /// a line does not make an older reader call the file corrupt.
    pub fn parse(text: &str) -> Option<Self> {
        let mut format = None;
        let mut version = None;
        let mut pid = None;
        let mut pages = Vec::new();

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "format" => format = value.trim().parse().ok(),
                "version" => version = Some(value.trim().to_string()),
                "pid" => pid = value.trim().parse().ok(),
                "page" => {
                    let (spec, mode) = value.trim().rsplit_once(':')?;
                    pages.push((Page::parse(spec).ok()?, Mode::from_word(mode)?));
                }
                _ => {}
            }
        }

        Some(Self {
            format: format?,
            version: version?,
            pid: pid?,
            pages,
        })
    }

    /// Whether a reader of this format can trust the rest of the file.
    pub fn is_understood(&self) -> bool {
        self.format == FORMAT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> Note {
        Note {
            version: "0.1.0".to_string(),
            format: FORMAT,
            pid: 4321,
            pages: vec![
                (
                    Page {
                        name: "acpmf_physics".to_string(),
                        bytes: 2048,
                    },
                    Mode::Owned,
                ),
                (
                    Page {
                        name: "acpmf_static".to_string(),
                        bytes: 2048,
                    },
                    Mode::Mirrored,
                ),
            ],
        }
    }

    #[test]
    fn a_note_survives_being_written_and_read_back() {
        let original = note();
        let read = Note::parse(&original.render()).expect("parses");
        assert_eq!(read, original);
    }

    #[test]
    fn the_note_is_readable_by_a_person() {
        let text = note().render();
        assert!(text.contains("version=0.1.0"), "{text}");
        assert!(text.contains("page=acpmf_physics:2048:owned"), "{text}");
        assert!(text.contains("page=acpmf_static:2048:mirrored"), "{text}");
        // Every line is a key and a value, so `cat` is a usable tool.
        for line in text.lines() {
            assert!(line.contains('='), "{line:?}");
        }
    }

    #[test]
    fn a_later_version_adding_a_line_does_not_break_this_one() {
        let mut text = note().render();
        text.push_str("something-new=7\n");
        assert_eq!(Note::parse(&text), Some(note()));
    }

    #[test]
    fn a_note_missing_what_it_must_have_is_refused() {
        assert_eq!(Note::parse(""), None);
        assert_eq!(Note::parse("format=1\nversion=0.1.0\n"), None, "no pid");
        assert_eq!(Note::parse("version=0.1.0\npid=1\n"), None, "no format");
        assert_eq!(Note::parse("format=1\npid=1\n"), None, "no version");
    }

    #[test]
    fn a_page_line_that_makes_no_sense_is_refused_rather_than_skipped() {
        let text = "format=1\nversion=0.1.0\npid=1\npage=nonsense\n";
        assert_eq!(Note::parse(text), None);
        let text = "format=1\nversion=0.1.0\npid=1\npage=a:2048:sideways\n";
        assert_eq!(Note::parse(text), None);
        let text = "format=1\nversion=0.1.0\npid=1\npage=a:0:owned\n";
        assert_eq!(Note::parse(text), None);
    }

    #[test]
    fn a_note_with_no_pages_is_still_a_note() {
        let text = "format=1\nversion=0.1.0\npid=9\n";
        let read = Note::parse(text).expect("parses");
        assert!(read.pages.is_empty());
        assert_eq!(read.pid, 9);
    }

    #[test]
    fn a_format_from_another_release_is_read_but_not_trusted() {
        let text = format!("format={}\nversion=0.1.0\npid=1\n", FORMAT + 1);
        let read = Note::parse(&text).expect("parses");
        assert!(!read.is_understood());
        assert!(note().is_understood());
    }

    /// A `Display` that ignores width makes every table using it ragged.
    #[test]
    fn a_mode_obeys_the_width_it_is_given() {
        assert_eq!(format!("[{:<9}]", Mode::Owned), "[owned    ]");
        assert_eq!(format!("[{:>9}]", Mode::Mirrored), "[ mirrored]");
    }

    #[test]
    fn both_modes_survive_the_round_trip_as_words() {
        for mode in [Mode::Owned, Mode::Mirrored] {
            assert_eq!(Mode::from_word(mode.word()), Some(mode));
            assert_eq!(mode.to_string(), mode.word());
        }
        assert_eq!(Mode::from_word("halfway"), None);
    }

    /// A name with a colon in it is legal, and the mode is after the last one.
    #[test]
    fn a_page_name_containing_a_colon_still_reads_back() {
        let odd = Note {
            pages: vec![(
                Page {
                    name: "Session:12".to_string(),
                    bytes: 64,
                },
                Mode::Owned,
            )],
            ..note()
        };
        assert_eq!(Note::parse(&odd.render()), Some(odd));
    }

    #[test]
    fn junk_between_the_lines_is_ignored() {
        let text = "format=1\n\n   \nversion=0.1.0\nnot a key value line\npid=3\n";
        assert!(Note::parse(text).is_some());
    }
}
