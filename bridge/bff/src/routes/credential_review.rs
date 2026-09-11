use axum::{
    Json,
    extract::{Extension, State},
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::operator::{CredentialRequest, credential_write_error, is_dns1123_label, is_env_key};
use crate::{
    auth::Principal,
    error::{AppError, AppResult},
    kars::credential_review::{CredentialReview, ReviewedWrite, StoredSource},
    state::AppState,
};

const AUDIENCE: &str = "kars-bridge/credential-review/v1";
const VALUE_AUDIENCE: &str = "kars-bridge/credential-write-intent/v1";
const LIFETIME: i64 = 300;
const MAX_SUBMISSIONS: u8 = 3;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
enum Purpose {
    Review,
    Continuation,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    aud: String,
    sub: String,
    exp: i64,
    purpose: Purpose,
    submission: u8,
    review: CredentialReview,
    stored: Option<StoredSource>,
    value_tag: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialContinuation {
    pub token: String,
    pub source: Option<StoredSource>,
    pub outcome: String,
}

impl std::fmt::Debug for CredentialContinuation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialContinuation([redacted])")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewRequest {
    pub namespace: String,
    pub kind: String,
    pub target: String,
    pub target_uid: Option<String>,
    pub key: String,
    pub continuation: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResponse {
    pub token: String,
    pub metadata: CredentialReview,
    pub expires_at: i64,
    pub submission: u8,
    pub continuation: bool,
    pub binding_only: bool,
}

fn conflict() -> AppError {
    AppError::Conflict("Credential review changed, expired, or does not match this operator and write intent. No automatic retry is permitted.".into())
}

fn require_operator(principal: &Principal) -> AppResult<()> {
    if principal.sub.is_empty()
        || !principal
            .roles
            .iter()
            .any(|role| role == "operator" || role == "admin")
    {
        return Err(AppError::Forbidden(
            "An operator is required for credential review".into(),
        ));
    }
    Ok(())
}

fn signing_key(state: &AppState) -> AppResult<Vec<u8>> {
    let secret = state
        .principal_secret()
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| {
            AppError::Forbidden(
                "Signed operator sessions are required for credential review".into(),
            )
        })?;
    let mut hash = Sha256::new();
    hash.update(b"kars-bridge/credential-review-signing-key/v1\0");
    hash.update(secret.as_bytes());
    Ok(hash.finalize().to_vec())
}

fn sign(key: &[u8], claims: &Claims) -> AppResult<String> {
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err(conflict());
    }
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(key),
    )
    .map_err(|_| AppError::Upstream("Credential review signing failed".into()))
}

fn verified(key: &[u8], token: &str, principal: &Principal, purpose: Purpose) -> AppResult<Claims> {
    if token.len() > 32768 {
        return Err(conflict());
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&[AUDIENCE]);
    validation.leeway = 0;
    let claims = decode::<Claims>(token, &DecodingKey::from_secret(key), &validation)
        .map_err(|_| conflict())?
        .claims;
    let now = chrono::Utc::now().timestamp();
    if claims.sub != principal.sub
        || claims.purpose != purpose
        || claims.exp <= now
        || claims.exp > now + LIFETIME
        || claims.submission == 0
        || claims.submission > MAX_SUBMISSIONS
    {
        return Err(conflict());
    }
    Ok(claims)
}

fn value_tag(
    key: &[u8],
    claims: &Claims,
    source: Option<&StoredSource>,
    value: &str,
) -> AppResult<String> {
    // Only the HS256 signature leaves this function, never the value-bearing payload.
    let token = encode(&Header::new(Algorithm::HS256), &serde_json::json!({
        "aud": VALUE_AUDIENCE, "sub": claims.sub, "exp": claims.exp,
        "namespace": claims.review.target.namespace, "kind": claims.review.target.kind,
        "target": claims.review.target.name, "key": claims.review.key,
        "sourceUid": source.map(|source| source.uid.as_str()).or(claims.review.source.uid.as_deref()),
        "sourceVersion": source.map(|source| source.version.as_str()).or(claims.review.source.version.as_deref()),
        "value": value,
    }), &EncodingKey::from_secret(key)).map_err(|_| conflict())?;
    token
        .rsplit_once('.')
        .map(|(_, signature)| signature.to_string())
        .ok_or_else(conflict)
}

