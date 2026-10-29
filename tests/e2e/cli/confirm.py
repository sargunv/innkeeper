#!/usr/bin/python3
"""Confirm only this suite's launcher dialogs on its disposable desktop."""

import os
import sys
import time

import pyatspi

if (
  os.environ.get("LINUXAPK_NESTED") != "1"
  or os.environ.get("XDG_RUNTIME_DIR") == f"/run/user/{os.getuid()}"
):
  sys.exit("launcher confirmation requires the isolated test desktop")


def walk(node):
  yield node
  for child in node:
    yield from walk(child)


while True:
  try:
    desktop = pyatspi.Registry.getDesktop(0)
    for app in desktop:
      if app.name != "xdg-desktop-portal-gtk":
        continue
      for dialog in app:
        if dialog.name != "Create Application":
          continue
        nodes = list(walk(dialog))
        names = [
          n.queryText().getText(0, -1)
          for n in nodes
          if n.getRole() == pyatspi.ROLE_TEXT
        ]
        if not any(
          n in ("Wikipedia", "Calculator", "OpenCalc", "Stale") for n in names
        ):
          continue
        button = next(
          (
            n
            for n in nodes
            if n.name == "Create" and n.getRole() == pyatspi.ROLE_PUSH_BUTTON
          ),
          None,
        )
        if button is not None:
          button.queryAction().doAction(0)
          print(f"confirmed test launcher: {names}", flush=True)
          break
  except Exception:
    # A dialog can disappear between the snapshot and the action.
    pass
  time.sleep(0.2)
