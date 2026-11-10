# Threat model: the system

| | |
|---|---|
| Required by | CLAUDE.md §58 ("Keep a system threat model"), decision item 112, finding 7 of `DESIGN_REVIEW_2026-11-10.md` |
| Covers | Origin Desktop and Origin Server as whole systems: who attacks them, what they hold, what each edition defends, and which component is answerable for each attacker. The three component models stay the authority for their own boundaries: `THREAT_MODEL_SYSCALLS.md`, `THREAT_MODEL_IOMMU.md`, `THREAT_MODEL_ADDRESS_SPACES.md` |
| Written | 2026-11-10, by TASKS item 19a. Most of what it points at is not built: it is written now so that each component is designed against a stated attacker, as §58 and §83 expect |
| Evidence | None of its own. Where a row says **Proven**, the proof is the component model's, under QEMU only. Nothing has run on hardware (§74). Statements about a processor come from `docs/research/2026-11-10-speculation-processor-hardening.md` |
| Status | The coverage tables record what the documents and the code say today. The nine acceptances at the end are **proposed**: each is the owner's to make, and none is made until the owner answers |

## Words used for state

| Word | Meaning |
|---|---|
| **Proven** | A boot proof or boot test with a negative control exists, under QEMU. The component model or `SOURCE_PROVENANCE.md` records it |
| **Boot-tested** | A boot proof exists; no control is recorded |
| **Built** | Code exists and runs in the boots; no proof is aimed at this attacker |
| **Designed** | An accepted ADR or a rule of CLAUDE.md says what to build; nothing is built |
| **Proposed** | An ADR awaiting the owner |
| **Nothing** | No design and no code |

## What is being protected

| # | Asset | Held where |
|---|---|---|
| S1 | What people store: files, settings, mail, keys they keep | Storage; nothing encrypts it yet |
| S2 | What programs hold while they run: memory, open objects, messages in transit | The kernel's address spaces, handles and channels |
| S3 | Secrets that open other things: passwords, storage keys, recovery keys, session tokens | Not designed (§57, §59) |
| S4 | The system's own integrity: the loader, the kernel, services, installed software, their configuration | Signed images (ONBF); the boot chain (ADR-0002) |
| S5 | Authority: what each principal may do | Handles and their rights; capabilities; the administrator's elevation |
| S6 | Availability: the machine starts, runs and can be repaired | Boot proofs (ADR-0013); recovery (§65), not built |
| S7 | Trust in what Origin and publishers ship: root, release and kernel keys; publisher identities | Not created yet; signing tools exist for test keys |
| S8 | Privacy: what the machine tells anyone else | §61's rules; no telemetry exists |
| S9 | On a server: its workloads, each administrator's actions, and the record of them | Not designed |

## Who the attackers are

| Name | Who | Can | Cannot, by assumption |
|---|---|---|---|
| **A network peer** | Anything that can send the machine a packet or answer one: a host on its network, a router on the path, a client of a service it runs | Send anything, at any rate; replay, drop and reorder; pose as any address | Break the cryptography the profile chooses |
| **A hostile program** | Code running in ring 3 with whatever its manifest was granted: a malicious application, one from an unverified publisher, a first-party service that has been taken over | Run any instruction ring 3 allows, with any timing, on every processor it is given; make any call; time anything it can see | Run in ring 0; hold a handle it was not given |
| **A hostile or compromised device** | A device inside the machine with altered firmware, or one somebody plugs in | Issue any bus transfer and any interrupt; answer its driver with anything; claim to be another kind of device | Change the processor or the platform firmware |
| **A thief, machine off** | Holds the hardware, powered off or hibernated, for as long as they like | Remove and copy every storage device; change what is on them; replace parts | Know a secret that is in nobody's possession but the owner's |
| **A thief, machine locked** | Holds a machine that is running with its session locked, or asleep in memory | All of the above, and attach devices, press keys, restart it, and reach its memory with equipment | Unlock the session |
| **Someone with brief access** | Minutes alone with a machine that then goes back to its owner | Start it from other media, change firmware settings, add or swap hardware, alter the boot partition | Stay to watch |
| **A stolen publisher key** | Whoever holds a registered publisher's signing key | Sign anything in that publisher's name and identifier space | Sign as Origin, as another publisher, or attest a driver |
| **A compromised update server** | Whoever controls what the distribution point sends, and when | Serve old, altered or mixed files; serve different machines different things; withhold; watch who asks | Sign as a release key |
| **A server's own administrator** | A person with administrative authority on an Origin Server, acting in bad faith, or whoever took their account | Everything the role allows | Act as another administrator without that person's credential |

Two parties are not attackers here because nothing defends against them: the platform firmware, and for a virtual machine the hypervisor under it. Acceptance AC1 names them.

