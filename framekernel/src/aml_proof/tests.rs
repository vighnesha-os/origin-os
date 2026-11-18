//! The boot proof of the AML interpreter on the host: QEMU-shaped tables built in memory with the
//! interpreter's own test encoder, behind a fake firmware like the census's, and every branch of
//! the verdict (the contract of TASKS item 63's step B3, §6).

use alloc::string::String;

use super::*;

// The AML encoder `origin-aml`'s tests use (ADR-0019 A5), compiled here as it is there.
#[allow(dead_code)]
#[path = "../../../firmware/acpi/aml/src/tests/encode.rs"]
mod encode;

use encode::{
    B, Fe, alias, arg, bank_field, buffer, byte, call, device, dword, field, increment,
    index_field, local, logic, method, name, nm, one, op_region, package, ret, scope, store,
    string, table, while_, word, zero,
};

/// Where the tables of a [`Machine`] start in physical memory.
const BASE: u64 = 0x7F00_0000;
const RSDP: u64 = BASE;
const XSDT: u64 = BASE + 0x100;
const FADT: u64 = BASE + 0x1000;
const DSDT: u64 = BASE + 0x2000;
/// The first SSDT; each next one 0x8000 bytes on.
const SSDT: u64 = BASE + 0x1_0000;
const SSDT_STRIDE: u64 = 0x8000;
/// An address no firmware memory covers, above 4 GiB and below.
const NOWHERE: u64 = 0x1_0000_0000;
const LOW: u64 = 0x10_0000;

/// The clock of a host that never moves.
const fn still() -> u64 {
    0
}

/// A WordBusNumber descriptor for buses 0 to 255, and the end tag: QEMU's root bridge's `_CRS`
/// begins so.
const CRS: [u8; 18] = [
    0x88, 0x0D, 0x00, 0x02, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x01,
    0x79, 0x00,
];

/// A PCI interrupt link whose `_STA` reads its routing register in the LPC bridge's
/// configuration space, as QEMU's do.
fn link(n: &str, register: &str) -> B {
    device(
        n,
        &[
            name("_HID", dword(0x0F0C_D041)),
            method(
                "_STA",
                0,
                &[ret(nm(&alloc::format!("\\_SB_.PCI0.SF8_.{register}")))],
            ),
        ],
    )
}

/// A processor device whose `_STA` reads its presence bit in SystemIO, as QEMU's do.
fn cpu(n: &str, uid: u8) -> B {
    device(
        n,
        &[
            name("_HID", string("ACPI0007")),
            name("_UID", byte(uid)),
            method("_STA", 0, &[ret(call("\\_SB_.CPUS.CSTA", &[byte(uid)]))]),
        ],
    )
}

/// What QEMU's DSDT holds that the proof reads: `\_S5_`, `\PICF` and `\_PIC`, the root bridge
/// with its `_HID` and `_CRS`, an HPET whose `_STA` reads its registers under the Global Lock,
/// four links and two processors; then `extra`. Revision 1, as QEMU's: 32-bit integers.
fn qemu_dsdt(extra: &[B]) -> Vec<u8> {
    qemu_dsdt_of(1, extra)
}

/// As [`qemu_dsdt`], of `revision`.
fn qemu_dsdt_of(revision: u8, extra: &[B]) -> Vec<u8> {
    qemu_dsdt_with(revision, &[], name("_CRS", buffer(byte(18), &CRS)), extra)
}

/// As [`qemu_dsdt_of`], with `pic_more` run in `\_PIC` after its store, and `crs` in place of the
/// root bridge's `_CRS` name: for the tests that make a named result costly (B3-14).
fn qemu_dsdt_with(revision: u8, pic_more: &[B], crs: B, extra: &[B]) -> Vec<u8> {
    let mut pic = vec![store(arg(0), nm("PICF"))];
    pic.extend_from_slice(pic_more);
    let mut body = vec![
        name("\\_S5_", package(4, &[zero(), zero(), zero(), zero()])),
        name("PICF", zero()),
        method("_PIC", 1, &pic),
        scope(
            "\\_SB_",
            &[
                device(
                    "PCI0",
                    &[
                        name("_HID", dword(0x080A_D041)),
                        name("_ADR", zero()),
                        crs,
                        device(
                            "SF8_",
                            &[
                                name("_ADR", dword(0x001F_0000)),
                                op_region("PIRQ", 2, byte(0x60), byte(0x0C)),
                                field(
                                    "PIRQ",
                                    0x01,
                                    &[
                                        Fe::Named("PRQA", 8),
                                        Fe::Named("PRQB", 8),
                                        Fe::Named("PRQC", 8),
                                        Fe::Named("PRQD", 8),
                                    ],
                                ),
                            ],
                        ),
                    ],
                ),
                device(
                    "HPET",
                    &[
                        name("_HID", dword(0x0301_D041)),
                        op_region("HPTM", 0, dword(0xFED0_0000), word(0x400)),
                        // DWordAcc, Lock: each access under the Global Lock.
                        field(
                            "HPTM",
                            0x13,
                            &[Fe::Named("VEND", 32), Fe::Named("PRD_", 32)],
                        ),
                        method("_STA", 0, &[ret(nm("VEND"))]),
                    ],
                ),
                link("LNKA", "PRQA"),
                link("LNKB", "PRQB"),
                link("LNKC", "PRQC"),
                link("LNKD", "PRQD"),
                op_region("PRST", 1, word(0x0CD8), byte(0x0C)),
                field(
                    "PRST",
                    0x01,
                    &[Fe::Reserved(32), Fe::Named("CPEN", 1), Fe::Reserved(7)],
                ),
                device(
                    "CPUS",
                    &[
                        name("_HID", string("ACPI0010")),
                        method("CSTA", 1, &[ret(nm("\\_SB_.CPEN"))]),
                        cpu("C000", 0),
                        cpu("C001", 1),
                    ],
                ),
            ],
        ),
    ];
    body.extend_from_slice(extra);
    qemu_ids(table(b"DSDT", revision, &body))
}

