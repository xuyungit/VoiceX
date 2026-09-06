# 阿里云百炼语音合成接入调研（2026-08-13）

调研目标：确认阿里云百炼（Model Studio）的 TTS 模型该按几个供应商接入，
以及 `qwen3-tts-flash` 与 `qwen-audio-3.0-tts-flash` 的接口差异。

**结论基于实测，不是读文档得出的**——官方参数表在多处与实际行为不符，详见 §3。
复现脚本：[`scripts/tts/aliyun_probe.py`](../scripts/tts/aliyun_probe.py)，
读取应用自己数据库里的百炼 Key，跑的就是应用将来要跑的账号。

> **状态：已实现**（同日）。后端在
> [`src-tauri/src/tts/aliyun.rs`](../src-tauri/src/tts/aliyun.rs)，按 §4 的设计落地。
> 音色表已用 `aliyun::tests::voice_table_matches_what_the_service_accepts`
> 逐个对真实账号验证；端到端链路用 `aliyun::tests::live_synthesis_decodes_and_plays`
> 验证（两个模型均 48 kHz 出声，约 8 秒音频）。

---

## 1. 结论

**做成一个供应商「阿里云百炼」，下面挂一个模型下拉。**

两个模型走各自的 HTTP + SSE 端点，请求/响应结构几乎相同，差异小到用一张
参数名映射表就能吸收：

| | `qwen3-tts-flash` | `qwen-audio-3.0-tts-flash` |
|---|---|---|
| 端点路径 | `/api/v1/services/aigc/multimodal-generation/generation` | `/api/v1/services/audio/tts/SpeechSynthesizer` |
| 主机 | `dashscope.aliyuncs.com` | 同左（workspace 主机亦可） |
| 请求体 | `{model, input:{text, voice, …}, parameters:{…}}` | 同左 |
| 流式开关 | `X-DashScope-SSE: enable` | 同左 |
| 流式分块 | `output.audio.data`（base64 mp3） | 同左 |
| 末块 | `output.audio.url` + `usage.characters` | 同左 |
| 语速 | `speech_rate` | `rate` |
| 音调 | `pitch_rate` | `pitch` |
| 格式 | `response_format` | `format` |
| 音色 | `Cherry` 等约 48 个 | `longanfengyue` 等 12 个 |

**不需要 WebSocket。** 两条 HTTP+SSE 链路都能流式吐 base64 mp3，首包
400–600 ms，与现有 Volcengine 实现（282–621 ms）同一量级。这意味着
[`volcengine.rs`](../src-tauri/src/tts/volcengine.rs) 的三线程骨架、
[`decode.rs`](../src-tauri/src/tts/decode.rs) 的 MP3 管线、
[`playback.rs`](../src-tauri/src/tts/playback.rs) 全部原样复用，一行解码器都不用新写。

需要**在同一个供应商内部按模型分派**的只有三处：端点路径、三个参数名、音色表。

---

## 2. 与前一版结论的差异

本文档 08-13 上午的初版基于官方文档，结论是「协议不同必须拆两个供应商，
且两类都必须走 WebSocket」。实测推翻了其中四条：

| 初版结论 | 实测 |
|---|---|
| qwen3-tts HTTP 没有语速/音调/格式/采样率参数 | **错**。四个全部生效，只是文档参数表没列 |
| qwen3-tts HTTP 单次上限 512 Token | **错**。5000 字被正常接受 |
| 两类都必须走 WebSocket | **错**。HTTP+SSE 首包 400–600 ms，够用 |
| B 类强制 WorkspaceId 域名 | **错**。旧 `dashscope.aliyuncs.com` 主机 HTTP 与 WS 均可用 |

保留成立的：端点不可互换、音色不跨模型、参数名不通用。

---

## 3. 实测记录

### 3.1 参数是否真的生效（`qwen3-tts-flash` / HTTP）

