# Research ledger: the toolchain's path in the images, and compiler flags from the environment

| | |
|---|---|
| Date | 2026-11-11 |
| For | `developer-tools/xtask/src/toolchain.rs`, and the new check in the reproducible-build gate, `developer-tools/xtask/src/reproducible.rs` (TASKS item 32) |
| Requires | CLAUDE.md §75 and §60 (rebuild independently; build paths are tracked), §14 (frame pointers are enforced), §61, and §9 and §16 (verify the toolchain locally; research first, today's revisions) |
| Method | Experiments with the pinned toolchain on the development Mac, each built from nothing in its own target directory, at `main` `f654d71`. The rustc book and the Cargo book were read on the day from their publisher. No compiler, Cargo or operating-system source was opened (§3) |

## 1. What was found

Item 31's ledger (`2026-11-10-loader-reproducible-build.md` §9) found that both images hold the path the toolchain is installed at. At `f654d71`:

- the loader holds one such path, and the kernel eight;
- each is the source location of a panic in standard-library code that is compiled into Origin's crates, under `library/core/src/slice/` and `library/alloc/src/` (`vec`, `collections/btree`);
- each has the form `/Users/<account>/.rustup/toolchains/1.99.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/…`: the builder's account name and the build host's triple;
- the kernel also holds seven locations of the form `/rustc/<commit>/library/…`, which come from library code that was compiled when the toolchain was built.

So another account, or another machine, gives strings of another length and cannot build the same bytes; and an image that is released carries the account name of whoever built it.

## 2. Sources

| ID | Source | How it was obtained |
|---|---|---|
| R1 | The rustc book as published for Rust 1.99.1: "Command-line Arguments" (`--remap-path-prefix`, `--remap-path-scope`) and the chapter "Remap source paths": `doc.rust-lang.org/1.99.1/rustc/command-line-arguments.html` and `…/rustc/remap-source-paths.html` | Read on 2026-11-11 |
| R2 | The Cargo book as published for 1.99.1, "Configuration": `doc.rust-lang.org/1.99.1/cargo/reference/config.html`, for `build.rustflags`, how values from several sources are joined, `--config`, and the names of environment variables | Read on 2026-11-11 |
| R3 | Cargo's own source, one file: `src/compiler/build_runner/compilation_files.rs` in `github.com/rust-lang/cargo` at commit `5f94df4789f005f9a352888e8355ffc645b7ed0e`, which is the commit `cargo -vV` reports for 1.99.1. MIT OR Apache-2.0 | Read on 2026-11-12, for §12.6: how a crate's `-C metadata` value is made, which R2 does not say. A build tool's source (CLAUDE.md §4), read after step 2's code was committed; nothing of it is in Origin's code |
| R4 | The linker's own source, two files: `lld/ELF/SyntheticSections.cpp` and `lld/Common/Version.cpp` in `github.com/rust-lang/llvm-project` at commit `1b9c0d5ff9bbe7634aead059efe6b11a7eeba145`, the commit the pinned `rust-lld` names in what it writes. Apache-2.0 WITH LLVM-exception | Read on 2026-11-12, for §13.3: how the linker makes the string it writes into `.comment`. A build tool's source (CLAUDE.md §4); nothing of it is in Origin's code |
| C2 | CI's run 43248605239 at `c34fdab`: its `check` job's log, and the artifact `reproducible-gate-files` it kept | Read and fetched on 2026-11-12, for §13 |
| C3 | CI's run 43275652672 at `8ae9e9b`: its `check` job's log (job 131383211984), and the artifact `reproducible-gate-files` it kept | Read and fetched on 2026-11-12, for §16 |
| C1 | CI's logs for this repository: run 43201395354 at `f5ab54f` (its `check` and toolchain jobs) and run 43230979284 at `9e4f79f` (its `check` job) | Read on 2026-11-12, for §12.4 and §12.5 |
| T1 | The toolchain itself: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, commit `58cf8f9c35b355199727627d07ae34a4e2846d67`, and `cargo 1.99.1 (5f94df478 2026-08-27)`, on `aarch64-apple-darwin` | What `cargo build -v` passes to rustc, and what the images then hold (`strings`, `cmp`, `llvm-objdump`), each observed on 2026-11-11 |

What R1 says, in this ledger's words:

- `--remap-path-prefix FROM=TO` rewrites a path that starts with `FROM` so that it starts with `TO`, as text. It applies to what the compiler writes: diagnostics, debug information, and what `file!()` expands to, which is where a panic's location comes from. It is described as best effort.
- Given several times, the last one that matches applies.
- `--remap-path-scope` narrows it to some of those; without it everything is remapped.
- It does not reach what a linker writes by itself, and names the PDB as an example. The loader has had no PDB since item 31.
- R1 says nothing about the standard library's own source, the sysroot or `/rustc/<commit>`. What the compiler does there is §3's experiment.

What R2 says:

- Compiler flags come from one of four places, and the first that is present is used: `CARGO_ENCODED_RUSTFLAGS`; `RUSTFLAGS`; every matching `target.<triple>.rustflags`, joined; `build.rustflags`.
- With `--target`, those flags go to the target's own compilations and not to build scripts.
- A list set in several configuration sources is joined, the higher source's items last. `--config KEY=VALUE` on the command line is the highest.
- `target.<triple>.rustflags` can be set from the environment as `CARGO_TARGET_<TRIPLE>_RUSTFLAGS`, the triple in capitals with underscores.

## 3. The cause is the `rust-src` component

The loader with `test-trust-store` and the kernel, built five ways with plain `cargo`:

| Build | Loader | Kernel | Paths of this machine |
|---|---|---|---|
| as the repository built them | `874b6727…` | `c3964810…` | 1 and 8, under the toolchain's directory |
| with `--remap-path-prefix=<sysroot>/lib/rustlib/src/rust=/rustc/<commit>` | `f034ed8c…` | `eef10ba4…` | none |
| with a sysroot made of symbolic links to the toolchain's files, all but `lib/rustlib/src` | `f034ed8c…` | `eef10ba4…` | none |
| with a sysroot at another path that holds a real copy of `lib/rustlib/src`, and the remap for that path | `f034ed8c…` | `eef10ba4…` | none |
| the same sysroot, no remap | `dc686227…` | `66bbe08d…` | 1 and 8, under that sysroot |

So:

- With `rust-src` installed, the compiler names the standard library's source by where the files really are. Without it, it writes `/rustc/<commit>/library/…`.
- The remap gives, byte for byte, what the compiler writes when it has no local copy of that source. It adds nothing of its own.
- Source at another path gives other images (the last row against the first: 10,326 bytes of the loader differ and 66,435 of the kernel), and the remap for that path gives the same images again.

`rust-toolchain.toml` installs `rust-src` because CLAUDE.md §8 lists it. Nothing in the repository uses it.

## 4. The candidates

| Candidate | Result |
|---|---|
| No `rust-src` in `rust-toolchain.toml` | Gives the right images (row 3). But the images would then depend on what each machine has installed: rustup lets anyone add the component back, and editors ask for it. It also changes §8's list, which is the owner's. **The owner chose the remap (2026-11-11)** |
| Cargo's `trim-paths` profile option | Refused by Cargo 1.99.1: "feature `trim-paths` is required … not stabilized in this version of Cargo" |
| The remap written in `.cargo/config.toml` | Cannot be: the directory is another path on every machine, and that file has no way to ask the compiler for it |
| The remap computed by `xtask` and passed to every image build | **What the tree has** |

## 5. How `xtask` passes it

- `xtask` asks the pinned compiler once for its directory (`rustc --print sysroot`) and its commit (`rustc -vV`). If either cannot be read, or the commit is not 40 hexadecimal digits, it stops: it does not build an image without the remap.
- It adds `--config target.<triple>.rustflags=["--remap-path-prefix=<sysroot>/lib/rustlib/src/rust=/rustc/<commit>"]` for both freestanding targets to the three commands that build an image, or a library that goes into one: the loader's build, the kernel's build, and the library builds of the no-panic check, which share the kernel's compiled libraries. Cargo joins it to the flags `.cargo/config.toml` holds (seen in `cargo build -v`).
- If the directory is reached through a symbolic link, the path with the link followed is remapped as well. Which of the two the compiler writes is its own affair.
- The scope is left at everything. The target is `/rustc/<commit>`, because that is what the compiler writes by itself (§3), so an image is the same whether or not a machine has `rust-src`.
- A plain `cargo build` gets none of this, and its image holds the path. `.cargo/config.toml` says so.

## 6. Compiler flags from the environment

What reached rustc for the loader's binary, with each variable set (`cargo build -v`, Cargo 1.99.1):

| Variable | The repository's flags | The variable's flag |
|---|---|---|
| none | all there | — |
| `RUSTFLAGS`, set and empty | **none** | — |
| `RUSTFLAGS=-C debug-assertions=no` | **none** | there |
| `CARGO_ENCODED_RUSTFLAGS`, set and empty | **none** | — |
| `CARGO_ENCODED_RUSTFLAGS=…` | **none** | there |
| `CARGO_BUILD_RUSTFLAGS=…` | all there | not there |
| `CARGO_TARGET_X86_64_UNKNOWN_UEFI_RUSTFLAGS=…` | all there | there |

"The repository's flags" are `force-frame-pointers` and the loader's two link arguments; for the kernel they are `force-frame-pointers` and ADR-0017's two hardening options. So the first two variables silently drop what §14 and ADR-0017 require, even when they are empty, and a per-target variable adds flags the repository does not hold.

`xtask` now stops before it builds anything if `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, or the per-target variable of either image target is set. `CARGO_BUILD_RUSTFLAGS` is let through: it reaches no build that has flags for its target, and the images always do. `cargo xtask exclusive` runs with them set, because it builds nothing itself; an `xtask` it runs makes the check for itself. The Loom runner sets `RUSTFLAGS` on its own child process, for the host's test build, and that is untouched.

## 7. What changed in the images

At `f654d71`, with and without the remap:

| | Loader (`test-trust-store`) | Kernel ELF |
|---|---|---|
| File size | 175,616 bytes, both | 1,217,856 bytes, both |
| Code section | `.text`, `0x25ADF` bytes, both | `.text`, `0xBECF4` bytes, both |
| Instructions | 35,086, both, the same count of each | 182,152, both, the same count of each |
| Read-only data | `.rdata` 32 bytes shorter | `.rodata` 296 bytes shorter |
| Bytes that differ | 9,477 | 66,047 |

Each path is 37 bytes shorter on this machine (the kernel's eight make its 296), so what follows it in the read-only data moves, and every instruction that addresses that data holds another displacement. With addresses and displacements masked, the disassembly (`llvm-objdump -d`) is the same text in both, for both images.

## 8. What `xtask` itself showed

In one turn on the machine, at `dfa8281`:

- The gate passes, and prints these digests: the loader with no feature `6e85a3f3…019c0`, with `test-trust-store` `f034ed8c…436af`, with `developer-mode` `b0b43f7e…71a1f`, and the kernel ELF `eef10ba4…0d76`. The second and the last are §3's remapped builds, made there with plain `cargo`.
- **A second copy of the toolchain.** Its whole directory, 1.0 GB, was copied to another path. The copy's `rustc` reports that path as its directory. With the copy's `cargo` and `rustc` in use, the gate passes and prints the same four digests.
- **With the remap taken out of `xtask`,** the gate fails at the first image and names the toolchain's directory, the offset and the whole location found there. Run with the second copy, it names the copy's path. The two builds of that image were the same bytes each time: before this task the gate passed in that state.
- `cargo xtask check`, `boot-test` and `boot-matrix` pass (the provenance entry has the counts). The loader and the kernel that booted have the gate's digests.

## 9. Found, not changed

- **An image built by plain `cargo build` still holds the path.** Only `xtask` adds the remap. The gate, the boots and CI all build through `xtask`.
- **Other configuration files can still add flags.** Cargo reads `.cargo/config.toml` from every directory above the checkout, and from Cargo's home. A `target.<triple>.rustflags` there is joined to the repository's. `xtask` does not look for one.
- **The gate does not look for the bare home directory.** It looks for the toolchain's directory, Cargo's home and the checkout, each as named and with links followed. A home directory can be as short as `/root`, which an image's own text could hold.
- **A path under eight bytes is not looked for,** and the gate says which. `/src` is in every image, inside the source locations of Origin's own code.

## 10. Not verified

- **Another machine.** Everything here is one Mac. The second copy of the toolchain is the same files at another path, run by the same account on the same host.
- **A Linux build host.** CI builds there. Whether the compiler and the linker write the same bytes on Linux as on macOS, with the path out of the way, is not known. The gate prints each image's SHA-256, so the first CI run after this lands can be compared with the digests in the provenance entry.

## 11. Brought up to the batch (2026-11-12): the ring-3 build and the pinned commit

The batch (`land/s4` at `e51e098`) gained two things after this ledger was written: a kernel that carries a program built for ring 3 (task S4-1), and one file for the pinned compiler's commit (TASKS 22a). Each touches §5.

**A third way in.** The kernel includes the bytes of the program it carries (`include_bytes!` of what `onbf-pack carried` describes). That program is built for the userspace triple with the flags of `.cargo/userspace.toml`, which had no remap, and it held one location: `<sysroot>/lib/rustlib/src/rust/library/core/src/slice/index.rs`. The independent review of S4-1 found it (`docs/engineering/REVIEW_S4-1_2026-11-12.md`, F5). So with the batch merged, the gate of §8 fails on the kernel's image, as it should.

Shown with plain `cargo` on the merged tree, building `first-program` (29,784 bytes) as `xtask` does:

| Build | Paths of this machine | What it holds in their place |
|---|---|---|
| as the batch built it | 1, under the toolchain's directory | — |
| with `--config target.x86_64-unknown-linux-gnu.rustflags=["--remap-path-prefix=…"]` after `--config .cargo/userspace.toml` | none | `/rustc/<commit>/library/core/src/slice/index.rs` |

Cargo joins the second `--config` to the first's list, as R2 says of lists.

**What the tree has now.** `ring3::userspace()`, which every ring-3 build and clippy run goes through, passes `toolchain::userspace_config()`: the remap for the userspace triple alone. The freestanding builds do not name that triple, because on CI it is the host's own: R2 says a target's flags reach the host's build scripts when no `--target` is given.

**The commit comes from one file.** The remap's target is `/rustc/<commit>`. `xtask` now reads the commit from `developer-tools/toolchain/rustc-commit-hash`, the way CI's install step reads it (a `#` starts a comment, white space does not count, 40 lower-case hexadecimal digits must be left), and stops if the compiler in use reports another. The check matters to the images and not only to the record: the library code the toolchain ships compiled already holds `/rustc/<its own commit>`, so with another compiler an image would hold two.

**The refusal already covers the ring-3 build** (the review's F10). `xtask` checks the environment when it starts, before any command but `exclusive`, so `RUSTFLAGS` and `CARGO_ENCODED_RUSTFLAGS` cannot replace the list in `.cargo/userspace.toml` either. Not refused yet: `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS`, which would add flags to the ring-3 build and, on CI, to the host's.

**What `xtask` showed on the merged tree,** in one turn on the machine, at ``119d78d``:

- The gate passes in 40 s and prints: the loader with no feature, 196,096 bytes, `b02de0363fb0643bcc0f936ec40df7f88cb1a7eb19a193f1ef6b9b4adb1a8ccd`; with `test-trust-store`, 199,168 bytes, `b3a2e18497bbe03fb9dbaa9db20b2962c4815e5e0dbfdd9e88d2012cb60c74d7`; with `developer-mode`, 199,168 bytes, `6dfe435c44f1356fe1b99ee1b5de286439fdefe9d29ace0bce7287fde82b6baa`; the kernel ELF, 1,676,104 bytes, `e3c95ed22ae6ecd49edb0d7fb87fc6a06a093bc9a10a8db489b316fda1c025f3`.
- With a second copy of the toolchain's whole directory at another path, and that copy's `cargo` and `rustc` in use, it prints the same four.
- **With the remap taken out of the freestanding builds** (one line of `toolchain.rs`), it fails at the first image: the loader holds the toolchain's directory at offset 0x2c79d, in `…/library/core/src/slice/mod.rs`.
- **With the remap taken out of the ring-3 build alone,** the three loaders pass and the kernel fails: it holds the toolchain's directory at offset 0x142c4, in `…/library/core/src/slice/index.rs`, the location the carried program brings. So the search of the kernel's image covers the bytes of what it carries.
- Each of `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` and the two freestanding per-target variables stops `xtask` with exit 1; with `CARGO_BUILD_RUSTFLAGS` it runs.
- `cargo xtask check`, `boot-test` and `boot-matrix` pass (the provenance entry has the counts). The kernel that booted has the gate's digest.

With and without the freestanding remap, on the merged tree (plain `cargo`, the carried program remapped in both):

| | Loader (`test-trust-store`) | Kernel ELF |
|---|---|---|
| File size | 199,168 bytes, both | 1,676,104 bytes, both |
| Code section | `.text`, `0x2A13F` bytes, both | `.text`, `0x104F94` bytes, both |
| Instructions | 39,429, both, the same count of each | 245,061, both, the same count of each |
| Read-only data | `.rdata` 32 bytes shorter | `.rodata` 296 bytes shorter |
| Bytes that differ | 9,429 | 99,789 |

As in §7, the disassembly is the same text in both once addresses and displacements are masked. The remapped builds here have the digests the gate printed.

**What a CI run must show.** CI's `cargo xtask check` job prints the gate's four lines, each with an image's size and SHA-256. For the same commit:

- the four digests are the ones the development Mac prints: then the images do not depend on the host's system, its processor, the account or any path, for the first time on two machines;
- if one differs, the gate itself still passed on CI (two builds there agree and hold no path of that machine), so the difference is something a machine's own two builds share. The next step is to get both files and compare them as §7 does: CI would need to keep the image as an artifact of the run;
- the toolchain job has passed, so the compiler is the pinned one on both sides.

The digests change with every change to the kernel's or the loader's source, so the comparison is of one commit: the Mac's figures for the batch's tip are the ones the keeper's own `check` prints there, not the ones in this ledger.

**Still open after this:** a check of each carried program's own bytes before the kernel includes them, and the refusal of the userspace triple's variable. Both wait for S4-1's review fixes, which edit the same files.

## 12. Step 2 (2026-11-12): each carried program's own bytes, the other ways in, and CI

The coordinating session asked for this once S4-1's review answers were in the batch (`land/s4` at `9e4f79f`). Its contract is under "Task contracts" in `TASKS.md`.

### 12.1 What step 1 said too much about

§11 and step 1's matrix rows say the kernel "with the program it carries" is built twice and compared. The kernel was built twice. The program was built once: `ring3::programs()` built it in Cargo's own target directory, and both of the gate's kernels included that one build. So a carried program that differed from one build to the next would not have been seen. Nothing was found wrong with the program; the record said more than was shown.

### 12.2 What the gate does now

The gate has two target directories. In each it now builds, from nothing: the three loaders, each carried program, and then the kernel, which includes the program built in that same directory. For each program it reads six files from each directory: the file the linker wrote, the four segment files `onbf-pack carried` writes, and the description (`first_program.rs`). With the three loaders and the kernel that is ten files. Each pair must be the same bytes, and neither may hold the toolchain's directory, Cargo's home or the checkout.

After the kernel's pair, the gate looks for each segment file's bytes in the kernel's image and fails unless every one is there. So what it searched is what the kernel carries. In the real kernel each of the four occurs exactly once.

The two directories must hold the same file names, or the gate fails: a program that gained or lost a segment between two builds is a difference too.

### 12.3 Every way in that was tried

Each row was tried with Cargo 1.99.1 on the development Mac, with plain `cargo build -v` or with a small program put in the compiler's place that records each time it is run. "Reaches" means the build's `rustc` command changed.

| Way in | The ring-3 build | The loader and the kernel | `xtask` |
|---|---|---|---|
| `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` | replaces every flag of `.cargo/userspace.toml` | replaces every flag (§6) | refuses (step 1) |
| `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS` | reaches it; its flags go in front of the file's | not their triple | **refuses now** |
| `CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS`, `…_UEFI_RUSTFLAGS` | not its triple | reaches them (§6) | refuses (step 1) |
| `CARGO_BUILD_RUSTFLAGS` | does not reach it | does not reach them (§6) | lets through |
| `RUSTC`, `CARGO_BUILD_RUSTC` | run in the compiler's place: 11 times in one build | the same: 15 times in one build of the loader | **refuses now** |
| `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER` | each is run around the compiler | the same | **refuses now** |
| `CARGO_TARGET_X86_64_UNKNOWN_NONE_LINKER`, `…_UEFI_LINKER` | not its triple | replaces the linker | **refuses now** |
| `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` | does not reach it: the file's `-C linker=rust-lld` is used | not their triple | lets through |
| A configuration file above the checkout, or in Cargo's home, with `[target.x86_64-unknown-linux-gnu] rustflags`, `[target.'cfg(target_os = "linux")'] rustflags` or `[target.'cfg(all())'] rustflags` | reaches it; joined to the file's | the same, by their own triples or `cfg` | **does not look: a limit, below** |
| The same file with `[build] rustflags` | does not reach it | does not reach them | — |

Three notes on the table.

- **Why a compiler or wrapper variable is refused and not only reported.** `xtask` asks the `rustc` on the path for its directory and commit, and checks the commit against the pin. A variable that makes Cargo run another program for every compilation goes around that check: the commit would be the pinned one and the compiler would not.
- **Why the userspace triple's linker variable is let through.** It does not reach the ring-3 build, whose flags name the linker themselves. And on a Linux host it is the host's own setting, which a developer may need for host tools.
- **`xtask` itself is started by `cargo run`.** What Cargo and rustup add to its environment was listed with a three-line program: `CARGO`, `CARGO_HOME`, `CARGO_MANIFEST_DIR`, `CARGO_MANIFEST_PATH`, the `CARGO_PKG_…` variables, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN`, `RUSTUP_TOOLCHAIN_SOURCE` and `RUST_RECURSION_COUNT`. None is refused, and nothing in the repository or in CI's workflow sets one that is.

**The limit: a configuration file outside the repository.** Cargo reads `.cargo/config.toml` in every directory above the one it runs in, and in its home, and joins their lists to the repository's. Such a file can add flags to any image. `xtask` does not look for one, for three reasons:

- Cargo 1.99.1 has no stable command that prints the configuration in effect (`cargo config get` is unstable), so `xtask` would have to find and read the files as Cargo does, and would be wrong wherever it read them differently;
- this project's worktrees sit under the main checkout, so every worktree build already reads the main checkout's `.cargo/config.toml`. A rule against any file above the checkout would refuse the project's own layout;
- the gate cannot see such a file either, since both of its builds share it. What catches it is a comparison with a build made elsewhere: the digests would differ.

So the limit stands, and it is one more reason the comparison across machines matters. Ending it would mean `xtask` passing each image's complete flag list itself, so that nothing outside could add to it. That is a change to how every image is built, and is not proposed here.

### 12.4 What CI has shown, and what it has not

**No CI run has reached the gate on a tree that holds item 32.** The batch's pull request ran CI at `9e4f79f` (run 43230979284). Its `cargo xtask check` job failed in `xtask`'s own host tests, 242 passed and 1 failed: a test step 1 added, `the_ring_3_builds_get_the_remap_for_their_own_triple_alone`, asserted that no entry of the freestanding builds' configuration contains the userspace triple. On CI the toolchain's directory is named `1.99.1-x86_64-unknown-linux-gnu`, and the entry holds that directory. The test had only ever run on the Mac, whose toolchain is named for `aarch64-apple-darwin`. The keys were right on both; the test was wrong. It now compares keys, and a second test builds the entries from a directory named as CI's is (`9648878`).

`xtask`'s tests are the first tests `check` runs, after the format check and seven clippy runs. So for that tree CI ran nothing after them: not the other crates' host tests on an x86-64 host, not Loom, not this gate.

**The last digests CI printed are of `f5ab54f`** (run 43201395354), a tree that holds S4-1 and not item 32. Its gate was item 31's:

| Image | Size on CI | CI's SHA-256 |
|---|---|---|
| loader, no feature | 196,096 | `cbe34acdc6fcbe070f2aa012265ecd553ffbb6e4436d6c28dfd6daed8cd5ea6a` |
| loader, `test-trust-store` | 199,168 | `2fbd37a2142e15e48c0a8eb97b3ec95f9d2eea30bd57f4cee53c3f838d555b29` |
| loader, `developer-mode` | 199,168 | `d58cdc00943884584fa2fd12bee1a828f1ca466f48eb0dd0242669883444190b` |
| kernel ELF | 1,676,072 | `6808e6eca1e96a8939faf1a1cc23d2bc68eb242602eace201b6914c14c6b51ea` |

Those images hold the path of CI's toolchain, as the Mac's held the Mac's before item 32, so the two machines' digests for that commit could not be equal. They can still be used.

### 12.5 The Mac's builds of that commit, compared with CI's

This is the comparison across two machines that can be made today. It is of `f5ab54f`, the tree before item 32, with the Mac's build given the two things known to differ. Nothing was pushed for it.

**The first: the path.** In a copy of `f5ab54f`, each target's flag list in `.cargo/config.toml` and `.cargo/userspace.toml` got one more flag, which remaps the Mac's toolchain source directory to the path CI's has (`/home/runner/.rustup/toolchains/1.99.1-x86_64-unknown-linux-gnu/lib/rustlib/src/rust`). That commit's own `cargo xtask reproducible-gate` was then run in the copy, in the machine's turn.

**The second: the host the compiler reports.** §12.6 says why it matters. A stand-in was put in the compiler's place with `RUSTC`. It passes every command to the pinned `rustc` unchanged, and changes one thing: in the output of `rustc -vV`, the line `host: aarch64-apple-darwin` reads `host: x86_64-unknown-linux-gnu`. CI's toolchain job prints the same seven lines with that host. These builds were made with plain `cargo` at two jobs, with the commands `xtask` runs; with `rustc` as it is, the same commands give the digests of the copy's gate, for all four images.

| Image of `f5ab54f` | The Mac, with CI's path | The Mac, with CI's path and CI's host line | CI |
|---|---|---|---|
| loader, no feature | `cbe34acd…5ea6a` | `cbe34acd…5ea6a` | `cbe34acd…5ea6a` |
| loader, `test-trust-store` | `d42c6ec7…8685` | `2fbd37a2…5b29` | `2fbd37a2…5b29` |
| loader, `developer-mode` | `dc329a24…1bf3` | `d58cdc00…190b` | `d58cdc00…190b` |
| kernel ELF | `e247d280…11ac`, 1,676,104 bytes | `2f7ece6c…86e1`, 1,676,088 bytes | `6808e6ec…51ea`, 1,676,072 bytes |

What that shows:

- **The three loaders: the Mac writes CI's bytes, bit for bit,** once those two things are CI's. So for these three images, two things are all that a build takes from its host. Item 32 removed the first. The second is still there.
- **The loader with no feature does not follow the host line:** its bytes are CI's with the path alone.
- **The kernel is not explained.** The host line changes its bytes and moves its size 16 bytes toward CI's, and CI's is 16 bytes smaller again and another digest. Something else about the build on CI reaches the kernel. It was not found. Finding it needs CI's own file, to compare section by section, or a Linux machine to build on; neither was available.
- The carried program: its four segments are the same bytes with either host line. The file the linker wrote is not: it differs outside those segments. CI printed nothing for it at `f5ab54f`, so it was compared with nothing of CI's.

**What follows for a tree that holds item 32.** The path is no longer an input, so no stand-in for it is needed. The host line still is. So when CI's `check` reaches the gate on the batch, its digests are expected to differ from the Mac's for the two feature loaders and the kernel, and may agree for the loader with no feature. That is a prediction and has not been seen.

The full digests of the middle columns, for whoever repeats this: `d42c6ec7a84cf78bec15d60943ac4d569a197929c495399a758f813c76578685`, `dc329a24944a5eb4893942f1992a83d61ce78cbd59eb2d48836b2a64fe481bf3` and `e247d2800a26a94c02d3fc5e5be1b4ac28fb7ee2a29d071941a275374e4a11ac` with the path; `2f7ece6cb60b3f2276ecb136cbf6b6b6b61b94c63b392d7562fb1ff05f2086e1` for the kernel with the path and the host line. CI's are in §12.4.

For the batch's own tree the three loaders were built the same two ways, with the remap `xtask` passes (plain `cargo`, two jobs; with `rustc` as it is they have the gate's digests):

| Loader at `9648878` | The Mac: the gate's digest | The Mac with CI's host line: what CI should print |
|---|---|---|
| no feature | `b02de0363fb0643bcc0f936ec40df7f88cb1a7eb19a193f1ef6b9b4adb1a8ccd` | the same |
| `test-trust-store` | `b3a2e18497bbe03fb9dbaa9db20b2962c4815e5e0dbfdd9e88d2012cb60c74d7` | `77a858ada76471428a90b92b4a774392fcfa530d94a23512bda9c41b5677b8c1` |
| `developer-mode` | `6dfe435c44f1356fe1b99ee1b5de286439fdefe9d29ace0bce7287fde82b6baa` | `ee2a063f093fbbf4135f2b29155165d901d4c5b7bb29c20e915d36024e7e3eaf` |

If CI prints the right-hand column, the host line is the whole of the difference for the loaders on that tree too. No figure can be given for the kernel.

### 12.6 How the host gets in: Cargo's metadata, through a build script

Cargo gives every crate a `-C metadata` value. The compiler mixes it into each symbol's name and into the names of the pieces it compiles a crate in.

Cargo's source says how the value is made (`src/compiler/build_runner/compilation_files.rs` at Cargo's commit `5f94df4789f005f9a352888e8355ffc645b7ed0e`, the one `cargo -vV` reports for 1.99.1; MIT OR Apache-2.0; read on 2026-11-12 because the Cargo book does not say):

- it never includes the compiler flags, so the remap does not change it;
- it includes the lines of `rustc -vV`, but the `host:` line only for a unit built for the host, such as a build script;
- it includes the values of the units a unit depends on. So a crate that depends on a build script's run takes the host in through it.

Two crates in the images have a build script: `origin-version` (it writes the release's constants from `origin-version.toml`) and `framekernel` (it prints the kernel's link arguments). Seen with `cargo build -v` and the stand-in of §12.5:

- **Loader:** 8 of its 11 compilations get another `-C metadata` value with CI's host line: the build script, `origin_version`, and six more (`onbf_format`, `onbf_verify`, `origin_boot_info`, `origin_paging`, the loader's library and its binary). `origin_crypto`, `origin_random` and `onbf_merkle` keep theirs.
- **Kernel:** its symbol names carry each crate's value. Of the ten Origin crates named there, five get another with CI's host line (`framekernel`, `origin_abi`, `origin_boot_info`, `origin_frame`, `origin_paging`) and five keep theirs. With it, `.text` is the same size and other bytes, `.rodata` is 8 bytes shorter, and the string table is 15 bytes shorter: `origin_abi`'s value is one character shorter in each of the 14 names that hold it, and one piece's name is a character shorter.
- The remap changes no unit's value: every one is the same with and without it.

This agrees with item 31's ledger, where a build script given to the loader's own crate reordered its code.

**What would end it:** an image none of whose units depends on a unit built for the host. Then Cargo's value for each of them takes in nothing of the host. That means the two build scripts out of the images' dependency graphs: the constants and the link arguments supplied another way. It is not built here: it changes two crates this task does not own and every image's bytes, and it is put to the coordinating session as a proposal.

### 12.7 What `xtask` showed

All of it on `9648878`, the last commit that changes code, in one turn on the machine (14:06 to 14:56 UTC), except where a line says otherwise.

- **Before any of this step's code,** at `9e4f79f`, the gate passed in 37 s and printed four digests: the loader with no feature, 196,096 bytes, `b02de0363fb0643bcc0f936ec40df7f88cb1a7eb19a193f1ef6b9b4adb1a8ccd`; with `test-trust-store`, 199,168 bytes, `b3a2e18497bbe03fb9dbaa9db20b2962c4815e5e0dbfdd9e88d2012cb60c74d7`; with `developer-mode`, 199,168 bytes, `6dfe435c44f1356fe1b99ee1b5de286439fdefe9d29ace0bce7287fde82b6baa`; the kernel ELF, 1,680,416 bytes, `92b4debb5d71c70fbaf0e1eb7338dbd329f7e9860123c0027cce31c224e105dd`.
- **After,** the gate passes in 38 s (35 s on `22abf35`, which differs from `9648878` in one test) and prints ten files. The four above have the same digests: this step changed no image. The six new ones, all of `first_program`: as the linker wrote it, 29,784 bytes, `251e985c43ea78e3f6bf50c11d131c463c6157579a0dee3b13ded353bcb060c2`; `first_program.0.bin`, 4,200 bytes, `02fe296a23249a7ba579189834e0ead60b468d3e0966dec7c4c42abb3e51d841`; `.1.bin`, 10,170 bytes, `01f7addb33cac76c6aa8bf93d7607b2ab3ed30eb8455b684846fdf920fbad38c`; `.2.bin`, 640 bytes, `592ee8a478796bb0cc4274c7cb9336f57a6fa4f5c834b24389944e553bccdb14`; `.3.bin`, 24 bytes, `3341bcae4c97d7ba73e0728c783373725905eab0417cf220037920f0960679cb`; `first_program.rs`, 1,748 bytes, `276f45954297d6f94c6e42516ed26e338ebc4f773143896324a76da72ff71721`. Then: "the kernel ELF holds the bytes of each of the 4 carried segments the gate read".
- **With a second copy of the toolchain at another path,** and that copy's `cargo` and `rustc` in use, it prints the same ten.
- **With the remap taken out of the ring-3 build alone** (one line of `toolchain.rs`, put back with `git checkout` of that file), the gate fails after 25 s. The three loaders pass. It stops at "the carried program `first_program` as the linker wrote it holds a path of the machine that built it", the toolchain's directory at offset 0x891, in `…/library/core/src/slice/index.rs`. It prints nothing for the kernel: in step 1 this same control failed only once the kernel had been built.
- **Each refused variable stops `xtask` before it builds.** Fourteen settings, covering all thirteen variables (the userspace triple's flags variable both empty and not): each exits 1 within a second and names the variable, and the gate's directories were as they had been. With `CARGO_BUILD_RUSTFLAGS` or the userspace triple's linker variable set, `xtask` runs.
- `cargo xtask check` passes in 6 min 40 s, the gate included: 1,673 host tests, 0 failed, 27 ignored; Loom, all 53 models passed and all 78 negative controls were caught; the requirements, provenance, roadmap, progress, document, image, `unsafe`, assembly and interface gates each printed their line.
- `cargo xtask boot-test` passes on its first attempt, 11 of 11 scenarios in 129 s; its kernel boot ends `boot verdict: degraded: 63 of 64 proofs passed; failed: speculation controls`.
- `cargo xtask boot-matrix` passes on its first attempt, with no scenario repeated: 124 of 124 in 39 min 6 s. 21 of its full boots end with that verdict, which is the batch's under QEMU's software processor (decision item 154); the machine with neither random-number instruction ends `degraded: 58 of 60 proofs passed; 4 did not run; failed: speculation controls, random-number generator (no randomness)`, which is what `boot-random` asks of it.
- The kernel that booted has the gate's digest and holds no path under `/Users` or `/private`. Each of the four segment files occurs in it exactly once (at 0x13b4b, 0x14bb3, 0x1736d and 0x175ed).
- `xtask`'s own tests: 248 pass and 1 is ignored. On a copy of `9e4f79f` with `9648878` alone applied, 244 pass.

### 12.8 CI keeps the files the gate read (added the same day)

The coordinating session asked for this before step 2 lands, so that the first CI run to pass `check` already keeps CI's kernel file, which §12.5 could not get.

- **The gate leaves a copy of every file it read,** from each of its two builds, in `target/xtask/reproducible/files/first/` and `…/second/`: `loader`, `loader-test-trust-store`, `loader-developer-mode`, `carried-first_program-…` (six) and `kernel`. The copies are needed because the loader's three builds write one path in a target directory. The directory is emptied when the gate starts, and each build's file is copied before the two are compared, so a file that differs is there from both builds. About 2.3 MB for each build.
- **CI's `check` job uploads that directory** as the artifact `reproducible-gate-files`, with the action and the pinned commit the workflow already uses, kept for 14 days. The step runs when `check` fails too. If `check` failed before the gate there is nothing to upload, which is a warning; and the step is marked so that it cannot decide whether the job passes. What CI gates on is unchanged.
- The ten digests are already in the job's log: the gate prints one line for each file.

On the Mac, at `0230c4c`: `cargo xtask check` passes in 6 min 13 s (1,674 host tests, 0 failed, 27 ignored; Loom 53 models, 78 controls). The gate prints the same ten digests as in §12.7 and leaves ten files for each build, 2.3 MB each, the second build's the same bytes as the first's; the kept kernel and loaders have the digests it printed. No boot was run for this commit: no image changed.

Not seen yet: the step has not run on CI. Its first run is the one on a head that holds it.

### 12.9 Not verified

- **No image of a tree that holds item 32 has been compared across two machines.** §12.5 is of the tree before it, with the Mac given CI's path. The direct comparison needs a CI run whose `check` job reaches the gate; the Mac's ten digests for the batch's images are in §12.7.
- **Why CI's kernel differs from the Mac's with CI's path and host line** (§12.5). Only its size and digest are known.
- Whether taking the build scripts out (§12.6) makes the feature loaders the same on both kinds of host. It follows from Cargo's source and from §12.5; it was not built and tried.
- The stand-in changes what Cargo is told about the host and nothing about the compiler. A compiler that really runs on Linux was not available: what one does is known only from CI's logs.
- The refusals were tried one variable at a time. Combinations were not.
- The configuration-file routes were tried with a file one directory above a copy of the checkout and with one in a stand-in for Cargo's home. A file that Cargo finds some other way was not looked for.

## 13. The first comparison with CI on a tree that holds item 32: `c34fdab` (2026-11-12)

The batch was pushed at `c34fdab`, whose tree is `41c03c2`'s: item 32 with step 2 and the kept files (§12.8), and both build scripts still in place. CI ran on it (run 43248605239, on the batch's pull request).

### 13.1 What ran on CI for the first time

- `cargo xtask check` passed there: 1,675 host tests, 0 failed, 27 ignored (the Mac runs 1,674: `origin-rt-mem`'s test of the direction flag is compiled only for an x86-64 host, and passed there); Loom, all 53 models and all 78 controls. Nothing after `xtask`'s own tests had run on CI for a tree with item 32 before this (§12.4).
- The reproducible-build gate passed on CI: each of the ten files was the same from CI's two builds, and free of that machine's paths. The check job's machine was an AMD EPYC 7763 with 4 processors, on the `ubuntu-24.04` image.
- The step that keeps the gate's files ran. The artifact `reproducible-gate-files` is 1,839,090 bytes as uploaded and holds the ten files from each build, 4.5 MB unpacked. The kernel in it has the digest the log printed, and each file of the second build is the same bytes as the first's.

### 13.2 The ten files, the Mac's against CI's

The Mac's are the gate's at `41c03c2`, and the keeper's own ten for `c34fdab`, built in another checkout on the same Mac, are the same. "With CI's host line" is the Mac's build with the stand-in of §12.5, made before CI's result was read.

| File | The Mac | The Mac with CI's host line | CI | The Mac's and CI's |
|---|---|---|---|---|
| loader, no feature | `b02de036…` | `b02de036…` | `b02de0363fb0643bcc0f936ec40df7f88cb1a7eb19a193f1ef6b9b4adb1a8ccd` | **the same** |
| loader, `test-trust-store` | `b3a2e184…` | `77a858ad…` | `77a858ada76471428a90b92b4a774392fcfa530d94a23512bda9c41b5677b8c1` | differ |
| loader, `developer-mode` | `6dfe435c…` | `ee2a063f…` | `ee2a063f093fbbf4135f2b29155165d901d4c5b7bb29c20e915d36024e7e3eaf` | differ |
| `first_program` as the linker wrote it | `251e985c…`, 29,784 bytes | `daf14f35…`, 29,784 bytes | `5dedc72efb3b8b51bace0bbae197115fa74e0f8af7b386741a9f0d80853cd54b`, 29,768 bytes | differ |
| `first_program.0.bin` | `02fe296a…` | `02fe296a…` | `9e35a5739dc0e0133499a77bd12430720f0788a30c50e7f39e27b4b3c68dbf16` | differ, in one byte |
| `first_program.1.bin` | `01f7addb…` | `01f7addb…` | `01f7addb33cac76c6aa8bf93d7607b2ab3ed30eb8455b684846fdf920fbad38c` | **the same** |
| `first_program.2.bin` | `592ee8a4…` | `592ee8a4…` | `592ee8a478796bb0cc4274c7cb9336f57a6fa4f5c834b24389944e553bccdb14` | **the same** |
| `first_program.3.bin` | `3341bcae…` | `3341bcae…` | `3341bcae4c97d7ba73e0728c783373725905eab0417cf220037920f0960679cb` | **the same** |
| `first_program.rs` | `276f4595…` | `ac996eb1…` | `1f825d08192d07695f9a8d1d57fd2643770c03f43105b268868215e325466047` | differ |
| kernel ELF | `92b4debb…`, 1,680,416 bytes | `5168a904…`, 1,680,400 bytes | `90e7505387cd209a8d35529a6eea4bdaf07e69860eb32ebdbc1947009574b032`, 1,680,384 bytes | differ |

**Four of the ten are the same bytes on the two machines, and six are not.**

**The three loaders came out as §12.5 predicted**, digest for digest: CI's plain loader is the Mac's, and CI's two feature loaders are the ones the Mac writes with CI's host line. So for the loaders the host line was the whole difference on this tree too.

The carried program and the kernel did not: with CI's host line the Mac's are still not CI's. That is the difference §12.5 could not explain at `f5ab54f`. With CI's own files it can be found.

### 13.3 The third thing a build takes from its host: the linker's name for itself

CI's `first_program` and the Mac's with CI's host line were compared section by section. Nineteen of the twenty sections are the same bytes, the code, the data and the symbol table among them. One differs, `.comment`:

- the Mac's linker writes `Linker: LLD 23.1.1 (https://github.com/rust-lang/llvm-project.git 1b9c0d5f…)`;
- CI's writes `Linker: LLD 23.1.1 (/checkout/src/llvm-project/llvm 1b9c0d5f…)`.

That text is fixed when the linker itself is built (R4): its version, and the repository and revision its build was configured with. The pinned toolchain's `rust-lld` for macOS and the one for Linux were built from the same revision and name the repository differently. CI's string is 14 bytes shorter.

`.comment` is not loaded. But it moves what is after it in the file, and that reaches loaded bytes in one place: the ELF header holds the offset of the section table (`e_shoff`), and the first loadable segment holds the ELF header. So:

- `first_program` as linked: `.comment` differs, the file is 16 bytes shorter on CI, and one byte of its header differs (offset 0x28);
- `first_program.0.bin`, the segment the kernel carries, differs in that one byte;
- the kernel: its own `.comment` and one byte of its own header differ in the same way, and one byte of its read-only data differs, which is the program's header byte inside the segment it carries. Its other 18 sections are the same bytes as the Mac's with CI's host line;
- `first_program.rs` quotes the linker output's digest in a comment, so it differs too;
- the loaders are PE images, have no `.comment`, and are not touched by this.

**It reaches what boots.** `onbf-pack` converts the kernel's ELF to the image the loader starts. Converted on the Mac, CI's kernel and the Mac's with CI's host line give two images of the same size that differ in 225 bytes: the two header bytes, and the digests the image holds over them.

### 13.4 The three together are all of it

The linker reads an environment variable, `LLD_VERSION`, in place of its own text (R4; the source says it is for testing). Step 2's tree was built on the Mac once more, with plain `cargo` at two jobs, with CI's host line and with `LLD_VERSION` set to CI's string.

**All ten files are then CI's, bit for bit**: the three loaders, the program's six files, and the kernel.

So on this tree a build took exactly three things from its host:

| What | How it reaches an image | State |
|---|---|---|
| Where the toolchain is installed | the standard library's source paths, in panic locations | removed by item 32 (§5, §11) |
| The host the compiler reports | Cargo's `-C metadata` values, through a build script | removed by item 44 (§14); not yet compared with CI |
| The linker's name for itself | `.comment` of each ELF, and through `e_shoff` one loaded byte of each | still there when this was written; ended since by item 44 (§14.5), and not yet compared with CI |

Nothing else was found: no difference in generated code, in data, or in symbol names between the Mac's compiler and linker and CI's, for these ten files.

### 13.5 What could end the third

Not built when this was written; put to the coordinating session, which made it part of item 44. §14.5 is what was built.

- Cargo's `[env]` configuration can set `LLD_VERSION` for every link. Tried on the Mac with `--config env.LLD_VERSION=…`: the program's `.comment` then holds the given text and the compiler's version, and nothing of the linker's own. One entry in `.cargo/config.toml` would do it for `xtask` and for plain `cargo` alike.
- What it rests on: a variable the linker's source calls a testing aid. A gate that reads `.comment` in each ELF it builds would fail on any machine if a later linker stopped honouring it.
- Other ways: remove `.comment` from the linker's output with a second tool; or have `onbf-pack` not carry the section table's fields of an ELF header. Each changes more than one entry does.

### 13.6 Not verified

- **No file of a tree with item 44 has been compared with CI's.** §14 has the Mac's side.
- With the three inputs set as CI's, the Mac gives CI's bytes; that the reverse holds, CI giving the Mac's, was not tried.
- One CI machine. The `check` job runs on one runner image, so whether another image's toolchain writes the same bytes is not known from this run. At `f5ab54f` the job ran on another processor (an EPYC 9V45) and its loaders fit the same account.
- Whether `LLD_VERSION` behaves the same in the linker's COFF driver was not looked at; the loaders have no `.comment`.

## 14. Item 44 (2026-11-12): no unit of an image is built for the host

§12.6 found one way the build host gets into an image: Cargo's `-C metadata` value for a unit built for the host takes in the host the compiler reports, and each unit takes in the values of the units it depends on. Two crates under the images had a build script. This section is what was done about it. The contract is "44" under "Task contracts" in `TASKS.md`. §13, written while this was being built, found one more way, and §14.5 ends that too.

### 14.1 What was changed

- **The release's constants** were written at build time by `runtime/version/build.rs`. They are now a file in the tree, `runtime/version/src/generated.rs`, which the crate includes. `cargo xtask version` writes it from `origin-version.toml`, with the same parser and the same text the script used, now in `runtime/version/manifest.rs`. Its first line says what wrote it. It is a function of `origin-version.toml` alone.
- **A gate, `version-gate`,** in `cargo xtask check`: the committed file must be exactly what the generator writes from `origin-version.toml`. When it is not, the gate names the first line that differs and the command that rewrites the file.
- **The kernel's link arguments** were printed by `framekernel/build.rs` for the kernel's binary. The same nine, in the same order, are now in `.cargo/config.toml` under the kernel's target, where the loader's two have been since item 31. The kernel is the one binary built for that target.
- **Both build scripts are removed.**
- **The linker's line about itself is fixed** (§14.5), added after §13 found it: one entry in `.cargo/config.toml`, and a check in the reproducible-build gate.
- **A gate, `image-graph-gate`,** in `cargo xtask check`: no package that the loader, the kernel or a carried program is built from may have a build script or be a procedural macro. Which packages have such a target is read from Cargo's own account (`cargo metadata`), and what each image is built from, from its dependency tree with every feature on (`cargo tree`, normal and build dependencies).

### 14.2 Nothing was lost: the transitional commit

Removing a build script changes Cargo's values, and with them the order of an image's code (item 31's ledger, §6). So the bytes cannot be compared across the removal. They can be compared across a step that keeps the scripts.

`62d045a` moves the constants and the link arguments and leaves both build scripts in place, empty. While a script exists, Cargo gives every crate the value it had. At that commit the reproducible-build gate passes, and **each of the ten files it keeps is the same bytes as the one kept at `41c03c2`**: the three loaders, the carried program's six files, and the kernel. So every constant has the value it had, and every link argument reaches the linker as it did.

The generated file itself is the script's output with another first line: `cargo xtask version`, run on a file made that way, reported it already right.

### 14.3 What the removal changed in the images

`da2b82e` removes the scripts. Each image was compared with the one built before it. Sizes did not change.

| File | Before (`41c03c2`) | After (`da2b82e`) |
|---|---|---|
| loader, no feature, 196,096 bytes | `b02de0363fb0643bcc0f936ec40df7f88cb1a7eb19a193f1ef6b9b4adb1a8ccd` | `b3ee60b9173235fdc38d708a2c95e14e3c065abdc8d89b28231c693a787dd6d2` |
| loader, `test-trust-store`, 199,168 bytes | `b3a2e18497bbe03fb9dbaa9db20b2962c4815e5e0dbfdd9e88d2012cb60c74d7` | `a9f7ddf8536b6cef49e81af38bc3dd70b2788e9baedd6d26cde2c85013830905` |
| loader, `developer-mode`, 199,168 bytes | `6dfe435c44f1356fe1b99ee1b5de286439fdefe9d29ace0bce7287fde82b6baa` | `ee2a063f093fbbf4135f2b29155165d901d4c5b7bb29c20e915d36024e7e3eaf` |
| `first_program` as the linker wrote it, 29,784 bytes | `251e985c43ea78e3f6bf50c11d131c463c6157579a0dee3b13ded353bcb060c2` | `bd2f3f3c4f627b762ca576f4773e7082f476fe8420d0014d498410786c800f30` |
| its four segment files | unchanged | the same bytes |
| its description, 1,748 bytes | `276f45954297d6f94c6e42516ed26e338ebc4f773143896324a76da72ff71721` | `cf54ca01bc92e1f19738983e3873b6f14b01fa530cf23f4257fd2691d1be4f22` |
| kernel ELF, 1,680,416 bytes | `92b4debb5d71c70fbaf0e1eb7338dbd329f7e9860123c0027cce31c224e105dd` | `1f3d05341758da0174c7997ec7ad24708dd05e082b68addd70d6a632cdfc8537` |

**The kernel** (the toolchain's `llvm-readobj`, `llvm-nm` and `llvm-objdump`):

- its 20 section headers are the same, field for field: name, type, flags, address, offset, size and alignment;
- its 9 program headers are the same, field for field. So the layout, the segment permissions, RELRO and the entry are as they were;
- 1,644 symbols, with the same names once the hashes are taken out by demangling, the same kinds and the same sizes. Every data symbol is at the address it had. 132 functions are at another address;
- 245,551 instructions, 192 kinds, the same count of each;
- each of its 1,332 functions has the same instructions, in the same order, once what depends on placement is masked: a direct branch's target and a displacement from `%rip`;
- 90,080 of its bytes differ: 40,580 in `.text` (the displacements and targets), 49,282 in the symbol and string tables (the names), 146 in `.rodata` and 72 in the relocations.

So it is the same code with 132 functions in another order. **Performance cost, in ADR-0018 V3's first form: none, by construction**: no instruction was added, removed or changed, and no code generation flag changed.

**The loaders:** the same five sections with the same sizes, and the same count of each instruction (39,415, 39,428 and 39,427). In two of them `.rdata`'s used length differs by 8 bytes within the same size on disk.

**The carried program:** its four segments are the same bytes, so the kernel carries what it carried. The file the linker wrote has the same sections, program headers, symbols at the same addresses and instructions; the names in its symbol table changed. Its description differs in one comment line, which quotes that file's digest.

One thing seen and not looked into: the new `developer-mode` loader is, byte for byte, the file §12.5 got from the old tree with CI's host line.

### 14.4 With the compiler reporting CI's host

The stand-in of §12.5 again: plain `cargo` at two jobs, with the `--config` entries `xtask` passes, and a `rustc` that differs only in the `host:` line of `rustc -vV`. At `da2b82e`:

- with `rustc` as it is, all ten files are the gate's bytes, which checks the method;
- **with CI's host line, all ten files are the gate's bytes too.**

Before item 44 the same change gave other bytes for the two feature loaders, the kernel and the program's linker output (§12.5, §12.6). So what Cargo is told about the host no longer reaches any of the ten files.

**That is a prediction, not a comparison with CI.** The stand-in changes what Cargo is told and nothing about the compiler or the linker, which on CI are other programs built for another machine. §13 shows what that can hide, and what it hid: the linker's name for itself.

### 14.5 The linker's line about itself, fixed

§13 found the third thing a build took from its host while this task was being built, and the coordinating session put ending it into this task, so that the images' bytes change in one merge.

- **The entry.** `.cargo/config.toml` has `[env] LLD_VERSION = { value = "Linker: rust-lld of the toolchain that rust-toolchain.toml pins (text from .cargo/config.toml)", force = true }`. The linker writes that in place of its own line (R4). The text names the linker as the pinned toolchain's and holds no version, so a toolchain upgrade does not make it false; the compiler's own line beside it, `rustc version 1.99.1 (58cf8f9c3 2026-10-13)`, carries the version and is the same on both hosts.
- **One place.** It is Cargo's own configuration, so plain `cargo` and `xtask` get the same line, and `xtask` passes nothing for it. `xtask`'s own `--config` entries, for the remap and for the ring-3 flags, are joined to the file and do not replace it: the gate reads the line in the kernel's ELF and in the program's, which are built with those entries.
- **Which wins,** tried with Cargo 1.99.1 on the carried program:

  | Setting | The line in `.comment` |
  |---|---|
  | nothing set | the linker's own |
  | an entry in the file, not forced | the entry's |
  | the same, and `LLD_VERSION` set outside | **the outside value** |
  | a forced entry in the file, and `LLD_VERSION` set outside, or set and empty | the entry's |
  | a forced entry, and a forced entry in a file above the checkout | the checkout's entry |
  | a forced entry, and `--config env.LLD_VERSION="…"` on the command line | Cargo refuses to merge the two, and builds nothing |

  So the entry is forced, and `xtask` does not need to refuse the variable: a value in the environment cannot take the entry's place. The gate fails if the entry is not forced.
- **The loaders** are PE images. The linker's COFF driver writes no such line: none of the three holds `LLD`, `Linker:` or the compiler's version, and each is the same bytes with the entry as without.
- **The check.** For each ELF the reproducible-build gate builds, the kernel's and each carried program's linker output, `.comment` must hold the fixed line, and may hold besides it only the compiler's line. The linker's own line, or any other, fails it. And none of the ten files may hold the linker's own line anywhere. The fixed line is read from `.cargo/config.toml`, and an entry that is missing or not forced fails the gate before anything is built.
- **What it did to the images** (`3761692`): the three loaders and the program's segments 1, 2 and 3 are the same bytes as before the entry. The rest changed once more:

  | File | Before the entry (`a81af90`) | With it |
  |---|---|---|
  | `first_program` as the linker wrote it | `bd2f3f3c…`, 29,784 bytes | `8a6658ab4662300661306886431348de8a1346c084bd61acda1901b30052b627`, 29,768 bytes |
  | `first_program.0.bin` | `02fe296a…` | `9e35a5739dc0e0133499a77bd12430720f0788a30c50e7f39e27b4b3c68dbf16`: one byte, the section table's offset |
  | `first_program.rs` | `cf54ca01…` | `364739ab7e1b18462e7fffeefe1a0e33282e1d795a19cd271fdef08487991bc5`: the line that quotes the linker output's digest |
  | kernel ELF | `1f3d0534…`, 1,680,416 bytes | `897a6d0fc43a2cae92342f69da35a8b3d34bda30df3f60637bbf66c9858b178a`, 1,680,400 bytes |

  No code or data changes: the line is 16 bytes shorter after alignment, and what differs is `.comment`, the offset in each ELF header, and the one byte of the program's header that the kernel carries.
- **With the compiler reporting CI's host,** all ten files are the same bytes as without (plain `cargo`, two jobs).
- **An existing target directory.** Cargo does not link a file again when `[env]` changes: nothing it tracks for that file changed. Tried: a program and a kernel built without the entry, then built again with it, are not linked again, and keep the linker's own line. `3761692`'s own run showed what follows. The ordinary target directory still held `first-program` as linked before the entry; the kernel built there carried its old header; and `boot-test` booted a kernel one byte off the gate's (`50ccb41e…` where the gate printed `897a6d0f…`). The gate builds from nothing and was right. `68ed2b1` makes every `xtask` build of a carried program or of the kernel read `.comment` of what it linked: a file without the fixed line is removed, with Cargo's own copy of it under `deps/`, and linked once more, and if it is still wrong that is an error. On that directory `cargo xtask build` then linked `first-program` again and said so, and the kernel there was the gate's, byte for byte; a second build linked nothing again. **What this path has been exercised by:** a host test, and that one run on that one stale directory, in this session's checkout. Nowhere else. The keeper's first build on the merge printed no such note: the merge itself changed what the kernel and `first-program` are built from, so Cargo linked both again anyway and there was no stale file for `xtask` to find.

### 14.6 The controls and the gates

**On `a81af90`,** the last commit before the linker's line, in one turn on the machine (15:37 to 16:27 UTC):

- **`framekernel/build.rs` put back** (an empty script, removed again afterwards): `image-graph-gate` fails, "the kernel is built from `framekernel`, which has a build script (framekernel/build.rs)".
- **A build script given to the loader's own package:** it fails, "the loader is built from `origin-boot-uefi`, which has a build script (boot/uefi/build.rs)".
- With the two original scripts still in place, before `da2b82e`, it named both, for the loader, the kernel and the carried program: `origin-version`'s under all three and `framekernel`'s under the kernel.
- **A value changed in `origin-version.toml` and the file not rewritten** (`boot-info` 0.6 to 0.7, put back with `git checkout`): `version-gate` fails, names `cargo xtask version`, and shows line 24 as generated and as committed.
- `cargo xtask check` passes in 7 min 12 s, both new gates and the reproducible-build gate included: 1,683 host tests, 0 failed, 27 ignored; Loom, all 53 models and all 78 controls. The gate's ten digests are those of §14.3.
- `cargo xtask boot-test` passes on its first attempt, 11 of 11 scenarios in 129 s; its kernel boot ends `boot verdict: degraded: 63 of 64 proofs passed; failed: speculation controls`.
- `cargo xtask boot-matrix` passes on its first attempt, with no scenario repeated: 124 of 124 in 40 min 31 s. 21 of its full boots end with that verdict (decision item 154), and the machine with neither random-number instruction ends `degraded: 58 of 60 proofs passed; 4 did not run; failed: speculation controls, random-number generator (no randomness)`, which is what `boot-random` asks of it.

At `da2b82e` `check` had failed, in clippy over `origin-version`'s tests: the generator's function, moved out of the build script, was used by `xtask` and by nothing in the crate's own test build. `a81af90` gives the crate the test it lacked: the file it includes is, to the byte, what the generator writes. No image changed between the two commits.

**On `3761692`,** the commit with the linker's line, in one turn (16:28 to 16:37 UTC): the two controls below, the gate, `check` (1,687 host tests, 0 failed, 27 ignored) and `boot-test` (11 of 11) passed. Its `boot-test` is not evidence for the gate's kernel: it booted the kernel one byte off, as §14.5 says.

**On `68ed2b1`,** the last commit that changes code, in one turn (16:41 to 16:50 UTC):

- **The entry taken out of `.cargo/config.toml`** (put back with `git checkout`): the reproducible-build gate fails at once, "`.cargo/config.toml` has no `LLD_VERSION = { value = "...", force = true }` under `[env]`", and builds nothing.
- **The entry there and not forced:** it fails at once, naming `force = true`, and builds nothing.
- **The check run by hand** (`cargo xtask linker-text-gate`) on files built before the entry: the Mac's kernel at `a81af90` is refused, naming the line the Mac's linker writes; CI's kernel and CI's `first_program` from run 43248605239 are refused, naming the line CI's linker writes. A loader, which is not an ELF, is an error and not a pass.
- The reproducible-build gate passes in 34 s, with the ten digests of §14.3 (the loaders) and §14.5, and prints for the program's linker output and for the kernel that `.comment` holds the fixed line and not the linker's own.
- `cargo xtask check` passes in 6 min 11 s: 1,688 host tests, 0 failed, 27 ignored; Loom, all 53 models and all 78 controls.
- `cargo xtask boot-test` passes on its first attempt, 11 of 11 scenarios in 127 s; its kernel boot ends `boot verdict: degraded: 63 of 64 proofs passed; failed: speculation controls`.
- After the boots, the kernel and the program in the ordinary target directory are the gate's, byte for byte.
- `xtask`'s own tests: 261 pass and 1 is ignored.

The boot matrix was not run after `a81af90`: the coordinating session set `check` and `boot-test` for the later commits, and the keeper runs all three on the merge. Between `a81af90` and `68ed2b1` the kernel differs in `.comment`, one header byte and one byte of read-only data.

### 14.7 The three things a build took from its host, and what was not looked for

Said plainly, for release work.

| What | Ended by | How it is kept so |
|---|---|---|
| Where the toolchain is installed | item 32: `xtask`'s remap to `/rustc/<commit>` | the gate searches every file for the toolchain's directory, Cargo's home and the checkout |
| The host the compiler reports | item 44: no unit of an image is built for the host | `image-graph-gate` |
| The linker's name for itself | item 44: the fixed line | the gate reads `.comment` in every ELF it builds |

With CI's values for the three, the Mac built all ten of CI's files at `c34fdab` bit for bit (§13.4). So for those ten files, on these two hosts, there was no fourth. **That is the evidence there is; it is not a proof that no other host would show one.**

Expected when this was written, and seen since (§16): for a tree with item 44, CI prints the Mac's ten digests, the ones in §14.3 for the loaders and §14.5 for the rest.

What was not looked for:

- **Another kind of host than these two.** One Mac with an Apple processor, and CI's Linux machines with x86-64 processors of two models. No Windows host, no Linux host with another processor family, no other release of macOS or of the runner's image.
- **Another build of the same toolchain version.** Both hosts have rustup's own builds of 1.99.1. A distribution's own build of the same version, or a linker of another version under the same compiler, could differ in ways these did not; the gate's check of `.comment` would catch another linker line and nothing else about it.
- **The other runner image.** Only the `check` job, on `ubuntu-24.04`, prints digests.
- **The image that boots.** It is `onbf-pack`'s conversion of the kernel's ELF, and the gate compared the ELF. §15 puts the converted image in the gate; CI's conversion has not been seen.
- **A configuration file outside the repository** that adds compiler flags (§12.3). Its `[env]` entries cannot replace the fixed line.

### 14.8 For a toolchain upgrade

Three things here rest on how the pinned tools behave. CLAUDE.md §15 asks for a compiler upgrade to be checked before it is taken; these are three of the checks, and what catches each.

- **`LLD_VERSION`.** The linker's source calls it an aid for testing. If a later linker does not read it, the linker's own line is back in `.comment`, and the reproducible-build gate fails on every machine, naming the line. So does every `xtask` build of the kernel or of a carried program.
- **Cargo's `-C metadata`.** If a later Cargo takes the host into the value of a unit that is not built for the host, the images follow the host again and no gate on one machine sees it: only the comparison of the Mac's digests with CI's does. That comparison has to be made again after an upgrade.
- **The standard library's source paths.** The remap's target, `/rustc/<commit>`, is checked against the pinned commit, and the gate searches for the path.

### 14.9 Not verified

- When this was written, no file of a tree with item 44 had been built on CI, and it was not known that a linker really running on Linux honours the entry. Both are seen since: §16.
- No human has reviewed any of this, and nothing ran on physical hardware.

## 15. Item 44, step 2 (2026-11-12): the image that boots, in the gate

§14.7 listed one thing no gate covered: the loader starts the image `onbf-pack krn` converts the kernel's ELF to, and the gate compared the ELF. The coordinating session asked for it as this task's next piece. Its contract is "44, step 2" under "Task contracts" in `TASKS.md`.

### 15.1 What the gate does now

Each of the gate's two builds converts its own kernel ELF, with the command `cargo xtask build` and the boots use, into its own directory. The converted image is an eleventh file, treated as the other ten are: the two builds' must be the same bytes; neither may hold a path of the machine or the linker's own line; its digest is printed; and a copy of each is kept under `files/`, as `kernel-image`, so CI's artifact holds it. The check that the kernel carries its programs' segments still reads the ELF.

On the Mac, at `3fe5aef`: 1,545,848 bytes, `239f308b539b85e2b28d14ee066ca9652cc8c021600f2f7490421bba9a6f5c92`. After a boot test, the image the boots used (`target/xtask/KERNEL.ONBF`) is that file, byte for byte. The other ten digests are unchanged.

### 15.2 Signing, and why no signed image is in the gate

The converted image is unsigned. Signing is a separate step with a key: the boot tests do it with test keys, and a release does it in release infrastructure (CLAUDE.md §59).

A signed image is never the same twice, by design. `onbf-pack`'s own account (`developer-tools/onbf-pack/src/sign.rs`): the ML-DSA signature is hedged with fresh randomness (FIPS 204 §3.4); the container, the content digest and the statement stay deterministic; the claimed signing time is an input and never the host's clock.

Measured, to say exactly what the signature is a limit on: the gate's image was signed twice with the test key and the boot tests' fixed signing time. The two signed images are the same size and differ in 4,603 bytes, all of them within one span of 4,627 bytes, which is the length of an ML-DSA-87 signature. Everything outside that span is the same bytes: the image, the statement with its content digest, and the Ed25519 signature.

So what a signature covers, the statement and through its digest the image, is compared by the gate as the unsigned image. The signature bytes themselves cannot be compared, on one machine or two. Nothing about signing was changed.

### 15.3 The control and the gates

On `3fe5aef`, the last commit that changes code, in one turn on the machine (16:56 to 17:06 UTC):

- **The gate's reading of the converted image made to follow its directory** (a three-line edit of `reproducible.rs` that adds the directory's path to what was read, put back with `git checkout`): the gate fails, "the kernel image that boots (its ELF as `onbf-pack krn` converts it) is not reproducible", after the ten other files, the kernel's ELF among them, had passed.
- The gate passes in 35 s (34 s before this step) and prints eleven digests.
- `cargo xtask check` passes in 6 min 15 s: 1,689 host tests, 0 failed, 27 ignored; Loom, all 53 models and all 78 controls.
- `cargo xtask boot-test` passes on its first attempt, 11 of 11 scenarios in 134 s, its kernel boot ending `boot verdict: degraded: 63 of 64 proofs passed; failed: speculation controls`.
- After the boots, the converted image, the kernel's ELF and the program in the ordinary target directory are the gate's, byte for byte.

No boot matrix for this step: no image changed, and the keeper runs all three on the merge.

### 15.4 Not verified

- **CI's digest for the converted image,** when this was written. `onbf-pack` is a host tool, built for each host; that its Linux build converts the same ELF to the same bytes was expected and not seen. It is seen since: §16.
- A carried program's own converted form: the kernel carries its segments as the gate already reads them, and they are compared as files.
- A signed release image: no release signing exists yet to compare around.

## 16. The comparison with CI on a tree that holds item 44: `8ae9e9b` (2026-11-12)

The batch was pushed once more, at `8ae9e9b`, whose tree is `c6c2c68`'s: items 32 and 44 and item 44's step 2. CI ran on it (run 43275652672, its `cargo xtask check` job 131383211984).

### 16.1 What CI's job showed by itself

- `cargo xtask check` passed: 1,690 host tests, 0 failed, 27 ignored (the Mac runs 1,689; §13.1 says which one it cannot); Loom, all 53 models and all 78 controls.
- **The new gates passed on a Linux host:** `version-gate`; `image-graph-gate`, over the same 19 packages under 3 images; and the reproducible-build gate, each of its eleven files the same from CI's two builds and free of that machine's paths.
- **The Linux linker honours the fixed line.** The gate printed, for the program's linker output and for the kernel, that `.comment` holds the fixed line and not the linker's own. §14.9 had this as not verified.
- The step that keeps the gate's files ran: 3,008,614 bytes as uploaded, the eleven files from each build, 7.4 MB unpacked.
- The job's machine was an Intel Xeon Platinum 8573C with 4 processors, on the `ubuntu-24.04` image: the third processor model in three runs (§13.1, §13.6).

### 16.2 The eleven files, the Mac's against CI's

The Mac's are the gate's at `3fe5aef`, the last commit that changes code. The keeper's own eleven for `8ae9e9b`, built in another checkout on the same Mac, are the same eleven.

| File | Bytes | SHA-256, on the Mac and on CI |
|---|---|---|
| loader, no feature | 196,096 | `b3ee60b9173235fdc38d708a2c95e14e3c065abdc8d89b28231c693a787dd6d2` |
| loader, `test-trust-store` | 199,168 | `a9f7ddf8536b6cef49e81af38bc3dd70b2788e9baedd6d26cde2c85013830905` |
| loader, `developer-mode` | 199,168 | `ee2a063f093fbbf4135f2b29155165d901d4c5b7bb29c20e915d36024e7e3eaf` |
| `first_program` as the linker wrote it | 29,768 | `8a6658ab4662300661306886431348de8a1346c084bd61acda1901b30052b627` |
| `first_program.0.bin` | 4,200 | `9e35a5739dc0e0133499a77bd12430720f0788a30c50e7f39e27b4b3c68dbf16` |
| `first_program.1.bin` | 10,170 | `01f7addb33cac76c6aa8bf93d7607b2ab3ed30eb8455b684846fdf920fbad38c` |
| `first_program.2.bin` | 640 | `592ee8a478796bb0cc4274c7cb9336f57a6fa4f5c834b24389944e553bccdb14` |
| `first_program.3.bin` | 24 | `3341bcae4c97d7ba73e0728c783373725905eab0417cf220037920f0960679cb` |
| `first_program.rs` | 1,748 | `364739ab7e1b18462e7fffeefe1a0e33282e1d795a19cd271fdef08487991bc5` |
| kernel ELF | 1,680,400 | `897a6d0fc43a2cae92342f69da35a8b3d34bda30df3f60637bbf66c9858b178a` |
| kernel image that boots | 1,545,848 | `239f308b539b85e2b28d14ee066ca9652cc8c021600f2f7490421bba9a6f5c92` |

**All eleven are the same bytes on the two machines.** They were compared as files, each of the Mac's against the one in CI's artifact, and not by digest alone; CI's log prints the same eleven digests; and in the artifact each file of CI's second build is the same bytes as its first's.

At `c34fdab`, before item 44, four of ten were the same (§13.2). Each digest here is the one predicted before CI ran: §14.3 and §14.5 for the first ten, §15.1 for the eleventh.

### 16.3 What that shows, and what it does not

**Shown:** for one commit, the eleven files the reproducible-build gate builds, built twice from nothing on an Apple-processor Mac and twice from nothing on a GitHub-hosted x86-64 Linux machine, are the same bytes in all four builds. Among them is the image that boots. The three things a build took from its host (§13.4) are ended on a real Linux host as well as under the Mac's stand-ins for one: the remap, the absence of host units, and the linker's fixed line each held there.

**Not shown,** and what the requirement rows still ask:

- **Every shipped artifact.** These eleven are what the tree builds today. The formats that will ship more, packages, drivers, applications, an installer's image, a release manifest, do not exist yet, and a new artifact is in the gate only when someone adds it there.
- **A release candidate.** There has been none. This is one development commit.
- **An independent rebuild.** CI built the pushed commit from nothing on another machine, with another account and another kind of host. It was started by the same push and uses the same workflow, toolchain download and repository as every other run; nobody outside the project rebuilt anything, and no step of a release says who rebuilds and where.
- **A comparison that runs.** The gate on each machine runs on every `check`. The comparison across the two was made by hand, once, from CI's log and its kept files. Nothing compares the two machines' digests on the next commit.
- **Other hosts.** One Mac with an Apple processor and GitHub's x86-64 Linux runners on one image. §14.7 lists what was not looked for.
- **A signed image.** What a release ships is signed, and a signed image is never the same bytes twice (§15.2). The criterion's "the same bytes" can hold for what a signature covers and not for the signature.
- **The relink path** (§14.5) has been exercised by a host test and one run on one stale directory in one checkout.

### 16.4 What would close the gap between this and a check

Not built when this was written; put to the coordinating session with the result. It decided the rows' states on this evidence (three are `built`, none `done`), made the second of these TASKS item 45, and put the first to the owner as decision item 275.

- A comparison that runs: CI's `check` job on both of its runner images, and a job that compares the two artifacts. That is two machines on every run, both Linux; it is a new thing CI must pass.
- A tool for the comparison made here by hand: a command that takes a directory of kept files and compares it with the gate's own.
- Words in the rows' criterion for a signed artifact, which cannot be "the same bytes".
