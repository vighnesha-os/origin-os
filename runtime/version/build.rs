//! Left in place for one commit, and empty (TASKS item 44).
//!
//! The constants this script used to write are now `src/generated.rs`. While a build script
//! exists Cargo gives this crate, and every crate that depends on it, the `-C metadata`
//! values it gave them before, so an image built from this commit must be the same bytes as
//! one built from the commit before it. That is the evidence that every constant kept its
//! value. The next commit removes this file.

fn main() {}
