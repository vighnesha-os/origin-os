# Research ledger: what current Linux and Ubuntu support, for ADR-0015's devices

| | |
|---|---|
| Date | 2026-11-10 |
| For | `docs/adr/ADR-0015-device-standards-sdk.md` (D6, D9); TASKS items 13 and 16; decision items 91–96 |
| Requires | CLAUDE.md §3 (references for gaps only), §16 (current revisions), §17, §32–§35, §54, §74, rule 30 |
| Method | First pass: published sources (kernel.org and Ubuntu's release lists, news reports, mailing-list archives, vendor support notes). Second pass, the same day, on the owner's instruction: "read Linux files under open source, but don't copy, just refer to understand". That pass settled section 6's open points from the files themselves: <br>• Linux **v7.4-rc2**, commit `7475ea8b58616b33c04afb976cf0fbaad9dd63fb` (2026-11-09), through GitHub's API; <br>• BlueZ's **5.88** tree (tagged 2026-11-05); <br>• Launchpad's records of Ubuntu's published kernels. <br>What was learned is written here in Origin's words; no code is copied into this ledger. No Origin code was written in this session after the reading (item 92). Versions are today's (item 96). Section 7 records the licence lines of Linux files read earlier the same day. |

## 1. The question

The owner asked whether the latest Linux kernel and Ubuntu already support the devices ADR-0015 covers, because if so they help Origin.

Linux helps in two ways:

- **As a reference for gaps** (items 91–96).
- **As a known-good system** on the same machine when testing.

It never helps as a source of code.

## 2. Versions on 2026-11-10

| Project | Version | Source |
|---|---|---|
| Linux mainline | 7.4-rc2 (2026-11-09). 7.4-rc1 was 2026-11-01, after the largest merge window in the kernel's history (19,671 commits). Final expected around 2026-12-20 | [kernel.org](https://www.kernel.org/releases.json), [Phoronix](https://www.phoronix.com/news/Linux-7.4-Massive-Window), [Phoronix rc1](https://phoronix.com/news/Linux-7.4-rc1) |
| Linux stable | 7.3.3 (2026-11-06). 7.2 is end of life | kernel.org |
| Linux long-term | 6.18.62, 6.12.116, 6.6.162, 6.1.192, 5.15.225, 5.10.274 | kernel.org |
| Ubuntu | 26.10 (2026-10-15, Linux 7.3: its launch images carried a pre-release 7.3 kernel, 7.3.0-9; 7.3.0-10 is in its updates), with an x86-64-v3 image. 26.04.1 LTS runs **Linux 7.0** (7.0.0-43 in its updates); no newer hardware-enablement kernel is published for it yet | Launchpad's published sources for `linux` in `resolute` and `stonking`, read 2026-11-10; [Ubuntu releases](https://changelogs.ubuntu.com/meta-release), [Phoronix](https://phoronix.com/news/Ubuntu-26.10-Released), [UbuntuHandbook](https://ubuntuhandbook.org/index.php/2026/10/ubuntu-26-10-released-with-kernel-7-3-gnome-51-amd64v3-image/), [PBXScience](https://pbxscience.com/?p=23001) |

## 3. Coverage

| Area | In Linux now | Missing or partial | Sources |
|---|---|---|---|
| NVMe | Mature driver. 7.3 removes a small-I/O bottleneck on PCIe Gen 5 drives; 7.4 adds DMA-BUF buffers for raw NVMe I/O through io_uring; 7.2 added per-controller timeouts | **v7.4-rc2's NVMe header** defines Flexible Data Placement, zoned namespaces, sanitize, telemetry, power states and NVMe 2.0's in-band authentication. It defines nothing for 2.4's exported-subsystem migration or post-quantum security, and no Copy command. So NVMe 2.4's new features (published 2026-08-04) are absent. Admin commands (firmware, sanitize, format, telemetry) mostly go through passthrough from user tools | [7.4 block](https://phoronix.com/news/Linux-7.4-Block), [7.3](https://phoronix.com/news/Linux-7.3-Faster-Small-Direct), [7.4 DMA-BUF](https://phoronix.com/news/DMA-BUF-IO-uring-Linux-7.4), [7.2](https://phoronix.com/news/linux-7.2-storage) |
| PCIe 5/6/7 | Gen 5 fully. Link encryption and device authentication framework (IDE, TDISP) since 6.19, needing a platform security module | **At v7.4-rc2:** <br>• the PCI code has link encryption (IDE), the security-manager (TSM) layer, DOE mailboxes, bandwidth control, AER and DPC; <br>• its register header knows Flit Mode and the 64 GT/s speed, but not 128 GT/s; <br>• the July 2026 Flit Logging series is **not merged** (no such code in the tree). <br>Rust SPDM was an RFC in March 2026. PCIe 7 (128 GT/s): nothing, and no hardware | [Flit Logging](https://patchew.org/linux/20260714183950.1620016-1-yazen.ghannam@amd.com/), [Flit status](https://patchew.org/linux/20250207161836.2755-1-ilpo.jarvinen@linux.intel.com), [6.19 link encryption](https://scour.ing/p/https://www.phoronix.com/news/Linux-6.19-PCIe-Link-Encrypt) |
| USB4, Thunderbolt 4/5 | Intel's connection manager: tunnels, device authorization, retimer updates, power states. USB4 v2 and Thunderbolt 5 since about 6.5–6.10. 7.2 adds USB4STREAM (direct transfer between two hosts). 7.3 checks USB-C cables for alt-mode support | **At v7.4-rc2** the Thunderbolt driver recognizes the USB4 host controllers of Panther Lake (its M and P variants, two each) and Wildcat Lake, and implements asymmetric links. **Nova Lake's controller is not in it yet.** Apple Silicon USB4 is still patches (August 2026) | [7.3 USB](https://phoronix.com/news/Linux-7.3-USB), [USB4STREAM](https://penguinpulse.pages.dev/news/2026-06-23-linux-72-kernel-merges-usb4stream-for-high-speed-system-to-s.html), [6.5 USB4 v2](https://www.phoronix.com/forums/forum/hardware/general-hardware/1395945-linux-6-5-adding-initial-support-for-usb4-v2-intel-barlow-ridge), [Apple USB4](https://phoronix.com/news/Linux-USB4-Apple-Silicon) |
| Wi-Fi 7 | Wi-Fi 7 itself works. v7.4-rc2 carries Intel's newer driver core (`mld`) beside its two older ones, and the Wi-Fi core implements multi-link operation. Dell says systems it ships from 2026 have native Wi-Fi 7 on Linux | Multi-link operation needs kernel 6.16+, current firmware, a supplicant with multi-link support and a router that supports it. User reports on Intel BE200/BE201 cards are mixed | [Dell](https://www.dell.com/support/kbdoc/en-us/000309405/wi-fi-7-support-on-dell-computers-with-client-linux-operating-systems), [Excentis](https://support.excentis.com/knowledge/article/1005), [Intel community](https://community.intel.com/t5/Wireless/how-to-enable-MLO-on-be200/m-p/1664682) |
| Bluetooth 6.x | LE Audio and Auracast work. Upstream documentation for qualifying BlueZ products (May 2026) | **Channel Sounding** (6.0's main feature) **is now present.** v7.4-rc2's Bluetooth header defines its commands and events. BlueZ **5.88** (tagged 2026-11-05) adds a Channel Sounding test client and documents ranging interfaces. Whether it works on a given controller is unproven here. No support was mapped to 6.1 or 6.2 | [BlueZ weekly, April 2026](https://bluez.github.io/news/2026/weekly-report-w17/), [kernel patch](https://lkml.iu.edu/hypermail/linux/kernel/2512.2/02271.html), [Collabora LE Audio](https://www.collabora.com/news-and-blog/blog/2025/11/24/implementing-bluetooth-le-audio-and-auracast-on-linux-systems/), [Collabora qualification](https://collabora.com/news-and-blog/blog/2026/05/26/simplifying-bluetooth-qualification-for-linux-bluez-new-upstream-documentation) |
| Intel 2026 CPUs | **Panther Lake:** power management and thermal hints (7.0), the rugged Panther Lake R (7.2–7.4), NPU. **Wildcat Lake:** NPU driver 1.32. **Nova Lake:** family 18 (as ADR-0009 Amendment 1 uses), audio (6.19, 7.0), graphics stable in 7.3, camera (IPU8) coming | Nova Lake NPU: nothing found | [7.0 PM](https://phoronix.com/news/linux-7.0-power-management), [7.4 PTL-R](https://phoronix.com/news/Linux-7.4-Rugged-Panther-Lake), [NPU 1.32](https://www.phoronix.com/news/Intel-NPU-Linux-Driver-1.32), [NVL family 18](https://www.tomshardware.com/pc-components/cpus/intel-prepares-for-nova-lake-cpus-with-new-linux-support-retiring-20-year-old-family-6-designation-in-favor-of-family-18), [NVL-S graphics](https://phoronix.com/news/Linux-7.3-Intel-NVL-S-Stable) |
| AMD 2026 CPUs | **Gorgon Point** is Zen 5, already supported. **Zen 6:** early support in 7.3 (HSMP, PMF), more in 7.4 including amd-pstate tuning. A low-power Zen 6 core type is defined | EPYC Venice: v7.4-rc2's memory-error (EDAC) driver handles family 1Ah models 50h–57h. That is the range AMD's counter document 69163 covers, which the press ties to Venice. Nothing more Venice-specific was looked for | [7.3 Zen 6](https://www.digitalcitizen.life/linux-7-3-adds-early-amd-zen-6-support-ryzen-ai-halo-rgb-controls-and-major-laptop-improvements/), [7.4 amd-pstate](https://www.phoronix.com/news/Linux-7.4-AMD-P-State-Zen-6), [Zen 6 HSMP](https://phoronix.com/news/AMD-Zen-6-Client-HSMP-Linux) |
| Displays | Linux 7.4 enables HDMI 2.1 by default in AMD's driver | That arrangement covers AMD's code, not Origin's (ADR-0015 D7) | [Phoronix rc1](https://phoronix.com/news/Linux-7.4-rc1) |

Rows marked "at v7.4-rc2" were read from the tree itself. The rest rest on news reports and mailing-list archives, not release notes. Where a source says "patches" or "under review", this ledger does not count the feature as present.

## 4. What this means for Origin

- **Gaps vendors leave.** Wi-Fi chip interfaces, Thunderbolt details, PCIe behaviour without the paid specification, and new platforms all have their best public description in Linux. Intel and AMD write that support before launch from their internal documents. Under items 91–96 such a gap may be read there, written into Origin's own specification, and implemented in a separate session.
- **A known-good system for testing.** Booting Ubuntu on the same machine shows how the hardware behaves. That evidence comes from the hardware, not from Linux code. It does not replace Origin's own proofs (rules 14 and 30).
- **Ubuntu 26.10's x86-64-v3 image** matches the CPU baseline Origin chose (§14). It is a sign that the baseline suits current machines, not evidence about Origin.

## 5. Where Linux does not help

- NVMe 2.4's new features, PCIe 7, PCIe 6 Flit logging and Nova Lake's USB4 controller, none of which v7.4-rc2 has. Origin works from the specification alone there.
- Firmware-dependent features. Linux's support often rests on `linux-firmware`, wpa_supplicant and BlueZ. Origin needs that firmware under its own licences (§38).
- Testing. Linux supporting a device says nothing about whether Origin's driver works.

## 6. Re-check when

Re-check when a driver's work starts (item 96), and at Linux 7.4 final (expected around 2026-12-20).

Settled by the second pass, at v7.4-rc2:

- Flit Logging is not merged.
- Channel Sounding is in the kernel's definitions and in BlueZ 5.88.
- NVMe 2.4's features are absent.
- Venice has memory-error support.
- Ubuntu 26.04 runs Linux 7.0.

Still open:

- Flit Logging in 7.5;
- Nova Lake's USB4 controller in the Thunderbolt driver;
- NVMe 2.4 features;
- Ubuntu 26.04's next hardware-enablement kernel, usually published with its .2 point release.

## 7. Licences of the likely references (read 2026-11-10)

Earlier the same day, before item 91 was decided, Claude answered the owner's question about using Linux for drivers. To do so it read the licence line at the top of these Linux files through GitHub's API, on `torvalds/linux` master after v7.4-rc2. The exact commit was not recorded. Only licence lines were read, not code.

| File | Licence |
|---|---|
| `drivers/nvme/host/core.c` | GPL-2.0 |
| `drivers/usb/host/xhci.c` | GPL-2.0 |
| `drivers/thunderbolt/tb.c`, `drivers/thunderbolt/usb4.c` | GPL-2.0 |
| `drivers/iommu/intel/iommu.c` | GPL-2.0-only |
| `drivers/net/ethernet/intel/igc/igc_main.c` | GPL-2.0 |
| `drivers/net/wireless/intel/iwlwifi/iwl-drv.c` | GPL-2.0 OR BSD-3-Clause |
| `drivers/net/wireless/realtek/rtw89/core.c` | GPL-2.0 OR BSD-3-Clause |
| `drivers/net/wireless/mediatek/mt76/mac80211.c` | BSD-3-Clause-Clear |
| `drivers/net/wireless/ath/ath12k/core.c` | BSD-3-Clause-Clear |
| `net/mac80211/main.c` | GPL-2.0-only |
| `net/bluetooth/hci_core.c` | GPL-2.0 |
| `drivers/bluetooth/btintel.c` | GPL-2.0-or-later |
| `drivers/bluetooth/btusb.c` | Not read: the request returned nothing |
| `drivers/gpu/drm/xe/xe_device.c` | MIT |
| `drivers/gpu/drm/amd/amdgpu/amdgpu_device.c`, `drivers/gpu/drm/i915/i915_driver.c` | MIT-style permission notice (no SPDX line) |

wpa_supplicant and hostapd, which Ubuntu packages, are BSD-3-Clause for their main source, according to [Debian's copyright file](https://metadata.ftp-master.debian.org/changelogs/main/w/wpa/unstable_copyright); Ubuntu's package comes from Debian's.

Under items 93 and 96, a driver that consults one of these files records it again: its licence, the tag or commit read, the date, and the gap it filled.
