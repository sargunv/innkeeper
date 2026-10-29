#!/usr/bin/env bash
# Check upstream bases, not our edited overlay copies. A pin bump must port
# every changed fork before updating its base hash. No image content is stored.
set -euo pipefail
img=${1:?img dir} apex=${2:?apex dir} bases=${3:?base hashes} overlay=${4:?overlay dir}
failed=0
while read -r want path; do
  [[ -n $want && $want != \#* ]] || continue
  case $path in
    system/*) base=$img/system/$path ;;
    apex/*) base=$apex/${path#apex/} ;;
    *)
      echo "unknown fork base: $path" >&2
      exit 1
      ;;
  esac
  got=missing
  if [[ -f $base ]]; then
    got=$(sha256sum <"$base")
    got=${got%% *}
  fi
  if [[ $got != "$want" ]]; then
    echo "image fork drift: $path (base $want, image $got)" >&2
    if [[ -f $base ]]; then diff -u --label "image/$path" --label "overlay/$path" "$base" "$overlay/$path" >&2 || true; fi
    failed=1
  fi
done <"$bases"
if ((failed)); then
  echo 'Port the overlay forks to this image, then update image/fork-bases.sha256 (see image/README.md).' >&2
  exit 1
fi
