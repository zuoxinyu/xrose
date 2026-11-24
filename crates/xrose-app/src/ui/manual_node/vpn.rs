//! Client VPN form controls. Hidden advanced imported options survive normal edits.

use super::super::widgets::{FieldWidth, dialog_control_row, dialog_field};
use super::controls::{Presets, fixed_choice, integer_input, preset_field, secret_field};
use super::{AppView, Palette, textarea_field};
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::*;
use xrose_core::model::{NodeDraft, OpenConnectOptions, OpenVpnOptions, Protocol, VpnCertificates};
use xrose_core::settings::Language;

const DATA_CIPHERS: &[&str] = &[
    "AES-256-GCM",
    "AES-128-GCM",
    "CHACHA20-POLY1305",
    "AES-192-GCM",
    "AES-256-CBC",
    "AES-128-CBC",
];

pub(super) struct VpnForm {
    ca: Entity<TextareaState>,
    certificate: Entity<TextareaState>,
    key: Entity<TextareaState>,
    control_key: Entity<TextareaState>,
    key_password: Entity<InputState>,
    fingerprints: Entity<InputState>,
    mtu: Entity<InputState>,
    ciphers: Entity<InputState>,
    fallback_cipher: Entity<InputState>,
    auth: Entity<InputState>,
    path: Entity<InputState>,
    group: Entity<InputState>,
    cookie: Entity<InputState>,
    user_agent: Entity<InputState>,
    reported_os: Entity<InputState>,
    pub disable_udp: bool,
    pub use_pushed_dns: bool,
    openvpn: OpenVpnOptions,
    openconnect: OpenConnectOptions,
}

