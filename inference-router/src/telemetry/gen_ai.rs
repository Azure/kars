// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! OpenTelemetry GenAI Semantic Conventions — attribute constants + typed
//! helpers.
//!
//! Reference spec:
//!   <https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/gen-ai.md>
//!   (`docs/gen-ai/README.md` + `docs/registry/attributes/gen-ai.md`,
//!   consumed 2026-04-24).
//!
//! The router emits GenAI spans and metrics for every upstream model call.
//! Hard-coding attribute keys and enum values in call-sites has two known
//! failure modes:
//!
//! 1. Typos ship silently and dashboards miss data.
//! 2. When the spec renames an attribute (e.g. `gen_ai.response.model`
//!    was once `gen_ai.response.model_id`), grepping twenty files is
//!    noise; one constant is a one-line change.
//!
//! This module is **pure data**. No I/O, no span emission, no allocation
//! beyond what a `str` constant needs. The routes module (Phase 1
//! `inference-router/src/mcp/…` work) is the first consumer; it is wired
//! up there, not here. Landing the constants first locks the wire format
//! before any emission PR can drift from spec.
//!
//! Stability: as of 2026-04-24 the GenAI SemConv attributes below are
//! marked **Development** in the upstream spec. We still pin the exact
//! string values; when they promote to Stable, this module becomes the
//! single place to re-verify.

#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Attribute keys
//
// Grouped per spec section. Ordering matches the upstream registry table
// so diffs against the spec read cleanly.

// ---- General ---------------------------------------------------------------

/// `gen_ai.system` — the GenAI provider (string; e.g. `openai`,
/// `azure.ai.inference`, `anthropic`).
pub const ATTR_SYSTEM: &str = "gen_ai.system";

/// `gen_ai.operation.name` — canonical operation kind. See [`Operation`].
pub const ATTR_OPERATION_NAME: &str = "gen_ai.operation.name";

/// `gen_ai.conversation.id` — session/thread id used to correlate messages.
pub const ATTR_CONVERSATION_ID: &str = "gen_ai.conversation.id";

// ---- Request --------------------------------------------------------------

/// `gen_ai.request.model` — requested model identifier.
pub const ATTR_REQUEST_MODEL: &str = "gen_ai.request.model";

/// `gen_ai.request.temperature` — sampling temperature (double).
pub const ATTR_REQUEST_TEMPERATURE: &str = "gen_ai.request.temperature";

/// `gen_ai.request.top_p` — nucleus sampling parameter (double).
pub const ATTR_REQUEST_TOP_P: &str = "gen_ai.request.top_p";

/// `gen_ai.request.top_k` — top-k sampling parameter (int).
pub const ATTR_REQUEST_TOP_K: &str = "gen_ai.request.top_k";

/// `gen_ai.request.max_tokens` — max tokens to generate (int).
pub const ATTR_REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";

/// `gen_ai.request.frequency_penalty` — frequency penalty (double).
pub const ATTR_REQUEST_FREQUENCY_PENALTY: &str = "gen_ai.request.frequency_penalty";

/// `gen_ai.request.presence_penalty` — presence penalty (double).
pub const ATTR_REQUEST_PRESENCE_PENALTY: &str = "gen_ai.request.presence_penalty";

/// `gen_ai.request.stop_sequences` — array of stop sequences.
pub const ATTR_REQUEST_STOP_SEQUENCES: &str = "gen_ai.request.stop_sequences";

/// `gen_ai.request.seed` — RNG seed (int).
pub const ATTR_REQUEST_SEED: &str = "gen_ai.request.seed";

/// `gen_ai.request.choice.count` — `n` alternatives requested (int).
pub const ATTR_REQUEST_CHOICE_COUNT: &str = "gen_ai.request.choice.count";

/// `gen_ai.request.encoding_formats` — embedding encoding formats.
pub const ATTR_REQUEST_ENCODING_FORMATS: &str = "gen_ai.request.encoding_formats";

// ---- Response -------------------------------------------------------------

/// `gen_ai.response.model` — model identifier that actually served.
pub const ATTR_RESPONSE_MODEL: &str = "gen_ai.response.model";

/// `gen_ai.response.id` — upstream response identifier.
pub const ATTR_RESPONSE_ID: &str = "gen_ai.response.id";

/// `gen_ai.response.finish_reasons` — array of finish reasons.
pub const ATTR_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";

// ---- Token usage ----------------------------------------------------------

/// `gen_ai.usage.input_tokens` — prompt tokens (int).
pub const ATTR_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";

/// `gen_ai.usage.output_tokens` — completion tokens (int).
pub const ATTR_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

/// `gen_ai.token.type` — `input` or `output` (used on the token-usage
/// histogram metric to separate directions).
pub const ATTR_TOKEN_TYPE: &str = "gen_ai.token.type";

