# Requirements matrix: PART XV, Complete developer platform

One row for each statement of CLAUDE.md in §70 and §71. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S70-01 | Create native developer tools for: Building `.ose`. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-02 | Create native developer tools for: Packaging `.oapp`. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-03 | Create native developer tools for: Creating `.osi`. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-04 | Create native developer tools for: Signing `.od`. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-05 | Create native developer tools for: Linking. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-06 | Create native developer tools for: Debugging. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-07 | Create native developer tools for: Profiling. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-08 | Create native developer tools for: Crash-symbol analysis. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-09 | Create native developer tools for: ABI validation. | build | both/hw | 37 | partial | developer-tools/oidl, developer-tools/xtask/src/interfaces.rs | The tool runs on Origin as an SDK client and has tests of its own. | xtask:interface-gate, file:developer-tools/oidl/tests/generated.rs | host | none | A host tool: the interface gate holds each interface's ABI to its lock. Nothing runs on Origin. | - |
| S70-10 | Create native developer tools for: SDK documentation. | build | both/hw | 37 | partial | developer-tools/oidl, developer-tools/xtask/src/interfaces.rs | The tool runs on Origin as an SDK client and has tests of its own. | xtask:interface-gate, file:developer-tools/oidl/tests/generated.rs | host | none | A host tool: the interface language generates reference documentation for each interface. Nothing runs on Origin, and no SDK exists to document. | - |
| S70-11 | Create native developer tools for: Package validation. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-12 | Create native developer tools for: Driver verification. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-13 | Create native developer tools for: UI inspection. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-14 | Create native developer tools for: Conformance testing. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-15 | Developer command-line tools are permitted. On Origin OS they are SDK clients like any other program (§48). | rule | both/hw | 37 | not-started | - | - | - | - | none | - | 39 |
| S70-16 | They must not become mandatory hidden backends for ordinary graphical OS administration. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S70-17 | Host tools that run on macOS or Linux are outside §48 until Origin hosts its own builds (ADR-0011 N12). | info | - | 37 | - | - | - | - | - | none | - | - |
| S71-01 | Provide: Origin SDK headers/bindings. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-02 | Provide: Rust application templates. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-03 | Provide: Stable API documentation. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-04 | Provide: UI component documentation. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-05 | Provide: Threading examples. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-06 | Provide: Async examples. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-07 | Provide: Graphics examples. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-08 | Provide: Installer packaging. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-09 | Provide: Debugging. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-10 | Provide: Testing. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-11 | Provide: Deployment guides. | build | both/hw | 37 | not-started | - | - | - | - | none | - | - |
| S71-12 | Examples are supplementary to actual SDK implementation. | info | - | 37 | - | - | - | - | - | none | - | - |
