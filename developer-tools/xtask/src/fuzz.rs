//! `cargo xtask fuzz`: coverage-guided fuzzing on stable Rust (owner item 359; TASKS item 144;
//! CLAUDE.md §72).
//!
//! The targets are in `tests/fuzz`, a workspace of its own (`tests/fuzz/Cargo.toml`), built by
//! cargo-fuzz with `--sanitizer none`, which needs no nightly compiler
//! (`docs/toolchain/VERIFICATION.md`).
//!
//! **Where fuzzing runs** (the coordinating session's ruling of 2026-11-26):
//!
//! - The fuzzing runs themselves run in CI: each target for a fixed time, in a job of its own
//!   (`.github/workflows/ci.yml`, job `fuzz`). They are not part of `check`.
//! - On the development machine, only what developing the harnesses needs: short trial runs and
//!   replays, each in a turn on the machine. A run longer than [`LOCAL_LIMIT`] seconds is
//!   refused unless `CI` is set in the environment, as GitHub sets it.
//! - A crash, once minimized, becomes an ordinary host test in the crate it crashed, so `check`
//!   replays it in seconds on stable Rust, without libFuzzer.
//!
//! **What a run does:** it checks that the repository's workspace resolves no fuzzing crate
//! (`libfuzzer-sys` and `arbitrary` are host-only and stay in `tests/fuzz/Cargo.lock`; CLAUDE.md
//! §5), that the seeds are within their caps, and that cargo-fuzz is the recorded release; it
//! fetches the fuzz workspace's crates against its lock file, runs the harness's own tests
//! (what each writer writes reaches acceptance), runs the control [`CONTROL`], which must be
//! found, and then the target for the time asked, from a scratch corpus seeded from
//! `tests/fuzz/seeds/<target>`. A crash or a timeout is minimized, printed as hex, and fails
//! the run: it is a finding, for the coordinating session.
//!
//! **The seeds** are written by the harness's own writers (`tests/fuzz/harness/examples/seeds.rs`,
//! run by hand: `cargo run --manifest-path tests/fuzz/Cargo.toml -p origin-fuzz-harness
//! --example seeds -- tests/fuzz/seeds`), and committed.
//!
//! **What it cannot show:** fuzzing finds bugs and proves nothing (matrix row S73-12); a run
//! that finds nothing says only that the inputs it reached did not fail. Without a sanitizer,
//! memory errors inside `std` and `alloc` go unseen; every fuzzed crate forbids `unsafe`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, cargo, repo_root, run_step, target_dir};

/// The cargo-fuzz release recorded in `docs/toolchain/VERIFICATION.md` and the provenance file.
const CARGO_FUZZ: &str = "cargo-fuzz 0.13.2";
/// The fuzz workspace, from the repository's root.
const FUZZ_DIR: &str = "tests/fuzz";
/// The committed seeds, one directory per target.
const SEEDS_DIR: &str = "tests/fuzz/seeds";
/// The harness library, whose own tests run before any fuzzing.
const HARNESS_MANIFEST: &str = "tests/fuzz/harness/Cargo.toml";
/// A run's length when none is asked for.
const DEFAULT_SECONDS: u64 = 60;
/// The longest run allowed without `CI` set: minutes, on the development machine.
const LOCAL_LIMIT: u64 = 300;
/// The control: a planted crash behind a four-byte magic, which only coverage guidance finds in
/// a short run (`tests/fuzz/targets/smoke_finds.rs`).
const CONTROL: &str = "smoke_finds";
/// The bytes the control's crash starts with.
const CONTROL_MAGIC: &[u8; 4] = b"ORGN";
/// The longest the control may take; it is found in under a second on the development machine.
const CONTROL_SECONDS: u64 = 120;
/// libFuzzer's per-input time limit (the ruling on Q7): an input that runs longer is a finding.
const TIMEOUT_SECONDS: u64 = 10;
/// libFuzzer's memory limit, polled: a run that grows past it is a finding.
const RSS_LIMIT_MB: u64 = 2048;
/// The seeds' caps (the ruling on Q9): per target, and in all.
const SEEDS_PER_TARGET: u64 = 1 << 20;
const SEEDS_TOTAL: u64 = 4 << 20;
/// The crates that may appear only in the fuzz workspace's lock file (CLAUDE.md §5).
const HOST_ONLY: [&str; 2] = ["libfuzzer-sys", "arbitrary"];
/// The names of what libFuzzer writes for a failing input.
const FINDINGS: [&str; 4] = ["crash-", "timeout-", "oom-", "leak-"];

