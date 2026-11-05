//! Shared menu descriptions for button and context-menu entry points.
use super::{AppView, tr};
use gpui_kit::component::WindowExt;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::*;
use xross_core::service::ToastLevel;

#[derive(Clone)]
pub(super) enum MenuTarget {
    NodeSelector,
    FrontProxy,
    Node(String),
    Nodes,
    Rule(String, Option<String>),
    Group(String),
    Subscription(String),
    Dns(String),
    Host(String),
    Connections,
}

#[derive(Clone)]
pub(super) enum Command {
    SelectNode(String, String),
    SetFrontProxy(Option<String>),
    EditNode(String),
    TestNode(String),
    DownloadNode(String),
    CopyNode(String),
    QrNode(String),
    DeleteNode(String),
    DeleteNodes(Vec<String>),
    BatchNodes,
    CleanNodes,
    MoveRule(String, Option<String>, isize),
    AssignRule(String, Option<String>),
    DeleteRule(String),
    RenameGroup(String),
    EnableGroup(String),
    MoveGroup(String, isize),
    DeleteGroup(String),
    RefreshSubscription(String),
    DeleteSubscription(String),
    DeleteDns(String),
    DeleteHost(String),
    RefreshConnections,
    CloseConnections,
    ClearRecording,
    ClearLogs,
    DeleteMesh(String),
    DeleteVersion(xross_core::model::CoreKind, String),
}

pub(super) fn builder(
    view: WeakEntity<AppView>,
    target: MenuTarget,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + Clone {
    move |menu, window, cx| {
        let Some(view) = view.upgrade() else {
            return menu;
        };
        let handle = cx.entity().downgrade();
        view.update(cx, |this, cx| {
            if this
                .action_menu
                .as_ref()
                .is_some_and(|old| old.entity_id() != handle.entity_id())
            {
                this.dismiss_action_menu(cx);
            }
            this.action_menu = Some(handle);
            this.build_menu(target.clone(), menu, window, cx)
        })
    }
}

fn item(
    view: &WeakEntity<AppView>,
    label: SharedString,
    enabled: bool,
    command: Command,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label)
        .disabled(!enabled)
        .on_click(move |_, window, cx| {
            let _ = view.update(cx, |this, cx| {
                this.run_command(command.clone(), false, window, cx)
            });
        })
}

