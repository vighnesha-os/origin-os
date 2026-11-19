//! `origin-aml`: Origin OS's interpreter for the ACPI Machine Language, ACPI 6.6 chapter 20
//! (ADR-0019 A3 and A5; TASKS item 63).
//!
//! The firmware's definition blocks (the DSDT and SSDTs) are loaded into one namespace, and their
//! control methods are run on request. The interpreter is a library: it reaches hardware, time,
//! the Global Lock and notifications only through the [`Host`] its caller provides, and the host,
//! not the library, decides what may be touched (ADR-0019 A2). Until the user-mode ACPI service
//! exists, the kernel's boot proof will be its host (step B3); the tests are its host today.
//!
//! **Hostile input.** A definition block is untrusted (ADR-0019 T1). The library has no `unsafe`
//! and never recurses in Rust: the engine keeps its own stack of frames on the heap, so a block's
//! nesting cannot exhaust the caller's stack. It is written not to panic: the lints below refuse
//! indexing, unchecked arithmetic, `unwrap`, `expect` and the panicking macros; the one panicking
//! slice method it uses is given equal lengths by construction; and `cargo xtask check`'s no-panic
//! check reads the release object, which holds the whole interpreter through [`no_panic_probe`]
//! (the review of B1, F3). Every evaluation runs under the [`Limits`]: loop iterations, time,
//! call depth, memory and steps, a step being an opcode or a unit of the work between opcodes
//! (name comparisons, field-list entries, bytes copied, the collector's slots: the review of B1,
//! F1). **Every loop in the library is bounded by the limits** (the audit after N-soak-1, the
//! soak's hang): it has a bound the code fixes (a constant, the integer width, the call-depth
//! limit, the field chain's depth); or it walks an input that is counted as it goes, each pass a
//! step, a unit charged per element, entry, unit, comparison or level, or a unit for each 64
//! bytes charged before it runs or by the allocation that holds them; or it charges at least one
//! unit per pass. No host call may wait past the time left (see [`Host`]). When a limit is reached the
//! evaluation stops, every name it created is removed, and the error says which limit and where.
//! A table whose load fails is rolled back whole.
//!
//! **Memory.** Objects live in an arena named by handles with generations, collected by a
//! mark-and-sweep collector at safe points between steps; every allocation goes through `mem.rs`,
//! which charges it to the memory limit before it is made.
//!
//! **What is built** (steps B1 to B5 of TASKS item 63): the whole parser, the namespace,
//! every operator, field units over every region space through the host, mutexes and the Global
//! Lock, events, the device initialization walk ([`Interpreter::initialize`]) and `_REG`
//! ([`Interpreter::connect_space`]). `Load` and `LoadTable` load a table while an evaluation
//! runs, on the engine's frame stack, after `origin-acpi-tables` has checked it (`load.rs`).
//! `Unload` is refused (G8).
//!
//! **Tolerances** (step B5; `tolerate.rs`): where TASKS 64's ledger says deployed firmware or
//! other implementations rely on ACPICA's default, the library follows it, never ACPICA's lenient
//! mode, and tells its host of each tolerance through [`Host::report`], bounded per evaluation:
//! a load is best-effort, a failing statement of load-time code abandoned; a mutex outlives the
//! method that took it until the outermost evaluation ends; load-time package names are bound
//! once ([`Interpreter::bind_deferred`]).
//!
//! Sources: ACPI 6.6 and Microsoft's `_OSI` documentation, paraphrased
//! (`docs/research/2026-11-14-aml-interpreter.md`), and, for step B5, TASKS item 64's ledger
//! (`docs/research/2026-11-14-aml-gaps-acpica.md`), which another session wrote from ACPICA's
//! source under CLAUDE.md §3. No interpreter's source was read by the session that wrote this
//! code.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::cast_possible_truncation
    )
)]

extern crate alloc;

mod config;
mod convert;
mod engine;
mod error;
mod exec;
mod host;
mod init;
mod load;
mod mem;
mod name;
mod namespace;
mod object;
mod op;
mod osi;
mod pci;
mod probe;
mod region;
mod store;
mod sync;
mod tolerate;
mod value;

#[cfg(test)]
mod tests;

pub use config::{Config, DefinitionBlock, IntegerWidth, Limits};
pub use error::{At, Error, ErrorKind, Limit, Result};
pub use host::{
    BridgeBus, BufferAccess, Host, HostError, PciAddress, RegionAccess, Report, Space, TableKey,
    Trace,
};
pub use init::InitOutcome;
pub use osi::{OS_NAME, OSI_STRINGS, REV};
#[doc(hidden)]
pub use probe::no_panic_probe;
pub use tolerate::REPORTS;
pub use value::{Node, Plain, Value};

use alloc::vec::Vec;

use engine::{CallFrame, Cursor, Frame, OwnerCtx};
use mem::Budget;
use namespace::Namespace;
use object::{Heap, Val};

/// The revision the `Revision` opcode answers: this interpreter's own, 1 (the contract's §6).
pub const INTERPRETER_REVISION: u64 = 1;

