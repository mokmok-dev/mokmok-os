#!/usr/bin/env bash
# Runs the mutation gate for every workspace crate and fails on a survivor.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

if [[ "${MUTANTS_IN_SCOPE:-0}" != "1" ]] \
    && command -v systemd-run >/dev/null 2>&1 \
    && systemctl --user show-environment >/dev/null 2>&1; then
  exec systemd-run --user --scope --unit="mokmok-jobs-mutants-$$" \
    -p MemoryMax=80% -p MemoryHigh=70% --setenv=MUTANTS_IN_SCOPE=1 \
    bash -lc 'exec "$@"' bash "$0" "$@"
fi

jobs=${MUTANTS_JOBS:-4}
packages=(
  mokmok-jobs-core
  mokmok-jobs-memory
  mokmok-jobs-jsonl
  mokmok-jobs
)

for package in "${packages[@]}"; do
  echo "== ${package}"
  cargo mutants --package "${package}" --jobs "${jobs}" --timeout 60 --no-shuffle
done
