"""Keep each fictional case's source packet consistent with its answer key."""
import csv
import json
import re
import unittest
from decimal import Decimal
from pathlib import Path

from grade import matches


class MediumPacketTests(unittest.TestCase):
    def test_source_documents_reconcile_to_keyed_income_and_payments(self):
        root = Path(__file__).resolve().parent.parent
        for case in sorted((root / 'bench').glob('medium-*/case.json')):
            with self.subTest(case=case.parent.name):
                spec = json.loads(case.read_text())
                answers = json.loads((case.parent / 'answers.json').read_text())
                docs = root / spec['inputs'] / 'documents'

                def keyed(form, prefix):
                    hits = [v for k, v in answers['forms'][form]['fields'].items()
                            if k.lstrip('=').startswith(prefix)]
                    self.assertEqual(len(hits), 1)
                    return Decimal(str(hits[0]))

                ledger = list(csv.DictReader((docs / 'business-ledger.csv').read_text().splitlines()))
                receipts = sum(Decimal(r['amount']) for r in ledger if r['category'] == 'receipts')
                deductible = sum(Decimal(r['amount']) * (Decimal('.5') if 'meals' in r['description'] else 1)
                                 for r in ledger if r['category'] == 'expense')
                miles = Decimal(re.search(r'Business miles: (\d+)', (docs / 'mileage.txt').read_text())[1])
                deductible += miles * Decimal('.70')
                self.assertEqual(keyed('f1040sc', '1.'), receipts)
                self.assertEqual(keyed('f1040sc', '28.'), deductible)
                self.assertEqual(keyed('f1040sc', '31.'), receipts - deductible - keyed('f1040sc', '30. Use'))
                rental_rows = list(csv.DictReader((docs / 'rental-ledger.csv').read_text().splitlines()))
                rent = sum(Decimal(r['amount']) for r in rental_rows if r['category'] == 'income')
                annual_cost = sum(Decimal(r['amount']) for r in rental_rows if r['category'] == 'expense')
                ratio = Decimal('.8') if case.parent.name == 'medium-mixed-use' else Decimal(1)
                rental_expenses = (annual_cost + 10000) * ratio
                self.assertEqual(keyed('f1040se', 'Income. Row: 3.'), rent)
                self.assertEqual(keyed('f1040se', 'Row: 20.'), rental_expenses)
                self.assertEqual(keyed('f1040se', 'Row: 21.'), rent - rental_expenses)
                nec = (docs / '1099-nec-morgan.txt').read_text()
                self.assertIn(next(r['amount'] for r in ledger if '1099-NEC' in r['description']), nec)
                w2 = (docs / 'w2-alex.txt').read_text()
                wages = Decimal(re.search(r'Box 1 wages: ([\d.]+)', w2)[1])
                withheld = Decimal(re.search(r'Box 2 federal income tax withheld: ([\d.]+)', w2)[1])
                estimates = sum(Decimal(r['amount']) for r in csv.DictReader(
                    (docs / 'estimated-payments.csv').read_text().splitlines()))
                self.assertEqual(keyed('f1040', 'Income. Attach'), wages)
                self.assertEqual(Decimal(answers['headline']['withheld']), withheld)
                self.assertEqual(keyed('f1040', '26.'), estimates)
                self.assertEqual(Decimal(answers['outcome']['expected']),
                                 withheld + estimates - Decimal(answers['headline']['total_tax']))

    def test_business_loss_has_no_home_office_deduction_or_se_schedule(self):
        key = json.loads((Path(__file__).parent / 'medium-business-loss/answers.json').read_text())
        self.assertNotIn('f1040sse', key['forms'])
        self.assertIn('f1040sse', key['not_required'])
        office = [v for k, v in key['forms']['f1040sc']['fields'].items() if k.startswith('=30. Use')]
        self.assertEqual(office, [0])

    def test_business_loss_unused_se_lines_accept_blank_or_zero_only(self):
        key = json.loads((Path(__file__).parent / 'medium-business-loss/answers.json').read_text())
        for form, prefix in [('f1040', '=23.'), ('f1040s1', '=15.'), ('f1040s1', '=26.')]:
            selector = next(k for k in key['forms'][form]['fields'] if k.startswith(prefix))
            want = key['forms'][form]['fields'][selector]
            for value in [None, '', '0', '0.00']:
                self.assertTrue(matches(want, value, selector, 1))
            for value in ['10', 'NaN', 'not a number']:
                self.assertFalse(matches(want, value, selector, 1))
