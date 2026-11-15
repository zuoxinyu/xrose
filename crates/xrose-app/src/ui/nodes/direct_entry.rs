//! Fixed direct exit rendered by the common node row.
use super::*;
use xrose_core::model::{DIRECT_VIRTUAL_NODE_ID, Protocol, WorkMode};

impl AppView {
    pub(super) fn direct_entry(
        &self,
        p: &Palette,
        busy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let mut node = Node::new(
            self.tr("本机直连", "Direct").to_string(),
            Protocol::Socks,
            "",
            0,
        );
        node.id = DIRECT_VIRTUAL_NODE_ID.into();
        let detail = if self.snapshot.settings.effective_tun() {
            self.tr(
                "全局进程监控 · 拦截规则与 DNS 生效",
                "System-wide capture · Firewall rules and DNS active",
            )
        } else {
            self.tr(
                "本地代理入站 · 拦截规则与 DNS 生效",
                "Local proxy traffic · Firewall rules and DNS active",
            )
        };
        render_virtual_node_row(
            &node,
            VirtualExit::Direct {
                detail: detail.to_string(),
            },
            self.snapshot.model.work_mode == WorkMode::Direct,
            busy,
            self,
            p,
            window,
            cx,
        )
    }
}
