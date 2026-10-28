#!/usr/bin/env bash
# build:host: what runs on the desktop. Two install trees:
#   host      bin/{linuxapk,linuxapk-manager}, libexec/linuxapk/linuxapk-bridge,
#             the manager's desktop entry, metainfo, icon and MIME types
#   launcher  libexec/linuxapk/linuxapk-session
set -euo pipefail
cargo build -q --release -p linuxapk -p linuxapk-manager -p linuxapk-bridge -p linuxapk-session
bin=out/cargo/release
data=host/manager/data

h=out/build/host/install
rm -rf "$h"
install -D -m 755 -s -t "$h/bin" "$bin/linuxapk" "$bin/linuxapk-manager"
install -D -m 755 -s -t "$h/libexec/linuxapk" "$bin/linuxapk-bridge"
install -D -m 644 -t "$h/share/applications" "$data/dev.linuxapk.Manager.desktop"
install -D -m 644 -t "$h/share/metainfo" "$data/dev.linuxapk.Manager.metainfo.xml"
install -D -m 644 -t "$h/share/icons/hicolor/scalable/apps" "$data/dev.linuxapk.Manager.svg"
install -D -m 644 -t "$h/share/mime/packages" "$data/dev.linuxapk.Manager.xml"

l=out/build/launcher/install
rm -rf "$l"
install -D -m 755 -s -t "$l/libexec/linuxapk" "$bin/linuxapk-session"
