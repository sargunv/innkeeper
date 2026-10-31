#!/usr/bin/python3
"""One session API check in the input group: cache and runtime permission."""

import json
import os
from pathlib import Path

import dbus

name = "dev.linuxapk.Manager.Session.i_" + os.environ[
  "LINUXAPK_PROFILE"
].replace("-", "_")
proxy = dbus.Interface(
  dbus.SessionBus().get_object(name, "/dev/linuxapk/Session"),
  "dev.linuxapk.Session1",
)
package = "dev.linuxapk.probes"


def call(action, permission="", enabled=False):
  response = json.loads(
    proxy.App(package, action, permission, enabled, timeout=22)
  )
  assert not response["error"], response
  return response["info"]


def granted(info):
  return next(
    p["granted"] for p in info["permissions"] if p["name"] == permission
  )


permission = "android.permission.POST_NOTIFICATIONS"
data = Path(os.environ.get("LINUXAPK_DATA", ""))
if not str(data) or data == Path("."):
  data = (
    Path(__file__).resolve().parents[2]
    / "out/dev/flatpak"
    / os.environ["LINUXAPK_PROFILE"]
    / "android-data"
  )
cache = data / "data" / package / "cache/session-api-probe"
files = data / "data" / package / "files/session-api-probe"
assert not granted(call("permission", permission, False))
call("clear-cache")
assert not cache.exists(), "clear-cache left the probe's temporary file"
assert files.read_text() == "persistent", "clear-cache removed app data"
# The fresh install grants this; restore it for the notification scenario.
call("permission", permission, True)
print(
  "ok   session D-Bus clears cache, preserves data and toggles a runtime permission"
)
