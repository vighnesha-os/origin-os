# ADR-0005: Process ABI, capability handles and IPC

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-07: the four questions below answered with the recommended option each, then the whole ADR (P1–P12) accepted as proposed. Implementation in progress. **Amendment 1 (below): Accepted** by the owner on 2026-11-08, with the recommended option of each item. **Amendment 2 (below): Accepted** by the owner on 2026-11-09, through ADR-0011. **Amendment 3 (below): Accepted** by the owner on 2026-11-09, through `docs/spec/OIDL.md`'s question Q8. **Amendment 4 (below): Accepted** by the owner on 2026-11-09. **Amendment 5 (below): Accepted** by the owner on 2026-11-10. **Amendment 6 (below): Accepted** by the owner on 2026-11-10; not built. **Amendment 7 (below): Accepted** by the owner on 2026-11-11; built by task S3-6 in the second step-3 batch, under QEMU only. **Amendment 8 (below): Accepted** by the owner on 2026-11-12 (items 204, 206–212, 214, 215, 217 and 220): what ADR-0011 N14 step 4 needs; not built. **Amendment 9 (below): Accepted** by the owner on 2026-11-13 (item 272): the flags at a thread's entry. The kernel already starts a thread so; no boot proof reads the flags yet. |
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

**Status: Accepted by the owner on 2026-11-09, through ADR-0011 questions 5 and 6 (CLAUDE.md §84). B1 is built (task S3-2, 2026-11-10, as C1 and C2 decide); B2 is not.** The ADR's accepted text is otherwise unchanged.

| # | Decision | What it changes |
|---|---|---|
| B1 | The kernel stamps each channel message with the sender's identity, which the sender cannot set or forge (ADR-0011 N9). A capability says what may be done; identity says who asked, for per-user policy and audit (CLAUDE.md §30, §48). The fields and their layout are specified with the slice that builds them, and are an ABI decision (§84) | P8 says nothing about identity |
| B2 | Each system-call number is accepted only from that number's own instruction address in `origin.core` (ADR-0011 N5). A call from anywhere else is refused. It is hardening, not authorization | P4's "accepted only from `origin.core`'s code range", and A6 until `origin.core` exists |

## Amendment 3 (Accepted, 2026-11-09): what `channel_read` reports about handles

**Status: Accepted by the owner on 2026-11-09, through `docs/spec/OIDL.md` question Q8 (CLAUDE.md §84). Built by task S3-2 (2026-11-10), through C3.** The ADR's accepted text and Amendment 1's A4 are otherwise unchanged.

| # | Decision | What it changes |
|---|---|---|
| B3 | `channel_read` delivers each received handle as a record `{u32 handle; u32 object_type; u32 rights}` instead of a bare value, so a receiver can check a handle's type and rights before using it, which ADR-0011 N9 requires of the generated stub. It lands in the same slice as B1, while `user-abi` is at major 0 | P8 and A4: the out-handles array of `channel_read` |

## Amendment 4 (Accepted, 2026-11-09): what ADR-0011 N14 step 3 needs

**Status: Accepted by the owner on 2026-11-09, all thirteen items with the recommended answer (decision sheet items 56–68; CLAUDE.md §84). C1, C2 and C3 are built (task S3-2, 2026-11-10, with B1 and B3), C4, C5 and C6 (task S3-3, 2026-11-10: the start record and its launch path), C11 and C12 (task S3-4, 2026-11-10: `not_found` and the service directory, `origin.directory` 0.1), and C7 and C8, with C6's rights for a memory object and its rule for another process's address space (task S3-5b, 2026-11-10: memory objects, `origin.kernel` 0.5, as Amendment 5 decides); the other three are not.** The scoping is `docs/research/2026-11-09-n14-step3-scope.md`. The ADR's accepted text and Amendments 1–3 are otherwise unchanged; where an item below refines a P decision, the item governs.

