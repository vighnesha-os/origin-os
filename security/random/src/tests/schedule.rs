//! When a generator reseeds, from what, and what backs off (ADR-0016 R4 item 8, R6; TASKS 18b
//! S2, S3 and S6).

use super::world::{Rule, World};
use crate::{
    Allow, Condition, Error, Instruction, Maint, PIECE, RESEED_DRAWS, RESEED_NANOS,
    RESEED_REQUESTS, REST_NANOS, fill_fresh, next_u64,
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
    // Eight draws for itself and, the root being due with it, eight for the root (R6).
    assert_eq!(request(&mut world), (16, 0));
    assert!(!world.cpus[0].node.due(world.now));
    assert!(!world.root.due(world.now));
    assert_eq!(world.root.counts().reseeds, 1);
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
    assert_eq!(request(&mut world), (16, 0));
    assert_eq!(world.cpus[0].node.counts().reseeds_rdseed, 2);
    assert_eq!(world.root.counts().reseeds, 2);
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
    // The delayed reseed, and the root's, which is due as well.
    assert_eq!(request(&mut world), (16, 0));
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
    // Dry, then RDRAND at once: eight draws for the processor, and eight for the root,
    // which is due and for which RDSEED is still resting.
    assert_eq!(request(&mut world), (100, 16));
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
    // That request made twelve draws, so the root, which is due as well, waits for the next
    // one: eight RDRAND draws.
    assert_eq!(world.root.counts().reseeds, 0);
    assert_eq!(request(&mut world), (0, 8));
    assert_eq!(world.root.counts().reseeds, 1);
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
    // It goes on over the following requests, eight draws each, though nothing is due here.
    // The first of them has eight draws left for the root.
    let mut requests = 1;
    while world.cpus[0].node.condition(Rdseed) == Condition::Testing {
        let (rdseed, rdrand) = request(&mut world);
        assert_eq!(rdrand, if requests == 1 { 8 } else { 0 });
        assert_eq!(rdseed, 8);
        requests += 1;
        assert!(requests <= 128);
    }
    assert_eq!(world.root.counts().reseeds, 2);
    assert_eq!(requests, 128, "1,024 draws at eight a request");
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Healthy);
    assert_eq!(world.cpus[0].node.counts().retests, 1);
    // The root has heard of it, and the next reseed is from RDSEED again.
    assert_eq!(world.root.sources(), 2);
    assert_eq!(request(&mut world), (0, 0));
    // And when both are next due, both are reseeded from RDSEED in one request.
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world), (16, 0));
    assert_eq!(world.root.counts().reseeds, 3);
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
    // Eight RDRAND draws for the processor, and no test of RDSEED. (The root was reseeded
    // by the second of the 64 requests and is not due.)
    assert_eq!(world.root.counts().reseeds, 1);
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
    // The next request finishes it with seven draws, and then has eight left for the root,
    // which is due: RDSEED is still resting, so they are RDRAND's.
    assert_eq!(request(&mut world), (0, 15));
    assert!(!world.cpus[0].node.due(world.now));
    assert!(!world.root.due(world.now));
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
    // Nothing it had gathered is kept: the next request makes all eight of its draws itself.
    assert_eq!(world.cpus[0].node.have, 0);
    assert!(world.cpus[0].node.entropy.iter().all(|&b| b == 0));
    world.now += REST_NANOS;
    let tried = world.tries(0, Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Rdseed) - tried, 8);
}

#[test]
fn the_figures_are_the_adrs() {
    // ADR-0016 R4, R6 and R7, and TASKS 18b S3: a change to any of these is a change to them.
    assert_eq!(Rdseed.tries(), 100);
    assert_eq!(Rdrand.tries(), 10);
    assert_eq!(RESEED_DRAWS, 8);
    assert_eq!(RESEED_REQUESTS, 1 << 20);
    assert_eq!(RESEED_NANOS, 60_000_000_000);
    assert_eq!(REST_NANOS, 1_000_000_000);
    assert_eq!(PIECE, 128);
    assert_eq!((Rdseed.name(), Rdrand.name()), ("RDSEED", "RDRAND"));
}

