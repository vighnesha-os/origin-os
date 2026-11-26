//! Fuzz target `loader_place` (TASKS item 144): raw bytes resealed as an ONBF image, then the UEFI loader's verification and placement (`harness/src/onbf.rs`).
#![no_main]

libfuzzer_sys::fuzz_target!(init: origin_fuzz_harness::require_overflow_checks(), |data: &[u8]| {
    origin_fuzz_harness::onbf::exercise_loader(data);
});
