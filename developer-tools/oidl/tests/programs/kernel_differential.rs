//! Compares the generated `dispatch` with the argument handling of the hand-written kernel, as
//! it was at commit 9ca0438: `framekernel/src/syscall.rs` and the checks `ipc::handle_duplicate`
//! makes on its arguments alone. The transcription below (`hand_written`) stops where the kernel
//! first touches user memory or a handle table, because the generated decoder stops there too.
//! Compiled with optimizations and run by tests/generated.rs.
//!
//! One rule has changed since that commit, by decision: a handle fills its register (ADR-0005
//! Amendment 6, E1; task S4-0), so `handle_arg` refuses nothing any more. Everything else is the
//! transcription as it was.

use oidl_generated::oidl_rt as rt;
use oidl_generated::origin_kernel as k;
use oidl_generated::origin_kernel::kernel::server as s;

const INVALID_ARGS: i64 = -1;
const BAD_HANDLE: i64 = -2;
const NOT_SUPPORTED: i64 = -11;

const MAX_MESSAGE_BYTES: u64 = 65_536;
const MAX_MESSAGE_HANDLES: u64 = 64;
const RIGHTS_ALL: u32 = 0x1FF;
const SIGNALS_ALL: u32 = 0x1F;
const MAP_ACCESS_ALL: u32 = 0x7;

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Status(i64),
    Exit,
}

/// `handle_arg` in `syscall.rs`, with E1 applied: until S4-0 a register above 32 bits was
/// `bad_handle` here; now every value goes to the table, which is past what this compares.
#[allow(clippy::unnecessary_wraps)]
fn handle_arg(raw: u64) -> Result<u64, i64> {
    Ok(raw)
}

/// `dispatch` in `framekernel/src/syscall.rs`, for the parts decided by registers alone.
/// A process's debug permission is `debug_calls`.
fn hand_written(call: u64, a: [u64; 6], debug_calls: bool) -> Verdict {
    let [a0, a1, a2, a3, a4, _] = a;
    let result: Result<(), i64> = match call {
        0 => return Verdict::Exit,
        1 | 2 if !debug_calls => Err(NOT_SUPPORTED),
        1 => Ok(()),
        // debug_write: length > 128 is INVALID_ARGS, then the copy.
        2 => {
            if a1 > 128 {
                Err(INVALID_ARGS)
            } else {
                Ok(())
            }
        }
        // handle_close
        3 => handle_arg(a0).map(|_| ()),
        // handle_duplicate: the handle, then the rights fit in 32 bits, then the out pointer
        // (user memory; assumed good here), then ipc::handle_duplicate's undefined-bits check.
        4 => (|| {
            handle_arg(a0)?;
            let rights = u32::try_from(a1).map_err(|_| INVALID_ARGS)?;
            if rights & !RIGHTS_ALL != 0 {
                return Err(INVALID_ARGS);
            }
            Ok(())
        })(),
        // channel_create: only a pointer.
        5 => Ok(()),
        // channel_write: handle, then the two lengths, then the limits.
        6 => (|| {
            handle_arg(a0)?;
            if a2 > MAX_MESSAGE_BYTES || a4 > MAX_MESSAGE_HANDLES {
                return Err(INVALID_ARGS);
            }
            Ok(())
        })(),
        // channel_read: the handle; capacities are clamped, never refused.
        7 => handle_arg(a0).map(|_| ()),
        // wait_one: the handle; the mask must fit, be non-zero and have no unknown bits.
        8 => (|| {
            handle_arg(a0)?;
            let mask = u32::try_from(a1).map_err(|_| INVALID_ARGS)?;
            if mask == 0 || mask & !SIGNALS_ALL != 0 {
                return Err(INVALID_ARGS);
            }
            Ok(())
        })(),
        // The memory calls (S3-5b) came after 9ca0438, with the definition; their register checks
        // are OIDL §10's. memory_object_create: only pointers.
        11 => Ok(()),
        // memory_map: two handles, then pointers.
        12 => handle_arg(a0).and_then(|_| handle_arg(a1)).map(|_| ()),
        // memory_unmap: the handle, then two u64s.
        13 => handle_arg(a0).map(|_| ()),
        // memory_protect: the handle, two u64s, then access: it must fit, declare no unknown bit
        // and not be zero.
        14 => (|| {
            handle_arg(a0)?;
            let access = u32::try_from(a3).map_err(|_| INVALID_ARGS)?;
            if access == 0 || access & !MAP_ACCESS_ALL != 0 {
                return Err(INVALID_ARGS);
            }
            Ok(())
        })(),
        // The event and port calls (S3-6, ADR-0005 Amendment 7) came with the definition too;
        // their register checks are OIDL §10's. event_create: only a pointer.
        9 => Ok(()),
        // object_signal: the handle, then each mask must fit and declare no unknown bit; either
        // may be zero.
        10 => (|| {
            handle_arg(a0)?;
            for mask in [a1, a2] {
                let mask = u32::try_from(mask).map_err(|_| INVALID_ARGS)?;
                if mask & !SIGNALS_ALL != 0 {
                    return Err(INVALID_ARGS);
                }
            }
            Ok(())
        })(),
        // port_create: only a pointer.
        15 => Ok(()),
        // object_wait_async: two handles, the key, then the mask: it must fit, be non-zero and
        // declare no unknown bit.
        16 => (|| {
            handle_arg(a0)?;
            handle_arg(a1)?;
            let mask = u32::try_from(a3).map_err(|_| INVALID_ARGS)?;
            if mask == 0 || mask & !SIGNALS_ALL != 0 {
                return Err(INVALID_ARGS);
            }
            Ok(())
        })(),
        // port_wait: the handle, the deadline, then a pointer.
        17 => handle_arg(a0).map(|_| ()),
        // port_cancel: two handles and the key.
        18 => handle_arg(a0).and_then(|_| handle_arg(a1)).map(|_| ()),
        // clock_read: only a pointer.
        19 => Ok(()),
        // Every other number: the catch-all.
        _ => Err(NOT_SUPPORTED),
    };
    Verdict::Status(match result {
        Ok(()) => 0,
        Err(status) => status,
    })
}

