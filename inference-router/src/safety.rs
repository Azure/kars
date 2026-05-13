// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

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
//! When a content flag is detected, we report it to the AGT governance engine for
//! trust scoring, behavior monitoring, and tamper-evident audit logging.

use serde::{Deserialize, Serialize};

use crate::governance::Governance;

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
///
/// Two optional env-var knobs (default behaviour unchanged when unset):
/// * `AZURECLAW_CONTENT_FLAG_MIN_SEVERITY` (`safe|low|medium|high`, default
///   `low`) — minimum Foundry severity that raises a category flag. `filtered:
///   true` from Foundry always wins regardless of this threshold.
/// * `AZURECLAW_SUPPRESS_CONTENT_FLAGS` (comma-separated, e.g.
///   `violence,sexual`) — listed categories never raise a flag (no trust
///   penalty, no audit). Useful where Foundry's `violence` heuristic
///   over-fires on legitimate security/research content (e.g. "exploit",
///   "attack", "compromise"). Affects only the four severity-graded
///   categories; `jailbreak` and `indirect_attack` cannot be suppressed.
fn check_filter_results(results: &ContentFilterResults, flags: &mut ContentFlags) {
    let min_sev_level: u8 = match std::env::var("AZURECLAW_CONTENT_FLAG_MIN_SEVERITY")
        .ok()
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("medium") => 2,
        Some("high") => 3,
        Some("safe") => 0,
        _ => 1, // low (default)
    };
    let suppressed: std::collections::HashSet<String> =
        std::env::var("AZURECLAW_SUPPRESS_CONTENT_FLAGS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
    let sev_meets = |sev: Option<&str>| -> bool {
        let level = match sev {
            Some("safe") => 0u8,
            Some("low") => 1,
            Some("medium") => 2,
            Some("high") => 3,
            _ => 0,
        };
        level > 0 && level >= min_sev_level
    };

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
        if !suppressed.contains("hate") && (h.filtered || sev_meets(h.severity.as_deref())) {
            flags.hate_detected = true;
            if h.filtered {
                flags.filtered_categories.push("hate".into());
            } else {
                flags.detected_categories.push("hate".into());
            }
        }
    }
    if let Some(ref sh) = results.self_harm {
        if !suppressed.contains("self_harm") && (sh.filtered || sev_meets(sh.severity.as_deref())) {
            flags.self_harm_detected = true;
            if sh.filtered {
                flags.filtered_categories.push("self_harm".into());
            } else {
                flags.detected_categories.push("self_harm".into());
            }
        }
    }
    if let Some(ref sx) = results.sexual {
        if !suppressed.contains("sexual") && (sx.filtered || sev_meets(sx.severity.as_deref())) {
            flags.sexual_detected = true;
            if sx.filtered {
                flags.filtered_categories.push("sexual".into());
            } else {
                flags.detected_categories.push("sexual".into());
            }
        }
    }
    if let Some(ref vi) = results.violence {
        if !suppressed.contains("violence") && (vi.filtered || sev_meets(vi.severity.as_deref())) {
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

/// Report detected content flags to the AGT governance engine for trust scoring and audit.
/// Fire-and-forget — does not block the response.
pub async fn report_content_flags_to_agt(
    governance: &Governance,
    sandbox_name: &str,
    flags: &ContentFlags,
) {
    if !flags.any_detected() {
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

    let result = governance.report_content_flag(
        sandbox_name,
        &report,
        &flags.filtered_categories,
        &flags.detected_categories,
        flags.trust_penalty(),
    );

    tracing::info!(
        sandbox = %sandbox_name,
        penalty = flags.trust_penalty(),
        categories = ?flags.detected_categories,
        result = %result,
        "AGT: content flag reported (native)"
    );
}

// ─── Slice 2c: InferencePolicy contentSafety floor enforcement ───────────────

/// Ordered severity ladder matching Azure Content Safety conventions.
/// `Safe < Low < Medium < High`. The router compares the parsed Foundry
/// `severity` string against the policy floor: a finding **strictly
/// above** the floor blocks the response. A finding **at or below** the
/// floor is allowed (and trust-scored via `report_content_flags_to_agt`
/// on a separate axis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeverityLevel {
    Safe,
    Low,
    Medium,
    High,
}

impl SeverityLevel {
    /// Parse the Foundry severity string (`"safe"|"low"|"medium"|"high"`,
    /// lowercase). Also accepts the InferencePolicy controller's
    /// PascalCase form (`"Safe"|"Low"|"Medium"|"High"`) so the loader
    /// can reuse this parser. Returns `None` for unknown values — the
    /// caller treats unknown as "no opinion" and falls back to a
    /// permissive decision.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "safe" => Some(Self::Safe),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Stable PascalCase representation used in policy bodies and
    /// surfaced in 403 responses so operators can copy the value back
    /// into their `InferencePolicy`.
    #[must_use]
    pub fn as_pascal(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

/// Per-category severity ceilings + Prompt Shields requirement.
/// Constructed by the InferencePolicy loader from the compiled
/// `contentSafety` block. A `None` field means "no ceiling for this
/// category"; the floor stays permissive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentSafetyFloor {
    pub hate: Option<SeverityLevel>,
    pub self_harm: Option<SeverityLevel>,
    pub sexual: Option<SeverityLevel>,
    pub violence: Option<SeverityLevel>,
    pub require_prompt_shields: bool,
}

impl ContentSafetyFloor {
    /// True iff the policy sets at least one ceiling or requires
    /// Prompt Shields. Used by the chat handler to skip the floor
    /// evaluation entirely when the policy is permissive — keeps the
    /// hot path free of JSON re-walking when no floor is configured.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.hate.is_some()
            || self.self_harm.is_some()
            || self.sexual.is_some()
            || self.violence.is_some()
            || self.require_prompt_shields
    }

    /// Parse a compiled `contentSafety` JSON object (the shape written
    /// by `controller/src/inference_policy_compile.rs`). Unknown
    /// severity strings are dropped so a future Azure-side ladder
    /// extension does not crash old routers. Returns the
    /// always-permissive default if `value` is `null` or not an object.
    #[must_use]
    pub fn from_compiled_json(value: &serde_json::Value) -> Self {
        if !value.is_object() {
            return Self::default();
        }
        let sev = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(SeverityLevel::parse)
        };
        Self {
            hate: sev("hate"),
            self_harm: sev("selfHarm"),
            sexual: sev("sexual"),
            violence: sev("violence"),
            require_prompt_shields: value
                .get("requirePromptShields")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        }
    }
}

/// Reason a response was blocked by `enforce_floor`. Surfaced verbatim
/// in the 403 body and in audit logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloorViolation {
    /// Observed Foundry severity exceeds the policy ceiling for this
    /// category. `category` is the lowercase Foundry name
    /// (`hate|self_harm|sexual|violence`).
    SeverityExceeded {
        category: &'static str,
        observed: SeverityLevel,
        floor: SeverityLevel,
    },
    /// Policy required Prompt Shields annotations, but the upstream
    /// response had no `prompt_filter_results` at all (neither
    /// `jailbreak` nor `indirect_attack`). Fail-closed.
    PromptShieldsMissing,
}

