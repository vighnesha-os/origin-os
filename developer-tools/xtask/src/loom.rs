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
const WENT_BACK: &[&str] = &["the clock went backwards"];
const STALE: &[&str] = &["a processor kept a translation the shootdown took away"];
/// An initiator gave up waiting for an answer that could never come (`PATIENCE` in `models.rs`).
const NO_ANSWER: &[&str] = &["a processor the shootdown asked never answered"];
/// A request for one address space interrupted a processor in another.
const OUTSIDE: &[&str] = &["a shootdown for one space reached a processor in another"];
/// The frame was reused while something could still reach it, or two things reused it.
const REUSED_TOO_SOON: &[&str] = &[
    "Causality violation",
    "the frame was not freed once",
    "a processor kept a translation the shootdown took away",
];
const LOST_UPDATE: &[&str] = &["the floor is the largest reading"];
/// A change to a space returned with a page of it as it was (S3-5b's review, finding A).
const UNFINISHED: &[&str] = &["a change that stalled left a page as it was"];
/// A change gave back a frame a processor could still reach through a stale translation.
const GIVEN_BACK: &[&str] = &[
    "Causality violation",
    "a change gave back what a stalled shootdown left reachable",
];
/// A change that returned a stall left its space confirmed.
const FORGOTTEN: &[&str] = &["a change that stalled left its space confirmed"];

/// What the models of a reclaim that stalls report (`models.rs`; TASKS item 73): what a slot held
/// reused while a processor could still reach it, a slot never given back or given back twice,
/// and two slots out of the tables at once.
const WAITING_REUSED: &[&str] = &[
    "Causality violation",
    "a processor kept a translation the shootdown took away",
    "what an unmap returned was not released exactly once",
];
const NEVER_GIVEN_BACK: &[&str] = &[
    "a stalled slot was never given back",
    "what an unmap returned was not released exactly once",
];
const TWO_OUT: &[&str] = &["a second slot was unmapped while one waited"];
/// What a waiting slot held, released on a stall of a fresh shootdown (T73-5).
const RELEASED_ON_A_STALL: &[&str] = &[
    "what a waiting slot held was released on a stall",
    "Causality violation",
    "what an unmap returned was not released exactly once",
];

/// What the port models report (`models.rs`): a packet lost or doubled, a binding left armed
/// after its handle closed, and a closed port left holding something.
const PORT_LOST: &[&str] = &["a signal was lost, or one binding made two packets"];
const PORT_STILL_ARMED: &[&str] = &["a binding stayed armed after its handle closed"];
const PORT_CLOSED: &[&str] = &[
    "a packet was queued in a closed port",
    "a binding stayed armed on a closed port",
    "a close did not take back its binding's storage",
];

/// What the contexts models report (`models.rs`; TASKS item 70): a live context's stack in no
/// record, a stack found free before its processor's last use of it, a context lost, and an
/// ended context never retired.
const CTX_UNCOVERED: &[&str] = &["a live context's stack was in no record"];
const CTX_LAST_TOUCH: &[&str] = &["a stack was found free before its processor's last use of it"];
const CTX_LOST: &[&str] = &["a published context went unrecorded"];
const CTX_NEVER_RETIRED: &[&str] = &["an ended context's home was never retired"];
/// B1's and B2's reports (TASKS item 81): a settle counted while a reap still held what it frees,
/// or its read of what the reap freed was not ordered after the freeing.
const BACKLOG_EARLY: &[&str] = &[
    "a settle passed while a reap still held what it frees",
    "Causality violation",
];
/// K1's report (TASKS item 77): a switch, or the table read back, named a home other than the
/// incoming context's.
const CTX_WRONG_HOME: &[&str] = &["a switch named a stack not the incoming context's own"];
/// W1's reports (TASKS item 75): a first frame both withdrawn and resumed, or neither; a running
/// context's stack uncovered; C's context lost; a withdrawn context's home left behind.
const CTX_WITHDRAWN: &[&str] = &[
    "a published context was both withdrawn and resumed, or neither",
    "a live context's stack was in no record",
    "a published context went unrecorded",
    "a withdrawn context kept its home",
];

/// What the exits models report (`models.rs`; TASKS item 79): a settle that passed while an
/// ended thread still held what it gives back at its reap, seen as a wrong value or as a read
/// Loom's cell finds unordered with the reaper's write.
const EXITS_EARLY: &[&str] = &[
    "a settle passed while an ended thread still held what it gives back",
    "Causality violation",
];

/// What the moves models report (`moves_models.rs`; TASKS item 95, ADR-0020 S4 and S5): a frame
/// taken twice, a frame a refused move did not give back, a frame resumed on another processor
/// before its own had left its stack, and two holders of the move gate overlapping or missing
/// each other's writes. A home a scan missed is `CTX_UNCOVERED`.
const MOVE_TAKEN_TWICE: &[&str] =
    &["a frame was taken twice: by a move and a switch, or by two switches"];
const MOVE_NOT_GIVEN_BACK: &[&str] =
    &["a frame back from a full destination could not be resumed at its source"];
const MOVE_RESUMED_TOO_SOON: &[&str] =
    &["a frame was resumed on another processor before its own had left its stack"];
/// LV3's report (TASKS item 96): a frame its processor was still switching out was answered
/// `NotHere`, which the kernel's mover takes for a lost frame.
const MOVE_CALLED_LOST: &[&str] = &["a frame being switched out was called lost"];
const MOVE_GATE: &[&str] = &[
    "two holders of the move gate at once",
    "a holder of the move gate missed the one before it",
];

/// What `threads_a_stale_name_wakes_nobody` reports (TASKS item 92): a wake-up by a stale name
/// reached the record's next thread.
const STALE_WOKE: &[&str] = &["a stale name woke the record's next thread"];

/// What the move models report when a wake-up trusts a location it did not read again under the
/// lock (TASKS item 96): the slot it reaches holds no record, which enters pinned mode (ADR-0020
/// S20, trigger 3) and, in these models, which plant no inconsistency, fails as one.
const STALE_SLOT_MET: &[&str] = &["a wake-up met an inconsistency: Misplaced"];

/// What `threads_a_misplaced_location_wakes_only_its_thread` reports (TASKS item 96): a wake-up by
/// a misplaced location woke the thread the slot holds.
const MISPLACED_WOKE: &[&str] = &["a misplaced location woke the slot's other thread"];

/// What `threads_a_wake_racing_an_exit_follows_nothing` reports (TASKS item 92): a wake-up
/// followed an exit, or a new thread in the record, as if it were a move.
const EXIT_FOLLOWED: &[&str] = &["a wake-up counted an exit or a new thread as a move"];

/// What `threads_a_switch_takes_each_record_in_turn` reports (the review of TASKS item 92, R92-8):
/// a switch restored a thread's state other than its last switch-out saved.
const NOT_RESTORED: &[&str] =
    &["a switch restored other than what the thread's last switch-out saved"];

/// The models of a change to a space whose shootdowns may stall (`models.rs`).
const ROUND_STALLS: &str =
    "a_change_whose_round_stalls_is_still_made_and_frees_only_what_a_flush_covered";
const FIRST_FLUSH_STALLS: &str = "a_change_whose_first_flush_stalls_is_still_made";

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

