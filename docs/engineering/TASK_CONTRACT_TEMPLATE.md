# Task contract template

| | |
|---|---|
| Required by | CLAUDE.md §78 ("Every task must define"), decision item 122 |
| Where a contract goes | `docs/engineering/TASKS.md`, under "Task contracts", as a `###` heading with the task's ID and name |
| When | Before any of the task's code or documents, and committed before the work starts |
| Written | 2026-11-12, by TASKS item 25 |

A contract is the eleven rows of the table below, in §78's order, each with the task's own text. Copy the block, replace each cell, and keep the field names exactly as they are: they are §78's words. A field with nothing to say says so ("None outside the repository", "None set"), and never stays empty.

## The block to copy

```markdown
### <ID>. <What the task makes, in a few words>

<Who assigned it and when, and that this was written before any of it was built. The TASKS item or decision items it builds.>

| Field | |
|---|---|
| Task ID | <TASKS item, or the step's own ID>, on branch `<branch>`, cut from `<branch>` at `<commit>`. |
| Objective | <What is true when the task is done, in a few sentences.> **Not in this task:** <what a reader might expect here and will not find>. |
| Source ownership | <The files and directories this task alone edits. Who else owns what it touches, and how shared files stay narrow.> |
| Architecture contract | <The directive's sections, the ADRs and decisions by ID, and the specifications in the repository that bind the work. Whether any of it is an ABI or format decision for the owner (§84).> |
| Relevant specifications | <Each external specification, with its revision and the date it was read (§16), or "None outside the repository".> |
| Required dependencies | <The branch and commit it builds on, the tasks that must land first, any new crate or tool (§5), and any owner answer it waits for.> |
| Security invariants | <Numbered statements that stay true throughout, each one a test or a gate can refute.> |
| Performance constraints | <The budget, or "None set", and what is measured and reported, with where it ran (§53, decision item 115).> |
| Required tests | <Host tests, boot proofs, models and gates; each negative control, and how it is planted and undone.> |
| Actual deliverables | <What is committed: code, tests, records. Whether anything is pushed.> |
| Acceptance conditions | <What must pass, and where (§74). The rows of the requirements matrix this task completes, by ID: each takes its condition from here as its own criterion (docs/requirements/README.md, "Acceptance criteria"). Who checks it, and what the task does not show.> |
```

## What each field holds

| Field | What it holds | Seen in the contracts so far |
|---|---|---|
| Task ID | The TASKS item or step, the branch, and the commit it was cut from. | Every contract names its branch; S3-6 and S4-0 name their step of ADR-0011 N14 as well. |
| Objective | The result, not the activity. A "Not in this task" sentence for what a reader might expect to find. | 18a, 18b and TASKS 25 say what is not in the task. |
| Source ownership | The files only this task edits, and who owns the rest it touches. Shared files (`TASKS.md`, `SOURCE_PROVENANCE.md`, `STATUS.md`) are named with how the diff stays narrow. | S3-6 and S4-0 split the files between the interface session, the framework session and the coordinating session. |
| Architecture contract | Sections of CLAUDE.md, ADR decisions by ID, decision items, specifications in `docs/spec/`. | 17 and TASKS 25 say why no ADR is needed (§84). |
| Relevant specifications | External specifications with revision and the date read, or none. | 17 lists Intel's and AMD's manuals read on the day; S4-0 says "None outside the repository". |
| Required dependencies | Base branch and commit, tasks that land first, any new crate, any owner answer awaited. | S4-0 names `main` at `04ec842`; 17 says "None new" and that `xtask` stays `std` only. |
| Security invariants | Numbered, each refutable. | S3-6 has twelve and records two later corrections (items 178, 179) in place. |
| Performance constraints | A budget, or "None set", and what is reported as a figure without a budget. | S3-6 and S4-0 report QEMU figures with no budget. |
| Required tests | Tests, proofs and negative controls, with how each control is planted and put back. | S4-0's controls are temporary edits booted once and put back byte for byte. |
| Actual deliverables | What is committed; whether anything is pushed. | S3-6, S4-0 and TASKS 25 say nothing is pushed; the earlier contracts do not say. |
| Acceptance conditions | What passes, where, who checks it, and the matrix rows it completes. | S4-0 and TASKS 25 name the matrix rows they change. The contracts before the matrix (TASKS 35, 2026-11-12) could not. |

## After the contract

- **Changes while building** are written below the contract, under their own heading, with the reason for each. The contract itself is kept as written (19b, "What changed from that contract while building").
- **The result** is written below it too: done or not, where it differs from the contract, and where its evidence is (17 and 31, "Result").
- **A correction to the contract** made after the owner answers is marked in place with the decision item that made it (S3-6, items 178 and 179).
- **Decisions** the task raises go to the coordinating session as proposals with a recommendation, never to the owner directly; an answer that changes accepted text is an amendment for the owner before it is built (§84).

## Checked against the contracts in `TASKS.md` (2026-11-12, at `3fcf639`)

Every contract table in `TASKS.md` was read for its field names. There are ten:

| Contract | Where in `TASKS.md` | Fields | In §78's order |
|---|---|---|---|
| 17, the assembly blocks' options and four gates | inside item 17 | all eleven | yes |
| 19a, the system threat model and ADR-0017 | inside item 19 | all eleven | yes |
| 19b, ADR-0017's first code | inside item 19 | all eleven | yes |
| 31, the reproducible loader | inside item 31 | all eleven | yes |
| 35, the requirements matrix | inside item 35 | all eleven | yes |
| 18a, the random-number generator's research and ADR | "Task contracts" | all eleven | yes |
| 18b, the generator in the kernel and the loader | "Task contracts" | all eleven | yes |
| S3-6, events, ports and `clock_read` | "Task contracts" | all eleven | yes |
| S4-0, 64-bit handles | "Task contracts" | all eleven | yes |
| 25, the records | "Task contracts" | all eleven | yes |

The template's field names are the ones all ten use, word for word. What differs:

- **Five contracts are not under "Task contracts".** 17, 19a, 19b, 31 and 35 sit inside their items in "Next, in order", although the section says "A task writes its contract here". They were written before the section existed or beside the item they answer. They are left where they are: moving them would rewrite other tasks' records.
- **Some work done since item 122 has no contract table in this tree:** S3-5b (memory objects) and H2-stacks, both built on 2026-11-10, the day item 122 was accepted; and TASKS item 39 (the fingerprint's handle width, 2026-11-12), whose line gives "done when" conditions in prose. Item 32 was done on a branch this tree does not hold, so its contract, if it has one, cannot be checked here.
- **Nothing checks a contract.** No gate reads `TASKS.md`, so a contract with a missing or renamed field would pass `cargo xtask check`. The matrix rows S78-01 to S78-11 ask that "a check reads it"; they stay short of done until one does.
