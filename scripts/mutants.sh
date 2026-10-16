#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# The gate. A mutant counts in the score only if it compiled and was tested: an unviable mutant
# proves nothing about the suite either way, and a timeout counts against the score because the
# suite failed to detect the change rather than the change being harmless. Overridable so the
# gate's own sensitivity can be demonstrated without editing it.
readonly FLOOR="${MUTANTS_TARGET:-95}"

# The scored member for the single-package mode. Overridable so one gate serves every package; the
# default is the package it was written for, so an existing invocation keeps scoring that crate.
readonly PACKAGE="${MUTANTS_PACKAGE:-mokmok-event-bus}"

# Where `--all` records every member's score and `--check-baseline` reads it back. A score that is
# not in here is a claim rather than a measurement.
readonly LEDGER="${MUTANTS_LEDGER:-mutation.json}"

MODE=single
JOBS=8
ARGUMENTS=()

# The sweep's scratch directory. The EXIT trap reads it, and a trap cannot read a `local` of the
# function that armed it, because the function has returned by the time the trap runs.
SCRATCH=""
trap 'rm -rf "$SCRATCH"' EXIT

while [ "$#" -gt 0 ]; do
  case "$1" in
    --all)
      MODE=all
      ;;
    --check-baseline)
      MODE=check
      ;;
    --jobs)
      JOBS="$2"
      ARGUMENTS+=("$1" "$2")
      shift
      ;;
    *)
      ARGUMENTS+=("$1")
      ;;
  esac

  shift
done

# Caught, missed, timeout, unviable, as a tab-separated line.
counts_of() {
  jq -r '
    [.outcomes[] | select(.scenario != "Baseline")]
    | group_by(.summary) | map({key: .[0].summary, value: length}) | from_entries
    | "\(.CaughtMutant // 0)\t\(.MissedMutant // 0)\t\(.Timeout // 0)\t\(.Unviable // 0)"
  ' "$1"
}

# Tenths of a percent, so the printed score and any comparison agree.
score_of() {
  local caught="$1" tested="$2"

  echo $((caught * 1000 / tested))
}

members() {
  cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name' | LC_ALL=C sort
}

# A score is about the tree it was measured on. Sorting the file list keeps the digest stable
# across a filesystem that returns directory entries in a different order. `find` exits non-zero
# when a path is missing, and a member's own `tests/` directory is optional here, so the pipeline
# finishes with whatever it did find rather than failing the sweep.
digest_of() {
  find "$@" -type f 2>/dev/null | LC_ALL=C sort | xargs -r sha256sum | sha256sum | cut -d' ' -f1 || true
}

# The files that decide what a score means, as opposed to which tree it was measured on.
harness_digest() {
  digest_of scripts/mutants.sh .cargo/mutants.toml Cargo.toml
}

# The kernel's and the tui's integration tests spawn a confined tool. A package-scoped build
# never produces another package's bin, so the harness's relocated tree cannot find it and the
# unmutated baseline fails before a mutant is tested. An absolute path outside the tree survives
# the harness's per-job copy.
build_confiner() {
  cargo build --package mokmok-sandbox --bin mokmok-sandbox-confine

  export MOKMOK_CONFINER="$PWD/target/debug/mokmok-sandbox-confine"
}

report_single() {
  local status=0

  cargo mutants --package "$PACKAGE" "${ARGUMENTS[@]}" || status=$?

  if [ "$status" -ne 0 ] && [ "$status" -ne 2 ]; then
    echo "cargo mutants failed with status $status" >&2

    exit "$status"
  fi

  for argument in "${ARGUMENTS[@]}"; do
    if [ "$argument" = "--list" ]; then
      # A listing writes no outcomes, so scoring here would report a previous run.
      exit "$status"
    fi
  done

  if [ ! -f mutants.out/outcomes.json ]; then
    echo "cargo mutants left no mutants.out/outcomes.json to score" >&2

    exit 1
  fi

  local caught missed timeout unviable tested tenths
  IFS=$'\t' read -r caught missed timeout unviable < <(counts_of mutants.out/outcomes.json)

  tested=$((caught + missed + timeout))

  echo "caught=$caught missed=$missed timeout=$timeout unviable=$unviable"

  if [ "$tested" -eq 0 ]; then
    echo "no mutant was tested, so the score is undefined" >&2

    exit 1
  fi

  tenths=$(score_of "$caught" "$tested")

  echo "score=$((tenths / 10)).$((tenths % 10))%"
  echo "missed mutants, if any:"
  cat mutants.out/missed.txt

  if [ $((caught * 100)) -lt $((FLOOR * tested)) ]; then
    echo "target=${FLOOR}%: not met" >&2

    exit 1
  fi

  echo "target=${FLOOR}%: met"
}

sweep_all() {
  build_confiner

  SCRATCH="$( mktemp -d )"
  local scratch="$SCRATCH"

  : > "$scratch/rows.jsonl"

  local member
  while read -r member; do
    echo "=== $member ==="

    local status=0
    set +e
    cargo mutants --package "$member" --jobs "$JOBS" --output "$scratch/$member" 2>&1 \
      | tee "$scratch/$member.log"
    status=${PIPESTATUS[0]}
    set -e

    record_row "$scratch" "$member" "$status"

    echo
  done < <(members)

  assemble_ledger "$scratch"
}

