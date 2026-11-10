# Threat model: the system-call boundary

| | |
|---|---|
| Required by | CLAUDE.md §58 ("Threat-model every privileged interface"), §29, §30, §42 |
| Covers | The way into the kernel from ring 3 and back (`framekernel/frame/src/cpu.rs`, `trap.rs`, `usercopy.rs`, `userspace.rs`); the calls (`framekernel/src/syscall.rs`, `handles.rs`, `ipc.rs`, `channel.rs`, `process.rs`, `wait.rs`); the decoder generated from `sdk/interfaces/origin.kernel.oidl` (`framekernel/syscalls`); ADR-0005 P1–P12 and Amendments 1–4 |
| Written | 2026-11-10, against S3-1's code (c3ccacf). §58 and §83 expect a threat model during design; P4–P11 were designed and built without one, so this is written after them. S3-2's changes (a message's writer, handle records) are added in their own section, written with them, and S3-3's (process start) and S3-4's (the service directory) before their code. |
| Evidence | QEMU only. Nothing has run on hardware (§74). The boot proofs named below are ADR-0013 rows: "ring-3 isolation", "per-processor control state", "IPC and handles" and "process and thread objects" all stop the boot when they fail. The IPC proof makes 117 hostile calls and checks each status. |

## What is being protected

- The kernel's memory and control flow. No value from ring 3 may become a kernel pointer, an index past a table, or a jump target.
- Other processes: their memory, their handles and objects, and the messages queued for them.
- Rights. A handle never gains a right (P2), and a call does only what its handle's type and rights allow.
- The kernel's availability: its memory, the locks it holds with interrupts off, and the processors.
- Since S3-2, the truth of what a read reports: who wrote each message, and each received handle's type and rights.

## Who the attackers are

| Actor | Can | Today |
|---|---|---|
| A hostile ring-3 program | Run any instructions in ring 3, pass any register values, put anything in its own memory, and change that memory from another thread during a call | The main threat. Only the boot proofs' programs run in ring 3, created by kernel code |
| A hostile peer | Write messages and move handles to another process over a channel it holds, close its end at any time, never read | In scope |
| A buggy or compromised `origin.core` | Issue any call with any arguments from the pinned code range | `origin.core` does not exist yet (step 4). Until it does, the creator names each process's range (A6) |
| Code outside `origin.core` | Run `syscall` from its own pages | Refused by the range pin (A6). The per-call pin (B2) is not built |

**Out of scope:** a kernel already compromised another way, physical attacks, malicious firmware or microcode, and DMA (`THREAT_MODEL_IOMMU.md`). Speculative execution is listed as open (I2) but not modelled in detail. What each exclusion means for the product, where it is covered and what is accepted by name, is in `THREAT_MODEL_SYSTEM.md` (EX1 to EX12). Speculation is decided in `docs/adr/ADR-0017-speculation-processor-hardening.md`, which is Proposed.

## Threats

Status: **Proven** (a boot proof whose negative control is recorded in `SOURCE_PROVENANCE.md`), **Boot** (a boot proof, no control recorded), **Host** (host tests only), **By design** (no proof), **Partly**, **Open**.

### Entering and leaving the kernel

| # | Threat | Defence | Status |
|---|---|---|---|
| E1 | Another way in: `int n`, `sysenter`, or a 32-bit compatibility-mode `syscall` | `syscall` is the only entry besides faults and interrupts. Every IDT gate is DPL 0, so `int 0x80` from ring 3 is a #GP. `IA32_SYSENTER_CS` is zero, so `sysenter` is a #GP. The GDT has no 32-bit code segment, so there is no compatibility mode | **Boot** (the `int 0x80` program; "per-processor control state") |
| E2 | Ring 3 enters with TF, IF, DF, NT or AC set, so the kernel single-steps, takes an interrupt on the user stack, copies backwards or runs with SMAP off | `IA32_FMASK` clears all five on `syscall`. An interrupt from ring 3 with AC set is counted, and the `ac` program checks the count stays zero | **Boot** (`ac`, `step`) |
| E3 | An NMI or machine check arrives after `syscall`, before the kernel has left the user stack | NMI and #MC use their own IST stacks, with guard pages | **Proven** (M4's control) |
| E4 | A program confuses the kernel's GS base | `swapgs` on entry and exit; `CR4.FSGSBASE` is off, so ring 3 cannot set a GS base; the `gs` program loads a null GS and checks it survives | **Boot** |
| E5 | A return to a non-canonical RIP faults in ring 0 with ring 3's GS or stack (the SYSRET problem) | The kernel returns with IRETQ, and the top page of the lower half is never mapped, so a `syscall` cannot sit where RCX would be non-canonical | **By design**, host-tested |
| E6 | A return leaks kernel values in registers | Every register but RAX, RCX and R11 is restored from the frame. RAX holds the status, and RCX and R11 hold ring 3's own RIP and RFLAGS. The kernel is built without SIMD, and SIMD state is saved across switches | **Boot** (`regs`; "SIMD state across switches") |
| E7 | Code outside the process's system-call range makes a call | Refused (A6); the process's range is set when it is created | **Boot** |
| E8 | Code inside the range jumps to the `syscall` instruction of another call | None: the range is pinned, not each call's own address (B2) | **Open** |
| E9 | A program uses the developer calls to hold the serial line with interrupts off | `debug_report` and `debug_write` need the `kernel.debug` capability, given only to proof processes; others get `not_supported`. A line is at most 128 bytes | **Boot** (`nodebug`) |
| E10 | A program reads the descriptor tables' addresses to find the KASLR slide | UMIP is on, so `sidt` and `sgdt` fault in ring 3 | **Boot** (`sidt`) |
| E11 | The CET shadow stack is not enabled, although P6 asks for it at entry | None | **Open**, a recorded deviation |
| E12 | **A nested NMI.** After an NMI is delivered, further NMIs are blocked until the next IRET executes, whoever executes it. If the NMI handler takes an exception of its own, that exception's return unblocks them while the handler is still on its stack. The next NMI enters through IST2, which loads the same top of stack every time, and overwrites the frame and the saved registers the first handler has yet to restore. The interrupted context is lost | None in the entry path: `exception_common` and `exception_saved` treat vector 2 like any other, with no mark that the handler is running and no second place for its frame. What keeps the window shut today is how little the handler does. For vector 2, `dispatch` records a count and the frame's address and returns: it reads no CPU-local data and no user memory, takes no lock and prints nothing. No breakpoint is armed (DR7 is cleared on every processor), and its code and its stack are always mapped. Two ways in remain. The kernel's own: a machine check during those instructions that `machine_check` judges recoverable returns with IRETQ. And one below the kernel: an SMI on an AMD processor, where the manual read does not say that NMIs blocked before the SMI stay blocked after it (Intel's says they do) | **Open.** Ring 3 cannot reach it: no call raises an NMI or a machine check, and the kernel's only NMI source is its own boot proof. The "NMI stack" proof behind E3 takes one NMI on IST2 and never two. FRED closes it where the processor has FRED; the path kept for processors without it needs a guard (ADR-0017 Z6 and Z12, Proposed) |

