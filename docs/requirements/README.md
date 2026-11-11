# The requirements matrix

CLAUDE.md §1 makes 100% coverage of approved requirements the completion target, "measured against a versioned requirements matrix". This directory is that matrix. The owner decided its shape on 2026-11-12 (decision items 191 to 196), and `cargo xtask requirements-gate`, which is part of `cargo xtask check`, holds it to the rules on this page.

| | |
|---|---|
| Directive revision | 7.6 |
| Version | The matrix is versioned with the repository: a commit's matrix is checked against that commit's directive, ADRs and tree. |
| The figure | Printed by `cargo xtask requirements-gate`, and not written here, so it cannot go stale. |
| Evidence so far | Host tests, QEMU, and guests under KVM in CI. Nothing has run on a physical machine (CLAUDE.md §74). |
| Review so far | The fields of every row are Claude's judgement. No person has reviewed the matrix, or anything it describes. |

## What a row is

One row for each statement of CLAUDE.md, and one for each decision of an accepted ADR (item 191). The tables here are the only copy (item 192): nothing generates them, and they are changed by hand.

**A statement of the directive** is found by rule, outside fenced code:

- a line of the header that starts with a bold label;
- a body row of a table;
- a list item, bulleted or numbered (a numbered item keeps its number);
- a paragraph that is not a lead-in.

A lead-in is a paragraph that ends with a colon. It is joined in front of each item of the list after it, or of the one paragraph after it: "Implement:" and "- Controller discovery." are the statement "Implement: Controller discovery." A paragraph that is only a bold label joins the paragraph after it. A lead-in with a code block after it is a statement by itself.

**A decision of an ADR** is a heading `### X1. Title` or `## Decision N: title`, a paragraph `**X1. Title.**` under "Decisions", a row `| X1 | … |` of a table with a `#` column and a "Decision" column, a numbered item under "Approved defaults", or an amendment that holds none of these. Only an ADR whose status is Accepted is read.

A row quotes what it stands for. A statement is quoted whole, so a change to it anywhere is seen. A decision is quoted by its title: its heading, its question, its leading bold phrase or its first sentence. The gate fails when a statement or a decision has no row, and when a row quotes what its source no longer says.

The specifications, the threat models and ADR-0013's list of boot proofs are cited by rows and are not split into rows. Neither are the decision sheet's items: rows name them (see "Decision items").

## The files

| File | Holds |
|---|---|
| `directive-NN-<name>.md` | The statements of one PART of the directive, in the directive's order. `00` is the header and `21` the final rules. |
| `adr-NNNN.md` | The decisions of one ADR, in the ADR's order. |
| `README.md` | These rules, the directive's revision, the scope changes and the decision items without rows. |

## The columns

