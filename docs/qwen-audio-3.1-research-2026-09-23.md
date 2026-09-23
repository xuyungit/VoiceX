# Qwen-Audio 3.1（TTS / ASR）调研（2026-09-23）

调研目标：百炼上新出的 `qwen-audio-3.1-*` 语音合成与语音识别模型能否替换应用里现在用的
3.0 版本（`qwen-audio-3.0-tts-flash`、`qwen-audio-3.0-asr-flash-streaming`、
`qwen-audio-3.0-asr-flash`），接入改动有多大，价格差多少。

来源：百炼文档（语音合成概述、实时/非实时语音合成、语音识别概述、实时语音识别、
客户端/服务端事件、模型调用价格、Qwen-Audio-TTS 音色列表）与模型广场，均为 2026-09-23 读取。

> **状态：已接入**（同日）。TTS 新增 `qwen-audio-3.1-tts-flash` 并设为默认模型；
> ASR 的模型列表新增 `qwen-audio-3.1-asr-flash-streaming`（实时）与 `qwen-audio-3.1-asr-flash`
> （Batch / 录后精修）。改动与验证见 §5。

**token 折算和成本数字是实测得出的**——文档只给了每百万 token 单价，没给这几个模型的
音频 token 折算率。实测用的是应用数据库里的百炼 Key（与
[`scripts/tts/aliyun_probe.py`](../scripts/tts/aliyun_probe.py) 同一账号），
先用 TTS 合成一段 11.36 s 的中文，再把同一段 PCM 喂给各个 ASR 模型。

---

## 1. 结论

- **两个模型都能直接接进来，协议不变。** 3.1 TTS 走的端点、请求体、参数名都和 3.0 相同
  （`/api/v1/services/audio/tts/SpeechSynthesizer`，`format` / `sample_rate` / `rate` 等）；
  3.1 实时 ASR 走同一个 `/api-ws/v1/inference` 的 run-task 协议。改动集中在模型名判断和
  音色表（见 §5）。
- **确实降价了，而且降得多。** 计费方式从「按字符 / 按秒」改成「按 token」，折算下来：
  - TTS：同一段文本，3.1 的花费约为 3.0 的 **16%**（便宜约 84%）。
  - 实时 ASR：**不一定更便宜**。每次请求有约 60 token 的固定开销，用户词典（热词 + 上下文）
    还要另算 token。不带词典时 10 秒一句约为 3.0 的 38%；带上当前 14 条词典，
    10 秒一句约为 55%，**短于约 4 秒的句子反而比 3.0 贵**（§3.2）。
  - 非实时（批量）ASR：约为 3.0 的 **10%**（便宜约 90%）。
- **需要留意的差异**：3.1 TTS 没有 3.0 那 500 多个「基础音色」，只有约 57 个 `_v3.1`
  系统音色；3.1 TTS 按输出音频时长计费，语速调慢会更贵（3.0 只按输入字符计费）；
  3.1 实时 ASR 文档说断句改用 `vad_model`，但实测 3.0 的 `max_sentence_silence` 对它仍然生效（§3.4）。

---

## 2. 模型与接入方式

### 2.1 TTS：`qwen-audio-3.1-tts-flash`

| | `qwen-audio-3.0-tts-flash`（现用） | `qwen-audio-3.1-tts-flash` |
|---|---|---|
| 上架 | 2026-07-14 | 2026-09-18 |
| 接入 | WebSocket / HTTP（同一模型名） | 同左 |
| HTTP 端点 | `/api/v1/services/audio/tts/SpeechSynthesizer` | 同左 |
| 旧主机 `dashscope.aliyuncs.com` | 可用 | **实测可用** |
| 指令控制 `instruction` | 支持 | 支持 |
| 情感 / 富语言标签（`[excited]`、`[laughing]` 等） | 支持 | 支持（仅单向流式） |
| 声音复刻 / 设计 | 支持 | 支持（复刻对带噪 / 混响参考音频更鲁棒） |
| 系统音色 | 12 个 + 500 余个基础音色 | 约 57 个，全部带 `_v3.1` 后缀，**无基础音色** |
| 计费 | 输入 1 元 / 万字符（汉字计 2） | 输入 1.5 元 / 百万 token，输出 12 元 / 百万 token |
| 免费额度 | 1 万字符 | 100 万 token |
| RPM | 180 | 180 |

