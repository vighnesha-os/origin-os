//! Per-process handle tables (ADR-0005 P1–P2, Amendment 6 E1).
//!
//! A handle is a 64-bit value (E1): the slot index plus one in the top [`SLOT_BITS`] bits and
//! the slot's generation in the low [`GENERATION_BITS`], so a handle is never 0 and a stale
//! handle (its slot freed and reused) never names the new object. How the bits are divided is
//! the kernel's, not the ABI's. The slot is placed above bit 32 on purpose: a value cut to 32
//! bits anywhere names no slot and is `bad_handle`, so a path that still keeps 32 bits of a
//! handle fails at once instead of naming the wrong object. Each entry holds an object and its
//! rights; every lookup states what it needs ([`Need`]), and a duplicate can only remove
//! rights. What a system call needs comes from its definition (`docs/spec/OIDL.md` §6), through
//! the generated decoder: there is no second list of rights here.
//!
//! A slot whose generations are used up is retired rather than reused, so "a stale handle
//! never names a reused slot" holds however long a process runs. With 40 bits that takes about
//! a trillion uses of one slot, and a table has about 16 million slots, so no process meets the
//! limit in its life (E1). The table allocates only with `try_reserve`: a hostile process that
//! fills it gets [`Error::Limit`] or [`Error::NoMemory`], never an allocation failure that stops
//! the kernel.
//!
//! Generic over the object type, safe, tested on the host.

use alloc::vec::Vec;

use origin_abi::{Error, Handle};
use origin_frame::nospec;

/// Bits of a handle that hold its slot's index plus one: the top of the value.
pub const SLOT_BITS: u32 = 24;
/// Bits of a handle that hold its slot's generation: the rest, below the slot.
pub const GENERATION_BITS: u32 = 64 - SLOT_BITS;
/// The last generation a slot can have; a slot that reaches it is retired when it is freed.
const LAST_GENERATION: u64 = (1 << GENERATION_BITS) - 1;
/// Most entries any table can have (the slot field's range, less the 0 that means none).
pub const MAX_HANDLES: usize = (1 << SLOT_BITS) - 1;

/// What a lookup requires of a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    /// Every right the handle must have.
    pub rights: u32,
    /// The object type it must name (a value of `ObjectType`), or 0 for any.
    pub object: u32,
}

impl Need {
    /// Nothing but that the handle is valid.
    pub const NOTHING: Self = Self {
        rights: 0,
        object: 0,
    };

    /// These rights, on an object of any type.
    pub const fn rights(rights: u32) -> Self {
        Self { rights, object: 0 }
    }
}

/// An object a table can hold: it says what type it is, for [`Need::object`].
pub trait Typed {
    /// Its object type, a value of `ObjectType`.
    fn object_type(&self) -> u32;
}

struct Slot<T> {
    generation: u64,
    entry: Option<(T, u32)>,
}

/// One process's handles.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    limit: usize,
    live: usize,
}

impl<T> core::fmt::Debug for HandleTable<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HandleTable")
            .field("live", &self.live)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

/// The slot index and generation a handle names; `None` for a handle with no slot.
fn split(handle: Handle) -> Option<(usize, u64)> {
    let index = (handle >> GENERATION_BITS).checked_sub(1)?;
    Some((usize::try_from(index).ok()?, handle & LAST_GENERATION))
}

/// The handle of `index` at `generation`. `index` is below [`MAX_HANDLES`] and `generation`
/// at most [`LAST_GENERATION`], as the table keeps them.
fn join(index: usize, generation: u64) -> Handle {
    ((index as u64 + 1) << GENERATION_BITS) | (generation & LAST_GENERATION)
}

