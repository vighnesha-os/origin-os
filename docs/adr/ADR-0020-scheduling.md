# ADR-0020: Scheduling across processors: moving threads, balancing, placement, classes, accounting and quotas (M3)

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-20 ("Accept all 24"), every decision as proposed: decision-sheet items 318 to 340, its questions 1 to 23 (CLAUDE.md §18, §84). Written by TASKS item 90; the coordinating session ruled on its outline the same day before it was put: the identity refactor first, with no change of behaviour (S1); processors offline and back in an ADR-0008 amendment, with M3-1 partial until it is built (S21); lending across calls in ADR-0005, as its Amendment 11, accepted beside this ADR as item 341 (S16); and additions in S4, S6, S9, S11, S12, S20 and S23. **Nothing in it is built.** No person has reviewed it. Every figure in it is a hypothesis (§53, decision item 115, ADR-0018 V6). Amendment 1, accepted by the owner on 2026-11-23 (decision sheet item 355), lets an idle processor ask a busy one, by one reschedule IPI, to publish a waiting thread's frame |
| Decides | How a thread moves between processors without breaking what the kernel's proofs rest on today; who moves threads, when and where (load balancing and placement by topology, capacity, migration cost, power and heat); affinity; scheduling classes and priorities; CPU accounting; CPU quotas; priority-inversion mitigation; EEVDF's place; the safe fallback; where CPU offline and online belongs |
| Requires | CLAUDE.md P0, P1, P2, P8; §1 (roles 3, 4, 5, 6, 16, 23, 33, 35, 53; "Specify operation semantics", "Discover and report capabilities truthfully", "Deny by default", "Establish performance budgets before measurement"); §2, §16, §18, §24 to §27, §53, §58, §61, §66, §67, §73, §74, §83; final rules 6 to 9, 16, 21, 29, 30 |
| Performance cost | None, by construction: S2, S3, S6, S8, S13 and S18 to S21. Unmeasured, owed, each with its model below and its row in the cost ledger (ADR-0018 V5): S1 (CL47), S4 (CL48), S5 (CL49), S7 (CL50), S9 (CL51), S10 (CL52), S11 (CL53), S12 (CL54), S14 (CL55), S15 (CL56), S16 (CL57) and S17 (CL58); and S23, under the existing row CL20 (the boot proofs' waits). S22 is the budgets themselves, B20 to B26. Amendment 1: unmeasured, owed (CL62) |
| Code | None changed. The build plan (`docs/engineering/M3_SCHEDULING_PLAN.md`) splits the work into TASKS items 92 to 109 |
| Evidence | `docs/research/2026-11-20-m3-scheduling.md` (the ledger: sources I1 to I4, A2, A3, the PPRs, ACPI66, P1 to P4, and §6's table N1 to N13 of what the code rests on), read 2026-11-20; the kernel on `land/batch-8` at `fa88211a`, whose TASKS items 70, 73, 75, 77 and 79 (the table of contexts, the reclaim, the exits counts) were not yet on `main` when this ADR was accepted |
| Builds on | ADR-0003 K6 and K7; ADR-0005 P8 and P11; ADR-0008 and its Amendment 1; ADR-0009 Amendment 1; ADR-0011 N6; ADR-0012; ADR-0013 and its Amendment 2; ADR-0017 Z3 and Z5; ADR-0018; ADR-0019 (`_CPC` needs the AML interpreter) |
| Amends | ADR-0003 K7 ("Threads are never migrated"); the closing note of `origin_sync::contexts` ("A context never moves between processors") |

## Context

**What exists.** One run queue per processor, holding at most 16 contexts (`framekernel/src/runqueue.rs`): stride scheduling (Waldspurger and Weihl, 1995) with a weight per thread, the idle flow in slot 0, wake preemption for woken threads, and a 100 Hz tick (ADR-0003 K5). Threads are created on a processor their spawner names and **never leave it** (ADR-0003 K7). The kernel discovers each processor's topology (`origin_frame::topology`: thread, core, module, die and package fields from CPUID leaves 1Fh, 0Bh and 80000026h, and the raw hybrid core-type code) and the NUMA map (`framekernel::numa`, from SRAT and SLIT), and uses neither for scheduling. There are no classes, no accounting, no CPU quotas, no affinity, no load balancing and no inheritance.

**What M3 asks.** `docs/ROADMAP.md`'s exit tests M3-1 to M3-4 (decision item 259) name the matrix rows this ADR covers: S26-07 load balancing, S26-08 affinity, S26-09 CPU accounting, S26-10 interactive and S26-11 background scheduling, S26-12 priority-inversion mitigation, S26-13 resource quotas, S26-14 the EEVDF evaluation after simpler policies pass their baselines, S26-15 a safe fallback; all of §27 (S27-01 to S27-10, placement by physical cores, SMT siblings, shared caches, hybrid capacity, NUMA, migration cost, power, heat and the foreground, and the rule not to assume every logical processor performs alike); S24-11 and S24-13 (SMT and hybrid capabilities, partial today), S25-08 (priorities, partial), S25-09 (affinity) and S24-14 (CPU online and offline).

**The constraint that shapes all of it.** Load balancing means moving threads, and a good part of what the eighth batch proved rests on threads never moving. The ledger's §6 lists thirteen places (N1 to N13). Four of them break outright if a thread is simply moved:

- **N1, wake-ups name a place.** A thread is named by (processor, run-queue slot, generation), and wait queues and ports hold that name. A thread that registers with a wait queue, is preempted before it blocks and is then moved is woken at its old slot; the wake-up is dropped, and the thread blocks for ever.
- **N3, the processor index read with interrupts on.** 74 calls of `this_cpu()`, several in thread context with interrupts on, then used after a point where the thread could be preempted (`end_with`, `current_thread`, `current_user`, `reap`, the storage proof's per-processor threads). After a move each acts on another processor's state.
- **N4 (a), a scanner can miss a moving stack.** Whether a stack may be given back is decided by scanning every processor's table of contexts in index order. `origin_sync::contexts` says a move must claim the stack's home in the new table before retiring it in the old; but a scan that reads the new table before the claim and the old one after the retirement finds the home in neither. Claim-before-retire alone does not close it.
- **N4 (b), a frame is published while its processor still stands on the stack.** `Contexts::switch` stores the outgoing context's frame and returns along the outgoing stack; the trap stub loads the incoming stack pointer only afterwards. Today only the same processor can take that frame. Once another processor may, it could resume the thread while the first still runs its return path on the same stack.

**What can be shown where.** Every move's correctness can be shown under QEMU's software processor and as KVM guests in CI, on machines of 2 to 288 processors. SMT cannot today: no boot-matrix machine has two threads per core. Hybrid parts, Intel's Hardware Feedback Interface, CPPC's differing highest performance and thermal signals cannot be shown under QEMU at all; they are host-tested against table images, and qualified only on ADR-0009 H4's machines (§74). Nothing here claims more (ADR-0018 V9).

## Words used here

- **Move** (migration): a thread's context, run-queue entry and record pass from one processor (the **source**) to another (the **destination**).
- **Movable / pinned:** whether a thread may be moved (S2).
- **Ready / suspended:** a thread that is runnable but not running, whose frame is saved in its processor's table of contexts.
- **Domain:** a set of processors that share something: a core (SMT), a cache, a package, a NUMA node (S8).
- **Capacity:** how much work a processor does per unit of time, relative to the most capable processor of the machine, from 1 to 1024 (S9).
- **Load:** the sum of a processor's runnable threads' weights, scaled by its capacity (S9).
- **Class:** Interactive, Normal or Background (S12).

## Decisions

### Part A. Moving a thread without breaking the proofs

### S1. A thread keeps one identity across moves: a record found through its location

**Decision.** Every thread has one record from its creation to its reaping, in a kernel-wide table, and `ThreadRef` names the record (its number in the table and a generation), not a processor and slot. The record holds what the per-slot arrays hold today (the stack, the FPU area, the user context, the exit mark) and the new per-thread state (pin count, affinity, class and level, accounting, last processor and time it ran, moves) and a **location**: the processor and queue slot that hold the thread.

- **Ownership.** Every field but the location is read and written only under the lock of the run queue the location names, the *owning queue*. The location changes only under the owning queue's lock and, for a move, the destination's lock too. It is read without a lock (Acquire) to know which lock to take.
- **Wake-up.** `wake(t)` reads the location, takes that queue's lock, and reads the location again. If it is unchanged, the thread cannot move while the lock is held, and the wake-up proceeds as today (`RunQueue::wake`, with its pending wake-up for a thread not yet blocked). If it changed, a move completed in between: the lock is released and the wake-up follows the new location. Each retry follows a completed move, and S10 limits how often one thread moves, so a wake-up retries a bounded number of times; after 64 it reports and enters S20's fallback.
- **Stale names.** The generation is compared under the owning lock; a reaped thread's number is reused only with a new generation, so an old name never wakes a later thread (as the slot's generation does today).
- **Storage.** The table grows in chunks published once and never moved or freed, as `PerCpu` does (ADR-0008 Amendment 1, D1); a record is reused, not freed. Its size is part of what a thread costs, so ADR-0005 G5's per-thread figure grows by it, as the kernel's build measures it.

**Evidence.** The ledger §6, N1 and N2; `origin_sync::wait`'s promise that no wake-up is lost; `wait::Scheduler::Thread` is the one type every holder of a thread's name uses (the wait queues and the ports' delivery), so the change has one seam.

**Alternatives.** (a) Pin a thread while it is registered with a wait queue or port: no refactor, but every future holder of a thread's name (an owner for inheritance, an affinity change made from another thread, a debugger) must remember to pin, and the rule fails silently when one does not. (b) Leave a forwarding record in the old slot: it must outlive every name that may still point at it, with no bound. (c) Reference-counted thread objects in the queues: reference counting on every wait, and names that are not `Copy`, which the wait-queue protocol and its Loom models assume.

**Trade-offs.** A refactor of `sched.rs`, `wait.rs` and the spawn paths before any balancing exists. In return every later feature names a thread one way. It is the first build task, and it lands with **no change of behaviour**: no thread moves, and `boot-matrix` passes with the same scenarios and markers (the coordinating session's ruling of 2026-11-20).

**Cost.** Unmeasured, owed: a wake-up adds one Acquire load and one compare under the lock it already takes; a switch reads the incoming thread's fields from its record instead of the per-slot arrays (an indirection; B1's trend shows it).

**Migration consequences.** No ABI: `ThreadRef` never crosses the system-call boundary. The Loom models' copy of `sched::wake` (`OnCpu::wake` in `models.rs`) gains the location loop. ADR-0005's thread object (`ThreadObject`) can later name the record for SDK operations on a thread.

**Note, 2026-11-20: the loop is real code in the models (TASKS item 92; the coordinating session's ruling Q1; no decision changes).**
- **One loop.** The location loop is `origin_sync::threads::wake`. The kernel instantiates it over its run queues (`sched::wake`) and the Loom models over theirs, so the models run the loop itself, not a copy; `OnCpu::wake` only calls it. The record and its block are the same code in both.
- **What a changed location says.** Under the lock the loop took, a location found changed is one of three things:
  - clear: the thread has exited, and there is nothing to wake;
  - set, with another generation: the record holds a new thread, and the name is stale;
  - set, with the same generation: the thread moved. Only this is followed, and only this counts towards the 64.
- **Why the three are told apart.** Without that, a wake-up racing its thread's exit, or a reuse of its record, would count as a move although nothing moved.
- **The count.** No thread moves until S6 is built. The kernel counts the moves wake-ups follow, and its boot proofs require that count to be 0 (the IPC proof's condition and every settle's verdict, with no line of their own).

**Note, 2026-11-20: how S1's storage is built, and where a record's state is kept (TASKS item 92; the coordinating session's rulings Q2 and Q3).** The storage is built as the decision says. Where the state is kept refines the Ownership bullet above, as ruling Q3 accepted (the review of TASKS item 92, R92-10).
- **The table.** One block of 32 records per processor, in the per-processor table that also holds its zombies (`framekernel::threads::THREADS`). Each block is a chunk published once, when the processors are discovered (`percpu::size_per_cpu`; ADR-0008 Amendment 1, D1), and never moved or freed.
- **Taking and giving back.** A spawn takes its record from the block of the processor it queues the thread on. The thread's reap gives it back there, as it takes the zombie's state out.
- **Why 32 is enough.** While no thread moves, a processor's records belong to the threads of its run queue (at most 15) and its zombies awaiting their reap (at most 16). So 31 × the processor count covers every thread the run queues and zombie lists can hold, and S1's growth path applies only if a queue's capacity ever grows. A spawn takes its record before its push, so one about to find its queue full holds a 32nd for a moment.
- **The memory.** It is counted in the boot-time sizing, before the root is sized, and nothing is allocated when a thread starts. So ADR-0005 G5's per-thread figure is unchanged.
- **Where the state is kept (ruling Q3, refining the Ownership bullet).** Each record's state (stack, FPU area, user context, exit mark) is read and written under a lock of the record's own, not under the owning queue's. That lock is taken after the owning queue's lock and the processor's thread table, and before a block's allocator. The location and the generation are kept as the decision above says.

**Threats.** A stale name cannot wake another thread (the generation). No program can name a record: handles name thread objects (ADR-0005 P1).

**Note, 2026-11-22: records from any block, and how long a wake-up keeps interrupts off (TASKS item 96; the coordinating session's rulings Q4 and Q18, its second ruling on Q6 of TASKS item 92, and its rulings on the review of TASKS item 96; no decision changes).**
- **Taking and giving back, with moves.** A spawn takes its record from the block of the processor it queues the thread on, or, if that block is used up, from the next block round that has one (`origin_sync::threads::take_from_any`). The reap still gives a record back to its own block, by its number.
  - **Why it is needed.** With moves, a processor's records can be held by threads that left it, so the count of 32 per block no longer holds block by block.
  - **Why it is enough.** The count holds for the machine: every held record belongs to a queued thread (at most 15 per processor) or a zombie (at most 16 per processor), wherever it came from, so at most 31 × P are held of the 32 × P there are. The settles' check of the records counts them for the machine for the same reason (the review's R96-3).
  - **Cost.** The search past the own block runs only when that block is used up, which no boot does.
- **The record check** (S20, trigger 3) is made before a slot is woken: the kernel compares the record the slot holds with the wake-up's by address (`KernelQueues::holds`, through `RunQueue::held`), after the generation compare, under the same lock. Records never move, so an address names one record. A wake-up that met an inconsistency finds the record's number from the record (`threads::number_of`), so the location loop keeps no register for it.
- **How long a wake-up keeps interrupts off (D93-2).** `sched::wake` runs the location loop, the compare and the IPI in one interrupts-off section (TASKS item 93). Each move it follows adds at most the rest of one move's critical section (B20, a hypothesis of 2 µs) and one look under a queue lock: about 2.5 µs on the budgets' figures.
  - **Paced** at S10's rate, or at the migration proofs' one move per thread per tick, a wake-up follows at most one move. Every proof boot of TASKS item 96 saw at most one follow per wake-up.
  - **Unpaced,** a caller that broke `migrate`'s documented rule could make one follow 64: about 160 µs, which **exceeds B18's 50 µs**.
  - **No such caller exists** in TASKS item 96. "thread migration: delivery" measures the longest span of a sender's wake-up that followed a move, `emu`: 162 µs on the default machine, 3.7 to 8.8 ms on the 65- and 67-processor machines; and, for scale, of one that followed none: 62 µs and 46 to 117 ms. Those long spans are the host descheduling a virtual processor, not the wake-up: a section with no lock, spun for the wake-ups' recent median, shows long spans at the same rate per second spent within about 10% on every large-machine boot (PERFORMANCE_BUDGETS.md, B18's note).
  - TASKS item 98's contract must re-check this bound when it adds run-time movers, and leave the section after every 8 follows if any unpaced caller can exist (Q18 (b)).
- **Past 64 retries** (Q17): the wake-up enters S20's pinned mode and follows again with moves withdrawn, as this decision says. A second exhaustion stops the kernel: the flag itself failed.

### S2. Threads are pinned unless declared movable, and some never move

**Decision.** Every kernel thread is created pinned to the processor its spawner names, exactly as today. A spawner may declare a kernel thread movable. User threads become movable when the task that finishes S3's conversion of the system-call paths lands; until then they are pinned. These never move:

1. a thread whose pin count is above zero, or that is pinned by its spawner;
2. **a thread whose end may have been seen**: `end_with` raises the pin before `Exits::end` and never lowers it, so a thread holding an exit mark never leaves its processor, and `origin_sync::exits`' argument (both of a thread's counts on its own processor) holds as written (N5);
3. a thread running on a stack that is not its own: `with_measured_stack` pins for its closure;
4. the boot or idle flow, which has no record;
5. a zombie (N6);
6. in this version, any thread that is not ready (N10: a sleeper's or timed waiter's deadline is in its own processor's ticks).

**The pin.** A count in the thread's record, raised and lowered by the running thread itself with plain stores and no lock: only the running thread writes it, and a mover reads it under the owning queue's lock, which the thread's last switch-out released after its last write.

**Evidence.** N3, N5, N6, N10; the storage proof's per-processor threads (`storage::cpu_io_thread`) and the scheduler's per-processor workers rely on staying put.

**Alternatives.** Movable by default with pins where needed: every one of N3's sites, and every per-processor proof thread, would be wrong on the day migration lands.

**Cost.** None, by construction, for a pinned thread: no mover looks past its pin. A pin section costs the running thread two plain stores (owed with S1).

**Migration consequences.** Existing threads keep their behaviour. A thread type that becomes movable says so at its spawn, reviewed with S3's conversion of its code.

**Threats.** Pins are the kernel's; a program cannot pin itself except through hard affinity (S11), which gives it no share.

**Note, 2026-11-22: the movable declaration, as built (TASKS item 96; the coordinating session's ruling Q3; no decision changes).** A record holds a `movable` flag, one byte in room the record had, so the kernel's record stays 128 bytes.
- **Who sets it.** The spawner, through `sched::spawn_movable`, after `Block::take` and before its push, while no thread runs on the record. `Block::take` resets it for the record's next thread, with a Release store after the new generation.
- **Who reads it.** A mover reads it, with the pin count, under the owning queue's lock (the review of TASKS item 93, R93-7). A wake-up that followed moves reads it after its loop, with no lock, between two reads of the generation, both the name's (Acquire, paired with `take`'s Release; the review of TASKS item 96, R96-5); a mismatch counts the follows apart.
- **Who is movable.** Every thread is pinned unless so declared. In TASKS item 96 only the migration proofs' threads are.
- **Later.** TASKS item 100's affinity replaces the flag (S11).

### S3. The processor index is read only under a witness that the thread cannot move

**Decision.** `cpu::this_cpu()` takes a witness, a zero-sized, non-`Send` type that only an interrupts-off section (`without_interrupts`'s closure), the trap entry, early boot and S2's pin guard can make, and that cannot outlive them. Every per-processor table lookup in thread context goes through it, so an index read before a possible preemption cannot be used after it. The 74 call sites are converted; a site without a witness does not compile.

**Evidence.** N3.

**Alternatives.** (a) Review alone: the hazard is invisible at the call site. (b) A token gate in `cargo xtask` refusing `this_cpu()` outside listed functions: the gates read tokens and are brittle (the review of TASKS item 58, F2). (c) Never moving threads: no balancing.

**Cost.** None, by construction: the witness has no size, and the code generated is the same load through GS.

**Migration consequences.** An interface change inside the kernel and the framework, converted call site by call site; S2's pinned default keeps every caller right meanwhile.

**Threats.** A confused index is how one thread would act on another's state (mark it ending, register it with a wait queue). The type closes it when the code is built (ADR-0018 V4, step 1).

**Note, 2026-11-22: how S3 and S2's pin are built, and what S3 costs (TASKS item 93; the coordinating session's rulings Q2, Q3 and Q8; no decision changes).**
- **S3's cost is not "none, by construction" at five sites** that read the index with no section before S3: `here_console`, `reap`'s check, `tlb::note`, `tlb::note_answer` and `run_probe`.
  - Each read the index in the very way S3 forbids, so each now opens an interrupts-off section: one flag save, a `cli` and a restore. The section is P0, not a choice (ADR-0018 V1, point 1).
  - Ledger row CL60 holds the cost's model and the static counts.
- **Two more sites, `wake` and the fair-share proof's worker, had sections of their own as first built.** The comparison with the base showed what they cost, so both changed (TASKS item 93's entry):
  - `wake` takes one section, as the base took one, now around the wake-up and its IPI decision. Its calls, locked instructions and GS load are the base's, with 19 more instructions (CL60).
  - The worker's measured loop has no section. It reads the index once, under one section, after it stops.
- **Everywhere else the cost is as stated.** The switch path's code equals the base's, instruction for instruction (TASKS item 93's entry).
- **As built, the witness is two types,** both made only in `origin_frame::trap`:
  - `Here`, from an interrupts-off section or the trap entry. Its holder cannot leave the processor while nothing it runs switches it out.
  - `Pinned`, from S2's pin or from the boot or idle flow, checked once at the flow's entry. Its holder stays on the processor even when switched out, and it gives a `Here`.
- **The pin guard's `Pinned`** comes through a one-time mint that `sched` holds. S2's pin, as built, costs more than S2's Cost paragraph says: `sched::pin()` takes this processor's queue lock in a section to find the running record (CL47's note of this date).
- **The index the witness gives** (`ThisCpu`) can be copied out as a plain value. The types stop a per-processor entry (`PerCpu::this`) from outliving its witness, but not a plain index from being used afterwards. Each such use is named in `REVIEW_T93_2026-11-21.md`.

### S4. A context moves between tables by lend, adopt and forget, under a move sequence

**Decision.** `origin_sync::contexts` gains four operations, safe code modelled by Loom:

| Operation | Where | What |
|---|---|---|
| `lend(frame)` | source | Takes the frame of a slot that is neither the running nor the ended one, by compare-and-swap from the frame to 0 (the same take a switch or a withdrawal makes, so at most one succeeds), and returns the slot's home. Fails if the frame is not there |
| `adopt(home, frame)` | destination | Claims a free slot with `home` (compare-and-swap from 0) and stores `frame` (Release): `publish` with a home that need not equal the frame. Fails if the table is full |
| `restore(home, frame)` | source | Puts a lent frame back after a failed adopt (compare-and-swap from 0 to the frame in the slot whose home is `home`) |
| `forget(home)` | source | Clears the home (Release store of 0) of the slot whose frame was lent |

A move runs lend, adopt, forget, in that order, so the source's home stays until the destination holds it: claim before retire, as the module asks. **The move sequence** closes N4 (a). Two machine-wide counters: a mover raises **begun** before `adopt` and raises **finished** after `forget` (Release); both are read-modify-writes. Every scan of all the tables (`cpu::stack_in_use`, `stack::slot_in_contexts`) reads finished, then begun, before it scans (Acquire), and begun again after; it scans again if the first two differ (a move under way) or if begun changed.

**The scan terminates** (the coordinating session's ruling of 2026-11-20). Every move holds the **move gate**, a machine-wide spin lock, from its `lend` to its `forget`, with interrupts off; the gate is taken after both queue locks, and whoever holds it takes no other lock. A scan makes at most four tries on the fast path above, which takes no lock. A try fails only if a move began or finished during it, and moves come at bounded rates: at most one per pass of a processor's idle loop, one per period and level of the periodic pull on each processor, one per directed request (an affinity change, an evacuation) and one per thread per period of the widest level it crosses (S7, S10, S11, S21). After the fourth failed try, the scan takes the gate, scans once with no move under way, and releases it: it waits at most for one move's critical section (budget B20, 2 µs), whatever the rate of moves. So a scan ends after at most five passes and one wait for the gate. A scanner may hold a queue lock (a stack dropped under it) and then take the gate; a mover holds two queue locks before the gate; the order is always queue locks, then the gate, so no cycle forms.

Why the fast path is enough when it answers, in outline: a scan that misses a moving home read the destination before the claim and the source after the forget. Reading the source's home after the forget (or after any later write to that word, each ordered after it through compare-and-swaps that read it) orders that mover's raise of begun before the scan's last read of begun. So either that raise was already in the scan's first read of begun, and then the first two reads differ unless the read of finished already included the same move's finish, which would have ordered its claim before the scan; or it was not, and begun changed. A single counter whose parity says "a move is under way" is not enough: two moves at once leave it even. Loom decides it (MV1 and MV6 below), not this outline.

**Capacity** (N13). A mover adopts only after it has found a free slot in the destination's run queue, under that queue's lock, so the destination's table never holds more than its queue's 16 contexts, one ended, and spawns in flight.

**Loom models** (new, "contexts: moves"), each with a negative control the tool plants:

| Model | Shows | Control |
|---|---|---|
| MV1 | A mover and a scanner that reads the destination first: the home is found, or the scan repeats | No move sequence: Loom finds the home missed |
| MV2 | A move against the source's own switch naming the same frame: at most one takes it | `lend` by a plain store |
| MV3 | The destination resumes the context only after `adopt`; the source never resumes it after `lend` | `adopt` before `lend` |
| MV4 | A `forget` followed by another context's claim of the same slot: no scan is told "uncovered" for the moved stack | The second raise made before `forget` |
| MV5 | A full destination: `restore` gives the source back a frame it can resume | `restore` omitted |
| MV6 | Two movers contending for the gate and a scanner on the fast path: neither moving home is missed | One counter whose parity marks a move under way, with the gate taken after the counter's first raise |
| MV7 | A scan racing back-to-back moves of the same context: the scan answers within four fast tries and one slow one, and finds the home | The slow path removed: the scan gives up without an answer |

**Evidence.** N4 (a), N13; the T68-2 finding in the review of TASKS item 68, the same shape.

**Alternatives.** (a) One table for the whole machine: every switch searches it, with contention on 65 to 288 processors, and TASKS item 77's per-processor naming of the ring-3 stack becomes machine-wide. (b) A birth table that keeps every thread's home for life, with guest slots elsewhere: no scanner race, but a processor's table fills with homes of threads that left it, and its capacity becomes the machine's. (c) Claim-before-retire alone: the miss above.

**Cost.** Unmeasured, owed: per move, the gate, three compare-and-swaps and two stores on lines the other processor holds, and one increment of each of two machine-wide counters; per scan, three more loads, and the gate on the rare slow path. Moves are serialised machine-wide by the gate; if a 65- to 288-processor machine shows contention on it, a gate per NUMA node is the next step, which the counter pair already allows. A switch is unchanged.

**Migration consequences.** The module's closing note ("A context never moves between processors") is replaced by this decision. `Contexts::withdraw` keeps its meaning: only a spawner calls it, for a context it never queued, which was never moved.

**Threats.** A double resume, two processors on one stack, is memory corruption; the compare-and-swap on the frame word rules it out by construction. The move sequence keeps a stack from being given back while a context may run on it.

**Note, 2026-11-21: how S4 is built (TASKS item 95; the coordinating session's rulings Q1, Q2, Q4, Q6 and Q12; no decision changes).**
- **The witnesses (Q1).** `lend`, `adopt`, `restore` and `forget` take a `Moving`, which only `Moves::begin` makes, by taking the gate and then raising begun. Dropping it raises finished (Release) and then frees the gate, also on a panic. A panic under the gate halts the kernel anyway (its panic policy), so that matters to the host tests and the models, not to recovery. `lend` returns a `Lent` that names its slot. `adopt` consumes it and answers an `Adopted`, or hands it back with the refusal; `Lent::restore` consumes it too; `forget` is on `Adopted` alone. So a frame is never adopted twice, nor adopted and restored, and a home is forgotten only after an adoption (the review's R95-1; three doctests show what does not compile). A `Lent` dropped unsettled restores itself, and an `Adopted` dropped forgets. `Moves::relocate` runs the four in the order above, with the mover's own work (S6) between `adopt` and `forget`. Leaking a token with `core::mem::forget`, which safe code can, defeats this (the delta review's D95-5): nothing in the kernel may.
- **Begun is raised before `lend` (Q2),** not only before `adopt`. A pass that reads the source's frame word after `lend` took it then counts the move too, which covers a suspended frame on a stack that is not its home (S2 keeps such threads from moving anyway). A refused `lend` is therefore an empty move: both counters raised, nothing taken (CL48).
- **`lend`'s refusals (Q4).** It reads the leaving record (S5) before the slots and answers `Leaving` (not yet published: try later), `BootFlow` (the boot or idle flow's slot, which never moves) or `NotHere` (no slot holds the frame suspended). See S5's note for why `NotHere` alone is not yet S20's inconsistency.
- **The scan's fast tries (Q12).** `FAST_TRIES` is 4 in every real build and 2 in the Loom models, as the table's 32 slots are 4 there. A host test shows the kernel's four fast passes and one gated pass, deterministically.
- **MV4's control (Q6).** Run first, as the ruling asked, and not caught: with finished raised before `forget`, MV4 passed at three preemptions (5,859 executions) and exhaustively (1,363,402 executions, 82 s on the development Mac). It is not a bug. What the scan needs is finished raised after `adopt` (or `restore`): a pass that reads the source's home word from the forget on happens after the forget, so it counts the move's raise of begun, and a reading of finished that counts the move sees the destination's claim. Where finished falls against `forget` does not matter (the outline in `contexts.rs`, "Moves between processors"). An independent reviewer, given the code and the invariants and not this answer, said the same, and named the control that is a bug: finished raised before `adopt`. MV4 catches that at three preemptions and exhaustively ("a live context's stack was in no record"). The control column above is kept as accepted; MV4's controls in `cargo xtask loom` are MX4a (a claim by a Relaxed load and a store, not a compare-and-swap), MX4b (the forget's store Relaxed) and MX4c (finished raised before `adopt`, the reviewer's). The two answers agree, so nothing waited for a ruling (`docs/engineering/REVIEW_T95_2026-11-21.md`, section 1). The control column above is kept as accepted; MV4's controls are MX4a (a claim made by a Relaxed load and a store), MX4b (the forget's store Relaxed) and MX4c (finished raised before `adopt`), each caught, and the column's own control is recorded as run and not caught.
- **The gate's wait (the review of TASKS item 95, R95-2).** "It waits at most for one move's critical section", above, holds only while no other processor waits for the gate too. The gate is built as a ticket lock, which serves waiters in the order they came, so a scanner's wait is at most one critical section for each processor waiting ahead of it, each with interrupts off: still bounded whatever the rate of moves, but by the number of processors, not by one. A test-and-set lock would not bound it at all: a waiter could lose to every newcomer in turn. Loom cannot show the wait ends, since it does not explore an execution that spins past its bound; MV7 shows the scan answers, and finds the home, when the gate is won, and MV9 that the gate admits one holder at a time, a scan's last pass among them, each seeing the writes of those before it (the delta review's D95-2, with three controls). The order has two costs the old lock did not (the delta review's D95-3 and D95-8). A mover must turn interrupts off before it takes its ticket, not only once it holds the gate: an interrupt on its processor whose scan takes a later ticket would wait for ever for a caller that cannot run, and every later holder behind it; S3's interrupts-off witness, required in `begin`, is left to TASKS items 93 and 96. And a scanner may wait, with interrupts off, for one critical section per processor ahead of it, each possibly another scan's whole pass, during which it answers no interrupt; on the oversubscribed emulated machines a processor descheduled while it holds a ticket stalls everyone behind it. That cost is measured when moves exist (CL48, TASKS item 96).

**Note, 2026-11-22: the gate requires interrupts off, by type (TASKS item 96; the coordinating session's ruling Q1; the review of TASKS item 95, R95-12 and D95-3; no decision changes).**
- **The token.** `Moves::begin` takes `trap::InterruptsOff`, made only by an interrupts-off section (`trap::section_off`) and by the trap entry (`Switch::interrupts_off`). `Moves` is generic over a `MoveWitness`, so origin-sync need not name the framework's types.
- **The kernel's entry.** The kernel's one `Moves` is `cpu::MOVES`, over its tables. Its one caller of `begin` is `cpu::move_context`, which a host test holds. `move_context` passes `stack::same_slot` itself as `lend`'s same-stack check: a change from TASKS item 96's contract, which had a closure there, accepted by the coordinating session.
- **Non-generic, out of line.** `move_context` was first written public and generic; the kernel crate then instantiated it, exported `CONTEXTS` and `MOVES`, and the position-independent kernel reached them through the GOT in `resume` and `dispatch`. Non-generic and `#[inline(never)]`, it leaves those two exactly as before; only the mover pays an indirect call, under the gate.
- **`check_landing`.** It takes the same token (R93-10).

### S5. A frame becomes resumable elsewhere only after its processor has left the stack

**Decision.** `Contexts::switch` no longer stores the outgoing context's frame in its slot. It records the slot and the frame as **leaving**. The processor publishes them, storing the frame in the slot (Release) and then clearing the leaving record (Release), at its next call into the table: the top of its next `switch`, beside `retire_ended`, or the idle loop's and the tick's entry. Both run on another stack. Until published, nobody can take the frame, the source included; the source's own next switch publishes before it chooses. `covers` reads the leaving record before the slots, so that a scan cannot miss a frame between the two stores.

A thread is therefore movable from its source's next trap entry after it was switched out: within one tick, sooner on any interrupt. A mover that finds a frame not yet published picks another thread or tries later.

**Loom model** LV1: the source's last use of the outgoing stack (a relaxed store, as the exit models already model it) happens before any other thread's resume of the frame. Control: publication at the switch, as today; Loom finds a resume concurrent with the last use.

**Evidence.** N4 (b): `framekernel/frame/src/cpu.rs`, `exception_saved` (`call {dispatch}`, then `mov rsp, rax`) and `resume`.

**Alternatives.** (a) Publish from the trap stub right after `mov rsp, rax`: immediate and precise, but it changes the framework's assembly, and the store would be outside Rust's memory model, so Loom could not check it. (b) A per-processor count the stub raises after loading the new stack pointer, which movers wait on: a cross-processor wait inside a move. (c) Publish at the switch and let movers wait until the source's running slot changes: that does not prove the stack was left, since the return path runs after it changes. If S22's pickup budget is missed because of this delay, (a) is the next step, with its own review.

**Cost.** Unmeasured, owed: each switch adds one plain load (nothing leaving) or two plain stores (a frame published), on lines the processor owns, as `retire_ended`'s single load does (TASKS item 70, T70-2).

**Migration consequences.** `Contexts::switch` and `retire_ended` change; the trap entry does not. TASKS item 77's naming and checking of the ring-3 stack are unchanged: they read the running slot's home.

**Threats.** Without this, a move could put two processors on one kernel stack: memory corruption and a route from one thread's data to another's.

**Note, 2026-11-21: where a frame is published, as built (TASKS item 95; the coordinating session's ruling Q3; no decision changes).**
- **At the top of every `switch`,** before `next` is taken, so a switch straight back to the context that is leaving publishes it and then takes it (a host test). Every scheduler upcall, the tick's included, reaches `switch` whether or not it changes the running context, so a switched-out frame is published within one tick.
- **In `cpu::stack_in_use`,** beside its retirement, with interrupts off.
- **Not yet from the idle loop:** TASKS item 98 adds that call with the idle pull, the first code that needs a frame sooner than the next tick.
- **Never from a handler that can interrupt a switch** (an NMI, a machine check, a debug exception, a double fault), nor from the other vectors' handlers.
- **The boot or idle flow's first switch-out** claims its slot with no frame and leaves its frame like any other.
- **A window for TASKS item 96.** The scheduler's hook stores the outgoing thread's frame in its run queue under the queue lock; the switch records it as leaving only after that lock is released, on the way out of the trap entry. A mover that takes the source's queue lock in between sees a ready thread whose frame its table has not yet seen, and `lend` answers `NotHere`. So `NotHere` alone is not S20's inconsistency ("a `lend` that fails where its checks said the frame was published"): item 96's checks must establish that the frame was published, for example by the source's table answering whether the frame is in a slot, or by the run queue marking a thread switched out since the source's last call into its table.

**Note, 2026-11-22: the window between naming a frame and switching it out, told from a lost frame (TASKS item 96; the coordinating session's ruling Q2; no decision changes).**
- **The window.** A scheduler names the frame its running context leaves under its queue lock. Its switch records the frame as leaving only after that lock is released.
- **What a mover sees in it.** A mover that takes the lock in between finds the thread ready and not running, but its frame in no record.
- **How `lend` tells the window apart.** It reads `current` first (Acquire). If the running context's home lies in the frame's stack, it refuses `NotYet`. Only then does it read the leaving record and the slots.
- **The ordering.** `switch` stores `current` with Release, the same plain move on x86-64: the static counts of `resume`, `on_yield` and `dispatch` equal those before TASKS item 96, their disassembly included.
- **What follows.** `NotHere` and `BootFlow` after the mover's checks are now S20's inconsistency (trigger 2).
- **The evidence.** Loom's LV3, with LX5 (`current` stored Relaxed) and LX6 (the leaving record read first), each caught. At boot, control C96-1b (the running slot's frame handed to the mover) is refused by `NotYet`, and C96-1c (with `NotYet` gone too) by the published-frame check and trigger 2's withdrawal (ADR-0013, TASKS item 96's controls).

### S6. What a move carries, and what stays where it is

**Decision.** A thread is moved only if it is ready, not running, its frame published (S5), movable and not pinned (S2), allowed on the destination by its affinity (S11), within its move rate (S10), and both processors' FPU format is the boot processor's (below). The mover holds both queue locks, in CPU-index order, with interrupts off, then the move gate (S4), and in one critical section: `lend`; `adopt` (or `restore` and give up); `RunQueue::depart` on the source and `RunQueue::arrive` on the destination; the record's location; `forget`. What passes:

- **The run-queue entry:** the saved frame, the weight, the pending wake-up (`wake_pending`, so a wake-up that arrived before the thread blocked is not lost) and the thread's **lag**: its pass minus the source's floor (the lowest pass among its ready contexts), never below 0. It arrives at the destination's floor plus that lag. A move earns no credit and loses none, the rule a new or woken context already follows ("sleeping earns no credit").
- **Ownership** of the record's stack, FPU area and user context: nothing is copied, only the owning queue changes.

What stays: zombies and the exit counts (N5, N6), and so TASKS item 79's settle; sleeping and blocked threads (N10); TASKS item 73's reclaim (N7: the stack region is mapped alike everywhere, and every reclaim's shootdown reaches every processor); the address space (N11: the destination loads it at the switch, and PCIDs are off); ADR-0017 Z3's barrier, decided on the destination by its own last process (N12).

**The FPU format** (N9). Each application processor, at start, compares its XCR0 and the size of its enabled XSAVE state (CPUID.0DH.00H:EBX) with the boot processor's. One that differs never takes part in a move, as source or destination, and the boot says so. The SDM puts the supported-XCR0 bitmap and every component's offset in its Platform domain and checks a standard-format image only against XCR0 and its header (I1 §21.1.4, §13.4.3, §13.8.1), but never says outright that an image moves between logical processors (ledger §2.10), and AMD reports the same values for Zen 5 and Zen 5c without saying so either (§2.7). So it is checked, not assumed. The kernel's area size becomes the boot processor's, and each processor checks against it.

*Note, 2026-11-21 (TASKS item 94, as built, on the coordinating session's rulings on its contract; not an amendment).*
- **What is compared:** XCR0 as XGETBV reads it back, and CPUID.0DH.00H:EBX read after XCR0 is set: equal both, or the processor takes no part. Each component's offset and size in the standard format is in the Platform domain (SDM Vol. 1, Table 21 for leaf 0DH), so a difference there is not one the documents allow, and it is not compared.
- **The area:** the boot processor's CPUID.0DH.00H:ECX, never less than its own enabled size, recorded once by the boot processor before the first area is allocated. Shrinking it to the enabled size is TASKS item 111.
- **An AP whose enabled state is larger than the area** would write past every area its threads save into. It does not come online: it refuses before its handshake and is parked, as one below the baseline is.
- **The flag** is written by each processor on itself, with its control state, in one store before its handshake answer. It is asked through the processors online (`smp::Online::takes_part_in_moves`), so a processor parked after a late answer never takes part. The boot prints one line ("FPU format: … N of N processors alike, every one taking part in moves"), and ADR-0013 records it as a Report proof, until the mover of TASKS item 96 withholds such a processor.

**Evidence.** N2, N5 to N12.

**Wake-ups.** A woken thread stays on the processor it is on: S7 decides it.

**Alternatives.** Moving blocked and sleeping threads now, and placing threads at their wake-up: S7's wake-placement decision says why not.

**Cost.** None, by construction, on a switch. A move's cost is S4's and S5's.

**Migration consequences.** `RunQueue` gains `depart` and `arrive`, host-tested as the rest of it is.

**Threats.** The FPU area holds a thread's data and is restored only on its own thread, as today; a move copies neither the area nor the stack.

**Note, 2026-11-22: the move, as built (TASKS item 96; no decision changes).** `sched::migrate(thread, to, online)`, in one interrupts-off section:
1. the location read;
2. both queue locks, in index order;
3. the location read again, and the generation compared;
4. pinned mode checked under the source's lock;
5. eligibility:
   - the slot holds this record;
   - the thread is ready and not running (`RunQueue::frame`);
   - it is declared movable, and its pin count is 0;
   - both processors take part in moves;
   - the destination's queue has room;
6. `cpu::move_context`, under the gate. It publishes the mover's own leaving frame if the source is its processor, then:
   - lends the frame;
   - the destination adopts it;
   - `RunQueue::depart`, then `arrive`, then the location;
   - the source forgets the home;
7. the locks released, and one reschedule IPI to a destination that is not the mover's processor.

What it carries:
- **The lag.** A thread departs with its pass less its source's floor, and arrives at its destination's floor plus that lag: no credit, no loss.
- **A pending wake-up** is carried. `arrive` marks the slot woken, so the IPI's `preempt_for_wake` runs the arrival where a woken thread would run (the ruling's Q11).
- **No rate of its own** (Q13). Its callers pace themselves; S10's rate is TASKS item 98's.
- **A full destination** refuses the move ("queue full") with nothing lost, and a full source can move a thread out: "thread migration: integrity" shows both at boot (N13; the review's R96-13).

**Note (2026-11-22, TASKS item 96; the coordinating session's ruling on TASKS item 129's reproduction, a reading of this decision, not an amendment).** The floor this decision names, "the lowest pass among its ready contexts", is read as follows:
- **What counts.** The contexts that take part in the share: threads, and slot 0 where it is the boot flow, which yields in the race. Never the idle flow, which "stays apart" (S12).
- **With none of them ready, the floor holds its last value.** It never falls. `push`, `admit` and `arrive` never place a pass below it, so it only rises, and "a move earns no credit and loses none" holds: with all weights equal, a thread departs with a lag of at most one of its strides.
- **Why the reading was needed.** Before it, a queue with nothing ready took the running context's pass. On an application processor that is the idle flow's, which is charged only for the quanta nothing else wanted. So:
  - a thread that blocked there was left far above the next newcomer;
  - its next move carried the difference as lag, and every round trip added it again;
  - on the two-processor machines, lags at a move reached 4.06 billion strides;
  - one thread left on CPU 0 with 212 million of them waited past the integrity proof's patience behind the boot flow's yields (TASKS item 129, reproduced with the event ring on 2026-11-22).
- **As built.** `RunQueue` keeps the value (`kept_floor`). It is raised:
  - to the running context's pass when that context was the last ready one that counts (it blocked, slept or exited);
  - at a departure;
  - when slot 0 becomes the idle flow.

  A host test holds the case in small, against a control that plants the old fallback. The randomized model holds two properties over every step: no floor falls, and no departing lag exceeds one stride.

### Part B. Who moves threads, and where to

### S7. Load balancing: idle processors pull; busy ones only kick

**Decision.** Three paths, each a pull: the processor that will run the thread is the one that moves it.

1. **Idle pull.** A processor about to run its idle flow (nothing ready in its queue) looks for a movable ready thread: in its own core first, then its cache domain, then outward through S8's levels; at each level it takes from the processor with the highest load whose surplus is at least one thread, preferring threads that are not cache-hot (S10). It runs in the idle flow, with interrupts off.
2. **Idle kick.** A processor that makes a thread ready while it already has one waiting, and while a processor nearby is idle (a per-domain mask of idle processors, kept as processors enter and leave their idle flow), sends one reschedule IPI to the nearest idle one, at most one kick per tick per processor. The kicked processor's idle flow runs path 1.
3. **Periodic pull.** At each level of S8's domains, on that level's period (S10), a processor compares its load with the busiest processor of its sibling groups and pulls one thread if moving it lowers the larger of the two loads, that is, if the difference exceeds the thread's own weight scaled by capacity; at the NUMA level the difference must exceed one and a half times it. It runs in the tick handler, before the queue's step, with interrupts off.

**Wake placement in version 1** (a decision, at the coordinating session's ruling of 2026-11-20). A woken thread is queued on the processor it is on, the one where it blocked, as today. Balancing happens only by the three paths above: idle pull, the kick and the periodic pull. No processor is chosen at a wake-up, for two reasons. **S6's deadlines:** a blocked or sleeping thread's deadline is in its own processor's ticks (N10), so placing it elsewhere at its wake-up would mean turning that deadline into monotonic time and moving it with the timer queue, and S1's wake-up would then change a thread's location as well as its state. **S10's cache-hot rule:** a thread that blocked a moment ago is likely still in its processor's caches, so its own processor is the right first choice, and an idle one in the same cache domain takes it within budget B21 if that processor stays busy. Placement at wake-up is later, by amendment, once S7's and S10's figures show whether the pull's delay matters.

Idle pull and kicks keep the machine **work-conserving**: no processor idles while another has two or more movable ready threads it may take. The periodic pull keeps it **fair by weight** where every processor is busy. Each processor publishes its load in a word of its own, written under its queue lock, read without a lock by balancers; a stale read only picks a worse candidate, since the move checks everything again under both locks.

**Locks.** A move takes the two queue locks in CPU-index order; a processor that holds its own and needs a lower one releases and takes both in order. No mover holds a wait queue's lock (a waker never does either), and none starts a TLB shootdown while holding a queue lock, so a processor spinning on a queue lock with interrupts off never holds up a shootdown that the holder waits for.

**Evidence.** Work stealing (P3, §4): balancing initiated by the processors that have nothing to do costs the busy ones nothing. ADR-0018 V1: no avoidable cost on the busy path. P3's random victims and bounds do not carry over (its model has no blocking, no fairness between programs and no locality; ledger §3.4), so victims are chosen by topology and load.

**Alternatives.** (a) Periodic push by busy processors: the busy path pays. (b) One machine-wide queue: one lock for every switch. (c) Topology-blind random stealing (P3): moves across caches and nodes freely. (d) A balancing thread per domain: one more thread to schedule, the same locks.

**Cost.** Unmeasured, owed: idle pull costs only idle processors; a kick costs one load of an idle mask per make-ready with a thread already waiting, and an IPI when one is idle; the periodic pull costs each processor, at each period, a read of its sibling groups' load words, and sometimes a move (S22's B22).

**Migration consequences.** `sched::run_ap`'s idle loop and `on_tick` gain the pulls; the queue's step is unchanged.

**Threats.** A program cannot move another's threads. One that makes threads ready in a loop causes at most one kick per tick per processor.

### S8. Scheduling domains from each processor's own reports

**Decision.** At boot, each processor reads its own reports, and the kernel builds a tree of domains, narrowest first:

| Level | From | On the target hardware |
|---|---|---|
| Core (SMT siblings) | the thread field of leaf 1Fh, 0Bh or 80000026h (already read) | two threads per core on Zen 5 and Zen 5c; one or two on Intel's hybrid parts, by core type |
| Cache cluster | leaf 04H (Intel) or Fn8000_001D (AMD): processors whose APIC IDs agree above the sharing shift for L2 | Intel's efficient cores share an L2 per module |
| Last-level cache | the same, for the last level | an AMD core complex (8 Zen 5 or 16 Zen 5c cores) |
| Package or die | the package and die fields | |
| NUMA node | SRAT's proximity domain, nodes ordered by SLIT's distance | two nodes on every boot-matrix machine |
| Machine | | |

A level whose groups each hold one child is dropped. Where ACPI's PPTT is present its cache structures take precedence, as ACPI says (ACPI66 §5.2.31). Leaf 04H is in the SDM's Logical Processor domain (I1 §21.1.4), so each processor reads its own; where two processors disagree about sharing a cache, the kernel takes the conservative answer (no sharing) and says so. The boot prints the tree, and a Report-class proof compares it with the machine `cargo xtask` booted.

**Evidence.** Ledger §2.1, §2.2, §2.7, §2.8.

**Alternatives.** (a) PPTT first: optional, and not shown to exist on x86 platforms (ledger §2.2). (b) Cache-to-cache latencies measured at boot: slow, and under virtualization they measure the host. (c) A flat machine: against S27-10.

**Cost.** None, by construction, while the system runs: built once at boot.

**Migration consequences.** `framekernel::numa`'s map gains its first user. Hot-added processors (ADR-0008 Amendment 1, D2) join the tree with S21's work.

**Threats.** The reports are the processor's and the firmware's (AC1). A malformed table (PPTT, SRAT, SLIT) is parsed as untrusted input, as every ACPI table is (ADR-0019).

**Note (2026-11-21, TASKS item 97; the coordinating session's ruling Q1 on its contract, a reading of this decision, not an amendment).**
- **What "its cache structures take precedence, as ACPI says" means.** It is read with ACPI 6.6 §5.2.31.2. That section gives precedence to a cache's properties flagged valid in the table (its size, sets, ways, line size and type) over the processor's own discovery. Which processors share a cache is not among them: the PPTT's cache structures complement or replace that discovery.
- **How sharing is decided.** The PPTT's claims and CPUID's are both claims, combined conservatively, as the build plan's invariant has it ("a disagreement about sharing a cache is read as 'not shared' and reported"). Each group is split wherever either claim splits it. Every processor whose group either claim would have made larger is counted, and the "scheduling domains" proof fails. Where CPUID reports no sharing, the PPTT's claim stands alone.
- **A PPTT that breaks a rule of §5.2.31 its reading rests on** is not used for sharing at all. Examples are a cache chain entering a cache that another level's node lists, or a processor the PPTT and the MADT do not both name.
- **No boot stops on this.** The proof is Report class (ADR-0013 R3), so on a real machine whose firmware disagrees it degrades the verdict, and the boot goes on.

### S9. Capacity and load, only from what the hardware reports

**Decision.** Each processor has a capacity from 1 to 1024, the most capable processor of the machine being 1024, taken from the first of these sources that holds:

1. **Intel's Hardware Feedback Interface** (CPUID.06H:EAX\[19\]): the performance capability of the processor's row, class 0, scaled so the largest in the machine is 1024; updated when the package's thermal interrupt reports a change (IA32_PACKAGE_THERM_STATUS bit 26), which the kernel acknowledges by clearing the bit after reading the table (I3 §17.6.6, §17.6.7). A processor whose every cell is 0 has capacity 0: no movable thread is placed there, and its movable threads are pulled away (Intel's "avoid" hint). With the force-idle flag also set, pinned threads there keep running at M3; injecting idle time for them is power management's (M10).
2. **ACPI `_CPC`'s Highest Performance**, on any vendor where firmware publishes it, re-read on processor Notify 0x85; `_OSC` bit 12 is set once the kernel acts on it (ACPI66 §8.4.6, pp. 475 and 644).
3. **The processor's own register:** Intel's IA32_HWP_CAPABILITIES Highest (thread scope), or AMD's CPPC_CAPABILITY_1 HighestPerf (per logical processor, and readable only once CPPC is enabled, which is power management's to decide; AMD documents no change notification, so it is re-read on a slow period).
4. **AMD's static efficiency ranking** (Fn8000_0026): an order among cores of one type, not a ratio; it breaks ties.
5. **Otherwise every processor is 1024.**

Never from the core-type code (Intel: its value implies no attribute; AMD: the native model ID does not even tell Zen 5 from Zen 5c) or a model name (§24). A processor's **load** is the sum of its runnable threads' weights times 1024 over its capacity; balancing compares loads, so a slower processor is given proportionately less. Thread Director's per-thread classes are not used in M3: Intel gives them no meaning and states no context-switch duty (ledger §2.4); choosing a column per thread waits for measurements on the hybrid machine, as an amendment.

**What is shown where** (the coordinating session's ruling of 2026-11-20). QEMU 11.1 models no hybrid part (ledger §5), so:

| Where | What it shows |
|---|---|
| Host tests | Capacity from synthetic inputs of every source: HFI tables (a change, an "avoid" row, force idle, efficiency 255), `_CPC` packages (revisions 2 to 4, Highest differing between processors, Notify 0x85), HWP and AMD CPPC register values, and Fn8000_0026 reports (core types, rankings, the heterogeneous flag); placement and balancing over those capacities |
| Boot proofs, QEMU and KVM guests | SMT, NUMA, packages and caches as QEMU lays them out, and every processor at equal capacity (source 5): moves, balancing and affinity at equal capacity |
| Physical machines in scope only | Hybrid placement and every capacity source but the last: ADR-0009 H4's hybrid machine (#2) and AMD machine (#5), under exit test M3-6, with records in `docs/hardware/records/` |

Nothing under QEMU is evidence of hybrid placement (ADR-0018 V9; §74).

**Evidence.** Ledger §2.3 to §2.7.

**Alternatives.** (a) Capacity from core types (refused by both vendors' texts). (b) A benchmark at boot (the host's speed under virtualization, and noise on hardware). (c) Equal capacity everywhere (against S27-10 on every hybrid machine in scope).

**Trade-offs.** Long-run fairness on a hybrid part is "equal time on capacity-scaled processors", not equal work; accounting records both (S14).

**Cost.** Unmeasured, owed: a table read and a rescale of every processor's capacity on each HFI change (the SDM gives no rate); a multiply per load update.

**Migration consequences.** The HFI table is kernel memory that the processor writes: allocated per package, zeroed before it is enabled, never mapped to a program, and not reused after disabling until the processor acknowledges it (I3 §17.6.4, §17.6.5). INIT and SIPI clear the enables (§17.6.9), so S21's restart of a processor enables them again.

**Threats.** The table's values are bounded bytes; nothing is indexed by them without a check (ADR-0017 Z9 where an index could be chosen).

**Note, 2026-11-22: INIT and SIPI as SDM -093 reads, and how sources 1 to 3 are built (TASKS item 106; the coordinating session's rulings Q1, Q2, Q5, Q6, Q11 and Q14 of 2026-11-21; a reading, no decision changes).**

- **INIT and SIPI.** The migration note above says they clear the enables. SDM -093 says otherwise: IA32_HW_FEEDBACK_PTR and IA32_HW_FEEDBACK_CONFIG keep their values across INIT (I3 §17.6.4, §17.6.5). Only GETSEC\[SENTER\] and GETSEC\[ENTERACCS\] reset them, which Origin never runs; INIT and the wait-for-SIPI state reset only the per-processor Thread Director enable (17D4H), which M3 does not use (§17.6.9). §17.6.9 still asks software to re-enable when it processes a SIPI. The kernel enables the HFI only at the thermal round (`origin_frame::hfi`), once every processor has started, so no INIT or SIPI follows the enable at boot, and the enable's own read-back is the read-back after start. S21's restart of a processor should read both registers back rather than assume they were cleared.
- **Source 1** is used on a package only where every processor of it reports the same table, and every one's thermal LVT entry was free: an entry the firmware unmasked or pointed at SMM is left as it is, and nothing is written to that package (ruling Q5). Each processor's start only reads; the round takes every LVT of the package first, each on its own processor, and only then has the package's first processor enable the HFI, so every LVT is unmasked before the hardware's initial notification (the independent review's R106-2). An HFI the firmware enabled is left as it is and not used: the kernel cannot know whose memory its pointer names.
- **Heat** needs a sensor beside the registers: the thermal monitor (CPUID.01H:EDX\[29\]) or the digital thermal sensor (CPUID.06H:EAX\[0\]), from which the thermal interrupt comes (I3 §17.8.2.5, §17.8.4, §17.8.5.1). QEMU 11.1.3's software processor reports the registers and neither sensor.
- **Source 2** is empty at boot on every machine: ADR-0019 A2 keeps the firmware's AML out of the kernel, so `_CPC` is evaluated only in the AML proof, on QEMU's tables, and counted (ruling Q1 (a)). Notify 0x85 and `_OSC` bit 12 wait for the SCI and `_OSC` (ADR-0019 areas C and D; ruling Q9). An AMD machine without the HFI, whose firmware did not enable CPPC, therefore has equal capacity at M3.
- **Source 3** is read only where the firmware already enabled HWP or CPPC: the kernel never writes an enable or a request register (ruling Q2; power management's, M10).
- **Capacity is live** in `origin_frame::capacity`: placement reads `capacity::current(cpu)`, and `Domains.capacity` keeps the boot value (ruling Q6). The thermal interrupt's handler reads a package's rows, rescales every processor, allocation-free and in O(processors), and only then acknowledges (ruling Q14). It publishes processor by processor, so a reader may see some new capacities beside old ones for a moment; capacity is a placement hint, and that mixed view is harmless.
- **What QEMU shows** is the absence of every source, the round in which every processor takes the thermal vector once, and the MSR probe recovering a #GP. Every present path needs ADR-0009 H4's machines (`docs/research/2026-11-21-hardware-capacity.md` §8).

### S10. Migration cost, cache-hot threads, move rates and NUMA

**Decision.**

- **Cache-hot.** A thread that ran on its processor less than H ago is cache-hot, measured by that processor's own TSC (never by comparing two processors' counters, N10). Only an idle pull from a processor sharing the source's last-level cache may take a cache-hot thread; periodic pulls take threads that are not.
- **Move rate.** A thread moves at most once per period of the widest level the move crosses. This bounds S1's wake-up retries and prevents ping-pong.
- **Periods.** One tick at the core and cache levels, 4 ticks at the package, 16 at the node and the machine.
- **NUMA.** A thread has a home node, at M3 its spawn processor's. It leaves its node only through an idle pull that found nothing nearer, or a periodic pull at the node level past S7's threshold. Pages do not follow it: memory placement is §28's NUMA policy, decided with the memory work.

H starts at 1 ms; H, the periods and the thresholds are hypotheses to be set on declared hardware (ledger §7, question 5).

**Evidence.** CLAUDE.md §27 (migration cost), §66; ledger §2.8, §2.11.

**Alternatives.** No cache-hot rule (moves that cost more than they gain); a migration cost measured per machine at boot (noise under virtualization).

**Cost.** Unmeasured, owed: one TSC read and compare per candidate examined.

**Migration consequences.** None beyond S7.

**Threats.** None new.

### S11. Affinity

**Decision.** A thread has a **hard** affinity, the processors it may run on, and an optional **soft** one, a domain the balancer looks in first. The hard affinity is one of: pinned to one processor; a domain of S8's tree; an interned set, shared and reference-counted; or any. It is never a per-thread bitmap (288 processors are 36 bytes, 4,096 are 512). Placement and balancing never put a thread outside its hard set. A change of hard affinity that excludes the thread's present processor moves it at once, by a directed move: whichever processor makes the change performs the move, with both queue locks; a move need not be made by its destination, only balancing is pull-only. An empty set is refused.

`CpuIndex` never leaves the kernel (ADR-0008, "Migration consequences"). **The SDK's surface for affinity is a later SDK decision under ADR-0011, and this ADR does not design it**; the only constraint it sets is that one, and that a program reaches no processor it could not reach otherwise. Setting another process's threads' affinity needs a right over that process (its `manage` handle, ADR-0005); the default is any.

**Evidence.** Matrix rows S25-09, S26-08; ADR-0008's note on exposing processors.

**Alternatives.** Per-thread bitmaps (memory that grows with the machine); a processor number in the SDK (ADR-0008 refuses `CpuIndex` for it).

**Cost.** None, by construction, on a switch; owed: one check per candidate in a pull.

**Migration consequences.** An SDK surface later, through ADR-0011; nothing here is ABI.

**Threats.** A program pinning its own threads harms only itself; affinity never adds share. A Refuse-class proof shows that no thread ever runs outside its hard set (S23).

### Part C. Classes, priorities, accounting, quotas and inversion

### S12. Three classes on one proportional-share queue, classed only by trusted signals

**Decision.** Every thread is **Interactive**, **Normal** or **Background**; the idle flow stays apart. All three share one proportional-share queue per processor, and the class decides:

| | Background | Normal | Interactive |
|---|---|---|---|
| Weight band | 1 to 3 | 4 to 32 (default 8, today's `DEFAULT_WEIGHT`) | 33 to 256 |
| Preemption on wake-up | never: it waits for the tick | today's rule: if its pass is no higher than the running one's | always over a running Normal or Background thread |
| Placement | prefers efficient processors (HFI efficiency, low capacity) | by load | prefers capable processors |

A Background thread is never starved: its share is its weight's share, however small, and nothing is strict priority. **How a thread is classed:** a kernel thread by its spawner. A user thread takes the class of the thread that made it, capped by its process's **class ceiling**; a process's ceiling is set by whoever launches it, within the launcher's own; the session launcher now, and the compositor from M8, hold the right to raise a process's threads to Interactive while it is in the foreground and to lower them after. A program may lower its own threads' class, never raise it above its ceiling. There is no heuristic interactivity detection.

**Evidence.** CLAUDE.md §26, §27 (foreground responsiveness), §2 P2; `RunQueue::preempt_for_wake` already gives woken threads the processor; ADR-0005 P11 (limits set by the creator).

**Alternatives.** (a) Strict priority between classes: Background work, security updates among it, could starve under steady foreground load. (b) Heuristics from sleep patterns: a program can game them, and nobody can explain a decision they made. (c) Many priority levels (dozens): more to get wrong, no requirement asks for them. A reserved-bandwidth class for the compositor and audio (§52, §55) is not decided here: it belongs with M8's frame deadlines and M10's audio, as an amendment.

**Cost.** None, by construction, on the switch path: the class sets a weight and a branch in the wake-up rule that exists today. Owed: a placement preference examined per candidate.

**Migration consequences.** ADR-0005's start record will carry the ceiling as one of the limits a creator sets: an ADR-0005 amendment, the owner's (ABI). **The SDK's surface for classes and levels is a later SDK decision under ADR-0011**, not designed here.

**Threats.** A program cannot make itself Interactive (deny by default), so it cannot take the processor from the foreground by claiming it.

### S13. Priorities: levels within a class

**Decision.** Within each class a thread has a level (five in Normal and Interactive, three in Background), each selecting a weight inside the class's band in steps of about √2 (Normal: 4, 6, 8, 11, 16; Interactive: 45, 64, 90, 128, 181; Background: 1, 2, 3). The table is the run queue's, not an ABI: programs see classes and levels.

**Evidence.** Matrix row S25-08; today's weights (`runqueue::MAX_WEIGHT` 1024 leaves room for S16's lending).

**Alternatives.** Raw weights in the SDK: a number whose meaning changes with every other thread's.

**Cost.** None, by construction: a table lookup at a level change.

**Migration consequences, threats.** As S12.

### S14. CPU accounting from the TSC at every switch, on one processor at a time

**Decision.** At every switch, the processor adds to the outgoing thread the time since it switched in, measured by its own TSC: run time, and run time scaled by its capacity (work on a hybrid part). It also keeps per thread the ready-wait time (from becoming ready to running, on one processor: a move closes the source's part and opens the destination's), voluntary and involuntary switches, and moves; per processor, busy and idle time. A thread's process accumulates its threads' times at their switch-outs. Interrupt time is not split out at M3. **Two processors' TSC readings are never subtracted** (N10).

Reading: a process reads its own threads' accounts and those of processes it holds a `manage` right over; machine-wide figures need the Diagnostics or Hardware-health right (ADR-0014 S7, ADR-0011 §49).

**Evidence.** Matrix row S26-09; ledger §2.11.

**Alternatives.** Tick sampling (a thread that runs between ticks is never charged, and a 100 Hz tick gives 10 ms grain).

**Cost.** Unmeasured, owed: one RDTSC and a few additions per switch (B25).

**Migration consequences.** The SDK's accounting interface is ADR-0011's.

**Threats.** Another program's CPU time is a timing channel (AC5 accepts such channels in general); the rights above keep it from programs without a reason to see it (§61).

### S15. CPU quotas: a share per process, and an optional cap

**Decision.**

- **Share.** A process has a CPU share, in weight units, set by its creator within the creator's own; it defaults to its class's default weight. The share is divided among the process's runnable threads in proportion to their levels' weights, so a process cannot take more of a contended processor by making more threads. Each runnable thread's effective weight is its weight times the process's share over the sum of its runnable threads' weights, read from two words of the process at each charge; the sum changes at each block and wake-up of the process's threads.
- **Cap.** Optionally, at most a given processor time per period (for example two processors' worth per 100 ms), from a budget refilled each period and taken by each processor in slices of 1 ms at its ticks. A processor that finds the budget empty makes the process's threads on it ineligible until the refill.
- **Who sets them.** The creator, as every other limit (ADR-0005 P11); a creator cannot give more than it has. Kernel threads have neither.

Idle pull keeps the machine work-conserving whatever the shares (S7): a process with eight threads on an idle eight-processor machine still runs on all eight.

**Evidence.** Matrix row S26-13; the system threat model ("Limits a flood: a server stays up as far as its quotas reach"); P4 §4 and P1 p. 27 name hierarchical sharing as the next step beyond one level.

**Alternatives.** (a) Per-thread fairness only, as today: a hostile program takes more of the machine by making threads. (b) Shares nested to any depth (hierarchical stride, P4 §4): error bounded by the tree's height, but more work on every charge; a later amendment. (c) A cap enforced by lowering the class instead of throttling: no bound.

**Trade-offs.** The effective weight is approximate while a process's threads block and wake on several processors at once; the error is bounded by one charge on each processor.

**Cost.** Unmeasured, owed: an atomic update of the process's runnable sum at each block and wake-up of its threads, two loads at each charge, and a budget take per tick per capped process.

**Migration consequences.** The start record gains the share and the cap: an ADR-0005 amendment, the owner's (ABI). Until then the kernel's proofs set them directly.

**Threats.** Makes CPU time one of the quotas a server is defended by.

### S16. Priority inversion: weight and class lent to an owner, transitively

**Decision.**

- **Spin locks** are held with interrupts off on one processor (B18 bounds the stretch), the extreme case of a ceiling: nothing can preempt the holder, so they cannot invert.
- **Owned waits.** A new wait-queue form records an owner: the first users are a kernel sleeping lock and, at M6, the user mutex of `origin.core`. While a thread waits on an owned wait, it lends its effective weight and its class to the owner: the owner runs with its own weight plus every weight lent to it (up to `MAX_WEIGHT`) and the highest class among the lenders. Lending is transitive along owner chains to a depth of 8; a longer chain stops lending there and is reported. A loan ends when the waiter is woken or gives up. The owner is found and changed under its owning queue's lock (S1's loop), so a loan reaches a thread wherever it runs.
- **Calls between programs are not decided here.** Origin's IPC is asynchronous channels, and a synchronous call is an `origin.core` helper (ADR-0005 P8): the kernel does not know that a service's thread is working for a waiting client. ADR-0005 owns the call's semantics, so lending a client's class and weight across a call is **an amendment to ADR-0005**: its Amendment 11, J1 to J6, accepted beside this ADR (item 341). It is built after the M3 core (batches 9 and 10), as TASKS item 109. Until then a service runs at the class its launcher gives it.
- **No ceiling protocol**: it needs every lock's users known in advance (P2 §IV), which user locks never are.

**Evidence.** P2: basic inheritance bounds blocking to one critical section per lower-priority job or per lock (Theorems 3 and 6) but rests on one processor and fixed priorities (ledger §3.3). Lending weight and class is the same idea for a fair-share scheduler; its bound on a multiprocessor is not proved here, and the proof is Report-class (S23).

**Alternatives.** The ceiling protocol (no deadlock, one critical section of blocking) where users are known: none of Origin's blocking locks have that property. Doing nothing until user locks exist: the mechanism would then arrive with M6 untested.

**Cost.** Unmeasured, owed: only on owned waits, a walk of the owner chain under its queue locks, one lock at a time.

**Migration consequences.** `origin_sync::wait` gains the owned form, Loom-modelled (a lending and a returning that leave every weight as it was).

**Threats.** A program can only lend what its own threads have, and only to the owner of a lock object it was given a handle to; the loan ends with the wait.

### Part D. Power, heat and SMT

### S17. Power and heat: the mechanisms in M3, the policy later

**Decision.** M3 builds these mechanisms, and power management (§67, M10) decides when each applies:

- **Avoid:** a processor with capacity 0 (S9) takes no movable thread.
- **Consolidate:** where the HFI efficiency capability is 255, a "pack" placement fills those processors first and spreads only when they are full (I3 §17.6.7).
- **Pack or spread:** pack places work on the fewest processors, the most efficient first, up to a utilisation threshold; spread places it on idle cores first. **The default is spread.**
- **Heat:** a processor that reports throttling (Intel's IA32_THERM_STATUS bit 0, core scope; on AMD, CPPC_STATUS.MinEx or a fall of CPPC_CAPABILITY_2's present maximum; under ACPI, Notify 0x83 or 0x84) has its capacity lowered to what it reports, or, with no figure, is skipped as a destination until it reports normal again. Where HFI exists it already folds heat into its values.
- **Idling processors on request:** ACPI's Processor Aggregator Device asks for a number of processors to be idled (`_PUR`, ACPI66 §8.5): the mechanism is S21's evacuation without taking the processor offline, all SMT siblings of a core first. Answering `_PUR` is power management's.

**Evidence.** Ledger §2.4, §2.7, §2.9: neither ACPI nor the vendors say what a scheduler should do with a throttled processor; Intel says what its hints ask.

**Alternatives.** Policy in M3 too: there is no power service yet to own it, and §67's laptop policy needs hardware.

**Cost.** None, by construction, with the default (spread, nothing throttled). Owed: the thermal interrupt's handling.

**Migration consequences.** None now; M10's power ADR picks the policy.

**Threats.** The signals come from the processor and the firmware (AC1).

### S18. SMT siblings: idle cores first; isolation as ADR-0017 left it

**Decision.** Placement prefers a core whose siblings are all idle over an idle sibling of a busy core; balancing counts a core's threads as one core-level group. ADR-0017 Z5 stays: STIBP where AMD recommends it, enhanced IBRS on Intel, and AC5's acceptance of timing channels between siblings. **Core scheduling** (only one process's threads on a core at a time), which Z5 left to the scheduler, is not built in M3: it is an option for Origin Server, as an amendment, and the records it needs (each thread's process, each processor's core) exist after S1 and S8. Taking siblings offline, Z5's other option, waits for S21.

**Evidence.** ADR-0017 Z5 and its alternatives; AMD: return predictions are not shared between siblings (A2 §3.2.9).

**Alternatives.** Core scheduling now (a constraint on every placement, for a channel AC5 accepts); SMT off (halves the threads; neither vendor asks for it).

**Cost.** None, by construction: a preference among candidates.

**Migration consequences, threats.** AC5 unchanged.

### Part E. EEVDF, the fallback, and processors going offline

### S19. EEVDF is evaluated, not adopted; stride stays the baseline

**Decision.** The run queue stays stride scheduling. EEVDF (P1) is evaluated once S1 to S16 pass their proofs (S26-14's order):

1. A second run queue with the same interface, host-tested, including property tests of Corollary 2's bound (lag strictly within ±q when no request exceeds a quantum) in steady conditions, and of zero total lag (Lemma 2).
2. Paired kernels (ADR-0018 V7: a scripted edit to an exported copy swaps the queue; no switch enters the tree) booted in turn on one machine: interactive wake-to-run latency under load, the fair share's error, and the switch cost.
3. Adopted only by an amendment carrying those figures. Stride then stays as S20's fallback queue.

Two policy questions EEVDF leaves open must be answered then: the lag a thread keeps across a sleep (P1 §5's three strategies; today's queue drops credit, P1's strategy 2) and how a request's length is chosen (Interactive threads would ask for short requests, which is how EEVDF gives latency without share).

**Evidence.** Ledger §3.1, §3.2: EEVDF bounds every client's lag by its own request, against stride's error that grows with the number of clients (P4 p. 8), and separates latency from share; it is a one-resource algorithm, with no multiprocessor part. With at most 16 contexts on a processor, stride's weakness is bounded.

**Alternatives.** Adopt EEVDF now (against S26-14's order, and before a baseline exists to compare with); never evaluate it (against S26-14).

**Cost.** None, by construction, until adopted.

**Migration consequences.** None until the amendment.

**Threats.** None.

### S20. The safe fallback: pinned mode

**Decision.** **Pinned mode** is today's scheduler with moves withdrawn: one machine-wide flag that every mover checks under the source's lock. Per-processor queues, classes, accounting and quotas go on. It is entered:

1. at boot, when the proof "thread migration: delivery" fails (ADR-0013 R2: a proof that covers one capability withdraws that capability; S23);
2. while running, when a move or a wake-up meets an inconsistency it can stop safely at: a `lend` that fails where its checks said the frame was published, a location naming a queue that does not hold the thread, a wake-up past 64 retries. A line names it, and the boot or the system goes on pinned.

It is never left during a boot. An inconsistency that could have let two processors run one context, or given back a stack in use, still stops the kernel (P0; ADR-0013 R1). Classes fall back to weights; an adopted EEVDF would fall back to stride. There is no boot option at M3: a setting needs the SDK (ADR-0011).

**The fallback never hides a failure from a proof** (the coordinating session's rulings of 2026-11-20):

- **Case 1 under `cargo xtask`.** "thread migration: delivery" is a **required marker on every machine** of every scenario. When it fails, the kernel withdraws moves and goes on, as ADR-0013 R2's Withdraw class lets it, but the missing marker and its `: … failed` line fail the scenario (ADR-0013 R8).
- **Case 2 under `cargo xtask`.** The line the kernel prints when it enters pinned mode at run time names the trigger (the inconsistency and where it was met), and it is a **failure marker in every `cargo xtask` scenario**, during the proofs or after them: a scenario whose boot prints it fails. The machine goes on pinned only so that the rest of the boot's evidence is still printed.
- **In production** (a boot that no `cargo xtask` scenario judges), either case writes a **persistent fault record** naming the trigger (CLAUDE.md §69's persistent failure records; until that store exists, a line in the kernel's log that survives to the console), and the system continues pinned.

So pinned mode is a recovery only where nobody is proving anything; wherever a proof is judged, it is a failure.

**Evidence.** Matrix row S26-15; ADR-0013 R1 and R2.

**Alternatives.** A second scheduler kept in the tree for the fallback (most of it would be the same code); a boot option (a setting outside the SDK).

**Cost.** None, by construction: one flag read per move.

**Migration consequences.** ADR-0013's table gains the Withdraw row.

**Threats.** A machine that falls back loses balancing, not isolation.

**Note, 2026-11-22: pinned mode, as built, and its cost (TASKS item 96; the coordinating session's rulings Q5, Q6, Q17 and A, and its rulings on the review; no decision changes).**
- **The flag.** One machine-wide flag, entered by compare-and-swap from 0, so the first trigger is the one named. Every later inconsistency is counted, a wake-up's second one included (the review's R96-10). The flag is never left.
- **The triggers:**
  1. "thread migration: delivery" failed, for any reason (R96-1);
  2. a `lend` that found no frame after the mover's checks (`NotHere`, `BootFlow`);
  3. a location naming a slot that holds another record or none. The record is checked by address under the queue's lock before the slot is woken, in `wake` and in `migrate`. After it, every queue is searched, each under its own lock; if none holds the record, the location is followed once more, and searched once more, so that a thread a move in flight carried past the search is still woken (R96-6);
  4. a wake-up past 64 retries, which then follows again with moves withdrawn. A second exhaustion stops the kernel (Q17).
- **The line.** `origin-kernel: scheduler: pinned mode: moves withdrawn: <trigger> on CPU <n><what it did next>`, at most 153 bytes, within the console's line (R96-9). A wake-up writes it out of line inside its interrupts-off section, through the console's queue, as an interrupt handler's line is written (a change from TASKS item 96's contract, which had it after the section); a mover after its locks are released. It is a failure in every `cargo xtask` scenario. Entered before the boot's verdict, it fails "thread migration: delivery" (Q5).
- **Against "None, by construction":** the record check of trigger 3 is on every wake-up's path. Traced instruction by instruction against the code before TASKS item 96, a wake-up of a blocked thread runs the same 163 instructions with the same two calls, and one of a thread not yet blocked runs 134 against 132 (PERFORMANCE_BUDGETS.md, CL47's note). A move adds one flag read. Everything else is out of line, reached only by an inconsistency.

### S21. Processors offline and back: in M3, after moves, in its own amendment

**Decision.** S24-14 is part of M3 (M3-1 names it), but it is not built with this ADR. It needs moves first, and then touches what this ADR does not: device interrupts aimed at the processor (IRTEs, MSI-X, the NVMe per-processor queues, ADR-0008 Amendment 1 D6), its timers, its membership of TLB shootdowns (`tlb::mark_online`), the park (ADR-0008 C7), the restart with ADR-0017's controls applied again, and HFI's enables, which INIT and SIPI clear (S9). **Recommended: ADR-0008 Amendment 2**, written after the move task lands. ADR-0020 gives it only **evacuation**: every movable thread of a processor moved away by directed moves; refused while a pinned thread other than the processor's own per-processor workers remains. **Exit test M3-1 names processors taken offline and back, so M3-1 stays partial until that amendment is built**, whatever else of M3 is done (`docs/ROADMAP.md`, M3).

**Evidence.** ADR-0008 Amendment 1 D2 (reserved indexes for processors brought online later); ADR-0017 Z5.

**Alternatives.** Build it here (a batch twice as large, across interrupts and devices); leave it out of M3 (against M3-1).

**Cost.** None, by construction: evacuation is a series of moves.

**Migration consequences.** M3-1 waits for that amendment's task.

**Threats.** Recorded in that amendment.

### Part F. Budgets and proofs

### S22. Provisional budgets (hypotheses)

**Decision.** These are in `docs/engineering/PERFORMANCE_BUDGETS.md` as B20 to B26, each a hypothesis with its model, judged against nothing until declared hardware exists (decision item 115; ADR-0018 V6):

| Budget | Path | Hypothesis | Model |
|---|---|---|---|
| B20 | One move between two processors that share a last-level cache, measured on the mover from the first lock to the last unlock | 2 µs | Two queue locks on lines the other processor holds (about 150 ns each), three compare-and-swaps and two stores on remote lines (about 100 ns each), two scans of 16 queue slots, the record's update |
| B21 | Idle pickup: a second ready thread on a busy processor, to running on an idle processor of the same last-level cache in its shallowest idle state | 10 µs | One IPI and one wake-up as in B2, one B20, one B1 |
| B22 | The periodic pull's work on a busy processor | 2 µs a tick on average, never more than 5 µs in one tick | A read of its sibling groups' load words; within B18's 50 µs with interrupts off |
| B23 | An Interactive thread woken from another processor while Normal threads keep every processor busy, to running | 20 µs | One IPI, wake preemption, one B1 |
| B24 | Balance: 2P equal, never-yielding, movable threads started on one processor of P | Within 100 ms every processor runs two (one more or less); over 2 s each thread's share within 25 % of the mean | S7's pulls at S10's periods |
| B25 | Accounting at a switch | 20 ns | One RDTSC and a few additions |
| B26 | A capped process | At most its cap plus one slice per processor in each period | S15's slices |

**Cost.** None, by construction: documents.

### S23. The proofs, their classes, and the machines

**Decision.** New boot proofs, classed by ADR-0013's rules:

| Proof | Class | Passes when |
|---|---|---|
| thread migration: integrity | **Refuse** (R1(b): structural, P0) | Movable threads that block and are woken through wait queues, and check their own register, stack and SIMD canaries, are moved at random among every processor for 2 s by a mover thread; a per-thread "running on" word, set by compare-and-swap at switch-in and cleared at switch-out, is never found set by a second processor; no canary changes; the settle's compare of heap, frames and stacks is unchanged |
| thread migration: delivery | **Withdraw** (moves); a required marker on every machine, so its failure fails the scenario (S20) | Every wake-up sent to a moving thread is received (the senders' and the receivers' counts agree) |
| scheduling domains | Report | The tree matches the machine `cargo xtask` booted, computed from its `-smp` fields and NUMA options |
| load balancing | Report | B24 on the machines of 2 to 8 processors; on the 65-, 67- and 288-processor software-processor machines the shares are reported, as ADR-0013 Amendment 2 does for the fair share |
| affinity | **Refuse** | No thread is ever switched in on a processor outside its hard set (a count that must stay 0) |
| scheduling classes | Report | B23; a Background thread among Normal ones gets at least half its weight's share |
| CPU accounting | Report | On each processor, its threads' run time plus its idle time is within 1 % of the elapsed time over the window; a worker's account agrees with its own measure within 10 % |
| CPU quotas | Report | B26; two processes of equal share, one with 8 threads and one with 1, get within a factor of 2 of equal time on one processor |
| weight lending | Report, with a Refuse part | The Background owner of an owned lock that an Interactive thread waits on, among Normal hogs on its processor, finishes its section within twice its unloaded time; the Refuse part: every lent weight is returned (each thread's weight after equals its weight before) |

Each has negative controls (`docs/engineering/M3_SCHEDULING_PLAN.md`).

**Pinned mode in the scenarios.** The run-time pinned-mode line (S20, case 2) is a failure marker in every `cargo xtask` scenario, and "thread migration: delivery" a required marker on every machine (S20, case 1).

**The existing timing proofs on the large machines.** Moves, balancing, classes and quotas change what the scheduler does under the proofs that already measure it: the fair share, the preemption window, the timed waits and the settles, on the 65- and 67-processor machines under QEMU's software processor. Two rulings made without such figures failed correct kernels there on 2026-11-19 (the review of TASKS item 66). So **every task that turns balancing on or changes what a proof accepts**: S7's pulls and kick, S10's rates and periods, S11's movable user threads, S12's classes, S15's quotas, S17's placement modes and S19's paired queues, first runs a **diagnostic round** on those two machines, with balancing (or its own change) on and off, both with the machine idle and beside load. It sends the figures to the coordinating session before any of that task's proofs, bounds, verdicts or "reported" rows are set or changed, and until they are read no allowance is widened. The build plan puts the round in each such task's acceptance. The existing proofs' own threads are pinned (S2), so tasks S1 to S6 change none of their scheduling; their rounds are the existing `boot-matrix` runs.

**New machines:** an SMT machine (`-smp 8,sockets=1,cores=4,threads=2`, two NUMA nodes) and a two-package machine (`-smp 8,sockets=2,cores=4,threads=1`, a node per package). As ADR-0008 Amendment 1 D7 requires, each is measured on the development Mac first, and joins CI's required jobs only if the real-processor boot job stays well inside its 180-minute limit on the slowest runner.

**Cost.** Owed, under ADR-0018's ledger row CL20 (the boot proofs' waits): about 2 s of moves and 2 s of balancing per boot, and the new machines' boots in `boot-matrix`.

**Note, 2026-11-22: the migration proofs, as built (TASKS item 96; the coordinating session's rulings Q9 and Q10, and its rulings on the review; an implementation, not a weakening).**
- **The mover.** The boot flow, which never moves (S2, point 4), not a mover thread: it runs both proofs' moves, paced at one per thread per tick of its own processor.
- **The "running on" word.** The integrity proof's threads hold their word themselves, not the switch path. Each runs its whole loop with interrupts off, so only its own yields and blocks switch it out. It takes the word by compare-and-swap first thing after each return from one, and releases it by compare-and-swap last thing before the next: for the only threads that move, "set at switch-in, cleared at switch-out", at no cost to the switch path. The word is not held from its release to the trap of the switch, nor from the switch's return to the take: a few instructions each. TASKS item 100, which makes other threads movable, revisits it.
- **The canaries.** Six values kept across each thread's loop, which the compiler may hold in registers or spill, so they check the restored stack a second time rather than the general registers; 256 bytes of stack; four XMM registers, which only the thread's FPU area holds. Pinning general registers needs an `asm!` helper in the framework (the review's R96-8; a follow-up).
- **Coverage as a progress wait.** "Every processor a source and a destination" is reached by moving until it holds, for at least 2 s and within 30 s, not judged inside a fixed 2 s: ADR-0013 R1 (b) lets a Refuse proof wait with a budget of seconds, not depend on load (R96-4). One boot of the 67-processor machine, judged the old way, made 276 moves while the host's load rose from 9.9 to 25.4, and missed it.
- **At the queue's capacity.** On two processors, a destination filled to its capacity refuses 8 moves as full with nothing lost, and a full source moves a thread out (N13's row; R96-13): 2 ms on the default machine, 141 ms to 1.5 s on the 67-processor machine.
- **A safe stop during integrity fails it** (R96-12). Integrity is a Refuse proof of a P0 property, and a correct kernel meets no S20 trigger, so meeting one fails the proof. The kernel itself goes on pinned, as S20 says; ADR-0013 R4 and R5 then stop the boot. The proof's verdict and the run-time behaviour are separate.
- **Delivery's path.** It counts the carried wake-ups whose receiver held a token it had not taken, before and after the move, and requires one (R96-2): 155 of 215 on the default machine, about 1300 to 1900 on the 67-processor machine.

## Invariants, and what shows each holds

| Invariant (ledger §6) | Held by | Loom | Boot proof and control |
|---|---|---|---|
| N1: a wake-up reaches its thread wherever it is | S1 | The wait-queue models with a mover: register, preempt, move, wake, block; no wake-up lost. Control: the location not re-read under the lock | delivery; control: `wake_pending` not carried |
| N2: a ready thread's state is complete when it can be picked | S1, S6: one record, changed only under the owning lock | — | integrity (`UNREADY_SWITCHES` stays 0); control: the location published before `arrive` |
| N3: no index used after the thread may have moved | S3 (built), S2 | — | the compiler; control: a site with no witness does not build |
| N4 (a): a stack is never uncovered by a move | S4 | MV1, MV4, MV6 | integrity's settle compare |
| N4 (b): no frame is resumed while its processor is on its stack | S5 | LV1 | integrity's "running on" word; control: publication at the switch (may not show under emulation; Loom's control is the evidence) |
| N5: exit counts on the thread's own processor | S2: an ending thread is pinned | the exits models, unchanged | the settles, unchanged |
| N6, N7, N8: zombies, reclaim, settle | S6: unchanged | unchanged | unchanged |
| N9: FPU state survives a move | S6's check | — | integrity's SIMD canaries; control: the FPU area not carried |
| N10: no deadline or TSC reading crosses processors | S6, S10, S14 | — | accounting |
| N11, N12: address space and Z3's barrier | S6: unchanged | — | the user-isolation proof, unchanged, with user threads movable |
| N13: a table's capacity | S4 | MV5 | integrity runs at the queue's capacity |
| Double resume | S4 | MV2, MV3 | integrity; control: `lend` by a plain store |

## Alternatives (the whole design)

| Alternative | For | Against |
|---|---|---|
| Never move threads; place each at creation only | No change to any proof | Neither balancing nor S27's placement after creation; matrix rows S26-07 and S27-* stay open |
| One machine-wide run queue | Balancing for free | One lock for every switch on 65 to 288 processors; TASKS item 77's per-processor stack naming lost |
| Adopt EEVDF with balancing at once | One rewrite | Against S26-14's order; nothing to compare it with |
| Classes as strict priorities | Simple, familiar | Background starves; updates are Background |
| Topology from PPTT only | The firmware's own view | Optional, and not shown on x86 (ledger §2.2) |

## Trade-offs

- **Order of work.** S1 to S6 change no policy and much code: the identity refactor and the move protocol land before any thread is balanced. Nothing measurable improves until S7.
- **Delay before a thread can move** (S5): up to a tick after it is switched out. Paid for safety; S5's alternative (a) is the remedy if B21 is missed.
- **Approximate group fairness** (S15) in exchange for a process not gaining share by making threads.
- **Hybrid fairness** (S9) is in time on capacity-scaled processors, not in work.
- **What QEMU cannot show** (SMT until the new machine, hybrid, HFI, CPPC, heat) is host-tested only until ADR-0009 H4's machines run.

## Migration consequences

- **No ABI or format changes here.** Three follow as decisions of their own: ADR-0005's start record gains the class ceiling, the CPU share and the cap (S12, S15); ADR-0011 decides the SDK's affinity, class, accounting and quota interfaces (S11 to S15); ADR-0008 Amendment 2 decides processors going offline and back (S21), and until it is built M3-1 stays partial. ADR-0005 Amendment 11 (S16; item 341), accepted beside this ADR, adds a flag to `channel_write` and a mark on a channel end, built as TASKS item 109.
- **ADR-0003 K7** ("Threads are never migrated") is amended by S4 to S7. **ADR-0013** gains S23's rows. **ADR-0017 Z5**'s core scheduling stays an option (S18).
- **`PERFORMANCE_BUDGETS.md`** holds S22's budgets as B20 to B26, and the ledger's rows CL47 to CL58 for the costs owed above.
- **The requirements matrix** holds a row for each decision, A20-S1 to A20-S23 (`docs/requirements/adr-0020.md`).

## Threat model (§58)

| Threat | Attacker | Decision | Remaining |
|---|---|---|---|
| A lost wake-up leaves a thread blocked for ever (denial of service against a thread) | none needed: a race | S1 | — |
| Two processors on one kernel stack (memory corruption, one thread's data in another's) | none needed: a race | S4, S5 | — |
| A stack given back while a context may run on it | none needed: a race | S4's move sequence and move gate | — (a scan always answers: four tries without the gate, then one under it) |
| More of the processor by making more threads | a hostile program | S15 | Approximate (S15's trade-off) |
| Claiming Interactive to starve the foreground or the system | a hostile program | S12 | — |
| Kick storms (IPIs to idle processors) | a hostile program | S7: one kick per tick per processor | Power and noise within that bound |
| Reading others' CPU time | a hostile program | S14: rights | AC5 |
| The HFI table: memory the processor writes | a hostile program trying to read or plant values | S9: kernel memory, never mapped to a program, zeroed before use, not reused before the processor acknowledges | AC1 |
| Sibling threads of two processes on one core | a hostile program | ADR-0017 Z5 (S18) | AC5, as accepted |
| Borrowing class through a lock | a hostile program | S16: loans only from waiters on a lock object it shares by handle, ending with the wait | — |

## What this does not decide

The SDK's interfaces (ADR-0011); the start record's new limits (ADR-0005); processors offline and back (ADR-0008 Amendment 2); power policy (M10); a reserved-bandwidth class for the compositor and audio (M8, M10); Thread Director's per-thread classes; moving blocked and sleeping threads at wake-up; memory following threads across NUMA nodes (§28); the tick's rate and stopping it on idle processors (budget B17); core scheduling (ADR-0017 Z5).

## Questions for the owner (answered 2026-11-20: the recommended answer for all 24)

The owner answered "Accept all 24" on 2026-11-20. The coordinating session numbered them as decision-sheet items 318 to 341: question n is item 317 + n. Question 24 is ADR-0005 Amendment 11.

| # | Item | Question | Recommended | Answer |
|---|---|---|---|---|
| 1 | 318 | Give every thread one record and name it by number and generation, found through its location, so wake-ups follow moves (S1)? Or pin threads while they are registered with a wait queue or port? | **Accept S1** | Accepted |
| 2 | 319 | Keep every kernel thread pinned unless declared movable, make user threads movable only after S3's conversion, and never move ending threads, threads on a borrowed stack, zombies, or (in this version) threads that are not ready (S2)? | **Accept** | Accepted |
| 3 | 320 | Make the processor index readable only under a witness the thread cannot move, a type the compiler checks, converting all 74 sites (S3)? Or rely on review? | **Accept the witness** | Accepted |
| 4 | 321 | Move a context between tables by lend, adopt and forget, under a move gate, with a begun/finished move sequence that makes a scan repeat at most four times before it takes the gate once, under seven new Loom models (S4)? | **Accept** | Accepted |
| 5 | 322 | Make an outgoing frame resumable only once its processor has left the stack, published at its next call into the table (S5)? Or publish from the trap stub (assembly outside Loom's view)? | **Accept deferred publication** | Accepted |
| 6 | 323 | Move only ready threads, carrying their lag, weight and pending wake-up, after checking every processor's FPU format against the boot processor's (S6)? | **Accept** | Accepted |
| 7 | 324 | Balance by pulls only (idle pull, one kick per tick, a slow periodic pull per domain level), never by busy processors pushing, and in version 1 queue a woken thread on the processor it is on, with no placement at wake-up (S7)? Or periodic push? | **Accept pulls** | Accepted |
| 8 | 325 | Build scheduling domains from each processor's own CPUID reports, SRAT and SLIT, with PPTT taking precedence where present and disagreements read as "not shared" (S8)? | **Accept** | Accepted |
| 9 | 326 | Take capacity only from the hardware's reports (HFI, then `_CPC`, then the processor's register, then AMD's ranking, else equal), never from core types or names, and leave Thread Director's classes for later (S9)? | **Accept** | Accepted |
| 10 | 327 | Adopt the cache-hot rule, move rates and the NUMA rule, with H, periods and thresholds as hypotheses (S10)? | **Accept** | Accepted |
| 11 | 328 | Hard and soft affinity as domains or interned sets, never per-thread bitmaps, with `CpuIndex` never exposed and the SDK's form left to ADR-0011 (S11)? | **Accept** | Accepted |
| 12 | 329 | Three classes (Interactive, Normal, Background) on one proportional-share queue, set only by trusted signals with a ceiling from the launcher, and no heuristics (S12)? Or strict priority between classes? | **Accept three classes on one queue** | Accepted |
| 13 | 330 | Levels within a class, mapped to weights by a table that is not ABI (S13)? | **Accept** | Accepted |
| 14 | 331 | Account CPU time from the TSC at every switch, never across processors, readable under rights (S14)? | **Accept** | Accepted |
| 15 | 332 | Give each process a CPU share divided among its threads, and an optional cap, both set by its creator (S15)? Or keep per-thread fairness only? | **Accept share and cap** | Accepted |
| 16 | 333 | Lend weight and class to the owner of an owned wait, transitively to depth 8, leaving lending across service calls to ADR-0005 (S16)? | **Accept** | Accepted |
| 17 | 334 | Build the power and heat mechanisms in M3 with "spread" as the default, leaving the policy to power management (S17)? | **Accept** | Accepted |
| 18 | 335 | Prefer idle cores over idle siblings, keep ADR-0017 Z5 as it is, and leave core scheduling as a Server option (S18)? | **Accept** | Accepted |
| 19 | 336 | Keep stride as the baseline and evaluate EEVDF afterwards by paired kernels, adopting it only by amendment (S19)? | **Accept** | Accepted |
| 20 | 337 | Pinned mode as the safe fallback: entered when the delivery proof fails or a recoverable inconsistency is met; under `cargo xtask` the delivery proof a required marker on every machine and the run-time entry line a failure marker in every scenario, so the fallback never hides a failure from a proof; in production a persistent fault record and the system continues pinned; no boot option at M3 (S20)? | **Accept** | Accepted |
| 21 | 338 | Put processors going offline and back in ADR-0008 Amendment 2, written after the move task lands, with only evacuation from this ADR, and M3-1 partial until that amendment is built (S21)? Or design it here now? | **Accept the amendment later** | Accepted |
| 22 | 339 | Record S22's budgets as hypotheses (S22)? | **Accept** | Accepted |
| 23 | 340 | Adopt S23's proofs and classes, the diagnostic round on the 65- and 67-processor machines (balancing on and off, idle and beside load) before any task that turns balancing on or changes what a proof accepts sets a proof, a bound or a reported row, and an SMT machine and a two-package machine in `boot-matrix` under ADR-0008 Amendment 1 D7's CI condition (S23)? | **Accept** | Accepted |
| 24 | 341 | Taken alongside: amend ADR-0005 so that a client waiting on a call lends its class and weight to the server thread that read its message, until the reply, the client's stop or its deadline, built after the M3 core (ADR-0005 Amendment 11, J1 to J6)? | **Accept** | Accepted |

## Amendment 1 (2026-11-23): an idle processor asks a busy one to publish a waiting frame

*Accepted by the owner on 2026-11-23 ("Allow it"; decision sheet item 355). Proposed by TASKS item 98's contract (its question Q2), which found the case below on `27699a39`. TASKS item 98 builds it. Until that lands, nothing here is built.*

**The finding.** S5 publishes a switched-out frame at its processor's next call into the table, and says the thread "is therefore movable from its source's next trap entry after it was switched out: within one tick, sooner on any interrupt". That does not hold when a processor runs exactly two threads that never yield:
- each tick's switch publishes the waiting thread's frame and takes it at once, and the thread switched out becomes the leaving one;
- so between two ticks the only waiting thread is always in the leaving record, `lend` refuses it, and an idle processor can never take it;
- S7's work conservation then fails in its plainest case: two threads on one processor while another idles.

With three or more threads, all but the last one switched out can be lent. The evidence is `Contexts::switch` and `lend` in `framekernel/frame/sync/src/contexts.rs`, and a host test of TASKS item 98: a mover looking between 64 switches was refused 64 times, and after one call into the table that switches nothing, the frame was lent.

**Change to S7: a fourth message, the ask.**

1. An idle processor whose pull (S7, path 1) finds a thread it could otherwise take, but whose frame its source has not published, may send that source one reschedule IPI. It then idles.
2. The source's reschedule upcall runs as any other. In it the source publishes its leaving frame, as S5's "next call into the table" does, and then kicks the nearest idle processor, as S7's path 2 does.
3. The kicked processor's idle flow runs path 1 and pulls the published frame.

**Its bounds.**
- A processor sends at most one balancing IPI a tick, kicks and asks together. This is S7's "at most one kick per tick per processor".
- A processor takes at most one ask a tick, however many processors are idle. An asker sends only if it is the first to mark the source as asked, and the source clears the mark when it publishes, or at its tick.
- An ask is sent only for a thread that every other check of the pull would take: declared for balancing, ready, not running, not pinned, within S10's rate and cache-hot rule, with room at the destination.

**What does not change.**
- Every move is still a pull: the processor that will run the thread is the one that moves it. A busy processor never moves a thread.
- S5 stands: a frame is published only by its own processor, after it has left the stack. The ask adds no publication point, because a reschedule upcall is a scheduler upcall, which already reaches the table (S5's note of 2026-11-21).
- The switch path, the trap entry's assembly and the tick's step are unchanged.
- Every step waits on an event, and none on a time.

**Where it differs from accepted text:**
- **S7's decision** lists three paths, with busy processors only kicking. The ask is a message from an idle processor to a busy one, which S7 did not have.
- **S7's cost row** says the idle pull "costs only idle processors", and its evidence says balancing started by idle processors "costs the busy ones nothing". With the ask, a busy processor takes one interrupt and sends one IPI, at most once a tick, and only while an idle processor waits for one of its threads.
- **S5's sentence** quoted above is narrowed: with exactly two alternating threads, the waiting one becomes movable only after an ask, or after its processor publishes for a kick of its own.

**Alternatives.**

| | Why not |
|---|---|
| S5's alternative (a): publish from the trap stub, right after the new stack pointer is loaded | Every switch pays one or two stores, in the framework's assembly and outside Rust's memory model, so Loom cannot check it. S5 names it as the next step only if B21 is missed because of this delay |
| No ask | A processor with exactly two runnable threads is never relieved while another idles: in TASKS item 98, and in item 99, whose periodic pull needs the same frame |
| The source holds the waiting thread back at its tick until an idle processor has taken it | No interrupt on the busy processor, but it changes the tick's step, delays the pull by up to a tick, and adds a state that step must honour |

**Performance cost (ADR-0018 V3).** Unmeasured, owed (CL62): on a busy processor, one reschedule interrupt taken, one publication (two plain stores on lines the processor owns) and one IPI sent, at most once a tick, and only while an idle processor waits for one of its threads; on the idle processor, one IPI sent. Nothing on the switch path. TASKS item 98 counts the asks at boot and prints them.

**Threat model (§58).**
- An ask is the reschedule vector: it names no thread and carries no data. No program can send one; only a processor's idle flow does.
- A program that keeps exactly two threads runnable on a processor causes at most one ask a tick there, and what it gains is a processor that would otherwise idle.
- Many idle processors cannot flood one busy processor: it takes at most one ask a tick.
- S20 stands: with moves withdrawn, no ask is sent.
