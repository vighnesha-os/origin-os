# Independent review, addendum: TASKS item 70, the answers to `REVIEW_T70_2026-11-19.md`

- **What was reviewed:** branch `claude/sweet-wilbur-674acb`, range `380cb861..18ced439` (unpushed). The code alone is `380cb861..4775d00c`: `a42700ae`, `a2bb4146`, `103ae0fd`, `352ee282`, `927e699d` and `4775d00c`. In the merge `50ec0efb` (`main` at `2d0187c0`), I read only what it changes in the files TASKS 70 owns:
  - `contexts.rs`, `cpu.rs`, `stack.rs`, `sched.rs`, `stacks.rs` and `measured_stack.rs` are unchanged by it;
  - `loom.rs` takes `main`'s one-line test change;
  - the window markers in `main.rs` survived TASKS 61's refactor (`main.rs:3221`, `3225`).
- **Records read:** `18ced439`'s `TASKS.md`, `SOURCE_PROVENANCE.md` and ADR-0013, the hand-over `t70-answers-handover.md`, and the evidence logs `t70-evidence/answers-*`.
- **When:** 2026-11-19, from 10:17 UTC (`date -u`).
- **By whom:** the same AI reviewer as the first review (Claude Opus 5.5), independent of the author session and of the coordinating session. No person has reviewed this.
- **What I ran**, on an export of `18ced439` (`git archive`, every file touched) in my scratchpad, inside `cargo xtask exclusive` from that copy:
  - `cargo test -p origin-sync --lib contexts`: 13 passed, 0 failed.
  - `cargo xtask loom` (10:21 to 10:27 UTC): all 61 models passed and all 89 negative controls were caught (123 model runs). L5 took 24 executions with the new load before the swap, and C1 to C6 were each caught by the model the records name.
  - From the answers' evidence, I tabulated the 29 window lines of `answers-gate-boot-matrix-50ec0efb.log` and the one of `boot-test`. Holds were at most 154 and 46 µs, readiness waits at most 10,891 and 14,805 µs, and hold waits at most 179 and 113 µs, all `kept for reuse`, as recorded.
  - I read each control log: W-1 to W-5, P-1 and the baseline print what ADR-0013's second control table says.
  - No boot, `check`, `boot-test` or `boot-matrix` of my own.
- **Sources:** Origin's repository and the author's logs only. No other project's source was opened.

## Summary

All seven findings are closed by their commits. T70-7 is closed as a code path: the four frames were never reproduced, so it does not settle what caused them.

The fixes bring no High and no Medium. Two new Low findings:
- **T70-8:** the 500 ms cap on the boot flow's interrupts-off wait for the hold turns a host stall of half a second into a Refuse failure. That is tighter than ADR-0013 R1(b)'s "budget of seconds", and the boot flow's side does not need the cap.
- **T70-9:** sentences the answers supersede are left standing:
  - in `TASKS.md`'s Result and `SOURCE_PROVENANCE.md`'s first entry;
  - in `retire_ended`'s own doc comment, which still says it runs "with interrupts on";
  - one answer sentence calls T70-7's path "the explanation" of a failure never reproduced.

## Each finding of the first review

