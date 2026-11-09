#!/usr/bin/env bash
# Lets this job open /dev/kvm and records what the processor offers. GitHub does not promise
# nested virtualization on these runners, so a runner without it fails here, by name: a boot
# that could not run is not a pass.
set -euo pipefail

if [ ! -e /dev/kvm ]; then
  echo "::error::this runner has no /dev/kvm, so nothing here can run on a real processor" >&2
  exit 1
fi

echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' |
  sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null
sudo udevadm control --reload-rules
sudo udevadm trigger --name-match=kvm
sudo udevadm settle
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
  echo "::error::this job cannot open /dev/kvm: $(ls -l /dev/kvm)" >&2
  exit 1
fi

for parameter in /sys/module/kvm_intel/parameters/nested /sys/module/kvm_amd/parameters/nested; do
  if [ -e "$parameter" ]; then
    echo "$parameter: $(cat "$parameter")"
  fi
done
flags=$(grep -m1 '^flags' /proc/cpuinfo)
for flag in svm vmx smep smap umip avx2 avx512f x2apic shstk ibt ibrs ibpb stibp ssbd la57 pdpe1gb fsgsbase; do
  if [[ " $flags " == *" $flag "* ]]; then
    printf '%s ' "$flag"
  fi
done
echo
