"""Validate answer-key selectors and round-trip their values through official PDFs.
This checks benchmark plumbing, not an agent's tax-preparation performance.
usage: python3 bench/validate.py bench/medium/answers.json /path/to/blank/forms --nii target/debug/nii
No input PDFs are modified. No model API or third-party Python packages required.
"""
import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

from grade import fields, find, grade


def sample(want):
    if isinstance(want, dict) and 'contains_any' in want:
        return want['contains_any'][0]
    if isinstance(want, dict):
        return sample(want['any_of'][0]) if 'any_of' in want else want['contains']
    return want


def validate(answers, forms_dir, nii):
    with tempfile.TemporaryDirectory(prefix='taxbench-key-') as tmp:
        workspace = Path(tmp)
        for stem, spec in answers['forms'].items():
            src = forms_dir / f'{stem}.pdf'
            ff = fields(nii, src)
            values = {}
            for key, want in spec['fields'].items():
                if isinstance(want, dict) and 'row_layout' in want:
                    continue
                hits = find(ff, key)
                if len(hits) != 1:
                    raise ValueError(f'{stem}: {key!r} matched {len(hits)} fields')
                values[hits[0]['name']] = sample(want)
            for layout in spec.get('row_layouts', {}).values():
                # Seed the first accepted presentation and clear unused rows, so
                # round-trips are independent of values in the input template PDF.
                expected = layout['alternatives'][0]
                for i, row in enumerate(layout['rows']):
                    for column, selector in row.items():
                        hits = find(ff, selector)
                        if len(hits) != 1:
                            raise ValueError(f'{stem}: {selector!r} matched {len(hits)} fields')
                        values[hits[0]['name']] = sample(expected[i][column]) if i < len(expected) else ''
            dst = workspace / src.name
            shutil.copy2(src, dst)
            values_file = workspace / 'values.json'
            values_file.write_text(json.dumps(values))
            subprocess.run([nii, 'form', 'fill', str(dst), '--values', str(values_file)],
                           check=True, capture_output=True, text=True)
        report = grade(answers, workspace, nii)
        if not report['return_complete']:
            failures = [r for r in report['results'] if not r[-1]]
            raise ValueError(json.dumps({**{k: v for k, v in report.items() if k != 'results'},
                                         'failed_fields': failures}, indent=2))
        return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('answers', type=Path)
    parser.add_argument('forms', type=Path)
    parser.add_argument('--nii', default='target/release/nii')
    args = parser.parse_args()
    report = validate(json.loads(args.answers.read_text()), args.forms, args.nii)
    print(f"Answer-key PDF round-trip: {report['passed']}/{report['total']}; "
          f"net refund {report['net_refund']:,.0f}; reconciliations pass")

if __name__ == '__main__':
    main()
