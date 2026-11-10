#!/usr/bin/env python3
"""Loopback forwarder for Task 017 live runs against OpenCode Go.

OpenCode Go only routes requests that carry a stable `x-opencode-session` per
conversation and a client-specific User-Agent. Ditto's `openai-compatible`
provider sends neither, so the evaluation runner points Ditto at this
forwarder, which:

- injects `x-opencode-session` (set per attempt through `POST /__session`),
- sets `User-Agent: ditto-eval/1.0`,
- replaces the inbound Authorization with the key from `UPSTREAM_API_KEY`,
- streams the upstream body back unchanged.

It changes no request or response content. Usage: eval_model_proxy.py PORT UPSTREAM_BASE_URL
"""
import http.client
import json
import os
import sys
import threading
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = urllib.parse.urlsplit(sys.argv[2].rstrip("/"))
KEY = os.environ["UPSTREAM_API_KEY"]
# Set DUMP_DIR to keep each request body for size measurements (ditto eval).
DUMP_DIR = os.environ.get("DUMP_DIR")
DUMP_SEQ = {"n": 0}
STATE = {"session": "ditto-eval-default"}
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format, *args):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        if self.path == "/__session":
            with LOCK:
                STATE["session"] = json.loads(body)["session"]
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        with LOCK:
            session = STATE["session"]
        if DUMP_DIR:
            with LOCK:
                DUMP_SEQ["n"] += 1
                n = DUMP_SEQ["n"]
            os.makedirs(DUMP_DIR, exist_ok=True)
            with open(os.path.join(DUMP_DIR, f"request-{n:04d}.json"), "wb") as handle:
                handle.write(body)
        path = UPSTREAM.path + self.path[len("/v1"):] if self.path.startswith("/v1") else UPSTREAM.path + self.path
        conn = http.client.HTTPSConnection(UPSTREAM.netloc, timeout=300)
        conn.request("POST", path, body=body, headers={
            "Authorization": f"Bearer {KEY}", "Content-Type": "application/json",
            "Accept": self.headers.get("Accept", "*/*"), "User-Agent": "ditto-eval/1.0",
            "x-opencode-session": session})
        response = conn.getresponse()
        self.send_response(response.status)
        self.send_header("Content-Type", response.getheader("Content-Type", "application/json"))
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        while chunk := response.read1(65536):
            self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
            self.wfile.flush()
        self.wfile.write(b"0\r\n\r\n")
        conn.close()


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
