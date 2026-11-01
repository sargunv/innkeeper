#!/usr/bin/python3
"""Send input to the disposable compositor's seat.

The events are abs:x,y, rel:x,y, button:code,state, key:code,state and
axis:axis,value. They come from argv, or from lines on stdin with --stdin.
"""

import sys

from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
actions = sys.stdin if sys.argv[1:] == ["--stdin"] else sys.argv[1:]
for action in actions:
  bus.call_sync(
    "dev.sargunv.Innkeeper.TestDesktop",
    "/dev/sargunv/Innkeeper/TestDesktop",
    "dev.sargunv.Innkeeper.TestDesktop",
    "Input",
    GLib.Variant("(s)", (action.strip(),)),
    None,
    Gio.DBusCallFlags.NONE,
    5000,
    None,
  )
