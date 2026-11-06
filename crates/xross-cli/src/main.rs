//! Headless companion for Xross: compile configs, inspect subscriptions and
//! validate a core without opening the GUI.

use std::path::PathBuf;

use xross_core::AppService;
use xross_core::adapters::{self, CompileContext};
use xross_core::model::{CoreKind, Node, NodeDraft, Protocol, TransportKind};
use xross_core::paths::AppPaths;
use xross_core::subscription::parse_subscription;
use xross_core::system;
use xross_core::system::privilege;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match privilege::helper_main(&args) {
        Ok(true) => return,
        Ok(false) => {}
        Err(err) => {
            eprintln!("提权助手：{err}");
            std::process::exit(1);
        }
    }
    let result = match args.first().map(String::as_str) {
        Some("compile") => compile(&args[1..]),
        Some("check") => check(&args[1..]),
        Some("parse") => parse(&args[1..]),
        Some("route") => route_files(&args[1..]),
        Some("import") => import(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("add-node") => add_node(&args[1..]),
        Some("doctor") => doctor(&args[1..]),
        Some("seed-demo") => seed_demo(&args[1..]),
        Some("core") => core_command(&args[1..]),
        Some("fetch-core") => fetch_core(&args[1..]),
        Some("run") => run_core(&args[1..]),
        Some("helper") => helper_command(&args[1..]),
        Some("trace") => trace_link(&args[1..]),
        Some("mesh") => mesh_command(&args[1..]),
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
  xross-cli route preview|import <file|-> [--home DIR]   预览或追加路由规则与规则集
  xross-cli route convert <file|-> [-o FILE] [--home DIR]   转换为 Xross 路由文件
  xross-cli route export [--sets-only] [-o FILE] [--home DIR]
  xross-cli parse <file|->          解析订阅并打印摘要
  xross-cli import <file|->         导入节点到当前配置
  xross-cli export [--all]          导出分享链接
  xross-cli add-node --server HOST [--name N] [--protocol vless] [--port 443]
                     [--uuid U] [--password P] [--method aes-256-gcm]
                     [--transport ws|grpc|h2|httpupgrade|tcp] [--path /ws]
                     [--host H] [--service-name S] [--sni S] [--alpn h2]
                     [--reality-pbk KEY] [--reality-sid ID] [--no-tls]
                     [--insecure] [--tags a,b] [--wireguard-config FILE] [--vpn-config FILE] [--ca FILE]
                     [--client-cert FILE] [--client-key FILE] [--vpn-flavor anyconnect]
                     [--auth-group GROUP] [--vpn-path /] [--core sing-box] [--home DIR]
  xross-cli helper status           提权助手状态（JSON）
  xross-cli helper install          安装并启动提权助手（会请求管理员权限）
  xross-cli helper start|restart    启动/重启已安装的提权助手
  xross-cli helper update           使用当前版本更新并启动提权助手
  xross-cli helper uninstall        卸载提权助手
  xross-cli helper run              前台运行助手（需要 root/管理员）
  xross-cli doctor                  输出 TUN/端口/权限检查
  xross-cli seed-demo               写入演示节点/订阅/规则（用于截图或试用）
  xross-cli core install --core xray --path <bin> --version 1.8.24
  xross-cli core use --core xray --version 1.8.24
  xross-cli fetch-core [--core sing-box]  从 GitHub 下载并校验核心
  xross-cli run [--core xray] [--seconds 5] [--home DIR]
  xross-cli trace <https://…|tcp://…|udp://…|icmp://…>   追踪测试链接
  xross-cli demo                    打印带示例节点的 sing-box + xray 配置
  xross-cli version
  xross-cli mesh save <file|-> [--home DIR]    加密保存 JSON 配置；密钥只从文件/标准输入读取
  xross-cli mesh list|status|doctor [--home DIR]
  xross-cli mesh install [--home DIR]         下载并验证 Tailscale/Wintun 组件
  xross-cli mesh check [--home DIR]           离线校验独立 Tailscale 配置，不启动网卡
  xross-cli mesh connect <ID> [ID...] [--seconds N] [--home DIR]
  xross-cli mesh probe <ID> <IP> [--home DIR]
  xross-cli mesh disconnect|logout|delete <ID> [--home DIR]

环境变量 XROSS_HOME 可指定配置/数据目录。"#
    );
}

