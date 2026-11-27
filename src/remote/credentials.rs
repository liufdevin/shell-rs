use super::{Backend, Operation, Reply, SavedDebugDevice, Server};
use crate::{
    cli::Secret,
    secrets::{SecretRef, SharedSecretStore},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroize::Zeroizing;

#[derive(Serialize, Deserialize)]
struct SavedPassword {
    url: String,
    username: String,
    password: String,
}

impl Drop for SavedPassword {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.password.zeroize();
    }
}

#[derive(Serialize, Deserialize)]
struct SavedDebugKey {
    url: String,
    device_id: String,
    key: String,
}

impl Drop for SavedDebugKey {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.key.zeroize();
    }
}

pub(super) fn password_ref(id: &str) -> SecretRef {
    SecretRef::credential(format!("remote-management:{id}"))
}

pub(super) fn debug_key_ref(id: &str) -> SecretRef {
    SecretRef::credential(format!("remote-debug:{id}"))
}

pub(super) fn read_password(
    secrets: &SharedSecretStore,
    server: &Server,
) -> anyhow::Result<Option<Secret>> {
    let Some(value) = secrets.get(&password_ref(&server.id))? else {
        return Ok(None);
    };
    let record: SavedPassword = serde_json::from_str(&value)?;
    if record.url != super::api::server_url(&server.url)? || record.username != server.username {
        return Ok(None);
    }
    Ok(Some(Secret::new(record.password.clone())))
}

pub(super) fn read_debug_key(
    secrets: &SharedSecretStore,
    device: &SavedDebugDevice,
) -> anyhow::Result<Option<Secret>> {
    let Some(value) = secrets.get(&debug_key_ref(&device.id))? else {
        return Ok(None);
    };
    let record: SavedDebugKey = serde_json::from_str(&value)?;
    if record.url != super::api::server_url(&device.url)? || record.device_id != device.device_id {
        return Ok(None);
    }
    Ok(Some(Secret::new(record.key.clone())))
}

pub(super) fn perform(
    backend: Arc<dyn Backend>,
    secrets: SharedSecretStore,
    operation: Operation,
) -> Reply {
    match operation {
        Operation::SilentLogin(server) => match read_password(&secrets, &server) {
            Ok(Some(password)) => {
                let mut reply = backend.perform(Operation::Login(server, password));
                reply.silent = true;
                reply.keep_connection = true;
                reply
            }
            _ => Reply {
                silent: true,
                keep_connection: true,
                ..Reply::default()
            },
        },
        Operation::SavedLogin(server) => match read_password(&secrets, &server) {
            Ok(Some(password)) => login(backend, server, password),
            result => Reply {
                prompt: Some((server, None)),
                error: result.err().map(|_| "credential_read".into()),
                keep_connection: true,
                ..Reply::default()
            },
        },
        Operation::RememberLogin(server, password, remember) => {
            let mut reply = login(backend, server.clone(), password.clone());
            if reply.error.is_none() && reply.catalog.is_some() {
                let result = if remember {
                    let record = SavedPassword {
                        url: super::api::server_url(&server.url).unwrap(),
                        username: server.username.clone(),
                        password: password.expose().to_owned(),
                    };
                    let encoded = Zeroizing::new(serde_json::to_string(&record).unwrap());
                    secrets.set(&password_ref(&server.id), &encoded)
                } else {
                    secrets.delete(&password_ref(&server.id))
                };
                if result.is_err() {
                    reply.error = Some("credential_save".into());
                }
            }
            reply
        }
        Operation::Forget(id) => {
            let mut reply = backend.perform(Operation::Logout(id.clone()));
            if reply.error.is_none() {
                if secrets.delete(&password_ref(&id)).is_ok() {
                    reply.forgotten = Some(id);
                } else {
                    reply.error = Some("credential_delete".into());
                }
            }
            reply
        }
        Operation::OpenSavedDebugScreen(device) => {
            match read_debug_key(&secrets, &device) {
                Ok(Some(key)) => {
                    let mut reply = backend.perform(Operation::OpenDebugScreen {
                        url: device.url.clone(),
                        device_id: device.device_id.clone(),
                        key,
                    });
                    reply.keep_connection = true;
                    if let Some(screen) = reply.screen.as_mut() {
                        screen.device_name = device.label().to_owned();
                    }
                    if reply.error.as_deref().is_some_and(|code| {
                        matches!(code, "http_401" | "http_403" | "invalid_params")
                    }) {
                        reply.debug_prompt = Some(device);
                    }
                    reply
                }
                result => Reply {
                    debug_prompt: Some(device),
                    error: result.err().map(|_| "credential_read".into()),
                    keep_connection: true,
                    ..Reply::default()
                },
            }
        }
        Operation::RememberDebugScreen { device, key, save } => {
            let mut reply = backend.perform(Operation::OpenDebugScreen {
                url: device.url.clone(),
                device_id: device.device_id.clone(),
                key: key.clone(),
            });
            reply.keep_connection = true;
            if let Some(screen) = reply.screen.as_mut() {
                screen.device_name = device.label().to_owned();
            }
            if reply.error.is_none()
                && save
                && let Ok(normalized_url) = super::api::server_url(&device.url)
            {
                let record = SavedDebugKey {
                    url: normalized_url,
                    device_id: device.device_id.clone(),
                    key: key.expose().to_owned(),
                };
                let encoded = Zeroizing::new(serde_json::to_string(&record).unwrap());
                if secrets.set(&debug_key_ref(&device.id), &encoded).is_ok() {
                    reply.saved_debug_device = Some(device);
                } else {
                    reply.error = Some("credential_save".into());
                }
            }
            reply
        }
        Operation::ForgetDebugDevice(id) => {
            if secrets.delete(&debug_key_ref(&id)).is_ok() {
                Reply {
                    forgotten_debug_device: Some(id),
                    keep_connection: true,
                    ..Reply::default()
                }
            } else {
                Reply {
                    error: Some("credential_delete".into()),
                    keep_connection: true,
                    ..Reply::default()
                }
            }
        }
        operation => backend.perform(operation),
    }
}

