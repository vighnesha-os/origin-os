# ADR-0011: The native SDK is the only way to the operating system

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-09, as recommended: questions 1–6 below, decision sheet items 33–38 (CLAUDE.md §18, §84). Question 7 was answered the same day (item 39). **Amendment 1 (below): Accepted** by the owner on 2026-11-10 (item 108); not built. **Amendment 2 (below): Accepted** by the owner on 2026-11-12 (items 197–203, 205, 213, 216, 218 and 219): what N14 step 4 needs; not built. N6, and N13's gates on `unsafe` and on dependency direction, are built (2026-11-09: the framework crate `framekernel/frame/`, with no `unsafe` left outside it; `docs/engineering/SOURCE_PROVENANCE.md`), which is step 1 of N14; N1–N5, N7–N12, the rest of N13 and the rest of N14 are not built. The owner set the direction the same day: "100% native SDK (if possible)", with the console, the terminal and the kernel included, and "every module as SDK" |
| Requires | CLAUDE.md P1, P2, P3, P8; §3, §5, §7, §13, §22, §30, §42, §44, §45, §47–§50, §53, §58, §62, §63, §66, §70, §72; final rules 5, 10, 11, 12, 20, 21, 36 |
| Evidence | `docs/research/2026-11-09-native-sdk.md`; `docs/engineering/PRE_MERGE_REVIEW_2026-11-09.md` |
| Builds on | ADR-0001 (framekernel argument R1, user-mode drivers D2, default 10); ADR-0003 K1; ADR-0005 (P1, P4, P6, P8, P12, A6); ONBF §6.1 |
| Would change | ADR-0003 K1 (the safe-library, unsafe-binary split), ADR-0005 (Amendment 2: caller identity and per-call pinning), `origin-abi`'s layout. CLAUDE.md §48 and §70, amended to match in revision 7.2 (question 7) |

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

## Questions for the owner (all answered 2026-11-09)

1. Are command-line tools, the shell, the terminal and the recovery console SDK clients with no private doors, going further than §48 and §70 (N1, N2)? **Yes (2026-11-09).**
2. Stable promises per capability, with every module having a typed interface, rather than a stable SDK per module (N3, N4)? **Yes (2026-11-09).**
3. The Framekernel framework crate first, before the kernel grows further, at the cost of delaying M5 and M6 work (N6, N14)? **Yes (2026-11-09).**
4. A small original interface language rather than adopting one (N7)? **Yes (2026-11-09).**
5. Kernel-stamped caller identity on every channel message, as an amendment to ADR-0005 (N9)? **Yes (2026-11-09).**
6. Each system-call number accepted only from its own address in `origin.core` (N5)? **Yes (2026-11-09).**
7. If accepted, will you amend CLAUDE.md §48 and §70 to say this? They are your text, so Claude has not changed them. **Yes (2026-11-09; decision sheet item 39): Claude drafts the wording and the owner approves it before it goes in. The owner approved it the same day: CLAUDE.md revision 7.2.**

## Amendment 1 (Accepted, 2026-11-10): N1's rule gets a mechanism