| # | Decision | Refines |
|---|---|---|
| C1 | A message's stamped identity (Amendment 2, B1) is `{u64 process; u64 principal}`. Process IDs are assigned by the kernel, unique for the boot and never reused; 0 is the kernel. A process ID grants nothing. The principal is opaque, set when the process is created, inherited by default (a different one only through a capability) and immutable. Program identity is added in a later minor. | B1 |
| C2 | The identity reaches the reader through `channel_read`'s size-versioned result (`read_sizes`, which may be renamed while the ABI is at major 0): the identity fields follow the existing 12 bytes, a caller asking for the old size gets no identity and no error, and the fields are written on `BUFFER_TOO_SMALL` too. | B1, P8 |
| C3 | OIDL lets plain structs of integers, enums and bits be elements of system-call vectors and arrays, encoded field by field, never by the compiler's layout. B3's handle records use it. This is a change to OIDL's format (`docs/spec/OIDL.md` §3.1, §10, §18 to be revised to match). | B3, N7 |
| C4 | The start record, version 1: a 32-byte header (`size`, `abi_major`, `abi_minor`, `abi_min_minor`, reserved, then the handle, argument and environment counts and byte totals); 16-byte handle entries `{handle, object_type, rights, role}`; then the arguments, then the environment, each a `u32`-length-prefixed byte string padded to 8. Exact size, zero padding, at most 64 handles and 64 KiB. The kernel writes it, read-only to the process, at a random address, before the first thread runs. | P6 |
| C5 | Start-record roles: `DIRECTORY = 5` is added; `SERVICE = 4` is retired and never reused, because the directory is the only way a process finds services (ADR-0011 N9). | P6 |
| C6 | Default rights. The process's own handle: read, write, manage, wait, duplicate, transfer. Thread: wait, manage, duplicate, transfer. Memory object: read, write, map, duplicate, transfer, and never execute for an anonymous object. Event: wait, signal, duplicate, transfer. Port: read, write, duplicate, transfer. Changing another process's address space needs `write` on its process handle; `manage` is for termination and thread control. | P2, P3, P7 |
| C7 | Memory objects are committed in full at creation and charged to the creator (demand commit comes later, with a user page-fault path and P10). In v1, unmap and protect act on whole mappings; a fixed-address request never replaces an existing mapping; nothing is executable until the Code Integrity Table exists (ONBF §8.2). | P7, P11 |
| C8 | The map call is `memory_map(process, object, map_request) -> address`, with the placement, access and flags in a sized `map_request` struct. | P4, P7 |
| C9 | Ports: one-shot, level-triggered bindings with a `u64` key and packet storage reserved at bind time, charged to the binder. An implicit cancel (the bound handle closed or moved) delivers a `canceled` packet; an explicit `port_cancel` is silent and also removes packets already queued for that object and key, so the key can be reused at once. A packet is `{key; kind; trigger; observed; reserved; data}`, one per `port_wait`. | P9 |
| C10 | Events are implemented now (`event_create`, `object_signal`). In v1, `object_signal` works on events only, and only on `SIGNALED`. | P3, P9 |
| C11 | `not_found = -14` is appended to the status codes. | P5 |
| C12 | The service directory's interface is `origin.directory` 0.1, keyed by the OIDL interface ID and major version (the same ID the hello carries), with a pipelined, two-way `connect(interface_id, major, server)` whose reply carries `not_found` or `limit`. | ADR-0011 N9 |
| C13 | User mode reads the clock through a `clock_read` system call; a shared read-only clock page may come later, with its own ordering argument and measurements (ADR-0012 C7). | P9 |

## Amendment 5 (Accepted, 2026-11-10): what S3-5b's memory objects need and C6–C8 do not fix

**Status: Accepted by the owner on 2026-11-10, all five items with the recommended answer (decision sheet items 101–105; CLAUDE.md §84). All five are built (task S3-5b, 2026-11-10: `origin.kernel` 0.5 and `user-abi` 0.5, the memory calls and their boot proof).** The options, and what was rejected for each, are in task S3-5b's research ledger, `docs/research/2026-11-10-memory-objects.md`. C6–C8 stand; where an item below refines one, the item governs.

