#!/usr/bin/env bash
# Reproduce the duplicate local pid used by concurrent Flatpak vold instances.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/shim-netlink
mkdir -p "$out"
cc -O1 -Wall -Werror -o "$out/t" "$here/shim-netlink.c"
"$out/t"
