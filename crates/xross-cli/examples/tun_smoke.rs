//! Manual Windows acceptance probe. Requires an isolated helper and XROSS_HOME.
#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;
    use xross_core::{
        adapters,
        core::{CoreManager, LogBus, ProcessPhase, ProcessSupervisor, SpawnSpec, WatchdogConfig},
        model::CoreKind,
        paths::AppPaths,
        settings::Settings,
    };
    let root = std::path::PathBuf::from(std::env::var("XROSS_HOME")?).canonicalize()?;
    if !root.starts_with(std::env::temp_dir().canonicalize()?)
        || !std::env::var("XROSS_HELPER_SOCKET")?.starts_with("io.xross.tun-test.")
    {
        return Err("测试必须使用临时目录和独立助手管道".into());
    }
    if xross_core::system::privilege::is_elevated() {
        return Err("客户端必须以普通权限运行".into());
    }
    let paths = AppPaths::from_root(&root);
    paths.ensure()?;
    let logs = LogBus::new(200);
    let manager = CoreManager::new(paths.clone(), logs.clone());
    let binary = manager.resolve_binary(CoreKind::SingBox, &Settings::default())?;
    let config = paths.runtime_dir.join("tun-smoke.json");
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let baseline = xross_core::mesh::platform::baseline()?;
        xross_core::mesh::coexist::check_networks(&["10.254.254.0/30".into()], &baseline.protected)?;
        let archive = xross_core::mesh::driver::install(&paths).await?;
        xross_core::mesh::driver::unpack(&archive, binary.parent().unwrap())?;
        xross_core::util::atomic_write_json(&config, &serde_json::json!({
            "log":{"level":"info"},
            "inbounds":[{"type":"tun","tag":"tun-smoke","interface_name":"xross-tun-smoke","address":["10.254.254.1/30"],"auto_route":false,"strict_route":false,"dns_mode":"disabled","stack":"system"}],
            "outbounds":[{"type":"direct","tag":"direct"}],
            "route":{"final":"direct","auto_detect_interface":true}
        }))?;
        let report = adapters::validate(CoreKind::SingBox, &binary, &config).await?;
        if !report.ok { return Err(xross_core::Error::config(report.summary())); }
        let mut spec = SpawnSpec::new(CoreKind::SingBox, binary);
        spec.args = xross_core::core::run_args(CoreKind::SingBox, &config);
        spec.config_path = Some(config);
        spec.workdir = Some(paths.data_dir.clone());
        spec.elevated_tun = true;
        let supervisor = ProcessSupervisor::new(logs.clone());
        let watchdog = WatchdogConfig { enabled:false, max_retries:0 };
        let mut proxy_addresses = Vec::new();
        let initial_mesh_baseline = xross_core::mesh::platform::worker_baseline(&mut proxy_addresses)?;
        for attempt in 0..2 {
            supervisor.start(spec.clone(), watchdog).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            let started = logs.recent(200, xross_core::settings::LogLevel::Info, "sing-box started").len();
            let state = supervisor.state();
            if state.phase != ProcessPhase::Running || started <= attempt {
                supervisor.stop().await?;
                return Err(xross_core::Error::process(state.last_exit.map(|e|e.reason).unwrap_or_else(||"TUN 未完成初始化".into())));
            }
            println!("TUN started: pid={}, iteration={}", state.pid.unwrap(), attempt + 1);
            let now = xross_core::mesh::platform::baseline()?;
            if !now.preserves_connectivity(&baseline) {
                supervisor.stop().await?;
                return Err(xross_core::Error::process("原有默认路由或 DNS 发生变化，已停止测试"));
            }
            if std::env::args().any(|a|a=="--crash-owner") { std::process::exit(0); }
            let mesh_baseline = xross_core::mesh::platform::worker_baseline(&mut proxy_addresses)?;
            if !proxy_addresses.contains(&"10.254.254.1".to_string()) || !mesh_baseline.preserves_connectivity(&initial_mesh_baseline) {
                return Err(xross_core::Error::process("未识别后启动的代理 TUN"));
            }
            supervisor.stop().await?;
            let after_stop = xross_core::mesh::platform::worker_baseline(&mut proxy_addresses)?;
            if !after_stop.preserves_connectivity(&mesh_baseline) {
                return Err(xross_core::Error::process("代理 TUN 回收被误判为原有网络变化"));
            }
            if supervisor.state().phase != ProcessPhase::Stopped { return Err(xross_core::Error::process("TUN 未停止")); }
        }
        println!("TUN helper start, logs, stop and restart passed");
        Ok::<(), xross_core::Error>(())
    })?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("此测试仅支持 Windows");
}
