# ADR-0005: Process ABI, capability handles and IPC

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-07: the four questions below answered with the recommended option each, then the whole ADR (P1–P12) accepted as proposed. Implementation in progress. **Amendment 1 (below): Accepted** by the owner on 2026-11-08, with the recommended option of each item. **Amendment 2 (below): Accepted** by the owner on 2026-11-09, through ADR-0011. |
| Scope | Kernel objects and handles, the system-call convention, status codes, process and thread start, memory mapping, IPC, waiting, faults, termination, ABI versioning |
| Builds on | ONBF §6.1 (EXE/LIB images, syscall-origin pinning, TLS in FS), §8.2 (sealed objects, the Code Integrity Table, mapping rules), §8.3 (the user-mode Loader Service and the start record); ADR-0001 D2 (user-mode drivers); ADR-0004 (address-space layout) |
| Requires | CLAUDE.md §29 (processes and isolation), §30 (IPC and capabilities), §42 (executable ABI), §44 (runtime), §49 (SDK) |

## Context

M4 has per-process address spaces, ring 3, one system-call gate and a fault-safe user copy (ADR-0003 K8). Everything above that is user-visible ABI: how a program names kernel objects, calls the kernel, receives results, starts, waits and talks to services. ONBF already decided three things this ADR must respect:
- system-call numbers are private to each release, and only `origin.core`'s code may issue system calls (syscall-origin pinning); the stable interface is the C ABI of the Origin system libraries;
- the kernel never parses ONBF for userspace: the user-mode Loader Service maps images, and the kernel allows executable mappings only from sealed objects whose Merkle root is in the Code Integrity Table (CIT);
- a process receives its initial capability handles in a start record.

## Proposed decisions

### P1. Capability handles, no ambient authority
A process names every kernel object through a **handle** in its own handle table: a 32-bit value (table index and a generation, so a stale handle never names a reused slot; 0 is never valid). Each handle carries **rights**. There are no global names in the kernel: no path lookups, no process IDs that grant access. A process can act only on objects it holds handles to; files, devices and services are reached through handles that services hand out over IPC.
*Alternatives:* POSIX-style descriptors plus global paths and PIDs (familiar, but every global name is ambient authority that sandboxing must then take away).

### P2. Rights only shrink
Rights include `read`, `write`, `map`, `execute` (map RX, still subject to the CIT), `duplicate`, `transfer`, `wait`, `signal` and `manage`, plus per-type rights. `duplicate(handle, rights)` may only remove rights; a handle sent over IPC keeps at most the rights it had. No operation ever adds a right.

### P3. Object types in v1
`Process`, `Thread`, `MemoryObject` (anonymous, or sealed from the Package Service), `Channel` (an IPC endpoint pair), `Event`, `Port` (a completion queue), `Timer`. Driver objects (`Interrupt`, `IommuDomain`, `MmioRegion`) follow in the driver-ABI ADR (ADR-0001 D2).

### P4. The system-call convention
- `syscall`; RAX = the call number, private to the release (ONBF §6.1) and accepted only from `origin.core`'s code range.
- Arguments in RDI, RSI, RDX, R10, R8, R9 (R10 replaces RCX, which `syscall` overwrites). RCX and R11 are clobbered; every other register is preserved.
- RAX returns a `Status` (P5). Further results go through user out-pointers. A call that needs more than six arguments takes a pointer to a structure.
- Every structure crossing the boundary begins with a `u32 size`, so a newer minor version can append fields, and is decoded field by field (ONBF-R7). A boolean is a `u8` checked to be 0 or 1 (CLAUDE.md §10).
- Every user pointer is checked against the calling process's own tables (present, user, and writable where written) and copied with fault recovery; the kernel never otherwise dereferences user memory.

### P5. Status codes
One stable signed enumeration, part of the C ABI: `OK = 0`, then negative values such as `INVALID_ARGS`, `BAD_HANDLE`, `WRONG_TYPE`, `ACCESS_DENIED`, `NO_MEMORY`, `FAULT` (a bad user pointer), `SHOULD_WAIT`, `TIMED_OUT`, `PEER_CLOSED`, `CANCELED`, `NOT_SUPPORTED` and `LIMIT`. Values never change meaning; new ones are only appended. A POSIX layer, if one is ever built, maps them to `errno`.

