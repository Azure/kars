// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::kars::receipt_log::ReceiptLog;

#[derive(Clone, Debug, Default)]
pub(crate) struct AnchorPins {
    pub key_id: Option<String>,
    pub public_key: Option<String>,
}

pub(super) struct TrustedAnchor {
    pub key_id: String,
    pub public_key_b64: String,
    pub public_key: [u8; 32],
    pub scheme: String,
    pub pinned: bool,
}

fn configured(name: &str) -> Result<Option<String>, &'static str> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("Configured receipt anchor pin is not valid UTF-8.")
        }
    }
}

fn public_key(value: &str) -> Result<[u8; 32], &'static str> {
    STANDARD
        .decode(value.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("Receipt anchor public key must be a base64-encoded 32-byte Ed25519 key.")
}

impl AnchorPins {
    pub fn from_env() -> Result<Self, &'static str> {
        Ok(Self {
            key_id: configured("BRIDGE_RECEIPT_ANCHOR_KEY_ID")?,
            public_key: configured("BRIDGE_RECEIPT_ANCHOR_PUBKEY")?,
        })
    }

    pub(super) fn resolve(&self, log: &ReceiptLog) -> Result<TrustedAnchor, &'static str> {
        let (key_id, public_key_b64, scheme) = log
            .anchor()
            .ok_or("No published receipt public-key anchor is available.")?;
        let key = public_key(&public_key_b64)?;
        if let Some(expected) = &self.key_id {
            // The controller defines key IDs as full SHA-256 fingerprints of
            // the raw public key, not an independently mutable ConfigMap label.
            if expected != &super::sha256_hex(&key) || expected != &key_id {
                return Err(
                    "Receipt anchor key does not match the configured SHA-256 fingerprint.",
                );
            }
        }

        if let Some(expected) = &self.public_key
            && public_key(expected)? != key
        {
            return Err("Receipt anchor key does not match the configured public-key pin.");
        }
        Ok(TrustedAnchor {
            key_id,
            public_key_b64,
            public_key: key,
            scheme,
            pinned: self.key_id.is_some() || self.public_key.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    const ID: &str = "21fe31dfa154a261626bf854046fd2271b7bed4b6abe45aa58877ef47f9721b9";

    fn log() -> ReceiptLog {
        ReceiptLog {
            public_key: Some(
                [
                    ("keyId", ID),
                    ("publicKey", KEY),
                    ("scheme", "DSSEv1+ed25519"),
                ]
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .into_iter()
                .collect(),
            ),
            ..ReceiptLog::default()
        }
    }

    #[test]
    fn pins_follow_the_controller_raw_key_fingerprint_contract() {
        assert_eq!(super::super::sha256_hex(&public_key(KEY).unwrap()), ID);
        for (key_id, public_key) in [
            (None, None),
            (Some(ID.into()), None),
            (None, Some(format!(" {KEY}\n"))),
            (Some(ID.into()), Some(KEY.into())),
        ] {
            let configured = key_id.is_some() || public_key.is_some();
            let anchor = AnchorPins { key_id, public_key }.resolve(&log()).unwrap();
            assert_eq!(anchor.pinned, configured);
            assert_eq!(anchor.key_id, ID);
        }
    }

    #[test]
    fn copied_key_id_cannot_substitute_another_public_key() {
        let pins = AnchorPins {
            key_id: Some(ID.into()),
            public_key: None,
        };
        let mut log = log();
        log.public_key
            .as_mut()
            .unwrap()
            .insert("publicKey".into(), STANDARD.encode([17_u8; 32]));
        assert!(pins.resolve(&log).is_err());
        assert!(!AnchorPins::default().resolve(&log).unwrap().pinned);
    }

    #[test]
    fn malformed_empty_mismatched_or_missing_pins_fail_closed() {
        for (key_id, public_key) in [
            (Some(String::new()), None),
            (Some("wrong".into()), None),
            (None, Some(String::new())),
            (None, Some("not base64".into())),
            (None, Some(STANDARD.encode([1_u8; 31]))),
            (None, Some(STANDARD.encode([1_u8; 33]))),
            (Some(ID.into()), Some(STANDARD.encode([1_u8; 32]))),
        ] {
            assert!(AnchorPins { key_id, public_key }.resolve(&log()).is_err());
        }
        assert!(
            AnchorPins::default()
                .resolve(&ReceiptLog::default())
                .is_err()
        );
        let mut invalid = log();
        invalid
            .public_key
            .as_mut()
            .unwrap()
            .insert("publicKey".into(), "invalid".into());
        assert!(AnchorPins::default().resolve(&invalid).is_err());
    }
}
