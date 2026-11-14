# The requirements matrix

CLAUDE.md §1 makes 100% coverage of approved requirements the completion target, "measured against a versioned requirements matrix". This directory is that matrix. The owner decided its shape on 2026-11-12 (decision items 191 to 196, and items 221 to 223 for three things that building it added), and `cargo xtask requirements-gate`, which is part of `cargo xtask check`, holds it to the rules on this page.

| | |
|---|---|
| Directive revision | 7.9 |
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

A row quotes what it stands for. A statement is quoted whole, so a change to it anywhere is seen. A decision is quoted by its title (item 223): its heading, its question, its leading bold phrase or its first sentence. The gate fails when a statement or a decision has no row, and when a row quotes what its source no longer says.

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
| Acceptance | What must pass for the row to be done. A row that is not started may have none (item 195). Many rows share one criterion today: see "Acceptance criteria". |
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

The coverage figure is the `done` rows over the counted rows, with `built`, `partial`, `not-verified` and `not-started` printed beside it. `info` and `removed` rows are not counted. `partial` and `not-verified` are not in the owner's first six answers. They were added while the matrix was built, from the task's brief ("a requirement with no evidence in the tree is 'not started' or 'not verified'"), and the owner accepted them as decision item 221. Neither counts as coverage.

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
- A row whose evidence ran on hardware names its record: a file under `docs/hardware/records/` that holds a table with a row for each of Machine, Firmware, Source revision, Date and Result. The gate reads it. No other file under `docs/hardware/` is a record, the shortlist of machines to buy among them, and no record exists yet.
- `agent` in the Review column means a record of a review covers the row's implementation: `docs/engineering/PRE_MERGE_REVIEW_2026-11-09.md`, `REVIEW_S3-5A_2026-11-10.md`, `REVIEW_S3-5B_2026-11-10.md`, `REVIEW_H2-STACKS_2026-11-10.md` and `REVIEW_T58_2026-11-14.md` (ADR-0008 Amendment 1's rows, D1 to D8, as built by TASKS item 58) in the same directory, or the two reviews of task 18b that `docs/engineering/TASKS.md` records. All are AI reviewers' work. The first read the code as it stood on 2026-11-09 and lists the files no reviewer opened: rows built from those files, and rows for work that came later with no review of its own, say `none`. Code the first review read and that has changed since, the move of the privileged code into the framework crate among the changes, has had no second reading.
- The second step-3 batch's work (memory objects, the kernel stacks' region and its slide, the kernel's random-number generator, events, ports and `clock_read`) is recorded in `docs/STATUS.md` under QEMU's software processor only. Its rows say `emu`, and so does a row that names one of its proofs beside older ones.

## Acceptance criteria

A row past `not-started` has acceptance criteria (item 195), and today most rows share theirs. The gate prints how many different criteria the rows have between them and how many rows the commonest one serves. When the matrix was brought up to the second step-3 batch, the 594 rows past `not-started` had 111 different criteria, and one sentence served 276 of them: "Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot." That sentence says where a kernel row must be shown, and not what must be shown for that row. "Implement: Flushes." cannot be judged done by it.

A row gets a criterion of its own when the task that will finish it writes its contract (CLAUDE.md §78). The contract's acceptance conditions name the rows they complete, and each of those rows takes the condition that is its own, in place of the shared sentence. A row whose scope is hardware is not to be marked `done` on the shared sentence alone.

## What the rows do not quote

A row quotes a statement. The directive's headings, its code blocks (§8's `rust-toolchain.toml` and §81's tree) and its tables' header rows are not statements, and no row quotes them. They are held another way, so that what a row is stays as item 191 decided it: for each PART the gate takes those lines in order, with the fences and the rules between sections, and compares their SHA-256 with the one recorded here. A change to a heading or a code block fails the gate until someone has read it against that PART's rows and recorded the new digest. `00` is the header and `21` the final rules. With the rows' quotes, this holds every line of the directive that is not blank.

