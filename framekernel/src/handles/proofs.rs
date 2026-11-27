//! Kani's proofs of the handle table (ADR-0005 P1, P2, P11; Amendment 6, E1; TASKS item 143,
//! step 4; row S73-07). Declared in `handles.rs`, so that it can build any table from its
//! fields. Built only by `cargo kani` (`cfg(kani)`), which no build of the kernel sets.
//!
//! # The invariant, from E1 and P11
//!
//! - **P11:** `live` handles, at most the table's `limit`, itself at most [`MAX_HANDLES`].
//! - **E1:** every slot's generation is from 1 to [`LAST_GENERATION`]. ADR-0005's "Why E1":
//!   "A slot is retired when its generations run out, which is how P1's promise is kept." So a
//!   slot is used once at each generation, its last included, and a handle removed at the last
//!   generation leaves its slot retired: never free again (H7). A handle is its slot's index
//!   plus one above [`GENERATION_BITS`] and its generation below (H1).
//! - **What the induction needs of the representation** (not a promise of the ADR): `live` is
//!   the number of full slots; the free list holds each empty slot below its last generation
//!   once, an empty slot at its last generation at most once (free if it has a use left,
//!   absent if it is retired), and nothing else; and it has room for every slot, so that
//!   `remove`'s push never allocates (`handles.rs`, "Never grows").
//!
//! # What each step keeps, and so what holds for any sequence
//!
//! Every step harness also proves, from the table before it ([`step_keeps`]):
//! - every slot the operation does not name keeps its generation and entry, so no operation
//!   adds a right to another handle (P2);
//! - no slot's generation comes down, and a retired slot stays retired;
//! - `remove` moves its slot to exactly the next generation, or retires it.
//!
//! With these, H3 holds for any sequence: a handle names its slot at one generation, and once
//! it is removed the slot's generation is above it, or the slot is retired, and neither comes
//! back. That is the promise E1 rejects wrapping generations for ("Alternatives to E1").
//!
//! # The method, and what it covers
//!
//! Step 2's method. [`new_is_whole`] proves that `new` establishes the invariant. Each step is
//! proved from **any** table of exactly `N` slots that meets it, for each `N` from 0 to [`S`]:
//! every generation, entry, object, right and quota Kani's, the free list in either order, and
//! each list with the least room the invariant allows (so `insert`'s and `reserve`'s own
//! reservations are what make room afterwards). By induction, the invariant and the step
//! properties hold after any sequence of `insert`, `remove`, `duplicate`, `take_all` and
//! `reserve` from `new`, as long as the table has at most [`S`] slots before each operation.
//! No table past that is proved. The slot count, a batch's length and a reservation's count are
//! fixed per harness: a count Kani chose made each `Vec`'s growth a path of its own, and runs
//! cost ten to a hundred times as much (TASKS item 143's step-4 entry).
//!
//! **`reserve(R)`** is proved by its own contract: `Ok` exactly when the quota has room for `R`,
//! and then both lists have room for every slot the next `R` inserts can add, so (by
//! `try_reserve`'s promise not to allocate within capacity) they allocate nothing; that they
//! then succeed follows from `insert`'s proof that it refuses only at the quota, while the
//! table has at most [`S`] slots before each insert. So for two slots and `R = 2`, the second
//! insert, from three slots, is outside it.
//!
//! **Not proved:** a failed allocation (Kani's allocator never fails, so `NoMemory` is not
//! reached); tables past [`S`] slots, free lists of three entries or more, and batches of three
//! handles or more, so `check_all`'s distinctness is shown for pairs only; `insert`'s and
//! `reserve`'s `MAX_HANDLES` guards, which need 2^24 - 1 slots and are unreached here, so H6's
//! "exactly" holds while `slots.len() + R <= MAX_HANDLES`; `nospec`'s Mask form and the
//! kernel's assembly for it (every lookup here takes the Fence form, whose host stand-in is the
//! identity); concurrency (each table is a process's, behind its lock).
#![forbid(unsafe_code)]

