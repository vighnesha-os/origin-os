//! x86-64 descriptor encodings for the GDT, the TSS and the IDT (Intel SDM Vol. 3A §3.4.5
//! segment descriptors, §7.2.3 the 64-bit TSS descriptor, §6.14.1 64-bit IDT gates).
//!
//! Pure arithmetic, unit-tested on the host. The binary loads the tables (`lgdt`, `ltr`,
//! `lidt`).
#![forbid(unsafe_code)]

/// GDT selector of the kernel code segment.
pub const KERNEL_CODE: u16 = 0x08;
/// GDT selector of the kernel data segment.
pub const KERNEL_DATA: u16 = 0x10;
/// GDT selector of the TSS (a 16-byte descriptor at entries 3 and 4).
pub const TSS: u16 = 0x18;
/// GDT selector of the user data segment (index 5), with RPL 3.
pub const USER_DATA: u16 = 0x28 | 3;
/// GDT selector of the user 64-bit code segment (index 6), with RPL 3.
pub const USER_CODE: u16 = 0x30 | 3;
/// Number of 8-byte GDT entries: null, kernel code, kernel data, the two halves of the TSS,
/// user data and user code.
pub const GDT_ENTRIES: usize = 7;
/// Size of the 64-bit TSS in bytes (SDM Vol. 3A Figure 8-11).
pub const TSS_SIZE: u32 = 104;
/// IST slot used for #DF, so a double fault always runs on a known-good stack.
pub const DOUBLE_FAULT_IST: u8 = 1;
/// IST slot used for NMI: an NMI can arrive at any instruction, including the one after
/// `syscall`, before the kernel has left the user stack.
pub const NMI_IST: u8 = 2;
/// IST slot used for #MC, for the same reason.
pub const MACHINE_CHECK_IST: u8 = 3;

/// 64-bit kernel code segment: present, DPL 0, code execute/read, L = 1, G = 1.
pub const fn kernel_code_segment() -> u64 {
    descriptor(0x9A, 0xA)
}

/// Kernel data segment: present, DPL 0, data read/write, D/B = 1, G = 1. In 64-bit mode
/// base and limit are ignored; the descriptor must still be valid for SS and DS.
pub const fn kernel_data_segment() -> u64 {
    descriptor(0x92, 0xC)
}

/// 64-bit user code segment: present, DPL 3, code execute/read, L = 1, G = 1.
pub const fn user_code_segment() -> u64 {
    descriptor(0xFA, 0xA)
}

/// User data segment: present, DPL 3, data read/write, D/B = 1, G = 1.
pub const fn user_data_segment() -> u64 {
    descriptor(0xF2, 0xC)
}

/// A flat code or data descriptor (base 0, limit 0xFFFFF) with the given access byte and
/// flags nibble.
const fn descriptor(access: u8, flags: u8) -> u64 {
    let limit_low = 0xFFFF_u64;
    let limit_high = 0xF_u64;
    limit_low | ((access as u64) << 40) | (limit_high << 48) | (((flags & 0xF) as u64) << 52)
}

