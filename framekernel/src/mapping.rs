//! The rules of memory objects and their mappings that need no hardware (ADR-0005 P7, P11 as
//! C6, C7 and C8 decide; ADR-0011 N14 step 3, task S3-5b), tested on the host: what a map
//! request asks for, a process's records of its whole mappings, where a random placement lands,
//! and the budget that limits unmap and protect (`THREAT_MODEL_ADDRESS_SPACES.md` T15, T16). The
//! kernel's `memory_object` applies them to address spaces.
//!
//! - **A request** names what a mapping allows (read, and at most one of write and execute),
//!   the object's pages it maps, and where: at random, or at a fixed address. Write with execute,
//!   or no read, is `invalid_args`; execute is `access_denied`, since no anonymous object holds
//!   the right (C6, C7); a right the handle lacks is `access_denied`; a range outside the object
//!   or the user half, or not in whole pages, is `invalid_args`.
//! - **A process's mappings** are whole: unmap and protect act on exactly one (C7). A record is
//!   reserved, busy, before its pages are mapped, and is busy again while it changes, so the
//!   address space's own work runs with none of these records' locks held.
//! - **The budget** is a generic cell rate: each unit (one shootdown, or one processor it
//!   interrupts) costs an interval of credit, and at most `burst` units can be spent ahead of
//!   time. A change is priced before it runs and refused if the budget cannot pay.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_abi::{Error, map_access, map_flags};
use origin_frame::usercopy::{USER_END, USER_START};

/// Bytes in a page (the definition's `page_size`).
pub const PAGE: u64 = origin_abi::PAGE_SIZE;
/// The most pages one shootdown covers: the framework's round of a change
/// (`userspace::CHUNK`). Used only to price a change before it runs; what it actually cost is
/// charged afterwards.
pub const PAGES_PER_SHOOTDOWN: u64 = 128;
/// A random placement lands on a page in `[PLACE_LOW, PLACE_HIGH)`: the upper part of the user
/// half (ADR-0004 H1), clear of the low addresses programs are placed at. Start records are
/// placed in the same region (`launch`), and a placement steps over every mapping.
pub const PLACE_LOW: u64 = 0x0000_1000_0000_0000;
/// One past the last address a random placement uses.
pub const PLACE_HIGH: u64 = 0x0000_7000_0000_0000;

/// What a mapping allows. In this version nothing is executable (C7), so these are all there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Allow {
    /// Read only.
    Read,
    /// Read and write.
    ReadWrite,
}

impl Allow {
    /// What the definition's `map_access` bits ask for. Write with execute, no read, or a bit
    /// this version does not define is `InvalidArgs`; execute is `AccessDenied`.
    pub const fn from_bits(bits: u32) -> Result<Self, Error> {
        let write = bits & map_access::WRITE != 0;
        let execute = bits & map_access::EXECUTE != 0;
        if bits & !map_access::ALL != 0 || bits & map_access::READ == 0 || (write && execute) {
            return Err(Error::InvalidArgs);
        }
        if execute {
            return Err(Error::AccessDenied);
        }
        Ok(if write { Self::ReadWrite } else { Self::Read })
    }

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

/// `bytes` as whole pages, or `InvalidArgs` if it is not a multiple of a page.
pub const fn whole_pages(bytes: u64) -> Result<u64, Error> {
    if !bytes.is_multiple_of(PAGE) {
        return Err(Error::InvalidArgs);
    }
    Ok(bytes / PAGE)
}

/// The end of `pages` pages from `base`, if they are page-aligned and inside the user half.
pub fn user_range_end(base: u64, pages: u64) -> Option<u64> {
    if !base.is_multiple_of(PAGE) || base < USER_START {
        return None;
    }
    pages
        .checked_mul(PAGE)
        .and_then(|bytes| base.checked_add(bytes))
        .filter(|&end| end <= USER_END)
}

/// A map request's fields, as the generated decoder read them (it has refused bits the
/// definition does not declare).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// `map_access` bits.
    pub access: u32,
    /// `map_flags` bits.
    pub flags: u32,
    /// Where the mapping starts in the object, in bytes.
    pub offset: u64,
    /// How many bytes it maps.
    pub length: u64,
    /// With `FIXED`, where it goes.
    pub address: u64,
}

