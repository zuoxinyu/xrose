# 异地组网

EasyTier 2.6.4 作为 Rust 库静态链接，源码提交固定为
`8428a89d2dabc94c97d370ec607c6ca142473626`，保留上游默认 feature。
不安装、不调用 EasyTier 官方可执行文件。版本随 Xross 发布，Cargo.lock
同时锁定其传递依赖。普通 GUI 只持有加密配置及脱敏快照；特权助手启动当前
Xross 可执行文件的私有 `--mesh-worker` 模式，工作进程独立运行 Tokio 和
`NetworkInstanceManager`。

Tailscale 使用单独的 sing-box 1.14.4，组件下载与启动前均验证固定 SHA-256。
管理 API 仅监听 loopback，随机 Bearer 凭证通过受保护文件传递；启动后使用
gRPC 版本协商、状态订阅、设备探测及注销接口。身份目录独立于代理 core，
断开保留身份，注销执行 Tailscale Logout。删除配置不会替用户撤销控制台设备。

## Windows 构建

安装 Rust MSVC、Visual Studio C++ 构建工具和 7-Zip，然后执行：

```powershell
powershell -NoProfile -File scripts/build-windows.ps1
powershell -NoProfile -File scripts/build-windows.ps1 -Action test
```

脚本将经过 SHA-256 校验的 protoc 26.1 放入 `target/tools`，只修改当前进程环境。
EasyTier 的上游构建脚本还会下载 VC-LTL/YY-Thunks 原生兼容库，因此首次构建
需要访问其 GitHub 发布页；构建缓存完成后可离线复用。发布构建应保存这些
上游原生工件并审计其来源。静态库并不包含 Windows 内核驱动：连接前下载
Wintun 0.14.1 官方签名分发包并校验官方 SHA-256，助手解压 DLL 和许可证到
私有工作目录，仅由工作进程加载。EasyTier 的 TUN 依赖也会验证 DLL 签名。
Xross 构建层修正上游 `Packet.lib` 的相对搜索路径，并延迟加载可选的 Packet.dll；
不分发 Npcap DLL，不安装抓包驱动。常规接口枚举使用 Windows API；本轮不提供
依赖抓包驱动的入口协议。若采用 Cargo vendor，可通过 `XROSS_EASYTIER_SOURCE`
指定同一固定提交的源码根目录。

## 使用

侧栏“组网”页面支持两种后端各运行一个网络。先准备特权助手，再新增配置。
Windows GUI 及由它安装的计划任务助手不创建命令行窗口，提权操作后台运行；
需要管理员授权时仍显示系统 UAC 提示。
计划任务绑定安装用户 SID，使用 `InteractiveToken` 和 `HighestAvailable`，
不以通用 Users 组作为运行身份。旧安装可通过重新安装助手更新启动项。
安装器将助手复制到数据目录的 `helper/xross-helper.exe`，仅为该副本设置 High
完整性标签，避免开发目录继承的 Medium 标签限制计划任务及工作进程；GUI 程序
保持普通权限。权限检测同时检查管理员身份和 High 完整性，不再仅凭管理员组判断。
更新 Xross 后重新安装助手即可替换副本并更新启动项。
EasyTier 必须填写网络名称、密钥及显式入口节点；虚拟 IPv4 留空时使用自动分配。
Tailscale 首次使用先点击“安装 Tailscale 组件”（已安装时不显示），然后新增网络、
选择 Tailscale，填写配置名称与设备名称；控制服务器留空使用官方服务，也可填写
自己的 HTTPS 控制服务器。点击“加密保存”后，在网络卡片点击“连接”，等待出现
“在浏览器登录”按钮并点击，在浏览器选择所属 tailnet 的账号完成设备授权。
如需管理员批准，批准后才会连接；后续断开、重连会保留登录状态。
连接按钮会等待后端启动结果；进入待登录状态时提示浏览器授权，启动失败时直接
显示错误。Windows 首次创建虚拟网卡的 API 就绪等待上限为 30 秒。组件提前退出与
API 等待超时分别报告，标准输出和错误输出仅在内存中分类为固定的脱敏诊断，
不写入原始日志或暴露登录链接、身份密钥。
不自动加入公共 EasyTier 中继，不自动连接、不启用出口节点、不发布本机网段。
EasyTier 首期支持 IPv4 虚拟地址及远端网段。macOS/Linux 保持源代码兼容，
目前不启用组网连接，也不宣称网络验证完成。

