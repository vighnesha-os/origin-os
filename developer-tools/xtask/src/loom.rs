//! `cargo xtask loom`: the Loom models of `origin-sync` (CLAUDE.md §73) and their negative
//! controls.
//!
//! Loom stays out of the kernel's workspace: neither its crates nor the registry that serves
//! them are an input to building the kernel (CLAUDE.md §5, §60, §75). The models are ordinary
//! tests of `origin-sync`, built with `--cfg loom` in a scratch workspace of their own under the
//! target directory: a verbatim copy of the source the models need, which alone names `loom`
//! (`LOOM_DEV_DEPENDENCY`, an exact version) and which is locked by `loom.lock`, committed
//! beside this file and pinned crate by crate by `cargo xtask check`. Cargo verifies every
//! crate it downloads for that workspace against the lock's checksum.
//!
//! Each negative control applies one deliberate bug to that scratch copy (the working tree is
//! never touched) and requires a named model to report that bug: a model that cannot fail proves
//! nothing. A control whose text no longer matches the source stops the run, so controls
//! cannot silently stop applying. So does a model that does not report its search, or that no
//! control can break.
//!
//! How far the models search is decided here and in `models.rs`, not by what happens to be
//! exported: `loom_cargo` takes every `LOOM_*` and `ORIGIN_LOOM_*` variable out of their
//! environment, so `cargo xtask check` always runs the same search. On `cargo xtask loom`
//! alone, `ORIGIN_LOOM_DEEP` selects each model's larger bound, and `ORIGIN_LOOM_EXPERIMENT`
//! lets those variables through for an experiment, which proves nothing and runs no control.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Result, cargo, check_model_lock, collect_files, repo_root, run_step, target_dir};

/// The lock file of the model-checking workspace, committed so that what runs is reviewable.
/// Its packages are pinned by `APPROVED_THIRD_PARTY` in `main.rs`: changing it is a new approval.
pub(crate) const MODEL_LOCK: &str = include_str!("loom.lock");

/// What the scratch copy's `origin-sync` adds to its manifest; the kernel's copy has no such
/// table. The version must be the one `loom.lock` locks.
const LOOM_DEV_DEPENDENCY: &str = "\n[dev-dependencies]\nloom = \"=0.7.2\"\n";

/// The scratch workspace's directory, under the target directory.
const SCRATCH: &str = "loom-scratch";

/// Asks for each model's larger bound, on `cargo xtask loom` only (`models.rs` reads it).
const DEEP: &str = "ORIGIN_LOOM_DEEP";
/// Lets the environment's search settings reach the models, on `cargo xtask loom` only.
const EXPERIMENT: &str = "ORIGIN_LOOM_EXPERIMENT";

/// How far the models search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Search {
    /// Each model's ordinary bound, whatever is exported. All `cargo xtask check` ever runs.
    Gate,
    /// Each model's larger bound. A long run.
    Deep,
    /// Whatever `LOOM_*` and `ORIGIN_LOOM_*` in the environment say. Proves nothing.
    Experiment,
}

impl Search {
    /// What this run of the tool was asked for: `cargo xtask check` is a gate whatever is
    /// exported, and only `cargo xtask loom` can be asked for anything else.
    fn requested() -> Self {
        if env::args().nth(1).as_deref() != Some("loom") {
            Self::Gate
        } else if env::var_os(EXPERIMENT).is_some() {
            Self::Experiment
        } else if env::var_os(DEEP).is_some() {
            Self::Deep
        } else {
            Self::Gate
        }
    }
}

/// Whether `name` is a variable that can change what a model searches: Loom's own (its
/// preemption bound, permutation and time limits, checkpoint file, ...) and `models.rs`'s.
fn is_search_setting(name: &OsString) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with("LOOM_") || name.starts_with("ORIGIN_LOOM_"))
}

/// The variables in this process's environment that `is_search_setting` matches.
fn search_settings() -> Vec<OsString> {
    env::vars_os()
        .map(|(name, _)| name)
        .filter(is_search_setting)
        .collect()
}

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

