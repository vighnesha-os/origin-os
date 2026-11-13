//! The launch path (ADR-0005 P6, amendments C1 and C4–C6; ADR-0011 N14 step 3, task S3-3): the
//! one way the kernel starts a process with a start record.
//!
//! A creator makes the child with [`Process::proof_child_of`] (the boot proofs) or
//! [`Process::pinned_child_of`] (a program), so the child acts for the creator's principal and
//! for no other; maps its image; has [`place`] put the start record's pages at a
//! random address, read-only; then [`start`] gives the child its own process and first thread
//! and what the creator passes (BOOTSTRAP and DIRECTORY channel ends), writes the record from
//! what the child's table then holds, and starts the first thread with RDI holding the
//! record's address. Until the Loader Service exists (ONBF §8.3), only kernel code launches.
//!
//! A launch that fails after the child holds anything closes the child's table: the child's
//! handle to itself would otherwise keep a process that never ran alive for ever.
//!
//! A program the kernel carries in its image (ADR-0011 Amendment 2, L1 to L3 and L7; tasks S4-1
//! and S4-2) is launched in two steps around those two. [`load_program`] makes the child pinned
//! per call (ADR-0005 B2): it maps `origin.core` into it at a random place, read-execute, on the
//! one set of frames every process shares, which records that place as where the child's calls
//! must come from. It then places the program at another random base, maps each page with its
//! final permissions and writes it through its frame, relocations applied and each import slot
//! holding the address of its export in the child's `origin.core`; and maps a stack at a third
//! random place with an unmapped page on either side. Nothing is ever writable and executable,
//! and RELRO pages, the import slots among them, are read-only to the program from the start.
//! [`start_program`] then places the start record and starts the first thread at the entry, as
//! [`place`] and [`start`] do for any child.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use origin_abi::start::{self, Entry, role};
use origin_abi::{Error, Handle, rights};
use origin_frame::objref::ObjectRef;
use origin_frame::userspace::{ChangeError, UserFrame};

use crate::core_image;
use crate::cpu::{CpuIndex, without_interrupts};
use crate::handles::{Need, Typed};
use crate::ipc::{ChannelEnd, Object};
use crate::paging::{Access, MapError, PAGE};
use crate::process::{Limits, Process, ThreadObject};
use crate::program::{self, Carried, CoreAt, SegmentAccess};

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

/// What a launch reports when it cannot map a page, with the reason kept (ADR-0005 D4; S4-1's
/// review, finding F4): `LIMIT` when the child has as many page tables as one space may have,
/// `NO_MEMORY` for a want of memory, and `INVALID_ARGS` for an address the launch path never
/// asks for (outside the user half, or already mapped).
fn map_refused(e: ChangeError) -> Error {
    match e {
        ChangeError::Limit => Error::Limit,
        ChangeError::Map(MapError::OutOfMemory) => Error::NoMemory,
        _ => Error::InvalidArgs,
    }
}

/// Maps the pages of `child`'s start record, for what `given` holds, read-only and not
/// executable, at a random address no other mapping of the child touches (C4). The record is
/// written by [`start`]. `LIMIT` past 64 handles or 64 KiB, or when the child's page tables are
/// at their bound, `NOT_SUPPORTED` without a random-number generator, `NO_MEMORY` without frames.
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
                .map_err(map_refused)?;
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
        if !process.write_page(*frame, 0, chunk) {
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
    /// Pages of its first thread's stack: at least one, and at most
    /// [`program::MAX_STACK_PAGES`].
    pub(crate) stack_pages: u64,
}

