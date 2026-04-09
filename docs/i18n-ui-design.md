# VoiceX 多语言改造设计稿

> **状态：已完成** — 全部 5 个阶段均已实施落地（2026-04-09 确认）。

## 目标

为 VoiceX 增加中英文界面切换能力，并保证以下几个区域在 macOS / Windows 上行为一致：

- 主窗口 Vue 界面
- Tauri Tray 菜单
- HUD 悬浮窗
- 用户可见的错误与状态提示

本次设计默认支持三档语言模式：

- `system`：跟随系统语言
- `zh-CN`：简体中文
- `en-US`：English

## 设计原则

### 1. 语言是全局显示偏好，不是导航项

语言切换不应放入左侧导航，因为侧边栏用于页面切换，而语言属于全局显示设置。

### 2. 保持单一事实来源

语言设置需要持久化到后端设置中，不能只停留在前端内存状态。主窗口、HUD、Tray 菜单都应读取同一个最终语言结果。

### 3. 前后端各司其职

- TypeScript 负责主窗口和 HUD 的文案翻译
- Rust 负责系统语言解析、设置持久化、Tray 菜单翻译
- 用户可见错误尽量使用“错误码 + 前端翻译”，减少 Rust 直接返回中文字符串

### 4. 先稳定架构，再批量翻译

先打通语言模型、设置和渲染链路，再迁移页面文案。不要先零散替换字符串。

## 视觉方向

### Visual Thesis

克制、安静、工具感强，让语言切换像一个稳定的全局控制项，而不是抢注意力的新功能。

### Content Plan

- 全局框架：保留现有左侧导航和主工作区
- 顶部工具区：承载语言切换入口
- 设置页：保留正式语言设置项，用于解释“跟随系统”
- HUD / Tray：不提供独立入口，只响应全局语言设置

### Interaction Thesis

- 顶部工具区轻微淡入，不制造新的视觉重心
- 语言切换器使用小型 segmented control
- 选中项使用平滑横向滑块动画
- 选择 `system` 时显示当前解析结果，降低不确定感

## 顶部语言切换设计

## 推荐方案

将 `src/App.vue` 中当前几乎空白的 `content-header` 升级为“顶部全局工具条”。

布局建议：

```text
| Sidebar | [ drag region ------------------------ ] [ Auto | 中文 | EN ]
|         | [ page content starts below, aligned to same content width  ]
```

### 设计要点

- 语言控件放在顶部工具条右侧，而不是窗口绝对右上角
- 工具条内容与主内容区使用同一条对齐线
- 左侧大面积区域保持可拖拽
- 控件区域标记为 `no-drag`
- 语言控件视觉弱化，避免与页面标题竞争

### 为什么这样更自然

- 它属于全局框架，不会和左侧导航混淆
- 它比藏在 About 或设置页更容易发现
- 它未来还能容纳主题、诊断、帮助等全局入口
- 对 macOS 和 Windows 都更稳定，不会和窗口控制区抢空间

## 语言切换控件规格

### 顶部快捷切换

顶部控件建议使用固定短标签，以保证两种界面语言下都容易识别：

- `Auto`
- `中文`
- `EN`

### 正式设置文案

设置页中使用完整标签：

- `Follow System`
- `简体中文`
- `English`

### 跟随系统提示

当用户选择 `system` 时，在控件附近或设置页内显示解析结果：

- 中文界面：`当前跟随系统：English`
- 英文界面：`Currently following system: 简体中文`

### 视觉建议

- 高度控制在 `28px` 到 `32px`
- 底色接近 `--color-bg-secondary`
- 激活态仅做浅亮面变化，不使用强烈主色填充
- 使用胶囊形轮廓，边框保持细且低对比

## 语言模型

建议统一定义：

```ts
type UiLanguage = 'system' | 'zh-CN' | 'en-US'
type ResolvedLocale = 'zh-CN' | 'en-US'
```

规则：

- 用户选 `zh-CN` 时，最终语言为 `zh-CN`
- 用户选 `en-US` 时，最终语言为 `en-US`
- 用户选 `system` 时，由 Rust 根据系统 locale 解析出最终语言
- 中文系 locale 统一映射为 `zh-CN`
- 其他 locale 暂时统一映射为 `en-US`

这样做的目标是避免前后端重复实现复杂判断逻辑，并为未来扩展更多语言保留路径。

## 前后端职责划分

## TypeScript 侧

负责：

- Vue 主窗口国际化
- HUD 文案翻译
- 页面内按钮、标签、空态、弹窗文案
- 根据 `resolvedLocale` 实时刷新界面

