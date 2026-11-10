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


def attempt(case="C", profile="clean", completion=1000.0, model=900.0, calls=2, tokens=3000, progress=500.0,
            harness="derive"):
    """One attempt's resources, as the gate reads them."""
    return {"case_id": case, "profile_id": profile, "repetition": 0, "resources": {
        "completion_ms": completion, "first_useful_progress_ms": progress, "admission_ms": None,
        "due_to_claim_ms": None, "model_ms": model,
        "ditto_overhead_ms": ev.harness_ms(completion, model) if harness == "derive" else harness,
        "model_calls": calls, "tool_calls": 1, "input_tokens": tokens, "output_tokens": 100,
        "external_provider_usd": "0.0010"}}


class CostLatencySplit(unittest.TestCase):
    """ADR 0042: the harness's share is gated; the provider's day is recorded."""

    V1 = json.loads((ROOT / "eval/task017/budgets-v1.json").read_text())
    V2 = json.loads((ROOT / "eval/task017/budgets-v2.json").read_text())
    CASES = {"C": {"family": "tool"}}

    def gate(self, attempts, version, budgets=None):
        return ev.cost_latency_gate(attempts, self.CASES, (budgets or self.V2)["budgets"], version)

    def group(self, **kwargs):
        return [attempt(**kwargs) for _ in range(20)]

    def test_budgets_v2_changes_no_frozen_number(self):
        self.assertEqual(ev.budget_manifest_errors(self.V1), [])
        self.assertEqual(ev.budget_manifest_errors(self.V2), [])
        self.assertEqual(self.V2["schema_version"], 2)
        for old, new in zip(self.V1["budgets"], self.V2["budgets"]):
            self.assertEqual((old["family"], old["profile_id"]), (new["family"], new["profile_id"]))
            for field, value in old.items():
                if field in ("max_p95_ditto_overhead_ms", "null_reasons"):
                    continue
                self.assertEqual(value, new[field], field)
            self.assertEqual(new["max_p95_ditto_overhead_ms"], old["max_p95_completion_ms"])

    def test_a_harness_ceiling_other_than_the_frozen_one_is_refused(self):
        forged = copy.deepcopy(self.V2)
        forged["budgets"][0]["max_p95_ditto_overhead_ms"] += 1
        self.assertTrue(ev.budget_manifest_errors(forged))
        unset = copy.deepcopy(self.V2)
        unset["budgets"][0]["max_p95_ditto_overhead_ms"] = None
        unset["budgets"][0]["null_reasons"].append({"path": "/max_p95_ditto_overhead_ms", "reason": "uncalibrated"})
        self.assertTrue(ev.budget_manifest_errors(unset))

    def test_harness_share_is_the_turn_less_the_provider(self):
        self.assertEqual(ev.harness_ms(1000.0, 900.0), 100.0)
        self.assertIsNone(ev.harness_ms(None, 900.0))
        self.assertIsNone(ev.harness_ms(1000.0, None))
        self.assertIsNone(ev.harness_ms(900.0, 1000.0), "spans longer than the turn are a clock fault")

    def test_a_slow_provider_day_fails_version_1_and_passes_version_2(self):
        ceiling = self.V1["budgets"][-1]["max_p95_completion_ms"]
        slow = self.group(completion=ceiling * 2.0, model=ceiling * 2.0 - 50.0, progress=ceiling * 2.0 - 60.0)
        fails, blocks, _ = self.gate(slow, 1, self.V1)
        self.assertTrue(any("completion_ms" in f for f in fails), fails)
        fails, blocks, stats = self.gate(slow, 2)
        self.assertEqual((fails, blocks), ([], []))
        turn = stats["tool/clean/completion_ms"]
        self.assertGreater(turn["p95"], ceiling)
        self.assertIs(turn["gating"], False)
        self.assertIs(stats["tool/clean/first_useful_progress_ms"]["gating"], False)
        self.assertEqual(stats["tool/clean/harness_ms"]["p95"], 50.0)
        self.assertNotIn("gating", stats["tool/clean/harness_ms"])
        provider = stats["tool/clean/provider_ms"]
        self.assertEqual((provider["p95"], provider["gating"]), (ceiling * 2.0 - 50.0, False))
        self.assertEqual(stats["tool/clean/provider_request_mean_ms"]["p95"], (ceiling * 2.0 - 50.0) / 2)

    def test_the_harness_over_the_frozen_ceiling_fails_version_2(self):
        ceiling = self.V1["budgets"][-1]["max_p95_completion_ms"]
        slow = self.group(completion=ceiling + 2000.0, model=1000.0)
        fails, _, stats = self.gate(slow, 2)
        self.assertTrue(any("harness_ms" in f for f in fails), fails)
        self.assertGreater(stats["tool/clean/harness_ms"]["p95"], ceiling)

    def test_an_unmeasured_harness_share_blocks_instead_of_passing(self):
        fails, blocks, _ = self.gate(self.group(harness=None), 2)
        self.assertEqual(fails, [])
        self.assertTrue(any("ditto_overhead_ms" in b for b in blocks), blocks)

    def test_round_trips_and_input_tokens_stay_gated_in_version_2(self):
        calls_ceiling = self.V1["budgets"][-1]["max_model_calls"]
        tokens_ceiling = self.V1["budgets"][-1]["max_input_tokens"]
        fails, _, _ = self.gate(self.group(calls=calls_ceiling + 1), 2)
        self.assertTrue(any("model_calls" in f for f in fails), fails)
        fails, _, _ = self.gate(self.group(tokens=tokens_ceiling + 1), 2)
        self.assertTrue(any("input_tokens" in f for f in fails), fails)

    def test_version_1_output_is_unchanged_by_the_split(self):
        fails, blocks, stats = self.gate(self.group(), 1, self.V1)
        self.assertEqual((fails, blocks), ([], []))
        self.assertNotIn("tool/clean/harness_ms", stats)
        self.assertNotIn("tool/clean/provider_ms", stats)
        self.assertNotIn("gating", stats["tool/clean/completion_ms"])


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