3.1 的音色（`voice` 区分大小写）：

- 方言 4 个：`longanhuan_v3.1`（重庆话）、`longanlingxin_v3.1`（云南话）、
  `longanfengyue_v3.1`（东北话）、`xunanchuan_v3.1`（甘肃话）
- 中文 23 个：`yuxiaoyun_v3.1`、`anxiaolan_v3.1`、`xieshurou_v3.1`、`wenhuaizhi_v3.1`、
  `xuyanchu_v3.1`、`anmingyuan_v3.1`、`huozhuoshi_v3.1`、`andi_v3.1` 等
- 英文 15 个：`Emily_v3.1`、`Luna_v3.1`、`Eric_v3.1`、`Luca_v3.1`（英式），
  `Abby_v3.1`、`Ava_v3.1`、`Brian_v3.1`、`David_v3.1` 等（美式）
- 角色 15 个：`longanyang_v3.1`、`longanya_v3.1`、`libai_v3.1`、`longling_v3.1` 等

注意 `longanfengyue_v3.1` 在 3.1 里是**东北话**音色，而 3.0 的 `longanfengyue`
是应用当前的默认音色——默认值不能只加个后缀了事，要重新挑。

### 2.2 ASR：`qwen-audio-3.1-asr-flash-*` 一族

模型广场上的卡片写的是 `qwen-audio-3.1-asr-flash-filetrans`，但 3.1 实际有四个变体：

| 模型 | 模式 | 接入 | 单价（输入 / 输出，元 / 百万 token） | 对应的 3.0 |
|---|---|---|---|---|
| `qwen-audio-3.1-asr-flash-streaming` | 实时 | WebSocket `/api-ws/v1/inference` | 6 / 4.5 | `qwen-audio-3.0-asr-flash-streaming`（0.00033 元 / 秒） |
| `qwen-audio-3.1-asr-flash-message` | 实时（整句出） | 同上 | 6 / 4.5 | 无 |
| `qwen-audio-3.1-asr-flash` | 非实时，≤5 分钟 | HTTP multimodal-generation | 0.8 / 2.7 | `qwen-audio-3.0-asr-flash`（0.00022 元 / 秒） |
| `qwen-audio-3.1-asr-flash-filetrans` | 非实时，≤12 小时 | HTTP，文件 URL | 0.8 / 2.7 | `qwen-audio-3.0-asr-flash-filetrans`（0.00022 元 / 秒） |

免费额度：3.1 各 100 万 token（约合 20 小时音频）；3.0 各 10 小时。

实时 ASR（`-streaming`）3.0 → 3.1 的协议差异（客户端事件文档）：

- `vad_model`：3.1 新增，`near_meeting_16k`（近场）/ `far_field_meeting_16k`（远场，默认）。
  文档写明 3.1 的断句**只**通过它配置；`max_sentence_silence` 是 3.0 / Fun-ASR 的参数。
- `keep_dialect`：3.1 新增，默认 `false`（方言转写成普通话）。
- `input.context`、即时热词 `vocabulary`、`vocabulary_id`、`language_hints`（最多 4 个）
  三个版本都支持，写法不变。
- 服务端事件结构不变；`usage` 里除了 `duration` 多了 `input_tokens` / `output_tokens`。

非实时 `qwen-audio-3.1-asr-flash`：请求体与 3.0 相同，单文件上限从 10 MB 放宽到 2 GB
（时长仍是 5 分钟），并新增说话人分离。

