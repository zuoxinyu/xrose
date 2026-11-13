# Xrose 架构与编译约定

本文档说明 Xrose 的核心分层、`AppModel` 编译约定、双内核能力映射及平台边界。工作区分为三个 crate：`crates/xrose-core`（非 UI 核心与服务）、`crates/xrose-app`（GPUI 界面与托盘）和 `crates/xrose-cli`（无界面工具）。

## 1. 核心分层与 AppModel

直接以单一内核的 JSON 结构建模会导致适配第二个内核时状态与语义泄漏。Xrose 的 [`AppModel`](../crates/xrose-core/src/model/mod.rs) 仅保存与具体内核无关的用户意图：

- `work_mode`：`WorkMode::Direct`（本机直连）或 `WorkMode::Proxy`（代理），与分流模板、出站组选择正交。
- `nodes`：协议、凭据与 TLS / 传输参数，不含内核字段名。
- `groups`：出站组（`selector` / `urltest` / `fallback` / `loadbalance`）及各组切换策略 `switch_policy`。
- `routing`：自定义规则分组（`rule_groups`）、规则（`rules`，含 `geosite` / `geoip` / `rule_sets` / `process` 等语义匹配器）与规则集定义。
- `dns`：上游服务器（`tag` / `address` / `domains` / `detour`）、Fake-IP、Hosts 与 `final` 策略。
- `inbounds`：mixed / socks / http / TUN / tproxy / redirect。
- `default_group` 指定默认代理出站组。

协议字段名（如 `outbounds`、`protocol`、`route`）仅允许出现在 `adapters::xray` 与 `adapters::singbox` 中。UI 层只能通过 `AppService` 同步发起操作，并从 `UiSnapshot` 读取状态，不在 `render` / `tick` 路径中执行磁盘或网络访问。

## 2. 本机直连模式

新配置默认采用 `CoreKind::SingBox`、`WorkMode::Direct` 与系统 DNS，确保零订阅、零节点即可启动实时监控与诊断；默认广告拦截规则由用户按需启用，避免首次直连启动时因拉取外部规则集失败而阻塞。

- **虚拟身份隔离**：常量 `DIRECT_VIRTUAL_NODE_ID`（`__xrose_direct__`）仅用于界面与菜单展示，不写入 `model.nodes`，从而隔离节点计数、分享导出、订阅替换与组网同步。
- **直连编译策略**：
  - 省略远端代理出站，避免空节点编译报错；所有 `Proxy` 路由动作映射为直连。
  - 启用规则中的 `Block` 动作原样保留（sing-box `action: reject` / Xray `blackhole`），组网共建路由与 MagicDNS 照常注入。
  - 若配置的 DNS 上游依赖代理 `detour`，直连编译期跳过该上游及其分域规则并生成中文警告；无剩余可用上游时自动回退系统解析器，防止断网。
  - `required_rule_sets` 仅收集 `Block` 规则及有效直连 DNS 引用的规则集。
  - 未启用 TUN 且开启系统代理时，直连模式经本地混合 / HTTP 入站接管系统代理流量，保证统计与拦截规则可用。
- **无损切换**：点选「本机直连」仅更新 `work_mode`；从直连模式点选真实节点或组网出口时，在同一事务中切回 `WorkMode::Proxy` 并保留原分流模板与出站组配置。

## 3. Xray 与 sing-box 能力映射

不支持的能力优先在 `CompiledConfig::warnings` 中生成中文降级提示，仅在输入无法编译时返回错误。