/// `t` with QEMU's OEM ID and OEM table ID ([`QEMU_DSDT`]), sealed again: the proof runs only on
/// QEMU's tables (B3-2).
fn qemu_ids(mut t: Vec<u8>) -> Vec<u8> {
    t[10..16].copy_from_slice(&QEMU_DSDT.0);
    t[16..24].copy_from_slice(&QEMU_DSDT.1);
    t[9] = 0;
    let sum = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

/// A device below a PCI bridge with a PCI_Config region: naming its function needs the bridge's
/// bus number, which the boot host refuses.
fn below_a_bridge() -> B {
    scope(
        "\\_SB_.PCI0",
        &[device(
            "BR10",
            &[
                name("_ADR", dword(0x0001_0000)),
                device(
                    "DEV0",
                    &[
                        name("_ADR", zero()),
                        op_region("DCFG", 2, zero(), byte(0x10)),
                        field("DCFG", 0x02, &[Fe::Named("DVID", 16)]),
                        method("_STA", 0, &[ret(nm("DVID"))]),
                    ],
                ),
            ],
        )],
    )
}

/// The deepest chain of fields the interpreter allows a host access through, short of its
/// limit: an alias of an index field over index fields over a bank field, whose bank register
/// is a field (the review of B2's Addendum 2: the field chain, one alias), read by a device's
/// `_STA`.
fn deep_chain() -> B {
    scope(
        "\\_SB_",
        &[
            op_region("DPIO", 1, word(0x0300), byte(0x10)),
            field("DPIO", 0x01, &[Fe::Named("BNK0", 8)]),
            bank_field(
                "DPIO",
                "BNK0",
                one(),
                0x01,
                &[Fe::Reserved(24), Fe::Named("BIX0", 8), Fe::Named("BDT0", 8)],
            ),
            index_field(
                "BIX0",
                "BDT0",
                0x01,
                &[Fe::Named("IX10", 8), Fe::Named("DT10", 8)],
            ),
            index_field(
                "IX10",
                "DT10",
                0x01,
                &[Fe::Named("IX20", 8), Fe::Named("DT20", 8)],
            ),
            index_field("IX20", "DT20", 0x01, &[Fe::Named("DEEP", 8)]),
            alias("DEEP", "ADEP"),
            device("DPDV", &[method("_STA", 0, &[ret(nm("\\_SB_.ADEP"))])]),
        ],
    )
}

/// `n` devices whose `_STA` each counts to `count` before it answers.
fn counting_devices(n: usize, count: u64) -> B {
    let devices: Vec<B> = (0..n)
        .map(|k| {
            device(
                &alloc::format!("W{k:03}"),
                &[method(
                    "_STA",
                    0,
                    &[
                        store(zero(), local(0)),
                        while_(
                            logic(0x95, local(0), encode::int(count)),
                            &[increment(local(0))],
                        ),
                        ret(byte(0x0F)),
                    ],
                )],
            )
        })
        .collect();
    scope("\\_SB_", &devices)
}

/// A table with `signature` and `body`, its length and checksum set.
fn raw_table(signature: &[u8; 4], revision: u8, body: &[u8]) -> Vec<u8> {
    table(signature, revision, &[body.to_vec()])
}

/// The firmware's tables as QEMU gives them, in fake physical memory: an RSDP, an XSDT listing a
/// FADT and the SSDTs, the FADT's DSDT.
struct Machine {
    dsdt: Vec<u8>,
    ssdts: Vec<Vec<u8>>,
    /// Where the FADT says the DSDT is.
    dsdt_at: u64,
    /// More XSDT entries, listed after the SSDTs.
    entries: Vec<u64>,
    /// The stack's lowest address, if the test knows it.
    stack_floor: Option<usize>,
    /// How far the firmware's clock moves at each reading, in nanoseconds (B3-14's time half).
    tick: u64,
}

impl Machine {
    fn new(dsdt: Vec<u8>) -> Self {
        Self {
            dsdt,
            ssdts: Vec::new(),
            dsdt_at: DSDT,
            entries: Vec::new(),
            stack_floor: None,
            tick: 0,
        }
    }

    /// Lays the tables out and runs the proof with `host`, `limits` and `heap`.
    fn run_with<H: Host>(
        &self,
        host: H,
        limits: Limits,
        heap: &mut dyn FnMut() -> Option<u64>,
    ) -> (Findings, String) {
        let mut memory = vec![0u8; usize::try_from(SSDT - BASE).unwrap() + 8 * 0x8000];
        let mut put = |addr: u64, bytes: &[u8]| {
            let at = usize::try_from(addr - BASE).unwrap();
            memory[at..at + bytes.len()].copy_from_slice(bytes);
        };
        // A FADT of 276 bytes, as the census's tests build it, its DSDT at `dsdt_at`.
        let mut f = vec![0u8; 240];
        f[40 - 36..44 - 36].copy_from_slice(&u32::try_from(self.dsdt_at).unwrap().to_le_bytes());
        f[46 - 36..48 - 36].copy_from_slice(&9u16.to_le_bytes());
        put(FADT, &raw_table(b"FACP", 6, &f));
        put(DSDT, &self.dsdt);
        let mut xsdt = FADT.to_le_bytes().to_vec();
        for (k, ssdt) in self.ssdts.iter().enumerate() {
            let at = SSDT + SSDT_STRIDE * u64::try_from(k).unwrap();
            put(at, ssdt);
            xsdt.extend_from_slice(&at.to_le_bytes());
        }
        for e in &self.entries {
            xsdt.extend_from_slice(&e.to_le_bytes());
        }
        put(XSDT, &raw_table(b"XSDT", 1, &xsdt));
        let mut rsdp = vec![0u8; 36];
        rsdp[..8].copy_from_slice(b"RSD PTR ");
        rsdp[9..15].copy_from_slice(b"ORIGIN");
        rsdp[15] = 2;
        rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
        rsdp[24..32].copy_from_slice(&XSDT.to_le_bytes());
        let sum = |b: &[u8]| 0u8.wrapping_sub(b.iter().fold(0u8, |s, &x| s.wrapping_add(x)));
        rsdp[8] = sum(&rsdp[..20]);
        rsdp[32] = sum(&rsdp);
        put(RSDP, &rsdp);
        let mut fw = Memory {
            bytes: Vec::leak(memory),
            clock: core::cell::Cell::new(0),
            tick: self.tick,
        };
        let mut out = String::new();
        let found = run(
            &mut out,
            &mut fw,
            RSDP,
            host,
            limits,
            heap,
            self.stack_floor,
        );
        (found, out)
    }

    /// The proof with the boot's host, limits and a heap that never moves.
    fn run(&self) -> (Findings, String) {
        self.run_with(BootHost::new(still), Limits::BOOT_PROOF, &mut || Some(4096))
    }
}

/// Firmware memory at [`BASE`], and a clock that moves by `tick` at each reading.
struct Memory {
    bytes: &'static [u8],
    clock: core::cell::Cell<u64>,
    tick: u64,
}

impl Memory {
    fn range(&self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
        let at = addr
            .checked_sub(BASE)
            .and_then(|a| usize::try_from(a).ok())
            .ok_or("not the firmware's memory")?;
        self.bytes
            .get(at..at.checked_add(len).ok_or("wraps")?)
            .ok_or("not the firmware's memory")
    }
}

impl Firmware for Memory {
    fn lend(&mut self, addr: u64, len: usize) -> Result<&'static [u8], &'static str> {
        self.range(addr, len)
    }
    fn copy(&mut self, addr: u64, out: &mut [u8]) -> Result<(), &'static str> {
        out.copy_from_slice(self.range(addr, out.len())?);
        Ok(())
    }
    fn now(&self) -> u64 {
        let now = self.clock.get();
        self.clock.set(now + self.tick);
        now
    }
}

