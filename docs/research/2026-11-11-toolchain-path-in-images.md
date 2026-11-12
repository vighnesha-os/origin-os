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
| The linker's name for itself | `.comment` of each ELF, and through `e_shoff` one loaded byte of each | **still there** |

Nothing else was found: no difference in generated code, in data, or in symbol names between the Mac's compiler and linker and CI's, for these ten files.

### 13.5 What could end the third

Not built; put to the coordinating session.

- Cargo's `[env]` configuration can set `LLD_VERSION` for every link. Tried on the Mac with `--config env.LLD_VERSION=…`: the program's `.comment` then holds the given text and the compiler's version, and nothing of the linker's own. One entry in `.cargo/config.toml` would do it for `xtask` and for plain `cargo` alike.
- What it rests on: a variable the linker's source calls a testing aid. A gate that reads `.comment` in each ELF it builds would fail on any machine if a later linker stopped honouring it.
- Other ways: remove `.comment` from the linker's output with a second tool; or have `onbf-pack` not carry the section table's fields of an ELF header. Each changes more than one entry does.

### 13.6 Not verified

- **No file of a tree with item 44 has been compared with CI's.** §14 has the Mac's side.
- With the three inputs set as CI's, the Mac gives CI's bytes; that the reverse holds, CI giving the Mac's, was not tried.
- One CI machine. The `check` job runs on one runner image, so whether another image's toolchain writes the same bytes is not known from this run. At `f5ab54f` the job ran on another processor (an EPYC 9V45) and its loaders fit the same account.
- Whether `LLD_VERSION` behaves the same in the linker's COFF driver was not looked at; the loaders have no `.comment`.
