use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;

use crate::kars::receipt::{KarsReceiptSpec, ReceiptClaim};

pub(super) const SCHEME: &str = "DSSEv1+ed25519";
const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
const PREDICATE_TYPE: &str = "https://kars.azure.com/attestations/GovernanceReceipt/v0";

pub(super) struct BoundStatement {
    pub payload: Vec<u8>,
    pub statement: Value,
    pub claims: Vec<ReceiptClaim>,
}

pub(super) fn binds_subject(
    statement: &Value,
    spec: &KarsReceiptSpec,
    namespace: &str,
    task_name: &str,
) -> bool {
    let Some(digest) = spec
        .envelope_digest
        .strip_prefix("sha256:")
        .filter(|value| {
            matches!(value.len(), 32 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    else {
        return false;
    };
    let Some(subjects) = statement.get("subject").and_then(Value::as_array) else {
        return false;
    };
    let expected_name = format!("{namespace}/{task_name}");
    spec.task_ref.name == task_name
        && subjects.len() == 1
        && subjects[0].get("name").and_then(Value::as_str) == Some(expected_name.as_str())
        && subjects[0]
            .pointer("/digest/sha256")
            .and_then(Value::as_str)
            == Some(digest)
}

/// Validate the signed data's structure and binding, not its signature.
/// Signature and inclusion verification remain separate mandatory steps.
pub(super) fn decode(
    spec: &KarsReceiptSpec,
    namespace: &str,
    task_name: &str,
) -> Result<BoundStatement, String> {
    if spec.scheme != SCHEME || spec.dsse.payload_type != PAYLOAD_TYPE {
        return Err("unsupported receipt signing scheme or payload type".into());
    }
    if spec.task_ref.name != task_name || spec.predicate_type != PREDICATE_TYPE {
        return Err("receipt task reference or predicate type does not match its request".into());
    }
    let payload = STANDARD
        .decode(&spec.dsse.payload)
        .map_err(|_| "receipt payload is not valid base64")?;
    let statement: Value =
        serde_json::from_slice(&payload).map_err(|_| "receipt payload is not valid JSON")?;
    if statement.get("_type").and_then(Value::as_str) != Some(STATEMENT_TYPE)
        || statement.get("predicateType").and_then(Value::as_str) != Some(PREDICATE_TYPE)
    {
        return Err("signed statement has an unsupported type or predicate".into());
    }
    if !binds_subject(&statement, spec, namespace, task_name) {
        return Err("signed subject does not bind this exact namespace, task and envelope".into());
    }
    let claims: Vec<ReceiptClaim> = serde_json::from_value(
        statement
            .pointer("/predicate/claims")
            .cloned()
            .ok_or("signed predicate has no claim matrix")?,
    )
    .map_err(|_| "signed claim matrix is malformed")?;
    if !spec.claims.is_empty()
        && (spec.claims.len() != claims.len()
            || spec.claims.iter().zip(&claims).any(|(echo, signed)| {
                echo.class != signed.class
                    || echo.status != signed.status
                    || echo.detail != signed.detail
            }))
    {
        return Err("unsigned receipt claims contradict the signed predicate".into());
    }
    Ok(BoundStatement {
        payload,
        statement,
        claims,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kars::receipt::{DsseEnvelope, DsseSignature};
    use crate::kars::task::LocalObjectRef;
    use crate::providers::receipt::{ReceiptTestSigner as SigningKey, verify_ed25519};
    use serde_json::json;

    fn receipt() -> (KarsReceiptSpec, SigningKey) {
        let key = SigningKey::from_bytes(&[42; 32]);
        let claims = vec![ReceiptClaim {
            class: "completeness".into(),
            status: "PARTIAL".into(),
            detail: "Not all controls were independently observed.".into(),
        }];
        let payload = serde_json::to_vec(&json!({
            "_type": STATEMENT_TYPE,
            "predicateType": PREDICATE_TYPE,
            "subject": [{
                "name": "tenant/task",
                "digest": {"sha256": "0123456789abcdef0123456789abcdef"}
            }],
            "predicate": {"claims": claims}
        }))
        .unwrap();
        let signature = key.sign(&super::super::pae(PAYLOAD_TYPE, &payload));
        (
            KarsReceiptSpec {
                task_ref: LocalObjectRef {
                    name: "task".into(),
                },
                envelope_digest: "sha256:0123456789abcdef0123456789abcdef".into(),
                predicate_type: PREDICATE_TYPE.into(),
                scheme: SCHEME.into(),
                key_id: "test-key".into(),
                dsse: DsseEnvelope {
                    payload: STANDARD.encode(payload),
                    payload_type: PAYLOAD_TYPE.into(),
                    signatures: vec![DsseSignature {
                        keyid: "test-key".into(),
                        sig: STANDARD.encode(signature),
                    }],
                },
                claims,
            },
            key,
        )
    }

    #[test]
    fn valid_signed_claims_are_the_projection_source() {
        let (mut spec, _) = receipt();
        assert_eq!(
            decode(&spec, "tenant", "task").unwrap().claims[0].status,
            "PARTIAL"
        );
        spec.claims.clear();
        assert_eq!(
            decode(&spec, "tenant", "task").unwrap().claims[0].status,
            "PARTIAL"
        );
    }

    #[test]
    fn a_valid_signature_does_not_authenticate_a_forged_unsigned_pass() {
        let (mut spec, key) = receipt();
        spec.claims[0].status = "PASS".into();
        let payload = STANDARD.decode(&spec.dsse.payload).unwrap();
        assert!(verify_ed25519(
            &key.public_key(),
            &super::super::pae(PAYLOAD_TYPE, &payload),
            &spec.dsse.signatures[0].sig,
        ));
        assert!(decode(&spec, "tenant", "task").is_err());
    }

    #[test]
    fn an_envelope_digest_appearing_in_unrelated_text_is_not_a_binding() {
        let (mut spec, _) = receipt();
        let mut payload: Value =
            serde_json::from_slice(&STANDARD.decode(&spec.dsse.payload).unwrap()).unwrap();
        payload["subject"][0]["digest"]["sha256"] = json!("wrong");
        payload["predicate"]["note"] = json!(spec.envelope_digest);
        spec.dsse.payload = STANDARD.encode(serde_json::to_vec(&payload).unwrap());
        assert!(decode(&spec, "tenant", "task").is_err());
    }

    #[test]
    fn rejects_cross_namespace_and_cross_task_replays() {
        let (mut spec, _) = receipt();
        assert!(decode(&spec, "other-tenant", "task").is_err());
        assert!(decode(&spec, "tenant", "other-task").is_err());
        spec.task_ref.name = "other-task".into();
        assert!(decode(&spec, "tenant", "other-task").is_err());
    }

    #[test]
    fn rejects_empty_digests_and_unsupported_envelope_types() {
        let (mut spec, _) = receipt();
        spec.envelope_digest.clear();
        assert!(decode(&spec, "tenant", "task").is_err());
        let (mut spec, _) = receipt();
        spec.dsse.payload_type = "text/plain".into();
        assert!(decode(&spec, "tenant", "task").is_err());
        let (mut spec, _) = receipt();
        spec.scheme = "DSSEv1-unknown".into();
        assert!(decode(&spec, "tenant", "task").is_err());
    }

    #[test]
    fn rejects_malformed_payloads_instead_of_using_unsigned_claims() {
        let (mut spec, _) = receipt();
        for payload in ["not base64", "e30="] {
            spec.dsse.payload = payload.into();
            assert!(decode(&spec, "tenant", "task").is_err());
        }
    }
}
