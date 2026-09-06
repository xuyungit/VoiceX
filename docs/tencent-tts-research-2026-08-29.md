# 腾讯云 TTS（朗读引擎候选）调研简报

最后更新：2026-08-29
数据来源：腾讯云官方文档（语音合成产品 1073）+ 中文搜索交叉验证

## 状态标记

- **已完成**：产品线现状、字数限制、价格、接入方式均已由官方文档确认。
- **结论**：**暂不接入**（详见 §7）。
- **待确认**：大模型音色的实际音质（无第三方评测，需控制台试听）；首包延迟（官方未给数字）。

---

> **结论（2026-08-29）**：腾讯**没有"混元 TTS"独立产品**，2026 年也没有新一代大模型 TTS 发布——现役"超自然大模型音色"是 2025 年 1 月的代际，之后只是零星加音色。产品长期缺乏更新，接入价值有限，**暂不接入**。若未来腾讯发布新一代 TTS（尤其是混元系语音合成），再按本文 §6 的接入映射评估。

## 1. 产品线现状

- 腾讯云语音合成（产品 ID 1073）音色分四档：**精品音色**（101xxx，约 16 个）、**大模型音色**（501000–501009、601008–601014，约 18 个）、**超自然大模型音色**（502xxx/602xxx/603xxx）、**一句话复刻音色**。
- **无"混元 TTS / Hunyuan-TTS"独立产品**：产品页与文档均无"混元"字样；混元与 TTS 的关联仅是"TTS + 混元 LLM + ASR 组成语音对话"的方案文章。
- **2026 年无代际更新**：产品动态显示 2026-03 仅新增 2 个超自然音色（"沉稳青叔""邻家女孩"）；最近一次代际升级是 2025-01 的"超自然女声"。
- 旁支：TokenHub 大模型服务平台（产品 1823）托管第三方语音模型（如 MiniMax-Speech-2.8-Hd，约合 3.5 元/万字符），未见混元 TTS；"大模型播客"（2025-11）面向播客生成，与朗读无关。

## 2. 合成字数限制

| 接口 | 形态 | 单次限制 | 朗读适用性 |
|---|---|---|---|
| 基础合成 TextToVoice | HTTP POST，一次性 base64 返回 | 中文 **150 字** / 英文 500 字母 | 不适合（无流式） |
| 实时合成 `wss://tts.cloud.tencent.com/stream_ws` | WS，一次送文本、流式返回音频 | 中文 **600 字** / 英文 1800 字母 | **首选** |
| 流式文本合成 `wss://tts.cloud.tencent.com/stream_wsv2` | WS，流式送文本 + 流式返回音频 | 单会话 **1 万字符**（10 分钟无文本自动断开） | 可选，协议更复杂 |
| 长文本合成 | 异步任务（TaskId + 轮询/回调取音频 URL） | **10 万字符**，3 小时内出结果 | 不适合实时朗读 |

> 600 字/请求对 VoiceX 不是硬伤：`split_for_backend()` 按句子边界切块即可（CosyVoice 的分块上限约 150 字），代价是段间多几次 WS 握手，可预建下一段连接掩盖。

## 3. 价格（元/万字符；汉字、字母、数字、标点、空格均算 1 字符）

- **免费额度**（领取后 3 个月有效，一账号一次）：大模型音色 **10 万字符**；超自然 2 万字符；基础/精品 800 万字符。
- **后付费**：大模型音色 **1.2 起**（阶梯降至 0.55）；超自然 **6.5 起**（降至 4.9）；精品 0.3；长文本接口大模型音色 3.2 起。
- **资源包**：1 年有效，10 万字符档起（超自然 60 元/10 万 ≈ 6 元/万）。
- 对比参照（第三方口径）：阿里百炼 CosyVoice 约 2 元/万字符——腾讯"大模型音色"更便宜，"超自然"明显更贵。

## 4. 接入方式

