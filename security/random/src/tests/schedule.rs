//! When a generator reseeds, from what, and what backs off (ADR-0016 R4 item 8, R6; TASKS 18b
//! S2, S3 and S6).

use super::world::{Rule, World};
use crate::{
    Condition, Error, Instruction, RESEED_NANOS, RESEED_REQUESTS, REST_NANOS, fill_fresh, next_u64,
};

use Instruction::{Rdrand, Rdseed};

/// One request on the caller's processor, and how many tries of each instruction it made.
fn request(world: &mut World) -> (u64, u64) {
    let cpu = world.on;
    let before = (world.tries(cpu, Rdseed), world.tries(cpu, Rdrand));
    next_u64(world).unwrap();
    (
        world.tries(cpu, Rdseed) - before.0,
        world.tries(cpu, Rdrand) - before.1,
    )
}

#[test]
fn no_instruction_is_touched_until_a_reseed_is_due() {
    let mut world = World::new(1);
    world.boot();
    let steps = world.steps;
    for _ in 0..1000 {
        assert_eq!(request(&mut world), (0, 0));
    }
    // One step for each request: nothing else runs.
    assert_eq!(world.steps - steps, 1000);
    assert_eq!(world.cpus[0].node.requests_since_seed(), 1000);
}

#[test]
fn sixty_seconds_bring_one_reseed_of_eight_draws() {
    let mut world = World::new(1);
    world.boot();
    world.now += RESEED_NANOS - 1;
    assert_eq!(request(&mut world), (0, 0), "one nanosecond early");
    world.now += 1;
    assert!(world.cpus[0].node.due(world.now));
    assert_eq!(request(&mut world), (8, 0));
    assert!(!world.cpus[0].node.due(world.now));
    assert_eq!(
        world.cpus[0].node.requests_since_seed(),
        1,
        "the count starts again"
    );
    assert_eq!(request(&mut world), (0, 0));
    let counts = world.cpus[0].node.counts();
    assert_eq!(
        (
            counts.reseeds_rdseed,
            counts.reseeds_rdrand,
            counts.reseeds_dry
        ),
        (1, 0, 0)
    );
    // And again a minute later.
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.cpus[0].node.counts().reseeds_rdseed, 2);
}

#[test]
fn two_to_the_twenty_requests_bring_a_reseed() {
    let mut world = World::new(1);
    world.boot();
    for _ in 0..RESEED_REQUESTS - 1 {
        next_u64(&mut world).unwrap();
    }
    assert_eq!(
        world.cpus[0].node.requests_since_seed(),
        RESEED_REQUESTS - 1
    );
    assert_eq!(
        request(&mut world),
        (0, 0),
        "request 2^20 is answered first"
    );
    assert!(world.cpus[0].node.due(world.now));
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.cpus[0].node.counts().reseeds_rdseed, 1);
}

#[test]
fn with_rdseed_unhealthy_the_reseed_comes_from_rdrand() {
    let mut world = World::new(1);
    world.line(0, Rdseed).rules.push(Rule::Bit {
        from: 0,
        bit: 3,
        set: false,
    });
    world.boot();
    world.now += RESEED_NANOS;
    // Due: RDSEED's start-up test begins again (eight draws of it), and RDRAND reseeds.
    assert_eq!(request(&mut world), (8, 8));
    let counts = world.cpus[0].node.counts();
    assert_eq!((counts.reseeds_rdrand, counts.reseeds_dry), (1, 0));
    assert_eq!(counts.retests, 1);
}

#[test]
fn a_dry_rdseed_costs_one_draws_tries_a_second_and_not_one_a_request() {
    // RDSEED alone, and it stops answering after the start.
    let mut world = World::reporting(1, true, false);
    world.boot();
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried,
        count: u64::MAX,
    });
    world.now += RESEED_NANOS;
    // The first request that is due spends one draw's tries, and is answered all the same.
    assert_eq!(request(&mut world), (100, 0));
    assert!(
        world.cpus[0].node.due(world.now),
        "the reseed is delayed, not done"
    );
    // Every request in the following second is answered without a try.
    for _ in 0..500 {
        assert_eq!(request(&mut world), (0, 0));
    }
    world.now += REST_NANOS - 1;
    assert_eq!(request(&mut world), (0, 0));
    // When the second has passed, one more draw's tries, once.
    world.now += 1;
    assert_eq!(request(&mut world), (100, 0));
    assert_eq!(request(&mut world), (0, 0));
    assert_eq!(world.cpus[0].node.counts().not_now_rdseed, 2);
    // Fresh entropy cannot be had meanwhile.
    let mut out = [0xEE; 16];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 16]);
    // RDSEED comes back: the delayed reseed is made.
    world.line(0, Rdseed).rules.clear();
    world.now += REST_NANOS;
    assert_eq!(request(&mut world), (8, 0));
    assert!(!world.cpus[0].node.due(world.now));
}