| Finding | Status | Why |
|---|---|---|
| T70-1 (Medium) | **Closed** | `cpu.rs:842` wraps the retirement in `without_interrupts`. The only other caller is `Contexts::switch` (`contexts.rs:228`), which runs from the trap entry with interrupts off. So no switch on the processor can fall between the take and the clear. NMI, #MC and #DB do not touch the table. `this_cpu()` is read before interrupts go off, which is sound because a thread does not move. The capacity count (`contexts.rs:83-88`) is true again under the stated condition: at most 16 + 1 of 32. The new host test shows the gap on the real code. Without a Loom model the condition is documentation, not a type, so a future caller with interrupts on reopens the leak (safe, but lost). The module says so. |
| T70-2 (Low) | **Closed** | `retire_ended` returns after one relaxed load when `ended == NONE` (`contexts.rs:278-280`). So a `resume` that switches nothing, and a drop with nothing ended, run no locked instruction. Under the "interrupts off, this processor only" precondition, the load reads this processor's own last write (coherence), so it never skips a pending retirement. In L5, where the dropper is a separate thread, a stale `NONE` only defers the retirement to the main thread's next call, and L5 passes (below). The corrected cost ("an exit costs two `xchg`, a switch adds none") is right against the base: an exit's record and its later retirement are one `xchg` each, and a switch's take is one `lock cmpxchg`, as before. The old sentence still stands in the Result (T70-9). |
| T70-3 (Low) | **Closed** | `contexts.rs:58-66` gives the two C++20 rules, calls L5's execution a false alarm, keeps the swap for Loom, and adds the rule for the next such failure. The wording is right. `SOURCE_PROVENANCE.md:5200` still says the question was "not settled" (T70-9). |
| T70-4 (Low) | **Closed** | The clause trims and reclaims first (`stacks.rs:627-628`), so room is all but certain. Without room W3 now fails, "not judged" (`stacks.rs:649`). W-5 skips `stack_in_use`'s retirement only for the window thread's exit. It reaches W3 and fails it (`false (leaked)`), and then the proof's own count (`lost: 1`) fails too, as the log shows. MS-7's note now cites W-2 and W-5, which is accurate: each shows the drop reading a stale home and leaking. |
| T70-5 (Low) | **Closed**, with T70-8 as a consequence | The held processor's spin and the boot flow's spin for the hold are both capped at 500 ms (`stacks.rs:437`, `594`, and the AP's at `stacks.rs:529`). Either processor's interrupts-off span in the clause is then about 500 ms plus the trap path, under `tlb.rs`'s 1 s. The line reports both of the boot flow's waits, and ADR-0013 records the 106 µs exit hold. W-4's log shows each cap ending its wait at 500,063 and 500,003 µs. The row's sentence "a hold over a second would stall one" describes a hold the cap no longer allows. It is harmless, but it reads as if the reliance were still open. |
| T70-6 (Low) | **Closed** | The put-back is `compare_exchange(NONE, ended)` (`contexts.rs:286`). With interrupts off a newer exit cannot record itself in the gap at all. If the precondition were broken, the newer record survives and the refused one stays covered, and its caller halts. A refusal whose gap saw an exit both recorded and retired finds `NONE` and puts its own record back, which is correct. The host test drives the exit inside the gap. |
| T70-7 (Low) | **Closed in code; the four frames still unexplained by evidence** | `ZOMBIES` is now lowered after the FPU area, the stack (its `give_back`, under the region lock), the address space and `thread_reaped` (`sched.rs:234-266`, the decrement at line 264), with no early exit between. `zombies()` (Acquire) is read only by `settle` (`sched.rs:225`, `230`). So when `settle` sees zero, every reaped stack has reached the cache or been retired before its `trim()`. The reorder adds nothing new: `settle` already waits up to 2 s, and `reap_zombies`'s early return on a zero count can only now see a higher count, which costs one extra walk of an empty array. |

## New findings

### T70-8 (Low): the 500 ms cap on the boot flow's wait for the hold makes a half-second host stall a Refuse failure

**Where:**
- `framekernel/src/stacks.rs:437` (`HOLD_MILLIS = 500`).
- `stacks.rs:592-596`: the boot flow spins for `HELD` with interrupts off, for at most that long, and then reports `NotHeld`.
- ADR-0013 R1(b) (lines 29-31): a Refuse proof may wait "with a budget of seconds" and may not "demand a tight time bound".

**The scenario.** After the word `GO`, the boot flow can see `HELD` only after the held processor has made progress:
1. the thread sees `GO`;
2. it makes the sleep's trap;
3. `with_queue` takes the queue and thread locks, then `follow_thread` runs;
4. it takes the compare-and-swap to `HELD`.

On the 65-, 67- and 288-processor software-processor machines, a guest processor can go unscheduled for over a second. ADR-0013 Amendment 3 exists because shootdowns stalled there for exactly that reason. If that happens inside this span, the clause prints "never reached the window", the kernel stacks proof fails, and the boot is refused. Amendment 3(b) does not re-run it, because no stack was lost. Before the answers that budget was 5 s.
- **How likely:** the gates saw at most 179 µs, so the margin is large, and the risk is rare. It is a timing bound in a Refuse proof, which R1(b) asks to avoid.
- **Why the boot flow's side needs no cap:**
  - The only cap the shootdown argument needs is on the held processor's hold. That hold is bounded by how fast the boot flow answers once it sees `HELD`: microseconds, unless the boot flow's own processor stalls.
  - The boot flow's wait for `HELD` need not keep interrupts off, so it need not be short.

**Suggested fix:**
- Let the boot flow wait for `HELD` the way `yield_until` waits, opening `cpu::interrupt_window()` each round, without yielding, with a budget of seconds. It then answers shootdowns while it waits.
- Keep the 500 ms cap on the held processor alone.
- Say in ADR-0013's row which wait is capped and why, and drop or reword "a hold over a second would stall one".
- If the cap stays on both, the row should say that the clause bounds a cross-processor progress step at 500 ms, against R1(b). That is a decision for the coordinator.

**How sure:** the path is from the code and Amendment 3's own account of the machines. It has not been observed.

### T70-9 (Low): sentences the answers supersede are left standing, one in the code

**Where, all at `18ced439`:**
- `framekernel/frame/sync/src/contexts.rs:266-268`: `retire_ended`'s doc comment still says the condition "holds whenever this is called, from `Self::switch` or with interrupts on". Four lines below, the comment says "Call it with interrupts off".
- `docs/engineering/TASKS.md`, still standing in item 70's Result:
  - line 2302: ruling (c) answered "an atomic swap, not with interrupts off";
  - line 2313: "the budget: 5 s each way", with the old hold figures;
  - line 2328: the old cost sentence, "An exit adds one `xchg`".

  The answers block (lines 2337 on) says the first and the last are superseded or corrected, but the Result's own lines carry no mark, and line 2313 is not mentioned.
- `docs/engineering/SOURCE_PROVENANCE.md:5200`: "whether Loom's search is wider than C11 there was not settled". The answers entry cites the C++20 rules but does not mark that sentence as answered.
- `TASKS.md:2353`: "This is the explanation of the directory proof's four frames". The four frames were not reproduced, and my first review called the path plausible. Line 2370's "closed in reasoning, not by recurrence" is the accurate wording.
- `developer-tools/xtask/src/loom.rs:1381`: C6's comment describes a race between a retirement and the next exit's switch, which the "interrupts off" precondition now excludes. C6 now guards a caller that breaks the precondition, which is still worth having, but the comment should say so.

**Suggested fix:**
- Edit `retire_ended`'s doc comment to match the precondition.
- Mark each superseded Result and provenance sentence in place, for example "(superseded: see the answers to the review)", or edit it.
- Change "This is the explanation" to "a path that gives the logged symptom; not reproduced".
- Reword C6's comment.

None of this changes behaviour.

## Checked and found sound

- **Load-then-swap under concurrent `stack_in_use` callers.** Each processor's `stack_in_use` retires only its own table (`CONTEXTS.get(this_cpu())`), so callers on different processors never share an `ended`. Callers on one processor are serialised by interrupts off. NMI, #MC and #DB do not call it. A drop inside a scheduler hook (`on_exit`'s fallback when the zombie table is full) is already in the trap with interrupts off, and retires only the previous exit before `switch` finds `NONE`.
- **The CAS put-back racing a newer exit.** Covered under T70-6 above.
- **T70-7 against `settle`'s trims.** No reader of `zombies()` other than `settle`. The decrement comes after `give_back`'s region lock, through `fetch_sub` (AcqRel) and `zombies()` (Acquire). So `settle`'s `trim()` sees the stack in the cache, and nothing returns between the take and the decrement.
- **The 500 ms caps.** The held processor gives up and flags it (`HOLD_GAVE_UP`, which fails the clause), so an ask made after a give-up cannot pass.
  - The flag is read after the boot flow's `slot_state`, which takes the region lock the held processor's reap later releases, so the read cannot miss it.
  - If the boot flow's cap and the hold race, the existing `GO → GAVE_UP` compare-and-swap resolves it, and the ask still happens inside the window.
