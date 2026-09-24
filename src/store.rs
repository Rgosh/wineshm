//! The Linux side: the files under `/dev/shm` that the pages live in.
//!
//! Everything here is ordinary file work, which is the point — it is the half
//! that can be tested on the machine you are reading this on, rather than only
//! inside a Wine prefix.

use crate::page::Page;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// Where Linux keeps its shared memory.
pub const SHM_DIR: &str = "/dev/shm";

/// The directory the pages are published in.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        Self::at(SHM_DIR)
    }
}

impl Store {
    /// A store in a directory of your choosing — a temporary one, in tests.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory itself.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where one page's bytes live.
    pub fn path(&self, page: &Page) -> PathBuf {
        self.dir.join(&page.name)
    }

    /// Whether this looks like a directory pages can be published in.
    pub fn is_usable(&self) -> bool {
        self.dir.is_dir()
    }

    /// Open a page's file, at exactly the size asked for, full of zeroes.
    ///
    /// Three things happen here and all three are load-bearing:
    ///
    /// **Created if missing**, which is the ordinary case.
    ///
    /// **Resized every time, not only on creation.** A file that already
    /// exists keeps the length it had, and the length is what a reader checks
    /// before mapping. A block that grew between two releases leaves the old,
    /// short file behind: the reader refuses the mapping, and every version
    /// number in sight says the two sides agree — because they do. It is the
    /// file that is stale.
    ///
    /// **Zeroed, not merely resized.** Setting a length the file already has
    /// changes not one byte, so a page left behind by a previous run survives
    /// into this one intact and reads as a live session. That is the worst
    /// failure this program can have: numbers nobody measured, in the right
    /// shape, with a plausible car and track in them. Zeroes are the state
    /// every reader already knows how to wait through.
    pub fn prepare(&self, page: &Page) -> io::Result<File> {
        let path = self.path(page);
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.set_len(page.bytes as u64)?;
        zero(&file, page.bytes)?;
        Ok(file)
    }

    /// Write zeroes over a page's file without creating it.
    ///
    /// **For a page this process does not hold.** After a handover the section
    /// belongs to the writer and the file is watched from Linux — see
    /// [`crate::handoff`] — so when the writer goes, the zeroing has to happen
    /// from out here. A page that is not there needs no blanking and is not a
    /// failure.
    pub fn blank(&self, page: &Page) -> io::Result<()> {
        let file = match std::fs::OpenOptions::new()
            .write(true)
            .open(self.path(page))
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        zero(&file, page.bytes)
    }

