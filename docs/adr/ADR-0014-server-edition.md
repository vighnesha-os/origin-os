# ADR-0014: Origin Server is an edition of the same release, with the same native desktop

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-10, as recommended ("Accept all", decision sheet items 71–76 and 81; CLAUDE.md §18). **Amendment 1 (below): Accepted** by the owner on 2026-11-10 (item 117); it rewords S4. Nothing here is built. It is not an ABI or format decision: how an edition is written into the release manifest is left to a later format decision (§84). |
| Requires | CLAUDE.md P0, P1, P2, P3, P6, P8; header (production objective, target hardware); §1, §11, §26, §44, §48–§50, §52–§54, §57–§59, §61, §62, §64, §69, §74, §76, §82; final rules 10–12, 14, 19, 30, 40 |
| Evidence | The owner's direction (below); ADR-0009's KVM boots on GitHub's runners; ADR-0010's screen-dump test; the checks in Evidence |
| Builds on | ADR-0009 (hardware targets, H2), ADR-0010 (the GOP framebuffer console), ADR-0011 (N1, N2, N7, N9, N11) |
| Would change | CLAUDE.md header, §44, §48, §49, §74, §76, §82 and final rule 12 (revision 7.3) |

## Context

CLAUDE.md describes "a … GUI-first, SDK-native … desktop operating system". On 2026-11-10 the owner widened that:

- "I am building Desktop OS, and Server OS with Native GUI and OS as Native SDK".
- "I want server also should have native UI or GUI not terminal".
- "Can we run server also native GUI desktop and built complete OS as Native SDK?"

So the question is not whether a server gets a GUI. It is how a server edition fits one unified release (§62), when it is built, and what a server needs beyond the desktop: remote administration, several administrators at once, workloads that must not suffer, and hardware a desktop never meets.

## Decisions

### S1. Two editions of one release (item 71)

Origin Desktop and Origin Server are installation profiles of the same unified release. They share one kernel, one SDK, one UI and compositor, the same desktop and one signed release manifest (P6, §62). **Origin Server installs the full Origin desktop by default.** The editions differ only in defaults:

| | Desktop | Server |
|---|---|---|
| Services started | Desktop set | Server set (S7), plus the desktop |
| Camera, microphone, Bluetooth, audio | On | Off by default; the same packages install on request (§61) |
| Scheduling | Foreground interaction first | Workloads first; an active console session is interactive (S5) |
| Update activation | User's choice | At a time an administrator sets (§64 staging) |

No kernel or SDK code is compiled differently per edition (P8). How the manifest names an edition is a format decision for later (§84).

### S2. Desktop first (item 72)

The desktop goes through M9 first. The server's GUI is M8's UI and compositor, so it arrives with them. Server-only work starts after M9: the remote administration connection, remote desktop, service management and hardware health (S4, S7). Remote administration needs M10's networking and new cryptography (§59). The server edition is qualified for its own declared scope (§76, rule 40).

### S3. The first server platform is a KVM guest showing its GUI on the VM's display (item 73)

The display is the firmware's framebuffer (ADR-0010 E1), drawn by the software renderer (§52). Input comes from a USB keyboard and tablet through xHCI (§34). Storage is NVMe and the network is virtio-net, both of which Origin already drives under QEMU. GitHub's free runners boot Origin through KVM (ADR-0009), and `cargo xtask boot-test` already checks a QEMU screen dump (ADR-0010 Evidence), so GUI tests can run in CI.

A virtual machine is its own declared scope and is not hardware qualification (rule 30). Physical servers come later, on platforms in the declared scope, such as AMD's EPYC Venice (Zen 6) and Intel's Xeon 6+ Clearwater Forest (header; ADR-0009 Amendment 1). Older rented servers are bench equipment only. On a physical server the console is the display the baseboard management controller (BMC) offers through the firmware, seen through the BMC's remote console, with the BMC's keyboard and mouse appearing as USB devices. That is to be verified on the first server target (What this does not decide).

### S4. Administration only through the native GUI (item 74)

There are three routes, all SDK clients with no private doors (ADR-0011 N2):

