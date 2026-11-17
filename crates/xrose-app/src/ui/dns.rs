use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::dns::{self, DnsPreset};
use xrose_core::model::{DnsServer, DnsServerType, MeshDnsMode, WorkMode};
use xrose_core::settings::Language;

use super::AppView;
use super::menus::{self, MenuTarget};
use super::theme::palette;
use super::widgets::*;
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon};

fn visible_dns_servers(servers: &[DnsServer]) -> Vec<&DnsServer> {
    servers.iter().filter(|s| !s.is_fakeip()).collect()
}

fn dns_rules_summary(domains: &[String], lang: Language) -> String {
    if domains.is_empty() {
        super::tr(lang, "匹配所有域名", "All domains").to_string()
    } else {
        domains.join(", ")
    }
}

impl AppView {
    pub(super) fn select_default_dns(&mut self, tag: &str, cx: &mut Context<Self>) {
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
                .when(model.work_mode == WorkMode::Mesh, |card| {
                    card.child(ChoiceGroup::new(
                        "mesh-dns-mode",
                        vec![self.tr("随出口", "Use exit DNS"), self.tr("自定义 DNS", "Custom DNS")],
                        if model.mesh_mode.dns == MeshDnsMode::Exit { 0 } else { 1 },
                    ).on_change(cx.listener(|this, index, _, cx| {
                        let mode = if *index == 0 { MeshDnsMode::Exit } else { MeshDnsMode::Custom };
                        if let Err(error) = this.service.set_mesh_dns_mode(mode) {
                            this.service.toast(xrose_core::service::ToastLevel::Error, error.to_string());
                        }
                        this.sync(cx);
                    })))
                    .child(div().text_sm().text_color(p.muted_fg).child(if model.mesh_mode.dns == MeshDnsMode::Exit {
                        self.tr("组网模式使用出口 DNS。以下自定义配置会保留，本地 Fake-IP 已暂停。", "Mesh mode uses exit DNS. Your custom settings are preserved; local Fake-IP is paused.")
                    } else {
                        self.tr("组网模式使用下方自定义 DNS，查询经组网出口发送。本地 Fake-IP 已暂停。", "Mesh mode uses the custom DNS below through the exit. Local Fake-IP is paused.")
                    }))
                })
                .child({
                    let mut labels = vec![self.tr("分流 + Fake-IP", "Split + Fake-IP"), self.tr("分流 DNS", "Split DNS"), self.tr("加密 DNS", "Encrypted DNS"), self.tr("系统 DNS", "System DNS")];
                    if current_preset.is_none() { labels.push(self.tr("自定义", "Custom")); }
                    ChoiceGroup::new("dns-presets", labels, current_preset.and_then(|p| DnsPreset::all().iter().position(|v| *v == p)).unwrap_or(4)).cards()
                        .on_change(cx.listener(|this, index, _, cx| { if let Some(preset) = DnsPreset::all().get(*index) { this.service.set_dns_preset(*preset); this.sync(cx); } }))
                })
                .child({
                    let tip = self.tr("仅 sing-box 下支持", "Supported on sing-box only");
                    div()
                        .flex()
                        .items_center()
                        .gap_6()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    toggle("fake-ip", self.tr("fake-ip", "fake-ip"), dns_config.fake_ip)
                                        .on_change(cx.listener(|this, checked, _, cx| {
                                            this.service.set_fake_ip(*checked);
                                            this.sync(cx);
                                        })),
                                )
                                .child(
                                    div()
                                        .id("fake-ip-support-tip")
                                        .aria_label(tip.clone())
                                        .flex()
                                        .items_center()
                                        .text_color(p.muted_fg)
                                        .hover(|s| s.text_color(p.fg))
                                        .child(Icon::new(IconName::Info).size(px(14.)))
                                        .tooltip(move |window, cx| {
                                            Tooltip::new(tip.clone()).build(window, cx)
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(p.muted_fg)
                                        .child(self.tr("fake-ip 网段", "fake-ip range")),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(p.fg)
                                        .child(dns_config.fake_ip_range.clone()),
                                ),
                        )
                }),
        );

