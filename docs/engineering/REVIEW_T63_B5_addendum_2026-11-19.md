# Addendum to the review of TASKS item 63, step B5: the answers

| | |
|---|---|
| **Scope** | `claude/aml-tolerance`, `6b4c5e09..caeb1008`: point 1 ruled again (`c4e0719d`, `77121794`), G15's stated limits (`fe40b139`), the hand-over's points 2 to 5 (`1abef830`), and the answers to B5-R1 to B5-R9 (`180bc6ad` to `8be81730`, records `caeb1008`). The merge `6b4c5e09` changes nothing in `origin-aml` or the AML proof: in the paths reviewed it touches only `developer-tools/xtask/src/main.rs`, with nothing AML-related |
| **Who reviewed** | The same AI reviewer, started by the coordinating session. No human has reviewed this code |
| **Read** | The amendment and the note after it in `t63-g-rulings.md`; `t63-b5-answers-handover.md`, read after the code; every library, kernel and test diff of the range; `op.rs`'s operand table; the ledger's §9 G15 and stated-limit lines; ADR-0013's AML row; the committed review's Status column |
| **What this is not** | No boot, no `check`, no 1M soak, nothing in release. `origin-aml` host tests only, in a fresh export of `caeb1008`, each run under `cargo xtask exclusive` (listed at the end). The author's 210 and 42 passing host tests are theirs; the full gates have not yet run on this tip |
| **Result** | **All nine findings are closed by their commits.** B5-R3's ruling is built, with one residual the author names. Point 1 is built right, and its two stated limits are recorded accurately, apart from the PDF page numbers. **New: 2 Low, 3 Informational.** The most important, **B5-R10**: R3's marking removes from binding *live* names that an abandoned statement made on its way, not only dropped ones. A table-level `While` abandoned on its third pass loses the binding of packages its first two passes stored in a global: reading that global then fails `NotFound`, where G1's pass would give an empty element and a report |

## Verdicts on B5-R1 to B5-R9