- **WS 实时/流式接口鉴权**：独立的 **HMAC-SHA1 签名**（AppId + SecretId + SecretKey，参数字典序拼串 → HMAC-SHA1 → base64，签名放 URL query）——**不是**腾讯云 v3（TC3-HMAC-SHA256）签名，实现约几十行。HTTP TextToVoice 才走 v3 签名。
- **返回**：二进制音频帧 + JSON 文本帧（时间戳/final 标志），支持边合成边播。
- **音频格式**：pcm / mp3（16bit 单声道），**无 opus**；采样率 8k/16k（默认）/24k。
- **参数**：VoiceType（整数音色 ID）、Speed（-2~6，支持小数）、Volume（-10~10）、EmotionCategory、EnableSubtitle。
- **SSML**：仅基础 HTTP 接口支持；stream_wsv2 不支持。
- **并发**：精品/大模型 20 路，超自然/复刻 10 路。
- **首包延迟**：官方未给数字（推测数百毫秒级，需实测）。

## 5. 音色与口碑

- 官方称支持中文、英文、粤语、四川话及中英混读。
- **无可靠第三方评测**将腾讯大模型音色与火山/CosyVoice 直接对比（搜到的对比只覆盖火山 vs CosyVoice，首包 0.3s vs 0.4–0.5s）。"超自然"档的拟人度提升属厂商宣传，需控制台试听台自行验证。

## 6. 与 VoiceX 架构的映射（若未来接入）

现有模式（`src-tauri/src/tts/volcengine.rs`、`aliyun.rs`）：单 api_key 配置 → HTTP 流式请求 → `ChunkSource` → `decode` → `playback`；超长文本用 `mod.rs::split_for_backend`（火山 5000 字，阿里按模型 spec）。

新增 `tencent.rs` 的差异点：
1. **凭证三元组**：AppId + SecretId + SecretKey（现有 provider 均为单 api_key），settings 需新增字段。
2. **协议是 WebSocket**（现有两家均 HTTP 流式）：需引入 `tokio-tungstenite` 之类依赖。推荐 stream_ws + `split_for_backend(text, 600)`，二进制帧直接喂 `ChunkSource`。
3. **签名**：HMAC-SHA1 + base64 + 字典序参数排序（确认 `hmac`/`sha1` crate 是否已在依赖树）。
4. **音频**：请 mp3 或 pcm 24k，走现有 `decode.rs`；Speed 需把归一化 rate 映射到 [-2, 6]（参照 volcengine.rs 的 `speech_rate_from_normalized`）。

估计工作量 1–2 天（含新依赖与签名实现）。

## 7. 评估与结论

### 7.1 优势
- 大模型音色 1.2 元/万字符 + 10 万字符免费额度，个人朗读用量下成本可忽略。
- WS 流式返回音频，满足低首包朗读的硬性需求；中英混读官方支持。

### 7.2 劣势 / 风险
- **产品停滞**：2025-01 后无代际更新，2026 年只加了 2 个音色——投入接入的回报存疑。
- 音质无第三方背书，需试听验证；首包延迟无官方承诺。
- 600 字/请求偏小；无 opus；流式接口无 SSML；WS + 三元组签名使接入成本高于火山/阿里。

### 7.3 结论
**暂不接入。** 现有火山/CosyVoice/MiMo 已覆盖朗读需求，腾讯档位的价格优势不足以抵消"产品停滞 + 音质未知 + 接入成本更高"。

**下次复查要点**：
1. 产品动态页（1073/49616）是否出现新一代大模型 TTS / 混元系语音合成；
2. TokenHub（1823）是否上架 hunyuan-tts 类模型；
3. 若有新代际：核对 §2 字数限制与 §3 价格是否随之变化，再按 §6 评估接入。

## 8. 参考来源

- 产品页 https://cloud.tencent.com/product/tts
- 音色列表 https://cloud.tencent.com/document/product/1073/92668
- 产品动态 https://cloud.tencent.com/document/product/1073/49616
- 基础合成 TextToVoice https://cloud.tencent.com/document/product/1073/37995
- 实时合成（WebSocket） https://cloud.tencent.com/document/product/1073/94308
- 流式文本合成 wsv2 https://cloud.tencent.com/document/product/1073/108595
- 长文本合成 https://cloud.tencent.com/document/product/1073/57373
- 计费概述 https://cloud.tencent.com/document/product/1073/34112
- 购买指南 https://cloud.tencent.com/document/product/1073/78325
- TokenHub 语音模型 https://cloud.tencent.com/document/product/1823/130055
