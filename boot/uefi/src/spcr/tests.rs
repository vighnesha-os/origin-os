//! The loader's walk to the SPCR (TASKS item 84), over tables built here in a model of memory:
//! good, short, mis-summed, without an SPCR, with a memory-mapped SPCR and with a refused one.
//! The boot scenarios prove the path on QEMU's tables and on the rented board's shapes.

use origin_boot_info::{EARLY_UART_BAUD, EarlyUart, MemoryRegion, RegionKind};
use origin_spcr::SpcrError;

use super::{
    Found, HeaderFault, MAX_SPCR_LENGTH, MAX_XSDT_LENGTH, Missing, Refused, RsdpError, XsdtError,
    described, find,
};

const RSDP_AT: u64 = 0x9000_0014;
const XSDT_AT: u64 = 0x9000_1000;
const OTHER_AT: u64 = 0x9000_2000;
const SPCR_AT: u64 = 0x9000_3000;
const SECOND_AT: u64 = 0x9000_4000;

/// Memory as the loader sees it: ranges it may read, each holding bytes.
#[derive(Clone)]
struct Memory(Vec<(u64, Vec<u8>)>);

impl Memory {
    fn read(&self, at: u64, out: &mut [u8]) -> bool {
        for (base, bytes) in &self.0 {
            let Some(offset) = at.checked_sub(*base) else {
                continue;
            };
            let Ok(offset) = usize::try_from(offset) else {
                continue;
            };
            if let Some(src) = offset
                .checked_add(out.len())
                .and_then(|end| bytes.get(offset..end))
            {
                out.copy_from_slice(src);
                return true;
            }
        }
        false
    }

    fn find(&self, rsdp: u64) -> Found {
        find(rsdp, &mut |at, out| self.read(at, out))
    }

    fn at(&mut self, base: u64) -> &mut Vec<u8> {
        &mut self.0.iter_mut().find(|(b, _)| *b == base).unwrap().1
    }
}

fn seal(t: &mut [u8], at: usize) {
    t[at] = 0;
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[at] = 0u8.wrapping_sub(sum);
}

fn rsdp(xsdt: u64) -> Vec<u8> {
    let mut r = vec![0u8; 36];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[9..15].copy_from_slice(b"ORIGIN");
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt.to_le_bytes());
    seal_rsdp(&mut r);
    r
}

fn seal_rsdp(r: &mut [u8]) {
    r[8] = 0;
    r[32] = 0;
    let first = r[..20].iter().fold(0u8, |s, &b| s.wrapping_add(b));
    r[8] = 0u8.wrapping_sub(first);
    let all = r.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    r[32] = 0u8.wrapping_sub(all);
}

fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[..4].copy_from_slice(signature);
    t[8] = 1;
    t[10..16].copy_from_slice(b"ORIGIN");
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    seal(&mut t, 9);
    t
}

fn xsdt(entries: &[u64]) -> Vec<u8> {
    let body: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
    table(b"XSDT", &body)
}

/// A revision 2 SPCR for COM2 at 115 200 baud, as QEMU's and the rented board's.
fn spcr() -> Vec<u8> {
    let mut t = vec![0u8; 80];
    t[..4].copy_from_slice(b"SPCR");
    t[4..8].copy_from_slice(&80u32.to_le_bytes());
    t[8] = 2;
    t[10..16].copy_from_slice(b"ORIGIN");
    t[40..52].copy_from_slice(&[1, 8, 0, 1, 0xF8, 0x02, 0, 0, 0, 0, 0, 0]);
    t[58] = 7;
    t[60] = 1;
    t[64..68].copy_from_slice(&[0xFF; 4]);
    seal(&mut t, 9);
    t
}

fn machine() -> Memory {
    Memory(vec![
        (RSDP_AT, rsdp(XSDT_AT)),
        (XSDT_AT, xsdt(&[OTHER_AT, SPCR_AT])),
        (OTHER_AT, table(b"APIC", &[0; 8])),
        (SPCR_AT, spcr()),
    ])
}