const SPIN_LOCK: &str = "framekernel/frame/sync/src/lib.rs";
const WAIT: &str = "framekernel/frame/sync/src/wait.rs";
const RUN_QUEUE: &str = "framekernel/src/runqueue.rs";
const MODELS: &str = "framekernel/frame/sync/src/models.rs";
/// The models of moving a context between tables (TASKS item 95), the module `models::moves`,
/// in a file of its own beside `MODELS`. Its models are named `moves::<function>`.
const MOVES_MODELS: &str = "framekernel/frame/sync/src/moves_models.rs";
/// The module path of `MOVES_MODELS` under `models`, as its models report themselves.
const MOVES_MODULE: &str = "moves::";
const FLOOR: &str = "framekernel/frame/sync/src/floor.rs";
const SHOOTDOWN: &str = "framekernel/frame/sync/src/shootdown.rs";
const REFCOUNT: &str = "framekernel/frame/sync/src/refcount.rs";
const SLOTS: &str = "framekernel/frame/sync/src/slots.rs";
const PORTS: &str = "framekernel/frame/sync/src/ports.rs";
const CONTEXTS: &str = "framekernel/frame/sync/src/contexts.rs";
const BACKLOG: &str = "framekernel/frame/sync/src/backlog.rs";
const EXITS: &str = "framekernel/frame/sync/src/exits.rs";
const THREADS: &str = "framekernel/frame/sync/src/threads.rs";

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
            // A thread's state reaches the next holder of its record, and the running slot's
            // record a reader on another processor, only through the locks (TASKS item 92).
            ("threads_a_switch_takes_each_record_in_turn", CAUSALITY),
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
        to: "            Err(_) => {\n                drop(SpinGuard { lock: self, _shares_t: PhantomData });\n                None\n            }\n",
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
        // TASKS item 92 (ADR-0020 S1): today's `ThreadRef`, the processor and slot a thread had
        // when it was named, kept across a move. In the models a thread registers on the
        // processor it started on, so that is the slot its processor's `home` records.
        name: "wake_to_the_stale_slot",
        file: MODELS,
        from: "        let record = self.world.record(thread.record);
        match threads::wake(
            &*self.world,
            record,
            thread.record,
            thread.generation,
            no_inconsistency,
            no_line,
        )
        .outcome
        {
",
        to: "        let (cpu, slot) = (self.world.cpus.iter().enumerate())
            .find_map(|(cpu, c)| c.home.filter(|h| h.0 == thread.record).map(|h| (cpu, h.1)))
            .unwrap();
        let ready = {
            let mut queue = self.world.cpus[cpu].queue.lock();
            let generation = queue.generation(slot);
            queue.wake(slot, generation)
        };
        let cpu = cpu as u32;
        match (Outcome::Reached { ready, cpu }) {
",
        models: &[
            ("threads_a_wake_follows_a_move", DEADLOCK),
            ("threads_a_timed_wake_follows_a_move", DEADLOCK),
        ],
    },
    Control {
        // The location read once and trusted: a move that completes between the read and the
        // lock leaves the wake-up at the slot the thread left. Before TASKS item 96 that slot's
        // wake was lost (a deadlock). Since its record check, every failing interleaving is the
        // same one: the wake-up reads the location before the move, the move departs the thread
        // from processor 0's slot 1 (which nothing takes again in these models), and the wake-up
        // then locks processor 0 and finds slot 1 holding no record: S20's trigger 3, which the
        // models' hook reports as an inconsistency. An interleaving whose lock comes before the
        // move reaches the thread there, and its pending wake-up travels with it: no failure.
        // Unplanted, both models pass.
        name: "location_not_read_again_under_the_lock",
        file: THREADS,
        from: "            if now != Some(at) {
                return changed(now, record.generation() == generation);
            }
",
        to: "            let _ = now;
",
        models: &[
            ("threads_a_wake_follows_a_move", STALE_SLOT_MET),
            ("threads_a_timed_wake_follows_a_move", STALE_SLOT_MET),
        ],
    },
    Control {
        // The review of TASKS item 92, R92-1: a wake-up that follows its thread to the processor
        // it moved to but wakes the slot it left there. In the move models the thread leaves
        // slot 1 and arrives in slot 2, beside a blocked thread in slot 1.
        name: "wake_to_the_slot_it_left",
        file: THREADS,
        from: "                        ready: Q::wake_slot(queue, at.slot()),\n",
        to: "                        ready: Q::wake_slot(queue, at.slot().min(1)),\n",
        models: &[
            ("threads_a_wake_follows_a_move", DEADLOCK),
            ("threads_a_timed_wake_follows_a_move", DEADLOCK),
        ],
    },
    Control {
        // TASKS item 96 (ADR-0020 S20, trigger 3; the coordinating session's ruling Q6): the
        // record check removed. A location planted naming a slot that holds another thread then
        // wakes that thread, and the named one is never woken.
        name: "wake_without_the_record_check",
        file: THREADS,
        from: "                if Q::holds(queue, at.slot(), number, record) {\n",
        to: "                if true {\n",
        models: &[("threads_a_misplaced_location_wakes_only_its_thread", MISPLACED_WOKE)],
    },
    Control {
        // TASKS item 96 (ADR-0020 S6, N1): a move that does not carry the pending wake-up. A
        // wake-up that reached the thread before its move is lost with it, and the thread blocks
        // for good on its new processor.
        name: "move_drops_the_pending_wake_up",
        file: RUN_QUEUE,
        from: "            *p = wake_pending;\n",
        to: "            let _ = wake_pending;\n            *p = false;\n",
        models: &[
            ("threads_a_wake_follows_a_move", DEADLOCK),
            ("threads_a_timed_wake_follows_a_move", DEADLOCK),
        ],
    },
    Control {
        // The review's R92-6: the location read Relaxed. A wake-up that locks the old queue after
        // its thread's exit may read the new thread's location there with the old generation,
        // and follow it as a move.
        name: "location_load_relaxed",
        file: THREADS,
        from: "Location::unpack(self.location.load(Acquire))",
        to: "Location::unpack(self.location.load(Relaxed))",
        models: &[("threads_a_wake_racing_an_exit_follows_nothing", EXIT_FOLLOWED)],
    },
    Control {
        // The review's R92-8: a slot that names another thread's record (slots 1 and 2, the
        // model's two threads, swapped), so a switch saves and restores the wrong thread's state.
        name: "slot_names_another_record",
        file: RUN_QUEUE,
        from: "            Some(_) => self.record.get(slot).copied().flatten(),\n",
        to: "            Some(_) => self.record.get(slot ^ 3).copied().flatten(),\n",
        models: &[("threads_a_switch_takes_each_record_in_turn", NOT_RESTORED)],
    },
    Control {
        // A location found clear under the lock taken for a move: the wake-up follows an exit.
        name: "an_exit_followed_as_a_move",
        file: THREADS,
        from: "        (None, _) => Look::Ends(Outcome::Gone),\n",
        to: "        (None, _) => Look::Moved,\n",
        models: &[("threads_a_wake_racing_an_exit_follows_nothing", EXIT_FOLLOWED)],
    },
    Control {
        // A location found set with a new generation taken for a move: the wake-up follows the
        // record's next thread.
        name: "a_new_thread_followed_as_a_move",
        file: THREADS,
        from: "        (Some(_), false) => Look::Ends(Outcome::Stale),\n",
        to: "        (Some(_), false) => Look::Moved,\n",
        models: &[("threads_a_wake_racing_an_exit_follows_nothing", EXIT_FOLLOWED)],
    },
    Control {
        // A name kept past its thread's reap wakes the record's next thread.
        name: "generation_not_compared",
        file: THREADS,
        from: "            Look::Ends(if record.generation() == generation {\n",
        to: "            Look::Ends(if record.generation() == record.generation() {\n",
        models: &[("threads_a_stale_name_wakes_nobody", STALE_WOKE)],
    },
    Control {
        // The run queue reports a waiter it did not make ready: nothing can ever run it.
        name: "run_queue_wake_readies_nobody",
        file: RUN_QUEUE,
        from: "                if let Some(s) = self.state.get_mut(slot) {
                    *s = State::Ready;
                }
                self.admit(slot, floor);
                if let Some(w) = self.woken.get_mut(slot) {
",
        to: "                self.admit(slot, floor);
                if let Some(w) = self.woken.get_mut(slot) {
",
        models: &[
            ("wait_queue_waker_and_waiter_relaxed", DEADLOCK),
            ("wait_queue_same_cpu_wake_resumes_on_a_tick", DEADLOCK),
        ],
    },
    Control {
        // A wake that leaves no mark: the reschedule IPI finds nobody it should run.
        name: "run_queue_wake_leaves_no_mark",
        file: RUN_QUEUE,
        from: "                if let Some(w) = self.woken.get_mut(slot) {
                    *w = true;
                }
                true
",
        to: "                true
",
        models: &[("wait_queue_waker_and_waiter_relaxed", DEADLOCK)],
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
        from: "                self.world.cpus[cpu as usize].kick.notify();
",
        to: "                let _ = cpu;
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
    // The monotonic clock's floor (ADR-0012).
    Control {
        // A reading below the floor comes back as it was read: no floor at all.
        name: "floor_gives_back_the_reading",
        file: FLOOR,
        from: "                return floor;\n",
        to: "                return reading;\n",
        models: &[
            ("floor_handoff_between_two_processors", WENT_BACK),
            ("floor_handoff_chain", WENT_BACK),
        ],
    },
    Control {
        // The floor is never raised, so every reading above it is given out and forgotten.
        name: "floor_never_rises",
        file: FLOOR,
        from: "                floor,\n                reading,\n",
        to: "                floor,\n                floor,\n",
        models: &[
            ("floor_handoff_between_two_processors", WENT_BACK),
            ("floor_handoff_chain", WENT_BACK),
        ],
    },
    Control {
        // Raising the floor with a load and a store instead of one atomic step: two readers
        // can both act on the same old floor, and the smaller reading can land last.
        name: "floor_raised_by_a_plain_store",
        file: FLOOR,
        from: "        let mut floor = self.latest.load(Ordering::Relaxed);
        loop {
            if reading <= floor {
                return floor;
            }
            match self.latest.compare_exchange_weak(
                floor,
                reading,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return reading,
                Err(newer) => floor = newer,
            }
        }
",
        to: "        let floor = self.latest.load(Ordering::Relaxed);
        if reading <= floor {
            return floor;
        }
        self.latest.store(reading, Ordering::Relaxed);
        reading
",
        models: &[("floor_racing_readers_end_at_the_largest_reading", LOST_UPDATE)],
    },
    Control {
        // The caller's handoff weakened to `Relaxed`: the later call no longer happens after
        // the earlier one, so a stale floor at or above its reading is taken at face value.
        // The guarantee is for calls the caller has ordered, and the model can tell.
        name: "floor_handoff_not_ordered",
        file: MODELS,
        from: "                    handed.store(first + 1, Release);\n",
        to: "                    handed.store(first + 1, Relaxed);\n",
        models: &[("floor_handoff_between_two_processors", WENT_BACK)],
    },
    // TLB shootdown (N14 step 3, S3-5a).
    Control {
        // The initiator goes on without waiting for the answers.
        name: "shootdown_does_not_wait",
        file: SHOOTDOWN,
        from: "                while served.load(Ordering::Acquire) < generation {\n",
        to: "                while false && served.load(Ordering::Acquire) < generation {\n",
        models: &[
            ("shootdown_reaches_an_online_processor", STALE),
            ("shootdown_two_initiators_serve_each_other", STALE),
            ("deferred_free_comes_after_the_last_use_of_a_stale_translation", REUSED_TOO_SOON),
        ],
    },
    Control {
        // Nobody is told, so nobody answers.
        name: "shootdown_skips_its_target",
        file: SHOOTDOWN,
        from: "                if id != me && self.must_answer(id, scope) {\n",
        to: "                if false && id != me && self.must_answer(id, scope) {\n",
        models: &[("shootdown_reaches_an_online_processor", STALE)],
    },
    Control {
        // The protocol's storage one processor short of the machine (ADR-0008 Amendment 1, D1
        // and D7): the last processor cannot come online in it, is never asked, and keeps its
        // translation.
        name: "shootdown_records_one_short",
        file: MODELS,
        from: "            shootdown: Shootdown::new(Held::new(count)),\n",
        to: "            shootdown: Shootdown::new(Held::new(count - 1)),\n",
        models: &[(
            "shootdown_reaches_the_last_processor_whatever_the_count",
            STALE,
        )],
    },
    Control {
        // The initiator asks every processor that must answer but waits for all but the last
        // index (ADR-0008 Amendment 1, D7).
        name: "shootdown_wait_stops_one_short",
        file: SHOOTDOWN,
        from: "            for target in 0..count {\n",
        to: "            for target in 0..count.saturating_sub(1) {\n",
        models: &[(
            "shootdown_reaches_the_last_processor_whatever_the_count",
            STALE,
        )],
    },
    Control {
        // The target answers without doing what it was asked.
        name: "shootdown_answer_without_invalidating",
        file: SHOOTDOWN,
        from: "            hardware.invalidate(Request::decode(self.request.load(Ordering::Relaxed)));\n",
        to: "            let _ = self.request.load(Ordering::Relaxed);\n",
        models: &[
            ("shootdown_reaches_an_online_processor", STALE),
            ("deferred_free_comes_after_the_last_use_of_a_stale_translation", REUSED_TOO_SOON),
        ],
    },
    Control {
        // The answer is not ordered with the invalidation it reports.
        name: "shootdown_answer_relaxed",
        file: SHOOTDOWN,
        from: "            served.store(generation, Ordering::Release);\n",
        to: "            served.store(generation, Ordering::Relaxed);\n",
        models: &[
            ("shootdown_reaches_an_online_processor", STALE),
            ("deferred_free_comes_after_the_last_use_of_a_stale_translation", REUSED_TOO_SOON),
        ],
    },
    Control {
        // The initiator reads the answers without acquiring them: what a target did before it
        // answered, its invalidation and its last use of the page, is not ordered before what the
        // initiator does next, the frame's release included (review finding 7).
        name: "initiator_reads_answers_relaxed",
        file: SHOOTDOWN,
        from: "                while served.load(Ordering::Acquire) < generation {\n",
        to: "                while served.load(Ordering::Relaxed) < generation {\n",
        models: &[
            ("shootdown_reaches_an_online_processor", STALE),
            ("deferred_free_comes_after_the_last_use_of_a_stale_translation", REUSED_TOO_SOON),
        ],
    },
    Control {
        // The request is not published with its generation: a target can read the previous
        // one and invalidate the wrong page.
        name: "shootdown_generation_relaxed",
        file: SHOOTDOWN,
        from: "self.generation.fetch_add(1, Ordering::Release)",
        to: "self.generation.fetch_add(1, Ordering::Relaxed)",
        models: &[("shootdown_reaches_an_online_processor", STALE)],
    },
    Control {
        // An initiator waiting for its turn does not serve the one that has it: each waits
        // for the other's answer.
        name: "shootdown_waiting_initiator_does_not_serve",
        file: SHOOTDOWN,
        from: "                self.service(hardware);\n                prim::spin_loop();\n",
        to: "                prim::spin_loop();\n",
        models: &[("shootdown_two_initiators_serve_each_other", NO_ANSWER)],
    },
    Control {
        // The initiator reads who is online without ordering that after its change.
        name: "shootdown_no_fence_before_reading_the_online_set",
        file: SHOOTDOWN,
        from: "            prim::fence(Ordering::SeqCst);\n            let count = self.records.count();\n",
        to: "            let count = self.records.count();\n",
        models: &[
            ("shootdown_and_a_processor_coming_online", STALE),
            ("shootdown_and_a_processor_entering_the_space", STALE),
        ],
    },
    Control {
        // A processor comes online and walks tables without ordering that after the flag.
        name: "online_no_fence_before_the_first_flush",
        file: SHOOTDOWN,
        from: "            prim::fence(Ordering::SeqCst);\n            hardware.invalidate(Request::Everything);\n",
        to: "            hardware.invalidate(Request::Everything);\n",
        models: &[("shootdown_and_a_processor_coming_online", STALE)],
    },
    Control {
        // A processor enters the space and walks its tables without ordering that after
        // saying so: the initiator can miss it while its walks still see the old entry.
        name: "entering_a_space_without_a_fence",
        file: SHOOTDOWN,
        from: "        prim::fence(Ordering::SeqCst);\n        load();\n",
        to: "        load();\n",
        models: &[("shootdown_and_a_processor_entering_the_space", STALE)],
    },
    Control {
        // A processor still on the old tables counts as gone once it has announced the new ones.
        name: "a_processor_leaving_is_not_counted",
        file: SHOOTDOWN,
        from: "            || self
                .leaving(id)
                .is_some_and(|leaving| leaving.load(Ordering::Acquire) == root)
",
        to: "",
        models: &[("shootdown_and_a_processor_leaving_the_space", STALE)],
    },
    Control {
        // `holds` reads `leaving` first, in the order `switch_space` writes the two: a switch
        // that writes both between its reads is missed by each, the old root no longer in
        // `loaded` and not yet in `leaving` (review finding 7).
        name: "holds_reads_leaving_first",
        file: SHOOTDOWN,
        from: "        self.loaded(id)
            .is_some_and(|loaded| loaded.load(Ordering::Acquire) == root)
            || self
                .leaving(id)
                .is_some_and(|leaving| leaving.load(Ordering::Acquire) == root)
",
        to: "        self.leaving(id)
            .is_some_and(|leaving| leaving.load(Ordering::Acquire) == root)
            || self
                .loaded(id)
                .is_some_and(|loaded| loaded.load(Ordering::Acquire) == root)
",
        models: &[("shootdown_and_a_processor_leaving_the_space", STALE)],
    },
    Control {
        // The old root is forgotten before the CR3 write, not after it.
        name: "the_old_root_forgotten_before_the_switch",
        file: SHOOTDOWN,
        from: "        prim::fence(Ordering::SeqCst);\n        load();\n        if let Some(leaving) = self.leaving(me) {\n            leaving.store(0, Ordering::Release);\n        }\n",
        to: "        prim::fence(Ordering::SeqCst);\n        if let Some(leaving) = self.leaving(me) {\n            leaving.store(0, Ordering::Release);\n        }\n        load();\n",
        models: &[("shootdown_and_a_processor_leaving_the_space", STALE)],
    },
    Control {
        // The record and the CR3 write with interrupts on: the scheduler's switch, in an
        // interrupt on the same processor, can land between them (review finding 1).
        name: "a_switch_that_can_be_interrupted",
        file: SHOOTDOWN,
        from: "        hardware.without_interrupts(|here| {\n            self.switch_here(hardware, hardware.this_cpu(here), to, load);\n        });\n",
        to: "        let me = hardware.without_interrupts(|here| hardware.this_cpu(here));\n        self.switch_here(hardware, me, to, load);\n",
        models: &[(
            "a_switch_and_the_scheduler_on_one_processor",
            &["the record names a space this processor has left"],
        )],
    },
    Control {
        // A visit that leaves no record: a shootdown for the visited space passes the
        // processor over while it still holds the translation (review finding 2).
        name: "a_visit_that_is_not_recorded",
        file: SHOOTDOWN,
        from: "            if let Some(slot) = slot {\n                slot.store(visiting, Ordering::Release);\n            }\n",
        to: "",
        models: &[("shootdown_and_a_processor_visiting_the_space", STALE)],
    },
    Control {
        // A request for one space interrupts everyone.
        name: "a_space_request_asks_everyone",
        file: SHOOTDOWN,
        from: "                Scope::Space(root) => self.holds(id, root),\n",
        to: "                Scope::Space(root) => root == root,\n",
        models: &[("shootdown_for_a_space_asks_only_the_processors_in_it", OUTSIDE)],
    },
    // The deferred free (N14 step 3, S3-5a).
    Control {
        // The frame goes back to the allocator as soon as it leaves the tables.
        name: "released_before_the_shootdown",
        file: SHOOTDOWN,
        from: "        match self.shoot(hardware, request, scope) {
            Ok(done) => {
                release(value);
                Ok(done)
            }
            Err(stalled) => {
                core::mem::forget(value);
                Err(stalled)
            }
        }
",
        to: "        release(value);
        self.shoot(hardware, request, scope)
",
        models: &[
            ("deferred_free_comes_after_the_last_use_of_a_stale_translation", REUSED_TOO_SOON),
            ("deferred_free_of_a_frame_shared_by_two_spaces", REUSED_TOO_SOON),
        ],
    },
    Control {
        // Giving up a reference does not publish what its holder did with the frame.
        name: "reference_released_relaxed",
        file: REFCOUNT,
        from: "self.count.fetch_sub(1, Ordering::Release)",
        to: "self.count.fetch_sub(1, Ordering::Relaxed)",
        models: &[("deferred_free_of_a_frame_shared_by_two_spaces", REUSED_TOO_SOON)],
    },
    Control {
        // Whoever is last does not see what the others did with the frame.
        name: "last_reference_without_a_fence",
        file: REFCOUNT,
        from: "        prim::fence(Ordering::Acquire);\n        true\n",
        to: "        true\n",
        models: &[("deferred_free_of_a_frame_shared_by_two_spaces", REUSED_TOO_SOON)],
    },
    Control {
        // The first unmapper thinks it is the last.
        name: "a_shared_frame_freed_by_the_first_unmap",
        file: REFCOUNT,
        from: "!= 1 {\n            return false;\n",
        to: "!= 2 {\n            return false;\n",
        models: &[("deferred_free_of_a_frame_shared_by_two_spaces", REUSED_TOO_SOON)],
    },
    Control {
        // Nobody is in the space, as far as the initiator can tell.
        name: "a_space_request_asks_nobody",
        file: SHOOTDOWN,
        from: "                Scope::Space(root) => self.holds(id, root),\n",
        to: "                Scope::Space(root) => root != root,\n",
        models: &[("shootdown_for_a_space_asks_only_the_processors_in_it", STALE)],
    },
    Control {
        // A slot being reclaimed is free before its shootdown: the next taker can have it while
        // another processor still holds the old translation (ADR-0004 Amendment 1, A2).
        name: "slot_free_before_the_shootdown",
        file: SLOTS,
        from: "        self.set(slot, State::Reclaiming);\n",
        to: "        self.free(slot);\n",
        models: &[("slots_reclaimed_slot_is_taken_only_after_its_shootdown", STALE)],
    },
    Control {
        // What a reclaimed slot held goes back as soon as it leaves the tables.
        name: "slot_released_before_the_shootdown",
        file: SLOTS,
        from: "                let unmapped = unmap(slot, pages);
                let outcome = shoot(slot, pages);
                match outcome {
                    Ok(completed) => {
                        release(unmapped);
                        let mut table = self.table.lock();
                        table.counts.retired -= 1;
                        table.counts.pages -= u64::from(pages);
                        table.free(slot);
                        Some(Reclaim::Freed {
                            completed,
                            asked_again: 0,
                        })
                    }
                    Err(stall) => {
                        let mut table = self.table.lock();
                        // Out of the tables, so no longer mapped, though still retired.
                        table.counts.pages -= u64::from(pages);
                        table.set(slot, State::Waiting);
                        table.waiting = Some(Waiting {
                            slot,
                            pages,
                            kept: unmapped,
                            stall,
                            asked_again: 0,
                        });
                        self.publish(&table);
                        Some(Reclaim::Stalled {
                            stall,
                            asked_again: 0,
                        })
                    }
                }
            }
