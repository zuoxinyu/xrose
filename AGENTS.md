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

`compile` on an empty profile fails with `没有可编译的出站节点` — run `seed-demo` first, or use
`demo`, which builds both configs from a built-in sample without touching any state.

Isolate all state with `XROSS_HOME` so you never touch the user's real profile:

- PowerShell: `$env:XROSS_HOME = "$env:TEMP\xross-demo"; cargo run -p xross-app`
- bash: `XROSS_HOME=/tmp/xross-demo cargo run -p xross-app`

`xross-cli` also has `parse`, `import`, `export`, `doctor`, `core`, `fetch-core`, `run` and
`trace` subcommands — prefer it over the GUI when verifying core logic.

Formatting: `cargo fmt --all --check` currently reports pre-existing diffs, so format only the
files you changed (`rustfmt <file>`). `cargo clippy` is available but not enforced.

## Workspace layout

| Crate | Responsibility |
| --- | --- |
| `crates/xross-core` | Everything that is not UI: model, adapters, core binaries/process, subscriptions, storage, runtime APIs, system integration, `service.rs` facade |
| `crates/xross-app` | GPUI Kit UI (`ui/`), macOS tray (`tray.rs`), assets |
| `crates/xross-cli` | Headless entry points for the same core |

`xross-core/src/`: `model/` (intent), `adapters/` (compile to core JSON), `core/` (binaries,
process, logs), `subscription/`, `storage/` (SQLite + crypto), `runtime/` (Clash API, Xray stats),
`system/` (proxy, PAC, autostart, TUN, single instance, privilege IPC), plus `route.rs`
(local rule preview), `dns.rs` (presets/validation), `trace.rs` (link tracer), `updates.rs`.

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
- Module-level `//!` docs and inline comments in English; error and warning strings in Chinese.
- Errors go through `crate::error::Error` + `Result`; `to_string()` is rendered to the user
  verbatim, so make messages actionable.
- Serialized enums use `#[serde(rename_all = "snake_case")]`; config structs keep
  `#[serde(default)]` so older files still load.
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
  `cargo test -p xross-core -- --skip second_acquire_fails`; 87 tests pass.
- `rust-toolchain.toml` is deleted in the current working tree even though the README still
  mentions it. `Cargo.toml` declares `rust-version = "1.92"`; the installed toolchain is stable
  1.99.
- Baseline `cargo build --workspace` emits two warnings (`unused import PathBuf` in
  `system/autostart.rs`, `fn run is never used` in `system/proxy.rs`). Treat new warnings as
  regressions; leave those two alone unless the task is about them.
- The tray is macOS-only; `tray.rs` compiles to a no-op stub elsewhere.
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
| Rule preview, tracer | `route.rs`, `trace.rs` |
| Window backdrop, sidebar material | `crates/xross-app/src/window_effects.rs`, `system/window_effects.rs`, `ui/theme.rs` (`Palette::sidebar`) |
| UI page | new file in `crates/xross-app/src/ui/`, then `Page` variant + `icon()` + `zh()`/`en()` + `page_content()` arm in `ui/mod.rs` |
| CLI subcommand | `crates/xross-cli/src/main.rs` |
