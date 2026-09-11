# 选中朗读在 Safari 部分页面失效：分析、修复与验证

> 日期：2026-09-11。触发场景：Safari 中打开本机 DeepSeek Harness（`http://127.0.0.1:3080`，`@deepseek-ai/dsh` 0.1.5-rc.2），在右侧 Markdown 预览里选中文字后按朗读热键（⌃⌥⌘R），没有朗读；同一页面里的少数区域却能读。
> 本文记录已实测的事实、已实施的修复和仍属推断的部分，各条都标了依据。"朗读剪贴板"热键作为新功能另行排期（§5），本次未做。

## 1. 失败时的实际链路

用户提供的失败日志（VoiceX 结构化日志，`pnpm tauri dev` 的终端里）：

```
event=hotkey_action action=read_selection state=idle
event=selection_start
event=selection_ax role=AXWebArea subrole=- attr=empty status=-25212 enumerated=true has_sel_text=false has_sel_range=false has_marker_range=true has_value=true
event=selection_err error=no_selection
```

逐行对应 `src-tauri/src/selection/macos/mod.rs` 的取词链路：

1. 焦点元素是页面本身（`AXWebArea`），说明键盘焦点在页面里，不在地址栏或输入框。
2. `AXSelectedText` 返回 `kAXErrorNoValue`（−25212）。**WebKit 的 web area 对这个属性永远这样回答，与有没有选中无关**；它公布的选区属性只有 `AXSelectedTextMarkerRange`（`has_marker_range=true`）。这一点 `docs/tts_plan.md` §5.1 在 2026-08-12 就量到了。
3. 于是走 Copy 兼容模式：等修饰键抬起 → 快照剪贴板 → 合成 ⌘C → 轮询 `changeCount`，300 ms 内没变化即 `copy_timeout`。
4. 代码把"AX 说是空的 + 复制超时"合并报成 `no_selection`（`mod.rs` 里 `Err(CopyTimeout) if ax_reported_empty`）。所以日志里的 `no_selection` **不是**"页面没有选区"，而是"复制没在 300 ms 内落地"。这个合并对 web area 是错的：第 2 步的 `empty` 本来就不是证据。

用户复核过失败后剪贴板里也没有选区内容，即那次 ⌘C 根本没被 Safari 执行（不是迟到，是没落地）。

## 2. 关于 Harness 页面本身的核对

| 问题 | 结论 | 依据 |
| --- | --- | --- |
| Markdown 预览是不是 iframe | 不是，是主文档里的普通 React DOM（`MarkdownText`）；只有 `.html` 预览用 `sandbox` iframe | 读 `~/.npm/_npx/…/@deepseek-ai/dsh` 打包产物 |
| 页面是否拦截 `copy`/⌘C | 没有。全局 `keydown` 只处理 Escape；Lexical 编辑器的 `copy` 处理只作用在编辑器根节点内 | 同上 |
| pdf.js 的 `AnnotationEditorUIManager.copy()` 会在 document 级 `preventDefault` | 代码存在但**从未实例化**（`new AnnotationEditorUIManager(` 0 处），排除 | 同上 |
| 能读的那些区域是什么 | 推断是可编辑控件（聊天输入框等）。它们是 `AXTextArea`，公布 `AXSelectedText`，走第 1 层 AX 就成功；正文属于 web area，只能走 marker range 或 Copy | 与 §1 第 2 条一致，未逐一实测 |

所以"和页面设计有关"是对的，但关系不在"页面反复制"，而在**选区落在 web area 里还是落在可编辑控件里**。任何 WebKit 页面的正文都属前者；Safari 之前能读的页面，只是那些页面对合成 ⌘C 响应得够快。

为什么 Harness 页面上合成 ⌘C 没落地，**没有拿到直接证据**。候选解释：页面是 14M token 会话的单页应用，截图时还有后台任务在跑，WebContent 进程繁忙，合成按键排在事件队列后面；或者选区在按键到达前被流式重绘销毁。修复后 Safari 不再依赖 ⌘C，这个问题对 WebKit 不再成立，就没有继续追。

## 3. 已实施的修复

### 3.1 WebKit marker range 取词层（根因修复）

`src-tauri/src/selection/macos/ax.rs` 新增 `FocusedElement::selected_text_via_marker_range()`：读 `AXSelectedTextMarkerRange`，再用 `AXUIElementCopyParameterizedAttributeValue` 调参数化属性 `AXStringForTextMarkerRange` 取字符串。`macos/mod.rs` 在属性枚举发现 `has_marker_range=true` 时调用它，成功则以 `SelectionSource::AxMarkerRange`（日志 `source=ax_marker_range`）返回，**不合成按键、不碰剪贴板、不受安全输入影响**。`SelectionProbe` 增加 `markerAttribute`/`markerStatus` 两个字段，诊断报告能看到这一层的结果。

