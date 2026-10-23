# LLM correction benchmark

The default is a **short CLI benchmark of every configured model**: three short
cases, one round, actual outputs and times in the terminal, then the current ranking and rolling standings.
Copy `config.example.toml` to `config.toml`, fill in provider credentials and
uncomment one `[judge]`. Copy `quick_cases.example.toml` to `quick_cases.toml`
and replace the illustrative examples with your own dictations.

```sh
cargo run -- --dry-run                # inputs, all models and call counts; no API calls
cargo run                            # all [[provider]] entries × three short cases × 1 round
cargo run -- --provider "model A" --provider "model B"
cargo run -- --provider "model A" --case "典型纠错" --rounds 3
cargo run -- --speed-only             # explicit speed ranking, without quality judgments
```

The final ranking sorts by **basic passes**, then **successful calls**, then
**median request time** (descending, descending, ascending). Exactly equal keys
share a place. Each whole answer receives one `pass / needs_revision / uncertain`
judgment from the configured judge. `focus` states the basic requirement;
`expected` illustrates an acceptable correction, not a unique wording or a
human-validated label. The rubric checks the whole meaning, basic cleanup and
necessary repairs; it allows equivalent wording and does not fact-check names
against outside knowledge. The terminal shows the inputs, outputs and each
judgment, so the ranking can be inspected directly. The content-preservation case
checks unauthorized name/version substitution, not the position of negation:
equivalent wording and completing an omitted speech verb are acceptable when
the rejected and intended entities remain the same. Replacing an intended name
with a more familiar one fails the basic check, regardless of fluency or speed.

Identical judgments are deduplicated across models/rounds. One request per
distinct output, concurrency 8 by default (`eval.typesafe_concurrency`), a 15 s
timeout, no retries, no edit questions and no secondary reviewer. Confidence
below 0.7 is a visible review hint; it does not rewrite the decision or affect rank.
This threshold is not a calibrated correctness guarantee. A model with any
explicitly uncertain/failed judgment is listed
as **pending**, without a numerical place. Failed/empty correction calls count as
failures, retain their elapsed times, and cannot earn a basic pass. They are not
sent to the judge. Successful-call medians/ranges exclude those failed calls.

Each run saves the same terminal output to `report.log`, plus outputs, timings,
judge rubric/verdicts and ranking in `results.json`. No HTML is generated.
Quick mode updates `quick-standings.json` and prints a **rolling leaderboard** after
the current ranking. It reuses the previous points/decay calculation: Borda points
(N..1, tied positions share their points), recent 10 runs, decay 0.9 by default.
`eval.standing_scheme`, `standing_window`, `standing_decay`, and
`standing_retire_after` retain their meanings. The rolling table shows recent
weighted-average points, change from the preceding cumulative table, and this
run's place/basic passes. `--no-standings` disables history reads/writes and the
rolling table; `--standings <path>` selects a different quick history file.

Saved quick rankings under `--log-dir` are imported automatically without model
calls; duplicate views/copies of one run do not earn extra points. Unchanged files
are cached so large legacy reports are not reparsed each time. Cases (including
focus), prompt/dictionary, rounds, model roster/settings recorded in the result,
judge/rubric/reported version and ranking policy must match within a season.
Different seasons remain in history and are reported separately. The old full
benchmark's `standings.json` is preserved separately. A run with pending judgments
does not earn standings points: missing evaluation is not treated as model failure.
Changing the case/rubric can therefore start a season with only one comparable run.

Show a saved run and its cumulative standings up to that run, without API calls:

```sh
cargo run -- --standings-results runs/<run>/results.json
```

This reads standing settings from the config; it does not load credentials or
rewrite the saved result. Each live run also embeds its cumulative-table snapshot
in `results.json`. `--no-log` skips the per-run directory; combine it with
`--no-standings` to also avoid writing rolling history.
Private cases, credentials and run artifacts are ignored by Git.

Use one everyday cleanup, one typical repair, and one correct-content preservation
case. These few samples provide an initial comparison, not a general capability
leaderboard. Add rounds/cases when needed. Quick mode defaults to one round even
if the legacy root `rounds` is larger; use `--rounds` to repeat it. Every configured
provider runs by default; `--provider` narrows the list. The former `quick_providers`
shortlist is no longer used and can be removed from old configs.

## Full evaluation and targeted regression checks