| # | Decision | Refines |
|---|---|---|
| D1 | A creation call that takes arguments takes them in a sized request struct, which grows by P4's own rule. `memory_object_create(request: object_request) -> object`, with `object_request { length: u64 }`. A creation call with no argument yet takes none until it has a real one (S3-6's `event_create` and `port_create`; OIDL refuses an empty struct), so from major 1 such a call can gain an argument only as a new call. | P4, C7 |
| D2 | `map_request { access: map_access; flags: map_flags; offset: u64; length: u64; address: u64 }`, 40 bytes. Offsets, lengths and fixed addresses are in bytes, each a multiple of a new constant `page_size` (4096) and never rounded; a length is never 0; `address` is given only with `fixed`, and is 0 otherwise. `map_access { read, write, execute }`: read is required, write with execute is `invalid_args`, and execute is `access_denied` (C7). `map_flags { fixed }`: without it, the kernel places the mapping at random. | P7, C7, C8 |
| D3 | `memory_unmap(process, address, length)` and `memory_protect(process, address, length, access)`. In v1 the range is exactly one whole mapping, and anything else is `invalid_args`; the length is there so that a later minor can allow part of one. Protect only reduces access: an increase is `access_denied`. | P7, C7 |
| D4 | Quotas, set by the creator in the limits it gives the process: the bytes committed by memory objects the process created that still exist, whether held by handles or by mappings; the whole mappings its address space may hold; and a rate for unmap and protect, a burst and units regained per second, with each change priced before it runs at one unit per shootdown and per processor it may interrupt. Past a quota or the mapping limit: `limit`. Without frames: `no_memory`. A change the rate cannot pay for: `should_wait`, with nothing changed; its real cost is charged afterwards. A process given no quota gets nothing. | P11, C7 |
| D5 | The rest. Every call needs `write` on its process handle, the caller's own included. `invalid_args` for a fixed address over a mapping, a process that has ended, and a range that is not one mapping; `access_denied` for execute; `timed_out` for a stalled shootdown, the change made and its frames never reused (`THREAT_MODEL_ADDRESS_SPACES.md` T3); Of unmap and protect, only unmap can return `no_memory`: when the kernel heap cannot replace the bookkeeping a stalled shootdown leaked. The first failure wins, in this order: the pointers and the request, the handles, what is asked, the quotas, the placement. Addresses are page-aligned inside ADR-0004 H1's user range. A random placement takes a page of `[0x1000_0000_0000, 0x7000_0000_0000)`, drawn from RDSEED or RDRAND; with no hardware entropy the map is `not_supported`. A mapping holds its object, so closing a handle never unmaps. New constants: `memory_object_rights` (C6's set) and `page_size`. `user-abi` goes from 0.4 to 0.5 in one step. | P2, P5, C6; ADR-0004 H1 |

**Note, 2026-11-11 (no decision changes).** D5's words "drawn from RDSEED or RDRAND" and "no hardware entropy" describe the source as it was on 2026-11-10. Since task 18b a random placement is drawn from the kernel's one generator (`origin_frame::random`; ADR-0016, decision item 113), and nothing else reads those instructions. The statuses and their order are as D5 says: when the generator refuses a draw, the answer is `not_supported`.

**Note on D4 (Accepted by the owner on 2026-11-10; decision sheet item 142).** D4 says "Without frames: `no_memory`" and does not name the bound on the page-table frames one address space may use (32, the framework's `MAX_TABLES`). The review of S3-5b found that `memory_map` returns `limit` for three causes (`docs/engineering/REVIEW_S3-5B_2026-11-10.md`, finding C). The owner decided what each returns:

- A map that needs a page-table frame past the address space's bound returns **`limit`**. It is a bound on the process, like its quotas.
- A map that fails because the machine has no free frame, or because the kernel heap cannot grow, returns **`no_memory`**.

`memory_map` already declares both statuses, so `user-abi` stays 0.5. **Built 2026-11-11** (the framework's `map_shared_run` tells the causes apart, and the kernel returns the status of each); the `limit` case is shown by a boot proof under QEMU, and `no_memory` by a host test only, since no boot runs a machine out of frames.

**Note on D5 (Accepted by the owner on 2026-11-10, "Accept all"; decision sheet items 155 and 156).** The review of S3-5b found two cases D5 does not name (`docs/engineering/REVIEW_S3-5B_2026-11-10.md`, the low observations). The owner decided what each returns:

- `memory_unmap` or `memory_protect` on a mapping that another thread is changing at that moment returns **`should_wait`**. Nothing has changed, and a retry either succeeds or finds the mapping gone. It is the status both calls already return when the caller's budget cannot pay, so a caller handles the two alike. Until this is built the kernel returns `invalid_args`, which wrongly says there is no such mapping.
- `memory_map` with a random placement returns **`limit`** when eight draws in a row land on something mapped. It is a bound of the process's own address space; `no_memory` stays for frames and the kernel heap (the note on D4). This is what the kernel does today.

Each call already declares its status, so `user-abi` stays 0.5. **Both built 2026-11-11:** the first with a boot proof line under QEMU, the second pinned by a host test.

**Second note on D5 (Accepted by the owner on 2026-11-11, "Accept all"; decision sheet items 162 to 164).** The review of the fixes found three more cases D5 does not place (`docs/engineering/REVIEW_S3-5B_2026-11-10.md`, step 2). The owner decided to keep what the kernel does:

- `memory_map` at a fixed place that runs over a page already mapped returns **`invalid_args`**, also when the address space is at its bound of page tables. The space looks at its records before it makes tables, so it finds the taken place first. `limit` (the note on D4) is for a run whose place is free.
- `memory_protect` on a mapping another call is changing returns **`should_wait`** before the access asked is compared with what the mapping allows. That comparison is not settled while the mapping changes. A retry gets the true answer, `access_denied` included.
- A mapping whose own `memory_map` has not yet written its address to its caller counts as being changed (the first note): `memory_unmap` or `memory_protect` on it returns **`should_wait`**. Until the address is written the kernel may still take the mapping back, so it is its maker's alone.

No status is added and no call declares a new one, so `user-abi` stays 0.5. **Built** (on the batch since `f56263a`). The third has a host test and a boot proof line under QEMU. The first and the second follow from the order of the code (`memory_object::put` reserves the record before it enters pages; `memory_object::protect` takes the mapping before it compares the access). Since 2026-11-11 a line of the memory proof pins both under QEMU (`9434b31`; negative controls 30 to 32 in `docs/engineering/SOURCE_PROVENANCE.md`). The line is made through the calls the system calls make, in processes the proof builds and never runs: no boot shows either order from ring 3.

## Amendment 6 (Accepted, 2026-11-10): 64-bit handles, and what a start record's strings are

**Status: Accepted by the owner on 2026-11-10, with the recommended answer ("Accept all", decision sheet items 108 and 109; CLAUDE.md §84). Neither item is built.** The evidence is findings 3 and 4 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`. E1 changes P1. Where an item below differs from P1, P6, B3 or C4, the item governs.

| # | Decision | Refines |
|---|---|---|
| E1 | **A handle is a 64-bit value.** It is still opaque to programs (ADR-0011 N9) and still never 0, and a stale handle still never names a reused slot. How the kernel divides the bits between a slot and its generation is not part of the ABI. The division must leave a process no practical limit on how many handles it creates in its life: 24 bits of slot and 40 of generation, for example, give about 16 million live handles and about a trillion uses of each slot. `received_handle` becomes `{u64 handle; u32 object_type; u32 rights}`, 16 bytes. `start_entry` becomes `{u64 handle; u32 object_type; u32 rights; u32 role}`, 24 bytes with its padding. A handle argument fills its register, so the decoder's check that the upper 32 bits are zero goes (`docs/spec/OIDL.md` §10). A channel message's slots are positions, not handle values, and do not change (§7.4). The change lands in one `user-abi` minor, while the ABI is at major 0, after S3-5b and S3-6, which change the same calls. | P1; B3; C4 |
| E2 | **A start record's arguments and environment are bytes for the program alone.** No system library reads the environment: the loader, `origin.core` and the SDK libraries take nothing from it. A child's environment is empty unless its launcher sets it, so nothing is inherited. First-party services take their configuration through typed interfaces, never through arguments (ADR-0011 Amendment 1, M5). C4's layout does not change. | P6; C4 |

**Why E1.** A handle today is 16 bits of slot and 16 of generation (`framekernel/src/handles.rs`). A slot is retired when its generations run out, which is how P1's promise is kept. So a process holds at most 65,535 handles at once, and creates at most 65,535 × 65,535, about 4.29 billion, in its life. After that every call that makes or receives a handle returns `limit`, for good. At 1,000 handles a second that is 50 days. Every service call moves handles (`origin.directory`'s `connect` passes one for each connection), and Origin Server expects months without a restart.

**Why E2.** Environment strings that libraries read are how other systems grew attacks through `LD_PRELOAD` and `PATH`, and arguments are how one program comes to drive another with text. P6 and C4 say what the strings look like, and nothing says who may read them.

**Alternatives to E1.**

| Alternative | For | Against |
|---|---|---|
| Keep 32 bits and retire slots, as today | No change | The lifetime above, and at most 65,535 live handles in one process |
| Keep 32 bits and let generations wrap | No ABI change | A stale handle can then name a later object: P1's promise becomes "very unlikely" |
| Keep 32 bits, with more slot bits and fewer generation bits | More live handles | Each slot wears out sooner. The product stays 2^32 |
| 64 bits | No practical lifetime, and room for far more live handles | 4 more bytes in `received_handle` and 8 in `start_entry`; every generated binding changes once |

**Trade-offs.** Each handle record grows, and one change touches the kernel's table, `origin-abi`, the OIDL definition and every generated crate together. In return the limit goes for good, at the only time when changing it breaks nothing outside the tree.

**Migration consequences.** `user-abi` is at major 0, so no program outside the tree depends on a handle's width (P12). After the first release with third-party programs the width could change only with a new major. E2 changes no layout and no built behaviour: no system library exists yet to read an environment.

## Amendment 7 (Accepted, 2026-11-11): what events, ports and `clock_read` need and C9, C10 and C13 leave open

**Status: Accepted by the owner on 2026-11-11, all 19 items with the recommended answer ("Accept all", decision sheet items 169–187; CLAUDE.md §84). None is built: no behaviour below has run.** The options, and what was rejected for each, are in task S3-6's research ledger, `docs/research/2026-11-11-events-ports.md`, section "Shapes for the owner" (its shape n of 1–18 is F n; its shape 20 is F19; its shape 19, the call numbers, is recorded below and is no decision). C6, C9, C10, C11 and C13 stand. F1 makes one exception to P4, named in its row; where another item below is more exact than C9, C10 or C13, the item governs.

| # | Item | Decision | Refines |
|---|---|---|---|
| F1 | 169 | **The packet.** `port_packet` is a fixed 32-byte record of C9's six fields in C9's order, with no padding and no `size` field: `key: u64` at 0, `kind: u32` at 8 (`signal = 1`, `canceled = 2`; 0 is never a kind), `trigger: u32` at 12, `observed: u32` at 16, `reserved: u32` at 20 (zero), `data: u64` at 24 (zero in v1). **This is an exception to P4's "Every structure crossing the boundary begins with a `u32 size`"**, for this record only; OIDL §3.1 already allows a plain record as a call's result. A different layout would be a new call. | C9; P4 |
| F2 | 170 | **Binding.** `object_wait_async(port, object, key, signals)` needs `write` on the port and `wait` on the object and takes no options argument. `signals` is non-zero and names declared bits only. A signal already raised queues the packet before the call returns. `key` is compared, never interpreted. The first failure wins, in D5's order: the mask (`invalid_args`); the port's handle, then the object's (`bad_handle`, `wrong_type`, `access_denied`); an object that raises no signals (`bad_handle`, as `wait_one`); the quota (`limit`); the storage (`no_memory`, with the charge returned). If another thread closes the bound handle or the port during the call, the answer is `bad_handle` and nothing stays bound. | C9; D1, D5 |
| F3 | 171 | **Reading.** `port_wait(port, deadline) -> packet` needs `read` on the port. The deadline is `wait_one`'s: absolute, monotonic, nanoseconds; 0 polls; `deadline_infinite` never expires. `timed_out` when nothing is queued by then. `canceled` when the handle waited through is closed or moved during the wait, even if a duplicate keeps the port alive; such a wait consumes no packet. The out pointer is checked before the wait and written only on `ok`. A packet is consumed when it is taken: if the copy then faults, the answer is `fault` and the packet is gone, as `channel_read` treats a message it has taken. The status `canceled` is not the packet kind `canceled`. | C9; A2 |
| F4 | 172 | **Explicit cancel.** `port_cancel(port, object, key)` needs `write` on the port and no right on the object. It matches the port, the object behind the handle given, and the key, and silently removes every armed binding and every queued packet of either kind that matches. `not_found` when nothing matched; that status's description gains the port's case and its meaning does not change. The authority is the port's `write`: processes that share it share control of the port's bindings. | C9; C11 |
| F5 | 173 | **`object_signal(object, clear, set)`** keeps the signature it has as a reserved call. Events only in v1 (`wrong_type` otherwise) and only `signaled` (any other declared bit in either mask is `invalid_args`). Two empty masks are `ok` and change nothing. Clear then set is one atomic update, `(old & !clear) | set`; a bit in both masks ends set; there is no pulse. Order: undeclared bits (`invalid_args`); the handle (`bad_handle`, then `access_denied`); the type (`wrong_type`); then the bits. | C10 |
| F6 | 174 | **The default rights' names.** `event_rights = wait | signal | duplicate | transfer` and `port_rights = read | write | duplicate | transfer`: C6 word for word. | C6 |
| F7 | 175 | **Quotas.** Three new per-process counts set by the creator: `events`, `ports`, `bindings`. C9's "charged to the binder" is the `bindings` quota of the process that called `object_wait_async`, held from the bind until that binding's one packet has left the port. Events and ports are charged to their creator until the object is destroyed, as channels are. Past a quota: `limit`. Without memory: `no_memory`. A process given no quota gets nothing (D4). No kernel-wide bound yet; that is step 4's. | C9; P11, D4 |
| F8 | 176 | **`wait_one`** works on an event. On a port it is `access_denied` (C6 gives a port no `wait`, and P2 adds no right). A port raises no signals in v1, so a port cannot be bound to a port. | C6; P2, P9 |
| F9 | 177 | **Binding the same thing twice** is allowed: the same port, object and key give two bindings and two packets, each with its own storage and charge. `port_cancel` removes every match. | C9 |
| F10 | 178 | **`clock_read() -> nanos`** reads the monotonic clock only and takes no clock argument. Its one status is `fault`: the result goes through an out pointer (P4). A wall clock is a different call. What the clock does across suspend is not decided here. | C13; P4 |
| F11 | 179 | **A port that outlives its binder.** *The port is transferred:* the binder stays charged for its bindings, and the port's own charge stays with its creator. *The port's last handle is closed:* every binding and queued packet goes, silently, and every charge is refunded. *The binder exits while another process holds the port:* exit closes every handle (P11), so each armed binding becomes a `canceled` packet; those packets, and packets already queued, stay queued and stay charged to the exited binder's account until they are read or cancelled, or the port closes. A queued message already stays charged to a sender that has exited. | C9; P11 |
| F12 | 180 | **A binding's storage is refunded** when its packet is read, when it is explicitly cancelled, and when the port closes. An implicit cancel refunds nothing: the storage becomes the `canceled` packet. | C9 |
| F13 | 181 | **The mask is not checked against the object's type.** `signals` may name any declared bits; a bit the object's type never raises never fires, as with `wait_one`. | C9; A2 |
| F14 | 182 | **What a packet reports.** `trigger` is the mask the binding asked. `observed` is all of the object's signals at the moment the packet was queued; for a `canceled` packet, at the cancel. A queued packet stays if the signal later falls. | C9 |
| F15 | 183 | **Order, and no bound per port.** Packets leave a port in the order they were queued. A port has no bound of its own: every packet is some binding's reserved storage, so the binders' quotas bound the queue. Which waiting thread receives a packet is unspecified. | C9 |
| F16 | 184 | **An implicit cancel follows the handle, not the object.** Closing or moving the handle a binding was made through cancels it, even if a duplicate still reaches the object. Closing another duplicate does nothing; `handle_duplicate` copies no binding; a binding does not travel with a moved handle. A transfer cancels only if the `channel_write` succeeds. The port side goes by the object: closing the port handle used at bind, while another keeps the port alive, leaves the binding. A `signal` packet already queued when its handle closes stays, and no `canceled` packet follows it: one binding, one packet. | C9 |
| F17 | 185 | **The two creation calls.** `event_create()` and `port_create()` take no argument (D1) and answer `fault`, `limit` or `no_memory`. An event starts with no signal raised. The out pointer is checked before anything is made. | C10, C9; D1 |
| F18 | 186 | **What can be bound:** any object `wait_one` can wait on, which is channel ends, processes, threads and events. | C9; P9 |
| F19 | 187 | **What S3-6 leaves out.** Packets a program queues itself, and edge-triggered or repeating bindings, are not in this task; each would be a new call later. The `Timer` object of P3 is not built by S3-6 either. **It stays owed under P3, unchanged**, and is tracked as TASKS item 36. Until it exists, a deadline on `port_wait` or `wait_one`, with `clock_read`, covers a timeout. | P3, P9 |

**The interface these add up to**, written against the grammar of `docs/spec/OIDL.md` with the doc comments left out. The tool had not parsed it when it was accepted. A method's error set is frozen for a major version, so each list of errors is part of what was accepted.

```
@since(0.6) const event_rights: rights = wait | signal | duplicate | transfer;
@since(0.6) const port_rights: rights = read | write | duplicate | transfer;

@since(0.6) enum packet_kind: u32 { signal = 1; canceled = 2; }

@since(0.6) struct port_packet {      // 32 bytes, no padding
    key: u64;                         // @0   what the binder gave
    kind: packet_kind;                // @8
    trigger: signals;                 // @12  the mask the binding asked
    observed: signals;                // @16  the object's signals when the packet was queued
    reserved: u32;                    // @20  zero
    data: u64;                        // @24  zero in v1
}

method event_create = 9 () -> (event: handle<event>,)
    errors(fault, limit, no_memory) requires none;
method object_signal = 10 (object: handle<any, signal>, clear: signals, set: signals,)
    errors(bad_handle, invalid_args, access_denied, wrong_type) requires none;
@since(0.6) method port_create = 15 () -> (port: handle<port>,)
    errors(fault, limit, no_memory) requires none;
@since(0.6) method object_wait_async = 16 (port: handle<port, write>, object: handle<any, wait>,
                                           key: u64, @nonzero signals: signals,)
    errors(bad_handle, wrong_type, invalid_args, access_denied, limit, no_memory) requires none;
@since(0.6) method port_wait = 17 (port: handle<port, read>, deadline: u64,) -> (packet: port_packet,)
    errors(bad_handle, wrong_type, access_denied, fault, timed_out, canceled) requires none;
@since(0.6) method port_cancel = 18 (port: handle<port, write>, object: handle<any>, key: u64,)
    errors(bad_handle, wrong_type, access_denied, not_found) requires none;
@since(0.6) method clock_read = 19 () -> (nanos: u64,)
    errors(fault) requires none;
```

**Recorded, and no decision.**

- **Call numbers:** 9 `event_create` and 10 `object_signal`, as reserved today; 15 `port_create`, 16 `object_wait_async`, 17 `port_wait`, 18 `port_cancel`, 19 `clock_read`. They are private to the release (P4).
- **Two error sets change:** `event_create` and `object_signal` are reserved calls today and answer only `not_supported`. They lose that status and take the sets above. Major 0 reports such a change without refusing it (OIDL §9).
- **Versions:** `origin.kernel` 0.5 becomes 0.6, and `user-abi` 0.5 becomes 0.6.

**Why F1 has no size.** The caller would have to set it before every `port_wait`, and the kernel would read and check it on the path that should be the shortest. Two plain records cross the boundary already, `received_handle` and `start_entry`, each inside a vector or a larger record. `port_packet` is the first plain record a call writes by itself, and the generator's path for that has never run; building it will show whether it works as the OIDL specification says. Room to grow is `reserved`, `data` and new kinds.

**Why F11 keeps an exited binder charged.** Removing its bindings and packets silently would leave a reader on a shared port waiting for a packet that never comes, and C9 says an implicit cancel delivers. Moving the charge to whoever holds the port would be a charge nobody agreed to. So a binder's exit leaves no armed binding, and leaves no charge once the port is read, cancelled or closed. It leaves no charge at once only when the port dies with the binder.

**Trade-offs.** No options word, no clock argument and no size in the packet mean that edge-triggered bindings, another clock or a larger packet each need a new call. A port has no bound of its own, so a port's queue is as long as its binders' quotas allow. Sharing a port's `write` shares control of its bindings (F4), which the system-call threat model must carry as a row. P3's `Timer` object stays owed (F19).

**Migration consequences.** `user-abi` is at major 0, so no program outside the tree depends on these shapes (P12). From major 1 the packet's layout, each method's parameters and each error set are frozen. The 64-bit handles of E1 do not change the packet, which holds no handle.

**What stays unknown.** Whether the packet's 12 spare bytes are enough for an I/O completion: asynchronous I/O is not designed. A deadline's resolution is the tick, 10 ms today. No order is promised among threads waiting on one port. Kernel-wide bounds and default quotas are step 4's. When built, this runs under QEMU on the development Mac only.

## Amendment 8 (Accepted, 2026-11-12): what ADR-0011 N14 step 4 needs at the system-call boundary

**Status: Accepted by the owner on 2026-11-12, every item with the recommended answer ("Accept all 30", decision sheet items 197–220; CLAUDE.md §84). Nothing in it is built.** The options, the evidence and what was rejected for each are in step 4's research ledger, `docs/research/2026-11-11-n14-step4-scope.md`, section 5; its candidate n is decision item 196 + n. The other twelve answers are ADR-0011 Amendment 2 (L1–L12): `origin.core`, the per-call pin's method, and Diagnostics. Where a row below is more exact than an accepted row, the row below governs. G1 replaces A6's answer for every process pinned per call, and G3 and G4 are drafts, decided again when their steps begin.

| # | Item | Decision | Refines |
|---|---|---|---|
| G1 | 204 | **A call from anywhere but its own stub stops the thread, as a fault does.** No status comes back. The thread stops as a fault stops it (P10): the process's exception channel receives the event once such channels exist, and until then the thread ends as a faulting thread ends today, with a record of the number and the address. A number that has no stub is the same violation. This holds for every process pinned per call (ADR-0011 L8); the kernel's proof processes keep A6's range and its `access_denied`. | B2 ("refused"); A6 |
| G2 | 206 | **O2 closes for step 4 by absence.** No call creates a thread or a process. The kernel is the only creator, and starts one thread for each step-4 program. The system-call threat model's row O2 reads: closed for step 4 by absence; it reopens when a call creates a thread or a process, and that call comes with its quota. | P11; `THREAT_MODEL_SYSCALLS.md`, "What must be closed" |
| G3 | 207 | **Where thread creation starts (a draft, decided again with step 5).** One call that creates and starts a thread, `thread_create(process: handle<process, manage>, request: thread_request) -> (thread: handle<thread>)`, with a sized `thread_request { entry, stack, argument, thread_pointer }`; errors `bad_handle, wrong_type, access_denied, invalid_args, fault, limit, no_memory` in D5's order; a `threads` count charged at creation and refunded when the thread is reaped, a launcher's first thread included; the kernel chooses the processor, and keeps on each a reserve of run-queue slots that no program's thread takes. The ledger's candidate 11 has the full text. | P6, P11, C6 |
| G4 | 208 | **Where process creation starts (a draft, decided again with the Loader Service).** A creation call and a start call, each behind a named capability that only the Loader Service and the session launcher hold (OIDL §6; ADR-0011 M1). Creation takes the child's limits in a sized request, and refuses with `limit` what the creator cannot pay for (G5). Start has the kernel write the start record from the handles and roles the creator passes (C4), map `origin.core` and record its base (ADR-0011 L7), and start the first thread. A change rate above 10^9 a second is refused with `invalid_args`, not taken as 10^9 (row M8). | P6, P11; ADR-0011 M1–M3 |
| G5 | 209 | **The kernel-wide bound is paid by creators.** A set of limits is worth a figure in bytes: the most kernel memory and frames they let a process make the kernel hold. That is each count times the largest kernel object of its kind, plus the byte quotas, the page tables up to the framework's bound, and a kernel stack and an FPU area for each thread. A creator pays the figure out of its own when it makes a process, and gets it back when the process is destroyed. The kernel holds the root figure, which is what its frames allow at boot less a reserve for its own use; both are printed at boot. A creation the creator cannot pay for is `limit`. No charge made by a running process changes. In step 4 the kernel is the only creator: the root, and a payment at each launch. A port's index (row K7), which no quota counts, is measured per binding before the figure is used. | P11; D4; F7 |
| G6 | 210 | **No implicit default quotas.** A launcher gives each kind deliberately, and a kind not given is zero, as D4 and F7 say. In step 4 each program's limits are written beside its image, and are the least it needs, found from its own high-water marks in its boot result. What the Loader Service gives a program that asks for nothing is decided with its manifest keys (§84, milestone M6). | D4; F7 |
| G7 | 211 | **A launcher fills a directory through a typed `setup` interface on the directory's BOOTSTRAP channel.** `origin.directory` 0.2 gains `setup`, with `add(interface_id: u64, major: u16, provider: handle<channel, read \| write \| wait>)`. Its errors: `limit` for a full table, and a new status appended to `origin.kernel`'s (P5) for a key the table already holds (D2's rule, now on the wire). Only the launcher holds the BOOTSTRAP channel. On a provider's channel the directory is the client: it sends the hello first (OIDL §7.7) and reads only that reply, without waiting for it before it serves its own clients. Entries are added and never removed in v1; a provider that has gone answers `limit` at the next connect, as D7 already does. In step 4 each program that has a directory has its own. | C12; P5; ADR-0011 N9 |
| G8 | 212 | **A provider receives connections as a one-way `accept` on a `provider` interface.** `origin.directory` 0.2 gains `provider`, with `oneway method accept(interface_id: u64, major: u16, server: handle<channel, read \| write \| wait>)`, which a provider serves on the channel its launcher gave the directory. It is one-way because the directory never waits on a provider (D7): a full queue makes the directory's write answer `should_wait`, and the client gets `limit` and a closed end. The key travels with the end, so one provider channel can serve several keys, and the client's hello on the end names the interface. | C12; row D7 |
| G9 | 214 | **Ring 3's clock stays at full resolution.** RDTSC and RDTSCP already run in ring 3 and are finer than `clock_read`, so coarsening the call alone would deny a program nothing; acceptance AC5 (item 137) accepts timing channels between programs in general. Row K15 closes as accepted under AC5. Whether to set CR4.TSD is an open point of ADR-0017's. | F10; row K15 |
| G10 | 215 | **The `Timer` object is not in step 4.** Each step-4 program waits on one port with a deadline (F3), which covers every timeout it has. P3's `Timer` stays owed (F19, TASKS item 36). | P3; F19 |
| G11 | 217 | **The Diagnostics service gets the boot report through one kernel call that only it may make:** a call in a new minor of `origin.kernel` that copies the report into the caller's buffer, gated by a named kernel capability held as process state, as `kernel.debug` is. The launch path gives that capability to the Diagnostics service only. The report's records are ADR-0011 L10's, and their exact shapes are put to the owner with task S4-5. | P4; OIDL §6 |
| G12 | 220 | **No call drops a message.** A service reads with a buffer of `max_message_bytes` and keeps room in its table for `max_message_handles` more. A read that still answers `limit` (A4: its table cannot take the message's handles) makes it close that connection, and the message's handles are closed with the end. A client that sends what the service cannot take loses only its own connection. A discard call waits until a service shows it needs one. | A4; S3-2's finding "reading without room" |

**Recorded, and no decision.**

- **Versions.** G7's status and G11's call each take one more minor of `origin.kernel` and `user-abi` (OIDL §9), after S4-0's 0.7 (E1). `origin.directory` 0.2 comes with S4-4.
- **Rows of the system-call threat model** change with these answers: E8 (G1, ADR-0011 L6–L8), O2 (G2), D11 (G7, G8), K5 (G5, G6) and K15 (G9), and its list "What must be closed".

**Trade-offs.** G1 gives a hijacked thread no second try, and a program with a wrong stub no error to read: it stops. G2 leaves step 4 with one thread per program. G5 makes every creator hold what its children could cost, so a launcher must be given more than it uses itself; the figure is only honest once each kind's largest object is measured. G7 appends a status to the one enumeration every interface draws from (P5), at the next minor; only `setup.add` declares it. G12 costs a client its connection, not the service its queue.

**Migration consequences.** `user-abi` is at major 0 (P12), so no program outside the tree depends on these shapes. G3 and G4 change nothing until their steps; their final shapes may differ, and are put to the owner then.

**What stays unknown.** Each kind's largest kernel object, which G5's figure needs. Whether one directory process serves many programs' tables (step 5, with the session launcher). When built, this runs under QEMU and as KVM guests in CI only.

## Amendment 9 (Accepted, 2026-11-13): the flags at a thread's entry

**Status: Accepted by the owner on 2026-11-13 with the recommended answer ("Accept all 5", decision sheet item 272; CLAUDE.md §84). The kernel already starts a thread this way; nothing proves it at boot yet.** The evidence is `docs/research/2026-11-12-psabi-revision.md`, rows R1 and R2. The independent review of task S4-1 raised it (finding F7): the runtime's `_start` relied on the direction flag, and P6 did not state the flags.

| # | Item | Decision | Refines |
|---|---|---|---|
| H1 | 272 | **At a thread's entry the direction flag and the six status flags are clear, and interrupts are on.** RFLAGS.DF, CF, PF, AF, ZF, SF and OF are 0, as the table for process initialization has them in the System V x86-64 psABI that CLAUDE.md §17 pins, and IF is 1. A program may rely on no other bit of RFLAGS. This holds for a process's first thread and for every thread started later. | P6 |

**Options that were put.** (a) As decided. (b) State the whole register, `0x202`: it would promise bits that are the kernel's to choose, such as AC and ID, and any bit a later processor adds. (c) State nothing, and leave each runtime to clear the direction flag itself: a program not built on Origin's runtime would then have nothing to rely on.

**Recorded, and no decision.**
- The kernel starts every thread with RFLAGS `0x202` today (`NEW_RFLAGS` in the framework's `stack.rs`), which meets H1.
- `origin-rt`'s `_start` clears the direction flag itself since S4-1's review. Whether it keeps doing so is its builder's choice: H1 makes it unnecessary and it costs one byte.
- A boot proof that reads the flags at entry, before any instruction of the program changes them, with a control that starts a thread with the direction flag set, is owed with the next task that touches the launch path (S4-2). Until then the matrix row is "not verified".

**Performance cost (ADR-0018 V3).** None, by construction: it states what the kernel already does.

**Trade-offs.** A program gets less than the whole register. That is deliberate: what it gets is what the psABI's calling sequence needs.

**Migration consequences.** None: `user-abi` is at major 0 (P12), and the kernel's behaviour does not change.

**What stays unknown.** Nothing has read the flags from ring 3 at entry. The psABI's other chapters were not read against this revision (the ledger's section 4).