impl<T> HandleTable<T> {
    /// An empty table that holds at most `limit` handles (the process's quota, P11).
    pub fn new(limit: usize) -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            limit: limit.min(MAX_HANDLES),
            live: 0,
        }
    }

    /// Handles in the table.
    pub const fn len(&self) -> usize {
        self.live
    }

    /// Whether the table is empty.
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many more handles fit.
    pub const fn free_slots(&self) -> usize {
        self.limit.saturating_sub(self.live)
    }

    /// Makes sure `count` further [`HandleTable::insert`]s cannot fail: a handle count over
    /// the quota is [`Error::Limit`], and memory for the slots is taken now.
    pub fn reserve(&mut self, count: usize) -> Result<(), Error> {
        if count > self.free_slots() {
            return Err(Error::Limit);
        }
        let fresh = count.saturating_sub(self.free.len());
        if self.slots.len().saturating_add(fresh) > MAX_HANDLES {
            return Err(Error::Limit);
        }
        self.slots.try_reserve(fresh).map_err(|_| Error::NoMemory)?;
        self.ensure_free_room(fresh)
    }

    /// Makes room in the free list for every slot, plus `fresh` more slots, so that
    /// [`HandleTable::remove`] never allocates.
    fn ensure_free_room(&mut self, fresh: usize) -> Result<(), Error> {
        let want = self.slots.len().saturating_add(fresh);
        let more = want.saturating_sub(self.free.len());
        self.free.try_reserve(more).map_err(|_| Error::NoMemory)
    }

    /// Adds `object` with `rights`; on failure the object is handed back.
    pub fn insert(&mut self, object: T, rights: u32) -> Result<Handle, (Error, T)> {
        if self.live >= self.limit {
            return Err((Error::Limit, object));
        }
        let index = match self.free.pop() {
            Some(i) => i as usize,
            None => {
                if self.slots.len() >= MAX_HANDLES {
                    return Err((Error::Limit, object));
                }
                if self.slots.try_reserve(1).is_err() || self.ensure_free_room(1).is_err() {
                    return Err((Error::NoMemory, object));
                }
                self.slots.push(Slot {
                    generation: 1,
                    entry: None,
                });
                self.slots.len() - 1
            }
        };
        let Some(slot) = self.slots.get_mut(index) else {
            return Err((Error::Limit, object));
        };
        slot.entry = Some((object, rights));
        self.live += 1;
        Ok(join(index, slot.generation))
    }

    /// Every object the table holds, once for each handle, and the table gone: for a process
    /// that has ended, whose bindings are cancelled object by object before its handles close
    /// (ADR-0005 Amendment 7, F11). Allocates nothing.
    pub fn into_objects(self) -> impl Iterator<Item = T> {
        self.slots
            .into_iter()
            .filter_map(|slot| slot.entry.map(|(object, _)| object))
    }
}

impl<T: Typed> HandleTable<T> {
    /// The object behind `handle` and its rights, if the handle is valid and meets `need`. The
    /// object type is checked before the rights, in the order a decoder checks a received
    /// handle (`docs/spec/OIDL.md` §7.5).
    pub fn get(&self, handle: Handle, need: Need) -> Result<(&T, u32), Error> {
        let (index, generation) = split(handle).ok_or(Error::BadHandle)?;
        // The index is a program's. `nospec` keeps it in the table on a wrongly guessed path
        // too (ADR-0017 Z9).
        let index = nospec::index(index, self.slots.len()).ok_or(Error::BadHandle)?;
        let slot = self.slots.get(index).ok_or(Error::BadHandle)?;
        match &slot.entry {
            Some((object, r)) if slot.generation == generation => {
                if need.object != 0 && object.object_type() != need.object {
                    Err(Error::WrongType)
                } else if r & need.rights == need.rights {
                    Ok((object, *r))
                } else {
                    Err(Error::AccessDenied)
                }
            }
            _ => Err(Error::BadHandle),
        }
    }

    /// Removes `handle` if it meets `need`, returning its object and rights.
    pub fn remove(&mut self, handle: Handle, need: Need) -> Result<(T, u32), Error> {
        self.get(handle, need)?;
        let (index, _) = split(handle).ok_or(Error::BadHandle)?;
        let index = nospec::index(index, self.slots.len()).ok_or(Error::BadHandle)?;
        let slot = self.slots.get_mut(index).ok_or(Error::BadHandle)?;
        let entry = slot.entry.take().ok_or(Error::BadHandle)?;
        // A slot whose generations are used up is retired: reusing it would let a stale
        // handle name a later object, which P1 rules out.
        if slot.generation < LAST_GENERATION {
            slot.generation += 1;
            // Never grows: `insert` made room for every slot.
            self.free.push(index as u32);
        }
        self.live -= 1;
        Ok(entry)
    }

    /// Whether every handle in `list` is valid, distinct and meets `need`. Changes nothing: a
    /// caller that also needs the objects can inspect them before it commits with
    /// [`HandleTable::take_all`].
    pub fn check_all(&self, list: &[Handle], need: Need) -> Result<(), Error> {
        for (i, &handle) in list.iter().enumerate() {
            self.get(handle, need)?;
            if list.get(i + 1..).is_some_and(|rest| rest.contains(&handle)) {
                return Err(Error::InvalidArgs);
            }
        }
        Ok(())
    }

