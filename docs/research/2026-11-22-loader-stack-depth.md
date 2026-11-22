# How deep the loader's stack goes before `ExitBootServices`

| | |
|---|---|
| Date | 2026-11-22 |
| Kind | Research ledger (CLAUDE.md §16). Rental F's findings, rank 1 (`docs/research/2026-11-22-rental-f-findings.md`); the rank-2 list's candidate 2 (`docs/research/2026-11-21-loader-before-the-call.md`); TASKS item 127 |
| Decided by | The coordinating session's rulings on TASKS item 127's contract, Q1 to Q6, 2026-11-22 |
| Ran | `cargo xtask stack-depth`: first at `cad07768` (three scenarios, 9 boots, 05:14 to 05:16 UTC), then at `d1c63ef4` (four scenarios, 12 boots, 05:41 to 05:42 UTC), in turns of the development Mac; QEMU 11.1.3's software processor (TCG) with OVMF, one processor; Rust 1.100.0 |
| Read | UEFI 2.11 §2.3.4, from the owner's copy (PDF pp. 112–113, printed pp. 28–29), paraphrased. Origin's own code and its own builds' link maps and disassembly. No other project's source |
| By | An AI session (the TASKS 112/116/121/127 session); no person |

## The answer

**The stack is far from UEFI's 128 KiB, so it drops to the bottom of the candidates** (the coordinating session's ruling under Q6 (a), 2026-11-22).

| Path | The deepest point written, from `efi_main`'s RSP | With `efi_main`'s own frame | Of 128 KiB |
|---|--:|--:|--:|
| **What the rentals ran:** an unsigned kernel, accepted by developer mode, normal or bench loader | 6,432 bytes in the final run; 8,264 at most in either run | 10,312 bytes at most | 7.9% |
| **The shipping path:** the normal loader with a signed kernel, Ed25519 and ML-DSA-87 verified | 20,032 to 21,352 bytes | 23,240 bytes at most | 17.7% |

- **What it means for a firmware stack overflow.** For the stack to pass what UEFI promises, the firmware's own frames would need more than 120 KiB beyond the loader's on the rentals' path, or more than 105 KiB on the shipping path.
- **OVMF's frames are already in the figure:** its console, file system, allocation, memory-map and timer-interrupt frames ran on the same stack.
- **AMI's frames on the rented board have not been seen.** A firmware that needs 100 KiB of stack for those services would be far outside anything here.
- **The rentals ran the unsigned path.** Bench images are never signed, and the rented loader said "developer mode: unsigned kernel images are accepted". So the 7.9% is the rented boards' figure under QEMU, and the 17.7% is the shipping loader's.

## What UEFI promises

UEFI 2.11 §2.3.4 (x64 platforms) says that while boot services run, an image has 128 KiB or more of stack available, 16-byte aligned, which may be mapped non-executable. A caller of a runtime service after the exit must give it 4 KiB or more.

## The method

- **A measurement build.** A new flag, `cfg(origin_stack_measure)`, is given only by `cargo xtask stack-depth`, into `target/stack-measure`. The build is the developer-mode loader, normal or bench, never signed and never rented. TASKS item 112's F1 and F2 hold the flag and its text, "stack measure", as they hold the bench build's.
- **The paint.** In `efi_main`'s first statement the loader reads RSP. It writes a pattern into every 8-byte word from 128 KiB below that RSP up to 512 bytes below the painter's own RSP. That is exactly the stack UEFI promises, and nothing beyond it.
- **The calibration, the positive control.** A function writes every word of a 64 KiB array on its own frame, and the scan must find that array's lowest word, or a word at most 512 bytes below it.
- **The phases.** At the end of each of six phases, the lowest word that no longer holds the pattern is the deepest point written since the last paint. Its distance from `efi_main`'s RSP is the phase's depth. The phase's line is then printed, and the range below the current RSP is painted again. The phases:
  - P1 reading the kernel file;
  - P2 verifying it;
  - P3 placing it and starting the generator;
  - P4 the allocations and the framebuffer;
  - P5 the page tables and the table walks;
  - P6 the last lines, the bench loader's reads, the last memory map and the mark, up to the call.

  P6's line goes straight to the SPCR's UART after the last map, so no firmware call changes its key.
- **The static chain, the cross-check (ruling Q4).** The measurement build is linked with a map (`/MAP`). From the map's symbols and `llvm-objdump`'s listing, each function's frame is what its prologue reserves: the return address, its pushes, and its `sub rsp`, probed or not. The longest chain from `efi_main` runs over direct calls and tail jumps. Indirect calls (into the firmware, or through a function pointer) are leaves. The measurement's own functions count no frame, and a stack probe counts none.
- **The scenarios,** three boots each:
  - the normal loader;
  - the bench loader as a rental runs it, with QEMU reporting rental A's EPYC 4244P, so that the loader takes the AMD branch and scans the DSDT;
  - the bench loader with `BENCH-FCH`'s values, a run that makes the call;
  - (the second run only) the normal loader with the test-signed kernel.

