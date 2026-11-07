//! File pickers and review of additive routing imports.
use gpui_base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::path::Path;
use xrose_core::route::transfer::{ExportScope, ImportSummary, SkipReason};
use xrose_core::service::ToastLevel;

use super::{AppView, theme, widgets::section_title};

impl AppView {
    pub(super) fn pick_route_import(&self, cx: &mut Context<Self>) {
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.tr(
                "选择 Xrose 或 SwitchyOmega 规则文件",
                "Select an Xrose or SwitchyOmega rule file",
            )),
        });
        let service = self.service.clone();
        let error = self.tr(
            "无法打开文件选择器，请重试",
            "Could not open the file picker. Please retry.",
        );
        cx.spawn(async move |_, _| match result.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    service.open_route_file(path);
                }
            }
            Ok(Ok(None)) => {}
            _ => service.toast(ToastLevel::Error, error.to_string()),
        })
        .detach();
    }

    pub(super) fn pick_route_export(&self, scope: ExportScope, cx: &mut Context<Self>) {
        let name = match scope {
            ExportScope::RuleSets => "xrose-rule-sets.json",
            ExportScope::Group(_) => "xrose-rule-group.json",
            ExportScope::Rule(_) => "xrose-rule.json",
            ExportScope::All => "xrose-routing.json",
        };
        let result = cx.prompt_for_new_path(Path::new("."), Some(name));
        let service = self.service.clone();
        let error = self.tr(
            "无法打开保存对话框，请重试",
            "Could not open the save dialog. Please retry.",
        );
        cx.spawn(async move |_, _| match result.await {
            Ok(Ok(Some(path))) => service.export_route_file(scope, path),
            Ok(Ok(None)) => {}
            _ => service.toast(ToastLevel::Error, error.to_string()),
        })
        .detach();
    }

    pub(super) fn route_import_review(
        &self,
        id: String,
        report: &ImportSummary,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = theme::palette(cx);
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|b| b == "route-file" || b == "core");
        let cancel_id = id.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .bg(p.bg)
            .child(section_title(
                &p,
                self.tr("导入预览", "Import preview"),
                format!(
                    "{} · {} {} · {} {} · {} {}",
                    report.format,
                    report.rules,
                    self.tr("条规则", "rules"),
                    report.groups,
                    self.tr("个分组", "groups"),
                    report.rule_sets,
                    self.tr("个规则集", "rule sets")
                ),
            ))
            .child(div().text_sm().text_color(p.muted_fg).child(self.tr(
                "规则和分组追加到各自列表末尾；相同的规则集只保留一份。",
                "Rules and groups are appended to their lists. Identical rule sets are kept once.",
            )))
            .when(report.catch_all, |d| {
                d.child(div().text_sm().text_color(p.warning).child(self.tr(
                    "包含匹配全部 TCP / UDP 流量的规则。该规则启用时，排在其后的规则不会命中。",
                    "Contains a TCP/UDP catch-all. When enabled, rules after it will not match.",
                )))
            })
            .when(report.skipped > 0, |d| {
                d.child(div().text_sm().text_color(p.warning).child(format!(
                        "{} {} · {} {}",
                        self.tr("跳过条件：", "Skipped conditions:"),
                        report.skipped,
                        self.tr("源文件行号（前 8 条）：", "Source lines (first 8):"),
                        report
                            .skipped_lines
                            .iter()
                            .map(|s| {
                                let reason = match s.reason {
                                    SkipReason::Url => self.tr("URL 条件", "URL condition"),
                                    SkipReason::BrowserCondition => {
                                        self.tr("浏览器条件", "browser condition")
                                    }
                                    SkipReason::InvalidPattern => {
                                        self.tr("不支持的模式", "unsupported pattern")
                                    }
                                };
                                format!("{} ({reason})", s.line)
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    )))
            })
            .when(report.fallback_profiles > 0, |d| {
                d.child(div().text_sm().text_color(p.warning).child(format!(
                    "{} {} {}",
                    self.tr("未匹配：", "Unmatched:"),
                    report.fallback_profiles,
                    self.tr(
                        "个情景模式或出站组，将使用默认代理。",
                        "profiles or groups will use the default proxy."
                    )
                )))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-route-import")
                            .compact()
                            .label(self.tr("取消", "Cancel"))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.cancel_route_import(&cancel_id);
                                this.sync(cx);
                            })),
                    )
                    .child(
                        Button::new("confirm-route-import")
                            .compact()
                            .primary()
                            .label(self.tr("确认导入", "Import"))
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.confirm_route_file(id.clone());
                                this.sync(cx);
                            })),
                    ),
            )
    }
}
