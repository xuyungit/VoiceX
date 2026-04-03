# Release Process

本文档记录 VoiceX 当前使用中的固定发布流程。

目标是把每次 release 都收敛到同一套步骤，避免遗漏版本号、变更记录、构建产物或 GitHub Release 操作。

## 适用范围

当前仓库的发布方式是：

- macOS 安装包在本地构建，使用 `pnpm mac:build-local`
- 每次正式 release 都必须创建 GitHub Release，并把 macOS 安装包上传到该 Release
- Windows 安装包通过 GitHub Actions 构建并上传到 GitHub Release
- 发布前通常会更新 `CHANGELOG.md`
- 如有必要，会小幅更新中英文 README
- 如果改动较大，README 内容需要人工参与确认，不默认由发布流程自动兜底

## 发布前原则

- 只在准备好的 `main` 上发版
- 先确认本次 release 的功能和修复范围，再进入版本号和打包步骤
- 不要把本地工作笔记或调试目录混入发布提交，例如 `.local-notes/`、`.claude/`
- `dist/` 是构建产物，不手改
- `docs/` 和 `swift_ui_references/` 只放参考资料，不作为发布产物目录

## 版本号同步位置

每次发版时，版本号必须同时更新以下 3 处：

- `package.json`
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`

版本 tag 使用 `vX.Y.Z` 格式，例如：

- `v0.9.2`

## 标准发布步骤

### 1. 确认发布内容

- 确认 `main` 上本次要发布的提交范围
- 如有必要，先补最后一轮问题修复
- 确认是否需要更新 `CHANGELOG.md`
- 确认是否需要小幅更新 `README.md`、`README.en.md`
- 如果涉及较大功能变动、文案重写或对外描述变化，README 需人工确认

### 2. 执行发布前校验

至少执行：

```bash
pnpm build
```

如果本次改动涉及 `src-tauri/`，还应执行：

```bash
cd src-tauri
cargo check
cd ..
```

如果改动集中在某个高风险模块，最好再补一轮和改动直接相关的手动验证。

### 3. 更新版本号

把版本号同步更新到：

- `package.json`
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`

### 4. 更新变更记录

更新：

- `CHANGELOG.md`

要求：

- 为新版本新增一节，沿用当前 changelog 风格
- 优先记录用户可感知的新增、变更、修复
- 不要把尚未验证完成的事项写成已经完成

### 5. 如有必要，更新 README

按需更新：

- `README.md`
- `README.en.md`

适用场景：

- 新增用户会直接接触的能力
- 安装、权限、构建、发布方式发生变化
- ASR/LLM provider、平台支持、操作方式有明显变化

注意：

- 仅做小幅同步更新时，可随 release 一起提交
- 如果是较大改动，必须人工参与确认内容，不默认在发布步骤里一次性改完

### 6. 本地构建 macOS 安装包

在 macOS 上执行：

```bash
pnpm mac:build-local
```

说明：

- 该命令会构建 Release 版本
- 使用本地签名身份进行签名
- 安装到 `/Applications/VoiceX.app`
- 默认还会产出可上传到 GitHub Release 的 macOS 安装包，通常位于 `src-tauri/target/release/bundle/dmg/`
- 适合当前项目的本地 macOS 发包方式

如果本机尚未准备本地签名身份，先执行一次：

```bash
pnpm mac:setup-signing
```

### 7. 提交 release 变更

提交信息沿用 conventional commits，建议使用：

```bash
git add package.json src-tauri/Cargo.toml src-tauri/tauri.conf.json CHANGELOG.md README.md README.en.md
git commit -m "chore: release vX.Y.Z"
```

注意：

- 实际 `git add` 时按本次改动范围增减文件
- 不要把无关改动或本地未跟踪目录一起提交

### 8. 打 tag

示例：

```bash
git tag vX.Y.Z
```

### 9. 推送提交和 tag

```bash
git push origin main
git push origin vX.Y.Z
```

### 10. 发布 GitHub Release

在 GitHub 上基于刚推送的 tag 创建并发布 Release。

建议使用 `gh` 直接操作，至少完成：

```bash
gh release create vX.Y.Z --title "vX.Y.Z" --notes-file /path/to/release-notes.md
gh release upload vX.Y.Z /path/to/VoiceX_X.Y.Z_aarch64.dmg --clobber
```

要求：

- 不能只打 tag 不创建 GitHub Release
- 不能只创建 draft 后停在那里；正式 release 必须进入 published 状态
- macOS 安装包必须上传到对应的 GitHub Release
- 如果本次 release 有多架构或额外 macOS 产物，也应一并上传

注意：

- Windows 自动构建依赖 GitHub Release 进入 published 状态
- 如果只创建 draft 而不发布，Windows workflow 不会自动开始

### 11. 等待 Windows 安装包构建完成

仓库中已有 Windows release workflow：

- `.github/workflows/windows-release.yml`

该流程会：

- 基于 tag 对应源码构建 Windows bundle
- 校验 tag 对应提交属于 `main`
- 将构建产物上传到对应的 GitHub Release

### 12. 上传或核对 Release 产物

最终检查 GitHub Release 页面至少包含：

- macOS 本地构建出来并已上传的安装包，至少包含当次 release 对应的 `.dmg`
- Windows workflow 生成的安装包
- 正确的 release notes 或 changelog 摘要

## 建议的发布核对清单

每次发版前，至少确认以下项目都完成：

- 发布范围已确认
- `pnpm build` 已通过
- 如果涉及 `src-tauri/`，`cargo check` 已通过
- 三处版本号已同步
- `CHANGELOG.md` 已更新
- README 是否需要更新已确认
- macOS 已执行 `pnpm mac:build-local`
- macOS 安装包已实际上传到 GitHub Release
- release commit 已创建
- tag 已创建并 push
- GitHub Release 已发布
- Windows workflow 已成功完成
- Release 页面产物已核对

## 当前约定

- tag 格式使用 `vX.Y.Z`
- release commit 建议使用 `chore: release vX.Y.Z`
- changelog 继续沿用现有 Keep a Changelog 风格
- macOS 继续采用本地构建与本地签名流程
- 每次正式 release 都要在 GitHub 上创建对应 Release，并上传 macOS 安装包
- Windows 继续采用 GitHub Actions 自动构建上传流程
