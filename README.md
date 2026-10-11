# NotIntoIt

<p align="center"><img src="docs/logo.png" alt="You wouldn't vibe your taxes" width="640"></p>

> [!WARNING]
> **NotIntoIt is a proof of concept, not tax software, and not tax advice.**
> AI models make mistakes, including ones that change what you owe. Review
> every form it fills against your documents and the IRS instructions before
> you sign or file anything. You are solely responsible for what you send to
> the IRS. NotIntoIt never e-files.

LLM-assisted tax prep for people who are not into it. NotIntoIt is an MCP
server that gives an AI assistant what it needs to prepare a US federal return
on real IRS forms: it reads your documents, fetches and fills the actual
irs.gov PDFs, shows you the result, does the math exactly, and grounds its
answers in the Internal Revenue Code and the IRS's own instructions.

![Claude Code preparing a fictional taxpayer's 2025 return with NotIntoIt, ending on the filled Form 1040](docs/demo.gif)

*Claude Sonnet prepares [Jane Doe's](examples/jane-doe) 2025 return from her
W-2, 1099s, and notes: seven forms, about 25 tool calls, two minutes (sped up
4× in the middle). The result:*

[![The filled 2025 Form 1040, pages 1 and 2](docs/filled-1040.png)](docs/filled-1040.png)

You get filled PDFs to review, print, and file yourself. Tax data you share
with an assistant goes to that assistant's model provider (or stays on your
machine with a local model); everything else runs locally.

## Status

Working end to end for tax year 2025 (and 2024 tax computation). Claude Sonnet
prepares the [example taxpayer's](examples/jane-doe) complete return —
Form 1040 with Schedules 1, 3, D, 8812 and Forms 8949 and 2441 — at 46/46
on [TaxBench](#taxbench), and so do cheaper hosted, open-weights, and local models. Not yet: state returns, and scanned documents beyond
what a vision model can read.

## Install

Linux and macOS:

```sh
curl -LsSf https://github.com/DerekParks/NotIntoIt/releases/latest/download/nii-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/DerekParks/NotIntoIt/releases/latest/download/nii-installer.ps1 | iex"
```

![Installing nii on a fresh Ubuntu container and fetching a Form 1040](docs/install.gif)

Prebuilt for Linux x86_64 and arm64 (glibc 2.38+, e.g. Ubuntu 24.04), macOS
on Apple Silicon, and Windows x86_64 (needs the Visual C++ runtime); archives
are on the [releases page](https://github.com/DerekParks/NotIntoIt/releases).
Elsewhere, build from source with Rust 1.89+:

```sh
cargo install --git https://github.com/DerekParks/NotIntoIt nii --locked
```

Either way you get one binary, `nii`. It is self-contained apart from pdfium,
which it downloads on first use.

## Use it from an assistant (MCP)

Claude Code:

```sh
claude mcp add --scope user notintoit -- nii mcp
```

Claude Desktop — add to `claude_desktop_config.json` (use the absolute path
from `which nii`):

```json
{
  "mcpServers": {
    "notintoit": { "command": "/home/you/.cargo/bin/nii", "args": ["mcp"] }
  }
}
```

Any MCP client works the same way. Put your documents in
`~/Taxes/2025/` and ask something like *"Prepare my 2025 federal return from
the documents in my tax workspace."*

### Tools

| tool | what it does |
|---|---|
| `list_documents` / `read_document` | Lists and reads files in the tax workspace: PDF text page by page (scans are flagged for `view_pdf`), notes, CSVs. Nothing outside the workspace is readable. |
| `fetch_irs_form` | Downloads a form for a tax year from irs.gov into `~/Taxes/<year>/forms/`. Takes everyday names ("Schedule D", "8812", "W-2") as well as IRS file stems. Continuous-use forms (1099s) resolve to the revision in effect. Keeps an existing working copy unless told to overwrite. |
| `list_form_fields` | Lists a form's fields with a plain-English description of each (from the IRS form's own accessibility text), type, page, length limit, and current value. Long forms list a page at a time so the listing fits in a client's tool-output limit. |
| `fill_form` | Fills fields by name, or any unambiguous abbreviation of one (a suffix, a path missing a subform, indexes left off). Writes every valid value, rejects the rest one by one with the reason (unknown or ambiguous names with "did you mean", over-length values, wrong types), and echoes each written value next to its line's description. Can be called repeatedly. |
| `view_pdf` | Renders a page with its filled values as an image, lists those values as text, and can open the PDF in your viewer. |

Every tool stays inside the tax workspace: documents are read from it, forms are fetched into it, and `fill_form` and `view_pdf` refuse PDFs or output paths anywhere else.
| `calculate` | Exact arithmetic for form lines, so the model never adds in its head. |
| `compute_tax` | Form 1040 line 16 for 2024 and 2025 (Tax Table, Tax Computation Worksheet, or Qualified Dividends and Capital Gain Tax Worksheet), every line shown. Reproduces all 2,060 rows of each year's official Tax Table. |
| `search_tax_law` | Hybrid keyword + semantic search over Title 26 of the U.S. Code and the IRS instructions for Form 1040, Schedule D, Forms 8949 and 2441, Schedule 8812, and Publications 17, 501, 503, 550. Statute hits carry a caveat when a recent amendment may not apply to every tax year. |

### First run

On startup the server downloads the prebuilt search index for its version from
the GitHub release (about 10 MB, checksum-verified; ready in seconds) and the
local embedding model it uses for queries (65 MB). If the download isn't
available, it builds the index itself instead: it fetches Title 26 from
uscode.house.gov and the IRS instructions and publications, then embeds about
15k chunks on your CPU (roughly 20 minutes). `search_tax_law` reports progress
until the index is ready; the other tools work immediately, and a failed build
retries on its own. `nii corpus fetch` and `nii corpus build` do the same from
the command line; `NII_INDEX_URL=none` skips the download.

## Where things live

| | default | override |
|---|---|---|
| tax workspace (your documents and forms) | `~/Taxes` | `--workspace` / `NII_WORKSPACE` |
| caches, model, index, pdfium | platform data dir (`~/.local/share/notintoit`, `~/Library/Application Support/notintoit`) | `--data-dir` / `NII_DATA_DIR` |
| whether `view_pdf` may open your PDF viewer | yes | `NII_NO_VIEWER=1` turns it off (headless and sandboxed servers) |

## CLI

Everything the MCP server does is also a command:

```sh
nii form fetch 1040 --year 2025               # prints the working copy's path
nii form fields ~/Taxes/2025/forms/f1040.pdf --page 1
nii form fill ~/Taxes/2025/forms/f1040.pdf --values answers.json
nii form view ~/Taxes/2025/forms/f1040.pdf    # opens your viewer; --png out.png to render
nii form flatten f1040.pdf --output print.pdf # burn values into the page for printing
nii corpus build                              # build the search index in the foreground
nii search "home office deduction"
```

## TaxBench

How small a model can still do your taxes? `bench/` runs an agent on a
fictional taxpayer in an isolated workspace (only the taxpayer's notes and
documents, only NotIntoIt's tools) and grades the filled PDFs line by line
against an answer key.

![TaxBench results: form lines correct out of 46 and refunds right, for 24 models](docs/taxbench.png)

| model | lines (mean /46) | refund right | $ per run |
|---|---|---|---|
| Claude Sonnet 5.5 (Claude Code) | 46.0 | 3/3 | 0.52 |
| DeepSeek V4 Pro | 46.0 | 2/2 | 0.30 |
| Qwen3.8 Flash-Next, local Q4 | 46.0 | 3/3 | — |
| GPT-6 Luna | 45.7 | 3/3 | 0.03 |
| GLM 5.3 | 45.0 | 3/3 | 0.24 |
| Claude Haiku 5.5 | 43.3 | 2/3 | 0.10 |
| Qwen3.8 27B, local FP8 | 40.0 | 1/3 | — |
| Gemini 3.5 Flash-Lite | 39.0 | 1/3 | 0.14 |
| DeepSeek V4 Flash | 37.3 | 0/3 | 0.05 |
| Qwen 3.6 35B-A3B | 25.7 | 0/3 | 0.26 |
| Gemma 4 26B-A4B | 19.7 | 0/3 | 0.07 |
| Ministral 8B | 17.3 | 0/3 | 0.04 |
| Qwen3-Next 80B-A3B | 16.0 | 0/3 | 0.15 |
| Gemma 4 31B | 13.7 | 0/3 | 0.12 |
| Nemotron 3 Nano 30B | 11.3 | 0/3 | 0.04 |
| 9 more models under 7 lines | 0–6.3 | none | — |

Every model at 45 lines or more gets Jane's refund right to the dollar, and
two of them (DeepSeek V4 Pro, GLM 5.3) are open weights. The most common miss
just below that is filing Jane as Single instead of head of household, which
flows through the standard deduction to the refund; further down, Form 2441
Part I/II and the $15 foreign tax credit go wrong. The gpt-oss models plan the
return and then stop without filling a form. Timeouts and provider errors are
excluded from the means, so DeepSeek V4 Pro rests on two runs. Every model
except Sonnet ran under pi; `bench/card/make_card.py` redraws the card from
`bench/runs/`.

```sh
python3 bench/run.py jane-doe --agent claude --model sonnet --runs 3
python3 bench/run.py jane-doe --agent pi --model google/gemini-flash-latest --runs 3
python3 bench/run.py jane-doe --agent pi --model flash-next-hybrid --runs 3   # any pi model, incl. local
```

Each run's filled forms, transcript, and grade land in `bench/runs/` (not
committed). `bench/grade.py <answers.json> <forms dir>` grades any set of
forms. Agents: `claude` (Claude Code), `pi` (any model pi can reach, hosted or
local), and `gemini` (Gemini CLI, which since June 2026 only serves paid API
keys; use `pi` for Gemini models otherwise). Scores vary between runs; compare
means over several. The example
taxpayer is [Jane Doe](examples/jane-doe), a single parent with wages, a
dependent care FSA, savings interest, student loans, and a brokerage account
with a wash sale.

## Development

```sh
cargo test --workspace     # unit tests; the PDF tests download pdfium on first run
cargo clippy --workspace --all-targets
cargo fmt --all
```

Releasing: bump `version` in `Cargo.toml`, tag `vX.Y.Z`, and push the tag; the
release workflow builds and publishes the binaries. Then attach the index the
new version will download:

```sh
nii corpus build && nii corpus pack
gh release upload vX.Y.Z target/index-release/nii-index.db.zst target/index-release/nii-index.db.zst.sha256
```

| path | what |
|---|---|
| `apps/cli` | `nii`: the CLI and MCP server |
| `crates/pdf` | form download, field listing (XFA descriptions), filling with generated appearances, rendering, flattening |
| `crates/core` | shared types, `calculate`, and the line 16 tax computation |
| `crates/corpus` | Title 26 USLM parsing (with amendment caveats) and IRS HTML guidance chunking |
| `crates/embed`, `crates/search` | local embeddings and the SQLite FTS5 + sqlite-vec hybrid index |
| `bench/` | TaxBench harness, grader, and cases |
| `examples/jane-doe` | the example taxpayer and the script that generates her documents |
| `spikes/` | early de-risking spikes ([SPIKES.md](SPIKES.md)) |