The previous evaluation pipeline is available with **`--full`**. It uses
`test_cases.toml`, the config's `rounds` (default 3), all providers unless filtered,
and the old judges, score formulas and standings. Its rankings are legacy
diagnostics, not a validated measure of personal preference. Existing cases and
saved runs are retained.

You can also run a difficult case through quick mode with the same basic check:

```sh
cargo run -- --cases test_cases.toml --case "case name" --provider "model A"
cargo run -- --full --no-standings     # all legacy checks, without adding a season entry
cargo run -- --full --case "case name" --provider "model A" --no-standings
```

`--replay`, `--rescore-results`, `--finalize-results`, `--probe-fidelity` and
`--probe-quality` explicitly select the legacy evaluation pipeline too. They
keep their existing behavior. `--quick` conflicts with those evaluation actions.
`--skip-judge` remains a legacy full-mode flag; it is not how quick mode works.

Verify quick mode without contacting any model service (Rust unit tests plus a
local HTTP fixture that checks the real CLI, call counts, failures, empty outputs,
selection, saved artifacts, rolling history and preserved full-mode standings):

```sh
cargo test
cargo build
uv run python scripts/verify_quick.py
```

## The app's prompt and dictionary

`prompt_source = "app"` (a root key in `config.toml`) sends the correction prompt
and hotword dictionary the installed app is using, read from its database
(`VOICEX_DB`, or `voicex.db` in the app data directory), and builds the system
prompt exactly as the app does for a dictation with history context off: one
term per line, duplicates dropped, `（空）` for an empty dictionary,
`{{INPUT_HISTORY}}` as `（已关闭）`. The config's `prompt` and `dictionary` are then
not used. Dictionary sites in the cases follow the same dictionary, so cases
written for the app's dictionary need this setting. `results.json` keeps the
prompt, the dictionary and `run.prompt_source`; a replay refuses a run whose
prompt or dictionary has since changed in the app.

Check what a case file will be scored on before spending calls:

```sh
cargo run -- --full --check-cases
```

It prints every case's correction tasks (dictionary or semantic, repeats shared)
and cleanup sites under the dictionary in use, then stops. A removal meant as
cleanup that appears as a semantic task is a word the filler list and the stutter
rule do not cover; leave it as heard in `expected` if it should not be scored.

## Parallel runs and local feedback

Correction calls now run across independent services, with six provider workers
by default. Each provider's cases and rounds stay serial, and providers with the
same model at the same URL origin take turns. Different models of one platform
run side by side: Ark and Gemini limit requests per account and model, so they do
not share a quota. Join providers that do share one with
`concurrency_group = "service-name"` inside their `[[provider]]` entries. Queue
time is excluded from request latency; saved case/provider/round ordering remains
unchanged.

In full mode, the slowest provider of the newest saved run under `--log-dir` starts first, and
a provider that run did not have starts before it. The stage lasts at least as
long as its slowest provider, so that one should not start last. On the
2026-10-10 run this ordering and per-model groups take the simulated correction
stage from 149 s to about 109 s. `run.execution.start_order` records which run
the order came from.

```sh
cargo run -- --full --concurrency 6
cargo run -- --full --concurrency 1 --no-standings
```

The root `concurrency` config key controls the same limit. `[eval]`
`typesafe_concurrency` continues to control the initial judge (default 8);
`reviewer_concurrency` controls deduplicated secondary reviews (default 4).
Progress prints during all three stages, and `run.execution` saves stage wall
times and the scheduling policy. Replay preserves recorded request times and
records its execution mode separately. Parallel wall-time gains are measured
per run, not guaranteed; endpoints and network conditions can affect latency.

The explicit `--feedback-results` export creates **`feedback.html`**, a standalone
local page with embedded results. Normal CLI runs do not generate it. `src/feedback.html` is the
source template and contains no cases. The generated page shows
the raw input and output. Mark **可直接用 / 需要修改 / 明显改错**, optionally add a
note or preferred wording, and export the feedback JSON (copy text or download;
embedded previews may not support downloads). Import accepts pasted JSON or a
local file. Model identity, latency
and the reference wording are hidden until opened; exports record whether they
were viewed. Identical outputs for the same case share one decision, retaining
the identities of every underlying call. Failed calls remain visible.

