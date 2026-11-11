# The roles of CLAUDE.md §1, checked against the directive, 2026-11-11

| | |
|---|---|
| Asked for | The owner, giving the second text for §1: "Update CLAUDE.md at line: 35 with following, check carefully, if anything missing" |
| Checked | The owner's text as it stands in CLAUDE.md 7.5, §1: 55 lines, which are three opening paragraphs, 50 roles and two closing paragraphs |
| Against | CLAUDE.md 7.5 from its header to the final rules, read in full for this check; the lines of ADR-0014 and ADR-0015 on the subjects below; the 26 agents of §77; the owner's first text for §1 (28 roles, commit `fc92eca` on this branch) |
| How | One AI reader went through the directive section by section and named the role that owns each. A search of the owner's text for each subject then confirmed whether any role names it. "Named by no role" means the words are in no role; where a wider phrase in a role can be read to cover the subject, the finding says so |
| Not done | Nothing was built or tested: this is a check of text. No ADR other than 0014 and 0015 was read again for it. **No human has reviewed this** |
| Outcome | The owner answered with a third text the same day. It took in much of this check and left the rest to Claude: "improve if you can ... You decide" (decision sheet item 168). The last section says what became of each proposal and lists every change Claude made. Everything from "Result" to "What was checked and found right" is the check as the owner read it, and describes the second text |

## Result

The text holds up. Every part of the directive but two has a role. No role contradicts a rule of the directive or an accepted ADR. The nine roles named by revision 7.4's sentence are all among the 50, and 24 of §77's 26 agents have a role.

What is missing is of five kinds:

1. **Two subjects that have a part or a section of the directive to themselves have no owner:** original source and provenance (Part II), and the NPU with the Origin Compute API (§37).
2. **Two rules the design review made strict are in no role:** how the SDK-only rule is enforced (§48), and the GUI as a security boundary (§52).
3. **About thirty smaller subjects the directive names are named by no role.** Most can be read into a wider phrase; a few cannot.
4. **Five wordings of the first text were clearer** than what replaced them.
5. **Several subjects have two owners,** one closing rule cannot be read literally for every role, and the records role 45 owns do not exist yet.

## Where each part of the directive lands

| Part | Sections | Roles that own it | Named by no role |
|---|---|---|---|
| I, authority | §1–§2 | the lead roles; all | P4, original source: see Part II |
| II, original source | §3–§7 | 2 enforces the policy; 25, 27, 31 and 47 touch provenance and licences in their own areas | the provenance record; the rules for reading reference code; the similarity check; runtime-dependency exceptions; the public repository's contribution rules |
| III, toolchain | §8–§15 | 2, 37 | nothing |
| IV, research and ADRs | §16–§18 | 1, 27, 46 | nothing |
| V, boot, installation, first run | §19–§23 | 7, 21, 30, 39, 49, 50 | the boot manager (§20) |
| VI, Framekernel | §24–§30 | 3, 4, 5, 6, 18, 29, 30, 33, 38 | nothing |
| VII, devices | §31–§38 | 8, 9, 11, 13, 14, 40, 47 | the NPU and the Compute API (§37); video acceleration (§36); PCIe error reporting (§32) |
| VIII, storage | §39–§41 | 11, 34, 44 | nothing |
| IX, applications | §42–§47 | 18, 22 | the runtime library and the route to Rust `std` (§44) |
| X, SDK and GUI | §48–§53 | 19, 20, 35, 37, 38, 49, 50 | the SDK-only rule's enforcement (§48); search, notifications, printing, remote administration, remote desktop and hardware health (§49); input methods, bidirectional and complex-script text, fonts (§51); the compositor's security rules (§52) |
| XI, network, audio, peripherals | §54–§56 | 12, 14, 34, 40 | the firewall, TLS and VPN interfaces (§54); pen, game controllers, cameras, printers and scanners (§56) |
| XII, accounts, security, trust | §57–§61 | 16, 17, 21, 25, 29, 30, 31, 32, 42 | session locking, user switching and administrative authorization (§57); the TPM and measured boot (§58); side channels, constant time and zeroization (§59); private vulnerability reports (§6, §60) |
| XIII, updates and versions | §62–§65 | 21, 22, 25, 31, 43, 44 | nothing |
| XIV, performance, power, reliability | §66–§69 | 15, 23, 26, 33 to 36 | running the firmware's ACPI code, which no section names either (proposal 7) |
| XV, developer platform | §70–§71 | 19, 37, 48 | nothing |
| XVI, testing | §72–§76 | 24, 32, 36, 41, 45, 46 | continuous integration and the gates |
| XVII, agents | §77–§80 | 28, 41 | source provenance (agent 25); session hand-offs (agent 26, §79) |
| XVIII to XX, structure, milestones, workflow | §81–§85 | 28, 45 | milestones and their exit tests (§82) |

