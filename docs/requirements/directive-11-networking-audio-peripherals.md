# Requirements matrix: PART XI, Networking, audio and peripherals

One row for each statement of CLAUDE.md in §54 to §56. The columns and their rules are in [README.md](README.md), and `cargo xtask requirements-gate` holds this file to them and to the directive.

| ID | Requirement | Kind | Scope | Owner | State | Implementation | Acceptance | Evidence | Env | Review | Limitations | Decided by |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| S54-01 | Implement: Ethernet. | build | both/hw | 12 | partial | framekernel/src/net.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:network packets | virt | agent | Proven in memory and through the virtio-net device. Not a stack a network may face. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-02 | Implement: IPv4. | build | both/hw | 12 | partial | framekernel/src/net.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:network packets | virt | agent | Proven in memory and through the virtio-net device. Not a stack a network may face. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-03 | Implement: IPv6. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-04 | Implement: ARP/NDP. | build | both/hw | 12 | partial | framekernel/src/net.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:network packets | virt | agent | ARP only, proven in memory. No NDP. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-05 | Implement: TCP. | build | both/hw | 12 | partial | framekernel/src/tcp.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:TCP | virt | agent | A connection core proven by loopback in memory. It is not connected to the NIC and has no acceptance checks, so it must not face a network. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-06 | Implement: UDP. | build | both/hw | 12 | partial | framekernel/src/net.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:network packets | virt | agent | Proven in memory and through the virtio-net device. Not a stack a network may face. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-07 | Implement: ICMP. | build | both/hw | 12 | partial | framekernel/src/net.rs, framekernel/src/virtionet.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:network packets | virt | agent | Proven in memory and through the virtio-net device. Not a stack a network may face. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S54-08 | Implement: DHCP. | build | both/hw | 12 | partial | framekernel/src/dhcp.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | file:framekernel/src/dhcp.rs | host | agent | A client's messages and lease state machine, tested on the host. Never run against a server. | - |
| S54-09 | Implement: DNS. | build | both/hw | 12 | partial | framekernel/src/dns.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | file:framekernel/src/dns.rs | host | agent | A message parser with bounded name decompression, tested on the host. No resolver. | - |
| S54-10 | Implement: Routing. | build | both/hw | 12 | partial | framekernel/src/route.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | file:framekernel/src/route.rs | host | agent | An IPv4 longest-prefix table, tested on the host. Nothing forwards by it. | - |
| S54-11 | Implement: Firewall. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-12 | Implement: Socket APIs. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-13 | Implement: TLS integration. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-14 | Implement: Wi-Fi. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-15 | Implement: Bluetooth. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-16 | Implement: Network diagnostics. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-17 | Implement: VPN extension interfaces. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-18 | Implement standards-based interoperability testing. | build | both/hw | 12 | not-started | - | - | - | - | none | - | - |
| S54-19 | A Wi-Fi or Bluetooth driver needs a legitimate source for its chip's programming interface (ADR-0015 D6): vendor documentation that lets Origin publish source written from it; or | rule | both/hw | 12 | not-started | - | - | - | - | none | - | 85 |
| S54-20 | A Wi-Fi or Bluetooth driver needs a legitimate source for its chip's programming interface (ADR-0015 D6): §3's process for gaps: Linux read only as a reference, what it explains written into Origin's own specification first, and the code written from that in a separate session (decision items 91–93). | rule | both/hw | 12 | not-started | - | - | - | - | none | - | 85, 91–93 |
| S54-21 | Never translate another operating system's driver. Host stacks may be built from the public specifications first. Before either radio ships, check Bluetooth SIG qualification and licensing and the radio regulations. | rule | both/hw | 12 | not-started | - | - | - | - | none | - | 85 |
| S55-01 | Build: Audio discovery. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-02 | Build: Output and input. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-03 | Build: Mixer. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-04 | Build: Stream routing. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-05 | Build: Volume management. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-06 | Build: Low-latency playback. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-07 | Build: USB Audio. | build | both/hw | 60 | not-started | - | - | - | - | none | - | - |
| S55-08 | Build: HD Audio. | build | both/hw | 60 | not-started | - | - | - | - | none | - | - |
| S55-09 | Build: Microphone support. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-10 | Build: Audio device hot-plug. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-11 | Build: Privacy controls. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S55-12 | Build: Error recovery. | build | both/hw | 14 | not-started | - | - | - | - | none | - | - |
| S56-01 | Support appropriate standard device interfaces for: Keyboards. | build | both/hw | 61 | partial | framekernel/src/i8042.rs, framekernel/src/ps2.rs | Its boot proofs pass on each declared machine (§74), and each of its negative controls fails the boot. | proof:i8042 | virt | none | The i8042 controller is brought up and PS/2 scancodes are decoded. No interrupt handler delivers a key, and there is no USB keyboard. Run only under QEMU's processor and, in CI, as KVM guests on real processors, with QEMU's devices and firmware: no physical machine (§74). | - |
| S56-02 | Support appropriate standard device interfaces for: Mice. | build | both/hw | 61 | not-started | - | - | - | - | none | - | - |
| S56-03 | Support appropriate standard device interfaces for: Touchpads. | build | both/hw | 61 | not-started | - | - | - | - | none | - | - |
| S56-04 | Support appropriate standard device interfaces for: Touchscreens. | build | both/hw | 61 | not-started | - | - | - | - | none | - | - |
| S56-05 | Support appropriate standard device interfaces for: Pen. | build | both/hw | 61 | not-started | - | - | - | - | none | - | - |
| S56-06 | Support appropriate standard device interfaces for: Game controllers. | build | both/hw | 61 | not-started | - | - | - | - | none | - | - |
| S56-07 | Support appropriate standard device interfaces for: Cameras. | build | both/hw | 62 | not-started | - | - | - | - | none | - | - |
| S56-08 | Support appropriate standard device interfaces for: Webcams. | build | both/hw | 62 | not-started | - | - | - | - | none | - | - |
| S56-09 | Support appropriate standard device interfaces for: Printers. | build | both/hw | 63 | not-started | - | - | - | - | none | - | - |
| S56-10 | Support appropriate standard device interfaces for: Scanners. | build | both/hw | 63 | not-started | - | - | - | - | none | - | - |
| S56-11 | Support appropriate standard device interfaces for: Displays. | build | both/hw | 13 | not-started | - | - | - | - | none | - | - |
| S56-12 | Support appropriate standard device interfaces for: USB docks. | build | both/hw | 40 | not-started | - | - | - | - | none | - | - |
| S56-13 | Proprietary devices require device-specific implementation evidence. | rule | both/hw | 14 | not-started | - | - | - | - | none | - | - |
