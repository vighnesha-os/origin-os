//! The bound on one user address space's page-table frames (S3-5b's review, finding C; ADR-0005
//! Amendment 5, D4, as the owner's item 142 settles it). What refuses a table must be told
//! apart: the kernel reports a space at its bound as `limit`, and a want of memory as
//! `no_memory`. Host-tested here; `userspace` applies it to a space's tables.
//!
//! The same bound holds a space's two tables of page records ([`ready_records`]): a run of
//! pages that the space's tables could never map is refused before any room is made for its
//! records, so what a space holds depends on its own bound and not on the size of an object
//! another process made (task S4-3, a fix to S3-5b's `prepare_run`; ADR-0005 G5).
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

/// Why a run of pages could not be made ready to map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunRefusal {
    /// A page of the run is mapped already, at this address.
    AlreadyMapped(u64),
    /// The run's records would pass the bound on the pages a space can map.
    Limit,
    /// One of the two record tables could not grow.
    NoMemory,
}

/// Makes ready the records of a run of `count` pages: refuses it if `overlap` names a page of it
/// that is mapped already, then if the space's `records` would pass `bound` pages, and only then
/// makes room for `count` more in `records` and for every record there would then be in
/// `spare`. In that order: a run over a mapped page is refused as such whatever its length, and
/// a refused run leaves both tables as they were, their capacity included.
///
/// # Errors
///
/// [`RunRefusal::AlreadyMapped`] for `overlap`; [`RunRefusal::Limit`] past `bound`;
/// [`RunRefusal::NoMemory`] if a table cannot grow, or if the count overflows.
pub fn ready_records<T>(
    overlap: Option<u64>,
    records: &mut Vec<T>,
    spare: &mut Vec<T>,
    count: usize,
    bound: usize,
) -> Result<(), RunRefusal> {
    if let Some(virt) = overlap {
        return Err(RunRefusal::AlreadyMapped(virt));
    }
    let total = records
        .len()
        .checked_add(count)
        .ok_or(RunRefusal::NoMemory)?;
    if total > bound {
        return Err(RunRefusal::Limit);
    }
    if records.try_reserve(count).is_err() || spare.try_reserve(total).is_err() {
        return Err(RunRefusal::NoMemory);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pages a space with 32 tables can map, as `userspace::MAX_USER_PAGES` has it.
    const BOUND: usize = 29 * 512;

    /// A space's two record tables as a run finds them: `mapped` records, the room the earlier
    /// maps left, and a spare table with room for all of them.
    fn tables(mapped: usize) -> (Vec<u64>, Vec<u64>) {
        let mut records: Vec<u64> = (0..mapped as u64).collect();
        records.reserve(3);
        let spare = Vec::with_capacity(records.len() + 1);
        (records, spare)
    }

    #[test]
    fn a_run_past_the_bound_is_refused_and_adds_no_room_to_either_table() {
        for mapped in [0, 1, 7, BOUND - 1, BOUND] {
            let (mut records, mut spare) = tables(mapped);
            let before = (
                records.len(),
                records.capacity(),
                spare.len(),
                spare.capacity(),
            );
            // A large shared object: a million pages into a space that can map 14,848.
            for count in [BOUND - mapped + 1, 1_000_000, usize::MAX - mapped] {
                assert_eq!(
                    ready_records(None, &mut records, &mut spare, count, BOUND),
                    Err(RunRefusal::Limit),
                    "{mapped} mapped, a run of {count}"
                );
                assert_eq!(
                    (
                        records.len(),
                        records.capacity(),
                        spare.len(),
                        spare.capacity()
                    ),
                    before,
                    "no room added for a run of {count}"
                );
            }
        }
    }

    #[test]
    fn a_run_that_fits_gets_room_in_both_tables() {
        for (mapped, count) in [(0, 1), (0, BOUND), (5, 300), (BOUND - 1, 1)] {
            let (mut records, mut spare) = tables(mapped);
            assert_eq!(
                ready_records(None, &mut records, &mut spare, count, BOUND),
                Ok(())
            );
            assert!(records.capacity() >= mapped + count);
            assert!(spare.capacity() >= mapped + count);
            assert_eq!(records.len(), mapped);
        }
    }

    #[test]
    fn a_run_over_a_mapped_page_is_refused_as_such_first() {
        for count in [1, BOUND, BOUND + 1, 1_000_000] {
            let (mut records, mut spare) = tables(BOUND);
            let before = (records.capacity(), spare.capacity());
            assert_eq!(
                ready_records(Some(0x4000), &mut records, &mut spare, count, BOUND),
                Err(RunRefusal::AlreadyMapped(0x4000)),
                "a run of {count}"
            );
            assert_eq!((records.capacity(), spare.capacity()), before);
        }
    }

    #[test]
    fn a_count_that_overflows_is_refused_for_want_of_memory_with_nothing_added() {
        let (mut records, mut spare) = tables(4);
        let before = (records.capacity(), spare.capacity());
        assert_eq!(
            ready_records(None, &mut records, &mut spare, usize::MAX, usize::MAX),
            Err(RunRefusal::NoMemory)
        );
        assert_eq!((records.capacity(), spare.capacity()), before);
    }

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
