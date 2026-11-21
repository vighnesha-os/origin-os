# Research ledger: the kernel's timing proofs under QEMU's software processor

| | |
|---|---|
| Date | 2026-11-19 |
| Question | Why do the kernel's timing proofs fail `boot-matrix` under QEMU's software processor (TCG) on the development Mac, with nothing wrong in the kernel, and what rule lets each judge the kernel rather than the host's load, without weakening it on a real machine? (TASKS item 66.) |
| Method | Kernel boots of the `kernel boot` scenario alone, one machine per `cargo xtask boot-test`, each inside the machine's turn (`cargo xtask exclusive --`), serial logs kept, the 1-minute load average read before and after. "Load N" is N busy loops (`while :; do :; done`) planted on the 10-thread host, standing for work outside the turn; load 0 is the turn alone. A diagnostic build (a temporary edit, put back with `git checkout`) printed each processor's tick watch. The records of earlier failures are those in `TASKS.md` and `SOURCE_PROVENANCE.md`. |
| Host | The development Mac (Apple M1 Max, 10 threads), QEMU 11.1.3 TCG with OVMF, Rust 1.100.0. Under TCG on this host the guest TSC is the host's nanosecond clock: exactly 1,000,000 kHz. |
| Reviewed by | One AI reviewer (`docs/engineering/REVIEW_T66_2026-11-19.md`, and its addendum review `REVIEW_T66_addendum_2026-11-19.md`; its findings and answers are in `TASKS.md`, entry 66). **No person has reviewed this ledger.** |
| Other sources | None. No source of QEMU, OVMF or any other project was opened (CLAUDE.md §3). |

## 1. Before the change: 33 boots (`0ed7bfb9`'s kernel, 2026-11-19 02:10 to 03:00 UTC)

| Machines | Boots | Calibration windows discarded (all 32 = a fallback) | RTC second, widest uncertainty | APIC timer, 50 ticks | Fewest switches on a processor (bound 15) | Timed wait (bound 80 ms) | Failed |
|---|---|---|---|---|---|---|---|
| 65 and 67 processors, load 0 | 14 | 2 to 32; **7 of 14 fell back** | ±4,187 µs | 501 to 531 ms | 17 | 50 to 60 ms | 1: the RTC second refuted a fallback 1.5 % off (984,717 kHz; 1,015,729 ± 3,198 µs) |
| 65 processors, load 3 | 3 | 1 to 2 | ±9,689 µs | 589 to 647 ms | 20 | 51 ms | 2: an RTC second undecided; the TLB proof's 2 s wait for a prober |
| 65 and 67 processors, load 10 | 2 | 10, 27 | — | 1,027 and 1,070 ms | — | — | 2: the APIC timer (Refuse) |
| 1 to 4 processors, load 0 | 6 | 0 or 1 | ±14 µs | 500 to 503 ms | 51 | 50 to 60 ms | 0 |
| 2 and 4 processors, load 10 and 20 | 8 | 0 to 7 | ±4,616 µs | 500 to 593 ms | 42 | 50 to 62 ms | 1: `xtask`'s marker, the kernel having printed "51 interrupts" |

- **The calibration.** On the large machines a 10 ms window is spoiled about 98 % of the time. With all 32 spoiled the least disturbed window was kept, 0.02 % to 1.5 % off here and 2.9 % off in the records; past 1 % the RTC second refuted it. That is every refuted RTC second recorded from 2026-11-14 to 2026-11-19.
- **The host's load.** The 65-processor QEMU is itself the load: the average rose from 3.8 to 16.6 in one boot and stayed at 20 to 33 across large boots with nothing planted.
- **A probe of QEMU's threads** in one 65-processor boot: all 65 virtual processors busy, filling the host, for about 14 s while the firmware brought them up, then the kernel's early boot. The samples (every 1.5 s) cannot place the calibration in that time, so this neither confirms nor rules out a cause of the spoiled windows.