",
        to: "                release(unmap(slot, pages));
                let outcome = shoot(slot, pages);
                match outcome {
                    Ok(completed) => {
                        let mut table = self.table.lock();
                        table.counts.retired -= 1;
                        table.counts.pages -= u64::from(pages);
                        table.free(slot);
                        Some(Reclaim::Freed {
                            completed,
                            asked_again: 0,
                        })
                    }
                    Err(_) => None,
                }
            }
",
        models: &[(
            "slots_reclaimed_slot_is_taken_only_after_its_shootdown",
            REUSED_TOO_SOON,
        )],
    },
    Control {
        // A taker takes a slot that is being reclaimed as if it were free.
        name: "slot_taken_while_it_is_reclaimed",
        file: SLOTS,
        from: "                let from = table.first_free;
                let slot = (from..table.len()).find(|&s| table.state(s) == Some(State::Free))?;
",
        to: "                let from = 0;
                let slot = (from..table.len())
                    .find(|&s| matches!(table.state(s), Some(State::Free | State::Reclaiming)))?;
",
        models: &[("slots_reclaimed_slot_is_taken_only_after_its_shootdown", STALE)],
    },
    Control {
        // TASKS item 73: a stall taken for an answer, so what the slot held goes back while the
        // processor that stalled may still reach it.
        name: "slot_released_on_a_stall",
        file: SLOTS,
        from: "                let outcome = shoot(slot, pages);\n                match outcome {\n",
        to: "                let outcome = shoot(slot, pages);\n                match outcome.or(Ok(Completed { processors: 0, ticks: 0 })) {\n",
        models: &[(
            "slots_stalled_reclaim_is_given_back_only_after_a_fresh_shootdown",
            WAITING_REUSED,
        )],
    },
    Control {
        // T73-5 (the independent review of TASKS item 73): the slot's pages left the tables only
        // after its shootdown, so a processor that served it can walk to them again.
        name: "slot_unmapped_after_its_shootdown",
        file: SLOTS,
        from: "                let unmapped = unmap(slot, pages);\n                let outcome = shoot(slot, pages);\n",
        to: "                let outcome = shoot(slot, pages);\n                let unmapped = unmap(slot, pages);\n",
        models: &[
            ("slots_reclaimed_slot_is_taken_only_after_its_shootdown", WAITING_REUSED),
            (
                "slots_stalled_reclaim_is_given_back_only_after_a_fresh_shootdown",
                WAITING_REUSED,
            ),
        ],
    },
    Control {
        // T73-5: what a waiting slot held goes back when a fresh shootdown stalls too.
        name: "slot_released_on_a_reask_stall",
        file: SLOTS,
        from: "                    Err(again) => {\n                        table.set(slot, State::Waiting);\n",
        to: "                    Err(again) => {\n                        if let Some(w) = table.waiting.take() {\n                            drop(table);\n                            release(w.kept);\n                            return Some(Reclaim::Stalled { stall: again, asked_again: 1 });\n                        }\n                        table.set(slot, State::Waiting);\n",
        models: &[("slots_a_reask_that_stalls_keeps_the_slot", RELEASED_ON_A_STALL)],
    },
    Control {
        // TASKS item 73: a waiting slot given back once the processor that stalled has answered,
        // with no fresh shootdown: its late answer may have served another page's request.
        name: "slot_freed_on_an_answer_without_a_fresh_shootdown",
        file: SLOTS,
        from: "                self.table.lock().set(slot, State::Reclaiming);\n                let outcome = shoot(slot, pages);\n",
        to: "                self.table.lock().set(slot, State::Reclaiming);\n                let outcome: Result<Completed, Stalled> = Ok(Completed { processors: 0, ticks: 0 });\n",
        models: &[(
            "slots_stalled_reclaim_is_given_back_only_after_a_fresh_shootdown",
            WAITING_REUSED,
        )],
    },
    Control {
        // TASKS item 73, Q2: two reclaims at once.
        name: "the_reclaim_role_never_refused",
        file: SLOTS,
        from: "        if self.reclaiming.load(Ordering::Relaxed) {\n            return None;\n        }\n        #[cfg(all(test, not(loom)))]\n        self.role_tries.fetch_add(1, Ordering::Relaxed);\n        self.reclaiming\n            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)\n            .ok()\n            .map(|_| Role(&self.reclaiming))\n",
        to: "        let _ = self.reclaiming.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed);\n        Some(Role(&self.reclaiming))\n",
        models: &[
            ("slots_two_reclaimers_and_a_waiting_slot", NEVER_GIVEN_BACK),
            ("slots_at_most_one_slot_out_of_the_tables", TWO_OUT),
        ],
    },
    Control {
        // TASKS item 73, Q2: a Retired slot unmapped while another waits.
        name: "a_slot_unmapped_while_one_waits",
        file: SLOTS,
        from: "        if let Some(w) = &self.waiting {\n",
        to: "        if let Some(w) = self.waiting.as_ref().filter(|_| false) {\n",
        models: &[("slots_at_most_one_slot_out_of_the_tables", TWO_OUT)],
    },
    Control {
        // TASKS item 73, Q1: a processor counted as having answered before it served.
        name: "answered_before_the_processor_served",
        file: SHOOTDOWN,
        from: ".is_none_or(|served| served.load(Ordering::Acquire) >= stall.generation)",
        to: ".is_none_or(|served| served.load(Ordering::Acquire) + 1 >= stall.generation)",
        models: &[(
            "shootdown_answered_only_after_the_stalled_processor_served",
            STALE,
        )],
    },
    Control {
        // S3-5b's review, finding A, case a, as the code was: a change returns when the flush
        // that should make an earlier stall good stalls again, with nothing of it made.
        name: "change_stops_at_a_stalled_first_flush",
        file: SHOOTDOWN,
        from: "            Err(_) => true,
        };
        rounds.prepare()?;
