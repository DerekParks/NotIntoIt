# Medium personal-tax benchmark card

Updated October 14, 2026. Small exploratory evaluation of MCP-assisted preparation
of fictional **2025 federal joint returns**. Five related cases share one family:
wages, two children, a sole proprietorship and a residential rental, with variations
for losses, personal use and estimated payments. This is not a CPA examination or
an estimate of performance on the general taxpayer population.

## Sonnet results

Exact model observed in all seven attempts: **`claude-sonnet-5-5`**, using Claude
Code with only NotIntoIt's MCP tools available. Each attempt used a fresh workspace.

| Case | Attempts | Field score(s) | Refund within $1 | Report integrity | Reported cost, total |
|---|---:|---|---:|---|---:|
| Original medium | 1 | 124/124 | 1/1 | 9/9 | $0.48 |
| Rental loss | 1 | 126/126 | 1/1 | 9/9 | $0.47 |
| Mixed personal/rental use | 2 | 124/124, 124/124 | 2/2 | 9/9 each | $0.92 |
| Business loss | 2 | 110/110*, **80/110** | **1/2** | 9/9 each | $0.71 |
| Estimated payments | 1 | 124/124 | 1/1 | 9/9 | $0.45 |

**6/7 refunds within tolerance; total reported cost $3.03.** The four October 13
attempts cost $1.76; the three October 14 attempts cost $1.27. This sample does not
establish reliability or that Sonnet is the cheapest model. Costs are client-reported
API estimates, not invoice reconciliation.

*The first business-loss attempt originally scored 107/110. Its key was corrected
to accept blank or zero on three unused SE/adjustment lines, giving 110/110.
Original PDFs, transcripts and grades were preserved. The second attempt's 80/110
is a substantive failure, not a layout or rounding difference.

## What the scores mean

- **Field accuracy:** identity, amounts, form presence, table-layout equivalents
  and declared cross-form reconciliations. Numeric tolerance is $1; cents and
  whole-dollar presentations can both pass. Cases have different denominators.
- **Refund accuracy:** independent outcome measure against the case's expected
  refund. A correct refund alone does not establish a complete return.
- **Report integrity:** nine deterministic checks of JSON report presence, year,
  readable artifact presence, independent validation agreement, status, file
  inventory/hashes, instruction receipt hashes/year, and unresolved dispute status.
  It does not grade the Markdown prose, advice, citation relevance or receipt
  authenticity. A faithfully reported wrong return can pass integrity.
- **Manual review:** rental-loss limits and mixed-use allocations deliberately
  retain `needs_review`. That is expected behavior, not a failed arithmetic check.

## Observed failure and current mitigation

On the second business-loss attempt, Sonnet deducted a $1,000 simplified home
office expense despite a $3,000 tentative business loss. That eliminated the case's
positive net QBI and overstated the refund by **$96** ($11,382 versus $11,286).
The then-current validator reported 77 checks passed and missed the tax-rule error.

The current implementation blocks that saved return with
`home_office_income_limit`. Reported asset activity and actual/unknown home-office
methods require worksheet review rather than an unconditional tentative-profit
cap. Regression tests cover losses, zero profit, positive-profit limits and gain
exceptions. Correct saved returns still pass the bounded checks. **No new model
attempt has been run after this fix**; replay demonstrates detection, not recovery.

The wizard now explicitly compares allowable itemized deductions with the
applicable standard deduction. This is interview guidance, not a deterministic
Schedule A decision engine. All five current cases favor the standard deduction;
itemization-winning cases and that new interview behavior remain unevaluated.

## Earlier original-case results

These used different tool versions and are **not a controlled model ranking**.
They did not cover the four variants above.

| Model / route | Attempts | Current field scores | Refund within $1 | Limitations |
|---|---:|---|---:|---|
| DeepSeek V4 Pro / pi + OpenRouter | 3 | 119/124, 116/124, 124/124 | 3/3 | Unnecessary forms; identity/SE errors in earlier attempts; misleading final advice |
| Flash Next hybrid / pi + LTJ | 1 | 124/124* | 1/1 | Unnecessary Form 4562; report retained review; explanatory errors |

*Flash originally scored 123/124. The grader now accepts its two equal,
owner-specific rental-QBI rows as an alternative to one combined row. See the
[original medium case history](medium/README.md) for preserved scores, costs,
instruction use and validator changes. Local serving's reported $0 API cost does
not include hardware or electricity.

## Reproduction and limits

The Sonnet runs used tool commits `d039726` and `9443000`, with binary SHA-256
`3edd1834bcffdbe413684431e59521621d3d3d3d1af698d10736a4798fda6965`.
The October 13 calls used the `sonnet` alias; artifacts confirm its exact resolved
model. October 14 explicitly pinned `claude-sonnet-5-5`, with $0.55 per-attempt
client-side limits and no retries/resumptions. Limits can overshoot by a final
request. The vendor model ID is not a guarantee of immutable model weights.

```sh
cargo build --release -p nii
NII_DATA_DIR="$PWD/data/taxbench" python3 bench/run.py medium-business-loss \
  --agent claude --model claude-sonnet-5-5 --runs 1 --retries 0 --resumes 0 --nudges 0
```

This example does not set a spending cap. The capped smoke-suite wrapper was
temporary, not a shipped suite-runner feature. New runs use the current tool build
and will not exactly reproduce historical validator behavior.

[Case definitions and verification](medium-variants.md) are committed. Raw run
artifacts and suite summaries are local under `bench/runs/` and ignored by Git;
they are not public downloadable evidence. The local summaries are
`sonnet-variants-summary.json` and `suite-20261014-172117-sonnet-pinned.json`.

The cases are small, related and authored alongside the tools. They do not cover
state/entity/foreign returns, diverse households, all eligibility rules or all IRS
lines. Source availability, model nondeterminism and changing tools affect results.
Passing these checks does not certify filing readiness. This is a documented
proof-of-concept pause point; no returns were signed or filed.
