#!/usr/bin/env bash
# OKF v0.2 conformance check for one knowledge bundle.
# Implements SPEC.md sections 8, 9, and 11.

set -uo pipefail

frontmatter_problems() {
	awk -v file="$1" '
		NR == 1 && $0 != "---" { print file ":1: frontmatter does not open on line 1"; exit }
		NR == 1 { inside = 1; next }
		inside && $0 ~ /^---[[:space:]]*$/ { inside = 0; closed = 1; next }
		inside {
			if ($0 ~ /^[[:space:]]*$/ || $0 ~ /^[[:space:]]*#/) next
			if ($0 ~ /^[A-Za-z_][A-Za-z0-9_-]*:/) {
				key = $0
				sub(/:.*/, "", key)
				rest = $0
				sub(/^[^:]*:/, "", rest)
				gsub(/^[[:space:]]+|[[:space:]]+$/, "", rest)
				if (key == "type") {
					typeseen = 1
					if (rest == "" || rest == "[]" || rest == "~") print file ":" NR ": type is empty"
				}
				next
			}
			if ($0 ~ /^[[:space:]]/) next
			print file ":" NR ": frontmatter line is neither a key nor an indented continuation"
		}
		END {
			if (inside) print file ":1: frontmatter never closes"
			else if (!typeseen) print file ":1: no type key"
		}
	' "$1"
}

index_problems() {
	awk -v file="$1" -v root="$2" '
		NR == 1 && $0 == "---" { inside = 1; next }
		inside && $0 ~ /^---[[:space:]]*$/ { inside = 0; next }
		inside {
			if (root != "yes" || $0 !~ /^okf_version:/) print file ":" NR ": an index frontmatter holds only okf_version, and only at the bundle root"
			next
		}
		/^#/ {
			if ($0 !~ /^#+[[:space:]]+[^[:space:]]/) print file ":" NR ": a heading carries text"
			next
		}
		/^[[:space:]]*$/ { next }
		/^\* \[[^]]+\]\([^)]+\)([[:space:]]+-[[:space:]].*)?$/ { next }
		{ print file ":" NR ": an index entry reads * [Title](path) - description" }
	' "$1"
}

log_problems() {
	awk -v file="$1" '
		/^##[[:space:]]/ {
			if ($0 !~ /^##[[:space:]]+[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][[:space:]]*$/) {
				print file ":" NR ": a date heading reads ## YYYY-MM-DD"
			} else {
				day = $0
				sub(/^##[[:space:]]+/, "", day)
				gsub(/[[:space:]]/, "", day)
				if (previous != "" && day > previous) print file ":" NR ": dates run newest first"
				previous = day
			}
			next
		}
		/^#/ { next }
		/^[[:space:]]*$/ { next }
		/^\* / { next }
		{ print file ":" NR ": a log entry is a bullet" }
	' "$1"
}

path_candidates() {
	awk -v rel="$2" '
		NR == 1 && $0 != "---" { exit }
		NR == 1 { inside = 1; next }
		inside && $0 ~ /^---[[:space:]]*$/ { inside = 0; exit }
		inside {
			line = $0
			while (match(line, /(resource|computation):/)) {
				rest = substr(line, RSTART + RLENGTH)
				line = rest
				sub(/^[[:space:]]+/, "", rest)
				sub(/[,}].*$/, "", rest)
				gsub(/[[:space:]]+$/, "", rest)
				if (rest == "" || rest ~ /^https?:/) next
				if (rest ~ /[[:space:]]/) next
				print rel "\t" rest
			}
		}
	' "$1"
}

link_candidates() {
	grep -o ']([^)]*)' "$1" | sed -e 's/^](//' -e 's/)$//' | grep -v '^#' | grep -v '^https\?:' | grep -v '^mailto:'
}

