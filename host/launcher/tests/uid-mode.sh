#!/usr/bin/env bash
# The automatic uid mode, as `linuxapk-session status` reports it for a stopped
# instance (no session is started):
#  - subuid when the subordinate id files give the user 49999 ids or more
#    (and newuidmap is installed), single otherwise;
#  - an existing /data keeps its mode: single if the user owns its files,
#    subuid otherwise (tests/boot.sh checks both with a real /data).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
[[ -x $session ]] || {
  echo "no $session (run \`mise run build\`)" >&2
  exit 1
}
tmp=$repo/out/test/uid-mode
rm -rf "$tmp"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT
fail=0
check() { # name expected actual
  if [[ $2 == "$3" ]]; then echo "ok   $1"; else
    echo "FAIL $1: expected '$2', got '$3'"
    fail=1
  fi
}
# uid_mode <subid file> [data dir]
uid_mode() {
  env LINUXAPK_INSTANCE=uid-mode-test LINUXAPK_DATA="${2:-$tmp/fresh}" \
    LINUXAPK_SUBUID_FILE="$1" LINUXAPK_SUBGID_FILE="$1" "$session" status |
    sed -n 's/^uid-mode=//p'
}

ranged=subuid
command -v newuidmap newgidmap >/dev/null || ranged=single
echo "$(id -un):100000:65536" >"$tmp/by-name"
echo "$(id -u):100000:49999" >"$tmp/by-uid"
printf '%s\n' "someone-else:100000:65536" "$(id -un):100000:1000" >"$tmp/small"
: >"$tmp/empty"

check "a range for the user's name" "$ranged" "$(uid_mode "$tmp/by-name")"
check "a range for the user's uid, just big enough" "$ranged" "$(uid_mode "$tmp/by-uid")"
check "too small a range, or someone else's" single "$(uid_mode "$tmp/small")"
check "no range" single "$(uid_mode "$tmp/empty")"
check "no subordinate id files" single "$(uid_mode "$tmp/missing")"
mkdir -p "$tmp/single-data/system"
check "a single-mode /data stays single" single "$(uid_mode "$tmp/by-name" "$tmp/single-data")"
# Any /data whose system/ the user doesn't own: a root-owned stand-in (status
# only reads it).
check "a subuid /data stays subuid" subuid "$(uid_mode "$tmp/empty" /usr/lib/systemd)"
exit $fail
