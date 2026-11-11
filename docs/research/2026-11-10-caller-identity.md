# Research: caller identity on channels (2026-11-10)

Backing notes for task S3-2 (CLAUDE.md §16): every channel message stamped with its writer's identity (ADR-0005 Amendment 2, B1), and `channel_read` reporting that identity and each received handle as a record (Amendment 3, B3), under the decisions the owner accepted on 2026-11-09 (Amendment 4, C1–C3). What was read, what it means for Origin, the tests defined before the code, and what stays unknown. These are conceptual references only: **no source code of any of these systems was read** (CLAUDE.md §3). The scoping of the whole step is `docs/research/2026-11-09-n14-step3-scope.md`.

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, `zx_channel_read_etc` reference, `fuchsia.dev/reference/syscalls/channel_read_etc` | No date on the page | Handle records on read; what a too-small read reports; whether a message names its sender | Read through WebFetch, 2026-11-10 |
| Fuchsia, "Zircon kernel concepts", `fuchsia.dev/fuchsia-src/concepts/kernel/concepts` | No date on the page | How long an object ID stays unique | Read through WebFetch, 2026-11-10 |
| Fuchsia, `zx_object_get_info` reference | No date on the page | How a channel peer can learn who holds the other end | Read through WebFetch, 2026-11-10 |
| seL4, "IPC" tutorial, `docs.sel4.systems/Tutorials/ipc.html` | No date on the page | Identity carried by the capability (badges) | Read through WebFetch, 2026-11-10 |
| Android, `android.os.Binder` reference | Current page, 2026-11-10 | A kernel-supplied caller identity per transaction, and its limits | Read through WebFetch, 2026-11-10 |
| Linux man-pages 6.19, `unix(7)` | 2026-02-08 | Credentials captured at connection time, and credentials the sender states | Read through WebFetch, 2026-11-10 |
| Linux man-pages 6.19, `pidfd_open(2)` | 2026-02-08 | A reference that keeps naming one process | Read through WebFetch, 2026-11-10 |
| QNX SDP 8.0C Library Reference, `_msg_info` and `MsgInfo()` | Updated 2026-06-11 | A kernel report of the sending process and thread for a received message | Read through WebFetch, 2026-11-10 |
| Apple Developer Forums thread 842442, replies by Apple Developer Technical Support | August 2026 | The audit token from the Mach message trailer as the sender's identity | Read through WebFetch, 2026-11-10 |
| Apple Developer Forums, statements that the audit token carries a PID version for PID wrap-around | Search results, 2026-11-10 | Why a reused process number needs a generation | Search-result summary only; the posts were not read |
| Genode Foundations 24.05, "Recursive system structure" | 24.05 by URL; no version on the page | Identity assigned along the chain of parents | Read through WebFetch, 2026-11-10 |

## What they show

**Most systems that report a sender have the kernel supply it, per message.** Binder gives a service the Linux UID and the process ID of the process that sent the current transaction. QNX's `MsgInfo()` reports the sending process and thread of a received message. Apple's Developer Technical Support calls the audit token "the canonical identifier" and compares it with the one in the Mach message trailer. In each case the sender does not write the value.

**A value the sender states is not identity.** Linux `SCM_CREDENTIALS` lets the sender state a process, user and group ID, which the kernel checks against the sender's own unless it holds a privilege. Binder's work-source UID is set by the caller, and the reference says it cannot be trusted. Origin has no such field: nothing the writer passes reaches the stamp.

**Identity captured once goes stale.** `SO_PEERCRED` returns the credentials in effect when the socket was connected or created. Whoever writes later, through a descriptor that has been passed on, is not who that answer names. Origin's channel ends move between processes (ADR-0005 P8, A3), so an identity taken when a channel is created or an end is first held would name the wrong writer.

**A reused process number names the wrong process.** Binder's reference warns against using the PID as a security identifier, because PIDs are reused. Apple's audit token adds a PID version for wrap-around (search results only), and Linux added pidfds to keep naming one process. Zircon's object IDs are 64-bit, unique for the life of the running system and never reused, and 0 is invalid. ADR-0005 C1 chose Zircon's rule for process IDs, and S3-1 built it.

**Identity can be a property of the capability instead.** seL4 delivers, with each message, the badge of the endpoint capability the sender used. The server sets badges when it mints capabilities, one per client, so the badge tells it which client wrote. Origin's ends are capabilities too, but they are moved, not minted per client, so the end says only which channel a message came through. B1 adds who wrote it.

