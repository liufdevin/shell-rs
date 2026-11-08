use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{Core, DISCOVERY_PORT, Device, clean_name, fingerprint, is_lan};

const MAGIC: &str = "shelllf-lan-v1";

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
    discover_at(&[
        SocketAddr::new(Ipv4Addr::BROADCAST.into(), DISCOVERY_PORT),
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), DISCOVERY_PORT),
    ])
}

pub fn discover_at(targets: &[SocketAddr]) -> Result<Vec<Device>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(150)))?;
    let nonce = rand::random();
    let bytes = serde_json::to_vec(&Query {
        magic: MAGIC.into(),
        nonce,
    })?;
    for target in targets {
        let _ = socket.send_to(&bytes, target);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut found = Vec::<Device>::new();
    let mut buffer = [0u8; 2048];
    while Instant::now() < deadline && found.len() < 64 {
        let Ok((count, source)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        if !is_lan(source.ip()) {
            continue;
        }
        let Ok(reply) = serde_json::from_slice::<Advertisement>(&buffer[..count]) else {
            continue;
        };
        if reply.magic != MAGIC || reply.nonce != nonce || reply.port == 0 || reply.key.len() > 1024
        {
            continue;
        }
        let Ok(key) = russh::keys::PublicKey::from_openssh(&reply.key) else {
            continue;
        };
        let id = fingerprint(&key);
        if found.iter().any(|device| device.id == id) {
            continue;
        }
        found.push(Device {
            id,
            name: clean_name(&reply.name),
            endpoint: SocketAddr::new(source.ip(), reply.port),
            public_key: reply.key,
        });
    }
    Ok(found)
}

pub(super) fn respond(core: Arc<Core>, socket: UdpSocket, port: u16, stop: Arc<AtomicBool>) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buffer = [0u8; 256];
    let mut last = Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Acquire) {
        let Ok((count, source)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        if !is_lan(source.ip())
            || !core.is_active()
            || core.is_blocked(source.ip())
            || last.elapsed() < Duration::from_millis(100)
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
        last = Instant::now();
    }
}
