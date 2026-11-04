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
- 原生 AppKit 及完整 Chrome 语义树已完成受控场景；广泛应用覆盖尚未完成。
- 超过 16,384 个 UTF-16 单元的字段目前明确拒绝，尚未实现大文档局部范围读取；快改快发和长时间资源表现仍需验证。
- 输入法状态在平台提供能力时检查：macOS 使用 SDK 的 `AXTextInputMarkedTextMarkerRange`，Windows 使用 TextEdit 的 active composition；未知能力不会标为已提交。已知组词期间跳过正文，歧义样本可继续观察；只有后续一致读取确认稳定、且没有更新的未解决输入，才保存新对照。旧记录缺少稳定性证据时在页面标明，不自动改写或删除原记录。
- Windows 目前没有 UIA 文本或焦点订阅，仅使用 10/30 秒等检查点与收尾；聊天中的快速修改后发送可能完全漏检。这是后续事件适配工作，不能由编译通过推断与 macOS 覆盖相当。
- 本机截图、原始计时样本和逐次调试记录不纳入提交。
