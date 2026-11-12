# The kernel stacks' region: what moves, and how (ADR-0004 Amendment 1, A2)

| | |
|---|---|
| Date | 2026-11-10 |
| Task | H2-stacks, ADR-0004 Amendment 1 A2 (owner's decision sheet item 99); CLAUDE.md §13, §14, §16, §25, §28 |
| Sources | Intel SDM Vol. 3A §4.10.4.3 (no TLB entry is made from an entry whose P flag is 0, so a page that becomes present needs no invalidation) and §6.14.5 (the Interrupt Stack Table), as the code already cites them; they were not re-read for this note, and the AMD64 manual was not read. Rust 1.99.1's code generation for `x86_64-unknown-none`, checked locally (below). No other operating system's source or documentation was consulted (CLAUDE.md §3; owner items 91–97). |
| Code | `framekernel/frame/sync/src/slots.rs`, `framekernel/frame/src/{stack,vm,tlb,cpu,smp,early,memory}.rs`, `framekernel/paging/src/{lib,layout}.rs`, `framekernel/src/{stacks,sched}.rs` |

## The question

ADR-0004 H2 puts the kernel stacks at `0xFFFF_C000_0000_0000`, each with an unmapped guard page below it in virtual space. Until this task every guard was a page of the direct map, unmapped. That cost a frame, cut a hole in the direct map (splitting a 2 MiB page into 512 4 KiB ones when the hole fell inside one), and made every new stack wait for a TLB shootdown on every processor. The coordinator asked first which stacks are in scope, thread stacks aside: the interrupt stacks and the per-processor boot stacks.

## Which stacks move

| Stack | Until now | Decision | Why |
|---|---|---|---|
| Kernel thread stacks (`sched`, 16 KiB) | Heap memory, guard cut out of the direct map | **Move** | Threads come and go: this is where the cost and the holes were |
| Each application processor's own stack (64 KiB) and the park drill's (4 KiB) | `GuardedStack`, as above | **Move** | The same type; kept for good |
| Each application processor's four interrupt stacks (#DF, NMI, #MC, #DB; 16 KiB each) | In its heap `CpuTables`, each guard cut out of the direct map | **Move** | They were the remaining guards in the direct map. `smp::start_one` makes them before the processor starts, when the region and the heap exist; `init_ap` writes their tops into the processor's TSS |
| The boot processor's four interrupt stacks | In the kernel image (`BSP_STACKS`), guards unmapped in the image once the kernel's tables are live | **Stay** | They must exist at the first boot step (`cpu::init`), before the kernel's tables, the heap or the region. Their guards are image pages, not the direct map, so they split nothing there. Moving them later would mean rewriting a live processor's TSS under possible NMIs, for no gain |
| The boot stack (the loader's `KernelStack` region) | Direct map; its guard page is left out of the map by `layout::build` | **Stays** | The loader makes it and hands it over (ADR-0004 H4), and the boot processor runs on it for good. Its guard is a page the direct map never maps, inside a region that is 4 KiB leaves anyway: nothing is split |
| The TLB proof's three pages (`GuardablePage`) | Heap pages, cut out of the direct map and mapped back | **Move** | Not stacks, but the only other code that cut pages out of the direct map. In the region, nothing splits a direct-map page after boot, which the boot proof can then check exactly |

So every guard in the direct map is gone, and `vm::make_guards` now refuses anything below the kernel image's window.

## How the region works

- **Slots.** 2048 slots of 128 KiB (32 for each processor the kernel indexes), 256 MiB from `0xFFFF_C000_0000_0000`. A stack's pages sit at the top of its slot; everything below, at least 64 KiB since no stack is larger than half a slot, is never mapped. The next slot's bottom is unmapped too, so the space above a stack's top faults as well.
- **Tables made at boot.** `build_kernel_tables` makes the region's page directory and its 128 page tables right after the kernel half's top-level entries (`AddressSpace::reserve_tables`), and the pool is sized for them: 129 frames, 516 KiB. Mapping a stack therefore never needs a table, cannot fail for want of one, and never takes one that device registers or firmware tables need.
- **Mapping is free of shootdowns.** A page that was not present is in no TLB, so making a stack writes entries and waits for nobody.
- **Giving back.** Dropping a stack waits for nobody either, so it may happen under any lock: a refused spawn drops its stack under the run queue's lock. Up to 16 freed stacks stay mapped for the next stack of their size, which reuses the same frames at the same addresses: no translation changes, so no shootdown is needed, and the stack is zeroed again. The rest are retired.
- **Reclaim.** A retired stack's pages leave the tables, a kernel shootdown of exactly those pages reaches every processor, and only then are its frames and its slot free (`origin_sync::slots::Slots::reclaim`). If the shootdown stalls, the slot and the frames are lost for good. `sched::reap` reclaims, with no lock held, so each processor's idle loop does it too.
- **Stacks still in use** are leaked rather than given back, as before (`cpu::stack_in_use`).

### Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the guards in the direct map | What A2 replaces: a frame and a hole per guard, splits, and a shootdown per new stack |
| Addresses handed out once and never reused | Needs no shootdown for the addresses, but page tables are never freed and the pool is fixed, so the tables would run out |
| Runs of pages of any length, first fit | Fragments, and is more code, for three stack sizes that all fit one slot size |
| Page tables made when first needed, from the shared pool | A burst of stacks would take tables for good that device registers and firmware tables need; made at boot, the region's cost is fixed and visible (403 tables where there were 274) |
| No cache of freed stacks | Every thread's end would cost a shootdown of every processor; with the cache, a thread's whole life costs none |

## What the stack's guard relies on

- **Stack probes.** A frame larger than the space below a stack could step over it. Rust 1.99.1 emits inline stack probes for `x86_64-unknown-none`: compiling a function with a 20,000-byte frame for that target (`rustc --target x86_64-unknown-none -O --emit asm`) gives `subq $4096, %rsp; movq $0, (%rsp)` four times and then the remaining 3,632 bytes, so every page is touched in order and the first page past a stack's end is the first one written. Here the space below is at least 64 KiB anyway.
- **An overflow of a running stack ends the kernel, reported.** The #PF that an overflow raises has no IST, so its frame cannot be pushed onto the stack that overflowed: the processor raises #DF, which runs on its own interrupt stack and reports. A boot cannot show this and go on. The boot proof shows instead that the page below each stack, the bottom of its slot and the bottom of the next slot do not translate.
- **The backtrace.** `cpu::capture_backtrace` read up to 256 KiB above the frame pointer, safe while what lay above a stack was mapped direct-map memory. In the region, a stack's slot ends at its top, so the window now stops there.

## Risks and what is left

- The direct map still maps every stack's frames, as it maps every frame the kernel owns: an access through that alias is not caught. Only accesses through the stack's own addresses fault.
- More than 2048 stacks at once are refused like a full run queue (`QueueError::Full`).
- A stalled reclaim leaks the slot and its frames; no boot can stall a processor on purpose, so this is tested on the host only.
- Under QEMU's software CPU, zeroing a reused stack through its own 4 KiB mappings is slower than zeroing fresh frames through the direct map, with the same `memset`. That is an emulator effect; physical hardware will give the real figures.
- **Not built, and a question for the coordinator:** the region has no slide of its own. Stack addresses are predictable, since the lowest free slot is taken first. The direct map and the kernel image are slid (ADR-0004 H3).