判据是返回音频的字节数：WAV 头里的帧数字段是坏的（读出来是 44739 秒），
但字节长度直接反映音频时长，且同参数重跑波动 <2%，所以字节数变了就是参数生效了。

基线 245,804 B（24 kHz / 16-bit / 单声道 WAV，约 5.1 s）：

| 请求字段 | 结果 | 判定 |
|---|---|---|
| `parameters.speech_rate=0.5` | 533,660 B（2.1x） | ✅ 生效 |
| `parameters.speech_rate=2.0` | 138,980 B（0.55x） | ✅ 生效 |
| `parameters.pitch_rate=0.5` | 1,064,964 B（4.2x） | ✅ 生效（语义见下） |
| `parameters.response_format=mp3` | 返回 `.mp3`，66,499 B | ✅ 生效 |
| `parameters.sample_rate=16000` | 180,660 B（0.71x ≈ 16/24） | ✅ 生效 |
| `parameters.volume=10` | 245,804 B（与基线逐字节同长） | 字节数测不出音量，未判定 |
| `parameters.rate=2.0` | 257,324 B（≈基线） | ❌ 名字不对，被忽略 |
| `parameters.format=mp3` | 仍返回 WAV | ❌ 名字不对，被忽略 |
| `input.format=mp3` | 仍返回 WAV | ❌ 位置不对，被忽略 |

**以上 `speech_rate` / `pitch_rate` / `response_format` / `sample_rate` 四个
参数，官方非实时 HTTP 文档的参数表里一个都没有**（该表只列
`model` / `input.text` / `input.voice` / `input.language_type` /
`instructions` / `optimize_instructions`）。

> `pitch_rate=0.5` 让音频长到 4.2 倍，超过朴素重采样应有的 2 倍。降调确实会
> 拉长时长，但倍率对不上，说明它不是单纯的重采样。接入时要听感确认这个参数
> 到底改了什么，别直接把 UI 的音调滑块接上去。

`qwen-audio-3.0-tts-flash` 一侧，`rate=0.5` 放在 `input` 或 `parameters` 都生效
（448,411 B / 458,011 B，均约 2x），说明该端点对两个位置都接受。

### 3.2 流式结构与首包延迟

`X-DashScope-SSE: enable`，测试文本 111 字：

| 链路 | 首包 | 块数 | 首块魔数 |
|---|---|---|---|
| `qwen3-tts-flash` 默认 | 661 ms | 28 | `52494646` = `RIFF`（WAV） |
| `qwen3-tts-flash` + `response_format=mp3` | **428 ms** | 29 | `49443303` = `ID3`（MP3） |
| `qwen-audio-3.0-tts-flash` + `format=mp3` @ workspace 主机 | 539 ms | 23 | `ID3` |
| `qwen-audio-3.0-tts-flash` + `format=mp3` @ 旧 dashscope 主机 | **407 ms** | 23 | `ID3` |

两条链路的分块结构完全一致：中间块 `output.audio.data` 是 base64，末块带
`output.audio.url` 和 `usage.characters`。

### 3.3 端点、音色、独占字段

```
qwen3-tts-flash          @ multimodal-generation    OK
qwen3-tts-flash          @ SpeechSynthesizer        400 InvalidParameter: url error
qwen-audio-3.0-tts-flash @ SpeechSynthesizer        OK
qwen-audio-3.0-tts-flash @ multimodal-generation    400 InvalidParameter: url error

qwen3-tts-flash          + voice=longanfengyue      400 Invalid voice specified
qwen-audio-3.0-tts-flash + voice=Cherry             400 [cosyvoice:]Engine error [411]

qwen3-tts-flash          + input.material_id        200 OK（静默忽略）
qwen3-tts-flash          + input.instruct           200 OK（静默忽略）
qwen-audio-3.0-tts-flash + input.language_type      200 OK（静默忽略）
```

端点绑死模型，音色两个方向都不通用——这两条和另一份调研一致。

