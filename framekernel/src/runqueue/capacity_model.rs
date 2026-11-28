//! TASKS item 129: integrity's capacity phase modelled on the real run queue, a host test.
//!
//! One processor's queue as TASKS item 98's turn T2 found it in the stops of the phase. Slot 0 is
//! the boot flow B, which makes every move. Five integrity threads take slots 1 to 5, in every
//! order: two local pairs, (T0, T1) and (T6, T7), each of which passes a turn and blocks for it
//! but yields keeping it every fourth iteration; and T3, whose partner is on another processor and
//! hands the turn back as a remote wake-up after a short random delay. Fillers are blocked in the
//! slots left. Integrity's threads run with interrupts off, so only their own yields and blocks
//! switch them out. B tries a move, yields, and then has its interrupt window: a remote
//! wake-up's preemption, or a tick at random. A move needs its thread ready and not running.
//!
//! - **Before the fix,** B moved T0. In some slot orders and iteration phases, the one moment T0 is
//!   ready and not running comes after its own yield, and a thread a stride behind then runs and
//!   its round-robin tie hands the processor back to T0 before B; B finds T0 blocked at every try
//!   ([`the_unfixed_phase_can_miss_its_thread_at_every_try`], the model's control).
//! - **With the fix,** B moves the phase's own thread C, which only yields: B runs only while C
//!   does not, so C is ready at every try, in every order and phase
//!   ([`the_phases_own_thread_is_ready_at_every_try`]).
#![forbid(unsafe_code)]

use super::*;
extern crate std;
use std::vec::Vec;

/// The integrity threads by index: T0, T1, T6, T7, T3, and the phase's own thread C.
const T0: usize = 0;
const T3: usize = 4;
const C: usize = 5;
/// Scheduling events in one case.
const STEPS: usize = 600;
/// A tick at B's window in this many, and the remote partner's delay up to this many events.
const TICK_IN: u64 = 10;
const REMOTE_MOST: u64 = 5;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// One case: `order` gives the threads in slot order from slot 1 (C among them only if
/// `with_c`), `phases` the four paired threads' iteration counts at the start, and `target` the
/// thread B tries to move. Returns B's tries and how many found the target ready.
fn run(order: &[usize], phases: [u64; 4], target: usize) -> (u64, u64) {
    let mut rng = Rng(7919);
    let mut q: RunQueue<u32> = RunQueue::new();
    let mut slot = [0usize; 6];
    for (k, &t) in (2u64..).zip(order) {
        if let (Some(s), Ok(at)) = (slot.get_mut(t), q.push(8 * k)) {
            *s = at;
        }
    }
    let mut fillers = Vec::new();
    for k in 0..CAPACITY - 1 - order.len() {
        fillers.extend(q.push(1024 + 8 * u64::try_from(k).unwrap_or(0)).ok());
    }
    // The fillers, T0 (pair 0's turn is T1's) and T7 (pair 1's is T6's) block first.
    let mut first: Vec<usize> = fillers;
    first.extend([slot[T0], slot[3]]);
    for s in first {
        for _ in 0..64 {
            if q.current == s {
                break;
            }
            let f = q.saved[q.current];
            q.yield_now(f);
        }
        assert_eq!(q.current, s, "slot {s} never ran to block");
        let f = q.saved[q.current];
        assert!(q.block(f).is_ok());
    }
    let mut iteration = [phases[0], phases[1], phases[2], phases[3], 1];
    let mut remote: Option<u64> = None;
    let (mut pending, mut at_window) = (false, false);
    let (mut tries, mut found) = (0u64, 0u64);
    for _ in 0..STEPS {
        if let Some(left) = remote {
            if left == 0 {
                remote = None;
                let g = q.generation(slot[T3]);
                q.wake(slot[T3], g);
                pending = true;
            } else {
                remote = Some(left - 1);
            }
        }
        let f = q.saved[q.current];
        match (0..6).find(|&t| slot[t] != 0 && q.current == slot[t]) {
            None => {
                if at_window {
                    at_window = false;
                    if pending {
                        pending = false;
                        q.preempt_for_wake(f);
                        continue;
                    }
                    if rng.next().is_multiple_of(TICK_IN) {
                        q.tick(f);
                        continue;
                    }
                }
                tries += 1;
                found += u64::from(q.state[slot[target]] == State::Ready);
                q.yield_now(f);
                at_window = true;
            }
            Some(C) => {
                q.yield_now(f);
            }
            Some(t) => {
                iteration[t] += 1;
                if iteration[t].is_multiple_of(4) {
                    q.yield_now(f);
                } else if t == T3 {
                    remote = Some(1 + rng.next() % REMOTE_MOST);
                    assert!(q.block(f).is_ok());
                } else {
                    let partner = t ^ 1;
                    if matches!(q.state[slot[partner]], State::Blocked(_)) {
                        let g = q.generation(slot[partner]);
                        q.wake(slot[partner], g);
                    }
                    assert!(q.block(f).is_ok());
                }
            }
        }
    }
    (tries, found)
}

/// Every order of the five integrity threads.
fn orders() -> Vec<[usize; 5]> {
    fn go(k: usize, a: &mut [usize; 5], out: &mut Vec<[usize; 5]>) {
        if k == a.len() {
            out.push(*a);
            return;
        }
        for i in k..a.len() {
            a.swap(k, i);
            go(k + 1, a, out);
            a.swap(k, i);
        }
    }
    let mut out = Vec::new();
    go(0, &mut [0, 1, 2, 3, 4], &mut out);
    out
}

/// Every iteration phase of the four paired threads.
fn phases() -> impl Iterator<Item = [u64; 4]> {
    (0..256u64).map(|p| [p % 4, p / 4 % 4, p / 16 % 4, p / 64])
}

#[test]
fn the_phases_own_thread_is_ready_at_every_try() {
    let mut cases = 0u64;
    for five in orders() {
        let order = [five[0], five[1], five[2], five[3], five[4], C];
        for p in phases() {
            let (tries, found) = run(&order, p, C);
            assert!(tries > 0, "order {order:?}, phases {p:?}: B never ran");
            assert_eq!(
                found, tries,
                "order {order:?}, phases {p:?}: C was not ready at a try"
            );
            cases += 1;
        }
    }
    assert_eq!(cases, 30_720);
}

#[test]
fn the_unfixed_phase_can_miss_its_thread_at_every_try() {
    let (mut never, mut orders_trapping) = (0u64, 0u64);
    for order in orders() {
        let trapped = phases()
            .filter(|&p| {
                let (tries, found) = run(&order, p, T0);
                tries > 0 && found == 0
            })
            .count();
        never += u64::try_from(trapped).unwrap_or(u64::MAX);
        orders_trapping += u64::from(trapped > 0);
    }
    assert!(
        never > 0,
        "the model never trapped the unfixed phase: it no longer shows what TASKS item 129 fixed"
    );
    std::println!(
        "the unfixed phase: {never} of 30720 cases never found T0 ready, in {orders_trapping} of 120 slot orders"
    );
}
