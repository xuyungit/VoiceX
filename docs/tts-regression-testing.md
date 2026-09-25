# 朗读功能回归测试指南

> 适用范围：划词朗读、翻译并朗读、HUD 字幕（`src-tauri/src/tts/`、`src-tauri/src/selection/`、`src/hud/`、设置页「朗读」）。
> 听写（ASR）链路目前没有自动化驱动，不在本文范围内。
> 读者：维护者，以及被派来执行一轮回归的 Agent。第 8 节是派遣时可以直接用的提示词。

## 1. 什么时候跑哪一级

完整回归要占用前台十几分钟（TextEdit、Safari 等会被反复拉到前台，键盘被注入，扬声器出声），期间这台 Mac 基本不能干别的。所以分三级，默认只做 L0。

| 级别 | 内容 | 耗时 | 什么时候做 |
|---|---|---|---|
| L0 | `cargo test --lib` + `pnpm build` | 约 2 分钟，不占前台 | 每次改动 |
| L1 | L0 + 与改动直接相关的**一个**实机用例（第 3 节里挑最窄的那条） | 1–2 分钟 | 改了单测覆盖不到的行为：热键、HUD 生命周期、后端切换、选区读取 |
| L2 | 本文第 3 节的完整流程 | 15–25 分钟，全程占用前台 | 发版前；合并一个里程碑；动了 `controller.rs` 的会话状态机、`cloud_playback`、选区读取层、HUD 窗口管理；或维护者点名要求 |

判断不了属于哪一级时按 L0 做，并在汇报里说明哪些行为没有实机验证，由维护者决定要不要补。HUD 的观感（字号、位置、停留时画面上是什么）最终由维护者目测，脚本和 Agent 都只能给出旁证。

## 2. 前置条件

### 2.1 一次性的授权

| 谁 | 需要的权限 | 缺了的表现 |
|---|---|---|
| 运行中的 VoiceX（开发版继承启动它的终端的授权） | 辅助功能、输入监控 | 热键无反应；选区读取报 `permission_denied` |
| 跑脚本的 shell | 辅助功能（注入按键）、自动化（System Events、TextEdit、Safari 等） | `CGEventPost` 静默失败：没有 `hotkey_action` 日志、HUD 不出现 |
| 跑 `hud_shot.sh` 的 shell | 屏幕录制 | `screencapture` 报 `could not create image from rect` |

已知情况（2026-09）：Claude 桌面应用里 Bash 工具的进程和 Terminal.app **没有**屏幕录制权限；桌面应用的「终端」面板有。后台作业（`run_in_background`、`&`）里注入的按键会被静默丢弃，**驱动脚本必须在前台 shell 里跑**。

### 2.2 每次开跑之前

1. 屏幕解锁，维护者知情：这段时间电脑会被占用、会出声。
2. 应用以带日志的方式启动（日志只走 stderr，`~/Library/Application Support/com.voicex.app/logs/voicex.log` 不是实时日志）：

   ```bash
   pnpm tauri dev 2>&1 | tee /tmp/voicex-tts.log
   ```

   应用已经由维护者在别的终端里启动、又没有 tee 时：`translate_read.sh` 和不带 `--log` 的 `hud_probe.sh` 仍然能跑（分别按历史行和 HUD 窗口判定），依赖日志的三个脚本（`smoke_phase0.sh`、`negative_cases.sh`、`p0_survey.sh`）不能跑。不要擅自重启维护者的应用，先问。
3. 记下当前设置，结束时照此还原。**只用 `json_extract` 取单个字段，绝不 `select value` 整行**——这一行里有各家的 API Key，任何时候都不能打印到终端、日志或报告里：

   ```bash
   sqlite3 -cmd ".timeout 3000" "$HOME/Library/Application Support/com.voicex.app/voicex.db" \
     "select json_extract(value,'\$.ttsEnabled'), json_extract(value,'\$.ttsProviderType'), json_extract(value,'\$.ttsCaptionsEnabled'), json_extract(value,'\$.ttsTranslateEnabled') from user_config where key='app_settings';"
   ```

