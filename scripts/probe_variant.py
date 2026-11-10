#!/usr/bin/env python3
"""Task 042: probe one harness variant on the cases that can show the change.

A variant is a JSON manifest naming a small, exact text edit to a harness
surface and the cases to measure it on:

  {
    "name": "p3-second-hop",
    "edits": [
      {"file": "crates/kernel/src/turn/shared.rs",
       "find": "exact text", "replace": "new text"}
    ],
    "cases": ["P3"]
  }

Subcommands (all stdlib, no network of their own):

  apply <variant>     edit the named files, keeping their exact bytes aside
  revert <variant>    restore those bytes and verify the digests match
  round <variant>     apply, build release, run the cases, compare, revert
  compare <base> <variant>  per-case verified/recall/cost delta from two reports

A round is refused when the tree is dirty outside the files the variant edits,
so a probe never mixes with unrelated work. The backup lives under the
repository's git directory, so it is never tracked.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from decimal import Decimal
from pathlib import Path


def repository_root() -> Path:
    return Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                               text=True, check=True).stdout.strip())


def store_dir(root: Path, name: str) -> Path:
    git_dir = subprocess.run(["git", "rev-parse", "--git-path", "probe-variants"], cwd=root,
                             capture_output=True, text=True, check=True).stdout.strip()
    path = (root / git_dir) if not Path(git_dir).is_absolute() else Path(git_dir)
    return path / name


def load_variant(path: Path) -> dict:
    variant = json.loads(path.read_text(encoding="utf-8"))
    if not variant.get("name") or not variant.get("edits") or not variant.get("cases"):
        raise SystemExit("variant needs name, edits and cases")
    for edit in variant["edits"]:
        for key in ("file", "find", "replace"):
            if key not in edit:
                raise SystemExit(f"edit missing {key}: {edit}")
        if edit["find"] == edit["replace"]:
            raise SystemExit(f"edit does not change anything: {edit['file']}")
    return variant


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def clean_tree_except(root: Path, allowed: set[Path]) -> list[str]:
    result = subprocess.run(["git", "status", "--porcelain"], cwd=root, capture_output=True,
                            text=True, check=True).stdout
    dirty = []
    for line in result.splitlines():
        if line.startswith("??"):
            continue  # untracked files are not part of any diff the variant makes
        path = root / line[3:].split(" -> ")[-1]
        if path not in allowed:
            dirty.append(line)
    return dirty


def apply(root: Path, variant: dict) -> Path:
    """Edit every file or nothing. Returns the backup directory."""
    store = store_dir(root, variant["name"])
    if store.exists():
        raise SystemExit(f"{store} already exists; run revert first")
    grouped: dict[Path, list[dict]] = {}
    for edit in variant["edits"]:
        grouped.setdefault(root / edit["file"], []).append(edit)
    plan = []
    for path, edits in grouped.items():
        if not path.is_file():
            raise SystemExit(f"missing file: {path}")
        original = path.read_bytes()
        text = original.decode("utf-8")
        for edit in edits:
            count = text.count(edit["find"])
            if count != 1:
                raise SystemExit(f"{edit['file']}: expected exactly one match for "
                                 f"{edit['find']!r}, found {count}")
            text = text.replace(edit["find"], edit["replace"], 1)
        plan.append((path, original, text.encode("utf-8")))
    touched = {path for path, _, _ in plan}
    dirty = clean_tree_except(root, touched)
    if dirty:
        raise SystemExit("tree is dirty outside this variant:\n  " + "\n  ".join(dirty))
    store.mkdir(parents=True)
    manifest = {"name": variant["name"], "files": []}
    for path, original, replaced in plan:
        manifest["files"].append({"path": str(path.relative_to(root)),
                                  "original_sha256": digest(original),
                                  "replaced_sha256": digest(replaced)})
    for path, _, replaced in plan:
        path.write_bytes(replaced)
    (store / "manifest.json").write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    for path, original, _ in plan:
        (store / f"{digest(original)}.orig").write_bytes(original)
    return store


def revert(root: Path, variant: dict) -> None:
    store = store_dir(root, variant["name"])
    manifest_path = store / "manifest.json"
    if not manifest_path.is_file():
        raise SystemExit(f"nothing to revert for {variant['name']}")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    for entry in manifest["files"]:
        path = root / entry["path"]
        original = (store / f"{entry['original_sha256']}.orig").read_bytes()
        if digest(original) != entry["original_sha256"]:
            raise SystemExit(f"backup for {entry['path']} is corrupt")
        path.write_bytes(original)
        if digest(path.read_bytes()) != entry["original_sha256"]:
            raise SystemExit(f"restore of {entry['path']} did not reproduce the bytes")
    shutil.rmtree(store)


def case_stats(report_path: Path) -> dict:
    attempts = json.loads(report_path.read_text(encoding="utf-8"))["attempts"]
    stats: dict = {"attempts": 0, "verified": 0, "usd": Decimal(0), "input_tokens": 0,
                   "completion_ms": [], "recall": [0, 0], "by_case": {}}
    for attempt in attempts:
        case_id = attempt["case_id"]
        row = stats["by_case"].setdefault(case_id, {"attempts": 0, "verified": 0, "usd": Decimal(0),
                                                    "recall": [0, 0], "ms": []})
        resources = attempt["resources"]
        verified = bool(attempt.get("independent_success")) and attempt.get("state") == "observed"
        usd = Decimal(str(resources.get("external_provider_usd") or 0))
        recall = ((attempt.get("context") or {}).get("recall_at_5") or {})
        for row_ in (stats, row):
            row_["attempts"] += 1
            row_["verified"] += int(verified)
            row_["usd"] += usd
            if row_ is stats:
                stats["input_tokens"] += int(resources.get("prompt_tokens") or 0)
        row["recall"][0] += int(recall.get("numerator") or 0)
        row["recall"][1] += int(recall.get("denominator") or 0)
        if resources.get("completion_ms") is not None:
            row["ms"].append(resources["completion_ms"])
    return stats


def gates(report_path: Path) -> dict:
    report = json.loads(report_of(report_path).read_text(encoding="utf-8"))
    summary = report.get("gates") or report.get("summary") or {}
    if isinstance(summary, dict):
        return {name: (value.get("state") if isinstance(value, dict) else value)
                for name, value in summary.items()}
    return {}


def report_of(path: Path) -> Path:
    return path / "report.json" if path.is_dir() else path


def compare(base: Path, variant: Path) -> int:
    left, right = case_stats(report_of(base)), case_stats(report_of(variant))
    print(f"{'case':6} {'base':>10} {'variant':>10} {'delta':>7}   recall      usd")
    failed = False
    for case_id in sorted(set(left["by_case"]) | set(right["by_case"])):
        a = left["by_case"].get(case_id, {"attempts": 0, "verified": 0, "recall": [0, 0], "usd": Decimal(0)})
        b = right["by_case"].get(case_id, {"attempts": 0, "verified": 0, "recall": [0, 0], "usd": Decimal(0)})
        pa = f"{a['verified']}/{a['attempts']}"
        pb = f"{b['verified']}/{b['attempts']}"
        delta = b["verified"] - a["verified"]
        if delta < 0:
            failed = True
        ra = f"{a['recall'][0]}/{a['recall'][1]}" if a["recall"][1] else "-"
        rb = f"{b['recall'][0]}/{b['recall'][1]}" if b["recall"][1] else "-"
        if a["recall"][1] and b["recall"][1] and b["recall"][0] * a["recall"][1] < a["recall"][0] * b["recall"][1]:
            failed = True
        print(f"{case_id:6} {pa:>10} {pb:>10} {delta:+7d}   {ra} -> {rb}   "
              f"${b['usd']:.4f}")
    base_gates, variant_gates = gates(base), gates(variant)
    for name, state in sorted(base_gates.items()):
        if variant_gates.get(name) not in (None, state) and state == "pass":
            print(f"gate {name}: {state} -> {variant_gates.get(name)}")
            failed = True
    if left["input_tokens"] and right["input_tokens"]:
        growth = (right["input_tokens"] - left["input_tokens"]) / left["input_tokens"]
        print(f"prompt tokens: {left['input_tokens']} -> {right['input_tokens']} "
              f"({growth * 100:+.1f}%)")
        if growth > 0.05:
            print("token growth above 5%: the win must survive distillation")
    print(f"total: {left['verified']}/{left['attempts']} -> {right['verified']}/{right['attempts']}"
          f"  ${left['usd']:.4f} -> ${right['usd']:.4f}")
    return 1 if failed else 0


def round_variant(args: argparse.Namespace) -> int:
    root = repository_root()
    variant = load_variant(Path(args.variant))
    out = Path(args.out).expanduser()
    apply(root, variant)
    try:
        build = ["cargo", "build", "--release", "--locked", "-q", "-p", args.package]
        print("+ " + " ".join(build))
        if subprocess.run(build, cwd=root).returncode != 0:
            return 2
        run = [sys.executable, "-B", "scripts/eval_task017.py", "run",
               "--daemon", args.daemon, "--build-profile", "release",
               "--pricing", args.pricing, "--out", str(out),
               "--cases", *variant["cases"]]
        for flag in ("base_url", "model", "upstream_key_env", "budgets"):
            value = getattr(args, flag, None)
            if value:
                run += [f"--{flag.replace('_', '-')}", value]
        if args.dry_run:
            run.append("--dry-run")
        print("+ " + " ".join(run))
        if subprocess.run(run, cwd=root).returncode != 0:
            return 3
    finally:
        revert(root, variant)
        print(f"reverted {variant['name']}")
    if args.compare_to:
        return compare(Path(args.compare_to).expanduser(), out / "report.json")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    for name in ("apply", "revert"):
        command = sub.add_parser(name)
        command.add_argument("variant")

    compare_parser = sub.add_parser("compare")
    compare_parser.add_argument("base")
    compare_parser.add_argument("variant")

    round_parser = sub.add_parser("round")
    round_parser.add_argument("variant")
    round_parser.add_argument("--out", required=True)
    round_parser.add_argument("--compare-to")
    round_parser.add_argument("--daemon", default="target/release/ditto-daemon")
    round_parser.add_argument("--package", default="ditto-daemon")
    round_parser.add_argument("--pricing", required=True)
    round_parser.add_argument("--base-url")
    round_parser.add_argument("--model")
    round_parser.add_argument("--upstream-key-env")
    round_parser.add_argument("--budgets")
    round_parser.add_argument("--dry-run", action="store_true")

    args = parser.parse_args()
    root = repository_root()
    if args.command == "apply":
        store = apply(root, load_variant(Path(args.variant)))
        print(f"applied; backups in {store}")
        return 0
    if args.command == "revert":
        revert(root, load_variant(Path(args.variant)))
        print("reverted")
        return 0
    if args.command == "compare":
        return compare(Path(args.base).expanduser(), Path(args.variant).expanduser())
    return round_variant(args)


if __name__ == "__main__":
    raise SystemExit(main())
