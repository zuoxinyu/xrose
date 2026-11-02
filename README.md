# Xross

<p align="center">
  <img src="crates/xross-app/assets/logo-mark.png" width="140" alt="Xross logo" />
</p>

跨平台 Xray / sing-box 桌面客户端，使用 **Rust** 与 **GPUI Kit** 构建。

![节点页面](docs/screenshots/nodes.png)

<table>
  <tr>
    <td><img src="docs/screenshots/subscriptions.png" alt="订阅" /></td>
    <td><img src="docs/screenshots/routing.png" alt="路由" /></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/dns.png" alt="DNS" /></td>
    <td><img src="docs/screenshots/logs.png" alt="日志" /></td>
  </tr>
</table>

更多截图：[连接](docs/screenshots/connections.png) · [设置](docs/screenshots/settings.png)

Xross 的核心设计不是"给某个 core 的 JSON 套一层界面"，而是维护一套与 core 无关的
**用户意图模型**（节点、出站组、入站、路由、DNS），再由两个 `CoreAdapter` 编译成
差异极大的 Xray / sing-box 配置。因此第二个 core 不是事后补丁，而是一等公民。

```
┌────────────────────────────── GPUI (xross-app) ──────────────────────────────┐
│  节点 · 订阅 · 路由 · DNS · 连接 · 日志 · 设置       可折叠两栏布局 / 中英双语  │
└───────────────────────────────────┬──────────────────────────────────────────┘
                                    │  AppService（同步 API + 轮询快照）
┌───────────────────────────────────▼──────────────────────────────────────────┐
│                            xross-core                                        │
│  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐  ┌──────────────────┐   │
│  │ AppModel     │─▶│ CoreAdapter  │─▶│ CoreManager │─▶│ xray / sing-box  │   │
│  │ nodes/groups │  │ xray | sbox  │  │ 进程+校验   │  │  run -config     │   │
│  │ routing/dns  │  └──────────────┘  └─────────────┘  └──────────────────┘   │
│  └──────────────┘        │                  │                                │
│  订阅解析 / 分享链接       │ config.json     ├─ stdout/stderr → 滚动日志        │
│  SQLite + 加密           └─ run -test / check└─ Clash API / Xray StatsService   │
└──────────────────────────────────────────────────────────────────────────────┘
```

## 已实现

### 异地组网（Windows 首期）

- EasyTier 2.6.4 Rust 库静态链接，由特权助手的独立 Xross 工作进程运行；不调用 EasyTier 外部程序。
- Tailscale 使用独立的 sing-box 1.14.4，支持浏览器登录、自定义控制服务器、设备探测及完整 MagicDNS 域名。
- 两种网络独立于代理配置，可同时连接；SQLite v3 加密保存配置，退出清理工作进程和所属 DNS 规则。
- 自动生成代理直连、TUN 排除与 PAC 绕过；Windows 共存期间暂时关闭 `strict_route`，断开后恢复。
- 新增“组网”页面及 `xross-cli mesh`。构建、权限、使用限制和测试边界见 [组网文档](docs/MESH.md)。

Windows 首次构建使用 `powershell -NoProfile -File scripts/build-windows.ps1` 准备 protoc，另需安装 7-Zip。
全流量 TUN 共存必须在独立 Windows 环境验收；macOS/Linux 首期仅保持源码兼容。

### 1. Core 管理（`crates/xross-core/src/core/`）

- **二进制管理**：下载 GitHub Release、SHA-256 校验、多版本共存、按平台/架构选择、
  与 App 完全分离（`cores/<core>/<version>/`）。版本清单写入 `manifest.json`；
  支持自定义可执行文件路径覆盖。
- **进程生命周期**：启动/停止/重启、`watchdog` 自动拉起（指数退避、可配置次数）、
  退出码与 stderr 归因（端口占用、配置错误、权限不足、OOM/信号等）。
- **配置生成与校验**：编译 → 写临时配置 → 真实执行 `xray run -test` /
  `sing-box check` → 解析输出中的行号与错误，未通过时拒绝启动。
