#!/usr/bin/env bash
# How long each real-CPU boot job of this run took over its `cargo --locked xtask boot-matrix`
# step, read from the run's own job list, for processor-coverage.sh to print beside the job's
# processor (TASKS item 53). The time follows the processor the runner happened to get far more
# than any change: docs/engineering/PERFORMANCE_TRENDS_2026-11-13_ci-boot-step.md. A record
# only: nothing here fails a job, and no figure is compared with anything.
#
# Needs `GH_TOKEN` with `actions: read` and nothing more, and the run's `GITHUB_REPOSITORY`,
# `GITHUB_RUN_ID` and `GITHUB_RUN_ATTEMPT`. With `JOBS_JSON` set it reads that file instead of
# asking the API, for tests. Writes one file per job, named by its runner label:
#   seconds|conclusion
# where seconds is empty when the step has no start or no end.
set -uo pipefail

out=${1:?usage: boot-step-durations.sh <directory to write>}
mkdir -p "$out"

if [ -n "${JOBS_JSON:-}" ]; then
  if ! jobs=$(cat -- "$JOBS_JSON"); then
    echo "::warning title=Boot step durations::could not read $JOBS_JSON"
    exit 0
  fi
else
  repo=${GITHUB_REPOSITORY:-} run=${GITHUB_RUN_ID:-} attempt=${GITHUB_RUN_ATTEMPT:-1}
  if [ -z "$repo" ] || [ -z "$run" ]; then
    echo "::warning title=Boot step durations::GITHUB_REPOSITORY and GITHUB_RUN_ID name no run"
    exit 0
  fi
  if ! jobs=$(gh api "repos/$repo/actions/runs/$run/attempts/$attempt/jobs?per_page=100"); then
    echo "::warning title=Boot step durations::the run's job list could not be read; the summary goes without them"
    exit 0
  fi
fi

# One line per real-CPU job: label, then whole seconds (or nothing), then the step's
# conclusion (or its status while it has none). The label comes from the job's name, which the
# matrix in ci.yml writes; it is still checked before it names a file.
lines=$(jq -r '
  .jobs[]
  | select(.name | test("^boots on a real CPU \\(.+\\)$"))
  | (.name | capture("^boots on a real CPU \\((?<label>.+)\\)$").label) as $label
  | ([.steps[]? | select(.name == "Run cargo --locked xtask boot-matrix")] | first) as $step
  | if $step == null then "\($label)||no such step"
    else
      (if ($step.started_at and $step.completed_at)
       then (($step.completed_at | fromdateiso8601) - ($step.started_at | fromdateiso8601) | tostring)
       else "" end) as $seconds
      | "\($label)|\($seconds)|\($step.conclusion // $step.status)"
    end' <<<"$jobs") || {
  echo "::warning title=Boot step durations::the run's job list was not in the expected form"
  exit 0
}

while IFS='|' read -r label seconds conclusion; do
  [ -n "$label" ] || continue
  if [[ ! "$label" =~ ^[A-Za-z0-9._-]+$ ]] || [ "$label" = . ] || [ "$label" = .. ]; then
    echo "::warning title=Boot step durations::a job label that cannot name a file was skipped"
    continue
  fi
  [[ "$seconds" =~ ^[0-9]*$ ]] || seconds=
  conclusion=${conclusion//[^A-Za-z_ ]/}
  printf '%s|%s\n' "$seconds" "$conclusion" >"$out/$label"
  echo "$label: ${seconds:-?} s ($conclusion)"
done <<<"$lines"
exit 0
