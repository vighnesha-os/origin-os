# Design review: the directive and the design documents, 2026-11-10

| | |
|---|---|
| Asked for | The owner: "review very carefully the CLAUDE.md and design documents … Extreme highest security, performance, design, native SDK no calling of shell commands from GUI or UI. Check very carefully each and everything." |
| Read in full | CLAUDE.md 7.1, and every line 7.2 and 7.3 changed; ADR-0001 to ADR-0015 with their amendments; `docs/spec/ONBF.md`, its errata, signature encodings and key registry; `docs/spec/OIDL.md`; the three threat models; the decision sheet (items 1–105); `TASKS.md`; the pre-merge review; `origin.kernel.oidl` and `origin.directory.oidl`; the workspace, toolchain, version and CI files |
| Read in part | `STATUS.md` (the top and the milestone table in full, the rest trimmed); the research ledgers, where a finding rested on one; the source, where a finding had to be checked |
| At | `land/s3-part2` at `02d9ba8` (the newest code, CLAUDE.md 7.2) and `docs/editions-device-sdk` at `a95d05b` (CLAUDE.md 7.3, ADR-0014, ADR-0015). Neither is on `main` (finding 1) |
| How | One AI reviewer read the documents and checked each finding against the code at those commits. Three read-only sweeps by sub-reviewers covered the source tree, the build and CI, and the records. Every sweep result used below as a finding was re-read in the file first; the list of what conforms is the sweeps' own. The uncommitted edits in the worktree `editions-devices` were read with `git status` and `git diff`; nothing there was changed |
| Not done | Nothing was built, tested or booted: the machine was running other sessions' boots, and this is a review of documents. §17's standard revisions were not re-checked today (the baseline is 2026-11-07). The cryptography's mathematics and the soundness of each `unsafe` block were not audited. **No human has reviewed this** |
| Outcome | The owner accepted all 18 decisions on 2026-11-10 ("Accept all"). They are decision sheet items 106–123: decision *n* below is item 105 + *n*. What follows is the review as the owner read it. The state it describes in findings 1 and 2 has changed since: the editions branch is committed, pushed and merged with the landing line, and the commit holding the PDF is on no branch |

Line numbers are those of the two commits above. `E:` marks a file read on `docs/editions-device-sdk`; everything else is `land/s3-part2`.

## Result

The documents are strongest where they are specific. ONBF verifies before it parses, allows one reading of a file, and needs two signature algorithms to agree. OIDL's messages have one encoding and an exact size, with handles out of band. Handles carry rights that only shrink, the kernel stamps each message with its writer, and a process starts with nothing but its start record. Each threat model lists what is open, with a negative control for what it calls proven. The status words are honest: "QEMU only" and "no human has reviewed this code" appear wherever they apply.

The weaknesses are of five kinds:

1. **The accepted rules are not where sessions read them** (findings 1, 2).
2. **Two promises have no mechanism yet:** nothing first-party runs a command (3), and a kernel that holds only what a kernel must (6).
3. **One limit is built into the ABI:** a 32-bit handle gives every process a lifetime (4).
4. **One defect is in code that runs on every boot:** assembly that promises the compiler something it does not do (5).
5. **Decisions are missing for code that exists, or that the next step needs:** a system threat model and a speculation policy (7), a random-number generator (8), locking rules (13), performance budgets (10).

Only the kernel's own proof programs run in ring 3 today, so none of this is yet a hole a hostile program can use. Findings 3 to 8 should be settled before ADR-0011 N14 step 4 puts the first real program there.

## Findings

Severity says when: **High** before the step named; **Medium** before the milestone named; **Low** is tidying.

### 1. The accepted directive is not the one sessions read (High; now)

- `origin/main` carries CLAUDE.md 7.1. Revision 7.2 (accepted 2026-11-09) is only on the draft pull request's branch.
- Revision 7.3, ADR-0014, ADR-0015, ADR-0002 Amendment 1, ADR-0009 Amendment 1 and decision items 71–96 (accepted 2026-11-10) are only on the local branch `docs/editions-device-sdk`, which is on no remote. Nor is `land/s3-part2`.
- Item 97, the rule it adds to §3, and a research ledger (`docs/research/2026-11-10-linux-ubuntu-device-coverage.md`) are not committed at all. They are edits to seven files in the worktree `.claude/worktrees/editions-devices`, last changed at 11:59 today.
- The main checkout is on `m0/toolchain-verification`: CLAUDE.md 7.1, ADR-0001 to ADR-0006 only, 109 commits behind its own upstream. A session started there is given 7.1. This review was.

**Why it matters.** §79 makes the repository the record, and CLAUDE.md is the rule every agent follows (§77). Three rules the owner accepted are invisible from the main checkout: no first-party component starts another program (`E:CLAUDE.md:1165`), reference code is read and written in separate sessions (`E:CLAUDE.md:160`), and restricted documents stay out of Git (`E:CLAUDE.md:502`). Items 71–97 exist on one disk, and item 97 in one working tree.