4. 读出热键绑定并导出给驱动脚本（维护者的朗读热键不是默认值；不做这一步，脚本按的是没人监听的键）：

   ```bash
   eval "$(scripts/tts/hotkey_env.py)"
   ```

   它处理字母、数字和 Return / Tab / Space / Delete / Escape。纯修饰键或带 Fn 的绑定无法用 CGEvent 按出来，会报错退出；VoiceX 没有命名的键（功能键、方向键、标点）同样报错退出，这时手工设置 `VOICEX_READ_KEY` / `VOICEX_READ_MODS`、`VOICEX_TRANSLATE_KEY` / `VOICEX_TRANSLATE_MODS`（KEY 是 macOS 虚拟键码，MODS 是 `control,option,shift,command` 的逗号组合；存储格式 `键|修饰位|Fn`，修饰位 0x1000 control、0x0800 option、0x0200 shift、0x0100 command）。
5. 云端后端：用维护者当前配置的那一家。**小米 mimo 暂不用于测试**，除非维护者明确说可以。
6. 关掉 VoiceX 的设置窗口，或至少在测试期间不要在里面保存：设置页保存的是它内存里的整份设置，会盖掉第 3.5 节临时改的字段。

## 3. 完整回归流程（L2）

按顺序做。前一步 FAIL 不必停，记下来继续；出现 INVALID 先排除环境原因（见第 9 节）再重跑那一条。

### 3.1 静态检查（不占前台）

```bash
cd src-tauri && cargo test --lib
```

```bash
pnpm build
```

记录通过数与 ignored 数（2026-09-18 为 362 通过、9 ignored；ignored 的是需要网络和凭据的后端实测，回归不跑）。

### 3.2 划词朗读主链路（需要日志）

```bash
scripts/tts/smoke_phase0.sh --log /tmp/voicex-tts.log
```

TextEdit 与 Safari 各一轮：选区读取 → 出声 → 第二次热键停止，并核对读到的字数等于夹具字数。约 1 分钟。

```bash
scripts/tts/negative_cases.sh --log /tmp/voicex-tts.log
```

没有可读内容时的错误码（`focus_is_self`、`no_selection` 等），每条用例断言具体的码。约 1 分钟。

以下两条只在动了选区读取层（`src-tauri/src/selection/`）时加跑：

```bash
scripts/tts/p0_survey.sh --log /tmp/voicex-tts.log
```

七个应用（textedit safari chrome vscode notes preview terminal）的选区路径分布，冷启动的应用多时要 5 分钟以上；`--app` 可以只跑其中几个。它是调查不是闸门，结果表原样贴进报告。

```bash
scripts/tts/marker_range_probe.sh
```

Safari WebKit marker range 读取，直接跑 ignored 单测，不需要运行中的应用。

### 3.3 翻译并朗读（不需要日志）

```bash
scripts/tts/translate_read.sh --case all
```

五个用例，按 `history_record` 里 `mode = translate_read` 的行判定：

| 用例 | 期望 |
|---|---|
| success | 出一行，无音频路径 |
| long | 约 2850 字，出一行且译文尾部保留最后一节的编号（抓输出截断） |
| toolong | 超过 5000 字，LLM 调用前拒绝，无行 |
| cancel | 热键后 0.5 秒 Esc，无行 |
| stop | 朗读中第二次热键，行存在，朗读中断 |

约 2–3 分钟。long 偶尔因为模型少翻几节而 FAIL（B.3 记录过一次），单独重跑一次 `--case long`，两次都失败才算数。

### 3.4 HUD 与字幕生命周期

`hud_probe.sh` 每次只做一轮朗读：摆夹具、按热键、等 `--speak` 秒、朗读还在就再按一次，报告 HUD 何时出现、何时消失；带 `--log` 时打印这一轮的结构化事件，由执行者对照第 4 节的期望序列判定。

