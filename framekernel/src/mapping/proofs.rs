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
//! - **M2:** `check` is exactly [`expected_check`], written from C6 to C8, D2 and the module's
//!   stated order: what is allowed, then the handle's rights for it (read for any mapping, and
//!   write too for a writable one, as M3 pins `needs` to), then whole pages inside the object,
//!   then the placement (anywhere, or a fixed run of whole pages inside the user half). So it
//!   accepts every request those rules allow and refuses every other, with the error they give.
//! - **M3:** `within` is "allows nothing more", and `needs` is monotone in it, with `READ` for
//!   any mapping and `WRITE` exactly for a writable one: the link between M2's rights and the
//!   ABI's.
//!
//! **Not proved here:**
//! - `mapping.rs`'s other host-built logic: `frames`, `place`, `place_at_random`, `price`,
//!   `Budget` (T15, T16) and `Mappings` (C7's whole mappings, D5's `should_wait`). Their host
//!   tests cover them; a proof of them is later work.
//! - `memory_object.rs`'s `protect`, which calls `within`, and the rest of a mapping's work.
//!   They build only for the kernel's target, so Kani cannot compile them on the host; the
//!   boot proofs show them.
//! - `nospec`'s Mask form and the kernel's assembly for it: every lookup here takes the Fence
//!   form, whose host stand-in is the identity. The kept path's result is the same.
#![forbid(unsafe_code)]

use super::{Allow, Asked, PAGE, Place, Request, check};
use origin_abi::rights::{READ, WRITE};
use origin_abi::{Error, map_access, map_flags};
use origin_frame::usercopy::{USER_END, USER_START};

/// `from_bits` for every value: M1, from the definition's `map_access` bits.
#[kani::proof]
fn allow_from_every_bits() {
    let bits: u32 = kani::any();
    let read = bits & map_access::READ != 0;
    let write = bits & map_access::WRITE != 0;
    let execute = bits & map_access::EXECUTE != 0;
    let unknown = bits & !map_access::ALL != 0;
    match Allow::from_bits(bits) {
        Ok(allow) => {
            kani::assert(
                !unknown && read && !execute,
                "only defined bits, with read, no execute",
            );
            kani::assert(
                allow == if write { Allow::ReadWrite } else { Allow::Read },
                "what the bits say",
            );
            kani::cover(allow == Allow::ReadWrite, "read and write allowed");
        }
        Err(Error::InvalidArgs) => kani::assert(
            unknown || !read || (write && execute),
            "invalid_args only for an unknown bit, no read, or write with execute",
        ),
        Err(Error::AccessDenied) => kani::assert(
            !unknown && read && execute && !write,
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

/// What `check` must return, written from C6 to C8, D2 and the module's order, not through
/// `check`'s helpers: what is allowed (M1 proves `from_bits` against the bits), the rights for
/// it, the pages, the placement.
fn expected_check(request: &Request, object_pages: u64, held: u32) -> Result<Asked, Error> {
    let allow = Allow::from_bits(request.access)?;
    let needs = match allow {
        Allow::Read => READ,
        Allow::ReadWrite => READ | WRITE,
    };
    if held & needs != needs {
        return Err(Error::AccessDenied);
    }
    if request.offset % PAGE != 0 || request.length % PAGE != 0 {
        return Err(Error::InvalidArgs);
    }
    let (first, pages) = (request.offset / PAGE, request.length / PAGE);
    if pages == 0
        || first
            .checked_add(pages)
            .is_none_or(|end| end > object_pages)
    {
        return Err(Error::InvalidArgs);
    }
    let place = match request.flags {
        0 if request.address == 0 => Place::Anywhere,
        flags if flags == map_flags::FIXED => {
            let base = request.address;
            let inside = base % PAGE == 0
                && base >= USER_START
                && pages
                    .checked_mul(PAGE)
                    .and_then(|bytes| base.checked_add(bytes))
                    .is_some_and(|end| end <= USER_END);
            if !inside {
                return Err(Error::InvalidArgs);
            }
            Place::At(base)
        }
        _ => return Err(Error::InvalidArgs),
    };
    Ok(Asked {
        allow,
        first,
        pages,
        place,
    })
}

/// `check` for every request, object size and rights: P1 and M2, exactly.
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
    let got = check(&request, object_pages, held);
    kani::assert(
        got == expected_check(&request, object_pages, held),
        "check is exactly what C6 to C8 and D2 allow, in the module's order",
    );
    kani::cover(
        matches!(
            got,
            Ok(Asked {
                place: Place::At(_),
                ..
            })
        ),
        "a fixed mapping is accepted",
    );
    kani::cover(
        matches!(
            got,
            Ok(Asked {
                place: Place::Anywhere,
                ..
            })
        ),
        "a mapping placed anywhere is accepted",
    );
    kani::cover(
        got == Err(Error::AccessDenied),
        "a request past the rights is refused",
    );
}