const COM2: EarlyUart = EarlyUart {
    port: 0x2F8,
    interface: 0,
    flags: EARLY_UART_BAUD,
    baud: 115_200,
    clock_hz: 0,
    reserved: 0,
};

#[test]
fn the_rented_boards_spcr_names_com2() {
    let found = machine().find(RSDP_AT);
    assert_eq!(found, Found::Port(COM2));
    assert_eq!(found.early_uart(), COM2);
    assert_eq!(COM2.check(), Ok(()));
    assert_eq!(
        found.to_string(),
        "the SPCR names a UART at I/O port 0x2f8 (interface 0x0, 115200 baud): the kernel writes to it from its first line"
    );
}

#[test]
fn a_rate_left_to_the_firmware_and_a_stated_clock_are_carried() {
    let mut m = machine();
    let t = m.at(SPCR_AT);
    t[58] = 0;
    seal(t, 9);
    let found = m.find(RSDP_AT);
    let uart = EarlyUart {
        flags: 0,
        baud: 0,
        ..COM2
    };
    assert_eq!(found, Found::Port(uart));
    assert!(
        found
            .to_string()
            .contains("(interface 0x0, the speed as the firmware set it)")
    );
    // Revision 3 states the UART's clock at offset 76.
    let mut m = machine();
    let t = m.at(SPCR_AT);
    t[8] = 3;
    t[76..80].copy_from_slice(&48_000_000u32.to_le_bytes());
    seal(t, 9);
    let found = m.find(RSDP_AT);
    assert_eq!(found.early_uart().stated_clock(), Some(48_000_000));
    assert_eq!(found.early_uart().check(), Ok(()));
}

#[test]
fn a_memory_mapped_uart_names_none() {
    let mut m = machine();
    let t = m.at(SPCR_AT);
    t[36] = 0x12;
    t[40..52].copy_from_slice(&[0, 32, 0, 3, 0, 0x90, 0xDC, 0xFE, 0, 0, 0, 0]);
    seal(t, 9);
    let found = m.find(RSDP_AT);
    assert_eq!(found, Found::Memory(0xFEDC_9000));
    assert_eq!(found.early_uart(), EarlyUart::NONE);
    assert!(found.to_string().starts_with(
        "the SPCR names a UART in memory space at 0xfedc9000: the kernel moves to it once its page tables are live"
    ));
}

#[test]
fn a_refused_spcr_names_none() {
    // Interface 3 (a full 16550 with no FIFO, not 16550-compatible here) and the keyboard
    // controller's port: the parser's refusals, word for word.
    for (offset, value, why) in [
        (36usize, 3u8, SpcrError::Interface(3)),
        (44, 0x60, SpcrError::Address(0x60)),
    ] {
        let mut m = machine();
        let t = m.at(SPCR_AT);
        t[offset] = value;
        if offset == 44 {
            t[45] = 0;
        }
        seal(t, 9);
        let found = m.find(RSDP_AT);
        assert_eq!(found, Found::Refused(Refused::Parse(why)));
        assert_eq!(found.early_uart(), EarlyUart::NONE);
        assert_eq!(
            found.to_string(),
            format!(
                "the SPCR was refused ({why:?}); the kernel's console starts on COM1 and the screen"
            )
        );
    }
}

#[test]
fn the_rsdp_is_checked_as_the_kernel_checks_it() {
    assert_eq!(machine().find(0), Found::Missing(Missing::NoRsdp));
    assert_eq!(
        machine().find(RSDP_AT + 1),
        Found::Missing(Missing::RsdpUnreadable(RSDP_AT + 1))
    );
    type Mutate = fn(&mut Vec<u8>);
    let cases: [(Mutate, RsdpError); 5] = [
        (|r| r[0] = b'X', RsdpError::Signature),
        (|r| r[9] ^= 1, RsdpError::Checksum),
        (|r| r[28] ^= 1, RsdpError::Checksum),
        (|r| r[15] = 0, RsdpError::Revision(0)),
        (|r| r[20] = 20, RsdpError::Length(20)),
    ];
    for (i, (mutate, why)) in cases.into_iter().enumerate() {
        let mut m = machine();
        let r = m.at(RSDP_AT);
        mutate(r);
        // The first two are judged before any checksum; the rest are re-sealed so that only the
        // field is wrong.
        if !matches!(i, 1 | 2) {
            seal_rsdp(r);
        }
        assert_eq!(
            m.find(RSDP_AT),
            Found::Missing(Missing::Rsdp(why)),
            "case {i}"
        );
    }
}

