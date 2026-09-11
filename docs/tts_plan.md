# VoiceX 跨应用选中文本朗读（TTS）功能实施计划

> 文档状态：正式版 / 开发基线
> 日期：2026-08-12（阶段 0 完成后回填实测结论；同日回填阶段 1 的 P0 路径分布实测，见 §5.1）
> 讨论过程与各方意见记录见 `tts_plan_discussion_archive.md`

## 0. 进度

| 阶段 | 状态 | 说明 |
|---|---|---|
| 阶段 0 最小端到端原型 | **已完成** | commit `193e5e5`，分支 `feat/tts-selected-text-phase0` |
| 阶段 1 测试基建与选区验证 | 部分完成 | P0 七应用路径分布已实测（§5.1）；**L2c 实质已存在**（`p0_survey.sh` / `smoke_phase0.sh` / `negative_cases.sh` 都是注入真实热键 + 断言结构化日志）；剪贴板失败关闭规则已单测；负例已覆盖两条。缺 L2b 与门禁脚本 `evaluate_gate.py`——**已判定为发布流程而非产品，等真要对外宣布时再做**（§7 第 6 条） |
| 阶段 2 热键多动作路由工业化 | **已完成** | 冲突检测、配置化、持久化、冲突的 UI 呈现均已落地（随 §5.2 设置页一起做） |
| 阶段 3 系统语音链路工业化 | **已完成** | 取消语义、Escape、ASR/TTS 互斥、参数化、`AVSpeechSynthesizerDelegate`（替换轮询）、HUD 状态显示全部落地 |
| 阶段 4 云端后端 | **首家已接通** | 火山引擎 Seed-TTS 2.0 全链路可用：播放层（cpal + symphonia 流式解码）、`VolcengineBackend`、设置页配置分组均已落地，**真机端到端与两条打断路径已验证**（§5.4）。情感参数暂缓 |
| 阶段 5 打磨与发布收尾 | 部分完成 | 双语 README 已补"选中朗读"章节（含与系统「朗读所选内容」的差异、数据去向）；设置页已在选中云端引擎时一次性告知；✅ L2b 诊断已挂入设置页（诊断模式下可见）。剩余仅门禁脚本 |

**2026-08-12 产品方向转向**：原型阶段结束，重心转为产品化。两项重大变更——**§3.4 敏感度机制整体废除**（连带解除云端的前置决策），以及**实施顺序改为"删规则 → 设置页 → delegate+HUD → 中英分段 → 云端逐个接入"**。新会话请先读 §7，再读 §3.4、§5.2、§5.3。

**2026-08-12（同日晚）§7 第 1、2 步已实施**：敏感度机制已从代码中删除；朗读设置页 `/reading-settings` 已上线。

**2026-08-12 收尾状态**：选中朗读功能**已可日常使用**，火山引擎为默认推荐引擎。真机验证覆盖：完整朗读链路、热键与 Escape 两条打断路径、HUD 状态与波形、VS Code 取词。`cargo test --lib` 219 项全绿。剩余未完成项**全部是发布流程**（L2b 诊断命令、门禁脚本），不是功能缺口。

**同日再次调整（§7 顺序重排）**：火山引擎 Seed-TTS 2.0 已完成原型验证（§5.4），**云端接入提到 delegate/HUD 之前**。硬依据是实测出来的：本地 18 个中文音色全为 compact 档，增强/高级档 0 个，而 `say` 默认更好听是因为它用 **Siri 音色——第三方应用永远拿不到**。本地路径的天花板是苹果策略设的，调不动，所以尽快上云端。中英分段音色一步**已取消**。另有一处文档更正见 §4.4 #1（原先关于 ad-hoc 签名导致 TCC 失效的说法是错的）。

标注 **⚠️ 实测修正** 的段落，是真机验证推翻或修正了原计划假设的地方；§5.1 是 P0 七应用的实测数据与结论。注意 §5.1 中"敏感度"相关的结论已促成该机制废除，其中的隐私侧处置要求均已作废，取词侧事实仍然有效。

阶段 0 代码位置：

- `src-tauri/src/selection/` — 平台无关接口 + macOS 实现（`macos/ax.rs` 取词、`macos/clipboard.rs` Copy 降级）
- `src-tauri/src/tts/` — `TtsBackend` trait、`MacSystemBackend`、会话控制与结构化日志
- `src-tauri/src/hotkey/manager.rs` — 单监听器内的第二个动作绑定与冲突检测
- `scripts/tts/` — CGEvent 注入工具（`cgevent_key.py` / `cgevent_click.py`）、阶段 0 smoke（`smoke_phase0.sh`）、共享 harness 机制（`lib.sh`）与 P0 路径调查（`p0_survey.sh`）

运行方式：先以 `pnpm tauri dev 2>&1 | tee <log>` 启动，再执行 `scripts/tts/smoke_phase0.sh --log <log>`（阶段 0 两应用链路）或 `scripts/tts/p0_survey.sh --log <log>`（P0 七应用路径分布，`--app` 可指定子集）。均需要辅助功能权限，见 §4.4 #1。

## 1. 背景与目标

为 VoiceX 增加跨应用朗读能力：用户在任意应用中选中文字，按下可配置的全局快捷键，VoiceX 读取所选文字并通过所选 TTS 引擎朗读。

**产品形态**：在现有 VoiceX 中扩展，不做独立应用；代码上实现为边界清晰的独立 TTS 子系统（选区读取、TTS 后端、会话控制解耦）。macOS 首发，核心接口保持平台无关，Windows 后置。

**首版包含**：独立朗读快捷键（可配置）；AX 选区读取 + 可关闭的 Copy 兼容模式；macOS 系统语音（默认、零配置）；朗读设置页（Provider / 音色 / 语速 / 音量 / 音调 / 试听，见 §5.2）；HUD 状态显示（§5.3）；若干常用云端 TTS Provider（逐个接入，见阶段 4）；开始、停止、取消与明确错误提示；不保存选区历史。

**首版不包含**：OCR、跨应用逐词高亮、暂停/继续、输出设备选择与 ducking、Windows、扫描 PDF / Canvas / 远程桌面识别、中英混排自动分段音色（§5.2 末尾，紧随首版之后）。

**非保证场景**（对外表述与现有文本注入能力对齐）：图片、扫描 PDF、Canvas、自绘控件、远程桌面、受保护内容、安全输入框。

## 2. 关键决策

| 决策点 | 结论 | 要点 |
|---|---|---|
| 产品形态 | 扩展 VoiceX，不做独立应用 | 热键、权限、按键模拟、前台识别、供应商配置、HUD/托盘等基础设施约七成现成；macOS 权限按应用授权，两个应用分别索权是体验倒退。若未来出现独立用户群、独立定价或权限冲突等条件再评估拆分，且先抽共享核心库 |
| 取词策略 | AX 优先、Copy 兜底 | AX 无副作用但覆盖率有限；Copy 覆盖率高但有剪贴板副作用，作为可关闭的兼容模式。验证体系对两条路径同时度量，留数据复核余地 |
| 开发顺序 | 原型优先 | 第一步用最短路径跑通"热键 → 取词 → 系统语音朗读"端到端原型；随后建立验证基建并工业化。原型跑通 ≠ 覆盖率达标，选区覆盖率门禁是宣布功能可用的前置条件 |
| 云端时机 | 系统语音验证后接入（**接入节奏见下一行，已改**） | 系统语音链路先完整交付（零密钥可用）。统一 `TtsBackend` 接口自阶段 0 即存在（最小形态），避免抽象空转。~~之后一次调研多家云端、首期接入一家~~ |
| 验证方式 | 自动化优先 | 脚本驱动真实应用的端到端测试 + 脚本化门禁判定，替代人工测试矩阵；人工仅保留五项不可自动化的残余工作（§4.4） |
| 隐私边界（2026-08-12 改） | **不做内容判定** | 用户选中并主动要求朗读的文字就照读，本地与云端引擎一视同仁。单机工具、文字由用户自己选中、风险由用户自己承担；原三态判定的复杂度、竞态与误挡远大于收益（§3.4） |
| 云端接入节奏（2026-08-12 改） | 先接常用几家，再调研补漏 | 原为"先出多家调研简报、冻结选型、再接一家"。改为先把已知常用的供应商逐个接上，之后再查遗漏 |

## 3. 技术方案

### 3.1 选区读取：分层策略

macOS 无单一 API 能覆盖所有应用的选区读取；业界成熟做法为分层 fallback。运行时顺序：

1. **AX 直接读取**：快捷键触发瞬间冻结前台应用与聚焦元素快照 → 读 `kAXFocusedUIElementAttribute` → `kAXSelectedTextAttribute`。成功时零副作用。
2. **AX 范围读取**：元素只提供选区范围时，经 `kAXSelectedTextRangeAttribute` + 参数化属性取文本。仅在类型与范围明确有效时执行，不吞 API 错误。
3. **Copy 兼容模式**（可关闭）：等待热键修饰键抬起 → 复核前台应用未变 → 备份剪贴板（多类型，失败关闭规则见 §3.4）→ 模拟 Cmd-C → 以 `NSPasteboard changeCount` 判定复制是否发生（超时 300 ms）→ 读纯文本 → 无并发修改时恢复。
4. **明确失败**：HUD 区分"没有选中文字 / 当前控件不支持 / 权限缺失 / 复制超时 / 安全输入拒绝 / 修饰键未抬起 / 前台应用已切换"，日志记录结构化失败类型（不记全文）。

**⚠️ 实测修正（阶段 0）**：

