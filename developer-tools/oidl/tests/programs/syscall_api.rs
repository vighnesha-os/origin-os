//! Exercises the syscall-transport outputs on the host: the kernel-side decoders' values, a
//! sized struct's version rules, and the safe client through the C ABI against a mock kernel.
//! Compiled and run by tests/generated.rs.

use std::sync::Mutex;

use oidl_generated::oidl_rt as rt;
use oidl_generated::origin_fixture_calls as f;
use oidl_generated::origin_fixture_calls::calls::server as cs;
use oidl_generated::origin_kernel as k;
use oidl_generated::origin_kernel::kernel::client as kc;
use oidl_generated::origin_kernel::kernel::server as ks;

// --- The mock kernel: the C entry points the safe client links against -------------------------

#[derive(Debug, Default, Clone)]
struct Log {
    calls: Vec<String>,
    /// What the next channel_read should do: (message bytes, handles).
    next_read: Option<(Vec<u8>, Vec<u32>)>,
    /// What the next wait_one should report: (status, observed).
    next_wait: (i64, u32),
    /// The status the next call should return.
    status: i64,
}

static MOCK: Mutex<Option<Log>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Log) -> R) -> R {
    let mut guard = MOCK.lock().expect("lock");
    f(guard.get_or_insert_with(Log::default))
}

fn reset() {
    *MOCK.lock().expect("lock") = Some(Log::default());
}