Drafts are saved in browser local storage; export JSON for durable storage or
moving between browsers. Import checks the result-set identity before merging
feedback. No server or network connection is used, and human labels do not alter
automatic scores or standings. Keep the original HTML/results alongside the
feedback: exported labels link to those outputs by dataset/item ID, without
revealing hidden model names or references in the export panel. The page contains
test transcripts and notes may contain private excerpts; keep private runs local.
The page includes only selected result fields, never provider credentials, raw
HTTP error bodies or the configuration.

Build the page from an existing run without any configuration or API calls:

```sh
cargo run -- --feedback-results runs/<run>/results.json
# Optional destination for the HTML page:
cargo run -- --feedback-results runs/<run>/results.json --output /path/to/review.html
```

The legacy edit questions remain diagnostic. Human feedback is collected
before choosing a new rubric, sample distribution or repair-cost model.

## Scoring version 10

The whole-transcript questions about one output (severe hallucination,
readability, style) now share one state, identifier hints included, and go to
the judge in one request, as TypeSafe recommends for questions over the same
state; the Decisions judge gets them in one request too. Judging the
2026-10-11 outputs twice, this cut judge input tokens from 3.72M to 2.58M and
judge time from 127 s to 83 s; severe verdicts stayed the same within the usual
rejudging noise (16 flips between the two, 17 before). Readability and style now
also see the identifier hints, so their answers can differ slightly from version 9.

Splitting the severe check into one yes/no question per kind of change, the
other half of that guidance, was tried on the same outputs and not adopted: it
sent more outputs to the reviewer, whose verdicts on borderline outputs are the
least stable part, and doubled the verdicts that flip when the same outputs are
judged twice (17 → 36).

## Scoring version 9

Version 9 stops scoring a contextual repair site as missed only because the
misheard word is gone. A model may rephrase around it: for 「项目的进度已经严重之后了」
(之后 → 滞后), 「项目的进度已经严重落后了」 means what the speaker meant without the
word. Such a site now goes to the judge with the whole sentences as written,
like any other rewritten site; a deletion that loses the meaning still earns
nothing. On the 2026-10-11 real-dictation cases this decided 2–5 sites a run.

Version 8 changes version 6 twice. A confirmed severe hallucination now costs
three rounds of quality instead of one (`SEVERE_EXTRA_ROUNDS` in
`src/scoring.rs`). On the 2026-10-11 cases, Gemini 3.8 Flash turned the
correctly heard GPT 6.1 and Luna into GPT-4o and Llama in every round, as it had
in real use. It still placed first, because one zeroed round cost about as much
as a somewhat weaker repair rate. A silent change of meaning costs the user far
more than a missed repair, so it weighs three times as much.

Version 7, which version 8 includes, removed the transcript gate. That judge question
(`transcript` / `incomplete` / `extended` / `not_transcript`) voided a round's
correction and filler credits when it answered `not_transcript`. On the
2026-10-11 real-dictation cases, where the app's prompt asks for polished text,
it gave that answer to ordinary paraphrases at about even odds: 44 of 90 outputs
for one model, 34 of 89 for another. Models answering the speaker instead of
correcting were never seen in real use. An output that changes, adds or drops
substance is still caught by the severe-hallucination check below.

Every case is worth 100 points. Average rounds within a case, then average cases
with equal weight; failed calls remain in both denominators.

| Component | Points | Rule |
| --- | ---: | --- |
| Correction | 30 | Average fixed repair tasks derived from input/reference. Dictionary and contextual repairs use the same task budget. Repeated occurrences of the same repair share one task; occurrence credits average within it. |
| Fidelity | 40 | Preserve substantive entities, numbers, versions, facts and central intent. Keeping an ASR mistake introduces no hallucination, but earns no correction credit. |
| Filler/stutter removal | 10 | Average removal credit at reference-derived cleanup sites. |
| Readability | 5 | One whole-text judge question about punctuation, grammar and sentence boundaries. |
| Conversational style | 5 | One whole-text judge question about voice and organization. Mild edits are assessed together. |
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
at default weights) and costs two more rounds of quality on top, so the round
scores 10 × speed − 180; its measured speed stays independent. The Correction,
Fidelity and Cleanup columns stay shares; only Composite and Ability carry the
extra cost. Failed correction
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