**A kernel that reports nothing leaves identity to the services.** Zircon's channel read reports no sender. A peer can learn the koid of a channel's other end from `zx_object_get_info`, and the process holding it only with a handle to a process. ADR-0011 N9 asks for more: the reader needs the writer's identity without a handle to it.

**Received handles come with their type and rights.** `zx_channel_read_etc` returns, for each handle, its value, object type and rights, and an unused field set to zero (16 bytes). When a buffer is too small it reports both sizes the message needs. ADR-0005 B3 chose the same record without the spare field.

**Identity can be assigned by the creator.** In Genode a parent that forwards a child's session request may change its label under its own policy, so a child's identity comes from its parents, not from the child. C1's principal works the same way: the creator sets it and the process cannot change it.

**Services that act for others need to separate the two identities.** Binder lets a service reset the calling identity of the current thread before calling interfaces in its own process, so their checks see the service and not its caller.

## What this means for Origin

1. **Stamped at write time, by `channel_write`.** The kernel takes the identity from the writing thread's process and stores it in the message under the channel lock, with the bytes and the handles. No argument carries it, so a writer cannot set or forge it (B1). It is not taken when the channel is created, because ends move (A3), and not when the message is read, because the writer may have exited by then.
2. **The identity is `{u64 process; u64 principal}` (C1).** Process IDs come from S3-1's counter: unique for the boot, never reused, 0 the kernel, and they grant nothing. A recorded ID never names a later process. The counter is 64 bits wide, and at one process every nanosecond it would last 584 years.
3. **The principal is given when the process is created, and never changes.** Only kernel code creates processes today, so the boot code gives each process its principal. S3-3's launch path will pass the creator's own by default, and a different one only through a capability (C1). It is a plain field, written before the process is shared and never after, so it needs no lock.
4. **Identity is information, not authority.** A process ID names no object and opens nothing (P1). A reader that wants to act on the writer needs a handle the writer gave it.
5. **A relayed message names the relay.** The stamp is always the last writer. A relay that vouches for an earlier sender says so in its own protocol, and the reader can trust that only as far as it trusts the relay.
6. **The reader gets it through `read_sizes`, by size (C2).** The two fields follow the 12 bytes at offsets 16 and 24, aligned to 8, and the structure becomes 32 bytes. A caller whose `size` is below 32 gets the 12-byte fields and no error, and nothing past them is written. The identity is written on `buffer_too_small` too, so a reader can decide from the writer whether to make room.
7. **Each received handle is a record `{u32 handle; u32 object_type; u32 rights}` (B3, C3).** It is 12 bytes, with no padding and no reserved field, encoded field by field. Unlike Zircon's 16-byte record it has no spare field: a new field would be a new structure, which the fingerprint lock reports at major 0 and refuses after it.
8. **The kernel writes no message today.** Only `syscall.rs` calls `ipc::channel_write`. If the kernel ever writes one, its process field is 0 (C1), and its principal will need a decision then.

## Tests defined before the code (§16 step 12)

- **Host, `channel.rs`:** a message keeps its writer's identity through the queue, a too-small read and a close, and the random-sequence model checks the identity of every message read.
- **Host, OIDL:** the semantic check accepts a plain struct of integers, enums and bits as a system-call vector or array element, and refuses a struct holding a handle, a string, a vector, an optional or a sized struct. The generated client and server encode and decode records field by field, never by the compiler's layout. The C header compiles as C23 and C++23. The mock-kernel program reads identity and records through the safe client.
- **Boot, ring 3:** the reader sees the writer's process ID and principal, all 64 bits of each, never its own. When an end moves from one writer to another, each message carries the identity of the process that wrote it, even when it is read after the move. A caller with the old size gets no identity and no error, and the bytes past 12 stay as they were. A too-small read reports the identity. Each record carries the object type and the rights the handle has in the reader's table.
- **Negative controls:** each boot proof above fails the boot when the kernel stamps the reader instead of the writer, stamps at read time, writes past the caller's size, skips the identity on `buffer_too_small`, or reports the wrong type or rights.

## Unknown

- **Program identity**, which C1 leaves to a later minor.
- **What principal values mean**: who assigns them and how they relate to accounts (CLAUDE.md §57). The kernel treats them as opaque numbers.
- **Discarding without reading.** Zircon can drop a message that does not fit. Origin's reader cannot drop an unwanted message without room for it. Neither B1 nor B3 needs that.
- **The kernel's principal**, if the kernel ever writes a message.
- **Thread identity.** QNX also reports the sending thread. C1 does not, because threads are not principals.
- **What sequential IDs reveal.** A reader learns roughly how many processes were created between two messages. C1 accepts this, since IDs grant nothing. The system-call threat model lists it as open.
