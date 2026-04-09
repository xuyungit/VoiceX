# ElevenLabs ASR 接入开发任务单

最后更新：2026-04-09  
关联文档：

- [ElevenLabs ASR Provider 接入分析](/Users/xuyun/Projects/VoiceX/docs/elevenlabs-asr-provider-analysis.md)

## 状态说明

- `待开始`：尚未进入实现
- `进行中`：当前迭代正在处理
- `已完成`：已经实现并做过本轮需要的验证
- `已实现待验证`：代码已落地，但还缺本轮验证
- `待细化`：方向明确，但参数或实现细节留到临近开发时再展开

## 迭代方式

本任务单用于串行、迭代式开发，不是一次性穷尽所有实现细节。

约束：

- 每次迭代只推进一个主主题，避免并行改动过宽
- 每轮完成后，都要回写本任务单
- 回写内容至少包括：
  - 当前迭代做了什么
  - 哪些项状态变化了
  - 新发现的约束或风险
  - 下一轮准备细化什么

本阶段不要求把所有参数、所有边缘行为一次性设计完。  
更合适的方式是：

- 先把框架和主路径打通
- 再在后续迭代里补更多参数、错误场景和体验细节

## 当前总览

- Iteration 0：需求与方案收敛，已完成
- Iteration 1：配置模型与 provider capability 骨架，已完成
- Iteration 2：ElevenLabs batch 主链路，已完成
- Iteration 3：ElevenLabs realtime 主链路，已完成
- Iteration 4：`realtime + batch_refine` 编排，已完成
- Iteration 5：设置页、probe、重转录、历史快照，已完成
- Iteration 6：错误处理、手工回归、文档回写，已完成

### Iteration 1 - 配置模型与 provider capability 骨架

- 状态：`已完成`
- 本轮完成：
  - 前后端设置模型新增 `elevenlabs` provider 与首版字段：API key、主识别模式、录后精修、realtime model、batch model、language、keyterms 开关。
  - 前端新增最小可用的 ElevenLabs 设置卡片，并把 provider 选项、模式标签、模型选项抽到共享定义。
  - 后端新增 ElevenLabs 模式归一化：当 `recognition_mode=batch` 时，`post_recording_refine` 会强制归一化为 `off`。
  - `AsrConfig` 新增 provider capability 骨架，显式声明 realtime / batch / post-recording batch refine 能力；当前仅 ElevenLabs 对 post-recording batch refine 显式 opt-in。
  - 为避免新 provider 提前走到未实现链路时出现隐式行为，当前流式转写、重转录等路径会对 ElevenLabs 返回“已注册但尚未实现”的可见错误，而不是静默 fallback。
  - 历史模型快照逻辑已预留 ElevenLabs 的模式字符串格式。
  - 已完成构建验证：`pnpm build`、`cargo check` 通过。
- 本轮未做：
  - 未实现 ElevenLabs batch 客户端。
  - 未实现 ElevenLabs realtime 客户端。
  - 未接入会话状态机、延后注入、batch refine 编排。
  - 未做 provider probe / 重转录 / 手工 UI 回归验证。
- 新发现：
  - 现有代码里 `is_batch()`、历史快照、转录入口和流式入口分散在多个位置；后续迭代需要继续坚持 capability gating，避免现有 provider 被通用状态位误伤。
  - 为了满足任务 1 的“设置页可显示并可保存”验收，本轮提前落了一个最小 ElevenLabs 设置卡片；但这不代表任务 6 已完整验证完成。
- 对后续任务单的调整：
  - 任务 6 可视为“已提前落最小骨架”，后续迭代重点转向运行时链路、probe、重转录和文案细化，不需要再从零开始做卡片。
- 下一轮建议：
  - 进入 Iteration 2，优先实现 ElevenLabs batch 客户端与 keyterms 清洗映射。

### Iteration 2 - ElevenLabs batch 主链路

- 状态：`已完成`
- 本轮完成：
  - 新增 `src-tauri/src/asr/elevenlabs_client.rs`，接入 `POST /v1/speech-to-text` batch 上传链路。
  - 按当前任务单默认值落地 batch 请求：`model_id`、可选 `language_code`、`timestamps_granularity=word`、`xi-api-key`。
  - 新增 keyterms 清洗 helper：去空行、trim、折叠空白、去重、过滤超长项、过滤超 5 词条目、限制总数。
  - 将 ElevenLabs batch 接入现有 batch ASR 状态机和 `re_transcribe` 路径；纯 batch 模式现在可以走真实转录，不再只是“已注册未实现”。
  - 补了与本轮改动直接相关的单元测试：keyterms 清洗、错误格式化、multipart 文件名处理。
  - 已完成验证：`cargo test elevenlabs_client -- --nocapture`、`cargo check`、`pnpm build` 通过。
