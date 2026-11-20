# Addendum review of TASKS item 66: the answers to REVIEW_T66_2026-11-19.md

- **Range:** `c0f0d02a..f700daa0` on `claude/t66-timing-proofs`, unpushed.
  - The last code commit is `875b8c07`.
  - `90ee371b` merges `main` at `2d0187c0`. It changes none of the files TASKS item 66 owns (`git diff 4c856ce3 90ee371b` on those files is empty).
- **Date:** Thu Nov 19 13:01 UTC 2026 (`date -u`).
- **Reviewer:** the same AI reviewer (Claude) as the first review, independent of the author session. No person reviewed this.
- **Read:**
  - The addendum hand-over `t66-addendum.md`.
  - The kernel, framework and records diff `c0f0d02a..f700daa0`.
  - The code around the diff at the tip: `sched.rs` `on_tick`, `run_window`, `judge_run`, `prove`, `worker`, `respawn_workers`; `runqueue.rs`; `apic.rs`; `timer.rs`; `stalls.rs`; `wait.rs`; `time.rs`.
  - ADR-0013's TASKS 66 section and its control table, `TASKS.md` entry 66, `SOURCE_PROVENANCE.md`, CL20 and the new research ledger.
  - The control and gate logs in `t66-evidence/review-answers.tar.gz`.
- **On ruling A:** `t66-rulings.md` in the coordination notes still holds only the 03:28Z rulings and has no ruling A. I judged ruling A from the coordinator's message and from its wording in ADR-0013 and `TASKS.md`.
- **Ran** in a fresh `git archive f700daa0` export, each command inside that copy's `cargo xtask exclusive --`:
  - `cargo test -p origin-frame --lib -- apic:: stalls::`: 24 passed, including my probe test, which I added to the export only.
  - `cargo test -p framekernel --lib -- runqueue:: rtc::`: 51 passed, including my probe test.
  - `cargo xtask loom`, with the probe files restored to the commit's bytes: all 56 models passed and all 83 negative controls were caught, in 316 s.
  - `cargo clippy -D warnings` as `check` runs it: `origin-sync --all-targets`, and `origin-frame` and `framekernel` with `--lib --tests`. All three were clean.
  - `cargo fmt --all -- --check` was clean.
  - Nothing booted. The author's tree was untouched. I read no project source outside Origin's repository.

**New findings: Medium 3, Low 3.** IDs continue from T66-R10.

---

## Status of the first review's findings

| ID | Status | Reason |
|---|---|---|
| T66-R1 (High) | **Closed, with limits and two new Lows** | See below. A Medium about the repeat's correctness clauses is new: T66-R11. |
| T66-R2 (Medium) | **Closed** | `yield_until_away_for(|| false, tsc_hz, tsc_hz / 50)` measures at the real rate. `away` is 0 when `far == this_cpu()`. The figure is printed on its own line. T66-21 fails at run 1 as claimed. |
| T66-R3 (Medium) | **Closed, one small gap** | `wait_for_others` prints every wait past its former bound. The run of turns past 500 ms is said, and the timed waits' wait past 4 s is said for the last run only (T66-R16). The records list the widened waits, and the control labels are corrected (ADR-0013:655). |
| T66-R4 (Low) | **Closed** | `judge_ticks` takes `min(wanted, MAX_TICK_STAMPS)` stamps, and a test covers it. |
| T66-R5 (Low) | **Partly closed** | The ten-gap rule catches a fast rate masked by a single stall. It does not catch one masked by recurring stalls, and the records overstate it: T66-R13. |
| T66-R6 (Low) | **Closed** | An interrupt window before `before` is read (`timer.rs:621`) drains an expiry left pending. At most one can be pending, since the timer is masked with a zero count. |
| T66-R7 (Low) | **Closed** | I measured every new line at its widest figures, and all are under 160 bytes. The worst are the tick failure line (155) and the last-run window line (150). The first timed-wait line (the text from before TASKS item 66) reaches 166 only with five-digit milliseconds and now carries no failure text, so a split cannot hide one. The short run prints its run's length. The reaper uses the boot-flow console on the boot flow. |
| T66-R8 (Low) | **Closed** | `stalls.rs` has four host tests, which pass. `settle`'s own reclaim reports through `reclaim_stacks`. The stacks proof's reclaims and user-space stalls are recorded as a departure. |
| T66-R9 (Low) | **Closed** | `take_turns` opens a window between turns when interrupts are off (`time.rs:643-649`, the check at 647). Readings stay under the lock with interrupts off. |
| T66-R10 (Low) | **Closed, one record stale** | Dates, the evidence pointer (now in-repository, `docs/research/2026-11-19-timing-proofs.md`), CL20 and the qualifier are fixed. But `SOURCE_PROVENANCE.md:5186` now claims tests that no longer exist: T66-R12. |

