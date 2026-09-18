#!/usr/bin/env python3
"""Reproducible offline pre-v0.1 measurements; Python standard library only.

Build with --offline/--locked. Run production-disabled and injected-test servers
separately using disposable storage, a cleared environment, and loopback HTTP.
Never select a live provider. Null means unavailable, never a fabricated zero.
"""
import argparse
from collections import Counter
from datetime import datetime, timedelta, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import socket
import subprocess
import tempfile
import time
import urllib.request

REPO = Path(__file__).resolve().parent.parent


def summary(samples):
    ordered = sorted(samples)
    def percentile(p):
        return ordered[math.ceil(len(ordered) * p) - 1] if ordered else None
    return {"samples_ms": samples, "p50_ms": percentile(.5), "p95_ms": percentile(.95),
            "method": "nearest rank; CLI spawn through terminal output; no warmup discarded"}


def ram(pid):
    result = {"rss_bytes": None, "peak_rss_bytes": None, "reason": "Linux /proc status unavailable"}
    try:
        fields = dict(line.split(":", 1) for line in Path(f"/proc/{pid}/status").read_text().splitlines())
        result.update(rss_bytes=int(fields["VmRSS"].split()[0]) * 1024,
                      peak_rss_bytes=int(fields["VmHWM"].split()[0]) * 1024,
                      reason="Linux VmRSS/VmHWM; server process only, excludes CLI and sort children")
    except (OSError, KeyError, ValueError):
        pass
    return result


def accounting(events):
    kinds = Counter(e["kind"] for e in events)
    calls = kinds["model.requested"]
    return {"model_requests": calls,
            "tool_executions": sum(kinds[k] for k in ("execution.started", "sort.started", "agent.sort.started")),
            "tool_count_basis": "durable dispatch records, not model intent or OS syscall tracing",
            "event_counts": dict(sorted(kinds.items())),
            "model_tokens": None if calls else 0,
            "token_basis": "fixture emits no usage; tokens unavailable for model requests",
            "external_provider_spend_usd": 0,
            "live_equivalent_cost_usd": None if calls else 0,
            "cost_basis": "offline disabled/injected drivers; no external provider calls; no live pricing or usage assumed",
            "total_operating_cost_usd": None}


def execute(args, **kwargs):
    result = subprocess.run(args, cwd=REPO, text=True, capture_output=True, timeout=180, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args}:\n{result.stdout}\n{result.stderr}")
    return result


def build():
    compiled = execute(["cargo", "build", "--offline", "--locked", "-p", "ditto-cli", "-p", "ditto-daemon", "--message-format=json"])
    binaries = {"cli": executable(compiled.stdout, "ditto", False),
                "production_daemon": executable(compiled.stdout, "ditto-daemon", False)}
    compiled = execute(["cargo", "test", "--offline", "--locked", "-p", "ditto-daemon", "--no-run", "--message-format=json"])
    binaries["fixture_test_executable"] = executable(compiled.stdout, "ditto-daemon", True)
    return binaries


def executable(messages, name, test):
    paths = set()
    for line in messages.splitlines():
        item = json.loads(line)
        if (item.get("reason") == "compiler-artifact" and item.get("executable")
                and item["target"]["name"] == name and item["target"]["kind"] == ["bin"]
                and item["profile"]["test"] == test):
            paths.add(Path(item["executable"]).resolve())
    if len(paths) != 1:
        raise RuntimeError(f"expected one executable for {name} (test={test}); found {len(paths)}")
    return paths.pop()


