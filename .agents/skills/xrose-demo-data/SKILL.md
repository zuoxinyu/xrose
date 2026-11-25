---
name: xrose-demo-data
description: >-
  Construct, seed, and verify isolated demo data for Xrose across all desktop
  pages and headless CLI checks without touching real user profiles or leaking
  private information. Use when the user asks to create demo/mock data, prepare
  a clean Xrose profile for UI previews or README screenshots, populate
  fictional nodes/subscriptions/rules/health/mesh/runtime states, or build an
  isolated showcase copy.
---

# Xrose Demo Data Construction

Construct realistic, privacy-safe, reproducible demo data for Xrose in an
isolated `XROSE_HOME` directory—covering both persisted configuration/health
state and (when needed) runtime-only UI states for full-page showcases and
`xrose-screenshots`.

## Non-Negotiable Rules

1. **Strict Profile Isolation**: Always isolate state via `XROSE_HOME` or
   `--home <temp_dir>`. Never read, modify, or launch against the user's real
   Xrose config directory.
2. **Zero Privacy Leaks**: Never copy or reference real IPs, real domains,
   real hostnames, usernames, local paths, tokens, subscription URLs,
   `secrets.key`, or decrypted `enc:v1:` payloads. Use only fictional/reserved
   values (see [data-catalog.md](./references/data-catalog.md)).
3. **Isolated Mock Builds**: If runtime-only UI states require a temporary
   source copy with mock data injection:
   - Never commit mock data hooks to the main repository.
   - Set a dedicated `CARGO_TARGET_DIR` for **every** `cargo build`,
     `cargo test`, and `cargo run` invocation on the copy—never reuse the main
     workspace `target/` or installed app bundle.
   - Afterwards, verify the normal workspace build against a fresh empty
     directory: `xrose-cli mesh list --home <empty_dir>` must output `[]`.

## Choose the Right Tier

| Tier | When to Use | What It Populates | Needs Source Copy? |
| :--- | :--- | :--- | :--- |
| **Tier 1: Persisted (`XROSE_HOME` + `xrose-cli`)** | CLI checks, UI layout QA, nodes/health/subscriptions/routing/DNS/settings/saved mesh profiles | SQLite (`xrose.db`) + `config/settings.json` | No |
| **Tier 2A: Live Core in Isolated `XROSE_HOME`** | Testing Direct mode, local proxy running state, real logs/trace | Tier 1 + live `sing-box`/`xray` runtime | No |
| **Tier 2B: Isolated Mock Build** | Deterministic README screenshots or full-app demos needing live sparklines, connections, failed connections, 6-stage trace, online Mesh peers, and active tray popover without real servers | Tier 1 + in-memory `UiSnapshot` / `TrayStatus` fixtures | Yes (with separate `CARGO_TARGET_DIR`) |

---

## Tier 1: Persisted Demo Profile (Default)

### 1. Create a Clean Isolated `XROSE_HOME`

**Windows (PowerShell):**
```powershell
$DEMO_HOME = Join-Path $env:TEMP "xrose-demo"
Remove-Item -Recurse -Force $DEMO_HOME -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $DEMO_HOME | Out-Null
```

**macOS / Linux (bash/zsh):**
```bash
DEMO_HOME="/tmp/xrose-demo"
rm -rf "$DEMO_HOME" && mkdir -p "$DEMO_HOME"
```

### 2. Seed Baseline Data with `xrose-cli`

```bash
cargo run -p xrose-cli -- seed-demo --home "$DEMO_HOME"
```

This populates:
- 7 multi-protocol fictional nodes (`VLESS WS/TLS`, `VLESS Reality gRPC`,
  `VMess WS`, `Trojan WS`, `Shadowsocks`, `Hysteria2`, `TUIC`) under
  `*.example.com`
- 1 subscription (`Demo Airport`) with traffic usage (60 GiB / 200 GiB) and
  30-day expiry
- 1 custom routing rule (`ChatGPT 走美国`)
- Profile name `演示配置` in `WorkMode::Proxy`
- Offline 5-dimension node health samples, hourly latency buckets, and
  AI/Streaming/Exit reachability reports (`seed_demo_health`)

### 3. Layer Optional Persisted Fixtures

Add only what the target scenario needs:

