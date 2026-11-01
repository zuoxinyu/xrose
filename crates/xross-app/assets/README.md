# Xross brand assets

- `logo-mark.png` — transparent X + rose mark, 900×900
- `app-icon.png` / `-256` / `-128` / `-96` / `-64` — rounded navy app icon (small sizes use a flatter, higher-contrast variant)
- `icons/xross.png` — icon for the hicolor theme
- `io.xross.Xross.desktop` — desktop entry matching the Wayland `app_id`

Install the desktop entry and icon for correct taskbar/launcher branding:

```bash
install -Dm644 crates/xross-app/assets/io.xross.Xross.desktop \
  ~/.local/share/applications/io.xross.Xross.desktop
install -Dm644 crates/xross-app/assets/icons/xross.png \
  ~/.local/share/icons/hicolor/256x256/apps/xross.png
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```
