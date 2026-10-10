"""TaxBench: run an agent on a case in an isolated workspace, then grade its filled forms.

    python3 bench/run.py jane-doe --agent claude --model sonnet
    python3 bench/run.py jane-doe --agent pi --model google/gemini-flash-latest --runs 3
    python3 bench/run.py jane-doe --agent pi --model flash-next-hybrid --runs 3
The gemini agent (Gemini CLI) needs a paid Gemini API key since June 2026.
"""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime
from pathlib import Path

from grade import grade

BENCH = Path(__file__).resolve().parent
REPO = BENCH.parent
GEMINI_BUILTIN_TOOLS = [
    "codebase_investigator", "delegate_to_agent", "glob", "google_web_search", "list_directory", "read_file",
    "read_many_files", "replace", "run_shell_command", "save_memory", "search_file_content", "web_fetch",
    "write_file", "write_todos",
]


def server(nii: Path, workspace: Path) -> dict:
    return {"command": str(nii), "args": ["mcp"], "env": {"NII_WORKSPACE": str(workspace)}}


def claude_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str) -> tuple[list[str], str | None]:
    config = cwd.parent / "mcp.json"
    config.write_text(json.dumps({"mcpServers": {"nii": server(nii, workspace)}}))
    return [
        "claude", "-p", "--model", model, "--tools", "", "--mcp-config", str(config), "--strict-mcp-config",
        "--allowedTools", "mcp__nii__*", "--output-format", "stream-json", "--verbose",
    ], prompt


def gemini_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str) -> tuple[list[str], str | None]:
    settings = cwd / ".gemini" / "settings.json"
    settings.parent.mkdir(parents=True)
    settings.write_text(json.dumps({
        "mcpServers": {"nii": {**server(nii, workspace), "trust": True}},
        "tools": {"exclude": GEMINI_BUILTIN_TOOLS},
    }, indent=2))
    return [
        "gemini", "-m", model, "--allowed-mcp-server-names", "nii", "--approval-mode", "yolo",
        "-o", "stream-json", prompt,
    ], None


def pi_provider(model: str) -> str:
    """The provider pi lists for an exact model id."""
    listing = subprocess.run(["pi", "--offline", "--list-models", model], capture_output=True, text=True,
                             stdin=subprocess.DEVNULL).stdout
    providers = [cols[0] for cols in (line.split() for line in listing.splitlines()[1:]) if len(cols) > 1 and cols[1] == model]
    if len(providers) != 1:
        sys.exit(f"pi model {model!r} matched providers {providers}; pass --model provider/id")
    return providers[0]


def pi_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str) -> tuple[list[str], str | None]:
    config = cwd / ".pi" / "mcp.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({
        "mcpServers": {"nii": {**server(nii, workspace), "exposure": "direct"}},
        "autoEnableCodemode": False,
    }, indent=2))
    provider, _, model_id = model.rpartition("/")
    return [
        "pi", "-p", "--provider", provider or pi_provider(model_id), "--model", model_id, "--mode", "json",
        "--no-session", "--no-context-files", "--no-extensions", "-e", "builtin:mcp", "--no-skills",
        "--no-prompt-templates", "--no-themes", "--offline", "--approve", "--tools", "mcp__nii__*", prompt,
    ], None


AGENTS = {"claude": claude_command, "gemini": gemini_command, "pi": pi_command}


def run_once(case: str, agent: str, model: str, nii: Path, timeout: int, index: int) -> dict:
    spec = json.loads((BENCH / case / "case.json").read_text())
    answers = json.loads((BENCH / case / "answers.json").read_text())
    prompt = (BENCH / case / "prompt.txt").read_text()
    year = str(spec["tax_year"])
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    out = BENCH / "runs" / case / f"{stamp}-{agent}-{model.replace('/', '_')}-{index}"
    out.mkdir(parents=True)

    with tempfile.TemporaryDirectory(prefix="taxbench-") as tmp:
        workspace, cwd = Path(tmp) / "ws", Path(tmp) / "cwd"
        (workspace / year).mkdir(parents=True)
        cwd.mkdir()
        for item in spec["copy"]:
            src = REPO / spec["inputs"] / item
            dst = workspace / year / item
            shutil.copytree(src, dst) if src.is_dir() else shutil.copy2(src, dst)

        cmd, stdin = AGENTS[agent](model, nii, workspace, cwd, prompt)
        started = time.monotonic()
        try:
            proc = subprocess.run(cmd, cwd=cwd, input=stdin or "", capture_output=True, text=True, timeout=timeout)
            exit_code, stdout, stderr = proc.returncode, proc.stdout, proc.stderr
        except subprocess.TimeoutExpired as e:
            exit_code, stdout, stderr = "timeout", e.stdout or "", e.stderr or ""
            stdout = stdout.decode() if isinstance(stdout, bytes) else stdout
            stderr = stderr.decode() if isinstance(stderr, bytes) else stderr
        elapsed = time.monotonic() - started

        (out / "transcript.jsonl").write_text(stdout)
        (out / "stderr.log").write_text(stderr)
        forms = workspace / year / "forms"
        if forms.exists():
            shutil.copytree(forms, out / "forms")

    report = grade(answers, out / "forms", str(nii))
    tool_calls = sum(stdout.count(marker) for marker in ('"type":"tool_use"', '"type": "tool_use"', '"type":"tool_execution_start"'))
    meta = {
        "case": case, "agent": agent, "model": model, "exit": exit_code, "seconds": round(elapsed, 1),
        "tool_calls": tool_calls, "score": report["passed"], "total": report["total"],
        "unneeded_forms": report["unneeded_forms"], "command": cmd,
    }
    for line in stdout.splitlines():
        if '"type":"result"' in line or '"type": "result"' in line:
            meta["result_event"] = json.loads(line)
    (out / "meta.json").write_text(json.dumps(meta, indent=2, default=str))
    (out / "grade.json").write_text(json.dumps(report, indent=2, default=str))
    print(f"{out.relative_to(REPO)}: {report['passed']}/{report['total']} in {elapsed:.0f}s, exit {exit_code}")
    for stem, key, want, got, ok in report["results"]:
        if not ok:
            print(f"   FAIL {stem:8} {key[:60]!r}: want {want!r}, got {got!r}")
    return meta


def main():
    p = argparse.ArgumentParser()
    p.add_argument("case")
    p.add_argument("--agent", choices=AGENTS, required=True)
    p.add_argument("--model", required=True)
    p.add_argument("--runs", type=int, default=1)
    p.add_argument("--timeout", type=int, default=1800, help="seconds per run")
    p.add_argument("--nii", type=Path, default=REPO / "target" / "release" / "nii")
    a = p.parse_args()
    if not a.nii.exists():
        sys.exit(f"{a.nii} not found: cargo build --release -p nii")
    scores = [run_once(a.case, a.agent, a.model, a.nii.resolve(), a.timeout, i)["score"] for i in range(a.runs)]
    if a.runs > 1:
        print(f"scores: {scores}, mean {sum(scores) / len(scores):.1f}")


if __name__ == "__main__":
    main()
