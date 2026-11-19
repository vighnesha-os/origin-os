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
//! **Sync levels** (G2, as ACPICA's default: TASKS 64's ledger §3, which has no lenient mode for
//! them): an acquisition below the current level, a release at another level (ACPI 4.0's rule), or
//! a `Serialized` method entered below it, is an error.
//!
//! **A mutex outlives the method that took it** (G2, step B5; the coordinating session's ruling of
//! 2026-11-19, on TASKS 72's finding that a server's Super I/O helpers take a mutex in one method
//! and release it in another). It is held until the outermost evaluation ends; whatever is still
//! held then is released and reported ([`crate::Report::Released`]), and the evaluation does not
//! fail. A `Serialized` method's own mutex goes when that method ends. The library never
//! serializes a method because it creates names, so no method has its level waived for that. An
//! evaluation that fails releases what it holds without a report: its error is the report; and
//! nothing, whatever `export` ran, outlives an evaluation (B2-6).

use alloc::vec::Vec;

use crate::Interpreter;
use crate::error::{Error, ErrorKind, Limit, Result, err};
use crate::host::{Host, Trace};
use crate::mem;
use crate::object::{NodeId, ObjRef, Object, ScopeKind, Val};

/// A mutex the running evaluation holds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Held {
    /// The mutex object, or `None` for a serialized method's implicit mutex.
    pub(crate) mutex: Option<ObjRef>,
    /// The node `Acquire` named it by, for the report if it is still held at the end.
    pub(crate) node: Option<NodeId>,
    /// The method activation whose implicit mutex this is.
    pub(crate) call: u64,
    pub(crate) sync: u8,
    pub(crate) global: bool,
}

/// `Acquire`'s and `Wait`'s "for ever".
const FOREVER: u64 = 0xFFFF;

/// The longest wait the host is ever asked for, in milliseconds: one below "for ever".
const LONGEST: u64 = 0xFFFE;

impl<H: Host> Interpreter<H> {
    /// The current sync level: the highest held.
    fn sync_level(&self) -> u8 {
        self.note_held_scan();
        self.held.iter().map(|h| h.sync).max().unwrap_or(0)
    }