### P6. Process and thread start
- The Loader Service creates the process, maps it (ONBF §8.3) and starts its first thread. Registers at entry: RIP at the image entry, RSP 16-byte aligned, RDI pointing to the read-only start record, every other general-purpose register zero, default x87 and MXCSR state, the CET shadow stack enabled (ONBF §8.2).
- The start record (`size`, ABI version, the kernel's supported ABI range, a handle list with a role per handle such as the bootstrap channel, its own process and thread, and the services it was granted, then arguments and environment as byte strings) is the only thing a process starts with.
- The thread pointer is FS, set through a thread system call (ONBF §6.1.1). GS stays the kernel's: entry from user mode uses `swapgs`, and user code must not rely on GS in v1.

### P7. Memory
- `MemoryObject`s are created anonymous and zero-filled, committed on demand, or handed out sealed by the Package Service.
- `map(process, object, offset, length, address, rights)` places a mapping at a random address unless a fixed lower-half address is asked for. W^X is absolute; RX is allowed only from a sealed object whose root is in the CIT, or with the `codegen.jit` capability (ONBF §8.2). `unmap` and `protect` (which may only reduce access, except for the JIT rule) complete the set.
- Bulk data between processes moves by sharing a `MemoryObject`, not by copying messages.

### P8. IPC: asynchronous message channels
- `channel_create` returns two endpoints. `write` queues a message of up to **64 KiB** of bytes and up to **64 handles**, which are moved out of the sender's table (rights preserved, P2). `read` takes the next message, or returns `SHOULD_WAIT`.
- Messages are copied once into the kernel and once out. Each channel has a bounded queue, and a full queue returns `SHOULD_WAIT` (backpressure).
- Closing an endpoint signals `PEER_CLOSED` to the other.
- A synchronous call (write, wait, read) is an `origin.core` helper, with a kernel fast path to come once measured. Typed protocols, versioned at connection time (CLAUDE.md §30), are an SDK layer above channels.

### P9. Waiting, completion queues, cancellation, time
- Objects raise **signals** (`READABLE`, `WRITABLE`, `PEER_CLOSED`, `SIGNALED`, `TERMINATED`). `wait_one(handle, signals, deadline)` blocks until one is raised or the deadline passes; deadlines are on the monotonic clock in nanoseconds (CLAUDE.md §68).
- A `Port` is a completion queue. Objects are bound to it with a key, and waiting on the port returns packets: one thread can serve many objects, and asynchronous I/O completes there.
- Cancellation unbinds or cancels a pending wait, which completes with `CANCELED`.

### P10. Faults and exceptions
There are no POSIX signals and no asynchronous upcalls. A fault in user mode stops the faulting thread and sends an exception message to the process's exception channel, if one is bound (a debugger or crash handler). Otherwise the process is terminated, with a crash record for its parent. The kernel itself is never harmed (crash isolation, CLAUDE.md §29; today's behaviour, which ends the thread, is the special case without the channel).

### P11. Termination, accounting, quotas
Exiting, or being terminated through a `manage` handle, closes every handle the process holds, unmaps its memory and raises `TERMINATED`. Memory, handles and threads are accounted per process against quotas set by its creator (CLAUDE.md §26, §29).

### P12. Versioning
The Origin userspace ABI version `{major, minor}` (a new `[compatibility] user-abi` key in `origin-version.toml`, separate from the release version, ONBF-R8) is checked by the Loader Service against each image's `abi`. Because `origin.core` ships in the same unified release as the kernel, system-call numbers may change between releases without breaking a single program.

## Consequences