| 能力 | Xray | sing-box | Xrose 处理 |
| --- | --- | --- | --- |
| mixed 入站 | 无 | 有 | Xray 拆成 SOCKS + HTTP（`port + 1`），界面提示 |
| 局域网认证与独立端口 | SOCKS 与 HTTP 分别配置账号 | mixed 入站配置账号 | 共用端口时本机也需认证；独立端口模式保留本机回环入口 |
| TUN | 原生（自动路由需 26.6.27+） | 原生 | Xray 生成 `gateway`、`autoSystemRoutingTable` 与 `autoOutboundsInterface`；共享提权启动与进程回收 |
| AnyTLS | 不支持 | 1.12+ | 密码认证、必选 TLS 与空闲会话参数；Xray 跳过并警告 |
| OpenVPN | 不支持 | 1.14+ endpoint | TLS 客户端、UDP/TCP、账号密码与 PEM 证书、控制通道密钥、分域 DNS；旧内核跳过并修复组引用 |
| OpenConnect | 不支持 | 1.14+ endpoint | 六种协议类型、HTTPS 路径、认证组、账号密码/证书/会话、分域 DNS；交互认证暂未接入 |
| WireGuard | 用户态出站 | endpoint | 单对端、密钥、地址、允许网段、MTU、reserved；不创建系统 WireGuard 网卡 |
| 自定义规则分组 | 按组展开 | 按组展开 | 启用组按顺序先于未分组/预设规则；本地预览共用 `effective_rules()`；PAC 从启用规则收集直连条件 |
| selector 热切换 | 无 | Clash API | Xray 选择节点需改写配置并重启 |
| urltest | `balancer` + `observatory` | 原生 `urltest` | 分别生成 |
| 故障转移 / 自定义切换策略 | 路由固定到选中节点 | 单成员选择组，避免缓存覆盖 | 客户端按策略探测、校验并重启；故障组不再降级为最低延迟 |
| loadbalance（默认策略） | `random` | 无 | sing-box 映射为 `selector` + 警告 |
| block | `blackhole` 出站 | `action: reject` | 不生成旧版 `block` 出站 |
| geoip / geosite | 原生字段 | 1.8 起移除 | 自动生成 `.srs` rule-set 引用 |
| GFWList 来源 | `geosite:gfw`（需对应资产） | `geosite-gfw` rule-set | 使用 MetaCubeX `gfw.srs`；编译、预下载与手动更新共用来源解析 |
| fake-ip | 不支持 | 原生（1.12+ typed DNS） | Xray 跳过 DNS `fakeip` server |
| DNS schema | `servers[].address` | 1.12+ typed server + `domain_resolver` | 只生成 1.12+ 新式 DNS 格式 |
| 连接列表与断开 | 无 | Clash API | 支持断开单条或确认断开全部连接 |
| 实时进程视图 | 无 | 活跃连接按进程聚合 | 支持按进程查看连接与创建阻止规则 |
| 连接进程归因 | 无连接列表 | `route.find_process` | Linux/Windows/macOS 开启进程查找；权限或远端请求缺失时明确提示 |
| 实时统计 | `StatsService`（gRPC） | Clash API | Xray 通过 `xray api statsquery` 查询 |
| 历史流量统计 | 业务入站总量、节点与直连计数 | 总量及节点 / 进程 / 主域名分类 | 默认关闭，按会话与本地自然日保存，周一为每周起点；柱状图与表格共用查询 |
| 组网访问 | 优先 freedom 规则 + 专用 DNS | 绑定组网接口直连 + TUN 排除 + 专用 DNS | `CompileContext.mesh` 注入，不改变 `AppModel`；MagicDNS 优先于 Fake-IP |
| 组网出口出站 | 本机认证 SOCKS 出站 | 本机认证 SOCKS 出站 | 「节点」页列出已连接组网出口候选；每个组网配置维护稳定代理节点 ID |
| Windows 组网与 TUN 共存 | 自动路由排除组网网段 | 保留用户 `strict_route` 设置 | 严格路由可能影响组网 DNS，界面提示可手动关闭 |

### 协议与 TUN 适配说明

