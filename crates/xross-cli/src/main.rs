//! Headless companion for Xross: compile configs, inspect subscriptions and
//! validate a core without opening the GUI.

use std::path::PathBuf;

use xross_core::adapters::{self, CompileContext};
use xross_core::model::{CoreKind, Node, Protocol};
use xross_core::paths::AppPaths;
use xross_core::subscription::parse_subscription;
use xross_core::system;
use xross_core::AppService;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("compile") => compile(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("parse") => parse(&args[1..]),
        Some("import") => import(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("doctor") => doctor(&args[1..]),
        Some("seed-demo") => seed_demo(&args[1..]),
        Some("core") => core_command(&args[1..]),
        Some("fetch-core") => fetch_core(&args[1..]),
        Some("run") => run_core(&args[1..]),
        Some("trace") => trace_link(&args[1..]),
        Some("demo") => demo(),
        Some("version") | Some("--version") | Some("-V") => {
            println!("xross-cli {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            usage();
            return;
        }
    };
    if let Err(err) = result {
        eprintln!("错误：{err}");
        std::process::exit(1);
    }
}

fn usage() {
    println!(
        r#"xross-cli — headless Xross utilities

用法 / Usage:
  xross-cli compile [--core xray|sing-box] [-o config.json] [--home DIR]
  xross-cli check   [--core xray|sing-box] [--home DIR]
  xross-cli parse <file|->          解析订阅并打印摘要
  xross-cli import <file|->         导入节点到当前配置
  xross-cli export [--all]          导出分享链接
  xross-cli doctor                  输出 TUN/端口/权限检查
  xross-cli seed-demo               写入演示节点/订阅/规则（用于截图或试用）
  xross-cli core install --core xray --path <bin> --version 1.8.24
  xross-cli core use --core xray --version 1.8.24
  xross-cli fetch-core [--core sing-box]  从 GitHub 下载并校验核心
  xross-cli run [--core xray] [--seconds 5] [--home DIR]
  xross-cli trace <https://…|tcp://…|udp://…|icmp://…>   追踪测试链接
  xross-cli demo                    打印带示例节点的 sing-box + xray 配置
  xross-cli version

环境变量 XROSS_HOME 可指定配置/数据目录。"#
    );
}

fn parse_args(
    args: &[String],
) -> (
    Option<CoreKind>,
    Option<PathBuf>,
    Option<PathBuf>,
    Vec<String>,
) {
    let mut core = None;
    let mut out = None;
    let mut home = None;
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--core" | "-c" => {
                i += 1;
                core = args.get(i).and_then(|v| match v.as_str() {
                    "xray" => Some(CoreKind::Xray),
                    "sing-box" | "singbox" => Some(CoreKind::SingBox),
                    _ => None,
                });
            }
            "--out" | "-o" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            "--home" => {
                i += 1;
                home = args.get(i).map(PathBuf::from);
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    (core, out, home, positional)
}

fn service(home: Option<PathBuf>) -> Result<AppService, String> {
    let paths = match home {
        Some(home) => AppPaths::from_root(&home),
        None => AppPaths::discover(),
    };
    if std::env::var_os("XROSS_DEBUG").is_some() {
        eprintln!("[debug] opening service at {}", paths.data_dir.display());
    }
    let service = AppService::with_paths(paths).map_err(|e| e.to_string());
    if std::env::var_os("XROSS_DEBUG").is_some() {
        eprintln!("[debug] service ready");
    }
    service
}

fn compile(args: &[String]) -> Result<(), String> {
    let (core, out, home, _) = parse_args(args);
    let service = service(home)?;
    let settings = service.settings();
    let core = core.unwrap_or(settings.core);
    let model = service.model();
    let issues = model.validate(core);
    for issue in &issues {
        eprintln!("警告：{issue}");
    }
    let paths = service.paths();
    let ctx = CompileContext::new(&settings, paths.data_dir.clone(), paths.runtime_dir.clone());
    let compiled = adapters::adapter_for(core)
        .compile(&model, &settings, &ctx)
        .map_err(|e| e.to_string())?;
    for warning in &compiled.warnings {
        eprintln!("警告：{warning}");
    }
    let text = compiled.to_pretty_string().map_err(|e| e.to_string())?;
    match out {
        Some(path) => {
            xross_core::util::atomic_write(&path, text.as_bytes()).map_err(|e| e.to_string())?;
            println!("已写入 {}", path.display());
        }
        None => println!("{text}"),
    }
    Ok(())
}

fn check(args: &[String]) -> Result<(), String> {
    let (core, _, home, _) = parse_args(args);
    let service = service(home)?;
    let settings = service.settings();
    let core = core.unwrap_or(settings.core);
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let manager = service.core();
    let model = service.model();
    let outcome = runtime
        .block_on(manager.prepare(&model, &settings, core))
        .map_err(|e| e.to_string())?;
    println!("配置：{}", outcome.config_path.display());
    println!("{}", outcome.report.summary());
    for issue in &outcome.report.issues {
        match issue.line {
            Some(line) => println!("  第 {line} 行：{}", issue.message),
            None => println!("  {}", issue.message),
        }
    }
    for warning in &outcome.compiled.warnings {
        println!("  警告：{warning}");
    }
    if !outcome.report.ok {
        std::process::exit(2);
    }
    Ok(())
}

fn read_input(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("-") | None => {
            use std::io::Read;
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| e.to_string())?;
            Ok(text)
        }
        Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string()),
    }
}

