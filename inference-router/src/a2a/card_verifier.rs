//! Inbound A2A AgentCard verification — the symmetric counterpart to
//! [`card_server`](super::card_server).
//!
//! When a remote A2A peer initiates a call against our `azureclaw-a2a-gateway`
//! (forthcoming) or hands us a card during JSON-RPC method dispatch, we must
//! verify it before trusting any claim it makes. This module performs that
//! verification end-to-end as a **pure function** — caller-fetched bytes in,
//! `VerifiedCallerIdentity` (or structured error) out — so all threat-model
//! cases can be exhausted in unit tests before any byte reaches the network.
//!
//! ## What this module enforces
//!
//! 1. **Strict JSON parse.** Tolerant deserialisation only on the wrapping
//!    envelope — every field type pin is enforced by `AgentCard`'s serde
//!    derive. Malformed → [`CardVerifyError::Parse`].
//! 2. **Protocol-version pin.** A2A spec §4.4.1 places the version inside
//!    `version` (the *agent* version, not the protocol version). The protocol
//!    version, when present, lives at the JSON top level under
//!    `protocolVersion`. Currently pinned to [`A2A_PROTOCOL_VERSION`]. We
//!    accept absence (some reference impls omit) but reject any present-but-
//!    differing value (downgrade defence).
//! 3. **Required-field shape.** `name`, `version`, `capabilities` MUST exist
//!    and be non-empty (see [`CardVerifyError::EmptyRequiredField`]).
//! 4. **Trusted signature.** Delegates to
//!    [`super::card_signing::verify_card`]; rejects unsigned cards
//!    ([`CardVerifyError::Unsigned`]) and cards whose every `kid` is unknown
//!    or whose every signature failed verification.
//! 5. **URL claim binding (optional).** When the verifier is given an
//!    expected URL prefix (e.g. the SNI the TLS connection arrived on), the
//!    card's `provider.url` (or top-level `url`) MUST start with it. Mitigates
//!    the "verified-but-misbinding" class — a card validly signed by a
//!    trusted key for a *different* origin shouldn't be accepted on this
//!    connection.
//! 6. **Freshness window (optional).** When the card includes the optional
//!    A2A 1.0 `validFrom` / `validUntil` (RFC 3339), they are honoured
//!    against caller-supplied `now`. Skewed-clock attacks bounded by the
//!    caller's wall-clock provider.
//!
//! ## What this module is NOT
//!
//! - Not a network fetcher. Bytes are caller-supplied. The forthcoming
//!   `azureclaw-a2a-gateway` daemon is the I/O layer.
//! - Not the JSON-RPC method dispatcher. `message/send`, `tasks/get`,
//!   `tasks/cancel` belong to `phase1/a2a-jsonrpc-dispatch`.
//! - Not the AP2 commerce extension validator. AP2 mandates land in
//!   `phase1/a2a-ap2-mandates`.
//!
//! ## Threat model
//!
//! See `docs/security-audits/2026-04-25-phase1-a2a-card-verifier.md`.

use std::collections::HashMap;
use std::time::SystemTime;

use ed25519_dalek::VerifyingKey;
use serde::Deserialize;

use super::agent_card::{A2A_PROTOCOL_VERSION, AgentCard};
use super::card_signing::{self, CardSignError};

/// A successfully verified inbound card, plus the kid that signed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCallerIdentity {
    /// The `kid` of the signature that passed verification. Useful for
    /// downstream audit + replay-detection lookups.
    pub kid: String,
    /// The agent's claimed name (`AgentCard.name`).
    pub name: String,
    /// The agent's claimed version (`AgentCard.version`).
    pub version: String,
    /// The verified provider URL, if the card carried one.
    pub provider_url: Option<String>,
}

/// Errors the inbound verification pipeline raises. Each is a distinct
/// observable failure mode — callers branch on the variant for audit
/// emission and HTTP status mapping.
#[derive(Debug, thiserror::Error)]
pub enum CardVerifyError {
    /// JSON envelope parse failed. Includes serde error string for
    /// audit; never includes raw input bytes (which may be hostile).
    #[error("parse agent card: {0}")]
    Parse(String),

