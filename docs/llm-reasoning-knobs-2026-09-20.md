# LLM reasoning knobs: the lowest setting per endpoint (2026-09-20)

VoiceX corrects and translates short dictated text. Thinking adds nothing to
that job and costs seconds, so every LLM request should ask for the lowest
reasoning the endpoint has. This note records what "lowest" is on each endpoint
we use, measured against the live APIs, and how the app and `tools/llm-bench`
apply it.

## Why this was looked at

In `tools/llm-bench`, "DeepSeek V4 Flash (Volc)" went from about 1.6 s per
correction to 9–12 s from 2026-09-18. The entry sent `reasoning_effort:
minimal`, the value that silences Doubao Seed models on the same host. The
Volc-hosted `deepseek-v4-1-flash-260910` ignores it and thinks as if no knob
were sent (375–1395 reasoning tokens per correction). There is no portable
"off": one value is the floor on one model, ignored by the next and an HTTP 400
on a third.

## Method

One request per variant, the bench's "Multiple errors" case with the bench
prompt and dictionary, non-streaming. Latency is measured to the full body.
Thinking is read from `usage.completion_tokens_details.reasoning_tokens` and
from the length of `message.reasoning_content`; "0" below means both were zero.
Single samples: the latencies show the order of magnitude, not a benchmark.

## Results

| Host / model | No knob | Lowest that works | Rejected or ignored |
|---|---|---|---|
| Ark `deepseek-v4-flash-ga-260731` | 14.6 s, 1630 tokens | `minimal`, `none`, `thinking: disabled` → 0 | |
| Ark `deepseek-v4-1-flash-260910` | 6–11 s | `none`, `thinking: disabled` → 0, about 2 s | `minimal` ignored |
| Ark `doubao-seed-2-0-mini`, `2-0-lite`, `2-1-pro`, `1-8-251228`, `doubao-seed-evolving` | 18.6 s, 2128 tokens (2.0 mini) | `minimal`, `none`, `thinking: disabled` → 0 | |
| Ark `doubao-seed-1-6-flash-250828` | | only `thinking: disabled` → 0, 2.0 s | `minimal` → 1229 tokens, 11.8 s; `none` → 679 tokens, 6.6 s |
| OpenAI `gpt-5.6-luna`, `gpt-5.4-mini` | 3.2 s, 164 tokens | `none` → 0 | `minimal` → 400 |
| OpenAI `gpt-5-mini` | | `minimal` → 0 | `none` → 400 |
| OpenAI `o4-mini` | | `low` (320 tokens) | `none` → 400 |
| OpenAI `gpt-4.1-mini` | 0 | send nothing | any `reasoning_effort` → 400 |
| api.deepseek.com `deepseek-v4-flash` | 10.6 s, 1998 tokens | `none`, `thinking: disabled`, or both → 0, about 1 s | |
| open.bigmodel.cn `glm-5.3-flash` | 6.9 s, 261 tokens | `reasoning_effort: low` → 0 | `thinking: disabled` → 400, "该模型始终思考" |
| DashScope `qwen3.8-flash` | 70.5 s, 1064 tokens | `enable_thinking: false`, 2.4 s | |
| Cerebras `qwen-3.8-27b` | 71 tokens | `none` → 0 | `low` → 83 tokens |
| Cerebras `gpt-oss-120b` | 90 tokens | `low` → 34 tokens | `none` → 400 (accepts low/medium/high) |
| Xiaomi MiMo `mimo-v2.5` | 7.3 s, 112 tokens | `thinking: disabled` → 0, 1.5 s; `none` → 0, 0.9 s | |

`thinking: disabled` means the body field `"thinking": {"type": "disabled"}`.

Gemini was not re-probed here. Its rules are the ones already in the app and
the bench: `thinkingBudget: 0` on 2.5 Flash, `thinkingLevel: LOW` on 2.5 Pro
and on 3.x Flash (which reject `MINIMAL`), nothing on Flash-Lite.

## Rules drawn from the table

- **Volcengine Ark**: `thinking: disabled`. It is the only spelling every Ark
  model honors; `minimal` and `none` are each ignored by at least one model.
- **OpenAI**: by model line. `gpt-5.1` and later take `none`; `gpt-5` takes
  `minimal`; o-series stops at `low`; `gpt-4.x` and earlier get no field,
  because any value is a 400.
- **DashScope (Qwen)**: `enable_thinking: false`.
- **api.deepseek.com, Xiaomi MiMo**: `thinking: disabled`.
- **open.bigmodel.cn (GLM)**: `reasoning_effort: low`.
- **Cerebras**: `reasoning_effort: none`, except `gpt-oss`, which stops at
  `low` on any host.
- **Gemini**: as above, inside `generation_config.thinkingConfig`.

## Where the rules live

- App: `src-tauri/src/llm/reasoning.rs`. "Lowest" is the default choice for
  every provider. A blank reasoning setting means Lowest; `server_default`
  sends no reasoning field; any other value is sent as `reasoning_effort`
  (`reasoning.effort` on the Responses API). Custom endpoints are recognized
  by API host and model. An unrecognized host gets no reasoning field and the
  LLM page says so, instead of guessing a value that might be a 400 or be
  silently ignored. The page shows the exact fields that will be sent
  (`preview_llm_reasoning`).
- Settings migration: Volcengine's old stored default `minimal` becomes blank
  (Lowest). Efforts a user chose (`low`, `medium`, `high`, or anything on an
  OpenAI or custom endpoint) are kept.
- Bench: `tools/llm-bench` entries name their knob explicitly
  (`reasoning_effort` and `[provider.extra]`). An entry of `type =
  "volcengine"` with no `reasoning_effort` sends `thinking: disabled`, and
  `type = "qwen"` sends `enable_thinking: false`, matching the app. The notes
  in `config.example.toml` list the floor for the other hosts.

## Adding a model

Send the bench case once with no knob and once with the candidate floor, and
compare `reasoning_tokens`. A 200 response does not show the knob worked:
`doubao-seed-1-6-flash` and `deepseek-v4-1-flash` both accept `minimal` and
keep thinking.