- **AnyTLS / WireGuard**：AnyTLS 字段依据 [sing-box 文档](https://sing-box.sagernet.org/configuration/outbound/anytls/)；WireGuard 依据 [sing-box endpoint](https://sing-box.sagernet.org/configuration/endpoint/wireguard/) 与 [Xray 出站文档](https://xtls.github.io/en/config/outbounds/wireguard.html)，`Credentials.wireguard` 随配置加密保存，`NodeDraft` 统一校验密钥长度与地址格式。
- **OpenVPN / OpenConnect**：依据 sing-box 1.14.4 类型定义（[OpenVPN](https://sing-box.sagernet.org/configuration/endpoint/openvpn-client/)、[OpenConnect](https://sing-box.sagernet.org/configuration/endpoint/openconnect/)）生成用户态 endpoint，通用 TLS / v2ray 传输层不叠加到 VPN。导入器不执行 `.ovpn` 指令或读取本地外部文件。启动时传入实际内核版本，低于 1.14 时跳过 VPN 节点并在纯 VPN 无法编译时提示升级。
- **Xray TUN**：依据 [Xray TUN 文档](https://xtls.github.io/en/config/inbounds/tun.html) 与 [v26.6.27 定义](https://github.com/XTLS/Xray-core/blob/v26.6.27/infra/conf/tun.go)，通过 CIDR 差集排除用户与组网网段，DNS 劫持规则先于 LAN 绕过并将系统解析器替换为显式上游以防回环；`strict_route` 与网络栈不映射并提示警告。Linux/macOS 不自动接管系统 DNS，Windows 配置 TUN 接口 DNS。

## 4. 出站组、节点切换与测速

- **出站组事务**（`service/outbound_groups.rs`）：成员与切换策略以单次配置事务提交，检查基线版本避免覆盖并发编辑。未显式改动首选节点时保留最新选择；移除当前选中节点后优先选择当前内核可用的成员。默认组及被路由（含禁用规则）或 DNS 引用的组禁止删除。
- **自动切换策略**（`service/node_switch.rs`）：`switch_policy` 控制关闭、仅故障切换、延迟优先或吞吐优先。策略任务仅在内核运行且无待应用编辑时按组独立探测已应用配置，不读取列表历史测速；容差或吞吐提升达标（>=10%，单轮每节点最多下载 1 MiB）或连续失败达阈值时触发切换，在内核操作锁内校验配置并重启，失败自动回滚。
- **HTTP 延迟与下载探测**（`runtime::probe`）：启动短期隔离测试内核，仅包含目标节点与独立本地 HTTP 入站，关闭系统代理继承、重定向与自动解压；sing-box 测试内核开启 `auto_detect_interface`，Xray TUN 下检测上联网卡绑定 `sockopt.interface`。测速完成后立即停止进程并清理临时配置。

## 5. 路由、规则集与文件交换

- **规则编译**：`RoutingConfig.rule_groups` 保存自定义分组；启用分组按顺序优先于未分组与预设规则展开，内核编译、本地路由预览（`effective_rules()`）与 PAC 直连收集共用同一规则顺序。
- **规则集更新**：sing-box 启动只准备当前路由和 DNS 实际引用的文件。路由页的批量更新还包括其他内置路由模板、已保存但未启用的规则和 DNS 引用，按标签去重并优先采用配置的镜像。默认上游、MetaCubeX、jsDelivr 及自定义地址模板按配置档保存，供列表、编译与下载共用；显式指定的独立地址保留。列表从本地缓存恢复最后更新时间；条件请求按下载 URL 区分，切源后首次更新重新获取内容，失败与 HTTP 304 保留上一次写入时间。
- **路由交换**（`route/transfer.rs`）：定义独立于数据库版本的 `xrose-routing` v1 JSON（包含 `rules`、`rule_groups`、`rule_sets` 与用于跨配置解析出站组的 `outbound_names`），不含节点、订阅或运行状态。同标签且同定义的规则集直接复用，定义冲突则拒绝整批导入；规则集来源限制为无凭据与无查询参数的 HTTP(S) URL。
- **SwitchyOmega / AutoProxy 转换**（`route/transfer/omega.rs`）：按 [SwitchyOmega 解析顺序](https://github.com/FelisCatus/SwitchyOmega/blob/master/omega-pac/src/rule_list.coffee) 转换为统一意图模型并生成预览摘要，不支持的浏览器专有条件记入跳过提示。后台解析结果仅存内存，用户确认后验证预览 ID 与引用完整性再事务提交。

## 6. 异地组网架构

组网层独立于 `CoreKind`，由 `mesh::MeshProfile` 表达身份、入口、出口、子网与端口转发意图，`MeshController` 经带协议版本与调用者鉴权的助手 IPC 管理会话。

- **双后端运行**：EasyTier 2.6.4 静态库在 `--mesh-worker` 中初始化；Tailscale 使用固定 sing-box 1.14.4 子进程与鉴权 gRPC，通过用户态 endpoint + 专用 TUN 实现，仅将批准路由写入 `route_address` 并为当前 tailnet 后缀设置带归属记录的 DNS（Windows NRPT / macOS `/etc/resolver` / Linux `systemd-resolved`）。
- **双向端口转发**（组网协议 v9+）：服务将「本机 → 组网」与「组网 → 本机」统一为 `ForwardRules` 在线更新事务，先由工作进程校验并热更新监听，再落库持久化，失败自动回滚。Tailscale 通过本地鉴权 SOCKS 服务桥与路由约束保障目标隔离，不开放任意主机端口。
- **自动重连与出口选择**：重连监控（`service/mesh_reconnect.rs`）在提权助手恢复后按网络类型串行恢复会话并重新校验代理共存路由。选择组网出口时重连对应组网并等待就绪，随后在同一事务中更新组网出口与手动出站组选择。
- **设备发现与配置共享**：使用版本化 TCP 协议仅向已连接网络的成员发送握手；共享码模式使用挑战应答，无认证模式通过 Noise NN 临时密钥协商传输本次共享数据密钥。预览在内存中完成，确认时以单次 SQLite 事务写入加密配置，协议本身、共享码与预览不落库。

完整权限、路由隔离与构建细节见 [异地组网文档](MESH.md)。

## 7. 订阅导入与链接识别

### 订阅抓取与合并

1. `fetch_subscription`：发送条件请求（`ETag` / `If-Modified-Since`）并解析 `subscription-userinfo`。
2. `parse_subscription`：自动识别 Base64、分享链接、Clash YAML、SIP008 与 sing-box JSON。
3. **合并与去重**：按订阅 ID 替换旧节点（保留其他订阅），按指纹去重；Clash `proxy-groups` 按名称合并，新节点加入默认出站组。更换订阅 URL 会清理旧缓存验证信息，跨来源更换清除自定义请求头。
4. **更新代理策略**：订阅更新与资源下载各有独立开关。启用时优先使用设置中的 SOCKS5 前置代理，由代理解析目标域名。前置代理留空时使用运行中内核的 HTTP 或混合入站。关闭时请求直接连接。两类请求均支持自定义 User-Agent。

### 链接与二维码导入、分享

- **分享**：节点与订阅共用复制链接操作和二维码弹窗。执行时按 ID 读取当前快照；节点导出协议链接，订阅仅分享完整订阅 URL，不附带自定义请求头或节点列表。二维码在内存生成。

- **多通道导入**：节点页与订阅页共用 `ui/qr` 管线，支持剪贴板粘贴、文件/图片拖放及屏幕二维码扫描，最终统一交由 `AppService::import_shared_content` 与订阅解析器处理，识别 v2rayN VMess JSON、标准 URI 与 Shadowrocket 编码链接（参考 [v2rayN 导出](https://github.com/2dust/v2rayN/tree/master/v2rayN/ServiceLib/Handler/Fmt) 与 [Shadowrocket 生成格式](https://github.com/cedar2025/Xboard/blob/master/app/Protocols/Shadowrocket.php)）。
- **屏幕取景扫描**：添加节点、添加订阅及主窗口快捷键共用取景会话。每个显示器有一个透明窗口，仅在指针所在屏幕显示准星和预览。按住 ⌘（macOS）或 Ctrl 滚动可缩放取景框；点击目标或预览、按回车或空格导入，右键或 Esc 取消。一次确认只导入一个二维码。点击二维码或预览后，根据导入结果播放成功或失败提示音；键盘确认保持静音。预览沿用节点与订阅的隐私遮罩，不写入扫码历史、剪贴板，也不保留截图。
- **采集与快捷键**：截屏和 QR 解码在后台执行，识别图像最长边为 1024px；滚动底层页面或切屏会丢弃旧结果。macOS 使用 ScreenCaptureKit；Windows 使用 XCap 的 Windows Graphics Capture；X11 按 RandR 显示器定位，KDE Wayland 经截图门户按逻辑坐标裁切。Windows 与 Wayland 在内存中短暂复用当前屏幕的截图。扫码快捷键为 macOS 的 `⇧⌘S` 和 Windows / Linux 的 `Ctrl+Shift+S`，仅在 Xrose 主窗口有焦点时生效，包括窗口内的对话框。其他应用占用该组合键时，可使用添加节点或订阅对话框中的扫码按钮。

## 8. 连接诊断与流量统计

- **实时速率与进程视图**（`flow::FlowRateTracker`）：后台监控按 ~1.2 秒采样活跃连接并结合单调时钟计算瞬时上下行速率，聚合生成进程实时流量排行 `ProcessLiveTraffic` 写入 `RuntimeView`；支持按连接或活跃进程查看、按需加载进程图标缓存、断开单条/全部连接，以及一键生成精确域名、单 IP 或进程阻止规则（去重后置顶保存）。
- **会话录制与链路测试**：录制维护最多 2000 条连接元数据与 100 次测试结果（不采集内容正文，停止即冻结）。链路测试（`trace.rs`）结合工作模式预览命中规则（含直连 `Block` 规则解释），并与本机 DNS 预解析、多上游 DNS A/AAAA 并发对比（`dns/probe.rs`）、STUN UDP/NAT 探测及免内核系统网络体检（`system/net_diag.rs`）共同服务于测试页与 `xrose-cli doctor`。
- **历史流量统计**（`traffic_statistics`）：默认关闭，开启后在 `service/traffic_statistics` 中串行采样、保存与查询。按会话与采集时本地自然日归档，使用离线 PSL 归并主域名并脱敏程序路径；实体身份与名称经 `SecretBox` 加密存入 `traffic_sessions` / `traffic_entities` / `traffic_daily`，每 10 秒增量覆盖写入脏记录（运行中清零或切换内核通过代次隔离防错账）。前端共享 `UiSnapshot.traffic_statistics` 缓存，渲染与 tick 零磁盘读取。

## 9. 生命周期、配置重载与启动

### 编译与进程生命周期

```
UI 操作 ──▶ AppModel 变更 ──▶ SQLite(加密) ──▶ config_dirty = true
                                                   │
             点击「应用配置」/ 启动 / 热重载触发 ◀────┘
                              │
                    compile(model, settings)
                              │
                   写入 runtime/<core>-config.json
                              │
                  xray run -test / sing-box check
                       │ok                 │fail
                     spawn            保留原内核并报告错误
                       │
               stdout/stderr → LogBus → 滚动文件
                       │
             Clash API / StatsService → 快照 → UI 轮询
```

启动流程依次完成设置同步、预编译检查与缺失内核/规则集/地理数据补齐、配置编译与校验，最终交由 `ProcessSupervisor` 启动进程、收集日志并执行退避崩溃重启。

### 配置热重载与设置生效范围

「设置 → Core → 热重载」默认开启。开启时，服务防抖合并连续配置编辑，在运行中先校验候选配置，再替换内核进程，失败时尝试恢复原配置并保留待应用标记；应用更新后若检测到已安装的提权助手需要更新，也会按需自动触发更新。`AppService::save_settings` 仅在影响当前内核运行的参数变更时标记 `config_dirty`：

| 设置项 | 生效方式 |
| --- | --- |
| 内核类型、当前内核版本或自定义路径 | 下次启动生效；运行时按热重载设置应用 |
| 已启用入站端口、监听地址、局域网配置 | 校验并重载内核后生效 |
| TUN 开关与严格路由 | 运行时校验并重启内核；停止时仅保存 |
| 内核日志级别、sing-box API 端口、Xray 统计端口 | 对应内核重载后生效 |
| 默认测速地址 / 间隔 | 自动出站组或 Xray observatory 需重载；手动测速即时读取 |
| 崩溃自动重启策略与重试次数 | 即时更新监控策略 |
| 系统代理、PAC 开关与端口 | 经系统代理 / PAC 流程应用，不重启内核 |
| 外观、语言、材质、隐私、托盘、通知、日志轮转限制等 | 客户端处理，不重启内核 |

### 启动动作与系统通知

- macOS 应用包由 `scripts/build-macos.sh` 在构建时生成。签名优先使用 `XROSE_SIGN_IDENTITY` 指定的身份，其次使用本机有效的 Apple Development 证书；没有可用证书时使用临时签名。从 Finder 或 `open` 启动 `Xrose.app` 时使用其应用身份；直接运行 `xrose-app` 不创建应用包，也不重新启动进程。
- `Settings.autostart` 独立控制系统登录入口 `--autostart`；`startup_proxy` 与各组网 `startup_*` 配置由 `run_startup_actions()` 在 GUI 每次启动时按序执行一次。
- `AppService::toast()` 维护应用内提示并在启用系统通知（Windows WinRT Toast / macOS UserNotifications / Linux 桌面通知）时后台投递，30 秒内同内容去重，投递失败仅记日志而不递归弹窗。

## 10. 托盘、窗口材质与发布选项

桌面发布分 full 与 lite。full 在启动 GUI 前调用 `bundled::prepare`，将已校验的离线资源安装到用户数据目录，通过 `BinaryManager` 注册 sing-box。完成标记按数据目录保存。应用更新按当前平台、架构和格式匹配 lite 产物。资源布局与工作流见[发布说明](RELEASE.md)。

```
Shell_NotifyIcon / AppKit status item / Linux StatusNotifierItem
                          │ 回调仅排队 TrayCommand
         GPUI 立即唤醒（200 ms 兜底）打开 / 收起独立 PopUp 窗口
                          │
         GPUI Kit dashboard (400 ms 快照刷新)
                          │
          AppService ◀── 用户动作 ──▶ 主窗口 / 设置 / 退出
```

- **托盘浮窗**：三端共用 `tray::popup` 渲染与 `tray::dashboard` 快照，包含状态、直连/节点切换、路由预设下拉、TUN 开关、组网状态与测速入口；Windows 按显示器 DPI 定位并在任务栏方向执行 250ms/167ms 非阻塞滑入动画，遵循系统动态效果开关；Linux D-Bus 注册在独立线程完成，无宿主时 `available()` 返回 `false`，关闭窗口直接退出。主窗口句柄独立跟踪，浮窗复用不影响主窗口隐藏恢复。Wayland 通过 xdg-output 的逻辑坐标选择显示器，支持 layer-shell 时沿托盘所在面板的边缘定位，否则使用普通浮窗。
- **关闭到托盘**：Windows 和 macOS 隐藏主窗口。KDE 撤下主窗口及其任务栏条目，服务和托盘继续运行。从托盘恢复时重建控件并恢复页面和筛选条件。托盘宿主离线时自动恢复隐藏的主窗口。其他 Linux 桌面最小化主窗口。
- **窗口材质**：`Settings.backdrop` 仅影响客户端外观，主题切换后重新执行 `window_effects::make_surface_translucent`；Windows 切换时清理旧 accent policy 并重设 DWM 客户区扩展（支持 Mica / Mica Alt / Acrylic），macOS 提供 vibrancy 毛玻璃，Wayland 按合成器公布的模糊能力（`ext-background-effect-v1` / KWin blur）为侧栏与托盘浮窗提供毛玻璃，X11 与不支持模糊的环境使用纯色面板。
- **调试与发布变量**：
  - 发布构建设置 `XROSE_GITHUB_REPOSITORY=<owner>/<repo>` 可启用「关于」页项目链接与客户端版本检查；未配置仍可检查内核更新。
  - 开发验证可在隔离 `XROSE_HOME` 下设置 `XROSE_TRAY_PREVIEW=1` 自动弹出托盘浮窗（debug 构建主窗口支持 `Ctrl/Cmd+Shift+T` 复现打开）。

## 11. 持久化与安全边界

- **加密存储与迁移**：使用 SQLite WAL 模式。新库按 [v1 建表定义](../crates/xrose-core/src/storage/schema_v1.sql) 一次创建，版本由 `PRAGMA user_version` 记录。v2 停止使用旧配置中的前置节点 ID，手动 SOCKS5 地址保存在加密设置中。开发版 v14、v15 使用相同表结构，首次打开时先备份数据库，再更新标识和版本号。更早的开发库须先用支持 v14 或 v15 的开发版打开，或另用新的数据目录。节点凭据、订阅令牌、局域网认证、组网配置与统计实体均使用 ChaCha20-Poly1305（`SecretBox`，`enc:v1:` 前缀）加密，密钥文件权限 `0600`（支持通过 `XROSE_SECRET_KEY` 覆盖，详见 [`storage/crypto.rs`](../crates/xrose-core/src/storage/crypto.rs)）。
- **提权助手隔离**：GUI 以普通用户权限运行；`--helper` 在进程入口最早期分流，不加载用户配置库与界面。助手经受限单行 JSON IPC（Unix domain socket / Windows 命名管道）执行 TUN 内核代启、组网工作进程托管、系统 DNS 与透明代理规则，通过 `Ping` 校验应用版本、TUN 助手协议版本、组网协议版本与助手修订号。Unix 校验 `SO_PEERCRED` / `LOCAL_PEERCRED` euid，Windows 校验同用户 DACL、调用者 PID 与 SID，并仅对通过 SHA-256 摘要校验的已登记内核使用固定参数启动，通过进程句柄与 Job Object / 独立进程组确保异常退出回收。安装、更新与卸载通过系统原生授权（UAC / `osascript` / `pkexec`）完成，不提供 setuid 包装。
- **系统代理还原**：在修改网络设置前保存快照。macOS 分别使用内核实际监听的 HTTP 和 SOCKS 端口，还原时先写地址，再还原启用状态并回读验证。设置页与菜单栏浮窗订阅 macOS 当前网络的代理变更，显示 Xrose、部分使用或外部代理状态。自动重载和退出还原会跳过用户或其他应用已修改的条目。还原失败会保留快照供重试，GUI 在获得单实例锁后执行崩溃恢复。
