// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! JWT library adapter: maintained RSA public verification with the former
//! RustCrypto key-size bounds, and AWS-LC for every other operation.

use jsonwebtoken::{
    Algorithm, AlgorithmFamily, DecodingKey, DecodingKeyKind,
    crypto::{CryptoProvider, JwtVerifier, aws_lc},
    errors::{ErrorKind, Result},
    signature::{self, Verifier},
};
use rsa::{
    BoxedUint, PublicKey,
    pkcs1::der::Decode,
    sha2::{Sha256, Sha384, Sha512},
};
use std::sync::LazyLock;

pub(crate) static DEFAULT_PROVIDER: LazyLock<CryptoProvider> = LazyLock::new(|| CryptoProvider {
    verifier_factory: new_verifier,
    ..aws_lc::DEFAULT_PROVIDER.clone()
});

type VerifyRsa = fn(&PublicKey, &[u8], &[u8]) -> std::result::Result<(), rsa::Error>;

fn new_verifier(algorithm: &Algorithm, key: &DecodingKey) -> Result<Box<dyn JwtVerifier>> {
    let verify: VerifyRsa = match algorithm {
        Algorithm::RS256 => PublicKey::pkcs1v15_verify::<Sha256>,
        Algorithm::RS384 => PublicKey::pkcs1v15_verify::<Sha384>,
        Algorithm::RS512 => PublicKey::pkcs1v15_verify::<Sha512>,
        Algorithm::PS256 => PublicKey::pss_verify::<Sha256>,
        Algorithm::PS384 => PublicKey::pss_verify::<Sha384>,
        Algorithm::PS512 => PublicKey::pss_verify::<Sha512>,
        _ => return (aws_lc::DEFAULT_PROVIDER.verifier_factory)(algorithm, key),
    };
    if key.family() != AlgorithmFamily::Rsa {
        return Err(ErrorKind::InvalidKeyFormat.into());
    }
    Ok(Box::new(RsaVerifier {
        algorithm: *algorithm,
        key: key.clone(),
        verify,
    }))
}

struct RsaVerifier {
    algorithm: Algorithm,
    key: DecodingKey,
    verify: VerifyRsa,
}

impl Verifier<Vec<u8>> for RsaVerifier {
    fn verify(
        &self,
        message: &[u8],
        signature: &Vec<u8>,
    ) -> std::result::Result<(), signature::Error> {
        let key = rsa_public_key(&self.key)?;
        (self.verify)(&key, message, signature).map_err(signature::Error::from_source)
    }
}

fn rsa_public_key(key: &DecodingKey) -> std::result::Result<PublicKey, signature::Error> {
    let (n, e) = match key.kind() {
        DecodingKeyKind::RsaModulusExponent { n, e } => (
            BoxedUint::from_be_slice_vartime(n),
            BoxedUint::from_be_slice_vartime(e),
        ),
        DecodingKeyKind::SecretOrDer(bytes) => {
            let key = rsa::pkcs1::RsaPublicKeyRef::from_der(bytes)
                .map_err(signature::Error::from_source)?;
            (
                BoxedUint::from_be_slice_vartime(key.modulus.as_bytes()),
                BoxedUint::from_be_slice_vartime(key.public_exponent.as_bytes()),
            )
        }
    };
    // The previous verifier had no minimum and a 4096-bit maximum. Apply
    // those bounds explicitly; retain the library's other public-key checks.
    PublicKey::new_with_size_limits(n, e, 0..=4096).map_err(signature::Error::from_source)
}

impl JwtVerifier for RsaVerifier {
    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_constructor_preserves_size_and_component_checks() {
        for n in [vec![17], vec![255; 64], vec![255; 512]] {
            let key = DecodingKey::from_rsa_raw_components(&n, &[3]);
            assert!(rsa_public_key(&key).is_ok());
        }

        let mut above_limit = vec![255; 513];
        above_limit[0] = 1;
        for (n, e) in [
            (vec![0], vec![3]),
            (vec![1], vec![3]),
            (vec![16], vec![3]),
            (vec![17], vec![0]),
            (vec![17], vec![1]),
            (vec![17], vec![2]),
            (vec![17], vec![19]),
            (vec![255; 64], vec![255; 9]),
            (above_limit, vec![1, 0, 1]),
        ] {
            let key = DecodingKey::from_rsa_raw_components(&n, &e);
            assert!(rsa_public_key(&key).is_err());
        }
    }
}
