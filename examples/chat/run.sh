#!/bin/sh
# Holds a conversation with the agent in a terminal: type a line, read the agent's work and its
# answer, type another. Run it from the workspace root as `sh examples/chat/run.sh`; it builds
# nothing, so build first with `cargo build --workspace`.
#
# The provider is the scripted one by default, so this run reaches no network and needs no
# credential: the answers come from the JSON below, handed out in the order the loop asks for them.
#
# MOKMOK_PROVIDER=ollama swaps the script for a real OpenAI-compatible endpoint, the route
# examples/coding-agent/run.sh takes, where the model answers for itself:
#   OLLAMA_API_KEY=... MOKMOK_PROVIDER=ollama sh examples/chat/run.sh
# It reads `OLLAMA_API_KEY`, writes it to a 0600 file outside every write grant, and passes
# `--provider ollama --model "$MOKMOK_MODEL" --endpoint "$MOKMOK_ENDPOINT"
# --provider-credential-file <that file>`. MOKMOK_MODEL defaults to kimi-k2.7-code and MOKMOK_ENDPOINT
# to https://ollama.com/v1; point MOKMOK_ENDPOINT at http://127.0.0.1:11434/v1 for a server on this
# host.
#
# `mokmok-tui` drives the agent loop in its own process, exactly as `mokmokd agent run` does, and
# opens the event log directly. It connects to no bus, because the bus has no inbound command:
# a `publish` to a session topic sits in the bus's own store and nothing reads it. So the data
# directory is the conversation's owner. Against a deployed service the log belongs to `mokmokd`
# and its mode is 0700, so run this as that user:
#   sudo -u mokmokd MOKMOK_TUI=<store path>/bin/mokmok-tui sh /tmp/chat/run.sh
# Copy the example to /tmp first: /home/<user> is 0700, and the script's grants have to reach it.
#
# Leave with Ctrl-C. The session is left open and resumable, and the whole conversation is printed
# as plain text once the terminal is handed back. The command the script prints on exit reads it
# back later with `mokmokd status`.

set -eu

example_dir="$(cd "$(dirname "$0")" && pwd)"

root="$(cd "$example_dir/../.." && pwd)"

tui=""
if [ -n "${MOKMOK_TUI:-}" ]; then
  tui="$MOKMOK_TUI"
elif command -v mokmok-tui >/dev/null 2>&1; then
  tui="$(command -v mokmok-tui)"
elif [ -x "$root/target/debug/mokmok-tui" ]; then
  tui="$root/target/debug/mokmok-tui"
else
  echo "run.sh: set MOKMOK_TUI to a mokmok-tui binary, or run 'cargo build --workspace'" >&2
  exit 1
fi

data_dir="${MOKMOK_DATA_DIR:-$(mktemp -d)}"
checkout="${MOKMOK_CHECKOUT:-$(mktemp -d)}"
session="$(cat /proc/sys/kernel/random/uuid)"
provider="${MOKMOK_PROVIDER:-scripted}"

# The answers, in the order the loop asks for them. One conversation of two exchanges: the first
# line runs a command and then answers, the second line runs another command and answers again. A
# `command` entry is one model call, so the turn that carries the command and the turn that answers
# afterwards are two entries. Every entry may carry `say`, which is what the human reads on screen.
cat > "$data_dir/script.json" <<JSON
[
  {"action": "command", "command": "cd $checkout && printf '1\\n1\\n2\\n3\\n5\\n' > fib.txt && cat fib.txt", "say": "Writing fib.txt first."},
  {"action": "final", "say": "fib.txt holds 1, 1, 2, 3, 5. Anything else?"},
  {"action": "command", "command": "cd $checkout && { printf 'fib:\\n'; cat fib.txt; } > fib.new && mv fib.new fib.txt && cat fib.txt", "say": "Adding the header."},
  {"action": "final", "say": "Done. fib.txt now starts with a header line and then the five values."}
]
JSON

printf 'data dir:  %s\n' "$data_dir" >&2
printf 'checkout:  %s\n' "$checkout" >&2
printf 'session:   %s\n' "$session" >&2
printf 'provider:  %s\n' "$provider" >&2
printf 'read it back with: mokmokd --data-dir %s status --session %s\n' "$data_dir" "$session" >&2
printf 'try these lines:\n' >&2
printf '  write fib.txt in the checkout with the first five Fibonacci numbers\n' >&2
printf '  add a header line that says fib:\n' >&2
if [ "$provider" != ollama ]; then
  printf 'for a real model instead of the script: OLLAMA_API_KEY=... MOKMOK_PROVIDER=ollama sh examples/chat/run.sh\n' >&2
fi

set -- \
  --data-dir "$data_dir" \
  --allow-tool /bin/sh \
  --allow-path /nix/store \
  --allow-write "$checkout" \
  --session "$session" \
  --goal "Hold a conversation with me. The checkout is yours to read and write." \
  --checkout "$checkout" \
  --max-turns 16

case "$provider" in
  scripted)
    set -- "$@" --script "$data_dir/script.json"
    ;;
  ollama)
    # The credential is read from a file at open and never from the environment: every confined tool
    # inherits that environment unless the caller says otherwise. The file sits outside every
    # --allow-write grant, and the kernel refuses one that does not.
    credential_dir="$(mktemp -d)"
    umask 077
    printf '%s' "${OLLAMA_API_KEY:-ollama}" > "$credential_dir/credential"
    set -- "$@" \
      --provider ollama \
      --model "${MOKMOK_MODEL:-kimi-k2.7-code}" \
      --endpoint "${MOKMOK_ENDPOINT:-https://ollama.com/v1}" \
      --provider-credential-file "$credential_dir/credential"
    ;;
  *)
    printf 'run.sh: MOKMOK_PROVIDER must be scripted or ollama, not %s\n' "$provider" >&2
    exit 1
    ;;
esac

exec "$tui" "$@"