CLI 示例（所有测试都使用隔离目录）：

```powershell
$env:XROSS_HOME = "$env:TEMP\xross-mesh-test"
xross-cli mesh install
xross-cli mesh check
xross-cli mesh api-check
xross-cli mesh save profile.json
xross-cli mesh list
xross-cli mesh connect <EasyTier-ID> <Tailscale-ID> --seconds 120
xross-cli mesh status
xross-cli mesh probe <ID> <设备IP>
xross-cli mesh disconnect <ID>
xross-cli mesh logout <ID>
xross-cli mesh doctor
```

`save -` 从标准输入读取 JSON，密钥不得放进命令行。示例配置只使用假数据：

```json
{
  "name": "测试网络",
  "provider": "easy_tier",
  "hostname": "xross-test",
  "network_name": "example-network",
  "network_secret": "replace-in-private-file",
  "peers": ["tcp://192.0.2.1:11010"],
  "ipv4": "10.126.126.2/24",
  "remote_subnets": []
}
```

Tailscale 配置仅需 `provider: "tailscale"`、名称及 hostname。`connect` 在前台保持
会话；Ctrl+C、超时、GUI 退出或所有者进程崩溃会停止工作进程。关闭到托盘保持
连接。跨进程的 status/disconnect/logout 仍需同一 Windows 用户通过助手鉴权。
隔离测试助手也必须以同一个 `XROSS_HOME` 启动，避免身份目录落入正式助手目录。

## 路由、DNS 与权限边界

EasyTier 显式设置远端网段白名单，不自动安装所有节点发布的代理网段。
Tailscale 上游 `accept_routes` 没有逐网段 OS 白名单，所以使用用户态 endpoint，
配合独立 sing-box TUN 入站；该入站的 `route_address` 仅包含 Tailscale 地址范围
及用户明确填写的远端网段。控制服务器新增路由不会直接写入系统路由表。
TUN 使用专用地址 `10.255.253.1/30` 和 `fdfe:dcba:9877::1/126`（连接前检查冲突），
与代理 TUN 默认地址分离；实际 tailnet 地址从状态 API 显示。
该连接用于访问远端设备和网段，不自动开放本机服务给 tailnet。
设备探测通过 Tailscale 自带 gRPC ping 完成；用户态转发的业务验收使用 TCP/UDP，
不能以系统 `ping` 的 ICMP 超时直接判定该连接不可用。

连接前检查本地接口网段及其他组网网段。运行中检查实际接口路由、默认路由
和双后端冲突，发现冲突则停止引入冲突的连接。代理编译上下文接收独立的
共存计划，增加 TUN 排除、优先直连、PAC 绕过及 MagicDNS 规则；不写入 AppModel。
自动联动基于最后已应用的代理配置，先校验再重启，启动失败回滚，保留待应用
编辑。Windows 组网运行时暂时关闭代理 TUN `strict_route`，严格 DNS 防泄漏
过滤相应放宽；全部断开后恢复原始配置值。

Tailscale DNS 只监听 `127.0.0.153:53`。NRPT 只设置当前 tailnet 完整后缀，
不改全局 DNS、不设置搜索后缀；代理内相同域名优先交给该解析器而非 fake-IP。
NRPT 操作先写归属记录，正常退出及助手下次启动只清理 Xross 对应规则。
助手校验协议版本、命名管道对端 PID、进程 SID 和随机会话凭证；进程句柄避免
PID 重用，Job Object 确保工作进程及其子进程随助手退出。
Windows 管道显式设置同用户 ACL 和 Medium 完整性标签，允许普通权限 GUI
连接高权限助手；每次请求仍校验调用者 SID，组网命令另校验会话所属 PID。

SQLite v3 的组网配置独立加密保存。普通快照不含网络密钥，临时登录链接仅供
所属 GUI/前台 CLI 打开浏览器，不进入普通 CLI 状态、日志或数据库。

