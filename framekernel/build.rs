//! Left in place for one commit, and empty (TASKS item 44).
//!
//! The link arguments this script used to print are now in `.cargo/config.toml`, under the
//! kernel's target. While a build script exists Cargo gives this crate the `-C metadata`
//! value it gave it before, so a kernel built from this commit must be the same bytes as one
//! built from the commit before it. That is the evidence that every argument still reaches
//! the linker, in the same order. The next commit removes this file.

fn main() {}
