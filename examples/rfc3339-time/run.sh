#!/bin/sh
# Runs examples/rfc3339-time/workload.sh as a sandboxed workload and prints the timestamp the
# workload captured. `mokmokd submit` prints only the SHA-256 of that output, so the runner maps the
# hash back to the artifact blob and writes the blob's bytes to stdout. Run it from the workspace
# root as `sh examples/rfc3339-time/run.sh`; the last line of its output is the timestamp.
#
# `submit` opens no bus connection. It writes to <data-dir>/events.sqlite and runs the workload in
# this process, so a serving daemon does no more than relay the committed notice to
# `session:<session>`.
#
# To write into a deployed service's data directory, run it on the service host as that service's
# user:
#   sudo -u mokmokd MOKMOKD_DATA_DIR=/var/lib/mokmokd MOKMOKD=<store path>/bin/mokmokd \
#     sh /tmp/rfc3339-time/run.sh
# The runner passes --allow-path on the example directory, so the service user has to read it, and
# /home/<user> is 0700. Copy the example to /tmp: the shipped unit already passes --allow-write
# /tmp, which grants read and execute as well.

set -eu

example_dir="$(cd "$(dirname "$0")" && pwd)"

daemon=""
if [ -n "${MOKMOKD:-}" ]; then
  daemon="$MOKMOKD"
elif command -v mokmokd >/dev/null 2>&1; then
  daemon="$(command -v mokmokd)"
else
  echo "run.sh: set MOKMOKD to a mokmokd binary, or build one with 'nix build .#default'" >&2
  exit 1
fi

data_dir="${MOKMOKD_DATA_DIR:-}"
if [ -z "$data_dir" ]; then
  data_dir="$(mktemp -d)"
fi
mkdir -p "$data_dir"

# The workload inherits the daemon's environment, so its bare `date` resolves through the runner's
# PATH; put the host coreutils directory there rather than trusting whatever PATH the caller had.
PATH="$(dirname "$(command -v date)"):$PATH"
export PATH

session="$(cat /proc/sys/kernel/random/uuid)"
key="$(cat /proc/sys/kernel/random/uuid)"

output="$(
  "$daemon" \
    --data-dir "$data_dir" \
    --allow-tool /bin/sh \
    --allow-path /nix/store \
    --allow-path "$example_dir" \
    submit \
    --session "$session" \
    --idempotency-key "$key" \
    --program /bin/sh \
    --arg "$example_dir/workload.sh"
)"

field() {
  sed -n "s/^$1=\([^ ]*\).*/\1/p"
}

hash="$(printf '%s\n' "$output" | field stdout)"

# A workload that never finished, which includes one the sandbox refused, has no captured output:
# `submit` prints the `stdout=` line only when there is a result to report.
if [ -z "$hash" ]; then
  printf '%s\n' "$output" >&2
  echo "run.sh: the workload produced no captured output" >&2
  exit 1
fi

# The store nests each blob under the first two characters of its hash.
blob_path() {
  printf '%s/artifacts/%s/%s' "$data_dir" "$(printf '%s' "$1" | cut -c1-2)" "$1"
}

# A workload that ran and printed nothing has an empty blob whose hash is as valid as any other, and
# `submit` reports the byte count of the stdout and stderr lines alike, so the blob is what tells a
# run that produced nothing apart from one that produced something.
blob="$(blob_path "$hash")"
if [ ! -s "$blob" ]; then
  echo "run.sh: the workload captured nothing into $blob" >&2
  exit 1
fi

printf 'session=%s\n' "$session" >&2
printf 'execution=%s\n' "$(printf '%s\n' "$output" | field execution)" >&2
cat "$blob"
