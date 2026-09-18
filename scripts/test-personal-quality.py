"""Assessment adversaries and the real offline CLI/memory/restart boundary."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.dont_write_bytecode = True


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f"personal-{name}.py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


baseline = module("baseline")


class AssessmentTests(unittest.TestCase):
    def setUp(self):
        self.quality = module("quality")
        self.nodes = [self.node("packing", "harbor packing checklist includes passport"),
                      self.node("train", "harbor train departs Friday")]
        self.forbidden = {"stale": [self.node("old", "cedar timezone is UTC")],
                          "scope": [self.node("other", "cedar timezone is PRIVATE")],
                          "irrelevant": [self.node("banana", "bananas ripen tomorrow")],
                          "noise": [self.node("noise", "synthetic unrelated pebble 000000")]}

    @staticmethod
    def node(identity, text):
        return {"id": identity, "kind": "claim", "summary": text, "origin": "user",
                "epistemic": "asserted", "scope": "session", "confidence": 1.0,
                "source_event_ids": ["source-" + identity]}

    def observation(self, nodes, identity="request-0"):
        return {"request_id": identity, "context_json": json.dumps({"nodes": nodes}, separators=(",", ":"))}

    def assess(self, nodes, expected=None, forbidden=None):
        return self.quality.assess_context(self.observation(nodes),
                                          self.nodes if expected is None else expected,
                                          self.forbidden if forbidden is None else forbidden)

    def failure(self, nodes, expected=None):
        with self.assertRaises(AssertionError) as caught:
            self.assess(nodes, expected)
        return caught.exception.diagnostics

    def test_cardinalities_and_explicit_empty_denominators(self):
        for n in (0, 1, 2):
            with self.subTest(n=n):
                result = self.assess(self.nodes[:n], self.nodes[:n], dict(self.forbidden, noise=[]))
                self.assertTrue(result["passed"])
                self.assertEqual(result["selected_node_count"], n)
                for name in ("exact_set", "exact_order"):
                    self.assertEqual(result["metrics"][name]["value"], 1.0)
                for name in ("recall_at_2", "returned_context_precision"):
                    metric = result["metrics"][name]
                    self.assertEqual((metric["numerator"], metric["denominator"]), (n, n))
                    self.assertEqual(metric["value"], 1.0 if n else None)
                    self.assertEqual(bool(metric["null_reason"]), n == 0)
                self.assertIsNone(result["metrics"]["noise_leaks"]["value"])
                self.assertTrue(result["metrics"]["noise_leaks"]["null_reason"])
                self.assertEqual(result["metrics"]["nontrivial_order"]["denominator"], int(n > 1))
        self.failure(self.nodes[:1], [])

    def test_order_partial_recall_and_duplicate_precision(self):
        result = self.failure(self.nodes[::-1])
        self.assertEqual(result["metrics"]["exact_set"]["value"], 1)
        self.assertEqual(result["metrics"]["exact_order"]["value"], 0)
        self.assertEqual(result["metrics"]["recall_at_2"]["value"], 1)
        result = self.failure(self.nodes[:1])
        self.assertEqual(result["metrics"]["recall_at_2"]["value"], .5)
        self.assertEqual(result["missing_ids"], ["train"])
        result = self.failure([*self.nodes, self.nodes[0]])
        self.assertEqual(result["duplicate_ids"], ["packing"])
        self.assertEqual(result["metrics"]["returned_context_precision"]["value"], 2 / 3)
        result = self.failure([self.nodes[0], self.nodes[0], self.nodes[1]])
        self.assertEqual(result["metrics"]["recall_at_2"]["value"], .5)

    def test_metadata_provenance_and_unknown_nodes_fail_closed(self):
        for field, bad in (("summary", "wrong"), ("kind", "goal"), ("origin", "model"),
                           ("epistemic", "inferred"), ("scope", "task"), ("confidence", True),
                           ("confidence", .9), ("source_event_ids", ["memory-record-event"]),
                           ("valid_from", "2026-01-01"), ("valid_until", None), ("extra", "field")):
            changed = copy.deepcopy(self.nodes)
            changed[0][field] = bad
            with self.subTest(field=field, bad=bad):
                result = self.failure(changed)
                self.assertFalse(result["passed"])
                self.assertTrue(result["mismatched_nodes"] or result["malformed_nodes"])
        result = self.failure([*self.nodes, self.node("unknown", "unknown")])
        self.assertEqual(result["extra_ids"], ["unknown"])
        for bad in (None, 1, {}, {"id": []}, {"id": "broken", "summary": 3}):
            with self.subTest(bad=bad):
                self.assertTrue(self.failure([bad])["malformed_nodes"])
        for raw in ('{}', '{"nodes":null}', '{"nodes":[],"extra":1}', '{"nodes":[],"nodes":[]}', 'bad'):
            with self.subTest(raw=raw), self.assertRaises(AssertionError):
                self.quality.assess_context({"request_id": "r", "context_json": raw}, [], self.forbidden)

    def test_all_leak_categories_match_id_or_embedded_text(self):
        for category, forbidden in self.forbidden.items():
            for node in (dict(forbidden[0], summary="changed"),
                         self.node("renamed", "prefix " + forbidden[0]["summary"] + " suffix")):
                with self.subTest(category=category, node=node):
                    result = self.failure([*self.nodes, node])
                    metric = result["metrics"][category + "_leaks"]
                    self.assertEqual((metric["numerator"], metric["denominator"]), (1, 1))
                    self.assertEqual(metric["contributors"][0]["ranks"], [3])

    def reconciliation_fixture(self):
        # Literal trip queries have the same set but opposite order. No corpus oracle.
        plans, events, calls, observations = [], [], [], []
        for i, (case, query, nodes) in enumerate((
                ("corrected_timezone", "cedar timezone",
                 [self.node("current", "cedar timezone is KST")]),
                ("trip_packing", "harbor packing", self.nodes),
                ("trip_train", "harbor train", self.nodes[::-1]),
                ("meal_preference", "supper preference",
                 [self.node("food", "supper preference is vegetarian")]),
                ("no_match", "observatory telescope", []))):
            identity = f"01K000000000000000002{i:05d}"
            task, turn, request = "run_" + identity, f"turn_{i}", f"model_request_{i}"
            plans.append({"case": case, "repetition": 0, "query": query, "session": "personal",
                          "client_request_id": identity, "task_id": task, "turn_id": turn})
            common = {"session_id": "personal", "task_id": task, "correlation_id": turn}
            seq = 1 + i * 2
            events.append(dict(common, seq=seq, event_id=f"input-{i}", actor="user", kind="input.received",
                               payload={"text": query, "agent_run": {"version": 1, "request_id": identity}}))
            events.append(dict(common, seq=seq+1, event_id=f"model-{i}", actor="system", kind="model.requested",
                               span_id=request, causation_id=f"input-{i}",
                               payload={"event_version": 1, "turn_id": turn, "request_index": 0,
                                        "request": {"request_id": request, "control": {"cancellation_id": turn},
                                                    "turn": {"context": {"nodes": nodes}, "conversation": [
                                                        {"type": "message", "role": "user", "content": [
                                                            {"type": "text", "text": query}]}]}}}))
            calls.append(request)
            observations.append(self.observation(nodes, request))
        return observations, events, plans, calls, 0

    def test_reconciliation_identity_query_order_and_capsule_adversaries(self):
        data = self.reconciliation_fixture()
        result = self.quality.reconcile_observations(*data)
        self.assertEqual(result["metric"]["value"], 1)
        self.assertEqual(result["counts"]["planned"], 5)
        self.assertEqual(result["counts"]["missing"], 0)
        self.assertEqual(result["counts"]["duplicate"], 0)
        self.assertEqual(result["counts"]["unmatched"], 0)
        mutations = [
            lambda d: d[0].pop(), lambda d: d[0].append(d[0][0]), lambda d: d[0].reverse(),
            lambda d: d[3].pop(), lambda d: d[3].append("unknown"), lambda d: d[3].reverse(),
            lambda d: d[3].__setitem__(1, d[3][0]), lambda d: d[1].pop(),
            lambda d: d[1].append(d[1][0]), lambda d: d[1].reverse(),
            lambda d: d[1][2]["payload"].update(text="wrong query"),
            lambda d: d[2][1].update(query="wrong query"),
            lambda d: d[2][1].update(case="wrong_case"),
            lambda d: d[2][1].update(repetition=1),
            lambda d: d[2][0].update(repetition=False),
            lambda d: d[1][3]["payload"]["request"]["turn"]["conversation"][0]["content"][0].update(text="wrong query"),
            lambda d: d[1][3].update(session_id="elsewhere"),
            lambda d: d[1][3].update(task_id="wrong"),
            lambda d: d[1][3].update(correlation_id="wrong"),
            lambda d: d[1][3]["payload"].update(turn_id="wrong"),
            lambda d: d[1][3]["payload"].update(request_index=1),
            lambda d: d[1][3]["payload"].update(request_index=False),
            lambda d: d[1][3].update(span_id="wrong"),
            lambda d: d[1][3].update(causation_id="wrong"),
            lambda d: d[1][3].update(event_id=d[1][1]["event_id"]),
            lambda d: d[1][3].update(seq=d[1][1]["seq"]),
            lambda d: d[1][3].update(seq=4.0),
            lambda d: d[1][2]["payload"]["agent_run"].update(request_id="wrong"),
            lambda d: d[1][2]["payload"]["agent_run"].update(version=True),
            lambda d: d[1][3]["payload"].update(event_version=1.0),
            lambda d: d[1][3]["payload"]["request"]["turn"]["context"].update(nodes=[]),
            lambda d: d[0][1].update(context_json=json.dumps(json.loads(d[0][1]["context_json"]), indent=2)),
        ]
        for i, mutate in enumerate(mutations):
            bad = copy.deepcopy(data)
            mutate(bad)
            with self.subTest(mutation=i), self.assertRaises(AssertionError):
                self.quality.reconcile_observations(*bad)
        with self.assertRaises(AssertionError):
            self.quality.reconcile_observations(*data[:-1], 2)
        with self.assertRaises(AssertionError):
            self.quality.reconcile_observations([], [], [], [], 0)

    def test_reconciliation_rejects_consistently_omitted_frozen_case(self):
        observations, events, plans, calls, boundary = copy.deepcopy(self.reconciliation_fixture())
        observations.pop()
        del events[-2:]
        plans.pop()
        calls.pop()
        with self.assertRaises(AssertionError):
            self.quality.reconcile_observations(observations, events, plans, calls, boundary)

    def test_unmatched_post_restart_input_is_rejected(self):
        observations, events, plans, calls, boundary = self.reconciliation_fixture()
        events.append({"seq": 11, "event_id": "extra-input", "kind": "input.received", "actor": "user",
                       "session_id": "personal", "task_id": None, "payload": {"text": "unplanned input"}})
        with self.assertRaises(AssertionError):
            self.quality.reconcile_observations(observations, events, plans, calls, boundary)

    def test_malformed_reconciliation_records_have_structured_diagnostics(self):
        for index, replacement in ((0, None), (0, {"request_id": []}), (1, None),
                                   (2, {"client_request_id": []}), (3, [])):
            data = list(copy.deepcopy(self.reconciliation_fixture()))
            data[index][0] = replacement
            with self.subTest(index=index, replacement=replacement):
                with self.assertRaises(AssertionError) as caught:
                    self.quality.reconcile_observations(*data)
                self.assertIn("counts", caught.exception.diagnostics)

    def test_final_hash_failure_does_not_publish_or_leave_staging_file(self):
        with tempfile.TemporaryDirectory(prefix="ditto-corpus-publication-") as temporary:
            root = Path(temporary)
            output = root / "report.json"
            output.write_text("previous report")
            artifact = root / "artifact"
            artifact.write_text("synthetic artifact")
            with mock.patch.object(sys, "argv", ["quality", "--output", str(output)]), \
                 mock.patch.object(self.quality.baseline, "build", return_value={"cli": artifact}), \
                 mock.patch.object(self.quality.baseline, "execute", return_value=mock.Mock(stdout="test")), \
                 mock.patch.object(self.quality, "workload", return_value={}), \
                 mock.patch.object(self.quality, "source_hashes", side_effect=[{"a": "original"}, {"a": "original"}, {"a": "changed"}]), \
                 mock.patch.object(self.quality.signal, "signal"), \
                 mock.patch.object(sys, "stderr", new_callable=io.StringIO) as diagnostics:
                with self.assertRaises(AssertionError):
                    self.quality.main()
                failure = json.loads(diagnostics.getvalue())["failure"]
                self.assertEqual(failure["stage"], "publication")
                self.assertIn("accounting", failure)
            self.assertEqual(output.read_text(), "previous report")
            self.assertFalse(list(root.glob(".task016-*.pending")))

    def test_argument_bounds_and_optimized_python_fail_before_build(self):
        script = str(Path(self.quality.__file__))
        for args in (["--history-size", "0"], ["--history-size", "5001"],
                     ["--samples", "1"], ["--samples", "101"]):
            result = subprocess.run([sys.executable, script, "--output", "unused.json", *args],
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("history-size must be", result.stderr)
        result = subprocess.run([sys.executable, "-O", script, "--output", "unused.json"],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("without -O", result.stderr)


class WorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binaries = baseline.build()

    def test_corpus_workload_replays_all_five_queries_after_restart(self):
        quality = module("quality")
        with tempfile.TemporaryDirectory(prefix="ditto-corpus-regression-") as temporary:
            result = quality.workload(Path(temporary), self.binaries, 4, 2)
        self.assertEqual(len(result["requests"]), 10)
        expected = [
            ("cedar timezone", ["cedar timezone is KST"]),
            ("harbor packing", ["harbor packing checklist includes passport", "harbor train departs Friday"]),
            ("harbor train", ["harbor train departs Friday", "harbor packing checklist includes passport"]),
            ("supper preference", ["supper preference is vegetarian"]),
            ("observatory telescope", []),
        ] * 2
        for run, (query, summaries) in zip(result["requests"], expected):
            self.assertEqual(run["query"], query)
            self.assertEqual(run["input_event"]["payload"]["text"], query)
            self.assertEqual(run["model_event"]["payload"]["request"]["turn"]["conversation"],
                             [{"type": "message", "role": "user", "content": [{"type": "text", "text": query}]}])
            self.assertEqual([n["summary"] for n in json.loads(run["context_json"])["nodes"]], summaries)
            self.assertGreater(run["input_event"]["seq"], result["recovery"]["last_seq_before_restart"])
        self.assertEqual(result["seed_accounting"]["event_counts"], {"input.received": 11, "context.node.recorded": 11})
        self.assertEqual(result["whole_workload_accounting"]["model_requests"], 10)
        self.assertEqual(result["whole_workload_accounting"]["injected_driver_calls"], 10)
        for key, value in (("exact_set", (10, 10)), ("exact_order", (10, 10)),
                           ("nontrivial_order", (4, 4)), ("recall_at_2", (12, 12)),
                           ("returned_context_precision", (12, 12))):
            metric = result["metrics"][key]
            self.assertEqual((metric["numerator"], metric["denominator"]), value)

    def test_profiles_use_distinct_client_request_identities(self):
        quality = module("quality")
        identities = []
        for noise in (0, 4):
            with tempfile.TemporaryDirectory(prefix="ditto-corpus-identities-") as temporary:
                result = quality.workload(Path(temporary), self.binaries, noise, 2)
            identities.extend(run["client_request_id"] for run in result["requests"])
        self.assertEqual(len(identities), 20)
        self.assertEqual(len(set(identities)), 20)

    def test_actual_cli_corrects_restarts_and_observes_each_capsule(self):
        # Preserve Task 015's independently exercised opt-in driver contract.
        with tempfile.TemporaryDirectory(prefix="ditto-quality-regression-") as temporary:
            server = baseline.Server(Path(temporary), True, self.binaries)
            server.environment["DITTO_BASELINE_OBSERVE_CONTEXT"] = "1"
            try:
                server.start()
                old = server.cli("memory", "save", "cedar timezone is UTC")
                corrected = server.cli("memory", "save", "cedar timezone is KST", "--replaces", old["memory_id"])
                server.cli("memory", "save", "bananas ripen tomorrow")
                server.cli("memory", "save", "cedar timezone is PRIVATE", "--session", "elsewhere")
                for i in range(4):
                    server.cli("memory", "save", f"synthetic unrelated pebble {i:06d}")
                self.assertEqual(baseline.accounting(server.events())["model_requests"], 0)
                boundary = server.get("/health")["durable_events"]
                server.stop()
                server.start()
                self.assertEqual(server.get("/health")["durable_events"], boundary)
                for i in range(2):
                    value = server.cli("run", "cedar timezone", "--request-id", f"01K000000000000000001{i:05d}")
                    self.assertEqual(value["status"], "unverified")
                observed = [json.loads(line) for line in (server.data / "fixture-contexts.jsonl").read_text().splitlines()]
                calls = (server.data / "fixture-calls.txt").read_text().splitlines()
                requests = [e["payload"]["request"] for e in server.events() if e["kind"] == "model.requested"]
                self.assertEqual(len(observed), 2)
                self.assertEqual([o["request_id"] for o in observed], calls)
                self.assertEqual(calls, [r["request_id"] for r in requests])
                for observation, request in zip(observed, requests):
                    context = json.loads(observation["context_json"])
                    self.assertEqual(context, request["turn"]["context"])
                    self.assertEqual([(n["id"], n["summary"]) for n in context["nodes"]],
                                     [(corrected["memory_id"], "cedar timezone is KST")])
            finally:
                server.stop()

    def test_context_log_is_opt_in_and_call_counting_is_unchanged(self):
        with tempfile.TemporaryDirectory(prefix="ditto-quality-opt-in-") as temporary:
            server = baseline.Server(Path(temporary), True, self.binaries)
            try:
                server.start()
                server.cli("run", "cedar timezone", "--request-id", "01K00000000000000000300000")
                self.assertFalse((server.data / "fixture-contexts.jsonl").exists())
                self.assertEqual(len((server.data / "fixture-calls.txt").read_text().splitlines()), 1)
            finally:
                server.stop()

    def test_quality_sigterm_reaps_server_and_removes_store(self):
        self.cleanup_case("SIGTERM", restart=False)

    def test_quality_sigterm_after_restart_reaps_server_and_removes_store(self):
        self.cleanup_case("SIGTERM", restart=True)

    def test_quality_sigint_after_restart_reaps_server_and_removes_store(self):
        self.cleanup_case("SIGINT", restart=True)

    def test_assessment_failure_reaps_server_and_preserves_existing_report(self):
        self.cleanup_case("assessment", restart=True)

    def cleanup_case(self, mode, restart):
        bootstrap = '''
import importlib.util, json, sys, time
from pathlib import Path
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("quality", sys.argv[1])
quality = importlib.util.module_from_spec(spec)
spec.loader.exec_module(quality)
marker = Path(sys.argv[2])
mode, restart = sys.argv[3], sys.argv[4] == "True"
original_start = quality.baseline.Server.start
def start(server):
    original_start(server)
    if len(server.startups) == (2 if restart else 1):
        pending = marker.with_suffix(".pending")
        pending.write_text(json.dumps({"pid": server.process.pid, "root": str(server.root)}))
        pending.replace(marker)
        if mode != "assessment":
            time.sleep(30)
quality.baseline.Server.start = start
if mode == "assessment":
    original_assess = quality.assess_context
    def fail(observation, expected, forbidden):
        observation = dict(observation, context_json='{"nodes":[]}')
        return original_assess(observation, expected, forbidden)
    quality.assess_context = fail
sys.argv = [sys.argv[1], "--output", str(marker.with_suffix(".report")), "--history-size", "4", "--samples", "2"]
quality.main()
'''
        with tempfile.TemporaryDirectory(prefix="ditto-quality-signal-") as temporary:
            marker = Path(temporary) / "ready.json"
            if mode == "assessment":
                marker.with_suffix(".report").write_text("existing report must survive")
            process = subprocess.Popen([sys.executable, "-c", bootstrap,
                                        str(Path(__file__).with_name("personal-quality.py").resolve()),
                                        str(marker), mode, str(restart)],
                                       env=dict(os.environ, TMPDIR=temporary), text=True,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            child_pid = None
            try:
                deadline = time.monotonic() + 30
                while not marker.exists():
                    self.assertIsNone(process.poll(), process.communicate() if process.poll() is not None else "")
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                observed = json.loads(marker.read_text())
                child_pid = observed["pid"]
                if mode != "assessment":
                    process.send_signal(getattr(signal, mode))
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, {"SIGTERM": 143, "SIGINT": 130, "assessment": 1}[mode], (stdout, stderr))
                if mode == "assessment":
                    diagnostics = [json.loads(line) for line in stderr.splitlines() if line.startswith('{"failure":')]
                    self.assertEqual(len(diagnostics), 1, stderr)
                    self.assertEqual(diagnostics[0]["failure"]["accounting"]["model_requests"], 10)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child_pid, 0)
                self.assertFalse(Path(observed["root"]).exists())
                if mode == "assessment":
                    self.assertEqual(marker.with_suffix(".report").read_text(), "existing report must survive")
                else:
                    self.assertFalse(marker.with_suffix(".report").exists())
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate(timeout=5)
                if child_pid is not None:
                    try:
                        os.kill(child_pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass


if __name__ == "__main__":
    unittest.main()
