#!/usr/bin/python3
"""Window control on the runner's private desktop, never the user's bus."""

import os
from pathlib import Path
import sys

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

assert os.environ.get("LINUXAPK_NESTED") == "1"
assert os.environ["XDG_RUNTIME_DIR"] != f"/run/user/{os.getuid()}"
DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
visible = sys.argv[1] == "restore"
if os.environ.get("LINUXAPK_TEST_COMPOSITOR") == "mutter":
  control = bus.get_object(
    "dev.linuxapk.TestDesktop", "/dev/linuxapk/TestDesktop"
  )
  found = control.Visibility(visible, dbus_interface="dev.linuxapk.TestDesktop")
else:
  # A mapped Android frame can precede the desktop window list. Return an
  # observation so the runner can retry rather than accepting a silent no-op.
  loop = GLib.MainLoop()
  found = False
  name = "dev.linuxapk.TestVisibility"
  path = "/dev/linuxapk/TestVisibility"

  class Result(dbus.service.Object):
    @dbus.service.method(name, in_signature="b", out_signature="")
    def Done(self, matched):
      global found
      found = bool(matched)
      loop.quit()

  owner = dbus.service.BusName(name, bus, do_not_queue=True)
  result = Result(bus, path)
  root = Path(__file__).resolve().parents[2]
  scratch = root / "out/test/e2e" / os.environ["LINUXAPK_PROFILE"]
  scratch.mkdir(parents=True, exist_ok=True)
  script = scratch / "visibility.js"
  script.write_text(
    "let matched = false;\nfor (const w of workspace.windowList()) {\n"
    '  if (String(w.resourceClass).includes("dev.linuxapk.probes")) {\n'
    f"    w.minimized = {str(not visible).lower()};\n"
    + ("    workspace.activeWindow = w;\n" if visible else "")
    + "    matched = true;\n  }\n}\n"
    + f'callDBus("{name}", "{path}", "{name}", "Done", matched);\n'
  )
  scripting = dbus.Interface(
    bus.get_object("org.kde.KWin", "/Scripting"), "org.kde.kwin.Scripting"
  )
  ident = scripting.loadScript(
    str(script), "linuxapk-window-visibility", signature="ss"
  )
  try:
    dbus.Interface(
      bus.get_object("org.kde.KWin", f"/Scripting/Script{ident}"),
      "org.kde.kwin.Script",
    ).run()
    GLib.timeout_add(1000, lambda: loop.quit())
    loop.run()
  finally:
    scripting.unloadScript("linuxapk-window-visibility")
sys.exit(0 if found else 1)
