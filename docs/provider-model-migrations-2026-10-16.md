# 供应商模型与鉴权迁移（2026-10-16）

本文记录本次提交的源码行为、隔离范围与本地离线验证。基线为 `201872b755d1e38429cb0af88b4a7e5769a27c52`。

## 已完成：源码及离线验证

| 项目 | 默认值及兼容行为 |
| --- | --- |
| 方舟 LLM | 默认统一为 `doubao-seed-2-0-mini-260428`。仅迁移内置 `llmVolcengineModel` 字段的精确旧默认 `doubao-seed-2-0-mini-260215`；自定义模型、部署 ID、独立接入点和地址不改。官方公布 2026-10-22 14:00（UTC+8）EOS 与系统替换安排，不能据此断言旧 ID 必然直接断供。 |
| Qwen 实时快照 | `qwen3-asr-flash-realtime-2025-10-27`、`qwen3-asr-flash-realtime-2026-02-10` 从预设移除；仅在 `qwenAsrModel` 精确迁移为 `qwen3-asr-flash-realtime`。 |
| Qwen 短音频快照 | `qwen3-asr-flash-2025-09-08`、`qwen3-asr-flash-2026-02-10` 从预设移除；仅在 `qwenAsrBatchModel` 精确迁移为 `qwen3-asr-flash`。 |
| Qwen 异步 filetrans | `qwen3-asr-flash-filetrans-2025-11-17` 标记停服、禁用，并在请求前拒绝。保存值保留，中英文提示要求按录音时长手动选模型；长音频异步接口不能仅靠替换 ID 迁移到当前短音频同步接口。 |
| Fun-ASR 快照 | `fun-asr-realtime-2025-11-07` 从预设移除；仅在 `funasrModel` 精确迁移为 `fun-asr-realtime`。 |
| Gemini Live | 默认改为 `gemini-3.5-transcribe-live`，首次升级迁移精确旧默认 `gemini-3.1-flash-live-preview`；沿用原有专用 TEXT、input transcription 与 manual VAD 分支，接入目录的十分钟录音上限。旧 preview 仍可手动选择，原 AUDIO 分支保留。 |
| OpenAI LLM | 前后端默认统一为官方当前推荐的 `gpt-6-luna`。首次升级时，仅在官方 `https://api.openai.com/v1` 地址（允许末尾斜杠或缺省地址字段）下迁移精确旧值 `gpt-4o-mini` / `gpt-4o-mini-2024-07-18`；兼容服务地址和自定义模型保留。沿用现有策略发送 `reasoning_effort: none`。 |
| Soniox | 每次连接用现有凭据构造 `Authorization: Bearer <api_key>` 升级请求，敏感头禁止在 Debug 输出中显示。首帧 JSON 与其诊断日志不再含 key。空值、空格、控制字符和非 ASCII key 在本地明确报鉴权错误，不修剪或静默修复。 |

ASR 自动迁移保持 provider、region/endpoint、凭据、识别模式和录后精修设置。阿里采用当前客户端支持的同协议主线，未把官方跨 provider 建议直接当作字符串替换。

尚在服务的 OpenAI / Gemini 旧默认用持久字段 `modelDefaultsVersion: 1` 执行一次升级迁移；只处理缺少版本或版本为 0 的旧设置。升级后主动选择旧型号，后续读取不会再次覆盖。未来版本或错误类型不会当作旧版本处理。精确停服快照校验与方舟已知默认迁移独立执行。

启动持久迁移、读取设置迁移与空模型默认回退共用新定义。迁移不归并任意未知、自定义或空白模型值，不改历史 Bench 数据。数据库测试只打开内存 SQLite。

Soniox 继续使用原共享 WebSocket 网络层、代理与取消路径；未新增持久凭据、扩大权限或修改其他供应商连接构造。401 保留连接阶段、不可重试的鉴权错误。现有错误、取消与 session 回归测试覆盖相关状态，但不等于真实录音与界面验收。

## 提交范围与已有改动隔离

从基线通过独立临时索引构造 23 文件候选树，在干净临时目录验证；仅复用本机已有依赖和编译缓存。未安装新依赖，未修改依赖清单或锁文件。

本次纳入已获授权的最小通用停服支持：共享目录 `retired` 元数据结构、前后端精确 ID 校验、已保存选项禁用/标记、Vue 通用提示与中英文文案。阿里迁移依赖这些支持。

另一会话原有 Gemini 批量清理的具体预设删除、停服条目、README 说明、五项 Rust 独立测试及十一项前端断言未纳入本候选树。原工作树中上述内容完整保留。原有维护报告、历史本轮过程报告、JEV Bench 脚本及 `.local-notes/` 也不提交，未覆盖、回滚或删除原工作内容。

