# ADR-0016: The kernel's random-number generator

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-10, as proposed, with the recommended answer to each of the seven questions below: "Accept all" to questions 1 to 6, in the session that wrote this ADR (decision sheet items 146 to 151), and "Approve all six" to the downloads of question 7, earlier the same day (item 145). **Built** (tasks 18a and 18b, 2026-11-10 and 2026-11-11): the mechanism in `origin-crypto`; the policy in `origin-random`; the kernel's service with a generator on every processor; the loader's own generator, its two slides and boot info 0.6; the consumers on `main`; the Withdraw row; the gate on the two instructions. **Not built:** the loader's call to the firmware's random-number protocol (R10 step 1), the consumers that are not yet on `main`, and what the section "What was built" lists. **It has run under QEMU only**, on its software processor: nothing here has run on a real processor, and no figure it prints claims anything. **No human has reviewed it.** |
| Decides | Decision item 113 and finding 8 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`; TASKS item 18. It carries out ADR-0013 Amendment 1 and the note of 2026-11-10 in ADR-0004 H3 |
| Amendments | Amendment 1 (2026-11-11, decision item 166): the loader also hands over the value the kernel stacks' region is placed from; the secret area is 72 bytes. At the end of this document |
| Changes accepted decisions | Three, each named where it happens and each recorded in the ADR it changes: ADR-0002 B4 (boot info 0.6, R10; item 147); ADR-0004 H3 (the firmware's generator is an addition and never a source of its own; two slides, two draws; R3, R10; items 147 and 148); ADR-0013's inventory (one row, which Amendment 1 already accepted; R8) |
| Requires | CLAUDE.md P0, P1, §3, §4, §13, §14, §16 to §19, §24, §58, §59, §61, §72, §84; rules 14, 16, 17, 21, 35 |
| Code | `security/crypto/src/hmac.rs`, `hmac_drbg.rs`, `wipe.rs`; `security/random/` (`origin-random`); `framekernel/frame/src/random.rs`; `boot/uefi/src/random.rs` and `main.rs`; `boot/info`; `developer-tools/xtask/src/mnemonics.rs` |
| Evidence | `docs/research/2026-11-10-kernel-random-numbers.md` (the ledger, cited below by its row numbers A, B and C); `docs/engineering/SOURCE_PROVENANCE.md`, "HMAC-SHA-512 and HMAC_DRBG" |
| Related | ADR-0001 D3, ADR-0002 B4, ADR-0004 H3, ADR-0008, ADR-0009 Amendment 1, ADR-0011 N6, ADR-0012, ADR-0013 Amendment 1 |

**What stands behind this.** SP 800-90A Rev. 1, SP 800-90B and SP 800-90C were read in the parts that bear on it (ledger §2). The built mechanism was compared with the standard's text step by step, and it gives NIST's own answers on NIST's test vectors for it. R3 to R13 are built since task 18b: their policy is tested on the build host against an independent reference and planted faults, and the whole has booted under QEMU. "What was built", near the end, says how each point was built and what was left.

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

| Document | Revision | Its part here |
|---|---|---|
| SP 800-90A | Rev. 1, June 2015. A Rev. 2 is announced and has no draft yet | The mechanism and how it is used |
| SP 800-90B | January 2018. Its two announced errata do not touch health tests | The two health tests |
| SP 800-90C | September 2025 | How generators and sources are put together: the tree of R7 |
| FIPS 198-1 | July 2008. NIST plans to move it to SP 800-224, still a draft | HMAC |

**Origin claims no conformance to any of them and no validation.** It uses their algorithms and follows their rules where it can. The standards build on entropy sources that a laboratory has validated (ledger C2), and Origin validates nothing. R4 and R7 name each place where the design differs from them.

### R2. Where the code lives

| Layer | Where | What it holds | `unsafe` |
|---|---|---|---|
| The mechanism | `security/crypto` (`origin-crypto`), **built** | HMAC-SHA-512; `HmacDrbgSha512`: instantiate, reseed, generate, a request with prediction resistance, uninstantiate | Forbidden |
| The policy | `security/random` (`origin-random`), a new crate | The health tests; how draws are retried; how a seed is put together; when a generator is reseeded. Written over two small traits, a source of draws and a clock, so all of it is tested on the host | Forbidden |
| The kernel's service | `framekernel/frame/src/random.rs` | The two instructions; the root and a generator for each processor; overwriting; the boot wiring. The only way the rest of the kernel gets a random value | Yes: the framework is where the kernel's `unsafe` lives (ADR-0011 N6) |
| The loader | `boot/uefi` | The two instructions; the firmware's protocol; the slides; the seed it hands on | As the loader has today |

- `framekernel/src/entropy.rs` goes. Its retry logic moves into the policy crate.
- The kernel links `origin-crypto` for the first time. The crate joins the leaf crates the `unsafe` gate lists, which it can because it forbids `unsafe`.
- **A gate** in `cargo xtask check`: the mnemonics `rdrand` and `rdseed` appear only in the framework's file and the loader's. That is what makes "nothing else draws from the processor's instructions directly" (§59) checkable.

### R3. The sources, and what each is trusted for

SP 800-90C lets several sources' outputs be concatenated into one seed and gives two ways to count the entropy. Its Method 1 counts the physical sources and lets anything else be mixed in uncounted (ledger C2). That is the rule here.

| Source | Role |
|---|---|
| **RDSEED**, 64-bit form | The entropy a seed is counted on. Intel's guide describes its output as conditioned entropy, provided for software to seed its own generators with |
| **RDRAND**, 64-bit form | Always mixed in. Counted on only when RDSEED is absent or unhealthy, and then the boot says the generator's strength is no more than the processor's own generator's, which is the standard's rule for a root seeded from another generator (ledger C6) |
| **The loader's seed**, 64 bytes | Always mixed in when present. **Never counted, and never enough by itself** |

Three things about these sources have to be said plainly.

- **They are not three independent sources.** By Intel's guide, RDSEED and RDRAND are two outputs of one circuit, which a package's cores share. AMD's design was not read. The firmware's generator may draw from the same circuit: the test firmware's most likely does (ledger §8). "Seeded from more than one source" (§59) is true of the interfaces. It becomes true of the entropy only where the firmware draws from something else.
- **None is validated by Origin.** Whether a given processor's RDSEED holds an entropy-source validation was not checked, and the kernel cannot check it. The firmware's seed cannot even be tested: it is one sample from a generator the kernel never sees.
- **Only the 64-bit forms are used.** AMD-SB-7055 is about the shorter ones.

This changes ADR-0004 H3, which lists `EFI_RNG_PROTOCOL` as a source the loader may fall back on alone. Here it never stands alone (question 3).

### R4. Health tests on the raw draws

On each processor, for each instruction the processor reports:

1. **Not ready is not a value.** The carry flag clear means no value: retry, RDRAND up to 10 times (Intel's figure) and RDSEED up to 100 times with a PAUSE between tries. A draw whose every try was not ready is "not now", not a failure.
2. **Zero and all ones are not values.** A draw of 0 or of 2^64 − 1 with the carry flag set is discarded and retried. For zero this is AMD's own workaround, and Intel's table describes a successful value as non-zero. All ones is added as a second stuck pattern, on no source read today. A draw whose every try gave one of the two is a failure.
3. **Repetition count test** (SP 800-90B §4.4.1), cutoff 2: two equal draws in a row fail.
4. **Adaptive proportion test** (§4.4.2), a window of 512 draws, cutoff 2: the window's first draw coming again fails.
5. **Stuck-bit test**, Origin's own: within each window of 512 draws, every one of the 64 bit positions must take both values. A healthy source fails it about once in 2^505 windows. It catches what the two tests above cannot see on whole values: half of every draw stuck, which is what a shorter form of the instruction would produce.
6. **Start-up:** 1,024 consecutive draws that pass all of the above, before any draw of that instruction is used (SP 800-90B §4.3 item 4). They are then used, as the standard allows: R5.
7. **Continuous:** every later draw goes through tests 1 to 5.
8. **A failure** makes that instruction unhealthy on that processor and is reported. The draws in hand are discarded. The instruction is not drawn from again until it passes a whole new start-up test, which is tried when its processor's generator is next due a reseed. This is SP 800-90C's rule for a failed source (ledger C3), as far as software can follow it: the kernel cannot know that a cause was corrected, only that the test passes again.

**The cutoffs.** SP 800-90B computes them from a source's assessed entropy and an accepted false-alarm rate, 2^-20. Worked exactly (ledger §5, which reproduces the standard's own table): for a 64-bit draw assumed to hold anywhere from 20 to 28 bits, both cutoffs are 2. At the entropy the vendors claim, the adaptive proportion formula gives a cutoff of 1, which every window meets at its first draw. So 2 is the tightest cutoff the standard's formulas give that means anything. A healthy source trips it about once in 2^55 windows.

**What these tests are not.** RDSEED and RDRAND are not noise sources. They return a conditioner's and a generator's output, and the noise behind them cannot be reached. The tests SP 800-90B requires are on raw noise, before conditioning, and for these instructions they run inside the processor. What the kernel runs are the further tests the standard allows on conditioned output (ledger B1). So this is not SP 800-90B health testing, and Origin must not call it that. The tests catch gross failure, which is the standard's own description of what continuous tests catch: a constant, a repeat, a handful of values, a stuck bit. They cannot catch a source that looks random and is predictable. Against that there is only the mixing in R3, which helps only as far as the sources differ (question 4).

### R5. Seeding

The shape is SP 800-90C §7's tree of generators: a root seeded from the sources, and a child seeded from the root (ledger C5 to C7). **No draw goes into two seeds** (SP 800-90A §8.6.9).

| Parameter | The root, on the boot processor | A processor's generator |
|---|---|---|
| Entropy input (counted) | The 1,024 start-up draws of RDSEED (8,192 bytes); of RDRAND where RDSEED is not healthy | 64 bytes of the root's output |
| Nonce | The 1,024 start-up draws of the other instruction, when it is healthy too; otherwise 4 further draws | 16 further bytes of the root's output |
| Personalization string (not counted) | A fixed label, the time-stamp counter, and the loader's 64 bytes when there are any | A fixed label, the processor's index, the time-stamp counter, and 1,024 draws of its own from each instruction that is healthy on it |

- **The standard's amounts are far smaller:** 384 bits to instantiate an HMAC_DRBG at 256 bits, from the source for the root and from the parent for a child. The root takes every draw its start-up tests accepted, because the draws are already made and more entropy than the minimum is the safe side to err on. With RDRAND only, 1,024 is also the number Intel's guide gives for being sure the processor's generator was reseeded in between (more than 1,022).
- **A processor's generator rests on two things:** the root, which is what the standard counts, and its own processor's draws, which go in as the extra input the standard allows and does not count. A weakness in one is covered by the other. The boot processor's own generator is a child like any other and draws again for it.
- **Every processor gets a generator,** also one none of whose instructions is healthy: the root seeds it, and the boot reports it.
- **A known-answer test comes first** (SP 800-90A §11.3): instantiate, reseed and generate from fixed inputs, compared with fixed answers, before the first real instantiation. NIST's vectors are to be its answers.

### R6. Reseeding

- **When.** Before a request, if the generator has answered 2^20 requests since its last seed or 60 seconds have passed. SP 800-90C leaves the criteria to the implementer and names time and a count of outputs (§7.2.3). Both figures are first values, set where a reseed's cost (8 draws and one update) is noise. 18b measures that cost and may change them.
- **A processor's generator** reseeds from 8 draws of its own RDSEED, or of its RDRAND where RDSEED is not healthy, through the continuous tests, with the time-stamp counter and the processor's index as additional input. That is 512 bits for the 256 the standard asks. With no healthy instruction of its own it reseeds from 64 bytes of the root's output. With no root to ask either, it goes on without reseeding, which SP 800-90C allows a child (ledger C8), up to SP 800-90A's limit of 2^48 requests, which the mechanism enforces. The kernel reports it.
- **The root** is reseeded on the same schedule, from 8 draws, by whichever processor next uses it and has a healthy instruction.
- **When every instruction on every processor has failed,** the root stops: it seeds nothing and answers nothing until an instruction passes a start-up test again. That is the standard's rule for a generator all of whose sources have failed (ledger C3). The processors' generators go on with the seeds they have, as above (question 3).
- **Prediction resistance.** A caller that must have output from fresh entropy (a long-lived key) asks for it. The request goes to the root, which SP 800-90C says is the one generator of a tree that can give it: the root is reseeded from 8 RDSEED draws of the asking processor and then generates, in the order SP 800-90A §9.3.1 gives. On a processor without healthy RDSEED the request is refused: RDRAND is a generator's output, and nothing read says how many AMD draws span a reseed of it. No consumer needs this yet.
- **Not built, and open:** reseeding after a resume from sleep or hibernation, and when a virtual machine is restored from a snapshot or cloned. Both replay a generator's state, which SP 800-90C rules out by name (ledger C10). Neither can happen today: there is no power management and no snapshot signal. Each must be closed before the feature that makes it possible ships.

### R7. State for each processor

- **A root, and one generator for each processor.** SP 800-90C draws exactly this as its example of a tree in an operating system (Appendix A.3.4). A request is answered by the generator of the processor it runs on. No ordinary request waits on a lock another processor holds.
- **Two differences from the standard's tree,** both for the same reason, that a processor should not have to reach the root's lock to stay well seeded. A child's own draws go into its first seed, uncounted. And a child reseeds from its own instruction first, where the standard has the parent first and the source as the alternative; its Appendix gives the cost of reaching a parent on another processor as a reason for alternatives.
- **Storage** is static, one slot of 576 bytes (the state's 544, rounded to whole cache lines) for each of ADR-0008's processor indexes, so no two processors share a line. It moves with per-processor state when that is sized at discovery (ADR-0008 C6).
- **A request runs with interrupts off**, at most 128 bytes at a time. A longer request is filled in pieces, with interrupts restored between them. Pieces of one request may come from different processors' generators if the thread moves; that is harmless.
- **Interrupt handlers may ask.** NMI and machine-check handlers must not: they can interrupt a request in the middle.
- **The root** is behind a lock. It seeds and reseeds the others and answers requests with prediction resistance, and nothing else.
- **A processor that goes offline** has its generator uninstantiated. One that comes online runs its start-up tests again and is seeded anew.

### R8. With no healthy seed

**The root is instantiated only if the boot processor has a healthy RDSEED or a healthy RDRAND.** Otherwise, or if the known-answer test fails, there is no generator, and:

- every request returns an error. No function returns a value on failure, and none returns zero for "none";
- no consumer falls back to the instructions: R2's gate removes the possibility;
- so nothing is launched, nothing is mapped at a random address, and the network asks for no lease. Each of those already refuses without randomness today;
- the boot goes on to its verdict, which names the generator as failed.

This is the row ADR-0013 Amendment 1 accepted: "hardware entropy" becomes a Withdraw row, and what is withheld is randomness. The row, `framekernel::bootproof` and the host test that pins them change in one commit, in 18b. The boot proofs that launch a process cannot run on such a machine. They must report "did not run", which ADR-0013 R6 already has, and not fail: 18b has to make them do so.

Both standards end the same way: a generator that fails its own test is terminated, and gives nothing until it is instantiated again after someone has intervened (SP 800-90A §11.4.2; ledger C9). Here that means until the next boot.

A machine with RDRAND only is not this case. It boots, with the report R3 gives. AMD's second workaround, hiding RDSEED through CPUID, produces exactly that machine (question 3).

### R9. Overwriting secrets (§59)

A generator's state is worth every value it will give until its next seed. Old copies of it are worth more: they give values already handed out. SP 800-90A requires that uninstantiation erase the state and that a test show it (§9.4, §11.2). There are three layers, because no one layer can promise the whole.

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
2. It instantiates a generator as R5 instantiates a root, with the firmware's bytes in the personalization string.
3. **It draws the kernel's slide and the direct map's slide as two requests.** Today both come from one draw. *(Amendment 1 adds a third value, for the kernel stacks' region, handed over beside the seed.)*
4. Last before `ExitBootServices`, it generates 64 bytes into the boot-info region, uninstantiates its generator and overwrites its stack.
5. With no healthy instruction it has no generator: no slide, as ADR-0004 H3 already says, and no seed.

The loader's generator and the kernel's root are two roots, one after the other on one machine. The 64 bytes are not seed material in the standard's sense, which would make the kernel's root a child: they are extra input, which may be anything (ledger C5).

**Boot info 0.6** (a change to ADR-0002 B4, and the owner's under §84; question 2): one field is appended.

| Field | Type | Meaning |
|---|---|---|
| `boot_seed` | `u64` | The physical address of 64 bytes inside the region that holds the boot info, or 0 when the loader has none to give |

- The structure grows from 176 to 184 bytes. Major 0 still means the kernel accepts only its exact version.
- *(Amendment 1: the area `boot_seed` names is 72 bytes, these 64 and then 8 for the kernel stacks' region.)*
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
- The start-up tests cost up to 2,048 draws for each processor, in parallel on the application processors, and the boot processor draws as many again for its own generator. What that costs on real processors is not known and is measured in 18b.

### R13. What a boot says

One line when the root is instantiated and one for each processor: which instructions passed, how many draws, whether the loader gave a seed, and the cost. Counts and yes-or-no only: no draw, seed, state or output is ever printed (§61).

## What was built (tasks 18a and 18b), and how

The contract of task 18b in `docs/engineering/TASKS.md` has the detail, as S1 to S7. Each point below is something the decisions above left open or that building changed in the telling. **None changes a decision.**

| Decision | As built |
|---|---|
| R2: where the code lives | As decided. The gate (`developer-tools/xtask/src/mnemonics.rs`) also holds the instructions to their 64-bit forms (R3) and refuses the compiler's intrinsics for them |
| R4: the health tests | As decided, in `origin-random`. A draw whose tries were a mixture of "not ready" and stuck values (zero or all ones) is held to be a failure, which R4 does not say: a working instruction gives such a value about once in 2^63 tries. The tests remember two draws (the one before, and a window's first), in the processor's static storage. While a processor starts, a draw that ended "not now" is made again, up to eight times, before the instruction is given up for now: a start has nothing else to do, and that is when the instructions are most likely to give nothing. Before this (the review of 2026-11-11) one "not now" cost an instruction its start-up test, and among a lone instruction's four nonce draws it cost the machine its generator |
| R5: seeding | As decided, byte for byte: a host test compares nine outputs with a Python reference written from R5's table. A second start of a root or of a processor's generator changes nothing. Each region of the seed area says whose draws it holds, and a first seed empties a region whose instruction has failed since they were collected (R4 item 8); the other instruction's draws then take the first place, so the personalization string stays one run of bytes |
| R6: reseeding | As decided, in steps (S1). A reseed's eight draws are gathered before the request's first piece, each try in a step of its own. When RDSEED was "not now" for one draw, it rests for one second and the reseed is made at once from RDRAND, with the RDSEED draws already gathered as additional input (S3). A request makes at most 16 draws (S2). A request that finds a processor due always does one piece of its maintenance in the same step, so a thread that moves between processors cannot starve one. **The root's schedule** is kept by the processors: one that has just reseeded itself looks at the root, without waiting for its lock, and if the root is due reseeds it from eight more draws. That is how "whichever processor next uses it" is built; before the review of 2026-11-11 a processor used the root only while it started, so the root kept its first seed. If the lock is taken, the draws cannot be had or the request has no draws left, the processor looks again at a later request, a second on; draws that do not reach the root are overwritten, and the root's gathering never takes a processor's own. **Prediction resistance:** the request overwrites the processor's store when it begins there and is given a number; the root takes a store only from the request that holds its number; a request that moves begins again where it arrives, and one that ends unanswered overwrites what it gathered. So every one of the eight draws is made inside the request. **Waits** (an instruction's rest, the period before a new start-up test, the reseed period) are measured from when they began, and a clock that reads earlier than that ends the wait |
| R7: state for each processor | As decided: static, one slot for each processor index, aligned so that no two share a cache line. A slot is larger than the 576 bytes R7 gives for the state alone, because the tests' values and the store are beside it. No processor goes offline today, so nothing uninstantiates a generator while the kernel runs. A thread that moves between steps: the root reseeds only a processor that needs it, not whichever one the thread is on by then; a draw's tries are counted across a move, and a request that has given an instruction up does not try it again on another processor; a stuck value seen on one processor is not held against another's instruction |
| R8: no healthy seed | As decided, and one step further: when the boot's proof of the generator fails for any reason, `random::withhold()` refuses every later request, so the Withdraw row withholds what it says (ADR-0013 R2). "Runs no program" (Migration) is built as "launches no process": the kernel's fixed ring-3 proof programs ask for no random value and still run. The proof's request with prediction resistance may be answered "not now" where RDSEED is healthy: it is made again after the instruction's rest, three times in all, and only a wrong answer fails the row (R4 item 1; ADR-0013 R1(b); control C43) |
| R9: overwriting | Z1 and Z2 as decided. Z3 is the whole of a stack that belongs to the generator, one for each processor, 8 KiB with a guard page, in place of "a fixed depth" of the caller's stack (S5): every step runs on it with interrupts off, it is overwritten before they come back on, and the scratch registers are cleared. The boot proof paints that stack, measures how deep a request goes (3,024 bytes in the first build) and checks that only zeros are on it after a request |
| R10: the loader | Steps 2 to 5 as decided, and Amendment 1's third request, with the same step: interrupts off, a stack of the generator's own, registers cleared. **Step 1's call to the firmware's random-number protocol is not built**: the session that built this had read EDK II's declaration of the protocol (ledger §12), and decision item 92 has that code written by a session that has not. The loader says so in its line. Boot info 0.6 as decided; the kernel also requires the 64 bytes to lie clear of the boot-info header and its two arrays |
| R11: what the kernel sees | `origin_frame::random::{fill, next_u64, fill_fresh}` and one error type with the two cases. A request that arrives in the middle of a step on the same processor is refused with "not now" |
| R12: cost | Printed at every boot: a 64-byte request, and the longest step with interrupts off, while a processor started and since. Under QEMU's software processor these were about 22 µs, 0.8 ms and 0.2 ms: an emulator's figures, which claim nothing. The budgets stay hypotheses |
| R13: what a boot says | One line for the root, one for each processor, three for the proof. Counts and yes-or-no only |

**Left to other tasks:** the firmware's protocol (above); two draws on `land/s3-part2` (read at `e9eae13` on 2026-11-11). `framekernel/src/memory_object.rs` places a mapping through `entropy::hardware_u64`, which becomes `origin_frame::random::next_u64`. `framekernel/frame/src/stack.rs` draws the stack region's base through `cpu::rdseed` and `cpu::rdrand`, and that is not a change of one call: `early.rs` draws it where the kernel's tables are made, before the heap and the clock, so before the kernel's generator exists. The owner decided how on 2026-11-11 (decision item 166; ADR-0016 Amendment 1): the loader's generator makes a third 8-byte request and hands the value over directly after its seed, and `draw_region` takes it through `origin_frame::random::take_stack_value`, which this branch provides and proves. `framekernel/src/stacks.rs` does not draw: its proof reads which instruction the framework drew from, and changes with it; neither compiles against this tree as it stands; reseeding on resume and on a restored snapshot (R6, T10); an interface for programs (R11); everything on hardware.

## Evidence

Only R1's mechanism is built, so only it has evidence. The detail is in `SOURCE_PROVENANCE.md`.

| What | Evidence |
|---|---|
| HMAC-SHA-512 | RFC 4231's seven test cases; 273 lengths on which Python's `hmac` and RFC 2104's formula agree; every key length to 260 against the formula |
| The generator against published values | The six SHA-512 values of RFC 6979 appendix A.2.5 to A.2.7: instantiation, and a request of one output block and of two |
| The generator against the standard | Read beside SP 800-90A §9 and §10.1.2 after it was written: every step and limit agreed, and the order of two checks was changed to the standard's |
| Reseeding, additional input, prediction resistance | NIST's ACVP vectors for HMAC_DRBG over SHA-512: all 30, 15 with prediction resistance and 15 with a reseed. Also a corpus of 145 instantiations from an independent Python implementation by the same author, which the HMAC-DRBG of an installed OpenSSL 4.1.0 reproduces |
| The tests themselves | 53 faults planted one at a time: 51 fail a test, and the other 2 leave a stack buffer un-overwritten, which safe code cannot observe |
| No panic path, no `unsafe` | The kernel-target release object references `memcpy`, `memset` and one formatter function; the crate forbids `unsafe` |
| Overwriting (R9 Z2) | The same object holds the overwrite calls. One build, not a promise |
| Cost (R12) | Measured on the build host, not on the target |
| R3 to R8, the policy | `origin-random`: 72 host tests (55 before the review of 2026-11-11). Each health test on hand-made sequences, and the cutoffs against SP 800-90B's formulas, which give the standard's own table; seeds byte for byte against an independent Python reference; the schedule, the back-off and the 16-draw bound with a scripted source and clock; a thread that moves between steps. 107 faults are planted one at a time: 105 fail a test, and two are listed as not observable (a local variable's bytes; the root told of a change twice). Planting them found one real fault, since fixed: a draw's failed tries were counted across draws. An independent read of the crate at `cef9342` then found eight things, all folded in (task 18b's record in `TASKS.md`) |
| R2's gate | Eight tests of its own; on the tree, the instructions are run 4 times in the 2 files listed |
| R9 to R13, the machine | Boots under QEMU 11.1.3 with OVMF, software processor: the standard boot, a processor with RDRAND only, a processor with neither instruction, and the controls of ADR-0013, C37 to C43. **Nothing on hardware** |

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Keep drawing from the instructions, behind one function that tests them | Smallest change | Still one hardware source with nothing mixed in, a hardware draw for every mapping, and no answer when the instruction is briefly dry. The owner chose a generator (item 113) |
| CTR_DRBG over AES-256 (SP 800-90A §10.2) | Fastest with AES instructions; what Intel's processors use themselves (ledger §7) | Origin has no AES. It would be a second primitive to write and validate for one user, and a portable AES has to be written with care for what its table lookups leak |
| Hash_DRBG over SHA-512 (§10.1.1) | Fewer hash computations for each request | The accepted decision names HMAC_DRBG, and HMAC is needed in any case (TASKS 14). The two were not compared further |
| A generator on SHAKE256 | Origin has SHAKE; simple and fast | NIST has announced one for SP 800-90A Rev. 2 and has not published it. Building it now would be inventing a construction (§4, §59) |
| A generator on ChaCha20 | Very fast | Not an SP 800-90A mechanism, and Origin has no ChaCha20 |
| One generator behind one lock | Least state, simplest seeding | Every request on every processor waits on one lock with interrupts off (§66) |
| A generator for each processor with no root | No shared state at all | Nothing to seed a processor whose instructions fail; the loader's seed reaches only the boot processor; and it is not the standard's tree |
| The standard's tree exactly: a child seeded and reseeded by the root alone | Nothing to explain | Every reseed on every processor takes the root's lock, and a child holds nothing but what the root gave it |
| Let the firmware's seed start the generator alone | Boots a machine whose processor source is dead | It cannot be tested or reseeded, and a firmware generator of unknown quality would then stand behind every key. Question 3 |
| Refuse the boot with no healthy seed | Simplest gate | ADR-0013 Amendment 1 weighed it: it fails R1(b), since a correct kernel can meet a slow source |
| Stop every generator when every source has failed at run time | The strictest reading of the standards | A machine that was well seeded stops launching programs, though each generator still gives backtracking resistance and SP 800-90C allows a child to go on. Question 3 |
| Put the loader's 64 bytes inside `BootInfo` | No pointer to check | The structure is `Copy` and `Debug` (R10) |
| Pass the firmware's bytes straight to the kernel | Nothing for the loader to compute | The loader needs a generator for its slides anyway, and the kernel would depend on firmware output |
| A volatile write inside `origin-crypto` | Named temporaries guaranteed | R9: it does not remove the need for Z3, and costs the crate its place as a leaf with no `unsafe` |
| Run the adaptive proportion test on a reduced alphabet, as SP 800-90B §6.4 allows | The formula then gives a cutoff for the vendors' own entropy claim | §6.4 was not read. R4's stuck-bit test covers the case a whole-value test misses. To be weighed in 18b |
| Time the processor's jitter for a source of Origin's own | Independent of the vendor's circuit | An entropy source needs a model and an assessment (SP 800-90B). That is research, not this task. It can be added later as additional input that is never counted |

## Trade-offs

- **A request costs microseconds where a draw cost a fraction of one.** In return the hardware is touched only to seed, a stuck source is caught before its output is used, and no value depends on one instruction alone.
- **A machine with no healthy source starts no program.** That is ADR-0013 Amendment 1's accepted price (P0).
- **Each processor spends up to 2,048 draws when it starts,** and the boot processor twice that. It buys a start-up test of the standard's length and a seed far beyond the minimum.
- **Half a kilobyte of state for each processor,** and a stack overwrite on every request.
- **The health tests promise little,** and R4 says so. The alternative is to promise more than a consumer of conditioned output can deliver.
- **Two departures from the standard's tree** (R7) buy requests and reseeds that take no shared lock. The price is that the design cannot be described as the standard's without a footnote.
- **The whole design trusts the processor.** If RDSEED and RDRAND are predictable to someone and look healthy, and the firmware adds nothing independent, the generator is predictable to them. Every system that seeds from these instructions shares that. The system threat model (TASKS 19) treats firmware and microcode as trusted; this is the same acceptance.

## Migration consequences

- **`origin-crypto`** gains two public types and its first secret-handling code. Its public hashers are unchanged, and SHA-512's existing vectors still pass. **Built.**
- **A new crate,** `origin-random`, joins the workspace, the freestanding libraries' checks and the kernel's leaf crates.
- **The `unsafe` gate's lists change:** `origin-crypto` and `origin-random` become leaves the framework may use, and `framekernel/frame/src/random.rs` joins the files that hold `unsafe` (TASKS 17's gate).
- **`framekernel/src/entropy.rs` is removed;** `kernel.rs`'s `prove_entropy` is replaced; `bootproof.rs`'s row, its host test, ADR-0013's inventory and `xtask`'s markers for the old line change together (ADR-0013 Amendment 1).
- **Boot info 0.6:** `origin-boot-info`, the loader, the kernel, `origin-version.toml` and the version the KRN manifest records. ADR-0002 B4 gets a paragraph for 0.6. No program sees boot info, so no user ABI changes.
- **ADR-0004 H3** is amended as R3 and R10 say.
- **Three consumers change:** `launch.rs`, `virtionet.rs`, and S3-5b's mapping code, which should land after 18b.
- **New boots for `xtask`:** a processor with RDRAND only (`-rdseed`); one with neither, which must end with the degraded verdict and run no program; and controls that plant a stuck source, a stuck bit and a failing known-answer test.
- **Later, each with its own decision:** a random interface for programs; the TPM and a virtio random device as further inputs; reseeding on resume and on a virtual machine's generation change; SHA instructions for SHA-512.

## Threat model (§58)

| # | Who or what | What they can do | What happens |
|---|---|---|---|
| T1 | A stuck or dead instruction that reports success (AMD-SB-7055's class; an emulator's bug) | Feed constants, repeats or half-constant values | R4 discards zero and all ones, fails a repeat within 512 draws and fails a bit that never changes. The instruction is not used; with none left on the boot processor, R8 |
| T2 | A source that looks random and is predictable to its maker or to someone who has broken it | Know the seed | Not detected. R3's mixing helps only if the firmware's generator is independent. **Accepted:** the processor is in the trusted base |
| T3 | A hypervisor | Answer RDSEED and RDRAND itself, and supply the firmware | Out of scope: it also controls the guest's memory |
| T4 | Firmware that returns chosen or weak bytes | Choose the 64 bytes | They are concatenated with the processor's draws and never counted. SP 800-90A says of such input that an adversary's knowing it costs nothing while the entropy input is unknown (§8.7.1) |
| T5 | A program in ring 3 | Run RDSEED in a loop, which is not privileged, and keep it dry | Reseeds from RDSEED are delayed and fall back to RDRAND; requests for prediction resistance may be refused for a time. No value becomes predictable. Start-up happens before any program runs |
| T6 | A program in ring 3 | Read a generator's output or state | No interface gives either today (R11). Output seen later through a program interface would say nothing of the state, which is what the generator is for |
| T7 | A kernel bug that reads kernel memory | Read one processor's state | That processor's values until its next reseed, at most 60 seconds or 2^20 requests, are predictable. Earlier values are not (backtracking resistance), nor other processors'. Stale copies on stacks: R9 Z3 |
| T8 | A kernel bug that writes kernel memory | Anything | Out of scope here |
| T9 | Speculative reads of the direct map | Read the state without a bug in the usual sense | The state is ordinary kernel memory. Whatever ADR-0017 (TASKS 19) decides covers it; this ADR adds nothing |
| T10 | A restored snapshot, a cloned virtual machine, a resume from hibernation | Replay a state, so two machines give the same values | **Open** (R6). Not reachable today |
| T11 | An interrupt in the middle of a request | See or use a half-updated state | Requests run with interrupts off. NMI and machine-check handlers never ask (R7) |
| T12 | A consumer that mistakes failure for a value | Use zero as if it were random | Every function returns a `Result`. There is no value for "none" |
| T13 | Timing | Learn a secret from how long a request takes | No branch or memory index depends on a secret. SHA-512 here has no tables. Lengths and counts are public |
| T14 | Logs and diagnostics | Read what was printed | Counts and yes-or-no only (R13) |
| T15 | Memory the loader used, reused later | Find the loader's state or the seed | The loader overwrites its generator and stack; the kernel overwrites the seed (R9, R10) |
| T16 | Physical attacks, fault injection | Flip or read state | Out of scope here, as in the other threat models; the system model decides |

## What this does not decide, and what it cannot show

- **An interface for programs** (R11).
- **Conformance or validation** of any kind (R1). The sources are not validated, and two rules of the standard's tree are departed from (R7).
- **Anything about hardware.** Not one line of this has run on a processor. The start-up tests' cost, RDSEED's behaviour when sixty-four processors ask at once, and the cost of a request on x86-64 are all unmeasured.
- **That the overwrites survive the compiler** beyond what one build shows (R9 Z2).
- **What a non-maskable interrupt leaves.** An NMI or a machine check that arrives in the middle of a step saves the interrupted registers on its own stack, which is not overwritten. Neither handler asks for a random value (R7). T11 does not say this; task 18b's contract names it (S7), and it is not closed.
- **A processor whose only healthy instruction is kept dry.** Where RDRAND is not healthy on a processor and a program keeps RDSEED dry (T5), that processor's reseed waits for as long as RDSEED gives nothing; it tries one draw a second. T5's row says reseeds "are delayed and fall back to RDRAND" and does not say this. On a machine whose processors are alike the root cannot help: its own reseed needs the same instruction. The independent review of 2026-11-11 found it (its finding 5); it is R6 as accepted, and it is put to the owner in task 18b's record.
- **The root's schedule depends on requests.** A reseed happens before a request (R6). On a machine where no processor with a healthy instruction answers a request, nothing reseeds the root, and a processor that is fed by the root gets output of a seed older than 60 seconds.
- **Whether eight is enough.** How often RDSEED gives nothing while sixty-four processors start is unmeasured. A start waits out eight draws' worth of "not now" for each instruction; past that it goes on with the other instruction or, with none, with no generator, as before.
- **Where a reseed's draws run, and what backs off when RDSEED is dry.** The coordinating session's review found these open after acceptance (2026-11-10). Task 18b's contract settled them as S1 to S7 without changing R4, R6 or R7, and they are built: "What was built", below.
- **More of the mechanism than its vectors reach.** NIST's 30 vectors for SHA-512 all use one shape of input (ledger §9). Other lengths, and a request of no bytes, are checked against the author's reference, OpenSSL's generator and the standard's text.

## Questions for the owner

Each had a recommended answer. **All seven are answered (2026-11-10), each as recommended.** The owner answered "Accept all" to questions 1 to 6 together, and had answered question 7 earlier the same day. The decision sheet has them as items 146 to 151 and 145.

1. **Accept the design, R1 to R9 and R11 to R13:** one HMAC_DRBG over SHA-512; a root and a generator for each processor, in the shape of SP 800-90C's tree with the two differences R7 names; the health tests of R4; seeding and reseeding as R5 and R6 give them; no generator and no randomness without a healthy seed; the three layers of overwriting; a new crate for the policy? **Recommended: yes.** **Yes (2026-11-10; item 146).**
2. **Boot info 0.6** (§84): one appended field, `boot_seed`, the physical address of 64 secret bytes inside the boot-info region or 0, which the kernel overwrites once it has taken them? **Recommended: yes.** The alternative, the bytes inside the structure, puts a secret in a type that is copied and printed. **Yes (2026-11-10; item 147).**
3. **What may seed, and what happens when the sources fail?** Three parts. RDRAND with no RDSEED: the generator runs, the boot says so, and requests for prediction resistance are refused. The firmware's seed with neither instruction: no generator. Every instruction failing after boot: the root stops, and the processors' generators go on with the seeds they have. **Recommended: as stated.** The second part amends ADR-0004 H3, which lets the loader fall back on the firmware's generator alone. The stricter alternative to the third is to stop giving random values altogether until an instruction passes its tests again. **As stated (2026-11-10; item 148).**
4. **Accept what "health tests on the raw source" can mean here** (§59): the tests run on the instructions' outputs, because the noise behind them cannot be reached; they catch gross failure only; and Origin claims no SP 800-90B conformance? **Recommended: yes.** If you want more, the only route is a source of Origin's own with its own assessment, which is research. **Yes (2026-11-10; item 149).** Draft 7.4's §59 was reworded to say so, in commit `44de977` on `docs/design-review-2026-11-10`.
5. **Keep `unsafe` out of `origin-crypto`:** best-effort overwriting there, and the guarantee in the framework, by volatile writes over the state and an overwrite of the stack after every request (R9)? **Recommended: yes.** **Yes (2026-11-10; item 150).**
6. **§17 of CLAUDE.md,** which is your text: pin SP 800-90A to Rev. 1 (June 2015) and SP 800-90B to January 2018, and add SP 800-90C (September 2025) and FIPS 198-1 (July 2008)? **Recommended: yes.** **Yes (2026-11-10; item 151).** The directive's text is not changed on this ADR's branch. The session that keeps the directive put the wording into draft 7.4 on `docs/design-review-2026-11-10` the same day (commit `44de977`), together with §59's wording for question 4. It reaches `main` on the owner's word (decision item 106).
7. **One download:** NIST's HMAC_DRBG vectors, `hmacDRBG-1.0/internalProjection.json`, 1,160,048 bytes, from github.com/usnistgov/ACVP-Server at commit `975de31`, the release decision item 19's vectors are pinned to. It would be converted and committed as host-only test data with NIST's notice, as item 19's were. **Recommended: yes.** They are NIST's published answers for the paths RFC 6979's values do not reach, and they are to be the answers of the start-up self-test (R5). **Answered 2026-11-10: approved** ("Approve all six", to a question that also covered five documents to read; decision item 145). The file was fetched, its two SHA2-512 groups are committed, and the mechanism gives NIST's answer on all 30 vectors.

## Amendment 1 (Accepted, 2026-11-11): the loader also hands over the value the kernel stacks' region is placed from

**Status: Accepted by the owner on 2026-11-11, as recommended ("Accept all" to three recommendations put by the coordinating session, which relayed the words; decision sheet item 166; CLAUDE.md §18, §84). Built on the generator's branch as far as that branch reaches: the loader's request, the area and its checks, the kernel's function and its proof. The stack region itself is on the next batch.**

**Why.** ADR-0004 Amendment 2 (on the next batch) slides the kernel stacks' region, and draws its base where the kernel builds its page tables: before the heap and the clock. This ADR's generator needs both and starts just before the other processors do, so at that point there is nothing to ask, and R2's gate leaves the instructions to the generator alone. The base needs 64 random bits from somewhere that exists by then.

**The decision.** It amends R10 and the size boot info 0.6 gives its secret area (item 147), and ADR-0004 Amendment 2's "the kernel chooses this slide, and boot info does not change".

- **The loader makes a third 8-byte request** of its generator, after the two slides: 64 bits for the kernel stacks' region. It writes them directly after the 64 bytes of the seed, last before `ExitBootServices`, as a request of its own. Both are handed over or neither.
- **The secret area `boot_seed` names is 72 bytes:** the seed, then the value, a little-endian `u64`. The value is kept out of `BootInfo` for the reason the seed is: the structure is copied and printed. The kernel's checks cover all 72: inside a region of kind `BootInfo`, clear of the header and both arrays, no wrap.
- **Boot info stays 0.6.** It has not landed, so there is no second version bump.
- **The kernel takes the value once, early,** through `origin_frame::random::take_stack_value`: no instruction is run and no generator is asked. A second take, or a take after the generator has started, gives nothing. When the root takes the seed, all 72 bytes are overwritten and read back.
- **The kernel still places the region.** Only the random value comes from the loader. With no loader generator there is no area and no value, and the region stays at its fixed base, as ADR-0004 H3 already has it for the slides.
- **Nothing prints the value** (ADR-0004 Amendment 2, A6).

**Alternatives, as offered to the owner and not chosen.**

| Alternative | For | Against |
|---|---|---|
| Derive the value from the seed with a key-derivation function | No boot-info change | The seed gets a second use, and the directive would need a new standard for the derivation |
| Start the kernel's generator before the heap and the clock | The value comes from the kernel's own generator | It moves most of the generator's machinery into early boot: its stack, its seed area, its clock |
| Place the region after the generator starts | No hand-over at all | The generator's own stacks live in that region |

**Trade-offs.** The loader's generator stands behind one more of the kernel's secrets, where the kernel's own would have. Both are seeded from the same instructions through the same tests, so nothing is weaker; but a machine whose loader has no generator has a fixed stack region as well as a fixed image and direct map, which is ADR-0004 H3's existing price. The secret area is one read longer-lived than the seed needs: the value is read while the tables are built, and the area is overwritten when the generator starts, a few boot steps later.

**Migration.** `origin-boot-info` (`BOOT_SECRET_LEN`, `BOOT_STACK_VALUE_LEN`), the loader, `origin_frame::random` and the kernel's boot proof, all on the generator's branch. On the next batch, `stack::draw_region` becomes the caller of `take_stack_value` in place of its own draws, `stacks.rs`'s proof follows, and ADR-0004 Amendment 2 gains a note on A4 and A5. Until then nothing in the generator's branch places that region: its boot takes the value once, to prove the hand-over, and drops it.

**Threat model.** The value joins the seed in T15 (memory the loader used) and in R9's rule that the kernel overwrites what the loader handed over. It adds no interface: a program in ring 3 reaches neither the area nor the function.
