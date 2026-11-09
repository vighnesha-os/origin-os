#!/usr/bin/env bash
# Sums up which processors this run's real-CPU boots ran on, from the records
# processor-report.sh wrote, and says whether both vendors were covered. A record only: it
# never fails the run, because no job can choose its processor (ci.yml).
set -euo pipefail

dir=${1:?usage: processor-coverage.sh <directory of processor records>}
shopt -s nullglob
records=("$dir"/*)

summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
{
  echo "### Processors this run's real-CPU boots ran on"
  echo
  if [ ${#records[@]} -eq 0 ]; then
    echo "No record arrived: the real-CPU jobs did not get as far as naming their processors."
  else
    echo "| Vendor | Processor | Family / model / stepping | Logical processors | Runner | Features |"
    echo "|---|---|---|---|---|---|"
    for record in ${records[@]+"${records[@]}"}; do
      IFS='|' read -r vendor model family number stepping logical runner features <"$record"
      echo "| $vendor | $model | $family / $number / $stepping | $logical | $runner | $features |"
    done
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
