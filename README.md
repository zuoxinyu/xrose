# Xross

<p align="center">
  <img src="crates/xross-app/assets/logo-mark.png" width="140" alt="Xross logo" />
</p>

Xross 是跨平台的 Xray / sing-box 桌面客户端，同时内置异地组网（EasyTier / Tailscale），使用 Rust 与 GPUI Kit 构建，支持 Windows、macOS 和 Linux。

![节点页面](docs/screenshots/nodes.png)

## 功能

- **代理**：管理节点与订阅，配置出站组、路由规则和 DNS；支持系统代理、PAC 与 TUN。
- **路由**：导入、导出规则、分组与规则集定义，可将 SwitchyOmega Conditions 和 AutoProxy 列表转换为本地路由规则。
- **Core**：下载和切换 Xray 或 sing-box 版本，应用配置前自动校验。
- **订阅**：支持 Base64、分享链接、Clash YAML、SIP008 与 sing-box JSON。
- **诊断**：连接列表、日志、延迟与下载测速、连接链路测试。
- **界面**：深浅色主题、中英文、托盘面板。

## 局域网代理与更新

在「设置 → 网络与代理」开启局域网访问后，展开下方的「高级设置」即可配置第二端口和认证账号。默认共用主端口，开启认证后，本机和局域网客户端都需要登录。启用第二端口后，主端口仅监听本机，局域网设备使用独立端口和配置的认证方式。sing-box 在同一端口接受 HTTP 和 SOCKS5。Xray 在每个配置端口接受 SOCKS5，HTTP 使用其下一端口。

「高级设置」中的「前置代理」可从当前配置中选择节点，局域网关闭时也可使用。选择后，内核先连接前置节点，再通过它连接目标代理节点。局域网监听和前置代理设置在下次启动内核或点击「应用配置」后生效。

在「设置 → 更新与备份」可分别开启「使用代理更新订阅」和「使用代理更新资源」，也可填写 User-Agent。两项代理开关默认关闭。开启后，请先启动带 HTTP 或混合入站的内核。资源包括版本检查、内核安装包、校验文件、规则集、地理数据及组网组件。User-Agent 留空时使用默认值，已有订阅的自定义值优先。

## 导入和导出路由

打开「路由 → 路由规则 → 导入 / 导出」选择文件。导入前可查看规则、分组、规则集数量及转换提示，确认后追加到当前配置。运行中的内核需点击「应用配置」才会使用新规则。单条规则和整个分组可从各自的「⋯」菜单导出。Xross JSON 保留规则顺序、启用状态、动作和引用的规则集；只导出规则集时，文件包含来源与更新设置，不包含已下载的数据。要使用导入的规则集，在「添加规则」中选择「规则集」并填写标签。

SwitchyOmega 请使用自动切换情景模式导出的 **Conditions 规则列表**，或 AutoProxy 列表（明文、Base64 均可）。导入后生成一个独立分组，排在已有自定义分组之后、未分组和预设规则之前。不带结果的列表将匹配项转为代理、例外转为直连，未命中时继续匹配原配置。带 `@with result` 的列表保留指定动作；末尾的 `* +情景模式` 会接管该分组之后的 TCP / UDP 流量，可在分组中禁用这条规则。

| 源条件 | 转换结果 |
|---|---|
| `example.org`、`*.example.org` | 完整域名、域名后缀（包含根域名） |
| 主机通配符、主机正则、`Ip: 192.0.2.0/24` | 主机匹配或目标 IP 网段 |
| AutoProxy 的 `\|\|example.org`、`@@\|\|safe.example.org` | 域名后缀；例外优先 |
| URL 路径、URL 正则、浏览器专有条件 | 跳过；预览列出数量和源文件行号 |

`direct` 对应直连；命名情景模式优先匹配同名出站组，找不到时使用默认代理，并在预览中提示。SwitchyOmega 整份设置备份（`.bak`）、PAC 脚本和二进制 `.srs` 不是路由交换文件。条件顺序和语法见 [SwitchyOmega 格式说明](https://github.com/FelisCatus/SwitchyOmega/wiki/SwitchyOmega-conditions-format)。

## 异地组网

在“组网”页连接 EasyTier 或 Tailscale，让多台设备处于同一虚拟网络。两种后端可同时在线，且独立于代理 Core。

- **两种后端**：EasyTier 内置于应用；Tailscale 支持官方服务和自建控制服务器，使用浏览器授权登录。
- **出口节点**：使用远端设备作为出口，或将本机发布为出口。
- **子网路由**：发布本机子网，或访问指定的远端子网。
- **作为代理节点**：设置本地出站端口后，组网出口会生成为节点，可加入出站组和分流规则。
- **端口转发**：将本地 TCP / UDP 端口转发到组网设备端口。
- **自动分流**：组网连接期间，自动处理代理直连、TUN 路由排除和 PAC 绕过。
- **随应用启动**：可选择启动应用时自动连接。

组网操作由提权助手执行，使用前需在设置中安装。构建要求、平台依赖与使用细节见 [组网文档](docs/MESH.md)。

## 快速开始

要求：Rust 1.95+，Windows、macOS 或 Linux 桌面。Linux 需要 GTK3、`libxkbcommon` 与 `wayland-client`（或 X11）开发库。Windows 构建参见 [组网文档](docs/MESH.md)。

```bash
cargo run -p xross-app
```

首次使用：

1. 在“订阅”页添加订阅链接，或在“节点”页导入分享链接、手动添加节点。
2. 在“设置 → Core 管理”下载 Xray 或 sing-box。
3. 点击右上角“启动”。修改路由或 DNS 后，点击“应用配置”重启 Core 生效。

使用 TUN 或组网时，按提示安装提权助手。升级 Xross 后，可在「设置 → 提权助手与环境检查」点击「更新助手」，载入当前版本。更新会中断由助手管理的连接。

使用独立数据目录：`XROSS_HOME=/tmp/xross-demo cargo run -p xross-app`。

## CLI

`xross-cli` 是无界面入口，可用于脚本和调试：

```bash
xross-cli parse ./subscription.txt           # 解析订阅并列出节点
xross-cli import ./sub.yaml                  # 导入订阅文件
xross-cli route preview ./rules.sorl         # 预览 SwitchyOmega 转换结果
xross-cli route convert ./rules.sorl -o ./rules.json  # 仅转换，不导入当前配置
xross-cli route import ./rules.sorl          # 追加到当前配置
xross-cli route export -o ./routing.json     # 导出规则、分组和规则集
xross-cli compile --core sing-box -o c.json  # 编译配置
xross-cli check --core sing-box              # 用真实 Core 校验
xross-cli doctor                             # TUN / 端口 / 权限检查
xross-cli mesh --help                        # 组网命令
```

其余子命令见 `xross-cli --help`。

## 文档

- [组网](docs/MESH.md)
- [架构](docs/ARCHITECTURE.md)
- [UI 规范](docs/DESIGN.md)
- [路线图与已知限制](ROADMAP.md)

## 许可

GPL-3.0-or-later，见 `LICENSE`。GPUI / GPUI Kit 为 Apache-2.0。
