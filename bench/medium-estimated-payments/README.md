# medium-estimated-payments — fictional 2025 joint return

Independent runnable variation of the medium family case.

Four $2,000 cash estimates plus a $500 prior-year refund credit = $8,500, separately from $12,000 withholding.

AGI $144,339; QBI deduction $6,748; taxable income $106,091; income tax $13,168; total tax $13,289; payments $20,500; refund **$7,211**. Income below $100,000 uses the IRS Tax Table, not direct bracket tax.

Sources: [2025 Publication 527](https://www.irs.gov/publications/p527), [2025 Form 8995 instructions](https://www.irs.gov/instructions/i8995), [2025 Schedule C instructions](https://www.irs.gov/instructions/i1040sc), [2025 Form 1040 instructions](https://www.irs.gov/instructions/i1040gi).

Run: `python3 bench/run.py medium-estimated-payments --agent pi --model provider/model --runs 1`. No model run is part of answer-key verification.
