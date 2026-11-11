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
    /// Nothing by that name or key is here: a service directory holds no provider of the
    /// interface and major asked for (amendment C11).
    NotFound = -14,
}

impl Error {
    /// Every error, in value order.
    pub const ALL: [Self; 14] = [
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
        Self::NotFound,
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
            Self::NotFound => "NOT_FOUND",
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
    /// The rights of a process's handle to itself (amendment C6). Changing another process's
    /// address space needs `WRITE`; `MANAGE` is for termination and thread control. `MAP`,
    /// `EXECUTE` and `SIGNAL` do not apply.
    pub const PROCESS: u32 = READ | WRITE | MANAGE | WAIT | DUPLICATE | TRANSFER;
    /// The rights of a thread's handle to itself (amendment C6).
    pub const THREAD: u32 = WAIT | MANAGE | DUPLICATE | TRANSFER;
    /// The rights of a new memory object (amendment C6): never `EXECUTE`, since an anonymous
    /// object is never executable; `WAIT`, `SIGNAL` and `MANAGE` do not apply.
    pub const MEMORY_OBJECT: u32 = READ | WRITE | MAP | DUPLICATE | TRANSFER;
    /// The rights of a new event (amendments C6 and F6).
    pub const EVENT: u32 = WAIT | SIGNAL | DUPLICATE | TRANSFER;
    /// The rights of a new port (amendments C6 and F6): no `WAIT`, since a port raises no
    /// signals (F8).
    pub const PORT: u32 = READ | WRITE | DUPLICATE | TRANSFER;
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
    /// `channel_read(handle, bytes, capacity, records, capacity, out_sizes)`: each handle
    /// received is a [`super::received_handle`] record (B3).
    pub const CHANNEL_READ: u64 = 7;
    /// `wait_one(handle, signals, deadline, out_observed)`.
    pub const WAIT_ONE: u64 = 8;
    /// `event_create(out_event)` (amendment F17).
    pub const EVENT_CREATE: u64 = 9;
    /// `object_signal(handle, clear, set)`: user signals ([`super::signals::SIGNALED`]).
    pub const OBJECT_SIGNAL: u64 = 10;
    /// `memory_object_create(request, out_object)`: a [`super::object_request`].
    pub const MEMORY_OBJECT_CREATE: u64 = 11;
    /// `memory_map(process, object, request, out_address)`: a [`super::map_request`].
    pub const MEMORY_MAP: u64 = 12;
    /// `memory_unmap(process, address, length)`: one whole mapping.
    pub const MEMORY_UNMAP: u64 = 13;
    /// `memory_protect(process, address, length, access)`: one whole mapping, its access only
    /// reduced ([`super::map_access`]).
    pub const MEMORY_PROTECT: u64 = 14;
    /// `port_create(out_port)` (amendment F17).
    pub const PORT_CREATE: u64 = 15;
    /// `object_wait_async(port, object, key, signals)` (amendment F2).
    pub const OBJECT_WAIT_ASYNC: u64 = 16;
    /// `port_wait(port, deadline, out_packet)`: a [`super::port_packet`] (amendment F3).
    pub const PORT_WAIT: u64 = 17;
    /// `port_cancel(port, object, key)` (amendment F4).
    pub const PORT_CANCEL: u64 = 18;
    /// `clock_read(out_nanos)`: the monotonic clock (amendment F10).
    pub const CLOCK_READ: u64 = 19;
}

/// Bytes in a page: memory objects, mappings, offsets and fixed addresses are whole pages of
/// this size (P7).
pub const PAGE_SIZE: u64 = 4096;

/// What a mapping allows (P7, amendments C7 and D2): `READ` always, and at most one of `WRITE` and
/// `EXECUTE`; nothing is executable until the Code Integrity Table exists.
pub mod map_access {
    /// Read.
    pub const READ: u32 = 1 << 0;
    /// Write.
    pub const WRITE: u32 = 1 << 1;
    /// Execute (refused in this version).
    pub const EXECUTE: u32 = 1 << 2;
    /// Every bit defined in this version.
    pub const ALL: u32 = READ | WRITE | EXECUTE;
}

/// How a mapping is placed (P7, amendments C7 and D2).
pub mod map_flags {
    /// At the request's address, whose pages must all be free: a mapping is never replaced.
    pub const FIXED: u32 = 1 << 0;
    /// Every bit defined in this version.
    pub const ALL: u32 = FIXED;
}

/// The structure `memory_object_create` reads (amendments C7 and D1), which begins with its own
/// `u32 size`, set by the caller to a version it knows (P4).
///
/// ```text
/// 0  u32 size     16
/// 4  u32          zero
/// 8  u64 length   bytes of the object: a non-zero multiple of PAGE_SIZE
/// ```
pub mod object_request {
    /// Bytes of this version of the structure.
    pub const SIZE: u32 = 16;
    /// Byte offset of `size`.
    pub const SIZE_AT: usize = 0;
    /// Byte offset of `length`.
    pub const LENGTH_AT: usize = 8;
}

/// The structure `memory_map` reads (amendments C7, C8 and D2), which begins with its own
/// `u32 size` (P4).
///
/// ```text
/// 0  u32 size      40
/// 4  u32 access    map_access bits
/// 8  u32 flags     map_flags bits
/// 12 u32           zero
/// 16 u64 offset    where the mapping starts in the object: a multiple of PAGE_SIZE
/// 24 u64 length    bytes mapped: a non-zero multiple of PAGE_SIZE
/// 32 u64 address   with FIXED, where it goes; otherwise zero
/// ```
pub mod map_request {
    /// Bytes of this version of the structure.
    pub const SIZE: u32 = 40;
    /// Byte offset of `size`.
    pub const SIZE_AT: usize = 0;
    /// Byte offset of `access`.
    pub const ACCESS_AT: usize = 4;
    /// Byte offset of `flags`.
    pub const FLAGS_AT: usize = 8;
    /// Byte offset of `offset`.
    pub const OFFSET_AT: usize = 16;
    /// Byte offset of `length`.
    pub const LENGTH_AT: usize = 24;
    /// Byte offset of `address`.
    pub const ADDRESS_AT: usize = 32;
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
/// 0  u32 size              set by the caller: bytes of the structure it knows
/// 4  u32 bytes             the message's byte count
/// 8  u32 handles           the message's handle count
/// 12 u32                   zero (from minor 2)
/// 16 u64 sender_process    from minor 2: the writer's process ID (B1, C1)
/// 24 u64 sender_principal  from minor 2: the writer's principal
/// ```
///
/// The kernel refuses a `size` below [`read_sizes::BASE_SIZE`], and writes only what fits in
/// the caller's `size`: a caller that gives 12 gets the counts, nothing past them, and no
/// error (C2). The counts and the writer are also written when the buffers are too small
/// ([`Error::BufferTooSmall`]). The writer is stamped by the kernel when the message is
/// written, from the writing process; nothing the writer passes reaches it.
pub mod read_sizes {
    /// Bytes of this version of the structure, with the writer's identity.
    pub const SIZE: u32 = 32;
    /// Bytes of the first version: the counts alone.
    pub const BASE_SIZE: u32 = 12;
    /// Byte offset of `size`.
    pub const SIZE_AT: usize = 0;
    /// Byte offset of `bytes`.
    pub const BYTES_AT: usize = 4;
    /// Byte offset of `handles`.
    pub const HANDLES_AT: usize = 8;
    /// Byte offset of `sender_process`.
    pub const SENDER_PROCESS_AT: usize = 16;
    /// Byte offset of `sender_principal`.
    pub const SENDER_PRINCIPAL_AT: usize = 24;
}

/// What `channel_read` writes for each handle it moves into the caller's table (B3): the
/// handle, the type of the object it names and its rights there, so a receiver can check both
/// before it uses the handle. Each record is 12 bytes, every field a little-endian `u32`, and
/// the capacity passed is in records.
///
/// ```text
/// 0  u32 handle        its value in the caller's table
/// 4  u32 object_type   an [`ObjectType`]
/// 8  u32 rights        [`rights`]
/// ```
pub mod received_handle {
    /// Bytes of one record.
    pub const SIZE: usize = 12;
    /// Byte offset of `handle`.
    pub const HANDLE_AT: usize = 0;
    /// Byte offset of `object_type`.
    pub const OBJECT_TYPE_AT: usize = 4;
    /// Byte offset of `rights`.
    pub const RIGHTS_AT: usize = 8;
}

/// What `port_wait` writes (C9; amendment F1): one packet, 32 bytes, every field
/// little-endian, with no padding and no `size` (an exception to P4 for this record only).
///
/// ```text
/// 0  u64 key        what the binder gave, unchanged
/// 8  u32 kind       a packet_kind: SIGNAL or CANCELED, never 0
/// 12 u32 trigger    the mask the binding asked
/// 16 u32 observed   the object's signals when the packet was queued
/// 20 u32            zero
/// 24 u64 data       zero in this version
/// ```
pub mod port_packet {
    /// Bytes of one packet.
    pub const SIZE: usize = 32;
    /// Byte offset of `key`.
    pub const KEY_AT: usize = 0;
    /// Byte offset of `kind`.
    pub const KIND_AT: usize = 8;
    /// Byte offset of `trigger`.
    pub const TRIGGER_AT: usize = 12;
    /// Byte offset of `observed`.
    pub const OBSERVED_AT: usize = 16;
    /// Byte offset of `reserved`.
    pub const RESERVED_AT: usize = 20;
    /// Byte offset of `data`.
    pub const DATA_AT: usize = 24;
}

/// What made a packet (amendment F1).
pub mod packet_kind {
    /// The object's signals met the binding's mask.
    pub const SIGNAL: u32 = 1;
    /// The binding was cancelled implicitly (F11, F16).
    pub const CANCELED: u32 = 2;
}

/// The start record, version 1 (P6, amendment C4): what a process begins with. The kernel
/// writes it before the first thread runs, on pages the process may read but not write or
/// execute, at a random address, and the first thread finds that address in RDI.
///
/// ```text
/// 0    u32 size            bytes of the whole record, exactly
/// 4    u16 abi_major       the kernel's userspace ABI
/// 6    u16 abi_minor
/// 8    u16 abi_min_minor   the oldest minor of abi_major the kernel serves
/// 10   u16 reserved        zero
/// 12   u32 handle_count    at most 64
/// 16   u32 arg_count
/// 20   u32 arg_bytes       bytes of the arguments, prefixes and padding included
/// 24   u32 env_count
/// 28   u32 env_bytes       bytes of the environment, the same way
/// 32   handle_count × { u32 handle; u32 object_type; u32 rights; u32 role }
///      the arguments, then the environment: each a u32 length, its bytes, then zeros to 8
/// ```
///
/// The whole record is at most 64 KiB, and every padding byte is zero.
pub mod start {
    use crate::{ABI_VERSION, Handle};