但**独占字段不会报错，是静默返回 200**。这比报错危险：把 qwen-audio 的
`instruct` 原样发给 qwen3，接口照收不误，只是完全没效果，排查时看不出任何异常。
所以参数不能靠"发过去试试"，必须按模型白名单构造。

`qwen-audio-3.0-tts-flash` 的错误信息里写的是 `[cosyvoice:]Engine error`，
印证了它和 CosyVoice 共用后端——这是后面加 CosyVoice 成本极低的直接证据。

### 3.4 单次文本长度上限

只判「请求是否被接受」（收到第一个音频块即断开）：

| 模型 | 500 字 | 1200 字 | 2000 字 | 5000 字 | 20000 字 |
|---|---|---|---|---|---|
| `qwen3-tts-flash` | 479 ms | 789 ms | 955 ms | **1887 ms** | 未测 |
| `qwen-audio-3.0-tts-flash` | — | — | 609 ms | 574 ms | ❌ 拒绝 |

- **`qwen3-tts-flash` 接受 5000 字**，文档写的 512 Token 上限不存在。
  但首包延迟随文本线性增长，5000 字要 1.9 s——现有 `MAX_CHARS = 5000` 能跑通，
  只是长文开口偏慢。
- `qwen-audio-3.0-tts-flash` 在 20000 字处拒绝，错误信息
  `Each request sends text is limited: 20000, current: 36000`。注意我发的是
  20000 个字符，服务端算成 36000——**它的计数不是纯字符数**，做切分时不能
  按字符数卡到上限边缘。

顺带一个结论：**非流式长文本合成慢到不可用**。5000 字非流式请求跑了 7 分钟
仍未返回。接入必须走 SSE，这不是为了首包好看，是功能性要求。

### 3.5 WebSocket（备查，本期不接）

| 链路 | 结果 |
|---|---|
| `wss://…/api-ws/v1/realtime?model=qwen3-tts-flash` | ❌ 拒绝（非 JSON 响应，握手即失败） |
| `wss://…/api-ws/v1/realtime?model=qwen3-tts-flash-realtime` | ✅ 13 块 mp3 |
| `wss://{workspace}.cn-beijing.maas…/api-ws/v1/inference` + `qwen-audio-3.0-tts-flash` | ✅ 9 个二进制帧 mp3 |
| `wss://dashscope.aliyuncs.com/api-ws/v1/inference` + `qwen-audio-3.0-tts-flash` | ✅ 同上 |

**`qwen3-tts-flash` 在 WebSocket 上连不上，必须换成 `qwen3-tts-flash-realtime`**
——另一份调研这一条完全正确。两个是不同的模型名，不是同一模型的两种接法。

realtime 链路的事件序列（实测收到）：`session.updated` →
`input_text_buffer.committed` → `response.created` → `response.output_item.added`
→ `response.content_part.added` → `response.audio.delta` ×N →
`response.content_part.done` → `response.output_item.done` →
`response.audio.done` → `response.done`。

WS 相对 HTTP 的唯一实质好处是文本可增量 `append`、无单次长度上限。我们的
选中文本在请求发出前就全部已知，用不上——和
[`volcengine.rs`](../src-tauri/src/tts/volcengine.rs) 开头写的理由一样。

---

## 4. 落地设计

### 4.1 设置项

```
ttsProviderType: 'system' | 'volcengine' | 'aliyun'
aliyunTtsApiKey:     string          // 与 qwenAsrApiKey 同一个百炼 Key，但独立存
aliyunTtsModel:      'qwen3-tts-flash' | 'qwen-audio-3.0-tts-flash'
aliyunTtsVoiceQwen3: string          // 默认 'Cherry'
aliyunTtsVoiceAudio: string          // 默认 'longanfengyue'
aliyunTtsRate:       number          // 0..1，沿用现有归一化
aliyunTtsVolume:     number
```

