# Requirements matrix: PART X, Completely native SDK and GUI

One row for each statement of CLAUDE.md in §48 to §53. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S48-01 | Every first-party component must obtain OS functionality only through genuine native Origin SDK interfaces (ADR-0011). | rule | both/hw | 19 | not-started | - | - | - | - | none | - | 33, 39 |
| S48-02 | This binds GUI applications, the desktop, the installer, Origin Setup, the recovery console, command-line tools, the shell, the terminal and system services calling one another. None has a private door: a command-line tool has no power the SDK does not offer. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-03 | The valid architecture is: Client → Origin SDK → authenticated native service → Framekernel/driver. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-04 | The following is prohibited for ordinary OS functionality: Client → hidden shell command → CLI program → operating-system operation. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-05 | No first-party component starts another program to do its work or parses another program's text output. | rule | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-06 | A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108): Starting a program is a capability. Only the session launcher, the shell, the terminal and developer tools may declare it, and a release gate checks every first-party manifest. | rule | both/hw | 19 | not-started | - | - | - | - | none | - | 108 |
| S48-07 | A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108): A program that does not hold it is never handed the launch interface. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 108 |
| S48-08 | A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108): The base image holds no command interpreter. The shell and the terminal are an optional developer package. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 108 |
| S48-09 | A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108): No system library reads the environment. A child's environment is empty unless its launcher sets it, and first-party services take their configuration through typed interfaces, never through arguments. | rule | both/hw | 19 | not-started | - | - | - | - | none | - | 108 |
| S48-10 | A mechanism enforces this, not review alone (ADR-0011 Amendment 1, decision item 108): No interface takes text that its receiver executes or hands to a program. | rule | both/hw | 19 | not-started | - | - | - | - | none | - | 108 |
| S48-11 | Programs reach the Framekernel only through the SDK, by way of `origin.core`. The Framekernel does not call the SDK. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 38 |
| S48-12 | The terminal may exist as an optional development tool. It is an ordinary SDK client and is not the graphical desktop's control plane. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-13 | The only exceptions are ADR-0011 N12's closed list: code that implements the SDK's guarantees or runs where no SDK can exist yet. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-14 | Origin Server is administered the same way. Every administrative operation is available in the native GUI, and none needs a terminal (ADR-0014 S4 and Amendment 1, decision item 117): on its own console; | build | server/virt | 50 | not-started | - | - | - | - | none | - | 74, 117 |
| S48-15 | Origin Server is administered the same way. Every administrative operation is available in the native GUI, and none needs a terminal (ADR-0014 S4 and Amendment 1, decision item 117): from the native management app on another Origin machine, over an authenticated, encrypted connection; | build | server/virt | 50 | not-started | - | - | - | - | none | - | 74, 117 |
| S48-16 | Origin Server is administered the same way. Every administrative operation is available in the native GUI, and none needs a terminal (ADR-0014 S4 and Amendment 1, decision item 117): in a remote desktop session, which must exist before the Server edition ships. | build | server/virt | 50 | not-started | - | - | - | - | none | - | 74, 117 |
| S48-17 | Neither edition needs a terminal or a web page to administer. | rule | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S48-18 | Installing without an operator and configuring many machines are done through the SDK as well: Origin Setup applies a signed, declarative provisioning document. No script runs (ONBF-R3). | rule | both/hw | 19 | not-started | - | - | - | - | none | - | 117 |
| S49-01 | Implement typed SDKs for: Applications. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-02 | Implement typed SDKs for: Processes. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-03 | Implement typed SDKs for: Threads. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-04 | Implement typed SDKs for: Parallel tasks. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-05 | Implement typed SDKs for: Async I/O. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-06 | Implement typed SDKs for: Files. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-07 | Implement typed SDKs for: Storage. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-08 | Implement typed SDKs for: Networking. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-09 | Implement typed SDKs for: Audio. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-10 | Implement typed SDKs for: Graphics. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-11 | Implement typed SDKs for: Input. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-12 | Implement typed SDKs for: Devices. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-13 | Implement typed SDKs for: Installation. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-14 | Implement typed SDKs for: Updates. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-15 | Implement typed SDKs for: Security. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-16 | Implement typed SDKs for: Accounts. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-17 | Implement typed SDKs for: Credentials. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-18 | Implement typed SDKs for: Power. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-19 | Implement typed SDKs for: Notifications. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-20 | Implement typed SDKs for: Search. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-21 | Implement typed SDKs for: Printing. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-22 | Implement typed SDKs for: Accessibility. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-23 | Implement typed SDKs for: Localization. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-24 | Implement typed SDKs for: Compute. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-25 | Implement typed SDKs for: Diagnostics. | build | both/hw | 19 | not-started | - | - | - | - | none | - | - |
| S49-26 | Implement typed SDKs for: Remote administration. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 81 |
| S49-27 | Implement typed SDKs for: Service management. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 81 |
| S49-28 | Implement typed SDKs for: Hardware health. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 81 |
| S49-29 | Implement typed SDKs for: Remote desktop. | build | both/hw | 19 | not-started | - | - | - | - | none | - | 81 |
| S49-30 | The last four serve Origin Server first (ADR-0014 S7). Device technologies follow §31's three layers (ADR-0015). | info | - | 19 | - | - | - | - | - | none | - | - |
| S49-31 | Support stable versioned APIs and a C-compatible ABI. | build | both/hw | 19 | partial | developer-tools/oidl, sdk/interfaces | Every SDK interface is versioned, has a generated C header, and keeps its ABI across a major version. | file:developer-tools/oidl/tests/generated.rs, xtask:interface-gate | host | none | The interface language versions each interface and generates a C header, which its tests compile. No SDK library is built from one. | - |
| S50-01 | Build: Native windows. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-02 | Build: Controls. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-03 | Build: Layout engine. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-04 | Build: Event dispatch. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-05 | Build: Text engine. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-06 | Build: Input system. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-07 | Build: Menus. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-08 | Build: Dialogs. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-09 | Build: Notifications. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-10 | Build: Clipboard. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-11 | Build: Drag and drop. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-12 | Build: Accessibility tree. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-13 | Build: Themes. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-14 | Build: High-DPI scaling. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-15 | Build: Hardware rendering. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-16 | Build: Animation. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-17 | Build: Multimonitor support. | build | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S50-18 | Do not depend on HTML/Electron or browser processes for the native desktop. | rule | both/hw | 20 | not-started | - | - | - | - | none | - | - |
| S51-01 | Implement Unicode-aware text behavior, including: UTF-8. | build | both/hw | 66 | partial | framekernel/src/text.rs | The text engine handles it for every script §51 names, with tests of each. | file:framekernel/src/text.rs | host | none | A streaming UTF-8 decoder, tested against `core::str` on the host. Not yet part of a text engine. | - |
| S51-02 | Implement Unicode-aware text behavior, including: Unicode 18.0 data compatibility. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-03 | Implement Unicode-aware text behavior, including: Normalization. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-04 | Implement Unicode-aware text behavior, including: Grapheme segmentation. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-05 | Implement Unicode-aware text behavior, including: Word segmentation. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-06 | Implement Unicode-aware text behavior, including: Line breaking. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-07 | Implement Unicode-aware text behavior, including: Bidirectional text. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-08 | Implement Unicode-aware text behavior, including: Script detection. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-09 | Implement Unicode-aware text behavior, including: Font fallback. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-10 | Implement Unicode-aware text behavior, including: OpenType shaping. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-11 | Implement Unicode-aware text behavior, including: Ligatures. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-12 | Implement Unicode-aware text behavior, including: Complex-script positioning. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-13 | Implement Unicode-aware text behavior, including: Emoji. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-14 | Implement Unicode-aware text behavior, including: Input methods. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-15 | Implement Unicode-aware text behavior, including: Text editing. | build | both/hw | 66 | partial | framekernel/src/editbuffer.rs | The text engine handles it for every script §51 names, with tests of each. | file:framekernel/src/editbuffer.rs | host | none | A gap buffer indexed by scalar value, model-tested on the host. No editing surface uses it. | - |
| S51-16 | Implement Unicode-aware text behavior, including: Selection and caret movement. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-17 | Test Telugu, Hindi, Arabic, Latin and mixed-direction text. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S51-18 | Fonts and Unicode data must have appropriate licensing and provenance. | build | both/hw | 66 | not-started | - | - | - | - | none | - | - |
| S52-01 | Implement: Window surfaces. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-02 | Implement: Input routing. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-03 | Implement: Frame scheduling. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-04 | Implement: Damage tracking. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-05 | Implement: GPU composition. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-06 | Implement: Software fallback. | build | both/hw | 65 | not-started | - | - | - | - | none | A 2D software rasterizer exists as groundwork (`framekernel/src/raster.rs`). There is no compositor. | - |
| S52-07 | Implement: Atomic display state changes where hardware supports them. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-08 | Implement: Display modes. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-09 | Implement: Multi-monitor. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-10 | Implement: Mixed refresh rates. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-11 | Implement: HDR where supported. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-12 | Implement: Color management. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-13 | Implement: GPU reset recovery. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-14 | Implement: Smooth animation. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-15 | Implement: Accessibility integration. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-16 | The compositor must not be blocked by ordinary application work. | build | both/hw | 65 | not-started | - | - | - | - | none | - | - |
| S52-17 | The compositor is a security boundary between programs (decision item 116): A client sees only its own surfaces and its own input. | build | both/hw | 65 | not-started | - | - | - | - | none | - | 116 |
| S52-18 | The compositor is a security boundary between programs (decision item 116): Synthetic input, global shortcuts, screen capture, reading another client's surfaces, and clipboard access without a user action are capabilities. | build | both/hw | 65 | not-started | - | - | - | - | none | - | 116 |
| S52-19 | The compositor is a security boundary between programs (decision item 116): Credential and elevation prompts are drawn and read by the compositor, on a path no client can draw over or inject into, behind a secure attention key. | build | both/hw | 65 | not-started | - | - | - | - | none | - | 116 |
| S52-20 | The compositor is a security boundary between programs (decision item 116): A remote session is marked as one. | build | both/hw | 65 | not-started | - | - | - | - | none | - | 116 |
| S53-01 | Measure: Input-to-display latency. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-02 | Measure: Window movement latency. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-03 | Measure: Scroll smoothness. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-04 | Measure: Frame pacing. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-05 | Measure: Missed frame deadlines. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-06 | Measure: p50/p95/p99 frame times. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-07 | Measure: CPU/GPU saturation effects. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-08 | Measure: Display hot-plug recovery. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-09 | Measure: Background installation interference. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-10 | Measure: Background update interference. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-11 | Establish benchmark thresholds on declared supported hardware. | build | both/hw | 35 | not-started | - | - | - | - | none | - | - |
| S53-12 | Until that hardware exists, keep provisional budgets in `docs/engineering/PERFORMANCE_BUDGETS.md`, marked as hypotheses, and record the figures CI measures as trends. They claim nothing (decision item 115). | build | both/hw | 35 | partial | docs/engineering/PERFORMANCE_BUDGETS.md, docs/engineering/PERFORMANCE_TRENDS_2026-11-12.md, docs/engineering/PERFORMANCE_TRENDS_2026-11-13_ci-boot-step.md, .github/ci/boot-step-durations.sh | The file holds a budget marked as a hypothesis, with its model, for each path the boot times, and each CI run's figures are recorded beside them as trends, on no threshold. | file:docs/engineering/PERFORMANCE_BUDGETS.md, file:docs/engineering/PERFORMANCE_TRENDS_2026-11-12.md, file:docs/engineering/PERFORMANCE_TRENDS_2026-11-13_ci-boot-step.md | host | none | The trends are one CI run's, read from its logs by hand: nothing records them at each run. How long the boot jobs took, by the runner's processor, is read by hand from 40 jobs of 20 runs; since TASKS item 53 each run's summary prints its own boot jobs' step time beside their processors, and nothing keeps it from one run to the next. No budget is verified on any processor, and the cost ledger in the same file has no row yet. | 115, 243 |
| S53-13 | Do not claim superior performance to Windows without a reproducible comparison. | rule | both/hw | 35 | not-started | - | - | - | - | none | - | - |
