use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6, UdpSocket};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{Core, DISCOVERY_PORT, Device, clean_name, fingerprint, is_lan};

const MAGIC: &str = "shelllf-lan-v1";
const MULTICAST: Ipv6Addr = Ipv6Addr::new(0xff12, 0, 0, 0, 0, 0, 0x534c, 0x4c46);

fn multicast_interface(interface: &crate::interface::Interface) -> bool {
    interface.addresses.iter().any(|(address, _)| matches!(address, IpAddr::V6(address) if !address.is_loopback() && !address.is_unspecified() && !address.is_multicast()))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    magic: String,
    nonce: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Advertisement {
    magic: String,
    nonce: u64,
    name: String,
    port: u16,
    key: String,
}

pub fn discover() -> Result<Vec<Device>> {
    let mut targets = vec![
        SocketAddr::new(Ipv4Addr::BROADCAST.into(), DISCOVERY_PORT),
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), DISCOVERY_PORT),
        SocketAddr::new(Ipv6Addr::LOCALHOST.into(), DISCOVERY_PORT),
    ];
    for interface in crate::interface::interfaces().iter() {
        if multicast_interface(interface) {
            targets.push(SocketAddr::V6(SocketAddrV6::new(
                MULTICAST,
                DISCOVERY_PORT,
                0,
                interface.index,
            )));
        }
        for (address, prefix) in &interface.addresses {
            if let IpAddr::V4(address) = address
                && (1..=30).contains(prefix)
                && !address.is_loopback()
                && is_lan((*address).into())
            {
                targets.push(SocketAddr::new(
                    Ipv4Addr::from(u32::from(*address) | (u32::MAX >> u32::from(*prefix))).into(),
                    DISCOVERY_PORT,
                ));
            }
        }
    }
    targets.sort();
    targets.dedup();
    discover_at(&targets)
}

