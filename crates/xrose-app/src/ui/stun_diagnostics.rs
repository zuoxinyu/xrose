//! Explicit external UDP test; refreshing local diagnostics never sends a probe.
use super::*;

impl AppView {
    pub(crate) fn stun_diagnostic_card(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let p = palette(cx);
        let busy = self.snapshot.busy.iter().any(|key| key == "stun");
        card(&p)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .flex_wrap()
                    .child(section_title(
                        &p,
                        self.tr("UDP 连通性与 NAT", "UDP connectivity and NAT"),
                        if self.snapshot.model.work_mode == xrose_core::model::WorkMode::Mesh {
                            self.tr("通过组网链路探测 UDP；结果反映出口路径，不代表本机物理网络的 NAT。", "Probe UDP through the mesh; results describe the exit path, not the host's physical NAT.")
                        } else { self.tr(
                            "通过公共 STUN 服务检查本机 UDP 出口。",
                            "Check outbound UDP access using public STUN servers.",
                        ) },
                    ))
                    .child(
                        Button::new("probe-stun")
                            .compact()
                            .h(px(theme::CONTROL_HEIGHT))
                            .disabled(busy)
                            .loading(busy)
                            .label(self.tr("检测 UDP", "Test UDP"))
                            .on_click(cx.listener(|this, _, _, _| this.service.probe_nat())),
                    ),
            )
            .when_some(self.snapshot.nat_probe.as_ref(), |c, result| {
                c.child(div().text_sm().child(result.summary(self.lang())))
            })
    }
}
