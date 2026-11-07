//! On-demand resolver comparisons and host diagnostics on the testing page.
use super::{
    AppView,
    theme::{CONTROL_HEIGHT, Palette, palette},
    widgets::{card, section_title},
};
use gpui_base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::*;
use xrose_core::dns::probe::ProbeRow;
use xrose_core::service::ToastLevel;

const RESOLVER_WIDTH: f32 = 140.;
const LATENCY_WIDTH: f32 = 72.;

impl AppView {
    pub(super) fn dns_comparison_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let busy = self.snapshot.busy.iter().any(|s| s == "dns-probe");
        let title = section_title(
            &p,
            self.tr("DNS 解析对比", "DNS comparison"),
            self.tr(
                "使用上方链路测试的域名，并发查询各解析器的 A 与 AAAA 记录",
                "Compare A and AAAA records for the host entered in Link trace",
            ),
        );
        let compare = Button::new("compare-dns")
            .compact()
            .h(px(CONTROL_HEIGHT))
            .label(self.tr("对比解析", "Compare resolvers"))
            .loading(busy)
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| {
                let target = this.trace_input.read(cx).value();
                if let Err(e) = this.service.compare_dns(&target) {
                    this.service.toast(ToastLevel::Error, e.to_string());
                }
                this.sync(cx);
            }));
        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .flex_wrap()
            .child(title)
            .child(compare);
        let mut body = card(&p).child(header);
        let Some(result) = self.snapshot.dns_comparison.as_ref() else {
            return body;
        };

        let addresses = format!(
            "{} · {}",
            self.tr("地址与归属地", "Addresses and locations"),
            result.host
        );
        let columns = div()
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .text_color(p.muted_fg)
            .child(
                div()
                    .w(px(RESOLVER_WIDTH))
                    .child(self.tr("解析器", "Resolver")),
            )
            .child(div().flex_1().child(addresses))
            .child(div().w(px(LATENCY_WIDTH)).child(self.tr("耗时", "Latency")));
        body = body
            .child(columns)
            .children(result.rows.iter().map(|row| self.dns_result_row(row, &p)));
        let warning = || div().text_xs().text_color(p.warn_text);
        if result.rows.iter().any(|row| row.reserved) {
            body = body.child(warning().child(self.tr(
                "部分结果属于本地或保留地址段，请核对内网 DNS 与 Fake-IP 设置",
                "Some answers are local or reserved. Check private DNS and Fake-IP settings",
            )));
        }
        if result.different_subnets {
            body = body.child(warning().child(self.tr(
                "不同解析器返回的网段不同，CDN 调度与分域策略也可能产生差异",
                "Resolvers returned different prefixes. CDNs and split DNS can also cause this",
            )));
        }
        body
    }

    fn dns_result_row(&self, row: &ProbeRow, p: &Palette) -> Div {
        let mut answers = div().flex_1().min_w(px(0.)).flex().flex_col().gap_1();
        for (ip, location) in row.addresses.iter().zip(&row.locations) {
            // Results can outlive a privacy toggle. Do not expose a location
            // gathered before privacy mode was enabled.
            let value = if self.snapshot.settings.privacy_mode {
                ip.to_string()
            } else {
                format!("{ip} · {location}")
            };
            answers = answers.child(div().text_sm().text_color(p.fg).child(value));
        }
        if let Some(error) = &row.error {
            answers = answers.child(div().text_xs().text_color(p.warn_text).child(error.clone()));
        } else if row.addresses.is_empty() {
            answers = answers.child(self.tr("无地址记录", "No address records"));
        }
        div()
            .flex()
            .gap_2()
            .items_start()
            .border_t_1()
            .border_color(p.border)
            .pt_2()
            .child(
                div()
                    .w(px(RESOLVER_WIDTH))
                    .text_sm()
                    .child(row.resolver.clone()),
            )
            .child(answers)
            .child(
                div()
                    .w(px(LATENCY_WIDTH))
                    .text_sm()
                    .child(format!("{} ms", row.elapsed_ms)),
            )
    }

    pub(super) fn system_report_card(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let report = self.snapshot.network_report.as_ref();
        let busy = self.snapshot.busy.iter().any(|s| s == "network-report");
        let title = section_title(
            &p, self.tr("系统网络与环境诊断", "System network diagnostics"),
            self.tr("查看网卡、默认路由、系统 DNS 与代理、端口监听和提权助手状态",
                    "Inspect interfaces, routes, system DNS and proxies, listeners, and the privilege helper"),
        );
        let refresh = Button::new("refresh-network-report")
            .compact()
            .h(px(CONTROL_HEIGHT))
            .label(self.tr("刷新诊断", "Refresh"))
            .loading(busy)
            .disabled(busy)
            .on_click(cx.listener(|this, _, _, cx| {
                this.service.refresh_network_report();
                this.sync(cx);
            }));
        let copy = Button::new("copy-network-report")
            .compact()
            .h(px(CONTROL_HEIGHT))
            .ghost()
            .label(self.tr("复制报告", "Copy report"))
            .disabled(report.is_none() || busy)
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(text) = this.service.network_report_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    this.service.toast(
                        ToastLevel::Success,
                        this.tr("已复制诊断报告", "Diagnostic report copied")
                            .to_string(),
                    );
                }
            }));
        let actions = div().flex().gap_2().child(refresh).child(copy);
        let header = div()
            .flex()
            .items_start()
            .justify_between()
            .gap_3()
            .flex_wrap()
            .child(title)
            .child(actions);
        let body = card(&p).child(header);
        let Some(report) = report else { return body };
        body.children(report.rows.iter().map(|row| {
            let detail = row.detail(self.snapshot.settings.privacy_mode).to_owned();
            div()
                .flex()
                .gap_3()
                .items_start()
                .text_sm()
                .child(
                    div()
                        .w(px(RESOLVER_WIDTH))
                        .text_color(p.muted_fg)
                        .child(row.label.clone()),
                )
                .child(div().flex_1().min_w(px(0.)).text_color(p.fg).child(detail))
        }))
    }
}
