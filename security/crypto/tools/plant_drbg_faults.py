#!/usr/bin/env python3
"""Plant one fault at a time in origin-crypto's HMAC and HMAC_DRBG, and see that a test fails.

A check of the tests, not of the code (CLAUDE.md §72): each entry of FAULTS is a small wrong
edit, a step left out, a constant changed, a check dropped. The script makes the edit, runs the
HMAC and HMAC_DRBG tests, puts the file back, and reports which tests failed. A fault is caught
when a test fails or the crate no longer compiles. The tests are the crate's own and, apart from
them, NIST's vectors for the generator (`tests/kat`), so the report also says which faults NIST's
answers catch by themselves.

Two faults at the end cannot be caught and are listed to say so: leaving a stack buffer
un-overwritten changes nothing a test in safe code can observe. That those overwrites are in
the built object is checked another way (SOURCE_PROVENANCE.md).

Every edit is an exact string that must occur once. When the code changes so that one no longer
does, the script stops and names it: bring the entry up to date. The files are restored whatever
happens, and the tests are run once more at the end on the restored tree.

Host-only engineering tool; nothing here ships. It edits the working tree while it runs, so
run nothing else in the checkout meanwhile.

Usage: python3 -I security/crypto/tools/plant_drbg_faults.py <repository root>
"""

import re
import subprocess
import sys
from pathlib import Path

if len(sys.argv) != 2:
    sys.exit(__doc__)
ROOT = Path(sys.argv[1])
SRC = ROOT / "security/crypto/src"
H, D, S, W, B = "hmac.rs", "hmac_drbg.rs", "sha512.rs", "wipe.rs", "block.rs"

