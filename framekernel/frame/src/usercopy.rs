//! User-pointer checks (ADR-0005 P4; ADR-0004 H1): the arithmetic and the page-by-page test
//! a system call applies before it touches user memory. Safe, tested on the host; the
//! kernel supplies the walk of the calling process's own page tables.
//!
//! A range is valid only inside the user half, `0x1000` to the page below the topmost
//! (ADR-0004 H1: page 0 and the top page stay unmapped), and only if every page of it is
//! present and user in the caller's tables, and writable when it will be written. A
//! zero-length range names no memory, so any pointer is accepted with it.
#![forbid(unsafe_code)]

use origin_abi::Error;
use origin_paging::{PAGE, Translation};

/// The lowest user address (page 0 is never mapped).
pub const USER_START: u64 = 0x1000;
/// One past the highest user address: the top page of the lower half stays unmapped, so a
/// return RIP can never be non-canonical.
pub const USER_END: u64 = 0x0000_7FFF_FFFF_F000;

/// Whether `addr` could name user memory (the range a user-copy fault may be recovered in).
pub const fn is_user_address(addr: u64) -> bool {
    addr >= USER_START && addr < USER_END
}

/// A checked user range: inside the user half, or empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserRange {
    start: u64,
    end: u64,
}

/// How a user range is going to be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserAccess {
    /// The kernel will read it.
    Read,
    /// The kernel will write it.
    Write,
}

impl UserRange {
    /// The range of `len` bytes at `start`, or [`Error::Fault`] if it wraps or leaves the
    /// user half. A zero-length range is always accepted and names no memory.
    pub fn new(start: u64, len: u64) -> Result<Self, Error> {
        if len == 0 {
            return Ok(Self { start: 0, end: 0 });
        }
        let end = start.checked_add(len).ok_or(Error::Fault)?;
        if start < USER_START || end > USER_END {
            return Err(Error::Fault);
        }
        Ok(Self { start, end })
    }

    /// Whether the range names no memory.
    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The first address.
    pub const fn start(&self) -> u64 {
        self.start
    }

    /// Length in bytes.
    pub const fn len(&self) -> u64 {
        self.end - self.start
    }

    /// The address of every page the range touches.
    pub fn pages(&self) -> impl Iterator<Item = u64> + use<> {
        let (first, end) = (self.start & !(PAGE - 1), self.end);
        (0..).map_while(move |k: u64| {
            let page = first.checked_add(k.checked_mul(PAGE)?)?;
            (page < end).then_some(page)
        })
    }
}

