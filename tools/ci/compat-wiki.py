#!/usr/bin/env python3
"""Render saved nightly reports; never launches an Android session."""

import argparse
import html
import json
from pathlib import Path
import tomllib

REPO = "https://github.com/sargunv/linux.apk"
STATUSES = {
  "works": "Works",
  "partly": "Partly works",
  "broken": "Doesn't work",
  "error": "Not tested (harness error)",
}


def cell(value):
  return (
    html.escape(str(value), quote=False)
    .replace("|", "&#124;")
    .replace("\n", "<br>")
    .replace("\r", "")
  )


def measure(value, unit):
  return "Not measured" if value is None else f"{value} {unit}"


def render(reports, corpus, budgets, commit, run_url):
  source = f"{REPO}/blob/{commit}/tests/compat"
  lines = [
    "# Compatibility",
    "",
    f"Commit: [{commit}]({REPO}/commit/{commit}) · [CI run]({run_url})",
    "",
    f"Apps from the [F-Droid corpus]({source}/corpus.toml), smoke-tested by the "
    f"[compat harness]({source}/README.md): install, launch, a non-blank first "
    "frame, then tap, scroll and Back without a crash or ANR, plus per-app scripts.",
    "",
    "App start is time to the first frame on a fresh install in a running "
    "session; memory is app total PSS after the interactions. Cold session "
    "start is start to sys.boot_completed on previously booted data. Warm app "
    "start is the median for working apps. Idle session memory excludes page "
    "cache, measured 90 s after a cold start.",
  ]
  for mode, result in reports.items():
    lines += ["", f"## {mode.capitalize()}", ""]
    if result is None:
      lines += ["No results produced; see the CI run for the failure."]
      continue
    lines += [
      f"Run date: {cell(result['date'])} · Image: {cell(result['build'])}",
      "",
      f"{len(result['apps'])} apps: "
      + ", ".join(
        f"{sum(a['result'] == key for a in result['apps'])} {label.lower()}"
        for key, label in STATUSES.items()
      )
      + ".",
      "",
      "| App | Version | Framework | Native code | Status | App start | Memory | Notes |",
      "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for app in result["apps"]:
      info = corpus[app["package"]]
      native = info["native"]
      native = (
        "x86_64"
        if "x86_64" in native.split(",")
        else native
        if native == "none"
        else f"{native} only"
      )
      values = [
        app["name"],
        app["version_name"],
        info["framework"],
        native,
        STATUSES[app["result"]],
        measure(app.get("launch_ms"), "ms"),
        measure(app.get("pss_mb"), "MB"),
        app["reason"],
      ]
      lines.append("| " + " | ".join(map(cell, values)) + " |")
    lines += [
      "",
      "Session memory (cgroup) at end of run: "
      + measure(result.get("session_mem_mb"), "MiB")
      + ".",
      "",
      f"### Budgets ({mode})",
      "",
      f"A run fails above the budget plus its [margin]({source}/budgets.toml).",
      "",
      "| Measure | Budget | Margin | Fails above | Measured |",
      "| --- | --- | --- | --- | --- |",
    ]
    for key, label, unit in [
      ("session_start_ms", "Cold session start", "ms"),
      ("app_start_ms", "Warm app start", "ms"),
      ("idle_memory_mib", "Idle session memory", "MiB"),
    ]:
      budget = budgets[key]
      limit = int(budget["budget"] * (1 + budget["margin"]) + 0.5)
      lines.append(
        f"| {label} | {budget['budget']} {unit} | {budget['margin']:.0%} | "
        f"{limit} {unit} | {measure(result.get(key), unit)} |"
      )
  return "\n".join(lines) + "\n"


def main():
  parser = argparse.ArgumentParser(description=__doc__)
  parser.add_argument("--reports", type=Path, required=True)
  parser.add_argument("--commit", required=True)
  parser.add_argument("--run-url", required=True)
  parser.add_argument("--output", type=Path, required=True)
  args = parser.parse_args()
  root = Path(__file__).resolve().parents[2]
  corpus = tomllib.loads((root / "tests/compat/corpus.toml").read_text())
  budgets = tomllib.loads((root / "tests/compat/budgets.toml").read_text())
  reports = {}
  for mode in ("headless", "windowed"):
    path = args.reports / mode / "results.json"
    reports[mode] = json.loads(path.read_text()) if path.exists() else None
  if not any(reports.values()):
    parser.error("no saved compatibility results; leaving the wiki unchanged")
  args.output.parent.mkdir(parents=True, exist_ok=True)
  args.output.write_text(
    render(
      reports,
      {a["package"]: a for a in corpus["app"]},
      budgets,
      args.commit,
      args.run_url,
    )
  )


if __name__ == "__main__":
  main()
