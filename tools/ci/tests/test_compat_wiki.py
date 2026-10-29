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
COMMIT = "9523a147"
RUN_URL = "https://github.com/sargunv/linux.apk/actions/runs/123"


class WikiTest(unittest.TestCase):
  def test_saved_results_include_status_metrics_notes_and_provenance(self):
    results = json.loads(FIXTURE.read_text())
    corpus = tomllib.loads((ROOT / "tests/compat/corpus.toml").read_text())
    budgets = tomllib.loads((ROOT / "tests/compat/budgets.toml").read_text())
    md = WIKI.render(
      {"headless": results, "windowed": results},
      {a["package"]: a for a in corpus["app"]},
      budgets,
      COMMIT,
      RUN_URL,
    )
    for text in [
      "Run date: 2026-10-28",
      f"[{COMMIT}]({WIKI.REPO}/commit/{COMMIT})",
      f"[CI run]({RUN_URL})",
      "## Headless",
      "## Windowed",
      "| OpenCalc | 3.2.0 | Android Views | none | Works | 111 ms | 35 MB |",
      "| Open Sudoku | 4.11.3 | Android Views | arm64-v8a only | Doesn't work | Not measured | Not measured |",
      "INSTALL_FAILED_NO_MATCHING_ABIS",
      "| Partly works | 116 ms | 82 MB |",
      "VPN unavailable in Flatpak",
      "Not tested (harness error)",
      "harness &#124; stopped<br>&lt;no frame&gt;",
      "| Cold session start | 2500 ms | 25% | 3125 ms | 2162 ms |",
      "| Warm app start | 200 ms | 25% | 250 ms | 171 ms |",
      "| Idle session memory | 545 MiB | 10% | 600 MiB | 542 MiB |",
      "Session memory (cgroup) at end of run: 954 MiB.",
    ]:
      with self.subTest(text=text):
        self.assertIn(text, md)

  def test_cli_marks_missing_mode_and_is_deterministic(self):
    with self.workspace() as work:
      work = Path(work)
      (work / "headless").mkdir()
      (work / "headless/results.json").write_bytes(FIXTURE.read_bytes())
      output = work / "Compatibility.md"
      command = self.command(work, output)
      subprocess.run(command, check=True, capture_output=True)
      first = output.read_bytes()
      self.assertIn(
        b"No results produced; see the CI run for the failure.", first
      )
      subprocess.run(command, check=True, capture_output=True)
      self.assertEqual(output.read_bytes(), first)

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
      "--commit",
      COMMIT,
      "--run-url",
      RUN_URL,
      "--output",
      str(output),
    ]


if __name__ == "__main__":
  unittest.main()
