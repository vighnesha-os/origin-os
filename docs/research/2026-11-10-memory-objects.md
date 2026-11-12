# Research: memory objects (2026-11-10)

Backing notes for task S3-5b (CLAUDE.md §16): `memory_object_create`, `memory_map`, `memory_unmap` and `memory_protect`, sharing by passing the handle, random placement and quotas, under the owner's decisions C6, C7 and C8 (ADR-0005 Amendment 4, accepted 2026-11-09), ADR-0005 P2, P3, P4, P7 and P11, and ADR-0004 H2 and its Amendment 1. It also covers what S3-5a's threat model left to this task: T15, T16 and T18 of `docs/engineering/THREAT_MODEL_ADDRESS_SPACES.md` and its "What must be closed" item 1, and O2 of `docs/engineering/THREAT_MODEL_SYSCALLS.md`. What was read, what it means for Origin, the tests defined before the code, and what stays unknown. These are conceptual references only: **no source code of any of these systems was read** (CLAUDE.md §3). The scoping of the whole step is `docs/research/2026-11-09-n14-step3-scope.md`.

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, `zx_vmo_create`, `fuchsia.dev/reference/syscalls/vmo_create` | No date on the page | Creation options and a new object's default rights | Read through WebFetch, 2026-11-10 |
| Fuchsia, `zx_vmar_map`, `fuchsia.dev/reference/syscalls/vmar_map` | No date on the page | Placement at a chosen address with and without replacing, random placement, which rights a mapping needs, the errors | Read through WebFetch, 2026-11-10 |
| Fuchsia, `zx_vmar_protect`, `fuchsia.dev/reference/syscalls/vmar_protect` | No date on the page | What a change of permissions may add, the range it covers, gaps | Read through WebFetch, 2026-11-10 |
| Linux man-pages, `mmap(2)`, `man7.org/linux/man-pages/man2/mmap.2.html` | 6.19, 2026-03-19 | `MAP_FIXED` against `MAP_FIXED_NOREPLACE`, `munmap` of an empty range, the limit on mappings | Read through WebFetch, 2026-11-10: the documentation only, for the named reason that it records both answers to C7's fixed-address question (memory: vendor documents first, other systems' documentation for a named gap) |
| Linux kernel documentation, `sysctl/vm`, `max_map_count`, `docs.kernel.org/admin-guide/sysctl/vm.html` | Site 7.4.0-rc2; the entry says it is valid for 2.6.29 | A per-process limit on mappings and its default | Read through WebFetch, 2026-11-10 |
| N. Amit, A. Tai, M. Wei, "Don't shoot down TLB shootdowns!", EuroSys 2020 | 2020 | Why a shootdown costs, and the risk of batching | The abstract and citation only, through a web search, 2026-11-10; the paper's measurements were not read |

## What they show

**A chosen address never replaces, in the design C7 follows.** Zircon's `ZX_VM_SPECIFIC` "does not replace existing mappings", and an overlap is `ZX_ERR_ALREADY_EXISTS`; replacing takes a separate flag, `ZX_VM_SPECIFIC_OVERWRITE`. Linux's `MAP_FIXED` discards what it overlaps, and the page warns that it "is hazardous because it forcibly removes preexisting mappings"; `MAP_FIXED_NOREPLACE` (Linux 4.17) refuses a collision with `EEXIST`. Without a chosen address Zircon places the mapping at random, by the region's policy.

**Each permission needs its right on the object, and write and execute need read.** Zircon's `zx_vmar_map` asks `ZX_RIGHT_READ`, `ZX_RIGHT_WRITE` or `ZX_RIGHT_EXECUTE` on the object's handle for each permission, and refuses write or execute without read. A new object's default rights have no execute (`zx_vmo_create`), as C6's have none for an anonymous object.

