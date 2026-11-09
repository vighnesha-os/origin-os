//! `oidl`: the Origin interface language (`docs/spec/OIDL.md`, ADR-0011 N7). Host only; it is
//! not part of the shipping OS.
//!
//! - [`parse`] reads one definition into a syntax tree ([`lex`] and [`ast`]), treating the file
//!   as untrusted input.
//! - [`sema`] resolves names, computes layouts ([`layout`]) and applies every rule of the
//!   specification, producing a [`model`].
//! - The generators ([`codegen`]) turn a model into Rust, C and documentation; [`fingerprint`] and
//!   [`corpus`] produce the ABI fingerprint and the hostile-message corpus, the corpus labelled
//!   by [`wire`], an independent model of the wire format.
//!
//! Generation is deterministic: the same input bytes give the same output, with no timestamps,
//! paths or other environment input (CLAUDE.md §75).
//!
//! **The wire format is Proposed**: generated code is written to scratch locations only. No
//! Origin OS component uses it until the owner accepts the format (CLAUDE.md §84).
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )
)]

pub mod ast;
pub mod codegen;
pub mod corpus;
pub mod diag;
pub mod fingerprint;
pub mod layout;
pub mod lex;
pub mod model;
pub mod names;
pub mod parse;
pub mod sema;
pub mod wire;

#[cfg(test)]
#[path = "../../../runtime/onbf/format/src/testing.rs"]
mod testing;
#[cfg(test)]
mod tests;

pub use diag::{Code, Diagnostic};
pub use sema::{Input, compile};
