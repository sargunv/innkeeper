#!/usr/bin/env bash
# Run a CI test on a private bus, runtime dir, audio server and virtual KWin.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
N=$repo/out/ci/desktop
mkdir -p "$N"/{home,config,data,cache,state,bin}
rt=$(mktemp -d -p "$XDG_RUNTIME_DIR" lapk-ci-XXXXXX)
unit=linuxapk-ci-desktop
# Only the user manager's control socket is shared; portals and audio stay
# private. The host launcher creates its capped service through this socket.
ln -s "$XDG_RUNTIME_DIR/systemd" "$rt/systemd"
cleanup() {
  systemctl --user stop "$unit.scope" 2>/dev/null || true
  rm -rf "$rt"
}
trap cleanup EXIT
# Preserve the mise tool paths and caches when HOME becomes private.
export MISE_DATA_DIR=${MISE_DATA_DIR:-$HOME/.local/share/mise}
export MISE_CACHE_DIR=${MISE_CACHE_DIR:-$HOME/.cache/mise}
export CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
export GRADLE_USER_HOME=${GRADLE_USER_HOME:-$HOME/.gradle}
export LINUXAPK_INSTANCE=ci
export LINUXAPK_DATA=$N/android-data
export LINUXAPK_IDLE_GRACE=0
# The staged launcher starts a separate service. Forward this test's desktop
# environment to that service without changing the user's manager environment.
cat >"$N/bin/systemd-run" <<'RUN'
#!/usr/bin/env bash
set -euo pipefail
args=()
for key in HOME PATH XDG_RUNTIME_DIR XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME PULSE_SERVER; do
  [[ ! -v $key ]] || args+=("--setenv=$key=${!key}")
done
exec /usr/bin/systemd-run "${args[@]}" "$@"
RUN
chmod +x "$N/bin/systemd-run"
cat >"$N/session.sh" <<'SESSION'
#!/usr/bin/env bash
set -euo pipefail
pipewire >"$CI_DESKTOP/pipewire.log" 2>&1 &
pipewire-pulse >"$CI_DESKTOP/pulse.log" 2>&1 &
wireplumber >"$CI_DESKTOP/wireplumber.log" 2>&1 &
kwin_wayland --virtual --no-lockscreen --socket lapk-ci --width 1280 --height 800 \
  >"$CI_DESKTOP/kwin.log" 2>&1 &
for _ in $(seq 100); do
  [[ -S $XDG_RUNTIME_DIR/lapk-ci && -S $XDG_RUNTIME_DIR/pulse/native ]] && break
  sleep 0.1
done
test -S "$XDG_RUNTIME_DIR/lapk-ci"
test -S "$XDG_RUNTIME_DIR/pulse/native"
trap 'mise run session -- stop || true' EXIT
"$@"
SESSION
chmod +x "$N/session.sh"
systemd-run --user --scope --collect --unit "$unit" -p MemoryMax=10G \
  -- env -u DISPLAY -u DBUS_SESSION_BUS_ADDRESS -u AT_SPI_BUS_ADDRESS \
  HOME="$N/home" XDG_RUNTIME_DIR="$rt" XDG_CONFIG_HOME="$N/config" \
  XDG_DATA_HOME="$N/data" XDG_CACHE_HOME="$N/cache" XDG_STATE_HOME="$N/state" \
  WAYLAND_DISPLAY=lapk-ci XDG_CURRENT_DESKTOP=KDE CI_DESKTOP="$N" \
  PATH="$N/bin:$PATH" PULSE_SERVER="unix:$rt/pulse/native" \
  LIBGL_ALWAYS_SOFTWARE=1 KWIN_COMPOSE=Q \
  dbus-run-session -- "$N/session.sh" "$@"
