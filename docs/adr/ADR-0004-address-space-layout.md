# ADR-0004: Virtual address-space layout, higher-half kernel and KASLR

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-07, as proposed (H1–H5, and the recommended answer to each of the four questions below). **H1–H4 implemented the same day** (boot info 0.3; loader-built tables; KASLR from RDSEED/RDRAND; the kernel in the higher half with its own tables). H5's global kernel pages, with cross-processor TLB shootdown, were implemented later the same day; PCIDs are not done yet. H2's last clause, that the kernel-half entries point at tables allocated once at boot, was not honoured until N14 step 3, task S3-5a (2026-11-10): each process used to copy whatever entries existed when it was created. Now every entry 256–511 gets a table before the kernel's tables go live, and a boot proof holds a mapping made in a fresh slot after a process exists to being visible from that process. **Amendment 1, accepted by the owner on 2026-11-10** (decision sheet items 98–100): the kernel heap stays in the direct map, the stacks move to their own region (built by task H2-stacks, 2026-11-10), and PML4 slot 480 holds the kernel-half proof's probe page. **Amendment 2, accepted by the owner on 2026-11-10** (decision sheet item 144): the stack region gets a slide, chosen by the kernel (not yet done). |
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
| `0xFFFF_C000_0000_0000` … | kernel stacks (Amendment 1, A2): 2048 slots of 128 KiB (256 MiB), each stack at the top of its own slot with at least 64 KiB below it never mapped; the region's page tables are made at boot. Kernel threads' stacks and each application processor's own stack and four interrupt stacks are here. The boot processor's interrupt stacks are in the kernel image, since they must exist at the first boot step; the boot stack is the loader's (H4). The heap is in the direct map for good (A1) |
| `0xFFFF_E000_0000_0000` … | MMIO windows, mapped uncached on demand by drivers. Since ADR-0010: the first 2 GiB hold the firmware framebuffer, mapped by the loader; drivers' MMIO starts above them |
| `0xFFFF_F000_0000_0000` (PML4 slot 480) | one page and nothing else: the kernel-half proof's probe page (Amendment 1, A3) |
| `0xFFFF_FFFF_8000_0000` + slide | the kernel image, with its segment permissions from the signed ONBF table |

Each address space has the same PML4 entries 256–511, pointing at kernel tables allocated once at boot, so kernel mappings never need to be copied or synchronized between address spaces.

### H3. KASLR
The loader chooses the slides: the kernel image at 2 MiB granularity within its 2 GiB window, the direct map at 1 GiB granularity. Entropy: RDSEED, else RDRAND, else `EFI_RNG_PROTOCOL`; with none, boot proceeds without a slide and says so. The kernel never prints its base after boot except to the serial debug console in developer mode.

