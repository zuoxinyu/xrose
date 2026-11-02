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
| fake-ip | 不支持 | 原生（1.12+ typed DNS） | Xray 跳过 DNS fakeip server |
| DNS schema | `servers[].address` | 1.12+ typed server + domain_resolver | 只生成新式 DNS |
| 连接列表 | 无 | Clash API | 界面区分 core 能力 |
| 统计 | StatsService(gRPC) | Clash API | Xray 通过 `xray api statsquery` |

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
托盘菜单点击 ──▶ WndProc/NSMenu 回调只入队 TrayAction
                          │
         GPUI tick (200 ms) 取出动作
                          │
        AppService 变更 ◀──┴──▶ TrayCommand::ShowWindow / Quit
                          │
          Win32 ShowWindow / AppKit orderOut
```

- `tray::dashboard::build` 从同一份 `UiSnapshot` 生成状态行、实时速率与节点列表，
  Windows 原生菜单与 macOS `NSMenu` 使用同一套数据来源。
- 关闭窗口走 `on_window_should_close`：`close_to_tray` 且托盘可用时隐藏窗口并返回
  `false`；`minimize_to_tray` 由 tick 检测 `IsIconic` / `isMiniaturized` 后隐藏。
- 退出必须经托盘菜单或 `Cmd/Ctrl+Q`，它会先置 `quitting` 标志，避免被“关闭到托盘”拦截。
- 托盘不可用（Linux 无后端、Windows 低完整性宿主被 shell 拒绝）时退化为关闭即退出，
  绝不留下一个无法恢复的隐藏窗口。

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
