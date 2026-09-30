//! Mutual password proof bound to the TLS session.
//!
//! Both sides derive `K = Argon2id(password, salt)` and prove knowledge of
//! it with `HMAC-SHA256(K, label || E)`, where `E` is keying material
//! exported from this connection's TLS session. A man in the middle holds
//! two different TLS sessions, so neither side's proof verifies at the
//! other end. The password itself never crosses the network.

use argon2::{Algorithm, Argon2, Params, Version};
use flyx_protocol::KdfParams;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub(crate) const CLIENT_LABEL: &[u8] = b"flyx client proof v1";
pub(crate) const HOST_LABEL: &[u8] = b"flyx host proof v1";
const EXPORTER_LABEL: &[u8] = b"EXPORTER-flyx-auth-v1";

#[derive(Debug, thiserror::Error)]
pub(crate) enum AuthError {
    #[error("invalid key-derivation parameters: {0}")]
    Kdf(argon2::Error),
    #[error("TLS keying material unavailable")]
    Exporter,
    #[error("system random number generator failed")]
    Random,
}

/// Derives the 32-byte proof key. Slow on purpose (tens of milliseconds):
/// call from a blocking task.
pub(crate) fn derive_key(
    password: &str,
    salt: &[u8; 16],
    kdf: KdfParams,
) -> Result<[u8; 32], AuthError> {
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(AuthError::Kdf)?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(AuthError::Kdf)?;
    Ok(key)
}

/// Keying material unique to this connection's TLS session.
pub(crate) fn exporter(connection: &quinn::Connection) -> Result<[u8; 32], AuthError> {
    let mut out = [0u8; 32];
    connection
        .export_keying_material(&mut out, EXPORTER_LABEL, b"")
        .map_err(|_| AuthError::Exporter)?;
    Ok(out)
}

fn mac(key: &[u8; 32], label: &[u8], exporter: &[u8; 32]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length works");
    mac.update(label);
    mac.update(exporter);
    mac
}

pub(crate) fn prove(key: &[u8; 32], label: &[u8], exporter: &[u8; 32]) -> [u8; 32] {
    mac(key, label, exporter).finalize().into_bytes().into()
}

/// Constant-time check of a peer's proof.
pub(crate) fn verify(key: &[u8; 32], label: &[u8], exporter: &[u8; 32], proof: &[u8; 32]) -> bool {
    mac(key, label, exporter).verify_slice(proof).is_ok()
}

pub(crate) fn random_salt() -> Result<[u8; 16], AuthError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|_| AuthError::Random)?;
    Ok(salt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: KdfParams = KdfParams {
        memory_kib: 8 * 1024,
        iterations: 1,
        parallelism: 1,
    };

    #[test]
    fn same_password_and_session_verify() {
        let salt = [3u8; 16];
        let exporter = [9u8; 32];
        let host_key = derive_key("correct horse", &salt, FAST).unwrap();
        let client_key = derive_key("correct horse", &salt, FAST).unwrap();
        let proof = prove(&client_key, CLIENT_LABEL, &exporter);
        assert!(verify(&host_key, CLIENT_LABEL, &exporter, &proof));
    }

    #[test]
    fn wrong_password_fails() {
        let salt = [3u8; 16];
        let exporter = [9u8; 32];
        let host_key = derive_key("correct horse", &salt, FAST).unwrap();
        let client_key = derive_key("wrong horse", &salt, FAST).unwrap();
        let proof = prove(&client_key, CLIENT_LABEL, &exporter);
        assert!(!verify(&host_key, CLIENT_LABEL, &exporter, &proof));
    }

    #[test]
    fn proof_is_bound_to_session_and_direction() {
        let salt = [3u8; 16];
        let key = derive_key("pw", &salt, FAST).unwrap();
        let proof = prove(&key, CLIENT_LABEL, &[1u8; 32]);
        // Different TLS session (as seen through a MITM).
        assert!(!verify(&key, CLIENT_LABEL, &[2u8; 32], &proof));
        // A client proof cannot be replayed as a host proof.
        assert!(!verify(&key, HOST_LABEL, &[1u8; 32], &proof));
    }

    #[test]
    fn salt_changes_key() {
        let a = derive_key("pw", &[1u8; 16], FAST).unwrap();
        let b = derive_key("pw", &[2u8; 16], FAST).unwrap();
        assert_ne!(a, b);
        assert_ne!(random_salt().unwrap(), random_salt().unwrap());
    }
}
