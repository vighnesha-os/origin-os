# Second addendum to the review of TASKS item 63, step B5: the answers to the addendum

| | |
|---|---|
| **Scope** | `claude/aml-tolerance`, `743b4c3e..41e6bf18`: `b83fe91a` (B5-R10, B5-R11), `f182d374` (R12), `c252b83a` (R13), `83c76eb8` (R14), `d852444a` (the nit), `41e6bf18` (records) |
| **Who reviewed** | The same AI reviewer, started by the coordinating session. No human has reviewed this code |
| **Read** | The coordinating session's last note in `t63-g-rulings.md` (requirements 1 to 4); `t63-b5-addendum-answers-handover.md`; `b83fe91a` in full; the other four commits briefly |
| **What this is not** | `origin-aml` host tests only, in a fresh export of `41e6bf18`, under `cargo xtask exclusive`. No boot, `check`, soak or release run |
| **Result** | **R10 and R11 are closed; requirements 1 to 4 hold, with one exception.** R12, R13, R14 and the nit are closed. **New: 1 Low, 1 Informational.** **B5-R15:** a failed table's names that are still reachable are reported unbound and emptied even when they name something, because the table's bytes are released before the pass reads the names from them |

## B5-R10 and B5-R11 (`b83fe91a`)

**The method.**
- **The list is weak:** it is no longer a collector root.
- **A complete collection prunes it** to what the marking reached, before the sweep, so it never holds a freed handle.
- **A pass first marks what is reachable from the roots, without freeing:** `sift_deferred` runs `mark_reachable`, prunes, and clears the marks. It then charges the marking's work (`root_work` plus what was traced) and a unit per entry, in one charge, before binding anything. If no entry in scope waits, there is no marking and no charge.
- **Abandoned statements and dropped elements are no longer marked by range.** That old rule survives only as the planted fault `MarkAbandoned`.
- **Failed tables keep their names listed.** A run-time load binds its own names at its end, failed or not; the failed case binds after its `Loaded(Err)`, so the trace stays paired.

**Requirements, checked by probe on `41e6bf18`.**
1. **Reachable means bound or reported.** Holds, except B5-R15.
   - **Q1** (the `While` abandoned on its third pass) gives `Unbound(46)` twice, for the two packages in `GP`, and `Unbound(74)` for the control. `GP` reads `[[Uninitialized], [Uninitialized]]` where it used to fail `NotFound`.
   - **Q2b** binds the one package `P` still holds and reports it (`Unbound(31)`).
2. **Unreachable means neither.** Holds. Q2b's other 9,999 names are neither bound nor reported.
3. **Independent of the collector.** Holds for what is bound and reported. Q1 and Q2b give identical reports with and without a full collection just before the pass. The argument also holds: nothing outside the roots holds an object across a collection point, since the collector is held off in `with_address`'s nested runs. So a name unreachable at one collection stays unreachable.
4. **The list keeps nothing alive.** Holds. Live objects after a collection are 13 with names, against 12 without (they were 10,012 before this fix). The planted fault `StrongDeferred` shows a test can see this.

**Cost and charge.**
- A pass marks the whole live heap once: at the boot pass, and at each run-time load that listed a name. The work is charged, so it stays bounded by the step limit. CL41 states it.
- In Q2b the pass cost 12,517 steps with 10,000 uncollected entries waiting, and 2,532 after a collection: B5-R16.

**Choices the author names.** I accept all three as stated limits:
- in a `with_address` nested run, a name only an out-of-frame value can read is taken off unbound;
- an aborted evaluation's load leaves its names listed, harmlessly;
- the soak audit counts only marked `Deferred` elements.

## R12, R13, R14 and the nit

| # | Verdict | Why |
|---|---|---|
| B5-R12 | **Closed** | `fail_load` reports its releases before the unwind; the planted fault `ReleaseAfterUnwind` restores the old order |
| B5-R13 | **Closed** | The ledger now says PDF 1008 (printed 937) and PDF 1010 (printed 939); the rulings' amendment is corrected too. The string that fits a wider field is now in the stated limit |
| B5-R14 | **Closed** | `Suppressed` counts tolerances and pieces apart; the proof fails on each with its own reason; planted fault `SuppressedAsOne`. This changes the public `Report::Suppressed` fields, which is fine given no stability promise yet |
| Nit | **Closed** | `the_skip_asks_the_host_nothing_and_reaches_no_fatal` is in `NoReport`'s list |

