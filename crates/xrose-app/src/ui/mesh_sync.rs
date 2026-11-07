//! Mesh peer capabilities, selective sharing, and review before importing.
use super::*;
use gpui_kit::component::WindowExt;
use gpui_kit::component::input::Input;
use std::collections::BTreeSet;
use xrose_core::mesh::{
    MeshPeer, MeshProvider,
    sync::data::{Category, Difference},
};

pub(super) struct SyncUi {
    send_keys: BTreeSet<String>,
    send_page: usize,
    cats: BTreeSet<Category>,
    receive: Option<(String, String, String)>,
    code: Entity<InputState>,
    review: String,
    merge_requested: bool,
    selected: BTreeSet<String>,
    page: usize,
}
impl SyncUi {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        Self {
            send_keys: Default::default(),
            send_page: 0,
            cats: BTreeSet::new(),
            receive: None,
            code: cx.new(|cx| InputState::new(window, cx).masked(true)),
            review: String::new(),
            merge_requested: false,
            selected: BTreeSet::new(),
            page: 0,
        }
    }
    fn reset_share_selection(&mut self) {
        self.cats.clear();
        self.send_page = 0;
        self.send_keys.clear();
    }
    pub fn open_receive(
        &mut self,
        profile: String,
        peer: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<AppView>,
    ) {
        self.receive = Some((profile, peer, name));
        self.review.clear();
        self.selected.clear();
        self.page = 0;
        self.merge_requested = false;
        self.code
            .update(cx, |input, cx| input.set_value("", window, cx));
    }
}
fn label(lang: Language, c: Category) -> SharedString {
    match c {
        Category::Nodes => tr(lang, "节点", "Nodes"),
        Category::Subscriptions => tr(lang, "节点订阅", "Subscriptions"),
        Category::Networks => tr(lang, "组网配置", "Mesh profiles"),
        Category::Rules => tr(lang, "自定义规则", "Custom rules"),
    }
}
pub(super) fn badges(peer: &MeshPeer, provider: MeshProvider, lang: Language, p: &Palette) -> Div {
    let mut row = div().flex().flex_wrap().items_center().gap_1();
    let version = peer.client_version.trim();
    let client = if version.is_empty() {
        format!(
            "{} · {}",
            provider.label(),
            tr(lang, "版本未知", "Version unknown")
        )
    } else {
        format!("{} {}", provider.label(), version)
    };
    row = row.child(pill(client, p.muted_fg, p.panel_alt));
    let mut os = peer.os.as_str();
    if let Some(x) = &peer.xrose {
        row = row.child(pill(format!("Xrose {}", x.version), p.primary, p.panel_alt));
        if os.is_empty() {
            os = &x.os;
        }
    }
    if !os.is_empty() {
        let display = match os {
            "macos" | "darwin" => "macOS",
            "ios" => "iOS",
            "windows" => "Windows",
            "linux" => "Linux",
            "android" => "Android",
            other => other,
        };
        row = row.child(pill(display.to_string(), p.muted_fg, p.panel_alt));
    }
    if peer.offers_exit {
        row = row.child(pill(
            tr(lang, "出口节点", "Exit node"),
            p.info,
            p.info.opacity(0.1),
        ));
    }
    if !peer.subnets.is_empty() || peer.xrose.as_ref().is_some_and(|x| !x.subnets.is_empty()) {
        row = row.child(pill(
            tr(lang, "子网路由", "Subnets"),
            p.info,
            p.info.opacity(0.1),
        ));
    }
    if provider == MeshProvider::Tailscale && peer.tailscale_ssh {
        row = row.child(pill("Tailscale SSH", p.info, p.info.opacity(0.1)));
    }
    row
}
impl AppView {
    pub(super) fn open_mesh_receive_dialog(
        &mut self,
        profile: String,
        peer: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(review) = &self.snapshot.mesh_sync.review {
            self.service.cancel_mesh_review(&review.id);
        }
        self.mesh_sync_ui
            .open_receive(profile.clone(), peer.clone(), name, window, cx);
        self.sync(cx);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let busy = this.snapshot.busy.iter().any(|key| key == "mesh-sync");
                let review = this
                    .snapshot
                    .mesh_sync
                    .review
                    .as_ref()
                    .filter(|review| review.profile == profile && review.peer == peer);
                let body = if review.is_some() {
                    this.sync_review(&p, cx).unwrap()
                } else if this.mesh_sync_ui.merge_requested && !busy {
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(this.tr("已合并所选配置", "Selected configuration merged"))
                        .child(
                            Button::new("mesh-receive-done")
                                .compact()
                                .primary()
                                .label(this.tr("完成", "Done"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                } else {
                    this.receive_controls(&profile, &p, cx).unwrap_or_else(div)
                };
                let close_view = cx.entity().downgrade();
                let close_profile = profile.clone();
                let close_peer = peer.clone();
                super::dialogs::settings_dialog(
                    dialog,
                    this.tr("接收配置", "Receive configuration"),
                    body,
                    &p,
                    window,
                )
                .overlay_closable(false)
                .on_cancel(move |_, _, _| !busy)
                .on_close(move |_, window, cx| {
                    let _ = close_view.update(cx, |this, cx| {
                        if let Some(review) =
                            this.service.snapshot().mesh_sync.review.filter(|review| {
                                review.profile == close_profile && review.peer == close_peer
                            })
                        {
                            this.service.cancel_mesh_review(&review.id);
                        }
                        this.mesh_sync_ui.receive = None;
                        this.mesh_sync_ui.review.clear();
                        this.mesh_sync_ui.selected.clear();
                        this.mesh_sync_ui.merge_requested = false;
                        this.mesh_sync_ui
                            .code
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.sync(cx);
                    });
                })
            })
        });
    }

    pub(super) fn open_mesh_share_dialog(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .snapshot
            .mesh
            .networks
            .iter()
            .any(|n| n.profile_id == id && n.phase == xrose_core::mesh::MeshPhase::Connected)
        {
            return;
        }
        self.mesh_sync_ui.receive = None;
        self.mesh_sync_ui.reset_share_selection();
        if self.snapshot.mesh_sync.sharing_profile.as_ref() != Some(&id) {
            self.service.prepare_mesh_share(id.clone());
        }
        self.sync(cx);
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let Some(view) = view.upgrade() else {
                return dialog;
            };
            view.update(cx, |this, cx| {
                let p = palette(cx);
                let busy = this.snapshot.busy.iter().any(|s| s == "mesh-share");
                let connected = this.snapshot.mesh.networks.iter().any(|n| {
                    n.profile_id == id && n.phase == xrose_core::mesh::MeshPhase::Connected
                });
                let body = if connected {
                    this.share_controls(&id, cx)
                } else {
                    div().text_sm().text_color(p.muted_fg).child(this.tr(
                        "网络已断开。重新连接后可共享配置。",
                        "Reconnect this network to share configuration.",
                    ))
                };
                let service = this.service.clone();
                let profile = id.clone();
                super::dialogs::settings_dialog(
                    dialog,
                    this.tr("共享配置", "Share configuration"),
                    body,
                    &p,
                    window,
                )
                .overlay_closable(false)
                .on_cancel(move |_, _, _| !busy)
                .on_close(move |_, _, _| service.close_mesh_share(&profile))
            })
        });
    }

    fn share_controls(&mut self, id: &str, cx: &mut Context<Self>) -> Div {
        let lang = self.lang();
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|s| s == "mesh-sync" || s == "mesh-share");
        let sharing = self.snapshot.mesh_sync.sharing_profile.as_deref() == Some(id);
        let mut panel = div().flex().flex_col().gap_2();
        let mut actions = div().flex().items_center().flex_wrap().gap_2();
        if sharing {
            let profile = id.to_string();
            let stop = profile.clone();
            let message = tr(
                lang,
                "已共享所选配置。将共享码交给接收设备，十分钟内有效。",
                "Selected configuration is shared. Give the code to the receiving device within ten minutes.",
            );
            panel = panel.child(div().text_sm().child(message));
            actions = actions
                .child(
                    Button::new("mesh-copy-code")
                        .compact()
                        .label(tr(lang, "复制共享码", "Copy sharing code"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(code) = this.service.mesh_share_code(&profile) {
                                cx.write_to_clipboard(ClipboardItem::new_string(code));
                                this.service.toast(
                                    xrose_core::service::ToastLevel::Success,
                                    this.tr("已复制共享码", "Sharing code copied"),
                                );
                            }
                        })),
                )
                .child(
                    Button::new("mesh-revoke")
                        .compact()
                        .label(tr(lang, "停止共享", "Stop sharing"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.service.stop_mesh_share(&stop);
                            this.sync(cx);
                            window.close_dialog(cx);
                        })),
                );
        } else {
            let Some(catalog) = self
                .snapshot
                .mesh_sync
                .catalog
                .as_ref()
                .filter(|v| v.profile == id)
                .cloned()
            else {
                if busy {
                    return panel.child(div().text_sm().child(tr(
                        lang,
                        "正在读取共享项目…",
                        "Loading items to share…",
                    )));
                }
                let profile = id.to_string();
                return panel.child(
                    Button::new("mesh-reload-share")
                        .compact()
                        .label(tr(lang, "重新读取共享项目", "Reload items to share"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mesh_sync_ui.reset_share_selection();
                            this.service.prepare_mesh_share(profile.clone());
                            cx.notify();
                        })),
                );
            };
            let mut categories = div().flex().flex_wrap().gap_3();
            for c in Category::ALL {
                let available = catalog.rows.iter().filter(|r| r.category == c).count();
                let list = catalog.clone();
                categories = categories.child(
                    toggle(
                        SharedString::from(format!("share-{c:?}")),
                        format!("{} ({available})", label(lang, c)),
                        self.mesh_sync_ui.cats.contains(&c),
                    )
                    .disabled(available == 0)
                    .on_change(cx.listener(move |this, on, _, cx| {
                        let ui = &mut this.mesh_sync_ui;
                        ui.send_page = 0;
                        if *on {
                            ui.cats.insert(c);
                        } else {
                            ui.cats.remove(&c);
                        }
                        for row in list.rows.iter().filter(|r| r.category == c) {
                            if *on {
                                ui.send_keys.insert(row.key.clone());
                            } else {
                                ui.send_keys.remove(&row.key);
                            }
                        }
                        cx.notify();
                    })),
                );
            }
            panel=panel.child(div().text_sm().child(tr(lang,"选择共享类别与项目。所选项目中的节点凭据、订阅地址及组网密钥将一并发送。",
                "Select categories and items to share. Selected items include node credentials, subscription URLs and mesh network keys."))).child(categories);
            const PAGE: usize = 12;
            let visible: Vec<_> = catalog
                .rows
                .iter()
                .filter(|r| self.mesh_sync_ui.cats.contains(&r.category))
                .collect();
            let pages = visible.len().div_ceil(PAGE).max(1);
            for row in visible
                .iter()
                .skip(self.mesh_sync_ui.send_page * PAGE)
                .take(PAGE)
            {
                let key = row.key.clone();
                panel = panel.child(
                    toggle(
                        SharedString::from(format!("share-item-{key}")),
                        format!("{} · {}", label(lang, row.category), row.name),
                        self.mesh_sync_ui.send_keys.contains(&key),
                    )
                    .on_change(cx.listener(move |this, on, _, cx| {
                        if *on {
                            this.mesh_sync_ui.send_keys.insert(key.clone());
                        } else {
                            this.mesh_sync_ui.send_keys.remove(&key);
                        }
                        cx.notify();
                    })),
                );
            }
            if pages > 1 {
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("send-prev")
                                .compact()
                                .label(tr(lang, "上一页", "Previous"))
                                .disabled(self.mesh_sync_ui.send_page == 0)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.mesh_sync_ui.send_page =
                                        this.mesh_sync_ui.send_page.saturating_sub(1);
                                    cx.notify();
                                })),
                        )
                        .child(format!("{}/{}", self.mesh_sync_ui.send_page + 1, pages))
                        .child(
                            Button::new("send-next")
                                .compact()
                                .label(tr(lang, "下一页", "Next"))
                                .disabled(self.mesh_sync_ui.send_page + 1 >= pages)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mesh_sync_ui.send_page =
                                        (this.mesh_sync_ui.send_page + 1).min(pages - 1);
                                    cx.notify();
                                })),
                        ),
                );
            }
            let profile = id.to_string();
            let close = profile.clone();
            let count = self.mesh_sync_ui.send_keys.len();
            actions = actions
                .child(
                    Button::new("mesh-share")
                        .compact()
                        .primary()
                        .label(format!(
                            "{} ({count})",
                            tr(lang, "生成共享码", "Create sharing code")
                        ))
                        .disabled(busy || count == 0)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.service.offer_mesh_data(
                                profile.clone(),
                                catalog.id.clone(),
                                this.mesh_sync_ui.send_keys.clone(),
                            );
                            this.sync(cx);
                        })),
                )
                .child(
                    Button::new("mesh-clear-share")
                        .compact()
                        .disabled(count == 0)
                        .label(tr(lang, "清空所选", "Clear selection"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mesh_sync_ui.send_keys.clear();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("mesh-close-share")
                        .compact()
                        .disabled(busy)
                        .label(tr(lang, "取消", "Cancel"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.service.close_mesh_share(&close);
                            window.close_dialog(cx);
                        })),
                );
        }
        panel.child(actions)
    }

    fn receive_controls(&mut self, id: &str, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let lang = self.lang();
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|s| s == "mesh-sync" || s == "mesh-share");
        let (profile, peer, name) = self.mesh_sync_ui.receive.clone().filter(|v| v.0 == id)?;
        let available = self
            .snapshot
            .mesh
            .networks
            .iter()
            .filter(|network| {
                network.profile_id == profile
                    && network.phase == xrose_core::mesh::MeshPhase::Connected
            })
            .flat_map(|network| &network.peers)
            .any(|candidate| candidate.id == peer && candidate.online && candidate.xrose.is_some());
        let empty = self.mesh_sync_ui.code.read(cx).value().trim().is_empty();
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().text_color(p.muted_fg).child(format!(
                    "{} · {}",
                    tr(
                        lang,
                        "接收配置，请输入对端共享码",
                        "Receive configuration using the sender’s sharing code"
                    ),
                    name
                )))
                .child(Input::new(&self.mesh_sync_ui.code))
                .when(!available, |d| {
                    d.child(div().text_sm().text_color(p.warn_text).child(tr(
                        lang,
                        "设备当前不可用，请确认网络已连接且对端在线。",
                        "The device is unavailable. Check the network and the sender’s connection.",
                    )))
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("mesh-fetch")
                                .compact()
                                .primary()
                                .disabled(busy || empty || !available)
                                .loading(busy)
                                .label(if busy {
                                    tr(lang, "正在接收…", "Receiving…")
                                } else {
                                    tr(lang, "接收并预览", "Receive and preview")
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let code = this.mesh_sync_ui.code.read(cx).value().to_string();
                                    this.service.receive_mesh_data(
                                        profile.clone(),
                                        peer.clone(),
                                        code,
                                    );
                                    this.mesh_sync_ui
                                        .code
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    this.sync(cx);
                                })),
                        )
                        .child(
                            Button::new("mesh-close-receive")
                                .compact()
                                .disabled(busy)
                                .label(tr(lang, "关闭", "Close"))
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                ),
        )
    }
    fn sync_review(&mut self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        const PAGE: usize = 20;
        let review = self.snapshot.mesh_sync.review.as_ref()?;
        let busy = self.snapshot.busy.iter().any(|k| k == "mesh-sync");
        let lang = self.lang();
        let device = self
            .snapshot
            .mesh
            .networks
            .iter()
            .find(|n| n.profile_id == review.profile)
            .and_then(|n| n.peers.iter().find(|p| p.id == review.peer))
            .map(|p| p.name.as_str())
            .filter(|s| !s.is_empty());
        let title = match device {
            Some(name) => format!("{} · {}", tr(lang, "合并预览", "Merge preview"), name),
            None => tr(lang, "合并预览", "Merge preview").to_string(),
        };
        let ui = &mut self.mesh_sync_ui;
        if ui.review != review.id {
            ui.review = review.id.clone();
            ui.selected = review
                .rows
                .iter()
                .filter(|r| r.difference == Difference::New)
                .map(|r| r.key.clone())
                .collect();
            ui.page = 0;
        }
        let mut card = div().flex().flex_col().gap_3().child(section_title(
            p,
            title,
            tr(
                lang,
                "新增项目已选中。勾选冲突项将采用对端配置。",
                "New items are selected. Selecting a conflict uses the sender’s configuration.",
            ),
        ));
        let pages = review.rows.len().div_ceil(PAGE).max(1);
        for row in review.rows.iter().skip(ui.page * PAGE).take(PAGE) {
            let key = row.key.clone();
            let diff = match row.difference {
                Difference::New => tr(lang, "新增", "New"),
                Difference::Conflict => tr(lang, "冲突", "Conflict"),
                Difference::Identical => tr(lang, "相同", "Identical"),
            };
            card = card.child(
                toggle(
                    SharedString::from(format!("sync-choice-{key}")),
                    format!("{} · {} · {}", label(lang, row.category), row.name, diff),
                    ui.selected.contains(&key),
                )
                .disabled(busy || row.difference == Difference::Identical)
                .on_change(cx.listener(move |this, on, _, cx| {
                    if *on {
                        this.mesh_sync_ui.selected.insert(key.clone());
                    } else {
                        this.mesh_sync_ui.selected.remove(&key);
                    }
                    cx.notify();
                })),
            );
        }
        let id = review.id.clone();
        let mut bar = div().flex().items_center().gap_2().flex_wrap();
        if pages > 1 {
            bar = bar
                .child(
                    Button::new("sync-prev")
                        .compact()
                        .disabled(busy || ui.page == 0)
                        .label(tr(lang, "上一页", "Previous"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mesh_sync_ui.page = this.mesh_sync_ui.page.saturating_sub(1);
                            cx.notify();
                        })),
                )
                .child(format!("{}/{}", ui.page + 1, pages))
                .child(
                    Button::new("sync-next")
                        .compact()
                        .disabled(busy || ui.page + 1 >= pages)
                        .label(tr(lang, "下一页", "Next"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mesh_sync_ui.page = (this.mesh_sync_ui.page + 1).min(pages - 1);
                            cx.notify();
                        })),
                );
        }
        Some(
            card.child(
                bar.child(
                    Button::new("mesh-commit-sync")
                        .primary()
                        .compact()
                        .disabled(busy || ui.selected.is_empty())
                        .loading(busy)
                        .label(format!(
                            "{} ({})",
                            tr(lang, "合并所选项目", "Merge selected"),
                            ui.selected.len()
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mesh_sync_ui.merge_requested = true;
                            this.service
                                .merge_mesh_data(id.clone(), this.mesh_sync_ui.selected.clone());
                            this.sync(cx);
                        })),
                )
                .child(
                    Button::new("mesh-cancel-sync")
                        .compact()
                        .disabled(busy)
                        .label(tr(lang, "取消", "Cancel"))
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            ),
        )
    }
}
