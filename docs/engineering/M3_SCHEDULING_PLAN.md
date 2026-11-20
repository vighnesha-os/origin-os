# Build plan: M3 scheduling across processors (ADR-0020)

| | |
|---|---|
| Written | 2026-11-20, by TASKS item 90; revised the same day at the coordinating session's rulings (the batch split, the first task's acceptance, the move gate, the diagnostic rounds, lending across calls in ADR-0005) |
| For | `docs/adr/ADR-0020-scheduling.md` and ADR-0005 Amendment 11, both accepted by the owner on 2026-11-20 ("Accept all 24", decision items 318 to 341). **No task starts before those records are on `main`** (decision item 106) |
| Numbering | TASKS items 92 to 109, numbered by the coordinating session on 2026-11-20 in the order of the plan's first draft, whose tasks were lettered P1 to P18: P*n* is TASKS item 91 + *n*. Each heading keeps its letter, which the hand-over and the decision sheet use |
| Sizes | In commits of the size the batches have used: small, real, no padding. **Batch 9:** items 92 to 98, 100 and 101, about 94. **Batch 10:** items 99 and 102 to 108, about 68. **After the M3 core:** item 109, about 9 |
| Rules for every task | CLAUDE.md §83's workflow; a §78 contract from `TASK_CONTRACT_TEMPLATE.md`; Loom models built and controlled by `cargo xtask loom`; boot proofs under `cargo xtask boot-matrix`, run inside `cargo xtask exclusive`; every new proof's ADR-0013 class and controls recorded in ADR-0013's table; `SOURCE_PROVENANCE.md` updated; no operating system's source read (§3) |
| The large-machine round (ADR-0020 S23) | **Every task that turns balancing on or changes what a proof accepts** (items 98, 99, 100, 102, 104, 107, 108 and 109) first boots the 65- and 67-processor software-processor machines with balancing, or its own change, on and off, each with the machine idle and beside load, and sends the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) to the coordinating session. None of that task's proofs, bounds, verdicts or "reported" rows is set or changed before the figures are read: two rulings made without such figures failed correct kernels on those machines on 2026-11-19. It is in each such task's acceptance below, and in its size |

## The order

```text
batch 9
92 thread records ──► 93 index witness + pins ──┐
94 FPU format check ────────────────────────────┤
95 moves between tables (origin-sync) ──────────┼──► 96 the move + proofs ──► 98 idle pull + kick
97 domains, equal capacity, new machines ───────┘                        └──► 100 affinity
                                                                          (97 feeds 98 and 100) ──► 101 records
batch 10
98 ──► 99 periodic pull
92 ──► 102 classes ──► 104 quotas
92 ──► 103 accounting
92 ──► 105 weight lending (owned waits)
97 ──► 106 hardware capacity and heat ──► 107 power mechanisms
96 to 105 passing ──► 108 EEVDF evaluation
after the M3 core
105 + ADR-0011 L4's fast-path design ──► 109 lending across calls (ADR-0005 Amendment 11)
not in these batches
96 ──► ADR-0008 Amendment 2 (processors offline and back); M3-1 stays partial until its task is built
```

**May run at the same time:** items 92, 94, 95 and 97 (different files; item 95 puts its Loom models in a module of their own, so it does not edit the part of `models.rs` item 92 changes). Then item 93 alone (it edits 15 files). In batch 10, items 102, 103 and 105 each touch `runqueue.rs` or `sched.rs` and go one at a time. **Two writers never touch the same files.**

## Batch 9

### TASKS item 92 (P1). Thread records and names that survive moves (ADR-0020 S1)