- **修饰键等待是必需步骤，不是优化**。用户按住 ⌥⌘ 时合成 Cmd-C，目标应用收到的是 ⌥⌘C——别人的快捷键。实现为最长等待 800 ms，超时返回 `modifiers_held` 而非硬发按键。
- **等待窗口后必须复核前台应用**。800 ms 足够切换窗口；不复核就可能把 Cmd-C 发给一个从未做过安全输入检查的应用，并读回错误内容。不一致返回 `foreground_changed`。
- **`org.nspasteboard.TransientType` 标记在取词方向不可实现**，原文的承诺已删除。剪贴板管理器污染来自**目标应用**写入的那一次复制，VoiceX 既不是写入方，也无法事后给它打标。若仍要减少污染，需要单独设计（例如缩短占用窗口，或在文档中如实声明该限制），不要假装已做。

**⚠️ 实测修正（阶段 1，§5.1）**：

- **`AXWebArea` 上的"AX 范围读取"对 Safari 无效，不要按原顺序实现。** WebKit 不公布 `AXSelectedText`，也不公布 `AXSelectedTextRange`，只有 `AXSelectedTextMarkerRange`。第 2 层若照原样实现，Safari 一行都读不到。
- **判断某个应用能不能靠 AX，要看它公布的属性表，不能只看 `AXError`。** Safari 对一个自己没有的属性回的是 `kAXErrorNoValue`（−25212，"有属性但没值"）而不是 `kAXErrorAttributeUnsupported`。状态码在这里会说谎，属性枚举不会。

**Electron/Chromium 兼容适配器**：不对 Chrome/Electron 一概写入 `AXManualAccessibility`。实现为应用特定适配器：先查询目标进程是否支持该属性；仅对已验证的 Electron 应用（逐个验证后加入适配表）启用，并记录是否改变了目标应用状态。**Chrome 的默认 AX 行为已实测：无需任何干预，`AXSelectedText` 直接可用（§5.1）。** 真正需要适配的是 VS Code——冷启动时系统级取不到聚焦元素，AX 树起来之后才有，阶段 1 需要为此做适配器。

```text
全局快捷键（单监听器内的 ReadSelection 动作）
  -> 冻结前台应用与焦点快照（HUD 不夺焦点）
  -> AX 直接读取 -> AX 范围读取
  -> Copy 兼容读取（修饰键等待 -> 前台复核 -> 快照 -> Cmd-C -> 读取 -> 恢复）
  -> 文本规范化（表格文本、换行、空白）
  -> TtsSession -> TtsBackend（系统语音直接朗读 | 云端合成+播放）
  -> HUD 状态
```

### 3.2 Rust 与 macOS 原生接口

技术路径可行：代码库已有全部三类互操作先例（C FFI、CGEvent、objc2），无需 Swift/ObjC 混编。具体依赖与格式细节在对应阶段落地确认：

- **C FFI（AX API）**：`hotkey/permissions.rs` 已有 `AXIsProcessTrusted` 先例；取词只需约 5 个 ApplicationServices C 函数，自声明 `extern "C"` 实现。`core-foundation` 目前是传递依赖，需加为直接依赖以管理 CFString/CFTypeRef。
- **CGEvent（模拟 Cmd-C）**：`injector/clipboard.rs` 的 Cmd-V 注入已覆盖事件 source 与 flag 处理等深层问题；取词方向（备份 → 复制 → 读取 → 条件恢复）与注入方向（写入 → 粘贴 → 恢复）假设不同，**代码独立实现，不复用注入路径**。
- **objc2（NSPasteboard changeCount）**：objc2-app-kit 已在依赖中，需加 `NSPasteboard` feature；`arboard` 不暴露 changeCount，此处直调。
- **AVSpeechSynthesizer（系统语音）**：需新增 `objc2-avf-audio` 依赖。阶段 0 仅 speak/stop + `isSpeaking` 轮询（带启动锁存与启动超时）；阶段 3 实现 delegate（objc2 `define_class!`）以准确区分完成/停止/取消/失败，按 objc2 官方模式实现并单测。
- **云端音频播放**：reqwest + rodio（需新增依赖），纯 Rust 路径。云端流式响应的实际格式（PCM / WAV / MP3 / 分块编码）决定能否渐进播放，在阶段 4 原型确认后再定播放方案。
- **线程约束**：AX 调用固定单线程串行（专用 worker 线程）。AppKit 侧按对象区分，不搞一刀切——判据是 objc2 是否将该类标为 `MainThreadOnly`：
  - `AVSpeechSynthesizer`：经 `run_on_main_thread` 在主线程访问。
  - `NSPasteboard`：**⚠️ 实测修正** objc2 未标记为 `MainThreadOnly`（对照 `NSWindow` 有该标记），`generalPasteboard()` 也是无需 `MainThreadMarker` 的安全函数；现有 `injector/clipboard.rs` 经 arboard 同样在非主线程操作。且 Copy 路径要轮询 changeCount 最长 300 ms，放主线程等于卡住 UI 那么久。因此剪贴板操作留在 selection worker 线程。
- **⚠️ `run_on_main_thread` 无法取消**：闭包一旦入队就一定会执行，调用方超时放弃等待也拦不住。凡是有副作用的主线程闭包（尤其 speak）必须自己携带取消 token 并在执行前复查，否则会出现"调用已失败/已取消，但稍后仍开始朗读"。阶段 3 换 delegate 时这条约束依旧成立。

### 3.3 架构组件

- **HotkeyManager 多动作路由**：一个系统级监听器（现有 `rdev::grab`），动作映射（`Dictate` / `ReadSelection`），设置时冲突检测，事件先解析为动作再路由到对应 Session。保留听写的轻点/长按/双击语义；TTS 首版单击语义：空闲=朗读、合成中=取消、播放中=停止；Escape 仅在 TTS 活动时生效。**任何阶段都不创建第二个全局键盘 hook**。
- **SelectionReader**（平台无关接口）：`read_selection(context) -> SelectionResult { text, source(AX/AXRange/ClipboardCopy), app_info, timing, diagnostics }`。macOS 首期实现；核心逻辑不依赖 AX 类型。
- **TtsBackend**（统一控制接口，自阶段 0 存在最小形态）：

```text
TtsBackend
  list_voices()
  start(request, cancel_token)
  stop()
  status()
├── MacSystemBackend
│     AVSpeechSynthesizer 直接朗读；不产音频 bytes，不经 SpeechPlayback
└── CloudBackend（阶段 4 引入）
      CloudTtsProvider：合成，返回 stream/bytes（capabilities/list_voices/synthesize/cancel、
        最大文本长度、流式与 SSML 声明、标准化错误）
      SpeechPlayback：rodio 播放（开始/停止/完成/失败事件；与 History 录音回放完全分离）
```

  系统后端不被强迫生成音频 bytes；云端后端内部组合合成与播放。前端引擎/音色选项来自共享定义（沿用 ASR/LLM 供应商模式）。History 录音回放与 TTS 播放是两条线，UI 与音频层不混用同一套"播放"语义。
- **TtsSession** 状态机：`Idle -> ReadingSelection -> Synthesizing -> Playing -> Idle`（系统语音路径合成与播放合并为"朗读中"），所有阶段可取消；新请求取消旧请求；**状态变迁写入结构化日志**，作为自动化断言接口。
- **⚠️ 会话所有权用单一代际槽表达**（阶段 0 已落地，阶段 3 的状态机在此之上生长）：一个 `AtomicU64` 记录当前拥有会话的代际（0 = 空闲），`claim()` 取得 `CancelToken`，`finish()` 只在仍持有时才释放。原先设想的"读取中布尔量 + 后端状态"两段拼接有窗口期两边都说不忙，第二次按热键会再开一轮取词而不是停止当前这轮。要点：
  - token 必须**贯穿到 backend**，不能只停在 controller——主线程闭包也要拿着它复查（见 §3.2）。
  - 后端一切终态（完成 / 失败 / 拒绝）都必须 `finish()`，否则会话永久卡在"朗读中"。
  - 键盘 hook 读该槽必须 lock-free：event tap 回调里加锁有导致 tap 超时被系统禁用的风险。
- **ASR/TTS 资源协调**（**已提前在阶段 0 落地**，原定阶段 3）：开始听写先停 TTS；录音中触发 TTS 则拒绝（`speak_err error=recording_active`，HUD 提示待阶段 3）；TTS 不申请麦克风权限；播放失败必须释放资源。提前的理由：一旦热键可用，朗读声会被麦克风录回去并转写，属于按一次就坏的交互。录音状态由 session actor 循环统一镜像出来，避免两边各自查询造成竞态。

### 3.4 隐私与安全边界（失败关闭合同）

**⚠️ 2026-08-12 决策：选区敏感度机制整体废除。** 原设计有 `Safe` / `Secure` / `Unknown` 三态，唯一用途是决定"这段文字敢不敢发给云端供应商"。产品决定不要这层判断：**用户选中并主动要求朗读的文字，就照读，本地引擎与云端引擎一视同仁。** 理由是这是单机工具，文字由用户自己选中、风险也由用户自己承担，不存在后台悄悄抓取的情形；而三态判定带来的复杂度、竞态与误挡（见下）远大于它的收益。

据此删除：`Sensitivity` 枚举、`FocusedElement::sensitivity()`、`SAFE_ROLES` 白名单、`SelectionOutcome.sensitivity`、`TtsRequest.sensitivity`、元素级 `AXSecureTextField` 拒读，以及原先"阶段 4 前四选一"的浏览器方案决策（连同它阻塞的云端接入，一并解除）。

**✅ 2026-08-12 已按本节实施完毕**，代码中不再有敏感度概念。

**保留一项，但理由是功能而非隐私**：`IsSecureEventInputEnabled()` 仍作为 **Copy 路径的前置检查**（实施时已从函数开头下移到 Copy 降级之前，AX 路径因此不再被它拦住）。系统处于安全输入状态时会直接丢弃合成按键事件，Cmd-C 根本发不出去；留着它是为了立刻回一个明确错误，而不是让用户等满 300 ms 超时再得到一个无法解释的失败。它不再表达"这段文字敏感"。