/// The 16-byte 64-bit TSS descriptor for a TSS at `base`: present, DPL 0, type 0x9
/// ("64-bit TSS, available"), limit `TSS_SIZE - 1`, byte granularity.
pub const fn tss_descriptor(base: u64) -> [u64; 2] {
    let limit = (TSS_SIZE - 1) as u64;
    let low = (limit & 0xFFFF)
        | ((base & 0xFF_FFFF) << 16)
        | (0x89_u64 << 40)
        | (((limit >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    [low, base >> 32]
}

/// A 64-bit interrupt gate (type 0xE: interrupts stay disabled in the handler), present,
/// DPL 0, to `offset` in segment `selector`, using IST slot `ist` (0 = none).
pub const fn interrupt_gate(offset: u64, selector: u16, ist: u8) -> [u64; 2] {
    gate(offset, selector, ist, 0)
}

/// As [`interrupt_gate`], but DPL 3: user mode may raise it with `int` (a system call).
/// Every other gate is DPL 0, so `int n` from user mode for any other vector is a #GP.
pub const fn user_interrupt_gate(offset: u64, selector: u16) -> [u64; 2] {
    gate(offset, selector, 0, 3)
}

const fn gate(offset: u64, selector: u16, ist: u8, dpl: u8) -> [u64; 2] {
    let access = 0x8E_u64 | (((dpl & 3) as u64) << 5);
    let low = (offset & 0xFFFF)
        | ((selector as u64) << 16)
        | (((ist & 0x7) as u64) << 32)
        | (access << 40)
        | (((offset >> 16) & 0xFFFF) << 48);
    [low, offset >> 32]
}

/// The 104-byte 64-bit TSS as 26 little-endian `u32` words (SDM Vol. 3A Figure 8-11).
///
/// Every field is zero except IST1–IST3 (the #DF, NMI and #MC stack tops, `ists`) from byte
/// 36, and the I/O
/// map base at byte 102. The map base is set to `TSS_SIZE`, which means "no I/O permission
/// bitmap" (user mode can reach no port). RSP0, the stack an interrupt from user mode
/// switches to, is set per thread later ([`TSS_RSP0_OFFSET`]).
pub const fn tss_image(ists: [u64; 3]) -> [u32; 26] {
    let mut t = [0u32; 26];
    let mut i = 0;
    while i < 3 {
        // IST(i+1) is at byte 36 + 8 i: words 9 + 2 i (low) and 10 + 2 i (high).
        t[9 + 2 * i] = ists[i] as u32;
        t[10 + 2 * i] = (ists[i] >> 32) as u32;
        i += 1;
    }
    t[25] = TSS_SIZE << 16; // bytes 100..104: reserved u16, then the I/O map base
    t
}

/// Byte offset of RSP0 in the 64-bit TSS.
pub const TSS_RSP0_OFFSET: usize = 4;

/// True for the exception vectors where the processor pushes an error code (SDM Vol. 3A
/// Table 6-1): #DF, #TS, #NP, #SS, #GP, #PF, #AC, #CP, #VC and #SX.
pub const fn has_error_code(vector: u8) -> bool {
    matches!(vector, 8 | 10..=14 | 17 | 21 | 29 | 30)
}

/// Mnemonic and name of exception `vector` (SDM Vol. 3A Table 6-1).
pub const fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "#DE divide error",
        1 => "#DB debug",
        2 => "NMI",
        3 => "#BP breakpoint",
        4 => "#OF overflow",
        5 => "#BR bound range exceeded",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        8 => "#DF double fault",
        10 => "#TS invalid TSS",
        11 => "#NP segment not present",
        12 => "#SS stack-segment fault",
        13 => "#GP general protection",
        14 => "#PF page fault",
        16 => "#MF x87 floating-point error",
        17 => "#AC alignment check",
        18 => "#MC machine check",
        19 => "#XM SIMD floating-point exception",
        20 => "#VE virtualization exception",
        21 => "#CP control protection",
        28 => "#HV hypervisor injection",
        29 => "#VC VMM communication",
        30 => "#SX security exception",
        _ => "reserved",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_segments_have_the_well_known_encodings() {
        assert_eq!(kernel_code_segment(), 0x00AF_9A00_0000_FFFF);
        assert_eq!(kernel_data_segment(), 0x00CF_9200_0000_FFFF);
        assert_eq!(user_code_segment(), 0x00AF_FA00_0000_FFFF);
        assert_eq!(user_data_segment(), 0x00CF_F200_0000_FFFF);
        assert_eq!((USER_DATA >> 3, USER_DATA & 3), (5, 3));
        assert_eq!((USER_CODE >> 3, USER_CODE & 3), (6, 3));
    }

    #[test]
    fn only_the_user_gate_has_dpl_3() {
        let [kernel, _] = interrupt_gate(0x1234, KERNEL_CODE, 0);
        let [user, _] = user_interrupt_gate(0x1234, KERNEL_CODE);
        assert_eq!((kernel >> 40) & 0xFF, 0x8E);
        assert_eq!((user >> 40) & 0xFF, 0xEE, "present, DPL 3, interrupt gate");
        assert_eq!(kernel & !(0xFF << 40), user & !(0xFF << 40));
    }

    #[test]
    fn tss_descriptor_splits_the_base_across_both_halves() {
        let base = 0xFFFF_8000_1234_5678;
        let [low, high] = tss_descriptor(base);
        assert_eq!(low & 0xFFFF, 103, "limit");
        assert_eq!((low >> 16) & 0xFF_FFFF, 0x34_5678, "base 23:0");
        assert_eq!((low >> 40) & 0xFF, 0x89, "present, 64-bit TSS available");
        assert_eq!((low >> 56) & 0xFF, 0x12, "base 31:24");
        assert_eq!(high, 0xFFFF_8000, "base 63:32, upper bits reserved zero");
    }

    #[test]
    fn interrupt_gate_fields() {
        let [low, high] = interrupt_gate(0x0123_4567_89AB_CDEF, KERNEL_CODE, DOUBLE_FAULT_IST);
        assert_eq!(low & 0xFFFF, 0xCDEF, "offset 15:0");
        assert_eq!((low >> 16) & 0xFFFF, 0x08, "selector");
        assert_eq!((low >> 32) & 0x7, 1, "IST");
        assert_eq!((low >> 40) & 0xFF, 0x8E, "present, DPL 0, interrupt gate");
        assert_eq!((low >> 48) & 0xFFFF, 0x89AB, "offset 31:16");
        assert_eq!(high, 0x0123_4567, "offset 63:32");
    }

    #[test]
    fn error_code_vectors_match_the_sdm_table() {
        let with: Vec<u8> = (0..32).filter(|&v| has_error_code(v)).collect();
        assert_eq!(with, vec![8, 10, 11, 12, 13, 14, 17, 21, 29, 30]);
        assert_eq!(exception_name(3), "#BP breakpoint");
        assert_eq!(exception_name(15), "reserved");
    }
    #[test]
    fn tss_image_places_ist1_and_the_io_map_base() {
        let ists = [
            0x1122_3344_5566_7788,
            0x99AA_BBCC_DDEE_FF00,
            0x0102_0304_0506_0708,
        ];
        let t = tss_image(ists);
        let bytes: Vec<u8> = t.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(bytes.len(), 104);
        assert_eq!(&bytes[36..44], &ists[0].to_le_bytes());
        assert_eq!(&bytes[44..52], &ists[1].to_le_bytes());
        assert_eq!(&bytes[52..60], &ists[2].to_le_bytes());
        assert_eq!(u16::from_le_bytes([bytes[102], bytes[103]]), 104);
        let others = bytes
            .iter()
            .enumerate()
            .filter(|(i, _)| !(36..60).contains(i) && !(102..104).contains(i));
        assert!(others.into_iter().all(|(_, &b)| b == 0));
    }
}
