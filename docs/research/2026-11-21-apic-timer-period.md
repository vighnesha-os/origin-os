# Research ledger: the APIC timer's period from its count, when reads stall or cost more

| | |
|---|---|
| Date | 2026-11-21 |
| Question | (1) Why does the tick proof's period check refuse a correct timer under KVM on one runner's processor (a Xeon Platinum 8370C, CI run 44837410914), and what rule decides it there without passing a fast timer whose reload a stall hides (the second addendum review's T66-R17)? (2) Does a read's bracket of TSC readings enclose the instant the timer's count was taken, as every rule here assumes? (TASKS item 78; the coordinating session's rulings Q1 to Q8 and its rulings on the round.) |
| Method | (1) A diagnostic round: kernel boots of the `kernel boot` scenario alone, each inside one turn of the machine (`cargo xtask exclusive --`), the host's load average read before and after each, in an export of the branch's code at `fcef041d` that was patched and never committed. The live verdict stayed today's rule in every boot. Every candidate rule was fed one stream of the running timer's count, read for 64 fifths of a period straight after the live sample, so each saw the same reads. Plants stood in for what QEMU's software processor does not do. (2) The vendors' manuals, read on 2026-11-21 at today's revisions, piped from `curl` into `pypdf` in memory, nothing saved. Facts are paraphrased with section and page. No text is copied. |
| Host | The development Mac (Apple M1 Max, 10 threads), QEMU 11.1.3 TCG with OVMF, Rust 1.100.0. Under TCG the guest TSC is the host's nanosecond clock, 1,000,000 kHz. |
| Follows | `docs/research/2026-11-19-timing-proofs.md` §3.3 and §4 (the period from the count, and the stated limit T66-R17); `docs/research/2026-11-10-speculation-processor-hardening.md` (LFENCE and MSR C001_1029 on AMD, rows A3 to A5) |
| Reviewed by | One AI reviewer, for the code built from it (`docs/engineering/REVIEW_T78_2026-11-21.md`). **No person has reviewed this ledger.** |
| Other sources | None. No source of QEMU, OVMF, KVM or any operating system was opened (CLAUDE.md §3). |

## 1. Sources

| ID | Document | Revision and date | How read | SHA-256 |
|---|---|---|---|---|
| I1 | Intel, *Intel 64 and IA-32 Architectures Software Developer's Manual*, Volumes 2A to 2D (325383) | -093, September 2026, still the newest on 2026-11-21. 11,294,447 bytes | `cdrdv2.intel.com/v1/dl/getContent/671110`, in memory: the LFENCE, RDTSC and RDTSCP entries | `d137a7887787bbb06c1a8b8bafd51958d8d2e16b4dff987a03226c845cca1516`, as `2026-11-20-m3-scheduling.md` row I2 |
| I2 | Intel SDM, Volumes 3A to 3D (325384) | -093, as I1. 9,589,981 bytes | `…/671447`, in memory: §13.12.3 and §20.17 | `77919be320f35da2782771ef2cb65a868cf68e62c8f8279f2ca61199cb70d1f8`, as that ledger's row I3 |
| A1 | AMD, *AMD64 Architecture Programmer's Manual Volume 2: System Programming* (24593) | Rev. 3.46, 2026-10-28. 15,813,681 bytes | AMD's portal, document `LNBl5evreogi0r25VIFF2g`, in memory: §7.6.4 and §16.11.2 | `e4130d5aa26227b6a116d75af9e0f6e5adb4c034ff0d49a3c0fe73c3f49794cf`, as the speculation ledger's A1 |
| A2 | AMD, *AMD64 Architecture Programmer's Manual Volume 3* (24594) | Rev. 3.39, 2026-10-28. 16,226,519 bytes | the portal, document `BqEoC2FadIAgbrUdEzDJIg`, in memory: the LFENCE, RDTSC and RDTSCP entries, and CPUID Fn8000_0021_EAX | `b598514b19f204d973040d9867a7ffc48c949009a8b3dc86b20c9188e721ee45`, as the speculation ledger's A2 |
| A3 | AMD white paper, "Software Techniques for Managing Speculation on AMD Processors" (70628) | As the speculation ledger's A3. 1,001,827 bytes | the portal, document `OJEWMABowiTsPL~bAIv77g`, in memory: page 4 | `d86742c1bf84c7c9a543e014a42152d525bd3cda05eb7f67b453fe5df72036a6`, unchanged since 2026-11-10 |
| A4 | AMD, *Processor Programming Reference for AMD Family 1Ah Model 51h, Revision B0* (58264), Volume 1 of 6 | **Rev. 0.50, 2026-11-05** (the portal: 2026-11-13). It replaces Rev. 0.45, which the speculation ledger read. Archive 6,082,752 bytes; volume 1, 3,185,813 bytes | the portal, document `jnTWWU_wKBBltQOmlj040A`, the ZIP opened in memory: CPUID Fn8000_0001_EDX and Fn8000_0021_EAX | archive `5cdf8fbf1e34c6dfc75874f9c02ee18b670e036fe778b8403cde05edb7576b97`; volume 1 `61725c23c794d31f120564cb84b9fc4f92ce6f9a8ef09fe88d052423688987d9` |
| K1 | The kernel, branch `claude/t78-period-check-1b3ec5` | `fcef041d`, 2026-11-21 | read in the worktree | — |

