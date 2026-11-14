//! The tests: a host that models region memory and keeps a fake clock, the encoder, and the
//! test-only stand-in for `origin-acpi-tables`.

mod encode;
mod host_ops;
mod limits;
mod namespace;
mod qemu;
mod random;
mod semantics;
mod tables_standin;

use std::collections::BTreeMap;

use crate::{
    BufferAccess, Config, Host, HostError, IntegerWidth, Interpreter, Limits, PciAddress,
    RegionAccess, TableKey, Value,
};

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
    pub(crate) sent: Vec<u8>,
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
    fn transfer(&mut self, a: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError> {
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
    fn acquire_global_lock(&mut self, _: u16) -> Result<bool, HostError> {
        if self.global_times_out {
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

/// Called by the engine: `code` was run (decoded and, for an operator, completed).
pub(crate) fn ran(code: u16) {
    RAN.with(|r| {
        r.borrow_mut().insert(code);
    });
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
    let (block, _) = tables_standin::block(&t).expect("encoder tables are well-formed");
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