Virtualization is right as it stands. Role 41 covers Origin as a guest under test. Running virtual machines or containers on Origin is deferred to its own ADR (ADR-0014), so no role should own it yet.

## Proposals

Each is a recommendation. The words in quotation marks are proposed additions to the owner's text; none is in CLAUDE.md.

### 1. A role for original source, provenance and licensing

The directive gives this a priority (P4), a whole part (§3–§7), an agent (§77, 25), a step of every task (§83, 17) and three final rules (2, 3, 27). Decision items 91–97, 121 and 122 add the rules for reading Linux and other open-source code. In the roles it is one phrase of role 2: "and original-source policy". Roles 25, 27, 31 and 47 hold provenance and licences for builds, specifications, artifacts and firmware, and none holds them for the source.

> 51. Original-Source, Provenance, and Licensing Lead. Own the original-source policy, source provenance records, the rules for reading reference code, licence compatibility, third-party notices, runtime-dependency exceptions, and contribution sign-off. Keep the reading of reference code apart from the writing of Origin code, record every source consulted, and check similarity before publication.

### 2. A role for the NPU and the Compute API

P5 names NPUs. §37 is a section, §77 has an agent for it (14), §49 lists Compute, and M11 is "graphics acceleration and compute". In the roles it is the last three words of role 13: "supported compute accelerators". No role names accelerator discovery, tensors, a model runtime or the CPU fallback.

> 52. Compute and AI Accelerator Architect. Own the Origin Compute API, accelerator discovery, tensor memory, data transfer, asynchronous execution, scheduling across CPUs, GPUs, and NPUs, and model-runtime integration. Report device capabilities truthfully and qualify CPU fallback, error recovery, and firmware dependencies.

### 3. Role 19: the SDK-only rule's enforcement

Role 19 owns coverage: every device standard and management operation has an SDK interface backed by a real service. It does not hold the other half of §48: that no first-party client has another way in. That half is final rule 11, ADR-0011 and its Amendment 1 (decision item 108).

> Add to role 19: Enforce the SDK-only rule for every first-party client: no hidden shell commands, no launching of another program to do a client's work, and no parsing of another program's text output.

### 4. Role 20: the GUI as a security boundary, international text, and visual design

§52 makes the compositor a security boundary between programs (decision item 116). No role says so: role 20 has no security words, role 29 covers the kernel and drivers, and role 16 speaks of system boundaries in general. §51's text engine is "text rendering, typography, internationalization" in role 20, without input methods, bidirectional text or complex scripts, which Telugu, Hindi and Arabic need (§51's own test list). §50's themes and animation are in no role.

> Add to role 20: Own input methods, bidirectional and complex-script text, font fallback, themes, and animation. Keep the compositor a security boundary: isolate each client's surfaces and input, treat synthetic input, screen capture, global shortcuts, and clipboard access without a user action as capabilities, and draw credential and elevation prompts on a trusted path.

### 5. Roles 13 and 14: the device classes the directive names

"Video", "camera", "print" and "scan" are in no role. §36 requires video acceleration, §34 video devices, §56 cameras, webcams, printers, scanners, pen and game controllers, and §49 a Printing interface. Role 14's "approved peripherals" can be read to cover them, and then nothing shows they were meant.

> Role 13: "... shader compilation, composition, video acceleration, and supported compute accelerators."
>
> Role 14: "... keyboards, pointing devices, touch, pen, game controllers, cameras, printers, scanners, HID, and approved peripherals."

### 6. Role 8: hardware errors and health. Role 50: remote administration and remote desktop

