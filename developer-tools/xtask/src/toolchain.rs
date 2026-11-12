//! What keeps an image from depending on the machine that builds it (CLAUDE.md §75, §60;
//! `docs/research/2026-11-11-toolchain-path-in-images.md`).
//!
//! Two things reached the kernel's and the loader's images from outside the repository:
//!
//! - **Where the toolchain is installed.** With the `rust-src` component present, the compiler
//!   names the standard library's source by where it really is, so every panic location in
//!   library code compiled into Origin's crates held a path under the builder's home
//!   directory. [`image_config`] gives the functions that build the loader, the kernel and its
//!   libraries the flag that writes `/rustc/<commit>` there instead, which is what the compiler
//!   itself writes when it has no local copy of that source. [`userspace_config`] gives the
//!   ring-3 builds the same: the kernel's image holds the bytes of the programs it carries.
//! - **Flags, compilers and linkers in the environment.** `RUSTFLAGS` and
//!   `CARGO_ENCODED_RUSTFLAGS`, even empty, replace every flag the repository's files set: the
//!   frame pointers, the kernel's hardening options, the loader's link arguments, the ring-3
//!   programs' whole list. The per-target variables add flags the repository does not hold.
//!   Others put a program in the pinned compiler's place, or a linker in an image's.
//!   [`refuse_environment_flags`] stops `xtask` before it builds anything.
//!
//! An image built by plain `cargo build` gets neither: it holds the toolchain's path.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::ring3::USERSPACE_TARGET;
use crate::{KERNEL_TARGET, Result, UEFI_TARGET, read_file, repo_root};

/// The pinned toolchain, as the compiler itself reports it.
#[derive(Debug)]
pub(crate) struct Toolchain {
    /// `rustc --print sysroot`: the directory the toolchain is installed in.
    pub(crate) sysroot: PathBuf,
    /// The triple of the machine the compiler runs on.
    pub(crate) host: String,
    /// The commit the compiler was built from, 40 hexadecimal digits: the one the repository
    /// pins in [`PINNED_COMMIT`], which the compiler in use must report too.
    pub(crate) commit: String,
}

/// The one place the pinned compiler's commit is written (TASKS item 22a). The toolchain
/// check and CI's install step read it as well, and both fail on a compiler that reports
/// another.
const PINNED_COMMIT: &str = "developer-tools/toolchain/rustc-commit-hash";

/// Environment variables `xtask` does not run with, and what each would do. Each was seen to
/// reach an image's build with Cargo 1.99.1 (`cargo build -v`, and a program that records its
/// own use; `docs/research/2026-11-11-toolchain-path-in-images.md` §6 and §12):
///
/// - the first two replace every `target.<triple>.rustflags` entry, even when they are empty;
/// - a per-target flags variable is added in front of them, so a flag the repository's files
///   do not set gets through;
/// - a compiler or wrapper variable makes Cargo run that program for every compilation, where
///   [`toolchain`] asks the `rustc` on the path for its commit;
/// - a per-target linker variable replaces the kernel's linker and the loader's.
///
/// Not here, each for its reason. `CARGO_BUILD_RUSTFLAGS` reaches no build that has flags for
/// its target, and every image has. `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` does not
/// reach a ring-3 build, whose flags name the linker themselves, and on a Linux host it is
/// the host's own. A configuration file above the checkout or in Cargo's home can do most of
/// this too, and is not looked for: see the ledger's §12.
const REFUSED: &[(&str, &str)] = &[
    (
        "CARGO_ENCODED_RUSTFLAGS",
        "replaces every compiler flag the repository sets, even when it is empty",
    ),
    (
        "RUSTFLAGS",
        "replaces every compiler flag the repository sets, even when it is empty",
    ),
    (
        "CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS",
        "adds compiler flags to the kernel that the repository does not hold",
    ),
    (
        "CARGO_TARGET_X86_64_UNKNOWN_UEFI_RUSTFLAGS",
        "adds compiler flags to the loader that the repository does not hold",
    ),
    (
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        "adds compiler flags to the programs built for ring 3 that the repository does not hold",
    ),
    (
        "RUSTC",
        "puts another program in the pinned compiler's place",
    ),
    (
        "CARGO_BUILD_RUSTC",
        "puts another program in the pinned compiler's place",
    ),
    (
        "RUSTC_WRAPPER",
        "runs every compilation through a program the repository does not hold",
    ),
    (
        "RUSTC_WORKSPACE_WRAPPER",
        "runs every compilation through a program the repository does not hold",
    ),
    (
        "CARGO_BUILD_RUSTC_WRAPPER",
        "runs every compilation through a program the repository does not hold",
    ),
    (
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "runs every compilation through a program the repository does not hold",
    ),
    (
        "CARGO_TARGET_X86_64_UNKNOWN_NONE_LINKER",
        "puts another linker in the place of the kernel's",
    ),
    (
        "CARGO_TARGET_X86_64_UNKNOWN_UEFI_LINKER",
        "puts another linker in the place of the loader's",
    ),
];

