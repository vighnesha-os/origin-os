//! `origin-abi`: the definitions the Framekernel and `origin.core` share (ADR-0005,
//! accepted 2026-11-07).
//!
//! - [`Error`] and [`OK`]: the stable status codes (P5). A system call returns `0` or a
//!   negative code in RAX; new codes are only ever appended.
//! - [`rights`], [`signals`], [`ObjectType`]: the capability model (P1–P3, P9).
//! - [`syscall`]: system-call numbers. They are **private to the release** (P4, ONBF §6.1):
//!   only `origin.core`, built in the same unified release, uses them.
//! - [`limits`]: message and wait limits (P8, P9).
//! - [`start`]: the start record every process begins with (P6), encoded and decoded field
//!   by field (ONBF-R7).
//!
//! `no_std`, no allocation, no `unsafe`, no panics.
#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    deny(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::cast_possible_truncation
    )
)]

/// The userspace ABI version (`[compatibility] user-abi`, P12).
pub const ABI_VERSION: (u16, u16) = (
    origin_version::USER_ABI_MAJOR,
    origin_version::USER_ABI_MINOR,
);

/// A handle: a 32-bit name for a kernel object in one process's handle table (P1).
pub type Handle = u32;
/// Never a valid handle.
pub const INVALID_HANDLE: Handle = 0;

/// The success status.
pub const OK: i64 = 0;

/// Every failure a system call can report (P5). The values are part of the C ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    /// An argument is out of range or inconsistent.
    InvalidArgs = -1,
    /// The handle is not valid in this process (never issued, closed, or stale).
    BadHandle = -2,
    /// The handle names an object of another type.
    WrongType = -3,
    /// The handle lacks a right the operation needs, or the caller may not do this.
    AccessDenied = -4,
    /// The kernel is out of memory for this request.
    NoMemory = -5,
    /// A user pointer is not valid, mapped or writable as needed.
    Fault = -6,
    /// The operation cannot proceed now (empty or full queue); wait and retry.
    ShouldWait = -7,
    /// The deadline passed.
    TimedOut = -8,
    /// The other end of a channel is closed.
    PeerClosed = -9,
    /// A wait was cancelled.
    Canceled = -10,
    /// Not supported by this kernel.
    NotSupported = -11,
    /// A quota or table limit was reached.
    Limit = -12,
    /// The caller's buffer is too small; the sizes it needs were written to the out pointer.
    BufferTooSmall = -13,
}

impl Error {
    /// Every error, in value order.
    pub const ALL: [Self; 13] = [
        Self::InvalidArgs,
        Self::BadHandle,
        Self::WrongType,
        Self::AccessDenied,
        Self::NoMemory,
        Self::Fault,
        Self::ShouldWait,
        Self::TimedOut,
        Self::PeerClosed,
        Self::Canceled,
        Self::NotSupported,
        Self::Limit,
        Self::BufferTooSmall,
    ];

    /// The raw status value.
    pub const fn raw(self) -> i64 {
        self as i64
    }

    /// The error for a raw status, or `None` for `OK` and unknown values.
    pub fn from_raw(raw: i64) -> Option<Self> {
        Self::ALL.iter().copied().find(|e| e.raw() == raw)
    }

    /// This status's name in the C ABI (P5).
    pub const fn name(self) -> &'static str {
        match self {
            Self::InvalidArgs => "INVALID_ARGS",
            Self::BadHandle => "BAD_HANDLE",
            Self::WrongType => "WRONG_TYPE",
            Self::AccessDenied => "ACCESS_DENIED",
            Self::NoMemory => "NO_MEMORY",
            Self::Fault => "FAULT",
            Self::ShouldWait => "SHOULD_WAIT",
            Self::TimedOut => "TIMED_OUT",
            Self::PeerClosed => "PEER_CLOSED",
            Self::Canceled => "CANCELED",
            Self::NotSupported => "NOT_SUPPORTED",
            Self::Limit => "LIMIT",
            Self::BufferTooSmall => "BUFFER_TOO_SMALL",
        }
    }
}

