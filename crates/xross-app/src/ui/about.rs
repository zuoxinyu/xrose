use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;

use super::theme::{palette, Palette};
use super::widgets::*;
use super::AppView;

struct CreditItem {
    name: &'static str,
    repo: &'static str,
    license: &'static str,
    url: &'static str,
    zh_desc: &'static str,
    en_desc: &'static str,
}

impl AppView {
    pub(crate) fn page_about(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let platform = &self.snapshot.platform;

        let core_credits = [
            CreditItem {
                name: "sing-box",
                repo: "SagerNet/sing-box",
                license: "GPL-3.0-or-later",
                url: "https://github.com/SagerNet/sing-box",
                zh_desc: "通用代理内核，提供出站协议、TUN 虚拟网卡、规则集路由与类型化 DNS 支持。",
                en_desc: "Universal proxy platform providing outbound protocols, TUN interface, rule-set routing, and typed DNS.",
            },
            CreditItem {
                name: "Xray-core",
                repo: "XTLS/Xray-core",
                license: "MPL-2.0",
                url: "https://github.com/XTLS/Xray-core",
                zh_desc: "Project X 代理内核，提供 VLESS、XTLS Vision、Reality 等协议与流量统计接口。",
                en_desc: "Project X proxy core supporting VLESS, XTLS Vision, Reality, and traffic statistics APIs.",
            },
            CreditItem {
                name: "EasyTier",
                repo: "EasyTier/EasyTier",
                license: "LGPL-3.0",
                url: "https://github.com/EasyTier/EasyTier",
                zh_desc: "去中心化异地组网组件，支持节点直连打洞、子网代理与虚拟局域网组建。",
                en_desc: "Decentralized mesh VPN supporting peer-to-peer NAT traversal, subnet routing, and virtual LANs.",
            },
            CreditItem {
                name: "Tailscale",
                repo: "tailscale/tailscale",
                license: "BSD-3-Clause",
                url: "https://github.com/tailscale/tailscale",
                zh_desc: "基于 WireGuard 的异地组网组件，支持 Tailnet 互联与自定义控制平面（Headscale）。",
                en_desc: "WireGuard-based mesh network supporting Tailnet connectivity and self-hosted control planes (Headscale).",
            },
            CreditItem {
                name: "GPUI Kit",
                repo: "longbridge/gpui-component",
                license: "Apache-2.0",
                url: "https://gpui-kit.com",
                zh_desc: "基于 GPUI 的跨平台桌面 UI 组件库，驱动 Xross 的主窗口与托盘界面渲染。",
                en_desc: "GPUI-based cross-platform desktop UI toolkit powering the Xross main window and tray flyout.",
            },
        ];

        let ruleset_credits = [
            CreditItem {
                name: "sing-geosite",
                repo: "SagerNet/sing-geosite",
                license: "GPL-3.0-or-later",
                url: "https://github.com/SagerNet/sing-geosite",
                zh_desc: "内置 geosite-cn、geosite-geolocation-!cn 等 .srs 二进制域名规则集，用于大陆直连与海外站点分流。",
                en_desc: "Provides built-in geosite-cn and geosite-geolocation-!cn binary (.srs) domain rule sets.",
            },
            CreditItem {
                name: "sing-geoip",
                repo: "SagerNet/sing-geoip",
                license: "GPL-3.0-or-later",
                url: "https://github.com/SagerNet/sing-geoip",
                zh_desc: "内置 geoip-cn 等 .srs 二进制 IP 归属地规则集，用于中国大陆 IP 地址直连匹配。",
                en_desc: "Provides built-in geoip-cn binary (.srs) IP address rule sets for mainland China direct routing.",
            },
            CreditItem {
                name: "meta-rules-dat",
                repo: "MetaCubeX/meta-rules-dat",
                license: "GPL-3.0-or-later",
                url: "https://github.com/MetaCubeX/meta-rules-dat",
                zh_desc: "内置 geosite-gfw (.srs) 规则集来源，用于按需代理（GFWList）预设路由模式。",
                en_desc: "Provides the geosite-gfw (.srs) rule set used by the GFWList routing preset.",
            },
            CreditItem {
                name: "domain-list-community",
                repo: "v2fly/domain-list-community",
                license: "MIT",
                url: "https://github.com/v2fly/domain-list-community",
                zh_desc: "上游社区维护的域名分类数据仓库，为 Geosite 规则集提供基础域名列表。",
                en_desc: "Community-maintained domain list repository serving as the upstream source for Geosite rule sets.",
            },
        ];

        let license_points = [
            self.tr(
                "Xross 基于 GNU General Public License v3.0 or later (GPL-3.0-or-later) 许可证开源发布。",
                "Xross is released as open-source software under the GNU General Public License v3.0 or later (GPL-3.0-or-later).",
            ),
            self.tr(
                "你可以自由使用、复制、修改和再分发本软件；修改后的衍生版本在分发时须采用相同许可证开放源代码。",
                "You may freely use, copy, modify, and redistribute this software; distributed derivative works must remain under the same license with source code made available.",
            ),
            self.tr(
                "本软件按“原样（AS IS）”提供，不附带任何适销性或特定用途适用性的明示与暗示担保。",
                "This software is provided 'AS IS', without warranty of any kind, express or implied.",
            ),
            self.tr(
                "运行时下载或调用的代理内核、组网组件及规则集均独立于客户端分发，分别适用各自上游项目的开源许可证。",
                "Proxy cores, mesh networking binaries, and rule sets are distributed independently and governed by their respective upstream licenses.",
            ),
        ];

        div()
            .flex()
            .flex_col()
            .gap_4()
            // App identity card
            .child(
                card(&p)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .flex_wrap()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(img(self.logo_badge.clone()).w(px(52.)).h(px(52.)))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_lg()
                                                            .font_weight(FontWeight::BOLD)
                                                            .text_color(p.fg)
                                                            .child("Xross"),
                                                    )
                                                    .child(pill(
                                                        format!("v{}", self.snapshot.app_version),
                                                        p.primary,
                                                        p.accent,
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(p.muted_fg)
                                                    .child(format!(
                                                        "Rust + GPUI Kit · {} / {}",
                                                        platform.os, platform.arch
                                                    )),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new("about-github")
                                            .compact()
                                            .icon(IconName::ExternalLink)
                                            .label("GitHub")
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service
                                                    .open_url("https://github.com/xross-app/xross");
                                            })),
                                    )
                                    .child(
                                        Button::new("about-issues")
                                            .compact()
                                            .ghost()
                                            .label(self.tr("问题反馈", "Issues"))
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service.open_url(
                                                    "https://github.com/xross-app/xross/issues",
                                                );
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr(
                                "跨平台 Xray / sing-box 桌面客户端，集成节点订阅、分流路由、类型化 DNS 与异地组网管理。",
                                "Cross-platform desktop client for Xray and sing-box with unified subscription, routing, DNS, and mesh networking management.",
                            )),
                    ),
            )
            // Open-source license card
            .child(
                card(&p)
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .child(section_title(
                                &p,
                                self.tr("开源许可说明", "Open-Source License"),
                                self.tr(
                                    "软件授权条款与第三方组件许可边界",
                                    "Licensing terms and third-party component boundaries",
                                ),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(pill("GPL-3.0-or-later", p.primary, p.accent))
                                    .child(
                                        Button::new("open-gpl3")
                                            .compact()
                                            .ghost()
                                            .icon(IconName::ExternalLink)
                                            .label(self.tr("许可证全文", "Full license"))
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.service.open_url(
                                                    "https://www.gnu.org/licenses/gpl-3.0.html",
                                                );
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .children(license_points.into_iter().map(|point| {
                                div()
                                    .flex()
                                    .items_start()
                                    .gap_2()
                                    .child(div().flex_shrink_0().child("•"))
                                    .child(div().flex_1().min_w(px(0.)).child(point))
                            })),
                    ),
            )
            // Cores & Mesh acknowledgements
            .child(
                card(&p)
                    .child(section_title(
                        &p,
                        self.tr(
                            "致谢：内核与基础组件",
                            "Acknowledgements: Cores & Infrastructure",
                        ),
                        self.tr(
                            "Xross 集成或支持调用的开源代理内核、异地组网与界面框架",
                            "Open-source proxy cores, mesh networking engines, and UI framework integrated with Xross",
                        ),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                core_credits
                                    .into_iter()
                                    .map(|item| self.render_credit_row(&p, item, cx)),
                            ),
                    ),
            )
            // Built-in rule sets acknowledgements
            .child(
                card(&p)
                    .child(section_title(
                        &p,
                        self.tr(
                            "致谢：内置规则集",
                            "Acknowledgements: Built-in Rule Sets",
                        ),
                        self.tr(
                            "默认路由预设与分流规则所使用的上游开源规则集项目",
                            "Upstream open-source rule-set repositories used by default routing presets",
                        ),
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .children(
                                ruleset_credits
                                    .into_iter()
                                    .map(|item| self.render_credit_row(&p, item, cx)),
                            ),
                    ),
            )
    }

    fn render_credit_row(
        &self,
        p: &Palette,
        item: CreditItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let url = item.url;
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .p_3()
            .rounded_md()
            .bg(p.bg)
            .border_1()
            .border_color(p.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.fg)
                                    .child(item.name),
                            )
                            .child(div().text_xs().text_color(p.muted_fg).child(item.repo))
                            .child(pill(item.license, p.muted_fg, p.panel_alt)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(self.tr(item.zh_desc, item.en_desc)),
                    ),
            )
            .child(
                div().flex_shrink_0().child(
                    Button::new(SharedString::from(format!("credit-{}", item.name)))
                        .compact()
                        .ghost()
                        .icon(IconName::ExternalLink)
                        .tooltip(url)
                        .on_click(cx.listener(move |this, _, _, _| {
                            this.service.open_url(url);
                        })),
                ),
            )
    }
}