/// A fuzz target: its name, its largest input, and its dictionary, if any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    name: &'static str,
    max_len: usize,
    dict: Option<&'static str>,
}

/// Every target but the smoke targets, in the order CI lists them.
const TARGETS: [Target; 9] = [
    target("onbf_image", 128 * 1024),
    target("onbf_image_built", 64 * 1024),
    target("loader_place", 128 * 1024),
    target("ocbor", 64 * 1024),
    target("ocbor_built", 16 * 1024),
    target("signature", 64 * 1024),
    target("signature_built", 64 * 1024),
    Target {
        name: "aml",
        max_len: 64 * 1024,
        dict: Some("dicts/aml.dict"),
    },
    target("aml_built", 16 * 1024),
];

const fn target(name: &'static str, max_len: usize) -> Target {
    Target {
        name,
        max_len,
        dict: None,
    }
}

/// What a person asked for.
#[derive(Debug, PartialEq, Eq)]
enum Request {
    /// The targets, one per line.
    List,
    /// One target for a fixed time.
    Run { target: Target, seconds: u64 },
    /// One input, run once, to reproduce a finding.
    Replay { target: Target, input: PathBuf },
}

const USAGE: &str =
    "usage: cargo xtask fuzz list | <target> [--seconds N] | replay <target> <file>";

/// Reads the command's arguments. `in_ci` lifts the local limit on a run's length.
fn parse(args: &[&str], in_ci: bool) -> Result<Request> {
    let find = |name: &str| {
        TARGETS
            .iter()
            .copied()
            .find(|t| t.name == name)
            .ok_or_else(|| format!("fuzz: no target `{name}` (`cargo xtask fuzz list`)"))
    };
    match args {
        ["list"] => Ok(Request::List),
        ["replay", name, input] => Ok(Request::Replay {
            target: find(name)?,
            input: PathBuf::from(input),
        }),
        [name] => Ok(Request::Run {
            target: find(name)?,
            seconds: DEFAULT_SECONDS,
        }),
        [name, "--seconds", n] => {
            let seconds: u64 = n.parse().ok().filter(|&s| s > 0).ok_or_else(|| {
                format!("fuzz: --seconds wants a whole number above 0, not `{n}`")
            })?;
            if seconds > LOCAL_LIMIT && !in_ci {
                return Err(format!(
                    "fuzz: {seconds} s is a CI run; on the development machine a run lasts at \
                     most {LOCAL_LIMIT} s (the coordinating session's ruling: fuzzing runs in CI, \
                     and locally only short trials)"
                ));
            }
            Ok(Request::Run {
                target: find(name)?,
                seconds,
            })
        }
        _ => Err(USAGE.to_string()),
    }
}

/// `cargo xtask fuzz …`, in a turn on the machine (the caller takes it).
pub(crate) fn run(args: &[&str]) -> Result {
    let request = parse(args, std::env::var_os("CI").is_some())?;
    if request == Request::List {
        for t in TARGETS {
            println!("{}", t.name);
        }
        return Ok(());
    }
    let root = repo_root();
    check_isolation(
        &fs::read_to_string(root.join("Cargo.lock"))
            .map_err(|e| format!("fuzz: Cargo.lock: {e}"))?,
    )?;
    check_seeds(&seed_sizes(&root)?)?;
    check_version()?;
    let fuzz_manifest = root.join(FUZZ_DIR).join("Cargo.toml");
    run_step(
        cargo()
            .args(["fetch", "--locked", "--manifest-path"])
            .arg(&fuzz_manifest),
    )?;
    run_step(offline(cargo()).args([
        "test",
        "--locked",
        "--manifest-path",
        HARNESS_MANIFEST,
        "--",
        "--nocapture",
    ]))?;
    match request {
        Request::List => Ok(()),
        Request::Run { target, seconds } => {
            control(&root)?;
            fuzz(&root, target, seconds)
        }
        Request::Replay { target, input } => {
            let status = libfuzzer(&root, target.name)
                .arg(&input)
                .status()
                .map_err(|e| format!("fuzz: cannot start cargo fuzz: {e}"))?;
            if status.success() {
                println!(
                    "xtask: fuzz: {} ran {} without failing",
                    target.name,
                    input.display()
                );
                Ok(())
            } else {
                Err(format!(
                    "fuzz: {} fails on {}",
                    target.name,
                    input.display()
                ))
            }
        }
    }
}

