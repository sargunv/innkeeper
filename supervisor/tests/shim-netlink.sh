#!/usr/bin/env bash
# The shim's uevent bind (shim/netlink.h): two sockets bound to one pid, as
# vold in two Flatpak sessions.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/shim-netlink
mkdir -p "$out"
cc -O1 -Wall -Werror -o "$out/t" "$here/shim-netlink.c"
"$out/t"