- **Files:** `framekernel/src/sched.rs`, `runqueue.rs` (slots map to records), `wait.rs`, `process.rs` (spawn paths), `stacks.rs`; a new `framekernel/src/threads.rs` for the table; `framekernel/frame/sync/src/models.rs` (`OnCpu::wake`'s copy of `sched::wake` gains the location loop).
- **Invariants:** a record's fields change only under its owning queue's lock, the location too; a stale name never wakes a later thread; `follow_thread`'s "a ready slot has its stack" (`UNREADY_SWITCHES` stays 0); the exit mark moves into the record unchanged (N5).
- **Tests:**
  - host tests of the table: reuse with a new generation, a stale name refused;
  - **Loom:** a wake by identity finds the thread wherever its record's location says it is, with a stand-in mover that changes the location between a thread's registration and its block; **negative control: a wake sent to the stale slot, as today's `ThreadRef` would, which the model must catch** as a lost wake-up;
  - every existing boot proof, unchanged.
- **Accepts when** (the coordinating session's ruling): **no change of behaviour.** No thread moves; `boot-matrix` passes with the same scenarios and the same markers, three runs in a row; the Loom model and its control pass and fail as above; B1's trend on the development Mac within its spread before and after (paired boots, ADR-0018 V7).
- **Size:** 14.

### TASKS item 93 (P2). The processor-index witness and the pin (S2, S3)

- **Files:** `framekernel/frame/src/cpu.rs` (the witness type; `this_cpu` takes it), the 74 call sites in 15 files (`sched.rs`, `cpu.rs`, `tlb.rs`, `timer.rs`, `storage.rs`, `smp.rs`, `random.rs`, `console.rs`, `virtionet.rs`, `kernel.rs`, `irq.rs`, `wait.rs`, `time.rs`, `stacks.rs`, `pcie.rs`); `sched::pin()` and the record's pin count; `end_with` and `with_measured_stack` pin.
- **Invariants:** no processor index is used outside the section that read it (N3); an ending thread never moves (N5); a thread on a measured stack never moves.
- **Tests:** the build is the check; host tests of the pin count; a control: one site converted back to the old call fails to build. Every boot proof unchanged.
- **Accepts when:** every site converted, the old signature gone, each conversion listed in the review with the section it now reads under; no change of behaviour, as item 92.
- **Size:** 10.

### TASKS item 94 (P3). Every processor's FPU format checked (S6)

- **Files:** `framekernel/frame/src/cpu.rs` (`enable_simd` returns XCR0 and the enabled size; the global becomes the boot processor's), `smp.rs` (each AP compares and records "takes part in moves"), the boot line.
- **Invariants:** an FPU area is restored only under the XCR0 and size it was saved under (N9).
- **Tests:** host tests of the comparison; a boot control (a planted different XCR0 on one AP) shows that processor excluded and named, and the boot otherwise unchanged.
- **Accepts when:** every boot-matrix machine reports every processor taking part; the control is caught.
- **Size:** 3.

### TASKS item 95 (P4). Moves between tables of contexts (S4, S5)

- **Files:** `framekernel/frame/sync/src/contexts.rs` (`lend`, `adopt`, `restore`, `forget`; the leaving record and its publication; the begun/finished counters; the move gate; a scan with four fast tries and one under the gate); a new Loom module beside `models.rs`; `framekernel/frame/src/cpu.rs` and `stack.rs` (scans through it).
- **Invariants:** a frame is taken by one processor at most; a home is never in no table while its context may run (N4 a); no frame is resumable while its processor is on its stack (N4 b); a scan always ends, after at most five passes and one wait for the gate; a table holds at most its queue's contexts, one ended, and spawns in flight (N13).
- **Tests:** host tests of each operation, including every refusal; **Loom MV1 to MV7 and LV1** with their controls (ADR-0020 S4, S5), MV7 being a scan racing back-to-back moves of the same context; the existing contexts models and their controls unchanged; every boot proof unchanged (nothing moves yet, but every switch now publishes late).
- **Accepts when:** `cargo xtask loom` passes with every new control reported; B1's trend within its spread; no change of behaviour.
- **Size:** 16.

### TASKS item 96 (P5). The move, its proofs and pinned mode (S6, S20, S23)

- **Files:** `sched.rs` (`migrate`: both queue locks in index order, the move gate, the eligibility checks, lend, adopt or restore, depart, arrive, location, forget; pinned mode's flag and its entries, failing the boot while the proofs run and writing a fault record afterwards); `runqueue.rs` (`depart`, `arrive`, lag); a new proof module `framekernel/src/migration_proof.rs`; ADR-0013's rows; `developer-tools/xtask` markers.
- **Invariants:** every row of ADR-0020's invariants table. A woken thread is queued where it blocked (S6).
- **Tests:**
  - host tests of `depart` and `arrive` (lag kept, never credit), with a randomized test against a brute-force model;
  - boot proof **thread migration: integrity** (Refuse) and **thread migration: delivery** (Withdraw: pinned mode, a failed boot under `cargo xtask`), on every boot-matrix machine with two or more processors; only the proofs' own threads are movable, so no existing proof's scheduling changes;
  - controls, each a scripted edit booted once: `lend` by a plain store (integrity refuses the boot); `wake_pending` not carried (delivery fails, naming the trigger, and the boot goes on pinned); the FPU area not carried (integrity's SIMD canaries); the location published before `arrive` (`UNREADY_SWITCHES`); a run-time inconsistency planted after the proofs (a fault line, pinned, no failure); publication at the switch, as today (recorded if emulation does not show it: LV1's control is then the evidence).
- **Accepts when:** both proofs pass on every machine, three runs of `boot-matrix` in a row; each control caught as recorded; the settles' compares unchanged.
- **Size:** 14.

### TASKS item 97 (P6). Domains, equal capacity and the new machines (S8, S9's source 5 and the host-tested sources, S23)

- **Files:** `framekernel/frame/src/topology.rs` (leaf 04H and Fn8000_001D sharing, read on each processor), a new `framekernel/frame/src/domains.rs` (the tree), `framekernel/frame/src/acpi.rs` (PPTT, parsed as untrusted input), `framekernel/src/numa.rs` (node order), capacity: every processor equal in boot proofs; the HFI, `_CPC`, register and Fn8000_0026 sources as pure functions over synthetic inputs (the drivers are item 106's); `developer-tools/xtask` (the SMT machine `-smp 8,sockets=1,cores=4,threads=2` and the two-package machine `-smp 8,sockets=2,cores=4,threads=1`, each with two NUMA nodes, and their expected tree lines).
- **Invariants:** a disagreement about sharing a cache is read as "not shared" and reported; no capacity from a core-type code (S27-10).
- **Tests:** host tests for every encoding (Intel's leaf 04H, AMD's 8000001D, PPTT with and without cache IDs, malformed tables) and for capacity from synthetic HFI tables, `_CPC` packages and Fn8000_0026 reports (ADR-0020 S9, "What is shown where"); Report proof **scheduling domains** on every machine; controls: a planted wrong sharing shift, a planted PPTT that disagrees.
- **Accepts when:** the tree matches on every machine, the new machines included; each new machine's boot time measured on the development Mac and recorded with its share of CI's real-processor job (ADR-0008 Amendment 1 D7).
- **Size:** 12.

### TASKS item 98 (P7). Idle pull and the idle kick (S7.1, S7.2, S10)

- **Files:** `sched.rs` (`run_ap`'s idle loop; make-ready paths), per-domain idle masks and per-processor load words (`PerCpu`, sized at discovery), `timer.rs` (the kick IPI's rate).
- **Invariants:** a move only by pull; queue locks in index order, then the gate; no shootdown while a queue lock is held; at most one kick per tick per processor; cache-hot threads only to a processor sharing the last-level cache.
- **Tests:** **the diagnostic round first** (pulls and kicks on and off, the 65- and 67-processor machines, figures to the coordinating session); host tests of victim choice; Report proof **load balancing** (B24) with the ADR-0013 Amendment 2 allowance on the 65-, 67- and 288-processor machines; B21's figure printed as a trend; controls: pulls disabled (the balance proof fails), kicks disabled (B21's trend rises).
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with pulls and kicks on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then the proof passes on the machines of 2 to 8 processors, three runs in a row.
- **Size:** 12 (2 of them the diagnostic round).

### TASKS item 100 (P9). Affinity, and user threads made movable (S11, S2)

- **Files:** the record's affinity (pinned, domain, interned set, any), directed moves for affinity changes, the balancer's checks; `process.rs` (user threads spawned movable once item 93 has converted the system-call paths). No SDK surface (ADR-0011 decides it later).
- **Invariants:** no thread runs outside its hard set.
- **Tests:** **the diagnostic round first** (user threads movable on and off, since the user-isolation and IPC proofs' threads then move); host tests of interned sets; Refuse proof **affinity** (a count of switch-ins outside the hard set stays 0) with a control (the balancer's check removed); the user-isolation proof unchanged with movable user threads.
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with user threads movable on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; both proofs pass on every machine; the review lists every system-call path item 93 converted.
- **Size:** 9 (2 of them the diagnostic round).

### TASKS item 101 (P10). Records for batch 9

- **Files:** `docs/requirements/` (ADR-0020's rows A20-S1 to A20-S23, which the records of items 318 to 341 added as not started, and S26-07, S26-08, S26-15, S27-01 to S27-06, S27-10, S24-11, S25-09, each updated to what is built), `docs/engineering/PERFORMANCE_BUDGETS.md` (B20, B21 and B24's trends, and ledger rows CL47 to CL50, CL52 and CL53 brought up to what was measured), ADR-0013's table, `docs/ROADMAP.md` (M3's state; M3-1 still partial), `SOURCE_PROVENANCE.md`.
- **Accepts when:** `cargo xtask check` passes, its gates included.
- **Size:** 4.

**Batch 9 total:** item 92, 14; 93, 10; 94, 3; 95, 16; 96, 14; 97, 12; 98, 12; 100, 9; 101, 4: **94**, with room for review answers in the batch's usual margin.

## Batch 10

### TASKS item 99 (P8). The periodic pull and the NUMA rule (S7.3, S10)

- **Files:** `sched.rs` (`on_tick` before the queue's step), the domains' periods and thresholds.
- **Invariants:** B22: never more than 5 µs in one tick, within B18 with interrupts off.
- **Tests:** the diagnostic round first; a case of item 98's proof with every processor busy and uneven (3 threads on 2 processors), on the two-node machines; B22 printed as a trend; control: the periodic pull disabled.
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with the periodic pull (and S10's rates and periods) on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then shares within 25 % of the mean in the busy, uneven case, and no tick over B22's bound.
- **Size:** 8 (2 diagnostic).

### TASKS item 102 (P11). Classes and levels (S12, S13)

- **Files:** `runqueue.rs` (class bands, the wake-up rule per class), `sched.rs` (class and level in the record; placement preference), `process.rs` (the ceiling, kernel-set until ADR-0005's start-record amendment). No SDK surface (ADR-0011).
- **Tests:** the diagnostic round first; host tests of the band table and the wake-up rule; Report proof **scheduling classes** (B23; a Background thread's minimum share); a Refuse case: a thread asking above its ceiling is refused; controls: Interactive's preemption removed (B23 fails), the ceiling's check removed (the Refuse case fails).
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with classes on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then the classes proof and its Refuse case pass on every machine.
- **Size:** 10 (2 diagnostic).

### TASKS item 103 (P12). CPU accounting (S14)

- **Files:** `sched.rs` (`with_queue`'s switch: one RDTSC, the record's counters, the process's sums), `runqueue.rs` (ready-wait start).
- **Tests:** host tests of the counters across a move (two parts, never one subtraction across processors); Report proof **CPU accounting**; B25 printed as a trend; control: a switch that forgets to charge.
- **Size:** 5.

### TASKS item 104 (P13). CPU quotas (S15)

- **Files:** `quota.rs` (share and cap beside the count quotas), `runqueue.rs` (effective weight at each charge), `sched.rs` (throttling at the tick).
- **Tests:** the diagnostic round first; host tests of effective weights and the budget's slices; Report proof **CPU quotas** (B26; 8 threads against 1); controls: share not divided (the 8-thread process takes 8 times as much), cap not enforced.
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with shares and caps on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then the quotas proof passes on every machine.
- **Size:** 10 (2 diagnostic).

### TASKS item 105 (P14). Weight lending for owned waits (S16)

- **Files:** `framekernel/frame/sync/src/wait.rs` (the owned form), its Loom models, `sched.rs` (lending and returning under the owning lock), a kernel sleeping lock as its first user.
- **Tests:** Loom (lending and returning leave every weight as it was; a depth-8 chain stops there), with controls; Report proof **weight lending** with its Refuse part.
- **Size:** 8.

### TASKS item 106 (P15). Hardware capacity and heat (S9's sources 1 to 3, S17's heat)

- **Files:** a new `framekernel/frame/src/hfi.rs` (the table per package, its MSRs, the thermal interrupt and acknowledgement), `_CPC` through `origin-aml` and Notify 0x85 (ADR-0019), HWP's and AMD CPPC's capability registers where readable, thermal status.
- **Tests:** host tests against table images (a change, an "avoid" row, force idle, efficiency 255) and `_CPC` packages of revisions 2 to 4; under QEMU only the absence paths. **Hybrid placement is shown only on ADR-0009 H4's machines** (M3-6, §74), with records in `docs/hardware/records/`.
- **Size:** 10.

### TASKS item 107 (P16). Power mechanisms (S17)

- **Files:** placement's pack and spread, avoid, consolidate; evacuation without offlining (`_PUR`'s mechanism, not its policy).
- **Tests:** the diagnostic round first; host tests of placement in each mode; a boot case that packs on request and spreads again.
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with pack and spread on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then the placement case passes.
- **Size:** 7 (2 diagnostic).

### TASKS item 108 (P17). EEVDF evaluation (S19)

- **Files:** a second run queue beside `runqueue.rs`, host-tested; a scripted edit for paired kernels; a ledger of the figures.
- **Tests:** property tests of EEVDF's Corollary 2 and Lemma 2 in steady conditions; the diagnostic round with each queue; paired boots on one machine (B23, the fair share's error, B1).
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with each queue on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; the paired figures are recorded; adoption is the owner's, by amendment.
- **Size:** 10 (2 diagnostic).

**Batch 10 total:** item 99, 8; 102, 10; 103, 5; 104, 10; 105, 8; 106, 10; 107, 7; 108, 10: **68**.

## After the M3 core

### TASKS item 109 (P18). Lending across calls (ADR-0005 Amendment 11, J1 to J6)

- **Depends on:** item 105 (lending); the design of ADR-0011 L4's fast path, or the `CALL` flag alone if L4 is not yet designed.
- **Files:** `framekernel/src/ipc.rs` and `channel.rs` (the flag, the writer recorded, the loan at read, its end), the generated client (OIDL) for two-way methods, `user-abi`.
- **Tests:** Loom (no loan outlives the wait; every weight returns), with controls; Report proof (an Interactive client's call to a Background service among Normal busy threads completes within twice its unloaded time), with a control removing the loan; the diagnostic round first.
- **Accepts when:** **the large-machine round:** the 65- and 67-processor software-processor machines booted with the loan on and off, each idle and beside load, and the figures for the existing timing proofs (the fair share, the preemption window, the timed waits, the settles) sent to the coordinating session and read **before** any of this task's proofs, bounds, verdicts or "reported" rows are set; then its Loom models and proof pass with their controls.
- **Size:** 9.

## Not in these batches

- **Processors offline and back** (ADR-0020 S21): ADR-0008 Amendment 2 is written after item 96 lands; its task follows. **Exit test M3-1 stays partial until it is built.**
- **The SDK's interfaces** for affinity, classes, accounting and quotas (ADR-0011), and **the start record's new limits** (ADR-0005): decisions of their own.
- **M3-6:** every proof above on ADR-0009 H4's machines, one with SMT and one hybrid, with records.
