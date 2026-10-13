"""TaxBench: run an agent on a case in an isolated workspace, then grade its filled forms.

    python3 bench/run.py jane-doe --agent claude --model sonnet
    python3 bench/run.py jane-doe --agent pi --model google/gemini-flash-latest --runs 3
    python3 bench/run.py jane-doe --agent pi --model flash-next-hybrid --runs 3
The gemini agent (Gemini CLI) needs a paid Gemini API key since June 2026.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
from datetime import datetime
from pathlib import Path

from grade import grade

BENCH = Path(__file__).resolve().parent
REPO = BENCH.parent
# Variables of an enclosing Claude Code session, withheld from benchmarked agents.
SESSION_ENV_PREFIXES = ("CLAUDECODE", "CLAUDE_CODE_", "CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT", "ANTHROPIC_BASE_URL")
GEMINI_BUILTIN_TOOLS = [
    "codebase_investigator", "delegate_to_agent", "glob", "google_web_search", "list_directory", "read_file",
    "read_many_files", "replace", "run_shell_command", "save_memory", "search_file_content", "web_fetch",
    "write_file", "write_todos",
]


def server(nii: Path, workspace: Path) -> dict:
    return {"command": str(nii), "args": ["mcp"], "env": {"NII_WORKSPACE": str(workspace), "NII_NO_VIEWER": "1"}}


def claude_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str, resume: bool) -> tuple[list[str], str | None]:
    config = cwd.parent / "mcp.json"
    config.write_text(json.dumps({"mcpServers": {"nii": server(nii, workspace)}}))
    session = cwd.parent / "claude-session"
    if not resume:
        session.write_text(str(uuid.uuid4()))
    return [
        "claude", "-p", "--model", model, "--tools", "", "--mcp-config", str(config), "--strict-mcp-config",
        "--allowedTools", "mcp__nii__*", "--output-format", "stream-json", "--verbose",
        "--resume" if resume else "--session-id", session.read_text(),
    ], prompt


def gemini_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str, resume: bool) -> tuple[list[str], str | None]:
    if resume:
        raise NotImplementedError("the gemini agent cannot resume a session")
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


# What the bench shares from the operator's pi config: API keys and the provider catalog.
PI_SHARED_FILES = ("auth.json", "models-store.json")


def pi_agent_dir(home: Path, model_id: str, upstream: str | None) -> Path:
    """A bench-only pi config: default settings, the operator's keys and custom providers, and
    with `upstream`, an OpenRouter routing pin for `model_id`."""
    source = Path(os.environ.get("PI_CODING_AGENT_DIR", Path.home() / ".pi" / "agent"))
    agent = home / "pi-agent"
    if agent.exists():
        return agent
    agent.mkdir()
    for name in PI_SHARED_FILES:
        if (source / name).exists():
            (agent / name).symlink_to(source / name)
    (agent / "settings.json").write_text("{}\n")
    models_path = source / "models.json"
    models = json.loads(models_path.read_text()) if models_path.exists() else {}
    if upstream:
        openrouter = models.setdefault("providers", {}).setdefault("openrouter", {})
        override = openrouter.setdefault("modelOverrides", {}).setdefault(model_id, {})
        override.setdefault("compat", {})["openRouterRouting"] = {"only": [upstream], "allow_fallbacks": False}
    (agent / "models.json").write_text(json.dumps(models, indent=2))
    return agent


def pi_command(model: str, nii: Path, workspace: Path, cwd: Path, prompt: str, resume: bool) -> tuple[list[str], str | None]:
    config = cwd / ".pi" / "mcp.json"
    config.parent.mkdir(parents=True, exist_ok=True)
    config.write_text(json.dumps({
        "mcpServers": {"nii": {**server(nii, workspace), "exposure": "direct"}},
        "autoEnableCodemode": False,
    }, indent=2))
    provider, model_id = model.split("/", 1) if "/" in model else (pi_provider(model), model)
    return [
        "pi", "-p", "--provider", provider, "--model", model_id, "--mode", "json",
        "--session-dir", str(cwd.parent / "pi-sessions"), *(["--continue"] if resume else []), "--no-context-files", "--no-extensions", "-e", "builtin:mcp", "--no-skills",
        "--no-prompt-templates", "--no-themes", "--offline", "--approve", "--tools", "mcp__nii__*", prompt,
    ], None


AGENTS = {"claude": claude_command, "gemini": gemini_command, "pi": pi_command}
RESUMABLE = {"claude", "pi"}
RESUME_PROMPT = "Continue."
NUDGE_PROMPT = "Please continue until the return is complete: every form it needs filled out and checked."


def tool_version() -> str:
    """Git commit of the tools under test, marked dirty if their code has uncommitted changes."""
    git = lambda *args: subprocess.run(["git", *args], cwd=REPO, capture_output=True, text=True).stdout.strip()
    dirty = git("status", "--porcelain", "--", "apps", "crates")
    return git("rev-parse", "--short", "HEAD") + ("-dirty" if dirty else "")


def run_once(case: str, agent: str, model: str, nii: Path, timeout: int, index: int, max_resumes: int,
             max_nudges: int, upstream: str | None) -> dict:
    spec = json.loads((BENCH / case / "case.json").read_text())
    answers = json.loads((BENCH / case / "answers.json").read_text())
    prompt = (BENCH / case / "prompt.txt").read_text()
    year = str(spec["tax_year"])
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    label = model.replace("/", "_") + (f"@{upstream.replace(' ', '-')}" if upstream else "")
    out = BENCH / "runs" / case / f"{stamp}-{agent}-{label}-{index}"
    out.mkdir(parents=True)

    with tempfile.TemporaryDirectory(prefix="taxbench-") as tmp:
        workspace, cwd = Path(tmp) / "ws", Path(tmp) / "cwd"
        (workspace / year).mkdir(parents=True)
        cwd.mkdir()
        for item in spec["copy"]:
            src = REPO / spec["inputs"] / item
            dst = workspace / year / item
            shutil.copytree(src, dst) if src.is_dir() else shutil.copy2(src, dst)

        env = {k: v for k, v in os.environ.items() if not k.startswith(SESSION_ENV_PREFIXES)}
        if agent == "pi":
            env["PI_CODING_AGENT_DIR"] = str(pi_agent_dir(Path(tmp), model.split("/", 1)[-1], upstream))
        started = time.monotonic()
        stdout = stderr = ""
        resumes = nudges = 0
        message = prompt
        while True:
            cmd, stdin = AGENTS[agent](model, nii, workspace, cwd, message, bool(resumes or nudges))
            remaining = timeout - (time.monotonic() - started)
            if remaining <= 0:
                exit_code = "timeout"
                break
            try:
                proc = subprocess.run(cmd, cwd=cwd, input=stdin or "", capture_output=True, text=True,
                                      timeout=max(remaining, 1), env=env)
                exit_code, out_text, err_text = proc.returncode, proc.stdout, proc.stderr
            except subprocess.TimeoutExpired as e:
                exit_code = "timeout"
                out_text = e.stdout.decode() if isinstance(e.stdout, bytes) else e.stdout or ""
                err_text = e.stderr.decode() if isinstance(e.stderr, bytes) else e.stderr or ""
            stdout += out_text
            stderr += err_text
            if exit_code != 0 or provider_error(out_text) or agent not in RESUMABLE:
                break
            if dropped_call(out_text) and resumes < max_resumes:
                resumes += 1
                message = RESUME_PROMPT
            elif "fill_form" not in tools_called(stdout) and nudges < max_nudges:
                nudges += 1
                message = NUDGE_PROMPT
            else:
                break
        elapsed = time.monotonic() - started

        (out / "transcript.jsonl").write_text(stdout)
        (out / "stderr.log").write_text(stderr)
        forms = workspace / year / "forms"
        if forms.exists():
            shutil.copytree(forms, out / "forms")

    report = grade(answers, out / "forms", str(nii))
    status = outcome(exit_code, stdout)
    tool_calls = len(tools_called(stdout))
    meta = {
        "case": case, "agent": agent, "model": model, "upstream": upstream, "status": status, "exit": exit_code,
        "seconds": round(elapsed, 1), "resumes": resumes, "nudges": nudges,
        "tool_version": tool_version(), "tool_calls": tool_calls, "score": report["passed"], "total": report["total"],
        "net_refund": report["net_refund"], "outcome_error": report["outcome_error"],
        "outcome_correct": report["outcome_correct"], "usage": usage(stdout),
        "return_complete": report["return_complete"],
        "consistency_problems": report["consistency_problems"],
        "unneeded_forms": report["unneeded_forms"], "command": cmd,
    }
    for event in events(stdout):
        if event.get("type") == "result":
            meta["result_event"] = event
    (out / "meta.json").write_text(json.dumps(meta, indent=2, default=str))
    (out / "grade.json").write_text(json.dumps(report, indent=2, default=str))
    refund = "refund correct" if report["outcome_correct"] else f"refund off by {report['outcome_error']}"
    restarts = f", {resumes} resumed, {nudges} nudged" if resumes or nudges else ""
    print(f"{out.relative_to(REPO)}: {status}, {report['passed']}/{report['total']}, {refund}, {elapsed:.0f}s{restarts}",
          flush=True)
    if status == "provider_error":
        print(f"   {provider_error(stdout)}", flush=True)
    elif status == "agent_error" and agent_error(stdout):
        print(f"   {agent_error(stdout)}", flush=True)
    elif status == "completed":
        for stem, key, want, got, ok in report["results"]:
            if not ok:
                print(f"   FAIL {stem:8} {key[:60]!r}: want {want!r}, got {got!r}", flush=True)
    return meta


def events(stdout: str):
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            yield event


def tools_called(stdout: str) -> list[str]:
    """Names of the MCP tools a transcript called, without the server prefix, in order."""
    names = []
    for event in events(stdout):
        if event.get("type") == "tool_execution_start":
            names.append(event.get("toolName", ""))
        elif event.get("type") == "assistant" and isinstance(event.get("message"), dict):
            names += [c.get("name", "") for c in event["message"].get("content", []) if c.get("type") == "tool_use"]
    return [n.rsplit("__", 1)[-1] for n in names]


def dropped_call(stdout: str) -> bool:
    """Whether the session ended on a turn with reasoning but neither text nor a tool call."""
    last = None
    for event in events(stdout):
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        if event.get("type") == "message_end" and message.get("role") == "assistant":
            last = message
    if last is None or last.get("stopReason") != "stop":
        return False
    content = last.get("content") or []
    has_text = any(c.get("type") == "text" and c.get("text", "").strip() for c in content)
    has_call = any(c.get("type") == "toolCall" for c in content)
    return not has_text and not has_call


def usage(stdout: str) -> dict:
    """Token totals across the run's model calls (pi and Claude Code transcripts)."""
    total = {"input": 0, "cached": 0, "output": 0, "cost": 0.0}
    for event in events(stdout):
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        if event.get("type") == "message_end" and message.get("role") == "assistant":
            u = message.get("usage") or {}
            total["input"] += u.get("input", 0)
            total["cached"] += u.get("cacheRead", 0)
            total["output"] += u.get("output", 0)
            total["cost"] += (u.get("cost") or {}).get("total", 0.0)
        if event.get("type") == "result" and isinstance(event.get("usage"), dict):
            u = event["usage"]
            total["input"] += u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0)
            total["cached"] += u.get("cache_read_input_tokens", 0)
            total["output"] += u.get("output_tokens", 0)
            total["cost"] += event.get("total_cost_usd", 0.0)
    return total