**废除前记录在案的两个缺陷**（保留作为决策依据，不再需要修复）：VS Code 冷启动与热启动会得出不同敏感度，同一段文字发不发云端取决于 Electron AX 树那一刻起没起来（时序竞态）；Terminal 整屏回滚命中 `AXTextArea` 白名单被判 `Safe`，而回滚里常有已打印的 token。两者都说明这套 role 白名单既不可靠也不严密——留着它反而给人一种其实并不存在的安全感。

**剪贴板失败关闭规则**（与上述无关，继续有效）：

- Copy 降级前必须成功快照所有声明为可恢复的 item/type；发现无法快照的类型（私有 UTI、file promise、延迟提供数据、超大数据）时**默认拒绝 Copy 降级**并返回明确错误，而不是覆盖后尽力恢复。
- 恢复承诺限定于冻结的类型集：纯文本、HTML、RTF、PNG 图片、文件 URL；不承诺任意剪贴板内容 100% 恢复。
- 有并发修改（changeCount 变化非本操作所致）时不覆盖新内容。

**数据留存**：

- 默认引擎为本地系统语音；切换到云端 Provider 时在设置页说明"选中文字将发送给该供应商"（一次性告知，不做逐次拦截）。
- 默认不保存选区全文与合成音频；普通日志只记长度、来源、目标应用、错误码。
- 诊断模式记录文本须用户显式开启并提示风险。
- 取消后不得继续下载、播放或把旧结果带入下一次会话。

## 4. 自动化验证策略

验证以自动化为主，人工只保留物理上无法自动化的残余项。

### 4.1 验证金字塔

| 层级 | 内容 | 执行方式 |
|---|---|---|
| L1 单元测试 | TtsSession 状态机（取消、重复触发、错误态）；热键动作路由与冲突检测；剪贴板快照/恢复逻辑（含失败关闭分支）；文本规范化；错误分类；云端请求构造与分段 | `cargo test`，CI 可跑 |
| L2 端到端选区验证 | 三层结构，见 §4.2 | 本机一条命令，输出 JSON 报告 |
| L3 残余人工清单 | 见 §4.4，共 5 项，多为一次性 | 显式清单，逐项打勾 |

### 4.2 端到端选区验证：三层结构

macOS TCC/辅助功能授权按**进程身份**计（`AXIsProcessTrusted` 检查的是当前进程），独立 CLI 通过不能证明签名安装的 VoiceX.app 通过。因此 L2 分为三层：

- **L2a 探针 CLI**（开发诊断用，不作发布门禁）：`selection-probe` bin，执行一次完整分层读取，输出 JSON：前台 app / Bundle ID、聚焦元素 role/subrole/支持属性、各路径成败与耗时、文本长度与 hash（默认不含全文，`--include-text` 供 fixture 比对）、剪贴板恢复状态、结构化错误。用于快速迭代 SelectionReader 算法。
- **L2b VoiceX 进程内诊断命令**（发布门禁数据来源）：签名安装的 VoiceX 进程暴露诊断触发方式（Tauri command / deep link / 开发者设置入口），以真实 TCC 身份执行同一 SelectionReader 并输出同构 JSON。该诊断能力保留至正式版，便于用户反馈"某应用读不到"。
- **L2c 链路级 E2E**（发布门禁数据来源）：harness 经 CGEvent 注入真实朗读快捷键，触发 VoiceX 完整链路，断言 TtsSession 结构化日志的状态序列、取消行为与 HUD 不夺焦点。

**harness 公共设施**：

- **驱动脚本**：`scripts/tts/` 下每个 P0 应用一个独立 driver：启动应用 → 载入 fixture 文本 → 程序化选中（Cmd-A / Shift+方向键 / 模拟鼠标拖选）→ **断言目标应用仍在前台**（否则该用例判 `invalid`，不计 pass/fail）→ 触发 L2b/L2c → 与 fixture 期望值比对。

**⚠️ 阶段 0 踩过的 harness 坑（阶段 1 的 driver 直接沿用结论；阶段 1 又踩出四条，见 §5.1 末尾）**：

- **按键必须用 CGEvent 注入到 HID tap，AppleScript 不行**。rdev 的 grab 在 `kCGHIDEventTap`，而 System Events 的 `key code` 投递在 session tap——HID tap 在链路更前面，**完全看不到** AppleScript 注入的键。阶段 0 因此写了 `scripts/tts/cgevent_key.py`（ctypes 直调 CoreGraphics，无第三方依赖）。
- **移动键盘焦点必须用真实鼠标事件**。System Events 的 `click at` 是 AX click，点得到元素但**不移动键盘焦点**：在 Safari 里点完页面再 Cmd-A，选中的仍是地址栏，复制出来的是 URL。需用 CGEvent 鼠标事件（`scripts/tts/cgevent_click.py`，已含拖选支持）。
- **Safari 不接受 AppleScript 导航到 `file://`**：`set URL of current tab` 会停在 `favorites://`。只能走 Launch Services（`open -a Safari FILE`）；而 `open` 会把页面塞进最前面那个窗口的 tab，所以要先 `make new document` 造一个自己的窗口，让"最前面"是我们的。
- **driver 的清理逻辑是最危险的代码，必须按唯一标记定位**。阶段 0 两次真实事故：`close document 1 saving no` 关的是最前面那个文档（可能是用户未保存的工作）；"关掉当前 tab 是 fixture 的那个窗口"会连带关掉同窗口内用户的其他 tab（实际弄丢过一个 3 tab 的窗口）。规则：每次运行生成唯一 RUN_ID 并嵌进 fixture，**只关带该标记的 document/tab**；只关 tab 不关窗口；只有自己创建且只剩空白页的窗口才可关闭；定位不到宁可残留。
- **断言要能证明读到的是预期文本**。只断言"取词成功"会被地址栏内容骗过——必须比对字符数或 hash。
- **fixtures**：中文、英文、中英混合、多行、标点/数字/URL/emoji、超长文本；负例：无选区、密码框（自建测试窗体）、焦点在 VoiceX 自身、剪贴板含不可快照类型。
- **报告**：汇总 JSON；`scripts/tts/evaluate_gate.py` 按 §4.3 输出 `GO` / `HOLD` / `INCOMPLETE` 及逐应用明细。
- **运行环境**：本机运行（需一次性授予终端与 VoiceX 权限）；CI 只跑 L1。

### 4.3 验收门槛（脚本判定）

**执行完整性规则**：

- P0 应用未安装或 driver 失效 → 门禁输出 `INCOMPLETE/HOLD`，**不得输出 GO**。
- P1/可选应用可 skip，不进入通过率分母。
- 报告必须输出：必测场景数、实际执行数、skip 数、invalid 数、成功率、每个 P0 应用的普通文本基线结果。

**`GO` 条件（数据来源为 L2b + L2c，L2a 不算数）**：

- P0 应用全部实际执行；每个 P0 至少一个普通文本基线场景通过；总场景通过率 ≥ 90%。
- 冻结类型集内剪贴板恢复率 100%（无并发修改时）；并发修改用例不覆盖新内容；不可快照类型用例正确拒绝降级。
- Copy 路径 ≤ 300 ms 或明确超时错误；AX 路径 P95 ≤ 100 ms。
- 空选区、焦点在自身、无前台应用三负例返回各自明确错误类型。

（原有的 `Secure` 泄露数与 `Unknown` 发送云端计数两条门槛已随 §3.4 敏感度机制废除而删除。）

未达门槛时先分类失败原因，不得以 OCR、静默剪贴板读取或其他防御性 fallback 掩盖（`HOLD` 路径）。

### 4.4 残余人工清单

1. **一次性系统权限授予**：终端与 VoiceX 的辅助功能 / 输入监控 / 自动化权限（macOS 强制人工）。*阶段 0 已在开发机完成。*
   **⚠️ 2026-08-12 更正**：此处原写"debug 构建的 ad-hoc 签名每次重建都变，TCC 授权随之失效需重新授予"——**这是错的**，与实际使用经验矛盾（开发中反复重建，听写与热键始终正常）。真实机制是：dev 构建是没有 bundle、ad-hoc 签名、无团队标识的裸二进制（`codesign -dvvv` 实测 `flags=0x20002(adhoc,linker-signed)`、`TeamIdentifier=not set`），它自身没有可被 TCC 记住的身份，于是 macOS 把权限请求归属到**责任进程（responsible process）**——从终端启动就用终端的那份授权。所以 cdhash 每次变确实是事实，但 TCC 根本没在看它，**重建不会导致授权失效**。
   由此，门禁只认 L2b/L2c 而不认 L2a 探针 CLI 的真实理由也要改：不是签名会变，而是**责任进程不同**，探针 CLI 与签名安装的 VoiceX 拿到的授权归属不是同一份。
   实际会踩的坑只有一个：**换一个启动器就要重新授权一次**（Terminal、iTerm、VS Code 集成终端、以及被其他 app 的进程树拉起来时各算一份）。授权列表按启动器增长，不按构建次数增长。
2. **默认值决策表确认**（§4.5）：不提出异议即视为按默认值执行。*阶段 0 按默认值执行，无异议。*
3. ~~**主观音质与延迟验收**~~ —— **✅ 已完成**。系统语音与火山 Seed-TTS 2.0 均已主观验收；火山的中英混排与情感取值也各听过一轮（§5.4）。
4. **不可脚本化应用抽查**（可选，非门禁）：微信、Word 等各做一次人工选中朗读。
5. ~~**听写功能回归 smoke**~~ —— **✅ 2026-08-12 已完成**。跨三轮开发的欠账清掉了：期间热键路由改动过三次（TTS 第二绑定与冲突检测、启动时读持久化配置、运行时 `apply_read_selection_hotkey`、以及后端按 provider 选择）。产品在完整使用中持续验证，听写注入与取消路径均无异常。

### 4.5 默认值决策表

以下默认值视为已确认，开发按此推进。标注 **2026-08-12 改** 的行是产品化转向时修订过的。

