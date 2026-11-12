# The design documents read against the milestone exit tests, 2026-11-12

| | |
|---|---|
| Asked for | The owner, to the coordinating session: "Have you checked end to end of CLAUDE.md and it's design and review documents completely?" The answer was no. Then: "Yes, read them" |
| Why | Decision items 252 to 263 (milestone exit tests, and two dates in ADR-0009) were recommended from CLAUDE.md and the requirements matrix alone. The session had not read the accepted ADRs, the threat models or the specifications in full |
| Read in full | CLAUDE.md 7.6. `DESIGN_REVIEW_2026-11-10.md`, `PRE_MERGE_REVIEW_2026-11-09.md`, `REVIEW_S3-5A_2026-11-10.md`, `REVIEW_S3-5B_2026-11-10.md`, `REVIEW_H2-STACKS_2026-11-10.md`, `ROLES_CHECK_2026-11-11.md`. ADR-0001 to ADR-0017 with their amendments, on `main` at `418aeee`. ADR-0018 on `origin/docs/adr-0018` (pull request 22: accepted, not on `main`). The four threat models. `docs/spec/`: `ONBF.md`, `ONBF-errata.md`, `ONBF-manifest-keys.md`, `ONBF-signature-encoding.md`, `OIDL.md`. The differences of those files on `origin/land/s4` at `5ea97ef` |
| Not read in full | The decision sheet before item 224; `STATUS.md` past its top; `SOURCE_PROVENANCE.md`; the 32 research ledgers; the source, except where a finding had to be checked |
| Checked against | Items 252 to 263; `docs/ROADMAP.md` at `670c53f` (TASKS item 25, not landed); the briefs and contracts of TASKS items 16, 25 and 41 (S4-1); S4-1's worktree at `89a3676` with its uncommitted launch path |
| How | One AI session read each document from first line to last and wrote a note after each. Every finding below was then checked in the file it cites. The row counts come from a script that compares the roadmap's row IDs with the matrix's |
| Not done | Nothing was built or booted. No vendor document was read again. **No person has reviewed this** |
| Outcome | Eleven findings. Five needed the owner's answer: decision items 264 to 268, which the owner accepted on 2026-11-12 ("Accept all 5"). The rest are corrections to records, or notes for tests that are not accepted yet. `docs/ROADMAP.md` changes with TASKS item 42 |

## Result

The three task briefs hold up against the accepted documents. Nothing in ADR-0001 to ADR-0018 forbids what TASKS 16, TASKS 25 or S4-1 build.

The milestone exit tests do not hold up as well. They were written from §82 and the matrix's directive rows, and four accepted documents say things about milestones that the tests do not carry: `THREAT_MODEL_SYSTEM.md`'s table "What must exist, and when", ADR-0009 H6, ADR-0017 and `ONBF.md` §13.

One statement the session made to the owner was wrong (F4).

## Findings

Severity says when: **High** before the decision it bears on is relied on; **Medium** before the milestone named; **Low** is a record to correct.

