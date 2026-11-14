//! Mutexes, the Global Lock, events, time, `Notify` and `Fatal` (ACPI 6.6 §19.6.2, §19.6.42,
//! §19.6.47, §19.6.89, §19.6.95, §19.6.117, §19.6.118, §19.6.125, §19.6.127, §19.6.129,
//! §19.6.136, §19.6.149; §5.2.10.1).
//!
//! **One evaluation at a time** (the contract's §8). Every method an evaluation runs belongs to one
//! invocation, so a mutex is never held by another: `Acquire` takes it, or nests if this evaluation
//! holds it already, and fails only on a sync-level violation. `\_GL` and fields with the `Lock`
//! rule also take the Global Lock, through the host, which may time out. A `Wait` with no signal
//! pending can be ended only by another evaluation, so with a timeout it sleeps the timeout and
//! reports one, and without one it is refused ([`ErrorKind::WouldBlock`]).
//!
//! **Sync levels** (G2, strict until the reading session answers): an acquisition below the
//! current level, or a release at another level, is an error; an evaluation that ends holding a
//! mutex has every one released and reports [`ErrorKind::MutexHeld`].

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{ErrorKind, Result, err};
use crate::host::Host;
use crate::mem;
use crate::object::{NodeId, ObjRef, Object, ScopeKind, Val};

/// A mutex the running evaluation holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Held {
    /// The mutex object, or `None` for a serialized method's implicit mutex.
    pub(crate) mutex: Option<ObjRef>,
    /// The method activation whose implicit mutex this is.
    pub(crate) call: u64,
    pub(crate) sync: u8,
    pub(crate) global: bool,
}

/// `Acquire`'s and `Wait`'s "for ever".
const FOREVER: u64 = 0xFFFF;

impl<H: Host> Interpreter<H> {
    /// The current sync level: the highest held.
    fn sync_level(&self) -> u8 {
        self.held.iter().map(|h| h.sync).max().unwrap_or(0)
    }

    /// A serialized method's implicit mutex, taken at its sync level.
    pub(crate) fn hold_implicit(&mut self, call: u64, sync: u8) -> Result<()> {
        if sync < self.sync_level() {
            return err(ErrorKind::SyncLevel);
        }
        mem::push(
            &mut self.held,
            Held {
                mutex: None,
                call,
                sync,
                global: false,
            },
            &mut self.budget,
        )
    }

    /// A serialized method returns: its implicit mutex goes.
    pub(crate) fn drop_implicit(&mut self, call: u64) {
        if let Some(i) = self
            .held
            .iter()
            .rposition(|h| h.mutex.is_none() && h.call == call)
        {
            self.held.remove(i);
        }
    }

    /// Releases everything held (an evaluation ends or is aborted). Returns whether anything was.
    pub(crate) fn release_held(&mut self) -> bool {
        let any = !self.held.is_empty();
        while let Some(h) = self.held.pop() {
            if let Some(r) = h.mutex
                && let Ok(Object::Mutex(m)) = self.heap.get_mut(r)
            {
                m.depth = 0;
            }
            if h.global {
                // Nothing to do on failure: the evaluation is over either way.
                let _ = self.host.release_global_lock();
            }
        }
        if self.held.capacity() > 0 {
            mem::release(core::mem::take(&mut self.held), &mut self.budget);
        }
        any
    }

    fn mutex_of(&self, v: Val) -> Result<(ObjRef, crate::object::Mutex)> {
        let Val::Obj(r) = v else {
            return err(ErrorKind::Type);
        };
        match self.heap.get(r)? {
            Object::Mutex(m) => Ok((r, *m)),
            _ => err(ErrorKind::Type),
        }
    }

    /// `Acquire`: `Ones` on a timeout, `Zero` when held.
    pub(crate) fn acquire(&mut self, v: Val, timeout: u64) -> Result<Val> {
        let (r, m) = self.mutex_of(v)?;
        if m.depth > 0 {
            if let Object::Mutex(m) = self.heap.get_mut(r)? {
                m.depth = m.depth.saturating_add(1);
            }
            return Ok(Val::Int(0));
        }
        if m.sync < self.sync_level() {
            return err(ErrorKind::SyncLevel);
        }
        mem::reserve(&mut self.held, 1, &mut self.budget)?;
        if m.global {
            let t = u16::try_from(timeout.min(FOREVER)).unwrap_or(0xFFFF);
            match self.host.acquire_global_lock(t) {
                Ok(true) => {}
                Ok(false) => return Ok(Val::Int(self.width.mask())),
                Err(e) => return err(ErrorKind::Host(e)),
            }
        }
        if let Object::Mutex(m) = self.heap.get_mut(r)? {
            m.depth = 1;
        }
        mem::push(
            &mut self.held,
            Held {
                mutex: Some(r),
                call: 0,
                sync: m.sync,
                global: m.global,
            },
            &mut self.budget,
        )?;
        Ok(Val::Int(0))
    }

