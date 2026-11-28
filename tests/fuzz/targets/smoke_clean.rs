//! Smoke target with nothing to find (TASKS item 144, step 1): a run for a fixed time must end
//! with success, so a fixed-time run's exit code can be trusted.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut sum = 0u32;
    for &b in data {
        sum = sum.rotate_left(5) ^ u32::from(b);
    }
    std::hint::black_box(sum);
});