ADR-0014 S7 makes hardware health one of the four interfaces the Server edition needs first: machine-check and memory errors, PCIe errors (§32), sensors, and the BMC where it is documented. No role names any of them.

§48 gives three ways to administer a server and says the remote desktop session must exist before the Server edition ships; ADR-0014 S4 gives it an ADR of its own. Role 50 has "remote and BMC-console diagnostics", which is diagnosis, not administration.

> Add to role 8: Own hardware error reporting and hardware health: machine checks, memory errors, PCIe errors, and sensors.
>
> Role 50: "... secure administration through native SDK services, the native management GUI, remote administration, remote desktop sessions, and remote and BMC-console diagnostics."

### 7. Twelve smaller additions

| Role | Words to add | The directive |
|---|---|---|
| 7 | "the boot manager, deployment selection, and boot-attempt records" | §20 |
| 7 | "ACPI runtime evaluation" | Battery, thermal zones, the lid and power button, and the embedded controller (§67) are described on most machines by code in the firmware's ACPI tables, which the system must run. Neither a role, a section nor an ADR names that interpreter. It runs firmware-supplied code, so it needs an ADR of its own before M10 |
| 12 | "the firewall, TLS integration, and VPN interfaces" | §54, §58 |
| 17 | "side-channel and constant-time analysis, and secret zeroization" | §59 |
| 18 | "the Origin runtime library and the route to Rust `std`" | §11, §44 |
| 21 | "session locking, user switching, administrative authorization, and device ownership" | §57 |
| 25 | "continuous integration and the automated gates" | The header, §8, and every rule that says "a gate checks" |
| 26 | "private vulnerability reports and coordinated disclosure" | §6, §60. The first text's role 26 had "vulnerability response" |
| 28 | "session hand-offs and durable task state" | §79, §83 step 18, §77 agent 26 |
| 30 | "Secure Boot, TPM use, and measured boot" | §19, §58 |
| 45 | "milestone exit tests" | §82. The first text's role 28 tracked "verified milestones" |
| 49 | "search and notifications" | §49, §50 |

### 8. Five wordings of the first text to restore

| Where | The first text | The second text | Why it matters |
|---|---|---|---|
| Opening sentence | "end-to-end technical accountability" | "technical accountability" | The list that follows names no boot chain, installation, updates, recovery or release. Without "end-to-end" it can be read as the limit |
| Role 6 | "memory accounting" | "accounting" | Says which accounting |
| Role 15 | "clock synchronization" | "synchronization" | Elsewhere in the text the word means locks |
| Role 25 | "release qualification" | "qualification" | Role 24 is the qualification lead |
| Role 24 | "Record whether evidence comes from host tests, emulation, virtualized execution, or physical hardware" | "Record each test environment" | Final rule 30 and §74 turn on exactly these four kinds |

### 9. One sentence for subjects with two owners

Two or three roles hold the same thing in several places:

- entropy, keys and signatures: 17 and 31;
- provenance, artifact integrity and signing: 25 and 31;
- authenticated updates and rollback: 22 and 31;
- SDK documentation, references and examples: 19, 37 and 48;
- frame pacing and input latency: 13, 20 and 35;
- the editions' installation and deployment profiles, workload policies and BMC console: 39, 49 and 50;
- window management and the desktop: 20 and 49.

The pattern looks deliberate: one role builds and another checks, which the second opening paragraph asks for ("independent review"). Said once, it becomes a rule, and no subject is left with two owners or none.

> Add to the closing paragraphs: Where two roles cover one subject, the Architect decides its design, and the Lead reviews it independently and owns the acceptance evidence. The Chief Operating System Architect settles any other overlap.

The sentence says who decides. A Lead may still change the code: roles 33 and 34 say "Optimize".

For the editions this reads: role 39 owns what the editions share and where they may differ; roles 49 and 50 each own everything inside one edition.

### 10. How "Every role must define ..." is read

The first closing paragraph asks every role for security invariants, performance budgets, resource limits, failure behavior and acceptance evidence. Roles 24, 27, 28, 36, 41, 45, 46 and 48 own no running code, so a performance budget or a resource limit has no literal meaning for them. The paragraph also does not say where these are written.

