# NotIntoIt — Plan

LLM-assisted tax prep for humans who are, well, not into it. The app's job is
**context engineering**: turn the user's documents plus the relevant slice of US
tax law into a well-built prompt, let an LLM (Claude first, provider-agnostic by
design) reason about it with tool calls, and land the result in real, filled
IRS PDFs the user reviews in-app. Cowork meets Cursor meets TurboTax.

**This is not e-filing software.** Output is filled PDFs for the user to review,
print, and mail (or transcribe into an e-file provider). E-filing requires IRS
authorization and is explicitly out of scope. The human reviews everything; the
app makes review easy (citations, field highlighting, editable forms).

---

## The core loop

```
user drops W2.pdf into workspace
  → Rust: sniff type (fillable AcroForm? scanned image? which doc?)
  → LLM vision: extract to structured JSON (W2 schema) — user confirms
  → agent turn begins: context = extracted docs + workspace state
      tools available to the model:
        search_tax_law(query)        — hybrid FTS+vector over local corpus
        fetch_irs_form(form, year)   — download f1040.pdf etc. from irs.gov
        list_form_fields(form)       — field manifest (names, types, checkbox exports)
        fill_form(form, {field: value, ...})
        read_document(path)          — original or extracted JSON
        calculate(expr)              — deterministic arithmetic, never trust LLM math
  → model: "W-2 income → you need a 1040" → fetches f1040.pdf → fills it
  → PDF.js shows the filled 1040; user edits fields in-place; chat continues
```

Everything is a plain file in a per-tax-year workspace directory — the file
tree pane is a real directory. No opaque app state.

```
~/Taxes/2026/
  documents/    w2-employer.pdf, 1099-int-bank.pdf, ...   (originals, read-only)
  extracted/    w2-employer.json, ...                     (structured, user-confirmed)
  forms/        f1040.pdf, f1040s1.pdf, ...               (filled output)
  workspace.json                                          (session state, chat log)
```

## Architecture

Cargo workspace. The Rust backend is where all the interesting work lives
(good news for the learn-Rust goal); the UI is a thin webview.

```
crates/
  corpus/    download + parse + chunk tax law sources     (pure Rust, CLI-first)
  search/    SQLite index: FTS5 + sqlite-vec + RRF hybrid (pure Rust)
  embed/     fastembed wrapper, model download-on-first-run
  pdf/       pdfium-render: fill, extract, rasterize; form-field manifests
  llm/       provider trait + Anthropic impl; agent loop; tool dispatch
  core/      domain types: workspace, documents, extraction schemas
apps/
  cli/       `nii` — corpus build, search, fill, extract; every feature usable headless
  desktop/   Tauri 2 app
```

Rule: **the CLI can do everything the GUI can.** The GUI is a client of the same
crates. This keeps the core testable and makes milestone 1–2 pure-Rust work —
the right place to learn the language before Tauri IPC enters the picture.

### App shell: Tauri 2

- Webview UI is the pragmatic choice: PDF.js (viewer + in-place form editing),
  chat with markdown/streaming, file tree — all solved problems in HTML;
  all painful in egui/iced.
- Backend is 100% Rust; frontend talks to it over Tauri commands/events.
- Frontend framework: **Svelte 5** (small, no build-time ceremony; React fine
  too if preferred — nothing in the plan depends on the choice).
- Three panes: **file tree | chat | PDF viewer** (tabbed per form/document).

### LLM layer

- `Provider` trait (messages in, streamed events out, tool-use support);
  Anthropic implementation first. The agent loop, tool schemas, and context
  builder are provider-neutral.
- Vision extraction replaces OCR as the primary path: send the document
  (PDF/image) to Claude with a per-doc-type JSON schema (W-2, 1099-INT/DIV/NEC,
  1098, ...) and force structured output. More accurate than tesseract on real
  scans, and it classifies at the same time. Optional later: local
  tesseract/vision-model pre-pass as a privacy mode.
