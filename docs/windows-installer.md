# Windows 安装包

安装包：`dist/installers/xxtab-0.1.3-windows-x64-setup.exe`，旁边的 `.sha256` 文件用于校验。

支持 Windows 10 1809+ 和 Windows 11 x64。默认安装到 `C:\Program Files\xxtab`，创建开始菜单快捷方式和可选桌面快捷方式，可从 Windows“已安装的应用”中卸载。安装向导使用简体中文和程序的 X 图标。安装、运行及卸载需要管理员权限。

安装包仅包含界面程序、命令行程序、许可文件，以及未经修改的官方 WireGuard 0.5.3 x64 MSI。不打包 docs、README、示例配置或诊断脚本；新建配置的模板已内置于程序。没有系统 WireGuard 时自动安装；标准安装路径中已有 WireGuard 时跳过，不升级或降级它。构建时验证 MSI 的固定 SHA256 和 WireGuard LLC 数字签名，安装时再次核对解压后的 SHA256。首次构建下载 MSI 需要联网，完成的安装包不需要联网下载依赖。

打包专用二进制静态链接 C 运行库，不依赖额外安装的 `VCRUNTIME140.dll`。它们与普通 `target/release` 构建分开存放，因此原有便携版不被覆盖。

程序不会随安装自动连接。安装完成后打开 xxtab，导入或新建配置，再点击连接。升级和卸载会检查安装目录的程序文件是否被占用，提示先断开连接并退出，不强制结束正在运行的客户端。

用户配置位于 `%LOCALAPPDATA%\xxtab\profiles`，升级和卸载均保留，也不递归清空应用目录中的用户自建文件。精简安装包不会主动删除旧版遗留的文档或用户修改过的示例。若使用其他管理员账号授权运行，配置属于实际运行程序的账号。

新版安装器仅在首次安装原本不存在的 WireGuard 后，在管理员保护的 `HKLM\Software\xxtab\Dependencies\WireGuard` 中记录官方 MSI 产品码和程序 SHA256，升级会保留该记录。卸载 xxtab 时，若产品码与程序哈希仍匹配，且没有 WireGuard 隧道服务或官方客户端中保存的独立配置，则调用官方 MSI 一并卸载依赖；需要重启时会提示。原先已有、来源不明或后来更换的 WireGuard 保留。卸载失败时显示错误码，日志位于执行卸载账号的 `%LOCALAPPDATA%\xxtab\wireguard-uninstall.log`。

旧版安装器没有来源记录，升级时不会把已存在的 WireGuard 自动认领。旧版留下的依赖需在 Windows“已安装的应用”中手动卸载一次；以后由新版安装器首次安装的依赖才会自动随应用卸载。

## 同名服务和异常退出恢复

新版在启动 WireGuard 前，在当前用户临时目录的 `xxtab-<名称>.lock` 中记录运行时配置路径和 WireGuard 程序路径（不记录密钥）。下次连接时，只有拿到同名独占锁，并确认已有服务的启动参数与记录完全一致，才会卸载上次异常退出残留的服务后重新连接。其他客户端的同名服务、其他用户的服务或缺少有效记录时均不会自动删除；查询权限不足会单独提示。正常断开会清空记录。

旧版异常退出产生的服务没有这份记录，需要先确认来源，再断开对应服务。不要仅根据 `xxtab0` 这个名称删除服务。正常使用请通过托盘“退出”关闭程序，以便清理 WireGuard 和外层服务器绕行路由；恢复机制不能追溯旧进程丢失的路由归属。

## 重新打包

需要 Windows Rust MSVC 工具链、Visual Studio C++ Build Tools 和 Inno Setup 6.5+（本次使用 6.7.1，使用时遵守其许可条款）：

```powershell
cd F:\pro\rust\xxtab
.\tools\build-installer.ps1
```

未自动找到编译器时：

```powershell
.\tools\build-installer.ps1 -Iscc 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

脚本编译 `x86_64-pc-windows-msvc` 静态 CRT 版本到 `target/installer`，校验依赖，读取 Cargo 版本，生成安装 EXE 和 SHA256。`-SkipBuild` 只复用该目录中已构建的安装版程序。安装清单只包含明确列出的运行程序、依赖及许可文件，不会打入源码、测试、文档、脚本、示例、dist 下的旧程序备份或用户配置。

本次安装包没有发布者代码签名证书，EXE 未签名，Windows 的权限提示可能显示“未知发布者”；附带的官方 WireGuard MSI 保留其有效签名。

## 验证

在管理员 PowerShell 且已安装 WireGuard 的环境中执行：

```powershell
.\tools\test-installer.ps1
```

该测试使用独立的测试 AppId、安装目录和快捷方式名称，完成后卸载测试程序。覆盖排除非运行文件、安装文件哈希、CLI 启动、开始菜单和桌面快捷方式、卸载注册信息、检测已有 WireGuard、占用文件时拒绝升级、再次升级、卸载清理以及保留用户自建文件。另通过测试安装器内编译隔离的 MSI 替身，验证依赖来源记录跨升级保留、自带依赖卸载、更换哈希或产品码时保留、存在其他隧道或查询失败时保留、MSI 失败保留记录、需要重启的返回码处理。测试不启动 GUI，不安装、修复或卸载系统 WireGuard；仅保留空的测试注册表命名空间。

缺少 WireGuard 时的首次 MSI 安装分支未在全新虚拟机中实测；当前测试环境已经安装了 WireGuard。对应分支调用官方 MSI 静默安装，检查返回码和安装结果，并处理需要重启的返回码。
