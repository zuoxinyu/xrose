---
name: xrose-screenshots
description: >-
  Cross-platform workflow (Windows, macOS, Linux) to capture and update README
  screenshots for all Xrose desktop pages as WebP images in docs/screenshots/
  using cua-driver. Use when the user asks to screenshot Xrose pages, refresh
  README screenshots, or export docs/screenshots/*.webp with active
  MicaAlt/vibrancy and desktop wallpaper framing.
---

# Xrose README Screenshots (Cross-Platform `cua-driver` → WebP)

Capture consistent, publication-ready screenshots of every Xrose page on
**Windows, macOS, or Linux** and save them as `.webp` files in
`docs/screenshots/` for `README.md` / `README_EN.md`.

## Prerequisites

- **`cua-driver` MCP server** active (`health_report` passes).
  - **Windows**: UIAutomation + D3D11 work out of the box.
  - **macOS**: Accessibility + Screen Recording granted (`cua-driver permissions grant`).
  - **Linux**: X11 or Wayland capture ready (on KDE Plasma Wayland, run `cua-driver kwin-capture on` if needed).
- **`ffmpeg`** with `libwebp` and **`ffprobe`** in `PATH`.
- **Python 3** (standard library only, no third-party packages required).
- **Xrose** running (`xrose-app.exe` on Windows, `Xrose.app` on macOS, `xrose-app` on Linux).

## Visual & Framing Standards

Match the existing assets in `docs/screenshots/`:

- **Theme & Material**: Light mode (`浅色`) with active translucent material:
  - **Windows**: `Mica Alt`
  - **macOS**: native vibrancy (default translucent material)
  - **Linux**: Wayland blur / vibrancy (when available)
- **Window State**: Foreground / active so MicaAlt / vibrancy and active window
  shadow render properly.
- **Outer Background**: Clean desktop wallpaper around all 4 edges (no other
  windows peeking behind Xrose).
- **Margin**:
  - `--padding 30 --padding-unit px` → 30 physical pixels.
  - `--padding 30 --padding-unit dp` → 30 logical points/dp (60 physical px at
    2x HiDPI/Retina, matching the existing `2520×1720` macOS screenshots).
  - Confirm with the user if not specified; default is `30` physical px.
- **Privacy**: Before capturing real profiles, confirm whether **隐私模式**
  (Privacy Mode) should be enabled or a demo `XROSE_HOME` profile is loaded so
  real server IPs/domains are not leaked into `README.md`.
- **No Stray Tooltips**: Park the cursor outside the Xrose window (e.g., `(0, 0)`
  in desktop coordinates) before each capture so hover tooltips do not appear.

## Workflow

### 1. Locate Xrose & Prepare Appearance (All Platforms)

1. Find Xrose via `list_windows({app: "xrose"})` and record `pid` and `window_id`.
2. Call `get_window_state({pid, window_id, include_screenshot: false})` to get
   fresh accessibility `element_token` handles.
3. Configure appearance via background accessibility actions:
   - Click `Button "设置"`.
   - Re-read `get_window_state({pid, window_id, include_screenshot: false})`.
   - Select `RadioButton "浅色"`.
   - On Windows, select `RadioButton "Mica Alt"`; on macOS/Linux, ensure the
     translucent vibrancy option is selected if shown.
   - If privacy masking is desired and not yet enabled, toggle `Button "隐私模式"`.

### 2. Expose Desktop Wallpaper & Keep Xrose Active (Platform-Specific)

Do **not** call `bring_to_front` on macOS or Linux (it is Windows-only).

| Platform | 1. Hide Background Windows | 2. Bring Xrose to Foreground (Active) |
| :--- | :--- | :--- |
| **Windows** | `hotkey({keys: ["win", "m"], target: {kind: "desktop", display_id: "primary"}})` | `bring_to_front({pid, window_id})` |
| **macOS** | First activate Xrose, then `hotkey({keys: ["cmd", "option", "h"], target: {kind: "desktop", display_id: "primary"}})` (Hide Others) | `launch_app({bundle_id: "io.xrose.Xrose"})` (or `open -a Xrose`) |
| **Linux** | Minimize/hide other windows via desktop shortcut or WM (`wmctrl -k on` on X11) | `launch_app` or WM activate (`wmctrl -i -a <window_id>` on X11) |

Then park the cursor outside Xrose (`move_cursor` to `(0, 0)` in desktop scope)
to avoid hover tooltips.

### 3. Capture Each Page & Convert to WebP

See [page-manifest.md](./references/page-manifest.md) for all 13 screenshots
(`nodes.webp`, `node-health.webp`, `diagnostics.webp`, `mesh.webp`,
`connections.webp`, `direct.webp`, `subscriptions.webp`, `routing.webp`,
`dns.webp`, `logs.webp`, `settings.webp`, `tray.webp`, `about.webp`) and how to
reach each view.

For each page:

1. Switch to the target page via `element_token` (or open the expanded view such
   as node health detail / tray popover).
2. Ensure Xrose is still foreground/active using the platform method in Step 2.
3. Capture the full display at native resolution to a temporary PNG:
   ```text
   get_desktop_state({
     max_image_dimension: 0,
     screenshot_out_file: "<temp_dir>/<page>.png"
   })
   ```
4. Crop around Xrose's live bounds and encode to WebP with
   [crop_to_webp.py](./scripts/crop_to_webp.py):
   ```bash
   python3 .agents/skills/xrose-screenshots/scripts/crop_to_webp.py \
     --input "<temp_dir>/<page>.png" \
     --output "docs/screenshots/<page>.webp" \
     --pid <PID> \
     --window-id <WINDOW_ID> \
     --padding 30 \
     --padding-unit px \
     --scale auto \
     --quality 90
   ```
   *(On Windows, use `python` if `python3` is not aliased. `--scale auto`
   automatically converts macOS Retina logical bounds to physical pixels while
   keeping Windows physical bounds 1:1. Use `--rect x,y,w,h` if cropping a
   custom region or tray popover.)*

### 4. Verify Results

1. Check dimensions and file sizes printed by `crop_to_webp.py`:
   - All 12 main-window screenshots should have identical dimensions.
   - Typical WebP size at `--quality 90` is ~200–900 KB per main window page.
2. Visually inspect key outputs (at minimum `nodes.webp`, `node-health.webp`,
   `diagnostics.webp`, `tray.webp`) with `read_file` to confirm:
   - Light mode + active MicaAlt / vibrancy effect.
   - Clean, symmetrical desktop wallpaper margin on all 4 sides.
   - No stray hover tooltips, cursor overlays, or leaked private addresses.
3. Remove temporary PNGs and verify `git status` shows only intended files under
   `docs/screenshots/`.