/// `cargo xtask check`'s step (the coordinating session's ruling on Q16): the fuzz workspace
/// type-checked as cargo-fuzz builds it, with `--cfg fuzzing`, every target of both its crates,
/// without the coverage pass and without running anything. A harness that drifts from a
/// parser's interface then fails `check`, and not only CI's fuzz jobs. Its own target directory
/// keeps it from rebuilding anything of the workspace's.
pub(crate) fn type_check() -> Result {
    run_step(
        cargo()
            .args([
                "check",
                "--locked",
                "--workspace",
                "--all-targets",
                "--manifest-path",
            ])
            .arg(repo_root().join(FUZZ_DIR).join("Cargo.toml"))
            .env("RUSTFLAGS", "--cfg fuzzing")
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("CARGO_TARGET_DIR", target_dir().join("fuzz-check")),
    )
}

/// Cargo with the network off: after `fetch --locked`, every crate comes from the lock file.
fn offline(mut c: Command) -> Command {
    c.env("CARGO_NET_OFFLINE", "true");
    c
}

/// `cargo fuzz run` for `target` in the fuzz workspace, without a sanitizer, offline; what
/// follows is the corpus or the input, then libFuzzer's options after a `--`. It starts the fuzz
/// target, a test program of first-party libraries: one of the two places
/// `host_tools::FUZZ_RUNS` lists, by decision item 365, which also asks that `fuzz` and `run`
/// stand together in one list.
fn libfuzzer(root: &Path, target: &str) -> Command {
    let mut c = offline(cargo());
    c.args(["fuzz", "run", "--fuzz-dir"])
        .arg(root.join(FUZZ_DIR))
        .args(["--sanitizer", "none", target]);
    c
}

/// libFuzzer's options for a fixed-time run of `target`.
fn options(target: Target, root: &Path, seconds: u64, artifacts: &Path) -> Vec<String> {
    let mut o = vec![
        format!("-max_total_time={seconds}"),
        format!("-timeout={TIMEOUT_SECONDS}"),
        format!("-rss_limit_mb={RSS_LIMIT_MB}"),
        format!("-max_len={}", target.max_len),
        format!("-artifact_prefix={}/", artifacts.display()),
        "-print_final_stats=1".to_string(),
    ];
    if let Some(dict) = target.dict {
        o.push(format!(
            "-dict={}",
            root.join(FUZZ_DIR).join(dict).display()
        ));
    }
    o
}

/// A fresh scratch directory under the target directory.
fn fresh(dir: &Path) -> Result {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("fuzz: {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(dir).map_err(|e| format!("fuzz: {}: {e}", dir.display()))
}

/// The files of `dir` whose names begin as libFuzzer names a failing input.
fn findings(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| FINDINGS.iter().any(|f| n.starts_with(f)))
        })
        .collect();
    found.sort();
    found
}

/// The control: [`CONTROL`] must find its planted crash within [`CONTROL_SECONDS`], or this
/// build's coverage instrumentation is not working and no run of it can be trusted.
fn control(root: &Path) -> Result {
    let work = target_dir().join("fuzz").join(CONTROL);
    let corpus = work.join("corpus");
    let artifacts = work.join("artifacts");
    fresh(&corpus)?;
    fresh(&artifacts)?;
    println!("xtask: fuzz: the control {CONTROL} must find its planted crash");
    let status = libfuzzer(root, CONTROL)
        .arg(&corpus)
        .arg("--")
        .arg(format!("-max_total_time={CONTROL_SECONDS}"))
        .arg("-seed=1")
        .arg(format!("-artifact_prefix={}/", artifacts.display()))
        .status()
        .map_err(|e| format!("fuzz: cannot start cargo fuzz: {e}"))?;
    let found = findings(&artifacts)
        .iter()
        .any(|p| fs::read(p).is_ok_and(|b| b.starts_with(CONTROL_MAGIC)));
    if !status.success() && found {
        println!("xtask: fuzz: the control's crash was found and kept: the instrumentation works");
        Ok(())
    } else {
        Err(format!(
            "fuzz: the control {CONTROL} did not find its planted crash in {CONTROL_SECONDS} s \
             (exit {status}): this build's coverage guidance is not working, so no run of it can \
             be trusted"
        ))
    }
}

