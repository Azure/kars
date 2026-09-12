// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use schemars::{Schema, SchemaGenerator};
use serde_json::{Value, json};

fn identity() -> Value {
    json!({
        "type": "object",
        "required": ["name", "uid"],
        "properties": {
            "name": {"type": "string", "minLength": 1, "maxLength": 253},
            "uid": {"type": "string", "minLength": 1, "maxLength": 128}
        }
    })
}

fn target() -> Value {
    json!({
        "type": "object",
        "required": ["kind", "namespace", "name", "uid"],
        "properties": {
            "kind": {"type": "string", "enum": ["KarsSandbox", "KarsTask", "KarsTeam"]},
            "namespace": {"type": "string", "minLength": 1, "maxLength": 63},
            "name": {"type": "string", "minLength": 1, "maxLength": 253},
            "uid": {"type": "string", "minLength": 1, "maxLength": 128}
        }
    })
}

pub fn bindings(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "required": ["grant", "sources"],
        "properties": {
            "grant": identity(),
            "sources": {
                "type": "array", "minItems": 1, "maxItems": 3,
                "items": {
                    "type": "object",
                    "required": ["scope", "source", "keys"],
                    "properties": {
                        "scope": {"type": "string", "enum": ["workspace", "team", "target"]},
                        "source": identity(),
                        "keys": {
                            "type": "array", "maxItems": 128,
                            "items": {"type": "string", "pattern": "^[A-Z_][A-Z0-9_]{0,127}$"}
                        },
                        "owner": target()
                    }
                }
            }
        }
    })
}

pub fn github_binding(_: &mut SchemaGenerator) -> Schema {
    schemars::json_schema!({
        "type": "object",
        "required": ["grant", "connection", "repositories"],
        "properties": {
            "grant": identity(),
            "connection": identity(),
            "repositories": {
                "type": "array", "minItems": 1, "maxItems": 32,
                "x-kubernetes-list-type": "set",
                "items": {
                    "type": "string", "maxLength": 140,
                    "pattern": "^[a-z0-9._-]{1,39}/[a-z0-9._-]{1,100}$"
                }
            },
            "write": {"type": "boolean", "default": false}
        }
    })
}
