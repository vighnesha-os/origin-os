//! Fuzz target `ocbor_built` (TASKS item 144): OCBOR-1 documents and KRN manifests written from the fuzzer's choices (`harness/src/ocbor.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::ocbor::built(data);
});