| 事项 | 默认值 |
|---|---|
| P0 应用名单（自动化） | Safari、Chrome、VS Code、TextEdit、备忘录、预览（PDF）、Terminal |
| P1 应用（人工抽查，非门禁） | 微信、Word |
| Copy 兼容模式 | 默认开启（受 §3.4 失败关闭规则约束）；HUD 标注文本来源；设置中可关闭。**✅ 阶段 1 已按 §5.1 复核并维持原默认值**：七个 P0 里只有 Safari 与 VS Code 依赖 Copy，其余五个走 AX——代价比阶段 0 担心的小（Chrome 不受影响，"关掉就没有浏览器支持"不成立），但关掉仍会失去 Safari 与 VS Code，因此设置项必须明确写出这两个应用会受影响，而不是笼统说"兼容性下降" |
| 默认朗读快捷键 | ⌥⌘R（避开系统"朗读所选内容"的 Option-Esc；可配置，冲突检测拦截与听写键重复） |
| 播放中再按快捷键 | 停止（暂停/继续后置；产品倾向于不加播放控制） |
| 云端 Provider 顺序 | **2026-08-12 二次改**：**首发确定为火山引擎 Seed-TTS 2.0**（接口已原型验证打通，见 §5.4）。之后逐个接其他常用供应商，常用的都支持后再调研遗漏。阿里云 DashScope（Qwen TTS / CosyVoice 系列）仍是候选，但不再是首发 |
| 云端音频格式 | **2026-08-12 新增**：mp3（实测 8 KB/s，等效 PCM 的 1/6），`symphonia` 流式解码，`cpal` 应用内播放。人工试听确认压缩不伤音质。详见 §5.4 |
| 首版语音参数 | **2026-08-12 二次改**：语速 + 音量 + 音调，均已在设置页落地。**音调是 macOS 本地专有项**——火山的 `audio_params` 只有 `speech_rate` / `loudness_rate`，选中云端 Provider 时隐藏该行。~~中英混排自动分段音色~~ **已取消**（§5.4 结论四） |
| 超长文本 | 系统语音不设限；云端按句分段、总量上限 5000 字符、超限先 HUD 提示再朗读前 5000 字符 |
| 朗读历史 | 不保存 |

## 5. 实施阶段

每阶段列出任务、**自动化验收**（脚本/测试判定）与**人工参与**（对应 §4.4 编号）。按"原型优先"顺序执行：阶段 0 先跑通端到端，阶段 1–3 工业化，阶段 4 接云端。

### 阶段 0：最小端到端原型（系统语音）— ✅ 已完成

目标：最短路径跑通"热键 → 取词 → Mac 系统语音朗读 → 停止"，验证整条链路成立。

已完成任务：
- ✅ **热键**：现有 `HotkeyManager` 同一 `rdev` 监听器内的第二个动作绑定（⌥⌘R → `ReadSelection`）；**未创建第二个全局 hook**。再按一次即停止。
- ✅ `selection` 模块最小实现：AX 直接读取 + Copy 兜底（当时含 `Secure` 拒读——该机制已于 2026-08-12 废除，见 §3.4；"快照失败即拒绝降级"第一天生效且继续有效）。
- ✅ `TtsBackend` trait 最小定义 + `MacSystemBackend`：speak / stop；`isSpeaking` 轮询，带启动锁存 + 启动超时。
- ✅ 关键事件结构化日志（`event=` 词表即测试契约，与 `scripts/tts/` 同步变更）。

计划外提前完成（理由见对应段落）：
- ✅ 会话取消语义与 `CancelToken`（§3.3）——原计划留到阶段 3，但阶段 0 的轮询实现本身就需要它才正确。
- ✅ Escape 停止朗读（仅在 TTS 活跃时生效，否则透传）。
- ✅ ASR/TTS 最小互斥（§3.3）。
- ✅ 热键冲突检测（与听写键相同则告警并禁用，避免静默失效）；配置化仍留在阶段 2。
- ✅ 非 macOS 平台不注册该热键（避免吞掉组合键却无后端）。

自动化验收结果：`cargo test` 178 项全绿；smoke 脚本在 TextEdit 与 Safari 上连续通过。实测数据：

| 应用 | 取词路径 | 敏感度 | 耗时 | 剪贴板 |
|---|---|---|---|---|
| TextEdit | `ax` | `safe` | 9–34 ms | 未触碰 |
| Safari | `clipboard_copy` | `unknown` | 105–127 ms | 快照/恢复完整 |

人工参与：#1 已完成；#5 未做（见 §4.4）。

**⚠️ 关键实测结论：Safari 走 Copy 降级，不走 AX。**（功能是通的——文字正确、发声正常、剪贴板还原；这里说的是取词路径，不是能力缺失。）

已确认：聚焦元素 role 为 `AXWebArea`；`AXSelectedText` 未产出文本，落到 Copy 降级。

阶段 0 遗留的 `unsupported` / `empty` 分叉**已在阶段 1 查清，结论见 §5.1**——并且原来那个二分法本身不足以定案，只看状态码会得出相反的结论。

**覆盖率提醒**：阶段 0 只测了 TextEdit 与 Safari 两个应用（计划要求如此）。P0 名单其余五个已在 §5.1 补测。

阶段 0 遗留的已知缺口（均按计划后置，非缺陷）：无 HUD 状态、无设置 UI、无云端、无语言检测（中文由系统默认语音朗读，若系统语言为英文则听感很差）、完成检测为轮询而非 delegate（极短文本可能在两次 poll 间讲完而被误报为 `start_timeout`）。

### 5.1 P0 七应用路径分布实测（2026-08-12）

方法与阶段 0 相同：`scripts/tts/p0_survey.sh` 逐个应用铺设 fixture、用真实鼠标事件把键盘焦点移进内容区、Cmd-A 全选、注入 ⌥⌘R，再从结构化日志读结果。为定案 AX 分叉，`selection/macos/` 新增 `event=selection_ax` 探针，记录 role/subrole、`AXSelectedText` 的分支与**原始 `AXError`**，并在降级路径上额外枚举元素实际公布的属性。

| 应用 | 取词路径 | 敏感度 | role | `AXSelectedText` | 原始 status | 公布 `AXSelectedTextRange` | 公布 `AXSelectedTextMarkerRange` | 耗时 | 剪贴板 |
|---|---|---|---|---|---|---|---|---|---|
| TextEdit | `ax` | `safe` | `AXTextArea` | text | 0 | 未探测 | 未探测 | 15 ms | 未触碰 |
| Safari | `clipboard_copy` | `unknown` | `AXWebArea` | empty | −25212 | **false** | **true** | 110 ms | 完整还原 |
| Chrome | `ax` | `unknown` | `AXWebArea` | text | 0 | 未探测 | 未探测 | 8 ms | 未触碰 |
| VS Code | `clipboard_copy` | `safe` | `AXTextArea` | empty | 0 | **true** | true | 110 ms | 完整还原 |
| 备忘录 | `ax` | `safe` | `AXTextArea` | text | 0 | 未探测 | 未探测 | 8 ms | 未触碰 |
| 预览（PDF） | `ax` | `unknown` | `AXGroup` / `AXSharedDocumentContainer` | text | 0 | 未探测 | 未探测 | 9 ms | 未触碰 |
| Terminal | `ax` | `safe` | `AXTextArea` | text | 0 | 未探测 | 未探测 | 8 ms | 未触碰 |

（"未探测"＝AX 已直接给出文本，没必要再花一次往返枚举属性，不等于不支持。）

> **注意**：上表"敏感度"一列是**当时**按 §3.4 旧规则算出来的历史测量值。该机制已于 2026-08-12 整体废除（见 §3.4），代码中不再有此概念。保留这一列是因为下面的结论三、四正是废除它的依据。

**⚠️ 结论一：Safari 的分叉答案是"两者都不是"，只看状态码会做出错误决策。**
状态码是 −25212（`kAXErrorNoValue`），按 §5 阶段 0 写的二分法应判为 `empty`，进而得出"属性可用、只是我们读错了元素，AX 路径对浏览器有救"。但属性枚举显示 `AXSelectedText` **根本不在 web area 公布的属性表里**（`has_sel_text=false`），`AXSelectedTextRange` 同样没有，只有 WebKit 自己那套 `AXSelectedTextMarkerRange`。WebKit 是对一个它并不提供的属性回了 NoValue 而非 Unsupported。
成立的是原计划 `unsupported` 那一支：**标准 AX 范围读取救不了 Safari**，要么改用 marker range API，要么继续走 Copy。原计划让人"跑一次 debug 日志看状态码"就开工，会直接走错方向；属性枚举才是定案依据。

**⚠️ 结论二：Chrome 与 Safari 完全不同，AX 直接可用。**
`AXWebArea` + `AXSelectedText` 直接返回文本，8 ms，无需 Copy，也**没有**动 §3.1 说的 `AXManualAccessibility`。计划里"不要把 Safari 的结论外推到 Chrome"的提醒是对的。**推论**：浏览器不可用的根因不是取词路径（Chrome 取词是全场最快的），而是 §3.4 把 `AXWebArea` 判为 `Unknown` 这条规则——即使取词完美，Chrome 一样上不了云端。§3.4 的四选一决策因此与取词路径无关，不能靠改进取词绕过。**（后续：该决策已作废——§3.4 的整套敏感度机制被废除，云端不再按来源拦截。）**

**⚠️ 结论三：VS Code 的 AX 可用性随状态漂移，敏感度判定因此不确定。**（**此结论已促成 §3.4 废除敏感度机制**；下面记录的取词侧事实仍然有效，隐私侧的处置要求已作废。）
同一应用同一操作，两次运行得到两种结果：

- 冷启动后首次：`event=selection_ax focused=none`——系统级根本取不到聚焦元素（Electron 的 AX 树尚未开启），敏感度 `unknown`。
- 之后：`AXTextArea`，`AXSelectedText` 存在但返回空串（status 0），且**公布了 `AXSelectedTextRange`**，敏感度 `safe`。

