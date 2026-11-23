# Contributing to Origin OS

Origin OS is developed in the open, under MIT OR Apache-2.0. This page says what a contribution from outside the project needs. It states rules that [`CLAUDE.md`](CLAUDE.md), the engineering directive, already sets; where the two differ, the directive decides.

## 1. Every contribution is original

Origin OS is written from scratch (`CLAUDE.md` §3). So is every contribution.

- **Write it yourself, for this project,** from published specifications, vendor documents, and Origin's own decisions and specifications in [`docs/`](docs).
- **Do not copy, translate or restructure code** from Linux, Windows, ReactOS, the BSDs, macOS or any other operating system, driver or GUI framework. Code is not original because it was renamed, rearranged or rewritten in Rust.
- **Reading other open-source code as a reference** is allowed only as §3 says:
  - for a named reason, such as a device quirk the documents leave out;
  - recorded in [`docs/engineering/SOURCE_PROVENANCE.md`](docs/engineering/SOURCE_PROVENANCE.md): each file, its project, its tag or commit, its licence, the date, the reason, and the gap it filled;
  - with what was learned written first as facts, in Origin's own words and structure, and the code written from that.
  
  ReactOS is excluded. Leaked or proprietary code is never opened.
- **Keep restricted material out.** The repository is public. Do not add a specification, firmware image or other file whose terms forbid redistribution (§17, §38).
- **No third-party runtime code.** Shipping code depends on no third-party implementation crate (§5). An exception needs the owner's approval before any work depends on it. Tools that run only on the build host may use approved development dependencies.

## 2. The declaration, and the sign-off

Every commit of a contribution ends with a sign-off line in your own name:

```text
Signed-off-by: Your Name <you@example.org>
```

`git commit -s` adds it. By signing off you certify the [Developer Certificate of Origin, version 1.1](https://developercertificate.org/), and you declare that:

1. the contribution is original under `CLAUDE.md` §3: you wrote it for this project, it is not copied or translated from another operating system, driver or framework, and any reference you read is recorded as section 1 says;
2. you offer it under the repository's licence, MIT OR Apache-2.0.

Whatever tools you used to write it, you are the one who signs: the declaration covers everything in the commit. A pull request whose commits lack the sign-off is not merged.

## 3. Before you open a pull request

- **Talk first, for anything larger than a small fix.** Open an issue that says what you want to change and which specification it follows. Decisions on binary interfaces and formats, on the product's scope and on runtime dependencies are the owner's (§1), and are recorded as an ADR in [`docs/adr/`](docs/adr) (§18). A pull request that changes one without an accepted decision is not merged.
- **Use the pinned toolchain.** [`rust-toolchain.toml`](rust-toolchain.toml) selects it. Do not change it: a compiler upgrade is a task of its own (§15).
- **Run the checks,** and say in the pull request which you ran and what they printed:
  - `cargo xtask check`, always;
  - `cargo xtask boot-test`, for anything the loader or the kernel builds;
  - `cargo xtask boot-matrix`, for a change to the kernel, the loader or a driver.
  
  Never report a test you did not run.
- **Follow the code rules** (§13, §11):
  - `unsafe` only in the files the gate lists, and every `unsafe` block with its invariants written beside it;
  - no Linux API and no `cfg(target_os = "linux")` in Origin's code;
  - untrusted input parsed with checked arithmetic and explicit bounds;
  - no placeholder, no pretended success and no fake hardware support: a feature that is not implemented and proven reports "not supported".
- **Bring the records with the change:** a provenance entry for a new crate or a new source (§6), the requirement rows it moves in [`docs/requirements/`](docs/requirements/README.md), and the progress page, regenerated with `cargo xtask progress`.
- **Keep commits small and real.** Each one builds. Do not add code to make a change look larger (§7).

## 4. Review and merging

- Every change is reviewed. Unsafe code, privilege boundaries, cryptography and shared kernel interfaces need a review independent of their author (§1).
- [`CODEOWNERS`](CODEOWNERS) names the owners of the kernel and of the SDK's interfaces, and GitHub asks them for a review when a pull request touches those paths.
- CI must pass.
- Merging to `main` takes the owner's approval (§1).

## 5. Security problems

Do not put the details of a vulnerability in a public issue, pull request or discussion. [`SECURITY.md`](SECURITY.md) says how to report one privately.

## 6. Licence

A contribution is offered under MIT OR Apache-2.0, like the rest of the repository ([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). Third-party test data keeps its own licence, stated beside it.