/// The boot host with one answer changed, for the controls.
#[derive(Debug)]
struct Changed {
    boot: BootHost,
    /// Answers SystemMemory reads with 0Fh (present and functioning) instead of refusing them.
    grant_memory: bool,
    /// Fails every release of the Global Lock.
    fail_release: bool,
    /// Answers every bridge's secondary bus number with 1 instead of refusing it.
    grant_bridge: bool,
}

impl Changed {
    const fn new() -> Self {
        Self {
            boot: BootHost::new(still),
            grant_memory: false,
            fail_release: false,
            grant_bridge: false,
        }
    }
}

impl Host for Changed {
    fn now_100ns(&mut self) -> u64 {
        self.boot.now_100ns()
    }
    fn read(&mut self, access: &RegionAccess<'_>) -> Result<u64, HostError> {
        if self.grant_memory && access.space == Space::SYSTEM_MEMORY {
            return Ok(0x0F);
        }
        self.boot.read(access)
    }
    fn write(&mut self, access: &RegionAccess<'_>, value: u64) -> Result<(), HostError> {
        self.boot.write(access, value)
    }
    fn bridge_bus(&mut self, request: &BridgeBus) -> Result<u8, HostError> {
        if self.grant_bridge {
            return Ok(1);
        }
        self.boot.bridge_bus(request)
    }
    fn transfer(&mut self, access: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError> {
        self.boot.transfer(access, data)
    }
    fn sleep_ms(&mut self, ms: u64) {
        self.boot.sleep_ms(ms);
    }
    fn stall_us(&mut self, us: u8) {
        self.boot.stall_us(us);
    }
    fn acquire_global_lock(&mut self, timeout_ms: u16) -> Result<bool, HostError> {
        self.boot.acquire_global_lock(timeout_ms)
    }
    fn release_global_lock(&mut self) -> Result<(), HostError> {
        if self.fail_release {
            return Err(HostError::Failed);
        }
        self.boot.release_global_lock()
    }
    fn notify(&mut self, node: &[[u8; 4]], value: u64) {
        self.boot.notify(node, value);
    }
    fn fatal(&mut self, kind: u8, code: u32, arg: u64) {
        self.boot.fatal(kind, code, arg);
    }
    fn debug(&mut self, text: &[u8]) {
        self.boot.debug(text);
    }
    fn table(
        &mut self,
        key: &TableKey,
        out: &mut dyn FnMut(u64, &[u8]),
    ) -> Result<bool, HostError> {
        self.boot.table(key, out)
    }
}

fn last_line(out: &str) -> &str {
    out.lines().last().unwrap()
}

#[test]
fn a_qemu_shaped_machine_passes() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    // A third processor in an SSDT, loaded after the DSDT in the XSDT's order.
    m.ssdts.push(table(
        b"SSDT",
        1,
        &[scope("\\_SB_.CPUS", &[cpu("C002", 2)])],
    ));
    let (found, out) = m.run();
    assert!(found.passed(), "{out}");
    assert!(
        out.contains("origin-kernel: aml: DSDT rev 1, ")
            && out.contains("OEM \"BOCHS \", \"BXPC    \": loaded as table 0"),
        "{out}"
    );
    assert!(out.contains("origin-kernel: aml: SSDT rev 1, "), "{out}");
    assert!(out.contains(": loaded as table 1"), "{out}");
    assert!(
        out.contains(
            "origin-kernel: aml: named results: \\_S5_ true; \\PICF 0 true; \\_PIC (1) true; \\PICF 1 true; \\_SB.PCI0._HID PNP0A08 true; \\_OSI (\"Windows 2022\") Ones true; \\_OSI (\"Linux\") Zero true; \\_REV 2 true; \\_SB.PCI0._CRS a template true; all true"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "origin-kernel: aml: walk: 11 devices: 3 initialized, 0 children only, 0 absent, 8 _STA refused, 0 _INI refused, 0 failed otherwise; HPET's _STA refused in SystemMemory true; 3 processors, each _STA refused in SystemIO true; 4 links (LNKx), each _STA refused in PCI_Config true"
        ),
        "{out}"
    );
    let seen = found.seen;
    assert_eq!(seen.refused, [1, 3, 4], "{out}");
    assert_eq!((seen.bridges, seen.tables, seen.answered), (0, 0, 0));
    // The HPET's field is locked: the Global Lock was taken for it, and given back.
    assert!(seen.lock_taken > 0 && seen.lock_taken == seen.lock_given_back);
    assert_eq!(found.tables, 2);
    assert!(found.depth > 0 && found.peak > 0 && found.nodes > 0);
    assert!(
        last_line(&out).ends_with(
            "8 refusals (SystemMemory 1, SystemIO 3, PCI_Config 4, bridges 0); named results true; heap back true; passed"
        ),
        "{out}"
    );
}

#[test]
fn the_boot_host_refuses_everything_that_reaches_hardware_and_grants_the_lock() {
    let mut h = BootHost::new(still);
    let access = RegionAccess {
        space: Space::SYSTEM_MEMORY,
        address: 0,
        width: 8,
        pci: None,
        region: &[],
        time_left_100ns: 1,
    };
    assert_eq!(h.read(&access), Err(HostError::Refused));
    assert_eq!(h.write(&access, 0), Err(HostError::Refused));
    let bridge = BridgeBus {
        bridge: origin_aml::PciAddress {
            segment: 0,
            bus: 0,
            device: 1,
            function: 0,
        },
        time_left_100ns: 1,
    };
    assert_eq!(h.bridge_bus(&bridge), Err(HostError::Refused));
    let transfer = BufferAccess {
        space: Space::SMBUS,
        address: 0,
        write: false,
        attribute: 0,
        length: 0,
        connection: None,
        bit_offset: 0,
        bit_length: 8,
        region: &[],
        time_left_100ns: 1,
    };
    assert_eq!(h.transfer(&transfer, &mut [0; 4]), Err(HostError::Refused));
    let key = TableKey {
        signature: *b"OEM1",
        oem_id: [0; 6],
        oem_table_id: [0; 8],
    };
    let mut handed = false;
    assert_eq!(
        h.table(&key, &mut |_, _| handed = true),
        Err(HostError::Refused)
    );
    assert!(!handed);
    assert_eq!(h.acquire_global_lock(0), Ok(true));
    assert_eq!(h.release_global_lock(), Ok(()));
    assert!(!h.tracing());
}

