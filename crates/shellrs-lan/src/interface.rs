use std::{
    io,
    net::{IpAddr, Ipv6Addr},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub(super) struct Interface {
    pub index: u32,
    pub name: String,
    pub addresses: Vec<(IpAddr, u8)>,
}

pub(super) fn interfaces() -> Arc<Vec<Interface>> {
    type Snapshot = (Instant, Arc<Vec<Interface>>);
    static CACHE: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| {
            Mutex::new((
                Instant::now() - Duration::from_secs(10),
                Arc::new(Vec::new()),
            ))
        })
        .lock()
        .unwrap();
    if cache.0.elapsed() >= Duration::from_secs(5) {
        cache.1 = Arc::new(enumerate().unwrap_or_default());
        cache.0 = Instant::now();
    }
    cache.1.clone()
}

pub(super) fn on_link(address: Ipv6Addr, local: Ipv6Addr, prefix: u8) -> bool {
    if !(1..=128).contains(&prefix)
        || address.segments()[0] & 0xe000 != 0x2000
        || local.segments()[0] & 0xe000 != 0x2000
    {
        return false;
    }
    let mask = u128::MAX << (128 - u32::from(prefix));
    u128::from(address) & mask == u128::from(local) & mask
}

#[cfg(windows)]
fn enumerate() -> io::Result<Vec<Interface>> {
    use windows_sys::Win32::{
        Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, NO_ERROR},
        NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6},
    };
    let mut size = 16384u32;
    for _ in 0..3 {
        if size > 1024 * 1024 {
            return Err(io::Error::other("interface inventory too large"));
        }
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let first = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let status = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                std::ptr::null(),
                first,
                &mut size,
            )
        };
        if status == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if status == ERROR_NO_DATA {
            return Ok(Vec::new());
        }
        if status != NO_ERROR {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let mut result = Vec::new();
        let mut current = first;
        while !current.is_null() && result.len() < 256 {
            let adapter = unsafe { &*current };
            if adapter.OperStatus == IfOperStatusUp && adapter.Ipv6IfIndex != 0 {
                let mut name = Vec::new();
                if !adapter.FriendlyName.is_null() {
                    for offset in 0..1024 {
                        let character = unsafe { *adapter.FriendlyName.add(offset) };
                        if character == 0 {
                            break;
                        }
                        name.push(character);
                    }
                }
                let mut interface = Interface {
                    index: adapter.Ipv6IfIndex,
                    name: String::from_utf16_lossy(&name),
                    addresses: Vec::new(),
                };
                let mut unicast = adapter.FirstUnicastAddress;
                while !unicast.is_null() && interface.addresses.len() < 256 {
                    let address = unsafe { &*unicast };
                    let socket = address.Address.lpSockaddr;
                    if !socket.is_null() {
                        let family = unsafe { (*socket).sa_family };
                        if family == AF_INET6
                            && address.Address.iSockaddrLength
                                >= std::mem::size_of::<SOCKADDR_IN6>() as i32
                        {
                            let value = unsafe { &*socket.cast::<SOCKADDR_IN6>() };
                            interface.addresses.push((
                                IpAddr::V6(Ipv6Addr::from(unsafe { value.sin6_addr.u.Byte })),
                                address.OnLinkPrefixLength,
                            ));
                        } else if family == AF_INET
                            && address.Address.iSockaddrLength
                                >= std::mem::size_of::<SOCKADDR_IN>() as i32
                        {
                            let value = unsafe { &*socket.cast::<SOCKADDR_IN>() };
                            interface.addresses.push((
                                IpAddr::V4(std::net::Ipv4Addr::from(
                                    unsafe { value.sin_addr.S_un.S_addr }.to_ne_bytes(),
                                )),
                                address.OnLinkPrefixLength,
                            ));
                        }
                    }
                    unicast = address.Next;
                }
                result.push(interface);
            }
            current = adapter.Next;
        }
        return Ok(result);
    }
    Err(io::Error::other("interface inventory changed repeatedly"))
}

#[cfg(unix)]
fn enumerate() -> io::Result<Vec<Interface>> {
    let mut first = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut first) } != 0 {
        return Err(io::Error::last_os_error());
    }
    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) };
        }
    }
    let _guard = Addresses(first);
    let mut result = Vec::<Interface>::new();
    let mut current = first;
    let mut count = 0;
    while !current.is_null() && count < 4096 {
        count += 1;
        let entry = unsafe { &*current };
        if entry.ifa_flags & libc::IFF_UP as u32 != 0
            && !entry.ifa_addr.is_null()
            && !entry.ifa_netmask.is_null()
        {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let index = unsafe { libc::if_nametoindex(entry.ifa_name) };
            let family = unsafe { (*entry.ifa_addr).sa_family as i32 };
            let value = if family == libc::AF_INET6 {
                let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in6>() };
                let mask = unsafe { &*entry.ifa_netmask.cast::<libc::sockaddr_in6>() };
                Some((
                    IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr)),
                    mask.sin6_addr
                        .s6_addr
                        .iter()
                        .map(|byte| byte.count_ones() as u8)
                        .sum(),
                ))
            } else if family == libc::AF_INET {
                let address = unsafe { &*entry.ifa_addr.cast::<libc::sockaddr_in>() };
                let mask = unsafe { &*entry.ifa_netmask.cast::<libc::sockaddr_in>() };
                Some((
                    IpAddr::V4(std::net::Ipv4Addr::from(
                        address.sin_addr.s_addr.to_ne_bytes(),
                    )),
                    mask.sin_addr.s_addr.count_ones() as u8,
                ))
            } else {
                None
            };
            if let Some(value) = value
                && index != 0
            {
                if let Some(interface) =
                    result.iter_mut().find(|interface| interface.index == index)
                {
                    if interface.addresses.len() < 256 {
                        interface.addresses.push(value);
                    }
                } else if result.len() < 256 {
                    result.push(Interface {
                        index,
                        name,
                        addresses: vec![value],
                    });
                }
            }
        }
        current = entry.ifa_next;
    }
    Ok(result)
}

#[cfg(not(any(windows, unix)))]
fn enumerate() -> io::Result<Vec<Interface>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn global_peers_must_share_an_actual_on_link_prefix() {
        let local = "2408:1234:5678:9abc::1".parse().unwrap();
        assert!(on_link(
            "2408:1234:5678:9abc::2".parse().unwrap(),
            local,
            64
        ));
        assert!(!on_link(
            "2408:1234:5678:9abd::2".parse().unwrap(),
            local,
            64
        ));
        assert!(!on_link("2606:4700:4700::1111".parse().unwrap(), local, 64));
        assert!(!on_link(local, local, 0));
        assert!(on_link(local, local, 128));
        assert!(!on_link("::1".parse().unwrap(), local, 64));
    }
    #[test]
    fn interface_inventory_is_bounded_and_has_valid_indices() {
        let found = enumerate().unwrap();
        assert!(found.len() <= 256);
        assert!(found.iter().all(|interface| interface.index > 0));
    }
}
