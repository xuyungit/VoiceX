# LLM correction benchmark

Copy `config.example.toml` and `test_cases.example.toml` to the local config/cases
files, then run `cargo run` in this directory. Credentials, run artifacts and
standings are ignored by Git. Each run saves `report.log` and `results.json`.

## Scoring version 6

Every case is worth 100 points. Average rounds within a case, then average cases
with equal weight; failed calls remain in both denominators.

| Component | Points | Rule |
| --- | ---: | --- |
| Correction | 30 | Average fixed repair tasks derived from input/reference. Dictionary and contextual repairs use the same task budget. Repeated occurrences of the same repair share one task; occurrence credits average within it. |
| Fidelity | 40 | Preserve substantive entities, numbers, versions, facts and central intent. Keeping an ASR mistake introduces no hallucination, but earns no correction credit. |
| Filler/stutter removal | 10 | Average removal credit at reference-derived cleanup sites. |
| Readability | 5 | One whole-text JEV question about punctuation, grammar and sentence boundaries. |
| Conversational style | 5 | One whole-text JEV question about voice and organization. Mild edits are assessed together. |
| Speed | 10 | Absolute per-call score: full at 500 ms, zero at the application's text-dependent timeout, log-linear between. |

A repair miss affects only its correction task. For example, in the current six
cases, missing all four Abacus → Abaqus occurrences while fixing Midas loses
15 points in that case, or **2.5 points in the overall composite**. Fixing two of
the four occurrences earns half of that task's credit. Other dimensions are not
penalized for the repair miss.

Readability and style explicitly exclude correction misses and factual changes.
Formal but readable writing can earn full readability and lose style credit.
A fluent refusal can be readable while failing fidelity. The severe-hallucination
gate prevents readable wrong answers from retaining quality points.

A confirmed severe hallucination zeros **that output round's quality** (90 points
at default weights); its measured speed stays independent. Failed correction
calls score zero everywhere. Slight wording/nuance changes are outside the
severe check. Terminal evaluator failures are visible and score zero in affected
components; failed whole-text fidelity evaluation zeros that round's quality.
Every model still receives a final ranking; there is no human handoff.

Weights are explicit fractions and must sum to 1:

```toml
[eval]
weight_correction = 0.30
weight_fidelity = 0.40
weight_clean = 0.20
weight_latency = 0.10
```

Old dictionary/semantic/basic/cloud/quality/success weight keys are rejected with
migration guidance. This prevents silently reusing the old effective dictionary
weight of 50%. `Able#` ranks quality alone; `Fast#` ranks successful-call latency.
The speed timeout matches the app: 10 seconds up to 120 characters, multiplied by
1.5 per doubling (rounded up), at most 60 seconds. Judge time is excluded.

The occurrence-level Dictionary/Semantic/Clean table remains diagnostic evidence;
it does not determine the composite. Results also save each fixed task plan,
occurrence credit, whole-text quality judgment, ungated/gated metric, case score
and provider score. All models use the same plan for a case.

## Automatic severe-hallucination check

Every successful output gets a whole-transcript check against raw ASR input and
the human-approved correction reference. The reference authorizes repairs; it is
not required wording or independent external fact verification. An exactly
unchanged output introduces no new hallucination and passes fidelity directly.

`severe_hallucination_rubric.json` defines the default rubric. It distinguishes
retained ASR mistakes (including formatting `cloud md` as `cloud.md`) from newly
substituted names/files/versions. It also checks substantial candidate omissions,
reversed core requirements and invented validation/decisions. Turning a central
question into a firm assertion is severe; slight hedging changes are not.

JEV's `changed`, `uncertain`, missing judgments and `faithful` judgments below
**0.7 confidence** go to `[hallucination_reviewer]` for an independent binary
answer with a reason. All severe penalties require that review. Equivalent
questions share results within a run. Malformed/failed reviewer requests get one
retry; terminal errors are saved separately from hallucinations and score zero
quality. Missing local repair/quality judgments also score zero visibly. The
ranking's `Checks / H / E` shows finalized fidelity coverage, severe hallucinations
and output rounds with evaluator errors.

`typesafe_questions.json` supplies local questions; `quality_questions.json`
supplies readability/style. A custom questions file can override `fidelity`,
`readability` and `style`. Configure reviewer reasoning explicitly and validate the
reviewer against the probes. `--skip-judge` still finalizes fidelity through the
reviewer, but missing correction/readability/style answers score zero.