| Part | Headings and code blocks |
|---|---|
| 00 | 8fb3e01751526add5d63d5d517a8f8621660dda3b500f24f7c4f076a9a244138 |
| 01 | 9574396467ce88db554ffc3f9463d575fadd2d4a78e739c7e2f9c8ab2f8e252f |
| 02 | 1da37825b5c4a35fe5f1408b44dc4a2e9e9f44412a1d2079addc78cfc43bb5cc |
| 03 | 8db217ade4bd8371cae157a19d571cc0d9083f72209dff868794cb40b6c59e4c |
| 04 | 0c31aadbb4e67324b30a914452e8fb910f3ae2910c0c95da512ec18a0d316935 |
| 05 | 21101169dacc3ce7ef8852f62b5d41f03bba5a1fc3599bda9762c299ded41c8c |
| 06 | 0727dd4c05d1e5b9d06349d50ea10275424260949f1e0e5ede3f1f11c53db918 |
| 07 | 76eafee36394c6391c5b2645005aa7d409b69ec228a71df1539c7ae7aaf8489d |
| 08 | c1fae620c99305460bdca8acb6ba87ed048b5010af7be38c6bd656d6368a0268 |
| 09 | 254f5e753bfb1ce41ef2582a2d65c600f2009bd1e30d334dc0a13f958e402595 |
| 10 | 91b5dbfd1e2a35835d587714f660cf08c2d48e0d3db2384e19002ac675709894 |
| 11 | 7e08edbdda7b69aa4fe9f29df35881cd940900621b344903f2a5d54f167b6062 |
| 12 | ba9a64354cfa536144fc4db510b3bafa08f7214027919fc7b174ecd84806fa13 |
| 13 | 7672b9b030938006495901222993d738724455d8bfb74ebb62e86ad386edc551 |
| 14 | 224a445a3fd7c0c441625ad45f722e4665a5fdecdb5bd03c01365a3278345c99 |
| 15 | 1b7bf177601008c3e66c0a05dbd491f28a3930a68e7c18492617fbe1322358ae |
| 16 | ff95730353156b1a0c4288535e73052866023a6a4e6429980358a1cd61e75fc2 |
| 17 | 3f01f61de978143238058df16eb60f850d0cb390330e486d2b0ddb154051c193 |
| 18 | 56fb4a2251f330f8f40de34917084cee0639afcb1dfb994e365171e91daa9960 |
| 19 | 4bc703e94e3122c59df2beb2c93b9a2f0d02109d4d6874472255dcb036f5e5f4 |
| 20 | 85e1453a710b02df34d8760c6d774d407ec4b617b87af3f798d0a69e5e685388 |
| 21 | 6670da65798f7994914b091f524374f1eb2308d2fd8e26efadd7074ffe3caf45 |

## The rows that move the figure

Two cells move the figure by themselves. A row marked `info` leaves the count, and a row marked `done` joins the rows that are covered. The gate does not judge either cell, so each takes two edits: the row's own, and its ID in the lists here. A row that is `done` or `info` and is not listed fails the gate, and so does a listed row that is not.

| List | Rows |
|---|---|
| done | H-05 |
| done | S1-03, S5-01, S5-10, S6-01, S6-02, S6-03, S6-04, S6-05, S6-06, S6-07, S6-08, S6-09, S6-10, S6-11, S6-13, S8-01, S8-02, S8-04, S8-06, S8-08, S9-11, S10-04, S10-06, S11-01, S11-02, S12-01, S13-01, S13-12, S13-14, S13-15, S17-03, S17-26, S18-16, S18-17, S59-03, S59-13, S60-03, S60-11, S62-01, S62-16, S62-17, S72-08, S79-04, S82-19, S84-07, S84-11, S84-12 |
| done | F-01 |
| done | A08-D5, A15-D8, A18-V2, A18-V3, A18-V5, A18-V8 |
| info | H-01, H-02, H-03, H-04, H-09, H-10 |
| info | S1-01, S1-02, S1-07, S1-75, S1-102, S4-01, S4-02, S4-03, S4-04, S4-05, S4-06, S4-07, S4-08, S4-09, S4-10, S4-11, S4-12, S4-13, S4-14, S4-15, S5-02, S7-01, S12-03, S14-12, S17-24, S49-30, S63-07, S71-12, S77-01, S79-01, S80-02, S82-18 |
| info | P9-01 |
| info | F-42 |
| info | A03-K2, A05-F19, A05-G10, A06-F6, A09-H8, A11-N11, A12-C5, A12-C7, A13-R8, A16-Am2, A17-Z11 |

## Decision items

Every answered item of `docs/engineering/OWNER_DECISIONS.md` is named in the "Decided by" cell of at least one row, or is listed here (item 191). `none` means the item asks for nothing that a row could carry. `owed` means its rows are owed: the text that holds its decision is not in this tree yet (the owner accepted this kind of entry as decision item 222). Nothing is owed today.

