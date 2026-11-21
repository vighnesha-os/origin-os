# Bench boot: Latitude.sh m4-metal-small, 2026-11-21 (rental C)

| | |
|---|---|
| Machine | Latitude.sh `m4-metal-small`, Amsterdam: AMD EPYC 4244P, CPUID family 19h (25), model 61h (97), stepping 2; board Supermicro H13SRE-F (system AS-3015MR-H10TNR); 12 logical processors; 64 GiB of RAM, most of it above 4 GiB. Very likely rental A's machine: the provider gave this rental the server ID it gave rental A, and the firmware and every ACPI table hashed below match rental A's |
| Firmware | American Megatrends 2.5a (2025-12-08), UEFI 2.9 (firmware revision 0x50023 as the loader read it), UEFI mode, Secure Boot off; console redirection on COM2 (I/O port 0x2F8) at 115 200 baud, the UART the SPCR names |
| Source revision | `1c9c7a960f1ddb34380dccd88242b00539880cc0` (`main` after PR 47: TASKS item 84, boot info 0.8); `cargo xtask build`, developer-mode loader; `BOOTX64.EFI` SHA-256 `30c4e77b812486d75969732a1c6f8d07e01172af09f4ca57aa80a6e4195a7777`; `KERNEL.ONBF` SHA-256 `6b1c67b78c233581a02e761203e19fb2a6e1f7ec96625f67538b26b2dd4ac3a0` |
| Date | 2026-11-21 |
| Result | Boots 1, 2 and 3 alike: the loader named the SPCR's UART at I/O port `0x2f8` for the kernel and ran to `origin-boot: exiting boot services; entering the Framekernel`. No kernel line reached the serial console in the 8 minutes each boot was given, and the machine did not restart by itself. No verdict |
| Scope | By hand, since the kernel printed nothing on the console: AuthenticAMD family 19h, older than Zen 5; outside the declared scope, a bench machine (ADR-0009 A2). Bench equipment: it qualifies nothing and supports no hardware claim (CLAUDE.md §74, §76, rules 30 and 40; item 233) |
| Environment | A physical machine with no hypervisor; the provider's firmware and BMC |
| Boot entry | Origin's loader replaced `\EFI\ubuntu\shimx64.efi`, which `Boot0002`, first in `BootOrder`, starts (item 236). The provider's system had been started by `Boot0004`; every reset of this rental started `Boot0002` |
| Speculation controls | Not printed |
| IOMMU | IVRS present: one IOMMU, no IVMD block (the same table as rentals A and B). Bring-up not printed |
| Log | `2026-11-21-latitude-m4-metal-small.log`, SHA-256 `015fe6e97d03114020356c079fc4040bb5b5e79c1168cd78496c43a549637234`: the serial console from each boot's firmware start to the next reset, the provider's own system left out, with IP and MAC addresses, GUIDs and the server's ID removed. Runs of 20 or more spaces, which the firmware uses to place its progress text, are written as line breaks |
| Not kept here | ACPI tables, `lspci -vvv`, `efibootmgr -v`, `dmesg`, CPUID: held privately by the owner (item 232). SHA-256 of the tables Origin reads first: SPCR `2f93c54c877ec205131b5e18fb4601c5806e98a05c1b4983ee0a5f0e2fc71df1`, IVRS `7f8517f6c55cceb3f9b8335bfd831d7f20049d51cf089ccc57e6f8098fdfdbc3`, APIC `9b9e93881d3ee48a0a51f7e6b5db0c809b2ccba34dc715be9e595b808d2664ea`, FACP `2918822e4067ee055656d769964e396783827098adfa48dde5263452d6b5ae8c`, FPDT `ee3c7c272face85db12433c01112474b1d21c4bd71143ebbfd504c32de09bfd9`, DSDT `22ef9ebbf9c351a1d18abb58233b6130c8b542aa22e2409b2a9232f5ec3f2277` |

What happened, and where the boot stops: `docs/research/2026-11-21-rental-c-findings.md`.
