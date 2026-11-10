//! Memory objects and their mappings (ADR-0005 P3, P7 and P11 as C6, C7 and C8 decide; ADR-0011
//! N14 step 3, task S3-5b). The rules are [`crate::mapping`]'s, tested on the host; this module
//! applies them to the framework's address spaces (`origin_frame::userspace`).
//!
//! - **An object** is its frames, zeroed and committed when it is made, and the charge they cost
//!   its creator's account. A mapping holds its object and each mapped page its frame
//!   (`SharedFrame`), so frames go with their last mapping and handle, and the charge with the
//!   object: mapping an object and closing its handle does not escape the quota.
//! - **Every call names the process whose space changes** and needs `write` on that handle, the
//!   caller's own included (C6). The address space's operations run with no lock of this module
//!   held, since they may wait for a shootdown (`UserSpace::unmap`): a mapping is reserved, busy,
//!   in the process's records first, and changed while it is busy.
//! - **Unmap and protect are priced before they run** against the caller's budget, and refused
//!   with `should_wait`, changing nothing, if it cannot pay (T15, T16); what each cost
//!   (`Change`) is charged afterwards.
//! - **A map tells its caller the address before the mapping can be used by any other call.**
//!   If that fails, the mapping is taken back whatever the budget holds, and its cost charged
//!   afterwards: a mapping nobody was told of does not stay.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_abi::{Error, Handle};
use origin_frame::objref::ObjectRef;
use origin_frame::userspace::{Change, ChangeError, SharedFrame, UserSpace};

use crate::cpu::without_interrupts;
use crate::handles::Need;
use crate::ipc::Object;
use crate::mapping::{self, Allow, Asked, PAGE, Place, Request};
use crate::paging::{Access, MapError};
use crate::process::{Account, Process};

/// Draws of a random place before a map gives up on finding a free one.
const PLACEMENT_TRIES: u32 = 8;

/// A memory object (C7): its frames, committed at creation, and their charge.
pub(crate) struct MemoryObject {
    frames: Vec<SharedFrame>,
    _charge: Committed,
}

/// Bytes of committed memory charged to an account, given back when this is dropped.
struct Committed {
    account: ObjectRef<Account>,
    bytes: usize,
}

impl Drop for Committed {
    fn drop(&mut self) {
        self.account.refund_committed(self.bytes);
    }
}

impl MemoryObject {
    /// An object of `length` bytes, its frames zeroed and committed now and charged to
    /// `account` (C7). `InvalidArgs` unless `length` is a non-zero multiple of a page, `Limit`
    /// past the account's quota, `NoMemory` without frames; on any failure the charge goes back.
    pub(crate) fn create(account: &ObjectRef<Account>, length: u64) -> Result<Self, Error> {
        let pages = mapping::whole_pages(length)?;
        if pages == 0 {
            return Err(Error::InvalidArgs);
        }
        let bytes = usize::try_from(length).map_err(|_| Error::Limit)?;
        account.charge_committed(bytes)?;
        let charge = Committed {
            account: account.clone(),
            bytes,
        };
        let count = usize::try_from(pages).map_err(|_| Error::Limit)?;
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(count)
            .map_err(|_| Error::NoMemory)?;
        for _ in 0..count {
            frames.push(SharedFrame::new().ok_or(Error::NoMemory)?);
        }
        Ok(Self {
            frames,
            _charge: charge,
        })
    }

    /// Its size in pages.
    pub(crate) fn pages(&self) -> u64 {
        self.frames.len() as u64
    }

    /// Its frame at page `page`, for a proof.
    pub(crate) fn frame(&self, page: u64) -> Option<&SharedFrame> {
        self.frames.get(usize::try_from(page).ok()?)
    }
}

/// The process `handle` names in `caller`'s table, with what `need` asks of it.
pub(crate) fn process_of(
    caller: &Process,
    handle: Handle,
    need: Need,
) -> Result<ObjectRef<Process>, Error> {
    without_interrupts(|| match caller.handles.lock().get(handle, need)? {
        (Object::Process(process), _) => Ok(process.clone()),
        _ => Err(Error::WrongType),
    })
}

/// The memory object `handle` names in `caller`'s table, with what `need` asks of it, and the
/// rights the handle has.
pub(crate) fn object_of(
    caller: &Process,
    handle: Handle,
    need: Need,
) -> Result<(ObjectRef<MemoryObject>, u32), Error> {
    without_interrupts(|| match caller.handles.lock().get(handle, need)? {
        (Object::Memory(object), rights) => Ok((object.clone(), rights)),
        _ => Err(Error::WrongType),
    })
}

/// The access a mapping that allows `allow` has in the page tables.
const fn access(allow: Allow) -> Access {
    match allow {
        Allow::Read => Access::Read,
        Allow::ReadWrite => Access::ReadWrite,
    }
}

