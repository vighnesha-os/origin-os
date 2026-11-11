# ADR-0002: Boot path from UEFI to the Framekernel (M1)

| | |
|---|---|
| Status | **Accepted** on 2026-11-07, under the owner's instruction of 2026-11-07 to finalise the design with the recommended options (B1–B8 as written, boot info 0.3, and B7 as decided below). B7's default was changed by Amendment 1, accepted by the owner on 2026-11-10 (decision sheet item 78). |
| Date | 2026-11-07; Amendment 1 2026-11-10; boot info 0.6 accepted 2026-11-10 and built 2026-11-11 (B4) |
| Owner decisions still open | None. B7, carried over from ADR-0001, was decided on 2026-11-07 and amended on 2026-11-10. |
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
3. Signatures, by role `origin-kernel`: exactly one entry with no certificate chain, by a key in the loader's compiled-in trust store, with both signatures verified over the statement (`onbf_verify::verify_krn_block`, 2026-11-08; errata V1–V5, decided by the owner on 2026-11-08). Every byte of the kernel beyond the header and the section table is read only after this step.
4. Manifest SHA-256, then `KrnManifest::decode`, then `check_against`, which requires the entry point to lie in RX segment content.
5. `cpu_features`. Any non-zero value is refused until the CPUID tests in the key registry are implemented: fail closed.
6. Every SEGMENT's content against its Merkle root (`onbf_merkle::root`), and the RELOCS section against its SHA-256.

Any failure stops the boot with a message on the console.

### B3. Placement
- One `AllocatePages(AllocateAnyPages, …)` call covers the whole image span.
- Each segment's content is copied to `base + mem_offset`, and the rest of `mem_size` is zero-filled.
- Then each RELOCS slot gets `*(base + rva) += base` (RELR, ONBF §6.1).

Memory types:
- **The kernel image** is `EfiLoaderCode`. When the boot info is built, it is marked `KernelImage` *by address*, which splits the containing region.
  - OVMF's default no-execute policy (`PcdDxeNxMemoryProtectionPolicy`) is understood to mark `EfiLoaderData` and the OS-defined range as non-executable, so code placed there could not run. `EfiLoaderCode` is the type UEFI intends for loaded code, and the firmware keeps it executable.
  - That NX default is from knowledge of EDK2, not verified in its source here. Using `EfiLoaderCode` is correct either way.
- **The boot info and the kernel stack** are data. They use **OS-defined types**: UEFI reserves 0x8000_0000 and above for OS loaders. 0x8000_0002 is the boot info and 0x8000_0003 the kernel stack, so the firmware's own memory map identifies them.

*KASLR:* in M1 the physical base is whatever the firmware returns. A randomized base, from `EFI_RNG_PROTOCOL`, belongs with the kernel's own page tables in M2, when the virtual layout becomes the kernel's to choose.

### B4. The boot-info structure

*0.3 (2026-11-07, ADR-0004 H4):* `page_table_root`, `direct_map_base`, `kernel_virtual_base` and `flags` (KASLR) are appended (128 bytes). The boot-info pointer in RDI and the initial RSP are direct-map addresses; every other address field stays physical. The page below the stack is a guard (not mapped).

*0.4 (2026-11-08, ADR-0010 E1, **accepted by the owner on 2026-11-08**, CLAUDE.md §84):* the firmware's framebuffer is appended (48 bytes, `size` 176): physical base, size, the virtual address the loader mapped it at, width, height, stride and the channel masks of a 32-bit pixel, or all zero if there is none. A missing or unusable framebuffer never stops the boot. `origin-version.toml` carries 0.4 on the branch that implements it.

*0.5 (2026-11-09, **accepted by the owner on 2026-11-09**, decision sheet item 69, CLAUDE.md §84):* two region kinds, `SpecificPurpose` (14) and `HotPluggable` (15). The loader gives them to conventional memory (type 7) whose descriptor carries `EFI_MEMORY_SP` (UEFI 2.8: memory set aside for one purpose, such as high-bandwidth or CXL-attached memory) or `EFI_MEMORY_HOT_PLUGGABLE` (UEFI 2.11: memory that can be removed while the machine runs); specific-purpose wins when both are set. The kernel allocates from neither and does not direct-map them, so neither reaches general use until the kernel can manage it. Other types keep the kind their type gives, since the kernel uses no other type as general RAM yet; when it reclaims `LoaderReclaimable` memory, that reclaim must consult the same attributes. Research: `docs/research/2026-11-09-uefi-2.8-to-2.11.md`.

