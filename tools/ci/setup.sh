#!/usr/bin/env bash
# Hosted runners only: never run privileged setup on a contributor's machine.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true ]] || {
  echo 'GitHub runner required' >&2
  exit 1
}
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  clang pkg-config libgtk-4-dev libadwaita-1-dev libpulse-dev libxkbcommon-dev \
  podman uidmap passt flatpak cpio dbus-user-session kwin-wayland \
  xdg-desktop-portal xdg-desktop-portal-gtk xdg-desktop-portal-kde \
  at-spi2-core python3-pyatspi \
  libgl1-mesa-dri mesa-vulkan-drivers pipewire pipewire-pulse wireplumber pulseaudio-utils
# Ubuntu restricts mounts in unprivileged user namespaces by default.
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
if [[ ! -e /dev/udmabuf ]]; then
  sudo modprobe udmabuf
fi
sudo chmod 666 /dev/udmabuf
test -r /dev/udmabuf && test -w /dev/udmabuf
XDG_RUNTIME_DIR=/run/user/$(id -u)
export XDG_RUNTIME_DIR
sudo loginctl enable-linger "$USER"
sudo systemctl start "user@$(id -u).service"
systemctl --user start pipewire pipewire-pulse wireplumber
systemctl --user set-environment LIBGL_ALWAYS_SOFTWARE=1 KWIN_COMPOSE=Q
echo "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" >>"$GITHUB_ENV"
echo "DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus" >>"$GITHUB_ENV"
# Container builds mount the runner's home. Put the NDK inside that mount.
mkdir -p "$HOME/Android/Sdk"
cp -a "$ANDROID_HOME/licenses" "$HOME/Android/Sdk/"
echo "ANDROID_HOME=$HOME/Android/Sdk" >>"$GITHUB_ENV"
