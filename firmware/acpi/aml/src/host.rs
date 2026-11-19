//! What the interpreter asks of its host: the only way out of the library (ADR-0019 A3; the
//! contract's §2).
//!
//! **The host, not the library, enforces ADR-0019 A2.** The library forwards every operation
//! region access, `Load` from SystemMemory included, as a request; the host decides whether that
//! region may be touched at all: only what the firmware's operation regions name, never memory the
//! kernel or a program owns. A refusal comes back as [`HostError::Refused`] and ends the
//! evaluation with that error, reported with the space and address.
//!
//! **Two requests are of no region the firmware declared, and a host must decide them knowingly**
//! (the review of B2's Addendum 2, for step B3's host):
//!
//! - [`Host::bridge_bus`] reads a PCI bridge's secondary bus number (its configuration byte
//!   0x19), which the library needs to name the function of a PCI_Config region below the bridge.
//!   No `OperationRegion` names it: the host allows it by its kind, or answers from its own
//!   enumeration of the bus, or refuses it, and then every PCI_Config region below a bridge fails.
//! - A `DataTableRegion` is a SystemMemory region at the address the host's own [`Host::table`]
//!   answer gave: the host told the library where the table is, and is then asked to read there.
//!   It should grant exactly the table it named, and nothing beyond its length.
//!
//! **`Fatal` is a report.** [`Host::fatal`] tells the host what the firmware said; the library
//! then ends the evaluation with an error, and it is for the host to decide what to do. Nothing in
//! the library ends the host.
//!
//! Every call is synchronous and the interpreter runs one evaluation at a time (the contract's
//! §8). Every method of [`Host`] is called: the clock for the time limit and `Timer`; the region
//! accesses, `Sleep`, `Stall`, the Global Lock, `Notify`, `Fatal`, `Debug` and `BreakPoint` by
//! the operators that name them; [`Host::bridge_bus`] by the walk that names a PCI_Config
//! region's function; [`Host::table`] by `DataTableRegion` and `LoadTable`; [`Host::trace`] only
//! when [`Host::tracing`] says so.
//!
//! **No call waits past the time limit** (the review of B1, F9). A call that may wait is given
//! the time the evaluation has left, and the host must not wait longer:
//!
//! - [`Host::acquire_global_lock`]'s timeout is at most the time left, and never 0xFFFF ("for
//!   ever"): a wait for ever, or longer than the time left, is cut to the time left (and to
//!   65,534 ms), and a timeout of the cut wait ends the evaluation with [`crate::Limit::Time`];
//! - [`Host::sleep_ms`] and [`Host::stall_us`] are charged against the time left before they
//!   are called, and refused if it cannot hold them;
//! - [`Host::read`], [`Host::write`], [`Host::transfer`] and [`Host::bridge_bus`] carry
//!   `time_left_100ns`: a device that has not answered by then is a [`HostError::Timeout`]. With
//!   no time left, none is made.
//!
//! **The other calls return at once** (the review of B2, B2-12): [`Host::now_100ns`],
//! [`Host::release_global_lock`], [`Host::notify`], [`Host::fatal`], [`Host::debug`],
//! [`Host::breakpoint`], [`Host::trace`] and [`Host::table`] are given no time because they must
//! not wait: a host queues a notification, records a report or a trace, and stops in no debugger,
//! rather than block the evaluation. `table` copies a table the host already holds, from the XSDT
//! it read at boot: `LoadTable` loads the bytes, and `DataTableRegion` keeps only the address and
//! the length. **The library checks the copy before it uses either** (B4; the review of B2,
//! B2-14), with `origin-acpi-tables`: a header whose length is the copy's, a checksum, and the
//! key's signature and IDs. A copy that fails gives `LoadTable` Zero and `DataTableRegion` a
//! [`HostError::Failed`]. `LoadTable` never asks for a `DSDT` or an `SSDT`.
//!
//! **Tracing allocates nothing**, so that a host tracing the interpreter sees what one that does
//! not would have done: method paths are written into a fixed buffer of 32 segments, and a deeper
//! path is given as empty.

/// An operation region address space (ACPI 6.6 Table 5.221).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Space(pub u8);

