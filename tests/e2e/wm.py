#!/usr/bin/python3
"""The session's window operations over Session1, for the e2e scripts:
wm LAUNCH --es package P [--es component C] [--ez new-window true]
wm LIST
wm CLOSE (--es package P | --ei display N)
Prints the window manager's reply, one line per window for LIST, or
`error: ...` (still exiting 0, as `am broadcast` did: closing a window that
isn't there is routine)."""

import os
import sys

from gi.repository import Gio, GLib

action, args = sys.argv[1], sys.argv[2:]
extras = {args[i + 1]: args[i + 2] for i in range(0, len(args), 3)}
name = "dev.sargunv.Innkeeper.Session.p_" + os.environ[
  "INNKEEPER_PROFILE"
].replace("-", "_")
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)


def call(method, signature, values):
  return bus.call_sync(
    name,
    "/dev/sargunv/Innkeeper/Session",
    "dev.sargunv.Innkeeper.Session1",
    method,
    GLib.Variant(signature, values) if signature else None,
    None,
    Gio.DBusCallFlags.NONE,
    45000,
    None,
  ).unpack()[0]


try:
  if action == "LAUNCH":
    reply = call(
      "Launch",
      "(sssatsbbuu)",
      (
        extras["package"],
        extras.get("component", ""),
        "",
        [],
        "",
        False,
        extras.get("new-window") == "true",
        0,
        0,
      ),
    )
  elif action == "LIST":
    reply = "\n".join(call("Windows", None, None))
  elif action == "CLOSE":
    reply = call(
      "Close",
      "(is)",
      (int(extras.get("display", -1)), extras.get("package", "")),
    )
  else:
    sys.exit(f"wm: unknown action {action}")
except GLib.Error as e:
  reply = f"error: {e.message}"
print(reply)
