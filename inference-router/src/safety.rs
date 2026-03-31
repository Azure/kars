//! Foundry Guardrail annotation parsing + AGT content flag reporting.
//!
//! Instead of calling the standalone Content Safety API (which requires a
//! separate Azure resource), we rely on Foundry's built-in guardrails
//! (`Microsoft.DefaultV2`) that are applied to all model deployments by default.
//!
//! Foundry returns content filter annotations in the inference response:
//! - `prompt_filter_results` (top-level): jailbreak, hate, violence, etc.
//! - `choices[].content_filter_results`: output-side filtering
//! - On 400 errors: `error.innererror.content_filter_result` with details
//!
//! When a content flag is detected, we report it to the AGT sidecar for
//! trust scoring, behavior monitoring, and tamper-evident audit logging.

use serde::{Deserialize, Serialize};

use crate::sidecar::SidecarProxy;

// ─── Guardrail Annotation Types ──────────────────────────────────────────────

/// A single content filter category result from Foundry guardrails.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContentFilterResult {
    pub filtered: bool,
    #[serde(default)]
    pub detected: Option<bool>,
    #[serde(default)]
    pub severity: Option<String>,
}

/// All content filter categories returned by Foundry.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ContentFilterResults {
    #[serde(default)]
    pub jailbreak: Option<ContentFilterResult>,
    #[serde(default)]
    pub indirect_attack: Option<ContentFilterResult>,
    #[serde(default)]
    pub hate: Option<ContentFilterResult>,
    #[serde(default)]
    pub self_harm: Option<ContentFilterResult>,
    #[serde(default)]
    pub sexual: Option<ContentFilterResult>,
    #[serde(default)]
    pub violence: Option<ContentFilterResult>,
}

/// A single prompt filter result from Foundry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PromptFilterResult {
    pub prompt_index: u32,
    pub content_filter_results: ContentFilterResults,
}

/// Summary of detected content flags from a Foundry response.
#[derive(Debug, Clone, Default)]
pub struct ContentFlags {
    pub jailbreak_detected: bool,
    pub indirect_attack_detected: bool,
    pub hate_detected: bool,
    pub self_harm_detected: bool,
    pub sexual_detected: bool,
    pub violence_detected: bool,
    /// Categories that were actually filtered (blocked) by Foundry.
    pub filtered_categories: Vec<String>,
    /// Categories that were detected but not filtered.
    pub detected_categories: Vec<String>,
}

impl ContentFlags {
    /// Returns true if any content flag was detected.
    pub fn any_detected(&self) -> bool {
        self.jailbreak_detected
            || self.indirect_attack_detected
            || self.hate_detected
            || self.self_harm_detected
            || self.sexual_detected
            || self.violence_detected
    }

    /// Calculate a trust penalty based on severity of detections.
    /// Jailbreak/indirect_attack = -100, content categories = -50 each.
    pub fn trust_penalty(&self) -> i32 {
        let mut penalty = 0i32;
        if self.jailbreak_detected {
            penalty -= 100;
        }
        if self.indirect_attack_detected {
            penalty -= 100;
        }
        if self.hate_detected {
            penalty -= 50;
        }
        if self.self_harm_detected {
            penalty -= 50;
        }
        if self.sexual_detected {
            penalty -= 50;
        }
        if self.violence_detected {
            penalty -= 50;
        }
        penalty
    }
}

// ─── Annotation Parsing ──────────────────────────────────────────────────────

/// Parse `prompt_filter_results` from a successful (200) Foundry response.
pub fn parse_prompt_filter_results(response_body: &serde_json::Value) -> ContentFlags {
    let mut flags = ContentFlags::default();

    let results = match response_body
        .get("prompt_filter_results")
        .and_then(|v| v.as_array())
    {
        Some(arr) => arr,
        None => return flags,
    };

    for item in results {
        if let Ok(pfr) = serde_json::from_value::<PromptFilterResult>(item.clone()) {
            check_filter_results(&pfr.content_filter_results, &mut flags);
        }
    }

    flags
}

