// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Standard content/receipt digest adapters, not secret-key derivation.
//! Callers retain their existing framing, normalization and truncation contracts.

use sha2::{Digest, Sha256};

pub(crate) fn sha256(bytes: impl AsRef<[u8]>) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(sha256(bytes))
}

pub(crate) fn sha256_parts<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_standard_known_answers_without_normalizing_bytes() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_parts([b"a".as_slice(), b"", b"bc"]), sha256(b"abc"));
        assert_ne!(sha256(b"abc"), sha256(b"abc\n"));
        assert_ne!(sha256(b"abc"), sha256(b"ABC"));
        assert_ne!(sha256(b"abc"), sha256(b"abc\0"));
    }
}