语种 / 方言：文档给 3.1 列的方言是「上海、南昌、宁波、客家、杭州、温州、湖南、福建、粤语、
苏州」，比 3.0 那一长串（含西南、中原等官话口音）短。这可能只是文档写法不同，但四川话、
河南话等口音在 3.1 上的效果要实测确认，不能默认没有退化。

`qwen-audio-3.1-asr-flash-message` 值得单独一提：同一协议，额外支持
`disfluency_removal_enabled`（去语气词并润色）和 `intermediate_result_enabled`
（默认**不**返回中间结果）。它和应用现有的 LLM 纠错环节有重叠，可以作为后续选项评估，
这次不必接入。

---

## 3. 实测

### 3.1 token 折算

| 用量 | 5.68 s | 11.36 s | 22.72 s |
|---|---|---|---|
| TTS 3.1 `output_tokens` | — | 142 | — |
| 实时 ASR 3.1 `input_tokens` | 133 | 204 | 346 |
| 批量 ASR 3.1 `input_tokens` | 128 | 199 | 341 |

- **音频 = 12.5 token / 秒**，TTS 输出和 ASR 输入一样（142 / 11.36 = 12.5；
  ASR 的斜率 (346 − 204) / 11.36 = 12.5）。
- ASR 每次请求另有固定开销：实时约 62 token，批量约 57 token。
- ASR 输出：约 0.6 token / 汉字（56 个汉字 → 35 token）。
- TTS 输入：56 个汉字 + 标点 → 60 token（对应 3.0 计费口径的 113 字符）。

### 3.2 成本对比

| 场景 | 3.0 | 3.1 | 3.1 / 3.0 |
|---|---|---|---|
| TTS，113 计费字符 → 11.36 s 音频 | 0.0113 元 | 0.00179 元 | 16% |
| TTS，每小时朗读音频（中文） | ≈3.58 元 | ≈0.57 元 | 16% |
| 实时 ASR，3 s 一句 | 0.00099 元 | 0.00064 元 | 65% |
| 实时 ASR，10 s 一句 | 0.0033 元 | 0.00126 元 | 38% |
| 实时 ASR，连续 1 小时（不计每句开销） | 1.19 元 | ≈0.32 元 | 27% |
| 批量 ASR，10 s | 0.0022 元 | 0.00023 元 | 10% |

**用户词典的 token 开销（实时 ASR）**：用应用里现有的 14 条词典、6.48 s 音频实测，
不带词典 143 input tokens；只发即时热词 194；只发上下文 213；两者都发（应用的实际行为）237，
即每次请求多 94 token。按 6 元 / 百万 token 算，这 94 token 就是 0.00056 元，
相当于 3.0 的 1.7 秒音频。由此算出的盈亏平衡点：

| 实时 ASR 每句时长 | 3.0 | 3.1，不带词典 | 3.1，带 14 条词典 |
|---|---|---|---|
| 3 s | 0.00099 元 | 0.00064 元 | 0.00120 元（**比 3.0 贵**） |
| 10 s | 0.0033 元 | 0.00126 元 | 0.00182 元 |
| 盈亏平衡 | — | ≈1.5 s | ≈4 s |

词典越大，平衡点越靠后。批量 ASR 单价只有 0.8 元 / 百万 token，词典开销可以忽略。

按本机近 60 天的实际用量估算（`history_record` 表，`qwen-audio-3.0-asr-flash-streaming`
共 888 次，合计 28113 s，平均 31.7 s / 次，短于 4 s 的只有 47 次）：
- 3.0：约 9.3 元。
- 3.1：约 3.3 元，计入每次约 62 + 94 token 的请求与词典开销。
- 3.1 约为 3.0 的 36%。这里的听写以长句为主，所以词典开销影响不大。

3.1 TTS 按输出音频时长计费，所以语速调到 0.5 会让输出 token 大约翻倍；3.0 只看输入字符。
即便翻倍，也仍比 3.0 便宜。

### 3.3 延迟与识别结果（单次样本，只作量级参考）