/// Where the `rust-src` component keeps the standard library's source, under the sysroot.
const SOURCE_DIRECTORY: &str = "lib/rustlib/src/rust";

/// A path shorter than this is not looked for in an image: `/src` is in every image's own
/// text, as part of the source locations of Origin's code.
const SHORTEST_PATH: usize = 8;

/// The toolchain's facts, asked of `rustc` once in a run.
pub(crate) fn toolchain() -> Result<&'static Toolchain> {
    static TOOLCHAIN: OnceLock<std::result::Result<Toolchain, String>> = OnceLock::new();
    TOOLCHAIN
        .get_or_init(ask_rustc)
        .as_ref()
        .map_err(Clone::clone)
}

/// Runs the pinned `rustc` from the repository's root, as Cargo will.
fn ask_rustc() -> Result<Toolchain> {
    let rustc = |args: &[&str]| -> Result<String> {
        let out = Command::new("rustc")
            .args(args)
            .current_dir(repo_root())
            .output()
            .map_err(|e| format!("cannot run rustc: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "`rustc {}` failed ({}): {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        String::from_utf8(out.stdout)
            .map_err(|_| format!("`rustc {}` did not print UTF-8", args.join(" ")))
    };
    let sysroot = rustc(&["--print", "sysroot"])?.trim().to_string();
    if sysroot.is_empty() {
        return Err("`rustc --print sysroot` printed nothing".into());
    }
    let (host, reported) = parse_version(&rustc(&["-vV"])?)?;
    let pin = repo_root().join(PINNED_COMMIT);
    let text =
        String::from_utf8(read_file(&pin)?).map_err(|_| format!("{PINNED_COMMIT} is not UTF-8"))?;
    let commit = same_commit(pinned_commit(&text)?, &reported)?;
    Ok(Toolchain {
        sysroot: PathBuf::from(sysroot),
        host,
        commit,
    })
}

/// The commit [`PINNED_COMMIT`] holds, read as CI's install step reads it: everything from a
/// `#` to the end of its line is a comment, white space does not count, and what is left must
/// be 40 lower-case hexadecimal digits.
fn pinned_commit(text: &str) -> Result<String> {
    let commit: String = text
        .lines()
        .map(|line| line.split('#').next().unwrap_or_default())
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    if commit.len() == 40
        && commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(commit)
    } else {
        Err(format!(
            "{PINNED_COMMIT} does not hold one commit of 40 hexadecimal digits"
        ))
    }
}

/// `pinned`, if the compiler in use reports it. The remap writes `/rustc/<pinned>`, and the
/// library code the toolchain ships compiled already holds `/rustc/<reported>`: with another
/// compiler an image would hold both, and would not be the image the repository describes.
fn same_commit(pinned: String, reported: &str) -> Result<String> {
    if pinned == reported {
        Ok(pinned)
    } else {
        Err(format!(
            "rustc reports commit {reported}; the pinned commit is {pinned} ({PINNED_COMMIT}). Build with the toolchain rust-toolchain.toml names"
        ))
    }
}

/// The host triple and the commit from what `rustc -vV` prints. A report without a full
/// commit is refused: the remap's target would not be what another machine writes.
fn parse_version(report: &str) -> Result<(String, String)> {
    let field = |name: &str| {
        report
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_string())
            .ok_or_else(|| format!("`rustc -vV` has no `{}` line", name.trim()))
    };
    let host = field("host: ")?;
    let commit = field("commit-hash: ")?;
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "`rustc -vV` gives the commit `{commit}`, which is not 40 hexadecimal digits"
        ));
    }
    if host.is_empty() {
        return Err("`rustc -vV` gives an empty host".into());
    }
    Ok((host, commit))
}