The reading proposed, with no change to the text: each role defines them for the areas it owns, in the place that area already has: its ADR, its specification, its threat model, `PERFORMANCE_BUDGETS.md` and each task's contract, which §78 already requires to state security invariants, performance constraints, tests and acceptance conditions. Where one of the five does not apply, the record says so and why. No stand-alone document is written for each role (§7).

### 11. §77: point to §1

§77 still lists 26 agents. The second closing paragraph already separates the two ideas: roles are responsibilities, and tasks are assigned by source ownership, independence, resources and integration needs. With proposals 1, 2 and 7 in, every agent of §77 has a role, and the list only repeats §1 in fewer words, with a different count.

> Replace §77's list with: The specialist roles and their responsibilities are in §1. Agents are given bounded tasks from them, as §1 and §78 say.

§77's last line stays: "Do not execute every agent simultaneously without task independence."

### 12. A task for the records role 45 owns

Role 45 owns "the approved requirement inventory, coverage matrix, acceptance criteria, dependencies, evidence freshness, and completion decisions". The repository has no requirement inventory and no coverage matrix, so the completion target of the second opening paragraph, 100% coverage of approved requirements, cannot be measured today. `docs/ROADMAP.md`, which §82 names for the milestones' exit tests, does not exist either; it is part of TASKS item 25.

The proposal is a task of its own: one row for each approved requirement of CLAUDE.md, the accepted ADRs and the decision sheet, with its owning role, implementation, tests, evidence and state, and a gate in `cargo xtask check` that fails when a row marked done names evidence that is not there.

## What was checked and found right

- Each of the 50 roles has a title of its own, the numbers run from 1 to 50 without a gap, and the text has no doubled word and no character outside plain ASCII.
- The nine roles of revision 7.4's sentence map to roles 1, 2, 3, 8, 16, 19, 20, 23 and 28.
- §77's agents 1 to 24 map to roles; agent 14, the NPU, only to the three words of role 13 (proposal 2).
- Role 39's "common kernel, SDK, desktop, and release identity", role 50's "native management GUI" and "BMC-console diagnostics", and role 41's KVM guests agree with ADR-0014 and ADR-0009.
- Role 19's "every approved device standard ... has the required SDK coverage" agrees with ADR-0015, and role 30's mitigations "on supported configurations" with ADR-0017.
- The second closing paragraph agrees with §77's last line and with §78.

## After the check: the owner's third text, and what Claude changed in it (decision item 168)

Later on 2026-11-11 the owner sent a third text for §1, with these words: "Consider following, and improve if you can. Consider extreme security, performance, Native SDK, Native Desktop, Native GUI or UI, CPU, GPU, NPU consider these also. You can do extremely very good, You decide."

The third text has seven opening paragraphs, 64 specialist roles and 22 engineering requirements that apply to every role. Roles 1 to 50 keep their numbers and subjects; all but two are reworded. Roles 51 to 64 are new: a Chief Systems Architect, system integration, resource governance, platform reliability, and ten roles for driver families, the GPU and the NPU among them.

CLAUDE.md 7.5 holds that text with the 47 changes listed below. They are Claude's, made under the owner's delegation, and the owner can change any of them.

### What became of the twelve proposals