## What each edition defends

**Defends** means the design intends to stop it. **Limits** means it bounds the damage or makes it visible. **Accepts** means it does not try, and says so by name below.

| Attacker | Origin Desktop | Origin Server |
|---|---|---|
| A network peer | **Defends.** Nothing listens by default. Every packet is parsed as hostile | **Defends.** The remote administration endpoint and remote desktop are the only listeners, and both authenticate every connection (ADR-0014 S5). **Limits** a flood: a server stays up as far as its quotas reach |
| A hostile program | **Defends** other programs' memory, objects and authority, and the kernel. **Limits** what leaks through timing and speculation | The same. Its programs are fewer and chosen by an administrator, and they include services that face the network |
| A hostile device | **Defends** memory and interrupts from any device. An external device gets nothing until it is authorized | The same. The devices a baseboard controller presents are its console, and are trusted as the console is (AC1) |
| A thief, machine off | **Defends** what is stored, once storage encryption exists. **Accepts** the loss of the hardware | **Defends** the same: disks leave data centres |
| A thief, machine locked | **Limits.** The session stays locked. This model asks one thing of §32's design: a device attached to a locked machine gets no DMA and no driver. **Accepts**, as proposed, that secrets in memory can be read with equipment (AC2) | A running server in someone else's building is that operator's (AC8) |
| Someone with brief access | **Limits.** A changed boot chain does not start, or starts and is seen to have changed. **Accepts** added hardware (AC3) | The same, and a baseboard controller is a standing form of this access (AC1) |
| A stolen publisher key | **Limits.** The software can do only what its manifest declares and the user or administrator grants; revocation quarantines it | The same |
| A compromised update server | **Defends** what is installed, its freshness and its direction: only signed, unexpired and never older. **Accepts** that updates can be withheld for a while (AC9) | The same |
| A server's own administrator | Not an attacker: on a desktop the owner is the administrator | **Defends** two things: every action is tied to the person who took it, and the rules for signed code hold against an administrator too. **Accepts**, as proposed, that an administrator can read what the system can read (AC7) |

## Which component covers each attacker

| Attacker | Comes in through | Covered by | State |
|---|---|---|---|
| A network peer | Packets to the machine | The network stack and a firewall (§54). Today ARP, DHCP and TFTP run in ring 0 inside the kernel's own NIC proof (`framekernel/src/virtionet.rs`); where the stack belongs is TASKS item 23 | **Built** as proof code, with host tests. No model of a hostile peer. No firewall |
| | A service that listens | On a server: the remote administration endpoint and remote desktop, each with an ADR and a threat model of its own (ADR-0014 S4, Amendment 1) | **Designed** in outline |
| | A protected connection | The cryptographic profile's transport (TASKS item 27) | **Nothing** |
| A hostile program | System calls | `THREAT_MODEL_SYSCALLS.md` | **Proven** or boot-tested row by row. Open: E8, E11, E12, O2, V2, I2 |
| | Its own address space | `THREAT_MODEL_ADDRESS_SPACES.md` | **Proven** and modelled. Open: T15, T16 |
| | The services it can reach | The directory its launcher builds; the kernel's stamp on every message (ADR-0011 N9; the system-call model's D and W rows) | **Proven** in part. Open: D11 |
| | Starting other programs | The launch capability, a base image with no command interpreter, an empty environment (ADR-0011 Amendment 1) | **Designed** |
| | Other programs' windows, input and clipboard | The compositor's rules (CLAUDE.md §52, decision item 116) | **Designed** as rules. No compositor |
| | The processor's speculation, and timing | `ADR-0017` | **Proposed** |
| | Its package, and what it asks for when installed | ONBF's tiers, declared capabilities and sandbox (ADR-0001 Decision 4; `ONBF.md` §7.7) | **Designed**. The container's parsers are built and host-tested; nothing installs a package |
| | Devices, through the SDK | Rights for dangerous operations (ADR-0015 D3) | **Designed** |
| | Using everything up | Quotas (ADR-0005 P11) | **Boot-tested** for channels and queued messages (O1). Open: O2 |
| A hostile device | DMA and interrupts | `THREAT_MODEL_IOMMU.md` | **Proven** for confinement and for remapped interrupts. Open: T5, T6, T8, T9, T11, T15, T19, T23 |
| | What it tells its driver | Each driver's checks; mutation runs over the parsers (TASKS item 3); third-party drivers in user mode only (ADR-0001 Decision 2) | **Built** for NVMe, virtio-net and PCI. No user-mode driver exists |
| | Being plugged in | External-device authorization (§32; decision item 9) | **Nothing**. No hot-plug exists |
| A thief, machine off | The storage devices | Storage encryption with keys that are not on the disk (§21, §40; TASKS item 27) | **Nothing** |
| A thief, machine locked | The lock screen | The session and the compositor (§52, §57) | **Nothing** |
| | Ports | External-device authorization; the IOMMU model | As above |
| | Memory | Nothing (AC2) | **Nothing** |
| Someone with brief access | The boot chain | Secure Boot with Origin's keys (ADR-0002 B7, Amendment 1); the loader's check of the kernel's signature (B2) | The loader's check is **Proven**, with a test key: Origin's own keys do not exist yet. Keys in firmware and a first stage: **Designed** |
| | A boot chain changed all the same | Measured boot, and secrets sealed to what was measured (ADR-0002 Amendment 2, TASKS item 28) | **Nothing**; the amendment is not written |
| | Firmware settings, added hardware | Nothing (AC1, AC3) | |
| A stolen publisher key | A signed package or update | Capabilities and tiers bound what the software may do; revocation is signed data with an expiry; a driver also needs Origin's attestation (`ONBF.md` §7.4 to §7.7) | **Designed**. No record yet shows a user that a key signed something unexpected: the transparency log is reserved for a later phase |
| A compromised update server | The update channel | Signed manifests under a threshold of release keys; expiry against freezing; a security version against rollback (CLAUDE.md §64; `ONBF.md` §7.6, §8.1) | **Designed**. The security version is not checked yet (decision item 120) |
| A server's own administrator | The management interfaces | A process for each remote session under that administrator's principal (ADR-0014 Amendment 1, A3); elevation (ADR-0014 S5); rights for dangerous operations (ADR-0015 D3) | **Designed** |
| | Unsigned code in ring 0 | Third-party drivers never run in the kernel; only verified code is mapped executable (ADR-0001 Decision 2; ONBF-R4) | **Designed**; the loader's half is **Proven** |
| | The record of what they did | An audit record an administrator cannot quietly change | **Nothing** |

