# ADR-0004: Virtual address-space layout, higher-half kernel and KASLR

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-07, as proposed (H1–H5, and the recommended answer to each of the four questions below). **H1–H4 implemented the same day** (boot info 0.3; loader-built tables; KASLR from RDSEED/RDRAND; the kernel in the higher half with its own tables). H5's global kernel pages, with cross-processor TLB shootdown, were implemented later the same day; PCIDs are not done yet. |
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
| `0xFFFF_8000_0000_0000` + slide | direct map of all RAM, read/write, never executable (1 GiB or 2 MiB pages; holes as in K2) |
| `0xFFFF_C000_0000_0000` … | kernel heap and stacks (per-thread stacks with guard pages in virtual space, not in the direct map) |
| `0xFFFF_E000_0000_0000` … | MMIO windows, mapped uncached on demand by drivers |
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
- Kernel stacks move to the stack region with virtual guard pages.

## Questions for the owner (answered 2026-11-07: yes to 1–3, loader-built tables for 4)

1. Accept the 47-bit user range (H1)?
2. Accept KASLR from the loader with RDSEED/RDRAND/EFI_RNG and "no slide, reported" as the fallback (H3)?
3. Accept "no KPTI by default on 2026 platforms" (H5)?
4. Loader-built tables (H4) or kernel self-relocation?
