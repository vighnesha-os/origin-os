//! The rules of memory objects' mappings that need no hardware (ADR-0005 P7, P11 as C7 decides;
//! ADR-0011 N14 step 3, task S3-5b), tested on the host: a process's records of its whole
//! mappings, and the budget that limits unmap and protect (`THREAT_MODEL_ADDRESS_SPACES.md` T15,
//! T16). The kernel applies them to address spaces.
//!
//! - **A process's mappings** are whole: unmap and protect act on exactly one (C7). A record is
//!   reserved, busy, before its pages are mapped, and is busy again while it changes, so the
//!   address space's own work runs with none of these records' locks held.
//! - **The budget** is a generic cell rate: each unit (one shootdown, or one processor it
//!   interrupts) costs an interval of credit, and at most `burst` units can be spent ahead of
//!   time. A change is priced before it runs and refused if the budget cannot pay.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_abi::Error;

/// Bytes in a page.
pub const PAGE: u64 = crate::paging::PAGE;

/// The most pages one shootdown covers: the framework's round of a change
/// (`userspace::CHUNK`). Used only to price a change before it runs; what it actually cost is
/// charged afterwards.
pub const PAGES_PER_SHOOTDOWN: u64 = 128;

/// What a mapping allows. In this version nothing is executable (C7), so these are all there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allow {
    /// Read only.
    Read,
    /// Read and write.
    ReadWrite,
}

impl Allow {
    /// The rights a handle needs to map this.
    pub const fn needs(self) -> u32 {
        match self {
            Self::Read => origin_abi::rights::READ,
            Self::ReadWrite => origin_abi::rights::READ | origin_abi::rights::WRITE,
        }
    }

    /// Whether this allows nothing that `other` does not.
    pub const fn within(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Read, _) | (Self::ReadWrite, Self::ReadWrite)
        )
    }
}

/// What an unmap or protect of `pages` pages may cost at most: one unit per shootdown and per
/// processor each one may interrupt, with `loaded` processors holding the space, and one more
/// round first if an earlier change stalled (`unconfirmed`).
pub fn price(pages: u64, loaded: u32, unconfirmed: bool) -> u64 {
    let rounds = pages.div_ceil(PAGES_PER_SHOOTDOWN) + u64::from(unconfirmed);
    rounds.saturating_mul(1 + u64::from(loaded))
}

/// A rate its creator sets for a process: `burst` units at once, refilled at `per_second`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    /// Units that can be spent at once.
    pub burst: u32,
    /// Units the budget regains each second.
    pub per_second: u32,
}

/// A budget of shootdowns and interrupts (T15, T16), as a generic cell rate: each unit costs
/// `interval` nanoseconds, and the credit spent may run at most `tau` nanoseconds ahead of the
/// clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    interval: u64,
    tau: u64,
    /// The time at which everything spent so far will have been earned back.
    earned_at: u64,
}

impl Budget {
    /// A full budget at `rate`. A rate of 0 per second refills once a second at most.
    pub const fn new(rate: Rate) -> Self {
        let per_second = if rate.per_second == 0 {
            1
        } else {
            rate.per_second as u64
        };
        let interval = 1_000_000_000 / per_second;
        Self {
            interval,
            tau: (rate.burst as u64).saturating_mul(interval),
            earned_at: 0,
        }
    }

    /// Spends `units` at `now` (nanoseconds on the monotonic clock) if the budget allows them;
    /// otherwise spends nothing and says so.
    pub fn try_spend(&mut self, now: u64, units: u64) -> bool {
        let start = self.earned_at.max(now);
        let Some(after) = units
            .checked_mul(self.interval)
            .and_then(|cost| start.checked_add(cost))
        else {
            return false;
        };
        if after - now > self.tau {
            return false;
        }
        self.earned_at = after;
        true
    }

    /// Spends `units` at `now` whether the budget allows them or not: what a change cost
    /// beyond its price.
    pub fn charge(&mut self, now: u64, units: u64) {
        self.earned_at = self
            .earned_at
            .max(now)
            .saturating_add(units.saturating_mul(self.interval));
    }
}

