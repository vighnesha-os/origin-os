//! Loom models of moving a context between two processors' tables of contexts (ADR-0020 S4 and
//! S5; TASKS item 95): `models::moves`, a child of `models` so that it runs under the same
//! `check`, bounds and helpers, kept in a file of its own beside `models.rs`.
//!
//! The real code, compiled in unchanged: `contexts::Moves` (the move gate, the begun and finished
//! counters, `relocate` and the bounded `scan`) and `contexts::Contexts` (`lend`, `adopt`,
//! `Lent::restore`, `Adopted::forget`, `switch` with its deferred publication, and `covers`), with
//! four slots per table and two fast tries per scan in place of 32 and four (`SLOTS` and
//! `FAST_TRIES` under `cfg(all(test, loom))`). What stands in for the machine:
//!
//! - a thread that calls `Contexts::switch` or `Contexts::publish_leaving` on a table is that
//!   table's processor in its trap entry;
//! - a thread that calls `Moves::relocate` is a mover, on any processor; the queue locks a mover
//!   holds around it (ADR-0020 S6) are not modelled, so these models over-approximate what may run
//!   beside a move, which only adds executions;
//! - a thread that calls `Moves::scan` with a pass over the tables in index order is another
//!   processor deciding whether a stack may be given back (`cpu::contexts_cover`); interrupts do
//!   not exist, so its `Quiet` runs the pass as it is;
//! - a relaxed store made after a switch-out and before the processor's next call into its table
//!   is the processor's last use of the outgoing stack, on its way out of the trap entry, as in
//!   the contexts models' L3.
//!
//! **What is proved**, within each model's printed bound, for every interleaving and every value
//! a relaxed load may read in Loom's model of the C11 memory model:
//!
//! - MV1: a scan whose pass reads the destination before the source finds a home that moves
//!   between them, or tries again (N4 (a); ADR-0020 S4).
//! - MV2: a move and the source's own switch naming the same frame: exactly one takes it. When
//!   the move wins, the switch is refused, which on the kernel halts it: TASKS item 96's mover
//!   lends only a thread its source queue says is ready, under that queue's lock, so the source's
//!   scheduler cannot have named the frame (`Moves::relocate`, "What the caller must hold").
//! - MV2b: the same against a switch straight back to the context the source has just switched
//!   out, which publishes the frame and takes it at once.
//! - MV3: a moved context is resumed by the destination only after its `adopt`, and never by the
//!   source after its `lend`: never by both.
//! - MV4: a `forget` followed by another context's claim of the same slot: a scan is never told
//!   the moved stack is uncovered.
//! - MV5: a destination with one free slot, which a publication on it and the move race for:
//!   whichever loses, the move is refused and `restore` gives the source back a frame it resumes,
//!   or the publication is refused.
//! - MV6: two movers contending for the gate beside a scan: neither moving home is missed.
//! - MV7: a scan racing three back-to-back moves of one context, enough for each of its fast
//!   passes to miss the home, finds it, after its pass under the gate if need be. That the scan
//!   makes at most `FAST_TRIES + 1` passes is its code's structure; that the gate's wait ends is
//!   the ticket order's, which Loom's spin bound cannot show (an execution that spins past the
//!   bound is not explored further).
//! - MV8: a context suspended on a stack that is not its home (a measured stack) moves; a scan
//!   asking about that stack, which only the frame word covers, finds it. Begun is raised before
//!   `lend` for this: between `lend` and `adopt` the frame is in neither table.
//! - MV9: the gate admits one holder at a time, and a scan's last pass is one of them: two moves
//!   and a scan whose passes find nothing are never inside the gate together, and each holder
//!   sees what those before it wrote. In the other models a scan's last pass finds the home
//!   whether or not it holds the gate (the delta review of TASKS item 95, D95-2).
//! - LV1: a frame switched out on one processor is resumed by another only after the first
//!   processor's last use of the stack it was saved on (N4 (b); ADR-0020 S5).
//! - LV2: a frame on a stack that is not its context's home is covered while its processor
//!   publishes it, by the leaving record or by its slot (ADR-0020 S5).
//! - LV3: a frame its processor's scheduler has named as the one its running context leaves, and
//!   that the processor is still switching out, is never answered `NotHere` by a mover that found
//!   it named under the queue lock: `lend` reads `current` first and refuses `NotYet` (TASKS item
//!   96; ADR-0020 S5's dated note).
//!
//! **Not proved:** the kernel's mover (TASKS item 96), its queue locks and the run-queue entry it
//! passes; more than two tables, or more than the moves each model makes; that the gate's wait
//! ends (above); and the deterministic bound with the kernel's own four fast tries, which the
//! host test `contexts::tests::a_scan_makes_its_fast_passes_then_one_under_the_gate` shows.
#![forbid(unsafe_code)]

