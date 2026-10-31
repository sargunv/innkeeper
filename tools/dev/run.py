#!/usr/bin/python3
"""Run the stage in a private Flatpak; all systemd access stays on the host."""

import array
import configparser
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import socket
import sys
import time

TOP = Path(__file__).resolve().parents[2]
APP = "dev.sargunv.Innkeeper"
STAGE = TOP / "out/stage"
BASE = TOP / "out/dev"
ENV = os.environ.copy()
ENV["PATH"] = os.pathsep.join(
  p
  for p in ENV.get("PATH", "/usr/bin:/bin").split(os.pathsep)
  if not p.endswith("/mise/shims")
)
PROFILE = ENV.get("INNKEEPER_PROFILE", "dev")
if not re.fullmatch(r"[a-z0-9-]+", PROFILE):
  sys.exit("dev runner requires a named INNKEEPER_PROFILE")
ENV["INNKEEPER_PROFILE"] = PROFILE
# The dev runner shares the user's real session bus, so isolate it unless a
# nested desktop set a private XDG_RUNTIME_DIR. INNKEEPER_TEST=0 opts out.
if (
  ENV.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
  == f"/run/user/{os.getuid()}"
):
  ENV.setdefault("INNKEEPER_TEST", "1")
ENV.setdefault(
  "INNKEEPER_IMAGES", str(Path.home() / ".local/share/innkeeper/images")
)
ENV.setdefault(
  "INNKEEPER_DATA", str(BASE / "flatpak" / PROFILE / "android-data")
)
SCRATCH = BASE / "flatpak" / PROFILE
SCRATCH.mkdir(parents=True, exist_ok=True)
XDG = ("XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME")
# Entries, Flatpak state and notices never go to the real XDG dirs: only ones
# under out/ (a nested test desktop's) are kept.
for key in XDG:
  if not ENV.get(key, "").startswith(str(TOP / "out") + "/"):
    ENV[key] = str(SCRATCH / key.split("_")[1].lower())
  Path(ENV[key]).mkdir(parents=True, exist_ok=True)
INSTALL = TOP / "out/flatpak/user"
ENV["FLATPAK_USER_DIR"] = str(INSTALL)
ENV["HOME"] = str(SCRATCH / "home")
Path(ENV["HOME"]).mkdir(parents=True, exist_ok=True)
HOST_BUS = f"unix:path=/run/user/{os.getuid()}/bus"
# Every scope Flatpak makes for the app gets these limits, including the ones
# the portal starts for alarm wakeups: systemd applies a unit-name-prefix
# drop-in to each new app-flatpak-dev.sargunv.Innkeeper-<n>.scope.
DROPIN = Path(
  f"/run/user/{os.getuid()}/systemd/user/app-flatpak-{APP}-.scope.d/50-innkeeper-dev-memory.conf"
)
MEMORY = "[Scope]\nMemoryHigh=6G\nMemoryMax=8G\n"
MEMORY_MAX = str(8 << 30)
# Room for one capped session plus the desktop.
MIN_AVAILABLE_GIB = 10
SYSTEMD_ENV = dict(
  os.environ,
  DBUS_SESSION_BUS_ADDRESS=HOST_BUS,
  XDG_RUNTIME_DIR=f"/run/user/{os.getuid()}",
)


def run(argv, **kwargs):
  return subprocess.run(argv, env=ENV, **kwargs)


def bootstrap():
  if (
    run(
      ["flatpak", "info", "--user", APP],
      stdout=subprocess.DEVNULL,
      stderr=subprocess.DEVNULL,
    ).returncode
    == 0
  ):
    return
  subprocess.run(
    [str(TOP / "packaging/flatpak/dev-init.sh")], env=ENV, check=True
  )


