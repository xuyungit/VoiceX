# 输入后修改观察：验证入口

本目录保留可复用的合成夹具与验证脚本。正文均使用合成样例，不向用户的历史数据库插入测试记录。

## 普通单元测试

差异算法、Unicode 范围、观察状态机、保存许可和删除隔离不需要原生窗口：

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::
```

提交前检查：

```sh
pnpm build
cargo check --manifest-path src-tauri/Cargo.toml --lib
cargo test --manifest-path src-tauri/Cargo.toml --lib
```

原生集成测试默认 `ignored`，普通测试不会弹出合成窗口或向目标输入框派发文字。Windows 当前保留编译检查，实机功能测试后置。

## macOS 原生集成

以下脚本会启动合成 AppKit 窗口或独立 Chrome 临时会话，并在测试输入框派发合成文字。需要测试进程具备辅助功能权限，测试期间保持目标前台且不操作键盘。仅在安排好的验证时段运行，不用于调试纯算法。

```sh
scripts/post-edit/check-macos-sessions.sh
scripts/post-edit/check-chrome-fixture.sh input full session
scripts/post-edit/check-chrome-fixture.sh text full session
scripts/post-edit/check-chrome-fixture.sh editable full session
scripts/post-edit/check-chrome-fixture.sh password full
scripts/post-edit/check-macos-dispatch.sh
```

Chrome 脚本参数：第一项为 `input`、`text`、`editable` 或 `password`；第二项省略时测试冷辅助功能树，`full` 仅在独立测试进程使用 `--force-renderer-accessibility`；第三项为 `read`（默认）或 `session`。这些标志不由生产读取器设置。完整语义树下成功不能替代冷树或真实应用验证。

AppKit 会话回归把观察窗口缩短至 9 秒，覆盖短暂歧义恢复、持续歧义结束、稳定修改后通知清空、焦点离开、再次注入，以及 6 万字文档中部的局部读取和修改。测试等待真实观察线程结束，把结果经生产写入函数存入隔离的内存 SQLite，再读回检查原文、修改结果、稳定性和结束原因；不会连接用户的历史数据库。

若同时运行 `tauri dev`，修改 Rust 源码可能触发 VoiceX 重启并让主窗口抢走夹具前台。应先完成 `cargo test --manifest-path src-tauri/Cargo.toml --lib --no-run`，等待开发进程稳定，再用上述命令输出的测试二进制绝对路径运行，期间不再修改源码：

```sh
VOICEX_POST_EDIT_TEST_BINARY=/absolute/path/to/voicex_lib-... scripts/post-edit/check-macos-sessions.sh
```

夹具启动时先检查 AX 端点就绪，准备阶段允许 800 ms；之后观察仍使用生产的 300 ms 读取预算。这项回归验证已就绪的控件，不覆盖冷启动：本机新启动 AppKit 的首次 `AXFocusedUIElement` 查询曾耗时约 400 ms，可能超出生产预算，仍需后续覆盖工作处理。

### 实际编辑器（Electron / WebKit）

macOS 读取器通过目标应用包内的 `Electron Framework.framework` 识别 Electron，查询应用 `AXRole` 请求原生辅助接口；不切换完整读屏模式。空 subrole 仍须通过真实 Chromium 节点 ID、editable ancestor 自身身份以及密码/编辑能力检查。应用名称本身不构成放行依据。

WebKit 编辑控件同样可能返回空 subrole，但没有 Chromium 节点 ID。其通用识别检查标记/范围接口，并实际生成、验证一个不含正文的文本标记。可编辑的 `AXWebArea` 没有普通文本框的数字选区接口，改用文本标记取得选区及局部正文；文档总长度只读元数据，正文及选区前缀都限制在局部窗口内。只读网页不放行。

当焦点节点提供 `AXHighestEditableAncestor` 时，读取器先检查原节点是否为密码，再验证同进程的父级路径，观察实际编辑区域。编辑器内部焦点通知只触发一次合并复核，是否离开目标由原生读取器确认，避免内部节点切换直接结束观察。WebArea 的选区标记所属节点及父级同样先做密码检查。上述路径不使用应用名称，也不会只凭属性列表放行。

实际应用的交互回归入口（当前包含 Grok Bot、WorkBuddy、DeepSeek Harness、Obsidian 源码模式、邮件正文及 Typora 即时渲染模式）使用已编译的测试二进制，避免开发服务在验证时重启抢焦点。Swift 启动器运行主循环跟踪前台，切到每个目标时分别启动一个测试进程；不要在同一个 libtest 进程内等待跨应用切换，AppKit 的前台属性可能停留在主循环未更新的缓存：

```sh
swift scripts/post-edit/check-macos-editors.swift /absolute/path/to/voicex_lib-...
# 单独验证 Obsidian：提前准备临时空白笔记，并切到源码模式。
swift scripts/post-edit/check-macos-editors.swift /absolute/path/to/voicex_lib-... md.obsidian
# 邮件使用无收件人的临时空白写信正文；Typora 使用即时渲染的临时空白文档，不发送邮件。
swift scripts/post-edit/check-macos-editors.swift /absolute/path/to/voicex_lib-... com.apple.mail abnerworks.Typora
```

看到 `Ready` 后，按命令指定的应用顺序在各自空白输入框输入 `VoiceX test A`，保持光标在末尾，停约 3 秒；将 A 改为 B，再停约 3 秒。不要发送。测试按生产后锚定规则确认光标前唯一的合成原文，允许局部窗口包含上下文。它验证生产读取器、稳定范围和隔离 SQLite 读回结果，同时报告是否收到修改通知；不向真实历史写入数据，也不代表已经验证 ASR 服务和真实听写派发。失败日志只包含读取错误、字符数、选区、角色和合成标记匹配状态，不输出其他草稿正文。默认普通测试不会运行它。

生产读取器按框架及控件能力识别，不使用这些应用名称作为 Electron 支持名单；这里的名单只限制人工原生测试的读取范围。其他应用可复用同一读取器，但仍需检查焦点、密码属性、正文/光标范围、通知和真实文档行为。

WebKit 标记范围还有独立的后台合成回归，窗口保持后台，不需要输入键盘。它检查短文修改、6 万字正文的局部读取，以及只读和密码控件在请求正文前被拒绝。此测试直接提供合成光标位置，只验证实际 AX 范围读写，不覆盖前台焦点及真实选区的取得：

```sh
swiftc scripts/post-edit/webkit-fixture.swift -o /tmp/voicex-webkit-fixture
VOICEX_POST_EDIT_WEBKIT_FIXTURE=/tmp/voicex-webkit-fixture cargo test --manifest-path src-tauri/Cargo.toml --lib post_edit::platform::macos_tests::background_webkit_marker_ranges -- --exact --ignored --test-threads=1
```

### 通用局部读取与定时复核

macOS 以听写前的选区为中心，各保留约 2,048 个 UTF-16 单元上下文，后续固定左端并按文档长度变化更新右端；不会跟随光标重新选取别处的内容。剪裁边缘保存上下文守卫，无法维持边缘一致时跳过样本并继续复核，未恢复则不能保存旧对照。首次窗口边缘会对齐 UTF-16 代理对，正文不做替换或归一化。前读不可用时，后读窗口还会为预期的听写长度保留空间。

Windows 通过 UIA 选区向两侧扩展有限的 Character 单元，保留原生 TextRange；真正的文档首尾会随文档扩展，内侧截断边缘保留原生锚点并检查上下文。正文及选择前缀都仅从局部范围读取。单次局部文本仍限制为 16,384 个 UTF-16 单元，过大的单次选区、听写或窗口增长可能触达此预算，文档总长度本身不再直接触发拒绝。平台接口必须实际提供相应范围能力；虚拟化编辑器仍需实测。

所有控件都会独立定时复核，不以“订阅成功”作为事件可靠的证据：前 10 秒每秒一次，之后每 2 秒一次；事件可提前触发读取，已知修改仍需至少 800 ms 后的一致样本确认稳定。保留 60 秒观察时限、90 次原生读取上限、单次 300 ms 超时、单一工作线程和代次隔离。焦点、安全输入及用户开关仍可终止读取。修改和发送都落在两次读取之间时，仍可能漏采。

普通测试覆盖 20 万字文档的局部范围、剪裁边缘失效、Unicode 边界、长听写的后锚定窗口，以及无事件/虚假事件能力下的修改后清空和完整 60 秒读取次数；这些测试不能替代实际编辑器、输入法和 Windows UIA 验证。

2026-11-05，预编译的 AppKit 原生回归在前台运行通过（20.51 秒）：6 万字合成文档中部的预读返回 4,096 个 UTF-16 单元，实际注入后读取并保存了正确的听写修改对照；原有密码拒读、歧义恢复、清空和再次注入场景也通过。该结果限于已就绪的受控 AppKit 控件，真实编辑器的长文档与 Windows UIA 尚未验证。

Zed 1.27.0 本机元数据探针只见窗口、容器和窗口按钮，未见正文控件；该版本官方 `crates/zed/src/main.rs::build_application()` 默认走 `Application::new_inaccessible`，只有启动环境中设置 `ZED_EXPERIMENTAL_A11Y=1` 才启用实验性辅助功能。这与 Electron 的接口初始化不同；本轮没有重启用户的 Zed 或验证实验模式，尚不能标为支持。

终端画布即使声明 `AXTextArea` 也不等于可读文本框。Kaku 的当前实测控件没有正文、字符数或选择范围读取能力；不能把终端输出视为输入框，需另行评估终端专用方案。

派发计时脚本默认把原始数据放在临时目录，输出汇总后清理；如需保留，传入输出路径：

```sh
scripts/post-edit/check-macos-dispatch.sh /tmp/voicex-dispatch-samples.json
```

该计时只衡量受控原生夹具中的派发路径，不代表完整 ASR、HUD、目标应用 CPU 或能耗验收。

## 实际 Vue 历史页的隔离验证

项目 Vite 开发服务运行在 1520 端口时，另运行：

```sh
node scripts/post-edit/history-ui-preview.mjs
```

打开 `http://127.0.0.1:1522/history?scenario=edited`。可选场景为 `empty`、`edited`、`long`、`many`、`error` 和 `save-error`。此服务加载实际 Vue 页面并注入模拟 Tauri 接口，适合核对 Tab、空状态、长文、分页和错误提示；不能作为真实目标应用采集成功的证据。

