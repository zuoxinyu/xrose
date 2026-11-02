# AGENTS.md

Cross-platform desktop client for **Xray** and **sing-box**, written in Rust with GPUI Kit.
Rust workspace (3 crates), GPL-3.0-or-later. Product docs are Chinese; user-facing strings are
Chinese with English available through `tr(lang, zh, en)`.

## Commands

| Purpose | Command |
| --- | --- |
| Build everything | `cargo build --workspace` |
| Run the GUI | `cargo run -p xross-app` |
| Unit tests | `cargo test -p xross-core` |
| One test | `cargo test -p xross-core <substring>` |
| Compile both configs, no GUI | `cargo run -p xross-cli -- demo` |
| Populate a throwaway profile | `cargo run -p xross-cli -- seed-demo --home <dir>` |
| Compile the saved profile | `cargo run -p xross-cli -- compile --core sing-box -o c.json --home <dir>` |
| Validate with a real core | `cargo run -p xross-cli -- check --core sing-box --home <dir>` |
| Add a manual node | `cargo run -p xross-cli -- add-node --server h --protocol vless --uuid <uuid>` |
| Privilege helper | `cargo run -p xross-cli -- helper status` / `install` / `start` / `uninstall` |

`compile` on an empty profile fails with `没有可编译的出站节点` — run `seed-demo` first, or use
`demo`, which builds both configs from a built-in sample without touching any state.

Isolate all state with `XROSS_HOME` so you never touch the user's real profile:

- PowerShell: `$env:XROSS_HOME = "$env:TEMP\xross-demo"; cargo run -p xross-app`
- bash: `XROSS_HOME=/tmp/xross-demo cargo run -p xross-app`

`xross-cli` also has `parse`, `import`, `export`, `add-node`, `doctor`, `core`, `fetch-core`,
`run`, `helper`, `mesh` and `trace` subcommands — prefer it over the GUI when verifying core logic.

Formatting: `cargo fmt --all --check` currently reports pre-existing diffs, so format only the
files you changed (`rustfmt <file>`). `cargo clippy` is available but not enforced.

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `crates/xross-core` | Everything that is not UI: model, adapters, core binaries/process, subscriptions, mesh, storage, runtime APIs, system integration, `service.rs` facade |
| `crates/xross-app` | GPUI Kit UI (`ui/`), tray dashboard (`tray/`: macOS `NSMenu`, Windows `Shell_NotifyIcon`), assets |
| `crates/xross-cli` | Headless entry points for the same core |

`xross-core/src/`: `model/` (intent), `adapters/` (compile to core JSON), `core/` (binaries,
process, logs), `subscription/`, `mesh/` (EasyTier/Tailscale, coexistence plan), `storage/`
(SQLite + crypto), `runtime/` (Clash API, Xray stats), `system/` (proxy, PAC, autostart, TUN,
single instance, privilege IPC), plus `route.rs` (local rule preview), `dns.rs`
(presets/validation), `trace.rs` (link tracer), `updates.rs`.

## Architecture invariants

1. **`AppModel` is core-agnostic.** It describes user intent (nodes, groups, inbounds, routing,
   DNS) and must never contain `outbounds`, `route`, `inbound.protocol` or other wire-format
   names. Those live only in `adapters/xray.rs` and `adapters/singbox.rs`. If a change needs a
   core-specific concept, add a `CoreKind::capabilities()` flag and resolve it inside the adapter.
2. **Unsupported capability = degrade + warn**, not fail: push a Chinese message into
   `CompiledConfig::warnings` (the UI displays them). Return `Err(Error::config(..))` only when
   the input genuinely cannot be compiled (e.g. a mixed-inbound port that cannot be split).
3. **`AppService` is the only surface the UI touches.** Methods are synchronous and dispatch to
   an internal Tokio runtime; results reach the view through `snapshot()` → `UiSnapshot`. Slow
   work gets a `busy` key and a toast; never block `render`/`tick`.
4. **The view polls.** `main.rs` runs a 200 ms timer calling `AppView::tick`, which refreshes the
   snapshot every 400 ms; an active trace short-circuits that loop to animate. Don't add network
   or disk work to render paths.
5. **No hot reload.** Applying the model rewrites `<runtime>/<core>-config.json`, runs
   `xray run -test` / `sing-box check`, then restarts the process. Only sing-box selector
   switching goes through the Clash API. Mutations persist to SQLite and set `config_dirty`.
6. **sing-box output targets 1.12+** (typed DNS, `route.default_domain_resolver`, `action:
   reject`, `.srs` rule sets). Do not reintroduce legacy `geoip`/`geosite` fields or pre-1.12
   DNS schema.