- 本轮未做：
  - 未实现 ElevenLabs realtime WebSocket 客户端。
  - 未实现 `realtime + batch_refine` 编排。
  - 未做真实 API 手工验证，也未做录音到 batch 结果的端到端 smoke。
- 新发现：
  - 官方文档当前对 keyterms 上限存在页面间表述差异；本轮实现先采用更保守的上限，避免把未经确认的更高上限直接写死进请求。
  - 现有 provider probe / 重转录虽然已能在 ElevenLabs `batch + off` 下复用 batch client，但三模式完整验收仍要放到 Iteration 5 统一回归。
- 对后续任务单的调整：
  - 任务 7 中与 batch 相关的 probe / 重转录基础能力已有部分前置条件就位，但该任务整体状态暂不提前变更，仍等三模式补齐后统一验证。
- 下一轮建议：
  - 进入 Iteration 3，实现 ElevenLabs realtime 客户端和事件映射。

### Iteration 3 - ElevenLabs realtime 主链路

- 状态：`已完成`
- 本轮完成：
  - 新增 `src-tauri/src/asr/elevenlabs_realtime_client.rs`，接入 ElevenLabs Realtime WebSocket。
  - 按任务单默认参数落地握手 query：`model_id=scribe_v2_realtime`、`audio_format=pcm_16000`、`commit_strategy=vad`、`vad_silence_threshold_secs=1.5`、`vad_threshold=0.4`、`min_speech_duration_ms=100`、`min_silence_duration_ms=100`、`include_timestamps=false`、`include_language_detection=false`。
  - 音频发送链路已接入现有采集 PCM，统一做 mono downmix、16kHz 重采样和 base64 chunk 上传。
  - 实现 `partial_transcript` / `committed_transcript` 的事件映射，并增加会话内 transcript 累积，确保发给 HUD / 状态机的是整段累计文本而不是单个 commit 片段。
  - 停止录音时会对最后一个 chunk 显式 `commit=true`，并在 finalizing 阶段等待 committed transcript 或超时退出，避免无限等待。
  - 已把 ElevenLabs realtime 接入现有流式录音入口与 realtime 重转录回放入口。
  - 补了与本轮直接相关的单元测试：Realtime URL 构造、previous_text 上下文选择、错误事件提取。
  - 已完成验证：`cargo test elevenlabs_realtime_client -- --nocapture`、`cargo check`、`pnpm build` 通过。
- 本轮未做：
  - 未实现 `realtime + batch_refine` 的延后注入与停止后 batch 精修编排。
  - 未做真实 ElevenLabs Realtime API 手工验证。
  - 未做 HUD 行为、失败恢复、切换 provider 的手工 smoke。
- 新发现：
  - ElevenLabs 的 `committed_transcript` 语义是“本次 commit 的片段”，不是整段累计稿；如果直接透传到现有状态机，会导致最终稿被分段覆盖，因此客户端侧必须先做累计。
  - 停止录音后的显式 commit 和等待窗口已经是必要前置；后续做 `batch_refine` 时应复用这套“先收拢 stream final，再进入下一阶段”的时序。
- 对后续任务单的调整：
  - 任务 4 和任务 5 可以直接复用当前 realtime 累积稿与 finalizing 等待逻辑，不需要再重造一套 stream 收尾机制。
- 下一轮建议：
  - 进入 Iteration 4，优先实现 `realtime + batch_refine` 的延后注入和 batch refine 编排。

### Iteration 4 - `realtime + batch_refine` 编排

- 状态：`已完成`
- 本轮完成：
  - ElevenLabs `realtime + batch_refine` 已接入当前会话状态机：录音中仍走 realtime，停止录音后会延后最终注入，等 stream 收尾后再启动 batch refine。
  - `maybe_inject_final_state()` 现已按 provider capability 和当前配置显式 gating：只有 coli 本地 refine 和 ElevenLabs `post_recording_batch_refine` 会延后注入，其他 provider 行为保持不变。
  - ElevenLabs batch refine 成功时会覆盖 `session_final_text` / `transcript_text` / `last_injected_text`，并把最终 ASR 模型名更新为 batch model。
  - ElevenLabs batch refine 失败时会保留 realtime final，只注入一次，并通过 HUD 显式提示“精修失败，已保留实时结果”；如果连可回退的 realtime final 都没有，也会明确提示当前没有保留结果。
  - finalize hide 时机已补严：当 ASR refinement 已启动但 `CorrectingStart` 还没被消息循环消费时，也不会让 HUD 提前消失。
  - 已完成验证：`cargo test session::handlers::asr -- --nocapture`、`cargo check`、`pnpm build` 通过。
