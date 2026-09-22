//! Noticing that nothing happened, so that nothing is done about it.
//!
//! **The first optimisation, and the one that pays.** When a page has to be
//! copied rather than owned, the bridge this replaces copied it on every tick
//! whether or not a single byte had moved. Most ticks it had not: a racing
//! game's static block is written once a session, its graphics block stops
//! entirely in a menu, and a paused car writes nothing anywhere.
//!
//! Comparing first costs a `memcmp`, which reads memory and writes none, and
//! stops at the first byte that differs. Copying costs a `memcpy` *and* the
//! page fault that dirties the destination. Skipping the copy is most of the
//! work saved, and the counters here are what let the program say so out loud
//! rather than a readme claiming it.

/// The last bytes seen for one page, and what has been done about them.
#[derive(Debug, Clone)]
pub struct Shadow {
    last: Vec<u8>,
    looks: u64,
    copies: u64,
    bytes_copied: u64,
}

impl Shadow {
    /// A shadow for a page of this size, starting out as zeroes.
    ///
    /// Zeroes rather than "nothing seen yet" on purpose: a freshly prepared
    /// page *is* zeroes — see [`crate::store::Store::prepare`] — so a first
    /// look at a page nobody has written yet correctly finds no change and
    /// costs no copy.
    pub fn new(bytes: usize) -> Self {
        Self {
            last: vec![0; bytes],
            looks: 0,
            copies: 0,
            bytes_copied: 0,
        }
    }

    /// Look at what is there now. `true` when it differs from last time.
    ///
    /// A page whose length has changed under us counts as changed, and the
    /// shadow takes the new shape: that is a writer that restarted with a
    /// different build, and refusing to follow it would mean publishing the
    /// old size for ever.
    pub fn changed(&mut self, fresh: &[u8]) -> bool {
        self.looks += 1;
        if self.last.len() == fresh.len() && self.last == fresh {
            return false;
        }
        self.last.clear();
        self.last.extend_from_slice(fresh);
        self.copies += 1;
        self.bytes_copied += fresh.len() as u64;
        true
    }

    /// What was there at the last look.
    pub fn seen(&self) -> &[u8] {
        &self.last
    }

    /// How many times this page has been looked at.
    pub fn looks(&self) -> u64 {
        self.looks
    }

    /// How many of those looks found a change worth copying.
    pub fn copies(&self) -> u64 {
        self.copies
    }

    /// How many bytes have actually been written out.
    pub fn bytes_copied(&self) -> u64 {
        self.bytes_copied
    }

    /// The share of looks that turned into work, 0.0 to 1.0.
    ///
    /// A page nobody has looked at yet reports zero rather than dividing by
    /// one: no looks is no work, which is the honest answer and not an
    /// undefined one.
    pub fn work_share(&self) -> f64 {
        if self.looks == 0 {
            return 0.0;
        }
        self.copies as f64 / self.looks as f64
    }

