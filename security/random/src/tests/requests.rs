//! Requests: pieces, a thread that moves between steps, and the refusals (ADR-0016 R7, R11).

use super::world::World;
use crate::{Error, Instruction, RESEED_NANOS, fill, fill_fresh, next_u64};

#[test]
fn a_request_is_answered_in_pieces_of_at_most_128_bytes() {
    let mut world = World::new(1);
    world.boot();
    for (len, pieces) in [
        (0usize, 1u64),
        (1, 1),
        (8, 1),
        (64, 1),
        (128, 1),
        (129, 2),
        (300, 3),
    ] {
        let steps = world.steps;
        let mut out = vec![0u8; len];
        fill(&mut world, &mut out).unwrap();
        assert_eq!(world.steps - steps, pieces, "{len} bytes");
        if len >= 16 {
            assert!(out[..8].iter().any(|&b| b != 0), "{len} bytes");
            assert!(out[len - 8..].iter().any(|&b| b != 0), "{len} bytes");
        }
    }
    // Seven requests, eight pieces more than requests... each piece is a request to the
    // mechanism.
    assert_eq!(world.cpus[0].node.requests_since_seed(), 10);
}

#[test]
fn values_differ_between_requests_and_between_processors() {
    let mut world = World::new(4);
    world.boot();
    let mut seen = std::collections::BTreeSet::new();
    for round in 0..50 {
        for cpu in 0..4 {
            world.on = cpu;
            assert!(
                seen.insert(next_u64(&mut world).unwrap()),
                "round {round}, processor {cpu}"
            );
        }
    }
    assert_eq!(seen.len(), 200);
}

#[test]
fn a_thread_that_moves_between_steps_is_still_answered() {
    let mut world = World::new(3);
    world.boot();
    world.now += RESEED_NANOS;
    // After every step the thread is on the next processor.
    world.mover = Some(Box::new(|_, on| (on + 1) % 3));
    let mut total = [0u64; 3];
    for _ in 0..40 {
        let before: Vec<u64> = (0..3)
            .map(|cpu| world.tries(cpu, Instruction::Rdseed))
            .collect();
        let mut out = [0u8; 300];
        fill(&mut world, &mut out).unwrap();
        assert!(out[..8].iter().any(|&b| b != 0));
        assert!(out[292..].iter().any(|&b| b != 0));
        let mut drawn = 0;
        for (cpu, (sum, was)) in total.iter_mut().zip(&before).enumerate() {
            let tries = world.tries(cpu, Instruction::Rdseed) - was;
            *sum += tries;
            drawn += tries;
        }
        assert!(drawn <= 16, "one request drew {drawn}");
    }
    // Every processor's generator was reseeded from its own RDSEED, by whichever requests
    // passed through; no request made more than sixteen draws.
    for (cpu, tries) in total.iter().enumerate() {
        assert!(!world.cpus[cpu].node.due(world.now), "processor {cpu}");
        assert_eq!(
            world.cpus[cpu].node.counts().reseeds_rdseed,
            1,
            "processor {cpu}"
        );
        assert!(*tries >= 8, "processor {cpu}");
    }
    // Prediction resistance takes eight draws made on one processor inside the request. A
    // thread that is somewhere else after every step never has them: it is refused, and
    // nothing it gathered stays.
    let mut out = [0xEE; 64];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 64]);
    // A thread that moves once gathers again where it arrives, and is answered: the draws it
    // left behind are not the root's fresh entropy.
    world.on = 0;
    let moved_at = world.steps + 4;
    world.mover = Some(Box::new(
        move |step, on| if step == moved_at { 1 } else { on },
    ));
    let before = (
        world.tries(0, Instruction::Rdseed),
        world.tries(1, Instruction::Rdseed),
    );
    let fresh = world.root.counts().fresh;
    fill_fresh(&mut world, &mut out).unwrap();
    assert!(out.iter().any(|&b| b != 0));
    assert_eq!(world.tries(0, Instruction::Rdseed) - before.0, 4);
    assert_eq!(
        world.tries(1, Instruction::Rdseed) - before.1,
        8,
        "all eight where it asks"
    );
    assert_eq!(world.root.counts().fresh, fresh + 1);
    // The four draws it left on processor 0 are nobody's fresh entropy, and that processor's
    // own reseed does not take them over either: it makes its eight, and eight for the root.
    assert_eq!(world.cpus[0].node.have, 4);
    world.mover = None;
    world.on = 0;
    world.now += RESEED_NANOS;
    let before = world.tries(0, Instruction::Rdseed);
    next_u64(&mut world).unwrap();
    assert_eq!(world.tries(0, Instruction::Rdseed) - before, 16);
    assert_eq!(world.cpus[0].node.have, 0);
}

