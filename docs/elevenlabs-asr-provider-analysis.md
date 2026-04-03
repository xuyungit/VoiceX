# ElevenLabs ASR Provider 接入分析

最后更新：2026-04-03  
查阅范围：ElevenLabs 官方 Speech-to-Text 文档与当前 VoiceX 代码结构

## 状态标记

- 已完成：官方文档调研、与 VoiceX 现有架构的映射、接入方案分析
- 已实现但未验证：无
- 建议方案：采用同一 provider 同时支持 `realtime` 与 `batch` 两种模式，首发默认 `realtime`
- 待确认：Batch 高级能力在首版是否暴露到设置 UI，还是先只做底层能力与重转录支持

## 1. 官方接口与能力摘要

### 1.1 Batch Speech-to-Text

- 接口：`POST https://api.elevenlabs.io/v1/speech-to-text`
- 认证：`xi-api-key` header
- 模型：官方 API Reference 当前列出 `scribe_v2`、`scribe_v1`
- 输入：
  - `file` 上传文件，要求音频长度至少 100ms，文件大小小于 3GB
  - 也支持 `source_url`
  - `file_format=pcm_s16le_16` 时可走更低延迟路径；否则可直接传常见编码音视频文件
- 常用参数：
  - `language_code`
  - `timestamps_granularity`: `none | word | character`
  - `tag_audio_events`
  - `diarize`
  - `num_speakers`
  - `temperature`
  - `no_verbatim`，仅 `scribe_v2` 支持
- 高级参数：
  - `keyterms`，最多 1000 个；每个 keyterm 小于 50 字符，归一化后最多 5 个词；会产生额外成本
  - `entity_detection` / `entity_redaction`
  - `webhook` / `webhook_id` / `webhook_metadata`
- 多声道：
  - `use_multi_channel=true`
  - 最多 5 个声道
  - 多声道模式下必须 `diarize=false`
  - 每个声道默认映射为 `speaker_0` 到 `speaker_4`

### 1.2 Realtime Speech-to-Text

- 接口：`wss://api.elevenlabs.io/v1/speech-to-text/realtime`
- 认证：
  - 服务端可直接用 `xi-api-key`
  - 客户端网页场景可用 `token` query 参数；官方要求通过 single-use token 避免暴露 API key
- 模型：官方当前文档使用 `scribe_v2_realtime`
- 查询参数：
  - `model_id`
  - `include_timestamps`
  - `include_language_detection`
  - `audio_format`
  - `language_code`
  - `commit_strategy`: `manual | vad`
  - `vad_silence_threshold_secs`
  - `vad_threshold`
  - `min_speech_duration_ms`
  - `min_silence_duration_ms`
- 发送事件：
  - `input_audio_chunk`
  - 示例里可携带 `audio_base_64`、`sample_rate`、`commit`、`previous_text`
- 接收事件：
  - `session_started`
  - `partial_transcript`
  - `committed_transcript`
  - `committed_transcript_with_timestamps`
- 官方建议：
  - 麦克风场景优先使用 VAD commit strategy
  - 建议 16kHz PCM
  - 仅支持 mono
  - 每块音频建议 0.1 到 1 秒

### 1.3 Realtime 的几个关键限制

- 官方文档说明，Realtime 在发送第一段音频后大约 2 秒才开始产出转写
- 手动 commit 建议每 20 到 30 秒一次；默认 90 秒自动 commit
- `previous_text` 只能在第一段音频发送时提供，之后再发会报错
- 官方建议 `previous_text` 最好控制在 50 个字符以内
- 实时事件错误类型里明确包括：
  - `auth_error`
  - `quota_exceeded`
  - `rate_limited`
  - `queue_overflow`
  - `resource_exhausted`
  - `session_time_limit_exceeded`
  - `chunk_size_exceeded`
  - `insufficient_audio_activity`
  - `commit_throttled`

## 2. 与 VoiceX 现有架构的映射

### 2.1 当前项目的关键结构

- VoiceX 已经不是“一个 provider 只对应一种模式”的设计
- 现有先例：
  - `OpenAI` 同时支持 `batch` 与 `realtime`
  - `Gemini` / `Cohere` 是 batch-only
  - `Gemini Live` / `Qwen` / `Soniox` 是 realtime-only
- `src-tauri/src/asr/config.rs` 里的 `is_batch()` 会影响：
  - HUD 展示模式
  - 录音结束后是否走批量识别
  - 重转录路径