use super::{GENERATION_BITS, HandleTable, LAST_GENERATION, MAX_HANDLES, Need, Slot, Typed};
use alloc::vec::Vec;
use origin_abi::{Error, Handle};

/// The most slots a table holds before the operation a harness proves.
const S: usize = 2;

/// A test object: an identity and a type, both Kani's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Obj {
    id: u8,
    kind: u32,
}

impl Typed for Obj {
    fn object_type(&self) -> u32 {
        self.kind
    }
}

fn any_obj() -> Obj {
    Obj {
        id: kani::any(),
        kind: kani::any(),
    }
}

/// The handle of slot `index` at `generation`, written here from E1, not through `join`.
fn handle_of(index: usize, generation: u64) -> Handle {
    ((index as u64 + 1) << GENERATION_BITS) | generation
}

/// The slot a handle names, from E1 (only for a handle with a slot).
fn index_of(handle: Handle) -> usize {
    ((handle >> GENERATION_BITS) - 1) as usize
}

/// Any table of exactly `N` slots that meets the invariant: each slot's generation and entry
/// Kani's, the free list each empty slot below its last generation and each such slot at its
/// last that Kani chooses, in either order, any quota at least `live`, and each list with the
/// least room the invariant allows.
fn any_table<const N: usize>() -> HandleTable<Obj> {
    let mut slots = Vec::with_capacity(N);
    let mut live = 0;
    let mut i = 0;
    while i < N {
        let generation = kani::any_where(|&g: &u64| (1..=LAST_GENERATION).contains(&g));
        let entry = if kani::any() {
            live += 1;
            Some((any_obj(), kani::any()))
        } else {
            None
        };
        slots.push(Slot { generation, entry });
        i += 1;
    }
    let mut free = Vec::with_capacity(N);
    let mut i = 0;
    while i < N {
        let empty = slots[i].entry.is_none();
        // At its last generation an empty slot is free if it has its last use left, and
        // retired if it has had it: Kani chooses which.
        if empty && (slots[i].generation < LAST_GENERATION || kani::any()) {
            free.push(i as u32);
        }
        i += 1;
    }
    if free.len() == 2 && kani::any() {
        free.swap(0, 1);
    }
    let limit = kani::any_where(|&l: &usize| l >= live && l <= MAX_HANDLES);
    HandleTable {
        slots,
        free,
        limit,
        live,
    }
}

/// How many times `index` is in the free list, walked by index up to [`S`] + 1 entries.
fn times_free(t: &HandleTable<Obj>, index: usize) -> usize {
    let mut count = 0;
    let mut k = 0;
    while k < t.free.len() && k <= S {
        if t.free[k] as usize == index {
            count += 1;
        }
        k += 1;
    }
    count
}

/// The invariant, checked, walking each list by index up to [`S`] + 1 entries (a table the
/// harnesses build, grown by one operation).
fn check_whole(t: &HandleTable<Obj>) {
    kani::assert(
        t.live <= t.limit && t.limit <= MAX_HANDLES,
        "P11: live within the quota",
    );
    kani::assert(
        t.slots.len() <= S + 1 && t.free.len() <= S + 1,
        "the harness's bound",
    );
    kani::assert(
        t.free.capacity() >= t.slots.len(),
        "the free list has room for every slot, so remove never allocates",
    );
    let mut full = 0;
    let mut i = 0;
    while i < t.slots.len() && i <= S {
        let slot = &t.slots[i];
        kani::assert(
            slot.generation >= 1 && slot.generation <= LAST_GENERATION,
            "E1: a generation from 1 to the last",
        );
        let listed = times_free(t, i);
        if slot.entry.is_some() {
            full += 1;
            kani::assert(listed == 0, "a full slot is not free");
        } else if slot.generation == LAST_GENERATION {
            kani::assert(
                listed <= 1,
                "a slot at its last generation is free at most once",
            );
        } else {
            kani::assert(listed == 1, "an empty slot is free, once");
        }
        i += 1;
    }
    kani::assert(full == t.live, "live counts the full slots");
    let mut k = 0;
    while k < t.free.len() && k <= S {
        kani::assert((t.free[k] as usize) < t.slots.len(), "free names slots");
        k += 1;
    }
}

