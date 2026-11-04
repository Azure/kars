// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Content previews are diagnostic, untrusted, and never execution evidence.
//! Sanitize complete input before truncating so a cut cannot hide a secret marker.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::LazyLock;

pub const MAX_PREVIEW_BYTES: usize = 2048;
pub const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const REDACTED: &str = "[REDACTED]";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolPreview {
    pub text: String,
    pub truncated: bool,
    pub redacted: bool,
}

static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)authorization|authentication|password|passwd|pwd|secret|credential|cookie|token|apikey|accesskey|privatekey|clientkey|signature|connectionstring").unwrap()
});
static SECRET_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)-----BEGIN [^-]*PRIVATE KEY|(?:authorization|password|passwd|pwd|secret|credential|api[_ -]?key|access[_ -]?key|private[_ -]?key|client[_ -]?secret|(?:access|refresh|id)[_ -]?token|accountkey|sharedaccesssignature)\s*["']?\s*[:=]|\b(?:bearer|basic)\s+\S+|\b(?:gh[pousr]_|github_pat_|sk-|xox[baprs]-|eyJ)[A-Za-z0-9_.-]+"#).unwrap()
});
static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s\"'<>]+"#).unwrap());
static OPAQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_+/=-]{48,}").unwrap());

fn sensitive(key: &str) -> bool {
    let normalized: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    SENSITIVE.is_match(&normalized)
        || matches!(
            normalized.to_ascii_lowercase().as_str(),
            "key" | "sig" | "auth"
        )
}

struct Sanitizer {
    redacted: bool,
    truncated: bool,
    nodes: usize,
    secrets: Vec<String>,
}

impl Sanitizer {
    fn string(&mut self, value: &str, depth: usize) -> String {
        if SECRET_TEXT.is_match(value) {
            self.redacted = true;
            return REDACTED.into();
        }
        // JSON embedded in a body must not bypass sensitive-key redaction.
        if let Ok(nested) = serde_json::from_str::<Value>(value)
            && matches!(nested, Value::Object(_) | Value::Array(_))
        {
            return self.value(&nested, depth + 1).to_string();
        }
        let mut result = value.to_string();
        for secret in &self.secrets {
            if result.contains(secret) {
                result = result.replace(secret, REDACTED);
                self.redacted = true;
            }
        }
        result = URL
            .replace_all(&result, |captures: &regex::Captures<'_>| {
                let Ok(mut url) = reqwest::Url::parse(&captures[0]) else {
                    self.redacted = true;
                    return REDACTED.to_string();
                };
                if !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    self.redacted = true;
                    let _ = url.set_username("");
                    let _ = url.set_password(None);
                    url.set_query(None);
                    url.set_fragment(None);
                    format!("{} [credentials/query/fragment omitted]", url)
                } else {
                    captures[0].to_string()
                }
            })
            .into_owned();
        if OPAQUE.is_match(&result) {
            self.redacted = true;
            result = OPAQUE.replace_all(&result, REDACTED).into_owned();
        }
        let clean: String = result
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            .collect();
        if clean.len() > 1024 {
            self.truncated = true;
            let mut end = 1024;
            while !clean.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &clean[..end])
        } else {
            clean
        }
    }

    fn value(&mut self, value: &Value, depth: usize) -> Value {
        self.nodes += 1;
        if depth > 6 || self.nodes > 96 {
            self.truncated = true;
            return json!("[preview limit]");
        }
        match value {
            Value::Object(fields) => {
                let sensitive_file = fields.iter().any(|(key, value)| {
                    matches!(key.as_str(), "path" | "file" | "file_path" | "filename")
                        && value.as_str().is_some_and(|path| {
                            let path = path.to_ascii_lowercase();
                            path.split('/').any(|part| {
                                part == ".env"
                                    || part.starts_with(".env.")
                                    || part == "id_rsa"
                                    || part == "id_ed25519"
                            }) || path.ends_with(".pem")
                                || path.ends_with(".key")
                                || sensitive(&path)
                        })
                });
                let mut selected = serde_json::Map::new();
                for (key, value) in fields.iter().take(24) {
                    let private = sensitive(key)
                        || (sensitive_file
                            && matches!(
                                key.as_str(),
                                "content"
                                    | "contents"
                                    | "text"
                                    | "body"
                                    | "data"
                                    | "old_text"
                                    | "new_text"
                            ));
                    let key = self.string(key, depth + 1);
                    let value = if private {
                        self.redacted = true;
                        json!(REDACTED)
                    } else {
                        self.value(value, depth + 1)
                    };
                    selected.insert(key, value);
                }
                self.truncated |= fields.len() > 24;
                Value::Object(selected)
            }
            Value::Array(items) => {
                self.truncated |= items.len() > 16;
                Value::Array(
                    items
                        .iter()
                        .take(16)
                        .map(|v| self.value(v, depth + 1))
                        .collect(),
                )
            }
            Value::String(value) => json!(self.string(value, depth)),
            _ => value.clone(),
        }
    }
}

