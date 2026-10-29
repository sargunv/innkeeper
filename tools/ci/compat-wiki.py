#!/usr/bin/env python3
"""Render saved nightly reports; never launches an Android session."""

import argparse
import html
import json
from pathlib import Path
import tomllib

REPO = "https://github.com/sargunv/linux.apk"
STATUSES = {
  "works": "✅ Works",
  "partly": "⚠️ Partly works",
  "broken": "❌ Doesn't work",
  "error": "➖ Not tested",
}


def cell(value):
  return (
    html.escape(str(value), quote=False)
    .replace("|", "&#124;")
    .replace("\n", "<br>")
    .replace("\r", "")
  )


def render(reports, corpus):
  source = f"{REPO}/blob/main/tests/compat"
  lines = [
    "# Compatibility",
    "",
    f"Apps from the [F-Droid corpus]({source}/corpus.toml), smoke-tested by the "
    f"[compat harness]({source}/README.md): install, launch, a non-blank first "
    "frame, then tap, scroll and Back without a crash or ANR, plus per-app scripts.",
  ]
  for mode, result in reports.items():
    lines += ["", f"## {mode.capitalize()}", ""]
    if result is None:
      lines += ["No results produced; see the CI run for the failure."]
      continue
    lines += [
      f"Image: {cell(result['build'])}",
      "",
      f"{len(result['apps'])} apps: "
      + " · ".join(
        f"{label.split()[0]} {sum(a['result'] == key for a in result['apps'])}"
        for key, label in STATUSES.items()
      )
      + ".",
      "",
      "| App | Version | Framework | Native code | Result | Notes |",
      "| --- | --- | --- | --- | --- | --- |",
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
        app["reason"],
      ]
      lines.append("| " + " | ".join(map(cell, values)) + " |")
  return "\n".join(lines) + "\n"


def main():
  parser = argparse.ArgumentParser(description=__doc__)
  parser.add_argument("--reports", type=Path, required=True)
  parser.add_argument("--output", type=Path, required=True)
  args = parser.parse_args()
  root = Path(__file__).resolve().parents[2]
  corpus = tomllib.loads((root / "tests/compat/corpus.toml").read_text())
  reports = {}
  for mode in ("headless", "windowed"):
    path = args.reports / mode / "fresh-results.json"
    reports[mode] = json.loads(path.read_text()) if path.exists() else None
  if not any(reports.values()):
    parser.error("no saved compatibility results; leaving the wiki unchanged")
  args.output.parent.mkdir(parents=True, exist_ok=True)
  args.output.write_text(
    render(
      reports,
      {a["package"]: a for a in corpus["app"]},
    )
  )


if __name__ == "__main__":
  main()