/// How many times `index` is in a picture's free list.
fn listed(p: &Picture, index: usize) -> usize {
    let mut count = 0;
    let mut k = 0;
    while k < p.free_count && k <= S {
        if p.free[k] as usize == index {
            count += 1;
        }
        k += 1;
    }
    count
}

/// What every step keeps, from the table `before` it: every slot it does not name (`named`)
/// unchanged, every generation at least what it was, and a retired slot retired.
fn step_keeps(before: &Picture, t: &HandleTable<Obj>, named: [Option<usize>; 2]) {
    kani::assert(t.slots.len() >= before.slot_count, "no slot is dropped");
    let mut i = 0;
    while i < before.slot_count && i <= S && i < t.slots.len() {
        let (g0, e0) = before.slots[i];
        let (g1, e1) = (t.slots[i].generation, t.slots[i].entry);
        kani::assert(g1 >= g0, "E1: a generation never comes down");
        if named[0] != Some(i) && named[1] != Some(i) {
            kani::assert(
                g1 == g0 && e1 == e0,
                "P2: a slot the operation does not name is unchanged",
            );
        }
        if e0.is_none() && g0 == LAST_GENERATION && listed(before, i) == 0 {
            kani::assert(
                e1.is_none() && g1 == LAST_GENERATION && times_free(t, i) == 0,
                "E1: a retired slot stays retired",
            );
        }
        i += 1;
    }
}

/// What `get` must return, written from P1, E1 and OIDL §7.5's order, from the slots alone.
fn expected(t: &HandleTable<Obj>, handle: Handle, need: Need) -> Result<(Obj, u32), Error> {
    let top = handle >> GENERATION_BITS;
    if top == 0 {
        return Err(Error::BadHandle);
    }
    let index = (top - 1) as usize;
    let generation = handle & LAST_GENERATION;
    match t.slots.get(index) {
        Some(Slot {
            generation: g,
            entry: Some((object, rights)),
        }) if *g == generation => {
            if need.object != 0 && object.kind != need.object {
                Err(Error::WrongType)
            } else if rights & need.rights != need.rights {
                Err(Error::AccessDenied)
            } else {
                Ok((*object, *rights))
            }
        }
        _ => Err(Error::BadHandle),
    }
}

fn any_need() -> Need {
    Need {
        rights: kani::any(),
        object: kani::any(),
    }
}

/// A copy of what a table holds, to compare before and after: at most [`S`] + 1 slots and
/// free entries, in fixed arrays, so that comparing walks no further than the table runs.
#[derive(Clone, Copy)]
struct Picture {
    slots: [(u64, Option<(Obj, u32)>); S + 1],
    slot_count: usize,
    free: [u32; S + 1],
    free_count: usize,
    limit: usize,
    live: usize,
}

impl PartialEq for Picture {
    /// Field by field, each list compared element by element up to its count (a derived
    /// comparison compares the arrays' bytes, which unrolls far past the tables' size).
    fn eq(&self, other: &Self) -> bool {
        if self.slot_count != other.slot_count
            || self.free_count != other.free_count
            || self.limit != other.limit
            || self.live != other.live
        {
            return false;
        }
        let mut i = 0;
        while i < self.slot_count && i <= S {
            if self.slots[i] != other.slots[i] {
                return false;
            }
            i += 1;
        }
        let mut i = 0;
        while i < self.free_count && i <= S {
            if self.free[i] != other.free[i] {
                return false;
            }
            i += 1;
        }
        true
    }
}

fn picture(t: &HandleTable<Obj>) -> Picture {
    let mut p = Picture {
        slots: [(0, None); S + 1],
        slot_count: t.slots.len(),
        free: [0; S + 1],
        free_count: t.free.len(),
        limit: t.limit,
        live: t.live,
    };
    let mut i = 0;
    while i < t.slots.len() && i <= S {
        p.slots[i] = (t.slots[i].generation, t.slots[i].entry);
        i += 1;
    }
    let mut i = 0;
    while i < t.free.len() && i <= S {
        p.free[i] = t.free[i];
        i += 1;
    }
    p
}

