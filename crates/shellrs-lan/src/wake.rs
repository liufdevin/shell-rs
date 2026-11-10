use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::{Core, Device, RemoteRequest, Response};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeInterface {
    mac: [u8; 6],
    address: Ipv4Addr,
    prefix: u8,
}

impl WakeInterface {
    fn is_valid(&self) -> bool {
        self.mac != [0; 6]
            && self.mac[0] & 1 == 0
            && (1..=30).contains(&self.prefix)
            && !self.address.is_loopback()
            && crate::address::is_lan(self.address.into())
    }
}

pub(super) fn validate(interfaces: &[WakeInterface]) -> Result<()> {
    ensure!(
        interfaces.len() <= 16 && interfaces.iter().all(WakeInterface::is_valid),
        "invalid_wake_interfaces"
    );
    Ok(())
}

pub(super) fn local_interfaces() -> Vec<WakeInterface> {
    crate::interface::interfaces()
        .iter()
        .flat_map(|interface| {
            interface.addresses.iter().filter_map(|(address, prefix)| {
                let IpAddr::V4(address) = address else {
                    return None;
                };
                let value = WakeInterface {
                    mac: interface.mac?,
                    address: *address,
                    prefix: *prefix,
                };
                value.is_valid().then_some(value)
            })
        })
        .take(16)
        .collect()
}

fn packet(mac: [u8; 6]) -> [u8; 102] {
    let mut bytes = [0xff; 102];
    for chunk in bytes[6..].as_chunks_mut::<6>().0 {
        chunk.copy_from_slice(&mac);
    }
    bytes
}

fn same_subnet(local: Ipv4Addr, remote: Ipv4Addr, prefix: u8) -> bool {
    if !(1..=30).contains(&prefix) {
        return false;
    }
    let mask = u32::MAX << (32 - prefix);
    u32::from(local) & mask == u32::from(remote) & mask
}

fn send(interfaces: &[WakeInterface]) -> Result<()> {
    validate(interfaces)?;
    let mut sent = false;
    for local in crate::interface::interfaces().iter() {
        for (address, prefix) in &local.addresses {
            let IpAddr::V4(address) = address else {
                continue;
            };
            for remote in interfaces {
                if !same_subnet(*address, remote.address, *prefix)
                    || !same_subnet(*address, remote.address, remote.prefix)
                {
                    continue;
                }
                let Ok(socket) = UdpSocket::bind((*address, 0)) else {
                    continue;
                };
                if socket.set_broadcast(true).is_err() {
                    continue;
                }
                let broadcast =
                    Ipv4Addr::from(u32::from(*address) | (u32::MAX >> u32::from(*prefix)));
                let bytes = packet(remote.mac);
                for _ in 0..3 {
                    sent |= socket.send_to(&bytes, (broadcast, 9)).is_ok();
                }
            }
        }
    }
    ensure!(sent, "wake_network_unavailable");
    Ok(())
}

impl Core {
    pub fn has_wake_info(&self, peer: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .configuration
            .wake_peers
            .get(peer)
            .is_some_and(|interfaces| !interfaces.is_empty())
    }

    pub fn is_waking(&self) -> bool {
        !self.waking.lock().unwrap().is_empty()
    }

    pub(super) fn refresh_wake(&self, device: &Device, cancelled: &AtomicBool) -> Result<()> {
        let response = crate::transport::request(
            self,
            device,
            RemoteRequest::WakeInfo,
            cancelled,
            &mut |_, _| Ok(()),
        );
        let Ok(Response {
            wake_interfaces, ..
        }) = response
        else {
            return Ok(());
        };
        validate(&wake_interfaces)?;
        let mut state = self.state.lock().unwrap();
        if !state.configuration.peers.contains_key(&device.id)
            || state.configuration.wake_peers.get(&device.id) == Some(&wake_interfaces)
        {
            return Ok(());
        }
        let mut configuration = state.configuration.clone();
        configuration
            .wake_peers
            .insert(device.id.clone(), wake_interfaces);
        self.persist(&configuration)?;
        state.configuration = configuration;
        Ok(())
    }
}

struct Waking<'a> {
    core: &'a Core,
    peer: &'a str,
}

