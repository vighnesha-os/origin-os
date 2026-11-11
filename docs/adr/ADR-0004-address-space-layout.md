# ADR-0004: Virtual address-space layout, higher-half kernel and KASLR

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-07, as proposed (H1–H5, and the recommended answer to each of the four questions below). **H1–H4 implemented the same day** (boot info 0.3; loader-built tables; KASLR from RDSEED/RDRAND; the kernel in the higher half with its own tables). H5's global kernel pages, with cross-processor TLB shootdown, were implemented later the same day; PCIDs are not done yet. H2's last clause, that the kernel-half entries point at tables allocated once at boot, was not honoured until N14 step 3, task S3-5a (2026-11-10): each process used to copy whatever entries existed when it was created. Now every entry 256–511 gets a table before the kernel's tables go live, and a boot proof holds a mapping made in a fresh slot after a process exists to being visible from that process. **Amendment 1, accepted by the owner on 2026-11-10** (decision sheet items 98–100): the kernel heap stays in the direct map, the stacks move to their own region (not yet done), and PML4 slot 480 holds the kernel-half proof's probe page. |
| Supersedes | ADR-0003 K2's identity mapping, once implemented |
| Related | ADR-0002 B3 (KASLR deferred to here), B4 (boot info), ADR-0003 K2, K8 |

## Context

Today the kernel runs identity-mapped (virtual = physical) in its own tables (ADR-0003 K2). That was the smallest correct step, but it occupies the lower half, which user processes need, and it gives no address-space randomization. M4 needs one address space per process with a shared kernel half.

## Proposed decisions

### H1. 4-level paging; the lower half belongs to user space
User addresses are `0x0000_0000_0000_1000` to `0x0000_7FFF_FFFF_E000` (page 0 and the top page stay unmapped), 47 bits (128 TiB). 5-level paging (LA57) is detected and deferred: when enabled it widens user space, not the ABI's lower bound.
*Alternative:* a smaller user range (e.g. 46 bits) to leave room for guard regions — not needed with an unmapped top page.

### H2. Kernel half layout (PML4 slots 256–511)
| Range | Use |
|---|---|
| `0xFFFF_8000_0000_0000` + slide | direct map of all RAM, read/write, never executable (1 GiB or 2 MiB pages; holes as in K2). The kernel heap is in it (Amendment 1, A1) |
| `0xFFFF_C000_0000_0000` … | kernel stacks (per-thread stacks with guard pages in virtual space, not in the direct map). Until Amendment 1's A2 is built they are still in the direct map; the heap is there for good (A1) |
| `0xFFFF_E000_0000_0000` … | MMIO windows, mapped uncached on demand by drivers. Since ADR-0010: the first 2 GiB hold the firmware framebuffer, mapped by the loader; drivers' MMIO starts above them |
| `0xFFFF_F000_0000_0000` (PML4 slot 480) | one page and nothing else: the kernel-half proof's probe page (Amendment 1, A3) |
| `0xFFFF_FFFF_8000_0000` + slide | the kernel image, with its segment permissions from the signed ONBF table |

Each address space has the same PML4 entries 256–511, pointing at kernel tables allocated once at boot, so kernel mappings never need to be copied or synchronized between address spaces.

### H3. KASLR
The loader chooses the slides: the kernel image at 2 MiB granularity within its 2 GiB window, the direct map at 1 GiB granularity. Entropy: RDSEED, else RDRAND, else `EFI_RNG_PROTOCOL`; with none, boot proceeds without a slide and says so. The kernel never prints its base after boot except to the serial debug console in developer mode.

