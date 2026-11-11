# Xrose 桌面发布

初次安装可选 **full**，内含 Xrose、xrose-cli、官方 sing-box 1.14.4 及五个二进制规则集。已有内核和规则集时可用 **lite** 更新应用。两个版本使用同一个 GUI 二进制、应用标识和用户数据格式。Xray 可在设置中单独下载。

| 系统 | GitHub Release 产物 | 架构 |
| --- | --- | --- |
| macOS | DMG，拖入「应用程序」安装 | Apple Silicon、Intel |
| Windows | ZIP，解压后运行 Xrose.exe | x64、ARM64 |
| Linux | AppImage、Debian 包 | x86_64、aarch64 |
| Arch Linux | 原生 pacman 包（.pkg.tar.zst） | x86_64 |

AppImage 下载后赋予执行权限。Debian 系统用 `apt install ./文件名.deb`，Arch Linux 用 `pacman -U ./文件名.pkg.tar.zst`。full 和 lite 的系统包名均为 `xrose`，可直接替换。Windows 更新前退出 Xrose，把 lite ZIP 解压到原目录，保留 `XroseData`。应用内检查新版本后，下载按钮打开相同平台、架构和安装格式的 lite 包，由用户完成替换。

## 首次启动

Full 在打开主窗口前将资源写入当前用户的持久数据目录。指定 `--home` 或 `XROSE_HOME` 时，下列路径均位于该目录的 `data/` 中。Windows 便携版默认使用程序旁的 `XroseData` 作为 home，其余平台使用系统应用数据目录。

- `bin/xrose-cli`（Windows 为 `.exe`）可通过完整路径运行。
- `cores/sing-box/1.14.4/` 保存原版可执行文件，并登记到内核管理器。全新配置默认选中此版本；已有设置保留原来的内核选择。
- `rulesets/` 包含 geoip-cn、geosite-cn、geosite-gfw、geosite-geolocation-!cn 和 geosite-category-ads-all。
- `mesh-binaries/` 保留经过固定 SHA-256 校验的官方压缩包，供 Tailscale 组件独立验证。Windows 的虚拟网卡驱动仍按需安装。

首次复制会校验内核和规则集。全部完成后记录状态，中断可重试。此后管理员或用户通过应用下载的内核和规则集独立于应用包。还可在隔离目录运行 `xrose-app --prepare-resources --home <目录>`，仅执行同一套资源准备流程。

## 本地构建

在目标操作系统上安装 Rust、C/C++ 工具链、protoc 和 Python 3，执行对应命令。输出默认在工作区的 `dist/`。macOS 的 `scripts/build-macos.sh` 仍用于本地构建安装单个 .app。

| 系统 | 命令 | 其他要求 |
| --- | --- | --- |
| macOS | `scripts/release-macos.sh` | Xcode 命令行工具 |
| Windows | `./scripts/build-windows.ps1 -Action release` | MSVC、7-Zip、Python 在 PATH 中 |
| Linux | `scripts/release-linux.sh` | 编译依赖见发布工作流，需 dpkg-dev、squashfs-tools、patchelf、zstd。x86_64 另需 makepkg 或 Docker |

Linux 构建流程在 Debian/Ubuntu 环境生成 deb。Arch 包使用 makepkg；Ubuntu 构建机通过官方 Arch 容器执行这一步。AppImage 使用 linuxdeploy 收集共享库，appimagetool 生成映像。CI 在 Ubuntu 24.04 构建，不保证兼容使用更早 glibc 的发行版。

首次打包需要网络。内核与规则集的下载地址或版本、SHA-256 见 [资源清单](../packaging/resources.lock.json)。规则集锁定到具体提交，geosite-gfw 使用与应用相同的 MetaCubeX 来源。修改内核版本时同步更新组网组件的官方摘要。已验证的下载缓存在 `target/release-cache/`，不随源码提交。

## GitHub Actions

[release.yml](../.github/workflows/release.yml) 支持手动构建和标签发布。手动运行仅生成可下载的工作流产物。推送与 Cargo 工作区版本一致的标签（如 `v0.1.0`）后，各平台解包测试 full/lite，全部成功再创建 GitHub Release，附带各安装包与 `SHA256SUMS`。发布构建自动设置 `XROSE_GITHUB_REPOSITORY`；本地构建若需启用应用更新，请将其设为正式发布仓库的 `owner/repo`。

macOS 本机继续优先使用指定身份或本机 Apple Development 证书。公开分发可在仓库 Secrets 配置 `MACOS_DEVELOPER_ID_P12`（Base64 编码）和 `MACOS_DEVELOPER_ID_PASSWORD`，使用 Developer ID Application 签名。再配置 `MACOS_NOTARY_APPLE_ID`、`MACOS_NOTARY_TEAM_ID` 和 `MACOS_NOTARY_PASSWORD` 可公证 DMG 并附上凭据。未配置证书时，干净的 CI runner 会生成临时签名、未经公证的 DMG。Windows ZIP 默认没有 Authenticode 签名。
