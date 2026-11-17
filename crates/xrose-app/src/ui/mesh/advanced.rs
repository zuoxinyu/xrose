//! Advanced settings for the embedded EasyTier backend.
use super::*;
use gpui_kit::component::switch::Switch;
use xrose_core::mesh::{P2pPolicy, TcpAcceleration};

impl AppView {
    pub(super) fn easytier_advanced_settings(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let options = &self.mesh_form.easytier;
        let options_list = || div().flex().flex_col().gap_2();
        // Bind each displayed value and its edit handler to the same draft field.
        macro_rules! option {
            ($($field:ident).+, $zh:literal, $en:literal, $hint:literal, $tip:literal) => {{
                let title = self.tr($zh, $en);
                dialog_control_row(
                    p,
                    title.clone(),
                    self.tr($hint, $tip),
                    Switch::new(concat!("et-", stringify!($($field).+)))
                        .checked(options.$($field).+)
                        .accessibility_label(title)
                        .on_change(cx.listener(|this, value, _, cx| {
                            this.mesh_form.easytier.$($field).+ = *value;
                            cx.notify();
                        })),
                )
            }};
        }
        let policies = [
            P2pPolicy::Automatic,
            P2pPolicy::OnDemand,
            P2pPolicy::Disabled,
        ];
        let acceleration = [
            TcpAcceleration::Off,
            TcpAcceleration::Kcp,
            TcpAcceleration::Quic,
        ];
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_2()
            .min_w(px(0.))
            .pb_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(dialog_section_heading(
                        p,
                        self.tr("连接与路由", "Connections and routing"),
                    ))
                    .child(
                        Button::new("et-reset")
                            .ghost()
                            .compact()
                            .label(self.tr("恢复高级设置默认值", "Reset advanced settings"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.mesh_form.easytier = EasyTierOptions::default();
                                this.mesh_form
                                    .mtu
                                    .update(cx, |s, cx| s.set_value("", window, cx));
                                this.mesh_form.error = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(dialog_control_row(
                p,
                self.tr("自动建立 P2P 连接", "Establish P2P connections"),
                "",
                ChoiceGroup::new(
                    "et-p2p",
                    vec![
                        self.tr("自动", "Auto"),
                        self.tr("按需", "On demand"),
                        self.tr("关闭", "Off"),
                    ],
                    policies.iter().position(|v| v == &options.p2p),
                )
                .small()
                .on_change(cx.listener(move |this, index: &usize, _, cx| {
                    this.mesh_form.easytier.p2p = policies[*index];
                    cx.notify();
                })),
            ))
            .child(
                options_list()
                    .child(option!(
                        p2p_only,
                        "仅使用直连",
                        "Direct connections only",
                        "没有直连时不经中继传输数据。",
                        "Peers without a direct link cannot exchange data."
                    ))
                    .child(option!(
                        request_p2p,
                        "请求对端主动连接",
                        "Request a direct link",
                        "请对端主动与本机建立 P2P 连接。",
                        "Ask peers to initiate a P2P connection to this device."
                    ))
                    .child(option!(
                        latency_first,
                        "延迟优先",
                        "Prefer lower latency",
                        "按总延迟而非跳数选择路径。",
                        "Choose the lowest-latency path regardless of hop count."
                    ))
                    .child(option!(
                        ipv6_transport,
                        "允许 IPv6 传输",
                        "Allow IPv6 transport",
                        "允许通过 IPv6 连接组网设备。",
                        "Allow EasyTier to reach peers over IPv6."
                    ))
                    .child(option!(
                        device.bind_physical,
                        "绑定物理网卡",
                        "Bind to physical interfaces",
                        "避免通过其他虚拟网络建立连接。",
                        "Avoid connecting through other virtual networks."
                    ))
                    .child(option!(
                        device.private_mode,
                        "私有模式",
                        "Private mode",
                        "拒绝未经信任的其他网络接入。",
                        "Reject untrusted connections from other networks."
                    )),
            )
            .child(divider(p))
            .child(dialog_section_heading(
                p,
                self.tr("传输与代理", "Transport and proxy"),
            ))
            .child(dialog_control_row(
                p,
                self.tr("TCP 流量加速", "TCP acceleration"),
                "",
                ChoiceGroup::new(
                    "et-tcp",
                    vec![self.tr("关闭", "Off"), "KCP".into(), "QUIC".into()],
                    acceleration
                        .iter()
                        .position(|v| v == &options.tcp_acceleration),
                )
                .small()
                .on_change(cx.listener(move |this, index: &usize, _, cx| {
                    this.mesh_form.easytier.tcp_acceleration = acceleration[*index];
                    cx.notify();
                })),
            ))
            .child(
                options_list()
                    .child(option!(
                        device.accept_kcp,
                        "接受 KCP 代理",
                        "Accept KCP proxy",
                        "允许对端使用 KCP 代理连接本机。",
                        "Accept KCP-proxied connections from peers."
                    ))
                    .child(option!(
                        device.accept_quic,
                        "接受 QUIC 代理",
                        "Accept QUIC proxy",
                        "允许对端使用 QUIC 代理连接本机。",
                        "Accept QUIC-proxied connections from peers."
                    ))
                    .child(option!(
                        device.userspace_stack,
                        "用户态 TCP/IP 栈",
                        "User-space TCP/IP stack",
                        "用于子网代理及支持的传输代理。",
                        "Use a user-space stack for subnet and transport proxies."
                    ))
                    .child(option!(
                        device.multi_thread,
                        "多线程运行",
                        "Multi-threaded runtime",
                        "使用多个线程处理此网络的流量。",
                        "Use multiple threads to handle this network."
                    ))
                    .child(option!(
                        disable_encryption,
                        "关闭组网传输加密",
                        "Disable traffic encryption",
                        "流量将不再加密；需与对端设置一致。",
                        "Traffic will be unencrypted. Peers must use the same setting."
                    )),
            )
            .child(dialog_field_with_desc(
                p,
                self.tr("虚拟网卡 MTU", "Interface MTU"),
                self.tr(
                    "留空使用默认值（有效范围 400–1380）。",
                    "Leave blank for default (valid range 400–1380).",
                ),
                &self.mesh_form.mtu,
                FieldWidth::Compact,
            ))
            .child(divider(p))
            .child(dialog_section_heading(
                p,
                self.tr("NAT 穿透与转发", "NAT traversal and forwarding"),
            ))
            .child(
                options_list()
                    .child(option!(
                        device.tcp_hole_punching,
                        "TCP 打洞",
                        "TCP hole punching",
                        "通过 TCP 尝试穿透 NAT。",
                        "Try to establish direct TCP links through NAT."
                    ))
                    .child(option!(
                        device.udp_hole_punching,
                        "UDP 打洞",
                        "UDP hole punching",
                        "通过 UDP 尝试穿透 NAT。",
                        "Try to establish direct UDP links through NAT."
                    ))
                    .child(option!(
                        device.symmetric_nat_punching,
                        "对称 NAT 打洞",
                        "Symmetric NAT traversal",
                        "对称 NAT 下尝试额外的 UDP 打洞。",
                        "Use additional UDP traversal techniques for symmetric NAT."
                    ))
                    .child(option!(
                        device.port_mapping,
                        "自动端口映射",
                        "Automatic port mapping",
                        "通过 UPnP / NAT-PMP 向路由器申请端口。",
                        "Request router port mappings via UPnP or NAT-PMP."
                    ))
                    .child(option!(
                        device.relay_peer_rpc,
                        "中继对端 RPC",
                        "Relay peer RPC",
                        "可为其他网络中继建立直连所需的控制消息。",
                        "Relay control messages to help other networks establish direct links."
                    ))
                    .child(option!(
                        device.system_forwarding,
                        "使用系统转发",
                        "Use system forwarding",
                        "用系统路由代替子网代理内置 NAT；须自行配置转发和回程路由。",
                        "Replace subnet proxy NAT with system routing. Configure forwarding and return routes on the host."
                    ))
                    .when(cfg!(windows), |g| {
                        g.child(option!(
                            device.udp_broadcast_relay,
                            "中继 UDP 广播",
                            "Relay UDP broadcasts",
                            "将物理网卡上的局域网广播转发给组网设备。",
                            "Forward LAN broadcasts from physical interfaces to mesh peers."
                        ))
                    }),
            )
    }
}