两次都落到 Copy 降级、文本都正确，所以功能上看不出来；但**敏感度从 `unknown` 变成了 `safe`**。按 §3.4，这正好是"禁止发送云端"与"允许发送云端"的分界。也就是说，同一段文字会不会被发给云端供应商，取决于 Electron 的 AX 树那一刻起来了没有——这是隐私规则里的竞态，不是可接受的不确定性。~~阶段 3/4 必须处理~~ —— 已改为直接废除敏感度机制（§3.4）：与其修一个既不可靠也不严密的判定，不如承认它挡不住什么。**取词侧仍需处理**：冷启动取不到聚焦元素是实打实的可靠性问题，需要 Electron 适配器。
另一面：VS Code 是全表唯一真正符合原计划 `empty` 诊断的应用（属性在、值为空、且公布 `AXSelectedTextRange`），AX 范围读取对它有现实机会。

**⚠️ 结论四：Terminal 判为 `safe`，但终端回滚缓冲区里常有机密。**（同上，**此结论已促成 §3.4 废除敏感度机制**，不再需要按 bundle id 打补丁。）
`AXTextArea` 命中 §3.4 的安全 role 白名单，于是整屏回滚（本次实测 429 字符，含提示符与历史输出）被判为 `safe`，按规则可以发往云端。`IsSecureEventInputEnabled()` 只覆盖"正在进行安全输入"的瞬间，挡不住历史输出里已经打印出来的 token、环境变量、密钥。role 白名单在终端类应用上是过宽的——这正说明它给的是一种并不存在的安全感，于是整套一并废除。

**路径分布小结**：七个 P0 里 **5 个走 AX、2 个走 Copy**（Safari、VS Code）。AX 路径 8–15 ms，Copy 路径 110 ms，均在 §4.3 的预算内（AX P95 ≤ 100 ms、Copy ≤ 300 ms）。两次 Copy 降级的剪贴板都完整还原。

**这不是门禁数据**：本轮是 §7 要求的路径调查，数据来自 smoke 级脚本而非 L2b/L2c，未覆盖负例（空选区、密码框、不可快照剪贴板类型），不得据此输出 `GO`。

**harness 侧新踩的坑**（阶段 1 的 driver 直接沿用）：

- **每条 AppleScript 都要显式 `with timeout`**。osascript 默认每个 AppleEvent 等 120 秒；冷启动中的应用不泵事件，于是 Preview 与 Terminal 各自把驱动卡了几分钟，一轮跑掉八分钟。现在统一经 `lib.sh` 的 `osa` 包一层短超时，并用 `app_ready` 轮询到应用真的应答为止——"进程起来了"不等于"能应答 AppleScript"。
- **"两个空值相等"是个会骗过守卫的 bug**。Terminal driver 本来用"新建窗口 id ≠ 前台窗口 id 就判 invalid"防止读到 harness 自己的终端；AppleScript 超时后两个 id 都是空串，`!=` 不成立，守卫静默放行，结果把别人的提示符当成 fixture 读了 44 字符还报成功。守卫必须先断言两边都非空。
- **Notes 不能边遍历 `whose` 结果边删**。`repeat with n in (notes whose …)` 配 `delete n` 不报错也不生效，第一轮因此在用户笔记库里留下了 fixture。改成反复 `delete item 1 of matches` 才真正删除。
- **Preview 用 System Events 关窗**，不用它自己的 `close document`：应用不应答 AppleEvent 时，正是最需要清理的时刻。按窗口标题里的 RUN_ID 定位。

### 阶段 1：测试基建与选区读取验证

已完成：
- ✅ P0 七应用路径分布实测与 `event=selection_ax` 探针（§5.1）；`scripts/tts/lib.sh` + `p0_survey.sh` 七应用 driver（含前台断言与 invalid 判定）。

剩余任务（按 §5.1 的结论调整过优先级；**✅ 标记的已于 2026-08-12 完成**）：
- ✅ `selection` 模块：多类型剪贴板快照/恢复与失败关闭分支已实现并单测；前台快照冻结、文本规范化、结构化错误分类均已落地。
- **AX 范围读取降级为按应用取舍，不再是通用第 2 层**：Safari 不公布 `AXSelectedTextRange`（做了也没用，要救只能走 `AXSelectedTextMarkerRange`）；VS Code 公布了，是目前唯一有现实收益的目标。先量收益再决定是否实现。
- ✅ **Electron 适配器已实现**：`AXManualAccessibility` 白名单（`com.microsoft.VSCode`、`com.vscodium.codium`）。根因是 Electron 在该属性置位前**不公布任何 AX 属性**，所以冷启动 `AXFocusedUIElement` 什么都取不到。双重门控：只在取词确实为空时问、每进程只问一次；被拒绝不算错误，继续走 Copy 降级。
  **不对 Electron 一概处理**——Chrome 无需干预，Claude Desktop（同为 Electron）实测也走 AX 直取 17 ms（§5.4）。**"Electron 需要专门适配器"这个笼统结论不成立**，能否 AX 直取是各应用自己 AX 树的属性。
- ✅ **L2c 链路级 E2E 已存在**（`p0_survey.sh` / `smoke_phase0.sh` / `negative_cases.sh`）。**L2a 不做、L2b 与门禁脚本推迟**，理由见 §7 第 6 条。
- ✅ harness 负例：焦点在自身、空选区（`negative_cases.sh`，断言具体错误码）。未覆盖：不可快照剪贴板类型、无前台应用。
- ✅ L1 单测：剪贴板快照预算与还原判定、changeCount 判定、规范化、错误分类。

自动化验收：✅ `cargo test --lib` 219 项全绿。门禁脚本判定 **推迟**——见 §7 第 6 条，它是发布流程而非产品。
人工参与：#1、#2 已完成；#4（微信 / Word 抽查）未做。

~~停止条件~~：原写"`HOLD/INCOMPLETE` 时不进入阶段 4/5"。**该条已随 2026-08-12 的产品化转向作废**——阶段 4 已完成且真机验证通过，门禁不再是它的前置。

### 阶段 2：热键多动作路由工业化（部分已完成）

剩余任务：动作映射正式化（阶段 0 的硬编码绑定演进为可配置）；TTS 快捷键设置与持久化；快捷键录制流程适配；底层监听与 ASR Session 解耦。

已完成：冲突检测（含听写键运行时改动后的重新评估）与其单测。

自动化验收：路由与冲突检测单测全绿；听写手势（轻点/长按/双击/Escape）语义由状态机单测覆盖。
人工参与：#5（听写 smoke）——已欠账，见 §4.4。

### 阶段 3：系统语音链路工业化 — ✅ 已完成

已完成：取消语义与 token 贯穿；Escape 停止；ASR/TTS 互斥；音色/语速/音量/音调参数化；`AVSpeechSynthesizerDelegate`（objc2 `define_class!`）替换轮询；HUD 状态显示（§5.3）。

**⚠️ delegate 实施时踩到的坑，值得记住**：启动一次朗读会先 `stopSpeakingAtBoundary` 停掉上一条，而那条的 `didCancel` 是**在新 utterance 注册之后**才异步送达的。不做身份匹配，它会把新注册从槽位里取走，于是新朗读的 `didFinish` 找不到归属、**会话永不释放**——热键从此只会"停止"。解法是按 utterance 地址匹配回调（只比较、绝不解引用；注册中的那条在可能触发回调期间一直被 synthesizer 持有，地址不会被复用）。`AVSpeechUtterance` 没有 `Send`，所以持有它不是选项；而协议要求 `Send + Sync` 也排除了把 delegate 类标成 main-thread-only。

轮询换成 delegate 后，极短文本误报 `start_timeout` 随之消失；原先那个"读完短文本后两秒内按热键是停止而不是开始"的用户可见异常也一并没了。`START_TIMEOUT` 保留为**看门狗**，只覆盖"引擎完全不回调"这一种 delegate 报不出来的情况。

自动化验收：✅ 单测全绿（含 delegate 类注册与协议一致性、被取代 utterance 的回调隔离）；✅ L2c 链路级 E2E 通过（`selection_start → selection_ok → speak_start → speak_started → speak_finished`，两条打断路径均无多余的完成/错误事件）。
人工参与：✅ #3 已完成（系统语音与火山均已主观验收）。

本阶段完成即达成"零密钥可用"的完整功能。

### 5.2 朗读设置页（2026-08-12 定稿，同日实施完毕）

**✅ 已实施**：`src/views/ReadingSettings.vue` + `/reading-settings` 路由 + 侧边栏"朗读"条目；后端命令在 `src-tauri/src/commands/tts.rs`。下表所列设置项（含"建议"两项）全部落地。

产品要求：**对齐现有 ASR 设置页的形态**——先选 Provider，再配置该 Provider 支持的选项。比 ASR 简单，没有历史记录之类的东西。

落点：新增路由 `/reading-settings` + 侧边栏条目（现有设置是独立路由页，不是页内标签页）。Provider 下拉从第一天就存在，初期只有"系统语音"一项，云端逐个追加时纯属增量。

**现成可复用**：热键录制走 `record_hotkey` / `apply_hotkey_config` + `formatHotkey`（照抄 `views/InputSettings.vue` 的流程）；热键冲突检测后端已有，UI 只需显示；本地音色数据源 `MacSystemBackend::list_voices()` 已实现（`AVSpeechSynthesisVoice::speechVoices()`）；`AppSettings` 是扁平结构，加字段是机械活。

| 设置项 | 取舍 | 备注 |
|---|---|---|
| Provider 下拉 | 必须 | v1 仅"系统语音" |
| 音色 | 必须 | **解决当前最痛的问题**：中文正在用系统默认音色朗读，系统语言为英文时听感很差 |
| 语速 | 必须 | 归一化 0–1 存储，UI 用 0.5x–2x 刻度 |
| 音量 | 必须 | |
| **试听按钮** | 必须 | 没有它调语速全靠盲猜；相当于 ASR 页的连通性测试 |
| 朗读热键录制 | 必须 | 与听写键冲突时要在 UI 上显示出来 |
| 启用选中朗读（总开关） | 必须 | 比"清空热键"更直观地临时关掉整个功能 |
| 音调 `pitchMultiplier` | 建议 | 系统语音原生支持 0.5–2.0，几乎零成本；原计划列为后置 |
| Copy 兼容模式开关 | 建议 | 放"高级"折叠区。文案须写明**关掉会影响 Safari 与 VS Code**（§5.1 实测结论），不要笼统说"兼容性下降" |