- **日志**：接入 stdout/stderr、按级别过滤、大小滚动日志文件
  （`xross.log` / `xross.1.log`，可配置上限与保留数）。

> Xray 与 sing-box 都不支持热加载配置。因此"改配置 → 应用"在本项目中就是
> **重启 core**；唯一走 API 的热切换是 sing-box 的 selector 组（Clash API）。

编译器面向 **sing-box 1.12+**（已在 1.14.4 上验证）：使用新式 typed DNS
（`type: https/udp/fakeip/local`）、`route.default_domain_resolver` 与
`action: reject`，不再生成 1.11 及更早版本的 legacy DNS 字段。
Xray 侧已在 1.8.24 上通过 `run -test`。

### 2. 配置编译层（`crates/xross-core/src/adapters/`）

- `AppModel` 以意图建模，不绑定任何 core 的 schema。
- **Xray**：`dokodemo-door` 透明代理、`balancers` + `observatory`（leastPing）、
  `geosite:` / `geoip:` 原生字段、fake-ip 跳过并提示、Stats API 入站与策略。
  没有 mixed 入站（自动拆为 SOCKS + HTTP(port+1)）；没有 TUN；不支持 Hysteria2/TUIC。
- **sing-box**：内建 TUN、`selector` / `urltest`、`.srs` rule-set、
  新版路由动作（`sniff`、`hijack-dns`、`reject`）、typed DNS + fake-ip、Clash API。
  旧版 `geoip`/`geosite` 字段已移除，编译器自动生成 rule-set 引用
  （通常使用 SagerNet；GFWList 使用 MetaCubeX 提供的 `gfw.srs`）；
  没有 fallback/loadbalance 出站时映射为 urltest/selector 并给出警告。
  启动前预下载缺失的 rule-set，避免 core 在启动阶段因网络问题崩溃。
- 编译结果包含 **warnings**：哪些节点被跳过、哪些语义被降级，都会显示在界面上。

### 3. 订阅与节点

- 定时更新；`ETag` / `If-Modified-Since`；自定义 UA 与请求头；`304` 处理。
- 解析：**Base64**、**v2rayN 分享链接**（vmess / vless / trojan / ss / hysteria2 / tuic）、
  **Clash YAML**（含 proxy-groups）、**SIP008**、**sing-box JSON**。
- 统一 `Node` 模型；分享链接**双向转换**（导入/导出）；二维码（终端块字符渲染）；
  指纹去重；订阅分组导入；排序字段。
- **手动添加节点**：协议（VMess / VLESS / Trojan / Shadowsocks / Hysteria2 / TUIC /
  SOCKS / HTTP）、凭据、TLS / Reality（pbk + shortId）、uTLS 指纹、ALPN、
  传输层（TCP / WebSocket / gRPC / HTTP/2 / HTTPUpgrade）、UDP、标签。
  校验规则集中在 `NodeDraft::into_node`，界面、CLI 与测试共用同一套错误文案。
- 解析 `subscription-userinfo` 响应头，展示已用/总量/到期时间，并可视化进度。

### 4. 路由与 DNS

- 规则编辑器：域名 / 后缀 / 关键字 / 正则 / GeoSite / GeoIP / IP-CIDR /
  进程名 / 端口 / 网络 / rule-set / inbound；动作：直连 / 代理 / 阻止。
- 预设：绕过中国大陆、GFWList、全局、仅代理指定应用、自定义。
- 规则集下载与更新：`.srs` 落盘、ETag 增量、SHA-256 记录、本地优先引用。
  手动更新也包含路由和 DNS 隐式引用的规则集，旧内置 GFWList 地址会自动修正。
- DNS：fake-ip、DoH/DoT/DoQ、按域名分流、hosts、dns-hijack、final server、
  独立的 DNS 校验器（重复 tag、非法网段、final 不存在等）。

### 5. 出站与入站

- 出站组：selector / urltest / fallback / loadbalance；手动切换。
- 入站：mixed / socks / http / TUN / tproxy / redirect；端口、allow LAN、UDP、sniff。
- 系统代理：GNOME `gsettings`、KDE `kioslaverc`、macOS `networksetup`、
  Windows 注册表 + `InternetSetOption`；**崩溃后自动还原**（快照文件）。