class Server:
    def __init__(self, root, fixture, binaries):
        self.root, self.fixture, self.binaries = root, fixture, binaries
        self.data = root / "data"
        self.process = None
        self.startups = []
        self.latencies = {}
        self.environment = {"PATH": os.defpath, "RUST_LOG": "error"}
        # Disable proxy discovery explicitly for loopback measurement requests.
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def start(self):
        with socket.socket() as reserved:
            reserved.bind(("127.0.0.1", 0))
            bind = f"127.0.0.1:{reserved.getsockname()[1]}"
        self.api = "http://" + bind
        environment = dict(self.environment)
        if self.fixture:
            environment.update(DITTO_BASELINE_DATA=str(self.data), DITTO_BASELINE_BIND=bind)
            command = [str(self.binaries["fixture_test_executable"]), "baseline::fixture_server", "--exact", "--ignored", "--nocapture"]
        else:
            command = [str(self.binaries["production_daemon"]), "--provider", "disabled", "--bind", bind,
                       "--data-dir", str(self.data), "--capabilities-dir", str(REPO / "capabilities")]
        self.log = (self.root / "server.log").open("a")
        start = time.perf_counter()
        self.process = subprocess.Popen(command, env=environment, stdout=self.log, stderr=self.log)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            assert self.process.poll() is None, (self.root / "server.log").read_text()
            try:
                self.get("/health")
                self.startups.append((time.perf_counter() - start) * 1000)
                return
            except OSError:
                time.sleep(.01)
        raise AssertionError("server startup timeout")

    def stop(self):
        if self.process is not None:
            # Abrupt process-loss recovery; no claim about host/power-loss durability.
            self.process.kill()
            self.process.wait(timeout=5)
            self.log.close()
            self.process = None

    def get(self, route):
        with self.http.open(self.api + route, timeout=5) as response:
            return json.load(response)

    def cli(self, *args, success=True, metric=None, human=False):
        start = time.perf_counter()
        result = subprocess.run([str(self.binaries["cli"]), "--api", self.api,
                                 *(["--human"] if human else []), *args],
                                env=self.environment, text=True, capture_output=True, timeout=20)
        elapsed = (time.perf_counter() - start) * 1000
        if metric:
            self.latencies.setdefault(metric, []).append(elapsed)
        assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
        if human:
            assert "\x1b" not in result.stdout + result.stderr
            return result
        return json.loads(result.stdout) if result.stdout else None

    def events(self):
        events, after = [], 0
        while True:
            page = self.get(f"/v1/events?after_seq={after}&limit=500")
            if not page:
                return events
            events.extend(page)
            after = page[-1]["seq"]

    def wait_status(self, kind, identity, state):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            value = self.get(f"/v1/{kind}?request_id={identity}&session_id=personal")
            if value["status"] == state:
                return value
            time.sleep(.01)
        raise AssertionError((kind, identity, value))


