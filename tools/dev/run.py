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
APP = "dev.linuxapk.Manager"
STAGE = TOP / "out/stage"
BASE = TOP / "out/dev"
ENV = os.environ.copy()
ENV["PATH"] = os.pathsep.join(
  p
  for p in ENV.get("PATH", "/usr/bin:/bin").split(os.pathsep)
  if not p.endswith("/mise/shims")
)
MODE = ENV.get("LINUXAPK_MODE", "flatpak")
INSTANCE = ENV.get("LINUXAPK_INSTANCE", "dev")
if not re.fullmatch(r"[a-z0-9-]+", INSTANCE):
  sys.exit("dev runner requires a named LINUXAPK_INSTANCE")
ENV["LINUXAPK_INSTANCE"] = INSTANCE
ENV.setdefault(
  "LINUXAPK_IMAGES", str(Path.home() / ".local/share/linuxapk/images")
)
ENV.setdefault("LINUXAPK_DATA", str(BASE / MODE / INSTANCE / "android-data"))
SCRATCH = BASE / MODE / INSTANCE
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


def command(program, args, gate=None, instance_fd=None):
  argv = [
    "flatpak",
    "run",
    "--user",
    "--no-documents-portal",
    f"--app-path={STAGE}",
  ]
  # The manifest's current permissions, which dev-init.sh's snapshot may lack.
  manifest = (TOP / "packaging/flatpak/dev.linuxapk.Manager.yml").read_text()
  finish = manifest.split("finish-args:\n", 1)[1].split("modules:", 1)[0]
  argv += re.findall(r"^  - (--\S+)$", finish, re.MULTILINE)
  argv.append(f"--filesystem={STAGE}:ro")
  # Exact scratch paths and the image cache only; never --filesystem=home/host.
  paths = {
    str(SCRATCH),
    str(Path(ENV["LINUXAPK_DATA"]).parent),
    ENV["LINUXAPK_IMAGES"],
  }
  paths.update(ENV[k] for k in XDG)
  if program == "linuxapk":
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
      k.startswith("LINUXAPK_")
      or k in XDG
      or k
      in (
        "PULSE_SERVER",
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
    str(STAGE / "bin/linuxapk")
    if program == "linuxapk"
    else str(STAGE / "libexec/linuxapk/linuxapk-session")
  )
  if gate:
    argv += [
      "--command=sh",
      f"--instance-id-fd={instance_fd}",
      APP,
      "-c",
      'while ! test -f "$1"; do sleep 0.05; done; shift; exec "$@"',
      "sh",
      str(gate),
      "env",
      *inner_env,
      target,
      *args,
    ]
  else:
    argv += ["--command=env", APP, *inner_env, target, *args]
  return argv


def connect():
  runtime = ENV.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")
  path = Path(runtime) / "app" / APP / f"linuxapk-{INSTANCE}/control.sock"
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
      print("stopped\nuid-mode=single")
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
    if text.startswith("running:"):
      try:
        cg = Path((SCRATCH / "cgroup").read_text())
        pid = (cg / "cgroup.procs").read_text().split()[0]
        text = re.sub(r"host pid \d+", f"host pid {pid}", text)
      except (OSError, IndexError):
        pass
    print(text, end="")
    return 0 if text.startswith("running:") else 1


def available():
  minimum = int(ENV.get("LINUXAPK_MIN_AVAILABLE_GIB", "25"))
  subprocess.run(["free", "-g"], check=True)
  while True:
    fields = Path("/proc/meminfo").read_text().splitlines()
    kib = int(
      next(l for l in fields if l.startswith("MemAvailable:")).split()[1]
    )
    if kib >= minimum * 1024 * 1024:
      return
    print(
      f"dev runner: less than {minimum} GiB available; waiting five minutes",
      file=sys.stderr,
      flush=True,
    )
    time.sleep(300)