use super::*;

use crate::contexts::{
    AdoptRefused, FAST_TRIES, LendRefused, MoveRefused, MoveWitness, Moves, Quiet, Scanned,
};

/// What MV2 and MV3 report when one frame was taken twice.
const TAKEN_TWICE: &str = "a frame was taken twice: by a move and a switch, or by two switches";
/// What MV5 reports when a frame given back by a refused move could not be resumed at its source.
const NOT_GIVEN_BACK: &str =
    "a frame back from a full destination could not be resumed at its source";
/// What LV1 reports when a frame was resumed elsewhere before its processor's last use of its
/// stack was visible.
const RESUMED_TOO_SOON: &str =
    "a frame was resumed on another processor before its own had left its stack";
/// What MV9 reports when two holders of the move gate were inside it at once.
const GATE_SHARED: &str = "two holders of the move gate at once";
/// What MV9 reports when a holder of the move gate did not see what the one before it wrote.
const GATE_UNORDERED: &str = "a holder of the move gate missed the one before it";
/// What LV3 reports when a frame its processor was still switching out was found in no record.
const CALLED_LOST: &str = "a frame being switched out was called lost";

/// The models' witness that interrupts are off, which `Moves::begin` asks for (TASKS item 96):
/// `()`, since a host thread stands in for a processor with interrupts off.
struct Model;

impl MoveWitness for Model {
    type Off<'a> = ();
}

/// A host thread stands in for a processor with interrupts off: nothing interrupts it.
struct Plain;

impl Quiet for Plain {
    fn quietly<R>(&self, f: impl FnOnce() -> R) -> R {
        f()
    }
}

/// `lend`'s same-stack check answering "not the same stack" for any two addresses, which the
/// models written before `LendRefused::NotYet` pass (TASKS item 96): with it `lend` reads `current`
/// and the running home as the kernel does, and answers as before. The check can only add the
/// refusal `NotYet`, never a take, so the properties those models prove hold with the kernel's
/// check too; LV3 runs it.
fn apart(_: u64, _: u64) -> bool {
    false
}

/// `lend`'s same-stack check for the contexts models' stacks (`ctx_stack`): two addresses in one
/// 64 KiB window of the stacks' area, as `origin_frame::stack::same_slot` answers for the kernel's.
fn one_stack(a: u64, b: u64) -> bool {
    let window = |at: u64| at.checked_sub(0x10_0000).map(|offset| offset / 0x1_0000);
    window(a).is_some() && window(a) == window(b)
}

/// Two tables, index order 0 then 1, shared with the model's threads.
fn two_tables() -> Arc<[Contexts; 2]> {
    Arc::new([Contexts::new(), Contexts::new()])
}

/// A scan of both tables, in index order, for stack `n`, through `moves`' sequence.
fn scan_for(moves: &Moves<Model>, tables: &[Contexts; 2], n: u64) -> Scanned {
    moves.scan(|| tables.iter().any(|table| ctx_covered(table, n)), &Plain)
}