",
        to: "            Err(stall) => return Err(stall.into()),
        };
        rounds.prepare()?;
",
        models: &[(FIRST_FLUSH_STALLS, UNFINISHED)],
    },
    Control {
        // Finding A, case b, as the code was: a change returns at the first round whose
        // shootdown stalls, and the rounds after it are never made.
        name: "change_stops_at_a_stalled_round",
        file: SHOOTDOWN,
        from: "                    // Kept, with what every later round takes, until the flush at the end.
                    Err(_) => stalled = true,
",
        to: "                    Err(stall) => {
                        rounds.lost();
                        return Err(stall.into());
                    }
",
        models: &[(ROUND_STALLS, UNFINISHED)],
    },
    Control {
        // A round whose shootdown stalled gives back what it took at once.
        name: "change_gives_back_a_stalled_round_at_once",
        file: SHOOTDOWN,
        from: "                    Err(_) => stalled = true,
",
        to: "                    Err(_) => {
                        rounds.shot();
                        stalled = true;
                    }
",
        models: &[(ROUND_STALLS, GIVEN_BACK)],
    },
    Control {
        // The flush at the end of a change stalls, and what the change kept is given back anyway.
        name: "change_gives_back_what_a_stalled_flush_left",
        file: SHOOTDOWN,
        from: "                Err(stall) => {
                    rounds.lost();
",
        to: "                Err(stall) => {
                    rounds.flushed();
",
        models: &[(ROUND_STALLS, GIVEN_BACK), (FIRST_FLUSH_STALLS, GIVEN_BACK)],
    },
    Control {
        // A change that returns a stall leaves its space confirmed, so the next change would
        // not flush first.
        name: "change_forgets_its_stall",
        file: SHOOTDOWN,
        from: "                    rounds.lost();
                    return Err(refused.unwrap_or_else(|| stall.into()));
",
        to: "                    rounds.lost();
                    self.unconfirmed.store(false, Ordering::Relaxed);
                    return Err(refused.unwrap_or_else(|| stall.into()));
",
        models: &[(ROUND_STALLS, FORGOTTEN), (FIRST_FLUSH_STALLS, FORGOTTEN)],
    },
    Control {
        // The bind reads the signals before it takes the source's lock: a raise in between
        // delivers to nothing, and the binding is armed after it, for a signal already raised.
        name: "bind_reads_the_signals_before_the_lock",
        file: PORTS,
        from: "        let done = scheduler.without_interrupts(move || {
            let mut arms = source.state.lock();
            if !still_open() {
                return Done::Refused(Refused::HandleClosed, stored, way_back, way_on);
            }
            let observed = signals();
",
        to: "        let early = signals();
        let done = scheduler.without_interrupts(move || {
            let mut arms = source.state.lock();
            if !still_open() {
                return Done::Refused(Refused::HandleClosed, stored, way_back, way_on);
            }
            let observed = early;
",
        models: &[("ports_bind_against_raise", PORT_LOST)],
    },
    Control {
        // The bind asks whether the handle is open before it takes the source's lock: a close in
        // between finds nothing armed to cancel, and the binding is armed with its handle gone.
        name: "bind_asks_the_handle_before_the_lock",
        file: PORTS,
        from: "            let mut arms = source.state.lock();
            if !still_open() {
",
        to: "            let open = still_open();
            let mut arms = source.state.lock();
            if !open {
",
        models: &[(
            "ports_bind_against_a_close_of_the_bound_handle",
            PORT_STILL_ARMED,
        )],
    },
    Control {
        // The bind does not look at whether the port has closed: a binding whose signal is
        // already raised queues its packet in a closed port.
        name: "bind_ignores_a_closed_port",
        file: PORTS,
        from: "            if state.closed || !counted {
                return Done::Refused(Refused::PortClosed, stored, way_back, way_on);
            }
",
        to: "            let _ = counted;\n",
        models: &[("ports_close_against_a_bind_that_queues", PORT_CLOSED)],
    },
    Control {
        // An explicit cancel takes the armed binding but leaves a packet already queued.
        name: "explicit_cancel_leaves_a_queued_packet",
        file: PORTS,
        from: "            state.queue.remove_where(
                |store| store.source == number && store.key == key,
                &mut storage,
            );
",
        to: "",
        models: &[(
            "ports_raise_against_an_explicit_cancel",
            &[
                "an explicit cancel did not take its binding back once",
                "a packet from a cancelled binding",
            ],
        )],
    },
    Control {
        // The implicit cancel picks no binding: the one made through the closed handle stays
        // armed.
        name: "implicit_cancel_picks_nothing",
        file: PORTS,
        from: "        let (queued, canceled) = self.fire(scheduler, Kind::Canceled, signals, |store, _| {
            store.owner.as_ref().is_some_and(&which)
        });
",
        to: "        let _ = &which;
        let (queued, canceled) = self.fire(scheduler, Kind::Canceled, signals, |_, _| false);
",
        models: &[
            ("ports_raise_against_an_implicit_cancel", PORT_STILL_ARMED),
            (
                "ports_bind_against_a_close_of_the_bound_handle",
                PORT_STILL_ARMED,
            ),
        ],
    },
    Control {
        // A close takes the port's queue and index but leaves each binding armed for the port
        // on its source, where a later raise queues a packet in the closed port.
        name: "close_leaves_the_armed_bindings",
        file: PORTS,
        from: "                way_back.disarm(scheduler, self, &mut storage);\n",
        to: "                let _ = way_back;\n",
        models: &[
            ("ports_close_against_raise", PORT_CLOSED),
            ("ports_close_against_a_bind_that_arms", PORT_CLOSED),
        ],
    },
    Control {
        // A delivery that finds the source's lock held gives up, as if the holder would see its
        // signal: the holder may have read the signals already.
        name: "delivery_gives_up_on_a_busy_source",
        file: PORTS,
        from: "            let mut arms = self.state.lock();
            if arms.armed.is_empty() {
",
        to: "            let Some(mut arms) = self.state.try_lock() else {
                return (0, Stack::new());
            };
            if arms.armed.is_empty() {
",
        models: &[("ports_two_raisers", PORT_LOST)],
    },
    Control {
        // A delivery queues the packet and wakes no thread waiting for one.
        name: "delivery_wakes_no_taker",
        file: PORTS,
        from: "        wake_ports(scheduler, &fired);\n",
        to: "",
        models: &[("ports_taker_against_a_delivery", DEADLOCK)],
    },
    Control {
        // A close wakes no thread waiting on the port.
        name: "close_wakes_no_waiter",
        file: PORTS,
        from: "        // Each finds the port closed.\n        self.wake(scheduler);\n",
        to: "",
        models: &[("ports_close_against_a_waiter", DEADLOCK)],
    },
    Control {
        // A reservation looks only at the index's free room, not at the room the other
        // reservations already keep: two count on the same last slot, and the second bind finds
        // the index full.
        name: "reservation_ignores_the_room_others_keep",
        file: PORTS,
        from: "                if state.index.room() > reserved {\n",
        to: "                if state.index.room() > 0 {\n",
        models: &[(
            "ports_two_reservations_race_for_the_last_slot",
            &["a bind found no room"],
        )],
    },
    Control {
        // The reservation does not look at the port's bound under the lock (ADR-0005 I1): two
        // binds that both looked before either reserved pass a bound of one together.
        name: "reserve_ignores_the_port_s_bound",
        file: PORTS,
        from: "                if state.index.used().saturating_add(reserved) >= self.bound {
                    return Err(Unreserved::Full);
                }
",
        to: "",
        models: &[(
            "ports_two_reservations_race_for_a_bound_of_one",
            &["two reservations passed a bound of one"],
        )],
    },
    Control {
        // Two reservations that both find no room both grow the index (the independent review
        // of TASKS 57, finding T1): the heap holds two new blocks at once, past the two the
        // kernel-wide bound counts for the port.
        name: "two_growers_at_once",
        file: PORTS,
        from: "                if state.growing {
                    return Ok(Look::Wait);
                }
