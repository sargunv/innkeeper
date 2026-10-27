#!/usr/bin/env bash
# The bpf() emulator (pivot-exec/bpfemu.c) survives hostile callers: element
# operations on ring buffers, oversized LPM prefixes, FIFO and symlinked pins,
# and unbounded object creation (see bpfemu.c for the cases). Runs in a
# private user and mount namespace with a tmpfs on /sys/fs/bpf, as the
# session has.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
cc -O1 -Wall -Werror -o "$tmp/t" "$here/bpfemu.c" "$here/../pivot-exec/bpfemu.c" -lpthread
mkdir "$tmp/outside"
# shellcheck disable=SC2016 # expanded by the inner shell
unshare -Urm sh -c 'mount -t tmpfs bpf /sys/fs/bpf && exec "$1" "$2"' sh "$tmp/t" "$tmp/outside"
