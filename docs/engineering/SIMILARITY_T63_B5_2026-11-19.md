# Similarity check: TASKS item 63, step B5, against the ACPICA files read for TASKS item 64 (2026-11-19)

| | |
|---|---|
| Required by | CLAUDE.md §3 (the reference-reading split, and its third rule: the similarity check before the first code written this way is published, signed off by a person), §6 (provenance; no scan proves independent authorship); decision items 91 to 97 and 121; the coordinating session's ruling of 2026-11-19 that the check covers step B5 and only step B5 |
| Checked | Step B5 of the AML interpreter, as the coordinating session named it: `git diff 2d0187c0 f3aff5ef -- firmware/acpi/aml 'framekernel/src/aml_proof*'` on `claude/aml-tolerance`. That is 25 files, 4,973 lines added and 406 removed. `d99d1c56..2d0187c0` changes nothing under those paths, so the diff is B5's alone. Steps B1 to B4 were written before any ACPICA reading and are not in it. Read by `git show` and `git diff`; that session's worktree was not opened |
| Against | The 48 source files that §5 of `docs/research/2026-11-14-aml-gaps-acpica.md` lists: ACPICA release 20260930, `open-acpica/acpica` at commit `e85aa3bebaae81ed1e6d163511ac7e1eafe732cf`, under the BSD-3-Clause option of `BSD-3-Clause OR GPL-2.0-only`. The change log (`documents/changes.txt`) is not code and was not fetched |
| Checked by | The TASKS item 64 session, which read those files for the ledger and writes no Origin code, now or later. No person has reviewed this check |
| The download | The owner allowed it in the coordinating session on 2026-11-19: asked "Allow downloading ACPICA's 48 source files for B5's similarity check?", the owner answered "Allow download" (as the coordinating session relayed it). The 48 files were fetched on 2026-11-19 at 13:16 UTC from `raw.githubusercontent.com` at the commit above, with no other request and no project or personal details sent, into a new folder in this session's scratchpad, outside every worktree. 937,373 bytes; the SHA-256 of their sorted `shasum -a 256` manifest is `8d9f0621…7387bf00`. Nothing in that folder was run. **All of it was deleted after the check** (see the last section) |
| What this is not | Nothing was built, run or tested by this check apart from its own comparison scripts. It is evidence about text and structure, never about behaviour on a machine |
| Result | **No similarity to fix in any of the 25 files.** Where B5 and ACPICA agree, the agreement is a fact the ledger states, or a shape the AML encoding or the stated behaviour leaves no choice about. Two small rules B5 settled itself coincide with ACPICA, and are listed for the owner. One comment credits ACPICA's hosts with what the ledger says of ACPICA itself; that is wording, not similarity |

## What a similarity check cannot prove

No scan proves independent authorship (CLAUDE.md §6). This check looked for the marks copying would leave and found none. That is not proof that none of ACPICA's thinking reached the code another way. Three limits matter here:

- **C against Rust.** Text matching across two languages is weak. The token comparison below caught a planted, close translation only in part (control C4). The main method was reading the two side by side.
- **The writer's training.** The session that wrote B5 is a Claude model, trained on public code that likely includes ACPICA. §3's split, writing from the ledger alone, is what guards against that. This check can only look for drift in what was written.
- **Only these 48 files.** ACPICA files outside the ledger's §5 were not compared, and nor was any other project.

## Method

1. **The files.** Each file's line count is exactly one less than the ledger's §5 says, for all 48. The ledger counted lines in the browser, where the file's final newline added an empty last line. The line numbers the ledger cites are unaffected, and three were checked against the downloaded files: `acconfig.h` 43, `utglobal.c` 98, `nsaccess.c` 198. The ledger's "Lines" column wants one subtracted from each figure, a records correction left to the coordinating session.
2. **Four comparison aids,** short scripts in a scratch folder of their own (not kept in the repository; they read the files as data and run nothing from them):
   - **identifiers:** every identifier of two or more words in B5's added lines, split into words and compared with ACPICA's identifiers after removing ACPICA's component prefixes; exact word sequences, and any shared run of three words;
   - **prose:** runs of 4, 5 and 6 words shared by B5's added comments and strings and ACPICA's comments and strings, licence text excluded;
   - **code shape:** runs of 12, 16 and 24 tokens shared after reducing both languages to the same alphabet: names, numbers and strings each become one token; control-flow words and operators are kept; brackets and declarations are dropped;
   - **constants:** every number over 16 in B5's added lines, with where ACPICA uses the same value.