/// `--config` arguments for a `cargo` command that builds the loader, the kernel, or a
/// library that goes into one. They add, for both freestanding targets, a remap of the
/// toolchain's source directory to `/rustc/<commit>`. Cargo joins them to the flags
/// `.cargo/config.toml` holds.
pub(crate) fn image_config() -> Result<Vec<String>> {
    config_for(&[UEFI_TARGET, KERNEL_TARGET])
}

/// The same for a ring-3 build, whose target is the userspace triple. Given after
/// `.cargo/userspace.toml`, it is joined to that file's flags. It is not part of
/// [`image_config`]: on CI the userspace triple is the host's own.
pub(crate) fn userspace_config() -> Result<Vec<String>> {
    config_for(&[USERSPACE_TARGET])
}

/// `--config target.<triple>.rustflags=[the remap]` for each of `targets`.
fn config_for(targets: &[&str]) -> Result<Vec<String>> {
    let toolchain = toolchain()?;
    let flags = remap_flags(&source_directories(&toolchain.sysroot), &toolchain.commit)?;
    Ok(entries_for(targets, &flags))
}

/// `--config target.<triple>.rustflags=<flags>` for each of `targets`, `flags` being a TOML
/// array.
fn entries_for(targets: &[&str], flags: &str) -> Vec<String> {
    targets
        .iter()
        .flat_map(|target| {
            [
                "--config".to_string(),
                format!("target.{target}.rustflags={flags}"),
            ]
        })
        .collect()
}

/// The toolchain's source directory as the compiler names it, and with every symbolic link
/// followed if that is another path: which of the two the compiler writes is its own affair.
fn source_directories(sysroot: &Path) -> Vec<PathBuf> {
    let named = sysroot.join(SOURCE_DIRECTORY);
    let mut all = vec![named.clone()];
    if let Ok(real) = named.canonicalize()
        && real != named
    {
        all.push(real);
    }
    all
}

/// A TOML array with one `--remap-path-prefix=<directory>=/rustc/<commit>` for each directory.
fn remap_flags(directories: &[PathBuf], commit: &str) -> Result<String> {
    let mut flags = Vec::new();
    for directory in directories {
        let directory = directory.to_str().ok_or_else(|| {
            format!(
                "the toolchain's directory {} is not UTF-8, so it cannot be remapped",
                directory.display()
            )
        })?;
        flags.push(toml_string(&format!(
            "--remap-path-prefix={directory}=/rustc/{commit}"
        )));
    }
    Ok(format!("[{}]", flags.join(", ")))
}

/// `text` as a TOML basic string: a directory's name may hold spaces, quotes or backslashes.
fn toml_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Fails if the environment holds compiler flags (see [`REFUSED`]).
pub(crate) fn refuse_environment_flags() -> Result {
    match refused(|name| env::var_os(name).is_some()) {
        None => Ok(()),
        Some((name, what)) => Err(format!(
            "{name} is set in the environment. It {what}, so an image built now would not be the one the repository describes (CLAUDE.md §14, §75). Unset it and run again"
        )),
    }
}

/// The first refused variable that `is_set`, with what it would do.
fn refused(is_set: impl Fn(&str) -> bool) -> Option<(&'static str, &'static str)> {
    REFUSED.iter().copied().find(|(name, _)| is_set(name))
}

