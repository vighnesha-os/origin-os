# ADR-0017: Speculation and processor hardening

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-10, as proposed, with the recommended answer to each of the nine questions at the end (decision sheet items 124 to 132; CLAUDE.md §18). The owner's answer was "Accept all", to one question that covered these nine and the nine acceptances of `THREAT_MODEL_SYSTEM.md`, in the session that wrote both. **Z1, Z2, Z3, Z5, Z9 and Z10 are built** (TASKS item 19b; "As built" below), under QEMU's software processor, which has none of the controls: no control has been written on a processor that has it. Z4, Z6, Z7, Z8 and Z12 are not built. No processor in scope has been run. No user-visible ABI changes (§84). Two answers touch accepted ADRs when the work is built: ADR-0003 K8 and ADR-0005 P4 will be amended to describe the FRED entry path (item 127), and ADR-0013's table gains a Report row (item 131). It is the ADR decision item 112 asked for. ADR-0011 N14 step 4 waited for Z2, Z3, Z5, Z9 and Z10; it now waits for them to be run on a processor that has the controls. **Amendment 1 (below): Accepted** by the owner on 2026-11-10 (item 152): the toolchain's prebuilt libraries are outside Z10, by name |
| Requires | CLAUDE.md P0, P1, §12 to §14, §16, §18 ("Speculation and processor hardening"), §24, §58, §74, rules 14, 16, 21, 30 and 40 |
| Evidence | `docs/research/2026-11-10-speculation-processor-hardening.md` ("the ledger"). Documents only: **no processor in scope has been run**, and no cost below is measured |
| Code | None changed. The code half (TASKS item 19b) would change `framekernel/frame/src/cpu.rs`, `trap.rs`, `usercopy.rs` and `smp.rs`, add one framework module, and touch `framekernel/src/handles.rs`, the OIDL generator's dispatcher, `.cargo/config.toml` and `developer-tools/xtask` |
| Related | ADR-0001 (default 7: shadow stacks, IBT later), ADR-0003 K8, ADR-0004 H1, H2 and H5, ADR-0005 P4 and P6, ADR-0009 Amendment 1, ADR-0011 N6 and N13, ADR-0013; `THREAT_MODEL_SYSCALLS.md` rows E1 to E5, E11, E12 and I2; `THREAT_MODEL_SYSTEM.md` |

## Context

**Nothing is in place.** The kernel reads no speculation register and writes none. It has SMEP, SMAP and UMIP, one LFENCE after its conditional SWAPGS, and that is all (ledger §7). The system-call threat model lists the gap as open (I2), with shadow stacks (E11) and now a nested NMI (E12).

**Safe Rust does not help.** A bounds check is a conditional branch. A processor that mispredicts it runs the access anyway, out of bounds, before it finds out. The kernel half, with a direct map of all memory, is in every address space (ADR-0004 H2, H5). So one speculative read in ring 0 can reach any process's memory.

**The owner's rule** is that this is decided before any program that is not a boot proof runs in ring 3 (decision item 112; CLAUDE.md §24).

**What the vendors' documents say,** in short (the ledger has each with its source):

- Intel's 2026 generations and AMD's family 1Ah isolate the kernel's indirect branch predictions in hardware, once a bit is set: enhanced IBRS on Intel, automatic IBRS on AMD. Neither isolates one program from another. That takes IBPB.
- Intel's table of affected processors gives Panther Lake, Wildcat Lake and Clearwater Forest as not affected by Meltdown, L1TF, the data-sampling families, Indirect Target Selection and the processor advisories of 2025 and 2026. What still applies needs software: bounds check bypass, branch history injection, speculative store bypass, the SWAPGS case.
- Both vendors guarantee LFENCE as a barrier after a bounds check. AMD also guarantees masking with a conditional move or an AND on all its processors. Intel guarantees masking only for the family 6 processors current when it wrote, and Nova Lake is family 18.
- FRED replaces the IDT, the interrupt stack table and SWAPGS with one delivery and two returns. It ends the nested-NMI hazard by design. Intel lists it for Panther Lake, Clearwater Forest and Diamond Rapids. AMD's Model 51h reports it; Zen 5 does not.
- LASS refuses an access to the wrong half of the address space before any page walk, speculative accesses included. Intel only.
- Both vendors have supervisor shadow stacks. On the IDT path they need a token that can be left busy by a fault; under FRED there is no token.
- The stable compiler can turn jump tables off. It has no stable switch for retpolines, IBT or a stack protector.

