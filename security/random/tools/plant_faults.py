#!/usr/bin/env python3
"""Plant one fault at a time in origin-random, and see that a test fails.

A check of the tests, not of the code (CLAUDE.md §72): each entry of FAULTS is a small wrong
edit, a test left out, a number changed, a step skipped. The script makes the edit, runs the
crate's tests, puts the file back, and reports which tests failed. A fault is caught when a test
fails or the crate no longer compiles, and the end says how many did each and how many did
neither.

The entries named in MODEL are the negative controls of the randomized model
(`src/tests/model.rs`): defects that need requests to interleave, a thread to move or an
instruction to go dry, which the model is there to notice. For each of them a test of the model
has to fail, whatever else does, and the script says what the model reported. It ends with a
failure if one of them goes unnoticed by the model.

Every edit is an exact string that must occur once. When the code changes so that one no longer
does, the script stops and names it: bring the entry up to date. The files are restored whatever
happens, and the tests are run once more at the end on the restored tree.

Host-only engineering tool; nothing here ships. It edits the working tree while it runs, so
run nothing else in the checkout meanwhile. The tests it runs are the ones `cargo test` runs: the
model's short runs, not its runs at length.

Usage: python3 -I security/random/tools/plant_faults.py <repository root> [part of a fault's name]
       python3 -I security/random/tools/plant_faults.py <repository root> --model
       python3 -I security/random/tools/plant_faults.py <repository root> --strings

With `--model` only the model's negative controls are planted. With `--strings` nothing is edited
or run: it only says which entries' strings no longer occur once.
"""

import re
import subprocess
import sys
from pathlib import Path

if len(sys.argv) not in (2, 3):
    sys.exit(__doc__)
ROOT = Path(sys.argv[1])
# An optional second argument runs only the faults whose name holds it.
ONLY = sys.argv[2] if len(sys.argv) == 3 else ""
SRC = ROOT / "security/random/src"
H, N, R, D, S, T = "health.rs", "node.rs", "root.rs", "drive.rs", "seed.rs", "selftest.rs"