| # | Verdict | Why |
|---|---|---|
| B5-R1 | **Closed** | `skip_term` now takes the operand kind. A name may be a call only in a statement position (`ONE` is `TermArg`) or a `TermArg`/`TermArgRaw` one; in `Super`, `SuperOpt`, `Target`, `Simple` and `Data` positions it is stepped over as a name (tolerate.rs, `skip_term`). **Checked against every entry of `op.rs`'s table:** every place the decoder reads with `decode_location` is `Super`, `SuperOpt`, `Target` or `Simple` there. That covers `Store`'s target, `RefOf`, `Increment`, `Decrement`, `Notify`, `SizeOf`, `ObjectType`, `Acquire`, `Signal`, `Wait`, `Reset`, `Release`, `CondRefOf`, `CopyObject`'s `Simple`, and the targets of `T1`/`T2`, `Divide`, `ToString`. Every `TermArg` is a call in both, `DerefOf`'s `TermArgRaw` included. `Name`'s `Data` names are `Grammar` in the decoder (whole table), and the skip takes no arguments for them either. The three cases of the review are tests, with the planted fault `SkipCallsInPlaces`, and the soak's sentinel `Name (QS, One)` catches it too |
| B5-R2 | **Closed** | The pass runs before `pop_frame` (load.rs, `finish_load`) and charges only its own table's entries, the list's tail, before binding anything. A limit there fails the table through `abort` and is traced. **Probe Q5,** a run-time table with a package of three names under every step limit from 1 to 173: in 81 runs (limits 53 to 133) the evaluation ends `[Load, Limit(Steps), Loaded(Err(Limit(Steps)))]` with the table rolled back; in the rest the limit falls after `Loaded(Ok)` and the table stays (Q8). No run gives a `Load` without its `Loaded`. "The tail is its own" holds: a nested load binds and truncates its entries, or `fail_table` drops them all; a method's packages are `Lazy` and never listed |
| B5-R3 | **Closed as ruled, with the residual the author names, and a new consequence (B5-R10)** | A dropped extra element is marked never to bind over the element's own bytes (`carrier.at` to the element's end), a nested package's names included. So are an abandoned statement's names and a failed table's. The list keeps listed elements as collector roots, so a pass sees the same set whether or not the collector ran; once marked, an element is off the list and collectible. **Residual** (hand-over point 2): a temporary package of a statement that completes keeps its names until the pass, so an unbound one is reported though nothing can read it. It is now deterministic, and B5-R11 measures what it holds. **New:** the abandoned statement's byte range also takes names that are live (B5-R10) |
| B5-R4 | **Closed** | **Audits 1, 3 and 5 are in.** Audit 1 checks each table's `Abandoned` reports against its count, and that a block failing after an abandonment failed for a whole-table kind, which is the contract's check stated for the failing case. Audit 3 compares `Released` reports with a test-only count of releases when the evaluation ended well and nothing was suppressed. Audit 5 checks each logged access against its region, looked up by path. **Grammar:** every item of §4.9 is now generated. That is failing `If` and `While` predicates; an unterminated string with a byte above 0x7F; extra elements with a nested package of names; a place naming an argument-taking method; a duplicate across tables; and `ToInteger`/`ToHexString`, already there through `un` 0x96–0x99, as the author says. Nothing is left "listed with a reason". One gap: the soak's `While` predicate fails on its first pass only; a later pass is covered by the host test. **`NoReport`:** 46 tests fail with it planted (34, 5, 5 and 2). One test that asserts reports, `the_skip_asks_the_host_nothing_and_reaches_no_fatal` (three), is not in the list: a nit |
| B5-R5 | **Closed** | ADR-0013's row names "the names in QEMU's packages could not be bound", the tolerance reason and the pieces reason, and says the binding branch is tested since B5-R5. The host test reaches the branch first by a clock that turns late after `K` readings, searching for `K`. It holds |
| B5-R6 | **Closed** | `pins` has its comment again; `ec_wide`'s starts with its own |
| B5-R7 | **Closed** | `AlreadyLoaded` names what is compared and what is not |
| B5-R8 | **Closed** | A hold records its method outside load-time code, and `Released` carries both paths. The test shows a mutex its method declared reported with no mutex path and the method's, and the sync level held at 5, so a level-1 `Acquire` later in the evaluation fails `SyncLevel` |
| B5-R9 | **Closed** (an Informational nit, B5-R12) | `fail_load` reports what it releases. Tested with a mutex the boot table declared |

## Point 1, as ruled again, and its two stated limits

**Built right.** `write_value` (region.rs):
- **One write** when the data fits the field in whole bytes, or is an integer, or the field is a buffer-protocol one. A string is a buffer of its bytes, without its NUL.
- **Otherwise field-sized pieces, lowest first.** Each piece is zeroed and filled from the data's next `bit_len` bits, so the last is zero-extended. Each costs a unit of its own on top of each unit write's, and the count is `ceil(bits / bit_len)`, after the zero-width guard, so the division cannot be by zero.

