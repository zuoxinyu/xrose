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

## 安全边界

- 桌面 UI 以普通用户运行；TUN 路由由 sing-box 或特权 helper 完成。
- helper 协议为单行 JSON，仅暴露 TUN/DNS/透明代理规则等必要操作。
- 密钥文件 0600；数据库字段以 `enc:v1:` 前缀区分。
- 代理快照用于崩溃恢复：启动时发现残留即还原，避免用户断网。
