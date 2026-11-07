# ADR-0002: Boot path from UEFI to the Framekernel (M1)

| | |
|---|---|
| Status | **Proposed.** Owner approval is needed before Accepted (CLAUDE.md §84). Development builds follow it; nothing built on it ships until then. |
| Date | 2026-11-07 |
| Owner decisions still open | B7, the boot trust anchor (carried over from ADR-0001). The other items are proposals the owner may change. |
| Implements | CLAUDE.md §19 (UEFI boot chain), §20 (boot manager, later), §58 (Secure Boot), ONBF §8.1 (chain of trust) |
| Code | `boot/info` (`origin-boot-info`), `boot/uefi` (`origin-boot-uefi`), `framekernel/`, `developer-tools/onbf-pack` |

## Context

M1 step 1 ended with a loader that validates the system table and returns `EFI_UNSUPPORTED`. Step 2 must load a real Framekernel, which is a KRN profile ONBF image (ONBF §6.5). It must verify the image with the code that now exists (`onbf-format`, `onbf-merkle`, `origin-crypto`), leave the firmware, and enter the kernel.

## Proposed decisions

### B1. Where the kernel comes from
The loader reads `\EFI\ORIGIN\KERNEL.ONBF`, using two protocols:
- `EFI_LOADED_IMAGE_PROTOCOL.DeviceHandle`, to find the volume the loader itself came from;
- `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` on that volume, to read the file.

A/B deployments with fallback (CLAUDE.md §20, §64) will replace the fixed path. *Alternatives considered:* a loader-embedded kernel, which would tie every kernel update to re-signing the loader; or a raw partition, which would need a block-level reader in the loader with no gain.

### B2. Verification order (ONBF-R1, §8.1)
Nothing variable-length is interpreted before it is verified.
1. `Image::parse`. The profile must be KRN.
2. Content digest: SHA-512 over the header and section table.
3. Signatures, by role `origin-kernel`. **Not implemented yet.** See B8.
4. Manifest SHA-256, then `KrnManifest::decode`, then `check_against`, which requires the entry point to lie in RX segment content.
5. `cpu_features`. Any non-zero value is refused until the CPUID tests in the key registry are implemented: fail closed.
6. Every SEGMENT's content against its Merkle root (`onbf_merkle::root`), and the RELOCS section against its SHA-256.

Any failure stops the boot with a message on the console.

### B3. Placement
- One `AllocatePages(AllocateAnyPages, …)` call covers the whole image span.
- Each segment's content is copied to `base + mem_offset`, and the rest of `mem_size` is zero-filled.
- Then each RELOCS slot gets `*(base + rva) += base` (RELR, ONBF §6.1).

The loader allocates the kernel image, the boot info and the kernel stack with **OS-defined UEFI memory types**. UEFI reserves 0x8000_0000 and above for OS loaders: 0x8000_0001 for the kernel image, 0x8000_0002 for the boot info, 0x8000_0003 for the kernel stack. Because of that the firmware's own memory map identifies them, and the loader never has to split regions by address.

*KASLR:* in M1 the physical base is whatever the firmware returns. A randomized base, from `EFI_RNG_PROTOCOL`, belongs with the kernel's own page tables in M2, when the virtual layout becomes the kernel's to choose.

### B4. The boot-info structure
`origin-boot-info` defines the structure, at version 0.1 (`origin-version.toml [compatibility] boot-info`); major 0 means unstable. It contains:
- a magic number, the version and the structure's size;
- the physical memory regions, using Origin's own kinds: the loader translates the UEFI map so the kernel never sees UEFI structures;
- the kernel base and span;
- the ACPI 2.0 RSDP, from the configuration table;
- the UEFI system table, for runtime services later;
- the kernel stack.

The kernel validates the header before it trusts any other field. The KRN manifest records the boot-info version the kernel expects (key 24), and the loader refuses a mismatch.

### B5. Leaving the firmware
1. Allocate everything first: the kernel pages, a 64 KiB stack, the boot-info page with room for the region array, and the memory-map buffer.
2. Print the last console line, then clear the loader's panic console.
3. Call `GetMemoryMap`, translate it into the region array (no allocation happens here), then `ExitBootServices(map_key)`.
4. On `EFI_INVALID_PARAMETER`, which means the map changed, get the map and translate again, then retry once, as UEFI 2.11 §7.4 allows.

After that, the loader calls nothing in the firmware.

### B6. Entering the kernel
- `cli`; RSP set to the 16-byte-aligned top of the kernel stack; RBP cleared.
- Then `call entry`, with RDI holding the boot-info address. That is the System V convention (`extern "sysv64"`), explicit in the assembly because the loader itself is built for the Microsoft x64 ABI.
- The firmware's identity mapping stays active. The kernel installs its own page tables in M2.
- If the kernel ever returns, the loader halts the CPU.

### B7. Boot trust anchor (open; carried from ADR-0001)
There are two options:
- signing the loader under Microsoft UEFI CA 2023 (with an annual audit);
- Origin keys enrolled in firmware by the user or OEM.

This decision is the owner's. Neither changes B1–B6.

### B8. Developer mode
A loader built with the `developer-mode` feature accepts an unsigned KRN image and says so on the console. The `xtask` boot test builds this variant.

A loader built without the feature refuses unsigned images. Until signature verification exists (ONBF §7), that build cannot boot anything, which is the safe failure.

## Consequences
- The kernel binary is a static-PIE ELF converted by `onbf-pack`. The loader never parses ELF (ADR-0001 D1).
- QEMU + OVMF proves this path end to end. Physical hardware is still unproven (CLAUDE.md §74).
- No protection against physical tampering exists until B7 is decided and signatures are verified (B2 step 3).