#[test]
fn the_xsdt_is_checked_whole() {
    let mut m = machine();
    m.at(XSDT_AT)[40] ^= 1;
    assert_eq!(
        m.find(RSDP_AT),
        Found::Missing(Missing::Xsdt(XsdtError::Checksum))
    );
    let mut m = machine();
    m.at(XSDT_AT)[..4].copy_from_slice(b"RSDT");
    assert_eq!(
        m.find(RSDP_AT),
        Found::Missing(Missing::Xsdt(XsdtError::Signature(*b"RSDT")))
    );
    for length in [20u32, MAX_XSDT_LENGTH + 1] {
        let mut m = machine();
        let x = m.at(XSDT_AT);
        x[4..8].copy_from_slice(&length.to_le_bytes());
        seal(x, 9);
        assert_eq!(
            m.find(RSDP_AT),
            Found::Missing(Missing::Xsdt(XsdtError::Length(length)))
        );
    }
    // A length past the bytes that can be read: the read is refused, not run off the end.
    let mut m = machine();
    let x = m.at(XSDT_AT);
    x[4..8].copy_from_slice(&4096u32.to_le_bytes());
    seal(x, 9);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Missing(Missing::XsdtUnreadable(XSDT_AT))
    );
    // Not readable at all.
    let mut m = machine();
    m.0.retain(|(base, _)| *base != XSDT_AT);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Missing(Missing::XsdtUnreadable(XSDT_AT))
    );
}

#[test]
fn without_an_spcr_none_is_named() {
    let mut m = machine();
    *m.at(XSDT_AT) = xsdt(&[OTHER_AT]);
    let found = m.find(RSDP_AT);
    assert_eq!(found, Found::Missing(Missing::NotListed));
    assert_eq!(
        found.to_string(),
        "no SPCR (the XSDT lists none); the kernel's console starts on COM1 and the screen"
    );
    // An entry the loader cannot read might have been the SPCR: said so, with its address.
    let mut m = machine();
    *m.at(XSDT_AT) = xsdt(&[OTHER_AT, 0x1234_5000]);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Missing(Missing::EntryUnreadable(0x1234_5000))
    );
    // An empty XSDT.
    let mut m = machine();
    *m.at(XSDT_AT) = xsdt(&[]);
    assert_eq!(m.find(RSDP_AT), Found::Missing(Missing::NotListed));
}