#[test]
fn a_bridges_bus_refused_is_allowed_and_counted() {
    let (found, out) = Machine::new(qemu_dsdt(&[below_a_bridge()])).run();
    assert!(found.passed(), "{out}");
    assert_eq!(found.seen.bridges, 1, "{out}");
    assert!(
        out.contains(", 8 _STA refused, 0 _INI refused, 0 failed otherwise;"),
        "{out}"
    );
    assert!(last_line(&out).contains("(SystemMemory 1, SystemIO 2, PCI_Config 4, bridges 1)"));
}

#[test]
fn a_missing_dsdt_fails() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.dsdt_at = 0;
    let (found, out) = m.run();
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
    // `origin-acpi-tables` refuses a FADT that names no DSDT.
    assert!(
        out.contains("origin-kernel: aml: no DSDT: the FADT is refused"),
        "{out}"
    );
    assert!(last_line(&out).ends_with("; aml failed: no DSDT"), "{out}");

    let mut m = Machine::new(qemu_dsdt(&[]));
    m.dsdt_at = LOW;
    let (found, out) = m.run();
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
    assert!(out.contains("the DSDT: not readable"), "{out}");
}

#[test]
fn a_dsdt_with_a_bad_checksum_is_not_loaded() {
    let mut dsdt = qemu_dsdt(&[]);
    dsdt[40] ^= 1;
    let (found, out) = Machine::new(dsdt).run();
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
    assert!(
        out.contains("origin-kernel: aml: the DSDT: DSDT rev 1, "),
        "{out}"
    );
    assert!(out.contains("checksum"), "{out}");
    assert_eq!(found.tables, 0);
}

#[test]
fn a_refused_ssdt_is_not_loaded_and_fails_the_proof() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    let mut ssdt = table(b"SSDT", 1, &[name("XTRA", one())]);
    ssdt[36] ^= 1;
    m.ssdts.push(ssdt);
    let (found, out) = m.run();
    assert_eq!(
        found.failed,
        Some("a definition block was refused"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: an XSDT entry: SSDT rev 1, "),
        "{out}"
    );
    // The DSDT was still loaded, and the rest of the proof ran.
    assert_eq!(found.tables, 1);
    assert!(found.named, "{out}");
}

#[test]
fn an_xsdt_entry_that_cannot_be_read_fails_the_proof() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.entries.push(NOWHERE);
    let (found, out) = m.run();
    assert_eq!(
        found.failed,
        Some("an XSDT entry could not be read"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: an XSDT entry: not readable"),
        "{out}"
    );
}

#[test]
fn a_definition_block_that_does_not_load_fails_the_proof() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    // `XTRA` twice: the second is a duplicate, and the table is rolled back.
    m.ssdts.push(table(
        b"SSDT",
        1,
        &[name("XTRA", one()), name("XTRA", one())],
    ));
    let (found, out) = m.run();
    assert_eq!(
        found.failed,
        Some("a definition block did not load"),
        "{out}"
    );
    assert!(out.contains("load failed at table 1 offset "), "{out}");
    assert!(out.contains("Duplicate; rolled back"), "{out}");
}

#[test]
fn an_expected_result_wrong_fails_the_proof() {
    // A root bridge that says it is a conventional PCI one (PNP0A03).
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_.PCI0",
        &[store(dword(0x030A_D041), nm("_HID"))],
    )]);
    let (found, out) = Machine::new(dsdt).run();
    assert!(!found.named, "{out}");
    assert_eq!(
        found.failed,
        Some("a named result was not as QEMU's tables give it"),
        "{out}"
    );
    assert!(
        out.contains("\\_SB.PCI0._HID PNP0A08 false (the integer 0x30ad041)"),
        "{out}"
    );
    assert!(out.contains("; all false"), "{out}");
    assert!(last_line(&out).contains("named results false;"), "{out}");
}

#[test]
fn a_host_that_grants_a_region_fails_the_proof() {
    let host = Changed {
        grant_memory: true,
        ..Changed::new()
    };
    let (found, out) =
        Machine::new(qemu_dsdt(&[])).run_with(host, Limits::BOOT_PROOF, &mut || Some(0));
    assert!(!found.passed(), "{out}");
    assert_eq!(found.seen.answered, 1, "{out}");
    // The HPET's `_STA` read its register, so its required refusal is missing too; but the
    // recorder's own count comes first, as the design rests on it (B3-5).
    assert_eq!(found.walked.hpet, Some(false), "{out}");
    assert_eq!(
        found.failed,
        Some("the host answered a request for hardware other than by refusing it"),
        "{out}"
    );
    assert!(out.contains("answered otherwise 1;"), "{out}");
}

#[test]
fn a_global_lock_not_given_back_fails_the_proof() {
    let host = Changed {
        fail_release: true,
        ..Changed::new()
    };
    let (found, out) =
        Machine::new(qemu_dsdt(&[])).run_with(host, Limits::BOOT_PROOF, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("the Global Lock was not given back as often as it was granted"),
        "{out}"
    );
    assert!(found.seen.lock_other > 0, "{out}");
}

#[test]
fn a_device_that_fails_other_than_by_a_refusal_fails_the_proof() {
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_",
        &[device("BAD0", &[method("_STA", 0, &[ret(string("x"))])])],
    )]);
    let (found, out) = Machine::new(dsdt).run();
    assert_eq!(
        found.failed,
        Some("a device failed other than by a refusal"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: walk: \\_SB_.BAD0 failed other than by a refusal: Type"),
        "{out}"
    );
    assert_eq!(found.walked.failed, 1);
}

