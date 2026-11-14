//! The Fixed ACPI Description Table, signature `FACP` (ACPI 6.6 §5.2.9).
//!
//! Every field is read. Which of the later fields a table has is decided by its length, not its
//! revision (§5.2.2: a table only grows): an ACPI 1.0 table of 116 bytes has none of the 64-bit
//! fields, and each later field is `None` when the table ends before it.
//!
//! Where a 32-bit field has a 64-bit `X_` form, the `X_` form is used when its address is not zero,
//! and the 32-bit field is then ignored whatever it holds (§5.2.9: the two need not agree).
//! [`Fadt::block`], [`Fadt::facs_address`] and [`Fadt::dsdt_address`] apply that rule.
//!
//! With `HW_REDUCED_ACPI` set, ACPI 6.6 tells the OS to ignore the fields at offsets 46 to 108 and
//! 148 to 232 and nine of the flags; none of them is checked then.

use crate::error::{Error, ErrorKind};
use crate::gas::{AccessSize, AddressSpace, Gas};
use crate::header::{self, Table};
use crate::read::Bytes;

/// The shortest FADT: ACPI 1.0's, through the fixed feature flags.
pub const MIN_LEN: usize = 116;

/// The OEM's intended power-management profile (§5.2.9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmProfile {
    /// Not given (0).
    Unspecified,
    /// A desktop (1).
    Desktop,
    /// A mobile, battery-capable machine (2).
    Mobile,
    /// A workstation (3).
    Workstation,
    /// An enterprise server (4).
    EnterpriseServer,
    /// A small-office or home server (5).
    SohoServer,
    /// An appliance (6).
    AppliancePc,
    /// A server that trades power for performance (7).
    PerformanceServer,
    /// A tablet (8).
    Tablet,
    /// A value ACPI 6.6 reserves (9 and above).
    Reserved(u8),
}

impl PmProfile {
    const fn from_id(id: u8) -> Self {
        match id {
            0 => Self::Unspecified,
            1 => Self::Desktop,
            2 => Self::Mobile,
            3 => Self::Workstation,
            4 => Self::EnterpriseServer,
            5 => Self::SohoServer,
            6 => Self::AppliancePc,
            7 => Self::PerformanceServer,
            8 => Self::Tablet,
            _ => Self::Reserved(id),
        }
    }
}

/// The IA-PC boot architecture flags (`IAPC_BOOT_ARCH`, §5.2.9.3). Bits 6 to 15 are reserved and
/// ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootArch(pub u16);

impl BootArch {
    /// Devices the user can see on an LPC or ISA bus (bit 0).
    #[must_use]
    pub const fn legacy_devices(self) -> bool {
        self.0 & 1 != 0
    }
    /// An 8042 keyboard controller at ports 60h and 64h (bit 1).
    #[must_use]
    pub const fn i8042(self) -> bool {
        self.0 & (1 << 1) != 0
    }
    /// The OS must not probe the VGA ranges (bit 2).
    #[must_use]
    pub const fn vga_not_present(self) -> bool {
        self.0 & (1 << 2) != 0
    }
    /// The OS must not enable MSI (bit 3).
    #[must_use]
    pub const fn msi_not_supported(self) -> bool {
        self.0 & (1 << 3) != 0
    }
    /// The OS must not take control of PCIe ASPM (bit 4).
    #[must_use]
    pub const fn pcie_aspm_controls(self) -> bool {
        self.0 & (1 << 4) != 0
    }
    /// No CMOS RTC at the legacy addresses (bit 5).
    #[must_use]
    pub const fn cmos_rtc_not_present(self) -> bool {
        self.0 & (1 << 5) != 0
    }
}

/// Whether the processor's caches survive a power loss (fixed feature flags, bits 22 and 23).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistentCaches {
    /// Not reported here; the NFIT's platform capabilities say (0).
    NotReported,
    /// Not persistent: software must flush them (1).
    NotPersistent,
    /// Persistent (2).
    Persistent,
    /// The encoding ACPI 6.6 reserves (3).
    Reserved,
}