def workload(root, fixture, binaries, samples, idle_seconds):
    server = Server(root, fixture, binaries)
    if fixture:
        server.environment["DITTO_BASELINE_DISABLE_SCHEDULER_DRIVER"] = "1"
    identity = lambda number: f"01K000000000000000000{number:05d}"
    evidence = {}
    try:
        server.start()
        startup_ram = ram(server.process.pid)
        before = accounting(server.events())
        time.sleep(idle_seconds)
        idle_ram = ram(server.process.pid)
        after = accounting(server.events())
        assert before["model_requests"] == after["model_requests"] == 0
        assert after["tool_executions"] == 0
        attachment = root / "list.txt"
        attachment.write_text("pear\napple\npear", encoding="utf-8")
        checkpoints = []
        last = None
        for i in range(samples):
            if fixture:
                args = ("run", "Sort the attached list", "--sort-file", str(attachment), "--allow-deduplicate")
                last = server.cli(*args, "--request-id", identity(i), metric="model_sort_cli_ms")
                assert last["status"] == "unverified" and last["sort"]["state"] == "verified"
                assert last["sort"]["output"] == "apple\npear\n"
                kind = "run"
            else:
                args = ("sort", str(attachment), "--unique")
                last = server.cli(*args, "--request-id", identity(i), metric="sort_cli_ms")
                assert last["status"] == "verified" and last["output"] == "apple\npear\n"
                kind = "sort"
            checkpoint = ram(server.process.pid)
            checkpoint["completed_requests"] = i + 1
            checkpoint["durable_events"] = server.get("/health")["durable_events"]
            checkpoints.append(checkpoint)
        repeated_accounting = accounting(server.events())
        assert repeated_accounting["model_requests"] == (2 * samples if fixture else 0)
        assert repeated_accounting["tool_executions"] == samples
        count = server.get("/health")["durable_events"]
        retry = server.cli(*args, "--request-id", identity(samples - 1), metric="exact_retry_cli_ms")
        assert retry == last and count == server.get("/health")["durable_events"]
        human = server.cli(kind + "-status", identity(samples - 1), human=True)
        assert "Sort: verified" in human.stdout
        if fixture:
            assert "Model answer (unverified)" in human.stdout and "deduplication allowed" in human.stdout
        evidence["exact_retry_no_new_events"] = True

        # Real human start paths for every family, including disabled admission.
        server.cli("sort", str(attachment), "--request-id", identity(100), human=True)
        run_human = server.cli("run", "Sort the attached list", "--sort-file", str(attachment),
                              "--request-id", identity(101), human=True, success=fixture)
        assert "Exact file:" in run_human.stderr and "deduplication not allowed" in run_human.stderr
        assert "--session='personal'" in run_human.stderr
        if fixture:
            assert "Model answer (unverified)" in run_human.stdout
        else:
            assert "503" in run_human.stderr

        due = datetime.now(timezone.utc) + timedelta(seconds=2)
        window = ("--at", due.isoformat(timespec="milliseconds"), "--expires",
                  (due + timedelta(seconds=30 if fixture else 1)).isoformat(timespec="milliseconds"))
        schedule = server.cli("schedule", "baseline scheduled answer", *window, "--request-id", identity(200), human=True)
        repeat = server.cli("repeat", "baseline repeated answer", *window, "--every-seconds", "60",
                            "--occurrences", "2", "--request-id", identity(201), human=True)
        assert "Schedule: pending" in schedule.stdout and "Repeat: active" in repeat.stdout
        before_restart = server.events()
        boundary = before_restart[-1]["seq"]
        before_calls = accounting(before_restart)["model_requests"]
        if fixture:
            assert not any(e["kind"] in ("schedule.claimed", "schedule.occurrence.claimed") for e in before_restart)
            assert before_calls == 2 * samples + 2
            assert len((server.data / "fixture-calls.txt").read_text().splitlines()) == before_calls
        server.stop()
        server.environment.pop("DITTO_BASELINE_DISABLE_SCHEDULER_DRIVER", None)
        server.start()
        recovered = server.cli(kind + "-status", identity(samples - 1), metric="recovered_status_cli_ms")
        assert recovered == last
        evidence["terminal_result_survives_restart"] = True
        if fixture:
            server.wait_status("schedules", identity(200), "unverified")
            deadline = time.monotonic() + 15
            while True:
                repeated = server.get(f"/v1/repeats?request_id={identity(201)}&session_id=personal")
                child = repeated.get("last_occurrence", {})
                if child.get("status") == "unverified":
                    break
                assert time.monotonic() < deadline, repeated
                time.sleep(.01)
            assert repeated["claimed_occurrences"] == 1
            # The first process had no scheduler driver. Check new durable work
            # and the independent driver log across that exact restart boundary.
            post_restart = [e for e in server.events() if e["seq"] > boundary]
            claims = Counter(e["kind"] for e in post_restart)
            assert claims["schedule.claimed"] == claims["schedule.occurrence.claimed"] == 1
            assert claims["model.requested"] == 2
            assert len((server.data / "fixture-calls.txt").read_text().splitlines()) == before_calls + 2
            evidence["pending_work_dispatched_after_restart"] = True
            evidence["scheduled_restart_boundary"] = {
                "first_process_scheduler_driver": "disabled (test-only)",
                "last_seq_before_shutdown": boundary,
                "claims_before_shutdown": 0,
                "claims_after_restart": claims["schedule.claimed"] + claims["schedule.occurrence.claimed"],
                "model_requests_after_restart": claims["model.requested"],
                "injected_driver_calls_after_restart": 2,
                "work_event_sequences_after_restart": [e["seq"] for e in post_restart if e["kind"] in
                    ("schedule.claimed", "schedule.occurrence.claimed", "model.requested")],
            }
        else:
            server.wait_status("schedules", identity(200), "missed")
            evidence["disabled_schedule_expired_without_model"] = True
        server.cli("repeat-cancel", identity(201), human=True)
        server.cli("schedule-status", identity(200), human=True, success=fixture)

        if fixture:
            server.cli("run", "baseline block", "--request-id", identity(300), "--detach")
            deadline = time.monotonic() + 5
            blocked_task = server.cli("run-status", identity(300))["task_id"]
            while not any(e["kind"] == "model.requested" and e.get("task_id") == blocked_task
                          for e in server.events()):
                assert time.monotonic() < deadline
                time.sleep(.01)
            requested = accounting(server.events())["model_requests"]
            while len((server.data / "fixture-calls.txt").read_text().splitlines()) != requested:
                assert time.monotonic() < deadline, "fixture call did not enter the blocking stream"
                time.sleep(.01)
            server.stop()
            server.start()
            interrupted = server.cli("run-status", identity(300), success=False)
            assert interrupted["status"] == "interrupted"
            count = server.get("/health")["durable_events"]
            retry = server.cli("run", "baseline block", "--request-id", identity(300), success=False)
            assert retry == interrupted and server.get("/health")["durable_events"] == count
            evidence["active_owner_loss_interrupted_without_retry"] = True
        else:
            server.stop()
            server.start()
        cancelled = server.cli("repeat-status", identity(201), human=True)
        assert "Repeat: cancelled" in cancelled.stdout
        evidence["repeat_cancellation_survives_restart"] = True
        events = server.events()
        counts = accounting(events)
        if fixture:
            actual_calls = len((server.data / "fixture-calls.txt").read_text().splitlines())
            assert actual_calls == counts["model_requests"]
            counts["injected_driver_calls"] = actual_calls
        else:
            assert counts["model_requests"] == 0
        return {"label": "fixture-backed test server" if fixture else "production daemon; provider disabled",
                "startup_and_recovery_ms": server.startups,
                "startup_timing_basis": "spawn to first successful health; 10 ms polling; includes HTTP readiness probe",
                "idle_seconds": idle_seconds, "startup_ram": startup_ram, "idle_ram": idle_ram,
                "startup_model_calls": before["model_requests"], "startup_tool_calls": before["tool_executions"],
                "idle_model_calls": after["model_requests"] - before["model_requests"],
                "idle_tool_calls": after["tool_executions"] - before["tool_executions"],
                "repeated_use_ram": checkpoints, "final_ram": ram(server.process.pid),
                "latency": {name: summary(values) for name, values in server.latencies.items()},
                "repeated_work_accounting": repeated_accounting, "whole_workload_accounting": counts,
                "storage_bytes": sum(p.stat().st_size for p in server.data.rglob("*") if p.is_file()),
                "verified_sort_requests": samples, "model_answer_quality": None,
                "first_useful_progress_ms": None, "isolated_model_time_ms": None, "isolated_tool_time_ms": None,
                "ditto_only_overhead_ms": None,
                "unavailable_basis": "no live model/usage/prices or isolated timing instrumentation; CLI exposes terminal answers; no quality inference",
                "recovery_evidence": evidence}
    finally:
        server.stop()