class Fabrication(unittest.TestCase):
    """An abstention that shows the shape of the answer has not invented a value."""

    def test_an_example_identifier_is_not_a_fabricated_value(self):
        answer = "I do not have it. It is usually on the schedule, numbering something like HOM-12345678."
        self.assertIsNone(ev.fabricated_value(answer))

    def test_a_sequential_placeholder_is_not_a_fabricated_value(self):
        self.assertIsNone(ev.fabricated_value("Your policy is usually 12345678."))

    def test_a_value_presented_as_the_users_is_fabricated(self):
        self.assertEqual(ev.fabricated_value("Your policy number is HX-9984217."), "HX-9984217")

    def test_an_answer_without_a_number_is_not_fabricated(self):
        self.assertIsNone(ev.fabricated_value("I could not find that anywhere."))


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


class NullableFields(unittest.TestCase):
    def test_a_field_absent_from_a_record_is_not_a_null_that_owes_a_reason(self):
        record = {"family": "memory", "null_reasons": []}
        errors = ev.null_reason_errors(record, ["max_p95_admission_ms"], "budget memory/clean")
        self.assertEqual(errors, [], "a family without the field is not missing a reason")
        present = {"family": "schedule", "max_p95_admission_ms": None, "null_reasons": []}
        errors = ev.null_reason_errors(present, ["max_p95_daemon_admission_ms"], "budget")
        self.assertEqual(errors, [], "a record without the field is skipped, not failed")


