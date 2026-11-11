#!/usr/bin/env python3
"""Plant one fault at a time in origin-random, and see that a test fails.

A check of the tests, not of the code (CLAUDE.md §72): each entry of FAULTS is a small wrong
edit, a test left out, a number changed, a step skipped. The script makes the edit, runs the
crate's tests, puts the file back, and reports which tests failed. A fault is caught when a test
fails or the crate no longer compiles.

Every edit is an exact string that must occur once. When the code changes so that one no longer
does, the script stops and names it: bring the entry up to date. The files are restored whatever
happens, and the tests are run once more at the end on the restored tree.

Host-only engineering tool; nothing here ships. It edits the working tree while it runs, so
run nothing else in the checkout meanwhile.

Usage: python3 -I security/random/tools/plant_faults.py <repository root>
"""

import re
import subprocess
import sys
from pathlib import Path

if len(sys.argv) != 2:
    sys.exit(__doc__)
ROOT = Path(sys.argv[1])
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
 ("node: the clock never makes a reseed due", [(N, "                || now.saturating_sub(self.seeded_at) >= RESEED_NANOS)\n    }\n\n    /// Whether a start-up test", "                || false)\n    }\n\n    /// Whether a start-up test")]),
 ("node: the count never makes a reseed due", [(N, "            && (self.drbg.requests_since_seed() >= RESEED_REQUESTS\n                || now.saturating_sub(self.seeded_at) >= RESEED_NANOS)\n    }\n\n    /// Whether a start-up test", "            && (now.saturating_sub(self.seeded_at) >= RESEED_NANOS)\n    }\n\n    /// Whether a start-up test")]),
 ("node: a failed instruction keeps its draws in the store", [(N, "    fn failed(&mut self, which: Instruction, now: u64) {\n        self.drop_draws(which);", "    fn failed(&mut self, which: Instruction, now: u64) {")]),
 ("node: a failed instruction may be tested again at once", [(N, "        self.line_mut(which).retest_after = now.saturating_add(RESEED_NANOS);", "        self.line_mut(which).retest_after = now;")]),
 ("node: 'not now' does not rest the instruction", [(N, "        self.line_mut(which).rest_until = now.saturating_add(REST_NANOS);\n        match which {", "        match which {")]),
 ("node: a draw of stuck values is only 'not now'", [(N, "        if saw_set_aside {\n            if matches!", "        if false {\n            if matches!")]),
 ("node: a reseed asks RDRAND first", [(N, "        let which = if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {\n            Instruction::Rdseed\n        } else if rdrand {", "        let which = if rdrand {\n            Instruction::Rdrand\n        } else if rdseed && (self.have == 0 || self.from == Instruction::Rdseed) {\n            Instruction::Rdseed\n        } else if false {")]),
 ("node: a resting RDSEED is tried all the same", [(N, "let rdseed = self.rdseed.health.is_healthy() && now >= self.rdseed.rest_until;", "let rdseed = self.rdseed.health.is_healthy();")]),
 ("node: RDSEED draws are dropped when RDRAND takes over", [(N, "                    copy(to, from);\n                    self.aside_have = self.have;", "                    copy(to, from);")]),
 ("node: a reseed's additional input has no time-stamp counter", [(N, "            copy(to, &tsc.to_le_bytes());\n        }\n        if let Some(to) = self.aside.get_mut(8..16)", "            copy(to, &0u64.to_le_bytes());\n        }\n        if let Some(to) = self.aside.get_mut(8..16)")]),
 ("node: a reseed's additional input has no index", [(N, "copy(to, &u64::from(self.index).to_le_bytes());", "copy(to, &0u64.to_le_bytes());")]),
 ("node: a reseed does not start the clock again", [(N, "        if reseeded {\n            self.seeded_at = now;\n            match from {", "        if reseeded {\n            match from {")]),
 ("node: a reseed from RDRAND is never counted as dry", [(N, "                    if self.rdseed.health.is_healthy() {\n                        self.counts.reseeds_dry", "                    if false {\n                        self.counts.reseeds_dry")]),
 ("node: the store is not emptied after a reseed", [(N, "        let from = self.from;\n        self.clear_store();", "        let from = self.from;")]),
 ("node: a test begins again though no reseed is due", [(N, "            if !testing\n                && due\n                && matches!", "            if !testing\n                && matches!")]),
 ("node: a test begins again within the period", [(N, "                && matches!(line.health.condition(), Condition::Unhealthy)\n                && now >= line.retest_after\n            {", "                && matches!(line.health.condition(), Condition::Unhealthy)\n            {")]),
 ("node: a request does not wait for a running test", [(N, "&& (self.due(now) || self.retesting() || self.owes_root())", "&& (self.due(now) || self.owes_root())")]),
 ("node: the root is never told of a change", [(N, "        if self.owes_root() {\n            return Maint::TellRoot;\n        }\n", "")]),
 ("node: a processor's own draws are left out of its first seed", [(N, "let end = seed::HEAD.saturating_add(regions.saturating_mul(seed::REGION));", "let end = seed::HEAD;")]),
 ("node: the root's nonce bytes are used as the entropy input", [(N, "            (Some(entropy), Some(nonce), Some(personalization)) => self\n                .drbg\n                .instantiate(entropy, nonce, personalization)\n                .is_ok(),\n            _ => false,\n        }\n    }\n\n    /// The standard's uninstantiate", "            (Some(entropy), Some(nonce), Some(personalization)) => self\n                .drbg\n                .instantiate(nonce, entropy, personalization)\n                .is_ok(),\n            _ => false,\n        }\n    }\n\n    /// The standard's uninstantiate")]),
 ("node: the seed area is left as it was after a processor starts", [(N, "        self.from_root_len = 0;\n        wipe(area);\n        self.region0 = 0;", "        self.from_root_len = 0;\n        self.region0 = 0;")]),
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
 ("drive: a caller with interrupts off gets the whole budget", [(D, "let budget = if self.quick { 1 } else { which.tries() };", "let budget = which.tries();")]),
 ("drive: a refused request leaves what was written", [(D, "    if result.is_err() {\n        wipe(out);\n    }\n    result\n}\n\n/// 64 random bits", "    result\n}\n\n/// 64 random bits")]),
 ("drive: a refused fresh request leaves what was written", [(D, "        result = fresh_piece(platform, part);\n        if result.is_err() {\n            break;\n        }\n    }\n    if result.is_err() {\n        wipe(out);\n    }", "        result = fresh_piece(platform, part);\n        if result.is_err() {\n            break;\n        }\n    }")]),
 ("drive: a start-up test that is not ready stays running", [(D, "                        if !failed && testing {\n                            ctx.node.abandon_test(which);\n                        }\n", "")]),
 ("drive: no pause between tries", [(D, "                    return Ok(Collected::Without);\n                }\n                platform.pause();", "                    return Ok(Collected::Without);\n                }")]),
 ("drive: the count of tries is not started again for a new draw", [(D, "            Try::Stored | Try::Completed | Try::Failed | Try::Unwanted => {\n                self.of = None;\n                None\n            }", "            Try::Stored | Try::Completed | Try::Failed | Try::Unwanted => None,")]),
 ("drive: a full store is asked for fresh output without the root being told", [(D, "                let answer = in_root_step(platform, |ctx, root| {\n                    root.account(ctx.node);\n                    root.fresh_piece(ctx.node, out, ctx.now, ctx.tsc)", "                let answer = in_root_step(platform, |ctx, root| {\n                    root.fresh_piece(ctx.node, out, ctx.now, ctx.tsc)")]),
 # The self-test.
 ("selftest: the answer is not compared", [(T, "    difference == 0 && !drbg.is_instantiated()", "    !drbg.is_instantiated()")]),
 ("selftest: only the first step is run", [(T, "    for step in vector.steps {\n        let done", "    for step in vector.steps.iter().take(1) {\n        let done")]),
 ("selftest: only the first vector is run", [(T, "    VECTORS.iter().all(passes) && refuses()", "    VECTORS.iter().take(1).all(passes) && refuses()")]),
]


