//! Fuzz target `aml_built` (TASKS item 144): AML term lists written from the fuzzer's choices, under the boot's limits (`harness/src/aml.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::aml::built(data);
});
