use kube::api::DynamicObject;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub kind: String,
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub scope: String,
    pub source: Identity,
    pub keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Target>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBindings {
    pub grant: Identity,
    pub sources: Vec<Selection>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitHubBinding {
    pub grant: Identity,
    pub connection: Identity,
    pub repositories: Vec<String>,
    pub write: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceState {
    pub name: String,
    pub uid: String,
    pub resource_version: String,
    #[serde(default)]
    pub ownership_from_resource_version: Option<String>,
    pub keys: Vec<String>,
    pub phase: String,
    pub reason: String,
    #[serde(default)]
    pub target: Option<Target>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub secret: Identity,
    pub purpose: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Legacy {
    pub source_name: String,
    pub namespace: String,
    pub namespace_uid: String,
    pub secret: Identity,
    pub resource_version: String,
    pub keys: Vec<String>,
    #[serde(default)]
    pub target: Option<Target>,
}

#[derive(Clone, Debug)]
pub struct Grant {
    pub identity: Identity,
    pub agent_keys: Vec<String>,
    pub stores: Vec<Store>,
    pub sources: Vec<SourceState>,
    pub legacy: Vec<Legacy>,
    pub reviewed_legacy: Vec<Legacy>,
    pub document: DynamicObject,
}

impl Grant {
    pub(super) fn approves_agent_key(&self, key: &str) -> bool {
        [
            "TELEGRAM_BOT_TOKEN",
            "TELEGRAM_ALLOW_FROM",
            "SLACK_BOT_TOKEN",
            "DISCORD_BOT_TOKEN",
            "WHATSAPP_ENABLED",
            "BRAVE_API_KEY",
            "TAVILY_API_KEY",
            "EXA_API_KEY",
            "FIRECRAWL_API_KEY",
            "PERPLEXITY_API_KEY",
        ]
        .contains(&key)
            || self.agent_keys.iter().any(|allowed| allowed == key)
    }
}