def outcome(exit_code, stdout: str) -> str:
    """completed, timeout, provider_error (the model API failed, e.g. 429), or agent_error."""
    if exit_code == "timeout":
        return "timeout"
    if provider_error(stdout):
        return "provider_error"
    return "completed" if exit_code == 0 and not agent_error(stdout) else "agent_error"


API_ERROR = re.compile(r"rate.?limit|overloaded|too many requests|\b(429|500|502|503|529)\b|internal server|connection",
                       re.IGNORECASE)


def provider_error(stdout: str) -> str | None:
    """The last model-API error in a transcript, if the run ended on one. pi marks these with
    stopReason "error"; Claude Code reports them as an error result whose text names the API failure."""
    last = None
    for event in events(stdout):
        message = event.get("message") if isinstance(event.get("message"), dict) else {}
        if event.get("type") == "message_end" and message.get("role") == "assistant":
            last = message.get("errorMessage") if message.get("stopReason") == "error" else None
        if event.get("type") == "result" and event.get("is_error"):
            text = str(event.get("result"))
            last = text if API_ERROR.search(text) else None
    return last[:300] if last else None


def agent_error(stdout: str) -> str | None:
    """A Claude Code error result that is the agent's own doing (max turns, budget), not the API's."""
    for event in events(stdout):
        if event.get("type") == "result" and event.get("is_error") and not API_ERROR.search(str(event.get("result"))):
            return f"{event.get('subtype')}: {str(event.get('result'))[:200]}"
    return None