### Arguments and user memory

| # | Threat | Defence | Status |
|---|---|---|---|
| A1 | A pointer names kernel memory, an unmapped page, or a page the kernel would write but ring 3 may only read | Checked against the calling process's own page tables (present, user, writable where written), then copied in a STAC/CLAC window with fault recovery. SMEP and SMAP are on and proven at boot | **Boot** ("bad pointers") |
| A2 | An address plus a length wraps, or a count times an element size overflows | `UserRange` refuses a wrapping or out-of-half range. The decoder checks each count against its bound before the multiplication, and no bound exceeds 64 KiB | **Host**, **Boot** ("sizes and numbers") |
| A3 | Another thread changes an argument between the kernel's check and its use | Every input is copied into a kernel buffer once, before it is checked, and nothing is read twice; `read_sizes.size` is read once | **By design** |
| A4 | A malformed scalar: out of range, an undeclared enum value or bit, a boolean that is not 0 or 1 | The generated decoder refuses it with `invalid_args` before the implementation runs (OIDL §7) | **Host**, **Boot** ("sizes and numbers") |
| A5 | An unknown or reserved call number | `not_supported`, without running any implementation | **Host** |
| A6 | An implementation returns a status its call does not declare | Counted, the first recorded, and the IPC proof fails | **Proven** (S3-1 control 9) |
| A7 | A bad out pointer loses a message or the handles that came with it | Out pointers are checked before a message is taken. If a copy-out still fails, the handles installed for it are closed | **Boot** ("bad pointers") |

