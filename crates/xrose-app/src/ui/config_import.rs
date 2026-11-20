//! Unified configuration import dialog: file/clipboard source, summary review,
//! selective sections, and merge/replace confirmation.

use std::collections::BTreeSet;

use gpui_base::Disableable;
use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use xrose_core::config_import::{
    ConfigFormat, ConfigImportSummary, ConfigImportView, ImportMode, ImportOptions, ImportSection,
    SectionCounts,
};
use xrose_core::service::ToastLevel;
use xrose_core::settings::Language;

use super::dialogs::{dialog_footer, settings_dialog};
use super::theme::{CONTROL_HEIGHT, DISABLED_OPACITY, Palette, palette};
use super::widgets::{ChoiceGroup, dialog_control_row, divider, pill, toggle};
use super::{AppView, tr};

#[derive(Default)]
pub(super) struct ConfigImportUi {
    review_id: Option<String>,
    mode: ImportMode,
    sections: BTreeSet<ImportSection>,
    profile_index: usize,
    commit_requested: bool,
    inline_error: Option<String>,
}

impl ConfigImportUi {
    pub(super) fn reset(&mut self) {
        self.review_id = None;
        self.mode = ImportMode::Merge;
        self.sections.clear();
        self.profile_index = 0;
        self.commit_requested = false;
        self.inline_error = None;
    }

    fn sync_from_view(&mut self, view: &ConfigImportView) {
        if self.review_id.as_deref() != Some(view.id.as_str()) {
            self.review_id = Some(view.id.clone());
            self.mode = ImportMode::Merge;
            self.profile_index = view.summary.selected_profile;
            self.sections = view.summary.available_sections.iter().copied().collect();
            self.commit_requested = false;
            self.inline_error = None;
        }
    }

    fn effective_selected(&self, summary: &ConfigImportSummary) -> BTreeSet<ImportSection> {
        let available: BTreeSet<ImportSection> =
            summary.available_sections.iter().copied().collect();
        self.sections.intersection(&available).copied().collect()
    }
}

fn format_label(format: ConfigFormat, lang: Language) -> SharedString {
    match lang {
        Language::Zh => format.label().into(),
        Language::En => format.en_label().into(),
    }
}

fn section_label(section: ImportSection, lang: Language) -> SharedString {
    match section {
        ImportSection::Nodes => tr(lang, "代理节点", "Proxy nodes"),
        ImportSection::Groups => tr(lang, "出站组", "Outbound groups"),
        ImportSection::Routing => tr(lang, "路由规则与规则集", "Routing rules & rule sets"),
        ImportSection::Dns => tr(lang, "DNS 配置", "DNS configuration"),
        ImportSection::Subscriptions => tr(lang, "节点订阅", "Subscriptions"),
    }
}

