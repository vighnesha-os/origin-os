//! Fuzz target `signature` (TASKS item 144): a signature block as raw bytes: its parser and verifiers (`harness/src/signature.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::signature::raw(data);
});
