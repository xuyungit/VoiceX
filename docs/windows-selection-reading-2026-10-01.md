# Windows 选中朗读：技术分析与实现

> 文档状态：分析 + 首版实现（代码已合入分支，**尚未在 Windows 真机验证**）
> 日期：2026-10-01
> 前置：`tts_plan.md`（macOS 方案与 §3.4 失败关闭合同）、`clipboard-read-requirements-2026-09-23.md`（M2 Windows concealed）、`selection-safari-failure-and-clipboard-fallback-2026-09-11.md`

## 0. 结论摘要

| 问题 | 结论 |
|---|---|
| 能否开放给 Windows | 可以。会话控制、云端合成与播放（cpal + symphonia）、HUD、热键监听本来就跨平台；真正缺的只有**取词**和**剪贴板**两块平台代码，外加 Windows 没有系统语音 |
| 取词怎么做 | 与 macOS 同构的两层：**UI Automation `TextPattern` 选区**（无副作用）→ 取不到时**模拟 Ctrl+C + 全格式快照还原**（可关闭的兼容模式） |
| 剪贴板的难点 | Windows 剪贴板是全局锁、有延迟渲染、有 GDI 句柄格式和系统合成格式。快照按格式分类处理，拿不准就拒绝（失败关闭），读取与还原在同一次 `OpenClipboard` 内完成 |
| 必须新增的 Windows 专属处理 | ① 吞掉含 Alt/Win 的热键后要注入"菜单遮罩键"，否则会弹开始菜单/激活菜单栏；② 前台应用以管理员身份运行时直接报 `target_elevated`（UIPI 会让读取失败、让 Ctrl+C 被静默丢弃）；③ 焦点在密码框时报 `secure_input` |
| 系统语音 | Windows 不提供（`say` / AVSpeech 是 macOS 专有）。选"系统语音"时设置页给警告，按热键时 HUD 明确报错，**不会**自动改用云端引擎 |
| 风险 | 未在真机验证；UIA 查询会让 Chromium/Electron 应用打开无障碍树（VS Code 可能因此进入屏幕阅读器优化模式，待实测）；默认热键与 Xbox Game Bar 录制快捷键重合 |

## 1. 现状：为什么之前只开 macOS

朗读链路 = 热键 → 取词 → （可选 LLM）→ TTS 后端 → 播放 → HUD。逐段看平台依赖：

| 环节 | 之前的平台状态 |
|---|---|
| 热键监听（`hotkey/manager.rs`，rdev `grab`） | 跨平台，Windows 上是 `WH_KEYBOARD_LL` 低级钩子，听写早已在用 |
| 取词（`selection/`） | **仅 macOS**：`macos/ax.rs`（AX API）+ `macos/clipboard.rs`（NSPasteboard + CGEvent Cmd-C）；其他平台返回 `platform_unsupported` |
| 读剪贴板（`tts/clipboard_text.rs`） | macOS 直读 NSPasteboard 并识别 concealed；其他平台经 `arboard`，**没有** concealed 判定（需求文档 M2 明确要求 Windows 上线前补上） |
| 云端 TTS（火山 / 阿里 / MiMo / Azure / Edge） | 跨平台，纯网络 + cpal 播放 |
| 系统语音（`mac_say.rs` / `mac_system.rs`） | 仅 macOS |
| HUD / 会话 / 统计 | 跨平台 |
| 热键绑定（`lib.rs`、`commands/tts.rs`） | 用 `cfg(target_os = "macos")` 关掉了，理由是"没有取词实现时注册热键只会吞掉按键" |

所以"开放给 Windows"实际是：补齐取词与剪贴板两块平台代码，放开热键绑定的 cfg，再处理 Windows 没有系统语音这一点。下面逐项分析。

## 2. 取词 API 选型