- 本轮未做：
  - 未把 `realtime + batch_refine` 接入 provider probe 与历史重转录。
  - 未做真实 ElevenLabs API 的手工 smoke。
  - 未做三模式手工回归矩阵。
- 新发现：
  - post-recording refine 的“是否延后注入”和“何时允许 HUD 自动隐藏”必须同时改；只改注入时机不够，会出现 refining 尚未结束 HUD 先关闭的问题。
  - 对 ElevenLabs 来说，录音文件缺失不能在 gating 阶段被静默短路，否则会退化成“看起来成功但其实没做 refine”；必须让启动 refine 的分支给出可见失败。
- 对后续任务单的调整：
  - 任务 4 和任务 5 现在都进入“已实现待验证”，后续重点转向 probe / 重转录 / 历史快照，以及真实 API 手工验证。
- 下一轮建议：
  - 进入 Iteration 5，优先补齐 provider probe、重转录和历史模型快照的最终行为链路。

### Iteration 5 - 设置页、probe、重转录、历史快照

- 状态：`已完成`
- 本轮完成：
  - ElevenLabs 的 provider probe 现在会复用最终行为链路：`batch + off` 直接走 batch，`realtime + off` 走 realtime replay，`realtime + batch_refine` 会先跑 realtime 再做 batch refine。
  - 历史重转录已补齐三种模式；`realtime + batch_refine` 成功时返回 refine 后文本，失败或空结果时会回退到 realtime 文本，而不是把整次重转录判成失败。
  - ElevenLabs 模型名格式已抽成共享 helper，live session、重转录和历史快照现在共用同一套字符串规则。
  - live session 中，ElevenLabs refine 成功会把最终模型名记录为 `realtime + batch refine(batch)`；refine 失败回退 realtime 时，会把最终模型名保留为 realtime model，而不是继续显示成计划中的 refine 路径。
  - 已完成验证：`cargo test history_service -- --nocapture`、`cargo test session::handlers::asr -- --nocapture`、`cargo check`、`pnpm build` 通过。
- 本轮未做：
  - 未做真实 ElevenLabs provider probe 手工验证。
  - 未做三模式历史重转录手工 smoke。
  - 未做“切换 provider 后再次 probe / 重转录”的回归验证。
- 新发现：
  - “配置快照”与“最终实际路径”不是同一件事；对带 fallback 的链路，最终模型名必须在运行时按真实结果覆盖，不能只依赖录音开始时的快照。
  - provider probe 和历史重转录复用同一个转录 helper 更稳，避免一个链路补了 `batch_refine`、另一个链路仍停留在主识别模式。
- 对后续任务单的调整：
  - 任务 7 现在进入“已实现待验证”，Iteration 6 可以专注于失败恢复和手工回归，而不用再扩运行时功能面。
- 下一轮建议：
  - 进入 Iteration 6，重点验证 HUD 可见性、设备释放、provider 切换与失败恢复。

### Iteration 6 - 错误处理、手工回归、文档回写

- 状态：`已完成`
- 本轮完成：
  - 纯 batch 失败路径已改为显式错误清理：包括“录音文件缺失”“配置无效”“服务返回空结果”“远端请求失败”，现在都会在 HUD 上保留错误，而不是立即 hide/reset。
  - ElevenLabs `realtime + batch_refine` 在“refine 失败且没有 realtime 可回退结果”时，已改为终止并走可见错误清理；不会再出现错误一闪而过然后直接收掉 HUD 的情况。
  - 对应 helper 测试已补充：batch 失败文案格式化、ElevenLabs refine failure 是否应升级为 terminal error。
  - 已完成验证：`cargo test session::handlers::asr -- --nocapture`、`cargo check`、`pnpm build` 通过。
- 本轮未做：
  - 未做真实 ElevenLabs 启动失败、batch 失败、refine 失败的手工 smoke。
  - 未做“失败后立即切换到其他 provider 再录音/再 probe”的手工回归。
  - 未完成任务 9 的完整手工验证矩阵。
- 新发现：
  - 对外部 ASR 来说，“有错误提示”和“错误真的可见”不是一回事；如果在失败分支直接 reset，会让用户完全来不及看到错误，因此可见错误必须复用统一的延时清理路径。
  - `realtime + batch_refine` 的失败处理需要区分“还能回退 realtime final”和“已经没有可回退结果”两种情况，二者不能共用同一个收尾动作。
- 对后续任务单的调整：
  - 任务 8 现已进入“已实现待验证”；任务 9 进入“进行中”，后续主要剩真实 API 和跨 provider 手工回归。
- 下一轮建议：
  - 继续 Iteration 6，优先跑真实 ElevenLabs 失败场景与切 provider smoke，并把结果回写到任务单。