    /// Bytes of the header.
    pub const HEADER: usize = 32;
    /// Bytes of one handle entry.
    pub const ENTRY: usize = 16;
    /// Most handles a start record carries.
    pub const MAX_HANDLES: usize = 64;
    /// Most bytes a start record takes, all of it.
    pub const MAX_SIZE: usize = 64 * 1024;
    /// The oldest minor of [`ABI_VERSION`]'s major the kernel serves. At major 0 it serves
    /// only its own.
    pub const ABI_MIN_MINOR: u16 = ABI_VERSION.1;

    /// Byte offset of `size`.
    pub const SIZE_AT: usize = 0;
    /// Byte offset of `abi_major`.
    pub const ABI_MAJOR_AT: usize = 4;
    /// Byte offset of `abi_minor`.
    pub const ABI_MINOR_AT: usize = 6;
    /// Byte offset of `abi_min_minor`.
    pub const ABI_MIN_MINOR_AT: usize = 8;
    /// Byte offset of `reserved`.
    pub const RESERVED_AT: usize = 10;
    /// Byte offset of `handle_count`.
    pub const HANDLE_COUNT_AT: usize = 12;
    /// Byte offset of `arg_count`.
    pub const ARG_COUNT_AT: usize = 16;
    /// Byte offset of `arg_bytes`.
    pub const ARG_BYTES_AT: usize = 20;
    /// Byte offset of `env_count`.
    pub const ENV_COUNT_AT: usize = 24;
    /// Byte offset of `env_bytes`.
    pub const ENV_BYTES_AT: usize = 28;
    /// Byte offset of an entry's `handle`, from the entry's start.
    pub const ENTRY_HANDLE_AT: usize = 0;
    /// Byte offset of an entry's `object_type`.
    pub const ENTRY_OBJECT_TYPE_AT: usize = 4;
    /// Byte offset of an entry's `rights`.
    pub const ENTRY_RIGHTS_AT: usize = 8;
    /// Byte offset of an entry's `role`.
    pub const ENTRY_ROLE_AT: usize = 12;

