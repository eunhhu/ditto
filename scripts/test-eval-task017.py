"""Task 037: suite v1 integrity, the evidence validator's adversaries and a real dry-run boundary."""
import copy
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("eval_task017", ROOT / "scripts/eval_task017.py")
ev = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ev)
SUITE = ROOT / "eval/task017/suite-v1.json"
DAEMON = ROOT / "target/debug/ditto-daemon"


class Suite(unittest.TestCase):
    def test_frozen_suite_is_valid_and_covers_the_minimum(self):
        suite = ev.strict_loads(SUITE.read_bytes())
        self.assertEqual(ev.suite_errors(suite), [])
        ids = {c["case_id"] for c in suite["cases"]}
        self.assertLessEqual({"P1", "P2", "P3", "P4", "P5", "T1", "T2", "S1", "S2", "X1"}, ids)
        self.assertEqual(len(ev.planned_attempts(suite)), 130)

    def test_suite_rejections(self):
        suite = ev.strict_loads(SUITE.read_bytes())
        long_use = next(i for i, p in enumerate(suite["profiles"]) if p["profile_id"] == "long_use")
        for mutate in (lambda s: s["cases"].pop(0), lambda s: s["cases"][0].update(repetitions=4),
                       lambda s: s["profiles"][long_use].update(unrelated_memories=999),
                       lambda s: s["cases"][0].update(profiles=["clean"]),
                       lambda s: s["cases"].append(copy.deepcopy(s["cases"][0]))):
            broken = copy.deepcopy(suite)
            mutate(broken)
            self.assertNotEqual(ev.suite_errors(broken), [], mutate)

    def test_pinned_fixture_digest_reproduces(self):
        suite = ev.strict_loads(SUITE.read_bytes())
        pinned = next(p for p in suite["profiles"] if p["profile_id"] == "long_use")["inert_header_fixture_sha256"]
        with tempfile.TemporaryDirectory() as tmp:
            try:
                digest = ev.build_inert_fixture(Path(tmp) / "fixture")
            except (OSError, subprocess.CalledProcessError) as error:
                self.skipTest(f"header tool unavailable: {error}")
        self.assertEqual(digest, pinned)

    def test_duplicate_keys_and_constants_are_refused(self):
        for text in ('{"a":1,"a":2}', '{"a":NaN}'):
            with self.assertRaises(ValueError):
                ev.strict_loads(text)

    def test_nearest_rank(self):
        self.assertEqual(ev.nearest_rank(list(range(1, 21)), 0.95), 19)
        self.assertEqual(ev.nearest_rank([5], 0.95), 5)

    def test_cost_uses_decimal_list_price(self):
        pricing = json.loads((ROOT / "eval/task017/pricing/deepseek-v4.1-flash-zen.json").read_text())
        self.assertEqual(ev.price(pricing, 1_000_000, 1_000_000), "1.5")


