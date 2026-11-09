# Windows 安装包

安装包：`dist/installers/xxtab-版本-windows-x64-setup.exe`，旁边的 `.sha256` 文件用于校验。

支持 Windows 10 1809+ 和 Windows 11 x64。默认安装到 `C:\Program Files\xxtab`，创建开始菜单快捷方式和可选桌面快捷方式，可从 Windows“已安装的应用”中卸载。安装向导使用简体中文和程序的 X 图标。安装、运行及卸载需要管理员权限。

安装包仅包含界面程序、命令行程序、私有 WireGuard 运行时及许可文件。不打包 docs、README、示例配置或诊断脚本；新建配置的模板已内置于程序。运行时从未经修改的官方 WireGuard 0.5.3 x64 MSI 提取，在安装包中命名为 `xxtab\runtime\wd.exe`，不会安装或启动独立的 WireGuard 桌面客户端，也不会修改本机已有的 WireGuard。构建时验证 MSI 和提取运行时的 WireGuard LLC 数字签名，并校验 MSI 的固定 SHA256。首次构建下载 MSI 需要联网，完成的安装包不需要联网下载依赖。

打包专用二进制静态链接 C 运行库，不依赖额外安装的 `VCRUNTIME140.dll`。它们与普通 `target/release` 构建分开存放，因此原有便携版不被覆盖。

程序不会随安装自动连接。安装完成后打开 xxtab，导入或新建配置，再点击连接。升级和卸载会检查安装目录的程序文件是否被占用，提示先断开连接并退出，不强制结束正在运行的客户端。

Windows 的服务器绕行路由改用系统 IP Helper API 查询、添加和清理，不依赖 `Get-NetRoute`、`Find-NetRoute` 或 `New-NetRoute` PowerShell cmdlet，因此不会因 NetTCPIP 提供程序加载或刷新网卡而卡在 PowerShell 超时。程序只管理本次运行成功创建的服务器 IPv4 `/32` 路由；已有同一主机路由会保留。

当前用户 SID、WireGuard 服务状态及服务启动信息均改用 Windows 原生 API 获取。`run` 连接流程不再启动 PowerShell，因此无需关闭 ConstrainedLanguage 或修改系统安全策略。

用户配置位于 `%LOCALAPPDATA%\xxtab\profiles`，升级和卸载均保留，也不递归清空应用目录中的用户自建文件。精简安装包不会主动删除旧版遗留的文档或用户修改过的示例。若使用其他管理员账号授权运行，配置属于实际运行程序的账号。

卸载 xxtab 时，应用私有运行时会和程序文件一同删除。安装器不会卸载或修改任意已有的系统 WireGuard；旧版 xxtab 曾安装的独立 WireGuard 也会保留，需由使用者在 Windows“已安装的应用”中自行决定是否删除。

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

本次安装包没有发布者代码签名证书，EXE 未签名，Windows 的权限提示可能显示“未知发布者”；私有携带的官方 WireGuard 运行时保留其有效签名。

## 验证

在管理员 PowerShell 且已安装 WireGuard 的环境中执行：

```powershell
.\tools\test-installer.ps1
```

该测试使用独立的测试 AppId、安装目录和快捷方式名称，完成后卸载测试程序。覆盖排除非运行文件、安装文件哈希、私有运行时签名、CLI 启动、开始菜单和桌面快捷方式、卸载注册信息、占用文件时拒绝升级、再次升级、卸载清理以及保留用户自建文件。测试不启动 GUI 或隧道，也不安装、修复或卸载系统 WireGuard。
