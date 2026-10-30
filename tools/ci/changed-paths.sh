#!/usr/bin/env bash
# Only known documentation paths skip code checks; unknown inputs run everything.
set -euo pipefail
base=${1:?base sha required}
head=${2:?head sha required}
code=false
if [[ $base =~ ^0+$ ]] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
  code=true # New branches and unavailable history must run the full gate.
else
  paths=$(mktemp)
  trap 'rm -f "$paths"' EXIT
  git diff --name-only --no-renames -z "$base" "$head" >"$paths"
  while IFS= read -r -d '' path; do
    case "$path" in
      README.md | */README.md | AGENTS.md | docs/*.md | LICENSE) ;;
      *)
        code=true
        break
        ;;
    esac
  done <"$paths"
fi
printf 'code=%s\n' "$code"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then printf 'code=%s\n' "$code" >>"$GITHUB_OUTPUT"; fi
