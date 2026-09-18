#!/usr/bin/env python3
"""Offline lexical context selection after correction/restart, at two history sizes.

Only synthetic data, actual CLI/HTTP/kernel, and the cfg(test) fixture driver.
No live provider, model-answer assessment, semantic retrieval or speed claim.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import signal
import sys
import tempfile
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("baseline", Path(__file__).with_name("personal-baseline.py"))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
REPO = baseline.REPO
QUERY = "cedar timezone"


def assess_context(observation, memories):
    """Fail closed on missing/extra/wrong context, independently of fixture text."""
    nodes = json.loads(observation["context_json"])["nodes"]
    corrected = memories["corrected"]

    def excluded(memory):
        return all(n["id"] != memory["memory_id"] and memory["text"] not in n["summary"] for n in nodes)

    outcomes = {
        "correction_recall": any(n["id"] == corrected["memory_id"] and n["summary"] == corrected["text"] for n in nodes),
        "stale_fact_exclusion": excluded(memories["old"]),
        "irrelevant_exclusion": excluded(memories["irrelevant"]),
        "scope_isolation": excluded(memories["other_session"]),
        "noise_exclusion": all(excluded(memory) for memory in memories["noise"]),
        "only_expected_context": len(nodes) == 1 and nodes[0]["id"] == corrected["memory_id"],
    }
    assert all(outcomes.values()), (observation["request_id"], outcomes)
    return {"outcomes": outcomes, "selected_node_count": len(nodes),
            "serialized_bytes": len(observation["context_json"].encode("utf-8"))}


def assess_observations(observations, requested, memories):
    ids = [r["request_id"] for r in requested]
    assert ids and len(set(ids)) == len(ids), "missing or duplicate durable model requests"
    assert [o["request_id"] for o in observations] == ids, "missing, duplicate, reordered or unrelated observations"
    results = []
    for observation, request in zip(observations, requested):
        assert json.loads(observation["context_json"]) == request["turn"]["context"], "driver/journal capsule mismatch"
        results.append({**observation, **assess_context(observation, memories)})
    return results


def checkpoint(server):
    files = {str(p.relative_to(server.data)): p.stat().st_size
             for p in sorted(server.data.rglob("*")) if p.is_file()}
    instrumentation = sum(size for name, size in files.items() if name.startswith("fixture-"))
    return {"ram": baseline.ram(server.process.pid), "durable_events": server.get("/health")["durable_events"],
            "storage_bytes": sum(files.values()), "storage_files_bytes": files,
            "fixture_log_bytes": instrumentation, "durable_storage_bytes": sum(files.values()) - instrumentation}


def workload(root, binaries, noise_count, samples):
    server = baseline.Server(root, True, binaries)
    server.environment["DITTO_BASELINE_OBSERVE_CONTEXT"] = "1"
    try:
        server.start()
        initial = checkpoint(server)
        assert initial["durable_events"] == 0
        started = time.perf_counter()

        def save(text, session="personal", replaces=None):
            args = ["memory", "save", text, "--session", session]
            if replaces:
                args += ["--replaces", replaces]
            result = server.cli(*args)
            assert result["outcome"] == "recorded", result
            return {**result, "text": text, "session": session, "replaces": replaces}

        memories = {"old": save("cedar timezone is UTC")}
        memories["corrected"] = save("cedar timezone is KST", replaces=memories["old"]["memory_id"])
        memories["irrelevant"] = save("bananas ripen tomorrow")
        memories["other_session"] = save("cedar timezone is PRIVATE", session="elsewhere")
        memories["noise"] = [save(f"synthetic unrelated pebble {i:06d}") for i in range(noise_count)]
        seed_ms = (time.perf_counter() - started) * 1000
        after_seed = checkpoint(server)
        seed_accounting = baseline.accounting(server.events())
        assert seed_accounting["model_requests"] == seed_accounting["tool_executions"] == 0
        assert seed_accounting["event_counts"] == {"input.received": noise_count + 4, "context.node.recorded": noise_count + 4}
        assert not (server.data / "fixture-calls.txt").exists()
        assert not (server.data / "fixture-contexts.jsonl").exists()
        boundary = after_seed["durable_events"]
        server.stop()
        server.start()
        after_restart = checkpoint(server)
        assert after_restart["durable_events"] == boundary

        # Inspect the recovered public memory path, including all ID pages. This
        # is outside query latency; the restarted process is therefore not cold.
        started = time.perf_counter()
        for session in ("personal", "elsewhere"):
            found, cursor = {}, None
            while True:
                args = ["memory", "list", "--session", session, "--limit", "100"]
                if cursor:
                    args += ["--after-id", cursor]
                page = server.cli(*args)
                for memory in page["memories"]:
                    assert memory["id"] not in found
                    found[memory["id"]] = memory["text"]
                next_cursor = page["next_after_id"]
                if next_cursor is None:
                    break
                assert cursor is None or next_cursor > cursor
                cursor = next_cursor
            active = [memories[k] for k in ("corrected", "irrelevant", "other_session")] + memories["noise"]
            assert found == {m["memory_id"]: m["text"] for m in active if m["session"] == session}
        listing_ms = (time.perf_counter() - started) * 1000
        assert server.get("/health")["durable_events"] == boundary

        runs, query_checkpoints = [], []
        for i in range(samples):
            identity = f"01K000000000000000002{i:05d}"
            result = server.cli("run", QUERY, "--request-id", identity, metric="query_cli_ms")
            assert result["status"] == "unverified" and result["request_id"] == identity, result
            runs.append({"client_request_id": identity, "task_id": result["task_id"], "status": result["status"]})
            query_checkpoints.append(checkpoint(server))
        events = server.events()
        counts = baseline.accounting(events)
        model_events = [e for e in events if e["kind"] == "model.requested"]
        assert len(model_events) == samples and all(e["seq"] > boundary for e in model_events)
        assert [e["task_id"] for e in model_events] == [r["task_id"] for r in runs]
        assert all(e["session_id"] == "personal" for e in model_events)
        requested = [e["payload"]["request"] for e in model_events]
        calls = (server.data / "fixture-calls.txt").read_text().splitlines()
        assert calls == [r["request_id"] for r in requested]
        observations = [json.loads(line) for line in (server.data / "fixture-contexts.jsonl").read_text().splitlines()]
        assessed = assess_observations(observations, requested, memories)
        for run, event, observation in zip(runs, model_events, assessed):
            run.update(observation, model_event_seq=event["seq"])
            node = json.loads(observation["context_json"])["nodes"][0]
            assert (node["scope"], node["origin"], node["epistemic"]) == ("session", "user", "asserted")
            assert node["source_event_ids"] == [memories["corrected"]["memory_id"].removeprefix("memory-").upper()]
        assert counts["model_requests"] == samples and counts["tool_executions"] == 0
        assert counts["event_counts"].get("task.completed", 0) == 0
        counts["injected_driver_calls"] = len(calls)
        return {
            "profile": "minimal_history" if noise_count == 0 else "longer_history",
            "unrelated_memories": noise_count, "saved_memories_including_superseded": noise_count + 4,
            "query": QUERY, "samples": samples, "memories": memories, "seed_duration_ms": seed_ms,
            "startup_and_recovery_ms": server.startups, "initial": initial, "after_seed": after_seed,
            "after_restart": after_restart, "recovered_memory_listing_ms": listing_ms,
            "recovery": {"last_seq_before_restart": boundary, "no_new_events_on_reopen_or_listing": True,
                         "all_active_memories_recovered": True, "queries_after_restart": samples},
            "after_each_query": query_checkpoints, "latency": baseline.summary(server.latencies["query_cli_ms"]),
            "requests": runs, "seed_accounting": seed_accounting, "whole_workload_accounting": counts,
            "outcomes": {name: all(r["outcomes"][name] for r in runs) for name in runs[0]["outcomes"]},
            "model_answer_quality": None, "first_useful_progress_ms": None, "isolated_model_time_ms": None,
            "isolated_tool_time_ms": None, "ditto_only_overhead_ms": None,
            "unavailable_basis": "fixed fixture answer is not assessed; no live usage/prices or isolated timing instrumentation",
        }
    finally:
        server.stop()


def source_hashes():
    sources = {p for base in ("apps", "crates", "capabilities", "scripts", ".cargo")
               for p in (REPO / base).rglob("*") if p.is_file() and p.suffix in (".rs", ".toml", ".py", ".sh", ".json")}
    sources.update(REPO / name for name in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml"))
    return {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(sources)}


def main():
    signal.signal(signal.SIGTERM, baseline.terminate)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--history-size", type=int, default=1000, help="unrelated memories in longer profile (1..5000)")
    parser.add_argument("--samples", type=int, default=5, help="unique queries per profile (2..100)")
    args = parser.parse_args()
    if not 1 <= args.history_size <= 5000 or not 2 <= args.samples <= 100:
        parser.error("history-size must be 1..5000 and samples 2..100")
    if not __debug__:
        parser.error("Python optimization disables measurement assertions; run without -O")
    sources = source_hashes()
    binaries = baseline.build()
    binary_hashes = {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}
    hardware = {"memory_total_bytes": None, "cpu_description": None}
    try:
        memory = dict(line.split(":", 1) for line in Path("/proc/meminfo").read_text().splitlines())
        hardware["memory_total_bytes"] = int(memory["MemTotal"].split()[0]) * 1024
        cpu = dict(line.split(":", 1) for line in Path("/proc/cpuinfo").read_text().splitlines() if ":" in line)
        cpu = {k.strip(): v.strip() for k, v in cpu.items()}
        hardware["cpu_description"] = cpu.get("model name", cpu.get("Hardware", cpu.get("CPU part")))
    except (OSError, KeyError, ValueError):
        pass
    report = {
        "schema": 1, "workload": "task015-offline-context-history-v1", "utc": datetime.now(timezone.utc).isoformat(),
        "history_size": args.history_size, "samples_per_profile": args.samples,
        "platform": platform.platform(), "machine": platform.machine(), "cpu_count": os.cpu_count(), "hardware": hardware,
        "rustc": baseline.execute(["rustc", "--version", "--verbose"]).stdout.strip(),
        "cargo": baseline.execute(["cargo", "--version"]).stdout.strip(), "python": platform.python_version(),
        "build_profile": "debug, default features, offline locked; exact Task 014 Cargo JSON artifact resolver",
        "build_settings": {key: os.environ.get(key) for key in ("CARGO_BUILD_JOBS", "CARGO_BUILD_TARGET", "RUSTFLAGS")},
        "git_head": baseline.execute(["git", "rev-parse", "HEAD"]).stdout.strip(),
        "source_identity_basis": "source digests include uncommitted files; documentation/reports excluded; checked unchanged after workload",
        "source_sha256": sources, "binary_sha256": binary_hashes,
        "binary_paths_relative_to_repo": {name: os.path.relpath(path, REPO) for name, path in binaries.items()},
        "executed_binaries": ["cli", "fixture_test_executable"],
        "capability_packages": len(list((REPO / "capabilities").rglob("capability.toml"))),
        "model_settings": "cfg(test) offline-baseline-fixture; zero-delay fixed answer; no usage; no production provider selection",
        "measurement_basis": {
            "startup": "spawn to health readiness; 10 ms polling; recovery after abrupt process kill, not power loss",
            "latency": "CLI spawn through terminal JSON; no warmup discarded; recovered memory listing precedes queries",
            "context": "exact serde_json ContextCapsule bytes captured by test driver, reconciled with durable model.requested",
            "storage": "live logical file sizes including WAL/SHM; fixture logs separately counted; excludes server stdout log",
            "comparison": "sequential fresh stores: 0 then N unrelated session memories; same facts, query and sample count",
        },
        "limits": "one synthetic lexical query; no general/model-answer quality, semantic retrieval, cross-session recall, live tokens/cost, superiority or v0.1 readiness claim",
        "results": [],
    }
    for noise_count in (0, args.history_size):
        with tempfile.TemporaryDirectory(prefix="ditto-personal-quality-") as temporary:
            report["results"].append(workload(Path(temporary), binaries, noise_count, args.samples))
    assert sources == source_hashes(), "source drift during measurement; rerun"
    assert binary_hashes == {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}, "binary drift during measurement"
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"Offline context/history workload passed; report: {args.output}")


if __name__ == "__main__":
    main()
