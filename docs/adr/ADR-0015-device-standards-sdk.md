# ADR-0015: Every device standard, completely, through the SDK

| | |
|---|---|
| Status | **Accepted** by the owner on 2026-11-10, as recommended ("Accept all", decision sheet items 80 and 82–87; CLAUDE.md §18). Nothing here is built. It sets how device interfaces are shaped, not their binary layout: each interface's OIDL definition is its own decision when it is written (§84). |
| Requires | CLAUDE.md P0, P3, P5, P9; §3, §5, §6, §16–§18, §30–§36, §38, §49, §54, §56, §58, §59, §72, §74, §82; final rules 2, 3, 10, 14, 26, 30, 35 |
| Evidence | The checks in Evidence, 2026-11-10 |
| Builds on | ADR-0001 D2 (third-party drivers in user mode), ADR-0007 (the IOMMU), ADR-0011 (N3, N4, N7, N9) |
| Would change | CLAUDE.md §17, §31, §35, §36, §54 (revision 7.3) |

## Context

On 2026-11-10 the owner wrote: "I want kernel as native SDK, similarly, NVME driver, ...etc, all same like USB 4.x and Thunderbolt 4,5, WIFI 6,7 ..etc, Bluetooth 6.0 ...etc with all features as Native SDK".

ADR-0011 already decides the kernel part. Programs reach the kernel only through `origin.core` (N5). Drivers are written against a stable Driver SDK, and the kernel's own crates use the Framekernel SDK (N4). What is new is that each device standard is reachable through a typed interface covering everything its published specification offers.

Three things limit how fast that can happen. Some specifications and programming interfaces are not public. Some features need licences or certification. And QEMU emulates only part of the hardware.

## Decisions

### D1. Three typed layers per device technology (item 82)

Each layer is defined once in OIDL (ADR-0011 N7):

| Layer | What it holds | Promise |
|---|---|---|
| **Class** | What every device of the kind does | Stable per capability (ADR-0011 N3) |
| **Standard** | Everything the published standard defines | Versioned with the standard's revision |
| **Vendor** | Extras a vendor documents in a form Origin may use | Only where such documents exist |

Drivers are written against the Driver SDK and expose devices through these layers only.

| Technology | Class | Standard layer, for example |
|---|---|---|
| NVMe | Storage | Namespaces, health log, firmware download and activation, format, sanitize, power states, zoned namespaces |
| USB | USB devices | Enumeration, configurations, transfers, hubs |
| USB4, Thunderbolt 4/5 | Device fabric | Routers, ports, tunnels (PCIe, DisplayPort, USB 3), bandwidth allocation, device approval (D5) |
| Ethernet | Network link | Link state, offloads, statistics |
| Wi-Fi 6/7 | Network link | Scanning, WPA3, 6 GHz, Wi-Fi 7 multi-link operation, roaming, regulatory domain, power save |
| Bluetooth | Bluetooth | Discovery, pairing, GATT, LE Audio, Channel Sounding |
| Displays | Display | Modes, DisplayPort features (D7) |

### D2. Every feature reported truthfully (item 82)

The standard layer defines all of a standard's features. Each device reports which of them it supports, because devices differ: not every drive can sanitize, and not every Wi-Fi card does multi-link operation (P9). An operation is offered only once Origin implements it and has proven it. An unsupported one returns a typed "not supported" status, never a pretended success (§1, rule 14). A hardware feature proven only under QEMU is reported as exactly that (rule 30).

### D3. Dangerous operations need their own rights (item 83)

These operations need rights of their own, checked by the service that performs them (ADR-0011 N9):

- firmware update;
- format, sanitize and namespace management;
- approving a USB4 or Thunderbolt device, which gives it DMA (ADR-0007);
- radio transmit settings, including the regulatory domain.

Raw access (NVMe command passthrough, raw Bluetooth HCI, raw 802.11 frames and monitor mode) is a diagnostics right for administrators. It is never given to applications by default (P0, §30, §58).

### D4. Unprivileged stacks and fuzzed parsers (item 83)

The Wi-Fi and Bluetooth host stacks and the USB4 connection manager run as unprivileged user-mode services, confined by the IOMMU where they drive a DMA-capable device (§31, ADR-0007). Every parser of radio frames, HCI events, USB and USB4 descriptors and configuration space gets the treatment the existing parsers had (TASKS item 3) and fuzzing (§72) before it reads real input. Radio frames come from anyone in range, and parsers of them have a history of remotely exploitable bugs.

### D5. Thunderbolt 4/5 is part of the USB4 interface (item 84)

