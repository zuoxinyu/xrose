//! Private worker entry point. Credentials arrive through an inherited pipe.
use super::{
    MeshPhase, MeshProvider, MeshRuntimeView,
    ipc::{Bootstrap, WorkerCommand},
};
use crate::{Error, Result};
use std::io::{BufRead, Read, Write};

fn publish(state: &MeshRuntimeView) {
    if let Ok(json) = serde_json::to_string(state) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "XROSS_MESH:{json}");
        let _ = out.flush();
    }
}
pub fn main() -> Result<()> {
    if !crate::system::privilege::is_elevated() {
        return Err(Error::other("组网工作进程需要特权助手启动"));
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(262145)
        .read_line(&mut line)
        .map_err(|_| Error::other("读取组网启动请求失败"))?;
    if line.len() > 262144 {
        return Err(Error::other("组网请求过大"));
    }
    let boot: Bootstrap =
        serde_json::from_str(&line).map_err(|_| Error::other("组网启动请求无效"))?;
    drop(line);
    boot.profile.validate()?;
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        loop {
            let mut line = String::new();
            if input.by_ref().take(32769).read_line(&mut line).unwrap_or(0) == 0 {
                break;
            }
            if line.len() > 32768 {
                break;
            }
            if let Ok(c) = serde_json::from_str(&line) {
                if sender.blocking_send(c).is_err() {
                    return;
                }
            }
        }
        let _ = sender.blocking_send(WorkerCommand::Stop);
    });
    let mut builder = if boot.profile.provider == MeshProvider::EasyTier
        && !boot.profile.easytier.device.multi_thread
    {
        tokio::runtime::Builder::new_current_thread()
    } else {
        let mut builder = tokio::runtime::Builder::new_multi_thread();
        builder.worker_threads(2);
        builder
    };
    let runtime = builder
        .enable_all()
        .build()
        .map_err(|_| Error::other("组网运行时初始化失败"))?;
    let mut final_state = MeshRuntimeView {
        profile_id: boot.profile.id.clone(),
        provider: boot.profile.provider,
        phase: MeshPhase::Starting,
        ..Default::default()
    };
    publish(&final_state);
    let result=runtime.block_on(async {
        let mut backend=Backend::start(&boot).await?;
        let mut interval=tokio::time::interval(std::time::Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut dns_suffix=None;
        let mut dns_warning=None;
        let mut probe=None;
        let mut current_profile = boot.profile.clone();
        let mut proxy_addresses=boot.baseline.proxy_tun_addresses.clone();
        loop {tokio::select!{
            command=receiver.recv()=> match command {
                Some(WorkerCommand::Stop)|None=>break,
                Some(WorkerCommand::Logout)=>{backend.logout().await?;break;}
                Some(WorkerCommand::SetForwards { id, rules }) => {
                    let result = backend.update_forwards(&mut current_profile, rules).await;
                    final_state.forward_update = Some(super::ForwardUpdate {
                        id,
                        error: result.err().map(|e| e.to_string()),
                    });
                    publish(&final_state);
                }
                Some(WorkerCommand::Probe{ip})=>{
                    let result=backend.probe(ip.clone()).await;
                    probe=Some(match result {Ok(latency_ms)=>super::MeshProbe{ip,success:true,latency_ms,detail:"设备可达".into()},Err(error)=>super::MeshProbe{ip,success:false,latency_ms:None,detail:error.to_string()}});
                    final_state.probe=probe.clone();publish(&final_state);
                }
            },
            _=interval.tick()=>{
                let mut state=backend.snapshot().await?;
                state.probe=probe.clone();
                state.forward_update=final_state.forward_update.clone();
                let current=super::platform::worker_baseline(&mut proxy_addresses)?;
                if let Some(change)=current.connectivity_change(&boot.baseline){return Err(Error::other(format!("检测到{change}，已停止组网以保护现有联网通道")));}
                super::coexist::check_networks(&state.routes,&boot.baseline.protected)?;
                let other_routes=current.mesh_routes.iter().filter(|route| route.interface!=state.interface)
                    .filter(|route| route.cidr.parse::<ipnet::IpNet>().is_ok_and(|n| !n.addr().is_multicast() && n.addr()!=std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST) && !is_link_local(n.addr())))
                    .map(|route|route.cidr.clone()).collect::<Vec<_>>();
                super::coexist::check_networks(&state.routes,&other_routes)?;
                if !state.interface.is_empty(){
                    let installed=current.installed_routes(&state.interface)?;
                    for route in installed {
                        let Ok(net)=route.parse::<ipnet::IpNet>() else{continue;};
                        if net.prefix_len()==0{return Err(Error::other("组网尝试接管默认路由，已停止"));}
                        if net.addr().is_multicast() || net.addr()==std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST) || is_link_local(net.addr()){continue;}
                        super::coexist::check_networks(&[route.clone()],&boot.baseline.protected)?;
                        if state.provider==MeshProvider::Tailscale {
                            let mesh_space=["100.64.0.0/10".parse::<ipnet::IpNet>().unwrap(),"fd7a:115c:a1e0::/48".parse().unwrap(),super::TAILSCALE_TUN_NETWORK.parse().unwrap(),super::TAILSCALE_TUN_IPV6_NETWORK.parse().unwrap()];
                            let allowed=mesh_space.iter().chain(boot.profile.remote_subnets.iter().filter_map(|v|v.parse().ok()).collect::<Vec<ipnet::IpNet>>().iter()).any(|n|n.contains(&net.network())&&n.prefix_len()<=net.prefix_len());
                            if !allowed{return Err(Error::other("Tailscale 下发了未批准的远端网段，已断开；请在配置中明确添加该网段"));}
                        }
                        state.routes.push(net.trunc().to_string());
                    }
                }
                state.routes.sort();state.routes.dedup();
                if state.phase==MeshPhase::Connected && state.dns_suffix!=dns_suffix {
                    super::platform::cleanup_dns(&boot.directory)?;
                    dns_warning=None;
                    if let(Some(suffix),Some(address))=(&state.dns_suffix,&state.dns_address){
                        if let Err(error)=super::platform::set_dns(&boot.directory,&boot.profile.id,suffix,address) {
                            super::platform::cleanup_dns(&boot.directory)?;
                            dns_warning=Some(format!("系统组网域名解析未启用：{error}；可继续通过 IP 或应用代理访问"));
                        }
                    }
                    dns_suffix=state.dns_suffix.clone();
                }
                if state.error.is_none() { state.error=dns_warning.clone(); }
                final_state=state;publish(&final_state);
            }
        }}
        drop(backend);Ok::<(),Error>(())
    });
    let cleanup = super::platform::cleanup_dns(&boot.directory);
    let error = result.and(cleanup).err();
    final_state.finish(error.map(|e| e.to_string()));
    publish(&final_state);
    Ok(())
}
fn is_link_local(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(a) => a.is_link_local(),
        std::net::IpAddr::V6(a) => (a.segments()[0] & 0xffc0) == 0xfe80,
    }
}
enum Backend {
    EasyTier(super::easytier::Backend),
    Tailscale(super::tailscale::Backend),
}
impl Backend {
    async fn start(b: &Bootstrap) -> Result<Self> {
        #[cfg(windows)]
        {
            super::driver::load(&b.directory.join("wintun.dll"))?;
        }
        match b.profile.provider {
            MeshProvider::EasyTier => Ok(Self::EasyTier(
                super::easytier::Backend::start(b.profile.clone()).await?,
            )),
            MeshProvider::Tailscale => Ok(Self::Tailscale(
                super::tailscale::Backend::start(
                    b.profile.clone(),
                    &b.directory,
                    b.tailscale_binary
                        .as_deref()
                        .ok_or_else(|| Error::binary("缺少组网组件"))?,
                    b.sync_port,
                )
                .await?,
            )),
        }
    }
    async fn update_forwards(
        &mut self,
        current: &mut super::MeshProfile,
        rules: Vec<super::PortForward>,
    ) -> Result<()> {
        let mut next = current.clone();
        next.port_forwards = rules;
        next.validate()?;
        if let Err(error) = self.set_forwards(&next.port_forwards).await {
            self.set_forwards(&current.port_forwards)
                .await
                .map_err(|_| Error::other("未能恢复原转发监听，请释放冲突端口后重试"))?;
            return Err(error);
        }
        *current = next;
        Ok(())
    }
    async fn set_forwards(&mut self, rules: &[super::PortForward]) -> Result<()> {
        match self {
            Self::EasyTier(e) => e.set_forwards(rules).await,
            Self::Tailscale(t) => t.forwards.apply(rules).await,
        }
    }
    async fn snapshot(&mut self) -> Result<MeshRuntimeView> {
        match self {
            Self::EasyTier(e) => e.snapshot().await,
            Self::Tailscale(t) => t.snapshot(),
        }
    }
    async fn logout(&mut self) -> Result<()> {
        match self {
            Self::EasyTier(_) => Err(Error::other("EasyTier 使用共享网络密钥，请断开或删除配置")),
            Self::Tailscale(t) => t.logout().await,
        }
    }
    async fn probe(&mut self, ip: String) -> Result<Option<f64>> {
        match self {
            Self::EasyTier(_) => {
                let target = ip
                    .parse::<std::net::IpAddr>()
                    .map_err(|_| Error::config("设备地址无效"))?;
                #[cfg(windows)]
                let mut command = {
                    let mut c = tokio::process::Command::new("ping.exe");
                    c.args(["-n", "1", "-w", "3000"]).creation_flags(0x08000000);
                    c
                };
                #[cfg(unix)]
                let mut command = {
                    let mut c = tokio::process::Command::new("ping");
                    c.args(["-n", "-c", "1"]);
                    c
                };
                command.arg(target.to_string()).kill_on_drop(true);
                let output =
                    tokio::time::timeout(std::time::Duration::from_secs(4), command.output())
                        .await
                        .map_err(|_| Error::other("设备探测超时"))?
                        .map_err(|_| Error::other("启动设备探测失败"))?;
                if output.status.success() {
                    let text = String::from_utf8_lossy(&output.stdout);
                    let latency = regex::Regex::new(r"[=<]\s*(\d+)\s*ms")
                        .unwrap()
                        .captures(&text)
                        .and_then(|c| c[1].parse().ok());
                    Ok(latency)
                } else {
                    Err(Error::other("设备没有响应 ICMP，可能被对端防火墙阻止"))
                }
            }
            Self::Tailscale(t) => t.probe(ip).await,
        }
    }
}
