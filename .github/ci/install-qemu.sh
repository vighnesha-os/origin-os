#!/usr/bin/env bash
# Builds the QEMU the boot proofs are pinned to, from its release tarball, checked against the
# digest below (CLAUDE.md §60). The firmware (OVMF) comes with it. Ubuntu's own QEMU is a
# different version, and the proofs describe this one.
set -euo pipefail

version=11.1.3
sha256=5a8289c7940dc77d3873c81a6c29ba3afe1446fc9d36e77fa1658cdbecb84b22

sudo apt-get update
sudo apt-get install -y build-essential flex bison ninja-build pkg-config python3-venv \
  libglib2.0-dev libpixman-1-dev libslirp-dev

cd "$RUNNER_TEMP"
curl -fsSL --retry 3 -o qemu.tar.xz "https://download.qemu.org/qemu-$version.tar.xz"
echo "$sha256  qemu.tar.xz" | sha256sum -c -
tar xf qemu.tar.xz
cd "qemu-$version"
./configure --prefix="$HOME/qemu" --target-list=x86_64-softmmu --enable-kvm --enable-slirp \
  --disable-docs --disable-gtk --disable-sdl --disable-vnc --disable-tools
make -j"$(nproc)"
make install

echo "$HOME/qemu/bin" >> "$GITHUB_PATH"
banner=$("$HOME/qemu/bin/qemu-system-x86_64" --version)
echo "${banner%%$'\n'*}" | tee -a "$GITHUB_STEP_SUMMARY"
