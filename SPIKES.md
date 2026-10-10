# M0 Spike Results

Run 2026-07-06 on the primary Fedora machine (release builds). Spike code in
`spikes/src/bin/`; data under `data/` (gitignored). All four spikes **pass** —
no plan changes required, three design rules added.

## S1 — sqlite-vec brute-force latency ✅ PASS

`s1_sqlite_vec`: real Title 26 text (55.9 MB XML → 26,296 ~1000-char chunks),
512 real bge-small embeddings + random int8 padding to 200k vectors (identical
compute cost for a brute-force scan), `vec0(embedding int8[384]
distance_metric=cosine, source text, year integer)`.

| measurement | result |
|---|---|
| KNN k=100, 200k int8 vectors, unfiltered | p50 53.6ms / p95 63.6ms |
| KNN with `source='usc'` metadata filter | p50 52.6ms / p95 65.1ms (free) |
| FTS5 MATCH over 26k real chunks | p50 0.6ms |
| hybrid end-to-end (embed query + KNN + FTS + RRF) | p50 66.7ms / p95 83.3ms |
| 200k inserts (single transaction) | 1.1s |
| db size on disk | 124 MB |

Verdict: brute force is more than fast enough inside an LLM tool call.
No ANN needed at this scale; LanceDB fallback stays theoretical.

Design rules:
- int8 params must pass through `vec_int8(?)` in both INSERT and MATCH —
  raw blobs are rejected for int8 columns.
- ⚠️ embedding throughput was ~16 chunks/s (CPU, batch 32): a full 250k-chunk
  corpus ≈ 4.5h one-time. Parallelize (multiple ort sessions / rayon over
  batches) or ship a prebuilt index. Not a blocker; M1 concern.

## S2 addendum (2026-07-06, M1): GPU embedding verified on the 3090 Ti

- Built with `--features nii-embed/cuda`: ort downloads a CUDA-enabled
  onnxruntime and `copy-dylibs` drops `libonnxruntime_providers_cuda.so`
  next to the binary.
- **cuDNN 9 is a hard runtime dependency** of the CUDA EP and is not in the
  Fedora/NVIDIA repos on this box. Workaround: point `LD_LIBRARY_PATH` at a
  cuDNN wheel already in the uv cache
  (`ls -td ~/.cache/uv/archive-v0/*/nvidia/cudnn/lib | head -1`). Fragile —
  proper fix: vendor cuDNN next to the indexer or install it system-wide.
- The EP is opt-in per call (`--cuda-device N`) and configured with
  `error_on_failure` — a silent CPU fallback burned a smoke test before this.
- Throughput: full Title 26 (11,360 chunks) embedded + indexed in **6s** on
  one 3090 Ti (~1,900 chunks/s vs 16/s CPU). Sharding across both GPUs is
  unnecessary at this corpus size.

## S2 — fastembed/ort packaging ✅ PASS (Linux; macOS pending)

- `ort v2.0.0-rc.12` with default `download-binaries`: fetched a prebuilt
  onnxruntime at build time (cache: `~/.cache/ort.pyke.io/`) and **statically
  linked it** — `ldd` shows only system libs (libstdc++, ssl, z). The s1
  binary is 35 MB all-in. Very shippable.
- Model download-on-first-run works: `BGESmallENV15Q` → 65 MB on disk under
  `data/models/` (full HF snapshot, not just the 34 MB onnx file). First init
  2.4s incl. download; warm init 68ms.
- pdfium is a separate 7.3 MB shared lib loaded at runtime from an explicit
  path — bundle per-platform (github.com/bblanchon/pdfium-binaries).
- TODO: repeat build on `mba` (Apple Silicon) before betting the Tauri bundle
  story on it.

## S3 — pdfium f1040 fill round-trip ✅ PASS (with two real findings)

`s3_pdf_fill` on the live 2025 f1040 (220 KB, 2 pages, 126 text fields,
73 checkboxes, 0 other):

- **Text fields: pdfium-render just works.** `set_value` → save → reload →
  values read back exactly; rendered PNG shows values with correct appearance
  streams, including comb-field max-length truncation.
- **Finding 1 — the form is `XfaForeground`, not plain AcroForm**: static-XFA
  overlay (XFAF). AcroForm layer is real and fillable; PDF.js and pdfium both
  drive it. Ingestion should log form type and flag anything dynamic-XFA.
- **Finding 2 — checkbox on-states**: IRS checkboxes name their on-state
  `/1`, `/2`, ... in `AP /N`. `pdfium-render 0.9.2 set_checked` hardcodes
  `/V=/Yes /AS=/Yes` → renders unchecked in every viewer (caught by the pixel
  check; readback lied). And its `is_checked()` compares against `"Yes"`, so
  it misreports correctly-checked `/1` states. Worth reporting upstream.
- **Fix (production design confirmed)**: read the true on-state from
  `AP /N` (what the manifest generator records) and write `/V`+`/AS` via a
  lopdf pass. Rendered check mark verified by pixel inspection.
- lopdf note: pdfium saves strings as UTF-16BE-with-BOM; decode PDF text
  strings before comparing field names.

Fill stack going forward: pdfium-render for text fields, extraction, and
rasterization; lopdf (with the manifest's on-state names) for checkbox/radio
state; verify by rendered pixels per form-year, never by accessor readback.

## S4 — FTS5 tokenizer vs statutory citations ✅ PASS

Verdict: **plain default `unicode61` wins.** `(`/`)`/`§` are separators, so
phrase-quoted `"401(k)"` → tokens `401 k` and matches `401(k)`,
`§ 401(k)`, and `401(k)(2)(B)(i)`. Adding tokenchars (`§`, `()`)
only *hurt* recall (splits diverge between doc and query); trigram added
nothing over default.

Design rule: the search layer must **phrase-quote queries** before FTS5 —
raw `401(k)` is an FTS5 syntax error. Colloquial "401k" only matches
colloquial text; consider a small citation-synonym expansion
(401k ↔ "401(k)") at query time in M1.

## Carried into M1/M2

1. Query builder: phrase-quote FTS input; BGE query prefix on embeddings;
   `vec_int8()` wrappers.
2. Manifest generator: record checkbox on-state from `AP /N` per widget,
   per form-year; decode UTF-16BE names.
3. Fill engine: pdfium text + lopdf button-state pass; pixel-level visual
   regression per supported form-year.
4. Corpus indexing: decided — ship a prebuilt index, built on the dev box's
   2x RTX 3090 (ort `cuda` feature + `CUDAExecutionProvider`, one process per
   `device_id`). CPU rebuild path kept for from-source users.
5. Verify ort static-link + pdfium bundle on macOS (`mba`).
