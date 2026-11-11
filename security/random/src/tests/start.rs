//! Starting a root and the processors' generators (ADR-0016 R4, R5, R8).

use super::world::{Rule, World};
use crate::{
    Condition, Error, Fault, Instruction, Node, Source, Try, fill, fill_fresh, fill_from_root,
    next_u64, seed, start_processor, start_root,
};

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

#[test]
fn with_no_instruction_there_is_no_generator_and_every_request_says_so() {
    let mut world = World::reporting(2, false, false);
    let root = start_root(&mut world, false);
    assert!(root.self_test);
    assert!(!root.instantiated);
    assert_eq!(
        (root.rdseed, root.rdrand),
        (Condition::Absent, Condition::Absent)
    );
    for cpu in 0..2 {
        world.on = cpu;
        let started = start_processor(&mut world);
        assert!(!started.generator);
        assert!(!world.cpus[cpu].node.has_generator());
        let mut out = [0xEE; 40];
        assert_eq!(fill(&mut world, &mut out), Err(Error::NoGenerator));
        assert!(all_zero(&out), "a refused request leaves zeros");
        assert_eq!(fill(&mut world, &mut []), Err(Error::NoGenerator));
        assert_eq!(next_u64(&mut world), Err(Error::NoGenerator));
        let mut out = [0xEE; 40];
        assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NoGenerator));
        assert!(all_zero(&out));
        let mut out = [0xEE; 40];
        assert_eq!(
            fill_from_root(&mut world, &mut out),
            Err(Error::NoGenerator)
        );
        assert!(all_zero(&out));
    }
    assert!(!world.root.is_instantiated());
}

#[test]
fn the_seed_area_is_empty_after_every_start() {
    let mut world = World::new(2);
    seed::extra_slot(&mut world.cpus[0].area)
        .unwrap()
        .fill(0x77);
    assert!(start_root(&mut world, true).instantiated);
    assert!(all_zero(&world.cpus[0].area), "after the root");
    for cpu in 0..2 {
        world.on = cpu;
        assert!(start_processor(&mut world).generator);
        assert!(all_zero(&world.cpus[cpu].area), "after processor {cpu}");
        // What the root handed over is gone too.
        assert!(all_zero(&world.cpus[cpu].node.from_root));
        assert_eq!(world.cpus[cpu].node.from_root_len, 0);
        assert_eq!(world.cpus[cpu].node.region_draws(0), 0);
        assert_eq!(world.cpus[cpu].node.region_draws(1), 0);
    }
}

#[test]
fn an_instruction_that_fails_its_start_up_test_is_left_out() {
    // Each way RDSEED can fail on the boot processor, at some draw of its start-up test.
    let cases: [(&str, Rule, Fault); 4] = [
        (
            "a stuck bit",
            Rule::Bit {
                from: 0,
                bit: 17,
                set: true,
            },
            Fault::StuckBit,
        ),
        (
            "a repeat",
            Rule::Value {
                from: 700,
                count: 2,
                value: 0x1234_5678_9ABC_DEF1,
            },
            Fault::Repeat,
        ),
        (
            "a constant from the start",
            Rule::Value {
                from: 0,
                count: u64::MAX,
                value: 0xAAAA_AAAA_AAAA_AAAA,
            },
            Fault::Repeat,
        ),
        (
            "nothing but zero",
            Rule::Value {
                from: 300,
                count: u64::MAX,
                value: 0,
            },
            Fault::StuckValue,
        ),
    ];
    for (name, rule, fault) in cases {
        let mut world = World::new(1);
        world.line(0, Instruction::Rdseed).rules.push(rule);
        let root = start_root(&mut world, false);
        assert!(root.instantiated, "{name}: RDRAND alone starts a root");
        assert_eq!(root.rdseed, Condition::Unhealthy, "{name}");
        assert_eq!(root.rdrand, Condition::Healthy, "{name}");
        assert_eq!(
            world.cpus[0].node.last_fault(Instruction::Rdseed),
            Some(fault),
            "{name}"
        );
        // The root is counted on RDRAND, and says so (R3).
        assert_eq!(world.root.counted(), Some(Instruction::Rdrand), "{name}");
        assert!(!world.root.both());
        assert_eq!(world.given(0, Instruction::Rdrand), 1028, "{name}");
        let started = start_processor(&mut world);
        assert!(started.generator);
        assert_eq!(started.own_regions, 1, "{name}: RDRAND's draws only");
        assert_eq!(world.root.sources(), 1);
        assert!(all_zero(&world.cpus[0].area));
        assert!(next_u64(&mut world).is_ok());
        // RDSEED is not healthy here, so nothing is fresh.
        let mut out = [0u8; 8];
        assert_eq!(fill_fresh(&mut world, &mut out), Err(Error::NotNow));
    }
}

