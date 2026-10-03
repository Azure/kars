// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use super::{Error, Result};

#[derive(Clone, Copy)]
pub(super) enum Role {
    Runtime,
    Dispatcher,
}
impl Role {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Dispatcher => "dispatcher",
        }
    }
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn seed(root: &str, role: Role, owner_uid: &str, pod_uid: &str) -> Result<[u8; 32]> {
    if root.len() != 64 || !component(owner_uid) || !component(pod_uid) {
        return Err(Error::Invalid("invalid mission identity input"));
    }
    let key = hex::decode(root).map_err(|_| Error::Invalid("invalid mission identity root"))?;
    let input = serde_json::to_vec(&["kars-mission-identity-v1", role.name(), owner_uid, pod_uid])
        .map_err(|_| Error::Invalid("mission identity encoding failed"))?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key)
        .map_err(|_| Error::Invalid("invalid mission identity root"))?;
    mac.update(&input);
    Ok(mac.finalize().into_bytes().into())
}

pub(super) fn did(root: &str, role: Role, owner_uid: &str, pod_uid: &str) -> Result<String> {
    let signing = SigningKey::from_bytes(&seed(root, role, owner_uid, pod_uid)?);
    let digest = hex::encode(Sha256::digest(signing.verifying_key().as_bytes()));
    Ok(format!("did:mesh:{}", &digest[..32]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_identity_matches_typescript_hmac_vectors() {
        let root = "a".repeat(64);
        assert_eq!(
            hex::encode(seed(&root, Role::Runtime, "sandbox-uid", "pod-uid").unwrap()),
            "0c1459ec1399d4d1c867a81a2b22e5e5d05d4752748d35a45a5deebe77bf60bf"
        );
        assert_eq!(
            hex::encode(seed(&root, Role::Dispatcher, "sandbox-uid", "pod-uid").unwrap()),
            "e582df2d3650389264eba8d39d5f0076d881660ee6bf826364deebb2fabe36f0"
        );
        assert_eq!(
            did(&root, Role::Runtime, "sandbox-uid", "pod-uid").unwrap(),
            did(
                &root.to_uppercase(),
                Role::Runtime,
                "sandbox-uid",
                "pod-uid"
            )
            .unwrap()
        );
        let original = did(&root, Role::Runtime, "sandbox-uid", "pod-uid").unwrap();
        for changed in [
            did(&root, Role::Dispatcher, "sandbox-uid", "pod-uid"),
            did(&root, Role::Runtime, "other-owner", "pod-uid"),
            did(&root, Role::Runtime, "sandbox-uid", "other-pod"),
            did(&"b".repeat(64), Role::Runtime, "sandbox-uid", "pod-uid"),
        ] {
            assert_ne!(original, changed.unwrap());
        }
    }

    #[test]
    fn mission_identity_rejects_malformed_inputs_without_echoing_them() {
        for root in [
            "secret".into(),
            "z".repeat(64),
            "a".repeat(63),
            "é".repeat(32),
        ] {
            let error = did(&root, Role::Runtime, "owner", "pod")
                .unwrap_err()
                .to_string();
            assert!(!error.contains(&root));
        }
        for value in [
            "".into(),
            "-uid".into(),
            "a/b".into(),
            "x".repeat(254),
            "é".into(),
        ] {
            assert!(did(&"a".repeat(64), Role::Runtime, &value, "pod").is_err());
            assert!(did(&"a".repeat(64), Role::Runtime, "owner", &value).is_err());
        }
    }
}