## Code conventions

- User-visible text is Chinese. In the UI supply both languages at the call site:
  `tr(lang, zh, en)` / `self.tr("停止", "Stop")` — no central translation table.
- **Copy and tone (UI, CLI, README, docs):**
  - Keep all copy concise, formal, and objective. State what a control does or what the current
    state is; never lecture the user or editorialize (no "最容易出错的一层", "这是有意取舍").
  - Avoid AI-style rhetorical contrasts (`不是…而是…`, `不是事后补丁，而是一等公民`) and
    self-justifying disclaimers (`与订阅并行：…凭据不会上传`, `不调用外部程序`).
  - Do not leak internal implementation details, crypto algorithms, or schema history into UI
    subtitles or button labels (write `保存` / `保存配置` instead of `加密保存`; describe
    user-facing behavior instead of `ChaCha20-Poly1305` or `1.12+ predefined action`).
  - Avoid colloquialisms, slang, and anthropomorphic phrasing (use `节点订阅` instead of
    `机场订阅`, `崩溃自动重启` instead of `watchdog 自动拉起`, `发起方` / `DNS 解析` instead of
    `谁发起` / `谁解析`, `HTTP 测速` instead of `测真连`).
  - Use unified terminology across UI and docs: `提权助手` (not `特权助手`), `内核` / `Core`,
    `出站组`, `规则集`, `异地组网` (`组网` in navigation).
- Module-level `//!` docs and inline comments in English; error and warning strings in Chinese.
- Errors go through `crate::error::Error` + `Result`; `to_string()` is rendered to the user
  verbatim, so make messages actionable.
- Serialized enums use `#[serde(rename_all = "snake_case")]`; config structs keep
  `#[serde(default)]` so older files still load.
- Flex rows with variable-length text: give the growing child `.min_w(px(0.))` plus
  `.truncate()` / `.line_clamp(n)`, and `.flex_shrink_0()` on fixed-width controls or badges.
  Without that, long log lines, rule summaries and node names push past the card at 150% DPI.
  Prefer wrapping whole toolbars (`flex_wrap`) over a single row of buttons.
- Dependencies are declared once under `[workspace.dependencies]` and pulled in with
  `.workspace = true`; platform-only crates sit behind `[target.'cfg(...)'.dependencies]`.
- Commits follow conventional style with a lowercase scope, e.g. `feat(ui,macos): …`;
  one concern per commit.

## Testing

- Tests are inline `#[cfg(test)] mod tests` blocks at the bottom of the file under test. There is
  no `tests/` directory and no mocking framework: parsers and adapters are asserted against
  string/JSON literals.
- Service-level tests create a throwaway store via `AppPaths::from_root(tempfile::tempdir())` and
  `AppService::with_paths(...)`.
- Tests must not need network access, a real `xray`/`sing-box` binary, root, or a display.
- When changing an adapter, extend the structural assertions in that adapter's test module rather
  than snapshotting an entire config.

## Known state and gotchas

- `system::single_instance::tests::second_acquire_fails` **fails on Windows**: the lock itself
  works, but `existing_pid()` cannot read the file while the lock is held (Windows byte-range
  locks are mandatory, so the read-back is a sharing violation). Use
  `cargo test -p xross-core -- --skip second_acquire_fails`; 157 tests pass.
- If the workspace tree carries a **Low mandatory integrity label** (the sandbox applied one
  earlier in this session), a process launched from `target\debug\xross-app.exe` inherits that
  Low IL. Low IL cannot call `Shell_NotifyIconW` against the Medium IL shell
  (`ERROR_ACCESS_DENIED`, and `%TEMP%` writes fail too). Restore the label with
  `icacls C:\Users\iamzu\xross /setintegritylevel (OI)(CI)M` and verify with `icacls`; inherited
  labels on children update automatically. The tree is currently Medium again. While it was Low,
  tests needed `$env:TEMP`/`$env:TMP` pointed at the workspace; that workaround is no longer
  needed. The app still degrades to “close quits” and logs the reason if the tray is refused —
  do not force `available()` to `true` in production code.
- The tray is implemented for **macOS and Windows**; `tray/unsupported.rs` makes
  `available()` false elsewhere, so closing the window still quits.
- Windows cores are adopted into a `KILL_ON_JOB_CLOSE` job object
  (`crates/xross-core/src/core/job.rs`), so a crashed or force-killed GUI cannot leave a core
  running. A stray `sing-box.exe` keeps `data/cache.db` locked and every later start fails with
  `initialize cache-file: timeout`; kill the leftover process once if an older build left one
  behind.