fn section_detail(section: ImportSection, counts: &SectionCounts, lang: Language) -> SharedString {
    if !counts.has_section(section) {
        return tr(lang, "无内容", "None");
    }
    match section {
        ImportSection::Nodes => match lang {
            Language::Zh => format!("{} 个节点", counts.nodes).into(),
            Language::En => format!(
                "{} {}",
                counts.nodes,
                if counts.nodes == 1 { "node" } else { "nodes" }
            )
            .into(),
        },
        ImportSection::Groups => match lang {
            Language::Zh => format!("{} 个出站组", counts.groups).into(),
            Language::En => format!(
                "{} {}",
                counts.groups,
                if counts.groups == 1 {
                    "group"
                } else {
                    "groups"
                }
            )
            .into(),
        },
        ImportSection::Routing => {
            let mut parts = Vec::new();
            if counts.rules > 0 {
                parts.push(match lang {
                    Language::Zh => format!("{} 条规则", counts.rules),
                    Language::En => format!(
                        "{} {}",
                        counts.rules,
                        if counts.rules == 1 { "rule" } else { "rules" }
                    ),
                });
            }
            if counts.rule_groups > 0 {
                parts.push(match lang {
                    Language::Zh => format!("{} 个分组", counts.rule_groups),
                    Language::En => format!(
                        "{} {}",
                        counts.rule_groups,
                        if counts.rule_groups == 1 {
                            "group"
                        } else {
                            "groups"
                        }
                    ),
                });
            }
            if counts.rule_sets > 0 {
                parts.push(match lang {
                    Language::Zh => format!("{} 个规则集", counts.rule_sets),
                    Language::En => format!(
                        "{} {}",
                        counts.rule_sets,
                        if counts.rule_sets == 1 {
                            "rule set"
                        } else {
                            "rule sets"
                        }
                    ),
                });
            }
            parts.join(" · ").into()
        }
        ImportSection::Dns => {
            let mut parts = Vec::new();
            if counts.dns_servers > 0 {
                parts.push(match lang {
                    Language::Zh => format!("{} 个服务器", counts.dns_servers),
                    Language::En => format!(
                        "{} {}",
                        counts.dns_servers,
                        if counts.dns_servers == 1 {
                            "server"
                        } else {
                            "servers"
                        }
                    ),
                });
            }
            if counts.dns_hosts > 0 {
                parts.push(match lang {
                    Language::Zh => format!("{} 条 Hosts", counts.dns_hosts),
                    Language::En => format!(
                        "{} {}",
                        counts.dns_hosts,
                        if counts.dns_hosts == 1 {
                            "host"
                        } else {
                            "hosts"
                        }
                    ),
                });
            }
            if counts.dns_fake_ip {
                parts.push("Fake-IP".into());
            }
            parts.join(" · ").into()
        }
        ImportSection::Subscriptions => match lang {
            Language::Zh => format!("{} 条订阅", counts.subscriptions).into(),
            Language::En => format!(
                "{} {}",
                counts.subscriptions,
                if counts.subscriptions == 1 {
                    "subscription"
                } else {
                    "subscriptions"
                }
            )
            .into(),
        },
    }
}

