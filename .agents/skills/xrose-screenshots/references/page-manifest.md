# Xrose README Screenshot Manifest

All 13 screenshots referenced in `README.md` live under `docs/screenshots/`.

| Output File | Page / View | Navigation (UIA / Action) | Expected Visual State |
| :--- | :--- | :--- | :--- |
| `nodes.webp` | 节点（代理模式） | Sidebar `Button "节点"` | Proxy mode active, traffic sparkline visible, one proxy node selected, health badges (`A–F`) and latencies visible |
| `node-health.webp` | 五维健康详情与可达性检测 | On `节点` page, click a proxy node card to expand its detail drawer/panel | Five-dimension health bars (`可用性 A` / `延迟 L` / `稳定性 S` / `吞吐 T` / `可达性 R`), 24h latency bar, AI/Streaming/Exit reachability grid |
| `diagnostics.webp` | 六阶段可视化链路诊断 | Sidebar `Button "诊断"` | Completed 6-stage trace (`请求源 → 入站 → DNS 解析 → 路由 / 分流 → 出站节点 → 目标服务`) + Quick Add Rule card |
| `mesh.webp` | 异地组网 | Sidebar `Button "组网"` | EasyTier / Tailscale status, online peers, exit/subnet tags |
| `connections.webp` | 实时连接与活跃进程 | Sidebar `Button "连接"` | Active connections list, process icons, traffic stats |
| `direct.webp` | 本机直连模式 | On `节点` page, select `本机直连` | Direct mode active, local monitoring & ad-block ruleset status |
| `subscriptions.webp` | 订阅 | Sidebar `Button "订阅"` | Subscription cards with node counts & update timestamps |
| `routing.webp` | 路由 | Sidebar `Button "路由"` | Routing rules, preset rulesets, custom rule groups |
| `dns.webp` | DNS | Sidebar `Button "DNS"` | Encrypted DNS servers, routing & fallback settings |
| `logs.webp` | 日志 | Sidebar `Button "日志"` | Structured runtime logs with level filters |
| `settings.webp` | 设置 | Sidebar `Button "设置"` → `TabItem "外观"` | Appearance tab showing Language, Theme (`浅色`), and Window Material (`Mica Alt`) |
| `tray.webp` | 托盘浮窗 | Click Xrose system tray icon to open tray popover; query popover window bounds via `list_windows({pid})` | Compact floating popover (~320dp wide) showing sing-box/EasyTier/Tailscale status, quick node switch, failed connections, TUN toggle |
| `about.webp` | 关于 | Sidebar `Button "关于"` | App logo, version number, core versions, license & links |

## Reference Dimensions (Existing macOS Retina 2x Set)

- **Main window pages (12 files)**: `2520 × 1720` (`2400×1600` window + `60px` [30dp] wallpaper margin on each side)
- **Tray popover (`tray.webp`)**: `832 × 1684` (shown at `width="320"` in `README.md`)
