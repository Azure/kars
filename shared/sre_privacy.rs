// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Shared wire contract for controller issuance and private-router authorization.
//! Included by both binaries without another dependency or feature variant.

use serde_json::{Value, json};

pub const REVISION: &str = "kars.azure.com/sre-privacy/v2";

pub fn secret_access_reviews(namespace: &str) -> Vec<Value> {
    let mut reviews = Vec::new();
    for scope in [Some(namespace), None] {
        for verb in ["get", "list", "watch"] {
            for name in [
                None,
                Some("router-services-admin"),
                Some("sre-api-router-identity"),
            ] {
                let mut attributes = json!({"group":"","resource":"secrets","verb":verb});
                if let Some(scope) = scope {
                    attributes["namespace"] = scope.into();
                }
                if let Some(name) = name {
                    attributes["name"] = name.into();
                }
                reviews.push(json!({
                    "apiVersion":"authorization.k8s.io/v1","kind":"SubjectAccessReview",
                    "spec":{"user":"system:serviceaccount:kars-sre:sandbox",
                        "groups":["system:authenticated","system:serviceaccounts","system:serviceaccounts:kars-sre"],
                        "resourceAttributes":attributes},
                }));
            }
        }
    }
    reviews
}

pub fn require_denial(response: &Value) -> Result<(), &'static str> {
    if response["status"]["allowed"].as_bool() != Some(false)
        || response["status"]
            .get("evaluationError")
            .is_some_and(|error| !error.is_null() && error.as_str() != Some(""))
    {
        return Err("Legacy SRE Secret get/list/watch authorization is allowed or indeterminate");
    }
    Ok(())
}

/// Only metadata is requested. Even an Opaque Secret carrying this reserved
/// annotation is quarantined: no type mutation or stale SA-UID alias is adopted.
pub fn reject_legacy_aliases(list: &Value, account_uids: &[&str]) -> Result<(), &'static str> {
    let items = list["items"]
        .as_array()
        .ok_or("SRE credential metadata inventory is invalid")?;
    if list["metadata"]
        .get("continue")
        .is_some_and(|token| !token.is_null() && token.as_str() != Some(""))
    {
        return Err("SRE credential metadata inventory is incomplete");
    }
    for item in items {
        let metadata = &item["metadata"];
        if ["name", "uid", "resourceVersion"]
            .iter()
            .any(|key| metadata[*key].as_str().is_none_or(str::is_empty))
        {
            return Err("SRE credential metadata omitted its exact identity");
        }
        let annotations = &metadata["annotations"];
        if annotations["kubernetes.io/service-account.name"] == "sre-api-router"
            || annotations["kubernetes.io/service-account.uid"]
                .as_str()
                .is_some_and(|uid| !uid.is_empty() && account_uids.contains(&uid))
        {
            return Err(
                "Unsafe legacy SRE token Secret alias exists; operator review required, no Secret adopted or deleted",
            );
        }
    }
    Ok(())
}