- `src-tauri/src/services/asr_manager.rs` 只负责流式 provider
- `src-tauri/src/session/handlers/asr.rs` 负责 batch provider 的录后识别
- `src-tauri/src/commands/retranscribe.rs` 已支持：
  - 对 batch provider 直接传文件
  - 对 realtime provider 回放 PCM 做离线重转录

### 2.2 当前音频管线对 ElevenLabs 的影响

- 当前录音落盘是单声道 Ogg/Opus 文件
- 当前实时采集发给 ASR 的也是 mono PCM chunk
- 这意味着：
  - ElevenLabs Realtime 可以直接复用现有“实时 PCM 推流”架构
  - ElevenLabs Batch 初版可以直接上传当前录下来的 Ogg/Opus 文件，不需要为了“可用”额外转码

这部分来自仓库代码，不是 ElevenLabs 官方文档：

- 录音文件写入 `recording-*.opus`，并使用 `Channels::Mono`
- 实时采集注释写明发出的是 mono PCM i16 chunk

### 2.3 ElevenLabs 和 VoiceX 需求的匹配度

适合 VoiceX 的点：

- Realtime 有低延迟 partial / committed transcript，适合 HUD 实时显示
- Batch 有完整文件识别，适合录后识别、provider probe、历史重转录
- 同一个厂商同时提供两条能力线，和现有 OpenAI provider 结构一致

需要特别注意的点：

- ElevenLabs Realtime 没有文档化的热词 / keyterms 参数
- ElevenLabs Batch 才有 `keyterms`
- 所以如果我们把 ElevenLabs 做成“只接 realtime”，那 VoiceX 词典能力在这个 provider 上会明显变弱

这是基于官方文档做出的推断，不是官方原文承诺：

- 对 VoiceX 这种桌面听写产品，Realtime 更像“主路径”
- Batch 更像“稳定录后识别 + 词典增强 + 历史重转录路径”

## 3. 模式选择分析

### 3.1 方案 A：只接 Realtime

优点：

- 最符合 VoiceX 主工作流，录音中就能出字
- HUD 体验完整
- 可以直接复用现有 realtime provider 架构

缺点：

- 无法使用 Batch 的 `keyterms`
- 无法使用 `no_verbatim`
- 无法使用 `entity_detection` / `entity_redaction`
- 无法使用多声道转录
- 历史重转录只能走“离线回放成实时流”，不是官方 batch 主路径

结论：

- 能上线
- 但对 VoiceX 来说不够完整，尤其是词典/热词能力会出现模式不对称

### 3.2 方案 B：只接 Batch

优点：

- 实现简单
- 能吃到 `keyterms`
- 适合 provider probe、历史重转录、长音频文件

缺点：

- 录音中没有实时文字
- HUD 会退化成纯波形 + 录后等待
- 对 VoiceX 的核心听写体验不够好

结论：

- 不建议作为首发唯一形态

### 3.3 方案 C：同一 provider 同时支持 Realtime + Batch

优点：

- 与当前 OpenAI provider 的产品模型一致
- 对用户更容易理解
- 可以把 ElevenLabs 的两条官方能力线都利用起来
- Realtime 负责主听写体验
- Batch 负责录后识别、provider probe、历史重转录，以及更强的词典偏置

缺点：

- 设置项会比只做一种模式更多
- 需要明确哪些配置跨模式共用，哪些配置只对 batch 生效

结论：

- 这是最符合 VoiceX 当前产品与代码结构的方案

### 3.4 Realtime 能否独立承担“实时 + 后修”

先说结论：

- 可以承担“实时显示 + 局部回改未提交文本”
- 不能从官方文档中得出“它等价于火山那种公开的流式后二次精修通道”
- 如果我们要做“录音结束后整段再精修一次”，官方能力上更可靠的做法仍然是额外跑 Batch

依据：

- 官方把 Realtime 输出明确分成 `partial transcripts` 和 `committed transcripts`
- 官方说明 `committed transcripts` 是“the final results of the transcription segment”
- 官方又说明 commit 之后会 “clear the processed accumulated transcript and start a new segment”

这意味着：

- 在 commit 之前，partial 本身就是 interim 结果，允许被后续音频继续修正
- 一旦某段被 commit，官方表述把它定义成该 segment 的最终结果，而不是“后面还会继续二次修”