/// Parse content filter result from a Foundry 400 error response.
/// Error shape: `{ "error": { "innererror": { "content_filter_result": { ... } } } }`
pub fn parse_error_content_filter(response_body: &serde_json::Value) -> ContentFlags {
    let mut flags = ContentFlags::default();

    let cfr = response_body
        .get("error")
        .and_then(|e| e.get("innererror"))
        .and_then(|ie| ie.get("content_filter_result"));

    if let Some(cfr_val) = cfr {
        if let Ok(results) = serde_json::from_value::<ContentFilterResults>(cfr_val.clone()) {
            check_filter_results(&results, &mut flags);
        }
    }

    flags
}

/// Parse `prompt_filter_results` from the first SSE chunk of a streaming response.
pub fn parse_streaming_prompt_filter(chunk_text: &str) -> ContentFlags {
    for line in chunk_text.split('\n') {
        let line = line.trim();
        if !line.starts_with("data: ") || line == "data: [DONE]" {
            continue;
        }
        let json_str = &line[6..];
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
            if v.get("prompt_filter_results").is_some() {
                return parse_prompt_filter_results(&v);
            }
        }
    }

    ContentFlags::default()
}

/// Check individual filter results and populate flags.
fn check_filter_results(results: &ContentFilterResults, flags: &mut ContentFlags) {
    if let Some(ref jb) = results.jailbreak {
        if jb.detected.unwrap_or(false) || jb.filtered {
            flags.jailbreak_detected = true;
            if jb.filtered {
                flags.filtered_categories.push("jailbreak".into());
            } else {
                flags.detected_categories.push("jailbreak".into());
            }
        }
    }
    if let Some(ref ia) = results.indirect_attack {
        if ia.detected.unwrap_or(false) || ia.filtered {
            flags.indirect_attack_detected = true;
            if ia.filtered {
                flags.filtered_categories.push("indirect_attack".into());
            } else {
                flags.detected_categories.push("indirect_attack".into());
            }
        }
    }
    if let Some(ref h) = results.hate {
        if h.filtered || h.severity.as_deref().map_or(false, |s| s != "safe") {
            flags.hate_detected = true;
            if h.filtered {
                flags.filtered_categories.push("hate".into());
            } else {
                flags.detected_categories.push("hate".into());
            }
        }
    }
    if let Some(ref sh) = results.self_harm {
        if sh.filtered || sh.severity.as_deref().map_or(false, |s| s != "safe") {
            flags.self_harm_detected = true;
            if sh.filtered {
                flags.filtered_categories.push("self_harm".into());
            } else {
                flags.detected_categories.push("self_harm".into());
            }
        }
    }
    if let Some(ref sx) = results.sexual {
        if sx.filtered || sx.severity.as_deref().map_or(false, |s| s != "safe") {
            flags.sexual_detected = true;
            if sx.filtered {
                flags.filtered_categories.push("sexual".into());
            } else {
                flags.detected_categories.push("sexual".into());
            }
        }
    }
    if let Some(ref vi) = results.violence {
        if vi.filtered || vi.severity.as_deref().map_or(false, |s| s != "safe") {
            flags.violence_detected = true;
            if vi.filtered {
                flags.filtered_categories.push("violence".into());
            } else {
                flags.detected_categories.push("violence".into());
            }
        }
    }
}

// ─── AGT Content Flag Reporting ──────────────────────────────────────────────