音色按模型族分开存：ID 不通用，共用一个键会在切模型时发出必然报 400 的请求。
语速/音量可以共用，两个模型的量纲相同（0.5–2.0 倍率 / 0–100）。

WorkspaceId 不做成设置项：实测旧主机可用，多一个必填框只会挡住用户。等阿里云
真的下线旧域名再加，届时可以复用 ASR 侧已有的
[`qwen_workspace_host()`](../src-tauri/src/asr/funasr_client.rs:719)。

### 4.2 后端

新增 `src-tauri/src/tts/aliyun.rs`，照
[`volcengine.rs:198`](../src-tauri/src/tts/volcengine.rs:198) 的三线程骨架
（网络 / 解码 / 播放）改写，`CancelToken` 的交接规则一字不改。模型差异集中在
一个函数里：

```rust
struct ModelSpec {
    endpoint: &'static str,   // 两条路径之一
    rate_key: &'static str,   // "speech_rate" | "rate"
    format_key: &'static str, // "response_format" | "format"
    max_chars: usize,         // 5000 | 15000（留出计数差的余量）
}
```

一律请求 mp3，SSE 逐行解析取 `output.audio.data` 做 base64 解码，喂给现有的
`ChunkSource` → `decode_mp3_stream` → `Playback`。末块的 `url` 直接丢弃。

### 4.3 参数映射

VoiceX 存的是 0.0–1.0、以 0.5 为 1x 的归一化语速。

| VoiceX | qwen3-tts-flash | qwen-audio-3.0-tts-flash |
|---|---|---|
| `rate` | `parameters.speech_rate` | `parameters.rate` |
| `volume` | 不发，走本地播放增益 | 同左 |
| `pitch` | 本期不接（§3.1 语义存疑） | 本期不接 |

`(rate / 0.5).clamp(0.5, 2.0)` 直接给，比 Volcengine 那套 -50..100 干净。

音量维持本地增益的理由和 Volcengine 一致：本地调节即时生效，改音量不需要
重新合成一次（重新合成要重新计费）。

### 4.4 需要改的现有代码

[`ReadingSettings.vue:479`](../src/views/ReadingSettings.vue:479) 现在用
`v-if="!isVolcengine"` 藏音调滑块。本期阿里云也不接音调，所以可以先不动；
但这个判断迟早要换成 `TtsBackend` 上的能力位，写死供应商名的判断不可扩展。

---

## 5. 第一阶段范围

- 一个供应商 `aliyun`，模型下拉两项：`qwen3-tts-flash`（默认）、`qwen-audio-3.0-tts-flash`
- 传输 HTTP + SSE，一律请求 mp3
- 参数只做 voice / 语速，音量走本地增益
- 音色内置 allow-list（沿用 Volcengine 的做法），设置页同时允许手填 ID
- `max_chars`：qwen3 设 5000（实测可过），qwen-audio 设 15000（为 §3.4 的计数差留余量）

**明确推迟**：`instructions` 指令控制、`pitch`、SSML、情感/富语言标签、
声音复刻（`material_id`）、声音设计（`prompt`）、CosyVoice 全系、MiniMax、
opus 编码、WebSocket 实时链路、方言音色分组 UI。

每个模型的独占参数确实会组合爆炸，而且 §3.3 证明发错了不报错——所以第一阶段
一个都不做，等两条主链路跑通再按白名单逐个加。

---

## 6. 模型与价格（备查）

百炼 TTS 共四个系列：

- **Qwen3-TTS**：`qwen3-tts-flash`、`qwen3-tts-instruct-flash`、对应 `-realtime` 版本、VC/VD 变体
- **Qwen-Audio-3.0-TTS**：`qwen-audio-3.0-tts-flash` / `-plus`
- **CosyVoice**：`v1` / `v2` / `v3-flash` / `v3-plus` / `v3.5-flash` / `v3.5-plus`
- **MiniMax**：`MiniMax/speech-2.8-hd` 等（转售，仅 HTTP）