The original Luna/Deepseek/GLM/GPT 6.1/Sora dictation is in `fidelity_cases.toml`
with its history provenance, and in the default/example cases. The historical
corrected output is not the reference answer.

## One-sentence model reviews

Every completed run prints a Chinese `一句话评测` section in ranking order and
saves `assessment` plus `assessment_evidence` for every entry in `results.json`'s
`ranking`. Reviews are generated from the measured run, without additional model
requests, and do not affect scores, ranking or season points.

A review names observed strengths, average successful-call latency, serious
hallucination/meaning-change counts and the main repair weaknesses. Dictionary
misses use actual task names; contextual misses use the approved repair or a
structural description of a trailing qualifier. Weakness priority follows their
repair-point contribution, with repeated occurrences sharing the task weight.
Tiny probability-level deductions are omitted from prose. Successful calls over
five seconds, correction failures and evaluator failures are reported explicitly.

`未检出严重幻觉` is limited to the number of finalized outputs in the current
run; it never becomes a permanent zero-hallucination claim. Missing judgments
are not described as model repair misses. An entirely failed run cannot earn
speed/quality praise. Repair and cleanup descriptions use ungated component
credits, so a severe hallucination is reported as such rather than incorrectly
attributed to poor punctuation or dictionary use. Evidence preserves full task
phrases, coverage, counts, credits and latency; console quotations are shortened
for readability. `run.assessment_version` versions the prose independently of
scoring rules.

## Probes, replay and rescoring

```sh
cargo run -- --probe-fidelity severe_hallucination_probes.json \
  --output runs/severe-probe.json
cargo run -- --probe-quality quality_probes.json \
  --output runs/quality-probe.json
```

Probe labels/score ranges stay outside requests. The severe set contains 34 pairs:
15 severe changes and 19 acceptable repairs, mild edits or retained ASR mistakes,
including two real outputs retaining formatted file errors. The six quality
anchors check dimension independence, formal rewriting and multiple mild edits.

`--probe-review-all` bypasses JEV and validates the secondary reviewer directly on
every fidelity pair. `--fidelity-rubric` replaces the probe's JEV question only;
it does not change normal bench scoring or the secondary review rubric.

Reuse outputs/timing and make fresh local correction judgments:

```sh
cargo run -- --replay runs/<original>/results.json --no-standings
```

Migrate old scores without new correction calls or changing saved local repair
credits; judge the new whole-text components:

```sh
cargo run -- --rescore-results runs/<original>/results.json --no-standings
```

On version-6 sources, unchanged whole-text questions/thresholds retain their saved
judgments; changed questions are rejudged. Prompt, dictionary, inputs, references,
models and available rounds must match. A reduced case set needs a matching
`--cases` file. Original runs stay unchanged.

Record a reviewed current-version result without repeating completed questions:

```sh
cargo run -- --finalize-results runs/<version-6-run>/results.json
```

Finalization validates the saved rubric and fidelity threshold, writes a new
artifact and records all models in standings. The original source identity is
retained across rescoring/finalization, so repeat recording replaces its entry
instead of awarding twice. Version 6 starts a separate season; old runs remain.

Verify numerical aggregation and preserved evidence offline:

```sh
uv run python scripts/verify_rescore.py ORIGINAL_RESULTS REVISED_RESULTS
```

## Verified on 2026-10-02

The final automatic JEV + GPT-6-luna high-reasoning path classified all 34 fidelity
pairs correctly, including a weak JEV pass on question-to-assertion that the
secondary reviewer rejected. All six readability/style anchors passed after
clarifying dimension independence. These are fixed calibration examples, not a
population-level accuracy guarantee. Initial probe failures remain in run files.

The saved 2026-10-01 run was rescored under version 6 with all 288 original
outputs/timings/tokens/errors and local base scores preserved (floating-point
round trips checked within 1e-12). All 16 models received ranks, with no pending
or evaluator errors; two original correction failures remain in the denominator.
The new score for GPT-6-luna is 87.7 and for GPT-5.6-luna is 87.1. Abaqus misses
cost GPT-6-luna 2.5 overall points; GPT-5.6-luna also loses quality for one confirmed
question-to-assertion change. Historical scoring-version results remain available.
