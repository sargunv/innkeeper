#!/usr/bin/env bash
# The owned FUSE peer survives INIT until primary unmount, including remount.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/shim-fuse
mkdir -p "$out"
cc -O1 -Wall -Werror -o "$out/t" "$here/shim-fuse.c"
"$out/t"
