"""Render the TaxBench results card (docs/taxbench.png) from bench/runs.

    uv run --with matplotlib python bench/card/make_card.py
"""

import json
import pathlib
import statistics

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch, Patch

REPO = pathlib.Path(__file__).resolve().parents[2]
RUNS = REPO / "bench" / "runs" / "jane-doe"
OUT = REPO / "docs" / "taxbench.png"

SURFACE = "#1a1a19"
TEXT_PRIMARY = "#f0efec"
TEXT_SECONDARY = "#c3c2b7"
TEXT_MUTED = "#8a897f"
GRID = "#2e2e2c"
HOSTED = "#3987e5"
LOCAL = "#d95926"

NAMES = {
    "sonnet": "Claude Sonnet 5.5",
    "openrouter/anthropic/claude-haiku-5.5": "Claude Haiku 5.5",
    "openrouter/openai/gpt-6-luna": "GPT-6 Luna",
    "flash-next-hybrid": "Qwen3.8 Flash-Next (local, Q4)",
    "google/gemini-3.5-flash-lite": "Gemini 3.5 Flash-Lite",
    "openrouter/deepseek/deepseek-v4-flash": "DeepSeek V4 Flash",
    "openrouter/qwen/qwen3.6-35b-a3b": "Qwen 3.6 35B-A3B",
    "openrouter/qwen/qwen3-next-80b-a3b-instruct": "Qwen3-Next 80B-A3B",
    "openrouter/google/gemma-4-26b-a4b-it": "Gemma 4 26B-A4B",
    "openrouter/mistralai/mistral-small-3.2-24b-instruct": "Mistral Small 3.2 24B",
    "openrouter/mistralai/ministral-3b-2512": "Ministral 3B",
    "openrouter/nvidia/nemotron-3-nano-30b-a3b": "Nemotron 3 Nano 30B",
    "openrouter/mistralai/ministral-8b-2512": "Ministral 8B",
    "openrouter/ibm-granite/granite-4.2-8b": "Granite 4.2 8B",
    "openrouter/google/gemma-4-31b-it": "Gemma 4 31B",
    "openrouter/openai/gpt-5-nano": "GPT-5 nano",
    "openrouter/nvidia/nemotron-3-super-120b-a12b": "Nemotron 3 Super 120B",
    "openrouter/openai/gpt-oss-120b": "gpt-oss-120b",
    "openrouter/openai/gpt-oss-20b": "gpt-oss-20b",
    "openrouter/z-ai/glm-5.3": "GLM 5.3",
    "openrouter/deepseek/deepseek-v4-pro-0813": "DeepSeek V4 Pro",
    "vllm-local/qwen3.8-27b": "Qwen3.8 27B (local, FP8)",
}
LOCAL_MODELS = {"flash-next-hybrid", "vllm-local/qwen3.8-27b"}


def results() -> list[dict]:
    by_model: dict[str, list[dict]] = {}
    for d in sorted(RUNS.iterdir()):
        meta = d / "meta.json"
        if meta.exists():
            m = json.loads(meta.read_text())
            if m.get("status", "completed") == "completed":
                key = m["model"] + (f"@{m['upstream']}" if m.get("upstream") else "")
                by_model.setdefault(key, []).append(m)
    rows = []
    for model, runs in by_model.items():
        runs = runs[-3:]
        base, _, upstream = model.partition("@")
        rows.append({
            "model": model,
            "name": NAMES.get(base, base) + (f" via {upstream}" if upstream else ""),
            "total": runs[0]["total"],
            "mean": statistics.mean(r["score"] for r in runs),
            "right": sum(bool(r.get("outcome_correct")) for r in runs),
            "n": len(runs),
            "local": base in LOCAL_MODELS,
        })
    return sorted(rows, key=lambda r: (-r["mean"], r["name"]))


def main():
    rows = results()
    total = rows[0]["total"]
    assert all(r["total"] == total for r in rows), "runs graded against different answer keys"
    height = 1.05 + 0.34 * len(rows) + 0.75
    fig = plt.figure(figsize=(12, height), dpi=110, facecolor=SURFACE)
    ax = fig.add_axes([0.255, 0.75 / height, 0.6, 0.34 * len(rows) / height])
    ax.set_facecolor(SURFACE)

    y = list(range(len(rows)))[::-1]
    for yi, r in zip(y, rows):
        width = max(r["mean"], 0.25)
        ax.add_patch(FancyBboxPatch(
            (0, yi - 0.3), width, 0.6,
            boxstyle="round,pad=0,rounding_size=0.12",
            mutation_aspect=1 / 6,
            facecolor=LOCAL if r["local"] else HOSTED,
            edgecolor="none",
        ))
        ax.text(width + 0.6, yi, f"{r['mean']:.1f}", va="center", ha="left", color=TEXT_PRIMARY, fontsize=10.5)
        mark = "✓ " if r["right"] == r["n"] else ""
        ax.text(total + 9.5, yi, f"{mark}{r['right']}/{r['n']}", va="center", ha="right",
                color=TEXT_PRIMARY if r["right"] == r["n"] else TEXT_MUTED, fontsize=10.5)

    ax.set_xlim(0, total)
    ax.set_ylim(-0.6, len(rows) - 0.4)
    ax.set_yticks(y, [r["name"] for r in rows], color=TEXT_SECONDARY, fontsize=10.5)
    ax.set_xticks([0, 10, 20, 30, 40, 46], ["0", "10", "20", "30", "40", "46"], color=TEXT_MUTED, fontsize=9)
    ax.tick_params(length=0)
    ax.grid(axis="x", color=GRID, linewidth=0.8)
    ax.set_axisbelow(True)
    for spine in ax.spines.values():
        spine.set_visible(False)
    ax.text(total + 9.5, len(rows) - 0.2, "refund right", ha="right", va="bottom", color=TEXT_MUTED, fontsize=9)
    ax.text(0, len(rows) - 0.2, "form lines correct (mean of 3 runs, out of 46)", ha="left", va="bottom",
            color=TEXT_MUTED, fontsize=9)

    fig.text(0.03, 1 - 0.38 / height, "TaxBench: can a model + MCP do simple-ish taxes?", color=TEXT_PRIMARY,
             fontsize=19, fontweight="bold", va="center")
    fig.text(0.03, 1 - 0.75 / height,
             "One fictional 2025 return (head of household, dependent care FSA, wash sale, foreign tax credit), "
             "filled end to end on real IRS forms.",
             color=TEXT_SECONDARY, fontsize=10.5, va="center")
    fig.legend(handles=[Patch(color=HOSTED, label="hosted API"), Patch(color=LOCAL, label="local model")],
               loc="upper right", bbox_to_anchor=(0.975, 1 - 0.2 / height), frameon=False,
               labelcolor=TEXT_SECONDARY, fontsize=10, handlelength=1.2, ncols=2)
    fig.text(0.03, 0.3 / height,
             "Agents: Claude Code (Sonnet) and pi (others), same 9 tools, isolated workspace. Timeouts and provider "
             "errors are excluded from the mean.\ngpt-oss models plan the return and then stop without filling forms. "
             "github.com/DerekParks/NotIntoIt",
             color=TEXT_MUTED, fontsize=8.5, va="center", linespacing=1.5)
    OUT.parent.mkdir(exist_ok=True)
    fig.savefig(OUT, facecolor=SURFACE)
    print(OUT, len(rows), "models")


if __name__ == "__main__":
    main()