// ---- Agents ---------------------------------------------------------------

/// `gen_ai.agent.id` — agent's unique identifier.
pub const ATTR_AGENT_ID: &str = "gen_ai.agent.id";

/// `gen_ai.agent.name` — human-readable agent name.
pub const ATTR_AGENT_NAME: &str = "gen_ai.agent.name";

/// `gen_ai.agent.version` — agent version string.
pub const ATTR_AGENT_VERSION: &str = "gen_ai.agent.version";

/// `gen_ai.agent.description` — free-form agent description.
pub const ATTR_AGENT_DESCRIPTION: &str = "gen_ai.agent.description";

// ---- Tool calls -----------------------------------------------------------

/// `gen_ai.tool.call.id` — tool call identifier.
pub const ATTR_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

/// `gen_ai.tool.name` — tool name invoked by the model.
pub const ATTR_TOOL_NAME: &str = "gen_ai.tool.name";

/// `gen_ai.tool.type` — e.g. `function`, `extension`, `datastore`.
pub const ATTR_TOOL_TYPE: &str = "gen_ai.tool.type";

/// `gen_ai.tool.description` — tool description string.
pub const ATTR_TOOL_DESCRIPTION: &str = "gen_ai.tool.description";

// ---- Errors (standard OTel error attributes, kept here for discoverability)

/// `error.type` — the error type from the OTel general registry, emitted
/// on GenAI spans when an upstream call fails. Spec-cited as the correct
/// error attribute on GenAI spans in `docs/gen-ai/spans.md`.
pub const ATTR_ERROR_TYPE: &str = "error.type";

// ---------------------------------------------------------------------------
// Metric names

/// `gen_ai.client.operation.duration` — histogram (seconds).
pub const METRIC_OPERATION_DURATION: &str = "gen_ai.client.operation.duration";

/// `gen_ai.client.token.usage` — histogram (tokens).
pub const METRIC_TOKEN_USAGE: &str = "gen_ai.client.token.usage";

/// `gen_ai.server.request.duration` — histogram (seconds, server-side).
pub const METRIC_SERVER_REQUEST_DURATION: &str = "gen_ai.server.request.duration";

// ---------------------------------------------------------------------------
// Operation name enum
//
// The spec defines `gen_ai.operation.name` as a closed-ish set of values.
// Unknown operations are permitted but we only ever emit canonical values.

/// Canonical values for `gen_ai.operation.name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Chat,
    TextCompletion,
    Embeddings,
    GenerateContent,
    ExecuteTool,
    CreateAgent,
    InvokeAgent,
}

impl Operation {
    /// The canonical string value as emitted on spans.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::TextCompletion => "text_completion",
            Self::Embeddings => "embeddings",
            Self::GenerateContent => "generate_content",
            Self::ExecuteTool => "execute_tool",
            Self::CreateAgent => "create_agent",
            Self::InvokeAgent => "invoke_agent",
        }
    }

    /// Parse a wire value into a known operation. `None` means the value
    /// is unknown to this SemConv version.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "chat" => Some(Self::Chat),
            "text_completion" => Some(Self::TextCompletion),
            "embeddings" => Some(Self::Embeddings),
            "generate_content" => Some(Self::GenerateContent),
            "execute_tool" => Some(Self::ExecuteTool),
            "create_agent" => Some(Self::CreateAgent),
            "invoke_agent" => Some(Self::InvokeAgent),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// System (provider) constants — closed-world short list; we emit any
// string, but pin the frequently-used ones so call-sites share spelling.

pub mod systems {
    //! Known `gen_ai.system` values. Not an exhaustive list — the
    //! attribute accepts any string — but using these constants avoids
    //! spelling drift across call-sites (`"azureai"` vs `"azure.ai.inference"`).
    pub const OPENAI: &str = "openai";
    pub const AZURE_AI_INFERENCE: &str = "azure.ai.inference";
    pub const AZURE_AI_OPENAI: &str = "azure.ai.openai";
    pub const ANTHROPIC: &str = "anthropic";
    pub const AWS_BEDROCK: &str = "aws.bedrock";
    pub const VERTEX_AI: &str = "vertex_ai";
    pub const GEMINI: &str = "gemini";
    pub const COHERE: &str = "cohere";
    pub const DEEPSEEK: &str = "deepseek";
    pub const GROQ: &str = "groq";
    pub const MISTRAL_AI: &str = "mistral_ai";
}

// ---------------------------------------------------------------------------
// Token type

/// Values for `gen_ai.token.type` on the token-usage histogram metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Input,
    Output,
}

impl TokenType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
        }
    }
}

// ---------------------------------------------------------------------------
// GenAI attribute bag
//
// A typed, allocation-free builder. The consumer (future MCP / routes
// module) can either hand-emit via `tracing::Span::record(key, value)`
// or iterate the rendered form for a logger that wants `&str` attribute
// values. We deliberately do NOT drag in `opentelemetry::KeyValue` here
// — call-sites get to pick the emission mechanism.

