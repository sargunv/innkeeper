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
export LINUXAPK_PROFILE=${LINUXAPK_PROFILE:-bench-$1-$(date +%s%N)}
[[ $LINUXAPK_PROFILE =~ ^bench-[a-z0-9-]+$ ]] || {
  echo 'expected fresh bench-* profile' >&2
  exit 1
}
# Refuse before installing traps: an existing profile belongs to someone else.
[[ ! -e out/bench/$LINUXAPK_PROFILE && ! -e out/dev/flatpak/$LINUXAPK_PROFILE ]] || {
  echo 'benchmark profile must be fresh' >&2
  exit 1
}
unit=linuxapk-bench-run-$LINUXAPK_PROFILE
cleanup() {
  local result=$? cg
  trap - EXIT
  if [[ -f out/dev/flatpak/$LINUXAPK_PROFILE/cgroup ]]; then
    cg=$(cat "out/dev/flatpak/$LINUXAPK_PROFILE/cgroup")
    systemctl --user stop "${cg##*/}" 2>/dev/null || true
  fi
  systemctl --user stop "linuxapk-nested-$LINUXAPK_PROFILE" 2>/dev/null || true
  systemctl --user stop "$unit.scope" 2>/dev/null || true
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT TERM
systemd-run --user --scope --quiet --unit="$unit" -p MemoryMax=6G -p TasksMax=1024 \
  cargo run -q -p linuxapk-bench -- "$@" &
wait "$!"
