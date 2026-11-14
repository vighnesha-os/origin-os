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
//! **The library reaches nothing of the host** (ADR-0011 Amendment 4, T1). It reads and writes
//! no file, runs no program and prints nothing: its caller hands it each definition's text and
//! name and the versions file's text ([`compile`]), and writes what it returns
//! ([`codegen::generate_output`], [`fingerprint::fingerprint_file`],
//! [`fingerprint::updated_lock`]). Its callers are the command line (`src/main.rs`), a thin
//! client, and `xtask`, which calls it in process. `cargo xtask host-tools-gate` holds the rule for
//! the forms it reads, which its doc comment lists with what it cannot see; it also holds this
//! crate's dependencies to a listed set, and the command line to naming no crate but this one
//! and `std`.
//!
//! The owner accepted the format on 2026-11-09 (`docs/spec/OIDL.md` §19, CLAUDE.md §84). Generated
//! code is written to a directory the caller names; no Origin OS component uses it yet.
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
