//! A concrete interpreter for `cargo xtask check`'s no-panic check (the review of B1, F3).
//!
//! The interpreter is generic over its host, and a library compiled alone holds no code of a
//! generic function until something instantiates it. This module instantiates it with a host whose
//! answers the optimizer cannot see through and runs each entry point once, so the release object
//! the check reads holds the engine, the operators, the conversions, the stores, the regions, the
//! synchronization, the walk and the API, after a host's success and after its refusal alike. The check also requires this function's symbol, so that a change dropping it
//! fails rather than passing having seen nothing. Nothing else calls it.

use core::hint::black_box;

use crate::host::{BridgeBus, BufferAccess, Host, HostError, RegionAccess, TableKey, Trace};
use crate::{Config, DefinitionBlock, IntegerWidth, Interpreter, Limits, Space, Value};

/// A host whose every answer is opaque to the compiler (`core::hint::black_box`): it refuses
/// everything and keeps no time, but the optimizer cannot know that, so the code after a success
/// and after a refusal both stay in the object (the review of B1, A2). It traces, so that the
/// tracing paths are in the object too.
struct Opaque;

impl Host for Opaque {
    fn now_100ns(&mut self) -> u64 {
        black_box(0)
    }
    fn read(&mut self, a: &RegionAccess<'_>) -> Result<u64, HostError> {
        black_box(a);
        black_box(Err(HostError::Refused))
    }
    fn write(&mut self, a: &RegionAccess<'_>, v: u64) -> Result<(), HostError> {
        black_box((a, v));
        black_box(Err(HostError::Refused))
    }
    fn bridge_bus(&mut self, b: &BridgeBus) -> Result<u8, HostError> {
        black_box(b);
        black_box(Err(HostError::Refused))
    }
    fn transfer(&mut self, a: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError> {
        black_box((a, &mut *data));
        black_box(Err(HostError::Refused))
    }
    fn sleep_ms(&mut self, ms: u64) {
        black_box(ms);
    }
    fn stall_us(&mut self, us: u8) {
        black_box(us);
    }
    fn acquire_global_lock(&mut self, t: u16) -> Result<bool, HostError> {
        black_box(t);
        black_box(Err(HostError::Refused))
    }
    fn release_global_lock(&mut self) -> Result<(), HostError> {
        black_box(Err(HostError::Refused))
    }
    fn notify(&mut self, n: &[[u8; 4]], v: u64) {
        black_box((n, v));
    }
    fn fatal(&mut self, k: u8, c: u32, a: u64) {
        black_box((k, c, a));
    }
    fn debug(&mut self, t: &[u8]) {
        black_box(t);
    }
    fn breakpoint(&mut self) {
        black_box(());
    }
    fn report(&mut self, r: &crate::Report<'_>) {
        black_box(r);
    }
    fn tracing(&self) -> bool {
        black_box(true)
    }
    fn trace(&mut self, e: &Trace<'_>) {
        black_box(e);
    }
    fn table(&mut self, k: &TableKey, _: &mut dyn FnMut(u64, &[u8])) -> Result<bool, HostError> {
        black_box(k);
        black_box(Err(HostError::Refused))
    }
}

/// Loads `aml` and runs every entry point once. For the no-panic check only.
#[doc(hidden)]
pub fn no_panic_probe(aml: &[u8], path: &str, args: &[Value]) -> usize {
    let config = Config {
        limits: Limits::BOOT_PROOF,
    };
    let Ok(mut i) = Interpreter::new(Opaque, config, IntegerWidth::Bits64) else {
        return 0;
    };
    let block = DefinitionBlock {
        signature: *b"SSDT",
        oem_id: *b"ORIGIN",
        oem_table_id: *b"PROBE   ",
        aml,
    };
    let mut n = usize::from(i.load(block).is_ok());
    if let Ok(Some(node)) = i.lookup(path) {
        n = n.saturating_add(usize::from(i.evaluate_node(node, args).is_ok()));
        n = n.saturating_add(i.path_of(node).map_or(0, |p| p.len()));
    }
    let _ = i.initialize(&mut |_, _| n = n.saturating_add(1));
    let _ = i.connect_space(Space::EMBEDDED_CONTROL, true, &mut |_, _| {});
    n.saturating_add(i.memory_used())
}
