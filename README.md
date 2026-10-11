# NotIntoIt

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

**Not tax advice, and nothing is e-filed.** You get filled PDFs to review,
print, and file yourself. Tax data you share with an assistant goes to that
assistant's model provider (or stays on your machine with a local model);
everything else runs locally.

## Status

Working end to end for tax year 2025 (and 2024 tax computation). Claude Sonnet
prepares the [example taxpayer's](examples/jane-doe) complete return —
Form 1040 with Schedules 1, 3, D, 8812 and Forms 8949 and 2441 — at 40–46/46
on [TaxBench](#taxbench). Not yet: state returns, scanned-document OCR beyond
what a vision model can read, the desktop app ([PLAN.md](PLAN.md)).

## Install

Requires Rust 1.89+.

```sh
cargo install --path apps/cli --locked
```

This installs one binary, `nii`. It is self-contained apart from pdfium, which
it downloads on first use.

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
| `fetch_irs_form` | Downloads a form for a tax year from irs.gov into `~/Taxes/<year>/forms/`. Continuous-use forms (1099s) resolve to the revision in effect. Keeps an existing working copy unless told to overwrite. |
| `list_form_fields` | Lists a form's fields with a plain-English description of each (from the IRS form's own accessibility text), type, page, length limit, and current value. |
| `fill_form` | Fills fields by name (or any unique suffix of one). Validates every value first — unknown or ambiguous names (with "did you mean"), over-length values, wrong types — and writes nothing if any is invalid. Can be called repeatedly. |
| `view_pdf` | Renders a page with its filled values as an image, and can open the PDF in your viewer. |
| `calculate` | Exact arithmetic for form lines, so the model never adds in its head. |
| `compute_tax` | Form 1040 line 16 for 2024 and 2025 (Tax Table, Tax Computation Worksheet, or Qualified Dividends and Capital Gain Tax Worksheet), every line shown. Reproduces all 2,060 rows of each year's official Tax Table. |
| `search_tax_law` | Hybrid keyword + semantic search over Title 26 of the U.S. Code and the IRS instructions for Form 1040, Schedule D, Forms 8949 and 2441, Schedule 8812, and Publications 17, 501, 503, 550. Statute hits carry a caveat when a recent amendment may not apply to every tax year. |

### First run

On startup the server builds the search index in the background: it downloads
Title 26 from uscode.house.gov (8 MB zip) and the IRS instructions and
publications (about 12 MB of HTML), downloads a local embedding model (65 MB),
and embeds everything on your CPU (about 15k chunks; roughly 20 minutes on a
desktop CPU). `search_tax_law` reports progress until the index is ready; the
other tools work immediately. A failed build (e.g. upstream down) retries on
its own with backoff. Filled forms are written with their own appearance
streams, so they display the same in every viewer and survive printing.

## Where things live

| | default | override |
|---|---|---|
| tax workspace (your documents and forms) | `~/Taxes` | `--workspace` / `NII_WORKSPACE` |
| caches, model, index, pdfium | platform data dir (`~/.local/share/notintoit`, `~/Library/Application Support/notintoit`) | `--data-dir` / `NII_DATA_DIR` |

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

| path | what |
|---|---|
| `apps/cli` | `nii`: the CLI and MCP server |
| `crates/pdf` | form download, field listing (XFA descriptions), filling with generated appearances, rendering, flattening |
| `crates/core` | shared types, `calculate`, and the line 16 tax computation |
| `crates/corpus` | Title 26 USLM parsing (with amendment caveats) and IRS HTML guidance chunking |
| `crates/embed`, `crates/search` | local embeddings and the SQLite FTS5 + sqlite-vec hybrid index |
| `bench/` | TaxBench harness, grader, and cases |
| `examples/jane-doe` | the example taxpayer and the script that generates her documents |
| `spikes/` | M0 de-risking spikes ([SPIKES.md](SPIKES.md)) |

See [PLAN.md](PLAN.md) for the design and roadmap.