/// `WaitQueue::take_all`'s one critical section.
const TAKE_ALL: &str = "        scheduler.without_interrupts(|| core::mem::replace(&mut *self.waiters.lock(), [None; N]))\n";

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
            ("wait_queue_timed_waiter_and_waker_relaxed", DEADLOCK),
            ("wait_queue_same_cpu_wake_resumes_on_a_tick", DEADLOCK),
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
        models: &[
            ("wait_queue_waker_and_waiter_release_acquire", DEADLOCK),
            ("wait_queue_timed_waiter_and_waker_release_acquire", DEADLOCK),
            ("wait_queue_same_cpu_wake_resumes_on_a_tick", DEADLOCK),
            ("wait_queue_same_cpu_timed_wake_resumes_on_a_tick", DEADLOCK),
        ],
    },
    Control {
        // The run queue reports a waiter it did not make ready: nothing can ever run it.
        name: "run_queue_wake_readies_nobody",
        file: RUN_QUEUE,
        from: "                if let Some(s) = self.state.get_mut(slot) {
                    *s = State::Ready;
                }
                self.admit(slot, floor);
                true
",
        to: "                self.admit(slot, floor);
                true
",
        models: &[
            ("wait_queue_waker_and_waiter_relaxed", DEADLOCK),
            ("wait_queue_same_cpu_wake_resumes_on_a_tick", DEADLOCK),
        ],
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
        models: &[
            ("wait_queue_waker_and_waiter_relaxed", DEADLOCK),
            ("wait_queue_wake_all_wakes_every_waiter", DEADLOCK),
        ],
    },
    Control {
        // As above, for a timed wait; the deadline never comes, so only a wake-up can end it.
        name: "timed_condition_checked_before_the_lock",
        file: WAIT,
        from: "            let step = scheduler.without_interrupts(|| {
                let mut waiters = self.waiters.lock();
                let done = condition();
",
        to: "            let step = scheduler.without_interrupts(|| {
                let done = condition();
                let mut waiters = self.waiters.lock();
",
        models: &[("wait_queue_timed_waiter_and_waker_relaxed", DEADLOCK)],
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
        models: &[
            ("wait_queue_waker_and_waiter_relaxed", DEADLOCK),
            ("wait_queue_wake_all_wakes_every_waiter", DEADLOCK),
        ],
    },
    Control {
        // As above, for a timed wait.
        name: "timed_registered_after_unlocking",
        file: WAIT,
        from: "                    return Step::Block;
                }
                match waiters.iter_mut().find(|w| w.is_none()) {
",
        to: "                    return Step::Block;
                }
                drop(waiters);
                let mut waiters = self.waiters.lock();
                match waiters.iter_mut().find(|w| w.is_none()) {
",
        models: &[("wait_queue_timed_waiter_and_waker_relaxed", DEADLOCK)],
    },
    Control {
        // A thread that blocks without registering: no waker can name it.
        name: "no_registration",
        file: WAIT,
        from: "                    Some(free) => {
                        *free = Some(thread);
                        Some(true)
                    }
                    None => Some(false),
",
        to: "                    Some(_) => Some(true),
                    None => Some(false),
",
        models: &[("wait_queue_waker_and_waiter_relaxed", DEADLOCK)],
    },
    Control {
        // As above, for a timed wait.
        name: "timed_no_registration",
        file: WAIT,
        from: "                    Some(free) => {
                        *free = Some(thread);
                        Step::Block
                    }
",
        to: "                    Some(_) => Step::Block,
",
        models: &[("wait_queue_timed_waiter_and_waker_relaxed", DEADLOCK)],
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
        // As above, for a timed wait.
        name: "timed_no_recheck_after_a_block",
        file: WAIT,
        from: "                Step::Block => {
                    scheduler.block_until(deadline);
                    blocked += 1;
                }
",
        to: "                Step::Block => {
                    scheduler.block_until(deadline);
                    return Ok(blocked + 1);
                }