resolve() {
	local from_dir=$1 target=$2
	if [[ $target == /* ]]; then
		printf '%s%s\n' "$bundle" "$target"
	else
		printf '%s/%s\n' "$from_dir" "$target"
	fi
}

check() {
	local bundle=$1
	local problems=""
	local files_checked=0
	local file rel dir

	while IFS= read -r file; do
		files_checked=$((files_checked + 1))
		dir=$(dirname -- "$file")
		rel=${file#"$bundle"/}
		case $(basename -- "$file") in
			index.md)
				if [[ $file == "$bundle/index.md" ]]; then
					problems+=$(index_problems "$file" yes)$'\n'
				else
					problems+=$(index_problems "$file" no)$'\n'
				fi
				;;
			log.md) problems+=$(log_problems "$file")$'\n' ;;
			*) problems+=$(frontmatter_problems "$file")$'\n' ;;
		esac

		local target resolved
		while IFS= read -r target; do
			[[ -n $target ]] || continue
			resolved=$(resolve "$dir" "$target")
			if [[ ! -e $resolved ]]; then
				problems+="$rel: link target $target does not exist"$'\n'
			fi
		done < <(link_candidates "$file" 2>/dev/null)

		while IFS=$'\t' read -r origin candidate; do
			[[ -n $candidate ]] || continue
			resolved=$(resolve "$dir" "$candidate")
			if [[ ! -e $resolved ]]; then
				problems+="$origin: path-valued field $candidate does not exist"$'\n'
			fi
		done < <(path_candidates "$file" "$rel" 2>/dev/null)
	done < <(find "$bundle" -name '*.md' | sort)

	problems=$(printf '%s\n' "$problems" | sed '/^[[:space:]]*$/d')
	local count=0
	if [[ -n $problems ]]; then
		printf '%s\n' "$problems"
		count=$(printf '%s\n' "$problems" | grep -c .)
	fi
	printf 'files_checked=%s problems=%s\n' "$files_checked" "$count"
	[[ $count -eq 0 ]]
}

attest() {
	local receipt=$1
	local command exit_code files_checked claimed
	command=$(jq -r '.command // ""' "$receipt")
	exit_code=$(jq -r '.exit_code // ""' "$receipt")
	files_checked=$(jq -r '.files_checked // ""' "$receipt")
	claimed=$(jq -r '.problems // ""' "$receipt")

	local bundle actual_files actual_problems
	bundle=$(jq -r '.bundle // ""' "$receipt")
	if [[ -z $bundle || ! -d $bundle ]]; then
		printf 'verdict=fail\nreason=the receipt names no bundle directory\n'
		return 1
	fi

	local out
	out=$(check "$bundle")
	actual_files=$(printf '%s\n' "$out" | sed -n 's/^files_checked=\([0-9]*\).*/\1/p')
	actual_problems=$(printf '%s\n' "$out" | sed -n 's/^files_checked=[0-9]* problems=\([0-9]*\).*/\1/p')

	printf 'command=%s\nexit_code=%s\nfiles_checked=%s\nproblems=%s\n' \
		"$command" "$exit_code" "$actual_files" "$actual_problems"

	if [[ $actual_problems != 0 ]]; then
		printf 'verdict=fail\nreason=the bundle has %s conformance problems\n' "$actual_problems"
		printf '%s\n' "$out" | grep -v '^files_checked='
		return 1
	fi
	if [[ $claimed != "$actual_problems" || $files_checked != "$actual_files" ]]; then
		printf 'verdict=fail\nreason=the receipt claims files_checked=%s problems=%s\n' "$files_checked" "$claimed"
		return 1
	fi
	printf 'verdict=pass\n'
}

case ${1:-} in
	check) check "${2:?usage: okf-conformance.sh check <bundle-dir>}" ;;
	attest) attest "${2:?usage: okf-conformance.sh attest <receipt.json>}" ;;
	*)
		printf 'usage: %s check <bundle-dir> | attest <receipt.json>\n' "$(basename -- "$0")" >&2
		exit 2
		;;
esac
