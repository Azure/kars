// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Typed MCP content blocks, including embedded resources (never dereferenced).
//! Schema: <https://modelcontextprotocol.io/specification/2025-11-25/schema#contentblock>

use base64::Engine;
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use serde_json::{Map, Number, Value};

/// All five content kinds in MCP 2025-06-18 and 2025-11-25.
/// Extension fields are retained only inside a validated, known content kind.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text {
        text: String,
        #[serde(flatten)]
        attributes: ContentAttributes,
    },
    Image {
        #[serde(deserialize_with = "base64_data")]
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(flatten)]
        attributes: ContentAttributes,
    },
    Audio {
        #[serde(deserialize_with = "base64_data")]
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(flatten)]
        attributes: ContentAttributes,
    },
    Resource {
        resource: ResourceContents,
        #[serde(flatten)]
        attributes: ContentAttributes,
    },
    ResourceLink {
        #[serde(flatten)]
        resource: ResourceLink,
    },
}

impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            attributes: ContentAttributes::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentAttributes {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub annotations: Option<Annotations>,
    #[serde(
        default,
        rename = "_meta",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub audience: Option<Vec<Audience>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "priority"
    )]
    pub priority: Option<Number>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub last_modified: Option<String>,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Audience {
    User,
    Assistant,
}

/// The schema uses anyOf, not oneOf: a resource may contain both text and blob.
/// At least one must be present, and neither may mask an invalid sibling field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "ResourceContentsWire", rename_all = "camelCase")]
pub struct ResourceContents {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceContentsWire {
    #[serde(deserialize_with = "uri")]
    uri: String,
    #[serde(default, deserialize_with = "present")]
    mime_type: Option<String>,
    #[serde(default, deserialize_with = "present")]
    text: Option<String>,
    #[serde(default, deserialize_with = "present")]
    blob: Option<String>,
    #[serde(default, rename = "_meta", deserialize_with = "present")]
    meta: Option<Map<String, Value>>,
    #[serde(flatten)]
    extensions: Map<String, Value>,
}

impl TryFrom<ResourceContentsWire> for ResourceContents {
    type Error = &'static str;

    fn try_from(value: ResourceContentsWire) -> Result<Self, Self::Error> {
        if value.text.is_none() && value.blob.is_none() {
            return Err("embedded resource requires text or blob");
        }
        if let Some(blob) = &value.blob {
            validate_base64(blob)?;
        }
        Ok(Self {
            uri: value.uri,
            mime_type: value.mime_type,
            text: value.text,
            blob: value.blob,
            meta: value.meta,
            extensions: value.extensions,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLink {
    pub name: String,
    #[serde(deserialize_with = "uri")]
    pub uri: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub title: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub description: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub mime_type: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub size: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub icons: Option<Vec<Icon>>,
    #[serde(flatten)]
    pub attributes: ContentAttributes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Icon {
    #[serde(deserialize_with = "uri")]
    pub src: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub mime_type: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub sizes: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub theme: Option<IconTheme>,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IconTheme {
    Light,
    Dark,
}

/// Optional schema fields may be absent, but explicit null is not their type.
pub(super) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn priority<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Number>, D::Error> {
    let value = Number::deserialize(deserializer)?;
    if !value
        .as_f64()
        .is_some_and(|value| (0.0..=1.0).contains(&value))
    {
        return Err(D::Error::custom(
            "annotation priority must be between 0 and 1",
        ));
    }
    Ok(Some(value))
}

fn uri<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.chars().any(|c| c.is_whitespace() || c.is_control())
        || reqwest::Url::parse(&value).is_err()
    {
        return Err(D::Error::custom("resource URI must be an absolute URI"));
    }
    Ok(value)
}

fn validate_base64(value: &str) -> Result<(), &'static str> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map(|_| ())
        .map_err(|_| "content data must be base64")
}

fn base64_data<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    validate_base64(&value).map_err(D::Error::custom)?;
    Ok(value)
}

#[cfg(test)]
mod tests;
