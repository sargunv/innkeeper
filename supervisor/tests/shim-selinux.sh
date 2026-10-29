#!/usr/bin/env bash
# init must be able to read executable labels on both SELinux and AppArmor hosts.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/shim-selinux
mkdir -p "$out"
cc -shared -fPIC -O1 -Wall -Werror -o "$out/liblabels.so" "$here/selinux-labels.c"
cc -O1 -Wall -Werror -o "$out/t" "$here/shim-selinux.c" -ldl
"$out/t" "$out"
