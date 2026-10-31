#!/usr/bin/env bash
# build:host: what runs on the desktop. Two install trees:
#   host      bin/{innkeeper,innkeeper-manager}, libexec/innkeeper/innkeeper-bridge,
#             the manager's desktop entry, metainfo, icon and MIME types
#   launcher  libexec/innkeeper/innkeeper-session
set -euo pipefail
cargo build -q --release -p innkeeper -p innkeeper-manager -p innkeeper-bridge -p innkeeper-session
bin=out/cargo/release
data=host/manager/data

h=out/build/host/install
rm -rf "$h"
install -D -m 755 -s -t "$h/bin" "$bin/innkeeper" "$bin/innkeeper-manager"
install -D -m 755 -s -t "$h/libexec/innkeeper" "$bin/innkeeper-bridge"
install -D -m 644 -t "$h/share/applications" "$data/dev.sargunv.Innkeeper.desktop"
install -D -m 644 -t "$h/share/metainfo" "$data/dev.sargunv.Innkeeper.metainfo.xml"
install -D -m 644 -t "$h/share/icons/hicolor/scalable/apps" "$data/dev.sargunv.Innkeeper.svg"
install -D -m 644 -t "$h/share/icons/hicolor/symbolic/apps" "$data/dev.sargunv.Innkeeper-symbolic.svg"
install -D -m 644 -t "$h/share/mime/packages" "$data/dev.sargunv.Innkeeper.xml"

l=out/build/launcher/install
rm -rf "$l"
install -D -m 755 -s -t "$l/libexec/innkeeper" "$bin/innkeeper-session"