    /// What a start-record handle is for (amendment C5).
    pub mod role {
        /// The channel to whoever started the process.
        pub const BOOTSTRAP: u32 = 1;
        /// The process itself.
        pub const SELF_PROCESS: u32 = 2;
        /// Its first thread.
        pub const SELF_THREAD: u32 = 3;
        // 4 was SERVICE: retired, and never reused, because a process finds services through
        // its directory (ADR-0011 N9).
        /// The channel to the service directory.
        pub const DIRECTORY: u32 = 5;
    }

    /// One handle the record names.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Entry {
        /// Its value in the process's table.
        pub handle: Handle,
        /// The type of the object it names (an [`ObjectType`](crate::ObjectType)).
        pub object_type: u32,
        /// Its rights.
        pub rights: u32,
        /// What it is for: a [`role`].
        pub role: u32,
    }

    /// Why a start record could not be encoded or decoded.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum StartError {
        /// The buffer is too small, or the record runs past it.
        Short,
        /// More than [`MAX_HANDLES`] handles.
        TooManyHandles,
        /// More than [`MAX_SIZE`] bytes.
        TooLarge,
        /// A size or count disagrees with the rest, or a reserved or padding byte is not zero.
        Malformed,
    }

    fn put(out: &mut [u8], at: usize, bytes: &[u8]) -> Result<(), StartError> {
        let end = at.checked_add(bytes.len()).ok_or(StartError::Short)?;
        out.get_mut(at..end)
            .ok_or(StartError::Short)?
            .copy_from_slice(bytes);
        Ok(())
    }