- PAC：本地 HTTP 托管 + 动态 PAC 生成，可设为系统自动代理 URL。

### 6. 系统集成

- TUN 使用 sing-box 原生实现；启动前做 `/dev/net/tun`、端口、权限、路由冲突检查。
- **托盘浮窗**（Windows / macOS / Linux）：使用 GPUI Kit 绘制独立卡片式 dashboard，
  包含运行状态、实时速率与累计流量、可滚动的组内节点与延迟、系统代理、启停 core、
  TUN 开关、路由预设下拉框、全部测速、设置、显示主窗口与退出；再次点击图标、失焦或按 Esc 收起。
  浮窗使用紧凑字号；路由下拉框与主界面同步，运行中切换会校验并重启 Core，停止时只保存。
  Windows 从任务栏方向滑入/滑出，支持中途反转并遵循系统“动画效果”开关；
  TUN 在 Core 运行时通过校验、重启生效，停止时只保存设置，Xray 下不可启用。
  Windows 使用 `Shell_NotifyIcon`，macOS 使用 AppKit status item，Linux 使用 StatusNotifierItem。
  **最小化到托盘**与**关闭到托盘**可在设置中分别开关；Linux 使用最小化/激活恢复，保留任务栏入口。
- **提权助手**：GUI 永不提权。安装为 Windows 计划任务（绑定当前用户，登录时最高权限）/
  macOS LaunchDaemon / Linux systemd service，通过本地 socket（Unix socket /
  Windows 命名管道）以单行 JSON 执行少量操作：TUN 准备、系统 DNS、Linux tproxy 规则。
  Unix 端校验 peer UID；安装/卸载走系统授权（UAC / osascript / pkexec）。
  Windows GUI 及由它安装的助手在后台运行，不显示命令行窗口；UAC 授权提示保留。
  Windows 安装器维护独立的助手程序副本与 High 完整性标签；GUI 保持普通权限。
  更新程序后重新安装助手以同步其版本。
  未安装时 TUN 仍由 core 自行创建，界面明确提示助手状态。
- 开机自启：Linux `.desktop`、macOS LaunchAgent、Windows Run 键；支持 `--minimized`。
- 单实例锁（文件锁 + PID）。

### 7. 运行时

- sing-box / Clash 兼容 API：`/traffic` 数据来自 `/connections` 累计值差分，
  `/connections` 列表、按进程/域名展示、一键断开、`/proxies` 组状态、
  `/proxies/{name}/delay` 真实延迟。
- Xray：`xray api statsquery`（其内部即 gRPC StatsService）解析累计流量；
  连接列表不可得，界面会明确说明。
- 延迟测试：并发 TCP 握手测速、结果缓存（SQLite）、走代理的真实延迟。
- 日志查看器、速率/累计流量曲线（快照轮询）。
- 连接页支持域名/IP/进程筛选，展开查看发起方、协议、规则、出站链与返回路径；
  一键测试目标，按精确域名/IP/进程添加置顶规则，应用配置后生效。
  sing-box 在 Linux/Windows/macOS 开启进程查找；权限不足或远端连接可能无法归因。
- 会话录制保留最多 2000 条连接元数据和 100 次测试结果，支持停止、回看、清空、JSON 导出；
  不采集正文，退出后清空。连接表按约 1.2 秒采样，极短连接可能遗漏。
- 测试链路图可点击阶段查看完整详情；区分本机 DNS 实测、配置预览、core 关联观测和返回路径推导。
  测试由 Xross 发起，不能复现原进程身份；运行中由 core 判断路由，本机 DNS 失败仍可继续交给 core。
- 日志支持鼠标跨行选择、Ctrl+C 复制，以及复制当前筛选结果。
- 端口诊断识别当前 core 的监听 PID，区分正常使用与其他进程占用。

### 8. 持久化与安全

- SQLite（WAL）+ 版本化迁移；迁移前自动备份数据库。
- Profile 多配置：新建/切换/重命名/删除；导入/导出 JSON 备份。
- 节点凭据与订阅 token 使用 **ChaCha20-Poly1305** 加密后落库
  （随机 32 字节密钥，0600 权限；也支持 `XROSS_SECRET_KEY`）。
