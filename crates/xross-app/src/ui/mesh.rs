//! Mesh configuration and cached runtime state. All work goes through AppService.
use super::{theme::palette, tr, widgets::*, AppView};
use gpui_base::{Disableable, Selectable};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::mesh::{MeshPhase, MeshProfile, MeshProfileView, MeshProvider, MeshSecret};
use xross_core::service::ToastLevel;

pub(crate) struct MeshForm {
    visible: bool,
    id: String,
    provider: MeshProvider,
    name: Entity<InputState>,
    hostname: Entity<InputState>,
    network: Entity<InputState>,
    secret: Entity<InputState>,
    peers: Entity<InputState>,
    address: Entity<InputState>,
    subnets: Entity<InputState>,
    control: Entity<InputState>,
}

impl MeshForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        Self {
            visible: false,
            id: MeshProfile::default().id,
            provider: MeshProvider::EasyTier,
            name: super::text_input(window, cx, ""),
            hostname: cx.new(|cx| InputState::new(window, cx).default_value("xross")),
            network: super::text_input(window, cx, ""),
            secret: cx.new(|cx| InputState::new(window, cx).masked(true)),
            peers: super::text_input(window, cx, "tcp://192.0.2.1:11010"),
            address: super::text_input(window, cx, "10.126.126.2/24"),
            subnets: super::text_input(window, cx, "192.168.50.0/24"),
            control: super::text_input(window, cx, "https://controlplane.tailscale.com"),
        }
    }

    fn load(&mut self, profile: MeshProfileView, window: &mut Window, cx: &mut Context<AppView>) {
        self.id = profile.id;
        self.provider = profile.provider;
        self.visible = true;
        for (input, value) in [
            (&self.name, profile.name),
            (&self.hostname, profile.hostname),
            (&self.network, profile.network_name),
            (&self.secret, String::new()),
            (&self.peers, profile.peers.join(", ")),
            (&self.address, profile.ipv4),
            (&self.subnets, profile.remote_subnets.join(", ")),
            (&self.control, profile.control_url),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    fn profile(&self, cx: &Context<AppView>) -> MeshProfile {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let list = |input: &Entity<InputState>| {
            value(input)
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        MeshProfile {
            id: self.id.clone(),
            name: value(&self.name),
            provider: self.provider,
            hostname: value(&self.hostname),
            network_name: value(&self.network),
            network_secret: MeshSecret(self.secret.read(cx).value().to_string()),
            peers: list(&self.peers),
            ipv4: value(&self.address),
            remote_subnets: list(&self.subnets),
            control_url: value(&self.control),
            auto_connect: false,
        }
    }
}

impl AppView {
    pub(crate) fn page_mesh(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let busy = self.snapshot.busy.iter().any(|key| key == "mesh");
        let mut body = div().flex().flex_col().gap_4().w_full().min_w(px(0.));
        body=body.child(card(&p)
            .child(section_title(&p,self.tr("异地组网","Mesh networks"),self.tr("EasyTier 与 Tailscale 可同时在线，独立于代理核心","EasyTier and Tailscale run independently of the proxy core")))
            .child(div().flex().flex_wrap().gap_2()
                .child(Button::new("mesh-new").primary().label(self.tr("新增网络","Add network")).on_click(cx.listener(|this,_,window,cx|{
                    this.mesh_form.load(MeshProfile::default().summary(),window,cx);cx.notify();
                })))
                .when(!self.snapshot.mesh.tailscale_installed,|d|d.child(Button::new("mesh-install").label(self.tr("安装 Tailscale 组件","Install Tailscale component")).disabled(busy).on_click(cx.listener(|this,_,_,_|this.service.mesh_install_tailscale()))))
                .when(!self.snapshot.platform.helper_available,|d|d.child(Button::new("mesh-helper").label(self.tr("准备特权助手","Set up helper")).on_click(cx.listener(|this,_,_,_|{
                    if this.snapshot.platform.helper_installed {this.service.start_helper();} else {this.service.install_helper();}
                })))))
            .child(div().text_sm().text_color(p.muted_fg).child(self.tr("默认不自动连接、不启用出口节点、不发布本机网段。关闭到托盘会保持连接，退出时断开。","Connections are manual. Exit nodes and local subnet advertising are disabled. Connections remain in the tray and stop on exit.")))
            .when(!self.snapshot.mesh.supported,|d|d.child(empty_hint(&p,tr(lang,"当前平台仅支持配置管理；网络功能首先在 Windows 启用。","Configuration is available here; networking is enabled on Windows first."))))) ;

        if self.mesh_form.visible {
            let mut form = card(&p).child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("mesh-kind-et")
                            .label("EasyTier")
                            .selected(self.mesh_form.provider == MeshProvider::EasyTier)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mesh_form.provider = MeshProvider::EasyTier;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("mesh-kind-ts")
                            .label("Tailscale")
                            .selected(self.mesh_form.provider == MeshProvider::Tailscale)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mesh_form.provider = MeshProvider::Tailscale;
                                cx.notify();
                            })),
                    ),
            );
            let mut fields = vec![
                (self.tr("配置名称", "Profile name"), &self.mesh_form.name),
                (
                    self.tr("设备名称", "Device hostname"),
                    &self.mesh_form.hostname,
                ),
            ];
            if self.mesh_form.provider == MeshProvider::EasyTier {
                fields.extend([
                    (self.tr("网络名称", "Network name"), &self.mesh_form.network),
                    (
                        self.tr(
                            "网络密钥（编辑时留空保留）",
                            "Network secret (leave blank to keep)",
                        ),
                        &self.mesh_form.secret,
                    ),
                    (
                        self.tr("入口节点（逗号分隔）", "Entry peers (comma separated)"),
                        &self.mesh_form.peers,
                    ),
                    (
                        self.tr(
                            "虚拟 IPv4（留空自动分配）",
                            "Virtual IPv4 (blank for automatic)",
                        ),
                        &self.mesh_form.address,
                    ),
                ]);
            } else {
                fields.push((
                    self.tr(
                        "控制服务器（留空使用 Tailscale）",
                        "Control server (blank for Tailscale)",
                    ),
                    &self.mesh_form.control,
                ));
            }
            fields.push((
                self.tr(
                    "允许访问的远端网段（逗号分隔）",
                    "Allowed remote subnets (comma separated)",
                ),
                &self.mesh_form.subnets,
            ));
            for (label, input) in fields {
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w(px(0.))
                        .child(div().text_sm().child(label))
                        .child(Input::new(input)),
                );
            }
            if self.mesh_form.provider == MeshProvider::Tailscale {
                form = form.child(
                    div().text_sm().text_color(p.muted_fg).child(self.tr(
                        "保存后点击“连接”，再点击“在浏览器登录”完成授权。断开会保留登录状态。",
                        "Save, then connect and select “Sign in with browser” to authorize this device. Disconnecting keeps you signed in.",
                    )),
                );
            }
            body = body.child(
                form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("mesh-save")
                                .primary()
                                .label(self.tr("加密保存", "Save encrypted"))
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    match this.service.save_mesh_profile(this.mesh_form.profile(cx))
                                    {
                                        Ok(_) => {
                                            this.mesh_form.visible = false;
                                            this.mesh_form
                                                .secret
                                                .update(cx, |s, cx| s.set_value("", window, cx));
                                        }
                                        Err(e) => {
                                            this.service.toast(ToastLevel::Error, e.to_string())
                                        }
                                    }
                                    this.sync(cx);
                                })),
                        )
                        .child(
                            Button::new("mesh-cancel")
                                .label(self.tr("取消", "Cancel"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.mesh_form.visible = false;
                                    this.mesh_form
                                        .secret
                                        .update(cx, |s, cx| s.set_value("", window, cx));
                                    cx.notify();
                                })),
                        ),
                ),
            );
        }

        for profile in self.snapshot.mesh.profiles.clone() {
            let provider = profile.provider;
            let id = profile.id.clone();
            let edit = profile.clone();
            let delete = profile.id.clone();
            let state = self
                .snapshot
                .mesh
                .networks
                .iter()
                .find(|n| n.profile_id == profile.id)
                .cloned()
                .unwrap_or_default();
            let active = matches!(
                state.phase,
                MeshPhase::Starting
                    | MeshPhase::NeedsLogin
                    | MeshPhase::Connected
                    | MeshPhase::Stopping
            );
            let label = match state.phase {
                MeshPhase::Disconnected => tr(lang, "未连接", "Disconnected"),
                MeshPhase::Starting => tr(lang, "正在连接", "Connecting"),
                MeshPhase::NeedsLogin => tr(lang, "等待登录或批准", "Sign-in or approval required"),
                MeshPhase::Connected => tr(lang, "已连接", "Connected"),
                MeshPhase::Stopping => tr(lang, "正在断开", "Disconnecting"),
                MeshPhase::Failed => tr(lang, "连接失败", "Failed"),
            };
            let mut network = card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(div().flex_1().min_w(px(0.)).truncate().child(format!(
                            "{} · {}",
                            profile.name,
                            provider.label()
                        )))
                        .child(pill(label, p.muted_fg, p.panel_alt)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new(SharedString::from(format!("mesh-connect-{}", id.clone())))
                                .primary()
                                .disabled(busy || !self.snapshot.mesh.supported)
                                .label(if active {
                                    tr(lang, "断开", "Disconnect")
                                } else {
                                    tr(lang, "连接", "Connect")
                                })
                                .on_click(cx.listener(move |this, _, _, _| {
                                    if active {
                                        this.service.mesh_disconnect(provider);
                                    } else {
                                        this.service.mesh_connect(&id);
                                    }
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "mesh-edit-{}",
                                edit.id.clone()
                            )))
                            .label(tr(lang, "编辑", "Edit"))
                            .disabled(active || busy)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.mesh_form.load(edit.clone(), window, cx);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "mesh-delete-{}",
                                delete.clone()
                            )))
                            .label(tr(lang, "删除配置", "Delete profile"))
                            .disabled(active || busy)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Err(e) = this.service.delete_mesh_profile(&delete) {
                                        this.service.toast(ToastLevel::Error, e.to_string());
                                    }
                                    this.sync(cx);
                                },
                            )),
                        )
                        .when(active && provider == MeshProvider::Tailscale, |d| {
                            d.child(
                                Button::new("mesh-logout")
                                    .label(tr(lang, "注销设备身份", "Log out device"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.service.mesh_logout(provider)
                                    })),
                            )
                        }),
                );
            if let Some(url) = state.login_url {
                network = network.child(
                    Button::new("mesh-login")
                        .primary()
                        .label(self.tr("在浏览器登录", "Sign in with browser"))
                        .on_click(cx.listener(move |this, _, _, _| this.service.open_url(&url))),
                );
            }
            if !state.addresses.is_empty() {
                network =
                    network.child(div().text_sm().min_w(px(0.)).line_clamp(3).child(format!(
                        "{}: {}",
                        self.tr("虚拟地址", "Addresses"),
                        state.addresses.join(", ")
                    )));
            }
            if !state.routes.is_empty() {
                network =
                    network.child(div().text_sm().min_w(px(0.)).line_clamp(4).child(format!(
                        "{}: {}",
                        self.tr("网段与路由", "Networks and routes"),
                        state.routes.join(", ")
                    )));
            }
            if let Some(suffix) = state.dns_suffix {
                network = network.child(div().text_sm().child(format!("MagicDNS: *.{suffix}")));
            }
            if active && cfg!(windows) && self.snapshot.settings.effective_tun() {
                network=network.child(div().text_sm().text_color(p.warning).child(self.tr("组网运行期间暂时关闭 strict_route，严格 DNS 防泄漏过滤已放宽。","While mesh is active, strict_route is temporarily disabled and strict DNS leak filtering is relaxed.")));
            }
            if let Some(probe) = state.probe {
                network = network.child(
                    div()
                        .text_sm()
                        .text_color(if probe.success { p.success } else { p.warning })
                        .min_w(px(0.))
                        .line_clamp(3)
                        .child(format!(
                            "{} · {}{}",
                            probe.ip,
                            probe.detail,
                            probe
                                .latency_ms
                                .map(|ms| format!(" · {ms:.1} ms"))
                                .unwrap_or_default()
                        )),
                );
            }
            for message in [state.error, state.warning].into_iter().flatten() {
                network = network.child(
                    div()
                        .text_sm()
                        .text_color(p.danger)
                        .min_w(px(0.))
                        .line_clamp(4)
                        .child(message),
                );
            }
            for peer in state.peers {
                let target = peer.addresses.first().cloned();
                let peer_id = peer.id.clone();
                network =
                    network.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w(px(0.)).text_sm().truncate().child(
                                format!(
                                    "{} · {} · {}{}",
                                    peer.name,
                                    peer.addresses.join(", "),
                                    if peer.online {
                                        tr(lang, "在线", "Online")
                                    } else {
                                        tr(lang, "离线", "Offline")
                                    },
                                    peer.latency_ms
                                        .map(|ms| format!(" · {ms:.1} ms"))
                                        .unwrap_or_default()
                                ),
                            ))
                            .when_some(target, |d, ip| {
                                d.child(
                                    Button::new(SharedString::from(format!(
                                        "mesh-probe-{}",
                                        peer_id
                                    )))
                                    .compact()
                                    .flex_shrink_0()
                                    .label(tr(lang, "探测", "Probe"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.service.mesh_probe(provider, ip.clone())
                                    })),
                                )
                            }),
                    );
            }
            body = body.child(network);
        }
        if self.snapshot.mesh.profiles.is_empty() {
            body = body.child(empty_hint(
                &p,
                self.tr(
                    "添加一个网络以连接异地设备",
                    "Add a network to connect remote devices",
                ),
            ));
        }
        body
    }
}