**A change of permissions may not add one.** `zx_vmar_protect` refuses an increase in a sub-region with `ZX_ERR_ACCESS_DENIED`, fails on a gap with `ZX_ERR_NOT_FOUND`, and on failure leaves every permission as it was. It works on byte ranges; C7's v1 works on whole mappings only, and P7's `protect` may only reduce.

**Mappings are counted.** Linux caps a process's mappings (`max_map_count`, 65530 by default) and reports `ENOMEM` past it; the page notes that "most applications need less than a thousand maps". Zircon's job policy can forbid creating objects at all (`ZX_POL_NEW_VMO`).

**Creation takes options from the start.** `zx_vmo_create` takes `options`, 0 in the simplest case.

**A shootdown costs because TLBs are not kept coherent by the hardware,** so software sends interprocessor interrupts; Amit, Tai and Wei argue that batching them to save that cost can break correctness. S3-5a's shootdowns already reach only the processors with the space loaded and report each change's cost (`Change`), which is what a limit needs.

## What this means for Origin

1. **The interface** (`origin.kernel` 0.5, `user-abi` 0.5): four calls, numbers 11 to 14; `map_access` bits (`read`, `write`, `execute`); `map_flags` bits (`fixed`); a sized `map_request` (`access`, `flags`, `offset`, `length`, `address`), as C8 says; C6's rights for a memory object as a constant. Every shape C6–C8 do not fix went to the owner (CLAUDE.md §84), who accepted the five items below on 2026-11-10: ADR-0005 Amendment 5, D1–D5.
2. **Rights.** A new object's handle has C6's rights: read, write, map, duplicate and transfer. Every map, unmap and protect names a process and needs `write` on that handle, the caller's own included (C6). A map needs `map` on the object, `read` for any mapping and `write` for a writable one. `execute` is refused with `access_denied`, since an anonymous object never has it; write and execute together, or a mapping without read, with `invalid_args`.
3. **Placement.** Without `fixed`, at a random page of the user half's upper part from RDSEED or RDRAND, as `launch::place` does, clear of every mapping; without entropy the map fails with `not_supported`. With `fixed`, at a page-aligned address in the user half, refused with `invalid_args` if any page of the range is mapped: it never replaces.
4. **Whole mappings.** Unmap and protect take an address and a length and act only on exactly one whole mapping; anything else is `invalid_args`. A later minor can allow parts without a new method. Protect only reduces: an increase is `access_denied`.
5. **Commitment and quotas.** An object's frames are allocated, zeroed, at creation and charged to its creator's account: `limit` past the quota, `no_memory` without frames. The charge goes back with the object's last reference, and a mapping holds one, so mapping an object and closing its handle does not escape the quota. Each process may hold as many mappings as its creator allows (`limit` past it), which also bounds T18's bookkeeping. `MAX_TABLES`, 32 page-table frames per space, stays: a random placement can take three tables, so a process maps about seven objects at random. Raising it, or placing mappings so they share tables, is a framework change and T18's measurement first.
6. **A hostile loop (T15, T16).** Each process has a budget of shootdowns and interrupts, refilled with time at a rate its creator sets. An unmap or protect is priced before it runs, from the pages it covers and `processors_loaded`, and refused with `should_wait` if the budget cannot pay: nothing changes, and no shootdown turn is taken. Afterwards the `Change` it reports is what is charged. `should_wait` says the refusal is for now; `limit` stays for the quotas, which hold until something is freed.
7. **A stalled shootdown** makes the change and leaks the frames (S3-5a). The call reports `timed_out`, so the caller learns of it (T3).
8. **Sharing** only by passing the handle, whose rights only shrink (P2). A mapping keeps its object, as Zircon's does, so closing a handle never unmaps.
9. **Concurrency.** The new state is a process's list of mappings and its budget, under one spin lock, and the account's committed bytes, with the account's existing `try_update`. No new lock-free code, so no new Loom model; an object's life is `ObjectRef`'s, already modelled.

## Shapes for the owner (asked and accepted 2026-11-10)