- Windows main window class is `Zed::Window` (GPUI), not `GPUIWindow`; `tray/windows.rs`
  locates the main HWND by that class name. The tray message window is `XrossTrayWindow`
  on the same GPUI main thread, so its messages are dispatched by GPUI's message loop.
  `Shell_NotifyIcon` also needs the app to run at the same or higher integrity than the shell.
- GPUI's **Windows backend ignores `WindowOptions.icon`**: it loads icon resource **ID 1** from
  the executable for the window class. `crates/xross-app/build.rs` generates `xross-app.ico`
  from `assets/app-icon-256.png` and embeds it as ID 1 — keep that ID. macOS Dock icon is set at
  runtime in `app_icon.rs` (`NSApplication.setApplicationIconImage`); Linux gets the image via
  `WindowOptions.icon` plus the shipped `io.xross.Xross.desktop` (`Icon=xross`).
- `rust-toolchain.toml` is deleted in the current working tree even though the README still
  mentions it. `Cargo.toml` declares `rust-version = "1.92"`; the installed toolchain is stable
  1.99.
- Baseline `cargo build --workspace` emits two warnings (`unused import PathBuf` in
  `system/autostart.rs`, `fn run is never used` in `system/proxy.rs`). Treat new warnings as
  regressions; leave those two alone unless the task is about them.
- `crates/xross-app/build.rs` raises the **Windows main-thread stack to 16 MiB**. GPUI lays a page
  out as one deep `Div` tree; in debug builds the settings page alone gets close to the 1 MiB
  Windows default and aborts with a stack overflow once a core version row is added. Keep the
  linker arg when touching the build; `XROSS_PAGE=settings` reproduces the old crash quickly.
- The window backdrop (macOS vibrancy / Windows acrylic) only shows through where the UI is
  transparent: `window_effects::make_surface_translucent` keeps `theme.tokens.background` clear,
  and GPUI Kit's root plugin would otherwise paint it over the whole window. Anything that calls
  `Theme::change` / `Theme::sync_system_appearance` must re-run that helper (see `ui/settings.rs`).
- Platform code (Windows registry + `InternetSetOption`, macOS `networksetup`, GNOME/KDE) is
  compiled but was originally validated only on Linux. Run `xross-cli doctor` / `check` before
  claiming a platform fix works.
- Never log, copy or commit node credentials, subscription tokens, `secrets.key` or decrypted
  `enc:v1:` values. Test fixtures use fake IDs and passwords.
- `README.md` (features, limitations, test list) and `docs/ARCHITECTURE.md` (Xray vs sing-box
  difference table) describe current behavior — update them when a capability mapping changes.

## Where to change what

| Task | Files |
| --- | --- |
| Protocol/credential fields | `crates/xross-core/src/model/node.rs` |
| Share links in/out | `subscription/links.rs`, `subscription/links_export.rs` |
| Subscription format detection | `subscription/parse.rs` (Base64, v2rayN links, Clash YAML, SIP008, sing-box JSON) |
| Emitted config | `adapters/{xray,singbox}.rs` + the table in `docs/ARCHITECTURE.md` |
| Binary download/verify/lifecycle | `core/binary.rs`, `core/process.rs`, `updates.rs` |
| Persistence/schema | `storage/mod.rs` (bump `SCHEMA_VERSION`, add a migration) |
| Proxy/PAC/autostart/TUN | `system/*` |
| Mesh networking | `crates/xross-core/src/mesh/`, `ui/mesh.rs`, `docs/MESH.md` |
| Rule preview, tracer | `route.rs`, `trace.rs` |
| Manual node form | `model/node.rs` (`NodeDraft`), `ui/manual_node.rs`, `ui/nodes.rs` |
| Tray dashboard / window hide-show | `crates/xross-app/src/tray/` (`dashboard.rs` is shared; `macos.rs` / `windows.rs` are backends) |
| Privilege helper install/IPC | `system/privilege.rs`, `xross-app/src/main.rs` (`helper_main` runs first), `xross-cli/src/main.rs` |
| Window backdrop, sidebar material | `crates/xross-app/src/window_effects.rs`, `system/window_effects.rs`, `ui/theme.rs` (`Palette::sidebar`) |
| UI page | new file in `crates/xross-app/src/ui/`, then `Page` variant + `icon()` + `zh()`/`en()` + `page_content()` arm in `ui/mod.rs` |
| CLI subcommand | `crates/xross-cli/src/main.rs` |
