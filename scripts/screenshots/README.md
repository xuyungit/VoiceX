# README screenshots / README 截图

These scripts render the current Vue pages and HUD with an isolated, in-memory
Tauri command fixture. They never connect to the desktop backend or read the
local database, personal history, recordings, or credentials. The capture script
blocks requests outside the local preview server.

这些脚本通过独立的内存 Tauri 接口替身渲染当前 Vue 页面和 HUD，不连接桌面后端，
不读取本机数据库、个人历史、录音或密钥。截图脚本会阻止访问本地预览服务器以外的地址。

`usage-snapshot.json` contains only the aggregate usage counters and the few
non-secret Overview display settings approved for the README on 2026-11-05.
The snapshot matches `get_usage_stats` and `get_local_usage_stats`; both languages
render the same values. It contains no device IDs or dictation text. All history
entries, device names, and settings on the other pages are synthetic. Connection
tests and speech synthesis are not run by this fixture.

`usage-snapshot.json` 保存 2026-11-05 经用户同意用于 README 的汇总统计及概览所需的少量
非敏感设置。统计与 `get_usage_stats`、`get_local_usage_stats` 的字段一致，中英文共用同一份
数值；其中没有设备 ID 或听写正文。其他页面的历史、设备名称和配置均为示例。
此替身不会执行连接测试或语音合成。

## Capture / 截图

From the repository root, start the isolated preview:

在仓库根目录启动独立预览：

```sh
node scripts/screenshots/preview.mjs
```

Then use Playwright CLI in another terminal:

然后在另一个终端中使用 Playwright CLI：

```sh
npx --package @playwright/cli playwright-cli -s=readme open 'http://127.0.0.1:1523/overview?lang=zh' --headed
npx --package @playwright/cli playwright-cli -s=readme run-code --filename=scripts/screenshots/capture.js
npx --package @playwright/cli playwright-cli -s=readme close
```

The script writes 34 PNGs under `assets/screenshots/{zh,en}/` at 2× pixel density.
Main-window pages are captured at the app's default 900 × 650 window size, each
in light (`<name>.png`) and dark (`<name>-dark.png`); the READMEs use `<picture>`
so GitHub shows the one matching the reader's theme. The stream HUD and the caption
render in the window sizes `src-tauri/src/hud/window.rs` gives them on a 1120 × 700
screen, and each image is cropped to the capsule and its shadow. The stream HUD's
last words are still unconfirmed, so they show grey. The first-launch images are the Overview permission
card (the fixture reports permissions missing) and the macOS drag helper at its
420 × 160 window size with a transparent background; System Settings itself is
not captured. Reading examples include the translate-and-read settings
and a transparent caption HUD driven by synthetic `state:reading` / `state:caption`
events.
The HUD, caption, and drag helper look the same in both themes and are captured once.
Review every image before publishing. The images show
frontend content without a native title bar; they do not demonstrate live ASR,
TTS, or Windows validation.

脚本会在 `assets/screenshots/{zh,en}/` 写入 34 张 2 倍像素密度的 PNG。主窗口页面按应用默认的
900 × 650 窗口尺寸截取，浅色（`<name>.png`）和深色（`<name>-dark.png`）各一张；README 用
`<picture>` 让 GitHub 按读者的主题显示对应的一张。流式 HUD 和朗读字幕按 `src-tauri/src/hud/window.rs` 在 1120 × 700
屏幕上给出的窗口尺寸渲染，截图裁到胶囊及其阴影。流式 HUD 最后几个字仍未确认，显示为灰色。
首次启动的两张图分别是概览中的权限卡片（替身报告权限缺失）和 macOS 拖拽助手（420 × 160
窗口尺寸、透明背景）；不截取系统设置本身。朗读示例包括翻译朗读设置，
以及通过模拟 `state:reading` / `state:caption` 事件驱动的透明字幕 HUD。HUD、字幕和拖拽助手在两种主题下外观相同，只截一张。
发布前应逐张检查。
截图仅展示前端内容，不含原生标题栏，也不代表真实 ASR、TTS 或 Windows 验证。

For manual inspection, use `?lang=zh` or `?lang=en` and optionally `&theme=dark`
on `/overview`, `/history`, `/asr-settings`, `/input-settings`, or `/reading-settings`.
For reading captions, open `/src/hud/index.html?lang=zh&hud=reading` (or `lang=en`).
Add `&perm=missing` to `/overview` for the permission card, and open
`/src/permission-guide/index.html?lang=zh` for the drag helper.
Stop the server with Ctrl+C when finished. Neither script changes production UI
code or persists application settings.

手动预览可在上述路由加 `?lang=zh` 或 `?lang=en`，并可加 `&theme=dark`。
朗读字幕可通过 `/src/hud/index.html?lang=zh&hud=reading`（或 `lang=en`）预览。
在 `/overview` 加 `&perm=missing` 可预览权限卡片，拖拽助手可打开
`/src/permission-guide/index.html?lang=zh`。
完成后按 Ctrl+C 停止服务器。脚本不会修改正式界面代码或持久化应用设置。
