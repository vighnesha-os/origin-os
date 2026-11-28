//! Smoke target with a planted crash (TASKS item 144, step 1).
//!
//! The crash is behind four bytes compared one at a time. Blind random input reaches it once
//! in about 2^32 tries; libFuzzer reaches it in seconds only if the build's comparison and
//! edge instrumentation works. So a run that finds it shows the coverage guidance works, and
//! that a panic is caught and its input kept.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let [b'O', b'R', b'G', b'N', ..] = data {
        panic!("planted: the smoke target's magic was found");
    }
});
