# ADR-0011: The native SDK is the only way to the operating system

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-09, as recommended: questions 1–6 below, decision sheet items 33–38 (CLAUDE.md §18, §84). Question 7 is still the owner's. **Nothing here is built.** The owner set the direction the same day: "100% native SDK (if possible)", with the console, the terminal and the kernel included, and "every module as SDK" |
| Requires | CLAUDE.md P1, P2, P3, P8; §3, §5, §7, §13, §22, §30, §42, §44, §45, §47–§50, §53, §58, §62, §63, §66, §70, §72; final rules 5, 10, 11, 12, 20, 21, 36 |
| Evidence | `docs/research/2026-11-09-native-sdk.md`; `docs/engineering/PRE_MERGE_REVIEW_2026-11-09.md` |
| Builds on | ADR-0001 (framekernel argument R1, user-mode drivers D2, default 10); ADR-0003 K1; ADR-0005 (P1, P4, P6, P8, P12, A6); ONBF §6.1 |
| Would change | ADR-0003 K1 (the safe-library, unsafe-binary split), ADR-0005 (Amendment 2: caller identity and per-call pinning), `origin-abi`'s layout. CLAUDE.md §48 and §70, if the owner edits them (question 7) |

## Context

CLAUDE.md §48 requires GUI → SDK → authenticated service → kernel, and forbids a GUI reaching the OS through a hidden command. It binds the GUI. §48 and §70 allow a terminal and command-line tools, and say only that they must not be hidden backends.

The owner wants more: nothing first-party should reach the OS except through the SDK, the console and the terminal included, and the kernel should be SDK too. The question is what that can mean, because "SDK" covers several different things, and the cost and the promise differ for each.

The review found that none of this exists yet: no interface definitions, no generated bindings, no `origin.core`, and no privileged framework inside the Framekernel.

## Decisions

### N1. One rule

Every operation a program can ask the OS for is defined once, in a typed SDK interface. No first-party component obtains OS functionality any other way. It does not start another program to do the work, and it does not parse another program's text output.

### N2. Everyone is a client, with no private doors

GUI applications, the desktop, the installer, Origin Setup, the recovery console, command-line tools, the shell, the terminal and system services calling each other are all SDK clients. A command-line tool has no power the SDK does not offer: `origin-pkg install x` and the Install button in Settings make the same typed call.

The terminal stays an optional development tool (§48). It is an ordinary Origin UI application on a session interface. The shell starts programs through the Process interface.

### N3. Every module has a typed interface; only a capability gets a promise

