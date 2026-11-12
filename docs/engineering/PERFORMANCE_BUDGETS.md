# Performance budgets and the cost ledger

| | |
|---|---|
| Status | **Provisional.** Every budget here is a hypothesis (CLAUDE.md §53; decision item 115). No figure is from a declared machine, and none passes or fails anything |
| Decided by | Decision item 115 (the file); ADR-0018 V5 (its two tables) and V6 (it comes before any new figure). Built by TASKS item 26 |
| Owner | The Chief Performance Architect (role 23) sets the budgets. The Benchmarking and Regression Lead (role 36) checks the figures |
| Trends as of | `main` at `04ec842`, CI run 43131676181 of 2026-11-12. Every value: [PERFORMANCE_TRENDS_2026-11-12.md](PERFORMANCE_TRENDS_2026-11-12.md) |
| Review | No person has reviewed it. The budgets are Claude's, and nobody has measured against them |

**This file claims nothing.** Until a declared machine gives a figure, the most any document says of Origin is "no known avoidable cost; unmeasured on a real processor" (ADR-0018 V9). The cost ledger below is still empty, so even that has not been looked for yet.

## How to read it

- **A budget** is what a path should cost at the median on a declared machine (ADR-0009 Amendment 1), with every protection on. It is a target with a model, written before the figure it will be held to (CLAUDE.md §1, "Establish performance budgets before measurement").
- **A model** counts the steps in the path, so that a figure can be checked against its parts.
- **A trend** is a figure the kernel printed in CI. `emu` is QEMU's software processor. `virt` is a KVM guest on a cloud runner: a real processor, behind a host. Neither is `hw`, and neither is compared with a budget.
- **Nothing fails CI on a figure** until declared hardware exists (item 115; ADR-0018 V6).

## The budgets

Trends are medians, with the least and the greatest in brackets. The first `virt` figure is the guest on an EPYC 7763 (Zen 3, older than the declared scope), the second the guest on an EPYC 9V45 (Zen 5).

### Paths the boot times today

| # | Path | Budget (hypothesis) | Model | Trend, `emu` | Trend, `virt` |
|---|---|---|---|---|---|
| B1 | A context switch between two kernel threads | 300 ns | One context saved, one loaded, one step of the run queue | 1,502 ns (1,439 to 1,656) | 354 ns (351 to 464); 260 ns (248 to 281) |
| B2 | Wait-queue ping-pong, woken by IPI, one round trip | 5 µs | 2 IPIs, 2 wake-ups of a processor in its shallowest idle state, 2 of B1 | 16 µs (9 to 23) | 35 µs (27 to 40); 27 µs (25 to 36) |
| B3 | Channel ping-pong between two processes on two processors, one round trip | 8 µs | Up to 6 system calls (each side writes, waits in `wait_one` and reads), 2 wake-ups as in B2, 2 of B1 | 72.6 µs (52.0 to 80.2) | 59.2 µs (3.5 to 72.3); 58.4 µs (2.4 to 72.2) |
| B4 | A port: a bind, a signal that delivers it, a take and a clear | 400 ns for the four | Four of the kernel's own operations on one object, 100 ns each | 12.1 µs (11.6 to 18.2) | 369 ns (348 to 438); 377 ns (345 to 682) |
| B5 | A read of the monotonic clock in the kernel, alone | RDTSC and 10 ticks more | One RDTSC and one more access to a shared variable (ADR-0012 C8) | 100 ticks, RDTSC 23 | 29 ticks, RDTSC 28; 26 ticks, RDTSC 23 |
| B6 | A random request of up to 64 bytes | Under 5 µs (ADR-0016 R12, in its words) | 8 SHA-512 compressions | 15.5 µs (13.3 to 23.0) | 7.0 µs (4.6 to 7.1); 3.2 µs (2.4 to 3.6) |
| B7 | A lookup held in bounds (ADR-0017 Z9), its loop included | 2 ns masked; 10 ns fenced | A compare and a mask; or one LFENCE | 3.0 ns masked, 2.8 ns fenced | 1.2 ns and 7.9 ns; 0.5 ns and 6.1 ns |
| B8 | A map and an unmap of a page under a thread running on another processor | 3 µs | Two changes to a page table, and one shootdown round to one running processor: an IPI, an INVLPG, an acknowledgement | 7 µs (3 to 11) | 19 µs (0 to 21); 19 µs (0 to 21) |
| B9 | One IBPB | **None yet** | No vendor gives a figure (ADR-0017 Z3). The first declared processor that offers the barrier prints one, and the budget is set from it | not printed | not printed |
| B18 | The longest stretch with interrupts off, once the system runs | 50 µs | The longest single section of any kind: a UART section, a framebuffer section, or one step of the generator (ADR-0016) | Generator 140 µs; UART section at worst 172 µs (67 to 4,490); framebuffer section at worst 485 µs (37 to 3,031) | Generator 19 µs and 9 µs; UART at worst 851 µs (491 to 4,617) and 1,371 µs (333 to 5,331); framebuffer at worst 199 µs (178 to 5,052) and 533 µs (181 to 3,480) |

