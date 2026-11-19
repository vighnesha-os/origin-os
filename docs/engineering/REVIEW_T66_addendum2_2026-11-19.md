# Second addendum review of TASKS item 66: the answers to REVIEW_T66_addendum_2026-11-19.md (T66-R11 to R16)

- **Range:** `f700daa0..16e4fa13` on `claude/t66-timing-proofs`, unpushed. The code alone is `git diff 2364d3af 1bccb576`.
- **The merges:** `2364d3af` and `16e4fa13` merge `land/batch-7`. In TASKS item 66's files they change only:
  - TASKS item 70's hooks in `sched.rs`;
  - two comments in `timer.rs` and `time.rs`;
  - `stacks.rs`'s `WINDOW_WAIT_SECONDS`, which is now `crate::sched::OTHERS_PATIENCE` (`stacks.rs:434`), as ruled.
- **Date:** Thu Nov 19 2026, about 17:40 to 18:10 UTC (`date -u` at the start: 17:38:34).
- **Reviewer:** the same AI reviewer (Claude) as the first two reviews, independent of the author session. No person reviewed this.
- **Read:**
  - `t66-addendum2.md` and `t66-rulings.md`, its last sections.
  - The code diff.
  - The tip's `apic.rs`, `timer.rs` (`take_ticks`, `sample_period`, `read_count`, `init`), `sched.rs` (`with_queue`, `worker`, `run_window`, `respawn_workers`, `run_correct`, `prove`) and `runqueue.rs`.
  - ADR-0013's TASKS 66 section and control table, the research ledger's §3, `SOURCE_PROVENANCE.md`'s three TASKS 66 entries, and CL20.
  - The evidence archive `answers2-rounds.tar.gz`: `controls11`, `controls12`, `ev8` and `ev9`.
- **Ran,** in a fresh `git archive 16e4fa13` export, under that copy's `cargo xtask exclusive --`:
  - **Host tests:**
    - `origin-frame` `apic::` and `stalls::`: 24 passed.
    - `framekernel` `runqueue::` and `rtc::`: 56 passed, including one probe test I added to the export only, then removed.
  - **`cargo xtask loom`:** all 61 models passed and all 89 negative controls were caught.
  - **Clippy:** `-D warnings` on `origin-sync --all-targets`, and on `origin-frame` and `framekernel` with `--lib --tests`. All clean.
  - **`cargo fmt --check`:** clean.
  - **A Python model of `sample_period` and `PeriodSample`,** written line for line from `timer.rs:741-774` and `apic.rs:463-490`, kept in the scratchpad's `sim/`.
  - Nothing booted. The author's tree was untouched. No project source outside Origin's repository was read.

**New findings: Low 3.** IDs continue from T66-R17. No High and no Medium.

---

## Status of T66-R11 to R16

| ID | Status | Reason |
|---|---|---|
| T66-R11 (Medium) | **Closed** | See "R11" below. Control T66-27 fails as aimed (`controls11`). |
| T66-R12 (Medium) | **Closed** | The four tests are back unchanged from `c0f0d02a`, and they pass. The provenance records the deletion and the restoration in both entries. |
| T66-R13 (Medium) | **Closed, one Low** | The rate now comes from the timer's count. My recurring-stall probes are host tests: twice the rate, and 1.5× the rate, each with a stall in every tenth gap. In each, delivery reads `InBounds` and the count says `Fast`, so the run is `TooFast`. In boots, T66-22 and 22b, and T66-28 and 29 (1.1× fast with a 50 ms stall in every run), all fail at run 1 on the count's period (501,350 and 911,413 ppm). One gap in the sampler is T66-R17. |
| T66-R14 (Low) | **Closed** | `take_ticks` reads the three registers before `stop_timer` (`timer.rs:655`). The passing run stores them (`:609`). `timer_read_back` compares against them (`sched.rs:85`). Control T66-26 (`PERIOD` × 2) fails at once on CPU 0. |
| T66-R15 (Low) | **Closed** | See "R15" below. |
| T66-R16 (Low) | **Closed** | The 4 s line is now inside the loop, numbered per run (`wait.rs:348-356`). Control T66-25 shows `run 1 of 8`. |

### R11: the run generation and the wait for earlier workers

- **Correctness on every run:**
  - `run_correct` (`sched.rs:1340`) is judged after every run: every worker exited, and on every processor both workers ran and ran there.
  - `!correct` breaks the loop at once (`:1570-1574`), so a run that failed correctness is never repeated.
  - A repeat therefore happens only after every worker of the run before has added itself to `EXITED`, which is its last act before `exit()`.
