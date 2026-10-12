#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

readonly UNIT="mokmokd-daemon-proof"
readonly TEMPLATE="deploy/mokmokd.service"
readonly SESSION="018f2f1e-0000-7000-8000-000000000000"
readonly KEY="018f2f1e-0000-7000-8000-0000000000a1"

# A watchdog short enough to prove in seconds. `TimeoutAbortSec` is shortened with it because a
# frozen process cannot run the abort signal's handler, so systemd escalates to SIGKILL only after
# this timeout; the default (TimeoutStopSec) would make the restart wait for no reason.
readonly WATCHDOG="5s"
readonly ABORT="5s"

fail() {
  echo "daemon.sh: $1" >&2
  exit 1
}

mokmokd="${MOKMOKD:-}"
if [ -z "$mokmokd" ]; then
  for candidate in result/bin/mokmokd target/debug/mokmokd target/release/mokmokd; do
    if [ -x "$candidate" ]; then
      mokmokd="$PWD/$candidate"
      break
    fi
  done
fi
[ -x "$mokmokd" ] || fail "no built mokmokd; build the workspace or set MOKMOKD to one"

# `Config::resolve` refuses a confiner it cannot resolve and defaults to the sibling of
# `current_exe`, so a daemon with no confiner beside the binary exits before it binds the socket.
confiner="$(dirname "$mokmokd")/mokmok-sandbox-confine"

scratch="$(mktemp -d)"
cleanup() {
  if command -v systemctl >/dev/null 2>&1; then
    systemctl --user stop "$UNIT" >/dev/null 2>&1 || true
    systemctl --user reset-failed "$UNIT" >/dev/null 2>&1 || true
  fi
  rm -rf "$scratch"
}
trap cleanup EXIT

analyzer=1
command -v systemd-analyze >/dev/null 2>&1 || analyzer=0

manager=1
if ! command -v systemctl >/dev/null 2>&1 || ! systemctl --user show-environment >/dev/null 2>&1; then
  manager=0
fi
command -v systemd-run >/dev/null 2>&1 || manager=0

skipped=""

echo "mokmokd=$mokmokd"
echo "scratch=$scratch"

substituted="$scratch/mokmokd.service"
sed "s|@mokmokd@|$mokmokd|" "$TEMPLATE" > "$substituted"

echo "checking that the substituted unit parses and its ExecStart is executable"
if [ "$analyzer" -eq 1 ]; then
  if ! verify="$(systemd-analyze verify "$substituted" 2>&1)"; then
    echo "$verify"
    fail "systemd-analyze verify rejected $TEMPLATE"
  fi
  echo "systemd-analyze verify: exit 0"
else
  skipped="$skipped verify"
  echo "skip verify: systemd-analyze is not installed, so the unit's parse is unverified here"
fi

unit_prop() {
  systemctl --user show -p "$1" --value "$UNIT" 2>/dev/null || true
}

# The three properties settle at different moments across an auto-restart, and MainPID reads 0
# while the replacement is being forked, so a single sample is not a restart.
wait_replacement() {
  local before_pid="$1" before_restarts="$2" attempts="$3"
  local pid="" restarts=""
  for _ in $(seq 1 "$attempts"); do
    pid="$(unit_prop MainPID)"
    restarts="$(unit_prop NRestarts)"
    if [ "${restarts:-0}" -gt "${before_restarts:-0}" ] && [ "${pid:-0}" -gt 0 ] &&
      [ "$pid" != "$before_pid" ] && [ "$(unit_prop ActiveState)" = "active" ]; then
      break
    fi
    sleep 0.2
  done
  printf '%s %s\n' "${pid:-0}" "${restarts:-0}"
}

if [ "$manager" -eq 0 ]; then
  skipped="$skipped lifecycle"
  echo "skip lifecycle: no reachable systemd user manager, so start, watchdog, restart, and stop are unverified here"