- Context builder assembles: system prompt (role, guardrails, current tax year),
  workspace summary, user-confirmed extractions, retrieved law chunks with
  citations. Retrieval results always carry source + citation (e.g.
  `IRC §61(a)`, `Pub 17 ch. 5`) so the model can cite and the UI can link.
- `calculate` tool: all arithmetic done in Rust. The model proposes formulas,
  never sums numbers itself. Line-level math errors are the #1 credibility
  risk.

### Corpus: what we index (ranked by value)

All public domain, all bulk-downloadable (verified 2026-07-06):

1. **IRS Publications + Form Instructions — HTML** from irs.gov
   (`irs.gov/publications/p17`, `irs.gov/instructions/i1040gi`; enumerate via
   `sitemap.xml`). *The highest-value RAG content*: plain-language,
   taxpayer-facing, organized around real filing questions. Start set:
   Pub 17, 501, 502, 503, 505, 523, 525, 526, 550, 590-A/B, 596, 969 + the
   i1040 family.
2. **Title 26 USC (the actual tax code)** — one 56 MB USLM XML file from
   `uscode.house.gov/download/releasepoints/us/pl/{congress}/{law}/xml_usc26@....zip`.
   Semantic XML with stable section IDs (`/us/usc/t26/s61`) → deterministic
   section/subsection chunking. Authoritative grounding + citations.
3. **Treasury Regulations (26 CFR)** — eCFR API, updated daily:
   `ecfr.gov/api/versioner/v1/full/{date}/title-26.xml` (~88 MB; bigger than
   the statute). Reg numbering mirrors the code (§1.61-1 ↔ IRC §61) →
   cross-link in metadata.
4. **IRS form PDFs** — metadata only (form ↔ title ↔ year ↔ URL), not body
   text. Current: `irs.gov/pub/irs-pdf/f1040.pdf`; prior years:
   `irs.gov/pub/irs-prior/f1040--2022.pdf` (double hyphen).
5. **IRBs/Revenue Procedures** — selective only: the annual inflation-adjustment
   Rev. Proc. (brackets, standard deduction) and anything cited from the pubs.
   Skip bulk ingestion.

Do **not** scrape Cornell LII (CC BY-NC-SA on their markup) — everything comes
from official sources anyway.

Chunking is the biggest retrieval-quality lever, bigger than engine choice:
~500-token chunks with the full section path prepended, e.g.
`"IRC §401(k)(2) — Cash or deferred arrangements: ..."`. Each chunk carries
metadata: source, citation, section path, tax year applicability, URL.

### Search: SQLite, one file, hybrid

- **Store:** SQLite via `rusqlite` (bundled) — FTS5 for BM25 + `sqlite-vec`
  (`vec0`, int8-quantized vectors, metadata columns). One user-visible `.db`
  file holding text, vectors, and metadata; trivially rebuildable from source.
- **Scale check:** corpus ≈ 50–250k chunks at 384 dims. Brute-force sqlite-vec
  scan is expected to be fine at this size (spike S1 verifies on real data);
  ANN is an optimization we can defer.
- **Embeddings:** `fastembed` with **bge-small-en-v1.5 quantized** (384-dim,
  ~34 MB, downloads on first run, offline thereafter). Queries get the BGE
  query prefix; documents don't.
- **Hybrid:** hand-rolled RRF (~20 lines): top-100 FTS + top-100 vector,
  score = Σ 1/(60 + rank). Optional later: fastembed reranker
  (jina-reranker-v1-turbo) over the fused top-50.
- **Tokenizer care:** statutory citations (`§401(k)(2)(B)`, form numbers) need
  FTS5 tokenizer attention — spike S4 verdict: default `unicode61` wins;
  queries must be phrase-quoted.