    /// How many bytes a fixed-rate copier would have written over the same
    /// looks, for the line that compares the two.
    pub fn bytes_if_always_copied(&self) -> u64 {
        self.looks * self.last.len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_shadow_is_the_zeroes_a_fresh_page_holds() {
        let shadow = Shadow::new(8);
        assert_eq!(shadow.seen(), &[0u8; 8]);
        assert_eq!(shadow.looks(), 0);
        assert_eq!(shadow.copies(), 0);
    }

    /// The case the whole module exists for: a page nobody has written yet
    /// must not be copied on the first look.
    #[test]
    fn a_page_still_full_of_zeroes_is_not_a_change() {
        let mut shadow = Shadow::new(8);
        assert!(!shadow.changed(&[0u8; 8]));
        assert_eq!(shadow.copies(), 0);
        assert_eq!(shadow.looks(), 1);
    }

    #[test]
    fn a_byte_moving_is_a_change_and_is_taken() {
        let mut shadow = Shadow::new(4);
        assert!(shadow.changed(&[0, 0, 1, 0]));
        assert_eq!(shadow.seen(), &[0, 0, 1, 0]);
        assert_eq!(shadow.copies(), 1);
        assert_eq!(shadow.bytes_copied(), 4);
    }

    #[test]
    fn the_same_bytes_twice_is_one_copy() {
        let mut shadow = Shadow::new(4);
        assert!(shadow.changed(&[1, 2, 3, 4]));
        assert!(!shadow.changed(&[1, 2, 3, 4]));
        assert!(!shadow.changed(&[1, 2, 3, 4]));
        assert_eq!(shadow.copies(), 1);
        assert_eq!(shadow.looks(), 3);
    }

    #[test]
    fn a_change_in_the_last_byte_is_still_a_change() {
        let mut shadow = Shadow::new(4);
        shadow.changed(&[1, 2, 3, 4]);
        assert!(shadow.changed(&[1, 2, 3, 5]));
        assert_eq!(shadow.seen(), &[1, 2, 3, 5]);
    }

    #[test]
    fn a_change_in_the_first_byte_is_still_a_change() {
        let mut shadow = Shadow::new(4);
        shadow.changed(&[1, 2, 3, 4]);
        assert!(shadow.changed(&[9, 2, 3, 4]));
    }

    /// A writer that restarted with a different build changes the length.
    #[test]
    fn a_page_that_changes_size_is_followed() {
        let mut shadow = Shadow::new(4);
        assert!(shadow.changed(&[1, 2, 3, 4, 5, 6]));
        assert_eq!(shadow.seen(), &[1, 2, 3, 4, 5, 6]);
        assert!(!shadow.changed(&[1, 2, 3, 4, 5, 6]));

        // And shrinking, which is the same fault the other way round.
        assert!(shadow.changed(&[1, 2]));
        assert_eq!(shadow.seen(), &[1, 2]);
    }

    /// A prefix of the old bytes must not read as "no change".
    #[test]
    fn a_shorter_page_with_the_same_start_is_a_change() {
        let mut shadow = Shadow::new(0);
        shadow.changed(&[1, 2, 3, 4]);
        assert!(shadow.changed(&[1, 2, 3]));
    }

    #[test]
    fn an_empty_page_is_handled_rather_than_special_cased() {
        let mut shadow = Shadow::new(0);
        assert!(!shadow.changed(&[]));
        assert_eq!(shadow.bytes_copied(), 0);
    }

    #[test]
    fn the_work_share_is_copies_over_looks() {
        let mut shadow = Shadow::new(2);
        // one copy in four looks
        shadow.changed(&[1, 1]);
        shadow.changed(&[1, 1]);
        shadow.changed(&[1, 1]);
        shadow.changed(&[1, 1]);
        assert_eq!(shadow.looks(), 4);
        assert_eq!(shadow.copies(), 1);
        assert!((shadow.work_share() - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn a_shadow_nobody_has_looked_at_reports_no_work_rather_than_dividing_by_nothing() {
        let shadow = Shadow::new(4);
        assert_eq!(shadow.work_share(), 0.0);
        assert!(shadow.work_share().is_finite());
    }

    #[test]
    fn the_comparison_against_always_copying_counts_every_look() {
        let mut shadow = Shadow::new(10);
        for _ in 0..5 {
            shadow.changed(&[0u8; 10]);
        }
        assert_eq!(shadow.bytes_copied(), 0);
        assert_eq!(shadow.bytes_if_always_copied(), 50);
    }

    /// Alternating content copies every time — the worst case, and it must
    /// not be cheaper than the truth.
    #[test]
    fn a_page_that_never_settles_copies_every_look() {
        let mut shadow = Shadow::new(2);
        // From one rather than zero: a new shadow is zeroes, so `[0, 0]` is
        // correctly *not* a change, which the test above is about.
        for i in 1..=10u8 {
            assert!(shadow.changed(&[i, i]), "look {i} found no change");
        }
        assert_eq!(shadow.copies(), 10);
        assert!((shadow.work_share() - 1.0).abs() < f64::EPSILON);
    }
}
