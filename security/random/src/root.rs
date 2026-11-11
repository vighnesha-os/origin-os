//! The root generator: it seeds and reseeds the processors' generators and answers requests
//! with prediction resistance, and in the kernel nothing else (ADR-0016 R5 to R7; SP 800-90C
//! §7). The loader has a root and no processors' generators: there the root answers every
//! request.
//!
//! Every operation here is one step, taken while the caller holds the root's lock. None tries
//! an instruction: the draws a root is given were gathered before, in the asking processor's
//! own store (TASKS 18b S4).

use origin_crypto::HmacDrbgSha512;

use crate::health::STARTUP_DRAWS;
use crate::node::{Instruction, Node, PIECE, RESEED_NANOS, RESEED_REQUESTS};
use crate::seed::{self, NONCE_DRAWS};
use crate::wipe::wipe;

/// What the root did with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// It was done.
    Done,
    /// There is no root: none was instantiated (R8).
    NoGenerator,
    /// The root has stopped: every instruction on every processor has failed (R6).
    Stopped,
    /// The asking processor's store does not hold what the request needs.
    NotGathered,
    /// The root has reached the standard's limit of requests without a reseed.
    NotNow,
}

/// Counts for reports. None is secret.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct RootCounts {
    /// Processors' generators given a first seed.
    pub seeded: u32,
    /// Processors' generators reseeded from the root's output.
    pub reseeded: u32,
    /// Reseeds of the root itself.
    pub reseeds: u32,
    /// Pieces answered with prediction resistance.
    pub fresh: u64,
}

impl core::fmt::Debug for RootCounts {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RootCounts { .. }")
    }
}

/// The root generator.
///
/// It is neither `Clone` nor `Copy`, and its `Debug` prints nothing of it.
pub struct Root {
    drbg: HmacDrbgSha512,
    seeded_at: u64,
    /// The instruction its first seed was counted on (R3).
    counted: Option<Instruction>,
    /// Whether the nonce was the other instruction's start-up draws.
    both: bool,
    /// Whether the extra bytes were in its first seed.
    extra: bool,
    /// Healthy instructions, over all processors.
    sources: u32,
    counts: RootCounts,
}

impl Root {
    /// No root yet.
    pub const fn new() -> Self {
        Self {
            drbg: HmacDrbgSha512::new(),
            seeded_at: 0,
            counted: None,
            both: false,
            extra: false,
            sources: 0,
            counts: RootCounts {
                seeded: 0,
                reseeded: 0,
                reseeds: 0,
                fresh: 0,
            },
        }
    }

    /// Whether there is a root.
    pub const fn is_instantiated(&self) -> bool {
        self.drbg.is_instantiated()
    }

    /// The instruction the root's first seed was counted on. With RDRAND, the root's strength is
    /// no more than the processor's own generator's (R3).
    pub const fn counted(&self) -> Option<Instruction> {
        self.counted
    }

    /// Whether the other instruction's start-up draws were the nonce.
    pub const fn both(&self) -> bool {
        self.both
    }

    /// Whether the extra bytes went into the first seed.
    pub const fn extra(&self) -> bool {
        self.extra
    }

    /// Healthy instructions, over all processors.
    pub const fn sources(&self) -> u32 {
        self.sources
    }

    /// The counts.
    pub const fn counts(&self) -> RootCounts {
        self.counts
    }

    /// Whether every instruction on every processor has failed (R6): the root then seeds
    /// nothing and answers nothing until one passes a start-up test again.
    pub const fn stopped(&self) -> bool {
        self.drbg.is_instantiated() && self.sources == 0
    }

    /// Whether the root is due a reseed (R6).
    pub const fn due(&self, now: u64) -> bool {
        self.drbg.is_instantiated()
            && (self.drbg.requests_since_seed() >= RESEED_REQUESTS
                || now.saturating_sub(self.seeded_at) >= RESEED_NANOS)
    }

    /// Brings the root's count of healthy instructions up to date with `node`.
    pub fn account(&mut self, node: &mut Node) {
        for which in Instruction::BOTH {
            let healthy = node.is_healthy(which);
            if healthy != node.told_healthy(which) {
                self.sources = if healthy {
                    self.sources.saturating_add(1)
                } else {
                    self.sources.saturating_sub(1)
                };
                node.set_told_healthy(which, healthy);
            }
        }
    }

    /// Gives the root its first state from the seed area of the processor that starts it (R5),
    /// and empties the area. `extra` says the caller put [`seed::EXTRA_LEN`] bytes in
    /// [`seed::extra_slot`].
    ///
    /// It refuses unless region 0 holds a healthy instruction's 1,024 start-up draws and region
    /// 1 the other's, or four further draws of the same one.
    pub fn instantiate(
        &mut self,
        node: &mut Node,
        area: &mut [u8],
        now: u64,
        tsc: u64,
        extra: bool,
    ) -> bool {
        let both = node.region1 == STARTUP_DRAWS;
        let instantiated = self.instantiate_inner(node, area, tsc, extra);
        wipe(area);
        node.region0 = 0;
        node.region1 = 0;
        if instantiated {
            self.seeded_at = now;
            self.both = both;
            self.extra = extra;
            self.counted = Some(if node.is_healthy(Instruction::Rdseed) {
                Instruction::Rdseed
            } else {
                Instruction::Rdrand
            });
            self.account(node);
        }
        instantiated
    }