#[test]
fn a_failed_draw_empties_what_was_collected() {
    // Driven by hand: ten draws into region 0, then the tenth again.
    struct Fixed(Vec<u64>);
    impl Source for Fixed {
        fn reports(&self, _: Instruction) -> bool {
            true
        }
        fn try_once(&mut self, _: Instruction) -> Option<u64> {
            Some(self.0.remove(0))
        }
    }
    let mut values: Vec<u64> = (1..=10).map(|n| n * 0x0101_0101_0101_0101).collect();
    values.push(10 * 0x0101_0101_0101_0101);
    let mut source = Fixed(values);
    let mut node = Node::new();
    assert!(node.start(&source, 5));
    assert!(!node.start(&source, 6), "a processor starts once");
    assert_eq!(node.index(), 5);
    let mut area = vec![0u8; seed::AREA_LEN];
    // Nothing is drawn from an instruction that is not being tested.
    assert_eq!(
        node.collect_try(&mut source, Instruction::Rdseed, &mut area, 0, 1024, 0),
        Try::Unwanted
    );
    node.begin_test(Instruction::Rdseed);
    for n in 0..10u16 {
        assert_eq!(
            node.collect_try(&mut source, Instruction::Rdseed, &mut area, 0, 1024, 0),
            Try::Stored
        );
        assert_eq!(node.region_draws(0), n + 1);
    }
    // The draws are in the region, low byte first, where the layout says.
    assert_eq!(&area[seed::HEAD..seed::HEAD + 8], &[1; 8]);
    assert_eq!(&area[seed::HEAD + 72..seed::HEAD + 80], &[10; 8]);
    assert!(all_zero(&area[..seed::HEAD]));
    assert_eq!(
        node.collect_try(&mut source, Instruction::Rdseed, &mut area, 0, 1024, 0),
        Try::Failed
    );
    assert_eq!(node.region_draws(0), 0);
    assert!(all_zero(&area), "the region is emptied");
    assert_eq!(node.condition(Instruction::Rdseed), Condition::Unhealthy);
}

#[test]
fn not_ready_for_99_tries_is_a_draw_and_for_100_is_not_now() {
    // 99 tries not ready, then a value: the draw is made.
    let mut world = World::new(1);
    world
        .line(0, Instruction::Rdseed)
        .rules
        .push(Rule::NotReady {
            from: 500,
            count: 99,
        });
    let root = start_root(&mut world, false);
    assert_eq!(root.rdseed, Condition::Healthy);
    assert_eq!(world.tries(0, Instruction::Rdseed), 1024 + 99);
    assert_eq!(world.pauses, 99);
    assert_eq!(world.cpus[0].node.counts().not_now_rdseed, 0);

    // Two draws that each wait sixty tries: each has its own hundred.
    let mut world = World::new(1);
    for from in [300, 700] {
        world
            .line(0, Instruction::Rdseed)
            .rules
            .push(Rule::NotReady { from, count: 60 });
    }
    let root = start_root(&mut world, false);
    assert_eq!(root.rdseed, Condition::Healthy);
    assert_eq!(world.tries(0, Instruction::Rdseed), 1024 + 120);

    // 100 tries not ready: "not now". At a start that leaves the instruction out, with no
    // fault, and the root is counted on RDRAND.
    let mut world = World::new(1);
    world
        .line(0, Instruction::Rdseed)
        .rules
        .push(Rule::NotReady {
            from: 500,
            count: 100,
        });
    let root = start_root(&mut world, false);
    assert!(root.instantiated);
    assert_eq!(root.rdseed, Condition::Unhealthy);
    assert_eq!(world.cpus[0].node.faults(Instruction::Rdseed), 0);
    assert_eq!(world.cpus[0].node.last_fault(Instruction::Rdseed), None);
    assert_eq!(world.tries(0, Instruction::Rdseed), 600);
    assert_eq!(world.cpus[0].node.counts().not_now_rdseed, 1);
    assert_eq!(world.root.counted(), Some(Instruction::Rdrand));

    // RDRAND's budget is ten.
    let mut world = World::reporting(1, false, true);
    world
        .line(0, Instruction::Rdrand)
        .rules
        .push(Rule::NotReady { from: 5, count: 9 });
    assert!(start_root(&mut world, false).instantiated);
    let mut world = World::reporting(1, false, true);
    world
        .line(0, Instruction::Rdrand)
        .rules
        .push(Rule::NotReady { from: 5, count: 10 });
    let root = start_root(&mut world, false);
    assert!(
        !root.instantiated,
        "the only instruction was not ready: no root"
    );
    assert_eq!(world.tries(0, Instruction::Rdrand), 15);
}

