# OpenAI 听写润色提示词缓存研究

2026 年 11 月 20 日在 OpenAI 官方 Chat Completions 接口上验证了 GPT‑6.1 Terra Fast 的提示词缓存。显式缓存命中后，同长度请求的平均费用下降约 82%，六组配对请求的整段响应耗时没有显示稳定的改善。VoiceX 保留现用的短提示词，暂缓接入显式缓存。

## VoiceX 的可复用前缀

听写润色把规则和当前词典组成首条 `system` 消息，本次原始转写作为下一条 `user` 消息。ASR 也从同一份词典提取热词，按识别服务的协议发送。缓存实验按近期历史停用的场景执行。测试当时，本机配置中的润色规则和词典用 `o200k_base` 离线估算约 595 Token。这个数值随模板和词典而变，OpenAI 以服务端的实际 Token 数判定是否满足 GPT‑6.1 Terra 的 1,024 个可见输入 Token 门槛。[润色请求构造](../src-tauri/src/llm/client.rs) · [OpenAI 提示词缓存指南](https://developers.openai.com/api/docs/guides/prompt-caching)

## 实测结果

使用合成词典和合成听写文本。短、长两组的固定前缀离线估算分别为 581 和 1,132 Token。模型为 `gpt-6.1-terra`，返回的档位均为 `fast`，推理档为 `none`。请求使用 `store: false`，输出上限为 96 Token，所有响应均正常结束。六条不同的听写文本各跑三种条件，轮内打乱顺序。耗时从发起 HTTP 请求计至收到完整的非流式响应。费用按实际用量和当日 Fast 短上下文费率估算，包含输入、缓存读写和输出。[OpenAI API 定价](https://developers.openai.com/api/docs/pricing#fast-pricing-data)

| 条件 | 次数 | 缓存读取 | 缓存写入 | 整段耗时中位数 | 平均每次费用（美元） |
| --- | ---: | ---: | ---: | ---: | ---: |
| 短前缀，默认模式 | 6 | 0 | 0 | 1.176 秒 | 0.0015173 |
| 长前缀，显式模式且没有断点 | 6 | 0 | 0 | 1.433 秒 | 0.0026210 |
| 长前缀，首次显式写入 | 1 | 0 | 1,135 Token | 2.506 秒 | 0.0032595 |
| 同一长前缀，显式命中 | 6 | 每次 1,135 Token | 0 | 1.415 秒 | 0.0004628 |

另外，短前缀加显式断点的两次请求均未发生缓存读写。隔离出来的长前缀使用默认模式时，第一条写入 1,169 Token，第二条读取 1,136 Token 并写入 29 Token。23 次请求共约 0.0375 美元。

显式命中相对同长度关闭缓存的六组请求只在两组中更快。该批次可确认缓存读写和费用变化。持续延迟、真实使用频率下的命中率和增加示例后的润色质量仍待验证。按本次前缀长度和费率，在后续都命中且动态输入相同的条件下，一次写入加两次命中，长版累计输入费用低于三次短版。零散听写和再次写入会改变这个比较。

## 后续接入条件

如果词典或经过质量验证的示例让固定前缀自然达到门槛，可以把断点放在固定内容末尾，把每次变化的转写放进下一条消息。Chat Completions 的显式模式只写入标记的前缀。以下是已在 Terra Fast 上验证的请求结构节选。[Chat Completions API](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)

```json
{
  "prompt_cache_options": {"mode": "explicit", "ttl": "30m"},
  "messages": [
    {
      "role": "system",
      "content": [{
        "type": "text",
        "text": "固定润色规则、词典和示例",
        "prompt_cache_breakpoint": {"mode": "explicit"}
      }]
    },
    {"role": "user", "content": "本次转写"}
  ]
}
```

后续验证可以按请求统计 `usage.prompt_tokens`、`usage.prompt_tokens_details.cached_tokens`、`usage.prompt_tokens_details.cache_write_tokens`、输出 Token、返回档位和整段响应耗时。将首次写入和再次写入计入整批请求成本，并用同一批听写样例做质量对照。[OpenAI 缓存性能监测指南](https://developers.openai.com/api/docs/guides/prompt-caching#monitor-cache-performance)
