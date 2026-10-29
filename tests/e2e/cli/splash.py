#!/usr/bin/python3
"""Time a cold launch through the first Wayland commit with a buffer attached."""

import os
from pathlib import Path
import re
import subprocess
import sys
import time

output = Path(sys.argv[1])
start = time.monotonic()
child = subprocess.Popen(
  sys.argv[2:],
  env=dict(os.environ, WAYLAND_DEBUG="client"),
  stdout=subprocess.PIPE,
  stderr=subprocess.STDOUT,
  text=True,
)
attached = False
shown = None
with output.open("w") as log:
  for line in child.stdout:
    log.write(line)
    if re.search(r"wl_surface[#@]\d+\.attach\(wl_buffer", line):
      attached = True
    if (
      attached
      and shown is None
      and re.search(r"wl_surface[#@]\d+\.commit\(", line)
    ):
      shown = (time.monotonic() - start) * 1000
code = child.wait()
print(
  f"splash first buffer commit: {shown:.1f} ms"
  if shown is not None
  else "FAIL splash did not attach a buffer"
)
sys.exit(0 if code == 0 and shown is not None and shown <= 200 else 1)