/// MV1: context 1, published on processor 1 and never run, moves to processor 0, while another
/// processor's scan reads processor 0's table (the destination) before processor 1's (the
/// source). Claim before retire alone lets that pass miss the home; the move sequence makes the
/// scan try again, so it always finds it.
#[test]
fn mv1_a_scan_reading_the_destination_first_finds_a_moving_home() {
    check(
        "moves::mv1_a_scan_reading_the_destination_first_finds_a_moving_home",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[1].publish(ctx_frame(1)));
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    let moved =
                        moves.relocate((), &tables[1], &tables[0], ctx_frame(1), apart, |_| ());
                    assert_eq!(moved, Ok((ctx_frame(1), ())));
                })
            };
            assert!(scan_for(&moves, &tables, 1).covered, "{UNCOVERED}");
            mover.join().unwrap();
            assert!(ctx_covered(&tables[0], 1), "{UNCOVERED}");
            assert!(
                !ctx_covered(&tables[1], 1),
                "a moved home stayed in its source"
            );
        },
    );
}

/// MV2: context 1 is published on processor 0 and never run. Processor 0's boot flow switches to
/// it while a mover moves it to processor 1: exactly one of them takes the frame, and the other
/// is refused.
#[test]
fn mv2_a_move_and_the_source_switch_take_a_frame_once() {
    check(
        "moves::mv2_a_move_and_the_source_switch_take_a_frame_once",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[0].publish(ctx_frame(1)));
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    moves.relocate((), &tables[0], &tables[1], ctx_frame(1), apart, |_| ())
                })
            };
            let switched = ctx_switch(&tables[0], ctx_frame(9), ctx_frame(1), false, ctx_off);
            let moved = mover.join().unwrap();
            assert!(
                moved.is_ok() != switched.is_ok(),
                "{TAKEN_TWICE}: moved {moved:?}, switched {switched:?}"
            );
            if moved.is_ok() {
                assert_eq!(switched, Err(Refused::NeverSaved), "{TAKEN_TWICE}");
                assert!(ctx_covered(&tables[1], 1), "{UNCOVERED}");
            } else {
                assert_eq!(
                    moved,
                    Err(MoveRefused::Lend(LendRefused::NotHere)),
                    "{TAKEN_TWICE}"
                );
                assert!(ctx_covered(&tables[0], 1), "{UNCOVERED}");
            }
        },
    );
}

/// MV2b: processor 0 runs context 1, switches it out to its boot flow, and switches straight
/// back to it, the second switch publishing context 1's frame and taking it at once, while a mover
/// moves that frame to processor 1. Exactly one of the switch back and the move takes it.
#[test]
fn mv2b_a_move_and_a_switch_back_to_the_leaving_context_take_a_frame_once() {
    check(
        "moves::mv2b_a_move_and_a_switch_back_to_the_leaving_context_take_a_frame_once",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[0].publish(ctx_frame(1)));
            assert_eq!(
                ctx_switch(&tables[0], ctx_frame(9), ctx_frame(1), false, ctx_off),
                Ok(ctx_frame(1))
            );
            let at_1 = ctx_frame(1) - 0x100;
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    moves.relocate((), &tables[0], &tables[1], at_1, apart, |_| ())
                })
            };
            assert_eq!(
                ctx_switch(&tables[0], at_1, ctx_frame(9), false, ctx_off),
                Ok(ctx_frame(9))
            );
            let back = ctx_switch(&tables[0], ctx_frame(9) - 0x100, at_1, false, ctx_off);
            let moved = mover.join().unwrap();
            assert!(
                moved.is_ok() != back.is_ok(),
                "{TAKEN_TWICE}: moved {moved:?}, switched back {back:?}"
            );
        },
    );
}

