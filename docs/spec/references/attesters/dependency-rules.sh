#!/usr/bin/env bash
# The dependency rules `mokmok-os-architecture.md` section 9 asks CI to check.

set -uo pipefail

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

# The composition roots: the crates that compose others rather than being composed. Two rules read
# this one list, so a third root is one reviewed edit here rather than a second list to keep in step.
COMPOSITION_ROOTS="mokmok-kernel
mokmok-tui"

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

# Every rule asks Cargo rather than scanning the manifest text. A scan of `Cargo.toml` cannot see a
# dependency renamed with `package = "..."`, one declared under a `[target.'cfg(...)'.dependencies]`
# table, or the dotted `[dependencies.name]` form, so an edge written in any of those three shapes
# was invisible to it and the rule reported a pass over a crate that named the forbidden package.
load_metadata() {
	METADATA="$SCRATCH/metadata.json"
	cargo metadata --no-deps --offline --format-version 1 --manifest-path "$1/Cargo.toml" > "$METADATA" ||
		{
			printf 'cargo metadata failed for %s\n' "$1" >&2
			return 1
		}
}

members() {
	jq -r '
		. as $doc
		| .workspace_members[] as $id
		| $doc.packages[] | select(.id == $id)
		| .manifest_path | sub("/Cargo.toml$"; "") | sub("^.*/"; "")
	' "$METADATA"
}

# The role a member declares for itself, from `[package.metadata.mokmok]` in its own manifest.
declared_role() {
	jq -r --arg dir "$1" '
		.packages[]
		| select(.manifest_path | sub("/Cargo.toml$"; "") | sub("^.*/"; "") == $dir)
		| (.metadata.mokmok.purity // "")
	' "$METADATA"
}

# One member's declared dependencies, as `kind<TAB>package<TAB>rename`. An empty kind is a normal
# dependency, and `rename` is empty unless the manifest renamed the package.
edges() {
	jq -r --arg dir "$1" '
		.packages[]
		| select(.manifest_path | sub("/Cargo.toml$"; "") | sub("^.*/"; "") == $dir)
		| .dependencies[]
		| [(.kind // ""), .name, (.rename // "")] | @tsv
	' "$METADATA"
}

# What the crate links: every table except the dev and build ones, which reach a test or a build
# script rather than the artifact.
linked() {
	edges "$1" | awk -F'\t' '$1 != "dev" && $1 != "build" { print $2 }'
}

# The dev table as well. A test that composes the kernel records the same layering edge a dependency
# does, and that is the edge `kernel-dependents-are-applications` is about.
linked_and_tested() {
	edges "$1" | awk -F'\t' '$1 != "build" { print $2 }'
}

forbidden_hit() {
	local dir=$1 list=$2
	linked "$dir" | grep -x -F -f <(printf '%s\n' "$list") | paste -sd, -
}

members_declaring() {
	local repo=$1 want=$2 dir
	while IFS= read -r dir; do
		[[ $(declared_role "$dir") == "$want" ]] && printf '%s\n' "$dir"
	done < <(members "$repo")
}

is_composition_root() {
	case "
$COMPOSITION_ROOTS
" in
		*"
$1
"*) return 0 ;;
		*) return 1 ;;
	esac
}

rule_protocol-has-no-domain-dependency() {
	local repo=$1 hit
	hit=$(linked mokmok-protocol | grep '^mokmok-' | paste -sd, -)
	[[ -z $hit ]] || {
		printf 'mokmok-protocol depends on %s\n' "$hit"
		return 1
	}
}

rule_every-member-declares-its-role() {
	local repo=$1 dir role
	while IFS= read -r dir; do
		role=$(declared_role "$dir")
		case $role in
			pure | adapter) ;;
			shell)
				is_composition_root "$dir" || {
					printf '%s declares the shell role and is not a composition root\n' "$dir"
					return 1
				}
				;;
			'')
				printf '%s declares no role in its own manifest\n' "$dir"
				return 1
				;;
			*)
				printf '%s declares the unknown role %s\n' "$dir" "$role"
				return 1
				;;
		esac
	done < <(members "$repo")
}

