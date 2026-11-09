// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Request state shared by routing, candidate rewrites and upstream preparation.
//! Ingress supplies its already validated JSON; translators supply their generated
//! JSON. Only the public byte-based forwarding APIs need the compatibility parser.
//! Original bytes remain available for tolerant, non-object/malformed fallbacks.

use bytes::Bytes;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct RequestBody {
    value: Option<Value>,
    original: Option<Bytes>,
}

impl RequestBody {
    /// Compatibility boundary for byte callers, not an ingress validator. These
    /// callers may supply generated bodies larger than the HTTP ingress limit.
    pub(crate) fn from_bytes(body: Bytes) -> Self {
        let value = serde_json::from_slice::<Value>(&body).ok();
        Self {
            value,
            original: Some(body),
        }
    }

    pub(crate) fn unparsed(body: Bytes) -> Self {
        Self {
            value: None,
            original: Some(body),
        }
    }

    pub(crate) fn from_validated(body: Bytes, value: Value) -> Self {
        Self {
            value: Some(value),
            original: Some(body),
        }
    }

    pub(crate) fn from_value(value: Value) -> Self {
        // Translation can add a container around an already near-limit value.
        // Previously the next serde reader treated that serialization as opaque.
        // Preserve its raw forwarding fallback, without reparsing or a byte cap.
        if !within_reader_depth(&value, 128) {
            return Self::unparsed(Bytes::from(
                serde_json::to_vec(&value).expect("JSON request value serializes"),
            ));
        }
        Self {
            value: Some(value),
            original: None,
        }
    }

    pub(crate) fn json(&self) -> Option<&Value> {
        self.value.as_ref()
    }

    pub(crate) fn json_mut(&mut self) -> Option<&mut Value> {
        if self.value.is_some() {
            self.original = None;
        }
        self.value.as_mut()
    }

    pub(crate) fn with_model(&self, deployment: &str) -> Self {
        let mut body = self.clone();
        if body.json().is_some_and(Value::is_object) {
            body.json_mut().unwrap()["model"] = deployment.into();
        }
        body
    }

    /// Preserve untouched byte representations, including translator rollback.
    pub(crate) fn to_bytes(&self) -> serde_json::Result<Bytes> {
        match &self.original {
            Some(bytes) => Ok(bytes.clone()),
            None => serde_json::to_vec(self.value.as_ref().expect("request has JSON or bytes"))
                .map(Bytes::from),
        }
    }

    /// Upstream preparation has always normalized all valid JSON, even scalars.
    pub(crate) fn into_upstream_bytes(self) -> serde_json::Result<Bytes> {
        match self.value {
            Some(value) => serde_json::to_vec(&value).map(Bytes::from),
            None => Ok(self.original.expect("unparsed request retains its bytes")),
        }
    }
}

// serde_json's standard reader decrements its 128-container budget before
// entering each array or object, including empty containers.
fn within_reader_depth(value: &Value, remaining: usize) -> bool {
    match value {
        Value::Array(values) => {
            remaining > 1
                && values
                    .iter()
                    .all(|value| within_reader_depth(value, remaining - 1))
        }
        Value::Object(values) => {
            remaining > 1
                && values
                    .values()
                    .all(|value| within_reader_depth(value, remaining - 1))
        }
        _ => true,
    }
}

impl From<Bytes> for RequestBody {
    fn from(body: Bytes) -> Self {
        Self::from_bytes(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mutation_fallback_preserves_raw_bytes_and_upstream_normalizes_valid_scalars() {
        for raw in ["{broken", " [1, 2] \n", " null ", " 1e1 ", " \"text\" "] {
            let original = Bytes::copy_from_slice(raw.as_bytes());
            let body = RequestBody::from_bytes(original.clone());
            let candidate = body.with_model("fallback");
            assert_eq!(candidate.to_bytes().unwrap(), original);
            let expected = match body.json() {
                Some(value) => Bytes::from(serde_json::to_vec(value).unwrap()),
                None => original,
            };
            assert_eq!(candidate.into_upstream_bytes().unwrap(), expected);
        }
    }

    #[test]
    fn generated_depth_fallback_matches_the_standard_serde_reader() {
        for depth in [0, 1, 126, 127, 128, 129] {
            for object in [false, true] {
                for empty in [false, true] {
                    let mut value = if empty {
                        if object { json!({}) } else { json!([]) }
                    } else {
                        json!(0)
                    };
                    for _ in 0..depth {
                        value = if object {
                            json!({"nested":value})
                        } else {
                            json!([value])
                        };
                    }
                    let bytes = Bytes::from(serde_json::to_vec(&value).unwrap());
                    let parsed = serde_json::from_slice::<Value>(&bytes).ok();
                    let retained = RequestBody::from_value(value);
                    assert_eq!(
                        retained.json(),
                        parsed.as_ref(),
                        "depth={depth}, object={object}, empty={empty}"
                    );
                    assert_eq!(retained.into_upstream_bytes().unwrap(), bytes);
                }
            }
        }
    }

    #[test]
    fn failed_ingress_parse_is_not_retried_at_a_compatibility_boundary() {
        let original = Bytes::from_static(b"{\"model\":\"must-remain-opaque\"}");
        let body = RequestBody::unparsed(original.clone()).with_model("fallback");
        assert!(body.json().is_none());
        assert_eq!(body.into_upstream_bytes().unwrap(), original);
    }

    #[test]
    fn retained_generated_and_legacy_requests_have_isolated_candidate_state() {
        let original = Bytes::from_static(b" {\"model\":\"first\", \"model\":\"selected\"} \n");
        let parsed: Value = serde_json::from_slice(&original).unwrap();
        let generated = json!({"model":"selected", "extension":"x".repeat(2 * 1024 * 1024)});
        for body in [
            RequestBody::from_validated(original.clone(), parsed),
            RequestBody::from_bytes(original.clone()),
            RequestBody::from_value(generated),
        ] {
            let before = body.to_bytes().unwrap();
            let mut first = body.with_model("first-attempt");
            first.json_mut().unwrap()["stream_options"] = json!({"include_usage":true});
            let second = body.with_model("second-attempt");
            assert_eq!(first.json().unwrap()["model"], "first-attempt");
            assert_eq!(second.json().unwrap()["model"], "second-attempt");
            assert!(second.json().unwrap().get("stream_options").is_none());
            assert_eq!(body.to_bytes().unwrap(), before);
            if body.json().unwrap().get("extension").is_some() {
                assert!(second.to_bytes().unwrap().len() > 2 * 1024 * 1024);
                assert_eq!(
                    first.json().unwrap()["extension"],
                    second.json().unwrap()["extension"]
                );
            }
        }
    }
}
