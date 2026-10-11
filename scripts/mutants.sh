#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

status=0
cargo mutants --package mokmok-event-bus "$@" || status=$?

# The score counts only mutants that compiled and were tested, because an unviable mutant proves
# nothing about the suite either way. A timeout counts as not killed, because the suite did not
# detect the change; it only ran out of time. Run with --list to see what would be tested.
jq -r '
  [.outcomes[] | select(.scenario != "Baseline")] as $mutants
  | ($mutants | group_by(.summary) | map({key: .[0].summary, value: length}) | from_entries) as $counts
  | ($counts.CaughtMutant // 0) as $caught
  | ($counts.MissedMutant // 0) as $missed
  | ($counts.Timeout // 0) as $timeout
  | ($counts.Unviable // 0) as $unviable
  | ($caught + $missed + $timeout) as $tested
  | "caught=\($caught) missed=\($missed) timeout=\($timeout) unviable=\($unviable)",
    "score=\((if $tested == 0 then 0 else $caught * 1000 / $tested | floor end) / 10)%",
    "target=95% \(if $tested > 0 and $caught * 100 >= 95 * $tested then "met" else "not met" end)"
' mutants.out/outcomes.json

echo "missed mutants, if any:"
cat mutants.out/missed.txt

exit "$status"