",
        models: &[(
            "wait_queue_timed_stale_wake_up_is_harmless",
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
        models: &[
            ("wait_queue_waker_and_waiter_release_acquire", DEADLOCK),
            ("wait_queue_timed_waiter_and_waker_release_acquire", DEADLOCK),
        ],
    },
    Control {
        // Across processors nothing but the reschedule IPI restarts a halted one.
        name: "cross_cpu_wake_sends_no_ipi",
        file: MODELS,
        from: "        if woke && thread.cpu != self.cpu {
            target.kick.notify();
        }
",
        to: "        let _ = (woke, target);
",
        models: &[
            ("wait_queue_waker_and_waiter_release_acquire", DEADLOCK),
            ("wait_queue_timed_waiter_and_waker_release_acquire", DEADLOCK),
        ],
    },
    Control {
        // A wake from the waiter's own processor sends no IPI: its next tick is all it gets.
        name: "same_cpu_wake_gets_no_tick",
        file: MODELS,
        from: "        me.timer_tick();
",
        to: "",
        models: &[
            ("wait_queue_same_cpu_wake_resumes_on_a_tick", DEADLOCK),
            ("wait_queue_same_cpu_timed_wake_resumes_on_a_tick", DEADLOCK),
        ],
    },
    Control {
        name: "take_all_takes_nobody",
        file: WAIT,
        from: TAKE_ALL,
        to: "        let _ = scheduler;\n        [None; N]\n",
        models: &[("wait_queue_wake_all_wakes_every_waiter", DEADLOCK)],
    },
    Control {
        // Wake-all that is really wake-one: the other waiter is left blocked.
        name: "take_all_takes_one",
        file: WAIT,
        from: TAKE_ALL,
        to: "        let mut taken = [None; N];
        if let Some(first) = taken.first_mut() {
            *first =
                scheduler.without_interrupts(|| self.waiters.lock().iter_mut().find_map(Option::take));
        }
        taken
",
        models: &[(
            "wait_queue_wake_all_wakes_every_waiter",
            &["take_all left a waiter registered", "deadlock"],
        )],
    },
    Control {
        // The waiters are returned but stay registered.
        name: "take_all_leaves_the_waiters",
        file: WAIT,
        from: TAKE_ALL,
        to: "        scheduler.without_interrupts(|| *self.waiters.lock())\n",
        models: &[(
            "wait_queue_wake_all_wakes_every_waiter",
            &["take_all left a waiter registered"],
        )],
    },
    Control {
        // Copying and clearing in two critical sections: a waiter that registers between them
        // has its registration erased and is never woken.
        name: "take_all_in_two_steps",
        file: WAIT,
        from: TAKE_ALL,
        to: "        scheduler.without_interrupts(|| {
            let taken = *self.waiters.lock();
            *self.waiters.lock() = [None; N];
            taken
        })
",
        models: &[
            ("wait_queue_wake_all_keeps_a_concurrent_registration", DEADLOCK),
            ("wait_queue_wake_all_keeps_a_concurrent_timed_registration", DEADLOCK),
        ],
    },
];

/// The scratch copy's workspace: just the crate under test. The tables `scratch_manifest` adds
/// come from the real workspace's manifest, so that they cannot drift from it.
const SCRATCH_WORKSPACE: &str = r#"[workspace]
resolver = "3"
members = ["framekernel/sync"]

"#;

/// The tables of the real workspace that decide how the crate under test is compiled and linted.
const INHERITED_TABLES: &[&str] = &[
    "[workspace.package]",
    "[workspace.lints.rust]",
    "[workspace.lints.clippy]",
    "[profile.release]",
];

/// Added to the scratch workspace for the negative controls, and not for the real models. Loom
/// itself stays optimized, but each control is one build of this crate, which takes much longer
/// than the search that finds its bug.
const CONTROL_PROFILE: &str = r#"[profile.release.package.origin-sync]
opt-level = 0
"#;