/// One fixed-time run of `target`, from a scratch corpus seeded from its committed seeds.
fn fuzz(root: &Path, target: Target, seconds: u64) -> Result {
    let work = target_dir().join("fuzz").join(target.name);
    let corpus = work.join("corpus");
    let artifacts = work.join("artifacts");
    fresh(&corpus)?;
    fresh(&artifacts)?;
    let seeds = root.join(SEEDS_DIR).join(target.name);
    let mut seeded = 0;
    for entry in fs::read_dir(&seeds).into_iter().flatten().flatten() {
        let from = entry.path();
        if from.is_file() {
            fs::copy(&from, corpus.join(entry.file_name()))
                .map_err(|e| format!("fuzz: {}: {e}", from.display()))?;
            seeded += 1;
        }
    }
    println!(
        "xtask: fuzz: {} for {seconds} s from {seeded} seeds ({})",
        target.name,
        seeds.display()
    );
    let status = libfuzzer(root, target.name)
        .arg(&corpus)
        .arg("--")
        .args(options(target, root, seconds, &artifacts))
        .status()
        .map_err(|e| format!("fuzz: cannot start cargo fuzz: {e}"))?;
    let found = findings(&artifacts);
    if status.success() && found.is_empty() {
        println!(
            "xtask: fuzz: {} ran {seconds} s and found nothing",
            target.name
        );
        return Ok(());
    }
    let mut report = Vec::new();
    for input in &found {
        report.push(minimize(root, target, input));
    }
    Err(format!(
        "fuzz: FINDING in {} (exit {status}). It goes to the coordinating session before any \
         fix; a parser's is a security finding. Inputs kept in {}:\n{}",
        target.name,
        artifacts.display(),
        report.join("\n")
    ))
}

/// Minimizes a failing input with `cargo fuzz tmin` and describes the smallest form found, in
/// hexadecimal, for the regression test it becomes (the ruling on Q8). `tmin` starts the fuzz
/// target: the other place `host_tools::FUZZ_RUNS` lists (decision item 365).
fn minimize(root: &Path, target: Target, input: &Path) -> String {
    let mut c = offline(cargo());
    c.args(["fuzz", "tmin", "--fuzz-dir"])
        .arg(root.join(FUZZ_DIR))
        .args(["--sanitizer", "none", "--runs", "2000", target.name])
        .arg(input);
    let _ = c.status();
    let dir = input.parent().unwrap_or(root);
    let smallest = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("minimized-from-"))
        })
        .chain(std::iter::once(input.to_path_buf()))
        .filter_map(|p| fs::read(&p).ok().map(|b| (b, p)))
        .min_by_key(|(b, _)| b.len());
    match smallest {
        Some((bytes, path)) => format!(
            "  {} ({} bytes): {}",
            path.display(),
            bytes.len(),
            hex(&bytes[..bytes.len().min(4096)])
        ),
        None => format!("  {} (unreadable)", input.display()),
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Fails if cargo-fuzz is missing or is not the recorded release.
fn check_version() -> Result {
    let out = cargo()
        .args(["fuzz", "--version"])
        .output()
        .map_err(|e| format!("fuzz: cannot start cargo: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if version_ok(&text) {
        Ok(())
    } else {
        Err(format!(
            "fuzz: cargo-fuzz is `{}`, and {CARGO_FUZZ} is the release recorded for TASKS item \
             144: `cargo install --locked cargo-fuzz --version 0.13.2`",
            text.trim()
        ))
    }
}

fn version_ok(text: &str) -> bool {
    text.trim() == CARGO_FUZZ
}

/// Fails if the repository's workspace resolves a crate only the fuzz workspace may take.
fn check_isolation(root_lock: &str) -> Result {
    let named: Vec<&str> = root_lock
        .lines()
        .filter_map(|l| l.trim().strip_prefix("name = \""))
        .filter_map(|l| l.strip_suffix('"'))
        .filter(|n| HOST_ONLY.contains(n))
        .collect();
    if named.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fuzz: the repository's Cargo.lock resolves {}, which only tests/fuzz may take \
             (CLAUDE.md §5: host-only, and nothing that ships depends on them)",
            named.join(" and ")
        ))
    }
}

