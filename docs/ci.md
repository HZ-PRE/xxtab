# GitHub Actions 自动打包

工作流：`.github/workflows/ci.yml`，名称 **Build, test and package**。push、pull request 和手动 workflow_dispatch 都会运行。将这些改动提交并推送到 GitHub 后生效；修改本地文件不会自动触发远程构建。

在仓库 Actions 页选择一次成功运行，在页面底部 Artifacts 下载：

| Artifact | 内容 |
| --- | --- |
| xxtab-windows-x64 | 精简 Windows 安装 EXE 与 SHA256 |
| xxtab-macos-arm64 | Apple Silicon DMG、App ZIP 与 SHA256 |
| xxtab-macos-x86_64 | Intel DMG、App ZIP 与 SHA256 |
| xxtab-linux-x64 | Linux CLI tar.gz（程序与 LICENSE）及 SHA256 |

版本号来自 Cargo.toml。Windows 保留官方 WireGuard 离线安装依赖；Mac 使用系统 Homebrew WireGuard。所有产物排除 docs、README、示例、诊断脚本和用户配置。

Windows/Linux 执行 Rust 测试、rustfmt 和 Clippy。Mac 分别在 `macos-15`（arm64）和 `macos-15-intel` 上编译，额外测试系统路由和真实 WireGuard 接口清理、原生 AppKit 控件、显式停止及失去 GUI 心跳后的清理。失败时不会上传对应平台产物。

工作流仅需仓库只读权限，使用 GitHub 的 artifact 存储，不自动发布 Release、不提交代码、不安装到用户机器。fork PR 不需要任何仓库 secret。

Mac 默认 ad-hoc 签名，无 Developer ID 公证；正式发布的签名方式见 `docs/macos.md`。Linux 在 Ubuntu 22.04 构建，目标 glibc 2.35+。Windows 使用静态 CRT，目标 Windows 10 1809+ x64。