#[test]
fn a_fresh_request_that_moves_begins_again_whatever_number_it_holds() {
    // A request with prediction resistance begins on processor 0, the first there, and is on
    // processor 1 after three draws: it leaves them behind in a store with the number 1.
    let mut world = World::new(3);
    world.boot();
    let mut out = [0u8; 16];
    let moved_at = world.steps + 3;
    world.mover = Some(Box::new(
        move |step, on| if step == moved_at { 1 } else { on },
    ));
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.cpus[0].node.have, 3);
    // Another begins on processor 2, the first there, so it too holds the number 1, and is on
    // processor 0 after two draws. It still begins again there: all eight draws are made on
    // processor 0 after it arrived, and the three that lay there are overwritten.
    world.on = 2;
    let moved_at = world.steps + 2;
    world.mover = Some(Box::new(
        move |step, on| if step == moved_at { 0 } else { on },
    ));
    let before = world.tries(0, Instruction::Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Instruction::Rdseed) - before, 8);
    assert_eq!(world.tries(2, Instruction::Rdseed), 1024 + 2);
}

#[test]
fn a_stuck_value_on_one_processor_is_not_held_against_another() {
    use super::world::Rule;
    use crate::Condition;
    // RDSEED alone. Processor 0's gives zero once; the thread is on processor 1 from then
    // on, where RDSEED is not ready.
    let mut world = World::reporting(2, true, false);
    world.boot();
    world.now += RESEED_NANOS;
    let tried = world.tries(0, Instruction::Rdseed);
    world.line(0, Instruction::Rdseed).rules.push(Rule::Value {
        from: tried,
        count: 1,
        value: 0,
    });
    let tried = world.tries(1, Instruction::Rdseed);
    world
        .line(1, Instruction::Rdseed)
        .rules
        .push(Rule::NotReady {
            from: tried,
            count: u64::MAX,
        });
    world.mover = Some(Box::new(|_, _| 1));
    let before = world.tries(1, Instruction::Rdseed);
    next_u64(&mut world).unwrap();
    // One try on processor 0 and 99 on processor 1: one draw's tries. Processor 1's RDSEED
    // was "not now"; it gave no stuck value and has not failed.
    assert_eq!(world.tries(1, Instruction::Rdseed) - before, 99);
    assert_eq!(world.cpus[0].node.set_aside(Instruction::Rdseed), 1);
    assert_eq!(
        world.cpus[1].node.condition(Instruction::Rdseed),
        Condition::Healthy
    );
    assert_eq!(world.cpus[1].node.faults(Instruction::Rdseed), 0);
    assert_eq!(world.cpus[1].node.counts().not_now_rdseed, 1);
}

