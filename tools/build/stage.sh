#!/usr/bin/env bash
# Copy every module's install tree (out/build/<module>/install) into out/stage,
# the installable prefix, and write its manifest, share/innkeeper/manifest.tsv:
# one "path<TAB>module<TAB>sha256" line per file (a symlink's sha256 is of its
# target). Fails if two modules write one path.
set -euo pipefail
cd "$(dirname "$0")/../.."
stage=out/stage
manifest=share/innkeeper/manifest.tsv

# Every file and symlink, as "path<TAB>module".
rows=$(
  for tree in out/build/*/install; do
    m=${tree#out/build/} m=${m%/install}
    (cd "$tree" && find . \( -type f -o -type l \) -printf "%P\t$m\n")
  done | sort
)
dups=$(awk -F'\t' '{ w[$1] = w[$1] " " $2; n[$1]++ } END { for (p in n) if (n[p] > 1) print p ":" w[p] }' <<<"$rows")
if [[ -n $dups ]]; then
  echo "stage: paths written by more than one out/build/<module>/install (remove a stale one):" >&2
  sort <<<"$dups" >&2
  exit 1
fi

rm -rf "$stage"
mkdir -p "$stage"
for tree in out/build/*/install; do
  cp -a "$tree/." "$stage/"
done

mkdir -p "$stage/$(dirname "$manifest")"
while IFS=$'\t' read -r path m; do
  if [[ -L $stage/$path ]]; then
    sum=$(readlink "$stage/$path" | sha256sum)
  else
    sum=$(sha256sum <"$stage/$path")
  fi
  printf '%s\t%s\t%s\n' "$path" "$m" "${sum%% *}"
done <<<"$rows" >"$stage/$manifest"
echo "stage: $(wc -l <"$stage/$manifest") files in $stage"