#[test]
fn a_test_does_not_begin_again_unless_a_reseed_is_due() {
    let mut world = World::new(1);
    world.line(0, Rdseed).rules.push(Rule::Bit {
        from: 0,
        bit: 9,
        set: true,
    });
    world.boot();
    assert_eq!(world.cpus[0].node.condition(Rdseed), Condition::Unhealthy);
    // A minute after the failure the test may begin again, but only when a reseed is due.
    // This generator was reseeded a moment ago (set by hand), so maintenance does nothing.
    world.now += RESEED_NANOS;
    world.cpus[0].node.seeded_at = world.now;
    assert!(!world.cpus[0].node.due(world.now));
    let tried = world.tries(0, Rdseed);
    let now = world.now;
    let cpu = &mut world.cpus[0];
    struct Never;
    impl crate::Source for Never {
        fn reports(&self, _: Instruction) -> bool {
            true
        }
        fn try_once(&mut self, _: Instruction) -> Option<u64> {
            panic!("no instruction is to be tried")
        }
    }
    assert_eq!(
        cpu.node.maintain(&mut Never, now, 0, Allow::ALL),
        Maint::Idle
    );
    assert_eq!(cpu.node.condition(Rdseed), Condition::Unhealthy);
    assert_eq!(world.tries(0, Rdseed), tried);
    assert_eq!(world.cpus[0].node.counts().retests, 0);
}

#[test]
fn every_draw_of_a_fresh_request_has_its_own_tries() {
    let mut world = World::new(1);
    world.boot();
    let tried = world.tries(0, Rdseed);
    // The second and the fourth of the eight draws wait sixty tries each.
    for from in [tried + 1, tried + 63] {
        world
            .line(0, Rdseed)
            .rules
            .push(Rule::NotReady { from, count: 60 });
    }
    let mut out = [0u8; 16];
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Rdseed) - tried, 8 + 120);
    assert_eq!(world.cpus[0].node.counts().not_now_rdseed, 0);
}

#[test]
fn fresh_entropy_is_never_older_than_the_request() {
    use crate::{Answer, Fresh, Try};
    // Draws an ordinary reseed left in the store are not a fresh request's.
    let mut world = World::reporting(1, true, false);
    world.boot();
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried + 3,
        count: 100,
    });
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world), (103, 0));
    assert_eq!(
        world.cpus[0].node.have, 3,
        "three draws wait for the delayed reseed"
    );
    world.now += REST_NANOS;
    let tried = world.tries(0, Rdseed);
    let mut out = [0u8; 16];
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(
        world.tries(0, Rdseed) - tried,
        8,
        "all eight are made inside the request"
    );
    assert_eq!(world.cpus[0].node.have, 0);

    // A request that gathered its eight and could not reach the root keeps none of them.
    world.root_busy = u32::MAX;
    assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    world.root_busy = 0;
    assert_eq!(world.cpus[0].node.have, 0);
    assert!(world.cpus[0].node.entropy.iter().all(|&b| b == 0));
    let tried = world.tries(0, Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Rdseed) - tried, 8);

    // The root takes a store only from the request that filled it. Another request beginning
    // on the same processor takes the store over, and the first begins again.
    let now = world.now;
    let cpu = &mut world.cpus[0];
    struct Good<'a>(&'a mut super::world::Line);
    impl crate::Source for Good<'_> {
        fn reports(&self, _: Instruction) -> bool {
            true
        }
        fn try_once(&mut self, _: Instruction) -> Option<u64> {
            self.0.try_once_for_tests()
        }
    }
    let mut source = Good(&mut cpu.rdseed);
    let (first, did) = cpu.node.fresh_try(&mut source, now, None);
    assert_eq!(did, Fresh::Tried(Try::Stored));
    for _ in 0..7 {
        assert_eq!(
            cpu.node.fresh_try(&mut source, now, Some(first)),
            (first, Fresh::Tried(Try::Stored))
        );
    }
    assert_eq!(
        cpu.node.fresh_try(&mut source, now, Some(first)),
        (first, Fresh::Ready)
    );
    let mut out = [0xEE; 16];
    // A request with another number is given nothing from this store.
    assert_eq!(
        world
            .root
            .fresh_piece(&mut cpu.node, &mut out, now, 0, first.wrapping_add(1)),
        Answer::NotGathered
    );
    assert_eq!(out, [0xEE; 16]);
    // A second request begins here: the store is emptied and gets a new number.
    let (second, did) = cpu.node.fresh_try(&mut source, now, None);
    assert_ne!(second, first);
    assert_eq!(did, Fresh::Tried(Try::Stored));
    assert_eq!(cpu.node.have, 1, "the first request's eight draws are gone");
    // The first request finds its store taken over, and begins again in its turn.
    let (third, did) = cpu.node.fresh_try(&mut source, now, Some(first));
    assert!(third != first && third != second);
    assert_eq!(did, Fresh::Tried(Try::Stored));
    assert_eq!(cpu.node.have, 1);
    assert_eq!(
        world
            .root
            .fresh_piece(&mut cpu.node, &mut out, now, 0, first),
        Answer::NotGathered
    );
    // A request that ends unanswered overwrites what it gathered, and only that.
    cpu.node.end_fresh(second);
    assert_eq!(cpu.node.have, 1, "another request's store is left alone");
    cpu.node.end_fresh(third);
    assert_eq!(cpu.node.have, 0);
}

