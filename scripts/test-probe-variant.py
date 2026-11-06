#!/usr/bin/env python3
"""Tests for scripts/probe_variant.py (Task 042)."""

from __future__ import annotations

import importlib.util
import io
import json
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).with_name("probe_variant.py")
spec = importlib.util.spec_from_file_location("probe_variant", SCRIPT)
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def make_repo(root: Path, body: str) -> Path:
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(["git", "config", "user.email", "test@example.com"], cwd=root, check=True)
    subprocess.run(["git", "config", "user.name", "test"], cwd=root, check=True)
    target = root / "surface.txt"
    target.write_text(body, encoding="utf-8")
    (root / "other.txt").write_text("unrelated\n", encoding="utf-8")
    subprocess.run(["git", "add", "surface.txt", "other.txt"], cwd=root, check=True)
    subprocess.run(["git", "commit", "-qm", "base"], cwd=root, check=True)
    return target


def make_variant(root: Path, find: str, replace: str, name: str = "v1") -> Path:
    path = root / "variant.json"
    path.write_text(json.dumps({"name": name,
                                "edits": [{"file": "surface.txt", "find": find, "replace": replace}],
                                "cases": ["P3"]}), encoding="utf-8")
    return path


class ApplyRevertTest(unittest.TestCase):
    def test_round_trip_restores_exact_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            body = "alpha\nbeta\ngamma\n"
            target = make_repo(root, body)
            variant = probe.load_variant(make_variant(root, "beta", "beta two-hop"))
            probe.apply(root, variant)
            self.assertIn("beta two-hop", target.read_text(encoding="utf-8"))
            probe.revert(root, variant)
            self.assertEqual(target.read_bytes(), body.encode("utf-8"))
            self.assertFalse(probe.store_dir(root, "v1").exists())

    def test_ambiguous_edit_changes_nothing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            body = "beta\nbeta\n"
            target = make_repo(root, body)
            variant = probe.load_variant(make_variant(root, "beta", "gamma"))
            with self.assertRaises(SystemExit):
                probe.apply(root, variant)
            self.assertEqual(target.read_bytes(), body.encode("utf-8"))
            self.assertFalse(probe.store_dir(root, "v1").exists())

    def test_dirty_tree_outside_the_variant_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = make_repo(root, "beta\n")
            (root / "other.txt").write_text("edited elsewhere\n", encoding="utf-8")
            variant = probe.load_variant(make_variant(root, "beta", "gamma"))
            with self.assertRaises(SystemExit):
                probe.apply(root, variant)
            self.assertEqual(target.read_bytes(), b"beta\n")

    def test_second_apply_needs_a_revert(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_repo(root, "beta\n")
            variant = probe.load_variant(make_variant(root, "beta", "gamma"))
            probe.apply(root, variant)
            with self.assertRaises(SystemExit):
                probe.apply(root, variant)
            probe.revert(root, variant)

    def test_two_edits_to_one_file_both_apply(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            body = "one = [1]\ntwo = [2]\n"
            target = make_repo(root, body)
            manifest = root / "variant.json"
            manifest.write_text(json.dumps({"name": "v2", "cases": ["X1"], "edits": [
                {"file": "surface.txt", "find": "one = [1]", "replace": "one = [1, 3]"},
                {"file": "surface.txt", "find": "two = [2]", "replace": "two = [2, 4]"}]}), encoding="utf-8")
            variant = probe.load_variant(manifest)
            probe.apply(root, variant)
            self.assertEqual(target.read_text(encoding="utf-8"), "one = [1, 3]\ntwo = [2, 4]\n")
            probe.revert(root, variant)
            self.assertEqual(target.read_text(encoding="utf-8"), body)


def attempt(case: str, verified: bool, recall: tuple[int, int], usd: str, tokens: int) -> dict:
    return {"case_id": case, "state": "observed", "independent_success": verified,
            "resources": {"external_provider_usd": usd, "prompt_tokens": tokens,
                          "completion_ms": 1000},
            "context": {"recall_at_5": {"numerator": recall[0], "denominator": recall[1]}}}


class CompareTest(unittest.TestCase):
    def write(self, path: Path, attempts: list[dict], gates: dict) -> Path:
        path.mkdir(parents=True, exist_ok=True)
        report = path / "report.json"
        report.write_text(json.dumps({"attempts": attempts, "gates": gates}), encoding="utf-8")
        return report

    def test_gain_prints_and_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = self.write(Path(tmp) / "base", [attempt("P3", False, (0, 1), "0.01", 100)], {"safety": "pass"})
            variant = self.write(Path(tmp) / "variant", [attempt("P3", True, (1, 1), "0.02", 103)],
                                 {"safety": "pass"})
            buffer = io.StringIO()
            with redirect_stdout(buffer):
                code = probe.compare(base, variant)
            self.assertEqual(code, 0)
            self.assertIn("+1", buffer.getvalue())

    def test_regression_and_gate_loss_exit_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = self.write(Path(tmp) / "base", [attempt("P3", True, (1, 1), "0.01", 100)], {"safety": "pass"})
            variant = self.write(Path(tmp) / "variant", [attempt("P3", False, (0, 1), "0.01", 100)],
                                 {"safety": "fail"})
            buffer = io.StringIO()
            with redirect_stdout(buffer):
                code = probe.compare(base, variant)
            self.assertEqual(code, 1)
            self.assertIn("gate safety", buffer.getvalue())

    def test_recall_regression_exits_one_even_when_answers_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = self.write(Path(tmp) / "base", [attempt("P3", True, (2, 2), "0.01", 100)], {"safety": "pass"})
            variant = self.write(Path(tmp) / "variant", [attempt("P3", True, (1, 2), "0.01", 100)],
                                 {"safety": "pass"})
            with redirect_stdout(io.StringIO()):
                self.assertEqual(probe.compare(base, variant), 1)

    def test_compare_accepts_a_run_directory(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = self.write(Path(tmp) / "base", [attempt("P3", False, (0, 1), "0.01", 100)], {"safety": "pass"})
            self.write(Path(tmp) / "variant", [attempt("P3", True, (1, 1), "0.01", 100)], {"safety": "pass"})
            with redirect_stdout(io.StringIO()):
                self.assertEqual(probe.compare(base.parent, Path(tmp) / "variant"), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