*(Recorded 2026-11-10, from the design review's findings 8 and 14; decision sheet items 113 and 119. Two clauses of H3 are not what the code does. The loader has no `EFI_RNG_PROTOCOL` fallback, and it prints both slides in every build (`boot/uefi/src/main.rs`); the pre-merge review listed both as low. The firmware's generator becomes one seed of the kernel's generator (item 113). Addresses leave release logs before the Diagnostics interface serves the kernel log (item 119). Until both are built, H3 states the intent, not the build.)*

*(Amended 2026-11-10 by ADR-0016, R3 and R10, accepted by the owner the same day; decision sheet items 147 and 148. **Built on 2026-11-11 by task 18b**, except that the loader does not yet ask the firmware for bytes at all: that call is left to another session (ADR-0016, "What was built"). Three things change in H3. The loader takes its slides from a random-number generator of its own, seeded from RDSEED and RDRAND through health tests, and no longer from one draw of an instruction. The two slides are two requests of that generator, where today both come from one value. And `EFI_RNG_PROTOCOL` is no longer a source the loader falls back on alone: the firmware's bytes are mixed into the seed when the firmware gives any, and with no healthy instruction there is no generator and no slide, which is H3's own last case. Until task 18b builds this, the loader does what the note above says.)*

### H4. The loader builds the first page tables
The loader already holds the verified segment table. It builds tables mapping the kernel at its slid virtual base, the direct map and the boot info, loads CR3 just before the jump, and passes the table root in boot info **0.3** (all boot-info addresses become virtual). The kernel's entry runs at its virtual address from the first instruction and later rebuilds its tables with its own allocator.
*Alternative:* the kernel relocates itself from identity mapping (keeps the loader simpler, but the kernel's first instructions then run at a different address from the rest).

### H5. TLB efficiency and isolation
Kernel mappings are global (CR4.PGE); user address spaces use PCIDs where available. SMEP and SMAP stay on. **No kernel page-table isolation (KPTI) by default** on the supported 2026 platforms, which are not affected by Meltdown-class leaks; the CPU errata list decides per model.

## Consequences

- Boot info 0.3 and ADR-0002 B3/B4 change (both still Proposed).
- Every physical-to-virtual conversion goes through the direct-map offset (`phys_to_virt`), replacing today's identity assumption; `kernel_space`, `vm`, the AP trampoline (which must still start in low memory, then jump high) and the heap change accordingly.
- Kernel stacks move to the stack region with virtual guard pages.

## Questions for the owner (answered 2026-11-07: yes to 1–3, loader-built tables for 4)

1. Accept the 47-bit user range (H1)?
2. Accept KASLR from the loader with RDSEED/RDRAND/EFI_RNG and "no slide, reported" as the fallback (H3)?
3. Accept "no KPTI by default on 2026 platforms" (H5)?
4. Loader-built tables (H4) or kernel self-relocation?

## Amendment 1 (Accepted, 2026-11-10): the heap, the stacks and the probe page

**Status: Accepted by the owner on 2026-11-10 ("Accept all", decision sheet items 98–100).** The independent review of S3-5a (`docs/engineering/REVIEW_S3-5A_2026-11-10.md`, housekeeping) found that H2's table put the kernel heap and stacks at `0xFFFF_C000_0000_0000`, while the code kept both in the direct map. Its author recommended these answers. A1 and A3 describe what is built; A2 is not built yet. H2's table above now follows them; the rest of the ADR is unchanged.

| # | Decision |
|---|---|
| A1 | **The kernel heap is in the direct map,** as built. Its memory is frames reached through the direct-map offset. Freeing it changes no mapping and needs no shootdown, and it keeps the direct map's large pages. Like the rest of the direct map it is read/write and never executable, and it moves with the direct map's slide (H3). |
| A2 | **Kernel thread stacks move to `0xFFFF_C000_0000_0000` …,** each with an unmapped guard page below it in virtual space. A guard then costs no frame and splits no page of the direct map. Today each guard is a direct-map page unmapped, which costs a frame and splits a 2 MiB page into 4 KiB pages. Built as a framework task of its own. |
| A3 | **PML4 slot 480 (`0xFFFF_F000_0000_0000`) holds one page and nothing else:** the kernel-half proof's probe page. It is read-only, never executable and not global (S3-5a review finding 4), and mapped only while that proof runs. |
