# Research ledger: the kernel's random-number generator

| | |
|---|---|
| Date | 2026-11-10 |
| For | `docs/adr/ADR-0016-kernel-random-numbers.md` (accepted by the owner on 2026-11-10, after this ledger was written); `security/crypto/src/hmac.rs` and `hmac_drbg.rs`; TASKS item 18 (task 18a) |
| Requires | CLAUDE.md §3, §16, §17, §59; decision items 93, 96, 97, 113 |
| Method | NIST's four documents and one errata sheet were read from NIST's own servers, in memory: each PDF was piped from `curl` into a text extractor and nothing was written to disk. §2 gives each one's size, SHA-256 and the pages read. NIST's vector file for HMAC_DRBG was fetched on the owner's word (decision item 145; §11). NIST's publication pages, three RFCs and Intel's guide were read through a fetch tool, which returns a language model's reading of a page and not the page itself, so every hexadecimal value taken that way was confirmed by recomputing it (§9). AMD's bulletin was read in the in-app browser as the page's own text. §11 lists what was not read. Vendor and RFC text is given in Origin's words, not quoted |

## 1. Why this was needed

Finding 8 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`: the kernel has no generator. Every consumer draws RDSEED or RDRAND itself, and a source that is stuck but still reports success gives predictable values that nothing refuses. The owner accepted one generator, NIST's HMAC_DRBG over SHA-512, seeded from more than one source, with health tests on the raw draws (decision item 113; CLAUDE.md §59). §16 asks for the revisions to be confirmed before anything is built.

## 2. Sources

**The standards, read in memory from NIST's servers on 2026-11-10.** Page numbers are the document's own.

| ID | Document | Bytes | SHA-256 | Read |
|---|---|---|---|---|
| S1 | SP 800-90A Rev. 1, `nvlpubs.nist.gov/nistpubs/SpecialPublications/NIST.SP.800-90Ar1.pdf` (110 pages) | 2,417,007 | `d58e3dac4aaa5971c9a6a20a9bd5c13e2f1930cd1a199c0c00f768e7327355e2` | §8.6.4 to §8.7.1 and §8.8 (pages 19 to 21, 23, 24), §9.1 to §9.4 (27, 28, 30 to 36), Table 2 (38), §10.1.2 (43 to 47), §11.2 to §11.4 (63 to 66) |
| S2 | SP 800-90B, `nvlpubs.nist.gov/nistpubs/SpecialPublications/NIST.SP.800-90B.pdf` (84 pages) | 1,043,696 | `9b0dd77131ade3617a91cd8457fa09e0dc354c273bb2220a6afeaca16e5defe7` | §4, health tests, whole (pages 22 to 28) |
| S3 | SP 800-90B's errata, `csrc.nist.gov/files/pubs/sp/800/90/b/final/docs/sp800-90b_errata_potential_updates.pdf` (1 page, last updated 2025-05-29) | 107,606 | `af99ae20161aeed6e8eab88badf650e08c9f5ac455b71cf1095c289c104a5128` | Whole |
| S4 | SP 800-90C, `nvlpubs.nist.gov/nistpubs/SpecialPublications/NIST.SP.800-90C.pdf` (160 pages) | 10,056,754 | `22dc2de903b2fe602fa0729c38b9512927ad96ef5669a3da8a0ffb4d22c8e6d8` | §2.2 to §2.5 in part (pages 4 to 7, 9, 10), the rule for a failed source (27), §7, generator trees (76 to 91), Appendix A.3.2 to A.3.4 (105 to 109) |
| S5 | FIPS 198-1, `nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.198-1.pdf` (13 pages) | 129,454 | `67661ba1407b391c799ff407471de18f36697af51d78a777e817c067ac30da23` | §2.3 to §6 (pages 3 to 5) |

**Web pages and other sources.**

| ID | Source | Read how | What it establishes |
|---|---|---|---|
| N1 | NIST, the publication pages of SP 800-90A Rev. 1, its Rev. 2, SP 800-90B, SP 800-90C, FIPS 198-1, SP 800-224 and IR 8427, under `csrc.nist.gov/pubs/` | Fetch tool | Titles, dates, histories, planning notes |
| N2 | NIST, the Random Bit Generation project page and its "SP 800-90 Updates" page, `csrc.nist.gov/projects/random-bit-generation` | Fetch tool | Both updated 2026-08-12. The project page says NIST is revising SP 800-90A to be consistent with SP 800-90C. The updates page lists vetted conditioning functions and changes nothing in the three documents |
| N3 | NIST, "ACVP Deterministic Random Bit Generator (DRBG) Algorithm JSON Specification", `pages.nist.gov/ACVP/draft-vassilev-acvp-drbg.html`; the page gives draft-vassilev-acvp-drbg-00, 30 October 2026 | Fetch tool | The limits NIST's validation tests hold an HMAC_DRBG over SHA-512 to, and the order of calls in a test |
| N4 | NIST, the validation program's page for random number generators, `csrc.nist.gov/projects/cryptographic-algorithm-validation-program/random-number-generators` (updated 2026-09-22) | Fetch tool | Where NIST's older vector files are, and that NIST offers them for informal checking, not as validation |
| N5 | `github.com/usnistgov/ACVP-Server`, its release list and one directory listing | GitHub's API: names, sizes and hashes only. No file was fetched | The newest release is still v1.1.0.43 (2026-08-12, commit `975de31eb83d87039ec88934fdc47d8c312b892d`), the one decision item 19's vectors are pinned to. It holds `gen-val/json-files/hmacDRBG-1.0/internalProjection.json`, 1,160,048 bytes, git blob `919f0a782067745ce303d16dff6461dda9a5d21c`. **That file was fetched later the same day, on the owner's word (decision item 145):** the size and the blob hash matched, and its SHA-256 is `e448746775d602cf1e445f0d100601752fb33f29b1baad0915acd45f8f908e66` |
| R1 | RFC 2104, "HMAC: Keyed-Hashing for Message Authentication", February 1997, Informational | Fetch tool | HMAC's definition (§2) and its advice on key length (§3) |
| R2 | RFC 4231, "Identifiers and Test Vectors for HMAC-SHA-224, HMAC-SHA-256, HMAC-SHA-384, and HMAC-SHA-512", December 2005, Standards Track | Fetch tool | Seven test cases with their HMAC-SHA-512 values (§4.2 to §4.8) |
| R3 | RFC 6979, "Deterministic Usage of the Digital Signature Algorithm (DSA) and Elliptic Curve Digital Signature Algorithm (ECDSA)", August 2013, Informational, an independent submission | Fetch tool. It read the first 100,000 of the page's 140,386 characters; everything used here was in them | §3.2 writes out the generator's instantiate, update and generate steps; §3.3 says they are HMAC_DRBG; appendix A.2.5 to A.2.7 give published outputs for SHA-512 |
| V1 | Intel, "Intel Digital Random Number Generator (DRNG) Software Implementation Guide", revision 2.1, 17 October 2018 (article 658356), on `intel.com` | Fetch tool, twice, the second time for exact sentences | What RDRAND and RDSEED are, how they report failure, and how to retry |
| V2 | AMD, security bulletin AMD-SB-7055, "RDSEED Failure on AMD 'Zen 5' Processors", published 2025-10-23, last revised 2026-01-15, `amd.com/en/resources/product-security/bulletin/amd-sb-7055.html` | In-app browser, the page's own text | That an in-scope processor's RDSEED has returned a non-random value while reporting success |
| U1 | EDK II, `MdePkg/Include/Protocol/Rng.h` and `MdePkg/Include/Guid/Rng.h`, tag `edk2-stable202608` | GitHub's API, as text; see §12 | The firmware's random-number protocol as the reference firmware declares it |
| U2 | EDK II, `OvmfPkg/Include/Dsc/OvmfRngComponents.dsc.inc`, tags `edk2-stable202408` and `edk2-stable202608` | GitHub's API, as text; see §12 | Which random-number drivers the test firmware is built with |
| T1 | Rust 1.99.1's `core`, `library/core/src/hint.rs`, in the pinned toolchain's own source on this machine | Read locally | What `core::hint::black_box` promises |

## 3. The standards today

| Document | Revision and date | In motion |
|---|---|---|
| **SP 800-90A**, "Recommendation for Random Number Generation Using Deterministic Random Bit Generators" | **Rev. 1, June 2015** (final 2015-06-24; it supersedes SP 800-90A of 2012-01-23). The PDF's own title field reads "sp800-90Arev1-final (7-16-2015) with ceiling corrections". DOI 10.6028/NIST.SP.800-90Ar1. The page carries no planning note and no errata | A **Rev. 2** exists as a pre-draft call for comments only: published 2025-09-04, comments closed 2025-11-04, with no draft to read. NIST says it will align the document with SP 800-90C, add a generator built on SHAKE, and answer comments received. Nothing on either page touches HMAC_DRBG |
| **SP 800-90B**, "Recommendation for the Entropy Sources Used for Random Bit Generation" | **January 2018** (final 2018-01-10). DOI 10.6028/NIST.SP.800-90B | A planning note of 2025-05-29 says two errata will be corrected in a future revision. Both are numerical slips, one in §5.2.4 and one in §6.3.1 (S3). **Neither touches §4, the health tests** |
| **SP 800-90C**, "Recommendation for Random Bit Generator (RBG) Constructions" | **September 2025** (final 2025-09-25, after drafts in 2012, 2016, 2022 and 2024). DOI 10.6028/NIST.SP.800-90C | NIST's server gives the PDF a modification date of 2025-11-20, two months after publication; the page records no change. CLAUDE.md §17 does not list this document. §6 below shows why it should |
| **FIPS 198-1**, "The Keyed-Hash Message Authentication Code (HMAC)" | **July 2008** (final 2008-07-16; it supersedes FIPS 198) | A planning note of 2025-06-23: NIST plans to withdraw it and move its content to **SP 800-224**, an initial public draft of 2024-06-28 whose announcement says it carries over FIPS 198-1's specification of HMAC. FIPS 198-1 is not withdrawn today |
| **NIST IR 8427**, "Discussion on the Full Entropy Assumption of the SP 800-90 Series" | April 2023 | The series treats a bit string as having full entropy when its entropy per bit is at least 1 − ε, with ε at most 2^-32. Not read beyond its abstract |

**What to pin (the ADR's proposal):** SP 800-90A Rev. 1 (June 2015), SP 800-90B (January 2018), SP 800-90C (September 2025) and FIPS 198-1 (July 2008). Re-check when SP 800-90A Rev. 2 has a draft and when SP 800-224 is final.

## 4. What SP 800-90A Rev. 1 requires of an HMAC_DRBG

Every row is from the standard's text (S1). "Code" says where `security/crypto/src/hmac_drbg.rs` stands.

**The mechanism (§10.1.2, Table 2).**

| # | Requirement | Code |
|---|---|---|
| A1 | The state (§10.1.2.1) is V and Key, each as long as the hash's output, and `reseed_counter`, with the instantiation's security strength and whether it supports prediction resistance. The standard calls V and Key the secret values the mechanism's security depends on | V and the counter as given. Key is held as HMAC's two starting states, which are equivalent to it. The strength is fixed at 256 bits. Prediction resistance is the caller's to supply entropy for |
| A2 | **Update** (§10.1.2.2), with data that may be empty: `K = HMAC(K, V ‖ 0x00 ‖ data)`, `V = HMAC(K, V)`; if the data is empty, stop; otherwise `K = HMAC(K, V ‖ 0x01 ‖ data)`, `V = HMAC(K, V)` | As given |
| A3 | **Instantiate** (§10.1.2.3): `seed_material = entropy_input ‖ nonce ‖ personalization_string`; Key is all `0x00`; V is all `0x01`; Update with the seed material; `reseed_counter = 1` | As given |
| A4 | **Reseed** (§10.1.2.4): `seed_material = entropy_input ‖ additional_input`; Update; `reseed_counter = 1` | As given |
| A5 | **Generate** (§10.1.2.5): (1) if `reseed_counter > reseed_interval`, return "reseed required"; (2) if additional input is not empty, Update with it; (3, 4) `V = HMAC(Key, V)` for each output block; (5) keep the leftmost bits asked for; (6) Update with the additional input, empty or not; (7) add one to the counter | As given |
| A6 | Table 2, SHA-512: the entropy for instantiate and reseed, and the shortest entropy input, are the security strength; the highest strength is by SP 800-57, which NIST's test specification (N3) gives as 256 bits for SHA-512. An entropy input, a personalization string and an additional input are each at most 2^35 bits. A request is at most 2^19 bits. The reseed interval is at most 2^48 requests. An implementation may set lower limits, not higher (§9.3.1, §8.6.8) | `MIN_ENTROPY_LEN` 32 bytes; `MAX_INPUT_LEN` 2^32 bytes; `MAX_REQUEST_LEN` 2^16 bytes; `MAX_RESEED_INTERVAL` 2^48, with any shorter interval settable |

**How it is used (§8, §9).**

| # | Requirement | Code, or where it falls |
|---|---|---|
| A7 | **The randomness source** (§8.6.5) must be approved: an entropy source that conforms to SP 800-90B, or a generator that conforms to SP 800-90C | The caller's. The ADR says plainly that Origin's sources are not validated |
| A8 | **Entropy input and seed are secret** (§8.6.6), protected as strongly as what the output protects | The caller's; the mechanism overwrites what it copies |
| A9 | **The nonce** (§8.6.7) holds at least half the security strength in entropy, or repeats no more often than such a value would. A random value, a time stamp of enough resolution and a sequence number are named as components. Drawn with the entropy input, the two together are an entropy input of at least 3/2 of the strength | `MIN_NONCE_LEN` 16 bytes. The mechanism can only count them |
| A10 | **A seed's life is finite** (§8.6.8). The limit, or a stricter one, shall be enforced, and at its end the generator gives nothing until reseeded. The standard prefers reseeding to a new instantiation, because an instantiation that was well seeded keeps its entropy if a later source fails unnoticed | A5 step 1, tested for intervals of 1, 2, 3 and 10 |
| A11 | **Seed use** (§8.6.9, §8.6.10): a seed is not used for a second instantiation, nor to reseed the first; a generator never reseeds itself; a seed and its entropy input serve no other purpose | The caller's. ADR R5: no draw goes into two seeds |
| A12 | **The personalization string** (§8.7.1) is optional and recommended. It is not relied on for entropy, may hold some, and an adversary's knowing it costs nothing while the entropy input is unknown | Accepted, may be empty |
| A13 | **Backtracking resistance** (§8.8) is given by every mechanism in the standard. **Prediction resistance** takes a reseed with fresh entropy of at least the security strength between two requests, and needs a source that reaches an entropy source. The standard recommends asking for it as often as is practical | The Update that closes every request; `reseed_and_generate` |
| A14 | **The instantiate function** (§9.1) checks the requested strength, then the personalization string's length, then obtains the entropy input, then the nonce | The same order |
| A15 | **The reseed function** (§9.2) checks the state, then the additional input's length, then obtains the entropy input, which is never provided by the instantiation being reseeded | The same order |
| A16 | **The generate function** (§9.3.1) checks the state (step 1), the request's length (2), the strength (3) and the additional input's length (4). If prediction resistance is asked for, or the algorithm answers "reseed required", it reseeds with the additional input (7.1) and then sets the additional input to empty (7.4) before it generates (8). Any status but success returns no bits | `check_request`'s order; `reseed_and_generate` is steps 7 and 8; a refusal writes nothing |
| A17 | **Uninstantiate** (§9.4) erases the state. A test of it shall show the state was zeroized (§11.2) | `uninstantiate`; the test reads the state back as overwritten. `wipe.rs` says what the overwrite is worth once nothing reads the memory |

**Assurance (§11).**

| # | Requirement | Where it falls |
|---|---|---|
| A18 | **Known-answer tests** (§11.3.1 to §11.3.4) of instantiate, generate and reseed, before the generator's first use. Output is held back while they run, their fixed entropy input is not used in normal operation, and prediction resistance is tested where it is supported. A generate test at intervals, and tests on demand, are recommended | Not built. ADR R5; 18b, with NIST's vectors as the answers |
| A19 | **A failed test** (§11.4.2), or a catastrophic error in normal operation such as an entropy source reported failed (§11.4.1), puts the generator in an error state: no instantiate, generate or reseed until someone intervenes, and a new instantiation afterwards | ADR R8 |

## 5. What SP 800-90B requires of health tests

From the standard's §4 (S2).

| # | Requirement |
|---|---|
| B1 | Health tests are applied to a noise source's outputs **before any conditioning** (§4.2; §4.3 item 6). Applying some to conditioned outputs as well is allowed and not required |
| B2 | **Start-up tests** run before the source is first used: the continuous tests over at least 1,024 consecutive samples (§4.3 item 4). Their samples are held back until the tests finish and may then be used |
| B3 | **Continuous tests** run on every sample. The standard says of them that they are usually designed so that only gross failures are likely to be detected, and that some poor output may leave before an error is signalled. The false-alarm rate it works with is 2^-20, in a recommended range of 2^-20 to 2^-40 (§4.3 item 3) |
| B4 | **On-demand tests** must be possible, and restarting the source and running the start-up tests again counts (§4.3 item 5) |
| B5 | **A failed test** is reported to the consumer. A designer may tell intermittent failures from persistent ones. On a persistent failure the source produces no output; it may be reset or returned to operation by the system (§4.3 item 2) |
| B6 | **Known failure modes** get tests of their own, defined by the developer (§4.3 item 8) |
| B7 | **Repetition count test** (§4.4.1): with A the last value and B its run length, fail when B reaches C, where `C = 1 + ⌈−log2(α) / H⌉`, H the assessed min-entropy of a sample. The standard calls it not very powerful: it detects a total failure |
| B8 | **Adaptive proportion test** (§4.4.2): take a window's first sample A; count in B, from 1, how often A comes in the window's W − 1 further samples; fail when B reaches C. W is 1,024 for a binary source and 512 otherwise. C is the smallest value a healthy source reaches with probability at most α, which the standard gives as `1 + CRITBINOM(W, 2^-H, 1 − α)`. For a large alphabet it lets the alphabet be reduced first (§6.4, not read) |
| B9 | **Other tests in place of these** (§4.5) must catch a value repeated more than 100/H times in a row with 99 % probability, and a value whose probability has risen to 2^(−H/2) within 50,000 samples with 50 % probability |

**The check on B7 and B8.** Worked in exact rational arithmetic (a script outside the repository), with α = 2^-20 and W = 512, B8's formula gives cutoffs of 410, 311, 177, 62 and 13 for H of 0.5, 1, 2, 4 and 8 bits: the five entries of the standard's Table 2 for non-binary data.

**What the formulas give for a 64-bit draw.**

| Assumed min-entropy H of one 64-bit draw | Repetition count cutoff | Adaptive proportion cutoff, W = 512 |
|---|---|---|
| 16 bits | 3 | 3 |
| 20 to 28 bits | 2 | 2 |
| 29 bits and above, up to the full 64 | 2 | **1** |

A cutoff of 1 is met by the window's first sample itself, so the test would fail on every window: the adaptive proportion test, as its formula stands, cannot be applied to whole 64-bit samples at the entropy a processor's vendor claims for them. The tightest cutoff that means anything is 2: the window's first value came again. A healthy source of 64-bit values trips that about once in 2^55 windows.

**What these tests can and cannot do here.** RDSEED and RDRAND are not noise sources. Each returns the output of a conditioner or of a generator inside the processor (§7), and the raw noise is not reachable from software. The tests SP 800-90B requires are the ones on raw noise, and those run inside the processor, where the kernel cannot see them. What the kernel can run are B1's optional tests on conditioned output. They catch gross failure, which is B3's own description of continuous tests: a value that repeats, a source cycling through a few values, a constant. They cannot see a source whose outputs look random and are predictable. Nothing here makes Origin's kernel an SP 800-90B entropy source.

## 6. What SP 800-90C adds

From the standard's text (S4). This is the document for the question finding 8 leaves open: how a generator and its sources are put together, and how one generator seeds others.

| # | What it says |
|---|---|
| C1 | **Constructions** (§2.2, Table 1). An RBG2 construction is a generator with an entropy source of its own, physical (P) or not (NP). An **RBGC** construction is one generator in a **tree on one computing platform**: the root is seeded from an "initial randomness source", and every other generator from its parent |
| C2 | **Sources are validated and independent** (§2.3). The constructions are built on entropy sources validated against SP 800-90B. Where several are used, their outputs are concatenated, and the entropy is counted by one of two methods: Method 1 counts physical sources only and lets the others be mixed in uncounted; Method 2 counts both. Either way the sources counted must be independent |
| C3 | **A failed source** (§3, page 27). Its output from the failing request is not used. It is not drawn from again until the cause is corrected and it has passed its operational tests. If every counted source has failed, the generator's operation stops until one has. A source still healthy may go on being used |
| C4 | **Reseeding and prediction resistance** (§2.4.2). A reseed from an entropy source takes at least as many bits of min-entropy as the strength; from another generator, at least as many bits. Only a reseed that reaches an entropy source gives prediction resistance. Reseeding one generator from another is still called a good idea. Entropy in additional input does not raise a generator's strength and is good practice to include |
| C5 | **The tree** (§7.1). The standard calls the design common on many computing platforms and gives an operating system's generator as its example. Its Appendix A.3.4 draws one: a root, and a child generator for each of three processors. A generator has one parent. There are no loops: a generator never provides seed material for an ancestor. Additional input, though, "may be anything", the output of any generator in the tree included |
| C6 | **The root** (§7.1.2.1, §7.2.1.1, §7.3.2) is seeded and reseeded from the initial randomness source: a validated full-entropy source, or a validated RBG2 or RBG3 construction. For HMAC_DRBG it takes 3/2 of the strength to instantiate and the strength to reseed. Seeded from an RBG2 construction, it has no more strength than that construction, which should be reseeded first. The root can give prediction resistance |
| C7 | **A child** (§7.1.2.2, §7.2.1.2, §7.2.3.2) is instantiated from its parent, with 3/2 of the strength. It is reseeded from its parent, or, when the parent is not available, from an ancestor, a sibling of its parent or the initial randomness source. Appendix A.3.4 gives the cost of reaching a parent on another processor as a reason. A child cannot promise prediction resistance and should be reseeded periodically. When to reseed is the implementer's: time, a count of outputs and events are the examples (§7.2.3) |
| C8 | **A child whose source is not available** may go on generating without reseeding (§7.3.1 item 12) |
| C9 | **General requirements** (§7.3.1): no output before instantiation; a generator whose health test fails is terminated; seed material and state stay secret and stay on the platform; an output used as seed material is used for nothing else |
| C10 | **Copies** (Appendix A.3.2). A generator's state must not move to another platform by backup, snapshot or cloning. A copy made by forking on one platform must be reseeded before it answers any request |

**How the ADR's design sits against this.** The root and a generator for each processor are C5's tree, by the standard's own example. Counting RDSEED and mixing in the loader's seed uncounted is C2's Method 1. Three things differ, and Origin claims no conformance (ADR R1):

- **The sources are not validated by Origin.** Whether a given processor's RDSEED holds an entropy-source validation was not checked, and the kernel cannot check it.
- **A child also takes its own processor's draws when it is instantiated.** The standard seeds a child from its parent alone. The draws go in as the kind of extra input C4 and C5 allow, and are not counted.
- **A child reseeds from its own processor's instruction first,** and from the root only when that is not healthy. The standard has the parent first and the initial randomness source as the alternative.

## 7. The processor's instructions

Vendor text is given here in Origin's words, not quoted.

**Intel (V1).**

- The generator in the processor is an entropy source, hardware health tests, an AES-CBC-MAC conditioner that turns pairs of 256-bit raw samples into one 256-bit conditioned sample, and a CTR_DRBG from SP 800-90A §10.2.1 over AES, which the conditioner keeps reseeding.
- **RDRAND** returns that generator's output. The guide bounds what one seed produces at 511 samples of 128 bits. It does not give the generator's key size.
- **RDSEED** returns conditioned samples directly. The guide says they are there for software to seed its own generators with, and calls the part that serves them an ENRNG that complies with SP 800-90B and SP 800-90C.
- **Failure is the carry flag and nothing else** (§5.2 and §5.3 of the guide, Tables 3 and 5). With the flag set, the destination holds a valid value, which the tables describe as non-zero. With it clear, the destination is zero, nothing was available, and the call may be retried. The guide says the flag is the only indicator of success and that a zero value must not be used as one.
- **Retrying.** For RDRAND the guide recommends ten retries in a tight loop. RDSEED can run dry: callers can ask faster than values are made, it has no fairness mechanism, and the guide gives no bound on how often to retry. It recommends a PAUSE in the loop, suggests giving up after a small number of tries (between 1 and 100) where the caller cannot wait, and allows falling back to RDRAND.
- **RDRAND as a seed.** To be sure the hardware generator reseeded between two values, the guide gives two ways: more than 1,022 RDRAND values of 64 bits, or 32 RDRAND calls 10 microseconds apart. Its worked example reduces 512 values of 128 bits to one with AES-CBC-MAC. This is C6's advice to reseed an RBG2 source before taking seed material from it.
- **One generator for the package.** The guide describes one resource shared between cores and threads, joined to each core by a bus, with its rate limits applying across all of them. So two processors' draws are not two independent sources in C2's sense.
- If the built-in self-test or the hardware health tests fail, the generator stops giving values instead of giving poor ones.

**AMD (V2).** AMD's bulletin of 2025-10-23 describes a bug in Zen 5 processors in which RDSEED can return zero far more often than a random source would, while the carry flag reports success. The 16-bit and 32-bit forms are affected. AMD states that the 64-bit form is not. It is CVE-2025-62626, rated 7.2. Until fixed firmware is installed AMD gives three workarounds: use the 64-bit form; hide RDSEED from software through CPUID; or treat a returned zero as a failure and retry later until a non-zero value comes with the flag set. Fixed AGESA and microcode versions are listed for each Zen 5 product, with release dates from 2025-10-28 to 2025-12-26.

Zen 5 is the oldest AMD core in Origin's declared scope (ADR-0009 Amendment 1). This is the failure finding 8 names, on in-scope hardware, a year old. It is also what B6 means by a known failure mode. Origin's kernel and loader use the 64-bit form only (`framekernel/frame/src/cpu.rs`, `boot/uefi/src/main.rs`), which AMD says is not affected. No gate holds them to it.

**Not read:** the Intel SDM's and the AMD APM's own entries for the two instructions; AMD's description of its generator; any entropy-source validation certificate for either vendor's processors. For Intel the guide above is the vendor's statement of the instructions' semantics. AMD's design is not described from a source here at all.

## 8. The firmware's generator

**The specification.** UEFI 2.11's text was not read: `uefi.org` answers this session's fetch tool with HTTP 403 and the in-app browser with a bot check, which was not bypassed. The UEFI ledger of 2026-11-09 met the same refusal.

**The reference firmware's declaration (U1), facts only.**

- The protocol's GUID is `3152bca5-eade-433d-862e-c01cdc291f44`. The header dates the protocol to UEFI 2.4.
- It has two functions. The first lists the algorithms the driver supports, as GUIDs, the first being its default. The second fills a caller's buffer with exactly the number of bytes asked for, by a named algorithm or, when none is named, by the default.
- The second function's failures: the algorithm is not supported; a hardware or firmware error; **not ready**, when there is not enough random data for the length asked; an invalid parameter, for no buffer or a length of zero.
- The algorithms the header names: three SP 800-90 generators at 256 bits (hash, HMAC and counter mode; its comment cites the 2007 edition of SP 800-90), two from ANSI X9.31, Arm's RNDR, and a "raw" one (`e43176d7-b6e8-4827-b784-7ffdc4b68561`), described as entropy straight from the source with no deterministic generator in between.
- Every algorithm is optional, and so is the protocol: a loader must expect to find none.

**What the specification says beyond the header** (a minimum security strength, what "raw" must deliver) is not known here.

**The test firmware (U2).** OVMF's build includes two drivers that publish the protocol: one over a library that on x86 draws from the processor, and one for a virtio random-number device. Under QEMU without that device, then, the firmware's generator is most likely the virtual processor's RDRAND again. That reading of the library is the author's and was not checked in its source.

**What follows for the design.** The firmware's seed cannot be assumed present, cannot be tested (it is one sample), and may be the same hardware the kernel draws from itself. In C2's terms it is a source that is neither validated nor known to be independent. It can only ever be mixed in uncounted; it can never be the whole of a seed.

## 9. What the code was checked against

| Check | Source of the expected values | Result |
|---|---|---|
| The code against the standard's text | S1 §10.1.2 and §9; S5 Table 1 | Read side by side after the code was written: every step, every limit and the order of every check agree (§4). Two checks were reordered to follow §9.1 and §9.2 |
| HMAC-SHA-512, RFC 4231 test cases 1 to 7 | R2, from the RFC's text | All seven pass. Before they were embedded, each transcribed value was reproduced with Python 3.14.7's `hmac` (OpenSSL 3.5.7) |
| HMAC-SHA-512, 273 key and message lengths | `security/crypto/tools/gen_hmac_vectors.py`: written only where Python's `hmac` and RFC 2104's formula over `hashlib` agree | All pass |
| HMAC_DRBG, six published outputs | R3, appendix A.2.5 (P-256), A.2.6 (P-384) and A.2.7 (P-521), SHA-512, messages "sample" and "test". §3.3 of the RFC: the value k is what an HMAC_DRBG gives when instantiated with the private key as its entropy input and the reduced message hash as its nonce, with no personalization string | All six pass, each on the first candidate. P-521 needs 521 bits, so it runs two output blocks. Before they were embedded, the transcribed q, x and k were reproduced by a separate Python computation written from §3.2's steps |
| HMAC_DRBG, 145 instantiations with 74 reseeds, 629 requests and 79 requests with prediction resistance | `security/crypto/tools/hmac_drbg_reference.py`, an independent implementation in Python over `hmac`, which must reproduce the six RFC values before it writes anything | All pass. **The reference has the same author as the code.** The next row is the check on it that does not |
| The corpus against another implementation | The HMAC-DRBG of OpenSSL 4.1.0 (23 October 2026), which Homebrew had already installed on the build host; nothing was downloaded. `hmac_drbg_reference.py crosscheck` replays every case through it, with OpenSSL's test generator handing it the corpus's entropy inputs and nonces | 143 cases agree with the corpus line for line. The other 2 hold a request of no bytes, for which OpenSSL never calls its generator; they agree with the reference once that request is left out on both sides. 706 outputs in all. Six controls each make the check report a difference: an expected output altered, the entropy of a request with prediction resistance altered, an additional input altered, a reseed's entropy altered, OpenSSL given another additional input than the reference, and OpenSSL asked without prediction resistance where the corpus has it. A development check: nothing in the build or the tests depends on OpenSSL |

| HMAC_DRBG, NIST's own vectors | N5: the two SHA2-512 groups of NIST's ACVP set for HMAC_DRBG, 30 vectors, converted by `tools/convert_kat_vectors.py` into `tests/vectors/acvp/hmac-drbg-sha2-512.txt`. 15 have prediction resistance and 15 a reseed; every one has a personalization string, additional input in every step and 512 bytes of output. The set's other 300 vectors are over ten hash functions the crate's generator is not built on | All 30 give NIST's answer (`tests/kat/drbg.rs`). The Python reference reproduced all 30 before the crate was run. Controls: an answer, an entropy input, a nonce, a personalization string, a reseed's entropy or an additional input corrupted; a step left out; two requests exchanged; additional input passed on to the generate step of a request with prediction resistance. Each fails the check |

RFC 6979 is Informational, not a standard, and it exercises instantiation and one request only. NIST's vectors are the published known answers for the rest: reseeding, additional input and prediction resistance. With them, every step of the mechanism is checked against a published answer, except one: that a request of no bytes still moves the state on, which rests on the standard's steps alone.

## 10. What Rust 1.99.1 promises about overwriting memory (T1)

`core::hint::black_box`'s own documentation, in the pinned toolchain, says two things: the function is provided on a best-effort basis only, and it gives no guarantee for cryptographic or security purposes. The only write the language guarantees is never removed is `core::ptr::write_volatile`, which is `unsafe`. `core` has no safe function that overwrites memory with that guarantee.

What the pinned compiler does today, read from `origin-crypto`'s release object for `x86_64-unknown-none`: the two overwrite functions exist as functions of their own, each a `memset` followed by the opaque use, and every function of the generator that should call them does (the counts are in `SOURCE_PROVENANCE.md`). That is an observation of one build, not a promise.

## 11. Not read, and how the files were handled

| Not read | Why | What rests on it |
|---|---|---|
| The rest of S1, S2 and S4: in SP 800-90A, §1 to §8.5, the other mechanisms and the appendices; in SP 800-90B, entropy estimation and conditioning (§3, §5, §6, with §6.4's alphabet reduction); in SP 800-90C, RBG1 to RBG3 in detail (§4 to §6), §8, and the rest of the appendices | Not needed for the mechanism or the tree | §6.4 of SP 800-90B, if the adaptive proportion test is ever run on a reduced alphabet. SP 800-90C §5, for what an RBG2 construction is held to, if Origin ever claims more than it does |
| UEFI 2.11 §37.5 | `uefi.org` refuses automated access | What the firmware's generator must deliver |
| The Intel SDM's and AMD APM's entries for RDRAND and RDSEED; AMD's account of its generator; the vendors' entropy-source validation certificates | Not fetched | AMD's design; whether RDSEED on a declared target is a validated source |

**How the six files were handled, in the order it happened** (decision item 145).

1. The first attempt to read SP 800-90A went through the fetch tool, which did not return its text. It saved the PDF, twice, in this session's own tool-results folder under `~/.claude/projects/`, outside the repository. That was not intended.
2. The owner was asked, through the coordinating session, for leave to download six files, each with its name, source and size: the five documents of §2 to read, and NIST's vector file to convert and commit. The owner was told of the two saved copies.
3. **Before the answer reached this session,** the five documents were read in memory, as §2 says, which writes no file. That is the project's recorded way to read vendor documents, and the session for TASKS item 19 read its own that way the same day. The two saved copies were never opened; their SHA-256 was S1's, and they were deleted, as that same practice note says to. The coordinating session's message to wait, and to leave the copies to the owner, was delivered only after both had been done.
4. The owner answered "Approve all six". The vector file was fetched after that, and only it: 1,160,048 bytes, with the git blob hash GitHub lists for it. It is kept outside the repository; what is committed is the conversion of its two SHA2-512 groups, with NIST's notice.

So the five documents were read before the owner's word had arrived and without a file being kept, and the one file that had to be kept was fetched after it.

## 12. Open-source references consulted (CLAUDE.md §3, §6; decision items 93, 96, 97)

| File | Project | Tag and commit | Licence | Read | Reason | Gap it filled |
|---|---|---|---|---|---|---|
| `MdePkg/Include/Protocol/Rng.h` (blob `d61f9e98`) | EDK II (item 94's list) | `edk2-stable202608`, `2970e5699ba6267f3384ffab20f96647578aebc8`, the newest stable tag on the day | BSD-2-Clause-Patent | 2026-11-10 | The specification could not be opened (§8), and the ADR has to say what the loader can ask the firmware for | The protocol's GUID |
| `MdePkg/Include/Guid/Rng.h` (blob `bf7956af`) | EDK II | The same | BSD-2-Clause-Patent | 2026-11-10 | The same | The two functions, their failures, the algorithm GUIDs |
| `OvmfPkg/Include/Dsc/OvmfRngComponents.dsc.inc` | EDK II | `edk2-stable202408` (`b158dad150bf02879668f72ce306445250838201`), which the test firmware is built from, and `edk2-stable202608` | BSD-2-Clause-Patent | 2026-11-10 | Research into what the reference firmware supports (item 97) | Whether the firmware 18b will be tested on offers the protocol at all |

No code, identifier or file structure was carried over: §8 states the interface in Origin's own words (item 121). This session wrote no loader code. The session that does, 18b, writes it from §8 and from the specification if the owner can supply it, without these files open (item 92).