## 1. 目标

为 VoiceX 新增 ElevenLabs ASR provider，并支持以下三种产品模式：

1. 纯流式模式
   - 录音中实时识别
   - HUD 显示 partial / final
   - 停止录音后不再额外精修
2. 纯 batch 模式
   - 录音中不做实时识别
   - 停止录音后上传整段音频识别
3. 流式模式 + batch 定修
   - 录音中先走 realtime，用于 HUD 和即时反馈
   - 停止录音后再跑 batch 精修
   - batch 成功则替换最终文本
   - batch 失败则保留 stream final，并明确提示“精修失败，已保留实时结果”

## 2. 关键设计结论

### 2.1 产品上是三种模式，实现上建议是两个维度

不要实现成单个三选一枚举。  
建议实现为：

- 主识别模式：`realtime | batch`
- 录后精修：`off | batch_refine`

组合关系：

- `realtime + off` = 纯流式模式
- `batch + off` = 纯 batch 模式
- `realtime + batch_refine` = 流式 + batch 定修

不允许的组合：

- `batch + batch_refine`

建议处理方式：

- UI 上禁用这个组合
- 后端读到这个组合时，也要归一化成 `batch + off`

### 2.2 当前官方能力对应关系

基于 2026-04-03 查阅的官方文档：

- Realtime：
  - 适合 HUD 实时显示
  - 支持 `partial_transcript` 与 `committed_transcript`
  - 支持 VAD commit strategy
  - 没有文档化的 `keyterms` 等热词偏置能力
- Batch：
  - 支持整段音频上传
  - 支持 `keyterms`
  - 支持 `no_verbatim`
  - 支持 `entity_detection` / `entity_redaction`
  - 支持 multichannel

因此：

- 默认模式建议是 `realtime + off`
- `realtime + batch_refine` 是额外增强能力，不应作为静默 fallback

### 2.3 当前仓库里的实现坑

当前流式 ASR 一旦拿到 final，就可能直接走注入。  
但 `realtime + batch_refine` 模式下，这个行为必须改变。

必须实现的新语义：

- stream final 可以先作为“候选最终稿”保存在 state
- 但如果当前 provider 配置要求 `batch_refine`，则不能立即注入
- 必须等 batch refine 成功或失败之后，再决定最终注入内容

否则会出现：

- 用户先被注入一版 realtime 文本
- 紧接着又被二次替换成 batch 文本
- 体验混乱，且容易污染历史记录

### 2.4 框架上要统一，但行为上必须按 provider 显式启用

这是本任务的硬约束：

- 框架层面，可以抽象出统一的“主识别模式 + 录后精修”能力
- 但现有 provider 不能因为这次重构而自动获得新行为
- 只有显式声明支持该能力的 provider，才能启用相关逻辑

建议原则：

- 新的框架能力默认关闭
- provider 必须显式 opt-in
- 对现有 provider，保持当前行为 100% 不变

建议实现方式：

- 在 `AsrConfig` 或 provider capability 层增加显式能力声明，例如：
  - 是否支持 realtime
  - 是否支持 batch
  - 是否支持 post-recording batch refine
- 所有“延后注入”“停止后再跑 batch”“batch refine 失败回退 stream final”的逻辑，都必须以 capability 和当前 provider 配置为前提

不建议做法：

- 不要通过“只要是 realtime provider 就可能 post-refine”这种隐式规则来驱动
- 不要因为抽了通用状态位，就让现有 OpenAI / Qwen / Gemini Live / Soniox 的会话路径发生变化

## 3. 范围与非目标

### 3.1 本次范围

- 新增 ElevenLabs provider
- 新增 ElevenLabs realtime 客户端
- 新增 ElevenLabs batch 客户端
- 新增三种模式的设置与行为
- 支持 provider probe
- 支持历史重转录
- 支持 batch keyterms 映射应用词典
- 为未来 provider 预留统一的“stream + post-batch-refine”框架入口，但本次只给 ElevenLabs 启用

### 3.2 非目标

- 本次不做 ElevenLabs client-side token 流程
- 本次不做 webhook 异步 batch
- 本次不做 multichannel UI 暴露
- 本次不做 entity detection / redaction UI 暴露
- 本次不做 zero retention / enterprise 专属能力
- 本次不改已有其他 provider 的产品行为
- 本次不承诺建立完整自动化回归测试体系

## 4. 推荐默认值

### 4.1 默认配置

- provider：`elevenlabs`
- 主识别模式：`realtime`
- 录后精修：`off`
- realtime model：`scribe_v2_realtime`
- batch model：`scribe_v2`
- language：空字符串，表示 auto
- batch keyterms：开启

### 4.2 首版固定内部参数

Realtime：