Printed today with no budget, as trends only:

| Path | Why it has no budget yet | Trend, `emu` | Trend, `virt` |
|---|---|---|---|
| All processors reading the clock at once | ADR-0012 C8 accepts that it scales poorly, and decides a remedy only when a hot reader appears | 288 ticks (80 to 557) | 103 ticks (28 to 177); 90 ticks (25 to 132) |
| The ports' round trip through the script interpreter | The proof's interpreter is in the path, and the kernel's line says "no budget" | 229 µs (61 to 323) | 93 µs (4 to 305); 115 µs (3 to 342) |
| The launch path: placing the start record; then granting, writing and starting | The accepted figure is ONBF's: 5% of added cold-launch latency on the reference hardware. It is a figure for loading an `.ose`, not for this path | 1,290 µs; 48 µs | 13 µs and 8 µs; 13 µs and 5 µs |
| A kernel stack: made from fresh frames; made from a kept frame; freed | No caller is hot | 5.0 µs; 18.4 µs; 4.2 µs | 3.1 µs, 768 ns and 989 ns; 2.7 µs, 739 ns and 542 ns |
| A memory object's map, protect and unmap of 16 pages, in a space no processor holds | No caller is hot. A budget for each page belongs with B8 and B13 | 23 µs; 27 µs; 19 µs | 3 µs, 3 µs and 2 µs; 1 µs, 3 µs and 1 µs |
| An unmap of a shared object's page under a thread that reads it | The same kind of path as B8, which has the budget | 32 µs (18 to 39) | 20 µs (1 to 37); 18 µs (0 to 20) |
| The directory exchange: three launches to their ends | It times three processes' whole lives, the waits for their ends included | 8.8 ms (4.1 to 15.0) | 6.6 ms (0.1 to 9.8); 6.2 ms (0.1 to 9.9) |
| A line drawn on the boot console's framebuffer | It is the boot console, not the compositor | 1.0 ms | 4.3 ms; 4.1 ms |
| Clearing bus mastering on every PCIe function | It runs once at boot, against QEMU's device model | 53 ms (24 to 75) | 879 ms (770 to 896); 800 ms (708 to 861) |
| A virtio-net ping, average round trip | QEMU's device and its user network | 142 µs | 79 µs; 62 µs |
| An NVMe read of 8 blocks, by interrupt | The device is QEMU's model. The budget comes with the first declared controller (CLAUDE.md §33) | 83 µs (73 to 146) | 118 µs (99 to 126); 83 µs (75 to 90) |
| A processor online after its start | It runs once for each processor at boot, and nobody has looked at where the time goes | 11.5 ms (10.0 to 12.4) | 10.6 ms (10.5 to 11.5); 10.5 ms (10.5 to 11.6) |
| A deadline's worst lateness | Three deadlines on an idle machine show too little to set one | 0 µs | 0 µs; 0 µs (0 to 1) |

What the trends already show, and what they do not:

