import copy
import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from review_grade import grade_review


class ReviewGradeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for directory in ['forms', 'review', 'documents']:
            (self.root / '2025' / directory).mkdir(parents=True)
        paths = ['2025/forms/f1040.pdf', '2025/documents/w2.txt']
        for path in paths:
            (self.root / path).write_bytes(b'fixture')
        self.config = {'w2_documents': paths[1:], 'w2_complete': True}
        self.validation = {'blocking_errors': 1, 'review_items': 0, 'findings': [{'code': 'bad'}]}
        self.report = {'tax_year': 2025, 'status': 'blocked', 'validation': self.validation,
                       'disputes': [], 'instruction_access': [], 'snapshots': [
                           {'path': p, 'purpose': purpose, 'sha256': hashlib.sha256(b'fixture').hexdigest()}
                           for p, purpose in zip(paths, ['return_pdf', 'w2_source'])]}
        (self.root / '2025/review/return-review.md').write_text('report')

    def evaluate(self, report=None):
        (self.root / '2025/review/return-review.json').write_text(json.dumps(report or self.report))
        with patch('review_grade.subprocess.run', return_value=subprocess.CompletedProcess(
                [], 1, stdout=json.dumps(self.validation), stderr='blocked')) as run:
            result = grade_review(self.root, 2025, 'nii', self.config)
            self.assertIn('--w2-complete', run.call_args.args[0])
            return result

    def test_honest_blocked_report_passes_integrity(self):
        self.assertEqual(self.evaluate()['status'], 'passed')

    def test_forged_status_omitted_findings_and_missing_inventory_fail(self):
        for mutate in [lambda r: r.update(status='checks_passed'),
                       lambda r: r.update(validation={**self.validation, 'findings': []}),
                       lambda r: r.update(snapshots=r['snapshots'][:1])]:
            report = copy.deepcopy(self.report)
            mutate(report)
            self.assertEqual(self.evaluate(report)['status'], 'failed')

    def test_later_pdf_edit_and_extra_pdf_fail(self):
        (self.root / '2025/forms/f1040.pdf').write_bytes(b'edited')
        self.assertEqual(self.evaluate()['status'], 'failed')
        (self.root / '2025/forms/extra.pdf').write_bytes(b'new')
        checks = {c['name']: c['passed'] for c in self.evaluate()['checks']}
        self.assertFalse(checks['complete_file_inventory'])

    def test_missing_report_and_path_escape_fail_closed(self):
        self.assertEqual(grade_review(self.root, 2025, 'nii', self.config)['status'], 'failed')
        report = copy.deepcopy(self.report)
        report['snapshots'][0]['path'] = '../outside.pdf'
        self.assertEqual(self.evaluate(report)['status'], 'failed')

    def test_wrong_year_receipt_and_waived_dispute_fail(self):
        report = copy.deepcopy(self.report)
        report['instruction_access'] = [{'tax_year': 2024, 'pdf': '2025/forms/f1040.pdf',
                                        'sha256': report['snapshots'][0]['sha256']}]
        report['disputes'] = [{'source_access_recorded': True, 'status': 'waived'}]
        checks = {c['name']: c['passed'] for c in self.evaluate(report)['checks']}
        self.assertFalse(checks['instruction_receipt_hashes_and_year'])
        self.assertFalse(checks['disputes_remain_unresolved'])

    def test_unconfigured_case_is_explicitly_skipped(self):
        self.assertEqual(grade_review(self.root, 2025, 'nii', None)['status'], 'not_configured')