**明确不做**：朗读历史、输出设备选择、ducking、暂停/继续、逐词高亮。

**v1 不做但已知**：中英混排的音色问题。单个音色下拉解决不了——选中文音色念英文一样难听。真正的解法是按语言分段、每段一个 utterance 依次入队，需要一个语言分段器，是独立的一块工作，排在设置页与 HUD 之后。

### 5.3 HUD v1（2026-08-12 定稿，同日实施完毕）

**✅ 已实施，但有一处结论被推翻。** 下面"本地拿不到真实电平、v1 不做波形"的判断对**本地路径**依然成立，但**云端路径有真实电平**——PCM 就在我们自己的播放层里，播放回调本来就要遍历每个样本，顺手算 RMS 是零额外成本。于是 v1 就有了真波形，只是来源和当初设想的完全不同：不是 `willSpeakRange` 的伪动画，而是实际输出的电平。`TtsBackend::audio_level()` 用 trait 默认实现返回 `None`，系统语音诚实地报告"我不知道"，HUD 此时**隐藏波形条**而不是画一条不动的平线——停住的波形看起来是控件坏了，不是"当前很安静"。

**落地形态与定稿时的差异**：
- **紧凑呈现**（204×78）而不是流式转写用的大尺寸——一次朗读除了"它正在发生"没有别的信息可显示。
- **两个状态**：准备中（只有图标）与朗读中（图标 + 波形）。分两个状态是因为云端首包要 282–621 ms，那段静默与"热键没反应"无法区分。
- 图标用 Material Symbols `text_to_speech` 原样引入，**不与听写的麦克风共用**——朗读和听写方向相反，不该共享符号。
- chip 显示「朗读」，超长截断时显示「朗读 · 已截断」。
- 取词失败（无选区/缺权限/安全输入/焦点在自身）会显示出来，此前只有日志里有。

**⚠️ 实施时踩到的两个坑**：
1. **电平量纲叠加**。HUD 已有 `sqrt(level) × 6.5` 的显示映射（按麦克风 RMS 调的），播放层又乘了一次增益，结果全程顶格。播放层现在报**纯 RMS、不加增益**——与 `audio/capture.rs` 报的是同一个量，一个事件字段一个含义；显示形状归 HUD 管，两边都做就会撞车。朗读用自己的显示系数 2.4，由实测分布定（median 0.061 / p90 0.127 / peak 0.146）而不是拍脑袋。
2. **可见性只在渲染时计算**。波形条的显隐原本只在 `renderTranscript()` 里算，而电平事件不触发渲染——于是整段朗读波形都是隐藏的，只在结束那一刻的收尾渲染里显示 400 ms（正好是 `HUD_LINGER_MS`）。"只在最后半秒闪一下"这个现象就是它的指纹。

**以下为定稿时的原始判断，保留作为决策记录：**

要求：第一版只要一个状态显示，表明"正在合成/朗读"即可。若能做成类似语音电平的波浪形更好。播放控制**倾向于不加**（暂停/继续等继续留在后置阶段）。

**好消息：波形组件已经存在。** `src/hud/hud.ts` 已有 `waveformBars` 元素、`icon-waveform` 图标，以及 `state:audio_level` / `state:audio_spectrum` 事件契约（听写时在用）；Rust 侧 emit 集中在 `services/hud_service.rs`。TTS 复用这套即可，不需要新做 UI。

**⚠️ 电平的技术限制与对策**：`AVSpeechSynthesizer.speak()` 直接推到输出设备，**拿不到音频缓冲，本地路径没有真实电平**。要真电平必须改用 `write(_:toBufferCallback:)` 自行渲染再自行播放，等于重构本地后端——v1 不做。

**采用的替代方案**：用 `AVSpeechSynthesizerDelegate` 的 `willSpeakRange` 事件驱动波形，每念到一个词跳一次。这是与实际语音同步的真实进度，不是假动画；而且 delegate 改造本来就在阶段 3 待办里（它同时修掉极短文本被误报 `start_timeout` 的 bug），一份工作办两件事。**因此 delegate 与 HUD 应当合并为同一步实施。**

### 5.4 火山引擎 Seed-TTS 2.0 原型验证（2026-08-12 实测）

产品选定火山引擎（字节豆包语音）为第一家云端 Provider。以下全部是真实请求打出来的，不是文档摘抄。

**接口选型：单向流式 HTTP。** 三个候选里——单向流式 HTTP、单向流式 WebSocket、双向流式 WebSocket——选第一个。**双向流式排除**：它解决的是"文本边生成边合成"（例如把 LLM 流式输出读出来），而选中朗读的文本在请求发出前就完全确定，用它等于为不存在的问题付连接生命周期管理的成本。**WebSocket 排除**：与 HTTP 在音质和首包延迟上没有差别，却要多管连接建立、心跳、重连、协议分帧；我们同时还要从零建播放层，能少一层是一层。

```
POST https://openspeech.bytedance.com/api/v3/tts/unidirectional
Headers: X-Api-Key: <API Key>
         X-Api-Resource-Id: seed-tts-2.0
         X-Api-Request-Id: <可选，追踪用>
Body:    { "user": {"uid": "..."},
           "req_params": { "text": "...", "speaker": "zh_female_vv_uranus_bigtts",
             "audio_params": {"format":"mp3","sample_rate":24000,
                              "speech_rate":0,"loudness_rate":0} } }
响应:     chunked，每行一个 JSON：{"code":0,"message":"","data":"<base64 音频>"}
          结束片 {"code":20000000,"message":"OK","data":null}
```

**⚠️ 两个文档没写清、只能靠试出来的坑：**

1. **控制台给的实例 id 不是 resource id。** 实例 id 形如 `TTS-SeedTTS2.0<19位数字>`，直接当 `X-Api-Resource-Id` 用会返回 `45000030 requested resource not granted`。正确取值就是字符串 `seed-tts-2.0`。
2. **Seed-TTS 2.0 有自己的音色命名空间 `*_uranus_bigtts`，与经典的 `*_moon_bigtts` / `*_mars_bigtts` 完全不通用。** 用经典音色会返回 `55000000 resource ID is mismatched with speaker related resource`——这个报错**不是**说 resource id 错了，实测 10 个经典音色全部如此，换成 `zh_female_vv_uranus_bigtts`（Vivi）立刻成功。已知可用：`zh_female_vv_uranus_bigtts`、`zh_male_liufei_uranus_bigtts`。
3. **没找到可用的音色列表 GET 端点**（`/api/v1/tts/speakers`、`/api/v3/tts/speakers`、`/api/v3/tts/ListSpeakers` 均 404）。v1 **内置音色白名单**，不要依赖运行时拉取。

**✅ 真机端到端已验证（2026-08-12）**：完整走通「选中文字 → ⌥⌘R → 火山读出」，日志事件链为 `selection_start → selection_ok source=ax → speak_start backend=volcengine → speak_started → speak_finished`，全程零 WARN / ERROR / `speak_err`。
- **两条打断路径都正确**：热键再按一次得到 `speak_stop/speak_stopped reason=hotkey`，Escape 得到 `reason=escape`；**两次打断之后都没有多余的 `speak_finished` 或 `speak_err`**——取消契约最容易出的两种错（解码线程在停止后仍报完成、把被截断的流当错误报上来）都没有发生。
- 长文本 138 字符从 `speak_started` 到 `speak_finished` 历时 22 秒，说明 `wait_until_drained` 跟到了真实播放结束，覆盖了独立测试（7.5 秒）没覆盖的时长。
- **⚠️ 日志语义差异**：`speak_stopped` 在系统语音下是引擎真的确认停止（跨主线程调 `stopSpeakingAtBoundary`），在火山下只是停止标志置位、实际静音发生在下一次音频回调（约 10 ms 后）。

**⚠️ 顺带推翻一条 §5.1 的推论**：本次取词的目标应用是 Claude Desktop（`com.anthropic.claudefordesktop`，Electron），`role=AXGroup`、**走 AX 直取、17–18 ms**，没有降级到 Copy。而 §5.1 里同为 Electron 的 VS Code 必须走 Copy。**因此"Electron 需要专门适配器"这个笼统结论不成立**——能否 AX 直取取决于各家应用自己的 AX 树，不是运行时决定的。VS Code 的冷启动取不到聚焦元素仍是它自己的问题。

**实测性能**（音色 Vivi，mp3 24kHz 单声道）：

| 用例 | 字符 | 首包 | 全部收完 | 分片 | 传输量 | 音频时长 |
|---|---|---|---|---|---|---|
| 纯中文 | 37 | **419 ms** | 1121 ms | 21 | 64.7 KB | 8.09 s |
| 中英混排 | 77 | **282 ms** | 1165 ms | 23 | 72.6 KB | 9.07 s |
| 长文本 | 222 | **621 ms** | 5063 ms | 112 | 359 KB | 44.88 s |

**结论一：首包 282–621 ms，与文本长度基本无关**；加上取词的 8–110 ms（§5.1），按键到出声约 0.3–0.7 秒。
**结论二：合成速度约实时的 7–9 倍**（44.88 s 音频 5.06 s 收完）。余量极大，流式播放几乎不可能欠载，缓冲策略可以做得很简单。
**结论三：流式是必须的，不是优化。** 长文本"下完再播"要干等 5.06 秒才出声，流式是 621 ms；文本越长差距越大，而长文本正是选中朗读的典型场景。**因此不能采用"下载完整文件交给系统播放器"的方案。**