impl VpnForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        let text = |window: &mut Window, cx: &mut Context<AppView>, value: &str| {
            cx.new(|cx| InputState::new(window, cx).default_value(value.to_string()))
        };
        let pem = |window: &mut Window, cx: &mut Context<AppView>| {
            cx.new(|cx| TextareaState::new(window, cx).rows(3))
        };
        Self {
            ca: pem(window, cx),
            certificate: pem(window, cx),
            key: pem(window, cx),
            control_key: pem(window, cx),
            key_password: cx.new(|cx| InputState::new(window, cx).masked(true)),
            fingerprints: text(window, cx, ""),
            mtu: integer_input(0, window, cx),
            ciphers: text(window, cx, ""),
            fallback_cipher: text(window, cx, ""),
            auth: text(window, cx, ""),
            path: text(window, cx, "/"),
            group: text(window, cx, ""),
            cookie: cx.new(|cx| InputState::new(window, cx).masked(true)),
            user_agent: text(window, cx, ""),
            reported_os: text(window, cx, ""),
            disable_udp: false,
            use_pushed_dns: true,
            openvpn: OpenVpnOptions::default(),
            openconnect: OpenConnectOptions::default(),
        }
    }

    pub fn load(&mut self, draft: &NodeDraft, window: &mut Window, cx: &mut Context<AppView>) {
        for input in [&self.cookie, &self.key_password] {
            input.update(cx, |s, cx| s.set_masked(true, window, cx));
        }
        self.openvpn = draft.openvpn.clone();
        self.openconnect = draft.openconnect.clone();
        self.disable_udp = draft.openconnect.disable_udp;
        self.use_pushed_dns = if draft.protocol == Protocol::Openconnect {
            draft.openconnect.use_pushed_dns
        } else {
            draft.openvpn.use_pushed_dns
        };
        let cert = if draft.protocol == Protocol::Openconnect {
            &draft.openconnect.certificates
        } else {
            &draft.openvpn.certificates
        };
        for (input, value) in [
            (&self.ca, &cert.authority_pem),
            (&self.certificate, &cert.certificate_pem),
            (&self.key, &cert.private_key_pem),
            (&self.control_key, &draft.openvpn.control_key),
        ] {
            input.update(cx, |state, cx| state.set_value(value.clone(), window, cx));
        }
        let mtu = if draft.protocol == Protocol::Openconnect {
            draft.openconnect.mtu
        } else {
            draft.openvpn.mtu
        };
        for (input, value) in [
            (&self.key_password, cert.private_key_password.clone()),
            (&self.fingerprints, cert.peer_fingerprints.join(", ")),
            (&self.mtu, mtu.to_string()),
            (&self.ciphers, draft.openvpn.data_ciphers.join(", ")),
            (&self.fallback_cipher, draft.openvpn.fallback_cipher.clone()),
            (&self.auth, draft.openvpn.auth_digest.clone()),
            (&self.path, draft.openconnect.path.clone()),
            (&self.group, draft.openconnect.auth_group.clone()),
            (&self.cookie, draft.openconnect.cookie.clone()),
            (&self.user_agent, draft.openconnect.user_agent.clone()),
            (&self.reported_os, draft.openconnect.reported_os.clone()),
        ] {
            input.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    pub fn write(
        &self,
        draft: &mut NodeDraft,
        lang: xrose_core::settings::Language,
        cx: &App,
    ) -> Result<(), String> {
        if !draft.protocol.is_vpn() {
            return Ok(());
        }
        let text = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let list = |input: &Entity<InputState>| {
            text(input)
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let certificates = VpnCertificates {
            authority_pem: self.ca.read(cx).value().to_string(),
            certificate_pem: self.certificate.read(cx).value().to_string(),
            private_key_pem: self.key.read(cx).value().to_string(),
            private_key_password: text(&self.key_password),
            peer_fingerprints: list(&self.fingerprints),
        };
        let mtu = text(&self.mtu).trim().parse::<u16>().map_err(|_| {
            crate::i18n::tr(
                lang,
                "VPN MTU 必须为整数（0 表示自动）",
                "VPN MTU must be an integer (0 for automatic)",
            )
            .to_string()
        })?;
        if draft.protocol == Protocol::Openvpn {
            let mut o = self.openvpn.clone();
            o.certificates = certificates;
            o.use_pushed_dns = self.use_pushed_dns;
            o.mtu = mtu;
            o.data_ciphers = list(&self.ciphers);
            o.fallback_cipher = text(&self.fallback_cipher);
            o.auth_digest = text(&self.auth);
            o.control_key = if o.control_mode.is_empty() {
                String::new()
            } else {
                self.control_key.read(cx).value().to_string()
            };
            draft.openvpn = o;
        } else {
            let mut o = self.openconnect.clone();
            o.certificates = certificates;
            o.use_pushed_dns = self.use_pushed_dns;
            o.mtu = mtu;
            o.path = text(&self.path).trim().into();
            o.auth_group = text(&self.group);
            o.cookie = text(&self.cookie);
            o.disable_udp = self.disable_udp;
            o.user_agent = text(&self.user_agent);
            o.reported_os = text(&self.reported_os);
            draft.openconnect = o;
        }
        Ok(())
    }

    pub fn fields(
        &self,
        protocol: Protocol,
        p: &Palette,
        lang: Language,
        cx: &Context<AppView>,
    ) -> impl IntoElement {
        let tr = |zh, en| crate::i18n::tr(lang, zh, en);
        let mut form = div().flex().flex_col().gap_2();
        if protocol == Protocol::Openvpn {
            form = form
                .child(dialog_control_row(
                    p,
                    tr("传输", "Transport"),
                    "",
                    fixed_choice(
                        "vpn-network",
                        OpenVpnOptions::TRANSPORTS,
                        &self.openvpn.transport,
                        "".into(),
                        cx,
                        |this, v| this.manual_form.vpn.openvpn.transport = v.into(),
                    ),
                ))
                .child(preset_field(
                    p,
                    lang,
                    tr("数据加密（优先顺序）", "Data ciphers (in order)"),
                    &self.ciphers,
                    Presets::Ordered(DATA_CIPHERS, &[',']),
                ))
                .child(preset_field(
                    p,
                    lang,
                    tr("兼容加密（可选）", "Fallback cipher (optional)"),
                    &self.fallback_cipher,
                    Presets::Single(DATA_CIPHERS),
                ))
                .child(preset_field(
                    p,
                    lang,
                    tr("认证摘要（可选）", "Auth digest (optional)"),
                    &self.auth,
                    Presets::Single(&["SHA256", "SHA384", "SHA512", "SHA1"]),
                ))
                .child(dialog_control_row(
                    p,
                    tr("证书匹配方式", "Certificate match"),
                    "",
                    fixed_choice(
                        "vpn-cert-match",
                        OpenVpnOptions::NAME_MATCHES,
                        &self.openvpn.certificate_name_match,
                        tr("默认", "Default"),
                        cx,
                        |this, v| this.manual_form.vpn.openvpn.certificate_name_match = v.into(),
                    ),
                ))
                .child(dialog_control_row(
                    p,
                    tr("控制通道模式", "Control channel mode"),
                    "",
                    fixed_choice(
                        "vpn-control-mode",
                        OpenVpnOptions::CONTROL_MODES,
                        &self.openvpn.control_mode,
                        tr("不使用", "None"),
                        cx,
                        |this, v| {
                            let options = &mut this.manual_form.vpn.openvpn;
                            if options.control_mode != v && v != "tls_auth" {
                                options.control_direction.clear();
                            }
                            options.control_mode = v.into();
                        },
                    ),
                ));
            if self.openvpn.control_mode == "tls_auth" {
                form = form.child(dialog_control_row(
                    p,
                    tr("密钥方向", "Key direction"),
                    "",
                    fixed_choice(
                        "vpn-key-direction",
                        OpenVpnOptions::KEY_DIRECTIONS,
                        &self.openvpn.control_direction,
                        tr("双向", "Bidirectional"),
                        cx,
                        |this, v| this.manual_form.vpn.openvpn.control_direction = v.into(),
                    ),
                ));
            }
            if !self.openvpn.control_mode.is_empty() {
                form = form.child(textarea_field(
                    p,
                    tr("控制通道密钥", "Control channel key"),
                    "",
                    &self.control_key,
                ));
            }
        } else {
            form = form
                .child(dialog_control_row(
                    p,
                    tr("类型", "Flavor"),
                    "",
                    fixed_choice(
                        "vpn-flavor",
                        OpenConnectOptions::FLAVORS,
                        &self.openconnect.flavor,
                        "".into(),
                        cx,
                        |this, v| this.manual_form.vpn.openconnect.flavor = v.into(),
                    ),
                ))
                .child(dialog_field(
                    p,
                    tr("服务器路径", "Server path"),
                    &self.path,
                    FieldWidth::Full,
                ))
                .child(dialog_field(
                    p,
                    tr("认证组（可选）", "Auth group (optional)"),
                    &self.group,
                    FieldWidth::Medium,
                ))
                .child(secret_field(
                    p,
                    tr("认证会话（可选）", "Session cookie (optional)"),
                    &self.cookie,
                ))
                .child(dialog_field(
                    p,
                    tr("客户端标识（可选）", "User agent (optional)"),
                    &self.user_agent,
                    FieldWidth::Full,
                ))
                .child(preset_field(
                    p,
                    lang,
                    tr("系统标识（可选）", "Reported OS (optional)"),
                    &self.reported_os,
                    Presets::Single(&[
                        "win",
                        "mac-intel",
                        "linux-64",
                        "linux",
                        "android",
                        "apple-ios",
                    ]),
                ));
        }
        form = form
            .child(dialog_field(
                p,
                tr("MTU（0 为自动）", "MTU (0 = auto)"),
                &self.mtu,
                FieldWidth::Compact,
            ))
            .child(dialog_field(
                p,
                tr("服务器指纹（可选）", "Peer fingerprints (optional)"),
                &self.fingerprints,
                FieldWidth::Full,
            ));
        for (label, input) in [
            (
                tr("CA 证书 PEM（可选）", "CA certificate PEM (optional)"),
                &self.ca,
            ),
            (
                tr(
                    "客户端证书 PEM（可选）",
                    "Client certificate PEM (optional)",
                ),
                &self.certificate,
            ),
            (
                tr(
                    "客户端私钥 PEM（可选）",
                    "Client private key PEM (optional)",
                ),
                &self.key,
            ),
        ] {
            form = form.child(textarea_field(p, label, "", input));
        }
        if protocol == Protocol::Openconnect {
            form = form.child(secret_field(
                p,
                tr("私钥密码（可选）", "Private key password (optional)"),
                &self.key_password,
            ));
        }
        form
    }
}
