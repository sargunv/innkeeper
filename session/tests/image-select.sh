#!/usr/bin/env bash
# Image selection in linuxapk-session (no session is started):
#  - the default image is the pin's top-level name, not an [[artifact]] name;
#  - LINUXAPK_IMAGE overrides it; unknown names are refused;
#  - the repo's image/pin.toml parses to what tools/fetch-image.sh downloads.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
session=$here/../linuxapk-session
repo=$(cd "$here/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
check() { # name expected actual
  if [[ $2 == "$3" ]]; then echo "ok   $1"; else
    echo "FAIL $1: expected '$2', got '$3'"
    fail=1
  fi
}
sel() { env -u LINUXAPK_IMAGE LINUXAPK_IMAGES="$tmp/images" LINUXAPK_OUT="$tmp/out" "$@" "$session" image | sed -n 's/^image=//p'; }
mkdir -p "$tmp/out"

cat >"$tmp/pin.toml" <<'PIN'
# comment
name = "ci-123"
url = "https://example.invalid/{artifact}"

[[artifact]]
name = "x-img-123.zip"
size = 1
sha256 = "00"

[[artifact]]
name = "sbom/sbom.spdx.json"
PIN
check "default is the pin's top-level name" ci-123 "$(sel env LINUXAPK_PIN="$tmp/pin.toml")"
check "LINUXAPK_IMAGE overrides the pin" emulator "$(sel env LINUXAPK_PIN="$tmp/pin.toml" LINUXAPK_IMAGE=emulator)"
check "no pin: emulator" emulator "$(sel env LINUXAPK_PIN="$tmp/missing.toml")"
if env LINUXAPK_IMAGE=bogus LINUXAPK_OUT="$tmp/out" "$session" image >/dev/null 2>&1; then
  check "unknown image refused" refused accepted
else
  check "unknown image refused" refused refused
fi
want=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["name"])' "$repo/image/pin.toml")
check "repo pin selected by default" "$want" "$(sel env)"
kind=$(env -u LINUXAPK_IMAGE LINUXAPK_OUT="$tmp/out" "$session" image | sed -n 's/^kind=//p')
check "repo pin is a cuttlefish image" cuttlefish "$kind"
exit $fail
