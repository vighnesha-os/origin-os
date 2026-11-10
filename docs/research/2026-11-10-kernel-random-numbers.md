# Research ledger: the kernel's random-number generator

| | |
|---|---|
| Date | 2026-11-10 |
| For | `docs/adr/ADR-0016-kernel-random-numbers.md` (Proposed); `security/crypto/src/hmac.rs` and `hmac_drbg.rs`; TASKS item 18 (task 18a) |
| Requires | CLAUDE.md §3, §16, §17, §59; decision items 93, 96, 97, 113 |
| Method | NIST's publication pages, three RFCs, Intel's guide and AMD's bulletin were read today as web pages. The RFCs and NIST's pages were read through a fetch tool, which returns a language model's reading of the page and not the page itself, so every hexadecimal value taken that way was confirmed by recomputing it (§9). AMD's bulletin was read in the in-app browser as the page's own text. **The standards themselves are PDFs, and their text was not read:** downloading a file needs the owner's word, which was asked for and had not come when this was written (§11). Where a statement below about SP 800-90A or SP 800-90B rests on the author's knowledge of the standard and not on a text read today, its row says so |

## 1. Why this was needed

Finding 8 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`: the kernel has no generator. Every consumer draws RDSEED or RDRAND itself, and a source that is stuck but still reports success gives predictable values that nothing refuses. The owner accepted one generator, NIST's HMAC_DRBG over SHA-512, seeded from more than one source, with health tests on the raw draws (decision item 113; CLAUDE.md §59). §16 asks for the revisions to be confirmed before anything is built.

## 2. Sources

| ID | Source | Read how | What it establishes |
|---|---|---|---|
| N1 | NIST, SP 800-90A Rev. 1's publication page, `csrc.nist.gov/pubs/sp/800/90/a/r1/final` | Fetch tool | Title, date, history, and that a Rev. 2 is in preparation |
| N2 | NIST, SP 800-90A Rev. 2's page, `csrc.nist.gov/pubs/sp/800/90/a/r2/iprd` | Fetch tool | What Rev. 2 is today: a call for comments with no draft text |
| N3 | NIST, SP 800-90B's publication page, `csrc.nist.gov/pubs/sp/800/90/b/final` | Fetch tool | Title, date, and the errata note |
| N4 | NIST, SP 800-90C's publication page, `csrc.nist.gov/pubs/sp/800/90/c/final` | Fetch tool | That SP 800-90C is final, and what it covers |
| N5 | NIST, the Random Bit Generation project page and its "SP 800-90 Updates" page, `csrc.nist.gov/projects/random-bit-generation` | Fetch tool | Both updated 2026-08-12. The project page says NIST is revising SP 800-90A to be consistent with SP 800-90C. The updates page lists vetted conditioning functions and changes nothing in the three documents |
| N6 | NIST, FIPS 198-1's page and SP 800-224's draft page, `csrc.nist.gov/pubs/fips/198-1/final`, `/pubs/sp/800/224/ipd` | Fetch tool | HMAC's standard and what is planned for it |
| N7 | NIST, IR 8427's page, `csrc.nist.gov/pubs/ir/8427/final` | Fetch tool | What "full entropy" means in the series |
| N8 | NIST, "ACVP Deterministic Random Bit Generator (DRBG) Algorithm JSON Specification", `pages.nist.gov/ACVP/draft-vassilev-acvp-drbg.html`; the page gives draft-vassilev-acvp-drbg-00, 30 October 2026 | Fetch tool | The limits NIST's validation tests hold an HMAC_DRBG over SHA-512 to, and the order of calls in a test |
| N9 | NIST, the validation program's page for random number generators, `csrc.nist.gov/projects/cryptographic-algorithm-validation-program/random-number-generators` (updated 2026-09-22) | Fetch tool | Where NIST's older vector files are, and what NIST says they are for |
| N10 | `github.com/usnistgov/ACVP-Server`, its release list and one directory listing | GitHub's API: names, sizes and hashes only. No file was fetched | The newest release is still v1.1.0.43 (2026-08-12, commit `975de31eb83d87039ec88934fdc47d8c312b892d`), the one decision item 19's vectors are pinned to. It holds `gen-val/json-files/hmacDRBG-1.0/internalProjection.json`, 1,160,048 bytes, git blob `919f0a782067745ce303d16dff6461dda9a5d21c` |
| R1 | RFC 2104, "HMAC: Keyed-Hashing for Message Authentication", February 1997, Informational, `rfc-editor.org/rfc/rfc2104.txt` | Fetch tool | HMAC's definition (§2) and its advice on key length (§3) |
| R2 | RFC 4231, "Identifiers and Test Vectors for HMAC-SHA-224, HMAC-SHA-256, HMAC-SHA-384, and HMAC-SHA-512", December 2005, Standards Track | Fetch tool | Seven test cases with their HMAC-SHA-512 values (§4.2 to §4.8) |
| R3 | RFC 6979, "Deterministic Usage of the Digital Signature Algorithm (DSA) and Elliptic Curve Digital Signature Algorithm (ECDSA)", August 2013, Informational, an independent submission | Fetch tool. It read the first 100,000 of the page's 140,386 characters; everything used here was in them | §3.2 writes out the generator's instantiate, update and generate steps; §3.3 says they are HMAC_DRBG; appendix A.2.5 to A.2.7 give published outputs for SHA-512 |
| V1 | Intel, "Intel Digital Random Number Generator (DRNG) Software Implementation Guide", revision 2.1, 17 October 2018 (article 658356), `intel.com/content/www/us/en/developer/articles/guide/intel-digital-random-number-generator-drng-software-implementation-guide.html` | Fetch tool, twice, the second time for exact sentences | What RDRAND and RDSEED are, how they report failure, and how to retry |
| V2 | AMD, security bulletin AMD-SB-7055, "RDSEED Failure on AMD 'Zen 5' Processors", published 2025-10-23, last revised 2026-01-15, `amd.com/en/resources/product-security/bulletin/amd-sb-7055.html` | In-app browser, the page's own text | That an in-scope processor's RDSEED has returned a non-random value while reporting success |
| U1 | EDK II, `MdePkg/Include/Protocol/Rng.h` and `MdePkg/Include/Guid/Rng.h`, tag `edk2-stable202608` | GitHub's API, as text; see §12 | The firmware's random-number protocol as the reference firmware declares it |
| U2 | EDK II, `OvmfPkg/Include/Dsc/OvmfRngComponents.dsc.inc`, tags `edk2-stable202408` and `edk2-stable202608` | GitHub's API, as text; see §12 | Which random-number drivers the test firmware is built with |
| T1 | Rust 1.99.1's `core`, `library/core/src/hint.rs`, in the pinned toolchain's own source on this machine | Read locally | What `core::hint::black_box` promises |

## 3. The standards today

| Document | Revision and date | In motion |
|---|---|---|
| **SP 800-90A**, "Recommendation for Random Number Generation Using Deterministic Random Bit Generators" | **Rev. 1, June 2015** (final 2015-06-24; it supersedes SP 800-90A of 2012-01-23). Authors Barker and Kelsey. DOI 10.6028/NIST.SP.800-90Ar1. The page carries no planning note and no errata (N1) | A **Rev. 2** exists as a pre-draft call for comments only: published 2025-09-04, comments closed 2025-11-04, with no draft to read. NIST says it will align the document with SP 800-90C, add a generator built on SHAKE, and answer comments received (N2). Nothing on either page touches HMAC_DRBG |
| **SP 800-90B**, "Recommendation for the Entropy Sources Used for Random Bit Generation" | **January 2018** (final 2018-01-10). DOI 10.6028/NIST.SP.800-90B (N3) | A planning note of 2025-05-29 says two errata have been identified and will be corrected in a future update or revision, and links an errata file. The file is a PDF and was not read, so **whether either erratum touches the two health tests is not known** |
| **SP 800-90C**, "Recommendation for Random Bit Generator (RBG) Constructions" | **September 2025** (final 2025-09-25, after drafts in 2012, 2016, 2022 and 2024). DOI 10.6028/NIST.SP.800-90C (N4) | It is the document that says how a generator (90A) and its entropy sources (90B) are put together, in four classes it names RBG1, RBG2, RBG3 and RBGC, with "subordinate DRBG" among its keywords. NIST's server gives the PDF a modification date of 2025-11-20, two months after publication; the page records no change. **Not read.** CLAUDE.md §17 does not list it |
| **FIPS 198-1**, "The Keyed-Hash Message Authentication Code (HMAC)" | **July 2008** (final 2008-07-16; it supersedes FIPS 198) (N6) | A planning note of 2025-06-23: NIST plans to withdraw it and move its content to **SP 800-224**, which is an initial public draft of 2024-06-28 whose announcement says it carries over FIPS 198-1's specification of HMAC. FIPS 198-1 is not withdrawn today. **Neither PDF was read**; HMAC here is written from RFC 2104, which FIPS 198-1 generalises |
| **NIST IR 8427**, "Discussion on the Full Entropy Assumption of the SP 800-90 Series" | April 2023 (N7) | The series treats a bit string as having full entropy when its entropy per bit is at least 1 − ε, with ε at most 2^-32. Not read beyond its abstract |

**What to pin (the ADR's proposal):** SP 800-90A Rev. 1 (June 2015), SP 800-90B (January 2018, with its errata to be read), FIPS 198-1 (July 2008), and SP 800-90C (September 2025) as a design reference. Re-check when Rev. 2 has a draft.

## 4. What SP 800-90A Rev. 1 requires of an HMAC_DRBG

"Basis" says what stands behind each row today. Three bases are independent of the author's memory: RFC 6979's text (R3), six of its published outputs reproduced by the code (§9), and NIST's test specification (N8). The rest is the author's knowledge of the standard, **to be checked against its text**.

**The mechanism (§10.1.2).**

| # | Requirement | Basis |
|---|---|---|
| A1 | The state is a key and a value V, each as long as the hash's output (512 bits here), and a count of requests since the last seed | Key and V: R3 §3.2, steps b and c. The count: the author's knowledge |
| A2 | **Update**, given data that may be empty: `K = HMAC(K, V ‖ 0x00 ‖ data)`, `V = HMAC(K, V)`; and if the data is not empty, a second round with `0x01` | R3 §3.2 steps d to g (data present) and step h's retry (data empty); reproduced |
| A3 | **Instantiate**: K is all `0x00`, V is all `0x01`, then Update with `entropy_input ‖ nonce ‖ personalization_string`; the count becomes 1 | R3 §3.2 and §3.3 for the entropy input and the nonce, with an empty personalization string; reproduced. That the personalization string comes last: the author's knowledge |
| A4 | **Reseed**: Update with `entropy_input ‖ additional_input`; the count becomes 1 | The author's knowledge |
| A5 | **Generate**: past the reseed interval, return "reseed required" and nothing else. Otherwise: if additional input was given, Update with it; then `V = HMAC(K, V)` once for each output block, keeping the leftmost bits asked for; then Update with the additional input, which may be empty; then add one to the count | Block generation, the leftmost bits and the closing Update with no data: R3 §3.2 step h, reproduced for one block and for two. The additional input's two uses and the count: the author's knowledge |
| A6 | For SHA-512: the highest security strength is 256 bits; an entropy input holds at least that much entropy in at least 256 bits; a nonce at instantiation is at least 128 bits | N8, Table 4, the row for `hmacDRBG` with SHA2-512: a maximum security strength of 256, a minimum entropy length of 256 and a minimum nonce length of 128 |
| A7 | For SHA-512: an entropy input, a personalization string and an additional input are each at most 2^35 bits; a request is at most 2^19 bits; the reseed interval is at most 2^48 requests | The author's knowledge. (N8's own maxima, 65536 bits for an input and 4096 for a request, are its test harness's, not the standard's) |
| A8 | An HMAC_DRBG uses no derivation function | N8: for `hmacDRBG` the derivation-function property must be left out or false |

**How it is used (§8, §9).**

| # | Requirement | Basis |
|---|---|---|
| A9 | The nonce (§8.6.7) holds at least half the security strength in entropy, or is a value that repeats no more often than such a value would. It may be drawn from the same source as the entropy input, in the same draw | The author's knowledge; the length is A6's |
| A10 | The personalization string (§8.7.1) is optional, should tell one instantiation from every other, and need not be secret | The author's knowledge |
| A11 | **Prediction resistance** (§8.8, §9.3.1): a request that asks for it is answered only after a reseed from fresh entropy. The reseed takes the request's additional input, and the generate step that follows takes none. The same sequence answers "reseed required" | The author's knowledge. N8's Table 7 agrees on the order of calls (instantiate, generate, generate, uninstantiate, with a reseed after instantiation in the reseed tests) and does not state the rule for the additional input |
| A12 | A generator that reaches the end of its seed's life and cannot be reseeded gives nothing more (§9.3.2) | The author's knowledge |
| A13 | **Uninstantiate** (§9.4) erases the state | The author's knowledge |
| A14 | Every generator in the standard gives backtracking resistance: a state learned now does not give earlier output. For HMAC_DRBG that is the Update that closes every request | The author's knowledge |
| A15 | **Health testing of the mechanism** (§11.3): known-answer tests of instantiate, generate and reseed before first use, and of generate from time to time; a failure puts the generator in an error state in which it gives nothing | The author's knowledge. N8 §6.1.2 puts reseeding at the end of a seed's life, uninstantiation and health checks outside what its own tests cover |

## 5. What SP 800-90B requires of health tests

All of this section is the author's knowledge of the standard, **to be checked against its text**, with one independent check on the two formulas (below).

| # | Requirement |
|---|---|
| B1 | An entropy source is a noise source, health tests on it, and an optional conditioning step. The health tests run on the noise source's raw samples, before any conditioning |
| B2 | Three kinds of test (§4.3): start-up tests, run before the source's first output is used, which are the continuous tests over at least 1,024 consecutive samples; continuous tests, on every sample while the source runs; and on-demand tests. Samples a start-up test examined may be used once it has passed |
| B3 | Two continuous tests are approved (§4.4). A designer may use others that detect the same failures |
| B4 | **Repetition count test** (§4.4.1): fail when one value comes C times in a row, with `C = 1 + ⌈−log2(α) / H⌉`. H is the source's assessed min-entropy for each sample and α the accepted chance of a false alarm, with 2^-20 recommended |
| B5 | **Adaptive proportion test** (§4.4.2): take a window's first sample and count how often it comes again in the window; fail when the count reaches C. The window W is 1,024 samples for a binary source and 512 otherwise, and `C = 1 + CRITBINOM(W, 2^-H, 1 − α)` |
| B6 | A failed test is reported, and the samples that failed it are not given out |

**The check on B4 and B5.** Worked in exact rational arithmetic (a script outside the repository), with α = 2^-20 and W = 512, B5's formula gives cutoffs of 311, 177, 62 and 13 for H of 1, 2, 4 and 8 bits. Those are the values the author remembers from the standard's own table, so the formula and the memory agree. That is not the same as reading the table.

**What the formulas give for a 64-bit draw.**

| Assumed min-entropy H of one 64-bit draw | Repetition count cutoff | Adaptive proportion cutoff, W = 512 |
|---|---|---|
| 16 bits | 3 | 3 |
| 20 to 28 bits | 2 | 2 |
| 29 bits and above, up to the full 64 | 2 | **1** |

A cutoff of 1 is met by the window's first sample itself, so the test would fail on every window: SP 800-90B's adaptive proportion test, as its formula stands, cannot be applied at the entropy a processor's vendor claims for these draws. The tightest cutoff that means anything is 2: the window's first value came again. A healthy source of 64-bit values trips that about once in 2^55 windows.

**What these tests can and cannot do here.** RDSEED and RDRAND are not noise sources. Each returns the output of a conditioner or of a generator inside the processor (§7), and the raw noise is not reachable from software. So the kernel cannot run SP 800-90B's tests as the standard means them, on raw noise, and nothing here may be called an SP 800-90B entropy source or conformant health testing. What the two algorithms can do on the instructions' outputs is catch gross failure: a value that repeats, a source cycling through a few values, a constant. They cannot see a source whose outputs look random and are predictable.

## 6. What SP 800-90C adds

Not read. From its page (N4): it specifies how to build a random bit generator from a 90A generator and 90B sources, in the classes RBG1, RBG2, RBG3 and RBGC, and it has a term for a generator seeded from another, the subordinate DRBG. The ADR's design, one root generator that seeds one generator for each processor, is that kind of arrangement. Whether it matches one of the standard's classes, and what the standard requires of a child's seed, is to be read before 18b builds it.

## 7. The processor's instructions

Vendor text is given here in Origin's words, not quoted.

**Intel (V1).**

- The generator in the processor is an entropy source, hardware health tests, an AES-CBC-MAC conditioner that turns pairs of 256-bit raw samples into one 256-bit conditioned sample, and a CTR_DRBG from SP 800-90A §10.2.1 over AES, which the conditioner keeps reseeding.
- **RDRAND** returns that generator's output. The guide bounds what one seed produces at 511 samples of 128 bits. It does not give the generator's key size.
- **RDSEED** returns conditioned samples directly. The guide says they are there for software to seed its own generators with, and calls the part that serves them an ENRNG that complies with SP 800-90B and SP 800-90C.
- **Failure is the carry flag and nothing else** (§5.2 and §5.3 of the guide, Tables 3 and 5). With the flag set, the destination holds a valid value, which the tables describe as non-zero. With it clear, the destination is zero, nothing was available, and the call may be retried. The guide says the flag is the only indicator of success and that a zero value must not be used as one.
- **Retrying.** For RDRAND the guide recommends ten retries in a tight loop. RDSEED can run dry: callers can ask faster than values are made, it has no fairness mechanism, and the guide gives no bound on how often to retry. It recommends a PAUSE in the loop, suggests giving up after a small number of tries (between 1 and 100) where the caller cannot wait, and allows falling back to RDRAND.
- **RDRAND as a seed.** To be sure the hardware generator reseeded between two values, the guide gives two ways: more than 1,022 RDRAND values of 64 bits, or 32 RDRAND calls 10 microseconds apart. Its worked example reduces 512 values of 128 bits to one with AES-CBC-MAC.
- **One generator for the package.** The guide describes one resource shared between cores and threads, joined to each core by a bus, with its rate limits applying across all of them. So two processors' draws are not two independent sources.
- If the built-in self-test or the hardware health tests fail, the generator stops giving values instead of giving poor ones.

**AMD (V2).** AMD's bulletin of 2025-10-23 describes a bug in Zen 5 processors in which RDSEED can return zero far more often than a random source would, while the carry flag reports success. The 16-bit and 32-bit forms are affected. AMD states that the 64-bit form is not. It is CVE-2025-62626, rated 7.2. Until fixed firmware is installed AMD gives three workarounds: use the 64-bit form; hide RDSEED from software through CPUID; or treat a returned zero as a failure and retry later until a non-zero value comes with the flag set. Fixed AGESA and microcode versions are listed for each Zen 5 product, with release dates from 2025-10-28 to 2025-12-26.

Zen 5 is the oldest AMD core in Origin's declared scope (ADR-0009 Amendment 1). This is the failure finding 8 names, on in-scope hardware, a year old. Origin's kernel and loader use the 64-bit form only (`framekernel/frame/src/cpu.rs`, `boot/uefi/src/main.rs`), which AMD says is not affected. No gate holds them to it.

**Not read:** the Intel SDM's and the AMD APM's own entries for the two instructions; AMD's description of its generator. For Intel the guide above is the vendor's statement of the same semantics. AMD's design is not described from a source here at all.

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

**What follows for the design.** The firmware's seed cannot be assumed present, cannot be tested (it is one sample), and may be the same hardware the kernel draws from itself. It can only ever add to a seed; it can never be the whole of one.

## 9. What the code was checked against

| Check | Source of the expected values | Result |
|---|---|---|
| HMAC-SHA-512, RFC 4231 test cases 1 to 7 | R2, from the RFC's text | All seven pass. Before they were embedded, each transcribed value was reproduced with Python 3.14.7's `hmac` (OpenSSL 3.5.7) |
| HMAC-SHA-512, 273 key and message lengths | `security/crypto/tools/gen_hmac_vectors.py`: written only where Python's `hmac` and RFC 2104's formula over `hashlib` agree | All pass |
| HMAC_DRBG, six published outputs | R3, appendix A.2.5 (P-256), A.2.6 (P-384) and A.2.7 (P-521), SHA-512, messages "sample" and "test". §3.3 of the RFC: the value k is what an HMAC_DRBG gives when instantiated with the private key as its entropy input and the reduced message hash as its nonce, with no personalization string | All six pass, each on the first candidate. P-521 needs 521 bits, so it runs two output blocks. Before they were embedded, the transcribed q, x and k were reproduced by a separate Python computation written from §3.2's steps |
| HMAC_DRBG, 145 instantiations with 74 reseeds, 629 requests and 79 requests with prediction resistance | `security/crypto/tools/hmac_drbg_reference.py`, an independent implementation in Python over `hmac`, which must reproduce the six RFC values before it writes anything | All pass. **The reference has the same author as the code**, so on reseeding, additional input and prediction resistance (A4, the second half of A5, A11) it shows that two implementations of one reading agree, not that the reading is the standard's |

RFC 6979 is Informational, not a standard, and it exercises instantiation and one request only. NIST's own vectors (N10, or the older set at N9) exercise the rest, and are the check that is missing.

## 10. What Rust 1.99.1 promises about overwriting memory (T1)

`core::hint::black_box`'s own documentation, in the pinned toolchain, says two things: the function is provided on a best-effort basis only, and it gives no guarantee for cryptographic or security purposes. The only write the language guarantees is never removed is `core::ptr::write_volatile`, which is `unsafe`. `core` has no safe function that overwrites memory with that guarantee.

What the pinned compiler does today, read from `origin-crypto`'s release object for `x86_64-unknown-none`: the two overwrite functions exist as functions of their own, each a `memset` followed by the opaque use, and every function of the generator that should call them does (the counts are in `SOURCE_PROVENANCE.md`). That is an observation of one build, not a promise.

## 11. Not read, and what waits on the owner

| Not read | Why | What rests on it |
|---|---|---|
| SP 800-90A Rev. 1's text (`NIST.SP.800-90Ar1.pdf`, nvlpubs.nist.gov, 2,417,007 bytes) | A PDF: a download, which needs the owner's word. Asked on 2026-11-10 | Every "author's knowledge" row of §4 |
| SP 800-90B's text (`NIST.SP.800-90B.pdf`, 1,043,696 bytes) and its errata (`sp800-90b_errata_potential_updates.pdf`, csrc.nist.gov, 107,606 bytes) | The same | All of §5; whether an erratum touches the health tests |
| SP 800-90C's text (`NIST.SP.800-90C.pdf`, 10,056,754 bytes) | The same | §6; the names and rules for a generator seeded from another |
| FIPS 198-1's text (`NIST.FIPS.198-1.pdf`, 129,454 bytes) | The same | Nothing in the code: RFC 2104 defines the same function and RFC 4231's values pass. The citation |
| NIST's HMAC_DRBG vectors (N10: 1,160,048 bytes) | The same; they would be converted and committed as decision item 19's were | An independent check of A4, A5's additional input, and A11 |
| UEFI 2.11 §37.5 | `uefi.org` refuses automated access | What the firmware's generator must deliver |
| The Intel SDM's and AMD APM's entries for RDRAND and RDSEED; AMD's account of its generator | PDFs | AMD's design; the exact instruction semantics beyond Intel's guide |

**One thing went wrong and is recorded here.** The first attempt to read SP 800-90A through the fetch tool did not return its text. The tool saved the PDF, twice, in this session's own tool-results folder under `~/.claude/projects/`, outside the repository. That was not intended. The copies were not opened, and the owner was told and asked whether to read or delete them.

## 12. Open-source references consulted (CLAUDE.md §3, §6; decision items 93, 96, 97)

| File | Project | Tag and commit | Licence | Read | Reason | Gap it filled |
|---|---|---|---|---|---|---|
| `MdePkg/Include/Protocol/Rng.h` (blob `d61f9e98`) | EDK II (item 94's list) | `edk2-stable202608`, `2970e5699ba6267f3384ffab20f96647578aebc8`, the newest stable tag on the day | BSD-2-Clause-Patent | 2026-11-10 | The specification could not be opened (§8), and the ADR has to say what the loader can ask the firmware for | The protocol's GUID |
| `MdePkg/Include/Guid/Rng.h` (blob `bf7956af`) | EDK II | The same | BSD-2-Clause-Patent | 2026-11-10 | The same | The two functions, their failures, the algorithm GUIDs |
| `OvmfPkg/Include/Dsc/OvmfRngComponents.dsc.inc` | EDK II | `edk2-stable202408` (`b158dad150bf02879668f72ce306445250838201`), which the test firmware is built from, and `edk2-stable202608` | BSD-2-Clause-Patent | 2026-11-10 | Research into what the reference firmware supports (item 97) | Whether the firmware 18b will be tested on offers the protocol at all |

No code, identifier or file structure was carried over: §8 states the interface in Origin's own words (item 121). This session wrote no loader code. The session that does, 18b, writes it from §8 and from the specification if the owner can supply it, without these files open (item 92).