/// The fixed feature flags (§5.2.9, Table 5.10). Bits 24 to 31 are reserved and ignored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedFeatures(pub u32);

impl FixedFeatures {
    const fn bit(self, n: u32) -> bool {
        self.0 & (1 << n) != 0
    }
    /// `WBINVD` flushes and invalidates every cache correctly (bit 0).
    #[must_use]
    pub const fn wbinvd(self) -> bool {
        self.bit(0)
    }
    /// `WBINVD` flushes every cache, without a guarantee of invalidation (bit 1).
    #[must_use]
    pub const fn wbinvd_flush(self) -> bool {
        self.bit(1)
    }
    /// C1 works on every processor (bit 2).
    #[must_use]
    pub const fn proc_c1(self) -> bool {
        self.bit(2)
    }
    /// C2 works with more than one processor (bit 3).
    #[must_use]
    pub const fn p_lvl2_up(self) -> bool {
        self.bit(3)
    }
    /// The power button is a control-method device, or there is none (bit 4).
    #[must_use]
    pub const fn pwr_button(self) -> bool {
        self.bit(4)
    }
    /// The sleep button is a control-method device, or there is none (bit 5).
    #[must_use]
    pub const fn slp_button(self) -> bool {
        self.bit(5)
    }
    /// The RTC's wake status is not in the fixed registers (bit 6).
    #[must_use]
    pub const fn fix_rtc(self) -> bool {
        self.bit(6)
    }
    /// The RTC alarm can wake the machine from S4 (bit 7).
    #[must_use]
    pub const fn rtc_s4(self) -> bool {
        self.bit(7)
    }
    /// The PM timer counts 32 bits, not 24 (bit 8).
    #[must_use]
    pub const fn tmr_val_ext(self) -> bool {
        self.bit(8)
    }
    /// The machine can dock (bit 9).
    #[must_use]
    pub const fn dck_cap(self) -> bool {
        self.bit(9)
    }
    /// The reset register can be used (bit 10).
    #[must_use]
    pub const fn reset_reg_sup(self) -> bool {
        self.bit(10)
    }
    /// The case is sealed (bit 11).
    #[must_use]
    pub const fn sealed_case(self) -> bool {
        self.bit(11)
    }
    /// The machine cannot detect a monitor or keyboard (bit 12).
    #[must_use]
    pub const fn headless(self) -> bool {
        self.bit(12)
    }
    /// A processor instruction must follow the write of `SLP_TYPx` (bit 13).
    #[must_use]
    pub const fn cpu_sw_slp(self) -> bool {
        self.bit(13)
    }
    /// The PM1 registers carry PCI Express wake status and enable bits (bit 14).
    #[must_use]
    pub const fn pci_exp_wak(self) -> bool {
        self.bit(14)
    }
    /// Monotonic counters should come from a platform timer (bit 15).
    #[must_use]
    pub const fn use_platform_clock(self) -> bool {
        self.bit(15)
    }
    /// `RTC_STS` is valid on waking from S4 (bit 16).
    #[must_use]
    pub const fn s4_rtc_sts_valid(self) -> bool {
        self.bit(16)
    }
    /// Wake events may stay armed in S5 (bit 17).
    #[must_use]
    pub const fn remote_power_on_capable(self) -> bool {
        self.bit(17)
    }
    /// Every local APIC must use the cluster destination model (bit 18).
    #[must_use]
    pub const fn force_apic_cluster_model(self) -> bool {
        self.bit(18)
    }
    /// Every local xAPIC must use physical destination mode (bit 19).
    #[must_use]
    pub const fn force_apic_physical_destination_mode(self) -> bool {
        self.bit(19)
    }
    /// The hardware-reduced ACPI profile (bit 20).
    #[must_use]
    pub const fn hw_reduced_acpi(self) -> bool {
        self.bit(20)
    }
    /// S0 idle saves as much power as S3 or more (bit 21).
    #[must_use]
    pub const fn low_power_s0_idle_capable(self) -> bool {
        self.bit(21)
    }
    /// Whether the processor's caches survive a power loss (bits 22 and 23).
    #[must_use]
    pub const fn persistent_cpu_caches(self) -> PersistentCaches {
        match (self.0 >> 22) & 0b11 {
            0 => PersistentCaches::NotReported,
            1 => PersistentCaches::NotPersistent,
            2 => PersistentCaches::Persistent,
            _ => PersistentCaches::Reserved,
        }
    }
}

