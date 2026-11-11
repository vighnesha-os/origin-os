//! A scripted machine: processors, their instructions, a clock and a root.

use crate::{Busy, Ctx, Instruction, Node, Platform, Root, Source, seed};

/// SplitMix64's output `n` (from 0) for `seed`: a fixed, well-spread sequence. The reference
/// tool `tools/seed_reference.py` computes the same.
pub(crate) fn splitmix(seed: u64, n: u64) -> u64 {
    let mut z = seed.wrapping_add(n.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The seed of processor `cpu`'s instruction `which`.
pub(crate) fn seed_of(cpu: u32, which: Instruction) -> u64 {
    let instruction = match which {
        Instruction::Rdseed => 1,
        Instruction::Rdrand => 2,
    };
    0x4F52_4947_494E_0000 + u64::from(cpu) * 0x100 + instruction
}

/// What an instruction does on some of its tries, in place of its sequence.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Rule {
    /// Not ready on tries `from..from + count`.
    NotReady { from: u64, count: u64 },
    /// This value on tries `from..from + count`, with success reported.
    Value { from: u64, count: u64, value: u64 },
    /// From try `from` on, bit `bit` of every value is `set`.
    Bit { from: u64, bit: u32, set: bool },
}

/// One instruction of one processor.
#[derive(Clone, Debug)]
pub(crate) struct Line {
    pub(crate) reported: bool,
    pub(crate) seed: u64,
    /// Tries made.
    pub(crate) tries: u64,
    /// Values handed out from the sequence.
    pub(crate) given: u64,
    pub(crate) rules: Vec<Rule>,
}

impl Line {
    /// One try, for a test that drives a step by hand.
    pub(crate) fn try_once_for_tests(&mut self) -> Option<u64> {
        self.try_once()
    }

    fn try_once(&mut self) -> Option<u64> {
        let at = self.tries;
        self.tries += 1;
        let mut forced = None;
        let mut mask = (u64::MAX, 0u64);
        for rule in &self.rules {
            match *rule {
                Rule::NotReady { from, count } if at >= from && at - from < count => return None,
                Rule::Value { from, count, value } if at >= from && at - from < count => {
                    forced = Some(value);
                }
                Rule::Bit { from, bit, set } if at >= from => {
                    if set {
                        mask.1 |= 1 << bit;
                    } else {
                        mask.0 &= !(1 << bit);
                    }
                }
                _ => {}
            }
        }
        if let Some(value) = forced {
            return Some(value);
        }
        let value = splitmix(self.seed, self.given);
        self.given += 1;
        Some((value & mask.0) | mask.1)
    }
}

/// One processor.
pub(crate) struct Cpu {
    pub(crate) node: Node,
    pub(crate) rdseed: Line,
    pub(crate) rdrand: Line,
    pub(crate) area: Vec<u8>,
}

struct Lines<'a> {
    rdseed: &'a mut Line,
    rdrand: &'a mut Line,
}

impl Source for Lines<'_> {
    fn reports(&self, which: Instruction) -> bool {
        match which {
            Instruction::Rdseed => self.rdseed.reported,
            Instruction::Rdrand => self.rdrand.reported,
        }
    }

    fn try_once(&mut self, which: Instruction) -> Option<u64> {
        match which {
            Instruction::Rdseed => self.rdseed.try_once(),
            Instruction::Rdrand => self.rdrand.try_once(),
        }
    }
}

/// The machine.
pub(crate) struct World {
    pub(crate) cpus: Vec<Cpu>,
    pub(crate) root: Root,
    /// The processor the caller is on.
    pub(crate) on: usize,
    pub(crate) now: u64,
    pub(crate) tsc: u64,
    pub(crate) quick: bool,
    /// Steps run, of both kinds.
    pub(crate) steps: u64,
    /// Steps run under the root's lock.
    pub(crate) root_steps: u64,
    pub(crate) pauses: u64,
    /// How many more times the root's lock is found taken.
    pub(crate) root_busy: u32,
    /// Whether a step is "already running" on the caller's processor.
    pub(crate) node_busy: bool,
    /// Where the caller is after step `n` (counted from 1), if it moves.
    pub(crate) mover: Option<Box<dyn FnMut(u64, usize) -> usize>>,
}

/// The time-stamp counter the tests run at, unless they change it.
pub(crate) const TSC: u64 = 0x1122_3344_5566_7788;

