# Research: the start record (2026-11-10)

Backing notes for task S3-3 (CLAUDE.md §16): the start record version 1, written by one launch path in the kernel, under the owner's decisions C4, C5 and C6 (ADR-0005 Amendment 4, accepted 2026-11-09) and C1's rule that a process inherits its creator's principal. What was read, what it means for Origin, the tests defined before the code, and what stays unknown. These are conceptual references only: **no source code of any of these systems was read** (CLAUDE.md §3). The scoping of the whole step is `docs/research/2026-11-09-n14-step3-scope.md`.

## Sources

| Source | Version | Used for | How it was read |
|---|---|---|---|
| Fuchsia, "Program loading", `fuchsia.dev/fuchsia-src/concepts/process/program_loading` | No date on the page | The processargs bootstrap message: what a new process receives and who writes it | Read through WebFetch, 2026-11-10 |
| Fuchsia, `zx_process_start` reference, `fuchsia.dev/reference/syscalls/process_start` | No date on the page | What the kernel itself hands the first thread, and the other registers | Read through WebFetch, 2026-11-10 |
| Linux man-pages 6.19, `getauxval(3)` | 2026-02-08 | The auxiliary vector the kernel's loader passes, its random bytes and its secure-mode flag | Read through WebFetch, 2026-11-10 |
| Linux man-pages 6.19, `execve(2)` | 2026-02-08 | Which credentials a new program keeps, how set-user-ID changes them, and the size limits on arguments and environment | Read through WebFetch, 2026-11-10 |
| Linux kernel documentation, `admin-guide/sysctl/kernel` (`randomize_va_space`) | 7.4.0-rc2 | Which regions of a new address space Linux places at random | Read through WebFetch, 2026-11-10 |

## What they show

**Who writes a new process's first data differs, and with it whom the process must trust.** Linux's ELF loader, in the kernel, passes the auxiliary vector to the new program. In Fuchsia a userspace loader writes the processargs message into the bootstrap channel, and the kernel only passes that channel's handle in the first argument register, with the other registers zero. Origin's ADR-0005 P6 and C4 put a read-only record, written by the kernel, at an address the first thread finds in RDI. A process then trusts only the kernel for what it starts with, whoever created it.

**Each handle comes with what it is for.** Fuchsia's processargs gives every initial handle an info entry naming its type or role: the process itself, its thread, its address-space root, its job, the loader service. Origin's entries carry a role and also the object type and rights, as B3's records do, so a process can check a handle before using it (C4).

**Arguments and environment are byte strings with limits.** Fuchsia's message carries argument and environment strings; Linux limits their total to a quarter of the allowed stack, and each string to 32 pages. C4 bounds the whole record at 64 KiB and 64 handles.

**Placement at random is the norm.** Linux places the stack, the mmap base and the vDSO at random addresses, and the heap too at its highest setting, and also hands the program 16 random bytes (`AT_RANDOM`). C4 places the record at a random address. Version 1 carries no random bytes.

**Who a process acts for is inherited, and changing it is the exception.** Across `execve` Linux keeps the real user and group IDs. A set-user-ID program changes the effective ID only through the program file's bit, and that bit is ignored under `no_new_privs`, on a `nosuid` mount and under ptrace; `AT_SECURE` tells the program. C1 makes Origin's principal the creator's by default, and a different one possible only through a capability. No such capability exists, so the launch path gives no way to choose another.

**The other registers are zero.** `zx_process_start` says all other registers are zero at start; P6 says the same of Origin's general-purpose registers.

## What this means for Origin

1. **One launch path in the kernel** creates the child from its creator. The child's principal is the creator's, read from the creator; the path takes no principal (C1). Only kernel code creates processes until the Loader Service exists (ONBF §8.3), so the kernel's boot proofs are the only creators today.
2. **What the child holds at start:** its own process and first thread, with C6's rights (process: read, write, manage, wait, duplicate, transfer; thread: wait, manage, duplicate, transfer), and the BOOTSTRAP and DIRECTORY channel ends the creator passes, with a channel's rights. Each goes in the child's table before its first thread can run.
3. **The layout, version 1 (C4):** a 32-byte header, `size` u32, `abi_major` u16, `abi_minor` u16, `abi_min_minor` u16, `reserved` u16, then `handle_count`, `arg_count`, `arg_bytes`, `env_count` and `env_bytes`, each u32. Then a 16-byte entry per handle, `{handle; object_type; rights; role}`, each a u32. Then the arguments, then the environment, each string a u32 length and its bytes, padded to 8 with zeros. The size is exact, every padding byte zero, at most 64 handles and 64 KiB. At major 0 the kernel supports only its own minor, so `abi_min_minor` is `abi_minor`.
4. **Roles (C5):** BOOTSTRAP 1, SELF_PROCESS 2, SELF_THREAD 3, DIRECTORY 5. The value 4, SERVICE in the hand-written ABI, is retired and never reused: a process finds services through the directory (ADR-0011 N9).
5. **Read-only, at a random address:** whole pages mapped read-only and not executable, placed at a random page in the upper part of the user half, clear of every other mapping, and written before the first thread runs. Without hardware entropy the launch fails: the record is never placed at a predictable address.
6. **A failed launch leaves nothing.** The child's handle to itself would keep a never-started process alive, so a launch that fails after granting closes the child's table.
7. **The codec stays hand-written** in `origin-abi` (the scoping ledger's §4), and a host test holds its offsets to the definition's `start_header` and `start_entry`.

## Tests defined before the code (§16 step 12)

- **Host, `origin-abi`:** a record with handles, arguments and environment encodes and decodes to the same values; its size is exact and its padding zero; 65 handles, and a record over 64 KiB, are refused; a wrong `size`, a non-zero `reserved` or padding byte, a string past the end and counts that disagree are each refused. The offsets equal the definition's.
- **Boot, ring 3:** a child launched from a creator finds the record's address in RDI; reads back the header, each entry (handle, type, C6 rights, role), the arguments and the environment; uses BOOTSTRAP, SELF_PROCESS and SELF_THREAD; and faults when it writes the record. The message it writes on BOOTSTRAP names the child and the creator's principal (B1). Two launches place their records at different addresses. A launch whose record would pass 64 KiB is refused and keeps nothing. A leak check covers it all.
- **Negative controls:** the record mapped writable, RDI not the record, a principal not the creator's, wider rights than C6, a fixed address, a wrong size or role, and a failed launch that keeps its handles: each fails the boot.

## Unknown

- **Random bytes for the program,** as Linux's `AT_RANDOM`: version 1 has no field for them.
- **Process creation from ring 3,** through the Loader Service (ONBF §8.3). Then a creator's handles are moved out of its table into the child's, as `channel_write` moves them.
- **What DIRECTORY names:** the service directory comes with S3-4. Until then the launch path passes whatever channel end its creator gives.
- **A principal through a capability** (C1): no such capability exists.
- **CET at entry** (P6): still a recorded deviation (system-call threat model, E11).
