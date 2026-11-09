#!/usr/bin/env bash
# Installs the toolchain rust-toolchain.toml pins and checks that it arrived (CLAUDE.md §8, §9),
# then records the machine this job's results come from: a job cannot choose its processor.
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

installed=$(rustup target list --installed)
for target in x86_64-unknown-none x86_64-unknown-uefi x86_64-unknown-linux-gnu; do
  if ! grep -qx "$target" <<<"$installed"; then
    echo "::error::the pinned toolchain has no $target" >&2
    exit 1
  fi
done

cpu=$(sed -n 's/^Model name: *//p' <<<"$(lscpu)")
echo "$cpu, $(nproc) processors, Linux $(uname -r), $compiler" | tee -a "$GITHUB_STEP_SUMMARY"