| | 首包 / 尾延迟 |
|---|---|
| TTS 3.0 / 3.1 首包（HTTP SSE） | 282 ms / 324–332 ms |
| 实时 ASR 3.0 / 3.1，`finish-task` 之后到 `task-finished` | 277 ms / 313–322 ms |
| 实时 ASR 3.1 + `vad_model=near_meeting_16k` | 553 ms |
| 批量 ASR 3.0 / 3.1（11 s 音频） | 3852 ms / 669 ms |

三个实时模型对这段 TTS 音频的转写一字不差（含「嗯，那个，」和「3点」「10点」的数字规整）。
`-message` 默认输出**不带标点**；开了 `disfluency_removal_enabled` 之后，语气词被删掉，
标点也恢复了。

### 3.4 接入时补测的几项（2026-09-23 下午）

- **3.0 的 VAD 参数对 3.1 仍然生效**：文档说 3.1 只用 `vad_model`，但把
  `max_sentence_silence=200` 发给 3.1 后，同一段音频被切成两句（而且每多切一句，
  就多一次约 62 token 的开销）。所以应用继续照发 `semantic_punctuation_enabled` /
  `max_sentence_silence` / `heartbeat`，设置页的这几行对 3.1 同样有效。
- **`vad_model` 没有接入**：服务端不校验取值（`bogus_vad` 也照常返回），近场 / 远场各跑 3 次，
  尾延迟没有稳定差异（音频突然结束时 250–490 ms 对 380–415 ms；末尾留 1 s 静音时都在 15–20 ms）。
  没有证据支持加一个设置项。
- **批量 ASR 的长音频**：约 5 分钟、内容不重复的会议口述（295 s），3.1 完整转出
  1308 / 1316 字（差额来自数字规整），用时 7.9 s，3.0 用时 77 s。只有把同一句话
  逐字重复 26 遍拼成的病态音频，3.1 才会陷入循环，一直输出到 8192 token 的输出上限。
- **批量上限**：11.8 MB 的 data URI 3.0 和 3.1 都接受；应用现有的 10 MB 保护没有改动。
- **按应用实际请求重放**：`build_run_task_message` 产生的完整参数（language_hints、
  VAD 参数、heartbeat、即时热词、input.context）与批量请求体，经业务空间主机发给 3.1，
  实时和批量都返回正确文本，热词（VoiceX、连续刚构桥、挠度）全部命中。
- **TTS**：
  - 单次上限与 3.0 相同，是 20000 单位（汉字计 2）：10000 字接受，15000 字被拒。
  - 一段 3000 字的文本（混有数字）完整合成出 590 s 音频，没有 CosyVoice 那种静默截断。
  - 5 种采样率都按请求生效。
  - 41 个音色全部可用；3.0 的音色 id 发给 3.1 一律返回 `Engine error [411]`。
- **方括号**：`[sad]` 这类标签名会被吞掉、不念出来；`[1]`、`a[i]` 这类普通方括号照常朗读。
  选中文本里恰好出现标签名的情况很少，所以没有做转义。

TTS 合成的音频比真实麦克风干净得多，近场 / 远场 VAD 哪个适合桌面麦克风、
方言口音有没有退化，都要用真实录音评估。

---

## 4. 接入 VoiceX 需要改的地方（调研时的清单，落地见 §5）

TTS（[`src-tauri/src/tts/aliyun.rs`](../src-tauri/src/tts/aliyun.rs)）：

- 新增 `qwen-audio-3.1-tts-flash` 的 `ModelSpec`：端点、参数名与 3.0 的 spec 相同；
  音色表换成 `_v3.1` 那一套，不含基础音色。
- `piece_chars` / 单次文本上限：文档没写 3.1 的上限，3.0 用的 9000 要实测后才能沿用。
- `controller.rs` 里两处按 `MODEL_QWEN_AUDIO` 选音色设置槽位的分支，要给 3.1 单独的槽位，
  否则切换模型会把 3.0 的音色 id 带过去，直接 400。
