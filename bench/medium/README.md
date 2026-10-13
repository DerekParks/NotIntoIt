# Medium: Rivera family, 2025

An original, fictional individual federal return for a married couple with two
children, wages, a sole-proprietor design business, and one residential rental.
Eight required PDFs, 124 graded fields, and six cross-form reconciliations.
Expected total tax: **$13,289**; payments: **$16,000**; refund: **$2,711**.
This is a family Form 1040 case, not an entity return.

## Practice-test research and provenance

The [AICPA official sample-test page](https://www.aicpa-cima.com/resources/video/get-familiar-with-the-cpa-exam-by-practicing-with-our-sample-tests)
links to the [interactive CPA sample test](https://exams.aicpa.org/Driver.Web.V13/).
It includes REG and TCP, but states that the sample question content is current
as of July 1, 2023. The interactive driver does not expose its questions as
readable HTML to the research tool. This case is independently authored in the
style of a multi-exhibit tax simulation; it does not reproduce an AICPA question
or assert CPA review. The answer key follows the IRS's 2025 rules instead.
Sources checked October 12, 2026:

- [2025 Schedule C instructions](https://www.irs.gov/instructions/i1040sc):
  business income/expenses, 70-cent mileage, 50% meals, simplified home office.
- [2025 Schedule SE instructions](https://www.irs.gov/instructions/i1040sse):
  92.35% net earnings, individual Social Security ceiling, Medicare and half-SE deduction.
- [2025 Schedule E instructions](https://www.irs.gov/instructions/i1040se) and
  [Publication 527](https://www.irs.gov/publications/p527): rent, refundable deposits,
  principal, expenses, and residential depreciation.
- [2025 Form 8995 instructions](https://www.irs.gov/instructions/i8995):
  QBI net of attributable half-SE deduction, rental trade/business income and income limit.
- [2025 Publication 1040](https://www.irs.gov/publications/p1040) and
  [2025 Schedule 8812 instructions](https://www.irs.gov/instructions/i1040s8):
  tax computation and child credit.

The rental's section 162 trade/business status is an explicit exercise assumption;
this case does not require a rental safe-harbor election or its attachment.
Passive-loss limitations, first-year depreciation, entity K-1s, state tax and
underpayment penalties are outside this medium case. A profitable passive rental
with no suspended losses does not need Form 8582. Existing building depreciation
without new assets or listed property does not require Form 4562 here.

## Answer worksheet

| Step | Amount |
|---|---:|
| Business receipts: 1099-NEC plus other receipts | 46,000 |
| Business expenses including 5,000 × $0.70 mileage and $1,000 deductible meals | 13,000 |
| Simplified home office: 200 sq ft × $5 | 1,000 |
| Schedule C profit | 32,000 |
| Rental receipts | 24,000 |
| Rental operating expenses | 10,000 |
| Building depreciation: $275,000 ÷ 27.5 | 10,000 |
| Schedule E profit | 4,000 |
| SE earnings: $32,000 × 92.35% | 29,552 |
| SE tax: rounded $3,664 Social Security + $857 Medicare | 4,521 |
| Deductible half of SE tax, whole-dollar rounding | 2,261 |
| Total income: wages $110,000 + interest $600 + profits $36,000 | 146,600 |
| AGI | 144,339 |
| Standard deduction | 31,500 |
| QBI: $32,000 − $2,261 + $4,000 | 33,739 |
| QBI deduction, rounded 20% (below income limit) | 6,748 |
| Taxable income | 106,091 |
| Income tax: $106,091 × 22% − $10,172, rounded | 13,168 |
| Child tax credit: two × $2,200 | 4,400 |
| Total tax: $13,168 − $4,400 + $4,521 | 13,289 |
| W-2 withholding + four $1,000 estimated payments | 16,000 |
| Refund | 2,711 |

Morgan's Schedule SE must not use Alex's W-2 Social Security wages. Ledger
personal expenses, owner draws, mortgage principal, and a refundable security
deposit must be excluded appropriately. Land is not depreciated; the prior
mid-month depreciation record establishes a full continuation year. The
1099-NEC repeats part of the receipts ledger. The January 2026 estimated payment
belongs to tax year 2025. The withholding alone exceeds prior-year total tax,
so no Form 2210 is necessary.

## First model runs

Two independent OpenRouter DeepSeek V4 Pro attempts on October 13, 2026 (UTC),
using pi and the rebuilt index (15,987 chunks, Title 26 release point 119-100,
all sixteen IRS guidance documents labeled 2025). Both used the original case
prompt; the retry received no answer-key hints or corrections from the first run.

| Attempt | Fields | Refund | Complete graded return | Model time | Reported cost |
|---|---:|---:|---|---:|---:|
| 1 | 119/124 | $2,711 (correct) | No | 508s | $0.216 |
| 2 | 116/124 | $2,711 (correct) | No | 324s | $0.216 |

Mean score: **117.5/124**; correct refunds: **2/2**; complete graded returns:
**0/2**; total reported model cost: **$0.431**. The one-time CPU index build is
excluded from model time and cost. These are two preliminary attempts, not a
broad comparison or a reliability estimate across taxpayers.

Both attempts used Alex's $115,000 W-2 Social Security wages on Morgan's
Schedule SE, even though Morgan has no wages. The incorrect remaining ceiling
was still above Morgan's SE earnings, so this mistake did not affect the refund.
Both left Schedule C gross profit blank and populated unnecessary Schedule B
and Form 4562. The first also omitted Schedule C line 3 and Schedule SE line 6.
The retry omitted the Form 1040 filing-status selection and all four SSN fields.
Both assistant summaries asserted completion/verification despite these errors.
Rendered PDFs confirmed the grading findings.

Original transcripts, filled PDFs, grades, run metadata, selected page renders,
and index provenance are retained under `bench/runs/medium/` (ignored by git).
The full reference cache is retained in `data/taxbench/` (also ignored), so reruns
can avoid the initial index rebuild:

```sh
NII_DATA_DIR="$PWD/data/taxbench" python3 bench/run.py medium --agent pi --model openrouter/deepseek/deepseek-v4-pro --runs 1
```

## Deterministic validator regression

The independent `nii validate` / MCP `validate_return` checker was subsequently
run against these original PDFs, without reading this case's answer key. It
blocked the first attempt with six errors and the retry with nine, including the
incorrect SE wage ownership/remaining wage base, blank intermediate lines and
missing identifying/status fields. Original PDF hashes were unchanged.

Corrected copies of the eight proposed forms pass 113 bounded checks. The repairs
are isolated under the first run's `validator-fixed/` directory; these are
validator regression artifacts, not new model benchmark runs. The original
populated Form 4562 requires manual review because this validator does not
implement its rule set; optional Schedule B is a warning, not an automatic claim
that it is legally unnecessary. The existing TaxBench answer-key grading remains
separate and still reports the original 119/124 and 116/124 results.

The server and wizard require validation before completion. Unsupported layouts,
years, source documents and special cases remain explicit review items rather
than silently receiving a pass. See the root README for scope and source formats.

## Run with validator and audited instruction mappings

A third DeepSeek V4 Pro/OpenRouter attempt on October 13, 2026 scored **124/124**
with the correct **$2,711 refund**, up from 119/124 and 116/124. It took 417.5
seconds, made 70 tool calls and reported **$0.6581** in model usage charges.
Artifacts are in `bench/runs/medium/20261013-103829-pi-openrouter_deepseek_deepseek-v4-pro-0/`.

The model called `validate_return` four times. The first invocation detected
populated fiscal-year fields and declined calendar-year arithmetic. Once those
were cleared, validation reported eight blocking errors, then three after initial
repairs, then zero. Findings included Alex's wages incorrectly attributed to
Morgan on Schedule SE, missing intermediate totals, and amounts placed on the
wrong Schedule 8812 lines. A separate read-only CLI validation of the final saved
PDFs confirmed zero blocking errors across 113 checks, with one review item for
unsupported Form 4562. The run's binary and mapping hashes are saved alongside
its independent validation report.

`return_complete` remained false: the model populated Form 4562 even though the
case does not require it. It also claimed all automated checks passed while the
validator still reported `needs_review`, and its final narrative incorrectly
suggested putting Alex's wages back onto Morgan's Schedule SE. The PDFs retained
the corrected owner-specific wages. The IRS Schedule SE instructions specify
individual spouse schedules; the Social Security wage ceiling applies to the
individual with self-employment income, not the couple's combined wages. This shows numerical repair succeeded while completion
reporting and explanatory accuracy still need improvement.

The model did not call `get_form_instructions`; this run tests the latest tool
set and shows validator-assisted recovery, but does not demonstrate use of the
new instruction mapping. It is one run, not evidence of consistent reliability.

## Run and validate

```sh
cargo build --release -p nii
python3 bench/run.py medium --agent claude --model sonnet --runs 3
python3 bench/run.py medium --agent pi --model provider/model --runs 3
python3 bench/report.py medium --markdown
python3 bench/medium/verify.py
python3 -m unittest discover -s bench -p 'test_*.py'
```

`verify.py` uses Python Decimal and the published Tax Computation Worksheet,
independently of nii's calculator, to check the numerical answer key.
For selector/fill/grader validation, fetch the eight 2025 forms into a temporary
workspace, then run:

```sh
python3 bench/validate.py bench/medium/answers.json /tmp/workspace/2025/forms --nii target/release/nii
```

This round-trip fills temporary copies from the answer key and checks 124/124;
it measures benchmark plumbing, not model performance. The model runs recorded above are separate from this answer-key validation. Whole-dollar fields allow $1 rounding tolerance;
identity fields and filing-status checks are exact. `=description` selectors
avoid overlapping checkbox and amount descriptions; `pN|` can restrict a page.

Only `intake.md` and `documents/` are copied to the agent workspace. The answer
key, verification script and this explanation stay outside it. Source documents
are transparent text/CSV transcriptions supported by `read_document`; this case
isolates tax reasoning rather than PDF/OCR extraction performance. Every person,
address, company and identifier is fictional. Area-000 SSNs are placeholders,
with eligibility stipulated for the exercise.

`return_complete` means all keyed checks pass, all required PDFs are readable and
populated, the outcome is valid, declared reconciliations hold, and no listed
unnecessary form is populated. It does not guarantee ungraded lines are correct
or certify a return as filing-ready. A downloaded blank form is not treated as
filed; a corrupt PDF fails the run's grade without discarding its transcript.