#[test]
fn zeros_among_the_tries_are_set_aside_and_a_draw_of_only_such_is_a_failure() {
    // Fifty zeros, then values: set aside, and the test goes on.
    let mut world = World::new(1);
    world.line(0, Instruction::Rdseed).rules.push(Rule::Value {
        from: 200,
        count: 50,
        value: 0,
    });
    let root = start_root(&mut world, false);
    assert_eq!(root.rdseed, Condition::Healthy);
    assert_eq!(world.cpus[0].node.set_aside(Instruction::Rdseed), 50);
    assert_eq!(world.tries(0, Instruction::Rdseed), 1024 + 50);

    // All ones and "not ready" by turns for a whole draw: stuck, which is a failure (the
    // mixture is held to be one).
    let mut world = World::new(1);
    let line = world.line(0, Instruction::Rdseed);
    line.rules.push(Rule::Value {
        from: 200,
        count: 100,
        value: u64::MAX,
    });
    for n in 0..50 {
        line.rules.push(Rule::NotReady {
            from: 200 + 2 * n,
            count: 1,
        });
    }
    let root = start_root(&mut world, false);
    assert_eq!(root.rdseed, Condition::Unhealthy);
    assert_eq!(
        world.cpus[0].node.last_fault(Instruction::Rdseed),
        Some(Fault::StuckValue)
    );
    assert_eq!(world.tries(0, Instruction::Rdseed), 300);
}

#[test]
fn a_second_start_changes_nothing() {
    let mut world = World::new(1);
    world.boot();
    let first = next_u64(&mut world).unwrap();
    let again = start_root(&mut world, false);
    assert!(!again.instantiated, "there is a root already");
    let started = start_processor(&mut world);
    assert!(!started.generator, "the processor has a generator already");
    assert!(world.cpus[0].node.has_generator());
    assert!(all_zero(&world.cpus[0].area));
    assert_ne!(next_u64(&mut world).unwrap(), first);
    assert_eq!(world.root.sources(), 2);
}

#[test]
fn a_processor_with_one_failed_instruction_still_gets_a_generator() {
    let mut world = World::new(2);
    // RDRAND repeats on processor 1 only.
    world.line(1, Instruction::Rdrand).rules.push(Rule::Value {
        from: 10,
        count: 2,
        value: 99,
    });
    world.boot();
    assert_eq!(
        world.cpus[1].node.condition(Instruction::Rdseed),
        Condition::Healthy
    );
    assert_eq!(
        world.cpus[1].node.condition(Instruction::Rdrand),
        Condition::Unhealthy
    );
    assert_eq!(world.root.sources(), 3);
    world.on = 1;
    assert!(next_u64(&mut world).is_ok());
}

#[test]
fn the_loaders_root_answers_requests_itself() {
    // The loader has a root and no processors' generators (R10).
    let mut world = World::new(1);
    seed::extra_slot(&mut world.cpus[0].area)
        .unwrap()
        .fill(0x33);
    assert!(start_root(&mut world, true).instantiated);
    let mut a = [0u8; 8];
    let mut b = [0u8; 8];
    fill_from_root(&mut world, &mut a).unwrap();
    fill_from_root(&mut world, &mut b).unwrap();
    assert_ne!(a, b);
    assert_ne!(a, [0; 8]);
    let mut seed64 = [0u8; 64];
    fill_from_root(&mut world, &mut seed64).unwrap();
    assert!(!all_zero(&seed64));
    let mut long = [0u8; 300];
    fill_from_root(&mut world, &mut long).unwrap();
    assert!(!all_zero(&long[256..]));
    // No processor's generator was started, so the ordinary request has nothing to ask.
    assert_eq!(fill(&mut world, &mut a), Err(Error::NoGenerator));
    world.root.uninstantiate();
    assert_eq!(fill_from_root(&mut world, &mut a), Err(Error::NoGenerator));
    assert_eq!(a, [0; 8]);
}