rule_pure-crates-are-no_std() {
	local repo=$1 dir role hit
	while IFS= read -r dir; do
		role=$(declared_role "$dir")
		if grep -q '^#!\[no_std\]' "$repo/$dir/src/lib.rs" 2>/dev/null; then
			if [[ $role != pure ]]; then
				printf '%s is no_std and declares the role %s\n' "$dir" "${role:-nothing}"
				return 1
			fi
		elif [[ $role == pure ]]; then
			printf '%s declares the pure role and is not no_std\n' "$dir"
			return 1
		else
			continue
		fi
		hit=$(forbidden_hit "$dir" "$IO_CRATES")
		[[ -z $hit ]] || {
			printf '%s is no_std and depends on %s\n' "$dir" "$hit"
			return 1
		}
	done < <(members "$repo")
}

rule_pure-crates-depend-only-on-pure-crates() {
	local repo=$1 dir dep role
	while IFS= read -r dir; do
		while IFS= read -r dep; do
			[[ -n $dep ]] || continue
			role=$(declared_role "$dep")
			[[ $role == pure ]] || {
				printf '%s is pure and depends on %s, which declares %s\n' "$dir" "$dep" "${role:-nothing}"
				return 1
			}
		done < <(linked "$dir" | grep '^mokmok-')
	done < <(members_declaring "$repo" pure)
}

rule_the-role-census-is-not-empty() {
	local repo=$1 pure adapter shell
	pure=$(members_declaring "$repo" pure | grep -c .)
	adapter=$(members_declaring "$repo" adapter | grep -c .)
	shell=$(members_declaring "$repo" shell | grep -c .)
	[[ $pure -gt 0 ]] || {
		printf 'no member declares the pure role, so the purity rule selects nothing\n'
		return 1
	}
	[[ $adapter -gt 0 ]] || {
		printf 'no member declares the adapter role\n'
		return 1
	}
	printf 'pure=%s adapter=%s shell=%s\n' "$pure" "$adapter" "$shell"
}

rule_event-store-has-no-transport-dependency() {
	local repo=$1 hit
	hit=$(forbidden_hit mokmok-event-store "$TRANSPORT_CRATES")
	[[ -z $hit ]] || {
		printf 'mokmok-event-store depends on %s\n' "$hit"
		return 1
	}
}

rule_scheduler-has-no-model-dependency() {
	local repo=$1 hit
	hit=$(forbidden_hit mokmok-scheduler "$MODEL_CRATES")
	[[ -z $hit ]] || {
		printf 'mokmok-scheduler depends on %s\n' "$hit"
		return 1
	}
}

rule_capability-performs-no-enforcement-io() {
	local repo=$1 hit source_hit
	hit=$(forbidden_hit mokmok-capability "$ENFORCEMENT_CRATES")
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
		hit=$(linked_and_tested "$dir" | grep -x 'mokmok-kernel')
		[[ -n $hit ]] || continue
		is_composition_root "$dir" || {
			printf '%s depends on mokmok-kernel and is outside the composition roots\n' "$dir"
			return 1
		}
	done < <(members "$repo")
}

RULES=(
	protocol-has-no-domain-dependency
	every-member-declares-its-role
	pure-crates-are-no_std
	pure-crates-depend-only-on-pure-crates
	the-role-census-is-not-empty
	event-store-has-no-transport-dependency
	scheduler-has-no-model-dependency
	capability-performs-no-enforcement-io
	kernel-dependents-are-applications
)

run_rule() {
	local repo=$1 rule=$2
	load_metadata "$repo" || return 2
	case $rule in
		protocol-has-no-domain-dependency) rule_protocol-has-no-domain-dependency "$repo" ;;
		every-member-declares-its-role) rule_every-member-declares-its-role "$repo" ;;
		pure-crates-are-no_std) rule_pure-crates-are-no_std "$repo" ;;
		pure-crates-depend-only-on-pure-crates) rule_pure-crates-depend-only-on-pure-crates "$repo" ;;
		the-role-census-is-not-empty) rule_the-role-census-is-not-empty "$repo" ;;
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
	if [[ -n $only ]]; then
		printf '%s\n' "${RULES[@]}" | grep -qxF "$only" || {
			printf 'unknown rule %s\n' "$only" >&2
			return 2
		}
	fi
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

write_manifest() {
	local path=$1 role=$2 extra=$3
	{
		printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2024"\n\n' "$(basename -- "$(dirname -- "$path")")"
		printf '[package.metadata.mokmok]\npurity = "%s"\n' "$role"
		if [[ -n $extra ]]; then
			printf '\n%s' "$extra"
		fi
	} > "$path"
}