### T66-R1 under ruling A (the coordinator's questions)

- **What is compared.**
  - **What is read:** on every processor, at the first tick after each run's `arm_watch`, `on_tick` → `note_tick` → `timer_read_back` (`sched.rs:66-84`) reads that processor's own `TimerInitial`, `TimerDivide` and `LvtTimer`. The handler runs on the ticking processor and uses `X2Apic::this_cpu()`.
  - **When:** the boot processor programs its timer in `prove`, and every application processor programs its own in `run_ap` after `GO`, which is long after the boot processor's timed proof. All of these are read at their first watched tick, and again in every run, because each run re-arms the watch.
  - **What it is compared with:** the expected value is `timer::periodic_programming()` (`timer.rs:350-358`). That is `PERIOD`, `divide_config(DIVISOR)` and a periodic, unmasked entry at `TIMER_VECTOR`, recomputed from the same inputs the tick proof used: `count` was stored into `PERIOD` at `timer.rs:603`, `calibrate` wrote `divide_config(DIVISOR)`, and `take_ticks` wrote the same LVT entry.
  - **So the read side is real but the "as timed" side is synthetic.** It is equal by construction today, but it is not a read-back of what the timed run held: T66-R14.
- **Can a misprogrammed timer still pass?**
  - **Caught at once:** a register differing from those values, on any processor, including masked, one-shot or TSC-deadline mode, a wrong divisor or a wrong count.
  - **Caught as `Short`:** a timer that never ticks during the window (masked, count 0, or a one-shot that already fired), which fails at the 10 s budget.
  - **Passes:**
    - a uniformly wrong `PERIOD` giving 30 to 100 Hz, or any fast rate; the read-back then agrees, and the 30 Hz floor is the only time clause (T66-R14);
    - a reprogramming after the first watched tick.
  - **Combined with partial switching**, the switch bound is weaker than before: T66-R15.
- **The controls.**
  - T66-20 (divide by 128 on CPU 1), T66-20b (count ×8) and T66-20c (one-shot) each fail at run 1, ending the window in 31 to 91 ms with the register named. The logs in `controls10` (on `875b8c07`) match ADR-0013:648-650.
  - All three plant on CPU 1 only. None plants a uniform error that also moves `PERIOD`.
- **The rest of the window is sound:**
  - `judge_window` passes only with the read-back "as timed", 50 ticks or more and the 15th within 500 ms.
  - `Stalled` needs a gap of two periods. My probe of a 25 Hz tick that reads back as timed gives `Stalled` in every run, so it fails at run 4.
  - `Short` and `Late` fail.
  - A short processor at the budget now fails, where at `c0f0d02a` nothing judged it.

---

## T66-R11 (Medium): a stalled preemption run is repeated without judging its correctness clauses, and the next run's workers are started over any that did not exit

**Where:**
- `framekernel/src/sched.rs:1482-1500`: the loop.
- `:1251-1256`: `exited`.
- `:1267-1285`: `respawn_workers` sets `STOP` to false at 1268 and `EXITED` to 0 at 1269.
- `:1503-1523`: `exited`, `right_cpu`, `lo > 0` and `rounds`, judged once, after the loop.
- `:693-711`: `worker` spins until `STOP`, then adds to `EXITED`.

**What happens:**
- The loop decides on `judge_run`'s timing verdict alone. When a run is `Stalled` and runs remain, it respawns without looking at that run's correctness:
  - whether every worker exited (`run.exited`);
  - whether each worker ran on its own processor (`RAN_ON`);
  - whether both workers ran (`lo > 0`).