**Recommend.** Commit the worktree's edits and push `docs/editions-device-sdk` today. Land the directive, the ADRs and the decision sheet on `main` through a pull request of their own as soon as the owner accepts them, outside the 100-commit rule. That changes decision item 70, so it is the owner's. Keep the main checkout on `main`.

### 2. Intel's VT-d specification is committed on the checked-out branch (High; before any push)

- Commit `5b6ab78` on the local `m0/toolchain-verification` adds `specifications/D51397-019-vt-directed-io-spec.pdf` (12,791,994 bytes). Its message says "Redistribution terms have not been reviewed (§17)".
- Decision item 18 records that PDF as kept "outside the repository because it is Intel's document", and `E:CLAUDE.md:502` keeps such documents out of the public repository.
- The branch tracks `origin/m0/toolchain-verification` (ahead 3, behind 109). A pull followed by a push would publish it.
- `879601c` on the same branch, the gem5 section of the processor-simulator ledger, is on no other branch.
- Nothing stops a document being committed: `.gitignore` has no rule and `cargo xtask check` has no gate.

**Recommend.** Do not push this branch. Move `879601c` to the landing branch, drop `5b6ab78`, then reset the local branch to its upstream. Add `specifications/` to `.gitignore`, and a gate that fails on a PDF or similar document outside an allowlist (the test vectors, with their notices).

### 3. "No first-party component runs a command" is a rule without a mechanism (High; before the Loader Service's interface)

This is the owner's first requirement, and ADR-0011 states it well (N1, N2). Today nothing breaks it: no shipping crate starts a process or a shell, and no system call or interface creates one. What will enforce it later is thin:

- N13 lists a gate, "first-party system code starts a first-party tool as a process" (`ADR-0011:112`). It is not built: `check()` runs no such gate (`developer-tools/xtask/src/main.rs:2717–2726`). A source lint could not see what a program does at run time in any case.
- A process's start record carries arguments and an environment as byte strings (`ADR-0005:127`, C4), checked for length and padding only (`runtime/abi/src/lib.rs:541–561`). No document says who may set them, whether a child inherits them, or that no system library reads them. Environment strings are how other systems grew `LD_PRELOAD` and `PATH` attacks.
- No document says that starting a program is itself a capability, or which first-party components hold it.
- OIDL has no rule against a method that takes text for the receiver to run.

**Recommend,** at choke points the design already has:

1. Starting a program is a named capability (say `process.launch`) on the Loader Service's interface, through OIDL's `requires` (`OIDL.md` §6).
2. A release gate reads every first-party manifest (ONBF §6.2 `capabilities`). Only a short list may declare it: the session launcher, the shell, the terminal, developer tools. Settings, the installer, Origin Setup, the desktop and every service: never.
3. A program without the capability is never handed the launch interface, because its launcher builds its directory from what it declared (ADR-0011 N9). The rule then holds at run time, not only in review.
4. The base image has no command interpreter. The shell and terminal are an optional developer package (§48 already calls the terminal optional).
5. No system library reads the environment. A child's environment is empty unless its launcher sets it. First-party services take their configuration through typed interfaces, never through arguments.
6. A GUI test (§72) reads the kernel's record of process creation and fails if a first-party GUI component created a process.
7. An OIDL review rule: no method takes text that the receiver executes or hands to a program.

This adds gates to ADR-0011 N13 and a rule to ADR-0005 P6, so it is the owner's to accept. The capability's name is decided with the Loader Service's interface (§84).

### 4. A 32-bit handle gives every process a lifetime (High; while `user-abi` is at major 0)

- A handle is 16 bits of slot and 16 bits of generation (`framekernel/src/handles.rs:3–5`; `runtime/abi/src/lib.rs:36`; ADR-0005 P1 `:20`).
- A slot is retired when its generations run out, so that a stale handle never names a reused slot (`handles.rs:9–11`, `:188–193`).
- A process can therefore hold at most 65,535 handles at once (`handles.rs:22`), and create at most 65,535 × 65,535 ≈ 4.29 billion in its life. After that every call that makes or receives a handle returns `limit`, for good.

At 1,000 handles a second that is 50 days. At 10,000 it is 5 days.

**Why it matters.** In an SDK-first system a handle is the unit of every service call: `origin.directory`'s `connect` passes one per connection, and any service that hands out a channel or an object per request makes one or two each time. The services that churn handles fastest are the ones that must never restart, and Origin Server expects months of uptime. The cap on live handles also bounds how many files or connections one server process can hold.

**Recommend.** Decide now, while nothing outside the tree depends on it: 64-bit handles (for example 24 bits of slot and 40 of generation). The guarantee stays and the lifetime goes. The cost is 4 bytes in `received_handle` and 8 in `start_entry`; registers are 64-bit already, and a channel message's slots are positions, not handle values, so the wire format does not change. The alternative is to let generations wrap, which weakens the guarantee to "very unlikely". This changes ADR-0005 P1 and `OIDL.md` §10 (`:451`), so it is an ABI decision for the owner (§84).

### 5. Twelve assembly blocks promise to preserve the flags; five certainly do not (High; now)