    /// Take a page's file away.
    ///
    /// A page nobody owns is worse than no page: it holds the last bytes that
    /// were written into it and goes on reading as a live feed. Missing is not
    /// a failure — two copies shutting down at once is a race nobody needs to
    /// hear about.
    pub fn remove(&self, page: &Page) -> io::Result<()> {
        match std::fs::remove_file(self.path(page)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Take all of them away, reporting how many could not be.
    ///
    /// Deliberately not `?` on the first failure: one page owned by another
    /// user would otherwise leave the rest of them behind, which is the state
    /// this is trying to avoid.
    pub fn remove_all(&self, pages: &[Page]) -> Vec<(String, io::Error)> {
        pages
            .iter()
            .filter_map(|page| {
                self.remove(page)
                    .err()
                    .map(|error| (page.name.clone(), error))
            })
            .collect()
    }
}

/// Write `bytes` zeroes over a file, from the start.
///
/// In chunks rather than one allocation the size of the page: a 64 MiB page
/// would otherwise ask for 64 MiB of heap to write nothing into.
fn zero(file: &File, bytes: usize) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    // 64 KiB. The exact figure is not load-bearing — any chunk size writes the
    // same bytes, which is why a mutation test flags this line and is right to
    // be ignored about it. What matters is that it is bounded: a 64 MiB page
    // must not ask for 64 MiB of heap to write nothing into.
    const CHUNK: usize = 64 * 1024;
    let blank = [0u8; CHUNK];
    let mut file = file;
    file.seek(SeekFrom::Start(0))?;

    let mut left = bytes;
    while left > 0 {
        let now = left.min(CHUNK);
        file.write_all(&blank[..now])?;
        left -= now;
    }
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wineshm-{name}"));
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

    #[test]
    fn a_page_lands_in_the_directory_under_its_own_name() {
        let store = Store::at("/dev/shm");
        assert_eq!(
            store.path(&page("acpmf_physics", 2048)),
            Path::new("/dev/shm/acpmf_physics")
        );
    }

    #[test]
    fn preparing_a_page_creates_it_at_the_asked_for_size() {
        let dir = scratch("create");
        let store = Store::at(&dir);
        let spec = page("thing", 2048);

        store.prepare(&spec).expect("prepare");
        let made = std::fs::metadata(store.path(&spec)).expect("metadata");
        assert_eq!(made.len(), 2048);
    }

    /// The fault that cost a release: a file left from an older build keeps
    /// its old length, and the length is what a reader checks.
    #[test]
    fn a_page_that_already_exists_is_resized_to_what_is_asked_for_now() {
        let dir = scratch("resize");
        let store = Store::at(&dir);

        store.prepare(&page("thing", 712)).expect("first");
        assert_eq!(std::fs::metadata(dir.join("thing")).expect("m").len(), 712);

        store.prepare(&page("thing", 2484)).expect("second");
        assert_eq!(std::fs::metadata(dir.join("thing")).expect("m").len(), 2484);

        // And back down again, because a build that maps less has the same
        // problem in the other direction.
        store.prepare(&page("thing", 128)).expect("third");
        assert_eq!(std::fs::metadata(dir.join("thing")).expect("m").len(), 128);
    }

    /// The worse half: same size, old bytes, reading as a live session.
    #[test]
    fn a_page_of_the_same_size_is_still_wiped() {
        let dir = scratch("wipe");
        let store = Store::at(&dir);
        let spec = page("thing", 64);

        {
            let mut file = std::fs::File::create(dir.join("thing")).expect("create");
            file.write_all(&[0xAB; 64]).expect("fill");
        }

        store.prepare(&spec).expect("prepare");

        let mut found = Vec::new();
        std::fs::File::open(dir.join("thing"))
            .expect("open")
            .read_to_end(&mut found)
            .expect("read");
        assert_eq!(found, vec![0u8; 64], "last session's bytes survived");
    }

    #[test]
    fn a_page_bigger_than_one_chunk_is_zeroed_all_the_way_through() {
        let dir = scratch("bigzero");
        let store = Store::at(&dir);
        let bytes = 64 * 1024 + 7;
        let spec = page("big", bytes);

        {
            let mut file = std::fs::File::create(dir.join("big")).expect("create");
            file.write_all(&vec![0xCD; bytes]).expect("fill");
        }
        store.prepare(&spec).expect("prepare");

        let found = std::fs::read(dir.join("big")).expect("read");
        assert_eq!(found.len(), bytes);
        assert!(
            found.iter().all(|byte| *byte == 0),
            "a byte past the first chunk survived"
        );
    }

    #[test]
    fn removing_a_page_takes_the_file_away() {
        let dir = scratch("remove");
        let store = Store::at(&dir);
        let spec = page("thing", 32);

        store.prepare(&spec).expect("prepare");
        assert!(store.path(&spec).exists());
        store.remove(&spec).expect("remove");
        assert!(!store.path(&spec).exists());
    }

    /// Two copies shutting down together is a race, not a fault.
    #[test]
    fn removing_a_page_that_is_already_gone_is_not_a_failure() {
        let dir = scratch("remove-twice");
        let store = Store::at(&dir);
        let spec = page("thing", 32);

        store.prepare(&spec).expect("prepare");
        store.remove(&spec).expect("first");
        store.remove(&spec).expect("second");
    }

    #[test]
    fn removing_all_of_them_reports_the_ones_it_could_not() {
        let dir = scratch("remove-all");
        let store = Store::at(&dir);
        let pages = vec![page("one", 16), page("two", 16), page("three", 16)];
        for spec in &pages {
            store.prepare(spec).expect("prepare");
        }

        assert!(store.remove_all(&pages).is_empty());
        for spec in &pages {
            assert!(!store.path(spec).exists(), "{} survived", spec.name);
        }
    }

    /// An error that is not "already gone" must not be swallowed.
    #[test]
    fn a_page_that_cannot_be_removed_is_reported() {
        let dir = scratch("remove-error");
        let store = Store::at(&dir);
        let spec = page("a-directory", 16);
        // A directory where the file should be: `remove_file` refuses it, and
        // that is a real failure rather than the "not found" that is not.
        std::fs::create_dir_all(store.path(&spec)).expect("dir");

        let why = store
            .remove(&spec)
            .expect_err("a directory is not removable");
        assert_ne!(why.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(store.remove_all(&[spec]).len(), 1);
    }

    /// The state after a handover: the file exists, something else wrote to
    /// it, and this process has to leave nothing readable in it.
    #[test]
    fn blanking_a_page_this_process_does_not_hold_empties_it() {
        let dir = scratch("blank-outside");
        let store = Store::at(&dir);
        let page = Page {
            name: "held-elsewhere".into(),
            bytes: 64,
        };
        {
            let file = store.prepare(&page).expect("prepared");
            drop(file);
        }
        std::fs::write(store.path(&page), vec![0xAB; 64]).expect("written by somebody else");

        store.blank(&page).expect("blanked");
        let after = std::fs::read(store.path(&page)).expect("read back");
        assert!(after.iter().all(|byte| *byte == 0), "{after:?}");
        assert_eq!(after.len(), 64, "the size is unchanged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A page already gone is the ordinary end of a session, not a fault.
    #[test]
    fn blanking_a_page_that_is_not_there_is_not_an_error() {
        let dir = scratch("blank-missing");
        let store = Store::at(&dir);
        let page = Page {
            name: "never-existed".into(),
            bytes: 16,
        };
        assert!(store.blank(&page).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `zero` is the whole of the defence against a stale page, so it is
    /// asserted on its own rather than only through `prepare`.
    #[test]
    fn zeroing_writes_zeroes_over_everything_that_was_there() {
        let dir = scratch("zero-direct");
        let path = dir.join("thing");
        std::fs::write(&path, vec![0xFF; 300]).expect("write");

        let file = File::options()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open");
        zero(&file, 300).expect("zero");
        drop(file);

        assert_eq!(std::fs::read(&path).expect("read"), vec![0u8; 300]);
    }

    /// A store pointed at nothing says so rather than failing later.
    #[test]
    fn a_directory_that_is_not_there_is_not_usable() {
        assert!(!Store::at("/definitely/not/here").is_usable());
        assert!(Store::at(scratch("usable")).is_usable());
    }

    #[test]
    fn preparing_into_a_directory_that_is_not_there_is_an_error_not_a_panic() {
        let store = Store::at("/definitely/not/here");
        assert!(store.prepare(&page("thing", 16)).is_err());
    }
}
