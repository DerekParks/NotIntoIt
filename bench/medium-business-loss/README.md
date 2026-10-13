# medium-business-loss — fictional 2025 joint return

Independent runnable variation of the medium family case.

$10,000 receipts minus $13,000 ordinary expenses = $3,000 loss. Simplified home-office deduction is zero. No SE tax; $4,000 rental QBI offsets the business loss, leaving $1,000 QBI.

AGI $111,600; QBI deduction $200; taxable income $79,900; income tax $9,114; total tax $4,714; payments $16,000; refund **$11,286**. Income below $100,000 uses the IRS Tax Table, not direct bracket tax.

Sources: [2025 Publication 527](https://www.irs.gov/publications/p527), [2025 Form 8995 instructions](https://www.irs.gov/instructions/i8995), [2025 Schedule C instructions](https://www.irs.gov/instructions/i1040sc), [2025 Form 1040 instructions](https://www.irs.gov/instructions/i1040gi).

Run: `python3 bench/run.py medium-business-loss --agent pi --model provider/model --runs 1`. No model run is part of answer-key verification.
