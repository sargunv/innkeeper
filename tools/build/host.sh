#!/usr/bin/env bash
# build:host: what runs on the desktop and in the Flatpak sandbox, into
# out/build/host/install: bin/{innkeeper,innkeeper-manager},
# libexec/innkeeper/{innkeeper-bridge,innkeeper-session}, the manager's desktop
# entry, metainfo, icons and MIME types.
set -euo pipefail
cargo build -q --release -p innkeeper -p innkeeper-manager -p innkeeper-bridge -p innkeeper-session
bin=out/cargo/release
data=host/manager/data

h=out/build/host/install
rm -rf "$h"
install -D -m 755 -s -t "$h/bin" "$bin/innkeeper" "$bin/innkeeper-manager"
install -D -m 755 -s -t "$h/libexec/innkeeper" "$bin/innkeeper-bridge" "$bin/innkeeper-session"
install -D -m 644 -t "$h/share/applications" "$data/dev.sargunv.Innkeeper.desktop"
install -D -m 644 -t "$h/share/metainfo" "$data/dev.sargunv.Innkeeper.metainfo.xml"
install -D -m 644 -t "$h/share/icons/hicolor/scalable/apps" "$data/dev.sargunv.Innkeeper.svg"
install -D -m 644 -t "$h/share/icons/hicolor/symbolic/apps" "$data/dev.sargunv.Innkeeper-symbolic.svg"
install -D -m 644 -t "$h/share/mime/packages" "$data/dev.sargunv.Innkeeper.xml"