    /// Required field was present but empty (e.g. `name = ""`).
    #[error("required field `{0}` is empty")]
    EmptyRequiredField(&'static str),

    /// Card declared a `protocolVersion` not equal to the version we
    /// pin. Defence against downgrade attacks against future versions
    /// of A2A that retire methods.
    #[error("protocol version mismatch: expected `{expected}`, card declared `{got}`")]
    ProtocolVersionMismatch { expected: String, got: String },

    /// Card carried no `signatures` field.
    #[error("card is unsigned")]
    Unsigned,

    /// Underlying JWS verification failed. Wraps the structured error
    /// from [`card_signing`] without flattening the variant — callers
    /// can match on it for fine-grained handling.
    #[error("signature verification failed: {0}")]
    Signature(#[from] CardSignError),

    /// Caller supplied an `expected_url_prefix` and the card's
    /// `provider.url` (or top-level `url`) doesn't match. Defence
    /// against verified-but-misbinding cards.
    #[error("provider url `{got}` does not match expected prefix `{expected}`")]
    UrlPrefixMismatch { expected: String, got: String },

    /// Card carried `validFrom` and the supplied `now` was earlier.
    #[error("card not yet valid: validFrom={valid_from}, now={now}")]
    NotYetValid { valid_from: i64, now: i64 },

    /// Card carried `validUntil` and the supplied `now` was after.
    #[error("card expired: validUntil={valid_until}, now={now}")]
    Expired { valid_until: i64, now: i64 },

    /// Card claimed a freshness field but it didn't parse as RFC 3339.
    #[error("malformed freshness field `{field}`: {reason}")]
    MalformedFreshness { field: &'static str, reason: String },
}

/// Configuration for the inbound verifier. Each field carries a doc-
/// comment describing its security significance — keep that doc as
/// the source of truth.
pub struct CardVerifierConfig<'a> {
    /// `kid → VerifyingKey` map of trust anchors. Empty map ⇒ *every*
    /// card fails (defence against misconfiguration shipping with no
    /// trust anchors and silently accepting everything).
    pub trusted_keys: HashMap<&'a str, &'a VerifyingKey>,

    /// When `Some`, the card's provider/top-level URL MUST start with
    /// this prefix. Set this to the TLS-SNI / Host header / mTLS-SAN
    /// so a valid card can't be replayed across origins.
    pub expected_url_prefix: Option<&'a str>,

    /// Caller-supplied wall clock (UNIX seconds). Used for
    /// `validFrom` / `validUntil` enforcement when present.
    pub now: SystemTime,
}

/// Top-level envelope used only to peek at `protocolVersion`. Many
/// reference impls omit it; we accept absence but reject divergence.
#[derive(Debug, Deserialize)]
struct ProtocolPeek {
    #[serde(rename = "protocolVersion")]
    protocol_version: Option<String>,
    #[serde(rename = "validFrom")]
    valid_from: Option<String>,
    #[serde(rename = "validUntil")]
    valid_until: Option<String>,
    #[serde(rename = "url")]
    url: Option<String>,
}

/// Verify an inbound A2A AgentCard. Pure / total / synchronous.
///
/// `raw` is the JSON bytes the caller fetched from the remote peer's
/// `/.well-known/agent.json` (or equivalent). `config` carries the
/// trust anchors + binding constraints + clock.
///
/// Returns [`VerifiedCallerIdentity`] on success — the caller may
/// then bind that identity to the connection / JSON-RPC session.
pub fn verify_inbound_card(
    raw: &[u8],
    config: &CardVerifierConfig<'_>,
) -> Result<VerifiedCallerIdentity, CardVerifyError> {
    // 1. Peek for protocol-version + freshness fields BEFORE strict parse.
    //    `AgentCard` doesn't currently carry these as typed fields, so we
    //    sniff them from the same bytes.
    let peek: ProtocolPeek = serde_json::from_slice(raw)
        .map_err(|e| CardVerifyError::Parse(format!("envelope peek: {e}")))?;

    if let Some(declared) = peek.protocol_version.as_deref() {
        if declared != A2A_PROTOCOL_VERSION {
            return Err(CardVerifyError::ProtocolVersionMismatch {
                expected: A2A_PROTOCOL_VERSION.to_string(),
                got: declared.to_string(),
            });
        }
    }

    // 2. Strict parse of the typed shape.
    let card: AgentCard = serde_json::from_slice(raw)
        .map_err(|e| CardVerifyError::Parse(format!("agent card: {e}")))?;

    // 3. Required-field non-empty checks.
    if card.name.trim().is_empty() {
        return Err(CardVerifyError::EmptyRequiredField("name"));
    }
    if card.version.trim().is_empty() {
        return Err(CardVerifyError::EmptyRequiredField("version"));
    }

    // 4. Freshness window (optional).
    if peek.valid_from.is_some() || peek.valid_until.is_some() {
        let now_secs = config
            .now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| CardVerifyError::MalformedFreshness {
                field: "now",
                reason: e.to_string(),
            })?
            .as_secs() as i64;

        if let Some(vf) = peek.valid_from.as_deref() {
            let parsed =
                parse_rfc3339_secs(vf).map_err(|e| CardVerifyError::MalformedFreshness {
                    field: "validFrom",
                    reason: e,
                })?;
            if now_secs < parsed {
                return Err(CardVerifyError::NotYetValid {
                    valid_from: parsed,
                    now: now_secs,
                });
            }
        }
        if let Some(vu) = peek.valid_until.as_deref() {
            let parsed =
                parse_rfc3339_secs(vu).map_err(|e| CardVerifyError::MalformedFreshness {
                    field: "validUntil",
                    reason: e,
                })?;
            if now_secs > parsed {
                return Err(CardVerifyError::Expired {
                    valid_until: parsed,
                    now: now_secs,
                });
            }
        }
    }

    // 5. Signature: requires `signatures` non-empty + at least one
    //    signature trusted+valid.
    let sigs = card.signatures.as_ref().ok_or(CardVerifyError::Unsigned)?;
    if sigs.is_empty() {
        return Err(CardVerifyError::Unsigned);
    }
    if config.trusted_keys.is_empty() {
        // Defence against misconfig: empty trust store should never
        // accept anything. card_signing::verify_card already returns
        // NoTrustedSignatureValid here, but make it explicit.
        return Err(CardVerifyError::Signature(
            CardSignError::NoTrustedSignatureValid,
        ));
    }
    let kid = card_signing::verify_card(&card, &config.trusted_keys)?;

    // 6. URL binding (optional). Prefer provider.url, fall back to
    //    top-level url (peek.url) when no provider.
    let provider_url = card.provider.as_ref().map(|p| p.url.clone()).or(peek.url);
    if let Some(prefix) = config.expected_url_prefix {
        let got = provider_url.as_deref().unwrap_or("");
        if !got.starts_with(prefix) {
            return Err(CardVerifyError::UrlPrefixMismatch {
                expected: prefix.to_string(),
                got: got.to_string(),
            });
        }
    }

    Ok(VerifiedCallerIdentity {
        kid,
        name: card.name,
        version: card.version,
        provider_url,
    })
}

/// Tiny RFC 3339 → unix-seconds parser. We don't pull in `chrono` for
/// this module; the format is a fixed `YYYY-MM-DDTHH:MM:SSZ` (or with
/// fractional seconds and offset) so a bounded parse is trivial and
/// avoids a dep surface that could shift defaults.
fn parse_rfc3339_secs(s: &str) -> Result<i64, String> {
    // Accept either trailing `Z` or `+HH:MM` / `-HH:MM`. Reject
    // anything else (defence against malformed timestamps that some
    // libs would silently coerce).
    let (datetime_part, offset_secs) = if let Some(stripped) = s.strip_suffix('Z') {
        (stripped, 0i64)
    } else {
        // Find last '+' or '-' that's after position 10 (date-part).
        let bytes = s.as_bytes();
        let mut split = None;
        for i in (10..bytes.len()).rev() {
            if bytes[i] == b'+' || bytes[i] == b'-' {
                split = Some(i);
                break;
            }
        }
        let split = split.ok_or_else(|| format!("missing tz suffix in `{s}`"))?;
        let (lhs, off) = s.split_at(split);
        // off is "+HH:MM" or "-HH:MM"
        if off.len() != 6 || (off.as_bytes()[0] != b'+' && off.as_bytes()[0] != b'-') {
            return Err(format!("malformed offset `{off}`"));
        }
        let sign = if off.as_bytes()[0] == b'+' {
            1i64
        } else {
            -1i64
        };
        let hh: i64 = off[1..3]
            .parse()
            .map_err(|e| format!("offset hours: {e}"))?;
        let mm: i64 = off[4..6]
            .parse()
            .map_err(|e| format!("offset minutes: {e}"))?;
        (lhs, sign * (hh * 3600 + mm * 60))
    };

    // Drop fractional seconds — we only care about whole seconds.
    let datetime_part = match datetime_part.find('.') {
        Some(i) => &datetime_part[..i],
        None => datetime_part,
    };

    if datetime_part.len() < 19 {
        return Err(format!("datetime too short: `{datetime_part}`"));
    }
    let year: i64 = datetime_part[0..4]
        .parse()
        .map_err(|e| format!("year: {e}"))?;
    let month: i64 = datetime_part[5..7]
        .parse()
        .map_err(|e| format!("month: {e}"))?;
    let day: i64 = datetime_part[8..10]
        .parse()
        .map_err(|e| format!("day: {e}"))?;
    let hour: i64 = datetime_part[11..13]
        .parse()
        .map_err(|e| format!("hour: {e}"))?;
    let minute: i64 = datetime_part[14..16]
        .parse()
        .map_err(|e| format!("minute: {e}"))?;
    let second: i64 = datetime_part[17..19]
        .parse()
        .map_err(|e| format!("second: {e}"))?;

    // Reject obviously-bad components without pulling chrono.
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return Err(format!("out-of-range component in `{s}`"));
    }

