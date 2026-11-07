//! Identifies TCP listeners belonging to the core managed by this instance.

use std::io;
use std::net::{IpAddr, SocketAddr};

/// Returns listening endpoints, never established connections or configured ports.
#[cfg(windows)]
pub fn listeners(pid: u32) -> io::Result<Vec<SocketAddr>> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{
        ERROR_INSUFFICIENT_BUFFER, ERROR_NO_DATA, ERROR_NOT_SUPPORTED,
    };
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    let mut listeners = Vec::new();
    for family in [AF_INET, AF_INET6] {
        let mut bytes = 0;
        // The first call obtains the size; the table can grow between calls.
        unsafe {
            GetExtendedTcpTable(
                std::ptr::null_mut(),
                &mut bytes,
                0,
                family as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            );
        }
        for attempt in 0..3 {
            let mut table = vec![0u32; (bytes as usize).div_ceil(size_of::<u32>())];
            let result = unsafe {
                GetExtendedTcpTable(
                    table.as_mut_ptr().cast(),
                    &mut bytes,
                    0,
                    family as u32,
                    TCP_TABLE_OWNER_PID_LISTENER,
                    0,
                )
            };
            if result == ERROR_INSUFFICIENT_BUFFER && attempt < 2 {
                continue;
            }
            if result == ERROR_NO_DATA || (family == AF_INET6 && result == ERROR_NOT_SUPPORTED) {
                break;
            }
            if result != 0 {
                return Err(io::Error::from_raw_os_error(result as i32));
            }
            if table.is_empty() {
                break;
            }
            let row_size = if family == AF_INET {
                size_of::<MIB_TCPROW_OWNER_PID>()
            } else {
                size_of::<MIB_TCP6ROW_OWNER_PID>()
            };
            let count = (table[0] as usize).min((table.len() * 4 - 4) / row_size);
            for index in 0..count {
                let row = unsafe { table.as_ptr().cast::<u8>().add(4 + index * row_size) };
                let (owner, ip, port) = if family == AF_INET {
                    let row = unsafe { row.cast::<MIB_TCPROW_OWNER_PID>().read_unaligned() };
                    (
                        row.dwOwningPid,
                        IpAddr::from(row.dwLocalAddr.to_ne_bytes()),
                        row.dwLocalPort,
                    )
                } else {
                    let row = unsafe { row.cast::<MIB_TCP6ROW_OWNER_PID>().read_unaligned() };
                    (
                        row.dwOwningPid,
                        IpAddr::from(row.ucLocalAddr),
                        row.dwLocalPort,
                    )
                };
                if owner == pid {
                    listeners.push(SocketAddr::new(ip, u16::from_be(port as u16)));
                }
            }
            break;
        }
    }
    Ok(listeners)
}

// Used by the Windows core startup readiness check.
#[cfg(windows)]
pub fn owns_listener(pid: u32, port: u16) -> bool {
    listeners(pid).is_ok_and(|listeners| {
        listeners.iter().any(|addr| {
            addr.port() == port && (addr.ip().is_unspecified() || addr.ip().is_loopback())
        })
    })
}

#[cfg(target_os = "linux")]
pub fn listeners(pid: u32) -> io::Result<Vec<SocketAddr>> {
    use std::collections::HashSet;
    use std::fs;
    let mut sockets = HashSet::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd"))? {
        let link = match fs::read_link(entry?.path()) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            result => result?,
        };
        if let Some(inode) = link
            .to_str()
            .and_then(|s| s.strip_prefix("socket:[")?.strip_suffix(']'))
        {
            sockets.insert(inode.to_owned());
        }
    }
    let mut listeners = Vec::new();
    for file in ["tcp", "tcp6"] {
        let table = match fs::read_to_string(format!("/proc/{pid}/net/{file}")) {
            Err(e) if file == "tcp6" && e.kind() == io::ErrorKind::NotFound => continue,
            result => result?,
        };
        for row in table.lines().skip(1) {
            let columns: Vec<_> = row.split_whitespace().collect();
            if columns.len() > 9 && columns[3] == "0A" && sockets.contains(columns[9]) {
                if let Some(addr) = proc_address(columns[1]) {
                    listeners.push(addr);
                }
            }
        }
    }
    Ok(listeners)
}

