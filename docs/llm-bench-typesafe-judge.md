# LLM Bench: replacing the scoring judge with TypeSafe

Date: 2026-09-20

> **Correction (2026-09-20, later the same day).** The scoring design recorded below is
> wrong in one important way, and this document is kept as the record of what was built,
> not as a recommendation. See `llm-bench-judge-audit-2026-09-20.md`. ("Shipped" below means
> "the default in the working tree": this judge was never committed.)
>
> - The "Scoring" section reads `fidelity`'s negative within-case correlation with the
>   checkpoints (−0.778 here; −0.758 reproducibly) as *independence*. It is an *inversion*:
>   `fidelity` is anchored to the ASR original, so every correct fix lowers it. On the
>   multi-error case the shipped `quality` averages 7.84 for outputs that fixed all three
>   errors and 8.03 for outputs that fixed none.
> - `asr_fix`, the only dimension that separates providers, was given weight 0.
> - Every accuracy figure below was measured on the same data the rubric was tuned on.
> - The audit also lists eight implementation defects (no de-duplication, silent defaults,
>   no retry, confidence averaged away, …) and proposes a diff-driven replacement.
>
> **Superseded.** The rubric, its `quality_*` weights and the free-form judge described below were
> removed from `tools/llm-bench` the same day; the diff-driven replacement is what the tool runs now
> (audit §8–§9). Nothing in the "Configuration" section below applies any more.

