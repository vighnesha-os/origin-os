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

Twenty boots of the same series on `66f47442`, 2026-11-19 10:34 to 10:56 UTC, in one turn:

| Machines | Boots | Calibration windows discarded | RTC second, widest uncertainty | APIC timer, 50 ticks | Least span of ten tick gaps | Preemption window | Fewest switches | Timed wait | Failed |
|---|---|---|---|---|---|---|---|---|---|
| 65 and 67 processors, load 0 | 10 | 2 to 61; 2 needed more than 32 | ±5,268 µs | 502 to 565 ms | 90 to 97 ms | 754 to 1,218 ms | 50 | 50 to 60 ms | 0 |
| 65 processors, load 3 | 3 | 27 to 49; 2 needed more than 32 | ±5,625 µs | 605 to 641 ms | 91 to 93 ms | 782 to 1,142 ms | 50 | 50 to 59 ms | 0 |
| 65 and 67 processors, load 10 | 2 | 2 and 3 | — | refused | 89 to 98 ms | — | — | — | 2, both at the tick proof (Refuse) |
| 2 processors, load 20 | 5 | 1 to 8 | ±3,047 µs | 510 to 561 ms | 91 to 96 ms | 560 to 623 ms | 50 | 50 to 78 ms | 0 |

- **No refusal and no repeat at loads 0, 3 and 20.** No RTC second was refuted, and no proof needed a second run.
- **Extended waits were said.** At load 3 the preemption workers' exit (2,177 to 4,089 ms) and the clock's turn-takers' start (2,217 and 2,305 ms) went past their former 2 s, and each printed so.
- **The least span of ten tick gaps** of a correct timer was 89 to 98 ms in every run of every boot, those at load 10 included, against the 85 ms (8.5 periods) under which a run within its bounds is measured again. In the controls, a correct timer after a 300 ms stall had 90 to 97 ms, and twice the rate 44 to 49 ms.
- **At ten busy loops both large machines were refused at the tick proof,** as before TASKS item 66:
  - the 65-processor machine was slow in all 4 runs, 943 to 1,283 ms for 50 ticks, with gaps of up to 179 ms;
  - the 67-processor machine failed at its first run on a median gap of 17.4 ms, over 150 % of the period. A merged expiry can also hide in a gap shorter than two periods, so a host this loaded can make a correct timer's median slow, and the contract's rule fails a slow median at once.

  Recorded, not changed: ten busy loops are outside load that the machine's turn exists to prevent. The host's one-minute load average stood at about 110 during these two boots.
- **The weights proof's fairness lines** (Report, which `xtask` forgives on these machines) came on 5 of the 10 idle large boots, as on 4 of 4 before the change.

### 3.2 The rounds before it

Each answer to the review was booted in the same series before the next was chosen:

| Code | The tick proof's rule | What the round showed |
|---|---|---|
| `90ee371b` (09:26 to 09:51 UTC) | A median gap under 95 % of the period counted as too fast | No failure at loads 0 and 3. At load 10 the 65-processor machine passed whole, its tick proof at run 3; the 67-processor machine passed it at run 2 and then waited 16,180 ms for its preemption workers to exit and was refused. At load 20 the ping-pong failed one boot by its own rule. But the controls T66-12 to T66-14, correct timers with a stall, were stopped at the tick proof: the poll-stamped gaps of a right timer have a median of 9.03 to 9.09 ms |
| `4dc3bff6` (10:03 to 10:20 UTC) | A run with a gap of two periods or more is stalled whatever its total, measured again, never passed | Idle large machines passed, a few at run 2. But almost every run under load has such a gap: all three 65-processor boots at load 3, three of five small boots at load 20 and both at load 10 failed the tick proof at run 4 |
| `66f47442` | A run within its bounds is measured again only if some ten consecutive gaps add up to under 8.5 periods | §3.1 |

The 67-processor machine's preemption workers did not exit within 10 s at load 10 in two earlier rounds (`d906221d` and `90ee371b`); its cause is not established.

## 4. What this does not show

- Anything under KVM, in CI or on a physical machine (CLAUDE.md §74).
- The cause of the calibration's spoiled windows on the large machines.
- A slow tick between about 30 Hz and the programmed 100 Hz with its timer programmed right: no timing clause catches it, as before TASKS item 66 (the preemption proof's floor is 15 ticks in 500 ms).
- A tick 5 % to 15 % fast whose total a stall made look right: the tick proof passes it, as before TASKS item 66.
- Why the 67-processor machine's preemption workers did not exit within 10 s at ten busy loops.
