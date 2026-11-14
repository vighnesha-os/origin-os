# Performance budgets and the cost ledger

| | |
|---|---|
| Status | **Provisional.** Every budget here is a hypothesis (CLAUDE.md §53; decision item 115). No figure is from a declared machine, and none passes or fails anything |
| Decided by | Decision item 115 (the file); ADR-0018 V5 (its two tables) and V6 (it comes before any new figure). Built by TASKS item 26 |
| Owner | The Chief Performance Architect (role 23) sets the budgets. The Benchmarking and Regression Lead (role 36) checks the figures |
| Trends as of | `main` at `04ec842`, CI run 43131676181 of 2026-11-12. Every value: [PERFORMANCE_TRENDS_2026-11-12.md](PERFORMANCE_TRENDS_2026-11-12.md) |
| Review | No person has reviewed it. The budgets and the ledger's rows are Claude's, and nobody has measured against them |
| Gate | `cargo xtask cost-gate`, run by `cargo xtask check` (ADR-0018 V8): the ledger's rows and its list of ADRs, and the "Performance cost" statement of every ADR and amendment accepted after ADR-0018. Its rules are in `developer-tools/xtask/src/costs.rs` |

**This file claims nothing.** Until a declared machine gives a figure, the most any document says of Origin is "no known avoidable cost; unmeasured on a real processor" (ADR-0018 V9). The cost ledger below has had its first fill, and it names two costs that may be avoidable (CL5, CL36), so even that claim cannot be made yet.

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

### Memory the kernel keeps for itself

The kernel-wide bound (ADR-0005 G5; TASKS item 55, task S4-3) sizes the kernel's root once at boot: what the free frames hold, less a reserve for the kernel's own use. G5 decides that a reserve exists; its size is a budget figure, the kernel's to choose (the coordinating session's ruling of 2026-11-13).

| # | What | Budget (hypothesis) | Model | Trend, `emu` | Trend, `virt` |
|---|---|---|---|---|---|
| B19 | The reserve the kernel keeps out of the root at boot | The larger of 64 MiB and an eighth of the free memory at sizing | The reserve takes everything the root does not count, and the hypothesis is judged against the whole list: the kernel's own use after the root is sized (its drivers' rings and buffers, its block cache, its own threads); the slab table's slack, at most its 4 MiB; the frames a stalled shootdown keeps for ever (T3); kernel stacks after their threads are reaped, up to 16 of 16 KiB kept for reuse and the retired ones until they are reclaimed; a process's own block and account, freed just after their payment comes back (a few KiB each); a thread object a handle keeps after its thread is reaped; the concurrent growth of one port's index; undelivered messages between their refund and their reaping (row O11 of the system-call threat model lists each, with why it is bounded). A system call's temporaries are counted in each thread's figure since the independent review of task S4-3 (finding B1), and the objects a process's mappings held come back only after its space has gone (finding B2), so neither is the reserve's. An eighth scales it with memory on a machine whose caches and drivers grow with its size. **Per processor (ADR-0008 Amendment 1, D4; TASKS item 58):** the per-CPU tables (5,062 bytes an index on the boot that sized them) and each application processor's stacks (a 64 KiB stack, four 16 KiB interrupt stacks, the generator's 8 KiB, their tables) are taken at discovery, before the root is sized, so they are in neither; what a processor takes after the root is sized comes out of the reserve: its share of the kernel's own threads and their stacks, its NVMe queue's pages where it has one (D6), and the stacks of a processor brought online later (D2's reserved indexes, not started by any task yet). That line is a hypothesis too, with no figure: the formula stays until a boot at the counts tested shows the kernel's use after sizing past it, and a per-processor term then goes through the coordinating session. Neither number is measured: the kernel's own use after boot is printed by no proof | On the development Mac's 512 MiB machine: 64 MiB of 501 MiB free, a root of 437 MiB (TASKS item 55's boot) | None yet |

- **An eighth of a large server's memory is a large reserve**: 8 GiB of 64 GiB. Whether that is too much, or too little for its caches and drivers, is judged on declared hardware, not here.
- **What would change it:** a figure for the kernel's own use after boot (its heap and frames at the end of the boot proofs, and under load), and a figure of what a machine in the declared scope needs for its devices.

## The cost ledger

One row for each place the design pays on each operation, or pays a price nobody has measured (ADR-0018 V5). A cost is stated in one of ADR-0018 V3's three forms.

