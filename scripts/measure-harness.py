#!/usr/bin/env python3
"""Measure the harness around the model: latency before dispatch, cost per
streamed delta, journal amplification, prompt prefix reuse between turns, and
concurrent sessions. Standard library only; no network beyond loopback.

    cargo build --release --locked -p ditto-daemon
    python3 scripts/measure-harness.py [--deltas 200] [--memories 12] [--runs 12]

A loopback OpenAI-compatible mock answers instantly, so the numbers are the
harness's own cost on this machine, not model latency. Daemon-side timings
come from durable event timestamps (millisecond precision).
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DAEMON = REPO / "target" / "release" / "ditto-daemon"


class Mock(BaseHTTPRequestHandler):
    """Streams `deltas` short text chunks, or waits `delay` seconds first."""

    protocol_version = "HTTP/1.1"
    deltas = 200
    delay = 0.0
    prompts = []
    marks = {}

    def log_message(self, *args):
        pass

    def do_POST(self):
        arrived = time.perf_counter()
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        question = body["messages"][-1]["content"]
        if question.startswith("[Ditto:"):
            question = question.split("]\n\n", 1)[-1]
        rendered = json.dumps(body.get("tools", []), sort_keys=True) + "".join(
            f"<|{m['role']}|>{m.get('content') or ''}" for m in body["messages"])
        Mock.prompts.append(rendered)
        Mock.marks[question] = {"arrived": arrived}
        time.sleep(Mock.delay)
        stream = "".join(
            "data: " + json.dumps({"choices": [{"index": 0, "delta": {"content": "tok "}}]}) + "\n\n"
            for _ in range(Mock.deltas))
        stream += "data: " + json.dumps({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})
        stream += "\n\ndata: [DONE]\n\n"
        data = stream.encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class Daemon:
    def __init__(self):
        if not DAEMON.is_file():
            raise SystemExit(f"build first: missing {DAEMON}")
        self.mock = ThreadingHTTPServer(("127.0.0.1", 0), Mock)
        threading.Thread(target=self.mock.serve_forever, daemon=True).start()
        self.data = tempfile.mkdtemp(prefix="ditto-measure-")
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        self.api = f"http://127.0.0.1:{port}"
        self.process = subprocess.Popen(
            [str(DAEMON), "--provider", "openai-compatible",
             "--base-url", f"http://127.0.0.1:{self.mock.server_address[1]}/v1", "--model", "mock",
             "--data-dir", os.path.join(self.data, "d"), "--capabilities-dir", str(REPO / "capabilities"),
             "--bind", f"127.0.0.1:{port}"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(400):
            try:
                self.get("/health")
                return
            except OSError:
                time.sleep(0.05)
        raise SystemExit("daemon did not start")

    def get(self, path):
        with urllib.request.urlopen(self.api + path, timeout=30) as response:
            return json.loads(response.read())

    def post(self, path, body):
        request = urllib.request.Request(
            self.api + path, json.dumps(body).encode(), {"content-type": "application/json"})
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read())

    def remember(self, text, session="personal"):
        _, event = self.post("/v1/commands/input", {"text": text, "session_id": session})
        self.post("/v1/commands/memory", {"session_id": session, "input_event_id": event["event"]["event_id"]})

    def run(self, request_id, text, session="personal"):
        self.post("/v1/commands/run", {"request_id": request_id, "session_id": session, "text": text})
        for _ in range(20_000):
            status = self.get(f"/v1/runs?session_id={session}&request_id={request_id}")
            if status["status"] != "running":
                return status
            time.sleep(0.002)
        raise SystemExit(f"run {request_id} did not finish")

    def events(self, task):
        events, after = [], 0
        while True:
            page = self.get(f"/v1/events?session_id=personal&task_id={task}&limit=1000&after_seq={after}")
            events += page
            if len(page) < 1000:
                return events
            after = page[-1]["seq"]

    def files_kb(self):
        sizes = {}
        for root, _, files in os.walk(self.data):
            for name in files:
                sizes[name] = sizes.get(name, 0) + os.path.getsize(os.path.join(root, name))
        return {name: round(size / 1024) for name, size in sorted(sizes.items()) if size > 4096}

    def close(self):
        self.process.terminate()
        self.process.wait()
        self.mock.shutdown()
        shutil.rmtree(self.data, ignore_errors=True)


def millis(event):
    return datetime.fromisoformat(event["recorded_at"].replace("Z", "+00:00")).timestamp() * 1e3


def median(values):
    values = sorted(values)
    return round(values[len(values) // 2], 2)


def turn_costs(args):
    Mock.deltas, Mock.delay, Mock.prompts, Mock.marks = args.deltas, 0.0, [], {}
    daemon = Daemon()
    try:
        for i in range(args.memories):
            daemon.remember(f"Fact number {i}: I like item {i}.")
        rows = []
        for i in range(1, args.runs + 1):
            request_id = f"01K{i:023d}"
            question = f"question {i}"
            started = time.perf_counter()
            daemon.run(request_id, question)
            events = daemon.events(f"run_{request_id}")
            first = {kind: next(e for e in events if e["kind"] == kind)
                     for kind in ("input.received", "model.requested")}
            outputs = [e for e in events if e["kind"] == "model.output"]
            text = sum(len(e["payload"]["stream_event"]["event"].get("text", "")) for e in outputs)
            payload = sum(len(json.dumps(e["payload"])) for e in events)
            rows.append({
                "client_to_provider_ms": (Mock.marks[question]["arrived"] - started) * 1e3,
                "harness_before_dispatch_ms": millis(first["model.requested"]) - millis(first["input.received"]),
                "per_delta_us": (millis(outputs[-1]) - millis(outputs[0])) * 1e3 / max(1, len(outputs) - 1),
                "events_per_turn": len(events),
                "journal_bytes_per_answer_byte": payload / max(1, text),
            })
        warm = rows[2:] or rows
        summary = {key: median(row[key] for row in warm) for key in warm[0]}
        summary.update(deltas=args.deltas, memories=args.memories, files_kb=daemon.files_kb())
        return summary
    finally:
        daemon.close()


def prefix_reuse(args):
    Mock.deltas, Mock.delay, Mock.prompts, Mock.marks = 1, 0.0, [], {}
    daemon = Daemon()
    try:
        for fact in ["I live in Seoul", "My dog is called Miso", "I prefer afternoon meetings",
                     "My sister is Jiwon", "I am allergic to peanuts", "I work on a project called Ditto"]:
            daemon.remember(fact)
        questions = ["What is my dog called?", "When do I like meetings?", "Where do I live?",
                     "What am I allergic to?", "Who is my sister?", "What is my project?",
                     "Tell me about Seoul.", "Any advice on peanuts?", "And my dog again?",
                     "Summarize what you know about me.", "Thanks!", "One more thing about meetings?"]
        for i, question in enumerate(questions, 1):
            daemon.run(f"01K{i:023d}", question)
        shares = []
        for previous, current in zip(Mock.prompts, Mock.prompts[1:]):
            same = 0
            for a, b in zip(previous, current):
                if a != b:
                    break
                same += 1
            shares.append(100 * same / len(current))
        return {"turns": len(Mock.prompts), "reused_prefix_percent_median": median(shares),
                "reused_prefix_percent_min": round(min(shares), 1)}
    finally:
        daemon.close()


def concurrency(_args):
    Mock.deltas, Mock.delay, Mock.prompts, Mock.marks = 1, 0.5, [], {}
    daemon = Daemon()
    try:
        results = {}

        def start(i):
            try:
                results[f"session{i}"] = daemon.post(
                    "/v1/commands/run",
                    {"request_id": f"01K{i:023d}", "session_id": f"session{i}", "text": "hello"})[0]
            except urllib.error.HTTPError as error:
                results[f"session{i}"] = error.code
        threads = [threading.Thread(target=start, args=(i,)) for i in range(1, 4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        return {"three_sessions_at_once": dict(sorted(results.items()))}
    finally:
        daemon.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--deltas", type=int, default=200)
    parser.add_argument("--memories", type=int, default=12)
    parser.add_argument("--runs", type=int, default=12)
    args = parser.parse_args()
    print(json.dumps({
        "turn_costs": turn_costs(args),
        "prefix_reuse": prefix_reuse(args),
        "concurrency": concurrency(args),
    }, indent=1))


if __name__ == "__main__":
    main()
