//! Platform network integration. Commands use fixed programs and typed arguments.
#[cfg(unix)]
#[path = "platform_unix.rs"]
mod unix;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(unix)]
pub use unix::{baseline_with_proxy_tun, interface_routes};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkBaseline {
    #[serde(default)]
    pub proxy_tun_addresses: Vec<String>,
    #[serde(default)]
    proxy_indices: Vec<u32>,
    pub protected: Vec<String>,
    pub defaults: Vec<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    #[serde(default)]
    pub mesh_routes: Vec<super::coexist::MeshRoute>,
}

impl NetworkBaseline {
    #[cfg(any(windows, test))]
    fn exclude_proxy_interfaces(&mut self) {
        let indices = std::mem::take(&mut self.proxy_indices);
        self.dns.retain(|row| {
            !row.split('|')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|i| indices.contains(&i))
        });
        self.defaults.retain(|row| {
            !row.rsplit('|')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|i| indices.contains(&i))
        });
    }

    /// Newly created adapters may add DNS rows without changing existing DNS.
    pub fn preserves_connectivity(&self, original: &Self) -> bool {
        self.connectivity_change(original).is_none()
    }

    pub fn connectivity_change(&self, original: &Self) -> Option<String> {
        if self.defaults != original.defaults {
            let indices = |rows: &[String], other: &[String]| {
                rows.iter()
                    .filter(|r| !other.contains(r))
                    .filter_map(|r| r.rsplit('|').next())
                    .collect::<Vec<_>>()
                    .join("、")
            };
            return Some(format!(
                "默认路由变化（新增接口：{}；移除接口：{}）",
                indices(&self.defaults, &original.defaults),
                indices(&original.defaults, &self.defaults)
            ));
        }
        for entry in &original.dns {
            let mut fields = entry.splitn(3, '|');
            let (Some(index), Some(family), Some(servers)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Some("DNS 基线格式无效".into());
            };
            let prefix = format!("{index}|{family}|");
            let current = self.dns.iter().find_map(|r| r.strip_prefix(&prefix));
            // Windows creates/removes empty Teredo DNS rows during TUN restarts.
            // Missing empty rows are harmless; assigning DNS to an existing
            // empty row or losing an actual resolver must still be detected.
            if current.unwrap_or("") != servers {
                return Some(format!(
                    "接口 {index} 的 {} DNS 变化",
                    if family == "2" { "IPv4" } else { "IPv6" }
                ));
            }
        }
        None
    }
}

#[cfg(windows)]
pub fn powershell(script: &str) -> Result<String> {
    use base64::Engine;
    use std::os::windows::process::CommandExt;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut child = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &base64::engine::general_purpose::STANDARD.encode(bytes),
        ])
        .creation_flags(0x08000000)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| Error::other("无法执行 Windows 网络操作"))?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| Error::other("无法读取网络操作结果"))?;
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = std::io::Read::read_to_end(&mut output, &mut data);
        data
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| Error::other("网络操作状态异常"))?
        {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::other("Windows 网络操作超时，请检查系统网络服务"));
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    };
    let output = reader.join().unwrap_or_default();
    if !status.success() {
        return Err(Error::other(
            "Windows 网络操作失败，请检查管理员权限和系统策略",
        ));
    }
    Ok(String::from_utf8_lossy(&output)
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string())
}
#[cfg(not(windows))]
pub fn powershell(_: &str) -> Result<String> {
    Err(Error::other("当前平台不支持 Windows 网络操作"))
}
#[cfg(windows)]
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn baseline() -> Result<NetworkBaseline> {
    baseline_with_proxy_tun(&[])
}

/// Keep addresses through stop/restart, including proxies started after mesh.
pub fn worker_baseline(known: &mut Vec<String>) -> Result<NetworkBaseline> {
    fn refresh(known: &mut Vec<String>) -> Result<bool> {
        #[cfg(windows)]
        {
            use crate::system::privilege::{HelperRequest, call};
            let reply = call(&HelperRequest::ProxyTunAddresses)?;
            if !reply.is_ok() {
                return Err(Error::other("无法读取提权助手的代理 TUN 状态"));
            }
            let addresses: Vec<String> = serde_json::from_str(reply.message())?;
            let old = known.clone();
            known.extend(addresses);
            known.sort();
            known.dedup();
            return Ok(*known != old);
        }
        #[cfg(not(windows))]
        {
            let _ = known;
            Ok(false)
        }
    }
    refresh(known)?;
    let current = baseline_with_proxy_tun(known)?;
    // A core can start while Windows is collecting the network snapshot.
    if refresh(known)? {
        baseline_with_proxy_tun(known)
    } else {
        Ok(current)
    }
}

