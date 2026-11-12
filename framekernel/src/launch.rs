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
//!
//! A program the kernel carries in its image (ADR-0011 Amendment 2, L1; task S4-1) is launched
//! in two steps around those two. [`load_program`] makes the child with its system calls pinned
//! to the program's one read-execute segment (ADR-0005 A6), places the program at a random base,
//! maps each page with its final permissions and writes it, relocations applied, through its
//! frame, then maps a stack at another random place with an unmapped page on either side.
//! Nothing is ever writable and executable, and RELRO pages are read-only to the program from
//! the start. [`start_program`] then places the start record and starts the first thread at the
//! entry, as [`place`] and [`start`] do for any child.
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
use crate::process::{Limits, Process, ThreadObject};
use crate::program::{self, Carried, SegmentAccess};

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

/// A random page at which `pages` pages fit in the record region, from the kernel's
/// random-number generator (ADR-0016). `None` when there is no generator: a start record never
/// goes at a predictable address.
fn random_place(pages: u64) -> Option<u64> {
    let slots = ((RECORD_HIGH - RECORD_LOW) / PAGE).checked_sub(pages)? + 1;
    let random = origin_frame::random::next_u64().ok()?;
    Some(RECORD_LOW + (random % slots) * PAGE)
}

/// Maps the pages of `child`'s start record, for what `given` holds, read-only and not
/// executable, at a random address no other mapping of the child touches (C4). The record is
/// written by [`start`]. `LIMIT` past 64 handles or 64 KiB, `NOT_SUPPORTED` without a
/// random-number generator, `NO_MEMORY` without frames.
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

/// What a carried program is launched with, written beside its image (ADR-0005 G6).
pub(crate) struct ProgramLimits {
    /// Its quotas: each kind given deliberately, and the least it needs.
    pub(crate) limits: Limits,
    /// Pages of its first thread's stack.
    pub(crate) stack_pages: u64,
}

/// A carried program in a new process, mapped and relocated, its first thread not started.
pub(crate) struct LoadedProgram {
    /// The child, which nothing else holds yet.
    pub(crate) process: Process,
    /// Where the program starts: each offset in its description is from here.
    pub(crate) base: u64,
    /// Its entry point's address.
    pub(crate) entry: u64,
    /// The lowest page of its stack.
    pub(crate) stack: u64,
    /// One past the top of its stack: the first thread's RSP.
    pub(crate) stack_top: u64,
}

/// A carried program, started.
pub(crate) struct LaunchedProgram {
    /// The child.
    pub(crate) process: ObjectRef<Process>,
    /// Its start record's address.
    pub(crate) record: u64,
}

/// A random value from the kernel's generator (ADR-0016), or `NOT_SUPPORTED` without one: a
/// program never goes at a predictable place.
fn random() -> Result<u64, Error> {
    origin_frame::random::next_u64().map_err(|_| Error::NotSupported)
}

/// Makes `creator`'s child for `program`, under `run`'s limits, with its system calls accepted
/// from its read-execute segment alone (ADR-0005 A6), and maps the program at a random base and
/// a stack at another random place. Every page is mapped once, with its final permissions, and
/// written through its frame: read-execute for code, read-only for read-only data and for RELRO
/// once its relocations are in, read-write and never executable for data. `INVALID_ARGS` for a
/// description that breaks [`program::check`]'s rules, `NOT_SUPPORTED` without a random-number
/// generator, `NO_MEMORY` without frames, `LIMIT` if no free place is found. On an error the
/// child, and every frame it was given, is dropped.
pub(crate) fn load_program(
    creator: &Process,
    program: &Carried,
    run: &ProgramLimits,
) -> Result<LoadedProgram, Error> {
    program::check(program).map_err(|_| Error::InvalidArgs)?;
    let base = program::random_base(program.span / PAGE, random()?).ok_or(Error::Limit)?;
    let code = program::code_range(program, base).ok_or(Error::InvalidArgs)?;
    let mut child = Process::child_of(creator, run.limits, code).ok_or(Error::NoMemory)?;
    map_image(&mut child, program, base)?;
    let stack = place_stack(&mut child, run.stack_pages)?;
    Ok(LoadedProgram {
        process: child,
        base,
        entry: base + program.entry,
        stack,
        stack_top: stack + run.stack_pages * PAGE,
    })
}

/// Maps every page of `program` at `base` into `child`, relocations applied.
fn map_image(child: &mut Process, program: &Carried, base: u64) -> Result<(), Error> {
    let mut page = Vec::new();
    page.try_reserve_exact(PAGE as usize)
        .map_err(|_| Error::NoMemory)?;
    page.resize(PAGE as usize, 0);
    for (index, segment) in program.segments.iter().enumerate() {
        let access = match segment.access {
            SegmentAccess::Read | SegmentAccess::ReadAfterRelocation => Access::Read,
            SegmentAccess::ReadExecute => Access::ReadExecute,
            SegmentAccess::ReadWrite => Access::ReadWrite,
        };
        for k in 0..segment.size / PAGE {
            program::fill_page(program, index, k, base, &mut page)
                .map_err(|_| Error::InvalidArgs)?;
            let frame = child
                .map_page(base + segment.rva + k * PAGE, access)
                .map_err(|_| Error::NoMemory)?;
            // A fresh frame is zero: only a page with bytes is written.
            if page.iter().any(|&b| b != 0) && !child.write_frame(frame, 0, &page) {
                return Err(Error::NoMemory);
            }
        }
    }
    Ok(())
}

/// Maps `pages` pages of stack, read-write and never executable, at a random place in the
/// programs' region with an unmapped page below and above it that nothing else of `child` uses.
/// Returns its lowest page.
fn place_stack(child: &mut Process, pages: u64) -> Result<u64, Error> {
    if pages == 0 {
        return Err(Error::InvalidArgs);
    }
    for _ in 0..PLACEMENT_TRIES {
        let guarded = program::random_base(pages + 2, random()?).ok_or(Error::Limit)?;
        if (0..pages + 2).any(|k| child.is_mapped(guarded + k * PAGE)) {
            continue;
        }
        let low = guarded + PAGE;
        for k in 0..pages {
            child
                .map_page(low + k * PAGE, Access::ReadWrite)
                .map_err(|_| Error::NoMemory)?;
        }
        return Ok(low);
    }
    Err(Error::Limit)
}

/// Starts `loaded` on processor `cpu`: places its start record for `given` and starts its first
/// thread at its entry, with RDI holding the record's address and RSP its stack's top
/// ([`place`], [`start`]).
pub(crate) fn start_program(
    mut loaded: LoadedProgram,
    given: Given<'_>,
    cpu: CpuIndex,
) -> Result<LaunchedProgram, Error> {
    let placed = place(&mut loaded.process, &given)?;
    let record = placed.record;
    let launched = start(
        loaded.process,
        placed,
        given,
        cpu,
        loaded.entry,
        loaded.stack_top,
    )?;
    Ok(LaunchedProgram {
        process: launched.process,
        record,
    })
}