pub fn discover_at(targets: &[SocketAddr]) -> Result<Vec<Device>> {
    let mut sockets = Vec::new();
    for address in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ] {
        if !targets
            .iter()
            .any(|target| target.is_ipv4() == address.is_ipv4())
        {
            continue;
        }
        let socket = match crate::transport::udp_socket(SocketAddr::new(address, 0)) {
            Ok(socket) => socket,
            Err(error) if crate::transport::family_unavailable(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if address.is_ipv4() {
            socket.set_broadcast(true)?;
        }
        socket.set_nonblocking(true)?;
        sockets.push(socket);
    }
    ensure!(!sockets.is_empty(), "local_network_unavailable");
    let nonce = rand::random();
    let bytes = serde_json::to_vec(&Query {
        magic: MAGIC.into(),
        nonce,
    })?;
    send_queries(&sockets, targets, &bytes);
    let mut last_sent = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found = Vec::<Device>::new();
    let mut buffer = [0u8; 2048];
    while Instant::now() < deadline && found.len() < 64 {
        if last_sent.elapsed() >= Duration::from_millis(500) {
            send_queries(&sockets, targets, &bytes);
            last_sent = Instant::now();
        }
        for socket in &sockets {
            for _ in 0..16 {
                let Ok((count, source)) = socket.recv_from(&mut buffer) else {
                    break;
                };
                if let Some(device) = advertisement(&buffer[..count], source, nonce)
                    && !found.iter().any(|known| known.id == device.id)
                    && found.len() < 64
                {
                    found.push(device);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(found)
}

fn send_queries(sockets: &[UdpSocket], targets: &[SocketAddr], bytes: &[u8]) {
    for target in targets {
        if let Some(socket) = sockets.iter().find(|socket| {
            socket
                .local_addr()
                .is_ok_and(|address| address.is_ipv4() == target.is_ipv4())
        }) {
            let _ = socket.send_to(bytes, target);
        }
    }
}

fn advertisement(bytes: &[u8], source: SocketAddr, nonce: u64) -> Option<Device> {
    if !is_lan(source.ip()) {
        return None;
    }
    let reply: Advertisement = serde_json::from_slice(bytes).ok()?;
    if reply.magic != MAGIC || reply.nonce != nonce || reply.port == 0 || reply.key.len() > 1024 {
        return None;
    }
    let key = russh::keys::PublicKey::from_openssh(&reply.key).ok()?;
    let mut endpoint = crate::address::normalize_endpoint(source);
    endpoint.set_port(reply.port);
    if !crate::address::valid_stored_endpoint(endpoint) {
        return None;
    }
    Some(Device {
        id: fingerprint(&key),
        name: clean_name(&reply.name),
        endpoint,
        public_key: reply.key,
    })
}

fn join_interfaces(socket: &UdpSocket, joined: &mut BTreeSet<u32>) {
    if !socket
        .local_addr()
        .is_ok_and(|address| address.is_ipv6() && address.ip().is_unspecified())
    {
        return;
    }
    let available: BTreeSet<_> = crate::interface::interfaces()
        .iter()
        .filter(|interface| multicast_interface(interface))
        .map(|interface| interface.index)
        .collect();
    for index in joined.difference(&available) {
        let _ = socket.leave_multicast_v6(&MULTICAST, *index);
    }
    joined.retain(|index| available.contains(index));
    for index in available {
        if !joined.contains(&index) && socket.join_multicast_v6(&MULTICAST, index).is_ok() {
            joined.insert(index);
        }
    }
}

pub(super) fn respond(core: Arc<Core>, sockets: Vec<UdpSocket>, port: u16, stop: Arc<AtomicBool>) {
    for socket in &sockets {
        if socket.set_nonblocking(true).is_err() {
            return;
        }
    }
    let mut joined: Vec<BTreeSet<u32>> = sockets.iter().map(|_| BTreeSet::new()).collect();
    let mut refreshed = Instant::now() - Duration::from_secs(10);
    let mut buffer = [0u8; 256];
    let mut last = BTreeMap::<(IpAddr, u32), Instant>::new();
    let mut budget = (Instant::now(), 0u32);
    while !stop.load(Ordering::Acquire) {
        if refreshed.elapsed() >= Duration::from_secs(5) {
            for (socket, joined) in sockets.iter().zip(&mut joined) {
                join_interfaces(socket, joined);
            }
            refreshed = Instant::now();
        }
        if budget.0.elapsed() >= Duration::from_secs(1) {
            budget = (Instant::now(), 0);
            last.retain(|_, time| time.elapsed() < Duration::from_secs(1));
        }
        for socket in &sockets {
            for _ in 0..16 {
                let Ok((count, source)) = socket.recv_from(&mut buffer) else {
                    break;
                };
                let source_key = (
                    source.ip(),
                    match source {
                        SocketAddr::V6(address) => address.scope_id(),
                        SocketAddr::V4(_) => 0,
                    },
                );
                if !is_lan(source.ip())
                    || !core.is_active()
                    || !core.is_discoverable()
                    || core.is_blocked(source.ip())
                    || last
                        .get(&source_key)
                        .is_some_and(|time| time.elapsed() < Duration::from_millis(100))
                    || last.len() >= 256
                    || budget.1 >= 64
                {
                    continue;
                }
                let Ok(query) = serde_json::from_slice::<Query>(&buffer[..count]) else {
                    continue;
                };
                if query.magic != MAGIC {
                    continue;
                }
                let Ok(key) = core.key.public_key().to_openssh() else {
                    continue;
                };
                let advertisement = Advertisement {
                    magic: MAGIC.into(),
                    nonce: query.nonce,
                    name: core.name.clone(),
                    port,
                    key,
                };
                if let Ok(bytes) = serde_json::to_vec(&advertisement) {
                    let _ = socket.send_to(&bytes, source);
                }
                last.insert(source_key, Instant::now());
                budget.1 += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advertisements_keep_the_receiving_interfaces_scope_and_validate_the_nonce() {
        let key =
            russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
                .unwrap();
        let bytes = serde_json::to_vec(&Advertisement {
            magic: MAGIC.into(),
            nonce: 123,
            name: "peer".into(),
            port: 45838,
            key: key.public_key().to_openssh().unwrap(),
        })
        .unwrap();
        let source = SocketAddr::V6(SocketAddrV6::new(
            "fe80::1234".parse().unwrap(),
            12345,
            0,
            17,
        ));
        let found = advertisement(&bytes, source, 123).unwrap();
        assert_eq!(found.endpoint, "[fe80::1234%17]:45838".parse().unwrap());
        assert!(advertisement(&bytes, source, 124).is_none());
        assert!(advertisement(&bytes, "[fe80::1234]:12345".parse().unwrap(), 123).is_none());
    }
}