",
        to: "",
        models: &[
            (
                "ports_two_reservations_race_for_a_bound_of_one",
                &["the index's blocks at once passed what the figure counts"],
            ),
            (
                "ports_two_reservations_race_to_grow_a_bound_of_two",
                &["the index's blocks at once passed what the figure counts"],
            ),
        ],
    },
    Control {
        // A growth that ends without waking whoever waits for it: the waiter stays blocked.
        name: "a_growth_wakes_no_one",
        file: PORTS,
        from: "        scheduler.without_interrupts(|| self.state.lock().growing = false);
        self.wake(scheduler);
",
        to: "        scheduler.without_interrupts(|| self.state.lock().growing = false);
",
        models: &[("ports_two_reservations_race_to_grow_a_bound_of_two", DEADLOCK)],
    },
    Control {
        // C1 (TASKS item 70): a reader that looks only at saved frames, as `stack_in_use` did. A
        // running context's stack is then in no record of the table.
        name: "contexts_reader_skips_homes",
        file: CONTEXTS,
        from: "            .any(|slot| inside(slot.home.load(Acquire)) || inside(slot.frame.load(Acquire)))\n",
        to: "            .any(|slot| inside(slot.frame.load(Acquire)))\n",
        models: &[
            ("contexts_switch_out_keeps_the_outgoing_stack_covered", CTX_UNCOVERED),
            ("contexts_switch_in_keeps_the_incoming_stack_covered", CTX_UNCOVERED),
        ],
    },
    Control {
        // C2: a home retired with a relaxed store: a reader that finds it gone need not see what
        // the processor did on the stack before it left.
        name: "contexts_retirement_relaxed",
        file: CONTEXTS,
        from: "        slot.home.store(0, Release);\n        Ok(())\n",
        to: "        slot.home.store(0, Relaxed);\n        Ok(())\n",
        models: &[(
            "contexts_an_ended_stack_is_free_only_after_its_last_use",
            CTX_LAST_TOUCH,
        )],
    },
    Control {
        // C3: the record moves at a switch, from the home of a running context to the frame of a
        // suspended one and back: the two records of T68-2. The incoming stack is uncovered
        // between its frame's take and its home's store. Since TASKS item 95 (ADR-0020 S5) the
        // outgoing frame is left in the leaving record rather than stored in its slot, so the
        // texts name `record_leaving`; the bug and its model are the same.
        name: "contexts_record_moves_at_a_switch",
        file: CONTEXTS,
        from: "        let outgoing = self.current.load(Relaxed);
        match self.slots.get(outgoing) {
            Some(_) if exiting => {
                let _ = self.ended.swap(outgoing, AcqRel);
            }
            Some(_) => self.record_leaving(outgoing, here),
",
        to: "        if let Some(slot) = self.slots.get(incoming) {
            slot.home.store(next, Release);
        }
        let outgoing = self.current.load(Relaxed);
        match self.slots.get(outgoing) {
            Some(_) if exiting => {
                let _ = self.ended.swap(outgoing, AcqRel);
            }
            Some(slot) => {
                self.record_leaving(outgoing, here);
                slot.home.store(0, Release);
            }
",
        models: &[(
            "contexts_switch_in_keeps_the_incoming_stack_covered",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // C4: the home retired in the exit's own switch, while the processor is still on the
        // stack it is leaving.
        name: "contexts_retired_before_leaving",
        file: CONTEXTS,
        from: "            Some(_) if exiting => {
                let _ = self.ended.swap(outgoing, AcqRel);
            }
",
        to: "            Some(slot) if exiting => slot.home.store(0, Release),
",
        models: &[(
            "contexts_an_ended_stack_is_free_only_after_its_last_use",
            CTX_LAST_TOUCH,
        )],
    },
    Control {
        // C5: a slot claimed by a load and a store: two claimers can take one slot, and one
        // context is lost.
        name: "contexts_claim_not_atomic",
        file: CONTEXTS,
        from: "        if self
            .home
            .compare_exchange(0, home, AcqRel, Acquire)
            .is_err()
        {
            return false;
        }
",
        to: "        if self.home.load(Acquire) != 0 {
            return false;
        }
        self.home.store(home, Release);
",
        models: &[(
            "contexts_publications_beside_a_switch_each_get_a_slot",
            CTX_LOST,
        )],
    },
    Control {
        // C6 (the coordinating session's ruling (c)): `ended` taken by a load, then cleared by a
        // swap whose value is dropped. An exit recorded between the two is wiped, and its home
        // is never retired. Since T70-1 every caller retires with interrupts off on its own
        // processor, so on a kernel no switch falls between the two; L5 runs its retirers as
        // threads, a caller that breaks that precondition, and C6 shows that the single swap
        // still covers it (the addendum to the review of TASKS item 70, T70-9).
        name: "contexts_ended_taken_in_two_steps",
        file: CONTEXTS,
        from: "        let ended = self.ended.swap(NONE, AcqRel);\n",
        to: "        let ended = self.ended.load(Acquire);\n        let _ = self.ended.swap(NONE, AcqRel);\n",
        models: &[(
            "contexts_two_retirers_lose_no_ended_context",
            CTX_NEVER_RETIRED,
        )],
    },
    Control {
        // BX1 (TASKS item 81): the count lowered before the release, the order `reap_closed` had
        // before TASKS item 81 (the review of TASKS item 79, T79-6): a settle on another processor
        // can find nothing pending while the release still holds what it frees. B1's alone: B2's
        // settle reaps and reads only once the ender has finished its reaps, releases included,
        // so no lowering before a release is ever seen there (B2 passes under BX1).
        name: "backlog_lowered_before_released",
        file: BACKLOG,
        from: "        let released = release();\n        self.count.fetch_sub(1, AcqRel);\n",
        to: "        self.count.fetch_sub(1, AcqRel);\n        let released = release();\n",
        models: &[("backlog_a_settle_sees_what_a_reap_freed", BACKLOG_EARLY)],
    },
    Control {
        // BX4 (the review of TASKS items 75, 77 and 81, R81-1): the count raised after the list's
        // lock is released, as `list_closed` did: a taker can take the piece uncounted, and its
        // lowering cancel another piece's raise while that piece is still listed.
        name: "backlog_raised_after_the_lock",
        file: BACKLOG,
        from: "        self.count.fetch_add(1, AcqRel);\n        drop(listed);\n",
        to: "        drop(listed);\n        self.count.fetch_add(1, AcqRel);\n",
        models: &[(
            "backlog_a_lowering_cannot_cancel_another_pieces_raise",
            BACKLOG_EARLY,
        )],
    },
    Control {
        // BX2: the count read Relaxed: a settle that reads 0 is not ordered after the releases.
        name: "backlog_pending_relaxed",
        file: BACKLOG,
        from: "        self.count.load(Acquire)\n",
        to: "        self.count.load(core::sync::atomic::Ordering::Relaxed)\n",
        models: &[("backlog_a_settle_sees_what_a_reap_freed", BACKLOG_EARLY)],
    },
    Control {
        // BX3: the count lowered Relaxed: the lowering heads no release sequence, so a settle
        // that reads it is not ordered after the release before it.
        name: "backlog_drained_relaxed",
        file: BACKLOG,
        from: "        self.count.fetch_sub(1, AcqRel);\n",
        to: "        self.count.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);\n",
        models: &[("backlog_a_settle_sees_what_a_reap_freed", BACKLOG_EARLY)],
    },
    Control {
        // KX1 (TASKS item 77): the answer carries the outgoing context's home, so the trap entry
        // would name the stack of the context it just suspended.
        name: "contexts_switch_names_the_outgoing_home",
        file: CONTEXTS,
        from: "        let home = self.slots.get(incoming).map(|slot| slot.home.load(Acquire));\n",
        to: "        let home = self.slots.get(outgoing).map(|slot| slot.home.load(Acquire));\n",
        models: &[("contexts_a_switch_names_the_incoming_home", CTX_WRONG_HOME)],
    },
    Control {
        // KX2: the answer carries `next` as the home: right on a context's first run, where its
        // frame is its home, and wrong once it resumes at a frame it saved, or for the boot flow.
        name: "contexts_switch_names_next_as_home",
        file: CONTEXTS,
        from: "        let home = self.slots.get(incoming).map(|slot| slot.home.load(Acquire));\n",
        to: "        let home = Some(next);\n",
        models: &[("contexts_a_switch_names_the_incoming_home", CTX_WRONG_HOME)],
    },
    Control {
        // KX3: the answer carries the first slot's home, whichever context runs.
        name: "contexts_switch_names_the_first_slot",
        file: CONTEXTS,
        from: "        let home = self.slots.get(incoming).map(|slot| slot.home.load(Acquire));\n",
        to: "        let home = self.slots.first().map(|slot| slot.home.load(Acquire));\n",
        models: &[("contexts_a_switch_names_the_incoming_home", CTX_WRONG_HOME)],
    },
    Control {
        // KX4: the trap entry's check reads the home of the first slot, not of `current`'s: a
        // landing on another context's stack could pass it.
        name: "contexts_running_home_reads_the_first_slot",
        file: CONTEXTS,
        from: "            .get(self.current.load(Relaxed))\n            .map(|slot| slot.home.load(Acquire))\n",
        to: "            .first()\n            .map(|slot| slot.home.load(Acquire))\n",
        models: &[("contexts_a_switch_names_the_incoming_home", CTX_WRONG_HOME)],
    },
    Control {
        // WX1 (TASKS item 75): a withdrawal that takes the first frame by a plain store, not by
        // the compare-and-swap a switch also makes: a switch that resumed the context and the
        // withdrawal can both succeed, and the running context's home is cleared.
        name: "contexts_withdrawal_takes_the_frame_by_a_store",
        file: CONTEXTS,
        from: "                && slot
                    .frame
                    .compare_exchange(frame, 0, AcqRel, Acquire)
                    .is_ok()
",
        to: "                && {
                    slot.frame.store(0, Release);
                    true
                }
",
        models: &[(
            "contexts_a_withdrawal_beside_a_switch_and_a_publication",
            CTX_WITHDRAWN,
        )],
    },
    Control {
        // WX2 (TASKS item 75; the coordinating session's addition (b)): the home cleared before
        // the first frame is taken. A publisher can claim the slot in between and have its frame
        // overwritten, and a context a switch resumed loses its home.
        name: "contexts_withdrawal_clears_the_home_first",
        file: CONTEXTS,
        from: "        let Some(slot) = self.slots.iter().find(|slot| {
            slot.home.load(Acquire) == frame
                && slot
                    .frame
                    .compare_exchange(frame, 0, AcqRel, Acquire)
                    .is_ok()
        }) else {
            return false;
        };
        slot.home.store(0, Release);
        true
