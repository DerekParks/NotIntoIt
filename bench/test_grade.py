"""Grader regressions: python3 -m unittest discover -s bench -p 'test_*.py'."""
import copy
import itertools
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from grade import find, grade

class GraderTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.forms = Path(self.temp.name)
        self.answers = {
            'forms': {'f1040': {'required': True, 'fields': {'refund': 100}},
                      'schedule': {'required': True, 'fields': {'profit': 500}}},
            'not_required': ['unneeded'],
            'outcome': {'form': 'f1040', 'refund': 'refund', 'owe': 'owe', 'expected': 100},
            'reconciliations': [{'name': 'income transfer',
                'total': {'form': 'f1040', 'field': 'income'},
                'parts': [{'form': 'schedule', 'field': 'profit'}]}],
        }
        self.data = {'f1040': [{'description': 'refund', 'value': '100'},
                               {'description': 'owe', 'value': ''},
                               {'description': 'income', 'value': '500'}],
                     'schedule': [{'description': 'profit', 'value': '500'}],
                     'unneeded': []}
        for stem in ['f1040', 'schedule']:
            (self.forms / f'{stem}.pdf').touch()

    def report(self):
        with patch('grade.fields', side_effect=lambda _, pdf: self.data[pdf.stem]):
            return grade(self.answers, self.forms, 'nii')

    def test_missing_schedule_cannot_pass_on_correct_refund(self):
        (self.forms / 'schedule.pdf').unlink()
        r = self.report()
        self.assertTrue(r['outcome_correct'])
        self.assertFalse(r['return_complete'])
        self.assertEqual(r['missing_required_forms'], ['schedule'])

    def test_blank_and_corrupt_forms_are_reported(self):
        self.data['schedule'] = []
        self.assertEqual(self.report()['blank_required_forms'], ['schedule'])
        with patch('grade.fields', side_effect=subprocess.CalledProcessError(1, 'nii')):
            r = grade(self.answers, self.forms, 'nii')
        self.assertFalse(r['return_complete'])
        self.assertEqual(len(r['unreadable_forms']), 2)

    def test_blank_extra_download_is_ok_but_populated_extra_is_not(self):
        (self.forms / 'unneeded.pdf').touch()
        self.assertTrue(self.report()['return_complete'])
        self.data['unneeded'] = [{'description': 'unused', 'value': '1'}]
        self.assertEqual(self.report()['unneeded_forms'], ['unneeded'])
        self.assertFalse(self.report()['return_complete'])

    def test_matching_net_with_simultaneous_refund_and_debt_is_rejected(self):
        self.data['f1040'][0]['value'] = '200'
        self.data['f1040'][1]['value'] = '100'
        self.assertFalse(self.report()['outcome_correct'])

    def test_reconciliation_detects_individually_ungraded_income(self):
        self.data['f1040'][2]['value'] = '550'
        r = self.report()
        self.assertEqual(r['passed'], r['total'])
        self.assertEqual(r['consistency_problems'], ['income transfer'])
        self.assertFalse(r['return_complete'])

    def test_multiple_filing_statuses_are_rejected(self):
        self.data['f1040'] += [
            {'description': 'Married filing jointly', 'value': '1'},
            {'description': 'Head of household', 'value': '1'}]
        self.assertFalse(self.report()['return_complete'])

    def test_zero_expected_refund_still_requires_an_explicit_outcome(self):
        self.answers['outcome']['expected'] = 0
        self.data['f1040'][0]['value'] = ''
        self.assertFalse(self.report()['outcome_correct'])

    def test_qbi_reconciliation_includes_all_five_rows(self):
        # A three-row allocation has the same total as a two-row presentation.
        answers = json.loads((Path(__file__).parent / 'medium/answers.json').read_text())
        check = next(c for c in answers['reconciliations']
                     if c['name'] == 'QBI business rows sum to total regardless of order')
        self.answers['reconciliations'] = [check]
        (self.forms / 'f8995.pdf').touch()
        self.data['f8995'] = [
            {'description': check['total']['field'][1:], 'value': '33739'},
            *[{'description': part['field'][1:], 'value': str(value)}
              for part, value in zip(check['parts'], [29739, 2000, 2000, 0, 0])],
        ]
        self.assertEqual(self.report()['consistency_problems'], [])
        self.data['f8995'][3]['value'] = '3000'
        self.assertIn(check['name'], self.report()['consistency_problems'])

    def qbi_layout_case(self, entries, positions=None):
        medium = json.loads((Path(__file__).parent / 'medium/answers.json').read_text())
        spec = copy.deepcopy(medium['forms']['f8995'])
        spec['fields'] = {key: want for key, want in spec['fields'].items()
                          if isinstance(want, dict) and 'row_layout' in want}
        self.answers['forms']['f8995'] = spec
        self.answers['reconciliations'] = []
        (self.forms / 'f8995.pdf').touch()
        schema = spec['row_layouts']['qbi_business_and_rental']['rows']
        positions = positions if positions is not None else range(len(entries))
        assigned = dict(zip(positions, entries))
        self.data['f8995'] = [
            {'description': selector.lstrip('='), 'value': assigned.get(i, {}).get(column, '')}
            for i, row in enumerate(schema) for column, selector in row.items()
        ]

    def test_qbi_combined_and_split_rental_rows_accept_any_order_or_position(self):
        business = {'name': 'Morgan graphic design', 'identifier': '000-12-3402', 'income': '29739'}
        combined = {'name': 'Rivera Rental', 'identifier': '000-12-3401', 'income': '4000'}
        alex = {'name': 'Larkspur Lane - Alex', 'identifier': '000123401', 'income': '2000'}
        morgan = {'name': 'Larkspur Lane - Morgan', 'identifier': '000123402', 'income': '2000'}
        for entries in [[business, combined], [business, alex, morgan]]:
            for positions in itertools.permutations(range(5), len(entries)):
                with self.subTest(positions=positions, entries=len(entries)):
                    self.qbi_layout_case(entries, positions)
                    report = self.report()
                    self.assertEqual(report['passed'], report['total'])
                    self.assertTrue(report['return_complete'])
        combined['identifier'] = '000-12-3402'
        self.qbi_layout_case([combined, business])
        self.assertTrue(self.report()['return_complete'])

    def test_qbi_layout_rejects_wrong_owner_duplicate_owner_and_offsetting_rows(self):
        b = {'name': 'Morgan graphic design', 'identifier': '000-12-3402', 'income': '29739'}
        a = {'name': 'Alex rental', 'identifier': '000-12-3401', 'income': '2000'}
        m = {'name': 'Morgan rental', 'identifier': '000-12-3402', 'income': '2000'}
        invalid = [
            [b, a],  # missing half the rent
            [{**b, 'identifier': '000-12-3401'}, a, m],  # business attributed to Alex
            [b, a, a],  # duplicated owner, despite correct total
            [b, {**a, 'income': '3000'}, {**m, 'income': '1000'}],
            [b, a, m, {'name': 'Adjustment', 'identifier': '000-12-3401', 'income': '100'},
                       {'name': 'Offset', 'identifier': '000-12-3401', 'income': '-100'}],
            [b, {**a, 'identifier': ''}, m],
            [b, {**a, 'name': ''}, m],
            [b, {**a, 'income': 'NaN'}, m],
            [b, {**a, 'income': 'Infinity'}, m],
            [{**b, 'name': 'Unrelated activity'}, a, m],
        ]
        for entries in invalid:
            with self.subTest(entries=entries):
                self.qbi_layout_case(entries)
                report = self.report()
                self.assertFalse(report['return_complete'])
                self.assertEqual(sum(not r[-1] for r in report['results']), 2)

    def test_qbi_layout_empty_rows_and_missing_schema_fields(self):
        b = {'name': 'Graphic design', 'identifier': '000-12-3402', 'income': '29739'}
        r = {'name': 'Residential rental', 'identifier': '000-12-3401', 'income': '4000'}
        self.qbi_layout_case([b, r, {'income': '0'}])
        self.assertTrue(self.report()['return_complete'])
        self.qbi_layout_case([b, r])
        self.data['f8995'].pop()
        report = self.report()
        self.assertFalse(report['return_complete'])
        self.assertEqual(len(report['key_problems']), 1)
        self.qbi_layout_case([b, r])
        self.data['f8995'].append(self.data['f8995'][0].copy())
        self.assertFalse(self.report()['return_complete'])

    def test_exact_selector_resolves_checkbox_and_amount_overlap(self):
        ff = [{'description': '1. Gross receipts or sales.', 'page': 1},
              {'description': 'Part I. Income. 1. Gross receipts or sales.', 'page': 1}]
        self.assertEqual(len(find(ff, '1. Gross receipts or sales.')), 2)
        self.assertEqual(find(ff, 'p1|=1. Gross receipts or sales.'), ff[:1])

if __name__ == '__main__':
    unittest.main()