fn parse(args: &[String]) -> Result<(), String> {
    let content = read_input(args)?;
    let parsed = parse_subscription(&content);
    println!(
        "格式：{}，节点：{}，分组：{}",
        parsed.format.label(),
        parsed.nodes.len(),
        parsed.groups.len()
    );
    for warning in &parsed.warnings {
        println!("  警告：{warning}");
    }
    for node in &parsed.nodes {
        println!(
            "  {:<28} {:<10} {} ({} {})",
            truncate(&node.name, 26),
            node.protocol.display_name(),
            node.endpoint(),
            node.transport.kind_name(),
            if node.tls.enabled { "TLS" } else { "plain" }
        );
    }
    Ok(())
}

fn import(args: &[String]) -> Result<(), String> {
    let (_, _, home, positional) = parse_args(args);
    let content = read_input(&positional)?;
    let service = service(home)?;
    let added = service.import_content(&content);
    println!("已导入 {added} 个节点");
    Ok(())
}

fn export(args: &[String]) -> Result<(), String> {
    let (_, _, home, _) = parse_args(args);
    let service = service(home)?;
    let model = service.model();
    let ids: Vec<String> = model.nodes.iter().map(|n| n.id.clone()).collect();
    let links = service.export_links(&ids);
    if links.is_empty() {
        eprintln!("当前配置没有可导出的节点");
    } else {
        println!("{links}");
    }
    Ok(())
}

fn doctor(args: &[String]) -> Result<(), String> {
    let (_, _, home, _) = parse_args(args);
    let service = service(home)?;
    let model = service.model();
    let settings = service.settings();
    for check in system::diagnose_tun(&model, &settings) {
        let tag = match check.level {
            system::CheckLevel::Ok => "OK",
            system::CheckLevel::Warn => "WARN",
            system::CheckLevel::Error => "ERROR",
        };
        println!("[{tag}] {}：{}", check.name, check.detail);
    }
    println!(
        "系统代理后端：{}，提权：{}，助手：{}",
        service.platform_proxy_backend(),
        system::privilege::is_elevated(),
        system::privilege::helper_available()
    );
    Ok(())
}

fn demo() -> Result<(), String> {
    use xross_core::model::AppModel;
    let mut model = AppModel::default();
    let mut node = Node::new("Demo VLESS", Protocol::Vless, "example.com", 443);
    node.credentials.uuid = Some("11111111-2222-3333-4444-555555555555".into());
    node.tls.enabled = true;
    node.tls.server_name = Some("example.com".into());
    node.name = "Demo VLESS".into();
    model.add_nodes(vec![node]);
    let settings = xross_core::settings::Settings::default();
    let ctx = CompileContext::new(
        &settings,
        PathBuf::from("/tmp/xross-demo-data"),
        PathBuf::from("/tmp/xross-demo-run"),
    );
    for core in CoreKind::all() {
        let compiled = adapters::adapter_for(core)
            .compile(&model, &settings, &ctx)
            .map_err(|e| e.to_string())?;
        println!("===== {} =====", core.display_name());
        println!(
            "{}",
            compiled.to_pretty_string().map_err(|e| e.to_string())?
        );
        for warning in &compiled.warnings {
            println!("# 警告：{warning}");
        }
    }
    Ok(())
}