- **Prebuilt index, shipped as an artifact.** The corpus index is built once
  per corpus release on the dev box's GPUs (2x RTX 3090) and published as a
  versioned, zstd-compressed `.db` (checksummed; manifest records Title 26
  release point, eCFR date, IRS pubs snapshot date, embedding model + dims,
  schema version). The app downloads it on first run — end users never pay
  the embedding cost. GPU path: `ort = { features = ["cuda"] }` (feature
  unification turns on CUDA in fastembed's ort session) +
  `with_execution_providers([CUDAExecutionProvider])` with `device_id` 0/1 to
  shard across both cards. `nii corpus build` stays runnable on pure CPU for
  anyone rebuilding from source — same code, no EP.
- Fallback if outgrown: LanceDB (verified: Rust API does FTS + vector + RRF in
  one store) — accepts Arrow/DataFusion binary weight and 0.x churn.

### PDF: fill, view, round-trip

Verified good news: **current IRS forms are plain AcroForms, not XFA.**
Field names are LiveCycle-style (`topmostSubform[0].Page1[0].f1_01[0]`);
checkboxes are export-value based (`/V` and `/AS` must match exactly, e.g.
filing-status boxes are a mutually-exclusive group).

- **Fill engine:** `pdfium-render` 0.9.x — `set_value()` / `set_checked()` via
  Pdfium's form-fill environment (generates appearance streams, the historical
  failure mode of this whole domain). Same engine Chrome uses; also gives us
  text extraction and rasterization for free. Fallback: direct `lopdf`
  manipulation with `NeedAppearances`.
- **Field manifests, pinned per form-year:** the IRS renames fields between
  revisions. For each supported (form, year): a checked-in manifest mapping
  semantic names (`filing_status`, `line_1a_wages`) → PDF field names + types +
  checkbox export values. Generated by a dev tool (pdfium enumeration;
  `pdftk dump_data_fields` as cross-check), semantic mapping curated by hand
  (LLM-assisted, human-verified). The `fill_form` tool exposes *semantic*
  names to the model — it never sees raw `f1_01[0]` gibberish.
- **Correction (2026-10-10): pdfium-render does not generate appearance
  streams.** Its `set_value` writes only `/V`; S3's renders looked right
  because pdfium regenerates appearances at render time, but Preview, poppler,
  printing, and flattening showed blank text fields. Filling is now pure
  lopdf with our own `/AP` generation (font/size/color from `/DA`, `/Q`
  alignment, comb fields, multiline wrap); tests flatten before measuring
  pixels and poppler is the cross-check. pdfium remains for render/flatten.
- **Field descriptions from XFA (built 2026-10-10):** IRS AcroForms carry
  no `/TU` tooltips, but the static XFA template has per-field accessibility
  text ("Filing Status. Check only one box. Single."). Fetching a form
  normalizes the working copy: XFA SOM paths are mapped to AcroForm names,
  the text is written into `/TU`, and `/XFA` is dropped — otherwise Acrobat
  renders the (empty) XFA data instead of our AcroForm values. 95%+ of f1040
  fields get a description, enough for a model to fill forms today; curated
  semantic manifests remain the path to stable names across form-years.
- **Viewer:** PDF.js in the webview with forms + storage enabled: renders
  filled values, lets the user edit fields in-place, and `saveDocument()`
  round-trips edits back to Rust as PDF bytes.