| Item | Rows | Why |
|---|---|---|
| 18, 145 | none | Each is a permission to download named specifications and test vectors once. |
| 292 | none | The acceptance of an independent similarity check in place of CLAUDE.md §3's read-and-write split, for one test-only fixture (TASKS item 40). A record of a deviation. |
| 22 | none | A permission to push one branch and open one pull request. |
| 23 | none | A comparison of two AMD manuals with AMD's own copies, done once. |
| 70, 161 | none | How pull requests are offered: at 100 commits, and pull request 15 at 76. A rule of process that no statement of the directive or decision of an ADR carries. |
| 167, 190 | none | How task 18b and the randomized model of its policy land: through pull request 16. Rules of process. |
| 224–228, 231–236 | none | How rented bench machines are used: the provider, the budget cap, who signs up and holds the credentials, the order of the rentals, how Origin boots there (items 231 and 236), what is kept and where, what a bench record may claim, no self-hosted runner, and rentals through Latitude's API (`docs/research/2026-11-12-first-bare-metal-boot.md`). Rules of process for bench equipment, which ADR-0009 A1 keeps outside the declared scope. |

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
| 283–290 | 2026-11-13 | The kernel uses every processor the MADT enables, whatever the count: per-processor state is sized at discovery and no processor count is compiled in, which replaces C6's 64 indexes (ADR-0008 Amendment 1). | A08-C6, A08-D1, A08-D2, A08-D3, A08-D4, A08-D5, A08-D6, A08-D7, A08-D8 |
| 295–299 | 2026-11-14 | The whole operating system as SDK reaches the host tools and the layers above the kernel. The host tools become libraries that `xtask` calls, and the Developer SDK once Origin hosts its own builds; §49 gains Origin UI, Windows and display, Desktop shell, Settings, Time and Developer tools; the shell has no private interface; and a module shared between SDK domains gets a first-party interface (ADR-0011 Amendment 4; CLAUDE.md 7.9). | A11-T1, A11-T2, A11-T3, A11-T4, A11-T5, S49-32, S49-33, S49-34, S49-35, S49-36, S49-37, S49-38, S70-17 |
| 301–307 | 2026-11-14 | ACPI 6.6 is implemented as everything it asks of the operating system on x86-64. Its content for Arm, RISC-V and LoongArch processors and the compilers for ASL and the data tables' language are out of scope; the firmware's AML runs in a user-mode service; and no ACPI row is done until a machine in the declared scope runs it (ADR-0019). | A19-* |

## What the gate checks, and what it does not

`cargo xtask requirements-gate` fails when:

- a file here is not a table with the columns above, or a cell holds a value the column does not allow;
- a row has no owner, or an owner that is not one of §1's roles;
- an ID is used twice, or a row is in the wrong file or out of its source's order;
- a statement or a decision has no row, or a row quotes what its source does not say;
- a row's state lacks what the table of states asks for, or names an implementation or evidence that is not in the tree;
- a row is `done` on evidence from a weaker environment than its scope, or says its evidence ran on hardware and names no record the gate can read;
- a row is `done` or `info` and the lists above do not name it, or they name a row that is not;
- a heading, a code block or a table's header in the directive is not as its PART's digest records it;
- an answered decision item is neither named by a row nor listed above, or a row names an item the sheet does not hold as answered;
- a `removed` row is not named by a scope change with its item;
- the revision at the top of this page is not the directive's;
- it read no row at all.

It does not check:

- whether an owner, a scope, a kind, acceptance criteria or a state is the right judgement. The figure moves with the kind and the state, and there it rests on review that has not happened: the lists above make such a change a second, visible edit, and no more;
- whether a row's criterion is its own: most are shared (see "Acceptance criteria");
- whether evidence passes, where it ran, beyond a hardware record's form, and who reviewed a row;
- whether the body of an ADR's decision changed under an unchanged title. The owner accepted this limit as decision item 223: whoever changes an ADR's decision brings its row up to date by hand.

## Changing the matrix

- **The directive changes.** The gate names each new statement, with the first cells of its row, and each row whose statement is gone. Change the quote of a statement that was reworded. Delete the row of a statement that was deleted, unless a decision took it out of scope: then mark it `removed`. Then change the revision at the top of this page.
- **An ADR is accepted or amended.** Add a row for each new decision to its `adr-NNNN.md`, or add the file.
- **The owner answers a decision item.** Name it in the row it decided, or list it above.
- **Work lands.** Change the row's state with the cells that state needs, and give the row the criterion its task's contract writes for it (CLAUDE.md §78). A row that becomes `done` is added to the list of done rows above.
- **A row's state changes, or the roadmap places a row or gives a milestone's state.** Run `cargo xtask progress` and commit `docs/PROGRESS.md` with the change: `cargo xtask check` refuses a page that is not the one the tree generates.
- **A heading or a code block of the directive changes.** Read the change against that PART's rows, then record the digest the gate prints.
- **Something runs on a machine.** Write its record under `docs/hardware/records/`, with the machine, its firmware, the source revision, the date and the result, and name it in the rows it is evidence for.
- **A state never goes up on memory.** It follows what the tree shows: `docs/STATUS.md`, the boot proofs, the tests and the gates.
