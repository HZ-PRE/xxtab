# macOS 客户端与打包

支持 macOS 13 及以上，分别提供 Apple Silicon（arm64）和 Intel（x86_64）包。界面使用 Swift/AppKit，传输与配置校验复用 Rust；不引入 WebView/Electron。macOS 的 WireGuard 由 Homebrew 的 `wireguard-go` 提供，内存使用包含该独立进程，不能直接套用 Windows 的测量结果。

## 安装与使用

先安装 [Homebrew](https://brew.sh/)，再执行：

```bash
brew install bash wireguard-tools wireguard-go
```

打开对应架构 DMG，将 `xxtab.app` 拖入 Applications。首次启动可从 Finder 右键打开；CI 默认使用 ad-hoc 签名，没有 Apple Developer ID 公证，部分系统策略可能阻止运行。正式分发建议配置后文的 Developer ID 签名和公证，不需要关闭系统全局安全检查。

界面支持连接、断开、重新连接、配置选择、新建、导入 `.toml` / `.conf`、编辑、只读查看和有界日志。默认 Endpoint 与 listen 均为 `127.0.0.1:51820`，remote 为 `127.0.0.1:7007`。配置语法与 Windows 共用；从 Windows 导入后，如使用私有 CA，请把 `ca_file` 改为 Mac 上的绝对路径。

最小化或关闭主窗口会保留菜单栏图标与隧道。通过菜单栏“显示窗口”恢复；菜单栏也提供连接、断开、重新连接、退出。退出会先停止隧道并清理。菜单/按钮会按连接和编辑状态启用。

配置目录：`~/Library/Application Support/xxtab/profiles`。导入不修改原文件，保存保留完整历史版本。私钥仅放在私有配置/会话文件中，不放在启动命令或日志里。App 内已包含新建配置模板，不需要安装示例文件。

## 权限与运行方式

界面以普通用户运行。点击连接时，系统通过 AppleScript 的 `with administrator privileges` 请求管理员授权，启动 App 内的 Rust 会话进程；不保存密码，不设置免密 sudo，不安装长期驻留的提权服务。

提权进程只接受私有会话目录中的配置数据，固定调用标准 Homebrew 目录的 `wg-quick`，GUI 模式忽略配置中的 `wireguard.executable`。GUI 使用按用户 UID 固定的接口逻辑名称 `xxm<uid>`，实际系统接口由 WireGuard 分配为 utun。GUI 每秒续期心跳；请求断开、正常退出或心跳停止约 10 秒后，会话进程调用原有停止逻辑。系统命令本身有超时，所以故障清理可能额外耗时。

会话目录以目录句柄固定，拒绝符号链接、非普通文件和非私有输入；root 只把运行配置写入自身的私有临时目录。日志最多保留 128 行，只在状态或内容变化时写快照。截图/日志分享前仍需确认不含自己的网络信息。

命令行也可以在终端使用：

```bash
./xxtab check ./xxtab.toml
sudo ./xxtab run ./xxtab.toml
```

CLI 支持 Ctrl+C/SIGTERM 清理。macOS 通过 `route` 维护到外层服务器的主机路由，保留已有路由；WireGuard 地址、DNS、内网路由通过系统 `wg-quick` 管理。

## 从源码打包

需要 Rust 1.92、Xcode Command Line Tools、Python 3 和上述 Homebrew 依赖：

```bash
xcode-select --install
bash tools/build-macos.sh
```

脚本按运行机器架构编译 Rust 和 AppKit，生成图标、签名、运行原生界面烟雾测试，再生成：

```text
dist/installers/xxtab-0.1.1-macos-arm64.dmg
dist/installers/xxtab-0.1.1-macos-arm64.app.zip
# Intel 对应 x86_64；旁边有各自 .sha256。
```

包内仅含 AppKit 程序、Rust 程序、应用图标、必要元数据和 LICENSE。WireGuard 由系统 Homebrew 安装，docs、源码、测试、诊断脚本、示例配置不进入安装包。

有 Developer ID 证书及已配置的 notarytool 钥匙串凭据时：

```bash
MACOS_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)' \
MACOS_NOTARY_PROFILE='your-notary-profile' \
bash tools/build-macos.sh
```

CI 默认不读取签名凭据，不提交公证。上述变量仅用于已经安装证书和凭据的构建环境。

## 当前验证范围

开发环境为 Windows + Debian/WSL：已运行两平台现有 Rust 测试与配置桥接测试；macOS 会话模块的可移植文件权限/符号链接拒绝逻辑在 Linux 审核夹具中编译测试。Swift 做了语法解析，构建脚本通过 Bash 语法检查。

AppKit 类型检查、macOS 路由和 WireGuard 生命周期、DMG 创建必须在 Mac 上执行；本地没有运行这些项目。GitHub Actions 已配置这部分检查，包括真实界面构造、配置桥接、WireGuard 接口启停、显式停止和心跳丢失清理。生命周期夹具使用本地 WebSocket 接收器及测试密钥，验证就绪和清理，不代表验证了远端 WireGuard 握手或公网吞吐。