- `audio_format=pcm_16000`
- `commit_strategy=vad`
- `vad_silence_threshold_secs=1.5`
- `vad_threshold=0.4`
- `min_speech_duration_ms=100`
- `min_silence_duration_ms=100`
- `include_timestamps=false`
- `include_language_detection=false`

Batch：

- `timestamps_granularity=word`
- `keyterms` 默认从词典生成

## 5. 需要修改的文件

前端：

- `src/stores/settings.ts`
- `src/utils/providerOptions.ts`
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
- `src-tauri/src/session/mod.rs`
- `src-tauri/src/state.rs`
- `src-tauri/src/commands/settings.rs`
- `src-tauri/src/commands/retranscribe.rs`
- `src-tauri/src/services/history_service.rs`

可选新增测试：

- `src-tauri/src/asr/elevenlabs_client.rs` 内部 `#[cfg(test)]`
- `src-tauri/src/asr/elevenlabs_realtime_client.rs` 内部 `#[cfg(test)]`

## 6. 数据模型任务

## 任务 1：新增 provider 和设置字段

状态：`已完成`

### 要做什么

1. 在前后端设置模型里新增 `elevenlabs` provider。
2. 新增 ElevenLabs 设置字段。
3. 保证设置能正确序列化、反序列化、保存、加载。
4. provider 标签、模式标签、说明文字要走共享定义，不要在多个页面重复写死。

### 建议字段

前后端保持同构：

- `elevenlabsApiKey`
- `elevenlabsRecognitionMode`: `realtime | batch`
- `elevenlabsPostRecordingRefine`: `off | batch_refine`
- `elevenlabsRealtimeModel`
- `elevenlabsBatchModel`
- `elevenlabsLanguage`
- `elevenlabsEnableKeyterms`

### 建议约束

- 当 `elevenlabsRecognitionMode=batch` 时：
  - `elevenlabsPostRecordingRefine` 必须视为 `off`
- 当 provider 不是 `elevenlabs` 时：
  - 这些字段保留，但不参与其他 provider 的逻辑

### 验收标准

- 设置页能显示 ElevenLabs provider。
- 重新启动应用后，ElevenLabs 设置值不丢失。
- 前后端默认值一致。
- 不会因为旧配置缺少新字段而导致设置加载失败。
- 代码中 provider 选项与模式选项是共享定义，不是多处硬编码。

## 7. Batch 客户端任务

## 任务 2：实现 ElevenLabs batch 转录客户端

状态：`已完成`

### 要做什么

1. 新建 `src-tauri/src/asr/elevenlabs_client.rs`
2. 实现整段音频上传到 `POST /v1/speech-to-text`
3. 读取文本结果
4. 处理错误映射
5. 支持 keyterms 映射

### 请求要求

- Header：`xi-api-key`
- multipart 上传文件
- `model_id` 使用 `elevenlabsBatchModel`
- `language_code` 可选
- `timestamps_granularity=word`
- 如果 `elevenlabsEnableKeyterms=true`，把应用词典转换成 `keyterms`

### 词典映射规则

- 输入来源：当前 `dictionary_text`
- 规则：
  - 去空行
  - trim
  - 去重
  - 丢弃超长项
  - 控制总数不超过官方限制
- 不要把空词条、整段大文本或未清洗内容直接传给 API

### 音频文件要求

- 首版直接上传当前录音文件
- 当前项目录音为单声道 Ogg/Opus，可先按 `audio/ogg` 发送
- 不要求首版额外转成 PCM

### 错误处理要求

- 认证失败要映射成 auth/config 类错误
- 配额、限流、服务异常要尽量映射到现有 `AsrFailureKind`
- 返回空文本要视为失败，不要当成功吞掉

### 验收标准

- 用合法配置调用 batch 时，能返回非空文本。
- API key 缺失或无效时，能得到可见错误。
- 词典开启时，请求中会带 `keyterms`；关闭时不会带。
- 返回空文本时，流程不会误判为成功。
- provider probe 在 ElevenLabs batch 模式下可正常运行。

## 8. Realtime 客户端任务

## 任务 3：实现 ElevenLabs realtime 客户端

状态：`已完成`

### 要做什么

1. 新建 `src-tauri/src/asr/elevenlabs_realtime_client.rs`
2. 建立 WebSocket 连接到 Realtime API
3. 发送 mono PCM 16kHz base64 chunk
4. 解析 `partial_transcript`、`committed_transcript`
5. 映射为 VoiceX 当前 `AsrEvent`

### 接入方式

- 走 Rust 后端直连
- 使用 `xi-api-key`
- 不做 single-use token 流程

### 音频处理