/// Whether every page of `range` is mapped for the kernel to do `access` on behalf of the
/// user: present, user, and writable when written. `translate` is the walk of the calling
/// process's own tables.
pub fn check_mapped(
    range: &UserRange,
    access: UserAccess,
    translate: impl Fn(u64) -> Option<Translation>,
) -> Result<(), Error> {
    for page in range.pages() {
        match translate(page) {
            Some(t) if t.user && (access == UserAccess::Read || t.writable) => {}
            _ => return Err(Error::Fault),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use std::collections::HashMap;

    fn leaf(user: bool, writable: bool) -> Option<Translation> {
        Some(Translation {
            phys: 0x1000,
            size: PAGE,
            writable,
            executable: false,
            user,
            global: false,
            uncached: false,
        })
    }

    #[test]
    fn the_bounds_are_the_address_space_layouts() {
        assert_eq!(
            USER_END, 0x0000_7FFF_FFFF_F000,
            "the top page stays unmapped"
        );
        assert!(UserRange::new(USER_END - 8, 8).is_ok());
        assert_eq!(UserRange::new(USER_END - 8, 9), Err(Error::Fault));
        assert_eq!(UserRange::new(USER_START - 1, 1), Err(Error::Fault));
        assert_eq!(UserRange::new(0, 8), Err(Error::Fault), "page 0");
        assert_eq!(
            UserRange::new(0xFFFF_8000_0000_0000, 8),
            Err(Error::Fault),
            "kernel half"
        );
        assert_eq!(
            UserRange::new(0x8000_0000_0000, 8),
            Err(Error::Fault),
            "non-canonical"
        );
        assert_eq!(
            UserRange::new(u64::MAX - 7, 16),
            Err(Error::Fault),
            "wraps past the top"
        );
    }

    #[test]
    fn a_zero_length_range_accepts_any_pointer_and_names_no_memory() {
        for p in [0, 1, u64::MAX, 0xFFFF_8000_0000_0000, 0x1234] {
            let r = UserRange::new(p, 0).unwrap();
            assert!(r.is_empty());
            assert_eq!(r.pages().count(), 0);
            assert_eq!(r.len(), 0);
        }
    }

    #[test]
    fn pages_cover_exactly_the_pages_touched() {
        let pages = |s, l| UserRange::new(s, l).unwrap().pages().collect::<Vec<_>>();
        assert_eq!(pages(0x2000, 1), [0x2000]);
        assert_eq!(pages(0x2FFF, 1), [0x2000]);
        assert_eq!(pages(0x2FFF, 2), [0x2000, 0x3000]);
        assert_eq!(pages(0x2000, 0x2000), [0x2000, 0x3000]);
        assert_eq!(
            pages(0x2001, 0x2000),
            [0x2000, 0x3000, 0x4000],
            "an unaligned range touches one page more"
        );
    }

    #[test]
    fn every_page_must_be_present_user_and_writable_when_written() {
        let range = UserRange::new(0x2800, 0x1000).unwrap();
        // Both pages good.
        let ok = |_| leaf(true, true);
        assert_eq!(check_mapped(&range, UserAccess::Write, ok), Ok(()));
        // The second page missing.
        let hole = |p| if p == 0x3000 { None } else { leaf(true, true) };
        assert_eq!(
            check_mapped(&range, UserAccess::Read, hole),
            Err(Error::Fault)
        );
        // A supervisor page in the middle.
        let sup = |p| leaf(p != 0x3000, true);
        assert_eq!(
            check_mapped(&range, UserAccess::Read, sup),
            Err(Error::Fault)
        );
        // Read-only: fine to read, not to write.
        let ro = |_| leaf(true, false);
        assert_eq!(check_mapped(&range, UserAccess::Read, ro), Ok(()));
        assert_eq!(
            check_mapped(&range, UserAccess::Write, ro),
            Err(Error::Fault)
        );
    }

    /// Random ranges over a random page map, against a byte-by-byte reference.
    #[test]
    fn random_ranges_match_a_byte_by_byte_reference() {
        const ADDRS: u64 = 16;
        for seed in 1..=cases(2000) as u64 {
            let mut rng = Rng::new(seed);
            let mut map: HashMap<u64, (bool, bool)> = HashMap::new();
            for p in 1..ADDRS {
                if rng.below(4) != 0 {
                    map.insert(p * PAGE, (rng.below(4) != 0, rng.below(2) != 0));
                }
            }
            let start = match rng.below(10) {
                0 => rng.edge_u64(),
                _ => rng.below((ADDRS * PAGE) as usize) as u64,
            };
            let len = match rng.below(10) {
                0 => rng.edge_u64(),
                _ => rng.below(3 * PAGE as usize) as u64,
            };
            let access = if rng.below(2) == 0 {
                UserAccess::Read
            } else {
                UserAccess::Write
            };
            let translate = |p: u64| {
                map.get(&p)
                    .and_then(|&(user, writable)| leaf(user, writable))
            };
            let got = UserRange::new(start, len).and_then(|r| check_mapped(&r, access, translate));
            let reference = if len == 0 {
                Ok(())
            } else if start.checked_add(len).is_none()
                || start < USER_START
                || start.saturating_add(len) > USER_END
            {
                Err(Error::Fault)
            } else {
                // Every byte of the range, on a page that allows the access.
                let mut result = Ok(());
                let mut a = start;
                while a < start + len {
                    let p = a & !(PAGE - 1);
                    let page_ok = match map.get(&p) {
                        Some(&(user, writable)) => user && (access == UserAccess::Read || writable),
                        None => false,
                    };
                    if !page_ok {
                        result = Err(Error::Fault);
                        break;
                    }
                    a = p + PAGE;
                }
                result
            };
            assert_eq!(
                got, reference,
                "seed {seed}: {start:#x}+{len:#x} {access:?}"
            );
        }
    }
}