- **Print path:** flatten via pdfium rasterization/appearance generation before
  final print output (NeedAppearances isn't honored by all print pipelines).
- **Visual regression tests per supported form:** fill known values → render
  with pdfium → compare pixels. Non-negotiable; blank-field bugs are silent.

### Privacy & trust posture

- Everything is local **except** LLM API calls — which necessarily carry tax
  data. Say so plainly in the UI on first run. Provider-agnostic design means a
  local model (Ollama/llama.cpp provider) is a real future option.
- Extractions are user-confirmed before use; filled forms are user-reviewed;
  every model claim about law should carry a citation the user can click
  (retrieval chunks carry source URLs).
- Prominent "this is not tax advice; review before filing" framing.

---

## Milestones

**M0 — Spikes (de-risk before building)**
- S1: sqlite-vec brute-force latency on ~200k real chunks, int8, with metadata
  filter, on a modest laptop.
- S2: ort/ONNX Runtime packaging per OS (bundled dylib vs static vs
  download-binaries) — the fiddliest shipping risk.
- S3: pdfium-render fill → save → open in PDF.js and Acrobat: do values
  render? Checkboxes? (f1040, real fields.)
- S4: FTS5 tokenizer on `§401(k)`-style citations.

**M1 — Corpus + search CLI** *(pure Rust; ideal learning ground)*
`nii corpus build` downloads Title 26 XML, eCFR XML, IRS pubs HTML; parses,
chunks, embeds (CUDA EP when GPUs available, CPU otherwise), indexes.
`nii index publish` / `nii index fetch` move the versioned prebuilt artifact.
`nii search "can I deduct home office"` returns cited chunks via hybrid RRF.
Success: eyeball-good results on 20 canned questions. The full GPU index
build waits on the 3090s freeing up; everything else proceeds on CPU with a
partial corpus in the meantime.

**M2 — PDF crate + manifests**
Fill/readback round-trip on f1040 + Schedules 1–3 + W-2. Manifest generator
dev-tool. Visual regression harness. `nii fill f1040 --data answers.json`.

**Lessons from the first full return (Jane Doe, 2026-10-10)**
- Statute text is *current* law. Title 26 chunks now carry an amendment
  caveat (law + "applies to taxable years beginning after …", parsed from
  the USLM effective-date notes), and post-2017 amendment notes are indexed
  as `§ N note` chunks. Without this, §129 read "$7,500" for 2025.
- Annual figures come from the year's IRS instructions/publications (HTML,
  post-legislation), not Rev. Procs: Rev. Proc. 2024-40 predates OBBBA (its
  2025 HOH standard deduction is superseded) and its tables scramble under
  PDF text extraction.
- Line 16 tax is deterministic: `compute_tax` holds checked-in brackets and
  capital-gain thresholds per year (from the Rev. Procs) and is tested
  against official Tax Table rows. Adding a year = one table + fixtures.
- Field listings are line-formatted, prefix-stripped, and deduplicated
  (8949 page 1: 24.7k -> 14.5k chars); `fill_form` accepts unique name suffixes.

**MCP server (shipped ahead of M3, 2026-10-10)**
`nii mcp`: stdio MCP server exposing `fetch_irs_form`, `list_form_fields`,
`fill_form`, `view_pdf`, `search_tax_law`. The search index builds in the
background on startup (locked, atomic swap, retry with backoff). Lets any
MCP client (Claude Code, Claude Desktop) drive the core loop before our own
agent loop and GUI exist.

**M3 — Agent loop, headless**
Provider trait + Anthropic impl + tool dispatch. `nii chat` in the terminal:
drop a W-2 PDF path, watch it extract → search → fetch → fill a real 1040.
**This is the end-to-end proof, before any GUI exists.**

**M4 — Tauri app**
Three-pane shell: file tree (real workspace dir), streaming chat, PDF.js viewer
with editable fields + round-trip save. Extraction-confirmation UX. Citation
links from chat into the law viewer.

**M5 — Breadth & polish**
More doc types (1099 family, 1098), more forms (Schedules A/B/C/D, state
some day), filled-field provenance highlighting (which chat turn set this
value?), reranker, privacy mode / local-model provider, prior-year support.

## Risks

- **LLM correctness** — mitigations: `calculate` tool for all math, citations
  required, user confirms extractions, user reviews forms. The app's stance is
  "meticulous assistant", not "authority".
- **sqlite-vec is pre-v1** — contained: index is a rebuildable artifact; the
  schema is ours; LanceDB is a validated fallback.
- **Form-year churn** — contained by per-form-year manifests + visual
  regression tests; adding a year is a manifest task, not a code task.
- **Appearance streams** — the perennial PDF gotcha; S3 + pixel tests exist
  because of it.
- **Scope creep toward "real tax software"** — the disclaimer/no-e-file stance
  is a feature. Revisit only if this stops being a hobby.