/// One of the fixed hardware register blocks the FADT locates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// The PM1a event block.
    Pm1aEvent,
    /// The PM1b event block.
    Pm1bEvent,
    /// The PM1a control block.
    Pm1aControl,
    /// The PM1b control block.
    Pm1bControl,
    /// The PM2 control block.
    Pm2Control,
    /// The PM timer.
    PmTimer,
    /// General-purpose event block 0.
    Gpe0,
    /// General-purpose event block 1.
    Gpe1,
}

impl Block {
    /// Every block.
    pub const ALL: [Self; 8] = [
        Self::Pm1aEvent,
        Self::Pm1bEvent,
        Self::Pm1aControl,
        Self::Pm1bControl,
        Self::Pm2Control,
        Self::PmTimer,
        Self::Gpe0,
        Self::Gpe1,
    ];

    /// The 32-bit field's offset, the `X_` field's offset, and the name of the 32-bit field.
    const fn fields(self) -> (usize, usize, &'static str) {
        match self {
            Self::Pm1aEvent => (56, 148, "PM1a_EVT_BLK"),
            Self::Pm1bEvent => (60, 160, "PM1b_EVT_BLK"),
            Self::Pm1aControl => (64, 172, "PM1a_CNT_BLK"),
            Self::Pm1bControl => (68, 184, "PM1b_CNT_BLK"),
            Self::Pm2Control => (72, 196, "PM2_CNT_BLK"),
            Self::PmTimer => (76, 208, "PM_TMR_BLK"),
            Self::Gpe0 => (80, 220, "GPE0_BLK"),
            Self::Gpe1 => (84, 232, "GPE1_BLK"),
        }
    }

    /// The offset of the byte holding the block's length.
    const fn length_at(self) -> usize {
        match self {
            Self::Pm1aEvent | Self::Pm1bEvent => 88,
            Self::Pm1aControl | Self::Pm1bControl => 89,
            Self::Pm2Control => 90,
            Self::PmTimer => 91,
            Self::Gpe0 => 92,
            Self::Gpe1 => 93,
        }
    }
}

/// A fixed hardware register block: where it is and how many bytes it decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Register {
    /// Where: the `X_` field if its address is not zero, else the 32-bit field as a system I/O
    /// port.
    pub gas: Gas,
    /// The bytes the block decodes, from its length field.
    pub length: u8,
}

