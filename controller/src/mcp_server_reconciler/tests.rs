// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;

#[derive(Debug)]
struct MockOk;
#[async_trait::async_trait]
impl JwksFetcher for MockOk {
    async fn fetch(&self, _: &str) -> Result<FetchedJwks, FetchError> {
        let raw = br#"{"keys":[{"kty":"OKP","crv":"Ed25519","kid":"k1","x":"AAA"}]}"#.to_vec();
        Ok(FetchedJwks { raw, key_count: 1 })
    }
}

#[derive(Debug)]
struct MockFailDns;
#[async_trait::async_trait]
impl JwksFetcher for MockFailDns {
    async fn fetch(&self, _: &str) -> Result<FetchedJwks, FetchError> {
        Err(FetchError::Discovery {
            class: "dns",
            detail: "name resolution failed".into(),
        })
    }
}

#[test]
fn parse_jwks_key_count_works() {
    assert_eq!(
        parse_jwks_key_count(br#"{"keys":[{"kid":"a"},{"kid":"b"}]}"#).unwrap(),
        2
    );
    assert!(parse_jwks_key_count(br#"{"foo":"bar"}"#).is_err());
}

#[test]
fn kid_from_public_is_deterministic_and_short() {
    let public = [0u8; 32];
    let kid = kid_from_public_bytes(&public);
    assert_eq!(kid.len(), 22);
    assert_eq!(kid, kid_from_public_bytes(&public));
    assert!(!kid.contains('='));
}

#[test]
fn build_conditions_emits_three_types_on_success() {
    let conditions = build_conditions(&[], Some(7), None);
    assert_eq!(conditions.len(), 3);
    for (name, expected) in [
        ("Ready", "True"),
        ("Progressing", "False"),
        ("Degraded", "False"),
    ] {
        let condition = conditions
            .iter()
            .find(|condition| condition.type_ == name)
            .unwrap();
        assert_eq!(condition.status, expected);
        assert_eq!(condition.observed_generation, Some(7));
    }
}

#[test]
fn build_conditions_emits_degraded_true_on_failure() {
    let conditions = build_conditions(&[], Some(2), Some(("JwksFetchFailed", "boom")));
    let ready = conditions
        .iter()
        .find(|condition| condition.type_ == "Ready")
        .unwrap();
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, "JwksFetchFailed");
    let degraded = conditions
        .iter()
        .find(|condition| condition.type_ == "Degraded")
        .unwrap();
    assert_eq!(degraded.status, "True");
    assert_eq!(degraded.message, "boom");
}

#[test]
fn build_conditions_preserves_transition_time_on_repeat_success() {
    let prior = build_conditions(&[], Some(1), None);
    std::thread::sleep(Duration::from_millis(5));
    let next = build_conditions(&prior, Some(1), None);
    assert_eq!(prior[0].last_transition_time, next[0].last_transition_time);
}

#[test]
fn build_conditions_stamps_new_time_on_status_flip() {
    let prior = build_conditions(&[], Some(1), None);
    std::thread::sleep(Duration::from_millis(5));
    let next = build_conditions(&prior, Some(1), Some(("JwksFetchFailed", "x")));
    assert_ne!(prior[0].last_transition_time, next[0].last_transition_time);
}

#[test]
fn fetch_error_class_buckets_are_safe_strings() {
    for error in [
        FetchError::Discovery {
            class: "dns",
            detail: "x".into(),
        },
        FetchError::Discovery {
            class: "tls",
            detail: "x".into(),
        },
        FetchError::Discovery {
            class: "timeout",
            detail: "x".into(),
        },
        FetchError::Discovery {
            class: "http_status",
            detail: "x".into(),
        },
        FetchError::Jwks {
            class: "tls",
            detail: "x".into(),
        },
        FetchError::InvalidJwks("x".into()),
    ] {
        assert!(matches!(
            error.class(),
            "dns" | "tls" | "timeout" | "http_status" | "invalid_jwks_format"
        ));
    }
}

#[test]
fn mock_fetchers_compile_and_do_not_panic() {
    let _ok: Arc<dyn JwksFetcher> = Arc::new(MockOk);
    let _fail: Arc<dyn JwksFetcher> = Arc::new(MockFailDns);
}

#[tokio::test]
async fn mock_ok_returns_one_key() {
    assert_eq!(MockOk.fetch("https://example").await.unwrap().key_count, 1);
}

#[tokio::test]
async fn mock_fail_dns_classifies() {
    assert_eq!(
        MockFailDns
            .fetch("https://example")
            .await
            .unwrap_err()
            .class(),
        "dns"
    );
}
