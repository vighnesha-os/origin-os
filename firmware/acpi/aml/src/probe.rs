//! A concrete interpreter for `cargo xtask check`'s no-panic check (the review of B1, F3).
//!
//! The interpreter is generic over its host, and a library compiled alone holds no code of a
//! generic function until something instantiates it. This module instantiates it with a host that
//! refuses everything and runs each entry point once, so the release object the check reads holds
//! the engine, the operators, the conversions, the stores, the regions, the synchronization, the
//! walk and the API. The check also requires this function's symbol, so that a change dropping it
//! fails rather than passing having seen nothing. Nothing else calls it.

use crate::host::{BufferAccess, Host, HostError, RegionAccess, TableKey};
use crate::{Config, DefinitionBlock, IntegerWidth, Interpreter, Limits, Space, Value};

/// A host that refuses every request and keeps no time.
struct Refusing;

impl Host for Refusing {
    fn now_100ns(&mut self) -> u64 {
        0
    }
    fn read(&mut self, _: &RegionAccess<'_>) -> Result<u64, HostError> {
        Err(HostError::Refused)
    }
    fn write(&mut self, _: &RegionAccess<'_>, _: u64) -> Result<(), HostError> {
        Err(HostError::Refused)
    }
    fn transfer(&mut self, _: &BufferAccess<'_>, _: &mut [u8]) -> Result<usize, HostError> {
        Err(HostError::Refused)
    }
    fn sleep_ms(&mut self, _: u64) {}
    fn stall_us(&mut self, _: u8) {}
    fn acquire_global_lock(&mut self, _: u16) -> Result<bool, HostError> {
        Err(HostError::Refused)
    }
    fn release_global_lock(&mut self) -> Result<(), HostError> {
        Err(HostError::Refused)
    }
    fn notify(&mut self, _: &[[u8; 4]], _: u64) {}
    fn fatal(&mut self, _: u8, _: u32, _: u64) {}
    fn debug(&mut self, _: &[u8]) {}
    fn table(&mut self, _: &TableKey, _: &mut dyn FnMut(u64, &[u8])) -> Result<bool, HostError> {
        Err(HostError::Refused)
    }
}

/// Loads `aml` and runs every entry point once. For the no-panic check only.
#[doc(hidden)]
pub fn no_panic_probe(aml: &[u8], path: &str, args: &[Value]) -> usize {
    let config = Config {
        limits: Limits::BOOT_PROOF,
    };
    let Ok(mut i) = Interpreter::new(Refusing, config, IntegerWidth::Bits64) else {
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
    i.initialize(&mut |_, _| n = n.saturating_add(1));
    let _ = i.connect_space(Space::EMBEDDED_CONTROL, true, &mut |_, _| {});
    n.saturating_add(i.memory_used())
}
