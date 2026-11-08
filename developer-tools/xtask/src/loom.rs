//! `cargo xtask loom`: the Loom models of `origin-sync` (CLAUDE.md §73) and their negative
//! controls.
//!
//! The models are ordinary tests of `origin-sync`, built with `--cfg loom` in a target
//! directory of their own, so that flag never invalidates a normal build. Each negative control
//! applies one deliberate bug to a scratch copy of the source (the working tree is never
//! touched) and requires a named model to report that bug: a model that cannot fail proves
//! nothing. A control whose text no longer matches the source stops the run, so controls
//! cannot silently stop applying.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, cargo, collect_files, repo_root, run_step, target_dir};

/// Which failure text a control may produce.
const CAUSALITY: &[&str] = &["Causality violation"];
const DEADLOCK: &[&str] = &["deadlock"];

/// One deliberate bug, and the models that must find it.
struct Control {
    name: &'static str,
    /// The file to change, relative to the repository root.
    file: &'static str,
    /// The source text to replace; it must occur exactly once.
    from: &'static str,
    to: &'static str,
    /// Each model in `origin-sync`'s `models` module that must fail, and what its failure must
    /// contain (one of these).
    models: &'static [(&'static str, &'static [&'static str])],
}

const SPIN_LOCK: &str = "framekernel/sync/src/lib.rs";
const WAIT: &str = "framekernel/sync/src/wait.rs";
const RUN_QUEUE: &str = "framekernel/src/runqueue.rs";
const MODELS: &str = "framekernel/sync/src/models.rs";

/// `lock()`'s successful exchange, before and after `lock_acquire_relaxed`.
const LOCK_EXCHANGE: &str =
    ".compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)";
const LOCK_EXCHANGE_RELAXED: &str =
    ".compare_exchange_weak(false, true, Ordering::Relaxed, Ordering::Relaxed)";
/// The guard's release of the lock.
const UNLOCK: &str = "self.lock.locked.store(false, Ordering::Release);";
const UNLOCK_RELAXED: &str = "self.lock.locked.store(false, Ordering::Relaxed);";
/// `RwLock::write`'s exchange.
const RWLOCK_WRITE_EXCHANGE: &str =
    ".compare_exchange_weak(0, WRITER, Ordering::Acquire, Ordering::Relaxed)";

const CONTROLS: &[Control] = &[
    Control {
        name: "lock_acquire_relaxed",
        file: SPIN_LOCK,
        from: LOCK_EXCHANGE,
        to: LOCK_EXCHANGE_RELAXED,
        models: &[
            ("spin_lock_two_threads_two_rounds", CAUSALITY),
            ("spin_lock_three_threads", CAUSALITY),
            ("wait_queue_waker_and_waiter_relaxed", CAUSALITY),
        ],
    },
    Control {
        name: "unlock_release_relaxed",
        file: SPIN_LOCK,
        from: UNLOCK,
        to: UNLOCK_RELAXED,
        models: &[
            ("spin_lock_two_threads_two_rounds", CAUSALITY),
            ("spin_lock_passes_writes_to_the_next_holder", CAUSALITY),
            ("wait_queue_waker_and_waiter_relaxed", CAUSALITY),
        ],
    },
    Control {
        name: "try_lock_acquire_relaxed",
        file: SPIN_LOCK,
        from: ".compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)",
        to: ".compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)",
        models: &[("try_lock_passes_writes_to_the_next_holder", CAUSALITY)],
    },
    Control {
        // The bug the comment in `try_lock` warns about.
        name: "failed_try_lock_unlocks",
        file: SPIN_LOCK,
        from: "            Err(_) => None,\n",
        to: "            Err(_) => {\n                drop(SpinGuard { lock: self });\n                None\n            }\n",
        models: &[(
            "try_lock_leaves_a_held_lock_held",
            &["Causality violation", "a lost update"],
        )],
    },
    Control {
        // Checking and then setting, in two steps: two threads can both see the lock free.
        name: "lock_not_atomic",
        file: SPIN_LOCK,
        from: "        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                prim::spin_loop();
            }
        }
