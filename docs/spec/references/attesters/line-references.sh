#!/usr/bin/env bash

set -uo pipefail

near() {
	local path=$1 line=$2 name=$3
	local from=$((line > 3 ? line - 3 : 1))
	local to=$((line + 3))
	sed -n "${from},${to}p" "$path" | grep -qF -- "$name"
}

resolve() {
	local repo=$1 doc=$2 file=$3
	if [[ -e $repo/$file ]]; then
		printf '%s\n' "$repo/$file"
		return
	fi
	local base
	base=$(sed -n 's/^resource:[[:space:]]*//p' "$doc" | head -1)
	if [[ -n $base ]]; then
		local dir
		dir=$(cd -- "$(dirname -- "$doc")" && cd -- "$base" 2>/dev/null && pwd)
		if [[ -n $dir && -f $dir/$file ]]; then
			printf '%s\n' "$dir/$file"
			return
		fi
	fi
	printf '\n'
}

check() {
	local repo=$1
	shift
	local problems=0 pairs=0 doc lineno line
	for doc in "$@"; do
		[[ -f $doc ]] || continue
		lineno=0
		while IFS= read -r line; do
			lineno=$((lineno + 1))
			[[ $line == *".rs:"* || $line == *".toml:"* || $line == *".nix:"* || $line == *".sh:"* ]] || continue

			local files=() numbers=() citation file rest
			while IFS= read -r citation; do
				[[ -n $citation ]] || continue
				file=${citation%%:*}
				rest=${citation#*:}
				local n
				for n in ${rest//,/ }; do
					files+=("$file")
					numbers+=("$n")
				done
			done < <(grep -oE '[A-Za-z0-9_./-]+\.(rs|toml|nix|sh):[0-9]+(,[0-9]+)*(-[0-9]+)?' <<<"$line")
			[[ ${#numbers[@]} -gt 0 ]] || continue

			local scope=$line
			if [[ $line =~ ^\|[[:space:]] ]]; then
				scope=${line#*|}
				scope=${scope%%|*}
			else
				scope=${line%%"\`${files[0]}:"*}
			fi
			local names=() token
			while IFS= read -r token; do
				[[ -n $token ]] || continue
				names+=("${token##*::}")
			done < <(grep -oE '`[A-Za-z_][A-Za-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*)*`' <<<"$scope" | tr -d '`')

			local index=0 name path from to
			for index in "${!numbers[@]}"; do
				file=${files[$index]}
				from=${numbers[$index]%%-*}
				to=${numbers[$index]##*-}
				path=$(resolve "$repo" "$doc" "$file")
				if [[ -z $path || ! -f $path ]]; then
					printf '%s:%s: %s does not resolve\n' "$doc" "$lineno" "$file"
					problems=$((problems + 1))
					continue
				fi
				pairs=$((pairs + 1))
				if [[ ${#names[@]} -eq ${#numbers[@]} ]]; then
					name=${names[$index]}
					if [[ -n $name ]] && ! near "$path" "$from" "$name" && ! near "$path" "$to" "$name"; then
						printf '%s:%s: %s is not near %s:%s\n' "$doc" "$lineno" "$name" "$file" "${numbers[$index]}"
						problems=$((problems + 1))
					fi
				else
					local found=no candidate
					for candidate in "${names[@]}"; do
						{ near "$path" "$from" "$candidate" || near "$path" "$to" "$candidate"; } && found=yes
					done
					if [[ ${#names[@]} -gt 0 && $found == no ]]; then
						printf '%s:%s: none of %s is near %s:%s\n' "$doc" "$lineno" "${names[*]}" "$file" "${numbers[$index]}"
						problems=$((problems + 1))
					fi
				fi
			done
		done < "$doc"
	done
	printf 'pairs_checked=%s citations_unresolved=%s\n' "$pairs" "$problems"
	[[ $problems -eq 0 ]]
}

attest() {
	local receipt=$1
	local repo claimed command
	repo=$(jq -r '.repo // ""' "$receipt")
	claimed=$(jq -r '.unresolved // ""' "$receipt")
	command=$(jq -r '.command // ""' "$receipt")

	if [[ -z $repo || ! -d $repo ]]; then
		printf 'verdict=fail\nreason=the receipt names no repository on this machine\n'
		return 1
	fi

	local docs=() doc
	while IFS= read -r doc; do docs+=("$doc"); done < <(find "$repo/docs/spec" -name '*.md' | sort)

	local out observed
	out=$(check "$repo" "${docs[@]}")
	observed=$(sed -n 's/^pairs_checked=[0-9]* citations_unresolved=\([0-9]*\)$/\1/p' <<<"$out")
	printf 'command=%s\ncitations_unresolved=%s\n' "$command" "$observed"
	printf '%s\n' "$out" | grep -v '^pairs_checked='
	if [[ $observed != 0 ]]; then
		printf 'verdict=fail\nreason=%s citations do not point at their symbol\n' "$observed"
		return 1
	fi
	if [[ $claimed != "$observed" ]]; then
		printf 'verdict=fail\nreason=the receipt claims unresolved=%s\n' "$claimed"
		return 1
	fi
	printf 'verdict=pass\n'
}

case ${1:-} in
	check) check "${2:?usage: line-references.sh check <repo-root> <doc>...}" "${@:3}" ;;
	attest) attest "${2:?usage: line-references.sh attest <receipt.json>}" ;;
	*)
		printf 'usage: %s check <repo-root> <doc>... | attest <receipt.json>\n' "$(basename -- "$0")" >&2
		exit 2
		;;
esac