新增日志行：

```
event=selection_ax_marker role=AXWebArea attr=text status=0
event=selection_ok source=ax_marker_range …
```

**实测**（`scripts/tts/marker_range_probe.sh`，本机 Safari，固定页面含中英文段落、粗体/链接/内联图片、代码块、2×2 表格）：焦点元素状态与用户日志完全一致（`AXWebArea`，`AXSelectedText` −25212，仅公布 marker range），marker range 读出 191 字符完整文本，测试通过。

已知边界：

- WebKit 用 U+FFFC（object replacement character）代替图片等替换元素，已在进入 `normalize_text` 前剥掉（有单测）。
- 选区**恰好止于表格末尾**时，一次实测少了最后一个单元格（149 vs 151 字符）；止于段落时完整。属 WebKit 的 marker 定位行为，未处理，遇到再说。
- 只处理焦点元素自身公布 marker range 的情形。`.html` 预览那种 sandbox iframe 的选区在嵌套 web area 里，焦点元素是否就是那个嵌套 web area 未验证。

### 3.2 Copy 路径的迟到复制（数据正确性 bug）

`clipboard.rs` 原先在 300 ms 超时时直接返回并注释"剪贴板未被触碰"。但发出去的 ⌘C 不会被撤回：应用只是慢，稍后照样把选区写进剪贴板，而此时已没人还原快照，用户剪贴板被静默覆盖。本次 Harness 的失败并不是这种情况（剪贴板确认没被写），但这个 bug 是真实的。

修复：超时后另起线程 `voicex-late-copy` 再观察 2 s，迟到落地且期间无其他写入（沿用 `may_restore` 规则）就还原快照，无论结果都记 `event=copy_landed_late landed=<bool> restored=<bool>`。读取本身仍立刻以超时失败返回，`COPY_TIMEOUT_MS` 保持 300。

### 3.3 未改的部分

- marker range 读到空时仍会继续走 Copy（与之前行为一致，保守）。若之后确认 WebKit 的空 marker range 就等于没选区，可以在这里直接返回 `no_selection`，省掉一次合成 ⌘C。
- `no_selection` 与 `copy_timeout` 的合并规则未动。对 WebKit 它现在很少再被触发。

## 4. 如何复测

1. `pnpm tauri dev` 正在跑的话会自动重编译 Rust 改动；否则重启。
2. 回到 Harness 页面，在正文选中一段，按 ⌃⌥⌘R。期望日志出现 `event=selection_ax_marker … attr=text status=0` 和 `selection_ok source=ax_marker_range`。
3. 若仍失败，把 `selection_ax_marker` 那一行贴出来：`status=-25212` 表示 WebKit 认为没有选区（选区在按键前被页面销毁），其他状态码是 API 层的问题。
4. "取词诊断"卡片在朗读设置页里，**只在通用设置的诊断模式（`enableDiagnostics`）开启后才显示**；报告里的 `markerAttribute`/`markerStatus` 就是第 3 条那两个值。
5. 不依赖 VoiceX 的独立探针：`scripts/tts/marker_range_probe.sh`（需要终端有辅助功能与自动化权限；会自己开一个 Safari 窗口并关掉）。

## 5. 兜底方案：朗读剪贴板（待排期，本次未做）

选中朗读的三条路径（AX、marker range、Copy）都依赖目标应用的 AX 树或对合成按键的响应；PDF 阅读器、远程桌面、Canvas 应用、部分 Electron 应用仍会漏。补一个"朗读剪贴板"热键：用户自己 ⌘C 后按热键，VoiceX 读 `NSPasteboardTypeString` 朗读。

- 做进 VoiceX 而不是另做软件：朗读链路（Provider、音色、HUD、停止、与听写互斥）都在 `TtsController`，`TtsRequest::plain(text)` 就是"朗读任意文字"的入口。
- 读剪贴板不需要辅助功能权限、不合成按键、不受安全输入影响，是三条路径里唯一天然跨平台的（Windows 只需 `GetClipboardData`）。
- 默认热键建议 ⌃⌥⌘V，避开带 C 的组合。**不要**在选中朗读失败时自动改读剪贴板：无提示地读出旧内容既莫名其妙又可能泄露；失败 HUD 上提示一句即可。
- 落点：`hotkey/manager.rs`（`ReadClipboardPressed`，三个热键两两查冲突）、`selection/`（`SelectionSource::Clipboard`，`clipboard.rs` 只读函数）、`tts/controller.rs`（复用 `start()` 的骨架只换取词一步）、设置页与 i18n（`clipboard_empty` 文案）。