1. **The server's own console**, locally or through the BMC's or the VM's remote console.
2. **The Origin management app on another Origin machine**, making the same SDK calls over an authenticated, encrypted connection (S7's remote administration).
3. **A remote desktop session per administrator.** A BMC gives one screen, and a server often has several administrators at once (§57). Remote desktop gets its own ADR (protocol, encryption, authentication, threat model) and **must be built before the server edition ships.**

The terminal stays an optional development tool (§48) and is never needed to administer either edition. There is no web administration page (§1, §50). Boot and panic text (ADR-0010; ADR-0011 N11) still goes to the screen and the serial port, where a BMC's serial-over-LAN can show it. That is diagnostic output, not a way to administer.

### S5. Server sessions are cheap and closed (item 75)

- With nobody logged in, only the login screen runs.
- Sessions run unprivileged, with the logged-in user's rights. Administration needs explicit elevation (§57).
- The compositor draws only where something changed (§52), so an idle desktop costs workloads almost nothing (P1).
- A console session gets interactive scheduling priority only while someone is using it. Otherwise workloads keep priority (§26).
- Nothing in the compositor or UI listens on a network. The remote administration endpoint and the remote desktop service are the only network-facing parts, and both authenticate every connection (§58).
- A console seen through a BMC is limited by the BMC's video compression, which Origin does not control. It gets its own §53 thresholds, declared per server target.

### S6. Software written for other systems: later (item 76)

Not now. Rust `std` comes first (§44). A POSIX library over the SDK would need its own ADR at M6. Linux APIs stay banned (§11). Until then the server edition runs only Origin-native programs, which is its largest limit as a server.

### S7. Server SDK areas (item 81)

§49 gains four areas, each defined once in OIDL (ADR-0011 N7). Local and remote callers use the same interfaces.

| Area | Covers |
|---|---|
| Remote administration | Connections, authentication, sessions; carrying any SDK interface to a remote caller |
| Service management | Start, stop, supervise, restart policy, dependencies, logs (§69) |
| Hardware health | Machine-check and memory errors, PCIe errors (§32), sensors, and the BMC where its interface is documented |
| Remote desktop | Sessions, display, input, clipboard, with S4's ADR |

Virtualization and containers come later, with their own ADR.

## Alternatives

| Alternative | For | Against |
|---|---|---|
| Desktop only, no server edition | Less work | Not what the owner wants |
| A separate server OS or kernel | Freedom to diverge | Breaks one release identity (P6, §62); two kernels to secure |
| A headless server administered from a terminal | Smallest attack surface; the usual server practice | Contradicts the owner's direction and ADR-0011 N2 |
| A management console only, without the full desktop (Claude's first recommendation) | Less code on the server | The owner chose the full desktop (2026-11-10) |
| A web administration page | Works from any machine with a browser | §1 and §50 rule out browser-based UI; a web stack is a large second surface |
| The BMC's console as the only remote GUI | No new protocol | One screen, so one administrator at a time; no Origin authentication |

## Evidence

- The owner's words above, 2026-11-10.
- KVM boots of Origin on GitHub's hosted runners: ADR-0009, "Free KVM on GitHub's hosted runners".
- The screen-dump check of the framebuffer console: ADR-0010, Evidence.
- Nothing in this ADR has been built or measured. The cost of an idle desktop on a server (S5) is a claim to measure once M8 exists (rule 21).

## Trade-offs

- A full desktop on every server adds code and memory to machines that are often network-exposed. S5 limits what runs and what listens, but the attack surface is larger than a headless server's.
- One UI and one SDK for both editions means one thing to build, learn and secure, and server administrators get the same tools as desktop users.
- Remote desktop is a large protocol to design and secure, and the server cannot ship without it (S4).
- The server edition waits for M9. In return it gets a finished desktop instead of a second, thinner UI.
- Without software from other systems (S6), the server edition is limited to Origin-native programs for a long time.

## Migration consequences

- CLAUDE.md revision 7.3 states the two editions, the administration routes, the SDK areas, the server test platform, qualification per edition and the server's place in the milestones.
- Nothing built changes. No interface, format or on-disk structure exists for editions yet.
- Later decisions this one creates: the edition field in the release manifest (§84), the remote desktop ADR, a server-targets amendment to ADR-0009, the POSIX question at M6, virtualization and containers.

## Threat model (§58)

| Interface | Attacker | Defence |
|---|---|---|
| Remote administration endpoint | Anyone on the network: guessing credentials, replaying, flooding | Mutual authentication and encryption before any SDK call; authorization at each service, not at the transport (ADR-0011 N9); rate limits; audit log (§69) |
| Remote desktop service | The same, plus input injection and clipboard leakage | Its own ADR. The same authentication; per-session isolation; clipboard sharing off unless the user turns it on |
| A desktop session on a server | A user, or a page a user opens, attacking the machine | Sessions unprivileged; elevation explicit (§57); consumer device services off (S1) |
| The BMC's console | Whoever controls the BMC | Out of Origin's control. Treated as physical access: it sees the screen and types on the keyboard |
| Hardware health records | A local program reading error records to learn about other workloads | Readable only with an administrator right |

## What this does not decide

- The remote desktop protocol (its own ADR, before the server ships).
- How the release manifest names an edition (§84).
- Physical server targets: an ADR-0009 amendment once 2026-generation Xeon and EPYC platforms can be reached. Whether their BMC displays and virtual USB devices behave as S3 assumes is unverified.
- Virtualization, containers, clustering.
- Whether Origin ever runs POSIX software (S6, at M6).

## Questions for the owner (answered 2026-11-10: "Accept all")

1. Server is an edition of the same release and installs the full desktop by default (S1)? **Yes** (item 71).
2. Desktop through M9 first, server-only work after (S2)? **Yes** (item 72).
3. The first server platform is a KVM guest with its GUI on the VM's display (S3)? **Yes** (item 73).
4. Administration only through the native GUI; remote desktop built before the server ships; no web administration page (S4)? **Yes** (item 74).
5. Server sessions cheap and closed (S5)? **Yes** (item 75).
6. Software written for other systems decided later, Rust `std` first (S6)? **Yes** (item 76).
7. Four server SDK areas added to §49 (S7)? **Yes** (item 81).

## Amendment 1 (Accepted, 2026-11-10): what "only through the native GUI" means, and three requirements on remote administration

**Status: Accepted by the owner on 2026-11-10, as recommended ("Accept all", decision sheet item 117; CLAUDE.md §18). It rewords S4 and item 74. Nothing is built.** The evidence is finding 12 of `docs/engineering/DESIGN_REVIEW_2026-11-10.md`.

| # | Decision |
|---|---|
| A1 | **S4's rule is that the GUI is always enough.** Every administrative operation is available in the native GUI, and none needs a terminal. S4's three routes stand. The rule does not forbid other SDK clients, which ADR-0011 N2 and CLAUDE.md §70 already allow. |
| A2 | **Installing without an operator, and configuring many machines, are done through the SDK.** Origin Setup applies a signed, declarative provisioning document. No script runs (ONBF-R3). The document's format is a decision of its own (§84). |
| A3 | **The remote-administration ADR (S7) must provide three things.** (a) Each remote session is a process of its own under the authenticated principal, holding only that session's handles, so the kernel's stamp names the administrator and not the endpoint (ADR-0011 N9). (b) A stated list of the interfaces that may be carried to a remote caller. This narrows S7's "any SDK interface". (c) A published protocol, with a hybrid key exchange (X25519 with ML-KEM-768, FIPS 203) to match the signature profile. FIPS 203 joins CLAUDE.md §17 with that ADR. |

**Why.**

- "Only" left no place for a machine nobody sits at when it is installed, or for a hundred machines that need the same setting. Neither needs a terminal, and neither is a GUI.
- The kernel stamps each message with the process that wrote it. An endpoint that relays a remote caller's calls is stamped as itself, so every service would apply its policy and its audit to the endpoint.

**Alternatives.**

| Alternative | For | Against |
|---|---|---|
| Keep "GUI only" to the letter | The simplest reading of the owner's words | Every server is installed and configured by hand, one at a time |
| A scripting interface for administrators | Familiar to administrators | It is the second control plane ADR-0011 N1 removes |
| One endpoint process that names the administrator in each message it relays | Fewer processes | Services would trust a claim in the data, not the kernel's stamp; one flaw in the endpoint speaks for every administrator |

**Trade-offs.** A process for each remote session costs memory, and the list of carried interfaces has to be kept as interfaces are added. In return a remote administrator has exactly the rights of the same person at the console, and no more.

**Migration consequences.** None: nothing is built, and no interface or format exists for any of it. CLAUDE.md §48 says A1 and A2 in revision 7.4.

**Threat model (§58), one more row.**

| Interface | Attacker | Defence |
|---|---|---|
| The remote administration endpoint, as a deputy | A remote caller who gets the endpoint to use its own authority for the caller's request | Each session is a process under the caller's principal with only that session's handles (A3a); only listed interfaces are carried (A3b) |
