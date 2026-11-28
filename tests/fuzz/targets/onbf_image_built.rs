//! Fuzz target `onbf_image_built` (TASKS item 144): ONBF images written from the fuzzer's choices with their digests sealed, then every reader of them and the loader's paths (`harness/src/onbf.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::onbf::built(data);
});
