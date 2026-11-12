# Roadmap

## Milestones（Project Xristina 演进路线）

Xrose 正按阶段演进至自研 Rust 统一数据面内核 **Project Xristina**，实现单虚拟网卡（Monolithic TUN）、内置统一 DNS、毫秒级无感热重载与自研抗探测协议：

| 里程碑 | 目标定位 | 当前状态 | 技术方案 |
| :--- | :--- | :--- | :--- |
| **M0** | **Direct as First Class**：默认直连模式、实时进程/连接监控、应用层阻止规则与系统诊断工作台 | **已完成** | [M0 方案](docs/MILESTONE.M0.DIRECT-AS-FIRST-CLASS.md) |
| **M1** | **Project Xristina 地基**：自研 Rust 单虚拟网卡（Monolithic TUN）、统一内置 DNS 总线、零拷贝 L3 交换与直连守护引擎 | 规划中 | [M1 蓝图](docs/MILESTONE.M1.PROJECT-XRISTINA.md) |
| **M2** | **Xristina Link（XLP）**：自研抗探测协议（TLS Exporter SPA 鉴权、分级形态塑形、QUIC + 轮转 TCP 双载波无感热备）与轻量服务端 `xristina-server` | 规划中 | [M2 方案](docs/MILESTONE.M2.XRISTINA-LINK.md) |
| **M3** | **生态协议兼容与热重载**：补齐主力兼容协议栈（VLESS / Trojan / SS-2022 / Hy2 等）、uTLS 指纹层与基于 RCU 的原子热重载 | 规划中 | 见 M1 §5 & §7 |
| **M4** | **原生组网融合**：EasyTier 内存态虚拟端口与纯 Rust Tailscale 驱动，消除多网卡与回环桥接损耗 | 规划中 | 见 M1 §6 |
| **M5** | **全协议收口与平台拓展**：默认内核切换与低内存移动端（iOS / Android）/ 路由器（OpenWrt）拓展 | 规划中 | 见 M1 §7 |

---

## 近期待办

### 系统集成

- [ ] 卸载与重置清理：回收提权助手计划任务 / 系统服务等残留副作用。

### UI 与文案

- [ ] 交互动画：页面切换、面板展开、列表增删与托盘浮窗统一曲线与时长，遵循系统“减少动态效果”设置。
- [ ] 补齐底层错误与警告提示的中英双语支持。
- [ ] 首次启动引导：缺少提权助手或自定义规则集时就地提示下一步。

### 验证

- [ ] macOS / Linux 实机全量验收：托盘交互、窗口材质、系统代理、提权助手、TUN 与组网端到端路径。
- [ ] Windows / Linux 的系统通知展示与登录启动实机验收。

### 构建与发布

- [ ] CI：GitHub Actions 在 Windows / macOS / Linux 上构建、运行 `cargo test -p xrose-core`、检查格式与 clippy。
- [x] 发布打包脚本与工作流：macOS DMG、Windows 便携 ZIP、Linux AppImage / deb、Arch Linux 原生包，均提供 full 与 lite。见[发布说明](docs/RELEASE.md)。

---

## 已知限制

- **Linux 托盘**：需要 StatusNotifierItem 宿主（如 KDE Plasma 或 GNOME AppIndicator 扩展）；不支持 layer-shell 的 Wayland 合成器下浮窗位置由合成器决定，非 KDE 桌面「关闭到系统托盘」以最小化主窗口实现；无宿主时关闭窗口即退出。
- **Windows 托盘**：进程完整性级别低于任务栏时托盘注册失败，应用退化为关闭即退出。可将程序置于常规目录，或执行 `icacls <目录> /setintegritylevel (OI)(CI)M` 恢复目录完整性标签。
- **提权助手**：Linux tproxy 规则与 macOS `networksetup` DNS 路径尚未在真机端到端验证。
- **系统代理**：仅支持 GNOME / KDE、macOS 与 Windows；其他桌面环境会明确报错。
- **Xray**：不支持连接列表与进程/域名流量统计；不支持 Hysteria2、TUIC、AnyTLS、OpenVPN 与 OpenConnect。
- **sing-box**：仅支持 1.12+ 版本。
- **App 更新**：仅提供版本检查与下载页跳转，不执行二进制自替换。
- **OpenVPN / OpenConnect**：暂不支持浏览器 SSO、动态认证表单、OpenVPN 静态密钥与 TAP；`.ovpn` 须内嵌证书与密钥；尚未经过真实服务端登录验证。
- **WireGuard**：一个节点对应一个对端，多对端配置需拆分导入。
- **移动端**：未实现。