The benchmark's `quality` dimension used to come from a free-form LLM judge: one
call that saw every provider's output and replied with JSON scoring the whole
field. This documents why that was replaced with [TypeSafe](https://docs.typesafe.ai/api),
what was measured, and how to tune or revert it.

## The API

Single endpoint, bearer auth:

```
POST https://api.typesafe.ai/v1/systemone
Authorization: Bearer <key>
```

```json
{
  "state": { "asr_original": "...", "corrected_output": "..." },
  "model": "jev-latest",
  "questions": {
    "fidelity":     { "type": "score", "instructions": "...", "criteria": ["level 0", "level 1", "level 2"] },
    "clean_output": { "type": "noul",  "instructions": "...", "criteria": { "true": "...", "false": "..." } }
  }
}
```

`state` may be a string, object or array — we pass an object so the rubric can
refer to the ASR input, the model's output, the reference answer and the hot-word
dictionary by name. Three question types exist: `noul` (yes/no → 0..1),
`choice` (categorical + probabilities) and `score` (2–10 ordered levels).

A `score` answer returns the probability-weighted position on the scale, the full
distribution, and a confidence:

```json
{ "type": "score", "score": 1.81, "confidence": 0.71,
  "probabilities": {"0": 0.0, "1": 0.19, "2": 0.81},
  "legend": {"0": "...", "1": "...", "2": "..."} }
```

Model `jev-latest` → `jev-1.13.0`. 64k context (32k for state + longest question),
1,200 req/min, 250k tok/s. Errors: 401 / 422 / 429 / 529.

## Why replace the old judge

Measured over one full benchmark (16 providers × 2 cases × 2 rounds = 60 scored
outputs), scoring the *same* outputs with both judges:

| per-provider, n=15 | old free-form judge | TypeSafe (as shipped) |
|---|---|---|
| quality spread across providers | **2.93** | 0.40 |
| sd across providers | **0.800** | 0.102 |
| correlation with program checkpoints | +0.973 | **−0.087** |
| ⤷ of which redundant (sd·│r│) | 0.778 | **0.009** |
| ⤷ of which new information | **0.185** | 0.101 |
| run-to-run sd (3 repeats, same input) | 0.072 | **0.019** |
| new information ÷ noise | 2.6 | **5.3** |
| failure mode | one bad reply loses the whole dimension | per-output, degrades gracefully |

Read that table carefully, because it does not say TypeSafe wins outright. The
old judge **discriminates far more** — a 2.93-point spread against 0.40. But
94.7% of its variance (r² = 0.947) is explained by the checkpoints the program
already scores deterministically. The checkpoints (`basic` / `cloud` / `bonus`)
already carry ~62% of the composite, so the old judge's 10% quality slot was
very nearly counting the same evidence a second time: its top three were exactly
the three providers at 100% checkpoints. Its own prompt told it not to
(*"硬性检查点…由程序按维度单独计分…请勿重复重罚"*), and its written rationales show
it did anyway (*"未结合语境纠正下一集"*, *"保留 read me"*).

Strip the redundant part and the two judges end up closer than the headline
spread suggests: 0.185 of genuinely new signal from the old judge against 0.101
from TypeSafe. TypeSafe contributes **less** usable signal in absolute terms —
about 55% as much — but almost all of what it contributes is new, and it is 2×
cleaner relative to its own run-to-run noise. That is the actual trade, and it
is narrower than "replace the judge" makes it sound.

The old judge was also, at the time of this work, **failing silently**: the
configured `gpt-5.6-terra` rejects `reasoning_effort = "minimal"` with HTTP 400,
so the judge produced nothing, `quality` was dropped from the weighting, and the
ranking printed `quality 0%` without ever saying the judge had broken. The same
stale value was zeroing the `GPT-5.6-luna` provider. Both are fixed to `"none"`.

## The rubric

`tools/llm-bench/typesafe_questions.json`, four one-dimensional questions
(per the docs' guidance: concrete situations rather than degrees, each level
described independently, structured `{what, examples}` where levels blur):

| dimension | type | what it catches |
|---|---|---|
| `fidelity` | score 0–2 | invented content, dropped sentences, answering the text instead of correcting it |
| `fluency` | score 0–2 | missing punctuation, garbled text, inconsistent 中英 mixing |
| `asr_fix` | score 0–2 | homophone / near-sound errors corrected |
| `clean_output` | noul | preamble, code fences, commentary around the text |

Validated against hand-built outputs with known-correct ordering before any real
data was scored:

| candidate | fidelity | fluency | asr_fix | clean |
|---|---|---|---|---|
| perfect | 1.88 | 1.71 | 1.91 | 0.92 |
| untouched ASR | 1.99 | 1.51 | **0.05** | 0.81 |
| hallucinated tail | **0.03** | 1.75 | 1.87 | 0.24 |
| truncated | **0.02** | 1.87 | 0.63 | 0.85 |
| answered instead of corrected | **0.02** | 1.85 | 1.80 | 0.41 |
| preamble | 1.22 | 1.66 | 1.91 | **0.03** |
| code fence | 1.84 | 1.65 | 1.89 | **0.09** |
| punctuation stripped | 1.88 | **0.09** | 1.83 | 0.92 |

Two rubric revisions were needed to get there. `asr_fix` originally ranked a
*missed* correction above a correct one, because nothing told the model which
word was wrong — fixed by passing `reference_answer` and `dictionary` in the
state. `fluency` originally rewarded rewriting spoken phrasing into formal prose,
scoring a faithful correction *below* a truncated one — fixed by scoping it to
mechanical polish and saying outright that spoken connectives are correct here.

## Scoring

Each successful round is scored on its own, then averaged per provider:

```
quality = 10 × (0.55·fidelity + 0.45·fluency) × (0.6 + 0.4·clean_output)
```

Rounds that errored are skipped, matching `score_checkpoints`; a provider with no
successful round scores 0. Weights are renormalized, so only their ratio matters.

**`asr_fix` ships at weight 0**, and that single choice is what buys the
orthogonality — and what costs the discrimination. Sweeping its weight:

| asr_fix weight | r vs checkpoints | quality spread |
|---|---|---|
| **0.00 (shipped)** | **−0.087** | **0.40** |
| 0.10 | +0.445 | 0.52 |
| 0.20 | +0.635 | 0.79 |
| 0.35 | +0.715 | 1.13 |
| 0.50 | +0.740 | 1.40 |

There is no setting that is both discriminating and independent. `asr_fix` is the
dimension that separates providers (per-output sd 0.090, against 0.017 for
fidelity and 0.021 for fluency) and it is also the one that measures what the
checkpoints already measure (r = +0.887). Weighting it buys spread by buying
redundancy. Shipping it at 0 is a deliberate choice of independence over spread;
raise it if you would rather have a quality column that moves.

It is still requested and printed, because it rides free in the same call and is
the most diagnostic column when a model regresses. `Qwen3.7-Flash` is the worked
example: `asr_fix` 0.51, by far the lowest, yet `quality` 8.70 in line with
everyone else. It still lands near the bottom of the composite, via the
`cloud 0%` column where that failure is actually scored.

`fidelity` is the dimension that earns its place on independence: r = +0.068
against checkpoints at the output level, and −0.778 within the one case that
varies. Checkpoints are substring and regex matches; they structurally cannot
notice that a model invented a paragraph, truncated the input, or answered it,
and `fidelity` drops such an output from ~8.7 to under 4.0.

`fluency` is the weaker justification, and worth stating plainly: at the output
level it correlates **+0.773** with the checkpoints, nearly as high as the
`asr_fix` that was excluded for exactly that reason. It survives at 45% because
the correlation is an artifact of pooling two cases of different difficulty — the
blended provider-level quality still lands at −0.087, and within the varying case
`fluency` falls to +0.330. That is a defensible reading of thin data, not a
strong one. If a third case is added and provider-level r climbs, `fluency` is
the first weight to revisit.

## Reading the output

```
  Provider                  Quality   Fidel   Fluen   Clean   AsrFx    Cnf
  DeepSeek V4 Flash (Volc)     8.95    0.97    0.87    0.92    0.93   0.77
  Qwen3.7-Flash                8.70    0.99    0.83    0.86    0.51   0.80
```

`Cnf` is the mean confidence across the score questions — the probability mass
sitting on one level. It measures the **rubric**, not the provider: a persistently
low `Cnf` means the level descriptions leave the model split on this kind of data
and should be retuned. It is how the two rubric bugs above were caught (the
original `fluency` sat at 0.25–0.30; it now runs 0.65–0.90).

Expect a narrow quality spread on a small, easy case set — on the current two
cases every provider returns faithful, clean text, and in case 1 all sixteen
score identically. That is the honest reading, not a defect: quality is a
guardrail against catastrophic rewrites, while the checkpoints do the
term-level discrimination. Add harder cases to make the dimension move.

## Configuration

```toml
[judge]
name = "TypeSafe-Judge"
base_url = "https://api.typesafe.ai/v1/systemone"
api_key = "file:.jev_key"     # literal, ${ENV_VAR}, or file:<path>
model = "jev-latest"
type = "typesafe"
```

Optional, in `[eval]`:

| key | default | meaning |
|---|---|---|
| `quality_fidelity` | 0.55 | weight of fidelity |
| `quality_fluency` | 0.45 | weight of fluency |
| `quality_asr_fix` | 0.0 | weight of asr_fix — see above before raising |
| `quality_clean_floor` | 0.6 | quality kept when output is dirty; 1.0 disables the gate |
| `typesafe_questions` | embedded | path to an alternative rubric |
| `typesafe_concurrency` | 8 | parallel scoring calls |

Cost is about **$0.005 per full benchmark run** (~127k input tokens at
$0.042/M). To go back to the free-form judge, uncomment the `[judge]` block kept
below the new one in `config.toml`; both code paths remain, selected by `type`.

## Is it worth switching?

On today's test set, the honest answer is **not urgent**. The shipped quality
column spans 0.40 points across fifteen providers; at 10% weight that is 0.4 of
100 composite points, and it reorders nobody. Both judges are close to
decorative here — the old one because its spread merely restates the
checkpoints, the new one because it is nearly flat.

What justifies the switch is instrument quality rather than today's ranking:
4× better run-to-run stability, a typed response that cannot silently cost the
whole dimension, per-output scoring with no position bias between providers, and
a rubric validated against hand-built failures (hallucination, truncation,
preamble, fences) that the checkpoints cannot catch by construction. Those
properties matter when a provider actually regresses — which none does on the
current two cases.

Note also that the strongest argument in the original writeup — "the old judge
failed silently" — was mostly a **config bug**, not an architectural flaw. With
`reasoning_effort = "none"` the old judge runs fine. What remains architectural
is the *silence*: a free-form judge can still return unparseable JSON, which is
why `parse_judge_report` carries fence-stripping fallbacks at all.

The highest-value next step is not judge selection. It is adding test cases where
models actually fail, then re-measuring both.

## Caveats

- **The quality dimension barely discriminates as shipped** (spread 0.40, sd
  0.102). Treat it as a guardrail against catastrophic rewrites, not as a ranking
  signal, until harder cases exist.
- Two test cases is a thin basis. The correlations above rest on one case with
  checkpoint variance (case 1 has none — every provider is identical there).
  Treat r≈0.89 for `asr_fix` as directional, and re-measure after adding cases.
- The rubric is written for Chinese ASR correction and assumes `expected` is
  present on a case. It falls back to scoring without a reference, but `asr_fix`
  is much weaker that way — that was the v1 failure.
- `jev-latest` floats. Pin `jev-1.13.0` if a run needs to be reproducible.