C6–C8 leave these shapes open. They went to the owner as one list, each with its recommendation and the alternatives rejected, and the owner accepted all five as recommended ("Accept all", 2026-11-10): ADR-0005 Amendment 5, D1–D5, decision sheet items 101–105. No system-call code or lock change was committed before the answers.

1. **Creation calls take their arguments in a sized request,** so a later minor can add an option by P4's own mechanism: `memory_object_create(request: object_request)`, `object_request { length }`. OIDL refuses an empty `bits` and an empty struct, so S3-6's `event_create` and `port_create`, which take nothing yet, keep no parameters until they have one; the rule binds from major 1. Rejected: an options `u32` that must be 0; a `bits` with a member v1 refuses; the length alone.
2. **`map_request`** is `{access, flags, offset, length, address}`, 40 bytes; bytes in whole pages, never rounded; `map_access` (`read`, `write`, `execute`; read required, never write with execute, execute refused) and `map_flags` (`fixed`). Rejected: access as `rights`; read implied; placement as an enum; page counts; rounding up.
3. **`memory_unmap(process, address, length)` and `memory_protect(process, address, length, access)`,** exactly one whole mapping in v1, the length there so a later minor can allow part of one. Rejected: naming a mapping by its base alone; a region object (none in P3); `not_found` for no mapping.
4. **Quotas and the budget** come from the creator's `Limits`: committed bytes (held by handles and mappings), whole mappings, and a rate for unmap and protect. `limit` past a quota, `no_memory` without frames, `should_wait` for a change the budget cannot pay now. Rejected: charging the mapping process; counting pages; `no_memory` for a quota; `limit` for the budget; blocking until it refills.
5. **The rest:** `write` on the caller's own process handle too; `invalid_args` for a fixed address over a mapping, an ended process and any range that is not one mapping; `access_denied` for execute; `timed_out` for a stalled shootdown; the order of the checks; page alignment within `[0x1000, 0x7FFF_FFFF_F000)`; random placement in `[0x1000_0000_0000, 0x7000_0000_0000)`; a mapping holding its object; the constants `memory_object_rights` and `page_size`; `user-abi` 0.4 to 0.5 in one step.

## Tests defined before the code (§16 step 12)

- **Host:** an object's charge and its return with the last reference; `map_request` decoding (each size, each undeclared bit, write with execute, no read, misalignment, a zero length, an offset and length past the object, overflow, addresses outside the user half, an address without `fixed`); the quota and the mapping limit to the unit; the budget's arithmetic; whole-mapping matching; placement that steps over a mapping.
- **Boot, Refuse:** two processes share an object by passing its handle and each sees the other's write; protect to read-only makes a write fault at the mapping; unmap makes a read fault there; a fixed address over a mapping is refused; execute is refused; the quota refuses past its bound; mapping into another process needs `write` on its process handle; two random placements differ; a hostile unmap loop is refused with `should_wait` before its budget is overrun and is served again once it refills; a leak check.
- **Hostile calls** added to the IPC proof's corpus: bad and stale handles, the wrong types, too few rights, each malformed request, user pointers into the kernel.
- **Negative controls:** one for each boot line.
- **Costs** (QEMU, no budget): map, unmap and protect, and the shootdown when a shared object is unmapped.

## Unknown

- **Demand commit** (C7's later step, with P10's user page-fault path), **sealed objects** (the Package Service) and **executable mappings** (the Code Integrity Table, ONBF §8.2).
- **Unmapping or protecting part of a mapping.**
- **Raising `MAX_TABLES`** or placing mappings to share tables (T18): measure first.
- **The budget's defaults:** the creator sets them; what the Loader Service gives a program is step 4's.
- **PCIDs** and **device DMA to shared frames**, items 4 and 5 of what must be closed in the address-space threat model.
- **T16's per-initiator requests,** which would remove the shared turn rather than bound it.
