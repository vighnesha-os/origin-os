//! Host tests of the pin's decision, with the real `Pins` type and every call of the real
//! definition: each edge the independent review of 2026-11-13 named (finding K2).

use super::*;
use origin_syscalls::origin_kernel::kernel::server::{CALLS, number};

/// Twenty distinct offsets, one a call, as a linked `origin.core` would have them: call `n`'s
/// `syscall` at `8n + 5`.
const fn offset(n: u64) -> u64 {
    8 * n + 5
}

fn pins() -> Pins {
    Pins {
        thread_exit: offset(number::THREAD_EXIT),
        debug_report: offset(number::DEBUG_REPORT),
        debug_write: offset(number::DEBUG_WRITE),
        handle_close: offset(number::HANDLE_CLOSE),
        handle_duplicate: offset(number::HANDLE_DUPLICATE),
        channel_create: offset(number::CHANNEL_CREATE),
        channel_write: offset(number::CHANNEL_WRITE),
        channel_read: offset(number::CHANNEL_READ),
        wait_one: offset(number::WAIT_ONE),
        event_create: offset(number::EVENT_CREATE),
        object_signal: offset(number::OBJECT_SIGNAL),
        memory_object_create: offset(number::MEMORY_OBJECT_CREATE),
        memory_map: offset(number::MEMORY_MAP),
        memory_unmap: offset(number::MEMORY_UNMAP),
        memory_protect: offset(number::MEMORY_PROTECT),
        port_create: offset(number::PORT_CREATE),
        object_wait_async: offset(number::OBJECT_WAIT_ASYNC),
        port_wait: offset(number::PORT_WAIT),
        port_cancel: offset(number::PORT_CANCEL),
        clock_read: offset(number::CLOCK_READ),
    }
}

/// Where the tests put `origin.core`: a page of the programs' region.
const BASE: u64 = 0x0000_0123_4567_8000;
const PINNED: CallOrigin = CallOrigin::Core { base: Some(BASE) };

/// The return address of a `syscall` at `offset` of `origin.core`.
const fn returning_from(offset: u64) -> u64 {
    BASE + offset + 2
}

#[test]
fn each_call_is_accepted_from_its_own_syscall_alone() {
    let pins = pins();
    for call in CALLS {
        assert_eq!(
            judge(
                PINNED,
                &pins,
                call.number,
                returning_from(offset(call.number))
            ),
            Verdict::Accept,
            "{}",
            call.name
        );
        // From every other call's `syscall`: the attack of row E8.
        for other in CALLS.iter().filter(|other| other.number != call.number) {
            let at = offset(other.number);
            assert_eq!(
                judge(PINNED, &pins, call.number, returning_from(at)),
                Verdict::Stop { address: BASE + at },
                "{} from {}'s syscall",
                call.name,
                other.name
            );
        }
    }
}

#[test]
fn a_number_or_an_offset_with_high_bits_set_is_no_calls() {
    let pins = pins();
    for call in CALLS {
        let at = offset(call.number);
        // The right offset, with a number that agrees in its low 32 bits only: what a jump to
        // a stub's `syscall` with a 64-bit RAX could ask, and a stub's `mov eax` cannot.
        for high in [1u64 << 32, 1 << 63, u64::MAX << 32] {
            assert_eq!(
                judge(PINNED, &pins, call.number | high, returning_from(at)),
                Verdict::Stop { address: BASE + at },
                "{} with {high:#x}",
                call.name
            );
        }
        // The right number, with an offset 4 GiB further on.
        let far = returning_from(at).wrapping_add(1 << 32);
        assert_eq!(
            judge(PINNED, &pins, call.number, far),
            Verdict::Stop {
                address: far.wrapping_sub(2)
            }
        );
    }
    // Numbers no method has, at a real stub's `syscall`.
    let at = returning_from(offset(number::DEBUG_REPORT));
    for unknown in [20, 21, 1295, 9999, 1 << 31, 1 << 63, u64::MAX] {
        assert_eq!(
            judge(PINNED, &pins, unknown, at),
            Verdict::Stop { address: at - 2 },
            "{unknown}"
        );
    }
}

#[test]
fn a_pinned_process_with_no_base_has_no_call_accepted() {
    let pins = pins();
    let unmapped = CallOrigin::Core { base: None };
    for call in CALLS {
        // Not even from where its `syscall` would be at any base, 0 among them.
        for rip in [offset(call.number) + 2, returning_from(offset(call.number))] {
            assert_eq!(
                judge(unmapped, &pins, call.number, rip),
                Verdict::Stop { address: rip - 2 }
            );
        }
    }
}

#[test]
fn return_addresses_at_the_edges_wrap_and_are_refused() {
    let pins = pins();
    let n = number::DEBUG_REPORT;
    // Below 2: the subtraction wraps, and with overflow checks on (as in a test build and in
    // the kernel's release build) a plain subtraction would panic here.
    for (rip, address) in [(0, u64::MAX - 1), (1, u64::MAX), (u64::MAX, u64::MAX - 2)] {
        assert_eq!(judge(PINNED, &pins, n, rip), Verdict::Stop { address });
    }
    // Below the base: the offset wraps to a value no stub has.
    for rip in [2, BASE, BASE + 1, BASE - 0x1000] {
        assert_eq!(
            judge(PINNED, &pins, n, rip),
            Verdict::Stop {
                address: rip.wrapping_sub(2)
            }
        );
    }
    // A base at the top of the address space: the sum wraps too, and is still refused.
    let top = CallOrigin::Core {
        base: Some(u64::MAX - 0xFFF),
    };
    assert_eq!(
        judge(top, &pins, n, offset(n)),
        Verdict::Stop {
            address: offset(n) - 2
        }
    );
}

#[test]
fn a_proof_process_is_judged_by_its_range_and_never_stopped() {
    let pins = pins();
    let range = CallOrigin::Range {
        start: 0x40_0000,
        end: 0x40_1000,
    };
    for (rip, verdict) in [
        // The two bytes at the range's first address, and at its last two.
        (0x40_0002, Verdict::Accept),
        (0x40_1000, Verdict::Accept),
        // One byte below, one byte past.
        (0x40_0001, Verdict::Deny),
        (0x40_1001, Verdict::Deny),
        // Below 2 and at the top: refused, without wrapping into the range.
        (0, Verdict::Deny),
        (1, Verdict::Deny),
        (u64::MAX, Verdict::Deny),
    ] {
        for number in [number::DEBUG_REPORT, 9999] {
            assert_eq!(judge(range, &pins, number, rip), verdict, "{rip:#x}");
        }
    }
    // The empty range the proofs' creators are given accepts nothing.
    let none = CallOrigin::Range { start: 0, end: 0 };
    assert_eq!(judge(none, &pins, 0, 2), Verdict::Deny);
}

#[test]
fn origin_core_is_mapped_into_a_pinned_process_once_and_into_no_other() {
    let fresh = CallOrigin::Core { base: None };
    assert_eq!(fresh.mapped_at(BASE), Some(PINNED));
    // A second mapping, and a proof process.
    assert_eq!(PINNED.mapped_at(BASE), None);
    assert_eq!(PINNED.mapped_at(BASE + 0x1000), None);
    let range = CallOrigin::Range {
        start: 0x40_0000,
        end: 0x40_1000,
    };
    assert_eq!(range.mapped_at(BASE), None);
}
