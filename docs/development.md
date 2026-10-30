# Development

## Prerequisites

mise provides Rust (with the `x86_64-linux-android` target), Java, CMake, Ninja,
dprint, shellcheck, shfmt, hk and jactionlint (`mise install`). The rest comes
from the host; none of it needs root.

| For               | What                                                                                                                                                                                                       |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| building          | Android SDK at `$ANDROID_HOME` (default `~/Android/Sdk`) with NDK `30.0.16248370`, build-tools `37.0.0` and `platforms;android-37.0`; podman; clang; GTK 4 (4.18+) and libadwaita (1.7+) development files |
| running a session | Flatpak, a Wayland desktop, GPU access (`/dev/udmabuf`) and audio; development sessions also use `systemd --user` and Python 3.11+                                                                         |

## Build

`mise run build` runs every `build:<module>` task, each into its own
`out/build/<module>/install` tree (paths relative to the prefix), then `stage`
merges them into `out/stage` and writes `share/linuxapk/manifest.tsv` (path,
module, sha256 per file). Stage fails if a module misses an artifact declared in
[tools/build/artifacts.toml](../tools/build/artifacts.toml) or two modules write
the same path. Nothing at runtime builds anything.

```mermaid
flowchart LR
  host["build:host<br/>cargo (glibc)"] --> stage
  ndk["ndk<br/>CMake superbuild"] --> sup["build:supervisor<br/>+ cargo (bionic broker)"] --> stage
  ndk --> gfx["build:graphics"] --> stage
  ndk --> aud["build:audio"] --> stage
  ndk --> sys["build:system"] --> stage
  vk["build:vulkan<br/>Mesa (container)"] --> stage
  it["build:image-tools<br/>(container)"] --> stage
  app["build:system-app<br/>Gradle"] --> stage
  data["build:data<br/>overlay, image config"] --> stage
  stage["stage → out/stage"]
```

- **One toolchain pin**,
  [tools/build/toolchain.env](../tools/build/toolchain.env): mise loads it,
  container builds get it with `--env-file`, and Gradle reads it from the
  environment.
- **One NDK superbuild**, the root `CMakeLists.txt` (presets `android` and
  `host-tests`), builds every Android-side C/C++ artifact; each module installs
  its CMake component.
- **One build container**, [tools/container](../tools/README.md), for Mesa and
  the static image tools. Every run is capped (`--memory`, `--pids-limit`);
  those builds are skipped while their inputs hash to the same stamp.
- `mise run clean` removes build output and keeps test results.

## Run

