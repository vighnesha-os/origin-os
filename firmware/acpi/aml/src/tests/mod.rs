//! The tests: a host that models region memory and keeps a fake clock, the encoder, and the
//! tables checked by `origin-acpi-tables`, as the interpreter's callers check them.

mod encode;
mod host_ops;
mod limits;
mod load;
mod namespace;
mod qemu;
mod random;
mod semantics;

use std::collections::BTreeMap;

use crate::{
    BufferAccess, Config, DefinitionBlock, Host, HostError, IntegerWidth, Interpreter, Limits,
    PciAddress, RegionAccess, TableKey, Value,
};

/// A definition block checked by `origin-acpi-tables` (its header, length and checksum, and a
/// signature of `DSDT`, `SSDT` or `PSDT`), and the integer width its revision would set were it
/// the DSDT: what the interpreter's callers hand it (B4's contract; B1's ruling (1)).
pub(crate) fn block(
    table: &[u8],
) -> Result<(DefinitionBlock<'_>, IntegerWidth), origin_acpi_tables::Error> {
    let checked = origin_acpi_tables::definition::DefinitionBlock::parse(table)?;
    let h = checked.table().header();
    Ok((
        DefinitionBlock {
            signature: h.signature.0,
            oem_id: h.oem_id,
            oem_table_id: h.oem_table_id,
            aml: checked.aml(),
        },
        IntegerWidth::from_dsdt_revision(checked.table().revision()),
    ))
}

/// A byte of a region space: the space, the PCI function (segment, bus, device, function) for
/// PCI_Config, and the address.
pub(crate) type Cell = (u8, Option<(u16, u8, u8, u8)>, u64);

/// One access the host saw: read or write, space, address, width, value, the PCI function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) write: bool,
    pub(crate) space: u8,
    pub(crate) address: u64,
    pub(crate) width: u8,
    pub(crate) value: u64,
    pub(crate) pci: Option<PciAddress>,
}

/// A host that models every byte-addressed region as memory (zero until written), records what
/// it is asked and told, and keeps a fake clock that moves by `tick` each time it is read. With
/// `refuse` set it refuses every region access, as the kernel's boot proof will.
#[derive(Debug, Default)]
pub(crate) struct TestHost {
    pub(crate) clock: u64,
    pub(crate) tick: u64,
    /// Read the wall clock instead of the fake one, from this instant.
    pub(crate) real_clock: Option<std::time::Instant>,
    pub(crate) refuse: bool,
    pub(crate) memory: BTreeMap<Cell, u8>,
    pub(crate) seen: Vec<Seen>,
    pub(crate) refused: usize,
    /// What a buffer transfer answers: the bytes it fills, in order.
    pub(crate) transfers: Vec<(BufferSeen, Vec<u8>)>,
    pub(crate) answer: Vec<u8>,
    pub(crate) global_held: u32,
    pub(crate) global_taken: u32,
    pub(crate) global_times_out: bool,
    pub(crate) notified: Vec<(Vec<[u8; 4]>, u64)>,
    pub(crate) fatals: Vec<(u8, u32, u64)>,
    pub(crate) breakpoints: u32,
    pub(crate) debug: Vec<Vec<u8>>,
    pub(crate) tables: Vec<(TableKey, u64, Vec<u8>)>,
    /// The bridges whose secondary bus was asked for.
    pub(crate) bridges: Vec<PciAddress>,
    /// The timeouts the Global Lock was asked with (never 0xFFFF: asserted).
    pub(crate) lock_waits: Vec<u16>,
    /// The time left each region access was given (never 0: asserted).
    pub(crate) time_left: Vec<u64>,
    /// With `Some`, the host traces: each event it heard.
    pub(crate) events: Option<Vec<Event>>,
}

/// A trace event, as the model host keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Load([u8; 4]),
    Loaded(Result<u32, crate::ErrorKind>),
    Enter(Vec<[u8; 4]>),
    Leave,
    /// Address, outcome.
    Read(u64, Result<u64, HostError>),
    /// Address, value, outcome.
    Write(u64, u64, Result<(), HostError>),
    /// Address, outcome.
    Transfer(u64, Result<usize, HostError>),
    BridgeBus(PciAddress, Result<u8, HostError>),
    Limit(crate::Limit),
}