Thunderbolt 4 and 5 are built on USB4, so the USB4 interface covers them and there is no separate Thunderbolt SDK. The work follows the public USB4 v2.0 specification (§17). Intel's Thunderbolt licence is read **before** its specification, to confirm it lets Origin publish source written from it. The Thunderbolt name appears in the product only after Intel certification. Until then the product says USB4, and Thunderbolt 3, 4 and 5 interoperability are features reported per D2 (§17, §35).

### D6. Wi-Fi and Bluetooth drivers wait for a legitimate source (item 85)

The major Wi-Fi chip makers (Intel, MediaTek, Qualcomm, Realtek) publish no programming interface for their chips. As far as was found, the only working drivers are in Linux and BSD source. Translating those to Rust is what §3 forbids. A driver may start only with one of two sources:

1. vendor documentation whose terms let Origin publish source written from it; or
2. a clean-room process approved in an original-source-exception ADR (§18). One person writes a behavioural specification and another implements from that alone, and both are recorded in `SOURCE_PROVENANCE.md` (§6).

Firmware the chips need is shipped under its own licence, as §38 requires.

*Added 2026-11-10, decision item 91 (the owner's rule):* Linux source may be read only as a reference, to understand what vendor documents and the official specifications and standards leave out. It is never copied. Items 92 and 93 (accepted the same day) set how such reading reaches Origin's code:

- what was learned goes first into Origin's own specification, in Origin's words, citing file, commit and licence;
- the code is then written from that in a separate session, without the Linux source;
- each driver's provenance lists the Linux files consulted and the gap each one filled.

This is the clean-room process route 2 above asks for. So a Wi-Fi or Bluetooth driver may start once the gaps in the vendor documents are filled this way, without a further ADR.

Bluetooth's host-to-controller interface (HCI) is standard, but each vendor's firmware-loading step is not public, so Bluetooth controllers have the same question on a smaller scale.

Meanwhile, these can be built and tested on the host from public specifications:

- 802.11 frame handling and the connection state machines;
- WPA3's SAE handshake;
- the Bluetooth host: HCI, L2CAP, ATT and GATT, the security manager, LE Audio.

All of them need key agreement and encryption that Origin does not have yet: P-256, AES-CCM and AES-GCM, AES-CMAC, HMAC-based key derivation. Each is implemented per §59.

Before either radio ships, two things need research:

- the Bluetooth SIG's membership, qualification and patent licence terms. The patent licence is reciprocal among members and tied to qualification, and nothing found says how it treats an open-source stack;
- radio regulatory duties: the country setting, radar avoidance in 5 GHz and the 6 GHz rules.

### D7. Displays and restricted documents (item 80)

- **DisplayPort comes first.** HDMI 2.1's high-rate (FRL) modes wait until their licensing allows an open implementation. The HDMI Forum refused AMD's open-source implementation in 2024, and AMD posted partial patches only in May 2026 (§36).
- **Read the Thunderbolt licence before its specification** (D5).
- **Keep documents whose terms forbid redistribution out of Git.** AMD's IOMMU specification 48882 is the precedent: it is excluded locally and never committed (§17).

### D8. The standards baseline (item 86)

§17 gains:

- IEEE 802.11-2024 with the 802.11be amendment (Wi-Fi 7);
- Bluetooth Core 6.2 or later.

Both are re-checked when the work starts (§16).

### D9. Order (item 87)

1. NVMe and the USB host controller (xHCI) come first, because QEMU emulates both and Origin already drives NVMe there.
2. USB4 and Thunderbolt, Wi-Fi and Bluetooth come at M10, on physical hardware. QEMU emulates none of them.
3. Their protocol logic is tested on the host with the specifications' test vectors.
4. Nothing is claimed from QEMU alone (§82, rule 30).

## Alternatives

| Alternative | For | Against |
|---|---|---|
| A stable SDK per driver (an "NVMe driver SDK") | Exactly what each driver offers | The owner already chose promises per capability, not per module (item 34): a stable interface per driver freezes what must change (P8) |
| Class interfaces only | Small and stable | Loses most of each standard's features, which the owner wants |
| Raw passthrough for applications | Every feature at once, with no interface work | Any application could wipe a drive, flash firmware or transmit anywhere (P0) |
| A compatibility layer running Linux or BSD drivers | Wi-Fi and Bluetooth hardware support quickly | Third-party kernel code in the runtime (§3, §5); its security is not Origin's |
| Wait until every feature works before exposing any | Interfaces never change | Nothing ships for years; D2 lets features land one at a time |

## Evidence

Checked 2026-11-10:

- Bluetooth Core 6.2 was published on 2025-11-03 ([Bluetooth SIG](https://www.bluetooth.com/bluetooth-core-6-2-feature-overview); [Nordic](https://devzone.nordicsemi.com/nordic/nordic-blog/b/blog/posts/whats-new-in-bluetooth-core-6-2-an-overview)). Whether 6.3 has appeared since is unconfirmed.
- IEEE 802.11be was approved on 2024-09-26 and published on 2025-07-22. It is free through the IEEE GET program ([IEEE](https://standards.ieee.org/ieee/802.11/10548/)).
- Bluetooth qualification and the patent and copyright licence ([qualification](https://www.bluetooth.com/develop-with-bluetooth/qualify/), [do I need to qualify?](https://support.bluetooth.com/hc/en-us/articles/360049018272), [PCLA](https://www.bluetooth.com/wp-content/uploads/2019/03/PCLA-ESign-Version-Version-11.pdf)).
- HDMI 2.1 and open source ([PCGH, 2024](https://www.pcgameshardware.de/Monitor-Display-Hardware-154105/News/HDMI-Open-Source-Implementierung-1441978/); [Phoronix, May 2026](https://phoronix.com/news/amdgpu-hdmi-2.1-frl-patches)).
- Thunderbolt 4/5 specifications are not public and need an Intel licence (CLAUDE.md §17).
- That no major Wi-Fi vendor publishes its chips' programming interface comes from Claude's knowledge, not from a fresh search. It is to be re-checked vendor by vendor before D6's first driver.

## Trade-offs

- A full standard layer per technology is a large interface to design and keep versioned. In return, programs get every feature through one checked path, and nothing needs raw access.
- Truthful per-device features make programs check before they use a feature. That is more work for application authors, and it removes guessing.
- Waiting for a legitimate source may leave Wi-Fi unsupported for a long time. Breaking §3 would put the original-source objective, and the project's legal standing as public source, at risk.
- Unprivileged radio stacks cost some IPC on every packet. The P0 gain is worth it for code that reads frames from strangers.

## Migration consequences

- CLAUDE.md revision 7.3: the §17 standards, the layers in §31, Thunderbolt in §35, DisplayPort first in §36 and the driver sources in §54.
- No interface exists yet, so nothing built changes. Each technology's OIDL interface is decided when it is written, and the Driver SDK still needs its own ADR, as ADR-0001 said.
- New tasks: research the Thunderbolt licence, Bluetooth licensing and qualification, vendor Wi-Fi documentation and radio regulation before M10. Implement the radios' crypto (§59).

## Threat model (§58)

| Interface | Attacker | Defence |
|---|---|---|
| Wi-Fi and Bluetooth frames | Anyone in radio range | Unprivileged stacks (D4); fuzzed parsers; no raw access for applications (D3) |
| A USB4 or Thunderbolt device | A malicious device or dock seeking DMA | Approval right (D3); IOMMU confinement before any tunnel opens (ADR-0007) |
| Firmware update, format, sanitize | A program destroying data or implanting firmware | Separate rights (D3); firmware signature checks where the device supports them |
| Raw passthrough | A program bypassing the interfaces | Administrator diagnostics right only (D3) |
| Regulatory settings | A program making the radio transmit illegally | A right of its own; the country setting is changed only by the system (D3, D6) |

## What this does not decide

- The OIDL interfaces themselves, technology by technology (§84).
- The Driver SDK's binary interface (its own ADR).
- Whether any clean-room process is approved (D6's ADR, if one is proposed).
- Which Wi-Fi or Bluetooth chips are supported first.
- Wi-Fi 8 (802.11bn), still a draft.

## Questions for the owner (answered 2026-11-10: "Accept all")

1. DisplayPort first; read the Thunderbolt licence before the specification; keep restricted documents out of Git (D7)? **Yes** (item 80).
2. Three typed layers per device technology, with each device reporting its features truthfully (D1, D2)? **Yes** (item 82).
3. Separate rights for dangerous operations; raw access for administrator diagnostics only; unprivileged stacks; fuzzed parsers (D3, D4)? **Yes** (item 83).
4. Thunderbolt 4/5 inside the USB4 interface, with the name only after certification (D5)? **Yes** (item 84).
5. Wi-Fi and Bluetooth drivers wait for a legitimate source; host stacks first; licensing and regulation checked before shipping (D6)? **Yes** (item 85).
6. Wi-Fi and Bluetooth standards added to §17 (D8)? **Yes** (item 86).
7. The order in D9? **Yes** (item 87).
