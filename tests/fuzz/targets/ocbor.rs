//! Fuzz target `ocbor` (TASKS item 144): OCBOR-1 as raw bytes: the validator, the cursor and the KRN manifest decoder (`harness/src/ocbor.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::ocbor::exercise(data);
});
