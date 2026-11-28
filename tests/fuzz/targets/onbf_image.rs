//! Fuzz target `onbf_image` (TASKS item 144): an ONBF container as raw bytes, parsed under each option set and judged against ONBF §4 (`harness/src/onbf.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::onbf::exercise(data);
});