| Column | What it holds |
|---|---|
| ID | `H-01` (header), `S33-07` (§33), `P9-01` (the opening of PART IX), `F-12` (final rules), `A05-C4` (ADR-0005's C4; `def4` is an approved default and `Am1` an amendment that is one decision). An ID is never used again: a new row of a section takes the number after the highest that section has had. |
| Requirement | The statement, whole, or the decision's title. A `\|` in it is written `\\|`. |
| Kind | `build`: something to build or deliver. `rule`: a standing rule on how the work is done. `info`: the statement asks for nothing; its row is kept and not counted. |
| Scope | The edition, then the environment its acceptance must be shown in: `both`, `desktop`, `server` or `eng` (the project's own process, tools and records), then `/`, then `host`, `emu`, `virt` or `hw`. `-` for an `info` row. |
| Owner | The number of the one accountable role in CLAUDE.md §1. |
| State | See "The states". `-` for an `info` row. |
| Implementation | Paths from the repository's root, separated by `, `. Each must exist. |
| Acceptance | What must pass for the row to be done. A row that is not started may have none (item 195). |
| Evidence | References the gate can find in the tree, separated by `, ` (item 194). See "Evidence". |
| Env | Where the row's evidence ran: the strongest environment that all of it has run in, of `host` (tests and tools on the development host), `emu` (QEMU's software processor), `virt` (a guest on a real processor under KVM, with QEMU's devices and firmware), `hw` (a physical machine). |
| Review | Who reviewed the implementation: `none`, `agent` (an AI reviewer, recorded in the tree) or `person:<name>`. |
| Limitations | What is missing, or what the evidence does not show. |
| Decided by | The decision items that approved or changed the row. For a decision of an ADR these may be the items that accepted its whole ADR. `-` where the text is older than the decision sheet or no item touched it. |

## The states

| State | Means | The row must have |
|---|---|---|
| `not-started` | Nothing is in the tree for it. For a `rule`: nothing in the tree checks it, though the rule binds. | No implementation, evidence, environment or review. |
| `not-verified` | Something is in the tree, and no evidence for it that the gate can find. | An implementation and acceptance criteria. No evidence. |
| `partial` | Part of it is implemented, with evidence. | An implementation, acceptance criteria, evidence, an environment, and limitations that say what is missing. |
| `built` | Implemented, with evidence, but short of the row's scope or of its acceptance criteria. Evidence from QEMU, for a row whose scope is hardware, is this. | The same as `partial`. |
| `done` | Its acceptance criteria pass within its declared scope (item 193). | An implementation, acceptance criteria, evidence, and an environment at least as strong as its scope's. |
| `removed` | A decision took it out of scope. The row stays (item 196). | The decision item, and a line under "Scope changes" that names the row. |

The coverage figure is the `done` rows over the counted rows, with `built`, `partial`, `not-verified` and `not-started` printed beside it. `info` and `removed` rows are not counted. `partial` and `not-verified` are not in the owner's answers: they come from the task's brief, "a requirement with no evidence in the tree is 'not started' or 'not verified'", and neither counts as coverage.

No kernel, loader or driver row can be `done` today: their scope is hardware, and their evidence is QEMU's and CI's virtual machines.

## Evidence

| Reference | Found when |
|---|---|
| `file:<path>` | The path exists. |
| `test:<file>::<name>` | The file holds `fn <name>(`. |
| `proof:<name>` | `framekernel/src/bootproof.rs` names a boot proof `"<name>"`. |
| `xtask:<command>` | `developer-tools/xtask/src/main.rs` has the command `cargo xtask <command>`. |

- The gate checks that evidence exists. Whether it passes is `cargo xtask check` and the boots.
- `virt` rests on CI's jobs that run `cargo xtask boot-matrix` under KVM (`.github/workflows/ci.yml`). Their results are in CI and in `docs/STATUS.md`, not in the tree's files.
- Under KVM the boot tests forgive four things that QEMU cannot show there, or that assume a software processor's timing: the last two steps of the VT-d pin proof, a small step of the TSC, a virtio-net ring that did not fill, and uneven fair shares (`kvm_forgives` in `developer-tools/xtask/src/main.rs`). QEMU's software processor proves those. A row whose proofs run in both says `virt`.
- A row whose evidence ran on hardware names its record under `docs/hardware/`. None exists yet.
- `agent` in the Review column means a record of a review covers the row's implementation: `docs/engineering/PRE_MERGE_REVIEW_2026-11-09.md`, `REVIEW_S3-5A_2026-11-10.md`, `REVIEW_S3-5B_2026-11-10.md` and `REVIEW_H2-STACKS_2026-11-10.md` in the same directory, or the two reviews of task 18b that `docs/engineering/TASKS.md` records. All are AI reviewers' work. The first read the code as it stood on 2026-11-09 and lists the files no reviewer opened: rows built from those files, and rows for work that came later with no review of its own, say `none`. Code the first review read and that has changed since, the move of the privileged code into the framework crate among the changes, has had no second reading.
- The second step-3 batch's work (memory objects, the kernel stacks' region and its slide, the kernel's random-number generator, events, ports and `clock_read`) is recorded in `docs/STATUS.md` under QEMU's software processor only. Its rows say `emu`, and so does a row that names one of its proofs beside older ones.

## Decision items

Every answered item of `docs/engineering/OWNER_DECISIONS.md` is named in the "Decided by" cell of at least one row, or is listed here (item 191). `none` means the item asks for nothing that a row could carry. `owed` means its rows are owed: the text that holds its decision is not in this tree yet.

| Item | Rows | Why |
|---|---|---|
| 18, 145 | none | Each is a permission to download named specifications and test vectors once. |
| 22 | none | A permission to push one branch and open one pull request. |
| 23 | none | A comparison of two AMD manuals with AMD's own copies, done once. |
| 70, 161 | none | How pull requests are offered: at 100 commits, and pull request 15 at 76. A rule of process that no statement of the directive or decision of an ADR carries. |
| 167, 190 | none | How task 18b and the randomized model of its policy land: through pull request 16. Rules of process. |

## Scope changes

One line for each approved change to what the product covers (item 196): the decision items, the date, what changed and the rows it touched. A row that a change took out of scope is `removed` and is named here. `A14-*` means every row whose ID starts with `A14-`.

| Item | Date | What changed | Rows |
|---|---|---|---|
| 1–4 | 2026-11-08 | OriginFS v0 is a read-only bring-up format that never ships in an approved release. Writing, crash consistency and integrity belong to a successor format with its own ADR (ADR-0006). | A06-* |
| 33–39 | 2026-11-09 | The SDK-only rule reaches every first-party client, not only the GUI: command-line tools, the shell, the terminal, the recovery console and system services (ADR-0011; CLAUDE.md 7.2, §48 and §70). | A11-N*, S48-*, S70-15 |
| 71–76, 81 | 2026-11-10 | Origin Server is added: an edition of the same release with the full native desktop, a KVM guest as its first platform, administration through the native GUI, and four SDK areas of its own (ADR-0014; CLAUDE.md 7.3). | A14-S*, H-01, S49-26, S49-27, S49-28, S49-29, S74-17, S82-15, S82-16, F-12 |
| 78 | 2026-11-10 | Secure Boot defaults to Origin's own keys, enrolled by users and OEMs. The first stage signed by Microsoft waits for an organisation that can hold that signature (ADR-0002 Amendment 1). | A02-B7, A02-Am1, S19-15 |
| 80 | 2026-11-10 | DisplayPort first. HDMI 2.1's high-rate modes wait until their licensing allows an open implementation (ADR-0015 D7). | A15-D7, S36-14 |
| 82–87 | 2026-11-10 | Every device standard is reached through the SDK in three layers, with dangerous operations behind rights of their own. Thunderbolt is part of the USB4 interface, radio drivers wait for a legitimate source, and USB4, Thunderbolt, Wi-Fi and Bluetooth come at M10 on physical hardware (ADR-0015). | A15-*, S31-17, S31-18, S31-19, S35-14, S54-19, S54-20, S82-17 |
| 88–90 | 2026-11-10 | The hardware scope narrows to Intel platforms of 2026 or later and AMD platforms of 2026 or later with Zen 5 or newer cores. Older machines are bench equipment. The kernel names the core generation and never refuses on it, which replaces ADR-0009 H2, and the installer's answer is decided at M9 (ADR-0009 Amendment 1). | A09-*, H-08, S24-18, S24-19, S24-20 |
| 117 | 2026-11-10 | "Administration only through the GUI" becomes "the GUI is always enough, and nothing needs a terminal". Remote desktop must exist before the Server edition ships, and unattended installs apply a signed provisioning document (ADR-0014 Amendment 1). | A14-A*, S48-14, S48-15, S48-16, S48-18 |

## What the gate checks, and what it does not

`cargo xtask requirements-gate` fails when:

- a file here is not a table with the columns above, or a cell holds a value the column does not allow;
- a row has no owner, or an owner that is not one of §1's roles;
- an ID is used twice, or a row is in the wrong file or out of its source's order;
- a statement or a decision has no row, or a row quotes what its source does not say;
- a row's state lacks what the table of states asks for, or names an implementation or evidence that is not in the tree;
- a row is `done` on evidence from a weaker environment than its scope;
- an answered decision item is neither named by a row nor listed above, or a row names an item the sheet does not hold as answered;
- a `removed` row is not named by a scope change with its item;
- the revision at the top of this page is not the directive's;
- it read no row at all.

It does not check whether an owner, a scope, a kind, acceptance criteria or a state is the right judgement, whether evidence passes, where evidence ran, who reviewed a row, or whether the body of an ADR's decision changed under an unchanged title.

## Changing the matrix

- **The directive changes.** The gate names each new statement, with the first cells of its row, and each row whose statement is gone. Change the quote of a statement that was reworded. Delete the row of a statement that was deleted, unless a decision took it out of scope: then mark it `removed`. Then change the revision at the top of this page.
- **An ADR is accepted or amended.** Add a row for each new decision to its `adr-NNNN.md`, or add the file.
- **The owner answers a decision item.** Name it in the row it decided, or list it above.
- **Work lands.** Change the row's state with the cells that state needs. Acceptance criteria come from the task's contract (CLAUDE.md §78).
- **A state never goes up on memory.** It follows what the tree shows: `docs/STATUS.md`, the boot proofs, the tests and the gates.