fn mesh_command(args: &[String]) -> Result<(), String> {
    use xross_core::mesh::{
        self, MeshProfile, MeshSecret,
        ipc::{MeshCommand, MeshRequest},
    };
    let (_, _, home, positional) = parse_args(args);
    let paths = home
        .as_ref()
        .map(|p| AppPaths::from_root(p))
        .unwrap_or_else(AppPaths::discover);
    paths.ensure().map_err(|e| e.to_string())?;
    let action = positional.first().map(String::as_str).unwrap_or("status");
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let request = |command| MeshRequest {
        version: mesh::PROTOCOL_VERSION,
        owner_pid: std::process::id(),
        session: MeshSecret(format!("{:032x}{:032x}", rand_token(), rand_token())),
        command,
    };
    match action {
        "status" => {
            let reply =
                mesh::ipc::call(request(MeshCommand::Inspect)).map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&reply).map_err(|e| e.to_string())?
            );
        }
        "doctor" => {
            println!(
                "EasyTier {}（静态库）；Tailscale sing-box {}",
                mesh::EASYTIER_VERSION,
                mesh::TAILSCALE_CORE_VERSION
            );
            println!("{}",serde_json::to_string_pretty(&serde_json::json!({"running":privilege::status().running,"elevated":privilege::is_elevated(),"detail":privilege::status().detail})).map_err(|e|e.to_string())?);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &mesh::platform::baseline().map_err(|e| e.to_string())?
                )
                .map_err(|e| e.to_string())?
            );
        }
        "probe" => {
            let id = positional.get(1).ok_or("请指定组网配置 ID")?.clone();
            let ip = positional.get(2).ok_or("请指定组网设备 IP")?.clone();
            mesh::ipc::call(request(MeshCommand::ProbeProfile {
                profile_id: id.clone(),
                ip: ip.clone(),
            }))
            .map_err(|e| e.to_string())?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(18);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let reply =
                    mesh::ipc::call(request(MeshCommand::Inspect)).map_err(|e| e.to_string())?;
                if let Some(probe) = reply
                    .networks
                    .iter()
                    .find(|n| n.profile_id == id)
                    .and_then(|n| n.probe.as_ref())
                    .filter(|p| p.ip == ip)
                {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(probe).map_err(|e| e.to_string())?
                    );
                    if !probe.success {
                        return Err("设备探测失败".into());
                    }
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err("设备探测超时".into());
                }
            }
        }
        "install" => {
            let settings = xross_core::Settings::load(&paths.settings_file())
                .map_err(|err| err.to_string())?;
            let network = xross_core::updates::UpdateNetwork::offline_resources(&settings);
            runtime
                .block_on(async {
                    mesh::tailscale::install(&paths, &network).await?;
                    #[cfg(windows)]
                    mesh::driver::install(&paths, &network).await?;
                    Ok::<(), xross_core::Error>(())
                })
                .map_err(|e| e.to_string())?;
            println!("组网组件已下载并通过 SHA-256 校验");
        }
        "api-check" => {
            runtime
                .block_on(mesh::tailscale::check_api(&paths))
                .map_err(|e| e.to_string())?;
            println!("Tailscale gRPC 版本、鉴权、状态订阅及登录准备验证通过（未创建网卡或 DNS 规则）");
        }
        "check" => {
            runtime
                .block_on(mesh::tailscale::check_configuration(&paths))
                .map_err(|e| e.to_string())?;
            println!("独立 Tailscale 配置校验通过（未创建网卡）");
        }
        "disconnect" | "logout" => {
            let id = positional.get(1).ok_or("请指定组网配置 ID")?.clone();
            mesh::ipc::call(request(MeshCommand::DisconnectProfile {
                profile_id: id,
                logout: action == "logout",
            }))
            .map_err(|e| e.to_string())?;
            println!("组网操作已提交");
        }
        "save" | "list" | "delete" => {
            let store = xross_core::storage::Store::open(&paths).map_err(|e| e.to_string())?;
            let controller = mesh::controller::MeshController::new(paths.clone(), store)
                .map_err(|e| e.to_string())?;
            match action {
                "save" => {
                    let source = positional.get(1).ok_or("请指定配置文件或 -（标准输入）")?;
                    let mut bytes = Vec::new();
                    if source == "-" {
                        use std::io::Read;
                        std::io::stdin()
                            .take(262_145)
                            .read_to_end(&mut bytes)
                            .map_err(|_| "读取配置失败")?;
                    } else {
                        bytes = std::fs::read(source).map_err(|_| "读取配置失败")?;
                    }
                    if bytes.len() > 262_144 {
                        return Err("组网配置过大".into());
                    }
                    let profile: MeshProfile = serde_json::from_slice(&bytes)
                        .map_err(|_| "组网 JSON 配置无效；请检查字段格式")?;
                    let service = xross_core::service::AppService::with_paths(paths.clone())
                        .map_err(|e| e.to_string())?;
                    println!(
                        "{}",
                        service
                            .save_mesh_profile(profile)
                            .map_err(|e| e.to_string())?
                    );
                }
                "list" => println!(
                    "{}",
                    serde_json::to_string_pretty(&controller.snapshot().profiles)
                        .map_err(|e| e.to_string())?
                ),
                _ => {
                    xross_core::service::AppService::with_paths(paths.clone())
                        .map_err(|e| e.to_string())?
                        .delete_mesh_profile(positional.get(1).ok_or("请指定组网配置 ID")?)
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        "connect" => {
            let svc = AppService::with_paths(paths).map_err(|e| e.to_string())?;
            let ids = positional
                .iter()
                .skip(1)
                .take_while(|v| !v.starts_with("--"))
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return Err("请指定一个或两个组网配置 ID".into());
            }
            for id in ids {
                svc.mesh_connect(id);
                wait_mesh(&svc)?;
            }
            let seconds = args
                .iter()
                .position(|v| v == "--seconds")
                .and_then(|i| args.get(i + 1))
                .and_then(|v| v.parse::<u64>().ok());
            println!(
                "组网会话运行中；Ctrl+C 或结束进程会清理网卡。可用 mesh status 查询、mesh disconnect <ID> 断开。"
            );
            let result=runtime.block_on(async {
                let deadline=tokio::time::sleep(std::time::Duration::from_secs(seconds.unwrap_or(86400*365)));
                tokio::pin!(deadline);
                let mut interval=tokio::time::interval(std::time::Duration::from_secs(2));
                let mut login_opened=std::collections::HashSet::new();
                loop {tokio::select!{
                    _=tokio::signal::ctrl_c()=>break Ok(()),
                    _=&mut deadline=>break Ok(()),
                    _=interval.tick()=>{
                        let state=svc.snapshot().mesh;
                        for network in &state.networks {
                            if let Some(url)=&network.login_url {if login_opened.insert(url.clone()){svc.open_url(url);println!("已打开浏览器，请完成 Tailscale 登录（独立设备身份）");}}
                            if network.phase==mesh::MeshPhase::Failed {break;}
                        }
                        if state.networks.iter().any(|n|n.phase==mesh::MeshPhase::Failed){break Err("组网启动或运行失败，请用 mesh status 查看诊断".to_string());}
                        if state.networks.is_empty(){break Ok(());}
                    }
                }}
            });
            svc.shutdown();
            result?;
        }
        _ => return Err("未知 mesh 操作".into()),
    }
    Ok(())
}

