# macOS 客户端与打包

支持 macOS 13 及以上，分别提供 Apple Silicon（arm64）和 Intel（x86_64）包。界面使用 Swift/AppKit，传输与配置校验复用 Rust；不引入 WebView/Electron。macOS 包内自带 Bash、wg、wg-quick 和 wireguard-go，无需用户安装 Homebrew。内存使用包含独立的 wireguard-go 进程，不能直接套用 Windows 的测量结果。

## 安装与使用

从 Release 下载对应架构的 DMG 或 App ZIP。运行依赖已经包含在 App 中，无需额外安装、联网下载组件或修改系统 PATH。

打开对应架构 DMG，将 `xxtab.app` 拖入 Applications。首次启动可从 Finder 右键打开；CI 默认使用 ad-hoc 签名，没有 Apple Developer ID 公证，部分系统策略可能阻止运行。正式分发建议配置后文的 Developer ID 签名和公证，不需要关闭系统全局安全检查。

界面支持连接、断开、重新连接、配置选择、新建、导入 `.toml` / `.conf`、编辑、只读查看和有界日志。默认 Endpoint 与 listen 均为 `127.0.0.1:51820`，remote 为 `127.0.0.1:7007`。配置语法与 Windows 共用；从 Windows 导入后，如使用私有 CA，请把 `ca_file` 改为 Mac 上的绝对路径。

最小化主窗口后，程序隐藏到顶部菜单栏，程序坞（Dock）不再显示 App，隧道继续运行；已打开的配置编辑窗口一并隐藏，未保存内容保留。通过菜单栏“显示窗口”恢复窗口和程序坞图标。关闭主窗口也会保留菜单栏图标与隧道。菜单栏提供连接、断开、重新连接、退出；退出会先停止隧道并清理。菜单/按钮会按连接和编辑状态启用。

配置目录：`~/Library/Application Support/xxtab/profiles`。导入不修改原文件，保存保留完整历史版本。私钥仅放在私有配置/会话文件中，不放在启动命令或日志里。App 内已包含新建配置模板，不需要安装示例文件。

## 权限与运行方式

界面以普通用户运行。点击连接时，系统通过 AppleScript 的 `with administrator privileges` 请求管理员授权，启动 App 内的 Rust 会话进程；不保存密码，不设置免密 sudo，不安装长期驻留的提权服务。

提权进程只接受私有会话目录中的配置数据，固定调用当前 App 的 `Contents/Resources/wireguard/wg-quick`，GUI 模式忽略配置中的 `wireguard.executable`。子进程仅使用内置组件目录与 macOS 系统目录，清除继承的环境变量。GUI 使用按用户 UID 固定的接口逻辑名称 `xxm<uid>`，实际系统接口由 WireGuard 分配为 utun。界面持有私有会话文件的独占锁，描述符设置 close-on-exec，后台据此判断界面进程是否仍然存活；不发送心跳，也不因界面停止刷新而断开。最小化、主线程卡顿、后台节流或暂停界面进程均不会释放该锁。用户主动断开、退出程序、界面进程真正退出，或后台收到终止信号时，会话进程执行停止和清理；系统命令本身有超时，所以故障清理可能额外耗时。

旧版主线程心跳可能因后台节流或界面阻塞超过 10 秒而触发自动停止。新版已取消心跳超时机制；界面进程退出、存活锁检查失败、断开请求和终止信号会分别记录原因，异常结束会标明意外停止。合盖或系统睡眠仍可能中断实际网络通信，外层连接失败后由传输层重连；此机制不阻止系统睡眠，也不保证睡眠期间保持网络可用。

会话目录以目录句柄固定，拒绝符号链接、非普通文件和非私有输入；root 只把运行配置写入自身的私有临时目录。日志最多保留 128 行，只在状态或内容变化时写快照。截图/日志分享前仍需确认不含自己的网络信息。

命令行也可以在终端使用：

```bash
./xxtab check ./xxtab.toml
sudo ./xxtab run ./xxtab.toml
```

CLI 支持 Ctrl+C/SIGTERM 清理。macOS 通过 `route` 维护到外层服务器的主机路由，保留已有路由；WireGuard 地址、DNS、内网路由通过系统 `wg-quick` 管理。

使用 App 内 CLI 时同样使用内置依赖。单独从源码编译、位于 App 外部的 CLI 仍支持标准 Homebrew 依赖（`brew install bash wireguard-tools wireguard-go`）。完整 App 缺少组件时会明确报错，不会悄悄切换到构建机器上的 Homebrew。

## 从源码打包

需要 Rust 1.92、Xcode Command Line Tools、Python 3.12+、Go 1.26.8；构建阶段需联网下载固定版本源码和 Go 模块。无需安装 Homebrew 的运行组件：

```bash
xcode-select --install
bash tools/build-macos.sh
```

脚本按运行机器架构编译 Rust 和 AppKit，生成图标、签名、运行原生界面烟雾测试，再生成：

```text
dist/installers/xxtab-0.1.3-macos-arm64.dmg
dist/installers/xxtab-0.1.3-macos-arm64.app.zip
# Intel 对应 x86_64；旁边有各自 .sha256。
```

包内仅含 AppKit/Rust 程序、图标、元数据、五个必要运行文件（Bash、wg、wireguard-go、wg-quick 启动脚本及上游脚本）和许可证。依赖从 `packaging/macos/dependencies.json` 固定的源码与补丁编译，逐项校验 SHA256，禁用 Bash 的外部 readline/gettext 依赖；检查内置依赖二进制只链接 macOS 系统库，并逐个签名。docs、源码、测试、诊断脚本和示例配置不进入 App。

由于分发 GPL 的 Bash 和 wireguard-tools，同版本 Release 另外提供 `xxtab-版本-macos-架构-dependency-sources.tar.gz` 及 SHA256，其中包含对应上游源码、补丁及构建脚本。该附件供源码获取与重建，不需要用户安装。App 的许可证目录中也记录附件名称。

有 Developer ID 证书及已配置的 notarytool 钥匙串凭据时：

```bash
MACOS_SIGNING_IDENTITY='Developer ID Application: Your Name (TEAMID)' \
MACOS_NOTARY_PROFILE='your-notary-profile' \
bash tools/build-macos.sh
```

CI 默认不读取签名凭据，不提交公证。上述变量仅用于已经安装证书和凭据的构建环境。

## 当前验证范围

开发环境为 Windows + Debian/WSL：已运行两平台现有 Rust 测试与配置桥接测试；macOS 会话模块的可移植文件权限/符号链接拒绝逻辑在 Linux 审核夹具中编译测试。Swift 做了语法解析，构建脚本通过 Bash 语法检查。

AppKit 类型检查、macOS 路由和 WireGuard 生命周期、DMG 创建必须在 Mac 上执行；本地没有运行这些项目。GitHub Actions 会把完整 App 复制到包含空格的新路径，在仅包含系统命令的 PATH 下检查组件版本、架构、系统动态库链接、最低系统版本、代码签名、配置桥接与原生界面，并实际测试 WireGuard 接口启停、界面所属测试进程暂停超过旧超时后会话仍存活、显式停止和所属进程退出后的清理。还会模拟内置 Bash 缺失，确保不回退 Homebrew。生命周期夹具使用本地 WebSocket 接收器及测试密钥，验证就绪和清理，不代表验证了远端 WireGuard 握手或公网吞吐。