FAULTS = [
 ("hmac: ipad is 0x35", [(H, "const IPAD: u8 = 0x36;", "const IPAD: u8 = 0x35;")]),
 ("hmac: opad is 0x5d", [(H, "const OPAD: u8 = 0x5c;", "const OPAD: u8 = 0x5d;")]),
 ("hmac: a block-length key is hashed too", [(H, "if key.len() > BLOCK {", "if key.len() >= BLOCK {")]),
 ("hmac: a long key is cut, not hashed", [(H, "if key.len() > BLOCK {", "if key.len() > usize::MAX - 1 {")]),
 ("hmac: the outer hash leaves the inner digest out", [(H, "        hash.update_secret(&digest);\n        hash.finalize_secret(out);", "        hash.finalize_secret(out);")]),
 ("hmac: the outer pad is K0 ^ ipad ^ opad ^ opad", [(H, "*byte ^= IPAD ^ OPAD;", "*byte ^= IPAD;")]),
 ("hmac: only the first part of a message is hashed", [(H, "        for part in parts {\n            hash.update_secret(part);", "        for part in parts.iter().take(1) {\n            hash.update_secret(part);")]),
 ("hmac: the streaming MAC starts from the outer state", [(H, "mac.message = mac.key.inner.clone();", "mac.message = mac.key.outer.clone();")]),
 ("drbg: the second round uses separator 0x00", [(D, "self.next_key(0x01, provided);", "self.next_key(0x00, provided);")]),
 ("drbg: the second round is skipped", [(D, "if provided.iter().any(|part| !part.is_empty()) {", "if provided.iter().any(|part| !part.is_empty()) && false {")]),
 ("drbg: the second round always runs", [(D, "if provided.iter().any(|part| !part.is_empty()) {", "if provided.iter().any(|part| !part.is_empty()) || true {")]),
 ("drbg: no update with the additional input before generating", [(D, "        if !additional_input.is_empty() {\n            self.update([additional_input, &[], &[]]);\n        }\n", "")]),
 ("drbg: the update after generating leaves the additional input out", [(D, "        self.update([additional_input, &[], &[]]);\n        self.reseed_counter = self.reseed_counter.saturating_add(1);", "        self.update([&[], &[], &[]]);\n        self.reseed_counter = self.reseed_counter.saturating_add(1);")]),
 ("drbg: no update after generating", [(D, "        self.update([additional_input, &[], &[]]);\n        self.reseed_counter = self.reseed_counter.saturating_add(1);", "        self.reseed_counter = self.reseed_counter.saturating_add(1);")]),
 ("drbg: V does not move between output blocks", [(D, "        for chunk in out.chunks_mut(OUT) {\n            self.next_v();", "        self.next_v();\n        for chunk in out.chunks_mut(OUT) {")]),
 ("drbg: the count is not advanced", [(D, "        self.reseed_counter = self.reseed_counter.saturating_add(1);\n", "")]),
 ("drbg: reseed is asked for one request early", [(D, "if self.reseed_counter > self.reseed_interval {\n            return Err(DrbgError::ReseedRequired);", "if self.reseed_counter >= self.reseed_interval {\n            return Err(DrbgError::ReseedRequired);")]),
 ("drbg: the count is never checked", [(D, "if self.reseed_counter > self.reseed_interval {\n            return Err(DrbgError::ReseedRequired);", "if self.reseed_counter > u64::MAX - 1 {\n            return Err(DrbgError::ReseedRequired);")]),
 ("drbg: a reseed does not start the count again", [(D, "        self.update([entropy_input, additional_input, &[]]);\n        self.reseed_counter = 1;", "        self.update([entropy_input, additional_input, &[]]);")]),
 ("drbg: a reseed leaves the additional input out", [(D, "self.update([entropy_input, additional_input, &[]]);", "self.update([entropy_input, &[], &[]]);")]),
 ("drbg: a reseed takes the additional input first", [(D, "self.update([entropy_input, additional_input, &[]]);", "self.update([additional_input, entropy_input, &[]]);")]),
 ("drbg: V starts as zeros", [(D, "self.v = [0x01; OUT];", "self.v = [0x00; OUT];")]),
 ("drbg: the key starts as ones", [(D, "self.key.set(&[0x00; OUT]);", "self.key.set(&[0x01; OUT]);")]),
 ("drbg: the personalization string is ignored", [(D, "self.update([entropy_input, nonce, personalization_string]);", "self.update([entropy_input, nonce, &[]]);")]),
 ("drbg: the nonce comes before the entropy input", [(D, "self.update([entropy_input, nonce, personalization_string]);", "self.update([nonce, entropy_input, personalization_string]);")]),
 ("drbg: prediction resistance passes the additional input on to generate", [(D, "self.generate(out, &[])", "self.generate(out, additional_input)")]),
 ("drbg: prediction resistance reseeds before it checks the request", [(D, "        self.check_request(out, additional_input)?;\n        self.reseed(entropy_input, additional_input)?;", "        self.reseed(entropy_input, additional_input)?;")]),
 ("drbg: uninstantiate leaves V", [(D, "        self.key.wipe();\n        wipe(&mut self.v);\n", "        self.key.wipe();\n")]),
 ("drbg: uninstantiate leaves the key", [(D, "        self.key.wipe();\n        wipe(&mut self.v);\n", "        wipe(&mut self.v);\n")]),
 ("drbg: uninstantiate leaves the generator instantiated", [(D, "        self.reseed_counter = 0;\n        self.instantiated = false;\n    }", "        self.reseed_counter = 0;\n    }")]),
 ("drbg: the new key is computed and not used", [(D, "        self.key.set(&key);\n        wipe(&mut key);", "        wipe(&mut key);")]),
 ("drbg: the new V is computed and not kept", [(D, "        self.v = v;\n        wipe(&mut v);", "        wipe(&mut v);")]),
 ("drbg: 31 bytes of entropy input are enough", [(D, "pub const MIN_ENTROPY_LEN: usize = 32;", "pub const MIN_ENTROPY_LEN: usize = 31;")]),
 ("drbg: 33 bytes of entropy input are needed", [(D, "pub const MIN_ENTROPY_LEN: usize = 32;", "pub const MIN_ENTROPY_LEN: usize = 33;")]),
 ("drbg: 15 bytes of nonce are enough", [(D, "pub const MIN_NONCE_LEN: usize = 16;", "pub const MIN_NONCE_LEN: usize = 15;")]),
 ("drbg: the nonce's length is not checked", [(D, "if nonce.len() < Self::MIN_NONCE_LEN {", "if nonce.len() > usize::MAX - 1 {")]),
 ("drbg: a request may be 65537 bytes", [(D, "pub const MAX_REQUEST_LEN: usize = 1 << 16;", "pub const MAX_REQUEST_LEN: usize = (1 << 16) + 1;")]),
 ("drbg: a second instantiation replaces the state", [(D, "        if self.instantiated {\n            return Err(DrbgError::AlreadyInstantiated);\n        }\n", "")]),
 ("drbg: a reseed works with no state", [(D, "        if !self.instantiated {\n            return Err(DrbgError::NotInstantiated);\n        }\n        // §9.2's order", "        // §9.2's order")]),
 ("drbg: an interval of zero is accepted", [(D, "if reseed_interval == 0 || reseed_interval > Self::MAX_RESEED_INTERVAL {", "if reseed_interval > Self::MAX_RESEED_INTERVAL {")]),
 ("drbg: an interval above 2^48 is accepted", [(D, "if reseed_interval == 0 || reseed_interval > Self::MAX_RESEED_INTERVAL {", "if reseed_interval == 0 {")]),
 ("drbg: the given interval is ignored", [(D, "            reseed_counter: 0,\n            reseed_interval,\n", "            reseed_counter: 0,\n            reseed_interval: Self::MAX_RESEED_INTERVAL,\n")]),
 ("drbg: the count is checked before the request's length", [(D, "        self.check_request(out, additional_input)?;\n        if self.reseed_counter > self.reseed_interval {\n            return Err(DrbgError::ReseedRequired);\n        }\n", "        if self.instantiated && self.reseed_counter > self.reseed_interval {\n            return Err(DrbgError::ReseedRequired);\n        }\n        self.check_request(out, additional_input)?;\n")]),
 ("drbg and hmac key: Clone is derived", [(D, "pub struct HmacDrbgSha512 {", "#[derive(Clone)]\npub struct HmacDrbgSha512 {"), (H, "pub(crate) struct Key {", "#[derive(Clone)]\npub(crate) struct Key {")]),
 ("drbg: Debug prints V", [(D, 'f.write_str("HmacDrbgSha512 { .. }")', 'write!(f, "HmacDrbgSha512 {{ v: {:?} }}", self.v)')]),
 ("sha512: the secret finalize does not overwrite the hasher", [(S, "            *dst = word.to_be_bytes();\n        }\n        self.wipe();\n    }", "            *dst = word.to_be_bytes();\n        }\n    }")]),
 ("sha512: wipe leaves the length", [(S, "        self.state = H512;\n        self.bytes = 0;\n    }", "        self.state = H512;\n    }")]),
 ("sha512: wipe leaves the chaining value", [(S, "        wipe_words(&mut self.state);\n        self.buffer.wipe();\n        self.state = H512;", "        self.buffer.wipe();")]),
 ("block: wipe leaves the buffered length", [(B, "        crate::wipe::wipe(&mut self.buf);\n        self.len = 0;", "        crate::wipe::wipe(&mut self.buf);")]),
 ("wipe: bytes are not overwritten", [(W, "    bytes.fill(0);\n", "")]),
 ("wipe: words are not overwritten", [(W, "    words.fill(0);\n", "")]),
 # Not observable from safe code: listed to record that the tests cannot see it.
 ("sha512: the schedule ring is not overwritten (not observable)", [(S, "    rounds_out_of_line(state, block, &mut w);\n    wipe_words(&mut w);", "    rounds_out_of_line(state, block, &mut w);")]),
 ("hmac: K0 is not overwritten (not observable)", [(H, "        self.outer.update_secret(&k0);\n        wipe(&mut k0);", "        self.outer.update_secret(&k0);")]),
]