FAULTS = [
 # The health tests.
 ("health: a repeat passes", [(H, "if self.have_last && value == self.last {", "if false && self.have_last && value == self.last {")]),
 ("health: the draw before is not remembered", [(H, "        self.last = value;\n        self.have_last = true;", "        self.have_last = true;")]),
 ("health: a window's first draw coming again passes", [(H, "if self.position != 0 && value == self.first {", "if false && self.position != 0 && value == self.first {")]),
 ("health: the window's first draw is not kept", [(H, "        if self.position == 0 {\n            self.first = value;\n        }\n", "")]),
 ("health: no stuck-bit test", [(H, "if self.ones != u64::MAX || self.zeros != u64::MAX {", "if false {")]),
 ("health: a bit stuck at one passes", [(H, "if self.ones != u64::MAX || self.zeros != u64::MAX {", "if self.ones != u64::MAX {")]),
 ("health: a bit stuck at zero passes", [(H, "if self.ones != u64::MAX || self.zeros != u64::MAX {", "if self.zeros != u64::MAX {")]),
 ("health: zero is a value", [(H, "if value == 0 || value == u64::MAX {", "if value == u64::MAX {")]),
 ("health: all ones is a value", [(H, "if value == 0 || value == u64::MAX {", "if value == 0 {")]),
 ("health: a start-up test is 1,023 draws", [(H, "if self.passed >= STARTUP_DRAWS {", "if self.passed >= STARTUP_DRAWS - 1 {")]),
 ("health: a window is 513 draws", [(H, "if self.position >= WINDOW {", "if self.position > WINDOW {")]),
 ("health: a failure leaves the instruction as it was", [(H, "        self.forget();\n        self.condition = Condition::Unhealthy;\n        self.faults", "        self.forget();\n        self.faults")]),
 ("health: a failure keeps the test's progress", [(H, "    fn fail(&mut self, fault: Fault) -> Offer {\n        self.forget();", "    fn fail(&mut self, fault: Fault) -> Offer {")]),
 ("health: stuck values are not a failure", [(H, "            self.fail(Fault::StuckValue)\n", "            Offer::SetAside\n")]),
 # Tries, the schedule and the store.
 ("node: RDSEED gets 99 tries", [(N, "Self::Rdseed => 100,", "Self::Rdseed => 99,")]),
 ("node: RDRAND gets 11 tries", [(N, "Self::Rdrand => 10,", "Self::Rdrand => 11,")]),
 ("node: a reseed takes seven draws", [(N, "pub const RESEED_DRAWS: u8 = 8;", "pub const RESEED_DRAWS: u8 = 7;")]),
 ("node: a reseed is due after 61 seconds", [(N, "pub const RESEED_NANOS: u64 = 60_000_000_000;", "pub const RESEED_NANOS: u64 = 61_000_000_000;")]),
 ("node: a reseed is due one request late", [(N, "pub const RESEED_REQUESTS: u64 = 1 << 20;", "pub const RESEED_REQUESTS: u64 = (1 << 20) + 1;")]),
 ("node: an instruction rests two seconds", [(N, "pub const REST_NANOS: u64 = 1_000_000_000;", "pub const REST_NANOS: u64 = 2_000_000_000;")]),
 ('node: the clock never makes a reseed due', [(N, '                || passed(now, self.seeded_at, RESEED_NANOS))\n    }\n\n    /// Whether a start-up test', '                || false)\n    }\n\n    /// Whether a start-up test')]),
 ('node: the count never makes a reseed due', [(N, '            && (self.drbg.requests_since_seed() >= RESEED_REQUESTS\n                || passed(now, self.seeded_at, RESEED_NANOS))\n    }\n\n    /// Whether a start-up test', '            && (passed(now, self.seeded_at, RESEED_NANOS))\n    }\n\n    /// Whether a start-up test')]),
 ("node: a failed instruction keeps its draws in the store", [(N, "    fn failed(&mut self, which: Instruction, now: u64) {\n        self.drop_draws(which);", "    fn failed(&mut self, which: Instruction, now: u64) {")]),
 ('node: a failed instruction may be tested again at once', [(N, '        let line = self.line_mut(which);\n        line.failed_at = Some(now);', '        let line = self.line_mut(which);\n        line.failed_at = None;')]),
 ("node: 'not now' does not rest the instruction", [(N, '        if rest {\n            self.line_mut(which).rest_since = Some(now);\n        }\n        self.count_not_now(which);', '        self.count_not_now(which);')]),
 ("node: a draw of stuck values is only 'not now'", [(N, '        if saw_stuck {\n            if matches!(self.line_mut(which).health.stuck()', '        if false && saw_stuck {\n            if matches!(self.line_mut(which).health.stuck()')]),
 ("node: a reseed asks RDRAND first", [(N, "        let which = if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {\n            Instruction::Rdseed\n        } else if rdrand {", "        let which = if rdrand {\n            Instruction::Rdrand\n        } else if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {\n            Instruction::Rdseed\n        } else if false {")]),
 ('node: a resting RDSEED is tried all the same', [(N, 'let rdseed = self.rdseed.health.is_healthy() && !self.rdseed.resting(now);', 'let rdseed = self.rdseed.health.is_healthy();')]),
 ("node: RDSEED draws are dropped when RDRAND takes over", [(N, "                    copy(to, from);\n                    self.aside_have = self.have;", "                    copy(to, from);")]),
 ("node: a reseed's additional input has no time-stamp counter", [(N, "            copy(to, &tsc.to_le_bytes());\n        }\n        if let Some(to) = self.aside.get_mut(8..16)", "            copy(to, &0u64.to_le_bytes());\n        }\n        if let Some(to) = self.aside.get_mut(8..16)")]),
 ("node: a reseed's additional input has no index", [(N, "copy(to, &u64::from(self.index).to_le_bytes());", "copy(to, &0u64.to_le_bytes());")]),
 ('node: a reseed does not start the clock again', [(N, '        if reseeded {\n            self.seeded_at = now;\n            match from {', '        if reseeded {\n            match from {')]),
 ("node: a reseed from RDRAND is never counted as dry", [(N, "                    if self.rdseed.health.is_healthy() {\n                        self.counts.reseeds_dry", "                    if false {\n                        self.counts.reseeds_dry")]),
 ("node: the store is not emptied after a reseed", [(N, "        let from = self.from;\n        self.clear_store();", "        let from = self.from;")]),
 ("node: a test begins again though no reseed is due", [(N, "            if !testing\n                && due\n                && matches!", "            if !testing\n                && matches!")]),
 ('node: a test begins again within the period', [(N, '                && matches!(line.health.condition(), Condition::Unhealthy)\n                && line.may_retest(now)\n            {', '                && matches!(line.health.condition(), Condition::Unhealthy)\n            {')]),
 ('node: a request does not wait for a running test', [(N, '&& (self.due(now) || self.retesting() || self.owes_root() || self.wants_root_look(now))', '&& (self.due(now) || self.owes_root() || self.wants_root_look(now))')]),
 ("node: the root is never told of a change", [(N, "        if self.owes_root() {\n            return Maint::TellRoot;\n        }\n", "")]),
 ("node: a processor's own draws are left out of its first seed", [(N, "let end = seed::HEAD.saturating_add(regions.saturating_mul(seed::REGION));", "let end = seed::HEAD;")]),
 ("node: the root's nonce bytes are used as the entropy input", [(N, "            (Some(entropy), Some(nonce), Some(personalization)) => self\n                .drbg\n                .instantiate(entropy, nonce, personalization)\n                .is_ok(),\n            _ => false,\n        }\n    }\n\n    /// The standard's uninstantiate", "            (Some(entropy), Some(nonce), Some(personalization)) => self\n                .drbg\n                .instantiate(nonce, entropy, personalization)\n                .is_ok(),\n            _ => false,\n        }\n    }\n\n    /// The standard's uninstantiate")]),
 ('node: the seed area is left as it was after a processor starts', [(N, '        self.from_root_len = 0;\n        wipe(area);\n        self.forget_regions();', '        self.from_root_len = 0;\n        self.forget_regions();')]),
 ("node: what the root handed over is left in place", [(N, "        let instantiated = self.instantiate_inner(area, tsc);\n        wipe(&mut self.from_root);", "        let instantiated = self.instantiate_inner(area, tsc);")]),
 ("node: a failed collection leaves the region as it was", [(N, "        if let Some(bytes) = seed::region(area, region) {\n            wipe(bytes);\n        }", "")]),
 ("node: draws are stored high byte first", [(N, "        copy(slot, &value.to_le_bytes());\n        self.set_collected(region, have.saturating_add(1));", "        copy(slot, &value.to_be_bytes());\n        self.set_collected(region, have.saturating_add(1));")]),
 ("node: fresh output from a store of RDRAND draws", [(N, "        self.store_full() && matches!(self.from, Instruction::Rdseed)", "        self.store_full()")]),
 ("node: a piece of 129 bytes", [(N, "pub const PIECE: usize = 128;", "pub const PIECE: usize = 129;")]),
 # The seed area.
 ("seed: another label for a root", [(S, 'b"Origin OS random root, 1"', 'b"Origin OS random root, 2"')]),
 ("seed: a processor's head has no index", [(S, "            PROCESSOR_LABEL,\n            &u64::from(index).to_le_bytes(),", "            PROCESSOR_LABEL,\n            &0u64.to_le_bytes(),")]),
 ("seed: the extra bytes are left out of a root's head", [(S, "        front.checked_add(EXTRA_LEN)", "        Some(front)")]),
 ("seed: a root's nonce is three further draws", [(S, "pub const NONCE_DRAWS: u16 = 4;", "pub const NONCE_DRAWS: u16 = 3;")]),
 # The root.
 ("root: it never stops", [(R, "        } else if self.sources == 0 {\n            Err(Answer::Stopped)", "        } else if false {\n            Err(Answer::Stopped)")]),
 ("root: fresh output without a reseed", [(R, "            Some(additional) => self\n                .drbg\n                .reseed_and_generate(out, &node.entropy, additional)\n                .is_ok(),", "            Some(additional) => {\n                let _ = additional;\n                self.drbg.generate(out, &[]).is_ok()\n            }")]),
 ("root: a recovered instruction is counted as lost", [(R, "                self.sources = if healthy {", "                self.sources = if !healthy {")]),
 ("root: the seed area is left as it was after the root starts", [(R, "        let instantiated = self.instantiate_inner(node, area, tsc, extra);\n        wipe(area);", "        let instantiated = self.instantiate_inner(node, area, tsc, extra);")]),
 ("root: the nonce is taken from region 0", [(R, "            area.get(entropy_end..entropy_end.saturating_add(nonce_len)),", "            area.get(seed::HEAD..seed::HEAD.saturating_add(nonce_len)),")]),
 ("root: a processor is reseeded from 32 bytes", [(R, "        let Some(entropy) = node.from_root.get_mut(..64) else {", "        let Some(entropy) = node.from_root.get_mut(..32) else {")]),
 # Between the steps.
 ("drive: a failed self-test is ignored", [(D, "    if started.is_ok() && self_test {", "    if started.is_ok() {")]),
 ("drive: a request may make 17 draws", [(D, "const REQUEST_DRAWS: u8 = 16;", "const REQUEST_DRAWS: u8 = 17;")]),
 ("drive: a test gets nine draws a request", [(D, "                    if *retested >= RESEED_DRAWS {", "                    if *retested > RESEED_DRAWS {")]),
 ('tries: a caller with interrupts off gets the whole budget', [(N, '        let budget = if quick { 1 } else { which.tries() };', '        let budget = which.tries();')]),
 ("drive: a refused request leaves what was written", [(D, "    if result.is_err() {\n        wipe(out);\n    }\n    result\n}\n\n/// 64 random bits", "    result\n}\n\n/// 64 random bits")]),
 ("drive: a refused fresh request leaves what was written", [(D, "        result = fresh_piece(platform, part);\n        if result.is_err() {\n            break;\n        }\n    }\n    if result.is_err() {\n        wipe(out);\n    }", "        result = fresh_piece(platform, part);\n        if result.is_err() {\n            break;\n        }\n    }")]),
 ('drive: a start-up test that is not ready stays running', [(D, '                    if testing {\n                        ctx.node.abandon_test(which);\n                    }\n', '')]),
 ("drive: no pause between a start's tries", [(D, '            Try::NotReady | Try::SetAside => platform.pause(),\n            // "Not now" is not a failure', '            Try::NotReady | Try::SetAside => {}\n            // "Not now" is not a failure')]),
 ('tries: the count is not started again for a new draw', [(N, '                let line = self.line_mut(which);\n                line.dry = 0;\n                line.stuck = false;\n                let completed', '                let line = self.line_mut(which);\n                line.stuck = false;\n                let completed')]),
 # The review of 2026-11-11: one fault for each fix.
 # 1. A request with prediction resistance is never answered from earlier draws.
 ('lend: a lending keeps what the store held when it began', [(N, '    fn begin_lend(&mut self) -> u64 {\n        self.clear_store();', '    fn begin_lend(&mut self) -> u64 {')]),
 ('lend: the end of a lending leaves its draws', [(N, '        if self.lent_to(number) {\n            self.clear_store();\n        }', '        if false && self.lent_to(number) {\n            self.clear_store();\n        }')]),
 ('fresh: the driver does not end a request that was not answered', [(D, '        let _ = in_step(platform, |ctx| ctx.node.end_lend(number));', '        let _ = number;')]),
 ('fresh: the root takes a store whatever request filled it', [(R, '        if !node.lent_to(number) || !node.store_is_fresh() {', '        if !node.store_is_fresh() {')]),
 ('fresh: a request goes on in a store that is not lent to it', [(N, '            Some(number) if self.lent_to(number) => number,', '            Some(number) => number,')]),
 ('lend: a number does not name its processor', [(N, '        let number = (u64::from(self.index) << 32) | u64::from(self.lends);', '        let number = u64::from(self.lends);')]),
 ("lend: a processor's own reseed takes over lent draws", [(N, '        if self.lent.is_some() {\n            // Draws of a gathering for the root', '        if false && self.lent.is_some() {\n            // Draws of a gathering for the root')]),
 # 2. The root keeps the processors' schedule.
 ('root schedule: no processor looks at the root after its start', [(D, '                if left >= RESEED_DRAWS {\n                    look_at_root(platform, on, left, false)?;\n                }\n', '')]),
 ('root schedule: a processor that has not seen the root does not look', [(N, '            None => true,\n        };\n        should', '            None => false,\n        };\n        should')]),
 ('root schedule: the look is made again at once, without a rest', [(N, '            && (self.rdseed.health.is_healthy() || self.rdrand.health.is_healthy())\n            && self.root_rested(now)\n    }', '            && (self.rdseed.health.is_healthy() || self.rdrand.health.is_healthy())\n    }')]),
 ('root schedule: draws that did not reach the root are kept', [(D, '        if let Some(number) = number {\n            ctx.node.end_lend(number);\n        }\n        if !reseeded && ctx.index == on {', '        if !reseeded && ctx.index == on {')]),
 ("root schedule: a request waits for the root's lock on the root's account", [(D, '                    look_at_root(platform, on, left, false)?;', '                    look_at_root(platform, on, left, true)?;')]),
 ('root schedule: the root is reseeded though it is not due', [(D, '        let due = root.due(ctx.now) && !root.stopped();\n        if !due {\n            ctx.node.saw_root(root.seeded_at());', '        let due = !root.stopped();\n        if !due {\n            ctx.node.saw_root(root.seeded_at());')]),
 ("root schedule: the root's gathering takes a processor's own draws", [(N, '        if self.lent.is_none() && (self.have != 0 || self.aside_have != 0) {\n            return None;\n        }\n', '')]),
 ("root schedule: the root's gathering goes past the request's draws", [(D, '        if drawn >= left {\n            break;\n        }', '        if false && drawn >= left {\n            break;\n        }')]),
 ('root schedule: a request with nine draws behind it still looks at the root', [(D, '                if left >= RESEED_DRAWS {\n                    look_at_root(platform, on, left, false)?;', '                if true {\n                    look_at_root(platform, on, left, false)?;')]),
 ('root schedule: a processor that starts does not reseed a root that is due', [(D, '    look_at_root(platform, on, REQUEST_DRAWS, true)?;\n', '')]),
 # 3. A start waits out a 'not now'.
 ("start: an instruction is given up at its first 'not now'", [(D, 'const START_PATIENCE: u32 = 8;', 'const START_PATIENCE: u32 = 0;')]),
 ("start: a ninth 'not now' is waited out", [(D, 'const START_PATIENCE: u32 = 8;', 'const START_PATIENCE: u32 = 9;')]),
 ("tries: a 'not now' is not counted", [(N, '        self.count_not_now(which);\n        Try::NotNow', '        Try::NotNow')]),
 ("start: a 'not now' that a start waits out rests the instruction", [(N, '        let (value, completed) = match self.draw(source, which, now, false, false) {', '        let (value, completed) = match self.draw(source, which, now, false, true) {')]),
 # 4. Start-up draws of an instruction that has failed since are not in a first seed.
 ("regions: a failed instruction's draws go into a processor's first seed", [(N, '        self.settle_regions(area);\n        let Some(head) = seed::write_processor_head', '        let Some(head) = seed::write_processor_head')]),
 ("regions: a failed instruction's draws go into a root's first seed", [(R, '        node.settle_regions(area);\n        let both', '        let both')]),
 ("regions: the second instruction's draws are not moved into the first's place", [(N, '        if self.region0 == 0 && self.region1 == STARTUP_DRAWS {', '        if false && self.region0 == 0 && self.region1 == STARTUP_DRAWS {')]),
 ('regions: a region does not say whose draws it holds', [(N, '        self.set_collected(region, have.saturating_add(1));\n        self.set_region_of(region, Some(which));', '        self.set_collected(region, have.saturating_add(1));')]),
 ('regions: region 1 keeps its draws after they moved down', [(N, '                self.region0_of = self.region1_of;\n            }\n            self.empty_region(area, 1);', '                self.region0_of = self.region1_of;\n            }')]),
 # 5. Waits are measured from when they began.
 ('clock: a clock that steps back prolongs a wait', [(N, '    now.wrapping_sub(since) >= period', '    now.saturating_sub(since) >= period')]),
 ("clock: the root's clock never makes it due", [(R, '                || passed(now, self.seeded_at, RESEED_NANOS))', '                || false)')]),
 # 6. A thread that moves.
 ('moving: a processor that needs nothing is reseeded from the root', [(R, '        if !node.wants_root(now) {\n            return Answer::NotGathered;\n        }\n', '')]),
 ('moving: the upkeep goes on where the thread arrives', [(D, '                    (ctx.index == on).then(|| {\n                        ctx.node\n                            .maintain(ctx.source, ctx.now, ctx.tsc, allow, quick)\n                    })', '                    Some(\n                        ctx.node\n                            .maintain(ctx.source, ctx.now, ctx.tsc, allow, quick),\n                    )')]),
 ('moving: the root is told on whichever processor the thread is', [(D, '                    let here = ctx.index == on;', '                    let here = ctx.index == on || true;')]),
 ('moving: a gathering for the root begins where the thread arrives', [(D, '        if ctx.index == on {\n            ctx.node.begin_root_gather()\n        } else {\n            None\n        }', '        ctx.node.begin_root_gather()')]),
 ('moving: a failed look rests whichever processor the thread is on', [(D, '        if !reseeded && ctx.index == on {', '        if !reseeded {')]),
 # 7. Nothing stays behind.
 ('left behind: with no root the extra bytes stay in the area', [(D, '        if !instantiated {\n            wipe(ctx.area);\n        }', '')]),
 ('left behind: uninstantiate leaves the store', [(N, '        self.drbg.uninstantiate();\n        self.clear_store();', '        self.drbg.uninstantiate();')]),
 ('left behind: a failed RDSEED leaves the draws set aside', [(N, '        if which == Instruction::Rdseed {\n            wipe(&mut self.aside);\n            self.aside_have = 0;\n        }', '')]),
 ('left behind: what the root handed over for a reseed stays', [(N, '            _ => false,\n        };\n        wipe(&mut self.from_root);\n        self.from_root_len = 0;\n        if reseeded {', '            _ => false,\n        };\n        if reseeded {')]),
 # Not observable by a test on a host, and listed to say so: a local variable's bytes.
 ('left behind: next_u64 leaves its copy of the value (not observable)', [(D, '    wipe(&mut bytes);\n    filled.map(|()| value)', '    filled.map(|()| value)')]),
 # The second review, of 2026-11-11: one fault for each fix.
 ('tries: a try that gives no value is not counted', [(N, '        line.dry = line.dry.saturating_add(1);\n', '')]),
 ("tries: a stuck value among a draw's tries is forgotten", [(N, '        line.stuck |= stuck;\n', '')]),
 ('root schedule: a look does not note when the root was seeded', [(N, '        self.root_seen = Some(seeded);', '        let _ = seeded;')]),
 ("root schedule: the root's own period does not make a processor look", [(N, '            Some(seeded) => passed(now, seeded, RESEED_NANOS),\n            None => true,', '            Some(_) => false,\n            None => true,')]),
 ('root schedule: a processor with no healthy instruction looks at the root', [(N, '        should\n            && (self.rdseed.health.is_healthy() || self.rdrand.health.is_healthy())\n            && self.root_rested(now)', '        should && self.root_rested(now)')]),
 ('root schedule: a root reseeded by a processor is still to be looked at by it', [(R, '            self.counts.reseeds = self.counts.reseeds.saturating_add(1);\n            node.saw_root(now);\n            Answer::Done\n        } else {\n            Answer::NotNow\n        }\n    }\n\n    /// Hands `node`', '            self.counts.reseeds = self.counts.reseeds.saturating_add(1);\n            Answer::Done\n        } else {\n            Answer::NotNow\n        }\n    }\n\n    /// Hands `node`')]),
 ('lend: the root reseeds itself from a store whatever its number', [(R, '        if !node.lent_full(number) {', '        if !node.store_full() {')]),
 ('lend: a gathering goes on in a store that is no longer its own', [(N, '        if !self.lent_to(number) {\n            return Maint::Delayed;\n        }\n', '')]),
 ('lend: a full store counts as lent to anyone', [(N, '        self.lent_to(number) && self.store_full()', '        self.store_full()')]),
 ('root: a refusal is not recorded on the processor refused', [(R, '        if answer != Answer::Done {\n            node.root_refused(now);\n        }\n', '')]),
 # The randomized model's negative controls that are not entries above (see MODEL): four of the
 # six defects it was held to notice on 2026-11-11. Each puts back something the second review
 # found, in a form the faults above do not have.
 ("model: a look notes its own time, not when the root was seeded", [(D, '            ctx.node.saw_root(root.seeded_at());', '            ctx.node.saw_root(ctx.now);')]),
 ('model: a gathering for the root goes on from what a lent store holds', [(N, '        Some(self.begin_lend())\n    }', '        if let Some(number) = self.lent {\n            return Some(number);\n        }\n        Some(self.begin_lend())\n    }')]),
 ("model: the root's gathering is capped at eight draws in all", [(D, '        if drawn >= left {\n            break;\n        }', '        if drawn >= left.min(RESEED_DRAWS) {\n            break;\n        }')]),
 ('model: a give-up is recorded by a later step', [
     (N, '    /// Rests `which` for [`REST_NANOS`]: for a start that has waited out as many "not now"', '    /// Planted.\n    pub fn planted_give_up(&mut self, which: Instruction) {\n        self.count_not_now(which);\n    }\n\n    /// Rests `which` for [`REST_NANOS`]: for a start that has waited out as many "not now"'),
     (D, '            Try::Failed | Try::Unwanted | Try::NotNow => {\n                if purpose == Purpose::Retest {\n                    stop_retest(&mut allow);\n                }\n            }', '            Try::Failed | Try::Unwanted | Try::NotNow => {\n                if purpose == Purpose::Retest {\n                    stop_retest(&mut allow);\n                }\n                if outcome == Try::NotNow {\n                    in_step(platform, |ctx| ctx.node.planted_give_up(which))?;\n                }\n            }')]),
 # The self-test.
 ("selftest: the answer is not compared", [(T, "    difference == 0 && !drbg.is_instantiated()", "    !drbg.is_instantiated()")]),
 ("selftest: only the first step is run", [(T, "    for step in vector.steps {\n        let done", "    for step in vector.steps.iter().take(1) {\n        let done")]),
 ("selftest: only the first vector is run", [(T, "    vectors.iter().all(passes)", "    vectors.iter().take(1).all(passes)")]),
]


