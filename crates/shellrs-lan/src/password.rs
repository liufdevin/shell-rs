use anyhow::{Result, ensure};
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Verifier {
    salt: [u8; 16],
    hash: [u8; 32],
}

impl Verifier {
    pub fn new(password: &str) -> Result<Self> {
        ensure!(
            password.chars().count() >= crate::MIN_PAIRING_PASSWORD_CHARS && password.len() <= 1024,
            "pairing password must contain at least {} characters",
            crate::MIN_PAIRING_PASSWORD_CHARS
        );
        let salt: [u8; 16] = rand::rng().random();
        Ok(Self {
            salt,
            hash: *derive(password, &salt)?,
        })
    }
    pub fn verify(&self, password: &str) -> Result<bool> {
        ensure!(password.len() <= 1024, "invalid password");
        Ok(bool::from(
            self.hash.ct_eq(derive(password, &self.salt)?.as_ref()),
        ))
    }
}
fn derive(password: &str, salt: &[u8; 16]) -> Result<Zeroizing<[u8; 32]>> {
    let mut hash = Zeroizing::new([0; 32]);
    let parameters =
        Params::new(64 * 1024, 3, 1, Some(32)).map_err(|error| anyhow::anyhow!(error))?;
    Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters)
        .hash_password_into(password.as_bytes(), salt, hash.as_mut())
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::Verifier;

    #[test]
    fn minimum_pairing_password_is_six_characters_not_six_bytes() {
        for password in ["", "abcde", "中文配对码"] {
            assert!(Verifier::new(password).is_err());
        }
        for password in ["lan6ok", "中文配对密码", "a longer pairing password"] {
            let verifier = Verifier::new(password).unwrap();
            assert!(verifier.verify(password).unwrap());
            assert!(!verifier.verify("wrong6").unwrap());
        }
        assert!(Verifier::new(&"x".repeat(1025)).is_err());
    }
}
