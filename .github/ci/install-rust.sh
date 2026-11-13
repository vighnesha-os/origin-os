#!/usr/bin/env bash
# Installs the toolchain rust-toolchain.toml pins and checks that it arrived, by version and by
# commit (CLAUDE.md §8, §9), then records the machine this job's results come from: a job cannot
# choose its processor.
set -euo pipefail

rustup toolchain install
rustc --version --verbose
cargo --version

channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
compiler=$(rustc --version)
if [[ -z $channel || $compiler != "rustc $channel "* ]]; then
  echo "::error::rustc is not the pinned ${channel:-channel}: $compiler" >&2
  exit 1
fi

# The version does not name one build: the compiler must also report the pinned commit
# (developer-tools/toolchain/rustc-commit-hash, which verify.sh reads too).
pin=developer-tools/toolchain/rustc-commit-hash
if [[ ! -r $pin ]]; then
  echo "::error::$pin is missing" >&2
  exit 1
fi
pinned=$(sed -e 's/#.*$//' "$pin" | tr -d '[:space:]')
reported=$(rustc --version --verbose | sed -n 's/^commit-hash: //p')
if [[ ! $pinned =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::$pin does not hold one commit of 40 hexadecimal digits" >&2
  exit 1
fi
if [[ $reported != "$pinned" ]]; then
  echo "::error::rustc reports commit ${reported:-none}; the pinned commit is $pinned" >&2
  exit 1
fi

installed=$(rustup target list --installed)
for target in x86_64-unknown-none x86_64-unknown-uefi x86_64-unknown-linux-gnu; do
  if ! grep -qx "$target" <<<"$installed"; then
    echo "::error::the pinned toolchain has no $target" >&2
    exit 1
  fi
done

# Linux and macOS name their processor and count their processors with different tools. The
# line for a Linux machine reads as it always has.
system=$(uname -s)
case $system in
  Linux)
    cpu=$(sed -n 's/^Model name: *//p' <<<"$(lscpu)")
    processors=$(nproc)
    ;;
  Darwin)
    cpu=$(sysctl -n machdep.cpu.brand_string)
    processors=$(sysctl -n hw.ncpu)
    ;;
  *)
    echo "::error::install-rust.sh cannot describe a $system machine" >&2
    exit 1
    ;;
esac
echo "$cpu, $processors processors, $system $(uname -r), $compiler" | tee -a "$GITHUB_STEP_SUMMARY"