# The model's negative controls: a test of `src/tests/model.rs` has to fail for each. The first
# two are faults for the reviews' fixes, which the scripted tests catch as well; the others are
# the model's own entries.
MODEL = (
    "moving: the upkeep goes on where the thread arrives",
    "lend: the root reseeds itself from a store whatever its number",
    "model: a look notes its own time, not when the root was seeded",
    "model: a gathering for the root goes on from what a lent store holds",
    "model: the root's gathering is capped at eight draws in all",
    "model: a give-up is recorded by a later step",
)
# What the names of the model's tests hold.
MODEL_TESTS = "tests::model::"

names = [name for name, _ in FAULTS]
for name in MODEL:
    if names.count(name) != 1:
        sys.exit(f"MODEL names {name!r}, which is {names.count(name)} entries of FAULTS")


def run():
    r = subprocess.run(["cargo", "test", "-j", "2", "--quiet", "--package", "origin-random", "--lib"],
                       cwd=ROOT, capture_output=True, text=True)
    out = r.stdout + r.stderr
    failed = set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M))
    failed |= set(re.findall(r"^---- (\S+) stdout ----", out, re.M))
    compile_error = "error[" in out or "error: could not compile" in out
    return r.returncode, sorted(failed), compile_error, model_says(out)


def model_says(out):
    """What the model's failing tests reported, with the numbers taken out: the rules that were
    broken, and what a test asserted besides."""
    said = set(re.findall(r"^ +seed \d+, first at request \d+: (.*)$", out, re.M))
    for line in re.findall(r"panicked at [^\n]*tests/model\.rs[^\n]*\n([^\n]*)", out):
        if "the rules were broken" not in line:
            said.add(line.strip())
    return sorted(set(re.sub(r"\d+", "N", line) for line in said))


