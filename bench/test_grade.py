"""Grader regressions: python3 -m unittest discover -s bench -p 'test_*.py'."""
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

    def test_exact_selector_resolves_checkbox_and_amount_overlap(self):
        ff = [{'description': '1. Gross receipts or sales.', 'page': 1},
              {'description': 'Part I. Income. 1. Gross receipts or sales.', 'page': 1}]
        self.assertEqual(len(find(ff, '1. Gross receipts or sales.')), 2)
        self.assertEqual(find(ff, 'p1|=1. Gross receipts or sales.'), ff[:1])

if __name__ == '__main__':
    unittest.main()
