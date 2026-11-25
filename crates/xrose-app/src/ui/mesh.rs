//! Mesh configuration and cached runtime state. All work goes through AppService.
mod advanced;
mod forwards;
use super::{
    AppView,
    menus::{self, MenuTarget},
    theme::{Palette, SEMANTIC_ALPHA, palette},
    tr,
    widgets::*,
};
pub(super) use forwards::ForwardEditor;
use gpui_base::{Disableable, SelectableText};
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::InputState;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::mesh::{
    EasyTierOptions, ExitRoutingMode, MeshPeer, MeshPhase, MeshProfile, MeshProfileView,
    MeshProvider, MeshRuntimeView, MeshSecret,
};
use xrose_core::service::ToastLevel;

const PROBE_FEEDBACK_DURATION: std::time::Duration = std::time::Duration::from_secs(6);

#[derive(Default)]
pub(super) struct ProbeFeedback {
    results: std::collections::HashMap<String, (xrose_core::mesh::MeshProbe, std::time::Instant)>,
}

impl ProbeFeedback {
    pub fn clear(&mut self, profile: &str) {
        self.results.remove(profile);
    }

    pub fn observe(&mut self, mesh: &xrose_core::mesh::MeshSnapshot, now: std::time::Instant) {
        self.results.retain(|id, _| {
            mesh.networks.iter().any(|network| {
                network.profile_id == *id
                    && network.phase == MeshPhase::Connected
                    && network.probe.is_some()
            })
        });
        for network in mesh
            .networks
            .iter()
            .filter(|network| network.phase == MeshPhase::Connected)
        {
            if let Some(probe) = &network.probe {
                let entry = self
                    .results
                    .entry(network.profile_id.clone())
                    .or_insert_with(|| (probe.clone(), now));
                if entry.0 != *probe {
                    *entry = (probe.clone(), now);
                }
            }
        }
    }

    fn for_peer(
        &self,
        profile: &str,
        peer: &xrose_core::mesh::MeshPeer,
        now: std::time::Instant,
    ) -> Option<&xrose_core::mesh::MeshProbe> {
        let (probe, observed) = self.results.get(profile)?;
        (now.saturating_duration_since(*observed) < PROBE_FEEDBACK_DURATION
            && peer.addresses.contains(&probe.ip))
        .then_some(probe)
    }
}

pub(crate) struct MeshForm {
    editing: bool,
    advanced_open: bool,
    easytier: EasyTierOptions,
    mtu: Entity<InputState>,
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
    service_forwards: Vec<xrose_core::mesh::ServiceForward>,
    outbound_port: Entity<InputState>,
    advertise_exit: bool,
    exit_routing: ExitRoutingMode,
    connect_on_startup: bool,
    auto_reconnect: bool,
}