| 方案 | 覆盖 | 副作用 | 结论 |
|---|---|---|---|
| **UI Automation**（`IUIAutomation` + `TextPattern`） | Win32 Edit/RichEdit（系统自带代理）、记事本、写字板、Word、WPF/WinUI 文本、Windows Terminal 与控制台、Edge/Chromium 网页（原生 UIA 的 `TextPattern`） | 只读查询；但会让 Chromium/Electron 打开无障碍树（见 §3.5） | **第一层**，对应 macOS 的 AX |
| MSAA / IAccessible2 | 老控件；IA2 有选区接口但需逐应用适配 | 同上 | 不做。UIA 已经通过 MSAA 代理覆盖老控件，IA2 收益不抵复杂度 |
| `SendMessage(WM_COPY)` | 只有标准 Edit 控件响应 | 动剪贴板 | 不做。覆盖面是 UIA 的子集 |
| **模拟 Ctrl+C** | 几乎所有能复制的地方（Electron、表格单元格、自绘控件、Java 应用） | 动剪贴板，需快照还原 | **第二层**，对应 macOS 的 Cmd-C 兼容模式 |
| OCR / 屏幕取词 | 图片、Canvas | 慢、需要识别模型 | 与 macOS 一致，不在范围内 |

### 2.1 UIA 读取细节（`selection/windows/uia.rs`）

- **从焦点元素向上找 `TextPattern`**。焦点元素常常不是持有文字的那个：网页里焦点是链接或文档本身，Word 是文档，嵌套 Web 视图里可能是更深的节点。沿 `ControlViewWalker` 向上最多 32 层，取第一个有 `TextPattern` 的祖先，调用 `GetSelection()`，每个范围 `GetText(-1)`（不截断），多段选区按顺序换行拼接。macOS 上 Safari 的教训（§5.1：状态码会说谎，要看属性表）在这里对应的是"焦点元素没有 pattern 不代表读不到"。
- **区分"空"与"不支持"**，与 macOS `AttributeRead` 同一套词汇（`text` / `empty` / `unsupported`）：控件明确说没选中 → 兼容模式复制超时时报 `no_selection`；控件不回答 → 报 `unsupported_control`。
- **线程与 COM**：UIA 客户端放在既有的 selection worker 线程上，首次使用时 `CoInitializeEx(MTA)`，`CUIAutomation8` 实例按线程缓存。不能放在有窗口的线程上（UIA 会向目标发跨进程消息，目标若是本进程窗口会死锁）。这正好沿用了 macOS AX 必须单线程串行的设计。
- **超时**：UIA 默认连接 2 s、事务 20 s，一个卡死的目标会让 HUD 停在"准备中"半分钟。通过 `IUIAutomation2` 设为 1 s / 1.5 s。某一层调用超时就直接停止向上回溯（否则每一层都要再等一次超时）。
- **密码框**：焦点元素 `IsPassword` 为真 → `secure_input`。这是 macOS"安全输入"的对应物，且沿用同样的规则：不改读剪贴板（剪贴板里很可能是要粘贴的密码）。
- **提权进程**：见 §3.3。

### 2.2 各类应用的预期路径（**待真机核对**）

| 应用 | 预期路径 | 说明 |
|---|---|---|
| 记事本（Win11）、写字板 | UIA | RichEdit 提供 `TextPattern` |
| Word | UIA | 文档区域提供 `TextPattern`，多段选区按段拼接 |
| Edge 网页 | UIA | Chromium 原生 UIA 提供文档级 `TextPattern`；焦点在链接上时靠向上回溯 |
| Chrome 网页 | UIA 或 Copy | 取决于该版本是否启用原生 UIA；未启用时经 MSAA 代理没有 `TextPattern`，落到 Copy |
| VS Code / 其他 Electron | 多半 Copy | Monaco 只在隐藏 textarea 里放选区片段，UIA 能否拿到完整选区不确定；Copy 兜底 |
| Windows Terminal、cmd/PowerShell 控制台 | UIA | 终端实现了 `TextPattern` 选区 |
| Excel 单元格区域 | Copy | 单元格网格不提供文本模式；Copy 得到制表符分隔文本（朗读前整理会处理表格） |
| 微信、QQ 等自绘界面 | Copy | 不暴露选区 |
| 以管理员身份运行的应用 | `target_elevated` | VoiceX 未提权时 |
| 远程桌面（mstsc 等） | 非保证 | 与 macOS 一致列为非保证场景：Ctrl+C 进到远端，剪贴板同步是异步的，可能超时；迟到复制有还原兜底 |