| 文件 | 本次提交内容 |
| --- | --- |
| `src/shared/llmModels.json` | 共享 LLM 默认及精确旧 ID |
| `src/shared/asrModels.json` | Live 默认和时限、阿里停服及迁移元数据 |
| `src-tauri/src/llm/config.rs`、`src-tauri/src/llm/mod.rs` | 共享定义读取与精确旧值匹配 |
| `src-tauri/src/commands/settings.rs`、`src-tauri/src/storage/database.rs` | 默认值、一次迁移标记、启动和读取迁移、内存数据库测试 |
| `src-tauri/src/services/llm_service.rs` | 新默认/回退及 reasoning 请求回归测试 |
| `src-tauri/src/asr/models.rs`、`src-tauri/src/asr/config.rs` | 停服校验、精确迁移、录音上限及边界测试 |
| `src-tauri/src/asr/gemini_live_client.rs` | 新默认及保留旧协议的回归测试 |
| `src-tauri/src/asr/soniox_client.rs` | 握手鉴权构造、去除首帧密钥、假 key 与本地 mock 测试 |
| `src/stores/settings.ts`、`src/stores/settings.spec.ts` | 前端共享默认、版本字段与平台 fixture |
| `src/utils/asrModels.ts`、`src/utils/asrModels.spec.ts` | 通用停服选项与本轮断言 |
| `src/components/asr/AsrModelSelect.vue`、`src/i18n/locales/en-US.ts`、`src/i18n/locales/zh-CN.ts` | 停服和 filetrans 限制提示 |
| `scripts/check-llm-defaults.mjs` | 默认一致性及 ASR 断言入口 |
| `README.md`、`README.zh-CN.md`、`README.en.md` | 默认、迁移规则及录音时限同步 |
| 本文件 | 验证及兼容性参考 |

## 候选提交验证结果

以下结果来自隔离后的候选源码，并非含另一会话全部改动的工作树：

- `node scripts/check-llm-defaults.mjs`：通过；MacIntel / Win32 两个 navigator fixture 各通过默认检查及 37 项 ASR 断言。
- `pnpm build`：通过（1622 条 locale 消息、Vue/TypeScript 与 Vite 构建，退出 0）。既有 i18n 脚本在沙箱内输出 `0.0.0.0:24678` 监听 EPERM，未影响成功；本轮未改该脚本。
- 原生 macOS `cargo check --offline --locked`：通过；既有 `block v0.1.6` future-incompatibility warning 保留。
- `cargo test --offline --locked --lib --no-run --message-format=json`：编译通过。随后执行本次候选产出的同一测试二进制，下列相关测试共 **105 项通过**。
- 暂存候选 `git diff --check`：通过；候选代码与测试输入逐文件核对，原工作树已存在文件内容哈希保持不变。

| Rust 测试过滤器 | 通过 |
| --- | ---: |
| `commands::settings::tests` | 29 |
| `asr::models::tests` | 6 |
| `storage::database::tests` | 6 |
| `asr::gemini_live_client::tests` | 5 |
| `asr::config::tests` | 13 |
| `services::llm_service::llm_key_tests` | 9 |
| `asr::qwen_client::tests` | 11 |
| `asr::qwen_transcription_client::tests` | 4 |
| `asr::protocol::tests` | 4 |
| `session::tests` | 4 |
| `asr::soniox_client::tests` | 10 |
| `network::websocket::tests` | 4 |

复现入口（Soniox 和代理测试需允许本地回环监听）：

```sh
node scripts/check-llm-defaults.mjs
pnpm build
cargo check --offline --locked --manifest-path src-tauri/Cargo.toml
cargo test --offline --locked --manifest-path src-tauri/Cargo.toml --lib <上表过滤器>
```

测试只用假 key、内存数据库和本地 mock；没有运行付费 Bench、调用真实模型 API、读取真实应用配置/密钥或安装、打包发布应用。常规前端构建的 `dist/` 是生成产物，未手改或提交。

## 已实现但未验证

- 真实账号、地区、代理下的模型可用性、延迟及识别/纠错/翻译/朗读效果，在线断流重连与 Soniox 服务端握手。
- macOS / Windows 真机 HUD 可见性、麦克风停止和释放、错误后切换 provider 或重试。既有生产状态路径已审阅，离线回归通过，不能称为端到端成功。
- Windows 原生构建：本机离线交叉检查曾因 Rust sysroot 缺目标 `core` 报 E0463；未安装或切换工具链。Win32 fixture 不等于 Windows 构建或真机验证。
- 仓库唯一工作流 `windows-release.yml` 只由 release 或带 tag 的手动事件触发，普通 main 推送不触发。未创建 tag、发布 release 或手动触发安装包构建；远端检查状态须按实际结果单独报告。

## 官方参考

- [方舟模型下线公告](https://docs.volcengine.com/docs/ark/model-deprecation-notice?lang=zh)：mini-260215 的 EOS、迁移目标与系统替换安排。
- [Soniox WebSocket 鉴权迁移](https://soniox.com/docs/guides/migrate-websocket-authentication)：2027-01-15 前迁移至握手 Bearer 鉴权。
- [千问下线通知](https://platform.qianwenai.com/docs/changelog/model-deprecations/old-models)及[阿里 ASR 目录](https://www.alibabacloud.com/help/en/model-studio/asr-model)：快照下线、主线及 filetrans 接口差异。
- [Gemini 停服表](https://ai.google.dev/gemini-api/docs/deprecations#live-api-models)及[3.5 Transcribe 协议](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-5-transcribe)：旧 Live preview 最早可能关停日、转写配置与十分钟上限。
- [OpenAI 停服公告](https://developers.openai.com/api/docs/deprecations#2026-10-14-gpt-4-series-model-deprecations)及[gpt-6-luna 模型说明](https://developers.openai.com/api/docs/models/gpt-6-luna)：快照 2027-01-14 移除、当前替代建议与 Chat Completions / none reasoning；未把旧别名的未来行为推断成已验证事实。
