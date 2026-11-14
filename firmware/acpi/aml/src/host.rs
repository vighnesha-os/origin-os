//! What the interpreter asks of its host: the only way out of the library (ADR-0019 A3; the
//! contract's §2).
//!
//! **The host, not the library, enforces ADR-0019 A2.** The library forwards every operation
//! region access, `Load` from SystemMemory included, as a request; the host decides whether that
//! region may be touched at all: only what the firmware's operation regions name, never memory the
//! kernel or a program owns. A refusal comes back as [`HostError::Refused`] and ends the
//! evaluation with that error, reported with the space and address.
//!
//! **`Fatal` is a report.** [`Host::fatal`] tells the host what the firmware said; the library
//! then ends the evaluation with an error, and it is for the host to decide what to do. Nothing in
//! the library ends the host.
//!
//! Every call is synchronous and the interpreter runs one evaluation at a time (the contract's
//! §8). Every method of [`Host`] is called: the clock for the time limit and `Timer`; the region
//! accesses, `Sleep`, `Stall`, the Global Lock, `Notify`, `Fatal`, `Debug` and `BreakPoint` by
//! the operators that name them; [`Host::table`] by none yet (`Load` and `LoadTable` wait for
//! `origin-acpi-tables`); [`Host::trace`] only when [`Host::tracing`] says so.
//!
//! **No call waits past the time limit** (the review of B1, F9). A call that may wait is given
//! the time the evaluation has left, and the host must not wait longer:
//!
//! - [`Host::acquire_global_lock`]'s timeout is at most the time left, and never 0xFFFF ("for
//!   ever"): a wait for ever, or longer than the time left, is cut to the time left (and to
//!   65,534 ms), and a timeout of the cut wait ends the evaluation with [`crate::Limit::Time`];
//! - [`Host::sleep_ms`] and [`Host::stall_us`] are charged against the time left before they
//!   are called, and refused if it cannot hold them;
//! - [`Host::read`], [`Host::write`] and [`Host::transfer`] carry `time_left_100ns`: a device
//!   that has not answered by then is a [`HostError::Timeout`]. With no time left, none is made.
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
/// GeneralPurposeIO, PCC, PlatformRtMechanism, FFixedHW).
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

    /// `Notify (node, value)`: `node` is the device, processor or thermal zone's path.
    fn notify(&mut self, node: &[[u8; 4]], value: u64);

    /// `Fatal (kind, code, arg)`: a report (see the module's documentation).
    fn fatal(&mut self, kind: u8, code: u32, arg: u64);

    /// A store to the `Debug` object, as text.
    fn debug(&mut self, text: &[u8]);

    /// `BreakPoint`. Does nothing unless the host has a debugger.
    fn breakpoint(&mut self) {}

    /// Whether this host traces the interpreter: [`Host::trace`] is called only if so.
    fn tracing(&self) -> bool {
        false
    }

    /// An event for a host that traces the interpreter.
    fn trace(&mut self, _event: &Trace<'_>) {}

    /// The table the XSDT lists under `key`: its physical address and a copy of its bytes, or
    /// `None`.
    ///
    /// # Errors
    /// Whatever the host refuses or fails.
    fn table(&mut self, key: &TableKey, out: &mut dyn FnMut(u64, &[u8]))
    -> Result<bool, HostError>;
}