## 3. 剪贴板：与 macOS 的对应关系与 Windows 特有难点

### 3.1 对照表

| macOS 概念 | Windows 对应 | 说明 |
|---|---|---|
| `NSPasteboard.changeCount` | `GetClipboardSequenceNumber()` | 无需打开剪贴板即可读取；每次 `EmptyClipboard` / `SetClipboardData` 都会变 |
| pasteboard item × type | 剪贴板格式（`EnumClipboardFormats`） | Windows 没有"多个 item"的概念，只有一组格式 |
| promise 类型（拒绝） | 延迟渲染（`SetClipboardData(fmt, NULL)`） | 不同：Windows 读延迟格式会让所有者**当场渲染**，能拿到数据，代价是时间（见 §3.2） |
| `org.nspasteboard.ConcealedType` | 注册格式 `ExcludeClipboardContentFromMonitorProcessing` | 微软文档化的约定，KeePass、1Password 等会设置 |
| CGEvent Cmd-C（HIDSystemState 源） | `SendInput` Ctrl+C（虚拟键 + 扫描码） | 见 §3.4 |
| `IsSecureEventInputEnabled` | 无全局等价物；改用焦点元素 `IsPassword` | |
| TCC 辅助功能权限 | 无需权限；但有 UIPI（§3.3） | |

### 3.2 快照与还原（`selection/windows/clipboard.rs`、`selection/windows_rules.rs`）

Windows 剪贴板的几个坑，以及各自的处理：

1. **全局锁**。同一时刻只有一个进程能 `OpenClipboard`，剪贴板管理器、`rdpclip`、系统剪贴板历史服务都会短暂占用。打开时短间隔重试（快照 500 ms 预算，复制落地后 1 s 预算），并且**快照完立即关闭**——否则目标应用写不进来。
2. **读取与还原在同一次打开内完成**。复制落地后打开剪贴板（打开成功即说明目标应用已写完），读 `CF_UNICODETEXT`，紧接着在同一会话里还原。剪贴板打开期间别人写不进来，所以 macOS 需要用 `changeCount` 比对来防"剪贴板管理器在读与还原之间写入"的那个窗口，在 Windows 上不存在。
3. **格式不都是内存块**。按格式分类（`plan_formats`，有单测）：

   | 格式 | 处理 |
   |---|---|
   | 普通内存格式与所有注册格式（文本、`HTML Format`、`Rich Text Format`、`PNG`、`CF_HDROP` 等） | 拷贝字节，原样写回 |
   | `CF_DIB` / `CF_DIBV5` | 系统会互相合成，只保留**先列出的那个**（即写入方自己放的），避免一张截图存两份 |
   | `CF_BITMAP`、`CF_PALETTE` | GDI 句柄，由保留的 DIB 合成；没有 DIB 时拒绝 |
   | `CF_ENHMETAFILE` | GDI 句柄，用 `GetEnhMetaFileBits` 存成字节，`SetEnhMetaFileBits` 还原 |
   | `CF_METAFILEPICT` | 由增强图元文件合成；单独出现时拒绝 |
   | `CF_OWNERDISPLAY`、`CF_DSP*` 句柄格式、私有句柄区 `0x200–0x2FF`、GDI 对象区 `0x300–0x3FF` | 无法如实捕获，**拒绝兼容模式**（失败关闭） |

   格式按 `EnumClipboardFormats` 的顺序写回——应用粘贴时按顺序取第一个认得的格式，顺序丢了会改变粘贴结果。
