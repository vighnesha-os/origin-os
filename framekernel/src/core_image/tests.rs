//! Host tests of `core_image`: what a description must hold before the kernel is built with it.

use super::*;
use origin_syscalls::origin_kernel::kernel::server::{CALLS, number};

/// Twenty stubs of eight bytes each, call `n`'s at `8 * n`: `mov eax, n`, `syscall`, `ret`.
fn code() -> &'static [u8] {
    let mut bytes = Vec::new();
    for call in CALLS {
        bytes.push(0xB8);
        bytes.extend_from_slice(&(call.number as u32).to_le_bytes());
        bytes.extend_from_slice(&[0x0F, 0x05, 0xC3]);
    }
    bytes.leak()
}

/// The pin of call `n` in [`code`]: five bytes into its stub.
const fn at(n: u64) -> u64 {
    8 * n + 5
}

fn pins() -> Pins {
    Pins {
        thread_exit: at(number::THREAD_EXIT),
        debug_report: at(number::DEBUG_REPORT),
        debug_write: at(number::DEBUG_WRITE),
        handle_close: at(number::HANDLE_CLOSE),
        handle_duplicate: at(number::HANDLE_DUPLICATE),
        channel_create: at(number::CHANNEL_CREATE),
        channel_write: at(number::CHANNEL_WRITE),
        channel_read: at(number::CHANNEL_READ),
        wait_one: at(number::WAIT_ONE),
        event_create: at(number::EVENT_CREATE),
        object_signal: at(number::OBJECT_SIGNAL),
        memory_object_create: at(number::MEMORY_OBJECT_CREATE),
        memory_map: at(number::MEMORY_MAP),
        memory_unmap: at(number::MEMORY_UNMAP),
        memory_protect: at(number::MEMORY_PROTECT),
        port_create: at(number::PORT_CREATE),
        object_wait_async: at(number::OBJECT_WAIT_ASYNC),
        port_wait: at(number::PORT_WAIT),
        port_cancel: at(number::PORT_CANCEL),
        clock_read: at(number::CLOCK_READ),
    }
}

const EXPORTS: &[CoreExport] = &[
    CoreExport {
        name: "origin_kernel_debug_report",
        offset: 8,
        since: (0, 0),
    },
    CoreExport {
        name: "origin_kernel_memory_map",
        offset: 96,
        since: (0, 5),
    },
    CoreExport {
        name: "origin_kernel_thread_exit",
        offset: 0,
        since: (0, 0),
    },
];

fn image() -> CoreImage {
    CoreImage {
        span: PAGE,
        code: code(),
        exports: EXPORTS,
        pins: pins(),
        release: RELEASE_FINGERPRINT,
    }
}

#[test]
fn a_description_of_stubs_passes() {
    assert_eq!(check(&image()), Ok(()));
    assert_eq!(pins().each().len(), CALLS.len());
    // Every pin is its own call's `syscall`, which follows the load of that call's number.
    for (number, offset) in pins().each() {
        let stub = &code()[offset as usize - 5..];
        assert_eq!(stub[0], 0xB8);
        assert_eq!(
            u64::from(u32::from_le_bytes(stub[1..5].try_into().unwrap())),
            number
        );
        assert_eq!(stub[5..7], SYSCALL);
    }
}

#[test]
fn the_span_is_whole_pages_that_hold_the_code() {
    for span in [0, PAGE - 1, PAGE + 8, MAX_SPAN + PAGE] {
        assert_eq!(check(&CoreImage { span, ..image() }), Err(CoreError::Span));
    }
    assert_eq!(
        check(&CoreImage {
            span: MAX_SPAN,
            ..image()
        }),
        Ok(())
    );
    assert_eq!(
        check(&CoreImage {
            code: &[],
            ..image()
        }),
        Err(CoreError::Span)
    );
    let long: &'static [u8] = vec![0xCC; PAGE as usize + 1].leak();
    assert_eq!(
        check(&CoreImage {
            code: long,
            ..image()
        }),
        Err(CoreError::Span)
    );
}

#[test]
fn the_exports_ascend_by_name_inside_the_code() {
    let with = |exports: &'static [CoreExport]| check(&CoreImage { exports, ..image() });
    assert_eq!(with(&[]), Ok(()));
    let outside: &'static [CoreExport] = vec![CoreExport {
        name: "origin_kernel_x",
        offset: code().len() as u64,
        since: (0, 0),
    }]
    .leak();
    assert_eq!(with(outside), Err(CoreError::Export));
    let reversed: &'static [CoreExport] = vec![EXPORTS[1], EXPORTS[0]].leak();
    assert_eq!(with(reversed), Err(CoreError::Export));
    let twice: &'static [CoreExport] = vec![EXPORTS[0], EXPORTS[0]].leak();
    assert_eq!(with(twice), Err(CoreError::Export));
    // A name that is another's beginning sorts first.
    assert!(before("origin_kernel_port", "origin_kernel_port_wait"));
    assert!(!before("origin_kernel_port_wait", "origin_kernel_port"));
    assert!(!before("same", "same"));
    assert!(before("a", "b") && !before("b", "a"));
}

#[test]
fn every_pin_is_a_syscall_of_the_code_and_no_two_calls_share_one() {
    let with = |edit: fn(&mut Pins)| {
        let mut pins = pins();
        edit(&mut pins);
        check(&CoreImage { pins, ..image() })
    };
    // One byte off, either way; the stub's first byte; past the code; the code's last byte.
    assert_eq!(with(|p| p.channel_write += 1), Err(CoreError::Pin));
    assert_eq!(with(|p| p.channel_write -= 1), Err(CoreError::Pin));
    assert_eq!(with(|p| p.thread_exit = 0), Err(CoreError::Pin));
    assert_eq!(with(|p| p.clock_read = 1 << 40), Err(CoreError::Pin));
    assert_eq!(with(|p| p.clock_read = 8 * 20 - 1), Err(CoreError::Pin));
    assert_eq!(with(|p| p.clock_read = u64::MAX), Err(CoreError::Pin));
    // Another call's `syscall`: a real one, and still refused.
    assert_eq!(with(|p| p.memory_map = p.memory_unmap), Err(CoreError::Pin));
    assert_eq!(
        with(|p| p.debug_report = p.thread_exit),
        Err(CoreError::Pin)
    );
    // Code in which a stub lost its `syscall`.
    let mut broken = code().to_vec();
    broken[at(number::WAIT_ONE) as usize] = 0x90;
    assert_eq!(
        check(&CoreImage {
            code: broken.leak(),
            ..image()
        }),
        Err(CoreError::Pin)
    );
}

#[test]
fn stubs_generated_from_another_definition_are_refused() {
    let mut release = RELEASE_FINGERPRINT;
    release[31] ^= 1;
    assert_eq!(
        check(&CoreImage { release, ..image() }),
        Err(CoreError::Release)
    );
    assert_eq!(
        check(&CoreImage {
            release: [0; 32],
            ..image()
        }),
        Err(CoreError::Release)
    );
}