/// MV3: context 1 is published on processor 0. A mover moves it to processor 1 while each
/// processor's boot flow tries once to switch to it. The destination resumes it only if the move
/// happened (after its `adopt`), the source only if it did not (before its `lend`), and never
/// both.
#[test]
fn mv3_only_one_processor_resumes_a_moving_context() {
    check(
        "moves::mv3_only_one_processor_resumes_a_moving_context",
        preempted(3, SPINS),
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[0].publish(ctx_frame(1)));
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    moves.relocate((), &tables[0], &tables[1], ctx_frame(1), apart, |_| ())
                })
            };
            let source = {
                let tables = Arc::clone(&tables);
                thread::spawn(move || {
                    ctx_switch(&tables[0], ctx_frame(8), ctx_frame(1), false, ctx_off).is_ok()
                })
            };
            let at_destination =
                ctx_switch(&tables[1], ctx_frame(9), ctx_frame(1), false, ctx_off).is_ok();
            let moved = mover.join().unwrap().is_ok();
            let at_source = source.join().unwrap();
            assert!(!(at_source && at_destination), "{TAKEN_TWICE}");
            assert!(
                !at_destination || moved,
                "{TAKEN_TWICE}: resumed before its adopt"
            );
            assert!(
                !(moved && at_source),
                "{TAKEN_TWICE}: resumed after its lend"
            );
        },
    );
}

/// MV4: context 1 moves from processor 1 to processor 0, and processor 1's freed slot is claimed
/// at once by a new context (stack 4), the table's only free slot once the move has forgotten
/// context 1's home there. A scan reading processor 0 first may read processor 1's slot holding
/// the new claim: it still never answers that stack 1 is uncovered.
#[test]
fn mv4_a_forgotten_slot_claimed_again_hides_no_moved_home() {
    check(
        "moves::mv4_a_forgotten_slot_claimed_again_hides_no_moved_home",
        preempted(3, SPINS),
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[1].publish(ctx_frame(1)));
            for n in 5..8 {
                assert!(tables[1].publish(ctx_frame(n)));
            }
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    let moved =
                        moves.relocate((), &tables[1], &tables[0], ctx_frame(1), apart, |_| ());
                    assert_eq!(moved, Ok((ctx_frame(1), ())));
                })
            };
            let claimer = {
                let tables = Arc::clone(&tables);
                thread::spawn(move || tables[1].publish(ctx_frame(4)))
            };
            assert!(scan_for(&moves, &tables, 1).covered, "{UNCOVERED}");
            mover.join().unwrap();
            let _ = claimer.join().unwrap();
            assert!(ctx_covered(&tables[0], 1), "{UNCOVERED}");
        },
    );
}

/// MV5: processor 1's table has one free slot, which a publication on processor 1 and a move of
/// context 1 from processor 0 race for (N13). Whichever loses is refused: a refused move restores
/// the frame, and processor 0's boot flow then resumes it; a move that won leaves processor 1 to
/// resume it.
#[test]
fn mv5_a_full_destination_gives_the_frame_back() {
    check(
        "moves::mv5_a_full_destination_gives_the_frame_back",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[0].publish(ctx_frame(1)));
            for n in 4..7 {
                assert!(tables[1].publish(ctx_frame(n)));
            }
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    moves.relocate((), &tables[0], &tables[1], ctx_frame(1), apart, |_| ())
                })
            };
            let published = tables[1].publish(ctx_frame(7));
            let moved = mover.join().unwrap();
            assert!(
                moved.is_ok() != published,
                "the last slot went to neither or both: moved {moved:?}, published {published}"
            );
            if moved.is_ok() {
                assert!(ctx_covered(&tables[1], 1), "{UNCOVERED}");
            } else {
                assert_eq!(moved, Err(MoveRefused::Adopt(AdoptRefused::Full)));
                assert_eq!(
                    ctx_switch(&tables[0], ctx_frame(9), ctx_frame(1), false, ctx_off),
                    Ok(ctx_frame(1)),
                    "{NOT_GIVEN_BACK}"
                );
            }
        },
    );
}