/// The checked FADT, every field decoded.
#[derive(Clone, Copy, Debug)]
pub struct Fadt<'a> {
    table: Table<'a>,
    /// The FACS's 32-bit physical address (`FIRMWARE_CTRL`).
    pub firmware_ctrl: u32,
    /// The DSDT's 32-bit physical address.
    pub dsdt: u32,
    /// The intended power-management profile.
    pub preferred_pm_profile: PmProfile,
    /// The SCI's interrupt: an 8259 IRQ, or with no 8259 its global system interrupt.
    pub sci_int: u16,
    /// The SMI command port (0: no SMM).
    pub smi_cmd: u32,
    /// The value that makes the firmware give up the ACPI registers.
    pub acpi_enable: u8,
    /// The value that gives them back.
    pub acpi_disable: u8,
    /// The value that asks the firmware for S4BIOS.
    pub s4bios_req: u8,
    /// If not zero, the value written when the OS takes over P-state control.
    pub pstate_cnt: u8,
    /// The GPE number where block 1's events begin.
    pub gpe1_base: u8,
    /// If not zero, the value written when the OS supports `_CST`.
    pub cst_cnt: u8,
    /// C2's worst-case latency in microseconds (above 100: no C2).
    pub p_lvl2_lat: u16,
    /// C3's worst-case latency in microseconds (above 1000: no C3).
    pub p_lvl3_lat: u16,
    /// Reads needed to flush the caches without `WBINVD`.
    pub flush_size: u16,
    /// The stride of those reads in bytes.
    pub flush_stride: u16,
    /// The duty-cycle field's first bit in `P_CNT`.
    pub duty_offset: u8,
    /// The duty-cycle field's width (0: no duty-cycle throttling).
    pub duty_width: u8,
    /// The RTC CMOS index of the day-of-month alarm (0: none).
    pub day_alrm: u8,
    /// The RTC CMOS index of the month alarm (0: none).
    pub mon_alrm: u8,
    /// The RTC CMOS index of the century (0: none).
    pub century: u8,
    /// The IA-PC boot architecture flags.
    pub iapc_boot_arch: BootArch,
    /// The fixed feature flags.
    pub flags: FixedFeatures,
    /// The reset register.
    pub reset_reg: Option<Gas>,
    /// The value that resets the machine when written to it.
    pub reset_value: Option<u8>,
    /// Arm's boot flags, kept raw (ADR-0019 A1: not interpreted on x86-64).
    pub arm_boot_arch: Option<u16>,
    /// The minor version (bits 0 to 3) and errata level (bits 4 to 7).
    pub minor_version: Option<u8>,
    /// The FACS's 64-bit physical address.
    pub x_firmware_ctrl: Option<u64>,
    /// The DSDT's 64-bit physical address.
    pub x_dsdt: Option<u64>,
    /// The sleep control register.
    pub sleep_control_reg: Option<Gas>,
    /// The sleep status register.
    pub sleep_status_reg: Option<Gas>,
    /// The hypervisor vendor that built the tables, if any (all zeros: none).
    pub hypervisor_vendor: Option<[u8; 8]>,
}