impl Space {
    /// System memory.
    pub const SYSTEM_MEMORY: Self = Self(0);
    /// System I/O ports.
    pub const SYSTEM_IO: Self = Self(1);
    /// PCI configuration space.
    pub const PCI_CONFIG: Self = Self(2);
    /// The embedded controller.
    pub const EMBEDDED_CONTROL: Self = Self(3);
    /// SMBus.
    pub const SMBUS: Self = Self(4);
    /// The RTC's CMOS memory.
    pub const SYSTEM_CMOS: Self = Self(5);
    /// A PCI device's BAR.
    pub const PCI_BAR_TARGET: Self = Self(6);
    /// IPMI.
    pub const IPMI: Self = Self(7);
    /// GPIO.
    pub const GENERAL_PURPOSE_IO: Self = Self(8);
    /// A generic serial bus (I2C, SPI, UART).
    pub const GENERIC_SERIAL_BUS: Self = Self(9);
    /// The Platform Communications Channel.
    pub const PCC: Self = Self(0x0A);
    /// The Platform Runtime Mechanism.
    pub const PLATFORM_RT_MECHANISM: Self = Self(0x0B);
    /// Functional fixed hardware.
    pub const FFIXED_HW: Self = Self(0x7F);
}

/// A PCI function's address, for PCI_Config regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    /// The segment group (`_SEG`).
    pub segment: u16,
    /// The bus (`_BBN` and the path below it).
    pub bus: u8,
    /// The device (`_ADR` bits 31:16).
    pub device: u8,
    /// The function (`_ADR` bits 15:0).
    pub function: u8,
}

/// One access to a byte-addressed region space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionAccess<'a> {
    /// The space.
    pub space: Space,
    /// The region's base plus the byte offset accessed. For PCI_Config, the offset in the
    /// function's configuration space.
    pub address: u64,
    /// 8, 16, 32 or 64.
    pub width: u8,
    /// For PCI_Config, the function.
    pub pci: Option<PciAddress>,
    /// The region's path from the root, segment by segment, for diagnostics.
    pub region: &'a [[u8; 4]],
    /// The time the evaluation has left, in 100 ns units: the host must not wait longer.
    pub time_left_100ns: u64,
}

/// One transaction on a buffer-protocol region space (SMBus, IPMI, GenericSerialBus,
/// GeneralPurposeIO, PCC, PlatformRtMechanism, FFixedHW), or an EmbeddedControl field wider than
/// 8 bits (step B5; B5's contract, Q6): `address` is the region's base plus the field's first
/// byte, `data` the bytes it spans, at most 255, `bit_offset` and `bit_length` where the field
/// lies in them (its offset within the first byte, and its bits). A read fills `data`; a write
/// sends it, the bits outside the field already merged by the field's update rule (for Preserve,
/// by a read transfer first). A host for the ACPI service makes it one Embedded Controller
/// transaction, as ACPICA does (TASKS 64's ledger §3, G5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferAccess<'a> {
    /// The space.
    pub space: Space,
    /// The region's base plus the field's byte offset (a command, a register, a pin number).
    pub address: u64,
    /// `true` for a write.
    pub write: bool,
    /// The access attribute byte (`AttribQuick` …), as the field list gives it.
    pub attribute: u8,
    /// The access length, for the attributes that take one.
    pub length: u8,
    /// The bytes of the field's `Connection` resource, if it has one.
    pub connection: Option<&'a [u8]>,
    /// Where the field starts, in bits from its `Connection` (from the region's start if it has
    /// none). For GeneralPurposeIO, the index of the first of the connection's pins the field
    /// covers (§5.5.2.4.6.1).
    pub bit_offset: u64,
    /// The field's length in bits: for GeneralPurposeIO, how many pins.
    pub bit_length: u64,
    /// The region's path, for diagnostics.
    pub region: &'a [[u8; 4]],
    /// The time the evaluation has left, in 100 ns units: the host must not wait longer.
    pub time_left_100ns: u64,
}

/// A PCI bridge's secondary bus number: byte 0x19 of its configuration space (a type 1 header),
/// which the library needs to name the function of a PCI_Config region below the bridge. Its own
/// request, not a region access: no `OperationRegion` declares it, so a host that grants only
/// what the firmware's regions name (ADR-0019 A2) allows this by its kind, or answers from its own
/// enumeration of the bus (the review of B2, B2-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeBus {
    /// The bridge, as its parent bus and its `_ADR` name it.
    pub bridge: PciAddress,
    /// The time the evaluation has left, in 100 ns units: the host must not wait longer.
    pub time_left_100ns: u64,
}

/// What a host answers instead of doing a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    /// Not allowed: the region is not one the firmware names, or holds memory the kernel or a
    /// program owns (ADR-0019 A2).
    Refused,
    /// There is nothing there to access.
    Absent,
    /// The device did not answer in time.
    Timeout,
    /// The access was attempted and failed.
    Failed,
}