/// MV6: two movers contend for the gate, each moving a context from processor 1 to processor 0,
/// while a scan of both tables asks about the first context's stack, then the second's. Neither
/// home is missed.
#[test]
fn mv6_two_movers_and_a_scan_miss_neither_home() {
    check(
        "moves::mv6_two_movers_and_a_scan_miss_neither_home",
        preempted(3, SPINS),
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[1].publish(ctx_frame(1)));
            assert!(tables[1].publish(ctx_frame(2)));
            let movers = [1, 2].map(|n| {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    let moved =
                        moves.relocate((), &tables[1], &tables[0], ctx_frame(n), apart, |_| ());
                    assert_eq!(moved, Ok((ctx_frame(n), ())));
                })
            });
            for n in [1, 2] {
                assert!(scan_for(&moves, &tables, n).covered, "{UNCOVERED}");
            }
            for mover in movers {
                mover.join().unwrap();
            }
        },
    );
}

/// MV7: one mover moves context 1 three times back to back, from processor 1 to 0, back to 1,
/// and to 0 again, while a scan reading processor 0's table first asks about stack 1. Two of the
/// moves are toward the table the scan reads first, so each of its two fast passes can miss the
/// home; the scan then makes its pass under the gate. It always finds the home, within
/// `FAST_TRIES` passes and one more.
#[test]
fn mv7_a_scan_racing_back_to_back_moves_ends_and_finds_the_home() {
    check(
        "moves::mv7_a_scan_racing_back_to_back_moves_ends_and_finds_the_home",
        preempted(4, SPINS),
        preempted(5, SPINS),
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            assert!(tables[1].publish(ctx_frame(1)));
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    for (from, to) in [(1, 0), (0, 1), (1, 0)] {
                        let moved = moves.relocate(
                            (),
                            &tables[from],
                            &tables[to],
                            ctx_frame(1),
                            apart,
                            |_| (),
                        );
                        assert_eq!(moved, Ok((ctx_frame(1), ())));
                    }
                })
            };
            assert!(scan_for(&moves, &tables, 1).covered, "{UNCOVERED}");
            mover.join().unwrap();
        },
    );
}

/// MV8: context 1 ran on processor 1 and was switched out on a measured stack (stack 7), so its
/// saved frame is there and its home on stack 1. It moves to processor 0 while a scan reading
/// processor 0's table first asks about stack 7, which only the frame word covers: between `lend`
/// and `adopt` the frame is in neither table, and the scan must still find it, or try again.
#[test]
fn mv8_a_frame_off_its_home_stack_is_found_while_it_moves() {
    check(
        "moves::mv8_a_frame_off_its_home_stack_is_found_while_it_moves",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            let _ = running_context_1(&tables[1]);
            let on_measured = ctx_stack(7).1 - 0x100;
            assert_eq!(
                ctx_switch(
                    &tables[1],
                    on_measured,
                    ctx_frame(9) - 0x100,
                    false,
                    ctx_off
                ),
                Ok(ctx_frame(9) - 0x100)
            );
            tables[1].publish_leaving();
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    let moved =
                        moves.relocate((), &tables[1], &tables[0], on_measured, apart, |_| ());
                    assert_eq!(moved, Ok((ctx_frame(1), ())));
                })
            };
            assert!(scan_for(&moves, &tables, 7).covered, "{UNCOVERED}");
            mover.join().unwrap();
            assert!(ctx_covered(&tables[0], 7), "{UNCOVERED}");
        },
    );
}

/// MV9's critical section: no other holder inside, and the count of the holders before it read
/// and written back one more, by a Relaxed load and store, so that only the gate orders them.
fn hold_the_gate(inside: &AtomicU64, holders: &AtomicU64) {
    assert_eq!(inside.fetch_add(1, Relaxed), 0, "{GATE_SHARED}");
    let before = holders.load(Relaxed);
    holders.store(before + 1, Relaxed);
    assert_eq!(inside.fetch_sub(1, Relaxed), 1, "{GATE_SHARED}");
}

