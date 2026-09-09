#!/usr/bin/env python3
"""Built daemon/CLI checks with disposable storage and no provider credentials."""
import json
from datetime import datetime, timedelta, timezone
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request


def main():
    repo = Path(__file__).resolve().parent.parent
    cli = repo / "target/debug/ditto"
    daemon = repo / "target/debug/ditto-daemon"
    assert cli.is_file() and daemon.is_file(), "build both applications first"
    environment = {"PATH": os.environ.get("PATH", ""), "RUST_LOG": "error"}
    with tempfile.TemporaryDirectory(prefix="ditto-run-smoke-") as temporary:
        root = Path(temporary)
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        api = f"http://127.0.0.1:{port}"
        process = None

        def run(*args, success=True):
            result = subprocess.run([str(cli), "--api", api, *args], env=environment,
                                    text=True, capture_output=True, timeout=10)
            assert (result.returncode == 0) == success, result.stderr
            return result

        def health():
            with urllib.request.urlopen(api + "/health", timeout=2) as response:
                return json.load(response)

        def start():
            nonlocal process
            process = subprocess.Popen([str(daemon), "--bind", f"127.0.0.1:{port}",
                "--data-dir", str(root / "data"), "--capabilities-dir", str(repo / "capabilities")],
                env=environment, stdout=log, stderr=log)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                assert process.poll() is None, "daemon exited during startup"
                try:
                    health()
                    return
                except OSError:
                    time.sleep(0.05)
            raise AssertionError("daemon did not become ready")

        def stop():
            nonlocal process
            if process is None:
                return
            process.terminate()
            try:
                assert process.wait(timeout=5) == 0, "daemon shutdown failed"
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
                raise AssertionError("daemon did not drain on SIGTERM")
            finally:
                process = None

        with (root / "daemon.log").open("w") as log:
            try:
                start()
                before = health()["durable_events"]
                request = "01K00000000000000000000004"
                disabled = run("run", "hello", "--request-id", request, success=False)
                assert request in disabled.stderr and "503" in disabled.stderr
                assert health()["durable_events"] == before
                attachment = root / "list.txt"
                attachment.write_text("b\na\nb", encoding="utf-8")
                disabled_sort = run("run", "sort attachment", "--sort-file", str(attachment),
                                    "--allow-deduplicate", "--request-id", request, success=False)
                assert "503" in disabled_sort.stderr and "sort attached file once" in disabled_sort.stderr
                assert health()["durable_events"] == before
                assert "404" in run("run-status", request, success=False).stderr
                assert "404" in run("run-cancel", request, success=False).stderr
                run("input", "record only", "--session", "personal")
                assert health()["durable_events"] == before + 1
                run("memory", "save", "meeting preference is morning")
                saved = json.loads(run("memory", "list").stdout)["memories"]
                # Keep an actual SSE connection open during SIGTERM. Shutdown
                # must close followers, not hang waiting on an infinite stream.
                stream = urllib.request.urlopen(api + "/v1/stream?session_id=personal", timeout=5)
                assert stream.readline(), "event stream did not deliver replay"
                stop()
                stream.close()
                start()
                assert json.loads(run("memory", "list").stdout)["memories"] == saved
                assert "404" in run("run-status", request, success=False).stderr
                schedule_id = "01K00000000000000000000024"
                due = datetime.now(timezone.utc) + timedelta(seconds=1)
                expires = due + timedelta(seconds=1)
                schedule_args = ("schedule", "future read-only request", "--request-id", schedule_id,
                    "--at", due.isoformat(timespec="milliseconds"),
                    "--expires", expires.isoformat(timespec="milliseconds"))
                accepted = json.loads(run(*schedule_args).stdout)
                assert accepted["status"] == "pending"
                assert accepted["waiting_for"] == "provider_disabled"
                assert len(json.loads(run("schedule-list").stdout)) == 1
                repeat_id = "01K00000000000000000000025"
                repeat_args = ("repeat", "repeated read-only request", "--request-id", repeat_id,
                    "--at", due.isoformat(timespec="milliseconds"),
                    "--expires", expires.isoformat(timespec="milliseconds"),
                    "--every-seconds", "60", "--occurrences", "1000")
                repeated = json.loads(run(*repeat_args).stdout)
                assert repeated["status"] == "active" and repeated["claimed_occurrences"] == 0
                assert repeated["waiting_for"] == "provider_disabled"
                stop()
                time.sleep(max(0, expires.timestamp() - time.time()) + 0.05)
                start()
                deadline = time.monotonic() + 5
                while True:
                    result = subprocess.run([str(cli), "--api", api, "schedule-status", schedule_id],
                        env=environment, text=True, capture_output=True, timeout=10)
                    status = json.loads(result.stdout)
                    if status["status"] == "missed":
                        assert result.returncode != 0
                        break
                    assert time.monotonic() < deadline, status
                    time.sleep(0.01)
                assert json.loads(run(*schedule_args, success=False).stdout)["status"] == "missed"
                assert json.loads(run("schedule-list").stdout) == []
                deadline = time.monotonic() + 5
                while True:
                    repeated = json.loads(run("repeat-status", repeat_id).stdout)
                    if repeated["missed_occurrences"] == 1:
                        break
                    assert time.monotonic() < deadline, repeated
                    time.sleep(0.01)
                assert repeated["status"] == "active" and repeated["next_occurrence"] == 2
                assert repeated["claimed_occurrences"] == 0 and "last_occurrence_id" not in repeated
                before_retry = health()["durable_events"]
                assert json.loads(run(*repeat_args).stdout) == repeated
                assert health()["durable_events"] == before_retry
                assert len(json.loads(run("repeat-list").stdout)) == 1
                cancelled = json.loads(run("repeat-cancel", repeat_id).stdout)
                assert cancelled["status"] == "cancelled" and "next_due_at" not in cancelled
                assert json.loads(run("repeat-list").stdout) == []
                stop()
                start()
                assert json.loads(run("repeat-status", repeat_id).stdout) == cancelled
                events = json.loads(run("events", "--limit", "1000").stdout)
                assert not any(event["kind"] == "model.requested" for event in events)
                assert not any(event["kind"] == "schedule.occurrence.claimed" for event in events)
                skipped = [event for event in events if event["kind"] == "schedule.repeat.skipped"]
                assert len(skipped) == 1
                assert skipped[0]["payload"]["from_occurrence"] == skipped[0]["payload"]["through_occurrence"] == 1
                stop()
                print(json.dumps({"result": "passed", "scenarios": [
                    "disabled run has no durable admission", "disabled sort attachment has no artifact or admission", "recoverable CLI request ID",
                    "missing run inspection/cancellation", "record-only input",
                    "SIGTERM with open SSE follower", "restart preserves memory and does not run rejected work",
                    "disabled schedule survives downtime and expires without model calls", "expired schedule retry does not requeue",
                    "repeat skips expired work without child creation or model calls", "repeat cancellation survives restart"
                ]}))
            finally:
                stop()


if __name__ == "__main__":
    main()