#[test]
fn the_first_spcr_whose_header_checks_is_parsed() {
    // A mis-summed SPCR first, then a good one: the good one is taken.
    let mut m = machine();
    let mut bad = spcr();
    bad[9] ^= 1;
    m.0.push((SECOND_AT, bad));
    *m.at(XSDT_AT) = xsdt(&[OTHER_AT, SECOND_AT, SPCR_AT]);
    assert_eq!(m.find(RSDP_AT), Found::Port(COM2));
    // Only the mis-summed one: refused, with why.
    *m.at(XSDT_AT) = xsdt(&[SECOND_AT]);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Refused(Refused::Header(HeaderFault::Checksum))
    );
    // A null entry names nothing, as in the kernel's walk (T84-8), and is passed over.
    *m.at(XSDT_AT) = xsdt(&[0, SPCR_AT]);
    assert_eq!(m.find(RSDP_AT), Found::Port(COM2));
    *m.at(XSDT_AT) = xsdt(&[0]);
    assert_eq!(m.find(RSDP_AT), Found::Missing(Missing::NotListed));
    // An SPCR the loader cannot read in full may be the kernel's first: no later one is named
    // (T84-8), too long or unreadable alike.
    for (declared, why) in [
        (
            MAX_SPCR_LENGTH as u32 + 1,
            Refused::TooLong(MAX_SPCR_LENGTH as u32 + 1),
        ),
        (120, Refused::Unreadable(SECOND_AT)),
    ] {
        let mut m = machine();
        let mut first = spcr();
        first[4..8].copy_from_slice(&declared.to_le_bytes());
        m.0.push((SECOND_AT, first));
        *m.at(XSDT_AT) = xsdt(&[SECOND_AT, SPCR_AT]);
        assert_eq!(m.find(RSDP_AT), Found::Refused(why));
    }
    // A checking SPCR that the parser refuses decides: no later one is tried.
    let mut m = machine();
    let mut refused = spcr();
    refused[36] = 3;
    seal(&mut refused, 9);
    m.0.push((SECOND_AT, refused));
    *m.at(XSDT_AT) = xsdt(&[SECOND_AT, SPCR_AT]);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Refused(Refused::Parse(SpcrError::Interface(3)))
    );
}

#[test]
fn a_short_or_overlong_spcr_is_refused() {
    // Declares less than its header: read as the header alone, which the header's check refuses.
    let mut m = machine();
    let t = m.at(SPCR_AT);
    t[4..8].copy_from_slice(&20u32.to_le_bytes());
    seal(t, 9);
    assert_eq!(
        m.find(RSDP_AT),
        Found::Refused(Refused::Header(HeaderFault::Length))
    );
    // Declares more than the loader reads.
    let mut m = machine();
    let declared = MAX_SPCR_LENGTH as u32 + 1;
    let t = m.at(SPCR_AT);
    t[4..8].copy_from_slice(&declared.to_le_bytes());
    assert_eq!(m.find(RSDP_AT), Found::Refused(Refused::TooLong(declared)));
    // Declares more than the memory holds: the read is refused.
    let mut m = machine();
    let t = m.at(SPCR_AT);
    t[4..8].copy_from_slice(&120u32.to_le_bytes());
    assert_eq!(
        m.find(RSDP_AT),
        Found::Refused(Refused::Unreadable(SPCR_AT))
    );
    // Shorter than the 80 bytes every revision defines, though its header checks.
    let mut m = machine();
    *m.at(SPCR_AT) = {
        let mut t = spcr();
        t.truncate(60);
        t[4..8].copy_from_slice(&60u32.to_le_bytes());
        seal(&mut t, 9);
        t
    };
    assert_eq!(
        m.find(RSDP_AT),
        Found::Refused(Refused::Parse(SpcrError::Length(60)))
    );
}

fn region(base: u64, pages: u64) -> MemoryRegion {
    region_of(base, pages, RegionKind::AcpiNvs)
}

fn region_of(base: u64, pages: u64, kind: RegionKind) -> MemoryRegion {
    MemoryRegion {
        base,
        pages,
        kind: kind as u32,
        reserved: 0,
    }
}

