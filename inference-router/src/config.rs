//! Configuration loaded from environment variables and ConfigMap mounts.

use anyhow::{Context, Result};

pub struct Config {
    /// Port to listen on (default: 8443)
    pub port: u16,

    /// Azure OpenAI endpoint (e.g. https://my-aoai.openai.azure.com/)
    pub azure_openai_endpoint: Option<String>,

    /// Default model deployment name
    pub default_model: String,

    /// Enable Azure AI Content Safety (default: true)
    pub content_safety_enabled: bool,

    /// Enable Prompt Shields (default: true)
    pub prompt_shields_enabled: bool,

    /// Azure AI Content Safety endpoint
    pub content_safety_endpoint: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            port: std::env::var("ROUTER_PORT")
                .unwrap_or_else(|_| "8443".into())
                .parse()
                .context("ROUTER_PORT must be a valid port number")?,

            azure_openai_endpoint: std::env::var("AZURE_OPENAI_ENDPOINT").ok(),

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
        })
    }
}