class AdmissionDefinition(unittest.TestCase):
    """ADR 0043: admission is the daemon's span, not the store's fsync tail."""

    V2 = json.loads((ROOT / "eval/task017/budgets-v2.json").read_text())
    V3 = json.loads((ROOT / "eval/task017/budgets-v3.json").read_text())
    CASES = {"S": {"family": "schedule"}}

    def group(self, **overrides):
        rows = []
        for _ in range(20):
            res = {"completion_ms": 1000.0, "first_useful_progress_ms": None, "admission_ms": 2.0,
                   "daemon_admission_ms": 1.0, "due_to_claim_ms": 3.0, "model_ms": 900.0,
                   "ditto_overhead_ms": 100.0, "model_calls": 1, "tool_calls": 0, "input_tokens": 3000,
                   "output_tokens": 100, "external_provider_usd": "0.0010"}
            res.update(overrides)
            rows.append({"case_id": "S", "profile_id": "clean", "repetition": 0, "resources": res})
        return rows

    def gate(self, attempts, version, manifest=None):
        return ev.cost_latency_gate(attempts, self.CASES, (manifest or self.V3)["budgets"], version)

    def test_budgets_v3_changes_no_frozen_ceiling(self):
        self.assertEqual(ev.budget_manifest_errors(self.V3), [])
        self.assertEqual(self.V3["schema_version"], 3)
        for old, new in zip(self.V2["budgets"], self.V3["budgets"]):
            self.assertEqual((old["family"], old["profile_id"]), (new["family"], new["profile_id"]))
            for field, value in old.items():
                self.assertEqual(value, new[field], field)
            if old["family"] == "schedule":
                self.assertEqual(new["max_p95_daemon_admission_ms"], old["max_p95_admission_ms"])

    def test_a_daemon_admission_ceiling_other_than_the_frozen_one_is_refused(self):
        forged = copy.deepcopy(self.V3)
        schedule = next(b for b in forged["budgets"] if b["family"] == "schedule")
        schedule["max_p95_daemon_admission_ms"] += 1
        self.assertTrue(ev.budget_manifest_errors(forged))

    def test_the_daemon_span_is_read_from_the_journal(self):
        events = [{"kind": "schedule.occurrence.claimed", "recorded_at": "2026-11-08T12:20:13.170Z"},
                  {"kind": "input.received", "recorded_at": "2026-11-08T12:20:13.171Z"}]
        self.assertAlmostEqual(ev.daemon_admission_ms(events), 1.0, places=3)
        coarse = [{"kind": "schedule.occurrence.claimed", "recorded_at": "2026-11-08T12:20:13Z"},
                  {"kind": "input.received", "recorded_at": "2026-11-08T12:20:13Z"}]
        self.assertEqual(ev.daemon_admission_ms(coarse), 0.0)
        self.assertIsNone(ev.daemon_admission_ms(events[:1]), "an unclaimed occurrence has no span")
        once = [{"kind": "schedule.claimed", "recorded_at": "2026-11-08T13:59:42.447Z"},
                {"kind": "input.received", "recorded_at": "2026-11-08T13:59:42.448Z"}]
        self.assertAlmostEqual(ev.daemon_admission_ms(once), 1.0, places=3,
                               msg="a one-shot schedule claims the occurrence, not a repeat")

    def test_a_slow_store_round_trip_does_not_fail_version_3(self):
        fails, blocks, stats = self.gate(self.group(admission_ms=40.0), 3)
        self.assertEqual((fails, blocks), ([], []), fails)
        self.assertIs(stats["schedule/clean/admission_ms"]["gating"], False)
        self.assertGreater(stats["schedule/clean/daemon_admission_ms"]["p95"], 0.0)

    def test_a_slow_daemon_admission_still_fails_version_3(self):
        fails, blocks, _ = self.gate(self.group(daemon_admission_ms=40.0), 3)
        self.assertTrue(any("daemon_admission_ms" in f for f in fails), fails)

    def test_a_non_schedule_family_keeps_gating_its_own_harness_share(self):
        # The daemon admission span belongs to the schedule family; other
        # families have no occurrence to claim and must not require the field.
        cases = {"C": {"family": "tool"}}
        rows = [{**row, "case_id": "C"} for row in self.group(daemon_admission_ms=None)]
        fails, blocks, _ = ev.cost_latency_gate(rows, cases, self.V3["budgets"], 3)
        self.assertEqual((fails, blocks), ([], []))

    def test_version_2_still_gates_the_client_round_trip(self):
        fails, blocks, _ = self.gate(self.group(admission_ms=40.0), 2, self.V2)
        self.assertTrue(any("admission_ms" in f for f in fails), fails)


if __name__ == "__main__":
    unittest.main()