#[test]
fn while_rdseed_rests_a_reseed_goes_straight_to_rdrand() {
    let mut world = World::new(1);
    world.boot();
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried,
        count: 100,
    });
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world), (100, 8), "dry, then RDRAND at once");
    assert_eq!(world.cpus[0].node.counts().reseeds_dry, 1);
    // Half a second later a reseed falls due again (by the clock): RDSEED is still resting
    // and is not tried.
    world.now += REST_NANOS / 2;
    world.cpus[0].node.seeded_at = 0;
    assert!(world.cpus[0].node.due(world.now));
    assert_eq!(request(&mut world), (0, 8));
    assert_eq!(world.cpus[0].node.counts().reseeds_dry, 2);
    // After the second, RDSEED is asked first again.
    world.now += REST_NANOS;
    world.cpus[0].node.seeded_at = 0;
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.cpus[0].node.counts().reseeds_rdseed, 1);
}

#[test]
fn a_caller_with_interrupts_off_gets_one_try_for_each_draw() {
    let mut world = World::new(1);
    world.boot();
    let tried = world.tries(0, Rdseed);
    // RDSEED is not ready once, on the third draw.
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried + 2,
        count: 1,
    });
    world.now += RESEED_NANOS;
    world.quick = true;
    let pauses = world.pauses;
    // Two RDSEED draws, one try that was not ready, and the reseed from RDRAND at once.
    assert_eq!(request(&mut world), (3, 8));
    assert_eq!(world.pauses, pauses, "it never waits in a pause");
    assert_eq!(world.cpus[0].node.counts().reseeds_dry, 1);
}

#[test]
fn a_failure_at_run_time_drops_the_draws_and_the_test_runs_again_a_minute_later() {
    let mut world = World::new(1);
    world.boot();
    // RDSEED repeats a value on the fourth and fifth draws of the next reseed.
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::Value {
        from: tried + 3,
        count: 2,
        value: 0x5555_0000_AAAA_1111,
    });
    world.now += RESEED_NANOS;
    // Five RDSEED tries, the fifth failing; the four draws in hand are dropped and the
    // reseed is made from eight RDRAND draws.
    assert_eq!(request(&mut world), (5, 8));
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Unhealthy);
    let counts = world.cpus[0].node.counts();
    assert_eq!(
        (counts.reseeds_rdrand, counts.reseeds_dry),
        (1, 0),
        "not a dry fall-back"
    );
    assert_eq!(
        world.root.sources(),
        1,
        "the root is told in the same request"
    );
    assert_eq!(request(&mut world), (0, 0));

    // The start-up test begins again when a reseed next falls due, and not before.
    world.now += RESEED_NANOS - 1;
    assert_eq!(request(&mut world), (0, 0));
    world.now += 1;
    assert_eq!(
        request(&mut world),
        (8, 8),
        "eight draws of the test, and RDRAND's reseed"
    );
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Testing);
    // It goes on over the following requests, eight draws each, though nothing is due.
    let mut requests = 1;
    while world.cpus[0].node.condition(Rdseed) == Condition::Testing {
        let (rdseed, rdrand) = request(&mut world);
        assert_eq!(rdrand, 0);
        assert_eq!(rdseed, 8);
        requests += 1;
        assert!(requests <= 128);
    }
    assert_eq!(requests, 128, "1,024 draws at eight a request");
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Healthy);
    assert_eq!(world.cpus[0].node.counts().retests, 1);
    // The root has heard of it, and the next reseed is from RDSEED again.
    assert_eq!(world.root.sources(), 2);
    assert_eq!(request(&mut world), (0, 0));
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world), (8, 0));
}

#[test]
fn a_test_that_fails_again_waits_a_whole_period() {
    let mut world = World::new(1);
    // RDSEED's bit 60 is stuck for good.
    world.line(0, Rdseed).rules.push(Rule::Bit {
        from: 0,
        bit: 60,
        set: true,
    });
    world.boot();
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Unhealthy);
    world.now += RESEED_NANOS;
    // The test runs again over 64 requests and fails at its 512th draw.
    let mut rdseed_tries = 0;
    for _ in 0..64 {
        rdseed_tries += request(&mut world).0;
    }
    assert_eq!(rdseed_tries, 512);
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Unhealthy);
    assert_eq!(world.cpus[0].node.faults(Rdseed), 2);
    // It is not begun again within the minute, however often a reseed falls due.
    world.now += RESEED_NANOS - 1;
    world.cpus[0].node.seeded_at = 0;
    assert_eq!(request(&mut world), (0, 8));
    assert_eq!(world.cpus[0].node.counts().retests, 1);
    world.now += 1;
    world.cpus[0].node.seeded_at = 0;
    assert_eq!(request(&mut world), (8, 8));
    assert_eq!(world.cpus[0].node.counts().retests, 2);
}