- `respawn_workers` then sets `STOP` back to false and `EXITED` to 0. A worker of the stalled run that had not yet seen `STOP` keeps spinning into the next run, under the same slot index, and writes the same `PROGRESS`/`RAN_ON` entries as its replacement. Its eventual exit counts toward the new run's `EXITED == workers`.

**Failing scenarios:**
1. **A worker that cannot exit is hidden.**
   - Run 1 is `Stalled`, and one worker on CPU n never gets to observe `STOP` within 10 s. That is a scheduler fault, or the unexplained "workers did not exit" the addendum reports at ten busy loops.
   - Run 2 spawns two more workers on CPU n while the old one still runs. At run 2's end the old worker's exit can stand in for a new worker that never exits, so `EXITED == workers` and `exited` is true.
   - The hand-over's "workers did not exit: scheduler failed" never prints. A thread is left alive into the following proofs, which compare heap and stack counts.
2. **A wrong-CPU or a starved worker in a stalled run is never judged.** If the fault is intermittent, a clean later run passes.

**What this contradicts:** the contract's security invariant (3), "A Refuse proof's correctness half is judged on every run" (`TASKS.md:2178`).

**Suggested fix:**
- Before repeating, require `run.exited` and, per processor, `right_cpu && lo > 0`. Otherwise fail at once.
- Never reset `EXITED` while a previous worker may live. For example, keep a run generation in each worker's argument and count exits per generation, or break and fail when `!run.exited`.
- Add a control: a worker that ignores `STOP` in run 1 of a stalled run.

**Confidence:** high, from the code. Not booted.

## T66-R12 (Medium): `4c856ce3` deleted every host test of R3's judgement; the records still claim them

**Where:**
- `framekernel/src/runqueue.rs` at `c0f0d02a`, lines 513-634: four tests.
  - `a_timed_wait_in_bounds_passes_and_its_edges_hold`
  - `a_late_wait_with_its_ticks_there_fails_at_once`
  - `a_late_wait_whose_processor_lost_ticks_is_measured_again`
  - `over_a_grid_only_a_wait_in_bounds_passes`
- At `f700daa0`, `judge_timed_waits` (`runqueue.rs:133`) has no test anywhere. `grep judge_timed_waits` finds only its definition and its call in `wait.rs:351`.
- `git show <c>:framekernel/src/runqueue.rs` counts 18 calls in the tests up to `bf073014`, and 1 from `4c856ce3` on.

**What happened:**
- The commit replaced the test module's first block with the window tests. The module's test count stayed 25, which hides the loss.
- `SOURCE_PROVENANCE.md:5186` still says "`runqueue::tests` (four of the same kind for the timed waits)".
- The review-answers entry (`:5211`) and `TASKS.md` do not record the deletion.
- The contract's "Required tests" (`TASKS.md:2180`) asks for host tests of R3's judgement at its edges and a grid.

**Status:** the function itself behaves as before. My probe gives 59/5 Pass, 81/8 Fail, 81/5 Repeat, 49 ms Fail, and the late long wait with away 100 Repeat. It is now unguarded.

**Fix:** restore the four tests from `c0f0d02a` (they compile against the unchanged `judge_timed_waits`), and add one for a late long wait with away 0 that must fail.

## T66-R13 (Medium): a fast tick masked by recurring stalls still passes the APIC timer proof (Refuse); the records and the code's own documentation say it cannot

**Where:**
- `framekernel/frame/src/apic.rs:340-346`: the doc says "a fast rate whose total a stall made look right never passes".
- `:370-392`: the verdict.
- `:565`: the test `a_fast_tick_masked_by_a_stall_never_passes`.
- `TASKS.md:2270`: "no longer passes".
- `TASKS.md:2273`: "a fast one's are all short".
- ADR-0013:608 and the research ledger: the limit stated as "5 % to 15 % fast".