/// A carried program in a new process, mapped and relocated, its first thread not started.
pub(crate) struct LoadedProgram {
    /// The child, which nothing else holds yet.
    pub(crate) process: Process,
    /// Where `origin.core` starts in the child: the base its calls are pinned to.
    pub(crate) core: u64,
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

/// Makes `creator`'s child for `program`, under `run`'s limits, pinned per call (ADR-0005 B2),
/// and maps `origin.core`, the program and a stack into it, each at a random place of its own.
/// `origin.core` is mapped read-execute on the frames every process shares, and its place is
/// recorded as where the child's calls must come from (ADR-0011 L2, L7). Every page of the
/// program is mapped once, with its final permissions, and written through its frame:
/// read-execute for code, read-only for read-only data and for RELRO once its relocations and
/// its imports are in (L3), read-write and never executable for data.
///
/// `INVALID_ARGS`, before anything is made, for a description that breaks [`program::check`]'s
/// rules or a stack of no pages or of more than [`program::MAX_STACK_PAGES`]; `NOT_SUPPORTED`
/// without a random-number generator; `NO_MEMORY` without frames, or if `origin.core` was never
/// written into its frames; `LIMIT` if no free place is found, or when the image and stack need
/// more page tables than one space may have. That last is found part-way through mapping, not
/// estimated first: a program or stack spread over more than about 40 MiB is refused after much
/// of it is mapped (S4-1's review, finding F4, kept as a limit; the boot proof shows such a
/// refusal gives everything back). On every error the child, and every frame and table it was
/// given, is dropped, and its mappings of `origin.core`'s frames with it.
pub(crate) fn load_program(
    creator: &Process,
    program: &Carried,
    run: &ProgramLimits,
) -> Result<LoadedProgram, Error> {
    let image = core_image::image();
    program::check(program, image).map_err(|_| Error::InvalidArgs)?;
    if run.stack_pages == 0 || run.stack_pages > program::MAX_STACK_PAGES {
        return Err(Error::InvalidArgs);
    }
    let mut child = Process::pinned_child_of(creator, run.limits).ok_or(Error::NoMemory)?;
    let core = place_core(&mut child, image.span / PAGE)?;
    let base = place_image(&mut child, program, CoreAt { base: core, image })?;
    let (stack, stack_top) = place_stack(&mut child, run.stack_pages)?;
    Ok(LoadedProgram {
        process: child,
        core,
        base,
        entry: base + program.entry,
        stack,
        stack_top,
    })
}

/// Maps `origin.core`, `pages` pages of it, into `child` at a random place in the programs'
/// region, which nothing else of the child uses yet, and returns that place: the base the
/// child's calls are pinned to from now on.
fn place_core(child: &mut Process, pages: u64) -> Result<u64, Error> {
    for _ in 0..PLACEMENT_TRIES {
        let place = program::random_base(pages, random()?).ok_or(Error::Limit)?;
        if (0..pages).any(|k| child.is_mapped(place + k * PAGE)) {
            continue;
        }
        child.map_core(place)?;
        return Ok(place);
    }
    Err(Error::Limit)
}

/// Maps `program` into `child` at a random base whose whole span is free, its imports bound to
/// `core`, and returns the base.
fn place_image(child: &mut Process, program: &Carried, core: CoreAt<'_>) -> Result<u64, Error> {
    let pages = program.span / PAGE;
    for _ in 0..PLACEMENT_TRIES {
        let base = program::random_base(pages, random()?).ok_or(Error::Limit)?;
        // Clear of `origin.core`, the one thing mapped so far: a page of the span that no
        // segment holds must stay unmapped too.
        if (0..pages).any(|k| child.is_mapped(base + k * PAGE)) {
            continue;
        }
        map_image(child, program, base, core)?;
        return Ok(base);
    }
    Err(Error::Limit)
}

/// Maps every page of `program` at `base` into `child`, relocations applied and imports bound
/// to `core`.
fn map_image(
    child: &mut Process,
    program: &Carried,
    base: u64,
    core: CoreAt<'_>,
) -> Result<(), Error> {
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
            program::fill_page(program, index, k, base, core, &mut page)
                .map_err(|_| Error::InvalidArgs)?;
            let frame = child
                .map_page(base + segment.rva + k * PAGE, access)
                .map_err(map_refused)?;
            // A fresh frame is zero: only a page with bytes is written.
            if page.iter().any(|&b| b != 0) && !child.write_page(frame, 0, &page) {
                return Err(Error::NoMemory);
            }
        }
    }
    Ok(())
}

/// Maps `pages` pages of stack, read-write and never executable, at a random place in the
/// programs' region with an unmapped page below and above it that nothing else of `child` uses.
/// Returns its lowest page and one past its highest: the first thread's RSP.
fn place_stack(child: &mut Process, pages: u64) -> Result<(u64, u64), Error> {
    let guarded = pages.checked_add(2).ok_or(Error::InvalidArgs)?;
    let bytes = pages.checked_mul(PAGE).ok_or(Error::InvalidArgs)?;
    for _ in 0..PLACEMENT_TRIES {
        // `random_base` keeps all `guarded` pages below `PROGRAM_HIGH`, so nothing here overflows.
        let place = program::random_base(guarded, random()?).ok_or(Error::Limit)?;
        if (0..guarded).any(|k| child.is_mapped(place + k * PAGE)) {
            continue;
        }
        let low = place + PAGE;
        for k in 0..pages {
            child
                .map_page(low + k * PAGE, Access::ReadWrite)
                .map_err(map_refused)?;
        }
        return Ok((low, low + bytes));
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
