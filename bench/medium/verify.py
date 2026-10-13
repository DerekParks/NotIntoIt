"""Independent Decimal worksheet for the medium answer key; no nii tax calculator.
Run: python3 bench/medium/verify.py
"""
import csv
import json
from decimal import Decimal, ROUND_HALF_UP
from pathlib import Path

HERE = Path(__file__).resolve().parent
DOCS = HERE.parents[1] / 'examples' / 'medium' / '2025' / 'documents'
D = Decimal

def dollars(value):
    return int(D(value).quantize(D('1'), rounding=ROUND_HALF_UP))

def ledger(name):
    with (DOCS / name).open() as f:
        return list(csv.DictReader(f))

def worksheet():
    business = ledger('business-ledger.csv')
    receipts = sum(D(r['amount']) for r in business if r['category'] == 'receipts')
    expenses = sum(D(r['amount']) * (D('.5') if 'meals' in r['description'] else 1)
                   for r in business if r['category'] == 'expense') + D(5000) * D('.70')
    profit = receipts - expenses - D(200) * D(5)
    rental = ledger('rental-ledger.csv')
    depreciation = D(275000) / D('27.5')
    rent_net = sum(D(r['amount']) for r in rental if r['category'] == 'income') - sum(
        D(r['amount']) for r in rental if r['category'] == 'expense') - depreciation
    earnings = profit * D('.9235')
    # Morgan, rather than Alex, owns the business and has no Social Security wages.
    social_security = dollars(min(earnings, D(176100)) * D('.124'))
    medicare = dollars(earnings * D('.029'))
    se_tax = social_security + medicare
    half_se = dollars(D(se_tax) / 2)
    total_income = D(110000) + D(600) + profit + rent_net
    agi = total_income - half_se
    qbi = profit - half_se + rent_net
    before_qbi = agi - D(31500)
    qbi_deduction = dollars(min(qbi * D('.2'), before_qbi * D('.2')))
    taxable = before_qbi - qbi_deduction
    # IRS 2025 Tax Computation Worksheet section B (100000–206700).
    assert 100000 <= taxable <= 206700
    income_tax = dollars(taxable * D('.22') - D(10172))
    child_credit = 2 * 2200
    total_tax = income_tax - child_credit + se_tax
    estimates = sum(D(r['amount']) for r in ledger('estimated-payments.csv'))
    return dict(business_profit=int(profit), rental_profit=int(rent_net),
                se_earnings=int(earnings), se_tax=se_tax, half_se=half_se,
                total_income=int(total_income), agi=int(agi), qbi=int(qbi),
                qbi_deduction=qbi_deduction, taxable_income=int(taxable),
                income_tax=income_tax, child_credit=child_credit, total_tax=total_tax,
                estimated_payments=int(estimates), refund=int(12000 + estimates - total_tax))


def main():
    result = worksheet()
    answers = json.loads((HERE / 'answers.json').read_text())
    links = {
        'business_profit': ('f1040sc', '31. Net profit'),
        'rental_profit': ('f1040se', '26. Total rental'),
        'se_earnings': ('f1040sse', '4a.'),
        'se_tax': ('f1040sse', '12. Self-employment'),
        'half_se': ('f1040s1', '15. Deductible'),
        'total_income': ('f1040', '9. Add'),
        'agi': ('f1040', '11a.'),
        'qbi': ('f8995', '2. Total qualified'),
        'qbi_deduction': ('f1040', '13a.'),
        'taxable_income': ('f1040', '15. Subtract'),
        'income_tax': ('f1040', '16. Amount.'),
        'child_credit': ('f1040', '19. Child'),
        'total_tax': ('f1040', '24. Add'),
        'estimated_payments': ('f1040', '26. 2025 estimated tax payments and amount'),
        'refund': ('f1040', 'Refund. 34.'),
    }
    for name, (form, prefix) in links.items():
        values = [v for k, v in answers['forms'][form]['fields'].items() if k.removeprefix('=').startswith(prefix)]
        assert values == [result[name]], (name, result[name], values)
    assert answers['outcome']['expected'] == result['refund']
    assert answers['headline']['refund'] == result['refund']
    print(json.dumps(result, indent=2))

if __name__ == '__main__':
    main()
