#!/usr/bin/env bash
# Merge every module's install tree (out/build/<module>/install) into
# out/stage, the installable prefix, and write its manifest,
# share/linuxapk/manifest.tsv: one "path<TAB>module<TAB>sha256" line per file
# (a symlink's sha256 is of its target). Fails if a module declared in
# tools/build/artifacts.toml lacks an artifact, or if two modules write one
# path that [overrides] doesn't settle.
set -euo pipefail
cd "$(dirname "$0")/../.."
stage=out/stage
manifest=share/linuxapk/manifest.tsv

# "module<TAB>name<TAB>task", "artifact<TAB>module<TAB>path" and
# "override<TAB>path<TAB>module" lines.
spec=$(
  python3 -I - tools/build/artifacts.toml <<'PY'
import sys, tomllib
s = tomllib.load(open(sys.argv[1], 'rb'))
for m, v in s['module'].items():
    print('module', m, v['task'], sep='\t')
    for a in v['artifacts']:
        print('artifact', m, a, sep='\t')
for p, m in s['overrides'].items():
    print('override', p, m, sep='\t')
PY
)
declare -A task=() winner=()
modules=()
while IFS=$'\t' read -r kind a b; do
  case $kind in
    module)
      modules+=("$a")
      task[$a]=$b
      ;;
    override) winner[$a]=$b ;;
  esac
done <<<"$spec"

errors=0
fail() {
  echo "stage: $*" >&2
  errors=$((errors + 1))
}

# Every file and symlink, as "path<TAB>module".
files=$(
  for m in "${modules[@]}"; do
    tree=out/build/$m/install
    if [[ -d $tree ]]; then
      (cd "$tree" && find . \( -type f -o -type l \) -printf "%P\t$m\n")
    else
      fail "no $tree (run \`mise run ${task[$m]}\`)"
    fi
  done | sort
)
((errors == 0)) || exit 1

conflicts=0
while IFS=$'\t' read -r path writers; do
  w=${winner[$path]:-}
  if [[ -z $w || " $writers " != *" $w "* ]]; then
    fail "$path is written by $writers (settle it in artifacts.toml [overrides])"
    conflicts=$((conflicts + 1))
  fi
done < <(awk -F'\t' '{ w[$1] = w[$1] " " $2; n[$1]++ } END { for (p in n) if (n[p] > 1) print p "\t" substr(w[p], 2) }' <<<"$files")

missing=0
while IFS=$'\t' read -r kind m path; do
  [[ $kind == artifact ]] || continue
  if [[ ! -e out/build/$m/install/$path ]]; then
    fail "$m: missing $path (run \`mise run ${task[$m]}\`)"
    missing=$((missing + 1))
  fi
done <<<"$spec"
echo "stage: $conflicts undeclared conflicts, $missing missing artifacts"
((errors == 0)) || exit 1

rm -rf "$stage"
mkdir -p "$stage"
for m in "${modules[@]}"; do
  cp -a "out/build/$m/install/." "$stage/"
done
for path in "${!winner[@]}"; do
  cp -a "out/build/${winner[$path]}/install/$path" "$stage/$path"
done

declare -A owner=()
while IFS=$'\t' read -r path m; do
  owner[$path]=${winner[$path]:-$m}
done <<<"$files"
mkdir -p "$stage/$(dirname "$manifest")"
printf '%s\n' "${!owner[@]}" | sort | while IFS= read -r path; do
  if [[ -L $stage/$path ]]; then
    sum=$(readlink "$stage/$path" | sha256sum)
  else
    sum=$(sha256sum <"$stage/$path")
  fi
  printf '%s\t%s\t%s\n' "$path" "${owner[$path]}" "${sum%% *}"
done >"$stage/$manifest"
echo "stage: $(wc -l <"$stage/$manifest") files from ${#modules[@]} modules in $stage"
