//! State and controls shared by the node creation and editing dialogs.
//!
//! The form only collects text; [`xrose_core::model::NodeDraft`] owns the
//! validation rules so the CLI and the UI cannot drift apart.

use gpui_base::Disableable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputState, Textarea, TextareaState};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::model::{Node, NodeDraft, Protocol, TransportKind};

mod vpn;

use super::AppView;
use super::theme::{Palette, palette};
use super::widgets::*;

pub(crate) struct ManualNodeForm {
    vpn: vpn::VpnForm,
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
    pub wireguard_private_key: Entity<InputState>,
    pub wireguard_public_key: Entity<InputState>,
    pub wireguard_pre_shared_key: Entity<InputState>,
    pub wireguard_addresses: Entity<InputState>,
    pub wireguard_allowed_ips: Entity<InputState>,
    pub wireguard_mtu: Entity<InputState>,
    pub wireguard_reserved: Entity<InputState>,

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
            vpn: vpn::VpnForm::new(window, cx),
            name: text(window, cx, "留空自动使用 地址:端口"),
            server: text(window, cx, "example.com 或 1.2.3.4"),
            port,
            uuid: text(window, cx, "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"),
            password: text(window, cx, "输入节点密码或密钥"),
            username: text(window, cx, "可选，例如 user"),
            method: text(window, cx, "aes-256-gcm"),
            flow: text(window, cx, "xtls-rprx-vision"),
            sni: text(window, cx, "留空默认使用服务器地址"),
            alpn: text(window, cx, "h2,http/1.1"),
            fingerprint: text(window, cx, "chrome"),
            reality_public_key: text(window, cx, "Base64url 公钥"),
            reality_short_id: text(window, cx, "0123abcd"),
            path,
            host: text(window, cx, "cdn.example.com"),
            service_name: text(window, cx, "grpc-service"),
            tags: text(window, cx, "hk, relay（逗号分隔）"),
            wireguard_private_key: text(window, cx, "Base64 私钥"),
            wireguard_public_key: text(window, cx, "Base64 对端公钥"),
            wireguard_pre_shared_key: text(window, cx, "Base64 预共享密钥（可选）"),
            wireguard_addresses: text(window, cx, "10.0.0.2/32, fd00::2/128"),
            wireguard_allowed_ips: text(window, cx, "0.0.0.0/0, ::/0"),
            wireguard_mtu: text(window, cx, "1408"),
            wireguard_reserved: text(window, cx, "0,0,0"),
            protocol: Protocol::Vless,
            transport: TransportKind::Tcp,
            tls: true,
            insecure: false,
            udp: true,
        }
    }

    /// Builds the draft. Only the port needs parsing here; every other rule
    /// (credentials, Reality, transport combinations) lives in the core.
    pub fn draft(
        &self,
        lang: xrose_core::settings::Language,
        cx: &App,
    ) -> std::result::Result<NodeDraft, String> {
        let port_text = self.port.read(cx).value().trim().to_string();
        let port = port_text.parse::<u16>().map_err(|_| {
            format!(
                "{}{port_text}{}",
                crate::i18n::tr(lang, "端口无效：", "Invalid port: "),
                crate::i18n::tr(lang, "（应为 1-65535）", " (expected 1-65535)"),
            )
        })?;
        let mut draft = NodeDraft {
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
            wireguard_private_key: self.wireguard_private_key.read(cx).value().to_string(),
            wireguard_public_key: self.wireguard_public_key.read(cx).value().to_string(),
            wireguard_pre_shared_key: self.wireguard_pre_shared_key.read(cx).value().to_string(),
            wireguard_addresses: self.wireguard_addresses.read(cx).value().to_string(),
            wireguard_allowed_ips: self.wireguard_allowed_ips.read(cx).value().to_string(),
            wireguard_mtu: self.wireguard_mtu.read(cx).value().to_string(),
            wireguard_reserved: self.wireguard_reserved.read(cx).value().to_string(),
            openvpn: Default::default(),
            openconnect: Default::default(),
        };
        self.vpn.write(&mut draft, lang, cx)?;
        Ok(draft)
    }

    pub fn localize_inputs(
        &self,
        lang: xrose_core::settings::Language,
        window: &mut Window,
        cx: &mut Context<AppView>,
    ) {
        let tr = |zh: &'static str, en: &'static str| crate::i18n::tr(lang, zh, en);
        for (input, placeholder) in [
            (
                &self.name,
                tr("留空自动使用 地址:端口", "Blank uses host:port"),
            ),
            (
                &self.server,
                tr("example.com 或 1.2.3.4", "example.com or 1.2.3.4"),
            ),
            (
                &self.uuid,
                tr(
                    "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
                    "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
                ),
            ),
            (
                &self.password,
                tr("输入节点密码或密钥", "Enter password or secret"),
            ),
            (&self.username, tr("可选，例如 user", "Optional, e.g. user")),
            (&self.method, tr("aes-256-gcm", "aes-256-gcm")),
            (&self.flow, tr("xtls-rprx-vision", "xtls-rprx-vision")),
            (
                &self.sni,
                tr("留空默认使用服务器地址", "Blank uses server address"),
            ),
            (&self.alpn, tr("h2,http/1.1", "h2,http/1.1")),
            (&self.fingerprint, tr("chrome", "chrome")),
            (
                &self.reality_public_key,
                tr("Base64url 公钥", "Base64url public key"),
            ),
            (&self.reality_short_id, tr("0123abcd", "0123abcd")),
            (&self.path, tr("/ws", "/ws")),
            (&self.host, tr("cdn.example.com", "cdn.example.com")),
            (&self.service_name, tr("grpc-service", "grpc-service")),
            (
                &self.tags,
                tr("hk, relay（逗号分隔）", "hk, relay (comma separated)"),
            ),
            (
                &self.wireguard_private_key,
                tr("Base64 私钥", "Base64 private key"),
            ),
            (
                &self.wireguard_public_key,
                tr("Base64 对端公钥", "Base64 peer public key"),
            ),
            (
                &self.wireguard_pre_shared_key,
                tr(
                    "Base64 预共享密钥（可选）",
                    "Base64 pre-shared key (optional)",
                ),
            ),
            (
                &self.wireguard_addresses,
                tr("10.0.0.2/32, fd00::2/128", "10.0.0.2/32, fd00::2/128"),
            ),
            (
                &self.wireguard_allowed_ips,
                tr("0.0.0.0/0, ::/0", "0.0.0.0/0, ::/0"),
            ),
            (&self.wireguard_mtu, tr("1408", "1408")),
            (&self.wireguard_reserved, tr("0,0,0", "0,0,0")),
        ] {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx)
            });
        }
    }

    pub fn reset(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        self.vpn.load(&NodeDraft::default(), window, cx);
        self.protocol = Protocol::Vless;
        self.transport = TransportKind::Tcp;
        self.tls = true;
        self.insecure = false;
        self.udp = true;
        let inputs: [(&Entity<InputState>, &str); 24] = [
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
            (&self.wireguard_private_key, ""),
            (&self.wireguard_public_key, ""),
            (&self.wireguard_pre_shared_key, ""),
            (&self.wireguard_addresses, ""),
            (&self.wireguard_allowed_ips, "0.0.0.0/0, ::/0"),
            (&self.wireguard_mtu, "1408"),
            (&self.wireguard_reserved, "0,0,0"),
        ];
        for (input, value) in inputs {
            let window = &mut *window;
            let cx = &mut *cx;
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    pub fn load(&mut self, node: &Node, window: &mut Window, cx: &mut Context<AppView>) {
        let draft = NodeDraft::from(node);
        self.vpn.load(&draft, window, cx);
        self.protocol = draft.protocol;
        self.transport = draft.transport;
        self.tls = draft.tls_enabled;
        self.insecure = draft.insecure;
        self.udp = draft.udp;
        for (input, value) in [
            (&self.name, draft.name),
            (&self.server, draft.server),
            (&self.port, draft.port.to_string()),
            (&self.uuid, draft.uuid),
            (&self.password, draft.password),
            (&self.username, draft.username),
            (&self.method, draft.method),
            (&self.flow, draft.flow),
            (&self.sni, draft.sni),
            (&self.alpn, draft.alpn),
            (&self.fingerprint, draft.fingerprint),
            (&self.reality_public_key, draft.reality_public_key),
            (&self.reality_short_id, draft.reality_short_id),
            (&self.path, draft.transport_path),
            (&self.host, draft.transport_host),
            (&self.service_name, draft.transport_service_name),
            (&self.tags, draft.tags),
            (&self.wireguard_private_key, draft.wireguard_private_key),
            (&self.wireguard_public_key, draft.wireguard_public_key),
            (
                &self.wireguard_pre_shared_key,
                draft.wireguard_pre_shared_key,
            ),
            (&self.wireguard_addresses, draft.wireguard_addresses),
            (&self.wireguard_allowed_ips, draft.wireguard_allowed_ips),
            (&self.wireguard_mtu, draft.wireguard_mtu),
            (&self.wireguard_reserved, draft.wireguard_reserved),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }
}

fn show_reality_fields(protocol: Protocol, public_key: &str, short_id: &str) -> bool {
    protocol == Protocol::Vless || !public_key.trim().is_empty() || !short_id.trim().is_empty()
}

pub(super) fn textarea_field(
    p: &Palette,
    label: impl Into<SharedString>,
    description: impl Into<SharedString>,
    input: &Entity<TextareaState>,
) -> Div {
    dialog_form_row(
        p,
        label,
        description,
        Textarea::new(input).h(px(80.)),
        RowWidth::Full,
    )
}

impl AppView {
    pub(crate) fn manual_node_fields(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let form = &self.manual_form;
        let protocol = form.protocol;
        let transport = form.transport;
        let tls = form.tls;
        let insecure = form.insecure;
        let udp = form.udp;

        let needs_uuid = matches!(protocol, Protocol::Vmess | Protocol::Vless | Protocol::Tuic);
        let needs_password = matches!(
            protocol,
            Protocol::Trojan
                | Protocol::Anytls
                | Protocol::Hysteria2
                | Protocol::Shadowsocks
                | Protocol::Tuic
                | Protocol::Socks
                | Protocol::Http
                | Protocol::Openvpn
                | Protocol::Openconnect
        );
        let needs_username = matches!(
            protocol,
            Protocol::Socks | Protocol::Http | Protocol::Openvpn | Protocol::Openconnect
        );
        let has_credentials =
            needs_uuid || needs_password || needs_username || protocol == Protocol::Vless;
        let tls_capable =
            !protocol.is_plain_proxy() && protocol != Protocol::Wireguard && !protocol.is_vpn();
        let transport_capable = !matches!(
            protocol,
            Protocol::Hysteria2
                | Protocol::Tuic
                | Protocol::Anytls
                | Protocol::Wireguard
                | Protocol::Openvpn
                | Protocol::Openconnect
        );

        let mut credentials = div().flex().flex_col().gap_2();
        if needs_uuid {
            credentials = credentials.child(dialog_field(
                &p,
                if protocol == Protocol::Tuic {
                    self.tr("UUID（TUIC）", "UUID (TUIC)")
                } else {
                    self.tr("UUID / 用户 ID", "UUID / user id")
                },
                &form.uuid,
                FieldWidth::Full,
            ));
        }
        if protocol == Protocol::Vless {
            credentials = credentials.child(dialog_field(
                &p,
                self.tr("Flow（可选）", "Flow (optional)"),
                &form.flow,
                FieldWidth::Medium,
            ));
        }
        if protocol == Protocol::Shadowsocks {
            credentials = credentials.child(dialog_field(
                &p,
                self.tr("加密方式", "Cipher"),
                &form.method,
                FieldWidth::Medium,
            ));
        }
        if needs_username {
            credentials = credentials.child(dialog_field(
                &p,
                self.tr("用户名", "Username"),
                &form.username,
                FieldWidth::Full,
            ));
        }
        if needs_password {
            credentials = credentials.child(dialog_field(
                &p,
                if needs_username {
                    self.tr("密码", "Password")
                } else {
                    self.tr("密码 / 密钥", "Password")
                },
                &form.password,
                FieldWidth::Full,
            ));
        }

        let transport_fields = match transport {
            TransportKind::WebSocket | TransportKind::Http2 | TransportKind::HttpUpgrade => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(dialog_field(
                        &p,
                        self.tr("路径 Path", "Path"),
                        &form.path,
                        FieldWidth::Full,
                    ))
                    .child(dialog_field(
                        &p,
                        self.tr("Host / CDN（可选）", "Host / CDN (optional)"),
                        &form.host,
                        FieldWidth::Full,
                    )),
            ),
            TransportKind::Grpc => Some(div().flex().flex_col().gap_2().child(dialog_field(
                &p,
                self.tr("serviceName", "serviceName"),
                &form.service_name,
                FieldWidth::Medium,
            ))),
            TransportKind::Tcp | TransportKind::Kcp | TransportKind::Quic => None,
        };
        let transport_note: SharedString = match transport {
            TransportKind::Tcp => self.tr(
                "TCP 直连，无需额外传输参数。",
                "Plain TCP: no extra transport parameters.",
            ),
            TransportKind::Kcp => self.tr(
                "mKCP 使用默认传输参数。",
                "mKCP uses default transport parameters.",
            ),
            TransportKind::Quic => self.tr(
                "QUIC 使用默认传输参数。",
                "QUIC uses default transport parameters.",
            ),
            _ => "".into(),
        };

        let show_reality = show_reality_fields(
            protocol,
            form.reality_public_key.read(cx).value().as_ref(),
            form.reality_short_id.read(cx).value().as_ref(),
        );
        let protocols = Protocol::manual();
        let selected_protocol = protocols
            .iter()
            .position(|candidate| *candidate == protocol);
        let transports = TransportKind::all();
        let selected_transport = transports
            .iter()
            .position(|candidate| *candidate == transport);

        div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w(px(0.))
            .pb_1()
            .child(dialog_control_row(
                &p,
                self.tr("协议", "Protocol"),
                "",
                ChoiceGroup::new(
                    "manual-protocol",
                    protocols
                        .into_iter()
                        .map(|candidate| SharedString::from(candidate.display_name()))
                        .collect(),
                    selected_protocol,
                )
                .small()
                .on_change(cx.listener(|this, index: &usize, window, cx| {
                    let Some(candidate) = Protocol::manual().into_iter().nth(*index) else {
                        return;
                    };
                    if candidate == this.manual_form.protocol {
                        return;
                    }
                    let old_port = this.manual_form.protocol.default_port().to_string();
                    if this.manual_form.port.read(cx).value().as_ref() == old_port.as_str() {
                        this.manual_form.port.update(cx, |s, cx| {
                            s.set_value(candidate.default_port().to_string(), window, cx)
                        });
                    }
                    this.manual_form.protocol = candidate;
                    if candidate == Protocol::Wireguard || candidate.is_vpn() {
                        this.manual_form.transport = TransportKind::Tcp;
                        this.manual_form.insecure = false;
                    }
                    if candidate.is_vpn() {
                        this.manual_form.tls = true;
                        for input in [
                            &this.manual_form.alpn,
                            &this.manual_form.fingerprint,
                            &this.manual_form.reality_public_key,
                            &this.manual_form.reality_short_id,
                        ] {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                    }
                    if matches!(
                        candidate,
                        Protocol::Hysteria2 | Protocol::Tuic | Protocol::Anytls
                    ) {
                        this.manual_form.tls = true;
                        this.manual_form.transport = TransportKind::Tcp;
                    }
                    this.manual_error = None;
                    cx.notify();
                })),
            ))
            .child(divider(&p))
            .child(dialog_section_heading(&p, self.tr("基础", "Basics")))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(dialog_field(
                        &p,
                        self.tr("名称（可选）", "Name (optional)"),
                        &form.name,
                        FieldWidth::Full,
                    ))
                    .child(dialog_field(
                        &p,
                        self.tr("服务器", "Server"),
                        &form.server,
                        FieldWidth::Full,
                    ))
                    .child(dialog_field(
                        &p,
                        self.tr("端口", "Port"),
                        &form.port,
                        FieldWidth::Compact,
                    )),
            )
            .when(has_credentials, |d| {
                d.child(divider(&p))
                    .child(dialog_section_heading(&p, self.tr("凭据", "Credentials")))
                    .child(credentials)
            })
            .when(protocol == Protocol::Wireguard, |d| {
                d.child(divider(&p))
                    .child(dialog_section_heading(&p, "WireGuard"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(dialog_field(
                                &p,
                                self.tr("私钥", "Private key"),
                                &form.wireguard_private_key,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("对端公钥", "Peer public key"),
                                &form.wireguard_public_key,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("预共享密钥（可选）", "Pre-shared key (optional)"),
                                &form.wireguard_pre_shared_key,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("本地地址（逗号分隔）", "Local addresses"),
                                &form.wireguard_addresses,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("允许网段", "Allowed networks"),
                                &form.wireguard_allowed_ips,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("MTU", "MTU"),
                                &form.wireguard_mtu,
                                FieldWidth::Compact,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("Reserved（3 字节）", "Reserved (3 bytes)"),
                                &form.wireguard_reserved,
                                FieldWidth::Compact,
                            )),
                    )
            })
            .when(protocol.is_vpn(), |d| {
                d.child(divider(&p))
                    .child(dialog_section_heading(&p, self.tr("VPN 参数", "VPN settings")))
                    .child(form.vpn.fields(protocol, &p, |zh, en| self.tr(zh, en)))
                    .child(dialog_control_row(
                        &p,
                        self.tr("分域 DNS", "Split DNS"),
                        "",
                        toggle(
                            "vpn-pushed-dns",
                            self.tr("使用 VPN 下发的分域 DNS", "Use VPN split DNS"),
                            form.vpn.use_pushed_dns,
                        )
                        .on_change(cx.listener(|this, checked, _, cx| {
                            this.manual_form.vpn.use_pushed_dns = *checked;
                            cx.notify();
                        })),
                    ))
                    .child(dialog_field(
                        &p,
                        self.tr("服务器证书名称（可选）", "Server cert name (optional)"),
                        &form.sni,
                        FieldWidth::Full,
                    ))
                    .when(protocol == Protocol::Openconnect, |d| {
                        d.child(dialog_control_row(
                            &p,
                            self.tr("传输模式", "Transport"),
                            self.tr(
                                "禁用 DTLS/UDP 数据通道，仅通过 TCP/TLS 转发流量。",
                                "Disable DTLS/UDP and send all traffic over TCP/TLS.",
                            ),
                            toggle(
                                "vpn-no-udp",
                                self.tr("仅使用 TLS 连接", "Use TLS transport only"),
                                form.vpn.disable_udp,
                            )
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.manual_form.vpn.disable_udp = *checked;
                                cx.notify();
                            })),
                        ))
                        .child(dialog_control_row(
                            &p,
                            self.tr("证书校验", "Verification"),
                            self.tr(
                                "跳过服务端 TLS 证书与主机名校验，仅在可信或自签名环境下启用。",
                                "Skip server TLS certificate and hostname checks. Only enable for trusted or self-signed endpoints.",
                            ),
                            toggle(
                                "vpn-insecure",
                                self.tr("允许不安全证书", "Allow insecure certificates"),
                                insecure,
                            )
                            .on_change(cx.listener(|this, checked, _, cx| {
                                this.manual_form.insecure = *checked;
                                cx.notify();
                            })),
                        ))
                    })
            })
            .when(tls_capable, |d| {
                d.child(divider(&p))
                    .child(dialog_section_heading(
                        &p,
                        if show_reality {
                            self.tr("TLS / Reality", "TLS / Reality")
                        } else {
                            self.tr("TLS", "TLS")
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(dialog_control_row(
                                &p,
                                "TLS",
                                if !transport_capable {
                                    self.tr(
                                        "当前协议强制启用 TLS。",
                                        "TLS is always enabled for this protocol.",
                                    )
                                } else {
                                    "".into()
                                },
                                toggle(
                                    "manual-tls",
                                    self.tr("启用 TLS", "Enable TLS"),
                                    tls || !transport_capable,
                                )
                                .disabled(!transport_capable)
                                .on_change(cx.listener(
                                    |this, checked, _, cx| {
                                        this.manual_form.tls = *checked;
                                        cx.notify();
                                    },
                                )),
                            ))
                            .child(dialog_control_row(
                                &p,
                                self.tr("证书校验", "Verification"),
                                self.tr(
                                    "跳过服务端 TLS 证书与主机名校验，仅在可信或自签名环境下启用。",
                                    "Skip server TLS certificate and hostname checks. Only enable for trusted or self-signed endpoints.",
                                ),
                                toggle(
                                    "manual-insecure",
                                    self.tr("允许不安全证书", "Allow insecure certificates"),
                                    insecure,
                                )
                                .on_change(cx.listener(
                                    |this, checked, _, cx| {
                                        this.manual_form.insecure = *checked;
                                        cx.notify();
                                    },
                                )),
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("SNI（可选）", "SNI (optional)"),
                                &form.sni,
                                FieldWidth::Full,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("ALPN（可选）", "ALPN (optional)"),
                                &form.alpn,
                                FieldWidth::Medium,
                            ))
                            .child(dialog_field(
                                &p,
                                self.tr("uTLS 指纹（可选）", "uTLS fingerprint"),
                                &form.fingerprint,
                                FieldWidth::Medium,
                            ))
                            .when(show_reality, |d| {
                                d.child(dialog_field(
                                    &p,
                                    self.tr("Reality 公钥 pbk", "Reality public key"),
                                    &form.reality_public_key,
                                    FieldWidth::Full,
                                ))
                                .child(dialog_field_with_desc(
                                    &p,
                                    self.tr("Reality shortId", "Reality shortId"),
                                    self.tr(
                                        "填写公钥即启用 Reality（自动开启 TLS），shortId 可留空。",
                                        "Setting a public key enables Reality and TLS automatically; shortId is optional.",
                                    ),
                                    &form.reality_short_id,
                                    FieldWidth::Medium,
                                ))
                            }),
                    )
            })
            .when(transport_capable, |d| {
                d.child(divider(&p))
                    .child(dialog_section_heading(&p, self.tr("传输层", "Transport")))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(dialog_control_row(
                                &p,
                                self.tr("类型", "Mode"),
                                transport_note,
                                ChoiceGroup::new(
                                    "manual-transport",
                                    transports
                                        .into_iter()
                                        .map(|candidate| {
                                            SharedString::from(candidate.display_name())
                                        })
                                        .collect(),
                                    selected_transport,
                                )
                                .small()
                                .on_change(cx.listener(|this, index: &usize, _, cx| {
                                    if let Some(candidate) = TransportKind::all().into_iter().nth(*index)
                                    {
                                        this.manual_form.transport = candidate;
                                        cx.notify();
                                    }
                                })),
                            ))
                            .when_some(transport_fields, |d, fields| d.child(fields)),
                    )
            })
            .child(divider(&p))
            .child(dialog_section_heading(&p, self.tr("其他", "Options")))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(!protocol.is_vpn(), |d| {
                        d.child(dialog_control_row(
                            &p,
                            self.tr("UDP 转发", "UDP"),
                            self.tr(
                                "允许通过该节点转发 UDP 流量。",
                                "Forward UDP traffic through this node.",
                            ),
                            toggle("manual-udp", self.tr("启用 UDP", "Enable UDP"), udp)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.manual_form.udp = *checked;
                                    cx.notify();
                                })),
                        ))
                    })
                    .child(dialog_field(
                        &p,
                        self.tr("标签（可选）", "Tags (optional)"),
                        &form.tags,
                        FieldWidth::Full,
                    )),
            )
    }

    pub(crate) fn manual_node_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let actions = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .child(
                Button::new("cancel-manual-node")
                    .label(self.tr("取消", "Cancel"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.close_dialog(cx);
                        this.clear_node_editor(window, cx);
                    })),
            )
            .child(
                Button::new("save-manual-node")
                    .primary()
                    .label(self.tr("保存节点", "Save node"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let draft = match this.manual_form.draft(this.lang(), cx) {
                            Ok(draft) => draft,
                            Err(err) => {
                                this.manual_error = Some(err);
                                window.refresh();
                                cx.notify();
                                return;
                            }
                        };
                        let result = if let Some(id) = &this.editing_node {
                            this.service.update_node(id, draft)
                        } else {
                            this.service.add_manual_node(draft).map(|_| ())
                        };
                        match result {
                            Ok(()) => {
                                window.close_dialog(cx);
                                this.clear_node_editor(window, cx);
                                this.sync(cx);
                            }
                            Err(err) => {
                                this.manual_error = Some(err.to_string());
                                window.refresh();
                                cx.notify();
                            }
                        }
                    })),
            );
        super::dialogs::dialog_footer(&p, self.manual_error.clone(), actions)
    }
}

#[cfg(test)]
mod tests {
    use super::super::theme::{FORM_FIELD_WIDTH, SETTING_LABEL_WIDTH};
    use super::{FieldWidth, Protocol, show_reality_fields};
    use gpui_kit::px;

    #[test]
    fn reality_fields_scope_to_vless_unless_already_populated() {
        assert!(show_reality_fields(Protocol::Vless, "", ""));
        assert!(!show_reality_fields(Protocol::Trojan, "", ""));
        assert!(!show_reality_fields(Protocol::Vmess, "", "  "));
        assert!(!show_reality_fields(Protocol::Hysteria2, "", ""));
        assert!(show_reality_fields(Protocol::Trojan, "pbk-value", ""));
        assert!(show_reality_fields(Protocol::Vmess, "", "abcd"));
    }

    #[test]
    fn field_width_maps_to_design_tokens() {
        assert_eq!(
            FieldWidth::Compact.fixed_width(),
            Some(px(SETTING_LABEL_WIDTH))
        );
        assert_eq!(FieldWidth::Medium.fixed_width(), Some(px(FORM_FIELD_WIDTH)));
        assert_eq!(FieldWidth::Full.fixed_width(), None);
    }
}