| # | 命令 | 验证什么 |
|---|---|---|
| H1 | `hud_probe.sh --kind read --fixture short --speak 25 --log LOG` | 自然读完：字幕、`speak_finished`、HUD 自行隐藏 |
| H2 | `hud_probe.sh --kind read --fixture long --speak 8 --log LOG` | 分段、朗读中停止、HUD 5 秒内隐藏 |
| H3 | `hud_probe.sh --kind read --fixture long --speak 0 --log LOG` | 出声前停止：无 `speak_started`、无 `caption`，HUD 不留空框 |
| H4 | `hud_probe.sh --kind translate --fixture long --speak 20 --log LOG` | 翻译阶段 → 朗读阶段 → 字幕推进 → 停止 |
| H5 | `hud_probe.sh --kind translate --fixture xlong --speak 45 --log LOG --shots DIR` | 三十多段的长译文：字幕按段推进、无 `speak_retry`、停止干净。只在动了 `cloud_playback` 或分段逻辑时跑 |

（`LOG` 即 `/tmp/voicex-tts.log`，脚本路径都在 `scripts/tts/` 下。）合计约 3 分钟，加 H5 约 5 分钟。

### 3.5 后端切换（可选，动了后端选择或 `say` 路径时做）

设置每次朗读时从数据库读，所以可以直接改库、下一次朗读生效，不用重启应用：

```bash
sqlite3 -cmd ".timeout 3000" "$HOME/Library/Application Support/com.voicex.app/voicex.db" \
  "update user_config set value=json_set(value,'\$.ttsProviderType','system') where key='app_settings';"
```

然后跑一次 H1。期望取决于系统音色：为空时走 `say`，`speak_start backend=mac_say`，没有 `speak_chunked` 和 `caption`，HUD 是紧凑版；选了音色时走 AVSpeech，`backend=mac_system`，字幕照常。

**做完立刻改回原值并用 2.2 第 3 步的查询核对。** 同样的办法可以临时关字幕（`'$.ttsCaptionsEnabled'` 设为 `json('false')`）验证紧凑版 HUD；也要还原。`json_set` 只碰指定字段，不会读出或打印别的内容。

### 3.6 界面部分

见第 5 节。L2 至少做 5.1（HUD 版式）和 5.3（设置页），5.2（实机截图）在有屏幕录制权限的 shell 可用时做。

## 4. 期望的日志序列

事件都是 `event=名字 key=value …` 的单行。下面只列判定用得到的，顺序有意义，中间可以夹别的行。

**任何一轮都必须有**：`hotkey_action action=… state=idle`。没有这一行说明注入的按键没到应用——这是环境问题（授权、后台作业、热键改绑），判 INVALID，不是 FAIL。

| 场景 | 期望 |
|---|---|
| 直接朗读、自然读完（H1） | `selection_start` → `selection_ok chars=N`（N 等于夹具字数）→ `speak_start kind=… backend=… chars=N` → `speak_started` → `caption index=0 total=1` → `speak_finished` |
| 长文本（H2、H4、H5） | `speak_start` 之后有 `speak_chunked pieces=K`（字幕开着时按 120 字分段）；`caption index=` 从 0 起递增、不跳号、不重复，`total=K` |
| 朗读中停止（H2、H4、H5） | 第二次热键是 `hotkey_action … state=active` → `speak_stop` → `speak_stopped` → `speak_cancelled`，三条都要有；不应出现 `speak_err`，也不应出现 `symphonia_core::probe: probe reach EOF` |
| 出声前停止（H3） | 没有 `speak_started`、没有 `caption`；同样的 `speak_stop` → `speak_stopped` → `speak_cancelled` |
| 翻译并朗读（H4） | `speak_start` 带 `llm=true`；历史里多一行 `translate_read`（`translate_history`）；失败时是 `llm_stage_err stage=translate error=…` |
| `say` 后端（3.5） | `backend=mac_say`，无 `speak_chunked`、无 `caption` |
| 云端重试 | 正常网络下不应出现 `speak_retry`（单段请求失败后的重试）、`backend_fallback`；出现了如实记录，不自行判 FAIL |