/// A focused set of GenAI attributes covering the request→response
/// lifecycle on one LLM call. Optional fields are `None` when unknown;
/// the caller never emits `None` attributes (keeps backends clean).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct GenAiAttributes<'a> {
    pub system: Option<&'a str>,
    pub operation: Option<Operation>,
    pub request_model: Option<&'a str>,
    pub response_model: Option<&'a str>,
    pub response_id: Option<&'a str>,
    pub conversation_id: Option<&'a str>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub finish_reasons: &'a [&'a str],
}

impl GenAiAttributes<'_> {
    /// Materialise the attributes as `(key, rendered value)` pairs.
    ///
    /// Numeric fields are rendered to owned strings (callers that need
    /// the numeric form should consume the raw fields). This helper is
    /// for the common case where a logger wants `&str` attribute values.
    pub fn to_rendered(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        if let Some(v) = self.system {
            out.push((ATTR_SYSTEM, v.to_string()));
        }
        if let Some(op) = self.operation {
            out.push((ATTR_OPERATION_NAME, op.as_str().to_string()));
        }
        if let Some(v) = self.request_model {
            out.push((ATTR_REQUEST_MODEL, v.to_string()));
        }
        if let Some(v) = self.response_model {
            out.push((ATTR_RESPONSE_MODEL, v.to_string()));
        }
        if let Some(v) = self.response_id {
            out.push((ATTR_RESPONSE_ID, v.to_string()));
        }
        if let Some(v) = self.conversation_id {
            out.push((ATTR_CONVERSATION_ID, v.to_string()));
        }
        if let Some(v) = self.input_tokens {
            out.push((ATTR_USAGE_INPUT_TOKENS, v.to_string()));
        }
        if let Some(v) = self.output_tokens {
            out.push((ATTR_USAGE_OUTPUT_TOKENS, v.to_string()));
        }
        if !self.finish_reasons.is_empty() {
            out.push((ATTR_RESPONSE_FINISH_REASONS, self.finish_reasons.join(",")));
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_keys_match_spec_strings() {
        // Frozen against semantic-conventions/docs/registry/attributes/gen-ai.md
        // retrieved 2026-04-24. Anything failing here means the spec
        // moved OR a typo slipped past review. Update + re-audit both.
        assert_eq!(ATTR_SYSTEM, "gen_ai.system");
        assert_eq!(ATTR_OPERATION_NAME, "gen_ai.operation.name");
        assert_eq!(ATTR_CONVERSATION_ID, "gen_ai.conversation.id");
        assert_eq!(ATTR_REQUEST_MODEL, "gen_ai.request.model");
        assert_eq!(ATTR_REQUEST_TEMPERATURE, "gen_ai.request.temperature");
        assert_eq!(ATTR_REQUEST_TOP_P, "gen_ai.request.top_p");
        assert_eq!(ATTR_REQUEST_TOP_K, "gen_ai.request.top_k");
        assert_eq!(ATTR_REQUEST_MAX_TOKENS, "gen_ai.request.max_tokens");
        assert_eq!(
            ATTR_REQUEST_FREQUENCY_PENALTY,
            "gen_ai.request.frequency_penalty"
        );
        assert_eq!(
            ATTR_REQUEST_PRESENCE_PENALTY,
            "gen_ai.request.presence_penalty"
        );
        assert_eq!(ATTR_REQUEST_STOP_SEQUENCES, "gen_ai.request.stop_sequences");
        assert_eq!(ATTR_REQUEST_SEED, "gen_ai.request.seed");
        assert_eq!(ATTR_REQUEST_CHOICE_COUNT, "gen_ai.request.choice.count");
        assert_eq!(
            ATTR_REQUEST_ENCODING_FORMATS,
            "gen_ai.request.encoding_formats"
        );
        assert_eq!(ATTR_RESPONSE_MODEL, "gen_ai.response.model");
        assert_eq!(ATTR_RESPONSE_ID, "gen_ai.response.id");
        assert_eq!(
            ATTR_RESPONSE_FINISH_REASONS,
            "gen_ai.response.finish_reasons"
        );
        assert_eq!(ATTR_USAGE_INPUT_TOKENS, "gen_ai.usage.input_tokens");
        assert_eq!(ATTR_USAGE_OUTPUT_TOKENS, "gen_ai.usage.output_tokens");
        assert_eq!(ATTR_TOKEN_TYPE, "gen_ai.token.type");
        assert_eq!(ATTR_AGENT_ID, "gen_ai.agent.id");
        assert_eq!(ATTR_AGENT_NAME, "gen_ai.agent.name");
        assert_eq!(ATTR_AGENT_VERSION, "gen_ai.agent.version");
        assert_eq!(ATTR_AGENT_DESCRIPTION, "gen_ai.agent.description");
        assert_eq!(ATTR_TOOL_CALL_ID, "gen_ai.tool.call.id");
        assert_eq!(ATTR_TOOL_NAME, "gen_ai.tool.name");
        assert_eq!(ATTR_TOOL_TYPE, "gen_ai.tool.type");
        assert_eq!(ATTR_TOOL_DESCRIPTION, "gen_ai.tool.description");
        assert_eq!(ATTR_ERROR_TYPE, "error.type");
    }

    #[test]
    fn metric_names_match_spec() {
        assert_eq!(
            METRIC_OPERATION_DURATION,
            "gen_ai.client.operation.duration"
        );
        assert_eq!(METRIC_TOKEN_USAGE, "gen_ai.client.token.usage");
        assert_eq!(
            METRIC_SERVER_REQUEST_DURATION,
            "gen_ai.server.request.duration"
        );
    }

    #[test]
    fn operation_canonical_strings() {
        assert_eq!(Operation::Chat.as_str(), "chat");
        assert_eq!(Operation::TextCompletion.as_str(), "text_completion");
        assert_eq!(Operation::Embeddings.as_str(), "embeddings");
        assert_eq!(Operation::GenerateContent.as_str(), "generate_content");
        assert_eq!(Operation::ExecuteTool.as_str(), "execute_tool");
        assert_eq!(Operation::CreateAgent.as_str(), "create_agent");
        assert_eq!(Operation::InvokeAgent.as_str(), "invoke_agent");
    }

    #[test]
    fn operation_roundtrips_from_wire() {
        for op in [
            Operation::Chat,
            Operation::TextCompletion,
            Operation::Embeddings,
            Operation::GenerateContent,
            Operation::ExecuteTool,
            Operation::CreateAgent,
            Operation::InvokeAgent,
        ] {
            assert_eq!(Operation::from_wire(op.as_str()), Some(op));
        }
        assert_eq!(Operation::from_wire(""), None);
        assert_eq!(Operation::from_wire("Chat"), None); // case-sensitive
        assert_eq!(Operation::from_wire("finetune"), None);
    }

    #[test]
    fn token_type_canonical_strings() {
        assert_eq!(TokenType::Input.as_str(), "input");
        assert_eq!(TokenType::Output.as_str(), "output");
    }

    #[test]
    fn systems_known_values() {
        assert_eq!(systems::OPENAI, "openai");
        assert_eq!(systems::AZURE_AI_INFERENCE, "azure.ai.inference");
        assert_eq!(systems::AZURE_AI_OPENAI, "azure.ai.openai");
        assert_eq!(systems::ANTHROPIC, "anthropic");
        assert_eq!(systems::AWS_BEDROCK, "aws.bedrock");
        assert_eq!(systems::VERTEX_AI, "vertex_ai");
    }

    #[test]
    fn attributes_render_omits_absent_fields() {
        let attrs = GenAiAttributes {
            system: Some(systems::AZURE_AI_INFERENCE),
            operation: Some(Operation::Chat),
            request_model: Some("gpt-4o"),
            ..GenAiAttributes::default()
        };
        let rendered = attrs.to_rendered();
        assert_eq!(rendered.len(), 3);
        assert!(
            rendered
                .iter()
                .any(|(k, v)| *k == ATTR_SYSTEM && v == "azure.ai.inference")
        );
        assert!(
            rendered
                .iter()
                .any(|(k, v)| *k == ATTR_OPERATION_NAME && v == "chat")
        );
        assert!(
            rendered
                .iter()
                .any(|(k, v)| *k == ATTR_REQUEST_MODEL && v == "gpt-4o")
        );
    }

    #[test]
    fn attributes_render_encodes_token_counts_and_finish_reasons() {
        let reasons: [&str; 2] = ["stop", "tool_calls"];
        let attrs = GenAiAttributes {
            input_tokens: Some(123),
            output_tokens: Some(45),
            finish_reasons: &reasons,
            ..GenAiAttributes::default()
        };
        let rendered = attrs.to_rendered();
        let by_key: std::collections::HashMap<_, _> = rendered.into_iter().collect();
        assert_eq!(
            by_key.get(ATTR_USAGE_INPUT_TOKENS).map(String::as_str),
            Some("123")
        );
        assert_eq!(
            by_key.get(ATTR_USAGE_OUTPUT_TOKENS).map(String::as_str),
            Some("45")
        );
        assert_eq!(
            by_key.get(ATTR_RESPONSE_FINISH_REASONS).map(String::as_str),
            Some("stop,tool_calls"),
        );
    }

    #[test]
    fn attributes_default_emits_nothing() {
        let rendered = GenAiAttributes::default().to_rendered();
        assert!(rendered.is_empty());
    }
}