### Handles and rights

| # | Threat | Defence | Status |
|---|---|---|---|
| H1 | A forged handle: a value never issued, or one that is live in another process | Each process has its own table; anything else is `bad_handle` | **Boot** ("bad handles") |
| H2 | A stale handle names a slot that has been reused | A 16-bit generation per slot. A slot whose generations are used up is retired, never reused | **Host** |
| H3 | A call runs with too few rights | The definition states the rights each handle needs, and the table enforces them on every lookup | **Proven** (S3-1 controls 1 and 2) |
| H4 | A handle of another type is used | The type is checked before the rights | **Proven** (S3-1 control 3) |
| H5 | A right is gained by duplicating or moving a handle | `handle_duplicate` only removes rights; a moved handle keeps the rights it had | **Boot** ("rights only shrink") |
| H6 | A handle is closed or moved away while a thread waits on it | The waiter is woken and completes with `canceled` | **Proven** (S3-1 controls 10 and 11) |
| H7 | A full handle table loses a message | Room for a message's handles is reserved before it is taken; a full table leaves it queued | **Boot** ("quotas") |

### Objects, quotas and lifetimes

| # | Threat | Defence | Status |
|---|---|---|---|
| O1 | A writer exhausts kernel memory through channels and queued messages | Each process is charged per channel and per queued byte, before anything is consumed. A queue holds 16 messages and 4 full-size messages of bytes | **Boot** ("quotas", "a writer over its byte quota", "its peer, unaffected") |
| O2 | Exhaustion through threads, processes or memory objects | Calls cannot create these yet. P11's memory and thread quotas do not exist | **Open** until S3-3 and S3-5b |
| O3 | A failed allocation panics the kernel | Everything a call creates is allocated fallibly and answered with `no_memory` | **Host** (allocation-failure test); no boot fault injection |
| O4 | Channel ends sent into each other's queues so that nothing can reach them, and memory is never freed (A3) | A transfer is refused for an end of the same channel, an end with other references, and an end whose chain of queues leads back. The walk stops at 16 links with `limit` | **Boot** ("transfers that would break the graph", "transfers into nested ends") |
| O5 | An object is used after it is freed, or leaked | Reference-counted objects; the IPC proof's leak check compares heap and frames exactly | **Proven** (S3-1 control 12) |
| O6 | `terminated` is raised before a process's memory is freed | The address space goes when the last thread is reaped, and `terminated` after it | **Proven** (S3-1 controls 4 and 5) |
| O7 | A process ID is reused, so a recorded ID names a later process | A 64-bit counter, never reused, 0 the kernel | **Proven** (S3-1 control 8) |
| O8 | A thread starts in a process that has already ended | One lock decides the last thread and refuses a late start. A thread is counted before it takes the address space | **By design** |

### Availability