4. **延迟渲染的代价**。快照读每个格式时，若所有者用了延迟渲染，它要当场渲染。Excel 复制大区域后，渲染位图/图元文件可能要好几秒。处理：总字节上限 32 MB（与 macOS 一致）+ **1.5 s 时间预算**，超出即拒绝，不让朗读被渲染用户旧剪贴板拖住。
5. **还原前先备齐所有句柄**。所有 `HGLOBAL` / 图元文件句柄在 `EmptyClipboard` 之前就分配好，能失败的步骤都在清空前失败；`SetClipboardData` 失败时未交出的句柄自动释放。
6. **迟到的复制**。300 ms 内序列号没变就报 `copy_timeout`，但 Ctrl+C 仍在目标队列里；后台线程再盯 2 s，复制一落地就还原（与 macOS 同一机制）。
7. **剪贴板历史（Win+V）**。目标应用的那次复制必然进历史；还原按原样写回，也会再进一条。这是已知副作用，已写进设置页说明。曾考虑给还原写入加 `CanIncludeInClipboardHistory=0`，放弃原因：还原内容本就是用户当前剪贴板，排除后 Win+V 顶部与实际剪贴板不一致，反而更难理解；同时也不碰云剪贴板同步的行为。

### 3.3 UIPI 与提权进程

用户界面特权隔离（UIPI）禁止普通进程向高完整性级别进程发送输入、读取其 UI。后果：UIA 读不到；`SendInput` 的 Ctrl+C 被**静默丢弃**（返回值和 `GetLastError` 都不提示），用户只会看到一个莫名的复制超时，开着"没选中时读剪贴板"时还会读出旧剪贴板。

处理：取词开始时用 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `TokenElevation` 判断前台进程与 VoiceX 自身是否提权；只有"目标确定提权、自己确定未提权"才报 `target_elevated`（HUD：「目标应用以管理员身份运行，无法取词」），判断不了时照常尝试，不猜。该错误不改读剪贴板，与 `permission_denied` 同类——需要用户处理。

### 3.4 合成 Ctrl+C

- **等修饰键松开**（最长 3 s，可被取消）。用 `GetAsyncKeyState` 查 Shift / Ctrl / Alt / Win。不等的话 Alt 还按着时发出的是 Ctrl+Alt+C，在很多键盘布局上就是 AltGr+C。
- **复核前台应用**未变，同 macOS。
- **按虚拟键 `VK_C` 发送**，附带 `MapVirtualKeyW` 得到的扫描码：加速键按虚拟键匹配，所以 Dvorak/AZERTY 下也是复制；扫描码给远程桌面客户端、部分 Java/游戏引擎界面用。四个事件一次 `SendInput`，中间插不进用户的按键。
- 注入的 Ctrl+C 会经过我们自己的低级钩子，只会更新修饰键状态，不会误触发绑定。

### 3.5 UIA 的副作用：Chromium / Electron 无障碍树

Windows 上 Chromium 检测到 UIA/MSAA 客户端查询后会打开自己的无障碍树，之后该进程持续多一些 CPU/内存开销。这与 macOS 上 Chrome 被 AX 查询、以及我们对 VS Code 设置 `AXManualAccessibility` 是同一类代价，macOS 方案已接受。需要真机关注的一点：**VS Code 在 `editor.accessibilitySupport: auto` 下检测到辅助技术时可能切到屏幕阅读器优化模式**。若实测确实会被一次性 UIA 查询触发，可选对策是对 Electron 应用跳过 UIA、直接走 Copy（需要兼容模式开启）。

## 4. 热键在 Windows 上的特殊问题

### 4.1 吞键后留下"裸 Alt / 裸 Win"——菜单遮罩键

钩子吞掉的是热键的主键（R），系统其余部分只看到修饰键按下又松开、中间什么都没有。在 Windows 上这就是一次"轻点"：

- 裸 Alt 轻点 → 前台窗口激活菜单栏（Office 显示 KeyTips）。之后的 Ctrl+C 会被菜单模式吃掉，兼容模式失败。
- 裸 Win 轻点 → **弹出开始菜单**，开始菜单变成前台窗口，取词直接报 `foreground_changed`。