impl AppView {
    pub(super) fn pick_config_import_file(&mut self, cx: &mut Context<Self>) {
        self.config_import_ui.inline_error = None;
        self.config_import_ui.commit_requested = false;
        let result = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(self.tr(
                "选择配置文件（YAML / JSON）",
                "Select a configuration file (YAML / JSON)",
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
                    service.open_config_import_file(path);
                }
            }
            Ok(Ok(None)) => {}
            _ => service.toast(ToastLevel::Error, error.to_string()),
        })
        .detach();
    }

    pub(super) fn read_config_import_clipboard(&mut self, cx: &mut Context<Self>) {
        self.config_import_ui.inline_error = None;
        self.config_import_ui.commit_requested = false;
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        if text.trim().is_empty() {
            self.config_import_ui.inline_error = Some(
                self.tr(
                    "剪贴板中没有文本内容，请先复制配置内容或选择文件",
                    "Clipboard does not contain text. Copy a configuration or choose a file.",
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let label = self.tr("剪贴板", "Clipboard").to_string();
        self.service.open_config_import_text(text, Some(label));
        self.sync(cx);
    }

    pub(super) fn open_config_import_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.service.clear_config_import();
        self.config_import_ui.reset();
        self.sync(cx);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                this.render_config_import_dialog(dialog, window, cx)
            })
        });
    }

    fn render_config_import_dialog(
        &mut self,
        dialog: Dialog,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let p = palette(cx);
        let lang = self.lang();
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|k| k == "config-import" || k == "core");
        let import_busy = self.snapshot.busy.iter().any(|k| k == "config-import");
        let review = self.snapshot.config_import.clone();
        if let Some(view) = &review {
            self.config_import_ui.sync_from_view(view);
        }

        let (body, footer_actions) = if let Some(view) = review {
            self.config_import_review_body(&view, busy, import_busy, &p, lang, cx)
        } else if import_busy {
            (
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .py_8()
                    .child(Spinner::new().small())
                    .child(div().text_sm().text_color(p.muted_fg).child(
                        if self.config_import_ui.commit_requested {
                            tr(lang, "正在导入配置…", "Importing configuration…")
                        } else {
                            tr(lang, "正在解析配置文件…", "Parsing configuration…")
                        },
                    )),
                div().flex().justify_end().gap_2().child(
                    Button::new("cfg-import-cancel-busy")
                        .disabled(true)
                        .label(tr(lang, "取消", "Cancel")),
                ),
            )
        } else if self.config_import_ui.commit_requested {
            (
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .py_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Icon::new(IconName::Check)
                                    .size(px(16.))
                                    .text_color(p.ok_text),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(p.fg)
                                    .child(tr(lang, "配置已导入", "Configuration imported")),
                            ),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(tr(
                        lang,
                        "所选模块已保存到当前配置。如内核正在运行，将按热重载设置自动应用，或可点击顶栏提示手动应用。",
                        "Selected sections have been saved. If a core is running, changes apply via hot reload or from the pending-changes banner.",
                    ))),
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cfg-import-another")
                            .compact()
                            .h(px(CONTROL_HEIGHT))
                            .label(tr(lang, "导入其他文件", "Import another"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.config_import_ui.reset();
                                this.pick_config_import_file(cx);
                            })),
                    )
                    .child(
                        Button::new("cfg-import-done")
                            .primary()
                            .label(tr(lang, "完成", "Done"))
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
        } else {
            self.config_import_source_body(busy, &p, lang, cx)
        };

        let close_view = cx.entity().downgrade();
        let error = self.config_import_ui.inline_error.clone();
        settings_dialog(
            dialog,
            tr(lang, "导入配置", "Import configuration"),
            body,
            &p,
            window,
        )
        .overlay_closable(false)
        .footer(dialog_footer(&p, error, footer_actions))
        .on_cancel(move |_, _, _| !import_busy)
        .on_close(move |_, _, cx| {
            let _ = close_view.update(cx, |this, cx| {
                this.service.clear_config_import();
                this.config_import_ui.reset();
                this.sync(cx);
            });
        })
    }

    fn config_import_source_body(
        &self,
        busy: bool,
        p: &Palette,
        lang: Language,
        cx: &mut Context<Self>,
    ) -> (Div, Div) {
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_md()
                    .bg(p.bg)
                    .border_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.fg)
                            .child(tr(lang, "选择配置来源", "Choose a configuration source")),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(tr(
                        lang,
                        "支持自动识别 Clash / Mihomo YAML、sing-box JSON、Xray JSON 与 Xrose JSON 备份。解析后可先预览摘要、勾选导入模块，再选择合并或覆盖。",
                        "Automatically detects Clash / Mihomo YAML, sing-box JSON, Xray JSON, and Xrose JSON backups. You can review the summary, select sections, and choose merge or replace before applying.",
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .pt_1()
                            .child(
                                Button::new("cfg-pick-file")
                                    .compact()
                                    .h(px(CONTROL_HEIGHT))
                                    .primary()
                                    .disabled(busy)
                                    .icon(IconName::FolderOpen)
                                    .label(tr(lang, "选择配置文件…", "Choose file…"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pick_config_import_file(cx);
                                    })),
                            )
                            .child(
                                Button::new("cfg-read-clipboard")
                                    .compact()
                                    .h(px(CONTROL_HEIGHT))
                                    .disabled(busy)
                                    .icon(IconName::ClipboardPaste)
                                    .label(tr(lang, "从剪贴板解析", "Parse clipboard"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.read_config_import_clipboard(cx);
                                    })),
                            ),
                    ),
            )
            .child(div().text_xs().text_color(p.muted_fg).child(tr(
                lang,
                "本地文件路径规则集与未知协议会自动跳过并在预览中列出，不会读取除所选文件外的其他本机文件。",
                "Local rule-set paths and unsupported protocols are skipped with warnings; no other local files are read.",
            )));

        let actions = div().flex().justify_end().gap_2().child(
            Button::new("cfg-import-close")
                .label(tr(lang, "取消", "Cancel"))
                .on_click(|_, window, cx| window.close_dialog(cx)),
        );
        (body, actions)
    }

    fn config_import_review_body(
        &self,
        view: &ConfigImportView,
        busy: bool,
        import_busy: bool,
        p: &Palette,
        lang: Language,
        cx: &mut Context<Self>,
    ) -> (Div, Div) {
        let summary = &view.summary;
        let selected = self.config_import_ui.effective_selected(summary);
        let is_replace = self.config_import_ui.mode == ImportMode::Replace;
        let groups_without_nodes = selected.contains(&ImportSection::Groups)
            && !selected.contains(&ImportSection::Nodes)
            && summary.counts.nodes > 0;

        let mut source_meta = div().flex().flex_wrap().items_center().gap_2().child(pill(
            format_label(summary.format, lang),
            p.primary,
            p.accent,
        ));
        if let Some(name) = &summary.source_name
            && !name.trim().is_empty()
        {
            source_meta = source_meta.child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(p.fg)
                    .child(name.clone()),
            );
        }
        if let Some(exported_at) = &summary.exported_at {
            source_meta = source_meta.child(
                div()
                    .text_xs()
                    .text_color(p.muted_fg)
                    .child(exported_at.clone()),
            );
        }

        let mut body = div().flex().flex_col().gap_3().pb_1().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .p_3()
                .rounded_md()
                .bg(p.bg)
                .border_1()
                .border_color(p.border)
                .child(source_meta)
                .child(
                    Button::new("cfg-change-file")
                        .compact()
                        .h(px(CONTROL_HEIGHT))
                        .ghost()
                        .disabled(busy)
                        .label(tr(lang, "更换文件", "Change file"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pick_config_import_file(cx);
                        })),
                ),
        );

        // Profile selector when an Xrose backup contains multiple profiles.
        if summary.profiles.len() > 1 {
            let review_id = view.id.clone();
            let labels: Vec<SharedString> = summary
                .profiles
                .iter()
                .enumerate()
                .map(|(idx, profile)| {
                    if profile.name.trim().is_empty() {
                        format!("{} {}", tr(lang, "配置档", "Profile"), idx + 1).into()
                    } else {
                        profile.name.clone().into()
                    }
                })
                .collect();
            let profile_choice = ChoiceGroup::new(
                "cfg-import-profile",
                labels,
                self.config_import_ui
                    .profile_index
                    .min(summary.profiles.len() - 1),
            )
            .small()
            .disabled(busy)
            .on_change(cx.listener(move |this, index: &usize, _, cx| {
                match this
                    .service
                    .select_config_import_profile(&review_id, *index)
                {
                    Ok(next) => {
                        this.config_import_ui.profile_index = next.selected_profile;
                        this.config_import_ui.sections =
                            next.available_sections.into_iter().collect();
                        this.config_import_ui.inline_error = None;
                        this.sync(cx);
                    }
                    Err(err) => {
                        this.config_import_ui.inline_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            }));
            body = body.child(dialog_control_row(
                p,
                tr(lang, "备份配置档", "Backup profile"),
                tr(
                    lang,
                    "该备份包含多个配置档，请选择要导入的配置档。",
                    "This backup contains multiple profiles. Choose one to import.",
                ),
                profile_choice,
            ));
        }

        // Import mode (Merge vs Replace).
        let mode_choice = ChoiceGroup::new(
            "cfg-import-mode",
            vec![
                tr(lang, "合并到当前配置", "Merge with current"),
                tr(lang, "覆盖所选模块", "Replace selected"),
            ],
            usize::from(is_replace),
        )
        .small()
        .disabled(busy)
        .on_change(cx.listener(|this, index: &usize, _, cx| {
            this.config_import_ui.mode = if *index == 1 {
                ImportMode::Replace
            } else {
                ImportMode::Merge
            };
            this.config_import_ui.inline_error = None;
            cx.notify();
        }));
        let mode_desc = if is_replace {
            tr(
                lang,
                "将替换当前配置中已勾选模块的内容，未勾选模块与本机组网网关保持不变。",
                "Replaces current content in the checked sections. Unchecked sections and local mesh gateways stay untouched.",
            )
        } else {
            tr(
                lang,
                "保留已有配置，按指纹与地址去重追加新节点、订阅与规则分组。",
                "Keeps existing configuration and appends deduplicated nodes, subscriptions, and rule groups.",
            )
        };
        body = body
            .child(dialog_control_row(
                p,
                tr(lang, "导入方式", "Import mode"),
                "",
                mode_choice,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(if is_replace { p.warn_text } else { p.muted_fg })
                    .child(mode_desc),
            )
            .child(divider(p));

        // Selective sections list.
        let mut sections_box = div().flex().flex_col().gap_2().child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.fg)
                .child(tr(lang, "导入模块", "Sections to import")),
        );
        for section in ImportSection::ALL {
            let available = summary.counts.has_section(section);
            let checked = available && self.config_import_ui.sections.contains(&section);
            let detail = section_detail(section, &summary.counts, lang);
            sections_box = sections_box.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(p.bg)
                    .border_1()
                    .border_color(p.border)
                    .when(!available, |d| d.opacity(DISABLED_OPACITY))
                    .child(
                        toggle(
                            SharedString::from(format!("cfg-section-{section:?}")),
                            section_label(section, lang),
                            checked,
                        )
                        .disabled(!available || busy)
                        .on_change(cx.listener(move |this, on, _, cx| {
                            if *on {
                                this.config_import_ui.sections.insert(section);
                            } else {
                                this.config_import_ui.sections.remove(&section);
                            }
                            this.config_import_ui.inline_error = None;
                            cx.notify();
                        })),
                    )
                    .child(div().text_xs().text_color(p.muted_fg).child(detail)),
            );
        }
        if groups_without_nodes {
            sections_box = sections_box.child(div().text_xs().text_color(p.muted_fg).child(tr(
                lang,
                "未勾选「代理节点」时，出站组仅关联本机已存在的同名或同指纹节点。",
                "Without Proxy nodes selected, outbound groups only bind to existing local nodes matching by fingerprint or name.",
            )));
        }
        body = body.child(sections_box);

        // Compatibility & skipped warnings.
        if !summary.warnings.is_empty() {
            const MAX_VISIBLE: usize = 8;
            let mut warn_box = div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_md()
                .bg(p.warning.opacity(0.10))
                .border_1()
                .border_color(p.warning.opacity(0.35))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.warn_text)
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(14.))
                                .text_color(p.warn_text),
                        )
                        .child(match lang {
                            Language::Zh => {
                                format!("解析与兼容提示（{} 条）", summary.warnings.len())
                            }
                            Language::En => {
                                format!("Compatibility notices ({})", summary.warnings.len())
                            }
                        }),
                );
            for warning in summary.warnings.iter().take(MAX_VISIBLE) {
                warn_box = warn_box.child(
                    div()
                        .text_xs()
                        .text_color(p.warn_text)
                        .child(format!("• {warning}")),
                );
            }
            if summary.warnings.len() > MAX_VISIBLE {
                let remaining = summary.warnings.len() - MAX_VISIBLE;
                warn_box =
                    warn_box.child(div().text_xs().text_color(p.muted_fg).child(match lang {
                        Language::Zh => format!("其余 {remaining} 条提示已省略"),
                        Language::En => format!("{remaining} more notices omitted"),
                    }));
            }
            body = body.child(warn_box);
        }

        let commit_id = view.id.clone();
        let cancel_id = view.id.clone();
        let can_submit = !busy && !selected.is_empty();
        let submit_label = if is_replace {
            tr(lang, "覆盖并导入", "Replace & import")
        } else {
            tr(lang, "确认导入", "Import")
        };

        let actions = div()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .child(
                Button::new("cfg-import-cancel")
                    .disabled(import_busy)
                    .label(tr(lang, "取消", "Cancel"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.service.cancel_config_import(&cancel_id);
                        this.config_import_ui.reset();
                        window.close_dialog(cx);
                    })),
            )
            .child(
                Button::new("cfg-import-confirm")
                    .primary()
                    .disabled(!can_submit)
                    .loading(import_busy)
                    .label(submit_label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let options = ImportOptions {
                            mode: this.config_import_ui.mode,
                            sections: this.config_import_ui.sections.clone(),
                            profile_index: this.config_import_ui.profile_index,
                        };
                        this.config_import_ui.commit_requested = true;
                        this.config_import_ui.inline_error = None;
                        this.service
                            .confirm_config_import(commit_id.clone(), options);
                        this.sync(cx);
                    })),
            );

        (body, actions)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigFormat, ConfigImportSummary, ConfigImportUi, ConfigImportView, ImportMode,
        ImportSection, Language, SectionCounts, section_detail,
    };

    #[test]
    fn section_detail_handles_singular_plural_and_empty_counts() {
        let empty = SectionCounts::default();
        assert_eq!(
            section_detail(ImportSection::Nodes, &empty, Language::Zh),
            "无内容"
        );
        assert_eq!(
            section_detail(ImportSection::Nodes, &empty, Language::En),
            "None"
        );

        let single = SectionCounts {
            nodes: 1,
            groups: 1,
            rules: 1,
            rule_groups: 1,
            rule_sets: 1,
            dns_servers: 1,
            dns_hosts: 1,
            dns_fake_ip: true,
            subscriptions: 1,
        };
        assert_eq!(
            section_detail(ImportSection::Nodes, &single, Language::En),
            "1 node"
        );
        assert_eq!(
            section_detail(ImportSection::Routing, &single, Language::En),
            "1 rule · 1 group · 1 rule set"
        );
        assert_eq!(
            section_detail(ImportSection::Dns, &single, Language::Zh),
            "1 个服务器 · 1 条 Hosts · Fake-IP"
        );
        assert_eq!(
            section_detail(ImportSection::Subscriptions, &single, Language::En),
            "1 subscription"
        );

        let multi = SectionCounts {
            nodes: 3,
            groups: 2,
            rules: 5,
            rule_groups: 0,
            rule_sets: 2,
            dns_servers: 2,
            dns_hosts: 0,
            dns_fake_ip: false,
            subscriptions: 2,
        };
        assert_eq!(
            section_detail(ImportSection::Nodes, &multi, Language::En),
            "3 nodes"
        );
        assert_eq!(
            section_detail(ImportSection::Routing, &multi, Language::En),
            "5 rules · 2 rule sets"
        );
        assert_eq!(
            section_detail(ImportSection::Dns, &multi, Language::En),
            "2 servers"
        );
    }

    #[test]
    fn sync_from_view_selects_only_available_sections_for_each_new_review() {
        let mut ui = ConfigImportUi::default();
        let view = ConfigImportView {
            id: "rev-1".into(),
            summary: ConfigImportSummary {
                format: ConfigFormat::ClashYaml,
                source_name: Some("fixture.yaml".into()),
                exported_at: None,
                counts: SectionCounts {
                    nodes: 2,
                    rules: 4,
                    ..Default::default()
                },
                available_sections: vec![ImportSection::Nodes, ImportSection::Routing],
                profiles: Vec::new(),
                selected_profile: 0,
                warnings: Vec::new(),
            },
        };
        ui.sync_from_view(&view);
        assert_eq!(ui.mode, ImportMode::Merge);
        assert_eq!(
            ui.effective_selected(&view.summary),
            [ImportSection::Nodes, ImportSection::Routing]
                .into_iter()
                .collect()
        );
        ui.mode = ImportMode::Replace;
        ui.sections.remove(&ImportSection::Routing);
        // Same review id preserves user edits.
        ui.sync_from_view(&view);
        assert_eq!(ui.mode, ImportMode::Replace);
        assert_eq!(
            ui.effective_selected(&view.summary),
            [ImportSection::Nodes].into_iter().collect()
        );
    }
}