/// MV9: two movers each hold a move (`Moves::begin`) with nothing in it but the critical
/// section, while a scan whose pass finds nothing makes its passes. While a move is under way the
/// fast passes cannot settle, so the scan makes its last pass, under the gate, and that pass
/// holds the same critical section. No two of the three are ever inside at once, and each sees
/// every holder before it.
#[test]
fn mv9_the_gate_admits_one_holder_at_a_time_and_a_scans_last_pass_is_one() {
    check(
        "moves::mv9_the_gate_admits_one_holder_at_a_time_and_a_scans_last_pass_is_one",
        preempted(3, SPINS),
        EXHAUSTIVE,
        || {
            let moves = Arc::new(Moves::<Model>::new());
            let inside = Arc::new(AtomicU64::new(0));
            let holders = Arc::new(AtomicU64::new(0));
            let movers = [0, 1].map(|_| {
                let (moves, inside, holders) = (
                    Arc::clone(&moves),
                    Arc::clone(&inside),
                    Arc::clone(&holders),
                );
                thread::spawn(move || {
                    let moving = moves.begin(());
                    hold_the_gate(&inside, &holders);
                    drop(moving);
                })
            });
            let passes = Cell::new(0);
            let scanned = moves.scan(
                || {
                    passes.set(passes.get() + 1);
                    if passes.get() > FAST_TRIES {
                        hold_the_gate(&inside, &holders);
                    }
                    false
                },
                &Plain,
            );
            for mover in movers {
                mover.join().unwrap();
            }
            assert!(!scanned.covered);
            let gated = u64::from(scanned.passes > FAST_TRIES);
            assert_eq!(holders.load(Relaxed), 2 + gated, "{GATE_UNORDERED}");
        },
    );
}

/// The boot flow of `table`, on stack 9, has run context 1 and switched back, so context 1 runs
/// nowhere and its frame `at_1` is published; then the boot flow switches to context 1 again.
/// Returns `at_1`.
fn running_context_1(table: &Contexts) -> u64 {
    assert!(table.publish(ctx_frame(1)));
    let at_1 = ctx_frame(1) - 0x100;
    assert_eq!(
        ctx_switch(table, ctx_frame(9), ctx_frame(1), false, ctx_off),
        Ok(ctx_frame(1))
    );
    assert_eq!(
        ctx_switch(table, at_1, ctx_frame(9), false, ctx_off),
        Ok(ctx_frame(9))
    );
    assert_eq!(
        ctx_switch(table, ctx_frame(9) - 0x100, at_1, false, ctx_off),
        Ok(at_1)
    );
    at_1
}

/// LV1: processor 0 runs context 1 and switches it out to its boot flow, then uses context 1's
/// stack once more on its way out of the trap entry, and only then makes its next call into the
/// table. A mover moves the frame to processor 1 as soon as it can, and processor 1 resumes it:
/// processor 1 must see that last use.
#[test]
fn lv1_a_frame_is_resumed_elsewhere_only_after_its_processor_left_the_stack() {
    check(
        "moves::lv1_a_frame_is_resumed_elsewhere_only_after_its_processor_left_the_stack",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            let at_1 = running_context_1(&tables[0]);
            let here = at_1 - 0x100;
            let last_use = Arc::new(AtomicU64::new(0));
            let source = {
                let (tables, last_use) = (Arc::clone(&tables), Arc::clone(&last_use));
                thread::spawn(move || {
                    assert_eq!(
                        ctx_switch(&tables[0], here, ctx_frame(9) - 0x100, false, ctx_off),
                        Ok(ctx_frame(9) - 0x100)
                    );
                    last_use.store(1, Relaxed);
                    tables[0].publish_leaving();
                })
            };
            let mover = {
                let (tables, moves) = (Arc::clone(&tables), Arc::clone(&moves));
                thread::spawn(move || {
                    moves.relocate((), &tables[0], &tables[1], here, apart, |_| ())
                })
            };
            if mover.join().unwrap().is_ok() {
                assert_eq!(
                    ctx_switch(&tables[1], ctx_frame(8), here, false, ctx_off),
                    Ok(here)
                );
                assert_eq!(last_use.load(Relaxed), 1, "{RESUMED_TOO_SOON}");
            }
            source.join().unwrap();
        },
    );
}

