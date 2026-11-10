//! The bound on one user address space's page-table frames (S3-5b's review, finding C; ADR-0005
//! Amendment 5, D4, as the owner's item 142 settles it). What refuses a table must be told
//! apart: the kernel reports a space at its bound as `limit`, and a want of memory as
//! `no_memory`. Host-tested here; `userspace` applies it to a space's tables.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

/// Why a space could not have one more page-table frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The space has as many as one space may have.
    Limit,
    /// No free frame, or no memory to keep one more in the space's list.
    NoMemory,
}

/// Adds a frame from `allocate` to `frames`, a space's page-table frames, unless it holds
/// `bound` of them already, and returns the new one. The bound is checked first: a space at its
/// bound is refused as a limit, and no frame is taken for it, whether memory is short or not.
///
/// # Errors
///
/// [`Refusal::Limit`] at the bound. [`Refusal::NoMemory`] if the list cannot grow, or
/// `allocate` has no frame.
pub fn one_more<F>(
    frames: &mut Vec<F>,
    bound: usize,
    allocate: impl FnOnce() -> Option<F>,
) -> Result<&F, Refusal> {
    if frames.len() >= bound {
        return Err(Refusal::Limit);
    }
    frames.try_reserve(1).map_err(|_| Refusal::NoMemory)?;
    frames.push(allocate().ok_or(Refusal::NoMemory)?);
    frames.last().ok_or(Refusal::NoMemory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_space_at_its_bound_is_refused_as_a_limit_and_one_without_a_frame_for_want_of_memory() {
        let mut frames: Vec<u32> = Vec::new();
        assert_eq!(one_more(&mut frames, 2, || Some(1)), Ok(&1));
        assert_eq!(
            one_more(&mut frames, 2, || None),
            Err(Refusal::NoMemory),
            "no free frame"
        );
        assert_eq!(frames, [1], "nothing added");
        assert_eq!(one_more(&mut frames, 2, || Some(2)), Ok(&2));
        // At the bound: a limit, and no frame is taken for it, with memory or without.
        let mut asked = 0;
        let mut allocate = |frame: Option<u32>| {
            asked += 1;
            frame
        };
        assert_eq!(
            one_more(&mut frames, 2, || allocate(Some(3))),
            Err(Refusal::Limit)
        );
        assert_eq!(
            one_more(&mut frames, 2, || allocate(None)),
            Err(Refusal::Limit)
        );
        assert_eq!(asked, 0);
        assert_eq!(frames, [1, 2]);
        // A bound of none refuses the first.
        assert_eq!(
            one_more(&mut Vec::new(), 0, || Some(1)),
            Err(Refusal::Limit)
        );
    }
}
