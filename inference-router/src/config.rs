//! Configuration loaded from environment variables.

use anyhow::{Context, Result};

pub struct Config {
    /// Port to listen on (default: 8443)
    pub port: u16,

    /// Azure AI Foundry endpoint for inference (e.g. https://my-resource.openai.azure.com/)
    /// Falls back to AZURE_OPENAI_ENDPOINT for dev-mode compatibility.
    pub foundry_endpoint: Option<String>,

    /// Foundry project endpoint for standalone APIs: Memory Store, Foundry IQ, Agent Service.
    /// (e.g. https://my-resource.services.ai.azure.com/api/projects/my-project)
    /// Uses https://ai.azure.com token audience.
    pub foundry_project_endpoint: Option<String>,

    /// Legacy Azure OpenAI endpoint — used as fallback if FOUNDRY_ENDPOINT is not set.
    pub azure_openai_endpoint: Option<String>,

    /// Default model name
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
        Ok(Self {
            port: std::env::var("ROUTER_PORT")
                .unwrap_or_else(|_| "8443".into())
                .parse()
                .context("ROUTER_PORT must be a valid port number")?,

            foundry_endpoint: std::env::var("FOUNDRY_ENDPOINT").ok().filter(|s| !s.is_empty()),
            foundry_project_endpoint: std::env::var("FOUNDRY_PROJECT_ENDPOINT").ok().filter(|s| !s.is_empty()),
            azure_openai_endpoint: std::env::var("AZURE_OPENAI_ENDPOINT").ok().filter(|s| !s.is_empty()),

            default_model: std::env::var("DEFAULT_MODEL")
                .or_else(|_| std::env::var("AZURE_OPENAI_DEPLOYMENT"))
                .or_else(|_| std::env::var("OPENCLAW_MODEL"))
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
