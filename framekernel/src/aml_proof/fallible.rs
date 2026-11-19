//! Every allocation the boot proof makes outside the interpreter, each after a `try_reserve` that
//! succeeded. While the interpreter may hold up to its memory limit, nothing the proof grows may
//! fail inside `alloc`, where a failure would end the kernel: a refusal is `None` or `false`
//! here, and the proof says what it could not keep (the coordinating session's ruling on step
//! B3's limit, condition 2; the review of B3, B3-6). `cargo xtask check`'s allocation gate refuses
//! every growing method in `aml_proof.rs` and lets this file alone grow a collection.

use alloc::vec::Vec;

use origin_aml::Value;

/// Adds `x` to `v`, or answers `false` if the heap cannot give the room.
pub(super) fn push<T>(v: &mut Vec<T>, x: T) -> bool {
    if v.try_reserve(1).is_err() {
        return false;
    }
    v.push(x);
    true
}

/// Adds what the walk reached to `v`, as [`push`]; a test may make it refuse.
pub(super) fn keep<T>(v: &mut Vec<T>, x: T) -> bool {
    #[cfg(test)]
    if REFUSE_KEEP.with(core::cell::Cell::get) {
        return false;
    }
    push(v, x)
}

/// One argument, or `None` if the heap cannot give it.
pub(super) fn one_arg(value: Value) -> Option<Vec<Value>> {
    let mut args = Vec::new();
    push(&mut args, value).then_some(args)
}

/// One string argument, or `None` if the heap cannot give it.
pub(super) fn string_arg(text: &[u8]) -> Option<Vec<Value>> {
    let mut s = Vec::new();
    s.try_reserve_exact(text.len()).ok()?;
    s.extend_from_slice(text);
    one_arg(Value::String(s))
}

#[cfg(test)]
std::thread_local! {
    /// Makes [`keep`] refuse, as a heap with no room would: the one branch of the verdict a host
    /// test cannot reach otherwise (the review of B3, B3-5).
    pub(super) static REFUSE_KEEP: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}