else
  [ -x "$confiner" ] || fail "$confiner is missing; build the workspace so the confiner sits beside mokmokd"

  shell=""
  for directory in ${PATH//:/ }; do
    for name in sh bash; do
      if [ -x "$directory/$name" ]; then
        shell="$directory/$name"
        break 2
      fi
    done
  done
  [ -n "$shell" ] || fail "no sh or bash on PATH, so the proof cannot run a tool"

  # `submit` refuses a path that is not a directory, so only the ones that exist are passed.
  allow_paths=()
  for directory in /nix/store /bin /usr /lib /lib64; do
    [ -d "$directory" ] && allow_paths+=(--allow-path "$directory")
  done

  echo "submitting one turn, so the restart has a database with history to serve"
  "$mokmokd" --data-dir "$scratch/data" --confiner "$confiner" --allow-tool "$shell" \
    "${allow_paths[@]}" submit --session "$SESSION" --idempotency-key "$KEY" \
    --program "$shell" --arg -c --arg true ||
    fail "the submit fixture failed, so there is no session for the restart to serve"
  echo "the database holds session $SESSION"

  echo "starting the daemon under a transient unit and waiting for active/running"
  systemd-run --user --no-block --collect --unit="$UNIT" \
    --property=Type=notify --property=NotifyAccess=main \
    --property=WatchdogSec="$WATCHDOG" --property=TimeoutAbortSec="$ABORT" \
    --property=TimeoutStartSec=15s \
    --property=Restart=always --property=RestartSec=1s \
    -- "$mokmokd" --data-dir "$scratch/data" serve --socket "$scratch/bus.sock"

  state=""
  for _ in $(seq 1 100); do
    state="$(unit_prop ActiveState)/$(unit_prop SubState)"
    [ "$state" = "active/running" ] && break
    sleep 0.2
  done
  [ "$state" = "active/running" ] || fail "the daemon reached $state, not active/running"
  echo "the daemon reached active/running (READY=1 was accepted)"

  echo "freezing the daemon with SIGSTOP and checking the watchdog restarts it"
  before_pid="$(unit_prop MainPID)"
  before_restarts="$(unit_prop NRestarts)"
  systemctl --user kill -s STOP "$UNIT"

  # `Result=watchdog` is the label systemd holds only while the abort signal is deferred; once the
  # replacement starts, the result resets to success, so this polls for it across that window.
  result=""
  for _ in $(seq 1 200); do
    result="$(unit_prop Result)"
    [ "$result" = "watchdog" ] && break
    sleep 0.2
  done
  [ "$result" = "watchdog" ] || fail "the watchdog never fired; Result=$result after SIGSTOP"
  echo "the watchdog fired: Result=watchdog"

  after_pid=""
  after_restarts=""
  read -r after_pid after_restarts < <(wait_replacement "$before_pid" "$before_restarts" 150)
  [ "${after_restarts:-0}" -gt "${before_restarts:-0}" ] || fail "NRestarts stayed at $before_restarts"
  if [ "${after_pid:-0}" -le 0 ] || [ "$after_pid" = "$before_pid" ]; then
    fail "no new MainPID after the watchdog restart (was $before_pid, now $after_pid)"
  fi
  echo "a new process serves again: NRestarts=$after_restarts MainPID=$after_pid"

  echo "killing the daemon with SIGKILL and checking the socket outlives it"
  before_pid="$(unit_prop MainPID)"
  before_restarts="$(unit_prop NRestarts)"
  systemctl --user kill -s SIGKILL "$UNIT"
  [ -S "$scratch/bus.sock" ] || fail "the socket did not outlive the SIGKILLed process"
  echo "the socket outlived the process"

  after_pid=""
  after_restarts=""
  read -r after_pid after_restarts < <(wait_replacement "$before_pid" "$before_restarts" 100)
  [ "${after_restarts:-0}" -gt "${before_restarts:-0}" ] || fail "NRestarts stayed at $before_restarts after SIGKILL"
  if [ "${after_pid:-0}" -le 0 ] || [ "$after_pid" = "$before_pid" ]; then
    fail "no replacement process after SIGKILL (was $before_pid, now $after_pid)"
  fi
  [ -S "$scratch/bus.sock" ] || fail "the replacement left no socket"
  echo "the replacement bound over the stale socket: NRestarts=$after_restarts MainPID=$after_pid"

  history="$("$mokmokd" --data-dir "$scratch/data" status --session "$SESSION")" ||
    fail "the replacement does not serve the same database"
  case "$history" in
    *"session=$SESSION"*) ;;
    *) fail "status did not report session $SESSION: $history" ;;
  esac
  echo "the replacement serves the same database: $(echo "$history" | head -n 1)"

  echo "stopping the daemon and checking it exits cleanly with no socket left"
  systemctl --user stop "$UNIT"
  result="$(unit_prop Result)"
  code="$(unit_prop ExecMainStatus)"
  [ "$result" = "success" ] || fail "Result=$result after stop, not success"
  [ "$code" = "0" ] || fail "ExecMainStatus=$code after stop, not 0"
  [ ! -e "$scratch/bus.sock" ] || fail "the socket was left behind after stop"
  echo "Result=success ExecMainStatus=0 with no leftover socket"
fi

echo "systemd-analyze security on the unit"
if [ "$analyzer" -eq 1 ]; then
  systemd-analyze security --offline=true "$substituted" 2>&1 | tail -n 1 || true
else
  skipped="$skipped security"
  echo "skip security: systemd-analyze is not installed, so the exposure score is unknown here"
fi

if [ -n "$skipped" ]; then
  echo "environment-dependent checks skipped:$skipped"
fi
