#!/usr/bin/env bash
# Installs Kani for `cargo xtask kani` (CLAUDE.md §73; owner item 358, TASKS item 143): the
# version read on 2026-11-26, its driver from crates.io through its own lock file, and its
# release bundle from GitHub, checked against the SHA-256 GitHub publishes for that asset before
# anything in it runs (CLAUDE.md §60). The bundle brings CBMC and Kani's compiler, and setup
# installs the nightly Rust that compiler is built for (nightly-2026-08-21) through rustup. They
# verify, and build nothing that ships: every build stays on the pinned 1.100.0.
set -euo pipefail

KANI_VERSION=0.68.0

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    triple=x86_64-unknown-linux-gnu
    digest=32e2b484d73ede0bbf64a2cf0879c4259422497d8aec4ee67d448de9ae7843d3
    ;;
  Darwin-arm64)
    triple=aarch64-apple-darwin
    digest=a5d39a5d5e748253a553aa62f295c6c397287927a28e5e32691d7ff2eda0c398
    ;;
  *)
    echo "::error::no pinned Kani bundle for $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

cargo install --locked kani-verifier --version "$KANI_VERSION"

bundle_dir=$(mktemp -d)
bundle="$bundle_dir/kani-$KANI_VERSION-$triple.tar.gz"
curl -sSLf --proto '=https' --tlsv1.2 -o "$bundle" \
  "https://github.com/model-checking/kani/releases/download/kani-$KANI_VERSION/kani-$KANI_VERSION-$triple.tar.gz"
if command -v sha256sum >/dev/null; then
  echo "$digest  $bundle" | sha256sum -c -
else
  echo "$digest  $bundle" | shasum -a 256 -c -
fi
cargo kani setup --use-local-bundle "$bundle"
rm -rf "$bundle_dir"
cargo kani --version --verbose