impl World {
    /// `cpus` processors, each reporting both instructions.
    pub(crate) fn new(cpus: u32) -> Self {
        Self::reporting(cpus, true, true)
    }

    /// `cpus` processors that report the instructions named.
    pub(crate) fn reporting(cpus: u32, rdseed: bool, rdrand: bool) -> Self {
        let line = |cpu, which, reported| Line {
            reported,
            seed: seed_of(cpu, which),
            tries: 0,
            given: 0,
            rules: Vec::new(),
        };
        Self {
            cpus: (0..cpus)
                .map(|cpu| Cpu {
                    node: Node::new(),
                    rdseed: line(cpu, Instruction::Rdseed, rdseed),
                    rdrand: line(cpu, Instruction::Rdrand, rdrand),
                    area: vec![0; seed::AREA_LEN],
                })
                .collect(),
            root: Root::new(),
            on: 0,
            now: 1_000_000,
            tsc: TSC,
            quick: false,
            steps: 0,
            root_steps: 0,
            pauses: 0,
            root_busy: 0,
            node_busy: false,
            mover: None,
        }
    }

    pub(crate) fn line(&mut self, cpu: usize, which: Instruction) -> &mut Line {
        match which {
            Instruction::Rdseed => &mut self.cpus[cpu].rdseed,
            Instruction::Rdrand => &mut self.cpus[cpu].rdrand,
        }
    }

    /// Tries made of an instruction of a processor.
    pub(crate) fn tries(&self, cpu: usize, which: Instruction) -> u64 {
        match which {
            Instruction::Rdseed => self.cpus[cpu].rdseed.tries,
            Instruction::Rdrand => self.cpus[cpu].rdrand.tries,
        }
    }

    /// Values an instruction of a processor has given from its sequence.
    pub(crate) fn given(&self, cpu: usize, which: Instruction) -> u64 {
        match which {
            Instruction::Rdseed => self.cpus[cpu].rdseed.given,
            Instruction::Rdrand => self.cpus[cpu].rdrand.given,
        }
    }

    /// Starts a root on processor 0 and a generator on every processor, in order, and leaves
    /// the caller on processor 0.
    pub(crate) fn boot(&mut self) {
        self.on = 0;
        let root = crate::start_root(self, false);
        assert!(root.instantiated, "the root did not start");
        for cpu in 0..self.cpus.len() {
            self.on = cpu;
            let started = crate::start_processor(self);
            assert!(started.generator, "processor {cpu} got no generator");
        }
        self.on = 0;
    }

    fn after_step(&mut self) {
        self.steps += 1;
        if let Some(mover) = self.mover.as_mut() {
            self.on = mover(self.steps, self.on);
        }
    }
}

impl Platform for World {
    fn step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>)) -> Result<(), Busy> {
        if self.node_busy {
            return Err(Busy::Node);
        }
        let cpu = &mut self.cpus[self.on];
        let mut source = Lines {
            rdseed: &mut cpu.rdseed,
            rdrand: &mut cpu.rdrand,
        };
        let mut ctx = Ctx {
            node: &mut cpu.node,
            source: &mut source,
            area: &mut cpu.area,
            index: self.on as u32,
            now: self.now,
            tsc: self.tsc,
        };
        step(&mut ctx);
        self.after_step();
        Ok(())
    }

    fn root_step(&mut self, step: &mut dyn FnMut(&mut Ctx<'_>, &mut Root)) -> Result<(), Busy> {
        if self.node_busy {
            return Err(Busy::Node);
        }
        if self.root_busy != 0 {
            self.root_busy -= 1;
            return Err(Busy::Root);
        }
        let cpu = &mut self.cpus[self.on];
        let mut source = Lines {
            rdseed: &mut cpu.rdseed,
            rdrand: &mut cpu.rdrand,
        };
        let mut ctx = Ctx {
            node: &mut cpu.node,
            source: &mut source,
            area: &mut cpu.area,
            index: self.on as u32,
            now: self.now,
            tsc: self.tsc,
        };
        step(&mut ctx, &mut self.root);
        self.root_steps += 1;
        self.after_step();
        Ok(())
    }

    fn pause(&mut self) {
        self.pauses += 1;
    }

    fn quick(&self) -> bool {
        self.quick
    }
}
