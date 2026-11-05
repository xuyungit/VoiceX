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

The script writes 12 PNGs under `assets/screenshots/{zh,en}/` at 2× pixel density.
It uses light Overview/settings pages, dark History, and the native stream HUD's
256 × 100 logical size. Review every image before publishing. The images show
frontend content without a native title bar; they do not demonstrate live ASR,
TTS, or Windows validation.

脚本会在 `assets/screenshots/{zh,en}/` 写入 12 张 2 倍像素密度的 PNG：浅色概览和设置页、
深色历史页，以及原生流式 HUD 的 256 × 100 逻辑尺寸。发布前应逐张检查。
截图仅展示前端内容，不含原生标题栏，也不代表真实 ASR、TTS 或 Windows 验证。

For manual inspection, use `?lang=zh` or `?lang=en` and optionally `&theme=dark`
on `/overview`, `/history`, `/asr-settings`, `/input-settings`, or `/reading-settings`.
Stop the server with Ctrl+C when finished. Neither script changes production UI
code or persists application settings.

手动预览可在上述路由加 `?lang=zh` 或 `?lang=en`，并可加 `&theme=dark`。
完成后按 Ctrl+C 停止服务器。脚本不会修改正式界面代码或持久化应用设置。
