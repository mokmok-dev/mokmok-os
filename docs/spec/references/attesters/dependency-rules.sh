#!/usr/bin/env bash
# The dependency rules `mokmok-os-architecture.md` section 9 asks CI to check.

set -uo pipefail

# `mokmok-tui` is an application that composes the kernel, the same way the `mokmokd`
# binary does, so it belongs on this list as a composition root.
KERNEL_DEPENDENT_ALLOWLIST="mokmok-tui"

IO_CRATES="tokio
rusqlite
axum
reqwest
hyper
tokio-tungstenite
futures-util
nix
landlock"

TRANSPORT_CRATES="axum
tokio
tokio-tungstenite
futures-util
hyper"

MODEL_CRATES="reqwest
hyper
mokmok-agent-runtime"

ENFORCEMENT_CRATES="tokio
nix
landlock"

members() {
	awk '
		/^\[/ { inside = 0 }
		/^\[workspace\]/ { inside = 1; next }
		inside && /^members[[:space:]]*=/ { listing = 1; next }
		listing && /^\]/ { listing = 0; next }
		listing { gsub(/[",[:space:]]/, ""); if ($0 != "") print }
	' "$1/Cargo.toml"
}

deps() {
	awk '
		/^\[/ { section = $0; gsub(/[][[:space:]]/, "", section); next }
		/^[A-Za-z0-9_-]+[[:space:]]*=/ && (section == "dependencies" || section == "dev-dependencies") {
			entry = $0
			sub(/[[:space:]]*=.*/, "", entry)
			print section "\t" entry
		}
	' "$1/Cargo.toml"
}

selected() {
	deps "$1" | awk -F'\t' -v s="$2" '$1 == s { print $2 }'
}

forbidden_hit() {
	local dir=$1 list=$2 section=${3:-dependencies}
	selected "$dir" "$section" | grep -x -F -f <(printf '%s\n' "$list") | paste -sd, -
}

no_std_crates() {
	local repo=$1 dir
	while IFS= read -r dir; do
		grep -q '^#!\[no_std\]' "$repo/$dir/src/lib.rs" 2>/dev/null && printf '%s\n' "$dir"
	done < <(members "$repo")
}

rule_protocol-has-no-domain-dependency() {
	local repo=$1 hit
	hit=$(selected "$repo/mokmok-protocol" dependencies | grep '^mokmok-' | paste -sd, -)
	[[ -z $hit ]] || {
		printf 'mokmok-protocol depends on %s\n' "$hit"
		return 1
	}
}

rule_pure-crates-have-no-io-dependency() {
	local repo=$1 dir hit
	while IFS= read -r dir; do
		hit=$(forbidden_hit "$repo/$dir" "$IO_CRATES")
		[[ -z $hit ]] || {
			printf '%s is no_std and depends on %s\n' "$dir" "$hit"
			return 1
		}
	done < <(no_std_crates "$repo")
}

rule_event-store-has-no-transport-dependency() {
	local repo=$1 hit
	hit=$(forbidden_hit "$repo/mokmok-event-store" "$TRANSPORT_CRATES")
	[[ -z $hit ]] || {
		printf 'mokmok-event-store depends on %s\n' "$hit"
		return 1
	}
}

rule_scheduler-has-no-model-dependency() {
	local repo=$1 hit
	hit=$(forbidden_hit "$repo/mokmok-scheduler" "$MODEL_CRATES")
	[[ -z $hit ]] || {
		printf 'mokmok-scheduler depends on %s\n' "$hit"
		return 1
	}
}

rule_capability-performs-no-enforcement-io() {
	local repo=$1 hit source_hit
	hit=$(forbidden_hit "$repo/mokmok-capability" "$ENFORCEMENT_CRATES")
	[[ -z $hit ]] || {
		printf 'mokmok-capability depends on %s\n' "$hit"
		return 1
	}
	source_hit=$(grep -rlE 'std::(process|fs|net)|Command::new' "$repo/mokmok-capability/src" 2>/dev/null | paste -sd, -)
	[[ -z $source_hit ]] || {
		printf 'mokmok-capability reaches the operating system in %s\n' "$source_hit"
		return 1
	}
}

rule_kernel-dependents-are-applications() {
	local repo=$1 dir hit
	while IFS= read -r dir; do
		[[ $dir == mokmok-kernel ]] && continue
		hit=$(deps "$repo/$dir" | awk -F'\t' '$2 == "mokmok-kernel" { print "yes" }')
		[[ -n $hit ]] || continue
		case ",$KERNEL_DEPENDENT_ALLOWLIST," in
			*",$dir,"*) ;;
			*) {
				printf '%s depends on mokmok-kernel and is outside the allowlist %s\n' "$dir" "$KERNEL_DEPENDENT_ALLOWLIST"
				return 1
			} ;;
		esac
	done < <(members "$repo")
}

RULES=(
	protocol-has-no-domain-dependency
	pure-crates-have-no-io-dependency
	event-store-has-no-transport-dependency
	scheduler-has-no-model-dependency
	capability-performs-no-enforcement-io
	kernel-dependents-are-applications
)

run_rule() {
	local repo=$1 rule=$2
	case $rule in
		protocol-has-no-domain-dependency) rule_protocol-has-no-domain-dependency "$repo" ;;
		pure-crates-have-no-io-dependency) rule_pure-crates-have-no-io-dependency "$repo" ;;
		event-store-has-no-transport-dependency) rule_event-store-has-no-transport-dependency "$repo" ;;
		scheduler-has-no-model-dependency) rule_scheduler-has-no-model-dependency "$repo" ;;
		capability-performs-no-enforcement-io) rule_capability-performs-no-enforcement-io "$repo" ;;
		kernel-dependents-are-applications) rule_kernel-dependents-are-applications "$repo" ;;
		*)
			printf 'unknown rule %s\n' "$rule"
			return 2
			;;
	esac
}