**⚠️ 结论四：中英混排单请求单音色即可，§7 第 4 步取消。** 77 字中英混排在单次请求、单个音色下合成完毕，无需任何分段处理，人工试听通过。原计划那个语言分段器（独立一块工作）**不做了**。

**格式与播放决策（2026-08-12 定）**：

- **音频格式用 mp3。** 实测 mp3 8 KB/s（64 kbps），等效 PCM 24kHz/16bit 单声道是 48 KB/s——**6 倍差距**，一次 45 秒朗读 359 KB vs 约 2.1 MB。人工试听确认 mp3 压缩不伤音质。代价是要自己做流式解码，**帧跨分片边界是这里最容易埋隐蔽 bug 的地方**，需要针对性测试。
- **解码器用 `symphonia`（仅启用 mp3 feature）**，不用 macOS 的 AudioToolbox——纯 Rust、无 C 依赖，且 Windows 上同样可用（CLAUDE.md 要求新功能考虑跨平台）。
- **播放在应用内做，用 `cpal`**（0.15 已在依赖里，目前仅用于输入设备枚举与采集），加一路输出流 + 环形缓冲。**不用 rodio**：它会锁定自己的 cpal 版本，与现有 0.15 撞版本是实打实的风险。**不走前端 webview 播放**：VoiceX 是托盘应用，主窗口经常关着，把核心功能的音频挂在 webview 生命周期上太脆，音频还要多过一遍 IPC。
- 现成可复用：`reqwest 0.11` 已带 `stream` feature、`base64 0.22`、`futures-util` 均在依赖里，只需新增 `symphonia`。

**参数映射**：语速走 `speech_rate`（实测可用区间 −50…+100，0 为中性；存储仍是归一化 0–1，由后端映射，`speech_rate = (倍率−1)×100` 正好对齐两端）。**音量不发给 API**，改在播放层做本地增益——即时生效、跨 provider 一致，且不依赖 `loudness_rate` 这个语义未文档化的参数（实测各取值对音频体积无可辨差异）。**火山没有音调参数**，音调因此是 macOS 专有项。

**⚠️ 设置归属决策（2026-08-12 定）：所有合成参数一律按 provider 独立，不区分"共用参数"与"独有参数"。**
最初的实现把语速/音量/音调放在共享位置、只有音色和凭证按 provider 分开，产品否决了这个划分。理由有二：一是**各引擎的响度与语速基线本就不同**，共享一个值等于逼用户在两个基线之间折中；二是**"共用还是独有"这个分类本身是负担**——每接一家 provider 就要重新争论一次，不如取消。这与 ASR 页早已成立的形态一致（连"语言"都是 `qwen_asr_language` / `google_stt_language_code` 各存一份）。

据此，`AppSettings` 分为三块：
- **功能级（与引擎无关）**：`ttsEnabled`、`ttsProviderType`、`ttsHotkeyConfig`、`ttsClipboardFallback`。热键与兼容模式属于"取词"，发生在合成之前。
- **系统语音**：`systemTtsVoiceId`、`systemTtsRate`、`systemTtsVolume`、`systemTtsPitch`
- **火山**：`volcTtsApiKey`、`volcTtsResourceId`、`volcTtsSpeaker`、`volcTtsRate`、`volcTtsVolume`

原先的 `ttsVoiceId` / `ttsRate` / `ttsVolume` / `ttsPitch` 已改名为 `systemTts*`。**改名会让旧值静默回落默认**（`#[serde(default)]`），当时字段刚落地一天、只有一个用户，成本约等于零；这类改名越晚越贵。

**⚠️ 情感参数：接口无法自证，只能靠听。** Seed-TTS 2.0 支持 `emotion` + `emotion_scale`（0–5）。诊断过程本身值得记下来，因为它会重演：三种放法（`req_params.emotion`、`audio_params.emotion`、`additions`）与**明显非法的取值**（`bogus_emotion_xyz`）**全都被静默接受、一律不报错**；而合成本身不确定——同一请求连跑 5 次音频体积极差 3264 字节，大于任何两个情感取值之间的差异。**字节比对因此完全无效**，任何"参数是否生效"的问题在这个接口上都只能人工试听定案。

**人工试听结论（2026-08-12）**：情感取值之间存在可辨差异，**参数确实生效**。但"不传 `emotion`"合成出来的语音**本身仍带一定情感色彩**——这是该音色的基线如此，不是参数失效。放置层级（`req_params` 还是 `audio_params`）尚未定案。

**当前处置：暂不实现，优先把端到端流程走稳。** 产品判断是先跑通再打磨。实现时的两条约束已经确定：默认必须是"不传该字段"（技术类文本带情感会很怪），以及**每个音色支持的情感列表各不相同**——而我们没有可用的音色列表接口，所以情感选项同样只能内置白名单、逐音色试出来。

**凭证**：与现有 ASR 的 `asr_app_key` / `asr_access_key` **不是同一份**，需在火山方舟控制台的语音合成大模型页面单独获取 API Key。存法与现有 ASR key 一致（`AppSettings` 扁平字段）。

### 阶段 4：云端后端

**⚠️ 2026-08-12 策略调整：改为"先接常用的几家，再调研补漏"。** 原计划是先做一次多家调研简报、冻结选型、再接入一家。产品决定反过来：先把心里已有的几个常用供应商逐个接上，等常用的都支持了，再去调研有没有遗漏。原先"阶段 4 前必须解决 §3.4 浏览器方案"这个前置条件**已随敏感度机制废除而解除**。

**2026-08-12 更新：第一家定为火山引擎 Seed-TTS 2.0，接口已原型验证打通，详见 §5.4。** 前置条件放宽：§5.2 设置页**已完成**，§5.3 HUD **不再是前置**（云端 Provider 只需要挂在设置页的 Provider 下拉里，那个下拉从第一天就在）。选区门禁 `GO` 仍是**对外宣布功能可用**的前提，但不阻塞云端接入开发。

每接一家的工作量：确认协议与输出编码 → 实现 `CloudTtsProvider` → 设置页追加该 Provider 的配置分组 → 试听打通。第一家还要额外把 `SpeechPlayback`（cpal 输出流 + 环形缓冲 + symphonia 流式解码）建起来——这一层 **provider 无关**，是所有云端后端的公共底座，应当先做。

任务：`SpeechPlayback` 播放层；`VolcengineBackend` 实现 `TtsBackend`（§5.4 的单向流式 HTTP）；按句分段与 5000 字符上限；连通性测试命令；取消、超时、标准化错误；设置页的火山配置分组与供应商告知文案。

**⚠️ 抽象风险**：`TtsBackend` 目前只有 `MacSystemBackend` 一个实现，而它是照着"从不产生音频字节、`start()` 返回即代表已受理"的形态设计的。云端后端形态完全不同（网络流 + 解码 + 播放 + 中途取消），**这个 trait 能不能扛住第二个实现尚未检验**。这正是提前接云端的收益之一：早撞早改，比在上面又叠三层之后再撞便宜得多。

自动化验收：本地 mock server 集成测试覆盖流式、取消、超时、错误映射、分段；真实 API smoke 测试（复用已配置密钥，断言返回音频且时长合理）；取消后无残留下载/播放（日志断言）。
人工参与：#3（云端音质与首包延迟主观验收一次）。

### 阶段 5：打磨与发布收尾

任务：L2b 诊断能力挂入设置页开发者入口；README 与设置页文案并列"语音输入 + 选中朗读"（双语词条），并说明与系统"朗读所选内容"的差异；隐私说明；`pnpm build` 类型检查；支持/非保证场景说明定稿。

自动化验收：`pnpm build` 通过；harness 全量回归 `GO`；`cargo test` 全绿。
人工参与：无新增。

### 后置阶段（不在首版）

- 暂停/继续、句级前进后退、流式播放优化、语言自动检测、发音替换规则、更多云端后端、可选朗读历史。
- Windows：UI Automation + Copy 降级实现 `WindowsSelectionReader`，复用 TtsBackend/Session/设置 UI；harness driver 体系同构迁移。

## 6. 主要风险与应对

| 风险 | 影响 | 应对 |
|---|---|---|
| AX 覆盖率不足 | 常用应用读不到选区 | harness 先行度量；Copy 降级；按失败分类处理而非掩盖。**✅ 已度量（§5.1）：7 个 P0 里 5 个走 AX、2 个走 Copy（Safari、VS Code），风险小于预期** |
| ~~浏览器选区恒为 `Unknown`~~ | ~~云端音色对读网页不可用~~ | **已消解**：§3.4 敏感度机制整体废除，云端不再按来源拦截 |
| harness 清理逻辑破坏用户数据 | 关掉用户未保存文档 / 标签页（阶段 0 真实发生过） | driver 只按唯一 RUN_ID 定位自己创建的资源；只关 tab 不关窗口；定位不到宁可残留 |
| 探针与正式应用 TCC 身份不一致 | 门禁误判（CLI 通过 ≠ 应用通过） | 三层验证；`GO` 仅基于 VoiceX 进程内数据（L2b/L2c） |
| 剪贴板恢复不完整 | 破坏用户剪贴板 | 失败关闭：不可快照即拒绝降级；恢复承诺限定冻结类型集；changeCount 判定；并发修改不覆盖；恢复先构造后清空（清空是破坏性步骤，任何失败都要发生在它之前）；可关闭兼容模式。~~TransientType 标记~~ 取词方向不可实现，见 §3.1 |
| UI 自动化 driver 脆弱 | 应用升级导致 harness 误报 | driver 按应用隔离；前台断言 + invalid 判定；P0 失效 → `INCOMPLETE` 而非静默跳过 |
| CI 无法授予 AX 权限 | harness 不能上 CI | 分层设计：L1 上 CI，L2 本机一条命令运行 |
| 快捷键改造影响 ASR | 听写行为回归 | 单监听器多动作路由（自阶段 0）+ 路由单测 + 一次人工 smoke（**尚未执行**） |
| 主线程闭包无法取消 | 取消后仍开始朗读 | 有副作用的闭包自带 token 并在执行前复查（§3.2） |
| TTS 与录音冲突 | 回声、资源争用 | 会话层互斥（听写优先），单测覆盖 |
| 云端首包延迟高 | 朗读体验迟钝 | 系统语音为默认基线；云端分段 + 流式（格式确认后实施） |
| 敏感文本外发 | 隐私风险 | 已按 §3.4 决策接受：默认本地引擎、切换云端时一次性告知、日志不记全文。不做逐次内容判定 |
| AVSpeech delegate 样板与内存风险 | 实现出错 | 阶段 0 用轮询锁存过渡；阶段 3 按 objc2 官方模式集中实现并单测事件映射 |
| macOS 特有代码扩散 | Windows 成本增加 | SelectionReader/TtsBackend/Session 平台无关，AX 类型不出模块 |