- **F1 at the gates' own load, without this task's change** (the keeper's `boot-matrix` runs of `land/batch-7`, 2026-11-19, with nothing else on the machine; logs in the keeper's scratchpad, `land/t71-boot-matrix-fail1.log`, `land/t71-boot-matrix-fail1-amd65-serial.log` and `land/b5-boot-matrix.log`):
  - on `39ad03b1` the AMD-IOMMU 65-processor machine discarded 32 windows, the old limit, kept a disturbed one and read 983,917 kHz, and its RTC second, 1,016,848 µs, refuted it; TASKS item 63 B5's final gates on `f3aff5ef` failed the same way, at 1,016,353 µs;
  - in a later run the same machine discarded 31 windows and still read 1,000,118 kHz, its RTC second 999,010 µs, a pass; the 67-processor machine discarded 22, and the Intel-IOMMU 65-processor machine 9.

  So a high count of discarded windows alone does not mean a bad figure: exhausting the old 32 does, which is what R1's 1,024 lifts. On the final code the AMD 65-processor boots discarded 42 and 25 windows and read 1,000,044 and 1,000,003 kHz, both seconds confirmed (§3.1).

## 2. Why the preemption proof does not judge a median of tick gaps (the diagnostic of 2026-11-19)

The first answer to the review's T66-R1 judged each processor's tick rate by the median of its tick gaps on the TSC, within 95 % to 150 % of the period. A diagnostic build printed every processor's counts in each run of the window:

| Machine (load) | Processor-runs | Gaps under 95 % of the period | Gaps over 150 % | The median verdict |
|---|---|---|---|---|
| 2 processors (0) | 2 | 35 % (CPU 1: 18 of 51) | 0 % | in bounds |
| 4 processors (0) | 4 | 20 to 22 % | 0 % | in bounds |
| 2 processors (20) | 2 | 42 % | 11 to 28 % | in bounds |
| 65 processors (0), two boots | 195 | 25 to 48 % | 20 to 47 % | in bounds; one boot's first run stalled and was run again |
| 65 processors (3) | — | CPU 6: 35 of 67 | 15 of 67 | **failed**, the rate being right |
| 67 processors (0) | — | CPU 24: 32 of 63 | 16 of 63 | **failed**, the rate being right |

- **Why the median wanders.** Under TCG a tick delivered late is followed by a short gap, since QEMU keeps the period's phase. So even a quiet two-processor machine has a third of its gaps under 95 %.
- **On the large machines** a processor's median wanders across both bounds: it failed a right rate in 2 of the 4 large boots.
- **As a repeat trigger** instead of a verdict, an estimated 2 % of large boots would still fail after 4 runs.