/// A loaded table's identity, as [`Interpreter::table`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableInfo {
    /// The signature.
    pub signature: [u8; 4],
    /// The OEM ID.
    pub oem_id: [u8; 6],
    /// The OEM table ID.
    pub oem_table_id: [u8; 8],
    /// Its load failed and was rolled back.
    pub failed: bool,
    /// Statements its load abandoned and reported, the load going on (step B5; saturating).
    pub abandoned: u32,
}

/// A loaded definition block.
#[derive(Debug)]
struct Table {
    aml: Vec<u8>,
    signature: [u8; 4],
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    /// Its load failed and was rolled back: its methods are not run.
    failed: bool,
    /// Statements its load abandoned (step B5; saturating).
    abandoned: u32,
}

/// The counts of one top-level evaluation, against the limits.
#[derive(Clone, Copy, Debug, Default)]
struct RunCounts {
    steps: u64,
    loops: u64,
    depth: u32,
    started: u64,
    /// The budget's `charged` when the work done was last counted.
    charged_seen: u64,
    /// Units counted since the clock was last read.
    since_clock: u64,
    /// Reports made this evaluation, given or not ([`REPORTS`]).
    reports: u64,
}

/// An AML interpreter: one namespace, its tables and its host.
pub struct Interpreter<H: Host> {
    host: H,
    config: Config,
    width: IntegerWidth,
    budget: Budget,
    heap: Heap,
    ns: Namespace,
    tables: Vec<Table>,
    frames: Vec<Frame>,
    /// For each frame, the scope names resolve in and the end of the innermost package around
    /// it, so that neither needs a scan of the frames (the review of B1, A1).
    ctx: Vec<engine::Ctx>,
    calls: Vec<CallFrame>,
    owners: Vec<OwnerCtx>,
    cur: Cursor,
    serial: u64,
    /// The mutexes the running evaluation holds, in the order taken (`sync.rs`).
    held: Vec<sync::Held>,
    /// How deep the runs of frames for a PCI_Config region outside the frames go (`pci.rs`,
    /// `with_address`): the collector runs only at 0.
    nested: u32,
    /// The region whose function an access is waiting for (`pci.rs`).
    pending: Option<object::NodeId>,
    /// How many `Loading` frames are on the stack (`load.rs`).
    loading: u32,
    /// The activations there were when the innermost table being loaded began: while there are
    /// as many, the code running is that table's table level (`load.rs`), or `usize::MAX` when
    /// no table is being loaded (the review of B4's addendum, B4-13).
    load_calls: usize,
    /// How many `Resolve` frames are on the stack.
    resolving: u32,
    /// How many holds of the Global Lock the running evaluation has: the host holds it for all
    /// of them (`sync.rs`).
    global_depth: u32,
    run: RunCounts,
    /// Steps counted by every evaluation so far, loads included ([`Interpreter::steps_done`]).
    steps_done: u64,
    /// The frame count below which the running evaluation does not reach.
    base: usize,
    /// The value the running evaluation ended with.
    result: Val,
    gc_threshold: usize,
}

impl<H: Host> core::fmt::Debug for Interpreter<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Interpreter")
            .field("width", &self.width)
            .field("tables", &self.tables.len())
            .field("nodes", &self.ns.live())
            .field("objects", &self.heap.live())
            .field("memory", &self.budget.used())
            .finish_non_exhaustive()
    }
}

impl<H: Host> Interpreter<H> {
    /// The host.
    pub const fn host(&self) -> &H {
        &self.host
    }

    /// The host, mutably.
    pub const fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// The integer width the DSDT set.
    pub const fn width(&self) -> IntegerWidth {
        self.width
    }

    /// Bytes held now, against [`Limits::memory_bytes`].
    pub const fn memory_used(&self) -> usize {
        self.budget.used()
    }

    /// The most bytes ever held at once.
    pub const fn memory_peak(&self) -> usize {
        self.budget.peak()
    }

    /// Nodes in the namespace.
    pub const fn node_count(&self) -> usize {
        self.ns.live()
    }

    /// Objects alive in the arena.
    pub const fn object_count(&self) -> usize {
        self.heap.live()
    }

    /// Steps counted by every top-level evaluation so far, loads and the initialization walk's
    /// included: for a caller that bounds several evaluations together, as the kernel's boot proof
    /// does (the review of B3, B3-1). Each evaluation is still bounded by the limits on its own.
    pub const fn steps_done(&self) -> u64 {
        self.steps_done
    }

    /// Tables loaded or attempted, failed ones included.
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Table `index`'s identity, and whether its load failed.
    pub fn table(&self, index: u32) -> Option<TableInfo> {
        let t = self.tables.get(usize::try_from(index).ok()?)?;
        Some(TableInfo {
            signature: t.signature,
            oem_id: t.oem_id,
            oem_table_id: t.oem_table_id,
            failed: t.failed,
            abandoned: t.abandoned,
        })
    }
}
