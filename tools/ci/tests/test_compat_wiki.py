"""Saved JSON proof for the wiki renderer; no APKs, sessions or network."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "tools/ci/compat-wiki.py"
SPEC = importlib.util.spec_from_file_location("compat_wiki", SCRIPT)
WIKI = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WIKI)
FIXTURE = Path(__file__).parent / "fixtures/results.json"


class WikiTest(unittest.TestCase):
  def test_saved_results_include_app_details_emoji_notes_and_image(self):
    results = json.loads(FIXTURE.read_text())
    corpus = tomllib.loads((ROOT / "tests/compat/corpus.toml").read_text())
    md = WIKI.render(
      {"headless": results, "windowed": results},
      {a["package"]: a for a in corpus["app"]},
    )
    for text in [
      f"Image: {results['build']}",
      "## Headless",
      "## Windowed",
      "4 apps: ✅ 1 · ⚠️ 1 · ❌ 1 · ➖ 1.",
      "| App | Version | Framework | Native code | Result | Notes |",
      "| OpenCalc | 3.2.0 | Android Views | none | ✅ Works |",
      "| Open Sudoku | 4.11.3 | Android Views | arm64-v8a only | ❌ Doesn't work |",
      "INSTALL_FAILED_NO_MATCHING_ABIS",
      "| ⚠️ Partly works |",
      "VPN unavailable in Flatpak",
      "| ➖ Not tested |",
      "harness &#124; stopped<br>&lt;no frame&gt;",
    ]:
      with self.subTest(text=text):
        self.assertIn(text, md)
    for text in [
      results["date"],
      "Commit:",
      "CI run](",
      "App start",
      "Memory",
      "Session memory",
      "Budgets",
      "budget",
      "Cold session start",
      "Warm app start",
      "Idle session memory",
      " ms",
      " MB",
      " MiB",
    ]:
      with self.subTest(absent=text):
        self.assertNotIn(text, md)

  def test_cli_ignores_run_noise_but_updates_compatibility(self):
    with self.workspace() as work:
      work = Path(work)
      (work / "headless").mkdir()
      results = json.loads(FIXTURE.read_text())
      saved = work / "headless/results.json"
      saved.write_text(json.dumps(results))
      output = work / "Compatibility.md"
      command = self.command(work, output)
      subprocess.run(command, check=True, capture_output=True)
      first = output.read_bytes()
      self.assertIn(
        b"No results produced; see the CI run for the failure.", first
      )
      results.update(
        date="2099-01-01",
        commit="changed-sha",
        run_url="https://example.com/run/2",
        session_mem_mb=9999,
        session_start_ms=9999,
        app_start_ms=9999,
        idle_memory_mib=9999,
      )
      for app in results["apps"]:
        app.update(launch_ms=9999, pss_mb=9999)
      saved.write_text(json.dumps(results))
      subprocess.run(command, check=True, capture_output=True)
      self.assertEqual(output.read_bytes(), first)
      results["apps"][0].update(
        result="partly", reason="new compatibility failure"
      )
      saved.write_text(json.dumps(results))
      subprocess.run(command, check=True, capture_output=True)
      self.assertNotEqual(output.read_bytes(), first)
      self.assertIn("4 apps: ✅ 0 · ⚠️ 2 · ❌ 1 · ➖ 1.", output.read_text())
      self.assertIn(
        "| ⚠️ Partly works | new compatibility failure |", output.read_text()
      )

  def test_no_results_leaves_existing_page_untouched(self):
    with self.workspace() as work:
      work = Path(work)
      output = work / "Compatibility.md"
      output.write_text("old page")
      result = subprocess.run(
        self.command(work, output), capture_output=True, text=True
      )
      self.assertNotEqual(result.returncode, 0)
      self.assertIn("no saved compatibility results", result.stderr)
      self.assertEqual(output.read_text(), "old page")

  def workspace(self):
    (ROOT / "out").mkdir(exist_ok=True)
    return tempfile.TemporaryDirectory(dir=ROOT / "out")

  def command(self, reports, output):
    return [
      "python3",
      str(SCRIPT),
      "--reports",
      str(reports),
      "--output",
      str(output),
    ]


if __name__ == "__main__":
  unittest.main()
