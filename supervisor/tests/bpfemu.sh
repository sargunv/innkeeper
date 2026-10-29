#!/usr/bin/env bash
# The bpf() emulator (broker/src/bpf.rs) survives hostile callers: element
# operations on ring buffers, oversized LPM prefixes, FIFO and symlinked pins,
# and unbounded object creation (see bpfemu.c for the cases). Runs in a
# private user and mount namespace with a tmpfs on /sys/fs/bpf, as the
# session has. Builds into out/test/supervisor/bpfemu.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/bpfemu
rm -rf "$out"
mkdir -p "$out/outside"
cargo build -q -p linuxapk-broker --example supervisor-probes
cc -O1 -Wall -Werror -o "$out/t" "$here/bpfemu.c" "$here/../../out/cargo/debug/examples/libsupervisor_probes.a" -lpthread -ldl -lm
# shellcheck disable=SC2016 # expanded by the inner shell
unshare -Urm sh -c 'mount -t tmpfs bpf /sys/fs/bpf && exec "$1" "$2"' sh "$out/t" "$out/outside"