impl FloorViolation {
    /// Stable error code for the 403 body. Distinct from the existing
    /// `content_filter` so operators can disambiguate "Foundry blocked
    /// the model" from "InferencePolicy blocked the response".
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SeverityExceeded { .. } => "inference_policy_content_safety_exceeded",
            Self::PromptShieldsMissing => "inference_policy_prompt_shields_required",
        }
    }

    /// Human-readable message embedded in the 403 body. Stable wording —
    /// downstream tooling matches on the `code` above, not the prose.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::SeverityExceeded {
                category,
                observed,
                floor,
            } => format!(
                "Response blocked: {} severity {} exceeds InferencePolicy ceiling {}",
                category,
                observed.as_pascal(),
                floor.as_pascal()
            ),
            Self::PromptShieldsMissing => {
                "Response blocked: InferencePolicy requires Prompt Shields but the upstream \
                 response carried no prompt_filter_results annotations"
                    .to_string()
            }
        }
    }
}

/// Walk a parsed Foundry response (200 or 400 shape) and decide
/// whether any category exceeds the policy floor. Pure function on
/// `(JSON, floor)` so the chat handler call site stays a one-liner and
/// the test surface covers every category × severity pair without
/// touching HTTP.
///
/// Decision order:
/// 1. If any `prompt_filter_results[*].content_filter_results.<category>.severity`
///    parses to a level **strictly above** the corresponding floor,
///    emit `SeverityExceeded`. First match wins (stable category
///    iteration order: hate → self_harm → sexual → violence).
/// 2. Same for `error.innererror.content_filter_result` on 400
///    responses.
/// 3. If `require_prompt_shields` is true and **no**
///    `prompt_filter_results` array (and no `jailbreak`/`indirect_attack`
///    inside `content_filter_results`) is present, emit
///    `PromptShieldsMissing`. Fail-closed.
#[must_use]
pub fn enforce_floor(
    response_body: &serde_json::Value,
    floor: &ContentSafetyFloor,
) -> Option<FloorViolation> {
    if !floor.is_active() {
        return None;
    }

    // Collect every `content_filter_results` object the response carries —
    // both the top-level prompt_filter_results array (200 path) and the
    // error.innererror.content_filter_result single object (400 path).
    let mut filter_results: Vec<&serde_json::Value> = Vec::new();
    let mut saw_prompt_filter_block = false;
    if let Some(arr) = response_body
        .get("prompt_filter_results")
        .and_then(|v| v.as_array())
    {
        saw_prompt_filter_block = true;
        for item in arr {
            if let Some(cfr) = item.get("content_filter_results") {
                filter_results.push(cfr);
            }
        }
    }
    if let Some(cfr) = response_body
        .get("error")
        .and_then(|e| e.get("innererror"))
        .and_then(|ie| ie.get("content_filter_result"))
    {
        saw_prompt_filter_block = true;
        filter_results.push(cfr);
    }

    // Stable category order — first violation wins so the 403 message
    // is deterministic across runs (important for audit-log assertion
    // in integration tests).
    let categories: [(&'static str, Option<SeverityLevel>); 4] = [
        ("hate", floor.hate),
        ("self_harm", floor.self_harm),
        ("sexual", floor.sexual),
        ("violence", floor.violence),
    ];

    for (cat, maybe_floor) in categories {
        let Some(floor_sev) = maybe_floor else {
            continue;
        };
        for cfr in &filter_results {
            if let Some(observed) = cfr
                .get(cat)
                .and_then(|c| c.get("severity"))
                .and_then(|v| v.as_str())
                .and_then(SeverityLevel::parse)
                && observed > floor_sev
            {
                return Some(FloorViolation::SeverityExceeded {
                    category: cat,
                    observed,
                    floor: floor_sev,
                });
            }
        }
    }

    if floor.require_prompt_shields && !saw_prompt_filter_block {
        return Some(FloorViolation::PromptShieldsMissing);
    }

    None
}

/// Slice 2c — streaming counterpart to [`enforce_floor`]. Inspects
/// the **first** SSE chunk (which is where Foundry surfaces
/// `prompt_filter_results` annotations for streamed responses), walks
/// its `data:` lines in order, parses each as JSON, and runs
/// `enforce_floor` against it. Returns the first violation found, or
/// `None` if every parsable chunk in the text is at or below the
/// floor.
///
/// Latency: this is invoked **once** per stream, only after the
/// outer `text.contains("prompt_filter_results")` quick check has
/// confirmed there's something worth parsing. Lines that are not
/// `data: { ... }` JSON (e.g. `data: [DONE]`, comments, blank
/// separators, or malformed lines) are skipped silently so a
/// single bad chunk cannot crash the stream. Hot-path cost is one
/// JSON parse per `data:` line in the first chunk — single-digit
/// microseconds in practice.
///
/// Decision logic must stay identical to [`enforce_floor`] so that
/// attackers cannot probe `stream=true` vs `stream=false` to choose
/// the laxer evaluation path. Tests cover this parity.
pub fn first_data_line_violation(
    chunk_text: &str,
    floor: &ContentSafetyFloor,
) -> Option<FloorViolation> {
    for line in chunk_text.lines() {
        let payload = match line
            .strip_prefix("data: ")
            .or_else(|| line.strip_prefix("data:"))
        {
            Some(p) => p.trim(),
            None => continue,
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload) else {
            // Defensive: skip malformed JSON chunks rather than
            // killing the whole stream. A subsequent chunk in this
            // same SSE message may still contain a usable filter
            // block.
            continue;
        };
        if let Some(violation) = enforce_floor(&parsed, floor) {
            return Some(violation);
        }
    }
    None
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

    // ── Property-based tests (s5-proptest) ────────────────────────────────────
    //
    // Content-safety parsers run on every Azure OpenAI response — including
    // adversarial / corrupted / partial responses. They must never panic.
    use proptest::prelude::*;

    fn arb_json() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i64>().prop_map(|n| serde_json::json!(n)),
            ".*".prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(4, 32, 8, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..8).prop_map(serde_json::Value::Array),
                proptest::collection::hash_map(".*", inner, 0..8)
                    .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// parse_prompt_filter_results MUST NOT panic on arbitrary JSON.
        /// Attacker may send `prompt_filter_results: 42` or a deeply nested
        /// object — parser must degrade to default flags, never crash.
        #[test]
        fn prop_parse_prompt_filter_never_panics(v in arb_json()) {
            let _ = parse_prompt_filter_results(&v);
        }

        /// parse_error_content_filter MUST NOT panic on arbitrary JSON.
        #[test]
        fn prop_parse_error_content_filter_never_panics(v in arb_json()) {
            let _ = parse_error_content_filter(&v);
        }

        /// parse_streaming_prompt_filter MUST NOT panic on arbitrary UTF-8
        /// strings. SSE stream parsing is stringly-typed and bytes could be
        /// anything (Azure retries, partial chunks, stray CRLFs, etc.).
        #[test]
        fn prop_parse_streaming_never_panics(s in ".*") {
            let _ = parse_streaming_prompt_filter(&s);
        }

        /// enforce_floor MUST NOT panic on arbitrary JSON regardless of
        /// floor configuration. Mirrors the Slice 2a/2b posture: every
        /// new enforcement axis stays panic-free under attacker-shaped
        /// upstream responses.
        #[test]
        fn prop_enforce_floor_never_panics(v in arb_json()) {
            let floor = ContentSafetyFloor {
                hate: Some(SeverityLevel::Low),
                self_harm: Some(SeverityLevel::Medium),
                sexual: Some(SeverityLevel::Low),
                violence: Some(SeverityLevel::High),
                require_prompt_shields: true,
            };
            let _ = enforce_floor(&v, &floor);
        }
    }

    // ── Slice 2c: contentSafety floor enforcement ───────────────────────

    #[test]
    fn severity_level_parses_both_cases_and_unknown_yields_none() {
        // Foundry returns lowercase; the controller compiler emits
        // PascalCase. Both must parse so the loader can hand the same
        // parser to either source.
        assert_eq!(SeverityLevel::parse("safe"), Some(SeverityLevel::Safe));
        assert_eq!(SeverityLevel::parse("Low"), Some(SeverityLevel::Low));
        assert_eq!(SeverityLevel::parse("MEDIUM"), Some(SeverityLevel::Medium));
        assert_eq!(SeverityLevel::parse("High"), Some(SeverityLevel::High));
        assert_eq!(SeverityLevel::parse("extreme"), None);
        assert_eq!(SeverityLevel::parse(""), None);
    }

    #[test]
    fn severity_level_is_strictly_ordered() {
        // The floor compare is `observed > floor`, so this ordering is
        // load-bearing — `Medium > Low` decides whether a response is
        // blocked.
        assert!(SeverityLevel::Safe < SeverityLevel::Low);
        assert!(SeverityLevel::Low < SeverityLevel::Medium);
        assert!(SeverityLevel::Medium < SeverityLevel::High);
    }

    #[test]
    fn content_safety_floor_from_compiled_json_picks_known_levels() {
        let v = serde_json::json!({
            "hate": "Medium",
            "selfHarm": "Low",
            "sexual": "garbage",
            "violence": null,
            "requirePromptShields": true
        });
        let floor = ContentSafetyFloor::from_compiled_json(&v);
        assert_eq!(floor.hate, Some(SeverityLevel::Medium));
        assert_eq!(floor.self_harm, Some(SeverityLevel::Low));
        // Unknown severity drops to None — defence-in-depth so a future
        // Azure-side ladder extension does not crash old routers.
        assert_eq!(floor.sexual, None);
        assert_eq!(floor.violence, None);
        assert!(floor.require_prompt_shields);
        assert!(floor.is_active());
    }

    #[test]
    fn content_safety_floor_from_null_yields_inactive_default() {
        let floor = ContentSafetyFloor::from_compiled_json(&serde_json::Value::Null);
        assert!(!floor.is_active());
        // Inactive floor short-circuits enforce_floor — must skip even
        // when the response would otherwise carry alarming severities.
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": { "hate": { "filtered": true, "severity": "high" } }
            }]
        });
        assert!(enforce_floor(&body, &floor).is_none());
    }

    #[test]
    fn enforce_floor_allows_when_observed_at_or_below_ceiling() {
        let floor = ContentSafetyFloor {
            hate: Some(SeverityLevel::Medium),
            ..Default::default()
        };
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": { "hate": { "filtered": false, "severity": "medium" } }
            }]
        });
        // `observed > floor` — equality is allowed (matches the
        // "Medium ceiling means Medium is OK, High is not" mental
        // model operators set in their CR).
        assert!(enforce_floor(&body, &floor).is_none());
    }

    #[test]
    fn enforce_floor_blocks_when_observed_exceeds_ceiling() {
        let floor = ContentSafetyFloor {
            hate: Some(SeverityLevel::Low),
            ..Default::default()
        };
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": { "hate": { "filtered": true, "severity": "high" } }
            }]
        });
        match enforce_floor(&body, &floor) {
            Some(FloorViolation::SeverityExceeded {
                category,
                observed,
                floor: f,
            }) => {
                assert_eq!(category, "hate");
                assert_eq!(observed, SeverityLevel::High);
                assert_eq!(f, SeverityLevel::Low);
            }
            other => panic!("expected SeverityExceeded, got {other:?}"),
        }
    }

    #[test]
    fn enforce_floor_first_match_wins_stable_iteration_order() {
        // Both hate (High vs Low ceiling) and violence (High vs Low
        // ceiling) exceed — hate must report first because the
        // iteration order is hate → self_harm → sexual → violence.
        // Deterministic ordering matters for audit-log assertions and
        // integration tests that grep for the violation category.
        let floor = ContentSafetyFloor {
            hate: Some(SeverityLevel::Low),
            violence: Some(SeverityLevel::Low),
            ..Default::default()
        };
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "hate": { "filtered": true, "severity": "high" },
                    "violence": { "filtered": true, "severity": "high" }
                }
            }]
        });
        match enforce_floor(&body, &floor) {
            Some(FloorViolation::SeverityExceeded { category, .. }) => {
                assert_eq!(category, "hate");
            }
            other => panic!("expected SeverityExceeded(hate), got {other:?}"),
        }
    }

    #[test]
    fn enforce_floor_walks_400_error_innererror_shape() {
        // Foundry surfaces filter results on 400 under a different
        // path — both branches must enforce the same way or attackers
        // could probe for low-severity 200 vs high-severity 400 to
        // bypass the floor.
        let floor = ContentSafetyFloor {
            violence: Some(SeverityLevel::Medium),
            ..Default::default()
        };
        let body = serde_json::json!({
            "error": {
                "innererror": {
                    "content_filter_result": {
                        "violence": { "filtered": true, "severity": "high" }
                    }
                }
            }
        });
        match enforce_floor(&body, &floor) {
            Some(FloorViolation::SeverityExceeded {
                category, observed, ..
            }) => {
                assert_eq!(category, "violence");
                assert_eq!(observed, SeverityLevel::High);
            }
            other => panic!("expected SeverityExceeded(violence), got {other:?}"),
        }
    }

    #[test]
    fn enforce_floor_require_prompt_shields_blocks_when_no_annotations_present() {
        // Fail-closed: Prompt Shields required, response has no
        // prompt_filter_results array anywhere — block. This catches
        // the case where the upstream deployment silently lost shield
        // configuration and the response surface looks "clean".
        let floor = ContentSafetyFloor {
            require_prompt_shields: true,
            ..Default::default()
        };
        let body = serde_json::json!({ "choices": [{ "message": { "content": "ok" } }] });
        assert_eq!(
            enforce_floor(&body, &floor),
            Some(FloorViolation::PromptShieldsMissing)
        );
    }

    #[test]
    fn enforce_floor_require_prompt_shields_allows_when_annotations_present() {
        // Even if the array is empty in content but the block exists,
        // the upstream is annotating — that's enough to satisfy the
        // fail-closed gate. (A genuinely missing shield would also
        // trip the category-severity branch in practice; this gate is
        // about catching configuration silence.)
        let floor = ContentSafetyFloor {
            require_prompt_shields: true,
            ..Default::default()
        };
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": {
                    "jailbreak": { "filtered": false, "detected": false }
                }
            }]
        });
        assert_eq!(enforce_floor(&body, &floor), None);
    }

    #[test]
    fn enforce_floor_inactive_skips_walk() {
        // Permissive floor must short-circuit so the hot path doesn't
        // pay the JSON walk cost when no policy is configured. We
        // can't observe time directly here, but we can prove the
        // branch is taken by feeding a body that would otherwise
        // certainly violate and asserting `None`.
        let floor = ContentSafetyFloor::default();
        assert!(!floor.is_active());
        let body = serde_json::json!({
            "prompt_filter_results": [{
                "prompt_index": 0,
                "content_filter_results": { "hate": { "filtered": true, "severity": "high" } }
            }]
        });
        assert!(enforce_floor(&body, &floor).is_none());
    }

    #[test]
    fn floor_violation_codes_are_stable_for_tooling() {
        // Downstream tooling matches on `code` — stability matters.
        let v = FloorViolation::SeverityExceeded {
            category: "hate",
            observed: SeverityLevel::High,
            floor: SeverityLevel::Low,
        };
        assert_eq!(v.code(), "inference_policy_content_safety_exceeded");
        assert!(v.message().contains("hate"));
        assert!(v.message().contains("High"));
        assert!(v.message().contains("Low"));

        let v2 = FloorViolation::PromptShieldsMissing;
        assert_eq!(v2.code(), "inference_policy_prompt_shields_required");
        assert!(v2.message().contains("Prompt Shields"));
    }

    // ── first_data_line_violation (Slice 2c streaming counterpart) ──

    fn high_hate_floor() -> ContentSafetyFloor {
        ContentSafetyFloor {
            hate: Some(SeverityLevel::Low),
            self_harm: None,
            sexual: None,
            violence: None,
            require_prompt_shields: false,
        }
    }

    #[test]
    fn first_data_line_violation_returns_none_when_no_data_lines() {
        let floor = high_hate_floor();
        assert!(first_data_line_violation("", &floor).is_none());
        assert!(first_data_line_violation(": comment\n\n", &floor).is_none());
        assert!(first_data_line_violation("data: [DONE]\n\n", &floor).is_none());
    }

    #[test]
    fn first_data_line_violation_returns_some_on_first_violating_chunk() {
        let floor = high_hate_floor();
        let chunk = "data: {\"prompt_filter_results\":[{\"content_filter_results\":{\"hate\":{\"severity\":\"high\",\"filtered\":false}}}]}\n\n";
        let v = first_data_line_violation(chunk, &floor).expect("violation");
        match v {
            FloorViolation::SeverityExceeded { category, .. } => assert_eq!(category, "hate"),
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn first_data_line_violation_returns_none_when_under_floor() {
        let floor = high_hate_floor();
        let chunk = "data: {\"prompt_filter_results\":[{\"content_filter_results\":{\"hate\":{\"severity\":\"safe\",\"filtered\":false}}}]}\n\n";
        assert!(first_data_line_violation(chunk, &floor).is_none());
    }

    #[test]
    fn first_data_line_violation_skips_malformed_json_lines() {
        let floor = high_hate_floor();
        let chunk = "data: not json\ndata: {\"prompt_filter_results\":[{\"content_filter_results\":{\"hate\":{\"severity\":\"high\",\"filtered\":false}}}]}\n";
        let v = first_data_line_violation(chunk, &floor);
        assert!(
            v.is_some(),
            "malformed prefix should not mask later violation"
        );
    }

    #[test]
    fn first_data_line_violation_stops_at_first_violation() {
        // Two violating chunks — must return the first one in line order.
        let floor = ContentSafetyFloor {
            hate: Some(SeverityLevel::Low),
            self_harm: Some(SeverityLevel::Low),
            sexual: None,
            violence: None,
            require_prompt_shields: false,
        };
        let chunk = concat!(
            "data: {\"prompt_filter_results\":[{\"content_filter_results\":{\"self_harm\":{\"severity\":\"high\",\"filtered\":false}}}]}\n",
            "data: {\"prompt_filter_results\":[{\"content_filter_results\":{\"hate\":{\"severity\":\"high\",\"filtered\":false}}}]}\n",
        );
        let v = first_data_line_violation(chunk, &floor).expect("violation");
        match v {
            FloorViolation::SeverityExceeded { category, .. } => {
                assert_eq!(
                    category, "self_harm",
                    "must return earliest line's violation"
                );
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn first_data_line_violation_parity_with_enforce_floor() {
        // Defence-in-depth: streaming and non-streaming branches must
        // reach identical decisions for the same payload. Attackers
        // must not be able to choose `stream=true` to bypass the
        // floor.
        let floor = ContentSafetyFloor {
            hate: Some(SeverityLevel::Medium),
            self_harm: None,
            sexual: None,
            violence: None,
            require_prompt_shields: true,
        };
        let payload = serde_json::json!({
            "prompt_filter_results": [{
                "content_filter_results": {
                    "hate": {"severity": "high", "filtered": false}
                }
            }]
        });
        let buffered = enforce_floor(&payload, &floor).expect("buffered violation");
        let chunk = format!("data: {payload}\n\n");
        let streamed = first_data_line_violation(&chunk, &floor).expect("streaming violation");
        assert_eq!(buffered.code(), streamed.code());
    }
}
