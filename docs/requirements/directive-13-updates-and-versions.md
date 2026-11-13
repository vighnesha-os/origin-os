# Requirements matrix: PART XIII, Atomic updates and unified versions

One row for each statement of CLAUDE.md in §62 to §65. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S62-01 | Create: `origin-version.toml` | build | eng/host | 25 | done | origin-version.toml | The file is in the repository and is the only place a version is written. | file:origin-version.toml | host | none | - | - |
| S62-02 | All released first-party system modules must belong to the same canonical Origin OS release. | build | eng/host | 25 | built | origin-version.toml, runtime/version, developer-tools/xtask/src/main.rs | Every first-party module carries the release identity generated from `origin-version.toml`, and a gate fails a crate that states a version of its own. | xtask:check, file:origin-version.toml | host | none | Holds for the crates that exist: no Cargo manifest may state a version, and the loader and the kernel carry the generated identity. Most of the listed modules do not exist yet. | - |
| S62-03 | This includes: Bootloader. | build | eng/host | 25 | built | origin-version.toml, runtime/version, developer-tools/xtask/src/main.rs | Every first-party module carries the release identity generated from `origin-version.toml`, and a gate fails a crate that states a version of its own. | xtask:check, file:origin-version.toml | host | none | Holds for the crates that exist: no Cargo manifest may state a version, and the loader and the kernel carry the generated identity. Most of the listed modules do not exist yet. | - |
| S62-04 | This includes: Framekernel. | build | eng/host | 25 | built | origin-version.toml, runtime/version, developer-tools/xtask/src/main.rs | Every first-party module carries the release identity generated from `origin-version.toml`, and a gate fails a crate that states a version of its own. | xtask:check, file:origin-version.toml | host | none | Holds for the crates that exist: no Cargo manifest may state a version, and the loader and the kernel carry the generated identity. Most of the listed modules do not exist yet. | - |
| S62-05 | This includes: Drivers. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-06 | This includes: SDK. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-07 | This includes: Runtime. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-08 | This includes: UI. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-09 | This includes: Desktop. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-10 | This includes: System services. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-11 | This includes: Security services. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-12 | This includes: Installer. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-13 | This includes: Update manager. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-14 | This includes: Recovery components. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-15 | This includes: First-party system applications. | build | eng/host | 25 | not-started | - | - | - | - | none | - | - |
| S62-16 | Use generated version metadata. | build | eng/host | 25 | done | runtime/version/src/generated.rs, runtime/version/manifest.rs, developer-tools/xtask/src/image_inputs.rs, origin-version.toml | The identity each image carries is generated from `origin-version.toml`, and `cargo xtask check` fails when the generated text is out of step with that file. | xtask:check, file:runtime/version/src/generated.rs, test:developer-tools/xtask/src/image_inputs.rs::a_generated_file_out_of_step_is_refused_with_the_command_that_rewrites_it | host | none | - | - |
| S62-17 | Do not manually edit version strings in every module. | rule | eng/host | 25 | done | developer-tools/xtask/src/main.rs | `cargo xtask check` fails when a Cargo manifest states a package version. | xtask:check | host | none | - | - |
| S63-01 | Maintain: Product release identity. | build | both/hw | 25 | built | origin-version.toml, runtime/version, developer-tools/xtask/src/main.rs | Every first-party module carries the release identity generated from `origin-version.toml`, and a gate fails a crate that states a version of its own. | xtask:check, file:origin-version.toml | host | none | Holds for the crates that exist: no Cargo manifest may state a version, and the loader and the kernel carry the generated identity. Most of the listed modules do not exist yet. | - |
| S63-02 | Maintain: ABI compatibility identifiers. | build | eng/host | 22 | built | origin-version.toml, sdk/interfaces | Each ABI has an identifier of its own in `origin-version.toml`, and a gate fails a change to an ABI that does not change its identifier. | file:origin-version.toml, xtask:interface-gate | host | none | `onbf-format`, `boot-info` and `user-abi` have identifiers. The gate holds the interfaces written in OIDL; boot info's is changed by hand. | - |
| S63-03 | Maintain: Package format versions. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S63-04 | Maintain: Persistent data schema versions. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S63-05 | Maintain: Hardware compatibility metadata. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S63-06 | All first-party modules must be associated with the same release version. | build | both/hw | 25 | built | origin-version.toml, runtime/version, developer-tools/xtask/src/main.rs | Every first-party module carries the release identity generated from `origin-version.toml`, and a gate fails a crate that states a version of its own. | xtask:check, file:origin-version.toml | host | none | Holds for the crates that exist: no Cargo manifest may state a version, and the loader and the kernel carry the generated identity. Most of the listed modules do not exist yet. | - |
| S63-07 | Unchanged binary payloads may be reused through content-addressed storage and a newly signed unified release manifest. | info | - | 22 | - | - | - | - | - | none | - | - |
| S63-08 | Do not require unnecessary recompilation merely to achieve metadata consistency. | rule | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-01 | Implement: Signed manifests. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-02 | Implement: Artifact hashes. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-03 | Implement: Trusted roots. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-04 | Implement: Key rotation. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-05 | Implement: Expiration checking. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-06 | Implement: Rollback protection. | build | both/hw | 22 | not-started | - | - | - | - | none | `origin-version.toml` has a security version that nothing reads yet (design review, finding 15). | - |
| S64-07 | Implement: Freeze-attack protection. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-08 | Implement: Mixed-metadata protection. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-09 | Implement: Staging. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-10 | Implement: Atomic deployment activation. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-11 | Implement: Boot-health checks. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-12 | Implement: Recovery. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-13 | Implement: System rollback. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-14 | Use TUF's threat model as a design reference. | rule | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S64-15 | Do not claim full TUF conformance without implementing its relevant requirements. | rule | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-01 | Support: Previous working deployment. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-02 | Support: Recovery boot. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-03 | Support: Offline repair. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-04 | Support: Filesystem checking. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-05 | Support: Driver rollback. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-06 | Support: Kernel rollback. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-07 | Support: Package database repair. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-08 | Support: Update cancellation before activation. | build | both/hw | 22 | not-started | - | - | - | - | none | - | - |
| S65-09 | Support: Recovery-key entry. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-10 | Support: Backup restoration. | build | both/hw | 44 | not-started | - | - | - | - | none | - | - |
| S65-11 | Support: Factory reset. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-12 | Support: Secure user-data removal where supported. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
| S65-13 | Keep user data separate from system image rollback. | build | both/hw | 21 | not-started | - | - | - | - | none | - | - |