// ---------------------------------------------------------------- H1: the encoding (E1)

/// `split` undoes `join` for every slot and generation a table keeps; a handle is never 0, and
/// its low 32 bits alone name no slot.
#[kani::proof]
fn the_encoding_is_e1s() {
    let index: usize = kani::any_where(|&i| i < MAX_HANDLES);
    let generation: u64 = kani::any_where(|&g| (1..=LAST_GENERATION).contains(&g));
    let h = super::join(index, generation);
    kani::assert(h == handle_of(index, generation), "the handle E1 describes");
    kani::assert(h != 0, "E1: never 0");
    kani::assert(
        super::split(h) == Some((index, generation)),
        "split undoes join",
    );
    kani::assert(
        super::split(h & 0xFFFF_FFFF).is_none(),
        "E1: the low 32 bits name no slot",
    );
    kani::cover(
        index == MAX_HANDLES - 1 && generation == LAST_GENERATION,
        "the last slot at its last generation",
    );
}

// ---------------------------------------------------------------- the base: new

/// `new` meets the invariant, for every quota, capped at [`MAX_HANDLES`]: the base of the
/// induction (step 2's R143-1).
#[kani::proof]
fn new_is_whole() {
    let asked: usize = kani::any();
    let t = HandleTable::<Obj>::new(asked);
    check_whole(&t);
    kani::assert(t.is_empty() && t.slots.is_empty(), "a new table is empty");
    kani::assert(t.limit == asked.min(MAX_HANDLES), "the quota asked, capped");
    kani::cover(asked > MAX_HANDLES, "a quota past MAX_HANDLES is capped");
}

// ---------------------------------------------------------------- H2: lookup (P1)

/// `get` of any handle with any need: exactly P1's and §7.5's answer, and nothing changes.
fn get_case<const N: usize>() {
    let t = any_table::<N>();
    let (handle, need): (Handle, Need) = (kani::any(), any_need());
    let before = picture(&t);
    let got = t.get(handle, need).map(|(o, r)| (*o, r));
    kani::assert(
        got == expected(&t, handle, need),
        "get is exactly P1's answer",
    );
    kani::assert(picture(&t) == before, "get changes nothing");
    kani::cover(
        if N == 0 { got.is_err() } else { got.is_ok() },
        "a lookup succeeds (on an empty table, is refused)",
    );
}

// ---------------------------------------------------------------- insert: P11, H6

fn insert_case<const N: usize>() {
    let mut t = any_table::<N>();
    let (object, rights) = (any_obj(), kani::any());
    let before = picture(&t);
    let result = t.insert(object, rights);
    match &result {
        Ok(h) => {
            check_whole(&t);
            kani::assert(*h != 0, "E1: never 0");
            kani::assert(t.live == before.live + 1, "one more handle");
            kani::assert(
                expected(&t, *h, Need::NOTHING) == Ok((object, rights)),
                "the new handle names its object with its rights",
            );
            step_keeps(&before, &t, [Some(index_of(*h)), None]);
        }
        Err((e, back)) => {
            kani::assert(*back == object, "the object is handed back");
            kani::assert(
                *e == Error::Limit && before.live >= before.limit,
                "H6: refused only at the quota",
            );
            kani::assert(picture(&t) == before, "a refused insert changes nothing");
        }
    }
    kani::cover(result.is_err(), "an insert is refused at the quota");
    kani::cover(
        if N == 0 {
            result.is_ok()
        } else {
            result.is_ok() && t.slots.len() == N
        },
        "an insert succeeds (on a table with slots, into a free slot)",
    );
}

// ---------------------------------------------------------------- remove: H3, H7

