#!/usr/bin/env bash
# Memory of a running session (INNKEEPER_PROFILE names its profile): PSS and RSS of every
# process in it, largest PSS first, then the session cgroup's totals.
#   tools/dev/footprint.sh [-n N]    (only the N largest processes)
# Android's processes are read from inside the session, since
# the host user can't read them; the outer ones (bridge, pasta, pivot-exec,
# the broker) from the host.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
session=$repo/tools/dev/session.sh
top=0
[[ ${1:-} == -n ]] && top=$2

pid=$("$session" status | sed -n 's/^running: host pid //p')
[[ -n $pid ]] || {
  echo "no running session${INNKEEPER_PROFILE:+ ($INNKEEPER_PROFILE)}" >&2
  exit 1
}
cg=$("$session" cgroup)

# "pss_kb rss_kb pid name" for each readable /proc/<pid> in $@ (default: all).
# shellcheck disable=SC2016 # also runs inside the session
rollup='[ $# -gt 0 ] || set -- /proc/[0-9]*
for d in "$@"; do
  [ -r "$d/smaps_rollup" ] || continue
  n=$(tr "\0" "\n" <"$d/cmdline" 2>/dev/null | head -n 1)
  [ -n "$n" ] || n="[$(cat "$d/comm")]"
  awk -v n="${n##*/}" -v p="${d#/proc/}" "/^Rss:/ { r = \$2 } /^Pss:/ { s = \$2 }
    END { if (r) print s, r, p, n }" "$d/smaps_rollup" 2>/dev/null
done
true'

# The dev driver reports a scope PID, which may be outside Android's PID namespace.
android_ns=$("$session" exec readlink /proc/self/ns/pid)
outer=()
while read -r p; do
  [[ $(readlink "/proc/$p/ns/pid" 2>/dev/null) == "$android_ns" ]] || outer+=("/proc/$p")
done < <(cat "$cg"/cgroup.procs "$cg"/*/cgroup.procs "$cg"/*/*/cgroup.procs 2>/dev/null)

{
  "$session" exec sh -c "$rollup" | sed 's/$/ android/'
  ((${#outer[@]})) && sh -c "$rollup" sh "${outer[@]}" | sed 's/$/ outer/'
} | sort -rn | awk -v top="$top" '
  { pss += $1; rss += $2; n++ }
  !top || n <= top { printf "%8.1f %8.1f  %-6s %s\n", $1 / 1024, $2 / 1024, $5, $4 }
  END { printf "%8.1f %8.1f  total PSS / RSS (MiB), %d processes\n", pss / 1024, rss / 1024, n }' |
  sed '1i\     PSS      RSS  where  process (MiB)'

awk -v cur="$(cat "$cg/memory.current")" '
  $1 ~ /^(anon|file|shmem|kernel|sock)$/ { v[$1] = $2 }
  END {
    printf "cgroup (MiB): current %.0f = anon %.0f + file %.0f (of it shmem %.0f) + kernel %.0f + sock %.0f; without page cache %.0f\n",
      cur / 1048576, v["anon"] / 1048576, v["file"] / 1048576, v["shmem"] / 1048576,
      v["kernel"] / 1048576, v["sock"] / 1048576, (cur - v["file"] + v["shmem"]) / 1048576
  }' "$cg/memory.stat"
