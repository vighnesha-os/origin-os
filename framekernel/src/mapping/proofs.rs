//! Kani's proofs of a map request's checks (ADR-0005 P7 as C6, C7 and C8 decide; TASKS item
//! 143, step 4; row S73-07). Built only by `cargo kani` (`cfg(kani)`), which no build of the
//! kernel sets.
//!
//! # What is proved
//!
//! For every request, every object size and every set of rights the handle holds:
//! - **M1:** `Allow::from_bits` refuses a bit past the three defined, write with execute and
//!   no read as `invalid_args`, and execute as `access_denied`, and otherwise gives `Read` or
//!   `ReadWrite` as the bits say.
//! - **M2:** `check` is `Ok` only when the handle holds every right the mapping allows, the
//!   pages are whole and inside the object, and a fixed address's run is inside the user half,
//!   page-aligned; it refuses in the order the module states: what is allowed, the rights, the
//!   range, the placement.
//! - **M3:** `within` is "allows nothing more", and `needs` is monotone in it.
//!
//! **Not proved here:** `memory_object.rs`'s `protect`, which calls `within`, and the rest of
//! a mapping's work. They build only for the kernel's target, so Kani cannot compile them on
//! the host; the boot proofs show them.
#![forbid(unsafe_code)]

use super::{Allow, PAGE, Place, Request, check};
use origin_abi::rights::{READ, WRITE};
use origin_abi::{Error, map_access, map_flags};
use origin_frame::usercopy::{USER_END, USER_START};

/// `from_bits` for every value: M1, from the definition's `map_access` bits.
#[kani::proof]
fn allow_from_every_bits() {
    let bits: u32 = kani::any();
    let (read, write, execute) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
    match Allow::from_bits(bits) {
        Ok(allow) => {
            kani::assert(
                bits & !7 == 0 && read && !execute,
                "only defined bits, with read, no execute",
            );
            kani::assert(
                allow == if write { Allow::ReadWrite } else { Allow::Read },
                "what the bits say",
            );
            kani::cover(allow == Allow::ReadWrite, "read and write allowed");
        }
        Err(Error::InvalidArgs) => kani::assert(
            bits & !7 != 0 || !read || (write && execute),
            "invalid_args only for an unknown bit, no read, or write with execute",
        ),
        Err(Error::AccessDenied) => kani::assert(
            bits & !7 == 0 && read && execute && !write,
            "access_denied only for execute",
        ),
        Err(_) => kani::assert(false, "no other error"),
    }
}

/// `within` and `needs`: M3.
#[kani::proof]
fn within_is_allows_nothing_more() {
    let pick = |b: bool| if b { Allow::ReadWrite } else { Allow::Read };
    let (a, b) = (pick(kani::any()), pick(kani::any()));
    let writes = |x: Allow| x == Allow::ReadWrite;
    kani::assert(
        a.within(b) == (!writes(a) || writes(b)),
        "within: allows nothing more",
    );
    if a.within(b) {
        kani::assert(a.needs() & !b.needs() == 0, "needs is monotone in within");
    }
    kani::assert(a.needs() & READ != 0, "every mapping needs read");
    kani::assert((a.needs() & WRITE != 0) == writes(a), "write needs write");
    kani::cover(a.within(b) && a != b, "read within read and write");
}

/// `check` for every request, object size and rights: P1 and M2.
#[kani::proof]
fn check_every_request() {
    let request = Request {
        access: kani::any(),
        flags: kani::any(),
        offset: kani::any(),
        length: kani::any(),
        address: kani::any(),
    };
    let object_pages: u64 = kani::any();
    let held: u32 = kani::any();
    let allowed = Allow::from_bits(request.access);
    match check(&request, object_pages, held) {
        Ok(asked) => {
            let Ok(allow) = allowed else {
                kani::assert(false, "Ok only for an access from_bits accepts");
                return;
            };
            kani::assert(asked.allow == allow, "the access asked for");
            kani::assert(
                held & allow.needs() == allow.needs(),
                "the handle holds every right needed",
            );
            kani::assert(
                request.offset % PAGE == 0 && request.length % PAGE == 0,
                "whole pages",
            );
            let (first, pages) = (request.offset / PAGE, request.length / PAGE);
            kani::assert(
                asked.first == first && asked.pages == pages,
                "the pages asked for",
            );
            kani::assert(
                pages > 0 && first.checked_add(pages).is_some_and(|e| e <= object_pages),
                "the pages lie inside the object",
            );
            match asked.place {
                Place::Anywhere => kani::assert(
                    request.flags == 0 && request.address == 0,
                    "anywhere only when asked so",
                ),
                Place::At(base) => {
                    kani::assert(
                        request.flags == map_flags::FIXED && base == request.address,
                        "a fixed place where asked",
                    );
                    kani::assert(
                        base % PAGE == 0
                            && base >= USER_START
                            && pages
                                .checked_mul(PAGE)
                                .and_then(|b| base.checked_add(b))
                                .is_some_and(|e| e <= USER_END),
                        "a fixed run lies in the user half",
                    );
                }
            }
            kani::cover(
                matches!(asked.place, Place::At(_)),
                "a fixed mapping is accepted",
            );
        }
        // The order: what is allowed is judged first, so its refusal is from_bits's own.
        Err(e) => {
            if let Err(first) = allowed {
                kani::assert(e == first, "what is allowed is checked first");
            } else if let Ok(allow) = allowed {
                if held & allow.needs() != allow.needs() {
                    kani::assert(e == Error::AccessDenied, "then the rights");
                } else {
                    kani::assert(e == Error::InvalidArgs, "then the range and the placement");
                }
            }
            let _ = map_access::ALL;
        }
    }
}