**First fill, 2026-11-13 (TASKS item 54),** from the Trade-offs and Consequences of ADR-0001 to ADR-0017, every place one of them names a cost, and the four threat models, read at `main` `4097413`. Every row is Claude's reading. No person has checked that the list is complete, and none of the estimates is a figure.

**How a row is named.** Each row has an ID, `CL` and a number, which never changes and is never reused. The ID column is beside V5's six columns, which are unchanged. An ADR or amendment written from now on that states a cost "Measured" or "Unmeasured, owed" names the row it adds or changes by that ID (ADR-0018 V8), and any ADR text that names a row, an older ADR's evidence included, is held to name one that exists.

| Row | Path | What the cost buys | Estimate, and its model | Measured, and where | Cheaper design considered | What would lift it |
|---|---|---|---|---|---|---|
| CL1 | Loading a native image | ADR-0001 D3, ADR-0001 def5 | Each image: one Ed25519 and one ML-DSA-65 verification over its statement, and SHA-256 over each 4 KiB page against its Merkle root. ONBF's budget is at most 5% added to a cold launch (`docs/spec/ONBF.md`) | Unmeasured, owed: no Loader Service exists, and the kernel's carried programs are compiled in (S4-1) | None recorded: the hybrid suite is chosen for trust (D3) | Nothing foreseen (P0). Measured against the 5% when the Loader Service loads an image |
| CL2 | A third-party driver's interrupts and requests | ADR-0001 D2 | Each interrupt or request: an IPC message and a switch into the driver's process, with an IBPB where the processor has one (CL28) | Unmeasured, owed: "That overhead is to be measured" (D2). No third-party driver exists | Third-party code in ring 0, refused: a ring-0 binary voids the framekernel's argument (D2) | Nothing foreseen (P0) |
| CL3 | The loader's check of the kernel, once a boot | ADR-0002 B2 | SHA-512 over the header and section table; one Ed25519 and one ML-DSA-87 verification; SHA-256 over the manifest and the relocations; each segment against its Merkle root | Unmeasured, owed: no boot prints it | None recorded | Nothing foreseen (P0, and already once a boot, ADR-0018 V4's second place). Budget B16 counts it |
| CL4 | The scheduler tick on every processor, idle ones included | ADR-0003 K5 | 100 timer interrupts a second on each processor, each an entry, a step of the run queue and a return; an idle processor is woken by each (budget B17) | Unmeasured, owed | TSC-deadline mode and a 1 kHz tick, K5's alternatives. Stopping the tick on an idle processor is not decided | A decision for idle processors with power management (M10) |
| CL5 | The return from each system call | ADR-0003 K8; `THREAT_MODEL_SYSCALLS.md` E5 | IRETQ in place of SYSRET; Intel's manual lists IRET among the serializing instructions | Unmeasured, owed: part of budget B10's model | SYSRET, refused for its hazard (E5). E5's second defence, the top page of the lower half never mapped, already keeps a `syscall` from sitting where the return address would not be canonical. Whether IRETQ is then avoidable is open, and no ADR has looked | FRED's ERETU on every processor that has it (ADR-0017 Z6). On the IDT path, a recorded argument that SYSRET's hazard cannot arise |
| CL6 | A message's stamp of who wrote it | ADR-0005 B1, ADR-0005 C1; ADR-0011 N9 | Each message: 16 bytes (`{u64 process; u64 principal}`) written with it and read with it | Unmeasured, owed: inside the channel ping-pong (budget B3), not apart from it | An endpoint that names the caller in the data, refused: services would trust a claim, not the kernel's stamp (ADR-0014 Amendment 1's alternatives) | Nothing foreseen: services authorise by it (P0) |
| CL7 | A program's clock read | ADR-0005 C13 | Each read in ring 3: a system call (budget B10) and the kernel's read with the floor (CL19) | Unmeasured, owed: step 4's measuring client prints `clock_read` beside the null call (ADR-0011 L11) | A shared read-only clock page, which C13 leaves for later "with its own ordering argument and measurements" | That page, once its ordering argument holds (ADR-0012 C7) and a figure shows the call matters |
| CL8 | An unmap or protect, by its shootdown | ADR-0005 D3; `THREAT_MODEL_ADDRESS_SPACES.md` T1, T6 | Each call that removes or narrows a mapping: an IPI to each processor that has the space loaded, an INVLPG there, and a wait for every answer (budget B8) | Budget B8's trend, a map and an unmap under a thread on another processor, medians: `emu` 7 µs; `virt` 19 µs (EPYC 7763) and 19 µs (EPYC 9V45). CI run 43131676181 | Several changes in one round, frames reused only after it. Not considered by an ADR | A figure on a declared machine over B8's 3 µs, and a batching design that keeps T1 (a frame is reused only after its shootdown completes) |
| CL9 | The IOMMU's tables for each device | ADR-0007 I3 | Memory: one domain's tables for each attached requester | Unmeasured, owed | None recorded: the Trade-offs call one domain per device "the strongest isolation available here" | Nothing foreseen (P0) |
| CL10 | A DMA unmap | ADR-0007 I5 | Each unmap: an invalidation, a drain, and a wait for its completion | Unmeasured, owed: "That cost has not been measured … no driver unmaps yet" (the Trade-offs) | Deferred invalidation, refused for the window it leaves open (the Trade-offs) | Nothing foreseen (P0). Measured with the first driver that unmaps |
| CL11 | A device's accesses through 4 KiB mappings | ADR-0007 I3, the Trade-offs' fourth point | More IOTLB misses than large pages give; and a device can write pages it only needs to read | Unmeasured, owed | Large pages, listed "Not yet" (the Consequences) | Large-page mappings, and read-only ones |
| CL12 | An IPI | ADR-0008 C3 | Each IPI: one table lookup from CPU index to APIC ID | Unmeasured, owed: inside budgets B2 and B8 | The APIC ID as the index, refused for compact arrays and one index type (the Trade-offs) | Nothing foreseen: hot paths read the index from GS |
| CL13 | The parking drill, every boot with an AP | ADR-0008 C7 | About 22 ms a boot, by construction: two INIT waits of 10 ms, 1 ms watching the count, and the AP's trip through the trampoline | Unmeasured, owed: the boot does not print the drill's total | None recorded | ADR-0013's question 5: which proofs a production boot runs, decided before M9 |
| CL14 | The framebuffer console | ADR-0010 E2, ADR-0010 Am1 | Each section: up to 256 pixels (1 KiB) of uncached writes, with the console lock held and interrupts off | Budget B18's trend, each boot's longest framebuffer section, median over boots: `emu` 485 µs (37 to 3,031); `virt` 199 µs (178 to 5,052) and 533 µs (181 to 3,480). CI run 43131676181 | Write-combining, which needs a PAT entry the kernel does not program yet (the Trade-offs) | Write-combining through the PAT; a figure on a physical panel |
| CL15 | The UART console | ADR-0010 Am2 | Each section: one status read and up to 16 data writes, 17 register accesses, with the console lock held and interrupts off | Budget B18's trend, each boot's longest UART section, median over boots: `emu` 172 µs (67 to 4,490); `virt` 851 µs (491 to 4,617) and 1,371 µs (333 to 5,331). In a guest each access leaves the guest | Shorter sections, refused: below 16 bytes the lock, the status read and the interrupt toggle cost about as much as the bytes (Amendment 2) | A figure on a physical UART, against B18's 50 µs |
| CL16 | The per-call pin | ADR-0011 L7; ADR-0005 B2 | Each system call: comparisons of its address against `origin.core`'s base plus the call's offset, with no table. The pin's decision is one function that is never inlined, `syscall::pin_verdict`, so that the image gate can read it, at the cost of one direct call and its return on every system call (ADR-0013, task S4-2) | Unmeasured, owed: the figure comes from TASKS item 47 (task S4-2), which times one call before the pin and after it | ADR-0005 A6's range for the whole library, which is weaker (`THREAT_MODEL_SYSCALLS.md` E8) | Nothing foreseen: it is hardening the owner accepted |
| CL17 | Widening narrow parameters in `origin.core`'s stubs | ADR-0011 L13 | Each call: one register-to-register instruction for each parameter narrower than 64 bits | Unmeasured, owed: the figure comes from TASKS item 47 (task S4-2), which times one call before the pin and after it, under QEMU and as KVM guests, as a trend | Every register parameter 64 bits wide in the C signature, which puts the widening on every caller; the kernel ignoring the upper bits, which weakens its check (L13's options) | Nothing foreseen |
| CL18 | A typed service round trip | ADR-0011 N8, ADR-0011 L11, ADR-0011 L12 | On one processor: 6 system calls, 2 switches between processes and 2 IBPBs (CL28), budget B12's parts | Unmeasured, owed: S4-6 measures it (L11). QEMU's and the KVM guests' figures leave out the IBPBs (L12) | P8's helper and its kernel fast path, waiting for N8's figures (ADR-0011 L4) | The fast path, designed with N8's figures |
| CL19 | The monotonic clock's floor | ADR-0012 C2, ADR-0012 C8 | Each read: one more access to a shared variable; every reader at once writes the same cache line | Budget B5's trend, medians, in ticks: a read alone against RDTSC alone, `emu` 100 against 23, `virt` 29 against 28 (EPYC 7763) and 26 against 23 (EPYC 9V45); every processor at once, `emu` 288, `virt` 103 and 90. CI run 43131676181 | Trusting the TSC; the floor only where the boot proof fails; commit-wait (ADR-0012's alternatives) | C8: a per-processor clock with a narrower promise, for a path that reads very often, decided then with measurements |
| CL20 | The boot proofs' waits, every boot | ADR-0013 R6, ADR-0013 R7 | Several seconds a boot: the 2 s weights proof alone, and 1 to 3.2 s for the RTC's second (ADR-0013's question 5) | Unmeasured, owed: no boot prints the proofs' total | None recorded | Question 5's answer: decide before M9 which proofs a production boot runs. Budget B16 reports the proofs' time beside boot to login |
| CL21 | A full desktop on every server | ADR-0014 S1, ADR-0014 S5 | Memory and code on every server. S5 says an idle desktop costs workloads "almost nothing", since the compositor draws only what changed | Unmeasured, owed: "a claim to measure once M8 exists" (ADR-0014) | A second, thinner server UI, refused for one UI and one SDK (the Trade-offs) | A figure once M8 exists |
| CL22 | A remote session | ADR-0014 A3 | Memory: a process for each remote session (Amendment 1's Trade-offs) | Unmeasured, owed | One endpoint process for every administrator, refused: services would trust a claim in the data (Amendment 1's alternatives) | Nothing foreseen (P0) |
| CL23 | Radio stacks and the USB4 connection manager, unprivileged | ADR-0015 D4 | Each packet or event: IPC between the controller's driver and the stack's process | Unmeasured, owed: nothing is built | The stacks in the kernel, refused for code that reads frames from strangers (the Trade-offs) | Nothing foreseen (P0) |
| CL24 | A random request | ADR-0016 R12, ADR-0016 R6, ADR-0016 R9 | Each request of up to 64 bytes: 8 SHA-512 compressions with interrupts off, and the generator's stack overwritten after it; a reseed, 8 draws and one update, every 2^20 requests or 60 s | Budget B6's trend, medians: `emu` 15.5 µs; `virt` 7.0 µs (EPYC 7763) and 3.2 µs (EPYC 9V45). CI run 43131676181. R12's budget is under 5 µs | Hash_DRBG over SHA-512, fewer hash computations, not compared further (ADR-0016's alternatives); one request's output kept and handed out 8 bytes at a time (R12) | R12's buffer, if a caller's figure shows the cost matters |
| CL25 | The generator's start-up tests on each processor | ADR-0016 R4, ADR-0016 R12 | Each processor as it starts: up to 2,048 draws, the boot processor twice that, in parallel on the application processors | The longest step with interrupts off while a processor started, median over boots: `emu` 971 µs; `virt` 213 µs (EPYC 7763) and 122 µs (EPYC 9V45). CI run 43131676181. The draws' total is not printed | None recorded | A figure on a declared machine |
| CL26 | Indirect branches in the kernel, isolated by the processor | ADR-0017 Z2 | Enhanced or automatic IBRS, set once on each processor; no instruction on entry. What the processor charges inside its predictor in that mode is not published | Unmeasured, owed: no CI guest offers either control | IBRS written on every entry, not built; retpolines, which the stable compiler cannot emit (Z10) | Nothing foreseen: it is the cheapest form both vendors offer |
| CL27 | Registers cleared on entry | ADR-0017 Z2 | Each entry: one zeroing instruction for each general-purpose register; as built, every one but RSP and RDI, on every entry | Unmeasured, owed: part of budget B10's model | None recorded: AMD recommends it for every processor (Z2) | Nothing foreseen |
| CL28 | IBPB at each change of process | ADR-0017 Z3 | One IBPB each time a processor enters ring 3 for a different process; two in each service round trip on one processor (ADR-0011 L12) | Unmeasured, owed: no CI guest offers IBPB. Each boot prints one where the processor has it (budget B9) | IBPB only for programs that ask; IBPB by trust class (ADR-0017's alternatives) | A figure on a declared processor, and trust classes from manifests and the Loader Service, as an amendment (Z3) |
| CL29 | A `SPEC_CTRL` value for each thread | ADR-0017 Z4 | Estimate: one MSR write at a switch between threads whose values differ, none while they agree. Not built: the value is written once on each processor | Unmeasured, owed | None recorded | Built only when a program can ask for SSBD or PSFD (Z4) |
| CL30 | STIBP on AMD processors that recommend it | ADR-0017 Z5 | Set once on each processor. What it costs a sibling thread's predictions is not published | Unmeasured, owed | SMT off, which halves the threads; core scheduling by process, not built (ADR-0017's alternatives) | The processor's recommendation (Fn8000_0008_EBX[17]) decides it |
| CL31 | The IDT path's LFENCE after the conditional SWAPGS | ADR-0017 Z6; `THREAT_MODEL_SYSCALLS.md` E4 | Each entry on a processor without FRED: one LFENCE | Unmeasured, owed | FRED, which removes it (Z6). Zen 5 has none | FRED on every processor in scope |
| CL32 | Shadow stacks | ADR-0017 Z8; ADR-0001 def7 | Each call and return: the processor also writes and checks the shadow stack; each switch: the shadow stack's pointer saved and loaded. Not built | Unmeasured, owed | No supervisor shadow stacks, refused: the framework is `unsafe` code (ADR-0017's alternatives) | Nothing foreseen: ADR-0009 H3 asks every declared machine for them |
| CL33 | A lookup clamped against a wrong guess | ADR-0017 Z9 | Each lookup whose index or address comes from ring 3: a compare and a conditional move, or one LFENCE | Budget B7's trend, its loop included, masked and fenced: `emu` 3.0 and 2.8 ns; `virt` 1.2 and 7.9 ns (EPYC 7763), 0.5 and 6.1 ns (EPYC 9V45). CI run 43131676181 | LFENCE after every check, refused: a barrier on every handle lookup; masking everywhere, refused: Intel promises it for family 6 only (ADR-0017's alternatives) | Intel's masking statement extended to family 18 and above: a table row, and the mask there |
| CL34 | The system-call dispatcher | ADR-0017 Z10 | Each system call: comparisons over the call numbers in place of one indirect jump through a table | Unmeasured, owed | A table indexed through Z9's clamp, which Z9 allows if one replaces the `match` | A figure showing the clamped table cheaper |
| CL35 | Frame pointers and overflow checks in the kernel's release build | CLAUDE.md §14; ADR-0017 Z10; ADR-0001 def8 | Each function: the frame pointer saved and set, and one register fewer; each checked operation: a test and a branch | Unmeasured, owed | None: §14 and decision item 123 ask for both | The directive's text (§14), which is the owner's |
| CL36 | Checking a user pointer | CLAUDE.md §30; ADR-0017 Z9; `THREAT_MODEL_ADDRESS_SPACES.md` T5 | Each copy in or out: a software walk of the space's tables, up to four levels, for each page the range touches, each page clamped (Z9), before the copy | Unmeasured, owed | Not considered by an ADR. The copy already runs through the space's own tables and recovers from a fault at its instruction (T5), so whether the walk before it is avoidable is open | A recorded argument that the copy's fault recovery and SMAP enforce what the walk checks, or a figure showing the walk does not matter |
| CL37 | Making a process, and its account's end | ADR-0005 G5, ADR-0005 G6 | Each creation: the figure's arithmetic (about thirty checked multiplications and additions) and one compare-and-swap on the root's one shared word; for a program's launch, one more give-back once its start record is written; for each page a proof process's launch maps, one more compare-and-swap; at the account's end one atomic add. Nothing on any system call, but one comparison on `memory_map`'s path (a run's records against the space's most pages, the fix to S3-5b's `prepare_run`). Memory: every process holds its worst case, a system call's temporaries in each thread's part (about 68 KiB a thread), not its use, and a process that may map is counted at its space's most page records, about 2 MiB over what most use, so the root admits fewer processes than memory would hold | Unmeasured, owed: no boot times a creation apart. The memory side, `emu`: the figures the boot prints for the first program (225,408 bytes of figure and 72,064 for its 17 pages, against a root of 437 MiB) | A ceiling for each kind across the kernel, checked on every charge; no kernel-wide bound (G5's options b and c, refused) | Charging the space's record tables per mapping as they grow, for the 2 MiB; a figure for creation on a declared machine |
| CL38 | Binding to a port | ADR-0005 I1 | Each bind: one more acquisition of the port's lock, with interrupts off, to compare the port's bindings armed and reserved with its creator's `bindings_per_port` before the binding's nodes are allocated, and the same comparison again where `reserved` is raised; at each growth of a port's index, at most about log2 of its bound times in the port's life, one more acquisition of the lock, to clear the growing mark, and a wake of the port's waiters, and a bind that needs room while another grows the index waits for that growth (the independent review of TASKS 57, T1 and T6); and a larger figure for each process given the kind, which lowers how many processes the root admits | Unmeasured, owed: TASKS item 57 builds it | One kernel constant for every port, refused by the owner (item 282, against G6) | Nothing foreseen: it is the bound the owner accepted |
| CL39 | Identity maps for firmware-reserved memory | ADR-0007 R1, ADR-0007 R2 | At a device's attach: one identity map for each page of each reserved range that names it; at each IOVA allocation in a domain that has identity maps: the skipped pages. A DMA through an identity map is translated as any other | Unmeasured, owed: TASKS item 40 builds it | Maps made at bring-up for every device; a separate window for identity maps (ADR-0007 Amendment 1's alternatives) | Nothing foreseen: questions 4 and 8 required the maps |
| CL40 | Any processor count | ADR-0008 D1, ADR-0008 D2, ADR-0008 D3, ADR-0008 D7, ADR-0008 D8 | Each per-processor lookup: a compare for index 0 and two loads of the table's pointer and length; a lookup that finds nothing also reads `cpu::count()`, to stop the kernel if a table was missed (review finding F2), and one that finds its element pays nothing more; each shootdown: a scan of the indexed processors instead of 64; each reserved index: a few KiB of tables, printed at boot; the stacks' region: one page table per 2 MiB, made at discovery; each AP's start: 10.2 ms of fixed waits | Unmeasured, owed: TASKS item 58 builds it and measures the lookup as a pair on the boot's timed paths | One block per processor holding all its state; static arrays of a larger fixed size; parallel start-up, a task of its own (ADR-0008 Amendment 1's alternatives) | A lookup kept to one load; parallel start-up |

### Each ADR, with its rows or "none"

ADR-0018 V8 asks the ledger to list ADR-0001 to ADR-0017, each with its rows or with "none" and the reason. ADR-0001 to ADR-0017 themselves are not edited.

| ADR | Rows | Why none |
|---|---|---|
| ADR-0001 | CL1, CL2, CL32, CL35 | |
| ADR-0002 | CL3 | |
| ADR-0003 | CL4, CL5 | |
| ADR-0004 | none | Every cost it states is none on any path: the heap stays in the direct map, so a free changes no mapping (A1); a guard page costs no frame (A2); the stack region's slide is a base set once, with at most one more page table at boot (Amendment 2) |
| ADR-0005 | CL6, CL7, CL8, CL16, CL37, CL38 | |
| ADR-0006 | none | OriginFS v0 is read-only and is to be replaced. Its Trade-offs name limits of the format (files up to 2 MiB, no snapshots, a block that can tear once writing comes), not a cost on an operation |
| ADR-0007 | CL9, CL10, CL11, CL39 | |
| ADR-0008 | CL12, CL13, CL40 | |
| ADR-0009 | none | It chooses the machines and the scope. What it adds to the kernel is a table read once at boot (A2), and its stated cost is the table's upkeep, not time on any path |
| ADR-0010 | CL14, CL15 | |
| ADR-0011 | CL6, CL16, CL17, CL18 | |
| ADR-0012 | CL19 | |
| ADR-0013 | CL20 | |
| ADR-0014 | CL21, CL22 | |
| ADR-0015 | CL23 | |
| ADR-0016 | CL24, CL25 | |
| ADR-0017 | CL26, CL27, CL28, CL29, CL30, CL31, CL32, CL33, CL34, CL35, CL36 | |
| The threat models | CL5, CL8, CL31, CL36 | |

What the first fill found, in short:

- **Most of the ledger is owed.** Of 36 rows, 7 have a trend from QEMU or a KVM guest and none a figure from a declared machine. The largest cost named anywhere, IBPB at each change of process (CL28), has no figure at all.
- **Two costs may be avoidable,** and no ADR has looked: IRETQ on every return from a system call (CL5), and the walk of the tables before each copy from a user pointer (CL36). Each is open until someone writes the argument, or measures it.
- **Two rows are paid at every boot by proofs written for development** (CL13, CL20). ADR-0013's question 5 already decides that before M9.

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