fn remove_case<const N: usize>() {
    let mut t = any_table::<N>();
    let (handle, need): (Handle, Need) = (kani::any(), any_need());
    let before = picture(&t);
    let wanted = expected(&t, handle, need);
    let result = t.remove(handle, need);
    match result {
        Ok(entry) => {
            kani::assert(
                Ok(entry) == wanted,
                "remove takes exactly the entry get names",
            );
            check_whole(&t);
            kani::assert(t.live == before.live - 1, "one fewer handle");
            let index = index_of(handle);
            step_keeps(&before, &t, [Some(index), None]);
            let (g0, _) = before.slots[index];
            if g0 < LAST_GENERATION {
                kani::assert(
                    t.slots[index].generation == g0 + 1 && times_free(&t, index) == 1,
                    "remove moves the slot to the next generation, free",
                );
            } else {
                kani::assert(
                    t.slots[index].generation == LAST_GENERATION && times_free(&t, index) == 0,
                    "H7: a slot used at its last generation is retired",
                );
            }
            kani::assert(
                expected(&t, handle, Need::NOTHING) == Err(Error::BadHandle),
                "H3: the handle is stale at once",
            );
        }
        Err(e) => {
            kani::assert(wanted == Err(e), "remove refuses as get does");
            kani::assert(picture(&t) == before, "a refused remove changes nothing");
        }
    }
    kani::cover(
        if N == 0 {
            result.is_err()
        } else {
            result.is_ok() && handle & LAST_GENERATION == LAST_GENERATION
        },
        "a removal at a slot's last generation (a refusal, on an empty table)",
    );
}

// ---------------------------------------------------------------- duplicate: H4, H6

fn duplicate_case<const N: usize>() {
    let mut t = any_table::<N>();
    let (handle, need, new_rights): (Handle, Need, u32) = (kani::any(), any_need(), kani::any());
    let before = picture(&t);
    let original = expected(&t, handle, need);
    let result = t.duplicate(handle, need, new_rights);
    match result {
        Ok(copy) => {
            let Ok((object, old)) = original else {
                kani::assert(false, "a duplicate only of a handle that meets the need");
                return;
            };
            kani::assert(new_rights & !old == 0, "P2: rights only shrink");
            kani::assert(
                expected(&t, copy, Need::NOTHING) == Ok((object, new_rights)),
                "the copy names the object with exactly the rights asked",
            );
            kani::assert(
                t.live == before.live + 1 && t.live <= t.limit,
                "H6: a duplicate counts toward the quota",
            );
            check_whole(&t);
            // The original's slot among them: duplicating never touches it.
            step_keeps(&before, &t, [Some(index_of(copy)), None]);
        }
        Err(e) => {
            let refused_as_get = original == Err(e);
            let adds_a_right = matches!(original, Ok((_, old)) if new_rights & !old != 0);
            kani::assert(
                refused_as_get
                    || (adds_a_right && e == Error::InvalidArgs)
                    || (original.is_ok() && e == Error::Limit && before.live >= before.limit),
                "a duplicate is refused only as get refuses, for a right added, or at the quota",
            );
            kani::assert(picture(&t) == before, "a refused duplicate changes nothing");
        }
    }
    let shrunk = matches!(original, Ok((_, old)) if new_rights != old);
    kani::cover(
        if N == 0 {
            result.is_err()
        } else {
            result.is_ok() && shrunk
        },
        "a duplicate with fewer rights (a refusal, on an empty table)",
    );
}

// ---------------------------------------------------------------- batches: H5

fn batch_case<const N: usize, const L: usize>() {
    let mut t = any_table::<N>();
    let list: [Handle; L] = kani::any();
    let need = any_need();
    let before = picture(&t);
    let mut wanted = [Err(Error::BadHandle); L];
    let mut k = 0;
    while k < L {
        wanted[k] = expected(&t, list[k], need);
        k += 1;
    }
    let checked = t.check_all(&list, need);
    kani::assert(picture(&t) == before, "check_all changes nothing");
    let result = t.take_all(&list, need);
    match &result {
        Ok(taken) => {
            kani::assert(
                checked.is_ok(),
                "take_all moves only what check_all accepts",
            );
            kani::assert(taken.len() == L, "every handle in the list is moved");
            let mut named = [None; 2];
            let mut k = 0;
            while k < L {
                kani::assert(
                    Ok(taken[k]) == wanted[k],
                    "P2: each moved entry is the one named, in the list's order",
                );
                kani::assert(
                    expected(&t, list[k], Need::NOTHING) == Err(Error::BadHandle),
                    "each moved handle is gone",
                );
                named[k] = Some(index_of(list[k]));
                k += 1;
            }
            check_whole(&t);
            step_keeps(&before, &t, named);
        }
        Err(_) => kani::assert(picture(&t) == before, "a refused batch changes nothing"),
    }
    kani::cover(
        if N < L {
            result.is_err()
        } else {
            result.is_ok()
        },
        "a batch moves (a refusal, where the table holds too few handles)",
    );
}

