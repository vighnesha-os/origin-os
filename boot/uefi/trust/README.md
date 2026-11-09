# Test trust store: TEST KEYS ONLY

The two files `TEST-ONLY-origin-kernel.ed25519` and `TEST-ONLY-origin-kernel.mldsa87` are the
public halves (32 and 2,592 bytes, raw) of a **test** `origin-kernel` key.

**This is not a release key and must never sign anything real.** Its private half is derived
from public text, so anyone can recompute it. It exists so that the boot tests can prove the
signature path end to end (ADR-0002 B2 step 3, errata V4).

A loader built with the Cargo feature `test-trust-store`, or with `developer-mode`, which
includes it, compiles these keys in and says so on every boot:

```text
origin-boot: TEST trust store: this loader trusts a development key and must never ship
```

A loader built without those features has an empty trust store and refuses every kernel: the
safe failure, until Origin's own root and kernel keys exist. A build that holds these keys
cannot be built with a non-zero build number (`origin-version.toml`), which release
engineering sets, so a released loader cannot hold them.

No private key is stored in this repository. `cargo xtask boot-test` derives the private key
into `target/xtask/test-keys/` from the text `TEST_KEY_TRUSTED` in
`developer-tools/xtask/src/main.rs`, and checks that the public half matches these files.

To regenerate these files after changing that text, remove them and any old key file (`keygen`
never overwrites anything), and run this from the repository root:

```bash
cargo run --package onbf-pack -- keygen --insecure-test-seed "TEST-ONLY origin-kernel key 1: the test trust store holds it" --key /tmp/test-origin-kernel.key --public-ed25519 boot/uefi/trust/TEST-ONLY-origin-kernel.ed25519 --public-mldsa boot/uefi/trust/TEST-ONLY-origin-kernel.mldsa87
```