#[unsafe(no_mangle)]
pub extern "C" fn origin_kernel_thread_exit(_code: u64) -> ! {
    unreachable!()
}
#[unsafe(no_mangle)]
pub extern "C" fn origin_kernel_debug_report(value: u64) -> i64 {
    with(|m| {
        m.calls.push(format!("debug_report {value}"));
        m.status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_debug_write(text: *const u8, count: u64) -> i64 {
    let bytes = unsafe { core::slice::from_raw_parts(text, count as usize) };
    let shown = String::from_utf8_lossy(bytes).into_owned();
    with(|m| {
        m.calls.push(format!("debug_write {shown}"));
        m.status
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn origin_kernel_handle_close(handle: u32) -> i64 {
    with(|m| {
        m.calls.push(format!("handle_close {handle}"));
        m.status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_handle_duplicate(handle: u32, rights: u32, out: *mut u32) -> i64 {
    with(|m| {
        m.calls.push(format!("handle_duplicate {handle} {rights}"));
        if m.status == 0 {
            unsafe { out.write(handle + 1000) };
        }
        m.status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_channel_create(out: *mut u32) -> i64 {
    with(|m| {
        m.calls.push("channel_create".to_string());
        if m.status == 0 {
            unsafe {
                out.write(77);
                out.add(1).write(78);
            }
        }
        m.status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_channel_write(
    end: u32,
    bytes: *const u8,
    bytes_count: u64,
    handles: *const u32,
    handles_count: u64,
) -> i64 {
    let payload = unsafe { core::slice::from_raw_parts(bytes, bytes_count as usize) }.to_vec();
    let moved = unsafe { core::slice::from_raw_parts(handles, handles_count as usize) }.to_vec();
    with(|m| {
        m.calls.push(format!("channel_write {end} {payload:?} {moved:?}"));
        m.status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_channel_read(
    end: u32,
    bytes: *mut u8,
    bytes_capacity: u64,
    handles: *mut u32,
    handles_capacity: u64,
    sizes: *mut u8,
) -> i64 {
    with(|m| {
        m.calls.push(format!("channel_read {end} {bytes_capacity} {handles_capacity}"));
        // The kernel reads the `size` the client set.
        let declared = unsafe { (sizes as *const u32).read_unaligned() };
        assert_eq!(declared, k::ReadSizes::SIZE, "the client sets size");
        let Some((message, list)) = m.next_read.clone() else {
            return k::Error::ShouldWait.raw();
        };
        if message.len() > bytes_capacity as usize || list.len() > handles_capacity as usize {
            unsafe {
                (sizes.add(4) as *mut u32).write_unaligned(message.len() as u32);
                (sizes.add(8) as *mut u32).write_unaligned(list.len() as u32);
            }
            return k::Error::BufferTooSmall.raw();
        }
        unsafe {
            core::ptr::copy_nonoverlapping(message.as_ptr(), bytes, message.len());
            core::ptr::copy_nonoverlapping(list.as_ptr(), handles, list.len());
            (sizes.add(4) as *mut u32).write_unaligned(message.len() as u32);
            (sizes.add(8) as *mut u32).write_unaligned(list.len() as u32);
        }
        0
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_wait_one(
    handle: u32,
    signals: u32,
    deadline: u64,
    observed: *mut u32,
) -> i64 {
    with(|m| {
        m.calls.push(format!("wait_one {handle} {signals} {deadline}"));
        let (status, seen) = m.next_wait;
        unsafe { observed.write(seen) };
        status
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn origin_kernel_event_create(_out: *mut u32) -> i64 {
    k::Error::NotSupported.raw()
}
#[unsafe(no_mangle)]
pub extern "C" fn origin_kernel_object_signal(_o: u32, _clear: u32, _set: u32) -> i64 {
    k::Error::NotSupported.raw()
}

fn handle(raw: u32) -> rt::Handle {
    rt::Handle::from_abi(raw)
}

fn client_paths() {
    reset();
    assert_eq!(kc::debug_report(7), Ok(()));
    with(|m| m.status = k::Error::NotSupported.raw());
    assert_eq!(kc::debug_report(7), Err(rt::CallError::Method(ks::DebugReportError::NotSupported)));
    with(|m| m.status = -99);
    assert_eq!(kc::debug_report(7), Err(rt::CallError::Undeclared(-99)), "a status no method declares");
    with(|m| m.status = 0);
    assert_eq!(kc::debug_write(b"hello"), Ok(()));
    assert_eq!(kc::handle_close(handle(5)), Ok(()));
    let rights = k::Rights::READ.union(k::Rights::WRITE);
    assert_eq!(kc::handle_duplicate(handle(5), rights), Ok(handle(1005)));
    assert_eq!(kc::channel_create(), Ok([handle(77), handle(78)]));
    assert_eq!(kc::channel_write(handle(3), b"abc", &[handle(4), handle(5)]), Ok(()));

    // channel_read: success fills the buffers and reports the counts through @count.
    with(|m| m.next_read = Some((b"xyz".to_vec(), vec![9])));
    let mut bytes = [0u8; 8];
    let mut handles = [rt::Handle::INVALID; 2];
    let out = kc::channel_read(handle(3), &mut bytes, &mut handles).expect("read");
    assert_eq!(out.bytes, 3);
    assert_eq!(out.handles, 1);
    assert_eq!(out.sizes, k::ReadSizes { bytes: 3, handles: 1 });
    assert_eq!(&bytes[..3], b"xyz");
    assert_eq!(handles[0], handle(9));

    // channel_read: too small writes the sizes (@also_on).
    with(|m| m.next_read = Some((vec![1; 20], vec![1, 2, 3])));
    let mut bytes = [0u8; 8];
    let mut handles = [rt::Handle::INVALID; 2];
    let failure = kc::channel_read(handle(3), &mut bytes, &mut handles).expect_err("too small");
    assert_eq!(
        failure,
        rt::CallError::Method(kc::ChannelReadFailure {
            error: ks::ChannelReadError::BufferTooSmall,
            sizes: Some(k::ReadSizes { bytes: 20, handles: 3 }),
        })
    );

    // An error the method declares but which writes nothing has no sizes.
    with(|m| m.next_read = None);
    let failure = kc::channel_read(handle(3), &mut bytes, &mut handles).expect_err("empty");
    assert_eq!(
        failure,
        rt::CallError::Method(kc::ChannelReadFailure {
            error: ks::ChannelReadError::ShouldWait,
            sizes: None,
        })
    );

    // wait_one: signals on success and, by @also_on, on a timeout.
    with(|m| m.next_wait = (0, k::Signals::READABLE.bits()));
    assert_eq!(kc::wait_one(handle(3), k::Signals::READABLE, 123), Ok(k::Signals::READABLE));
    with(|m| m.next_wait = (k::Error::TimedOut.raw(), k::Signals::PEER_CLOSED.bits()));
    assert_eq!(
        kc::wait_one(handle(3), k::Signals::READABLE, 0),
        Err(rt::CallError::Method(kc::WaitOneFailure {
            error: ks::WaitOneError::TimedOut,
            observed: Some(k::Signals::PEER_CLOSED),
        }))
    );
    // A result with an undeclared bit is a protocol error, not a value.
    with(|m| m.next_wait = (0, 0xFFFF_FFFF));
    assert_eq!(
        kc::wait_one(handle(3), k::Signals::READABLE, 0),
        Err(rt::CallError::Protocol(rt::WireError::Bits))
    );
    with(|m| m.next_wait = (k::Error::Canceled.raw(), 0xFFFF_FFFF));
    let failure = kc::wait_one(handle(3), k::Signals::READABLE, 0).expect_err("canceled");
    assert_eq!(
        failure,
        rt::CallError::Method(kc::WaitOneFailure { error: ks::WaitOneError::Canceled, observed: None }),
        "an undecodable @also_on value is reported as absent"
    );
    assert_eq!(kc::event_create(), Err(rt::CallError::Method(ks::EventCreateError::NotSupported)));
    let log = with(|m| m.calls.clone());
    assert!(log.contains(&"handle_duplicate 5 3".to_string()), "{log:?}");
    assert!(log.contains(&"channel_write 3 [97, 98, 99] [4, 5]".to_string()), "{log:?}");
    assert!(log.contains(&"channel_read 3 8 2".to_string()), "{log:?}");
    assert!(log.contains(&"wait_one 3 1 123".to_string()), "{log:?}");
}

// --- The kernel-side decoders: what `dispatch` hands the kernel ---------------------------------

#[derive(Default)]
struct Calls {
    holds: bool,
    log: Mutex<Vec<String>>,
}

impl Calls {
    fn push(&self, line: String) {
        self.log.lock().expect("lock").push(line);
    }
}

impl cs::Calls for Calls {
    fn holds(&self, _capability: f::Capability) -> bool {
        self.holds
    }
    fn scalars(&self, a: u8, b: i16, c: u32, d: i64, e: bool, g: f::Kind) -> Result<(), cs::ScalarsError> {
        self.push(format!("scalars {a} {b} {c} {d} {e} {g:?}"));
        Ok(())
    }
    fn modes(&self, g: f::Mode, h: u64, n: u64) -> Result<(), cs::ModesError> {
        self.push(format!("modes {} {h} {n}", g.bits()));
        Ok(())
    }
    fn buffers(
        &self,
        text: rt::UserSlice<u8>,
        data: rt::UserSlice<u32>,
        opt: Option<rt::HandleArg>,
        p: rt::UserRef<f::Pair>,
    ) -> Result<(), cs::BuffersError> {
        self.push(format!(
            "buffers {:x} {} {:x} {} {} {:?} {:x}",
            text.addr(),
            text.count(),
            data.addr(),
            data.count(),
            data.byte_len(),
            opt.map(|h| (h.value(), h.need_rights(), h.need_object())),
            p.addr()
        ));
        Ok(())
    }
    fn struct_io(
        &self,
        input: rt::UserRef<f::Options>,
        output: rt::UserRef<f::Options>,
        buffer: rt::UserBuffer<u8>,
    ) -> Result<(), cs::StructIoError> {
        self.push(format!(
            "struct_io {:x} {:x} {:x} {} {}",
            input.addr(),
            output.addr(),
            buffer.addr(),
            buffer.capacity(),
            buffer.byte_len()
        ));
        Ok(())
    }
    fn gated(&self) -> Result<(), cs::GatedError> {
        self.push("gated".to_string());
        Ok(())
    }
    fn rights(&self, h: rt::HandleArg) -> Result<(), cs::RightsError> {
        self.push(format!("rights {} {} {}", h.value(), h.need_rights(), h.need_object()));
        Ok(())
    }
    fn newer(&self, x: u64) -> Result<(), cs::NewerError> {
        self.push(format!("newer {x}"));
        Ok(())
    }
    fn results(
        &self,
        _a: rt::UserRef<bool>,
        _b: rt::UserRef<f::Kind>,
        _c: rt::UserRef<f::Mode>,
        _d: rt::UserRef<rt::Handle>,
        _e: rt::UserRef<rt::Handle>,
        _f: rt::UserRef<u64>,
    ) -> Result<(), cs::ResultsError> {
        self.push("results".to_string());
        Ok(())
    }
}

fn status(outcome: cs::Outcome) -> i64 {
    match outcome {
        cs::Outcome::Status(status) => status,
        cs::Outcome::ThreadExit => panic!("no call here exits"),
    }
}

fn decoded_values() {
    let calls = Calls { holds: true, log: Mutex::new(Vec::new()) };
    let run = |number: u64, args: [u64; 6]| status(cs::dispatch(&calls, number, args));
    // Scalars keep their types: the signed value arrives sign-extended.
    let minus_two = -2i64 as u64;
    assert_eq!(run(1, [200, minus_two, 7, minus_two, 1, 1]), 0);
    assert_eq!(run(1, [200, 40_000, 7, 0, 1, 1]), k::Error::InvalidArgs.raw(), "i16 out of range");
    assert_eq!(run(1, [200, minus_two, 7, 0, 1, (-5i64) as u64]), 0, "a negative enum value");
    assert_eq!(run(2, [0x11, 5, 9, 0, 0, 0]), 0);
    assert_eq!(run(2, [0, 5, 9, 0, 0, 0]), k::Error::InvalidArgs.raw(), "@nonzero bits");
    assert_eq!(run(2, [0x11, 5, 0, 0, 0, 0]), k::Error::InvalidArgs.raw(), "@nonzero u64");
    assert_eq!(run(2, [0x201, 5, 9, 0, 0, 0]), 0, "bit 9, from minor 1");
    assert_eq!(run(2, [0x400, 5, 9, 0, 0, 0]), k::Error::InvalidArgs.raw(), "an undeclared bit");
    // Pointers and counts.
    assert_eq!(run(3, [0x1000, 32, 0x2000, 8, 0, 0x3000]), 0);
    assert_eq!(run(3, [0x1000, 33, 0x2000, 8, 0, 0x3000]), k::Error::InvalidArgs.raw(), "over the string bound");
    assert_eq!(run(3, [0x1000, 32, 0x2000, 9, 0, 0x3000]), k::Error::InvalidArgs.raw(), "over the vector bound");
    assert_eq!(run(3, [0x1000, 32, 0x2000, 8, 7, 0x3000]), 0, "a handle; zero is absent");
    assert_eq!(run(3, [0x1000, 32, 0x2000, 8, 1 << 32, 0x3000]), k::Error::BadHandle.raw(), "over 32 bits");
    // A result buffer's capacity is clamped, never refused.
    assert_eq!(run(4, [0x1000, 0x2000, 0x3000, 100, 0, 0]), 0);
    assert_eq!(run(6, [5, 0, 0, 0, 0, 0]), 0);
    let log = calls.log.lock().expect("lock").clone();
    assert!(log.contains(&format!("scalars 200 -2 7 -2 true One")), "{log:?}");
    assert!(log.contains(&format!("scalars 200 -2 7 0 true Minus")), "{log:?}");
    assert!(log.contains(&"modes 17 5 9".to_string()), "{log:?}");
    assert!(log.contains(&"modes 513 5 9".to_string()), "{log:?}");
    assert!(log.contains(&"buffers 1000 32 2000 8 32 None 3000".to_string()), "{log:?}");
    assert!(log.contains(&"buffers 1000 32 2000 8 32 Some((7, 0, 0)) 3000".to_string()), "{log:?}");
    assert!(log.contains(&"struct_io 1000 2000 3000 16 16".to_string()), "the capacity 100 is clamped to the bound 16: {log:?}");
    assert!(log.contains(&"rights 5 3 4".to_string()), "need read|write and a channel: {log:?}");

    // A @reserved number answers not_supported without any implementation.
    assert_eq!(run(8, [0; 6]), k::Error::NotSupported.raw());
    assert_eq!(run(99, [0; 6]), k::Error::NotSupported.raw(), "an unknown number");
    // The capability gate.
    let denied = Calls { holds: false, log: Mutex::new(Vec::new()) };
    assert_eq!(status(cs::dispatch(&denied, 5, [0; 6])), k::Error::AccessDenied.raw());
    assert!(denied.log.lock().expect("lock").is_empty());
    assert_eq!(run(5, [0; 6]), 0);
}

fn sized_structs() {
    // `options`: u32 size, u16 a, u32 n, then from minor 1 a u64 b.
    assert_eq!(f::Options::BASE_SIZE, 12);
    assert_eq!(f::Options::SIZE, 24);
    assert_eq!(f::Options::SIZES, [12, 24]);
    assert_eq!(f::Options::SIZE_AT, 0);
    assert_eq!(f::Options::A_AT, 4);
    assert_eq!(f::Options::N_AT, 8);
    assert_eq!(f::Options::B_AT, 16);
    // A parameter's size must be exactly a version's.
    assert!(f::Options::accepts_param_size(12));
    assert!(f::Options::accepts_param_size(24));
    assert!(!f::Options::accepts_param_size(11));
    assert!(!f::Options::accepts_param_size(13));
    assert!(!f::Options::accepts_param_size(25));
    // A result only needs the oldest version's fields.
    assert!(f::Options::accepts_result_size(12));
    assert!(f::Options::accepts_result_size(100));
    assert!(!f::Options::accepts_result_size(11));
    assert_eq!(f::Options::writable(12), 12);
    assert_eq!(f::Options::writable(100), 24);

    // Padding is zeroed even over a buffer that held something else (§5.2).
    let value = f::Options { a: 0x0102, n: 7, b: Some(0x0102_0304_0506_0708) };
    let mut dirty = [0xFFu8; 24];
    dirty[0..4].copy_from_slice(&24u32.to_le_bytes());
    value.encode_fields(&mut dirty, 24).unwrap();
    assert_eq!(f::Options::decode(&dirty, 24, u16::MAX).unwrap(), value);
    assert_eq!(&dirty[6..8], &[0, 0], "the padding after `a`");

    // Encode then decode at both versions.
    let mut buffer = [0u8; 24];
    buffer[0..4].copy_from_slice(&24u32.to_le_bytes());
    value.encode_fields(&mut buffer, 24).unwrap();
    assert_eq!(f::Options::decode(&buffer, 24, u16::MAX).unwrap(), value);
    // The older caller's fields only; b stays absent and the kernel writes nothing past 12.
    let mut short = [0u8; 24];
    value.encode_fields(&mut short, 12).unwrap();
    assert_eq!(&short[12..], &[0u8; 12], "nothing past the caller's size");
    assert_eq!(
        f::Options::decode(&short, 12, u16::MAX).unwrap(),
        f::Options { a: 0x0102, n: 7, b: None }
    );
    // Padding is checked in the version it belongs to.
    let mut padded = buffer;
    padded[6] = 1;
    assert_eq!(f::Options::decode(&padded, 12, u16::MAX), Err(rt::WireError::Padding));
    let mut padded = buffer;
    padded[12] = 1;
    assert_eq!(f::Options::decode(&padded, 12, u16::MAX).map(|_| ()), Ok(()), "beyond this version");
    assert_eq!(f::Options::decode(&padded, 24, u16::MAX), Err(rt::WireError::Padding), "inside this version");
    // A short buffer or a size under the base is refused.
    assert_eq!(f::Options::decode(&buffer[..11], 11, u16::MAX), Err(rt::WireError::Size));
    assert_eq!(f::Options::decode(&buffer[..23], 24, u16::MAX), Err(rt::WireError::Size));

    // A plain struct is checked as a Fixed: its padding and size are exact.
    use oidl_generated::oidl_rt::Fixed;
    let pair = f::Pair { x: 0x0102, y: 3 };
    let mut bytes = [0u8; 4];
    pair.write(&mut bytes).unwrap();
    assert_eq!(bytes, [2, 1, 3, 0]);
    assert_eq!(f::Pair::read(&bytes, 0), Ok(pair));
    bytes[3] = 1;
    assert_eq!(f::Pair::read(&bytes, 0), Err(rt::WireError::Padding));
    assert_eq!(f::Pair::read(&bytes[..3], 0), Err(rt::WireError::Size));
    // An enum is closed, and a bits type's undeclared bits are refused.
    assert_eq!(f::Kind::read(&5i32.to_le_bytes(), 0), Err(rt::WireError::Enum));
    assert_eq!(f::Kind::read(&(-5i32).to_le_bytes(), 0), Ok(f::Kind::Minus));
    assert_eq!(f::Mode::read(&0x0200u16.to_le_bytes(), 0), Err(rt::WireError::Bits), "bit 9 arrived in minor 1");
    assert_eq!(f::Mode::read(&0x0200u16.to_le_bytes(), 1).map(|m| m.bits()), Ok(0x0200));
}

fn strings_are_checked_after_the_copy() {
    assert_eq!(rt::check_string(b"hello"), Ok("hello"));
    assert_eq!(rt::check_string(b""), Ok(""));
    assert_eq!(rt::check_string(&[0xFF, 0xFE]), Err(rt::WireError::Utf8));
    assert_eq!(rt::check_string(b"a\0b"), Err(rt::WireError::Nul));
}

fn main() {
    client_paths();
    strings_are_checked_after_the_copy();
    decoded_values();
    sized_structs();
    println!("syscall api: all checks passed");
}