- `copy_from_user` must check the calling process's tables, not the kernel's (today's limitation, ADR-0003 K8).
- `swapgs` arrives with the first user-settable GS-free entry path; the kernel's GS base stops being visible to user mode at all.
- The kernel needs a handle table, object reference counting and the waiting machinery before any service can run. The Loader, Package and Device Services are user-mode programs built on these calls.

## Evidence and comparisons

The handle-and-rights model follows the capability tradition (KeyKOS, EROS, seL4) and its use in a mainstream system (Zircon's handles and rights). Asynchronous channels with handle transfer and ports follow Zircon and Mach ports. Windows' completion ports show that completion queues scale for asynchronous I/O. These are conceptual references only: no source is used (CLAUDE.md §3).

## Questions for the owner (answered 2026-11-07: the recommended option for all four)

1. Capability handles with per-handle rights and no ambient authority (P1–P2)?
2. Asynchronous message channels that move handles, with shared memory objects for bulk data (P8), rather than synchronous rendezvous IPC?
3. Faults go to an exception channel, else terminate the process; no POSIX signals (P10)?
4. Message limits of 64 KiB and 64 handles (P8)?

## Amendment 1 (Accepted, 2026-11-08): what the first system-call slice needs and P1–P12 do not say

**Status: Accepted by the owner on 2026-11-08, with the recommended option of each item (CLAUDE.md §84).** Each item states behaviour the first system-call slice had to define; the ADR's accepted text (P1–P12) is unchanged. The alternatives are kept for the record.

| # | Question | Decision (implemented) | Alternative |
|---|---|---|---|
| A1 | Which rights does a new channel end get? | `READ`, `WRITE`, `DUPLICATE`, `TRANSFER`, `WAIT` (`rights::CHANNEL`): everything that applies to a channel, since no operation can ever add a right (P2) | Omit `DUPLICATE`: ends are then unique by construction, and A3's second rule goes away, but `duplicate` means nothing for channels |
| A2 | How does `wait_one` end besides a signal and a deadline? | Closing the waited-on handle, or moving it away in a message, completes the wait with `CANCELED`. A deadline of 0 is a poll, and `signals` of 0 or with undefined bits is `INVALID_ARGS` | Waits keep going until a signal or deadline, ignoring close (P9 says only that cancellation exists) |
| A3 | Which handle transfers does `channel_write` refuse? | `INVALID_ARGS` for a channel end that (a) is an end of the channel being written, (b) has another handle anywhere, or (c) would end up, through queues, inside its own queue. Without these a process can build a reference cycle that no process can ever read again, and the kernel can never free | Allow them and reclaim cycles with a garbage collector later; or (b) only, forbidding `DUPLICATE` on channel ends (A1) |
| A4 | What does `channel_read` report? | Its `out_sizes` is `{u32 size; u32 bytes; u32 handles}` (P4: starts with `size`, set by the caller, at least 12). A message too big for the buffers stays queued and returns `BUFFER_TOO_SMALL` with its sizes; a reader whose table cannot hold the message's handles gets `LIMIT`, and the message also stays | Dropping the message on a read that cannot hold it |
| A5 | When does a process end? | When its last thread ends, by `exit` or by a fault (P10's no-exception-channel case). There is no separate process exit call in this release's numbers | A `process_exit` call, or terminating a process on any thread's fault (P10) |
| A6 | Who decides the system-call origin before `origin.core` exists? | The creator names the code range for each process. A call from outside it returns `ACCESS_DENIED` | Terminate the process on the violation (ONBF §6.1 says only that such calls are not accepted) |

The implementation records each decision's consequences in `docs/engineering/SOURCE_PROVENANCE.md`.

## Amendment 2 (Accepted, 2026-11-09): caller identity and per-call pinning

**Status: Accepted by the owner on 2026-11-09, through ADR-0011 questions 5 and 6 (CLAUDE.md §84). Neither item is built.** The ADR's accepted text is otherwise unchanged.

| # | Decision | What it changes |
|---|---|---|
| B1 | The kernel stamps each channel message with the sender's identity, which the sender cannot set or forge (ADR-0011 N9). A capability says what may be done; identity says who asked, for per-user policy and audit (CLAUDE.md §30, §48). The fields and their layout are specified with the slice that builds them, and are an ABI decision (§84) | P8 says nothing about identity |
| B2 | Each system-call number is accepted only from that number's own instruction address in `origin.core` (ADR-0011 N5). A call from anywhere else is refused. It is hardening, not authorization | P4's "accepted only from `origin.core`'s code range", and A6 until `origin.core` exists |
