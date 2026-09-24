//! Watching pages the writer is holding, for a program that has its own loop.
//!
//! **The other half of [`crate::handoff`], as a library rather than a
//! program.** Once the copy inside the prefix has left, something outside has
//! to keep the note's pulse beating and notice the writer going — and the
//! obvious candidate is usually not another process at all. A program that is
//! reading this telemetry is already running for the whole session, already
//! links this crate, and is already awake sixty times a second drawing.
//!
//! Handing it a daemon to run instead would put the same rule in two places,
//! and two copies of a rule are two rules that will disagree. So the rule
//! lives here, and both the `wineshm` binary and any program embedding it call
//! the same code.
//!
//! # It never blocks
//!
//! [`Supervisor::look`] does one read and returns. It is meant to be called
//! from whatever loop the host already has — a frame, a timer, a select — so
//! that nothing has to be threaded and nothing waits on the game.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use wineshm::{Page, Store, supervise::{Supervisor, Seen}};
//!
//! let pages = vec![Page { name: "acpmf_physics".into(), bytes: 2048 }];
//! let mut watching = Supervisor::start(
//!     Store::default(),
//!     pages,
//!     "acpmf_physics",
//!     wineshm::watchdog::BLANK_AFTER,
//! )?;
//!
//! loop {
//!     match watching.look() {
//!         Seen::Writing => {}          // the game is alive; draw
//!         Seen::Quiet => {}            // between frames, nothing to do
//!         Seen::WriterGone => break,   // the pages are already away
//!     }
//! #   break;
//! }
//! # Ok(())
//! # }
//! ```

use crate::page::Page;
use crate::store::Store;
use core::time::Duration;
use std::io;
use std::time::Instant;

/// What one look found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// The block moved. The game is there.
    Writing,
    /// It did not move, and not for long enough to mean anything.
    Quiet,
    /// It has been quiet long enough that the writer has gone. **The pages
    /// have already been blanked and taken away** — there is nothing for the
    /// caller to clean up, and nothing left to watch.
    WriterGone,
}

/// Keeps a handed-over set of pages honest.
#[derive(Debug)]
pub struct Supervisor {
    store: Store,
    pages: Vec<Page>,
    heartbeat: Page,
    reader: crate::reader::Reader,
    buffer: Vec<u8>,
    shadow: crate::Shadow,
    dog: crate::Watchdog,
    note: String,
    note_path: std::path::PathBuf,
    last_look: Instant,
    last_beat: Instant,
    done: bool,
}

impl Supervisor {
    /// Take over a set of pages the writer is now holding.
    ///
    /// The note is rewritten in this process's name, because the one there was
    /// written by the copy inside the prefix and that process no longer
    /// exists: a reader checking who to blame would be told a lie.
    pub fn start(
        store: Store,
        pages: Vec<Page>,
        heartbeat: &str,
        blank_after: Duration,
    ) -> io::Result<Self> {
        let beating = pages
            .iter()
            .find(|page| page.name == heartbeat)
            .cloned()
            .ok_or_else(|| {
                io::Error::other(format!("{heartbeat} is not one of the pages handed over"))
            })?;
        let reader = crate::reader::Reader::open(&store, &beating)?;

        let note = crate::Note {
            version: crate::VERSION.to_string(),
            format: crate::announce::FORMAT,
            pid: std::process::id(),
            pages: pages
                .iter()
                .map(|page| (page.clone(), crate::Mode::Owned))
                .collect(),
        }
        .render();
        let note_path = store.dir().join(crate::announce::FILE);
        std::fs::write(&note_path, &note)?;

        Ok(Self {
            buffer: vec![0u8; beating.bytes],
            shadow: crate::Shadow::new(beating.bytes),
            dog: crate::Watchdog::new(blank_after),
            heartbeat: beating,
            reader,
            store,
            pages,
            note,
            note_path,
            last_look: Instant::now(),
            last_beat: Instant::now(),
            done: false,
        })
    }

    /// Which block is being watched.
    pub fn heartbeat(&self) -> &Page {
        &self.heartbeat
    }

    /// Whether the writer has gone and the pages are already away.
    pub fn is_finished(&self) -> bool {
        self.done
    }

    /// One look. Never blocks.
    ///
    /// Safe to call as often as the host likes: the watchdog is told how long
    /// it has been since the last look, so looking twice as often does not
    /// halve the patience.
    pub fn look(&mut self) -> Seen {
        if self.done {
            return Seen::WriterGone;
        }

        let since = self.last_look.elapsed();
        self.last_look = Instant::now();

        let stirred =
            self.reader.read_into(&mut self.buffer).is_ok() && self.shadow.changed(&self.buffer);

        if crate::liveness::due_to_beat(self.last_beat.elapsed(), crate::liveness::BEAT) {
            let _ = std::fs::write(&self.note_path, &self.note);
            self.last_beat = Instant::now();
        }

        if self.dog.saw(stirred, since) == crate::watchdog::Verdict::Blank {
            self.put_it_all_back();
            return Seen::WriterGone;
        }
        if stirred { Seen::Writing } else { Seen::Quiet }
    }

    /// How long the watched block has been unchanged.
    pub fn quiet_for(&self) -> Duration {
        self.dog.quiet_for()
    }

