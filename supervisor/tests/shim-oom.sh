#!/usr/bin/env bash
# Unit coverage of the Android-to-kernel OOM score mapping.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/shim-oom
mkdir -p "$out"
cc -O1 -Wall -Werror -o "$out/t" "$here/shim-oom.c"
"$out/t"
