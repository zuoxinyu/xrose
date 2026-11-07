# Xrose 品牌资源

[带字标的组合稿](../../../docs/branding/xrose-wordmark.svg)引用同目录的标志原稿，产品名写作 Xrose。

应用标志使用蓝紫色的 X 与玫瑰加粗轮廓。编辑[矢量原稿](../../../docs/branding/xrose.svg)后，在仓库根目录运行 `scripts/render-brand-icons.sh` 重新导出资源（需要 librsvg 的 `rsvg-convert`）。

- `logo-mark.png`：900×900 透明标志。
- `app-icon.png`、`app-icon-{256,128,96,64}.png`：应用、窗口及启动器使用的彩色图标。
- `tray-template.png`：菜单栏和托盘共用的透明蒙版；针对小尺寸加粗，按系统外观显示为黑色或白色，无底板和边框。
- `icons/xrose.svg`、`icons/xrose.png`、`icons/xrose-128.png`：hicolor 主题使用的矢量与彩色图标。
- `io.xrose.Xrose.desktop`：与 Wayland `app_id` 对应的桌面入口。

安装 Linux 桌面入口及启动器图标：

```bash
install -Dm644 crates/xrose-app/assets/io.xrose.Xrose.desktop \
  ~/.local/share/applications/io.xrose.Xrose.desktop
install -Dm644 crates/xrose-app/assets/icons/xrose.svg \
  ~/.local/share/icons/hicolor/scalable/apps/xrose.svg
install -Dm644 crates/xrose-app/assets/icons/xrose.png \
  ~/.local/share/icons/hicolor/256x256/apps/xrose.png
update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
```