    /// The note goes first: while it is there a reader takes it as a promise
    /// that the pages are too. Then the pages are zeroed before they are
    /// unlinked, so that a page that cannot be removed still holds nothing.
    fn put_it_all_back(&mut self) {
        let _ = std::fs::remove_file(&self.note_path);
        for page in &self.pages {
            let _ = self.store.blank(page);
        }
        let _ = self.store.remove_all(&self.pages);
        self.done = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wineshm-supervise-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn a_page(name: &str) -> Page {
        Page {
            name: name.to_string(),
            bytes: 64,
        }
    }

    fn published(dir: &std::path::Path, pages: &[Page]) -> Store {
        let store = Store::at(dir);
        for page in pages {
            store.prepare(page).expect("prepared");
        }
        store
    }

    #[test]
    fn a_heartbeat_that_was_not_handed_over_is_refused() {
        let dir = scratch("wrong-beat");
        let pages = vec![a_page("a")];
        let store = published(&dir, &pages);
        let why =
            Supervisor::start(store, pages, "b", Duration::from_secs(1)).expect_err("refused");
        assert!(format!("{why}").contains('b'), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The note the copy inside the prefix left names a process that has
    /// exited. A reader checking who is publishing has to be told the truth.
    #[test]
    fn starting_rewrites_the_note_in_this_processs_name() {
        let dir = scratch("note");
        let pages = vec![a_page("a"), a_page("b")];
        let store = published(&dir, &pages);
        std::fs::write(
            dir.join(crate::announce::FILE),
            "format=1\nversion=0\npid=999\n",
        )
        .expect("an older note");

        let watching =
            Supervisor::start(store, pages, "a", Duration::from_secs(1)).expect("started");
        assert!(!watching.is_finished());

        let note = crate::Note::parse(
            &std::fs::read_to_string(dir.join(crate::announce::FILE)).expect("read back"),
        )
        .expect("a note");
        assert_eq!(note.pid, std::process::id());
        assert_eq!(note.pages.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_block_being_written_reads_as_writing() {
        let dir = scratch("writing");
        let pages = vec![a_page("a")];
        let store = published(&dir, &pages);
        let mut watching =
            Supervisor::start(store, pages, "a", Duration::from_secs(60)).expect("started");

        std::fs::write(dir.join("a"), vec![7u8; 64]).expect("the writer");
        assert_eq!(watching.look(), Seen::Writing);
        // And an unchanged block afterwards is quiet, not gone.
        assert_eq!(watching.look(), Seen::Quiet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole point: when the writer goes, the pages go, and the caller is
    /// told once.
    #[test]
    fn silence_takes_the_pages_away_and_says_so_once() {
        let dir = scratch("gone");
        let pages = vec![a_page("a"), a_page("b")];
        let store = published(&dir, &pages);
        std::fs::write(dir.join("a"), vec![3u8; 64]).expect("something was there");
        std::fs::write(dir.join("b"), vec![4u8; 64]).expect("something was there");

        let mut watching = Supervisor::start(store, pages, "a", Duration::ZERO).expect("started");

        // The first look compares against zeroes, so a page that already has
        // something in it reads as a change — which is right, because taking
        // over happens while the game is writing. The look after it is the
        // first that can say anything about silence.
        assert_eq!(watching.look(), Seen::Writing);
        assert_eq!(watching.look(), Seen::WriterGone);

        assert!(!dir.join("a").exists(), "the pages were not taken away");
        assert!(
            !dir.join("b").exists(),
            "every page goes, not just the watched one"
        );
        assert!(
            !dir.join(crate::announce::FILE).exists(),
            "the note outlived the pages it promised"
        );

        // And it keeps saying so rather than starting again.
        assert!(watching.is_finished());
        assert_eq!(watching.look(), Seen::WriterGone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stated on purpose, because it looks like a bug until you see why: the
    /// shadow starts as zeroes, so the first look at a page that already holds
    /// something reports a change. Taking over happens while the game is
    /// writing, so that is the truth; and at worst it delays noticing an
    /// already-dead writer by a single look.
    #[test]
    fn the_first_look_at_a_page_with_data_in_it_reads_as_a_change() {
        let dir = scratch("first-look");
        let pages = vec![a_page("a")];
        let store = published(&dir, &pages);
        std::fs::write(dir.join("a"), vec![9u8; 64]).expect("data already there");

        let mut watching =
            Supervisor::start(store, pages, "a", Duration::from_secs(60)).expect("started");
        assert_eq!(watching.look(), Seen::Writing);
        assert_eq!(watching.look(), Seen::Quiet, "and only the first one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A host with a fast loop must not get less patience than one with a slow
    /// loop: the watchdog is told elapsed time, not a number of looks.
    #[test]
    fn looking_more_often_does_not_shorten_the_patience() {
        let dir = scratch("patience");
        let pages = vec![a_page("a")];
        let store = published(&dir, &pages);
        let mut watching =
            Supervisor::start(store, pages, "a", Duration::from_secs(30)).expect("started");

        for _ in 0..500 {
            assert_ne!(watching.look(), Seen::WriterGone);
        }
        assert!(dir.join("a").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