    /// Moves every handle in `list` out of the table, or none of them: each must be valid
    /// and distinct and meet `need`.
    pub fn take_all(&mut self, list: &[Handle], need: Need) -> Result<Vec<(T, u32)>, Error> {
        self.check_all(list, need)?;
        let mut taken = Vec::new();
        taken
            .try_reserve_exact(list.len())
            .map_err(|_| Error::NoMemory)?;
        for &handle in list {
            taken.push(self.remove(handle, need)?);
        }
        Ok(taken)
    }
}

impl<T: Typed + Clone> HandleTable<T> {
    /// A new handle to the same object with `new_rights`, which must be a subset of the
    /// original's; the original must meet `need` (the `duplicate` right, for a system call).
    pub fn duplicate(
        &mut self,
        handle: Handle,
        need: Need,
        new_rights: u32,
    ) -> Result<Handle, Error> {
        let (object, old) = self.get(handle, need)?;
        if new_rights & !old != 0 {
            return Err(Error::InvalidArgs);
        }
        let copy = object.clone();
        self.insert(copy, new_rights).map_err(|(e, _)| e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Rng, cases};
    use origin_abi::rights::{self, DUPLICATE, READ, TRANSFER, WRITE};
    use std::collections::HashMap;

    /// The object type every test object has.
    const KIND: u32 = 7;

    impl Typed for i32 {
        fn object_type(&self) -> u32 {
            KIND
        }
    }
    impl Typed for u32 {
        fn object_type(&self) -> u32 {
            KIND
        }
    }
    impl Typed for u64 {
        fn object_type(&self) -> u32 {
            KIND
        }
    }
    impl Typed for &str {
        fn object_type(&self) -> u32 {
            KIND
        }
    }

    fn r(rights: u32) -> Need {
        Need::rights(rights)
    }

    /// A duplicate needing [`DUPLICATE`], as the definition asks of `handle_duplicate`.
    fn dup<T: Typed + Clone>(
        t: &mut HandleTable<T>,
        h: Handle,
        new_rights: u32,
    ) -> Result<Handle, Error> {
        t.duplicate(h, r(DUPLICATE), new_rights)
    }

    #[test]
    fn handles_are_never_zero_and_name_their_object() {
        let mut t = HandleTable::new(8);
        let a = t.insert("a", READ).unwrap();
        let b = t.insert("b", READ | WRITE).unwrap();
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(t.get(a, r(READ)), Ok((&"a", READ)));
        assert_eq!(t.get(b, r(READ | WRITE)).map(|(o, _)| *o), Ok("b"));
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(0, Need::NOTHING), Err(Error::BadHandle));
        assert_eq!(
            t.get(a + 1, Need::NOTHING),
            Err(Error::BadHandle),
            "wrong generation"
        );
        assert_eq!(
            t.get(a ^ (1 << 30), Need::NOTHING),
            Err(Error::BadHandle),
            "another bit of the generation"
        );
    }

    /// E1: a handle fills 64 bits, and its slot lies above bit 32, so a value cut to its low
    /// 32 bits names nothing, whatever the slot and generation.
    #[test]
    fn a_handle_s_low_32_bits_alone_name_nothing() {
        let mut t = HandleTable::new(MAX_HANDLES);
        let mut handles = Vec::new();
        for i in 0..300u32 {
            handles.push(t.insert(i, READ).unwrap());
        }
        for &h in &handles {
            assert!(h > u64::from(u32::MAX), "{h:#x} fits in 32 bits");
            assert_eq!(t.get(h & 0xFFFF_FFFF, Need::NOTHING), Err(Error::BadHandle));
            assert_eq!(t.get(h >> 32, Need::NOTHING), Err(Error::BadHandle));
        }
        // The same holds for a slot reused many times: the generation never reaches the slot.
        let h = handles[7];
        t.remove(h, Need::NOTHING).unwrap();
        let again = t.insert(7, READ).unwrap();
        assert_eq!(
            again >> GENERATION_BITS,
            h >> GENERATION_BITS,
            "the slot is reused"
        );
        assert_eq!(
            t.get(again & 0xFFFF_FFFF, Need::NOTHING),
            Err(Error::BadHandle)
        );
    }

