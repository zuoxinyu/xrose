# 异地组网

Xrose 支持 **EasyTier** 与 **Tailscale** 两种异地组网后端，允许双后端同时在线，并与代理内核独立管理生命周期。

## 后端实现与组件边界

- **EasyTier（内置静态库）**：集成 EasyTier `2.6.4`（固定提交 `8428a89d2dabc94c97d370ec607c6ca142473626`），保留上游默认特性，无需外部可执行文件。普通 GUI 仅持有加密配置与脱敏快照；实际运行由提权助手托管当前可执行文件的 `--mesh-worker` 工作进程，在独立 Tokio 运行时中启动 `Instance`。
- **Tailscale（独立组件）**：使用独立的 sing-box `1.14.4` 实例运行，组件下载与启动前均校验固定 SHA-256。管理 API 仅监听 loopback，随机 Bearer 凭证通过受保护文件传递；启动后经 gRPC 进行版本协商、状态订阅、设备探测与注销。身份目录独立于代理内核，断开保留身份，注销执行 Tailscale Logout；删除配置不会撤销控制台上的设备记录。

## 平台要求与构建

### 运行要求

Windows、macOS 与 Linux 均支持组网连接。Tailscale 固定组件提供 x64 / ARM64 官方发行包，下载和解压前校验固定摘要：

- **Windows**：使用 Wintun 驱动（连接前自动下载签名分发包并校验官方 SHA-256）。
- **macOS**：使用系统 `utun` 接口。
- **Linux**：需要 `/dev/net/tun` 与 `iproute2`；系统 MagicDNS 依赖 `systemd-resolved`（`resolvectl`），不可用时提示解析配置失败，仍保留虚拟 IP 与应用代理访问。

### macOS / Linux 构建

需要 Rust `1.95+`、C/C++ 工具链及 `protoc`，直接执行 `cargo build --workspace`。macOS 调试构建使用完整 DWARF 展开信息，避免静态链接 GPUI 与 EasyTier 后超出 compact unwind 偏移上限。

### Windows 构建

需安装 Rust MSVC、Visual Studio C++ 构建工具与 7-Zip，执行构建脚本：

```powershell
powershell -NoProfile -File scripts/build-windows.ps1
powershell -NoProfile -File scripts/build-windows.ps1 -Action test
```

构建脚本将校验后的 `protoc 26.1` 置入 `target/tools` 且仅作用于当前进程环境。EasyTier 上游构建脚本在首次编译时会从 GitHub 下载 VC-LTL / YY-Thunks 原生兼容库，缓存完成后可离线复用。

- Xrose 构建层修正上游 `Packet.lib` 搜索路径并延迟加载可选 `Packet.dll`，不分发 Npcap DLL 或抓包驱动；常规接口枚举使用 Windows API，暂不提供依赖抓包驱动的入口协议。
- 静态库不包含内核驱动，运行时由提权助手将经过摘要和签名校验的 Wintun DLL 解压至私有工作目录并仅由工作进程加载。
- 若使用 Cargo vendor，可通过 `XROSE_EASYTIER_SOURCE` 指定同一固定提交的源码根目录。

## 使用说明

### 提权助手与初始化

组网操作由提权助手统一执行（当前组网助手协议版本为 **v11**）。首次使用或升级 Xrose 后，请先在「设置 → 提权助手」或「测试」页安装或更新助手，再发起组网连接。

- **Windows 权限隔离**：GUI 保持普通权限，计划任务绑定当前安装用户 SID（`InteractiveToken` + `HighestAvailable`），后台静默运行且不弹出命令行窗口；安装器仅对数据目录中的 `helper/xrose-helper.exe` 副本设置 High 完整性标签，权限检测同时校验管理员身份与 High 完整性。
- **设备名称**：新建 EasyTier 或 Tailscale 网络时，设备名默认使用 `xrose-{hostname}`（`hostname` 为本机短主机名），保存前可修改。

### EasyTier 连接与高级设置