/// `memory_map`: maps what `request` asks of `object`, held with the rights `held`, into
/// `target`'s address space, and returns where (C7, C8).
///
/// `report` tells the caller the address while the mapping is still reserved: until its caller
/// knows of it, no other call can change it or put another mapping in its place. If the report
/// fails the mapping goes again ([`withdraw`]), charged to `caller`, and the map fails with the
/// report's error. The report is a `dyn`, so the kernel holds this path once, whoever calls it.
pub(crate) fn map(
    caller: &Process,
    target: &Process,
    object: &ObjectRef<MemoryObject>,
    held: u32,
    request: &Request,
    report: &dyn Fn(u64) -> Result<(), Error>,
) -> Result<u64, Error> {
    let asked = mapping::check(request, object.pages(), held)?;
    let space = target.memory().ok_or(Error::InvalidArgs)?;
    let base = reserve(target, &space, &asked, object)?;
    if let Err(e) = map_pages(&space, base, object, &asked) {
        let reserved = without_interrupts(|| target.mappings.lock().remove(base));
        drop(reserved);
        return Err(e);
    }
    if let Err(e) = report(base) {
        withdraw(caller, target, &space, base, asked.pages);
        return Err(e);
    }
    without_interrupts(|| target.mappings.lock().made(base));
    Ok(base)
}

/// Takes back the mapping reserved at `base` whose address its caller could not be told: a
/// mapping nobody knows of must not stay. Its pages go through a shootdown, since a thread of
/// `target` may already have touched them. The budget is not asked first, since a refusal
/// would leave the mapping; what the change cost is charged to `caller` afterwards, so its
/// next unmap or protect waits that much longer (T15, T16). A caller cannot make this happen
/// for nothing: the page its address goes to has to be unmapped or made read-only between the
/// call's check and its write, and that change is priced.
fn withdraw(caller: &Process, target: &Process, space: &UserSpace, base: u64, pages: u64) {
    match space.unmap(base, pages) {
        Ok(change) => settle(caller, 0, change),
        // As in `unmap`: the change is made and its frames are kept from reuse for good (T3).
        Err(ChangeError::Stalled { .. }) => {}
        // As in `unmap`, only after an earlier stall, without memory for the records: some
        // pages may still be mapped. The record stays, as a mapping its process can unmap; it
        // goes with the process at the latest.
        Err(_) => {
            without_interrupts(|| target.mappings.lock().made(base));
            return;
        }
    }
    let object = without_interrupts(|| target.mappings.lock().remove(base));
    drop(object);
}

/// Reserves where `asked` goes in `target`'s records: at its fixed address if no page of it is
/// mapped, or at a random free place. The record is busy until the pages are mapped.
fn reserve(
    target: &Process,
    space: &UserSpace,
    asked: &Asked,
    object: &ObjectRef<MemoryObject>,
) -> Result<u64, Error> {
    let take = |base: u64| -> Result<bool, Error> {
        let reserved = without_interrupts(|| {
            target
                .mappings
                .lock()
                .reserve(base, asked.pages, asked.allow, object.clone())
        });
        match reserved {
            Ok(()) => {}
            // Over another mapping, unless the process has ended meanwhile.
            Err((Error::InvalidArgs, _)) => {
                return without_interrupts(|| target.mappings.lock().room()).map(|()| false);
            }
            Err((e, _)) => return Err(e),
        }
        // Pages the records do not know, the program's own and its start record's, are in the
        // space alone: a mapping goes nowhere any page is mapped. No page can appear here
        // between this look and `map_pages`: those pages are all mapped before the process has
        // a thread (`Process::map_page` needs the only reference to the space), and every later
        // page is mapped under its mapping's reserved record, as this one's are.
        if (0..asked.pages).any(|k| space.translate(base + k * PAGE).is_some()) {
            let reserved = without_interrupts(|| target.mappings.lock().remove(base));
            drop(reserved);
            return Ok(false);
        }
        Ok(true)
    };
    match asked.place {
        // Never over a mapping (C7).
        Place::At(base) => take(base)?.then_some(base).ok_or(Error::InvalidArgs),
        Place::Anywhere => {
            // An ended process, then the mapping limit, before the placement and its entropy
            // (D5's order).
            without_interrupts(|| target.mappings.lock().room())?;
            for _ in 0..PLACEMENT_TRIES {
                let random = crate::entropy::hardware_u64().ok_or(Error::NotSupported)?;
                let base = mapping::place(random, asked.pages).ok_or(Error::InvalidArgs)?;
                if take(base)? {
                    return Ok(base);
                }
            }
            Err(Error::Limit)
        }
    }
}