def command(program, args):
  argv = [
    "flatpak",
    "run",
    "--user",
    "--no-documents-portal",
    f"--app-path={STAGE}",
  ]
  # The manifest's current permissions, which dev-init.sh's snapshot may lack.
  manifest = (TOP / "packaging/flatpak/dev.sargunv.Innkeeper.yml").read_text()
  finish = manifest.split("finish-args:\n", 1)[1].split("modules:", 1)[0]
  argv += re.findall(r"^  - (--\S+)$", finish, re.MULTILINE)
  argv.append(f"--filesystem={STAGE}:ro")
  # Exact scratch paths and the image cache only; never --filesystem=home/host.
  paths = {
    str(SCRATCH),
    str(Path(ENV["INNKEEPER_DATA"]).parent),
    ENV["INNKEEPER_IMAGES"],
  }
  paths.update(ENV[k] for k in XDG)
  # Flatpak skips a --filesystem path that doesn't exist, so the image
  # download would land in the sandbox's RAM-backed tmpfs.
  for p in paths:
    Path(p).mkdir(parents=True, exist_ok=True)
  if program == "innkeeper":
    for arg in args:
      p = Path(arg)
      if p.is_absolute() and p.is_file():
        paths.add(str(p))
  for p in sorted(paths):
    if not any(
      p != parent and Path(p).is_relative_to(parent) for parent in paths
    ):
      argv.append(f"--filesystem={p}")
  inner_env = []
  # Flatpak sets XDG_*_HOME over any --env; env(1) sets them after it.
  for k, v in ENV.items():
    if (
      k.startswith("INNKEEPER_")
      or k in XDG
      or k
      in (
        "LANG",
        "LANGUAGE",
        "LC_ALL",
        "LC_MESSAGES",
        "LC_TIME",
        "LC_MEASUREMENT",
        "XDG_CURRENT_DESKTOP",
        "DBUS_SYSTEM_BUS_ADDRESS",
        "WAYLAND_DEBUG",
      )
    ):
      inner_env.append(f"{k}={v}")
  target = (
    str(STAGE / "bin/innkeeper")
    if program == "innkeeper"
    else str(STAGE / "libexec/innkeeper/innkeeper-session")
  )
  # The CLI is told its profile; the launcher reads INNKEEPER_PROFILE.
  if program == "innkeeper":
    args = ["--profile", PROFILE, *args]
  argv += ["--command=env", APP, *inner_env, target, *args]
  return argv


def connect():
  runtime = ENV.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
  path = Path(runtime) / "app" / APP / f"innkeeper-{PROFILE}/control.sock"
  sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
  sock.settimeout(120)
  try:
    sock.connect(str(path))
  except OSError:
    sock.close()
    raise
  return sock


def running():
  try:
    with connect() as sock:
      sock.send(b"status")
      return sock.recv(4096).startswith(b"running:")
  except OSError:
    return False