## 2. Whether a read's bracket encloses the count (Q7)

`read_count` at K1 takes the TSC, reads the timer's current count through its x2APIC MSR, and takes the TSC again: plain RDTSC, RDMSR, plain RDTSC, with no fence (`timer.rs`, `cpu::rdtsc`). Every rule here, today's included, treats the two readings as bounds on the instant of the count.

**What the vendors say.**
- **RDTSC is ordered with nothing, on both.** Intel: it may read the counter before earlier instructions have executed, and later instructions may start before it reads. LFENCE just before it makes it wait for everything earlier. LFENCE just after it keeps everything later from starting first (I1, Vol. 2B 4-559, PDF page 1262; I2, §20.17, Vol. 3B 20-43). AMD: it is not serializing, so earlier instructions need not have completed when it reads (A2, page 443, PDF page 480).
- **RDTSCP waits for what came before.** Intel: it waits until every earlier instruction has executed and earlier loads are globally visible, but later instructions may start before it reads. LFENCE just after it closes that (I1, Vol. 2B 4-561, PDF page 1264). AMD: it makes every older instruction retire before it reads the counter (A2, page 445, PDF page 482). AMD's white paper also calls RDTSCP dispatch serializing, and slower than LFENCE for that use (A3, page 4, mitigation G-2).
- **RDMSR is not serializing, x2APIC registers included, on both.** Intel: reading an APIC register in x2APIC mode through RDMSR keeps RDMSR's non-serializing nature. Accesses to the APIC registers occur in program order among themselves (I2, §13.12.3, Vol. 3A 13-41 and 13-42). AMD: the same, and RDMSR and WRMSR to x2APIC MSRs execute in program order with respect to each other only (A1, §16.11.2, page 691).
- **LFENCE.** Intel: it does not execute until every earlier instruction has completed locally, and nothing later starts until it completes (I1, Vol. 2A 3-552, PDF page 670). AMD: it orders loads and, in some systems, is configured to be dispatch serializing. Where CPUID Fn8000_0021_EAX bit 2 (LFenceAlwaysSerializing) is 1, it always is (A2, page 221, PDF page 259; A1, §7.6.4). A dispatch-serializing instruction makes itself and everything earlier retire before the next instruction executes (A1, §7.6.4). The white paper puts it in its own words: dispatch stops until the instruction is the oldest in the machine (A3, page 4).
- **What makes LFENCE serializing on AMD.** MSR C001_1029 bit 1 (mitigation G-2). Families 10h, 12h and 14h to 17h have the register, 0Fh and 11h always serialize and lack it, and AMD plans the register for every later processor (A3, page 4). On the processors in scope, the Family 1Ah Model 51h PPR, read today at its new revision, reports LFenceAlwaysSerializing fixed at 1 (A4, volume 1 page 135) and RDTSCP fixed at 1 (A4, volume 1 page 105). The speculation ledger found the same in Family 1Ah Model 02h's PPR (its row A5).

**So the bracket is not established today.** Nothing in either vendor's text keeps the first RDTSC from reading after the RDMSR has sampled the count, or the second from reading before it. The error is bounded by how far the processor reorders, for which no document read gives a figure.

**The decision: fence the bracket**, in `read_count` and for every rule. The first reading is RDTSCP then LFENCE, or RDTSC then LFENCE where there is no RDTSCP. The count's RDMSR follows. The second reading is RDTSCP, or LFENCE then RDTSC.
- **With RDTSCP** (CPUID Fn8000_0001_EDX[27]; every processor in scope has it):
  - **Intel:** LFENCE waits for the first reading and holds the RDMSR back until it completes; RDTSCP waits for the RDMSR to have executed.
  - **AMD:** RDTSCP retires everything older before it reads, and, being dispatch serializing (A3), lets nothing younger execute first. **No processor setting is relied on.**
- **Without RDTSCP:** the order rests on LFENCE being a barrier. That holds:
  - on Intel, always;
  - on AMD where bit 2 reports it;
  - on AMD where the kernel has set MSR C001_1029 bit 1: ADR-0017's plan `Lfence::KernelSets`, written by `cpu::init_speculation` from `load_tables`, on every processor, before `timer::init` can run, since it waits for `cpu::tables_ready`.

  Under a hypervisor the kernel does not write that register. So **an AMD guest with neither RDTSCP nor bit 2 keeps unordered brackets, as today**, and its ADR-0017 record already names an LFENCE not known to be a barrier.
- **CPUID is read once,** when the tick proof starts. Under a hypervisor CPUID leaves the guest, which would itself widen the bracket.

## 3. The diagnostic round (Q3; 2026-11-21, 03:35 to 04:44 UTC)