/// Maps `asked`'s pages of `object` at `base`. On a failure the pages mapped so far go again,
/// through a shootdown, since a thread may already have touched them.
fn map_pages(
    space: &UserSpace,
    base: u64,
    object: &MemoryObject,
    asked: &Asked,
) -> Result<(), Error> {
    for k in 0..asked.pages {
        let frame = object.frame(asked.first + k).ok_or(Error::InvalidArgs)?;
        if let Err(e) = space.map_shared(base + k * PAGE, frame, access(asked.allow)) {
            let _ = space.unmap(base, k);
            return Err(match e {
                // The space's page tables are full (`MAX_TABLES`) or out of frames.
                ChangeError::Map(MapError::OutOfMemory) => Error::Limit,
                // Not reached in this version: the place is reserved and was found unmapped
                // (`reserve`), its addresses were checked (`mapping::check`), and no mapping
                // of an object is executable.
                _ => Error::InvalidArgs,
            });
        }
    }
    Ok(())
}

/// What a change cost, in the budget's units: its shootdowns and the interrupts they sent.
fn cost(change: Change) -> u64 {
    u64::from(change.shootdowns) + u64::from(change.interrupts)
}

/// Prices a change of `pages` pages of `space` against `caller`'s budget, before it runs:
/// `ShouldWait` if the budget cannot pay now (T15, T16). Returns the price, to settle with
/// [`settle`] once the change has run.
fn pay(caller: &Process, space: &UserSpace, pages: u64) -> Result<u64, Error> {
    let price = mapping::price(pages, space.processors_loaded(), space.change_unconfirmed());
    let now = crate::time::monotonic_nanos();
    let paid = without_interrupts(|| caller.changes.lock().try_spend(now, price));
    paid.then_some(price).ok_or(Error::ShouldWait)
}

/// Charges `caller` what a change cost beyond the price it paid.
fn settle(caller: &Process, paid: u64, change: Change) {
    let beyond = cost(change).saturating_sub(paid);
    if beyond != 0 {
        let now = crate::time::monotonic_nanos();
        without_interrupts(|| caller.changes.lock().charge(now, beyond));
    }
}

/// `memory_unmap`: removes `target`'s whole mapping that is exactly `length` bytes from `base`
/// (C7), charged to `caller`.
pub(crate) fn unmap(
    caller: &Process,
    target: &Process,
    base: u64,
    length: u64,
) -> Result<(), Error> {
    let pages = mapping::whole_pages(length)?;
    let space = target.memory().ok_or(Error::InvalidArgs)?;
    let allow = without_interrupts(|| target.mappings.lock().begin(base, pages))?;
    let paid = match pay(caller, &space, pages) {
        Ok(paid) => paid,
        Err(e) => {
            without_interrupts(|| target.mappings.lock().end(base, allow));
            return Err(e);
        }
    };
    let result = match space.unmap(base, pages) {
        Ok(change) => {
            settle(caller, paid, change);
            Ok(())
        }
        // The change is made and its frames are kept from reuse for good (T3): the mapping is
        // gone all the same.
        Err(ChangeError::Stalled { .. }) => Err(Error::TimedOut),
        Err(e) => {
            // Only after an earlier stall, without memory for the records: some pages may be
            // gone. The mapping stays, to be unmapped again.
            without_interrupts(|| target.mappings.lock().end(base, allow));
            return Err(match e {
                ChangeError::Map(MapError::OutOfMemory) => Error::NoMemory,
                _ => Error::InvalidArgs,
            });
        }
    };
    let object = without_interrupts(|| target.mappings.lock().remove(base));
    drop(object);
    result
}

/// `memory_protect`: reduces what `target`'s whole mapping that is exactly `length` bytes from
/// `base` allows to `access` bits (C7), charged to `caller`.
pub(crate) fn protect(
    caller: &Process,
    target: &Process,
    base: u64,
    length: u64,
    bits: u32,
) -> Result<(), Error> {
    let new = Allow::from_bits(bits)?;
    let pages = mapping::whole_pages(length)?;
    let space = target.memory().ok_or(Error::InvalidArgs)?;
    let old = without_interrupts(|| target.mappings.lock().begin(base, pages))?;
    let unchanged = |result: Result<(), Error>| {
        without_interrupts(|| target.mappings.lock().end(base, old));
        result
    };
    if !new.within(old) {
        return unchanged(Err(Error::AccessDenied));
    }
    // Already what was asked: no shootdown, nothing to pay.
    if new == old {
        return unchanged(Ok(()));
    }
    let paid = match pay(caller, &space, pages) {
        Ok(paid) => paid,
        Err(e) => return unchanged(Err(e)),
    };
    let result = match space.protect(base, pages, access(new)) {
        Ok(change) => {
            settle(caller, paid, change);
            Ok(())
        }
        Err(ChangeError::Stalled { .. }) => Err(Error::TimedOut),
        // Nothing changed: a shared frame would be writable through one mapping and executable
        // through another, which this version never maps, or the range was refused.
        Err(_) => return unchanged(Err(Error::InvalidArgs)),
    };
    without_interrupts(|| target.mappings.lock().end(base, new));
    result
}