3. **Controls.** In a copy of B5's tree, four known copies were planted, then removed with the copy: C1, a function named after an ACPICA helper; C2, a copied ACPICA comment; C3, a copied ACPICA message; C4, a close Rust translation of ten lines of ACPICA's parse loop. Run on `743b4c3e` and again on `f3aff5ef`:

   | Control | Caught by | Result |
   |---|---|---|
   | C1 | identifiers | Caught |
   | C2 | prose, 6 words | Caught |
   | C3 | prose, 4 words | Caught |
   | C4 | code shape | Caught in part at 12 tokens (three runs on `743b4c3e`); missed at 16 and 24 |

4. **Reading.** Every non-test file of the diff was read in full, against the ACPICA files that cover the same gap, for three things: the same split into functions, the same order of steps, and comments or messages in ACPICA's words. The test files were read for their comments, names and fixtures. B5's code was first read at `743b4c3e`, then the difference to `f3aff5ef` (`b83fe91a`, `f182d374`, `c252b83a`, `83c76eb8`, `b028c14b` and their records).
5. **Facts.** Each behaviour B5 adds was traced to the ledger, to ACPI 6.6, or to a ruling of the coordinating session. Every mention of ACPICA in B5's added lines was checked against what the ledger says.

## What the aids found on `f3aff5ef`

- **Identifiers.** 340 identifiers of two or more words; 14 match an ACPICA identifier word for word, and none shares a run of three words without matching whole.
  - Of the 14, ACPI 6.6 names nine: `oem_id`, `oem_table_id`, `bit_offset`, `bit_length`, `SyncLevel`, `PackageLength`, `pkg_length`, `object_type` and `load_table`. They are the specification's OEM ID, OEM Table ID, field offsets and lengths, SyncLevel, PkgLength, ObjectType and LoadTable.
  - `load_op` is a test's name for `Load`.
  - The other four are plain words: `NotFound`, `table_count`, `field_value`, and `object_count`, a test helper.
- **Prose.** 19 runs of 4 words, 4 runs of 5, and 1 run of 6.
  - The 6-word run, "OEM ID and OEM table ID", names `LoadTable`'s parameters (ACPI 6.6 §19.6.78).
  - "separated by a space" and "string to integer conversion" are the specification's terms for the conversions.
  - The rest is ordinary English: "at the end of the", "as long as the field", "it is an error", "if there is one".
- **Code shape.** 6 runs of 12 tokens, none of 16 or 24, and none of them real:
  - a closure's `|` against a C macro's bitwise `|` (`tolerate.rs` 465; `amlcode.h` 312);
  - Rust generics' angle brackets against a C `#include` (`lib.rs` 190–199; `oswinxf.c` 23–24);
  - a chain of `!=` comparisons in `tolerate.rs`'s identity check against an unrelated handle check (`tolerate.rs` 410–414; `exconfig.c` 738–742).
- **Constants.** Outside the tests, the values B5 shares with ACPICA are fixed elsewhere:
  - AML opcodes (0x30, 0x31, 0x5B, 0x60, 0x6E, 0xA0) and 0x7F, which ACPI 6.6 §20 fixes;
  - 100 and 255 for `Stall`, and 255 for an Embedded Controller transfer, which the ledger states (G5, G7);
  - a shift mask of 63.

  The test constants are fixture values, and G15's 0x44434241 is the ledger's worked example.

## Verdict for each file

"Forced" means the agreement is a behaviour the ledger states or ACPI 6.6 defines, or a shape that behaviour leaves no choice about. ACPICA citations are file and lines at `e85aa3b`, under `source/`.