默认热键 Alt + Win + R 两种都会发生（取决于按键顺序）。处理沿用 AutoHotkey 的"menu mask key"做法：在钩子里吞掉朗读热键的同时，若 Alt 或 Win 按着，立即注入一次未分配的虚拟键 `0xE8` 的按下/松开，让修饰键不再"孤立"（`hotkey/menu_mask.rs`）。钩子对这个键直接放行，不更新修饰键状态、不参与绑定匹配、不会被录进热键。

> 听写热键（默认 Ctrl + Alt + Win + Space）理论上有同样的问题，但它是既有行为、本次没有改动，也没有在真机确认是否会触发，列为待验证项。

### 4.2 默认热键与系统快捷键

macOS 默认 ⌥⌘R / ⌥⌘T 映射到 Windows 是 **Alt + Win + R / T**，恰好是 Xbox Game Bar 的"开始/停止录制"和"显示/隐藏录制计时器"。低级钩子先于系统热键处理，开启朗读后这两个组合归 VoiceX。本次**保留**与 macOS 一致的默认值（同一个物理位置的修饰键），在设置页和 README 写明冲突、提示可改。备选方案（未采用）：Ctrl + Alt + 字母在很多欧洲布局上等同 AltGr，会吞掉字符；Ctrl + Win + 字母同样需要遮罩键且记忆性差。是否换默认值是产品决定，留待确认。

## 5. 读剪贴板（需求文档 M2）

`tts/clipboard_text.rs` 的 Windows 分支改为直接用 Win32：在**同一次** `OpenClipboard` 内先查 `ExcludeClipboardContentFromMonitorProcessing`，有则拒绝且不读入文本，否则读 `CF_UNICODETEXT`。两步不分开，避免检查后、读取前恰好复制进一个密码。分类规则沿用已有的 `classify`（有单测）。

## 6. TTS 引擎

- 云端引擎（火山 / 阿里 / MiMo / Azure / Edge）原样可用。Microsoft Edge 在线朗读无需密钥，是 Windows 上最省事的选择。
- 默认引擎仍是"系统语音"。在 Windows 上：引擎下拉里显示"系统语音（仅 macOS）"且不可新选；当前就是它时显示警告并隐藏用不了的音色与参数；按朗读热键时 HUD 显示「当前平台没有系统语音，请在朗读设置中选择引擎」。**不**自动改用 Edge——那会在用户没选择的情况下把文字发到在线服务。
- Windows 本地语音（SAPI / WinRT `SpeechSynthesizer`）可以作为后续独立任务。

## 7. 实现清单

| 文件 | 内容 |
|---|---|
| `src-tauri/src/selection/windows/mod.rs` | 取词主流程：前台应用 → 自身焦点 → 提权检查 → UIA（含密码框）→ Copy 兼容模式 |
| `src-tauri/src/selection/windows/uia.rs` | UIA 客户端（MTA、超时）、焦点元素信息、向上回溯读 `TextPattern` 选区 |
| `src-tauri/src/selection/windows/clipboard.rs` | 修饰键等待、全格式快照/还原、Ctrl+C、迟到复制还原 |
| `src-tauri/src/selection/windows_rules.rs` | 与 Win32 无关的判定（格式分类、字节预算、提权判定、选区拼接、控件类型名），**全平台编译**，单测在 macOS/Linux 上也能跑 |
| `src-tauri/src/win_clipboard.rs` | 共用的 Win32 剪贴板封装（打开重试、枚举、读写、序列号） |
| `src-tauri/src/hotkey/menu_mask.rs` + `manager.rs` | 菜单遮罩键 |
| `src-tauri/src/selection/mod.rs` | 新来源 `uia`、新错误 `target_elevated`、诊断探针新增 `focusedFramework` / `textPatternDepth` / `targetElevated`（`focusedRole` 填 UIA 控件类型，`focusedSubrole` 填类名，`axStatus` 填 HRESULT） |
| `src-tauri/src/tts/clipboard_text.rs` | Windows concealed 判定 |
| `src-tauri/src/tts/controller.rs` | 无可用引擎时 HUD 报错；`target_elevated` 不改读剪贴板 |
| `src-tauri/src/lib.rs`、`commands/tts.rs` | 热键在 Windows 上也注册 |
| `src/views/ReadingSettings.vue`、`src/hud/hud.ts`、i18n | 平台判断拆成"能取词"与"有系统语音"两件事；Windows 专属说明与错误文案 |
| `Cargo.toml` | `windows` 0.61（UIA 的 COM 接口，Tauri 已间接依赖同版本）；`windows-sys` 增加 DataExchange / Memory / Gdi / Security 特性 |