**Probes** at `f700daa0`, with my added test:
- Twice the rate, with a stall of five periods in every tenth gap: `InBounds`. Total 500 ms, median 5 ms, least ten gaps 100 ms.
- 1.5× the rate, with stalls of 3.4 periods every tenth gap: `InBounds`.
- One stall: 10 % fast with one 50 ms stall gives `InBounds`, which is the stated limit.

**Why:**
- A stall lengthens every ten-gap window that contains it. Stalls spaced at most ten gaps apart leave no short window, so `least_ten` stays at 8.5 periods or more for any rate whose total the stalls bring into bounds.
- The stated "5 % to 15 %" holds for a single stall only.
- Under TCG with load, the addendum itself says "almost every run has a gap of two periods". On a real machine, a periodic SMI does the same.

**Also, from `66f47442`:**
- The slow-side `Stalled` branch no longer requires the median to be at least 95 % (`apic.rs:389`). So a fast rate slowed by a long stall is now repeated instead of failing at once: my probe of 2× with a 1.5 s stall gives `Stalled`, where at `c0f0d02a` it was `Wrong`.
- A repeat never passes, but each repeat is another chance for the recurring-stall pattern above.
- ADR-0013:598 and its APIC timer row (line 98) still say "with its median gap in bounds".

**The cheap check:** measure the timer's period without its interrupts.
- **How:** inside a passing run's window, read `TimerCurrent` twice within one period, each read bracketed by RDTSC, as the calibration windows bracket theirs. Retry a pair whose brackets a stall widened. The countdown per TSC tick, with the initial count, gives the period in TSC ticks directly. Require it within 95 % to 150 % of `tsc_hz / HZ`.
- **Why it suits this:**
  - Interrupt delivery and host stalls cannot move it; they only cost retries.
  - It needs a bracket of about 1 % of the interval, not calibration's 0.2 %, so far fewer pairs are spoiled.
  - It catches a wrong count, a wrong divisor, or an APIC-to-TSC ratio error, which are the sources of a fast tick.
  - It would not fail a correct timer under load.
- **Caveats:** untried; it wants a control. A fast rate from extra interrupts on the timer's vector is still the stamps' job.
- **The minimum:** state the limit accurately ("a fast rate whose total stalls brought into bounds, when stalls fall in every ten-gap window"), fix the doc, the test name and `TASKS.md:2270`, and correct ADR-0013:598 and row 98.

## T66-R14 (Low): ruling A's "as timed" is recomputed, not read from the timed run

**Where:**
- `framekernel/frame/src/timer.rs:350-358`: `periodic_programming`.
- `framekernel/src/sched.rs:76-84`: `timer_read_back`.

**The problem:**
- The expected programming is rebuilt from `PERIOD` and the constants, not taken from the boot processor's registers while `take_ticks` timed them.
- If `PERIOD` ever differed from the `count` the proof timed, every processor's read-back would agree with it and say "as timed". Examples: a wrong `PERIOD.store`, or `start_periodic` and `periodic_programming` changed together.
- At 30 to 100 Hz such a tick also meets the 15-tick floor, so nothing fails.
- The same applies if `calibrate`'s divide write and `start_periodic`'s ever diverge.

**Fix:**
- In `take_ticks`, read the three registers once while the passing run's timer is still programmed, and store that snapshot.
- Compare each processor's read-back with the snapshot.
- Add a control that plants `PERIOD.store(count * 2)`.

## T66-R15 (Low): "Every timing bound a passing run met before, it meets now" is not quite so for the preemption proof

**Where:** ADR-0013:608; `runqueue.rs:289-305`; `sched.rs:1521-1523`.

**The difference:**
- The old Refuse bound was 15 switches within 500 ms of TSC time.
- The new one is the 15th tick within 500 ms, plus 15 switches anywhere in the 50-tick window, which lasts up to 10 s.

