# Xrose Demo Data Catalog

Reference for all persisted and runtime-only data surfaces across Xrose's 13
desktop views, plus safe fictional naming/addressing conventions.

## Fictional Data Conventions (Privacy Safe)

| Category | Safe Fictional Examples | Never Use |
| :--- | :--- | :--- |
| Proxy server domains | `hk.example.com`, `jp.example.com`, `sg.host.example.com`, `us.example.com`, `de.example.com`, `kr.example.com`, `tw.example.com` | Real provider/VPS domains |
| Public IPs (RFC 5737) | `192.0.2.x`, `198.51.100.x`, `203.0.113.x` | Real WAN/server IPs |
| EasyTier virtual IPs | `10.144.0.1/24` .. `10.144.0.20/24` | User's real LAN/VPN IPs |
| Tailscale virtual IPs | `100.64.0.10` .. `100.64.0.50` | Real tailnet IPs |
| MagicDNS / Tailnet domain | `demo.tailnet.example` | Real tailnet suffix / email |
| Hostnames / Devices | `studio-mac`, `workstation-win`, `home-nas`, `edge-jp-01`, `lab-linux` | Real machine hostnames or user names |
| Credentials / UUIDs | `11111111-2222-3333-4444-555555555555`, `demo-token`, `password123`, `demo-secret` | Real UUIDs, keys, passwords |
| Subscription URL | `https://example.com/api/v1/client/subscribe?token=demo-token` | Real airport/subscription links |

---

## Persisted vs. Runtime-Only Matrix (All 13 Views)

| View / Screenshot | Backing Data (`UiSnapshot` / Storage) | Persisted (`xrose.db` / `settings.json`)? | Populated by `seed-demo`? |
| :--- | :--- | :--- | :--- |
| `nodes` | `model.nodes`, `model.groups`, `model.work_mode`, `latency`, `node_health` | Yes | Yes |
| `nodes` (live sparkline & running badge) | `process.phase`, `runtime.history`, `runtime.up_speed`/`down_speed`, `runtime.running_proxy_node` | No (in-memory) | No |
| `node-health` | `node_health` (`health_probe_samples`, `health_hourly_buckets`, `health_reachability_cache`) | Yes | Yes (`seed_demo_health`) |
| `diagnostics` | `trace: Option<TraceSession>`, `recording: DiagnosticRecording` | No (in-memory) | No |
| `mesh` (saved profiles) | `mesh.profiles` (`mesh_profiles` table, encrypted) | Yes (`xrose-cli mesh save`) | No |
| `mesh` (online peers, RTT, NAT, exit) | `mesh.networks` (`MeshRuntimeState`, `MeshPeer`) | No (IPC from helper) | No |
| `connections` | `runtime.connections`, `runtime.live_traffic`, `failed_connections` | No (in-memory ring/poll) | No |
| `connections` (daily stats) | `traffic_statistics` (`traffic_sessions`, `traffic_entities`, `traffic_daily`) | Yes | No |
| `direct` | `model.work_mode = WorkMode::Direct`, ad-block rulesets, `process.phase` | Mode persisted; running state runtime-only | Mode is Proxy by default |
| `subscriptions` | `subscriptions` table (`Subscription` + `SubscriptionUsage`) | Yes | Yes (1 demo subscription) |
| `routing` | `model.rules`, `model.rule_sets`, `model.groups` | Yes | Yes (1 custom rule + presets) |
| `dns` | `model.dns`, `dns_comparison`, active `mesh` MagicDNS notice | `model.dns` persisted; notice/comparison runtime | Default DNS preset |
| `logs` | `logs: Vec<LogEntry>` (`LogBus` ring buffer) | No (in-memory) | No |
| `settings` | `settings` (`config/settings.json`), `binaries`, `updates` | `settings.json` persisted | Defaults |
| `tray` | `TrayStatus` (`phase`, `indicator`, `easytier`, `tailscale`, `up_speed`, `down_speed`, `unfixed_failed_connections`) + `UiSnapshot` | No (runtime-derived) | No |
| `about` | `app_version`, `platform`, `binaries` | Static / local binaries | Yes |

---

## Persisted JSON Templates

### Mesh Profiles