def scope(instance, record=True):
  o = run(
    ["flatpak", "ps", "--columns=instance,pid"],
    capture_output=True,
    text=True,
    check=True,
  )
  for line in o.stdout.splitlines():
    cols = line.split()
    if len(cols) >= 2 and cols[0] == instance and cols[1] != "0":
      try:
        cg = (
          Path(f"/proc/{cols[1]}/cgroup").read_text().strip().split("::", 1)[1]
        )
      except OSError:
        continue
      unit = next(
        (
          p
          for p in cg.split("/")
          if p.startswith("app-flatpak-") and p.endswith(".scope")
        ),
        None,
      )
      if unit is None:
        return False
      subprocess.run(
        [
          "systemctl",
          "--user",
          "set-property",
          "--runtime",
          unit,
          "MemoryHigh=6G",
          "MemoryMax=8G",
        ],
        env=SYSTEMD_ENV,
        check=True,
      )
      if record:
        (SCRATCH / "cgroup").write_text("/sys/fs/cgroup" + cg)
      return True
  return False


def watch_scopes(capped):
  """An alarm's replacement sandbox is created by the Flatpak portal."""
  runtime = Path(ENV.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
  found = False
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
      if b"_serve" not in argv and not (b"start" in argv and b"-f" in argv):
        continue
      found = True
      if info.parent.name not in capped:
        try:
          if scope(info.parent.name):
            capped.add(info.parent.name)
        except subprocess.CalledProcessError:
          # A test replacement must not remain alive with no memory bound.
          run(["flatpak", "kill", info.parent.name], check=False)
          raise
    except (OSError, configparser.Error, ProcessLookupError):
      continue
  return found


def serve(args):
  ended = SCRATCH / "ended"
  gate = SCRATCH / "capped"
  gate.unlink(missing_ok=True)
  rfd, wfd = os.pipe()
  child = None
  try:
    with (SCRATCH / "runner.log").open("ab") as log:
      child = subprocess.Popen(
        command("session", ["start", "-f", *args], gate, wfd),
        env=ENV,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        pass_fds=(wfd,),
      )
    os.close(wfd)
    with os.fdopen(rfd) as pipe:
      instance = pipe.readline().strip()
    if not instance:
      raise RuntimeError(
        "Flatpak did not publish an instance id (see runner.log)"
      )
    for _ in range(100):
      if scope(instance):
        break
      time.sleep(0.05)
    else:
      raise RuntimeError("Flatpak scope not found; refusing to run uncapped")
    gate.touch()
    capped = {instance}
    gone = 0
    while gone < 10:
      found = watch_scopes(capped)
      gone = 0 if child.poll() is None or found else gone + 1
      time.sleep(0.1)
  finally:
    if child is not None and child.poll() is None:
      child.terminate()
      child.wait()
    gate.unlink(missing_ok=True)
    ended.touch()


def supervise(action, args=()):
  # Python's monitor is capped too; Flatpak moves its app to a separate scope.
  restore = [f"LINUXAPK_INSTANCE={INSTANCE}"]
  for key in ("DBUS_SESSION_BUS_ADDRESS", "XDG_RUNTIME_DIR"):
    if key in os.environ:
      restore.append(f"{key}={os.environ[key]}")
  with (SCRATCH / "supervisor.log").open("ab") as log:
    subprocess.Popen(
      [
        "systemd-run",
        "--user",
        "--scope",
        "--quiet",
        "-p",
        "MemoryMax=256M",
        "env",
        *restore,
        sys.executable,
        str(Path(__file__).resolve()),
        action,
        *args,
      ],
      env=SYSTEMD_ENV,
      stdin=subprocess.DEVNULL,
      stdout=log,
      stderr=log,
      start_new_session=True,
    )


def run_capped(program, args):
  if program == "linuxapk":
    if not running():
      available()
    supervise("_watch")
  gate = SCRATCH / f"capped-{os.getpid()}"
  gate.unlink(missing_ok=True)
  rfd, wfd = os.pipe()
  child = subprocess.Popen(
    command(program, args, gate, wfd), env=ENV, pass_fds=(wfd,)
  )
  os.close(wfd)
  try:
    with os.fdopen(rfd) as pipe:
      instance = pipe.readline().strip()
    for _ in range(100):
      if scope(instance, record=False):
        break
      time.sleep(0.05)
    else:
      raise RuntimeError(
        "Flatpak command scope not found; refusing to run uncapped"
      )
    gate.touch()
    return child.wait()
  finally:
    if child.poll() is None:
      child.terminate()
      child.wait()
    gate.unlink(missing_ok=True)


def start(args):
  # Serialize starts without holding the lock for the lifetime of the session.
  with (SCRATCH / "start.lock").open("w") as lock:
    fcntl.flock(lock, fcntl.LOCK_EX)
    if running():
      return 0
    available()
    (SCRATCH / "ended").unlink(missing_ok=True)
    supervise("_serve", args)
    for _ in range(600):
      if running():
        return 0
      if (SCRATCH / "ended").exists():
        sys.exit(f"session failed; see {SCRATCH}/runner.log and supervisor.log")
      time.sleep(0.1)
    sys.exit(
      f"session did not start; see {SCRATCH}/runner.log and supervisor.log"
    )


def main():
  args = sys.argv[1:]
  program = "session"
  if args[:1] == ["--cli"]:
    program = "linuxapk"
    args = args[1:]
  if args[:1] == ["paths"]:
    print(
      json.dumps(
        {
          "state": str(Path(ENV["XDG_STATE_HOME"]) / f"linuxapk-{INSTANCE}"),
          "data_home": ENV["XDG_DATA_HOME"],
        }
      )
    )
    return 0
  if program == "session" and args[:1] == ["wait"]:
    available()
    return 0
  if MODE == "host":
    if args[:1] == ["start"] or program == "linuxapk":
      target = STAGE / "libexec/linuxapk/linuxapk-session"
      active = run(
        [str(target), "status"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
      )
      if active.returncode != 0:
        available()
    if args[:1] == ["cgroup"]:
      o = subprocess.check_output(
        [
          "systemctl",
          "--user",
          "show",
          "-P",
          "ControlGroup",
          f"linuxapk-session-{INSTANCE}",
        ],
        env=SYSTEMD_ENV,
        text=True,
      )
      print("/sys/fs/cgroup" + o.strip())
      return 0
    target = STAGE / (
      "bin/linuxapk"
      if program == "linuxapk"
      else "libexec/linuxapk/linuxapk-session"
    )
    os.execve(target, [str(target), *args], ENV)
  if MODE != "flatpak":
    sys.exit("LINUXAPK_MODE must be flatpak or host")
  if program == "session" and args[:1] in (
    ["status"],
    ["stop"],
    ["exec"],
    ["shell"],
  ):
    return control(args)
  bootstrap()
  if args[:1] == ["_watch"]:
    with (SCRATCH / "watch.lock").open("w") as lock:
      try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
      except BlockingIOError:
        return 0
      capped = set()
      gone = 0
      while gone < 100:
        # A first CLI install may spend minutes preparing the image before
        # it spawns Android. Its gated-command marker lives until it exits.
        caller_active = any(
          Path(f"/proc/{p.name.removeprefix('capped-')}").exists()
          for p in SCRATCH.glob("capped-*")
        )
        gone = 0 if watch_scopes(capped) or caller_active else gone + 1
        time.sleep(0.1)
    return 0
  if args[:1] == ["_serve"]:
    serve(args[1:])
    return 0
  if program == "session" and args[:1] == ["start"]:
    return start([a for a in args[1:] if a != "-f"])
  if program == "session" and args[:1] == ["cgroup"]:
    print((SCRATCH / "cgroup").read_text())
    return 0
  if program == "linuxapk":
    if ENV.get("DBUS_SESSION_BUS_ADDRESS", HOST_BUS) == HOST_BUS:
      sys.exit(
        "CLI integration tests require tests/nested.sh (a private desktop bus)"
      )
  return run_capped(program, args)


if __name__ == "__main__":
  try:
    sys.exit(main())
  except (RuntimeError, subprocess.CalledProcessError) as error:
    sys.exit(str(error))
