#!/usr/bin/env bash

set -uo pipefail

attest() {
	local receipt=$1
	local log command exit_code tests_run failed
	log=$(jq -r '.log // ""' "$receipt")
	command=$(jq -r '.command // ""' "$receipt")
	exit_code=$(jq -r '.exit_code // ""' "$receipt")
	tests_run=$(jq -r '.tests_run // ""' "$receipt")
	failed=$(jq -r '.failed // ""' "$receipt")

	if [[ -z $log || ! -f $log ]]; then
		printf 'verdict=fail\nreason=the receipt names no log on this machine\n'
		return 1
	fi

	local summary
	summary=$(grep -E '[0-9]+ tests run:' "$log" | tail -1)
	if [[ -z $summary ]]; then
		printf 'verdict=fail\nreason=the log holds no nextest summary line\n'
		return 1
	fi

	local observed_run observed_passed observed_failed observed_skipped
	observed_run=$(sed -n 's/.*[^0-9]\([0-9]\{1,\}\) tests run:.*/\1/p' <<<"$summary")
	observed_passed=$(sed -n 's/.*tests run: \([0-9]\{1,\}\) passed.*/\1/p' <<<"$summary")
	observed_skipped=$(sed -n 's/.*passed,.*\([0-9]\{1,\}\) skipped.*/\1/p' <<<"$summary")
	observed_failed=$(grep -o '[0-9]\{1,\} failed' <<<"$summary" | sed 's/ failed//')
	: "${observed_failed:=0}"

	printf 'command=%s\nexit_code=%s\ntests_run=%s\npassed=%s\nfailed=%s\nskipped=%s\nlog=%s\n' \
		"$command" "$exit_code" "$observed_run" "$observed_passed" "$observed_failed" "${observed_skipped:-0}" "$log"

	if grep -qE '^[^ ]* *(FAIL|TIMEOUT) \[' "$log"; then
		printf 'verdict=fail\nreason=the log holds a failing or timed-out test\n'
		return 1
	fi
	if [[ $observed_failed != 0 ]]; then
		printf 'verdict=fail\nreason=nextest reported %s failed\n' "$observed_failed"
		return 1
	fi
	if [[ $observed_run != "$tests_run" ]]; then
		printf 'verdict=fail\nreason=the receipt claims tests_run=%s and the log holds %s\n' "$tests_run" "$observed_run"
		return 1
	fi
	if [[ $observed_failed != "$failed" ]]; then
		printf 'verdict=fail\nreason=the receipt claims failed=%s\n' "$failed"
		return 1
	fi
	printf 'verdict=pass\n'
}

case ${1:-} in
	attest) attest "${2:?usage: nextest-count.sh attest <receipt.json>}" ;;
	*)
		printf 'usage: %s attest <receipt.json>\n' "$(basename -- "$0")" >&2
		exit 2
		;;
esac