- **Extra nodes / chains / groups / rules**:
  - `xrose-cli add-node ... --home "$DEMO_HOME"`
  - `xrose-cli route import <rules.json> --home "$DEMO_HOME"`
  - `xrose-cli config import <backup.json> --mode merge --home "$DEMO_HOME"`
- **Saved Mesh profiles (EasyTier / Tailscale)**:
  - Write fictional `MeshProfile` JSON (see
    [data-catalog.md](./references/data-catalog.md#mesh-profiles)) and load it:
    ```bash
    cargo run -p xrose-cli -- mesh save <mesh-profile.json> --home "$DEMO_HOME"
    ```
- **Appearance & Privacy settings**:
  - Edit `$DEMO_HOME/config/settings.json` (or toggle in UI) for `language`,
    `theme`, `window_material`, `privacy_mode`, and `core`.

### 4. Validate Headless Before Launching GUI

```bash
cargo run -p xrose-cli -- check --core sing-box --home "$DEMO_HOME"
cargo run -p xrose-cli -- health --home "$DEMO_HOME"
```

Launch GUI against the isolated profile:

```powershell
# Windows (PowerShell)
$env:XROSE_HOME = $DEMO_HOME; cargo run -p xrose-app
```
```bash
# macOS / Linux
XROSE_HOME="$DEMO_HOME" cargo run -p xrose-app
```

---

## Tier 2B: Runtime-Only Showcase Data (Isolated Copy)

Use this only when the task requires realistic in-memory states that cannot be
persisted in `xrose.db` (e.g., full 13-page README screenshot refreshes):
live traffic sparkline, active connections with process metadata, failed
connection captures, completed 6-stage diagnostic trace, online EasyTier /
Tailscale peers with RTT/exit tags, structured runtime logs, and active tray
popover status.

### 1. Create an Isolated Workspace Copy & Target Dir

Create a separate copy/worktree outside the main repo and assign its own
`CARGO_TARGET_DIR` (e.g., `$TEMP/xrose-demo-src` and
`$TEMP/xrose-demo-target`). Never build or run the mock copy with the main
workspace's `target/`.

### 2. Inject Runtime Fixtures

Consult [data-catalog.md](./references/data-catalog.md) for the exact
`UiSnapshot` and `TrayStatus` fields per page (`nodes`, `node-health`,
`diagnostics`, `mesh`, `connections`, `direct`, `subscriptions`, `routing`,
`dns`, `logs`, `settings`, `tray`, `about`) and recommended fictional values.

Keep all injected values coherent across pages:
- Selected proxy node name matches `runtime.running_proxy_node`, active
  `connections` chains, `trace` outbound stage, and `tray` quick-switch label.
- Mesh hostnames and virtual IPs (`10.144.0.x` / `100.64.0.x`) match between
  the `组网` page, `DNS` MagicDNS notice, and `tray` popover.
- Timestamps are relative to `now_unix()` so ages (`刚刚` / `2 分钟前`) look
  fresh regardless of when the build runs.

### 3. Build & Run Strictly with Isolated Env Vars

```powershell
# Windows (PowerShell)
$env:CARGO_TARGET_DIR = Join-Path $env:TEMP "xrose-demo-target"
$env:XROSE_HOME = Join-Path $env:TEMP "xrose-demo"
cargo run --manifest-path "$env:TEMP\xrose-demo-src\Cargo.toml" -p xrose-app
```

```bash
# macOS / Linux
CARGO_TARGET_DIR="/tmp/xrose-demo-target" \
XROSE_HOME="/tmp/xrose-demo" \
cargo run --manifest-path "/tmp/xrose-demo-src/Cargo.toml" -p xrose-app
```

---

## Verification & Cleanup Checklist

1. **Privacy audit**: Confirm zero real IPs, domains, hostnames, usernames,
   local paths, or subscription tokens appear in `$DEMO_HOME` or UI output.
2. **Main workspace integrity**: Run `git status` in the main repo—no mock
   code or stray files should be present.
3. **Normal build sanity check** (mandatory after any Tier 2B mock build):
   ```bash
   # Use a brand-new empty directory, never $DEMO_HOME
   cargo run -p xrose-cli -- mesh list --home <fresh-empty-dir>
   # Expected output: []
   ```
4. **Cleanup**: Remove temporary mock source copies and target directories once
   screenshots or demos are complete. If capturing README assets next, hand off
   to the `xrose-screenshots` skill.
