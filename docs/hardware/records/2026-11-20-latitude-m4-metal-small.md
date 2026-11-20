# Bench boot: Latitude.sh m4-metal-small, 2026-11-20 (rental B)

| | |
|---|---|
| Machine | Latitude.sh `m4-metal-small`, Amsterdam: AMD EPYC 4244P, CPUID family 19h (25), model 61h (97), stepping 2; board Supermicro H13SRE-F (system AS-3015MR-H10TNR); 12 logical processors; 64 GiB of RAM, most of it above 4 GiB |
| Firmware | American Megatrends 2.4 (2025-04-29), UEFI 2.9 (firmware revision 0x50023 as the loader read it), UEFI mode, Secure Boot off; console redirection on COM2 (I/O port 0x2F8) at 115 200 baud, the UART the SPCR names |
| Source revision | `69ce9ecd9327d1967a1351a71e4e489957459d11` (`main` `ea2293ca` and the rental tool, which changes no image input); `cargo xtask build`, developer-mode loader; `BOOTX64.EFI` SHA-256 `d6fb7c0a669f5a9c89c8e11c227e662cf1b29555616f98a3898f2cd1806d6391`; `KERNEL.ONBF` SHA-256 `de7ab079fb1f4c4fbf8a45f1ba6c5d00e5e56eed4b2bc582f7ebd693b21ed615` |
| Date | 2026-11-20 |
| Result | Boots 1, 2 and 3 alike: the loader ran to `origin-boot: exiting boot services; entering the Framekernel`. No kernel line reached the serial console in the 8 minutes each boot was given, and the machine did not restart by itself. No verdict |
| Scope | By hand, since the kernel printed nothing on the console: AuthenticAMD family 19h, older than Zen 5; outside the declared scope, a bench machine (ADR-0009 A2). Bench equipment: it qualifies nothing and supports no hardware claim (CLAUDE.md §74, §76, rules 30 and 40; item 233) |
| Environment | A physical machine with no hypervisor; the provider's firmware and BMC |
| Boot entry | Origin's loader replaced `\EFI\ubuntu\shimx64.efi`, which `Boot0002`, first in `BootOrder`, starts (item 236). The provider's system had been started by `Boot0004`, a network-boot entry; every reset of this rental started `Boot0002` |
| Speculation controls | Not printed |
| IOMMU | IVRS present: one IOMMU, no IVMD block (the same table as rental A's). Bring-up not printed |
| Log | `2026-11-20-latitude-m4-metal-small.log`, SHA-256 `41783e73ac991aadc8a97f5ce3ccc40798066ef7fb4c26669ac6fa98ab0fb700`: the serial console from each boot's firmware start to the next reset, the provider's own system left out, with IP and MAC addresses, GUIDs and the server's ID removed. Runs of 20 or more spaces, which the firmware uses to place its progress text, are written as line breaks |
| Not kept here | ACPI tables, `lspci -vvv`, `efibootmgr -v`, `dmesg`, CPUID: held privately by the owner (item 232). SHA-256 of the tables Origin reads first: SPCR `2f93c54c877ec205131b5e18fb4601c5806e98a05c1b4983ee0a5f0e2fc71df1`, IVRS `7f8517f6c55cceb3f9b8335bfd831d7f20049d51cf089ccc57e6f8098fdfdbc3`, APIC `9b9e93881d3ee48a0a51f7e6b5db0c809b2ccba34dc715be9e595b808d2664ea`, FACP `2918822e4067ee055656d769964e396783827098adfa48dde5263452d6b5ae8c`, FPDT `ee3c7c272face85db12433c01112474b1d21c4bd71143ebbfd504c32de09bfd9`, DSDT `b4a0ef3b36d44ac0505a9bc15718b8fc67ae17c466014ad36508b1072da611b4` |

What happened, and why the kernel's lines are not here: `docs/research/2026-11-20-rental-b-findings.md`.
