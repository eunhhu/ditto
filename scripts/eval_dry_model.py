#!/usr/bin/env python3
"""Zero-cost scripted OpenAI-compatible model for Task 037 dry runs.

It lets the real daemon exercise every campaign path (context, memory search,
artifact sort, schedules) without a provider. Its answers are mechanical echoes,
never model quality: a dry-run report can never pass readiness.
"""

from __future__ import annotations

import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

REFERENCE = re.compile(r"artifact:sha256:[0-9a-f]{64}")


def _text(message: dict) -> str:
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return ""


def plan(body: dict) -> dict:
    """Return either {"tool": name, "arguments": {...}} or {"text": ...}."""
    messages = body.get("messages", [])
    last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1)
    after = messages[last_user + 1 :]
    tools = {t["function"]["name"] for t in body.get("tools", []) if t.get("type") == "function"}
    user = _text(messages[last_user]) if last_user >= 0 else ""
    used = {call["function"]["name"] for m in after for call in m.get("tool_calls") or []}
    reference = REFERENCE.search(user)
    if reference and "artifact_sort" in tools and "artifact_sort" not in used:
        unique = '"allow_deduplicate":true' in user.replace(" ", "")
        return {"tool": "artifact_sort", "arguments": {"reference": reference.group(0), "unique": unique}}
    if not reference and "memory_manage" in tools and "memory_manage" not in used and "?" in user:
        query = re.sub(r"[^A-Za-z ]", " ", user.split("]")[-1]).strip()[:120] or "memory"
        return {"tool": "memory_manage", "arguments": {"action": "search", "query": query}}
    system = " ".join(_text(m) for m in messages if m.get("role") == "system")
    remembered = re.findall(r'"text":"((?:[^"\\]|\\.)*)"', system)
    observed = [_text(m) for m in after if m.get("role") == "tool"]
    parts = [f"Context: {' | '.join(remembered)}" if remembered else "No remembered context."]
    if observed:
        parts.append("Tool: " + " | ".join(o[:300] for o in observed))
    return {"text": "[dry-run fixture answer] " + " ".join(parts)}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:  # noqa: N802
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        step = plan(body)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def chunk(delta: dict, finish: str | None = None, usage: dict | None = None) -> None:
            data = {"id": "dry", "object": "chat.completion.chunk", "model": body.get("model"),
                    "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            if usage is not None:
                data["usage"] = usage
            self.wfile.write(b"data: " + json.dumps(data).encode() + b"\n\n")

        prompt = len(json.dumps(body)) // 4
        if "tool" in step:
            call = {"index": 0, "id": "call_dry", "type": "function",
                    "function": {"name": step["tool"], "arguments": json.dumps(step["arguments"])}}
            chunk({"role": "assistant", "tool_calls": [call]})
            chunk({}, "tool_calls", {"prompt_tokens": prompt, "completion_tokens": 20, "total_tokens": prompt + 20})
        else:
            chunk({"role": "assistant", "content": step["text"]})
            completion = len(step["text"]) // 4
            chunk({}, "stop", {"prompt_tokens": prompt, "completion_tokens": completion,
                               "total_tokens": prompt + completion})
        self.wfile.write(b"data: [DONE]\n\n")


def main() -> None:
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()


if __name__ == "__main__":
    main()