Every module is reached only through its typed interface. But a stable, versioned SDK promise is made per **capability**, by domain (§49's list: Files, Network, Devices and so on), not per module. Internal crates such as a ring buffer or a page-table walker have ordinary typed Rust APIs and promise nothing. A domain gets an SDK surface when something outside its own layer needs it (§7).

### N4. Three SDKs with three different promises

| SDK | For | Form | Promise |
|---|---|---|---|
| **Framekernel SDK** | Kernel services and first-party kernel drivers | Rust source API, inlined | None across releases. Built together in the unified release (ADR-0001 D2) |
| **Driver SDK** | User-mode drivers (§47) | Versioned Origin Driver ABI, over kernel objects | Stable. Its own ADR, as ADR-0001 said |
| **Application SDK** | Programs and services (§49) | C ABI in `origin.core` and the SDK libraries, with Rust bindings | Stable and versioned (§42, §63) |

### N5. The kernel is below the SDK, and the SDK is its only door

Everything the kernel offers reaches programs only through `origin.core`. System-call numbers stay private to each release (§42). The kernel enforces this by accepting each call number only from that number's own instruction address in `origin.core`, the method of Zircon and OpenBSD, extending ADR-0005 P4 and A6 (its Amendment 2). This is hardening. It is not authorization (N9).

The kernel does not call the SDK, because the SDK is built on it.

### N6. The Framekernel is itself an SDK: one privileged crate, safe services above it

A new crate holds every `unsafe` operation in the kernel and exposes the Framekernel SDK: typed, safe, and sound against safe callers. It offers `PhysAddr`, `Iova`, `MmioRegion`, `DmaBuffer`, interrupt registration and the like, so a memory-unsafe operation cannot be a safe function taking a raw `u64`. Every other kernel crate, drivers and services included, is `#![forbid(unsafe_code)]` and depends on it, never the reverse.

This makes ADR-0001's R1 true of the code. It replaces ADR-0003 K1, whose split separates what can be tested on the host, not what is trusted.

### N7. One interface definition, generated everywhere

A small, original interface language (OIDL) is the single source of truth. Each interface declares its methods, typed arguments, errors, version and the capability each method requires. A host tool generates:

- safe Rust clients and server traits;
- the C header and C-ABI entry points;
- message encoders and validating decoders;
- the kernel's call table, decoders and `origin.core`'s stubs, so numbers and layouts exist in one place;
- reference documentation;
- an ABI fingerprint for the compatibility gate, and a hostile-message test corpus.

The wire format is little-endian, aligned and fixed-layout, with handles out of band. Decoders check exact sizes, enum values, zero padding and that every boolean is a `u8` of 0 or 1 (§10). OCBOR-1 stays for signed metadata and files. The grammar is a specification that follows this ADR.

### N8. One definition, three costs

| Where the implementation is | Binding | Cost |
|---|---|---|
| In the caller's own process | A library call | A function call |
| In the kernel | `origin.core`, then `syscall` | A system call; clock reads from a shared page |
| In a service | A channel with generated stubs (ADR-0005 P8) | An IPC round trip |

No per-item crossing on a data path. For block I/O, frames and audio the SDK sets up shared memory objects and a completion port (ADR-0005 P7, P9), then gets out of the way. No numeric budget is set here (rule 21). **The first service domain must report its round-trip cost on the declared hardware before a second one is built.**

### N9. The SDK is not a security boundary

A program can skip the library. So every check lives in the kernel or the service.

- The generated server stub refuses a call before any service code runs, if the handle lacks the rights the interface declares or the message fails validation (§30).
- The kernel stamps each channel message with the sender's identity, which the sender cannot forge. A capability says what may be done; identity says who asked, for per-user policy and audit (§30, §48). This is ADR-0005's Amendment 2.
- A process finds services only through the directory channel in its start record (ADR-0005 P1, P6). Its launcher decides what that directory contains, from the application's declared capabilities (§45).
- Handle values are opaque in the SDK. `origin_abi::syscall` moves out of the crate holding the stable items.

### N10. Versions

Interface versions are ABI identifiers, separate from the product release (§63), and negotiated at connection time (§30, ADR-0005 P8). Within a major version only additions are allowed. All first-party modules still share one release identity (§62).

### N11. Three things called a console

1. The **early kernel console** (ADR-0010) runs before any SDK can exist and must not depend on one.
2. The **kernel log** is exposed through the Diagnostics interface, read with a capability.
3. The **recovery console** and every user-visible terminal are SDK clients.

### N12. The exceptions, a closed list

These implement the SDK's guarantees, or run where it cannot exist:

- the UEFI loader, in the firmware's world;
- early boot, descriptor and page-table set-up, and the AP trampoline;
- exception, NMI, machine-check and timer entry;
- the context switch, TLB shootdown, page-table and IOMMU programming;
- system-call entry and user-memory copies, which are the SDK's bottom layer;
- panic and urgent console output, with locks held and no heap;
- the first process and the Loader Service, up to the point where `origin.core` is mapped;
- host developer tools (`xtask`, `onbf-pack`, `onbf-sign`), which run on macOS or Linux and share code as libraries, until Origin hosts its own builds.

### N13. Enforced by the gates

`cargo xtask check` fails if: `unsafe` appears outside the framework crate (with a shrinking allowlist during migration); a dependency runs the wrong way; any crate but `origin.core` uses `syscall` or the private call numbers; first-party system code starts a first-party tool as a process; generated code is out of date; or an interface's ABI fingerprint changes incompatibly.

### N14. Order of work

Each step is its own task (§78), with tests defined first.

1. The Framekernel framework crate and its gate, before the kernel grows further.
2. OIDL and its generator, with the kernel's own calls as the first consumer.
3. Caller identity on channels, the start record and the service directory; memory objects and ports (M4's next slices).
4. `origin.core`, then one end-to-end service domain: Diagnostics first, because it is the smallest. Measure it (N8).
5. Files and Processes, then the shell and terminal.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| A stable SDK per module, as first asked | The simplest statement of the rule | Hundreds of interfaces frozen for good, including internals that must change (§7, P8). It also multiplies versioned units against one release identity (§62) |
| The directive as written: only the GUI goes through the SDK | No extra work | Leaves command-line backends as a second control plane, with shell injection and text-format coupling |
| Every kernel module behind IPC | Maximum isolation | Not a framekernel. A message per internal call breaks P1 and P2 |
| Stable system calls, as on Linux | No library required | Contradicts §42 and ONBF §6.1, and freezes the kernel's interface |
| An existing interface language (FIDL, Cap'n Proto, Protocol Buffers) | Mature tools | Their generated code would enter the shipping runtime (§3, §5). Each brings a large toolchain, and Origin needs a small subset |
| Rust traits and procedural macros as the definition | No new language | No source of truth for the C ABI, and it ties the interface to the compiler's view of Rust (§42) |
| OCBOR-1 as the message format | Already built | Self-describing and canonical: slower, and not usable in place |
| Keep the safe-library, unsafe-binary split (ADR-0003 K1) | No refactor | The review shows it is no trust boundary |

## Evidence

The ledger `docs/research/2026-11-09-native-sdk.md` records the sources and what was and was not read. In short: Zircon and Apple make a library the only door to the kernel, and Zircon and OpenBSD have the kernel enforce it per call. Fuchsia's RFC-0190 makes one definition the source of truth for its system calls because hand-kept definitions drifted. Asterinas shows a framekernel in which only the framework may use `unsafe`. Binder shows kernel-supplied caller identity, and Chromium's guidance explains why a capability does not provide it. From this repository, the pre-merge review measured what is missing: 310 `unsafe` sites in 21 files, no interface definitions, and channels with no sender identity. No performance figure here comes from real hardware.

## Trade-offs

- **The Framekernel split is a large refactor** of about 12,000 lines, and it should come before more kernel features. That delays M5 and M6 work.
- **The generator becomes critical tooling.** One bug in it repeats in every interface, so it needs its own fuzzing and review.
- **Every capability needs an interface before a tool can use it.** That is slower than writing a quick command, and it is the point.
- **Stable promises are expensive to keep.** Publishing per domain keeps their number small.
- **Caller identity costs bytes** on every message.

## Migration consequences

- No on-disk format changes. `user-abi` stays at major 0, with no compatibility promise yet, so `origin-abi` can be reorganized freely now. After the first release with third-party programs this becomes much harder, which is the reason to decide now.
- ADR-0003 K1 would be superseded when the framework crate lands. ADR-0005 gains Amendment 2.
- Removing a published SDK interface later is a compatibility change and must follow rule 39.

## Threat model (§58)

| # | Threat | Defence |
|---|---|---|
| S1 | A program skips the library and issues raw system calls | Per-number pinning (N5). Authorization never relies on the library (N9) |
| S2 | A forged or malformed message reaches a service | Generated validating decoders, before service code runs (N7, N9) |
| S3 | A sender claims another identity | The kernel stamps identity, and the sender cannot set it (N9) |
| S4 | Confused deputy: a service uses its own authority for a caller | Handles carry rights and are passed by the caller. Services act on the caller's handles |
| S5 | Version downgrade or a mismatched peer | Negotiation at connection time. Unknown majors are refused (N10) |
| S6 | Resource exhaustion through a service | Per-process quotas, as M4 already charges channel bytes. Bounded decoders. Each service sets its own limits |
| S7 | A bug in the generator is repeated everywhere | Generated code contains no `unsafe` outside named C-ABI shims. Hostile-message corpus. Reviewed like the parsers (§73) |
| S8 | A bad safe API lets safe code break memory safety | Soundness of the Framekernel SDK is the framework's proof obligation, with Loom and Kani where they apply (§73) |
| S9 | A malicious directory hands a program a fake service | The launcher builds the directory. A service's identity can be checked through the stamped sender (N9) |

## What this does not decide

The OIDL grammar, the Driver ABI, the graphics path, any POSIX layer, how `std` maps onto the SDK (§44), and numeric performance budgets.

## Questions for the owner (1–6 answered 2026-11-09; 7 open)

1. Are command-line tools, the shell, the terminal and the recovery console SDK clients with no private doors, going further than §48 and §70 (N1, N2)? **Yes (2026-11-09).**
2. Stable promises per capability, with every module having a typed interface, rather than a stable SDK per module (N3, N4)? **Yes (2026-11-09).**
3. The Framekernel framework crate first, before the kernel grows further, at the cost of delaying M5 and M6 work (N6, N14)? **Yes (2026-11-09).**
4. A small original interface language rather than adopting one (N7)? **Yes (2026-11-09).**
5. Kernel-stamped caller identity on every channel message, as an amendment to ADR-0005 (N9)? **Yes (2026-11-09).**
6. Each system-call number accepted only from its own address in `origin.core` (N5)? **Yes (2026-11-09).**
7. If accepted, will you amend CLAUDE.md §48 and §70 to say this? They are your text, so Claude has not changed them.
