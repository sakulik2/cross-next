//! 挑一个内网 IPv4 来绑定。
//!
//! 刻意不绑 `0.0.0.0`：那会连公网网卡、VPN、WSL 虚拟网卡一起监听。绑到具体的
//! 内网地址上，是这个"共享 token + 只绑内网"防护策略里"只绑内网"的那一半。

use std::net::Ipv4Addr;
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
    GET_ADAPTERS_ADDRESSES_FLAGS, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

/// 枚举所有已启用网卡上的私网 IPv4 地址。
///
/// **有默认网关的网卡排在前面。** 这一条很关键：WSL2 和 Hyper-V 的虚拟网卡用
/// `172.x` 段，按 RFC1918 判断也是"私网地址"，但那是宿主机内部的虚拟网段，
/// 局域网里的其它机器没有到它的路由，绑上去别的设备根本连不上。虚拟网卡通常
/// 没有默认网关，真正连着路由器的那张有，所以拿它当判据。
pub fn private_ipv4s() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    // 只要单播，跳过 anycast/multicast/DNS，减少要遍历的数据。
    let flags = GET_ADAPTERS_ADDRESSES_FLAGS(
        GAA_FLAG_SKIP_ANYCAST.0 | GAA_FLAG_SKIP_MULTICAST.0 | GAA_FLAG_SKIP_DNS_SERVER.0,
    );

    // 标准两步调用：先问要多大缓冲，再实际取。网卡可能在两次调用之间变化，
    // 所以 ERROR_BUFFER_OVERFLOW 时重试几次。
    let mut size: u32 = 16 * 1024;
    for _ in 0..3 {
        let mut buf = vec![0u8; size as usize];
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            )
        };

        const ERROR_SUCCESS: u32 = 0;
        const ERROR_BUFFER_OVERFLOW: u32 = 111;

        match ret {
            ERROR_SUCCESS => {
                unsafe { collect(buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH, &mut out) };
                break;
            }
            // size 已被写成所需大小，下一轮用它重试。
            ERROR_BUFFER_OVERFLOW => continue,
            _ => break,
        }
    }

    out
}

/// 顺着链表收集私网 IPv4，有默认网关的网卡优先。
unsafe fn collect(mut adapter: *const IP_ADAPTER_ADDRESSES_LH, out: &mut Vec<Ipv4Addr>) {
    // 分两组收集，最后拼接。有网关的是真正连着路由器的那张网卡。
    let mut routed = Vec::new();
    let mut isolated = Vec::new();

    while !adapter.is_null() {
        let a = unsafe { &*adapter };

        // IfOperStatusUp == 1。只要已启用的网卡。
        if a.OperStatus.0 == 1 {
            let has_gateway = !a.FirstGatewayAddress.is_null();
            let bucket = if has_gateway {
                &mut routed
            } else {
                &mut isolated
            };

            let mut unicast = a.FirstUnicastAddress;
            while !unicast.is_null() {
                let u = unsafe { &*unicast };
                let sockaddr = u.Address.lpSockaddr;

                if !sockaddr.is_null() && unsafe { (*sockaddr).sa_family } == AF_INET {
                    let sin = sockaddr as *const SOCKADDR_IN;
                    // S_un_b 是网络字节序的四个八位组，直接按顺序取即可。
                    let octets = unsafe { (*sin).sin_addr.S_un.S_un_b };
                    let ip = Ipv4Addr::new(octets.s_b1, octets.s_b2, octets.s_b3, octets.s_b4);

                    if is_private(&ip) {
                        bucket.push(ip);
                    }
                }

                unicast = u.Next;
            }
        }

        adapter = a.Next;
    }

    out.extend(routed);
    out.extend(isolated);
}

/// RFC1918 私网地址。刻意排除环回和 169.254 链路本地。
fn is_private(ip: &Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    match a {
        10 => true,
        172 => (16..=31).contains(&b),
        192 => b == 168,
        _ => false,
    }
}