/// LV2: processor 0 has just switched context 1 out on a measured stack (stack 7, not its home),
/// so the frame is leaving. Processor 0 publishes it while another processor asks whether stack 7
/// is in use: it is, by the leaving record or by the slot, before, during and after.
#[test]
fn lv2_a_frame_is_covered_while_its_processor_publishes_it() {
    check(
        "moves::lv2_a_frame_is_covered_while_its_processor_publishes_it",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let tables = two_tables();
            let _ = running_context_1(&tables[0]);
            let on_measured = ctx_stack(7).1 - 0x100;
            assert_eq!(
                ctx_switch(
                    &tables[0],
                    on_measured,
                    ctx_frame(9) - 0x100,
                    false,
                    ctx_off
                ),
                Ok(ctx_frame(9) - 0x100)
            );
            let reader = {
                let tables = Arc::clone(&tables);
                thread::spawn(move || assert!(ctx_covered(&tables[0], 7), "{UNCOVERED}"))
            };
            tables[0].publish_leaving();
            reader.join().unwrap();
            assert!(ctx_covered(&tables[0], 7), "{UNCOVERED}");
        },
    );
}

/// LV3 (ADR-0020 S5's dated note; TASKS item 96, the coordinating session's ruling Q2): processor
/// 0 runs context 1. Its scheduler names the frame context 1 leaves, under its queue lock (a spin
/// lock here), and its switch then records the frame as leaving, and its next call into the table
/// publishes it. A mover takes the lock and, if it finds the frame named, lends it under the gate
/// with the real same-stack check, still holding the lock, as the kernel's mover does (ADR-0020
/// S6). Between the naming and the switch the processor still runs on the frame's stack, so the
/// mover may be told `NotYet`, `Leaving`, or be lent the frame, and never `NotHere`: the kernel's
/// mover, which has seen the thread ready and not current under that lock, takes `NotHere` for a
/// lost frame (ADR-0020 S20). A frame lent is put back when its `Lent` is dropped.
#[test]
fn lv3_a_frame_being_switched_out_is_never_called_lost() {
    check(
        "moves::lv3_a_frame_being_switched_out_is_never_called_lost",
        EXHAUSTIVE,
        EXHAUSTIVE,
        || {
            let (tables, moves) = (two_tables(), Arc::new(Moves::<Model>::new()));
            let at_1 = running_context_1(&tables[0]);
            let here = at_1 - 0x100;
            // The frame the scheduler named as context 1's, under its queue lock; 0 until then.
            let named = Arc::new(SpinLock::new(0u64));
            let source = {
                let (tables, named) = (Arc::clone(&tables), Arc::clone(&named));
                thread::spawn(move || {
                    *named.lock() = here;
                    assert_eq!(
                        ctx_switch(&tables[0], here, ctx_frame(9) - 0x100, false, ctx_off),
                        Ok(ctx_frame(9) - 0x100)
                    );
                    tables[0].publish_leaving();
                })
            };
            let mover = {
                let (tables, moves, named) =
                    (Arc::clone(&tables), Arc::clone(&moves), Arc::clone(&named));
                thread::spawn(move || {
                    let named = named.lock();
                    if *named == 0 {
                        return;
                    }
                    let moving = moves.begin(());
                    let lent = tables[0].lend(&moving, *named, one_stack);
                    assert_ne!(
                        lent.as_ref().err(),
                        Some(&LendRefused::NotHere),
                        "{CALLED_LOST}"
                    );
                })
            };
            mover.join().unwrap();
            source.join().unwrap();
        },
    );
}
