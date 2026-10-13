"""Deterministic report integrity checks; does not grade prose or tax interpretation."""
import hashlib
import json
import subprocess
from pathlib import Path


def grade_review(workspace, year, nii, config):
    if config is None:
        return {'status': 'not_configured', 'passed': 0, 'total': 0, 'checks': []}
    root = Path(workspace).resolve()
    checks = []

    def check(name, ok):
        checks.append({'name': name, 'passed': bool(ok)})

    def local(relative):
        path = Path(relative)
        if path.is_absolute():
            raise ValueError('absolute artifact path')
        resolved = (root / path).resolve()
        if not resolved.is_relative_to(root):
            raise ValueError('artifact outside workspace')
        return resolved

    def hashed(item):
        return hashlib.sha256(local(item['path']).read_bytes()).hexdigest() == item['sha256']

    try:
        report = json.loads(local(f'{year}/review/return-review.json').read_text())
        check('report_present_and_parseable', isinstance(report, dict))
        check('tax_year', report['tax_year'] == int(year))
        check('readable_report_present', local(f'{year}/review/return-review.md').is_file())
        w2s = config['w2_documents']
        command = [str(nii), '--workspace', str(root), 'validate', '--year', str(year)]
        for source in w2s:
            command += ['--w2', source]
        if config['w2_complete']:
            command += ['--w2-complete']
        proc = subprocess.run(command, capture_output=True, text=True)
        # Nonzero is expected for blocked/review returns. Invalid output fails closed.
        independent = json.loads(proc.stdout)
        check('independent_validation_matches', report['validation'] == independent)
        expected = ('blocked' if independent['blocking_errors'] else
                    'needs_review' if independent['review_items'] or report['disputes'] else 'checks_passed')
        check('status_matches_findings', report['status'] == expected)
        snapshots = report['snapshots']
        observed = {(s['path'], s['purpose']) for s in snapshots}
        required = {(p.relative_to(root).as_posix(), 'return_pdf')
                    for p in local(f'{year}/forms').iterdir() if p.suffix.lower() == '.pdf'}
        required |= {(source, 'w2_source') for source in w2s}
        check('complete_file_inventory', observed == required and len(observed) == len(snapshots))
        check('snapshot_hashes_match', all(hashed(s) for s in snapshots))
        receipts = report['instruction_access']
        check('instruction_receipt_hashes_and_year', all(
            r['tax_year'] == int(year) and hashed({'path': r['pdf'], 'sha256': r['sha256']})
            for r in receipts))
        check('disputes_remain_unresolved', all(
            d['status'] == ('pending_manual_review' if d['source_access_recorded'] else 'source_required')
            for d in report['disputes']))
    except (OSError, ValueError, KeyError, TypeError, AttributeError) as error:
        check('report_evaluation', False)
        return {'status': 'failed', 'passed': sum(c['passed'] for c in checks),
                'total': len(checks), 'checks': checks, 'error': str(error)}
    passed = sum(c['passed'] for c in checks)
    return {'status': 'passed' if passed == len(checks) else 'failed',
            'passed': passed, 'total': len(checks), 'checks': checks,
            'limits': 'Checks report integrity, not prose accuracy, quote relevance, or filing readiness. Source receipt authenticity is not independently established.'}