So the rate is checked structurally instead (the coordinating session's ruling A): each processor's timer is read back and compared with what the boot processor's timed proof ran.

## 3. After the change

### 3.1 The final code

Twenty boots of the usual series on `0df830a8`, 2026-11-19 16:11 to 16:32 UTC, in one turn, and fifteen more on the large machines, 16:38 to 17:06 UTC, from a diagnostic build of the same code that allowed up to 12 runs of the preemption window and printed each run's 15th tick beside its 15th switch. The host's one-minute load average before and after each boot is in the logs' `summary.tsv` (`t66-evidence/answers2-rounds.tar.gz` in the coordination notes).

| Machines (load) | Boots | Host load average | Calibration windows discarded | RTC second, widest uncertainty | APIC timer, 50 ticks | Its count's period, ppm of nominal (pairs) | Preemption window | Failed |
|---|---|---|---|---|---|---|---|---|
| 65 and 67 processors (0) | 22 | 2 to 28 | 1 to 66; 7 needed more than 32 | ±3,952 µs | 500 to 515 ms | 997,710 to 1,005,457 (1 to 32) | 857 to 2,190 ms | 1: the window, stalled in all 4 runs |
| 65 processors (3) | 6 | 19 to 37 | 1 to 20 | ±7,611 µs | 554 to 621 ms | 1,000,056 to 1,002,710 (3 to 10) | 824 to 1,267 ms | 1: the window, stalled in all 4 runs |
| 65 and 67 processors (10) | 2 | 78 to 111 | 1 and 8 | ±8,223 µs | 563 ms (one boot) | 998,353 to 1,001,599 (1 to 13) | — | 2: the window, stalled in all 4 runs; the tick proof, slow in all 4 runs |
| 2 processors (20) | 5 | 33 to 111 | 0 to 3 | ±7,007 µs | 533 to 570 ms | 999,695 to 1,000,900 (3 to 4) | 596 to 646 ms | 1: the ping-pong at run 8 (its own rule, unchanged) |

- **No RTC second was refuted,** and every count's period was within −0.23 % to +0.55 % of nominal. The AMD 65-processor boots discarded 42 and 25 windows and took 1,000,044 and 1,000,003 kHz; before R1 the first would have kept a disturbed window.
- **The preemption window at idle.** In the first round one 65-processor boot failed it, its 4 runs stalled with gaps of 254 to 461 ms and 15th switches at 535 to 782 ms, and another passed at run 2. In the diagnostic round all 15 boots passed at run 1, the latest 15th switch at 491 ms. The deciding processor's 15th switch came with its 15th tick in every run (some other processor's switch lagged its tick by up to 178 ms), so the switch stamp of the addendum review's T66-R15 is not what stalls: a host stall of a few hundred milliseconds is. Such stalls come in episodes, by boot or by round: with the 15th-tick stamp, 0 of 30 idle runs stalled over three rounds, and at load 3 8 of 9 did in one round and none in another.

  | Round (stamp) | Idle large machines: runs stalled | Boots stalled in every run | At load 3: runs stalled | Boots stalled in every run |
  |---|---|---|---|---|
  | `4dc3bff6` (15th tick) | 0 of 10 | 0 | — | — |
  | `66f47442` (15th tick) | 0 of 10 | 0 | 0 of 3 | 0 |
  | `2364d3af` + diagnostic (15th tick) | 0 of 10 | 0 | 8 of 9 | 2 of 3 |
  | `0df830a8` (15th switch) | 6 of 14 | 1 of 10 | 4 of 6 | 1 of 3 |
  | `0df830a8` + window diagnostic (15th switch, up to 12 runs) | 0 of 12 | 0 | 0 of 3 | 0 |

  The logs are in `t66-evidence/answers2-rounds.tar.gz` (rounds `ev8` and `ev9`, with the window diagnostic's patch) and `t66-evidence/r13-diagnostic.tar.gz` (`ev7`), in the coordination notes.
- **The margin is thin, and it is the machines' own speed.** The latest 15th switch on a passing idle run of the large machines was 259 to 491 ms, against the 500 ms bound: 65 or 67 guest processors, all running the proof's workers at once, on the development Mac's 10 threads. It is not a defect, and nothing under KVM or on a physical machine is expected near it. There, as ruled, any repeat is a finding for qualification review.
- **The budget, first 8 runs** (the coordinating session's ruling of 2026-11-19): the longest episode then seen covered 4 runs, about 15 to 20 s.
- **Then a budget of time** (its ruling of 2026-11-20). On `734d36bb` the gates' `boot-matrix` failed the 67-processor machine's window stalled in all 8 runs, with gaps of 251 to 686 ms and 15th switches at 526 to 999 ms, while its tick proof passed at run 1. Over 46 boots of the large machines on the final rule (`ev8`, `ev9`, `ev11` and that `boot-matrix`), at idle or the gates' own load:

  | Window runs a boot needed | Boots |
  |---|---|
  | 1 | 41 |
  | 2 to 4, then passed | 2 (`ev8` intel-65.2, `ev11` sparse-67.3) |
  | failed at 4 (the budget then) | 1 (`ev8` intel-65.1) |
  | failed at 8 | 1 (the gates' `boot-matrix`, the 67-processor machine) |

  An episode outlasting 8 runs came once in about 46 boots, in the gate itself, so a count was not enough. The window now repeats within 120 s of wall time from the first run's start, at most 32 runs, each after a 2 s pause with every processor idle so that an episode can pass. The 500 ms bound and the rule are unchanged: a run is measured again only for a late 15th switch with a gap of two periods, never passed, and only after its correctness clauses held. The cost is up to about 120 s more boot time on a stalled large software-processor machine, and nothing on an unstalled one; xtask's boot timeout for those machines, 242 and 246 s, leaves room. If a gate still fails at 120 s, the next step is the owner's: a re-run of the whole boot on the named machines, as decision item 314 did for lost stacks.
- **With the budget, on `48b9d5ac`** (2026-11-19 19:20 to 19:52 UTC, the gates' `boot-matrix` and then 10 idle large boots in one turn, host load average 7 to 27): every large boot passed the window, 12 of 13 at run 1 and 1 s of the budget, and intel-65.1 at run 2 and 5 s, after one stall of 242 ms. Logs: `t66-evidence/final-gates-48b9d5ac.tar.gz`.
- **The idle round on `734d36bb`** (`ev11`, 10 large boots, host load average 21 to 30): all passed; nine needed 1 run of the window and one 4. Every tick proof passed at run 1, its period 996,186 to 1,003,551 ppm of nominal, from 1 to 14 pairs. Logs: `t66-evidence/final-gates-734d36bb.tar.gz`.
- **At ten busy loops** both large machines were refused, as before TASKS item 66: the 65-processor machine at the window, the 67-processor machine at the tick proof, slow in all 4 runs (1,093 to 1,262 ms for 50) with its count's period in bounds.
- **Extended waits were said:** the preemption workers' exit up to 3,959 ms at load 3 and 11,313 ms at load 10.

### 3.1a The second addendum review's gap rule, tried and reverted

The second addendum review's T66-R17 measured each gap between the period sampler's reads across both reads' brackets (`500f6cd1`), so that a stall inside an intermediate read could not hide a reload. On `8d9d3759` (2026-11-19 17:56 to 18:12 UTC, one turn) it failed the gates, and the coordinating session ruled it reverted (`361c813c`) and R17's case a stated limit, with a check that tolerates stalls as TASKS item 78.

| Rule | Machines and boots | Runs that found a clean pair | Pairs a clean pair took |
|---|---|---|---|
| `0df830a8`: the gap between the brackets | 65 and 67 processors, 37 boots at loads 0, 3 and 10 (`ev8`, `ev9`) | every run | 1 to 32 |
| `500f6cd1`: the gap across the brackets | the gates' `boot-matrix`, the Intel-IOMMU 65-processor boot | none of 4 runs | undecided after 64; the boot failed |
| the same | 10 idle large boots (`ev10`), host load average 13 to 31 | 2 boots: intel-65.4 at run 3, sparse-67.3 at run 1 | 27 and 13; the other 8 boots undecided after 64 in all 4 runs |

- **Delivery was fine** in those runs: 50 ticks in 502 to 515 ms, the longest gaps 13 to 21 ms.
- **The inference, unmeasured:** on these machines an intermediate read of the timer's count often stalls inside its own bracket for over 100 µs, most likely contending for QEMU's big lock under MTTCG. No read's bracket was recorded.
- **Logs:** `t66-evidence/r17-gates-fail.tar.gz` in the coordination notes.

### 3.2 The rounds before it

Each answer was booted in the same series before the next was chosen:

| Code | The rule tried | What the round showed |
|---|---|---|
| `d906221d` (05:32 to 05:54 UTC) | The first build: the tick proof's median gap; the window's 50 ticks | No refusal idle; at load 3 the clock proofs' 2 s wait for their turn-takers failed one boot (then given `OTHERS_PATIENCE`); at load 10 the tick proof failed one large machine and the other's workers did not exit |
| `90ee371b` (09:26 to 09:51 UTC) | A median gap under 95 % counted as too fast | No failure at loads 0 and 3. But the controls T66-12 to T66-14, correct timers with a stall, were stopped at the tick proof: the poll-stamped gaps of a right timer have a median of 9.03 to 9.09 ms |
| `4dc3bff6` (10:03 to 10:20 UTC) | A run with a gap of two periods or more never passes | Almost every run under load has such a gap: all three 65-processor boots at load 3, three of five small boots at load 20 and both at load 10 failed the tick proof at run 4 |
| `66f47442` (10:34 to 10:56 UTC) | A run within its bounds measured again if some ten consecutive gaps add up to under 8.5 periods | No refusal and no repeat at loads 0, 3 and 20; the least span of ten gaps of a correct timer was 89 to 98 ms. The addendum review then showed recurring stalls masking a fast rate (T66-R13) |
| `2364d3af` + diagnostic (13:17 to 13:38 UTC) | The ten-gap rule, with the count measured but not judged (§3.3) | Idle large machines passed; two of three load-3 boots failed the window with the 15th-tick stamp, stalled in all 4 runs |

The 67-processor machine's preemption workers did not exit within 10 s at load 10 in two rounds (`d906221d` and `90ee371b`); its cause is not established.

### 3.3 The timer's period from its count (the addendum review's T66-R13, diagnostic of 2026-11-19)

*(The rule this section led to, one pair whose end brackets are within 1 % of its interval, was replaced by TASKS item 78 on 2026-11-21: fenced reads, a period's bounds, stretches, longer pairs and a time budget. See `docs/research/2026-11-21-apic-timer-period.md`.)*

Before the rate's verdict moved to the timer's count, a temporary build on `2364d3af` measured it in every run of the tick proof without judging it.
- **How:** while the periodic timer ran, it read the timer's current count back to back, each read bracketed by RDTSC. A pair was two reads within one period: 2 ms apart (a fifth of the nominal period), or up to the read before the next reload if that came first, and at least 0.5 ms. A pair was read again if its two brackets together exceeded 1 % of its interval, or if two consecutive reads were more than 100 µs apart (time in which a reload could pass unseen). The period was the initial count times the interval over the count's fall. Each run took up to 8 clean pairs, at most 32 tries each.
- **Boots:** the series of §3.1, 2026-11-19 13:17 to 13:38 UTC, in one turn: 26 runs of the tick proof. Logs: `t66-evidence/r13-diagnostic.tar.gz` in the coordination notes, with the patch and the summariser.

| Machines (load) | Runs | Clean pairs, of those tried | The period, ppm of nominal | Widest uncertainty of a pair | Host load average |
|---|---|---|---|---|---|
| 65 and 67 processors (0) | 10 | 80 of 366 | 996,595 to 1,005,511 | ±4,701 ppm | 3 to 33 |
| 65 processors (3) | 3 | 24 of 106 | 995,454 to 1,004,178 | ±4,459 ppm | 22 to 54 |
| 65 and 67 processors (10) | 8 | 64 of 258 | 998,161 to 1,004,165 | ±4,518 ppm | 103 to 134 |
| 2 processors (20) | 5 | 40 of 66 | 999,579 to 1,004,317 | ±4,748 ppm | 52 to 139 |
| 2 processors (0), one boot | 1 | 8 of 8 | 999,596 to 1,003,360 | ±3,249 ppm | — |

- **The spread:** −0.45 % to +0.55 % of nominal on every machine and load, a pair's own uncertainty included. Every run got its 8 clean pairs; none was undecided.
- **The retries:** on the large machines about three pairs in four were spoiled, and the worst run needed 57 tries for its 8 clean pairs, about 7 for each.
- **The ruling** (the coordinating session, 2026-11-19): a clean pair's period within 97 % to 103 % of nominal, about three times the widest deviation and a 1.1× fast timer outside it; up to 64 tries per run, which at a 78 % spoil rate leaves a run undecided about once in ten million.
- **Also seen in that round:** two of the three 65-processor boots at load 3 failed the preemption window, stalled in all 4 runs, at a host load average of 33 to 54. That was ruling A's rule at the time, before T66-R15; §3.1 gives the final code's figures.

## 4. What this does not show

- Anything under KVM, in CI or on a physical machine (CLAUDE.md §74).
- The cause of the calibration's spoiled windows on the large machines.
- A rate within 3 % of nominal: the tick proof passes it, by its tolerance.
- A timer over 5 times fast *(from about 3.4 times fast: TASKS item 78's correction of 2026-11-21, in ADR-0013)* whose reload a stall inside an intermediate read of its count hides: the count passes it, and its delivery fails it as too fast in total unless recurring stalls also make that total look normal in the same run (the second addendum review's T66-R17; TASKS item 78). *(Closed by TASKS item 78 on 2026-11-21, below 20 times fast: a pair whose slope is not its stretches' is set aside. See `docs/research/2026-11-21-apic-timer-period.md`.)*
- A processor whose timer reads back as the boot processor's but ticks slower, its own timer clock being wrong, between about 30 Hz and 100 Hz: only the preemption proof's floor of 15 switches in 500 ms catches it, below that. One whose own timer clock runs fast: nothing catches it, the count's period being measured on the boot processor alone; on the platforms in scope one clock drives every core's APIC timer.
- Why the 67-processor machine's preemption workers did not exit within 10 s at ten busy loops.
