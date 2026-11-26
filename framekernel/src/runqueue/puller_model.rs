//! TASKS item 152: integrity's pulled phase's helpers modelled on the real run queue, a host test.
//!
//! One processor left to pull, its queue as the phase leaves it: slot 0 its idle context; one or
//! two of the phase's threads; and none, one or two of the phase's helpers, in every slot order.
//! The phase's threads run with interrupts on, as in the pulled phase. An odd iteration spins under
//! the word across one or two ticks and then yields. Every even one yields, except that every
//! fourth sleeps two ticks (or yields too, with the control's threads that never sleep). The
//! helpers only yield. A tick also comes at random while a thread runs between its yields.
//!
//! The frame rule is the context table's (ADR-0020 S5; `Contexts::lend`). A thread can be moved
//! only if it is ready, not running, and not the context this processor last switched out. That
//! context becomes movable at the processor's next switch, or at a tick that switches nothing (the
//! kick's publication). Whether one of the phase's threads can be moved is read after every event,
//! and judged over every three consecutive switches.
//!
//! - **With two helpers** ([`two_helpers_leave_a_thread_movable_within_two_switches`]), for one or
//!   two of the phase's threads, sleeping or not: wherever one of them stays awake from a switch to
//!   the second switch after it, one of them can be moved at some point in between.
//! - **The controls** ([`without_helpers_two_alternating_threads_are_never_movable`] and
//!   [`one_helper_leaves_one_thread_never_movable`]): two threads that never sleep and no helper,
//!   or one thread that never sleeps and one helper, are two contexts alternating. Every yield and every tick then
//!   switches, so the waiting one is always the context just switched out, and no thread of the
//!   phase can be moved at any point. That is TASKS item 152's CI failure, and why it takes two
//!   helpers, not one.
#![forbid(unsafe_code)]

use super::*;
extern crate std;
use std::vec::Vec;

/// What a context of the model is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// One of the phase's threads, by index.
    Thread(usize),
    Helper,
}

/// Scheduling events in one case.
const STEPS: usize = 600;
/// A tick between a thread's yields in this many events.
const TICK_IN: u64 = 7;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// A thread of the phase, as the model runs it.
#[derive(Clone, Copy, Default)]
struct Thread {
    iteration: u64,
    /// In the spin of an odd iteration: ticks still to come in it.
    spinning: Option<u64>,
}

/// What one case showed: events after which a thread of the phase could be moved; and the
/// windows of three consecutive switches over which one of them stayed awake, and of those, the
/// ones in which none could ever be moved.
#[derive(Default)]
struct Seen {
    movable: u64,
    windows: u64,
    missed: u64,
}

/// One case: `order` gives the contexts in slot order from slot 1, `phases` the threads'
/// iteration counts at the start, `sleeps` whether every fourth iteration sleeps, and `seed` the
/// ticks' draw.
fn run(order: &[Role], phases: [u64; 2], sleeps: bool, seed: u64) -> Seen {
    let mut rng = Rng(seed);
    let mut q: RunQueue<u32> = RunQueue::new();
    q.set_idle(true);
    let mut role = [None; CAPACITY];
    for (k, &r) in (1u64..).zip(order) {
        if let Ok(slot) = q.push(8 * k)
            && let Some(at) = role.get_mut(slot)
        {
            *at = Some(r);
        }
    }
    let mut threads = [Thread::default(); 2];
    for (t, p) in threads.iter_mut().zip(phases) {
        t.iteration = p;
    }
    let slots: Vec<usize> = (1..CAPACITY)
        .filter(|&s| matches!(role[s], Some(Role::Thread(_))))
        .collect();
    let mut leaving: Option<usize> = None;
    // Per switch count: the threads awake at every event after it (a mask by slot), and whether
    // one of them could be moved after any of those events.
    let (mut awake_all, mut movable_any): (Vec<u32>, Vec<bool>) =
        (std::vec![u32::MAX], std::vec![false]);
    let mut seen = Seen::default();
    for _ in 0..STEPS {
        let before = q.current;
        let f = q.saved[before];
        let mut ticked = false;
        match role[before] {
            None => {
                // The idle context halts until the next tick.
                q.tick(f);
                ticked = true;
            }
            Some(Role::Helper) => {
                if rng.next().is_multiple_of(TICK_IN) {
                    q.tick(f);
                    ticked = true;
                } else {
                    q.yield_now(f);
                }
            }
            Some(Role::Thread(i)) => {
                let t = &mut threads[i];
                match t.spinning {
                    Some(left) if left > 0 => {
                        t.spinning = Some(left - 1);
                        q.tick(f);
                        ticked = true;
                    }
                    Some(_) => {
                        // The spin is over: release the word and yield.
                        t.spinning = None;
                        q.yield_now(f);
                    }
                    None if rng.next().is_multiple_of(TICK_IN) => {
                        q.tick(f);
                        ticked = true;
                    }
                    None => {
                        t.iteration += 1;
                        if !t.iteration.is_multiple_of(2) {
                            // The spin begins, and the first tick comes in it.
                            t.spinning = Some(rng.next() % 2);
                            q.tick(f);
                            ticked = true;
                        } else if sleeps && t.iteration.is_multiple_of(4) {
                            assert!(q.sleep(f, 2).is_ok());
                        } else {
                            q.yield_now(f);
                        }
                    }
                }
            }
        }
        if q.current != before {
            leaving = Some(before);
            awake_all.push(u32::MAX);
            movable_any.push(false);
        } else if ticked {
            // A tick that switched nothing: the kick publishes what was switched out.
            leaving = None;
        }
        let awake = slots
            .iter()
            .filter(|&&s| q.state[s] == State::Ready)
            .fold(0u32, |m, &s| m | 1 << s);
        let movable = slots
            .iter()
            .any(|&s| q.state[s] == State::Ready && s != q.current && leaving != Some(s));
        seen.movable += u64::from(movable);
        if let (Some(a), Some(m)) = (awake_all.last_mut(), movable_any.last_mut()) {
            *a &= awake;
            *m |= movable;
        }
    }
    // Every complete window of three consecutive switch counts (the last is still open).
    let complete = awake_all.len().saturating_sub(1);
    for n in 0..complete.saturating_sub(2) {
        if awake_all[n] & awake_all[n + 1] & awake_all[n + 2] != 0 {
            seen.windows += 1;
            seen.missed += u64::from(!(movable_any[n] || movable_any[n + 1] || movable_any[n + 2]));
        }
    }
    seen
}

