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
    let mut body = vec![
        name("\\_S5_", package(4, &[zero(), zero(), zero(), zero()])),
        name("PICF", zero()),
        method("_PIC", 1, &[store(arg(0), nm("PICF"))]),
        scope(
            "\\_SB_",
            &[
                device(
                    "PCI0",
                    &[
                        name("_HID", dword(0x080A_D041)),
                        name("_ADR", zero()),
                        name("_CRS", buffer(byte(18), &CRS)),
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
    table(b"DSDT", revision, &body)
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
}

impl Machine {
    fn new(dsdt: Vec<u8>) -> Self {
        Self {
            dsdt,
            ssdts: Vec::new(),
            dsdt_at: DSDT,
            entries: Vec::new(),
        }
    }

    /// Lays the tables out and runs the proof with `host`, `limits` and `heap`.
    fn run_with<H: Host>(
        &self,
        host: H,
        limits: Limits,
        heap: &mut dyn FnMut() -> Option<u64>,
    ) -> (Findings, String) {
        let mut memory = vec![0u8; usize::try_from(SSDT - BASE).unwrap() + 4 * 0x8000];
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
        };
        let mut out = String::new();
        let found = run(&mut out, &mut fw, RSDP, host, limits, heap);
        (found, out)
    }

    /// The proof with the boot's host, limits and a heap that never moves.
    fn run(&self) -> (Findings, String) {
        self.run_with(BootHost::new(still), Limits::BOOT_PROOF, &mut || Some(4096))
    }
}

/// Firmware memory at [`BASE`].
struct Memory {
    bytes: &'static [u8],
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
        0
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
}

impl Changed {
    const fn new() -> Self {
        Self {
            boot: BootHost::new(still),
            grant_memory: false,
            fail_release: false,
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
            && out.contains("OEM \"ORIGIN\", \"AMLTEST \": loaded as table 0"),
        "{out}"
    );
    assert!(out.contains("origin-kernel: aml: SSDT rev 1, "), "{out}");
    assert!(out.contains(": loaded as table 1"), "{out}");
    assert!(
        out.contains(
            "origin-kernel: aml: named results: \\_S5_ true; \\PICF 0 true; \\_PIC (1) true; \\PICF 1 true; \\_SB.PCI0._HID PNP0A08 true; \\_SB.PCI0._CRS a template true; \\_OSI (\"Windows 2022\") Ones true; \\_OSI (\"Linux\") Zero true; \\_REV 2 true; all true"
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
    // The HPET's `_STA` read its register: the required refusal is missing.
    assert_eq!(found.walked.hpet, Some(false), "{out}");
    assert_eq!(
        found.failed,
        Some("the HPET's _STA was not refused in SystemMemory"),
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
    let dsdt = table(
        b"DSDT",
        1,
        &[
            name("\\_S5_", package(4, &[zero(), zero(), zero(), zero()])),
            name("PICF", zero()),
            method("_PIC", 1, &[store(arg(0), nm("PICF"))]),
        ],
    );
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
