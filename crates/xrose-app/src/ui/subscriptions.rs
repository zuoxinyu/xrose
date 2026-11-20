use super::menus::{self, MenuTarget};
use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::settings::Language;
use xrose_core::subscription::Subscription;
use xrose_core::util::format_bytes;

use super::AppView;
use super::theme::palette;
use super::widgets::*;

pub(super) const SUBSCRIPTION_INTERVAL_PRESETS: [u64; 6] = [1, 6, 12, 24, 72, 168];

pub(super) fn interval_preset_label(hours: u64, lang: Language) -> SharedString {
    if hours >= 48 && hours.is_multiple_of(24) {
        let days = hours / 24;
        match lang {
            Language::Zh => format!("{days} 天").into(),
            Language::En => format!("{days}d").into(),
        }
    } else {
        match lang {
            Language::Zh => format!("{hours} 小时").into(),
            Language::En => format!("{hours}h").into(),
        }
    }
}

pub(super) fn format_interval_every(hours: u64, lang: Language) -> String {
    if hours >= 48 && hours.is_multiple_of(24) {
        let days = hours / 24;
        match lang {
            Language::Zh => format!("每 {days} 天"),
            Language::En => format!("Every {days}d"),
        }
    } else {
        match lang {
            Language::Zh => format!("每 {hours} 小时"),
            Language::En => format!("Every {hours}h"),
        }
    }
}

