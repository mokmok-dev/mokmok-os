#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# The gate. A mutant counts in the score only if it compiled and was tested: an unviable mutant
# proves nothing about the suite either way, and a timeout counts against the score because the
# suite failed to detect the change rather than the change being harmless. Overridable so the
# gate's own sensitivity can be demonstrated without editing it.
readonly FLOOR="${MUTANTS_TARGET:-95}"

readonly WORKSPACE_FLOOR="${MUTANTS_WORKSPACE_TARGET:-98}"

# The scored member for the single-package mode. Overridable so one gate serves every package; the
# default is the package it was written for, so an existing invocation keeps scoring that crate.
readonly PACKAGE="${MUTANTS_PACKAGE:-mokmok-event-bus}"

# Where `--all` records every member's score and `--check-baseline` reads it back. A score that is
# not in here is a claim rather than a measurement.
readonly LEDGER="${MUTANTS_LEDGER:-mutation.json}"

MODE=single
# Four, not the eight that fits the core count: a suite that takes seconds on an idle machine takes
# tens of seconds under eight at once, and three of the kernel's integration tests give the daemon
# five seconds to report readiness. At eight they miss it, the unmutated baseline fails, and the
# member goes unscored. Measured on a 32-core machine.
JOBS=4
# The timeout is what catches a mutant that stops the suite terminating, so it has to sit well above
# what the suite costs under load or a slow machine reads as a hang. cargo-mutants sets 20s here,
# which is its floor and not enough: with it, nine mutants across two members were scored as
# timeouts that a quieter run caught.
TIMEOUT=60
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
    --timeout)
      TIMEOUT="$2"
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

# One of cargo-mutants' per-summary name files as a JSON array, or `[]` when the file is absent.
list_of() {
  if [ ! -f "$1" ]; then
    echo '[]'

    return
  fi

  jq -R -s 'split("\n") | map(select(length > 0))' "$1"
}

# Tenths of a percent, so the printed score and any comparison agree.
score_of() {
  local caught="$1" tested="$2"

  echo $((caught * 1000 / tested))
}

METADATA=""

metadata() {
  if [ -z "$METADATA" ]; then
    METADATA=$(cargo metadata --no-deps --format-version 1)
  fi

  printf '%s' "$METADATA"
}

members() {
  metadata | jq -r '.packages[].name' | LC_ALL=C sort
}

purity_of() {
  metadata | jq -r --arg name "$1" \
    '.packages[] | select(.name == $name) | .metadata.mokmok.purity // empty'
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

  for argument in "${ARGUMENTS[@]}"; do
    if [ "$argument" = "--list" ]; then
      # A listing writes no outcomes, so scoring here would report a previous run.
      exit "$status"
    fi
  done

  # As in `--all`: exit 3 means a mutant timed out, and the outcomes are written anyway. Only a run
  # that left no outcomes at all, such as one whose unmutated baseline failed, is unscoreable.
  if [ ! -f mutants.out/outcomes.json ]; then
    echo "cargo mutants exited $status without mutants.out/outcomes.json to score" >&2

    exit "$status"
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

  local swept
  if ! swept=$(sweep_targets); then
    exit 1
  fi

  local member
  for member in $swept; do
    echo "=== $member ==="

    local status=0
    set +e
    cargo mutants --package "$member" --jobs "$JOBS" --timeout "$TIMEOUT" \
      --output "$scratch/$member" 2>&1 | tee "$scratch/$member.log"
    status=${PIPESTATUS[0]}
    set -e

    record_row "$scratch" "$member" "$status"

    echo
  done

  assemble_ledger "$scratch"
}

# Every member, or the ones named on the command line, so one crate can be re-measured without
# re-running the sweep. An option and its value are not member names, and a name that is not a
# workspace member stops the sweep rather than becoming a ledger row of its own.
sweep_targets() {
  local index=0
  local named=()

  while [ "$index" -lt "${#ARGUMENTS[@]}" ]; do
    case "${ARGUMENTS[$index]}" in
      --jobs | --timeout | --re | --file | --exclude-re | --shard)
        index=$((index + 2))
        ;;
      -*)
        index=$((index + 1))
        ;;
      *)
        named+=("${ARGUMENTS[$index]}")
        index=$((index + 1))
        ;;
    esac
  done

  if [ "${#named[@]}" -gt 0 ]; then
    local name
    for name in "${named[@]}"; do
      if ! member_known "$name"; then
        echo "$name is not a workspace member" >&2

        return 1
      fi
    done

    printf '%s\n' "${named[@]}"

    return
  fi

  members
}

member_known() {
  local wanted="$1" member

  for member in $(members); do
    if [ "$member" = "$wanted" ]; then
      return 0
    fi
  done

  return 1
}

