#!/usr/bin/python3
"""Boot the OpenHarmony image in a private, memory-capped Flatpak sandbox.

  ohos/run.py [--timeout S] [--probe SCRIPT] [--display]

The sandbox is the dev runner's (tools/dev/run.py): the stage at /app, the
same permissions, the same named-profile scratch dirs, and the same
MemoryHigh/MemoryMax on its scope. ohos/sandbox.sh builds OH's root there and
runs its init under the supervisor. Init's log (/dev/kmsg) goes to
<scratch>/ohos-kmsg.log. Without --probe the run ends at --timeout (exit 0).

--display also runs the bridge, so OH's display shows in a desktop window
(log: <scratch>/ohos-bridge.log). It opens on $WAYLAND_DISPLAY: use a nested
desktop (tests/nested.sh), never the user's own.

--probe runs SCRIPT with OH's /system/bin/sh as root inside the session once
samgr is ready (ohos/sys_prod/etc/init/innkeeper_probe.cfg), prints its
output, and exits with its status.
"""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import tomllib

TOP = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOP / "tools/dev"))
sys.dont_write_bytecode = True
os.environ.setdefault("INNKEEPER_PROFILE", "ohos")
import run as dev  # noqa: E402

IMAGES = TOP / "out/ohos/images"
LAYER = TOP / "out/build/ohos/layer"


def pin_name():
  with (TOP / "image/ohos/pin.toml").open("rb") as f:
    return tomllib.load(f)["name"]


def argv(instance_fd, img, data, log, bridge):
  manifest = (TOP / "packaging/flatpak/dev.sargunv.Innkeeper.yml").read_text()
  finish = manifest.split("finish-args:\n", 1)[1].split("modules:", 1)[0]
  a = ["flatpak", "run", "--user", "--no-documents-portal"]
  a.append(f"--app-path={dev.STAGE}")
  a += re.findall(r"^  - (--\S+)$", finish, re.MULTILINE)
  for p in (dev.STAGE, TOP / "ohos", LAYER, img):
    a.append(f"--filesystem={p}:ro")
  a.append(f"--filesystem={dev.SCRATCH}")
  a += [
    "--command=env",
    f"--instance-id-fd={instance_fd}",
    dev.APP,
    f"INNKEEPER_OHOS_KMSG={log}",
    *([f"INNKEEPER_OHOS_BRIDGE={bridge}"] if bridge else []),
    *(
      [f"INNKEEPER_BROKER_DEBUG={os.environ['INNKEEPER_BROKER_DEBUG']}"]
      if "INNKEEPER_BROKER_DEBUG" in os.environ
      else []
    ),
    "bash",
    str(TOP / "ohos/sandbox.sh"),
    str(img),
    str(LAYER),
    str(data),
    "/app/libexec/innkeeper/pivot-exec",
  ]
  return a


def main():
  ap = argparse.ArgumentParser()
  ap.add_argument("--timeout", type=float, default=60)
  ap.add_argument("--probe", type=Path)
  ap.add_argument("--display", action="store_true")
  o = ap.parse_args()
  img = IMAGES / pin_name() / "img"
  if not (img / "system/system/bin/init").exists():
    sys.exit(f"no image at {img}; run image/ohos/fetch.sh first")
  if not (LAYER / "bin/innkeeper-brk").exists():
    sys.exit(f"no {LAYER}; run mise run ohos:build first")
  data = dev.SCRATCH / "ohos-data"
  subprocess.run(["rm", "-rf", str(data)], check=True)
  probe = data / "innkeeper"
  probe.mkdir(parents=True)
  if o.probe:
    (probe / "probe.sh").write_bytes(o.probe.read_bytes())
  log = dev.SCRATCH / "ohos-kmsg.log"
  log.write_bytes(b"")
  bridge = dev.SCRATCH / "ohos-bridge.log" if o.display else None
  if bridge:
    bridge.write_bytes(b"")
  dev.available()
  dev.cap()
  dev.bootstrap()
  rfd, wfd = os.pipe()
  with (dev.SCRATCH / "ohos-run.log").open("wb") as out:
    child = subprocess.Popen(
      argv(wfd, img, data, log, bridge),
      env=dev.ENV,
      stdin=subprocess.DEVNULL,
      stdout=out,
      stderr=out,
      pass_fds=(wfd,),
    )
  os.close(wfd)
  instance = ""
  try:
    with os.fdopen(rfd) as pipe:
      instance = pipe.readline().strip()
    if not instance:
      raise RuntimeError("Flatpak did not publish an instance id")
    done = probe / "probe.done"
    end = time.monotonic() + o.timeout
    while time.monotonic() < end and child.poll() is None:
      if o.probe and done.exists() and done.read_text().strip():
        sys.stdout.write((probe / "probe.out").read_text(errors="replace"))
        return int(done.read_text())
      time.sleep(0.2)
    if child.poll() is not None:
      print(f"sandbox exited: {child.returncode}", file=sys.stderr)
    elif o.probe:
      print("probe did not finish", file=sys.stderr)
    return 1 if o.probe else 0
  finally:
    if instance:
      dev.run(["flatpak", "kill", instance], check=False)
    if child.poll() is None:
      child.terminate()
    child.wait()
    print(f"log: {log}", file=sys.stderr)


if __name__ == "__main__":
  try:
    sys.exit(main())
  except (RuntimeError, subprocess.CalledProcessError) as error:
    sys.exit(str(error))