- **The trim and reclaim before the clause (`stacks.rs:627-628`).** These add a few reclaims outside any `settle`. A stall there fails the proof's own count without Amendment 3(b)'s re-run, but the proof already trims and reclaims that way at `stacks.rs:382-411`, so this adds no new class.
- **W-5's aim.** It removes the retirement only for the window thread's exit. The AP's idle context reaps right after that exit's switch, with no other switch between, so the control is repeatable in practice. It fails W3 for the reason W3 exists, and it fails the proof's count, as its log shows.
- **Records.** ADR-0013's second control table matches each log: baseline, W-1 to W-5 and P-1. The gate figures match my tabulation of the evidence (30 window boots, 23 kernel boots in `boot-matrix`, the hold and wait maxima). No address is printed in any new line, and no KVM, CI or hardware claim is made.

## Not covered

- I did not boot, and I did not run `check`, `boot-test`, `boot-matrix` or any control. The boot evidence is the author's.
- The four frames: T70-7's path is removed, but nothing shows it was the cause.
- T70-8's stall has not been observed.
- The merge `50ec0efb` outside the files TASKS 70 owns. In particular, TASKS 61's xtask refactor was read only for the window markers.
- TASKS 66's coming changes to `sched.rs` (`settle`'s waits), which the keeper must merge with `927e699d`.