",
        to: "        let Some(slot) = self.slots.iter().find(|slot| slot.home.load(Acquire) == frame) else {
            return false;
        };
        slot.home.store(0, Release);
        slot.frame.compare_exchange(frame, 0, AcqRel, Acquire).is_ok()
",
        models: &[(
            "contexts_a_withdrawal_beside_a_switch_and_a_publication",
            CTX_WITHDRAWN,
        )],
    },
    Control {
        // X1 (TASKS item 79): the end published before it is counted as ending, as `settle` was
        // before TASKS item 79 for every end: a settle that sees the end can find nothing
        // pending while the thread still holds everything.
        name: "exits_counted_after_publishing",
        file: EXITS,
        from: "        self.ending.fetch_add(1, AcqRel);\n        (Mark { exits: self }, publish())\n",
        to: "        let published = publish();\n        self.ending.fetch_add(1, AcqRel);\n        (Mark { exits: self }, published)\n",
        models: &[("exits_a_settle_sees_what_an_ended_thread_held", EXITS_EARLY)],
    },
    Control {
        // X2: the exit lowers `ending` before it counts the zombie: between the two, nothing is
        // pending.
        name: "exits_ending_lowered_before_the_zombie_counts",
        file: EXITS,
        from: "        self.zombies.fetch_add(1, AcqRel);
        if let Some(mark) = mark {
            mark.lower();
        }
",
        to: "        if let Some(mark) = mark {
            mark.lower();
        }
        self.zombies.fetch_add(1, AcqRel);
",
        models: &[
            ("exits_a_settle_sees_what_an_ended_thread_held", EXITS_EARLY),
            (
                "exits_two_threads_on_one_processor_each_count_for_themselves",
                EXITS_EARLY,
            ),
        ],
    },
    Control {
        // X3: the settle reads `zombies` before `ending`: it can read no zombie before the exit
        // counts one, and no ending thread after the exit lowers it.
        name: "exits_settle_reads_zombies_first",
        file: EXITS,
        from: "    let ending = each()
        .map(|exits| exits.ending.load(Acquire))
        .fold(0, u64::saturating_add);
    let zombies = each()
        .map(|exits| exits.zombies.load(Acquire))
        .fold(0, u64::saturating_add);
",
        to: "    let zombies = each()
        .map(|exits| exits.zombies.load(Acquire))
        .fold(0, u64::saturating_add);
    let ending = each()
        .map(|exits| exits.ending.load(Acquire))
        .fold(0, u64::saturating_add);
",
        models: &[
            ("exits_a_settle_sees_what_an_ended_thread_held", EXITS_EARLY),
            ("exits_a_settle_over_two_processors_sees_both", EXITS_EARLY),
        ],
    },
    Control {
        // X4: the reaper counts the zombie gone before it gives back what it held: T70-7's order
        // undone (the review of TASKS item 70).
        name: "exits_reaped_before_released",
        file: EXITS,
        from: "        let released = release();\n        self.zombies.fetch_sub(1, AcqRel);\n        released\n",
        to: "        self.zombies.fetch_sub(1, AcqRel);\n        release()\n",
        models: &[("exits_a_settle_sees_what_an_ended_thread_held", EXITS_EARLY)],
    },
    Control {
        // X5: the settle reads `ending` with Relaxed: reading the exit's lowering then orders
        // nothing, and its read of `zombies` may miss the zombie the exit counted first.
        name: "exits_ending_read_relaxed",
        file: EXITS,
        from: "        .map(|exits| exits.ending.load(Acquire))\n",
        to: "        .map(|exits| exits.ending.load(core::sync::atomic::Ordering::Relaxed))\n",
        models: &[("exits_a_settle_sees_what_an_ended_thread_held", EXITS_EARLY)],
    },
    Control {
        // X6 (the review of TASKS item 79, T79-1): an exit that brought no mark lowers `ending`
        // all the same, so another thread's lowering can stand for the ending thread's.
        name: "exits_lowered_without_a_mark",
        file: EXITS,
        from: "        if let Some(mark) = mark {
            mark.lower();
        }
",
        to: "        match mark {
            Some(mark) => mark.lower(),
            None => {
                self.ending.fetch_sub(1, AcqRel);
            }
        }