建议接入：

- `vue-i18n`

## Rust 侧

负责：

- 持久化 `uiLanguage`
- 检测系统语言
- 解析 `resolvedLocale`
- 根据最终语言生成 Tray 菜单文案
- 语言切换后重建 Tray 菜单

## 错误与状态文案策略

优先返回稳定代码，而不是直接返回最终显示文本。

例如：

- `missing_audio_file`
- `transcribe_timeout`
- `asr_empty_result`
- `playback_failed`

由前端将这些代码翻译成最终文案。这样可以避免出现界面为英文、错误提示仍为中文的混搭问题。

## 目录建议

### 前端

```text
src/
  i18n/
    index.ts
    locales/
      zh-CN.ts
      en-US.ts
```

Key 命名建议：

- `common.*`
- `nav.*`
- `overview.*`
- `history.*`
- `dictionary.*`
- `asr.*`
- `llm.*`
- `input.*`
- `sync.*`
- `postProcessing.*`
- `about.*`
- `dialog.*`
- `hud.*`
- `tray.*`
- `errors.*`

### Rust

建议新增：

```text
src-tauri/src/
  i18n.rs
  ui_locale.rs
```

职责建议：

- `ui_locale.rs`：系统语言解析、语言模式与最终语言计算
- `i18n.rs`：Tray 菜单等原生文案生成

## 当前代码落点

本次改造涉及的主要文件：

- `src/main.ts`
- `src/App.vue`
- `src/components/Sidebar.vue`
- `src/stores/settings.ts`
- `src/hud/index.html`
- `src/hud/hud.ts`
- `src-tauri/src/lib.rs`
- `src-tauri/src/commands/settings.rs`

## 分阶段实施计划

### 阶段 1：搭建语言基础设施 ✓

- 在前端设置中新增 `uiLanguage`
- 在 Rust `AppSettings` 中新增 `ui_language`
- 接入 `vue-i18n`
- 建立 `zh-CN` / `en-US` locale 文件
- 提供 `resolvedLocale` 获取方式

交付标准：

- 应用启动后可读取已保存语言
- 主窗口可根据语言切换刷新

### 阶段 2：实现顶部工具条和语言切换入口 ✓

- 将 `content-header` 升级为顶部工具条
- 增加语言 segmented control
- 保留足够 drag region
- 处理 Windows / macOS 下窗口控制与拖拽区域冲突

交付标准：

- 切换入口自然融入框架
- 不破坏现有布局
- 跨平台拖拽行为正常

### 阶段 3：迁移主窗口核心文案 ✓

优先顺序：

- Sidebar
- Overview
- 通用对话框 / 空态
- History
- 各设置页

交付标准：

- 主界面不存在明显硬编码中英文混杂
- 页面切换后文案保持一致

### 阶段 4：接入 Tray 菜单和 HUD ✓

- Rust 侧 Tray 菜单按 `resolvedLocale` 构建
- 语言切换后重建菜单
- HUD 复用前端 locale 资源或共享翻译表

交付标准：

- HUD 与主界面语言一致
- Tray 菜单与主界面语言一致

### 阶段 5：收口错误文案与遗漏项 ✓

- 清理前端提示中的硬编码文本
- 逐步将 Rust 返回的用户可见字符串改为错误码
- 统一空态、状态名、按钮文案

交付标准：

- 不出现“英文界面 + 中文错误提示”或反向混搭

## 风险与约束

### 1. 当前项目存在大量硬编码文案

需要接受这是一次跨组件的结构性迁移，而不是简单替换几个字符串。

### 2. 主窗口、HUD、Tray 分属不同运行环境

多语言不能只停留在 Vue 页面层，需要统一语言状态传播方式。

### 3. 桌面端拖拽区域不能被破坏

顶部语言控件加入后，必须明确区分 `drag-region` 与 `no-drag`。

### 4. 平台差异要前置考虑

macOS 与 Windows 的窗口控制和菜单习惯不同，语言入口和 tray 行为都应按“跨平台最稳”原则设计。

## 本次结论

本项目的多语言改造建议采用以下方案：

- 语言设置使用三档：`system` / `zh-CN` / `en-US`
- 语言切换入口放在顶部全局工具条右侧
- 主窗口与 HUD 由 TypeScript 负责翻译
- Tray 菜单与系统语言解析由 Rust 负责
- 错误与状态优先走“代码 + 翻译”模式

这个方案能在不破坏当前侧边栏结构的前提下，把语言切换做得自然、可发现、跨平台稳定，并为后续继续扩展更多语言留出清晰路径。
