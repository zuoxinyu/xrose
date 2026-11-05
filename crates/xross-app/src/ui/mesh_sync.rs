//! Mesh peer capabilities, selective sharing, and review before importing.
use super::*;
use gpui_kit::component::input::Input;
use std::collections::BTreeSet;
use xross_core::mesh::{
    MeshPeer, MeshProvider,
    sync::data::{Category, Difference},
};

pub(super) struct SyncUi {
    share: Option<String>,
    send_keys: BTreeSet<String>,
    send_page: usize,
    cats: BTreeSet<Category>,
    receive: Option<(String, String, String)>,
    code: Entity<InputState>,
    review: String,
    selected: BTreeSet<String>,
    page: usize,
}
impl SyncUi {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> Self {
        Self {
            share: None,
            send_keys: Default::default(),
            send_page: 0,
            cats: BTreeSet::new(),
            receive: None,
            code: cx.new(|cx| InputState::new(window, cx).masked(true)),
            review: String::new(),
            selected: BTreeSet::new(),
            page: 0,
        }
    }
    pub fn open_share(&mut self, id: String) {
        self.receive = None;
        self.share = Some(id);
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
        self.share = None;
        self.receive = Some((profile, peer, name));
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
    if let Some(x) = &peer.xross {
        row = row.child(pill(format!("Xross {}", x.version), p.primary, p.panel_alt));
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
    if !peer.subnets.is_empty() || peer.xross.as_ref().is_some_and(|x| !x.subnets.is_empty()) {
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
    pub(super) fn sync_controls(
        &mut self,
        id: &str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let lang = self.lang();
        let busy = self
            .snapshot
            .busy
            .iter()
            .any(|s| s == "mesh-sync" || s == "mesh-share");
        let sharing = self.snapshot.mesh_sync.sharing_profile.as_deref() == Some(id);
        let mut panel = div().flex().flex_col().gap_2().pt_2();
        if self.mesh_sync_ui.share.as_deref() == Some(id) || sharing {
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
                                        xross_core::service::ToastLevel::Success,
                                        this.tr("已复制共享码", "Sharing code copied"),
                                    );
                                }
                            })),
                    )
                    .child(
                        Button::new("mesh-revoke")
                            .compact()
                            .label(tr(lang, "停止共享", "Stop sharing"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.stop_mesh_share(&stop);
                                this.mesh_sync_ui.share = None;
                                cx.notify();
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
                        return Some(panel.child(div().text_sm().child(tr(
                            lang,
                            "正在读取共享项目…",
                            "Loading items to share…",
                        ))));
                    }
                    let profile = id.to_string();
                    return Some(
                        panel.child(
                            Button::new("mesh-reload-share")
                                .compact()
                                .label(tr(lang, "重新读取共享项目", "Reload items to share"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mesh_sync_ui.open_share(profile.clone());
                                    this.service.prepare_mesh_share(profile.clone());
                                    cx.notify();
                                })),
                        ),
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
                            .on_click(cx.listener(move |this, _, _, _| {
                                this.service.offer_mesh_data(
                                    profile.clone(),
                                    catalog.id.clone(),
                                    this.mesh_sync_ui.send_keys.clone(),
                                );
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.service.close_mesh_share(&close);
                                this.mesh_sync_ui.share = None;
                                cx.notify();
                            })),
                    );
            }
            panel = panel.child(actions);
        } else if !self
            .mesh_sync_ui
            .receive
            .as_ref()
            .is_some_and(|v| v.0 == id)
        {
            return None;
        }
        if let Some((profile, peer, name)) = self.mesh_sync_ui.receive.clone().filter(|v| v.0 == id)
        {
            panel = panel
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
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            Button::new("mesh-fetch")
                                .compact()
                                .primary()
                                .disabled(busy)
                                .label(tr(lang, "接收并预览", "Receive and preview"))
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
                                })),
                        )
                        .child(
                            Button::new("mesh-close-receive")
                                .compact()
                                .label(tr(lang, "关闭", "Close"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.mesh_sync_ui.receive = None;
                                    cx.notify();
                                })),
                        ),
                );
        }
        Some(panel)
    }
    pub(super) fn sync_review(&mut self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        const PAGE: usize = 20;
        let review = self.snapshot.mesh_sync.review.as_ref()?;
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
            ui.receive = None;
        }
        let mut card = card(p).child(section_title(
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
                .disabled(row.difference == Difference::Identical)
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
        let cancel = id.clone();
        let mut bar = div().flex().items_center().gap_2().flex_wrap();
        if pages > 1 {
            bar = bar
                .child(
                    Button::new("sync-prev")
                        .compact()
                        .disabled(ui.page == 0)
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
                        .disabled(ui.page + 1 >= pages)
                        .label(tr(lang, "下一页", "Next"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mesh_sync_ui.page = (this.mesh_sync_ui.page + 1).min(pages - 1);
                            cx.notify();
                        })),
                );
        }
        let busy = self.snapshot.busy.iter().any(|k| k == "mesh-sync");
        Some(
            card.child(
                bar.child(
                    Button::new("mesh-commit-sync")
                        .primary()
                        .compact()
                        .disabled(busy || ui.selected.is_empty())
                        .label(format!(
                            "{} ({})",
                            tr(lang, "合并所选项目", "Merge selected"),
                            ui.selected.len()
                        ))
                        .on_click(cx.listener(move |this, _, _, _| {
                            this.service
                                .merge_mesh_data(id.clone(), this.mesh_sync_ui.selected.clone())
                        })),
                )
                .child(
                    Button::new("mesh-cancel-sync")
                        .compact()
                        .disabled(busy)
                        .label(tr(lang, "取消", "Cancel"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.service.cancel_mesh_review(&cancel);
                            cx.notify();
                        })),
                ),
            ),
        )
    }
}
