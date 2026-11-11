//! The launch path (ADR-0005 P6, amendments C1 and C4–C6; ADR-0011 N14 step 3, task S3-3): the
//! one way the kernel starts a process with a start record.
//!
//! A creator makes the child with [`Process::child_of`], so the child acts for the creator's
//! principal and for no other; maps its image; has [`place`] put the start record's pages at a
//! random address, read-only; then [`start`] gives the child its own process and first thread
//! and what the creator passes (BOOTSTRAP and DIRECTORY channel ends), writes the record from
//! what the child's table then holds, and starts the first thread with RDI holding the
//! record's address. Until the Loader Service exists (ONBF §8.3), only kernel code launches.
//!
//! A launch that fails after the child holds anything closes the child's table: the child's
//! handle to itself would otherwise keep a process that never ran alive for ever.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_abi::start::{self, Entry, role};
use origin_abi::{Error, Handle, rights};
use origin_frame::objref::ObjectRef;
use origin_frame::userspace::UserFrame;

use crate::cpu::{CpuIndex, without_interrupts};
use crate::handles::{Need, Typed};
use crate::ipc::{ChannelEnd, Object};
use crate::paging::{Access, PAGE};
use crate::process::{Process, ThreadObject};

/// Start records go at a random page in `[RECORD_LOW, RECORD_HIGH)`: the upper part of the user
/// half (ADR-0004 H1), clear of the low addresses programs are placed at.
pub(crate) const RECORD_LOW: u64 = 0x0000_1000_0000_0000;
/// One past the last address a start record may use.
pub(crate) const RECORD_HIGH: u64 = 0x0000_7000_0000_0000;
/// Draws of a random place before a launch gives up on finding a free one.
const PLACEMENT_TRIES: u32 = 8;

/// What a creator gives its child besides the child's own process and thread.
pub(crate) struct Given<'a> {
    /// The channel back to the creator (BOOTSTRAP).
    pub(crate) bootstrap: Option<ChannelEnd>,
    /// The channel to the service directory (DIRECTORY).
    pub(crate) directory: Option<ChannelEnd>,
    /// The arguments.
    pub(crate) args: &'a [&'a [u8]],
    /// The environment.
    pub(crate) env: &'a [&'a [u8]],
}

impl Given<'_> {
    /// The entries the record will have: the child's process and thread, and what it is given.
    fn handles(&self) -> usize {
        2 + usize::from(self.bootstrap.is_some()) + usize::from(self.directory.is_some())
    }
}

/// Where a child's start record will be, its pages mapped and still empty.
pub(crate) struct Placed {
    /// The record's address: what the first thread finds in RDI.
    pub(crate) record: u64,
    /// The record's size.
    pub(crate) size: usize,
    frames: Vec<UserFrame>,
}

/// A started child.
pub(crate) struct Launched {
    /// The child.
    pub(crate) process: ObjectRef<Process>,
    /// Its start record's size.
    pub(crate) size: usize,
}

/// A random page at which `pages` pages fit in the record region, from hardware entropy.
/// `None` when the processor gives no entropy: a start record never goes at a predictable
/// address.
fn random_place(pages: u64) -> Option<u64> {
    let slots = ((RECORD_HIGH - RECORD_LOW) / PAGE).checked_sub(pages)? + 1;
    let random = crate::entropy::hardware_u64()?;
    Some(RECORD_LOW + (random % slots) * PAGE)
}