FILES = (H, N, R, D, S, T)
originals = {f: (SRC / f).read_text() for f in FILES}
if ONLY == "--strings":
    stale = 0
    for n, (name, edits) in enumerate(FAULTS, 1):
        for f, old, _ in edits:
            if originals[f].count(old) != 1:
                stale += 1
                print(f"fault {n} ({name}): its string occurs {originals[f].count(old)} times in {f}")
    sys.exit(f"{stale} of {len(FAULTS)} entries are out of date" if stale else 0)
unnoticed = []
restored = False
try:
    code, failed, _, _ = run()
    if code != 0:
        sys.exit(f"the unmodified tree does not pass: {failed}")
    print("baseline: passes", flush=True)
    planted = failing = uncompiled = by_model = only_model = controls = 0
    missed = []
    for n, (name, edits) in enumerate(FAULTS, 1):
        if ONLY == "--model":
            if name not in MODEL:
                continue
        elif ONLY not in name:
            continue
        planted += 1
        try:
            for f, old, new in edits:
                text = (SRC / f).read_text()
                if text.count(old) != 1:
                    sys.exit(f"fault {n} ({name}): {old!r} occurs {text.count(old)} times in {f}")
                (SRC / f).write_text(text.replace(old, new))
            code, failed, compile_error, said = run()
        finally:
            for f, text in originals.items():
                (SRC / f).write_text(text)
        models = [t.split(MODEL_TESTS, 1)[-1] for t in failed if MODEL_TESTS in t]
        others = [t.split("::", 1)[-1] for t in failed if MODEL_TESTS not in t]
        if code == 0:
            verdict = "NOT CAUGHT"
            missed.append(name)
        elif compile_error and not failed:
            verdict = "caught: does not compile"
            uncompiled += 1
        else:
            failing += 1
            by_model += bool(models)
            only_model += bool(models) and not others
            shown = others[:3] if others else ["model::" + t for t in models[:3]]
            verdict = (f"caught by {len(failed)} test(s), {len(models)} of them the model's: "
                       + ", ".join(shown) + (" ..." if len(failed) > len(shown) else ""))
        print(f"{n:3}. {name}: {verdict}", flush=True)
        if name in MODEL:
            controls += 1
            if models:
                print(f"       the model's tests: {', '.join(models)}", flush=True)
                for line in said:
                    print(f"       the model says: {line}", flush=True)
            else:
                unnoticed.append(name)
                print("       NOT NOTICED BY THE MODEL", flush=True)
    print(f"{planted} planted: {failing} fail a test, {uncompiled} do not compile, "
          f"{len(missed)} not caught", flush=True)
    for name in missed:
        print(f"not caught: {name}", flush=True)
    print(f"the model's tests fail for {by_model} of the {failing}, and for {only_model} of those "
          "no other test does", flush=True)
    print(f"the model's negative controls: {controls - len(unnoticed)} of {controls} fail a test "
          "of the model", flush=True)
    for name in unnoticed:
        print(f"not noticed by the model: {name}", flush=True)
finally:
    for f, text in originals.items():
        (SRC / f).write_text(text)
    code, failed, _, _ = run()
    restored = code == 0
    print("restored tree: " + ("passes" if restored else f"FAILS: {failed}"), flush=True)
if unnoticed or not restored:
    sys.exit(1)
