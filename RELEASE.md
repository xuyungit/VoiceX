# Release Process

本文档记录 VoiceX 正式版本的发布流程。正式版包含应用内更新配置；普通本地构建不检查更新，不能作为发布产物。

## 发布前

- 在准备好的 `main` 上确认自上个 tag 以来的功能和修复，更新 `CHANGELOG.md`；中英文 README 按需小幅同步，较大改动需人工参与确认。
- 将 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 和 `src-tauri/Cargo.lock` 中 `voicex` 主包的版本同步为新版本。
- 至少运行 `pnpm build`；涉及 `src-tauri/` 时还要运行 `cargo check`，并按改动补充相关验证。
- 确认 macOS 原有代码签名证书可用。更新应继续使用同一证书，以保留系统权限；不要为了发布重新创建证书。
- 确认本机 updater 私钥和密码可用（`~/.tauri/voicex-updater.key` 和 `~/.tauri/voicex-updater.password`，或对应的 `TAURI_SIGNING_PRIVATE_KEY` 和 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 环境变量），GitHub Actions 中也配置了同名 secrets。不要将私钥或密码提交到仓库。
- 不要提交本地工作笔记、调试目录或生成产物。
- Apple Silicon Mac 构建统一使用 `pnpm tauri`（发布脚本也通过此入口）。入口会下载并校验 `src/shared/localAsrModels.json` 中固定版本的 Qwen 引擎，再作为 sidecar 签名打包。不要直接用 `pnpm exec tauri build` 绕过此步骤。Windows 不打包 Qwen 引擎，模型不随安装包分发。

## 发布步骤

1. 提交版本和文档变更，使用 conventional commit，例如 `chore: release vX.Y.Z`。
2. 在该提交上创建 `vX.Y.Z` tag，推送 `main` 和 tag。`scripts/release-macos.sh` 要求 HEAD 指向该 tag、已跟踪文件无未提交修改，并且 tag 已推送。
3. 准备中英文 release notes 文件，然后在 macOS 上运行：

   ```bash
   pnpm mac:release vX.Y.Z --notes-file /path/to/release-notes.md
   ```

   脚本会创建 GitHub Release 草稿，使用 `tauri.updater.conf.json` 构建正式版并验证 app 签名，上传 DMG、带版本号的 `.app.tar.gz`、其 updater 签名及包含 macOS 条目的 `latest.json`。它不会安装 app 到 `/Applications`。`pnpm mac:build-local` 用于本地构建和安装，不能替代这个步骤。
4. 检查草稿的说明和 macOS 产物，然后发布：

   ```bash
   gh release edit vX.Y.Z --draft=false --latest
   ```

   也可以在第 3 步传入 `--publish`，由脚本上传后发布。发布事件会触发 Windows CI；不要提前发布再上传 macOS 清单，以免两个平台同时修改 `latest.json`。
5. 等待 `.github/workflows/windows-release.yml` 完成。CI 构建并上传签名的 Windows x64 `.exe` 和 `.sig`，将 Windows 条目合并进 `latest.json`。正式版不再提供 MSI。
6. 核对发布状态、工作流结果和全部产物：macOS DMG、macOS 更新包及签名、Windows EXE 及签名，以及 `latest.json` 中的版本、两个平台的下载地址和签名。确认公开的 latest 更新入口返回正确清单；可行时从上一正式版本验证应用内更新。
7. 检查 `https://dl.wddgxs.com/latest.json` 已同步为新版本，并包含 macOS 和 Windows 条目。镜像服务每 10 分钟自动同步最新正式 Release，无需手工上传；Windows CI 完成后通常在下一轮同步补齐 Windows 产物。镜像清单中的下载地址应指向镜像，签名应与 GitHub 清单一致；若尚未同步，检查镜像服务状态或按运维流程手动触发同步。

发布后、Windows CI 完成前，清单暂时只有 macOS 条目；Windows 用户在此期间不会收到该版本的更新。macOS 产物使用本地证书签名，未经过 Apple 公证。
