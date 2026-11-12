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