#[test]
fn no_request_makes_more_than_sixteen_draws() {
    // The worst case: RDSEED's test completes inside the request, RDSEED then gives seven
    // draws and goes dry, and RDRAND would give eight more.
    let mut world = World::new(1);
    world.boot();
    let node = &mut world.cpus[0].node;
    // Put RDSEED eight draws short of a finished test.
    node.begin_retest_for_tests(Rdseed, 1016);
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried + 8 + 7,
        count: 100,
    });
    world.now += RESEED_NANOS;
    let (rdseed, rdrand) = request(&mut world);
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Healthy);
    // Eight for the test and seven for the reseed, then the dry draw's hundred tries.
    assert_eq!(rdseed, 8 + 7 + 100);
    // One RDRAND draw, the sixteenth of the request, and no more.
    assert_eq!(rdrand, 1);
    assert!(
        world.cpus[0].node.due(world.now),
        "the reseed waits for the next request"
    );
    // The next request finishes it.
    assert_eq!(request(&mut world), (0, 7));
    assert!(!world.cpus[0].node.due(world.now));
}

#[test]
fn when_every_instruction_has_failed_the_root_stops_and_the_generators_go_on() {
    let mut world = World::new(2);
    world.boot();
    assert_eq!(world.root.sources(), 4);
    // Every instruction on both processors starts repeating one value.
    for cpu in 0..2 {
        for which in [Rdseed, Rdrand] {
            let tried = world.tries(cpu, which);
            world.line(cpu, which).rules.push(Rule::Value {
                from: tried,
                count: u64::MAX,
                value: 0x0F0F_0F0F_0F0F_0F0F,
            });
        }
    }
    world.now += RESEED_NANOS;
    for cpu in 0..2 {
        world.on = cpu;
        // The reseed finds each instruction failed, then asks the root.
        assert!(
            next_u64(&mut world).is_ok(),
            "processor {cpu} still answers"
        );
        assert_eq!(world.cpus[cpu].node.condition(Rdseed), Condition::Unhealthy);
        assert_eq!(world.cpus[cpu].node.condition(Rdrand), Condition::Unhealthy);
    }
    // Processor 0 was reseeded by the root while processor 1's instructions still counted;
    // once both have told the root, it has stopped.
    assert_eq!(world.root.sources(), 0);
    assert!(world.root.stopped());
    assert_eq!(world.cpus[0].node.counts().reseeds_root, 1);
    assert_eq!(world.cpus[1].node.counts().reseeds_root, 0);
    // The generators go on with the seeds they have (R6), and ask the root once a second.
    world.on = 1;
    let root_steps = world.root_steps;
    for _ in 0..100 {
        assert!(next_u64(&mut world).is_ok());
    }
    assert_eq!(world.root_steps, root_steps);
    assert!(world.cpus[1].node.due(world.now));
    // Prediction resistance is refused.
    let mut out = [0u8; 8];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));

    // RDRAND on processor 0 recovers: its test runs again a minute later, and when it has
    // passed the root answers again.
    world.line(0, Rdrand).rules.clear();
    world.on = 0;
    world.now += RESEED_NANOS;
    for _ in 0..200 {
        next_u64(&mut world).unwrap();
    }
    assert_eq!(world.cpus[0].node.condition(Rdrand), Condition::Healthy);
    assert_eq!(world.root.sources(), 1);
    assert!(!world.root.stopped());
    // Processor 1, with nothing of its own, is reseeded by the root again.
    world.on = 1;
    world.now += REST_NANOS;
    next_u64(&mut world).unwrap();
    assert_eq!(world.cpus[1].node.counts().reseeds_root, 1);
    assert!(!world.cpus[1].node.due(world.now));
}

#[test]
fn fresh_requests_reseed_the_root_every_piece() {
    let mut world = World::new(1);
    world.boot();
    let mut out = [0u8; 300];
    let tried = world.tries(0, Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    // Three pieces of at most 128 bytes: eight RDSEED draws each.
    assert_eq!(world.tries(0, Rdseed) - tried, 24);
    assert_eq!(world.root.counts().fresh, 3);
    assert!(out[..128].iter().any(|&b| b != 0));
    assert!(out[256..].iter().any(|&b| b != 0));
    // A request for no bytes is still one piece.
    fill_fresh(&mut world, &mut []).unwrap();
    assert_eq!(world.root.counts().fresh, 4);
    // RDSEED not ready for a whole draw: refused, the buffer zeroed, and RDSEED rests.
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried + 2,
        count: 100,
    });
    let mut out = [0xEE; 32];
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(out, [0; 32]);
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    assert_eq!(
        world.tries(0, Rdseed) - tried,
        102,
        "the second request tried nothing"
    );
    // The two draws it had gathered are kept for the next reseed or request.
    world.now += REST_NANOS;
    let tried = world.tries(0, Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Rdseed) - tried, 6);
}
