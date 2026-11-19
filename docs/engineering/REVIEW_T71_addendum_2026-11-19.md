# Independent review, addendum: TASKS item 71's answers

| | |
|---|---|
| Reviewed | Branch `claude/acpi-area-a-71`, the answers `b5621ef2..26464513`. The code ends at `ceac9b0f`; `26464513` is records only, and its code is byte-identical to `ceac9b0f`'s. Read: every code commit (`78f67374`, `b627337d`, `1d9325e8`, `5c67e6f7`, `fe39cb31`, `6451df58`, `ceac9b0f`), the records commit, `6b192824` (the review as received), and the hand-over `t71-answers-handover.md`. |
| When | 2026-11-19, from 10:17 UTC (`date -u`). |
| By | The same AI reviewer as the first report, independent of the author's session. No person has reviewed this. |
| Spec read again | ACPI 6.6 §5.2.24.4 to §5.2.24.6, through the same PDFKit reader. Each section says that only one of its pointer records will be produced. |
| Ran | All on scratchpad exports, each command inside `cargo xtask exclusive --`.<br>(1) On an export of `ceac9b0f`: `cargo test -p origin-acpi-tables -p origin-frame --lib`. The crate passed 80 with 1 ignored; `origin-frame` passed 287.<br>(2) Control AT-H3 again at `ceac9b0f`: with the `&& !fadt_seen` guard removed, `only_the_first_valid_fadt_and_fpdt_count` fails.<br>(3) Probe P4, a temporary test removed afterwards (each file compared back to its commit). A listed OEM table, valid with a correct checksum, starts 0x80 bytes before a genuine FACS and declares 0x100 bytes, so its extent holds the FACS's live 64 bytes. The probe was run against both `ceac9b0f` and `b5621ef2`. |

## Status of T71-1 to T71-8

| Finding | Status | Reason |
|---|---|---|
| T71-1 (Medium) | **Closed** | (a) Only the first valid FADT and FPDT count, as `find_table` and the census choose, and only the first pointer record of each kind; §5.2.24.4 to .6 say only one is produced. The worst case is exactly `CHANGING_MAX` (5), with a compile-time check against `NEVER_SLOTS` (8) at `firmware.rs:96`. Unreadable entries, headers and bodies are now noted.<br>(b) `guard_gaps` fails the verdict on `unlocated`, `untracked` and `already_lent`, including a first lookup that could not read the RSDP or XSDT. Lending still begins in that case, as ruled; the census now fails, so the false pass is gone.<br>(c) The census probes the FACS and the performance tables it found and checked itself, so the check is no longer circular. AT-11 and AT-12 show (c) and (b) catching what the base passed.<br>A residual narrowing for non-conforming firmware is T71-11. |
| T71-2 (Medium) | **Partly closed, with a regression** | P2's hazard is closed by the signature check alone: AT-13 at boot, plus host tests. The overlap rule added beside it drops a *genuine* structure from the guard, and the kernel then lends over it (T71-9). Removing the 1 MiB cap is sound only with T71-9 fixed. |
| T71-3 (Low) | **Closed as recorded** | The headline is reworded, and holes and `Reserved` memory are recorded as a known limit, as ruled. One citation in the answer is wrong (T71-10). |
| T71-4 (Low) | **Closed** | `port_block` now applies only the length rule to the port form (`length_verdict(.., true)`), so a reserved space or access size in `X_PM_TMR_BLK` falls back to the port. `relied_on_fadt` still sees the fault, so the census still fails. A memory-space `X_` form, an `X_` port above 0xFFFF, a length other than 4, and a hardware-reduced platform behave as before. Host tests are in the crate and the kernel, and AT-H6 covers it. |
| T71-5 (Low) | **Closed** | Nine caches are accepted and recorded as behaviour change 8. The test takes nine and refuses ten. `parse`'s and `MAX_LOOKUPS`' docs are corrected (three walks, the 17th lookup). |
| T71-6 (Low) | **Closed** | Each large table is asserted `Class::Checked`, and the NFIT's refusal at 400 copies is asserted by name. The PPTT and VIOT limit is stated in the test's doc. `lib.rs` names the 12 signatures. The narrowing is recorded as departure 7. |
| T71-7 (Low) | **Closed** | `firmware::guards` asks `bytes`' own `guard` and the registry's `Lending::guards`, which is the very predicate `lend` now calls, and records nothing. The probe from 8 bytes before each structure is added (AT-16 catches the start-inside-only overlap). The first lend not refused is named. `consistent()`'s doc is updated. The host test shows the broken-guard census lends nothing. |
| T71-8 (Low) | **Closed; the answer holds** | The comments in `timer.rs` (the SAFETY comment) and `time.rs` and the FADT module doc are corrected. On the refusal ranges: `firmware::bytes` returns `&'static str`, which cannot carry a range without a new error type through `lookup`, `acpi_table`'s callers and the AML proof's trait. Ruling 3 had already said "no new raw-address form", so the contract's sentence conflicted with its own ruling. Correcting the claim is right, and the census does name each never-lent range, the passed-over one and the first lend not refused. |

