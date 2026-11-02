//! Identifies TCP listeners belonging to the core managed by this instance.

#[cfg(windows)]
pub fn owns_listener(pid: u32, port: u16) -> bool {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
        TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

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
        for _ in 0..3 {
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
            if result == ERROR_INSUFFICIENT_BUFFER {
                continue;
            }
            if result != 0 || table.is_empty() {
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
                let matches = if family == AF_INET {
                    let row = unsafe { row.cast::<MIB_TCPROW_OWNER_PID>().read_unaligned() };
                    row.dwOwningPid == pid
                        && u16::from_be(row.dwLocalPort as u16) == port
                        && (row.dwLocalAddr == 0 || row.dwLocalAddr.to_ne_bytes() == [127, 0, 0, 1])
                } else {
                    let row = unsafe { row.cast::<MIB_TCP6ROW_OWNER_PID>().read_unaligned() };
                    row.dwOwningPid == pid
                        && u16::from_be(row.dwLocalPort as u16) == port
                        && (row.ucLocalAddr == [0; 16]
                            || row.ucLocalAddr == std::net::Ipv6Addr::LOCALHOST.octets())
                };
                if matches {
                    return true;
                }
            }
            break;
        }
    }
    false
}

#[cfg(target_os = "linux")]
pub fn owns_listener(pid: u32, port: u16) -> bool {
    use std::collections::HashSet;
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    let sockets: HashSet<String> = entries
        .flatten()
        .filter_map(|entry| {
            let link = std::fs::read_link(entry.path()).ok()?;
            let link = link.to_str()?;
            link.strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_string)
        })
        .collect();
    for file in ["tcp", "tcp6"] {
        let Ok(table) = std::fs::read_to_string(format!("/proc/{pid}/net/{file}")) else {
            continue;
        };
        for row in table.lines().skip(1) {
            let columns: Vec<_> = row.split_whitespace().collect();
            if columns.len() > 9
                && columns[3] == "0A"
                && columns[1]
                    .rsplit_once(':')
                    .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                    == Some(port)
                && sockets.contains(columns[9])
            {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "macos")]
pub fn owns_listener(pid: u32, port: u16) -> bool {
    std::process::Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
            "-Fp",
        ])
        .output()
        .ok()
        .is_some_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line == format!("p{pid}"))
        })
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn owns_listener(_pid: u32, _port: u16) -> bool {
    false
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    fn recognizes_only_the_process_owning_a_listener() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(super::owns_listener(std::process::id(), port));
        assert!(!super::owns_listener(u32::MAX, port));
    }
}