| # | Severity | Finding | Where | Proposed |
|---|---|---|---|---|
| F0 | High | **The roadmap gives no milestone to 842 of the matrix's 1,651 rows.** Standing rules belong to none by its rule 3. But about 250 `build` rows of the directive are in none either, with nearly every ADR's decision rows. Examples: copy-on-write, demand paging, memory-mapped files, the page cache, NUMA policies, large pages, OOM handling and reclamation (S28-08, S28-09, S28-11 to S28-16); `CONTRIBUTING.md`, `SECURITY.md` and `CODEOWNERS` (S6-17, S6-18); §49's SDK areas; §68 but for S68-08; §38; §70 to §73; §13, §14, §18. Item 256 says a row no sentence of §82 names goes to the milestone that names its subject, else M13. So M0 (item 252) and M2 (item 254) are incomplete as accepted. No gate reads the roadmap | `docs/ROADMAP.md` at `670c53f`; `docs/requirements/` | Item 264 |
| F3 | Medium, before M4 or M5 is judged | **M4-7 and M5-6 each name "a physical machine in scope". Their rows need one of each vendor.** M5-2 names VT-d and the AMD IOMMU, which are two rows (S32-07, S32-08). M4-5 names rows A17-*, and Z6 (FRED, else the IDT path), Z7 (LASS, Intel only) and Z12 (the nested-NMI guard, on the path Zen 5 uses) cannot all be shown on one vendor's machine. By item 255 the other vendor's rows stay "built". M3-6 already asks for two machines | Items 260 and 261; ADR-0007; ADR-0017 Z2, Z5, Z6, Z7, Z12 | Item 265 |
| F4 | High | **The session told the owner that item 255 could be met by "rental C". That is wrong.** Rental C is an EPYC 9275F, an EPYC 9005: Zen 5 from 2024, which ADR-0009 Amendment 1 A1 names as outside the scope. ADR-0011 L9 says no machine in the declared scope can be rented, and ADR-0009's update of 2026-11-09 says buying is off. So under item 255 no milestone from M1 on can be marked done until a volunteer's machine or a bought one exists. The alternative in TASKS 25's proposal (done for a declared virtual scope now, hardware at M13) was not shown to the owner. ADR-0014 S3 already gives the Server edition a KVM guest as "its own declared scope", but names no exact scope: QEMU's version, the machine type, the devices and the host's processor generation are not declared | The session's message of 2026-11-12 on items 252 to 258; ADR-0009 Amendment 1 A1; ADR-0011 L9; ADR-0014 S3 | No item: 255 is consistent with §74 and final rule 30 as it stands. The owner may reopen it. The sheet's line "None is owned or rentable within item 225's cap" now reads "none in the declared scope can be rented" |
| F9 | Medium, before M9's tests are accepted | **Item 253 sent Secure Boot to M12. Two accepted documents need it by M9.** `THREAT_MODEL_SYSTEM.md`, "What must exist, and when": before "Origin can be installed (M9)" come storage encryption, accounts and credentials, and "the Secure Boot keys". ADR-0002 Amendment 1: "The installer must guide this step". The roadmap's proposed M9 tests do not mention it; S19-12 and S19-15 sit in M12-5. Separately, `ONBF.md` §12 has the loader verify KRN **and SYS** "by the end of M1–M2", and §8.1 has it check the rollback floor and measure into the TPM; M1 as accepted has KRN's verification only, and ADR-0002's Amendment 2 for the rest is unwritten (TASKS item 28) | Item 253; `THREAT_MODEL_SYSTEM.md`; ADR-0002 Amendment 1; `ONBF.md` §8.1, §12 | Item 266 |
| F11 | Medium, before M6 is judged | **`ONBF.md` §13 is headed "Test obligations (exit criteria for M6/M7)" and the roadmap never cites it.** Its nine obligations include Kani on `onbf-format`, a second parser written independently by a different agent, executing from an unregistered root denied, a tampered byte giving an integrity fault and a repair, a power cut at every install step, and cold launch at most 5% slower. The owner accepted the specification on 2026-11-07. Item 262's M6-2 says only "verifies and loads an ONBF executable, refuses malformed and hostile images, and is fuzzed". The Code Integrity Table and sealed objects (§8.2) are specification rows and so are in no milestone (F0) | Item 262; `ONBF.md` §8.2, §13 | Item 267 |
| F1 | Medium, before M1 is judged | **M1-1 does not say which key.** It says the loader verifies the kernel's "signature outside developer mode". ADR-0002 B8 and errata V4: a loader without the `test-trust-store` feature trusts nothing until Origin's own kernel key exists, and that key waits for the signing ceremony (§60, TASKS item 6), which the system threat model puts before the first public release | Item 253; ADR-0002 B8; `ONBF-errata.md` V4 | Item 268 |
| F10 | Medium, before a first boot on a machine in scope | **A boot on a machine in scope has prerequisites the roadmap does not state.** ADR-0009 H6 and `THREAT_MODEL_IOMMU.md`: T4's two windows, T5, T6, T8, T9 and T23 are closed or accepted by name before the first boot, and T14 is closed. Item 230 accepts them "for bench boots on rented servers only" and says the acceptance "does not reach a volunteer's machine". `THREAT_MODEL_SYSCALLS.md`: E12 (the nested NMI) and I2 before any claim about real hardware. `THREAT_MODEL_ADDRESS_SPACES.md`: T10, which only hardware shows | Items 253, 254, 259 to 263 | The roadmap states them under each hardware test. No new decision: they are accepted rules. When a machine in scope exists, accepting T4 to T23 for it is the owner's |
| F2 | Low | M4-1 ("processes are created through the kernel's calls") rests on ADR-0005 G3 and G4, which are drafts decided again with step 5 and with the Loader Service. So M4 cannot close before part of M6's work | Item 260; ADR-0005 Amendment 8 | A line in the roadmap |
| F5 | Low | M2-6 says "each boot proof's negative controls fail the boot" without saying where. ADR-0013's controls are temporary edits booted under QEMU | Item 254; ADR-0013 | The roadmap says: controls under QEMU, proofs on the machine |
| F6 | For M8 and M9's tests, which are not accepted | The roadmap puts all of USB (§34) in M10. ADR-0014 S3 has the server's GUI take input "from a USB keyboard and tablet through xHCI", that GUI comes with M8, and an installer on a real machine needs a USB keyboard. ADR-0015 D9 says xHCI comes first | ADR-0014 S3; ADR-0015 D9 | Settle it when M8 and M9's tests are put to the owner |
| F7 | Low | ADR-0017's status line says N14 step 4 "now waits for them to be run on a processor that has the controls". Item 213 (ADR-0011 L9) decided that step 4 goes ahead with I2 at "Partly". The line is stale on `main` and on `land/s4` | ADR-0017, status | A dated note citing item 213. No decision changes |
| F8 | Low | ADR-0018 V3 has every decision accepted from here on state its performance cost. The records of items 252 to 263 say nothing. They change documents only | ADR-0018 V3, V8 | One line in the sheet: "none, by construction". Every later ADR or amendment carries the row |