- **Can a stale worker still count? No.**
  - The worker's progress and placement writes are guarded by `RUN == run`. Its exit is counted with `try_update` only while `EXITED`'s upper half is its run.
  - In any case no earlier worker is still in its loop when `respawn_workers` runs.
  - The order is sound: `RUN`, then `EXITED` (Release), then the counts cleared, then `STOP` cleared (Release), then the spawns.
  - The lower half of `EXITED` holds at most 2 × 288, far from the upper half.
- **Can a correct run fail? Not newly.**
  - The clauses now judged on every run were judged before on the last run, and a kernel that switches meets them on any run that took its 50 ticks.
  - A worker that does not exit within 10 s fails at once. That was already a failure, and the load-10 non-exit stays unexplained, as recorded.

### R15: the 15th switch

- **The stamp:**
  - `with_queue` (`sched.rs:368-393`) wraps every scheduling entry: tick, yield, sleep, block, exit and wake.
  - It stamps after the step that counted the switch (`runqueue.rs:260`), so the stamp is never early.
  - `switches_from` is taken when the watch is armed.
- **`judge_window`:**
  - It fails `at_switches == 0` (fewer than 15 switches in the window) before the bound (`:314`).
  - It passes only a 15th switch within 500 ms.
  - It repeats only a late switch with a tick gap of two periods.
- **My probes at the tip:**
  - **Your R15 case, a 40 Hz tick switching on every other tick:** `Stalled` in every run, so it fails at run 8 and never passes.
  - **55 Hz, every other tick:** `Late`, fails at once.
  - **100 Hz, every fourth tick:** `Late`, fails at once.
  - **45 Hz and 35 Hz, every tick:** `InBounds`. That is the stated floor of about 30 Hz.
- **On the old sentence:** "every timing bound a passing run met before" (ADR-0013:733) is now true for the switch bound.

### The `TimerCurrent` pair method (your question 2)

- **Is a stall inside a pair always caught by the bracket rule?**
  - **In the pair's own two reads, yes.** Their brackets make up `spread`, and a clean pair needs `spread` × 100 ≤ interval, with an interval of at least 0.5 ms. Any stall of more than about 5 µs inside either bracket is rejected.
  - **Between reads, a stall does not matter for accuracy.** The count and the TSC both keep running: under TCG both follow host time, and on hardware both run through an SMI. It matters only for hiding a reload, which is what the gap rule is for.
  - **Inside an intermediate read's bracket, no.** The gap rule measures from `prev.after` to `next.before` (`timer.rs:751`), and `spread` covers only the first and last reads. Such a stall escapes both: T66-R17.
- **Can a reload between the two reads go unseen?**
  - Hiding a reload needs two consecutive count samples at least a full true period apart.
  - **A correct or slow timer:** such a stall makes the span exceed the 2 ms target, so the pair closes at that very read, and its wide bracket makes the pair unclean. My model found no clean pair with a wrong period for stalls of 9 to 11 ms at any lead.
  - **A timer more than 5× fast:** the true period is below the target, so the stalled read can stay intermediate. The model gives a clean pair at 1,006,581 ppm for a timer 10× fast (T66-R17).
- **Is 97 % to 103 % of `tsc_hz / 100` the right reference? Yes.**
  - The measured period is `initial × (TSC ticks per count)`, where `initial` is this run's own register, T66-R14's snapshot.
  - The boot processor's timed values are an input to the measurement, so they cannot also be its reference.
  - `tsc_hz / HZ` is what 100 Hz means on the kernel's own clock.
  - What this checks is the APIC-to-TSC ratio and the programming. The absolute rate still rests on the calibrated TSC, which only the RTC second checks (Report, 1 %), as before TASKS item 66.
  - The band is wide against what was measured (−0.45 % to +0.55 %, uncertainty included). A clean pair's own error is at most ±0.5 %.
- **Can a timer that is wrong but stable pass?**
  - **Without stalls, only within 3 %**, the stated limit. The sampler measures the count's real period, a fresh error cannot move a clean pair by more than 0.5 %, and the first clean pair is taken, not the best, so there is no selection toward passing.
  - **One-shot, masked, TSC-deadline mode, or a count that does not fall:** no sample, so the run is undecided and never passes. Delivery fails as well.
  - **Delivery faults:** what passes is lost interrupts up to the 150 % total, or extra interrupts down to 95 %, which are bounds from before TASKS item 66.
