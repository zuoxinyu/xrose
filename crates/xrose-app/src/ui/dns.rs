use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::dns::{self, DnsPreset};
use xrose_core::model::{DnsServer, DnsServerType};

use super::AppView;
use super::menus::{self, MenuTarget};
use super::theme::palette;
use super::widgets::*;
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tooltip::Tooltip;

impl AppView {
    fn select_default_dns(&mut self, tag: &str, cx: &mut Context<Self>) {
        let dns = &self.snapshot.model.dns;
        if dns.final_server.as_deref() == Some(tag)
            || !dns
                .servers
                .iter()
                .any(|server| server.tag == tag && !server.is_fakeip())
        {
            return;
        }
        self.service.set_final_dns(tag);
        self.sync(cx);
    }

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
        let current_preset = self.snapshot.dns_preset();
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
                .child({
                    let mut labels = vec![self.tr("分流 + Fake-IP", "Split + Fake-IP"), self.tr("分流 DNS", "Split DNS"), self.tr("加密 DNS", "Encrypted DNS"), self.tr("系统 DNS", "System DNS")];
                    if current_preset.is_none() { labels.push(self.tr("自定义", "Custom")); }
                    ChoiceGroup::new("dns-presets", labels, current_preset.and_then(|p| DnsPreset::all().iter().position(|v| *v == p)).unwrap_or(4)).cards()
                        .on_change(cx.listener(|this, index, _, cx| { if let Some(preset) = DnsPreset::all().get(*index) { this.service.set_dns_preset(*preset); this.sync(cx); } }))
                })
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
                                .child(div().text_sm().font_family(cx.theme().mono_font_family.clone()).text_color(p.fg).child(dns_config.fake_ip_range.clone()))
                                .child(pill(self.tr("只读", "Read only"), p.muted_fg, p.panel_alt)),
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
                                .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .primary()
                                .icon(if self.show_dns_form {
                                    IconName::X
                                } else {
                                    IconName::Plus
                                })
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
                                            .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                            .primary()
                                            .label(self.tr("保存", "Save"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let tag = this.dns_tag.read(cx).value().trim().to_string();
                                                let address = this.dns_address.read(cx).value().trim().to_string();
                                                if tag.is_empty() || address.is_empty() {
                                                    this.service.toast(
                                                        xrose_core::service::ToastLevel::Warning,
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
                    let is_final = final_tag == tag;
                    let menu_target = MenuTarget::Dns(tag.clone());
                    let view = cx.entity().downgrade();
                    let row_tag = tag.clone();
                    radio_row_surface(SharedString::from(format!("dns-row-{tag}")), is_final, !server.is_fakeip(), &p)
                        .on_click(cx.listener(move |this, _, _, cx| this.select_default_dns(&row_tag, cx)))
                        .child(row_radio(SharedString::from(format!("final-{tag}")),
                            self.tr("设为默认 DNS", "Use as default DNS"), is_final, !server.is_fakeip(), &p)
                            .tooltip(move |window, cx| Tooltip::new(super::tr(lang, "未匹配其他规则时使用此服务器", "Use this server when no other rule matches")).build(window, cx))
                            .on_change(move |_, _, _, cx| {
                                cx.stop_propagation();
                                let _ = view.update(cx, |this, cx| this.select_default_dns(&tag, cx));
                            }))
                        .child(div().flex().flex_col().gap_1().flex_1().min_w(px(0.))
                            .child(div().flex().items_center().gap_2()
                                .child(div().text_sm().text_color(p.fg).child(server.tag.clone()))
                                .when(server.tag != server.server_type.as_str(), |d| d.child(pill(server.server_type.as_str(), p.muted_fg, p.panel_alt)))
                                .when(is_final, |d| d.child(pill(self.tr("默认", "Default"), p.primary, p.panel))))
                            .when(!server.is_fakeip(), |d| d.child(div().text_sm().text_color(p.muted_fg).truncate().child(server.display_address())))
                            .child(div().text_xs().text_color(p.muted_fg).truncate().child(if server.is_fakeip() { self.tr("用于 Fake-IP 解析", "Used for Fake-IP resolution").to_string() } else if server.domains.is_empty() { self.tr("匹配所有域名", "All domains").to_string() } else { server.domains.join(", ") })))
                        .child(more_button(SharedString::from(format!("dns-menu-{}", server.tag)), self.tr("服务器操作", "Server actions"), cx)
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .dropdown_menu(menus::builder(cx.entity().downgrade(), menu_target.clone())))
                        .context_menu(menus::builder(cx.entity().downgrade(), menu_target))
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
                                .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                .primary()
                                .icon(if self.show_host_form {
                                    IconName::X
                                } else {
                                    IconName::Plus
                                })
                                .label(if self.show_host_form {
                                    self.tr("收起", "Close")
                                } else {
                                    self.tr("添加解析", "Add host")
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
                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
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
                                                xrose_core::service::ToastLevel::Warning,
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
                    let target = MenuTarget::Host(domain.clone());
                    div()
                        .id(SharedString::from(format!("host-{domain}")))
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .rounded_md()
                        .bg(p.bg)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .text_color(p.fg)
                                .truncate()
                                .child(domain.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(p.muted_fg)
                                .truncate()
                                .child(ips.join(", ")),
                        )
                        .child(
                            more_button(
                                SharedString::from(format!("host-menu-{domain}")),
                                self.tr("静态解析操作", "Host mapping actions"),
                                cx,
                            )
                            .dropdown_menu(menus::builder(cx.entity().downgrade(), target.clone())),
                        )
                        .context_menu(menus::builder(cx.entity().downgrade(), target))
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