#[test]
fn a_thread_that_moves_does_not_get_another_processors_reseed_from_the_root() {
    // Processor 1 has no instruction: when it is due, the root reseeds it. The thread that
    // asks is on processor 0 by the time the root is reached, and processor 0 needs nothing
    // from the root: it is left as it is.
    let mut world = World::new(2);
    world.cpus[1].rdseed.reported = false;
    world.cpus[1].rdrand.reported = false;
    world.boot();
    world.now += RESEED_NANOS;
    world.on = 1;
    let first = world.steps + 1;
    world.mover = Some(Box::new(move |step, on| if step == first { 0 } else { on }));
    next_u64(&mut world).unwrap();
    assert_eq!(world.on, 0);
    assert_eq!(world.cpus[0].node.counts().reseeds_root, 0);
    assert_eq!(world.root.counts().reseeded, 0);
    assert!(
        world.cpus[1].node.due(world.now),
        "processor 1 is served next time"
    );
    assert!(
        world.cpus[0].node.due(world.now),
        "and processor 0 by its own instruction"
    );
    // Next time, without the move.
    world.mover = None;
    world.on = 1;
    next_u64(&mut world).unwrap();
    assert_eq!(world.cpus[1].node.counts().reseeds_root, 1);
    assert_eq!(world.root.counts().reseeded, 1);
    assert!(!world.cpus[1].node.due(world.now));
    assert_eq!(
        world.cpus[1].node.from_root, [0; 80],
        "what the root handed over is gone"
    );
    assert_eq!(world.cpus[1].node.from_root_len, 0);
}

#[test]
fn moving_between_processors_does_not_buy_more_tries() {
    // RDSEED alone, and dry everywhere. A thread that is on the other processor after every
    // step still gets one draw's tries, not one draw's tries on each.
    let mut world = World::reporting(2, true, false);
    world.boot();
    for cpu in 0..2 {
        let tried = world.tries(cpu, Instruction::Rdseed);
        world
            .line(cpu, Instruction::Rdseed)
            .rules
            .push(super::world::Rule::NotReady {
                from: tried,
                count: u64::MAX,
            });
    }
    world.now += RESEED_NANOS;
    world.mover = Some(Box::new(|_, on| 1 - on));
    let before = world.tries(0, Instruction::Rdseed) + world.tries(1, Instruction::Rdseed);
    next_u64(&mut world).unwrap();
    let tried = world.tries(0, Instruction::Rdseed) + world.tries(1, Instruction::Rdseed) - before;
    assert_eq!(tried, 100, "one draw's tries in all");
}

#[test]
fn a_request_inside_a_running_step_is_refused() {
    let mut world = World::new(1);
    world.boot();
    world.node_busy = true;
    let mut out = [0xEE; 24];
    assert_eq!(fill(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 24]);
    assert_eq!(next_u64(&mut world), Err(Error::NotNow));
    let mut out = [0xEE; 24];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 24]);
    world.node_busy = false;
    assert!(next_u64(&mut world).is_ok());
}

#[test]
fn a_taken_root_lock_is_waited_for_with_pauses() {
    let mut world = World::new(1);
    world.boot();
    world.root_busy = 5;
    let pauses = world.pauses;
    let tried = world.tries(0, Instruction::Rdseed);
    let mut out = [0u8; 16];
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.pauses - pauses, 5);
    assert_eq!(
        world.tries(0, Instruction::Rdseed) - tried,
        8,
        "the draws were made once"
    );
    // A lock that never comes free ends in a refusal, not a hang.
    world.root_busy = u32::MAX;
    let mut out = [0xEE; 16];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 16]);
}

#[test]
fn an_error_in_a_later_piece_zeroes_the_whole_buffer() {
    let mut world = World::new(2);
    world.boot();
    // The thread moves, after its first piece, to a processor that has no generator.
    world.cpus[1].node.uninstantiate();
    world.mover = Some(Box::new(|_, _| 1));
    let mut out = [0xEE; 300];
    assert_eq!(fill(&mut world, &mut out), Err(Error::NoGenerator));
    assert!(out.iter().all(|&b| b == 0));
}

#[test]
fn nothing_prints_a_state() {
    let mut world = World::new(1);
    world.boot();
    assert_eq!(format!("{:?}", world.cpus[0].node), "Node { .. }");
    assert_eq!(format!("{:?}", world.root), "Root { .. }");
    assert_eq!(
        format!("{:?}", world.cpus[0].node.counts()),
        "Counts { .. }"
    );
    assert_eq!(format!("{:?}", world.root.counts()), "RootCounts { .. }");
}