# `mokmok-alpha` and `mokmok-beta` stand in for members whose only job is to hold a role. The rest
# keep their real names, because three rules name those crates outright.
FAKE_MEMBERS="mokmok-alpha
mokmok-beta
mokmok-capability
mokmok-event-store
mokmok-kernel
mokmok-protocol
mokmok-scheduler
mokmok-tui"

write_root() {
	local root=$1 member
	{
		printf '[workspace]\nmembers = [\n'
		for member in $FAKE_MEMBERS; do
			printf '  "%s",\n' "$member"
		done
		printf ']\nresolver = "3"\n'
	} > "$root/Cargo.toml"
}

lay_down_base() {
	local root=$1 member
	mkdir -p "$root"
	write_root "$root"
	for member in $FAKE_MEMBERS; do
		mkdir -p "$root/$member/src"
		printf '\n' > "$root/$member/src/lib.rs"
	done
	for member in mokmok-alpha mokmok-protocol mokmok-scheduler mokmok-capability; do
		write_manifest "$root/$member/Cargo.toml" pure ""
		printf '#![no_std]\n' > "$root/$member/src/lib.rs"
	done
	write_manifest "$root/mokmok-beta/Cargo.toml" adapter ""
	write_manifest "$root/mokmok-event-store/Cargo.toml" adapter ""
	write_manifest "$root/mokmok-kernel/Cargo.toml" shell ""
	write_manifest "$root/mokmok-tui/Cargo.toml" shell ""
}

# Each fault is one manifest shape the reader must see through. The three that name a package under a
# key other than its own, or under a table other than `[dependencies]`, are the reason the reader is
# Cargo rather than a scan.
trip() {
	local root=$1 fault=$2
	case $fault in
		no-declaration) write_manifest "$root/mokmok-beta/Cargo.toml" "" "" ;;
		unknown-role) write_manifest "$root/mokmok-beta/Cargo.toml" adaptor "" ;;
		shell-outside-the-roots) write_manifest "$root/mokmok-alpha/Cargo.toml" shell "" ;;
		declared-pure-and-std) printf '\n' > "$root/mokmok-alpha/src/lib.rs" ;;
		no_std-and-declared-adapter) printf '#![no_std]\n' > "$root/mokmok-beta/src/lib.rs" ;;
		no_std-with-an-io-dependency) write_manifest "$root/mokmok-alpha/Cargo.toml" pure '[dependencies]
tokio = "1"
' ;;
		pure-depending-on-an-adapter) write_manifest "$root/mokmok-alpha/Cargo.toml" pure '[dependencies]
mokmok-beta = { path = "../mokmok-beta" }
' ;;
		no-pure-member)
			for member in mokmok-alpha mokmok-protocol mokmok-scheduler mokmok-capability; do
				write_manifest "$root/$member/Cargo.toml" adapter ""
			done
			;;
		protocol-depends-on-a-member) write_manifest "$root/mokmok-protocol/Cargo.toml" pure '[dependencies]
mokmok-alpha = { path = "../mokmok-alpha" }
' ;;
		event-store-with-a-transport) write_manifest "$root/mokmok-event-store/Cargo.toml" adapter '[dependencies]
tokio = "1"
' ;;
		scheduler-with-a-model-client) write_manifest "$root/mokmok-scheduler/Cargo.toml" pure '[dependencies]
reqwest = "0.12"
' ;;
		capability-with-an-io-dependency) write_manifest "$root/mokmok-capability/Cargo.toml" pure '[dependencies]
nix = "0.31"
' ;;
		capability-reaching-the-os) printf '#![no_std]\nfn main() { let _ = std::fs::read("x"); }\n' > "$root/mokmok-capability/src/lib.rs" ;;
		an-application-outside-the-roots) write_manifest "$root/mokmok-alpha/Cargo.toml" pure '[dependencies]
mokmok-kernel = { path = "../mokmok-kernel" }
' ;;
		a-test-outside-the-roots) write_manifest "$root/mokmok-alpha/Cargo.toml" pure '[dev-dependencies]
mokmok-kernel = { path = "../mokmok-kernel" }
' ;;
		renamed-io-dependency) write_manifest "$root/mokmok-capability/Cargo.toml" pure '[dependencies]