def control(args):
  try:
    sock = connect()
  except OSError:
    if args[0] == "stop":
      return 0
    if args[0] == "status":
      print("stopped")
      return 1
    sys.exit("the session is not running")
  with sock:
    if args[0] in ("exec", "shell"):
      words = args if args[0] == "exec" else ["exec", "/system/bin/sh"]
      sock.sendmsg(
        [b"\0".join(os.fsencode(a) for a in words)],
        [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", [0, 1, 2]))],
      )
      return int(sock.recv(4096).strip())
    sock.send(args[0].encode())
    if args[0] == "stop":
      while sock.recv(4096):
        pass
      return 0
    text = sock.recv(4096).decode()
    # Sandbox pids repeat on reboot. Use the host scope's process as the
    # harness's session identity so compat detects a replaced sandbox.
    cg = session_cgroup()
    if text.startswith("running:") and cg:
      try:
        pid = (cg / "cgroup.procs").read_text().split()[0]
        text = re.sub(r"host pid \d+", f"host pid {pid}", text)
      except (OSError, IndexError):
        pass
    print(text, end="")
    return 0 if text.startswith("running:") else 1


def available():
  info = Path("/proc/meminfo").read_text().splitlines()
  kib = int(next(l for l in info if l.startswith("MemAvailable:")).split()[1])
  if kib < MIN_AVAILABLE_GIB << 20:
    sys.exit(
      f"dev runner: {kib >> 20} GiB available, a session needs"
      f" {MIN_AVAILABLE_GIB}; stop another session first"
    )


def cap():
  """Install the memory drop-in. systemd finds a new drop-in only on reload."""
  if DROPIN.is_file() and DROPIN.read_text() == MEMORY:
    return
  DROPIN.parent.mkdir(parents=True, exist_ok=True)
  DROPIN.write_text(MEMORY)
  subprocess.run(
    ["systemctl", "--user", "daemon-reload"], env=SYSTEMD_ENV, check=True
  )


def session_cgroup():
  """The cgroup of this profile's session sandbox, wherever it was started:
  by `start`, by the CLI through the portal, or by an alarm wakeup. A portal
  session wins over a `start -f` sandbox left waiting for the alarm."""
  found = []
  runtime = Path(ENV.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
  for info in runtime.glob(".flatpak/*/info"):
    try:
      cfg = configparser.ConfigParser(interpolation=None)
      cfg.read(info)
      if cfg.get("Application", "name", fallback="") != APP:
        continue
      home = cfg.get("Instance", "instance-path", fallback="")
      if home != str(SCRATCH / "home/.var/app" / APP):
        continue
      pid = (info.parent / "pid").read_text().strip()
      argv = Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
      # `start -f` from start(), or the launcher's `_serve` for a session
      # the portal spawned.
      if b"_serve" not in argv and not (b"start" in argv and b"-f" in argv):
        continue
      cg = Path(f"/proc/{pid}/cgroup").read_text().strip().split("::", 1)[1]
      found.append((b"_serve" not in argv, Path("/sys/fs/cgroup" + cg)))
    except (OSError, IndexError, configparser.Error):
      continue
  return min(found)[1] if found else None


def capped(cg):
  """Whether the scope of cgroup cg has the drop-in's MemoryMax."""
  o = subprocess.run(
    ["systemctl", "--user", "show", "-P", "MemoryMax", cg.name],
    env=SYSTEMD_ENV,
    capture_output=True,
    text=True,
  )
  return o.stdout.strip() == MEMORY_MAX


def start(args):
  # Serialize starts without holding the lock for the lifetime of the session.
  with (SCRATCH / "start.lock").open("w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    if running():
      return 0
    available()
    cap()
    # flatpak run's client gets a scope of its own, so the session outlives
    # the caller's unit (a terminal, a test desktop). Flatpak moves the
    # sandbox itself to its app scope.
    restore = [
      f"{k}={ENV[k]}"
      for k in ("DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR")
      if k in ENV
    ]
    with (SCRATCH / "runner.log").open("ab") as log:
      child = subprocess.Popen(
        ["systemd-run", "--user", "--scope", "--quiet", "--collect"]
        + ["-p", "MemoryMax=256M", "env", *restore]
        + command("session", ["start", "-f", *args]),
        env=dict(
          ENV,
          **{
            k: SYSTEMD_ENV[k]
            for k in ("DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR")
          },
        ),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        start_new_session=True,
      )
    for _ in range(600):
      if running():
        cg = session_cgroup()
        if cg is None or not capped(cg):
          control(["stop"])
          sys.exit(f"dev runner: session scope {cg} is not memory capped")
        return 0
      if child.poll() is not None:
        sys.exit(f"session failed; see {SCRATCH}/runner.log")
      time.sleep(0.1)
    sys.exit(f"session did not start; see {SCRATCH}/runner.log")


def main():
  args = sys.argv[1:]
  program = "session"
  if args[:1] == ["--cli"]:
    program = "innkeeper"
    args = args[1:]
  if args[:1] == ["paths"]:
    print(
      json.dumps(
        {
          "state": str(Path(ENV["XDG_STATE_HOME"]) / f"innkeeper-{PROFILE}"),
          "data_home": ENV["XDG_DATA_HOME"],
        }
      )
    )
    return 0
  if program == "session" and args[:1] == ["wait"]:
    available()
    return 0
  if program == "session" and args[:1] == ["cgroup"]:
    cg = session_cgroup()
    if cg is None:
      return 1
    print(cg)
    return 0
  if program == "session" and args[:1] in (
    ["status"],
    ["stop"],
    ["exec"],
    ["shell"],
  ):
    return control(args)
  if program == "session" and args[:1] == ["image"]:
    # This only reads the pin and paths; the launcher allows it outside
    # Flatpak. Core checks it before every start, so avoid a second sandbox.
    return run(
      [str(STAGE / "libexec/innkeeper/innkeeper-session"), *args]
    ).returncode
  bootstrap()
  if program == "session" and args[:1] == ["start"]:
    return start([a for a in args[1:] if a != "-f"])
  if program == "innkeeper":
    if ENV.get("DBUS_SESSION_BUS_ADDRESS", HOST_BUS) == HOST_BUS:
      sys.exit(
        "CLI integration tests require tests/nested.sh (a private desktop bus)"
      )
  if program == "innkeeper" and not running():
    available()
  # The CLI and other commands can start the session through the portal.
  cap()
  return run(command(program, args)).returncode


if __name__ == "__main__":
  try:
    sys.exit(main())
  except (RuntimeError, subprocess.CalledProcessError) as error:
    sys.exit(str(error))
