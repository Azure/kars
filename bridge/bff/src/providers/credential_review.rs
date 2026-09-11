// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Compatibility adapter for the existing version-one credential review key.
//! This is the legacy secret-key derivation, not a content digest or HKDF.
//! Changing it requires a versioned review/continuation/value-tag migration.

use sha2::{Digest, Sha256};

pub(crate) fn derive_v1_key(principal_secret: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"kars-bridge/credential-review-signing-key/v1\0");
    hash.update(principal_secret.as_bytes());
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_v1_key_preserves_domain_null_byte_and_raw_secret_encoding() {
        for (secret, expected) in [
            (
                "public-test-principal-secret",
                "8e69e654c5c17251d1573e489777e4d62ad3b8bf4d10d6f331d03a6853332f87",
            ),
            (
                " public-test-principal-secret ",
                "f5978a2afbee1cf3936edd5d77b6ffb18547db05e343bbb41d4fba811f1fe69c",
            ),
        ] {
            assert_eq!(hex::encode(derive_v1_key(secret)), expected);
        }
    }
}