impl<'a> Fadt<'a> {
    /// Checks the table and every rule listed in the crate's documentation for the FADT.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let table = header::check_signature(bytes, b"FACP")?;
        let b = table.reader();
        b.require(MIN_LEN)?;
        let fadt = Self {
            table,
            firmware_ctrl: b.u32(36)?,
            dsdt: b.u32(40)?,
            preferred_pm_profile: PmProfile::from_id(b.u8(45)?),
            sci_int: b.u16(46)?,
            smi_cmd: b.u32(48)?,
            acpi_enable: b.u8(52)?,
            acpi_disable: b.u8(53)?,
            s4bios_req: b.u8(54)?,
            pstate_cnt: b.u8(55)?,
            gpe1_base: b.u8(94)?,
            cst_cnt: b.u8(95)?,
            p_lvl2_lat: b.u16(96)?,
            p_lvl3_lat: b.u16(98)?,
            flush_size: b.u16(100)?,
            flush_stride: b.u16(102)?,
            duty_offset: b.u8(104)?,
            duty_width: b.u8(105)?,
            day_alrm: b.u8(106)?,
            mon_alrm: b.u8(107)?,
            century: b.u8(108)?,
            iapc_boot_arch: BootArch(b.u16(109)?),
            flags: FixedFeatures(b.u32(112)?),
            reset_reg: Gas::read(&b, 116).ok(),
            reset_value: b.opt_u8(128),
            arm_boot_arch: b.u16(129).ok(),
            minor_version: b.opt_u8(131),
            x_firmware_ctrl: b.opt_u64(132),
            x_dsdt: b.opt_u64(140),
            sleep_control_reg: Gas::read(&b, 244).ok(),
            sleep_status_reg: Gas::read(&b, 256).ok(),
            hypervisor_vendor: b.array(268).ok(),
        };
        fadt.check(&b)?;
        Ok(fadt)
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &Table<'a> {
        &self.table
    }

    /// The FACS's address: `X_FIRMWARE_CTRL` if it is not zero, else `FIRMWARE_CTRL`, else none.
    #[must_use]
    pub fn facs_address(&self) -> Option<u64> {
        prefer(self.x_firmware_ctrl, self.firmware_ctrl)
    }

    /// The DSDT's address: `X_DSDT` if it is not zero, else `DSDT`, else none.
    #[must_use]
    pub fn dsdt_address(&self) -> Option<u64> {
        prefer(self.x_dsdt, self.dsdt)
    }

    /// The `X_` form of `block`, if the table has it.
    #[must_use]
    pub fn x_block(&self, block: Block) -> Option<Gas> {
        let (_, x, _) = block.fields();
        Gas::read(&self.table.reader(), x).ok()
    }

    /// Where `block` is and how long, or `None` if the platform has none: its `X_` form if that
    /// address is not zero, else its 32-bit field as a system I/O port if that is not zero.
    #[must_use]
    pub fn block(&self, block: Block) -> Option<Register> {
        let b = self.table.reader();
        let (legacy, _, _) = block.fields();
        let length = b.u8(block.length_at()).ok()?;
        if let Some(gas) = self.x_block(block).filter(|g| !g.is_null()) {
            return Some(Register { gas, length });
        }
        let port = b.u32(legacy).ok().filter(|&p| p != 0)?;
        Some(Register {
            gas: Gas {
                space: AddressSpace::SystemIo,
                bit_width: length.saturating_mul(8),
                bit_offset: 0,
                access: AccessSize::Undefined,
                address: u64::from(port),
            },
            length,
        })
    }

    /// The reset register and the value to write, if the flags say it can be used.
    #[must_use]
    pub fn reset(&self) -> Option<(Gas, u8)> {
        if !self.flags.reset_reg_sup() {
            return None;
        }
        Some((self.reset_reg?, self.reset_value?))
    }

    fn check(&self, b: &Bytes<'_>) -> Result<(), Error> {
        let value = |at: usize, field: &'static str| b.error(at, ErrorKind::Value { field });
        // The DSDT is always required.
        self.dsdt_address().ok_or_else(|| value(40, "DSDT"))?;
        // The FACS is optional if and only if the platform is hardware-reduced.
        let reduced = self.flags.hw_reduced_acpi();
        match self.facs_address() {
            None if !reduced => return Err(value(36, "FIRMWARE_CTRL")),
            Some(address) if address & 63 != 0 => {
                let at = if self.x_firmware_ctrl.is_some_and(|x| x != 0) {
                    132
                } else {
                    36
                };
                return Err(value(at, "FIRMWARE_CTRL: the FACS is 64-byte aligned"));
            }
            _ => {}
        }
        // A table that ends before the reset register is ACPI 1.0's, where the flag's bit was
        // reserved, and is ignored (§5.2.1.1).
        if let (true, Some(reset)) = (self.flags.reset_reg_sup(), self.reset_reg) {
            check_byte_register(b, 116, &reset, "RESET_REG")?;
        }
        for (at, reg, name) in [
            (244, self.sleep_control_reg, "SLEEP_CONTROL_REG"),
            (256, self.sleep_status_reg, "SLEEP_STATUS_REG"),
        ] {
            if let Some(reg) = reg.filter(|r| !r.is_null()) {
                check_byte_register(b, at, &reg, name)?;
            }
        }
        if reduced {
            return Ok(());
        }
        self.check_fixed_hardware(b)
    }

    /// The rules on the fixed hardware fields, which a hardware-reduced platform's OS ignores.
    fn check_fixed_hardware(&self, b: &Bytes<'_>) -> Result<(), Error> {
        let value = |at: usize, field: &'static str| b.error(at, ErrorKind::Value { field });
        for block in Block::ALL {
            let (legacy, x, name) = block.fields();
            if let Some(gas) = self.x_block(block).filter(|g| !g.is_null()) {
                gas.check_register(b, x, name)?;
                if matches!(block, Block::Gpe0 | Block::Gpe1)
                    && !matches!(
                        gas.space,
                        AddressSpace::SystemIo | AddressSpace::SystemMemory
                    )
                {
                    return Err(value(x, name));
                }
            }
            let present = self.block(block);
            let length_at = block.length_at();
            let length = b.u8(length_at)?;
            match block {
                // Required on a platform with fixed hardware, and at least 4 and 2 bytes long.
                Block::Pm1aEvent | Block::Pm1aControl if present.is_none() => {
                    return Err(value(legacy, name));
                }
                Block::Pm1aEvent if length < 4 => return Err(value(length_at, "PM1_EVT_LEN")),
                Block::Pm1aControl if length < 2 => return Err(value(length_at, "PM1_CNT_LEN")),
                Block::Pm2Control if present.is_some() && length == 0 => {
                    return Err(value(length_at, "PM2_CNT_LEN"));
                }
                // A PM timer is exactly 4 bytes.
                Block::PmTimer if present.is_some() && length != 4 => {
                    return Err(value(length_at, "PM_TMR_LEN"));
                }
                // A GPE block is a status half and an enable half.
                Block::Gpe0 | Block::Gpe1 if length & 1 != 0 => {
                    return Err(value(length_at, "GPEx_BLK_LEN"));
                }
                _ => {}
            }
        }
        // A month alarm needs a day alarm.
        if self.mon_alrm != 0 && self.day_alrm == 0 {
            return Err(b.error(106, ErrorKind::Inconsistent { field: "DAY_ALRM" }));
        }
        Ok(())
    }
}

