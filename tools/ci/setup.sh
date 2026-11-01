#!/usr/bin/env bash
# Provision a GitHub-hosted runner. The script uses sudo, so it refuses to run
# anywhere else, including a contributor's machine. The build mode prepares to
# compile, lint and unit-test the sources. The runtime mode prepares to run or
# package the stage on a software-rendered desktop.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true ]] || {
  echo 'GitHub runner required' >&2
  exit 1
}
mode=${1:?build or runtime}
sudo apt-get update -qq
case $mode in
  build)
    sudo apt-get install -y --no-install-recommends \
      clang pkg-config libgtk-4-dev libadwaita-1-dev libpulse-dev libxkbcommon-dev \
      podman uidmap passt
    {
      echo "CARGO_BUILD_JOBS=4"
      # Debug symbols and per-checkout incremental state would inflate the
      # caches without speeding up CI.
      echo "CARGO_PROFILE_DEV_DEBUG=0"
      echo "CARGO_PROFILE_TEST_DEBUG=0"
      echo "CARGO_INCREMENTAL=0"
      echo "CMAKE_BUILD_PARALLEL_LEVEL=4"
      # sccache comes from mise and the CMake presets. tools/build/sccache.sh
      # finds the workspace with git worktree list and uses it as the base
      # directory.
      echo "SCCACHE_CACHE_SIZE=500M"
      # CI caches out/cargo whole. Wrapping rustc with sccache would also copy
      # those artifacts into sccache's size-capped cache.
      echo "RUSTC_WRAPPER="
    } >>"$GITHUB_ENV"
    ;;
  runtime) ;;
  *) exit 2 ;;
esac
# The build container, supervisor unit tests and Flatpak need unprivileged mount namespaces.
sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
# Gralloc's unit tests and Android graphics need real dma-buf allocations.
if [[ ! -e /dev/udmabuf ]]; then
  sudo modprobe udmabuf
fi
sudo chmod 666 /dev/udmabuf
test -r /dev/udmabuf && test -w /dev/udmabuf
[[ $mode == runtime ]] || exit 0
sudo apt-get install -y --no-install-recommends \
  podman uidmap passt flatpak fuse3 cpio dbus-user-session gnome-shell mutter \
  xdg-desktop-portal xdg-desktop-portal-gtk \
  at-spi2-core python3-pyatspi python3-dbus python3-gi wl-clipboard \
  libgl1-mesa-dri mesa-vulkan-drivers pipewire pipewire-pulse wireplumber pulseaudio-utils
XDG_RUNTIME_DIR=/run/user/$(id -u)
export XDG_RUNTIME_DIR
sudo loginctl enable-linger "$USER"
sudo systemctl start "user@$(id -u).service"
systemctl --user start pipewire pipewire-pulse wireplumber
# Mutter uses surfaceless llvmpipe and advertises linux-dmabuf without a DRM node.
systemctl --user set-environment LIBGL_ALWAYS_SOFTWARE=1
{
  echo "INNKEEPER_TEST_COMPOSITOR=mutter"
  echo "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
  echo "DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus"
} >>"$GITHUB_ENV"
