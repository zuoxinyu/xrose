//! State and input row for the "add node by hand" form.
//!
//! The form only collects text; [`xross_core::model::NodeDraft`] owns the
//! validation rules so the CLI and the UI cannot drift apart.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::model::{NodeDraft, Protocol, TransportKind};

use super::theme::{palette, Palette};
use super::widgets::*;
use super::AppView;

pub(crate) struct ManualNodeForm {
    pub name: Entity<InputState>,
    pub server: Entity<InputState>,
    pub port: Entity<InputState>,
    pub uuid: Entity<InputState>,
    pub password: Entity<InputState>,
    pub username: Entity<InputState>,
    pub method: Entity<InputState>,
    pub flow: Entity<InputState>,
    pub sni: Entity<InputState>,
    pub alpn: Entity<InputState>,
    pub fingerprint: Entity<InputState>,
    pub reality_public_key: Entity<InputState>,
    pub reality_short_id: Entity<InputState>,
    pub path: Entity<InputState>,
    pub host: Entity<InputState>,
    pub service_name: Entity<InputState>,
    pub tags: Entity<InputState>,
    pub protocol: Protocol,
    pub transport: TransportKind,
    pub tls: bool,
    pub insecure: bool,
    pub udp: bool,
}

impl ManualNodeForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        let text = |window: &mut Window, cx: &mut Context<AppView>, placeholder: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_string()))
        };
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("443")
                .default_value("443")
        });
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("/ws")
                .default_value("/")
        });
        Self {
            name: text(window, cx, "名称（留空自动使用 地址:端口）"),
            server: text(window, cx, "example.com 或 1.2.3.4"),
            port,
            uuid: text(window, cx, "UUID / 用户 ID"),
            password: text(window, cx, "密码"),
            username: text(window, cx, "用户名（可选）"),
            method: text(window, cx, "aes-256-gcm"),
            flow: text(window, cx, "xtls-rprx-vision（可选）"),
            sni: text(window, cx, "TLS SNI（留空使用服务器地址）"),
            alpn: text(window, cx, "h2,http/1.1（可选）"),
            fingerprint: text(window, cx, "chrome（可选）"),
            reality_public_key: text(window, cx, "Reality 公钥 pbk"),
            reality_short_id: text(window, cx, "Reality shortId（可选）"),
            path,
            host: text(window, cx, "WS/H2 Host（可选）"),
            service_name: text(window, cx, "gRPC serviceName"),
            tags: text(window, cx, "标签，逗号分隔（可选）"),
            protocol: Protocol::Vless,
            transport: TransportKind::Tcp,
            tls: true,
            insecure: false,
            udp: true,
        }
    }

    /// Builds the draft. Only the port needs parsing here; every other rule
    /// (credentials, Reality, transport combinations) lives in the core.
    pub fn draft(&self, cx: &App) -> std::result::Result<NodeDraft, String> {
        let port_text = self.port.read(cx).value().trim().to_string();
        let port = port_text
            .parse::<u16>()
            .map_err(|_| format!("端口无效：{port_text}（应为 1-65535）"))?;
        Ok(NodeDraft {
            name: self.name.read(cx).value().to_string(),
            protocol: self.protocol,
            server: self.server.read(cx).value().to_string(),
            port,
            uuid: self.uuid.read(cx).value().to_string(),
            password: self.password.read(cx).value().to_string(),
            username: self.username.read(cx).value().to_string(),
            method: self.method.read(cx).value().to_string(),
            flow: self.flow.read(cx).value().to_string(),
            tls_enabled: self.tls,
            sni: self.sni.read(cx).value().to_string(),
            alpn: self.alpn.read(cx).value().to_string(),
            fingerprint: self.fingerprint.read(cx).value().to_string(),
            insecure: self.insecure,
            reality_public_key: self.reality_public_key.read(cx).value().to_string(),
            reality_short_id: self.reality_short_id.read(cx).value().to_string(),
            transport: self.transport,
            transport_path: self.path.read(cx).value().to_string(),
            transport_host: self.host.read(cx).value().to_string(),
            transport_service_name: self.service_name.read(cx).value().to_string(),
            udp: self.udp,
            tags: self.tags.read(cx).value().to_string(),
        })
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        self.protocol = Protocol::Vless;
        self.transport = TransportKind::Tcp;
        self.tls = true;
        self.insecure = false;
        self.udp = true;
        let inputs: [(&Entity<InputState>, &str); 17] = [
            (&self.name, ""),
            (&self.server, ""),
            (&self.port, "443"),
            (&self.uuid, ""),
            (&self.password, ""),
            (&self.username, ""),
            (&self.method, ""),
            (&self.flow, ""),
            (&self.sni, ""),
            (&self.alpn, ""),
            (&self.fingerprint, ""),
            (&self.reality_public_key, ""),
            (&self.reality_short_id, ""),
            (&self.path, "/"),
            (&self.host, ""),
            (&self.service_name, ""),
            (&self.tags, ""),
        ];
        for (input, value) in inputs {
            let window = &mut *window;
            let cx = &mut *cx;
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }
}

