#!/bin/sh
# Drives one coding task through `mokmokd agent run` against a real provider, and asserts the file
# the task asked for rather than a log line.
#
# The credential is the value of `OLLAMA_API_KEY`, which the daemon reads from its own environment
# because `--env-key OLLAMA_API_KEY` names that variable: a file the daemon read at open was the
# other way, and it cost a 0600 mode and a place outside every write grant to keep the value out of
# an environment any same-user process can read. What a confined tool sees is still only `PATH` and
# the operator's `--tool-env` entries, because `agent run` passes `Environment::Exact`, and
# `mokmok-kernel/tests/environment.rs` pins that the daemon's own variables do not cross.
#
# The gate cannot run this, because the gate has no network. `mokmok-kernel/tests/agent.rs` drives
# the same loop with a scripted provider, including the crash between an intent and its outcome and
# the resume that must not run the command twice.
#
# From the workspace root:
#   OLLAMA_API_KEY=... sh examples/coding-agent/run.sh
#
# Against a server on this host, where no code leaves the machine:
#   MOKMOK_ENDPOINT=http://127.0.0.1:11434/v1 sh examples/coding-agent/run.sh

set -eu

readonly MODEL="${MOKMOK_MODEL:-kimi-k2.7-code}"
readonly ENDPOINT="${MOKMOK_ENDPOINT:-https://ollama.com/v1}"
readonly GOAL='Write fib.sh in the checkout: a POSIX shell script that computes the values 1, 1, 2,
3, 5, 8, 13, 21, 34, 55 with a loop and prints each on its own line. Then run it with its output
redirected to out.txt in the checkout. When out.txt holds those ten lines, answer without calling
the tool.'

readonly EXPECTED='1
1
2
3
5
8
13
21
34
55'

fail() {
  echo "run.sh: $1" >&2
  exit 1
}

cd "$(dirname "$0")/../.."

daemon="${MOKMOKD:-}"
if [ -z "$daemon" ]; then
  for candidate in result/bin/mokmokd target/debug/mokmokd target/release/mokmokd; do
    if [ -x "$candidate" ]; then
      daemon="$PWD/$candidate"
      break
    fi
  done
fi
[ -x "$daemon" ] || fail "no built mokmokd; build the workspace or set MOKMOKD to one"

scratch="$(mktemp -d)"
checkout="$scratch/checkout"
data="$scratch/data"
mkdir -p "$checkout"

# The session's own files are the evidence for this run, so they outlive it. `MOKMOK_CLEAN=1`
# removes them, which is what a scripted harness wants and what an operator does after reading them.
cleanup() {
  if [ -n "${MOKMOK_CLEAN:-}" ]; then
    rm -rf "$scratch"
  else
    echo "run.sh: the session's data directory is $data" >&2
  fi
}
trap cleanup EXIT

export OLLAMA_API_KEY="${OLLAMA_API_KEY:-ollama}"

# The grants a coding tool needs: the shell it runs, the store every nix binary lives in, and the
# one directory it may change.
set -- \
  --data-dir "$data" \
  --allow-tool /bin/sh \
  --allow-path /nix/store \
  --allow-write "$checkout"
for directory in /bin /usr /lib /lib64; do
  if [ -d "$directory" ]; then
    set -- "$@" --allow-path "$directory"
  fi
done

session="$(cat /proc/sys/kernel/random/uuid)"

echo "run.sh: session=$session model=$MODEL endpoint=$ENDPOINT" >&2
"$daemon" "$@" agent run \
  --session "$session" \
  --goal "$GOAL" \
  --checkout "$checkout" \
  --max-turns 8 \
  --steps 32 \
  --token-budget 200000 \
  --max-answer-tokens 2048 \
  --tool-deadline-seconds 120 \
  --provider ollama \
  --model "$MODEL" \
  --endpoint "$ENDPOINT" \
  --env-key OLLAMA_API_KEY || fail "the run failed"

status="$("$daemon" "$@" status --session "$session")"
printf '%s\n' "$status"

printf '%s\n' "$status" | grep -q 'phase=Closed(Completed)' \
  || fail "the session did not close completed; the transcript is above"

# The transcript has to show the model being asked more than once, each ask with a durable prompt
# and a durable answer: one turn that ran a command and one that finished. A run that closed after
# a single ask reached the goal by luck or by a shortcut, and the run is not evidence of a loop.
prompts="$(printf '%s\n' "$status" | grep -c '^prompt ' || true)"
answers="$(printf '%s\n' "$status" | grep -c '^answer ' || true)"
results="$(printf '%s\n' "$status" | grep -c '^result ' || true)"
if [ "$prompts" -lt 2 ] || [ "$answers" -lt 2 ] || [ "$results" -lt 1 ]; then
  fail "the transcript holds prompts=$prompts answers=$answers results=$results; a loop needs two asks, one of them a command"
fi

[ -f "$checkout/out.txt" ] || fail "the task's file is not there; the transcript is above"
actual="$(cat "$checkout/out.txt")"
if [ "$actual" != "$EXPECTED" ]; then
  echo "--- out.txt ---" >&2
  printf '%s\n' "$actual" >&2
  fail "out.txt is not the ten values the goal named"
fi

echo "run.sh: out.txt holds the ten values, and the transcript above is the session"