*0.6 (2026-11-10, **accepted by the owner on 2026-11-10**, decision sheet item 147, CLAUDE.md §84; built on 2026-11-11 by task 18b and booted under QEMU):* one field is appended, `boot_seed`, a `u64`: the physical address of 64 bytes inside the region that holds the boot info, or 0 when the loader has none to give (`size` 184). The bytes are secret: output of the loader's own random-number generator, for the kernel's to take as extra input. The kernel checks they lie inside the boot-info region, takes them, and overwrites them before another processor starts. Neither side prints them. They are given by address because `BootInfo` is copied and printed. The decision and its reasons are ADR-0016 R10; `origin-version.toml` says 0.6 since task 18b built it; the kernel also requires the area to lie clear of the header and of the region and segment arrays. *Amended 2026-11-11 (decision sheet item 166; ADR-0016 Amendment 1), still as 0.6 because 0.6 has not landed:* the area `boot_seed` names is 72 bytes, the 64 of the seed and then 8, a little-endian `u64` from the loader's generator, from which the kernel places its stacks' region when it builds its page tables. The kernel takes that value once, overwrites all 72 bytes when its generator takes the seed, and prints none of them.

`origin-boot-info` defines the structure, at version 0.2 (`origin-version.toml [compatibility] boot-info`); major 0 means unstable, so the kernel accepts only its exact version. It contains:
- a magic number, the version and the structure's size;
- the physical memory regions, using Origin's own kinds: the loader translates the UEFI map so the kernel never sees UEFI structures;
- the kernel base and span;
- the ACPI 2.0 RSDP, from the configuration table;
- the UEFI system table, for runtime services later;
- the kernel stack;
- *(0.2, 2026-11-07)* the **kernel segment table**: for each SEGMENT, in RVA order, its RVA, its length in pages, and R/W/X access bits taken from the verified ONBF segment table (at most 16). A RELRO segment arrives without W, because the loader has already applied the relocations. The loader checks the list with `origin_boot_info::check_segments` before `ExitBootServices`, and the kernel checks it again: aligned, non-empty, sorted, disjoint, inside the span, readable, never W and X together.

The segment table exists so that the kernel maps its own image with exactly the permissions the signed image declares, instead of trusting linker symbols or re-parsing ELF. The kernel's first address space (`framekernel::kernel_space`) maps:
- each kernel segment with its declared access;
- a read/write, never-executable identity direct map of `Usable`, `LoaderReclaimable`, `BootInfo`, `KernelStack` and `AcpiReclaimable` regions;
- nothing else. Page 0, MMIO, ACPI NVS, runtime services and reserved memory stay unmapped, and the `KernelImage` region is outside the direct map, so there is no writable alias of kernel code.

The kernel validates the header before it trusts any other field. The KRN manifest records the boot-info version the kernel expects (key 24), and the loader refuses a mismatch.

### B5. Leaving the firmware
1. Allocate everything first: the kernel pages, a 64 KiB stack, the boot-info page with room for the region array, and the memory-map buffer.
2. Print the last console line, then clear the loader's panic console.
3. Call `GetMemoryMap`, translate it into the region array (no allocation happens here), then `ExitBootServices(map_key)`.
4. On `EFI_INVALID_PARAMETER`, which means the map changed, get the map and translate again, then retry once, as UEFI 2.11 §7.4 allows.

After that, the loader calls nothing in the firmware.

### B6. Entering the kernel

*Since 2026-11-07 (ADR-0004 H4):* the loader builds the first higher-half tables with the shared `origin-paging` layout builder (from a memory map taken after every allocation, with no firmware call during the build), relocates the kernel for its KASLR-chosen virtual base, and enters through a naked stub that sets EFER.NXE and CR0.WP, loads CR3, switches to the direct-map stack and calls the virtual entry. The stub's own pages are mapped at their identity address in those tables; the kernel's own tables, built next, drop them. The bullets below describe the original identity-mapped entry.

- `cli`; RSP set to the 16-byte-aligned top of the kernel stack; RBP cleared.
- Then `call entry`, with RDI holding the boot-info address. That is the System V convention (`extern "sysv64"`), explicit in the assembly because the loader itself is built for the Microsoft x64 ABI.
- The firmware's identity mapping stays active. The kernel then installs its own page tables (M2, done 2026-11-07): it sets EFER.NXE and CR0.WP and loads CR3, then proves the protections with four deliberate faults: a write to code, a write to read-only data, a fetch from writable data and a read of page 0.
- If the kernel ever returns, the loader halts the CPU.