## 当前验证边界

- 用户已确认在 Codex 的修改记录 Tab 看到了原文和修改结果，基础采集、保存、展示闭环有用户实测反馈。
- 2026-11-05：Grok Bot、WorkBuddy、DeepSeek Harness 已分别通过用户手动 A→B 的原生读取、变化通知、稳定范围和隔离数据库验证；该测试不覆盖真实听写注入或用户历史页。
- 同日 Obsidian 1.14.4 的临时笔记源码模式通过相同原生回归，复用了通用 Electron 读取器；实时预览、长文档和真实听写派发仍未验证。
- 同日邮件 16.0 的无收件人临时写信正文、Typora 1.14.15 的即时渲染临时文档分别通过真实前台 A→B 回归，收到修改通知，并正确保存、读回隔离测试记录。WebKit 后台合成范围回归也通过。真实听写注入、历史页、两款应用中的长文档和输入法交互仍未验证。
- 原生 AppKit 及完整 Chrome 语义树已完成受控场景；广泛应用覆盖尚未完成。
- 已实现大文档局部范围读取，16,384 个 UTF-16 单元限制作用于单次窗口；真实编辑器的长文档、快改快发和长时间资源表现仍需验证。
- 输入法状态在平台提供能力时检查：macOS 使用 SDK 的 `AXTextInputMarkedTextMarkerRange`，Windows 使用 TextEdit 的 active composition；未知能力不会标为已提交。已知组词期间跳过正文，歧义样本可继续观察。后续一致读取确认稳定后，清空通知不会抹掉这份证据：实际读到空框时，以“部分观察”保存此前确认的文本、时间和输入法状态。若已读到新的修改、组词或歧义内容，旧确认失效，需重新确认稳定。旧记录缺少稳定性证据时在页面标明，不自动改写或删除原记录。
- Windows 目前没有 UIA 文本或焦点订阅，使用同一套有界定时复核与收尾；本轮通过编译检查，实际交互尚未验证，不能推断与 macOS 覆盖相当。
- 本机截图、原始计时样本和逐次调试记录不纳入提交。
