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
