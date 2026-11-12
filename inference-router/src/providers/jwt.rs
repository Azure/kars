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
    match key.kind() {
        DecodingKeyKind::RsaModulusExponent { n, e } => rsa_public_components(n, e),
        DecodingKeyKind::SecretOrDer(bytes) => {
            let key = rsa::pkcs1::RsaPublicKeyRef::from_der(bytes)
                .map_err(signature::Error::from_source)?;
            rsa_public_components(key.modulus.as_bytes(), key.public_exponent.as_bytes())
        }
    }
}

fn rsa_public_components(n: &[u8], e: &[u8]) -> std::result::Result<PublicKey, signature::Error> {
    // The former BigUint decoder discarded zero padding. BoxedUint otherwise
    // allocates arithmetic precision from the entire encoded public component.
    let significant = |bytes: &[u8]| bytes.iter().take_while(|byte| **byte == 0).count();
    let n = &n[significant(n)..];
    let e = &e[significant(e)..];
    if n.len() > 512 {
        return Err(signature::Error::from_source(rsa::Error::ModulusTooLarge));
    }
    if e.len() > 5 {
        return Err(signature::Error::from_source(
            rsa::Error::PublicExponentTooLarge,
        ));
    }
    // The previous verifier had no minimum and a 4096-bit maximum. Apply
    // those bounds explicitly; retain the library's other public-key checks.
    PublicKey::new_with_size_limits(
        BoxedUint::from_be_slice_vartime(n),
        BoxedUint::from_be_slice_vartime(e),
        0..=4096,
    )
    .map_err(signature::Error::from_source)
}

impl JwtVerifier for RsaVerifier {
    fn algorithm(&self) -> Algorithm {
        self.algorithm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::traits::PublicKeyParts;

    #[test]
    fn public_key_constructor_preserves_size_and_component_checks() {
        for n in [vec![17], vec![255; 64], vec![255; 512]] {
            let key = DecodingKey::from_rsa_raw_components(&n, &[3]);
            assert!(rsa_public_key(&key).is_ok());
        }

        let mut above_limit = vec![255; 513];
        above_limit[0] = 1;
        for (n, e) in [
            (vec![], vec![3]),
            (vec![17], vec![]),
            (vec![], vec![]),
            (vec![0], vec![3]),
            (vec![1], vec![3]),
            (vec![16], vec![3]),
            (vec![17], vec![0]),
            (vec![17], vec![1]),
            (vec![17], vec![2]),
            (vec![17], vec![19]),
            (vec![255; 64], vec![1, 0, 0, 0, 0, 1]),
            (vec![255; 64], vec![255; 9]),
            (above_limit, vec![1, 0, 1]),
        ] {
            let key = DecodingKey::from_rsa_raw_components(&n, &e);
            assert!(rsa_public_key(&key).is_err());
        }
    }

    #[test]
    fn leading_zero_components_do_not_inflate_arithmetic_precision() {
        let short = DecodingKey::from_rsa_raw_components(&[17], &[3]);
        let mut padded_n = vec![0; 1024];
        padded_n.push(17);
        let mut padded_e = vec![0; 1024];
        padded_e.push(3);
        let padded = DecodingKey::from_rsa_raw_components(&padded_n, &padded_e);
        let padded = rsa_public_key(&padded).unwrap();
        let short = rsa_public_key(&short).unwrap();
        assert_eq!(padded.n_bits_precision(), short.n_bits_precision());
        assert_eq!(padded.e().bits_precision(), short.e().bits_precision());
    }
}
