//! Windows system integration. Commands use fixed scripts and typed arguments.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkBaseline {
    pub protected: Vec<String>,
    pub defaults: Vec<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    #[serde(default)]
    pub mesh_routes: Vec<super::coexist::MeshRoute>,
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
    Err(Error::other("异地组网当前仅在 Windows 启用"))
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn baseline() -> Result<NetworkBaseline> {
    let raw = powershell("$ErrorActionPreference='Stop'; $r=@(Get-NetRoute -ErrorAction Stop); [pscustomobject]@{ mesh_routes=@($r | Where-Object {$_.InterfaceAlias -like 'xross-mesh-*'} | ForEach-Object { [pscustomobject]@{cidr=$_.DestinationPrefix; interface=$_.InterfaceAlias} }); protected=@(Get-NetIPAddress | Where-Object { $_.InterfaceAlias -notlike 'xross-mesh-*' -and $_.InterfaceAlias -notlike '*Loopback*' } | ForEach-Object { \"$($_.IPAddress)/$($_.PrefixLength)\" }); dns=@(Get-DnsClientServerAddress | Where-Object {$_.InterfaceAlias -notlike 'xross-mesh-*'} | ForEach-Object { \"$($_.InterfaceIndex)|$($_.AddressFamily)|$($_.ServerAddresses -join ',')\" } | Sort-Object); defaults=@($r | Where-Object {$_.DestinationPrefix -in @('0.0.0.0/0','::/0')} | ForEach-Object { \"$($_.DestinationPrefix)|$($_.NextHop)|$($_.InterfaceIndex)\" } | Sort-Object) } | ConvertTo-Json -Compress")?;
    serde_json::from_str(&raw).map_err(|_| Error::other("无法读取本机路由基线"))
}
pub fn interface_routes(interface: &str) -> Result<Vec<String>> {
    let raw = powershell(&format!("$ErrorActionPreference='Stop'; ConvertTo-Json -Compress -InputObject @(Get-NetRoute -InterfaceAlias {} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty DestinationPrefix -Unique)", quote(interface)))?;
    serde_json::from_str(&raw).map_err(|_| Error::other("无法读取组网接口路由"))
}
pub fn secure_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|_| Error::other("无法创建组网私有目录"))?;
    #[cfg(windows)]
    {
        powershell(&format!("$ErrorActionPreference='Stop'; $p={}; $acl=New-Object System.Security.AccessControl.DirectorySecurity; $acl.SetAccessRuleProtection($true,$false); foreach($sid in @([System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value,'S-1-5-18','S-1-5-32-544')) {{ $rule=New-Object System.Security.AccessControl.FileSystemAccessRule((New-Object System.Security.Principal.SecurityIdentifier($sid)),'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $acl.AddAccessRule($rule) }}; [System.IO.Directory]::SetAccessControl($p,$acl)",quote(&path.to_string_lossy())))?;
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
    let script = format!("$ErrorActionPreference='Stop'; $m={}; $n={}; $existing=@(Get-DnsClientNrptRule | Where-Object {{$_.DisplayName -eq $m}}); if($existing.Count -eq 0) {{ $conflict=@(Get-DnsClientNrptRule | Where-Object {{$_.Namespace -contains $n -and $_.DisplayName -ne $m}}); if($conflict.Count -gt 0) {{ throw 'DNS namespace already owned' }}; Add-DnsClientNrptRule -Namespace $n -NameServers {} -DisplayName $m -Comment 'Xross session-owned MagicDNS' }}",quote(&marker),quote(&format!(".{suffix}")),quote(address));
    powershell(&script)?;
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
    powershell(&format!("$ErrorActionPreference='Stop'; Get-DnsClientNrptRule | Where-Object {{$_.DisplayName -eq {}}} | ForEach-Object {{ Remove-DnsClientNrptRule -Name $_.Name -Force }}",quote(&marker)))?;
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
            Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER},
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
#[cfg(not(windows))]
pub struct OwnerProcess;
#[cfg(not(windows))]
impl OwnerProcess {
    pub fn open(_: u32) -> Result<Self> {
        Err(Error::other("异地组网当前仅在 Windows 启用"))
    }
    pub fn alive(&self) -> bool {
        false
    }
}
