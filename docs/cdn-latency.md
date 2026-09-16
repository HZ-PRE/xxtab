# Cloudflare/CDN + nginx 的内网访问延迟

路径是：内网应用 → WireGuard → xxtab → Cloudflare 边缘节点 → nginx → wstunnel → WireGuard 服务端 → 内网主机。绕路、丢包重传和各段 TCP 排队都可能增加延迟。没有你的公网测量结果，不能断言某个节点就是瓶颈。

## 可选低延迟模式（0.1.1）

在 GUI 中断开连接，编辑隧道 TOML，添加或修改以下独立的表，保存后重新连接。已经有 `[transport]` 时修改其内容，不要重复添加。

```toml
[transport]
latency_mode = "interactive"
diagnostics_interval_secs = 15
```

模式保留 32 包队列、约 66 KiB 复用载荷缓冲和 100 ms 排队期限。优先发送不超过 256 字节的小报文，常见的加密 ACK、小请求及 WireGuard 握手可能受益；程序不解密或识别内部应用。每最多四个小包让出一个大包机会，保持大包进度。队列满时，小包可替换一个待发大包。同类报文保持 FIFO，不同大小的报文可能重新排序。

Linux 使用 `TCP_NOTSENT_LOWAT=16 KiB` 减少尚未发到网络的数据堆积，保留内核的在途数据窗口管理；内核不支持时记录提示，仅启用小包优先。Windows 将外层 TCP 发送缓冲设置为 128 KiB，**高 RTT 链路上的批量吞吐可能下降**。Windows 设置不能等价实现 Linux 的“只限制未发送数据”，也不能控制 CDN 或服务端的发送排队。

不配置或设为 `balanced` 时保留原来的 FIFO 和系统 TCP 缓冲行为。建议在同一目标上对比远程桌面响应、网页打开速度和文件传输，不合适时改回 `balanced`。没有自动修改已保存的用户配置。

## 日志

默认有流量时约每 15 秒输出一次 `transport health`，在 GUI 和 CLI 中均可查看；`diagnostics_interval_secs = 0` 可关闭。日志容量仍有限制。

| 字段 | 含义 |
| --- | --- |
| `tx` / `rx` | 统计区间内转发的加密 UDP 载荷速率 |
| `ws_rtt` | 最近一次匹配 WebSocket Ping/Pong 的往返时间，包含客户端当时的写入等待 |
| `sample_age` | RTT 样本距今秒数；旧样本不能代表当前负载延迟 |
| `queue_max` | 区间内被取出的报文在应用发送队列里的最大等待 |
| `write_max` | 区间内成功完成的批次的最大写入／刷新耗时 |
| `queue full` / `expired` | 每 5 秒汇总的应用队列拥塞／过期丢包 |

Pong 可能由代理处理，不能把 `ws_rtt` 当成内网主机 RTT 或 WireGuard 握手验证。内核、CDN 或服务端的排队不计入 `queue_max`；`write_max` 不包含当前仍阻塞的写入。空闲无流量时不持续输出健康日志。

## Windows 排查

连接后，在 PowerShell 中替换为自己的域名、内网 IP 和开放的服务端口：

```powershell
.\tools\diagnose-network.ps1 -Server vpn.example.com -TargetIP 10.0.0.10 -TargetPort 3389
# 同时对比内部域名解析：
.\tools\diagnose-network.ps1 -Server vpn.example.com -TargetIP 10.0.0.10 -TargetPort 443 -TargetName intranet.example.com
```

安装版脚本位于安装目录的 `tools` 下。脚本只解析 DNS、查看路由／MTU、测 ICMP 和 TCP 建连，不读取私钥或 path_prefix，不修改网络。分别在空闲和传输文件时运行；ICMP 被禁不代表 TCP 故障，TCP 建连耗时也不是应用完整响应时间。

检查内网 IP 是否实际走 WireGuard 接口。本地与远端内网地址段重叠时，更具体的本地路由可能抢走流量，需要按实际地址规划处理。对比内部域名解析与直接 IP；公共 DNS 通常不能解析企业私有域名。不要为测试随意改默认路由。

