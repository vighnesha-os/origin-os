# Review of H2-stacks and the framework's fixes for S3-5b, 2026-11-10

| | |
|---|---|
| Scope | Branch `claude/s3-5b-frame-fixes` at `e53e2fe`, merged into the step-3 batch (`land/s3-part2`) as `759d319`. It carries (1) task H2-stacks, ADR-0004 Amendment 1 A2: 18 commits from `3223aa6`, code ending at `c093df9`; (2) its merge with S3-5b (`3ca0811`); (3) the framework's fixes for findings A, B and C of `REVIEW_S3-5B_2026-11-10.md`: `12f32c5`, `2cca177`, `058203d`; (4) records (`e53e2fe`) |
| Why | Each task is reviewed before it joins the step-3 batch (CLAUDE.md §78, §83). The stack region and the address-space changes are framework code with unsafe boundaries (§13, §58) |
| Who reviewed | The coordinating AI session, by reading, with the author's reports in hand. This is not an independent review. **No human has reviewed this code** |
| Read in full | `framekernel/frame/src/stack.rs`; `framekernel/frame/sync/src/slots.rs` outside its tests; the three fix commits |
| Read as changes | `vm.rs`, `layout.rs`, `memory.rs`, `tlb.rs`, `cpu.rs`, `smp.rs`, `early.rs` of the framework; `origin-paging`'s `reserve_tables` and `leaf_sizes`; `sched.rs`; the conflict resolutions of `3ca0811` in the kernel |
| Not read | The boot proof `framekernel/src/stacks.rs`, xtask's marker changes, and the Loom models' text. They are covered here only by the gates below |
| Run by the reviewer on the merged tree `759d319` | `cargo xtask check` passed: 1,234 host tests in 43 suites, 0 failed, 8 ignored; all 42 Loom models passed and all 68 controls were caught. `cargo xtask boot-test` passed 11 of 11 scenarios, and its kernel boot passed 58 of 58 proofs (QEMU's software CPU), `kernel stacks` and `memory objects` among them. The merge had no conflicts |
| Reported by the author, not repeated | `cargo xtask boot-matrix` (18 boots, each 58 of 58) on a tree identical to `058203d`; the boot controls C26–C29; the costs before and after; that each commit builds and passes on its own |
| What this is not | Evidence from a real processor. Physical-hardware qualification (CLAUDE.md §74). A proof beyond the Loom models' bounds. Evidence that a running stack's overflow ends in the #DF report, or of a stalled reclaim outside host tests: the author lists both as not proven |

## H2-stacks: checked and found sound

- **The region.** `0xFFFF_C000_0000_0000`, 2048 slots of 128 KiB. A stack is at most 64 KiB and sits at the top of its slot, so at least 64 KiB below it is never mapped. All 129 page tables are made at boot (`reserve_tables`), and the boot's table pool is sized for them, so mapping a stack never needs a table.
- **A slot's life** (`origin_sync::slots`). Every change of state is made under one lock, and nothing else is done under it. A slot is taken by one holder at a time. A retired slot becomes free, and its frames are released, only after the shootdown of its pages completes; on a stall the frames are forgotten and the slot is lost for good. A slot being reclaimed cannot be taken or found by another reclaim.
- **No wait where none is allowed.** Making and giving back a stack wait for no processor. Only `stack::reclaim` does, and the scheduler calls it when it reaps, with no lock held.
- **A stack in use is never given back.** Dropping a stack a processor still runs on loses it instead.
- **Processors' stacks.** Each application processor's own stack and its four interrupt stacks are in the region and kept for ever, also when the processor never answers, as its heap stack was before. The page below each interrupt stack is proven to fault before the processor starts. The boot processor's interrupt stacks stay in the kernel image; their guards are image pages.
- **The direct map.** `vm::make_guards` refuses any page outside the kernel image, and `make_guard` and `unguard` are gone, so nothing cuts a page out of the direct map after boot. The proof counts the direct map's page sizes to show it.
- **The backtrace** stops at the top of the stack's slot, so it never reads the next slot's unmapped space.
- **Unsafe code.** The one new site (a volatile read of a processor's TSS words, `cpu::interrupt_stack_tops`) states its invariants: the words are written once before `ltr`, the image lives for ever, and no reference is made.

## The fixes for S3-5b: checked and found sound

- **A (`12f32c5`).** After `Err(Stalled)` from `UserSpace::unmap` or `protect`, every page of the range has its final entry. An earlier stall is settled first by a flush of the whole space. Once anything stalls, the remaining rounds are made with no shootdown each, and one flush of the whole space ends the change. Frames are given back only if that flush completes; otherwise they stay leaked and the space stays unconfirmed. A change waits for a stalled processor at most twice. A round the tables refuse takes precedence over the stall in the error returned.
- **B (`2cca177`).** `UserSpace::map_shared_run` refuses a run over any mapped page, makes every table and counts every frame's alias mode before it writes the first entry. So a refusal leaves no entry that was ever present and needs no shootdown. Tables made for a refused run stay, empty, and count against the space's bound.
- **C (`058203d`).** The bound is checked before a frame is asked for. A space at its table bound returns `ChangeError::Limit`; no free frame returns `MapError::OutOfMemory`.

## Observations

| # | Severity | Observation | Status |
|---|---|---|---|
| 1 | Low (records) | `slots::Counts::pages` is documented as the pages of Live, Idle and Retired slots, but a slot lost through `leak` keeps its pages in the count, while a slot lost through a stalled reclaim does not | Sent to the framework's author: make the two agree or say so in the comment |
| 2 | Low | With a stall, what the change cost is not returned, so the kernel charges only the price paid beforehand. As before the fix | Recorded; bounded by the price, which already assumes a shootdown |
| 3 | Open on the kernel's side | `memory_object.rs` still maps page by page through `map_shared`. Until it uses `map_shared_run`, finding B's unpriced shootdown is still reachable, the table bound returns `invalid_args`, and no free frame returns `limit`, where item 142 says `limit` and `no_memory` | Sent to the kernel's author |
| 4 | For the kernel's author to confirm | Whether the framework's own records cover every mapped page (the program's own and its start record's), so that the kernel's `translate` check before a fixed mapping can go | Sent to the kernel's author |

## Resolution

Landed on the step-3 batch's local branch as `759d319`. ADR-0004's wording now says the stack region is built (decision sheet item 143; five edits, applied with this record). The slide for the region (ADR-0004 Amendment 2, item 144) is accepted and not built. The stacks have run under QEMU's software CPU only.

## Addendum, 2026-11-11: the stack region's slide

**Scope.** Branch `claude/stack-slide` at `5abb120` (9 commits from `cdf4ed9`), merged into `land/s3-part2` as `685e34c`: ADR-0004 Amendment 2, A4–A6 with notes A6.1–A6.4 (decision sheet items 144 and 157–160), and the fix for observation 1 above (`40a013b`: the comment on `Counts::pages` now says what it counts, with a host test). Reviewed by the coordinating AI session by reading, with the author's report in hand. **No human has reviewed this code.**

**Read as changes:** `layout.rs`, `stack.rs`, `early.rs`, `told.rs`, `phys.rs`, `vm.rs`, `slots.rs`, and the kernel's `console.rs`, `kernel.rs`, `sched.rs` and `stacks.rs`; xtask's scan of the serial log. **Not read:** xtask's read through QEMU's monitor (`9f882e8`) and the records commit; the gates cover the first only where it runs.

**Run by the reviewer on the merged tree `685e34c`:** `cargo xtask check` passed (all 42 Loom models, all 68 controls caught); `cargo xtask boot-test` passed 11 of 11 scenarios, with the kernel boot saying its region's base was drawn from RDSEED and is not the fixed one (QEMU's software CPU). **Reported by the author, not repeated:** `cargo xtask boot-matrix` (18 boots, 58 of 58 each, 11 distinct bases read through the monitor, none printed), controls C30–C34 and H1–H3, and the forced boot without entropy.

**Checked and found sound.**
- The draw: `1 + random % placements` steps of 2 MiB from the window's start, so every base is aligned, keeps the whole region in PML4 slots 384–447, and is never the fixed base; the extremes are tested.
- The tables: 130 are counted and the early pool is sized from that count; the region is recorded (`place`) only after its tables are made, once.
- Every user of the region's bounds (`slot_of`, `slot_top`, `in_stack_region`, the proof) reads the placed base, with the range check written so it cannot overflow.
- Printing: `Region`'s and `Bases`' `Debug` leave the bases out; the exception report, the panic backtrace and the NMI line go through `phys::told`, which reads only atomics; every boot test now fails if its log holds a number in the stack window.

**Observations.**
1. A6.2 was built wider than its words: direct-map addresses and the exception report's general registers print as offsets too. Kept, and written into note A6.2.
2. Low. A register that holds a non-address value that happens to fall in a sliding window prints as an offset. Nothing is lost, since the base is known offline to nobody; a report's reader must know it.
3. Not covered: the monitor read on CI's QEMU (nothing is pushed), a boot without entropy that gets past the start-record proof, and any real processor.