## Decisions

Each decision names the processors it applies to by what CPUID reports, never by name (§24).

### Z1. The kernel reads what each processor reports, and says what it found

- One framework module reads, on every processor as it starts: CPUID leaf 07H (subleaves 0, 1 and 2), Fn8000_0008 and Fn8000_0021, and `IA32_ARCH_CAPABILITIES` where CPUID.07H.00H:EDX[29] says it exists. It keeps the result for each processor: the cores of a hybrid part may differ.
- It sets the controls of Z2 to Z7 on that processor before the processor runs anything else, and again after a resume from sleep, since a processor's reset clears them.
- Every boot prints one line: the controls that are on, and by name anything this ADR needs that the processor does not offer. The same facts are later served through a typed interface, like every other piece of system state (§48).
- **It never refuses to boot on this ground,** as ADR-0009 A2 never refuses on the scope. Every machine Origin can be tested on today lacks something here. A machine that lacks a control is a bench machine: no claim rests on it (§76).

### Z2. Indirect branches in the kernel: the hardware isolation, set once

| The processor reports | The kernel does |
|---|---|
| Enhanced IBRS (`IA32_ARCH_CAPABILITIES` bit 1) | Sets `IA32_SPEC_CTRL.IBRS` and never clears it |
| and BHI_NO clear, with `BHI_DIS_S` available (CPUID.07H.02H:EDX[4]) | Sets `BHI_DIS_S` |
| and RRSBA set, with `RRSBA_DIS_S` available (CPUID.07H.02H:EDX[2]) | Sets `RRSBA_DIS_S` |
| Automatic IBRS (CPUID Fn8000_0021_EAX[8]) | Sets EFER.AIBRSE |
| Neither | Runs without it and says so (Z1). The older way, writing IBRS on every entry, is not built |

- **No retpolines.** Both vendors prefer the hardware control on these processors, and the stable compiler has no supported way to emit them (Z10).
- **No clearing of the return predictor on entry.** Intel's answer for entries from user mode is SMEP. AMD's is SMEP, user and kernel ranges that do not overlap, and an unmapped page between them. Origin has all three (ADR-0004 H1). That unmapped page becomes an invariant of this ADR as well.
- **`IPRED_DIS_S` stays off.** Intel offers it for systems where privileged code runs code a user wrote. Origin's rule is that it has none: the kernel never interprets or compiles anything a program supplies, as no first-party interface takes text to execute (§48).
- **Registers are cleared on entry.** After the entry path has saved a program's registers, it zeroes them: on a system call, every general-purpose register that does not carry an argument; on any other entry from ring 3, all of them. AMD recommends it for every processor (mitigation G-1): a speculative path then has no program-chosen value to work with.

### Z3. IBPB between programs

- Each processor remembers the last process whose code it ran in ring 3. Before it enters ring 3 for a different process, it issues an IBPB.
- Not between threads of one process. Not when the same process resumes after kernel threads ran.
- **First policy: every change of process.** A cheaper policy, by classes of programs that trust each other, needs manifests and a Loader Service to say what the classes are. It comes as an amendment with measurements.
- On AMD the command is IBPB, not SBPB, and the kernel checks IBPB_RET. Where it is 0 the return predictor is not cleared, and the boot line says so.
- Every boot prints what one IBPB costs on that processor, beside the context-switch cost it already prints. No vendor gives a figure.

### Z4. Speculative store bypass and data-independent timing: off, with the mechanism ready