#[test]
fn the_required_refusals_must_be_there() {
    // No links, no processors: QEMU's tables always have both.
    let dsdt = qemu_ids(table(
        b"DSDT",
        1,
        &[
            name("\\_S5_", package(4, &[zero(), zero(), zero(), zero()])),
            name("PICF", zero()),
            method("_PIC", 1, &[store(arg(0), nm("PICF"))]),
        ],
    ));
    let (found, out) = Machine::new(dsdt).run();
    assert!(!found.passed(), "{out}");
    assert_eq!((found.walked.processors, found.walked.links), (0, 0));
    assert_eq!(found.walked.hpet, None, "{out}");
    assert!(
        out.contains("HPET's _STA refused in SystemMemory (no HPET);"),
        "{out}"
    );
    assert!(
        out.contains("0 processors, each _STA refused in SystemIO false;"),
        "{out}"
    );

    // A processor whose `_STA` answers without hardware.
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_.CPUS",
        &[device(
            "C009",
            &[
                name("_HID", string("ACPI0007")),
                method("_STA", 0, &[ret(byte(0x0F))]),
            ],
        )],
    )]);
    let (found, out) = Machine::new(dsdt).run();
    assert_eq!(
        found.failed,
        Some("not every processor's _STA was refused in SystemIO, or there was none"),
        "{out}"
    );
    assert_eq!(
        (found.walked.processors, found.walked.processors_refused),
        (3, 2)
    );
}

#[test]
fn a_walk_cut_off_by_its_bound_fails_the_proof() {
    // Each `_STA` fits the limit; the walk as a whole does not (the review of B2, B2-7).
    let limits = Limits {
        steps: 20_000,
        ..Limits::BOOT_PROOF
    };
    let dsdt = qemu_dsdt(&[counting_devices(20, 400)]);
    let (found, out) = Machine::new(dsdt).run_with(BootHost::new(still), limits, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("the walk was cut off by its bound"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: walk: cut off by its bound: ")
            && out.contains("Limit(Steps)"),
        "{out}"
    );
}

/// A memory limit the heap cannot give (the coordinating session's ruling on step B3's limit):
/// with the limit at its largest, a table that names a buffer of nearly 8 EiB and a device whose
/// `_STA` makes one are refused by the host's allocator, which returns null as the kernel heap
/// does when it has no frames. Each is the memory limit, named on its line; the proof fails
/// cleanly, says why, and gives the heap back.
#[test]
fn a_heap_too_small_for_the_limit_fails_the_proof_cleanly() {
    let huge = || buffer(encode::qword(0x7FFF_FFFF_FFFF_0000), &[]);
    let limits = Limits {
        memory_bytes: usize::MAX,
        ..Limits::BOOT_PROOF
    };
    // At load: the table is rolled back, and the rest of the proof runs.
    let mut m = Machine::new(qemu_dsdt_of(2, &[]));
    m.ssdts.push(table(b"SSDT", 2, &[name("BIGB", huge())]));
    let (found, out) = m.run_with(BootHost::new(still), limits, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("a definition block did not load"),
        "{out}"
    );
    assert!(out.contains("load failed at table 1 offset "), "{out}");
    assert!(out.contains("Limit(Memory); rolled back"), "{out}");
    assert!(found.named && found.heap_back, "{out}");
    // In the walk: the device fails with the memory limit, named.
    let dsdt = qemu_dsdt_of(
        2,
        &[scope(
            "\\_SB_",
            &[device("BIGD", &[method("_STA", 0, &[ret(huge())])])],
        )],
    );
    let (found, out) = Machine::new(dsdt).run_with(BootHost::new(still), limits, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("a device failed other than by a refusal"),
        "{out}"
    );
    assert!(
        out.contains("walk: \\_SB_.BIGD failed other than by a refusal: at table 0 offset "),
        "{out}"
    );
    assert!(out.contains(": Limit(Memory)"), "{out}");
    // The refused bytes were never held: the peak printed is what the interpreter held.
    assert!(found.peak < 1 << 20, "{out}");
}

#[test]
fn a_heap_that_did_not_come_back_fails_the_proof() {
    let mut readings = [Some(4096), Some(4160)].into_iter();
    let (found, out) = Machine::new(qemu_dsdt(&[])).run_with(
        BootHost::new(still),
        Limits::BOOT_PROOF,
        &mut || readings.next().flatten(),
    );
    assert!(!found.heap_back);
    assert_eq!(
        found.failed,
        Some("the kernel heap did not return to what it held before the proof"),
        "{out}"
    );
    assert!(
        last_line(&out).ends_with(
            "heap back false; aml failed: the kernel heap did not return to what it held before the proof"
        ),
        "{out}"
    );
    // No heap to read is not a heap that came back.
    let (found, _) = Machine::new(qemu_dsdt(&[])).run_with(
        BootHost::new(still),
        Limits::BOOT_PROOF,
        &mut || None,
    );
    assert!(!found.heap_back);
}

// --- B3-2: QEMU's tables only -----------------------------------------------------------------

/// Firmware that is not QEMU's: one line, its IDs escaped, no AML run, and a pass (ADR-0013 R6's
/// convention for an absent subject). The AML here would fail to load if it ran.
#[test]
fn tables_that_are_not_qemus_run_nothing_and_pass() {
    let dsdt = table(b"DSDT", 2, &[vec![0xFF, 0xFF, 0xFF]]);
    let (found, out) = Machine::new(dsdt).run();
    assert!(found.passed() && found.not_run, "{out}");
    assert_eq!(found.tables, 0);
    assert_eq!(found.seen, Seen::default(), "the host was never asked");
    assert_eq!(
        out,
        "origin-kernel: aml: the DSDT's OEM ID \"ORIGIN\" and OEM table ID \"AMLTEST \" are not QEMU's: the proof runs only on QEMU's own tables (ADR-0019 A3) and ran no AML; passed\n"
    );
    // The IDs come from firmware: anything but plain printable ASCII is escaped, so that they
    // cannot forge a line, a quote or a marker.
    let mut dsdt = table(b"DSDT", 2, &[]);
    dsdt[10..16].copy_from_slice(&[0x01, b'"', b'\\', b'A', 0xFF, b'\n']);
    dsdt[16..24].copy_from_slice(b"BXPC    ");
    dsdt[9] = 0;
    dsdt[9] = 0u8.wrapping_sub(dsdt.iter().fold(0u8, |s, &b| s.wrapping_add(b)));
    let (found, out) = Machine::new(dsdt).run();
    assert!(found.not_run && found.passed(), "{out}");
    assert!(
        out.contains("OEM ID \"\\x01\\x22\\x5cA\\xff\\x0a\" and OEM table ID \"BXPC    \""),
        "{out}"
    );
    assert_eq!(out.lines().count(), 1, "{out}");
}

/// B3-13: the IDs decide before any checksum. Other IDs on a DSDT whose checksum is wrong: the
/// proof does not run, and passes; the census, which checks every table, reports the checksum.
#[test]
fn other_ids_with_a_bad_checksum_are_not_run() {
    let mut dsdt = table(b"DSDT", 2, &[name("XTRA", one())]);
    dsdt[9] ^= 1;
    let (found, out) = Machine::new(dsdt).run();
    assert!(found.not_run && found.passed(), "{out}");
    assert!(out.contains("are not QEMU's"), "{out}");
}

/// QEMU's IDs on a DSDT whose header cannot be read past its first bytes: as before, the proof
/// cannot tell whose tables these are and fails (Q1 of B4's contract).
#[test]
fn a_dsdt_whose_header_cannot_be_copied_fails_the_proof() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.dsdt_at = LOW;
    let (found, out) = m.run();
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
    assert!(
        out.contains("origin-kernel: aml: the DSDT: not readable: its header cannot be copied"),
        "{out}"
    );
}

