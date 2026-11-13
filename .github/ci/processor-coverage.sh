#!/usr/bin/env bash
# Sums up which processors this run's real-CPU boots ran on, from the records
# processor-report.sh wrote, and says whether both vendors were covered. Given a second
# directory, the records boot-step-durations.sh wrote, it prints beside each processor how long
# that job's boot-matrix step took (TASKS item 53). A record only: it never fails the run,
# because no job can choose its processor (ci.yml), and no duration is held to anything.
set -euo pipefail

dir=${1:?usage: processor-coverage.sh <directory of processor records> [<directory of step durations>]}
durations=${2:-}
shopt -s nullglob
records=("$dir"/*)

# The boot-matrix step's duration for the job whose processor record is $1, as text for the
# table: "96 min 27 s", the step's conclusion when it did not succeed, or "not recorded".
step_time() {
  local file="$durations/$(basename -- "$1")" seconds conclusion text
  if [ -z "$durations" ] || [ ! -f "$file" ]; then
    echo "not recorded"
    return
  fi
  IFS='|' read -r seconds conclusion <"$file" || true
  if [[ "$seconds" =~ ^[0-9]+$ ]]; then
    text="$((seconds / 60)) min $((seconds % 60)) s"
  else
    text="no end"
  fi
  if [ "$conclusion" != success ]; then
    text="$text (${conclusion:-no conclusion})"
  fi
  echo "$text"
}

summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
{
  echo "### Processors this run's real-CPU boots ran on"
  echo
  if [ ${#records[@]} -eq 0 ]; then
    echo "No record arrived: the real-CPU jobs did not get as far as naming their processors."
  else
    echo "| Vendor | Processor | Family / model / stepping | Logical processors | Runner | Features | Boot-matrix step |"
    echo "|---|---|---|---|---|---|---|"
    for record in ${records[@]+"${records[@]}"}; do
      IFS='|' read -r vendor model family number stepping logical runner features <"$record"
      echo "| $vendor | $model | $family / $number / $stepping | $logical | $runner | $features | $(step_time "$record") |"
    done
    echo
    echo "The boot-matrix step's time is a trend that claims nothing (CLAUDE.md §53): a KVM guest on a shared GitHub runner, and the same boots take from about 48 to 96 minutes depending on the processor the runner got (\`docs/engineering/PERFORMANCE_TRENDS_2026-11-13_ci-boot-step.md\`). Compare a slow run with runs on the same processor before reading it as a regression. Nothing is held to a threshold."
  fi
} >>"$summary"

intel=no
amd=no
for record in ${records[@]+"${records[@]}"}; do
  case "$(cut -d'|' -f1 "$record")" in
    Intel) intel=yes ;;
    AMD) amd=yes ;;
  esac
done
coverage="Intel: $intel. AMD: $amd."
if [ "$intel" = yes ] && [ "$amd" = yes ]; then
  verdict="Both vendors were covered."
elif [ "$intel" = yes ] || [ "$amd" = yes ]; then
  verdict="Only one vendor was covered this time; the weekly processor sweep looks for the rest."
else
  verdict="No real-CPU boot named its processor."
fi
caveat=""
if [ ${#records[@]} -gt 0 ]; then
  caveat=" None of these is the declared 2026+ hardware (CLAUDE.md §74)."
fi
{
  echo
  echo "$coverage $verdict$caveat"
} >>"$summary"
echo "::notice title=Processor coverage::$coverage $verdict"
