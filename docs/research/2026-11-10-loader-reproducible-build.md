# Research ledger: why two builds of the loader differed, and what makes them the same

| | |
|---|---|
| Date | 2026-11-10 |
| For | The loader's link arguments in `.cargo/config.toml`, and the gate in `developer-tools/xtask/src/reproducible.rs` (TASKS item 31) |
| Requires | CLAUDE.md §75 and §60 (reproducible builds; timestamps and build paths tracked), §14, and §9 and §16 (verify the toolchain locally; research first, today's revisions) |
| Method | Experiments with the pinned toolchain on the development Mac, each built from nothing in a scratch target directory. The PE format specification and the Rust books were read on the day from their publishers. UEFI 2.11 was read later the same day, from a copy the owner supplied, and it changed the choice in §5. No linker, compiler or operating-system source was opened (§3) |

## 1. What was found

While comparing builds for TASKS item 17, two release builds of the same loader source gave `origin-boot.efi` files that differed in 10 to 12 bytes. The kernel ELF rebuilt byte for byte.

Reproduced on 2026-11-10 at `2fbaea6`: build, copy, `touch boot/uefi/src/main.rs`, build again, `cmp -l`. The files (175,616 bytes) differed in 10 bytes, in three places:

| File offset | Bytes | What it is |
|---|---|---|
| 0x80 | 1 to 4 | `TimeDateStamp` of the COFF file header: `0x6AF31402` and `0x6AF31408`, which are 12:54:26 and 12:54:32 UTC that day, the times of the two links |
| 0x2A63C | 1 to 4 | the same stamp, in the debug directory's one entry (type 2, CodeView) |
| 0x2A658 | 8 | the first eight bytes of the identifier in the CodeView record (`RSDS`). The other eight are always the text `LLD PDB.` |

How many bytes of a stamp differ depends on how far apart the two links were, which is why the count varies from 10 to 12.

## 2. Sources

| ID | Source | How it was obtained |
|---|---|---|
| P1 | Microsoft, "PE Format": `learn.microsoft.com/en-us/windows/win32/debug/pe-format`. The page's metadata dates it 2026-09-10 | Read on 2026-11-10. Sections used: "MS-DOS Stub (Image Only)", "Signature (Image Only)", "COFF File Header (Object and Image)", "Debug Directory (Image Only)", "Debug Type", the paragraph on `IMAGE_DEBUG_TYPE_REPRO`, and Appendix A, "Calculating Authenticode PE Image Hash" |
| R1 | The rustc book as published for Rust 1.99.1, "Codegen Options": `doc.rust-lang.org/1.99.1/rustc/codegen-options/index.html`, entries `strip` and `link-arg` | Read on 2026-11-10 |
| R2 | The Cargo book as published for 1.99.1, "Build Scripts": `doc.rust-lang.org/1.99.1/cargo/reference/build-scripts.html`, entry `cargo::rustc-link-arg-bins` | Read on 2026-11-10 |
| T1 | The toolchain itself: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`, `cargo 1.99.1 (5f94df478 2026-08-27)`, and its `rust-lld`, which reports `LLD 23.1.1` (rust-lang/llvm-project `1b9c0d5f`), on `aarch64-apple-darwin` | What it lists (`rust-lld -flavor link '/?'`), what rustc passes it (`--print link-args`, `cargo build -v`), and what it writes (`llvm-readobj`, `cmp`), each observed on 2026-11-10 |
| U1 | Unified Extensible Firmware Interface (UEFI) Specification, **Release 2.11**, UEFI Forum, Inc.; its title page is dated Nov 21, 2024. 2,301 pages. Sections used: 2.1.1 "UEFI Images" and 2.1.2 (the document's pages 16–17), 7.4.1 `LoadImage()` (197–199), 18.4 (793–794), 32.2.2 and 32.2.3 (1532–1533), 32.5.1 and 32.5.3.3 (1549–1554), and Appendix Q, References (2196) | `uefi.org` answered HTTP 403 to the fetch tool and to the browser, as it has before (`2026-11-09-uefi-2.8-to-2.11.md`). The owner then supplied a copy as a local file: 16,698,759 bytes, SHA-256 `a64b8e442004b91becc3de9afaf8ca61b259a9a3b436accb6b3711ab5400cee9`. The file's own record says it was made on 2024-11-22 and last changed on 2024-12-04, by the UEFI Forum. The system's download record names a third-party file store, not `uefi.org`, as where this copy came from, and it was not compared with the publisher's file. The file forbids copying its content and allows reading it out for accessibility: it was read in memory through the system's PDF library, and nothing of its text is in the repository (§17) |

What P1 says, in this ledger's words:

- The stub holds, at 0x3C, the file offset of the four-byte signature `PE\0\0`. The COFF file header follows it: `Machine` (2 bytes), `NumberOfSections` (2), then `TimeDateStamp` (4), which is when the file was created, in seconds since 1970. So the stamp starts eight bytes after the signature. In the loader the signature is at 0x78 and the stamp at 0x80.
- The debug directory is optional. Each entry has a time stamp of its own, a type, and the size and place of its data.
- An entry of type 16, `IMAGE_DEBUG_TYPE_REPRO`, says the image was built to be reproducible, and that its time stamps hold bits of a hash of the image's content instead of a time. The entry's data may be empty.
- The Authenticode hash of an image leaves out the checksum field, the certificate table's directory entry and the certificates, and anything after the last section. The time stamp is not on that list, and the debug directory and the CodeView record lie inside `.rdata`. So a signed loader can be rebuilt and compared with what was signed only if both are reproducible.

What R1 says: `strip=debuginfo` removes debug sections at link time, and since 1.79 it no longer stops a PDB being written for MSVC-style targets. `link-arg` appends one argument to the linker's command line.

What U1 says, in this ledger's words:

- **The format (2.1.1).** A UEFI image is a file that starts with an image header, and UEFI uses a subset of the PE32+ format. The header's `Subsystem` says what kind of image it is (10 for an application, 11 and 12 for the two kinds of driver) and its `Machine` says what code it holds (`0x8664` for x64).
- **What firmware does with it (2.1.1, 2.1.2, 7.4.1).** `LoadImage()` must load every section of the image, apply the relocations, and pass control at `AddressOfEntryPoint`. For an application the memory is given the loader's code and data types. `LoadImage()` fails if the image's type is not supported, or its format is corrupt or not understood.
- **What it does not mention.** A search of the text of all 2,301 pages finds no mention of the header's `TimeDateStamp`, of the header's checksum (the 24 pages with the word are about ACPI, GPT and other tables), of the debug directory, or of a CodeView record. Text inside the figures was not searched. The debugger chapter (18.4) describes loaded images to a debugger through a table the firmware builds, and asks nothing of the image.
- **Signing (32.2.2, 32.2.3).** A signature is embedded in the image: the fifth data directory entry of the header points to the certificates. For how the image's digest is made, U1 sends the reader to Microsoft's Authenticode format specification.
- **Authorizing an image (32.5.1, 32.5.3.3).** An image passes only if the authorized signature database holds either a hash of that image or a certificate that verifies one of its signatures, and the forbidden database holds neither the image's hash nor a certificate of the signing chain. The time that plays a part in revocation is the signature's trusted time stamp (RFC 3161), which is another thing from the header's stamp.
- **Which PE/COFF.** Appendix Q cites the "Microsoft Portable Executable and Common Object File Format Specification, Version 8.1", and the Authenticode format, Version 1.0. P1 is today's page, not version 8.1. Version 8.1 was not read, so this ledger does not know whether it has type 16.

Two things follow. A machine's owner can authorize one exact loader by its digest, and a revocation can name one: both need the loader that was measured to be the loader anyone can rebuild. And the loader's `Machine` (`0x8664`), `Subsystem` (10) and entry point are the same before and after this task (`llvm-readobj`), so the image is as valid an application as it was.

## 3. The two causes

For `x86_64-unknown-uefi`, rustc 1.99.1 runs rust-lld's COFF driver with:

    /NOLOGO /entry:efi_main /subsystem:efi_application <objects> /NXCOMPAT /OUT:<image>
    /OPT:REF,ICF /DEBUG /PDBALTPATH:%_PDB% /NODEFAULTLIB

**The time stamp.** With no argument about it, the linker writes the time of the link into the header, and into every debug directory entry.

**The identifier.** `/DEBUG` makes the linker write a PDB file beside the image, and a CodeView record in the image that names the PDB and carries an identifier for it. The identifier's first eight bytes change with every link because the PDB does:

- The PDB holds the full path of every file linked, twice: in its list of modules and in a copy of the linker's command line.
- One of those files is `symbols.o`, which rustc writes into a directory it creates for each link under a random name: `…/release/deps/rustcjxiWaz/symbols.o` in one build and `…/deps/rustcNPWmWo/symbols.o` in the next.
- The identifier follows the PDB's content. Shown by relinking by hand from rustc's saved command line (`-C save-temps`), with `/Brepro` so that the time plays no part:
  - the same command line twice, 1.1 s apart: the two PDBs are the same bytes, and so are the two images;
  - the same command line with `symbols.o` copied to a directory of another name, and nothing else changed: the PDBs differ in 24 bytes (the six letters of the directory's name in two places, and the PDB's own hash in two fields), and the images in 20 (the stamp in three places, and the identifier).

So no argument about time can make the loader reproducible while the record is there. The name of rustc's directory cannot be set.

**`-C strip` does not remove `/DEBUG` here.** Cargo already passes `-C strip=debuginfo` for a release build with no debug information (`cargo build -v`), and the link line above is what results. With `-C strip=symbols` the link line is the same.

## 4. What each candidate writes

Each row is the loader with `test-trust-store`, built twice from nothing in two target directories whose paths differ in length, a second or more apart.

| Arguments after rustc's own | Two builds | What the image holds |
|---|---|---|
| none | differ in 10 bytes | the link time, twice; a CodeView record; a PDB is written |
| `/Brepro` | differ in 20 bytes | the stamp is a hash, in the header and in two debug entries (CodeView and type 16), and differs because the identifier does; a PDB is written |
| `/Brepro /build-id:no` | differ in 20 bytes | the same. The linker's help says a build ID is "always on when generating PDB" |
| `/DEBUG:NONE` | differ in the header's stamp | the link time, once; no debug directory; no PDB |
| `/TIMESTAMP:0` | differ in 8 bytes | zero stamps, which agree; the CodeView record's identifier differs; a PDB is written. Seen in the gate's control on the loader with no feature (§8) |
| `/DEBUG:NONE /Brepro` | **the same bytes** | the stamp is a hash (`0x17939D58` for this source); the debug directory holds one empty entry of type 16; no PDB |
| `/DEBUG:NONE /TIMESTAMP:0` | **the same bytes** | the stamp is zero; no debug directory; no PDB |

Of two `/DEBUG` options the linker obeys the last, and `-C link-arg` arguments come after rustc's own: the link line ends `/DEBUG /PDBALTPATH:%_PDB% /NODEFAULTLIB /DEBUG:NONE /TIMESTAMP:0`.

## 5. The choice: `/DEBUG:NONE` and `/TIMESTAMP:0`

Both of the last two rows work. **`/TIMESTAMP:0` is what the tree has.**

`/Brepro` was chosen first, before U1 could be read, for two reasons. It is what today's PE format defines for this (P1): the type 16 entry tells a tool that the stamp is a hash and not a time. And the stamp then changes whenever any byte of the image does. That choice was built, tested and booted (`32f1a94`), and its image differed from the old one in 70 bytes.

Reading U1 changed it, for these reasons:

- What U1 says firmware uses of an image is its header's `Subsystem` and `Machine`, its sections, its relocations and its entry point. With `/TIMESTAMP:0` the image holds nothing else: no debug directory at all. With `/Brepro` it keeps a debug directory of one entry.
- That entry's type is defined in today's PE format. The PE/COFF revision U1 cites is version 8.1, and whether it has that type is not known here.
- A firmware's loader may read the debug directory, though U1 does not ask it to. Whether every firmware passes over an entry of a type it does not know can only be found out on real firmware, and the project has none to try (§74). If one did not, the loader would fail before it could print anything. A zero in a field U1 never mentions carries no such question.
- Nothing in Origin needs the stamp to tell two builds apart: images are known by their digests.

What it costs: a tool that prints the stamp shows 1970, and nothing in the image says why.

What `/DEBUG:NONE` costs: the PDB is no longer written. A release build of the loader carries no debug information, so it held symbol names and no more, and nothing in the repository read it. The code does not move when the record goes (§7), so a PDB for a given loader can still be had by linking a copy with `/DEBUG`.

## 6. Where the arguments are: the configuration file, not a build script

The kernel's link arguments are in `framekernel/build.rs`. The same was tried for the loader and dropped:

- A build script for the loader's package changes the `-C metadata` hash Cargo gives the loader's two crates, even if the script prints nothing: `3b914211032e4f92` became `2cc65db58834c175` for the binary.
- The image then differs from the old one in 3,755 bytes, `.text` among them. `.text` and `.rdata` keep their sizes and the count of each instruction is the same, so it is the same code in another order. But "no instruction changed" could then not be shown byte for byte, in the program every boot trusts first.
- With the arguments in `.cargo/config.toml`, beside the frame-pointer flag that §14 asks for, the hash does not change and `.text` is the same bytes as before (§7).

What that costs: `RUSTFLAGS` in the environment replaces the configuration file's whole list. With `RUSTFLAGS="-C debug-assertions=no"`, rustc was given neither the two link arguments nor `force-frame-pointers` (`cargo build -v`). The gate then fails and its report says so. The frame pointers were exposed in the same way before this change; see §9.

## 7. What changed in the image

The loader with `test-trust-store`, before (`2fbaea6`) and after, both 175,616 bytes: 74 bytes differ.

| File offset | Bytes that differ | What |
|---|---|---|
| 0x80 | 4 | the header's stamp: a link time before, zero after |
| 0x130 to 0x134 | 4 | the header's entry for the debug directory: at `0x2B638`, 28 bytes long before; none after |
| 0x1B0 | 1 | `.rdata`'s size: `0x468D` before, `0x4635` after, 88 bytes less |
| 0x2A63C to 0x2A652 | 12 | where the debug directory's one entry was: its stamp, its type, and its data's size and two addresses, all zero after |
| 0x2A654 to 0x2A68C | 53 | where the 57-byte CodeView record was, zero after (four of its bytes were zero already) |

`.text` (154,319 bytes) is the same bytes before and after, and the disassembly (`llvm-objdump -d`) is the same text. `Machine`, `Subsystem` and `AddressOfEntryPoint` (`0x3660`) are unchanged.

With `/Brepro`, as first built, 70 bytes differed: the same places without the header's entry, the directory's entry holding type 16.

## 8. What stays the same from build to build, on this machine

- The loader, after the change: after `touch` and a rebuild in the working target directory; from nothing in two scratch target directories, by hand and by the gate; and built from a copy of the source (`git archive`) in another directory, outside the checkout. Each gives SHA-256 `f0b34d78…5b823` for the `test-trust-store` loader. The first form, with `/Brepro`, gave `31d81e4b…88e58` in the same ways, and from a relink by hand.
- The gate's four controls on the real tree, each of which fails it: no argument (10 bytes), `/DEBUG:NONE` alone (the header's stamp), `/TIMESTAMP:0` alone (the identifier's 8 bytes), and a random build ID in the kernel (16 bytes, after the three loaders pass). With the first form, `/Brepro` alone failed it too (20 bytes).
- The kernel ELF (1,203,984 bytes), unchanged by this task: from nothing in two scratch target directories, and from the copy in another directory, SHA-256 `a0faf324…7f461` each time. Item 17's record has the same digest for `2fbaea6`, built in another worktree.

So on one machine neither image depends on the time, on what was built before, on the target directory, or on where the source is checked out.

## 9. Found, not changed

**The images hold the path the toolchain is installed at.** This is the next thing that stands between these builds and §75's independent rebuild.

- The loader holds one such path and the kernel eight, all of this form: `/Users/<account>/.rustup/toolchains/1.99.1-aarch64-apple-darwin/lib/rustlib/src/rust/library/core/src/slice/mod.rs`. They are the source locations of panics in standard-library code that is compiled into Origin's crates.
- The path holds the builder's account name and the build host's triple. CI installs the toolchain under another home directory, with `x86_64-unknown-linux-gnu` in the name. The strings differ in length there, so the images built on the Mac and in CI cannot be the same bytes. That is a prediction from the strings: no image built on another machine was compared.
- The gate cannot see this. Both of its builds use one toolchain in one place.
- The cause is the `rust-src` component, which `rust-toolchain.toml` installs (CLAUDE.md §8 lists it). With it present the compiler names the standard library's source by where it really is. Shown with a sysroot made of symbolic links to the pinned toolchain's own files, all but `lib/rustlib/src`, passed as `--sysroot`: the loader then holds no path under `/Users` or `/private`, and holds `/rustc/58cf8f9c35b355199727627d07ae34a4e2846d67/library/core/src/slice/mod.rs` in its place. The toolchain itself was not touched.
- Two ways to a fix, which give the same loader image, byte for byte (SHA-256 `cdf66e05…`):
  - no `rust-src` in the toolchain, as above. That changes `rust-toolchain.toml` and §8's list, so it is the owner's;
  - `--remap-path-prefix=<sysroot>/lib/rustlib/src/rust=/rustc/<the compiler's commit hash>` in the flags of both images. The prefix is different on every machine, so it cannot be written into `.cargo/config.toml`: something has to compute it for every build.
- Either one changes the bytes of the kernel and the loader, and how every build is run, so it is a task of its own. It was not done here.

**`RUSTFLAGS` in the environment drops `force-frame-pointers` too** (§6), for the kernel and the loader. Nothing in `xtask` clears it or refuses to build with it set. CI does not set it.

**A worktree under the main checkout reads two configuration files.** Cargo reads `.cargo/config.toml` from the working directory and from every directory above it, and joins the lists. The project's worktrees are under the main checkout, so their builds get `force-frame-pointers` twice today. Once this change is on `main`, a worktree whose own file lost the two link arguments would still get them from the main checkout, and the gate would pass there. CI's checkout has nothing above it.

**Not verified:** a Linux build host, where CI will run the gate for the first time; any firmware but OVMF; two different machines; that the copy of U1 is the publisher's file; version 8.1 of the PE/COFF specification.
