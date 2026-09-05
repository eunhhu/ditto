#!/usr/bin/env python3
"""Exercise the built CLI and daemon using only disposable local data."""
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
import time
import urllib.request


def main():
    repo = Path(__file__).resolve().parent.parent
    daemon = repo / "target/debug/ditto-daemon"
    cli = repo / "target/debug/ditto"
    assert daemon.is_file() and cli.is_file(), "build ditto-daemon and ditto-cli first"
    with tempfile.TemporaryDirectory(prefix="ditto-memory-smoke-") as temporary:
        root = Path(temporary).resolve()
        data = root / "data"
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            port = reserved.getsockname()[1]
        api = f"http://127.0.0.1:{port}"
        environment = {"PATH": os.environ.get("PATH", ""), "RUST_LOG": "error"}
        process = None

        def http(path):
            with urllib.request.urlopen(api + path, timeout=5) as response:
                return json.load(response)

        def run(*arguments, success=True):
            result = subprocess.run(
                [str(cli), "--api", api, *arguments], env=environment,
                capture_output=True, text=True, timeout=10,
            )
            assert (result.returncode == 0) == success, result.stderr
            return json.loads(result.stdout) if success else result.stderr

        def start():
            nonlocal process
            process = subprocess.Popen(
                [str(daemon), "--bind", f"127.0.0.1:{port}", "--data-dir", str(data),
                 "--capabilities-dir", str(repo / "capabilities")],
                env=environment, stdout=log, stderr=log,
            )
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                assert process.poll() is None, "daemon exited; inspect smoke log"
                try:
                    http("/health")
                    return
                except OSError:
                    time.sleep(0.05)
            raise AssertionError("daemon did not become ready")

        def stop():
            nonlocal process
            if process is not None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                process = None

        with (root / "daemon.log").open("w") as log:
            try:
                start()
                first = run("memory", "save", "오전 8시에 업무를 시작한다")
                assert first["outcome"] == "recorded"
                current = run("memory", "save", "오전 9시에 업무를 시작한다", "--replaces", first["memory_id"])
                page = run("memory", "list")
                assert len(page["memories"]) == 1
                memory = page["memories"][0]
                assert memory["text"] == "오전 9시에 업무를 시작한다"
                retry = run("memory", "from-input", memory["input_event_id"], "--replaces", first["memory_id"])
                assert retry["event_id"] == current["event_id"] and retry["outcome"] == "already_recorded"
                assert run("memory", "list", "--session", "separate")["memories"] == []

                failure = run("memory", "save", "복구한 정정 내용", "--replaces", first["memory_id"], success=False)
                captured = re.search(r"input ([0-9A-Z]{26}) was captured", failure)
                assert captured and "from-input" in failure
                recovered = run("memory", "from-input", captured[1], "--replaces", current["memory_id"])
                assert recovered["outcome"] == "recorded"
                run("memory", "save", "é" * 2048)
                before = http("/health")["durable_events"]
                run("memory", "save", "é" * 2048 + "a", success=False)
                assert http("/health")["durable_events"] == before
                expected = run("memory", "list")["memories"]
                first_page = run("memory", "list", "--limit", "1")
                last_page = run("memory", "list", "--limit", "1", "--after-id", first_page["next_after_id"])
                assert first_page["memories"] + last_page["memories"] == expected
                stop()
                for suffix in ("", "-wal", "-shm"):
                    (data / f"context-projection.db{suffix}").unlink(missing_ok=True)
                start()
                assert run("memory", "list")["memories"] == expected
                print(json.dumps({
                    "result": "passed", "active_memories": len(expected),
                    "scenarios": ["save", "correct", "idempotent retry", "scope isolation",
                                  "partial-input recovery", "exact UTF-8 limit", "paging",
                                  "restart and projection rebuild"],
                }))
            finally:
                stop()


if __name__ == "__main__":
    main()