/// One parameter per row: fixed label column on the left, control filling the
/// remaining width. Horizontal packing made multi-field rows unreadable once
/// the window was scaled (150% DPI) or narrowed.
fn field(
    p: &Palette,
    label: impl Into<SharedString>,
    input: Entity<InputState>,
    _grow: bool,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .max_w(px(900.))
        .child(
            div()
                .w(px(150.))
                .text_xs()
                .text_color(p.muted_fg)
                .child(label.into()),
        )
        .child(div().flex_1().child(Input::new(&input)))
}

fn section_label(p: &Palette, label: impl Into<SharedString>) -> impl IntoElement {
    div()
        .pt_2()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.fg)
        .child(label.into())
}

impl AppView {
    pub(crate) fn manual_node_card(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let form = &self.manual_form;
        let protocol = form.protocol;
        let transport = form.transport;
        let tls = form.tls;
        let insecure = form.insecure;
        let udp = form.udp;
        let error = self.manual_error.clone();

        let needs_uuid = matches!(protocol, Protocol::Vmess | Protocol::Vless | Protocol::Tuic);
        let needs_password = matches!(
            protocol,
            Protocol::Trojan
                | Protocol::Hysteria2
                | Protocol::Shadowsocks
                | Protocol::Tuic
                | Protocol::Socks
                | Protocol::Http
        );
        let needs_username = matches!(protocol, Protocol::Socks | Protocol::Http);
        let has_credentials =
            needs_uuid || needs_password || needs_username || protocol == Protocol::Vless;
        let tls_capable = !protocol.is_plain_proxy();
        let transport_capable = !matches!(protocol, Protocol::Hysteria2 | Protocol::Tuic);

        let mut credentials = div().flex().flex_col().gap_2();
        if needs_uuid {
            credentials = credentials.child(field(
                &p,
                if protocol == Protocol::Tuic {
                    self.tr("UUID（TUIC 用户 ID）", "UUID")
                } else {
                    self.tr("UUID / 用户 ID", "UUID / user id")
                },
                form.uuid.clone(),
                true,
            ));
        }
        if protocol == Protocol::Vless {
            credentials = credentials.child(field(
                &p,
                self.tr("Flow（可选）", "Flow (optional)"),
                form.flow.clone(),
                true,
            ));
        }
        if protocol == Protocol::Shadowsocks {
            credentials = credentials.child(field(
                &p,
                self.tr("加密方式", "Cipher"),
                form.method.clone(),
                true,
            ));
        }
        if needs_username {
            credentials = credentials.child(field(
                &p,
                self.tr("用户名", "Username"),
                form.username.clone(),
                true,
            ));
        }
        if needs_password {
            credentials = credentials.child(field(
                &p,
                if needs_username {
                    self.tr("密码", "Password")
                } else {
                    self.tr("密码 / 密钥", "Password")
                },
                form.password.clone(),
                true,
            ));
        }

        let transport_fields = match transport {
            TransportKind::WebSocket | TransportKind::Http2 | TransportKind::HttpUpgrade => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(field(
                    &p,
                    self.tr("路径 Path", "Path"),
                    form.path.clone(),
                    true,
                ))
                .child(field(
                    &p,
                    self.tr("Host / CDN（可选）", "Host / CDN (optional)"),
                    form.host.clone(),
                    true,
                )),
            TransportKind::Grpc => div().child(field(
                &p,
                self.tr("serviceName", "serviceName"),
                form.service_name.clone(),
                true,
            )),
            _ => div().text_xs().text_color(p.muted_fg).child(self.tr(
                "TCP 直连，无需额外传输参数",
                "Plain TCP: no extra transport options",
            )),
        };

        let card = card(&p)
            .border_color(p.primary.opacity(0.35))
            .child(                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(section_title(
                        &p,
                        self.tr("手动添加节点", "Add node manually"),
                        self.tr(
                            "与订阅并行：保存后自动加入默认出站组，凭据不会上传",
                            "Independent of subscriptions: saved into the default groups, credentials stay local",
                        ),
                    ))
                    .child(
                        Button::new("close-manual-form")
                            .compact()
                            .ghost()
                            .label("✕")
                            .tooltip(self.tr("收起", "Close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_manual_form = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr("协议", "Protocol")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(Protocol::manual().into_iter().map(|candidate| {
                                let selected = protocol == candidate;
                                div()
                                    .id(SharedString::from(format!(
                                        "manual-protocol-{}",
                                        candidate.as_str()
                                    )))
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .text_xs()
                                    .border_1()
                                    .border_color(if selected { p.primary } else { p.border })
                                    .bg(if selected { p.accent } else { p.bg })
                                    .cursor_pointer()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.manual_form.protocol = candidate;
                                        this.manual_error = None;
                                        cx.notify();
                                    }))
                                    .child(candidate.display_name())
                            })),
                    ),
            )
            .child(section_label(&p, self.tr("基础", "Basics")))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(field(
                        &p,
                        self.tr("名称（可选）", "Name (optional)"),
                        form.name.clone(),
                        true,
                    ))
                    .child(field(
                        &p,
                        self.tr("服务器", "Server"),
                        form.server.clone(),
                        true,
                    ))
                    .child(field(
                        &p,
                        self.tr("端口", "Port"),
                        form.port.clone(),
                        false,
                    )),
            )
            .when(has_credentials, |d| {
                d.child(section_label(&p, self.tr("凭据", "Credentials")))
                    .child(credentials)
            })
            .when(tls_capable, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(section_label(&p, self.tr("TLS / Reality", "TLS / Reality")))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_4()
                                .child(
                                    toggle("manual-tls", "TLS", tls).on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.manual_form.tls = *checked;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    toggle(
                                        "manual-insecure",
                                        self.tr("允许不安全证书", "Allow insecure"),
                                        insecure,
                                    )
                                    .on_change(cx.listener(
                                        |this, checked, _, cx| {
                                            this.manual_form.insecure = *checked;
                                            cx.notify();
                                        },
                                    )),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(field(
                                    &p,
                                    self.tr("SNI（可选）", "SNI (optional)"),
                                    form.sni.clone(),
                                    true,
                                ))
                                .child(field(
                                    &p,
                                    self.tr("ALPN（逗号分隔）", "ALPN (comma separated)"),
                                    form.alpn.clone(),
                                    true,
                                ))
                                .child(field(
                                    &p,
                                    self.tr("uTLS 指纹（可选）", "uTLS fingerprint"),
                                    form.fingerprint.clone(),
                                    true,
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(field(
                                    &p,
                                    self.tr("Reality 公钥 pbk", "Reality public key"),
                                    form.reality_public_key.clone(),
                                    true,
                                ))
                                .child(field(
                                    &p,
                                    self.tr("Reality shortId（可选）", "Reality shortId"),
                                    form.reality_short_id.clone(),
                                    true,
                                )),
                        )
                        .child(div().text_xs().text_color(p.muted_fg).child(self.tr(
                            "Reality：填写公钥即启用（自动开启 TLS），shortId 可留空",
                            "Reality turns on as soon as a public key is filled in; shortId may stay empty",
                        ))),
                )
            })
            .when(transport_capable, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(section_label(&p, self.tr("传输层", "Transport")))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .children(TransportKind::all().into_iter().map(|candidate| {
                                    let selected = transport == candidate;
                                    div()
                                        .id(SharedString::from(format!(
                                            "manual-transport-{}",
                                            candidate.as_str()
                                        )))
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .text_xs()
                                        .border_1()
                                        .border_color(if selected {
                                            p.primary
                                        } else {
                                            p.border
                                        })
                                        .bg(if selected { p.accent } else { p.bg })
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.manual_form.transport = candidate;
                                            cx.notify();
                                        }))
                                        .child(candidate.display_name())
                                })),
                        )
                        .child(transport_fields),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        toggle("manual-udp", self.tr("启用 UDP", "UDP"), udp).on_change(
                            cx.listener(|this, checked, _, cx| {
                                this.manual_form.udp = *checked;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(field(
                        &p,
                        self.tr("标签（逗号分隔，可选）", "Tags (comma separated)"),
                        form.tags.clone(),
                        true,
                    )),
            )
            .when(error.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .bg(p.accent)
                        .border_1()
                        .border_color(p.danger)
                        .text_xs()
                        .text_color(p.danger)
                        .child(error.clone().unwrap_or_default()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("save-manual-node")
                            .compact()
                            .primary()
                            .label(self.tr("保存节点", "Save node"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                let draft = match this.manual_form.draft(cx) {
                                    Ok(draft) => draft,
                                    Err(err) => {
                                        this.manual_error = Some(err);
                                        cx.notify();
                                        return;
                                    }
                                };
                                match this.service.add_manual_node(draft) {
                                    Ok(_) => {
                                        this.manual_form.reset(window, cx);
                                        this.manual_error = None;
                                        this.show_manual_form = false;
                                        this.sync(cx);
                                    }
                                    Err(err) => {
                                        this.manual_error = Some(err.to_string());
                                        cx.notify();
                                    }
                                }
                            })),
                    )
                    .child(
                        Button::new("reset-manual-node")
                            .compact()
                            .ghost()
                            .label(self.tr("重置", "Reset"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.manual_form.reset(window, cx);
                                this.manual_error = None;
                                cx.notify();
                            })),
                    ),
            );
        card
    }
}