/// QEMU's IDs on a DSDT whose checksum is wrong: the IDs say the tables are QEMU's, so the whole
/// table must pass `origin-acpi-tables`' check before any of it runs, and here it does not: the
/// proof fails as for any refused DSDT (B3-13 keeps this; the review of B4, B4-7).
#[test]
fn qemus_ids_with_a_bad_checksum_fail_no_dsdt() {
    let mut dsdt = qemu_dsdt(&[]);
    dsdt[9] ^= 1;
    let (found, out) = Machine::new(dsdt).run();
    assert!(!found.not_run, "{out}");
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
}

// --- B3-1: the whole proof bounded, nothing run to classify, each table once --------------------

/// A device whose `_HID` is an alias of a method is not read: the method never runs (it would
/// store to `Debug`, which the recorder counts), and the device is not a processor.
#[test]
fn an_alias_hid_runs_nothing_and_is_not_a_processor() {
    let dsdt = qemu_dsdt(&[
        method(
            "\\SLOW",
            0,
            &[
                store(string("ran"), encode::debug()),
                ret(string("ACPI0007")),
            ],
        ),
        scope(
            "\\_SB_",
            &[
                device("ALS0", &[alias("\\SLOW", "_HID")]),
                device("ALS1", &[alias("\\SLOW", "_HID")]),
            ],
        ),
    ]);
    let (found, out) = Machine::new(dsdt).run();
    assert!(found.passed(), "{out}");
    assert_eq!(found.seen.reports, 0, "a `_HID` method ran: {out}");
    assert_eq!(found.walked.processors, 2, "{out}");
}

/// An XSDT that lists one SSDT twice: it is loaded once, and said so.
#[test]
fn an_ssdt_listed_twice_is_loaded_once() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.ssdts.push(table(b"SSDT", 1, &[name("XTRA", one())]));
    m.entries.push(SSDT);
    let (found, out) = m.run();
    assert!(found.passed(), "{out}");
    assert_eq!(found.tables, 2, "{out}");
    assert!(
        out.contains(
            "origin-kernel: aml: XSDT entry 2 names the table at 0x7f010000 again: loaded once"
        ),
        "{out}"
    );
}

