//! CPU feature discovery (CLAUDE.md §24: CPU feature discovery, FPU/SIMD state; §14: the
//! `x86-64-v3` baseline), decoded from raw CPUID and XCR0 values so it is tested on the
//! host. The kernel (`src/cpu.rs`) runs CPUID and reports; this module decides what the
//! bits mean.
//!
//! References: Intel SDM Vol. 2A, CPUID (leaf 1, leaf 7 sub-leaf 0, leaf 0Dh sub-leaves 0
//! and 1); AMD64 APM Vol. 3; the x86-64 microarchitecture levels (the psABI supplement).

/// The CPUID leaves this module decodes, as the kernel read them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cpuid {
    /// Leaf 1: `[eax, ebx, ecx, edx]`.
    pub leaf1: [u32; 4],
    /// Leaf 7, sub-leaf 0.
    pub leaf7: [u32; 4],
    /// Leaf 0Dh, sub-leaf 0 (XSAVE sizes for XCR0-enabled and for all features).
    pub leaf_d0: [u32; 4],
    /// Leaf 8000_0001h (LAHF, LZCNT/ABM, and more).
    pub ext1: [u32; 4],
    /// The largest basic and extended CPUID leaves (`leaf 0 eax`, `leaf 8000_0000h eax`).
    pub max_basic: u32,
    pub max_ext: u32,
}

/// One decoded feature set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    // Leaf 1 ECX.
    pub sse3: bool,
    pub pclmulqdq: bool,
    pub ssse3: bool,
    pub fma: bool,
    pub cmpxchg16b: bool,
    pub pcid: bool,
    pub sse41: bool,
    pub sse42: bool,
    pub x2apic: bool,
    pub movbe: bool,
    pub popcnt: bool,
    pub aesni: bool,
    pub xsave: bool,
    pub osxsave: bool,
    pub avx: bool,
    pub f16c: bool,
    pub rdrand: bool,
    // Leaf 1 EDX.
    pub fpu: bool,
    pub tsc: bool,
    pub msr: bool,
    pub pae: bool,
    pub cmov: bool,
    pub pge: bool,
    pub mmx: bool,
    pub fxsr: bool,
    pub sse: bool,
    pub sse2: bool,
    // Leaf 7 EBX / ECX.
    pub fsgsbase: bool,
    pub bmi1: bool,
    pub avx2: bool,
    pub smep: bool,
    pub bmi2: bool,
    pub invpcid: bool,
    pub rdseed: bool,
    pub smap: bool,
    pub sha: bool,
    pub umip: bool,
    // Leaf 8000_0001h EDX / ECX.
    pub nx: bool,
    pub gbpages: bool,
    pub rdtscp: bool,
    pub long_mode: bool,
    pub lahf: bool,
    pub lzcnt: bool,
}

fn bit(v: u32, n: u32) -> bool {
    v & (1 << n) != 0
}

impl Features {
    /// Decodes the CPUID values the kernel gathered.
    pub fn decode(id: &Cpuid) -> Self {
        let [_, _, c1, d1] = id.leaf1;
        // Leaf 7 is read only when it exists.
        let [_, b7, c7, _] = if id.max_basic >= 7 { id.leaf7 } else { [0; 4] };
        let [_, _, ce, de] = if id.max_ext >= 0x8000_0001 {
            id.ext1
        } else {
            [0; 4]
        };
        Self {
            sse3: bit(c1, 0),
            pclmulqdq: bit(c1, 1),
            ssse3: bit(c1, 9),
            fma: bit(c1, 12),
            cmpxchg16b: bit(c1, 13),
            pcid: bit(c1, 17),
            sse41: bit(c1, 19),
            sse42: bit(c1, 20),
            x2apic: bit(c1, 21),
            movbe: bit(c1, 22),
            popcnt: bit(c1, 23),
            aesni: bit(c1, 25),
            xsave: bit(c1, 26),
            osxsave: bit(c1, 27),
            avx: bit(c1, 28),
            f16c: bit(c1, 29),
            rdrand: bit(c1, 30),
            fpu: bit(d1, 0),
            tsc: bit(d1, 4),
            msr: bit(d1, 5),
            pae: bit(d1, 6),
            cmov: bit(d1, 15),
            pge: bit(d1, 13),
            mmx: bit(d1, 23),
            fxsr: bit(d1, 24),
            sse: bit(d1, 25),
            sse2: bit(d1, 26),
            fsgsbase: bit(b7, 0),
            bmi1: bit(b7, 3),
            avx2: bit(b7, 5),
            smep: bit(b7, 7),
            bmi2: bit(b7, 8),
            invpcid: bit(b7, 10),
            rdseed: bit(b7, 18),
            smap: bit(b7, 20),
            sha: bit(b7, 29),
            umip: bit(c7, 2),
            nx: bit(de, 20),
            gbpages: bit(de, 26),
            rdtscp: bit(de, 27),
            long_mode: bit(de, 29),
            lahf: bit(ce, 0),
            lzcnt: bit(ce, 5),
        }
    }

