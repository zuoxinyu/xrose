//! Profile-scoped port forwarding overview and single-rule editor.
use super::*;
use crate::ui::{text_input, theme::CONTROL_HEIGHT};
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
    pub(super) fn mesh_profile_forwards(
        &self,
        profile: &MeshProfileView,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let rules: Vec<(bool, PortForward)> = profile
            .port_forwards
            .iter()
            .cloned()
            .map(|r| (false, r))
            .chain(
                profile
                    .service_forwards
                    .iter()
                    .map(|r| (true, local_rule(r))),
            )
            .collect();
        if rules.is_empty() {
            return div().py_2().text_xs().text_color(p.muted_fg).child(self.tr(
                "当前网络暂无端口转发规则，可转发至组网设备或向组网共享本机服务。",
                "No port forwarding rules for this network. Forward to mesh devices or share host services.",
            ));
        }
        let mut list = div().flex().flex_col();
        for (index, (service, rule)) in rules.into_iter().enumerate() {
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
                    xrose_core::privacy::mask_socket_addr(addr)
                } else {
                    addr.to_string()
                }
            };
            let source = if service {
                let addresses = self
                    .snapshot
                    .mesh
                    .networks
                    .iter()
                    .find(|n| n.profile_id == profile.id)
                    .map(|n| {
                        n.addresses
                            .iter()
                            .filter_map(|v| v.split('/').next()?.parse::<std::net::IpAddr>().ok())
                            .map(|ip| endpoint((ip, rule.listen.port()).into()))
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
            };
            list = list.child(
                div()
                    .id(SharedString::from(id.clone()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(index > 0, |d| d.border_t_1().border_color(p.border))
                    .py_1p5()
                    .child(
                        div()
                            .w(px(46.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .child(pill(rule.protocol.key().to_uppercase(), p.fg, p.panel_alt)),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .child(pill(
                                if service {
                                    self.tr("组网 → 本机", "Mesh → Host")
                                } else {
                                    self.tr("本机 → 组网", "Host → Mesh")
                                },
                                p.muted_fg,
                                p.panel_alt,
                            )),
                    )
                    .child(
                        div()
                            .w(px(192.))
                            .flex_shrink_0()
                            .min_w(px(0.))
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .whitespace_normal()
                            .child(source),
                    )
                    .child(
                        div()
                            .w(px(24.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(p.muted_fg)
                            .child("→"),
                    )
                    .child(
                        div()
                            .w(px(192.))
                            .flex_shrink_0()
                            .min_w(px(0.))
                            .text_sm()
                            .font_family(cx.theme().mono_font_family.clone())
                            .whitespace_normal()
                            .child(endpoint(rule.target)),
                    )
                    .child(div().flex_1().min_w(px(0.)))
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
        list
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
        let static_network = editor.original.is_some() || profiles.is_empty();
        let network_control = if !static_network {
            let ids: Vec<_> = profiles.iter().map(|p| p.id.clone()).collect();
            ChoiceGroup::new(
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
            }))
            .into_any_element()
        } else if let Some(index) = selected {
            div()
                .block()
                .w_full()
                .min_w(px(0.))
                .text_sm()
                .text_color(p.fg)
                .whitespace_normal()
                .child(network_label(&profiles[index]))
                .into_any_element()
        } else {
            div()
                .block()
                .w_full()
                .min_w(px(0.))
                .text_sm()
                .text_color(p.muted_fg)
                .whitespace_normal()
                .child(self.tr("网络已移除", "Network removed"))
                .into_any_element()
        };
        let protocol = ChoiceGroup::new(
            "forward-protocol",
            vec!["TCP".into(), "UDP".into()],
            usize::from(editor.protocol == ForwardProtocol::Udp),
        )
        .small()
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
        .small()
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
            .gap_2()
            .pb_1()
            .child(dialog_control_row(
                &p,
                self.tr("转发方向", "Direction"),
                "",
                direction,
            ))
            .child(dialog_form_row(
                &p,
                self.tr("所属网络", "Network"),
                "",
                network_control,
                if static_network {
                    RowWidth::Full
                } else {
                    RowWidth::Auto
                },
            ))
            .child(dialog_control_row(
                &p,
                self.tr("协议", "Protocol"),
                "",
                protocol,
            ))
            .child(dialog_field(
                &p,
                if service {
                    self.tr("组网服务端口", "Mesh service port")
                } else {
                    self.tr("本地监听地址和端口", "Local listen address and port")
                },
                &editor.listen,
                if service {
                    FieldWidth::Compact
                } else {
                    FieldWidth::Medium
                },
            ))
            .child(dialog_field_with_desc(
                &p,
                if service {
                    self.tr("本机服务地址和端口", "Host service address and port")
                } else {
                    self.tr("目标设备地址和端口", "Destination address and port")
                },
                if service {
                    self.tr(
                        "目标可填写回环或局域网 IP。组网成员通过本机虚拟地址访问此端口；连接期间保存立即生效。",
                        "Use a loopback or LAN IP as the target. Peers access this port on your mesh address. Changes apply immediately while connected.",
                    )
                } else {
                    self.tr(
                        "仅本机访问请监听 127.0.0.1；IPv6 地址请使用方括号。",
                        "Listen on 127.0.0.1 for local access only; enclose IPv6 addresses in brackets.",
                    )
                },
                &editor.target,
                FieldWidth::Medium,
            ));
        let actions = div()
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
                        let profile_id = editor.profile_id.clone();
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
                                &profile_id,
                                old.as_ref(),
                                Some(rule),
                            )
                        }) {
                            Ok(()) => {
                                super::activate_mesh_forwards_tab(
                                    &mut this.mesh_card_tabs,
                                    &mut this.collapsed_mesh_sections,
                                    &profile_id,
                                );
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
        crate::ui::dialogs::settings_dialog(
            dialog,
            if editor.original.is_some() {
                self.tr("编辑端口转发", "Edit port forward")
            } else {
                self.tr("添加端口转发", "Add port forward")
            },
            body,
            &p,
            window,
        )
        .overlay_closable(false)
        .footer(crate::ui::dialogs::dialog_footer(
            &p,
            editor.error.clone(),
            actions,
        ))
    }
}