def main():
    p = argparse.ArgumentParser()
    p.add_argument("case")
    p.add_argument("--agent", choices=AGENTS, required=True)
    p.add_argument("--model", required=True, nargs="+", help="one or more models, run in order")
    p.add_argument("--runs", type=int, default=1)
    p.add_argument("--timeout", type=int, default=1800, help="seconds per run")
    p.add_argument("--retries", type=int, default=2, help="extra attempts for a run that ends in a provider error")
    p.add_argument("--retry-wait", type=int, default=30, help="seconds to wait before retrying a provider error")
    p.add_argument("--resumes", type=int, default=5,
                   help="times to resume a session that ended on a dropped tool call (reasoning, no text or call)")
    p.add_argument("--nudges", type=int, default=1,
                   help="times to ask an agent that stopped before filling any form to continue")
    p.add_argument("--upstream", help="pin pi's OpenRouter models to this upstream provider, e.g. Cerebras")
    p.add_argument("--nii", type=Path, default=REPO / "target" / "release" / "nii")
    a = p.parse_args()
    if a.upstream and (a.agent != "pi" or not all(m.startswith("openrouter/") for m in a.model)):
        sys.exit("--upstream needs --agent pi and openrouter/... models")
    if not a.nii.exists():
        sys.exit(f"{a.nii} not found: cargo build --release -p nii")
    for model in a.model:
        runs = []
        for i in range(a.runs):
            for attempt in range(a.retries + 1):
                if attempt:
                    print(f"   provider error; retry {attempt}/{a.retries} in {a.retry_wait}s", flush=True)
                    time.sleep(a.retry_wait)
                runs.append(run_once(a.case, a.agent, model, a.nii.resolve(), a.timeout, i, a.resumes, a.nudges,
                                     a.upstream))
                if runs[-1]["status"] != "provider_error":
                    break
        scores = [r["score"] for r in runs if r["status"] == "completed"]
        other = [r["status"] for r in runs if r["status"] != "completed"]
        cost = sum(r["usage"]["cost"] for r in runs)
        if a.runs > 1 or len(a.model) > 1:
            mean = f"mean {sum(scores) / len(scores):.1f}" if scores else "no completed runs"
            not_scored = f"; not scored: {', '.join(other)}" if other else ""
            print(f"{model}: completed scores {scores}, {mean}{not_scored}; ${cost:.2f}", flush=True)


if __name__ == "__main__":
    main()