tokio_rt = { package = "tokio", version = "1" }
' ;;
		target-table-io-dependency) write_manifest "$root/mokmok-capability/Cargo.toml" pure "[target.'cfg(unix)'.dependencies]
tokio = \"1\"
" ;;
		dotted-table-kernel-dependency) write_manifest "$root/mokmok-alpha/Cargo.toml" pure '[dependencies.mokmok-kernel]
path = "../mokmok-kernel"
' ;;
		*)
			printf 'unknown fault %s\n' "$fault"
			return 2
			;;
	esac
}

faults() {
	local root fault rule expected detail status failures=0
	root=$(mktemp -d)
	lay_down_base "$root"
	for rule in "${RULES[@]}"; do
		if detail=$(run_rule "$root" "$rule"); then
			printf 'control=base rule=%s status=pass\n' "$rule"
		else
			printf 'control=base rule=%s status=fail detail=%s\n' "$rule" "$detail"
			failures=$((failures + 1))
		fi
	done

	while IFS='|' read -r fault rule expected; do
		[[ -n $fault ]] || continue
		lay_down_base "$root"
		trip "$root" "$fault"
		detail=$(run_rule "$root" "$rule") && status=pass || status=fail
		if [[ $status == fail && $detail == *"$expected"* ]]; then
			printf 'fault=%s rule=%s status=fail detail=%s\n' "$fault" "$rule" "$detail"
		else
			printf 'fault=%s rule=%s status=%s detail=%s expected=%s\n' \
				"$fault" "$rule" "$status" "$detail" "$expected"
			failures=$((failures + 1))
		fi
	done <<'CASES'
no-declaration|every-member-declares-its-role|mokmok-beta declares no role
unknown-role|every-member-declares-its-role|mokmok-beta declares the unknown role adaptor
shell-outside-the-roots|every-member-declares-its-role|mokmok-alpha declares the shell role
declared-pure-and-std|pure-crates-are-no_std|mokmok-alpha declares the pure role and is not no_std
no_std-and-declared-adapter|pure-crates-are-no_std|mokmok-beta is no_std and declares the role adapter
no_std-with-an-io-dependency|pure-crates-are-no_std|mokmok-alpha is no_std and depends on tokio
pure-depending-on-an-adapter|pure-crates-depend-only-on-pure-crates|mokmok-alpha is pure and depends on mokmok-beta
no-pure-member|the-role-census-is-not-empty|no member declares the pure role
protocol-depends-on-a-member|protocol-has-no-domain-dependency|mokmok-protocol depends on mokmok-alpha
event-store-with-a-transport|event-store-has-no-transport-dependency|mokmok-event-store depends on tokio
scheduler-with-a-model-client|scheduler-has-no-model-dependency|mokmok-scheduler depends on reqwest
capability-with-an-io-dependency|capability-performs-no-enforcement-io|mokmok-capability depends on nix
capability-reaching-the-os|capability-performs-no-enforcement-io|mokmok-capability reaches the operating system
an-application-outside-the-roots|kernel-dependents-are-applications|mokmok-alpha depends on mokmok-kernel and is outside
a-test-outside-the-roots|kernel-dependents-are-applications|mokmok-alpha depends on mokmok-kernel and is outside
renamed-io-dependency|capability-performs-no-enforcement-io|mokmok-capability depends on tokio
target-table-io-dependency|capability-performs-no-enforcement-io|mokmok-capability depends on tokio
dotted-table-kernel-dependency|kernel-dependents-are-applications|mokmok-alpha depends on mokmok-kernel and is outside
CASES

	rm -rf "$root"
	[[ $failures -eq 0 ]] || printf 'failures=%s\n' "$failures"
	[[ $failures -eq 0 ]]
}

case ${1:-} in
	list) printf '%s\n' "${RULES[@]}" ;;
	check) check "${2:?usage: dependency-rules.sh check <repo-root> [rule]}" "${3:-}" ;;
	attest) attest "${2:?usage: dependency-rules.sh attest <receipt.json>}" ;;
	faults) faults ;;
	*)
		printf 'usage: %s list | check <repo-root> [rule] | attest <receipt.json> | faults\n' "$(basename -- "$0")" >&2
		exit 2
		;;
esac