",
        to: "        while self.locked.load(Ordering::Acquire) {
            prim::spin_loop();
        }
        self.locked.store(true, Ordering::Relaxed);
",
        models: &[(
            "spin_lock_two_threads_two_rounds",
            &[
                "two holders at once",
                "Causality violation",
                "a lost update",
            ],
        )],
    },
    Control {
        name: "rwlock_read_acquire_relaxed",
        file: SPIN_LOCK,
        from: ".compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)",
        to: ".compare_exchange_weak(s, s + 1, Ordering::Relaxed, Ordering::Relaxed)",
        models: &[("rwlock_writer_excludes_readers", CAUSALITY)],
    },
    Control {
        name: "rwlock_write_acquire_relaxed",
        file: SPIN_LOCK,
        from: RWLOCK_WRITE_EXCHANGE,
        to: ".compare_exchange_weak(0, WRITER, Ordering::Relaxed, Ordering::Relaxed)",
        models: &[("rwlock_writers_exclude_each_other", CAUSALITY)],
    },
    Control {
        name: "rwlock_read_unlock_release_relaxed",
        file: SPIN_LOCK,
        from: "self.lock.state.fetch_sub(1, Ordering::Release);",
        to: "self.lock.state.fetch_sub(1, Ordering::Relaxed);",
        models: &[("rwlock_writer_excludes_readers", CAUSALITY)],
    },
    Control {
        name: "rwlock_write_unlock_release_relaxed",
        file: SPIN_LOCK,
        from: "self.lock.state.store(0, Ordering::Release);",
        to: "self.lock.state.store(0, Ordering::Relaxed);",
        models: &[("rwlock_writers_exclude_each_other", CAUSALITY)],
    },
    Control {
        // A reader that does not look for a writer.
        name: "rwlock_reader_ignores_the_writer",
        file: SPIN_LOCK,
        from: "            if s != WRITER\n                && self\n",
        to: "            if self\n",
        models: &[(
            "rwlock_writer_excludes_readers",
            &[
                "a reader beside a writer",
                "a writer beside another holder",
                "Causality violation",
            ],
        )],
    },
    Control {
        // A writer that does not wait for the readers to drain.
        name: "rwlock_writer_ignores_the_readers",
        file: SPIN_LOCK,
        from: RWLOCK_WRITE_EXCHANGE,
        to: ".compare_exchange_weak(self.state.load(Ordering::Relaxed), WRITER, Ordering::Acquire, Ordering::Relaxed)",
        models: &[(
            "rwlock_writer_excludes_readers",
            &[
                "a writer beside another holder",
                "a reader beside a writer",
                "Causality violation",
            ],
        )],
    },
    Control {
        // Readers that exclude one another: neither can reach the point of having it.
        name: "rwlock_readers_exclude_each_other",
        file: SPIN_LOCK,
        from: ".compare_exchange_weak(s, s + 1, Ordering::Acquire, Ordering::Relaxed)",
        to: RWLOCK_WRITE_EXCHANGE,
        models: &[(
            "rwlock_readers_share",
            &["Model exceeded maximum number of branches", "deadlock"],
        )],
    },
    Control {
        name: "wake_one_never_wakes",
        file: WAIT,
        from: "            scheduler.wake(t);\n",
        to: "            let _ = t;\n",
        models: &[
            ("wait_queue_waker_and_waiter_release_acquire", DEADLOCK),
            ("wait_queue_ping_pong", DEADLOCK),
            ("wait_queue_producer_consumer", DEADLOCK),
            ("wait_queue_two_waiters", DEADLOCK),
        ],
    },
    Control {
        // The other half of "no lost wake-up": the run queue keeps an early one.
        name: "run_queue_forgets_an_early_wake",
        file: RUN_QUEUE,
        from: "                if let Some(p) = self.wake_pending.get_mut(slot) {
                    *p = true;
                }
                false
