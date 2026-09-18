"""Assessment adversaries and the real offline CLI/memory/restart boundary."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

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
        self.memories = {key: {"memory_id": key, "text": text} for key, text in (
            ("old", "cedar timezone is UTC"), ("corrected", "cedar timezone is KST"),
            ("irrelevant", "bananas ripen tomorrow"), ("other_session", "cedar timezone is PRIVATE"))}
        self.memories["noise"] = [{"memory_id": "noise-0", "text": "synthetic unrelated pebble 000000"}]
        self.node = {"id": "corrected", "summary": self.memories["corrected"]["text"]}

    def observation(self, nodes, identity="request-0"):
        return {"request_id": identity, "context_json": json.dumps({"nodes": nodes}, ensure_ascii=False)}

    def test_exact_corrected_context_is_measured_from_serialized_capsule(self):
        observed = self.observation([self.node])
        result = self.quality.assess_context(observed, self.memories)
        self.assertTrue(all(result["outcomes"].values()))
        self.assertEqual(result["selected_node_count"], 1)
        self.assertEqual(result["serialized_bytes"], len(observed["context_json"].encode("utf-8")))

    def test_exclusions_fail_for_ids_and_for_values_under_different_ids(self):
        cases = [(self.memories[k], outcome) for k, outcome in (
            ("old", "stale_fact_exclusion"), ("irrelevant", "irrelevant_exclusion"),
            ("other_session", "scope_isolation"))] + [(self.memories["noise"][0], "noise_exclusion")]
        for memory, outcome in cases:
            for node in ({"id": memory["memory_id"], "summary": "changed"},
                         {"id": "unexpected", "summary": memory["text"]}):
                with self.subTest(node=node), self.assertRaises(AssertionError) as caught:
                    self.quality.assess_context(self.observation([self.node, node]), self.memories)
                self.assertFalse(caught.exception.args[0][1][outcome])

    def test_empty_wrong_duplicate_and_unknown_context_cannot_pass(self):
        for nodes in ([], [{"id": "corrected", "summary": "wrong"}],
                      [{"id": "wrong", "summary": self.node["summary"]}],
                      [self.node, self.node], [self.node, {"id": "unknown", "summary": "unknown"}]):
            with self.subTest(nodes=nodes), self.assertRaises(AssertionError):
                self.quality.assess_context(self.observation(nodes), self.memories)

    def test_every_observation_must_match_one_durable_model_request(self):
        observed = [self.observation([self.node], f"request-{i}") for i in range(2)]
        requested = [{"request_id": o["request_id"], "turn": {"context": json.loads(o["context_json"])}}
                     for o in observed]
        self.assertEqual(len(self.quality.assess_observations(observed, requested, self.memories)), 2)
        for bad in ([], observed[:1], observed + observed[:1], [observed[0], observed[0]],
                    [observed[0], self.observation([self.node], "unrelated")]):
            with self.subTest(bad=bad), self.assertRaises(AssertionError):
                self.quality.assess_observations(bad, requested, self.memories)
        changed = copy.deepcopy(requested)
        changed[1]["turn"]["context"]["nodes"] = []
        with self.assertRaises(AssertionError):
            self.quality.assess_observations(observed, changed, self.memories)
        with self.assertRaises(AssertionError):
            self.quality.assess_observations([], [], self.memories)

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

    def test_actual_cli_corrects_restarts_and_observes_each_capsule(self):
        # Independent of the new harness: this first fails at the missing driver
        # log, after real CLI saves, exact correction, process restart and runs.
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
        bootstrap = '''
import importlib.util, json, sys, time
from pathlib import Path
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("quality", sys.argv[1])
quality = importlib.util.module_from_spec(spec)
spec.loader.exec_module(quality)
marker = Path(sys.argv[2])
original_start = quality.baseline.Server.start
def start(server):
    original_start(server)
    pending = marker.with_suffix(".pending")
    pending.write_text(json.dumps({"pid": server.process.pid, "root": str(server.root)}))
    pending.replace(marker)
    time.sleep(30)
quality.baseline.Server.start = start
sys.argv = [sys.argv[1], "--output", str(marker.with_suffix(".report")), "--history-size", "4", "--samples", "2"]
quality.main()
'''
        with tempfile.TemporaryDirectory(prefix="ditto-quality-signal-") as temporary:
            marker = Path(temporary) / "ready.json"
            process = subprocess.Popen([sys.executable, "-c", bootstrap,
                                        str(Path(__file__).with_name("personal-quality.py").resolve()), str(marker)],
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
                process.send_signal(signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=10)
                self.assertEqual(process.returncode, 143, (stdout, stderr))
                with self.assertRaises(ProcessLookupError):
                    os.kill(child_pid, 0)
                self.assertFalse(Path(observed["root"]).exists())
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