    /// The division E1 leaves to the kernel: about 16 million live handles, a trillion uses
    /// of each slot, and the two fields together fill the value.
    #[test]
    fn the_fields_fill_the_value_and_meet_e1() {
        assert_eq!(SLOT_BITS + GENERATION_BITS, 64);
        assert_eq!(MAX_HANDLES, 0xFF_FFFF);
        assert_eq!(LAST_GENERATION, 0xFF_FFFF_FFFF);
        assert!(MAX_HANDLES >= 16_000_000);
        assert!(LAST_GENERATION >= 1_000_000_000_000);
        // The first and last slot, at the first and last generation, round-trip.
        for (index, generation) in [(0, 1), (0, LAST_GENERATION), (MAX_HANDLES - 1, 1)] {
            let h = join(index, generation);
            assert_ne!(h, 0);
            assert_eq!(split(h), Some((index, generation)));
        }
        assert_eq!(join(MAX_HANDLES - 1, LAST_GENERATION), u64::MAX);
        assert_eq!(split(0), None);
        assert_eq!(split(LAST_GENERATION), None, "no slot");
    }

    #[test]
    fn rights_are_checked_and_only_shrink() {
        let mut t = HandleTable::new(8);
        let h = t.insert(7u32, READ | DUPLICATE).unwrap();
        assert_eq!(t.get(h, r(WRITE)), Err(Error::AccessDenied));
        assert_eq!(
            dup(&mut t, h, READ | WRITE),
            Err(Error::InvalidArgs),
            "cannot add"
        );
        let d = dup(&mut t, h, READ).unwrap();
        assert_eq!(t.get(d, r(READ)), Ok((&7, READ)));
        assert_eq!(
            dup(&mut t, d, READ),
            Err(Error::AccessDenied),
            "no DUPLICATE right"
        );
        assert_eq!(t.remove(h, r(TRANSFER)), Err(Error::AccessDenied));
        assert_eq!(t.remove(h, Need::NOTHING), Ok((7, READ | DUPLICATE)));
    }

    /// An object of one type, for the type checks.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Kinded(u32);

