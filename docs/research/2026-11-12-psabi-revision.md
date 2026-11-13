# The x86-64 psABI revision that CLAUDE.md §17 pins (2026-11-12)

| | |
|---|---|
| Why | CLAUDE.md §17 said "pin the revision after verification", open since `docs/research/2026-11-07-onbf.md`. Task S4-2 publishes `origin.core`'s first C entry points, and their C ABI rests on this document (ADR-0011 L5) |
| Who and when | The coordinating session, 2026-11-12 15:33 UTC |
| Source | `gitlab.com/x86-psABIs/x86-64-ABI`, read through GitLab's public API with no account. A free official specification (CLAUDE.md §3's first kind of source) |
| Result | Decision items 271 to 273 of `docs/engineering/OWNER_DECISIONS.md`, accepted by the owner on 2026-11-13: CLAUDE.md 7.7 §17, ADR-0005 Amendment 9, ADR-0011 Amendment 3 |
| Kept out of the repository | The document's files. Only Intel's changes carry a stated licence in the text that was read (CC BY 4.0); the terms of the whole were not established, so nothing of it is copied here, and what follows is in Origin's words (§17) |

## 1. What the revision is

- The repository's default branch is `master`. Its newest commit on 2026-11-12 was `e1ce098331da5dbd66e1ffc74162380bcc213236`, committed 2025-03-12, whose subject says it clarifies that the excess bits of INTEGER-class arguments are unspecified. The list of branches gives the same commit for `master`.
- The document calls itself version 1.0 (its `\version` macro). The repository's tags end at `abi_0_96`, of 2005-06-10: there is no tag for 1.0, so a revision can only be named by a commit.
- Commits dated 2026 exist on merge-request references only: a redefinition of the large code model, relaxation relocations for it, a relocation named `R_X86_64_PCNEXT32`, and an addition about thunks. None is on `master`, and none touches the small code model, which is the only one Origin uses (ONBF §6.1).
- Earlier sessions could not read it: `docs/research/2026-11-07-onbf.md` (the site was blocked), `2026-11-09-oidl-wire-format.md` (a search summary only) and `2026-11-11-n14-step4-scope.md` (the commit list did not render).

## 2. What was read, and what it says

One file, `x86-64-ABI/low-level-sys-info.tex` at that commit (105,138 bytes, SHA-256 `2d42f2ab76b99a3b8456e6bc07e18314e85991119f72db90ca1175328e27b705`), in these places; and `abi.tex`, `macros.tex` and `README.md` for the version and the licence note.

| # | Where | What it says | What it means for Origin |
|---|---|---|---|
| R1 | The calling sequence's text on registers | The direction flag is clear on entry to a function and on return from it. The other user flags have no part in the calling sequence and are not kept across a call | `origin.core`'s entry points are C functions, so a caller has the flag clear, and the kernel keeps RFLAGS across a call |
| R2 | Process initialization, the table of flags | At a process's start the direction flag and the six status flags (carry, parity, auxiliary carry, zero, sign, overflow) are all clear | ADR-0005 P6 did not state the flags. The kernel starts a thread with RFLAGS `0x202`, which is this. Decision item 272 |
| R3 | Process initialization, the tables for the x87 control word and MXCSR | Round to nearest, every exception masked, double-extended precision for x87, no flush to zero and no denormals-are-zero | P6's "default x87 and MXCSR state" is this |
| R4 | Process initialization, the registers and the stack | Only RBP (unspecified), RSP (aligned to 16 bytes) and RDX (a function to register for exit) are specified. The stack holds the argument count, the argument and environment strings and the auxiliary vector | Origin does not take this part. P6 puts the start record in RDI, clears every other general-purpose register, and passes nothing on the stack |
| R5 | Parameter passing, after the classification rules | When an INTEGER-class value is passed or returned in a register or on the stack, the bits beyond the type's memory representation are unspecified, and the side that consumes the value extends it. For a `_Bool` the lowest 8 bits are significant and form 0 or 1 | The kernel's register decoding judges the whole register (OIDL §10). A naked stub (ADR-0011 L6) passes the caller's register on as it is. So a correct C call could be refused. Decision item 273 |

## 3. What Origin takes, and what it does not

- **Takes:** the data representation (sizes, alignments and layouts of the scalar and aggregate types, LP64) and the function calling sequence (registers, the stack at a call, the red zone, which registers a function keeps). This is what a compiler follows for `extern "C"` on the userspace build (CLAUDE.md §11), and what OIDL's generated C header relies on (OIDL §10, Q10).
- **Takes as a statement of the start state:** R2 and R3, through ADR-0005 P6.
- **Does not take:** the initial process stack, the auxiliary vector and the exit function in RDX (R4); the ILP32 chapter, since Origin has no x32 (CLAUDE.md §42); the large and medium code models (ONBF §6.1).

## 4. Not read, and not verified

- The chapters on object files, relocations, thread-local storage, unwinding, the control-flow-enforcement properties, and linker optimizations. `onbf-pack` reads ELF as linker output (ADR-0001), and ONBF §6.1.1 fixes thread-local storage; neither was read again against this revision. So §17's pin is of the revision, and of the three places above, and no more.
- Whether Rust 1.99.1's code generation follows this revision in every case. CLAUDE.md §10's regression tests show the cases they test. R5 is the reason item 273 does not rely on what a caller's compiler happens to do.
- The document's whole licence.

## 5. Re-check

Before `user-abi` reaches major 1, when `origin.core`'s exports are frozen (ADR-0005 P12): read `master` again that day (CLAUDE.md §3, "always read today's version"; §16), and compare it with this commit in the places above.
