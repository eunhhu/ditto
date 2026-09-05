#!/usr/bin/env python3
"""Built daemon/CLI checks with disposable storage and no provider credentials."""
import json
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
                stop()
                print(json.dumps({"result": "passed", "scenarios": [
                    "disabled run has no durable admission", "recoverable CLI request ID",
                    "missing run inspection/cancellation", "record-only input",
                    "SIGTERM with open SSE follower", "restart preserves memory and does not run rejected work"
                ]}))
            finally:
                stop()


if __name__ == "__main__":
    main()
