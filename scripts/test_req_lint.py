"""Regression tests for the REQ-OPS-007 traceability gate."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

LINTER = Path(__file__).with_name("req-lint.py").resolve()


class RequirementCatalogTests(unittest.TestCase):
    def run_gate(self, root):
        return subprocess.run(
            [sys.executable, "-B", LINTER, "--root", root], capture_output=True, text=True
        )

    def test_req_ops_007_source_export_requires_catalog(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_gate(directory)
            self.assertEqual(result.returncode, 1)
            self.assertIn("docs/requirements.md", result.stderr)

    def test_req_ops_007_only_definitions_count(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "docs").mkdir()
            catalog = root / "docs/requirements.md"
            catalog.write_text("| REQ-OPS-007 | See REQ-TEST-999. |\n")
            (root / "sample.rs").write_text("#[test]\nfn req_test_999_sample() {}\n")
            result = self.run_gate(root)
            self.assertEqual(result.returncode, 1)
            self.assertIn("REQ-TEST-999", result.stderr)
            catalog.write_text("| REQ-TEST-999 | A defined requirement. |\n")
            self.assertEqual(self.run_gate(root).returncode, 0)


if __name__ == "__main__":
    unittest.main()