#[test]
fn only_described_memory_is_read() {
    let map = [region(0x1000, 2), region(0x3000, 1), region(0x5000, 4)];
    assert!(described(&map, 0x1000, 0x2000));
    assert!(described(&map, 0x1FF0, 0x10));
    // Two regions that abut are one range.
    assert!(described(&map, 0x2FF0, 0x20));
    assert!(described(&map, 0x5000, 0x4000));
    // A gap, a range running past the last region, one before the first, and the edges.
    assert!(!described(&map, 0x3FF0, 0x20));
    assert!(!described(&map, 0x8FF0, 0x20));
    assert!(!described(&map, 0x0FF0, 0x20));
    assert!(!described(&map, 0, 8));
    assert!(!described(&map, 0x1000, 0));
    assert!(!described(&map, u64::MAX - 3, 8));
    assert!(!described(&[], 0x1000, 8));
    // What the kernel's walk refuses as well (T84-6): the first page, which firmware may leave
    // unmapped to catch null pointers, memory-mapped I/O and unaccepted memory, even where the
    // map describes them; loader data, where the refused machine's RSDP is, is read.
    let low = [region_of(0, 4, RegionKind::LoaderReclaimable)];
    assert!(!described(&low, 0x10, 36));
    assert!(!described(&low, 0x0FFF, 8));
    assert!(described(&low, 0x1000, 36));
    let kinds = [
        region_of(0x1000, 1, RegionKind::AcpiReclaimable),
        region_of(0x2000, 1, RegionKind::Mmio),
        region_of(0x3000, 1, RegionKind::Unaccepted),
        region_of(0x4000, 1, RegionKind::LoaderReclaimable),
    ];
    assert!(described(&kinds, 0x1000, 0x1000));
    assert!(!described(&kinds, 0x2000, 8));
    assert!(
        !described(&kinds, 0x1FF8, 16),
        "a range that runs into MMIO"
    );
    assert!(!described(&kinds, 0x3000, 8));
    assert!(described(&kinds, 0x4000, 8));
}

/// The bound on one walk's reads: the RSDP, the XSDT's header and its bytes in 64-byte chunks,
/// two reads for each entry, and one SPCR.
const MOST_READS: usize = 3 + MAX_XSDT_LENGTH as usize / 64 + 2 * (MAX_XSDT_LENGTH as usize / 8);

/// A port block allowed by a rule written here, not the one `EarlyUart::check` and the parser
/// share (T84-7): the four standard COM blocks, or a whole block at or above `0x1000`.
fn literal_port_rule(port: u16) -> bool {
    matches!(port, 0x3F8 | 0x2F8 | 0x3E8 | 0x2E8) || (0x1000..=0xFFF8).contains(&port)
}

/// Mutations of every byte of the three tables (TASKS item 84's review, T84-7). For each walk:
/// it never panics; it asks for at most [`MOST_READS`] reads, each no longer than an SPCR the
/// loader reads (1 KiB); and a UART it names is in a port block a rule written here allows. Over
/// the whole soak it must both name a UART often and ask for memory the model refuses, so both
/// the good path and the refusals are reached.
#[test]
fn hostile_tables_never_name_an_unchecked_uart() {
    use std::cell::Cell;
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let bases = [RSDP_AT, XSDT_AT, SPCR_AT];
    let (mut ports, mut refused_reads) = (0, 0);
    for _ in 0..20_000 {
        let mut m = machine();
        let base = bases[(next() % 3) as usize];
        let t = m.at(base);
        for _ in 0..1 + next() % 3 {
            let i = (next() % t.len() as u64) as usize;
            t[i] = next() as u8;
        }
        if next() % 2 == 0 {
            if base == RSDP_AT {
                seal_rsdp(t);
            } else {
                seal(t, 9);
            }
        }
        let (reads, longest, refused) = (Cell::new(0usize), Cell::new(0usize), Cell::new(0));
        let found = find(RSDP_AT, &mut |at, out| {
            reads.set(reads.get() + 1);
            longest.set(longest.get().max(out.len()));
            let ok = m.read(at, out);
            refused.set(refused.get() + usize::from(!ok));
            ok
        });
        assert!(
            reads.get() <= MOST_READS,
            "{} reads: {found:?}",
            reads.get()
        );
        assert!(
            longest.get() <= MAX_SPCR_LENGTH,
            "a read of {}",
            longest.get()
        );
        refused_reads += refused.get();
        let uart = found.early_uart();
        if uart.is_present() {
            ports += 1;
            assert!(literal_port_rule(uart.port), "{found:?}");
            assert!(
                matches!(uart.interface, 0x00 | 0x01 | 0x12 | 0x14),
                "{found:?}"
            );
        }
        let _ = found.to_string();
    }
    // The soak reached both the good path and the refused reads often enough to mean something.
    assert!(ports > 1_000, "{ports}");
    assert!(refused_reads > 100, "{refused_reads}");
}
