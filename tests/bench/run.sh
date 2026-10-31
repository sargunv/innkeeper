#!/usr/bin/env bash
# Manual only: builds and measurements share a bounded host scope.
set -euo pipefail
cd "$(dirname "$0")/../.."
case ${1:-} in
  input | frame) ;;
  *)
    echo 'usage: mise run bench -- input|frame [--noisy] [--seconds N] [--max-load N]'
    exit 0
    ;;
esac
export INNKEEPER_PROFILE=${INNKEEPER_PROFILE:-bench-$1-$(date +%s%N)} INNKEEPER_TEST=1
[[ $INNKEEPER_PROFILE =~ ^bench-[a-z0-9-]+$ ]] || {
  echo 'expected fresh bench-* profile' >&2
  exit 1
}
# Refuse before installing traps: an existing profile belongs to someone else.
[[ ! -e out/bench/$INNKEEPER_PROFILE && ! -e out/dev/flatpak/$INNKEEPER_PROFILE ]] || {
  echo 'benchmark profile must be fresh' >&2
  exit 1
}
unit=innkeeper-bench-run-$INNKEEPER_PROFILE
cleanup() {
  local result=$? cg
  trap - EXIT
  if cg=$(tools/dev/session.sh cgroup 2>/dev/null); then
    systemctl --user stop "${cg##*/}" 2>/dev/null || true
  fi
  systemctl --user stop "inkp-nested-$INNKEEPER_PROFILE" 2>/dev/null || true
  systemctl --user stop "$unit.scope" 2>/dev/null || true
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
systemd-run --user --scope --quiet --unit="$unit" -p MemoryMax=6G -p TasksMax=1024 \
  cargo run -q -p innkeeper-bench -- "$@" &
wait "$!"
