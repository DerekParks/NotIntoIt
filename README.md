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
irs.gov PDFs, shows you the result, computes the form-line arithmetic, and grounds its
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

### Guided tax interview

Start the bundled `tax-wizard` MCP prompt to work through household details,
income, deductions, credits and payments in short rounds of questions. It reads
existing documents first, asks for missing facts, and opens side-business and
rental branches only when relevant. It finishes with a factual summary, missing
items and preparation/review of the required forms. The optional `tax_year`
argument selects the year; otherwise the interview asks for it.

With the server registered as `notintoit`, Claude Code lists
`/notintoit:tax-wizard`; its explicit MCP-command spelling also works:

```text
/mcp__notintoit__tax-wizard 2025
```

Omit `2025` to choose the year in conversation. Rebuild/install the updated `nii`
binary and reconnect the MCP server before trying it. Other clients expose
`tax-wizard` through their MCP prompt picker if they support prompts; slash-command
names are client-specific. See [Claude Code's MCP prompt documentation](https://code.claude.com/docs/en/mcp#use-mcp-prompts-as-commands).

The workflow is embedded in the binary from
[apps/cli/prompts/tax-wizard.md](apps/cli/prompts/tax-wizard.md); no separate skill
installation is needed. It is an original interview using the
[IRS intake checklist](https://www.irs.gov/pub/irs-pdf/f13614c.pdf) and
[document checklist](https://www.irs.gov/filing/gather-your-documents) as coverage
references, without copying a commercial tax wizard. Verify the applicable tax
year's rules; the current IRS checklist can describe a different filing year.
The assistant keeps progress in the conversation, and invoking it again can
resume that context. It does not persist intake notes or enforce a deterministic
decision tree. Existing federal-only scope, computation limits and human review
requirements still apply.

### Tools

| tool | what it does |
|---|---|
| `list_documents` / `read_document` | Lists and reads files in the tax workspace: PDF text page by page (scans are flagged for `view_pdf`), notes, CSVs. Nothing outside the workspace is readable. |
| `fetch_irs_form` | Downloads a form for a tax year from irs.gov into `~/Taxes/<year>/forms/`. Takes everyday names ("Schedule D", "8812", "W-2") as well as IRS file stems. Continuous-use forms (1099s) resolve to the revision in effect. Keeps an existing working copy unless told to overwrite. |
| `get_form_instructions` | Resolves shared or embedded instructions for known forms and tries the conventional filename otherwise. Requires `tax_year`, returns the source URL, local PDF and page text; optional `page` or `search` navigates the booklet. Only exact-year archives are fetched; unavailable continuous-use revisions require manual lookup. PDFs live in `<year>/instructions/`, separate from return forms. |
| `list_form_fields` | Lists a form's fields with a plain-English description of each (from the IRS form's own accessibility text), type, page, length limit, and current value. Long forms list a page at a time so the listing fits in a client's tool-output limit. |
| `fill_form` | Fills fields by name, or any unambiguous abbreviation of one (a suffix, a path missing a subform, indexes left off). Writes every valid value, rejects the rest one by one with the reason (unknown or ambiguous names with "did you mean", over-length values, wrong types), and echoes each written value next to its line's description. Can be called repeatedly. |
| `view_pdf` | Renders a page with its filled values as an image, lists those values as text, and can open the PDF in your viewer. |
| `create_review_report` | Reruns validation against current PDFs/W-2s, records file hashes, and saves JSON/readable reports in `<year>/review/`. Lists unresolved findings with IRS instruction links and pages actually delivered in this MCP session. Sourced disputes remain pending manual review and never waive findings; both `blocked` and `needs_review` are tool errors with structured reports. |
| `validate_return` | Read-only deterministic checks of proposed 2025 calendar-year PDFs: identity/status fields, dollar arithmetic, schedule transfers, and each SE owner's wages against supplied W-2s. Returns structured errors/review items and exact field names for repairs; blocking findings are MCP tool errors. No answer key or model call. |
| `calculate` | Arithmetic for form lines with explicit rounding, so the model never adds in its head. |
| `compute_tax` | Form 1040 line 16 for 2024 and 2025 (Tax Table, Tax Computation Worksheet, or Qualified Dividends and Capital Gain Tax Worksheet), every line shown. Reproduces all 2,060 rows of each year's official Tax Table. |
| `search_tax_law` | Hybrid keyword + semantic search over Title 26 of the U.S. Code and the IRS instructions for Form 1040, Schedule D, Forms 8949 and 2441, Schedule 8812, Schedules C/E/SE, Form 8995, and Publications 17, 501, 503, 550, 334, 527, 925. Pass `tax_year` to flag mismatched or unknown guidance editions; each IRS hit reports its edition and source-download provenance. Statute hits carry a caveat when a recent amendment may not apply to every tax year. |

Every tool stays inside the tax workspace: documents are read from it, forms and instructions are fetched into it, and PDF tools refuse paths outside it.

For example, call `get_form_instructions` with
`{"form":"Schedule SE","tax_year":2025,"search":"line 8a"}`.
The CLI equivalent is `nii form instructions "Schedule SE" --year 2025 --search "line 8a"`.
The [audited mapping catalog](apps/cli/instruction-mappings.tsv) contains 104
explicit form mappings. It covers separate instructions, shared booklets
(1040 and numbered schedules, W-2/W-3, 1099-MISC/NEC, 1099-INT/OID,
1099-R/5498, 1098-E/T, 1099-SA/5498-SA, 1099-A/C, W-2G/5754), and embedded
instructions (including 4868, 8332, 8880, W-4 and W-9). The audit checked 108
unique document stems and verified 169 PDFs; source URLs and title excerpts are
recorded in [the audit fixture](apps/cli/tests/fixtures/irs-instruction-audit.json).
This covers common individual/family return forms, not every IRS product.

Form 1040-SR uses IRS stem `f1040s`; `f1040sr` means Schedule R. Both everyday
names and explicit stems preserve that distinction. Schedule EIC resolves to
its embedded child-information directions; the tool also points to Form 1040
instructions for credit eligibility/calculation. Unknown forms use an explicitly
labelled candidate filename.

A correct mapping does not guarantee an exact-year archive exists. For example,
1099-INT/OID instructions currently have a January 2024 revision and W-9 has a
March 2024 revision; requesting 2025 does not silently substitute those PDFs.
Some 2025 revisions, including 4506-T, are available at the current IRS URL but
were absent at the exact-year archive URL during this audit. Such cases remain
explicit lookup errors. Verify applicability before using another revision.

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

### Deterministic return checks

The assistant must call `validate_return` before declaring completion, correct
blocking findings, and rerun it after edits. The tax wizard includes the same
requirement. This adds an independent check of what actually landed in the PDFs;
it does not rely on the model's summary or the TaxBench answer key.

CLI (paths are inside the chosen tax workspace):

```sh
nii --workspace ~/Taxes validate --year 2025 --w2 2025/documents/w2-alex.txt --w2-complete
```

`--forms-dir` can select another workspace-relative directory. Keep only proposed
return PDFs in that directory; blank reference downloads are skipped. Canonical
IRS stems are recognized, including suffixed copies such as `f1040sc-design.pdf`
and `f1040sse-spouse.pdf` for multiple businesses/spouses. Summary schedules must
be unique; multiple Schedule E copies require manual reconciliation in this version.
The validator never changes a PDF; IRS XFA descriptions are normalized in memory for reading. CLI output is JSON and the exit status is
nonzero for `blocked` or `needs_review`; MCP also returns `structuredContent`.

Checks cover missing/duplicate identifiers, exactly one filing status, mutually
exclusive dependent credit boxes, required nonzero intermediate lines, integer-cent
arithmetic on Form 1040 and Schedules C/SE/1/2/E, basic Form 8995 and 8812 arithmetic,
required supporting schedules and transfers. Refund allocation includes a declared
line-38 penalty; the penalty itself is not recomputed. Monetary comparisons allow
$1 for line rounding; blank nonzero totals never pass as zero. Identity values are
not echoed in findings, and SSN/ITIN issuance or credit eligibility is not certified.

Supply the applicable W-2 source paths, and set `w2_complete` only after confirming
that all W-2s for the return are present. Sources are read locally: supported formats
are single-employer fillable IRS PDFs with explicit calendar-year fields, or labelled
`.txt`/`.md` transcriptions with tax year, employee SSN and Box 1/2/3 amounts (Box 7
when nonzero). Unknown/scanned/flattened sources, conflicting copies, special
statutory-employee treatment, and an unconfirmed inventory require review; missing
sources never establish zero wages. PDF duplicate copies of the same W-2 within a
file are counted once; separate source files must be selected correctly.

Statuses mean:

- `blocked`: specific fields or transfers fail; repair and rerun.
- `needs_review`: supported checks cannot resolve a source, layout or scope item.
- `checks_passed`: the listed bounded checks passed, **not** a filing-readiness certificate.

Only 2025 calendar-year rule layouts are implemented. Eligibility, elections,
carryovers, tax-rate worksheets, other form lines and unsupported forms still
require review. Clients and models must honor the findings; the server cannot
prevent an assistant from ignoring them or making an unsupported claim in chat.
Rules reference the [2025 IRS Form 1040](https://www.irs.gov/pub/irs-prior/f1040--2025.pdf),
[Schedule SE instructions](https://www.irs.gov/instructions/i1040sse),
[Schedule C instructions](https://www.irs.gov/instructions/i1040sc), and
[Form 8995 instructions](https://www.irs.gov/instructions/i8995).

The validator independently blocked both saved medium attempts (six and nine
errors). Corrected copies of the eight proposed forms passed 113 checks; original PDFs were unchanged.
These are validator tests, not new model benchmark scores.

### Home-office checks and deduction interview

The validator checks the simplified home-office income limit when Schedule C's
square-footage entries identify that method: absent reported asset activity, a
deduction above positive tentative profit is a blocking
`home_office_income_limit` finding. The shared deduction box alone does not identify
the method. Actual/unknown methods and reported asset activity produce
`home_office_limit_review`, because qualifying home-use gains can affect the
worksheet limit. This is a bounded check of reported forms, not a determination
that all relevant gains, locations or expenses were disclosed. Source:
[2025 Schedule C Simplified Method Worksheet](https://www.irs.gov/instructions/i1040sc).

The `tax-wizard` deduction interview requires a source-based standard-versus-itemized
comparison: eligibility, age/blindness/dependent adjustments, allowable Schedule A
categories after limits, missing records, both totals and the selected treatment.
It handles unavailable standard deductions and deliberate itemization elections;
the separate Schedule 1-A senior deduction is not added to the standard deduction.
This interview guides the assistant; it does not make Schedule A eligibility or
method selection a deterministic validator check. Source:
[2025 Schedule A instructions](https://www.irs.gov/instructions/i1040sca).

### Final review report and sourced disputes

After the last edit, call `create_review_report` with the same `tax_year`,
`forms_dir`, `w2_documents`, and `w2_complete` used for validation. The tool saves
`<year>/review/return-review.json` and `return-review.md`, reruns validation, and
captures SHA-256 hashes of the proposed PDFs and supplied W-2s. An edit after
that snapshot requires a new report. Blank/reference and unsupported PDFs remain
visible in the inventory; the report identifies which files the validator covered.

Use `status` and `allowed_final_statement` in the assistant's final response.
`blocked` and `needs_review` do not support completion claims. Even
`checks_passed` certifies only the listed deterministic checks, not filing readiness
or whether every form is necessary. Models can still ignore instructions in chat;
the tool's recorded status and artifacts remain independent of their claims.

Before disputing a finding or recommending reversing a correction, call
`get_form_instructions` for the filing year and relevant page/section. Optional
`disputes` entries identify the current finding with `code`, `form`, `field`,
`rationale`, `source_pdf`, `page`, and an exact `quote` of 10–2000 characters.
The server checks that this session actually delivered the cited page/quote and
that the instruction PDF is unchanged. Wrong-year sources, unseen pages, invented
quotes and changed files are marked `source_required`. A verified access record
becomes `pending_manual_review`, never a rule waiver: relevance and legal
interpretation are not automatically adjudicated. Existing findings remain intact.

IRS links attached to findings are lookup directions, not assertions that the
PDF exists or has been read. Source access records show delivered text, not human
review or applicability. Records live only in the current MCP session; restarting
requires rereading sources before recording a dispute. All report destinations
and cited local PDFs must remain inside the tax workspace.

CLI equivalent (no MCP-session source access or dispute records):

```sh
nii --workspace ~/Taxes review --year 2025 --w2 2025/documents/w2-alex.txt --w2-complete
```

The CLI saves both artifacts and exits nonzero for `blocked` or `needs_review`.
TaxBench preserves generated review reports and instruction PDFs with each run.

Medium runs also save `review-grade.json`, separate from field accuracy. The
grader reruns validation with the case's declared W-2 inventory, compares the
complete validation result and report status, checks PDF/source inventories and
hashes, and rejects waived disputes. An honest blocked report can pass integrity;
that does not mean the return passes validation. Missing reports fail, while cases
without `review_checks` are explicitly `not_configured`. These checks do not grade
free-form advice, source relevance, receipt authenticity, or the Markdown's prose.

## TaxBench

Four additional 2025 family cases extend `medium`: `medium-rental-loss`
(active-participation loss), `medium-mixed-use` (vacation-home allocation),
`medium-business-loss` (negative Schedule C and QBI netting), and
`medium-estimated-payments` (cash estimates plus a prior-year refund credit).
Each has its own complete input packet, answer key, source notes and run command.
Their expected refunds are $9,507, $1,655, $11,286 and $7,211, respectively.
Rental losses and personal-use days explicitly require validator review; correct
field values do not certify eligibility or allocation.

Validator regression tests mutate one field at a time in a clean family business
return: owner IDs, missing identity, filing status, wages/withholding, schedule
transfers, arithmetic, QBI rows, refund allocation and malformed amounts. Controls
exercise formatted identifiers/currency, explicit zeros and reordered QBI rows.
These tests use IRS field metadata rather than benchmark answer-key values.

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

The new [medium case](bench/medium/README.md) covers Alex and Morgan Rivera's
joint 2025 return: two children, wages, a freelance business with mileage, meals
and a simplified home office, and a depreciating residential rental. It grades
124 fields across eight forms and six reconciliations; the expected refund is
$2,711. Two preliminary OpenRouter DeepSeek V4 Pro runs scored 119/124 and 116/124; both got the refund right, and neither passed the complete-return checks. Reported total cost: $0.43. A third run with the validator scored 124/124 and repaired all blocking findings ($0.66; 418 seconds). It still populated an unnecessary Form 4562 and gave misleading final guidance, so `return_complete` remained false. It did not use the new instructions tool.

The [medium benchmark model card](bench/MEDIUM_MODEL_CARD.md) records seven
Sonnet 5.5 attempts across five cases, costs, grading definitions, the observed
business-loss failure and its validator mitigation. It also summarizes the earlier
DeepSeek/LTJ runs and documents what remains unevaluated.

```sh
python3 bench/run.py medium --agent claude --model sonnet --runs 3
python3 bench/report.py medium --markdown
python3 bench/medium/verify.py
```

Reports distinguish refund accuracy, line score, and `return_complete` (all
keyed checks and declared reconciliations pass; required forms are populated
and readable; no listed unnecessary form is populated). This checks benchmark
coverage, not every possible line or filing readiness. Blank reference downloads
are allowed; corrupt forms are reported as grading failures.

The search index now records each IRS guidance edition and download time
(unknown for legacy caches), and flags a mismatch when `search_tax_law` receives
`tax_year`. IRS HTML links still roll forward; this does not reconstruct historical
law. Index format 3 invalidates older indexes so startup rebuilds them with the
expanded corpus. To refresh cached source documents explicitly, run
`nii corpus build --force-download`. Release publishers must regenerate the
prebuilt index with this version before uploading it.

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
| `examples/medium` | the fictional family, freelance-business and rental source packet |
| `examples/jane-doe` | the example taxpayer and the script that generates her documents |
| `spikes/` | early de-risking spikes ([SPIKES.md](SPIKES.md)) |
