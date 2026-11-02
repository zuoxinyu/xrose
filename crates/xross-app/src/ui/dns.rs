use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::dns::{self, DnsPreset};
use xross_core::model::{DnsServer, DnsServerType};

use super::theme::palette;
use super::widgets::*;
use super::AppView;

impl AppView {
    pub(crate) fn page_dns(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let model = self.snapshot.model.clone();
        let dns_config = model.dns.clone();
        let issues = dns::validate(&dns_config);
        let issues = if issues.is_empty() {
            self.service.dns_issues()
        } else {
            issues
        };
        let final_tag = dns_config.final_server.clone().unwrap_or_default();

        let mut body = div().flex().flex_col().gap_4().w_full();

        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("DNS 策略", "DNS policy"),
                            self.tr(
                                "配置域名解析服务器、分流规则、Fake-IP 与静态 Hosts",
                                "Configure DNS servers, split-routing rules, Fake-IP, and static Hosts",
                            ),
                        ))
                        .child(
                            toggle("dns-enabled", self.tr("启用 DNS", "Enable DNS"), dns_config.enabled)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.service.set_dns_enabled(*checked);
                                    this.sync(cx);
                                })),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(DnsPreset::all().into_iter().map(|preset| {
                            div()
                                .id(SharedString::from(format!("dns-preset-{preset:?}")))
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .text_sm()
                                .border_1()
                                .border_color(p.border)
                                .bg(p.bg)
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.service.set_dns_preset(preset);
                                    this.sync(cx);
                                }))
                                .child(match preset {
                                    DnsPreset::SplitFakeIp => self.tr("分流 + fake-ip（推荐 sing-box）", "Split + fake-ip (sing-box)"),
                                    DnsPreset::Split => self.tr("分流 DNS（兼容 Xray）", "Split DNS (Xray compatible)"),
                                    DnsPreset::Encrypted => self.tr("全加密 DNS（DoH/DoT）", "Encrypted DNS (DoH/DoT)"),
                                    DnsPreset::System => self.tr("系统 DNS", "System DNS"),
                                })
                        })),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_6()
                        .child(
                            toggle("fake-ip", self.tr("fake-ip", "fake-ip"), dns_config.fake_ip)
                                .on_change(cx.listener(|this, checked, _, cx| {
                                    this.service.set_fake_ip(*checked);
                                    this.sync(cx);
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_xs().text_color(p.muted_fg).child(self.tr("fake-ip 网段", "fake-ip range")))
                                .child(div().text_sm().text_color(p.fg).child(dns_config.fake_ip_range.clone())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted_fg)
                                .child(if self.snapshot.settings.core.capabilities().fake_ip {
                                    self.tr("sing-box 原生支持", "native on sing-box")
                                } else {
                                    self.tr("Xray 不支持 fake-ip，编译时会跳过", "Xray cannot compile fake-ip")
                                }),
                        ),
                ),
        );

        // Servers.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("DNS 服务器", "DNS servers"),
                            self.tr("按域名条件匹配解析服务器，未命中时使用默认服务器（final）", "Match DNS servers by domain conditions; unmatched queries use the final server"),
                        ))
                        .child(
                            Button::new("toggle-dns-form")
                                .compact()
                                .primary()
                                .label(if self.show_dns_form {
                                    self.tr("收起", "Close")
                                } else {
                                    self.tr("添加服务器", "Add server")
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_dns_form = !this.show_dns_form;
                                    cx.notify();
                                })),
                        ),
                )
                .when(self.show_dns_form, |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .p_3()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(div().w(px(140.)).child(Input::new(&self.dns_tag)))
                                    .child(div().w(px(260.)).child(Input::new(&self.dns_address)))
                                    .child(div().flex_1().child(Input::new(&self.dns_domains))),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(self.tr(
                                                "地址支持 1.1.1.1、https://…、tls://…、quic://…；条件支持 geosite:cn、domain:foo",
                                                "Address: 1.1.1.1, https://…, tls://…, quic://…; conditions: geosite:cn, domain:foo",
                                            )),
                                    )
                                    .child(
                                        Button::new("save-dns-server")
                                            .compact()
                                            .primary()
                                            .label(self.tr("保存", "Save"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let tag = this.dns_tag.read(cx).value().trim().to_string();
                                                let address = this.dns_address.read(cx).value().trim().to_string();
                                                if tag.is_empty() || address.is_empty() {
                                                    this.service.toast(
                                                        xross_core::service::ToastLevel::Warning,
                                                        "tag 与地址不能为空",
                                                    );
                                                    return;
                                                }
                                                let server_type = if address.starts_with("https://") {
                                                    DnsServerType::Https
                                                } else if address.starts_with("tls://") {
                                                    DnsServerType::Tls
                                                } else if address.starts_with("quic://") {
                                                    DnsServerType::Quic
                                                } else if address.starts_with("h3://") {
                                                    DnsServerType::H3
                                                } else {
                                                    DnsServerType::Udp
                                                };
                                                let domains = this
                                                    .dns_domains
                                                    .read(cx)
                                                    .value()
                                                    .split(',')
                                                    .map(|s| s.trim().to_string())
                                                    .filter(|s| !s.is_empty())
                                                    .collect();
                                                let server = DnsServer {
                                                    tag,
                                                    server_type,
                                                    address,
                                                    domains,
                                                    detour: Some("proxy".into()),
                                                    strategy: None,
                                                    expect_ips: Vec::new(),
                                                    client_subnet: None,
                                                };
                                                this.service.upsert_dns_server(server);
                                                this.show_dns_form = false;
                                                this.sync(cx);
                                            })),
                                    ),
                            ),
                    )
                })
                .children(dns_config.servers.iter().map(|server| {
                    let tag = server.tag.clone();
                    let tag_delete = server.tag.clone();
                    let is_final = final_tag == server.tag;
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_3()
                        .rounded_md()
                        .bg(p.bg)
                        .border_1()
                        .border_color(if is_final { p.primary } else { p.border })
                        .child(
                            div()
                                .w(px(90.))
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(div().text_sm().text_color(p.fg).child(server.tag.clone()))
                                .child(pill(server.server_type.as_str(), p.muted_fg, p.panel_alt)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .flex_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .child(server.display_address()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(if server.domains.is_empty() {
                                            self.tr("匹配所有域名", "all domains").to_string()
                                        } else {
                                            server.domains.join(", ")
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(server.is_fakeip(), |d| {
                                    d.child(pill(
                                        self.tr("由 fake-ip 规则使用", "used by fake-ip rules"),
                                        p.muted_fg,
                                        p.panel_alt,
                                    ))
                                })
                                .when(!server.is_fakeip(), |d| {
                                    d.child(
                                        Button::new(SharedString::from(format!("final-{tag}")))
                                            .compact()
                                            .ghost()
                                            .label(if is_final {
                                                self.tr("当前 final", "final")
                                            } else {
                                                self.tr("设为 final", "set final")
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.service.set_final_dns(tag.clone());
                                                this.sync(cx);
                                            })),
                                    )
                                })
                                .child(
                                    Button::new(SharedString::from(format!("del-dns-{tag_delete}")))
                                        .compact()
                                        .ghost()
                                        .label("✕")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.service.delete_dns_server(&tag_delete);
                                            this.sync(cx);
                                        })),
                                ),
                        )
                })),
        );

        // Hosts.
        body = body.child(
            card(&p)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(section_title(
                            &p,
                            self.tr("Hosts", "Hosts"),
                            self.tr(
                                "配置域名到 IP 地址的静态映射",
                                "Configure static domain-to-IP address mappings",
                            ),
                        ))
                        .child(
                            Button::new("toggle-host-form")
                                .compact()
                                .label(if self.show_host_form {
                                    self.tr("收起", "Close")
                                } else {
                                    self.tr("添加", "Add")
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_host_form = !this.show_host_form;
                                    cx.notify();
                                })),
                        ),
                )
                .when(self.show_host_form, |d| {
                    d.child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(240.)).child(Input::new(&self.host_domain)))
                            .child(div().flex_1().child(Input::new(&self.host_ips)))
                            .child(
                                Button::new("save-host")
                                    .compact()
                                    .primary()
                                    .label(self.tr("保存", "Save"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let domain =
                                            this.host_domain.read(cx).value().trim().to_string();
                                        let ips: Vec<String> = this
                                            .host_ips
                                            .read(cx)
                                            .value()
                                            .split(',')
                                            .map(|s| s.trim().to_string())
                                            .filter(|s| !s.is_empty())
                                            .collect();
                                        if domain.is_empty() || ips.is_empty() {
                                            this.service.toast(
                                                xross_core::service::ToastLevel::Warning,
                                                "域名与 IP 不能为空",
                                            );
                                            return;
                                        }
                                        let mut hosts = this.snapshot.model.dns.hosts.clone();
                                        hosts.insert(domain, ips);
                                        this.service.set_hosts(hosts);
                                        this.show_host_form = false;
                                        this.sync(cx);
                                    })),
                            ),
                    )
                })
                .when(dns_config.hosts.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr("没有静态映射", "No static mappings"),
                    ))
                })
                .children(dns_config.hosts.iter().map(|(domain, ips)| {
                    let domain = domain.clone();
                    let domain_delete = domain.clone();
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .p_2()
                        .rounded_md()
                        .bg(p.bg)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(div().text_sm().text_color(p.fg).child(domain))
                                .child(
                                    div().text_xs().text_color(p.muted_fg).child(ips.join(", ")),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("del-host-{domain_delete}")))
                                .compact()
                                .ghost()
                                .label("✕")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let mut hosts = this.snapshot.model.dns.hosts.clone();
                                    hosts.remove(&domain_delete);
                                    this.service.set_hosts(hosts);
                                    this.sync(cx);
                                })),
                        )
                })),
        );

        if !issues.is_empty() {
            body = body.child(
                card(&p)
                    .border_color(p.danger)
                    .child(section_title(
                        &p,
                        self.tr("DNS 问题", "DNS issues"),
                        self.tr(
                            "这些配置可能导致解析失败或泄漏",
                            "These may cause failures or leaks",
                        ),
                    ))
                    .children(
                        issues
                            .into_iter()
                            .map(|issue| div().text_xs().text_color(p.danger).child(issue)),
                    ),
            );
        }

        let _ = lang;
        body
    }
}
