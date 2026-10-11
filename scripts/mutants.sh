#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# The gate. A mutant counts in the score only if it compiled and was tested: an unviable mutant
# proves nothing about the suite either way, and a timeout counts against the score because the
# suite failed to detect the change rather than the change being harmless. Overridable so the
# gate's own sensitivity can be demonstrated without editing it.
readonly TARGET="${MUTANTS_TARGET:-95}"

# Extra arguments pass through, so `./scripts/mutants.sh --list` shows what would run and
# `--shard 1/4` splits a long run.
status=0
cargo mutants --package mokmok-event-bus "$@" || status=$?

if [ "$status" -ne 0 ] && [ "$status" -ne 2 ]; then
  echo "cargo mutants failed with status $status" >&2

  exit "$status"
fi

for argument in "$@"; do
  if [ "$argument" = "--list" ]; then
    # A listing writes no outcomes, so scoring here would report a previous run.
    exit "$status"
  fi
done

if [ ! -f mutants.out/outcomes.json ]; then
  echo "cargo mutants left no mutants.out/outcomes.json to score" >&2

  exit 1
fi

IFS=$'\t' read -r caught missed timeout unviable < <(jq -r '
  [.outcomes[] | select(.scenario != "Baseline")]
  | group_by(.summary) | map({key: .[0].summary, value: length}) | from_entries
  | "\(.CaughtMutant // 0)\t\(.MissedMutant // 0)\t\(.Timeout // 0)\t\(.Unviable // 0)"
' mutants.out/outcomes.json)

tested=$((caught + missed + timeout))

echo "caught=$caught missed=$missed timeout=$timeout unviable=$unviable"

if [ "$tested" -eq 0 ]; then
  echo "no mutant was tested, so the score is undefined" >&2

  exit 1
fi

# Tenths of a percent, so the printed score and the comparison agree.
tenths=$((caught * 1000 / tested))

echo "score=$((tenths / 10)).$((tenths % 10))%"
echo "missed mutants, if any:"
cat mutants.out/missed.txt

if [ $((caught * 100)) -lt $((TARGET * tested)) ]; then
  echo "target=${TARGET}%: not met" >&2

  exit 1
fi

echo "target=${TARGET}%: met"