- **The verdict's order** (`tick_verdict`, `apic.rs:418`):
  - a wrong rate fails whatever delivery did;
  - an undecided rate never passes;
  - only an in-bounds rate with in-bounds delivery passes.
  - The host tests cover the whole table, and T66-30 (brackets planted wide) fails at run 4, undecided.

### The 8 attempts (your question 4)

- **No proof passes a run it should fail.**
  - Only `Stalled` repeats, and only after a correct run.
  - A run passes only on its own `InBounds` verdict and its own correctness.
  - A tick slow enough for every gap to be two periods is `Stalled` in every run, so it fails at run 8 (my probe and the authors' test).
- **What 8 runs change:** an intermittent stall-like fault gets 8 chances instead of 4, each repeat said on its line. That is the ruled trade, and on bare metal each repeat is a finding (ADR-0013:743-753).
- **Cost:** up to 7 more runs, each with a 10 s window budget and a 10 s exit wait, as CL20 now says.
- **Controls:**
  - T66-9b fails "stalled in every run" (`controls12`, at 4 runs).
  - T66-27 fails "not run again".
  - The 8-run reruns are not in the evidence (T66-R18).

---

## T66-R17 (Low): a stall inside an intermediate read's bracket escapes the sampler's gap rule and its spread; on a timer more than 5× fast it can hide a reload and give a clean pair near nominal

**Where:** `framekernel/frame/src/timer.rs:741-774` (`sample_period`; the gap check at `:751`); `framekernel/frame/src/apic.rs:463-490` (`spread` covers the first and last reads only).

**Scenario,** from the model of the code, `sim/sample_sim.py`:
- The timer is 10× fast: a true period of 1 ms against a nominal 10 ms.
- A gap restart puts `first` 111 µs before a reload.
- Reads go on 1 µs apart until 50 µs before the reload.
- The next read stalls 1.01 ms between its RDTSC and its MSR read. That is outside `next.before − prev.after`, so the gap rule passes, and its count is sampled late enough to be below the previous one, so the reload is not seen.
- Reads go on, and the pair closes at the next, visible reload, with clean end brackets.
- **Result:** `period` is 1,006,581 ppm of nominal, so `judge_period` says `InBounds`.
- With the gap measured from `prev.before` to `next.after`, the same reads give no clean pair.
- For a correct or slow timer the model finds no such case, because the hiding stall must exceed the 2 ms target, which closes the pair at that read.

**Reach:**
- It needs a timer more than 5× fast, a gap restart at the right phase, and a stall placed inside one read's bracket across a reload.
- The delivery stamps independently fail such a timer as `TooFast`, unless stalls also mask its total in the same run.
- So no realistic false pass is shown. But the answer to "is a stall inside a pair always caught" is no, for intermediate reads.

**Fix:**
- Measure the gap rule across the brackets, `next.after.wrapping_sub(prev.before) > gap`, or require every read's own bracket to be under `gap`.
- Add a host test of the sampler. Its logic is plain arithmetic over `CountRead`s and could move into `apic.rs` with a reader closure.

## T66-R18 (Low): the final code's controls and acceptance are not in the evidence

**Where:** ADR-0013:755-790; `t66-addendum2.md`, "Controls"; `t66-evidence/answers2-rounds.tar.gz`.

**What is missing:**
- The hand-over and ADR-0013 say the preemption proof's nine controls ran again on `1bccb576` (17:24 to 17:28 UTC) as `controls13`, and ADR-0013 quotes those lines: "run 8 of 8" for T66-9b, "run 1 of 8" for T66-8, 26 and 27.
- The archive was written at 17:06 UTC and holds only `controls11` and `controls12`. Those were on `0df830a8` with 4 runs: T66-9b ends "run 4 of 4".
- The quoted 8-run lines cannot be checked against any log.
- No evidence round ran the final code with 8 runs either:
  - `ev8` ran 4 runs, and failed one idle 65-processor boot at the window, stalled in all 4.
  - `ev9` was a 12-run diagnostic build.

**Why it matters:** the coordinator's acceptance line ("the large machines must pass reliably at the gates' own load") is not yet shown for `1bccb576`/`16e4fa13`. The full gates are pending, as the hand-over says.

**Fix:**
- Add `controls13` to the archive.
- Run the evidence series and the gates on the final tip before the result is recorded as accepted.

## T66-R19 (Low): records

1. **The first-answers provenance entry is stale and unmarked.** `SOURCE_PROVENANCE.md`, "TASKS item 66: the review's answers":
   - its **What** still describes `periodic_programming` and the ten-gap rule;
   - its **Known limits** still says "A tick 5 % to 15 % fast whose total a stall made look right passes the tick proof, as before";
   - neither is marked superseded, though `TASKS.md` marks its counterparts (`TASKS.md:2709`, `:2714`).
2. **ADR-0013's stated limit is half of it.** "A processor whose timer reads back as timed but ticks slower … is caught only below about 30 Hz" (ADR-0013:735) omits the other half: a processor whose own timer clock runs fast is not caught at all. The preemption proof has no upper bound on an application processor's tick. This is unchanged from before TASKS item 66, and hypothetical on hardware where every core's APIC timer shares one clock, but the limit should say both.
3. **The widest new line is wider than said.** The hand-over says about 152 bytes. The tick proof's failure line reaches 166 bytes with an extreme `period … ppm` (a 20-digit `u64`), and `: APIC timer is wrong` then straddles byte 160. It is harmless: the line is printed on the boot processor before any other processor runs, so its pieces cannot be separated.

---

## Checked and found sound

- **R12:**
  - The tests are restored byte for byte from `c0f0d02a`, and `runqueue::tests` has 30.
  - `SOURCE_PROVENANCE.md`'s first T66 entry names the deletion and the restoration, and the second gives the counts (25 → 29, then 30).
- **R14:** see the status table. Also:
  - the snapshot is taken before `stop_timer`, from the run that passed;
  - `timed_programming()` is `None` until then, so a read-back reads `Unread`, which fails;
  - T66-26 names CPU 0 because the boot processor's own `start_periodic` used the planted `PERIOD`. That is the intended reading.
- **R13's arithmetic** (`apic.rs:463-490`):
  - u128 products, checked subtractions, and `fall > 0`;
  - `ppm_of` is guarded against zero;
  - the brackets' half-spread bounds the interval's error;
  - count quantisation is small: 0.13 % at worst for a 1.5 MHz timer clock over 0.5 ms.
- **The tick proof's verdict at the tip:**
  - `judge_ticks` judges delivery only;
  - the ten-gap rule and `least_ten` are gone from the code and from every current record (only the ledger's history rows name them);
  - `apic.rs`'s doc comments and test names now say what the code does.
- **Lines** (ruling condition 2):
  - The answers' logs hold 25 distinct new-form lines with digits normalised. I checked them against xtask's literals.
  - `: … failed` appears only on real failures: `: scheduler failed` on the window line (including `stalled, not run again`), `: APIC timer is wrong`, and `: wait timing failed`.
  - No line contains another marker's text. Hex values appear only as register contents on failure lines; there are no addresses.
  - `period undecided`, `run R of 8` and the `waited … run R of 8` line carry no failure word.
  - The window diagnostic's lines in `ev9` come from a temporary patch; none is in the tip's code.
- **ADR-0013:**
  - The rules (lines 711-737) match the code, including correctness first, the read-back against the timed registers, the 15th switch, 8 runs, the count's 97 % to 103 %, 64 tries, and undecided never passing.
  - "What a passing run meets" is now accurate.
  - The bare-metal "a repeat is a finding" note is present (743-753).
  - The control quotes I could check against `controls11` and `controls12` match.
- **The research ledger** (§3.1, §3.3), checked against `ev8` and `ev9`:
  - every count's period at idle, 997,710 to 1,005,457 ppm;
  - windows discarded, 1 to 66;
  - the idle preemption window, up to 2,190 ms;
  - the load-3 and load-10 period ranges;
  - the AMD boots' 42 and 25 windows discarded;
  - the one idle failure (exit 1 in `ev8/summary.tsv`, `intel-65.1`) and the load-3 and load-20 failures.
- **CL20** gives the 8-run budget, the per-run waits and the count's cost: one 2 ms pair, 64 at most.
- **Loom and Clippy:** see below. `runqueue.rs`'s new fields are plain data under the queue's lock, and the `ReadBack` compile boundary holds.

## Not covered

- No boot. T66-R17 is from a line-for-line model of the sampler, not the kernel.
- The 8-run controls and the final gates (T66-R18).
- Under KVM, in CI or on hardware.
- The intermediate commits on their own.
- TASKS items 70, 71 and 63 B5's own changes, beyond where they touch TASKS item 66's files.

**Loom:** `cargo xtask loom` on the export of `16e4fa13` (pristine files, run inside that copy's `cargo xtask exclusive --`, finished 17:50 UTC): all 61 models passed and all 89 negative controls were caught (123 model runs).