#[test]
fn a_failed_self_test_means_no_generator_and_no_draw() {
    let mut world = World::new(1);
    let root = crate::drive::start_root_after(&mut world, false, false);
    assert!(!root.self_test);
    assert!(!root.instantiated);
    // The instructions were not even tried.
    assert_eq!(world.tries(0, Instruction::Rdseed), 0);
    assert_eq!(world.tries(0, Instruction::Rdrand), 0);
    assert_eq!(
        (root.rdseed, root.rdrand),
        (Condition::Untested, Condition::Untested)
    );
    assert!(!start_processor(&mut world).generator);
    assert_eq!(next_u64(&mut world), Err(Error::NoGenerator));
}

#[test]
fn a_failed_instructions_draws_leave_the_store() {
    use crate::{Allow, Maint};
    let mut world = World::new(1);
    world.boot();
    world.now += crate::RESEED_NANOS;
    // Five RDSEED draws into the store, then RDSEED gives the fifth again.
    let tried = world.tries(0, Instruction::Rdseed);
    world.line(0, Instruction::Rdseed).rules.push(Rule::Value {
        from: tried + 4,
        count: 2,
        value: 0x00FF_00FF_00FF_00FF,
    });
    let now = world.now;
    let step = |world: &mut World| {
        let cpu = &mut world.cpus[0];
        struct Both<'a>(&'a mut super::world::Line, &'a mut super::world::Line);
        impl Source for Both<'_> {
            fn reports(&self, _: Instruction) -> bool {
                true
            }
            fn try_once(&mut self, which: Instruction) -> Option<u64> {
                match which {
                    Instruction::Rdseed => self.0.try_once_for_tests(),
                    Instruction::Rdrand => self.1.try_once_for_tests(),
                }
            }
        }
        cpu.node.maintain(
            &mut Both(&mut cpu.rdseed, &mut cpu.rdrand),
            now,
            0,
            Allow::ALL,
        )
    };
    for n in 1..=5 {
        assert!(
            matches!(
                step(&mut world),
                Maint::Tried {
                    outcome: Try::Stored,
                    ..
                }
            ),
            "{n}"
        );
        assert_eq!(world.cpus[0].node.have, n);
    }
    assert!(matches!(
        step(&mut world),
        Maint::Tried {
            outcome: Try::Failed,
            ..
        }
    ));
    assert_eq!(world.cpus[0].node.have, 0, "the draws in hand are dropped");
    assert!(all_zero(&world.cpus[0].node.entropy));
    // The root is told, then RDRAND takes over with nothing set aside.
    assert_eq!(step(&mut world), Maint::TellRoot);
    world.cpus[0]
        .node
        .set_told_healthy(Instruction::Rdseed, false);
    assert!(matches!(
        step(&mut world),
        Maint::Tried {
            which: Instruction::Rdrand,
            outcome: Try::Stored,
            ..
        }
    ));
    assert_eq!(world.cpus[0].node.aside_have, 0);
    assert!(all_zero(&world.cpus[0].node.aside));
}

#[test]
fn the_root_gives_nothing_fresh_from_rdrand_draws() {
    use crate::Answer;
    let mut world = World::new(1);
    world.boot();
    // A store full of RDRAND draws: what a dry fall-back leaves if its reseed has not run.
    let node = &mut world.cpus[0].node;
    node.have = 8;
    node.from = Instruction::Rdrand;
    node.entropy = [0x42; 64];
    let mut out = [0xEE; 16];
    assert_eq!(
        world
            .root
            .fresh_piece(&mut world.cpus[0].node, &mut out, 0, 0),
        Answer::NotGathered
    );
    assert_eq!(out, [0xEE; 16], "nothing was written");
    assert_eq!(world.root.counts().fresh, 0);
    // The request itself gathers RDSEED draws in their place.
    let tried = world.tries(0, Instruction::Rdseed);
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(world.tries(0, Instruction::Rdseed) - tried, 8);
}
