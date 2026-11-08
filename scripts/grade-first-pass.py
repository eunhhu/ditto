#!/usr/bin/env python3
"""First-pass answer grading for a Task 017 run: rule-based, recorded as such.

Reads the blind sheet the runner produced, re-derives each item against the
suite oracle (required/forbidden answer atoms, abstention), and writes a
grades.json the runner can import. Items whose oracle atoms are only partly met
are marked `disputed`, which routes them to the second reading.

Anything this script cannot decide is left as `disputed` rather than scored as a
pass: an unreadable answer is not evidence of a good one.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ABSTAIN = (
    "don't have", "do not have", "no record", "nothing about", "can't find",
    "cannot find", "not in the memories", "don't know", "없", "찾을 수 없", "기억에 없",
)


def norm(text: str) -> str:
    return re.sub(r"\s+", " ", text or "").strip().lower()


def main(run_dir: Path) -> int:
    suite = json.loads((ROOT / "eval/task017/suite-v1.json").read_text(encoding="utf-8"))
    cases = {c["case_id"]: c for c in suite["cases"]}
    sheet = json.loads((run_dir / "grading/sheet.json").read_text(encoding="utf-8"))
    first = []
    for item in sheet["items"]:
        case = cases[item["case_id"]]
        oracle = case["oracle"]
        answer = norm(item["answer"])
        required = [a for a in oracle.get("required_answer_atoms") or [] if a]
        forbidden = [a for a in oracle.get("forbidden_answer_atoms") or [] if a]
        met = [a for a in required if norm(a) in answer]
        hit_forbidden = [a for a in forbidden if norm(a) in answer]
        abstained = any(phrase in answer for phrase in ABSTAIN)

        critical = bool(hit_forbidden)
        grounding = 2 if not hit_forbidden else 0
        if oracle.get("must_abstain"):
            fulfillment = 2 if abstained else 0
            calibration = 2 if abstained else 0
        else:
            fulfillment = 2 if required and len(met) == len(required) else (1 if met else 0)
            calibration = 2 if (fulfillment == 2 and not critical) else 1
            if abstained and required:
                fulfillment = min(fulfillment, 1)
                calibration = 0
        clarity = 2 if len(answer) >= 80 else (1 if len(answer) >= 20 else 0)
        disputed = bool(required and len(met) != len(required)) or grounding == 0
        first.append({
            "item_id": item["item_id"],
            "assessor_id": "eunhhu-first-pass-rules",
            "grounding": grounding,
            "fulfillment": fulfillment,
            "calibration": calibration,
            "clarity": clarity,
            "critical_failure": critical,
            "disputed": disputed,
            "note": "rule-based: atoms met %d/%d%s%s" % (
                len(met), len(required),
                "; forbidden: " + ", ".join(hit_forbidden) if hit_forbidden else "",
                "; abstained" if abstained else "",
            ),
        })
    out = run_dir / "grading/first-pass.json"
    out.write_text(json.dumps({"first": first}, ensure_ascii=False, indent=1), encoding="utf-8")
    disputed = sum(1 for g in first if g["disputed"])
    passed = sum(1 for g in first if g["grounding"] == g["fulfillment"] == g["calibration"] == 2
                 and g["clarity"] >= 1 and not g["critical_failure"])
    print("items=%d pass=%d disputed=%d -> %s" % (len(first), passed, disputed, out))
    return 0


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1]).expanduser().resolve()))
