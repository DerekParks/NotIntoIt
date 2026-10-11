"""Summarize TaxBench runs: per-model results, then the lines models miss most.

    python3 bench/report.py jane-doe [--markdown]
"""

import json
import statistics
import sys
from collections import defaultdict
from pathlib import Path

from grade import grade

BENCH = Path(__file__).resolve().parent
NII = str(BENCH.parent / "target" / "release" / "nii")


def load(case: str) -> list[dict]:
    answers = json.loads((BENCH / case / "answers.json").read_text())
    runs = []
    for d in sorted((BENCH / "runs" / case).iterdir()):
        meta_path = d / "meta.json"
        if not meta_path.exists():
            continue
        meta = json.loads(meta_path.read_text())
        meta["report"] = grade(answers, d / "forms", NII)
        meta["dir"] = d.name
        runs.append(meta)
    return runs


def summarize(runs: list[dict]) -> list[str]:
    by_model = defaultdict(list)
    for r in runs:
        by_model[f"{r['agent']}:{r['model']}"].append(r)
    rows = [("model", "runs", "scored", "lines", "refund right", "median $ off", "secs", "$ spent", "other")]
    for model, rs in sorted(by_model.items()):
        done = [r for r in rs if r.get("status", "completed") == "completed"]
        other = [r.get("status") for r in rs if r.get("status", "completed") != "completed"]
        lines = [r["report"]["passed"] for r in done]
        errors = [r["report"]["outcome_error"] for r in done if r["report"]["outcome_error"] is not None]
        missing = sum(r["report"]["outcome_error"] is None for r in done)
        total = done[0]["report"]["total"] if done else 0
        rows.append((
            model,
            str(len(rs)),
            str(len(done)),
            f"{statistics.mean(lines):.1f}/{total}" if lines else "-",
            f"{sum(r['report']['outcome_correct'] for r in done)}/{len(done)}" if done else "-",
            (f"{statistics.median(errors):,.0f}" if errors else "-") + (f" ({missing} blank)" if missing else ""),
            f"{statistics.mean(r['seconds'] for r in done):.0f}" if done else "-",
            f"{sum(r.get('usage', {}).get('cost', 0) for r in rs):.2f}",
            ", ".join(sorted(set(other))) or "",
        ))
    return rows


def misses(runs: list[dict]) -> list[tuple]:
    done = [r for r in runs if r.get("status", "completed") == "completed"]
    counts = defaultdict(list)
    for r in done:
        for stem, key, _want, _got, ok in r["report"]["results"]:
            if not ok:
                counts[(stem, key)].append(r["model"])
    ranked = sorted(counts.items(), key=lambda kv: -len(kv[1]))
    return [(stem, key, len(models), len(done), sorted(set(models))) for (stem, key), models in ranked]


def table(rows: list[tuple], markdown: bool) -> str:
    if markdown:
        head, *body = rows
        out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
        return "\n".join(out + ["| " + " | ".join(r) + " |" for r in body])
    widths = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
    return "\n".join("  ".join(c.ljust(w) for c, w in zip(r, widths)) for r in rows)


def main():
    case = sys.argv[1]
    markdown = "--markdown" in sys.argv
    runs = load(case)
    print(table(summarize(runs), markdown))
    print()
    rows = [("form", "line", "missed", "by")]
    for stem, key, n, total, models in misses(runs):
        rows.append((stem, key[:60], f"{n}/{total}", ", ".join(m.split("/")[-1] for m in models)))
    print(table(rows, markdown))


if __name__ == "__main__":
    main()
