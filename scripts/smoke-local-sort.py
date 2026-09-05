#!/usr/bin/env python3
"""Real production daemon/CLI, private disposable data, no provider or credentials."""
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
DAEMON = ROOT / "target/debug/ditto-daemon"
CLI = ROOT / "target/debug/ditto"


def main():
    with tempfile.TemporaryDirectory(prefix="ditto-sort-smoke-") as directory:
        work = Path(directory)
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        api = f"http://127.0.0.1:{port}"
        log = (work / "daemon.log").open("wb")

        def start():
            process = subprocess.Popen(
                [str(DAEMON), "--bind", f"127.0.0.1:{port}", "--data-dir", str(work / "data"),
                 "--capabilities-dir", str(ROOT / "capabilities"), "--provider", "disabled"],
                cwd=work, env={}, stdout=log, stderr=log,
            )
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    assert process.poll() is None, "daemon exited before health"
                    try:
                        get("/health")
                        return process
                    except OSError:
                        time.sleep(0.02)
                raise AssertionError("daemon startup timed out")
            except BaseException:
                stop(process)
                raise

        def stop(process):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                    raise

        def get(path):
            with urllib.request.urlopen(api + path, timeout=5) as response:
                return json.load(response)

        def cli(*args, success=True):
            result = subprocess.run([str(CLI), "--api", api, *map(str, args)], env={},
                                    cwd=work, capture_output=True, timeout=15, check=False)
            assert (result.returncode == 0) == success, result.stderr.decode()
            return json.loads(result.stdout) if success else result.stderr.decode()

        source = work / "list.txt"
        source.write_text("banana\napple\nbanana\n한글", encoding="utf-8")
        request = "01K4C2EV600000000000000003"
        process = start()
        try:
            result = cli("sort", source, "--unique", "--request-id", request)
            assert result["status"] == "verified"
            assert result["output"] == "apple\nbanana\n한글\n"
            assert cli("sort-status", request) == result
            assert cli("sort-cancel", request) == result
            before = get("/health")["durable_events"]
            assert cli("sort", source, "--unique", "--request-id", request) == result
            assert "409" in cli("sort", source, "--request-id", request, success=False)
            assert get("/health")["durable_events"] == before
            # No FIFO open may block before the CLI rejects nonregular input.
            if hasattr(__import__("os"), "mkfifo"):
                import os
                fifo = work / "fifo"
                os.mkfifo(fifo)
                assert "regular file" in cli("sort", fifo, success=False)
            stop(process)
            process = start()
            assert cli("sort-status", request) == result
            assert cli("sort", source, "--unique", "--request-id", request) == result
            assert get("/health")["durable_events"] == before + 1  # runtime.started only
            events = get("/v1/events?limit=100")
            assert sum(event["kind"] == "sort.started" for event in events) == 1
            assert sum(event["kind"] == "task.completed" for event in events) == 1
            assert not any(event["kind"] == "model.requested" for event in events)
            print(json.dumps({"status": "passed", "process_starts": 1, "verified_completions": 1,
                              "model_requests": 0, "restart_retry": "no execution", "output": result["output"]}, ensure_ascii=False))
        finally:
            stop(process)
            log.close()


if __name__ == "__main__":
    main()