33 boots in three turns, no other session's QEMU running in any (`t78-evidence/` in the coordinating session's notes holds every boot's lines, load averages and the patch).

**Candidates.** All judge pairs of reads as `sample_period` does: from one read to a fifth of a period, or to the read before a reload, at least a twentieth long.
- **C0:** today's rule. Both end brackets together within 1 % of the interval, then the period within 97 % to 103 %. A gap over P/100 breaks a pair. 64 pairs.
- **C1/δ:** C0, with every step across brackets at most P/δ.
- **C2:** the pair judged by its whole uncertainty interval, from the shortest and longest time its reads can be apart and its fall ±1 count. Inside 97 % to 103 % passes, outside fails, straddling is undecided.
- **C3/δ:** C2, with stretches of steps of at most P/δ. A pair whose slope interval misses its stretches' is set aside. One whose stretches' upper slope is over twice its lower one is undecided.
- **C4/δ:** C3, with a pair undecided at a fifth going on toward the reload, up to 0.9 of a period.
- **B:** a run bounded by 64 fifths of a period, with broken steps not counted.

**Stalls on the idle machines (diagnostic stream of 128 ms):**

| Machines | Widest bracket | Widest gap | Widest step | Steps over 2 ms | Longest stretch of steps ≤ P/100 |
|---|---|---|---|---|---|
| 1 and 4 processors (3 boots) | 43 µs to 1.4 ms | 384 to 468 µs | 0.39 to 1.7 ms | 0 | 3.3 to 125 ms |
| 65 and 67 processors (14 boots) | 4.3 to 8.9 ms | 0.95 to 11.8 ms | 6.7 to 15.6 ms | 14 to 49 | 0.26 to 1.25 ms |

**Idle machines (17 boots; C4/20B and C4/10B in 5).** C0, C2, C3 and C4 decided every run, in bounds. C2 and C3 decided the same pair, at the same value and moment, as C0 every time. C4 did too, except once: its longer pair decided earlier, at 999,752 ppm after 29.5 ms against C0's 1,001,960 ppm after 42.3 ms. The C1 family, which bounds every step, was undecided in 1 to 15 of the 17 runs. The worst time to decide was 49 ms. No candidate judged a correct timer wrong.

**Plants:**
- **(p1) Every read's bracket held open,** the model of the 8370C under KVM. C0 refused the correct timer in every boot with the 8370C's lines: four runs, each `period undecided after 64 pairs`, the interrupts on time. Decided in bounds:

  | Bracket per read | C0 | C2 | C3/20B | C3/100B, C4/100B | C4/20B, C4/10B |
  |---|---|---|---|---|---|
  | 30 µs | 0 of 4 | 4 of 4 (±1.5 % to ±1.7 %) | 4 of 4 | 4 of 4 | not a candidate yet |
  | 60 µs | 0 of 8 | 7 of 8 (±2.9 %) | 7 of 8 | 0 of 8 | 4 of 4 |
  | 120 µs | 0 of 8 | 0 of 8 | 0 of 8 | 0 of 8 | 4 of 4 (±2.6 % to ±2.9 %) |
  | 200 µs | 0 of 4 | 0 of 4 | 0 of 4 | 0 of 4 | 4 of 4 (±2.94 % to ±2.97 %) |
- **(p2) A fast timer whose reload one held read hides,** delivery masked to about 500 ms in total. At 3.6× fast with read 3 at 0.38 of a period, one boot of three passed the tick proof under C0: `period 972763 ppm from 1 pairs`, the timer's true rate 277,816 ppm. In the other two a gap broke the held pair. The diagnostic stream's held pair was broken in every boot, probably by QEMU translating code not run before (not shown), so the candidates' behaviour on it rests on host tests.
- **(p3) A correct timer read the same way:** every candidate in bounds.

**Host tests of the candidates (30, all passed).**
- C0 replays `sample_period` exactly.
- The hidden reload at 3.42× and 3.5× passes C0 and C2, and nothing with a step bound or a stretch.
- No candidate judges a correct timer wrong under 4 stall frequencies × 7 stall lengths × 4 bracket widths.
- No stretch candidate is in bounds for a reload one read hides, at 2.2×, 2.5×, 3.42×, 5×, 10×, 15× and 19× fast, with the hold at each of 351 placements from 0.05 to 1.8 periods, below its own bound.
- C4/20B decides 60, 120 and 200 µs brackets and is never in bounds for a timer 6 % out.

**Ruled (the coordinating session, 2026-11-21):**
- C2, then C4/20B with B, as separate commits landed together;
- the fence first (§2);
- CL20 in ADR-0018 V3's form, its worst case unchanged.

## 4. What this does not show

- The 8370C's own figures under KVM, until a CI run of the batch draws it with the figures line. Slow reads as the cause there is the (p1) model reproducing its lines, not a measurement.
- How far a processor reorders an unfenced RDTSC around an RDMSR: no document read gives a figure.
- Anything on a physical machine (CLAUDE.md §74).