/// Every order of `roles`.
fn orders(roles: &[Role]) -> Vec<Vec<Role>> {
    fn go(k: usize, a: &mut Vec<Role>, out: &mut Vec<Vec<Role>>) {
        if k == a.len() {
            out.push(a.clone());
            return;
        }
        for i in k..a.len() {
            a.swap(k, i);
            go(k + 1, a, out);
            a.swap(k, i);
        }
    }
    let mut out = Vec::new();
    go(0, &mut roles.to_vec(), &mut out);
    out
}

/// Every pair of starting iterations, within a cycle of four.
fn phases() -> impl Iterator<Item = [u64; 2]> {
    (0..16u64).map(|p| [p % 4, p / 4])
}

const SEEDS: [u64; 8] = [7919, 104_729, 1_299_709, 15_485_863, 2, 3, 5, 11];

#[test]
fn two_helpers_leave_a_thread_movable_within_two_switches() {
    let (mut cases, mut windows) = (0u64, 0u64);
    for threads in [1, 2] {
        let mut roles: Vec<Role> = (0..threads).map(Role::Thread).collect();
        roles.extend([Role::Helper, Role::Helper]);
        for order in orders(&roles) {
            for p in phases().filter(|p| threads == 2 || p[1] == 0) {
                for sleeps in [true, false] {
                    for seed in SEEDS {
                        let seen = run(&order, p, sleeps, seed);
                        assert_eq!(
                            seen.missed, 0,
                            "{order:?}, phases {p:?}, sleeps {sleeps}, seed {seed}: {} of {} windows had a thread awake throughout and none movable",
                            seen.missed, seen.windows
                        );
                        assert!(seen.movable > 0, "{order:?}: no thread was ever movable");
                        windows += seen.windows;
                        cases += 1;
                    }
                }
            }
        }
    }
    // One thread: 6 orders and 4 phases; two threads: 24 orders and 16 phases; each sleeping or
    // not, with 8 seeds.
    assert_eq!(cases, (6 * 4 + 24 * 16) * 2 * 8);
    std::println!(
        "two helpers: {cases} cases, {windows} windows with a thread awake throughout, none missed"
    );
}

#[test]
fn without_helpers_two_alternating_threads_are_never_movable() {
    let mut windows = 0u64;
    for order in orders(&[Role::Thread(0), Role::Thread(1)]) {
        for p in phases() {
            for seed in SEEDS {
                let seen = run(&order, p, false, seed);
                assert_eq!(
                    seen.movable, 0,
                    "{order:?}, phases {p:?}, seed {seed}: a thread was movable"
                );
                assert_eq!(seen.missed, seen.windows);
                windows += seen.windows;
            }
        }
    }
    assert!(
        windows > 0,
        "the control never kept a thread awake over a window"
    );
    std::println!(
        "no helpers, two threads that never sleep: {windows} windows, a thread movable in none"
    );
}

#[test]
fn one_helper_leaves_one_thread_never_movable() {
    let mut windows = 0u64;
    for order in orders(&[Role::Thread(0), Role::Helper]) {
        for p in phases().filter(|p| p[1] == 0) {
            for seed in SEEDS {
                let seen = run(&order, p, false, seed);
                assert_eq!(
                    seen.movable, 0,
                    "{order:?}, phases {p:?}, seed {seed}: the thread was movable"
                );
                assert_eq!(seen.missed, seen.windows);
                windows += seen.windows;
            }
        }
    }
    assert!(
        windows > 0,
        "the control never kept its thread awake over a window"
    );
    std::println!("one helper, one thread: {windows} windows, the thread movable in none");
}
