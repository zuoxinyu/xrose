//! Fixed work-mode choice above the sortable proxy nodes.
use super::*;
use xrose_core::model::{DIRECT_VIRTUAL_NODE_ID, WorkMode};

impl AppView {
    pub(super) fn direct_entry(
        &self,
        p: &Palette,
        automatic_group: bool,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.snapshot.model.work_mode == WorkMode::Direct;
        let label = self.tr("本机直连", "Direct");
        let detail = if self.snapshot.settings.effective_tun() {
            self.tr(
                "全局进程监控（TUN） · 拦截规则与 DNS 生效",
                "System-wide capture (TUN) · Firewall rules and DNS active",
            )
        } else {
            self.tr(
                "本地代理入站 · 开启 TUN 可捕获全部本机进程",
                "Local proxy traffic · Enable TUN to capture all processes",
            )
        };
        let primary = p.primary;
        let hover = p.panel_alt;
        let select = cx.listener(|this, _: &ClickEvent, window, cx| {
            this.run_command(Command::SelectWorkMode(WorkMode::Direct), false, window, cx);
        });
        div()
            .relative()
            .flex()
            .items_center()
            .rounded_lg()
            .border_1()
            .border_color(if selected {
                p.border.opacity(0.)
            } else {
                p.border
            })
            .bg(if selected { p.accent } else { p.bg })
            .when(selected, |row| {
                row.child(
                    div()
                        .absolute()
                        .left(px(1.))
                        .top(px(7.))
                        .bottom(px(7.))
                        .w(px(3.))
                        .rounded_full()
                        .bg(primary),
                )
            })
            .child(
                gpui_base::Radio::new(DIRECT_VIRTUAL_NODE_ID)
                    .checked(selected)
                    .disabled(busy)
                    .accessibility_label(label.clone())
                    .on_change(move |_, event, window, cx| select(event, window, cx))
                    .flex_1()
                    .min_w(px(0.))
                    .min_h(px(super::super::theme::NODE_ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl_4()
                    .pr_3()
                    .py_2()
                    .rounded_lg()
                    .border_1()
                    .border_color(p.border.opacity(0.))
                    .focus_visible(move |row| row.border_color(primary))
                    .when(!selected && !busy, |row| {
                        row.cursor_pointer().hover(move |s| s.bg(hover))
                    })
                    .when(busy, |row| {
                        row.opacity(super::super::theme::DISABLED_OPACITY)
                    })
                    .child(div().w(px(14.)).flex_shrink_0())
                    .child(radio_indicator(
                        selected,
                        if selected { primary } else { p.muted_fg },
                        primary,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
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
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(label),
                                    )
                                    .child(pill(
                                        self.tr("直连监控", "Direct monitor"),
                                        p.muted_fg,
                                        p.panel_alt,
                                    )),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(p.muted_fg)
                                    .child(detail),
                            ),
                    ),
            )
            .when(selected && automatic_group, |row| {
                row.child(
                    Button::new("resume-auto-proxy")
                        .compact()
                        .mr_3()
                        .disabled(busy)
                        .label(self.tr("恢复自动代理", "Resume automatic proxy"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run_command(
                                Command::SelectWorkMode(WorkMode::Proxy),
                                false,
                                window,
                                cx,
                            );
                        })),
                )
            })
    }
}