/// The table headed by the line `header` in `manifest`: from that line to the next table header.
fn toml_table(manifest: &str, header: &str) -> Option<String> {
    let mut lines = manifest.lines().skip_while(|line| line.trim() != header);
    let mut table = format!("{}\n", lines.next()?);
    for line in lines.take_while(|line| !line.trim_start().starts_with('[')) {
        table.push_str(line);
        table.push('\n');
    }
    Some(table)
}

/// The scratch workspace's manifest: `SCRATCH_WORKSPACE`, the inherited tables, then `profile`.
fn scratch_manifest(root: &Path, profile: &str) -> Result<String> {
    let path = root.join("Cargo.toml");
    let real = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut manifest = SCRATCH_WORKSPACE.to_string();
    for header in INHERITED_TABLES {
        let table = toml_table(&real, header)
            .ok_or_else(|| format!("{}: no table {header}", path.display()))?;
        manifest.push_str(&table);
        manifest.push('\n');
    }
    manifest.push_str(profile);
    Ok(manifest)
}

/// `cargo` for a Loom build of the scratch workspace at `dir`, in a target directory of its own
/// (so that `--cfg loom` never invalidates a normal build), with the search `search` decides.
fn loom_cargo(dir: &Path, search: Search) -> Command {
    let mut c = cargo();
    c.current_dir(dir)
        .env("RUSTFLAGS", "--cfg loom")
        // It would take precedence over `RUSTFLAGS`, and leave the models uncompiled.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("CARGO_TARGET_DIR", target_dir().join("loom"));
    if search != Search::Experiment {
        for name in search_settings() {
            c.env_remove(name);
        }
    }
    if search == Search::Deep {
        c.env(DEEP, "1");
    }
    c
}

/// Runs every model, then every negative control.
pub(crate) fn run() -> Result {
    check_model_lock(MODEL_LOCK)?;
    let root = repo_root();
    check_loom_cfgs_need_test(&root)?;
    let scratch = target_dir().join(SCRATCH);
    let search = Search::requested();
    let ignored: Vec<String> = search_settings()
        .iter()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| match search {
            Search::Gate => true,
            Search::Deep => name != DEEP,
            Search::Experiment => false,
        })
        .collect();
    if !ignored.is_empty() {
        println!(
            "xtask: loom: ignoring {} from the environment: this search does not depend on what \
             is exported ({EXPERIMENT} on `cargo xtask loom` lets them through, for an experiment)",
            ignored.join(", ")
        );
    }
    populate(&root, &scratch, "")?;
    // The Loom configuration is code no normal build sees: it gets the same lints. On a cold
    // cargo cache this first step fetches the crates, each checked against the lock file.
    run_step(loom_cargo(&scratch, Search::Gate).args([
        "clippy",
        "--locked",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))?;
    let report = run_models(&scratch, search)?;
    if search == Search::Experiment {
        println!(
            "xtask: loom: {EXPERIMENT} was set, so the models ran with the search settings in the \
             environment: this proves nothing, and the negative controls did not run"
        );
        return Ok(());
    }
    let path = scratch.join(MODELS);
    let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let models = model_names(&source);
    require_complete(&models, &report)?;
    controls(&root, &scratch, models.len())
}

/// The lines of `source` that use a `cfg` naming `loom` without also naming `test`.
fn loom_cfg_without_test(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            !line.trim_start().starts_with("//")
                && line.contains("cfg")
                && line.contains("loom")
                && !line.contains("test")
        })
        .map(|(index, _)| index + 1)
        .collect()
}

/// `--cfg loom` must not be able to select other code in a build of the kernel: whatever names
/// it must also require `test`, which no build of a shipping crate has. (`developer-tools/` is
/// not compiled for the kernel, and speaks of the flag in its own text.)
fn check_loom_cfgs_need_test(root: &Path) -> Result {
    let tools = root.join("developer-tools");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(
        root,
        &|path| path == tools,
        &|name| name.ends_with(".rs"),
        &mut files,
    )
    .map_err(|e| format!("{}: {e}", root.display()))?;
    let mut offending = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        for line in loom_cfg_without_test(&source) {
            offending.push(format!("{}:{line}", file.display()));
        }
    }
    if offending.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "a cfg naming loom must also require test, or --cfg loom could change the kernel:\n  {}",
            offending.join("\n  ")
        ))
    }
}