    // Days-from-civil (Howard Hinnant): exact, branch-light.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + hour * 3600 + minute * 60 + second - offset_secs;
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::agent_card::{AgentCapabilities, AgentProvider};
    use crate::a2a::card_signing::sign_card;
    use ed25519_dalek::SigningKey;
    use std::time::Duration;

    fn fresh_kp_seeded(seed: u8) -> (SigningKey, VerifyingKey) {
        let sk = SigningKey::from_bytes(&[seed; 32]);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn fresh_kp() -> (SigningKey, VerifyingKey) {
        // Tests are deterministic — use a per-test counter via thread-local
        // to keep keys distinct without dragging in a CSPRNG.
        thread_local! {
            static SEED: std::cell::Cell<u8> = const { std::cell::Cell::new(7) };
        }
        let seed = SEED.with(|s| {
            let v = s.get();
            s.set(v.wrapping_add(1));
            v
        });
        fresh_kp_seeded(seed)
    }

    fn base_card() -> AgentCard {
        AgentCard {
            name: "test-agent".into(),
            description: "test".into(),
            supported_interfaces: vec![],
            provider: Some(AgentProvider {
                url: "https://agents.example.com/test-agent".into(),
                organization: "example".into(),
            }),
            version: "1.0.0".into(),
            documentation_url: None,
            capabilities: AgentCapabilities::default(),
            security_schemes: None,
            security_requirements: None,
            default_input_modes: vec!["text/plain".into()],
            default_output_modes: vec!["text/plain".into()],
            skills: vec![],
            signatures: None,
            icon_url: None,
        }
    }

    fn signed_card_bytes(kid: &str) -> (Vec<u8>, VerifyingKey) {
        let (sk, vk) = fresh_kp();
        let signed = sign_card(base_card(), &sk, kid).unwrap();
        (serde_json::to_vec(&signed).unwrap(), vk)
    }

    fn cfg<'a>(
        keys: HashMap<&'a str, &'a VerifyingKey>,
        url_prefix: Option<&'a str>,
        now: SystemTime,
    ) -> CardVerifierConfig<'a> {
        CardVerifierConfig {
            trusted_keys: keys,
            expected_url_prefix: url_prefix,
            now,
        }
    }

    #[test]
    fn happy_path_returns_kid_and_name() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let res = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap();
        assert_eq!(res.kid, "k1");
        assert_eq!(res.name, "test-agent");
        assert_eq!(res.version, "1.0.0");
        assert_eq!(
            res.provider_url.as_deref(),
            Some("https://agents.example.com/test-agent")
        );
    }

    #[test]
    fn empty_trust_store_rejects_even_validly_signed_card() {
        let (raw, _) = signed_card_bytes("k1");
        let keys: HashMap<&str, &VerifyingKey> = HashMap::new();
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(
            err,
            CardVerifyError::Signature(CardSignError::NoTrustedSignatureValid)
        ));
    }

    #[test]
    fn unknown_kid_rejected_as_no_trusted_signature() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("different-kid", &vk);
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(
            err,
            CardVerifyError::Signature(CardSignError::NoTrustedSignatureValid)
        ));
    }

    #[test]
    fn unsigned_card_rejected() {
        let card = base_card();
        let raw = serde_json::to_vec(&card).unwrap();
        let keys: HashMap<&str, &VerifyingKey> = HashMap::new();
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::Unsigned));
    }

    #[test]
    fn empty_signatures_array_rejected_as_unsigned() {
        let mut card = base_card();
        card.signatures = Some(vec![]);
        let raw = serde_json::to_vec(&card).unwrap();
        let keys: HashMap<&str, &VerifyingKey> = HashMap::new();
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::Unsigned));
    }

    #[test]
    fn protocol_version_mismatch_rejected() {
        // Inject a wrong protocolVersion at the JSON level.
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["protocolVersion"] = serde_json::json!("99.99");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(
            err,
            CardVerifyError::ProtocolVersionMismatch { .. }
        ));
    }

    #[test]
    fn protocol_version_absent_accepted() {
        // base happy path already omits it — re-asserting for clarity.
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap();
    }

    #[test]
    fn protocol_version_pinned_accepted() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["protocolVersion"] = serde_json::json!(A2A_PROTOCOL_VERSION);
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        // protocolVersion injection isn't covered by the signature
        // (it isn't in AgentCard), so signature still verifies.
        verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap();
    }

    #[test]
    fn empty_name_rejected() {
        let mut card = base_card();
        card.name = "".into();
        let (sk, vk) = fresh_kp();
        let signed = sign_card(card, &sk, "k1").unwrap();
        let raw = serde_json::to_vec(&signed).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::EmptyRequiredField("name")));
    }

    #[test]
    fn whitespace_only_version_rejected() {
        let mut card = base_card();
        card.version = "   ".into();
        let (sk, vk) = fresh_kp();
        let signed = sign_card(card, &sk, "k1").unwrap();
        let raw = serde_json::to_vec(&signed).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(
            err,
            CardVerifyError::EmptyRequiredField("version")
        ));
    }

    #[test]
    fn url_prefix_match_passes() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        verify_inbound_card(
            &raw,
            &cfg(keys, Some("https://agents.example.com/"), SystemTime::now()),
        )
        .unwrap();
    }

    #[test]
    fn url_prefix_mismatch_rejected() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(
            &raw,
            &cfg(
                keys,
                Some("https://attacker.example.org/"),
                SystemTime::now(),
            ),
        )
        .unwrap_err();
        assert!(matches!(err, CardVerifyError::UrlPrefixMismatch { .. }));
    }

    #[test]
    fn url_prefix_set_but_card_has_no_provider_rejected() {
        let mut card = base_card();
        card.provider = None;
        let (sk, vk) = fresh_kp();
        let signed = sign_card(card, &sk, "k1").unwrap();
        let raw = serde_json::to_vec(&signed).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(
            &raw,
            &cfg(keys, Some("https://agents.example.com/"), SystemTime::now()),
        )
        .unwrap_err();
        assert!(matches!(err, CardVerifyError::UrlPrefixMismatch { .. }));
    }

    #[test]
    fn malformed_json_returns_parse_error() {
        let raw = b"not json at all";
        let keys: HashMap<&str, &VerifyingKey> = HashMap::new();
        let err = verify_inbound_card(raw, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::Parse(_)));
    }

    #[test]
    fn rfc3339_parser_handles_z_suffix() {
        // 2026-01-01T00:00:00Z = 1767225600.
        assert_eq!(
            parse_rfc3339_secs("2026-01-01T00:00:00Z").unwrap(),
            1767225600
        );
    }

    #[test]
    fn rfc3339_parser_handles_offset() {
        // 2026-01-01T01:00:00+01:00 == 2026-01-01T00:00:00Z.
        assert_eq!(
            parse_rfc3339_secs("2026-01-01T01:00:00+01:00").unwrap(),
            1767225600
        );
    }

    #[test]
    fn rfc3339_parser_handles_fractional_seconds() {
        assert_eq!(
            parse_rfc3339_secs("2026-01-01T00:00:00.123Z").unwrap(),
            1767225600
        );
    }

    #[test]
    fn rfc3339_parser_rejects_bad_input() {
        assert!(parse_rfc3339_secs("nope").is_err());
        assert!(parse_rfc3339_secs("2026-13-01T00:00:00Z").is_err()); // month 13
        assert!(parse_rfc3339_secs("2026-01-32T00:00:00Z").is_err()); // day 32
        assert!(parse_rfc3339_secs("2026-01-01T25:00:00Z").is_err()); // hour 25
    }

    #[test]
    fn valid_until_in_past_rejected() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["validUntil"] = serde_json::json!("2020-01-01T00:00:00Z");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::Expired { .. }));
    }

    #[test]
    fn valid_from_in_future_rejected() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["validFrom"] = serde_json::json!("2099-01-01T00:00:00Z");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::NotYetValid { .. }));
    }

    #[test]
    fn malformed_valid_from_rejected() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["validFrom"] = serde_json::json!("yesterday");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(
            err,
            CardVerifyError::MalformedFreshness {
                field: "validFrom",
                ..
            }
        ));
    }

    #[test]
    fn freshness_window_passes_when_now_in_range() {
        let (raw, vk) = signed_card_bytes("k1");
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["validFrom"] = serde_json::json!("2020-01-01T00:00:00Z");
        value["validUntil"] = serde_json::json!("2099-01-01T00:00:00Z");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap();
    }

    #[test]
    fn signature_tamper_after_signing_rejected() {
        let (raw, vk) = signed_card_bytes("k1");
        // Flip a byte in the body — re-serialize to keep valid JSON.
        let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        value["name"] = serde_json::json!("evil-substituted-name");
        let raw2 = serde_json::to_vec(&value).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let err = verify_inbound_card(&raw2, &cfg(keys, None, SystemTime::now())).unwrap_err();
        assert!(matches!(err, CardVerifyError::Signature(_)));
    }

    #[test]
    fn multi_signer_cards_pass_when_any_kid_trusted() {
        // Sign with two different keys; trust only one of them.
        let (sk1, vk1) = fresh_kp();
        let (sk2, _vk2) = fresh_kp();
        let card = sign_card(base_card(), &sk1, "k1").unwrap();
        let card = sign_card(card, &sk2, "k2").unwrap();
        let raw = serde_json::to_vec(&card).unwrap();
        let mut keys = HashMap::new();
        keys.insert("k1", &vk1);
        let res = verify_inbound_card(&raw, &cfg(keys, None, SystemTime::now())).unwrap();
        assert_eq!(res.kid, "k1");
    }

    #[test]
    fn far_future_now_doesnt_panic_without_freshness_field() {
        // Caller supplies an absurd clock; without validFrom/Until,
        // verifier shouldn't care.
        let (raw, vk) = signed_card_bytes("k1");
        let mut keys = HashMap::new();
        keys.insert("k1", &vk);
        let far_future = SystemTime::UNIX_EPOCH + Duration::from_secs(99_999_999_999);
        verify_inbound_card(&raw, &cfg(keys, None, far_future)).unwrap();
    }
}