| # | Proposal | In the owner's third text | Changed by Claude (item 168) |
|---|---|---|---|
| 1 | A role for original source, provenance and licensing | A requirement, "Preserve original implementation and provenance", and a sentence in role 2. No role | Role 67 |
| 2 | A role for the NPU and the Compute API | Role 56, and role 55 for the GPU | The NPU added to role 53's resource policies |
| 3 | The SDK-only rule's enforcement | Not taken | Opening paragraph 5, role 19 and a requirement |
| 4 | The GUI as a security boundary; international text; themes | Not taken | Roles 65 and 66; role 20 |
| 5 | Video, cameras, printers, scanners | Roles 62 and 63; pens and game controllers in role 61; media capabilities in roles 13 and 55 | Nothing |
| 6 | Hardware errors and health; remote administration and remote desktop | Role 54 for hardware faults | Remote administration and remote desktop in role 50 |
| 7 | Twelve smaller additions | Vulnerability response (role 26); session hand-offs (a requirement); side-channel assessment (role 17); ACPI coordination (role 64) | The boot manager (7); ACPI runtime evaluation (8); the firewall, TLS and VPN (12); constant time and zeroization (17); the runtime library and `std` (18); sessions and elevation (21); continuous integration (25); Secure Boot, the TPM and measured boot (30); milestone exit tests (45); search and notifications (49) |
| 8 | Five wordings restored | The accountability sentence rewritten from hardware to recovery; "qualification records" (25); the four kinds of evidence (a requirement) | "memory accounting" (6); "clock synchronization" (15) |
| 9 | One sentence for subjects with two owners | Role 31 validates and no longer owns; role 39 coordinates; a requirement routes system, OS, kernel and SDK decisions | Routes for security and performance; the Architect and Lead sentence; roles 16 and 23 take the chief titles the opening names |
| 10 | How "Every role must define ..." is read | The paragraph is gone; requirements replace it | Nothing |
| 11 | §77 points to §1 | Not answered | Nothing. §77 is as it was |
| 12 | A task for the requirement inventory and coverage matrix | The matrix is now the measure of the completion target (opening paragraph 3, a requirement, role 45) | TASKS item 35 |

### The 47 changes

"Why" names the area the owner asked to be considered, and the rule the change rests on.