| File | B5's change | Compared with | Verdict |
|---|---|---|---|
| `firmware/acpi/aml/src/tolerate.rs` (new) | The load policy: what fails a table whole; finding the block a failure abandons; unwinding; skipping the statement with a work list; reports and their bound; the identity check for a table already loaded; the binding pass over load-time package names, with a reachability marking | `components/parser/psloop.c` 300–460, `psobject.c` 580–639; `components/dispatcher/dspkginit.c` 147–161, 226–236, 437–490; `components/namespace/nsinit.c` 48–107, 426–446, `nsload.c` 62–68; `components/tables/tbdata.c` 38–82, 457–520 | **Forced, in one place; otherwise no similarity found.** The one place is skipping an `Else` after an abandoned `If`: B5 reads the next byte, steps over the `Else`'s package length when it is one, and checks the end against the range. That matches ACPICA's steps in `psloop.c` 421–456, but the ledger states the behaviour (§2) and the AML encoding allows only that shape: an `Else` is a separate opcode, with its own package length, after the `If`. Everything else differs in design: <br>• ACPICA completes the failed operation and resumes parsing where the failure stopped. B5 unwinds its own frames and parses the statement again from its start, running nothing, which the file records as Origin's choice. <br>• ACPICA resolves deferred package elements by walking the namespace for packages. B5 keeps a list of element objects with a bind state, and a marking from the collector's roots decides what is still readable; ACPICA has no collector. <br>• ACPICA spots a duplicate table by comparing the whole table at install time (`tbdata.c` 38–82). B5 compares signature, IDs and term list, and not the header's revisions or creator. <br>• The bounded reports have no counterpart in ACPICA. |
| `firmware/acpi/aml/src/engine.rs` | Statement start kept in each block frame; the abandon hook in the run loop; boot-table locals refused as `OutsideMethod`; string literals over 0x7F and unterminated ones, reported; extra package elements decoded, dropped and reported; deferred list and collector changes | `components/parser/psargs.c` 545–582; `components/dispatcher/dspkginit.c` 259–312 | **No similarity found.** The behaviours are the ledger's (G13, G14). Two places differ from ACPICA by design: <br>• B5 never writes into the table, where ACPICA ends an unterminated literal by writing a zero into the table's own bytes. <br>• ACPICA runs a table as a pseudo-method, so its table-level code has locals. B5 refuses them. <br>The report of extra elements carries both counts, as ACPICA's message does, because the ledger states that fact. |
| `firmware/acpi/aml/src/convert.rs` | G4's texts; string to integer as a total function; string to buffer with its NUL | `components/executer/exconvrt.c` 233–255, 286–403, 519–638; `components/utilities/utstrtoul64.c` 91–364, `utstrsuppt.c` 244–330, 394–520, `uthex.c` 14–60 | **Forced.** <br>• **Texts:** they are the ledger's (G4). <br>• **Order of steps:** white space, then `0x`, then leading zeros, which ACPICA follows too (`utstrtoul64.c` 112–134, 238–250, 327–341). It is the ledger's order and the only one that works: zeros cannot go before the prefix, which begins with one. <br>• **Hex without leading zeros:** the usual loop from the high digit down, as ACPICA writes it too (`exconvrt.c` 360–399). B5 keeps the last digit inside its loop, where ACPICA adds a zero after it. <br>• **Buffer texts:** room for five characters a byte, as in ACPICA (`exconvrt.c` 568, 582), is what "0x", two digits and a separator take. B5 puts each separator before a byte; ACPICA puts it after and trims the last. <br>• **Overflow:** B5 stops with checked arithmetic and a width mask; ACPICA has helpers of its own (`utstrsuppt.c` 394–520). <br>• **White space:** B5's set is a fact the ledger does not state (see below). |
| `firmware/acpi/aml/src/load.rs` | One key builder for `LoadTable` and `DataTableRegion`; G18's comparison; signature characters; a table already loaded refused; failed loads report releases before names go, bind their names, and keep their bytes until no listed name needs them | `components/tables/tbfind.c` 40–141; `components/executer/exconfig.c` 257–306, 330–402; `components/dispatcher/dsopcode.c` 475–560 | **Forced.** The rule is the ledger's (G18): IDs padded with zeros, compared exactly, an empty ID matching any table, and four name characters in the signature, which ACPI 6.6 §20.2.2 defines. B5 takes an all-zero key as empty; ACPICA tests the first byte. These are equal for AML strings. That `DataTableRegion` follows the same rule is B5's own decision, and it coincides with ACPICA (see below). The release and binding order on a failed load is Origin's. |
| `firmware/acpi/aml/src/region.rs` | `AnyAcc` as byte access; an Embedded Controller field wider than 8 bits as one host transfer of up to 255 bytes, merged by the update rule; a string written as a buffer of its bytes, in pieces when longer than the field, reported | `components/executer/exprep.c` 230–242, 503–518; `components/executer/exfldio.c` 586–675, 877–1008; `components/executer/exfield.c` 417–441 | **Forced.** The widths and the 255 come from the ledger (G5). <br>• **Embedded Controller fields:** ACPICA widens the field's access width to the whole field and keeps its usual path. B5 takes a separate path, passing the field's bit offset and length to the host with bytes already merged. <br>• **Strings to fields:** B5 differs from ACPICA on purpose (Table 19.7's pieces, by the coordinating session's ruling) and reports each such store. |
| `firmware/acpi/aml/src/sync.rs` | A mutex outlives its method until the outermost evaluation ends, then is released and reported; the method that took it recorded; `Stall` over 100 µs reported | `components/executer/exmutex.c` 162–276, 397–447, 489–555; `components/parser/psparse.c` 716–718; `components/executer/exsystem.c` 126–176 | **Forced.** When the release happens, and the 100 and 255 µs limits, are the ledger's (G2, G7). The mechanism, a list of holds with marks per evaluation, is B5's and earlier steps', unlike ACPICA's per-thread mutex list. |
| `firmware/acpi/aml/src/store.rs` | A zero-length named buffer takes the source's length | `components/executer/exstorob.c` 36–125 | **Forced** (G11). B5 replaces the object; ACPICA allocates new storage inside it. |
| `firmware/acpi/aml/src/host.rs` | `Report` and `Host::report`; `BufferAccess` documented for Embedded Controller fields; the host's table search for G18 | `components/tables/tbfind.c` 93–141 | **No similarity found.** The report interface has no counterpart in ACPICA. **Wording:** one comment says the host makes an Embedded Controller field one transaction "as ACPICA's hosts do". The ledger says that ACPICA makes it one access (G5), and nothing about its hosts. "As ACPICA does" would match the ledger. |
| `firmware/acpi/aml/src/exec.rs` | `ToHexString` of an integer through the short text; `Acquire` passes its name; `DataTableRegion` through the shared key | `components/executer/exconvrt.c` 449–517 | **No similarity found.** It calls B5's own helpers. |
| `firmware/acpi/aml/src/error.rs` | `MutexHeld` removed, `OutsideMethod` added; comments cite the ledger | None needed | **No similarity found.** The kinds are Origin's. |
| `firmware/acpi/aml/src/value.rs` | An evaluation that ends holding a mutex succeeds and reports | `components/parser/psparse.c` 716–718 | **No similarity found** (G2). |
| `firmware/acpi/aml/src/init.rs` | `initialize` binds deferred names first | `components/namespace/nsinit.c` 48–107 | **No similarity found.** |
| `firmware/acpi/aml/src/lib.rs` | Module text; `TableInfo.abandoned`; run counts; the deferred list | None needed | **No similarity found.** |
| `firmware/acpi/aml/src/object.rs` | Bind states for names in packages; heap queries for the marking | None needed | **No similarity found.** |
| `firmware/acpi/aml/src/osi.rs` | `_OS` is "Origin OS", by decision item 315 | `include/acconfig.h` 35–43 | **No similarity found.** The value differs from ACPICA's. |
| `firmware/acpi/aml/src/probe.rs` | The probe host's `report` | None needed | **No similarity found.** |
| `firmware/acpi/aml/src/tests/tolerance.rs` (new) | B5's tests and controls | Fixtures checked against the ledger's examples | **No similarity found.** AML is built with Origin's own builders; the comments cite rulings and the ledger. |
| `firmware/acpi/aml/src/tests/host_ops.rs`, `tests/load.rs`, `tests/mod.rs`, `tests/namespace.rs`, `tests/random.rs`, `tests/semantics.rs` | Earlier steps' tests brought to B5's rulings; the test host's reports and access log; planted faults; the soak's audits | None needed | **No similarity found.** |
| `framekernel/src/aml_proof.rs` | The boot proof counts reports and fails on a tolerance or a store in pieces on QEMU's tables | None needed | **No similarity found.** |
| `framekernel/src/aml_proof/tests.rs` | Host tests of those failures and of the binding pass | None needed | **No similarity found.** |

