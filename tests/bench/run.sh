#!/usr/bin/env bash
# Manual only: builds and measurements share a bounded host scope.
set -euo pipefail
cd "$(dirname "$0")/../.."
case ${1:-} in
  input | frame | audio | gpu | memory | binder) ;;
  *)
    echo 'usage: mise run bench -- input|frame|audio|gpu|memory|binder [--noisy] [--seconds N] [--max-load N]'
    exit 0
    ;;
esac
export LINUXAPK_INSTANCE=${LINUXAPK_INSTANCE:-bench-$1-$(date +%s%N)}
[[ $LINUXAPK_INSTANCE =~ ^bench-[a-z0-9-]+$ ]] || {
  echo 'expected fresh bench-* instance' >&2
  exit 1
}
# Refuse before installing traps: an existing instance belongs to someone else.
[[ ! -e out/bench/$LINUXAPK_INSTANCE && ! -e out/dev/flatpak/$LINUXAPK_INSTANCE ]] || {
  echo 'benchmark instance must be fresh' >&2
  exit 1
}
unit=linuxapk-bench-run-$LINUXAPK_INSTANCE
cleanup() {
  local result=$? cg
  trap - EXIT
  if [[ -f out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup ]]; then
    cg=$(cat "out/dev/flatpak/$LINUXAPK_INSTANCE/cgroup")
    systemctl --user stop "${cg##*/}" 2>/dev/null || true
  fi
  systemctl --user stop "linuxapk-nested-$LINUXAPK_INSTANCE" 2>/dev/null || true
  systemctl --user stop "$unit.scope" 2>/dev/null || true
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
systemd-run --user --scope --quiet --unit="$unit" -p MemoryMax=6G -p TasksMax=1024 \
  cargo run -q -p linuxapk-bench -- "$@" &
wait "$!"