| # | Where | Change | Why |
|---|---|---|---|
| 1 | opening 1 and 3, role 1 | The typographic apostrophe, three times, becomes the plain one | the plain apostrophe is what the rest of the directive uses |
| 2 | opening 2 | "system services, native SDKs, applications," becomes "system services, the native SDK, the native desktop and GUI, applications," | one SDK, not several (the header: one kernel, one SDK); the native desktop and GUI named |
| 3 | opening 5 | "original-source policy, architecture decisions," becomes "original-source policy, SDK-only rule, architecture decisions," | the SDK-only rule stands beside the other approved baselines (§48) |
| 4 | opening 6 | Now: "Begin every engineering session by establishing the actual source revision, repository state, implemented behavior, recorded decisions, task ownership, and current verification evidence before selecting the next task. Continue development through working, reviewable, tested milestones." It was: "Begin every engineering session by inspecting the existing implementation, repository state, recorded decisions, task ownership, and verification evidence. Continue development through working, reviewable, tested milestones." | the first requirement said the same thing; the two are one sentence now, with every element of both |
| 5 | opening 7 | "64 specialist roles" becomes "67 specialist roles" | three roles added |
| 6 | role 3 | Added at the end: "Keep the privileged framework small, with `unsafe` code and assembly confined to the files the gate lists." | security: the Framekernel's defining boundary (§13, decision items 110 and 111) |
| 7 | role 4 | Added at the end: "Apply the selected speculation controls and processor protections on every processor, at startup and after resume, and verify that they are in effect." | CPU, security: controls are per processor and a reset clears them (ADR-0017 Z1) |
| 8 | role 6 | "kernel heaps, accounting, and allocation failure" becomes "kernel heaps, memory accounting, and allocation failure" | says which accounting |
| 9 | role 7 | "boot handoff, and recovery entry." becomes "boot handoff, the boot manager with deployment selection and known-good fallback, and recovery entry." | the boot manager had no owner (§20) |
| 10 | role 8 | Added at the end: "Own ACPI runtime evaluation, and validate the tables and methods the firmware supplies." | battery, thermal, lid and embedded-controller support (§67, role 64) run firmware-supplied ACPI code; nothing named its owner |
| 11 | role 9 | Added at the end: "Run third-party drivers only in user mode, confined by the IOMMU, and keep kernel-mode drivers first-party." | security: the accepted driver rule (§47, ADR-0001) |
| 12 | role 12 | "sockets, routing, configuration, DNS, DHCP," becomes "sockets, routing, the firewall, TLS integration, VPN interfaces, configuration, DNS, DHCP," | security: the firewall, TLS and VPN interfaces had no owner (§54, §58) |
| 13 | role 15 | "wall time, timers, synchronization, and defined behavior" becomes "wall time, timers, clock synchronization, and defined behavior" | elsewhere in the text the bare word means locks |
| 14 | role 16 | "16. Security and Isolation Architect. Maintain system threat models" becomes "16. Chief Security Architect. Hold security authority across both editions and every subsystem. Maintain system threat models" | the opening names a Chief Security Architect and no role had the title; role 3 was retitled the same way |
| 15 | role 16 | "memory protection, DMA isolation," becomes "memory protection, exploit mitigations, DMA isolation," | security: W^X, ASLR, shadow stacks and guard pages (§14, §58) |
| 16 | role 16 | Added at the end: "Accept no change that weakens a security invariant in exchange for performance or convenience." | security: priority P0 and final rule 16, as an authority |
| 17 | role 17 | "side-channel assessment, and independent review" becomes "side-channel and constant-time assessment, secret zeroization, and independent review" | security: §59's list |
| 18 | role 18 | Added at the end: "Own the Origin runtime library and the route to Rust `std`. Keep system-call numbers private to each release, with the system libraries' C ABI as the stable binary interface, and verify each native image before parsing it." | SDK, security: the runtime library had no owner (§44); the stable interface (§42); verify before parse (ONBF-R1) |
| 19 | role 19 | "documentation, and API evolution. Define capability discovery," becomes "documentation, and API evolution. Define each interface once in the Origin interface language, generate its bindings from that definition, and keep the C ABI stable and versioned. Define capability discovery," | SDK: one source of truth for every interface (ADR-0011) |
| 20 | role 19 | Added at the end: "Enforce the SDK-only rule for every first-party client." | SDK: coverage had an owner, the ban on any other way in had none (§48) |
| 21 | role 20 | "Own window management, desktop interaction, native controls, input responsiveness," becomes "Own Origin UI: window management, desktop interaction, native controls, layout, event dispatch, themes, animation, clipboard, drag and drop, input responsiveness," | GUI: the rest of §50's toolkit |
| 22 | role 20 | Added at the end: "Build every first-party graphical interface with Origin UI, with no web view, browser engine, or foreign toolkit." | GUI: native means no HTML, Electron or browser process (§50) |
| 23 | role 21 | "first-run setup, authentication, permissions," becomes "first-run setup, authentication, login sessions, session locking, user switching, administrative authorization, permissions," | security: §57's session and elevation rules had no owner |
| 24 | role 22 | Added at the end: "Keep installation declarative: no install-time code runs." | security: ONBF-R3 (§46) |
| 25 | role 23 | "23. Performance and Reliability Architect. Establish workload-specific targets" becomes "23. Chief Performance Architect. Hold performance authority across both editions and every subsystem. Establish workload-specific targets" | the opening names a Chief Performance Architect and no role had the title |
| 26 | role 23 | "and test sustained operation." becomes "and test sustained operation and recovery from faults." | reliability stays in the role now that it has left the title (the second text's words) |
| 27 | role 25 | "Own reproducible builds, dependency inventories," becomes "Own reproducible builds, continuous integration and its gates, dependency inventories," | the gates enforce most of the directive's rules and had no owner |
| 28 | role 30 | "Own the boot trust chain, firmware validation," becomes "Own the boot trust chain, Secure Boot, TPM use, measured boot, firmware validation," | security: §19 and §58 name all three |
| 29 | role 45 | "acceptance criteria, dependencies, evidence freshness," becomes "acceptance criteria, milestone exit tests, dependencies, evidence freshness," | §82: no milestone is done without its exit tests |
| 30 | role 49 | "Integrate the shell," becomes "Integrate the desktop shell," | desktop: the bare word also means the command shell, which the base image does not hold (§48) |
| 31 | role 49 | "sessions, settings, native applications," becomes "sessions, settings, search, notifications, native applications," | desktop: §49 lists both and no role named them |
| 32 | role 49 | Added at the end: "Make every setting and management task available in the native GUI." | desktop: the GUI is always enough (§48) |
| 33 | role 50 | "the native management GUI, and remote and BMC-console diagnostics." becomes "a native management GUI that covers every administrative operation, authenticated and encrypted remote administration, remote desktop sessions, and remote and BMC-console diagnostics." | server, security: §48's three ways to administer a server; remote desktop must exist before the edition ships (ADR-0014 S4) |
| 34 | role 51 | "or shared architectural constraints." becomes "or shared architectural constraints to the owner." | says to whom |
| 35 | role 53 | "network, GPU, and background-work" becomes "network, GPU, NPU, and background-work" | NPU: §37 schedules work across CPU, GPU and NPU |
| 36 | role 55 | Added at the end: "Keep a validated fallback display path, and claim acceleration only for a working native driver." | GPU: §36's two closing rules |
| 37 | role 65 | A new role; its text is in CLAUDE.md §1 | GUI, security: §52 had no owner, and its security rules (decision item 116) were in no role |
| 38 | role 66 | A new role; its text is in CLAUDE.md §1 | GUI: §51's text engine had three words in role 20; fonts are parsed from files programs supply |
| 39 | role 67 | A new role; its text is in CLAUDE.md §1 | priority P4 and Part II had a requirement and no owner (§3 to §6, decision items 91 to 97 and 121) |
| 40 | requirement 1 | Removed: "Inspect and continue existing work. Establish the actual source revision, repository state, implemented behavior, relevant decisions, and current evidence before selecting the next task." | merged into opening paragraph 6 |
| 41 | requirement: ownership and authority | "kernel contracts through the Chief Kernel Architect, and SDK contracts through the Native SDK Architect." becomes "kernel contracts through the Chief Kernel Architect, SDK contracts through the Native SDK Architect, security decisions through the Chief Security Architect, and performance budgets through the Chief Performance Architect. Where an Architect and a Lead cover one subject, the Architect decides its design and the Lead verifies it independently." | security and performance had no route; several subjects have an Architect and a Lead |
| 42 | requirement: independent review | Added at the end: "State who reviewed each item, and say so when no person has." | so far every review has been by another AI session; the records say so and the rule should |
| 43 | requirement: graphics and accelerators | "Test both output correctness and resource isolation." becomes "Test both output correctness and resource isolation, including that no client can read memory another client left behind." | GPU, NPU, security: the isolation failure these devices are known for |
| 44 | requirements | A new requirement: "Reserve the owner's decisions." Its text is in CLAUDE.md §1 | the roles hold strong authority and none of them is the owner (§5, §80, §84; how work lands, decision item 70) |
| 45 | requirements | A new requirement: "Enforce the SDK-only rule." Its text is in CLAUDE.md §1 | SDK, GUI: the rule the owner asked for first, in the requirements every role reads (§48, ADR-0011) |
| 46 | requirements | A new requirement: "Discover and report capabilities truthfully." Its text is in CLAUDE.md §1 | CPU, GPU, NPU: §24's discovery rule and ADR-0015's truthful reporting, for every kind of hardware |
| 47 | requirements | A new requirement: "Deny by default." Its text is in CLAUDE.md §1 | security: the accepted capability model (ADR-0005 P1) and failing closed, as one rule for every role |

### What was left as it was

- **The numbers of roles 1 to 64.** The new roles are 65 to 67, so no number moves. The Chief Systems Architect stays role 51, though the requirements route system-wide decisions through it first.
- **§77.** It still lists 26 engineering agents (proposal 11).
- **"SATA/AHCI" in role 57.** The directive names NVMe and USB storage and no SATA controller. The word "approved" in that role keeps AHCI out until a decision adds it.
- **The three statements of the requirements matrix:** the target in opening paragraph 3, the rule in the requirements, the owner in role 45.

### How the changes were made and checked

- **Made by a script from the owner's words.** The text was taken from the owner's message. Each change is one entry that must apply exactly once, or the build stops. So the table above is the whole difference in words.
- **Form.** Role titles and requirement headings are bold, the list marker is "-", and the apostrophe is the plain one. No word changes for these.
- **Each addition rests on a rule that was read on the day,** in CLAUDE.md 7.5 or in ADR-0001 (D2), the ONBF specification (R1, R3), ADR-0005 (P1), ADR-0011 (the interface language, `origin.core`, N12), ADR-0014 (S4, S7), ADR-0015 (D2), ADR-0017 (Z1) and decision item 70. The one exception is ACPI runtime evaluation, which no section or ADR names: role 8 now owns it, and how it is built is still to be decided.
- **Not done.** No person has reviewed these changes, and the owner had not read them when this was written. Nothing was built or tested: this is text.
