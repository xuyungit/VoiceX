# VoiceX 没选中时读剪贴板 需求文档

> 文档状态：需求基线，M1 已实现
> 日期：2026-09-23
> 前置：选中朗读与翻译朗读已上线，见 `tts_plan.md`、`translate-read-requirements-2026-09-16.md`；本文只描述在其之上新增的能力

## 0. 决策摘要

| 主题 | 决策 |
|---|---|
| 功能形态 | 不加新热键。朗读键和翻译朗读键读不到选中文字时，改读剪贴板里的纯文本 |
| 开关 | 朗读页一个开关"没选中文字时读剪贴板"，两个朗读键共用，默认开 |
| 触发改读的情况 | 只在"这里没有可读的选中文字"时：没选中、控件不暴露选区、模拟复制没变化 |
| 不改读的情况 | 安全输入（密码框）、缺权限、焦点在 VoiceX 自身、剪贴板快照被拒、前台应用切换 |
| 后续流程 | 与选中文字完全相同：朗读前整理、翻译、音色、字幕、统计都不区分来源 |
| 敏感内容 | 复制方标记为 concealed（密码管理器）时拒绝朗读，文本不读入内存 |
| HUD | 标签注明来源："朗读剪贴板" / "翻译剪贴板"；改读后仍失败时文案以"读不到选中文字"开头 |
| 平台 | 与朗读键一致，只在 macOS 生效 |

### 0.1 曾考虑并放弃的方案

最初规划的是独立热键"朗读剪贴板"（默认 ⌥⌘V，默认关）。放弃的原因：

- 翻译朗读同样需要读剪贴板，独立热键要么再加一个"翻译剪贴板"键，要么翻译朗读用不上。
- 多一个键要记，⌥⌘V 还与访达"移到这里"冲突。
- "先复制再按朗读键"本来就是读不到选中文字时用户会自然尝试的做法。

## 1. 背景与目标

选中朗读要先从前台应用取到选中文字。有些位置取不到：兼容模式关闭时的 Safari、VS Code，终端，部分 Electron 应用。有时用户要听的内容本来就在剪贴板里，比如聊天应用"复制"按钮拿到的回答。

目标：

- 读不到选中文字时，朗读键和翻译朗读键都能改读剪贴板。
- 不改变有选中文字时的任何行为。
- 不读出密码，不在密码框里改读。

## 2. 范围与里程碑

| 里程碑 | 内容 | 说明 |
|---|---|---|
| M1 改读剪贴板 | 设置项、改读判定、剪贴板取词与 concealed 拒绝、HUD 来源标签与错误文案 | 本次实现 |
| M2 Windows | 打开 Windows 上的朗读快捷键时，一并实现 Windows 的 concealed 判定 | 跟随 Windows 朗读整体上线 |

## 3. 功能需求

### 3.1 何时改读剪贴板

选中取词失败后，开关打开且错误属于下表"改读"一栏时，读剪贴板；否则照旧报选中取词的错误。判定在 `controller.rs` 的 `falls_back_to_clipboard`，有单测。

| 选中取词错误 | 改读 | 理由 |
|---|---|---|
| `no_selection` | 是 | 没选中 |
| `unsupported_control` | 是 | 这个位置取不到选区，正是"先复制再按键"的场景 |
| `copy_timeout` | 是 | 模拟复制没有改变剪贴板，等同没选中 |
| `secure_input` | 否 | 密码框，剪贴板里很可能是要粘贴的密码 |
| `permission_denied`、`focus_is_self` | 否 | 用户需要去处理，不能悄悄换来源 |
| `clipboard_snapshot_refused`、`foreground_changed` | 否 | 可能确实有选中文字，改读剪贴板会读错东西 |
| 其他（`modifiers_held`、`cancelled` 等） | 否 | 不是"没有选区" |

取词后先检查会话是否已被取消，再决定改读，与原流程一致。

### 3.2 读剪贴板

- 只读不写，不模拟按键。兼容模式的 ⌘C 已在选中取词阶段结束并还原剪贴板，改读读到的是用户自己的剪贴板。
- macOS 直接读 `NSPasteboard.generalPasteboard`：类型中含 `org.nspasteboard.ConcealedType` 则拒绝，文本不读入内存；否则取 `NSPasteboardTypeString`。
- 其他平台经 `arboard` 取文本，取不到时看有没有图片，用来区分"空"和"不是文字"。
- 结果分类（`clipboard_text::classify`，有单测）与 HUD 文案：