CosyVoice 与 Qwen-Audio-3.0-TTS 共用一套 API（§3.3 的 `[cosyvoice:]` 错误前缀
可证），以后加 CosyVoice 只是往模型下拉里加一行。

价格（国际站美元报价，中国站人民币价以控制台为准），一律按**输入字符**计费：

| 模型 | 单价 | 免费额度 |
|---|---|---|
| `qwen3-tts-flash` | $0.1 / 万字符 | 1 万字符 |
| `qwen-audio-3.0-tts-flash` | $0.15 / 万字符 | 1 万字符 |
| `qwen-audio-3.0-tts-plus` | $0.2 / 万字符 | 1 万字符 |
| `cosyvoice-v3-flash` | 1 元 / 万字符 | — |

### 音色

**qwen3-tts-flash**（约 48 个，10 语种 + 9 种中文方言）：
`Cherry`（芊悦）、`Serena`（苏瑶）、`Ethan`（晨煦）、`Chelsie`（千雪）、
`Nofish`、`Jennifer`、`Ryan`、`Vincent`（田叔）、`Neil`（新闻腔）…
方言：`Jada`（上海）、`Dylan`（北京）、`Li`（南京）、`Marcus`（陕西）、
`Roy`（闽南）、`Peter`（天津）、`Sunny`/`Eric`（四川）、`Rocky`/`Kiki`（粤语）。

**qwen-audio-3.0-tts-flash**（12 个）：
`longanfengyue`、`longanyuanfei`、`longanlingxi`、`longanxiaoxin`、
`longanhuan_v3.6`、`longjielidou_v3.6`、`longpaopao_v3.6`、`longhuohuo_v3.6`、
`longchuanshu_v3.6`、`loongmary`、`loongeva_v3.6`、`loongjohn`。
plus 档是另一组（`longanlingxin`、`longanlufeng`）。另有 500+ 复刻底模。

---

## 7. 遗留未验证

1. **音量参数是否生效**：字节数测不出音量，需要比 RMS。不影响本期（走本地增益）。
2. **`pitch_rate` 的真实语义**：4.2x 时长对不上朴素重采样，需听感确认。
3. **免费额度是按模型还是按账号**：1 万字符的口径未验证。
4. **中国站人民币单价**：~~只拿到国际站美元报价~~ 已核实（见 §8，2026-08-30）。
5. **旧 `dashscope.aliyuncs.com` 主机的下线时间**：官方说要迁到 workspace 域名，
   但未给期限，实测仍可用。

---

## 8. 补记（2026-08-30）：cosyvoice-v3.5-flash 探测与中国站价格

### 8.1 中国站价格（官方定价页实时核实）

按输入字符计费、输出不计费；**一个汉字计 2 个字符**（英文/数字/标点/空格计 1），
SSML 标签不计入。免费额度均为 1 万字符（仅北京地域，开通后 90 天有效）。

| 模型 | 元/万字符 |
|---|---|
| qwen3-tts-flash | 0.8 |
| qwen-audio-3.0-tts-flash | 1.0 |
| qwen-audio-3.0-tts-plus | 1.4 |
| cosyvoice-v3.5-flash | 0.8 |
| cosyvoice-v3.5-plus | 1.5 |
| cosyvoice-v3-flash | 1.0 |
| cosyvoice-v3-plus / v2 / v1 | 2.0 |

百炼还上架了 MiniMax：speech-2.8-turbo 2 元、speech-2.8-hd 3.5 元（同一 API Key 可用）。

### 8.2 cosyvoice-v3.5-flash：无系统预置音色，不可作朗读引擎

- 实测（2026-08-30，SpeechSynthesizer HTTP 端点）：v3 音色表全量、各种 `_v3.5`
  后缀猜测、以及**不传 voice 参数**，一律返回 `InvalidParameter: [cosyvoice:]
  Engine return error code: 418`；官方 FAQ 确认 418 = 音色不支持。同请求换
  cosyvoice-v3-flash + `longanyang` 正常出音频（对照组）。