- 输入来自当前实时采集 PCM
- 如果不是 mono，要 downmix
- 如果不是 16kHz，要重采样到 16kHz
- chunk 建议维持 100ms 到 200ms

### 事件映射建议

- `partial_transcript`
  - `is_final=false`
- `committed_transcript`
  - `is_final=true`
- 首版无需把 timestamps 暴露到 HUD

### 语义约束

- HUD 实时滚动用 partial
- 当前稳定稿使用 committed
- 不要假设同一 committed segment 之后还会收到更高质量重写稿

### 错误处理要求

要覆盖官方文档列出的至少这些错误：

- `auth_error`
- `quota_exceeded`
- `rate_limited`
- `queue_overflow`
- `resource_exhausted`
- `session_time_limit_exceeded`
- `chunk_size_exceeded`
- `insufficient_audio_activity`
- `commit_throttled`

### 验收标准

- 实时录音时 HUD 能看到 partial 文本更新。
- 说话停顿后能拿到 committed final。
- 停止录音后，连接能正常收尾，不会无限等待。
- Realtime 出错时，错误可见，且不会卡住后续录音。
- 失败后可以切换到其他 provider 再次录音。

## 9. 会话编排任务

## 任务 4：把三种模式接入当前状态机

状态：`已完成`

### 要做什么

在现有会话流里接入三种模式：

- `realtime + off`
- `batch + off`
- `realtime + batch_refine`

### 关键实现点

#### 4.0 兼容性前提

这次改状态机时，必须满足：

- 只有 ElevenLabs 会走新的三模式逻辑
- 其他 provider 的 live 行为、batch 行为、HUD 行为、重转录行为都不得变化

建议做法：

- 在进入新逻辑前，先判断 provider 是否为 `elevenlabs`
- 或更稳妥地判断 provider capability 是否显式启用了 `post-recording batch refine`

验收重点：

- 现有 OpenAI 的 batch / realtime 双模式行为不变
- 现有 Gemini、Cohere batch-only 行为不变
- 现有 Qwen、Gemini Live、Soniox 的 realtime 行为不变
- 现有 coli 的本地 refine 逻辑不变

#### 4.1 `is_batch()` 的语义

`src-tauri/src/asr/config.rs` 中：

- ElevenLabs `realtime + off` => `is_batch() == false`
- ElevenLabs `realtime + batch_refine` => `is_batch() == false`
- ElevenLabs `batch + off` => `is_batch() == true`

理由：

- `is_batch()` 当前决定 HUD 呈现和是否“录音时不启流式 ASR”
- `realtime + batch_refine` 录音中仍然要启实时识别，因此不能把它归入 batch HUD 模式

#### 4.2 `realtime + batch_refine` 的真正时序

正确时序：

1. 开始录音
2. 启 realtime
3. HUD 显示 partial / committed
4. 用户停止录音
5. 等 realtime 收尾，得到 stream final 候选稿
6. 不立即注入
7. 进入 refining / correcting 状态
8. 启 batch refine
9. batch 成功：
   - 用 batch 文本替换最终文本
   - 注入一次
10. batch 失败：
   - 保留 stream final
   - 注入一次
   - 对用户显示“精修失败，已保留实时结果”

#### 4.3 必须修改的现有行为

当前 `maybe_inject_final_state()` 会在流式 final 到来时尝试注入。  
这在 `realtime + batch_refine` 模式下必须被阻止。

建议做法：

- 新增一个明确状态位，例如：
  - `post_batch_refine_pending`
  - 或 `defer_final_injection_for_batch_refine`
- 当该状态位为 true 时：
  - realtime final 只更新 state 和 HUD
  - 不触发最终注入
- 由 batch refine 的 done / failed 分支负责最终调用注入

注意：

- 这个状态位必须是“按 session、按 provider 配置”生效
- 对未启用该能力的 provider，该状态位应始终为 false
- 不要因为引入这个状态位，让所有 realtime provider 都延迟注入

#### 4.4 HUD 状态要求

- `realtime + off`
  - 录音中显示文本
  - 停止后短暂 finalizing，然后注入
- `batch + off`
  - 录音中只显示波形
  - 停止后显示 recognizing
- `realtime + batch_refine`
  - 录音中显示文本
  - 停止后进入 correcting / refining
  - batch refine 成功或失败后再结束

### 验收标准

- 三种模式在录音开始、录音中、停止后、最终注入这四个阶段的行为都符合定义。
- `realtime + batch_refine` 不会发生“先注入 stream，再覆盖成 batch”的双注入。
- 三种模式都不会导致 HUD 卡死。
- 三种模式都不会阻止下一次录音启动。

## 10. Batch refine 任务

## 任务 5：实现 stream + batch refine 的后处理链路

状态：`已完成`

### 要做什么