| 情况 | 错误码 | HUD 文案 |
|---|---|---|
| concealed | `clipboard_concealed` | 读不到选中文字，剪贴板里是密码，未朗读 |
| 非空白文本 | — | 正常朗读 |
| 空白或什么都没有 | `clipboard_empty` | 读不到选中文字，剪贴板也是空的 |
| 有内容但不是文本 | `clipboard_not_text` | 读不到选中文字，剪贴板里也不是文字 |
| 读取失败 | `clipboard_unavailable` | 读不到选中文字，剪贴板也读取失败 |

剪贴板只作为改读来源使用，所以这些错误码的文案都带"读不到选中文字"。

### 3.3 朗读流程

- 控制器内部用 `ReadSource { Selection, Clipboard }` 标记文本来源，与 `ReadKind { Read, Translate }` 正交。之后的 `stage_text`、音色、字幕、`backend.start`、`record_read` 都不区分来源。
- 朗读键改读剪贴板时仍走朗读前整理（若开启）；翻译朗读键改读剪贴板时照常翻译，5000 字上限照旧。上限提示文案由"选中文字过长"改为"文字过长"。
- 统计：按键的种类计数，不区分来源。

### 3.4 HUD

- `state:reading` 事件新增 `source` 字段（`selection` / `clipboard`）。会话开始时是 `selection`，改读后 HUD 驱动线程在下一次轮询时发出 `clipboard`；前端每次收到事件都会重绘标签。
- 标签：朗读 + 剪贴板显示"朗读剪贴板"，翻译 + 剪贴板显示绿色"翻译剪贴板"。

### 3.5 日志

- 改读时依次记录 `selection_err`（原错误）、`clipboard_fallback`（after=原错误码）、`clipboard_ok` 或 `clipboard_err`。
- `speak_start` 新增 `source` 字段。`hotkey_action`、`speak_start`、`speak_end` 的 `kind` 仍是按键名 `read_selection` / `translate_selection`，现有脚本不受影响。

### 3.6 平台

朗读热键只在 macOS 绑定，改读逻辑在其后，自然只在 macOS 生效。Windows 的 concealed 判定（`ExcludeClipboardContentFromMonitorProcessing` 格式）在 M2 与 Windows 朗读一起实现。

## 4. 设置项

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `ttsClipboardWhenNoSelection` | bool | true | 没选中文字时读剪贴板，两个朗读键共用 |

- 旧设置没有该字段，读取时取默认值 true。
- 原"兼容模式"（`ttsClipboardFallback`）改名为"取词兼容模式 / Selection compatibility mode"，避免与本功能混淆，行为不变。

## 5. 涉及模块

| 模块 | 改动 |
|---|---|
| `commands/settings.rs` | 新字段、默认值、序列化测试 |
| `tts/clipboard_text.rs` | 新文件：剪贴板取词、concealed 拒绝、分类与测试 |
| `tts/controller.rs` | `ReadSource`、`falls_back_to_clipboard`、`read_clipboard_instead`、HUD 驱动跟随来源、`speak_start` 的 `source`、测试 |
| `services/hud_service.rs` | `ReadingSource`，`emit_reading` 带来源 |
| `src/hud/hud.ts`、i18n | 来源标签、错误文案、上限文案 |
| `src/views/ReadingSettings.vue`、`stores/settings.ts` | 快捷键卡片下方新开关；兼容模式改名 |

## 6. 不做的事

- 独立的剪贴板热键（见 0.1）。
- 判断剪贴板内容新旧：系统不提供复制时间，只能靠常驻轮询，不值得。HUD 标签注明来源，按任一朗读键可随时停止。
- 剪贴板历史、监听剪贴板变化自动朗读。
- 富文本（HTML/RTF）转换：只读纯文本，标记交给朗读前整理。

## 附录 A M1 验证记录（2026-09-23）

- `cargo test --lib`（`src-tauri`）393 项通过。新增测试覆盖：设置默认值与键名、改读判定、剪贴板分类与错误码、HUD 来源字符串。
- `pnpm build`（`vue-tsc --noEmit` + 构建）通过。
- 尚未做实机验证。需要在真机上验证：
  - 选中文字照常朗读，标签是"朗读"；
  - 不选中、剪贴板有文字时改读，标签是"朗读剪贴板"；
  - 在终端里复制后按朗读键；
  - 翻译朗读键改读剪贴板，标签是"翻译剪贴板"；
  - 剪贴板为空、为图片、为 1Password 复制的密码时的提示；
  - 在密码框里按键时不改读；
  - 关闭开关后恢复原有报错。
