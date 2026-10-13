# Additional medium cases

These fictional 2025 joint-return cases retain the original medium family's
identities, children and ordinary source-document format. Each case supplies a
complete packet rather than requiring an agent to merge overrides. Keys and
grading files remain outside the agent's workspace.

| Case | Main decision | Expected refund | Key checks | Bounded validator result |
|---|---|---:|---:|---|
| `medium-rental-loss` | Active participation, $95,339 MAGI before loss, $1,000 deductible loss, Form 8582 exception | $9,507 | 126 | 113 checks; rental-loss review |
| `medium-mixed-use` | 120 rental / 150 used days; 80% allocation; investment rental excluded from QBI | $1,655 | 124 | 113 checks; personal-use review |
| `medium-business-loss` | $3,000 Schedule C loss; no simplified home-office deduction or SE tax; QBI netting | $11,286 | 110 | 90 checks passed |
| `medium-estimated-payments` | $8,000 cash estimates plus $500 prior-year credit; withholding stays separate | $7,211 | 124 | 113 checks passed |

Every key passed round-trip grading through the IRS PDFs. Filled copies were
independently validated with the supplied W-2; none produced blocking errors.
Line 16 amounts were separately checked using `compute_tax`, including the Tax
Table for taxable income below $100,000. These are key/validator checks, not
model benchmark scores. No reliability estimate follows from these checks.

For example:

```sh
python3 bench/run.py medium-rental-loss --agent pi --model provider/model --runs 1
python3 -m unittest discover -s bench -p 'test_*.py'
cargo test -p nii validate::tests
```

Each case README explains its amounts and links to the relevant 2025 IRS
instructions. Source-packet tests independently reconcile ledger receipts,
deductible business expenses, rental allocation and payment records to the keys.
Loss limitations and personal-use allocation remain manual review items; a
correct key score does not imply the validator implements those eligibility rules.

The validator mutation test starts each trial from a clean synthetic joint return
using actual IRS field metadata. It changes one field per trial (14 text-field
mutations and one extra filing-status checkbox), requires a specific finding,
and keeps formatting, explicit-zero and reordered-QBI controls passing.

## Sonnet smoke runs — October 13, 2026

Claude Code's `sonnet` alias resolved to `claude-sonnet-5-5`. One attempt per
case used local binary built from commit `d039726`; no retries were needed.

| Case | Current field score | Actual refund | Report integrity | Report status | Seconds | Reported cost |
|---|---:|---:|---:|---|---:|---:|
| Rental loss | 126/126 | $9,506.54 | 9/9 | `needs_review` | 77.5 | $0.4681 |
| Mixed use | 124/124 | $1,654.54 | 9/9 | `needs_review` | 77.6 | $0.4747 |
| Business loss | 110/110 | $11,286.00 | 9/9 | `checks_passed` | 64.5 | $0.3689 |
| Estimated payments | 124/124 | $7,210.54 | 9/9 | `checks_passed` | 73.8 | $0.4461 |

Total reported cost: **$1.757848**, across **293.4 seconds** of model runs.
All outcomes meet the existing $1 tolerance. Three runs retained cents instead
of applying the key's whole-dollar rounding. None produced blocking validator
errors or unnecessary populated forms. Rental-loss limits and personal-use
allocation remain manual review items, correctly reflected by the reports.

The business-loss attempt originally scored **107/110** because it left three
unused zero SE/adjustment lines blank. This case now explicitly accepts blank or
zero on Form 1040 line 23 and Schedule 1 lines 15 and 26; nonzero/malformed entries
still fail. Original `meta.json`, `grade.json`, PDFs and transcripts are unchanged;
`grade-rechecked.json` records the **110/110** regrade. The allowance has a
regression test and passes the answer-key PDF round-trip.

Artifacts are under `bench/runs/<case>/20261013-<timestamp>-claude-sonnet-0/`;
`bench/runs/sonnet-variants-summary.json` records exact costs, run paths and binary
hash. These files are ignored by Git. One attempt per case is a smoke test;
free-form advice and source interpretation were not graded, and passing bounded
checks does not certify filing readiness.