Two columns show how far a place can be trusted. They report 90% of 2000
redraws of the run's rounds. **Rerun** keeps the cases and redraws each model's
rounds: where the model lands if the run is repeated. **Case mix** also redraws
the cases, the same draw for every model: where it lands on a similar set of
cases. Overlapping ranges are places the run cannot tell apart. A wide Case mix
range means the place rests on a few cases: on 2026-10-10 GPT-6-luna was 1–4 on
a rerun but 1–15 on the case mix, because the late-qualifier case alone lifted it
(that case, a disputed sentence most models mishandled, was withdrawn the same day).
Both are reporting only and are saved as `place_90_rerun` / `place_90_case_mix`.
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

The judge's `changed`, `uncertain`, missing judgments and `faithful` judgments below
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

### Choosing the judge

`[judge] type` selects who answers these questions. TypeSafe JEV is the default;
the Decisions judge is experimental until it matches JEV on the probes below.

| `type` | Service | Endpoint from `base_url` |
|---|---|---|
| `"typesafe"` | TypeSafe System One (JEV), e.g. `jev-1.13.0` | `…/v1/systemone` |
| `"openai"` | OpenAI Decisions API, `gpt-6-luna` | `…/v1/decisions` |

Both ask the same questions files. For Decisions, each TypeSafe question is
translated: `{task, question}` becomes the instructions, a `score` criterion
becomes a level labelled by its index, a `choice` criterion becomes a choice, and
`examples`/`not_for` stay in the description. The state goes in as the same JSON
TypeSafe receives. A question field with no Decisions counterpart stops the run at
setup instead of being dropped. Answers come back in TypeSafe's shape and pass the
same completeness checks; a refusal is a judge failure. `typesafe_questions` and
`typesafe_concurrency` apply to either judge. `results.json` records
`judge.model`, `judge.backend` and `judge.reported_models`: the versions the
service says answered (`jev-latest` currently answers as `jev-1.13.0`). More
than one version in a run means it was judged by mixed versions.

To compare judges on identical outputs, keep the saved outputs and let the other
judge answer everything: `--replay runs/<run>/results.json --no-standings`.
`--rescore-results` and `--finalize-results` keep the saved run's correction
credits, so they refuse a judge model other than the one that judged the run.
Standings record each run's judge, and every judge has its own season: a
Decisions run never counts in the TypeSafe standings, nor replaces the TypeSafe
entry of the same source. Runs recorded before this count as TypeSafe, which
judged all of them.

On 2026-10-10 the current rubric, written and calibrated for JEV, gave these results on the same probes:

| Probe set | JEV (`jev-latest`) | Decisions (`gpt-6-luna`) |
|---|---|---|
| 34 severe pairs, judge alone | 33 correct | 29 correct |
| 34 severe pairs, after secondary review | 34 correct, 16 reviewed | 32 correct, 19 reviewed |
| 6 readability/style anchors | 6 pass | 4 pass |

Decisions passed a document-identity swap (confidence 0.73) and a question
turned into an assertion (0.80) as `faithful`, above the 0.7 review threshold. It
also rated the formal-rewrite anchor's readability 0.435, although the rubric
says formal register is not a readability defect. Its 12 quality questions
finished in 1.4 s. Results are in `runs/severe-probe-decisions.json` and
`runs/quality-probe-decisions.json`.

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

`--probe-review-all` bypasses the judge and validates the secondary reviewer directly on
every fidelity pair. `--fidelity-rubric` replaces the probe's judge question only;
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

On current-version sources, unchanged whole-text questions/thresholds retain their
saved judgments; changed questions are rejudged. A source with credits the transcript
gate voided (scoring 6 and earlier) is refused: replay it instead. Prompt, dictionary, inputs, references,
models and available rounds must match. A reduced case set needs a matching
`--cases` file. Original runs stay unchanged.

Record a reviewed current-version result without repeating completed questions:

```sh
cargo run -- --finalize-results runs/<current-version-run>/results.json
```

Finalization validates the saved rubric and fidelity threshold, writes a new
artifact and records all models in standings. The original source identity is
retained across rescoring/finalization, so repeat recording replaces its entry
instead of awarding twice. Version 6 starts a separate season; old runs remain.
A season is one scoring version under one judge (`typesafe`, `openai-decisions`)
on one case set. The case set is known by a fingerprint of every case's input,
reference and pins (`run.cases_fingerprint`); names and order do not count.
Changing the cases starts a new season, as on 2026-10-10 when the disputed
late-qualifier case was withdrawn. Runs recorded before the fingerprint was kept
count in no new season. Rescoring a saved run with the current `--cases` file
records it in the new season without correction calls.

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
