# Medium model card

![Medium TaxBench results](../docs/taxbench-medium.png)

October 14, 2026 · five fictional 2025 joint-return cases · one attempt per model/case.

| Model | Raw key checks | Refunds within $1 | Report integrity | Mean seconds | Total cost |
|---|---:|---:|---:|---:|---:|
| Sonnet 5.5 | 604/608 | 5/5 | 5/5 | 79 | $2.20 |
| GPT-6 Luna | 606/608 | 5/5 | 5/5 | 193 | $0.15 |
| DeepSeek V4 Pro (0813) | 605/608 | 5/5 | 5/5 | 565 | $1.14 |

**15 attempts; $3.49 total reported API cost**, under the $8 cap. No retries or resumptions.

Pins: `claude-sonnet-5-5` via Claude Code; `openai/gpt-6-luna` and `deepseek/deepseek-v4-pro-0813` via pi/OpenRouter.

Raw scores retain loss-sign/blank-zero conventions and the mixed-use property-code mismatch; they are key matches, not a legal accuracy percentage. Keys stayed fixed. Extra forms affect form-set completeness. Rental-loss and mixed-use limits remain review items. Report integrity does not certify tax advice or filing readiness.

Small, related cases; all favor the standard deduction. No reliability or itemization-selection claim.

[Chart data, costs, misses and provenance](../docs/taxbench-medium.json) · [Case definitions](medium-variants.md). Raw PDFs/transcripts remain local under ignored `bench/runs/`.

Regenerate: `uv run --with matplotlib python bench/card/make_medium_card.py`.