        // Servers.
        let visible_servers = visible_dns_servers(&dns_config.servers);
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
                                .when(!self.show_dns_form, |b| b.primary())
                                .icon(if self.show_dns_form {
                                    IconName::X
                                } else {
                                    IconName::Plus
                                })
                                .label(if self.show_dns_form {
                                    self.tr("收起面板", "Collapse")
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
                            .gap_3()
                            .p_3()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(inline_field_label(&p, self.tr("服务器标识", "Tag")))
                                    .child(
                                        div()
                                            .w(px(180.))
                                            .max_w_full()
                                            .child(Input::new(&self.dns_tag)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(inline_field_label(&p, self.tr("服务器地址", "Address")))
                                    .child(
                                        div()
                                            .w(px(360.))
                                            .max_w_full()
                                            .child(Input::new(&self.dns_address)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(inline_field_label(&p, self.tr("域名条件", "Domains")))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(Input::new(&self.dns_domains)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .justify_between()
                                    .items_center()
                                    .gap_3()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(p.border)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(220.))
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(self.tr(
                                                "地址支持 1.1.1.1、https://…、tls://…、quic://…；留空域名条件表示匹配所有域名",
                                                "Supports 1.1.1.1, https://…, tls://…, quic://…; leave domains blank to match all",
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .flex_shrink_0()
                                            .child(
                                                Button::new("cancel-dns-form")
                                                    .compact()
                                                    .ghost()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .label(self.tr("取消", "Cancel"))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.show_dns_form = false;
                                                        cx.notify();
                                                    })),
                                            )
                                            .child(
                                                Button::new("save-dns-server")
                                                    .compact()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .primary()
                                                    .label(self.tr("保存服务器", "Save server"))
                                                    .on_click(cx.listener(|this, _, window, cx| {
                                                        let tag = this.dns_tag.read(cx).value().trim().to_string();
                                                        let address = this.dns_address.read(cx).value().trim().to_string();
                                                        if tag.is_empty() || address.is_empty() {
                                                            this.service.toast(
                                                                xrose_core::service::ToastLevel::Warning,
                                                                this.tr(
                                                                    "标识与地址不能为空",
                                                                    "Tag and address are required",
                                                                )
                                                                .to_string(),
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
                                                        for input in [&this.dns_tag, &this.dns_address, &this.dns_domains] {
                                                            input.update(cx, |s, cx| s.set_value("", window, cx));
                                                        }
                                                        this.show_dns_form = false;
                                                        this.sync(cx);
                                                    })),
                                            ),
                                    ),
                            ),
                    )
                })
                .when(visible_servers.is_empty(), |d| {
                    d.child(empty_hint(
                        &p,
                        self.tr("没有 DNS 服务器", "No DNS servers"),
                    ))
                })
                .children(visible_servers.into_iter().map(|server| {
                    let tag = server.tag.clone();
                    let is_final = final_tag == tag;
                    let menu_target = MenuTarget::Dns(tag.clone());
                    let action_tag = tag.clone();
                    let address = server.display_address();
                    let rules = dns_rules_summary(&server.domains, lang);
                    let hover_bg = p.neutral_hover();
                    let hover_group = SharedString::from(format!("dns-group-{tag}"));
                    div()
                        .id(SharedString::from(format!("dns-row-{tag}")))
                        .group(hover_group.clone())
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(p.border)
                        .bg(p.bg)
                        .hover(move |s| s.bg(hover_bg))
                        .child(
                            div()
                                .id(SharedString::from(format!("dns-name-{tag}")))
                                .w(px(super::theme::DNS_NAME_COL_WIDTH))
                                .flex_shrink_0()
                                .min_w(px(0.))
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .when(tag.chars().count() > 16, |d| {
                                    let tip = tag.clone();
                                    d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                })
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(p.fg)
                                        .min_w(px(0.))
                                        .truncate()
                                        .child(tag.clone()),
                                )
                                .when(tag != server.server_type.as_str(), |d| {
                                    d.child(pill(server.server_type.as_str(), p.muted_fg, p.panel_alt))
                                })
                                .when(is_final, |d| {
                                    d.child(pill(self.tr("默认", "Default"), p.primary, p.accent))
                                }),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("dns-addr-{tag}")))
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .text_color(p.muted_fg)
                                .truncate()
                                .when(address.chars().count() > 20, |d| {
                                    let tip = address.clone();
                                    d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                })
                                .child(address),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("dns-rules-{tag}")))
                                .flex_1()
                                .min_w(px(0.))
                                .text_sm()
                                .text_color(p.muted_fg)
                                .truncate()
                                .when(!server.domains.is_empty() && rules.chars().count() > 20, |d| {
                                    let tip = rules.clone();
                                    d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                })
                                .child(rules),
                        )
                        .child(
                            div()
                                .w(px(super::theme::DNS_ACTION_WIDTH))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_end()
                                .when(!is_final, |d| {
                                    d.child(
                                        div()
                                            .id(SharedString::from(format!("dns-set-default-{tag}")))
                                            .opacity(0.)
                                            .group_hover(hover_group, |s| s.opacity(1.))
                                            .cursor_pointer()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(p.primary)
                                            .whitespace_nowrap()
                                            .hover(|s| s.underline())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.select_default_dns(&action_tag, cx);
                                            }))
                                            .child(self.tr("设为默认", "Set as default")),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .child(
                                    more_button(
                                        SharedString::from(format!("dns-menu-{tag}")),
                                        self.tr("服务器操作", "Server actions"),
                                        cx,
                                    )
                                    .on_click(|_, _, cx| cx.stop_propagation())
                                    .dropdown_menu(menus::builder(cx.entity().downgrade(), menu_target.clone())),
                                ),
                        )
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
                                .when(!self.show_host_form, |b| b.primary())
                                .icon(if self.show_host_form {
                                    IconName::X
                                } else {
                                    IconName::Plus
                                })
                                .label(if self.show_host_form {
                                    self.tr("收起面板", "Collapse")
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
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .rounded_md()
                            .bg(p.bg)
                            .border_1()
                            .border_color(p.border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(inline_field_label(&p, self.tr("目标域名", "Domain")))
                                    .child(
                                        div()
                                            .w(px(280.))
                                            .max_w_full()
                                            .child(Input::new(&self.host_domain)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(inline_field_label(&p, self.tr("映射地址", "IP addresses")))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(Input::new(&self.host_ips)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .pt_3()
                                    .border_t_1()
                                    .border_color(p.border)
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(220.))
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(self.tr(
                                                "支持 IPv4 / IPv6 地址，多个地址用逗号分隔",
                                                "IPv4 or IPv6 addresses, comma-separated",
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .flex_shrink_0()
                                            .child(
                                                Button::new("cancel-host-form")
                                                    .compact()
                                                    .ghost()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .label(self.tr("取消", "Cancel"))
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.show_host_form = false;
                                                        cx.notify();
                                                    })),
                                            )
                                            .child(
                                                Button::new("save-host")
                                                    .compact()
                                                    .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                                    .primary()
                                                    .label(self.tr("保存解析", "Save mapping"))
                                                    .on_click(cx.listener(|this, _, window, cx| {
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
                                                                this.tr(
                                                                    "域名与 IP 不能为空",
                                                                    "Domain and IP are required",
                                                                )
                                                                .to_string(),
                                                            );
                                                            return;
                                                        }
                                                        let mut hosts = this.snapshot.model.dns.hosts.clone();
                                                        hosts.insert(domain, ips);
                                                        this.service.set_hosts(hosts);
                                                        for input in [&this.host_domain, &this.host_ips] {
                                                            input.update(cx, |s, cx| s.set_value("", window, cx));
                                                        }
                                                        this.show_host_form = false;
                                                        this.sync(cx);
                                                    })),
                                            ),
                                    ),
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

#[cfg(test)]
mod tests {
    use super::{dns_rules_summary, visible_dns_servers};
    use xrose_core::dns::{DnsPreset, apply_preset};
    use xrose_core::model::DnsConfig;
    use xrose_core::settings::Language;

    #[test]
    fn visible_dns_servers_hides_virtual_fakeip() {
        let mut config = DnsConfig::default();
        apply_preset(&mut config, DnsPreset::SplitFakeIp);
        let tags: Vec<&str> = visible_dns_servers(&config.servers)
            .into_iter()
            .map(|s| s.tag.as_str())
            .collect();
        assert!(!tags.contains(&"fakeip"));
        assert_eq!(tags, vec!["cn", "remote", "local"]);
    }

    #[test]
    fn dns_rules_summary_formats_empty_and_non_empty_domains() {
        assert_eq!(dns_rules_summary(&[], Language::Zh), "匹配所有域名");
        assert_eq!(dns_rules_summary(&[], Language::En), "All domains");
        let domains = vec!["geosite:cn".into(), "example.com".into()];
        assert_eq!(
            dns_rules_summary(&domains, Language::Zh),
            "geosite:cn, example.com"
        );
    }
}