",
        models: &[(
            "exits_an_exit_without_a_mark_cannot_stand_for_one",
            EXITS_EARLY,
        )],
    },
    Control {
        // MX1 (TASKS item 95; ADR-0020 S4's control for MV1): no move sequence. A pass that found
        // nothing is believed at once, as a scan of claim-before-retire alone would be, so a pass
        // that reads the destination before the claim and the source after the forget misses the
        // home.
        name: "moves_no_move_sequence",
        file: CONTEXTS,
        from: "        finished == begun && self.begun.load(Acquire) == begun
",
        to: "        let _ = (finished, begun);
        true
",
        models: &[(
            "moves::mv1_a_scan_reading_the_destination_first_finds_a_moving_home",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // MX2 (MV2's and MV2b's control): `lend` takes the frame by a plain store, not by the
        // compare-and-swap a switch also makes, so the source's switch and the move can both take
        // it.
        name: "moves_lend_by_a_plain_store",
        file: CONTEXTS,
        from: "            if context
                .frame
                .compare_exchange(frame, 0, AcqRel, Acquire)
                .is_ok()
            {
                return Ok(Lent {
",
        to: "            if {
                context.frame.store(0, Release);
                true
            } {
                return Ok(Lent {
",
        models: &[
            ("moves::mv2_a_move_and_the_source_switch_take_a_frame_once", MOVE_TAKEN_TWICE),
            (
                "moves::mv2b_a_move_and_a_switch_back_to_the_leaving_context_take_a_frame_once",
                MOVE_TAKEN_TWICE,
            ),
        ],
    },
    Control {
        // MX3 (MV3's control): the destination adopts the context before the source lends it, so
        // for a moment both tables hold the frame and both processors can resume it.
        name: "moves_adopt_before_lend",
        file: CONTEXTS,
        from: "        let lent = source
            .lend(&moving, frame, same_stack)
            .map_err(MoveRefused::Lend)?;
        let adopted = match destination.adopt(&moving, lent) {
            Ok(adopted) => adopted,
            Err((refused, lent)) => {
                return if lent.restore() {
                    Err(MoveRefused::Adopt(refused))
                } else {
                    Err(MoveRefused::NotRestored)
                };
            }
        };
",
        // The frame is adopted as if lent (a home equal to the frame: the model's context never
        // ran), and only then lent; the source's home is forgotten as `relocate` does.
        to: "        let early = Lent {
            table: source,
            slot: usize::MAX,
            home: frame,
            frame,
            settled: true,
        };
        let Ok(early) = destination.adopt(&moving, early) else {
            return Err(MoveRefused::NotRestored);
        };
        let mut lent = source
            .lend(&moving, frame, same_stack)
            .map_err(MoveRefused::Lend)?;
        lent.settled = true;
        let adopted = Adopted {
            source,
            slot: lent.slot,
            home: lent.home,
        };
        drop((early, lent));
",
        models: &[("moves::mv3_only_one_processor_resumes_a_moving_context", MOVE_TAKEN_TWICE)],
    },
    Control {
        // MX4a (MV4's control; the coordinating session's ruling on Q6 of TASKS item 95): a claim
        // made by a Relaxed load and a store, not a compare-and-swap. A scan that reads the new
        // claim in the freed slot is then not ordered after the forget, so its last reading of
        // begun may miss the move. ADR-0020 S4's own control for MV4, finished raised before
        // forget, was run and not caught, exhaustively too: it is not a bug (S4's dated note of
        // 2026-11-21).
        name: "moves_claim_checked_by_a_relaxed_load",
        file: CONTEXTS,
        from: "        if self
            .home
            .compare_exchange(0, home, AcqRel, Acquire)
            .is_err()
        {
            return false;
        }
",
        to: "        if self.home.load(Relaxed) != 0 {
            return false;
        }
        self.home.store(home, Release);
",
        models: &[("moves::mv4_a_forgotten_slot_claimed_again_hides_no_moved_home", CTX_UNCOVERED)],
    },
    Control {
        // MX4b (MV4's control, as MX4a): the forget clears the home with a Relaxed store, so a scan
        // that finds the home gone is not ordered after the move's raise of begun.
        name: "moves_forget_relaxed",
        file: CONTEXTS,
        from: "        if let Some(slot) = self.source.slots.get(self.slot) {
            slot.home.store(0, Release);
        }
",
        to: "        if let Some(slot) = self.source.slots.get(self.slot) {
            slot.home.store(0, Relaxed);
        }
",
        models: &[("moves::mv4_a_forgotten_slot_claimed_again_hides_no_moved_home", CTX_UNCOVERED)],
    },
    Control {
        // MX4c (MV4's control, the independent reviewer's answer to Q6): finished raised after
        // `lend` and before `adopt`. A scan that reads that finish is not ordered after the
        // destination's claim, so it can read the destination before the claim, the source after
        // the forget, and believe the pass.
        name: "moves_finished_before_adopt",
        file: CONTEXTS,
        from: "        let adopted = match destination.adopt(&moving, lent) {
",
        to: "        self.raise_finished();
        let adopted = match destination.adopt(&moving, lent) {
",
        models: &[("moves::mv4_a_forgotten_slot_claimed_again_hides_no_moved_home", CTX_UNCOVERED)],
    },
    Control {
        // MX5 (MV5's control): a refused adoption does not put the frame back, by `restore` or by
        // the `Lent`'s drop: its context can never be resumed, at its source or anywhere.
        name: "moves_restore_omitted",
        file: CONTEXTS,
        from: "    fn put_back(&self) -> bool {
        self.table.slots.get(self.slot).is_some_and(|slot| {
            slot.frame
                .compare_exchange(0, self.frame, AcqRel, Acquire)
                .is_ok()
        })
    }
",
        to: "    fn put_back(&self) -> bool {
        let _ = (self.table, self.slot, self.frame);
        true
    }
",
        models: &[("moves::mv5_a_full_destination_gives_the_frame_back", MOVE_NOT_GIVEN_BACK)],
    },
    Control {
        // MX6 (MV6's control): one counter whose parity marks a move under way, raised before the
        // gate is taken. Two movers raise it to an even value together, and a scan then believes a
        // pass that missed the home the first one moved.
        name: "moves_one_counter_by_parity",
        file: CONTEXTS,
        from: "    /// A move's start: the gate, then begun.
    fn enter(&self) -> GateGuard<'_> {
        let gate = self.gate.lock();
        let _ = self.begun.fetch_add(1, Relaxed);
        gate
    }

    /// A move's end, before the gate is released: finished, with Release.
    fn raise_finished(&self) {
        let _ = self.finished.fetch_add(1, Release);
    }

    /// What a scan reads before its pass: finished, then begun.
    fn readings(&self) -> (u64, u64) {
        (self.finished.load(Acquire), self.begun.load(Acquire))
    }

    /// Whether a pass that found nothing can be believed: no move was under way when it began
    /// (finished and begun equal), and none began during it (begun unchanged).
    fn settled(&self, (finished, begun): (u64, u64)) -> bool {
        finished == begun && self.begun.load(Acquire) == begun
    }
",
        to: "    /// A move's start: the counter, then the gate.
    fn enter(&self) -> GateGuard<'_> {
        let _ = self.begun.fetch_add(1, Relaxed);
        self.gate.lock()
    }

    /// A move's end: the same counter again.
    fn raise_finished(&self) {
        let _ = self.begun.fetch_add(1, Release);
    }

    /// One counter.
    fn readings(&self) -> (u64, u64) {
        (0, self.begun.load(Acquire))
    }

    /// Even: no move under way.
    fn settled(&self, (_, begun): (u64, u64)) -> bool {
        begun % 2 == 0 && self.begun.load(Acquire) == begun
    }
",
        models: &[("moves::mv6_two_movers_and_a_scan_miss_neither_home", CTX_UNCOVERED)],
    },
    Control {
        // MX7 (MV7's control): the scan's pass under the gate removed. After its fast passes have
        // all overlapped a move, it gives up and answers "not covered" for a home it never saw.
        name: "moves_scan_without_its_gated_pass",
        file: CONTEXTS,
        from: "        let covered = quiet.quietly(|| {
            let _gate = self.gate.lock();
            pass()
        });
",
        to: "        let _ = quiet;
        let covered = false;