/// A kernel that accepts everything the decoder lets through.
struct Reached {
    debug_calls: bool,
}

impl s::Calls for Reached {
    fn holds(&self, capability: k::Capability) -> bool {
        match capability {
            k::Capability::KernelDebug => self.debug_calls,
            _ => false,
        }
    }
    fn thread_exit(&self, _code: u64) {}
    fn debug_report(&self, _value: u64) -> Result<(), s::DebugReportError> {
        Ok(())
    }
    fn debug_write(&self, _text: rt::UserSlice<u8>) -> Result<(), s::DebugWriteError> {
        Ok(())
    }
    fn handle_close(&self, _handle: rt::HandleArg) -> Result<(), s::HandleCloseError> {
        Ok(())
    }
    fn handle_duplicate(
        &self,
        _handle: rt::HandleArg,
        _rights: k::Rights,
        _new_handle: rt::UserRef<rt::Handle>,
    ) -> Result<(), s::HandleDuplicateError> {
        Ok(())
    }
    fn channel_create(
        &self,
        _ends: rt::UserRef<[rt::Handle; 2]>,
    ) -> Result<(), s::ChannelCreateError> {
        Ok(())
    }
    fn channel_write(
        &self,
        _end: rt::HandleArg,
        _bytes: rt::UserSlice<u8>,
        _handles: rt::HandleSlice,
    ) -> Result<(), s::ChannelWriteError> {
        Ok(())
    }
    fn channel_read(
        &self,
        _end: rt::HandleArg,
        _bytes: rt::UserBuffer<u8>,
        _handles: rt::UserBuffer<k::ReceivedHandle>,
        _sizes: rt::UserRef<k::ReadSizes>,
    ) -> Result<(), s::ChannelReadError> {
        Ok(())
    }
    fn wait_one(
        &self,
        _handle: rt::HandleArg,
        _signals: k::Signals,
        _deadline: u64,
        _observed: rt::UserRef<k::Signals>,
    ) -> Result<(), s::WaitOneError> {
        Ok(())
    }
    fn memory_object_create(
        &self,
        _request: rt::UserRef<k::ObjectRequest>,
        _object: rt::UserRef<rt::Handle>,
    ) -> Result<(), s::MemoryObjectCreateError> {
        Ok(())
    }
    fn memory_map(
        &self,
        _process: rt::HandleArg,
        _object: rt::HandleArg,
        _request: rt::UserRef<k::MapRequest>,
        _address: rt::UserRef<u64>,
    ) -> Result<(), s::MemoryMapError> {
        Ok(())
    }
    fn memory_unmap(
        &self,
        _process: rt::HandleArg,
        _address: u64,
        _length: u64,
    ) -> Result<(), s::MemoryUnmapError> {
        Ok(())
    }
    fn memory_protect(
        &self,
        _process: rt::HandleArg,
        _address: u64,
        _length: u64,
        _access: k::MapAccess,
    ) -> Result<(), s::MemoryProtectError> {
        Ok(())
    }
    fn event_create(&self, _event: rt::UserRef<rt::Handle>) -> Result<(), s::EventCreateError> {
        Ok(())
    }
    fn object_signal(
        &self,
        _object: rt::HandleArg,
        _clear: k::Signals,
        _set: k::Signals,
    ) -> Result<(), s::ObjectSignalError> {
        Ok(())
    }
    fn port_create(&self, _port: rt::UserRef<rt::Handle>) -> Result<(), s::PortCreateError> {
        Ok(())
    }
    fn object_wait_async(
        &self,
        _port: rt::HandleArg,
        _object: rt::HandleArg,
        _key: u64,
        _signals: k::Signals,
    ) -> Result<(), s::ObjectWaitAsyncError> {
        Ok(())
    }
    fn port_wait(
        &self,
        _port: rt::HandleArg,
        _deadline: u64,
        _packet: rt::UserRef<k::PortPacket>,
    ) -> Result<(), s::PortWaitError> {
        Ok(())
    }
    fn port_cancel(
        &self,
        _port: rt::HandleArg,
        _object: rt::HandleArg,
        _key: u64,
    ) -> Result<(), s::PortCancelError> {
        Ok(())
    }
    fn clock_read(&self, _nanos: rt::UserRef<u64>) -> Result<(), s::ClockReadError> {
        Ok(())
    }
}

