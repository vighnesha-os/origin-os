# The roles of CLAUDE.md §1, checked against the directive, 2026-11-11

| | |
|---|---|
| Asked for | The owner, giving the second text for §1: "Update CLAUDE.md at line: 35 with following, check carefully, if anything missing" |
| Checked | The owner's text as it stands in CLAUDE.md 7.5, §1: 55 lines, which are three opening paragraphs, 50 roles and two closing paragraphs |
| Against | CLAUDE.md 7.5 from its header to the final rules, read in full for this check; the lines of ADR-0014 and ADR-0015 on the subjects below; the 26 agents of §77; the owner's first text for §1 (28 roles, commit `fc92eca` on this branch) |
| How | One AI reader went through the directive section by section and named the role that owns each. A search of the owner's text for each subject then confirmed whether any role names it. "Named by no role" means the words are in no role; where a wider phrase in a role can be read to cover the subject, the finding says so |
| Not done | Nothing was built or tested: this is a check of text. No ADR other than 0014 and 0015 was read again for it. **No human has reviewed this** |
| Outcome | The text is in CLAUDE.md as the owner gave it (decision sheet item 165). The twelve proposals below are put to the owner and are not answered. None of them is in CLAUDE.md |

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