**Similarity to fix: none.**

## Facts in B5 that the ledger does not state

None of these points to another way from ACPICA into the code. Two coincide with ACPICA, and the owner may want to know that before signing.

1. **The white space a string-to-integer conversion skips** is ASCII space and 0x09 to 0x0D (`convert.rs`).
   - The ledger says only that white space is skipped.
   - B5's contract chose the set openly, as one of the small rules the ledger leaves open (its Q8), and the coordinating session accepted it.
   - It coincides with ACPICA, which uses the C library's white-space test (`components/utilities/utstrsuppt.c` 273–283). That test's usual definition is the same six characters.
2. **`DataTableRegion` uses G18's rule** (`load.rs`, `exec.rs`).
   - The ledger covers `LoadTable` only.
   - B5's contract (Q8) applied the rule to `DataTableRegion` because Origin gives both the same host call.
   - It coincides with ACPICA, whose `DataTableRegion` searches with the same routine as `LoadTable` (`components/dispatcher/dsopcode.c` 540).
3. **An empty string becomes a buffer of one zero byte.** This follows from the ledger's rule that a string's terminating zero is copied (G4), and matches `exconvrt.c` 233–255.
4. **A signature shorter than four characters is refused.** This follows from the ledger's rule that the first four characters must be name characters (G18).
5. **What "already loaded" means** is B5's own: signature, IDs and term list identical, the header's revisions and creator not compared. The ledger says only that a loaded table cannot be loaded again. ACPICA's own checks differ: by table index (`nsload.c` 62–68) and by the whole table (`tbdata.c` 38–82).

