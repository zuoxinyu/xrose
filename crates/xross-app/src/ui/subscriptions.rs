use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xross_core::subscription::Subscription;
use xross_core::util::{format_bytes, format_duration_secs};

use super::theme::palette;
use super::widgets::*;
use super::AppView;

impl AppView {
    pub(crate) fn page_subscriptions(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let lang = self.lang();
        let subscriptions = self.snapshot.subscriptions.clone();
        let now = chrono::Utc::now().timestamp();

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
                            self.tr("订阅", "Subscriptions"),
                            self.tr(
                                "定时更新，支持 ETag / If-Modified-Since 增量拉取",
                                "Scheduled refresh with ETag / If-Modified-Since",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    Button::new("refresh-all-subs")
                                        .compact()
                                        .label(self.tr("全部更新", "Refresh all"))
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.refresh_all_subscriptions();
                                        })),
                                )
                                .child(
                                    Button::new("toggle-sub-form")
                                        .compact()
                                        .primary()
                                        .label(if self.show_subscription_form {
                                            self.tr("收起", "Close")
                                        } else {
                                            self.tr("添加订阅", "Add")
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.show_subscription_form = !this.show_subscription_form;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .when(self.show_subscription_form, |d| {
                    d.child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(180.)).child(Input::new(&self.sub_name)))
                            .child(div().flex_1().child(Input::new(&self.sub_url)))
                            .child(
                                Button::new("add-subscription")
                                    .compact()
                                    .primary()
                                    .label(self.tr("添加", "Add"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let name = this.sub_name.read(cx).value().to_string();
                                        let url = this.sub_url.read(cx).value().to_string();
                                        match this.service.add_subscription(name, url) {
                                            Ok(_) => {
                                                this.sub_url.update(cx, |input, cx| {
                                                    input.set_value("", window, cx);
                                                });
                                                this.show_subscription_form = false;
                                            }
                                            Err(err) => {
                                                this.service.toast(
                                                    xross_core::service::ToastLevel::Error,
                                                    err.to_string(),
                                                );
                                            }
                                        }
                                        this.sync(cx);
                                    })),
                            ),
                    )
                })
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr(
                            "订阅 token 与节点凭据使用 ChaCha20-Poly1305 加密后存入本地数据库；自定义 UA / Header 在节点卡片中不可见但会被发送。",
                            "Subscription tokens and node credentials are encrypted with ChaCha20-Poly1305; custom UA/headers are sent on refresh.",
                        )),
                ),
        );

        if subscriptions.is_empty() {
            body = body.child(card(&p).child(empty_hint(
                &p,
                self.tr("还没有订阅，点击右上角添加", "No subscriptions yet"),
            )));
        }

        for subscription in subscriptions {
            let sub: Subscription = subscription;
            let usage = sub.usage.clone();
            let expire_text = usage
                .as_ref()
                .and_then(|u| u.expire)
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|dt| dt.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| self.tr("未知", "unknown").to_string());
            let remaining_text = usage
                .as_ref()
                .map(|u| match u.remaining() {
                    Some(_remaining) => {
                        format!("{} / {}", format_bytes(u.used()), format_bytes(u.total))
                    }
                    None => format!("{} / ∞", format_bytes(u.used())),
                })
                .unwrap_or_else(|| self.tr("未提供流量信息", "No usage info").to_string());
            let percent = usage.as_ref().map(|u| u.percent_used()).unwrap_or(0.0);
            let expired = usage.as_ref().map(|u| u.expired()).unwrap_or(false);
            let last_updated = sub
                .last_updated
                .map(|ts| {
                    let ago = (now - ts).max(0);
                    format!("{} {}", format_duration_secs(ago), self.tr("前", "ago"))
                })
                .unwrap_or_else(|| self.tr("从未更新", "never").to_string());
            let sub_id_refresh = sub.id.clone();
            let sub_id_delete = sub.id.clone();
            let sub_for_toggle = sub.clone();

            body = body.child(
                card(&p)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
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
                                                    .text_base()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(p.fg)
                                                    .child(sub.name.clone()),
                                            )
                                            .when(expired, |d| {
                                                d.child(pill(
                                                    self.tr("已过期", "expired"),
                                                    p.primary_fg,
                                                    p.danger,
                                                ))
                                            })
                                            .child(pill(
                                                format!(
                                                    "{} {}",
                                                    sub.node_count,
                                                    self.tr("节点", "nodes")
                                                ),
                                                p.muted_fg,
                                                p.panel_alt,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .child(sub.masked_url()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "refresh-{}",
                                            sub.id
                                        )))
                                        .compact()
                                        .label(self.tr("更新", "Refresh"))
                                        .on_click(
                                            cx.listener(move |this, _, _, _| {
                                                this.service.refresh_subscription(&sub_id_refresh);
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "delete-sub-{}",
                                            sub.id
                                        )))
                                        .compact()
                                        .danger()
                                        .label(self.tr("删除", "Delete"))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                let _ = this
                                                    .service
                                                    .delete_subscription(&sub_id_delete);
                                                this.sync(cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        toggle(
                                            SharedString::from(format!("sub-enabled-{}", sub.id)),
                                            "",
                                            sub.enabled,
                                        )
                                        .on_change(
                                            cx.listener(move |this, checked, _, cx| {
                                                let mut updated = sub_for_toggle.clone();
                                                updated.enabled = *checked;
                                                let _ = this
                                                    .service
                                                    .update_subscription_settings(updated);
                                                this.sync(cx);
                                            }),
                                        ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(remaining_text)
                                    .child(format!("{percent:.1}%")),
                            )
                            .child(
                                div()
                                    .h(px(6.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(p.panel_alt)
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h(px(6.))
                                            .w(relative((percent as f32 / 100.0).clamp(0.0, 1.0)))
                                            .bg(if percent > 90.0 { p.danger } else { p.primary }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_5()
                            .text_xs()
                            .text_color(p.muted_fg)
                            .child(format!("{}：{}", self.tr("到期", "Expires"), expire_text))
                            .child(format!(
                                "{}：{}",
                                self.tr("最后更新", "Updated"),
                                last_updated
                            ))
                            .when(sub.etag.is_some(), |d| {
                                d.child(pill("ETag", p.muted_fg, p.panel_alt))
                            })
                            .when(sub.usage.is_some(), |d| {
                                d.child(pill("userinfo", p.muted_fg, p.panel_alt))
                            }),
                    )
                    .when(sub.last_error.is_some(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(p.danger)
                                .child(sub.last_error.clone().unwrap_or_default()),
                        )
                    })
                    .when(!sub.enabled, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(p.warning)
                                .child(self.tr("已暂停自动更新", "Auto update paused")),
                        )
                    }),
            );
        }

        let _ = lang;
        body
    }
}
