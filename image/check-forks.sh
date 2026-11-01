#!/usr/bin/env bash
# Checks the upstream files that our overlay forks are based on, by hash. A pin
# bump must port every changed fork before it updates the fork's base hash. The
# hash list stores no image content. A line's optional third field names an
# overlay file that holds override stanzas for the base's services, in place of
# a fork at the same path.
set -euo pipefail
img=${1:?img dir} apex=${2:?apex dir} bases=${3:?base hashes} overlay=${4:?overlay dir}
failed=0
while read -r want path ours; do
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
    if [[ -n $ours ]]; then
      echo "  its services are overridden in overlay/$ours: port the changes there" >&2
    elif [[ -f $base ]]; then
      diff -u --label "image/$path" --label "overlay/$path" "$base" "$overlay/$path" >&2 || true
    fi
    failed=1
  fi
done <"$bases"
if ((failed)); then
  echo 'Port the overlay forks to this image, then update image/fork-bases.sha256 (see image/README.md).' >&2
  exit 1
fi