## The figures

Bytes below `efi_main`'s RSP after its prologue, the range over three boots; the run at `d1c63ef4`.

| Scenario | Calibration | P1 | P2 | P3 | P4 | P5 | P6 | Deepest |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| Normal loader, unsigned kernel | exact | 3,848–3,960 | 5,976 | 4,688–5,176 | 1,320–2,056 | 3,480–3,720 | 2,104–2,200 | 5,976 (4.6%) |
| Bench loader as a rental runs it | exact | 3,960 | 5,976 | 4,688 | 1,320 | 6,432 | 3,544–4,232 | 6,432 (4.9%) |
| Bench loader with `BENCH-FCH`'s values | exact | 3,848–4,088 | 5,976–6,376 | 4,688 | 1,320–2,104 | 6,432 | 4,536–4,776 | 6,432 (4.9%) |
| Normal loader, signed kernel | exact | 3,848–3,960 | 20,032–21,352 | 4,688 | 1,320–1,992 | 3,000–3,432 | 2,104–2,552 | 21,352 (16.3%) |

- **"Exact"** means the calibration found its array's lowest word itself, 0 bytes below it, in every boot.
- **The first run at `cad07768`** had the three unsigned scenarios. It gave the same figures to within some 1.8 KiB, with two higher ones in single boots: 7,000 in the bench loader's P2, and 8,264 in the `BENCH-FCH` scenario's P5. The other boots of the same build gave 5,976 and 6,432 there. So the difference is from boot to boot, not from the build: what the firmware's timer interrupt, or its console in P5, writes when it lands at the deepest point. In the signed run, that alone is up to 1,320 bytes. The table above is the second run's. The 8,264 is kept as the largest figure seen on the rentals' path.

**The static chain** (`d1c63ef4`'s measurement builds):
- **The longest chain:**
  - the normal build, 21,920 bytes, `efi_main`'s own 1,888 included;
  - the bench build, 22,080 bytes, with `efi_main`'s own 2,048.

  Both run `efi_main` → `kernel::verify` (1,536) → `onbf_verify::verify_krn_block` (3,136) → `origin_crypto::mldsa::mldsa87_verify` (13,312) → `mldsa::sample::add_matrix_entry_product` (1,056) → `sha3::Shake128::finalize_xof` (560) → …, in eight functions.
- **No recursion** over direct calls.
- **The two methods agree to the byte.** The signed scenario's quietest boot wrote 20,032 bytes below `efi_main`'s RSP, which with `efi_main`'s 1,888 is 21,920, exactly the static chain. The other boots' extra 1,320 bytes, in the phase that calls no firmware, are OVMF's timer interrupt landing at the deepest point.
- **The deepest indirect call is:**
  - 3,200 bytes down (normal);
  - 8,352 bytes down (bench).

  In the bench loader that is within the DSDT scan's walk, whose reads go through a closure, so the indirect call there is Origin's own.

**The loader's largest frames** are ML-DSA's verifications: `mldsa87_verify` reserves 13,312 bytes and `mldsa65_verify` 11,184. Only a signed kernel runs them, and the rentals' kernels were unsigned.

## What it does not show

- **AMI's frames.** The firmware's services ran as OVMF's. On the rented board their depth is unknown; the figure above says how much room they have.
- **Below the paint.** A phase that went past 128 KiB would read "128 KiB or more". None came near it.
- **Reserved but unwritten stack.** The paint counts words written; a frame reserved and never written to its bottom does not count. The static chain counts what prologues reserve. The two agree on the signed path's deepest chain.
- **`ExitBootServices`'s own frames.** The figure runs to the call. The firmware's exit handling is not measured, under QEMU or on the board.
- **The measurement build is not the shipping build.** Its hooks are out of line, and the normal and bench images are unchanged: at `cad07768`, `reproducible-compare` against `land/batch-10`'s `953cda2c` found all 40 files the same bytes, and the bench loader is still rental F's `7a3a7d64…`. Under the flag, the hooks still add calls to the functions they sit in.
- **A word written with the pattern's own value is not counted.** This is negligible, as in TASKS item 68.
- **One processor, TCG.** No KVM run, and no physical machine.

## The controls

At `cad07768`, each control planted and then restored, each of which failed for its own reason:
- **SD-1, the paint writing another value:** the calibration found the bottom of the promise ("131072 bytes or more"), and `stack-depth` failed.
- **SD-2, the measurement's text in a bench image:** `bench-build` refused it under F2.
- **SD-3, the flag named in an unlisted file:** F1 refused it.
- **SD-4, the scan from the top down:** the calibration found 552 bytes, where its array lay 65,552 bytes down, and `stack-depth` failed.

## The ranking that follows

**The rank-2 list's candidate 2,** the stack's depth on the firmware's stack, goes to the bottom of the candidates, below the generator's steps and the ACPI walk's reads. In rental F's findings, the next step's rank 1 (this measurement) is done, and the stack drops out of contention.