fn equal_tag(first: &str, second: &str) -> bool {
    first.len() == second.len()
        && first
            .as_bytes()
            .iter()
            .zip(second.as_bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn validate_input(input: &ReviewRequest) -> AppResult<()> {
    if !is_dns1123_label(&input.namespace)
        || !is_dns1123_label(&input.target)
        || !is_env_key(&input.key)
        || !["KarsSandbox", "KarsTask", "KarsTeam"].contains(&input.kind.as_str())
    {
        return Err(AppError::BadRequest(
            "An explicit credential workspace, target kind/name and key are required".into(),
        ));
    }
    Ok(())
}

fn matches_input(claims: &Claims, input: &ReviewRequest) -> bool {
    claims.review.target.namespace == input.namespace
        && claims.review.target.kind == input.kind
        && claims.review.target.name == input.target
        && claims.review.key == input.key
        && input
            .target_uid
            .as_ref()
            .is_none_or(|uid| claims.review.target.uid.as_ref() == Some(uid))
}

pub async fn review(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<ReviewRequest>,
) -> AppResult<Json<ReviewResponse>> {
    require_operator(&principal)?;
    validate_input(&input)?;
    let key = signing_key(&state)?;
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let claims = if let Some(token) = &input.continuation {
        let mut claims = verified(&key, token, &principal, Purpose::Continuation)?;
        if !matches_input(&claims, &input) {
            return Err(conflict());
        }
        if claims.value_tag.is_none() {
            return Err(conflict());
        }
        let current = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            if let Some(stored) = &claims.stored {
                cluster
                    .review_stored_credentials(&claims.review, stored)
                    .await
            } else {
                cluster.review_unwritten_credentials(&claims.review).await
            }
        })
        .await
        .map_err(|_| AppError::Upstream("Credential metadata review deadline".into()))?
        .map_err(credential_write_error)?;
        claims.review = current;
        claims.purpose = Purpose::Review;
        claims
    } else {
        let review = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            cluster.review_credentials(&input.namespace, &input.kind, &input.target, &input.key),
        )
        .await
        .map_err(|_| AppError::Upstream("Credential metadata review deadline".into()))?
        .map_err(credential_write_error)?;
        if input
            .target_uid
            .as_ref()
            .is_some_and(|uid| review.target.uid.as_ref() != Some(uid))
        {
            return Err(conflict());
        }
        Claims {
            aud: AUDIENCE.into(),
            sub: principal.sub,
            exp: chrono::Utc::now().timestamp() + LIFETIME,
            purpose: Purpose::Review,
            submission: 1,
            review,
            stored: None,
            value_tag: None,
        }
    };
    let response = ReviewResponse {
        token: sign(&key, &claims)?,
        metadata: claims.review,
        expires_at: claims.exp,
        submission: claims.submission,
        continuation: claims.value_tag.is_some(),
        binding_only: claims.stored.is_some(),
    };
    Ok(Json(response))
}

pub(super) async fn write(
    state: &AppState,
    principal: &Principal,
    input: CredentialRequest,
) -> AppResult<Json<serde_json::Value>> {
    require_operator(principal)?;
    let key = signing_key(state)?;
    let token = input.review.as_deref().ok_or_else(conflict)?;
    let claims = verified(&key, token, principal, Purpose::Review)?;
    if claims.review.target.namespace != input.namespace
        || claims.review.target.kind != input.kind
        || claims.review.target.name != input.target.trim()
        || claims.review.key != input.key.trim()
        || claims.review.target.uid.as_deref() != input.target_uid.as_deref()
    {
        return Err(conflict());
    }
    if let Some(expected) = &claims.value_tag {
        let actual = value_tag(&key, &claims, claims.stored.as_ref(), &input.value)?;
        if !equal_tag(expected, &actual) {
            return Err(conflict());
        }
    } else if claims.stored.is_some() {
        return Err(conflict());
    }
    let cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    let write = ReviewedWrite {
        review: claims.review.clone(),
        stored: claims.stored.clone(),
    };
    let value = input.value;
    let remaining = (claims.exp - chrono::Utc::now().timestamp()).clamp(0, 30);
    if remaining == 0 {
        return Err(conflict());
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(remaining as u64),
        cluster.write_reviewed_agent_credentials(&write, value.clone()),
    )
    .await
    .map_err(|_| {
        AppError::Upstream(
            "Credential write deadline; outcome is uncertain and cannot be automatically resumed"
                .into(),
        )
    })?;
    match result {
        Ok(result) => Ok(Json(result)),
        Err(failure) => {
            let failure = *failure;
            if matches!(&failure.error, kube::Error::Api(status) if status.code == 409)
                && claims.submission < MAX_SUBMISSIONS
                && (failure.stored.is_some() || !failure.write_attempted)
            {
                let source = failure.stored;
                let tag = value_tag(&key, &claims, source.as_ref(), &value)?;
                let continuation = Claims {
                    purpose: Purpose::Continuation,
                    submission: claims.submission + 1,
                    stored: source.clone(),
                    value_tag: Some(tag),
                    ..claims
                };
                return Err(AppError::CredentialConflict(Box::new(
                    CredentialContinuation {
                        token: sign(&key, &continuation)?,
                        outcome: if source.is_some() {
                            "source-stored"
                        } else {
                            "no-write-attempted"
                        }
                        .into(),
                        source,
                    },
                )));
            }
            Err(credential_write_error(failure.error))
        }
    }
}
