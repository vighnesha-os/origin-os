//! x86-64 descriptor encodings for the GDT, the TSS and the IDT (Intel SDM Vol. 3A §3.4.5
//! segment descriptors, §7.2.3 the 64-bit TSS descriptor, §6.14.1 64-bit IDT gates).
//!
//! Pure arithmetic, unit-tested on the host. The binary loads the tables (`lgdt`, `ltr`,
//! `lidt`).

/// GDT selector of the kernel code segment.
pub const KERNEL_CODE: u16 = 0x08;
/// GDT selector of the kernel data segment.
pub const KERNEL_DATA: u16 = 0x10;
/// GDT selector of the TSS (a 16-byte descriptor at entries 3 and 4).
pub const TSS: u16 = 0x18;
/// Number of 8-byte GDT entries: null, code, data, and the two halves of the TSS.
pub const GDT_ENTRIES: usize = 5;
/// Size of the 64-bit TSS in bytes (SDM Vol. 3A Figure 8-11).
pub const TSS_SIZE: u32 = 104;
/// IST slot used for #DF, so a double fault always runs on a known-good stack.
pub const DOUBLE_FAULT_IST: u8 = 1;

/// 64-bit kernel code segment: present, DPL 0, code execute/read, L = 1, G = 1.
pub const fn kernel_code_segment() -> u64 {
    descriptor(0x9A, 0xA)
}

/// Kernel data segment: present, DPL 0, data read/write, D/B = 1, G = 1. In 64-bit mode
/// base and limit are ignored; the descriptor must still be valid for SS and DS.
pub const fn kernel_data_segment() -> u64 {
    descriptor(0x92, 0xC)
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
    let low = (offset & 0xFFFF)
        | ((selector as u64) << 16)
        | (((ist & 0x7) as u64) << 32)
        | (0x8E_u64 << 40)
        | (((offset >> 16) & 0xFFFF) << 48);
    [low, offset >> 32]
}

/// The 104-byte 64-bit TSS as 26 little-endian `u32` words (SDM Vol. 3A Figure 8-11).
///
/// Every field is zero except IST1, the double-fault stack top, at byte 36, and the I/O
/// map base at byte 102. The map base is set to `TSS_SIZE`, which means "no I/O permission
/// bitmap". The RSP0–2 fields stay zero until user mode exists (M4).
pub const fn tss_image(ist1: u64) -> [u32; 26] {
    let mut t = [0u32; 26];
    t[9] = ist1 as u32; // bytes 36..40, IST1 low
    t[10] = (ist1 >> 32) as u32; // bytes 40..44, IST1 high
    t[25] = TSS_SIZE << 16; // bytes 100..104: reserved u16, then the I/O map base
    t
}

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
        let t = tss_image(0x1122_3344_5566_7788);
        let bytes: Vec<u8> = t.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(bytes.len(), 104);
        assert_eq!(&bytes[36..44], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(u16::from_le_bytes([bytes[102], bytes[103]]), 104);
        let others = bytes
            .iter()
            .enumerate()
            .filter(|(i, _)| !(36..44).contains(i) && !(102..104).contains(i));
        assert!(others.into_iter().all(|(_, &b)| b == 0));
    }
}