HUD 侧的期望：热键后 5 秒内出现；朗读结束（自然结束或停止）后 5 秒内消失，正常是一秒内。字幕模式下窗口是 680×140，1920×1080 逻辑屏上 rect 为 `620,820,680,140`（底部居中、下边距 120）。

## 5. 界面部分的测试方法

三种办法，能回答的问题不一样。

### 5.1 HUD 版式：内置浏览器打开 HUD 页面

能验证：字幕版式的字号、行高、行数、居中、状态行与边框是否隐藏、占位与错误态的颜色。不能验证：窗口位置、透明度、毛玻璃、真实的事件时序——页面脱离了 Tauri，没有事件进来。

1. 开发服务器在跑（`pnpm tauri dev` 自带，端口 1520）。用内置浏览器打开 `http://localhost:1520/src/hud/index.html`，视口设为 680×140（尺寸常量在 `src-tauri/src/hud/window.rs`，紧凑版是另一组）。
2. 页面脚本因为没有 Tauri 会停在加载态，用 JS 手工摆出要看的状态，再读计算样式：

   ```js
   document.body.classList.remove('hud-loading', 'compact-batch-mode', 'batch-wave-mode');
   document.body.classList.add('caption-mode');
   const t = document.getElementById('textArea');
   t.hidden = false;
   t.textContent = '这里放一段约 120 字的句子……';
   const cs = getComputedStyle(t);
   ({
     fontSize: cs.fontSize, lineHeight: cs.lineHeight, textAlign: cs.textAlign,
     scrollHeight: t.scrollHeight, clientHeight: t.clientHeight,
     statusArea: getComputedStyle(document.querySelector('.status-area')).display,
     border: getComputedStyle(document.querySelector('.hud-container')).borderTopWidth,
   });
   ```

3. 期望（M4 定稿）：`fontSize` 20px、`lineHeight` 28px、居中；约 120 字的中文正好四行（`scrollHeight` 112 = 4 × 28）；`.status-area` 为 `display: none`；边框宽度 0。占位文字（`.placeholder`）是 50% 白。
4. 截一张图附进报告。这一步只读不写：**不要用 JS 改页面来"修"问题，样式问题回到 `src/hud/hud.css` 改。**

### 5.2 实机 HUD 截图

能验证：真实窗口里画了什么、在屏幕上的位置。

`hud_probe.sh --shots DIR` 会在热键后 1–4 秒每秒截一张、之后每 4 秒一张；单独截一张用 `scripts/tts/hud_shot.sh OUT.png`。两者都要求**发起截图的 shell 有屏幕录制权限**（2.1）。没有这种 shell 时跳过本节并在报告里写明，不要尝试别的途径：按应用截图的工具认不出没有 bundle id 的开发版进程（`app_list_windows` 为空），已经试过。

翻译并朗读的正常画面序列（B.3 实测）：热键后约 0.4 秒是魔法棒图标 +「正在翻译...」，约 1.4 秒是朗读图标 +「正在准备朗读...」，约 2.4 秒起只显示第一句文字，之后按段推进。朗读结束后的 400 ms 停留期间画面应当还是最后一句而不是空框——截图间隔抓不住这 400 ms，这一条留给维护者目测。

### 5.3 设置页：内置浏览器打开路由

能验证：「朗读」设置页的分组、文案、i18n、控件是否都在。不能验证：真实的设置值和保存——浏览器里没有 Tauri 后端，控制台会有 `Cannot read properties of undefined (reading 'invoke')`，这是预期的，页面显示的全是默认值。