#[cfg(any(target_os = "linux", test))]
fn proc_address(field: &str) -> Option<SocketAddr> {
    let (ip, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    // procfs prints each native-endian 32-bit word in hexadecimal.
    let word = |s: &str| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
    let ip = match ip.len() {
        8 => IpAddr::from(word(ip)?),
        32 => {
            let mut octets = [0; 16];
            for (index, dst) in octets.chunks_exact_mut(4).enumerate() {
                dst.copy_from_slice(&word(ip.get(index * 8..index * 8 + 8)?)?);
            }
            IpAddr::from(octets)
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(target_os = "macos")]
pub fn listeners(pid: u32) -> io::Result<Vec<SocketAddr>> {
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Ftn",
        ])
        .output()?;
    if !output.status.success() {
        // lsof exits with 1 and no diagnostic when no visible files match.
        // The GUI treats its view of a privileged child as incomplete.
        if output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty() {
            return Ok(Vec::new());
        }
        return Err(io::Error::other("无法读取内核监听端口"));
    }
    Ok(lsof_addresses(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(any(target_os = "macos", test))]
fn lsof_addresses(fields: &str) -> Vec<SocketAddr> {
    let mut wildcard = None;
    let mut addresses = Vec::new();
    for line in fields.lines() {
        if let Some(kind) = line.strip_prefix('t') {
            wildcard = match kind {
                "IPv4" => Some(IpAddr::from([0; 4])),
                "IPv6" => Some(IpAddr::from([0; 16])),
                _ => None,
            };
        } else if let Some(name) = line.strip_prefix('n') {
            if let Some(addr) = name.parse().ok().or_else(|| {
                Some(SocketAddr::new(
                    wildcard?,
                    name.strip_prefix("*:")?.parse().ok()?,
                ))
            }) {
                addresses.push(addr);
            }
        }
    }
    addresses
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn listeners(_pid: u32) -> io::Result<Vec<SocketAddr>> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn reads_the_process_listeners() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert!(
            listeners(std::process::id())
                .unwrap()
                .contains(&listener.local_addr().unwrap())
        );
    }

    #[test]
    fn parses_lsof_wildcards_and_loopback() {
        let fields = "p42\nf3\ntIPv6\nn*:8080\nf4\ntIPv4\nn127.0.0.1:9090\n";
        assert_eq!(
            lsof_addresses(fields),
            ["[::]:8080", "127.0.0.1:9090"].map(|s| s.parse::<SocketAddr>().unwrap())
        );
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn parses_procfs_loopback() {
        assert_eq!(proc_address("0100007F:0050"), "127.0.0.1:80".parse().ok());
        assert_eq!(
            proc_address("00000000000000000000000001000000:1F90"),
            "[::1]:8080".parse().ok()
        );
    }
}

/// A read-only snapshot for both TCP listeners and bound UDP sockets. Commands
/// run on the diagnostics worker and never request elevated privileges.
#[derive(Clone, Debug)]
pub(super) struct Owner {
    pub proto: &'static str,
    pub addr: SocketAddr,
    pub pid: u32,
    pub name: String,
}

#[cfg(target_os = "macos")]
pub(super) fn visible_owners() -> Vec<Owner> {
    let mut owners = Vec::new();
    for proto in ["tcp", "udp"] {
        let mut cmd = std::process::Command::new("/usr/sbin/lsof");
        cmd.args(["-nP", "-i"]).arg(proto);
        if proto == "tcp" {
            cmd.arg("-sTCP:LISTEN");
        }
        let Ok(result) = cmd.arg("-Fpctn").output() else {
            continue;
        };
        let (mut pid, mut process, mut v6) = (0, "", false);
        let fields = String::from_utf8_lossy(&result.stdout);
        for line in fields.lines() {
            if let Some(p) = line.strip_prefix('p') {
                pid = p.parse().unwrap_or(0);
                process = "";
            } else if let Some(c) = line.strip_prefix('c') {
                process = c;
            } else if let Some(t) = line.strip_prefix('t') {
                v6 = t == "IPv6";
            } else if let Some(endpoint) = line.strip_prefix('n') {
                let endpoint = endpoint.split("->").next().unwrap_or(endpoint);
                let addr = endpoint.parse().ok().or_else(|| {
                    let port = endpoint.strip_prefix("*:")?.parse().ok()?;
                    Some(if v6 {
                        SocketAddr::from(([0; 16], port))
                    } else {
                        SocketAddr::from(([0; 4], port))
                    })
                });
                if let Some(addr) = addr {
                    owners.push(Owner {
                        proto,
                        addr,
                        pid,
                        name: process.into(),
                    });
                }
            }
        }
    }
    owners
}

#[cfg(target_os = "linux")]
pub(super) fn visible_owners() -> Vec<Owner> {
    let mut owners = Vec::new();
    let Ok(result) = std::process::Command::new("ss")
        .args(["-H", "-lntup"])
        .output()
    else {
        return owners;
    };
    let re = regex::Regex::new(r#"\("([^"]+)",pid=(\d+)"#).expect("valid ss pattern");
    for line in String::from_utf8_lossy(&result.stdout).lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let proto = match fields.first() {
            Some(&"tcp") => "tcp",
            Some(&"udp") => "udp",
            _ => continue,
        };
        let Some(addr) = fields
            .get(4)
            .and_then(|s| s.replace("*:", "[::]:").parse().ok())
        else {
            continue;
        };
        for caps in re.captures_iter(line) {
            if let Ok(pid) = caps[2].parse() {
                owners.push(Owner {
                    proto,
                    addr,
                    pid,
                    name: caps[1].into(),
                });
            }
        }
    }
    owners
}

#[cfg(windows)]
pub(super) fn visible_owners() -> Vec<Owner> {
    let script = r#"$ErrorActionPreference='SilentlyContinue'
$p=@{}; Get-Process | ForEach-Object { $p[$_.Id]=$_.ProcessName }
Get-NetTCPConnection -State Listen | ForEach-Object { 'tcp|{0}|{1}|{2}|{3}' -f $_.LocalAddress,$_.LocalPort,$_.OwningProcess,$p[[int]$_.OwningProcess] }
Get-NetUDPEndpoint | ForEach-Object { 'udp|{0}|{1}|{2}|{3}' -f $_.LocalAddress,$_.LocalPort,$_.OwningProcess,$p[[int]$_.OwningProcess] }"#;
    let Ok(out) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(5, '|');
            let proto = match parts.next()? {
                "tcp" => "tcp",
                "udp" => "udp",
                _ => return None,
            };
            let ip = parts.next()?.split('%').next()?.parse().ok()?;
            let addr = SocketAddr::new(ip, parts.next()?.parse().ok()?);
            Some(Owner {
                proto,
                addr,
                pid: parts.next()?.parse().ok()?,
                name: parts.next()?.trim().into(),
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn visible_owners() -> Vec<Owner> {
    Vec::new()
}