## New findings

Counts: **High 1, Medium 0, Low 2.**

### T71-9 — High — T71-2's overlap rule takes a genuine changing structure out of the guard whenever a lent table's declared extent covers it. The kernel then lends over it, which it refused before the answers.

**Where:** `framekernel/frame/src/acpi.rs`.
- `:568-594` (`ceac9b0f`): every recorded range that overlaps the RSDP, the RSDT, the XSDT, the DSDT or any XSDT entry's declared extent (`lendable_extent`, `:372`) is set to `None` and only reported.
- The doc of `changing_ranges` (`:405`) states this as the rule.

**The defect:** the set's extents come from header length fields, which are not checked against a checksum. Once the signature check is in place, a target that starts with `FACS`, `FBPT`, `S3PT` or `MBPT` is very likely the real structure. When such a structure overlaps a table, the malformed party is the table. The rule still gives the lend to the table.

**Probe P4 (run on both commits):**
- At `b5621ef2` the FACS is recorded, and the kernel's `lookup(.., b"OEMX", ..)` is refused (`Err("refused")`; lent: the RSDP and the XSDT only).
- At `ceac9b0f`:
  - the locator gives `passed_over = Some((Facs, 0x7f002000, 0x7f002040))`, "a structure the firmware changes overlaps a table the kernel lends";
  - `lookup(.., b"OEMX", ..)` returns the table, 256 bytes;
  - the lent ranges then include `(0x7f001f80, 0x7f002080)`, which holds the live FACS.

**The same happens at boot without any lookup asking for that table:**
- The census lends every XSDT entry (`read_table` → `fw.lend(addr, length)`). It does this before its live check, and the lend lasts for the kernel's life.
- If the covering table is the DSDT or an SSDT, the AML proof lends it too, and parses bytes that firmware in SMM writes (the global lock).

**What one malformed length field does now:**
- One length that runs over the FACS — an OEM table, an SSDT, or a DSDT a few bytes too long — is enough. The table's checksum need not even pass, because `whole_table` and the census lend before checking.
- The verdict does fail (a gap, and the census's own FACS probe). But the boot carries on with the reference, which is the outcome TASKS 71 exists to prevent.
- The STATUS line "Nothing lent that the firmware changes … nothing that overlaps one, wherever the range was named from" is false in this case.

**Why High:** the scale names this class, "a lend of changing firmware memory". It is a regression introduced by the answers, and it needs only one malformed length. That is untrusted input by the kernel's own rule (`acpi.rs`'s module doc). In T71-1 the comparable lend needed two independent faults and already existed in the base.

**Suggested fix:** let the overlap check trim, never drop, a target that passed the signature check.
1. Record its kind's minimum (64 for a FACS, 24 for an MBPT, 8 for the others).
2. Extend it to the declared length only where that covers no lent table.
3. If even the minimum overlaps a lent table, record it anyway. That table then becomes unreadable, as at `b5621ef2`. Report it as now, so the census fails.

This keeps P2 closed, since the signature check alone handles it (AT-13). It keeps the "FBPT whose length covers every table" test passing, because the FBPT is guarded at its first 8 bytes. It also keeps the uncapped length safe. Add P4 as a host test, and a boot control in the style of AT-14 that points a listed table's length over the FACS. If the coordinator ruled the current priority, this needs that ruling revisited.

### T71-10 — Low — The T71-3 answer cites F20 for holes; F20 is not about holes.

**Where:** `docs/engineering/TASKS.md:2023` (at `26464513`): "firmware does place ACPI tables in `Reserved` memory, and holes are TASKS item 62's F20".