check() {
	local repo=$1 only=${2:-} rule detail status failed=0
	for rule in "${RULES[@]}"; do
		[[ -n $only && $only != "$rule" ]] && continue
		if detail=$(run_rule "$repo" "$rule"); then
			status=pass
		else
			status=fail
			failed=1
		fi
		if [[ -n $detail ]]; then
			printf 'rule=%s status=%s detail=%s\n' "$rule" "$status" "$detail"
		else
			printf 'rule=%s status=%s\n' "$rule" "$status"
		fi
	done
	[[ $failed -eq 0 ]]
}

attest() {
	local receipt=$1
	local rule repo claimed command
	rule=$(jq -r '.rule // ""' "$receipt")
	repo=$(jq -r '.repo // ""' "$receipt")
	claimed=$(jq -r '.status // ""' "$receipt")
	command=$(jq -r '.command // ""' "$receipt")

	if [[ -z $rule || -z $repo || ! -d $repo ]]; then
		printf 'verdict=fail\nreason=the receipt names no rule, or no repository on this machine\n'
		return 1
	fi

	local detail status
	if detail=$(run_rule "$repo" "$rule"); then status=pass; else status=fail; fi

	printf 'rule=%s\ncommand=%s\nclaimed_status=%s\nobserved_status=%s\n' "$rule" "$command" "$claimed" "$status"

	if [[ $status == fail ]]; then
		printf 'verdict=fail\nreason=the rule fails on this tree: %s\n' "$detail"
		return 1
	fi
	if [[ $claimed != "$status" ]]; then
		printf 'verdict=fail\nreason=the receipt claims %s\n' "$claimed"
		return 1
	fi
	printf 'verdict=pass\n'
}

case ${1:-} in
	list) printf '%s\n' "${RULES[@]}" ;;
	check) check "${2:?usage: dependency-rules.sh check <repo-root> [rule]}" "${3:-}" ;;
	attest) attest "${2:?usage: dependency-rules.sh attest <receipt.json>}" ;;
	*)
		printf 'usage: %s list | check <repo-root> [rule] | attest <receipt.json>\n' "$(basename -- "$0")" >&2
		exit 2
		;;
esac