/// The size of each target's committed seeds, by target directory name.
fn seed_sizes(root: &Path) -> Result<Vec<(String, u64)>> {
    let mut sizes = Vec::new();
    let dir = root.join(SEEDS_DIR);
    for entry in fs::read_dir(&dir).map_err(|e| format!("fuzz: {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("fuzz: {}: {e}", dir.display()))?;
        let mut total = 0u64;
        for file in fs::read_dir(entry.path()).into_iter().flatten().flatten() {
            total += file.metadata().map(|m| m.len()).unwrap_or(0);
        }
        sizes.push((entry.file_name().to_string_lossy().into_owned(), total));
    }
    sizes.sort();
    Ok(sizes)
}

/// Fails if a target's seeds pass [`SEEDS_PER_TARGET`], all of them [`SEEDS_TOTAL`], or a
/// directory names no target.
fn check_seeds(sizes: &[(String, u64)]) -> Result {
    let mut total = 0u64;
    for (name, size) in sizes {
        if !TARGETS.iter().any(|t| t.name == name) {
            return Err(format!("fuzz: {SEEDS_DIR}/{name} is not a target's seeds"));
        }
        if *size > SEEDS_PER_TARGET {
            return Err(format!(
                "fuzz: {SEEDS_DIR}/{name} holds {size} bytes, past the cap of {SEEDS_PER_TARGET}"
            ));
        }
        total += size;
    }
    if total > SEEDS_TOTAL {
        return Err(format!(
            "fuzz: the seeds hold {total} bytes in all, past the cap of {SEEDS_TOTAL}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_read_and_a_long_local_run_refused() {
        let onbf = TARGETS[0];
        assert_eq!(parse(&["list"], false), Ok(Request::List));
        assert_eq!(
            parse(&["onbf_image"], false),
            Ok(Request::Run {
                target: onbf,
                seconds: DEFAULT_SECONDS
            })
        );
        assert_eq!(
            parse(&["onbf_image", "--seconds", "300"], false),
            Ok(Request::Run {
                target: onbf,
                seconds: 300
            })
        );
        assert!(parse(&["onbf_image", "--seconds", "301"], false).is_err());
        assert_eq!(
            parse(&["onbf_image", "--seconds", "600"], true),
            Ok(Request::Run {
                target: onbf,
                seconds: 600
            })
        );
        assert!(parse(&["onbf_image", "--seconds", "0"], true).is_err());
        assert!(parse(&["onbf_image", "--seconds", "ten"], true).is_err());
        assert!(parse(&["no_such_target"], false).is_err());
        assert!(
            parse(&[CONTROL], false).is_err(),
            "the control is not a target to fuzz"
        );
        assert_eq!(
            parse(&["replay", "aml", "x/crash-1"], false),
            Ok(Request::Replay {
                target: TARGETS[7],
                input: PathBuf::from("x/crash-1")
            })
        );
        assert!(parse(&[], false).is_err());
    }

    #[test]
    fn the_recorded_release_is_the_only_one_taken() {
        assert!(version_ok("cargo-fuzz 0.13.2\n"));
        assert!(!version_ok("cargo-fuzz 0.13.3\n"));
        assert!(!version_ok("error: no such command: `fuzz`"));
    }

    #[test]
    fn the_workspace_resolves_no_fuzzing_crate() {
        let lock = fs::read_to_string(repo_root().join("Cargo.lock")).unwrap();
        assert_eq!(check_isolation(&lock), Ok(()));
        // Planted: the same lock with libfuzzer-sys in it is refused.
        let planted =
            format!("{lock}\n[[package]]\nname = \"libfuzzer-sys\"\nversion = \"0.4.13\"\n");
        assert!(check_isolation(&planted).is_err());
        let planted = format!("{lock}\n[[package]]\nname = \"arbitrary\"\nversion = \"1.5.0\"\n");
        assert!(check_isolation(&planted).is_err());
    }

    #[test]
    fn the_fuzz_workspace_is_its_own_and_lists_every_target() {
        let root = repo_root();
        let workspace = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let members = workspace
            .split("\nmembers = [")
            .nth(1)
            .and_then(|m| m.split(']').next())
            .unwrap();
        assert!(
            !members.contains("tests/fuzz"),
            "the repository's workspace must not list the fuzz crates"
        );
        assert!(
            workspace.contains("\nexclude = [\"tests/fuzz\"]"),
            "the repository's workspace excludes the fuzz workspace by name"
        );
        let fuzz = fs::read_to_string(root.join(FUZZ_DIR).join("Cargo.toml")).unwrap();
        assert!(
            fuzz.contains("\n[workspace]\n"),
            "tests/fuzz is a workspace of its own"
        );
        for t in TARGETS {
            assert!(
                fuzz.contains(&format!(
                    "name = \"{}\"\npath = \"targets/{}.rs\"",
                    t.name, t.name
                )),
                "tests/fuzz/Cargo.toml has no [[bin]] for {}",
                t.name
            );
            assert!(
                root.join(FUZZ_DIR)
                    .join("targets")
                    .join(format!("{}.rs", t.name))
                    .is_file()
            );
            if let Some(dict) = t.dict {
                assert!(
                    root.join(FUZZ_DIR).join(dict).is_file(),
                    "{dict} is missing"
                );
            }
        }
        // Every [[bin]] but the smoke targets is a target here.
        for line in fuzz.lines().filter_map(|l| l.strip_prefix("name = \"")) {
            let name = line.trim_end_matches('"');
            if name.starts_with("origin-fuzz") || name.starts_with("smoke_") {
                continue;
            }
            assert!(
                TARGETS.iter().any(|t| t.name == name),
                "{name} is not in TARGETS"
            );
        }
    }

    #[test]
    fn the_seeds_are_within_their_caps() {
        let sizes = seed_sizes(&repo_root()).unwrap();
        assert_eq!(check_seeds(&sizes), Ok(()));
        assert!(!sizes.is_empty(), "no seeds are committed");
        // Planted: one target past its cap, all past theirs, and a directory of no target.
        assert!(check_seeds(&[("aml".into(), SEEDS_PER_TARGET + 1)]).is_err());
        let many: Vec<(String, u64)> = TARGETS
            .iter()
            .map(|t| (t.name.to_string(), SEEDS_PER_TARGET))
            .collect();
        assert!(check_seeds(&many).is_err());
        assert!(check_seeds(&[("elsewhere".into(), 1)]).is_err());
    }

    #[test]
    fn a_run_s_options_carry_the_limits() {
        let o = options(TARGETS[7], Path::new("/r"), 180, Path::new("/a"));
        assert!(o.contains(&"-max_total_time=180".to_string()));
        assert!(o.contains(&format!("-timeout={TIMEOUT_SECONDS}")));
        assert!(o.contains(&format!("-rss_limit_mb={RSS_LIMIT_MB}")));
        assert!(o.contains(&"-artifact_prefix=/a/".to_string()));
        assert!(
            o.iter()
                .any(|x| x.starts_with("-dict=") && x.ends_with("aml.dict"))
        );
        assert!(
            options(TARGETS[0], Path::new("/r"), 1, Path::new("/a"))
                .iter()
                .all(|x| !x.starts_with("-dict="))
        );
    }

    #[test]
    fn only_libfuzzer_s_failure_files_are_findings() {
        let dir = target_dir().join("xtask").join("fuzz-findings-test");
        fresh(&dir).unwrap();
        for name in [
            "crash-1",
            "timeout-2",
            "oom-3",
            "leak-4",
            "minimized-from-1",
            "slow-unit-5",
        ] {
            fs::write(dir.join(name), b"x").unwrap();
        }
        let found: Vec<String> = findings(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(found, ["crash-1", "leak-4", "oom-3", "timeout-2"]);
    }
}