#[test]
fn the_root_is_reseeded_on_the_processors_schedule() {
    let mut world = World::new(2);
    world.boot();
    assert!(!world.root.due(world.now));
    // Not due: a processor that asks does not go near the root.
    let asks = world.root_asks;
    assert_eq!(request(&mut world), (0, 0));
    assert_eq!(world.root_asks, asks);
    world.now += RESEED_NANOS;
    assert!(world.root.due(world.now));
    // The first processor to reseed itself reseeds the root too; the second looks, finds it
    // done and draws nothing for it.
    assert_eq!(request(&mut world), (16, 0));
    assert_eq!(world.root.counts().reseeds, 1);
    assert!(!world.root.due(world.now));
    assert_eq!(world.cpus[0].node.have, 0);
    world.on = 1;
    let asks = world.root_asks;
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.root_asks, asks + 1, "one look");
    assert_eq!(world.root.counts().reseeds, 1);
    // Neither looks again until it has reseeded itself again.
    let asks = world.root_asks;
    for on in [0, 1] {
        world.on = on;
        assert_eq!(request(&mut world), (0, 0));
    }
    assert_eq!(world.root_asks, asks);

    // A minute later, with the root's lock taken when the first processor looks: it does not
    // wait and draws nothing for the root, and the next processor to reseed itself does it.
    world.now += RESEED_NANOS;
    world.on = 0;
    world.root_busy = 1;
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(
        world.pauses, 0,
        "a request does not wait for the root's lock on the root's account"
    );
    assert!(world.root.due(world.now));
    world.on = 1;
    assert_eq!(request(&mut world), (16, 0));
    assert_eq!(world.root.counts().reseeds, 2);
    // The first processor still owes its look: it makes it a second later, and finds the
    // root done.
    world.on = 0;
    let asks = world.root_asks;
    assert_eq!(request(&mut world), (0, 0));
    assert_eq!(world.root_asks, asks, "the root is left alone for a second");
    world.now += REST_NANOS;
    assert_eq!(request(&mut world), (0, 0));
    assert_eq!(world.root_asks, asks + 1);
    assert_eq!(request(&mut world), (0, 0));
    assert_eq!(world.root_asks, asks + 1, "and it has looked");
}

