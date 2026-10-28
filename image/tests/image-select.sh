#!/usr/bin/env bash
# Image selection in linuxapk-session (no session is started):
#  - the image is the pin's top-level name, not an [[artifact]] name;
#  - a missing pin is refused;
#  - the repo's image/pin.toml parses to what image/fetch.sh downloads.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
session=$repo/out/stage/libexec/linuxapk/linuxapk-session
[[ -x $session ]] || {
  echo "no $session (run \`mise run build\`)" >&2
  exit 1
}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0
check() { # name expected actual
  if [[ $2 == "$3" ]]; then echo "ok   $1"; else
    echo "FAIL $1: expected '$2', got '$3'"
    fail=1
  fi
}
sel() { env LINUXAPK_IMAGES="$tmp/images" "$@" "$session" image | sed -n 's/^image=//p'; }

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
check "the pin's top-level name" ci-123 "$(sel env LINUXAPK_PIN="$tmp/pin.toml")"
if env LINUXAPK_PIN="$tmp/missing.toml" "$session" image >/dev/null 2>&1; then
  check "missing pin refused" refused accepted
else
  check "missing pin refused" refused refused
fi
want=$(python3 -I -c 'import sys,tomllib; print(tomllib.load(open(sys.argv[1],"rb"))["name"])' "$repo/image/pin.toml")
check "repo pin selected by default" "$want" "$(sel env)"
exit $fail
