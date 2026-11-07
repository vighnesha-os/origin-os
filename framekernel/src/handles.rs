//! Per-process handle tables (ADR-0005 P1–P2).
//!
//! A handle is a 32-bit value: the slot index plus one in the low 16 bits and the slot's
//! generation in the high 16 bits, so a handle is never 0 and a stale handle (its slot
//! freed and reused) never names the new object. Each entry holds an object and its rights;
//! every lookup states the rights it needs, and a duplicate can only remove rights.
//! Generic over the object type, safe, tested on the host.

use alloc::vec::Vec;

use origin_abi::{Error, Handle, rights};

/// Most entries any table can have (the index field's range).
pub const MAX_HANDLES: usize = 0xFFFF;

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

    /// Adds `object` with `rights`; on failure the object is handed back.
    pub fn insert(&mut self, object: T, rights: u32) -> Result<Handle, (Error, T)> {
        if self.live >= self.limit {
            return Err((Error::Limit, object));
        }
        let index = match self.free.pop() {
            Some(i) => usize::from(i),
            None => {
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

    /// The object behind `handle` and its rights, if it is valid and has every right in
    /// `need`.
    pub fn get(&self, handle: Handle, need: u32) -> Result<(&T, u32), Error> {
        let (index, generation) = split(handle).ok_or(Error::BadHandle)?;
        let slot = self.slots.get(index).ok_or(Error::BadHandle)?;
        match &slot.entry {
            Some((object, r)) if slot.generation == generation => {
                if r & need == need {
                    Ok((object, *r))
                } else {
                    Err(Error::AccessDenied)
                }
            }
            _ => Err(Error::BadHandle),
        }
    }

    /// Removes `handle` if it has every right in `need`, returning its object and rights.
    pub fn remove(&mut self, handle: Handle, need: u32) -> Result<(T, u32), Error> {
        self.get(handle, need)?;
        let (index, _) = split(handle).ok_or(Error::BadHandle)?;
        let slot = self.slots.get_mut(index).ok_or(Error::BadHandle)?;
        let entry = slot.entry.take().ok_or(Error::BadHandle)?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(index as u16);
        self.live -= 1;
        Ok(entry)
    }
}

impl<T: Clone> HandleTable<T> {
    /// A new handle to the same object with `new_rights`, which must be a subset of the
    /// original's; the original needs [`rights::DUPLICATE`].
    pub fn duplicate(&mut self, handle: Handle, new_rights: u32) -> Result<Handle, Error> {
        let (object, old) = self.get(handle, rights::DUPLICATE)?;
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
    use origin_abi::rights::{DUPLICATE, READ, TRANSFER, WRITE};

    #[test]
    fn handles_are_never_zero_and_name_their_object() {
        let mut t = HandleTable::new(8);
        let a = t.insert("a", READ).unwrap();
        let b = t.insert("b", READ | WRITE).unwrap();
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(t.get(a, READ), Ok((&"a", READ)));
        assert_eq!(t.get(b, READ | WRITE).map(|(o, _)| *o), Ok("b"));
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(0, 0), Err(Error::BadHandle));
        assert_eq!(
            t.get(0xFFFF_0001, 0),
            Err(Error::BadHandle),
            "wrong generation"
        );
    }

    #[test]
    fn rights_are_checked_and_only_shrink() {
        let mut t = HandleTable::new(8);
        let h = t.insert(7u32, READ | DUPLICATE).unwrap();
        assert_eq!(t.get(h, WRITE), Err(Error::AccessDenied));
        assert_eq!(
            t.duplicate(h, READ | WRITE),
            Err(Error::InvalidArgs),
            "cannot add"
        );
        let d = t.duplicate(h, READ).unwrap();
        assert_eq!(t.get(d, READ), Ok((&7, READ)));
        assert_eq!(
            t.duplicate(d, READ),
            Err(Error::AccessDenied),
            "no DUPLICATE right"
        );
        assert_eq!(t.remove(h, TRANSFER), Err(Error::AccessDenied));
        assert_eq!(t.remove(h, 0), Ok((7, READ | DUPLICATE)));
    }

    #[test]
    fn stale_handles_never_reach_a_reused_slot() {
        let mut t = HandleTable::new(4);
        let old = t.insert(1, READ).unwrap();
        t.remove(old, 0).unwrap();
        let new = t.insert(2, READ).unwrap();
        assert_eq!(old & 0xFFFF, new & 0xFFFF, "the slot is reused");
        assert_ne!(old, new, "with a new generation");
        assert_eq!(t.get(old, 0), Err(Error::BadHandle));
        assert_eq!(t.remove(old, 0), Err(Error::BadHandle));
        assert_eq!(t.get(new, 0), Ok((&2, READ)));
    }

    #[test]
    fn the_limit_is_a_quota_and_returns_the_object() {
        let mut t = HandleTable::new(2);
        t.insert(1, 0).unwrap();
        let h = t.insert(2, 0).unwrap();
        assert_eq!(t.insert(3, 0), Err((Error::Limit, 3)));
        assert_eq!(t.free_slots(), 0);
        t.remove(h, 0).unwrap();
        assert_eq!(t.free_slots(), 1);
        assert!(t.insert(4, 0).is_ok());
    }
}