### H4. The loader builds the first page tables
The loader already holds the verified segment table. It builds tables mapping the kernel at its slid virtual base, the direct map and the boot info, loads CR3 just before the jump, and passes the table root in boot info **0.3** (all boot-info addresses become virtual). The kernel's entry runs at its virtual address from the first instruction and later rebuilds its tables with its own allocator.
*Alternative:* the kernel relocates itself from identity mapping (keeps the loader simpler, but the kernel's first instructions then run at a different address from the rest).

### H5. TLB efficiency and isolation
Kernel mappings are global (CR4.PGE); user address spaces use PCIDs where available. SMEP and SMAP stay on. **No kernel page-table isolation (KPTI) by default** on the supported 2026 platforms, which are not affected by Meltdown-class leaks; the CPU errata list decides per model.

## Consequences

- Boot info 0.3 and ADR-0002 B3/B4 change (both still Proposed).
- Every physical-to-virtual conversion goes through the direct-map offset (`phys_to_virt`), replacing today's identity assumption; `kernel_space`, `vm`, the AP trampoline (which must still start in low memory, then jump high) and the heap change accordingly.
- Kernel stacks move to the stack region with virtual guard pages (done: task H2-stacks, 2026-11-10, Amendment 1 A2).

## Questions for the owner (answered 2026-11-07: yes to 1–3, loader-built tables for 4)

1. Accept the 47-bit user range (H1)?
2. Accept KASLR from the loader with RDSEED/RDRAND/EFI_RNG and "no slide, reported" as the fallback (H3)?
3. Accept "no KPTI by default on 2026 platforms" (H5)?
4. Loader-built tables (H4) or kernel self-relocation?

## Amendment 1 (Accepted, 2026-11-10): the heap, the stacks and the probe page

**Status: Accepted by the owner on 2026-11-10 ("Accept all", decision sheet items 98–100).** The independent review of S3-5a (`docs/engineering/REVIEW_S3-5A_2026-11-10.md`, housekeeping) found that H2's table put the kernel heap and stacks at `0xFFFF_C000_0000_0000`, while the code kept both in the direct map. Its author recommended these answers. A1–A3 describe what is built (A2 by task H2-stacks, 2026-11-10). H2's table above now follows them; the rest of the ADR is unchanged.

| # | Decision |
|---|---|
| A1 | **The kernel heap is in the direct map,** as built. Its memory is frames reached through the direct-map offset. Freeing it changes no mapping and needs no shootdown, and it keeps the direct map's large pages. Like the rest of the direct map it is read/write and never executable, and it moves with the direct map's slide (H3). |
| A2 | **Kernel thread stacks move to `0xFFFF_C000_0000_0000` …,** each with an unmapped guard page below it in virtual space. A guard then costs no frame and splits no page of the direct map. Until task H2-stacks each guard was a direct-map page unmapped, which cost a frame and split a 2 MiB page into 4 KiB pages. The TLB proof's three pages use the region too while that proof runs, so after boot nothing cuts a page out of the direct map. |
| A3 | **PML4 slot 480 (`0xFFFF_F000_0000_0000`) holds one page and nothing else:** the kernel-half proof's probe page. It is read-only, never executable and not global (S3-5a review finding 4), and mapped only while that proof runs. |

## Amendment 2 (Accepted, 2026-11-10): a slide for the stack region

**Status: Accepted by the owner on 2026-11-10 ("Accept all", decision sheet item 144). Not built yet.** Task H2-stacks (Amendment 1, A2) puts the kernel stacks at the fixed address `0xFFFF_C000_0000_0000` and takes the lowest free slot first, so a kernel stack's address is predictable, while the kernel image and the direct map are slid (H3). Its author asked rather than built, and recommended this answer. CLAUDE.md §58 requires ASLR.

| # | Decision |
|---|---|
| A4 | **The stack region's base is random.** When the kernel builds its own page tables it draws a 2 MiB-aligned base for the 256 MiB region within PML4 slots 384–447, the 32 TiB between the direct map's window and the MMIO windows. That is about 24 bits, the order of the image's slide. |
| A5 | **The kernel chooses this slide, not the loader.** H3 gives the loader the image's and the direct map's slides, because the loader maps both. The stack region's tables are the kernel's, so the kernel draws its base and boot info does not change. The entropy is RDSEED, else RDRAND. `EFI_RNG_PROTOCOL` is gone by then; with neither instruction the region is at its fixed base and the boot says so, as H3 does. |
| A6 | **H3's rule on printing covers it:** after boot the kernel prints neither the region's base nor an address in it, except to the serial debug console in developer mode. The proof of the slide must not print them either. |

**Cost.** None at run time: a stack's address adds a base that is set once. At boot, at most one more page table (130, not 129), because a 256 MiB region at 2 MiB alignment can cross a 1 GiB boundary.

**What it does not do.** The order of slots within the region stays as it is, so one leaked stack address reveals the base, as one leaked address reveals the direct map's slide.

**Alternatives.**
- *The loader draws it and passes it in boot info.* It keeps `EFI_RNG_PROTOCOL` as a third source, but needs a new boot-info version for a value only the kernel uses. Every processor in the declared scope has RDSEED and RDRAND.
- *A random slot for each stack as well.* More bits against a guess, none against a leak; it can be added later without changing this.
- *No slide.* Leaves kernel stack addresses the same on every boot of every machine.

**Building it.** A framework task of its own, after H2-stacks and the fixes from the review of S3-5b have landed. It is proven as H3's slides are: distinct placements across boots, a boot without entropy that reports the fixed base, and a negative control. It closes the item "the stack region has no slide" (T30, T31) in `docs/engineering/THREAT_MODEL_ADDRESS_SPACES.md`.
