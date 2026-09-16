# Windows 安装包

安装包：`dist/installers/xxtab-0.1.1-windows-x64-setup.exe`，旁边的 `.sha256` 文件用于校验。

支持 Windows 10 1809+ 和 Windows 11 x64。默认安装到 `C:\Program Files\xxtab`，创建开始菜单快捷方式和可选桌面快捷方式，可从 Windows“已安装的应用”中卸载。安装向导使用简体中文和程序的 X 图标。安装、运行及卸载需要管理员权限。

安装包包含界面程序、命令行程序、示例、说明文档，以及未经修改的官方 WireGuard 0.5.3 x64 MSI。没有系统 WireGuard 时自动安装；标准安装路径中已有 WireGuard 时跳过，不升级或降级它。构建时验证 MSI 的固定 SHA256 和 WireGuard LLC 数字签名，安装时再次核对解压后的 SHA256。首次构建下载 MSI 需要联网，完成的安装包不需要联网下载依赖。

打包专用二进制静态链接 C 运行库，不依赖额外安装的 `VCRUNTIME140.dll`。它们与普通 `target/release` 构建分开存放，因此原有便携版不被覆盖。

程序不会随安装自动连接。安装完成后打开 xxtab，导入或新建配置，再点击连接。升级和卸载会检查安装目录的程序文件是否被占用，提示先断开连接并退出，不强制结束正在运行的客户端。

用户配置位于 `%LOCALAPPDATA%\xxtab\profiles`，升级和卸载均保留。卸载不删除系统 WireGuard，也不递归清空应用目录中的用户自建文件。示例配置升级时不会覆盖已有文件。若使用其他管理员账号授权运行，配置属于实际运行程序的账号。

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

脚本编译 `x86_64-pc-windows-msvc` 静态 CRT 版本到 `target/installer`，校验依赖，读取 Cargo 版本，生成安装 EXE 和 SHA256。`-SkipBuild` 只复用该目录中已构建的安装版程序。安装清单只包含明确列出的程序和公共示例，不会打入 dist 下的旧程序备份或用户配置。

本次安装包没有发布者代码签名证书，EXE 未签名，Windows 的权限提示可能显示“未知发布者”；附带的官方 WireGuard MSI 保留其有效签名。

## 验证

在管理员 PowerShell 且已安装 WireGuard 的环境中执行：

```powershell
.\tools\test-installer.ps1
```

该测试使用独立的测试 AppId、安装目录和快捷方式名称，完成后卸载测试程序。覆盖安装文件哈希、CLI 启动、开始菜单和桌面快捷方式、卸载注册信息、检测已有 WireGuard、占用文件时拒绝升级、再次升级、保留修改过的示例、卸载清理以及保留用户自建文件。测试不启动 GUI，不安装或修复系统 WireGuard。

缺少 WireGuard 时的首次 MSI 安装分支未在全新虚拟机中实测；当前测试环境已经安装了 WireGuard。对应分支调用官方 MSI 静默安装，检查返回码和安装结果，并处理需要重启的返回码。