Save via `xrose-cli mesh save <file.json> --home "$DEMO_HOME"`:

**EasyTier example (`easytier-demo.json`):**
```json
{
  "id": "demo-easytier-01",
  "name": "工作室异地组网",
  "provider": "easytier",
  "hostname": "workstation-win",
  "auto_connect": false,
  "auto_reconnect": true,
  "network_name": "xrose-demo-mesh",
  "network_secret": "demo-network-secret",
  "peers": ["tcp://public.easytier.example.com:11010"],
  "ipv4": "10.144.0.10/24",
  "remote_subnets": ["192.168.50.0/24"],
  "control_url": "",
  "exit_node": "",
  "advertise_exit_node": false,
  "exit_routing": "direct",
  "advertised_subnets": [],
  "port_forwards": [],
  "service_forwards": [],
  "outbound_port": 0,
  "outbound_secret": ""
}
```

**Tailscale example (`tailscale-demo.json`):**
```json
{
  "id": "demo-tailscale-01",
  "name": "家庭节点 Tailnet",
  "provider": "tailscale",
  "hostname": "workstation-win",
  "auto_connect": false,
  "auto_reconnect": true,
  "network_name": "",
  "network_secret": "",
  "peers": [],
  "ipv4": "",
  "remote_subnets": [],
  "control_url": "",
  "exit_node": "",
  "advertise_exit_node": false,
  "exit_routing": "direct",
  "advertised_subnets": [],
  "port_forwards": [],
  "service_forwards": [],
  "outbound_port": 0,
  "outbound_secret": ""
}
```

---

## Runtime Fixture Guidelines (Tier 2B Mock Build)

When injecting in-memory fixtures in an isolated copy of `crates/xrose-core/src/service.rs`:

1. **`process` & `runtime` (`nodes`, `direct`, `tray`)**:
   - `ProcessState { phase: ProcessPhase::Running, core: Some(CoreKind::SingBox), .. }`
   - `runtime.running_proxy_node`: set to the first node's ID (e.g., `香港 01`).
   - `runtime.history`: 30–60 `TrafficSample` points with smooth sinusoidal + slight jitter upload (100–600 KB/s) and download (1.2–8.5 MB/s) curves so the sparkline renders cleanly.
   - `runtime.up_total` / `down_total`: e.g., `1.42 GiB` / `18.6 GiB`.
2. **`connections` & `failed_connections`**:
   - 8–12 `ClashConnection` entries spanning common apps (`Chrome`, `VS Code`, `Cargo`, `Spotify`, `Slack`, `curl`) with realistic mix of `proxy` (`香港 01` / `东京 Reality`), `direct`, and `block` rules, targeting public/fictional domains (`github.com`, `api.openai.com`, `cdn.example.net`, `updates.example.org`).
   - 2–3 `FailedConnectionRecord` entries in `failed_connections` (e.g., `Timeout` on `api.example.org:443`, `DnsFailed` on `internal.corp.example:443`, `Rejected` on `ads.tracker.example:443`) so both the Connections page and Tray quick-fix section are populated.
3. **`trace` (`diagnostics`)**:
   - Completed 6-stage `TraceSession` for `https://api.openai.com/v1/models` showing all stages succeeded (`请求源 → 入站 → DNS 解析 → 路由 / 分流 → 出站节点 → 目标服务`) with realistic stage latencies (e.g., DNS 14ms, TLS/Connect 82ms, Total 146ms) and matched rule `ChatGPT 走美国`.
4. **`mesh.networks` (`mesh`, `dns`, `tray`)**:
   - 1 connected `EasyTier` network (`10.144.0.10`) with 3–4 peers (`studio-mac` P2P 12ms, `home-nas` P2P 18ms with subnet `192.168.50.0/24`, `edge-jp-01` Relay 46ms with `exit_capable: true`).
   - 1 connected `Tailscale` network (`100.64.0.12`, MagicDNS `demo.tailnet.example`) with 2–3 online peers.
5. **`logs`**:
   - 15–25 structured `LogEntry` items across `App`, `Core`, and `Mesh` streams with `Info`, `Warn`, and occasional `Error` levels (core started, TUN ready, rulesets loaded, proxy connection matched, mesh peer P2P established).