**The defect:** TASKS 62's F20 is about every table staying lent for the kernel's life (ACPI reclaim, and copying instead of lending). It says nothing about holes in the memory map. The holes limit has no task or finding of its own, apart from F14 and T71-3.

**Suggested fix:** cite F14 or T71-3, or open a TASKS item for holes and `Reserved` memory, for example "tables only from ACPI reclaim, ACPI NVS, reserved or runtime memory, or holes mapped uncached".

### T71-11 — Low — A second FADT or FPDT's structures are neither guarded nor counted as a gap.

**Where:** `framekernel/frame/src/acpi.rs`.
- `:438`: the loop stops once the first valid FADT and FPDT are seen.
- `:493`: only the first FPDT is read.
- The census follows only its first FADT and FPDT.

**The defect:** an XSDT that lists two FPDTs is non-conforming. The OS takes the first, but firmware may still rewrite the second one's FBPT and S3PT on resume, since it set them up. At `b5621ef2` both FPDTs' targets were located, within the cap. Now they are not located, not probed, and not a gap, so the census passes while their middles are lendable. Only N3 guards their starts. The same holds for a second FADT's FACS.

This follows the "lookups' own rule" design. It is not counted, and nothing in the records names it.

**Suggested fix:** count a second FADT or FPDT in the XSDT as a guard gap. Or, since `NEVER_SLOTS` keeps 3 spare slots, record their signature-checked targets too, noting a gap when they do not fit. Name it in the records either way.

## The hand-over's decisions

1. **T71-8, the reasoned reply.** Sound (see the status table).
2. **`Firmware::guards` defaults to "not refused".** Sound. It is used only by the census's live check (`acpi_census.rs:580-596`).
   - A reader that does not override it makes every probe count as a lend the guard would make, so a census through it fails. That is fail-closed.
   - `aml_proof.rs:89` (`Boot`) and `aml_proof/tests.rs:376` (`Memory`) implement the trait for `lend`, `copy` and `now` only, and never run the census, so the default never runs for them.
   - One caution, not a finding: the default is fail-closed only because the census treats "not refused" as a failure. A future caller that used `guards() == false` as permission to lend would be fail-open. A line in the trait doc saying "never a precondition to lend" would fix the intent.