Every mention of ACPICA in B5's added lines was checked against the ledger. Each matches a fact the ledger states, except the `host.rs` wording above:
- G2 and G6 as ACPICA's defaults;
- G14's refused expression elements;
- G15's cut;
- G1's rule that a call happens only where one is possible.

## Behaviour that differs from ACPICA, outside the question of similarity

These were met while comparing. They are not similarity findings. They are listed so that the coordinating session can decide whether its records should name them.

- **An unterminated string literal.** ACPICA writes a zero into the last byte of the remaining range and ends the literal there, so the literal is one byte shorter than the range (`components/parser/psargs.c` 559–580; ledger G13). B5, as ruled, writes nothing and ends the literal at the range's end, so its literal holds one byte more. The ruling says "nothing in the AML is written"; it does not say that the length differs.
- **An Embedded Controller field over 255 bytes.** ACPICA makes an Embedded Controller field one access only when it is 255 bytes or fewer; a wider one keeps its ordinary access width, with no error (`components/executer/exprep.c` 503–518). The ledger states only the 255-byte limit (G5). B5's contract (Q6) refuses a wider field with `Range`.
- **`Stall`'s operand.** ACPICA cuts the operand to its low 32 bits before checking it against 255 (`components/executer/exoparg1.c` 166–169; ledger G7). Origin refuses any value over 255, as it did before B5; G7 was ruled unchanged.

## The downloaded files

Deleted on 2026-11-19 at 15:34 UTC, after the check:
- the folder of 48 ACPICA files;
- the two scratch copies of B5's tree with the planted controls;
- the aids' outputs, which held ACPICA phrases and line references.

The manifest's digest above is kept so that a later check can confirm it read the same bytes. Nothing from ACPICA is in the repository: this report names short identifiers only where a match needed naming, and quotes no code.

## Sign-off (CLAUDE.md §3, decision item 121)

| | |
|---|---|
| Checked by | The TASKS item 64 session (an AI that writes no Origin code), 2026-11-19 |
| Signed off by a person | Pending: the owner |
