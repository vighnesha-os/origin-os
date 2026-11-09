# Research: the native SDK as the only way to the operating system (2026-11-09)

Backing notes for ADR-0011 (CLAUDE.md §16). What was read, what it means for Origin, and what stays unknown. These are conceptual references only: no source code of any of these systems was read (CLAUDE.md §3).

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, "Zircon vDSO", fuchsia.dev/docs/concepts/kernel/vdso | No date on the page | The library as the only door to the kernel, and how the door is enforced | Read through WebFetch, 2026-11-09 |
| Fuchsia RFC-0190, "FIDL support for syscalls" | Accepted; submitted 2022-07-08, reviewed 2022-09-22 | One definition as the source of truth for kernel calls | Read through WebFetch, 2026-11-09 |
| Fuchsia, FIDL wire format reference, fuchsia.dev/docs/reference/fidl/language/wire-format | Current page, 2026-11-09 | A fixed-layout message format, and what must be validated | Read through WebFetch, 2026-11-09 |
| Fuchsia, "FIDL Overview" | Current page, 2026-11-09 | The kernel knows nothing of the interface language | Search-result summary only |
| Peng et al., "Asterinas: A Linux ABI-Compatible, Rust-Based Framekernel OS with a Small and Sound TCB", USENIX ATC '25, pp. 307–323; arXiv 2506.03876; project blog 2025-06-04 | 2025 | What a framekernel is, and how small its trusted part can be | Search-result summary only; the paper was not read in full |
| OpenBSD, `pinsyscalls(2)` | OpenBSD 7.5 and later | Pinning each system-call number to one instruction address | Search-result summary of the manual page |
| Apple, Technical Q&A QA1118, "Statically linked binaries on Mac OS X" | First published 2002, rewritten 2011 | A mainstream OS that promises nothing at the system-call boundary | Search-result summary |
| Chromium, "Android IPC Security Considerations" | Development tree, retrieved 2026-11-09 | Caller identity against capability tokens | Search-result summary |
| systemd, `org.freedesktop.systemd1(5)` | systemd 261 and 262 pages | A system manager that publishes a typed interface many clients call | Search-result summary |

## What they show

**The library as the only door.** Zircon's vDSO "is the sole means of access to system calls", and the interface between the kernel and the vDSO is "a private implementation detail". Apple gives no guarantee at the system-call boundary and supports only the system libraries. CLAUDE.md §42 and ONBF §6.1 already choose this for Origin.

**The door can be enforced by the kernel, per call.** Zircon constrains how the vDSO can be mapped and, for each system call, accepts only a fixed set of program-counter locations. OpenBSD's `pinsyscalls` gives each call number one permitted instruction offset, and any other use is a fatal signal. ADR-0005 P4 and A6 today check only a code range, so code that jumps to a `syscall` instruction inside the range can make any call. Per-number pinning closes that. It is hardening, not authorization.

**One definition, many outputs.** RFC-0190 makes one interface library the single source of truth for Fuchsia's system calls and generates headers, per-language bindings and documentation from it. Its motivation is the duplication and drift that hand-maintained definitions caused. Origin has the same problem at a smaller scale: each call is written by hand in `origin-abi`, in the kernel's decoder, and in any future stub.

**The kernel need not know the interface language.** FIDL's bindings use ordinary channels, and Fuchsia's kernel has "no innate knowledge" of it. ADR-0005 P8 says the same for Origin: typed protocols are an SDK layer above channels.

**A fixed-layout message format is enough, and needs strict validation.** FIDL is little-endian and aligned, encoded and decoded in place, not self-describing, and carries handles out of band. Validation is required on receipt: exact sizes, handle counts, recursion depth, enum values, presence flags and zero padding. Origin's OCBOR-1 is the opposite trade: canonical and self-describing, right for signed metadata and wrong for a hot IPC path.

**A framekernel's safety rests on a real boundary.** In Asterinas only the privileged framework may use `unsafe`, and every service, drivers included, is safe Rust on its API. The paper reports a TCB of 14.0 % of the code, against 43.8 % for Tock, 62.4 % for Theseus and 66.1 % for RedLeaf. These are that project's figures for its own design, not a target for Origin (CLAUDE.md §7).

**A capability says what may be done, not who is asking.** Binder gives a service the caller's UID, supplied by the kernel. Chromium's guidance warns that tokens passed between apps establish access but not identity. CLAUDE.md §30 and §48 require authenticated IPC, and capability handles alone do not supply that.

**Clients of one typed interface.** systemd publishes a typed manager interface with named methods and access control on the service side, and tools such as `busctl` and language bindings call it directly. How `systemctl` itself is built was not checked.

## What this repository shows (the pre-merge review, 2026-11-09)

- There is no interface definition, no generator, no C header, no `origin.core`, no `sdk/` directory and no userspace crate or target.
- `origin-abi` holds 13 status codes, 9 rights, 5 signals, 7 object types and 11 call numbers. M4's `syscall.rs` has 9 live calls with raw `u64` arguments, and no Rust layout crosses the boundary, so a C wrapper is not blocked.
- `origin_abi::syscall` is public in the same crate as the stable items. Handle values expose slot and generation bits (`handles.rs`).
- Channels carry no sender identity.
- The Framekernel has no framework boundary: 310 `unsafe` sites in 21 files, about half the kernel's non-test code in the crate that permits `unsafe`, and memory-unsafe operations exposed as safe functions.
- Interrupts are dispatched by hard-wired calls upward from `cpu::dispatch`, and the driver objects of ADR-0005 P3 do not exist.

## Unknown

- What a typed round trip costs on the declared hardware. QEMU's numbers would mean little (CLAUDE.md §53, rule 21), so ADR-0011 sets no numeric budget yet.
- How much of today's `unsafe` can be removed, as opposed to moved. Only doing the work will show.
- macOS's XPC peer-identity guarantees: the search found nothing authoritative. Windows was not studied.
- The Asterinas paper and the OpenBSD and Apple pages were not read in full.
