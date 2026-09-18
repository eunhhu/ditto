#!/usr/bin/env python3
"""Offline lexical context selection after correction/restart, at two history sizes.

Only synthetic data, actual CLI/HTTP/kernel, and the cfg(test) fixture driver.
No live provider, model-answer assessment, semantic retrieval or speed claim.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import signal
import sys
import tempfile
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("baseline", Path(__file__).with_name("personal-baseline.py"))
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)
REPO = baseline.REPO
# Frozen independently of runtime capsules and fixture answers (Task 016).
CORPUS = {
    "session": "personal", "recall_k": 2,
    "seeds": [
        {"label": "OLD", "session": "personal", "text": "cedar timezone is UTC"},
        {"label": "CURRENT", "session": "personal", "text": "cedar timezone is KST", "replaces": "OLD"},
        {"label": "PACKING", "session": "personal", "text": "harbor packing checklist includes passport"},
        {"label": "TRAIN", "session": "personal", "text": "harbor train departs Friday"},
        {"label": "FOOD", "session": "personal", "text": "supper preference is vegetarian"},
        {"label": "IRRELEVANT", "session": "personal", "text": "bananas ripen tomorrow"},
        {"label": "OTHER", "session": "elsewhere", "text": "cedar timezone is PRIVATE"},
    ],
    "noise": {"label": "NOISE[i]", "session": "personal", "text": "synthetic unrelated pebble {i:06d}",
              "indices": "0 <= i < profile unrelated_memories"},
    "cases": [
        {"case": "corrected_timezone", "query": "cedar timezone", "expected": ["CURRENT"]},
        {"case": "trip_packing", "query": "harbor packing", "expected": ["PACKING", "TRAIN"]},
        {"case": "trip_train", "query": "harbor train", "expected": ["TRAIN", "PACKING"]},
        {"case": "meal_preference", "query": "supper preference", "expected": ["FOOD"]},
        {"case": "no_match", "query": "observatory telescope", "expected": []},
    ],
    "exclusions": {"stale": ["OLD"], "scope": ["OTHER"],
                   "irrelevant": "CURRENT, PACKING, TRAIN, FOOD, IRRELEVANT minus expected labels",
                   "noise": "all NOISE[i]"},
}
NODE_FIELDS = ("id", "kind", "summary", "origin", "epistemic", "scope", "confidence", "source_event_ids")


class AssessmentFailure(AssertionError):
    def __init__(self, diagnostics):
        self.diagnostics = diagnostics
        super().__init__(diagnostics)


def require(condition, message):
    if not condition:
        raise AssessmentFailure({"error": message})


def strict_json(text):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    try:
        return json.loads(text, object_pairs_hook=unique,
                          parse_constant=lambda value: require(False, "nonfinite JSON number: " + value))
    except (ValueError, TypeError) as error:
        raise AssessmentFailure({"error": "malformed JSON", "detail": str(error)}) from error


def metric(numerator, denominator, contributors, empty_reason):
    return {"numerator": numerator, "denominator": denominator,
            "value": numerator / denominator if denominator else None,
            "null_reason": None if denominator else empty_reason, "contributors": contributors}


def assess_context(observation, expected, forbidden):
    """Measure exact independently supplied items; diagnostics survive any failure."""
    capsule = strict_json(observation.get("context_json"))
    require(isinstance(capsule, dict) and set(capsule) == {"nodes"}
            and isinstance(capsule["nodes"], list), "malformed capsule")
    nodes = capsule["nodes"]
    expected_ids = [node["id"] for node in expected]
    require(len(set(expected_ids)) == len(expected_ids), "duplicate expected identity")
    by_id = {node["id"]: node for node in expected}
    ids, valid, malformed, mismatched = [], [], [], []
    for rank, node in enumerate(nodes, 1):
        if isinstance(node, dict) and isinstance(node.get("id"), str):
            ids.append(node["id"])
        well_formed = (isinstance(node, dict) and set(node) == set(NODE_FIELDS)
                       and all(isinstance(node[k], str) for k in NODE_FIELDS[:6])
                       and type(node["confidence"]) in (int, float)
                       and isinstance(node["source_event_ids"], list)
                       and all(isinstance(v, str) for v in node["source_event_ids"]))
        if not well_formed:
            malformed.append({"rank": rank, "node": node})
        elif node["id"] in by_id:
            if node == by_id[node["id"]]:
                valid.append({"id": node["id"], "rank": rank})
            else:
                mismatched.append({"rank": rank, "node": node, "expected": by_id[node["id"]]})
    valid_ids = {item["id"] for item in valid}
    duplicates = sorted(identity for identity, count in Counter(ids).items() if count > 1)
    exact_set = (not malformed and not mismatched and not duplicates
                 and set(ids) == set(expected_ids) and len(nodes) == len(expected))
    exact_order = exact_set and ids == expected_ids
    metrics = {
        "exact_set": metric(int(exact_set), 1, valid, "no assessed request"),
        "exact_order": metric(int(exact_order), 1, valid, "no assessed request"),
        "nontrivial_order": metric(int(exact_order and len(expected) > 1), int(len(expected) > 1),
                                   valid if len(expected) > 1 else [], "expected cardinality below two"),
        "recall_at_2": metric(len({v["id"] for v in valid if v["rank"] <= 2}), len(expected),
                              [v for v in valid if v["rank"] <= 2], "empty expected context"),
        "returned_context_precision": metric(len(valid_ids), len(nodes), valid, "empty returned context"),
    }
    for category, items in forbidden.items():
        leaks = []
        for item in items:
            ranks = [rank for rank, node in enumerate(nodes, 1) if isinstance(node, dict)
                     and (node.get("id") == item["id"] or
                          isinstance(node.get("summary"), str) and item["summary"] in node["summary"])]
            if ranks:
                leaks.append({"id": item["id"], "ranks": ranks})
        metrics[category + "_leaks"] = metric(len(leaks), len(items), leaks, "no forbidden memories in category")
    result = {"passed": exact_order and all(metrics[k + "_leaks"]["numerator"] == 0 for k in forbidden),
              "metrics": metrics, "expected_items": expected,
              "missing_ids": [identity for identity in expected_ids if identity not in valid_ids],
              "extra_ids": [identity for identity in ids if identity not in by_id],
              "duplicate_ids": duplicates, "malformed_nodes": malformed, "mismatched_nodes": mismatched,
              "selected_node_count": len(nodes), "serialized_bytes": len(observation["context_json"].encode("utf-8"))}
    if not result["passed"]:
        raise AssessmentFailure(result)
    return result


def capsule_json(capsule):
    # serde struct field order, unlike the journal's serde_json::Value map order.
    require(isinstance(capsule, dict) and set(capsule) == {"nodes"}
            and isinstance(capsule["nodes"], list), "malformed durable capsule")
    ordered = []
    for node in capsule["nodes"]:
        require(isinstance(node, dict) and set(node) == set(NODE_FIELDS), "malformed durable node")
        ordered.append({field: node[field] for field in NODE_FIELDS})
    return json.dumps({"nodes": ordered}, ensure_ascii=False, separators=(",", ":"), allow_nan=False)


def reconcile_observations(observations, events, plans, calls, boundary):
    """Bind every record to its submitted query; never silently truncate with zip."""
    records = []
    counts = {"planned": len(plans), "durable": None, "durable_inputs": None,
              "call_log": len(calls), "observations": len(observations),
              "missing": None, "duplicate": None, "unmatched": None}
    try:
        model_events = [e for e in events if e.get("kind") == "model.requested"]
        inputs = [e for e in events if e.get("kind") == "input.received"
                  and (e["seq"] > boundary or "agent_run" in e.get("payload", {}))]
        counts.update(durable=len(model_events), durable_inputs=len(inputs))
        request_ids = [e.get("payload", {}).get("request", {}).get("request_id") for e in model_events]
        observed_ids = [o.get("request_id") for o in observations]
        client_ids = [p.get("client_request_id") for p in plans]
        input_ids = [e["payload"]["agent_run"].get("request_id") for e in inputs]
        def duplicates(values):
            return len(values) - len(set(values))
        counts.update({"planned": len(plans), "durable": len(model_events), "durable_inputs": len(inputs),
                  "call_log": len(calls), "observations": len(observations),
                  "missing": sum(max(0, len(plans) - len(items)) for items in (inputs, model_events, calls, observations)),
                  "duplicate": sum(duplicates(items) for items in (client_ids, input_ids, request_ids, calls, observed_ids)),
                  "unmatched": (len(set(input_ids) ^ set(client_ids)) + len(set(observed_ids) ^ set(request_ids))
                                + len(set(calls) ^ set(request_ids)))})
        require(plans and all(len(items) == len(plans) for items in (inputs, model_events, calls, observations)),
                "missing or extra reconciliation records")
        require(counts["duplicate"] == counts["unmatched"] == 0, "duplicate or unmatched identity")
        require(all(isinstance(i, str) and i for i in client_ids + input_ids + request_ids + calls + observed_ids),
                "malformed request identity")
        require(all(re.fullmatch(r"[0-7][0-9A-HJKMNP-TV-Z]{25}", identity) for identity in client_ids),
                "noncanonical client request identity")
        require(all(type(e["seq"]) is int and isinstance(e["event_id"], str) and e["event_id"] for e in events),
                "malformed event identity/sequence")
        require(client_ids == input_ids and request_ids == calls == observed_ids, "reordered or swapped records")
        require([e["seq"] for e in events] == list(range(1, len(events) + 1)), "event sequence gap/reorder/duplicate")
        require(len({e["event_id"] for e in events}) == len(events), "duplicate event identity")
        require(len({p["task_id"] for p in plans}) == len(plans)
                and len({p["turn_id"] for p in plans}) == len(plans), "duplicate task/turn")
        cases = {c["case"]: c for c in CORPUS["cases"]}
        require(all(type(p["repetition"]) is int and p["repetition"] >= 0 for p in plans),
                "malformed repetition")
        positions = [(p["repetition"], list(cases).index(p["case"])) for p in plans]
        require(positions == sorted(set(positions)), "reordered or duplicate case/repetition")
        rounds = sorted({p["repetition"] for p in plans})
        require(rounds == list(range(len(rounds))) and all(
            [p["case"] for p in plans if p["repetition"] == r] == list(cases)
            for r in rounds), "missing case/repetition")
        by_event = {e["event_id"]: e for e in events}
        for plan, source, event, observation in zip(plans, inputs, model_events, observations):
            payload, request = event["payload"], event["payload"]["request"]
            require(plan["query"] == cases[plan["case"]]["query"], "case/query mismatch")
            require(plan["session"] == CORPUS["session"], "planned session mismatch")
            require(plan["task_id"] == "run_" + plan["client_request_id"], "client/task mismatch")
            require(plan["turn_id"].startswith("turn_"), "malformed turn")
            for record in (source, event):
                require((record["session_id"], record["task_id"], record["correlation_id"])
                        == (plan["session"], plan["task_id"], plan["turn_id"]), "session/task/turn mismatch")
            require(boundary < source["seq"] < event["seq"], "request before restart/input")
            source_run = source["payload"]["agent_run"]
            require(source["actor"] == "user" and source["payload"]["text"] == plan["query"]
                    and type(source_run) is dict and set(source_run) == {"version", "request_id"}
                    and type(source_run["version"]) is int and source_run["version"] == 1
                    and source_run["request_id"] == plan["client_request_id"],
                    "durable input/query mismatch")
            require(event["actor"] == "system" and type(payload["event_version"]) is int
                    and payload["event_version"] == 1
                    and payload["turn_id"] == plan["turn_id"] and type(payload["request_index"]) is int
                    and payload["request_index"] == 0 and event["span_id"] == request["request_id"]
                    and request["control"]["cancellation_id"] == plan["turn_id"], "model request identity mismatch")
            # Follow the actual event causation chain back to this exact input.
            ancestor = event
            while ancestor["event_id"] != source["event_id"]:
                parent = by_event.get(ancestor.get("causation_id"))
                require(parent is not None and source["seq"] <= parent["seq"] < ancestor["seq"]
                        and (parent["session_id"], parent["task_id"], parent["correlation_id"])
                        == (plan["session"], plan["task_id"], plan["turn_id"]), "unmatched causal event identity")
                ancestor = parent
            require(request["turn"]["conversation"] == [
                {"type": "message", "role": "user", "content": [{"type": "text", "text": plan["query"]}]}],
                "model conversation/query mismatch")
            require(set(observation) == {"request_id", "context_json"}, "malformed observation")
            require(strict_json(observation["context_json"]) == request["turn"]["context"]
                    and observation["context_json"] == capsule_json(request["turn"]["context"]),
                    "driver/journal capsule byte mismatch")
            records.append({**plan, **observation, "input_event": source, "model_event": event,
                            "identity_reconciled": True})
    except (AssertionError, AttributeError, KeyError, TypeError, ValueError) as error:
        raise AssessmentFailure({"error": "identity reconciliation failed", "detail": str(error), "counts": counts,
                                 "metric": metric(len(records), len(plans),
                                                  [r["client_request_id"] for r in records], "no planned requests")}) from error
    return {"records": records, "counts": counts,
            "metric": metric(len(records), len(plans), [r["client_request_id"] for r in records], "no planned requests")}


def aggregate_metrics(requests):
    return {name: metric(sum(r["metrics"][name]["numerator"] for r in requests),
                         sum(r["metrics"][name]["denominator"] for r in requests),
                         [{"client_request_id": r["client_request_id"], "case": r["case"],
                           "repetition": r["repetition"], "items": r["metrics"][name]["contributors"]} for r in requests],
                         "no opportunities across assessed requests") for name in requests[0]["metrics"]}


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
    failure = {"profile_noise": noise_count, "samples_per_query": samples}
    try:
        server.start()
        initial = checkpoint(server)
        require(initial["durable_events"] == 0, "store is not fresh")
        started = time.perf_counter()

        def save(text, session="personal", replaces=None):
            args = ["memory", "save", text, "--session", session]
            if replaces:
                args += ["--replaces", replaces]
            result = server.cli(*args)
            require(result["outcome"] == "recorded", "memory save failed")
            return {**result, "text": text, "session": session, "replaces": replaces}

        memories = {}
        for seed in CORPUS["seeds"]:
            replaces = memories[seed["replaces"]]["memory_id"] if "replaces" in seed else None
            memories[seed["label"]] = save(seed["text"], seed["session"], replaces)
        memories["noise"] = [save(CORPUS["noise"]["text"].format(i=i)) for i in range(noise_count)]
        seed_ms = (time.perf_counter() - started) * 1000
        after_seed = checkpoint(server)
        seed_events = server.events()
        seed_accounting = baseline.accounting(seed_events)
        failure["accounting"] = seed_accounting
        require(seed_accounting["event_counts"] == {"input.received": noise_count + 7,
                                                   "context.node.recorded": noise_count + 7}, "seed accounting mismatch")
        boundary = after_seed["durable_events"]
        require(boundary == 2 * (7 + noise_count), "seed event count mismatch")
        require([e["seq"] for e in seed_events] == list(range(1, boundary + 1)), "seed event sequence mismatch")
        by_event = {e["event_id"]: e for e in seed_events}
        require(len(by_event) == boundary, "duplicate seed event identity")
        # Resolve actual source input, independently of the save response's memory-record event.
        all_memories = [memories[s["label"]] for s in CORPUS["seeds"]] + memories["noise"]
        for memory in all_memories:
            source_id = memory["memory_id"].removeprefix("memory-").upper()
            source, recorded = by_event[source_id], by_event[memory["event_id"]]
            expected = {"id": memory["memory_id"], "kind": "claim", "summary": memory["text"],
                        "origin": "user", "epistemic": "asserted", "scope": "session", "confidence": 1.0,
                        "source_event_ids": [source_id]}
            require(source["actor"] == "user" and source["kind"] == "input.received"
                    and source["session_id"] == memory["session"] and source.get("task_id") is None
                    and source["payload"]["text"] == memory["text"]
                    and source["seq"] < recorded["seq"] == memory["event_seq"], "memory input provenance mismatch")
            require(recorded["kind"] == "context.node.recorded" and recorded["actor"] == "system"
                    and recorded["session_id"] == memory["session"] and recorded.get("task_id") is None
                    and recorded["causation_id"] == source_id
                    and all(recorded["payload"]["node"][k] == v for k, v in expected.items())
                    and recorded["payload"]["node"]["supersedes"] == ([memory["replaces"]] if memory["replaces"] else [])
                    and recorded["payload"]["node"].get("valid_from") is None
                    and recorded["payload"]["node"].get("valid_until") is None, "memory record mismatch")
            memory.update(expected_item=expected, input_event=source, memory_event=recorded)

        def no_driver_records():
            require(not (server.data / "fixture-calls.txt").exists()
                    and not (server.data / "fixture-contexts.jsonl").exists(), "unexpected housekeeping driver call")
        no_driver_records()
        server.stop()
        server.start()
        after_restart = checkpoint(server)
        require(after_restart["durable_events"] == boundary, "restart appended events")

        started = time.perf_counter()
        active = [memories[k] for k in ("CURRENT", "PACKING", "TRAIN", "FOOD", "IRRELEVANT", "OTHER")] + memories["noise"]
        listings = {}
        for session in ("personal", "elsewhere"):
            found, cursor = {}, None
            while True:
                args = ["memory", "list", "--session", session, "--limit", "100"]
                if cursor:
                    args += ["--after-id", cursor]
                page = server.cli(*args)
                for memory in page["memories"]:
                    require(memory["id"] not in found, "duplicate listed memory")
                    found[memory["id"]] = memory["text"]
                next_cursor = page["next_after_id"]
                if next_cursor is None:
                    break
                require(cursor is None or next_cursor > cursor, "listing cursor did not advance")
                cursor = next_cursor
            require(found == {m["memory_id"]: m["text"] for m in active if m["session"] == session}, "recovered listing mismatch")
            listings[session] = found
        listing_ms = (time.perf_counter() - started) * 1000
        require(server.get("/health")["durable_events"] == boundary, "listing appended events")
        no_driver_records()

        runs, query_checkpoints = [], []
        for repetition in range(samples):
            for case in CORPUS["cases"]:
                identity = f"01K{noise_count:018d}{len(runs):05d}"
                result = server.cli("run", case["query"], "--session", CORPUS["session"],
                                    "--request-id", identity, metric=case["case"])
                require(result["status"] == "unverified" and result["request_id"] == identity, "run status/identity mismatch")
                runs.append({"case": case["case"], "query": case["query"], "session": CORPUS["session"],
                             "repetition": repetition, "client_request_id": identity, "task_id": result["task_id"],
                             "turn_id": result["turn_id"], "status": result["status"],
                             "latency_ms": server.latencies[case["case"]][-1]})
                query_checkpoints.append(checkpoint(server))
        events = server.events()
        counts = baseline.accounting(events)
        failure.update(accounting=counts, planned_count=len(runs))
        calls = (server.data / "fixture-calls.txt").read_text().splitlines()
        failure["call_log_count"] = len(calls)
        observation_lines = (server.data / "fixture-contexts.jsonl").read_text().splitlines()
        failure["observation_count"] = len(observation_lines)
        observations = [strict_json(line) for line in observation_lines]
        reconciliation = reconcile_observations(observations, events, runs, calls, boundary)
        assessed = []
        for run in reconciliation["records"]:
            case = next(c for c in CORPUS["cases"] if c["case"] == run["case"])
            expected = [memories[label]["expected_item"] for label in case["expected"]]
            forbidden = {"stale": [memories["OLD"]["expected_item"]], "scope": [memories["OTHER"]["expected_item"]],
                         "irrelevant": [memories[label]["expected_item"] for label in
                                        ("CURRENT", "PACKING", "TRAIN", "FOOD", "IRRELEVANT") if label not in case["expected"]],
                         "noise": [m["expected_item"] for m in memories["noise"]]}
            result = assess_context({"request_id": run["request_id"], "context_json": run["context_json"]}, expected, forbidden)
            result["metrics"]["identity_reconciliation"] = metric(1, 1, [run["client_request_id"]], "no planned request")
            assessed.append({**run, **result})
        total = samples * len(CORPUS["cases"])
        require(counts["model_requests"] == total and counts["tool_executions"] == 0
                and counts["event_counts"].get("task.completed", 0) == 0, "workload call/completion mismatch")
        counts["injected_driver_calls"] = len(calls)
        return {
            "profile": "minimal_history" if noise_count == 0 else "longer_history",
            "unrelated_memories": noise_count, "saved_memories_including_superseded": noise_count + 7,
            "samples_per_query": samples, "total_runs": total, "memories": memories, "seed_duration_ms": seed_ms,
            "startup_and_recovery_ms": server.startups, "initial": initial, "after_seed": after_seed,
            "after_restart": after_restart, "recovered_memory_listing_ms": listing_ms, "recovered_listings": listings,
            "recovery": {"last_seq_before_restart": boundary, "no_new_events_on_reopen_or_listing": True,
                         "all_active_memories_recovered": True, "queries_after_restart": total},
            "after_each_query": query_checkpoints,
            "latency_by_case": {name: baseline.summary(values) for name, values in server.latencies.items()},
            "latency_pooled_corpus": baseline.summary([r["latency_ms"] for r in assessed]),
            "requests": assessed, "seed_accounting": seed_accounting, "whole_workload_accounting": counts,
            "metrics": aggregate_metrics(assessed),
            "identity_reconciliation": {k: v for k, v in reconciliation.items() if k != "records"},
            "call_log": calls, "post_restart_events": [e for e in events if e["seq"] > boundary],
            "model_answer_quality": None, "task_completion": None, "semantic_recall": None, "tool_task_success": None,
            "first_useful_progress_ms": None, "isolated_model_time_ms": None, "isolated_tool_time_ms": None,
            "ditto_only_overhead_ms": None, "general_agent_quality": None, "v0_1_readiness": None,
            "unavailable_basis": "five synthetic lexical ContextCapsule cases; fixture answers unassessed; no semantic, task-success, live usage/cost or isolated timing evidence",
        }
    except (Exception, SystemExit, KeyboardInterrupt) as error:
        failure.update(type=type(error).__name__, diagnostics=getattr(error, "diagnostics", str(error)))
        print(json.dumps({"failure": failure}, sort_keys=True), file=sys.stderr)
        raise
    finally:
        server.stop()


def source_hashes():
    sources = {p for base in ("apps", "crates", "capabilities", "scripts", ".cargo")
               for p in (REPO / base).rglob("*") if p.is_file() and p.suffix in (".rs", ".toml", ".py", ".sh", ".json")}
    sources.update(REPO / name for name in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml"))
    return {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(sources)}


def main():
    signal.signal(signal.SIGTERM, baseline.terminate)
    signal.signal(signal.SIGINT, baseline.terminate)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--history-size", type=int, default=1000, help="unrelated memories in longer profile (1..5000)")
    parser.add_argument("--samples", type=int, default=5, help="repetitions per each of five frozen queries (2..100)")
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
        "schema": 2, "workload": "task016-offline-personal-task-corpus-v1", "utc": datetime.now(timezone.utc).isoformat(),
        "history_size": args.history_size, "samples_per_query": args.samples,
        "corpus": CORPUS, "corpus_sha256": hashlib.sha256(json.dumps(CORPUS, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        "corpus_hash_basis": "UTF-8 JSON, sorted keys, compact separators, no trailing newline",
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
            "comparison": "sequential fresh stores: 0 then N unrelated session memories; same frozen corpus and repetitions per query",
        },
        "limits": "five-case synthetic lexical ContextCapsule conformance after restart; no general/model-answer quality, semantic retrieval, cross-session recall, live tokens/cost, superiority or v0.1 readiness claim",
        "results": [],
    }
    for noise_count in (0, args.history_size):
        with tempfile.TemporaryDirectory(prefix="ditto-personal-quality-") as temporary:
            report["results"].append(workload(Path(temporary), binaries, noise_count, args.samples))
    # Stage beside the destination so replacement is atomic; a failed run never
    # publishes success or damages an older report. Hashes are checked last.
    pending = None
    try:
        require(sources == source_hashes(), "source drift during measurement; rerun")
        require(binary_hashes == {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()},
                "binary drift during measurement")
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=args.output.parent,
                                         prefix=".task016-", suffix=".pending", delete=False) as output:
            pending = Path(output.name)
            output.write(json.dumps(report, indent=2, sort_keys=True) + "\n")
            output.flush()
            os.fsync(output.fileno())
        require(sources == source_hashes(), "source drift before publication")
        require(binary_hashes == {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()},
                "binary drift before publication")
        require(report["corpus_sha256"] == hashlib.sha256(json.dumps(CORPUS, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
                "corpus drift before publication")
        pending.replace(args.output)
    except (Exception, SystemExit, KeyboardInterrupt) as error:
        print(json.dumps({"failure": {"stage": "publication", "type": type(error).__name__,
                         "diagnostics": getattr(error, "diagnostics", str(error)),
                         "accounting": [p.get("whole_workload_accounting") for p in report["results"]]}}), file=sys.stderr)
        raise
    finally:
        if pending is not None:
            pending.unlink(missing_ok=True)
    print(f"Offline five-case context corpus passed; report: {args.output}")


if __name__ == "__main__":
    main()