    fn get<const N: usize>(b: &[u8], at: usize) -> Result<[u8; N], StartError> {
        let end = at.checked_add(N).ok_or(StartError::Short)?;
        let mut v = [0u8; N];
        v.copy_from_slice(b.get(at..end).ok_or(StartError::Short)?);
        Ok(v)
    }

    fn u32_at(b: &[u8], at: usize) -> Result<u32, StartError> {
        get(b, at).map(u32::from_le_bytes)
    }

    fn u16_at(b: &[u8], at: usize) -> Result<u16, StartError> {
        get(b, at).map(u16::from_le_bytes)
    }

    /// Bytes one string takes: its length, its bytes, and zeros to the next multiple of 8.
    fn string_bytes(len: usize) -> Option<usize> {
        len.checked_add(4)?.checked_add(7).map(|n| n & !7)
    }

    /// Bytes a list of strings takes.
    fn strings_bytes(strings: &[&[u8]]) -> Option<usize> {
        strings
            .iter()
            .try_fold(0usize, |total, s| total.checked_add(string_bytes(s.len())?))
    }

    /// The size of the record for `handles` entries, `args` and `env`, within the limits.
    pub fn size_of(handles: usize, args: &[&[u8]], env: &[&[u8]]) -> Result<usize, StartError> {
        if handles > MAX_HANDLES {
            return Err(StartError::TooManyHandles);
        }
        let size = ENTRY
            .checked_mul(handles)
            .and_then(|n| n.checked_add(HEADER))
            .and_then(|n| n.checked_add(strings_bytes(args)?))
            .and_then(|n| n.checked_add(strings_bytes(env)?))
            .ok_or(StartError::TooLarge)?;
        if size > MAX_SIZE {
            return Err(StartError::TooLarge);
        }
        Ok(size)
    }

    fn u32_of(n: usize) -> Result<u32, StartError> {
        u32::try_from(n).map_err(|_| StartError::TooLarge)
    }

    /// Writes `strings` at `at`; returns the offset after them.
    fn put_strings(out: &mut [u8], at: usize, strings: &[&[u8]]) -> Result<usize, StartError> {
        let mut at = at;
        for s in strings {
            put(out, at, &u32_of(s.len())?.to_le_bytes())?;
            put(out, at.checked_add(4).ok_or(StartError::Short)?, s)?;
            at = at
                .checked_add(string_bytes(s.len()).ok_or(StartError::TooLarge)?)
                .ok_or(StartError::Short)?;
        }
        Ok(at)
    }