- 桌面通知未接入系统通知中心（当前使用应用内 toast，见 [TODO](#todo)）。

### 9. UI 与更新

- 两栏式布局：可折叠左侧菜单（节点/订阅/路由/DNS/连接/日志/设置），
  右侧内容区；深/浅色主题、跟随系统；中英文切换。
- 标题栏左侧展示当前组所选节点的名称、缓存延迟与服务器 IP 归属地；
  归属地通过 HTTPS 向 `ipwho.is` 查询（只发送服务器 IP），异步更新并在内存中缓存，
  查询失败显示“归属地未知”。节点信息区与运行状态右侧空白区均支持拖动窗口。
- 节点列表工具栏、标记与操作菜单统一使用与侧栏一致的线性图标。
- 节点支持通过拖动手柄排序并保存；测速、菜单和启用开关不会触发节点切换。
  标题行的“添加节点”统一提供链接导入和手动填写入口。
- 托盘 dashboard 与主界面共用 AppService 快照，400 ms 更新；原生回调只排队窗口命令，
  服务动作在 GPUI 线程执行。主窗口句柄单独记录，浮窗的创建/销毁不会影响主窗口隐藏恢复。
- 应用/任务栏图标：Windows 由 `build.rs` 从 256px PNG 生成 `.ico` 并以资源 ID 1 内嵌
  （GPUI 的 Windows 后端按该 ID 加载），macOS 在启动时设置 Dock 图标，Linux 使用
  `WindowOptions.icon` 与随附的 `.desktop` 图标。
- **设置 → 通用 → 窗口材质**即时作用于主窗口侧栏和托盘浮窗并持久保存：Windows 可选
  自动 / Mica / Mica Alt / Acrylic / 纯色，macOS 可选自动 / 毛玻璃（vibrancy）/ 纯色，
  Linux 可选自动 / 纯色。自动优先 Windows 11 22H2+ 的 Mica Alt，再回退 Acrylic；
  切换时清理旧 accent policy 并重新扩展 DWM 客户区，Mica 表面保持透明以显示系统材质。
  系统关闭透明效果、原生调用失败或平台不支持时回退纯色。Linux 不假定合成器支持模糊。
- 首次启动向导卡片（三步上手）。
- App / core / geo 数据检查与更新；App 更新提供下载页跳转（不做自替换安装）。
- Core 更新：下载 → SHA-256 → 解压 → 安装 → 切换版本，全程不阻塞界面。

## 快速开始

要求：Rust 1.96+（`rust-toolchain.toml` 已固定）、Linux/Wayland 或 X11 桌面。
Linux 需要 GTK3 相关开发库、`libxkbcommon`、`wayland-client`（或 X11 依赖）。

Core 版本：sing-box **1.12+**（推荐 1.14），Xray **1.8+**。

```bash
cargo run -p xross-app          # 启动 GUI
cargo test -p xross-core        # 核心单元测试
cargo run -p xross-cli -- demo  # 查看编译出的两套配置
```

首次运行：

1. **订阅** 页添加机场订阅，或在 **节点** 页粘贴分享链接 / 点击 **手动** 填写节点。
2. **设置 → Core 管理** 下载 xray 或 sing-box（自动校验 SHA-256）。
3. 右上角 **启动**。修改路由/DNS 后点击 **应用配置**（会重启 core）。

关闭窗口默认最小化到托盘；需要彻底退出请用托盘菜单的 **退出 Xross**，
或在 **设置 → 通用** 关闭“关闭到托盘”（托盘宿主不可用时关闭即退出）。

隔离数据目录：

```bash
XROSS_HOME=/tmp/xross-demo cargo run -p xross-app
```

## CLI

`xross-cli` 是同一套核心的无界面入口，便于调试与脚本化：

```bash
xross-cli parse ./subscription.txt          # 自动识别格式并列出节点
xross-cli import ./sub.yaml                 # 导入当前配置
xross-cli compile --core sing-box -o c.json # 仅编译（不启动）
xross-cli check --core sing-box             # 用真实 core 做 check
xross-cli export                            # 导出分享链接
xross-cli add-node --server hk.example.com --protocol vless \
  --uuid 11111111-2222-3333-4444-555555555555 --sni hk.example.com \
  --transport ws --path /ws                       # 手动添加节点
xross-cli helper status                     # 提权助手状态（JSON）
xross-cli helper install                    # 安装并启动助手（会请求管理员权限）
xross-cli helper start|restart|uninstall    # 启停/卸载已安装的助手
xross-cli doctor                            # TUN/端口/权限检查
```

## 测试

```bash
cargo test -p xross-core
```

覆盖：链接解析/导出往返、Clash/SIP008/sing-box 解析、去重、规则匹配、CIDR、
DNS 校验、加密落库、SQLite 迁移与备份、PAC HTTP 服务、日志轮转、进程退出归因、
单实例锁、订阅合并、Profile 切换、两套 adapter 的结构约束、tar.gz 魔数解压、
手动节点草稿校验（UUID/Reality/传输组合）、提权助手协议与状态标签等。
连接诊断覆盖分类筛选、录制去重/关闭/容量/停止冻结、规则置顶去重与持久化、
测试记录回看取消旧发布者、连接元数据关联和提前失败的阶段归因。

另外做了真实二进制验证：

```
xray 1.8.24:      run -test 通过，StatsService 统计查询正常
sing-box 1.11:    不支持（编译器只生成 1.12+ typed DNS）
sing-box 1.14.4:  check 通过，UI 启动 / Clash API / selector 热切换均验证
```

## 已知限制 / 路线图

- **Linux 托盘**：需要 StatusNotifierItem 宿主（如 KDE Plasma 或 GNOME AppIndicator 扩展）。
  不支持直接 Activate 的宿主通过简短原生菜单打开 GPUI 浮窗。Wayland 不允许客户端保证全局位置，
  浮窗位置由合成器决定；关闭到托盘使用最小化，保留任务栏入口。没有宿主时关闭窗口仍会退出。
- **跨平台验证**：macOS status item/vibrancy 与 Linux 桌面交互仍需相应桌面环境实机验证。
- **Windows 托盘与权限**：`Shell_NotifyIcon` 要求进程完整性级别不低于任务栏。若 exe 位于带
  Low 完整性标签的目录（例如受限构建沙盒），进程会继承 Low IL，向 Medium IL 的任务栏注册
  托盘会得到 `ERROR_ACCESS_DENIED`；应用会退化为“关闭即退出”并记录原因。把程序放到普通
  目录/安装包路径运行即可；若需要就地恢复，可执行
  `icacls <目录> /setintegritylevel (OI)(CI)M`（需要修改 SACL 的权限）后用 `icacls` 确认。
- **移动端**：未实现（无 VPN service / 前台通知 / 快捷磁贴）。
- **提权助手**：安装器已实现（Windows 计划任务 / macOS LaunchDaemon /
  Linux systemd），但 Linux tproxy 规则与 macOS `networksetup` DNS 路径尚未在
  真机上端到端验证；安装/卸载会弹出系统管理员授权。
- **Xray 连接列表**：StatsService 只提供计数，无法列出连接。
- **流量面板**：sing-box 使用 `/connections` 累计值差分；没有连接时图表为空。
- **系统代理**：仅在 GNOME/KDE、macOS、Windows 上生效；其他桌面会明确报错。
- **平台验证**：核心逻辑与 Windows 托盘窗口/菜单/隐藏恢复路径已在本机验证；
  Windows 注册表、`InternetSetOption`、macOS `networksetup`、LaunchDaemon 等代码
  按平台 API 编写，但尚未在真机 CI 上验证。
- **sing-box 版本**：编译器只生成 1.12+ 的 typed DNS；1.11 及更早版本会校验失败。
  这是有意取舍，避免同时维护两套 DNS schema。
- **App 自更新**：只做版本检查与下载页跳转，不做二进制自替换。
- **i18n**：导航、页面标题、主要按钮支持中英双语；部分提示与错误文本仍为中文。
- **WireGuard**：模型已预留协议位，但密钥字段未实现，编译时会给出警告并跳过。
- 系统代理后端在非 GNOME/KDE 的 Linux 桌面上不可用（会明确提示）。

## TODO

### 协议与 core

- [ ] 完善协议支持：AnyTLS 等（模型、手动添加、分享链接导入/导出、两套 adapter 同步补齐，
      不支持时按既有约定降级并给出 warning）。
- [ ] 优化操作连贯性：缺 core 时自动下载 core，缺 rule-set 时自动下载 rules，
      需要提权时自动引导安装 helper，减少“先手动准备好再启动”的断点。

### 系统集成

- [ ] 系统 toast：接入 Windows 通知中心 / macOS UserNotifications / Linux 桌面通知，
      替代当前仅有的应用内 toast。
- [ ] 清理系统副作用：退出或卸载时回收提权计划任务 / systemd service / LaunchDaemon、
      虚拟网卡、系统代理设置与 PAC 托管端口，避免端口被占用或残留规则。
- [ ] 合并提权助手到健康检查界面：把 helper 的状态、安装、启动、卸载收进 `doctor`
      式的检查列表，不再分散在设置页与 CLI。

### 组网

- [ ] **exit node**：选择某个组网节点作为出口。
- [ ] **subnet**：宣告/访问子网路由。
- [ ] **Tailscale / EasyTier 作为节点**：接入出站路由与出站组，而不只是独立开关。
- [ ] **通过组网同步配置**：多设备经组网通道共享同一份 profile。

### UI

- [ ] 提高输入框的对比度（深/浅色主题下均需满足可读性）。
- [ ] 提高边栏透明度，让窗口材质更明显。
- [ ] 优化 macOS / Windows 下的标题栏布局（红绿灯与自绘控件的避让、拖动区域）。
- [ ] 托盘显示 Tailscale / EasyTier 状态（连接状态与图标），与核心状态并列。

### 文案与引导

- [ ] 使用 Gemini Argon 润色文案：统一术语与语气，补齐错误/警告提示的中英双语。
- [ ] 优化 Onboarding 体验：首次启动流程更连贯，按“下载 core → 添加订阅 → 启动”串成一条
      引导路径，缺提权助手/缺 rule-set 时就地给出下一步动作，而不是让用户自己去找设置项。

### 验证

- [ ] macOS / Linux 实机验证：托盘（status item / StatusNotifierItem）、vibrancy 与
      窗口材质、系统代理、提权助手（LaunchDaemon / systemd）、TUN 与组网路径。

## 目录结构

```
crates/
  xross-core/
    src/model/          # AppModel：节点、组、入站、路由、DNS
    src/adapters/       # xray.rs / singbox.rs：编译为各自 schema
    src/core/           # binary / process / logs
    src/subscription/   # 解析、抓取、分享链接、二维码
    src/runtime/        # Clash API、Xray stats、延迟测试
    src/storage/        # SQLite、迁移、加密
    src/system/         # 系统代理、PAC、自启、单实例、特权助手、TUN 检查
    src/service.rs      # UI 唯一入口（AppService / UiSnapshot）
  xross-app/
    src/ui/             # GPUI 页面（节点/订阅/路由/DNS/连接/日志/设置）
    src/tray/           # dashboard.rs + macos.rs / windows.rs / unsupported.rs
  xross-cli/            # 无界面工具（含 add-node / helper）
```

## 许可

GPL-3.0-or-later（见 `LICENSE`）。GPUI / GPUI Kit 为 Apache-2.0。

品牌资源位于 [crates/xross-app/assets](crates/xross-app/assets)：透明 X/玫瑰标识、
圆角应用图标（512/256/128/96/64）与 `io.xross.Xross.desktop`。桌面环境中安装
desktop 文件与 hicolor 图标后，任务栏/启动器会显示对应 logo：

```bash
install -Dm644 crates/xross-app/assets/io.xross.Xross.desktop \
  ~/.local/share/applications/io.xross.Xross.desktop
install -Dm644 crates/xross-app/assets/icons/xross.png \
  ~/.local/share/icons/hicolor/256x256/apps/xross.png
```