    impl Typed for Kinded {
        fn object_type(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn the_object_type_is_checked_and_before_the_rights() {
        let mut t = HandleTable::new(4);
        let h = t.insert(Kinded(4), READ).unwrap();
        let of = |object| Need { rights: 0, object };
        assert_eq!(t.get(h, of(4)).map(|(o, _)| *o), Ok(Kinded(4)));
        assert_eq!(t.get(h, of(1)), Err(Error::WrongType));
        assert!(t.get(h, Need::NOTHING).is_ok(), "0 means any type");
        // The wrong type and too few rights at once: the type is what is reported.
        assert_eq!(
            t.get(
                h,
                Need {
                    rights: WRITE,
                    object: 1
                }
            ),
            Err(Error::WrongType)
        );
        // The right type and too few rights.
        assert_eq!(
            t.get(
                h,
                Need {
                    rights: WRITE,
                    object: 4
                }
            ),
            Err(Error::AccessDenied)
        );
        // Every lookup goes through the same check.
        assert_eq!(t.remove(h, of(1)), Err(Error::WrongType));
        assert_eq!(t.check_all(&[h], of(1)), Err(Error::WrongType));
        assert_eq!(t.take_all(&[h], of(1)), Err(Error::WrongType));
        assert_eq!(t.duplicate(h, of(1), READ), Err(Error::WrongType));
        assert_eq!(t.len(), 1, "nothing was consumed");
    }

    #[test]
    fn stale_handles_never_reach_a_reused_slot() {
        let mut t = HandleTable::new(4);
        let old = t.insert(1, READ).unwrap();
        t.remove(old, Need::NOTHING).unwrap();
        let new = t.insert(2, READ).unwrap();
        assert_eq!(
            old >> GENERATION_BITS,
            new >> GENERATION_BITS,
            "the slot is reused"
        );
        assert_ne!(old, new, "with a new generation");
        assert_eq!(t.get(old, Need::NOTHING), Err(Error::BadHandle));
        assert_eq!(t.remove(old, Need::NOTHING), Err(Error::BadHandle));
        assert_eq!(t.get(new, Need::NOTHING), Ok((&2, READ)));
    }

    /// Past 2^16 generations, where 32-bit handles retired a slot (E1's reason), the slot is
    /// still in use, and no value repeats.
    #[test]
    fn a_slot_outlives_the_old_limit_of_2_16_generations() {
        let mut t = HandleTable::new(4);
        let mut seen = std::collections::HashSet::new();
        let first = t.insert(0, READ).unwrap();
        t.remove(first, Need::NOTHING).unwrap();
        for i in 1..0x1_0010u32 {
            let h = t.insert(i, READ).unwrap();
            assert_eq!(
                h >> GENERATION_BITS,
                first >> GENERATION_BITS,
                "use {i} took another slot: the first was retired"
            );
            assert!(seen.insert(h), "handle {h:#x} issued twice");
            assert_eq!(t.remove(h, Need::NOTHING), Ok((i, READ)));
            assert_eq!(t.get(h, Need::NOTHING), Err(Error::BadHandle));
            assert_eq!(t.get(first, Need::NOTHING), Err(Error::BadHandle));
        }
        assert_eq!(t.len(), 0);
        assert_eq!(t.slots.len(), 1, "one slot served every use");
    }

    /// A slot whose generations are used up is retired, never reused, so no value can come
    /// back (H2). Driving one slot through 2^40 generations takes too long for a test, so the
    /// slot is set at its last generation and driven past it.
    #[test]
    fn a_slot_is_retired_when_its_generations_are_used_up() {
        let mut t = HandleTable::new(4);
        let h = t.insert(1u32, READ).unwrap();
        t.remove(h, Need::NOTHING).unwrap();
        t.slots[0].generation = LAST_GENERATION - 1;
        let before_last = t.insert(2, READ).unwrap();
        assert_eq!(split(before_last), Some((0, LAST_GENERATION - 1)));
        t.remove(before_last, Need::NOTHING).unwrap();
        let last = t.insert(3, READ).unwrap();
        assert_eq!(
            split(last),
            Some((0, LAST_GENERATION)),
            "the slot's last use"
        );
        t.remove(last, Need::NOTHING).unwrap();
        // The slot is retired: the next handle takes a new one, and neither old value works.
        let next = t.insert(4, READ).unwrap();
        assert_eq!(split(next), Some((1, 1)));
        for stale in [h, before_last, last] {
            assert_eq!(t.get(stale, Need::NOTHING), Err(Error::BadHandle));
        }
        assert_eq!(t.free.len(), 0, "a retired slot is never on the free list");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn an_emptied_table_gives_each_handle_s_object_once() {
        let mut t = HandleTable::new(8);
        let closed = t.insert(1u32, READ).unwrap();
        let kept = t.insert(2, READ | DUPLICATE).unwrap();
        t.insert(3, READ).unwrap();
        t.remove(closed, Need::NOTHING).unwrap();
        // The duplicate takes the freed slot, which then holds it and not the closed object.
        dup(&mut t, kept, READ).unwrap();
        t.insert(4, READ).unwrap();
        let mut objects: Vec<u32> = t.into_objects().collect();
        objects.sort_unstable();
        assert_eq!(objects, vec![2, 2, 3, 4], "a duplicate is one more handle");
        assert_eq!(HandleTable::<u32>::new(4).into_objects().count(), 0);
    }

    #[test]
    fn the_limit_is_a_quota_and_returns_the_object() {
        let mut t = HandleTable::new(2);
        t.insert(1, 0).unwrap();
        let h = t.insert(2, 0).unwrap();
        assert_eq!(t.insert(3, 0), Err((Error::Limit, 3)));
        assert_eq!(t.free_slots(), 0);
        t.remove(h, Need::NOTHING).unwrap();
        assert_eq!(t.free_slots(), 1);
        assert!(t.insert(4, 0).is_ok());
    }

    #[test]
    fn reserve_guarantees_the_inserts_and_refuses_past_the_quota() {
        let mut t = HandleTable::new(3);
        assert_eq!(t.reserve(4), Err(Error::Limit));
        t.reserve(3).unwrap();
        assert_eq!(t.reserve(3), Ok(()));
        let hs: Vec<_> = (0..3).map(|i| t.insert(i, 0).unwrap()).collect();
        assert_eq!(t.reserve(1), Err(Error::Limit));
        t.remove(hs[1], Need::NOTHING).unwrap();
        assert_eq!(t.reserve(1), Ok(()));
        assert!(t.insert(9, 0).is_ok());
    }

    #[test]
    fn a_batch_moves_entirely_or_not_at_all() {
        let mut t = HandleTable::new(8);
        let a = t.insert(1, TRANSFER | READ).unwrap();
        let b = t.insert(2, TRANSFER).unwrap();
        let no_transfer = t.insert(3, READ).unwrap();
        // One handle without the right, or one twice, or one bad: nothing moves.
        assert_eq!(
            t.take_all(&[a, no_transfer], r(TRANSFER)),
            Err(Error::AccessDenied)
        );
        assert_eq!(t.take_all(&[a, b, a], r(TRANSFER)), Err(Error::InvalidArgs));
        assert_eq!(
            t.take_all(&[a, 0x7777_0009_0000_0001], r(TRANSFER)),
            Err(Error::BadHandle)
        );
        assert_eq!(t.check_all(&[a, a], r(TRANSFER)), Err(Error::InvalidArgs));
        assert_eq!(t.len(), 3);
        assert_eq!(t.get(a, r(TRANSFER)).map(|(o, _)| *o), Ok(1));
        // Rights travel with the objects, unchanged.
        assert_eq!(
            t.take_all(&[b, a], r(TRANSFER)),
            Ok(vec![(2, TRANSFER), (1, TRANSFER | READ)])
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(a, Need::NOTHING), Err(Error::BadHandle));
    }

    /// Any sequence of operations, against a map of what should be there. Rights only
    /// shrink, stale values stay dead, and the live count and quota agree.
    #[test]
    fn a_random_operation_sequence_matches_a_model() {
        const LIMIT: usize = 24;
        for seed in 1..=cases(40) as u64 {
            let mut rng = Rng::new(seed);
            let mut table = HandleTable::new(LIMIT);
            let mut model: HashMap<Handle, (u64, u32)> = HashMap::new();
            let mut dead: Vec<Handle> = Vec::new();
            let mut next = 0u64;
            for _ in 0..2000 {
                let live: Vec<Handle> = model.keys().copied().collect();
                let pick = (!live.is_empty()).then(|| live[rng.below(live.len())]);
                match rng.below(8) {
                    0..=2 => {
                        let r = rng.next() as u32 & rights::ALL;
                        next += 1;
                        match table.insert(next, r) {
                            Ok(h) => {
                                assert!(model.insert(h, (next, r)).is_none());
                                assert!(!dead.contains(&h), "a stale value came back");
                            }
                            Err((e, v)) => {
                                assert_eq!((e, v), (Error::Limit, next));
                                assert_eq!(model.len(), LIMIT);
                            }
                        }
                    }
                    3 => {
                        if let Some(h) = pick {
                            let (obj, r) = model[&h];
                            let need = rng.next() as u32 & rights::ALL;
                            let got = table.get(h, Need::rights(need));
                            if r & need == need {
                                assert_eq!(got, Ok((&obj, r)));
                            } else {
                                assert_eq!(got, Err(Error::AccessDenied));
                            }
                        }
                    }
                    4 | 5 => {
                        if let Some(h) = pick {
                            let (obj, r) = model.remove(&h).unwrap();
                            assert_eq!(table.remove(h, Need::NOTHING), Ok((obj, r)));
                            dead.push(h);
                        }
                    }
                    6 => {
                        if let Some(h) = pick {
                            let (obj, r) = model[&h];
                            let wanted = rng.next() as u32 & rights::ALL;
                            let got = dup(&mut table, h, wanted);
                            match got {
                                Ok(d) => {
                                    assert!(r & DUPLICATE != 0);
                                    assert_eq!(wanted & !r, 0, "a duplicate added a right");
                                    assert!(model.insert(d, (obj, wanted)).is_none());
                                }
                                Err(Error::AccessDenied) => assert_eq!(r & DUPLICATE, 0),
                                Err(Error::InvalidArgs) => assert_ne!(wanted & !r, 0),
                                Err(Error::Limit) => assert_eq!(model.len(), LIMIT),
                                Err(e) => panic!("unexpected {e:?}"),
                            }
                        }
                    }
                    _ => {
                        if let Some(&h) = dead.get(rng.below(dead.len().max(1))) {
                            assert_eq!(
                                table.get(h, Need::NOTHING),
                                Err(Error::BadHandle),
                                "stale {h:#x}"
                            );
                            assert_eq!(table.remove(h, Need::NOTHING), Err(Error::BadHandle));
                        }
                    }
                }
                assert_eq!(table.len(), model.len());
                assert_eq!(table.free_slots(), LIMIT - model.len());
            }
        }
    }
}