    /// The x86-64 microarchitecture level this CPU meets: 1, 2, 3 or 4 (CLAUDE.md §14: the
    /// baseline is 3). `None` below level 1 (not a usable x86-64 CPU here).
    ///
    /// The levels (psABI supplement): v1 = SSE2 and the long-mode baseline; v2 adds SSE3,
    /// SSSE3, SSE4.1, SSE4.2, POPCNT, CMPXCHG16B, LAHF/SAHF; v3 adds AVX, AVX2, FMA, F16C,
    /// BMI1, BMI2, LZCNT, MOVBE and OSXSAVE; v4 adds the AVX-512 foundation (not decoded
    /// here, so v4 is never reported).
    pub fn arch_level(&self) -> Option<u8> {
        let v1 = self.long_mode && self.sse && self.sse2 && self.fpu && self.cmov;
        if !v1 {
            return None;
        }
        let v2 = self.sse3
            && self.ssse3
            && self.sse41
            && self.sse42
            && self.popcnt
            && self.cmpxchg16b
            && self.lahf;
        let v3 = v2
            && self.avx
            && self.avx2
            && self.fma
            && self.f16c
            && self.bmi1
            && self.bmi2
            && self.lzcnt
            && self.movbe
            && self.osxsave;
        Some(if v3 {
            3
        } else if v2 {
            2
        } else {
            1
        })
    }

    /// Whether the CPU meets the `x86-64-v3` baseline.
    pub fn meets_baseline(&self) -> bool {
        self.arch_level().is_some_and(|l| l >= 3)
    }
}

/// XCR0 feature bits (Intel SDM Vol. 1 §13.3). Bit 0 (x87) is always set.
pub mod xcr0 {
    pub const X87: u64 = 1 << 0;
    pub const SSE: u64 = 1 << 1;
    pub const AVX: u64 = 1 << 2;
    /// AVX-512 opmask, ZMM_hi256 and Hi16_ZMM; set together.
    pub const AVX512: u64 = (1 << 5) | (1 << 6) | (1 << 7);
}

/// The XCR0 value the kernel should set: x87 and SSE always, AVX when the CPU has it and
/// enabling it is safe (OSXSAVE will be set first). AVX-512 is left off for now.
pub fn wanted_xcr0(features: &Features) -> u64 {
    let mut x = xcr0::X87 | xcr0::SSE;
    if features.avx && features.xsave {
        x |= xcr0::AVX;
    }
    x
}

/// The XSAVE area size (bytes) the kernel must allocate per thread for the state `xcr0`
/// enables: leaf 0Dh sub-leaf 0 EBX is the size for the features currently in XCR0, and
/// ECX the size for every feature the CPU supports. The kernel sets XCR0 first, then reads
/// EBX; this takes ECX (the maximum) as the safe allocation, so the area always fits.
pub fn xsave_area_size(id: &Cpuid) -> Option<usize> {
    if id.max_basic < 0x0D {
        return None;
    }
    let max = id.leaf_d0[2];
    // The legacy FXSAVE area is 512 bytes plus a 64-byte XSAVE header; a sane CPU reports
    // at least that.
    (max >= 576).then_some(max as usize)
}

/// The size of the legacy FXSAVE area, used when XSAVE is absent (always 512 bytes,
/// 16-byte aligned).
pub const FXSAVE_SIZE: usize = 512;

#[cfg(test)]
mod tests {
    use super::*;