/// One whole mapping of an object, `T`.
#[derive(Debug)]
struct Record<T> {
    base: u64,
    pages: u64,
    allow: Allow,
    /// Being mapped, unmapped or protected: no other change may touch it.
    busy: bool,
    object: T,
}

impl<T> Record<T> {
    fn end(&self) -> u64 {
        self.base + self.pages * PAGE
    }
}

/// A process's whole mappings (C7), at most `limit` of them; each holds its object `T`.
#[derive(Debug)]
pub struct Mappings<T> {
    records: Vec<Record<T>>,
    limit: usize,
}

impl<T> Mappings<T> {
    /// None yet, and room for `limit`.
    pub const fn new(limit: usize) -> Self {
        Self {
            records: Vec::new(),
            limit,
        }
    }

    /// How many there are, made or being made.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Whether any mapping, made or being made, has a page in the `pages` pages from `base`.
    pub fn overlaps(&self, base: u64, pages: u64) -> bool {
        let end = base.saturating_add(pages.saturating_mul(PAGE));
        self.records.iter().any(|r| r.base < end && base < r.end())
    }

    /// Reserves the `pages` pages from `base` for a mapping of `object` being made, busy until
    /// [`made`](Self::made). `Limit` past the limit (or without memory for the record),
    /// `InvalidArgs` over another mapping; the object comes back with the refusal.
    pub fn reserve(
        &mut self,
        base: u64,
        pages: u64,
        allow: Allow,
        object: T,
    ) -> Result<(), (Error, T)> {
        if self.records.len() >= self.limit || self.records.try_reserve(1).is_err() {
            return Err((Error::Limit, object));
        }
        if self.overlaps(base, pages) {
            return Err((Error::InvalidArgs, object));
        }
        self.records.push(Record {
            base,
            pages,
            allow,
            busy: true,
            object,
        });
        Ok(())
    }

    /// The mapping reserved at `base` is made.
    pub fn made(&mut self, base: u64) {
        if let Some(r) = self.records.iter_mut().find(|r| r.base == base) {
            r.busy = false;
        }
    }

    /// Takes the whole mapping that is exactly the `pages` pages from `base`, made and not
    /// changing, for a change: what it allows. `InvalidArgs` if there is none.
    pub fn begin(&mut self, base: u64, pages: u64) -> Result<Allow, Error> {
        let r = self
            .records
            .iter_mut()
            .find(|r| r.base == base && r.pages == pages && !r.busy)
            .ok_or(Error::InvalidArgs)?;
        r.busy = true;
        Ok(r.allow)
    }

    /// The change to the mapping at `base` is over, and it now allows `allow`.
    pub fn end(&mut self, base: u64, allow: Allow) {
        if let Some(r) = self.records.iter_mut().find(|r| r.base == base) {
            r.allow = allow;
            r.busy = false;
        }
    }

    /// Takes the mapping at `base` out of the records: its object, for the caller to drop
    /// with no lock held.
    pub fn remove(&mut self, base: u64) -> Option<T> {
        let at = self.records.iter().position(|r| r.base == base)?;
        Some(self.records.swap_remove(at).object)
    }

    /// What the mapping at `base` allows, and whether it is changing.
    pub fn get(&self, base: u64) -> Option<(u64, Allow, bool)> {
        self.records
            .iter()
            .find(|r| r.base == base)
            .map(|r| (r.pages, r.allow, r.busy))
    }

