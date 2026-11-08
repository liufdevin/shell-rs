use std::net::{IpAddr, Ipv6Addr, SocketAddr, SocketAddrV6};

use anyhow::{Result, ensure};

pub(super) fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(address.into()),
        _ => address,
    }
}

pub(super) fn normalize_endpoint(address: SocketAddr) -> SocketAddr {
    if let SocketAddr::V6(value) = address
        && let Some(ip) = value.ip().to_ipv4_mapped()
    {
        return SocketAddr::new(ip.into(), value.port());
    }
    address
}

fn supported_ip(address: IpAddr) -> bool {
    match normalize_ip(address) {
        IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unicast_link_local()
                || address.is_unique_local()
                || address.segments()[0] & 0xe000 == 0x2000
        }
    }
}

pub(super) fn valid_stored_endpoint(address: SocketAddr) -> bool {
    supported_ip(address.ip())
        && address.port() != 0
        && (!matches!(address, SocketAddr::V6(value) if value.ip().is_unicast_link_local() && value.scope_id() == 0))
}

pub(super) fn is_lan(address: IpAddr) -> bool {
    match normalize_ip(address) {
        IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            if address.is_loopback() || address.is_unicast_link_local() || address.is_unique_local()
            {
                return true;
            }
            crate::interface::interfaces().iter().any(|interface| interface.addresses.iter().any(|(local, prefix)| {
                matches!(local, IpAddr::V6(local) if crate::interface::on_link(address, *local, *prefix))
            }))
        }
    }
}

fn scoped_address(text: &str) -> Result<SocketAddr> {
    let (address, zone) = text
        .rsplit_once('%')
        .ok_or_else(|| anyhow::anyhow!("invalid_device_address"))?;
    let address: Ipv6Addr = address.parse()?;
    let index = zone
        .parse::<u32>()
        .ok()
        .filter(|index| *index != 0)
        .or_else(|| {
            crate::interface::interfaces()
                .iter()
                .find(|interface| interface.name.eq_ignore_ascii_case(zone))
                .map(|interface| interface.index)
        })
        .ok_or_else(|| anyhow::anyhow!("invalid_network_interface"))?;
    Ok(SocketAddr::V6(SocketAddrV6::new(
        address,
        crate::TCP_PORT,
        0,
        index,
    )))
}

pub fn parse_endpoint(text: &str) -> Result<SocketAddr> {
    let text = text.trim();
    let endpoint = if let Ok(endpoint) = text.parse::<SocketAddr>() {
        endpoint
    } else if let Some(rest) = text.strip_prefix('[') {
        let (host, suffix) = rest
            .split_once(']')
            .ok_or_else(|| anyhow::anyhow!("invalid_device_address"))?;
        let port = if suffix.is_empty() {
            crate::TCP_PORT
        } else {
            suffix
                .strip_prefix(':')
                .ok_or_else(|| anyhow::anyhow!("invalid_device_address"))?
                .parse()?
        };
        let mut endpoint = if host.contains('%') {
            scoped_address(host)?
        } else {
            SocketAddr::new(IpAddr::V6(host.parse()?), crate::TCP_PORT)
        };
        endpoint.set_port(port);
        endpoint
    } else if text.contains('%') {
        scoped_address(text)?
    } else {
        SocketAddr::new(text.parse::<IpAddr>()?, crate::TCP_PORT)
    };
    let endpoint = normalize_endpoint(endpoint);
    ensure!(
        valid_stored_endpoint(endpoint) && is_lan(endpoint.ip()),
        "use a local network address with a valid interface"
    );
    Ok(endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_addresses_accept_scopes_brackets_defaults_and_mapped_addresses() {
        for (input, expected) in [
            ("::1", "[::1]:45838"),
            ("[::1]", "[::1]:45838"),
            ("[::1]:12345", "[::1]:12345"),
            ("fd12:3456::abcd", "[fd12:3456::abcd]:45838"),
            ("fe80::abcd%17", "[fe80::abcd%17]:45838"),
            ("[fe80::abcd%17]:12345", "[fe80::abcd%17]:12345"),
            ("[::ffff:192.168.1.2]:12345", "192.168.1.2:12345"),
            ("192.168.1.2", "192.168.1.2:45838"),
        ] {
            assert_eq!(
                parse_endpoint(input).unwrap(),
                expected.parse::<SocketAddr>().unwrap()
            );
        }
        for input in [
            "::",
            "[::1]:0",
            "[ff02::1]:45838",
            "fe80::1",
            "[fe80::1%0]:45838",
            "[fe80::1%999999999999]:45838",
            "8.8.8.8",
            "[::ffff:8.8.8.8]:45838",
            "[fd00::1]:99999",
        ] {
            assert!(parse_endpoint(input).is_err(), "{input}");
        }
    }
    #[test]
    fn stored_global_addresses_do_not_require_the_original_interface_to_be_online() {
        let address: SocketAddr = "[2606:4700:4700::1111]:45838".parse().unwrap();
        assert!(valid_stored_endpoint(address));
        assert!(!valid_stored_endpoint("[::]:45838".parse().unwrap()));
        assert!(!valid_stored_endpoint("[ff02::1]:45838".parse().unwrap()));
        assert!(!valid_stored_endpoint("[fe80::1]:45838".parse().unwrap()));
        assert_eq!(
            normalize_ip("::ffff:192.168.1.2".parse().unwrap()),
            "192.168.1.2".parse::<IpAddr>().unwrap()
        );
    }
}