Checked, case by case:
- **Lowest first, the last zero-extended:** the EC test's third transfer is `[0x45, 0x00]`. Probe Q3, 3 bytes into a 12-bit field: piece 1 writes 0x201 (bytes 0x01, 0x02), piece 2 writes 0x030 (0x30, 0x00).
- **`Store ("ABCDEFGH", F32)`:** writes 0x44434241, then 0x48474645, and the field keeps the second. `("ABCDEFGHI")` makes three, the last 0x49 (the author's test).
- **The EC field:** one transfer a piece. `"ABCDE"` into a 16-bit EC field gives three transfers at address 0 (`AB`, `CD`, `E\0`) and one `Pieces(E16, 3)`.
- **The bound:** 256 KiB into a 1-bit field ends `Limit(Steps)`, with every write made counted (writes ≤ steps) and one `Pieces` with 2,097,152.
- **`Pieces` once, before the first piece:** told before the loop. The kernel test shows it counted though the host refused the first write, so it came first. The proof counts it apart from tolerances, and fails with "a store in pieces was reported on QEMU's own tables". AML-8 has not run yet, as the hand-over says.

**The two stated limits are accurate** (ledger §9, "Two stated limits"). Probe Q3 on `caeb1008`:
- 2 bytes into a 12-bit field: one store, cut, no report. That is the byte comparison, as stated: Table 19.7's letter departed from, ACPICA matched.
- `"AB"` into a 4-bit field: four nibble writes (1, 4, 2, 4) and `Pieces(F4, 4)`, "four writes where the row gives two", as stated.
- `"A"`: one write of 1, its upper bits lost, as the String row says.

The claim that Origin departs from ACPICA only where `Pieces` reports holds against the ledger's G15. One thing the record shows only by `F32`'s example: a string that fits a field wider than 8 bits departs from the String row too (`"AB"` into a 16-bit field is one write, 0x4241, where the row gives two, leaving 0x0042). That falls under "holds exactly only for fields of 8 bits", but is not spelled out. **The page numbers are not accurate:** B5-R13.

## New findings

| # | Severity | Finding | Where | Proposed | Status |
|---|---|---|---|---|---|
| B5-R10 | Low | **R3's marking takes live names an abandoned statement made, not only dropped ones.** `abandon` marks never to bind every listed element whose bytes lie in the statement's range (tolerate.rs, `abandon` and `never_bind`). Some of those may already be stored somewhere live: a `While`'s earlier passes, or an inner `Store` inside the statement (`Store (Store (Package () {\X}, \P), UNDF)`). Marked `Lazy`, they are not bound by the pass and not reported, and a read resolves them each time. **Probe Q1:** a table-level `While` whose body stores `Package (1) {\NONE}` into `GP[I]` and whose predicate fails `Range` on the third pass. The load reports one `Abandoned`. `bind_deferred` reports only the control package outside the loop (`Unbound(74)`; `CTL` reads `[Uninitialized]`). `GP` fails `NotFound` when evaluated, where G1's pass would make those two elements empty and report them | `firmware/acpi/aml/src/tolerate.rs` `abandon`, `never_bind`; `engine.rs` `place_element` | Mark only what the abandoned statement could not have stored: the elements of a package it was building when it failed. Or bind an abandoned statement's elements as any others, and accept the hand-over's point-2 residual for them (reported, nothing reading them). Add Q1 as a test | Fixed, `b83fe91a` |
| B5-R11 | Low | **The list keeps every name load-time code puts in a temporary package alive until the pass, and the pass reports each one unbound.** This is the hand-over's point 2, measured. The list roots its elements (engine.rs, the collector's roots), so a table-level loop that builds packages holds one object for each name it ever made. **Probe Q2b:** a boot table whose `While` stores `Package (1) {\NONE}` into a named package `n` times. Listed: `n`. Live objects after a full collection: 10,012 for `n` = 10,000 and 50,012 for 50,000, against 12 for the same loop with no name in the package. `bind_deferred` then binds all `n` and reports `n` unbound: 64 `UnboundElement` and a `Suppressed` of the rest, though only the last package is readable. Live objects after the pass: 13. Bounded by the memory limit, but at large `n` a loop that loaded before now meets `Limit(Memory)` and fails the table whole; and on QEMU's tables any such report would fail the proof | `engine.rs` `collect_garbage` (the list as roots), `decode_name`; `tolerate.rs` `bind_names` | Record the retention and its rate (one object per name a load-time statement makes, until the pass) as accepted. Or let the list not root its elements and collect once at the start of `bind_deferred`, once a boot and charged, so that only readable names are bound and reported, whatever ran before | Fixed, `b83fe91a` |
| B5-R12 | Informational | **A failed run-time load reports its own mutex with empty paths.** `fail_load` unwinds the table's names before `release_reported`, so a mutex the failed table declared, taken by a method it declared, is reported `Released([], [])`. **Probe Q4** shows this. The report then says a mutex was released, without saying which | `load.rs` `fail_load` | Report before the unwind, or record that a failed table's own names are gone by then | Fixed, `f182d374` |
| B5-R13 | Informational | **The G15 record's PDF pages are one less than the PDF's.** The ledger cites Table 19.7 at "PDF pages 1007 and 1009", the hand-over at "1007 to 1009", and the rulings' amendment at "PDF page 1007". In the owner's copy, read with PDFKit (pages counted from 1, as the same ledger's §5 counts them: "Table 19.7, PDF 1008–1010"), the Buffer to Field Unit row is on PDF 1008 (printed 937) and the String to Field Unit row on PDF 1010 (printed 939). PDF 1007 holds the conversion example before the table | `docs/research/2026-11-14-aml-interpreter.md` §9; `t63-g-rulings.md` amendment | 1008 and 1010 | Fixed, `c252b83a` |
| B5-R14 | Informational | **A `Suppressed` report counts as a tolerance in the proof, even when only `Pieces` were suppressed.** The recorder counts every report but `Pieces` as a tolerance, `Suppressed` included. Past 64 reports in one evaluation, the proof would fail with the tolerance reason where only pieces were stored. QEMU's tables give neither | `framekernel/src/aml_proof.rs` `Recorder::report` | Leave it, or count `Suppressed` apart | Fixed, `83c76eb8` |

## Checked and found sound

- **The hand-over's points 2 to 5.** They are recorded as ruled. `a_chain_of_loads_is_bounded_by_the_call_depth` shows the depth bound with distinct tables, and `a_skip_whose_work_list_reaches_the_memory_limit_fails_the_table_whole` shows a skip at the memory limit failing the table whole and freeing everything.
- **The `deferred` list's discipline.** Room is reserved before the element is made, so no deferred element is ever off the list. The list is charged, counted in the recount and released by truncation. `never_bind` and `never_bind_table` count their work. A pass's charge comes before any binding, so a limit binds nothing.
- **The test-only counters** (`pass_entries`, `ended_holds`) are `#[cfg(test)]` and change nothing shipped.

## What I ran (2026-11-19)

All in a fresh `git archive caeb1008` at `<scratch>/b5a-copy`, with every file touched; the B5 scratch copy was removed first (its probes kept as `<scratch>/review_b5_round1.rs`). I added `firmware/acpi/aml/src/tests/review_b5a.rs` and `mod review_b5a;`. Each run, from the coordinating worktree: `cargo xtask exclusive -- sh -c 'cd <scratch>/b5a-copy && <command>'`. The first waited about 15 minutes for another session's gates to give up the machine.

- `cargo test -p origin-aml --lib review_b5a -- --nocapture --test-threads=1`: 5 passed (the probes print).
  - Q1 as in B5-R10.
  - Q2 was a flawed probe of mine: its target was an integer, so every store failed and was abandoned. It shows nothing about the list.
  - Q3 as above, and Q4 as in B5-R12.
  - Q5: 121 limited runs, every one with its `Loaded`, as in B5-R2.
- `cargo test -p origin-aml --lib review_b5a::q2b -- --nocapture --test-threads=1`: 1 passed, printing the figures in B5-R11. Memory used is not quoted there: after a collection it still holds the heap's high-water capacity, which the no-name control shows too.

## Resolution

Answered by the TASKS 63 session on 2026-11-19 (UTC), to the coordinating session's rulings of the same day, one commit for each finding on `claude/aml-tolerance` (B5-R10 and B5-R11, one cause, in one). This addendum was copied in as received at `743b4c3e`.

**B5-R10 and B5-R11: fixed, `b83fe91a`.** The coordinating session asked for four things:
1. a name in an object something can still read at the pass is bound or reported;
2. one nothing can read is neither;
3. the outcome does not depend on the collector;
4. the list keeps nothing alive.

What changed:
- **The list is weak.** The collector no longer marks it, and before it sweeps it takes off the list what it did not reach (`collect_garbage`, `mark_reachable`, `prune_deferred`).
- **A pass marks first.** It marks from the collector's roots and keeps on the list only what that reached, freeing nothing, so it may run where the collector may not (`sift_deferred`). The marking's work, counted as a collection's, and a unit an entry are charged before anything is bound. There is no marking when no entry waits. A name the marking did not reach was unreachable at every earlier collection too, so 3 holds.
- **B5-R3's marking is gone:** nothing by byte range in `abandon` or `place_element`, and a failed table's names stay listed. A failed run-time load now binds its names at its end, after its `Loaded`: it may have stored a package in an older object before failing. So a limit in that pass is the evaluation's.
- **Two stated edges.** The names of a load that an aborted evaluation failed (a limit, a `Fatal`) are bound by `bind_deferred` if that has yet to run, and are otherwise looked up when read. In a run for a PCI_Config region's function outside the frames, where the collector may not run, a name that only a value held outside the frames could still read is taken off unbound, and is looked up when read.

Tests:
- `what_an_abandoned_while_stored_on_its_earlier_passes_is_bound`: your Q1 and the inner `Store` case. Four names are reported, and `GP` and `P` read with empty elements, the same with the collector run before the pass and without.
- `a_loop_of_packages_with_names_keeps_no_more_alive_than_one_without`: Q2b at 5,000 passes. After a collection no more objects live than with an integer in the package and the one element `PK` reads; one name is listed and one reported.
- `a_failed_runtime_loads_name_in_an_older_object_is_bound`.
- `names_nothing_can_read_are_never_bound_whether_or_not_the_collector_ran`: R3's cases, now with no report at all, the temporary's included.
- The soak's audit now checks only names a marking reaches.

Controls: `StrongDeferred` (the list a root again), `NoSift`, `MarkAbandoned` (R3's marking restored) and `NoFailedPass`, each caught. Records: ledger §9's binding paragraph, CL41 (the marking's cost: once at each run-time load that listed a name, and once at the boot pass), and the first review's B5-R3 resolution marked superseded.

**B5-R12: fixed, `f182d374`.** `fail_load` reports its releases before the unwind removes the table's names. Test `a_failed_runtime_loads_own_mutex_is_reported_by_name` (your Q4's shape): `Released([TMX_], [TK__])`. Control `ReleaseAfterUnwind` is caught. Ledger G2's row says so.

**B5-R13: fixed, `c252b83a`.** I checked the owner's copy again with PDFKit, counting pages from 1. The Buffer to Field Unit row is on PDF page 1008 (printed 937) and the String to Field Unit row on PDF page 1010 (printed 939), as you found. Ledger §9 now says so. The stated limit "strings follow buffers" now names your case, in the ledger and in A19-A3's limitations: a string that fits a field wider than 8 bits departs from the String row too (`"AB"` into a 16-bit field: one write, 0x4241, where the row's two leave 0x0042).

**B5-R14: fixed, `83c76eb8`.** `Report::Suppressed` carries `tolerances` and `pieces` in place of `count`. The proof's recorder adds each to its own count, so the proof fails with `IN_PIECES`, `TOLERATED` or both, as the reports were. Tests:
- `reports_past_the_bound_are_counted_by_kind`: `Suppressed(0, 6)` after 70 stores in pieces, and `Suppressed(8, 8)` after 40 pairs of a reported `Stall` and a store in pieces.
- The kernel's `reports_past_the_bound_count_by_kind`, given the reports directly: on QEMU-shaped tables the host refuses each store's first write, so no evaluation reaches 65.

Control `SuppressedAsOne` is caught.

**The nit: fixed, `d852444a`.** `the_skip_asks_the_host_nothing_and_reaches_no_fatal` joins `NoReport`'s list.

**Host tests** under the machine's turn lock, on the tree these commits make (identical to the tree tested), 2026-11-19 13:38 UTC: `origin-aml`'s 215 passed and 1 ignored, and the kernel proof's 43 passed; formatting and both crates' Clippy are clean. The soak at a million cases, `check`, `boot-test`, `boot-matrix` with AML-1 to AML-8 and TASKS 72's harness run once, on the final tip, after your reading.
