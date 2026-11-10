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
//! - **A map's pages are entered all or none**, in one call to the address space, which refuses
//!   a run over any page it maps (`UserSpace::map_shared_run`). A refused map leaves no entry
//!   that was ever present, so it needs no shootdown and costs its caller's budget nothing.
//! - **Unmap and protect are priced before they run** against the caller's budget, and refused
//!   with `should_wait`, changing nothing, if it cannot pay (T15, T16); what each cost
//!   (`Change`) is charged afterwards. A mapping another call is making or changing at that
//!   moment is `should_wait` too, before anything is priced (the note on D5).
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
use crate::mapping::{self, Allow, Asked, Place, Refused, Request};
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

    /// The frames `asked` maps: its pages of this object, in order.
    fn run(&self, asked: &Asked) -> Option<&[SharedFrame]> {
        self.frames.get(mapping::frames(asked)?)
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

/// Why the address space refused a run, in the rules' words ([`Refused::status`] has the status
/// of each).
const fn refused(e: ChangeError) -> Refused {
    match e {
        ChangeError::Map(MapError::AlreadyMapped(_)) => Refused::Mapped,
        ChangeError::Limit => Refused::Tables,
        ChangeError::Map(MapError::OutOfMemory) => Refused::Memory,
        // Not reached in this version: the run's addresses were checked (`mapping::check`), no
        // mapping of an object is executable, and entering a run waits for no processor.
        _ => Refused::Other,
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
    let base = put(target, &space, &asked, object)?;
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
        // As in `unmap`: every page is out of the tables, and the frames a processor may still
        // reach are kept from reuse for good (T3).
        Err(ChangeError::Stalled { .. }) => {}
        // As in `unmap`, not reached in this version, and nothing changed: the record stays,
        // as a mapping its process can unmap; it goes with the process at the latest.
        Err(_) => {
            without_interrupts(|| target.mappings.lock().made(base));
            return;
        }
    }
    let object = without_interrupts(|| target.mappings.lock().remove(base));
    drop(object);
}

/// Puts `asked`'s pages of `object` into `target`'s space, at its fixed address or at a random
/// free place, and returns where. The mapping's record is reserved first and is still busy on
/// return. Its pages are then entered all or none, in one call to the address space
/// (`UserSpace::map_shared_run`).
///
/// The space refuses a run over any page it maps, from its own records. Those hold every user
/// page of the space: the program's own and its start record's (`UserSpace::map_page`), which
/// the mappings' records do not know, as well as every mapping's. So nothing looks at the page
/// tables first, and there is no time between a look and the map for another page to appear
/// in. A refused run was never entered: its record goes, and there is nothing to unmap and no
/// shootdown.
fn put(
    target: &Process,
    space: &UserSpace,
    asked: &Asked,
    object: &ObjectRef<MemoryObject>,
) -> Result<u64, Error> {
    let frames = object.run(asked).ok_or(Error::InvalidArgs)?;
    // Whether the run went in at `base`: `Ok(false)` if that place is taken.
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
        let Err(e) = space.map_shared_run(base, frames, access(asked.allow)) else {
            return Ok(true);
        };
        let reserved = without_interrupts(|| target.mappings.lock().remove(base));
        drop(reserved);
        // Over a page of the program's own or its start record's, the place is taken.
        refused(e).status().map_or(Ok(false), Err)
    };
    match asked.place {
        // Never over a mapping, or any other page of the space (C7).
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
/// (C7), charged to `caller`. A mapping another call is making or changing at this moment is
/// `ShouldWait`, before anything is priced: nothing is changed and nothing spent (the note on
/// D5).
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
        // A processor did not answer. The change is made all the same: every page of the
        // mapping is out of the tables, and the frames a processor may still reach are kept
        // from reuse for good (T3). The mapping is gone, as `timed_out` says (D5).
        Err(ChangeError::Stalled { .. }) => Err(Error::TimedOut),
        Err(e) => {
            // Not reached in this version: the space refuses only a range it cannot have,
            // before anything changes, and a mapping's range was checked when it was made.
            // Nothing changed, so the mapping stays.
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
/// `base` allows to `access` bits (C7), charged to `caller`. A busy mapping is `ShouldWait`, as
/// in [`unmap`], before `access` is compared with what the mapping allows: that is not settled
/// while another call changes it.
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
        // As in `unmap`: every page has its new permissions, and the mapping now allows `new`.
        Err(ChangeError::Stalled { .. }) => Err(Error::TimedOut),
        // Nothing changed: a shared frame would be writable through one mapping and executable
        // through another, which this version never maps, or the range was refused.
        Err(_) => return unchanged(Err(Error::InvalidArgs)),
    };
    without_interrupts(|| target.mappings.lock().end(base, new));
    result
}