**Status: Accepted by the owner on 2026-11-10, as recommended ("Accept all", decision sheet item 108; CLAUDE.md §18). Nothing in it is built.** The evidence is finding 3 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`. N1, N2 and N13 stand. This adds to N13, and says where N1 holds at run time.

**Why.** N13 lists a gate: "first-party system code starts a first-party tool as a process". It is a check of source text. It is not built, and it could not see what a program does when it runs: a helper crate or a general "run" method would pass it. Nothing breaks N1 today, because no shipping crate starts a process and no interface creates one. The mechanism has to exist before the Loader Service's interface does.

| # | Decision |
|---|---|
| M1 | **Starting a program is a named capability** on the Loader Service's interface, declared with OIDL's `requires` (`docs/spec/OIDL.md` §6). Its name is decided with that interface (§84). |
| M2 | **A release gate reads every first-party manifest** (ONBF §6.2, `capabilities`). Only the session launcher, the shell, the terminal and developer tools may declare the capability. The desktop, Settings, the installer, Origin Setup and every service never do. |
| M3 | **A program without the capability is never handed the launch interface.** Its launcher builds its directory from what it declared (N9), so the rule holds at run time and not only in review. |
| M4 | **The base image holds no command interpreter.** The shell and the terminal are an optional developer package (§48). |
| M5 | **The environment carries nothing by default,** and no system library reads it. First-party services take their configuration through typed interfaces, never through arguments (ADR-0005 Amendment 6, E2). |
| M6 | **A GUI test reads the kernel's record of process creation,** and fails if a first-party GUI component created a process (§72). |
| M7 | **No interface takes text that its receiver executes or hands to a program.** This is a review rule for every OIDL definition. |

**Alternatives.**

| Alternative | For | Against |
|---|---|---|
| N13's source gate alone | Nothing new to build | It reads source, not behaviour |
| No launch interface for first-party code at all | Nothing to misuse | The session launcher, the shell and the terminal must start programs (N2) |
| M6 alone, as an audit | Simple | It finds a violation after it has been written, and only on the paths a test walks |

**Trade-offs.** One more capability and one more gate to keep. A first-party component that truly needs to start a program has to be added to M2's list, in review, where the reason is seen.

**Migration consequences.** None now: no EXE or APP manifest exists yet, and ONBF's key registry assigns capability keys when those profiles are implemented. After a release, taking the capability away from a component that shipped with it is a compatibility change (rule 39).

**Threat model (§58), one more row.**

| # | Threat | Defence |
|---|---|---|
| S10 | A first-party component is made to run a command: text that reaches a service is passed to a program, or a helper is started with arguments an attacker shaped | The component holds no way to start a program (M1–M3), nothing reads an inherited environment (M5), and no interface takes text to execute (M7) |

## Amendment 2 (Accepted, 2026-11-12): what N14 step 4 needs, `origin.core` and the Diagnostics domain

**Status: Accepted by the owner on 2026-11-12, every item with the recommended answer ("Accept all 30", decision sheet items 197–220; CLAUDE.md §84). Nothing in it is built.** The options, the evidence and what was rejected for each are in step 4's research ledger, `docs/research/2026-11-11-n14-step4-scope.md`, section 5; its candidate n is decision item 196 + n. The other twelve answers are ADR-0005 Amendment 8 (items 204, 206–212, 214, 215, 217 and 220), written in the second step-3 batch beside Amendment 7 and reaching `main` with it. N1–N14 and Amendment 1 stand. Where a row below is more exact, the row governs, and L12 replaces N8's last sentence.

| # | Item | Decision | Refines |
|---|---|---|---|
| L1 | 197 | **Until the Loader Service exists, step 4's programs reach ring 3 inside the kernel's image.** At build time xtask links each program, and `origin.core`, with `rust-lld` for CLAUDE.md §11's triple; checks the output against ONBF §6.1.2's hard errors with `onbf-pack`'s ELF reader; and compiles into the kernel a private description: each segment's bytes and permissions, the entry, the relative relocations, and which import slot takes which export of `origin.core`. At run time the launch path places the program at a random base, applies the relocations, binds the imports (L3), makes them read-only and starts it. Nothing is parsed at run time. The KRN image's signature covers the carried bytes. The EXE and LIB profiles and the Loader Service stay milestone M6's (CLAUDE.md §82). | ONBF §8.3 (a note there); ADR-0005 C7 (relies on N12) |
| L2 | 198 | **`origin.core` is one image per release, with no writable data and nothing to relocate,** mapped read-execute into every process at a random place before its first thread, once, and never unmapped or changed. One set of frames serves every process. It is not a mapping `memory_unmap` or `memory_protect` can name, since both act only on a whole mapping `memory_map` made (ADR-0005 D3), and a fixed `memory_map` over it is `invalid_args`, as over any mapped page. | N5; ADR-0005 P6; ONBF §6.1 (LIB) |
| L3 | 199 | **The launch path binds each program's imports from `origin.core`** from a table made at build time, and makes them read-only before the first thread: ONBF §6.1's eager binding and full RELRO, done by the kernel for the programs it carries. A program holds no call number and no offset of `origin.core`. The start record gains nothing in step 4. | ADR-0005 P6; ONBF §8.3 |
| L4 | 200 | **In step 4 `origin.core` holds the generated stubs and nothing else.** ADR-0005 P8's synchronous call is composed by the generated channel client from the three stubs. P8's helper and its kernel fast path enter `origin.core` together, once the fast path is designed with N8's figures and OIDL can declare a library call. A program that composes the call keeps working when the helper arrives, since the stubs stay. | ADR-0005 P8 (when, not where); OIDL §11, §18 |
| L5 | 201 | **`origin.core` exports exactly the entry points OIDL generates for `origin.kernel`, and its version is `user-abi`.** Handles are 64 bits (ADR-0005 E1, task S4-0); names, statuses, booleans and structures are as OIDL §10 and Q10 say. Each export carries the minor its method arrived in, as ONBF §6.1's exports carry `since_abi_minor`. At major 0 nothing is promised (ADR-0005 P12); from major 1 each export is frozen with its method (OIDL §8, §9). Nothing else is exported: no helper (L4), no allocator, nothing that reads an environment (M5; ADR-0005 E2). **The System V x86-64 psABI revision is pinned (CLAUDE.md §17) before the first export is published (task S4-2).** | N4; OIDL §10 |
| L6 | 202 | **Each stub is a naked function** (`#[unsafe(naked)]` with `naked_asm!`), emitted once. A gate reads `origin.core`'s linked image: exactly one `syscall` for each method, at the offset the kernel's table holds, preceded by the load of its own number. It also reads every program image the kernel carries: no `syscall` in any of them, the kernel's proof programs excepted (L8). | N13; OIDL §10, §11; ADR-0005 B2 |
| L7 | 203 | **The kernel knows each call's offset in `origin.core` from the build, and records `origin.core`'s base when it maps it** (Zircon's method, not OpenBSD's table registered by a loader). The pin is generated from `origin.kernel.oidl` in the same build as `origin.core`, and OIDL §13's release fingerprint ties the two. The check is `rip − 2 == base + offset(number)`, written as comparisons as the generated dispatcher is, with no table indexed by the number (ADR-0017 Z9, Z10). How the kernel recognises `origin.core` when the Loader Service maps it from a sealed object is decided with the Loader Service. | N5; ADR-0005 B2; OIDL §18 |
| L8 | 205 | **The kernel's own proof processes keep ADR-0005 A6's range; every other process is pinned per call.** Which of the two a process is, is fixed when it is made. N13's gate on any crate but `origin.core` that uses `syscall` excepts the kernel's proof programs by name. | ADR-0005 A6, B2; N13 |
| L9 | 213 | **Step 4 goes ahead with row I2 of the system-call threat model at "Partly".** Step 4's programs are first-party and run under QEMU and as KVM guests only. Where a processor offers none of ADR-0017's controls, programs are not protected from each other's speculation, which acceptance AC6 (item 138) accepts for bench machines, and boots under QEMU keep ending "degraded" (item 154). I2's closure moves to: **before any claim about real hardware, and before any program that is not first-party runs in ring 3.** No machine in the declared scope can be rented, and buying is off. Renting a bench machine with Zen 5 or recent Intel cores would show the controls written on silicon, but it lies outside the scope (ADR-0009 A1), so it claims nothing and does not close I2; it is external spending, which is the owner's (CLAUDE.md §1). None is rented. | N14 step 4; `THREAT_MODEL_SYSTEM.md`, "What must exist"; `THREAT_MODEL_SYSCALLS.md`, "What must be closed", item 3; AC6 |
| L10 | 216 | **`origin.diagnostics` 0.1 serves the boot report.** The boot's verdict (ADR-0013 R6): how many proofs passed, failed and did not run, and the names of those that failed, as fixed strings of the kernel's table and never a formatted message. For each processor: what CPUID reports it is (vendor, family, model, stepping), and which of ADR-0017's controls it set and which it lacks (Z1's "served through a typed interface"). Fixed-size records, with no field that can hold an address. Its methods require a named capability that the launcher grants (OIDL §6), since the record tells a reader which defences are missing. ADR-0009 A2's scope verdict joins when TASKS item 16's table exists. The kernel log comes in a later minor, once the kernel writes one and addresses are out of it (ADR-0004 H3's note, item 119). The exact records are put to the owner as shapes with task S4-5, and the Diagnostics domain gets an ADR of its own then. | N11; N14 step 4; ADR-0017 Z1 |
| L11 | 218 | **What N8 measures in step 4.** The typed round trip, client to service and back, for a small reply and for the vector of processor records, with the client and the service on one processor and on two. At least 10,000 calls a case, so that p99.9 rests on ten samples above it; p50, p95, p99 and p99.9, with the count (CLAUDE.md §1). On the same boot: the channel ping-pong, a null call through `origin.core`, and `clock_read`, so that N8's three costs sit side by side. Printed at every boot as a Report result. A provisional figure goes into `docs/engineering/PERFORMANCE_BUDGETS.md` before the first measurement, marked a hypothesis (item 115) and derived from a stated model: the calls, switches and barriers in one round trip, each costed from what the boot prints. | N8 |
| L12 | 219 | **N8's figure on the declared hardware is owed before any performance claim and before release qualification. Step 5 does not wait for it.** Until that hardware exists, the figures from QEMU and from CI's KVM guests go into `PERFORMANCE_BUDGETS.md` as trends beside the hypothesis (CLAUDE.md §53, item 115). Neither offers IBPB, so their figures leave out the barrier ADR-0017 Z3 adds at each switch between processes on a processor that has one: two in each round trip made on one processor. This replaces N8's "before a second one is built". | N8 |

**Recorded, and no decision.**

- **S4-0 comes first.** ADR-0005 E1's 64-bit handles (item 109) land before `origin.core`, and take `user-abi` to 0.7.
- **Generated code.** L6's naked stubs, and S4-4's server handed the stamped identity (OIDL §18's `Caller`), change what the generator writes and no wire format. OIDL §10, §11 and §18 are revised to match when they are built.
- **Versions.** `origin.directory` goes from 0.1 to 0.2 in S4-4, and `origin.diagnostics` starts at 0.1 in S4-5. ADR-0005 Amendment 8's new status (G7) and report call (G11) each take one more minor of `origin.kernel` (OIDL §9).
- **CLAUDE.md §11** says `onbf-pack` "converts the linker output to `.ose`". It is the owner's text and is not changed. Under L1 that conversion waits for M6.

**Trade-offs.** Under L1 each change to a carried program rebuilds and re-signs the kernel, until the Loader Service exists. Under L4 a synchronous call is three system calls until the fast path is designed. L8 keeps two kinds of process, with two pins, while the proof programs stay off `origin.core`. L9 and L12 let steps 4 and 5 go ahead with no evidence from a declared machine, and nothing may be claimed from them.

**Migration consequences.** None for programs outside the tree: `user-abi` is at major 0 (ADR-0005 P12). When the Loader Service exists, L1's carried programs become EXE images it maps, and how L7's base is recorded then is decided with the Loader Service.

**What stays unknown.** The ledger's section 6: whether a naked function can ever be duplicated by the compiler (L6's gate holds the rule either way); whether `origin.core` links with nothing to relocate; the form of the kernel-private description; the round trip's cost; the psABI revision. When built, step 4 runs under QEMU and as KVM guests in CI. Nothing will have run on a declared machine.

## Amendment 3 (Accepted, 2026-11-13): `origin.core`'s C entry points under the pinned psABI

**Status: Accepted by the owner on 2026-11-13, each item with the recommended answer ("Accept all 5", decision sheet items 273 and 274; CLAUDE.md §84). L13's sentence on enums was corrected the same day ("Accept both", item 277). Nothing in it is built.** Amendment 2's L5 asked for the psABI revision to be pinned before task S4-2 publishes the first export. Pinning it (decision item 271; CLAUDE.md 7.7 §17; `docs/research/2026-11-12-psabi-revision.md`) showed one thing the accepted texts did not cover, L13. L14 answers a question the independent review of task S4-1 left for S4-2 (finding F11).

| # | Item | Decision | Refines |
|---|---|---|---|
| L13 | 273, 277 | **Each generated stub extends every parameter narrower than 64 bits to its whole register before the call.** Zero for an unsigned integer, a `bool` and a bits type; the sign for a signed integer; an enum as its underlying integer, zero where that is unsigned and the sign where it is signed. *(Corrected 2026-11-13, decision item 277: this sentence said "Zero for an unsigned integer, a `bool`, an enum and a bits type". OIDL §3 lets an enum sit over a signed integer, and the kernel's decoder reads such a register as signed, so a negative member extended with zeros would have been refused.)* The pinned psABI leaves the bits of an argument register beyond the parameter's type unspecified, and says the side that consumes the value extends it (the ledger's row R5). The kernel's register decoding judges the whole register (OIDL §10: "the value fits", "correctly sign-extended", "`0` or `1`"), and a naked stub (L6) passes the caller's register on as it is. So the stub, which is where the C call is consumed, extends; and the kernel's check stays as it is, so a call that does not come through a stub is still refused when its register does not fit. No C signature, wire format or call number changes. OIDL §10 gains the rule when S4-2 builds the stubs, as Amendment 2 says of the stubs' form. | L5, L6; OIDL §10; CLAUDE.md §10 |
| L14 | 274 | **`origin.core` reserves no exit codes at major 0.** `thread_exit`'s code is the program's, the whole `u64`. The three codes `origin-rt` ends a thread with (a panic, a start record that does not decode, an unwind) are that runtime's own documented convention: they share the program's space, and nothing may depend on them. How a process's end is reported, an exit apart from a panic and from a fault, is decided with P10's crash record and the process-creation calls (ADR-0005 G4), before `user-abi` reaches major 1. | L5; ADR-0005 P10 and P12 |

**Options that were put.** For L13: (a) as decided; (b) every register parameter a 64-bit type in the C signature, which puts the widening on every caller and makes the header say less than the interface; (c) the kernel ignores the upper bits, which weakens its check for every caller that is not a stub (the kernel's proof processes, L8). For L14: (a) as decided; (b) reserve a range of codes for the runtime and the system now, which fixes a detail of the ABI before the reporting of a process's end is designed.

**Recorded, and no decision.**
- **L6's gate.** The gate reads each stub for the load of its own number before its one `syscall`. The extensions sit before that load, and the gate has to allow exactly them: an extension of a parameter register of that method, and nothing else. That is S4-2's to build, with a control.
- **Results.** A status comes back in the whole of RAX as `int64_t`, and every other result through a pointer of its own size, so nothing on the way back needs extending.
- **The fourth argument.** A C caller passes it in RCX and the kernel takes it in R10 (ADR-0005 P4). That move is part of a stub already, and is not this decision.

**Performance cost (ADR-0018 V3).** L13: one register-to-register instruction for each parameter narrower than 64 bits, in each call. Unmeasured and owed: S4-2's exit tests already ask for the cost of one call before the pin and after it, and the figure is reported with them, under QEMU and as KVM guests in CI, as a trend (CLAUDE.md §53). L14: none, by construction.

**Trade-offs.** L13 spends an instruction on a caller whose compiler had already extended the value, which is most callers; in return a correct call is never refused for what a compiler left beside its argument. L14 leaves a parent unable to tell a panic from a program's own exit code for now.

**Migration consequences.** None for programs outside the tree: `user-abi` is at major 0 (ADR-0005 P12). The generated stubs change form with S4-2 in any case (L6).

**What stays unknown.** What Rust 1.99.1 and a C compiler each leave in the upper bits in practice was not measured, and L13 does not depend on it. When built, this runs under QEMU and as KVM guests in CI only.
