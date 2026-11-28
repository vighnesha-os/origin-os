//! Fuzz target `aml` (TASKS item 144): a DSDT's AML as raw bytes after the host's answers, under the boot's limits (`harness/src/aml.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::aml::raw(data);
});
