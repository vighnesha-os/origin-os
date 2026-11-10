# Threat model: the system-call boundary

| | |
|---|---|
| Required by | CLAUDE.md §58 ("Threat-model every privileged interface"), §29, §30, §42 |
| Covers | The way into the kernel from ring 3 and back (`framekernel/frame/src/cpu.rs`, `trap.rs`, `usercopy.rs`, `userspace.rs`); the calls (`framekernel/src/syscall.rs`, `handles.rs`, `ipc.rs`, `channel.rs`, `process.rs`, `wait.rs`); the decoder generated from `sdk/interfaces/origin.kernel.oidl` (`framekernel/syscalls`); ADR-0005 P1–P12 and Amendments 1–4 |
| Written | 2026-11-10, against S3-1's code (c3ccacf). §58 and §83 expect a threat model during design; P4–P11 were designed and built without one, so this is written after them. S3-2's changes (a message's writer, handle records) are added in their own section, written with them. |
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

**Out of scope:** a kernel already compromised another way, physical attacks, malicious firmware or microcode, and DMA (`THREAT_MODEL_IOMMU.md`). Speculative execution is listed as open (I2) but not modelled in detail.

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

## What must be closed

Before any program that is not a boot proof runs in ring 3 (N14 step 4):
1. **E8**, the per-call pin (Amendment 2, B2).
2. **O2**, quotas on threads, processes and memory (P11), with the calls that create them.
3. **I2**: a speculative-execution review against the declared processors.

Before release: **E11** (CET) and **V2**.

## References

- Intel 64 and IA-32 Architectures Software Developer's Manual, Vol. 2B (`SYSCALL`, `SYSENTER`, `IRET`) and Vol. 3A (privilege levels and gates, SMEP, SMAP, UMIP).
- CERT/CC, Vulnerability Note VU#649219, the SYSRET return to a non-canonical address (2012): <https://www.kb.cert.org/vuls/id/649219>
- Kocher et al., *Spectre Attacks: Exploiting Speculative Execution*, IEEE S&P 2019; Lipp et al., *Meltdown: Reading Kernel Memory from User Space*, USENIX Security 2018.
- `docs/research/2026-11-10-caller-identity.md`, for the writer's identity S3-2 adds.