/// The name of a raw status: `OK`, an [`Error`]'s name, or `UNKNOWN` for anything else.
pub fn status_name(raw: i64) -> &'static str {
    if raw == OK {
        "OK"
    } else {
        match Error::from_raw(raw) {
            Some(e) => e.name(),
            None => "UNKNOWN",
        }
    }
}

/// Decodes a boolean that crossed the boundary as a byte (P4, CLAUDE.md §10): 0 is false,
/// 1 is true, and every other value is refused, never coerced.
pub const fn decode_bool(raw: u8) -> Option<bool> {
    match raw {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Handle rights (P2). A right can be removed, never added.
pub mod rights {
    /// Read data or messages.
    pub const READ: u32 = 1 << 0;
    /// Write data or messages.
    pub const WRITE: u32 = 1 << 1;
    /// Map the object into an address space.
    pub const MAP: u32 = 1 << 2;
    /// Map it executable (still subject to the Code Integrity Table).
    pub const EXECUTE: u32 = 1 << 3;
    /// Duplicate the handle (with equal or fewer rights).
    pub const DUPLICATE: u32 = 1 << 4;
    /// Send the handle through a channel.
    pub const TRANSFER: u32 = 1 << 5;
    /// Wait on the object's signals.
    pub const WAIT: u32 = 1 << 6;
    /// Raise or clear the object's user signals.
    pub const SIGNAL: u32 = 1 << 7;
    /// Control the object (terminate a process, change a thread).
    pub const MANAGE: u32 = 1 << 8;
    /// Every right defined in this version.
    pub const ALL: u32 = (1 << 9) - 1;
    /// The rights of a new channel end: everything that applies to a channel (P2, P8,
    /// amendment A1). `MAP`, `EXECUTE`, `SIGNAL` and `MANAGE` do not apply.
    pub const CHANNEL: u32 = READ | WRITE | DUPLICATE | TRANSFER | WAIT;
}

/// Object signals (P9).
pub mod signals {
    /// A message or data can be read.
    pub const READABLE: u32 = 1 << 0;
    /// A message or data can be written.
    pub const WRITABLE: u32 = 1 << 1;
    /// The other end of a channel is closed.
    pub const PEER_CLOSED: u32 = 1 << 2;
    /// An event's user signal.
    pub const SIGNALED: u32 = 1 << 3;
    /// A process or thread has ended.
    pub const TERMINATED: u32 = 1 << 4;
    /// Every signal defined in this version.
    pub const ALL: u32 = (1 << 5) - 1;
}

/// Kernel object types (P3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ObjectType {
    /// A process.
    Process = 1,
    /// A thread.
    Thread = 2,
    /// A memory object.
    MemoryObject = 3,
    /// One end of a channel.
    Channel = 4,
    /// An event.
    Event = 5,
    /// A completion port.
    Port = 6,
    /// A timer.
    Timer = 7,
}

/// System-call numbers (P4): private to this release. Arguments go in RDI, RSI, RDX, R10,
/// R8, R9; RAX returns [`OK`] or an [`Error`]; further results go through out pointers.
pub mod syscall {
    /// `exit(code)`: ends the calling thread.
    pub const THREAD_EXIT: u64 = 0;
    /// `report(value)`: developer builds only, for boot proofs.
    pub const DEBUG_REPORT: u64 = 1;
    /// `write(buffer, length)`: developer builds only; one console line.
    pub const DEBUG_WRITE: u64 = 2;
    /// `close(handle)`.
    pub const HANDLE_CLOSE: u64 = 3;
    /// `duplicate(handle, rights, out_handle)`.
    pub const HANDLE_DUPLICATE: u64 = 4;
    /// `channel_create(out_handles)`: writes two handles.
    pub const CHANNEL_CREATE: u64 = 5;
    /// `channel_write(handle, bytes, length, handles, count)`.
    pub const CHANNEL_WRITE: u64 = 6;
    /// `channel_read(handle, bytes, capacity, handles, capacity, out_sizes)`.
    pub const CHANNEL_READ: u64 = 7;
    /// `wait_one(handle, signals, deadline, out_observed)`.
    pub const WAIT_ONE: u64 = 8;
    /// `event_create(out_handle)`.
    pub const EVENT_CREATE: u64 = 9;
    /// `object_signal(handle, clear, set)`: user signals ([`super::signals::SIGNALED`]).
    pub const OBJECT_SIGNAL: u64 = 10;
}

/// Limits (P8, P9).
pub mod limits {
    /// Most bytes in one channel message.
    pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
    /// Most handles in one channel message.
    pub const MAX_MESSAGE_HANDLES: usize = 64;
    /// A deadline that never passes.
    pub const DEADLINE_INFINITE: u64 = u64::MAX;
}

/// The structure `channel_read`'s `out_sizes` points at (P4): it begins with its own
/// `u32 size`, which the caller sets, so a later minor version can append fields.
///
/// ```text
/// 0  u32 size      set by the caller: bytes of the structure it knows
/// 4  u32 bytes     the message's byte count
/// 8  u32 handles   the message's handle count
/// ```
///
/// The kernel refuses a `size` below [`read_sizes::SIZE`], and never reads or writes past the
/// fields it knows. Both counts are also written when the buffers are too small
/// ([`Error::BufferTooSmall`]).
pub mod read_sizes {
    /// Bytes of this version of the structure.
    pub const SIZE: u32 = 12;
    /// Byte offset of `size`.
    pub const SIZE_AT: usize = 0;
    /// Byte offset of `bytes`.
    pub const BYTES_AT: usize = 4;
    /// Byte offset of `handles`.
    pub const HANDLES_AT: usize = 8;
}

/// The start record (P6): what a process begins with, at the address RDI holds.
///
/// ```text
/// 0  u32 size            total bytes of the record
/// 4  u16 abi_major       the kernel's userspace ABI
/// 6  u16 abi_minor
/// 8  u32 handle_count
/// 12 u32 reserved        zero
/// 16 handle_count × { u32 handle, u32 role }
/// ```
pub mod start {
    use crate::{ABI_VERSION, Handle};

    /// Size of the fixed header.
    pub const HEADER: usize = 16;
    /// Size of one handle entry.
    pub const ENTRY: usize = 8;
    /// Most handles a start record carries.
    pub const MAX_HANDLES: usize = 32;

    /// What a start-record handle is for.
    pub mod role {
        /// The channel to whoever started the process.
        pub const BOOTSTRAP: u32 = 1;
        /// The process itself.
        pub const SELF_PROCESS: u32 = 2;
        /// The first thread.
        pub const SELF_THREAD: u32 = 3;
        /// A service the creator granted.
        pub const SERVICE: u32 = 4;
    }

    /// Why a start record could not be encoded or decoded.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum StartError {
        /// The buffer is too small.
        Short,
        /// More than [`MAX_HANDLES`] handles.
        TooManyHandles,
        /// `size` disagrees with the handle count, or `reserved` is not zero.
        Malformed,
    }

    fn put(out: &mut [u8], at: usize, bytes: &[u8]) -> Result<(), StartError> {
        let end = at.checked_add(bytes.len()).ok_or(StartError::Short)?;
        out.get_mut(at..end)
            .ok_or(StartError::Short)?
            .copy_from_slice(bytes);
        Ok(())
    }

    /// Byte offset of entry `i`, counted from `base`.
    fn entry_at(base: usize, i: usize) -> Option<usize> {
        ENTRY.checked_mul(i)?.checked_add(base)
    }

    fn get<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N], StartError> {
        let end = at.checked_add(N).ok_or(StartError::Short)?;
        let mut v = [0u8; N];
        v.copy_from_slice(b.get(at..end).ok_or(StartError::Short)?);
        Ok(v)
    }

    /// Writes a start record for `handles` (`(handle, role)`) into `out`; returns its size.
    pub fn encode(handles: &[(Handle, u32)], out: &mut [u8]) -> Result<usize, StartError> {
        if handles.len() > MAX_HANDLES {
            return Err(StartError::TooManyHandles);
        }
        let size = entry_at(HEADER, handles.len()).ok_or(StartError::Short)?;
        let size32 = u32::try_from(size).map_err(|_| StartError::Short)?;
        let count = u32::try_from(handles.len()).map_err(|_| StartError::TooManyHandles)?;
        put(out, 0, &size32.to_le_bytes())?;
        put(out, 4, &ABI_VERSION.0.to_le_bytes())?;
        put(out, 6, &ABI_VERSION.1.to_le_bytes())?;
        put(out, 8, &count.to_le_bytes())?;
        put(out, 12, &0u32.to_le_bytes())?;
        for (i, (handle, role)) in handles.iter().enumerate() {
            let at = entry_at(HEADER, i).ok_or(StartError::Short)?;
            put(out, at, &handle.to_le_bytes())?;
            put(
                out,
                at.checked_add(4).ok_or(StartError::Short)?,
                &role.to_le_bytes(),
            )?;
        }
        Ok(size)
    }

    /// A decoded start record.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct StartRecord<'a> {
        /// The kernel's userspace ABI version.
        pub abi: (u16, u16),
        entries: &'a [u8],
        count: usize,
    }

    impl StartRecord<'_> {
        /// The `(handle, role)` entries.
        pub fn handles(&self) -> impl Iterator<Item = (Handle, u32)> + '_ {
            (0..self.count).filter_map(|i| {
                let at = entry_at(0, i)?;
                Some((
                    u32::from_le_bytes(get(self.entries, at).ok()?),
                    u32::from_le_bytes(get(self.entries, at.checked_add(4)?).ok()?),
                ))
            })
        }

        /// The first handle with `role`, if any.
        pub fn find(&self, role: u32) -> Option<Handle> {
            self.handles().find(|&(_, r)| r == role).map(|(h, _)| h)
        }
    }

    /// Decodes and checks a start record.
    pub fn decode(b: &[u8]) -> Result<StartRecord<'_>, StartError> {
        let size = u32::from_le_bytes(get(b, 0)?) as usize;
        let abi = (
            u16::from_le_bytes(get(b, 4)?),
            u16::from_le_bytes(get(b, 6)?),
        );
        let count = u32::from_le_bytes(get(b, 8)?) as usize;
        let reserved = u32::from_le_bytes(get(b, 12)?);
        if count > MAX_HANDLES {
            return Err(StartError::TooManyHandles);
        }
        if reserved != 0 || Some(size) != entry_at(HEADER, count) {
            return Err(StartError::Malformed);
        }
        let entries = b.get(HEADER..size).ok_or(StartError::Short)?;
        Ok(StartRecord {
            abi,
            entries,
            count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_values_are_stable_and_distinct() {
        // Pinned: these values are the C ABI (P5); changing one breaks every program.
        let expected: [(Error, i64); 13] = [
            (Error::InvalidArgs, -1),
            (Error::BadHandle, -2),
            (Error::WrongType, -3),
            (Error::AccessDenied, -4),
            (Error::NoMemory, -5),
            (Error::Fault, -6),
            (Error::ShouldWait, -7),
            (Error::TimedOut, -8),
            (Error::PeerClosed, -9),
            (Error::Canceled, -10),
            (Error::NotSupported, -11),
            (Error::Limit, -12),
            (Error::BufferTooSmall, -13),
        ];
        for (e, v) in expected {
            assert_eq!(e.raw(), v);
            assert_eq!(Error::from_raw(v), Some(e));
        }
        assert_eq!(Error::from_raw(OK), None);
        assert_eq!(Error::from_raw(-14), None);
        assert_eq!(Error::ALL.len(), expected.len());
    }

    #[test]
    fn every_status_has_a_distinct_name() {
        assert_eq!(status_name(OK), "OK");
        assert_eq!(status_name(-14), "UNKNOWN");
        assert_eq!(status_name(1), "UNKNOWN");
        let mut names: Vec<&str> = Error::ALL.iter().map(|e| e.name()).collect();
        for e in Error::ALL {
            assert_eq!(status_name(e.raw()), e.name());
        }
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Error::ALL.len());
    }

    #[test]
    fn a_boundary_boolean_is_0_or_1_and_nothing_else() {
        for raw in 0..=u8::MAX {
            let want = match raw {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            };
            assert_eq!(decode_bool(raw), want, "byte {raw:#x}");
        }
    }

    #[test]
    fn rights_signals_and_numbers_do_not_overlap() {
        let r = [
            rights::READ,
            rights::WRITE,
            rights::MAP,
            rights::EXECUTE,
            rights::DUPLICATE,
            rights::TRANSFER,
            rights::WAIT,
            rights::SIGNAL,
            rights::MANAGE,
        ];
        assert_eq!(r.iter().fold(0, |a, b| a | b), rights::ALL);
        assert_eq!(r.iter().map(|b| b.count_ones()).sum::<u32>(), 9);
        let s = [
            signals::READABLE,
            signals::WRITABLE,
            signals::PEER_CLOSED,
            signals::SIGNALED,
            signals::TERMINATED,
        ];
        assert_eq!(s.iter().fold(0, |a, b| a | b).count_ones(), 5);
        let n = [
            syscall::THREAD_EXIT,
            syscall::DEBUG_REPORT,
            syscall::DEBUG_WRITE,
            syscall::HANDLE_CLOSE,
            syscall::HANDLE_DUPLICATE,
            syscall::CHANNEL_CREATE,
            syscall::CHANNEL_WRITE,
            syscall::CHANNEL_READ,
            syscall::WAIT_ONE,
            syscall::EVENT_CREATE,
            syscall::OBJECT_SIGNAL,
        ];
        let mut sorted = n.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), n.len());
        assert_eq!(limits::MAX_MESSAGE_BYTES, 65536);
        assert_eq!(limits::MAX_MESSAGE_HANDLES, 64);
    }

    #[test]
    fn channel_rights_and_the_sizes_structure_are_what_the_abi_says() {
        // No right a channel cannot use, and every one it can (P2, P8).
        assert_eq!(
            rights::CHANNEL,
            rights::READ | rights::WRITE | rights::DUPLICATE | rights::TRANSFER | rights::WAIT
        );
        assert_eq!(rights::CHANNEL & !rights::ALL, 0);
        assert_eq!(signals::ALL.count_ones(), 5);
        // `size` first (P4); the fields follow without overlap.
        assert_eq!(read_sizes::SIZE_AT, 0);
        assert_eq!(read_sizes::BYTES_AT, 4);
        assert_eq!(read_sizes::HANDLES_AT, 8);
        assert_eq!(read_sizes::SIZE, 12);
    }

    #[test]
    fn start_records_round_trip_and_are_checked() {
        let handles = [
            (0x0001_0001, start::role::BOOTSTRAP),
            (0x0001_0002, start::role::SERVICE),
        ];
        let mut buf = [0u8; 64];
        let size = start::encode(&handles, &mut buf).unwrap();
        assert_eq!(size, 32);
        let r = start::decode(&buf[..size]).unwrap();
        assert_eq!(r.abi, ABI_VERSION);
        assert_eq!(r.handles().collect::<Vec<_>>(), handles);
        assert_eq!(r.find(start::role::BOOTSTRAP), Some(0x0001_0001));
        assert_eq!(r.find(start::role::SELF_THREAD), None);
        // The first handle is at byte 16, where a program reads it.
        assert_eq!(&buf[16..20], &0x0001_0001u32.to_le_bytes());
        // Malformed records are refused.
        let mut bad = buf;
        bad[12] = 1;
        assert_eq!(
            start::decode(&bad[..size]),
            Err(start::StartError::Malformed)
        );
        let mut bad = buf;
        bad[0] = 31;
        assert_eq!(
            start::decode(&bad[..size]),
            Err(start::StartError::Malformed)
        );
        assert_eq!(start::decode(&buf[..20]), Err(start::StartError::Short));
        let many = [(1u32, 1u32); start::MAX_HANDLES + 1];
        assert_eq!(
            start::encode(&many, &mut [0u8; 512]),
            Err(start::StartError::TooManyHandles)
        );
        assert_eq!(
            start::encode(&handles, &mut [0u8; 20]),
            Err(start::StartError::Short)
        );
    }
}
