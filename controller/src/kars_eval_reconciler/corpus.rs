// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

pub(super) async fn resolve_corpus(
    src: &CorpusSource,
) -> Result<ResolvedCorpus, (&'static str, String)> {
    match (&src.builtin, &src.bundle_ref) {
        (Some(name), None) => {
            // Use the embedded bytes directly — the runner reads the
            // mounted file and validates with the same parser, so the
            // bytes the controller mounts must be the exact bytes the
            // library shipped, not a re-serialisation (which would
            // perturb whitespace and ordering, changing the digest).
            let bytes = kars_eval_corpus::builtin_bytes(name).ok_or_else(|| {
                (
                    reason::CORPUS_BUILTIN_MISSING,
                    format!("builtin corpus {name:?} not found"),
                )
            })?;
            // Validate parseability so we never mount a corpus the
            // runner cannot read.
            kars_eval_corpus::parse(bytes).map_err(|e| {
                (
                    reason::CORPUS_PARSE_FAILED,
                    format!("builtin corpus {name:?} failed to parse: {e}"),
                )
            })?;
            let digest = sha256_hex(bytes);
            Ok(ResolvedCorpus {
                bytes: bytes.to_vec(),
                digest,
                label: format!("{CORPUS_LABEL_BUILTIN_PREFIX}{name}"),
            })
        }
        (None, Some(bundle)) => {
            let signer_policy_handle = crate::signer_policy::global();
            let result = match signer_policy_handle.snapshot() {
                crate::signer_policy::SignerPolicyState::FromConfigMap(p) => {
                    let cfg: crate::policy_fetcher::SignerPolicyConfig = p.into();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::eval_corpus::EvalCorpusKind,
                    >(bundle, &cfg)
                    .await
                }
                crate::signer_policy::SignerPolicyState::Malformed(msg) => Err(
                    crate::policy_fetcher::FetchError::SignerPolicyMalformed(msg),
                ),
                crate::signer_policy::SignerPolicyState::Absent => {
                    let cfg = crate::policy_fetcher::SignerPolicyConfig::from_env();
                    crate::policy_fetcher::fetch_and_verify_generic::<
                        crate::policy_canonical::eval_corpus::EvalCorpusKind,
                    >(bundle, &cfg)
                    .await
                }
            };
            match result {
                Ok(v) => Ok(ResolvedCorpus {
                    bytes: v.bytes,
                    digest: v.digest,
                    label: format!(
                        "{}/{}@{}",
                        bundle.registry, bundle.repository, bundle.digest
                    ),
                }),
                Err(e) => Err((reason::CORPUS_FETCH_FAILED, e.to_string())),
            }
        }
        (Some(_), Some(_)) | (None, None) => {
            // Defence in depth — CEL already enforces XOR.
            Err((
                reason::SPEC_INVALID,
                "spec.corpus must set exactly one of builtin or bundleRef".into(),
            ))
        }
    }
}
