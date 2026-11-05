//! Read-only Windows network snapshots use IP Helper in the worker's compartment.
use super::NetworkBaseline;
use crate::{Error, Result, mesh::coexist::MeshRoute};
use std::{collections::HashMap, net::IpAddr, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR},
    NetworkManagement::IpHelper::*,
    Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6},
};

/// Both IP Helper tables use Winsock addresses in network byte order.
unsafe fn ip_address(addr: *const SOCKADDR) -> Result<IpAddr> {
    // The family identifies the actual sockaddr allocation.
    unsafe {
        match addr.as_ref().map(|a| a.sa_family) {
            Some(AF_INET) => Ok((*addr.cast::<SOCKADDR_IN>())
                .sin_addr
                .S_un
                .S_addr
                .to_ne_bytes()
                .into()),
            Some(AF_INET6) => Ok((*addr.cast::<SOCKADDR_IN6>()).sin6_addr.u.Byte.into()),
            _ => Err(Error::other("Windows 返回了无效的接口地址，请重试")),
        }
    }
}

fn read_adapters(baseline: &mut NetworkBaseline, proxy_addresses: &[IpAddr]) -> Result<()> {
    // Windows recommends a 15 KiB initial buffer. Adapter churn can grow it.
    let mut bytes = 15 * 1024u32;
    for _ in 0..4 {
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        let mut next = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let result = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC.into(),
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST,
                ptr::null(),
                next,
                &mut bytes,
            )
        };
        match result {
            ERROR_BUFFER_OVERFLOW => continue,
            ERROR_NO_DATA => return Ok(()),
            NO_ERROR => {}
            _ => {
                return Err(Error::other(format!(
                    "无法读取 Windows 网卡和 DNS（错误 {result}），请检查网络服务"
                )));
            }
        }
        // All list nodes and strings are owned by buffer until traversal finishes.
        while let Some(adapter) = unsafe { next.as_ref() } {
            next = adapter.Next;
            let wide = |p: *const u16| {
                if p.is_null() {
                    String::new()
                } else {
                    unsafe { widestring::U16CStr::from_ptr_str(p) }.to_string_lossy()
                }
            };
            if wide(adapter.FriendlyName)
                .to_ascii_lowercase()
                .starts_with("xross-mesh-")
            {
                continue;
            }
            let tun = wide(adapter.Description)
                .to_ascii_lowercase()
                .starts_with("sing-tun tunnel");
            let v4_index = unsafe { adapter.Anonymous1.Anonymous.IfIndex };
            let v6_index = adapter.Ipv6IfIndex;
            let mut addr = adapter.FirstUnicastAddress;
            while let Some(unicast) = unsafe { addr.as_ref() } {
                let ip = unsafe { ip_address(unicast.Address.lpSockaddr) }?;
                if adapter.IfType != IF_TYPE_SOFTWARE_LOOPBACK {
                    baseline
                        .protected
                        .push(format!("{ip}/{}", unicast.OnLinkPrefixLength));
                }
                if tun && proxy_addresses.contains(&ip) {
                    baseline
                        .proxy_indices
                        .extend([v4_index, v6_index].into_iter().filter(|&i| i != 0));
                }
                addr = unicast.Next;
            }
            let mut v4 = Vec::new();
            let mut v6 = Vec::new();
            let mut addr = adapter.FirstDnsServerAddress;
            while let Some(server) = unsafe { addr.as_ref() } {
                let ip = unsafe { ip_address(server.Address.lpSockaddr) }?;
                if ip.is_ipv4() { &mut v4 } else { &mut v6 }.push(ip.to_string());
                addr = server.Next;
            }
            for (index, family, servers) in [(v4_index, AF_INET, v4), (v6_index, AF_INET6, v6)] {
                if index != 0 {
                    baseline
                        .dns
                        .push(format!("{index}|{family}|{}", servers.join(",")));
                }
            }
        }
        return Ok(());
    }
    Err(Error::other("Windows 网卡列表持续变化，请稍后重试"))
}

fn route_rows() -> Result<Vec<MIB_IPFORWARD_ROW2>> {
    let mut table = ptr::null_mut();
    let result = unsafe { GetIpForwardTable2(AF_UNSPEC, &mut table) };
    if result != NO_ERROR {
        return Err(Error::other(format!(
            "无法读取 Windows 路由表（错误 {result}），请检查网络服务"
        )));
    }
    // The API allocates a variable-length table, with NumEntries trailing rows.
    unsafe {
        let rows = std::slice::from_raw_parts(
            ptr::addr_of!((*table).Table).cast::<MIB_IPFORWARD_ROW2>(),
            (*table).NumEntries as usize,
        )
        .to_vec();
        FreeMibTable(table.cast());
        Ok(rows)
    }
}

fn route_prefix(row: &MIB_IPFORWARD_ROW2) -> Result<String> {
    let ip = unsafe { ip_address(ptr::addr_of!(row.DestinationPrefix.Prefix).cast()) }?;
    Ok(format!("{ip}/{}", row.DestinationPrefix.PrefixLength))
}

fn interface_alias(row: &MIB_IPFORWARD_ROW2) -> Result<String> {
    let mut alias = [0u16; 257]; // IF_MAX_STRING_SIZE + the terminating NUL.
    let result =
        unsafe { ConvertInterfaceLuidToAlias(&row.InterfaceLuid, alias.as_mut_ptr(), alias.len()) };
    if result != NO_ERROR {
        return Err(Error::other(format!(
            "无法读取 Windows 路由接口（错误 {result}），请重试"
        )));
    }
    Ok(String::from_utf16_lossy(
        &alias[..alias.iter().position(|&c| c == 0).unwrap_or(alias.len())],
    ))
}

pub fn baseline_with_proxy_tun(addresses: &[String]) -> Result<NetworkBaseline> {
    let ips: Vec<IpAddr> = addresses
        .iter()
        .map(|s| s.parse())
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::config("代理 TUN 地址无效"))?;
    let mut baseline = NetworkBaseline {
        proxy_tun_addresses: ips.iter().map(ToString::to_string).collect(),
        ..Default::default()
    };
    read_adapters(&mut baseline, &ips)?;
    let mut aliases = HashMap::new();
    for row in route_rows()? {
        let interface = match aliases.entry(row.InterfaceIndex) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(interface_alias(&row)?)
            }
        };
        if row.DestinationPrefix.PrefixLength == 0 {
            let hop = unsafe { ip_address(ptr::addr_of!(row.NextHop).cast()) }?;
            baseline.defaults.push(format!(
                "{}|{hop}|{}",
                route_prefix(&row)?,
                row.InterfaceIndex
            ));
        }
        if interface.to_ascii_lowercase().starts_with("xross-mesh-") {
            baseline.mesh_routes.push(MeshRoute {
                cidr: route_prefix(&row)?,
                interface: interface.clone(),
            });
        }
    }
    baseline.defaults.sort();
    baseline.dns.sort();
    baseline.exclude_proxy_interfaces();
    Ok(baseline)
}

pub fn interface_routes(interface: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for row in route_rows()? {
        if interface_alias(&row)?.eq_ignore_ascii_case(interface) {
            result.push(route_prefix(&row)?);
        }
    }
    result.sort();
    result.dedup();
    Ok(result)
}