/// Runs every model in the scratch workspace, showing its report as it comes and returning it.
fn run_models(scratch: &Path, search: Search) -> Result<String> {
    let args = [
        "test",
        "--locked",
        "--release",
        "--lib",
        "models::",
        "--",
        "--nocapture",
    ];
    println!("xtask: $ cargo {}", args.join(" "));
    let mut child = loom_cargo(scratch, search)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    let stdout = child.stdout.take().ok_or("cannot read cargo's output")?;
    let mut report = String::new();
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| format!("cannot read cargo's output: {e}"))?;
        println!("{line}");
        report.push_str(&line);
        report.push('\n');
    }
    let status = child.wait().map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok(report)
    } else {
        Err(format!("the Loom models failed ({status})"))
    }
}

/// The models `models.rs` defines: the name of the function after each `#[test]`.
fn model_names(source: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut lines = source.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line == "#[test]"
            && let Some(rest) = lines.by_ref().find_map(|l| l.strip_prefix("fn "))
            && let Some((name, _)) = rest.split_once('(')
        {
            names.push(name);
        }
    }
    names
}

/// The models that printed a search report (`check` in `models.rs`).
fn reported_models(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter_map(|line| line.split_once("loom model ")?.1.split_once(':'))
        .map(|(name, _)| name)
        .collect()
}

/// A test run can pass with nothing having been checked, if the models were not compiled in or
/// a model forgot to go through `check`; and a model that no control can break proves nothing.
fn require_complete(models: &[&str], report: &str) -> Result {
    let reported = reported_models(report);
    let silent: Vec<&str> = models
        .iter()
        .copied()
        .filter(|model| !reported.contains(model))
        .collect();
    if !silent.is_empty() {
        return Err(format!(
            "these Loom models reported no search (was `--cfg loom` applied?): {}",
            silent.join(", ")
        ));
    }
    let extra: Vec<&str> = reported
        .iter()
        .copied()
        .filter(|name| !models.contains(name))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "these names in the Loom report are not models in models.rs (a model reporting under \
             another's name?): {}",
            extra.join(", ")
        ));
    }
    let uncontrolled: Vec<&str> = models
        .iter()
        .copied()
        .filter(|model| {
            !CONTROLS
                .iter()
                .any(|c| c.models.iter().any(|(name, _)| name == model))
        })
        .collect();
    if !uncontrolled.is_empty() {
        return Err(format!(
            "no negative control can break these Loom models: {}",
            uncontrolled.join(", ")
        ));
    }
    Ok(())
}

