//! Build provenance for the manifest's `toolchain` map (key 2; CLAUDE.md §60).
//!
//! By default the values come from `rustc -vV`, run in the current directory, so rustup
//! selects the toolchain that `rust-toolchain.toml` pins for the repository. Each value can
//! be given explicitly instead, which is what reproducible release builds should do: the
//! packer then records exactly what the build system states, independent of the host.

use core::fmt;
use std::process::Command;

/// The three text fields of the KRN `toolchain` map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// `rustc` release, such as `1.99.1` (sub-key 0).
    pub rustc: String,
    /// `rustc` commit hash, 40 lowercase hex digits (sub-key 1).
    pub rustc_commit: String,
    /// LLVM version, such as `23.1.1` (sub-key 2).
    pub llvm: String,
}

/// Why the toolchain description could not be obtained or is invalid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolchainError {
    /// `rustc -vV` could not be run.
    Run(String),
    /// `rustc -vV` exited unsuccessfully.
    Failed(String),
    /// `rustc -vV` printed no line for this field.
    Missing(&'static str),
    /// A field breaks the KRN manifest rules (errata R4): `rustc` and `llvm` must be
    /// non-empty printable ASCII, and `rustc_commit` exactly 40 lowercase hex digits.
    Invalid {
        /// The field.
        field: &'static str,
        /// The rejected value.
        value: String,
    },
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run(e) => write!(
                f,
                "cannot run `rustc -vV` ({e}); pass --rustc-version, --rustc-commit and --llvm-version"
            ),
            Self::Failed(status) => write!(f, "`rustc -vV` failed ({status})"),
            Self::Missing(line) => write!(f, "`rustc -vV` printed no `{line}` line"),
            Self::Invalid { field, value } => {
                let rule = if *field == "rustc_commit" {
                    "exactly 40 lowercase hex digits"
                } else {
                    "non-empty printable ASCII"
                };
                write!(f, "toolchain {field} {value:?} must be {rule}")
            }
        }
    }
}

impl std::error::Error for ToolchainError {}

fn printable(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x20..=0x7E).contains(&b))
}

fn commit_hex(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Toolchain {
    /// Checks every field against the KRN manifest rules (errata R4).
    pub fn validate(&self) -> Result<(), ToolchainError> {
        let invalid = |field: &'static str, value: &str| ToolchainError::Invalid {
            field,
            value: value.to_string(),
        };
        if !printable(&self.rustc) {
            return Err(invalid("rustc", &self.rustc));
        }
        if !commit_hex(&self.rustc_commit) {
            return Err(invalid("rustc_commit", &self.rustc_commit));
        }
        if !printable(&self.llvm) {
            return Err(invalid("llvm", &self.llvm));
        }
        Ok(())
    }

    /// Reads the `release:`, `commit-hash:` and `LLVM version:` lines of `rustc -vV`
    /// output, and validates them.
    pub fn from_rustc_verbose_version(text: &str) -> Result<Self, ToolchainError> {
        let field = |prefix: &'static str| {
            text.lines()
                .find_map(|l| l.strip_prefix(prefix))
                .map(|v| v.trim().to_string())
                .ok_or(ToolchainError::Missing(prefix.trim_end_matches(' ')))
        };
        let toolchain = Self {
            rustc: field("release: ")?,
            rustc_commit: field("commit-hash: ")?,
            llvm: field("LLVM version: ")?,
        };
        toolchain.validate()?;
        Ok(toolchain)
    }

    /// Runs `rustc -vV` from the current directory and reads it.
    pub fn from_rustc() -> Result<Self, ToolchainError> {
        let out = Command::new("rustc")
            .arg("-vV")
            .output()
            .map_err(|e| ToolchainError::Run(e.to_string()))?;
        if !out.status.success() {
            return Err(ToolchainError::Failed(out.status.to_string()));
        }
        Self::from_rustc_verbose_version(&String::from_utf8_lossy(&out.stdout))
    }
}