/// Report detected content flags to the AGT sidecar for trust scoring and audit.
/// Fire-and-forget — does not block the response.
pub async fn report_content_flags_to_agt(
    sidecar: &SidecarProxy,
    sandbox_name: &str,
    flags: &ContentFlags,
) {
    if !sidecar.enabled || !flags.any_detected() {
        return;
    }

    let report = serde_json::json!({
        "agent_id": sandbox_name,
        "flags": {
            "jailbreak": flags.jailbreak_detected,
            "indirect_attack": flags.indirect_attack_detected,
            "hate": flags.hate_detected,
            "self_harm": flags.self_harm_detected,
            "sexual": flags.sexual_detected,
            "violence": flags.violence_detected,
        },
        "filtered_categories": flags.filtered_categories,
        "detected_categories": flags.detected_categories,
        "trust_penalty": flags.trust_penalty(),
    });

    match sidecar
        .forward("POST", "/report_content_flag", Some(&report))
        .await
    {
        Ok((status, _)) => {
            tracing::info!(
                sandbox = %sandbox_name,
                status,
                penalty = flags.trust_penalty(),
                categories = ?flags.detected_categories,
                "AGT: content flag reported"
            );
        }
        Err(e) => {
            tracing::warn!(
                sandbox = %sandbox_name,
                error = %e,
                "AGT: failed to report content flag (non-blocking)"
            );
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Benign response — no flags ──────────────────────────────────────

    #[test]
    fn test_parse_benign_response_no_flags() {
        let json = serde_json::json!({
            "choices": [{"message": {"content": "Hello"}}],
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "jailbreak": {"detected": false, "filtered": false},
                    "hate": {"filtered": false, "severity": "safe"},
                    "self_harm": {"filtered": false, "severity": "safe"},
                    "sexual": {"filtered": false, "severity": "safe"},
                    "violence": {"filtered": false, "severity": "safe"}
                }
            }]
        });
        let flags = parse_prompt_filter_results(&json);
        assert!(
            !flags.any_detected(),
            "benign response should have no flags"
        );
        assert_eq!(flags.trust_penalty(), 0);
        assert!(flags.filtered_categories.is_empty());
        assert!(flags.detected_categories.is_empty());
    }

    // ── Jailbreak detected and filtered ─────────────────────────────────

    #[test]
    fn test_parse_jailbreak_detected_and_filtered() {
        let json = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "jailbreak": {"detected": true, "filtered": true},
                    "hate": {"filtered": false, "severity": "safe"},
                    "self_harm": {"filtered": false, "severity": "safe"},
                    "sexual": {"filtered": false, "severity": "safe"},
                    "violence": {"filtered": false, "severity": "safe"}
                }
            }]
        });
        let flags = parse_prompt_filter_results(&json);
        assert!(flags.jailbreak_detected);
        assert!(flags.any_detected());
        assert_eq!(flags.trust_penalty(), -100);
        assert!(flags.filtered_categories.contains(&"jailbreak".to_string()));
    }

    // ── Jailbreak detected but not filtered ─────────────────────────────

    #[test]
    fn test_parse_jailbreak_detected_not_filtered() {
        let json = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "jailbreak": {"detected": true, "filtered": false}
                }
            }]
        });
        let flags = parse_prompt_filter_results(&json);
        assert!(flags.jailbreak_detected);
        assert_eq!(flags.trust_penalty(), -100);
        assert!(flags.detected_categories.contains(&"jailbreak".to_string()));
        assert!(flags.filtered_categories.is_empty());
    }

    // ── Error response with content_filter_result ────────────────────────

    #[test]
    fn test_parse_error_content_filter() {
        let json = serde_json::json!({
            "error": {
                "message": "content filtered",
                "type": null,
                "code": "content_filter",
                "innererror": {
                    "code": "ResponsibleAIPolicyViolation",
                    "content_filter_result": {
                        "jailbreak": {"detected": true, "filtered": true},
                        "hate": {"filtered": false, "severity": "safe"},
                        "self_harm": {"filtered": false, "severity": "safe"},
                        "sexual": {"filtered": false, "severity": "safe"},
                        "violence": {"filtered": false, "severity": "safe"}
                    }
                }
            }
        });
        let flags = parse_error_content_filter(&json);
        assert!(flags.jailbreak_detected);
        assert!(flags.any_detected());
        assert_eq!(flags.trust_penalty(), -100);
    }

    // ── Multiple categories detected ────────────────────────────────────

    #[test]
    fn test_parse_multiple_categories() {
        let json = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "jailbreak": {"detected": true, "filtered": true},
                    "hate": {"filtered": true, "severity": "high"},
                    "violence": {"filtered": false, "severity": "medium"},
                    "self_harm": {"filtered": false, "severity": "safe"},
                    "sexual": {"filtered": false, "severity": "safe"}
                }
            }]
        });
        let flags = parse_prompt_filter_results(&json);
        assert!(flags.jailbreak_detected);
        assert!(flags.hate_detected);
        assert!(flags.violence_detected);
        assert!(!flags.self_harm_detected);
        assert!(!flags.sexual_detected);
        // -100 jailbreak + -50 hate + -50 violence = -200
        assert_eq!(flags.trust_penalty(), -200);
    }

    // ── Indirect attack ─────────────────────────────────────────────────

    #[test]
    fn test_parse_indirect_attack() {
        let json = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "indirect_attack": {"detected": true, "filtered": false}
                }
            }]
        });
        let flags = parse_prompt_filter_results(&json);
        assert!(flags.indirect_attack_detected);
        assert_eq!(flags.trust_penalty(), -100);
    }

    // ── Missing prompt_filter_results ────────────────────────────────────

    #[test]
    fn test_parse_missing_prompt_filter_results() {
        let json = serde_json::json!({"choices": [{"message": {"content": "hi"}}]});
        let flags = parse_prompt_filter_results(&json);
        assert!(!flags.any_detected());
    }

    // ── Missing error innererror ─────────────────────────────────────────

    #[test]
    fn test_parse_error_no_innererror() {
        let json = serde_json::json!({"error": {"message": "bad request"}});
        let flags = parse_error_content_filter(&json);
        assert!(!flags.any_detected());
    }

    // ── Streaming annotation parsing ────────────────────────────────────

    #[test]
    fn test_parse_streaming_benign() {
        let chunk = r#"data: {"choices":[],"created":0,"id":"","model":"","object":"","prompt_filter_results":[{"prompt_index":0,"content_filter_results":{"hate":{"filtered":false,"severity":"safe"},"jailbreak":{"detected":false,"filtered":false},"self_harm":{"filtered":false,"severity":"safe"},"sexual":{"filtered":false,"severity":"safe"},"violence":{"filtered":false,"severity":"safe"}}}]}

data: {"choices":[{"delta":{"content":"Hello"}}]}"#;
        let flags = parse_streaming_prompt_filter(chunk);
        assert!(!flags.any_detected());
    }

    #[test]
    fn test_parse_streaming_jailbreak() {
        let chunk = r#"data: {"choices":[],"prompt_filter_results":[{"prompt_index":0,"content_filter_results":{"jailbreak":{"detected":true,"filtered":true},"hate":{"filtered":false,"severity":"safe"}}}]}"#;
        let flags = parse_streaming_prompt_filter(chunk);
        assert!(flags.jailbreak_detected);
        assert_eq!(flags.trust_penalty(), -100);
    }

    #[test]
    fn test_parse_streaming_no_annotations() {
        let chunk = r#"data: {"choices":[{"delta":{"content":"Hello"}}]}
data: [DONE]"#;
        let flags = parse_streaming_prompt_filter(chunk);
        assert!(!flags.any_detected());
    }

    // ── ContentFlags penalty calculation ─────────────────────────────────

    #[test]
    fn test_trust_penalty_all_categories() {
        let flags = ContentFlags {
            jailbreak_detected: true,
            indirect_attack_detected: true,
            hate_detected: true,
            self_harm_detected: true,
            sexual_detected: true,
            violence_detected: true,
            filtered_categories: vec![],
            detected_categories: vec![],
        };
        // -100 -100 -50 -50 -50 -50 = -400
        assert_eq!(flags.trust_penalty(), -400);
    }

    #[test]
    fn test_trust_penalty_none() {
        let flags = ContentFlags::default();
        assert_eq!(flags.trust_penalty(), 0);
        assert!(!flags.any_detected());
    }
}