**Failing scenario:**
- A processor ticking at 40 Hz with its timer reading back "as timed" (for example T66-R14's wrong `PERIOD`), whose scheduler switches only on every other tick.
- **Before:** 10 switches in 500 ms, so it failed.
- **Now:** the 15th tick comes at 375 ms and there are 25 switches in the window, so it passes.

**Fix:** stamp the TSC at the 15th switch, as `at_rounds` stamps the 15th tick, and bound that at 500 ms. The run queue already counts switches. Otherwise, qualify the sentence.

## T66-R16 (Low): only the last run of the timed waits says its wait went past 4 s

**Where:** `framekernel/src/wait.rs:360-366`.

**The gap:**
- `run.waited_ms` belongs to the run that decided.
- A repeated run whose wait for both waiters took more than 4 s prints nothing, though it can do so, since a repeat needs only that both returned.
- This is the only extension that is not always said.

**Fix:** print inside the loop, per run.

---

## Checked and found sound

- **Ruling A's read side:**
  - The three registers are read through the existing `X2Apic::read`, with no new `unsafe` block (only enum arms were added).
  - The masks follow the SDM's register layout: divide bits 0, 1 and 3; LVT vector, mask (bit 16) and mode (bits 17-18); the delivery status is excluded.
  - The read happens in the tick handler under the queue lock and takes no other lock. `PERIOD` is a single atomic load.
  - A misprogrammed timer ends the window at once (`settled`) and is named before `Short`.
  - The read-back runs on every processor, including the boot processor, on every run.
- **`875b8c07`, `runqueue::ReadBack`:**
  - It is plain `Copy` data inside `RunQueue`, behind the queue's lock, with no atomics and no `origin-frame` type.
  - The comparison moved to `sched::timer_read_back`, kernel-only.
  - It is sound for Loom: the 56 models and 83 controls pass in my export, including the three controls that edit `runqueue.rs`'s text, and `origin-sync --all-targets` Clippy is clean.
  - It is sound for the kernel: the verdict logic is the same as `4c856ce3`'s, with the comparison now made at tick time against `PERIOD`, which is set once.
- **T66-R5's ten-gap rule:**
  - It only adds `Stalled` to runs that were `InBounds`, so it introduces no new pass.
  - The "not shared with the next" filter can only drop windows that start at a double stamp. A fast timer seen in pairs still gives short spans: pairs a period apart span 5 periods.
  - The evidence (least spans 89 to 98 ms over 20 boots) supports 8.5 periods as a repeat threshold.
- **The `apic` edges:**
  - The first `wanted` stamps are used.
  - A median over 150 % is `Wrong` before the in-bounds branch.
  - Exactly 8.5 periods passes, and a span under it is `Stalled`.
  - Grid and probe tests pass.
- **Lines** (ruling condition 2):
  - I listed every distinct new-form line in the answer logs (40 forms, digits normalised) and checked them against xtask's literals.
  - The only lines with `: … failed` are real failures: `: scheduler failed` on the window line, `: APIC timer is wrong`, `: wait timing failed` and `: wait queues failed` on the timed-wait judgement line, and the existing settle line.
  - "stalled, run again" carries no failure word.
  - No line contains another marker's text.
  - The hex values printed are register contents on failure lines only, not addresses.
  - The window line begins `origin-kernel: scheduler: window`, so xtask's fair-share forgivers, which need `scheduler: CPU `, never forgive a failing window on the named machines.
  - The markers `APIC timer: 50 interrupts at 100 Hz in ` and the timed-wait fragments are still printed (the first timed-wait line kept its text).
- **Records:**
  - ADR-0013:591-663 states the rules as built, except for the two wording points in T66-R13.
  - The 26-row control table's quoted lines match the logs I checked: T66-2, 3, 5, 9, 9b, 10, 11 to 15, 17 to 19, 20, 20b, 20c, 21, 22 and 22b.
  - The bare-metal "a repeat is a finding" note is present (ADR-0013:612-622) and lists the proofs.
  - No record claims a KVM, CI or hardware result.
  - CL20 now gives per-run bounds.

## Not covered

- No boot. T66-R11, R13 (beyond host probes), R14 and R15 are reasoned, not booted.
- The TimerCurrent check suggested in T66-R13 is untried.
- The intermediate commits individually, except where a test's history was traced.
- `cargo xtask check` as a whole, the kernel-target build, `boot-test` and `boot-matrix`.
- The coordinator's ruling A text itself, which is absent from `t66-rulings.md`.