- SSBD and PSFD are off in the kernel and by default in programs. Intel's guidance, last revised in 2021, knows of no practical attack through store bypass on an operating system, and aims the control at code that isolates by language, such as a JIT.
- The context switch carries a `SPEC_CTRL` value for each thread, so that a program can be given SSBD or PSFD. How a program asks is an interface decision, taken with the Loader Service's manifest (§84).
- Intel's data operand independent timing mode is handled the same way. Intel advises against turning it on for the whole system. The kernel turns it on around its own code that handles secrets; which code that is belongs to the cryptographic profile (TASKS item 27) and to ADR-0016.

### Z5. SMT stays on, with branch isolation between sibling threads

- Sibling threads may run different processes.
- Intel: enhanced IBRS already stops one thread steering its sibling's indirect branches. AMD: automatic IBRS covers ring 0 only, so the kernel sets STIBP once on every processor and leaves it, where Fn8000_0008_EBX[17] recommends that.
- What is left between two threads of a core is timing and contention on shared hardware. Neither vendor asks for SMT to be off on these processors; both ask for constant-time code where secrets are handled. `THREAT_MODEL_SYSTEM.md` accepts the remainder by name (its AC5; the owner's acceptance, item 137).
- Not in this version: placing only one process's threads on a core, and a setting that takes sibling threads offline. The first belongs with the scheduler's policy (TASKS item 4), the second with processor offlining (§24).

### Z6. FRED where the processor has it; today's path where it does not

- When every processor reports FRED (CPUID.07H.01H:EAX[17]), the kernel sets CR4.FRED and uses FRED for every entry and return. Otherwise it uses today's path: the IDT, the interrupt stack table, SWAPGS and IRETQ. AMD's Zen 5 is in the declared scope and has no FRED, so both paths ship in one image, chosen once at boot.
- **The stack levels.** Level 0 for everything from ring 3 and for ordinary events in ring 0. A higher level for NMI and machine check. The highest for double fault alone.
- **What FRED removes:** the conditional SWAPGS and its LFENCE (E4); the moment after SYSCALL when the kernel is still on the program's stack (E3); the SYSRET hazard behind E5, since ERETU checks the return state before it changes any register; the nested NMI (E12).
- **What the FRED path must put back,** because the IDT did it:
  - INT n from ring 3 arrives as an event instead of faulting on a gate's privilege level. The handler ends the thread, as E1's #GP does today.
  - SYSENTER from ring 3 arrives as an event too. It is refused: SYSCALL stays the one way in (ADR-0005 P4).
  - Under FRED, SYSCALL is delivered as an event in place of its usual operation, and that operation is what loads RCX and R11. The FRED path leaves in both what the IDT path leaves, so a program sees one convention.
- **On AMD,** FRED is used only after AMD's own text for it has been read against Intel's, or the proofs have passed on a family 1Ah machine that reports it. AMD's manuals enumerate FRED and list its registers, and describe nothing else yet.
- **The IDT path gets a guard for nested NMIs (Z12).**

### Z7. LASS where the processor has it

- Each processor sets CR4.LASS (CPUID.07H.01H:EAX[6]) once it runs only in the upper half.
- It needs nothing new from the layout: user space is the lower half and the kernel the upper (ADR-0004 H1, H2), and the kernel's copies from user memory already run with RFLAGS.AC set under SMAP.
- **Two constraints it creates.** An application processor starts in low memory, so it sets LASS only after it has left the trampoline. And firmware runtime services live in the lower half: if ADR-0002's Amendment 2 allows any call into firmware after boot, that call runs with LASS off, in an address space of its own.
- The boot proofs that probe a user address from ring 0 expect a page fault. With LASS they get #GP, and change with it.
- AMD has no LASS. SMEP and SMAP stay as they are there.

### Z8. Shadow stacks: programs first, then the kernel under FRED, then the kernel on the IDT path

1. **Programs' shadow stacks** are already decided (ADR-0001 default 7; ADR-0005 P6) and not built (E11). They need CR4.CET, a shadow stack for each thread and its saved state. Nothing in this ADR changes that.
2. **The kernel uses supervisor shadow stacks wherever FRED is on.** FRED keeps the shadow stack honest with its own registers, and has no token to be left busy. Each kernel stack gets a shadow stack, and a context switch moves both.
3. **On the IDT path, only on evidence.** Intel: only where CPUID.07H.01H:EDX[18] says a token cannot be left busy unless a push takes a page fault. AMD publishes no such bit, so there it waits for the proofs on a real Zen 5 machine. On both, a shadow stack is always mapped, in ordinary write-back memory, with its token and first frame inside one 32-byte aligned region of one page.
4. **Until the kernel has them,** its manifest says `cet.shstk = false` (decision item 120).
5. **No indirect branch tracking.** The stable compiler cannot emit it (Z10), and AMD has none.

### Z9. One primitive for every index and address that comes from ring 3

A new framework module, `origin_frame::nospec`, with a safe interface:

- **An index.** Given an index and a length, it returns the index if it is in range and nothing otherwise. The value it returns has also been through a sequence that keeps it in range when the processor has guessed the check wrong.
- **A user address.** Given an address, it returns one that is in the user half. Under a wrong guess, an address outside the half becomes one that cannot be read.
- **Two forms, chosen once at boot** from the CPU identification table (ADR-0009 A2):

  | Processor | Form | On whose word |
  |---|---|---|
  | AMD, any family | Mask: a conditional move or an AND that depends on the comparison | AMD says it applies to all its processors |
  | Intel, family 6 | Mask | Intel's statement covers the family 6 processors current when it was written, in 2018 and 2021, and promises guidance before any that differ. Neither page has changed since |
  | Intel, family 18 or higher; anything else | Fence: the check, then LFENCE | Both vendors guarantee LFENCE. Intel's masking statement does not reach these families |

- The sequences are written in assembly inside the framework. Written in Rust, a compiler may turn a mask back into a branch.
- This is the one choice here made from the vendor and family and not from a feature bit, because no bit reports it. It lives in the table ADR-0009 A2 created, where errata go for the same reason.
- On AMD the kernel first makes sure LFENCE is dispatch-serializing: CPUID Fn8000_0021_EAX[2], or MSR C001_1029 bit 1.
- **Where it is used first:**
  - the handle table (`framekernel/src/handles.rs`, `HandleTable::get`): the slot index taken from a handle;
  - the call number: the generated dispatcher uses no jump table (Z10), and if a table ever replaces its `match`, the table is indexed through the primitive. A wrong guess among the dispatcher's comparisons can run another call's decoder on this call's arguments; that decoder's own lookups are masked like any other;
  - user addresses (`framekernel/frame/src/usercopy.rs`): before the copy instruction, after the range check.
- **The rule after that:** any lookup whose index or address comes from ring 3 goes through it. No lint can tell where a value came from, so the system-call threat model keeps the list and each new call adds to it.
- Every boot prints what a masked and a fenced lookup cost.

### Z10. The compiler's options

On stable Rust 1.99.1, checked on the day (ledger §3.12):

| Option | Kernel and framework | Why |
|---|---|---|
| `-C jump-tables=no` | **On** | Stable. A `match` on a number from ring 3 then compiles to comparisons, not to an indirect jump through a table |
| `-C force-frame-pointers=yes`, overflow checks | On, as §14 and decision item 123 already say | |
| Retpolines | **Off** | The only way is a target feature the compiler warns will become an error. Z2 uses the hardware control instead |
| An INT3 after every RET | **On** (question 6, accepted: item 129) | AMD recommends it for every processor. The only way is a target feature the stable compiler accepts with a warning that it is unsupported. A gate reads the kernel's image and fails if a RET is not followed by INT3, so the option cannot stop working unnoticed |
| The same in hand-written assembly | On | The framework's own RET, IRETQ, ERETS and ERETU are each followed by an INT3, and a gate checks it |
| IBT, a stack protector | Off | No stable flag (§14, confirmed) |

No nightly compiler is used (§12).

### Z11. What the kernel does not build

- **No page-table isolation, no buffer clearing on exit, no L1D flush.** Intel's 2026 generations are not affected by what those answer, and AMD says it has built no processor with the Meltdown behaviour. ADR-0004 H5 stands. The kernel reads the bits that say so, and reports a processor on which one is missing (Z1).
- **No mitigation for the software-only issues of a hypervisor** (post-barrier return predictions, guest to host predictor isolation). Origin runs no guests. The ADR that adds virtualization must read those documents first.
- **No reading of energy counters by unprivileged programs.** This is a rule for the Power interface when it is designed.

### Z12. A guard for nested NMIs on the IDT path

On the path Z6 keeps for processors without FRED:

- **The property.** While an NMI handler is on the NMI stack, a second NMI changes nothing that handler still needs. It is not lost either: the handler runs again before the interrupted context resumes.
- **What the design has to work with.** The processor pushes every NMI's frame at the same address, and unblocks NMIs at the next IRET, whoever executes it (ledger §3.10). So the first frame has to be out of that place before anything in the handler can fault or return, and the instructions between the handler's end and its own IRETQ have to be argued one by one.
- **It holds whatever let the second NMI in:** the kernel's own return from a recoverable machine check, or an IRET executed below the kernel.
- The design is written from those two rules in Intel's and AMD's manuals and from nothing else (§3), and is proven with a control that raises an exception inside the handler.

## Evidence

- **The vendors' documents:** the ledger, with each document's number, revision, date, size and hash, and what was and was not read. Its §3.1 has `IA32_ARCH_CAPABILITIES` bit by bit; §3.3 to §3.6 the controls and the issues that still apply; §3.7 to §3.9 FRED, LASS and shadow stacks; §3.11 what each vendor promises about masking; §4 what two family 1Ah models report.
- **The compiler:** Rust 1.99.1 stable, asked on the development machine and given one probe file to compile five ways (ledger §3.12).
- **Origin's code:** read at `2fbaea6` (ledger §7).
- **Not evidence:** no processor in scope was run, no control was set on any machine, and nothing was measured.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Retpolines everywhere, no hardware control | Works on processors without enhanced or automatic IBRS | No supported way to emit them on the stable compiler. Both vendors prefer the hardware control on these processors |
| Page-table isolation now | Covers a processor that is affected by Meltdown | None in scope is. It costs every entry and exit, and ADR-0004 H5 was accepted without it. On Intel, LASS gives the part that still matters, hiding the kernel's layout from timing, at no cost on entry |
| IBPB only for programs that ask | Cheapest | The programs that most need it are the ones nobody asked. No class of program exists yet to carry the request |
| IBPB by trust class from the start | Fewer barriers on the IPC path | Needs manifests and a Loader Service. It can follow as an amendment once costs are measured |
| SMT off by default | Removes every channel between sibling threads | Halves the threads. Neither vendor asks for it on these processors |
| Core scheduling by process | Keeps SMT and removes the cross-thread exposure | Scheduler work that does not exist yet (TASKS item 4) |
| LFENCE after every check, no masking | One form, guaranteed by both vendors on every processor | A barrier on every handle lookup, on the path every service call takes. Unmeasured, but LFENCE waits for everything before it |
| Masking everywhere | Fastest | Intel does not promise it beyond family 6 |
| FRED only | One entry path | Zen 5 is in scope and has none |
| The IDT path only | Nothing new to build | Keeps the hazards E3 to E5 and E12 on processors that offer the cure |
| No supervisor shadow stacks | Less work; safe Rust already removes most ways to corrupt a return address | The framework is `unsafe` code, and ADR-0009 H3 already asks every declared machine for shadow stacks |
| Supervisor shadow stacks on the IDT path first | Covers Zen 5 at once | Needs the token, and a promise AMD does not publish |
| `IPRED_DIS_S` and SSBD always on | More is covered by default | Costs nobody has measured, against attacks the vendors say need a runtime Origin does not have |
| A nightly compiler for retpolines, IBT and a stack protector | All three become available | §12 keeps release builds on stable. None is needed on a processor in scope |

## Trade-offs

- **Every boundary between two programs costs an IBPB,** and Origin's design puts a boundary in every service call. This is the largest cost here and nobody has measured it. Z3 pays it until there is a number and a safe way to pay less.
- **Two entry paths** are twice the most delicate code in the kernel, and the IDT path stays for as long as Zen 5 is in scope. Each needs its own proofs.
- **The masking table rests on the vendors' words,** and on Intel having published nothing new. A table row is cheap to change; a wrong row is silent.
- **A processor without the controls still boots** and runs programs. Z1 says so on every boot, and that is all it does.
- **Shadow stacks arrive in three steps.** Until the last, the kernel on Zen 5 has none.

## Migration consequences

- **No user-visible ABI changes.** SYSCALL stays the one way in, with the same registers (ADR-0005 P4). `user-abi` does not move. An interface for a program to ask for SSBD comes later and is its own decision (Z4).
- **Accepted ADRs whose wording changes when this is built** (questions 4 and 8, items 127 and 131): ADR-0003 K8 and ADR-0005 P4 describe one entry path; ADR-0013's table gains rows.
- **`THREAT_MODEL_SYSCALLS.md`:** rows E1, E3, E4, E5, E11, E12 and I2 change as each part is built.
- **The boot proofs** gain: the controls of Z2, Z3 and Z5 read back on every processor; the FRED path's ring-3 proofs, the same programs as today; an NMI inside the NMI handler; a masked lookup that stays in bounds; a kernel image with no jump table. Proofs that probe user addresses from ring 0 change under LASS (Z7).
- **`onbf-pack`:** `cet.shstk` follows Z8 (decision item 120).
- **The CPU identification table** (TASKS item 16) gains a column: the form Z9 uses.
- **`PERFORMANCE_BUDGETS.md`** (TASKS item 26) takes the costs the boot prints, as trends.
- **Nothing on disk and no format** depends on any of this.

## Threat model (§58)

| Who | What they try | What stands in the way | What remains |
|---|---|---|---|
| A hostile program | To steer the kernel's indirect branches | Z2 | Whatever the processor's own isolation leaves. On a processor with neither control, everything |
| | To steer them through branch history | `BHI_DIS_S` (Z2); cleared registers | Intel's note that a barrier does not clear history, where BHI_NO is clear |
| | To make the kernel read out of bounds on a wrong guess | Z9 | A lookup nobody put on the list. A family whose row in the table is wrong |
| | To steer another program's branches | Z3 | Programs that share a process |
| | To learn the kernel's layout by timing | Z7 on Intel | Everything on AMD, which has no LASS |
| | To steer a sibling thread's branches | Z5 | Timing and contention between siblings (accepted by name) |
| | To use a kernel memory bug to return somewhere else | Z8 | Zen 5 until step 3; any processor until step 2 is built |
| Anything that raises a second NMI | To corrupt the first one's frame | Z6, Z12 | Nothing known, once built |
| A hypervisor above Origin | To withhold or misreport a control | Nothing: a guest cannot check its host | Accepted in the system model |
| A processor outside the scope | To be affected by what Z11 does not build | Z1 reports it | The machine is unprotected and says so |

## As built: task 19b (2026-11-10)

The decisions above stand as accepted. This section records what building six of them found, so that the text above is not read as more than the code does. One finding was put to the owner and is Amendment 1, at the end.

| Decision | Where it is | What the code does that the text above does not say |
|---|---|---|
| Z1 | `origin_frame::speculation` (the decision and the record, safe code), `cpu::read_speculation` and `cpu::init_speculation` | Each processor runs it from `load_tables`, before anything else. The plan and whether the registers read back are kept per processor in one atomic word. **Not built:** the pass after a resume from sleep (there is no sleep yet), and the typed interface |
| Z2 | The same; the trap entry in `cpu.rs` | `IA32_SPEC_CTRL` is written whole, once, where any bit of it is enumerated. The entry zeroes every general-purpose register but RSP and RDI, on every entry, not only from ring 3: the saved values come back from the frame. RBP is zeroed by a conditional move for an entry from ring 3 only, because from ring 0 it is the chain a backtrace follows |
| Z3 | `speculation::Ledger`, `cpu::entering_process`, one call in the scheduler's `follow_thread` | The first process a processor takes up counts as a change. The barrier is issued at the switch, which is before the thread can reach ring 3 |
| Z5 | The same plan | STIBP follows AMD's two bits (Fn8000_0008_EBX[15] and [17]) whatever the maker's name, as every setting follows its own bit |
| Z9 | `origin_frame::nospec`; two sequences in `cpu.rs` | **The form's table is `nospec::choose`,** because the CPU identification table (TASKS item 16) does not exist yet. The mask is CMP and CMOVAE, which is on both vendors' lists. **A third thing goes through it besides an index and an address: a count** that sizes a piece of a kernel buffer (`nospec::length`), at three places in `syscall.rs` when this was built. **Since 2026-11-11 the four memory calls go through it too.** They were written before the rule and reviewed against it then: a request record's size in `syscall.rs`, and a count of pages in `mapping.rs`, where the address of a run of pages also passes `nospec::user_address` before an address space is asked (`766b5d0`, `bc7beda`, `1c0d99f`). `THREAT_MODEL_SYSCALLS.md` has the list. **Not on the list, and with no clamp:** the walk of a space's page tables that `UserSpace::check_user`, `copy_in` and `copy_out` make by a program's address, before the copies clamp it. That review found it. The framework's session judged it a gap on 2026-11-11 and will clamp each page the walk visits, and the address in `UserSpace::map_shared_run`, `unmap` and `protect`, at the framework's own boundary. Neither clamp is built or in this batch yet. **AMD's LFENCE control** is set only where the processor does not report Fn8000_0021_EAX[2], its family is one AMD's mitigation G-2 says has the register, and no hypervisor runs it (ledger §3.14) |
| Z10 | `.cargo/config.toml`; `developer-tools/xtask/src/image.rs` | See Amendment 1 below. The gate also reads the trap entry, since no boot proof can see Z2's register clearing from inside. It found one return nobody had listed: the far return that reloads CS in `load_tables` |

**Boot proofs and their rows (ADR-0013):** "speculation controls" (Report, item 131), "index and address clamps" (Refuse) and "IBPB at each change of process" (Refuse). The first fails on a processor that offers none of the controls, so every boot under QEMU's software processor ends `degraded: 58 of 59 proofs passed; failed: speculation controls`. The owner kept that as built (decision item 154); a row that passes when the kernel set everything the machine offers, and only prints what is lacking, was offered and not chosen.

**What no test here shows.** What a processor does on a wrongly guessed path: no boot proof can, and QEMU's processor does not speculate. That any control takes effect: the development machine's emulator has none, so every write is unexercised until CI's boots under KVM. The costs: the figures the boot prints are an emulator's.

### Amendment 1: the toolchain's prebuilt libraries are outside Z10 (decision item 152)

**Status: Accepted by the owner on 2026-11-10, as recommended (decision sheet item 152; CLAUDE.md §18).** The owner's answer was "Accept all" to eight recommendations put in the coordinating session, which relayed it to the session that built 19b. Built: the gate already behaves this way.

**What Z10 now says.** "An INT3 after every RET" and "no jump table" hold for **the code the build compiles**. The toolchain ships `core`, `alloc` and `compiler_builtins` already compiled for the kernel's target, without the kernel's options, and stable Rust cannot rebuild them. Their functions that are not generic arrive in the image as built, and are outside both rules, **accepted by name**.

- The gate holds every function the build compiles, and fails on one that breaks either rule.
- It prints the libraries' count on every run. In the kernel of 2026-11-10: 113 returns, 33 of them without an INT3, and 2 jump tables, both in `core::fmt::Formatter` (`pad` and `pad_integral`).
- **The question is looked at again at every toolchain upgrade (§15):** whether the new compiler can rebuild them on stable, and what the count has become.

**Evidence.** The image, read by the gate and by a second script in the coordinating session, which found 32 returns in 27 functions where the gate finds 33. The thirty-third is `memmove`'s last return, the last instruction of the code section: nothing follows it there, and the gate counts that as no INT3. Half are in `core::fmt` (16); the rest are in `core`'s string and byte searches (5), `alloc` (4), `__udivti3` (3), `compiler_builtins`' byte comparison (2), `memcmp` and `memmove`. AMD asks for an INT3 after a return on every one of its processors (ledger §3.6, mitigation G-5).

**Alternatives.**

| Option | Why not |
|---|---|
| Pin a nightly compiler for `-Z build-std` | It closes the gap. It also changes the compiler baseline: §12's exception process, a second toolchain in CI, and a build the stable compiler cannot reproduce. Offered and not chosen |
| Origin's own `memcpy`, `memmove`, `memset` and `memcmp` in the framework | Shrinks the gap by a few returns. `core::fmt` and the division helper stay. Not ruled out for later |
| Let the gate fail on the libraries | No build would pass |

**Trade-offs.** A stated part of the accepted Z10 does not reach the 113 returns in those libraries, about a tenth of the image's, and 33 of them have no INT3. What stands against it: the exposure is straight-line speculation past a return, on AMD processors, in formatting, byte searching and comparison, vector growth and 128-bit division, and two tables indexed by a formatter's alignment, which no program chooses directly. What it buys: one compiler, the stable one.

**Migration consequences.** None in the code. `THREAT_MODEL_SYSCALLS.md` row I2 names the gap. The toolchain-upgrade task (§15) gains a step. If a later compiler can rebuild the libraries on stable, the gate's exception is removed and this amendment is closed.

**Threat model.** No new interface. The attacker is the hostile program of `THREAT_MODEL_SYSTEM.md`, steering the kernel's speculation; this is a residue of row I2, named there.

## What this does not prove

- **Anything about a processor.** No processor in scope has been run (§74, rule 30). Six of the decisions are code now ("As built" above), run only under an emulator that has none of the controls.
- That Panther Lake, Wildcat Lake or Clearwater Forest set the bits Intel's table implies. The table's own key says a processor that is not affected may not report it.
- That AMD's FRED behaves as Intel's chapter describes.
- That masking holds on Intel's 2026 family 6 models. It rests on a statement written before they existed.
- What any control costs.
- That QEMU can show any of this. Its software processor does not speculate, and it models none of these registers' effects.

## Questions for the owner (answered 2026-11-10: the recommended answer to all nine)

1. **Accept Z1 to Z12 as the policy,** to be built as TASKS item 19b with its own contract? **Recommended: yes.** **Yes (2026-11-10, item 124).**
2. **IBPB at every change of process** until trust classes exist and the cost is measured (Z3)? **Recommended: yes.** P0 comes before P1, and there is no number yet to argue from. The alternative is IBPB only between different principals, which leaves a sandboxed program free to aim at the same user's other programs. **Yes (2026-11-10, item 125).**
3. **SMT on,** with branch isolation between sibling threads, and the remaining timing channels accepted by name (Z5)? **Recommended: yes.** The alternative is SMT off by default on Origin Server. **Yes (2026-11-10, item 126).**
4. **FRED wherever every processor reports it, with the IDT path kept for the rest** (Z6), and ADR-0003 K8 and ADR-0005 P4 amended to say so when it is built? **Recommended: yes.** **Yes (2026-11-10, item 127).**
5. **On AMD, FRED waits** for AMD's own text or for proofs on a machine that has it (Z6)? **Recommended: yes.** **Yes (2026-11-10, item 128).**
6. **An INT3 after every RET through a target feature the stable compiler accepts with a warning** (Z10)? §12 keeps the build on stable features, and this one is in between: not nightly, not supported. **Recommended: yes, with the gate that reads the image.** The alternative is to harden only the hand-written assembly. **Yes (2026-11-10, item 129).**
7. **Supervisor shadow stacks under FRED first, and on the IDT path only on evidence** (Z8)? **Recommended: yes.** **Yes (2026-11-10, item 130).**
8. **A processor that lacks a control boots, and the boot says so** (Z1), as a Report row in ADR-0013's table; whether the production installer refuses such a machine is decided with decision item 90 at M9? **Recommended: yes.** **Yes (2026-11-10, item 131).**
9. **Masking on AMD and on Intel family 6, LFENCE on Intel family 18 and above and on anything unknown** (Z9)? **Recommended: yes.** The stricter alternative is LFENCE on every Intel processor until Intel renews its statement; it costs a barrier on every handle lookup on Panther Lake. **Yes (2026-11-10, item 132).**