- In `framekernel/frame/src/cpu.rs` the blocks at `:1488`, `:1740`, `:1899`, `:1917` and `:2042` execute `or` or `and` and declare `options(…, preserves_flags)`. Both instructions write the status flags.
- Rust's rule for that option is that the block leaves the status flags as it found them. The compiler may then keep a comparison's result in the flags across the block. Breaking the rule is undefined behaviour.
- Seven more blocks declare it around a move to or from a control register and nothing else: `cpu.rs:528`, `:536`, `:1212`, `:1359`, `:1385`, `:1793`, and the loader's `boot/uefi/src/main.rs:926`. Intel's manual entry for that move should be read (§16): as this reviewer recalls it, it leaves the status flags undefined. That recollection is not a check.
- A scan of all 79 assembly blocks found 40 with the option and these 12 among them. The same file does the same work correctly, without the option, at `:504–512`.

**Why it matters.** This is the one crate whose soundness every other kernel crate rests on (ADR-0011 N6, S8), and these blocks set CR0, CR4 and EFER on every processor at every boot. The boots pass, which shows only that today's compiler output happens not to use the freedom. Each block's SAFETY comment argues about the registers, not the options, and no gate reads the options.

**Recommend.** Remove `preserves_flags` from the five now, and from the seven unless both vendors' manuals say the flags are unchanged. Add to the `unsafe` gate: a block that declares `preserves_flags` may hold only instructions from a short list known not to write flags.

### 6. No decision says what runs in ring 0, and the kernel library holds much that should not (High; before the kernel grows further)

§18 asks for a kernel-architecture ADR (`E:CLAUDE.md:508`). ADR-0011 says how kernel code is written (N4, N6). Nothing says what belongs there.

- **The kernel library holds code that is not a kernel's.** `framekernel/src/lib.rs` lists `base64` "for PEM and tokens" (`:26`), `json` for "config, manifests, SDK data" (`:58`), `semver` for "package dependency resolution" (`:80`), `raster` "for the UI" (`:78`), `text` "for the text engine" (`:66`), and `glob`, `editbuffer`, `deflate`, `dns`, `dhcp`, `tftp`, `tcp`, `route`, `ustar`. Twelve of these modules (4,620 lines) are named by nothing outside themselves. §81 puts this work under `networking/`, `ui/`, `runtime/` and `storage/`.
- **Two of them have no use under accepted decisions.** On the device a manifest is OCBOR-1 (`ONBF.md` §5), so nothing there parses JSON manifests. The package path has no X.509 (ADR-0001 default 4), so nothing there reads PEM.
- **The kernel library and the framework are under no no-panic rule.** Neither carries the lints, and neither is in the object-level check (`developer-tools/xtask/src/main.rs:120–131`), which the pre-merge review lists as open. A panic in the kernel stops a processor.
- **The gate does not fix which files may hold `unsafe`.** `origin-frame` is 31,288 lines. Twenty files (about 12,500 lines) use `unsafe`. Fourteen (about 13,600 lines: `pci.rs`, `ivrs.rs`, `amdvi.rs`, `topology.rs`, `vtd.rs`, `acpi.rs`, `dmar.rs` and others) forbid it at the top of the module, which the compiler enforces. Seventeen (about 5,000 lines) do neither. The gate is by directory (`developer-tools/xtask/src/unsafety.rs:27`), so adding `unsafe` to one of the seventeen, or deleting a module's forbid, fails nothing.

**Why it matters.** In a framekernel everything in these crates runs in ring 0. Safe Rust removes memory errors. It does not remove logic errors or panics. ADR-0015 D4 already puts the radio stacks in user mode for exactly this reason; no rule says the same for TCP/IP, filesystems, text or drawing.

**Recommend.** One ADR, "what lives where": a user-mode service by default; kernel placement needs a stated reason (it must exist before user mode can, or a measured cost); every in-kernel path that reads outside data is under the no-panic lints. Move the modules that are not the kernel's to the crates §81 names: they are pure, host-tested code, so the move is mechanical. Give the gate the list of the twenty files, and require the forbid in every other. The boot-proof programs are also compiled into every kernel image; ADR-0013's question 5 already leaves that for M9.

### 7. There is no system threat model, and no decision on speculation or the 2026 processors' own defences (High; before N14 step 4)

