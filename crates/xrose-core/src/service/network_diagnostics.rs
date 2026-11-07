//! User-triggered host and resolver diagnostics.
use super::*;

impl AppService {
    /// Blocking entry point for the CLI and the service's diagnostics worker.
    pub fn collect_network_report(&self) -> system::net_diag::NetworkReport {
        let helper = privilege::status();
        *self.inner.helper.write() = helper.clone();
        // Keep potentially slow OS commands outside the proxy mutex read by snapshots.
        let manager = ProxyManager::new(self.paths().proxy_snapshot());
        system::net_diag::collect(
            &self.model(),
            &self.settings(),
            &self.inner.mesh.snapshot(),
            &manager,
            &helper,
        )
    }

    /// Apply the current privacy setting at copy time, even for older samples.
    pub fn network_report_text(&self) -> Option<String> {
        let settings = self.settings();
        let report = self.inner.network_report.read().clone()?;
        let mut text = report.text(settings.privacy_mode);
        if let Some(probe) = self.inner.nat_probe.read().as_ref() {
            text.push('\n');
            text.push_str(
                &probe
                    .visible(settings.privacy_mode)
                    .summary(settings.language),
            );
        }
        Some(text)
    }

    pub fn refresh_network_report(&self) {
        if !self.inner.busy.write().insert("network-report".into()) {
            return;
        }
        self.refresh_doctor();
        let service = self.clone();
        self.spawn(async move {
            let worker = service.clone();
            match tokio::task::spawn_blocking(move || worker.collect_network_report()).await {
                Ok(report) => *service.inner.network_report.write() = Some(Arc::new(report)),
                Err(_) => service.toast(ToastLevel::Error, "读取系统网络状态失败，请重新检查"),
            }
            service.set_busy("network-report", false);
        });
    }

    pub fn compare_dns(&self, input: &str) -> Result<()> {
        let host = trace::parse_target(input).map_err(Error::config)?.host;
        if host.parse::<std::net::IpAddr>().is_ok() {
            return Err(Error::config(
                "DNS 对比需要域名，请在链路测试中输入域名或网址",
            ));
        }
        if !self.inner.busy.write().insert("dns-probe".into()) {
            return Ok(());
        }
        *self.inner.dns_comparison.write() = None;
        let service = self.clone();
        self.spawn(async move {
            let settings = service.settings();
            let core = service.core();
            let running = service.is_running();
            let model = running
                .then(|| core.applied_model())
                .flatten()
                .unwrap_or_else(|| service.model());
            let result = dns::probe::compare(
                host,
                model.dns,
                core.mesh_plan().dns,
                running,
                model.work_mode == WorkMode::Direct,
                core.active_proxy("socks"),
                core.active_proxy("http"),
                settings.language,
                settings.privacy_mode,
            )
            .await;
            *service.inner.dns_comparison.write() = Some(Arc::new(result));
            service.set_busy("dns-probe", false);
        });
        Ok(())
    }
}
