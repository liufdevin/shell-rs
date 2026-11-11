use super::{Backend, Operation, Reply, Server};
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

pub(super) fn password_ref(id: &str) -> SecretRef {
    SecretRef::credential(format!("remote-management:{id}"))
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

pub(super) fn perform(
    backend: Arc<dyn Backend>,
    secrets: SharedSecretStore,
    operation: Operation,
) -> Reply {
    match operation {
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
}