还有两个值得注意的信号：

- Realtime API 的 `session_started.config` 示例里出现了 `max_tokens_to_recompute: 5`
- 官方营销页提到 `predictive transcription`

这是基于官方文档的推断，不是官方明确承诺：

- ElevenLabs Realtime 内部确实存在一个“为了低延迟而保留短窗口回改”的机制
- 这个机制更像 partial 阶段的局部重算或预测修正
- 不能把它理解成“录音完成后，再自动给你跑一轮整段精修”

对 VoiceX 的直接含义：

- HUD 显示：可以安全依赖 partial 做实时展示，依赖 committed 作为当前段 final
- 录音结束后如果我们只保留 realtime 结果，产品上是成立的
- 但如果我们想要“更像二阶段精修”的效果，仍然应该显式增加一个 Batch 后处理步骤，而不是假设 realtime 自带这个能力

## 4. 建议方案

### 4.1 Phase 1

建议首发目标：

- 新增 `ElevenLabs` provider
- 同一 provider 支持 `realtime` 与 `batch` 模式
- 默认模式设为 `realtime`
- 不做“录音时 realtime，结束后再自动跑 batch 二次修正”的隐式双跑

原因：

- 隐式双跑会带来额外成本、额外等待和结果切换
- 用户也不一定希望一个 provider 在一次听写里偷偷走两套 API
- 按仓库约定，不应靠“防御性 fallback”或隐式兜底掩盖问题

### 4.2 Realtime 首版建议

- 运行位置：
  - 走 Rust 后端 WebSocket 连接
  - 直接使用 `xi-api-key`
- 不建议在当前桌面版里接 single-use token 方案

这是基于官方文档和当前架构做出的推断：

- 官方 single-use token 主要解决浏览器前端不能暴露 API key 的问题
- VoiceX 是 Tauri 桌面应用，密钥可以保存在后端设置里，接法更接近官方“server-side streaming”

首版参数建议：

- `model_id=scribe_v2_realtime`
- `audio_format=pcm_16000`
- `commit_strategy=vad`
- `vad_silence_threshold_secs=1.5`
- `vad_threshold=0.4`
- `min_speech_duration_ms=100`
- `min_silence_duration_ms=100`
- `include_timestamps=false`
- `include_language_detection=false`
- `language_code` 可选

Realtime 结果语义建议：

- HUD 中的滚动文本显示 partial
- 写入当前稳定结果时使用 committed transcript
- 不要期待 committed 之后 ElevenLabs 还会返回同一段的更高质量修订版

首版不建议暴露的项：

- `previous_text`
- 手动 commit
- enable_logging / zero retention

原因：

- `previous_text` 只能首包发送，而且官方建议最好小于 50 字符；它不像通用“历史上下文”那样可自由持续追加
- 手动 commit 不适合当前 VoiceX 麦克风听写主场景
- zero retention 只对 enterprise 可用，不适合先做成默认承诺

### 4.3 Batch 首版建议

建议支持这些能力：

- 文件上传识别
- `model_id=scribe_v2`
- `language_code`
- `timestamps_granularity=word`
- `keyterms`

建议暂不在首版 UI 暴露，但底层可预留：

- `tag_audio_events`
- `no_verbatim`
- `entity_detection`
- `entity_redaction`
- `webhook`
- `source_url`
- `use_multi_channel`

原因：

- 这些都是真能力，但不一定是 VoiceX 听写产品的首要设置项
- 先做太多会把设置页变复杂

如果要做“实时识别后再精修”的产品形态，建议这样理解：

- `realtime only`
  - 成本和链路更简单
  - 适合默认听写
- `realtime + batch refine`
  - 这是额外的一条官方支持链路
  - 适合做成可选增强模式，而不是默认隐式双跑

### 4.4 词典映射建议

建议：

- ElevenLabs Batch：把应用词典映射到 `keyterms`
- ElevenLabs Realtime：首版明确标注“不支持词典热词增强”

不建议做法：

- 不要把整份词典硬塞进 `previous_text`
- 不要把 Realtime 的上下文提示包装成“等效热词支持”

原因：

- 官方对 `previous_text` 有首包与长度限制
- 官方文档没有把它定义成 keyterm/hotword biasing 机制
- 如果我们把它说成“支持词典”，容易误导

## 5. 对下一步开发的具体指导

