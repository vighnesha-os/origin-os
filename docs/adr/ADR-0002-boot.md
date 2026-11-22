# ADR-0002: Boot path from UEFI to the Framekernel (M1)

| | |
|---|---|
| Status | **Accepted** on 2026-11-07, under the owner's instruction of 2026-11-07 to finalise the design with the recommended options (B1–B8 as written, boot info 0.3, and B7 as decided below). B7's default was changed by Amendment 1, accepted by the owner on 2026-11-10 (decision sheet item 78). B5 gained one line on the SPCR's UART before a halt after a failed `ExitBootServices` by Amendment 2, accepted by the owner on 2026-11-21 (decision sheet item 342). Amendment 3, accepted by the owner on 2026-11-21 (decision sheet item 344), lets a bench build, and only a bench build, call `ResetSystem` after `ExitBootServices`. Amendment 4, accepted by the owner on 2026-11-21 (decision sheet item 345), lets a bench build also reset the machine through the FCH's reset port and a triple fault, with no firmware code; three refinements to it were accepted the same day (decision sheet item 346). |
| Date | 2026-11-07; Amendment 1 2026-11-10; Amendment 2 2026-11-21; Amendment 3 2026-11-21; Amendment 4 2026-11-21; boot info 0.6 accepted 2026-11-10 and built 2026-11-11 (B4) |
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

*0.7 (2026-11-13, **accepted by the owner on 2026-11-13**, decision sheet item 293, CLAUDE.md §84; built by TASKS item 40, on `main` since 2026-11-14, pull request 27):* the loader gives `Reserved` to UEFI type 0 (`EfiReservedMemoryType`) alone, and two region kinds are added after `HotPluggable`. `Unaccepted`: UEFI type 15 (`EfiUnacceptedMemoryType`), memory the kernel does not use until it can accept it; it counts as RAM where a device's BAR is checked against RAM, and it is not firmware data. `FirmwareOther`: UEFI type 13 (`EfiPalCode`), the OEM types (0x7000_0000 to 0x7FFF_FFFF), the OS types (0x8000_0000 and above, except Origin's own two) and any type the loader does not know; firmware memory the kernel never uses, treated as `Reserved` where BARs and firmware tables are checked, and never accepted under an RMRR or an IVMD (ADR-0007 Amendment 1, R3). Until 0.7 the loader gave `Reserved` to all of these, so the kernel could not tell them apart. The independent review of TASKS item 40 found it (finding L2). Alternatives: carry each region's raw UEFI type (a larger change to the region record), or give these types the existing `Unusable` kind (which names memory with errors, and loses what unaccepted memory is). Cost (ADR-0018 V3): none, by construction: the loader translates each descriptor once at boot, and the kernel allocates from neither new kind.

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

## Amendment 2 (2026-11-21): one line on the SPCR's UART before a halt after `ExitBootServices`

*Accepted by the owner on 2026-11-21 ("Yes, keep it"; decision sheet item 342). Proposed by TASKS item 112's contract (Q6). TASKS item 112 builds it, in the ninth batch (`land/batch-9`); until that lands, nothing here is built.*

**Change to B5.** After the first `ExitBootServices` call the loader still calls nothing in the firmware. If that call and its one retry both fail, or the retry cannot be made, the loader now does one more thing before it halts: it writes one line to the UART boot info would name (Amendment 3 of ADR-0010: the SPCR's 16550 in I/O space, as the loader's walk found it before the call), and then halts as before. The line names each call's status, or why there was no retry:

```text
origin-boot: ExitBootServices failed (first call EFI_INVALID_PARAMETER, retry EFI_INVALID_PARAMETER): halting
```

- **No firmware call.** The loader reads that UART's line-status register and writes its transmit register with `in` and `out`, as the firmware left it programmed. It does not reprogram it.
- **Bounded.** Each byte waits for room for at most 100,000 line-status reads, the kernel's own bound. A byte that never finds room ends the line, and the loader halts anyway.
- **Only on the failure path.** A boot whose call succeeds writes nothing to the UART, and its console is unchanged. A machine whose firmware names no such UART gets no line, as before.
- It is the loader's only direct write to a UART (ADR-0010 Amendment 4).

