# Xrose 架构与编译约定

## 为什么是 AppModel

若直接以某个 core 的 JSON 建模，第二个 core 会到处漏。Xrose 的 `AppModel`
只描述用户意图：

- `work_mode` 保存本机直连或代理，与分流模板、出站组选择独立。
- `nodes`：协议 + 凭据 + TLS/传输，不含 core 字段名；
- `groups`：selector / urltest / fallback / loadbalance；
- `routing.rules`：语义 matcher（geosites / geoips / rule_sets / process ...）；
- `dns`：服务器（tag/address/domains/detour）、fake-ip、hosts、final；
- `inbounds`：mixed/socks/http/tun/tproxy/redirect。

`adapters::CoreAdapter::compile` 是唯一可以出现 `outbounds`、`protocol`、
`route` 这类字段名的地方。

## 本机直连与迁移

新配置默认采用 sing-box、`WorkMode::Direct` 和系统 DNS，默认广告拦截规则由用户启用。节点页、顶栏和托盘共用工作模式选择接口。虚拟标识 `__xrose_direct__` 仅用于界面。节点计数、节点订阅、分享、组网同步只处理实际节点。

数据库 v10 迁移热重载偏好，v11 移除旧的最小化到托盘设置。v12 在迁移前备份，逐条解密配置，为缺少工作模式的旧配置写入 `proxy`（有节点）或 `direct`（空节点），并加密保存。已声明的模式和未知 JSON 字段保留。选择工作模式只修改该字段，点选真实节点或组网出口时，工作模式与节点选择一起保存。

直连编译省略远端出站与前置代理，代理动作使用直连，启用的阻止规则、组网路由及 MagicDNS 继续生效。依赖代理的 DNS 上游及其分域规则会跳过并给出警告，缺少可用上游时使用系统 DNS。预下载范围限于拦截规则和有效 DNS 上游的依赖。开启系统代理且未启用 TUN 时，直连流量经本机 HTTP 或 mixed 入站，以便统计和拦截。

## 关键差异与处理

| 能力 | Xray | sing-box | Xrose 处理 |
| --- | --- | --- | --- |
| mixed 入站 | 无 | 有 | Xray 拆成 SOCKS + HTTP(port+1)，界面提示 |
| 局域网认证与独立端口 | SOCKS 和 HTTP 分别配置账号 | mixed 入站配置账号 | 共用端口时本机也需认证；独立端口模式保留本机回环入口 |
| 前置代理 | 目标节点 `streamSettings.sockopt.dialerProxy` | 目标出站或 endpoint 的 `detour` | 每个配置保存前置节点 ID；前置节点本身、组网本机出口和直连规则保留原路径 |
| TUN | 原生（自动路由需 26.6.27+） | 原生 | Xray 生成 `gateway`、`autoSystemRoutingTable` 与 `autoOutboundsInterface`；共享提权启动与进程回收 |
| AnyTLS | 不支持 | 1.12+ | 密码认证、必选 TLS 与空闲会话参数；Xray 跳过并警告 |
| OpenVPN | 不支持 | 1.14+ endpoint | TLS 客户端、UDP/TCP、账号密码与 PEM 证书、控制通道密钥、分域 DNS；旧内核跳过并修复组引用 |
| OpenConnect | 不支持 | 1.14+ endpoint | 六种协议类型、HTTPS 路径、认证组、账号密码/证书/会话、分域 DNS；交互认证暂未接入 |
| WireGuard | 用户态出站 | endpoint | 单对端、密钥、地址、允许网段、MTU、reserved；不创建系统 WireGuard 网卡 |
| 自定义规则分组 | 按组展开 | 按组展开 | 启用组按顺序先于未分组/预设规则；本地预览共用 `effective_rules()`；PAC 从启用规则收集直连条件 |
| selector 热切换 | 无 | Clash API | Xray 选择节点 = 改配置 + 重启 |
| urltest | balancer + observatory | 原生 urltest | 分别生成 |
| 故障转移 / 自定义节点切换策略 | 路由固定到选中节点 | 单成员选择组，避免旧缓存覆盖选择 | 客户端按策略探测、校验并重启；故障组不再降级为最低延迟 |
| loadbalance（默认策略） | random | 无 | sing-box 映射为 selector + 警告 |
| block | blackhole 出站 | `action: reject` | 不生成 legacy block 出站 |
| geoip/geosite | 原生字段 | 1.8 起移除 | 自动生成 `.srs` rule-set 引用 |
| GFWList 来源 | `geosite:gfw`（需对应资产） | `geosite-gfw` rule-set | 使用 MetaCubeX `gfw.srs`；编译、预下载、手动更新共用来源解析 |
| fake-ip | 不支持 | 原生（1.12+ typed DNS） | Xray 跳过 DNS fakeip server |
| DNS schema | `servers[].address` | 1.12+ typed server + domain_resolver | 只生成新式 DNS |
| 连接列表与断开 | 无 | Clash API | 支持断开单条或确认断开全部连接 |
| 实时进程视图 | 无 | 活跃连接按进程聚合 | 支持按进程查看连接与创建阻止规则 |
| 连接进程归因 | 无连接列表 | `route.find_process` | Linux/Windows/macOS 开启进程查找；权限或远端请求导致缺失时明确提示 |
| 实时统计 | StatsService(gRPC) | Clash API | Xray 通过 `xray api statsquery` |
| 历史流量统计 | 业务入站总量、节点与直连出站计数 | 总量及节点／进程／主域名分类 | 默认关闭，按会话与本地自然日保存，周一为每周起点；柱状图与表格共用查询结果 |
| 组网访问 | 优先 freedom 规则 + 专用 DNS | 绑定组网接口直连 + TUN 排除 + 专用 DNS | `CompileContext.mesh` 注入，不改变 AppModel；MagicDNS 优先于 fake-IP |
| 组网出口出站 | 本机认证 SOCKS 出站 | 本机认证 SOCKS 出站 | “节点”页列出已连接组网的出口候选。每个组网配置维护一个稳定的代理节点 ID，供出站组和分流规则引用 |
| Windows 组网与 TUN 共存 | 从自动路由中排除组网网段 | 保留用户 `strict_route` 设置 | 严格路由可能影响组网 DNS，界面给出提示 |

