#!/usr/bin/python3
"""Checks Android apps on the AT-SPI bus, as a screen reader sees them.

Finds the window labelled LABEL (argv[1]) under the bridge's AT-SPI
application, then checks the probes' A11yProbeActivity in it: the roles of
its widgets, that doAction(0) on the button changes the counter's text, that
the checkbox toggles, and that focusing the text field fires a focus event.
Prints ok/FAIL lines; exits 1 if any check failed.
"""

import sys
import time

import pyatspi
from gi.repository import GLib

LABEL = sys.argv[1]
TIMEOUT = 60
failed = False


def report(ok, what):
  global failed
  print(("ok   " if ok else "FAIL ") + what, flush=True)
  failed |= not ok


def walk(node, out, depth=0):
  if node is None or depth > 40:
    return out
  out.append(node)
  try:
    for i in range(node.childCount):
      walk(node.getChildAtIndex(i), out, depth + 1)
  except Exception:  # noqa: BLE001 - a node can go away mid-walk
    pass
  return out


def find_window():
  desktop = pyatspi.Registry.getDesktop(0)
  for i in range(desktop.childCount):
    app = desktop.getChildAtIndex(i)
    if app is None:
      continue
    try:
      for j in range(app.childCount):
        w = app.getChildAtIndex(j)
        if w is not None and w.name == LABEL:
          return app, w
    except Exception:  # noqa: BLE001
      continue
  return None, None


def wait_for(what, pred, timeout=TIMEOUT):
  deadline = time.time() + timeout
  while time.time() < deadline:
    v = pred()
    if v:
      return v
    time.sleep(0.5)
  return None


def nodes():
  _, w = find_window()
  return walk(w, []) if w else []


def named(name, roles=None):
  for n in nodes():
    try:
      if n.name == name and (roles is None or n.getRole() in roles):
        return n
    except Exception:  # noqa: BLE001
      pass
  return None


found = wait_for("window", lambda: find_window()[1])
report(found is not None, f"window {LABEL!r} on the AT-SPI bus")
if not found:
  sys.exit(1)
app, window = find_window()
print(
  f"     application {app.name!r}, toolkit {app.toolkitName!r}, "
  f"{len(nodes())} accessibles",
  flush=True,
)
report(window.getRole() == pyatspi.ROLE_FRAME, "the window is a frame")

button = wait_for(
  "button", lambda: named("Increment", [pyatspi.ROLE_PUSH_BUTTON])
)
report(button is not None, "push button 'Increment'")
check = named("Remember me", [pyatspi.ROLE_CHECK_BOX])
report(check is not None, "check box 'Remember me'")
entry = named("Name", [pyatspi.ROLE_ENTRY, pyatspi.ROLE_TEXT])
report(entry is not None, "text field 'Name'")
item = named("Banana", [pyatspi.ROLE_LIST_ITEM])
report(item is not None, "list item 'Banana'")
report(named("Count 0") is not None, "label 'Count 0'")

if button is not None:
  button.queryAction().doAction(0)
  report(
    wait_for("count", lambda: named("Count 1"), 15) is not None,
    "doAction(0) on 'Increment' -> 'Count 1'",
  )

if check is not None:
  check.queryAction().doAction(0)
  report(
    wait_for(
      "checked",
      lambda: (
        (c := named("Remember me")) is not None
        and c.getState().contains(pyatspi.STATE_CHECKED)
      ),
      15,
    )
    is not None,
    "doAction(0) on 'Remember me' -> checked",
  )

if entry is not None:
  events = []

  def on_focus(e):
    if e.detail1 == 1:
      try:
        events.append((e.source.name, e.source.getRole()))
      except Exception:  # noqa: BLE001
        events.append((None, None))

  pyatspi.Registry.registerEventListener(
    on_focus, "object:state-changed:focused"
  )
  entry.queryComponent().grabFocus()
  GLib.timeout_add(8000, pyatspi.Registry.stop)
  pyatspi.Registry.start()
  print(f"     focus events: {events}", flush=True)
  report(
    ("Name", entry.getRole()) in events,
    "grabFocus on 'Name' -> a focus event for it",
  )

sys.exit(1 if failed else 0)