fn rand_token() -> u128 {
    // MeshProfile already generates an OS-random UUID without another CLI dependency.
    u128::from_str_radix(
        &xross_core::mesh::MeshProfile::default().id.replace('-', ""),
        16,
    )
    .expect("UUID")
}

fn wait_mesh(service: &AppService) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(240);
    while service.is_busy("mesh") {
        if std::time::Instant::now() > deadline {
            service.shutdown();
            return Err("组网操作超时".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if let Some(error) = service
        .snapshot()
        .toasts
        .iter()
        .rev()
        .find(|t| t.level == xross_core::service::ToastLevel::Error)
    {
        service.shutdown();
        return Err(error.message.clone());
    }
    Ok(())
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
            xross_core::util::atomic_write_private(&path, text.as_bytes())
                .map_err(|e| e.to_string())?;
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

fn route_files(args: &[String]) -> Result<(), String> {
    use xross_core::route::transfer::{self, ExportScope};
    let mut out = None;
    let mut home = None;
    let mut positional = Vec::new();
    let mut args = args.iter().map(String::as_str);
    while let Some(arg) = args.next() {
        match arg {
            "-o" | "--out" | "--home" => {
                let path = args
                    .next()
                    .filter(|p| !p.starts_with('-'))
                    .ok_or_else(|| format!("{arg} 缺少路径"))?;
                let dest = if arg == "--home" { &mut home } else { &mut out };
                if dest.replace(PathBuf::from(path)).is_some() {
                    return Err(format!("请只指定一次 {arg}"));
                }
            }
            other => positional.push(other),
        }
    }
    let emit = |text: &str| -> Result<(), String> {
        if let Some(path) = &out {
            xross_core::util::atomic_write_private(path, text.as_bytes())
                .map_err(|_| "无法导出，请选择可写文件位置".to_string())?;
        } else {
            println!("{text}");
        }
        Ok(())
    };
    let usage = "用法：route preview|import|convert <文件|-> 或 route export [--sets-only]；convert / export 可用 -o 指定输出文件";
    match positional.as_slice() {
        [cmd @ ("preview" | "import" | "convert"), file] => {
            if *cmd != "convert" && out.is_some() {
                return Err(usage.into());
            }
            let text = if *file == "-" {
                transfer::read_text(std::io::stdin().lock())
            } else {
                transfer::read_file(std::path::Path::new(file))
            }
            .map_err(|e| e.to_string())?;
            let service = service(home)?;
            if *cmd == "convert" {
                let (bundle, summary) =
                    transfer::convert(&text, &service.model()).map_err(|e| e.to_string())?;
                eprintln!(
                    "{}",
                    serde_json::to_string(&summary).map_err(|e| e.to_string())?
                );
                return emit(&bundle);
            }
            let (id, summary) = service
                .preview_route_import(&text)
                .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?
            );
            if *cmd == "import" {
                service
                    .commit_route_import(&id)
                    .map_err(|e| e.to_string())?;
            }
        }
        ["export"] | ["export", "--sets-only"] => {
            let scope = if positional.len() == 1 {
                ExportScope::All
            } else {
                ExportScope::RuleSets
            };
            emit(
                &service(home)?
                    .export_route_text(&scope)
                    .map_err(|e| e.to_string())?,
            )?;
        }
        _ => return Err(usage.into()),
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

fn value_arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn add_node(args: &[String]) -> Result<(), String> {
    let (core, _, home, _) = parse_args(args);
    let mut draft = NodeDraft::default();
    if let Some(name) = value_arg(args, "--name") {
        draft.name = name;
    }
    if let Some(protocol) = value_arg(args, "--protocol") {
        draft.protocol = Protocol::parse(&protocol).ok_or_else(|| {
            format!("未知协议：{protocol}（可用 vmess/vless/trojan/anytls/ss/hysteria2/tuic/socks/http/wireguard/openvpn/openconnect）")
        })?;
    }
    draft.port = draft.protocol.default_port();
    if let Some(path) = value_arg(args, "--wireguard-config") {
        let json = std::fs::read(path).map_err(|e| format!("读取 WireGuard 配置失败：{e}"))?;
        let wg: xross_core::model::WireguardOptions =
            serde_json::from_slice(&json).map_err(|_| "WireGuard 配置格式无效".to_string())?;
        wg.validate().map_err(|e| e.to_string())?;
        draft.wireguard_private_key = wg.private_key;
        draft.wireguard_public_key = wg.public_key;
        draft.wireguard_pre_shared_key = wg.pre_shared_key;
        draft.wireguard_addresses = wg.addresses.join(",");
        draft.wireguard_allowed_ips = wg.allowed_ips.join(",");
        draft.wireguard_mtu = wg.mtu.to_string();
        draft.wireguard_reserved = wg
            .reserved
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
    }
    if let Some(path) = value_arg(args, "--vpn-config") {
        let content = std::fs::read(path).map_err(|e| format!("读取 VPN 配置失败：{e}"))?;
        match draft.protocol {
            Protocol::Openvpn => {
                draft.openvpn =
                    serde_json::from_slice(&content).map_err(|_| "OpenVPN 配置格式无效")?
            }
            Protocol::Openconnect => {
                draft.openconnect =
                    serde_json::from_slice(&content).map_err(|_| "OpenConnect 配置格式无效")?
            }
            _ => return Err("--vpn-config 仅用于 OpenVPN / OpenConnect".into()),
        }
    }
    if draft.protocol.is_vpn() {
        let certificates = if draft.protocol == Protocol::Openvpn {
            &mut draft.openvpn.certificates
        } else {
            &mut draft.openconnect.certificates
        };
        for (flag, target) in [
            ("--ca", &mut certificates.authority_pem),
            ("--client-cert", &mut certificates.certificate_pem),
            ("--client-key", &mut certificates.private_key_pem),
        ] {
            if let Some(path) = value_arg(args, flag) {
                *target = std::fs::read_to_string(path)
                    .map_err(|_| format!("无法读取 {flag} 文件，请检查路径和 UTF-8 PEM 内容"))?;
            }
        }
        if let Some(value) = value_arg(args, "--key-password") {
            certificates.private_key_password = value;
        }
        if let Some(value) = value_arg(args, "--vpn-flavor") {
            draft.openconnect.flavor = value;
        }
        if let Some(value) = value_arg(args, "--auth-group") {
            draft.openconnect.auth_group = value;
        }
        if let Some(value) = value_arg(args, "--vpn-path") {
            draft.openconnect.path = value;
        }
        if has_flag(args, "--no-udp") {
            draft.openconnect.disable_udp = true;
        }
    }
    draft.server =
        value_arg(args, "--server").ok_or_else(|| "缺少 --server <主机或 IP>".to_string())?;
    if let Some(port) = value_arg(args, "--port") {
        draft.port = port
            .parse::<u16>()
            .map_err(|_| format!("端口无效：{port}（应为 1-65535）"))?;
    }
    if let Some(value) = value_arg(args, "--uuid") {
        draft.uuid = value;
    }
    if let Some(value) = value_arg(args, "--password") {
        draft.password = value;
    }
    if let Some(value) = value_arg(args, "--username") {
        draft.username = value;
    }
    if let Some(value) = value_arg(args, "--method") {
        draft.method = value;
    }
    if let Some(value) = value_arg(args, "--flow") {
        draft.flow = value;
    }
    if let Some(value) = value_arg(args, "--sni") {
        draft.sni = value;
    }
    if let Some(value) = value_arg(args, "--alpn") {
        draft.alpn = value;
    }
    if let Some(value) = value_arg(args, "--fingerprint") {
        draft.fingerprint = value;
    }
    if let Some(value) = value_arg(args, "--path") {
        draft.transport_path = value;
    }
    if let Some(value) = value_arg(args, "--host") {
        draft.transport_host = value;
    }
    if let Some(value) = value_arg(args, "--service-name") {
        draft.transport_service_name = value;
    }
    if let Some(value) = value_arg(args, "--reality-pbk") {
        draft.reality_public_key = value;
    }
    if let Some(value) = value_arg(args, "--reality-sid") {
        draft.reality_short_id = value;
    }
    if let Some(value) = value_arg(args, "--tags") {
        draft.tags = value;
    }
    if let Some(transport) = value_arg(args, "--transport") {
        if draft.protocol == Protocol::Openvpn {
            draft.openvpn.transport = transport;
        } else {
            draft.transport = TransportKind::parse(&transport)
                .ok_or_else(|| format!("未知传输：{transport}（tcp/ws/grpc/h2/httpupgrade）"))?;
        }
    }
    draft.tls_enabled = !has_flag(args, "--no-tls");
    draft.insecure = has_flag(args, "--insecure");
    draft.udp = !has_flag(args, "--no-udp");

    let service = service(home)?;
    if let Some(core) = core {
        service.set_core(core);
    }
    let id = service.add_manual_node(draft).map_err(|e| e.to_string())?;
    match service.model().node(&id).cloned() {
        Some(node) => println!(
            "已添加节点：{}（{} {}:{}）",
            node.name,
            node.protocol.display_name(),
            node.server,
            node.port
        ),
        None => println!("已添加节点 {id}"),
    }
    Ok(())
}

fn helper_command(args: &[String]) -> Result<(), String> {
    let run_flag = |flag: &str| {
        privilege::helper_main(&[flag.to_string()])
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    match args.first().map(String::as_str) {
        Some("status") => run_flag(privilege::STATUS_ARG),
        Some("install") => {
            println!("{}", privilege::install().map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("update") => {
            println!("{}", privilege::update().map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("uninstall") => {
            println!("{}", privilege::uninstall().map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("start") => {
            println!("{}", privilege::start().map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("restart") => {
            println!("{}", privilege::restart().map_err(|e| e.to_string())?);
            Ok(())
        }
        Some("stop") => {
            privilege::request_shutdown().map_err(|e| e.to_string())?;
            println!("已请求提权助手退出");
            Ok(())
        }
        Some("run") => {
            let mut full = vec![privilege::HELPER_ARG.to_string()];
            full.extend(args[1..].iter().cloned());
            privilege::helper_main(&full)
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        _ => {
            eprintln!(
                "用法：xross-cli helper status|install|update|start|restart|stop|uninstall|run"
            );
            std::process::exit(2);
        }
    }
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
    let network = service.resource_network();
    let candidate = runtime
        .block_on(xross_core::updates::check_core_update(
            core,
            current.as_deref(),
            &network,
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
            .block_on(xross_core::updates::download_checksum(url, &network))
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
            &network,
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