## New findings

| # | Severity | Finding | Where | Proposed | Status |
|---|---|---|---|---|---|
| B5-R15 | Low | **A failed table's reachable names are reported unbound even when they name something.** `resolve_elem` reads a name from its table's bytes. `fail_load` releases the failed table's bytes (`fail_table`, load.rs 629) before its pass (641), and a failed boot table's bytes are gone before `bind_deferred`. So every name such a table stored into an older object fails to parse, becomes empty, and is reported `UnboundElement`. **Probe Q6:** a run-time table runs `Store (Package (1) {\EXST}, \GQ)`, where `\EXST` exists in the boot table, then fails on a reserved opcode. The result is `Unbound(4)`, and `\GQ` reads `[Uninitialized]`. The same happens for a failed boot table and `bind_deferred`. The report says the name names nothing, which is false. (The author's test uses `\NONF`, which names nothing, so it cannot tell the difference.) | `load.rs` `fail_load` 629–641, `fail_table`; `engine.rs` `resolve_elem`, and `load`'s failure path | Bind a failed run-time table's names before its bytes are released (after its `Loaded(Err)` trace, before `fail_table` drops the bytes). Keep a failed boot table's bytes until `bind_deferred`, while it still has listed names. Or report these with a reason of their own and record it. Add Q6, with a name that exists, as a test | Fixed, `b028c14b` |
| B5-R16 | Informational | **The pass's charge depends on how much garbage is uncollected.** `root_work` counts every slot, so the same pass cost 12,517 steps before a collection and 2,532 after (Q2b). What is bound and reported does not change. But under a tight step limit, whether the pass completes or meets `Limit(Steps)` can depend on when the collector last ran. This is true of all charged work; it is noted against requirement 3's wording | `tolerate.rs` `sift_deferred`; `engine.rs` `root_work` | Record it beside CL41's cost line | Recorded beside CL41 |

## What I ran (2026-11-19)

All in a fresh `git archive 41e6bf18` at `<scratch>/b5b-copy`, with every file touched; the previous copy was removed first. I added `tests/review_b5b.rs` and `mod review_b5b;`. I ran it with `cargo xtask exclusive -- sh -c 'cd <scratch>/b5b-copy && cargo test -p origin-aml --lib review_b5b -- --nocapture --test-threads=1'`, which passed 3 tests (Q1 again, Q2b again, Q6), each printing the figures above.

## Resolution

Answered by the TASKS 63 session on 2026-11-19 (UTC), to the coordinating session's rulings of the same day. This addendum was copied in as received at `022f3c0f`.

**B5-R15: fixed, `b028c14b`** (the fix, not a new reason, as ruled). `fail_table` marks a table failed and keeps its bytes; `release_failed_bytes` gives back the bytes of every failed table that no listed name reads from, at the failure itself, at the end of each binding pass, and after each prune of the list. So a run-time load binds before its bytes go, and a failed boot table keeps its bytes until `bind_deferred` has bound its names, charged to the memory limit until then. Tests, each with a name that exists (your Q6's `\EXST`) and one that does not:
- `a_failed_runtime_loads_names_in_an_older_object_are_bound`: `\GQ` reads `[7, Uninitialized]`, and only the missing name is reported;
- `a_failed_boot_tables_names_in_an_older_object_are_bound`: the same through `bind_deferred`, the bytes kept until the pass and gone after; a failed table that listed no name gives its bytes back at once.

Control `ReleaseBeforeBinding` (the bytes released at once, as before) is caught by both. The coordinating session reads this fix itself.

**B5-R16: recorded** beside CL41 in the records commit after `b028c14b`: the pass's marking counts every slot, garbage not yet collected included (your 12,517 steps against 2,532), so under a tight step limit whether a pass completes can depend on when the collector last ran, as with all charged work; what it binds and reports does not.

**Host tests** under the machine's turn lock on the tree of `b028c14b`, 2026-11-19 15:13 UTC: `origin-aml`'s 216 passed and 1 ignored, and the kernel proof's 43 passed; formatting and both crates' Clippy are clean. The full gates run once, on the final tip.