EasyTier 需配置网络名称、网络密钥及入口节点地址；虚拟 IPv4 留空时由入口连接与对端发现自动分配。若入口连通但未取得地址，请检查同网络设备是否在线，或填写网络内未占用的静态 IPv4，并核对入口协议、端口及网络名称（TCP 连通不代表握手完成）。默认配置不主动使用/发布出口，也不发布本机网段；虚拟地址、子网与出口转发支持 IPv4。

在「组网 → 添加网络」或已断开网络的「⋯ → 编辑」中点击右上角「高级设置」，可调整按网络生效的高级参数（对照 [EasyTier 官方配置表单](https://github.com/EasyTier/EasyTier/blob/8428a89d2dabc94c97d370ec607c6ca142473626/easytier-web/frontend-lib/src/components/Config.vue) 与[参数说明](https://easytier.cn/en/guide/network/configurations.html)）：

| 类别 | 可选项 |
| --- | --- |
| 连接与路由 | P2P 策略（自动 / 按需 / 关闭）；仅直连；请求对端主动连接；延迟优先；使用 IPv6 连接对端；绑定物理网卡；私有模式 |
| 传输与代理 | TCP 加速（关闭 / KCP / QUIC）；允许接收 KCP / QUIC 代理；用户态 TCP/IP 栈；多线程；传输加密；MTU（400–1380） |
| NAT 穿透与转发 | TCP / UDP / 对称 NAT 打洞；UPnP / NAT-PMP 端口映射；中继对端 RPC；系统转发；Windows 局域网 UDP 广播中继 |

> 默认保持传输加密、绑定物理网卡与多线程；关闭传输加密需与对端一致。选择「系统转发」需自行配置宿主机转发和回程路由；为保留 Xrose 的网卡生命周期、路由白名单与 DNS 清理机制，未开放无 TUN、公网 IPv6 及由 EasyTier 接管系统 DNS 等选项。

### Tailscale 连接与授权

Tailscale 连接时自动校验并补齐运行组件（也可点击「安装 Tailscale 组件」预先下载）。支持留空控制服务器以接入官方服务，或填写自建 HTTPS 控制服务器（如 Headscale）。

- 保存配置并点击「连接」后，按提示点击「在浏览器登录」完成设备授权；若停留在「等待管理员批准」，需由官方控制台或自建控制端管理员在设备列表中批准（批准后自动完成连接，重连保留登录状态）。
- Windows 上的控制面登录请求走系统路由，网络受限时可先建立可访问控制服务器的代理连接。Windows 首次创建虚拟网卡的 API 就绪等待上限为 30 秒。
- **独立客户端冲突**：独立 Tailscale 客户端与 Xrose 使用相同地址空间，不可同时接管系统路由；使用 Xrose 前请先断开独立客户端（如执行 `tailscale down`）。macOS 冲突检查覆盖普通网卡与 `utun` 点对点网卡的 IPv4/IPv6 地址。

### 启动连接与自动重连

- **随应用启动**：创建或编辑网络时可启用「随应用启动连接」（与「设置 → 启动与窗口」同步）。每种组网后端各保留一个启动配置，独立于「登录时启动应用」开关。
- **自动重连**：每个网络默认启用「意外断开后自动重连」。更新或重启提权助手后，Xrose 会核实会话状态并在退避间隔（最长 1 分钟）内恢复连接；手动断开、注销、删除配置或退出应用时停止重连。卡片处于「等待重连」时可随时取消。

## 设备发现与配置同步

组网卡片可展开「设备列表」，标出出口节点、子网路由、客户端类型与已知版本（Tailscale 公布主机密钥时显示 `Tailscale SSH`，实际访问仍受访问策略约束）。对已知地址的设备（含离线设备）可执行「探测设备」，结果在标签区显示 6 秒。

两台运行支持发现的 Xrose 设备接入同一组网（且网络策略放行成员间 `TCP 46973`）后，即可点对点共享配置：

1. **发送端**：在网络卡片「⋯ → 共享配置」中勾选要分享的节点、节点订阅、组网配置或自定义规则。默认生成一次性「共享码」，也可选择「无认证」开启公开共享；两种模式均在 10 分钟后过期，期间可随时停止（提示：无认证模式下同组网成员均可接收其中包含的节点凭据、订阅地址与网络密钥）。
2. **接收端**：在发送设备行右侧选择「⋯ → 接收配置」，输入共享码（或直接接收无认证共享），在弹窗中逐项预览差异并选择性合并；新增项目默认勾选，冲突项可选择是否覆盖；确认后以事务写入本机。

**同步边界**：
- 组网配置同步网络名称、入口、控制端、密钥及远端子网（EasyTier 同步 P2P、路由偏好、IPv6 传输、TCP 加速、加密与 MTU）；本机身份、静态地址、发布子网、出口、端口转发、自动重连与开机启动项由各端独立保留，正在连接的组网不会被覆盖。
- 自定义规则可连带规则分组与规则集同步；引用其他出站组时要求接收端存在同标识或唯一同名出站组。

## 出口、子网与端口转发

### 出口节点与代理联动

- **远端子网与本机子网**：远端子网支持逗号分隔 CIDR（不接受默认路由）；本机发布子网中 EasyTier 使用内置转发，Tailscale 使用用户态 endpoint，均无需改动宿主机 IP forwarding 开关。Tailscale 发布子网或出口仍需控制端批准。
- **使用远端出口**：连接组网后，在「节点」页的手动出站组中可直接点选已发现的在线出口节点（首次选择自动分配本地端口），Xray 与 sing-box 均可用；切换设备会短暂重连对应组网。仅填写远端出口节点不会安装宿主机默认路由。
- **出口出站端口**：配置未占用的 `1024–65535` 本地端口后，会自动生成「网络名 · 出口」节点供出站组和分流规则引用（配合代理 TUN 可覆盖全局流量）。该出站仅监听 loopback 并启用随机凭据，组网退出后连接直接失败而不回退直连。EasyTier 出口网关支持 IPv4 TCP/UDP。

### 双向端口转发

在组网页「端口转发」卡片可配置两个方向的 TCP / UDP 规则（IPv6 地址需加方括号）：

- **本机 → 组网**：将本地地址/端口（`127.0.0.1` 仅限本机，`0.0.0.0` 向局域网开放）转发至目标组网设备的 IP 与端口。
- **组网 → 本机**：将本机虚拟地址上的指定服务端口转发至本机可达的服务（如 `127.0.0.1:8080` 或局域网服务），无需发布整个子网。EasyTier 绑定当前网络虚拟 IPv4，Tailscale 经虚拟 IPv4/IPv6 接收；访问需符合组网访问策略与本机防火墙规则。

> 两种方向均可在连接期间热更新（增删、修改或切换方向），不中断组网或已有其他规则连接，断开组网时释放监听并在下次连接时自动恢复。端口转发规则仅保存于本机，不参与配置同步。

## CLI 使用示例

所有调试与脚本操作建议配合隔离目录 `XROSE_HOME`（`save -` 从标准输入读取 JSON，避免密钥进入命令行历史）：

```powershell
$env:XROSE_HOME = "$env:TEMP\xrose-mesh-test"
xrose-cli mesh install
xrose-cli mesh check
xrose-cli mesh api-check
xrose-cli mesh save profile.json
xrose-cli mesh list
xrose-cli mesh connect <EasyTier-ID> <Tailscale-ID> --seconds 120
xrose-cli mesh status
xrose-cli mesh probe <ID> <设备IP>
xrose-cli mesh disconnect <ID>
xrose-cli mesh logout <ID>
xrose-cli mesh doctor
```

示例 JSON（仅使用虚构保留地址与占位密钥）：

```json
{
  "name": "测试网络",
  "provider": "easy_tier",
  "hostname": "xrose-test",
  "network_name": "example-network",
  "network_secret": "replace-in-private-file",
  "peers": ["tcp://192.0.2.1:11010"],
  "ipv4": "10.126.126.2/24",
  "remote_subnets": []
}
```

> `connect` 在前台保持会话；隔离测试提权助手时须使用同一 `XROSE_HOME`，避免身份目录混入正式环境。

## 路由、DNS 与安全边界

- **路由白名单与共存**：EasyTier 显式配置远端网段白名单；Tailscale 使用用户态 endpoint 与专用 TUN（地址 `10.255.253.1/30` 及 `fdfe:dcba:9877::1/126`，启动前校验冲突），其 `route_address` 仅包含 tailnet 范围与用户指定的远端网段，防止控制端下发路由直接改写系统路由表。
- **代理联动**：连接前与运行中持续校验接口网段、默认路由及双后端冲突。组网信息通过独立的共存计划注入代理编译上下文（增加 TUN 排除、直连优先、PAC 绕过与 MagicDNS 规则），不污染 `AppModel`。代理 TUN 重启引发的 Windows Teredo 等虚拟接口空 DNS 瞬态变化会被基线比较容忍，而已有解析器修改与默认路由变动仍会严格告警。设置中的「TUN 严格路由」由用户控制，组网连接不会覆盖该选择。
- **用户态探测提示**：Tailscale 设备探测经自带 gRPC ping 完成；用户态转发业务建议以 TCP/UDP 连通性为准，不以系统 `ping` 的 ICMP 超时直接判定不可用。
- **MagicDNS 边界**：Tailscale DNS 在 Windows / Linux 监听 `127.0.0.153:53`，macOS 监听 `127.0.0.1:53`（端口占用则报错）。仅针对当前 tailnet 完整后缀设置带归属记录的域名路由（不改动全局 DNS 或覆盖其他程序的 `/etc/resolver`），代理侧对相同域名优先交由该解析器处理而非 Fake-IP。正常退出及下次助手启动只清理 Xrose 自身归属的 DNS 规则。
- **进程与 IPC 隔离**：Windows 命名管道显式限制同用户 ACL 与 Medium 完整性标签，每次请求校验协议版本、调用者 PID、进程 SID 与随机会话凭证，并通过 Job Object 确保工作进程随助手退出；Unix 校验内核对端 UID/PID，工作进程置于独立进程组以便整体回收。
- **凭据脱敏**：SQLite 中的 `mesh_profiles` 全量加密保存；普通快照与日志不含网络密钥或临时登录链接，标准输出错误仅在内存中归类为脱敏诊断信息。

## 校验与冒烟测试

- **离线与配置校验**：单元测试覆盖配置转换、迁移、密钥脱敏、网段冲突与 DNS 归属约束，不依赖管理员权限、虚拟网卡或外网；`mesh check` 使用真实 sing-box 执行离线校验；`mesh api-check` 启动无网卡临时端点验证 gRPC 协议、无效凭证拒绝与状态订阅。
- **Windows 独立冒烟脚本**：
  - `scripts/mesh-smoke-windows.ps1`：以随机 `XROSE_HELPER_SOCKET` 与 TEMP 隔离目录运行独立组网验证（支持 `-ProbeAddresses`、`-ProbeTcpPort`、`-DnsName`、`-CrashOwner`），并在结束或异常退出时核对虚拟网卡、NRPT 规则、原有默认路由与 DNS 基线完好。
  - `scripts/tun-smoke-windows.ps1`：配合 `cargo run -p xrose-cli --example tun_smoke` 验证受限 TUN（`auto_route=false`、`dns_mode=disabled`）的助手代启、日志与异常退出回收。

> 注：以上受限测试不替代全流量代理 TUN、跨网段真实流量及跨平台桌面环境的完整实机验收。

上游参考：[EasyTier](https://github.com/EasyTier/EasyTier/tree/v2.6.4/easytier) · [sing-box Tailscale](https://sing-box.sagernet.org/configuration/endpoint/tailscale/) · [Wintun](https://www.wintun.net/)