/// Where a mapping goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// At a random free address.
    Anywhere,
    /// At this address, all of whose pages must be free.
    At(u64),
}

/// A valid request: what the mapping allows, the object's pages it maps, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asked {
    /// What it allows.
    pub allow: Allow,
    /// The object's first page it maps.
    pub first: u64,
    /// How many pages.
    pub pages: u64,
    /// Where.
    pub place: Place,
}

/// Checks `request` against an object of `object_pages` pages held with the rights `held`, in
/// this order: what it allows, the handle's rights for that, the range in the object, then the
/// placement.
pub fn check(request: &Request, object_pages: u64, held: u32) -> Result<Asked, Error> {
    let allow = Allow::from_bits(request.access)?;
    if held & allow.needs() != allow.needs() {
        return Err(Error::AccessDenied);
    }
    let first = whole_pages(request.offset)?;
    let pages = whole_pages(request.length)?;
    if pages == 0
        || first
            .checked_add(pages)
            .is_none_or(|end| end > object_pages)
    {
        return Err(Error::InvalidArgs);
    }
    let place = match request.flags {
        0 if request.address == 0 => Place::Anywhere,
        map_flags::FIXED if user_range_end(request.address, pages).is_some() => {
            Place::At(request.address)
        }
        _ => return Err(Error::InvalidArgs),
    };
    Ok(Asked {
        allow,
        first,
        pages,
        place,
    })
}

/// Which of an object's frames `asked` maps: the places of its pages among them, in order.
/// `None` if they cannot be counted; `check` has refused a request past the object's end.
pub fn frames(asked: &Asked) -> Option<core::ops::Range<usize>> {
    let first = usize::try_from(asked.first).ok()?;
    let end = first.checked_add(usize::try_from(asked.pages).ok()?)?;
    Some(first..end)
}

/// Where a random draw places `pages` pages: a page of `[PLACE_LOW, PLACE_HIGH)` at which they
/// fit, or `None` if they cannot fit there at all.
pub fn place(random: u64, pages: u64) -> Option<u64> {
    let slots = ((PLACE_HIGH - PLACE_LOW) / PAGE).checked_sub(pages)? + 1;
    Some(PLACE_LOW + (random % slots) * PAGE)
}

/// Why an address space refused to map a run of pages. The framework's own errors are not built
/// on the host; `memory_object` names each of them in these words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A page of the run is mapped already.
    Mapped,
    /// The space has as many page tables as one space may have, and the run needs another.
    Tables,
    /// No free frame for a page table, or no kernel memory for the space's records.
    Memory,
    /// Anything else: addresses the space cannot map, or a frame that would be writable and
    /// executable. `check` and this version's mappings rule both out.
    Other,
}