    /// Writes the start record for `entries`, `args` and `env` at the start of `out`, padding
    /// included, and returns its size. Every byte of the record is written, so nothing else
    /// that was in `out` remains in it.
    pub fn encode(
        entries: &[Entry],
        args: &[&[u8]],
        env: &[&[u8]],
        out: &mut [u8],
    ) -> Result<usize, StartError> {
        let size = size_of(entries.len(), args, env)?;
        let record = out.get_mut(..size).ok_or(StartError::Short)?;
        record.fill(0);
        let arg_bytes = strings_bytes(args).ok_or(StartError::TooLarge)?;
        let env_bytes = strings_bytes(env).ok_or(StartError::TooLarge)?;
        put(record, SIZE_AT, &u32_of(size)?.to_le_bytes())?;
        put(record, ABI_MAJOR_AT, &ABI_VERSION.0.to_le_bytes())?;
        put(record, ABI_MINOR_AT, &ABI_VERSION.1.to_le_bytes())?;
        put(record, ABI_MIN_MINOR_AT, &ABI_MIN_MINOR.to_le_bytes())?;
        put(
            record,
            HANDLE_COUNT_AT,
            &u32_of(entries.len())?.to_le_bytes(),
        )?;
        put(record, ARG_COUNT_AT, &u32_of(args.len())?.to_le_bytes())?;
        put(record, ARG_BYTES_AT, &u32_of(arg_bytes)?.to_le_bytes())?;
        put(record, ENV_COUNT_AT, &u32_of(env.len())?.to_le_bytes())?;
        put(record, ENV_BYTES_AT, &u32_of(env_bytes)?.to_le_bytes())?;
        let mut at = HEADER;
        for entry in entries {
            for (offset, value) in [
                (ENTRY_HANDLE_AT, entry.handle),
                (ENTRY_OBJECT_TYPE_AT, entry.object_type),
                (ENTRY_RIGHTS_AT, entry.rights),
                (ENTRY_ROLE_AT, entry.role),
            ] {
                let field = at.checked_add(offset).ok_or(StartError::Short)?;
                put(record, field, &value.to_le_bytes())?;
            }
            at = at.checked_add(ENTRY).ok_or(StartError::Short)?;
        }
        let at = put_strings(record, at, args)?;
        let end = put_strings(record, at, env)?;
        if end != size {
            return Err(StartError::Malformed);
        }
        Ok(size)
    }

