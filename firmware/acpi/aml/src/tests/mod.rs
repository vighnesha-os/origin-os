//! The tests: a host that refuses every region and keeps a fake clock, the encoder, and the
//! test-only stand-in for `origin-acpi-tables`.

mod encode;
mod semantics;
mod tables_standin;

use crate::{
    BufferAccess, Config, Host, HostError, IntegerWidth, Interpreter, Limits, RegionAccess,
    TableKey, Value,
};

/// A host that refuses every region access and records what it is told. Its clock moves by
/// `tick` every time it is read, so the time limit is reached by any loop that runs long enough.
#[derive(Debug, Default)]
pub(crate) struct TestHost {
    pub(crate) clock: u64,
    pub(crate) tick: u64,
    pub(crate) debug: Vec<Vec<u8>>,
    pub(crate) refused: usize,
}

impl Host for TestHost {
    fn now_100ns(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(self.tick);
        self.clock
    }
    fn read(&mut self, _: &RegionAccess<'_>) -> Result<u64, HostError> {
        self.refused += 1;
        Err(HostError::Refused)
    }
    fn write(&mut self, _: &RegionAccess<'_>, _: u64) -> Result<(), HostError> {
        self.refused += 1;
        Err(HostError::Refused)
    }
    fn transfer(&mut self, _: &BufferAccess<'_>, _: &mut [u8]) -> Result<usize, HostError> {
        self.refused += 1;
        Err(HostError::Refused)
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.clock = self.clock.saturating_add(ms.saturating_mul(10_000));
    }
    fn stall_us(&mut self, us: u8) {
        self.clock = self.clock.saturating_add(u64::from(us) * 10);
    }
    fn acquire_global_lock(&mut self, _: u16) -> Result<bool, HostError> {
        Ok(true)
    }
    fn release_global_lock(&mut self) -> Result<(), HostError> {
        Ok(())
    }
    fn notify(&mut self, _: &[[u8; 4]], _: u64) {}
    fn fatal(&mut self, _: u8, _: u32, _: u64) {}
    fn debug(&mut self, text: &[u8]) {
        self.debug.push(text.to_vec());
    }
    fn table(&mut self, _: &TableKey, _: &mut dyn FnMut(u64, &[u8])) -> Result<bool, HostError> {
        Ok(false)
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