**Why.** The failed retry is the loader's one silent end. On rentals B and C (2026-11-20 and 2026-11-21) nothing reached the serial console between the loader's last line and the kernel's first, and a failed retry was the first cause rental C's findings ranked (`docs/research/2026-11-21-rental-c-findings.md`). On a server whose console is the SPCR's UART, the line says why at no cost to a boot that succeeds.

**Alternatives.**

| | Why not |
|---|---|
| Keep the silent halt | A failed `ExitBootServices` stays indistinguishable from a hang anywhere after it |
| Print through the firmware's console after the failure | UEFI 2.11 §7.4 allows only the memory-map service and `ExitBootServices` after the first call: the console may already be shut down |
| Only in bench builds (TASKS item 112) | Every machine whose console is the SPCR's UART would still halt silently |

**Performance cost (ADR-0018 V3).** None, by construction: the line is written only on the path that ends in a halt, and a boot that leaves the firmware does no extra work for it.

**Threat model (§58).** The write goes only to the port the SPCR named and the shared parser accepted (ADR-0010 Amendment 3), never to a guessed one. It happens after the firmware stopped driving the UART, and the loader halts right after it.

**Code (to come with TASKS item 112).** `boot/uefi/src/uart.rs` (the bounded writer and the line, host-tested through a port trait) and `boot/uefi/src/main.rs` (the calls' statuses and the port instructions).

## Amendment 3 (2026-11-21): a bench build may call `ResetSystem` after `ExitBootServices`

*Accepted by the owner on 2026-11-21 ("Allow in test builds"; decision sheet item 344). Proposed by rental D's findings (`docs/research/2026-11-21-rental-d-findings.md`, rank 1). TASKS item 116 builds it, in the ninth batch. Until that lands, nothing here is built.*

**Change to B5, for bench builds only.** A loader built with `--cfg origin_bench` (TASKS item 112) may make one call into the firmware after `ExitBootServices`: the runtime service `ResetSystem`, with a cold reset. It makes the call only to end a diagnostic boot on a rented bench machine. If the machine is still running two seconds after the call, the loader writes the reset register that the FADT names, which it read and checked as untrusted input during its table walk before the call. Then it halts.

- **Why:** on rentals B, C and D nothing reached the console after the loader's last line, and nothing told the two remaining causes apart: a call that never returns, and a console port that the firmware's exit switches off. A deliberate reset after the call is a signal that does not depend on that port. A reset proves the call returned.
- **Every loader that is not a bench build is unchanged:** B5 holds for it in full, as Amendment 2 left it. A bench loader is never signed or packaged (TASKS item 112's F1 to F3), and `cargo xtask check` and the reproducible gate show that no normal image contains this code.
- **Why `ResetSystem` first:** UEFI defines it as a runtime service, callable after `ExitBootServices`. The FADT's reset register needs no service call. On the board in question, though, it is an SMI command port, and nothing has shown that it resets the machine.

**Alternatives.**

| | Why not |
|---|---|
| The FADT's reset register alone, with no exception to B5 | Less certain to reset this board, and a reset that does not happen would read as the call never returning |
| A reset from the kernel | The kernel is not entered in this diagnostic, and its console is the port in question |

**Performance cost (ADR-0018 V3).** None, by construction: only bench builds contain the code, and none ships.

**Threat model (§58).** Only bench builds, which are never signed, contain the call. The FADT is parsed as untrusted input, and a register that fails the check is not written.

## Amendment 4 (2026-11-21): a bench build may reset the machine with no firmware code

*Accepted by the owner on 2026-11-21 ("Allow it"; decision sheet item 345). Proposed by rental E's findings (`docs/research/2026-11-21-rental-e-findings.md`, rank 1, on `land/batch-9` until it lands). TASKS item 121 builds it. Until that lands, nothing here is built.*

**Change to B5 and to Amendment 3, for bench builds only.** A loader built with `--cfg origin_bench` may, in addition to Amendment 3's two methods, reset the machine in ways that run no firmware code:

1. **Before the call, it reads and prints, and writes nothing:**
   - the FCH power-management block's decode-enable register, which says whether port `0xCF9` is decoded;
   - the bit that stops a write to `0xCF9` from resetting;
   - the two bits that let a processor shutdown reset the system;
   - the register that names the cause of the last reset.

   The block is at the address the firmware's own DSDT describes. A value that contradicts the documents' layout makes the loader say so, and then skip the control in step 2.
2. **The control, before the call:** if the last reset was not the loader's own `0xCF9` reset, the run is the boot's first. The loader writes `0x06` (a warm reset) to port `0xCF9` and makes no call. The FCH's own record of that reset then marks the next run.
3. **After the call:** in a run whose last reset was the loader's `0xCF9` reset, the loader writes `0x06` to `0xCF9` as the very first thing after `ExitBootServices` returns, before any line, register read or write-back. If no reset has come 100 ms later by the time-stamp counter, it loads an IDT with a zero limit and executes `int3`, a triple fault. If it is still running after that, it falls through to Amendment 3's order: `ResetSystem`, then the FADT's register, then the halt.

- **Why:** on rental E, three boots ended at the marker before the call, with no reset. That reads two ways: the call never returns, or it returned and both the console's path and the firmware's reset failed. Each reset of Amendment 3 runs firmware code (`ResetSystem`; on this board the FADT's register is an SMI command port). A reset with no firmware code, made before anything else after the call, separates the two. A second run in the boot proves the call returned.
- **What must hold on the machine:** whether `0xCF9` or a shutdown resets the machine depends on FCH bits the firmware sets. Step 1 prints them, and step 2's control shows `0xCF9` resetting before anything rests on it. The registers come from AMD's FCH documents for related parts, not from this processor's own. A run that finds no record of its own reset can only count runs.
- **Every loader that is not a bench build is unchanged,** as for Amendment 3. A bench loader is never signed or packaged (TASKS item 112's F1 to F3), and the reproducible gate shows that no normal image contains this code.

**Alternatives.**

| | Why not |
|---|---|
| Swap Amendment 3's order: the FADT's register first, then `ResetSystem` | Stays within item 344, but the FADT's register still runs firmware code on this board, so a silent boot would still read two ways |
| No more rentals: only the free comparison with a minimal UEFI program, and the COM port's ACPI objects read offline | Separates nothing by itself. The comparison is done anyway, in TASKS item 121 |
| A triple fault alone | Whether a shutdown resets this board depends on two FCH bits nothing has read. With `0xCF9` first and the control, a run that is not reset says which method failed |

**Refinements (2026-11-21, decision sheet item 346).** TASKS item 121's review found three places where the text above would make the diagnostic answer less, or nothing. The owner accepted all three, and they replace the text where they differ:

1. **No record of the last reset.** A run whose FCH record names no documented reset cause makes the call, and resets after it as step 3 says. Without this, a firmware that clears the record at start-up would make every run a control, and the call would never be made. Such a boot has no control. Its result rests on the `0xCF9` decode, trap and shutdown bits the loader prints, and the findings must say so.
2. **The reset only after a successful call.** After the call returns, the loader first compares the returned status, and nothing else comes before the reset: no line, no register read, no write-back. The `0xCF9` write follows only `EFI_SUCCESS`. After an error, the loader's normal retry runs, and its return gets the same check. So a second run proves the call succeeded, and a retry that hangs is not hidden behind a reset after the failed first call.
3. **The triple fault only where it can reset.** The triple fault is used only when the FCH's shutdown-message bit (`PMx74` bit 17) is set, which the loader prints. With it clear, the loader goes straight on to Amendment 3's order. Otherwise the processor would stop silently, and `ResetSystem` and the FADT's register would never run.

**Performance cost (ADR-0018 V3).** None, by construction: only bench builds contain the code, and none ships.

**Threat model (§58).**
- Only bench builds, which are never signed, contain the code.
- Before the call, the loader only reads the FCH's registers, at the address the firmware's DSDT describes.
- The only writes are the documented warm-reset command to `0xCF9` and the loader's own IDT, made to end a diagnostic boot on a rented bench machine.