record_row() {
  local scratch="$1" member="$2" status="$3"
  local outcomes="$scratch/$member/mutants.out/outcomes.json"
  local digest

  digest=$(digest_of "$member/src" "$member/tests")

  # A non-zero status only says some mutant survived or timed out; cargo-mutants still writes the
  # outcomes, and those mutants are exactly the ones the score has to account for. Only a run that
  # left no outcomes at all, such as one whose unmutated baseline failed, is unscoreable.
  if [ ! -f "$outcomes" ]; then
    echo "$member: cargo mutants exited $status without outcomes" >&2
    printf '{"name":%s,"score":null,"target":null,"tested":0,"missed":[],"digest":%s,"note":"cargo mutants exited %s without outcomes"}\n' \
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

  # One array per summary, so each count in the row matches its own list and
  # `tested == caught + len(missed) + timeout` holds. Folding timeouts into `missed` would make that
  # arithmetic double-count them.
  local missed_list timed_out_list
  missed_list=$(list_of "$scratch/$member/mutants.out/missed.txt")
  timed_out_list=$(list_of "$scratch/$member/mutants.out/timeout.txt")

  printf '{"name":%s,"score":%s,"target":%s,"tested":%s,"caught":%s,"missed":%s,"timeout":%s,"timed_out":%s,"digest":%s}\n' \
    "$(jq -n --arg v "$member" '$v')" "$score" "$target" "$tested" "$caught" "$missed_list" "$timeout" \
    "$timed_out_list" "$(jq -n --arg v "$digest" '$v')" \
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
  local previous='[]'

  # A narrowed sweep replaces the rows it measured and leaves the rest of the ledger alone, so
  # re-measuring one member cannot drop another member's recorded score.
  if [ -f "$LEDGER" ]; then
    previous=$(jq -c '.members // []' "$LEDGER")
  fi

  jq -s --argjson previous "$previous" --arg harness "$(harness_digest)" \
    --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    '{harness: $harness, generated: $generated,
      members: (($previous + .) | group_by(.name) | map(last) | sort_by(.name))}' \
    "$scratch/rows.jsonl" > "$LEDGER"

  echo "wrote $LEDGER"

  check_baseline || true
}

check_baseline() {
  if [ ! -f "$LEDGER" ]; then
    echo "no $LEDGER to read; run --all first" >&2

    return 1
  fi

  local failures=0 below=0 rows=0 caught_total=0 tested_total=0
  local name score target digest note stale purity missed_count timeout caught tested

  while read -r name; do
    if [ "$(jq -r --arg n "$name" '[.members[] | select(.name == $n)] | length' "$LEDGER")" -eq 0 ]; then
      echo "$name: in the workspace, not in the ledger" >&2
      failures=$((failures + 1))
    fi
  done < <(members)

  while read -r name; do
    rows=$((rows + 1))

    score=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .score // "null"' "$LEDGER")
    target=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .target // "null"' "$LEDGER")
    digest=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .digest' "$LEDGER")
    note=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .note // empty' "$LEDGER")
    missed_count=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | (.missed | length)' "$LEDGER")
    timeout=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .timeout // 0' "$LEDGER")
    caught=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .caught // 0' "$LEDGER")
    tested=$(jq -r --arg n "$name" '.members[] | select(.name == $n) | .tested // 0' "$LEDGER")

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

    # The row is a measurement this script wrote, so its own arithmetic has to hold: the counts add
    # up to what was tested, each list is as long as its count, and the score is the one those
    # counts give. A row that fails this was edited by hand.
    if ! jq -e '
      (.tested == .caught + (.missed | length) + .timeout)
      and (.timeout == (.timed_out | length))
      and ((.score * 10 | round) == ((.caught * 1000 / .tested) | floor))
    ' <<< "$(jq -c --arg n "$name" '.members[] | select(.name == $n)' "$LEDGER")" > /dev/null; then
      echo "$name: the row's own numbers disagree with each other" >&2
      failures=$((failures + 1))
    fi

    if awk -v s="$score" -v t="$target" 'BEGIN { exit !(s + 0 < t + 0) }'; then
      echo "$name: $score% below its recorded target of $target%" >&2
      failures=$((failures + 1))
    fi

    purity=$(purity_of "$name")

    case "$purity" in
      pure)
        if ! awk -v s="$score" 'BEGIN { exit (s + 0 == 100) ? 0 : 1 }' \
          || [ "$missed_count" -ne 0 ] || [ "$timeout" -ne 0 ]; then
          echo "$name: pure, so it must kill every mutant and time none out (score=$score missed=$missed_count timeout=$timeout)" >&2
          failures=$((failures + 1))
        fi
        ;;
      adapter | shell) ;;
      *)
        echo "$name: declares no purity role, so the gate cannot hold it to one" >&2
        failures=$((failures + 1))
        ;;
    esac

    caught_total=$((caught_total + caught))
    tested_total=$((tested_total + tested))

    awk -v s="$score" 'BEGIN { exit !(s + 0 < 100) }' && below=$((below + 1))
  done < <(jq -r '.members[].name' "$LEDGER")

  local tenths=""
  if [ "$tested_total" -gt 0 ]; then
    tenths=$(score_of "$caught_total" "$tested_total")

    echo "workspace: caught=$caught_total tested=$tested_total score=$((tenths / 10)).$((tenths % 10))% target=${WORKSPACE_FLOOR}%"

    if [ $((caught_total * 100)) -lt $((WORKSPACE_FLOOR * tested_total)) ]; then
      echo "workspace: below the ${WORKSPACE_FLOOR}% floor" >&2
      failures=$((failures + 1))
    fi
  else
    echo "workspace: no member tested a mutant, so the score is undefined" >&2
    failures=$((failures + 1))
  fi

  echo "ledger rows=$rows below-100=$below failures=$failures"

  [ "$failures" -eq 0 ]
}

case "$MODE" in
  single) report_single ;;
  all) sweep_all ;;
  check) check_baseline ;;
esac
