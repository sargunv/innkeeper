#!/usr/bin/env bash
# Compat harness: builds and runs compat/ (Rust) against the live session.
#   compat/run.sh [run] [--window] [--only PKG,...] [--accept] [--no-finish]
#   compat/run.sh fetch | report
# Without a running session it starts a headless one; at the end it leaves a
# session running with its window shown (--no-finish to skip).
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
export LINUXAPK_REPO=$repo
# A test session: links are only logged, and notifications and media players
# stay off the real desktop (session/linuxapk-session, docs/bridge.md).
export LINUXAPK_TEST=1
# A lane worktree has no extracted image; drive the main checkout's session.
if [[ -z ${LINUXAPK_SESSION:-} && ! -d $repo/out/img/system ]]; then
  main=$(dirname "$(git -C "$repo" rev-parse --path-format=absolute --git-common-dir)")
  export LINUXAPK_SESSION=$main/session/linuxapk-session
fi
# Android commands run through `linuxapk-session exec` inherit our niceness,
# and Android threads that set their priority back to 0 then fail: uiautomator
# dies at once with "SecurityException: Cannot set priority to 0" ("Killed").
# zsh runs background jobs at nice 5 (BG_NICE). Re-run at nice 0 through the
# user manager, which can raise priority back where we can't.
if (($(awk '{print $19}' /proc/$$/stat) > 0)) && [[ -z ${LINUXAPK_RENICED:-} ]]; then
  echo "compat: running at nice $(awk '{print $19}' /proc/$$/stat); restarting at nice 0" >&2
  env_args=()
  while IFS= read -r v; do
    case $v in
      LINUXAPK_* | WAYLAND_DISPLAY | DBUS_SESSION_BUS_ADDRESS | XDG_* | HOME | PATH) env_args+=("--setenv=$v=${!v}") ;;
    esac
  done < <(compgen -e)
  exec systemd-run --user --quiet --pipe --wait --collect -p Nice=0 --same-dir \
    "${env_args[@]}" --setenv=LINUXAPK_RENICED=1 -- "$0" "$@"
fi
cargo build -q -j 8 --release --manifest-path "$repo/compat/Cargo.toml"
exec "$repo/target/release/linuxapk-compat" "$@"