#[cfg(windows)]
pub fn baseline_with_proxy_tun(addresses: &[String]) -> Result<NetworkBaseline> {
    let addresses = addresses
        .iter()
        .map(|s| s.parse::<std::net::IpAddr>().map(|ip| ip.to_string()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::config("代理 TUN 地址无效"))?;
    let prefix = format!(
        "$ErrorActionPreference='Stop'; $proxyAddresses=@({}); $tunIndices=@(Get-NetAdapter -IncludeHidden | Where-Object {{$_.InterfaceDescription -like 'sing-tun Tunnel*'}} | Select-Object -ExpandProperty InterfaceIndex); $ips=@(Get-NetIPAddress); $proxyIndices=@($ips | Where-Object {{$_.IPAddress -in $proxyAddresses -and $_.InterfaceIndex -in $tunIndices}} | Select-Object -ExpandProperty InterfaceIndex -Unique); ",
        addresses
            .iter()
            .map(|s| quote(s))
            .collect::<Vec<_>>()
            .join(",")
    );
    let raw = powershell(
        &(prefix
            + "$ErrorActionPreference='Stop'; $r=@(Get-NetRoute -ErrorAction Stop); [pscustomobject]@{ proxy_indices=@($proxyIndices); mesh_routes=@($r | Where-Object {$_.InterfaceAlias -like 'xross-mesh-*'} | ForEach-Object { [pscustomobject]@{cidr=$_.DestinationPrefix; interface=$_.InterfaceAlias} }); protected=@($ips | Where-Object { $_.InterfaceAlias -notlike 'xross-mesh-*' -and $_.InterfaceAlias -notlike '*Loopback*' } | ForEach-Object { \"$($_.IPAddress)/$($_.PrefixLength)\" }); dns=@(Get-DnsClientServerAddress | Where-Object {$_.InterfaceAlias -notlike 'xross-mesh-*'} | ForEach-Object { \"$($_.InterfaceIndex)|$($_.AddressFamily)|$($_.ServerAddresses -join ',')\" } | Sort-Object); defaults=@($r | Where-Object {$_.DestinationPrefix -in @('0.0.0.0/0','::/0')} | ForEach-Object { \"$($_.DestinationPrefix)|$($_.NextHop)|$($_.InterfaceIndex)\" } | Sort-Object) } | ConvertTo-Json -Compress"),
    )?;
    let mut baseline: NetworkBaseline =
        serde_json::from_str(&raw).map_err(|_| Error::other("无法读取本机路由基线"))?;
    baseline.exclude_proxy_interfaces();
    baseline.proxy_tun_addresses = addresses;
    Ok(baseline)
}
#[cfg(windows)]
pub fn interface_routes(interface: &str) -> Result<Vec<String>> {
    let raw = powershell(&format!(
        "$ErrorActionPreference='Stop'; ConvertTo-Json -Compress -InputObject @(Get-NetRoute -InterfaceAlias {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty DestinationPrefix -Unique)",
        quote(interface)
    ))?;
    serde_json::from_str(&raw).map_err(|_| Error::other("无法读取组网接口路由"))
}
pub fn secure_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|_| Error::other("无法创建组网私有目录"))?;
    #[cfg(windows)]
    {
        powershell(&format!(
            "$ErrorActionPreference='Stop'; $p={}; $acl=New-Object System.Security.AccessControl.DirectorySecurity; $acl.SetAccessRuleProtection($true,$false); foreach($sid in @([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value,'S-1-5-18','S-1-5-32-544')) {{ $rule=New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier($sid)),'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $acl.AddAccessRule($rule) }}; [System.IO.Directory]::SetAccessControl($p,$acl)",
            quote(&path.to_string_lossy())
        ))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// NRPT changes are journaled before applying; only our exact rule ID is removed.
pub fn set_dns(root: &Path, session: &str, suffix: &str, address: &str) -> Result<()> {
    if !super::coexist::valid_suffix(suffix)
        || address
            .parse::<std::net::IpAddr>()
            .map_or(true, |ip| !ip.is_loopback())
    {
        return Err(Error::config("组网 DNS 配置无效"));
    }
    let marker = format!("Xross mesh {session}");
    let journal = root.join("dns-owner.json");
    crate::util::atomic_write_json(&journal, &marker)?;
    #[cfg(windows)]
    {
        let script = format!(
            "$ErrorActionPreference='Stop'; $m={}; $n={}; $existing=@(Get-DnsClientNrptRule | Where-Object {{$_.DisplayName -eq $m}}); if($existing.Count -eq 0) {{ $conflict=@(Get-DnsClientNrptRule | Where-Object {{$_.Namespace -contains $n -and $_.DisplayName -ne $m}}); if($conflict.Count -gt 0) {{ throw 'DNS namespace already owned' }}; Add-DnsClientNrptRule -Namespace $n -NameServers {} -DisplayName $m -Comment 'Xross session-owned MagicDNS' }}",
            quote(&marker),
            quote(&format!(".{suffix}")),
            quote(address)
        );
        powershell(&script)?;
    }
    #[cfg(unix)]
    unix::set_dns(root, session, suffix, address)?;
    Ok(())
}
pub fn cleanup_dns(root: &Path) -> Result<()> {
    let journal = root.join("dns-owner.json");
    if !journal.exists() {
        return Ok(());
    }
    let marker: String = serde_json::from_slice(
        &std::fs::read(&journal).map_err(|_| Error::other("读取 DNS 恢复记录失败"))?,
    )?;
    let id = root
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(|| Error::other("DNS 恢复目录无效"))?;
    if uuid::Uuid::parse_str(id).is_err() || marker != format!("Xross mesh {id}") {
        return Err(Error::other("DNS 恢复记录无效"));
    }
    #[cfg(windows)]
    powershell(&format!(
        "$ErrorActionPreference='Stop'; Get-DnsClientNrptRule | Where-Object {{$_.DisplayName -eq {}}} | ForEach-Object {{ Remove-DnsClientNrptRule -Name $_.Name -Force }}",
        quote(&marker)
    ))?;
    #[cfg(unix)]
    unix::cleanup_dns(root, id)?;
    std::fs::remove_file(journal).map_err(|_| Error::other("清理 DNS 恢复记录失败"))?;
    Ok(())
}

/// Only inspect our per-profile directories, never rules owned by other apps.
pub fn recover_dns(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for item in std::fs::read_dir(root).map_err(|_| Error::other("无法读取组网恢复记录"))?
    {
        let item = item.map_err(|_| Error::other("无法读取组网恢复目录"))?;
        if item
            .file_type()
            .map(|t| t.is_dir() && !t.is_symlink())
            .unwrap_or(false)
            && item
                .file_name()
                .to_str()
                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
        {
            cleanup_dns(&item.path())?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub struct OwnerProcess(std::os::windows::io::OwnedHandle);
#[cfg(windows)]
impl OwnerProcess {
    pub fn open(pid: u32) -> Result<Self> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            Security::{EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
            System::Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x00100000, 0, pid);
            if handle.is_null() {
                return Err(Error::other("组网所属进程已退出或不可访问"));
            }
            let owned = std::os::windows::io::OwnedHandle::from_raw_handle(handle);
            let mut other = std::ptr::null_mut();
            let mut current = std::ptr::null_mut();
            if OpenProcessToken(handle, TOKEN_QUERY, &mut other) == 0 {
                return Err(Error::other("无法验证组网调用者"));
            }
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut current) == 0 {
                CloseHandle(other);
                return Err(Error::other("无法验证助手身份"));
            }
            let mut size_a = 0;
            let mut size_b = 0;
            GetTokenInformation(other, TokenUser, std::ptr::null_mut(), 0, &mut size_a);
            GetTokenInformation(current, TokenUser, std::ptr::null_mut(), 0, &mut size_b);
            let mut a = vec![0usize; (size_a as usize + 7) / 8];
            let mut b = vec![0usize; (size_b as usize + 7) / 8];
            let ok =
                GetTokenInformation(other, TokenUser, a.as_mut_ptr().cast(), size_a, &mut size_a)
                    != 0
                    && GetTokenInformation(
                        current,
                        TokenUser,
                        b.as_mut_ptr().cast(),
                        size_b,
                        &mut size_b,
                    ) != 0;
            CloseHandle(other);
            CloseHandle(current);
            if !ok
                || EqualSid(
                    (*(a.as_ptr() as *const TOKEN_USER)).User.Sid,
                    (*(b.as_ptr() as *const TOKEN_USER)).User.Sid,
                ) == 0
            {
                return Err(Error::other("组网助手只接受同一用户的连接"));
            }
            Ok(Self(owned))
        }
    }
    pub fn alive(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(self.0.as_raw_handle(), 0)
                == 258
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_teredo_rows_can_disappear_without_relaxing_dns_protection() {
        let original = NetworkBaseline {
            dns: vec!["27|2|10.10.10.1".into(), "19|2|".into(), "19|23|".into()],
            ..Default::default()
        };
        let mut current = original.clone();
        current.dns.retain(|row| !row.starts_with("19|"));
        assert!(current.preserves_connectivity(&original));
        current.dns.push("19|23|".into());
        assert!(current.preserves_connectivity(&original));
        current.dns.push("19|2|127.0.0.1".into());
        assert_eq!(
            current.connectivity_change(&original).unwrap(),
            "接口 19 的 IPv4 DNS 变化"
        );
        current.dns = vec!["19|2|".into(), "19|23|".into()];
        assert_eq!(
            current.connectivity_change(&original).unwrap(),
            "接口 27 的 IPv4 DNS 变化"
        );
    }
    #[test]
    fn proxy_restart_keeps_physical_dns_and_routes_protected() {
        fn snapshot(index: u32) -> NetworkBaseline {
            let mut value = NetworkBaseline {
                defaults: vec!["0.0.0.0/0|10.10.10.1|27".into(), format!("::/0|::|{index}")],
                dns: vec!["27|2|10.10.10.1".into(), format!("{index}|2|172.19.0.2")],
                proxy_indices: vec![index],
                ..Default::default()
            };
            value.exclude_proxy_interfaces();
            value
        }
        let original = snapshot(4);
        let mut restarted = snapshot(55);
        assert!(restarted.preserves_connectivity(&original));
        restarted.dns[0] = "27|2|127.0.0.1".into();
        assert!(!restarted.preserves_connectivity(&original));
        restarted = snapshot(55);
        restarted.defaults.push("0.0.0.0/0|192.0.2.1|99".into());
        assert!(!restarted.preserves_connectivity(&original));
    }

    #[test]
    fn new_adapter_does_not_hide_changes_to_original_connectivity() {
        let original = NetworkBaseline {
            defaults: vec!["0.0.0.0/0|10.10.10.1|7".into()],
            dns: vec!["7|2|10.10.10.1".into()],
            ..Default::default()
        };
        let mut current = original.clone();
        current.dns.push("42|2|".into());
        assert!(current.preserves_connectivity(&original));
        current.dns[0] = "7|2|127.0.0.1".into();
        assert!(!current.preserves_connectivity(&original));
        current.dns.remove(0);
        assert!(!current.preserves_connectivity(&original));
        current = original.clone();
        current.defaults.push("0.0.0.0/0|10.254.254.1|42".into());
        assert!(!current.preserves_connectivity(&original));
    }
    #[test]
    fn foreign_dns_journal_is_never_cleaned() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&profile).unwrap();
        let journal = profile.join("dns-owner.json");
        crate::util::atomic_write_json(&journal, &"Some other application").unwrap();
        assert!(cleanup_dns(&profile).is_err());
        assert!(journal.exists());
        crate::util::atomic_write_json(&journal, &format!("Xross mesh {}", uuid::Uuid::new_v4()))
            .unwrap();
        assert!(cleanup_dns(&profile).is_err());
        assert!(journal.exists());
    }
    #[test]
    fn invalid_dns_never_writes_a_journal() {
        let root = tempfile::tempdir().unwrap();
        assert!(set_dns(root.path(), "test", "ts.net", "127.0.0.153").is_err());
        assert!(set_dns(root.path(), "test", "test.ts.net", "10.10.10.1").is_err());
        assert!(!root.path().join("dns-owner.json").exists());
    }
}
#[cfg(unix)]
pub struct OwnerProcess(i32);
/// Workers own a process group so a stuck worker cannot orphan its VPN child.
#[cfg(unix)]
pub fn kill_worker_group(pid: u32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    if let Ok(pid) = i32::try_from(pid) {
        if pid > 1 {
            unsafe {
                kill(-pid, 9);
            }
        }
    }
}
#[cfg(unix)]
impl OwnerProcess {
    pub fn open(pid: u32) -> Result<Self> {
        let pid = i32::try_from(pid)
            .ok()
            .filter(|&p| p > 0)
            .ok_or_else(|| Error::other("所属进程无效"))?;
        let owner = Self(pid);
        if !owner.alive() {
            return Err(Error::other("所属进程已退出或不可访问"));
        }
        Ok(owner)
    }
    pub fn alive(&self) -> bool {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        if unsafe { kill(self.0, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(1)
    }
}

#[cfg(not(any(windows, unix)))]
pub struct OwnerProcess;
#[cfg(not(any(windows, unix)))]
impl OwnerProcess {
    pub fn open(_: u32) -> Result<Self> {
        Err(Error::other("当前平台不支持进程归属检测"))
    }
    pub fn alive(&self) -> bool {
        false
    }
}