impl Drop for Waking<'_> {
    fn drop(&mut self) {
        let mut waking = self.core.waking.lock().unwrap();
        if let Some(count) = waking.get_mut(self.peer) {
            *count -= 1;
            if *count == 0 {
                waking.remove(self.peer);
            }
        }
        self.core.revision.fetch_add(1, Ordering::Release);
    }
}

pub(super) async fn connect(
    core: &Core,
    endpoint: SocketAddr,
    peer: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<tokio::net::TcpStream> {
    connect_with_wake(
        core,
        endpoint,
        peer,
        cancelled,
        Duration::from_secs(45),
        || {
            ensure!(!endpoint.ip().is_loopback(), "wake_network_unavailable");
            let interfaces = peer.and_then(|peer| {
                core.state
                    .lock()
                    .unwrap()
                    .configuration
                    .wake_peers
                    .get(peer)
                    .cloned()
            });
            let Some(interfaces) = interfaces.filter(|interfaces| !interfaces.is_empty()) else {
                bail!("wake_info_unavailable");
            };
            send(&interfaces)
        },
    )
    .await
}

async fn connect_with_wake(
    core: &Core,
    endpoint: SocketAddr,
    peer: Option<&str>,
    cancelled: &AtomicBool,
    wait: Duration,
    wake: impl FnOnce() -> Result<()>,
) -> Result<tokio::net::TcpStream> {
    let check = || -> Result<()> {
        ensure!(
            !cancelled.load(Ordering::Acquire) && core.is_active(),
            "command_cancelled_or_timed_out"
        );
        if let Some(peer) = peer {
            ensure!(core.has_peer(peer), "device is not paired");
        }
        ensure!(!core.is_blocked(endpoint.ip()), "blocked_by_device");
        ensure!(
            crate::address::is_lan(endpoint.ip()),
            "use a local network address"
        );
        Ok(())
    };
    check()?;
    let dial = || async {
        tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::TcpStream::connect(endpoint),
        )
        .await
        .map_err(anyhow::Error::from)?
        .map_err(anyhow::Error::from)
    };
    let failure = match dial().await {
        Ok(stream) => {
            check()?;
            return Ok(stream);
        }
        Err(error) => error,
    };
    check()?;
    let Some(peer) = peer else {
        return Err(failure);
    };
    let owner = {
        let mut waking = core.waking.lock().unwrap();
        let count = waking.entry(peer.to_owned()).or_default();
        *count += 1;
        *count == 1
    };
    let _waking = Waking { core, peer };
    if owner {
        core.revision.fetch_add(1, Ordering::Release);
        wake()?;
    }
    let deadline = tokio::time::Instant::now() + wait;
    while tokio::time::Instant::now() < deadline {
        check()?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Ok(stream) = dial().await {
            check()?;
            return Ok(stream);
        }
    }
    bail!("wake_timed_out")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoCommands;

    impl crate::Executor for NoCommands {
        fn hosts(&self) -> Vec<crate::SharedHost> {
            Vec::new()
        }

        fn execute(
            &self,
            _: &str,
            _: &str,
            _: &AtomicBool,
            _: &mut dyn FnMut(bool, &[u8]) -> std::io::Result<()>,
        ) -> Result<i32> {
            bail!("not used")
        }
    }

    #[test]
    fn offline_connection_wakes_once_waits_cancels_and_never_wakes_unpaired_devices() {
        use std::{net::TcpListener, sync::Arc};
        let directory = tempfile::tempdir().unwrap();
        let core = Core::load(directory.path(), "A", Arc::new(NoCommands)).unwrap();
        core.running.store(true, Ordering::Release);
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = port.local_addr().unwrap();
        drop(port);
        core.state.lock().unwrap().configuration.peers.insert(
            "paired".into(),
            Device {
                id: "paired".into(),
                name: "B".into(),
                endpoint,
                public_key: String::new(),
            },
        );
        let runtime = crate::transport::runtime().unwrap();
        runtime.block_on(async {
            let cancelled = AtomicBool::new(false);
            let mut listener = None;
            let stream = connect_with_wake(
                &core,
                endpoint,
                Some("paired"),
                &cancelled,
                Duration::from_secs(2),
                || {
                    assert!(core.is_waking());
                    listener = Some(TcpListener::bind(endpoint)?);
                    Ok(())
                },
            )
            .await
            .unwrap();
            assert!(!core.is_waking());
            drop(stream);
            drop(listener);
            let result = connect_with_wake(
                &core,
                endpoint,
                Some("paired"),
                &cancelled,
                Duration::from_secs(2),
                || {
                    cancelled.store(true, Ordering::Release);
                    Ok(())
                },
            )
            .await;
            assert!(result.unwrap_err().to_string().contains("cancelled"));
            assert!(!core.is_waking());
            cancelled.store(false, Ordering::Release);
            let result = connect_with_wake(
                &core,
                endpoint,
                Some("paired"),
                &cancelled,
                Duration::ZERO,
                || Ok(()),
            )
            .await;
            assert!(result.unwrap_err().to_string().contains("wake_timed_out"));
            assert!(!core.is_waking());
            assert!(
                connect_with_wake(
                    &core,
                    endpoint,
                    None,
                    &cancelled,
                    Duration::ZERO,
                    || panic!("unpaired access must not wake")
                )
                .await
                .is_err()
            );
            core.forget("paired").unwrap();
            assert!(
                connect_with_wake(
                    &core,
                    endpoint,
                    Some("paired"),
                    &cancelled,
                    Duration::ZERO,
                    || panic!("forgotten devices must not wake")
                )
                .await
                .is_err()
            );
        });
    }

    #[test]
    fn wake_metadata_survives_reload_and_is_removed_when_forgetting_a_peer() {
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let core = Core::load(directory.path(), "A", Arc::new(NoCommands)).unwrap();
        let peer = Device {
            id: core.id().to_owned(),
            name: "peer".into(),
            endpoint: "192.168.1.20:45838".parse().unwrap(),
            public_key: core.key.public_key().to_openssh().unwrap(),
        };
        let mut configuration = core.state.lock().unwrap().configuration.clone();
        configuration.peers.insert(peer.id.clone(), peer.clone());
        configuration.wake_peers.insert(
            peer.id.clone(),
            vec![WakeInterface {
                mac: [2, 1, 2, 3, 4, 5],
                address: "192.168.1.20".parse().unwrap(),
                prefix: 24,
            }],
        );
        core.persist(&configuration).unwrap();
        let reloaded = Core::load(directory.path(), "A", Arc::new(NoCommands)).unwrap();
        assert!(reloaded.has_wake_info(&peer.id));
        reloaded.forget(&peer.id).unwrap();
        let reloaded = Core::load(directory.path(), "A", Arc::new(NoCommands)).unwrap();
        assert!(!reloaded.has_wake_info(&peer.id));
        assert!(!reloaded.has_peer(&peer.id));
    }

    #[test]
    fn magic_packet_has_six_ff_bytes_and_sixteen_mac_copies() {
        let mac = [0x02, 1, 2, 3, 4, 5];
        let bytes = packet(mac);
        assert_eq!(bytes.len(), 102);
        assert_eq!(&bytes[..6], &[0xff; 6]);
        assert!(
            bytes[6..]
                .as_chunks::<6>()
                .0
                .iter()
                .all(|chunk| *chunk == mac)
        );
    }

    #[test]
    fn wake_metadata_is_bounded_and_rejects_invalid_macs_and_networks() {
        let mut value = WakeInterface {
            mac: [0x02, 1, 2, 3, 4, 5],
            address: Ipv4Addr::new(192, 168, 1, 20),
            prefix: 24,
        };
        assert!(validate(&[value.clone()]).is_ok());
        assert!(validate(&vec![value.clone(); 17]).is_err());
        value.mac[0] = 1;
        assert!(validate(&[value.clone()]).is_err());
        value.mac = [0; 6];
        assert!(validate(&[value.clone()]).is_err());
        value.mac = [0x02, 1, 2, 3, 4, 5];
        value.address = Ipv4Addr::new(8, 8, 8, 8);
        assert!(validate(&[value]).is_err());
        assert!(same_subnet(
            "192.168.1.2".parse().unwrap(),
            "192.168.1.20".parse().unwrap(),
            24
        ));
        assert!(!same_subnet(
            "192.168.2.2".parse().unwrap(),
            "192.168.1.20".parse().unwrap(),
            24
        ));
        assert!(!same_subnet(Ipv4Addr::LOCALHOST, Ipv4Addr::LOCALHOST, 0));
    }
}
