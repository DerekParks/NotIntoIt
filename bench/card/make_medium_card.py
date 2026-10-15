"""Render the pinned medium comparison from its committed result snapshot.

uv run --with matplotlib python bench/card/make_medium_card.py
"""
import json
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / 'docs/taxbench-medium.json'
OUT = ROOT / 'docs/taxbench-medium.png'
MODELS = [('claude-sonnet-5-5', 'Sonnet 5.5', 'Claude Code'),
          ('openrouter/openai/gpt-6-luna', 'GPT-6 Luna', 'pi / OpenRouter'),
          ('openrouter/deepseek/deepseek-v4-pro-0813', 'DeepSeek V4 Pro', 'pi / OpenRouter · 0813')]
CASES = [('medium', 'Original'), ('medium-rental-loss', 'Rental loss'),
         ('medium-mixed-use', 'Mixed use'), ('medium-business-loss', 'Business loss'),
         ('medium-estimated-payments', 'Est. payments')]
BG, PANEL, TEXT, MUTED = '#1a1a19', '#242423', '#f0efec', '#aaa99e'


def main():
    data = json.loads(DATA.read_text())
    rows = data['runs']
    indexed = {(r['model'], r['case']): r for r in rows}
    if len(indexed) != len(rows):
        raise ValueError('Duplicate model/case attempt in comparison snapshot')
    fig = plt.figure(figsize=(16.5, 7.3), dpi=160, facecolor=BG)
    ax = fig.add_axes([0, 0, 1, 1]); ax.set_xlim(0, 16.5); ax.set_ylim(0, 7.3); ax.axis('off')
    ax.text(.45, 6.72, 'TaxBench: medium returns', color=TEXT, fontsize=25, weight='bold')
    ax.text(.45, 6.28, 'Five fictional 2025 joint returns · one attempt per model/case · October 14, 2026',
            color=MUTED, fontsize=11)
    ax.text(.45, 5.78, 'MODEL / AGENT', color=MUTED, fontsize=9)
    start, width = 3.35, 1.42
    for i, (_, label) in enumerate(CASES):
        ax.text(start + i*width + .62, 5.78, label, color=MUTED, ha='center', fontsize=10)
    for x, label in [(11.05, 'Refunds'), (12.5, 'Form set'), (14, 'Reports'), (15.5, '$ total')]:
        ax.text(x, 5.78, label, color=MUTED, ha='center', fontsize=10)
    for j, (model, label, agent) in enumerate(MODELS):
        y = 4.72 - j*1.3
        ax.text(.45, y+.13, label, color=TEXT, fontsize=15, weight='bold')
        ax.text(.45, y-.20, agent, color=MUTED, fontsize=9)
        selected = [indexed[(model, case)] for case, _ in CASES if (model, case) in indexed]
        for i, (case, _) in enumerate(CASES):
            r = indexed.get((model, case))
            valid = r and 'score' in r and r.get('total', 0)
            percentage = r['score']/r['total'] if valid else 0
            color = '#246346' if percentage == 1 else '#285780' if percentage >= .9 else '#804c32' if valid else PANEL
            ax.add_patch(FancyBboxPatch((start+i*width, y-.40), 1.24, .92,
                                       boxstyle='round,pad=0.02,rounding_size=0.08', facecolor=color, edgecolor='none'))
            if valid:
                marker = '*' if r.get('status') != 'completed' else ''
                ax.text(start+i*width+.62, y+.11, f'{percentage:.0%}{marker}', ha='center', color=TEXT, fontsize=17, weight='bold')
                ax.text(start+i*width+.62, y-.20, f"{r['score']}/{r['total']}", ha='center', color=TEXT, fontsize=9)
            else:
                ax.text(start+i*width+.62, y, 'not run', ha='center', color=MUTED, fontsize=10)
        n = len(selected)
        right = sum(bool(r.get('outcome_correct')) for r in selected)
        reports = sum(r.get('review_grade', {}).get('status') == 'passed' for r in selected)
        complete = sum(bool(r.get('return_complete')) for r in selected)
        cost = sum(r.get('usage', {}).get('cost', 0) for r in selected)
        for x, value in [(11.05, f'{right}/{n}'), (12.5, f'{complete}/{n}'), (14, f'{reports}/{n}'), (15.5, f'${cost:.2f}')]:
            ax.text(x, y, value, color=TEXT, ha='center', fontsize=16)
    cost = sum(r.get('usage', {}).get('cost', 0) for r in rows)
    completed = sum(r.get('status') == 'completed' for r in rows)
    ax.text(.45, 1.35, f"{len(rows)} attempts · {completed} completed · ${cost:.2f} reported API cost", color=TEXT, fontsize=12)
    ax.text(.45, .96, 'Cells: raw answer-key matches. Refunds: within $1. Form set: keyed completeness. Reports: artifact integrity, not tax correctness.', color=MUTED, fontsize=9)
    ax.text(.45, .62, 'Loss signs, blank zeros and property classification affect raw points. Rental/mixed-use limits need review. * Incomplete attempt.', color=MUTED, fontsize=9)
    ax.text(.45, .28, 'Small, related cases; no reliability or filing-readiness claim. Pinned IDs + per-attempt data: docs/taxbench-medium.json', color=MUTED, fontsize=9)
    fig.savefig(OUT, facecolor=BG)
    plt.close(fig)
    print(OUT)


if __name__ == '__main__':
    main()