## What the component models put out of scope

Each of the three models excludes physical attacks, firmware and side channels, and a kernel already compromised. For the product, each exclusion is either covered somewhere or accepted by name.

| # | Excluded | By | For the product |
|---|---|---|---|
| EX1 | Physical attack with the machine off | All three | **To be covered** by storage encryption, and by a verified boot chain for what cannot be encrypted. Not an acceptance: it is a requirement before M9. Until then whoever holds the disk reads everything on it |
| EX2 | Physical attack on memory while the machine runs or sleeps | All three; the IOMMU model names memory | **Not covered.** Two possible covers, neither designed: dropping storage keys from memory when the session locks or the machine sleeps, and memory encryption where firmware turns it on. Proposed acceptance AC2 |
| EX3 | Debug ports; hardware added or swapped; signals measured at the machine | The IOMMU model names debug ports | **Accepted by name:** AC3. No software sees or closes a hardware debug port |
| EX4 | DMA through an external port | The system-call and address-space models point to the IOMMU model | **Covered there** (T1, T4 to T9), with that model's open rows. Nothing exists for external devices yet |
| EX5 | A boot chain or firmware setting changed by brief access | All three, as "physical" | **Covered in part:** Secure Boot (Designed) and measured boot (Nothing). What is left is AC3 |
| EX6 | Faults a program induces in memory hardware by how it reads and writes | Named by none of them | **Not covered.** It is a hostile program's attack on a physical weakness. AMD lists a 2025 bulletin on such attacks against DDR5 (AMD-SB-7048; its title and date only were read). Proposed acceptance AC4 |
| EX7 | Malicious firmware; system management mode; a baseboard controller; a hypervisor under a guest | The system-call and IOMMU models | **Buggy firmware is covered:** its tables are parsed as hostile input (the IOMMU model's T21 and T22; TASKS item 3). **Malicious firmware is accepted by name:** AC1. Measured boot will make a changed firmware visible to whoever checks. Calls into firmware after boot are for ADR-0002's Amendment 2 |
| EX8 | Malicious microcode | The system-call and IOMMU models | Origin loads none today. When it does: only from the signed release (§38), and by the kernel alone; loading it belongs in ADR-0015 D3's list of dangerous operations. Microcode that the firmware loaded is accepted with the firmware (AC1). A vendor's own check can fail: AMD-SB-7033, 2025 |
| EX9 | A kernel already compromised another way | All three | **Accepted with the design:** in a framekernel everything in ring 0 is one trust domain (ADR-0011 N6). What reduces the chance: less code in ring 0 (TASKS item 23), the `unsafe` and no-panic gates, shadow stacks (ADR-0017 Z8). What limits the aftermath: a change on disk fails verification at the next boot (ONBF-R2); a change in memory is not detected |
| EX10 | Speculative execution | The system-call model lists it as open (I2) | **Covered by ADR-0017**, which is Proposed. Until it is accepted and built this is open, and it holds up ADR-0011 N14 step 4 |
| EX11 | Speculative use of a stale translation before a shootdown completes | The address-space model | **Covered by that model's own rule,** by design. A frame is released only after every processor that could hold its translation has answered (T1). So a stale translation, followed for real or on a wrong guess, reaches a frame that still belongs to the space giving it up, or to one that still maps it. No boot can show this: QEMU's processor does not speculate. PCIDs (ADR-0004 H5) would change the argument and reopen it |
| EX12 | Other side channels: caches and timing, contention between the two threads of a core, power | The IOMMU and address-space models | **Covered in part.** Code that handles secrets is written constant-time (§59; the cryptographic profile). Sibling threads cannot steer each other's branches (ADR-0017 Z5). Energy counters are not given to unprivileged programs (ADR-0017 Z11). **The rest is accepted by name:** AC5 |

## Acceptances, by name

Each is proposed. Each is the owner's to make, because each says what the product does not promise.

| # | What is accepted | Recommended | Why |
|---|---|---|---|
| AC1 | The platform firmware, system management mode and the processor's microcode are trusted. So is the hypervisor under an Origin guest, and the baseboard controller of a server | **Accept** | Nothing above them can check them. Measured boot is the answer to a changed firmware, and it detects without preventing |
| AC2 | While a machine is locked or asleep, its secrets are in memory, and someone who can read memory with equipment gets them | **Accept for now, and decide again in the storage-encryption ADR** | The two covers each have a price: a locked machine that cannot touch its own encrypted data, or a requirement on the hardware. Neither has been researched |
| AC3 | Tampering with tools: implants, debug ports, bus probes, chips rewritten off the board, and anything measured at the machine | **Accept** | No software defence exists |
| AC4 | A program that induces faults in memory hardware | **Accept, and revisit when the list of declared machines exists** | The defences are in the memory and the platform. Whether declared server machines must have error-correcting memory is a hardware decision (ADR-0009) |
| AC5 | Timing and contention channels between programs, the two threads of one core included. A program can time what it shares with others | **Accept for programs in general.** First-party code that handles secrets must be constant-time, and that is not waived | AMD's 2026 bulletins on such channels recommend exactly this and no other change, and Intel's guidance for code that handles secrets is of the same kind (the ledger, §3.5 and §3.13) |
| AC6 | On a processor that lacks the controls ADR-0017 needs, programs are not protected from each other's speculation | **Accept, for bench machines only** | ADR-0009 already lets no claim rest on such a machine. ADR-0017's question 8 asks the same thing |
| AC7 | An administrator of an Origin Server can read whatever the system can read | **Accept, with attribution and an audit record as the defence** | The alternative is storage keys held by each user and not by the system, which is a decision for the storage and accounts designs |
| AC8 | Whoever operates the host of an Origin Server guest, or the building a server stands in, can read and change everything | **Accept** | The first server platform is a virtual machine (ADR-0014 S3). Guests the host cannot read would need an ADR and hardware Origin does not target yet |
| AC9 | A compromised update server can withhold updates until the signed metadata it serves has expired | **Accept** | It cannot be prevented from outside, only noticed |

## What must exist, and when

| Before | What |
|---|---|
| The first program that is not a boot proof runs in ring 3 (ADR-0011 N14 step 4) | ADR-0017 accepted, and its Z2, Z3, Z5, Z9 and Z10 built. The system-call model's own list: E8, O2, D11 |
| Anything but a boot proof uses the network | A decision on where the stack runs (TASKS item 23), and a model of a hostile peer |
| The compositor is designed (M8) | A threat model for §52's four rules |
| Origin can be installed (M9) | Storage encryption designed and built (EX1); AC2 decided; accounts and credentials (§57); the Secure Boot keys |
| Any claim about real hardware | The IOMMU model's list; ADR-0017's proofs on a processor in scope |
| Origin Server ships | The remote administration and remote desktop ADRs, each with a threat model; AC7 and AC8 answered; an audit record |
| The first public release | The update path (M12) with its threat model; the key ceremony (§60) |

## What this does not prove

- **Anything by itself.** It is a map of who is answerable for what. The proofs are the component models', and they are QEMU's.
- That the list of attackers is complete. It is the eight the design review named, with two trusted parties stated.
- That a component marked Designed will meet its attacker. Each needs its own model when it is built (§58).
- Anything about a processor. Those statements come from vendors' documents (the ledger), and no processor in scope has been run.

## References

- `docs/research/2026-11-10-speculation-processor-hardening.md`, for every statement about a processor, and for the two AMD bulletins named above (its §6).
- `docs/adr/ADR-0017-speculation-processor-hardening.md` (Proposed).
- The three component models, and the ADRs and sections named in each row.
