# Xross 架构与编译约定

## 为什么是 AppModel

若直接以某个 core 的 JSON 建模，第二个 core 会到处漏。Xross 的 `AppModel`
只描述用户意图：

- `nodes`：协议 + 凭据 + TLS/传输，不含 core 字段名；
- `groups`：selector / urltest / fallback / loadbalance；
- `routing.rules`：语义 matcher（geosites / geoips / rule_sets / process ...）；
- `dns`：服务器（tag/address/domains/detour）、fake-ip、hosts、final；
- `inbounds`：mixed/socks/http/tun/tproxy/redirect。

`adapters::CoreAdapter::compile` 是唯一可以出现 `outbounds`、`protocol`、
`route` 这类字段名的地方。

## 关键差异与处理

| 能力 | Xray | sing-box | Xross 处理 |
| --- | --- | --- | --- |
| mixed 入站 | 无 | 有 | Xray 拆成 SOCKS + HTTP(port+1)，界面提示 |
| TUN | 无（依赖 tun2socks） | 原生 | Xray 编译时跳过并警告 |
| selector 热切换 | 无 | Clash API | Xray 选择节点 = 改配置 + 重启 |
| urltest | balancer + observatory | 原生 urltest | 分别生成 |
| fallback / loadbalance | leastPing / random | 无 | sing-box 映射为 urltest/selector + 警告 |
| block | blackhole 出站 | `action: reject` | 不生成 legacy block 出站 |
| geoip/geosite | 原生字段 | 1.8 起移除 | 自动生成 `.srs` rule-set 引用 |
| GFWList 来源 | `geosite:gfw`（需对应资产） | `geosite-gfw` rule-set | 使用 MetaCubeX `gfw.srs`；编译、预下载、手动更新共用来源解析 |
| fake-ip | 不支持 | 原生（1.12+ typed DNS） | Xray 跳过 DNS fakeip server |
| DNS schema | `servers[].address` | 1.12+ typed server + domain_resolver | 只生成新式 DNS |
| 连接列表 | 无 | Clash API | 界面区分 core 能力 |
| 连接进程归因 | 无连接列表 | `route.find_process` | Linux/Windows/macOS 开启进程查找；权限或远端请求导致缺失时明确提示 |
| 统计 | StatsService(gRPC) | Clash API | Xray 通过 `xray api statsquery` |
| 组网访问 | 优先 freedom 规则 + 专用 DNS | 绑定组网接口直连 + TUN 排除 + 专用 DNS | `CompileContext.mesh` 注入，不改变 AppModel；MagicDNS 优先于 fake-IP |
| Windows 组网与 TUN 共存 | 无内建 TUN | 暂时关闭 `strict_route` | 明确提示 DNS 防泄漏过滤放宽，全部组网断开后恢复 |

组网独立于 `CoreKind`：`mesh::MeshProfile` 表达网络身份、入口和远端网段意图，
`MeshController` 通过带版本和调用者鉴权的助手 IPC 管理会话。EasyTier 2.6.4
静态库只在 Xross `--mesh-worker` 初始化；Tailscale 使用固定 sing-box 1.14.4
子进程及鉴权 gRPC。UI 只调用 AppService、读取 `UiSnapshot.mesh`。
SQLite schema v3 的 `mesh_profiles` 全量加密，不与代理 profile 混写。

`CoreManager` 独立保存最后已应用的模型、设置和编译结果。组网变化串行执行
编译、校验、必要的重启及进程检查，失败回滚已保存配置，保留用户待应用编辑。
Tailscale 使用用户态 endpoint 和仅包含批准路由的专用 TUN，避免 `accept_routes`
把控制服务器路由直接安装到系统；仅为当前 tailnet 后缀设置带归属记录的 NRPT。
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
内置 LAN/中国 IP 绕过仍优先，不自动重启 core。

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

## 启动顺序

1. 同步设置到模型（端口、LAN、TUN）。
2. sing-box：收集并预下载缺失的 `.srs` rule-set。
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
  `apply_config` 校验/重启流程；停止时不启动 Core。Xray 不可启用，Core 忙时不接受切换。
- 主窗口原生句柄在创建时记录，Windows 不再枚举同类窗口，macOS 不再对所有窗口隐藏/恢复。
  `close_to_tray` / `minimize_to_tray` 在 Windows/macOS 隐藏主窗口；Linux 使用 GPUI 的
  minimize/activate，保留任务栏入口，避免依赖 Wayland 不提供的隐藏/全局定位接口。
- Linux D-Bus 注册与更新在专用线程执行，通过有界通道接收快照；只有注册成功且宿主在线时
  `available()` 为 true。没有托盘宿主或 Windows shell 拒绝注册时，关闭窗口直接退出。
- `Settings.backdrop` 只描述外观偏好，不影响代理模型或 `config_dirty`。UI 按平台提供材质选项；
  主窗口/浮窗即时应用，主题切换后重新应用。Windows 切换材质时同时清理 accent policy 与旧 DWM
  材质，扩展客户区并回读 DWM 属性确认请求成功；Mica 根表面不再覆盖额外的 tint。
  不支持的材质退化为不透明面板。Linux 当前使用不透明背景。

开发验证可在隔离的 `XROSS_HOME` 下设置 `XROSS_TRAY_PREVIEW=1`，启动后自动打开浮窗；
debug 构建的主窗口在控件聚焦时支持 Ctrl/Cmd+Shift+T 再次打开，用于检查隐藏后的复用路径。

## 安全边界

- 桌面 UI 以普通用户运行；TUN 路由由 sing-box 或特权 helper 完成。
- helper 协议为单行 JSON，只暴露 TUN 准备、系统 DNS、透明代理规则等必要操作。
- helper 由系统服务/计划任务以管理员身份运行；GUI 通过本地 socket 调用。
  Unix 端校验 `SO_PEERCRED`/`LOCAL_PEERCRED` 的 euid（安装用户或 root），
  Windows 依赖命名管道的默认 DACL（创建者用户 + Administrators）。
- 安装/卸载/启动通过 UAC（`ShellExecuteEx runas` 等待子进程）、
  `osascript ... with administrator privileges`、`pkexec` 完成，不提供 setuid 包装。
- `--helper` 在 `xross-app`/`xross-cli` 的 `main` 最前面处理，早于数据库和 GUI；
  helper 进程不会打开用户配置库。
- 密钥文件 0600；数据库字段以 `enc:v1:` 前缀区分。
- 代理快照用于崩溃恢复：启动时发现残留即还原，避免用户断网。