impl AppView {
    pub(super) fn dismiss_action_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = self.action_menu.take() {
            let _ = menu.update(cx, |_, cx| cx.emit(DismissEvent));
        }
    }

    fn build_menu(
        &mut self,
        target: MenuTarget,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        self.snapshot = self.service.snapshot();
        let view = cx.entity().downgrade();
        let lang = self.lang();
        let busy = |key: &str| self.snapshot.busy.iter().any(|s| s == key);
        let add = |zh, en, enabled, command: Command| {
            let available = !busy("core")
                || matches!(
                    &command,
                    Command::CopyNode(_)
                        | Command::QrNode(_)
                        | Command::BatchNodes
                        | Command::ClearLogs
                        | Command::ClearRecording
                );
            item(&view, tr(lang, zh, en), enabled && available, command)
        };
        match target {
            MenuTarget::FrontProxy => {
                let model = &self.snapshot.model;
                menu = menu.item(
                    add("关闭", "Off", true, Command::SetFrontProxy(None))
                        .checked(model.front_proxy.is_none()),
                );
                for node in model
                    .nodes
                    .iter()
                    .filter(|n| n.usable_with(self.snapshot.settings.core))
                {
                    let name = if self.hide_ips && !node.server.is_empty() {
                        node.name.replace(&node.server, "•••")
                    } else {
                        node.name.clone()
                    };
                    menu = menu.item(
                        item(
                            &view,
                            name.into(),
                            !busy("core"),
                            Command::SetFrontProxy(Some(node.id.clone())),
                        )
                        .checked(model.front_proxy.as_ref() == Some(&node.id)),
                    );
                }
            }
            MenuTarget::NodeSelector => {
                let group = self
                    .selected_group
                    .as_deref()
                    .and_then(|id| self.snapshot.model.group(id))
                    .or_else(|| {
                        self.snapshot
                            .model
                            .default_group_id()
                            .and_then(|id| self.snapshot.model.group(&id))
                    });
                let Some(group) = group else {
                    return menu.label(tr(lang, "请先添加节点", "Add a node first"));
                };
                let automatic = group.kind != xross_core::model::GroupKind::Selector;
                if automatic {
                    menu = menu.label(tr(
                        lang,
                        "此出站组自动选择节点",
                        "This group selects nodes automatically",
                    ));
                }
                let selected = self.snapshot.selected_node(Some(&group.id));
                for id in &group.members {
                    let Some(node) = self.snapshot.model.node(id) else {
                        continue;
                    };
                    let name = if self.hide_ips && !node.server.is_empty() {
                        node.name.replace(&node.server, "•••")
                    } else {
                        node.name.clone()
                    };
                    menu = menu.item(
                        item(
                            &view,
                            format!(
                                "{name}  {}",
                                super::theme::latency_label(
                                    self.snapshot.latency.get(&node.id),
                                    busy("latency") && self.latency_pending.contains(&node.id),
                                    lang,
                                )
                            )
                            .into(),
                            !busy("core") && group.enabled && node.enabled && !automatic,
                            Command::SelectNode(group.id.clone(), node.id.clone()),
                        )
                        .checked(selected.is_some_and(|n| n.id == node.id)),
                    );
                }
                if group.members.is_empty() {
                    menu = menu.label(tr(lang, "此出站组暂无节点", "No nodes in this group"));
                }
            }
            MenuTarget::Node(id) => {
                let Some(node) = self.snapshot.model.node(&id) else {
                    return menu;
                };
                let testing = busy("latency") || busy("download") || busy("core");
                menu = menu
                    .item(add(
                        "编辑",
                        "Edit",
                        !busy("core"),
                        Command::EditNode(id.clone()),
                    ))
                    .item(add(
                        "HTTP 测速",
                        "HTTP test",
                        node.enabled && !testing,
                        Command::TestNode(id.clone()),
                    ))
                    .item(add(
                        "下载测速",
                        "Download test",
                        node.enabled && !testing,
                        Command::DownloadNode(id.clone()),
                    ))
                    .separator()
                    .item(add(
                        "复制链接",
                        "Copy link",
                        true,
                        Command::CopyNode(id.clone()),
                    ))
                    .item(add("二维码", "QR code", true, Command::QrNode(id.clone())))
                    .separator()
                    .item(add(
                        "删除节点",
                        "Delete node",
                        !busy("core"),
                        Command::DeleteNode(id),
                    ));
            }
            MenuTarget::Nodes => {
                let failed = self.snapshot.latency.values().any(|e| e.delay_ms.is_none());
                menu = menu
                    .item(add(
                        if self.batch_mode {
                            "退出批量管理"
                        } else {
                            "批量管理"
                        },
                        if self.batch_mode {
                            "Exit batch mode"
                        } else {
                            "Batch manage"
                        },
                        true,
                        Command::BatchNodes,
                    ))
                    .separator()
                    .item(add(
                        "清理超时节点",
                        "Remove timed-out nodes",
                        failed && !busy("latency"),
                        Command::CleanNodes,
                    ));
            }
            MenuTarget::Rule(id, group) => {
                let rules = match &group {
                    Some(gid) => match self
                        .snapshot
                        .model
                        .routing
                        .rule_groups
                        .iter()
                        .find(|g| &g.id == gid)
                    {
                        Some(g) => &g.rules,
                        None => return menu,
                    },
                    None => &self.snapshot.model.routing.rules,
                };
                let Some(index) = rules.iter().position(|r| r.id == id) else {
                    return menu;
                };
                menu = menu
                    .item(add(
                        "上移",
                        "Move up",
                        index > 0,
                        Command::MoveRule(id.clone(), group.clone(), -1),
                    ))
                    .item(add(
                        "下移",
                        "Move down",
                        index + 1 < rules.len(),
                        Command::MoveRule(id.clone(), group.clone(), 1),
                    ));
                let groups = self.snapshot.model.routing.rule_groups.clone();
                let can_edit = !busy("core");
                let sub_view = view.clone();
                let sub_id = id.clone();
                let submenu = PopupMenu::build(window, cx, move |mut sub, _, _| {
                    for g in &groups {
                        sub = sub.item(item(
                            &sub_view,
                            g.name.clone().into(),
                            can_edit && group.as_deref() != Some(&g.id),
                            Command::AssignRule(sub_id.clone(), Some(g.id.clone())),
                        ));
                    }
                    sub.separator().item(item(
                        &sub_view,
                        tr(lang, "未分组 / 预设", "Ungrouped / preset"),
                        can_edit && group.is_some(),
                        Command::AssignRule(sub_id.clone(), None),
                    ))
                });
                menu = menu
                    .item(PopupMenuItem::submenu(
                        tr(lang, "移动到分组", "Move to group"),
                        submenu,
                    ))
                    .separator()
                    .item(add(
                        "删除规则",
                        "Delete rule",
                        true,
                        Command::DeleteRule(id),
                    ));
            }
            MenuTarget::Group(id) => {
                let groups = &self.snapshot.model.routing.rule_groups;
                let Some(index) = groups.iter().position(|g| g.id == id) else {
                    return menu;
                };
                menu = menu
                    .item(add(
                        "重命名",
                        "Rename",
                        true,
                        Command::RenameGroup(id.clone()),
                    ))
                    .item(add(
                        if groups[index].enabled {
                            "禁用分组"
                        } else {
                            "启用分组"
                        },
                        if groups[index].enabled {
                            "Disable group"
                        } else {
                            "Enable group"
                        },
                        true,
                        Command::EnableGroup(id.clone()),
                    ))
                    .separator()
                    .item(add(
                        "上移",
                        "Move up",
                        index > 0,
                        Command::MoveGroup(id.clone(), -1),
                    ))
                    .item(add(
                        "下移",
                        "Move down",
                        index + 1 < groups.len(),
                        Command::MoveGroup(id.clone(), 1),
                    ))
                    .separator()
                    .item(add(
                        "解散分组",
                        "Ungroup rules",
                        true,
                        Command::DeleteGroup(id),
                    ));
            }
            MenuTarget::Subscription(id) => {
                if !self.snapshot.subscriptions.iter().any(|s| s.id == id) {
                    return menu;
                }
                let available = !busy(&format!("sub:{id}"));
                menu = menu
                    .item(add(
                        "更新",
                        "Refresh",
                        available,
                        Command::RefreshSubscription(id.clone()),
                    ))
                    .separator()
                    .item(add(
                        "删除订阅",
                        "Delete subscription",
                        available,
                        Command::DeleteSubscription(id),
                    ));
            }
            MenuTarget::Dns(id) => {
                let reason = self.snapshot.dns_deletion_reason(&id);
                if reason.is_some() {
                    menu = menu.label(tr(
                        lang,
                        "请先解除默认 DNS 或 Fake-IP 引用",
                        "Remove the default DNS or Fake-IP reference first",
                    ));
                }
                menu = menu.item(add(
                    "删除服务器",
                    "Delete server",
                    reason.is_none(),
                    Command::DeleteDns(id),
                ));
            }
            MenuTarget::Host(id) => {
                menu = menu.item(add(
                    "删除静态解析",
                    "Delete host mapping",
                    true,
                    Command::DeleteHost(id),
                ))
            }
            MenuTarget::Connections => {
                let available = self.snapshot.process.phase.is_alive()
                    && self.snapshot.settings.core.capabilities().clash_connections;
                menu = menu
                    .item(add(
                        "刷新",
                        "Refresh",
                        available,
                        Command::RefreshConnections,
                    ))
                    .separator()
                    .item(add(
                        "全部断开",
                        "Close all connections",
                        available && !self.snapshot.runtime.connections.is_empty(),
                        Command::CloseConnections,
                    ))
                    .item(add(
                        "清空录制",
                        "Clear recording",
                        self.snapshot.recording.started_at.is_some(),
                        Command::ClearRecording,
                    ));
            }
        }
        menu
    }

    pub(super) fn run_command(
        &mut self,
        command: Command,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.snapshot = self.service.snapshot();
        let command = if matches!(command, Command::CleanNodes) {
            Command::DeleteNodes(
                self.snapshot
                    .model
                    .nodes
                    .iter()
                    .filter(|n| {
                        self.snapshot
                            .latency
                            .get(&n.id)
                            .is_some_and(|e| e.delay_ms.is_none())
                    })
                    .map(|n| n.id.clone())
                    .collect(),
            )
        } else {
            command
        };
        let busy = |key: &str| self.snapshot.busy.iter().any(|s| s == key);
        let exists = match &command {
            Command::SelectNode(group_id, node_id) => {
                self.snapshot.model.group(group_id).is_some_and(|g| {
                    g.enabled
                        && g.kind == xross_core::model::GroupKind::Selector
                        && g.members.contains(node_id)
                }) && self.snapshot.model.node(node_id).is_some_and(|n| n.enabled)
            }
            Command::DeleteVersion(core, version) => {
                !busy("install")
                    && self
                        .snapshot
                        .binaries
                        .iter()
                        .any(|b| &b.core == core && &b.version == version)
            }
            Command::DeleteMesh(id) => self.snapshot.mesh.profiles.iter().any(|p| &p.id == id),
            Command::EditNode(id)
            | Command::TestNode(id)
            | Command::DownloadNode(id)
            | Command::CopyNode(id)
            | Command::QrNode(id)
            | Command::DeleteNode(id) => self.snapshot.model.node(id).is_some(),
            Command::DeleteRule(id) | Command::AssignRule(id, _) | Command::MoveRule(id, _, _) => {
                self.snapshot
                    .model
                    .routing
                    .rule_groups
                    .iter()
                    .flat_map(|g| &g.rules)
                    .chain(self.snapshot.model.routing.rules.iter())
                    .any(|r| &r.id == id)
            }
            Command::RenameGroup(id)
            | Command::EnableGroup(id)
            | Command::MoveGroup(id, _)
            | Command::DeleteGroup(id) => self
                .snapshot
                .model
                .routing
                .rule_groups
                .iter()
                .any(|g| &g.id == id),
            Command::DeleteDns(id) => self.snapshot.model.dns.servers.iter().any(|s| &s.tag == id),
            Command::DeleteHost(id) => self.snapshot.model.dns.hosts.contains_key(id),
            Command::RefreshSubscription(id) | Command::DeleteSubscription(id) => {
                self.snapshot.subscriptions.iter().any(|s| &s.id == id)
                    && !busy(&format!("sub:{id}"))
            }
            _ => true,
        };
        if !exists {
            return;
        }
        if !matches!(
            &command,
            Command::CopyNode(_)
                | Command::QrNode(_)
                | Command::BatchNodes
                | Command::ClearLogs
                | Command::ClearRecording
        ) && busy("core")
        {
            return;
        }
        if matches!(&command, Command::DeleteNodes(_)) && (busy("latency") || busy("download")) {
            return;
        }
        if matches!(
            &command,
            Command::EditNode(_)
                | Command::DeleteNode(_)
                | Command::TestNode(_)
                | Command::DownloadNode(_)
        ) && busy("core")
        {
            return;
        }
        if matches!(&command, Command::TestNode(_) | Command::DownloadNode(_))
            && (busy("latency") || busy("download"))
        {
            return;
        }
        let confirmation = match &command {
            Command::DeleteMesh(_) => Some((self.tr("删除组网配置", "Delete mesh profile"), self.tr("删除此组网配置，并移除关联出口节点与启动选项。", "Remove this mesh profile, its exit node, and startup selection."))),
            Command::DeleteVersion(core, version) => Some((self.tr("删除内核版本", "Delete core version"), format!("{} {} {}", self.tr("删除本地安装的", "Remove the installed"), core.display_name(), version).into())),
            Command::DeleteNodes(ids) => Some((self.tr("删除节点", "Delete nodes"), format!("{} {} {}", self.tr("将删除", "Delete"), ids.len(), self.tr("个节点，并从出站组中移除。", "nodes and remove them from outbound groups.")).into())),
            Command::DeleteNode(_) => Some((self.tr("删除节点", "Delete node"), self.tr("删除后将从所有出站组中移除此节点。", "The node will be removed from every outbound group."))),
            Command::DeleteRule(_) => Some((self.tr("删除规则", "Delete rule"), self.tr("删除后该规则将不再参与匹配，应用配置后生效。", "This rule will no longer match after applying the configuration."))),
            Command::DeleteGroup(_) => Some((self.tr("解散分组", "Ungroup rules"), self.tr("规则将移至未分组列表末尾，并保留有效启用状态。", "Rules will move to the end of the ungrouped list and retain their effective enabled state."))),
            Command::DeleteSubscription(id) => {
                let count = self.snapshot.subscriptions.iter().find(|s| &s.id == id).map(|s| s.node_count).unwrap_or(0);
                Some((self.tr("删除订阅", "Delete subscription"), format!("{} {count} {}", self.tr("同时删除", "Also delete"), self.tr("个订阅节点。", "subscription nodes.")).into()))
            }
            Command::DeleteDns(_) => Some((self.tr("删除服务器", "Delete server"), self.tr("删除此 DNS 服务器，应用配置后生效。", "Remove this DNS server after applying the configuration."))),
            Command::DeleteHost(_) => Some((self.tr("删除静态解析", "Delete host mapping"), self.tr("删除此域名的静态解析记录，应用配置后生效。", "Remove this static host mapping after applying the configuration."))),
            Command::CloseConnections => Some((self.tr("全部断开", "Close all connections"), self.tr("所有活动连接将立即断开，正在传输的数据可能中断。", "All active connections will close immediately; ongoing transfers may be interrupted."))),
            Command::ClearRecording => Some((self.tr("清空录制", "Clear recording"), self.tr("删除本次会话的连接与测试记录，此操作无法撤销。", "Delete this session's connection and test records. This cannot be undone."))),
            Command::ClearLogs => Some((self.tr("清空日志", "Clear logs"), self.tr("清空日志记录，此操作无法撤销。", "Clear the log records. This cannot be undone."))),
            _ => None,
        };
        if !confirmed {
            if let Some((title, description)) = confirmation {
                let view = cx.entity().downgrade();
                let cancel = self.tr("取消", "Cancel");
                let panel = super::theme::palette(cx).panel;
                window.open_alert_dialog(cx, move |dialog, _, _| {
                    let view = view.clone();
                    let command = command.clone();
                    dialog
                        .confirm()
                        .centered()
                        .w(px(super::theme::DIALOG_WIDTH))
                        .bg(panel)
                        .title(title.clone())
                        .description(description.clone())
                        .ok_text(title.clone())
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(cancel.clone())
                        .on_ok(move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.run_command(command.clone(), true, window, cx)
                            });
                            true
                        })
                });
                return;
            }
        }
        match command {
            Command::SetFrontProxy(id) => {
                if let Err(err) = self.service.set_front_proxy(id) {
                    self.service.toast(ToastLevel::Error, err.to_string());
                }
            }
            Command::SelectNode(group_id, node_id) => {
                self.service.select_node(&group_id, &node_id);
                self.refresh_header_location();
            }
            Command::DeleteMesh(id) => {
                if let Err(error) = self.service.delete_mesh_profile(&id) {
                    self.service.toast(ToastLevel::Error, error.to_string());
                }
            }
            Command::DeleteVersion(core, version) => self.service.remove_binary(core, &version),
            Command::EditNode(id) => {
                self.open_node_dialog(self.snapshot.model.node(&id).cloned(), true, window, cx)
            }
            Command::TestNode(id) => {
                if self.snapshot.model.node(&id).is_some_and(|n| n.enabled) {
                    self.latency_pending.clear();
                    self.latency_pending.insert(id.clone());
                    self.service.test_real_latency(&id);
                }
            }
            Command::DownloadNode(id) => {
                if self.snapshot.model.node(&id).is_some_and(|n| n.enabled) {
                    self.service.test_download(&id);
                }
            }
            Command::CopyNode(id) => {
                let link = self.service.export_links(&[id]);
                if !link.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(link));
                    self.service.toast(
                        ToastLevel::Success,
                        self.tr("分享链接已复制到剪贴板", "Share link copied to clipboard")
                            .to_string(),
                    );
                } else {
                    self.service.toast(
                        ToastLevel::Warning,
                        self.tr(
                            "该协议暂不支持导出链接",
                            "Link export is unavailable for this protocol",
                        )
                        .to_string(),
                    );
                }
            }
            Command::QrNode(id) => self.open_node_qr(&id, window, cx),
            Command::DeleteNode(id) => self.service.delete_node(&id),
            Command::DeleteNodes(ids) => {
                self.service.delete_nodes(&ids);
                self.batch_selected.clear();
            }
            Command::BatchNodes => {
                self.batch_mode = !self.batch_mode;
                if !self.batch_mode {
                    self.batch_selected.clear();
                }
            }
            Command::CleanNodes => {}
            Command::MoveRule(id, group, offset) => {
                self.service.move_rule(&id, group.as_deref(), offset)
            }
            Command::AssignRule(id, group) => self.service.assign_rule_group(&id, group.as_deref()),
            Command::DeleteRule(id) => self.service.delete_rule(&id),
            Command::RenameGroup(id) => {
                if let Some(group) = self
                    .snapshot
                    .model
                    .routing
                    .rule_groups
                    .iter()
                    .find(|g| g.id == id)
                {
                    self.rule_group_id = Some(id);
                    let name = group.name.clone();
                    self.rule_group_name.update(cx, |input, cx| {
                        input.set_value(name, window, cx);
                    });
                    self.rule_group_name.focus_handle(cx).focus(window, cx);
                }
            }
            Command::EnableGroup(id) => {
                if let Some(group) = self
                    .snapshot
                    .model
                    .routing
                    .rule_groups
                    .iter()
                    .find(|g| g.id == id)
                {
                    self.service.set_rule_group_enabled(&id, !group.enabled);
                }
            }
            Command::MoveGroup(id, offset) => self.service.move_rule_group(&id, offset),
            Command::DeleteGroup(id) => {
                self.service.delete_rule_group(&id);
                if self.rule_group_id.as_deref() == Some(&id) {
                    self.rule_group_id = None;
                }
            }
            Command::RefreshSubscription(id) => self.service.refresh_subscription(&id),
            Command::DeleteSubscription(id) => {
                if let Err(e) = self.service.delete_subscription(&id) {
                    self.service.toast(ToastLevel::Error, e.to_string());
                }
            }
            Command::DeleteDns(id) => {
                if let Err(e) = self.service.delete_dns_server(&id) {
                    self.service.toast(ToastLevel::Error, e.to_string());
                }
            }
            Command::DeleteHost(id) => {
                let mut hosts = self.snapshot.model.dns.hosts.clone();
                hosts.remove(&id);
                self.service.set_hosts(hosts);
            }
            Command::RefreshConnections => self.service.refresh_connections(),
            Command::CloseConnections => {
                if self.snapshot.process.phase.is_alive()
                    && self.snapshot.settings.core.capabilities().clash_connections
                {
                    self.service.kill_all_connections();
                }
            }
            Command::ClearRecording => self.service.clear_recording(),
            Command::ClearLogs => self.service.clear_logs(),
        }
        self.sync(cx);
    }
}
