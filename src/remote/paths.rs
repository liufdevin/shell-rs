use super::{Connected, RouteDiagnostic, Target};
use std::{
    net::Ipv4Addr,
    time::{Duration, Instant},
};

pub(super) const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct PathSnapshot {
    connection: Connected,
    routes: Vec<RouteDiagnostic>,
    checked: Instant,
}

impl PathSnapshot {
    pub fn from_core(
        connection: &Connected,
        routes: &[serde_json::Value],
        peers: &[serde_json::Value],
    ) -> Self {
        Self {
            connection: connection.clone(),
            routes: connection
                .devices
                .iter()
                .map(|target| RouteDiagnostic::from_core(target.virtual_ip, routes, peers))
                .collect(),
            checked: Instant::now(),
        }
    }

    pub fn is_current(&self, connection: &Connected) -> bool {
        self.connection.server == connection.server
            && self.connection.network == connection.network
            && self.connection.virtual_ip == connection.virtual_ip
            && self.connection.devices.len() == connection.devices.len()
            && self.connection.devices.iter().all(|old| {
                connection
                    .devices
                    .iter()
                    .any(|new| old.device_id == new.device_id && old.virtual_ip == new.virtual_ip)
            })
    }

    pub fn is_stale(&self) -> bool {
        self.checked.elapsed() >= Duration::from_secs(15)
    }

    pub fn devices(&self) -> impl Iterator<Item = (&Target, &RouteDiagnostic)> {
        self.connection.devices.iter().zip(&self.routes)
    }

    pub fn route(&self, device: &str, address: Ipv4Addr) -> Option<&RouteDiagnostic> {
        self.devices()
            .find(|(target, _)| target.device_id == device && target.virtual_ip == address)
            .map(|(_, route)| route)
    }
}

#[cfg(test)]
mod tests {
    use super::{Connected, PathSnapshot, Target};
    use crate::remote::Transport;
    use serde_json::json;
    use std::time::{Duration, Instant};

    fn connection() -> Connected {
        Connected::new(
            "server".into(),
            "network".into(),
            "Test".into(),
            "10.215.0.2".parse().unwrap(),
            vec![
                Target::new("board-a".into(), "A".into(), "10.215.0.11".parse().unwrap()),
                Target::new("board-b".into(), "B".into(), "10.215.0.12".parse().unwrap()),
            ],
        )
    }

    #[test]
    fn snapshots_match_device_identity_and_address_not_display_names_or_order() {
        let mut connection = connection();
        let snapshot = PathSnapshot::from_core(
            &connection,
            &[
                json!({"ipv4":"10.215.0.11","path_len":1}),
                json!({"ipv4":"10.215.0.12","path_len":2}),
            ],
            &[],
        );
        assert_eq!(
            snapshot
                .route("board-a", "10.215.0.11".parse().unwrap())
                .unwrap()
                .transport,
            Transport::Direct
        );
        assert_eq!(
            snapshot
                .route("board-b", "10.215.0.12".parse().unwrap())
                .unwrap()
                .transport,
            Transport::Relay
        );
        assert!(
            snapshot
                .route("board-b", "10.215.0.11".parse().unwrap())
                .is_none()
        );
        connection.devices.reverse();
        connection.devices[0].display_name = "Renamed".into();
        assert!(snapshot.is_current(&connection));
        connection.devices[0].virtual_ip = "10.215.0.13".parse().unwrap();
        assert!(!snapshot.is_current(&connection));
    }

    #[test]
    fn network_changes_device_removal_and_expiry_invalidate_snapshots() {
        let connection = connection();
        let mut snapshot = PathSnapshot::from_core(&connection, &[], &[]);
        assert!(
            snapshot
                .devices()
                .all(|(_, route)| route.transport == Transport::Unknown)
        );
        assert!(!snapshot.is_stale());
        snapshot.checked = Instant::now() - Duration::from_secs(16);
        assert!(snapshot.is_stale());
        for changed in [
            Connected {
                server: "other".into(),
                ..connection.clone()
            },
            Connected {
                network: "other".into(),
                ..connection.clone()
            },
            Connected {
                virtual_ip: "10.215.0.3".parse().unwrap(),
                ..connection.clone()
            },
            Connected {
                devices: vec![],
                ..connection.clone()
            },
        ] {
            assert!(!snapshot.is_current(&changed));
        }
    }
}