impl ToolPreview {
    pub fn omitted() -> Self {
        Self {
            text: "[Argument preview unavailable: capture limit or incomplete JSON]".into(),
            truncated: true,
            redacted: false,
        }
    }

    pub fn arguments(value: &Value) -> Self {
        let mut sanitizer = Sanitizer {
            redacted: false,
            truncated: false,
            nodes: 0,
            secrets: std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .filter(|(key, value)| sensitive(key) && value.len() >= 8)
                .map(|(_, value)| value)
                .collect(),
        };
        let mut text = serde_json::to_string_pretty(&sanitizer.value(value, 0)).unwrap_or_default();
        if text.len() > MAX_PREVIEW_BYTES {
            sanitizer.truncated = true;
            let mut end = MAX_PREVIEW_BYTES - '…'.len_utf8();
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push('…');
        }
        Self {
            text,
            truncated: sanitizer.truncated,
            redacted: sanitizer.redacted,
        }
    }

    pub fn encoded(arguments: &str) -> Self {
        if arguments.len() > MAX_ARGUMENT_BYTES {
            return Self::omitted();
        }
        match serde_json::from_str::<Value>(arguments) {
            Ok(value) => Self::arguments(&value),
            Err(_) => Self::omitted(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_nonsecret_urls_without_canonicalizing_arguments() {
        for url in [
            "https://example.net",
            "https://EXAMPLE.net:443/a/../b",
            "https://example.net/",
        ] {
            let preview = ToolPreview::arguments(&json!({"url": url, "method": "GET"}));
            let shown: Value = serde_json::from_str(&preview.text).unwrap();
            assert_eq!(shown["url"], url);
            assert!(!preview.redacted && !preview.truncated);
        }
        let text = "Fetch `https://example.net`.";
        let preview = ToolPreview::arguments(&json!({"content": text}));
        let shown: Value = serde_json::from_str(&preview.text).unwrap();
        assert_eq!(shown["content"], text);
    }

    #[test]
    fn keeps_useful_bodies_options_and_paths() {
        let preview = ToolPreview::arguments(
            &json!({"path":"/sandbox/handoff.md", "content":"A useful recommendation\nwith evidence", "offset":4}),
        );
        assert!(preview.text.contains("/sandbox/handoff.md"));
        assert!(preview.text.contains("A useful recommendation"));
        assert!(!preview.redacted && !preview.truncated);
    }

    #[test]
    fn redacts_nested_keys_embedded_credentials_urls_and_secret_files() {
        for value in [
            json!({"headers":{"Authorization":"private-value"},"nested":{"api_key":"private-value"}}),
            json!({"content":"password=private-value\nnext line"}),
            json!({"content":"{\"token\":\"private-value\"}"}),
            json!({"url":"https://user:private-value@example.com/docs?q=private-value#private-value"}),
            json!({"path":"/sandbox/.env", "content":"private-value"}),
            json!({"content":"-----BEGIN PRIVATE KEY-----\nprivate-value\n-----END PRIVATE KEY-----"}),
        ] {
            let preview = ToolPreview::arguments(&value);
            assert!(preview.redacted, "{preview:?}");
            assert!(!preview.text.contains("private-value"), "{preview:?}");
        }
    }

    #[test]
    fn embedded_json_preserves_useful_fields_with_bounded_depth() {
        let preview = ToolPreview::arguments(
            &json!({"body":"{\"path\":\"/sandbox/report.md\",\"content\":\"Useful evidence\"}"}),
        );
        assert!(preview.text.contains("Useful evidence"));
        assert!(!preview.truncated);
        let preview = ToolPreview::arguments(
            &json!({"url":"postgresql://test:private-value@database.example/db"}),
        );
        assert!(preview.redacted);
        assert!(!preview.text.contains("private-value"));
    }

    #[test]
    fn incomplete_json_is_never_an_unsanitized_prefix() {
        let preview = ToolPreview::encoded("{\"body\":\"private-value");
        assert!(preview.truncated);
        assert!(!preview.text.contains("private-value"));
    }

    #[test]
    fn bounded_unicode_and_late_secret_markers() {
        let preview = ToolPreview::arguments(
            &json!({"content":format!("{} password=private-value", "🦀".repeat(2000))}),
        );
        assert!(preview.redacted);
        assert!(!preview.text.contains("private-value"));
        let preview = ToolPreview::arguments(
            &json!({"content":"🦀".repeat(2000), "other":"🦀".repeat(2000)}),
        );
        assert!(preview.truncated);
        assert!(preview.text.len() <= MAX_PREVIEW_BYTES);
    }
}
