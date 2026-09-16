# GitHub Actions 自动打包

工作流：`.github/workflows/ci.yml`，名称 **Build, test and package**。仅在推送 `v*` 版本标签时触发，例如 `git push origin v0.1.3`。普通分支 push、PR 和手动 workflow_dispatch 均不触发。请先提交代码和工作流修改，再在该提交上创建版本标签；修改本地文件或仅在本地创建标签不会触发远程构建。

在仓库 Actions 页选择一次成功运行，在页面底部 Artifacts 下载：

| Artifact | 内容 |
| --- | --- |
| xxtab-windows-x64 | 精简 Windows 安装 EXE 与 SHA256 |
| xxtab-macos-arm64 | Apple Silicon DMG、App ZIP 与 SHA256 |
| xxtab-macos-x86_64 | Intel DMG、App ZIP 与 SHA256 |
| xxtab-linux-x64 | Linux CLI tar.gz（程序与 LICENSE）及 SHA256 |

版本号来自 Cargo.toml。Windows 保留官方 WireGuard 离线安装依赖；Mac 从固定源码编译并内置 Bash、wg、wg-quick、wireguard-go。安装包排除 docs、README、示例、诊断脚本和用户配置。Mac 额外上传独立的 `dependency-sources.tar.gz` 及 SHA256，提供第三方对应源码和构建方法；发布任务验证它们齐全后才公开 Release。

Windows/Linux 执行 Rust 测试、rustfmt 和 Clippy。Mac 分别在 `macos-15`（arm64）和 `macos-15-intel` 上编译，额外测试系统路由和真实 WireGuard 接口清理、原生 AppKit 控件、显式停止及失去 GUI 心跳后的清理。失败时不会上传对应平台产物。

推送 `v版本号` 标签时，CI 先检查标签与 Cargo.toml 完全一致，待四个平台构建成功后，汇总安装包和 SHA256，创建草稿 Release、上传全部文件并校验，再公开为 Latest。构建任务使用仓库只读权限，仅发布任务拥有 `contents: write`；使用内置 `GITHUB_TOKEN`，不需要个人令牌。

已公开的版本不会被覆盖；修复后必须增加版本号、使用新标签。失败留下的草稿可通过重跑发布任务补齐。操作步骤和客户端更新入口见 [程序更新](updating.md)。工作流不提交代码，也不安装到用户机器。

Mac 默认 ad-hoc 签名，无 Developer ID 公证；正式发布的签名方式见 `docs/macos.md`。Linux 在 Ubuntu 22.04 构建，目标 glibc 2.35+。Windows 使用静态 CRT，目标 Windows 10 1809+ x64。