/// One buffer transfer the host saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BufferSeen {
    pub(crate) space: u8,
    pub(crate) address: u64,
    pub(crate) write: bool,
    pub(crate) attribute: u8,
    pub(crate) length: u8,
    pub(crate) connection: Option<Vec<u8>>,
    pub(crate) bit_offset: u64,
    pub(crate) bit_length: u64,
    pub(crate) sent: Vec<u8>,
}

impl TestHost {
    /// An access was given `left` of time: never none (the review of B1, F9).
    fn given(&mut self, left: u64) {
        assert!(left > 0, "a region access made with no time left");
        self.time_left.push(left);
    }
}

fn key(a: &RegionAccess<'_>, k: u64) -> Cell {
    (
        a.space.0,
        a.pci.map(|p| (p.segment, p.bus, p.device, p.function)),
        a.address.wrapping_add(k),
    )
}

impl Host for TestHost {
    fn now_100ns(&mut self) -> u64 {
        if let Some(t0) = self.real_clock {
            return u64::try_from(t0.elapsed().as_nanos() / 100).unwrap_or(u64::MAX);
        }
        self.clock = self.clock.saturating_add(self.tick);
        self.clock
    }
    fn read(&mut self, a: &RegionAccess<'_>) -> Result<u64, HostError> {
        self.given(a.time_left_100ns);
        if self.refuse {
            self.refused += 1;
            return Err(HostError::Refused);
        }
        let mut v = 0u64;
        for k in (0..u64::from(a.width / 8)).rev() {
            v = (v << 8) | u64::from(*self.memory.get(&key(a, k)).unwrap_or(&0));
        }
        self.seen.push(Seen {
            write: false,
            space: a.space.0,
            address: a.address,
            width: a.width,
            value: v,
            pci: a.pci,
        });
        Ok(v)
    }
    fn write(&mut self, a: &RegionAccess<'_>, value: u64) -> Result<(), HostError> {
        self.given(a.time_left_100ns);
        if self.refuse {
            self.refused += 1;
            return Err(HostError::Refused);
        }
        for k in 0..u64::from(a.width / 8) {
            self.memory.insert(key(a, k), (value >> (8 * k)) as u8);
        }
        self.seen.push(Seen {
            write: true,
            space: a.space.0,
            address: a.address,
            width: a.width,
            value,
            pci: a.pci,
        });
        Ok(())
    }
    fn bridge_bus(&mut self, b: &crate::BridgeBus) -> Result<u8, HostError> {
        self.given(b.time_left_100ns);
        if self.refuse {
            self.refused += 1;
            return Err(HostError::Refused);
        }
        self.bridges.push(b.bridge);
        let p = b.bridge;
        Ok(*self
            .memory
            .get(&(2, Some((p.segment, p.bus, p.device, p.function)), 0x19))
            .unwrap_or(&0))
    }
    fn transfer(&mut self, a: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError> {
        self.given(a.time_left_100ns);
        if self.refuse {
            self.refused += 1;
            return Err(HostError::Refused);
        }
        let seen = BufferSeen {
            space: a.space.0,
            address: a.address,
            write: a.write,
            attribute: a.attribute,
            length: a.length,
            connection: a.connection.map(<[u8]>::to_vec),
            bit_offset: a.bit_offset,
            bit_length: a.bit_length,
            sent: data.to_vec(),
        };
        let n = self.answer.len().min(data.len());
        data[..n].copy_from_slice(&self.answer[..n]);
        self.transfers.push((seen, self.answer.clone()));
        Ok(n)
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.clock = self.clock.saturating_add(ms.saturating_mul(10_000));
    }
    fn stall_us(&mut self, us: u8) {
        self.clock = self.clock.saturating_add(u64::from(us) * 10);
    }
    fn acquire_global_lock(&mut self, timeout_ms: u16) -> Result<bool, HostError> {
        assert_ne!(timeout_ms, 0xFFFF, "the Global Lock asked for with no end");
        self.lock_waits.push(timeout_ms);
        // The firmware's lock is not recursive (§5.2.10.1): asked for again while held, it is
        // waited for until the wait ends.
        if self.global_times_out || self.global_held > 0 {
            return Ok(false);
        }
        self.global_held += 1;
        self.global_taken += 1;
        Ok(true)
    }
    fn release_global_lock(&mut self) -> Result<(), HostError> {
        assert!(
            self.global_held > 0,
            "the Global Lock released without being held"
        );
        self.global_held -= 1;
        Ok(())
    }
    fn notify(&mut self, node: &[[u8; 4]], value: u64) {
        self.notified.push((node.to_vec(), value));
    }
    fn fatal(&mut self, kind: u8, code: u32, arg: u64) {
        self.fatals.push((kind, code, arg));
    }
    fn breakpoint(&mut self) {
        self.breakpoints += 1;
    }
    fn tracing(&self) -> bool {
        self.events.is_some()
    }
    fn trace(&mut self, event: &crate::Trace<'_>) {
        use crate::Trace;
        let Some(events) = &mut self.events else {
            return;
        };
        events.push(match *event {
            Trace::Load { signature, .. } => Event::Load(signature),
            Trace::Loaded(r) => Event::Loaded(r.map_err(|e| e.kind)),
            Trace::Enter(path) => Event::Enter(path.to_vec()),
            Trace::Leave => Event::Leave,
            Trace::Read(a, r) => Event::Read(a.address, r),
            Trace::Write(a, v, r) => Event::Write(a.address, v, r),
            Trace::Transfer(a, r) => Event::Transfer(a.address, r),
            Trace::BridgeBus(b, r) => Event::BridgeBus(b.bridge, r),
            Trace::Limit(l) => Event::Limit(l),
        });
    }
    fn debug(&mut self, text: &[u8]) {
        self.debug.push(text.to_vec());
    }
    fn table(&mut self, k: &TableKey, out: &mut dyn FnMut(u64, &[u8])) -> Result<bool, HostError> {
        match self.tables.iter().find(|(t, _, _)| t == k) {
            Some((_, address, bytes)) => {
                out(*address, bytes);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

thread_local! {
    /// The opcodes, name prefixes, locals and arguments the engine has run on this thread.
    static RAN: std::cell::RefCell<std::collections::BTreeSet<u16>> =
        const { std::cell::RefCell::new(std::collections::BTreeSet::new()) };
}

thread_local! {
    /// How many operations the engine has run on this thread (each call of [`ran`]).
    static OPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Called by the engine: `code` was run (decoded and, for an operator, completed).
pub(crate) fn ran(code: u16) {
    RAN.with(|r| {
        r.borrow_mut().insert(code);
    });
    OPS.with(|n| n.set(n.get().saturating_add(1)));
}

/// The operations run on this thread since the last call.
pub(crate) fn take_ops() -> u64 {
    OPS.with(|n| n.replace(0))
}

/// What has run on this thread, and starts afresh.
pub(crate) fn take_ran() -> std::collections::BTreeSet<u16> {
    RAN.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

thread_local! {
    /// What the budget's count and a fresh count of what is held differed by, summed over every
    /// evaluation this thread ended: (over-stated, under-stated).
    static DRIFT: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// Called by `recount`: notes how far the incremental count had drifted.
pub(crate) fn note_drift(counted: usize, held: usize) {
    DRIFT.with(|d| {
        let (over, under) = d.get();
        d.set((
            over + counted.saturating_sub(held),
            under + held.saturating_sub(counted),
        ));
    });
}

/// The drift noted on this thread, and starts afresh.
pub(crate) fn take_drift() -> (usize, usize) {
    DRIFT.with(|d| d.replace((0, 0)))
}

/// A check the library makes, weakened on purpose by a test (the contract's §7; the review of B1,
/// F4). `random.rs`'s control test plants each in turn and requires the randomized tests to fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// A failed load keeps the names it made (`engine.rs`, `unwind`).
    SkipRollback,
    /// A method's dynamic names stay when it returns (`engine.rs`, `pop_owner`).
    KeepDynamicNames,
    /// No limit on a `While`'s iterations (`engine.rs`, `repeat_while`).
    NoLoopLimit,
    /// The clock never ends an evaluation (`engine.rs`, `check_time`).
    NoTimeLimit,
    /// The memory limit is checked at twice its value (`mem.rs`, `Budget::charge`).
    LooseMemoryLimit,
    /// A buffer field may end a byte past its buffer (`exec.rs`, `create_field`).
    WeakFieldBound,
    /// The Global Lock is kept when an evaluation ends holding it (`sync.rs`, `release_held`, and
    /// the backstop in `engine.rs`, `end_evaluation`).
    KeepGlobalLock,
    /// `Load` runs a block whose checksum or signature is wrong (`load.rs`, `exec_load`).
    LoadUnchecked,
    /// A load that fails inside an evaluation keeps the names it made (`load.rs`, `fail_load`).
    KeepFailedLoad,
    /// `Load` and `LoadTable` give True for a table that did not load, and Zero for one that did
    /// (`load.rs`, `load_result`).
    InvertLoadResult,
    /// A name may be made twice in one scope (`namespace.rs`, `create`).
    AllowDuplicate,
}

thread_local! {
    static FAULT: std::cell::Cell<Option<Fault>> = const { std::cell::Cell::new(None) };
}

/// Called at each check: whether this thread planted `f`.
pub(crate) fn planted(f: Fault) -> bool {
    FAULT.with(|c| c.get() == Some(f))
}

/// The span the planted fault `LoadUnchecked` loads without its checks (`load.rs`): the declared
/// length within the bytes, whatever their checksum and signature. Here, not in `load.rs`, so
/// that the library's files hold no allocation outside `mem.rs` (the allocation gate).
pub(crate) fn unchecked_span(bytes: &[u8]) -> Option<origin_acpi_tables::header::Table<'static>> {
    use origin_acpi_tables::header::{Header, check};
    let h = Header::read(bytes).ok()?;
    let mut copy = bytes.get(..usize::try_from(h.length).ok()?)?.to_vec();
    // The checksum made right on a copy, so that the check passes: what is planted is its
    // absence.
    copy[9] = 0;
    let sum = copy.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    copy[9] = 0u8.wrapping_sub(sum);
    check(Vec::leak(copy)).ok()
}

/// Plants `f` on this thread (or none).
pub(crate) fn plant(f: Option<Fault>) {
    FAULT.with(|c| c.set(f));
}

/// Runs `f` on a thread of its own and fails if it has not ended within `secs` seconds: for the
/// tests of inputs that once hung the interpreter, so that a return of the hang fails the test
/// rather than holding the machine.
pub(crate) fn ends_within(secs: u64, what: &str, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(()) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("{what}: still running after {secs} s")
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: failed"),
    }
}

pub(crate) type Interp = Interpreter<TestHost>;

/// An interpreter with the service's limits, for integers of `width`.
pub(crate) fn interp(width: IntegerWidth) -> Interp {
    interp_with(width, Limits::SERVICE)
}

pub(crate) fn interp_with(width: IntegerWidth, limits: Limits) -> Interp {
    Interpreter::new(TestHost::default(), Config { limits }, width).expect("predefined objects")
}

/// Loads `body` as an SSDT of revision 2 into `i`.
pub(crate) fn load(i: &mut Interp, body: &[encode::B]) -> crate::Result<u32> {
    let t = encode::table(b"SSDT", 2, body);
    let (block, _) = self::block(&t).expect("encoder tables are well-formed");
    i.load(block)
}

/// Evaluates `path` with no arguments.
pub(crate) fn eval(i: &mut Interp, path: &str) -> crate::Result<Option<Value>> {
    i.evaluate(path, &[])
}

/// Evaluates `path` and expects an integer.
#[track_caller]
pub(crate) fn int(i: &mut Interp, path: &str) -> u64 {
    match eval(i, path) {
        Ok(Some(Value::Integer(v))) => v,
        other => panic!("{path}: expected an integer, got {other:?}"),
    }
}
