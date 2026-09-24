//! Asking how big somebody else's block actually is.
//!
//! **The sizes are the hard part of using this program, and they are the part
//! it could not help with.** A page is a name and a number of bytes, and the
//! number is a fact about a program somebody else wrote. Get it wrong and
//! nothing says so: ask for less than the block holds and you read a prefix of
//! it, with the fields past your buffer simply absent; ask for more and the
//! mapping fails with a message about a view, which is true and unhelpful.
//!
//! Windows knows the answer. A section can be mapped in its entirety by asking
//! for zero bytes, and `VirtualQuery` on the resulting address reports how much
//! address space that took. So the bridge can be asked to look rather than the
//! person being asked to guess.
//!
//! # Why the answer is a range
//!
//! The report is in whole pages of address space. A 2048-byte section and a
//! 4096-byte one both occupy one 4 KiB page, and nothing distinguishes them
//! from outside. So the honest answer is an interval, and this module's job is
//! to state it as one rather than round it to a number that would look like a
//! measurement.

/// The granularity Windows reports mapped address space in.
///
/// Four kilobytes. Not the 64 KiB allocation granularity that governs where a
/// view may *start* — a view's length is rounded to the page size, and that is
/// what is being read back here.
pub const PAGE_GRANULARITY: usize = 4096;

/// What a look at a published section found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name that was asked about.
    pub name: String,
    /// How much address space the whole section took.
    pub region: usize,
}

impl Found {
    /// The range the real size must lie in, as `(more_than, at_most)`.
    pub fn size_range(&self) -> (usize, usize) {
        size_range(self.region, PAGE_GRANULARITY)
    }

    /// The range in words, for somebody deciding what to type.
    pub fn described(&self) -> String {
        let (over, upto) = self.size_range();
        if over == 0 {
            format!("at most {upto} bytes")
        } else {
            format!("more than {over} and at most {upto} bytes")
        }
    }
}

/// The interval a section's true size must lie in, given the address space it
/// took.
///
/// Rounding *up* is what the system did, so the true size is in
/// `(region - granularity, region]`. The lower bound is exclusive and the
/// upper inclusive, and a region of one page starts the range at zero rather
/// than at a negative number nobody can act on.
pub fn size_range(region: usize, granularity: usize) -> (usize, usize) {
    if granularity == 0 {
        return (region.saturating_sub(1), region);
    }
    (region.saturating_sub(granularity), region)
}

/// Whether a size somebody typed is consistent with what was measured.
///
/// **Consistent, not equal.** The measurement cannot distinguish 2048 from
/// 4096, so a size that falls inside the interval is as right as anything can
/// be shown to be from here. One outside it is wrong, and that is worth
/// saying.
pub fn fits(asked: usize, region: usize, granularity: usize) -> bool {
    let (over, upto) = size_range(region, granularity);
    asked > over && asked <= upto
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: usize = PAGE_GRANULARITY;

    #[test]
    fn one_page_of_address_space_means_anything_up_to_a_page() {
        assert_eq!(size_range(4096, G), (0, 4096));
    }

    #[test]
    fn four_pages_narrows_it_to_the_last_page() {
        assert_eq!(size_range(16384, G), (12288, 16384));
    }

    /// Assetto Corsa's blocks, which are the sizes this was written for: a
    /// 2048-byte block and a 15660-byte one are both inside what would be
    /// measured for them.
    #[test]
    fn the_sizes_this_was_written_for_are_consistent_with_their_regions() {
        assert!(fits(2048, 4096, G), "a 2048-byte block reads as one page");
        assert!(
            fits(15660, 16384, G),
            "a 15660-byte block reads as four pages"
        );
    }

    /// The failure this exists to catch: a size that is plausible, typed with
    /// confidence, and an order of magnitude out.
    #[test]
    fn a_size_far_from_the_measurement_does_not_fit() {
        assert!(!fits(2048, 65536, G), "2048 asked of a 64 KiB section");
        assert!(!fits(65536, 4096, G), "64 KiB asked of a one-page section");
        assert!(!fits(12288, 16384, G), "exactly the exclusive lower bound");
    }

    /// The upper bound is inclusive: a block that is exactly a whole number of
    /// pages is the commonest case there is.
    #[test]
    fn a_size_of_exactly_the_region_fits() {
        assert!(fits(4096, 4096, G));
        assert!(fits(16384, 16384, G));
    }

    #[test]
    fn a_region_of_nothing_admits_nothing() {
        assert_eq!(size_range(0, G), (0, 0));
        assert!(!fits(0, 0, G), "a size of zero is not a size");
        assert!(!fits(1, 0, G));
    }

    /// Nothing should divide by it, and nothing should panic on it.
    #[test]
    fn a_granularity_of_zero_does_not_panic() {
        assert_eq!(size_range(4096, 0), (4095, 4096));
    }

    #[test]
    fn what_it_says_names_both_ends_except_at_the_bottom() {
        let one = Found {
            name: "x".into(),
            region: 4096,
        };
        assert_eq!(one.described(), "at most 4096 bytes");

        let four = Found {
            name: "x".into(),
            region: 16384,
        };
        assert!(four.described().contains("12288"), "{}", four.described());
        assert!(four.described().contains("16384"), "{}", four.described());
    }
}