/// The XSDT entry `LoadTable` and `DataTableRegion` look for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableKey {
    /// The signature.
    pub signature: [u8; 4],
    /// The OEM ID, padded with NUL.
    pub oem_id: [u8; 6],
    /// The OEM table ID, padded with NUL.
    pub oem_table_id: [u8; 8],
}

/// What the interpreter tells a host that traces it (the contract's §2): each table's load,
/// each method entered and left, each region access with its outcome, each limit reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trace<'a> {
    /// A table starts to load.
    Load {
        /// Its signature.
        signature: [u8; 4],
        /// Its OEM table ID.
        oem_table_id: [u8; 8],
    },
    /// The table last started has loaded (its number), or failed.
    Loaded(Result<u32, crate::Error>),
    /// A method is entered: its path, or none if deeper than 32 segments.
    Enter(&'a [[u8; 4]]),
    /// The method last entered returns, or is left by an error.
    Leave,
    /// A region read, and what the host answered.
    Read(&'a RegionAccess<'a>, Result<u64, HostError>),
    /// A region write of the value, and what the host answered.
    Write(&'a RegionAccess<'a>, u64, Result<(), HostError>),
    /// A buffer-protocol transaction, and what the host answered.
    Transfer(&'a BufferAccess<'a>, Result<usize, HostError>),
    /// A bridge's secondary bus asked for, and what the host answered.
    BridgeBus(&'a BridgeBus, Result<u8, HostError>),
    /// A limit was reached: the load or evaluation ends with it.
    Limit(crate::Limit),
}

/// Something the interpreter tolerated, told to the host (TASKS item 63, step B5; the coordinating
/// session's rulings of 2026-11-19): firmware ACPI 6.6 calls wrong, accepted as TASKS 64's ledger
/// (`docs/research/2026-11-14-aml-gaps-acpica.md`) says deployed interpreters accept it. Never a
/// tolerance passed silently: each is one of these. One report is not a tolerance:
/// [`Report::Pieces`] marks a place where Origin knowingly differs from the deployed interpreter.
/// A place is a table and the offset in its term list. At most [`crate::REPORTS`] are given in one top-level evaluation (a load, an evaluation,
/// one `_STA`, `_INI` or `_REG` of a walk, or [`crate::Interpreter::bind_deferred`]), then one
/// [`Report::Suppressed`] with the count of the rest, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report<'a> {
    /// A statement of load-time code failed with `error` and was abandoned: skipped whole, from
    /// `statement` to `resumed`, where the load went on.
    Abandoned {
        /// What failed, and where.
        error: crate::Error,
        /// Where the statement began.
        statement: crate::At,
        /// Where the load went on: the statement's end.
        resumed: crate::At,
    },
    /// A block with the signature, OEM ID, OEM table ID and term list of loaded table `table`
    /// was refused, nothing of it created. The revision, OEM revision and creator are not
    /// compared: a boot load hands the interpreter no header (the review of B5, B5-R7).
    AlreadyLoaded {
        /// The table already loaded.
        table: u32,
    },
    /// A name in a package that load-time code made names nothing when the package's names were
    /// bound: the element is empty.
    UnboundElement {
        /// Where the name is.
        at: crate::At,
    },
    /// A package listed `found` elements for a count of `count`: those past the count were
    /// dropped.
    ExtraElements {
        /// Where the package is.
        at: crate::At,
        /// Its count.
        count: u64,
        /// The elements it listed.
        found: u64,
    },
    /// A string literal with no NUL before the end of the range around it, where it ends.
    Unterminated {
        /// Where the literal is.
        at: crate::At,
    },
    /// A string literal holds a byte above 0x7F (§20.2.3 allows 0x01 to 0x7F).
    HighBytes {
        /// Where the literal is.
        at: crate::At,
    },
    /// `Stall` of more than 100 µs (§19.6.129 asks for 100 at most), up to 255.
    LongStall {
        /// Where the `Stall` is.
        at: crate::At,
        /// Its microseconds.
        us: u8,
    },
    /// A mutex was released at an end other than its `Release`: the outermost evaluation's, or
    /// a failed run-time load's (the review of B5, B5-R9).
    Released {
        /// Its path, or empty for one deeper than 32 segments or named by no node: a mutex a
        /// method declared is gone with the method's names when the method returns.
        mutex: &'a [[u8; 4]],
        /// The path of the method that took it (the review of B5, B5-R8), or empty for load-time
        /// code, or for a method deeper than 32 segments or itself gone.
        method: &'a [[u8; 4]],
    },
    /// Data longer than a field unit was stored to it in `pieces` field-sized pieces, lowest
    /// first (ACPI 6.6 Table 19.7), where ACPICA writes one piece cut to the field. Not a
    /// tolerance: a store where Origin knowingly differs from the deployed interpreter (the
    /// coordinating session's ruling of 2026-11-19 on step B5's point 1). Told once, before the
    /// first piece.
    Pieces {
        /// The field's path, or empty for one deeper than 32 segments.
        field: &'a [[u8; 4]],
        /// How many pieces the store makes.
        pieces: u64,
    },
    /// Reports more in this evaluation, not given: how many of them were [`Report::Pieces`], and
    /// how many the rest, tolerances, so that a host can tell the two apart past the bound too
    /// (the review of B5's answers, B5-R14).
    Suppressed {
        /// Reports other than [`Report::Pieces`].
        tolerances: u64,
        /// [`Report::Pieces`].
        pieces: u64,
    },
}