fn seed_demo(args: &[String]) -> Result<(), String> {
    let (_, _, home, _) = parse_args(args);
    let service = service(home)?;
    let content = r#"
vless://11111111-2222-3333-4444-555555555555@hk.example.com:443?encryption=none&security=tls&sni=hk.example.com&type=ws&path=%2Fws#🇭🇰 香港 01
vless://11111111-2222-3333-4444-555555555555@jp.example.com:443?encryption=none&security=reality&sni=www.microsoft.com&type=grpc&serviceName=grpc&pbk=ml58sJg9puxNs502V-apSbKQGLslmU4T3W6btDPizWI&sid=abcd1234&flow=xtls-rprx-vision#🇯🇵 东京 Reality
vmess://eyJ2IjoiMiIsInBzIjoi5paw5Yqg5Z2hIDAyIiwiYWRkIjoic2cuaG9zdC5leGFtcGxlLmNvbSIsInBvcnQiOiI0NDMiLCJpZCI6IjExMTExMTExLTIyMjItMzMzMy00NDQ0LTU1NTU1NTU1NTU1NSIsImFpZCI6IjAiLCJuZXQiOiJ3cyIsImhvc3QiOiJzZy5ob3N0LmV4YW1wbGUuY29tIiwicGF0aCI6Ii93cyIsInRscyI6InRscyIsInNuaSI6InNnLmhvc3QuZXhhbXBsZS5jb20ifQ==
trojan://password123@us.example.com:443?security=tls&sni=us.example.com&type=ws&path=%2Ftj#🇺🇸 洛杉矶 Trojan
ss://YWVzLTI1Ni1nY206c2VjcmV0MTIz@tw.example.com:8388#🇹🇼 台北 SS
hysteria2://hy2pass@de.example.com:8443?sni=de.example.com&insecure=1&obfs=salamander&obfs-password=obfs#🇩🇪 法兰克福 HY2
tuic://11111111-2222-3333-4444-555555555555:tuicpass@kr.example.com:443?congestion_control=bbr&sni=kr.example.com#🇰🇷 首尔 TUIC
"#;
    let added = service.import_content(content);
    let settings = service.settings();
    let mut subscription = xross_core::subscription::Subscription::new(
        "Demo Airport",
        "https://example.com/api/v1/client/subscribe?token=demo-token",
    );
    subscription.node_count = added;
    subscription.last_updated = Some(xross_core::util::now_unix());
    subscription.etag = Some(r#"W/"demo-etag""#.into());
    subscription.usage = Some(xross_core::subscription::SubscriptionUsage {
        upload: 12 * 1024 * 1024 * 1024,
        download: 48 * 1024 * 1024 * 1024,
        total: 200 * 1024 * 1024 * 1024,
        expire: Some(xross_core::util::now_unix() + 30 * 86400),
    });
    service
        .update_subscription_settings(subscription)
        .map_err(|e| e.to_string())?;

    let mut route = xross_core::model::RouteRule::new(
        "ChatGPT 走美国",
        xross_core::model::Matchers {
            domain_suffixes: vec!["openai.com".into(), "chatgpt.com".into()],
            ..Default::default()
        },
        xross_core::model::RouteAction::proxy(),
    );
    route.name = "ChatGPT 走美国".into();
    service.upsert_rule(route);
    service.rename_profile("演示配置");
    println!(
        "已写入演示数据：{added} 个节点、1 个订阅、1 条自定义规则（{}）",
        settings.core.display_name()
    );
    Ok(())
}

fn core_command(args: &[String]) -> Result<(), String> {
    let sub = args.first().map(String::as_str);
    match sub {
        Some("install") => {
            let (core, _, home, _) = parse_args(&args[1..]);
            let core = core.unwrap_or(CoreKind::Xray);
            let path = args
                .iter()
                .position(|a| a == "--path")
                .and_then(|i| args.get(i + 1))
                .map(PathBuf::from)
                .ok_or_else(|| "缺少 --path <可执行文件>".to_string())?;
            let version = args
                .iter()
                .position(|a| a == "--version")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .unwrap_or_else(|| "custom".into());
            let service = service(home)?;
            service.adopt_binary(core, &version, &path);
            println!(
                "已登记 {} {} -> {}",
                core.display_name(),
                version,
                path.display()
            );
            Ok(())
        }
        Some("use") => {
            let (core, _, home, _) = parse_args(&args[1..]);
            let core = core.ok_or_else(|| "缺少 --core".to_string())?;
            let version = args
                .iter()
                .position(|a| a == "--version")
                .and_then(|i| args.get(i + 1))
                .cloned()
                .ok_or_else(|| "缺少 --version".to_string())?;
            let service = service(home)?;
            service.set_active_version(core, &version);
            println!("{} 将使用 {}", core.display_name(), version);
            Ok(())
        }
        _ => {
            eprintln!("用法：xross-cli core install|use ...");
            std::process::exit(2);
        }
    }
}

fn fetch_core(args: &[String]) -> Result<(), String> {
    let (core, _, home, _) = parse_args(args);
    let service = service(home)?;
    let settings = service.settings();
    let core = core.unwrap_or(settings.core);
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let manager = service.core();
    let current = manager
        .binaries()
        .resolve(core, settings.active_version(core).map(|s| s.as_str()))
        .map(|b| b.version);
    let candidate = runtime
        .block_on(xross_core::updates::check_core_update(
            core,
            current.as_deref(),
        ))
        .map_err(|e| e.to_string())?;
    let Some(candidate) = candidate else {
        println!("{} 已是最新版本", core.display_name());
        return Ok(());
    };
    println!(
        "下载 {} {}（{}）…",
        core.display_name(),
        candidate.version,
        xross_core::util::format_bytes(candidate.asset.size)
    );
    let checksum = match &candidate.checksum_url {
        Some(url) => runtime
            .block_on(xross_core::updates::download_checksum(url))
            .map_err(|e| e.to_string())?,
        None => None,
    };
    if checksum.is_none() {
        println!("警告：发布页没有提供校验文件，仅能验证下载完整性由 HTTPS 保证");
    }
    let record = runtime
        .block_on(manager.binaries().install_from_url(
            core,
            &candidate.version,
            &candidate.asset.url,
            checksum,
        ))
        .map_err(|e| e.to_string())?;
    let mut settings = service.settings();
    settings
        .active_versions
        .insert(core.as_str().into(), record.version.clone());
    settings.binary_paths.remove(core.as_str());
    settings
        .save(&service.paths().settings_file())
        .map_err(|e| e.to_string())?;
    println!(
        "安装完成：{} {}（sha256 {}…）",
        core.display_name(),
        record.version,
        &record.sha256[..12]
    );
    Ok(())
}

fn run_core(args: &[String]) -> Result<(), String> {
    let (core, _, home, _) = parse_args(args);
    let seconds = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5);
    let service = service(home)?;
    let settings = service.settings();
    let core = core.unwrap_or(settings.core);
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let manager = service.core();
    let model = service.model();
    let outcome = runtime
        .block_on(manager.start(&model, &settings, core))
        .map_err(|e| e.to_string())?;
    println!("{}", outcome.report.summary());
    if !outcome.report.ok {
        for issue in &outcome.report.issues {
            match issue.line {
                Some(line) => println!("  第 {line} 行：{}", issue.message),
                None => println!("  {}", issue.message),
            }
        }
        return Err("配置校验失败，未启动".into());
    }
    println!("core 已启动，运行 {seconds}s …");
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    let state = manager.state();
    println!(
        "状态：{:?} pid={:?} restarts={}",
        state.phase, state.pid, state.restarts
    );
    for entry in service
        .logs()
        .recent(20, xross_core::settings::LogLevel::Info, "")
    {
        println!("  {} {}", entry.timestamp, entry.message);
    }
    runtime
        .block_on(manager.stop())
        .map_err(|e| e.to_string())?;
    println!("已停止");
    Ok(())
}