Linux 可使用 `ip route get 内网IP`、`ping -c 10 内网IP`、`getent ahostsv4 内网域名`。服务端也检查到内网主机的延迟和 WireGuard 握手状态；不要分享包含密钥的 `wg showconf`。

## nginx / Cloudflare

参考 `deploy/nginx-xxtab.conf.example`；安装版在 `examples` 目录。替换域名、证书路径和 path_prefix，先用 `nginx -t` 检查，再应用到目标站点；不要覆盖承载其他业务的整个配置。

示例将 wstunnel 绑定到 `127.0.0.1:8080`，TLS 在 nginx 终止，Cloudflare 使用 **Full (strict)**。同机 nginx 到 wstunnel 使用 loopback WS，客户端到边缘节点、边缘节点到 nginx 仍使用 TLS。保留 HTTP/1.1 和 Upgrade/Connection，启用 TCP_NODELAY，设置合适空闲超时并使用心跳。

HTTP 缓存和缓冲显式关闭。nginx 完成 101 升级后本来就按双向隧道转发，关闭普通 HTTP 缓冲不保证降低已建立 WebSocket 的延迟。检查该路径是否还经过 Worker、Access 或额外中转；调整时保留现有鉴权要求。示例关闭 access log，错误日志仍可能记录请求路径，分享前删去敏感路径。

最有价值的对比是准备**原站直连配置副本**，访问同一内网 IP。若原站允许直连且证书受客户端信任，可在副本中设置 `server_ip = "原站IPv4"`，保留 `server` 域名用于 TLS 验证。Cloudflare Origin CA 证书不在普通公共根集合中，需给该测试配置指定相应 `ca_file`；不要关闭证书验证。不要自动更改线上 CDN 开关或服务器防火墙。

若原站直连明显更快，主要改善方向是链路／节点选择。网络允许时，原生 WireGuard UDP 通常更适合延迟敏感的内网访问。普通 Cloudflare HTTP 代理不能直接透传 WireGuard UDP；本程序当前未实现 QUIC/WebTransport。

## 测试边界

`tests/lan_workload.py` 通过真实 WireGuard 隧道传输文件并验证 SHA256，同时发送 32 字节 TCP 小请求。`tests/slow_tcp_proxy.py` 用有限缓冲和慢速读取复现代理排队，**不是公网 RTT/丢包模型，也不是 Cloudflare 的实现**。结果不能直接换算成你的部署提升。

运行 `tests/e2e_linux.py` 时设置 `XXTAB_LAN_BENCH=1`，可选 `XXTAB_PROXY_RATE=1048576`、`XXTAB_LAN_BYTES=4194304`、`XXTAB_LATENCY_MODE=interactive`。这些环境变量只用于隔离的测试夹具。

2026-09-16 在 Debian/WSL 中，旧版与新版本交替进行三轮，每次传输 4 MiB，慢代理每个方向配置 1 MiB/s 的读取速率。每次小请求之间等待 10 ms，负载场景只统计文件传输仍在进行时的请求。实际吞吐受 Python 代理调度影响，约 6 Mbps。三轮指标中位数如下：

| 场景 | 旧版小请求 P99 | interactive 小请求 P99 | 文件吞吐：旧版 → interactive |
| --- | --- | --- | --- |
| 空闲 | 8.98 ms | 9.04 ms | — |
| 同时上传 | 214.94 ms | 113.99 ms | 5.85 → 5.92 Mbps |
| 同时下载 | 283.99 ms | 278.78 ms | 5.96 → 5.95 Mbps |

上传负载下 P99 降低约 47%；空闲和下载方向没有明显改善。样本有限，三个上传轮次各有 42–67 个小请求，不应把尾延迟数字当作精确的长期承诺。六轮测试的文件校验、真实 WireGuard 握手和退出后的路由清理均通过。逐轮数据见 [lan-results.json](lan-results.json)。这是 Linux 慢代理测试，不是 Windows 性能对比，更不是 Cloudflare 公网实测。