record_row() {
  local scratch="$1" member="$2" status="$3"
  local outcomes="$scratch/$member/mutants.out/outcomes.json"
  local digest

  digest=$(digest_of "$member/src" "$member/tests")

  if [ "$status" -gt 2 ] || [ ! -f "$outcomes" ]; then
    echo "$member: cargo mutants exited $status without outcomes" >&2
    printf '{"name":%s,"score":null,"target":null,"tested":0,"missed":[],"digest":%s,"note":"cargo mutants exited %s"}\n' \
      "$(jq -n --arg v "$member" '$v')" "$(jq -n --arg v "$digest" '$v')" "$status" \
      >> "$scratch/rows.jsonl"

    return
  fi

  local caught missed timeout unviable tested tenths target score
  IFS=$'\t' read -r caught missed timeout unviable < <(counts_of "$outcomes")

  tested=$((caught + missed + timeout))

  if [ "$tested" -eq 0 ]; then
    echo "$member: no mutant was tested, so the score is undefined" >&2
    printf '{"name":%s,"score":null,"target":null,"tested":0,"missed":[],"digest":%s,"note":"no mutant was tested"}\n' \
      "$(jq -n --arg v "$member" '$v')" "$(jq -n --arg v "$digest" '$v')" \
      >> "$scratch/rows.jsonl"

    return
  fi

  tenths=$(score_of "$caught" "$tested")
  score=$((tenths / 10)).$((tenths % 10))

  local previous
  previous=$(ledger_target "$member")
  target=$(awk -v floor="$FLOOR" -v prev="$previous" -v now="$score" 'BEGIN {
    t = (prev == "") ? floor : prev
    if (now + 0 > t + 0) t = now + 0
    printf "%.1f", t
  }')

  local missed_list
  missed_list=$(jq -R -s 'split("\n") | map(select(length > 0))' \
    <(cat "$scratch/$member/mutants.out/missed.txt" "$scratch/$member/mutants.out/timeout.txt" 2>/dev/null || true))

  printf '{"name":%s,"score":%s,"target":%s,"tested":%s,"caught":%s,"missed":%s,"timeout":%s,"digest":%s}\n' \
    "$(jq -n --arg v "$member" '$v')" "$score" "$target" "$tested" "$caught" "$missed_list" "$timeout" \
    "$(jq -n --arg v "$digest" '$v')" \
    >> "$scratch/rows.jsonl"

  echo "$member: caught=$caught missed=$missed timeout=$timeout unviable=$unviable score=$score% target=$target%"
}

# A member already in the ledger keeps its recorded target, so a target only ever rises.
ledger_target() {
  [ -f "$LEDGER" ] || return 0

  jq -r --arg name "$1" '.members[] | select(.name == $name) | .target // empty' "$LEDGER" 2>/dev/null || true
}

assemble_ledger() {
  local scratch="$1"

  jq -s --arg harness "$(harness_digest)" --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{harness: $harness, generated: $generated, members: .}' \
    "$scratch/rows.jsonl" > "$LEDGER"

  echo "wrote $LEDGER"

  check_baseline || true
}

check_baseline() {
  if [ ! -f "$LEDGER" ]; then
    echo "no $LEDGER to read; run --all first" >&2

    return 1
  fi

  local failures=0 below=0 rows=0
  local name score target digest note stale

  while read -r name; do
    rows=$((rows + 1))

    score=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .score // "null"' "$LEDGER")
    target=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .target // "null"' "$LEDGER")
    digest=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .digest' "$LEDGER")
    note=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .note // empty' "$LEDGER")

    if [ ! -d "$name" ]; then
      echo "$name: in the ledger, not in the workspace" >&2
      failures=$((failures + 1))

      continue
    fi

    stale=$(digest_of "$name/src" "$name/tests")

    if [ "$stale" != "$digest" ]; then
      echo "$name: the source moved since the ledger was written, so its $score% is stale" >&2
      failures=$((failures + 1))
    fi

    if [ "$score" = "null" ] || [ "$target" = "null" ]; then
      echo "$name: no score recorded (${note:-no note})" >&2
      failures=$((failures + 1))

      continue
    fi

    if awk -v s="$score" -v t="$target" 'BEGIN { exit !(s + 0 < t + 0) }'; then
      echo "$name: $score% below its recorded target of $target%" >&2
      failures=$((failures + 1))
    fi

    awk -v s="$score" 'BEGIN { exit !(s + 0 < 100) }' && below=$((below + 1))
  done < <(jq -r '.members[].name' "$LEDGER")

  echo "ledger rows=$rows below-100=$below failures=$failures"

  [ "$failures" -eq 0 ]
}

case "$MODE" in
  single) report_single ;;
  all) sweep_all ;;
  check) check_baseline ;;
esac