fn generated(call: u64, a: [u64; 6], debug_calls: bool) -> Verdict {
    match s::dispatch(&Reached { debug_calls }, call, a) {
        s::Outcome::Status(status) => Verdict::Status(status),
        s::Outcome::ThreadExit => Verdict::Exit,
    }
}

const EDGES: [u64; 14] = [
    0,
    1,
    2,
    12,
    128,
    129,
    65_536,
    65_537,
    0x7FFF_FFFF,
    0xFFFF_FFFF,
    0x1_0000_0000,
    0x1FF,
    0x200,
    u64::MAX,
];

/// xorshift64*.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn edge(&mut self) -> u64 {
        let index = (self.next() % EDGES.len() as u64) as usize;
        match self.next() % 3 {
            0 => EDGES[index],
            1 => self.next(),
            _ => self.next() & 0x3FF,
        }
    }
}

fn compare(call: u64, a: [u64; 6], debug_calls: bool, mismatches: &mut usize) {
    let hand = hand_written(call, a, debug_calls);
    let made = generated(call, a, debug_calls);
    if hand != made {
        *mismatches += 1;
        if *mismatches <= 20 {
            eprintln!("MISMATCH call {call} args {a:x?} debug {debug_calls}: kernel {hand:?}, generated {made:?}");
        }
    }
}

fn main() {
    let mut mismatches = 0usize;
    let mut cases = 0u64;
    // Every call number the kernel knows and several it does not.
    for call in (0..=21u64).chain([65_535, 1 << 32, u64::MAX]) {
        for debug_calls in [false, true] {
            // Every edge in the first three registers, with the rest at the extremes.
            for &a0 in &EDGES {
                for &a1 in &EDGES {
                    for &a2 in &EDGES {
                        for a3 in [0, u64::MAX] {
                            for a4 in [0, 65, u64::MAX] {
                                compare(call, [a0, a1, a2, a3, a4, 0], debug_calls, &mut mismatches);
                                cases += 1;
                            }
                        }
                    }
                }
            }
            // Random registers.
            let mut rng = Rng(0x5EED_0000_0000_0001 ^ call ^ u64::from(debug_calls));
            for _ in 0..40_000 {
                let a = [rng.edge(), rng.edge(), rng.edge(), rng.edge(), rng.edge(), rng.edge()];
                compare(call, a, debug_calls, &mut mismatches);
                cases += 1;
            }
        }
    }
    println!("kernel differential: {cases} cases, {mismatches} mismatches");
    if mismatches != 0 {
        std::process::exit(1);
    }
}