- **B6 on the older guest is over its budget's figure**: 7.0 µs against "under 5 µs". The processor is a Zen 3, outside the declared scope, and a guest. It is recorded, and it decides nothing.
- **B2, B3 and B8 are far over theirs under KVM.** A virtual processor that halts is woken through the host, and the least values (2.4 µs for B3) show the path without that. Only a physical machine can say which the path is.
- **B9 has no figure at all.** It is the largest cost ADR-0017 names. Each service call made on one processor pays two (ADR-0011 L12).
- **B18 is far over its figure for the console in a guest.** A section that writes to the UART or draws on the framebuffer kept interrupts off for as long as 5.3 ms. In a guest each access to those devices leaves the guest. What a section costs on a physical UART is not known, and the generator's step, which touches no device, printed 9 µs on the Zen 5 guest.
- **Clearing bus mastering took most of a second in a guest**, against 53 ms on QEMU's software processor. It is boot time, on a device model, and it is recorded for the boot-to-login budget (B16).

### Paths with no timing yet

Budgets for what the review of 2026-11-10 listed (finding 10) and for ADR-0011 N8's three costs. Each is an assumed target. None has a figure of ours behind it.

| # | Path | Budget (hypothesis) | Model | Who prints the first figure |
|---|---|---|---|---|
| B10 | A null system call | 150 ns | SYSCALL in, one lookup as in B7, IRETQ out (the kernel returns by IRETQ by design: `THREAT_MODEL_SYSCALLS.md` E5) | Step 4's measuring client (ADR-0011 L11) |
| B11 | A null call through `origin.core` | B10 and 10 ns | One function call around B10 | The same |
| B12 | A typed service round trip: client to service and back | **Written by step 4's task S4-6 before it measures** (ADR-0011 L11) | Its parts on one processor: 6 system calls, 2 switches between processes, 2 of B9 | The same |
| B13 | A page fault answered from a ready frame | 1 µs | An exception in, one lookup, one change to a page table, IRETQ out | No path prints it yet |
| B14 | Input to display | Shown no later than the second refresh after the input: 33.3 ms at 60 Hz, 16.7 ms at 120 Hz | An input that just misses the compositor's deadline waits one interval, and its frame takes one more | The compositor's task (M8) |
| B15 | Frame time | p99 within one refresh interval; p99.9 within two | No missed deadline in 99 frames of 100, under the declared background load | The same |
| B16 | Boot to login | 3 s from `ExitBootServices` to a login screen that takes input, the boot proofs' own time reported beside it | Assumed. The parts are the loader's verification, the kernel's start, the storage driver, the services and the compositor | The installed system's task (M9) |
| B17 | Wake-ups of an idle processor | **None yet.** The scheduler tick is 100 Hz (ADR-0003), and no code stops it on an idle processor | Fewer wake-ups need an idle processor to stop its tick, which is a decision of its own | Power management's task (M10) |

## The cost ledger

One row for each place the design pays on each operation, or pays a price nobody has measured (ADR-0018 V5). A cost is stated in one of ADR-0018 V3's three forms.

| Path | What the cost buys | Estimate, and its model | Measured, and where | Cheaper design considered | What would lift it |
|---|---|---|---|---|---|

**It has no row yet.** Its first fill is a task of its own: reading the Trade-offs of ADR-0001 to ADR-0017 and the threat models, and listing each ADR with its rows or with "none" and the reason (ADR-0018 V5, V8).

## Adding a figure

1. Write the path's budget and its model here first, as a hypothesis (ADR-0018 V6).
2. Then record the figure as a trend, with its environment (`emu`, `virt` or `hw`), the revision and the run, and keep the raw values in a dated record beside this file.
3. A figure from a declared machine names its record under `docs/hardware/records/`. A rented server's figures are bench figures (decision item 233).
4. A protection's price is measured as a pair of kernels (ADR-0018 V7), and goes into the ledger.

## What is not here

- No threshold and no CI job that fails on a figure.
- No figure from a physical machine, and no figure with any of ADR-0017's controls on.
- Tail latencies. The boot prints one figure a boot, so these are medians over about 20 boots that differ in configuration. p95 and beyond need a measuring client that takes thousands of samples (ADR-0011 L11 asks for 10,000 a case).
- Energy, memory use and throughput budgets (CLAUDE.md §1, role 23). No path measures them yet.