impl AppView {
    pub(super) fn open_subscription_interval_dialog(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current =
            Subscription::clamp_interval_hours(self.snapshot.settings.subscription_update_hours);
        let hours_input =
            cx.new(|cx| InputState::new(window, cx).default_value(current.to_string()));
        let error = cx.new(|_| None::<String>);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let lang = this.lang();
                let parsed = Subscription::parse_interval_hours(&hours_input.read(cx).value()).ok();
                let preset_selected =
                    parsed.and_then(|h| SUBSCRIPTION_INTERVAL_PRESETS.iter().position(|&v| v == h));
                let preset_input = hours_input.clone();
                let preset_error = error.clone();
                let presets = ChoiceGroup::new(
                    "global-sub-interval-presets",
                    SUBSCRIPTION_INTERVAL_PRESETS
                        .iter()
                        .map(|&h| interval_preset_label(h, lang))
                        .collect(),
                    preset_selected,
                )
                .small()
                .on_change(move |index: &usize, window, cx| {
                    let hours = SUBSCRIPTION_INTERVAL_PRESETS[*index];
                    preset_input.update(cx, |input, cx| {
                        input.set_value(hours.to_string(), window, cx)
                    });
                    preset_error.update(cx, |e, _| *e = None);
                    window.refresh();
                });
                let form = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .pb_1()
                    .child(dialog_control_row(
                        &p,
                        this.tr("常用间隔", "Presets"),
                        "",
                        presets,
                    ))
                    .child(dialog_control_row(
                        &p,
                        this.tr("自定义间隔", "Custom interval"),
                        this.tr(
                            "1–720 小时。对所有未单独指定间隔的订阅生效。",
                            "1–720 hours. Applies to subscriptions using the default interval.",
                        ),
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .w(px(super::theme::SETTING_LABEL_WIDTH))
                                    .child(Input::new(&hours_input)),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(p.muted_fg)
                                    .child(this.tr("小时", "hours")),
                            ),
                    ));
                let dialog_error = error.read(cx).clone();
                let (hours_input, error) = (hours_input.clone(), error.clone());
                super::dialogs::dialog_shell(
                    dialog,
                    this.tr("默认更新间隔", "Default update interval"),
                    super::theme::DIALOG_WIDE,
                    &p,
                )
                .child(form)
                .overlay_closable(false)
                .footer(super::dialogs::dialog_footer(
                    &p,
                    dialog_error,
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-sub-interval")
                                .label(this.tr("取消", "Cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save-sub-interval")
                                .primary()
                                .label(this.tr("保存", "Save"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    match Subscription::parse_interval_hours(
                                        &hours_input.read(cx).value(),
                                    )
                                    .and_then(|hours| {
                                        this.service.set_subscription_update_hours(hours)
                                    }) {
                                        Ok(()) => {
                                            window.close_dialog(cx);
                                            this.sync(cx);
                                        }
                                        Err(err) => {
                                            error.update(cx, |e, _| *e = Some(err.to_string()));
                                            window.refresh();
                                        }
                                    }
                                })),
                        ),
                ))
            })
        });
    }

    pub(super) fn open_edit_subscription(
        &self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(sub) = self.snapshot.subscriptions.iter().find(|s| s.id == id) else {
            return;
        };
        let id = sub.id.clone();
        let server_hours = sub
            .server_update_interval_secs
            .filter(|&secs| secs > 0)
            .map(|secs| Subscription::clamp_interval_hours(secs / 3600));
        let global_hours =
            Subscription::clamp_interval_hours(self.snapshot.settings.subscription_update_hours);
        let initial_custom = sub.custom_update_interval_secs.is_some();
        let initial_hours = sub
            .custom_update_interval_secs
            .filter(|&secs| secs > 0)
            .map(|secs| Subscription::clamp_interval_hours(secs / 3600))
            .unwrap_or_else(|| server_hours.unwrap_or(global_hours));
        let name = cx.new(|cx| InputState::new(window, cx).default_value(sub.name.clone()));
        let url = cx.new(|cx| InputState::new(window, cx).default_value(sub.url.clone()));
        let custom_mode = cx.new(|_| initial_custom);
        let interval_input =
            cx.new(|cx| InputState::new(window, cx).default_value(initial_hours.to_string()));
        let error = cx.new(|_| None::<String>);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let lang = this.lang();
                let is_custom = *custom_mode.read(cx);
                let mode_state = custom_mode.clone();
                let mode_error = error.clone();
                let mode_choice = ChoiceGroup::new(
                    "sub-interval-mode",
                    vec![
                        this.tr("跟随默认", "Use default"),
                        this.tr("自定义", "Custom"),
                    ],
                    usize::from(is_custom),
                )
                .small()
                .on_change(move |index: &usize, window, cx| {
                    mode_state.update(cx, |custom, _| *custom = *index == 1);
                    mode_error.update(cx, |e, _| *e = None);
                    window.refresh();
                });

                let default_desc: SharedString = match (server_hours, lang) {
                    (Some(hours), Language::Zh) => format!(
                        "当前按服务端建议（{}）；选择自定义后不再被服务端响应头覆盖。",
                        format_interval_every(hours, lang)
                    )
                    .into(),
                    (Some(hours), Language::En) => format!(
                        "Currently using server hint ({}); custom intervals ignore server headers.",
                        format_interval_every(hours, lang)
                    )
                    .into(),
                    (None, Language::Zh) => format!(
                        "跟随全局默认（{}）；服务端返回更新间隔时会自动采用，自定义后不再覆盖。",
                        format_interval_every(global_hours, lang)
                    )
                    .into(),
                    (None, Language::En) => format!(
                        "Follows global default ({}); adopts server hints unless overridden.",
                        format_interval_every(global_hours, lang)
                    )
                    .into(),
                };

                let parsed_custom =
                    Subscription::parse_interval_hours(&interval_input.read(cx).value()).ok();
                let preset_selected = parsed_custom
                    .and_then(|h| SUBSCRIPTION_INTERVAL_PRESETS.iter().position(|&v| v == h));
                let preset_input = interval_input.clone();
                let preset_error = error.clone();
                let preset_choice = ChoiceGroup::new(
                    "sub-interval-presets",
                    SUBSCRIPTION_INTERVAL_PRESETS
                        .iter()
                        .map(|&h| interval_preset_label(h, lang))
                        .collect(),
                    preset_selected,
                )
                .small()
                .on_change(move |index: &usize, window, cx| {
                    let hours = SUBSCRIPTION_INTERVAL_PRESETS[*index];
                    preset_input.update(cx, |input, cx| {
                        input.set_value(hours.to_string(), window, cx)
                    });
                    preset_error.update(cx, |e, _| *e = None);
                    window.refresh();
                });

                let form = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pb_1()
                    .child(dialog_field(
                        &p,
                        this.tr("名称（可选）", "Name (optional)"),
                        &name,
                        FieldWidth::Medium,
                    ))
                    .child(dialog_field(
                        &p,
                        this.tr("订阅地址", "Subscription URL"),
                        &url,
                        FieldWidth::Full,
                    ))
                    .child(dialog_control_row(
                        &p,
                        this.tr("自动更新间隔", "Auto-update interval"),
                        if is_custom {
                            this.tr(
                                "自定义间隔优先于全局默认与服务端建议（1–720 小时）。",
                                "Overrides both global default and server hints (1–720 hours).",
                            )
                        } else {
                            default_desc
                        },
                        mode_choice,
                    ))
                    .when(is_custom, |d| {
                        d.child(dialog_control_row(
                            &p,
                            this.tr("间隔时长", "Interval"),
                            "",
                            div()
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_2()
                                .child(preset_choice)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .w(px(super::theme::SETTING_LABEL_WIDTH))
                                                .child(Input::new(&interval_input)),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(p.muted_fg)
                                                .child(this.tr("小时", "hours")),
                                        ),
                                ),
                        ))
                    });
                let dialog_error = error.read(cx).clone();
                let (name, url, id, custom_mode, interval_input, error) = (
                    name.clone(),
                    url.clone(),
                    id.clone(),
                    custom_mode.clone(),
                    interval_input.clone(),
                    error.clone(),
                );
                super::dialogs::settings_dialog(
                    dialog,
                    this.tr("编辑订阅", "Edit subscription"),
                    form,
                    &p,
                    window,
                )
                .overlay_closable(false)
                .footer(super::dialogs::dialog_footer(
                    &p,
                    dialog_error,
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-sub-edit")
                                .label(this.tr("取消", "Cancel"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("save-sub-edit")
                                .primary()
                                .label(this.tr("保存", "Save"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let custom_hours = if *custom_mode.read(cx) {
                                        match Subscription::parse_interval_hours(
                                            &interval_input.read(cx).value(),
                                        ) {
                                            Ok(hours) => Some(hours),
                                            Err(err) => {
                                                error.update(cx, |e, _| *e = Some(err.to_string()));
                                                window.refresh();
                                                return;
                                            }
                                        }
                                    } else {
                                        None
                                    };
                                    match this.service.edit_subscription_with_interval(
                                        &id,
                                        &name.read(cx).value(),
                                        &url.read(cx).value(),
                                        custom_hours,
                                    ) {
                                        Ok(()) => {
                                            window.close_dialog(cx);
                                            this.sync(cx);
                                        }
                                        Err(err) => {
                                            error.update(cx, |e, _| *e = Some(err.to_string()));
                                            window.refresh();
                                        }
                                    }
                                })),
                        ),
                ))
            })
        });
    }

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
                        .items_start()
                        .justify_between()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(section_title(
                                    &p,
                                    self.tr("订阅列表", "Subscription list"),
                                    self.tr(
                                        "管理节点订阅、自动更新与流量用量",
                                        "Manage node subscriptions, automatic refresh, and data usage",
                                    ),
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    more_button(
                                        "subscriptions-more",
                                        self.tr("更多操作", "More actions"),
                                        cx,
                                    )
                                    .dropdown_menu(menus::builder(
                                        cx.entity().downgrade(),
                                        MenuTarget::Subscriptions,
                                    )),
                                )
                                .child(
                                    Button::new("refresh-all-subs")
                                        .compact()
                                        .h(px(crate::ui::theme::CONTROL_HEIGHT))
                                        .icon(IconName::RefreshCw)
                                        .label(self.tr("全部更新", "Refresh all"))
                                        .loading(
                                            self.snapshot
                                                .busy
                                                .iter()
                                                .any(|key| key.starts_with("sub:")),
                                        )
                                        .disabled(subscriptions.is_empty())
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.service.refresh_all_subscriptions();
                                            this.sync(cx);
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
                .unwrap_or_else(|| self.tr("未知", "Unknown").to_string());
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
                .unwrap_or_else(|| self.tr("从未更新", "Never").to_string());
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
            let toggle_id = sub.id.clone();
            let full_url = SharedString::from(sub.url.clone());
            let effective_hours = sub
                .effective_update_interval_hours(self.snapshot.settings.subscription_update_hours);
            let interval_text = {
                let every = format_interval_every(effective_hours, lang);
                if sub.custom_update_interval_secs.is_some() {
                    format!(
                        "{}：{}（{}）",
                        self.tr("自动更新", "Auto refresh"),
                        every,
                        self.tr("自定义", "custom")
                    )
                } else if sub.server_update_interval_secs.is_some() {
                    format!(
                        "{}：{}（{}）",
                        self.tr("自动更新", "Auto refresh"),
                        every,
                        self.tr("服务端", "server")
                    )
                } else {
                    format!("{}：{}", self.tr("自动更新", "Auto refresh"), every)
                }
            };

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
                                                    self.tr("已过期", "Expired"),
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
                                            ))
                                            .when(refreshing, |d| {
                                                d.child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_1()
                                                        .flex_shrink_0()
                                                        .px_2()
                                                        .py_0p5()
                                                        .rounded_full()
                                                        .bg(p.accent)
                                                        .text_xs()
                                                        .text_color(p.primary)
                                                        .child(
                                                            Spinner::new()
                                                                .xsmall()
                                                                .color(p.primary),
                                                        )
                                                        .child(self.tr("更新中…", "Updating…")),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(p.muted_fg)
                                            .id(SharedString::from(format!(
                                                "subscription-url-{}",
                                                sub.id
                                            )))
                                            .tooltip(move |window, cx| {
                                                Tooltip::new(full_url.clone())
                                                    .max_w(px(440.))
                                                    .build(window, cx)
                                            })
                                            .child(sub.masked_url()),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        more_button(
                                            SharedString::from(format!("sub-menu-{}", sub.id)),
                                            self.tr("订阅操作", "Subscription actions"),
                                            cx,
                                        )
                                        .dropdown_menu(
                                            menus::builder(
                                                cx.entity().downgrade(),
                                                menu_target.clone(),
                                            ),
                                        ),
                                    )
                                    .child(
                                        icon_button(
                                            SharedString::from(format!("refresh-{}", sub.id)),
                                            IconName::RefreshCw,
                                            self.tr("更新订阅", "Refresh subscription"),
                                            cx,
                                        )
                                        .loading(refreshing)
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.service.refresh_subscription(&sub_id_refresh);
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
                                        .disabled(refreshing)
                                        .accessibility_label(
                                            self.tr("启用自动更新", "Enable automatic refresh"),
                                        )
                                        .tooltip(self.tr(
                                            "允许此订阅自动更新",
                                            "Allow automatic updates for this subscription",
                                        ))
                                        .on_change(
                                            cx.listener(move |this, checked, _, cx| {
                                                if let Err(error) = this
                                                    .service
                                                    .set_subscription_enabled(&toggle_id, *checked)
                                                {
                                                    this.service.toast(
                                                        xrose_core::service::ToastLevel::Error,
                                                        error.to_string(),
                                                    );
                                                }
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
                                            .w(relative((percent / 100.0).clamp(0.0, 1.0)))
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
                            .child(div().when(refreshing, |d| d.text_color(p.primary)).child(
                                if refreshing && sub.last_updated.is_none() {
                                    self.tr("正在拉取节点…", "Fetching nodes…").to_string()
                                } else if refreshing {
                                    format!(
                                        "{}：{}",
                                        self.tr("最后更新", "Updated"),
                                        self.tr("更新中…", "Updating…")
                                    )
                                } else {
                                    format!("{}：{}", self.tr("最后更新", "Updated"), last_updated)
                                },
                            ))
                            .when(sub.enabled, |d| d.child(div().child(interval_text))),
                    )
                    .when(!refreshing && sub.last_error.is_some(), |d| {
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
    use xrose_core::settings::Language;

    #[test]
    fn usage_thresholds_are_inclusive() {
        assert_eq!(super::usage_grade(79.9), 0);
        assert_eq!(super::usage_grade(80.), 1);
        assert_eq!(super::usage_grade(94.9), 1);
        assert_eq!(super::usage_grade(95.), 2);
    }

    #[test]
    fn interval_labels_format_hours_and_multi_day_presets() {
        assert_eq!(super::interval_preset_label(1, Language::Zh), "1 小时");
        assert_eq!(super::interval_preset_label(24, Language::Zh), "24 小时");
        assert_eq!(super::interval_preset_label(72, Language::Zh), "3 天");
        assert_eq!(super::interval_preset_label(168, Language::En), "7d");
        assert_eq!(super::format_interval_every(12, Language::Zh), "每 12 小时");
        assert_eq!(super::format_interval_every(72, Language::En), "Every 3d");
    }
}