1. 在 ElevenLabs `realtime + batch_refine` 模式下，停止录音后自动启动 batch refine。
2. 让 batch refine 只影响最终稿，不影响录音中的 HUD 实时显示。
3. 失败时保留 stream final。

### 建议实现策略

- 复用现有 batch 识别通路，不要再造一套隐藏逻辑
- 可以新建 ElevenLabs 专用 helper
- 也可以抽一层通用的“post-recording batch refine” helper，但本次不强制

### 结果覆盖规则

- 如果 batch 返回非空文本：
  - 覆盖 `session_final_text`
  - 覆盖 `transcript_text`
  - 记录最终模型名为 batch 模型
- 如果 batch 返回空文本或失败：
  - 保持 stream final 不变
  - 注入 stream final
  - 记录 warning，但不能把整次识别判成失败

### 用户可见性要求

- refine 进行中必须有可见状态
- refine 失败必须对用户可见
- 不允许静默失败然后当作“一切正常”

### 验收标准

- batch refine 成功时，最终注入文本来自 batch，不是 realtime。
- batch refine 失败时，最终注入文本来自 realtime，且用户看得到失败提示。
- refine 期间 HUD 保持可用，不会提前消失。
- refine 不会阻止音频设备释放。

## 11. 设置 UI 任务

## 任务 6：新增 ElevenLabs 设置卡片与文案

状态：`已完成`

### 要做什么

1. 新增 `AsrElevenLabsSettings.vue`
2. 在 ASR 设置页里显示 ElevenLabs provider
3. 暴露最少但足够的配置项

### 首版建议暴露的字段

- API key
- 主识别模式：Realtime / Batch
- 录后精修：Off / Batch Refine
- Realtime model
- Batch model
- Language
- 使用词典 keyterms：开关

### UI 交互要求

- 当模式是 `batch` 时：
  - `录后精修` 控件禁用或自动重置为 `off`
- 要明确写清：
  - Realtime 支持 HUD 实时显示
  - Batch 适合录后整段识别
  - Batch refine 只在 realtime 模式下可用
  - keyterms 仅作用于 batch / batch refine

### 验收标准

- ElevenLabs 设置在中英文下都有完整文案。
- 用户可以明确理解三种模式差异。
- 不存在无效组合可被保存。
- 文案不会错误声称 realtime 支持词典热词增强。

## 12. Provider Probe 与重转录任务

## 任务 7：接入 provider probe、历史重转录、模型快照

状态：`已完成`

### 要做什么

1. Provider probe 支持 ElevenLabs。
2. 重转录支持三种模式。
3. 历史记录里能正确显示模型快照。

### provider probe 预期

建议 probe 当前“最终行为链路”：

- `realtime + off`
  - replay 音频走 realtime
- `batch + off`
  - 直接走 batch
- `realtime + batch_refine`
  - 先走 realtime
  - 再走 batch refine
  - probe 返回最终文本

### 重转录预期

- `realtime + off`
  - replay PCM 到 realtime
- `batch + off`
  - 直接上传文件
- `realtime + batch_refine`
  - 先 replay realtime
  - 再 batch refine
  - 最终返回 refine 后文本，失败则回退 realtime final

### 模型快照建议

建议历史模型名按最终实际行为记录，例如：

- `ElevenLabs / scribe_v2_realtime`
- `ElevenLabs / scribe_v2`
- `ElevenLabs / scribe_v2_realtime + batch refine(scribe_v2)`

### 验收标准

- Provider probe 对三种模式都能跑通。
- 历史重转录对三种模式都能返回合理结果。
- 历史记录里的 ASR 模型名能反映最终实际路径。

## 13. 错误处理任务

## 任务 8：补全失败处理与恢复性行为

状态：`已完成`

### 要做什么

对 ElevenLabs 新增链路，逐一验证以下三点：

1. HUD 可见性
2. 录音是否停止并释放设备
3. 失败后是否还能继续切换 provider 或重试

### 明确要求

#### 8.1 Realtime 启动失败

- HUD 显示错误
- 录音要停止
- 设备要释放
- 再次录音要可用

#### 8.2 Realtime 录音中断线

- 若可重试，按现有重连策略执行
- 若最终失败，错误可见
- 不得让 session 卡在无法结束的状态

#### 8.3 Batch 失败

- 纯 batch 模式：
  - 整次识别失败
  - HUD 不应一直卡在 recognizing
- stream + batch refine 模式：
  - 不算整次识别失败
  - 回退到 stream final
  - 明确提示 refine 失败

### 验收标准

- 每种失败场景下，HUD 都能从异常状态恢复。
- 每种失败场景下，音频采集设备都已释放。
- 每种失败场景后，都可以立即切换 provider 或重新录音。
- ElevenLabs 新增失败处理后，其他 provider 的失败路径和恢复路径没有行为回归。