#[test]
fn a_processor_that_could_not_reseed_the_root_looks_again() {
    // One processor, so nobody else reseeds the root.
    let mut world = World::reporting(1, true, false);
    world.boot();
    world.now += RESEED_NANOS;
    // The lock is free at the look and taken when the eight draws are ready: they are not
    // kept for later.
    world.root_taken_at = vec![world.root_asks + 2];
    assert_eq!(request(&mut world), (16, 0));
    assert_eq!(world.pauses, 0);
    assert_eq!(world.root.counts().reseeds, 0);
    assert!(world.root.due(world.now));
    assert_eq!(world.cpus[0].node.have, 0);
    assert!(world.cpus[0].node.entropy.iter().all(|&b| b == 0));
    // The root is left alone for a second, then looked at again by the next request, though
    // this processor is not due itself: eight new draws, and the root is reseeded.
    assert_eq!(request(&mut world), (0, 0));
    world.now += REST_NANOS;
    assert!(!world.cpus[0].node.due(world.now));
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.root.counts().reseeds, 1);
    assert!(!world.root.due(world.now));
    assert_eq!(
        world.cpus[0].node.counts().reseeds_rdseed,
        1,
        "its own was not repeated"
    );
    assert_eq!(request(&mut world), (0, 0));

    // RDSEED dry when the root's draws are wanted: the processor has reseeded itself, the
    // root waits, and the processor looks again when RDSEED has rested.
    world.now += RESEED_NANOS;
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried + 8 + 3,
        count: 100,
    });
    assert_eq!(request(&mut world), (8 + 3 + 100, 0));
    assert_eq!(world.root.counts().reseeds, 1);
    assert_eq!(world.cpus[0].node.have, 0, "the three draws are not kept");
    assert_eq!(request(&mut world), (0, 0));
    world.now += REST_NANOS;
    assert_eq!(request(&mut world), (8, 0));
    assert_eq!(world.root.counts().reseeds, 2);

    // A request that has used its sixteen draws leaves the root to the next one.
    let mut world = World::new(1);
    world.boot();
    world.now += RESEED_NANOS;
    world.cpus[0].node.begin_retest_for_tests(Rdrand, 0);
    let reseeds = world.root.counts().reseeds;
    let before = (world.given(0, Rdseed), world.given(0, Rdrand));
    next_u64(&mut world).unwrap();
    assert_eq!(world.given(0, Rdrand) - before.1, 8, "towards the test");
    assert_eq!(world.given(0, Rdseed) - before.0, 8, "its own reseed");
    assert_eq!(world.root.counts().reseeds, reseeds);
    assert!(world.root.due(world.now));
    // The next request: eight more towards the test, eight for the root.
    let before = (world.given(0, Rdseed), world.given(0, Rdrand));
    next_u64(&mut world).unwrap();
    assert_eq!(world.given(0, Rdrand) - before.1, 8);
    assert_eq!(world.given(0, Rdseed) - before.0, 8);
    assert_eq!(world.root.counts().reseeds, reseeds + 1);
}

#[test]
fn a_processor_that_starts_when_the_root_is_due_reseeds_it_first() {
    let mut world = World::new(2);
    assert!(crate::start_root(&mut world, false).instantiated);
    assert!(crate::start_processor(&mut world).generator);
    world.now += RESEED_NANOS;
    world.on = 1;
    let started = crate::start_processor(&mut world);
    assert!(started.generator);
    assert_eq!(started.own_regions, 2);
    // Its 1,024 start-up draws of each, and eight more of RDSEED for the root.
    assert_eq!(world.given(1, Rdseed), 1032);
    assert_eq!(world.given(1, Rdrand), 1024);
    assert_eq!(world.root.counts().reseeds, 1);
    assert!(!world.root.due(world.now));
    assert_eq!(
        world.cpus[1].node.have, 0,
        "the draws went to the root and are gone"
    );
    assert!(next_u64(&mut world).is_ok());
}

#[test]
fn a_clock_that_steps_back_ends_a_wait() {
    use crate::node::passed;
    assert!(!passed(10, 10, 5));
    assert!(!passed(14, 10, 5));
    assert!(passed(15, 10, 5));
    assert!(
        passed(9, 10, 5),
        "a clock before the start of the wait has stepped back"
    );
    assert!(passed(0, u64::MAX, 1));
    assert!(passed(u64::MAX, 0, u64::MAX));

    // RDSEED alone, dry for one draw: it rests.
    let mut world = World::reporting(1, true, false);
    world.boot();
    world.now += RESEED_NANOS;
    let tried = world.tries(0, Rdseed);
    world.line(0, Rdseed).rules.push(Rule::NotReady {
        from: tried,
        count: 100,
    });
    assert_eq!(request(&mut world), (100, 0));
    assert_eq!(request(&mut world), (0, 0), "resting");
    // The clock steps back to before the machine started: the rest is over, not prolonged,
    // and a generator whose seed is "from the future" is due. Its reseed and the root's follow.
    world.now = 500_000;
    assert!(world.cpus[0].node.due(world.now));
    assert!(world.root.due(world.now));
    assert_eq!(request(&mut world), (16, 0));
    assert!(!world.cpus[0].node.due(world.now));
    assert!(!world.root.due(world.now));
}