",
        to: "                false\n",
        models: &[("wait_queue_waker_and_waiter_release_acquire", DEADLOCK)],
    },
    Control {
        // The condition read outside the lock: it can go stale before the registration.
        name: "condition_checked_before_the_lock",
        file: WAIT,
        from: "            let registered = scheduler.without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                if condition() {
",
        to: "            let registered = scheduler.without_interrupts(|| {
                let holds = condition();
                let mut waiters = self.waiters.lock();
                if holds {
",
        models: &[("wait_queue_waker_and_waiter_relaxed", DEADLOCK)],
    },
    Control {
        // The check and the registration in separate critical sections.
        name: "registered_after_unlocking",
        file: WAIT,
        from: "                    return Some(true);
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
",
        to: "                    return Some(true);
                }
                drop(waiters);
                let mut waiters = self.waiters.lock();
                match waiters.iter_mut().find(|w| w.is_none()) {
",
        models: &[("wait_queue_waker_and_waiter_relaxed", DEADLOCK)],
    },
    Control {
        // A thread that could not register must poll: blocking would wait for ever.
        name: "full_queue_blocks_unregistered",
        file: WAIT,
        from: "                        *free = Some(thread);
                        Some(true)
                    }
                    None => Some(false),
",
        to: "                        *free = Some(thread);
                        Some(true)
                    }
                    None => Some(true),
",
        models: &[("wait_queue_full_queue_polls", DEADLOCK)],
    },
    Control {
        // The boot flow cannot block: nothing can wake it.
        name: "boot_flow_blocks",
        file: WAIT,
        from: "                let Some(thread) = me else {
                    return Some(false);
                };
",
        to: "                let Some(thread) = me else {
                    return Some(true);
                };
",
        models: &[("wait_queue_boot_flow_polls", DEADLOCK)],
    },
    Control {
        // A timed wait on the boot flow must poll; it has nothing to be woken by.
        name: "boot_flow_timed_wait_blocks",
        file: WAIT,
        from: "                let Some(thread) = me else {
                    return Step::Poll;
                };
",
        to: "                let Some(thread) = me else {
                    return Step::Block;
                };
",
        models: &[(
            "wait_queue_boot_flow_timed_wait",
            &["the boot flow must never block"],
        )],
    },
    Control {
        // A registration left behind by a timed-out wait swallows the next wake-up.
        name: "timeout_leaves_its_registration",
        file: WAIT,
        from: "                if done || scheduler.monotonic_nanos() >= deadline {
                    if let Some(slot) = waiters.iter_mut().find(|w| **w == me && me.is_some()) {
                        *slot = None;
                    }
                    return Step::Finished(done);
",
        to: "                if done || scheduler.monotonic_nanos() >= deadline {
                    return Step::Finished(done);
",
        models: &[("wait_queue_timed_wait_gives_way", DEADLOCK)],
    },
    Control {
        // A wake-up can outlive its wait; `wait_until` must re-check, not trust it.
        name: "no_recheck_after_a_block",
        file: WAIT,
        from: "                Some(true) => {
                    scheduler.block();
                    blocked += 1;
                }
",
        to: "                Some(true) => {
                    scheduler.block();
                    return blocked + 1;
                }
",
        models: &[(
            "wait_queue_stale_wake_up_is_harmless",
            &["returned with the condition false"],
        )],
    },
    Control {
        // The rule for callers: change the state first, then wake.
        name: "waker_wakes_before_setting_the_flag",
        file: MODELS,
        from: "        flag.store(true, set);
        queue.wake_one(&me);
",
        to: "        queue.wake_one(&me);
        flag.store(true, set);
",
        models: &[("wait_queue_waker_and_waiter_release_acquire", DEADLOCK)],
    },
];

/// The scratch copy's workspace: just the crate under test, with the real workspace's language
/// edition and the `cfg(loom)` declaration.
const SCRATCH_WORKSPACE: &str = r#"[workspace]
resolver = "3"
members = ["framekernel/sync"]

[workspace.package]
edition = "2024"
rust-version = "1.99.1"
license = "MIT OR Apache-2.0"
publish = false