3. **The overlap set** (the RSDP, RSDT, XSDT, each listed entry and the DSDT, with entries whose signature is a changing structure's left out).
   - The membership is right for everything the kernel lends today. The census lends the RSDP's 36 bytes, the RSDT, the XSDT, every entry and the DSDT. `acpi_table` lends XSDT entries. The AML proof lends the DSDT and the listed SSDTs and PSDTs. RSDT entries are never followed.
   - Leaving out entries with a changing signature is right, since N3 never lends them.
   - The extents use the same length bound as the kernel's lends (`table_length`, at most 16 MiB).
   - **The priority is wrong (T71-9):** membership should trim a genuine structure's recorded range, not drop it.
4. **An unreadable target is neither recorded nor reported.** Acceptable, though the stated reason is slightly too strong.
   - A failed `copy(base, 8)` does not prove that no lend can cover another part of the structure. One case is a structure that straddles memory the map calls the kernel's, which is non-conforming for a FACS (§5.2.10). Another is a page-table mapping that fails at the first lookup and works later.
   - In both cases the census's own copy of the FACS fails, or its probe of a checked performance table finds no guard. Either way the census fails.
   - Keep it, and soften the reason to "and the census fails if it reads one".
5. **The ignored `FIRMWARE_CTRL` naming something other than a FACS is passed over silently.** Sound. §5.2.9 has that field ignored when `X_FIRMWARE_CTRL` is non-zero, and departure 2 already treats it so.
6. **A first FADT over 512 bytes is taken unchecked and noted; a first FPDT over 1 KiB is noted and its tables not located.** Sound. Both are gaps, so the census fails.
   - The only cost: a future ACPI revision with a FADT over 512 bytes would fail the census everywhere until the limit moves. ACPI 6.6's FADT is 276 bytes.
   - If the long first FADT is invalid, a later valid one is not examined, but the gap still fails the census.

## Records

**Accurate:**
- **ADR-0013's new rows AT-5 to AT-16 and AT-H1 to AT-H10** match the code by reading.
  - AT-8's count of 4 is right: two structures (recorded, and found by the census), with the middle and before probes not refused, and the start refused by N3.
  - AT-11's count of 2 is right.
  - AT-13's range `0x1fb7d0e8-0x1fb7d127` is right (base + 64 on a signature mismatch).
  - AT-14's range `0x1fb7d0e8-0x1fb7d153` is right: the XSDT's declared 108 bytes, for 9 entries.
  - AT-16 is right that `overlaps_any` serves both checks.
  - AT-H3 is reproduced here.
- **`ceac9b0f` and `7bfa7e69` share a tree** (`4eee8a56`). `cfa1c706..ceac9b0f` changes only `#[cfg(test)]` hunks in `acpi.rs` and `acpi_census.rs`, as claimed, so the boots on `cfa1c706` stand for the code.
- **The review file** (`6b192824`) is byte-identical to the report as delivered. Its Answers section summarises each commit correctly.

**Not accurate:**
- **T71-2.** The Answers section, TASKS' behaviour change 9, STATUS and the provenance entry describe the overlap rule exactly. With T71-9 they also describe a lend of changing memory as intended behaviour. STATUS's "nothing that overlaps one, wherever the range was named from" is not true for a structure passed over for overlap.
- **The F20 citation** (T71-10).

**Not verified:** the gate and boot figures on `cfa1c706` (507 s, 1,106 s, all 31 boots with `0 gaps`). They are the author's; I ran no boot.

## Checked and found sound (beyond the status table)

- **The registry.** `Lending::lend` now calls `Lending::guards`, which is the check the census asks, before the writable and slot checks. `firmware::guards` is `bytes`' `guard` plus `lend_guarded`, with no recording. The N3 four-byte copy inside it is a copy, not a lend.
- **The locator's first-valid rule** matches `find_table`, which takes the first entry with the signature whose `check_table` passes. An unreadable entry before the first valid FADT is noted, so it fails the census. Where the census's first checked FADT or FPDT differs from the locator's, it is because the crate refuses the locator's choice, and the census then fails on that refusal.
- **`ChangingRanges::push`** cannot overflow: there are five candidate slots, one per index. Its `None` arm is kept as a fail-closed note.
- **No new `unsafe`.** The SAFETY comment's edit in `timer.rs` is wording only and still true.
- **No new panic path in the crate's new code** (`dsdt_pointers`, `length_verdict`): every read goes through checked readers.

## Not covered

- No boot, `check` or `boot-matrix`. The kernel crate's host tests (the census tests) were not run here, nor Clippy.
- AT-5 to AT-16 at boot were not reproduced. Of the host controls, only AT-H3 was reproduced.
- Physical machines and KVM.

## Answers (2026-11-19, by the author's session)

Added after the report, which is above as received. The coordinating session accepted all three findings with rulings; each is answered by one commit on `claude/acpi-area-a-71`, and TASKS item 71's entry gives the detail ("The addendum review and its answers").

| Finding | Answer |
|---|---|
| T71-9 | `3d8fe56f`: trim, never drop. A target that passes the signature check is always recorded at its kind's minimum (64, 24 or 8 bytes), so a lent table over it is refused, as at `b5621ef2`; the rest of its declared extent is recorded up to the first lent table beyond the minimum. Every overlap and trim is kept, named by the census with both ranges, and fails its verdict. P4 is a host test in both crates, the trim another; the earlier FBPT test now records the FBPT up to the XSDT, and every lookup still works. Boot control AT-17 (in AT-14's style) is caught, and AT-17b shows the High at boot on `ceac9b0f`. The stated limit, a table listed inside a structure's declared extent beyond its minimum is lent and reported, is in `changing_ranges`' doc, STATUS and the records, and STATUS's sentence is corrected. |
| T71-10 | `ba9af2a4`: the citation is F14 and T71-3; no new task. |
| T71-11 | `c1ca3ed8`: after the first valid FADT and FPDT, every other FADT and FPDT is followed, valid or not, its targets recorded into the registry's spare slots; one with no slot left is counted and fails the census, and one that cannot be read or parsed is noted. Host tests for both outcomes; the compile-time check is `CHANGING_FIRST <= CHANGING_MAX <= NEVER_SLOTS`. |
| Decision 2's caution | `5c9648b5`: the census's `Firmware::guards` doc says it is a test of the guard, never a precondition to lend. |
| Decision 4 | Records only: the reason is softened to "the census fails if it reads one there". |
