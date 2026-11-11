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
    // Every processor's generator was reseeded, each from eight draws of its own RDSEED,
    // gathered by whichever requests passed through.
    for (cpu, tries) in total.iter().enumerate() {
        assert!(!world.cpus[cpu].node.due(world.now), "processor {cpu}");
        assert_eq!(
            world.cpus[cpu].node.counts().reseeds_rdseed,
            1,
            "processor {cpu}"
        );
        assert_eq!(*tries, 8, "processor {cpu}");
    }
    // Prediction resistance while moving: the eight draws must all be in the store of the
    // processor that asks the root, so it takes longer and still ends.
    let mut out = [0u8; 64];
    fill_fresh(&mut world, &mut out).unwrap();
    assert!(out.iter().any(|&b| b != 0));
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