### 5.1 建议新增的设置字段

- `elevenlabsApiKey`
- `elevenlabsMode`: `batch | realtime`
- `elevenlabsBatchModel`
- `elevenlabsRealtimeModel`
- `elevenlabsLanguage`
- `elevenlabsEnableKeyterms`

可选后续字段：

- `elevenlabsTagAudioEvents`
- `elevenlabsNoVerbatim`
- `elevenlabsIncludeTimestamps`
- `elevenlabsEnableLanguageDetection`

说明：

- Batch 与 Realtime 模型 ID 不同，建议分开存，不要强行共用一个 model 字段
- UI 可以像现有 OpenAI provider 一样先暴露 mode，再按 mode 显示对应说明

### 5.2 建议新增的代码入口

前端：

- `src/utils/providerOptions.ts`
- `src/stores/settings.ts`
- `src/views/AsrSettings.vue`
- `src/components/asr/AsrElevenLabsSettings.vue`
- `src/i18n/locales/en-US.ts`
- `src/i18n/locales/zh-CN.ts`

Rust 后端：

- `src-tauri/src/asr/config.rs`
- `src-tauri/src/asr/mod.rs`
- `src-tauri/src/asr/elevenlabs_client.rs`
- `src-tauri/src/asr/elevenlabs_realtime_client.rs`
- `src-tauri/src/services/asr_manager.rs`
- `src-tauri/src/session/handlers/asr.rs`
- `src-tauri/src/commands/retranscribe.rs`
- `src-tauri/src/commands/settings.rs`
- `src-tauri/src/services/history_service.rs`

### 5.3 Realtime 客户端实现建议

- 复用现有 realtime provider 的形态：
  - 建立 WebSocket
  - 读取 `session_started`
  - 持续发送 base64 PCM chunk
  - 把 `partial_transcript` 映射成非 final 事件
  - 把 `committed_transcript` 映射成 final 事件
- 音频预处理：
  - downmix 到 mono
  - resample 到 16kHz
  - chunk 长度保持 100ms 到 200ms 之间即可

### 5.4 Batch 客户端实现建议

- 直接 multipart 上传当前录音文件
- 当前 VoiceX 已经把录音存成单声道 Ogg/Opus，可先按 `audio/ogg` 上传
- provider probe 与历史重转录优先复用 batch 客户端

这是建议，不是现状：

- 如果后面要追求更低 batch 首包延迟，再评估是否补一条 `pcm_s16le_16` 上传路径

## 6. 风险与注意事项

### 6.1 不要做的事情

- 不要把 realtime 失败时自动无提示切到 batch
- 不要把 batch 的 keyterms 能力包装成 realtime 也支持
- 不要在文案里写成“ElevenLabs 已支持词典热词”，除非明确限定是 batch 模式

### 6.2 需要重点验证的点

- Realtime 断线/报错时：
  - HUD 是否关闭或回到正确状态
  - 录音是否停止并释放设备
  - 失败后能否继续切换 provider 或立即重试
- Realtime commit 与 HUD final 文本的映射是否稳定
- Batch 模式下 provider probe 与历史重转录是否正常
- Ogg/Opus 直接上传是否稳定，是否需要个别情况下转码

## 7. 最终结论

结论：

- ElevenLabs 不适合只做 batch，也不适合只做 realtime
- 对 VoiceX 最自然的接法，是做成和 OpenAI 类似的“单一 provider，显式选择 `realtime` / `batch` 模式”
- 产品默认值建议是 `realtime`
- Batch 不应该被当成失败兜底，而应该是显式模式与重转录能力
- 如果只做 Phase 1，优先级建议是：
  1. Realtime 听写主链路
  2. Batch 文件转录与 provider probe
  3. Batch `keyterms` 与历史重转录

## 参考链接

- [Speech to Text quickstart](https://elevenlabs.io/docs/eleven-api/guides/cookbooks/speech-to-text)
- [Client-side streaming](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/client-side-streaming)
- [Server-side streaming](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/server-side-streaming)
- [Transcripts and commit strategies](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/transcripts-and-commit-strategies)
- [Realtime event reference](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/event-reference)
- [Multichannel speech-to-text](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/batch/multichannel-transcription)
- [Create transcript API reference](https://elevenlabs.io/docs/api-reference/speech-to-text/convert)
- [Realtime STT API reference](https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime)