impl MeshForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        Self {
            editing: false,
            advanced_open: false,
            easytier: Default::default(),
            mtu: super::text_input(window, cx, "400–1380"),
            error: None,
            id: MeshProfile::default().id,
            provider: MeshProvider::EasyTier,
            name: super::text_input(window, cx, ""),
            hostname: super::text_input(window, cx, ""),
            network: super::text_input(window, cx, ""),
            secret: cx.new(|cx| InputState::new(window, cx).masked(true)),
            peers: super::text_input(window, cx, "tcp://192.0.2.1:11010"),
            address: super::text_input(window, cx, "10.126.126.2/24"),
            subnets: super::text_input(window, cx, "192.168.50.0/24"),
            control: super::text_input(window, cx, "https://controlplane.tailscale.com"),
            exit_node: super::text_input(window, cx, "peer-host / 10.126.126.1"),
            advertised: super::text_input(window, cx, "192.168.10.0/24"),
            forwards: super::text_input(window, cx, "tcp://127.0.0.1:8080/10.126.126.2:80"),
            service_forwards: Vec::new(),
            outbound_port: super::text_input(window, cx, "1024–65535"),
            advertise_exit: false,
            exit_routing: ExitRoutingMode::Direct,
            connect_on_startup: false,
            auto_reconnect: true,
        }
    }

    fn load(&mut self, profile: MeshProfileView, window: &mut Window, cx: &mut Context<AppView>) {
        self.service_forwards = profile.service_forwards.clone();
        self.advanced_open = false;
        self.easytier = profile.easytier;
        self.id = profile.id;
        self.provider = profile.provider;
        self.advertise_exit = profile.advertise_exit_node;
        self.exit_routing = profile.exit_routing;
        self.auto_reconnect = profile.auto_reconnect;
        self.error = None;
        for (input, value) in [
            (
                &self.mtu,
                self.easytier.mtu.map(|n| n.to_string()).unwrap_or_default(),
            ),
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

    fn profile(&self, cx: &Context<AppView>) -> xrose_core::Result<MeshProfile> {
        let value = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let list = |input: &Entity<InputState>| -> Vec<String> {
            value(input)
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        let easytier = if self.provider == MeshProvider::EasyTier {
            let mut options = self.easytier.clone();
            let mtu = value(&self.mtu);
            options.mtu = if mtu.is_empty() {
                None
            } else {
                Some(mtu.parse().map_err(|_| {
                    xrose_core::Error::config("EasyTier MTU 必须为 400–1380，或留空使用默认值")
                })?)
            };
            options
        } else {
            EasyTierOptions::default()
        };
        Ok(MeshProfile {
            easytier,
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
            auto_reconnect: self.auto_reconnect,
            exit_node: value(&self.exit_node),
            advertise_exit_node: self.advertise_exit,
            exit_routing: self.exit_routing,
            advertised_subnets: list(&self.advertised),
            port_forwards: list(&self.forwards)
                .iter()
                .map(|v| xrose_core::mesh::PortForward::parse(v))
                .collect::<xrose_core::Result<Vec<_>>>()?,
            outbound_port: if value(&self.outbound_port).is_empty() {
                0
            } else {
                value(&self.outbound_port)
                    .parse::<u16>()
                    .map_err(|_| xrose_core::Error::config("出口出站端口必须为 1024–65535"))?
            },
            outbound_secret: MeshSecret::default(),
            service_forwards: self.service_forwards.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum MeshCardTab {
    #[default]
    Devices,
    Forwards,
}

fn select_mesh_card_tab(
    tabs: &mut std::collections::HashMap<String, MeshCardTab>,
    collapsed: &mut std::collections::HashSet<String>,
    profile_id: &str,
    target: MeshCardTab,
) {
    let current = tabs.get(profile_id).copied().unwrap_or_default();
    if !collapsed.contains(profile_id) && current == target {
        collapsed.insert(profile_id.to_owned());
    } else {
        collapsed.remove(profile_id);
        tabs.insert(profile_id.to_owned(), target);
    }
}

pub(super) fn activate_mesh_forwards_tab(
    tabs: &mut std::collections::HashMap<String, MeshCardTab>,
    collapsed: &mut std::collections::HashSet<String>,
    profile_id: &str,
) {
    collapsed.remove(profile_id);
    tabs.insert(profile_id.to_owned(), MeshCardTab::Forwards);
}

fn toggle_mesh_section(collapsed: &mut std::collections::HashSet<String>, profile_id: &str) {
    if !collapsed.remove(profile_id) {
        collapsed.insert(profile_id.to_owned());
    }
}

fn empty_grid_field(label: SharedString, p: &Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(0.))
        .child(div().text_xs().text_color(p.muted_fg).child(label))
        .child(div().text_sm().text_color(p.muted_fg).py_1().child("—"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MeshTextKind {
    Plain,
    MagicDns,
    Address,
    Routes,
}

impl AppView {
    fn mesh_text(
        &self,
        id: String,
        label: &str,
        values: Vec<String>,
        kind: MeshTextKind,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let overflow = self.mesh_overflow.get(&id).copied().unwrap_or(false);
        let expanded = overflow && self.mesh_expanded.contains(&id);
        let text_sample = overflow.then(|| {
            self.expand_motion.sample_variant(
                &format!("mesh-text-{id}"),
                true,
                u8::from(expanded),
                window,
                cx,
            )
        });
        let chevron_progress = overflow
            .then(|| {
                self.expand_motion
                    .sample(&format!("mesh-text-chevron-{id}"), expanded, window, cx)
                    .progress
            })
            .unwrap_or(0.0);
        let copy_value = values.join(", ");
        let display_values: Vec<String> = if self.hide_ips {
            match kind {
                MeshTextKind::Address | MeshTextKind::Routes => values
                    .iter()
                    .map(|v| xrose_core::privacy::mask_ip_or_cidr(v))
                    .collect(),
                MeshTextKind::MagicDns => values
                    .iter()
                    .map(|v| xrose_core::privacy::mask_magic_dns_suffix(v))
                    .collect(),
                MeshTextKind::Plain => {
                    let suffixes = self.snapshot.mesh.magic_dns_suffixes();
                    values
                        .iter()
                        .map(|v| xrose_core::privacy::mask_magic_dns_name(v, &suffixes))
                        .collect()
                }
            }
        } else {
            values
        };
        let summary = display_values.join(", ");
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
        let mut disclosure = overflow.then(|| {
            let id = id.clone();
            let label = if expanded {
                self.tr("收起", "Collapse")
            } else {
                self.tr("展开", "Expand")
            };
            Button::new(SharedString::from(format!("{id}-expand")))
                .compact()
                .custom(icon_button_style(p, false, cx))
                .w(px(26.))
                .h(px(26.))
                .icon(Icon::new(IconName::ChevronRight).rotate(percentage(0.25 * chevron_progress)))
                .text_color(p.muted_fg)
                .flex_shrink_0()
                .accessibility_label(label.clone())
                .tooltip(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.mesh_expanded.remove(&id) {
                        this.mesh_expanded.insert(id.clone());
                    }
                    cx.notify();
                }))
        });
        // Keep the trailing disclosure aligned for addresses and routes so leading values stay flush.
        let has_label = !label.is_empty();
        let trailing_disclosure = matches!(kind, MeshTextKind::Address | MeshTextKind::Routes);
        let fill_width = expanded || (trailing_disclosure && overflow);
        let value_body =
            div()
                .min_w(px(0.))
                .py_1()
                .when(!expanded, |d| d.truncate().child(summary))
                .when(expanded, |d| {
                    d.flex().flex_col().gap_1().children(
                        display_values
                            .into_iter()
                            .enumerate()
                            .map(|(index, value)| {
                                div()
                                    .min_w(px(0.))
                                    .whitespace_normal()
                                    .child(SelectableText::new(
                                        SharedString::from(format!("{id}-value-{index}")),
                                        value,
                                    ))
                            }),
                    )
                });
        let value_slot = if let Some(sample) = text_sample {
            div().min_w(px(0.)).flex_1().child(sample.wrap(
                SharedString::from(format!("mesh-text-reveal-{id}")),
                value_body,
            ))
        } else {
            value_body.when(fill_width, |d| d.flex_1())
        };
        let content = div()
            .id(SharedString::from(format!("{id}-content")))
            .group(group.clone())
            .flex()
            .items_start()
            .gap_1()
            .max_w_full()
            .min_w(px(0.))
            .when(fill_width, |d| d.w_full())
            .children(disclosure.take_if(|_| !trailing_disclosure))
            .child(value_slot)
            .children(disclosure)
            .child(
                Button::new(SharedString::from(format!("{id}-copy")))
                    .compact()
                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                    .custom(icon_button_style(p, false, cx))
                    .w(px(26.))
                    .h(px(26.))
                    .icon(IconName::Copy)
                    .text_color(p.muted_fg)
                    .flex_shrink_0()
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .tooltip(self.tr("复制", "Copy"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_value.clone()));
                        this.service
                            .toast(ToastLevel::Success, this.tr("已复制", "Copied"));
                    })),
            );
        let text = div()
            .relative()
            .flex()
            .flex_1()
            .w_full()
            .min_w(px(0.))
            .when(has_label, |d| {
                d.text_sm().font_weight(FontWeight::MEDIUM).text_color(p.fg)
            })
            .when(
                matches!(kind, MeshTextKind::Address | MeshTextKind::Routes),
                |d| d.font_family(cx.theme().mono_font_family.clone()),
            )
            .child(measure)
            .child(content);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .when(has_label, |d| {
                d.gap_0p5().child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
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
        window.open_dialog(cx, move |dialog, window, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |this, cx| this.mesh_dialog(dialog, window, cx))
            } else {
                dialog
            }
        });
        cx.notify();
    }

    fn mesh_dialog(&mut self, dialog: Dialog, window: &Window, cx: &mut Context<Self>) -> Dialog {
        let p = palette(cx);
        let busy = self.snapshot.busy.iter().any(|key| key == "mesh");
        let toolbar = div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                ChoiceGroup::new(
                    "mesh-provider",
                    vec!["EasyTier".into(), "Tailscale".into()],
                    usize::from(self.mesh_form.provider == MeshProvider::Tailscale),
                )
                .disabled(self.mesh_form.editing || busy)
                .on_change(cx.listener(|this, index: &usize, _, cx| {
                    if this.mesh_form.editing || this.snapshot.busy.iter().any(|key| key == "mesh")
                    {
                        return;
                    }
                    this.mesh_form.advanced_open = false;
                    this.mesh_form.provider = if *index == 0 {
                        MeshProvider::EasyTier
                    } else {
                        MeshProvider::Tailscale
                    };
                    cx.notify();
                })),
            )
            .when(self.mesh_form.provider == MeshProvider::EasyTier, |bar| {
                bar.child(
                    Button::new("mesh-advanced")
                        .compact()
                        .ghost()
                        .disabled(busy)
                        .icon(if self.mesh_form.advanced_open {
                            IconName::ChevronLeft
                        } else {
                            IconName::Settings
                        })
                        .label(if self.mesh_form.advanced_open {
                            self.tr("基本设置", "Basic settings")
                        } else {
                            self.tr("高级设置", "Advanced settings")
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mesh_form.advanced_open = !this.mesh_form.advanced_open;
                            cx.notify();
                        })),
                )
            });
        let body = if self.mesh_form.advanced_open
            && self.mesh_form.provider == MeshProvider::EasyTier
        {
            self.easytier_advanced_settings(&p, cx)
        } else {
            let startup = toggle(
                "mesh-connect-on-startup",
                self.tr("随应用启动连接", "Connect when app starts"),
                self.mesh_form.connect_on_startup,
            )
            .on_change(cx.listener(|this, checked, _, cx| {
                this.mesh_form.connect_on_startup = *checked;
                cx.notify();
            }));
            let reconnect = toggle(
                "mesh-auto-reconnect",
                self.tr(
                    "意外断开后自动重连",
                    "Reconnect after an unexpected disconnect",
                ),
                self.mesh_form.auto_reconnect,
            )
            .on_change(cx.listener(|this, checked, _, cx| {
                this.mesh_form.auto_reconnect = *checked;
                cx.notify();
            }));
            let advertise_exit = toggle(
                "mesh-advertise-exit",
                self.tr(
                    "发布本机为出口节点",
                    "Advertise this device as an exit node",
                ),
                self.mesh_form.advertise_exit,
            )
            .on_change(cx.listener(|this, checked, _, cx| {
                this.mesh_form.advertise_exit = *checked;
                cx.notify();
            }));

            let mut form = div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w(px(0.))
                .pb_1()
                .child(dialog_section_heading(&p, self.tr("基础", "Basics")))
                .child(dialog_field(
                    &p,
                    self.tr("配置名称", "Profile name"),
                    &self.mesh_form.name,
                    FieldWidth::Medium,
                ))
                .child(dialog_field(
                    &p,
                    self.tr("设备名称", "Device hostname"),
                    &self.mesh_form.hostname,
                    FieldWidth::Medium,
                ))
                .child(dialog_control_row(
                    &p,
                    self.tr("启动连接", "Startup"),
                    self.tr(
                        "每种组网类型可选择一个启动连接配置。",
                        "Select one startup profile for each network type.",
                    ),
                    startup,
                ))
                .child(dialog_control_row(
                    &p,
                    self.tr("自动重连", "Auto reconnect"),
                    "",
                    reconnect,
                ))
                .child(divider(&p))
                .child(dialog_section_heading(
                    &p,
                    self.tr("网络参数", "Network settings"),
                ));

            if self.mesh_form.provider == MeshProvider::EasyTier {
                form = form
                    .child(dialog_field(
                        &p,
                        self.tr("网络名称", "Network name"),
                        &self.mesh_form.network,
                        FieldWidth::Medium,
                    ))
                    .child(dialog_field(
                        &p,
                        if self.mesh_form.editing {
                            self.tr("网络密钥（留空保留）", "Network secret (blank to keep)")
                        } else {
                            self.tr("网络密钥", "Network secret")
                        },
                        &self.mesh_form.secret,
                        FieldWidth::Full,
                    ))
                    .child(dialog_field_with_desc(
                        &p,
                        self.tr("入口节点", "Entry peers"),
                        self.tr(
                            "多个地址用逗号分隔。",
                            "Separate multiple peers with commas.",
                        ),
                        &self.mesh_form.peers,
                        FieldWidth::Full,
                    ))
                    .child(dialog_field_with_desc(
                        &p,
                        self.tr("虚拟 IPv4（可选）", "Virtual IPv4 (optional)"),
                        self.tr("留空自动分配。", "Leave blank for automatic assignment."),
                        &self.mesh_form.address,
                        FieldWidth::Medium,
                    ));
            } else {
                form = form.child(dialog_field_with_desc(
                    &p,
                    self.tr("控制服务器（可选）", "Control server (optional)"),
                    self.tr(
                        "留空使用 Tailscale 官方控制端；保存后连接并在浏览器中完成授权，断开连接保留登录状态。",
                        "Leave blank for Tailscale. Connect after saving to authorize in your browser; disconnecting preserves sign-in.",
                    ),
                    &self.mesh_form.control,
                    FieldWidth::Full,
                ));
            }

            let advertised_desc = if self.mesh_form.provider == MeshProvider::Tailscale {
                self.tr(
                    "多个网段用逗号分隔；发布子网与出口需在控制端批准。",
                    "Comma-separated CIDRs; advertised subnets and exit nodes require control-plane approval.",
                )
            } else {
                self.tr(
                    "多个网段用逗号分隔。",
                    "Separate multiple CIDRs with commas.",
                )
            };

            form = form
                .child(divider(&p))
                .child(dialog_section_heading(
                    &p,
                    self.tr("路由与转发", "Routing & forwarding"),
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("远端网段（可选）", "Remote subnets (optional)"),
                    self.tr(
                        "允许访问的远端网段，多个用逗号分隔。",
                        "Allowed remote CIDRs, comma-separated.",
                    ),
                    &self.mesh_form.subnets,
                    FieldWidth::Full,
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("本机发布子网（可选）", "Advertised subnets (optional)"),
                    advertised_desc,
                    &self.mesh_form.advertised,
                    FieldWidth::Full,
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("出口节点（可选）", "Exit node (optional)"),
                    self.tr("填写对端设备名或虚拟 IP。", "Peer device name or virtual IP."),
                    &self.mesh_form.exit_node,
                    FieldWidth::Medium,
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("出口出站端口（可选）", "Exit outbound port (optional)"),
                    self.tr(
                        "留空禁用；填写后在节点列表生成可用于出站组和分流规则的出口节点。",
                        "Leave blank to disable. Setting a port adds an exit node for outbound groups and routing rules.",
                    ),
                    &self.mesh_form.outbound_port,
                    FieldWidth::Compact,
                ))
                .child(dialog_field_with_desc(
                    &p,
                    self.tr("端口转发（可选）", "Port forwards (optional)"),
                    self.tr(
                        "多个用逗号分隔，格式：tcp://监听IP:端口/目标IP:端口（支持 udp）。",
                        "Comma-separated: tcp://listen-IP:port/target-IP:port (udp supported).",
                    ),
                    &self.mesh_form.forwards,
                    FieldWidth::Full,
                ))
                .child(dialog_control_row(
                    &p,
                    self.tr("发布本机出口", "Advertise exit"),
                    "",
                    advertise_exit,
                ))
                .when(self.mesh_form.advertise_exit, |d| {
                    d.child(dialog_control_row(
                        &p,
                        self.tr("出口流量处理", "Exit traffic"),
                        self.tr(
                            "按规则分流使用当前配置的路由与 DNS，独立于本机工作模式。",
                            "Routing uses this profile's rules and DNS independently of the local work mode.",
                        ),
                        ChoiceGroup::new(
                            "mesh-exit-routing",
                            vec![
                                self.tr("直接转发", "Forward directly"),
                                self.tr("按规则分流", "Apply routing rules"),
                            ],
                            usize::from(self.mesh_form.exit_routing != ExitRoutingMode::Direct),
                        )
                        .small()
                        .on_change(cx.listener(|this, index, _, cx| {
                            this.mesh_form.exit_routing = if *index == 0 {
                                ExitRoutingMode::Direct
                            } else {
                                ExitRoutingMode::Rules
                            };
                            cx.notify();
                        })),
                    ))
                });
            form
        };
        let view = cx.entity().downgrade();
        let title = if self.mesh_form.editing {
            self.tr("编辑网络", "Edit network")
        } else {
            self.tr("添加网络", "Add network")
        };
        super::dialogs::dialog_shell(dialog, title, super::theme::DIALOG_WIDE, &p)
            .overlay_closable(false)
            .on_close(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.mesh_form
                        .secret
                        .update(cx, |s, cx| s.set_value("", window, cx));
                    this.mesh_form.error = None;
                    cx.notify();
                });
            })
            .child(super::dialogs::dialog_toolbar(&p, toolbar))
            .child(super::dialogs::dialog_scroll_body(
                if self.mesh_form.advanced_open {
                    "mesh-advanced-scroll"
                } else {
                    "mesh-editor-scroll"
                },
                px(520.).min(window.viewport_size().height * 0.60),
                body,
            ))
            .footer(super::dialogs::dialog_footer(
                &p,
                self.mesh_form.error.clone(),
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
            ))
    }

    fn mesh_basic_info_grid(
        &self,
        profile: &MeshProfileView,
        state: &MeshRuntimeView,
        active: bool,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let local_name = if state.local_hostname.trim().is_empty() {
            profile.hostname.as_str()
        } else {
            state.local_hostname.as_str()
        };
        let show_full_grid = active
            && (state.phase == MeshPhase::Connected
                || !state.addresses.is_empty()
                || !state.routes.is_empty()
                || state.dns_suffix.is_some());
        if !show_full_grid && local_name.is_empty() {
            return None;
        }
        let mut grid = div()
            .grid()
            .grid_cols(4)
            .gap_4()
            .w_full()
            .min_w(px(0.))
            .child(
                div()
                    .col_span(1)
                    .min_w(px(0.))
                    .child(if local_name.is_empty() {
                        empty_grid_field(self.tr("本机设备名", "Local device name"), p)
                            .into_any_element()
                    } else {
                        self.mesh_text(
                            format!("mesh-{}-local-name", profile.id),
                            &self.tr("本机设备名", "Local device name"),
                            vec![local_name.to_owned()],
                            MeshTextKind::Plain,
                            p,
                            window,
                            cx,
                        )
                        .into_any_element()
                    }),
            );
        if show_full_grid {
            grid = grid
                .child(
                    div()
                        .col_span(1)
                        .min_w(px(0.))
                        .child(if state.addresses.is_empty() {
                            empty_grid_field(self.tr("虚拟地址", "Addresses"), p).into_any_element()
                        } else {
                            self.mesh_text(
                                format!("mesh-{}-addresses", profile.id),
                                &self.tr("虚拟地址", "Addresses"),
                                state.addresses.clone(),
                                MeshTextKind::Address,
                                p,
                                window,
                                cx,
                            )
                            .into_any_element()
                        }),
                )
                .child(
                    div()
                        .col_span(2)
                        .min_w(px(0.))
                        .child(if state.routes.is_empty() {
                            empty_grid_field(self.tr("网段与路由", "Networks and routes"), p)
                                .into_any_element()
                        } else {
                            self.mesh_text(
                                format!("mesh-{}-routes", profile.id),
                                &self.tr("网段与路由", "Networks and routes"),
                                state.routes.clone(),
                                MeshTextKind::Routes,
                                p,
                                window,
                                cx,
                            )
                            .into_any_element()
                        }),
                );
            if let Some(suffix) = &state.dns_suffix {
                grid = grid.child(div().col_span_full().min_w(px(0.)).child(self.mesh_text(
                    format!("mesh-{}-dns", profile.id),
                    "MagicDNS",
                    vec![format!("*.{suffix}")],
                    MeshTextKind::MagicDns,
                    p,
                    window,
                    cx,
                )));
            }
        }
        Some(grid)
    }

    fn mesh_peers_list(
        &self,
        profile_id: &str,
        provider: MeshProvider,
        active: bool,
        peers: &[MeshPeer],
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let lang = self.lang();
        if peers.is_empty() {
            return div()
                .py_2()
                .text_xs()
                .text_color(p.muted_fg)
                .child(if active {
                    self.tr(
                        "当前网络暂无已连接设备",
                        "No connected devices in this network",
                    )
                } else {
                    self.tr(
                        "连接网络后可查看成员设备",
                        "Connect to view network devices",
                    )
                });
        }
        let suffixes = self.snapshot.mesh.magic_dns_suffixes();
        let mut members = div().flex().flex_col().gap_2().w_full().min_w(px(0.));
        for peer in peers {
            let has_address = !peer.addresses.is_empty();
            let peer_id = format!("mesh-peer-{profile_id}-{}", peer.id);
            let expanded = self.mesh_expanded.contains(&peer_id);
            let mut badges = super::mesh_sync::badges(peer, provider, lang, p);
            if let Some(probe) =
                self.mesh_probe_feedback
                    .for_peer(profile_id, peer, std::time::Instant::now())
            {
                let color = if probe.success {
                    p.ok_text
                } else {
                    p.danger_text
                };
                let label = if probe.success {
                    match probe.latency_ms {
                        Some(ms) => {
                            format!("{} {ms:.1} ms", tr(lang, "设备延迟", "Device latency"))
                        }
                        None => tr(lang, "设备可达", "Device reachable").to_string(),
                    }
                } else {
                    tr(lang, "探测失败", "Probe failed").to_string()
                };
                let detail = if self.hide_ips {
                    label.clone()
                } else {
                    probe.detail.clone()
                };
                badges = badges.child(
                    div()
                        .id(SharedString::from(format!("{peer_id}-probe")))
                        .child(pill(label, color, color.opacity(SEMANTIC_ALPHA)))
                        .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx)),
                );
            }
            let mut subnets = peer.subnets.clone();
            if let Some(x) = &peer.xrose {
                subnets.extend(x.subnets.clone());
            }
            subnets.sort();
            subnets.dedup();
            let menu = menus::builder(
                cx.entity().downgrade(),
                MenuTarget::MeshPeer(profile_id.to_owned(), peer.id.clone()),
            );
            let hover_bg = p.row_hover();
            members = members.child(
                div()
                    .id(SharedString::from(peer_id.clone()))
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .bg(p.row_bg())
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
                                        if self.hide_ips {
                                            xrose_core::privacy::mask_magic_dns_name(
                                                &peer.name, &suffixes,
                                            )
                                        } else {
                                            peer.name.clone()
                                        },
                                    )),
                            )
                            .child(badges)
                            .when(!subnets.is_empty(), |d| {
                                let subnet_text = if self.hide_ips {
                                    subnets
                                        .iter()
                                        .map(|s| xrose_core::privacy::mask_ip_or_cidr(s))
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                } else {
                                    subnets.join(", ")
                                };
                                d.child(div().text_xs().text_color(p.muted_fg).child(subnet_text))
                            })
                            .when(has_address, |d| {
                                d.child(div().text_xs().text_color(p.muted_fg).child(
                                    self.mesh_text(
                                        peer_id.clone(),
                                        "",
                                        peer.addresses.clone(),
                                        MeshTextKind::Address,
                                        p,
                                        window,
                                        cx,
                                    ),
                                ))
                            })
                            .when(!has_address, |d| {
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
                                d.child(div().text_xs().text_color(p.muted_fg).child(format!(
                                    "{} {ms:.1} ms",
                                    tr(lang, "设备延迟", "Device latency")
                                )))
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
                    .child(
                        more_button(
                            SharedString::from(format!("{peer_id}-menu")),
                            tr(lang, "设备操作", "Device actions"),
                            cx,
                        )
                        .dropdown_menu(menu.clone()),
                    )
                    .context_menu(menu),
            );
        }
        members
    }

    fn mesh_card_subsection(
        &self,
        profile: &MeshProfileView,
        provider: MeshProvider,
        active: bool,
        busy: bool,
        state: &MeshRuntimeView,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let peer_count = state.peers.len();
        let forward_count = profile.port_forwards.len() + profile.service_forwards.len();
        let connected = state.phase == MeshPhase::Connected;
        if !connected && peer_count == 0 && forward_count == 0 {
            return None;
        }
        let active_tab = self
            .mesh_card_tabs
            .get(&profile.id)
            .copied()
            .unwrap_or_default();
        let collapsed = self.collapsed_mesh_sections.contains(&profile.id);
        let tab_variant = match active_tab {
            MeshCardTab::Devices => 0,
            MeshCardTab::Forwards => 1,
        };
        let section_sample = self.expand_motion.sample_variant(
            &format!("mesh-section-{}", profile.id),
            !collapsed,
            tab_variant,
            window,
            cx,
        );
        let profile_id = profile.id.clone();
        let selected_tab = (!collapsed).then_some(match active_tab {
            MeshCardTab::Devices => 0,
            MeshCardTab::Forwards => 1,
        });
        let toggle_profile_id = profile.id.clone();
        let toggle_label = if collapsed {
            self.tr("展开", "Expand")
        } else {
            self.tr("收起", "Collapse")
        };
        let toggle_tip = if collapsed {
            match active_tab {
                MeshCardTab::Devices => self.tr("展开设备列表", "Expand device list"),
                MeshCardTab::Forwards => self.tr("展开端口转发", "Expand port forwards"),
            }
        } else {
            match active_tab {
                MeshCardTab::Devices => self.tr("收起设备列表", "Collapse device list"),
                MeshCardTab::Forwards => self.tr("收起端口转发", "Collapse port forwards"),
            }
        };
        let add_forward_profile_id = profile.id.clone();
        let right_actions = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .pl_2()
            .when(!collapsed && active_tab == MeshCardTab::Forwards, |d| {
                d.child(
                    Button::new(SharedString::from(format!(
                        "mesh-card-add-forward-{}",
                        profile.id
                    )))
                    .compact()
                    .ghost()
                    .h(px(28.))
                    .border_1()
                    .border_color(p.border)
                    .icon(IconName::Plus)
                    .label(self.tr("添加转发", "Add forward"))
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_forward_editor(
                            add_forward_profile_id.clone(),
                            None,
                            None,
                            window,
                            cx,
                        );
                    })),
                )
            })
            .child(
                Button::new(SharedString::from(format!(
                    "mesh-section-toggle-{}",
                    profile.id
                )))
                .compact()
                .ghost()
                .h(px(28.))
                .accessibility_label(toggle_tip.clone())
                .tooltip(toggle_tip)
                .label(toggle_label)
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(px(14.))
                        .rotate(percentage(0.5 * section_sample.progress)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    toggle_mesh_section(&mut this.collapsed_mesh_sections, &toggle_profile_id);
                    cx.notify();
                })),
            );
        let devices_label = self.tr("设备列表", "Devices");
        let forwards_label = self.tr("端口转发", "Port forwarding");
        // Keep device list on the left and port forwarding on the right.
        let tabs = TabBar::new(SharedString::from(format!("mesh-card-tabs-{profile_id}")))
            .underline()
            .small()
            .w_full()
            .when_some(selected_tab, |bar, idx| bar.selected_index(idx))
            .children([
                Tab::new().aria_label(devices_label.clone()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(devices_label)
                        .child(tab_count_badge(
                            peer_count.to_string(),
                            selected_tab == Some(0),
                            p,
                            cx,
                        )),
                ),
                Tab::new().aria_label(forwards_label.clone()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(forwards_label)
                        .child(tab_count_badge(
                            forward_count.to_string(),
                            selected_tab == Some(1),
                            p,
                            cx,
                        )),
                ),
            ])
            .suffix(right_actions)
            .on_click(cx.listener(move |this, index: &usize, _, cx| {
                let Some(tab) = [MeshCardTab::Devices, MeshCardTab::Forwards]
                    .get(*index)
                    .copied()
                else {
                    return;
                };
                select_mesh_card_tab(
                    &mut this.mesh_card_tabs,
                    &mut this.collapsed_mesh_sections,
                    &profile_id,
                    tab,
                );
                cx.notify();
            }));
        let mut section = div().flex().flex_col().pt_1().child(tabs);
        if section_sample.mounted {
            let content = match active_tab {
                MeshCardTab::Devices => {
                    self.mesh_peers_list(&profile.id, provider, active, &state.peers, p, window, cx)
                }
                MeshCardTab::Forwards => self.mesh_profile_forwards(profile, p, cx),
            };
            section = section.child(section_sample.wrap(
                SharedString::from(format!("mesh-section-reveal-{}", profile.id)),
                div().pt_3().w_full().child(content),
            ));
        }
        Some(section)
    }

    pub(crate) fn page_mesh(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let busy = self.snapshot.busy.iter().any(|key| key == "mesh");
        let forward_busy = self.snapshot.busy.iter().any(|key| key == "mesh-forward");
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
                        .child(if cfg!(target_os = "macos") {
                            self.tr(
                                "支持多网并存与代理分流联动；主窗口关闭至菜单栏时保持连接，退出应用时自动断开。",
                                "Supports concurrent connections and proxy bypass integration. Connections stay active in the menu bar and disconnect on exit.",
                            )
                        } else {
                            self.tr(
                                "支持多网并存与代理分流联动；主窗口关闭至托盘时保持连接，退出应用时自动断开。",
                                "Supports concurrent connections and proxy bypass integration. Connections stay active in the tray and disconnect on exit.",
                            )
                        }),
                )
                .when(forward_busy, |d| {
                    d.child(
                        div()
                            .text_sm()
                            .text_color(p.muted_fg)
                            .child(self.tr("正在更新转发…", "Updating port forwards…")),
                    )
                })
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
                    | MeshPhase::NeedsApproval
                    | MeshPhase::Connected
                    | MeshPhase::Stopping
            );
            let connection_error = self
                .snapshot
                .mesh_connection_errors
                .get(&id)
                .cloned()
                .or_else(|| state.error.clone());
            let display_phase = if !active && connection_error.is_some() {
                MeshPhase::Failed
            } else {
                state.phase
            };
            let retrying = self.snapshot.mesh_reconnecting.contains(&id);
            let label = match display_phase {
                _ if retrying => tr(lang, "等待重连", "Waiting to reconnect"),
                MeshPhase::Disconnected => tr(lang, "未连接", "Disconnected"),
                MeshPhase::Starting => tr(lang, "正在连接", "Connecting"),
                MeshPhase::NeedsApproval => tr(lang, "等待管理员批准", "Awaiting device approval"),
                MeshPhase::NeedsLogin if state.login_url.is_some() => {
                    tr(lang, "等待登录", "Sign-in required")
                }
                MeshPhase::NeedsLogin => tr(lang, "正在获取登录链接", "Waiting for sign-in link"),
                MeshPhase::Connected => tr(lang, "已连接", "Connected"),
                MeshPhase::Stopping => tr(lang, "正在断开", "Disconnecting"),
                MeshPhase::Failed => tr(lang, "连接失败", "Failed"),
            };
            let status_color = match display_phase {
                _ if retrying => p.warn_text,
                MeshPhase::Connected => p.ok_text,
                MeshPhase::Failed => p.danger_text,
                MeshPhase::Starting
                | MeshPhase::NeedsLogin
                | MeshPhase::NeedsApproval
                | MeshPhase::Stopping => p.warn_text,
                MeshPhase::Disconnected => p.muted_fg,
            };
            let has_notice = active
                && cfg!(windows)
                && self.snapshot.settings.effective_tun()
                && self.snapshot.settings.tun_strict_route;
            let mut status_tips: Vec<String> = Vec::new();
            for message in [state.error.as_deref(), state.warning.as_deref()]
                .into_iter()
                .flatten()
            {
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
            let needs_auth = display_phase == MeshPhase::NeedsLogin;
            let needs_approval = display_phase == MeshPhase::NeedsApproval;
            let official_control = profile.control_url.is_empty()
                || profile.control_url.trim_end_matches('/')
                    == "https://controlplane.tailscale.com";
            let auth_action = if needs_approval && official_control {
                Some((
                    tr(lang, "打开设备列表", "Open Machines"),
                    "https://login.tailscale.com/admin/machines".to_owned(),
                ))
            } else if needs_auth {
                state
                    .login_url
                    .clone()
                    .map(|url| (tr(lang, "在浏览器登录", "Sign in with browser"), url))
            } else {
                None
            };
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
                            .when_some(auth_action, |d, (label, url)| {
                                d.child(
                                    Button::new(SharedString::from(format!("mesh-auth-{id}")))
                                        .label(label)
                                        .on_click(cx.listener(move |this, _, _, _| {
                                            this.service.open_url(&url)
                                        })),
                                )
                            })
                            .child(
                                more_button(
                                    SharedString::from(format!("mesh-menu-{id}")),
                                    tr(lang, "组网操作", "Network actions"),
                                    cx,
                                )
                                .dropdown_menu(menus::builder(
                                    cx.entity().downgrade(),
                                    MenuTarget::Mesh(id.clone()),
                                )),
                            )
                            .child(
                                Button::new(SharedString::from(format!("mesh-connect-{id}")))
                                    .map(|button| {
                                        if active {
                                            button
                                                .custom(
                                                    ButtonCustomVariant::new(cx)
                                                        .color(p.warning.opacity(0.1))
                                                        .foreground(p.warn_text)
                                                        .hover(p.warning.opacity(SEMANTIC_ALPHA))
                                                        .active(p.warning.opacity(SEMANTIC_ALPHA)),
                                                )
                                                .outline()
                                                .border_color(p.warn_text)
                                                .label(tr(lang, "断开", "Disconnect"))
                                        } else if retrying {
                                            button.outline().label(tr(
                                                lang,
                                                "取消重连",
                                                "Cancel reconnect",
                                            ))
                                        } else {
                                            button.primary().label(tr(lang, "连接", "Connect"))
                                        }
                                    })
                                    .disabled((busy && !retrying) || !self.snapshot.mesh.supported)
                                    .on_click(cx.listener(move |this, _, _, _| {
                                        if active {
                                            this.service.mesh_disconnect(provider);
                                        } else if retrying {
                                            this.service.cancel_mesh_reconnect(&id);
                                        } else {
                                            this.service.mesh_connect(&id);
                                        }
                                    })),
                            ),
                    ),
            );
            let detail = if retrying && self.snapshot.platform.helper_needs_update {
                Some(tr(
                    lang,
                    "更新提权助手后将自动重连。",
                    "Reconnection will resume after the helper is updated.",
                ))
            } else if retrying && !self.snapshot.platform.helper_available {
                Some(tr(
                    lang,
                    "提权助手就绪后将自动重连。",
                    "Reconnection will resume when the helper is ready.",
                ))
            } else {
                connection_error.map(Into::into)
            };
            if let Some(reason) = detail {
                network = network.child(
                    div()
                        .text_sm()
                        .text_color(if retrying { p.warn_text } else { p.danger_text })
                        .whitespace_normal()
                        .child(reason),
                );
            }
            if active {
                let auth_hint = if needs_approval && official_control {
                    Some(tr(
                        lang,
                        "请管理员在 Machines 页面找到标有 Needs approval 的设备，选择 Approve。",
                        "Ask an administrator to open Machines and select Approve for the device marked Needs approval.",
                    ))
                } else if needs_approval {
                    Some(tr(
                        lang,
                        "请联系此控制服务器的管理员批准设备。",
                        "Ask the administrator of this control server to approve the device.",
                    ))
                } else if needs_auth && state.login_url.is_none() {
                    Some(tr(
                        lang,
                        "如果迟迟没有登录链接，请检查控制服务器地址和网络连接，然后断开并重新连接。",
                        "If the sign-in link does not appear, check the control server address and connectivity, then disconnect and reconnect.",
                    ))
                } else {
                    None
                };
                if let Some(hint) = auth_hint {
                    network = network.child(div().text_sm().text_color(p.muted_fg).child(hint));
                }
            }
            if let Some(info_grid) =
                self.mesh_basic_info_grid(&profile, &state, active, &p, window, cx)
            {
                network = network.child(info_grid);
            }
            if let Some(subsection) =
                self.mesh_card_subsection(&profile, provider, active, busy, &state, &p, window, cx)
            {
                network = network.child(subsection);
            }
            body = body.child(network);
        }
        if self.snapshot.mesh.profiles.is_empty() {
            body = body.child(empty_hint(
                &p,
                self.tr(
                    "暂无组网配置，点击「添加网络」创建",
                    "No mesh networks configured",
                ),
            ));
        }
        body
    }
}