| # | Threat | Defence | Status |
|---|---|---|---|
| V1 | Long stretches with interrupts off | Calls run with interrupts on and take locks briefly. The global transfer lock's walk is bounded at 16 links. Copies of up to 64 KiB run with interrupts on | **Partly**: no measured bound |
| V2 | A ring-3 loop that never enters the kernel delays reaping and `terminated` on its processor | The timer preempts it, but reaping waits until the processor is given up | **Open** (S3-1's known limit) |
| V3 | A full wait queue | A thread that finds 16 waiters polls by yielding instead | **By design** |
| V4 | A deep call chain overflows a kernel stack | Each thread's kernel stack has a guard page, so an overflow faults instead of corrupting memory | **By design** |

### Information

| # | Threat | Defence | Status |
|---|---|---|---|
| I1 | Uninitialized kernel memory is copied out | Results are built in zeroed buffers, and the generated encoders zero padding (OIDL §5.2) | **Host** |
| I2 | Speculative execution: a call number or handle index used out of bounds speculatively, poisoned indirect branches, or a Meltdown-type read of the kernel half, which is mapped in every address space | None: no index masking, no indirect-branch mitigation, no page-table isolation, and `IA32_ARCH_CAPABILITIES` is not read | **Open** |
| I3 | Process IDs are sequential, so a program learns roughly how many processes were created between two of its observations | Accepted by ADR-0005 C1, since an ID grants nothing | **Open** (accepted) |

### A message's writer and handle records (S3-2)

ADR-0005 B1 and B3, as C1–C3 decide. The ledger is `docs/research/2026-11-10-caller-identity.md`. "Proven" here means the boot proofs "a message's writer" and "IPC and handles", with the controls recorded for S3-2 in `SOURCE_PROVENANCE.md`.

| # | Threat | Defence | Status |
|---|---|---|---|
| W1 | A writer forges its identity | `channel_write` stamps the message with the writing process's identity, which never changes, and queues the two together. No argument carries it | **Proven** (controls 4, 5) |
| W2 | The identity is stale: it names whoever held the end when the channel was made, or holds it when the message is read | Each message is stamped when it is written, and keeps its stamp while its end moves | **Proven** (control 6) |
| W3 | A recorded process ID comes to name a later process | IDs are never reused (O7) | **Proven** (S3-1 control 8) |
| W4 | Only part of a 64-bit field reaches the reader | Both fields are 64 bits end to end, and the proof's values have both halves set | **Proven** (control 9) |
| W5 | A reader that gives an older `size` has memory written past it | The kernel writes only what fits in the caller's `size`; the first version's 12 gets the counts and nothing past them, with no error | **Proven** (control 8) |
| W6 | A read too small for its message hides who wrote it, so a reader must take an unwanted message to learn its writer | The identity is written on `buffer_too_small` too | **Proven** (control 7) |
| W7 | Identity is taken for authority | A process ID or principal names nothing and opens nothing (P1, C1). Acting on the writer needs a handle the writer gave | **By design** |
| W8 | A relayed message names the relay, not the first sender | Intended: the stamp is the last writer. A relay that vouches for an earlier sender says so in its own protocol, and the reader trusts that only as far as it trusts the relay | **By design** |
| W9 | A received handle's type or rights are misreported, so a receiver trusts a handle it should not | Each record is made from what the reader's table now holds for the handle | **Proven** (controls 2, 3) |
| W10 | Records are written past the caller's buffer | Room for whole 12-byte records is checked before a message is taken | **Proven** (control 1) |
| W11 | A record or `read_sizes` value outside the definition reaches a client as a value | The generated client decodes each record and the sizes checked; an undeclared type, right or padding byte is a protocol error | **Host** |
| W12 | The writer's principal tells the reader more than it should | Intended: B1 delivers it. Principals are opaque numbers the creator chose; what they may encode is for the accounts design (§57) | **Open**, with I3 |

### Process start (S3-3)

ADR-0005 P6, as C4–C6 decide, and C1's inheritance of the principal. The ledger is `docs/research/2026-11-10-start-record.md`. Written before the code (§58, §83), then given the status the boot proof "process start" and its controls (recorded for S3-3 in `SOURCE_PROVENANCE.md`) earned.

| # | Threat | Defence | Status |
|---|---|---|---|
| P1 | A child acts for a principal its creator chose instead of the creator's own, so per-user policy and audit name the wrong user (C1) | The launch path reads the principal from the creator and takes none of its own; no capability to choose another exists | **Proven** (control 3): the message each child writes on BOOTSTRAP names the creator's principal (B1) |
| P2 | The process changes its start record, and code that reads it later trusts what the process wrote | The record's pages are mapped read-only and not executable | **Proven** (control 1): each child's write to its record faults at the record's address |
| P3 | The record sits where an attacker can predict it | A random page in the upper user half, from RDSEED or RDRAND, clear of every other mapping. Without entropy the launch fails rather than use a predictable address | **Proven** (control 5): two launches place their records at different addresses |
| P4 | The record carries kernel memory or stale bytes | Fresh zeroed frames, an exact size and zero padding, from the `origin-abi` encoder | **Boot**, **Host**: the record each child reads back decodes with `origin-abi`'s decoder, which refuses a wrong size or a non-zero padding byte; host tests of both |
| P5 | A start handle has more rights than C6 gives its role | Each role's rights are fixed in the launch path: C6 for the process and thread, a channel's for BOOTSTRAP and DIRECTORY | **Proven** (control 4): each entry's rights read back in ring 3 |
| P6 | An entry misreports a handle's role or type | Each entry is made from what the child's table holds for that handle | **Proven** (control 6): each entry read back and each handle used |
| P7 | The first thread runs before the record exists, or finds something else in RDI | The record is written, then the first thread starts with RDI holding its address; every other general-purpose register is zero (the first frame is built zeroed) | **Proven** (control 2) for RDI, read back in ring 3; **By design** for the other registers |
| P8 | A failed launch keeps the never-started child alive through its handle to itself, or leaks what its creator passed | A launch that fails after granting closes the child's table | **Proven** (control 7): a launch refused after its child held a handle gives everything back, and the leak check holds |
| P9 | Arguments or environment take unbounded kernel or user memory | At most 64 handles and 64 KiB per record, checked before anything is mapped | **Boot**, **Host**: a launch over 64 KiB is refused before anything is mapped; host tests of both limits to the byte |
| P10 | DIRECTORY is a channel to whatever the creator chose | Intended: a creator decides what its child can reach (P1 of ADR-0005: no ambient authority). The service directory comes with S3-4 | **By design** |

### The service directory (S3-4)

ADR-0011 N9 and S9, as C11 and C12 decide: `origin.directory` 0.1, served by a process over the channel a program's start record names DIRECTORY. The directory is not in the kernel, but it is the only way a program finds services, so it is modelled here with the boundary it extends. The ledger is `docs/research/2026-11-10-service-directory.md`. Written before the code (§58, §83), then given the status the boot proof "the service directory", its controls (recorded for S3-4 in `SOURCE_PROVENANCE.md`) and the host tests of `services/directory` earned.

| # | Threat | Defence | Status |
|---|---|---|---|
| D1 | A program reaches a service its launcher did not give it | Its only way to services is its DIRECTORY handle (N9; C5 retired SERVICE). A directory passes on only what its table holds, and its table comes from the launcher | **Host**: a key the table does not hold is `not_found`, and its end is closed. **Proven** (control 4) that the answer reaches a client in ring 3 as `not_found` |
| D2 | A service claims a name to intercept other programs' clients (squatting) | Entries are keyed by interface ID and major and made by the launcher. `connect` is the only method, and it never changes the table | **Host**: a connect never changes the table, and a second provider for a key is refused |
| D3 | A malformed connect, or a server end that is not a channel or lacks read, write, wait or transfer, reaches directory code | The generated stub checks the hello (`not_supported` for another interface or major), the message and the end's type and rights (`invalid_args`) before the directory runs, and the stub's caller closes the handles of a refused message (OIDL §7.5, §7.8) | **Host**: an end without `transfer`, or an event, never reaches the directory; a hello for another interface or major is `not_supported`; a connect before the hello closes the connection; the hostile corpus runs against `origin.directory`'s stub |
| D4 | A directory poses as its client to the service, or reads what the client sends it | The client writes on the end the directory passed on; the end leaves the directory's table when it is passed, and an end with another reference cannot be passed at all (O4). The service sees each message stamped with its writer's identity (B1) | **Proven** (control 3): the service reads the client's request, written before any reply, stamped with the client's process and principal; a directory that passes on an end of its own, carrying a request it wrote, is caught by the stamp |
| D5 | A directory hands a program a fake service (S9) | The launcher builds the directory. The service's replies are stamped with its identity, so the client can check who serves it | **Proven** (control 2): the client reads the service's reply stamped with the service's process, and a directory that keeps the end and answers for the service is caught. What the client checks it against is **Open** (step 4) |
| D6 | The server end of a refused connect stays open, so the client waits for ever, or it stays in the directory | On `not_found` and `limit` the directory closes the end before it replies, and the client's end reports `PEER_CLOSED` | **Proven** (control 5): the `not_found` connection's end reports `PEER_CLOSED` in ring 3. **Host**: the end is closed for `not_found` and for `limit` |
| D7 | A provider that stops reading makes the directory wait or grow | The directory never waits on a provider: an end it cannot pass on (a full queue, its own quota spent, a transfer the kernel refuses) is `limit`, closed, and nothing is kept past the call | **Host**: a provider that cannot take the end is `limit`, its end closed, and one that is gone is forgotten |
| D8 | A client exhausts the directory | The table is bounded and filled by the launcher; a connect keeps nothing once it is answered; the kernel charges every queued message to its writer (O1) | **Host**: the table's bound is refused, and every end a connect is given is passed on or closed, once. Fairness between one directory's clients is its loop's, **Open** until step 4 |
| D9 | A client gets a provider of another major than it asked for | The key includes the major, and the provider's own stub refuses a hello for another major on the passed end | **Host**: another major is another key. **Proven** (control 4): the connect for major 1 is `not_found` in ring 3 |
| D10 | `not_found` tells a program what its directory holds | Intended: a directory is its program's own, built from what the program declared (N9), so it tells the program nothing it was not given | **By design** |
| D11 | The back side: how a launcher fills a directory and how a provider receives connections | Not decided (step 4, ABI decisions then). The boot proof's arrangement, used by nothing else: the launcher writes the table on the directory's BOOTSTRAP, and the directory passes each connection on as a `connect` call on the provider's channel | **Open** |

## What must be closed

Before any program that is not a boot proof runs in ring 3 (N14 step 4):
1. **E8**, the per-call pin (Amendment 2, B2).
2. **O2**, quotas on threads, processes and memory (P11), with the calls that create them.
3. **I2**: a speculative-execution review against the declared processors. The review is `docs/research/2026-11-10-speculation-processor-hardening.md`, and ADR-0017 (Proposed) decides from it. I2 closes when the owner has accepted that ADR and its Z2, Z3, Z5, Z9 and Z10 are built.
4. **D11**, the directory's back side, decided before the first directory that is a program runs.

Before any claim about real hardware: **E12**, the nested NMI.

Before release: **E11** (CET) and **V2**.

## References

- Intel 64 and IA-32 Architectures Software Developer's Manual, Vol. 2B (`SYSCALL`, `SYSENTER`, `IRET`) and Vol. 3A (privilege levels and gates, SMEP, SMAP, UMIP).
- For E12, read on 2026-11-10: Intel SDM Volume 3, order 325384-093, September 2026, §7.7.2 (NMI blocking), §7.14.5 (the interrupt stack table) and §34.8 (NMIs and SMM); AMD64 APM Volume 2, publication 24593, Rev. 3.46, October 2026, §8.2.3, §8.9.4 and §10.3.3. The ledger is `docs/research/2026-11-10-speculation-processor-hardening.md`, §3.10.
- CERT/CC, Vulnerability Note VU#649219, the SYSRET return to a non-canonical address (2012): <https://www.kb.cert.org/vuls/id/649219>
- Kocher et al., *Spectre Attacks: Exploiting Speculative Execution*, IEEE S&P 2019; Lipp et al., *Meltdown: Reading Kernel Memory from User Space*, USENIX Security 2018.
- `docs/research/2026-11-10-caller-identity.md`, for the writer's identity S3-2 adds.
- `docs/research/2026-11-10-start-record.md` and `docs/research/2026-11-10-service-directory.md`, for process start (S3-3) and the service directory (S3-4).