/// The 64-bit value if present and not zero, else the 32-bit one if not zero.
fn prefer(wide: Option<u64>, narrow: u32) -> Option<u64> {
    wide.filter(|&w| w != 0)
        .or_else(|| (narrow != 0).then_some(u64::from(narrow)))
}

/// The rules for the reset and sleep registers (§5.2.9): system I/O, system memory or PCI
/// configuration space on bus 0 (with bits 48 to 63 of a PCI address clear), 8 bits wide at bit
/// offset 0.
fn check_byte_register(
    b: &Bytes<'_>,
    at: usize,
    reg: &Gas,
    field: &'static str,
) -> Result<(), Error> {
    reg.check_register(b, at, field)?;
    let space_ok = match reg.space {
        AddressSpace::SystemIo | AddressSpace::SystemMemory => true,
        AddressSpace::PciConfig => reg.address >> 48 == 0,
        _ => false,
    };
    if !space_ok || reg.bit_width != 8 || reg.bit_offset != 0 {
        return Err(b.error(at, ErrorKind::Value { field }));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{gas, put, seal, table};

    /// A full ACPI 6.6 FADT, 276 bytes, as a QEMU-like platform with fixed hardware gives it.
    pub(crate) fn fadt() -> Vec<u8> {
        let mut t = table(b"FACP", 6, &[0u8; 240]);
        put(&mut t, 36, &0x7FFE_0000u32.to_le_bytes()); // FIRMWARE_CTRL
        put(&mut t, 40, &0x7FFD_0000u32.to_le_bytes()); // DSDT
        t[45] = 1;
        put(&mut t, 46, &9u16.to_le_bytes());
        put(&mut t, 48, &0xB2u32.to_le_bytes());
        t[52] = 0xF1;
        t[53] = 0xF0;
        put(&mut t, 56, &0x600u32.to_le_bytes());
        put(&mut t, 64, &0x604u32.to_le_bytes());
        put(&mut t, 76, &0x608u32.to_le_bytes());
        put(&mut t, 80, &0x620u32.to_le_bytes());
        t[88] = 4;
        t[89] = 2;
        t[91] = 4;
        t[92] = 16;
        put(&mut t, 96, &0x0FFFu16.to_le_bytes());
        put(&mut t, 98, &0x0FFFu16.to_le_bytes());
        t[108] = 0x32;
        put(&mut t, 109, &0x0002u16.to_le_bytes());
        put(&mut t, 112, &((1u32 << 8) | (1 << 10) | 1).to_le_bytes());
        put(&mut t, 116, &gas(1, 8, 0, 0, 0xCF9));
        t[128] = 0x0F;
        t[131] = 6;
        put(&mut t, 132, &0x7FFE_0000u64.to_le_bytes());
        put(&mut t, 140, &0x7FFD_0000u64.to_le_bytes());
        put(&mut t, 148, &gas(1, 32, 0, 0, 0x600));
        put(&mut t, 172, &gas(1, 16, 0, 0, 0x604));
        put(&mut t, 208, &gas(1, 32, 0, 0, 0x608));
        put(&mut t, 220, &gas(1, 128, 0, 0, 0x620));
        seal(&mut t);
        t
    }

    pub(crate) fn edited(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut t = fadt();
        edit(&mut t);
        seal(&mut t);
        t
    }

    #[test]
    fn decodes_every_field() {
        let t = fadt();
        let f = Fadt::parse(&t).unwrap();
        assert_eq!(f.preferred_pm_profile, PmProfile::Desktop);
        assert_eq!(f.sci_int, 9);
        assert_eq!((f.acpi_enable, f.acpi_disable), (0xF1, 0xF0));
        assert_eq!(f.century, 0x32);
        assert!(f.iapc_boot_arch.i8042() && !f.iapc_boot_arch.cmos_rtc_not_present());
        assert!(f.flags.wbinvd() && f.flags.tmr_val_ext() && f.flags.reset_reg_sup());
        assert!(!f.flags.hw_reduced_acpi());
        assert_eq!(
            f.flags.persistent_cpu_caches(),
            PersistentCaches::NotReported
        );
        assert_eq!(f.facs_address(), Some(0x7FFE_0000));
        assert_eq!(f.dsdt_address(), Some(0x7FFD_0000));
        assert_eq!(f.reset().map(|(g, v)| (g.address, v)), Some((0xCF9, 0x0F)));
        let timer = f.block(Block::PmTimer).unwrap();
        assert_eq!((timer.gas.address, timer.length), (0x608, 4));
        assert_eq!(f.block(Block::Pm1bEvent), None);
        assert_eq!(f.block(Block::Gpe0).unwrap().length, 16);
        assert_eq!(f.minor_version, Some(6));
        assert_eq!(f.hypervisor_vendor, Some([0; 8]));
    }

    #[test]
    fn an_acpi_1_table_has_no_64_bit_fields() {
        let mut t = fadt();
        t.truncate(MIN_LEN);
        seal(&mut t);
        let f = Fadt::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (f.x_dsdt, f.reset_reg, f.sleep_control_reg),
            (None, None, None)
        );
        // The flags say the reset register is supported, but the table ends before it.
        assert_eq!(f.reset(), None);
        assert_eq!(
            f.block(Block::PmTimer).unwrap().gas.space,
            AddressSpace::SystemIo
        );
        let mut short = fadt();
        short.truncate(MIN_LEN - 1);
        seal(&mut short);
        assert!(matches!(
            Fadt::parse(&short).unwrap_err().kind,
            ErrorKind::TooShort { .. }
        ));
    }

    #[test]
    fn the_64_bit_form_wins_when_not_zero() {
        let t = edited(|t| {
            put(t, 40, &0x1234u32.to_le_bytes());
            put(t, 220, &gas(0, 128, 0, 0, 0xFED0_0000));
        });
        let f = Fadt::parse(&t).unwrap();
        assert_eq!(f.dsdt_address(), Some(0x7FFD_0000), "X_DSDT wins");
        assert_eq!(
            f.block(Block::Gpe0).unwrap().gas.space,
            AddressSpace::SystemMemory
        );
        let t = edited(|t| put(t, 140, &0u64.to_le_bytes()));
        assert_eq!(Fadt::parse(&t).unwrap().dsdt_address(), Some(0x7FFD_0000));
    }

    fn refused(edit: impl FnOnce(&mut Vec<u8>)) -> (usize, ErrorKind) {
        let e = Fadt::parse(&edited(edit)).unwrap_err();
        (e.at, e.kind)
    }

    #[test]
    fn each_rule_is_refused_by_name() {
        let v = |field| ErrorKind::Value { field };
        assert_eq!(
            refused(|t| {
                put(t, 40, &[0; 4]);
                put(t, 140, &[0; 8]);
            }),
            (40, v("DSDT"))
        );
        assert_eq!(
            refused(|t| {
                put(t, 36, &[0; 4]);
                put(t, 132, &[0; 8]);
            }),
            (36, v("FIRMWARE_CTRL"))
        );
        assert_eq!(
            refused(|t| put(t, 132, &0x7FFE_0010u64.to_le_bytes())),
            (132, v("FIRMWARE_CTRL: the FACS is 64-byte aligned"))
        );
        assert_eq!(refused(|t| t[116 + 1] = 16), (116, v("RESET_REG")));
        assert_eq!(refused(|t| t[116] = 3), (116, v("RESET_REG")));
        assert_eq!(
            refused(|t| put(t, 116, &gas(2, 8, 0, 0, 1 << 48))),
            (116, v("RESET_REG"))
        );
        assert_eq!(refused(|t| t[116 + 3] = 9), (116, v("RESET_REG")));
        assert_eq!(
            refused(|t| put(t, 244, &gas(3, 8, 0, 0, 0x62))),
            (244, v("SLEEP_CONTROL_REG"))
        );
        assert_eq!(
            refused(|t| {
                put(t, 56, &[0; 4]);
                put(t, 148, &[0; 12]);
            }),
            (56, v("PM1a_EVT_BLK"))
        );
        assert_eq!(refused(|t| t[88] = 3), (88, v("PM1_EVT_LEN")));
        assert_eq!(refused(|t| t[89] = 1), (89, v("PM1_CNT_LEN")));
        assert_eq!(refused(|t| t[91] = 3), (91, v("PM_TMR_LEN")));
        assert_eq!(refused(|t| t[92] = 15), (92, v("GPEx_BLK_LEN")));
        assert_eq!(
            refused(|t| put(t, 220, &gas(3, 8, 0, 0, 0x62))),
            (220, v("GPE0_BLK"))
        );
        assert_eq!(
            refused(|t| put(t, 72, &0x650u32.to_le_bytes())),
            (90, v("PM2_CNT_LEN"))
        );
        assert_eq!(
            refused(|t| t[107] = 0x0A),
            (106, ErrorKind::Inconsistent { field: "DAY_ALRM" })
        );
    }

    #[test]
    fn a_hardware_reduced_platform_skips_the_fixed_hardware() {
        let t = edited(|t| {
            put(t, 112, &(1u32 << 20).to_le_bytes());
            put(t, 36, &[0; 4]);
            put(t, 132, &[0; 8]);
            t[46..=108].fill(0xFF);
            put(t, 148, &[0xFF; 96]);
            put(t, 244, &gas(0, 8, 0, 1, 0xFED8_0000));
            put(t, 256, &gas(0, 8, 0, 1, 0xFED8_0004));
        });
        let f = Fadt::parse(&t).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(f.facs_address(), None);
        assert_eq!(f.sleep_status_reg.unwrap().address, 0xFED8_0004);
    }

    #[test]
    fn reserved_values_are_typed_not_refused() {
        let t = edited(|t| {
            t[45] = 200;
            put(t, 112, &((3u32 << 22) | 1 | (1 << 8)).to_le_bytes());
            t[111] = 0xFF;
            put(t, 109, &0xFFC2u16.to_le_bytes());
        });
        let f = Fadt::parse(&t).unwrap();
        assert_eq!(f.preferred_pm_profile, PmProfile::Reserved(200));
        assert_eq!(f.flags.persistent_cpu_caches(), PersistentCaches::Reserved);
        assert!(f.iapc_boot_arch.i8042());
    }
}