def terminate(signum, _frame):
    # Exit through workload's finally and TemporaryDirectory's context cleanup.
    # Repeated SIGTERM must not interrupt that cleanup.
    signal.signal(signum, signal.SIG_IGN)
    raise SystemExit(128 + signum)


def main():
    signal.signal(signal.SIGTERM, terminate)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=12)
    parser.add_argument("--idle-seconds", type=float, default=1)
    args = parser.parse_args()
    if not 2 <= args.samples <= 100 or not .05 <= args.idle_seconds <= 60:
        parser.error("samples must be 2..100 and idle-seconds .05..60")
    binaries = build()
    sources = sorted(p for base in ("apps", "crates", "capabilities", "scripts")
                     for p in (REPO / base).rglob("*") if p.is_file() and
                     (p.suffix in (".rs", ".toml", ".py", ".sh", ".json") or p.name == "Cargo.lock"))
    sources += [REPO / "Cargo.lock", REPO / "Cargo.toml"]
    digests = {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sources}
    report = {"schema": 1, "workload": "task014-offline-personal-v1", "utc": datetime.now(timezone.utc).isoformat(),
              "samples": args.samples, "platform": platform.platform(), "machine": platform.machine(),
              "cpu_count": os.cpu_count(), "rustc": execute(["rustc", "--version"]).stdout.strip(),
              "python": platform.python_version(), "build_profile": "debug, default features, offline locked",
              "git_head": execute(["git", "rev-parse", "HEAD"]).stdout.strip(),
              "source_sha256": digests,
              "model_settings": "disabled production / deterministic injected zero-delay sort-then-answer fixture; usage absent; no credentials",
              "limits": "small fixed catalogue/history; no long-use quality, learning, live-provider, child-process peak RAM or comparative performance claim",
              "results": []}
    report["hardware"] = {"memory_total_bytes": None, "cpu_description": None}
    try:
        memory = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
        report["hardware"]["memory_total_bytes"] = int(memory["MemTotal"].split()[0]) * 1024
        cpu = dict(line.split(":", 1) for line in Path("/proc/cpuinfo").read_text().splitlines() if ":" in line)
        cpu = {k.strip(): v.strip() for k, v in cpu.items()}
        report["hardware"]["cpu_description"] = cpu.get("model name", cpu.get("Hardware", cpu.get("CPU part")))
    except (OSError, KeyError, ValueError):
        pass
    report["capability_packages"] = len(list((REPO / "capabilities").rglob("capability.toml")))
    report["binary_resolution"] = "exact named compiler-artifact executables from Cargo JSON; inherited target configuration"
    report["binary_sha256"] = {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}
    for fixture in (False, True):
        with tempfile.TemporaryDirectory(prefix="ditto-personal-baseline-") as temporary:
            report["results"].append(workload(Path(temporary), fixture, binaries, args.samples, args.idle_seconds))
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"Offline baseline passed; report: {args.output}")


if __name__ == "__main__":
    main()