    fn instantiate_inner(&mut self, node: &Node, area: &mut [u8], tsc: u64, extra: bool) -> bool {
        if self.drbg.is_instantiated()
            || area.len() < seed::AREA_LEN
            || node.region0 != STARTUP_DRAWS
            || !(node.is_healthy(Instruction::Rdseed) || node.is_healthy(Instruction::Rdrand))
        {
            return false;
        }
        let nonce_len = if node.region1 == STARTUP_DRAWS {
            seed::REGION
        } else if node.region1 == NONCE_DRAWS {
            usize::from(NONCE_DRAWS).saturating_mul(8)
        } else {
            return false;
        };
        let Some(head) = seed::write_root_head(area, tsc, extra) else {
            return false;
        };
        let entropy_end = seed::HEAD.saturating_add(seed::REGION);
        match (
            area.get(seed::HEAD..entropy_end),
            area.get(entropy_end..entropy_end.saturating_add(nonce_len)),
            area.get(seed::HEAD.saturating_sub(head)..seed::HEAD),
        ) {
            (Some(entropy), Some(nonce), Some(personalization)) => self
                .drbg
                .instantiate(entropy, nonce, personalization)
                .is_ok(),
            _ => false,
        }
    }

    fn ready(&self) -> Result<(), Answer> {
        if !self.drbg.is_instantiated() {
            Err(Answer::NoGenerator)
        } else if self.sources == 0 {
            Err(Answer::Stopped)
        } else {
            Ok(())
        }
    }

    /// Reseeds the root from `node`'s full store (R6).
    pub fn reseed_from(&mut self, node: &mut Node, now: u64, tsc: u64) -> Answer {
        if let Err(answer) = self.ready() {
            return answer;
        }
        if !node.store_full() {
            return Answer::NotGathered;
        }
        let extra = node.stamp(tsc);
        let reseeded = match node.aside.get(..extra) {
            Some(additional) => self.drbg.reseed(&node.entropy, additional).is_ok(),
            None => false,
        };
        node.clear_store();
        if reseeded {
            self.seeded_at = now;
            self.counts.reseeds = self.counts.reseeds.saturating_add(1);
            Answer::Done
        } else {
            Answer::NotNow
        }
    }

    /// Hands `node` the entropy input and the nonce of its generator's first seed: 80 bytes of
    /// the root's output (R5).
    pub fn seed_processor(&mut self, node: &mut Node) -> Answer {
        if let Err(answer) = self.ready() {
            return answer;
        }
        match self.drbg.generate(&mut node.from_root, &[]) {
            Ok(()) => {
                node.from_root_len = 80;
                self.counts.seeded = self.counts.seeded.saturating_add(1);
                Answer::Done
            }
            Err(_) => {
                wipe(&mut node.from_root);
                node.from_root_len = 0;
                Answer::NotNow
            }
        }
    }

    /// Reseeds `node`'s generator from 64 bytes of the root's output: for a processor none of
    /// whose instructions is healthy (R6).
    pub fn reseed_processor(&mut self, node: &mut Node, now: u64, tsc: u64) -> Answer {
        if let Err(answer) = self.ready() {
            return answer;
        }
        let Some(entropy) = node.from_root.get_mut(..64) else {
            return Answer::NotNow;
        };
        if self.drbg.generate(entropy, &[]).is_err() {
            wipe(&mut node.from_root);
            node.from_root_len = 0;
            return Answer::NotNow;
        }
        node.from_root_len = 64;
        if node.reseed_from_root(now, tsc) {
            self.counts.reseeded = self.counts.reseeded.saturating_add(1);
            Answer::Done
        } else {
            Answer::NotNow
        }
    }

    /// One piece with prediction resistance (R6; SP 800-90A §9.3.1): the root is reseeded from
    /// `node`'s store, which must hold eight RDSEED draws, and then generates.
    pub fn fresh_piece(&mut self, node: &mut Node, out: &mut [u8], now: u64, tsc: u64) -> Answer {
        if let Err(answer) = self.ready() {
            return answer;
        }
        if out.len() > PIECE {
            return Answer::NotNow;
        }
        if !node.store_is_fresh() {
            return Answer::NotGathered;
        }
        let extra = node.stamp(tsc);
        let answered = match node.aside.get(..extra) {
            Some(additional) => self
                .drbg
                .reseed_and_generate(out, &node.entropy, additional)
                .is_ok(),
            None => false,
        };
        node.clear_store();
        if answered {
            self.seeded_at = now;
            self.counts.reseeds = self.counts.reseeds.saturating_add(1);
            self.counts.fresh = self.counts.fresh.saturating_add(1);
            Answer::Done
        } else {
            wipe(out);
            Answer::NotNow
        }
    }

    /// One piece of a plain request, for a root that has no processors' generators under it:
    /// the loader's (R10).
    pub fn piece(&mut self, out: &mut [u8]) -> Answer {
        if let Err(answer) = self.ready() {
            return answer;
        }
        if out.len() > PIECE {
            return Answer::NotNow;
        }
        match self.drbg.generate(out, &[]) {
            Ok(()) => Answer::Done,
            Err(_) => Answer::NotNow,
        }
    }

    /// The standard's uninstantiate function: the root's state is overwritten and there is no
    /// root.
    pub fn uninstantiate(&mut self) {
        self.drbg.uninstantiate();
    }
}

impl Default for Root {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Root {
    /// Prints nothing of the state (CLAUDE.md §61).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Root { .. }")
    }
}
