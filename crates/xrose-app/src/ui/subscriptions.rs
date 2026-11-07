use super::menus::{self, MenuTarget};
use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::subscription::Subscription;
use xrose_core::util::format_bytes;

use super::AppView;
use super::theme::palette;
use super::widgets::*;

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
                        .flex_wrap()
                        .gap_2()
                        .child(section_title(
                            &p,
                            self.tr("订阅列表", "Subscription list"),
                            self.tr(
                                "管理节点订阅、自动更新与流量用量",
                                "Manage node subscriptions, automatic refresh, and data usage",
                            ),
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("refresh-all-subs")
                                        .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("全部更新", "Refresh all"))
                                        .loading(self.snapshot.busy.iter().any(|key| key.starts_with("sub:")))
                                        .disabled(subscriptions.is_empty())
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.service.refresh_all_subscriptions();
                                        })),
                                )
                                .child(
                                    Button::new("toggle-sub-form")
                                        .compact().h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label(self.tr("添加订阅","Add subscription"))
                                        .on_click(cx.listener(|this,_,window,cx|this.open_subscription_dialog(window,cx))),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted_fg)
                        .child(self.tr(
                            "订阅地址与节点凭据均加密存储于本地数据库。",
                            "Subscription URLs and node credentials are stored encrypted in the local database.",
                        )),
                ),
        );

        if subscriptions.is_empty() {
            body = body.child(card(&p).child(empty_hint(
                &p,
                self.tr("还没有订阅。可粘贴订阅地址或二维码，也可拖入图片文件。", "No subscriptions yet. Paste a subscription URL or QR code, or drop an image file."),
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
                    crate::i18n::relative_time(lang, ago)
                })
                .unwrap_or_else(|| self.tr("从未更新", "never").to_string());
            let sub_id_refresh = sub.id.clone();
            let menu_target = MenuTarget::Subscription(sub.id.clone());
            let refreshing = self
                .snapshot
                .busy
                .iter()
                .any(|key| key == &format!("sub:{}", sub.id));
            let expiring = usage
                .as_ref()
                .and_then(|u| u.expire)
                .is_some_and(|ts| ts >= now && ts - now <= 7 * 86400);
            let sub_for_toggle = sub.clone();

            body = body.child(
                card(&p)
                    .id(SharedString::from(format!("subscription-{}", sub.id)))
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
                                                    p.danger_text,
                                                    p.danger.opacity(super::theme::SEMANTIC_ALPHA),
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
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .label(self.tr("更新", "Refresh"))
                                        .loading(refreshing)
                                        .on_click(
                                            cx.listener(move |this, _, _, _| {
                                                this.service.refresh_subscription(&sub_id_refresh);
                                            }),
                                        ),
                                    )
                                    .child(
                                        more_button(
                                            SharedString::from(format!("sub-menu-{}", sub.id)),
                                            self.tr("订阅操作", "Subscription actions"),
                                        )
                                        .dropdown_menu(
                                            menus::builder(
                                                cx.entity().downgrade(),
                                                menu_target.clone(),
                                            ),
                                        ),
                                    )
                                    .child(
                                        toggle(
                                            SharedString::from(format!("sub-enabled-{}", sub.id)),
                                            "",
                                            sub.enabled,
                                        )
                                        .accessibility_label(
                                            self.tr("启用自动更新", "Enable automatic refresh"),
                                        )
                                        .tooltip(self.tr(
                                            "允许此订阅自动更新",
                                            "Allow automatic updates for this subscription",
                                        ))
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
                                    .h(px(super::theme::PROGRESS_HEIGHT))
                                    .w_full()
                                    .rounded_full()
                                    .bg(p.panel_alt)
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .h(px(super::theme::PROGRESS_HEIGHT))
                                            .w(relative((percent as f32 / 100.0).clamp(0.0, 1.0)))
                                            .bg(match usage_grade(percent) {
                                                2 => p.danger,
                                                1 => p.warning,
                                                _ => p.primary,
                                            }),
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
                            .child(
                                div()
                                    .text_color(if expired {
                                        p.danger_text
                                    } else if expiring {
                                        p.warn_text
                                    } else {
                                        p.muted_fg
                                    })
                                    .child(format!(
                                        "{}：{}",
                                        self.tr("到期", "Expires"),
                                        expire_text
                                    )),
                            )
                            .child(format!(
                                "{}：{}",
                                self.tr("最后更新", "Updated"),
                                last_updated
                            )),
                    )
                    .when(sub.last_error.is_some(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(p.danger_text)
                                .child(sub.last_error.clone().unwrap_or_default()),
                        )
                    })
                    .when(!sub.enabled, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(p.warn_text)
                                .child(self.tr("已暂停自动更新", "Auto update paused")),
                        )
                    })
                    .context_menu(menus::builder(cx.entity().downgrade(), menu_target)),
            );
        }

        let _ = lang;
        body
    }
}

fn usage_grade(percent: f32) -> u8 {
    if percent >= 95. {
        2
    } else if percent >= 80. {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn usage_thresholds_are_inclusive() {
        assert_eq!(super::usage_grade(79.9), 0);
        assert_eq!(super::usage_grade(80.), 1);
        assert_eq!(super::usage_grade(94.9), 1);
        assert_eq!(super::usage_grade(95.), 2);
    }
}
