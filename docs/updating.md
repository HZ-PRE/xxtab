# 程序更新

更新源固定为 https://github.com/HZ-PRE/xxtab/releases ，仅使用正式 Release，不使用会过期且可能需要登录的 Actions Artifacts。不需要 GitHub 账号或令牌；公共 API 触发速率限制时稍后重试。

## 使用

- Windows：菜单“检查更新” → 确认下载 → SHA256 校验完成后确认安装。客户端先断开 WireGuard、清理路由并退出，临时更新器等待旧进程结束，再打开安装向导，沿用当前程序目录。向导中完成安装；配置保留在原来的 LocalAppData 目录。若清理失败则取消安装。便携运行时需同时保留 GUI 和同目录的 CLI。
- macOS：应用菜单“检查更新…” → 下载本机架构的 DMG → 确认断开并退出、打开安装包 → 拖入 Applications 替换旧版。此流程不自动覆盖 .app；配置保留在原来的 Application Support 目录。依赖随 App 一起更新，无需 Homebrew；签名和公证要求不变。
- Linux：`xxtab update check` 检查版本；`xxtab update download` 下载并校验最新包，输出 JSON（含文件路径和 SHA256）。正常停止旧进程后解压替换 CLI。也可用 `xxtab update download 0.1.3` 要求版本与检查时一致，防止下载期间 Latest 发生变化。

检查和下载按需运行，不启动常驻更新服务、不定时轮询，不在检查时断开现有连接。下载在后台流式写入临时目录，最多 256 MiB；SHA256 不符、大小不符或超时则拒绝更新并清理未完成的下载。完成的包位于系统临时目录的 `xxtab-update-*`，选择稍后安装时保留，可手动删除。Windows 安装前会再校验一次并限制文件写入。

客户端只接受本仓库、对应版本和架构的文件名；下载仅允许 GitHub HTTPS 域名及官方资源重定向。SHA256 用于检查包完整性，不等同于发布者代码签名；包及校验文件依赖本仓库的发布权限与 HTTPS 信任。只比较三段数字稳定版本，不降级；没有正式 Release 时显示“尚未发布正式版本”。

## 发布新版本

1. 修改 Cargo.toml 中的版本号，运行 `cargo check` 同步 Cargo.lock，并提交修改。
2. 推送代码以及与版本一致的标签，例如本次 `0.1.3`：

```sh
git push
git tag v0.1.3
git push origin v0.1.3
```

3. 等待 GitHub Actions 的 Windows、Linux、Apple Silicon 和 Intel Mac 构建全部成功。发布任务会验证产物 SHA256，再公开 Release，客户端此时才能发现更新。

正式包命名来自 Cargo.toml；Windows/Apple Silicon/Intel/Linux 分别匹配 `xxtab-版本-windows-x64-setup.exe`、`xxtab-版本-macos-arm64.dmg`、`xxtab-版本-macos-x86_64.dmg`、`xxtab-版本-linux-x64.tar.gz`，每个文件旁必须有同名 `.sha256`。

发布失败不会公开半成品。不要移动或覆盖已发布标签；每次修复增加版本号。旧版客户端没有检查更新入口，需要先手动安装一次含更新功能的版本。更新功能不会把 docs、脚本、源码或用户配置加入安装包。
