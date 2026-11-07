//! Unit tests: the ELF contract (one negative test per hard error), valid conversions
//! checked independently with `onbf-format`, `onbf-merkle` and the RELR decoder, RELR and
//! OCBOR-1 encodings, and determinism.

mod convert;
mod elf_writer;
mod encodings;
mod reject;

use crate::{KrnOptions, Toolchain};

/// Fixed options, so no test depends on the host's `rustc`.
pub(crate) fn options() -> KrnOptions {
    KrnOptions {
        cpu_features: 0,
        flags: "-C opt-level=3 -C force-frame-pointers=yes".to_string(),
        toolchain: Toolchain {
            rustc: "1.99.1".to_string(),
            rustc_commit: "58cf8f9c35b355199727627d07ae34a4e2846d67".to_string(),
            llvm: "23.1.1".to_string(),
        },
    }
}