    /// `Release`.
    pub(crate) fn release(&mut self, v: Val) -> Result<()> {
        let (r, m) = self.mutex_of(v)?;
        if m.depth == 0 {
            return err(ErrorKind::NotOwned);
        }
        if m.depth > 1 {
            if let Object::Mutex(m) = self.heap.get_mut(r)? {
                m.depth = m.depth.saturating_sub(1);
            }
            return Ok(());
        }
        if m.sync != self.sync_level() {
            return err(ErrorKind::SyncLevel);
        }
        if let Object::Mutex(m) = self.heap.get_mut(r)? {
            m.depth = 0;
        }
        if let Some(i) = self.held.iter().rposition(|h| h.mutex == Some(r)) {
            self.held.remove(i);
        }
        if m.global {
            self.host.release_global_lock().map_err(ErrorKind::Host)?;
        }
        Ok(())
    }

    /// Takes the Global Lock around a `Lock` field's access (§19.6.48), waiting for ever.
    pub(crate) fn lock_global(&mut self) -> Result<()> {
        match self.host.acquire_global_lock(0xFFFF) {
            Ok(true) => Ok(()),
            Ok(false) => err(ErrorKind::Host(crate::HostError::Timeout)),
            Err(e) => err(ErrorKind::Host(e)),
        }
    }

    pub(crate) fn unlock_global(&mut self) -> Result<()> {
        self.host
            .release_global_lock()
            .map_err(|e| ErrorKind::Host(e).into())
    }

    fn event_of(&self, v: Val) -> Result<ObjRef> {
        match v {
            Val::Obj(r) if matches!(self.heap.get(r)?, Object::Event(_)) => Ok(r),
            _ => err(ErrorKind::Type),
        }
    }

    /// `Signal`.
    pub(crate) fn signal(&mut self, v: Val) -> Result<()> {
        let r = self.event_of(v)?;
        if let Object::Event(n) = self.heap.get_mut(r)? {
            *n = n.saturating_add(1);
        }
        Ok(())
    }

    /// `Reset`.
    pub(crate) fn reset(&mut self, v: Val) -> Result<()> {
        let r = self.event_of(v)?;
        if let Object::Event(n) = self.heap.get_mut(r)? {
            *n = 0;
        }
        Ok(())
    }

    /// `Wait`: `Zero` if a signal was taken, `Ones` on a timeout.
    pub(crate) fn wait(&mut self, v: Val, timeout: u64) -> Result<Val> {
        let r = self.event_of(v)?;
        if let Object::Event(n) = self.heap.get_mut(r)?
            && *n > 0
        {
            *n = n.saturating_sub(1);
            return Ok(Val::Int(0));
        }
        if timeout >= FOREVER {
            return err(ErrorKind::WouldBlock);
        }
        self.sleep(timeout)?;
        Ok(Val::Int(self.width.mask()))
    }

    /// `Sleep`: the time is charged before it runs; one past the time limit is refused.
    pub(crate) fn sleep(&mut self, ms: u64) -> Result<()> {
        self.charge_time(ms.saturating_mul(10_000))?;
        self.host.sleep_ms(ms);
        self.check_time()
    }

    /// `Stall`: at most 255 µs (G7, strict until the reading session answers).
    pub(crate) fn stall(&mut self, us: u64) -> Result<()> {
        let us = u8::try_from(us).map_err(|_| ErrorKind::Range)?;
        self.charge_time(u64::from(us).saturating_mul(10))?;
        self.host.stall_us(us);
        self.check_time()
    }

    /// Refuses a wait of `t` (100 ns units) that the time limit cannot hold.
    fn charge_time(&mut self, t: u64) -> Result<()> {
        let now = self.host.now_100ns();
        let used = now.saturating_sub(self.run.started);
        if used.saturating_add(t) > self.config.limits.time_100ns {
            return err(ErrorKind::Limit(crate::Limit::Time));
        }
        Ok(())
    }

    /// `Timer`.
    pub(crate) fn timer(&mut self) -> Val {
        Val::Int(self.host.now_100ns() & self.width.mask())
    }

    /// `Notify`: to a device, processor or thermal zone (§19.6.95).
    pub(crate) fn notify(&mut self, node: NodeId, value: u64) -> Result<()> {
        let obj = self.ns.obj(node)?;
        match self.heap.get(obj)? {
            Object::Scope {
                kind: ScopeKind::Device | ScopeKind::Processor { .. } | ScopeKind::ThermalZone,
                ..
            } => {}
            _ => return err(ErrorKind::Type),
        }
        let mut path = Vec::new();
        let r = self.ns.path(node, &mut path, &mut self.budget);
        if r.is_ok() {
            self.host.notify(&path, value);
        }
        mem::release(path, &mut self.budget);
        r
    }

    /// `Fatal`: reported to the host, then the evaluation ends with it.
    pub(crate) fn fatal(&mut self, kind: u8, code: u32, arg: u64) -> Result<()> {
        self.host.fatal(kind, code, arg);
        err(ErrorKind::Fatal { kind, code, arg })
    }
}