impl Refused {
    /// The status of a map whose run was refused (ADR-0005 D4): `limit` at the bound on a
    /// space's page tables, `no_memory` for a want of memory. `None` if only the place is
    /// taken: a fixed address there is `invalid_args` (D5), and a random draw is made again.
    pub const fn status(self) -> Option<Error> {
        match self {
            Self::Mapped => None,
            Self::Tables => Some(Error::Limit),
            Self::Memory => Some(Error::NoMemory),
            Self::Other => Some(Error::InvalidArgs),
        }
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
    /// A full budget at `rate`. A rate of 0 per second refills once a second at most. A rate
    /// above a unit a nanosecond counts as a unit a nanosecond: its interval would be 0, every
    /// unit free and the burst no bound at all.
    pub const fn new(rate: Rate) -> Self {
        const SECOND: u64 = 1_000_000_000;
        let per_second = if rate.per_second == 0 {
            1
        } else if rate.per_second as u64 > SECOND {
            SECOND
        } else {
            rate.per_second as u64
        };
        let interval = SECOND / per_second;
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
    /// The process has ended: nothing more is taken.
    closed: bool,
}

impl<T> Mappings<T> {
    /// None yet, and room for `limit`.
    pub const fn new(limit: usize) -> Self {
        Self {
            records: Vec::new(),
            limit,
            closed: false,
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

    /// Whether one more mapping may be made: `InvalidArgs` once the process has ended, then
    /// `Limit` at the limit. In D5's order, what is asked comes before the quotas, and the
    /// quotas before the placement.
    pub fn room(&self) -> Result<(), Error> {
        if self.closed {
            Err(Error::InvalidArgs)
        } else if self.records.len() >= self.limit {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }

    /// Reserves the `pages` pages from `base` for a mapping of `object` being made, busy until
    /// [`made`](Self::made). Refused as [`room`](Self::room) says, then `InvalidArgs` over
    /// another mapping, and `NoMemory` without memory for the record; the object comes back
    /// with the refusal.
    pub fn reserve(
        &mut self,
        base: u64,
        pages: u64,
        allow: Allow,
        object: T,
    ) -> Result<(), (Error, T)> {
        if let Err(e) = self.room() {
            return Err((e, object));
        }
        if self.overlaps(base, pages) {
            return Err((Error::InvalidArgs, object));
        }
        if self.records.try_reserve(1).is_err() {
            return Err((Error::NoMemory, object));
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
        self.closed = true;
        core::mem::take(&mut self.records)
            .into_iter()
            .map(|r| r.object)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use origin_abi::rights;

    const RW: u32 = map_access::READ | map_access::WRITE;
    const ALL_RIGHTS: u32 = rights::MEMORY_OBJECT;

    fn request(access: u32, flags: u32, offset: u64, length: u64, address: u64) -> Request {
        Request {
            access,
            flags,
            offset,
            length,
            address,
        }
    }

    #[test]
    fn access_is_read_and_at_most_one_of_write_and_execute() {
        assert_eq!(Allow::from_bits(map_access::READ), Ok(Allow::Read));
        assert_eq!(Allow::from_bits(RW), Ok(Allow::ReadWrite));
        assert_eq!(
            Allow::from_bits(map_access::READ | map_access::EXECUTE),
            Err(Error::AccessDenied),
            "nothing is executable in this version"
        );
        for bad in [
            0,
            map_access::WRITE,
            map_access::EXECUTE,
            map_access::ALL,
            map_access::WRITE | map_access::EXECUTE,
            map_access::READ | 1 << 3,
        ] {
            assert_eq!(Allow::from_bits(bad), Err(Error::InvalidArgs), "{bad:#b}");
        }
    }

    #[test]
    fn protect_may_only_reduce() {
        assert!(Allow::Read.within(Allow::Read));
        assert!(Allow::Read.within(Allow::ReadWrite));
        assert!(Allow::ReadWrite.within(Allow::ReadWrite));
        assert!(!Allow::ReadWrite.within(Allow::Read));
    }

    #[test]
    fn a_valid_request_names_its_pages_and_its_place() {
        assert_eq!(
            check(&request(RW, 0, PAGE, 2 * PAGE, 0), 4, ALL_RIGHTS),
            Ok(Asked {
                allow: Allow::ReadWrite,
                first: 1,
                pages: 2,
                place: Place::Anywhere,
            })
        );
        let at = 0x0000_3000_0000_0000;
        assert_eq!(
            check(
                &request(map_access::READ, map_flags::FIXED, 0, 4 * PAGE, at),
                4,
                ALL_RIGHTS
            ),
            Ok(Asked {
                allow: Allow::Read,
                first: 0,
                pages: 4,
                place: Place::At(at),
            })
        );
    }

    #[test]
    fn each_malformed_request_is_refused_for_its_own_reason() {
        let at = 0x0000_3000_0000_0000;
        let cases = [
            // What it allows.
            (request(0, 0, 0, PAGE, 0), Error::InvalidArgs),
            (request(map_access::ALL, 0, 0, PAGE, 0), Error::InvalidArgs),
            (
                request(map_access::READ | map_access::EXECUTE, 0, 0, PAGE, 0),
                Error::AccessDenied,
            ),
            // The range in the object.
            (request(RW, 0, 0, 0, 0), Error::InvalidArgs),
            (request(RW, 0, 1, PAGE, 0), Error::InvalidArgs),
            (request(RW, 0, 0, PAGE + 1, 0), Error::InvalidArgs),
            (request(RW, 0, 0, 5 * PAGE, 0), Error::InvalidArgs),
            (request(RW, 0, 4 * PAGE, PAGE, 0), Error::InvalidArgs),
            (request(RW, 0, !(PAGE - 1), PAGE, 0), Error::InvalidArgs),
            (request(RW, 0, PAGE, !(PAGE - 1), 0), Error::InvalidArgs),
            // The placement.
            (request(RW, 0, 0, PAGE, at), Error::InvalidArgs),
            (
                request(RW, map_flags::FIXED, 0, PAGE, 0),
                Error::InvalidArgs,
            ),
            (
                request(RW, map_flags::FIXED, 0, PAGE, at + 1),
                Error::InvalidArgs,
            ),
            (
                request(RW, map_flags::FIXED, 0, PAGE, USER_END),
                Error::InvalidArgs,
            ),
            (
                request(RW, map_flags::FIXED, 0, 2 * PAGE, USER_END - PAGE),
                Error::InvalidArgs,
            ),
            (
                request(RW, map_flags::FIXED, 0, PAGE, 0xFFFF_8000_0000_0000),
                Error::InvalidArgs,
            ),
            (request(RW, 1 << 1, 0, PAGE, 0), Error::InvalidArgs),
        ];
        for (asked, why) in cases {
            assert_eq!(check(&asked, 4, ALL_RIGHTS), Err(why), "{asked:?}");
        }
    }

    #[test]
    fn a_mapping_needs_the_rights_it_gives() {
        let read = request(map_access::READ, 0, 0, PAGE, 0);
        let write = request(RW, 0, 0, PAGE, 0);
        assert!(check(&read, 1, rights::READ | rights::MAP).is_ok());
        assert_eq!(check(&read, 1, rights::MAP), Err(Error::AccessDenied));
        assert_eq!(
            check(&write, 1, rights::READ | rights::MAP),
            Err(Error::AccessDenied)
        );
        assert!(check(&write, 1, rights::READ | rights::WRITE | rights::MAP).is_ok());
    }

    #[test]
    fn the_last_user_page_can_be_mapped_but_nothing_past_it() {
        assert_eq!(user_range_end(USER_END - PAGE, 1), Some(USER_END));
        assert_eq!(user_range_end(USER_END - PAGE, 2), None);
        assert_eq!(
            user_range_end(0, 1),
            None,
            "the null page is not user memory"
        );
        assert_eq!(user_range_end(USER_START, 1), Some(USER_START + PAGE));
        assert_eq!(user_range_end(PAGE, u64::MAX), None);
    }

    #[test]
    fn a_request_maps_its_own_pages_of_the_object() {
        let later = check(&request(RW, 0, 3 * PAGE, 2 * PAGE, 0), 8, ALL_RIGHTS).expect("valid");
        assert_eq!(frames(&later), Some(3..5));
        let whole = check(&request(RW, 0, 0, 8 * PAGE, 0), 8, ALL_RIGHTS).expect("valid");
        assert_eq!(frames(&whole), Some(0..8));
        let last = check(&request(RW, 0, 7 * PAGE, PAGE, 0), 8, ALL_RIGHTS).expect("valid");
        assert_eq!(frames(&last), Some(7..8));
        let uncountable = Asked {
            allow: Allow::Read,
            first: u64::MAX,
            pages: 2,
            place: Place::Anywhere,
        };
        assert_eq!(frames(&uncountable), None);
    }

    #[test]
    fn a_refused_run_has_the_status_of_what_refused_it() {
        assert_eq!(
            Refused::Tables.status(),
            Some(Error::Limit),
            "a bound of the space"
        );
        assert_eq!(
            Refused::Memory.status(),
            Some(Error::NoMemory),
            "a want of memory"
        );
        assert_eq!(Refused::Other.status(), Some(Error::InvalidArgs));
        assert_eq!(
            Refused::Mapped.status(),
            None,
            "the place is taken: its map says what that means"
        );
    }

    #[test]
    fn a_random_place_fits_in_the_region() {
        for random in [0, 1, u64::MAX, 0x1234_5678_9ABC_DEF0] {
            for pages in [1, 2, 512] {
                let at = place(random, pages).expect("fits");
                assert!(at >= PLACE_LOW && at.is_multiple_of(PAGE));
                assert!(at + pages * PAGE <= PLACE_HIGH);
            }
        }
        let all = (PLACE_HIGH - PLACE_LOW) / PAGE;
        assert_eq!(place(u64::MAX, all), Some(PLACE_LOW));
        assert_eq!(place(0, all + 1), None);
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
    fn no_rate_makes_a_unit_free() {
        // At and above a unit a nanosecond. Above it the interval would be 0 without the bound.
        for per_second in [1_000_000_000, 1_000_000_001, u32::MAX] {
            let mut budget = Budget::new(Rate {
                burst: 2,
                per_second,
            });
            assert!(!budget.try_spend(7, 3), "{per_second}: above the burst");
            assert!(budget.try_spend(7, 2), "{per_second}");
            assert!(!budget.try_spend(7, 1), "{per_second}: the burst is spent");
            // A nanosecond later one unit is earned back, not two.
            assert!(!budget.try_spend(8, 2), "{per_second}");
            assert!(budget.try_spend(8, 1), "{per_second}");
            // What a change cost beyond its price still delays the next one.
            budget.charge(8, 5);
            assert!(!budget.try_spend(13, 1), "{per_second}: the charge is owed");
            assert!(budget.try_spend(14, 1), "{per_second}");
            // No burst allows nothing, at any rate.
            let mut none = Budget::new(Rate {
                burst: 0,
                per_second,
            });
            assert!(!none.try_spend(7, 1), "{per_second}: no burst");
        }
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
        // Once closed, everything comes back and nothing more is taken: the process has ended.
        assert_eq!(m.close(), [2]);
        assert!(m.is_empty());
        assert_eq!(
            m.reserve(a, 1, Allow::Read, 4),
            Err((Error::InvalidArgs, 4))
        );
    }

    #[test]
    fn room_says_why_not_in_d5s_order() {
        let mut m: Mappings<u32> = Mappings::new(1);
        assert_eq!(m.room(), Ok(()));
        assert_eq!(m.reserve(0x0000_2000_0000_0000, 1, Allow::Read, 1), Ok(()));
        assert_eq!(m.room(), Err(Error::Limit));
        assert_eq!(
            m.reserve(0x0000_3000_0000_0000, 1, Allow::Read, 2),
            Err((Error::Limit, 2)),
            "the quota before the placement"
        );
        let _ = m.close();
        // Ended, with room again: what is asked comes before the quota.
        assert_eq!(m.room(), Err(Error::InvalidArgs));
        let mut none: Mappings<u32> = Mappings::new(0);
        assert_eq!(none.room(), Err(Error::Limit));
        let _ = none.close();
        assert_eq!(none.room(), Err(Error::InvalidArgs));
    }
}