fn trace_link(args: &[String]) -> Result<(), String> {
    let (_, _, home, positional) = parse_args(args);
    let target = positional
        .first()
        .cloned()
        .unwrap_or_else(|| "https://www.gstatic.com/generate_204".into());
    let service = service(home)?;
    service.start_trace(&target).map_err(|e| e.to_string())?;
    println!("开始追踪：{target}");
    let mut last_printed = 0usize;
    let mut final_session = None;
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let Some(session) = service.trace_snapshot() else {
            continue;
        };
        while last_printed < session.steps.len() {
            let step = &session.steps[last_printed];
            let status = match step.status {
                xross_core::trace::TraceStatus::Pending => "pending",
                xross_core::trace::TraceStatus::Active => "active",
                xross_core::trace::TraceStatus::Ok => "ok",
                xross_core::trace::TraceStatus::Warn => "warn",
                xross_core::trace::TraceStatus::Error => "error",
                xross_core::trace::TraceStatus::Skipped => "skip",
            };
            println!(
                "  [{status:>7}] {:<12} {} {}",
                step.title,
                step.detail,
                step.duration_ms
                    .map(|ms| format!("({ms} ms)"))
                    .unwrap_or_default()
            );
            last_printed += 1;
        }
        if matches!(
            session.phase,
            xross_core::trace::TracePhase::Done | xross_core::trace::TracePhase::Failed
        ) {
            final_session = Some(session);
            break;
        }
    }
    if let Some(session) = final_session {
        println!("{}", session.summary.unwrap_or_default());
        if session.success == Some(true) {
            Ok(())
        } else {
            Err("测试未成功".into())
        }
    } else {
        Err("追踪超时".into())
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