/// The host. See the module's documentation for who enforces what.
pub trait Host {
    /// A monotonic clock in 100 ns units: `Timer`'s value and the time limit's clock.
    fn now_100ns(&mut self) -> u64;

    /// Reads `access.width` bits.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn read(&mut self, access: &RegionAccess<'_>) -> Result<u64, HostError>;

    /// Writes the low `access.width` bits of `value`.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn write(&mut self, access: &RegionAccess<'_>, value: u64) -> Result<(), HostError>;

    /// A PCI bridge's secondary bus number (see [`BridgeBus`]).
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn bridge_bus(&mut self, request: &BridgeBus) -> Result<u8, HostError>;

    /// Runs one buffer-protocol transaction: `data` holds the request and receives the answer.
    /// Returns how many bytes of `data` the answer fills.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn transfer(&mut self, access: &BufferAccess<'_>, data: &mut [u8]) -> Result<usize, HostError>;

    /// Yields for at least `ms` milliseconds (`Sleep`): no more than the time left, which was
    /// charged first.
    fn sleep_ms(&mut self, ms: u64);

    /// Waits at least `us` microseconds without yielding (`Stall`).
    fn stall_us(&mut self, us: u8);

    /// Takes the firmware-shared Global Lock (ACPI 6.6 §5.2.10.1), waiting up to `timeout_ms`:
    /// at most the time the evaluation has left, and never 0xFFFF (see the module's
    /// documentation). `Ok(false)` is a timeout.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn acquire_global_lock(&mut self, timeout_ms: u16) -> Result<bool, HostError>;

    /// Gives the Global Lock back.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn release_global_lock(&mut self) -> Result<(), HostError>;

    /// `Notify (node, value)`: `node` is the device, processor or thermal zone's path. Returns
    /// at once: the host queues it.
    fn notify(&mut self, node: &[[u8; 4]], value: u64);

    /// `Fatal (kind, code, arg)`: a report (see the module's documentation). Returns at once.
    fn fatal(&mut self, kind: u8, code: u32, arg: u64);

    /// A store to the `Debug` object, as text. Returns at once.
    fn debug(&mut self, text: &[u8]);

    /// `BreakPoint`. Does nothing unless the host has a debugger, and returns at once even then:
    /// the evaluation is never stopped in one.
    fn breakpoint(&mut self) {}

    /// Something tolerated (see [`Report`]). Required, with no default: every host decides what
    /// to do with each. Returns at once.
    fn report(&mut self, report: &Report<'_>);

    /// Whether this host traces the interpreter: [`Host::trace`] is called only if so.
    fn tracing(&self) -> bool {
        false
    }

    /// An event for a host that traces the interpreter. Returns at once.
    fn trace(&mut self, _event: &Trace<'_>) {}

    /// The table the XSDT lists under `key`: its physical address and a copy of its bytes, given
    /// to `out`, which the host calls at most once; or `false`, and `out` not called. Returns at
    /// once: the host already holds the table. A host that called `out` twice would leave the
    /// first copy charged and unreleased until the evaluation's recount (the review of B4,
    /// B4-11).
    ///
    /// The key's IDs are padded with NUL (G18, TASKS 64's ledger §3; step B5). A host finds the
    /// first table in root-table order whose signature is the key's and whose IDs match: an
    /// empty ID, all NUL, matches any table; any other is compared byte for byte, so a table whose
    /// ID is padded with spaces matches only a key that gives the spaces. The library checks the
    /// copy the same way.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn table(&mut self, key: &TableKey, out: &mut dyn FnMut(u64, &[u8]))
    -> Result<bool, HostError>;
}
