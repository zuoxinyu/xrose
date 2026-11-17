//! Client VPN form controls. Hidden advanced imported options survive normal edits.

use super::super::widgets::{FieldWidth, dialog_field, dialog_field_with_desc};
use super::{AppView, Palette, textarea_field};
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::*;
use xrose_core::model::{NodeDraft, OpenConnectOptions, OpenVpnOptions, Protocol, VpnCertificates};

pub(super) struct VpnForm {
    ca: Entity<TextareaState>,
    certificate: Entity<TextareaState>,
    key: Entity<TextareaState>,
    control_key: Entity<TextareaState>,
    key_password: Entity<InputState>,
    fingerprints: Entity<InputState>,
    mtu: Entity<InputState>,
    network: Entity<InputState>,
    ciphers: Entity<InputState>,
    fallback_cipher: Entity<InputState>,
    auth: Entity<InputState>,
    control_mode: Entity<InputState>,
    control_direction: Entity<InputState>,
    name_match: Entity<InputState>,
    flavor: Entity<InputState>,
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
            key_password: text(window, cx, ""),
            fingerprints: text(window, cx, ""),
            mtu: text(window, cx, "0"),
            network: text(window, cx, "udp"),
            ciphers: text(window, cx, ""),
            fallback_cipher: text(window, cx, ""),
            auth: text(window, cx, ""),
            control_mode: text(window, cx, ""),
            control_direction: text(window, cx, ""),
            name_match: text(window, cx, ""),
            flavor: text(window, cx, "anyconnect"),
            path: text(window, cx, "/"),
            group: text(window, cx, ""),
            cookie: text(window, cx, ""),
            user_agent: text(window, cx, ""),
            reported_os: text(window, cx, ""),
            disable_udp: false,
            use_pushed_dns: true,
            openvpn: OpenVpnOptions::default(),
            openconnect: OpenConnectOptions::default(),
        }
    }

    pub fn load(&mut self, draft: &NodeDraft, window: &mut Window, cx: &mut Context<AppView>) {
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
            (&self.network, draft.openvpn.transport.clone()),
            (&self.ciphers, draft.openvpn.data_ciphers.join(", ")),
            (&self.fallback_cipher, draft.openvpn.fallback_cipher.clone()),
            (&self.auth, draft.openvpn.auth_digest.clone()),
            (&self.control_mode, draft.openvpn.control_mode.clone()),
            (
                &self.control_direction,
                draft.openvpn.control_direction.clone(),
            ),
            (
                &self.name_match,
                draft.openvpn.certificate_name_match.clone(),
            ),
            (&self.flavor, draft.openconnect.flavor.clone()),
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
            o.transport = text(&self.network).trim().into();
            o.data_ciphers = list(&self.ciphers);
            o.fallback_cipher = text(&self.fallback_cipher);
            o.auth_digest = text(&self.auth);
            o.control_mode = text(&self.control_mode).trim().into();
            o.control_key = self.control_key.read(cx).value().to_string();
            o.control_direction = text(&self.control_direction).trim().into();
            o.certificate_name_match = text(&self.name_match).trim().into();
            draft.openvpn = o;
        } else {
            let mut o = self.openconnect.clone();
            o.certificates = certificates;
            o.use_pushed_dns = self.use_pushed_dns;
            o.mtu = mtu;
            o.flavor = text(&self.flavor).trim().into();
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
        tr: impl Fn(&'static str, &'static str) -> SharedString,
    ) -> impl IntoElement {
        let mut form = div().flex().flex_col().gap_2();
        let fields = if protocol == Protocol::Openvpn {
            vec![
                (
                    tr("传输（udp / tcp）", "Transport (udp / tcp)"),
                    self.network.clone(),
                    FieldWidth::Compact,
                ),
                (
                    tr("数据加密（逗号分隔）", "Data ciphers (comma separated)"),
                    self.ciphers.clone(),
                    FieldWidth::Full,
                ),
                (
                    tr("兼容加密（可选）", "Fallback cipher (optional)"),
                    self.fallback_cipher.clone(),
                    FieldWidth::Medium,
                ),
                (
                    tr("认证摘要（可选）", "Auth digest (optional)"),
                    self.auth.clone(),
                    FieldWidth::Medium,
                ),
                (
                    tr("证书匹配方式（可选）", "Certificate match (optional)"),
                    self.name_match.clone(),
                    FieldWidth::Medium,
                ),
                (
                    tr("控制通道模式（可选）", "Control channel mode (optional)"),
                    self.control_mode.clone(),
                    FieldWidth::Medium,
                ),
                (
                    tr("密钥方向（可选）", "Key direction (optional)"),
                    self.control_direction.clone(),
                    FieldWidth::Compact,
                ),
            ]
        } else {
            form = form.child(dialog_field_with_desc(
                p,
                tr("类型", "Flavor"),
                tr(
                    "支持 anyconnect / gp / fortinet / f5 / pulse / nc。",
                    "Supports anyconnect / gp / fortinet / f5 / pulse / nc.",
                ),
                &self.flavor,
                FieldWidth::Medium,
            ));
            vec![
                (
                    tr("服务器路径", "Server path"),
                    self.path.clone(),
                    FieldWidth::Full,
                ),
                (
                    tr("认证组（可选）", "Auth group (optional)"),
                    self.group.clone(),
                    FieldWidth::Medium,
                ),
                (
                    tr("认证会话（可选）", "Session cookie (optional)"),
                    self.cookie.clone(),
                    FieldWidth::Full,
                ),
                (
                    tr("客户端标识（可选）", "User agent (optional)"),
                    self.user_agent.clone(),
                    FieldWidth::Full,
                ),
                (
                    tr("系统标识（可选）", "Reported OS (optional)"),
                    self.reported_os.clone(),
                    FieldWidth::Medium,
                ),
            ]
        };
        for (label, input, width) in fields {
            form = form.child(dialog_field(p, label, &input, width));
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
        if protocol == Protocol::Openvpn {
            form = form.child(textarea_field(
                p,
                tr("控制通道密钥（可选）", "Control channel key (optional)"),
                tr(
                    "控制通道支持 tls_auth / tls_crypt / tls_crypt_v2；密钥方向可填 client / server。",
                    "Supports tls_auth / tls_crypt / tls_crypt_v2; key direction: client / server.",
                ),
                &self.control_key,
            ));
        } else {
            form = form.child(dialog_field(
                p,
                tr("私钥密码（可选）", "Private key password (optional)"),
                &self.key_password,
                FieldWidth::Full,
            ));
        }
        form
    }
}