",
        models: &[(
            "moves::mv7_a_scan_racing_back_to_back_moves_ends_and_finds_the_home",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // MX8 (the review of TASKS item 95, R95-7 (c)): finished raised with Relaxed. A scan that
        // reads it is not ordered after the destination's claim.
        name: "moves_finished_relaxed",
        file: CONTEXTS,
        from: "        let _ = self.finished.fetch_add(1, Release);
",
        to: "        let _ = self.finished.fetch_add(1, Relaxed);
",
        models: &[(
            "moves::mv1_a_scan_reading_the_destination_first_finds_a_moving_home",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // MX9 (R95-7 (d)): a pass believed whenever begun did not change across it, finished
        // unread. A move whose raise of begun the scan already counted, but whose claim it does not
        // yet see, is missed.
        name: "moves_settled_without_finished",
        file: CONTEXTS,
        from: "        finished == begun && self.begun.load(Acquire) == begun
",
        to: "        let _ = finished;
        self.begun.load(Acquire) == begun
",
        models: &[(
            "moves::mv1_a_scan_reading_the_destination_first_finds_a_moving_home",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // MX10 (R95-7 (e)): begun raised after `lend` rather than before it. A frame on a stack
        // that is not its home is in neither table between `lend` and `adopt`, and a scan that
        // reads the source's frame word after the take is then not ordered after the raise.
        name: "moves_begun_after_lend",
        file: CONTEXTS,
        from: "        let moving = self.begin(off);
        let lent = source
            .lend(&moving, frame, same_stack)
            .map_err(MoveRefused::Lend)?;
",
        to: "        let moving = self.begin(off);
        let _ = self.begun.fetch_sub(1, Relaxed);
        let lent = source
            .lend(&moving, frame, same_stack)
            .map_err(MoveRefused::Lend)?;
        let _ = self.begun.fetch_add(1, Relaxed);
",
        models: &[("moves::mv8_a_frame_off_its_home_stack_is_found_while_it_moves", CTX_UNCOVERED)],
    },
    Control {
        // LX1 (LV1's control; ADR-0020 S5): the outgoing frame stored in its slot at the switch, as
        // before TASKS item 95. Another processor can then take and resume it while its own
        // processor still returns along its stack.
        name: "moves_frame_published_at_the_switch",
        file: CONTEXTS,
        from: "            Some(_) => self.record_leaving(outgoing, here),
",
        to: "            Some(slot) => slot.frame.store(here, Release),
",
        models: &[(
            "moves::lv1_a_frame_is_resumed_elsewhere_only_after_its_processor_left_the_stack",
            MOVE_RESUMED_TOO_SOON,
        )],
    },
    Control {
        // LX4 (R95-7 (b)): the publication stores the frame in its slot with Relaxed: a mover that
        // takes it is not ordered after the processor's last use of the stack.
        name: "moves_publication_relaxed",
        file: CONTEXTS,
        from: "        if let Some(slot) = self.slots.get(self.leaving_slot.load(Relaxed)) {
            slot.frame.store(frame, Release);
        }
",
        to: "        if let Some(slot) = self.slots.get(self.leaving_slot.load(Relaxed)) {
            slot.frame.store(frame, Relaxed);
        }
",
        models: &[(
            "moves::lv1_a_frame_is_resumed_elsewhere_only_after_its_processor_left_the_stack",
            MOVE_RESUMED_TOO_SOON,
        )],
    },
    Control {
        // LX2a (LV2's control; the boot control C95-B1 plants the same): `covers` does not read the
        // leaving record, so a frame on a stack that is not its home is uncovered from its switch-
        // out to its publication.
        name: "moves_covers_skips_the_leaving_record",
        file: CONTEXTS,
        from: "        if inside(self.leaving.load(Acquire)) {
            return true;
        }
",
        to: "",
        models: &[(
            "moves::lv2_a_frame_is_covered_while_its_processor_publishes_it",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // LX2b (LV2's control): `covers` reads the leaving record after the slots: a publication
        // between the two reads moves the frame from the record to its slot behind the reader.
        name: "moves_covers_reads_the_leaving_record_last",
        file: CONTEXTS,
        from: "        if inside(self.leaving.load(Acquire)) {
            return true;
        }
        self.slots
            .iter()
            .any(|slot| inside(slot.home.load(Acquire)) || inside(slot.frame.load(Acquire)))
",
        to: "        self.slots
            .iter()
            .any(|slot| inside(slot.home.load(Acquire)) || inside(slot.frame.load(Acquire)))
            || inside(self.leaving.load(Acquire))
",
        models: &[(
            "moves::lv2_a_frame_is_covered_while_its_processor_publishes_it",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // LX3 (R95-7 (a)): the publication clears the leaving record before it stores the frame in
        // its slot: a reader between the two finds the frame in neither.
        name: "moves_publication_clears_first",
        file: CONTEXTS,
        from: "        if let Some(slot) = self.slots.get(self.leaving_slot.load(Relaxed)) {
            slot.frame.store(frame, Release);
        }
        self.leaving.store(0, Release);
",
        to: "        self.leaving.store(0, Release);
        if let Some(slot) = self.slots.get(self.leaving_slot.load(Relaxed)) {
            slot.frame.store(frame, Release);
        }
",
        models: &[(
            "moves::lv2_a_frame_is_covered_while_its_processor_publishes_it",
            CTX_UNCOVERED,
        )],
    },
    Control {
        // LX5 (LV3's control; TASKS item 96, the coordinating session's ruling Q2): the switch
        // stores `current` Relaxed. A mover that reads the new `current` is not ordered after the
        // leaving record the switch wrote before it, so it can find the frame in neither the
        // record nor its slot.
        name: "moves_current_stored_relaxed",
        file: CONTEXTS,
        from: "        self.current.store(incoming, Release);
",
        to: "        self.current.store(incoming, Relaxed);
",
        models: &[(
            "moves::lv3_a_frame_being_switched_out_is_never_called_lost",
            MOVE_CALLED_LOST,
        )],
    },
    Control {
        // LX6 (LV3's control): `lend` reads the leaving record before `current`. The record can
        // then be older than `current`: read before the switch, with `current` read after it, the
        // frame is in neither the record it read nor its slot.
        name: "moves_lend_reads_the_leaving_record_first",
        file: CONTEXTS,
        from: "        if self
            .slots
            .get(self.current.load(Acquire))
            .is_some_and(|running| same_stack(running.home.load(Acquire), frame))
        {
            return Err(LendRefused::NotYet);
        }
        if self.leaving.load(Acquire) == frame {
            return Err(LendRefused::Leaving);
        }
",
        to: "        let leaving = self.leaving.load(Acquire);
        if self
            .slots
            .get(self.current.load(Acquire))
            .is_some_and(|running| same_stack(running.home.load(Acquire), frame))
        {
            return Err(LendRefused::NotYet);
        }
        if leaving == frame {
            return Err(LendRefused::Leaving);
        }
",
        models: &[(
            "moves::lv3_a_frame_being_switched_out_is_never_called_lost",
            MOVE_CALLED_LOST,
        )],
    },
    Control {
        // MX11 (D95-2): a scan's last pass without the gate: it may overlap a move.
        name: "moves_last_pass_without_the_gate",
        file: CONTEXTS,
        from: "            let _gate = self.gate.lock();
            pass()
",
        to: "            pass()
",
        models: &[(
            "moves::mv9_the_gate_admits_one_holder_at_a_time_and_a_scans_last_pass_is_one",
            MOVE_GATE,
        )],
    },
    Control {
        // MX12 (D95-2): the gate's waiter reads the ticket served with Relaxed: admitted, it need
        // not see what the holder before it wrote.
        name: "moves_gate_served_relaxed",
        file: CONTEXTS,
        from: "        while self.serving.load(Acquire) != ticket {
",
        to: "        while self.serving.load(Relaxed) != ticket {
",
        models: &[(
            "moves::mv9_the_gate_admits_one_holder_at_a_time_and_a_scans_last_pass_is_one",
            MOVE_GATE,
        )],
    },
    Control {
        // MX13 (D95-2): a ticket taken by a load and a store, not one fetch-and-add: two takers
        // can hold the same ticket, and both are admitted.
        name: "moves_gate_ticket_not_atomic",
        file: CONTEXTS,
        from: "        let ticket = self.next.fetch_add(1, Relaxed);
",
        to: "        let ticket = self.next.load(Relaxed);
        self.next.store(ticket.wrapping_add(1), Relaxed);
",
        models: &[(
            "moves::mv9_the_gate_admits_one_holder_at_a_time_and_a_scans_last_pass_is_one",
            MOVE_GATE,
        )],
    },
];

/// The scratch copy's workspace: just the crate under test. The tables `scratch_manifest` adds
/// come from the real workspace's manifest, so that they cannot drift from it.
const SCRATCH_WORKSPACE: &str = r#"[workspace]
resolver = "3"
members = ["framekernel/frame/sync"]

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
    let path = scratch.join(MOVES_MODELS);
    let moves = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let moves: Vec<String> = model_names(&moves)
        .into_iter()
        .map(|name| format!("{MOVES_MODULE}{name}"))
        .collect();
    let mut models = model_names(&source);
    models.extend(moves.iter().map(String::as_str));
    require_complete(&models, &report)?;
    controls(&root, &scratch, models.len())
}

/// The lines of `source` that use a `cfg` naming `loom` without also naming `test`.
fn loom_cfg_without_test(source: &str) -> Vec<usize> {
    // Comments may speak of the flag; code may split a predicate across lines.
    let code: String = source
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    /// The only predicates under which `loom` may appear: each requires `test` to select Loom's
    /// code, or is the exact opposite of one that does.
    const ALLOWED: [&str; 3] = [
        "all(test,loom)",
        "not(all(test,loom))",
        "all(test,not(loom))",
    ];
    let mut offending = Vec::new();
    for (at, _) in code.match_indices("cfg") {
        let rest = code.get(at + 3..).unwrap_or("");
        let attr = rest.starts_with("_attr");
        let after = rest
            .strip_prefix("_attr")
            .or_else(|| rest.strip_prefix('!'))
            .unwrap_or(rest)
            .trim_start();
        let Some(body) = after.strip_prefix('(') else {
            continue;
        };
        let (mut depth, mut predicate) = (1usize, String::new());
        for c in body.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if attr && depth == 1 => break,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            if !c.is_whitespace() {
                predicate.push(c);
            }
        }
        if predicate.contains("loom") && !ALLOWED.contains(&predicate.as_str()) {
            offending.push(code.get(..at).unwrap_or("").lines().count().max(1));
        }
    }
    offending
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

/// The models a models file defines: the name of the function after each `#[test]`. Those of
/// `MOVES_MODELS` are reported, and named in controls, with `MOVES_MODULE` before them.
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

/// The models that printed a search report (`check` in `models.rs`): the name ends at the first
/// colon and space, since a model of `MOVES_MODELS` is named with its module's path, `moves::`.
fn reported_models(report: &str) -> Vec<&str> {
    report
        .lines()
        .filter_map(|line| line.split_once("loom model ")?.1.split_once(": "))
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
            "these names in the Loom report are not models in models.rs or moves_models.rs (a \
             model reporting under another's name?): {}",
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
    let crate_dir = root.join("framekernel/frame/sync");
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
    let manifest = Path::new("framekernel/frame/sync/Cargo.toml");
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
            "members = [\"framekernel/frame/sync\"]",
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
        for name in ["PATH", "XLOOM_", "ORIGIN_XTASK_LOCK_HELD", "RUSTFLAGS"] {
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
        // What a line-by-line check let through: `test` merely beside `loom`, and a predicate
        // rustfmt split across lines. The last two are the forms the crate really uses.
        let tricky = "#[cfg(any(test, loom))]\n#[cfg(all(not(test), loom))]\n#[cfg(\n    all(\n        not(test),\n        loom\n    )\n)]\n#[cfg_attr(any(test, loom), allow(dead_code))]\n#[cfg(not(all(test, loom)))]\n#[cfg(all(test, not(loom)))]\n";
        assert_eq!(loom_cfg_without_test(tricky), [1, 2, 3, 9]);
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
        let moved =
            format!("{MOVES_MODULE}mv1_a_scan_reading_the_destination_first_finds_a_moving_home");
        let report = format!("loom model {moved}: 1802 executions, at most 3 preemptions\n");
        assert_eq!(reported_models(&report), [moved.as_str()]);
        assert!(require_complete(&[moved.as_str()], &report).is_ok());
        let uncontrolled = "loom model not_a_control: 1 executions\n";
        assert!(
            require_complete(&["not_a_control"], uncontrolled)
                .unwrap_err()
                .contains("no negative control")
        );
    }
}
