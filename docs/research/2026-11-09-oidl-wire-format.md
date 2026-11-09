# Research: the OIDL wire format (2026-11-09)

Backing notes for `docs/spec/OIDL.md` (CLAUDE.md §16). What was read, what it means for Origin, and what stays unknown. These are conceptual references: **no source code of any of these systems was read** (CLAUDE.md §3). The wider ledger for ADR-0011 is `docs/research/2026-11-09-native-sdk.md`.

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, FIDL wire format reference, `fuchsia.dev/fuchsia-src/reference/fidl/language/wire-format` | Current page, 2026-11-09 | How a fixed-layout format frames messages and handles, and what must be validated | Read through WebFetch, 2026-11-09 |
| Cap'n Proto encoding specification, `capnproto.org/encoding.html` | Current page, 2026-11-09 | An alternative: pointers, evolution, capabilities | Read through WebFetch, 2026-11-09 |
| FlatBuffers internals, `flatbuffers.dev` and mirrors of its `Internals.md` | Search results, 2026-11-09 | An alternative: vtables and field presence | Search-result summary only |
| Fuchsia, `zx_channel_read_etc` reference | Current page, 2026-11-09 | Whether a receiver needs a handle's type and rights | Read through WebFetch, 2026-11-09 |
| System V AMD64 psABI, `gitlab.com/x86-psABIs/x86-64-ABI` | Version 1.0 by search results; the document was not read | Which revision §17 of the directive should pin | Search-result summary only |

## What they show

**FIDL's wire format is close to what ADR-0011 N7 asks, and differs where Origin's constraints differ.** It frames messages with a header and carries handles out of band, stores a present-or-absent marker in the body, packs out-of-line data 8-byte-aligned with zero padding, and validates sizes, handle counts, recursion depth and enum values. Origin's format differs in these ways: the header starts with `size` (ADR-0005 P4) rather than a transaction id; a handle's slot in the body is its index in the out-of-band list, which must equal its position, rather than a marker; a vector stores a count inline and its data trails in declaration order, so there is no out-of-line pointer and no recursion at all; ordinal 0 is a hello rather than an invalid value.

**Cap'n Proto is the opposite trade.** Structs have a data and pointer section, lists are reached through relative pointers, and a capability is an index into a separate table, the same idea as the slot index above. A message does not have one valid encoding; a canonical form is a separate rule. Its pointers need validation and bounds on depth and traversal, and it is a large format to reimplement (§3, §7).

**FlatBuffers carries field presence in the data.** An older reader sees a missing field as a default, which needs no negotiation. ADR-0011 N10 already requires negotiation at connection time, and a format with offsets has more to validate.

**A receiver needs each handle's type and rights.** `zx_channel_read_etc` returns the handle value, object type and rights for each handle, so a receiver can refuse a handle of the wrong kind or with too few rights. Origin's `channel_read` returns only handle values today, which is why the spec's question Q8 exists.

## What this means for Origin

- The format is original in its details, and the differences above are deliberate. No text or layout was copied from any of these documents.
- A count-and-trailing layout, plus declared bounds, removes the two largest validation risks of the pointer-based formats: pointer validation and depth limits. It costs nesting, which no first-party interface needs yet (§7).
- Canonical encoding comes from four small rules: zero padding, an exact size, determined handle slots and closed enums and bits.

## Unknown

- What a round trip costs on the declared hardware. No figure is claimed (rule 21); ADR-0011 N8 requires the first service domain to measure it.
- The current revision of the x86-64 psABI text. The search results point at version 1.0; the document was not read, so CLAUDE.md §17's "pin the revision after verification" is still open. Nothing in OIDL depends on a recent revision: it uses the natural-alignment rules, which have not changed.
- How `origin.core`'s stubs interact with per-number pinning (ADR-0005 B2) at link time. The stubs have one `syscall` instruction each, checked by disassembly, but how the kernel learns each instruction's address is for a later slice.
