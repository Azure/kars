//! Configuration loaded from environment variables and ConfigMap mounts.

use anyhow::{Context, Result};

pub struct Config {
    /// Port to listen on (default: 8443)
    pub port: u16,

    /// Inference provider: "azure-openai" | "azure-ai-foundry" (default: auto-detect)
    pub provider: String,

    /// Azure OpenAI endpoint (e.g. https://my-aoai.openai.azure.com/)
    pub azure_openai_endpoint: Option<String>,

    /// Foundry Models endpoint (e.g. https://my-resource.services.ai.azure.com/)
    pub foundry_endpoint: Option<String>,

    /// Default model deployment name
    pub default_model: String,

    /// Enable Azure AI Content Safety (default: true)
    pub content_safety_enabled: bool,

    /// Enable Prompt Shields (default: true)
    pub prompt_shields_enabled: bool,

    /// Azure AI Content Safety endpoint
    pub content_safety_endpoint: Option<String>,

    /// Daily token budget per sandbox (0 = unlimited)
    pub token_budget_daily: u64,

    /// Per-request token limit (0 = unlimited)
    pub token_budget_per_request: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let foundry_endpoint = std::env::var("FOUNDRY_ENDPOINT").ok();
        let azure_openai_endpoint = std::env::var("AZURE_OPENAI_ENDPOINT").ok();

        // Auto-detect provider: prefer Foundry if endpoint is set
        let provider = std::env::var("INFERENCE_PROVIDER").unwrap_or_else(|_| {
            if foundry_endpoint.is_some() {
                "azure-ai-foundry".into()
            } else {
                "azure-openai".into()
            }
        });

        Ok(Self {
            port: std::env::var("ROUTER_PORT")
                .unwrap_or_else(|_| "8443".into())
                .parse()
                .context("ROUTER_PORT must be a valid port number")?,

            provider,
            azure_openai_endpoint,
            foundry_endpoint,

            default_model: std::env::var("DEFAULT_MODEL")
                .unwrap_or_else(|_| "gpt-4.1".into()),

            content_safety_enabled: std::env::var("CONTENT_SAFETY_ENABLED")
                .unwrap_or_else(|_| "true".into())
                .parse()
                .unwrap_or(true),

            prompt_shields_enabled: std::env::var("PROMPT_SHIELDS_ENABLED")
                .unwrap_or_else(|_| "true".into())
                .parse()
                .unwrap_or(true),

            content_safety_endpoint: std::env::var("CONTENT_SAFETY_ENDPOINT").ok(),

            token_budget_daily: std::env::var("TOKEN_BUDGET_DAILY")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),

            token_budget_per_request: std::env::var("TOKEN_BUDGET_PER_REQUEST")
                .unwrap_or_else(|_| "0".into())
                .parse()
                .unwrap_or(0),
        })
    }
}
