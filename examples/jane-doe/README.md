# Example taxpayer: Jane Doe, tax year 2025

A complete, fictional 2025 return's worth of source documents for exercising
NotIntoIt end to end. Every person, business, EIN, and amount is made up;
SSNs use area `000`, which is never issued.

Jane is a single parent in Denver with a 3-year-old, a salaried job with a
dependent care FSA and 401(k), a savings account, student loans, and a
brokerage account where she sold a few positions, one of them into a wash
sale.

## Documents

| file | what it is |
|---|---|
| `2025/intake.md` | What Jane would tell a preparer: household, benefits, life facts |
| `2025/documents/w2-bramblewood-analytics.pdf` | W-2 (Copies B, C, 2): wages, 401(k) (12 D), dependent care FSA (box 10), health coverage (12 DD), Colorado FAMLI (box 14) |
| `2025/documents/1099-int-prairie-federal-savings.pdf` | 1099-INT from a high-yield savings account |
| `2025/documents/1098-e-granite-student-loans.pdf` | 1098-E student loan interest |
| `2025/documents/loyalty-brokerage-2025-consolidated-1099.pdf` | Consolidated 1099 (DIV, INT, B) with transaction detail and a wash sale |
| `2025/documents/little-sprouts-2025-childcare-statement.pdf` | Daycare's annual payment statement with its EIN |

The W-2, 1099-INT, and 1098-E are the real irs.gov forms, filled with `nii`
and flattened; the brokerage and daycare statements are drawn the way those
businesses would send them.

## Try it

Point an MCP server at this workspace so filled forms land in
`examples/jane-doe/2025/forms/`:

```sh
claude mcp add notintoit-jane -e NII_WORKSPACE="$PWD/examples/jane-doe" -- nii mcp
```

Then: *"Read Jane's intake notes and documents in examples/jane-doe/2025 and
prepare her 2025 Form 1040 with the schedules and forms it needs."*

## Benchmark it

`python3 bench/run.py jane-doe --agent claude --model sonnet` runs a clean
agent on just `2025/intake.md` and `2025/documents/` and grades its forms
against [`bench/jane-doe/answers.json`](../../bench/jane-doe/answers.json).
The next section is effectively the answer key, which is why the harness
never copies this README into the agent's workspace.

## What a careful preparer should catch

- **Filing status is Head of Household**, not the Single she used last year.
- **Child tax credit** for Leo (under 17, SSN, lived with her all year).
- **Form 2441 is required** because of W-2 box 10. The $5,000 FSA benefit is
  excluded, and since excluded benefits exceed the $3,000 one-child expense
  limit, there is **no additional dependent care credit** despite $13,200 paid.
- **W-2 boxes 1 vs 3/5**: 401(k) deferrals reduce box 1 only; the FSA and
  pre-tax premiums reduce all three. Box 12 DD and box 14 are informational.
- **401(k)**: nothing to deduct; box 13 only matters for IRA deductions.
- **Wash sale**: the broker already disallowed the August loss (box 1g, code
  W) and rolled it into the replacement lot's basis and holding period. Form
  8949 Part I (Box A) carries the adjustment; the November sale of the
  replacement shares is an ordinary short-term loss.
- **Capital gain distributions** (1099-DIV box 2a) go on Schedule D, and
  qualified dividends plus long-term gains get the preferential-rate worksheet.
- **Foreign tax paid** ($14.62) can be taken as a credit without Form 1116
  (under the $300 threshold, passive income reported on a 1099).
- **Schedule B is not required**: total interest (two 1099-INTs) and ordinary
  dividends are each under $1,500.
- **Student loan interest** is an adjustment to income; her MAGI is below the
  phase-out.
- **Earned income credit**: her income is too high.

## Regenerate

```sh
cargo build --release -p nii
uv run --with reportlab --with pypdf python examples/jane-doe/generate.py
```

All values live at the top of `generate.py`.