    /// Takes every record out and refuses any more: the process has ended. The objects come
    /// back to be dropped with no lock held.
    pub fn close(&mut self) -> Vec<T> {
        self.limit = 0;
        core::mem::take(&mut self.records)
            .into_iter()
            .map(|r| r.object)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_may_only_reduce() {
        assert!(Allow::Read.within(Allow::Read));
        assert!(Allow::Read.within(Allow::ReadWrite));
        assert!(Allow::ReadWrite.within(Allow::ReadWrite));
        assert!(!Allow::ReadWrite.within(Allow::Read));
    }

    #[test]
    fn a_change_is_priced_per_round_and_per_processor() {
        assert_eq!(price(1, 0, false), 1);
        assert_eq!(price(1, 1, false), 2);
        assert_eq!(price(128, 3, false), 4);
        assert_eq!(price(129, 3, false), 8);
        assert_eq!(price(1, 1, true), 4);
        assert_eq!(price(u64::MAX, u32::MAX, true), u64::MAX);
    }

    #[test]
    fn the_budget_spends_its_burst_then_waits_for_its_rate() {
        let second = 1_000_000_000;
        let mut budget = Budget::new(Rate {
            burst: 8,
            per_second: 8,
        });
        let now = 5 * second;
        for _ in 0..4 {
            assert!(budget.try_spend(now, 2));
        }
        // Refused, and nothing spent: still refused at once, allowed once a unit is earned.
        assert!(!budget.try_spend(now, 2));
        assert!(!budget.try_spend(now + second / 8, 2));
        assert!(budget.try_spend(now + second / 4, 2));
        // A price larger than the burst is never allowed.
        let mut fresh = Budget::new(Rate {
            burst: 8,
            per_second: 8,
        });
        assert!(!fresh.try_spend(now, 9));
        assert!(fresh.try_spend(now, 8));
    }

    #[test]
    fn a_charge_beyond_the_price_delays_the_next_spend() {
        let second = 1_000_000_000;
        let mut budget = Budget::new(Rate {
            burst: 4,
            per_second: 4,
        });
        assert!(budget.try_spend(second, 1));
        budget.charge(second, 3);
        assert!(!budget.try_spend(second, 1), "four units spent");
        assert!(budget.try_spend(second + second / 4, 1));
        // A huge charge saturates rather than wraps.
        budget.charge(second, u64::MAX);
        assert!(!budget.try_spend(u64::MAX - 1, 1));
    }

    #[test]
    fn mappings_are_whole_bounded_and_never_overlap() {
        let mut m: Mappings<u32> = Mappings::new(2);
        let a = 0x0000_2000_0000_0000;
        assert_eq!(m.reserve(a, 2, Allow::ReadWrite, 1), Ok(()));
        // Busy until made: nothing may change it.
        assert_eq!(m.begin(a, 2), Err(Error::InvalidArgs));
        m.made(a);
        // Overlaps, at either end and inside.
        for (base, pages) in [(a, 1), (a + PAGE, 1), (a - PAGE, 2), (a - PAGE, 4)] {
            assert_eq!(
                m.reserve(base, pages, Allow::Read, 9),
                Err((Error::InvalidArgs, 9)),
                "{base:#x} {pages}"
            );
        }
        // Touching is not overlapping.
        assert_eq!(m.reserve(a + 2 * PAGE, 1, Allow::Read, 2), Ok(()));
        assert_eq!(
            m.reserve(a - 4 * PAGE, 1, Allow::Read, 3),
            Err((Error::Limit, 3))
        );
        // Only exactly a whole mapping can be changed.
        assert_eq!(m.begin(a, 1), Err(Error::InvalidArgs));
        assert_eq!(m.begin(a + PAGE, 1), Err(Error::InvalidArgs));
        assert_eq!(m.begin(a, 2), Ok(Allow::ReadWrite));
        assert_eq!(m.begin(a, 2), Err(Error::InvalidArgs), "already changing");
        m.end(a, Allow::Read);
        assert_eq!(m.get(a), Some((2, Allow::Read, false)));
        assert_eq!(m.remove(a), Some(1));
        assert_eq!(m.remove(a), None);
        assert_eq!(m.len(), 1);
        // Once closed, everything comes back and nothing more is taken.
        assert_eq!(m.close(), [2]);
        assert!(m.is_empty());
        assert_eq!(m.reserve(a, 1, Allow::Read, 4), Err((Error::Limit, 4)));
    }
}
