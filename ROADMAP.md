# Roadmap

## 待办

### 节点与订阅

- [ ] 桌面端二维码扫描：通过可拖动取景框识别屏幕上的二维码，导入节点或订阅链接。

### 系统集成

- [ ] 清理系统副作用：退出或卸载时回收提权助手、虚拟网卡、系统代理设置与 PAC 端口。
- [x] 将提权助手的状态、安装、启动、卸载合并到测试页的检查列表。
- [x] 测试页补充系统诊断：网卡与默认路由、DNS 配置、代理模式、监听端口占用，结果可复制。

### 组网

- [ ] 通过组网同步配置：多设备经组网通道共享同一份 profile。

### UI 与文案

- [ ] 交互动画：页面切换、面板展开、列表增删与托盘浮窗统一曲线与时长，遵循系统“减少动态效果”设置。
- [ ] 补齐底层错误与警告提示的中英双语。
- [x] 新用户可从「本机直连」下载 sing-box，零节点启动。代理用户可在同一引导中添加订阅或节点。
- [ ] 首次启动缺少提权助手或自定义规则集时就地提示下一步。
- [ ] 关于页添加致谢。

### 验证

- [ ] macOS / Linux 实机验证：托盘、窗口材质、系统代理、提权助手、TUN 与组网。
- [ ] Windows / Linux 的通知展示与登录启动验收。

### 构建与发布

- [ ] CI：在 Windows / macOS / Linux 上构建，运行 `cargo test -p xrose-core`，检查格式与 clippy。
- [ ] 打包：Windows 安装包、macOS `.app` / dmg、Linux AppImage / deb。

## 已知限制

- **Linux 托盘**：需要 StatusNotifierItem 宿主；Wayland 下浮窗位置由合成器决定；无宿主时关闭窗口即退出。
- **Windows 托盘**：进程完整性级别低于任务栏时托盘注册失败，应用退化为关闭即退出。可将程序放到普通目录，或执行 `icacls <目录> /setintegritylevel (OI)(CI)M` 恢复。
- **提权助手**：Linux tproxy 规则与 macOS `networksetup` DNS 路径尚未在真机端到端验证。
- **系统代理**：仅支持 GNOME / KDE、macOS、Windows。
- **Xray**：无法列出连接；不支持 Hysteria2、TUIC、AnyTLS、OpenVPN、OpenConnect。
- **sing-box**：仅支持 1.12+。
- **App 更新**：仅提供版本检查与下载页跳转，不自动替换。
- **OpenVPN / OpenConnect**：暂不支持浏览器 SSO、动态认证表单、OpenVPN 静态密钥与 TAP；`.ovpn` 须内嵌证书与密钥；未经真实服务端登录验证。
- **WireGuard**：一个节点对应一个对端，多对端配置需拆分导入。
- **移动端**：未实现。