- 官方音色列表页无 v3.5 小节；声音复刻 API 文档将 v3.5-flash/plus 仅列为
  **复刻音色的 target_model**（创建复刻音色免费，需 10-20 秒样本音频）。
- 结论：v3.5 的低价（0.8 元）只对自备复刻/设计音色可用，**预置音色场景不可用**，
  VoiceX 暂不提供该选项（曾加入后因必然 418 回退）。复查信号：
  [CosyVoice 音色列表](https://help.aliyun.com/zh/model-studio/cosyvoice-voice-list)
  页面出现 v3.5 小节。
- 若将来做「声音复刻」功能，v3.5-flash 是首选合成目标（免费建音色 + 最低合成价）。

### 8.3 qwen-audio-3.0-tts-flash 音色全量（2026-08-30 核实）

- **系统音色共 12 个**（官方[音色列表页](https://help.aliyun.com/zh/model-studio/qwen-audio-tts-voice-list)）：
  此前内置 8 个，缺 龙泡泡 `longpaopao_v3.6`、龙火火 `longhuohuo_v3.6`、
  龙川叔 `longchuanshu_v3.6`、loongeva `loongeva_v3.6`，已实测补齐。
  plus 型号另有旗舰音色 `longanlingxin`/`longanlufeng`（flash 不可用）。
- **基础音色 597 个**：命名 `qwen-audio-3.0-tts-flash-<后缀>`，调用方式与系统
  音色一致，完整清单在音色列表页的 Excel（含名称/性别/年龄/特质/场景/试听）。
  场景分布：日常对话 316、情感陪伴 144、有声阅读 18、知识分享 6、新闻播报 12、
  有声书配音 7、古风有声书 10 等；英文仅 8 个。
- 从「有声阅读/知识分享/演讲朗诵」挑了 7 个补入内置列表（龙蓉桃涟、龙翼暮凌、
  龙羽瑶鸾、龙霓岚凤、龙瑾涟晓、Adrian Gao、Ivy Hu），全部实测出音频；
  其余基础音色可在音色选择器中手输 ID 使用。

---

## 参考

- [实时语音合成（总览）](https://help.aliyun.com/zh/model-studio/realtime-tts-user-guide)
- [非实时语音合成](https://help.aliyun.com/zh/model-studio/non-realtime-tts-user-guide)
- [语音合成模型列表](https://help.aliyun.com/zh/model-studio/tts-model)
- [Qwen-TTS API（非实时 HTTP）](https://help.aliyun.com/zh/model-studio/qwen-tts-api)
- [qwen-tts 实时合成交互流程与两种模式](https://help.aliyun.com/zh/model-studio/interactive-process-of-qwen-tts-realtime-synthesis)
- [Qwen-TTS-Realtime 客户端事件](https://help.aliyun.com/zh/model-studio/qwen-tts-realtime-client-events)
- [Qwen-TTS-Realtime 服务端事件](https://help.aliyun.com/zh/model-studio/qwen-tts-realtime-server-events)
- [Qwen-Audio-TTS/CosyVoice WebSocket API](https://help.aliyun.com/zh/model-studio/cosyvoice-websocket-api)
- [Qwen-Audio-TTS/CosyVoice 客户端事件](https://help.aliyun.com/zh/model-studio/cosyvoice-client-events)
- [Qwen-Audio-TTS/CosyVoice 服务端事件](https://help.aliyun.com/zh/model-studio/cosyvoice-server-events)
- [Qwen-TTS 音色列表](https://help.aliyun.com/zh/model-studio/qwen-tts-voice-list)
- [Qwen-Audio-TTS 音色列表](https://help.aliyun.com/zh/model-studio/qwen-audio-tts-voice-list)
- [百炼模型价格](https://help.aliyun.com/zh/model-studio/model-pricing)