/// A proof run past its whole bound fails with that reason: each SSDT's table-level code fits one
/// evaluation's steps, and together they pass [`WHOLE`] times them.
#[test]
fn a_proof_past_its_whole_bound_fails_with_that_reason() {
    let limits = Limits {
        steps: 20_000,
        ..Limits::BOOT_PROOF
    };
    let mut m = Machine::new(qemu_dsdt(&[]));
    for k in 0..8 {
        let counter = alloc::format!("CNT{k}");
        m.ssdts.push(table(
            b"SSDT",
            1,
            &[
                name(&counter, zero()),
                while_(
                    logic(0x95, nm(&counter), encode::int(300)),
                    &[increment(nm(&counter))],
                ),
            ],
        ));
    }
    let (found, out) = m.run_with(BootHost::new(still), limits, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("the proof was cut off by its bound"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: cut off by the proof's bound (3000000 us, 60000 steps) before an XSDT entry"),
        "{out}"
    );
    assert!(found.tables < 9, "{out}");
    assert!(
        !out.contains("load failed"),
        "each table fit one evaluation: {out}"
    );
}

// --- B3-14: each check of the proof's bound first, and its time half ------------------------------

/// The step limit of the bound tests: the whole proof's bound is [`WHOLE`] times it.
const L: u64 = 100_000;

fn bound_limits() -> Limits {
    Limits {
        steps: L,
        ..Limits::BOOT_PROOF
    }
}

/// An SSDT whose table-level code calls a method of its own that counts to `n` in a local, which
/// allocates nothing, so that its cost grows linearly with `n` (`k` names the method).
fn loading_ssdt(k: usize, n: u64) -> Vec<u8> {
    let method_name = alloc::format!("\\CNT{k}");
    table(
        b"SSDT",
        1,
        &[
            method(&method_name, 0, &counting(n)),
            call(&method_name, &[]),
        ],
    )
}

/// A method body that counts to `n` in `Local0`.
fn counting(n: u64) -> Vec<B> {
    vec![
        store(zero(), local(0)),
        while_(
            logic(0x95, local(0), encode::int(n)),
            &[increment(local(0))],
        ),
    ]
}

/// The steps one iteration of [`loading_ssdt`]'s loop and of [`counting`]'s costs, measured with
/// the interpreter itself (rounded up), so that the tests below put each cost where they mean to.
fn per_iteration() -> (u64, u64) {
    let fresh = || {
        Interpreter::new(
            BootHost::new(still),
            Config {
                limits: bound_limits(),
            },
            IntegerWidth::Bits32,
        )
        .unwrap()
    };
    let load = |i: &mut Interpreter<BootHost>, t: &[u8]| {
        let c = Checked::parse(t).unwrap();
        let h = c.table().header();
        i.load(DefinitionBlock {
            signature: h.signature.0,
            oem_id: h.oem_id,
            oem_table_id: h.oem_table_id,
            aml: c.aml(),
        })
        .unwrap();
    };
    let table_steps = |n| {
        let mut i = fresh();
        load(&mut i, &loading_ssdt(0, n));
        i.steps_done()
    };
    let method_steps = |n| {
        let mut i = fresh();
        load(
            &mut i,
            &table(b"SSDT", 1, &[method("CNTM", 0, &counting(n))]),
        );
        let before = i.steps_done();
        i.evaluate("\\CNTM", &[]).unwrap();
        i.steps_done() - before
    };
    (
        (table_steps(1000) - table_steps(0)).div_ceil(1000),
        (method_steps(1000) - method_steps(0)).div_ceil(1000),
    )
}

/// QEMU's tables with three SSDTs that each load at nine tenths of the step limit, so that the
/// loads stay within the whole bound (three times it) and half the limit more passes it.
fn three_loads(dsdt: Vec<u8>, per_table: u64) -> Machine {
    let mut m = Machine::new(dsdt);
    for k in 0..3 {
        m.ssdts.push(loading_ssdt(k, 9 * L / 10 / per_table));
    }
    m
}

/// The bound passed during `\_PIC`: the check before the next named result sees it first.
#[test]
fn the_bound_passed_in_a_named_result_is_seen_before_the_next() {
    let (per_table, per_method) = per_iteration();
    let dsdt = qemu_dsdt_with(
        1,
        &counting(L / 2 / per_method),
        name("_CRS", buffer(byte(18), &CRS)),
        &[],
    );
    let (found, out) =
        three_loads(dsdt, per_table)
            .run_with(BootHost::new(still), bound_limits(), &mut || Some(0));
    assert_eq!(found.failed, Some(CUT_OFF), "{out}");
    assert!(
        out.contains("\\_PIC (1) true; cut off by the proof's bound before \\PICF 1"),
        "{out}"
    );
    assert_eq!(found.tables, 4, "{out}");
}

/// The bound passed during the last named result (a `_CRS` method): the check before the walk
/// sees it first.
#[test]
fn the_bound_passed_in_the_last_named_result_is_seen_before_the_walk() {
    let (per_table, per_method) = per_iteration();
    let mut crs = counting(L / 2 / per_method);
    crs.push(ret(buffer(byte(18), &CRS)));
    let dsdt = qemu_dsdt_with(1, &[], method("_CRS", 0, &crs), &[]);
    let (found, out) =
        three_loads(dsdt, per_table)
            .run_with(BootHost::new(still), bound_limits(), &mut || Some(0));
    assert_eq!(found.failed, Some(CUT_OFF), "{out}");
    assert!(
        out.contains("\\_SB.PCI0._CRS a template true; all true"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: cut off by the proof's bound (3000000 us, 300000 steps) before the walk"),
        "{out}"
    );
}

/// The bound passed during the walk (a device's `_STA`, within the walk's own bound): the check
/// before the first classification sees it.
#[test]
fn the_bound_passed_in_the_walk_is_seen_before_a_classification() {
    let (per_table, per_method) = per_iteration();
    let mut sta = counting(L / 2 / per_method);
    sta.push(ret(byte(0x0F)));
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_",
        &[device("SLW0", &[method("_STA", 0, &sta)])],
    )]);
    let (found, out) =
        three_loads(dsdt, per_table)
            .run_with(BootHost::new(still), bound_limits(), &mut || Some(0));
    assert_eq!(found.failed, Some(CUT_OFF), "{out}");
    assert!(
        out.contains("origin-kernel: aml: cut off by the proof's bound (3000000 us, 300000 steps) before a classification"),
        "{out}"
    );
    assert!(!out.contains("before the walk"), "{out}");
}

/// The bound's time half: a firmware clock that moves four seconds at each reading passes it at
/// the FADT search's first check, which fails with the bound's own reason.
#[test]
fn the_bound_passed_in_the_fadt_search_fails_with_its_own_reason() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.tick = 4_000_000_000;
    let (found, out) = m.run();
    assert_eq!(found.failed, Some(CUT_OFF), "{out}");
    assert!(
        out.contains("origin-kernel: aml: cut off by the proof's bound (3000000 us, 30000000 steps) before the XSDT's search for a FADT ended"),
        "{out}"
    );
    assert!(!out.contains("no DSDT"), "{out}");
}

/// The bound's time half at a later check: a clock that moves 0.4 s a reading, the steps far from
/// their half.
#[test]
fn the_bounds_time_half_cuts_the_proof() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    m.tick = 400_000_000;
    let (found, out) = m.run();
    assert_eq!(found.failed, Some(CUT_OFF), "{out}");
    assert!(out.contains("cut off by the proof's bound"), "{out}");
}

// --- B3-5: each branch of the verdict, first ----------------------------------------------------

/// The recorder's own check: a host that answers a bridge's bus number, though every region access
/// is still refused, fails the proof with that reason first.
#[test]
fn a_host_that_answers_a_bridge_fails_the_proof_with_that_reason_first() {
    let host = Changed {
        grant_bridge: true,
        ..Changed::new()
    };
    let (found, out) = Machine::new(qemu_dsdt(&[below_a_bridge()])).run_with(
        host,
        Limits::BOOT_PROOF,
        &mut || Some(0),
    );
    assert_eq!(
        found.failed,
        Some("the host answered a request for hardware other than by refusing it"),
        "{out}"
    );
    assert_eq!((found.seen.answered, found.seen.bridges), (1, 0), "{out}");
}

/// A `DataTableRegion` in a device's `_STA` asks the host for a table.
#[test]
fn a_table_asked_for_fails_the_proof_with_that_reason_first() {
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_",
        &[device(
            "TBL0",
            &[method(
                "_STA",
                0,
                &[
                    encode::data_region("DTR0", string("OEM1"), string(""), string("")),
                    ret(byte(0x0F)),
                ],
            )],
        )],
    )]);
    let (found, out) = Machine::new(dsdt).run();
    assert_eq!(found.failed, Some("the AML asked for a table"), "{out}");
    assert_eq!(found.seen.tables, 1, "{out}");
}

/// Processors as required, and one link whose `_STA` answers without hardware.
#[test]
fn a_link_not_refused_fails_the_proof_with_that_reason_first() {
    let dsdt = qemu_dsdt(&[scope(
        "\\_SB_",
        &[device("LNKE", &[method("_STA", 0, &[ret(byte(0x0B))])])],
    )]);
    let (found, out) = Machine::new(dsdt).run();
    assert_eq!(
        found.failed,
        Some("not every LNKx _STA was refused in PCI_Config, or there was none"),
        "{out}"
    );
    assert_eq!((found.walked.links, found.walked.links_refused), (5, 4));
}