[workspace.lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(loom)'] }

# Loom itself stays optimized. Each control is one build of this crate, which takes much longer
# than the search that finds its bug.
[profile.release.package.origin-sync]
opt-level = 0
"#;

/// `cargo` for a Loom build of the workspace rooted at `dir`.
fn loom_cargo(dir: &Path) -> Command {
    let mut c = cargo();
    c.current_dir(dir)
        .env("RUSTFLAGS", "--cfg loom")
        .env("CARGO_TARGET_DIR", target_dir().join("loom"));
    c
}

/// Runs every model, then every negative control.
pub(crate) fn run() -> Result {
    let root = repo_root();
    // The Loom configuration is code no normal build sees: it gets the same lints.
    run_step(loom_cargo(&root).args([
        "clippy",
        "--locked",
        "--package",
        "origin-sync",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    run_step(loom_cargo(&root).args([
        "test",
        "--locked",
        "--package",
        "origin-sync",
        "--release",
        "--lib",
        "models::",
        "--",
        "--nocapture",
    ]))?;
    controls(&root)
}

/// Copies the files the models need from the working tree to the scratch workspace.
fn populate(root: &Path, scratch: &Path) -> Result {
    let write = |relative: &Path, bytes: &[u8]| -> Result {
        let path = scratch.join(relative);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    };
    let copy = |relative: &Path| -> Result {
        let bytes = fs::read(root.join(relative))
            .map_err(|e| format!("{}: {e}", root.join(relative).display()))?;
        write(relative, &bytes)
    };
    write(Path::new("Cargo.toml"), SCRATCH_WORKSPACE.as_bytes())?;
    copy(Path::new("Cargo.lock"))?;
    copy(Path::new(RUN_QUEUE))?;
    let crate_dir = root.join("framekernel/sync");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(&crate_dir, &|_| false, &|_| true, &mut files)
        .map_err(|e| format!("{}: {e}", crate_dir.display()))?;
    for file in files {
        let relative = file
            .strip_prefix(root)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        copy(relative)?;
    }
    Ok(())
}

/// Applies `control` to the scratch copy; its text must occur exactly once.
fn apply(control: &Control, scratch: &Path) -> Result {
    let path = scratch.join(control.file);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let found = text.matches(control.from).count();
    if found != 1 {
        return Err(format!(
            "control {}: expected its text exactly once in {}, found it {found} times; update \
             the control to match the source",
            control.name, control.file
        ));
    }
    fs::write(&path, text.replace(control.from, control.to))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Applies each control in turn and requires each of its models to fail with the expected
/// report.
fn controls(root: &Path) -> Result {
    let scratch = target_dir().join("loom-controls");
    let mut models_caught = 0;
    for control in CONTROLS {
        populate(root, &scratch)?;
        apply(control, &scratch)?;
        // A control that does not compile would "fail" for the wrong reason.
        let build = loom_cargo(&scratch)
            .args(["test", "--offline", "--release", "--lib", "--no-run"])
            .output()
            .map_err(|e| format!("control {}: cannot run cargo: {e}", control.name))?;
        if !build.status.success() {
            return Err(format!(
                "control {}: does not compile:\n{}",
                control.name,
                String::from_utf8_lossy(&build.stderr)
            ));
        }
        for &(model, expect) in control.models {
            let test = format!("models::{model}");
            let out = loom_cargo(&scratch)
                .args([
                    "test",
                    "--offline",
                    "--release",
                    "--lib",
                    &test,
                    "--",
                    "--exact",
                    "--nocapture",
                ])
                .env_remove("ORIGIN_LOOM_DEEP")
                .output()
                .map_err(|e| format!("control {}: cannot run cargo: {e}", control.name))?;
            let report = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if out.status.success() {
                return Err(format!(
                    "control {}: model {model} did not find the bug:\n{report}",
                    control.name
                ));
            }
            let Some(found) = expect.iter().find(|e| report.contains(*e)) else {
                return Err(format!(
                    "control {}: model {model} failed, but not with {expect:?}:\n{report}",
                    control.name
                ));
            };
            println!(
                "xtask: loom control {}: caught by {model}: \"{found}\"",
                control.name
            );
            models_caught += 1;
        }
    }
    println!(
        "xtask: loom: every model passed and all {} negative controls were caught ({models_caught} \
         model runs)",
        CONTROLS.len()
    );
    Ok(())
}
