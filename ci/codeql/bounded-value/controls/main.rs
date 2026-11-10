use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

fn case_bounded_str(input: &str) -> Option<JsonValue> {
    if input.len() > 4096 { return None; }
    serde_json::from_str(input).ok()
}
fn case_bounded_bytes(input: &[u8]) -> Option<JsonValue> {
    if input.len() > MAX_FRAME_BYTES { return None; }
    serde_json::from_slice(input).ok()
}
fn case_unbounded(input: &str) -> Option<JsonValue> {
    serde_json::from_str(input).ok()
}
fn case_after_parse(input: &str) -> Option<JsonValue> {
    let value = serde_json::from_str(input).ok();
    if input.len() > 4096 { return None; }
    value
}
fn case_wrong_input(input: &str, other: &str) -> Option<JsonValue> {
    if other.len() > 4096 { return None; }
    serde_json::from_str(input).ok()
}
fn case_branch_only(input: &str, enabled: bool) -> Option<JsonValue> {
    if enabled {
        if input.len() > 4096 { return None; }
    }
    serde_json::from_str(input).ok()
}
fn case_rebound(input: &str, other: &str) -> Option<JsonValue> {
    let mut current = input;
    if current.len() > 4096 { return None; }
    current = other;
    serde_json::from_str(current).ok()
}
fn case_expanded(input: &str) -> Option<JsonValue> {
    if input.len() > 4096 { return None; }
    let expanded = input.repeat(1000);
    serde_json::from_str(&expanded).ok()
}
fn case_huge_limit(input: &str) -> Option<JsonValue> {
    if input.len() > 16_000_000_000 { return None; }
    serde_json::from_str(input).ok()
}
struct Expanding(String);
impl<'de> Deserialize<'de> for Expanding {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|s| Expanding(s.repeat(1_000_000)))
    }
}
fn case_custom_deserialize(input: &str) -> Option<Expanding> {
    if input.len() > 4096 { return None; }
    serde_json::from_str(input).ok()
}
#[derive(Deserialize)]
struct Value { field: String }
fn case_spoofed_value(input: &str) -> Option<Value> {
    if input.len() > 4096 { return None; }
    serde_json::from_str(input).ok()
}
struct Alternating(std::cell::Cell<bool>);
impl std::ops::Deref for Alternating {
    type Target = str;
    fn deref(&self) -> &str {
        if self.0.replace(!self.0.get()) { "null" } else { "{\"different\":true}" }
    }
}
fn case_spoofed_deref(input: &Alternating) -> Option<JsonValue> {
    if input.len() > 4096 { return None; }
    serde_json::from_str(input).ok()
}

fn macro_metadata_response(object: &JsonValue) -> JsonValue {
    json!({"apiVersion":"v1", "metadata":object["metadata"]})
}
fn macro_genuinely_unused(object: &JsonValue) -> JsonValue {
    json!({"apiVersion":"v1"})
}
fn macro_assert_kind(kind: &str, actual: usize) {
    assert_eq!(actual, 3, "kind: {}", kind);
}
type StandardValueAlias = JsonValue;
fn case_bounded_alias(input: &str) -> Option<StandardValueAlias> {
    if input.len() > 4096 { return None; }
    serde_json::from_str::<StandardValueAlias>(input).ok()
}
fn case_bounded_annotated(input: &str) -> Option<JsonValue> {
    if input.len() > 4096 { return None; }
    let parsed: serde_json::Result<JsonValue> = serde_json::from_str(input);
    parsed.ok()
}
fn case_generic<T: serde::de::DeserializeOwned>(input: &str) -> Option<T> {
    if input.len() > 4096 { return None; }
    serde_json::from_str::<T>(input).ok()
}
fn case_mixed_sinks(input: &str, other: &str) -> (Option<JsonValue>, Option<JsonValue>) {
    if input.len() > 4096 { return (None, None); }
    (serde_json::from_str::<JsonValue>(input).ok(), serde_json::from_str::<JsonValue>(other).ok())
}
fn main() {}