    /// A list of strings, checked: each a length, its bytes and zero padding to 8, filling
    /// exactly the area they were given.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Strings<'a> {
        area: &'a [u8],
        count: usize,
    }

    impl<'a> Strings<'a> {
        fn check(area: &'a [u8], count: usize) -> Result<Self, StartError> {
            let mut at = 0usize;
            for _ in 0..count {
                let len = u32_at(area, at).map_err(|_| StartError::Malformed)? as usize;
                let start = at.checked_add(4).ok_or(StartError::Malformed)?;
                let next = at
                    .checked_add(string_bytes(len).ok_or(StartError::Malformed)?)
                    .ok_or(StartError::Malformed)?;
                let end = start.checked_add(len).ok_or(StartError::Malformed)?;
                let padding = area.get(end..next).ok_or(StartError::Malformed)?;
                if padding.iter().any(|&b| b != 0) {
                    return Err(StartError::Malformed);
                }
                at = next;
            }
            if at != area.len() {
                return Err(StartError::Malformed);
            }
            Ok(Self { area, count })
        }

        /// How many there are.
        pub const fn count(&self) -> usize {
            self.count
        }

        /// Each string, in order.
        pub fn iter(&self) -> impl Iterator<Item = &'a [u8]> + 'a {
            let area = self.area;
            let mut at = 0usize;
            (0..self.count).map_while(move |_| {
                let len = u32_at(area, at).ok()? as usize;
                let start = at.checked_add(4)?;
                let bytes = area.get(start..start.checked_add(len)?)?;
                at = at.checked_add(string_bytes(len)?)?;
                Some(bytes)
            })
        }
    }

    /// A decoded and checked start record.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct StartRecord<'a> {
        /// The kernel's userspace ABI version.
        pub abi: (u16, u16),
        /// The oldest minor of `abi`'s major the kernel serves.
        pub abi_min_minor: u16,
        entries: &'a [u8],
        count: usize,
        /// The arguments.
        pub args: Strings<'a>,
        /// The environment.
        pub env: Strings<'a>,
    }

    impl StartRecord<'_> {
        /// Each handle entry, in order.
        pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
            (0..self.count).map_while(|i| {
                let at = ENTRY.checked_mul(i)?;
                let field = |offset: usize| u32_at(self.entries, at.checked_add(offset)?).ok();
                Some(Entry {
                    handle: field(ENTRY_HANDLE_AT)?,
                    object_type: field(ENTRY_OBJECT_TYPE_AT)?,
                    rights: field(ENTRY_RIGHTS_AT)?,
                    role: field(ENTRY_ROLE_AT)?,
                })
            })
        }

        /// The first entry with `role`, if any.
        pub fn find(&self, role: u32) -> Option<Entry> {
            self.entries().find(|e| e.role == role)
        }
    }

    /// Decodes and checks the start record at the start of `b`: its size is exact, its counts
    /// agree with its areas, and every reserved and padding byte is zero.
    pub fn decode(b: &[u8]) -> Result<StartRecord<'_>, StartError> {
        let size = u32_at(b, SIZE_AT)? as usize;
        if size > MAX_SIZE {
            return Err(StartError::TooLarge);
        }
        let record = b.get(..size).ok_or(StartError::Short)?;
        if size < HEADER || u16_at(record, RESERVED_AT)? != 0 {
            return Err(StartError::Malformed);
        }
        let count = u32_at(record, HANDLE_COUNT_AT)? as usize;
        if count > MAX_HANDLES {
            return Err(StartError::TooManyHandles);
        }
        let arg_bytes = u32_at(record, ARG_BYTES_AT)? as usize;
        let env_bytes = u32_at(record, ENV_BYTES_AT)? as usize;
        let args_at = ENTRY
            .checked_mul(count)
            .and_then(|n| n.checked_add(HEADER))
            .ok_or(StartError::Malformed)?;
        let env_at = args_at
            .checked_add(arg_bytes)
            .ok_or(StartError::Malformed)?;
        if env_at.checked_add(env_bytes) != Some(size) {
            return Err(StartError::Malformed);
        }
        let entries = record.get(HEADER..args_at).ok_or(StartError::Malformed)?;
        let args = record.get(args_at..env_at).ok_or(StartError::Malformed)?;
        let env = record.get(env_at..size).ok_or(StartError::Malformed)?;
        Ok(StartRecord {
            abi: (u16_at(record, ABI_MAJOR_AT)?, u16_at(record, ABI_MINOR_AT)?),
            abi_min_minor: u16_at(record, ABI_MIN_MINOR_AT)?,
            entries,
            count,
            args: Strings::check(args, u32_at(record, ARG_COUNT_AT)? as usize)?,
            env: Strings::check(env, u32_at(record, ENV_COUNT_AT)? as usize)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_values_are_stable_and_distinct() {
        // Pinned: these values are the C ABI (P5); changing one breaks every program.
        let expected: [(Error, i64); 14] = [
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
            (Error::NotFound, -14),
        ];
        for (e, v) in expected {
            assert_eq!(e.raw(), v);
            assert_eq!(Error::from_raw(v), Some(e));
        }
        assert_eq!(Error::from_raw(OK), None);
        assert_eq!(Error::from_raw(-15), None);
        assert_eq!(Error::ALL.len(), expected.len());
    }

    #[test]
    fn every_status_has_a_distinct_name() {
        assert_eq!(status_name(OK), "OK");
        assert_eq!(status_name(-14), "NOT_FOUND");
        assert_eq!(status_name(-15), "UNKNOWN");
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
            syscall::MEMORY_OBJECT_CREATE,
            syscall::MEMORY_MAP,
            syscall::MEMORY_UNMAP,
            syscall::MEMORY_PROTECT,
            syscall::PORT_CREATE,
            syscall::OBJECT_WAIT_ASYNC,
            syscall::PORT_WAIT,
            syscall::PORT_CANCEL,
            syscall::CLOCK_READ,
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
        assert_eq!(read_sizes::BASE_SIZE, 12);
        // The writer's identity follows, each `u64` aligned (B1, C2).
        assert_eq!(read_sizes::SENDER_PROCESS_AT, 16);
        assert_eq!(read_sizes::SENDER_PRINCIPAL_AT, 24);
        assert_eq!(read_sizes::SIZE, 32);
        // Three `u32`s, in order, with no padding (B3).
        assert_eq!(received_handle::HANDLE_AT, 0);
        assert_eq!(received_handle::OBJECT_TYPE_AT, 4);
        assert_eq!(received_handle::RIGHTS_AT, 8);
        assert_eq!(received_handle::SIZE, 12);
    }

    /// A channel end in a start record, with `role`.
    fn channel_entry(handle: u32, role: u32) -> start::Entry {
        start::Entry {
            handle,
            object_type: ObjectType::Channel as u32,
            rights: rights::CHANNEL,
            role,
        }
    }

    #[test]
    fn the_rights_of_a_process_and_a_thread_are_c6s() {
        assert_eq!(
            rights::PROCESS,
            rights::READ
                | rights::WRITE
                | rights::MANAGE
                | rights::WAIT
                | rights::DUPLICATE
                | rights::TRANSFER
        );
        assert_eq!(
            rights::THREAD,
            rights::WAIT | rights::MANAGE | rights::DUPLICATE | rights::TRANSFER
        );
        assert_eq!(rights::PROCESS & !rights::ALL, 0);
        assert_eq!(rights::THREAD & !rights::ALL, 0);
    }

    #[test]
    fn a_memory_object_has_c6s_rights_and_never_execute() {
        assert_eq!(
            rights::MEMORY_OBJECT,
            rights::READ | rights::WRITE | rights::MAP | rights::DUPLICATE | rights::TRANSFER
        );
        assert_eq!(rights::MEMORY_OBJECT & rights::EXECUTE, 0);
        assert_eq!(rights::MEMORY_OBJECT & !rights::ALL, 0);
    }

    #[test]
    fn the_memory_requests_are_laid_out_field_by_field() {
        // Each field at its alignment after the implicit size, with no hole but the one named.
        assert_eq!(object_request::SIZE_AT, 0);
        assert_eq!(object_request::LENGTH_AT, 8);
        assert_eq!(object_request::SIZE as usize, object_request::LENGTH_AT + 8);
        assert_eq!(map_request::ACCESS_AT, 4);
        assert_eq!(map_request::FLAGS_AT, 8);
        assert_eq!(map_request::OFFSET_AT, 16);
        assert_eq!(map_request::LENGTH_AT, 24);
        assert_eq!(map_request::ADDRESS_AT, 32);
        assert_eq!(map_request::SIZE as usize, map_request::ADDRESS_AT + 8);
        assert_eq!(map_access::ALL, 0b111);
        assert_eq!(map_flags::ALL, 0b1);
        assert!(PAGE_SIZE.is_power_of_two());
    }

    #[test]
    fn start_records_round_trip_with_their_handles_arguments_and_environment() {
        let entries = [
            start::Entry {
                handle: 0x0001_0001,
                object_type: ObjectType::Process as u32,
                rights: rights::PROCESS,
                role: start::role::SELF_PROCESS,
            },
            start::Entry {
                handle: 0x0001_0002,
                object_type: ObjectType::Thread as u32,
                rights: rights::THREAD,
                role: start::role::SELF_THREAD,
            },
            channel_entry(0x0001_0003, start::role::BOOTSTRAP),
            channel_entry(0x0001_0004, start::role::DIRECTORY),
        ];
        let args: [&[u8]; 3] = [b"origin", b"", b"seven b"];
        let env: [&[u8]; 1] = [b"LANG=te"];
        let mut buf = [0xAAu8; 512];
        let size = start::encode(&entries, &args, &env, &mut buf).unwrap();
        // The header, four entries, then 4 + 6 bytes padded to 16, 4 to 8, 4 + 7 to 16, and
        // the environment's 4 + 7 to 16.
        assert_eq!(size, 32 + 4 * 16 + 16 + 8 + 16 + 16);
        assert_eq!(start::size_of(entries.len(), &args, &env), Ok(size));
        let r = start::decode(&buf[..size]).unwrap();
        assert_eq!(r.abi, ABI_VERSION);
        assert_eq!(r.abi_min_minor, ABI_VERSION.1);
        assert_eq!(r.entries().collect::<Vec<_>>(), entries);
        assert_eq!(r.find(start::role::DIRECTORY), Some(entries[3]));
        assert_eq!(r.find(4), None, "4 was SERVICE, and is retired");
        assert_eq!(r.args.count(), 3);
        assert_eq!(r.args.iter().collect::<Vec<_>>(), args);
        assert_eq!(r.env.iter().collect::<Vec<_>>(), env);
        // Where a program finds them.
        assert_eq!(&buf[..4], &(size as u32).to_le_bytes());
        assert_eq!(&buf[32..36], &0x0001_0001u32.to_le_bytes());
        assert_eq!(
            &buf[32 + 12..32 + 16],
            &start::role::SELF_PROCESS.to_le_bytes()
        );
        // Every reserved and padding byte is zero, whatever the buffer held, and nothing past
        // the record is written.
        assert_eq!(&buf[start::RESERVED_AT..start::RESERVED_AT + 2], &[0, 0]);
        let first_arg = start::HEADER + 4 * start::ENTRY;
        assert_eq!(&buf[first_arg + 4..first_arg + 10], b"origin");
        assert_eq!(&buf[first_arg + 10..first_arg + 16], &[0; 6]);
        assert_eq!(buf[size], 0xAA);
    }

    #[test]
    fn start_records_hold_their_limits() {
        let one = [channel_entry(1, start::role::BOOTSTRAP)];
        let mut buf = vec![0u8; start::MAX_SIZE + 64];
        let too_many = [channel_entry(1, start::role::BOOTSTRAP); start::MAX_HANDLES + 1];
        assert_eq!(
            start::encode(&too_many, &[], &[], &mut buf),
            Err(start::StartError::TooManyHandles)
        );
        let full = [channel_entry(1, start::role::BOOTSTRAP); start::MAX_HANDLES];
        assert!(start::encode(&full, &[], &[], &mut buf).is_ok());
        // Exactly 64 KiB is allowed, and one string byte more is not.
        let room = start::MAX_SIZE - start::HEADER - start::ENTRY - 4;
        let largest = vec![7u8; room];
        assert_eq!(
            start::encode(&one, &[&largest], &[], &mut buf),
            Ok(start::MAX_SIZE)
        );
        assert!(start::decode(&buf).is_ok());
        let over = vec![7u8; room + 1];
        assert_eq!(
            start::encode(&one, &[&over], &[], &mut buf),
            Err(start::StartError::TooLarge)
        );
        assert_eq!(
            start::encode(&one, &[b"x"], &[], &mut [0u8; 40]),
            Err(start::StartError::Short)
        );
    }

    #[test]
    fn malformed_start_records_are_refused() {
        let entries = [channel_entry(0x0001_0003, start::role::BOOTSTRAP)];
        let mut good = [0u8; 128];
        let size = start::encode(&entries, &[b"abc"], &[], &mut good).unwrap();
        let first_arg = start::HEADER + start::ENTRY;
        let refused = |edit: &dyn Fn(&mut [u8; 128])| {
            let mut bad = good;
            edit(&mut bad);
            start::decode(&bad).map(|_| ())
        };
        let malformed = Err(start::StartError::Malformed);
        assert_eq!(start::decode(&good[..size]).map(|_| ()), Ok(()));
        assert_eq!(
            refused(&|b| b[..4].copy_from_slice(&((size + 8) as u32).to_le_bytes())),
            malformed,
            "a size the areas do not fill"
        );
        assert_eq!(
            refused(&|b| b[..4].copy_from_slice(&8u32.to_le_bytes())),
            malformed,
            "a size under the header's"
        );
        assert_eq!(
            refused(&|b| b[start::RESERVED_AT] = 1),
            malformed,
            "reserved"
        );
        assert_eq!(
            refused(&|b| b[first_arg + 7] = 1),
            malformed,
            "a padding byte"
        );
        assert_eq!(
            refused(&|b| b[first_arg..first_arg + 4].copy_from_slice(&100u32.to_le_bytes())),
            malformed,
            "a string past its area"
        );
        assert_eq!(
            refused(&|b| b[start::ARG_COUNT_AT] = 2),
            malformed,
            "a count the area does not hold"
        );
        assert_eq!(
            refused(&|b| b[start::HANDLE_COUNT_AT] = 65),
            Err(start::StartError::TooManyHandles)
        );
        assert_eq!(
            refused(&|b| b[..4].copy_from_slice(&(64 * 1024 + 8u32).to_le_bytes())),
            Err(start::StartError::TooLarge)
        );
        assert_eq!(
            start::decode(&good[..size - 1]).map(|_| ()),
            Err(start::StartError::Short)
        );
    }
}
