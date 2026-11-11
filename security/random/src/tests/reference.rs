//! Seeds put together byte for byte as ADR-0016 R5 and R6 say, checked against an independent
//! reference: `tools/seed_reference.py` writes what each generator must give, from the ADR's
//! tables and a Python HMAC_DRBG, and these tests run the same scripts through the policy.

use super::world::World;
use crate::{
    Instruction, RESEED_NANOS, fill, fill_fresh, seed, start_processor, start_root,
    tests::world::Rule,
};

const REFERENCE: &str = include_str!("../testdata/seed_reference.txt");

/// The reference's 64 bytes for `name`.
fn expected(name: &str) -> Vec<u8> {
    let line = REFERENCE
        .lines()
        .find(|line| line.split(' ').next() == Some(name))
        .unwrap_or_else(|| panic!("the reference has no line {name}"));
    let hex = line.split(' ').nth(1).unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    assert_eq!(bytes.len(), 64, "{name}");
    bytes
}

/// 64 bytes from the generator of processor `cpu`.
fn request(world: &mut World, cpu: usize) -> Vec<u8> {
    world.on = cpu;
    let mut out = [0u8; 64];
    fill(world, &mut out).unwrap();
    out.to_vec()
}

#[test]
fn the_reference_has_nine_lines() {
    let names: Vec<&str> = REFERENCE
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "a.cpu0",
            "a.cpu1",
            "a.cpu2",
            "a.cpu0.reseeded",
            "a.cpu2.reseeded",
            "a.fresh",
            "b.cpu0",
            "c.cpu0",
            "e.cpu0"
        ]
    );
}

#[test]
fn three_processors_a_reseed_the_root_and_prediction_resistance() {
    let mut world = World::new(3);
    // Processor 2 reports neither instruction.
    world.cpus[2].rdseed.reported = false;
    world.cpus[2].rdrand.reported = false;
    // The root is given 64 extra bytes: in the kernel, the loader's seed.
    let extra: Vec<u8> = (0..64).map(|i| 0x40 + i).collect();
    seed::extra_slot(&mut world.cpus[0].area)
        .unwrap()
        .copy_from_slice(&extra);
    let root = start_root(&mut world, true);
    assert!(root.self_test && root.instantiated);
    assert!(world.root.extra() && world.root.both());
    assert_eq!(world.root.counted(), Some(Instruction::Rdseed));
    for cpu in 0..3 {
        world.on = cpu;
        let started = start_processor(&mut world);
        assert!(started.generator, "processor {cpu}");
        assert_eq!(started.own_regions, if cpu == 2 { 0 } else { 2 });
    }
    // The boot processor drew 1,024 of each for the root and as many again for itself.
    assert_eq!(world.given(0, Instruction::Rdseed), 2048);
    assert_eq!(world.given(0, Instruction::Rdrand), 2048);
    assert_eq!(world.given(1, Instruction::Rdseed), 1024);
    assert_eq!(world.given(2, Instruction::Rdseed), 0);
    assert_eq!(world.root.counts().seeded, 3);
    // Two healthy instructions on each of two processors.
    assert_eq!(world.root.sources(), 4);

    assert_eq!(request(&mut world, 0), expected("a.cpu0"));
    assert_eq!(request(&mut world, 1), expected("a.cpu1"));
    assert_eq!(request(&mut world, 2), expected("a.cpu2"));

    // Sixty seconds on, every generator is due a reseed.
    world.now += RESEED_NANOS;
    assert_eq!(request(&mut world, 0), expected("a.cpu0.reseeded"));
    assert_eq!(world.given(0, Instruction::Rdseed), 2056, "eight draws");
    assert_eq!(
        world.given(0, Instruction::Rdrand),
        2048,
        "RDRAND is not touched"
    );
    assert_eq!(world.cpus[0].node.counts().reseeds_rdseed, 1);

    // Processor 2 has nothing of its own: the root reseeds it.
    assert_eq!(request(&mut world, 2), expected("a.cpu2.reseeded"));
    assert_eq!(world.cpus[2].node.counts().reseeds_root, 1);
    assert_eq!(world.root.counts().reseeded, 1);

    // Prediction resistance on processor 1: eight of its RDSEED draws reseed the root.
    world.on = 1;
    let mut out = [0u8; 64];
    fill_fresh(&mut world, &mut out).unwrap();
    assert_eq!(out.to_vec(), expected("a.fresh"));
    assert_eq!(world.given(1, Instruction::Rdseed), 1032);
    assert_eq!(world.root.counts().fresh, 1);
    assert_eq!(world.root.counts().reseeds, 1);
}

#[test]
fn a_root_with_one_instruction_takes_four_further_draws_for_its_nonce() {
    for (name, which) in [
        ("b.cpu0", Instruction::Rdseed),
        ("c.cpu0", Instruction::Rdrand),
    ] {
        let mut world = World::reporting(
            1,
            which == Instruction::Rdseed,
            which == Instruction::Rdrand,
        );
        let root = start_root(&mut world, false);
        assert!(root.instantiated, "{name}");
        assert_eq!(world.given(0, which), 1028, "{name}: 1,024 and four more");
        assert_eq!(world.root.counted(), Some(which));
        assert!(!world.root.both() && !world.root.extra());
        let started = start_processor(&mut world);
        assert!(started.generator);
        assert_eq!(started.own_regions, 1);
        assert_eq!(world.given(0, which), 2052, "{name}");
        assert_eq!(request(&mut world, 0), expected(name));
        assert_eq!(world.root.sources(), 1);
    }
}

#[test]
fn a_dry_rdseed_hands_the_reseed_to_rdrand_and_its_draws_go_in_beside() {
    let mut world = World::new(1);
    world.boot();
    // Three more RDSEED draws arrive, then it is not ready for a whole draw's tries.
    let tried = world.tries(0, Instruction::Rdseed);
    world
        .line(0, Instruction::Rdseed)
        .rules
        .push(Rule::NotReady {
            from: tried + 3,
            count: 100,
        });
    world.now += RESEED_NANOS;
    let pauses = world.pauses;
    assert_eq!(request(&mut world, 0), expected("e.cpu0"));
    assert_eq!(world.tries(0, Instruction::Rdseed), tried + 103);
    assert_eq!(
        world.given(0, Instruction::Rdrand),
        2056,
        "eight RDRAND draws"
    );
    // A pause between tries, and none after the last.
    assert_eq!(world.pauses - pauses, 99);
    let counts = world.cpus[0].node.counts();
    assert_eq!(counts.not_now_rdseed, 1);
    assert_eq!(counts.reseeds_rdrand, 1);
    assert_eq!(counts.reseeds_dry, 1);
    assert_eq!(counts.reseeds_rdseed, 0);
}
