# AGENTS.md

Xrose 是 Project Xristina 旗下的 Xray / sing-box 跨平台桌面客户端，Rust + GPUI Kit，GPL-3.0-or-later。产品名固定写作 `Xrose`，仅首字母大写。
产品文档与界面文案为中文，英文通过 `tr(lang, zh, en)` 提供。

## 常用命令

- 构建 `cargo build --workspace`；运行 GUI `cargo run -p xrose-app`
- 打包安装 macOS 应用 `scripts/build-macos.sh [-r]`（产出 `target/macos/<profile>/Xrose.app`，签名后复制到 `~/Applications`）
- 测试 `cargo test -p xrose-core`
- 无界面验证优先用 `xrose-cli`（`demo`、`seed-demo --home <dir>`、`check --core sing-box --home <dir>`，详见 `--help`）
- 状态一律用 `XROSE_HOME=/tmp/xrose-demo` 或 `--home` 隔离，不得触碰用户真实配置
- 编译另有模拟数据的源码副本时，必须为副本单独设置 `CARGO_TARGET_DIR`，覆盖构建、测试与运行命令；不得复用主工作区的 `target` 或应用包。验证普通构建时，在新的空目录运行 `xrose-cli mesh list --home <dir>`，结果应为 `[]`。
- 只格式化自己改动的文件（`rustfmt <file>`）；`cargo build --workspace` 无警告，新增警告视为回归

## 结构

- `crates/xrose-core`：非 UI 逻辑（模型、适配器、内核进程、订阅、组网、存储、系统集成、`service.rs`）
- `crates/xrose-app`：GPUI 界面（`ui/`）、托盘（`tray/`）
- `crates/xrose-cli`：无界面入口

## 架构约束

1. `AppModel` 与内核无关，只保存用户意图；协议字段名只出现在 `adapters/xray.rs` 与 `adapters/singbox.rs`。
2. 内核不支持的能力：降级并在 `CompiledConfig::warnings` 写中文提示；仅在确实无法编译时返回 `Err`。
3. UI 只通过 `AppService` 访问核心；方法同步、结果经 `snapshot()` 返回，耗时操作设置 `busy` 并提示。
4. 不得阻塞 `render`/`tick`，渲染路径中不做网络或磁盘操作。
5. 「热重载」自动应用保存的配置。运行时先校验候选配置，再替换内核进程；替换失败时尝试恢复原配置。
6. sing-box 输出面向 1.12+，不得回退到旧版 `geoip`/`geosite` 与旧 DNS 格式。

## 代码约定

- 模块文档与注释用英文；用户可见文本、错误、警告用中文，UI 处用 `tr` 提供双语。
- 错误使用 `crate::error::Error`，信息需可操作。
- 序列化枚举用 `rename_all = "snake_case"`，配置结构保留 `#[serde(default)]`。
- 依赖统一在 `[workspace.dependencies]` 声明。
- 考虑开发和维护成本，如果实现代价突出，例如需要 vendor 第三方库、引入复杂的构建方式或明显会增加二进制体积、高资源占用，必须先说明原因、替代方案与维护成本，询问用户意见并获得明确同意后再引入；不得自行复制第三方库源码或新增对应的本地依赖覆盖。
- 持久化结构与内部协议变更：release（tag）前的迭代开发阶段不兼容修改不需要多次提升 `SCHEMA_VERSION` 或内部协议版本（`tun_process::VERSION`、`mesh::PROTOCOL_VERSION`）；仅在跨已发布版本且必须保留旧数据时才提升版本并编写迁移。
- 提交信息采用 conventional 风格，小写 scope，一次提交一个关注点，使用诸如feat(ui,windows)等前缀。
- 测试写在被测文件底部 `#[cfg(test)]`，不依赖网络、真实内核、root 或显示器。

## UI 与文案

- UI 相关改动前必须阅读 [docs/DESIGN.md](docs/DESIGN.md)，遵循其中的 token 与组件规范。
- 如果用户没有明确说明，开发新功能时，如果新增 UI 元素会破坏原有的布局，要先给出原型设计图。
- 文案简洁、正式、客观；不使用修辞性对比、行话或拟人表述，不暴露实现细节。不要把冗长的细节使用说明增加到 README.md 中。
- 统一术语：`提权助手`、`内核` / `Core`、`出站组`、`规则集`、`异地组网`（导航中为`组网`）、`节点订阅`、`HTTP 测速`。

## 隐私与安全

- 不得在代码、测试夹具、文档、日志或 commit 信息中泄露个人隐私，包括姓名、邮箱、用户名、本机路径、主机名、IP、设备标识等；示例一律使用虚构数据。
- 不得记录、复制或提交凭据、订阅链接/令牌、`secrets.key` 或解密后的 `enc:v1:` 内容。

## 平台注意

- 托盘支持 macOS、Windows 与 Linux（`StatusNotifierItem`）；Linux 无托盘宿主或其他未实现平台时 `available()` 必须为 false，不得强制为 true。
- 窗口材质支持 macOS vibrancy、Windows Mica / Mica Alt / Acrylic 与 Linux Wayland 毛玻璃（`ext-background-effect-v1` / KWin blur）；X11 及不支持模糊的合成器自动回退为纯色。修改 `Theme::change` / `Theme::sync_system_appearance` 的调用处后，需重新执行 `window_effects::make_surface_translucent`。
- Windows：保留 `crates/xrose-app/build.rs` 中的图标资源与主线程栈设置；已知失败测试 `second_acquire_fails` 可用 `-- --skip second_acquire_fails` 跳过。
- Windows：GUI 进程完整性级别须不低于 shell，否则 `Shell_NotifyIconW` 失败；若工作区目录带 Low 完整性标签，用 `icacls <workspace> /setintegritylevel (OI)(CI)M` 恢复。
- Windows：`build.rs` 将图标嵌入为资源 ID 1，并把主线程栈提升至 16 MiB，修改构建时两者都要保留。
- `objc` 宏依赖 `crates/xrose-app/Cargo.toml` 中 `[lints.rust]` 的 `cfg(feature = "cargo-clippy")` 条目；不得整体放行 `unexpected_cfgs`。
- `vendor/block`（`block 0.1.6` 静态类型修复）及 `vendor/gpui-pre`、`vendor/gpui-pre-windows`（Windows 字号与 DirectWrite 渲染、触摸、原生拖放、层表面与列表高度提示等本地修复）通过 `[patch.crates-io]` 引入。
- `gpui-pre 0.3.14` 起上游路径渲染改为按 tile 渲染（`PaintedPath::tile_count` + `D32_FLOAT` 深度模板），`directx_renderer.rs` 不再包含 `path_intermediate_texture`；不得重新引入按窗口分配的 MSAA 路径中间纹理。
- 注册表、`networksetup`、GNOME/KDE 等平台代码主要在 Linux 上验证过；声称平台修复有效前，先运行 `xrose-cli doctor` / `check`。

## 修改入口

详见 `docs/ARCHITECTURE.md`。新增 UI 页面：在 `ui/` 新建文件，并在 `ui/mod.rs` 添加 `Page` 变体、`icon()`、`zh()`/`en()` 与 `page_content()` 分支。新增 CLI 子命令：`crates/xrose-cli/src/main.rs`。能力映射变化时同步更新 `README.md` 与 `docs/ARCHITECTURE.md`。
