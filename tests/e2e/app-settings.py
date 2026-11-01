#!/usr/bin/python3
"""Check `innkeeper app` in the input group by clearing an app's cache and
toggling a runtime permission."""

import json
import os
from pathlib import Path
import subprocess

TOP = Path(__file__).resolve().parents[2]
package = "dev.sargunv.innkeeper.probes"
permission = "android.permission.POST_NOTIFICATIONS"


def innkeeper(*args):
  return subprocess.run(
    [str(TOP / "tools/dev/innkeeper.sh"), "app", *args],
    check=True,
    capture_output=True,
    text=True,
    timeout=30,
  ).stdout


def granted():
  info = json.loads(innkeeper("info", package, "--json"))
  return next(
    p["granted"] for p in info["permissions"] if p["name"] == permission
  )


data = Path(os.environ.get("INNKEEPER_DATA", ""))
if not str(data) or data == Path("."):
  data = (
    TOP / "out/dev/flatpak" / os.environ["INNKEEPER_PROFILE"] / "android-data"
  )
cache = data / "data" / package / "cache/session-api-probe"
files = data / "data" / package / "files/session-api-probe"
innkeeper("set", package, "--permission", f"{permission}=off")
assert not granted()
innkeeper("clear-cache", package)
assert not cache.exists(), "clear-cache left the probe's temporary file"
assert files.read_text() == "persistent", "clear-cache removed app data"
# The fresh install grants this permission, and the notification scenario
# needs it back.
innkeeper("set", package, "--permission", f"{permission}=on")
print(
  "ok   innkeeper app clears cache, preserves data and toggles a runtime permission"
)