def run():
    r = subprocess.run(["cargo", "test", "-j", "2", "--quiet", "--package", "origin-random", "--lib"],
                       cwd=ROOT, capture_output=True, text=True)
    out = r.stdout + r.stderr
    failed = set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M))
    failed |= set(re.findall(r"^---- (\S+) stdout ----", out, re.M))
    compile_error = "error[" in out or "error: could not compile" in out
    return r.returncode, sorted(failed), compile_error


FILES = (H, N, R, D, S, T)
originals = {f: (SRC / f).read_text() for f in FILES}
try:
    code, failed, _ = run()
    if code != 0:
        sys.exit(f"the unmodified tree does not pass: {failed}")
    print("baseline: passes", flush=True)
    caught = 0
    missed = []
    for n, (name, edits) in enumerate(FAULTS, 1):
        try:
            for f, old, new in edits:
                text = (SRC / f).read_text()
                if text.count(old) != 1:
                    sys.exit(f"fault {n} ({name}): {old!r} occurs {text.count(old)} times in {f}")
                (SRC / f).write_text(text.replace(old, new))
            code, failed, compile_error = run()
        finally:
            for f, text in originals.items():
                (SRC / f).write_text(text)
        if code == 0:
            verdict = "NOT CAUGHT"
            missed.append(name)
        elif compile_error and not failed:
            verdict = "caught: does not compile"
            caught += 1
        else:
            names = [t.split("::", 1)[-1] for t in failed]
            verdict = f"caught by {len(names)} test(s): " + ", ".join(names[:3]) + (" ..." if len(names) > 3 else "")
            caught += 1
        print(f"{n:2}. {name}: {verdict}", flush=True)
    print(f"{caught} of {len(FAULTS)} caught", flush=True)
    for name in missed:
        print(f"not caught: {name}", flush=True)
finally:
    for f, text in originals.items():
        (SRC / f).write_text(text)
    code, failed, _ = run()
    print("restored tree: " + ("passes" if code == 0 else f"FAILS: {failed}"), flush=True)