/// Writes the scratch workspace afresh from the working tree, with `profile` in its manifest.
fn populate(root: &Path, scratch: &Path, profile: &str) -> Result {
    // Nothing from another control, or from an older layout, may survive into this one.
    if scratch.exists() {
        fs::remove_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    }
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
    write(
        Path::new("Cargo.toml"),
        scratch_manifest(root, profile)?.as_bytes(),
    )?;
    write(Path::new("Cargo.lock"), MODEL_LOCK.as_bytes())?;
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
    // Only here, in the scratch copy of the crate's manifest, does Loom exist.
    let manifest = Path::new("framekernel/sync/Cargo.toml");
    let mut text = fs::read_to_string(scratch.join(manifest))
        .map_err(|e| format!("{}: {e}", scratch.join(manifest).display()))?;
    text.push_str(LOOM_DEV_DEPENDENCY);
    write(manifest, text.as_bytes())
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
fn controls(root: &Path, scratch: &Path, models: usize) -> Result {
    let mut models_caught = 0;
    for control in CONTROLS {
        populate(root, scratch, CONTROL_PROFILE)?;
        apply(control, scratch)?;
        // A control that does not compile would "fail" for the wrong reason.
        let build = loom_cargo(scratch, Search::Gate)
            .args([
                "test",
                "--locked",
                "--offline",
                "--release",
                "--lib",
                "--no-run",
            ])
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
            let out = loom_cargo(scratch, Search::Gate)
                .args([
                    "test",
                    "--locked",
                    "--offline",
                    "--release",
                    "--lib",
                    &test,
                    "--",
                    "--exact",
                    "--nocapture",
                ])
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
        "xtask: loom: all {models} models passed and all {} negative controls were caught \
         ({models_caught} model runs)",
        CONTROLS.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_ends_at_the_next_header() {
        let manifest = "[a]\nx = 1\n# note\n\n[b]\ny = 2\n[a.c]\nz = 3\n";
        assert_eq!(
            toml_table(manifest, "[a]").as_deref(),
            Some("[a]\nx = 1\n# note\n\n")
        );
        assert_eq!(
            toml_table(manifest, "[a.c]").as_deref(),
            Some("[a.c]\nz = 3\n")
        );
        assert_eq!(toml_table(manifest, "[missing]"), None);
    }

    #[test]
    fn the_scratch_workspace_is_linted_as_the_real_one_is() {
        let manifest = scratch_manifest(&repo_root(), CONTROL_PROFILE).unwrap();
        for line in [
            "members = [\"framekernel/sync\"]",
            "unsafe_op_in_unsafe_fn = \"deny\"",
            "undocumented_unsafe_blocks = \"deny\"",
            "codegen-units = 1",
            "opt-level = 0",
        ] {
            assert!(manifest.contains(line), "{line} not in:\n{manifest}");
        }
        assert!(!manifest.contains("\"framekernel\","), "{manifest}");
    }

    #[test]
    fn only_loom_settings_count_as_search_settings() {
        for name in [
            "LOOM_MAX_PREEMPTIONS",
            "LOOM_LOG",
            "ORIGIN_LOOM_SPINS",
            "ORIGIN_LOOM_DEEP",
        ] {
            assert!(is_search_setting(&OsString::from(name)), "{name}");
        }
        for name in ["PATH", "XLOOM_", "ORIGIN_QEMU_SMP", "RUSTFLAGS"] {
            assert!(!is_search_setting(&OsString::from(name)), "{name}");
        }
    }

    #[test]
    fn model_names_come_from_the_tests() {
        let source = "fn helper() {}\n#[test]\nfn first() {\n    check();\n}\n\n/// Doc.\n#[test]\n#[ignore]\nfn second() {}\n";
        assert_eq!(model_names(source), ["first", "second"]);
    }

    #[test]
    fn only_a_deep_search_asks_the_models_for_their_larger_bounds() {
        let asks_for_it = |search| {
            loom_cargo(Path::new("/nowhere"), search)
                .get_envs()
                .any(|(name, value)| name == DEEP && value.is_some())
        };
        assert!(asks_for_it(Search::Deep));
        assert!(!asks_for_it(Search::Gate));
        assert!(!asks_for_it(Search::Experiment));
    }

    #[test]
    fn a_cfg_naming_loom_must_require_test() {
        let source = "// cfg(loom) in a comment\n#[cfg(all(test, loom))]\n#[cfg(loom)]\nlet x = cfg!(loom);\n";
        assert_eq!(loom_cfg_without_test(source), [3, 4]);
        check_loom_cfgs_need_test(&repo_root()).unwrap();
    }

    #[test]
    fn an_unreported_or_uncontrolled_model_stops_the_run() {
        let model = CONTROLS[0].models[0].0;
        let report = format!("test x ... loom model {model}: 3 executions, exhaustive\n");
        assert!(require_complete(&[model], &report).is_ok());
        assert!(
            require_complete(&[model], "")
                .unwrap_err()
                .contains("reported no search")
        );
        let unknown = format!("{report}loom model elsewhere: 1 executions\n");
        assert!(
            require_complete(&[model], &unknown)
                .unwrap_err()
                .contains("elsewhere")
        );
        let uncontrolled = "loom model not_a_control: 1 executions\n";
        assert!(
            require_complete(&["not_a_control"], uncontrolled)
                .unwrap_err()
                .contains("no negative control")
        );
    }
}