1. 打开 `http://localhost:1520/reading-settings`（其它页同理：`/llm-settings`、`/history` 等，路由见 `src/router/index.ts`）。
2. 用页面文本核对各分组标题与说明是否齐全（朗读引擎、热键、翻译并朗读、朗读字幕及其「按句合成、云端每句一个请求」的说明）；需要时切换语言再核对一遍 en-US。
3. 持久化只能靠数据库核对：维护者在真实应用里改一个开关，再用 2.2 第 3 步的 `json_extract` 查询看字段是否变了。Agent 不代替维护者去点真实应用的设置窗口。

### 5.4 留给维护者目测的清单

报告末尾原样列出，逐条标注"脚本旁证"或"未验证"：

- 字幕字号、行数、位置、透明度是否合适；
- 朗读结束时最后一句停留约 0.4 秒后随窗口一起消失，中间没有空框；
- 出声前停止时 HUD 直接消失，没有空框；
- 出错时字幕被错误提示替换，约 2.6 秒后消失；
- 朗读进行中开始听写，HUD 让给听写、字幕不残留。

## 6. 收尾

1. 确认没有朗读还在进行（HUD 不在窗口列表里；`hud_probe.sh` 和 `translate_read.sh` 会自己停，但中途被打断的脚本不会）。
2. 夹具文档只按 RUN_ID 关闭，脚本已经这样做；**不要**"关掉最前面的文档 / 标签页"——那可能是维护者没保存的东西。
3. 用 2.2 第 3 步的查询核对设置与开跑前一致。
4. 关掉为测试打开的浏览器标签；`/tmp` 下的截图目录路径写进报告，不进仓库。
5. 回归过程中发现的问题只记录、不顺手修；修复是另一件事，修完按 L0/L1 验证。

## 7. 报告模板

```markdown
## 朗读回归 YYYY-MM-DD（commit abc1234，后端 aliyun，字幕开）

| 步骤 | 结果 | 备注 |
|---|---|---|
| 3.1 cargo test --lib | 362 passed / 9 ignored | |
| 3.1 pnpm build | 通过 | |
| 3.2 smoke_phase0 | PASS 2/2 | |
| 3.2 negative_cases | PASS n/n | |
| 3.3 translate_read --case all | PASS 5/5 | success 1 s，long 2 s |
| 3.4 H1–H4 | PASS / FAIL / INVALID 各几条 | 关键日志序列贴在下面 |
| 3.5 后端切换 | 未做（未涉及） | |
| 5.1 HUD 版式 | 符合 | 截图路径 |
| 5.2 实机截图 | 跳过：无屏幕录制权限的 shell | |
| 5.3 设置页 | 文案齐全 | |

### 失败与异常
（逐条：命令、期望、实际、相关日志行。INVALID 写明环境原因。）

### 未验证 / 留给维护者目测
（5.4 的清单）

### 设置还原核对
开跑前：…… 结束后：……
```

写进需求文档附录（如 `translate-read-requirements` 的附录 B）的只是结论性的几行；完整报告留在会话里或由维护者决定放哪。

## 8. 派遣 Agent 的提示词模板

```text
在 VoiceX 仓库根目录按 docs/tts-regression-testing.md 执行一轮 L2 朗读回归。

范围：第 3 节 3.1–3.4 全部，3.5 [做 / 不做]，第 5 节 5.1 与 5.3，5.2 仅在你有带屏幕录制权限的 shell 时做。
应用状态：[维护者已用 `pnpm tauri dev 2>&1 | tee /tmp/voicex-tts.log` 启动 / 未带日志启动，跳过依赖日志的脚本]。

硬性约束：
- 设置库里有 API Key。只许用 json_extract 取单个字段，任何输出里都不能出现 Key。
- 不用小米 mimo 做测试。
- 驱动脚本在前台 shell 里跑，先 eval "$(scripts/tts/hotkey_env.py)"。
- 没有 hotkey_action 日志的用例判 INVALID 并排查环境，不判 FAIL。
- 夹具只按 RUN_ID 关闭；不关、不改维护者自己的窗口和文档；不重启维护者的应用。
- 只测试和记录，不修改产品代码，不提交。
- 临时改过的设置必须还原并核对。

产出：按文档第 7 节的模板写报告，附关键日志行和截图路径，列出未验证项。
```