- Each of the three threat models covers one boundary, and each puts physical attacks, firmware and side channels out of scope (`THREAT_MODEL_SYSCALLS.md:27`, `THREAT_MODEL_IOMMU.md:26`, `THREAT_MODEL_ADDRESS_SPACES.md:25`). No document accepts those exclusions for the product. Yet §21, §58 and §65 require storage encryption, Secure Boot, a TPM and recovery keys, which exist to stop a physical attacker.
- Speculative execution is open, with no ADR: "None: no index masking, no indirect-branch mitigation, no page-table isolation, and `IA32_ARCH_CAPABILITIES` is not read" (`THREAT_MODEL_SYSCALLS.md:100`). CLAUDE.md says only "Relevant security mitigations" (`E:CLAUDE.md:646`).
- Rust does not help here. A bounds check is a branch, and a mispredicted branch reads out of bounds speculatively in safe code too. The kernel half, with a direct map of all memory, is in every address space, with no page-table isolation (ADR-0004 H2, H5).
- FRED is named once, in a table of alternatives (`E:ADR-0009:151`). LASS is named nowhere. Panther Lake, the first Intel generation in scope, is the first with FRED ([Phoronix, April 2026](https://www.phoronix.com/news/Linux-7.1-Enabled-Intel-FRED), a secondary source). FRED replaces the entry path whose hazards the system-call threat model defends by hand (E3 to E5). Intel's specification has not been read for Origin; the first handoff already says to research it under §16 rather than from memory.
- The model has no row for a nested NMI: one that arrives after the NMI handler has returned from an exception of its own, onto the same IST stack.

**Recommend.**

1. `docs/engineering/THREAT_MODEL_SYSTEM.md`: the attackers (a network peer, a hostile program, a hostile device, a thief with the machine off or locked, someone with brief access, a stolen publisher key, a compromised update server, a server's own administrator), what each edition defends, and which component covers each.
2. An ADR on speculation and processor hardening, before any program that is not a boot proof runs in ring 3, as the threat model already demands (`:159–162`). It decides: reading `IA32_ARCH_CAPABILITIES`; enhanced or automatic IBRS; IBPB between principals; the SMT policy; whether the kernel uses supervisor shadow stacks; FRED and LASS where CPUID reports them, with the existing entry path kept for AMD's Zen 5; the compiler-side options (§14).
3. An index-masking primitive in the framework, used by the handle table and the call table, so safe code above gets lookups that stay in bounds under speculation.

### 8. Randomness comes straight from the processor, with no generator and no health gate (High; before S3-5b lands)

- There is no random-number generator. `framekernel/src/entropy.rs:7–10` says so: "a later, reviewed piece".
- Every consumer draws RDSEED or RDRAND directly: where a start record goes (`launch.rs:77`), DHCP and TFTP identifiers (`virtionet.rs:1151`, `:1253`), the loader's KASLR slides, both from one draw (`boot/uefi/src/main.rs:956–981`), and, on the in-flight `claude/s3-5b-memory-objects`, one draw for every mapping (`memory_object.rs:183`).
- The boot proof that checks the source is classed Report: "Nothing consumes it yet" (`ADR-0013:137`; `bootproof.rs:90`). ADR-0013's own rule moves a proof "when something comes to depend on what it proves" (R3, `:43–45`).
- ADR-0004 H3 names `EFI_RNG_PROTOCOL` as the loader's last source (`:31`). The loader has no such code.

**Why it matters.** A source that is stuck but still reports success gives predictable addresses and identifiers, and nothing refuses. One hardware source with nothing mixed in is a single point of trust. A process mapping in a loop makes a hardware draw each time.

**Recommend.** Build the kernel's generator now, as a framework service: HMAC_DRBG from NIST SP 800-90A over the SHA-512 that `origin-crypto` already has, with NIST's vectors (§4, §59: an established algorithm, not a new one). Seed it from RDSEED, RDRAND and a seed the loader takes from the firmware; keep state per processor; reseed. Run SP 800-90B's repetition and proportion tests on raw draws. Move the entropy proof to Withdraw: without a healthy seed, no launch and no random placement, which already fail closed. Correct ADR-0013's row, and build or strike H3's firmware clause. Add SP 800-90A and 800-90B to §17.

### 9. The monotonic clock writes one shared cache line on every read (Medium; before `origin.core`'s clock)

- Every read does a compare-exchange on one variable shared by all processors (ADR-0012 C2). The ADR measured the cost: 2 ns with one reader and 582 ns with ten on the M1 host, and "it does not scale when everything reads the clock at once" (`:107–118`).
- User mode will read the clock through a system call (ADR-0005 C13), while ADR-0011 N8 promises "clock reads from a shared page".
- One wild reading "stops the clock for the rest of the boot" (`ADR-0012:67`).
- The design rests on one observation: a step back of 16 ticks in 261,083 handoffs, in 1 boot of 13, on a nested virtual machine. An unfenced RDTSC can produce that with counters that agree exactly. The reader's RDTSC may execute before the load that hands it the other processor's value has completed. The ledger records that RDTSC is unordered and that LFENCE orders it (rows I1, I7). No run with a fenced read is recorded.

**Why it matters.** Clock reads are among the most frequent calls on a desktop: frame pacing, audio, timers, every program's `now`. P1 and P2 depend on them.

**Recommend.** Keep C1's contract. Then:

1. Run `prove_consistent` with `LFENCE; RDTSC` on the same CI runners. If the steps go, the counters agree, and the floor is paying for an artifact of the measurement.
2. Design the fast path before step 4: a fenced per-processor read while the counters are trusted (invariant TSC, a boot check, a periodic cross-processor check), with the floor as the state the kernel falls back to when a check fails.
3. Bound how far one reading may raise the floor.

Step 2 would change ADR-0012 C2, which the owner accepted as unconditional. Step 1 changes nothing and should come first.

### 10. No performance or smoothness budget exists (Medium; before M8)

- ADR-0011 N8: "No numeric budget is set here" (`:76`). The only number in the documents is ONBF's cold-launch budget of 5 % (`ONBF.md:656`). §53 asks for thresholds on declared hardware (`E:CLAUDE.md:1305`), and there is no hardware.
- Meanwhile the costly paths are set: every system call returns through IRETQ (`THREAT_MODEL_SYSCALLS.md:41`), a synchronous call is a write, a wait and a read (`ADR-0005:53`), the tick is 100 Hz (`ADR-0003:37`), and the boot proofs add over five seconds to each boot.

**Recommend.** `docs/engineering/PERFORMANCE_BUDGETS.md`, with provisional targets marked as hypotheses: an IPC round trip, an empty system call, a context switch, a clock read, a page fault, input to display, the 99th-percentile frame time, boot to login, idle wake-ups. The kernel already prints several of these costs on every boot. Record them from CI's real-processor boots as trends, and make them thresholds when declared hardware exists. This claims nothing (rules 21, 22).

### 11. The directive has no security rules for the GUI (Medium; before M8's compositor design)

§50, §52 and §61 list features. Nothing requires that one program cannot read another's input or pixels. Two accepted designs already depend on it: ONBF's "trusted system prompt drawn by the compositor" (`ONBF.md:526`) and ADR-0014's "explicit elevation" (S5).

**Recommend,** for §52 and §58 in revision 7.4 (the owner's text):

- a client sees only its own surfaces and its own input;
- synthetic input, global shortcuts, screen capture, reading other surfaces, and clipboard access without a user action are capabilities;
- credential and elevation prompts are drawn and read by the compositor on a path no client can draw over or inject into, behind a secure attention key;
- a remote session is marked as one.

### 12. Server administration: two things to settle (Medium; with the remote-administration ADR)

- **The wording.** `E:CLAUDE.md:1173` says the server is administered "through the native GUI only". The same paragraph says neither edition "needs a terminal" (`:1179`), which is the real rule. "Only" leaves no place for unattended installation or for configuring many servers. Neither needs a terminal: both are SDK clients, like the command-line tools §70 already allows.
- **Identity.** ADR-0014 S7 has the remote endpoint "carrying any SDK interface to a remote caller" (`E:ADR-0014:75`). The kernel stamps a message with the process that wrote it (ADR-0011 N9). An endpoint that relays calls is stamped as itself, so services would apply policy and audit to the endpoint, not the administrator.

**Recommend.** State the rule as sufficiency: every administrative operation is in the native GUI, and none needs a terminal. Provisioning is a signed, declarative document that Origin Setup applies through the SDK, with no scripts, in the spirit of ONBF-R3. In the remote-administration ADR: each remote session is its own process under the authenticated principal, holding only that session's handles; a stated list of interfaces may be carried, not "any"; the protocol is a published one, with a hybrid key exchange (X25519 with ML-KEM-768, FIPS 203) to match the signature profile. FIPS 203 joins §17 then. The first point rewords decision item 74, so it is the owner's.

### 13. ADRs that §18 requires are missing or incomplete (Medium)

- **Locking and synchronization** (`E:CLAUDE.md:511`). No document gives a lock order. The address-space threat model has a rule the compiler "cannot" check (T20, `:74`) and an unbounded wait it left as it was (T19). **Recommend:** lock classes in a fixed order, enforced by types or by a debug-build checker; rules for interrupt context; "no shootdown while holding a lock taken with interrupts off" as a checked property.
- **The cryptographic profile** covers signatures only (ADR-0001 D3). Nothing is decided for storage encryption (§21, §40), password hashing (§57), transport (§54, ADR-0014) or the generator (finding 8). **Recommend:** extend the profile before M9, each part from a published standard (§59).
- **ADR-0002, 0003, 0004 and 0005** lack the trade-offs and migration consequences §18 requires. The sheet notes this for three of them (`OWNER_DECISIONS.md:97`). They are the base of the boot path, the kernel and the ABI. **Recommend:** add both sections with the next amendment to each.

### 14. The boot chain: what ONBF promises and ADR-0002 does not yet say (Medium; with the SYS profile)

- `ONBF.md` §8.1 (`:446–460`) has the loader verify the release manifest, check the rollback floor and measure into the TPM. ADR-0002 B2 has none of the three. TASKS item 6 tracks them; the ADR's own Consequences do not.
- A loader can measure through the firmware only before `ExitBootServices`. That constraint belongs in the ADR.
- Boot info carries the UEFI system table "for runtime services later" (`E:ADR-0002:61`). No decision says how the kernel calls firmware code in ring 0.
- ADR-0004 H3 says the kernel "never prints its base after boot" (`:31`). The loader prints both slides in every build (`boot/uefi/src/main.rs:451`), and the kernel prints a hexadecimal value in 61 places, addresses among them (each kernel segment's, for one: `kernel.rs:87`). The pre-merge review lists this as low. It becomes more than that at N14 step 4, when the Diagnostics interface serves the kernel log (ADR-0011 N11).

**Recommend.** ADR-0002 Amendment 2: measured boot (which registers, and the event log handed to the kernel); the release manifest and the rollback floor (a TPM counter, else an authenticated variable); firmware runtime calls (none after boot if possible; otherwise in an address space of their own, one at a time). Take addresses out of release logs before the Diagnostics interface exists.

### 15. Four statements the build makes that the code does not back (Medium; rule 14)

- `origin-version.toml:21`: "The boot loader refuses a release whose number is below the stored minimum." Nothing reads `SECURITY_VERSION` outside its own crate's tests.
- Every kernel manifest is packed with `cet.shstk = true` (`developer-tools/onbf-pack/src/krn.rs:234`). The loader and the kernel hold no shadow-stack code, and nothing reads the key.
- The manifest's `flags_digest`, its record of the compiler and linker flags (§60), is the SHA-256 of an empty string: `pack_kernel` passes no `--flags` (`xtask/src/main.rs:3482–3494`; `onbf-pack/src/main.rs:255`).
- `framekernel/frame/src/smp.rs:17–18`: "So W^X holds even while an AP starts." The trampoline's frame is written through the direct map (`:524–530`) and mapped executable at its identity address (`:533`), so the one frame is writable and executable at once until the identity mapping goes (`:365`).

**Recommend.** Make each true or say what is true: "will refuse" until the floor check exists; pack `shstk: false` for the kernel until it has shadow stacks; have `xtask` pass the canonical flags; unmap the trampoline's direct-map page, or make it read-only, while it is executable. One more sentence belongs in the threat models: every executable user frame also has a writable alias in the kernel's direct map (ADR-0004 H2). That is common practice, but ONBF-R5 reads as if no alias exists.

### 16. Reading reference code: three rules to add (Medium; before the first driver written this way)

ADR-0015 D6 first asked for "a clean-room process approved in an original-source-exception ADR", with one person specifying and another implementing. It now says the two-session process is that, "without a further ADR" (`E:ADR-0015:81`). Both sessions are the same model, which §3 itself says was trained on Linux.

**Recommend.**

1. Say what the intermediate specification may hold: facts (register layouts, required sequences, timings, firmware messages), in Origin's own structure and names. No code, no identifiers, and not the reference's file or function structure.
2. Prefer references under licences compatible with MIT or Apache-2.0 (BSD, ISC, MIT, and the vendor drivers that are dual-licensed), and record a notice.
3. Before the first such driver is published, run §6's similarity check against the files consulted, and have a person sign it off. Decision item 21 already advises legal advice on AI-written code; this is where it matters most.

### 17. The records do not yet meet §6, §78 and §82 (Medium)

From the records sweep; the lines quoted were re-read.

- **Provenance (§6).** The ledger is 600 KB of prose, organised by task. By the sweep's count of labelled fields, 3 of its 128 entries carry all ten that §6 lists. `origin-paging` and `origin-abi` have no entry of their own. Five entries still say the licence is not chosen. Neither copy has the fields 7.3 added for reference code (`E:CLAUDE.md:244`), and the consultations already recorded (QEMU, EDK II, and two Linux files at `E:docs/research/2026-11-10-cpu-identification.md:54`) carry no licence and no gap.
- **Task contracts (§78).** `TASKS.md` holds results and a backlog. No task states its owner, its dependencies, its security invariants, its performance constraints or its acceptance conditions. The next task, S3-5b, is a name and nothing more (`:76`).
- **Milestones (§82).** M0, M1 and M2 are marked done (`STATUS.md:35–38`). No record defines an exit test for any milestone. M0 is done while §10's `wcslen` test is outside the loader and CI never runs the §9 and §10 checks (finding 18).
- **Records the directive names that do not exist:** `docs/ROADMAP.md`, `docs/ARCHITECTURE.md`, `docs/engineering/CONTEXT_PROTOCOL.md`, a hardware compatibility database, an SBOM, and `SECURITY.md`, `CODEOWNERS` and `CONTRIBUTING.md` (the last three are TASKS item 12 on the editions branch).
- **Acceptance records.** ADR-0001 to ADR-0005 were accepted before the sheet existed and have no item. ADR-0009 is Accepted "by delegation" and says the owner has not reviewed its text. ADR-0007 gained two built changes marked "for the owner's review" (`:98`, `:101`) with no item. ADR-0005 Amendment 5's alternatives are in a ledger that exists only on `claude/s3-5b-memory-objects` (`ADR-0005:140`).

**Recommend.** A provenance table with one row per crate and §6's ten fields, checked by a gate, with the narrative kept beside it as evidence. A task template with §78's fields, used from S3-5b on. An exit-test list per milestone in `docs/ROADMAP.md`, with M0 to M2 marked against it. One confirming pass by the owner over ADR-0009 and ADR-0007's two additions.

### 18. Build and test gates behind the directive (Medium for the first two, Low for the rest)

- **Integer overflow wraps in the release kernel.** `[profile.release]` sets only `codegen-units` (`Cargo.toml:86–89`), and the framework crate, where arithmetic feeds page-table entries and raw pointers, has neither `clippy::arithmetic_side_effects` nor overflow checks. **Recommend** `overflow-checks = true` for the kernel and loader, with the cost measured: in the trusted crate a stop is better than a wrapped address.
- **Most of the kernel is tested only by booting it.** By the sweep's count about 21,000 of the 35,700 non-test lines of the kernel and the framework are compiled for the kernel target only, `syscall.rs` and `ipc.rs` among them, so only the QEMU boots run them. There are no Kani harnesses and no coverage-guided fuzz targets (both tracked). **Recommend** deciding, with the placement ADR, which of these modules can be made to build and test on the host.
- **The kernel is compiled for a generic x86-64**, though §14 sets x86-64-v3 and the kernel refuses anything less (`kernel.rs:118–154`). No target feature is set (`.cargo/config.toml`; `xtask/src/main.rs:3447–3457`), so bit counting and bit scans compile to slow sequences. **Recommend:** the loader checks the baseline before it enters the kernel (it already refuses extra features, `boot/uefi/src/kernel.rs:170`), then the kernel is built with `+popcnt,+lzcnt,+bmi1,+bmi2,+movbe` (integer only; no SIMD in the kernel).
- **CI never runs the §9 and §10 checks.** `developer-tools/toolchain/verify.sh` is called by no workflow. Its only record is from the Mac on 2026-11-07, from a tree with five uncommitted paths (`VERIFICATION.md:3`, `:19–20`). CI also builds without `--locked` and checks the compiler's version but not its commit (`.github/ci/install-rust.sh:12`).
- **§11's lint is narrow.** It matches the text `target_os="linux"` only (`xtask/src/main.rs:3353–3388`). `cfg(unix)`, `target_family` and `target_env` are all true on the userspace triple and pass.
- **No loader is built without a test feature** (`xtask/src/main.rs:152–159`); the release-shaped one is only linted. That is right while no real key exists. It needs its own boot test when one does.

### 19. Smaller points (Low)

- **`unsafe` without its argument (rule 5).** Twelve `#[unsafe(naked)]` functions (for example `cpu.rs:1663`, `:2056`, `:2062`, `:2068`), two `global_asm!` blocks and the extern block at `smp.rs:161` say what they do, but not what a caller must uphold. `clippy::missing_safety_doc` covers public functions only, so the workspace lints pass.
- **`boot/uefi/src/place.rs:196–201`** skips a segment that does not fit instead of failing. The parser's checks should make that unreachable; an error costs nothing.
- **The generated `StartHeader` decoder does not check that `reserved` is zero;** the hand-written one does (`runtime/abi/src/lib.rs:626`). OIDL has no way to say "must be zero" of a named field.
- **§17 omits specifications the code already implements:** VT-d Rev 5.20, AMD's 48882 Rev 3.11, virtio 1.1, RFC 1951, RFC 1350, RFC 2131. A register of specifications in use, built from the ledgers, would keep §17 true.
- **ADR-0015 D3's list of dangerous operations** should add UEFI variable writes, TPM clear and ownership, raw BMC commands, microcode updates and raw PCI configuration writes.
- **The signed statement does not name its suite or key** (`ONBF-signature-encoding.md` S3). It is safe with two suites of different lengths. Bind both in when a third suite is added.
- **Merkle roots are SHA-256,** so collision strength is about 128 bits beneath root keys that use ML-DSA-87. This is acceptable, and should be recorded as accepted.
- **Timer calibration tries the 8254 first** and spends about two seconds of budget where firmware has gated it (ADR-0013 F3, `:160`). Try CPUID 15h first where it reports a crystal frequency.
- **ADR-0008's 64 processors against ADR-0014's physical servers:** sizing per-processor state at discovery becomes a precondition of the server-targets amendment.
- **`origin.directory.oidl:15` writes `version 0.1` by hand,** though `origin-version.toml:3` calls itself the only file where a version is.
- **Records out of step:**
  - `README.md` says "There is no kernel yet".
  - `STATUS.md:29` says the trusted base "does not exist yet", four lines after `:25` says it landed.
  - `STATUS.md:41` says 13 proofs stop the boot; `bootproof.rs` has 19.
  - `STATUS.md:36` still gives the Microsoft-signed first stage as the default, and `:50` calls ADR-0007 I12 Proposed.
  - `ADR-0007:140` says question 10 is open; `:151` says it was accepted. `ADR-0011:5` says question 7 "is still the owner's"; `:181` records the answer.
  - "Proposed decisions" still heads `E:ADR-0002:15`, `ADR-0004:13` and `ADR-0005:17`, and `E:ADR-0002:56` says boot info is "at version 0.2".
  - `OWNER_DECISIONS.md:215` cites items 71–97; no commit holds item 97 (finding 1).
  - `TASKS.md:9` heads its Done table with the branch `m0/toolchain-verification`.
  - Final rule 11 (`E:CLAUDE.md:2167`) still binds only the UI.
- **Branches:** 71 local branches and 52 worktrees. One records commit on `claude/epic-joliot-bff82c` is on no line.

## What the sweeps found to conform

These are the sweeps' results, by pattern search and reading. They were not re-checked line by line.

- No shipping crate starts a process or a shell. Only host tools do (cargo, rustc, QEMU, git, `llvm-nm`), which ADR-0011 N12 exempts.
- No system call or interface creates a process, loads a file or takes a command. Launching exists only inside the kernel, for the boot proofs.
- The root `Cargo.lock` holds 19 workspace packages and no third-party package. Loom's 30 crates are in a lock of their own, pinned by checksum and reachable only from tests.
- No `bool` crosses an extern ABI or sits in a `repr(C)` type. Every decode of a boolean, an enum or a bit set from outside refuses an undeclared value. The one `transmute` (`irq.rs:80`) has its argument.
- Every crate inherits the workspace lints, and no crate turns one off at its root.
- Every shipping crate is `no_std`, with no Linux API and no Linux `cfg`.
- `todo!` and `unimplemented!`: none. `unwrap`, `expect` and `panic!` in the parsers of untrusted input and in the system-call and IPC paths: none.
- CI: actions pinned by commit, a read-only token, no secrets, QEMU checked by SHA-256.
- No manifest carries a version; the release identity is generated from one file.
- The OS's cryptography crate verifies only. Its vectors are NIST's and Wycheproof's, with their notices.

## Already tracked by the project

These are open in the project's own records, so they are not counted above: speculation controls and CET shadow stacks (pre-merge review; `THREAT_MODEL_SYSCALLS.md` I2, E11); the per-call system-call pin (E8); the open IOMMU threats T4 to T6, T8, T9, T11, T14, T15, T19 and T23; anti-rollback and TPM measurement (TASKS item 6); the no-panic check's reach; scheduler and allocator scaling, PCIDs, IOVA reuse; the production filesystem; Kani; coverage-guided fuzzing; an independent review of the cryptography; and any human review.

## Decisions for the owner

Each has the recommended answer. If accepted, they become decision items 106 onward, and the ADRs and CLAUDE.md 7.4 are amended before anything is built.

**Accepted by the owner on 2026-11-10: "Accept all".** The record is `OWNER_DECISIONS.md`, items 106–123.

| # | Question | Recommended |
|---|---|---|
| 1 | Commit and push the editions branch, and land directive, ADR and decision-sheet changes on `main` in their own pull requests, outside the 100-commit rule (finding 1)? | **Yes** |
| 2 | Drop the commit that holds Intel's PDF, keep the gem5 ledger, and add the ignore rule and the gate (finding 2)? | **Yes** |
| 3 | Make "no first-party component runs a command" structural: a launch capability, the manifest gate, no interpreter in the base image, an empty environment by default, the run-time test (finding 3)? | **Yes** |
| 4 | 64-bit handles, as an amendment to ADR-0005 P1 while `user-abi` is at major 0 (finding 4)? | **Yes** |
| 5 | Remove `preserves_flags` from the twelve assembly blocks and add the gate (finding 5)? | **Yes** |
| 6 | An ADR on what lives in the kernel; move the modules that are not the kernel's; pin the files that may hold `unsafe` (finding 6)? | **Yes** |
| 7 | A system threat model, and an ADR on speculation and processor hardening, both before N14 step 4 (finding 7)? | **Yes** |
| 8 | A kernel random-number generator (HMAC_DRBG over SHA-512) before S3-5b lands, with the entropy proof moved to Withdraw (finding 8)? | **Yes** |
| 9 | Run the fenced-read experiment on the clock now, and decide the fast path from its result (finding 9)? | **Yes** |
| 10 | Provisional performance budgets, recorded from CI as trends (finding 10)? | **Yes** |
| 11 | GUI security rules in §52 and §58 (finding 11)? | **Yes** |
| 12 | Reword "GUI only" as "the GUI is always enough, and nothing needs a terminal", with declarative provisioning and a process per remote session (finding 12)? | **Yes** |
| 13 | ADRs for locking and for the rest of the cryptographic profile; the two missing sections in ADR-0002 to ADR-0005 (finding 13)? | **Yes** |
| 14 | ADR-0002 Amendment 2: measured boot, the rollback floor, firmware runtime calls (finding 14)? | **Yes** |
| 15 | Make the four statements true or reword them (finding 15)? | **Yes** |
| 16 | The three added rules for reference code, with a person's sign-off on the first driver written that way (finding 16)? | **Yes** |
| 17 | A checked provenance table, task contracts from S3-5b on, and exit tests per milestone (finding 17)? | **Yes** |
| 18 | Overflow checks in the kernel and loader; x86-64-v3's integer features once the loader checks the baseline; `verify.sh` and `--locked` in CI; the wider §11 lint (finding 18)? | **Yes** |

## Sources outside the repository

Both are secondary sources, read on 2026-11-10 through a web search. Neither replaces the vendor's own documents (§16).

- [Phoronix: Linux 7.1 enables FRED by default for Panther Lake](https://www.phoronix.com/news/Linux-7.1-Enabled-Intel-FRED)
- [Phoronix: Intel LASS in Linux 7.1](https://www.phoronix.com/news/Intel-LASS-Linux-7.1)
