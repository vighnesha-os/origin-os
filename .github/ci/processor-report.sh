#!/usr/bin/env bash
# Names the processor this job runs on, so every pull request records which vendors and models
# its boots were tested on. GitHub does not let a job choose its processor (ci.yml), so this is
# a record, not a requirement: nothing here fails a job.
#
# Writes one line to the job summary, a notice the pull request shows, and, given a path as the
# first argument, a one-line record for the processors job to collect:
#   vendor|model|family|model number|stepping|logical processors|runner|features
set -euo pipefail

cpuinfo=${CPUINFO:-/proc/cpuinfo}
field() { awk -F': *' -v key="$1" '$1 ~ "^"key"[[:space:]]*$" { print $2; exit }' "$cpuinfo"; }

vendor_id=$(field vendor_id)
case "$vendor_id" in
  GenuineIntel) vendor=Intel ;;
  AuthenticAMD) vendor=AMD ;;
  *) vendor="other ($vendor_id)" ;;
esac
model=$(field 'model name')
family=$(field 'cpu family')
number=$(field model)
stepping=$(field stepping)
logical=$(grep -c '^processor' "$cpuinfo" || true)
runner="${RUNNER_LABEL:-${ImageOS:-unknown}}${ImageVersion:+ (image $ImageVersion)}"

# What the kernel uses or will use, where the processor offers it: the x86-64-v3 baseline's
# AVX2, AVX-512 and AMX, CET's shadow stacks and indirect-branch tracking, 5-level paging,
# x2APIC, 1 GiB pages, and the supervisor protections.
flags=" $(field flags) "
features=()
for flag in avx2 avx512f amx_tile shstk ibt la57 x2apic pdpe1gb smep smap umip; do
  if [[ "$flags" == *" $flag "* ]]; then
    features+=("$flag")
  fi
done
feature_list=${features[*]:-none}

line="**$vendor** $model (family $family, model $number, stepping $stepping); logical processors: $logical; runner: $runner; features: $feature_list"
echo "$line"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### The processor this job ran on"
    echo
    echo "$line"
    echo
    echo "A record only: GitHub does not let a job choose its processor, and none of its processors is the declared 2026+ hardware (CLAUDE.md §74)."
  } >>"$GITHUB_STEP_SUMMARY"
fi
echo "::notice title=Processor (${RUNNER_LABEL:-this job})::$vendor: $model, family $family model $number stepping $stepping"

if [ $# -ge 1 ]; then
  mkdir -p "$(dirname "$1")"
  printf '%s|%s|%s|%s|%s|%s|%s|%s\n' "$vendor" "$model" "$family" "$number" "$stepping" \
    "$logical" "$runner" "$feature_list" >"$1"
fi