/// A limit too small for the interpreter's predefined objects.
#[test]
fn an_interpreter_that_cannot_be_made_fails_the_proof() {
    let limits = Limits {
        memory_bytes: 64,
        ..Limits::BOOT_PROOF
    };
    let (found, out) =
        Machine::new(qemu_dsdt(&[])).run_with(BootHost::new(still), limits, &mut || Some(0));
    assert_eq!(
        found.failed,
        Some("the interpreter could not be made"),
        "{out}"
    );
    assert!(
        out.contains("origin-kernel: aml: no interpreter: Limit(Memory)"),
        "{out}"
    );
}

/// The walk's outcomes not kept, as with a heap that has no room left.
#[test]
fn outcomes_that_cannot_be_kept_fail_the_proof() {
    fallible::REFUSE_KEEP.with(|r| r.set(true));
    let (found, out) = Machine::new(qemu_dsdt(&[])).run();
    fallible::REFUSE_KEEP.with(|r| r.set(false));
    assert_eq!(
        found.failed,
        Some("no memory to keep what the walk reached"),
        "{out}"
    );
}

/// The FADT's DSDT pointer names a table that is not a DSDT.
#[test]
fn a_dsdt_pointer_to_another_table_fails_the_proof() {
    // With QEMU's IDs, so that it is judged as QEMU's DSDT (B3-13).
    let (found, out) = Machine::new(qemu_ids(table(b"SSDT", 1, &[]))).run();
    assert_eq!(found.failed, Some("no DSDT"), "{out}");
    assert!(
        out.contains("origin-kernel: aml: the DSDT: not one"),
        "{out}"
    );
}

/// `_INI` refused is allowed and counted; `_INI` failing otherwise fails the proof.
#[test]
fn an_ini_refused_is_allowed_and_one_that_fails_otherwise_is_not() {
    let ini = |body: B| {
        qemu_dsdt(&[scope(
            "\\_SB_",
            &[device("INI0", &[method("_INI", 0, &[body])])],
        )])
    };
    let (found, out) = Machine::new(ini(store(nm("\\_SB_.CPEN"), encode::local(0)))).run();
    assert!(found.passed(), "{out}");
    assert_eq!(found.walked.ini_refused, 1, "{out}");
    let (found, out) = Machine::new(ini(encode::divide(
        one(),
        zero(),
        encode::none(),
        encode::none(),
    )))
    .run();
    assert_eq!(
        found.failed,
        Some("a device failed other than by a refusal"),
        "{out}"
    );
    assert!(
        out.contains("walk: \\_SB_.INI0 failed other than by a refusal: at table 0 offset "),
        "{out}"
    );
    assert!(out.contains(": DivideByZero"), "{out}");
}

// --- B3-7: the room above the guard page ---------------------------------------------------------

/// Where the caller gives the stack's floor, the room above it at the deepest host call is printed
/// beside the depth.
#[test]
fn the_room_above_the_guard_page_is_printed_where_the_stack_is_known() {
    let mut m = Machine::new(qemu_dsdt(&[]));
    let here = stack_mark();
    m.stack_floor = Some(here.saturating_sub(1 << 20));
    let (found, out) = m.run();
    let room = found.room.expect("a floor was given");
    assert!(room > 0 && room < 1 << 20, "{out}");
    assert!(
        out.contains(&alloc::format!(
            "bytes below the proof's entry and {room} bytes above the stack's guard page (sampled at host calls: the depth a lower bound, the room an upper one)"
        )),
        "{out}"
    );
}

#[test]
fn a_resource_template_ends_with_its_end_tag() {
    assert!(ends_with_end_tag(&CRS));
    assert!(ends_with_end_tag(&[0x79, 0x00]));
    // An IRQ descriptor (small item 04h, 2 bytes) and the end tag.
    assert!(ends_with_end_tag(&[0x22, 0x01, 0x00, 0x79, 0x00]));
    // No end tag; one with bytes after it; a large item longer than the buffer; a truncated
    // length; an end tag of the wrong length; nothing.
    assert!(!ends_with_end_tag(&CRS[..16]));
    assert!(!ends_with_end_tag(&[0x79, 0x00, 0x00]));
    assert!(!ends_with_end_tag(&[0x88, 0xFF, 0xFF, 0x79, 0x00]));
    assert!(!ends_with_end_tag(&[0x88, 0x0D]));
    assert!(!ends_with_end_tag(&[0x78, 0x79, 0x00]));
    assert!(!ends_with_end_tag(&[]));
}

/// The proxy for the interpreter's stack in the kernel (ruling Q3 (a) of step B3's contract): the
/// whole proof, over QEMU-shaped tables with the deepest field chain and a bridge, on a thread of
/// 32 KiB. A proxy on the host's architecture, in release mode; the depth it reports is how far
/// below the proof's entry the deepest host call was made.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "release only: a debug build's frames are several times a release build's"
)]
fn the_proof_runs_on_a_32_kib_stack() {
    let m = Machine::new(qemu_dsdt(&[deep_chain(), below_a_bridge()]));
    let handle = std::thread::Builder::new()
        .stack_size(32 << 10)
        .spawn(move || m.run())
        .unwrap();
    let (found, out) = handle.join().unwrap();
    assert!(found.passed(), "{out}");
    // The deep chain and the bridge were reached.
    assert_eq!(found.seen.refused[1], 3, "{out}");
    assert_eq!(found.seen.bridges, 1, "{out}");
    assert!(found.depth > 0 && found.depth < 32 << 10, "{out}");
    eprintln!(
        "the boot proof on a 32 KiB stack: passed; the deepest host call {} bytes below the proof's entry (a proxy: {}, release)",
        found.depth,
        std::env::consts::ARCH
    );
}

/// QEMU's own DSDTs, when a directory of them is given (`ORIGIN_AML_TABLES`, as for
/// `origin-aml`'s test of them: QEMU's output stays out of the repository), each run through the
/// whole proof with the boot's host. Run by hand; without the variable it does nothing.
#[test]
fn qemu_dsdts_pass_the_proof() {
    let Some(dir) = std::env::var_os("ORIGIN_AML_TABLES") else {
        eprintln!("qemu_dsdts_pass_the_proof: ORIGIN_AML_TABLES is not set; nothing checked");
        return;
    };
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let mut checked = 0;
    for path in entries {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if !bytes.starts_with(b"DSDT") {
            continue;
        }
        let (found, out) = Machine::new(bytes).run();
        assert!(found.passed(), "{}:\n{out}", path.display());
        eprintln!("{}:\n{out}", path.display());
        checked += 1;
    }
    assert!(checked > 0, "no DSDT in {dir:?}");
}