fn login(backend: Arc<dyn Backend>, server: Server, password: Secret) -> Reply {
    let mut reply = backend.perform(Operation::Login(server.clone(), password.clone()));
    if reply.error.is_some() {
        reply.prompt = Some((server, Some(password)));
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::{password_ref, perform, read_password};
    use crate::{
        cli::Secret,
        remote::{Backend, Catalog, Operation, Reply, Server},
        secrets::{InMemorySecretStore, SecretRef, SecretStore, SharedSecretStore},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use zeroize::Zeroizing;

    #[derive(Default)]
    struct FakeBackend {
        reject: AtomicBool,
    }

    impl Backend for FakeBackend {
        fn perform(&self, operation: Operation) -> Reply {
            let mut reply = Reply::default();
            match operation {
                Operation::Login(server, password) => {
                    assert_eq!(password.expose(), "fixture-secret");
                    if self.reject.load(Ordering::SeqCst) {
                        reply.error = Some("http_401".into());
                    } else {
                        reply.catalog = Some((server.id, Catalog::default()));
                    }
                }
                Operation::Logout(id) => reply.logged_out = Some(id),
                Operation::OpenDebugScreen { .. } => {}
                _ => panic!("unexpected network operation"),
            }
            reply
        }
    }

    #[test]
    fn passwords_survive_new_backend_but_are_bound_to_origin_and_account() {
        let secrets: SharedSecretStore = Arc::new(InMemorySecretStore::default());
        let server = Server::new(
            "test".into(),
            "https://example.test".into(),
            "operator".into(),
        );
        let first = perform(
            Arc::new(FakeBackend::default()),
            secrets.clone(),
            Operation::RememberLogin(server.clone(), Secret::new("fixture-secret"), true),
        );
        assert!(first.error.is_none());
        let mut renamed = server.clone();
        renamed.name = "renamed".into();
        let second = perform(
            Arc::new(FakeBackend::default()),
            secrets.clone(),
            Operation::SavedLogin(renamed),
        );
        assert!(second.catalog.is_some() && second.prompt.is_none());
        for changed in [
            Server {
                url: "https://other.test".into(),
                ..server.clone()
            },
            Server {
                username: "someone".into(),
                ..server.clone()
            },
        ] {
            assert!(read_password(&secrets, &changed).unwrap().is_none());
        }
        assert!(
            !serde_json::to_string(&server)
                .unwrap()
                .contains("fixture-secret")
        );
        let reply = perform(
            Arc::new(FakeBackend::default()),
            secrets.clone(),
            Operation::Forget(server.id.clone()),
        );
        assert_eq!(reply.forgotten, Some(server.id.clone()));
        assert!(secrets.get(&password_ref(&server.id)).unwrap().is_none());
    }

    #[test]
    fn rejected_login_preserves_password_until_explicit_opt_out() {
        let secrets: SharedSecretStore = Arc::new(InMemorySecretStore::default());
        let server = Server::new(
            "test".into(),
            "https://example.test".into(),
            "operator".into(),
        );
        let backend = Arc::new(FakeBackend::default());
        perform(
            backend.clone(),
            secrets.clone(),
            Operation::RememberLogin(server.clone(), Secret::new("fixture-secret"), true),
        );
        backend.reject.store(true, Ordering::SeqCst);
        let reply = perform(
            backend.clone(),
            secrets.clone(),
            Operation::SavedLogin(server.clone()),
        );
        assert!(reply.prompt.is_some());
        assert!(read_password(&secrets, &server).unwrap().is_some());
        backend.reject.store(false, Ordering::SeqCst);
        perform(
            backend,
            secrets.clone(),
            Operation::RememberLogin(server.clone(), Secret::new("fixture-secret"), false),
        );
        assert!(read_password(&secrets, &server).unwrap().is_none());
    }

    struct RefusingStore;
    impl SecretStore for RefusingStore {
        fn get(&self, _: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
            anyhow::bail!("denied")
        }
        fn set(&self, _: &SecretRef, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("denied")
        }
        fn delete(&self, _: &SecretRef) -> anyhow::Result<()> {
            anyhow::bail!("denied")
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    #[test]
    fn keychain_failures_are_reported_and_deletion_keeps_the_profile() {
        let server = Server::new(
            "test".into(),
            "https://example.test".into(),
            "operator".into(),
        );
        let backend = Arc::new(FakeBackend::default());
        let secrets: SharedSecretStore = Arc::new(RefusingStore);
        let reply = perform(
            backend.clone(),
            secrets.clone(),
            Operation::SavedLogin(server.clone()),
        );
        assert_eq!(reply.error.as_deref(), Some("credential_read"));
        assert!(reply.keep_connection && reply.prompt.is_some());
        let reply = perform(
            backend.clone(),
            secrets.clone(),
            Operation::RememberLogin(server.clone(), Secret::new("fixture-secret"), true),
        );
        assert_eq!(reply.error.as_deref(), Some("credential_save"));
        assert!(reply.catalog.is_some());
        let reply = perform(backend, secrets, Operation::Forget(server.id));
        assert_eq!(reply.error.as_deref(), Some("credential_delete"));
        assert!(reply.forgotten.is_none());
    }

    #[test]
    fn silent_login_never_prompts_and_debug_device_key_persists_in_keychain() {
        let server = Server::new(
            "test".into(),
            "https://example.test".into(),
            "operator".into(),
        );
        let backend = Arc::new(FakeBackend::default());
        let secrets: SharedSecretStore = Arc::new(InMemorySecretStore::default());

        let reply = perform(
            backend.clone(),
            secrets.clone(),
            Operation::SilentLogin(server.clone()),
        );
        assert!(reply.silent);
        assert!(reply.prompt.is_none());
        assert!(reply.catalog.is_none());

        let device = super::SavedDebugDevice::new(
            "客厅测试板".into(),
            "https://example.test".into(),
            "7K9M-2R8X".into(),
        );
        let saved = perform(
            backend.clone(),
            secrets.clone(),
            Operation::RememberDebugScreen {
                device: device.clone(),
                key: Secret::new("5W3P-6T4H"),
                save: true,
            },
        );
        assert!(saved.error.is_none());
        assert_eq!(saved.saved_debug_device.as_ref(), Some(&device));
        assert!(
            super::read_debug_key(&secrets, &device)
                .unwrap()
                .is_some_and(|k| k.expose() == "5W3P-6T4H")
        );

        let deleted = perform(
            backend,
            secrets.clone(),
            Operation::ForgetDebugDevice(device.id.clone()),
        );
        assert_eq!(
            deleted.forgotten_debug_device.as_deref(),
            Some(device.id.as_str())
        );
        assert!(super::read_debug_key(&secrets, &device).unwrap().is_none());
    }
}