### B7. Boot trust anchor (decided 2026-11-07; default order reversed by Amendment 1, 2026-11-10)
**Both, in this order of preference for shipping:**
1. **Default: a first stage signed under Microsoft UEFI CA 2023.** A minimal first-stage loader carries the Microsoft signature so Origin boots on stock PCs with Secure Boot on. That stage trusts nothing but Origin's own release keys: it verifies the Origin loader and every later stage with the ONBF hybrid suite (Ed25519 + ML-DSA-87, ADR-0001 D3), so Microsoft's key vouches only for the first stage. The annual audit the CA requires is a cost to plan for.
2. **Supported: Origin-owned machines.** OEMs and users may enroll Origin's keys in the firmware's `db` (and remove others), which drops the Microsoft link entirely; the same Origin chain applies from the first stage on.
3. **Developer mode (B8)** stays for unsigned development images and never ships.

*Alternatives:* Microsoft signing only (no Origin-owned option), or Origin enrollment only (most PCs could then not boot Origin without changing firmware settings).
*Consequences:* the first stage stays small and changes rarely (each change needs a new Microsoft signature); Secured-core PCs, which may not trust the third-party CA by default, need the Origin-owned path or a firmware setting. Obtaining the signature is a business step outside the code.

### B8. Developer mode
A loader built with the `developer-mode` feature accepts an unsigned KRN image and says so on the console. The `xtask` boot test builds this variant.

A loader built without the feature refuses unsigned images. It also verifies signed ones (since 2026-11-08), against the compiled-in trust store, and that store is empty until Origin's own kernel key exists (errata V4). So a build without the `test-trust-store` feature still cannot boot anything, which is the safe failure.

Developer mode is a build feature, never a runtime choice. A developer-mode loader verifies a signed image like any other (errata V5, decided by the owner on 2026-11-08).

## Consequences
- The kernel binary is a static-PIE ELF converted by `onbf-pack`. The loader never parses ELF (ADR-0001 D1).
- QEMU + OVMF proves this path end to end. Physical hardware is still unproven (CLAUDE.md §74).
- The kernel's signature is verified (B2 step 3), but no protection against physical tampering exists yet: there is no release key, and the loader itself is not checked until B7's first stage exists.

## Amendment 1 (2026-11-10): Origin-owned keys first

*Accepted by the owner on 2026-11-10, as recommended (decision sheet item 78; CLAUDE.md §18, §19, §58).*

**Change.** B7's two shipping paths swap order. **The default is now B7 item 2:** users and OEMs enroll Origin's keys in the firmware's `db`. The Microsoft-signed first stage (B7 item 1) stays the plan, and it starts once an organisation exists that can hold the signature.

**Why.** Getting a first stage signed under Microsoft's third-party UEFI CA takes three things a public open-source project run by one person cannot provide today:

- **A Partner Center account and an EV code-signing certificate.** Microsoft's published UEFI signing requirements ask for both ([Microsoft, 2021](https://techcommunity.microsoft.com/blog/hardwaredevcenter/updated-uefi-signing-requirements/1062916)). EV certificates are issued to organisations, not individuals.
- **An annual security audit.** Since October 2025, submitters to the third-party CA must pass one every year, run by an independent partner Microsoft approves. Microsoft's page lists this under "Coming soon" ([Microsoft, posted 2025-09-21, updated 2026-06-26](https://techcommunity.microsoft.com/t5/hardware-dev-center/signing-with-the-new-2023-microsoft-uefi-certificates-what/ba-p/4455787)).
- **No shim exemption for Origin.** Shim submissions avoid the separate audit through the shim review board, but shipping `shim` would put third-party boot code in Origin's chain (§3).

The 2011 CA expired in June 2026, so only the 2023 CA matters ([Microsoft](https://support.microsoft.com/help/5062710)).

**Alternatives.**

| Alternative | Why not |
|---|---|
| Keep B7 as decided | The default would depend on a signature the project cannot obtain yet |
| Ship `shim` | Breaks §3 |
| Drop the Microsoft path for good | Most PCs could then never boot Origin without a firmware change. B7 rejected this, and nothing has changed that |

**Consequences.**

- On most PCs, booting Origin with Secure Boot on means putting the firmware into its custom key mode and enrolling Origin's keys. Secured-core PCs were already in that position (B7). The installer must guide this step (§21, §22). It is not yet built.
- Enrolment needs Origin's own release keys, which do not exist yet (TASKS item 6: the signing ceremony, §60).
- Developer mode (B8) is unchanged.
- B7's first stage is still worth building small and stable, because the same first stage is what Microsoft would later sign.

**Migration.** Nothing built changes: neither path exists yet. CLAUDE.md §19 says which path is the default (revision 7.3).