## 7. 直接下一步

阶段 0 已完成（commit `193e5e5`）。阶段 1 的 P0 路径调查已完成（§5.1）。分支 `feat/tts-selected-text-phase0`，未推送。

**产品方向已于 2026-08-12 转向"尽快产品化"**：原型阶段结束，常用应用的取词与合成都已验证可用，重心从"把取词做全"转为"把它变成应用里正式的一部分"。因此下面的顺序不再以选区门禁为主线。

**按此顺序实施**：

1. ~~**删除敏感度机制**（§3.4）~~ —— **✅ 已完成**。`Sensitivity`、`sensitivity()`、`SAFE_ROLES`、`SelectionOutcome`/`TtsRequest` 的字段、元素级 `AXSecureTextField` 拒读全部删除；`selection_ok` 日志不再有 `sensitivity=` 字段，`scripts/tts/` 的结果表同步去掉 `sens` 列。`IsSecureEventInputEnabled()` 保留但**下移到 Copy 降级之前**——AX 路径不再被它拦住，它现在只在"合成按键会被丢弃"时提前返回 `secure_input`。
2. ~~**朗读设置页**（§5.2）~~ —— **✅ 已完成**。`/reading-settings` 路由 + 侧边栏条目；`AppSettings` 新增 `ttsEnabled` / `ttsProviderType` / `ttsVoiceId` / `ttsRate` / `ttsVolume` / `ttsPitch` / `ttsHotkeyConfig` / `ttsClipboardFallback`。新建 `src-tauri/src/commands/tts.rs`（`list_tts_voices`、`preview_tts`、`stop_tts`、`apply_read_selection_hotkey`、`read_selection_hotkey_status`）。热键配置化与冲突的 UI 呈现一并完成，改键即时生效。语速按 0–1 存储（0.5 = 引擎默认 = 1x），UI 用 0.5x–2x；音调走引擎自己的 0.5–2.0，不做归一化。
   ~~**遗留**：试听是两个按钮~~ —— **✅ 已合并成单个开关**。当初拆成"试听 / 停止"是因为本地后端没有结束事件、做成开关必然显示错误状态；delegate 落地后有了结束事件，控制器在会话转为空闲时发 `tts:preview_ended`，设置页据此复位按钮。

   **验收状态**：`cargo test --lib` 188 项全绿（新增 6 项：热键状态区分"关掉"与"冲突"、冲突随听写键改动重算、空音色 id 视为引擎默认、参数按各自刻度进入请求、设置 blob 往返、老 blob 缺字段仍能加载）；`vue-tsc` 与 `pnpm build` 通过；中英两套界面已逐项目视核对。
   **尚未真机验证**（需要 app 窗口）：音色下拉能否真的填上、试听能否出声、改键后是否即时生效。注意验证时**必须从终端启动** `pnpm tauri dev`——从别的应用的进程树拉起来会因责任进程不同而拿不到辅助功能/输入监控授权（§4.4 #1），热键相关的验证会全部失败。
**⚠️ 2026-08-12 晚：3–5 步顺序已重排。** 产品要求"尽快把基本流程跑稳，然后尽快接在线合成服务"，且原因不只是偏好——见下面第 3 步开头那条硬约束。

3. ~~**播放层 + 火山接入**（阶段 4 与 §5.4）~~ —— **✅ 已完成**，真机端到端验证通过。以下保留当初的决策依据。**提到最前，因为本地路径的音质天花板是苹果的策略设的，不是我们代码写的**：实测本机 18 个中文音色全是 compact 档（`quality=1`），增强/高级档 0 个；而 `say` 默认听起来更好是因为它用的是 **Siri 音色，第三方应用永远拿不到**（不在 `AVSpeechSynthesisVoice.speechVoices()` 里）。换句话说，**再怎么调本地音色都没用，能选的里面就没有好的**。
   先做 `SpeechPlayback`（cpal 输出流 + 环形缓冲 + symphonia 流式解码），它 provider 无关、是所有云端后端的公共底座；再实现 `VolcengineBackend`；最后设置页加火山配置分组（隐藏音调那一行）。
4. ~~**delegate + HUD 最小状态**（§5.3 与阶段 3）~~ —— **✅ 已完成**（delegate 见阶段 3，HUD 见 §5.3）。**已瘦身**：`AVSpeechSynthesizerDelegate` 仍要做，但理由收窄为修 bug——极短文本误报 `start_timeout` 会让轮询等满 2 秒才释放会话，于是**读完一小段之后的两秒内再按热键是"停止"而不是"开始"**，这是用户能直接撞到的异常。
   **`willSpeakRange` 驱动波形的方案放弃**（§5.3）：它本来就是"本地拿不到音频缓冲"的妥协产物，而云端路径有真实音频流、有真电平，先做就是扔。HUD 这一步只做最小状态显示（"正在朗读" + 错误提示），波形等云端播放层建好后再接。
5. ~~**中英分段音色**~~ —— **已取消**。见 §5.4 结论四：火山单请求单音色处理中英混排，人工试听通过，语言分段器不需要了。
6. **阶段 1 剩余的验证基建** —— **2026-08-12 重新盘点，结论是现在不做**。

   **已完成**：负例 fixtures 两条（`scripts/tts/negative_cases.sh`：焦点在自身、空选区；各自断言**具体错误码**而不是"失败了"——"没选中文字"和"够不到这个控件"把用户指向完全不同的地方）；剪贴板失败关闭规则已单测（预算是跨类型累计而非单类型上限、溢出走饱和而非回绕成"还很宽裕"、有他人写入则跳过还原）。

   **L2c 其实一直存在**，此前的进度表低估了它：`p0_survey.sh` / `smoke_phase0.sh` / `negative_cases.sh` 干的正是 L2c 的事——CGEvent 注入真实热键 + 断言结构化日志序列。

   **剩下的三项建议这样处置**：
   - **L2a 探针 CLI：不做。** 计划自己写明它不计门禁，而它"快速迭代取词算法"的用途现在由 `p0_survey.sh` 承担。
   - **L2b 进程内诊断命令：✅ 已实现**（`diagnose_selection` 命令 + 朗读设置页的"取词诊断"卡片，在诊断模式开启时出现）。它跑的是**同一个 `read_selection`**——诊断走另一套实现的话什么也证明不了，所以取词过程中的观测被提成 `SelectionProbe` 一路带出来（含所有提前返回的失败分支，那正是最需要它的时候）。
     **报告只含事实、不含选中的文字**：前台应用、聚焦控件的 role/subrole、公布了哪些选区属性、走了哪条路径、耗时、剪贴板是否还原、以及当前的权限状态。字符数在结果里，原文不出进程。
     **有 5 秒倒计时**：点按钮会让 VoiceX 变成前台，立刻读只会得到 `focus_is_self`，所以要留时间切回目标应用。
   - **`evaluate_gate.py` 门禁脚本：等真要对外宣布时再做。** §4.3 那套门槛（场景通过率 ≥ 90%、剪贴板恢复率 100%、AX P95 ≤ 100 ms）是发布流程，提前做出来只会有一份没人看的报告。

   **未覆盖的负例**：不可快照剪贴板类型（需要一个真会提供 file promise 的应用来铺设，暂列人工抽查）、无前台应用（§4.3 点名的第三条，难以稳定构造）。

**始终欠着的一项**：人工清单 #5（听写回归 smoke，约 2 分钟）。**第 1、2 步实施时仍未执行，现在已欠三轮**：热键路由在第 2 步又改了一次（启动时读持久化配置、新增运行时 `apply_read_selection_hotkey`），单测覆盖到冲突判定与启用/停用，但真机听写从未验证。之所以不自动化：需要注入听写热键、录真实音频、发真实 ASR 请求，并把转写注入到当时的前台应用——副作用超出 smoke 应有的范围。
步骤：启动 VoiceX → 在任一输入框按住听写热键说一句 → 松开 → 确认文字照常注入；再按 Escape 确认取消路径正常。

**已作废、不要再做的事**：
- ~~敏感度三态判定重做~~ —— 整套机制删除（§3.4）。
- ~~阶段 4 前的浏览器 `Unknown` 四选一决策~~ —— 随上一条一并消失。
- **AX 范围读取**：已不是通用第 2 层，**建议整层不做**。对 Safari 无效（不公布 `AXSelectedTextRange`，只有 marker range），唯一有现实收益的目标是 VS Code，而 VS Code 走 Copy 也是通的（110 ms）。真要做也应先量收益。
  **2026-09-11 补充**：这条说的是通用的 `AXSelectedTextRange` 层，仍然不做。**WebKit 专用的 marker range 层已经实现**（`SelectionSource::AxMarkerRange`，`macos/ax.rs` 的 `selected_text_via_marker_range`）：焦点元素公布 `AXSelectedTextMarkerRange` 时，用参数化属性 `AXStringForTextMarkerRange` 直接取字符串，Safari 因此不再合成 ⌘C。起因、实测与边界见 `docs/selection-safari-failure-and-clipboard-fallback-2026-09-11.md`；探针脚本 `scripts/tts/marker_range_probe.sh`。