#[cfg(test)]
mod probe_feedback_tests {
    use super::{PROBE_FEEDBACK_DURATION, ProbeFeedback};
    use std::time::{Duration, Instant};
    use xrose_core::mesh::{MeshPeer, MeshPhase, MeshProbe, MeshRuntimeView, MeshSnapshot};

    #[test]
    fn polling_does_not_extend_feedback_and_results_stay_on_the_target_device() {
        let now = Instant::now();
        let peer = MeshPeer {
            addresses: vec!["192.0.2.2".into()],
            ..Default::default()
        };
        let other_peer = MeshPeer {
            addresses: vec!["192.0.2.3".into()],
            ..Default::default()
        };
        let mesh = MeshSnapshot {
            networks: vec![MeshRuntimeView {
                profile_id: "test-network".into(),
                phase: MeshPhase::Connected,
                probe: Some(MeshProbe {
                    ip: "192.0.2.2".into(),
                    success: true,
                    latency_ms: Some(12.0),
                    detail: "设备可达".into(),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut feedback = ProbeFeedback::default();
        feedback.observe(&mesh, now);
        assert!(feedback.for_peer("test-network", &peer, now).is_some());
        assert!(
            feedback
                .for_peer("test-network", &other_peer, now)
                .is_none()
        );
        assert!(feedback.for_peer("other-network", &peer, now).is_none());
        feedback.observe(&mesh, now + Duration::from_secs(5));
        assert!(
            feedback
                .for_peer("test-network", &peer, now + PROBE_FEEDBACK_DURATION)
                .is_none()
        );

        feedback.clear("test-network");
        feedback.observe(&mesh, now + Duration::from_secs(10));
        assert!(
            feedback
                .for_peer("test-network", &peer, now + Duration::from_secs(10))
                .is_some()
        );
    }

    #[test]
    fn changed_or_cleared_results_restart_feedback_and_disconnect_removes_it() {
        let now = Instant::now();
        let peer = MeshPeer {
            addresses: vec!["192.0.2.2".into()],
            ..Default::default()
        };
        let mut mesh = MeshSnapshot {
            networks: vec![MeshRuntimeView {
                profile_id: "test-network".into(),
                phase: MeshPhase::Connected,
                probe: Some(MeshProbe {
                    ip: "192.0.2.2".into(),
                    success: false,
                    latency_ms: None,
                    detail: "探测失败".into(),
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut feedback = ProbeFeedback::default();
        feedback.observe(&mesh, now);
        assert!(
            !feedback
                .for_peer("test-network", &peer, now)
                .unwrap()
                .success
        );
        mesh.networks[0].probe.as_mut().unwrap().success = true;
        feedback.observe(&mesh, now + Duration::from_secs(8));
        assert!(
            feedback
                .for_peer("test-network", &peer, now + Duration::from_secs(8))
                .unwrap()
                .success
        );
        let completed = mesh.networks[0].probe.take();
        feedback.observe(&mesh, now + Duration::from_secs(9));
        assert!(
            feedback
                .for_peer("test-network", &peer, now + Duration::from_secs(9))
                .is_none()
        );
        mesh.networks[0].probe = completed;
        feedback.observe(&mesh, now + Duration::from_secs(10));
        assert!(
            feedback
                .for_peer("test-network", &peer, now + Duration::from_secs(10))
                .is_some()
        );
        mesh.networks[0].phase = MeshPhase::Disconnected;
        feedback.observe(&mesh, now + Duration::from_secs(11));
        assert!(feedback.results.is_empty());
    }
}

#[cfg(test)]
mod card_tab_tests {
    use super::{
        MeshCardTab, activate_mesh_forwards_tab, select_mesh_card_tab, toggle_mesh_section,
    };
    use std::collections::{HashMap, HashSet};

    #[test]
    fn default_tab_is_always_devices() {
        assert_eq!(MeshCardTab::default(), MeshCardTab::Devices);
    }

    #[test]
    fn clicking_tabs_and_toggle_controls_collapse_and_expand() {
        let mut tabs = HashMap::new();
        let mut collapsed = HashSet::new();
        let id = "net-1";

        // Clicking the currently active expanded tab collapses the section.
        select_mesh_card_tab(&mut tabs, &mut collapsed, id, MeshCardTab::Devices);
        assert!(collapsed.contains(id));

        // Clicking another tab expands the section and switches to that tab.
        select_mesh_card_tab(&mut tabs, &mut collapsed, id, MeshCardTab::Forwards);
        assert!(!collapsed.contains(id));
        assert_eq!(tabs.get(id), Some(&MeshCardTab::Forwards));

        // Toggling collapses while keeping the active tab selected.
        toggle_mesh_section(&mut collapsed, id);
        assert!(collapsed.contains(id));
        assert_eq!(tabs.get(id), Some(&MeshCardTab::Forwards));

        // Toggling again restores expansion on the remembered tab.
        toggle_mesh_section(&mut collapsed, id);
        assert!(!collapsed.contains(id));
        assert_eq!(tabs.get(id), Some(&MeshCardTab::Forwards));

        // Saving a forward expands and selects the forwards tab.
        collapsed.insert(id.to_owned());
        tabs.insert(id.to_owned(), MeshCardTab::Devices);
        activate_mesh_forwards_tab(&mut tabs, &mut collapsed, id);
        assert!(!collapsed.contains(id));
        assert_eq!(tabs.get(id), Some(&MeshCardTab::Forwards));
    }
}