节点 HTTP 延迟与下载测试由 `runtime::probe` 启动短期测试内核，适配器生成仅含目标节点与本地 HTTP 入站的配置。
测试客户端显式连接该入站，关闭系统代理继承、重定向与自动解压；不复用当前选择组、规则、API 端口或缓存文件。
sing-box 测试内核启用 `auto_detect_interface`；Xray TUN 下的测试内核检测默认上联网卡并设置 `sockopt.interface`，先解析节点地址并保留 TLS SNI。TUN 运行状态取自已应用配置，普通延迟测试此时改用 HTTP；未启用 TUN 时保留 TCP 连接测试。
下载按实际接收字节计算平均速度，限制接收时间与数据量；空响应、HTTP 错误及连接中断返回失败。测试结束后停止并回收进程，删除临时配置。
WireGuard 字段依据 [sing-box endpoint 文档](https://sing-box.sagernet.org/configuration/endpoint/wireguard/)
与 [Xray 出站文档](https://xtls.github.io/en/config/outbounds/wireguard.html)。`Credentials.wireguard` 随配置整体加密保存，`NodeDraft` 统一校验密钥长度与地址格式。
配置中的前置节点失效时，编译会提示重新选择。独立 HTTP 延迟和下载测试复用同一前置节点；启用前置代理时，批量延迟测试改用 HTTP。Xray TUN 下单独为首跳解析地址并绑定上联网卡。字段定义见 [Xray Sockopt](https://xtls.github.io/config/transports/sockopt.html) 和 [sing-box 拨号字段](https://sing-box.sagernet.org/configuration/shared/dial/)。

节点切换策略保存在每个出站组的 `switch_policy`。数据库 v6 将内置代理组原先继承的手动模式明确保存为 `off`。任何类型的组显式关闭自动切换后，均按手动选择编译。故障转移组默认由客户端按成员顺序处理；其他类型只有显式选择策略时才由客户端接管。适配器先按实际内核能力剔除不可编译的节点，再把代理路由实际引用的策略组和成员交给服务。路由页设置默认代理出站。节点列表和标题栏均使用默认组。自动切换设置弹窗独立选择要编辑的组。

`service/outbound_groups.rs` 提供出站组保存、删除及单节点归属修改。复用现有组结构，成员和策略以一次配置事务保存，成功后发布快照并在内核运行时标记待应用。表单提交检查原始组配置或节点归属，拒绝覆盖并发编辑；未显式修改首选节点时保留最新选择。成员移除后修复选中节点，优先选取当前内核可用的成员。默认组以及路由或 DNS 引用的组禁止删除，禁用规则同样参与引用检查。节点页管理弹窗与节点归属弹窗共享服务接口，策略控件与已有自动切换设置复用。

`service/node_switch.rs` 仅在有运行中内核、无待应用编辑时探测已应用配置。每组独立记录检查间隔和当前节点连续失败次数；成功即清零，停止或更换内核进程后放弃旧探测。决策只比较本轮结果，不使用列表中历史测速或总流量。延迟使用组的 HTTP 测试地址和容差；吞吐量串行取最多 1 MiB 的下载样本，至少提升 10% 才切换。故障时按当前节点的订阅 ID 或服务器归属地的国家／地区字段过滤，不比较城市或翻译后的完整标签。范围内全不可用时保留原节点。

切换在内核操作锁中复核进程与组成员，基于上次已应用模型校验新配置，启动失败时回滚。运行时选择通过快照提供给节点页、标题栏与托盘，不改写用户保存的起始节点。

新增 `RoutingConfig.rule_groups` 使用 serde 默认空数组，旧配置无需数据库结构迁移；预设只替换未分组规则。
AnyTLS 字段依据 [sing-box 官方文档](https://sing-box.sagernet.org/configuration/outbound/anytls/)。
Xray TUN 字段依据 [官方 TUN 文档](https://xtls.github.io/en/config/inbounds/tun.html) 及 [26.6.27 配置定义](https://github.com/XTLS/Xray-core/blob/v26.6.27/infra/conf/tun.go)。
Xray 自动路由通过 CIDR 差集排除用户与组网网段，DNS 劫持规则先于 LAN 绕过；系统解析器在 TUN DNS 下替换为显式上游，避免解析回环。
`strict_route` 与网络栈选择在 Xray 下不映射，编译时警告；Linux/macOS 不自动接管系统 DNS，Windows 配置 TUN 接口 DNS。
提权助手从经过摘要校验的已登记二进制识别内核类型，校验对应 TUN 入站格式，生成该内核的固定启动参数，并提取 TUN 地址用于组网基线保护。

组网独立于 `CoreKind`：`mesh::MeshProfile` 表达网络身份、入口、出口选择、子网发布、远端网段和端口转发意图，
`MeshController` 通过带版本和调用者鉴权的助手 IPC 管理会话。EasyTier 2.6.4
静态库只在 Xrose `--mesh-worker` 初始化；Tailscale 使用固定 sing-box 1.14.4
子进程及鉴权 gRPC。UI 只调用 AppService、读取 `UiSnapshot.mesh`。
SQLite 的 `mesh_profiles` 全量加密；新增字段使用 serde 默认值，不改变数据库结构。出口网关凭据随组网配置保存且不进入快照；生成的本机 SOCKS 节点随代理配置保存，编辑组网时保持节点 ID 与出站组引用。
“节点”页从已连接的组网快照读取出口。Tailscale 使用已批准的出口标志，EasyTier 使用 Xrose 发布的签名组声明、IPv4 默认网段公告或用户指定的出口地址。切换到其他设备时，服务重新连接相应组网，等待目标设备就绪，再在同一事务中保存组网出口和手动出站组的选择。组网重连或保存失败时尝试恢复原连接。

数据库 v7 为加密的 EasyTier 配置补齐原行为对应的高级设置，保留更新时间及未知字段。高级设置经脱敏快照送入编辑草稿，由组网协议传递至工作进程，在构建 EasyTier 实例前应用；线程模式也用于创建实际 Tokio 运行时。网络偏好可随配置同步，本机转发、入站和穿透策略保留在本机。

组网协议 v6 允许应用在已有连接中更新端口转发。组网工作进程管理 TCP / UDP 监听，按协议和本地监听地址复用规则。EasyTier 通过设备虚拟地址选择网内目标，Tailscale 通过只路由到自身 endpoint 的本地认证 SOCKS 入口转发。工作进程确认更新后才持久化规则，监听或保存失败时恢复原规则。转发变化不触发代理内核或组网重启。

组网协议 v9 将两种方向统一为 `ForwardRules` 在线更新事务，界面合并在「端口转发」卡片，通过弹窗选择方向。持久化仍使用 `port_forwards` 与 `service_forwards`，SQLite v9 的加密迁移保留未知字段和更新时间；配置同步保留接收端本机规则。EasyTier 动态增删所属虚拟 IPv4 上的 TCP/UDP 监听，并通过主机套接字访问服务。Tailscale 使用固定的本地鉴权 SOCKS 服务桥；sing-box 只把自身 endpoint 回环映射的入站流量交给服务桥，规则表按协议与组网端口动态匹配，未配置目标拒绝，不直连任意主机端口。服务桥的凭据随机生成，只用于受保护的运行配置。配置共享通道优先于服务桥匹配。工作进程先校验两种规则，再更新并确认，保存失败时恢复原规则；方向切换也在同一事务中完成。增删不重启组网或代理；新增和删除其他规则保留已有转发连接。

EasyTier 使用公开的 Instance/PeerManager API，出口 TCP/UDP 经独立用户态网络栈发送，路由不可达时不调用本机直连。TCP/UDP 域名请求使用系统解析器，并选择 IPv4 目标；代理核心可按自身 DNS 策略预解析目标。Tailscale 配置编译仍集中在 sing-box adapter。

`CoreManager` 独立保存最后已应用的模型、设置和编译结果。组网变化串行执行
编译、校验、必要的重启及进程检查，失败回滚已保存配置，保留用户待应用编辑。
Tailscale 使用用户态 endpoint 和仅包含批准路由的专用 TUN，避免 `accept_routes`
把控制服务器路由直接安装到系统；仅为当前 tailnet 后缀设置带归属记录的 DNS：Windows NRPT、macOS `/etc/resolver`、Linux systemd-resolved 按接口域名路由。
Windows 的独立 Tailscale 组件关闭 `auto_detect_interface`，控制服务器连接使用系统路由，以支持已有代理 TUN 或 VPN。组网 TUN 仍仅接管指定网段。协议版本 7 要求更新提权助手，使工作进程使用修正后的配置。CLI 的 `mesh api-check` 等待登录链接、待审批或已连接状态后才报告成功。
出口和发布子网不加入宿主机默认路由；来自 Tailscale 的流量仅允许访问用户发布的网段，发布出口时允许互联网目的地。Unix 工作进程使用独立进程组，停止时回收子进程。
自动重连使用当前应用会话的连接记录。监控任务在提权助手恢复可用后核对工作进程状态，按网络类型串行恢复缺失的连接，并重新校验代理共存路由。每个网络可单独关闭重连，手动断开或注销会清除重连请求。

组网发现使用版本化 TCP 协议。服务仅向已连接网络的成员地址发送握手，EasyTier 监听本机虚拟地址，Tailscale 在用户态端点将本机地址的专用端口通过带随机凭据的 SOCKS 入站连接转交本机同步监听器。组网页展示经发现的 Xrose 信息与后端公布的能力。共享范围绑定当前网络和勾选项目，有效期十分钟。默认共享码模式使用挑战应答。无认证模式通过 Noise NN 临时密钥协商传递本次共享的数据密钥。两种模式均验证并解密数据包。接收端在内存中预览独立项目，确认时复查当前配置、依赖和运行中的组网，使用一个 SQLite 事务更新加密记录。发现协议、共享码和预览不写入数据库。

详见 [异地组网边界与构建说明](MESH.md)。

连接诊断由 `AppService` 统一提供：后台监控采样维护有界的会话录制，
`UiSnapshot.recording` 提供回看数据，导出在后台完成。录制不采集正文，
记录连接的首次/末次观测、关闭时间和最终测试结果；停止后冻结，开始新录制会替换旧记录。
Xray 可录制测试结果，不能提供连接列表。最多保留 2000 条连接和 100 次测试，溢出时淘汰最旧记录。

测试的本机 DNS 预解析与模型路由预览不等于 core 实际决策。运行中测试通过本地代理入站交给 core，
没有对应入站则报错，不静默退回直连。Clash 元数据关联需要目标主机和端口匹配，TCP 还匹配源端口；
HTTP 排除测试前已有连接，但同时访问相同目标时仍可能无法唯一归因，因此界面标为关联观测。
core 不暴露连接 DNS 解析器时明确显示不可得；返回路径是根据已使用的连接推导，时间仅表示端到端探测耗时。
诊断规则使用精确域名、单 IP（/32 或 /128）或进程名，去重后在用户规则中置顶保存，
内置 LAN/中国 IP 绕过仍优先，规则变更按热重载设置应用。

## 实时流量与系统诊断

`flow::FlowRateTracker` 接收连接标识、建立时间、进程和累计字节。后台采样用单调时钟计算速率，按当前活跃连接汇总进程并生成排序索引。界面从同一快照读取明细和指标。历史累计器单独处理跨采样、跨天及内核重启。

链路测试按工作模式预览实际启用的规则，并报告阻止规则及所属分组。DNS 对比分别发起 A 与 AAAA 查询，显示各上游的回答、耗时及地址差异。系统网络报告在后台按需采集，通过测试页与 CLI 共用。隐私模式在展示、复制时使用脱敏结果。UDP 连通性与 NAT 类型通过用户单独发起的 STUN 探测获得。

## 历史流量统计

`traffic_statistics` 提供无 UI 的累计器、日期窗口、分类与查询模型，`service/traffic_statistics` 串行执行采样、保存、清空和历史查询。采样复用约 1.2 秒的后台监控，手动刷新连接不进入累计器。运行代次包含内核、进程、启动时间、重启计数和提权会话，响应返回后重新验证；归因读取 `CoreManager` 的已应用配置，不读取待应用编辑。

运行中开启统计或清空后以第一次成功采样为基线；统计开启后启动的新内核从零累计。应用会话跨内核重启保留。sing-box 按连接累计值取差，分类增量限制在同次全局增量以内，剩余部分进入未归因；短连接可能无法完整归因。Xray 总量仅求和已编译的业务入站，节点分类使用各出站计数，两者不强行对账。程序归类移除路径及内核附带的账号后缀；主域名使用离线 PSL，支持公共多级后缀和国际化域名。

数据库 v8 新增 `traffic_sessions`、`traffic_entities`、`traffic_daily`。实体身份与名称通过 `SecretBox` 加密，SQL 索引仅使用随机实体 ID。日记录以会话、日期与实体为唯一键，保存内核来源、维度、上下行累计、采样时间和缺口标记。每 10 秒在事务中覆盖脏记录，失败保留内存数据重试；覆盖写入可重复执行。清空与写入串行，另以代次使旧采样及查询失效。记录长期保留，删除节点不级联删除历史。

`AppService` 的统计开关、查询与清空入口同步返回，磁盘访问进入后台任务。`UiSnapshot.traffic_statistics` 为共享缓存，渲染和 tick 不读取数据库。查询按日期和维度索引筛选，将落库记录与本次会话的内存记录按唯一键覆盖合并后汇总；排行限制只影响返回行数，不影响总量。展开的当前页面最多每 5 秒刷新，图表和表格共用数据。历史日期按采集时本地日期保留，时区变化不重新归档；日边界处增量按采样完成日期记录。

## 生命周期

```
UI 操作 ──▶ AppModel 变更 ──▶ SQLite(加密) ──▶ config_dirty = true
                                                   │
                    点击“应用配置” / 启动 ◀──────────┘
                              │
                    compile(model, settings)
                              │
                   写入 runtime/<core>-config.json
                              │
                  xray run -test / sing-box check
                       │ok                 │fail
                     spawn            显示行号+错误，不启动
                       │
               stdout/stderr → LogBus → 滚动文件
                       │
             Clash API / StatsService → 快照 → UI 轮询
```

## 订阅合并

1. `fetch_subscription`：条件请求 + `subscription-userinfo`。
2. `parse_subscription`：自动识别 Base64 / 链接 / Clash YAML / SIP008 / sing-box JSON。
3. 节点全部替换该订阅的旧节点（保留其他订阅），按指纹去重。
4. Clash `proxy-groups` 按名称合并到现有组；新节点自动加入默认 selector/urltest。

编辑订阅沿用原 ID 和节点归属。更换 URL 后清理旧地址的缓存验证信息和流量元数据，跨来源更换时清除请求头。

订阅请求和资源下载分别读取当前代理开关。开启时使用正在运行的内核已应用的 HTTP/mixed 监听地址和认证信息；内核未就绪会提示启动或关闭该开关。关闭时客户端显式直连。版本查询、校验文件、内核包、规则集、Xray 地理数据和组网组件共用资源下载策略。自定义 User-Agent 随更新请求发送，订阅可单独覆盖；改变订阅的有效 User-Agent 后，首次请求重新获取内容。

数据库 v4 将原先写入订阅的内置默认 User-Agent 迁移为继承全局设置，保留自定义值。旧配置的前置节点默认为空，新增开关默认为关闭。局域网账号使用现有密钥加密保存，运行配置使用独立临时文件原子替换，Unix 权限为 0600。

## 启动顺序

1. 同步设置到模型（端口、LAN、TUN）。
2. 预编译检查模型。sing-box 和 Xray 分别保存可执行文件来源，默认按进程 PATH 查找。选定的本地文件缺失时提示指定路径或下载内核；已选下载版本缺失时沿用安装恢复流程。
   sing-box 收集启用规则/DNS 引用并预下载规则集；Xray 按配置引用补齐 geo 数据。
   TUN/组网按需准备提权助手；Tailscale 连接前校验并补齐组件。
3. 编译 → 写 `<runtime>/<core>-config.json`。
4. `xray run -test` / `sing-box check`；失败则显示行号且不启动。
5. 启动进程；stdout/stderr 进入 LogBus；watchdog 监控退出并退避重启。

## 托盘与最小化

```
Shell_NotifyIcon / AppKit status item / Linux StatusNotifierItem
                          │ 回调只排队 TrayCommand
         GPUI 立即唤醒（200 ms 兜底）打开 / 收起独立 PopUp 窗口
                          │
         GPUI Kit dashboard (400 ms 快照刷新)
                          │
          AppService ◀── 用户动作 ──▶ 主窗口 / 设置 / 退出
```

- `tray::popup` 是三个平台共用的 GPUI Kit 浮窗。状态、流量、节点、测速与错误反馈均来自
  `tray::dashboard::build` / AppService；render 不读取磁盘或访问网络。
- 浮窗尺寸受显示器工作区约束，节点区域可滚动。Windows 按托盘图标所在显示器的 DPI 换算位置，
  macOS 将 status item 的 AppKit 坐标转换到 GPUI 坐标；Wayland 由合成器决定实际位置。
- 再次点击托盘图标、Esc 或失焦收起浮窗；Windows/macOS 隐藏并复用原生窗口，避免无障碍提供者
  引用失效句柄。点击 Windows/macOS 图标导致失焦时延迟到图标动作处理，
  避免先关闭又重新打开。显示主窗口与打开设置均先收起浮窗。
  原生显示/隐藏操作经 Win32 自定义消息 / AppKit 下一轮 run loop 执行，避免同步回调重入 GPUI。
- Windows flyout 使用原生消息窗口的非阻塞定时器，以任务栏所在边缘为滑入/滑出方向：
  250 ms Direct Entrance、167 ms Gentle Exit，中途反转从当前位置继续；
  `SPI_GETCLIENTAREAANIMATION` / `SPI_GETUIEFFECTS` 关闭时直接完成。动画期间取消 topmost，
  让窗口从任务栏后方出现，收起完成后保留窗口和无障碍句柄。
- 路由下拉框使用 GPUI Kit Select，同一窗口内绘制菜单，跟随快照同步当前预设和语言；
  `switch_route_preset` 复用已有预设模型，Core 运行时经 `apply_config` 校验/重启，停止时只保存，
  Core 忙时不接受切换。同一预设的重复选择不覆盖已有规则。
- TUN 开关通过 `AppService::set_tun_enabled` 持久化并同步模型，Core 运行时调用已有
  `apply_config` 校验/重启流程；停止时不启动 Core。两种内核均可启用，Core 忙时不接受切换。
- 主窗口原生句柄在创建时记录，Windows 不再枚举同类窗口，macOS 不再对所有窗口隐藏/恢复。
  最小化及启动时最小化均使用系统原生窗口行为。手动开启 `close_to_tray` 时提示一次；
  之后关闭窗口时，Windows/macOS 隐藏主窗口，Linux 使用 GPUI 的 minimize/activate 保留任务栏入口。
- Linux D-Bus 注册与更新在专用线程执行，通过有界通道接收快照；只有注册成功且宿主在线时
  `available()` 为 true。没有托盘宿主或 Windows shell 拒绝注册时，关闭窗口直接退出。
- `Settings.backdrop` 只描述外观偏好，不影响代理模型或 `config_dirty`。UI 按平台提供材质选项；
  主窗口/浮窗即时应用，主题切换后重新应用。Windows 切换材质时同时清理 accent policy 与旧 DWM
  材质，扩展客户区并回读 DWM 属性确认请求成功；Mica 根表面不再覆盖额外的 tint。
  不支持的材质退化为不透明面板。Linux 当前使用不透明背景。

发布时，在构建环境中将 `XROSE_GITHUB_REPOSITORY` 设为公开 GitHub 仓库的 `owner/repository`，即可启用关于页的项目链接与客户端版本检查。未配置时仍可检查内核更新。

开发验证可在隔离的 `XROSE_HOME` 下设置 `XROSE_TRAY_PREVIEW=1`，启动后自动打开浮窗；
debug 构建的主窗口在控件聚焦时支持 Ctrl/Cmd+Shift+T 再次打开，用于检查隐藏后的复用路径。

## 安全边界

- 桌面 UI 以普通用户运行；Windows TUN 核心通过 `tun_process` 由特权助手代启。
  `CoreManager` 在校验配置前验证 Wintun 压缩包摘要并部署 DLL 到实际核心目录。
  `ProcessSupervisor` 统一处理本地及助手进程的日志、停止、重启和回滚。
- helper 协议为单行 JSON，只暴露 TUN 核心代启、系统 DNS、透明代理规则等必要操作。
  TUN 请求校验协议版本、管道调用者 PID、所属用户、已登记核心摘要和运行目录内的配置；
  助手自行构造固定命令参数。进程句柄监控负责客户端退出回收，Job Object 负责助手崩溃回收。
- helper 由系统服务/计划任务以管理员身份运行；GUI 通过本地 socket 调用。
  Unix 端校验 `SO_PEERCRED`/`LOCAL_PEERCRED` 的 euid（安装用户或 root），
  Windows 依赖命名管道的默认 DACL（创建者用户 + Administrators）。
- 设置页按当前内核的实际 TCP 监听地址和端口判断占用。对助手代启的内核，通过已认证的 TUN 会话查询该子进程的监听端口；旧助手不支持查询且本地无法确认时，提示更新助手，不将其判为其他进程。
- 组网预检及路由协调保留内核校验报告和每个网络的最近一次连接错误。代理内核拒绝变更时，根据发生错误的内核种类检查更新；独立的 Tailscale 组件仍按固定摘要验证。版本检查在后台读取配置所选可执行文件的实际版本，按内核种类保存结果。设置页与侧栏共用快照，网络请求失败不覆盖已发现的更新。
- 提权助手的 Ping 回复携带应用版本和组网协议版本。旧助手仍可响应存活检查；组网连接会先验证其协议。更新助手会用当前可执行文件重新注册并启动服务；在同一次提权中请求启动，只有收到当前版本的回复才报告完成。安装/更新/卸载/启动通过 UAC（`ShellExecuteEx runas` 等待子进程）、
  `osascript ... with administrator privileges`、`pkexec` 完成，不提供 setuid 包装。
- `--helper` 在 `xrose-app`/`xrose-cli` 的 `main` 最前面处理，早于数据库和 GUI；
  helper 进程不会打开用户配置库。
- 密钥文件 0600；数据库字段以 `enc:v1:` 前缀区分。
- 代理快照用于崩溃恢复：启动时发现残留即还原，避免用户断网。

## 设置生效范围

「设置 → Core → 热重载」默认关闭。开启后，服务合并连续保存的配置，运行中自动校验并重载，停止时沿用下次启动流程。候选配置校验通过后才替换内核进程，启动失败时尝试恢复原配置。失败保留待应用状态，可修正配置或手动重试。同一开关控制 GUI 启动和需要助手时的版本检查；仅更新版本或协议不匹配的已安装助手，一次运行中自动更新失败后可手动重试。

`AppService::save_settings` 统一判断设置是否影响当前所选内核；只有相关字段变化且内核运行时才标记 `config_dirty`。
客户端偏好更新不重写代理模型，不清除已有的节点、路由或 DNS 待应用变更；重复保存同一设置不产生新提示。

| 设置 | 生效方式 |
| --- | --- |
| 内核类型、当前内核版本或自定义路径 | 下次启动内核生效；运行时按热重载设置应用 |
| 已启用混合入站的端口、监听地址、局域网访问 | 校验并重载内核后生效 |
| TUN 开关 | 运行时沿用自动校验/重启流程；停止时保存 |
| 内核日志级别 | 重载内核后生效 |
| sing-box API 端口 / Xray 统计端口 | 仅对应内核需要重载 |
| 默认延迟测试地址 | 仅使用该默认值的自动出站组需要重载；手动测试读取最新设置 |
| 默认延迟测试间隔 | 仅 Xray observatory 使用；sing-box 使用出站组自身间隔 |
| 崩溃自动重启开关与重试次数 | 即时更新进程监控策略 |
| 系统代理、PAC 开关与端口 | 由系统代理/PAC 流程应用，不重启内核 |
| 隐私、语言、主题、材质、托盘显示、关闭窗口行为、更新偏好 | 客户端处理，不重启内核 |
| 自启、启动时最小化、单实例、日志文件轮转限制 | 不重启内核；启动行为及日志文件限制按客户端生命周期生效 |
| 其他内核的版本、路径、API/统计端口 | 不影响当前内核 |

## 登录启动与通知

`Settings.autostart` 独立控制系统登录入口 `--autostart`。`startup_proxy` 与两种组网的
`startup_*` 配置 ID 控制应用启动动作；`run_startup_actions()` 由每次 GUI 启动调用且每进程
执行一次，不影响 CLI 或服务构造。旧 `autostart_*` 字段通过别名读取，原隐式启用的登录启动
迁移为显式 `autostart`，不改动现有系统启动项。组网表单通过 `AppService` 同步配置与启动选择，
每个后端保留一个配置 ID，关闭未选中的配置不影响已选项。
代理按保存的模式启动，系统代理等待内核启动成功；Tailscale、EasyTier 依次走已有组网连接与路由共存入口。
组网支持 Windows、macOS、Linux；缺失配置和登录授权需求通过提示呈现。

`AppService::toast()` 保留诊断事件，并在系统通知启用时投递到通知队列；GUI 初始化通知工作线程，无需依赖快照轮询。
Windows 使用 WinRT toast 和 Xrose 应用标识，macOS 使用 UserNotifications 授权与请求接口，
Linux 使用桌面通知服务。系统通知默认关闭；启用后界面隐藏应用内通知，投递错误仅写日志，避免递归通知。
事件记住产生时的通知通道，关闭开关不会将已投递的系统通知重新显示为应用内提示。30 秒内同内容去重。
提权助手检查通过异步锁串行执行，已运行时不提示；需要启动或安装时才发通知。

OpenVPN / OpenConnect 的适配以 sing-box 1.14.4 的类型定义和官方客户端文档为准：
[OpenVPN](https://sing-box.sagernet.org/configuration/endpoint/openvpn-client/)、
[OpenConnect](https://sing-box.sagernet.org/configuration/endpoint/openconnect/)。
VPN 参数和 PEM 内容保存在节点凭据中；导入器不会执行 `.ovpn` 指令或读取其引用的本地文件。
配置编译与节点探测均生成用户态 endpoint，通用 TLS / v2ray transport 不叠加到 VPN。
启动时将实际内核版本传入编译上下文，sing-box 1.14 以下跳过 VPN；纯 VPN 配置无法编译时给出升级提示。

## 节点链接导入

节点弹窗保留粘贴文本的换行，通过 `AppService::import_content` 与 CLI 共用订阅解析器。明文列表和整体 Base64 编码的列表均逐行导入，保留原顺序，跳过无法识别的行并报告数量。节点使用原有指纹去重。

`subscription/links.rs` 识别 v2rayN 的 VMess JSON、标准 URI，以及 Shadowrocket 将 VMess / VLESS 地址和凭据整体编码的链接。备注、传输、TLS 和 Reality 参数转换为现有节点字段。VMess 显式加密方式保存在 `Credentials.method`，两个内核在该字段为空时使用 `auto`。格式参考 [v2rayN 导出实现](https://github.com/2dust/v2rayN/tree/master/v2rayN/ServiceLib/Handler/Fmt) 与 [Shadowrocket 格式生成示例](https://github.com/cedar2025/Xboard/blob/master/app/Protocols/Shadowrocket.php)。

节点及订阅页共用 `ui/qr`：页面粘贴、图片或文本文件拖放、屏幕取景都经 `AppService::import_shared_content` 导入。相同订阅地址只添加一次。macOS 和 Windows 另接收来源应用直接提供的图片或文本。Linux 文件拖放使用 GPUI 原生文件事件。

扫描入口位于添加弹窗内。透明取景窗口先显示四角动画，随后在后台识别指针附近的区域，识别图像最长边为 1024 像素。macOS 使用 ScreenCaptureKit，Windows / Linux 使用 XCap。桌面切换及滚动使上次识别结果失效，后台结果按采集时的区域映射回屏幕坐标。节点预览沿用隐私模式。批量导出复用单节点分享链接编码器，按节点配置顺序以换行分隔。

## 路由规则交换

`route/transfer.rs` 定义版本独立于数据库的 `xrose-routing` v1 JSON：`rules`、`rule_groups`、`rule_sets`、用于跨配置解析出站组的 `outbound_names`。不交换节点、订阅、运行数据或默认路由设置。导入为规则及分组生成新 ID；再次导入会追加副本。同标签且定义相同的规则集复用，不同定义拒绝整批导入。规则集标签校验同时覆盖显式引用和隐式 Geo 引用；来源限无认证信息及查询参数的 HTTP(S) URL。

`route/transfer/omega.rs` 按 [SwitchyOmega 的列表解析顺序](https://github.com/FelisCatus/SwitchyOmega/blob/master/omega-pac/src/rule_list.coffee) 转换为同一意图模型。Conditions 保持原顺序，带结果的排除条件使用末尾默认动作；AutoProxy 将例外放在普通条件之前。文件内的默认规则保存在导入分组中，不修改全局默认出站。不能等价转换的浏览器条件计入预览。编译复用现有适配器，sing-box 用网络类型数组表达同时匹配 TCP 和 UDP。

文件选择后，`AppService` 在后台读取和解析，将摘要放入快照，完整预览仅保存在内存。确认时检查预览 ID、当前配置、出站组与规则集引用，持久化成功后才发布模型；取消或解析失败不写入配置。GUI 与 CLI 复用同一预览和提交流程。
