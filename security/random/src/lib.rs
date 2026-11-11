//! `origin-random`: the policy of Origin OS's random-number generator (CLAUDE.md §59;
//! ADR-0016 R2 to R8 and R10; TASKS 18b).
//!
//! The mechanism, NIST's HMAC_DRBG over SHA-512, is in `origin-crypto` and makes no entropy.
//! This crate decides everything around it:
//!
//! - **the tests** every draw of RDSEED and RDRAND passes before it is used ([`Health`]);
//! - **how often an instruction is tried**, and what happens when it is not ready or stuck;
//! - **how a first seed is put together** for a root and for a processor's generator
//!   ([`seed`]);
//! - **when a generator is reseeded**, from what, and what backs off ([`Node`]);
//! - **the root** that seeds the processors' generators and answers requests with prediction
//!   resistance ([`Root`]);
//! - **the known-answer self-test** that runs before any generator exists ([`self_test`]).
//!
//! It touches no hardware. The caller implements [`Platform`]: the two instructions, a clock,
//! and how a step is run. In the Framekernel a step runs with interrupts off on a stack that
//! is overwritten afterwards (R9); the loader and the host tests implement the same trait. So
//! all of the policy is tested on the build host.
//!
//! **What this is not.** Origin claims no conformance to SP 800-90A, 90B or 90C and no
//! validation (ADR-0016 R1). The tests here catch gross failure of an instruction only.
//!
//! Contract, the same as `origin-crypto`'s (ONBF §12): `no_std`, no allocation, no `unsafe`
//! and no panic path. No branch and no memory index depends on a draw, a seed or a state,
//! except the health tests' comparisons of one draw with another, whose outcome is public:
//! it decides whether the instruction is used at all.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        clippy::cast_possible_truncation
    )
)]

mod drive;
mod health;
mod kat;
mod node;
mod root;
pub mod seed;
mod selftest;
mod wipe;

#[cfg(test)]
mod tests;

pub use drive::{
    Busy, Ctx, Error, Platform, ProcessorStart, RootStart, fill, fill_fresh, fill_from_root,
    next_u64, start_processor, start_root,
};
pub use health::{Condition, Fault, Health, Offer, STARTUP_DRAWS, WINDOW};
pub use node::{
    Allow, Counts, Instruction, Maint, Node, PIECE, Piece, Purpose, RESEED_DRAWS, RESEED_NANOS,
    RESEED_REQUESTS, REST_NANOS, Source, Try,
};
pub use root::{Answer, Root, RootCounts};
pub use selftest::run as self_test;
