#!/usr/bin/env bash
# Exercise the actual seccomp receiver boundary, including interrupt/restart races.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here/../../out/test/supervisor/binder-restarts
mkdir -p "$out"
cargo build -q -p innkeeper-broker --example supervisor-probes
cc -O1 -Wall -Werror -o "$out/t" "$here/binder-restarts.c" \
  "$here/../../out/cargo/debug/examples/libsupervisor_probes.a" -lpthread -ldl -lm
timeout --kill-after=2 10 "$out/t"