- 前端 `settings.ts` 的 `aliyunTtsModel` 联合类型与默认值。
- 文本里的方括号：`[sad]`、`[laughing]` 这类片段会被当成情感标签，而不是照字面朗读。
  朗读用户选中的任意文本时这是一个新的行为差异（实测结论见 §3.4）。

实时 ASR（[`src-tauri/src/asr/funasr_client.rs`](../src-tauri/src/asr/funasr_client.rs)）：

- `qwen_uses_inference_protocol` 目前只认 `qwen-audio-3.0-asr-flash-streaming` 前缀；
  3.1 要走同一条路径。
- `model_supports_context` 加上 `qwen-audio-3.1-asr-flash-streaming`。
- ~~3.1 不发 `max_sentence_silence` / `semantic_punctuation_enabled`，改发 `vad_model`~~：
  实测这两个参数对 3.1 仍然生效，所以照发不改（§3.4）。

批量 ASR（[`src-tauri/src/asr/qwen_transcription_client.rs`](../src-tauri/src/asr/qwen_transcription_client.rs)）：

- `qwen_audio_flash_batch_model` 目前只认 `qwen-audio-3.0-asr-flash` 前缀，要把 3.1 也纳入。
- `models.rs` 校验、`AsrQwenSettings.vue` 的模型常量、i18n 里的模型标签。

---

## 5. 落地（2026-09-23）

TTS：

- [`aliyun.rs`](../src-tauri/src/tts/aliyun.rs)：
  - 新增 `MODEL_QWEN_AUDIO_31` 及其 `ModelSpec`：端点、参数写法与 3.0 相同，
    `max_chars` / `piece_chars` 都是 9000，采样率表也与 3.0 相同。
  - 音色表 `QWEN_AUDIO_31_VOICES` 收了 41 个，即全部非角色音色；默认 `anxiaolan_v3.1`。
  - `default_model()` 改为 3.1。
- 设置新增 `aliyun_tts_voice_qwen_audio31`（前端 `aliyunTtsVoiceQwenAudio31`）。
  [`controller.rs`](../src-tauri/src/tts/controller.rs) 的朗读音色与翻译朗读音色覆盖都按模型分派到它。
- 前端 `settings.ts` 默认模型改为 3.1；`ReadingSettings.vue` 增加模型选项与音色槽位。
- 已有用户的 `aliyunTtsModel` 保持不变，不做迁移。

ASR：

- `asrModels.json` 新增：
  - `qwen-audio-3.1-asr-flash-streaming`（realtime）
  - `qwen-audio-3.1-asr-flash`（batch）
  - `qwen-audio-3.1-asr-flash-filetrans`（unavailable）
- 两个判断函数由只认 3.0 改为同时认 3.0 / 3.1：
  - `qwen_uses_inference_protocol`
  - `qwen_audio_flash_batch_model`：同时排除同前缀的 `-streaming` / `-filetrans` / `-message`
- `model_supports_context` 加入 3.1 streaming。
- `AsrQwenSettings.vue` 的判断与后端保持一致；相关文案里的 “Qwen-Audio 3.0” 改为 “3.x”。
- 默认 ASR 模型不变。Qwen-Audio 系列需要业务空间 ID，默认值仍是免配置的 Qwen3-ASR。

验证：

- `cargo test --lib`：393 个通过；clippy 在改动行上没有新告警；`pnpm build` 通过。
- 线上测试 `aliyun::tests::voice_table_matches_what_the_service_accepts` 全部通过，
  3.1 的 41 个音色均有音频。
- ASR 按应用实际请求重放通过（§3.4）。
- 没有在应用里实际按热键听写或朗读。

复现脚本没有提交，调研和补测用的是临时 Python 脚本。数字如需复核，可以仿照
[`scripts/tts/aliyun_probe.py`](../scripts/tts/aliyun_probe.py) 的读 Key 方式重写。
