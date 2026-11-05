//! Mesh configuration and cached runtime state. All work goes through AppService.
use super::{
    AppView,
    theme::{Palette, palette},
    tr,
    widgets::*,
};
use gpui_base::{Disableable, SelectableText};
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::mesh::{MeshPhase, MeshProfile, MeshProfileView, MeshProvider, MeshSecret};
use xross_core::service::ToastLevel;

pub(crate) struct MeshForm {
    editing: bool,
    error: Option<String>,
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
    exit_node: Entity<InputState>,
    advertised: Entity<InputState>,
    forwards: Entity<InputState>,
    outbound_port: Entity<InputState>,
    advertise_exit: bool,
    connect_on_startup: bool,
}

impl MeshForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        Self {
            editing: false,
            error: None,
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
            exit_node: super::text_input(window, cx, ""),
            advertised: super::text_input(window, cx, "192.168.10.0/24"),
            forwards: super::text_input(window, cx, "tcp://127.0.0.1:8080/10.126.126.2:80"),
            outbound_port: super::text_input(window, cx, ""),
            advertise_exit: false,
            connect_on_startup: false,
        }
    }

    fn load(&mut self, profile: MeshProfileView, window: &mut Window, cx: &mut Context<AppView>) {
        self.id = profile.id;
        self.provider = profile.provider;
        self.advertise_exit = profile.advertise_exit_node;
        self.error = None;
        for (input, value) in [
            (&self.name, profile.name),
            (&self.hostname, profile.hostname),
            (&self.network, profile.network_name),
            (&self.secret, String::new()),
            (&self.peers, profile.peers.join(", ")),
            (&self.address, profile.ipv4),
            (&self.subnets, profile.remote_subnets.join(", ")),
            (&self.control, profile.control_url),
            (&self.exit_node, profile.exit_node),
            (&self.advertised, profile.advertised_subnets.join(", ")),
            (
                &self.forwards,
                profile
                    .port_forwards
                    .iter()
                    .map(|f| f.text())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            (
                &self.outbound_port,
                if profile.outbound_port == 0 {
                    String::new()
                } else {
                    profile.outbound_port.to_string()
                },
            ),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    fn profile(&self, cx: &Context<AppView>) -> xross_core::Result<MeshProfile> {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let list = |input: &Entity<InputState>| -> Vec<String> {
            value(input)
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        Ok(MeshProfile {
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
            exit_node: value(&self.exit_node),
            advertise_exit_node: self.advertise_exit,
            advertised_subnets: list(&self.advertised),
            port_forwards: list(&self.forwards)
                .iter()
                .map(|v| xross_core::mesh::PortForward::parse(v))
                .collect::<xross_core::Result<Vec<_>>>()?,
            outbound_port: if value(&self.outbound_port).is_empty() {
                0
            } else {
                value(&self.outbound_port)
                    .parse::<u16>()
                    .map_err(|_| xross_core::Error::config("出口出站端口必须为 1024–65535"))?
            },
            outbound_secret: MeshSecret::default(),
        })
    }
}

impl AppView {
    fn mesh_text(
        &self,
        id: String,
        label: &str,
        values: Vec<String>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let overflow = self.mesh_overflow.get(&id).copied().unwrap_or(false);
        let expanded = overflow && self.mesh_expanded.contains(&id);
        let value = values.join(", ");
        let summary = value.clone();
        let group = SharedString::from(format!("{id}-hover"));
        let measure_id = id.clone();
        let measure_text = SharedString::from(summary.replace(['\r', '\n'], " "));
        let view = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, window, cx| {
                let style = window.text_style();
                let line = window.text_system().shape_line(
                    measure_text.clone(),
                    style.font_size.to_pixels(window.rem_size()),
                    &[style.to_run(measure_text.len())],
                    None,
                );
                // Reserve the copy control in both states; expansion must not change this threshold.
                let next = line.width > (bounds.size.width - px(30.)).max(px(0.));
                if next != overflow {
                    let view = view.clone();
                    let id = measure_id.clone();
                    cx.defer(move |cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.mesh_overflow.insert(id, next) != Some(next) {
                                cx.notify();
                            }
                        });
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let content =
            div()
                .id(SharedString::from(format!("{id}-content")))
                .group(group.clone())
                .flex()
                .items_start()
                .gap_1()
                .max_w_full()
                .min_w(px(0.))
                .when(expanded, |d| d.w_full())
                .child(
                    div()
                        .min_w(px(0.))
                        .py_1()
                        .when(expanded, |d| d.flex_1())
                        .when(!expanded, |d| d.truncate().child(summary))
                        .when(expanded, |d| {
                            d.flex().flex_col().gap_1().children(
                                values.into_iter().enumerate().map(|(index, value)| {
                                    div().min_w(px(0.)).whitespace_normal().child(
                                        SelectableText::new(
                                            SharedString::from(format!("{id}-value-{index}")),
                                            value,
                                        ),
                                    )
                                }),
                            )
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("{id}-copy")))
                        .compact()
                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                        .ghost()
                        .w(px(26.))
                        .h(px(26.))
                        .icon(IconName::Copy)
                        .text_color(p.muted_fg)
                        .flex_shrink_0()
                        .opacity(0.)
                        .group_hover(group, |s| s.opacity(1.))
                        .tooltip(self.tr("复制", "Copy"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                            this.service
                                .toast(ToastLevel::Success, this.tr("已复制", "Copied"));
                        })),
                )
                .when(overflow, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("{id}-expand")))
                            .compact()
                            .h(px(crate::ui::theme::CONTROL_HEIGHT))
                            .ghost()
                            .w(px(26.))
                            .h(px(26.))
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .text_color(p.muted_fg)
                            .flex_shrink_0()
                            .tooltip(if expanded {
                                self.tr("收起", "Collapse")
                            } else {
                                self.tr("展开", "Expand")
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.mesh_expanded.remove(&id) {
                                    this.mesh_expanded.insert(id.clone());
                                }
                                cx.notify();
                            })),
                    )
                });
        let text = div()
            .relative()
            .flex()
            .flex_1()
            .w_full()
            .min_w(px(0.))
            .child(measure)
            .child(content);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .when(!label.is_empty(), |d| {
                d.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.muted_fg)
                        .py_1()
                        .child(label.to_owned()),
                )
            })
            .child(text)
    }

    pub(crate) fn open_mesh_dialog(
        &mut self,
        profile: Option<MeshProfileView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mesh_form.editing = profile.is_some();
        self.mesh_form.load(
            profile.unwrap_or_else(|| MeshProfile::default().summary()),
            window,
            cx,
        );
        self.mesh_form.connect_on_startup = self
            .snapshot
            .settings
            .mesh_startup_profile(self.mesh_form.provider)
            == Some(self.mesh_form.id.as_str());
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.mesh_dialog(dialog, cx))
            } else {
                dialog
            }
        });
        cx.notify();
    }

    fn mesh_dialog(&mut self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let p = palette(cx);
        let busy = self.snapshot.busy.iter().any(|key| key == "mesh");
        let mut form = div()
            .id("mesh-editor-scroll")
            .max_h(px(520.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .min_w(px(0.))
            .child(
                div().flex().flex_shrink_0().child(
                    ChoiceGroup::new(
                        "mesh-provider",
                        vec!["EasyTier".into(), "Tailscale".into()],
                        usize::from(self.mesh_form.provider == MeshProvider::Tailscale),
                    )
                    .disabled(self.mesh_form.editing || busy)
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        if this.mesh_form.editing
                            || this.snapshot.busy.iter().any(|key| key == "mesh")
                        {
                            return;
                        }
                        this.mesh_form.provider = if *index == 0 {
                            MeshProvider::EasyTier
                        } else {
                            MeshProvider::Tailscale
                        };
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
                    if self.mesh_form.editing {
                        self.tr("网络密钥（留空保留）", "Network secret (blank to keep)")
                    } else {
                        self.tr("网络密钥", "Network secret")
                    },
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
        fields.extend([
            (
                self.tr(
                    "本机发布子网（逗号分隔）",
                    "Advertised subnets (comma separated)",
                ),
                &self.mesh_form.advertised,
            ),
            (
                self.tr(
                    "出口节点（设备名或虚拟 IP）",
                    "Exit node (device name or virtual IP)",
                ),
                &self.mesh_form.exit_node,
            ),
            (
                self.tr(
                    "出口出站端口（留空禁用）",
                    "Exit outbound port (blank to disable)",
                ),
                &self.mesh_form.outbound_port,
            ),
            (
                self.tr("端口转发（逗号分隔）", "Port forwards (comma separated)"),
                &self.mesh_form.forwards,
            ),
        ]);
        for (label, input) in fields {
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_shrink_0()
                    .min_w(px(0.))
                    .child(div().text_sm().child(label))
                    .child(Input::new(input)),
            );
        }
        form = form.child(toggle("mesh-connect-on-startup", self.tr("随应用启动连接", "Connect when app starts"), self.mesh_form.connect_on_startup).flex_shrink_0().on_change(cx.listener(|this, checked, _, cx| { this.mesh_form.connect_on_startup = *checked; cx.notify(); })))
            .child(div().flex_shrink_0().text_sm().text_color(p.muted_fg).child(self.tr("每种组网类型可选择一个启动连接配置。", "Select one startup profile for each network type.")))
            .child(toggle("mesh-advertise-exit", self.tr("发布本机为出口节点", "Advertise this device as an exit node"), self.mesh_form.advertise_exit).flex_shrink_0().on_change(cx.listener(|this, checked, _, cx| { this.mesh_form.advertise_exit = *checked; cx.notify(); })))
            .child(div().flex_shrink_0().text_sm().text_color(p.muted_fg).child(self.tr("填写出口出站端口后，节点列表将生成可用于出站组和分流规则的出口节点。转发格式：tcp://监听IP:端口/目标IP:端口（支持 udp）。", "An outbound port creates a node for groups and routing rules. Forward format: tcp://listen-IP:port/target-IP:port (udp supported).")));
        if self.mesh_form.provider == MeshProvider::Tailscale {
            form = form.child(
                    div().flex_shrink_0().text_sm().text_color(p.muted_fg).child(self.tr(
                        "保存后连接，并在浏览器中完成授权。发布子网与出口需在控制端批准；断开连接保留登录状态。",
                        "Connect and authorize in your browser. Advertised subnets and exit nodes require control-plane approval. Disconnecting preserves sign-in.",
                    )),
                );
        }

        let view = cx.entity().downgrade();
        dialog
            .centered()
            .w(px(640.))
            .bg(p.panel)
            .text_color(p.fg)
            .border_color(p.border)
            .entrance(DialogEntrance::None)
            .overlay_closable(false)
            .on_ok(|_, _, _| false)
            .title(if self.mesh_form.editing {
                self.tr("编辑网络", "Edit network")
            } else {
                self.tr("添加网络", "Add network")
            })
            .on_close(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.mesh_form
                        .secret
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.mesh_form.error = None;
                    cx.notify();
                });
            })
            .child(form.when_some(self.mesh_form.error.clone(), |d, error| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(p.danger)
                        .child(error),
                )
            }))
            .footer(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("mesh-cancel")
                            .label(self.tr("取消", "Cancel"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mesh_form
                                    .secret
                                    .update(cx, |s, cx| s.set_value("", window, cx));
                                this.mesh_form.error = None;
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("mesh-save")
                            .primary()
                            .label(self.tr("保存配置", "Save profile"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                match this.mesh_form.profile(cx).and_then(|profile| {
                                    this.service.save_mesh_profile_with_startup(
                                        profile,
                                        this.mesh_form.connect_on_startup,
                                    )
                                }) {
                                    Ok(_) => {
                                        this.mesh_form
                                            .secret
                                            .update(cx, |s, cx| s.set_value("", window, cx));
                                        this.mesh_form.error = None;
                                        window.close_dialog(cx);
                                        this.sync(cx);
                                    }
                                    Err(err) => {
                                        this.mesh_form.error = Some(err.to_string());
                                        cx.notify();
                                    }
                                }
                            })),
                    ),
            )
    }

    pub(crate) fn page_mesh(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let busy = self.snapshot.busy.iter().any(|key| key == "mesh");
        let mut body = div().flex().flex_col().gap_4().w_full().min_w(px(0.));
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_start()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(section_title(
                                    &p,
                                    self.tr("异地组网", "Mesh networks"),
                                    self.tr(
                                        "管理 EasyTier 与 Tailscale 虚拟局域网连接",
                                        "Manage EasyTier and Tailscale virtual network connections",
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .justify_end()
                                .gap_2()
                                .when(!self.snapshot.mesh.tailscale_installed, |d| {
                                    d.child(
                                        Button::new("mesh-install")
                                            .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .label(self.tr("安装 Tailscale 组件", "Install Tailscale component"))
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service.mesh_install_tailscale();
                                            })),
                                    )
                                })
                                .when(!self.snapshot.platform.helper_available, |d| {
                                    d.child(
                                        Button::new("mesh-helper")
                                            .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .label(self.tr("启用提权助手", "Enable privilege helper"))
                                            .on_click(cx.listener(|this, _, _, _| {
                                                if this.snapshot.platform.helper_installed {
                                                    this.service.start_helper();
                                                } else {
                                                    this.service.install_helper();
                                                }
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("mesh-new")
                                        .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label(self.tr("添加网络", "Add network"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_mesh_dialog(None, window, cx);
                                        })),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(p.muted_fg)
                        .child(self.tr(
                            "支持多网并存与代理分流联动；主窗口关闭至托盘时保持连接，退出应用时自动断开。",
                            "Supports concurrent connections and proxy bypass integration. Connections stay active in the tray and disconnect on exit.",
                        )),
                )
                .when(!self.snapshot.mesh.supported, |d| {
                    d.child(empty_hint(
                        &p,
                        tr(
                            lang,
                            "当前平台不支持组网连接。",
                            "Mesh connections are unavailable on this platform.",
                        ),
                    ))
                }),
        );

        if let Some(review) = self.sync_review(&p, cx) {
            body = body.child(review);
        }
        for profile in self.snapshot.mesh.profiles.clone() {
            let provider = profile.provider;
            let id = profile.id.clone();
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
            let status_color = match state.phase {
                MeshPhase::Connected => p.ok_text,
                MeshPhase::Failed => p.danger_text,
                MeshPhase::Starting | MeshPhase::NeedsLogin | MeshPhase::Stopping => p.warn_text,
                MeshPhase::Disconnected => p.muted_fg,
            };
            let has_notice = active
                && cfg!(windows)
                && self.snapshot.settings.effective_tun()
                && self.snapshot.settings.tun_strict_route;
            let mut status_tips: Vec<String> = Vec::new();
            for message in [state.error, state.warning].into_iter().flatten() {
                let trimmed = message.trim();
                if !trimmed.is_empty() {
                    status_tips.push(trimmed.to_string());
                }
            }
            if has_notice {
                status_tips.push(
                    tr(
                        lang,
                        "TUN 严格路由可能影响组网 DNS；出现解析失败时可在设置中关闭。",
                        "TUN strict routing may affect mesh DNS. Disable it in settings if name resolution fails.",
                    )
                    .to_string(),
                );
            }
            let status_tip = (!status_tips.is_empty()).then(|| status_tips.join("\n"));
            let mut network = card(&p).child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap_2()
                            .child(div().min_w(px(0.)).truncate().child(profile.name.clone()))
                            .child(div().flex_shrink_0().child(pill(
                                provider.label(),
                                p.info,
                                p.info.opacity(0.1),
                            )))
                            .child(
                                div()
                                    .id(SharedString::from(format!("mesh-status-{}", profile.id)))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .text_xs()
                                    .text_color(status_color)
                                    .bg(status_color.opacity(0.1))
                                    .child(label)
                                    .when(has_notice, |d| {
                                        d.child(Icon::new(IconName::Info).size(px(13.)))
                                    })
                                    .when_some(status_tip, |d, tip| {
                                        d.tooltip(move |window, cx| {
                                            let tip = tip.clone();
                                            Tooltip::element(move |_, _| {
                                                div()
                                                    .max_w(px(320.))
                                                    .min_w(px(0.))
                                                    .whitespace_normal()
                                                    .child(tip.clone())
                                            })
                                            .build(window, cx)
                                        })
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .justify_end()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                more_button(
                                    SharedString::from(format!("mesh-menu-{id}")),
                                    tr(lang, "组网操作", "Network actions"),
                                )
                                .dropdown_menu(
                                    super::menus::builder(
                                        cx.entity().downgrade(),
                                        super::menus::MenuTarget::Mesh(id.clone()),
                                    ),
                                ),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "mesh-connect-{}",
                                    id.clone()
                                )))
                                .primary()
                                .disabled(busy || !self.snapshot.mesh.supported)
                                .label(if active {
                                    tr(lang, "断开", "Disconnect")
                                } else {
                                    tr(lang, "连接", "Connect")
                                })
                                .on_click(cx.listener(
                                    move |this, _, _, _| {
                                        if active {
                                            this.service.mesh_disconnect(provider);
                                        } else {
                                            this.service.mesh_connect(&id);
                                        }
                                    },
                                )),
                            ),
                    ),
            );
            let mut details = vec![
                (
                    "addresses",
                    self.tr("虚拟地址", "Addresses"),
                    state.addresses,
                ),
                (
                    "routes",
                    self.tr("网段与路由", "Networks and routes"),
                    state.routes,
                ),
            ];
            if let Some(suffix) = state.dns_suffix {
                details.push(("dns", "MagicDNS".into(), vec![format!("*.{suffix}")]));
            }
            details.retain(|(_, _, values)| !values.is_empty());
            if !details.is_empty() {
                network = network.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .w_full()
                        .min_w(px(0.))
                        .children(details.into_iter().map(|(key, label, values)| {
                            div()
                                .flex()
                                .items_start()
                                .gap_2()
                                .w_full()
                                .min_w(px(0.))
                                .text_sm()
                                .child(
                                    div()
                                        .py_1()
                                        .flex_shrink_0()
                                        .text_color(p.muted_fg)
                                        .child("•"),
                                )
                                .child(self.mesh_text(
                                    format!("mesh-{}-{key}", profile.id),
                                    &label,
                                    values,
                                    &p,
                                    cx,
                                ))
                        })),
                );
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
            let mut members = div().flex().flex_col().gap_2().w_full().min_w(px(0.));
            let has_members = !state.peers.is_empty();
            for peer in state.peers {
                let target = peer.addresses.first().cloned();
                let peer_id = format!("mesh-peer-{}-{}", profile.id, peer.id);
                let peer_group = SharedString::from(format!("{peer_id}-hover"));
                let expanded = self.mesh_expanded.contains(&peer_id);
                let badges = super::mesh_sync::badges(&peer, provider, lang, &p);
                let mut subnets = peer.subnets.clone();
                if let Some(x) = &peer.xross {
                    subnets.extend(x.subnets.clone());
                }
                subnets.sort();
                subnets.dedup();
                let receive = peer.xross.is_some() && peer.online;
                let source = (profile.id.clone(), peer.id.clone(), peer.name.clone());
                let hover_bg = p.panel_alt.opacity(0.45);
                members = members.child(
                    div()
                        .id(SharedString::from(peer_id.clone()))
                        .group(peer_group.clone())
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded_lg()
                        .bg(p.bg)
                        .border_1()
                        .border_color(p.border)
                        .hover(move |d| d.bg(hover_bg))
                        .child(dot(if peer.online { p.success } else { p.muted_fg }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .when(!expanded, |d| d.truncate())
                                        .when(expanded, |d| d.whitespace_normal())
                                        .child(SelectableText::new(
                                            SharedString::from(format!("{peer_id}-name")),
                                            peer.name,
                                        )),
                                )
                                .child(badges)
                                .when(!subnets.is_empty() && !self.hide_ips, |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(subnets.join(", ")),
                                    )
                                })
                                .when(!peer.addresses.is_empty(), |d| {
                                    d.child(div().text_xs().text_color(p.muted_fg).child(
                                        self.mesh_text(peer_id.clone(), "", peer.addresses, &p, cx),
                                    ))
                                })
                                .when(target.is_none(), |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .py_1()
                                            .min_h(px(26.))
                                            .child(tr(lang, "暂无 IP 地址", "No IP address")),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when_some(peer.latency_ms, |d, ms| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(format!("{ms:.1} ms")),
                                    )
                                })
                                .child(pill(
                                    if peer.online {
                                        tr(lang, "在线", "Online")
                                    } else {
                                        tr(lang, "离线", "Offline")
                                    },
                                    if peer.online { p.ok_text } else { p.muted_fg },
                                    if peer.online {
                                        p.ok_text.opacity(0.1)
                                    } else {
                                        p.panel_alt
                                    },
                                )),
                        )
                        .when(receive, |d| {
                            d.child(
                                Button::new(SharedString::from(format!("{peer_id}-receive")))
                                    .compact()
                                    .label(tr(lang, "接收配置", "Receive config"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.mesh_sync_ui.open_receive(
                                            source.0.clone(),
                                            source.1.clone(),
                                            source.2.clone(),
                                            window,
                                            cx,
                                        );
                                        cx.notify();
                                    })),
                            )
                        })
                        .child(div().w(px(26.)).h(px(26.)).flex_shrink_0().when_some(
                            target,
                            |d, ip| {
                                d.child(
                                    Button::new(SharedString::from(format!(
                                        "mesh-probe-{}",
                                        peer_id
                                    )))
                                    .compact()
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                    .ghost()
                                    .w(px(26.))
                                    .h(px(26.))
                                    .icon(IconName::Radar)
                                    .opacity(0.)
                                    .group_hover(peer_group, |s| s.opacity(1.))
                                    .text_color(p.muted_fg)
                                    .flex_shrink_0()
                                    .tooltip(tr(lang, "探测设备", "Probe device"))
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        this.service.mesh_probe(provider, ip.clone())
                                    })),
                                )
                            },
                        )),
                );
            }
            if state.phase == MeshPhase::Connected
                && let Some(panel) = self.sync_controls(&profile.id, &p, cx)
            {
                network = network.child(panel);
            }
            if has_members {
                network = network.child(members);
            }
            body = body.child(network);
        }
        if self.snapshot.mesh.profiles.is_empty() {
            body = body.child(empty_hint(
                &p,
                self.tr(
                    "暂无组网配置，点击“添加网络”创建",
                    "No mesh networks configured",
                ),
            ));
        }
        body
    }
}