日志沿用结构化事件：Windows 上 UIA 一层记为 `event=selection_uia`（角色、类名、框架、状态、回溯深度，不含文本），其余 `selection_ok` / `selection_err` / `copy_landed_late` / `clipboard_*` 与 macOS 相同。设置页"取词诊断"在 Windows 上同样可用。

## 8. 已做的验证与未做的验证

已做：

- 整个 crate 以 `x86_64-pc-windows-gnu` 目标 `cargo check` 通过（Linux 容器里用占位 C 工具链跑通依赖的构建脚本），所有 Win32 / UIA 调用的签名与类型都经过编译器检查。
- Linux 上 `cargo test --lib`：新增的格式分类、预算、提权判定、选区拼接、遮罩键判定等单测全部通过；既有测试中 4 项失败与本次无关（3 项断言 macOS 的按键显示名，1 项需要音频输出设备）。
- `pnpm build`（`vue-tsc` 类型检查 + 构建）通过；用 Windows UA 渲染朗读设置页核对了文案与禁用状态，macOS UA 下页面不变。

**未做：Windows 真机运行。** 以下清单需要在 Windows 10/11 上逐项确认后再对外宣布可用。

## 9. Windows 真机验证清单

1. 热键：Alt + Win + R 触发朗读；按住时**不弹开始菜单**、记事本**不激活菜单栏**；Alt + Win + T 同理；朗读中再按停止、Esc 停止且 Esc 不传给前台应用；空闲时 Esc 正常传给前台应用。
2. UIA 路径（`selection_ok source=uia`）：记事本、Word、Edge 网页（含焦点在链接上）、Windows Terminal、PowerShell 控制台、设置页的文本框。
3. Copy 路径（`source=clipboard_copy`）：Excel 单元格、VS Code、一个 Electron 应用、微信。确认读后剪贴板恢复为原内容：纯文本、富文本（从 Word 复制）、截图（Win+Shift+S）、资源管理器复制的文件、Excel 区域。
4. 剪贴板历史开启时的条目变化与说明一致。
5. 密码框中按热键 → HUD「安全输入中，无法取词」，且不读剪贴板。
6. 管理员身份运行的记事本 → HUD「目标应用以管理员身份运行，无法取词」。
7. 没选中文字 → 读剪贴板；KeePass 复制的密码 → 「剪贴板里是密码，未朗读」。
8. 引擎为"系统语音"时按热键 → HUD 报引擎不可用；换成 Edge 后正常朗读，字幕正常。
9. VS Code（`editor.accessibilitySupport: auto`）被读取一次后是否进入屏幕阅读器优化模式（§3.5）。
10. 一个卡死的应用（或调试器暂停的进程）中按热键：HUD 在约 1–2 s 内报错而不是长时间停在"准备中"。
11. 设置页"取词诊断"在 Windows 上输出 UIA 探针字段。
12. 听写热键 Ctrl + Alt + Win + Space 以不同松键顺序是否会弹开始菜单（§4.1 附注，既有行为）。

## 10. 后续

- 视 §9 第 9 项结果决定是否对 Electron 应用跳过 UIA。
- 视真机结果决定 Windows 默认热键是否改为不与 Game Bar 冲突的组合。
- Windows 本地语音后端（WinRT `SpeechSynthesizer`），让零配置也能朗读。
- 若听写热键确有开始菜单问题，复用 `menu_mask` 修复。
- `scripts/tts/` 下的自动化 harness 目前是 macOS（CGEvent 注入）专用，Windows 版可用 `SendInput` 同构实现。
