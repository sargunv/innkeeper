#!/usr/bin/python3
"""Boot the OpenHarmony image in a private, memory-capped Flatpak sandbox.

  ohos/run.py [--timeout S] [--until REGEX] [--probe SCRIPT] [--display]

The sandbox is the dev runner's (tools/dev/run.py): the stage at /app, the
same permissions, the same named-profile scratch dirs, and the same
MemoryHigh/MemoryMax on its scope. ohos/sandbox.sh builds OH's root there and
runs its init under the supervisor. Init's log (/dev/kmsg) goes to
<scratch>/ohos-kmsg.log. The run ends at --timeout, or once a log line
matches --until (exit 0); a timeout with --until unmatched exits 1.

--display also runs the bridge, so OH's display shows in a desktop window
(log: <scratch>/ohos-bridge.log). It opens on $WAYLAND_DISPLAY: use a nested
desktop (tests/nested.sh), never the user's own.

--probe runs SCRIPT with OH's /system/bin/sh as root inside the session once
samgr is ready (ohos/sys_prod/etc/init/linuxapk_probe.cfg), prints its
output, and exits with its status.
"""

import argparse
import os
from pathlib import Path
import re
import subprocess
import sys
import time

TOP = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOP / "tools/dev"))
sys.dont_write_bytecode = True
os.environ.setdefault("LINUXAPK_PROFILE", "ohos")
import run as dev  # noqa: E402

IMAGES = Path(os.environ.get("LINUXAPK_OHOS_IMAGES", TOP / "out/ohos/images"))
LAYER = TOP / "out/build/ohos/layer"


def pin_name():
  import tomllib

  with (TOP / "image/ohos/pin.toml").open("rb") as f:
    return tomllib.load(f)["name"]


def argv(gate, instance_fd, img, data, log, bridge):
  manifest = (TOP / "packaging/flatpak/dev.linuxapk.Manager.yml").read_text()
  finish = manifest.split("finish-args:\n", 1)[1].split("modules:", 1)[0]
  a = ["flatpak", "run", "--user", "--no-documents-portal"]
  a.append(f"--app-path={dev.STAGE}")
  a += re.findall(r"^  - (--\S+)$", finish, re.MULTILINE)
  for p in (dev.STAGE, TOP / "ohos", LAYER, img):
    a.append(f"--filesystem={p}:ro")
  a.append(f"--filesystem={dev.SCRATCH}")
  a += [
    "--command=sh",
    f"--instance-id-fd={instance_fd}",
    dev.APP,
    "-c",
    'while ! test -f "$1"; do sleep 0.05; done; shift; exec "$@"',
    "sh",
    str(gate),
    "env",
    f"LINUXAPK_OHOS_KMSG={log}",
    *([f"LINUXAPK_OHOS_BRIDGE={bridge}"] if bridge else []),
    *(
      f"{k}={v}" for k, v in os.environ.items() if k == "LINUXAPK_BROKER_DEBUG"
    ),
    "bash",
    str(TOP / "ohos/sandbox.sh"),
    str(img),
    str(LAYER),
    str(data),
    "/app/libexec/linuxapk/pivot-exec",
  ]
  return a


def main():
  ap = argparse.ArgumentParser()
  ap.add_argument("--timeout", type=float, default=60)
  ap.add_argument("--until")
  ap.add_argument("--probe", type=Path)
  ap.add_argument("--keep-data", action="store_true")
  ap.add_argument("--display", action="store_true")
  o = ap.parse_args()
  img = IMAGES / pin_name() / "img"
  if not (img / "system/system/bin/init").exists():
    sys.exit(f"no image at {img}; run image/ohos/fetch.sh first")
  if not (LAYER / "bin/linuxapk-broker").exists():
    sys.exit(f"no {LAYER}; run mise run ohos:build first")
  data = dev.SCRATCH / "ohos-data"
  if not o.keep_data:
    subprocess.run(["rm", "-rf", str(data)], check=True)
  probe = data / "linuxapk"
  probe.mkdir(parents=True, exist_ok=True)
  for f in ("probe.sh", "probe.out", "probe.done"):
    (probe / f).unlink(missing_ok=True)
  if o.probe:
    (probe / "probe.sh").write_bytes(o.probe.read_bytes())
  log = dev.SCRATCH / "ohos-kmsg.log"
  log.write_bytes(b"")
  bridge = dev.SCRATCH / "ohos-bridge.log" if o.display else None
  if bridge:
    bridge.write_bytes(b"")
  gate = dev.SCRATCH / f"capped-{os.getpid()}"
  gate.unlink(missing_ok=True)
  dev.available()
  dev.bootstrap()
  rfd, wfd = os.pipe()
  with (dev.SCRATCH / "ohos-run.log").open("wb") as out:
    child = subprocess.Popen(
      argv(gate, wfd, img, data, log, bridge),
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
    for _ in range(100):
      if instance and dev.scope(instance, record=False):
        break
      time.sleep(0.05)
    else:
      raise RuntimeError("Flatpak scope not found; refusing to run uncapped")
    gate.touch()
    pattern = re.compile(o.until) if o.until else None
    end = time.monotonic() + o.timeout
    while time.monotonic() < end and child.poll() is None:
      if pattern and pattern.search(log.read_text(errors="replace")):
        return 0
      done = probe / "probe.done"
      if o.probe and done.exists() and done.read_text().strip():
        sys.stdout.write((probe / "probe.out").read_text(errors="replace"))
        return int(done.read_text())
      time.sleep(0.2)
    if child.poll() is not None:
      print(f"sandbox exited: {child.returncode}", file=sys.stderr)
    elif o.probe:
      print("probe did not finish", file=sys.stderr)
    return 1 if pattern or o.probe else 0
  finally:
    if instance:
      dev.run(["flatpak", "kill", instance], check=False)
    if child.poll() is None:
      child.terminate()
    child.wait()
    gate.unlink(missing_ok=True)
    print(f"log: {log}", file=sys.stderr)


if __name__ == "__main__":
  try:
    sys.exit(main())
  except (RuntimeError, subprocess.CalledProcessError) as error:
    sys.exit(str(error))