def run():
    """The crate's HMAC and HMAC_DRBG tests, then NIST's vectors for the generator."""
    code, failed, compile_error = 0, set(), False
    for selection in (["--lib", "hmac"], ["--test", "kat", "drbg"]):
        r = subprocess.run(["cargo", "test", "-j", "2", "--quiet", "--package", "origin-crypto", *selection],
                           cwd=ROOT, capture_output=True, text=True)
        out = r.stdout + r.stderr
        failed |= set(re.findall(r"^test (\S+) \.\.\. FAILED", out, re.M))
        failed |= set(re.findall(r"^---- (\S+) stdout ----", out, re.M))
        compile_error |= "error[" in out or "error: could not compile" in out
        code |= r.returncode
    return code, sorted(failed), compile_error

originals = {f: (SRC / f).read_text() for f in (H, D, S, W, B)}
try:
    code, failed, _ = run()
    if code != 0:
        sys.exit(f"the unmodified tree does not pass: {failed}")
    print("baseline: passes", flush=True)
    caught = by_nist = 0
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
        elif compile_error and not failed:
            verdict = "caught: does not compile"; caught += 1
        else:
            nist = any(t.startswith("drbg::") for t in failed)
            own = [t.split("::")[-1] for t in failed if not t.startswith("drbg::")]
            verdict = f"caught by {len(own)} of the crate's test(s)" + (": " + ", ".join(own[:4]) if own else "")
            verdict += (" ..." if len(own) > 4 else "") + ("; and by NIST's vectors" if nist else "")
            caught += 1
            by_nist += nist
        print(f"{n:2}. {name}: {verdict}", flush=True)
    print(f"{caught} of {len(FAULTS)} caught; NIST's vectors alone catch {by_nist}", flush=True)
finally:
    for f, text in originals.items():
        (SRC / f).write_text(text)
    code, failed, _ = run()
    print("restored tree:", "passes" if code == 0 else f"FAILS {failed}", flush=True)