// ---------------------------------------------------------------- reserve: H6

/// `reserve(R)`: `Ok` exactly when the quota has room for `R` more handles; then it has, both
/// lists have room for every slot the next `R` inserts can add, and no entry changed. Otherwise
/// `Limit`, and nothing changed. (The module's documentation says what follows for the inserts.)
fn reserve_case<const N: usize, const R: usize>() {
    let mut t = any_table::<N>();
    let before = picture(&t);
    let room = before.limit - before.live;
    let result = t.reserve(R);
    kani::assert(
        result.is_ok() == (R <= room),
        "H6: Ok exactly when the quota has room",
    );
    match result {
        Ok(()) => {
            let fresh = R.saturating_sub(before.free_count);
            kani::assert(
                t.free_slots() >= R && picture(&t) == before,
                "H6: the room is there, and no entry changed",
            );
            kani::assert(
                t.slots.capacity() >= t.slots.len() + fresh
                    && t.free.capacity() >= t.slots.len() + fresh,
                "the memory for every reserved insert is taken now",
            );
        }
        Err(e) => {
            kani::assert(e == Error::Limit, "reserve refuses only past the quota");
            kani::assert(picture(&t) == before, "a refused reserve changes nothing");
        }
    }
    kani::cover(result.is_ok(), "the inserts are reserved");
    kani::cover(
        R == 0 || result.is_err(),
        "a reservation past the quota is refused",
    );
}

// ---------------------------------------------------------------- the harnesses, one per size

#[kani::proof]
#[kani::unwind(5)]
fn get_is_exactly_p1_at_0() {
    get_case::<0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn get_is_exactly_p1_at_1() {
    get_case::<1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn get_is_exactly_p1_at_2() {
    get_case::<2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn insert_keeps_the_table_whole_at_0() {
    insert_case::<0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn insert_keeps_the_table_whole_at_1() {
    insert_case::<1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn insert_keeps_the_table_whole_at_2() {
    insert_case::<2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn remove_keeps_the_table_whole_at_0() {
    remove_case::<0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn remove_keeps_the_table_whole_at_1() {
    remove_case::<1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn remove_keeps_the_table_whole_at_2() {
    remove_case::<2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn duplicate_only_shrinks_rights_at_0() {
    duplicate_case::<0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn duplicate_only_shrinks_rights_at_1() {
    duplicate_case::<1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn duplicate_only_shrinks_rights_at_2() {
    duplicate_case::<2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_0_of_1() {
    batch_case::<0, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_0_of_2() {
    batch_case::<0, 2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_1_of_1() {
    batch_case::<1, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_1_of_2() {
    batch_case::<1, 2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_2_of_1() {
    batch_case::<2, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn a_batch_moves_entirely_or_not_at_all_at_2_of_2() {
    batch_case::<2, 2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_0_for_0() {
    reserve_case::<0, 0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_0_for_1() {
    reserve_case::<0, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_0_for_2() {
    reserve_case::<0, 2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_1_for_0() {
    reserve_case::<1, 0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_1_for_1() {
    reserve_case::<1, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_1_for_2() {
    reserve_case::<1, 2>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_2_for_0() {
    reserve_case::<2, 0>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_2_for_1() {
    reserve_case::<2, 1>();
}

#[kani::proof]
#[kani::unwind(5)]
fn reserve_makes_room_at_2_for_2() {
    reserve_case::<2, 2>();
}