- `mise run session -- start|stop|status|log|exec|shell|selftest|prepare` runs
  the staged launcher ([host/launcher](../host/launcher/README.md)) in a private
  Flatpak, capped from the host, with scratch HOME and XDG dirs under
  `out/dev/flatpak/<instance>` (default instance `dev`); it waits while less
  than 25 GiB is available
  ([packaging/flatpak](../packaging/flatpak/README.md#development)).
- `tools/dev/linuxapk.sh install app.apk`, `run`, `uninstall`, `list`
  ([host/cli](../host/cli/README.md)) in the same Flatpak. It needs a desktop of
  its own: `LINUXAPK_INSTANCE=cli tests/nested.sh tools/dev/linuxapk.sh list`
  runs it on a nested KWin with private PipeWire/Pulse (no hardware monitors), a
  private bus, AT-SPI bus, runtime dir and HOME/XDG dirs, and removes them
  after.
- `packaging/flatpak/build.sh` builds the Flatpak from the committed tree and
  installs it into `FLATPAK_USER_DIR`; use a private one under `out/`
  ([packaging/flatpak](../packaging/flatpak/README.md)).
- `mise run install-dev` builds and installs the committed Flatpak in the user
  installation, adding the manager to the app menu as an APK handler. Do this
  only for a checkout you mean to use as your Linux.apk.
- Use `LINUXAPK_INSTANCE=<name>` for anything experimental: it is a separate
  session with its own `/data`, logs and sandbox, and stays off the real desktop
  ([security.md](security.md#test-sessions)).

## Test

Test tiers and their mise tasks are in [tests/README.md](../tests/README.md).
`mise run check` (hk and dprint) is the pre-commit gate; `mise run fix` formats.

## Android logs

Use a bounded snapshot inside a named session:

```sh
LINUXAPK_INSTANCE=debug mise run session -- exec timeout 10 logcat -b all -d -t 100
```

`logcat -b all` streams until interrupted; waiting for it to exit is expected.
`-b` selects buffers, `-d` dumps and exits, and `-t 100` also implies `-d`
(unlike `-T 100`, which keeps streaming). The timeout bounds a stalled reader.
In the #49 investigation, each buffer (`main`, `system`, `crash`, `events`,
`radio`, `stats`, `security`, `kernel`) and the combined dump returned within
one second in a fresh session. No buffer deadlock was reproduced.

The `kernel` buffer is empty: logd cannot read the host's `/proc/kmsg`, and the
supervisor does not emulate that stream. Android init's `/dev/kmsg` writes go to
the launcher's FIFO reader instead (`session -- log`). Empty `kernel` and
`security` buffers do not hold a dump open; `security` also requires Android's
device-owner logging configuration.

There is no tracefs. libcutils turns off atrace after failing to open
`trace_marker`; `image/vendor.prop` silences the resulting per-process
`cutils-trace: Error opening trace file` through `log.tag.cutils-trace=S`. This
leaves tracing unavailable and avoids exposing host tracefs or pretending that
trace markers were recorded.

OpenTTD 14.1.rev128's `Setting the swap interval is not supported` is expected
from its bundled `libsdl-1.2.so`. Its
[`SDL_GL_SetSwapInterval`](https://github.com/pelya/commandergenius/blob/sdl_android/project/jni/sdl-1.2/src/video/android/SDL_video.c)
reports this when the video driver has no `GL_SetSwapInterval` callback, before
calling EGL. This reports an unavailable SDL setting rather than an ANGLE
`eglSwapInterval` failure. Changing the bundled SDL belongs upstream, so we
leave that app diagnostic visible.

## A nested, isolated desktop

Features that open windows, portals or notifications are tested in a nested
compositor that is fully isolated from the user's desktop: a private D-Bus
session, a private `XDG_RUNTIME_DIR` and XDG dirs under `out/`, `DISPLAY` unset,
and `WAYLAND_DISPLAY` set to the nested socket before the bus starts, so
whatever the bus activates (portals, kded) inherits it.

```sh
N=$PWD/out/nested
mkdir -p $N/{home,config,data,cache,state}
# A short runtime dir: a socket path must fit in 108 bytes, which a deep
# checkout's out/ can exceed (KWin then dumps core). Remove it when done.
RT=$(mktemp -d -p "$XDG_RUNTIME_DIR" lapk-XXXXXX)
printf '#!/bin/sh\necho "$DBUS_SESSION_BUS_ADDRESS" >%s/bus\nexec sleep infinity\n' $N >$N/hold.sh
chmod +x $N/hold.sh   # the session's command: publishes the nested bus, then waits
E=(-u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS HOME=$N/home XDG_RUNTIME_DIR=$RT
   XDG_CONFIG_HOME=$N/config XDG_DATA_HOME=$N/data XDG_CACHE_HOME=$N/cache XDG_STATE_HOME=$N/state
   XDG_CURRENT_DESKTOP=KDE)
systemd-run --user --unit linuxapk-nested-kwin -p MemoryMax=3G -- env "${E[@]}" WAYLAND_DISPLAY=lapk-nested \
  dbus-run-session -- kwin_wayland --virtual --no-lockscreen --socket lapk-nested \
  --width 1920 --height 1080 --exit-with-session $N/hold.sh
WAYLAND_DISPLAY=$RT/lapk-nested DBUS_SESSION_BUS_ADDRESS=$(cat $N/bus) \
  LINUXAPK_INSTANCE=nested mise run session -- start
# linuxapk writes desktop entries under XDG_DATA_HOME for every instance, so
# point it at $N while keeping the session's image and /data:
D=(XDG_DATA_HOME=$N/data XDG_CONFIG_HOME=$N/config LINUXAPK_INSTANCE=nested
   LINUXAPK_IMAGES=$HOME/.local/share/linuxapk/images
   LINUXAPK_DATA=$HOME/.local/share/linuxapk/data-nested)
env "${D[@]}" out/stage/bin/linuxapk install some.apk
```

- Check `/proc/<pid>/environ` of every process on the nested bus (private
  `HOME`, nested `WAYLAND_DISPLAY`) before testing. Never start plasmashell,
  kded, akonadi or kdeconnect against the real dirs, and stop every unit you
  started when done.
- KWin's `org_kde_kwin_fake_input` gives real seat input. KWin offers it only to
  a client whose resolved executable (e.g. `/usr/bin/python3.14`, not the
  symlink) is the `Exec` of a `.desktop` file in `$N/data/applications` with
  `X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input`. `spectacle -b -n` with the
  nested bus captures screenshots.
- Inside Android, plain `screencap` shows only the launcher: each app window is
  its own virtual display, named `linuxapk:<package>`. Capture one with
  `screencap -d <id> -p <file>`, taking the id from
  `dumpsys SurfaceFlinger --display-id`, as tests/compat does.
- A fake `x-scheme-handler/https` handler in `$N/data/applications` logs what
  OpenURI opens, and a null sink (`LINUXAPK_AUDIO_SINK`) keeps media silent.