## Checked and found consistent

- **S4-1 (TASKS item 41).**
  - Its userspace flags match CLAUDE.md §11 and §14 and ADR-0001's defaults 7 to 9: `x86-64-v3`, frame pointers, `panic=abort`, a PIE, full RELRO, no executable stack.
  - Its runtime reads no environment (ADR-0005 E2; ADR-0011 M5).
  - Its launch draws from `origin_frame::random` and nowhere else (ADR-0016 R11; decision item 113).
  - Its ELF reader refuses all nine of `ONBF.md` §6.1.2's hard errors. The step-4 ledger's exit test names six of them.
  - Programs carried in the kernel image, with nothing parsed at run time, are what ADR-0011 L1 and the note to `ONBF.md` §8.3 accept.
  - Its program bases lie inside ADR-0004 H1's user range and below ADR-0005 D5's range for random mappings.
- **TASKS item 16.** Moving `nospec::choose` into the CPU identification table is what ADR-0017 Z9 and its migration list say. The scope line never refuses a boot (ADR-0009 A2).
- **TASKS item 25.** The provenance table and the contract template are what decision item 122 and finding 17 of the design review ask for.
- **Items 259 and 263.** ADR-0009 H4's two first machines are one with hybrid cores and one with SMT, as M3-6 asks. ADR-0015 D6 and D9 match M10's tests and the note the owner was given on Wi-Fi and Bluetooth.
- **Item 263's M10-5.** ADR-0012 C7 requires the clock to be re-anchored on resume.

## Decisions put to the owner

**Accepted by the owner on 2026-11-12: "Accept all 5".** Each had a recommendation. Performance cost of all five (ADR-0018 V3): none, by construction. They change documents and one gate on the build host.

| # | Question | Recommended |
|---|---|---|
| 264 | Have TASKS item 25's session place every counted `build` row of the matrix in one milestone, or list it as belonging to none with the reason, and add a gate in `cargo xtask check` that reads `docs/ROADMAP.md`? Where that changes an accepted test, the change comes back as one item (F0) | **Yes** |
| 265 | Do M4-7 and M5-6 each ask for one Intel and one AMD machine in scope, as M3-6 does (F3)? | **Yes** |
| 266 | Does Secure Boot with Origin's own keys (S19-12, S19-15) move from M12 to M9, where the installer guides enrolment, with a test key until the ceremony? The release manifest's verification, the rollback floor and measured boot stay in M12 with ADR-0002's Amendment 2 (F9) | **Yes** |
| 267 | Do M6's and M7's tests take `ONBF.md` §13's nine obligations by reference, each placed in M6 or M7 by its subject and none dropped (F11)? | **Yes** |
| 268 | Is M1-1's signature check met with the test trust store, on a build that says so at every boot, with Origin's own kernel key belonging to the ceremony before the first public release (F1)? | **Yes** |

## What this does not show

- That the list is complete. One reader made one pass.
- Anything about the documents not read in full, named at the top.
- That any accepted decision is right. This compares documents with each other.