## 验收边界

自动测试不创建虚拟网卡、不需要管理员或网络；覆盖配置转换、v2→v3 迁移、
密钥脱敏、网段冲突和 DNS 归属约束。`mesh check` 使用真实 sing-box 做纯配置
校验。实机连接必须另行记录测试结果，不能用配置校验代替连通性验证。
`mesh api-check` 启动无网卡、无 DNS 监听的临时用户态端点，验证真实 gRPC
版本、无效凭证拒绝和状态订阅；它需要网络，但不会登录或接管系统网络。
全流量代理 TUN、断网/崩溃恢复应在独立 Windows 环境验收；不得在任务依赖的
网络上改默认网关、全局 DNS、OpenWrt 服务或防火墙。没有隔离环境时该项待验证。

`scripts/mesh-smoke-windows.ps1` 是独立于助手 Job Object 的有时限回收脚本。
它只接受 TEMP 下的测试目录，通过随机 `XROSS_HELPER_SOCKET` 与正式助手隔离，
监测原有默认路由、DNS、管理端口和 HTTPS 连通性。`-ProbeAddresses` 限定探测
目标，`-ProbeTcpPort` 允许对明确指定的业务端口验证 TCP，`-DnsName` 验证完整域名；
`-CrashOwner` 在连通后结束测试所有者，并核对网卡和所属 NRPT 规则已清理。
脚本需要 UAC 提权，不安装永久服务，也不修改路由器。

`scripts/tun-smoke-windows.ps1` 为 `cargo run -p xross-cli --example tun_smoke`
提供独立助手与回收监控；两者须使用同一 TEMP 测试目录和随机 `XROSS_HELPER_SOCKET`。
测试目录须预先登记 sing-box 核心，示例只创建 `auto_route=false`、`dns_mode=disabled`
的受限网卡，验证助手代启、日志、两次启停；`--crash-owner` 验证客户端异常退出回收。
结束后写入测试目录的 `finish.flag`，监控程序清理助手并输出 `guardian-result.json`。
该测试不代替全流量 TUN 验收。

### 2026-11-03 Windows 验收记录

- 工作区构建通过；仅保留仓库原有的两条警告。修改文件的 rustfmt 检查通过。
- 核心离线测试 157 项通过，跳过已知 Windows `second_acquire_fails` 锁测试。
  新增 TUN 核心鉴权边界、所有者退出回收、下载包选择及新增网卡 DNS 基线回归测试。
- Windows 受限 TUN 的普通权限客户端经助手启动、日志、两次启停、客户端异常退出及
  助手失联回收通过；同时运行的 Tailscale 在新版复测中保持连接。
  默认路由、原有 DNS、OpenWrt 管理通道保持不变，测试网卡已删除。
- EasyTier 静态库与 OpenWrt EasyTier 2.4.5 成功组网，设备发现与探测通过。
- Tailscale 独立身份登录、TCP 访问、完整设备域名解析和 gRPC 设备探测通过。
- 双后端同时在线通过；普通权限客户端访问高权限助手通过。
- 独立回收进程强制结束所有者后，工作进程、网卡及所属 NRPT 规则清理通过；
  默认路由、WLAN DNS 和 OpenWrt 管理通道保持不变。OpenWrt 未作配置变更。
- sing-box 独立组网及代理共存配置通过真实核心检查；隔离的演示配置通过 Xray 检查。
- 新增、切换后端、加密保存与配置卡片已在隔离 GUI 中检查。

尚未验收：独立 Windows 环境的全流量代理 TUN、自动代理重启下的真实流量、
整机断网恢复、远端网段端到端访问，以及 macOS/Linux 的交叉构建和网络运行。
本机未为这些项目切换系统代理或启用全流量 TUN。测试身份保留在隔离目录，
断开没有注销控制服务器上的设备。

上游参考：[EasyTier 库](https://github.com/EasyTier/EasyTier/tree/v2.6.4/easytier)、
[sing-box Tailscale](https://sing-box.sagernet.org/configuration/endpoint/tailscale/)、
[Wintun 签名分发](https://www.wintun.net/)。
