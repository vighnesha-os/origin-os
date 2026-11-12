#!/usr/bin/env bash
# Fails if the job changed the checkout: a tracked file that differs from the commit (Cargo.lock
# among them), or a file that is neither tracked nor ignored. The builds write under target/,
# which is ignored.
set -euo pipefail

status=$(git status --porcelain)
if [[ -n $status ]]; then
  echo "::error::the job changed the checkout" >&2
  echo "$status" >&2
  git diff >&2
  exit 1
fi
echo "The checkout is as committed: no tracked file changed, and nothing new outside what .gitignore names."
