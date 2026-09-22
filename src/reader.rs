//! The Linux side, for the program that wants the data.
//!
//! **Because "it is just a file" is true and still not enough.** A reader has
//! to know whether anything is publishing, whether the block it wants is one
//! of them, and whether the file it is about to read is the size it expects —
//! and if it skips those, the failure is not an error but a page of zeroes
//! that looks like a car sitting still.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use wineshm::{Page, Store, reader::Reader};
//!
//! let store = Store::default();
//! let page = Page { name: "acpmf_physics".into(), bytes: 2048 };
//! let reader = Reader::open(&store, &page)?;
//!
//! let mut buffer = [0u8; 2048];
//! reader.read_into(&mut buffer)?;
//! # Ok(())
//! # }
//! ```

use crate::announce::{FILE, Note};
#[cfg(unix)]
use crate::page::Page;
use crate::store::Store;
#[cfg(unix)]
use std::io;

// `Reader` positions its reads with the Unix `pread`, so it is the one part of
// this module that is not available everywhere. Asking what is published —
// which is the note, an ordinary file — is available on both, and the bridge
// itself needs it: it runs as a Windows process and has to notice another
// bridge already publishing.
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::unix::fs::FileExt;

/// An open page, ready to be read as often as you like.
#[cfg(unix)]
#[derive(Debug)]
pub struct Reader {
    file: File,
    page: Page,
}

#[cfg(unix)]
impl Reader {
    /// Open a published page.
    ///
    /// The size is checked against the file's, because a length that does not
    /// match is the one failure that reads as data: a writer of another
    /// vintage publishing a shorter block leaves the tail of your buffer
    /// holding whatever it held before.
    pub fn open(store: &Store, page: &Page) -> io::Result<Self> {
        let path = store.path(page);
        let file = File::open(&path)?;
        let found = file.metadata()?.len();
        if found != page.bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is {found} bytes and {} was expected — the two sides are different \
                     versions",
                    path.display(),
                    page.bytes
                ),
            ));
        }
        Ok(Self {
            file,
            page: page.clone(),
        })
    }

    /// Which page this is.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// Read the whole block into a buffer of exactly its size.
    ///
    /// `&self` rather than `&mut self`: this is a positioned read with no file
    /// cursor to move, so one reader can be shared and read from several
    /// threads without any of them stepping on another's offset.
    pub fn read_into(&self, into: &mut [u8]) -> io::Result<()> {
        if into.len() != self.page.bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a buffer of {} bytes for a {}-byte page",
                    into.len(),
                    self.page.bytes
                ),
            ));
        }
        self.file.read_exact_at(into, 0)
    }

    /// The whole block, in a fresh `Vec`.
    ///
    /// For the call that happens once. In a loop, keep a buffer and use
    /// [`Self::read_into`] — allocating sixty times a second to hold two
    /// kilobytes is work nobody asked for.
    pub fn read(&self) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0u8; self.page.bytes];
        self.read_into(&mut bytes)?;
        Ok(bytes)
    }

    /// Read part of the block, for a reader that wants one field.
    pub fn read_at(&self, offset: u64, into: &mut [u8]) -> io::Result<()> {
        let end = offset.saturating_add(into.len() as u64);
        if end > self.page.bytes as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{end} bytes asked for from a {}-byte page", self.page.bytes),
            ));
        }
        self.file.read_exact_at(into, offset)
    }
}

/// What is publishing in this directory, if anything.
///
/// `None` when nothing is, or when the note is from a version this does not
/// understand — both of which mean "do not trust the files beside it".
///
/// **This does not say whether the bridge is still alive.** A bridge that was
/// killed leaves its note and its pages exactly as they were, and they read as
/// a session that is simply very quiet. Use [`pulse`] for that, or
/// [`live_publishing`] to ask both questions at once.
pub fn publishing(store: &Store) -> Option<Note> {
    let text = std::fs::read_to_string(store.dir().join(FILE)).ok()?;
    Note::parse(&text).filter(Note::is_understood)
}

/// Whether the bridge that wrote the note is still there.
///
/// A running bridge touches its note every [`crate::liveness::BEAT`], so the
/// file's modified time is a pulse. `None` when there is no note to take one
/// from.
pub fn pulse(store: &Store) -> Option<crate::Pulse> {
    pulse_after(store, crate::liveness::STALE)
}

/// The same, for a caller with its own patience.
pub fn pulse_after(store: &Store, stale_after: std::time::Duration) -> Option<crate::Pulse> {
    let touched = std::fs::metadata(store.dir().join(FILE))
        .ok()?
        .modified()
        .ok()?;
    let now = std::time::SystemTime::now();
    Some(match now.duration_since(touched) {
        Ok(age) => crate::liveness::pulse(age, false, stale_after),
        // The note is dated ahead of this clock, which a clock change does.
        Err(_) => crate::liveness::pulse(std::time::Duration::ZERO, true, stale_after),
    })
}

/// What is publishing here, and only if something still is.
///
/// The call to reach for when a program wants live data: a note left behind by
/// a bridge that died answers `None` rather than handing back a session that
/// ended hours ago with every number in it looking real.
pub fn live_publishing(store: &Store) -> Option<Note> {
    let note = publishing(store)?;
    pulse(store)?.is_worth_reading().then_some(note)
}

/// Whether a named block is published here, and at what size.
pub fn published_size(store: &Store, name: &str) -> Option<usize> {
    publishing(store)?
        .pages
        .into_iter()
        .find(|(page, _)| page.name == name)
        .map(|(page, _)| page.bytes)
}

