"""Offline accounting, artifact resolution, restart and process cleanup regressions."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch
import sys

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location("baseline", Path(__file__).with_name("personal-baseline.py"))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)


class AccountingTests(unittest.TestCase):
    def test_missing_measurements_are_unknown_not_zero(self):
        self.assertIsNone(baseline.summary([])["p95_ms"])
        self.assertIsNone(baseline.ram(-1)["rss_bytes"])
        result = baseline.accounting([{"kind": "model.requested"}])
        self.assertEqual(result["model_requests"], 1)
        self.assertIsNone(result["live_equivalent_cost_usd"])
        self.assertIsNone(result["model_tokens"])

    def test_offline_spend_and_absence_of_model_work_are_explicit(self):
        result = baseline.accounting([])
        self.assertEqual(result["external_provider_spend_usd"], 0)
        self.assertEqual(result["live_equivalent_cost_usd"], 0)
        self.assertEqual(result["model_tokens"], 0)
        self.assertIn("offline", result["cost_basis"])

    def test_tools_are_counted_at_execution_not_model_intent(self):
        result = baseline.accounting([
            {"kind": "model.requested"}, {"kind": "model.output"},
            {"kind": "capability.requested"}, {"kind": "execution.started"},
            {"kind": "agent.sort.started"}, {"kind": "sort.started"},
        ])
        self.assertEqual(result["tool_executions"], 3)
        self.assertEqual(result["model_requests"], 1)

    def test_percentiles_use_nearest_rank_and_retain_raw_samples(self):
        result = baseline.summary([4, 1, 3, 2])
        self.assertEqual(result["p50_ms"], 2)
        self.assertEqual(result["p95_ms"], 4)
        self.assertEqual(result["samples_ms"], [4, 1, 3, 2])


class ArtifactTests(unittest.TestCase):
    def test_servers_and_cli_execute_resolved_artifact_paths(self):
        with tempfile.TemporaryDirectory(prefix="ditto-artifact-paths-") as temporary:
            root = Path(temporary)
            binaries = {"cli": root / "alternate/cli", "production_daemon": root / "alternate/daemon",
                        "fixture_test_executable": root / "alternate/fixture"}
            for fixture in (False, True):
                server = baseline.Server(root, fixture, binaries)
                with patch.object(baseline.subprocess, "Popen") as spawn, \
                        patch.object(baseline.subprocess, "run") as run, \
                        patch.object(server, "get", return_value={}):
                    spawn.return_value.poll.return_value = None
                    run.return_value = subprocess.CompletedProcess([], 0, "{}", "")
                    try:
                        server.start()
                        server.cli("ping")
                        key = "fixture_test_executable" if fixture else "production_daemon"
                        self.assertEqual(spawn.call_args.args[0][0], str(binaries[key]))
                        self.assertEqual(run.call_args.args[0][0], str(binaries["cli"]))
                    finally:
                        server.stop()

    def test_build_resolves_exact_named_artifacts_in_alternate_target_directory(self):
        with tempfile.TemporaryDirectory(prefix="ditto-alternate-target-") as temporary:
            root = Path(temporary)
            expected = {"cli": root / "debug/ditto", "production_daemon": root / "debug/ditto-daemon",
                        "fixture_test_executable": root / "debug/deps/ditto_daemon-fixture"}

            def artifact(name, path, test=False):
                return {"reason": "compiler-artifact", "target": {"name": name, "kind": ["bin"]},
                        "profile": {"test": test}, "executable": str(path)}

            def cargo(args):
                self.assertIn("--message-format=json", args)
                messages = ([artifact("ditto", expected["cli"]),
                             artifact("ditto-daemon", expected["production_daemon"])]
                            if args[1] == "build" else
                            [artifact("unrelated-test", root / "wrong", True),
                             artifact("ditto-daemon", expected["fixture_test_executable"], True)])
                return subprocess.CompletedProcess(args, 0, "\n".join(map(json.dumps, messages)), "")

            with patch.object(baseline, "execute", side_effect=cargo), patch.dict(os.environ, {"CARGO_TARGET_DIR": temporary}):
                self.assertEqual(baseline.build(), expected)


class ProcessTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binaries = baseline.build()

    def test_due_schedule_and_repeat_cannot_claim_until_fixture_restart(self):
        with tempfile.TemporaryDirectory(prefix="ditto-restart-regression-") as temporary:
            server = baseline.Server(Path(temporary), True, self.binaries)
            server.environment["DITTO_BASELINE_DISABLE_SCHEDULER_DRIVER"] = "1"
            try:
                server.start()
                due = baseline.datetime.now(baseline.timezone.utc) + baseline.timedelta(seconds=1)
                window = ("--at", due.isoformat(timespec="milliseconds"), "--expires",
                          (due + baseline.timedelta(seconds=30)).isoformat(timespec="milliseconds"))
                server.cli("schedule", "scheduled", *window, "--request-id", "01K00000000000000000000901")
                server.cli("repeat", "repeated", *window, "--every-seconds", "60",
                           "--occurrences", "2", "--request-id", "01K00000000000000000000902")
                # Keep the server alive past due; an enabled driver would dispatch.
                time.sleep(max(0, (due - baseline.datetime.now(baseline.timezone.utc)).total_seconds()) + .2)
                one = server.get("/v1/schedules?request_id=01K00000000000000000000901&session_id=personal")
                repeat = server.get("/v1/repeats?request_id=01K00000000000000000000902&session_id=personal")
                self.assertEqual(one["status"], "pending")
                self.assertEqual(one["waiting_for"], "provider_disabled")
                self.assertEqual(repeat["claimed_occurrences"], 0)
                before = server.events()
                claims = {"model.requested", "schedule.claimed", "schedule.occurrence.claimed"}
                self.assertFalse(any(e["kind"] in claims for e in before))
                self.assertFalse((server.data / "fixture-calls.txt").exists())
                boundary = before[-1]["seq"]
                server.stop()
                del server.environment["DITTO_BASELINE_DISABLE_SCHEDULER_DRIVER"]
                server.start()
                server.wait_status("schedules", "01K00000000000000000000901", "unverified")
                deadline = time.monotonic() + 10
                while True:
                    repeat = server.get("/v1/repeats?request_id=01K00000000000000000000902&session_id=personal")
                    if repeat.get("last_occurrence", {}).get("status") == "unverified":
                        break
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.01)
                work = [e for e in server.events() if e["kind"] in claims]
                self.assertEqual(baseline.Counter(e["kind"] for e in work),
                                 {"model.requested": 2, "schedule.claimed": 1, "schedule.occurrence.claimed": 1})
                self.assertTrue(all(e["seq"] > boundary for e in work))
                self.assertEqual(len((server.data / "fixture-calls.txt").read_text().splitlines()), 2)
            finally:
                server.stop()

    def test_sigterm_reaps_server_and_removes_temporary_store(self):
        # Observe each real server at readiness, then terminate the harness during
        # its idle phase. No fake child or mocked cleanup implementation.
        bootstrap = '''
import importlib.util, json, sys
from pathlib import Path
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("baseline", sys.argv[1])
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
marker, fixture = Path(sys.argv[2]), sys.argv[3] == "True"
original_start = baseline.Server.start
def start(server):
    original_start(server)
    pending = marker.with_suffix(".pending")
    pending.write_text(json.dumps({"pid": server.process.pid, "root": str(server.root)}))
    pending.replace(marker)
baseline.Server.start = start
original_workload = baseline.workload
def workload(root, ignored_fixture, *args):
    return original_workload(root, fixture, *args)
baseline.workload = workload
sys.argv = [sys.argv[1], "--output", str(marker.with_suffix(".report")), "--idle-seconds", "60", "--samples", "2"]
baseline.main()
'''
        for fixture in (False, True):
            with self.subTest(fixture=fixture), tempfile.TemporaryDirectory(prefix="ditto-signal-regression-") as temporary:
                marker = Path(temporary) / "ready.json"
                environment = dict(os.environ, TMPDIR=temporary)
                child_pid = None
                process = subprocess.Popen([sys.executable, "-c", bootstrap, str(Path(baseline.__file__)),
                                            str(marker), str(fixture)], env=environment,
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                try:
                    deadline = time.monotonic() + 30
                    while not marker.exists():
                        self.assertIsNone(process.poll(), process.communicate() if process.poll() is not None else "")
                        self.assertLess(time.monotonic(), deadline, "server never became ready")
                        time.sleep(.01)
                    observed = json.loads(marker.read_text())
                    child_pid = observed["pid"]
                    store = Path(observed["root"])
                    self.assertTrue((store / "data/state.db").exists())
                    process.send_signal(signal.SIGTERM)
                    stdout, stderr = process.communicate(timeout=10)
                    self.assertEqual(process.returncode, 128 + signal.SIGTERM, (stdout, stderr))
                    with self.assertRaises(ProcessLookupError):
                        os.kill(child_pid, 0)
                    self.assertFalse(store.exists(), f"temporary store survived SIGTERM: {store}")
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
