//! Per-process handle tables (ADR-0005 P1–P2).
//!
//! A handle is a 32-bit value: the slot index plus one in the low 16 bits and the slot's
//! generation in the high 16 bits, so a handle is never 0 and a stale handle (its slot
//! freed and reused) never names the new object. Each entry holds an object and its rights;
//! every lookup states what it needs ([`Need`]), and a duplicate can only remove rights. What
//! a system call needs comes from its definition (`docs/spec/OIDL.md` §6), through the
//! generated decoder: there is no second list of rights here.
//!
//! A slot whose generations are used up is retired rather than reused, so "a stale handle
//! never names a reused slot" holds however long a process runs. The table allocates only
//! with `try_reserve`: a hostile process that fills it gets [`Error::Limit`] or
//! [`Error::NoMemory`], never an allocation failure that stops the kernel.
//!
//! Generic over the object type, safe, tested on the host.

use alloc::vec::Vec;

use origin_abi::{Error, Handle};
use origin_frame::nospec;

/// Most entries any table can have (the index field's range).
pub const MAX_HANDLES: usize = 0xFFFF;

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
    generation: u16,
    entry: Option<(T, u32)>,
}

/// One process's handles.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u16>,
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

fn split(handle: Handle) -> Option<(usize, u16)> {
    let index = (handle & 0xFFFF).checked_sub(1)?;
    Some((index as usize, (handle >> 16) as u16))
}

fn join(index: usize, generation: u16) -> Handle {
    (u32::from(generation) << 16) | (index as u32 + 1)
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
            Some(i) => usize::from(i),
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
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            // Never grows: `insert` made room for every slot.
            self.free.push(index as u16);
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
            t.get(0xFFFF_0001, Need::NOTHING),
            Err(Error::BadHandle),
            "wrong generation"
        );
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
        assert_eq!(old & 0xFFFF, new & 0xFFFF, "the slot is reused");
        assert_ne!(old, new, "with a new generation");
        assert_eq!(t.get(old, Need::NOTHING), Err(Error::BadHandle));
        assert_eq!(t.remove(old, Need::NOTHING), Err(Error::BadHandle));
        assert_eq!(t.get(new, Need::NOTHING), Ok((&2, READ)));
    }

    #[test]
    fn a_slot_is_retired_before_its_generation_can_repeat() {
        let mut t = HandleTable::new(4);
        let mut seen = std::collections::HashSet::new();
        // Every generation of the one slot, and a few more: none may repeat a value.
        for i in 0..0x1_0010u32 {
            let h = t.insert(i, READ).unwrap();
            assert!(seen.insert(h), "handle {h:#x} issued twice");
            assert_eq!(t.remove(h, Need::NOTHING), Ok((i, READ)));
            assert_eq!(t.get(h, Need::NOTHING), Err(Error::BadHandle));
        }
        assert_eq!(t.len(), 0);
        assert!(t.insert(1, READ).is_ok(), "the table keeps working");
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
            t.take_all(&[a, 0x7777_0009], r(TRANSFER)),
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
