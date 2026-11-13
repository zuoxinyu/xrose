//! Profile-scoped port forwarding overview and single-rule editor.
use super::*;
use crate::ui::{
    text_input,
    theme::{CONTROL_HEIGHT, DIALOG_WIDTH},
};
use xrose_core::mesh::{ForwardProtocol, ForwardRule, PortForward, ServiceForward};

fn local_rule(rule: &ServiceForward) -> PortForward {
    PortForward {
        protocol: rule.protocol,
        listen: ([127, 0, 0, 1], rule.port).into(),
        target: rule.target,
    }
}
fn service_rule(rule: &PortForward) -> ServiceForward {
    ServiceForward {
        protocol: rule.protocol,
        port: rule.listen.port(),
        target: rule.target,
    }
}

pub(in crate::ui) struct ForwardEditor {
    profile_id: String,
    original: Option<PortForward>,
    service: bool,
    original_service: bool,
    network_scroll: ScrollHandle,
    protocol: ForwardProtocol,
    listen: Entity<InputState>,
    target: Entity<InputState>,
    error: Option<String>,
}

impl AppView {
    pub(super) fn forward_card(&self, p: &Palette, busy: bool, cx: &mut Context<Self>) -> Div {
        let profiles = &self.snapshot.mesh.profiles;
        let add = Button::new("mesh-forward-add")
            .compact()
            .h(px(CONTROL_HEIGHT))
            .primary()
            .icon(IconName::Plus)
            .label(self.tr("添加转发", "Add forward"))
            .disabled(busy || profiles.is_empty())
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(profile) = this.snapshot.mesh.profiles.first() {
                    this.open_forward_editor(profile.id.clone(), None, None, window, cx);
                }
            }));
        let mut panel = card(p).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(self.tr("端口转发", "Port forwarding")),
                )
                .child(add),
        );
        if self.snapshot.busy.iter().any(|key| key == "mesh-forward") {
            panel = panel.child(
                div()
                    .text_sm()
                    .text_color(p.muted_fg)
                    .child(self.tr("正在更新转发…", "Updating port forwards…")),
            );
        }
        if profiles
            .iter()
            .all(|p| p.port_forwards.is_empty() && p.service_forwards.is_empty())
        {
            return panel.child(div().text_sm().text_color(p.muted_fg).child(
                if profiles.is_empty() {
                    self.tr("先添加网络，再设置端口转发。", "Add a network to set up port forwarding.")
                } else {
                    self.tr("添加转发可访问组网设备，或将本机可达的服务提供给组网成员。", "Forward to mesh devices or share host-reachable services with mesh peers.")
                },
            ));
        }
        for profile in profiles
            .iter()
            .filter(|p| !p.port_forwards.is_empty() || !p.service_forwards.is_empty())
        {
            let mut group = div().flex().flex_col().gap_1().child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(profile.name.clone()),
                    )
                    .child(pill(profile.provider.label(), p.muted_fg, p.panel_alt)),
            );
            let rules = profile
                .port_forwards
                .iter()
                .cloned()
                .map(|r| (false, r))
                .chain(
                    profile
                        .service_forwards
                        .iter()
                        .map(|r| (true, local_rule(r))),
                );
            for (index, (service, rule)) in rules.enumerate() {
                let id = format!(
                    "mesh-{}-{}-{index}",
                    if service { "service" } else { "forward" },
                    profile.id
                );
                let menu = menus::builder(
                    cx.entity().downgrade(),
                    if service {
                        MenuTarget::ServiceForward(profile.id.clone(), service_rule(&rule))
                    } else {
                        MenuTarget::Forward(profile.id.clone(), rule.clone())
                    },
                );
                let endpoint = |addr: std::net::SocketAddr| {
                    if self.hide_ips {
                        format!("•••:{}", addr.port())
                    } else {
                        addr.to_string()
                    }
                };
                group = group.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_t_1()
                        .border_color(p.border)
                        .py_1()
                        .child(pill(rule.protocol.key().to_uppercase(), p.fg, p.panel_alt))
                        .child(pill(
                            if service {
                                self.tr("组网 → 本机", "Mesh → Host")
                            } else {
                                self.tr("本机 → 组网", "Host → Mesh")
                            },
                            p.muted_fg,
                            p.panel_alt,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .whitespace_normal()
                                .child(format!(
                                    "{} → {}",
                                    if service {
                                        let addresses = self
                                            .snapshot
                                            .mesh
                                            .networks
                                            .iter()
                                            .find(|n| n.profile_id == profile.id)
                                            .map(|n| {
                                                n.addresses
                                                    .iter()
                                                    .filter_map(|v| {
                                                        v.split('/')
                                                            .next()?
                                                            .parse::<std::net::IpAddr>()
                                                            .ok()
                                                    })
                                                    .map(|ip| {
                                                        endpoint((ip, rule.listen.port()).into())
                                                    })
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default();
                                        if addresses.is_empty() {
                                            format!(
                                                "{}:{}",
                                                self.tr("本机虚拟地址", "Device mesh address"),
                                                rule.listen.port()
                                            )
                                        } else {
                                            addresses.join(", ")
                                        }
                                    } else {
                                        endpoint(rule.listen)
                                    },
                                    endpoint(rule.target)
                                )),
                        )
                        .child(
                            more_button(
                                SharedString::from(format!("{id}-menu")),
                                self.tr("转发操作", "Forward actions"),
                                cx,
                            )
                            .dropdown_menu(menu.clone()),
                        )
                        .context_menu(menu),
                );
            }
            panel = panel.child(group);
        }
        panel
    }

    pub(in crate::ui) fn open_forward_editor(
        &mut self,
        profile_id: String,
        original: Option<ForwardRule>,
        target_ip: Option<std::net::IpAddr>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (original, service) = match original {
            Some(ForwardRule::ToMesh(rule)) => (Some(rule), false),
            Some(ForwardRule::ToHost(rule)) => (Some(local_rule(&rule)), true),
            None => (None, false),
        };
        let listen = text_input(window, cx, if service { "8080" } else { "127.0.0.1:8080" });
        let target = text_input(window, cx, "192.0.2.2:80");
        if let Some(rule) = &original {
            listen.update(cx, |input, cx| {
                input.set_value(
                    if service {
                        rule.listen.port().to_string()
                    } else {
                        rule.listen.to_string()
                    },
                    window,
                    cx,
                )
            });
            target.update(cx, |input, cx| {
                input.set_value(rule.target.to_string(), window, cx)
            });
        } else if let Some(ip) = target_ip {
            let endpoint = match ip {
                std::net::IpAddr::V4(ip) => format!("{ip}:"),
                std::net::IpAddr::V6(ip) => format!("[{ip}]:"),
            };
            target.update(cx, |input, cx| input.set_value(endpoint, window, cx));
        }
        let protocol = original
            .as_ref()
            .map_or(ForwardProtocol::Tcp, |r| r.protocol);
        self.forward_editor = Some(ForwardEditor {
            profile_id,
            original,
            service,
            original_service: service,
            network_scroll: ScrollHandle::new(),
            protocol,
            listen,
            target,
            error: None,
        });
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| this.forward_dialog(dialog, window, cx))
        });
        cx.notify();
    }

    fn forward_dialog(&self, dialog: Dialog, window: &Window, cx: &mut Context<Self>) -> Dialog {
        let Some(editor) = self.forward_editor.as_ref() else {
            return dialog;
        };
        let p = palette(cx);
        let profiles = &self.snapshot.mesh.profiles;
        let selected = profiles.iter().position(|p| p.id == editor.profile_id);
        let service = editor.service;
        let network_label = |p: &MeshProfileView| format!("{} · {}", p.name, p.provider.label());
        let mut network = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(self.tr("所属网络", "Network")));
        if editor.original.is_none() && !profiles.is_empty() {
            let ids: Vec<_> = profiles.iter().map(|p| p.id.clone()).collect();
            let choices = ChoiceGroup::new(
                "forward-network",
                profiles.iter().map(|p| network_label(p).into()).collect(),
                selected,
            )
            .cards()
            .scrollable(&editor.network_scroll, px(CONTROL_HEIGHT * 4.))
            .on_change(cx.listener(move |this, index, _, cx| {
                if let (Some(editor), Some(id)) = (&mut this.forward_editor, ids.get(*index)) {
                    editor.profile_id.clone_from(id);
                    editor.error = None;
                    cx.notify();
                }
            }));
            network = network.child(choices);
        } else if let Some(index) = selected {
            network = network.child(
                div()
                    .text_sm()
                    .text_color(p.muted_fg)
                    .child(network_label(&profiles[index])),
            );
        } else {
            network = network.child(
                div()
                    .text_sm()
                    .text_color(p.muted_fg)
                    .child(self.tr("网络已移除", "Network removed")),
            );
        }
        let field = |label: SharedString, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child(label))
                .child(Input::new(input))
        };
        let protocol = ChoiceGroup::new(
            "forward-protocol",
            vec!["TCP".into(), "UDP".into()],
            usize::from(editor.protocol == ForwardProtocol::Udp),
        )
        .on_change(cx.listener(|this, index, _, cx| {
            if let Some(edit) = &mut this.forward_editor {
                edit.protocol = if *index == 0 {
                    ForwardProtocol::Tcp
                } else {
                    ForwardProtocol::Udp
                };
                cx.notify();
            }
        }));
        let direction = ChoiceGroup::new(
            "forward-direction",
            vec![
                self.tr("本机 → 组网", "Host → Mesh"),
                self.tr("组网 → 本机", "Mesh → Host"),
            ],
            usize::from(service),
        )
        .on_change(cx.listener(|this, index, window, cx| {
            if let Some(editor) = &mut this.forward_editor {
                let service = *index == 1;
                if service == editor.service {
                    return;
                }
                let value = editor.listen.read(cx).value();
                let next = if service {
                    value
                        .trim()
                        .parse::<std::net::SocketAddr>()
                        .map(|a| a.port().to_string())
                        .unwrap_or_default()
                } else {
                    value
                        .trim()
                        .parse::<u16>()
                        .map(|port| format!("127.0.0.1:{port}"))
                        .unwrap_or_default()
                };
                editor.service = service;
                editor.error = None;
                editor.listen.update(cx, |input, cx| {
                    input.set_placeholder(
                        if service { "8080" } else { "127.0.0.1:8080" },
                        window,
                        cx,
                    );
                    input.set_value(next, window, cx);
                });
                cx.notify();
            }
        }));
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .pb_1()
            .child(div().flex().flex_col().gap_1()
                .child(div().text_sm().child(self.tr("转发方向", "Direction")))
                .child(direction))
            .child(network)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.tr("协议", "Protocol"))
                    .child(protocol),
            )
            .child(field(
                if service { self.tr("组网服务端口", "Mesh service port") } else { self.tr("本地监听地址和端口", "Local listen address and port") },
                &editor.listen,
            ))
            .child(field(
                if service { self.tr("本机可达的服务地址和端口", "Host-reachable service address and port") } else { self.tr("目标设备地址和端口", "Destination address and port") },
                &editor.target,
            ))
            .child(div().text_xs().text_color(p.muted_fg).child(if service { self.tr(
                "目标可填写回环或局域网 IP。组网成员通过本机虚拟地址访问此端口；连接期间保存立即生效。",
                "Use a loopback or LAN IP as the target. Peers access this port on your mesh address. Changes apply immediately while connected."
            ) } else { self.tr(
                "仅本机访问请监听 127.0.0.1。IPv6 地址使用方括号。",
                "Listen on 127.0.0.1 for local access only. Enclose IPv6 addresses in brackets.",
            ) }))
            .when_some(editor.error.clone(), |d, error| {
                d.child(div().text_sm().text_color(p.danger_text).child(error))
            });
        let footer = div()
            .flex()
            .justify_end()
            .gap_2()
            .child(
                Button::new("forward-cancel")
                    .label(self.tr("取消", "Cancel"))
                    .on_click(cx.listener(|_, _, window, cx| window.close_dialog(cx))),
            )
            .child(
                Button::new("forward-save")
                    .primary()
                    .label(self.tr("保存", "Save"))
                    .disabled(selected.is_none() || self.snapshot.busy.iter().any(|s| s == "mesh"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let Some(editor) = &mut this.forward_editor else {
                            return;
                        };
                        let listen = editor.listen.read(cx).value();
                        let target = editor.target.read(cx).value();
                        let rule = PortForward::parse(&format!(
                            "{}://{}/{}",
                            editor.protocol.key(),
                            if editor.service {
                                format!("127.0.0.1:{}", listen.trim())
                            } else {
                                listen.trim().to_owned()
                            },
                            target.trim()
                        ));
                        match rule.and_then(|r| {
                            let old = editor.original.as_ref().map(|r| {
                                if editor.original_service {
                                    ForwardRule::ToHost(service_rule(r))
                                } else {
                                    ForwardRule::ToMesh(r.clone())
                                }
                            });
                            let rule = if editor.service {
                                ForwardRule::ToHost(service_rule(&r))
                            } else {
                                ForwardRule::ToMesh(r)
                            };
                            this.service.edit_mesh_forward_rule(
                                &editor.profile_id,
                                old.as_ref(),
                                Some(rule),
                            )
                        }) {
                            Ok(()) => {
                                window.close_dialog(cx);
                                this.sync(cx);
                            }
                            Err(e) => {
                                editor.error = Some(e.to_string());
                                cx.notify();
                            }
                        }
                    })),
            );
        dialog
            .centered()
            .w(px(DIALOG_WIDTH))
            .bg(p.panel)
            .text_color(p.fg)
            .border_color(p.border)
            .entrance(crate::ui::dialogs::DIALOG_ENTRANCE)
            .overlay_closable(false)
            .on_ok(|_, _, _| false)
            .title(if editor.original.is_some() {
                self.tr("编辑端口转发", "Edit port forward")
            } else {
                self.tr("添加端口转发", "Add port forward")
            })
            .child(crate::ui::dialogs::dialog_scroll_body(
                "forward-editor-body",
                window.viewport_size().height * 0.62,
                body,
            ))
            .footer(footer)
    }
}