@unittest.skipUnless(DAEMON.exists() and shutil.which("strace"), "needs target/debug/ditto-daemon and strace")
class DryRunBoundary(unittest.TestCase):
    """One real dry-run over the daemon; every adversary mutates a copy of its output."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = Path(tempfile.mkdtemp(prefix="ditto-eval-test-"))
        cls.run_dir = cls.tmp / "run"
        result = subprocess.run(
            [sys.executable, str(ROOT / "scripts/eval_task017.py"), "run", "--dry-run", "--daemon", str(DAEMON),
             "--build-profile", "debug", "--pricing", str(ROOT / "eval/task017/pricing/dry-run-zero.json"),
             "--out", str(cls.run_dir), "--cases", "T1", "T2"],
            cwd=ROOT, capture_output=True, text=True, timeout=900, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
        if result.returncode:
            raise AssertionError(result.stdout[-2000:] + result.stderr[-2000:])

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def errors(self, run_dir):
        return ev.report_errors(run_dir, SUITE.read_bytes())

    def copy(self):
        target = Path(tempfile.mkdtemp(dir=self.tmp)) / "run"
        shutil.copytree(self.run_dir, target, symlinks=True)
        return target

    def rewrite(self, run_dir, mutate):
        path = run_dir / "report.json"
        report = json.loads(path.read_text())
        mutate(report)
        path.write_text(json.dumps(report))

    def test_honest_dry_run_validates_and_never_claims_readiness(self):
        self.assertEqual(self.errors(self.run_dir), [])
        report = json.loads((self.run_dir / "report.json").read_text())
        self.assertEqual(report["readiness"], "blocked")
        gates = {g["gate_id"]: g["state"] for g in report["gates"]}
        self.assertEqual(gates["integrity"], "blocked")
        self.assertEqual(gates["safety"], "pass")
        self.assertEqual(gates["tool"], "pass")
        self.assertTrue(all(c["seed_model_calls"] == 0 and c["idle_model_calls"] == 0 for c in report["controls"]))

    def test_forged_gate_state_is_invalid(self):
        run = self.copy()
        self.rewrite(run, lambda r: (r["gates"][0].update(state="pass"), r.update(readiness="pass")))
        self.assertTrue(self.errors(run))

    def test_dropped_attempt_is_invalid(self):
        run = self.copy()
        self.rewrite(run, lambda r: r["attempts"].pop())
        self.assertTrue(self.errors(run))

    def test_null_without_reason_is_invalid(self):
        run = self.copy()

        def drop(report):
            attempt = next(a for a in report["attempts"] if a["null_reasons"])
            attempt["null_reasons"].pop()
        self.rewrite(run, drop)
        self.assertTrue(self.errors(run))

    def test_tampered_evidence_object_is_invalid(self):
        run = self.copy()
        report = json.loads((run / "report.json").read_text())
        index = run / report["evidence_index_path"]
        entry = json.loads(index.read_text())["entries"][0]
        target = index.parent / entry["relative_path"]
        target.write_bytes(target.read_bytes() + b" ")
        self.assertTrue(self.errors(run))

    def test_symlinked_evidence_is_invalid(self):
        run = self.copy()
        report = json.loads((run / "report.json").read_text())
        index = run / report["evidence_index_path"]
        entry = json.loads(index.read_text())["entries"][0]
        target = index.parent / entry["relative_path"]
        outside = self.tmp / f"outside-{os.getpid()}"
        outside.write_bytes(target.read_bytes())
        target.unlink()
        target.symlink_to(outside)
        self.assertTrue(self.errors(run))


class BudgetCheck(unittest.TestCase):
    """A p95 ceiling describes the group, not every single attempt."""

    def test_one_attempt_over_a_p95_ceiling_is_not_a_failure(self):
        values = [700, 760, 800, 810, 830, 850, 900, 910, 920, 930, 940, 950, 960, 970, 980, 990,
                  1000, 1100, 1200, 1400]
        p95, runaways = ev.budget_check(values, 1394)
        self.assertLessEqual(p95, 1394)
        self.assertEqual(runaways, [])

    def test_a_three_times_runaway_still_fails_alone(self):
        p95, runaways = ev.budget_check([700, 800, 900, 1000, 4200], 1394)
        self.assertEqual(runaways, [4200])


class CaseThreshold(unittest.TestCase):
    """A five-attempt case cannot carry a 0.80 threshold."""

    def test_small_denominators_only_block_a_real_defect(self):
        self.assertEqual(ev.case_threshold(5, 0.80), 0.40)
        self.assertEqual(ev.case_threshold(4, 0.80), 0.40)
        self.assertIsNone(ev.case_threshold(3, 0.80))

    def test_ten_attempts_carry_the_standard(self):
        self.assertEqual(ev.case_threshold(10, 0.80), 0.80)
        self.assertEqual(ev.case_threshold(40, 0.90), 0.90)


class StaleBinary(unittest.TestCase):
    """A campaign refuses to measure a binary that predates the source tree."""

    def test_staleness_check_covers_rust_and_capability_sources(self):
        newest = ev.newest_source_mtime()
        self.assertGreater(newest, 0)
        self.assertGreaterEqual(newest, (ev.ROOT / "Cargo.toml").stat().st_mtime)


class TransportLoss(unittest.TestCase):
    """A provider that answers with nothing is re-asked, an ordinary failure is not."""

    def attempt(self, **over):
        base = {"state": "failed", "failure": None,
                "resources": {"model_calls": 1, "input_tokens": None, "output_tokens": None}}
        base.update(over)
        return base

    def test_empty_response_counts_as_transport_loss(self):
        self.assertTrue(ev.transport_loss(self.attempt()))

    def test_a_failure_with_a_reason_is_a_real_failure(self):
        self.assertFalse(ev.transport_loss(self.attempt(failure="deadline")))

    def test_a_turn_that_spent_tokens_is_not_a_lost_response(self):
        self.assertFalse(ev.transport_loss(self.attempt(resources={"model_calls": 1, "input_tokens": 900,
                                                                  "output_tokens": 12})))
        self.assertFalse(ev.transport_loss(self.attempt(resources={"model_calls": 3, "input_tokens": None,
                                                                  "output_tokens": None})))

    def test_an_observed_turn_is_never_retried(self):
        self.assertFalse(ev.transport_loss(self.attempt(state="observed")))


class FailureLog(unittest.TestCase):
    """A store that fails to start must leave its daemon log in the run."""

    def test_empty_log_is_kept_once(self):
        runner = object.__new__(ev.Campaign)
        runner.failure_logs = []
        with tempfile.TemporaryDirectory() as tmp:
            log = Path(tmp) / "S2-clean.log"
            log.write_bytes(b"")
            daemon = type("Daemon", (), {"log": log})()
            runner.keep_failure_log(daemon)
            runner.keep_failure_log(daemon)
            self.assertEqual(runner.failure_logs, [log])

    def test_missing_log_is_ignored(self):
        runner = object.__new__(ev.Campaign)
        runner.failure_logs = []
        daemon = type("Daemon", (), {"log": Path("/nonexistent/x.log")})()
        runner.keep_failure_log(daemon)
        self.assertEqual(runner.failure_logs, [])


if __name__ == "__main__":
    unittest.main()