// Two attributes rather than `all(test, unix)`: clippy recognises a test
// module by a bare `cfg(test)`, and the lint exemptions in clippy.toml —
// `expect` and `unwrap` are allowed in tests — are skipped without it.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use crate::announce::{FORMAT, Mode};
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wineshm-reader-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn page(name: &str, bytes: usize) -> Page {
        Page {
            name: name.to_string(),
            bytes,
        }
    }

    fn publish(dir: &std::path::Path, pages: &[Page]) {
        let note = Note {
            version: crate::VERSION.to_string(),
            format: FORMAT,
            pid: 1,
            pages: pages.iter().map(|p| (p.clone(), Mode::Owned)).collect(),
        };
        std::fs::write(dir.join(FILE), note.render()).expect("note");
        for page in pages {
            std::fs::write(dir.join(&page.name), vec![0u8; page.bytes]).expect("page");
        }
    }

    #[test]
    fn a_published_page_can_be_opened_and_read() {
        let dir = scratch("read");
        let spec = page("thing", 16);
        publish(&dir, std::slice::from_ref(&spec));
        std::fs::write(dir.join("thing"), (0..16u8).collect::<Vec<_>>()).expect("fill");

        let store = Store::at(&dir);
        let reader = Reader::open(&store, &spec).expect("open");
        assert_eq!(reader.page(), &spec);
        assert_eq!(reader.read().expect("read"), (0..16u8).collect::<Vec<_>>());

        let mut into = [0u8; 16];
        reader.read_into(&mut into).expect("read_into");
        assert_eq!(into[15], 15);
    }

    /// Reading the same page twice must give the same bytes: a positioned
    /// read moves no cursor, which is what lets a reader be shared.
    #[test]
    fn reading_twice_does_not_walk_off_the_end() {
        let dir = scratch("twice");
        let spec = page("thing", 8);
        publish(&dir, std::slice::from_ref(&spec));
        std::fs::write(dir.join("thing"), [9u8; 8]).expect("fill");

        let reader = Reader::open(&Store::at(&dir), &spec).expect("open");
        assert_eq!(reader.read().expect("first"), vec![9u8; 8]);
        assert_eq!(reader.read().expect("second"), vec![9u8; 8]);
        assert_eq!(reader.read().expect("third"), vec![9u8; 8]);
    }

    /// The failure that reads as data rather than as an error.
    #[test]
    fn a_page_of_the_wrong_size_is_refused_at_open() {
        let dir = scratch("wrong-size");
        let spec = page("thing", 2048);
        publish(&dir, &[page("thing", 712)]);

        let why = Reader::open(&Store::at(&dir), &spec).expect_err("refused");
        assert_eq!(why.kind(), io::ErrorKind::InvalidData);
        assert!(why.to_string().contains("712"), "{why}");
        assert!(why.to_string().contains("2048"), "{why}");
    }

    #[test]
    fn a_page_that_is_not_published_is_an_ordinary_not_found() {
        let dir = scratch("missing");
        let why = Reader::open(&Store::at(&dir), &page("nothing", 16)).expect_err("refused");
        assert_eq!(why.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_buffer_of_the_wrong_size_is_refused_rather_than_part_filled() {
        let dir = scratch("buffer");
        let spec = page("thing", 16);
        publish(&dir, std::slice::from_ref(&spec));
        let reader = Reader::open(&Store::at(&dir), &spec).expect("open");

        let mut too_small = [0u8; 8];
        assert!(reader.read_into(&mut too_small).is_err());
        let mut too_big = [0u8; 32];
        assert!(reader.read_into(&mut too_big).is_err());
    }

    #[test]
    fn part_of_a_page_can_be_read_and_not_past_its_end() {
        let dir = scratch("partial");
        let spec = page("thing", 16);
        publish(&dir, std::slice::from_ref(&spec));
        std::fs::write(dir.join("thing"), (0..16u8).collect::<Vec<_>>()).expect("fill");
        let reader = Reader::open(&Store::at(&dir), &spec).expect("open");

        let mut four = [0u8; 4];
        reader.read_at(4, &mut four).expect("read_at");
        assert_eq!(four, [4, 5, 6, 7]);

        // Right up to the end is allowed; one past it is not.
        assert!(reader.read_at(12, &mut four).is_ok());
        assert!(reader.read_at(13, &mut four).is_err());
        assert!(reader.read_at(u64::MAX, &mut four).is_err());
    }

    #[test]
    fn what_is_publishing_can_be_asked_without_opening_anything() {
        let dir = scratch("publishing");
        let pages = vec![page("one", 16), page("two", 32)];
        publish(&dir, &pages);
        let store = Store::at(&dir);

        let note = publishing(&store).expect("a note");
        assert_eq!(note.pages.len(), 2);
        assert_eq!(published_size(&store, "two"), Some(32));
        assert_eq!(published_size(&store, "three"), None);
    }

    #[test]
    fn nothing_publishing_is_not_an_error() {
        let dir = scratch("nothing");
        assert!(publishing(&Store::at(&dir)).is_none());
        assert_eq!(published_size(&Store::at(&dir), "anything"), None);
    }

    /// A note from a format this build does not know is not to be trusted,
    /// and neither are the files beside it.
    #[test]
    fn a_note_from_another_format_is_not_believed() {
        let dir = scratch("other-format");
        std::fs::write(
            dir.join(FILE),
            format!("format={}\nversion=9.9.9\npid=1\n", FORMAT + 1),
        )
        .expect("note");
        assert!(publishing(&Store::at(&dir)).is_none());
    }
}
