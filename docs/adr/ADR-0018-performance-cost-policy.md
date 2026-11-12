# ADR-0018: Performance cost policy

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-12, as proposed, with the recommended answer to each of the nine questions at the end (decision sheet items 238 to 246; CLAUDE.md §18). The owner's answer was "Accept all 9", in the coordinating session, to the list the architecture-review session wrote on the owner's "Yes, write them". **Nothing is built:** there is no budgets file, no ledger and no gate yet. **No figure is measured here, and no human has reviewed it.** |
| Decides | What "zero performance trade-offs" means for Origin, which the owner asked on 2026-11-12, and how every design decision states and records what it costs. It gives decision item 115 (finding 10 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`; TASKS item 26) its place in the order of work |
| Requires | CLAUDE.md P0, P1, §1 (roles 16, 23 and 36; "Establish performance budgets before measurement", "Measure reproducibly", "Use appropriate distributions", "Report verified completion"), §2, §18, §30, §53, §66, §74, §79; rules 16, 21, 22, 30, 35 and 40 |
| Performance cost | None, by construction: this ADR changes documents and, with V8, one gate on the build host. It adds nothing to any image |
| Code | None changed. V8's gate would add to `developer-tools/xtask` |
| Evidence | The records as they stand on `main` at `04ec842`, read on 2026-11-12 and listed under "Evidence". Documents only |
| Related | ADR-0011 N8, L11 and L12; ADR-0012 C8; ADR-0013; ADR-0016 R12; ADR-0017 Z3; decision items 115 and 233 |

## Context

On 2026-11-12 the owner asked: "Have you considered zero performance tradeoffs? if need to consider zero performance tradeoffs what do you do?"

Nobody had. What the records held that day:

- **The order of priorities.** CLAUDE.md §2 puts correctness and security (P0) above performance (P1) and says "Never compromise P0 to meet another priority". Final rule 16 says the same. So the directive expects some performance to be paid for safety.
- **No one list of costs.** Each accepted ADR has a "Trade-offs" section, in prose, one ADR at a time.
- **Almost nothing measured.** ADR-0017 says of the barrier it puts at every boundary between two programs: "This is the largest cost here and nobody has measured it." The figures that exist come from QEMU's software processor and from CI's KVM guests, and they claim nothing about a declared machine.
- **No baseline.** The file of provisional budgets that §53 asks for, `docs/engineering/PERFORMANCE_BUDGETS.md` (decision item 115, TASKS item 26), was on no branch.

Read word for word, "zero performance trade-offs" cannot be met while P0 is first. Each of these costs processor time on purpose:

- integer overflow checks in release builds of the kernel and the loader (§14);
- third-party drivers in user mode, confined by the IOMMU (§47);
- every operation of a first-party client going through a service (§48);
- checking all data that crosses a privilege boundary (§30).

What can be met is narrower, and it can be checked: that no cost is paid without a reason, without a look for a cheaper design, and without being written down.

## Decisions

### V1. No avoidable cost

Origin reads "zero performance trade-offs" as "no avoidable cost". Performance is given up only where all three hold:

1. a P0 invariant, a rule of CLAUDE.md or an accepted ADR needs it;
2. no design we know gives the same result for less;
3. the price is written in the cost ledger (V5).

Literal zero is not adopted, and §2's order does not change. Where a cost and a P0 invariant meet, the invariant stays, and the cost goes into the ledger with what would let it be lifted.

Who judges is as CLAUDE.md §1 routes it: whether a cost is avoidable is the Chief Performance Architect's (role 23), and a security invariant is the Chief Security Architect's (role 16) and is never the thing given up.

### V2. Where the rule lives

This ADR holds the rule. CLAUDE.md is not changed for it: §2, §18, §53 and rules 16 and 21 already carry what it rests on.

### V3. Three ways to state a cost

Every design decision accepted from here on states its performance cost in one of three forms:

| Form | What it must give |
|---|---|
| "None, by construction" | The reason: what makes the cost nothing while the system runs |
| "Measured" | The figure, and where it was measured |
| "Unmeasured, owed" | The model the estimate comes from |

This ADR's own cost is in the table at its head.

### V4. The order of choice

A design takes the first of these that holds its invariant, and its ADR says where it stopped and why:

1. checked when the code is built;
2. checked once: at load, at boot, or when a handle or a mapping is made;
3. checked on each operation, in the cheapest measured form.

The order never takes away a check that an invariant needs. It moves the check to the earliest point at which it still holds. Data from a less trusted side is still checked each time it crosses a boundary (§30).

ADR-0017 already chose this way once. It declined page-table isolation, which costs every entry and exit and which no processor in scope needs, and took LASS on Intel, which hides the kernel's layout "at no cost on entry".

### V5. One cost ledger, in the budgets file

`docs/engineering/PERFORMANCE_BUDGETS.md` holds two tables:

- the budgets of decision item 115, one row a path;
- the cost ledger, one row for each place the design pays on each operation, or pays a price nobody has measured.

A ledger row gives:

- the path that pays;
- what the cost buys: the ADR decision, or the directive's section;
- the estimate, and its model;
- the measured figure, and its environment;
- the cheaper design that was considered;
- what would let the cost be lifted.

The ledger's first fill is made by reading the Trade-offs of ADR-0001 to ADR-0017 and the threat models.

### V6. The budgets file comes first

TASKS item 26 is built before any new figure is recorded: a hypothesis and its model for each path the boot already times, then the trends. No figure fails CI until declared hardware exists.

ADR-0011 L11 already needs the file before step 4's first measurement. This decision asks the same of every figure.

The boot already times these, on `main` at `04ec842`: the channel ping-pong, the ports' round trip, a masked and a fenced lookup, one IBPB where the processor has it, a 64-byte random request, a clock read, and the wait queues' ping-pong.

### V7. A protection's price is measured as a pair

One commit is built twice: once as it is, and once with one named edit that takes the protection out. The two kernels boot in turn on the same machine in the same run. The price is the difference, given with its spread.

- A script applies the edit to an exported copy, the way boot-proof controls are planted (ADR-0013).
- No switch that turns a protection off enters the tree. The kernel's crates have no build features today, and this adds none.
- The second kernel is never signed or packaged.
- Where a protection cannot be taken out without a redesign, its price comes from a stated model and is marked so.

### V8. A gate

`cargo xtask check` refuses:

- an ADR or an amendment accepted after this one with no "Performance cost" row in one of V3's forms;
- a "measured" or "owed" cost that names no ledger row;
- a ledger row whose ADR decision does not exist, or whose figure has no environment: `emu`, `virt` or `hw`, the requirements matrix's words.

ADR-0001 to ADR-0017 are not edited. The ledger lists each of them, with its rows or with "none" and the reason.

### V9. What may be claimed

Until a figure comes from declared hardware, the most a document says is "no known avoidable cost; unmeasured on a real processor".

- "Zero-cost", "zero overhead" and "zero performance trade-offs" are not said of Origin as a whole.
- One mechanism may be called "no cost by construction", with its reason.
- A rented server's figures are bench figures (decision item 233).

## Evidence

Read on `main` at `04ec842` on 2026-11-12. Nothing here is measured.

- **The budgets file is missing.** `docs/engineering/PERFORMANCE_BUDGETS.md` was on no local branch and no branch of `origin`. Decision item 115 accepted it on 2026-11-10 ("The file is to be written"), and TASKS item 26 is its task.
- **The requirements matrix agrees.** Its rows for budgets and measurement were all `not-started`: S1-91 ("Establish performance budgets before measurement"), S1-92, S1-93, S53-12, F-21 and F-22.
- **The largest known cost is unmeasured.** ADR-0017's Trade-offs, on Z3. ADR-0011 L12 adds that neither QEMU nor CI's KVM guests offer IBPB, so their figures leave that barrier out: two in each round trip made on one processor.
- **The boot prints costs already.** `framekernel/src/ipc_proof.rs`, `ipc_proof/events_ports.rs`, `kernel.rs`, `time.rs` and `wait.rs` hold the lines V6 lists.
- **Paired boots have been done before.** `docs/engineering/SOURCE_PROVENANCE.md` records two comparisons of a round trip, each as "six boots of each kernel, alternating which went first", and each found the gap inside the boot-to-boot spread.
- **Controls are scripted edits.** ADR-0013 records its controls as "one temporary edit to an exported copy", built and booted once.
- **The kernel has no build switch.** `framekernel/Cargo.toml` and `framekernel/frame/Cargo.toml` declare no features.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Adopt literal zero: no protection may cost time | It is what the words say | It cannot be met with P0 first, and it would need §2 and rule 16 to change. It would also invite calling a cost zero without measuring it |
| Change nothing: keep each ADR's Trade-offs in prose | No new work | Nobody can say what Origin pays in total, or which cost is the largest, or whether any was looked at twice. ADR-0017's largest cost would stay a sentence |
| A build switch that turns protections off, for measuring | One build, easy to compare | The switch would be in the tree and one mistake from a release. Role 16 accepts no weakened invariant for convenience. A scripted edit to a copy measures the same thing and leaves nothing behind |
| Fail CI on a figure now | Regressions are caught early | CI's runners are shared guests with no declared hardware. A threshold there would fail on noise, and decision item 115 already leaves thresholds until declared hardware exists |
| Put the rule into CLAUDE.md now | Every session reads the directive first | The directive already carries what the rule rests on, and V8's gate reaches every later ADR. A revision for this alone would cost a full pass of the requirements matrix |
| Add a "Performance cost" row to ADR-0001 to ADR-0017 | One form everywhere | It edits seventeen accepted records. The ledger can list them without changing a word the owner accepted |

## Trade-offs

- **The policy costs writing.** Every later ADR carries one more row, and each cost on a path carries a ledger row. That is the price of knowing the total.
- **The gate checks form.** It can see that a cost is stated and that a ledger row exists. It cannot see whether "none, by construction" is true, or whether a cost was avoidable. Those rest on review.
- **A pair measures what can be taken out.** A protection that is part of the structure, such as a service being its own process, has no second kernel. Its price is a model, and V7 says so.
- **A kernel with a protection taken out exists.** It lives on a build host or a bench machine for the length of a run, unsigned. Each edit that makes one is a security matter, and the Chief Security Architect (role 16) reviews it.
- **Today's figures cannot show the largest cost.** IBPB is offered by neither QEMU nor CI's KVM guests. Until a declared processor runs, V9 is all that can be said.
- **The ledger can go stale.** A figure is true of one revision on one machine. Each row names both, and the Benchmarking and Regression Lead (role 36) checks the figures.

## Migration consequences

- **Nothing in any image changes.** No ABI, format or interface moves.
- **TASKS item 26** builds `PERFORMANCE_BUDGETS.md`, now with the ledger's table beside the budgets.
- **A new task** makes the ledger's first fill and builds V8's gate. The coordinating session numbers it.
- **Later ADRs and amendments** carry a "Performance cost" row. ADR-0001 to ADR-0017 stay as they are.
- **The requirements matrix** gains a row for each decision here (`docs/requirements/adr-0018.md`).
- **ADR-0011 L11 and L12** are unchanged: their hypothesis and their trends go into the same file.
- **ADR-0017 Z3** is unchanged. Its IBPB becomes a ledger row marked "unmeasured, owed".

## What this does not decide

- No budget, no figure and no threshold. Those are TASKS item 26's, as hypotheses.
- Which protection is measured first, and on which machine.
- What counts as a regression once declared hardware exists.
- Any change to an accepted decision. Where the ledger shows a cost that looks avoidable, lifting it is a decision of its own.

## Questions for the owner (answered 2026-11-12: the recommended answer to all nine)

The owner answered "Accept all 9".

| # | Question | Recommended | Item |
|---|---|---|---|
| 1 | Read "zero performance trade-offs" as "no avoidable cost", with literal zero not adopted and §2's order unchanged (V1)? | **Accept** | 238 |
| 2 | Hold the rule in this ADR, with no change to CLAUDE.md now (V2)? | **Accept** | 239 |
| 3 | State every later decision's cost in one of three forms (V3)? | **Accept** | 240 |
| 4 | Choose in the order: at build, once, on each operation (V4)? | **Accept** | 241 |
| 5 | Keep one cost ledger in the budgets file, first filled from ADR-0001 to ADR-0017 and the threat models (V5)? | **Accept** | 242 |
| 6 | Build the budgets file before any new figure is recorded, with no figure failing CI until declared hardware exists (V6)? | **Accept** | 243 |
| 7 | Measure a protection's price as a pair of kernels, the second made by a scripted edit, with no off-switch in the tree (V7)? | **Accept** | 244 |
| 8 | Add the gate, and list ADR-0001 to ADR-0017 in the ledger without editing them (V8)? | **Accept** | 245 |
| 9 | Claim at most "no known avoidable cost; unmeasured on a real processor" until declared hardware gives a figure (V9)? | **Accept** | 246 |