    /// A scan of the held list: a unit of work an entry, so that no number of holds makes a
    /// return or an acquisition free (the audit of the loops, after N-soak-1).
    fn note_held_scan(&self) {
        self.note_work(u64::try_from(self.held.len()).unwrap_or(u64::MAX));
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
                node: None,
                call,
                sync,
                global: false,
            },
            &mut self.budget,
        )
    }

    /// The planted fault `ReleaseOnReturn`: a method's return releases the mutexes it took, as
    /// before step B5 (the review of B2, B2-10), so that the tests of G2 can be shown to catch it.
    #[cfg(test)]
    pub(crate) fn release_call_holds(&mut self, call: u64) {
        let mut globals = 0u32;
        for h in &self.held {
            if let Some(r) = h.mutex
                && h.call == call
            {
                if let Ok(Object::Mutex(m)) = self.heap.get_mut(r) {
                    m.depth = 0;
                }
                if h.global {
                    globals = globals.saturating_add(1);
                }
            }
        }
        self.held.retain(|h| !(h.mutex.is_some() && h.call == call));
        for _ in 0..globals {
            let _ = self.give_global();
        }
    }

    /// A serialized method returns: its implicit mutex goes.
    pub(crate) fn drop_implicit(&mut self, call: u64) {
        // One entry per activation: `retain` removes it without a panicking index.
        self.note_held_scan();
        self.held.retain(|h| !(h.mutex.is_none() && h.call == call));
    }

    /// The outermost evaluation has ended without an error: what it still holds above `above` is
    /// released, each mutex reported (G2, step B5).
    pub(crate) fn release_reported(&mut self, above: usize) {
        self.note_held_scan();
        let mut i = above;
        while let Some(h) = self.held.get(i).copied() {
            #[cfg(test)]
            let h = Held {
                mutex: h
                    .mutex
                    .filter(|_| !crate::tests::planted(crate::tests::Fault::SilentRelease)),
                ..h
            };
            if h.mutex.is_some() {
                self.tell_released(h.node);
            }
            i = i.saturating_add(1);
        }
        self.release_held(above);
    }

    /// Releases what was taken above the mark `above` (an evaluation ends or is aborted). Returns
    /// whether anything was.
    pub(crate) fn release_held(&mut self, above: usize) -> bool {
        let any = self.held.len() > above;
        while self.held.len() > above {
            let Some(h) = self.held.pop() else { break };
            if let Some(r) = h.mutex
                && let Ok(Object::Mutex(m)) = self.heap.get_mut(r)
            {
                m.depth = 0;
            }
            #[cfg(test)]
            let h = Held {
                global: h.global && !crate::tests::planted(crate::tests::Fault::KeepGlobalLock),
                ..h
            };
            if h.global {
                // Nothing to do on failure: the evaluation is over either way.
                let _ = self.give_global();
            }
        }
        if self.held.is_empty() && self.held.capacity() > 0 {
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

    /// `Acquire`: `Ones` on a timeout, `Zero` when held. `node` is the name it was given by, if
    /// any, for a report should it still be held when the outermost evaluation ends.
    pub(crate) fn acquire(&mut self, v: Val, node: Option<NodeId>, timeout: u64) -> Result<Val> {
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
        if m.global && !self.take_global(timeout)? {
            return Ok(Val::Int(self.width.mask()));
        }
        if let Object::Mutex(m) = self.heap.get_mut(r)? {
            m.depth = 1;
        }
        mem::push(
            &mut self.held,
            Held {
                mutex: Some(r),
                node,
                // The method activation that took it; 0 for load-time code. Its return no longer
                // releases it (G2, step B5).
                call: self.calls.last().map_or(0, |c| c.serial),
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
        // A mutex has one entry, its first acquisition: `retain` removes it without a panicking
        // index.
        self.note_held_scan();
        self.held.retain(|h| h.mutex != Some(r));
        if m.global {
            self.give_global()?;
        }
        Ok(())
    }

    /// Takes the Global Lock around a `Lock` field's access (§19.6.48), waiting for ever: as
    /// long as the time left allows.
    pub(crate) fn lock_global(&mut self) -> Result<()> {
        if self.take_global(FOREVER)? {
            Ok(())
        } else {
            err(ErrorKind::Limit(Limit::Time))
        }
    }

    /// Takes the Global Lock, waiting `timeout_ms` (`FOREVER`: for ever), but never past the
    /// time left nor for 0xFFFF (the review of B1, F9). `false` if the wait asked for timed out; a
    /// wait cut to the time left that times out ends the evaluation instead.
    ///
    /// The evaluation's own holds are counted: the host is asked only for the first, since the
    /// firmware's lock is not recursive (§5.2.10.1), and `Acquire (\_GL)` around a `Lock` field,
    /// or `Lock` fields inside one another, never wait on themselves (the review of B2, B2-5).
    fn take_global(&mut self, timeout_ms: u64) -> Result<bool> {
        if self.global_depth > 0 {
            self.global_depth = self.global_depth.saturating_add(1);
            return Ok(true);
        }
        let left_ms = self.time_for_host()? / 10_000;
        let cut = timeout_ms >= FOREVER || timeout_ms > left_ms;
        let t = if cut { left_ms } else { timeout_ms }.min(LONGEST);
        match self.host.acquire_global_lock(u16::try_from(t).unwrap_or(0)) {
            Ok(true) => {
                self.global_depth = 1;
                Ok(true)
            }
            Ok(false) if cut => err(ErrorKind::Limit(Limit::Time)),
            Ok(false) => Ok(false),
            Err(e) => err(ErrorKind::Host(e)),
        }
    }

    /// The time the evaluation has left for a host call that may wait, in 100 ns units; with
    /// none left, the evaluation ends.
    pub(crate) fn time_for_host(&mut self) -> Result<u64> {
        let used = self.host.now_100ns().saturating_sub(self.run.started);
        match self.config.limits.time_100ns.checked_sub(used) {
            Some(left) if left > 0 => Ok(left),
            _ => err(ErrorKind::Limit(Limit::Time)),
        }
    }

    /// Tells a tracing host of `event`.
    pub(crate) fn trace(&mut self, event: &Trace<'_>) {
        if self.host.tracing() {
            self.host.trace(event);
        }
    }

    /// Tells a tracing host that `node`'s method is entered: its path, written without
    /// allocating.
    pub(crate) fn trace_enter(&mut self, node: NodeId) {
        if self.host.tracing() {
            let mut buf = [[0u8; 4]; 32];
            let path = self.ns.path_into(node, &mut buf).unwrap_or(&[]);
            self.host.trace(&Trace::Enter(path));
        }
    }

    pub(crate) fn unlock_global(&mut self) -> Result<()> {
        self.give_global()
    }

    /// Gives back one of the evaluation's holds of the Global Lock: the last goes to the host.
    fn give_global(&mut self) -> Result<()> {
        match self.global_depth {
            0 => err(ErrorKind::NotOwned),
            1 => {
                self.global_depth = 0;
                self.host
                    .release_global_lock()
                    .map_err(|e| Error::from(ErrorKind::Host(e)))
            }
            n => {
                self.global_depth = n.saturating_sub(1);
                Ok(())
            }
        }
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

    /// `Stall`: at most 255 µs (G7, as ACPICA does: TASKS 64's ledger §3).
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
            return err(ErrorKind::Limit(Limit::Time));
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