## 9. 已知的坑

| 现象 | 原因 | 处理 |
|---|---|---|
| 热键后什么都没发生，日志也没有 `hotkey_action` | 跑脚本的 shell 没有辅助功能授权；脚本在后台作业里；热键改绑了而没跑 `hotkey_env.py` | 换前台 shell、补授权、导出热键后重跑；判 INVALID |
| 热键发出去了，却是"停止"而不是"开始" | 上一轮朗读还没结束（2870 字要读十来分钟），朗读中的热键是停止 | 脚本开头的 `stop_if_reading` 已处理；手工操作时先确认 HUD 不在 |
| 用例被判 INVALID：TextEdit is not frontmost | 注入前别的应用抢了前台（微信来消息等），脚本拒绝向别的应用注入 | 单独重跑这一条 |
| 修饰键热键正常，Esc / 字母热键全部失灵 | 别的应用占着 macOS Secure Input（密码框、某些终端） | 找到并退出那个输入状态；与 VoiceX 无关 |
| `screencapture: could not create image from rect` | 调用方没有屏幕录制权限 | 换有权限的 shell，或跳过 5.2 |
| `sqlite3` 报 `database is locked` | 应用正持有写事务 | 一律带 `-cmd ".timeout 3000"` |
| cancel 用例出了历史行 | LLM 回得比 Esc 还快（Cerebras 1.1 秒回过 2320 字） | `CANCEL_ESC_DELAY_S` 已调到 0.5；仍出现就如实记录 |
| Safari 选区读到的是地址栏 | System Events 的 `click at` 不移动键盘焦点 | 脚本用真实鼠标事件点窗口中心（`cgevent_click.py`），不要换回 AppleScript 点击 |
| 冷启动的应用让 osascript 卡两分钟 | AppleEvent 默认超时 120 秒 | `lib.sh` 的 `osa` 已加超时；新写驱动时用它，别直接调 `osascript` |
| 长文 long 用例偶发缺最后一节 | 模型输出不稳定，与朗读无关 | 重跑一次，两次都缺才记 FAIL |
| 改库后的设置又变回去了 | 设置窗口保存时写回了整份内存副本 | 测试期间关掉设置窗口 |

## 10. 脚本清单（`scripts/tts/`）

| 脚本 | 用途 | 需要日志 | 需要运行中的应用 |
|---|---|---|---|
| `hotkey_env.py` | 从设置库读两个朗读热键，输出给驱动脚本用的 `export` | 否 | 否 |
| `smoke_phase0.sh` | TextEdit / Safari 主链路 | 是 | 是 |
| `negative_cases.sh` | 错误码 | 是 | 是 |
| `p0_survey.sh` | 七个应用的选区路径调查 | 是 | 是 |
| `marker_range_probe.sh` | Safari marker range 读取 | 否 | 否 |
| `translate_read.sh` | 翻译并朗读五个用例，按历史行判定 | 否 | 是 |
| `hud_probe.sh` | 单轮朗读的 HUD / 字幕生命周期 | 可选 | 是 |
| `hud_shot.sh` | 截 HUD 窗口（要屏幕录制权限） | 否 | 是 |
| `lib.sh` | 公共部分：RUN_ID、日志截取、按键与点击注入、HUD 可见性、AppleScript 超时 | — | — |
| `cgevent_key.py` / `cgevent_click.py` | 在 HID tap 注入按键 / 鼠标点击 | — | — |
| `aliyun_probe.py` / `mimo_probe.py` | 后端接口探测，调研用，不属于回归 | — | — |

新增实机用例时：夹具带 RUN_ID、收尾只按 RUN_ID 清理、注入前断言前台应用、能按"留下了什么"（历史行、窗口、日志事件）判定的就不要靠 sleep 猜。