    /// A modern Intel CPU (roughly an Ice Lake / Tiger Lake), without AVX-512 enabled in
    /// this synthetic sample.
    fn modern() -> Cpuid {
        // Leaf 1 ECX: SSE3, PCLMULQDQ, SSSE3, FMA, CMPXCHG16B, PCID, SSE4.1, SSE4.2,
        // x2APIC, MOVBE, POPCNT, AESNI, XSAVE, OSXSAVE, AVX, F16C, RDRAND.
        let c1 = (1 << 0)
            | (1 << 1)
            | (1 << 9)
            | (1 << 12)
            | (1 << 13)
            | (1 << 17)
            | (1 << 19)
            | (1 << 20)
            | (1 << 21)
            | (1 << 22)
            | (1 << 23)
            | (1 << 25)
            | (1 << 26)
            | (1 << 27)
            | (1 << 28)
            | (1 << 29)
            | (1 << 30);
        // Leaf 1 EDX: FPU, TSC, MSR, PAE, PGE, CMOV, MMX, FXSR, SSE, SSE2.
        let d1 = (1 << 0)
            | (1 << 4)
            | (1 << 5)
            | (1 << 6)
            | (1 << 13)
            | (1 << 15)
            | (1 << 23)
            | (1 << 24)
            | (1 << 25)
            | (1 << 26);
        // Leaf 7 EBX: FSGSBASE, BMI1, AVX2, SMEP, BMI2, INVPCID, RDSEED, SMAP, SHA.
        let b7 = (1 << 0)
            | (1 << 3)
            | (1 << 5)
            | (1 << 7)
            | (1 << 8)
            | (1 << 10)
            | (1 << 18)
            | (1 << 20)
            | (1 << 29);
        let c7 = 1 << 2; // UMIP
        // Ext leaf 1 EDX: NX, 1 GiB pages, RDTSCP, long mode. ECX: LAHF, LZCNT.
        let de = (1 << 20) | (1 << 26) | (1 << 27) | (1 << 29);
        let ce = (1 << 0) | (1 << 5);
        Cpuid {
            leaf1: [0, 0, c1, d1],
            leaf7: [0, b7, c7, 0],
            leaf_d0: [0x07, 0x340, 0x980, 0], // EBX 832 (x87+SSE+AVX), ECX 2432
            ext1: [0, 0, ce, de],
            max_basic: 0x1F,
            max_ext: 0x8000_0008,
        }
    }

    #[test]
    fn a_modern_cpu_decodes_and_meets_v3() {
        let f = Features::decode(&modern());
        assert!(f.sse && f.sse2 && f.avx && f.avx2 && f.fma && f.bmi1 && f.bmi2);
        assert!(f.smep && f.smap && f.rdseed && f.rdrand && f.x2apic && f.nx && f.long_mode);
        assert!(f.xsave && f.osxsave && f.fsgsbase && f.invpcid && f.pcid);
        assert_eq!(f.arch_level(), Some(3));
        assert!(f.meets_baseline());
        assert_eq!(wanted_xcr0(&f), xcr0::X87 | xcr0::SSE | xcr0::AVX);
        assert_eq!(xsave_area_size(&modern()), Some(2432));
    }

    #[test]
    fn levels_are_graded() {
        // A v1 CPU: 64-bit with SSE2 but nothing above.
        let mut id = modern();
        id.leaf1[2] = 0; // no SSE3..AVX, no OSXSAVE
        id.leaf7 = [0; 4];
        id.ext1[2] = 0; // no LAHF/LZCNT
        let f = Features::decode(&id);
        assert_eq!(f.arch_level(), Some(1));
        assert!(!f.meets_baseline());
        assert_eq!(
            wanted_xcr0(&f),
            xcr0::X87 | xcr0::SSE,
            "no AVX without the bits"
        );

        // A v2 CPU: SSE4.2, POPCNT, CMPXCHG16B, LAHF, but no AVX/BMI.
        let mut id = modern();
        id.leaf1[2] &= !((1 << 28) | (1 << 12) | (1 << 29) | (1 << 27) | (1 << 26)); // no AVX, FMA, F16C, OSXSAVE, XSAVE
        id.leaf7[1] = 0; // no BMI/AVX2
        let f = Features::decode(&id);
        assert_eq!(f.arch_level(), Some(2));
    }

    #[test]
    fn missing_leaves_read_as_absent() {
        let id = Cpuid {
            leaf1: [0, 0, 1 << 28, 1 << 26], // claims AVX and SSE2 in leaf 1...
            leaf7: [0, 0xFFFF_FFFF, 0xFFFF_FFFF, 0], // ...but leaf 7 is not available
            ext1: [0, 0, 0xFFFF_FFFF, 0xFFFF_FFFF],
            leaf_d0: [0, 0, 0, 0],
            max_basic: 1,         // leaf 7 absent
            max_ext: 0x8000_0000, // ext leaf 1 absent
        };
        let f = Features::decode(&id);
        assert!(f.avx && f.sse2, "leaf 1 is read");
        assert!(!f.avx2 && !f.smep && !f.bmi2, "leaf 7 is not");
        assert!(!f.nx && !f.long_mode && !f.lzcnt, "ext leaf 1 is not");
        assert_eq!(f.arch_level(), None, "no long mode: not usable");
        assert_eq!(xsave_area_size(&id), None);
    }

    #[test]
    fn xsave_size_rejects_nonsense() {
        let mut id = modern();
        id.leaf_d0[2] = 100; // below the 576-byte minimum
        assert_eq!(xsave_area_size(&id), None);
        id.leaf_d0[2] = 576;
        assert_eq!(xsave_area_size(&id), Some(576));
    }
}