/// Maps the pages of `child`'s start record, for what `given` holds, read-only and not
/// executable, at a random address no other mapping of the child touches (C4). The record is
/// written by [`start`]. `LIMIT` past 64 handles or 64 KiB, `NOT_SUPPORTED` without hardware
/// entropy, `NO_MEMORY` without frames.
pub(crate) fn place(child: &mut Process, given: &Given<'_>) -> Result<Placed, Error> {
    let size = start::size_of(given.handles(), given.args, given.env).map_err(|_| Error::Limit)?;
    let pages = size.div_ceil(PAGE as usize) as u64;
    for _ in 0..PLACEMENT_TRIES {
        let record = random_place(pages).ok_or(Error::NotSupported)?;
        if (0..pages).any(|k| child.is_mapped(record + k * PAGE)) {
            continue;
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(pages as usize)
            .map_err(|_| Error::NoMemory)?;
        for k in 0..pages {
            let frame = child
                .map_page(record + k * PAGE, Access::Read)
                .map_err(|_| Error::NoMemory)?;
            frames.push(frame);
        }
        return Ok(Placed {
            record,
            size,
            frames,
        });
    }
    Err(Error::Limit)
}

/// Starts `child`, its image mapped and its record placed for `given`, on processor `cpu` at
/// `entry` with the stack pointer `stack_top` (P6). It gets its own process (C6's
/// `rights::PROCESS`), its first thread (`rights::THREAD`) and the ends in `given` (a channel's
/// rights), then its start record, made from what its table holds, then its first thread,
/// with RDI holding the record's address and every other register zero. On a failure after
/// it holds anything its table is closed, so nothing it was given or made stays alive.
pub(crate) fn start(
    child: Process,
    placed: Placed,
    given: Given<'_>,
    cpu: CpuIndex,
    entry: u64,
    stack_top: u64,
) -> Result<Launched, Error> {
    let process = ObjectRef::try_new(child).map_err(|_| Error::NoMemory)?;
    let thread = ObjectRef::try_new(ThreadObject::new()).map_err(|_| Error::NoMemory)?;
    match hand_over(&process, &thread, &placed, given)
        .and_then(|()| started(&process, &thread, &placed, cpu, entry, stack_top))
    {
        Ok(()) => Ok(Launched {
            process,
            size: placed.size,
        }),
        Err(e) => {
            process.abandon();
            Err(e)
        }
    }
}

/// Grants the child its start handles and writes its start record.
fn hand_over(
    process: &ObjectRef<Process>,
    thread: &ObjectRef<ThreadObject>,
    placed: &Placed,
    given: Given<'_>,
) -> Result<(), Error> {
    let mut granted: Vec<(Handle, u32)> = Vec::new();
    granted
        .try_reserve_exact(given.handles())
        .map_err(|_| Error::NoMemory)?;
    let ends = [
        (given.bootstrap, role::BOOTSTRAP),
        (given.directory, role::DIRECTORY),
    ];
    let own = [
        (
            Object::Process(process.clone()),
            rights::PROCESS,
            role::SELF_PROCESS,
        ),
        (
            Object::Thread(thread.clone()),
            rights::THREAD,
            role::SELF_THREAD,
        ),
    ];
    let given_ends = ends
        .into_iter()
        .filter_map(|(end, role)| Some((Object::Channel(end?), rights::CHANNEL, role)));
    for (object, rights, role) in own.into_iter().chain(given_ends) {
        granted.push((process.grant(object, rights)?, role));
    }
    // Each entry says what the child's table holds for the handle, as a received handle's
    // record does (B3).
    let mut entries: Vec<Entry> = Vec::new();
    entries
        .try_reserve_exact(granted.len())
        .map_err(|_| Error::NoMemory)?;
    without_interrupts(|| {
        let table = process.handles.lock();
        for &(handle, role) in &granted {
            let (object, rights) = table.get(handle, Need::NOTHING)?;
            entries.push(Entry {
                handle,
                object_type: object.object_type(),
                rights,
                role,
            });
        }
        Ok::<(), Error>(())
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(placed.size)
        .map_err(|_| Error::NoMemory)?;
    bytes.resize(placed.size, 0);
    let size = start::encode(&entries, given.args, given.env, &mut bytes)
        .map_err(|_| Error::InvalidArgs)?;
    if size != placed.size {
        return Err(Error::InvalidArgs);
    }
    for (frame, chunk) in placed.frames.iter().zip(bytes.chunks(PAGE as usize)) {
        if !process.write_frame(*frame, 0, chunk) {
            return Err(Error::NoMemory);
        }
    }
    Ok(())
}

/// Starts the first thread, with RDI holding the record's address.
fn started(
    process: &ObjectRef<Process>,
    thread: &ObjectRef<ThreadObject>,
    placed: &Placed,
    cpu: CpuIndex,
    entry: u64,
    stack_top: u64,
) -> Result<(), Error> {
    Process::start_thread_as(
        process,
        cpu,
        entry,
        stack_top,
        placed.record,
        thread.clone(),
    )
    .map(|_| ())
    .map_err(|_| Error::Limit)
}