/// A path of the build host that no image may hold, and what it is.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HostPath {
    pub(crate) what: &'static str,
    pub(crate) path: String,
}

/// The build host's paths that could reach an image: the toolchain's directory, Cargo's home
/// and the checkout, each as named and with symbolic links followed. A path that is not
/// UTF-8, or shorter than [`SHORTEST_PATH`], is returned in the second list: it is not looked
/// for, and the gate says so.
pub(crate) fn host_paths() -> Result<(Vec<HostPath>, Vec<HostPath>)> {
    let mut named = vec![
        ("the toolchain's directory", toolchain()?.sysroot.clone()),
        ("the checkout", repo_root()),
    ];
    let cargo_home = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")));
    if let Some(cargo_home) = cargo_home {
        named.push(("Cargo's home", cargo_home));
    }
    let mut both = Vec::new();
    for (what, path) in named {
        if let Ok(real) = path.canonicalize()
            && real != path
        {
            both.push((what, real));
        }
        both.push((what, path));
    }
    Ok(sort_host_paths(both))
}

/// Splits paths into those an image is searched for and those it cannot be.
fn sort_host_paths(paths: Vec<(&'static str, PathBuf)>) -> (Vec<HostPath>, Vec<HostPath>) {
    let mut sought = Vec::new();
    let mut skipped = Vec::new();
    for (what, path) in paths {
        let text = path.to_string_lossy().into_owned();
        let host_path = HostPath { what, path: text };
        if path.to_str().is_some() && host_path.path.len() >= SHORTEST_PATH {
            if !sought.contains(&host_path) {
                sought.push(host_path);
            }
        } else {
            skipped.push(host_path);
        }
    }
    (sought, skipped)
}

/// The first of `paths` that `image` holds, and the offset it is at.
pub(crate) fn host_path_in<'a>(
    image: &[u8],
    paths: &'a [HostPath],
) -> Option<(&'a HostPath, usize)> {
    paths.iter().find_map(|path| {
        let needle = path.path.as_bytes();
        if needle.is_empty() {
            return None;
        }
        image
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|at| (path, at))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "rustc 1.99.1 (58cf8f9c3 2026-10-13)\nbinary: rustc\ncommit-hash: 58cf8f9c35b355199727627d07ae34a4e2846d67\ncommit-date: 2026-10-13\nhost: aarch64-apple-darwin\nrelease: 1.99.1\nLLVM version: 23.1.1\n";

    #[test]
    fn the_host_and_the_commit_are_read_from_the_compilers_report() {
        assert_eq!(
            parse_version(REPORT),
            Ok((
                "aarch64-apple-darwin".to_string(),
                "58cf8f9c35b355199727627d07ae34a4e2846d67".to_string()
            ))
        );
    }

    #[test]
    fn a_report_without_a_full_commit_is_refused() {
        for (change, said) in [
            (
                REPORT.replace("58cf8f9c35b355199727627d07ae34a4e2846d67", "unknown"),
                "not 40 hexadecimal digits",
            ),
            (
                REPORT.replace("58cf8f9c35b355199727627d07ae34a4e2846d67", "58cf8f9c3"),
                "not 40 hexadecimal digits",
            ),
            (
                REPORT.replace(
                    "58cf8f9c35b355199727627d07ae34a4e2846d67",
                    "58cf8f9c35b355199727627d07ae34a4e2846d6z",
                ),
                "not 40 hexadecimal digits",
            ),
            (
                REPORT.replace("commit-hash: ", "commit: "),
                "no `commit-hash:` line",
            ),
            (REPORT.replace("host: ", "built for: "), "no `host:` line"),
            (String::new(), "no `host:` line"),
        ] {
            let error = parse_version(&change).expect_err("an incomplete report");
            assert!(error.contains(said), "{error}");
        }
    }

    #[test]
    fn the_pinned_commit_is_read_as_ci_reads_it() {
        let commit = "58cf8f9c35b355199727627d07ae34a4e2846d67";
        for text in [
            format!("{commit}\n"),
            format!("# The commit of the pinned compiler.\n# Two lines of comment.\n{commit}\n"),
            format!("  {commit}  # the 1.99.1 release\n\n"),
            format!("{}\n{}\n", &commit[..20], &commit[20..]),
        ] {
            assert_eq!(pinned_commit(&text).as_deref(), Ok(commit), "{text:?}");
        }
        for text in [
            String::new(),
            "# only a comment\n".to_string(),
            format!("{}\n", &commit[..39]),
            format!("{commit}0\n"),
            format!("{commit}\n{commit}\n"),
            format!("{}\n", commit.to_uppercase()),
            format!("#{commit}\n"),
            "unknown\n".to_string(),
        ] {
            let error = pinned_commit(&text).expect_err("not one commit");
            assert!(
                error.contains("does not hold one commit"),
                "{text:?}: {error}"
            );
        }
    }

    #[test]
    fn this_repositorys_pin_is_the_compiler_in_use() {
        let text = String::from_utf8(
            read_file(&repo_root().join(PINNED_COMMIT)).expect("the pin is in the repository"),
        )
        .expect("the pin is text");
        let pinned = pinned_commit(&text).expect("the pin holds one commit");
        assert_eq!(
            toolchain()
                .expect("the pinned toolchain is installed")
                .commit,
            pinned
        );
    }

    #[test]
    fn a_compiler_that_is_not_the_pinned_one_is_refused() {
        let pinned = "58cf8f9c35b355199727627d07ae34a4e2846d67".to_string();
        assert_eq!(same_commit(pinned.clone(), &pinned), Ok(pinned.clone()));
        let other = "0123456789abcdef0123456789abcdef01234567";
        let error = same_commit(pinned.clone(), other).expect_err("another compiler");
        assert!(error.contains(other) && error.contains(&pinned), "{error}");
        assert!(error.contains(PINNED_COMMIT), "{error}");
    }

    /// The configuration keys a list of `--config` arguments sets.
    fn keys(config: &[String]) -> Vec<&str> {
        config
            .chunks(2)
            .map(|pair| {
                assert_eq!(pair[0], "--config");
                pair[1].split('=').next().expect("a key")
            })
            .collect()
    }

    #[test]
    fn the_ring_3_builds_get_the_remap_for_their_own_triple_alone() {
        let config = userspace_config().expect("the pinned toolchain is installed");
        assert_eq!(keys(&config), ["target.x86_64-unknown-linux-gnu.rustflags"]);
        assert!(
            config[1]
                .starts_with("target.x86_64-unknown-linux-gnu.rustflags=[\"--remap-path-prefix="),
            "{}",
            config[1]
        );
        // The freestanding builds set no key of the userspace triple, which is CI's host.
        let images = image_config().expect("the pinned toolchain is installed");
        assert_eq!(
            keys(&images),
            [
                "target.x86_64-unknown-uefi.rustflags",
                "target.x86_64-unknown-none.rustflags"
            ]
        );
    }

    #[test]
    fn a_toolchain_directory_named_for_the_userspace_triple_changes_no_key() {
        // On CI the toolchain's directory holds the host's triple, which is the userspace
        // triple. The first form of the test above looked for that triple anywhere in an
        // entry, found it in the directory's name, and failed CI's check job on the batch
        // (run 43230979284, at 9e4f79f): it had only ever run on the development Mac.
        let commit = "58cf8f9c35b355199727627d07ae34a4e2846d67";
        let flags = remap_flags(
            &[PathBuf::from(
                "/home/runner/.rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu/lib/rustlib/src/rust",
            )],
            commit,
        )
        .expect("the directory is UTF-8");
        assert!(flags.contains(USERSPACE_TARGET));
        let images = entries_for(&[UEFI_TARGET, KERNEL_TARGET], &flags);
        assert_eq!(
            keys(&images),
            [
                "target.x86_64-unknown-uefi.rustflags",
                "target.x86_64-unknown-none.rustflags"
            ]
        );
        let ring3 = entries_for(&[USERSPACE_TARGET], &flags);
        assert_eq!(keys(&ring3), ["target.x86_64-unknown-linux-gnu.rustflags"]);
        // And the same on a machine whose toolchain is named for another host.
        let mac = remap_flags(
            &[PathBuf::from(
                "/Users/someone/.rustup/toolchains/1.99.1-aarch64-apple-darwin/lib/rustlib/src/rust",
            )],
            commit,
        )
        .expect("the directory is UTF-8");
        assert_eq!(
            keys(&entries_for(&[UEFI_TARGET, KERNEL_TARGET], &mac)),
            keys(&images)
        );
        assert!(entries_for(&[], &flags).is_empty());
    }

    #[test]
    fn the_remap_names_the_directory_and_the_commit() {
        let flags = remap_flags(
            &[PathBuf::from(
                "/home/someone/.rustup/toolchains/1.99.1-x/lib/rustlib/src/rust",
            )],
            "58cf8f9c35b355199727627d07ae34a4e2846d67",
        );
        assert_eq!(
            flags.as_deref(),
            Ok(
                "[\"--remap-path-prefix=/home/someone/.rustup/toolchains/1.99.1-x/lib/rustlib/src/rust=/rustc/58cf8f9c35b355199727627d07ae34a4e2846d67\"]"
            )
        );
    }

    #[test]
    fn a_directory_with_spaces_quotes_and_backslashes_stays_one_toml_string() {
        let flags = remap_flags(
            &[
                PathBuf::from("/Users/First Last/it's \"here\"\\x/src"),
                PathBuf::from("/Volumes/tab\there/src"),
            ],
            "0123456789abcdef0123456789abcdef01234567",
        )
        .expect("both are UTF-8");
        assert_eq!(
            flags,
            "[\"--remap-path-prefix=/Users/First Last/it's \\\"here\\\"\\\\x/src=/rustc/0123456789abcdef0123456789abcdef01234567\", \"--remap-path-prefix=/Volumes/tab\\u0009here/src=/rustc/0123456789abcdef0123456789abcdef01234567\"]"
        );
        // Every quote inside a string is escaped, so the array has exactly two strings.
        let unescaped = flags.replace("\\\\", "").replace("\\\"", "");
        assert_eq!(unescaped.matches('"').count(), 4, "{flags}");
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_that_is_not_utf8_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let directory = PathBuf::from(OsStr::from_bytes(b"/home/\xFF/src"));
        let error = remap_flags(&[directory], "0123456789abcdef0123456789abcdef01234567")
            .expect_err("not UTF-8");
        assert!(error.contains("is not UTF-8"), "{error}");
    }

    #[test]
    fn the_configuration_covers_both_freestanding_targets() {
        let config = image_config().expect("the pinned toolchain is installed");
        assert_eq!(config.len(), 4);
        assert_eq!(config[0], "--config");
        assert_eq!(config[2], "--config");
        let toolchain = toolchain().expect("the pinned toolchain is installed");
        for (entry, target) in [(&config[1], UEFI_TARGET), (&config[3], KERNEL_TARGET)] {
            assert!(
                entry.starts_with(&format!(
                    "target.{target}.rustflags=[\"--remap-path-prefix="
                )),
                "{entry}"
            );
            assert!(entry.contains(SOURCE_DIRECTORY), "{entry}");
            assert!(
                entry.contains(&format!("=/rustc/{}\"", toolchain.commit)),
                "{entry}"
            );
        }
    }

    #[test]
    fn each_refused_variable_is_refused_and_no_other() {
        for (name, _) in REFUSED {
            let found = refused(|asked| asked == *name).expect("a refused variable");
            assert_eq!(found.0, *name);
        }
        // Empty counts as set: an empty RUSTFLAGS still replaces the repository's flags.
        assert_eq!(
            refused(|asked| asked == "RUSTFLAGS").map(|r| r.0),
            Some("RUSTFLAGS")
        );
        assert_eq!(refused(|_| false), None);
        // Let through, each for the reason beside the list: the last is the host's own linker
        // on a Linux machine, and reaches no ring-3 build.
        for other in [
            "CARGO_BUILD_RUSTFLAGS",
            "PATH",
            "CARGO",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "RUSTUP_TOOLCHAIN",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        ] {
            assert_eq!(refused(|asked| asked == other), None, "{other}");
        }
        // The names are the ones Cargo derives from the triples: flags for all three targets
        // an image is built for, linkers for the two whose flags do not name their own.
        let variable = |target: &str, key: &str| {
            format!(
                "CARGO_TARGET_{}_{key}",
                target.to_uppercase().replace('-', "_")
            )
        };
        let is_refused = |name: &str| REFUSED.iter().any(|(refused, _)| *refused == name);
        for target in [UEFI_TARGET, KERNEL_TARGET, USERSPACE_TARGET] {
            let name = variable(target, "RUSTFLAGS");
            assert!(is_refused(&name), "{name}");
        }
        for target in [UEFI_TARGET, KERNEL_TARGET] {
            let name = variable(target, "LINKER");
            assert!(is_refused(&name), "{name}");
        }
        // No name is listed twice.
        for (i, (name, _)) in REFUSED.iter().enumerate() {
            assert!(
                REFUSED[i + 1..].iter().all(|(other, _)| other != name),
                "{name}"
            );
        }
    }

    #[test]
    fn a_host_path_planted_in_an_image_is_found_with_its_offset() {
        let paths = vec![
            HostPath {
                what: "the toolchain's directory",
                path: "/Users/someone/.rustup/toolchains/1.99.1-aarch64-apple-darwin".into(),
            },
            HostPath {
                what: "the checkout",
                path: "/Users/someone/work/origin-os".into(),
            },
        ];
        let mut image = vec![0x90u8; 4096];
        assert_eq!(host_path_in(&image, &paths), None);
        // The text every image holds of its own source is not a host path.
        image[100..125].copy_from_slice(b"framekernel/src/memory.rs");
        image[200..246].copy_from_slice(b"/rustc/58cf8f9c35b355199727627d07ae34a4e2846d6");
        assert_eq!(host_path_in(&image, &paths), None);
        // A panic location in library code, as the images held before the remap.
        let location = b"self.len()/Users/someone/.rustup/toolchains/1.99.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/slice/mod.rs";
        image[1000..1000 + location.len()].copy_from_slice(location);
        assert_eq!(host_path_in(&image, &paths), Some((&paths[0], 1010)));
        // The checkout too, at the very end of the image.
        let mut image = vec![0u8; 512];
        let checkout = paths[1].path.as_bytes();
        let at = image.len() - checkout.len();
        image[at..].copy_from_slice(checkout);
        assert_eq!(host_path_in(&image, &paths), Some((&paths[1], at)));
        // One byte short of the path is not the path.
        image[at + checkout.len() - 1] = b'X';
        assert_eq!(host_path_in(&image, &paths), None);
        assert_eq!(host_path_in(&[], &paths), None);
    }

    #[test]
    fn a_path_too_short_to_tell_from_an_images_own_text_is_set_aside() {
        let (sought, skipped) = sort_host_paths(vec![
            ("the checkout", PathBuf::from("/src")),
            ("the toolchain's directory", PathBuf::from("/opt/rust")),
            ("Cargo's home", PathBuf::from("/opt/rust")),
            ("Cargo's home", PathBuf::from("/root/.cargo")),
        ]);
        assert_eq!(
            skipped,
            vec![HostPath {
                what: "the checkout",
                path: "/src".into()
            }]
        );
        // A path with two names is looked for under each.
        assert_eq!(sought.len(), 3);
        assert!(sought.iter().all(|path| path.path.len() >= SHORTEST_PATH));
    }

    #[test]
    fn this_machines_paths_are_long_enough_to_look_for() {
        let (sought, _) = host_paths().expect("the pinned toolchain is installed");
        assert!(
            sought
                .iter()
                .any(|path| path.what == "the toolchain's directory"),
            "{sought:?}"
        );
        assert!(
            sought.iter().any(|path| path.what == "the checkout"),
            "{sought:?}"
        );
    }
}
