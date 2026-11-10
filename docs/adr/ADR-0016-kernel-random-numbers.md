# ADR-0016: The kernel's random-number generator

| | |
|---|---|
| Status | **Proposed** on 2026-11-10. Not accepted: the questions at the end are the owner's (CLAUDE.md §18, §84). **Built so far:** HMAC-SHA-512 and the HMAC_DRBG mechanism in `origin-crypto`, tested on the host and linked into nothing that boots. Everything this ADR proposes for the kernel, the loader and boot info is unbuilt, and stays unbuilt until the owner answers. Nothing here has run in a kernel, under QEMU or on hardware. **No human has reviewed it.** |
| Decides | Decision item 113 and finding 8 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`; TASKS item 18. It carries out ADR-0013 Amendment 1 and the note of 2026-11-10 in ADR-0004 H3 |
| Changes accepted decisions | Three, each named where it happens: ADR-0002 B4 (boot info 0.6, R10); ADR-0004 H3 (the firmware's generator is an addition and never a source of its own; two slides, two draws; R3, R10); ADR-0013's inventory (one row, which Amendment 1 already accepted; R8) |
| Requires | CLAUDE.md P0, P1, §3, §4, §13, §14, §16 to §19, §24, §58, §59, §61, §72, §84; rules 14, 16, 17, 21, 35 |
| Code | Built: `security/crypto/src/hmac.rs`, `hmac_drbg.rs`, `wipe.rs`. Proposed: `security/random/` (a new crate), `framekernel/frame/src/random.rs`, `boot/uefi`, `boot/info` |
| Evidence | `docs/research/2026-11-10-kernel-random-numbers.md` (the ledger); `docs/engineering/SOURCE_PROVENANCE.md`, "HMAC-SHA-512 and HMAC_DRBG" |
| Related | ADR-0001 D3, ADR-0002 B4, ADR-0004 H3, ADR-0008, ADR-0009 Amendment 1, ADR-0011 N6, ADR-0012, ADR-0013 Amendment 1 |

**One thing to know before reading on.** The ledger could not read the text of SP 800-90A or SP 800-90B: both are PDFs, and a download needs the owner's word, which was asked for. Six of the generator's published outputs and NIST's own test limits were checked today. The rest of what this ADR says the standards require is the author's knowledge of them, and the ledger marks each such row. Question 7 asks for the files. This ADR should not be accepted before they are read.

## Context

The kernel has no random-number generator. `framekernel/src/entropy.rs` says so: a generator is "a later, reviewed piece". Until now every consumer has drawn from the processor itself:

- where a process's start record goes (`framekernel/src/launch.rs`);
- DHCP and TFTP identifiers (`framekernel/src/virtionet.rs`);
- the loader's two KASLR slides, both from one draw (`boot/uefi/src/main.rs`);
- on the branch `claude/s3-5b-memory-objects`, one draw for every mapping.

Each draw also runs CPUID again to find out which instruction exists. Each consumer refuses when the instruction reports failure. None can see a source that is stuck and reports success.

That failure is not hypothetical. AMD's bulletin AMD-SB-7055 (2025-10-23) describes Zen 5 processors whose RDSEED can return zero far more often than a random source would while the carry flag reports success. Zen 5 is the oldest AMD core in Origin's declared scope. Origin uses the 64-bit form, which AMD says is not affected, but nothing holds it to that form and nothing would have noticed.

The owner accepted the remedy in outline (decision item 113): one generator, NIST's HMAC_DRBG over SHA-512; seeded from RDSEED, RDRAND and a seed the loader takes from the firmware; reseeded; with health tests on the raw draws; and the boot proof "hardware entropy" moved from Report to Withdraw. This ADR fills in the outline.

## Decisions

### R1. One generator, one algorithm

Every random value in the kernel and the loader comes from an HMAC_DRBG over SHA-512, as NIST SP 800-90A Rev. 1 §10.1.2 defines it, at a security strength of 256 bits. HMAC is FIPS 198-1's. Neither is changed in any step (CLAUDE.md §4, §59).

The revisions this is pinned to, as NIST's pages gave them on 2026-11-10:

| Document | Revision |
|---|---|
| SP 800-90A | Rev. 1, June 2015. A Rev. 2 is announced and has no draft yet |
| SP 800-90B | January 2018, with two errata NIST has announced and not yet folded in |
| SP 800-90C | September 2025, as a design reference for how the pieces are put together |
| FIPS 198-1 | July 2008. NIST plans to move it to SP 800-224, still a draft |

**Origin claims no conformance to any of them and no validation.** It uses their algorithms. R4 says why the health tests in particular cannot be the standard's.

### R2. Where the code lives

| Layer | Where | What it holds | `unsafe` |
|---|---|---|---|
| The mechanism | `security/crypto` (`origin-crypto`), **built** | HMAC-SHA-512; `HmacDrbgSha512`: instantiate, reseed, generate, a request with prediction resistance, uninstantiate | Forbidden |
| The policy | `security/random` (`origin-random`), a new crate | The health tests; how draws are retried; how a seed is put together; when a generator is reseeded. Written over two small traits, a source of draws and a clock, so all of it is tested on the host | Forbidden |
| The kernel's service | `framekernel/frame/src/random.rs` | The two instructions; a generator for each processor and the root; overwriting; the boot wiring. The only way the rest of the kernel gets a random value | Yes: the framework is where the kernel's `unsafe` lives (ADR-0011 N6) |
| The loader | `boot/uefi` | The two instructions; the firmware's protocol; the slides; the seed it hands on | As the loader has today |

- `framekernel/src/entropy.rs` goes. Its retry logic moves into the policy crate.
- The kernel links `origin-crypto` for the first time. The crate joins the leaf crates the `unsafe` gate lists, which it can because it forbids `unsafe`.
- **A gate** in `cargo xtask check`: the mnemonics `rdrand` and `rdseed` appear only in the framework's file and the loader's. That is what makes "nothing else draws from the processor's instructions directly" (§59) checkable.

### R3. The sources, and what each is trusted for

| Source | Role |
|---|---|
| **RDSEED**, 64-bit form | The entropy a seed is counted on. Intel's guide describes its output as conditioned entropy, provided for software to seed its own generators with |
| **RDRAND**, 64-bit form | Always mixed in. Counted on only when RDSEED is absent or unhealthy, and then the boot says the generator's strength is no more than the processor's own generator's |
| **The loader's seed**, 64 bytes | Always mixed in when present. **Never counted, and never enough by itself** |

Three things about these sources have to be said plainly.

- **They are not three independent sources.** By Intel's guide, RDSEED and RDRAND are two outputs of one circuit, which a package's cores share. AMD's design was not read. The firmware's generator may draw from the same circuit: the test firmware's most likely does (ledger §8). "Seeded from more than one source" (§59) is true of the interfaces. It becomes true of the entropy only where the firmware draws from something else.
- **The firmware's seed cannot be tested.** It is one sample from a generator the kernel never sees.
- **Only the 64-bit forms are used.** AMD-SB-7055 is about the shorter ones.

This changes ADR-0004 H3, which lists `EFI_RNG_PROTOCOL` as a source the loader may fall back on alone. Here it never stands alone (question 3).

### R4. Health tests on the raw draws

On each processor, for each instruction the processor reports:

1. **Not ready is not a value.** The carry flag clear means no value: retry, RDRAND up to 10 times (Intel's figure) and RDSEED up to 100 times with a PAUSE between tries, then give up on that instruction for now.
2. **Zero and all ones are not values.** A draw of 0 or of 2^64 − 1 with the carry flag set is discarded and retried like a draw that was not ready. For zero this is AMD's own workaround, and Intel's table describes a successful value as non-zero. All ones is added as a second stuck pattern, on no source read today. Together they discard two healthy draws in 2^64.
3. **Repetition count test** (SP 800-90B §4.4.1), cutoff 2: two equal draws in a row fail.
4. **Adaptive proportion test** (§4.4.2), a window of 512 draws, cutoff 2: the window's first draw coming again fails.
5. **Start-up:** 1,024 draws that pass all of the above, before any draw of that instruction is used. They are then used: R5.
6. **Continuous:** every later draw goes through tests 1 to 4.
7. **A failure** makes that instruction unhealthy on that processor. The draws in hand are discarded. It is used again only after a whole new start-up test passes, which is tried at the next reseed.

**The cutoffs.** SP 800-90B computes them from a source's assessed entropy and an accepted false-alarm rate, 2^-20. Worked exactly (ledger §5): for a 64-bit draw assumed to hold anywhere from 20 to 28 bits, both cutoffs are 2. At the entropy the vendors claim, the adaptive proportion formula gives a cutoff of 1, which every window meets at its first draw. So 2 is the tightest cutoff the standard's formulas give that means anything. A healthy source trips it about once in 2^55 windows.

**What these tests are not.** RDSEED and RDRAND are not noise sources. They return a conditioner's and a generator's output, and the noise behind them cannot be reached. SP 800-90B's tests are defined on raw noise. So this is not SP 800-90B health testing, and Origin must not call it that. The tests catch gross failure: a constant, a repeat, a handful of values. They cannot catch a source that looks random and is predictable. Against that there is only the mixing in R3, which helps only as far as the sources differ (question 4).

### R5. Seeding

A generator is instantiated from everything its processor's start-up tests drew and accepted. No draw is thrown away.

| Parameter | With RDSEED healthy | With RDRAND only |
|---|---|---|
| Entropy input | The 1,024 RDSEED draws (8,192 bytes) | 1,024 RDRAND draws |
| Nonce | The 1,024 RDRAND draws, when RDRAND is healthy too; otherwise 4 more RDSEED draws | 1,024 more RDRAND draws |
| Personalization string | A fixed label, the generator's role (root, or processor *n*), the time-stamp counter, and: for the root, the loader's 64 bytes when there are any; for a processor's generator, 64 bytes of the root's output | The same |

- The standard asks for 256 bits of entropy and a 128-bit nonce. 1,024 draws is two test windows, and far more than either needs. With RDRAND only it is also the number Intel's guide gives for being sure the processor's generator was reseeded in between (more than 1,022).
- **A processor's generator therefore rests on two things:** its own draws, and the root, which holds the boot processor's draws and the loader's seed. A weakness in one is covered by the other.
- A processor none of whose instructions is healthy gets no generator of its own. Its requests are answered by the root, under the root's lock, and the boot reports it.
- **A known-answer test comes first** (SP 800-90A §11.3): instantiate, reseed and generate from fixed inputs, compared with fixed answers, before the first real instantiation. A failure is treated as R8 treats a missing seed.

### R6. Reseeding

- **When.** Before a request, if the generator has answered 2^20 requests since its last seed or 60 seconds have passed. Both are first values, set where a reseed's cost (8 draws and one update) is noise. 18b measures that cost and may change them.
- **From what.** 8 draws of RDSEED, or of RDRAND where RDSEED is not healthy, through the continuous tests, with the time-stamp counter and the processor's index as additional input. That is 512 bits for the 256 the standard asks.
- **When it cannot.** With no healthy instruction the reseed is skipped and reported once, and tried again a second later. The generator goes on: it still gives backtracking resistance, and the standard allows up to 2^48 requests on one seed, which the mechanism enforces.
- **Prediction resistance.** A caller that must have output from fresh entropy (a long-lived key) asks for it. The generator reseeds from 8 RDSEED draws and then generates, as SP 800-90A §9.3.1 orders it. On a processor without healthy RDSEED the request is refused: RDRAND is a generator's output, and nothing read today says how many AMD draws span a reseed of it. No consumer needs this yet.
- **Not built, and open:** reseeding after a resume from sleep or hibernation, and when a virtual machine is restored from a snapshot or cloned. Both replay a generator's state. Neither can happen today: there is no power management and no snapshot signal. Each must be closed before the feature that makes it possible ships.

### R7. State for each processor

- **One generator for each processor, and a root.** A request is answered by the generator of the processor it runs on. No request waits on another processor's.
- **Storage** is static, one slot of 576 bytes (the state's 544, rounded to whole cache lines) for each of ADR-0008's processor indexes, so no two processors share a line. It moves with per-processor state when that is sized at discovery (ADR-0008 C6).
- **A request runs with interrupts off**, at most 128 bytes at a time. A longer request is filled in pieces, with interrupts restored between them. Pieces of one request may come from different processors' generators if the thread moves; that is harmless.
- **Interrupt handlers may ask.** NMI and machine-check handlers must not: they can interrupt a request in the middle.
- **The root** is behind a lock. It seeds the others, answers requests made before a processor has its generator, and answers for a processor with no healthy instruction. It is reseeded on R6's schedule by whichever processor next takes its lock with a healthy instruction.
- **A processor that goes offline** has its generator uninstantiated. One that comes online runs its start-up tests again.

### R8. With no healthy seed

**The root is instantiated only if the boot processor has a healthy RDSEED or a healthy RDRAND.** Otherwise, or if the known-answer test fails, there is no generator, and:

- every request returns an error. No function returns a value on failure, and none returns zero for "none";
- no consumer falls back to the instructions: R2's gate removes the possibility;
- so nothing is launched, nothing is mapped at a random address, and the network asks for no lease. Each of those already refuses without randomness today;
- the boot goes on to its verdict, which names the generator as failed.

This is the row ADR-0013 Amendment 1 accepted: "hardware entropy" becomes a Withdraw row, and what is withheld is randomness. The row, `framekernel::bootproof` and the host test that pins them change in one commit, in 18b. The boot proofs that launch a process cannot run on such a machine. They must report "did not run", which ADR-0013 R6 already has, and not fail: 18b has to make them do so.

A machine with RDRAND only is not this case. It boots, with the report R3 gives. AMD's second workaround, hiding RDSEED through CPUID, produces exactly that machine (question 3).

### R9. Overwriting secrets (§59)

A generator's state is worth every value it will give until its next seed. Old copies of it are worth more: they give values already handed out. There are three layers, because no one layer can promise the whole.

| Layer | What | The promise |
|---|---|---|
| Z1. The state | It lives only in the framework's static per-processor storage, never on a stack. Each update writes the new state over the old one. When a generator is uninstantiated the framework overwrites its storage with volatile writes | Guaranteed: the storage holds the newest state because the next request reads it, and a volatile write is never removed |
| Z2. Named temporaries | Inside `origin-crypto` every buffer that held secret material is overwritten before it goes out of use: the padded key, digests, each block's message schedule, a hasher's state | Best effort. The crate forbids `unsafe`, and Rust 1.99.1 documents its only safe barrier, `core::hint::black_box`, as best effort with no guarantee for security. In today's kernel-target object the overwrites are all there (ledger §10) |
| Z3. The stack | A request runs with interrupts off. Before they come back on, the framework overwrites the stack the generator ran on, to a fixed depth taken from the built object's frames (about 3 KiB today), and clears the scratch registers | Guaranteed for memory, whatever the compiler kept in its own slots. A boot proof searches that stack for the state's bytes after a request and must find none |

- **The loader** does all three before it enters the kernel: its generator is uninstantiated and its stack overwritten.
- **The kernel overwrites the loader's seed** in the boot-info region as soon as the root has taken it, before another processor starts.
- **What a caller is given** is the caller's to protect.
- **Why not `unsafe` in `origin-crypto`:** one volatile-write function there would make Z2 a guarantee for named buffers. It would not reach the compiler's own copies, so Z3 would be needed anyway, and the crate would stop being one the `unsafe` gate can list as a leaf (question 5).

### R10. The loader: its own generator, and a seed for the kernel

1. Before it chooses the kernel's place, the loader runs R4's start-up tests on its processor and asks the firmware's random-number protocol for 64 bytes with the firmware's default algorithm. The protocol being absent, not ready or failing is normal and is reported in one line.
2. It instantiates a generator as R5 does, with the firmware's bytes in the personalization string.
3. **It draws the kernel's slide and the direct map's slide as two requests.** Today both come from one draw.
4. Last before `ExitBootServices`, it generates 64 bytes into the boot-info region, uninstantiates its generator and overwrites its stack.
5. With no healthy instruction it has no generator: no slide, as ADR-0004 H3 already says, and no seed.

**Boot info 0.6** (a change to ADR-0002 B4, and the owner's under §84; question 2): one field is appended.

| Field | Type | Meaning |
|---|---|---|
| `boot_seed` | `u64` | The physical address of 64 bytes inside the region that holds the boot info, or 0 when the loader has none to give |

- The structure grows from 176 to 184 bytes. Major 0 still means the kernel accepts only its exact version.
- The 64 bytes are secret. The kernel checks they lie inside the boot-info region, takes them, and overwrites them. Neither side prints them.
- **Why an address and not the bytes:** `BootInfo` is `Copy` and derives `Debug`. Bytes inside it would be copied with every copy and printed with every print.
- **Why the loader's generator's output and not the firmware's bytes:** the kernel then never depends on what the firmware returned, and the loader's own draws are already in it.
- **Why 64 bytes:** one output block, and twice the generator's strength.

### R11. What the rest of the kernel sees

Three functions in `origin_frame::random`, each returning a `Result`: fill a buffer; a `u64`; fill a buffer with prediction resistance. One error type with two cases: no generator (R8), and not possible now (a refused request for prediction resistance). `launch.rs`, `virtionet.rs` and S3-5b's mapping code change to them and keep the statuses they return today.

**No interface for programs is decided here.** How a program in ring 3 gets random bytes belongs to `origin.core`'s definition (ADR-0011) and is an interface decision of its own.

### R12. Cost

Measured on the build host (an M1 Max, portable code, a loaded machine; indicative only, since the target is x86-64):

| Operation | Time |
|---|---|
| A request of up to 64 bytes | about 2.4 µs: 8 SHA-512 compressions |
| A request of 128 bytes | about 3.1 µs |
| A reseed from 64 bytes | about 4.2 µs |
| An instantiation from 8,192 bytes | about 38 µs |

- A draw today is one instruction and a round of CPUID. A request will cost more than that, and touches no hardware. The provisional budget, a hypothesis for `PERFORMANCE_BUDGETS.md` (decision item 115): **a request of up to 64 bytes in under 5 µs on a declared target.** The kernel prints the measured cost at every boot, as it does a context switch's (rule 21).
- **No buffer of output in the first build.** If S3-5b's mapping path shows the cost matters, a processor's generator may keep one request's worth of output and hand it out 8 bytes at a time. The rules that would make that sound: each piece is overwritten as it is handed out, a reseed discards the rest, and a request with prediction resistance never uses it.
- The start-up tests cost up to 2,048 draws for each processor, in parallel on the application processors. What that costs on real processors is not known and is measured in 18b.

### R13. What a boot says

One line when the root is instantiated and one for each processor: which instructions passed, how many draws, whether the loader gave a seed, and the cost. Counts and yes-or-no only: no draw, seed, state or output is ever printed (§61).

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Keep drawing from the instructions, behind one function that tests them | Smallest change | Still one hardware source with nothing mixed in, a hardware draw for every mapping, and no answer when the instruction is briefly dry. The owner chose a generator (item 113) |
| CTR_DRBG over AES-256 (SP 800-90A §10.2) | Fastest with AES instructions; what Intel's processors use themselves (ledger §7) | Origin has no AES. It would be a second primitive to write and validate for one user, and a portable AES has to be written with care for what its table lookups leak |
| Hash_DRBG over SHA-512 (§10.1.1) | Fewer hash computations for each request | The accepted decision names HMAC_DRBG, and HMAC is needed in any case (TASKS 14). The two were not compared further |
| A generator on SHAKE256 | Origin has SHAKE; simple and fast | NIST has announced one for SP 800-90A Rev. 2 and has not published it. Building it now would be inventing a construction (§4, §59) |
| A generator on ChaCha20 | Very fast | Not an SP 800-90A mechanism, and Origin has no ChaCha20 |
| One generator behind one lock | Least state, simplest seeding | Every request on every processor waits on one lock with interrupts off (§66) |
| A generator for each processor with no root | No shared state at all | A processor whose instructions fail has nothing; the loader's seed reaches only the boot processor |
| Let the firmware's seed start the generator alone | Boots a machine whose processor source is dead | It cannot be tested or reseeded, and a firmware generator of unknown quality would then stand behind every key. Question 3 |
| Refuse the boot with no healthy seed | Simplest gate | ADR-0013 Amendment 1 weighed it: it fails R1(b), since a correct kernel can meet a slow source |
| Put the loader's 64 bytes inside `BootInfo` | No pointer to check | The structure is `Copy` and `Debug` (R10) |
| Pass the firmware's bytes straight to the kernel | Nothing for the loader to compute | The loader needs a generator for its slides anyway, and the kernel would depend on firmware output |
| A volatile write inside `origin-crypto` | Named temporaries guaranteed | R9: it does not remove the need for Z3, and costs the crate its place as a leaf with no `unsafe` |
| Time the processor's jitter for a source of Origin's own | Independent of the vendor's circuit | An entropy source needs a model and an assessment (SP 800-90B). That is research, not this task. It can be added later as additional input that is never counted |

## Trade-offs

- **A request costs microseconds where a draw cost a fraction of one.** In return the hardware is touched only to seed, a stuck source is caught before its output is used, and no value depends on one instruction alone.
- **A machine with no healthy source starts no program.** That is ADR-0013 Amendment 1's accepted price (P0).
- **Each processor spends up to 2,048 draws when it starts.** It buys a start-up test of the standard's length and a seed far beyond the minimum.
- **Half a kilobyte of state for each processor,** and a stack overwrite on every request.
- **The health tests promise little,** and R4 says so. The alternative is to promise more than a consumer of conditioned output can deliver.
- **The whole design trusts the processor.** If RDSEED and RDRAND are predictable to someone and look healthy, and the firmware adds nothing independent, the generator is predictable to them. Every system that seeds from these instructions shares that. The system threat model (TASKS 19) should accept it by name.

## Migration consequences

- **`origin-crypto`** gains two public types and its first secret-handling code. Its public hashers are unchanged, and SHA-512's existing vectors still pass. **Built.**
- **A new crate,** `origin-random`, joins the workspace, the freestanding libraries' checks and the kernel's leaf crates.
- **The `unsafe` gate's lists change:** `origin-crypto` and `origin-random` become leaves the framework may use.
- **`framekernel/src/entropy.rs` is removed;** `kernel.rs`'s `prove_entropy` is replaced; `bootproof.rs`'s row, its host test, ADR-0013's inventory and `xtask`'s markers for the old line change together (ADR-0013 Amendment 1).
- **Boot info 0.6:** `origin-boot-info`, the loader, the kernel, `origin-version.toml` and the version the KRN manifest records. ADR-0002 B4 gets a paragraph for 0.6. No program sees boot info, so no user ABI changes.
- **ADR-0004 H3** is amended as R3 and R10 say.
- **Three consumers change:** `launch.rs`, `virtionet.rs`, and S3-5b's mapping code, which should land after 18b.
- **New boots for `xtask`:** a processor with RDRAND only (`-rdseed`); one with neither, which must end with the degraded verdict and run no program; and controls that plant a stuck source and a failing known-answer test.
- **Later, each with its own decision:** a random interface for programs; the TPM and a virtio random device as further inputs; reseeding on resume and on a virtual machine's generation change; SHA instructions for SHA-512.

## Threat model (§58)

| # | Who or what | What they can do | What happens |
|---|---|---|---|
| T1 | A stuck or dead instruction that reports success (AMD-SB-7055's class; an emulator's bug) | Feed constants or repeats | R4 discards zero and all ones and fails a repeat within 512 draws. The instruction is not used; with none left, R8 |
| T2 | A source that looks random and is predictable to its maker or to someone who has broken it | Know the seed | Not detected. R3's mixing helps only if the firmware's generator is independent. **Accepted:** the processor is in the trusted base |
| T3 | A hypervisor | Answer RDSEED and RDRAND itself, and supply the firmware | Out of scope: it also controls the guest's memory |
| T4 | Firmware that returns chosen or weak bytes | Choose the 64 bytes | They are concatenated with the processor's draws and never counted. Chosen bytes cannot take away what the draws hold |
| T5 | A program in ring 3 | Run RDSEED in a loop, which is not privileged, and keep it dry | Reseeds from RDSEED are delayed and fall back to RDRAND; requests for prediction resistance may be refused for a time. No value becomes predictable. Start-up happens before any program runs |
| T6 | A program in ring 3 | Read a generator's output or state | No interface gives either today (R11). Output seen later through a program interface would say nothing of the state, which is what the generator is for |
| T7 | A kernel bug that reads kernel memory | Read one processor's state | That processor's values until its next reseed, at most 60 seconds or 2^20 requests, are predictable. Earlier values are not (backtracking resistance), nor other processors'. Stale copies on stacks: R9 Z3 |
| T8 | A kernel bug that writes kernel memory | Anything | Out of scope here |
| T9 | Speculative reads of the direct map | Read the state without a bug in the usual sense | The state is ordinary kernel memory. Whatever the speculation ADR (TASKS 19) decides covers it; this ADR adds nothing |
| T10 | A restored snapshot, a cloned virtual machine, a resume from hibernation | Replay a state, so two machines give the same values | **Open** (R6). Not reachable today |
| T11 | An interrupt in the middle of a request | See or use a half-updated state | Requests run with interrupts off. NMI and machine-check handlers never ask (R7) |
| T12 | A consumer that mistakes failure for a value | Use zero as if it were random | Every function returns a `Result`. There is no value for "none" |
| T13 | Timing | Learn a secret from how long a request takes | No branch or memory index depends on a secret. SHA-512 here has no tables. Lengths and counts are public |
| T14 | Logs and diagnostics | Read what was printed | Counts and yes-or-no only (R13) |
| T15 | Memory the loader used, reused later | Find the loader's state or the seed | The loader overwrites its generator and stack; the kernel overwrites the seed (R9, R10) |
| T16 | Physical attacks, fault injection | Flip or read state | Out of scope here, as in the other threat models; the system model decides |

## What this does not decide, and what it cannot show

- **An interface for programs** (R11).
- **Which SP 800-90C construction this is.** That document was not read (ledger §6). It should be before 18b builds the root and its children.
- **Conformance or validation** of any kind (R1).
- **Anything about hardware.** Not one line of this has run on a processor. The start-up tests' cost, RDSEED's behaviour when sixty-four processors ask at once, and the cost of a request on x86-64 are all unmeasured.
- **That the overwrites survive the compiler** beyond what one build shows (R9 Z2).
- **That the mechanism is right where only the author's reference checked it:** reseeding, additional input and prediction resistance. NIST's vectors are the check (question 7).

## Questions for the owner

Each has a recommended answer. None is answered.

1. **Accept the design, R1 to R9 and R11 to R13:** one HMAC_DRBG over SHA-512; a root and a generator for each processor; the health tests of R4; seeding and reseeding as R5 and R6 give them; no generator and no randomness without a healthy seed; the three layers of overwriting; a new crate for the policy? **Recommended: yes.**
2. **Boot info 0.6** (§84): one appended field, `boot_seed`, the physical address of 64 secret bytes inside the boot-info region or 0, which the kernel overwrites once it has taken them? **Recommended: yes.** The alternative, the bytes inside the structure, puts a secret in a type that is copied and printed.
3. **What may seed the generator alone?** RDRAND with no RDSEED: yes, reported at boot, with requests for prediction resistance refused. The firmware's seed with neither instruction: no. **Recommended: as stated.** The second half amends ADR-0004 H3, which lets the loader fall back on the firmware's generator alone.
4. **Accept what "health tests on the raw source" can mean here** (§59): the tests run on the instructions' outputs, because the noise behind them cannot be reached; they catch gross failure only; and Origin claims no SP 800-90B conformance? **Recommended: yes.** If you want more, the only route is a source of Origin's own with its own assessment, which is research.
5. **Keep `unsafe` out of `origin-crypto`:** best-effort overwriting there, and the guarantee in the framework, by volatile writes over the state and an overwrite of the stack after every request (R9)? **Recommended: yes.**
6. **§17 of CLAUDE.md,** which is your text: pin SP 800-90A to Rev. 1 (June 2015) and SP 800-90B to January 2018, and add SP 800-90C (September 2025) and FIPS 198-1 (July 2008)? **Recommended: yes.**
7. **Six downloads,** asked on 2026-11-10 and needed before this ADR is accepted. To read, kept outside the repository: `NIST.SP.800-90Ar1.pdf` (2,417,007 bytes), `NIST.SP.800-90B.pdf` (1,043,696), `NIST.SP.800-90C.pdf` (10,056,754) and `NIST.FIPS.198-1.pdf` (129,454), all from nvlpubs.nist.gov, and `sp800-90b_errata_potential_updates.pdf` (107,606) from csrc.nist.gov. To convert and commit as host-only test vectors, as decision item 19 did: `hmacDRBG-1.0/internalProjection.json` (1,160,048 bytes) from github.com/usnistgov/ACVP-Server at commit `975de31`. **Recommended: yes to all six.**