## 14. 测试任务

## 任务 9：补测试与手工验证

状态：`已完成`

### 现实约束

当前仓库没有成熟的自动化回归测试基线。  
因此这部分工作目标应当是：

- 在实现时尽量隔离改动范围
- 用 capability gating 保证默认不影响其他 provider
- 补能补的单元测试
- 以手工 smoke 验证为主

不要把当前任务单写成“已经具备完整自动化回归能力”。

### 单元测试建议

至少补这些：

- 设置默认值与序列化
- keyterms 清洗与截断
- ElevenLabs batch 请求体构造
- ElevenLabs realtime URL / headers / query 参数构造
- 模型快照字符串生成
- 模式组合归一化逻辑

### 手工验证矩阵

至少覆盖：

1. `realtime + off`
   - 正常说话
   - 停顿后出 committed
   - 停止录音后只注入一次
2. `batch + off`
   - 录音中 HUD 不显示文字
   - 停止录音后出最终文本
3. `realtime + batch_refine`
   - 录音中有 HUD 文本
   - 停止后进入 refining
   - 成功时只注入 batch 最终稿
4. `realtime + batch_refine` 失败
   - 保留 realtime final
   - 用户看到 refine 失败提示
5. provider probe
6. 历史重转录
7. 切换 provider 后再录音
8. 现有 provider 冒烟回归：
   - OpenAI realtime
   - OpenAI batch
   - Gemini batch
   - Qwen realtime
   - coli realtime / batch

### 必跑命令

按仓库约定，提交前至少运行：

- `pnpm build`
- `cargo check`，至少针对 `src-tauri/`

如果改到相关 Rust 逻辑，最好补并运行对应测试。

### 验收标准

- `pnpm build` 通过。
- `cargo check` 通过。
- 关键手工场景全部通过。
- 通过 capability gating 和手工 smoke，没有发现其他 provider 的明显回归。

## 15. 实现顺序建议

推荐按以下顺序做，风险最低：

1. 任务 1：设置与 provider 注册
2. 任务 2：batch 客户端
3. 任务 3：realtime 客户端
4. 任务 4：状态机接入三种模式
5. 任务 5：stream + batch refine
6. 任务 6：设置页与 i18n
7. 任务 7：probe / 重转录 / 历史快照
8. 任务 8：失败恢复
9. 任务 9：测试与构建验证

更适合按迭代切分为：

1. Iteration 1
   - 任务 1
   - 只做配置模型、provider 注册、capability 骨架
2. Iteration 2
   - 任务 2
   - 打通 batch 主链路
3. Iteration 3
   - 任务 3
   - 打通 realtime 主链路
4. Iteration 4
   - 任务 4 + 任务 5
   - 打通 `realtime + batch_refine`
5. Iteration 5
   - 任务 6 + 任务 7
   - 补 UI、probe、重转录、历史快照
6. Iteration 6
   - 任务 8 + 任务 9
   - 收口错误处理、手工验证、文档回写

## 16. 完成定义

以下条件全部满足，才算这个任务真正完成：

- ElevenLabs provider 在设置页可配置、可保存、可切换
- 三种模式全部可用
- `realtime + batch_refine` 不会双注入
- batch refine 失败时保留 stream final，并对用户可见
- provider probe 支持 ElevenLabs
- 历史重转录支持 ElevenLabs
- 历史记录模型名能反映真实路径
- 现有 provider 在手工 smoke 中未发现明显回归
- `pnpm build` 通过
- `cargo check` 通过

## 17. 每轮迭代更新模板

每轮结束后，在本文件顶部附近补一段迭代记录，格式建议如下：

### Iteration N - 标题

- 状态：`已完成`
- 本轮完成：
  - ...
- 本轮未做：
  - ...
- 新发现：
  - ...
- 对后续任务单的调整：
  - ...
- 下一轮建议：
  - ...

## 18. 参考资料

- [ElevenLabs ASR Provider 接入分析](/Users/xuyun/Projects/VoiceX/docs/elevenlabs-asr-provider-analysis.md)
- [Speech to Text quickstart](https://elevenlabs.io/docs/eleven-api/guides/cookbooks/speech-to-text)
- [Client-side streaming](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/client-side-streaming)
- [Server-side streaming](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/server-side-streaming)
- [Transcripts and commit strategies](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/transcripts-and-commit-strategies)
- [Realtime event reference](https://elevenlabs.io/docs/eleven-api/guides/how-to/speech-to-text/realtime/event-reference)
- [Create transcript API reference](https://elevenlabs.io/docs/api-reference/speech-to-text/convert)
- [Realtime STT API reference](https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime)
