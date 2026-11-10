// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::AppState;
use crate::memory_access::MemoryAccess;
use axum::{
    Json,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
pub struct MemoryUpdates(Mutex<BTreeMap<(String, String), (String, Instant)>>);

impl MemoryUpdates {
    fn scope(&self, store: &str, id: &str) -> Option<String> {
        let mut updates = self.0.lock().ok()?;
        updates.retain(|_, (_, time)| time.elapsed() < Duration::from_secs(3600));
        updates
            .get(&(store.into(), id.into()))
            .map(|(scope, _)| scope.clone())
    }

    fn remember(&self, store: &str, id: &str, scope: &str) {
        if let Ok(mut updates) = self.0.lock() {
            updates.retain(|_, (_, time)| time.elapsed() < Duration::from_secs(3600));
            if updates.len() >= 4096 {
                if let Some(key) = updates
                    .iter()
                    .min_by_key(|(_, (_, time))| *time)
                    .map(|(key, _)| key.clone())
                {
                    updates.remove(&key);
                }
            }
            updates.insert((store.into(), id.into()), (scope.into(), Instant::now()));
        }
    }
}

pub struct AuthorizedMemoryRequest {
    store: String,
    scope: Option<String>,
    update: bool,
}

impl AuthorizedMemoryRequest {
    pub async fn current_authorization_digest(&self, state: &AppState) -> Option<String> {
        use sha2::{Digest, Sha256};
        let snapshot = state.memory_binding.read().await;
        if let Some(binding) = snapshot.as_ref().ok()?.as_ref() {
            if binding.store_name != self.store {
                return None;
            }
            if let Some(scope) = &self.scope {
                if scope != &binding.scope && !binding.shared_scopes.contains(scope) {
                    return None;
                }
            }
            return Some(binding.digest.clone());
        }
        // Keep the optional-binding snapshot locked while rechecking the exact
        // conventional private partition. Required or failed bindings never fall back.
        let access = crate::memory_binding_loader::resolve_access(
            None,
            &state.sandbox_name,
            &private_store(state),
        )
        .await
        .ok()?;
        if self.store != access.store_name
            || self
                .scope
                .as_ref()
                .is_some_and(|scope| scope != &access.scope)
        {
            return None;
        }
        let identity = &state.inference_budget.as_ref()?.identity;
        let authority = serde_json::to_vec(&json!({
            "kind": "sandbox-private",
            "sandbox": identity.sandbox,
            "namespaceUid": identity.runtime_namespace_uid,
            "store": access.store_name,
            "scope": access.scope,
        }))
        .ok()?;
        Some(format!("private:sha256:{:x}", Sha256::digest(authority)))
    }
}

pub fn denied(reason: &'static str) -> Response {
    (
        StatusCode::FORBIDDEN,
        [
            ("x-kars-decision", "Blocked"),
            ("x-kars-decision-by", "KarsMemory"),
            ("x-kars-decision-reason", reason),
        ],
        Json(json!({"error": {"type": "kars_memory_denial", "message": reason}})),
    )
        .into_response()
}

fn private_store(state: &AppState) -> String {
    std::env::var("FOUNDRY_MEMORY_STORE_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("memory-{}", state.sandbox_name))
}

pub async fn guard(
    state: &AppState,
    method: &Method,
    segments: &[String],
    body: &[u8],
) -> Result<Option<AuthorizedMemoryRequest>, Box<Response>> {
    if segments.first().map(String::as_str) != Some("memory_stores") {
        return Ok(None);
    }
    let fallback = private_store(state);
    let access = crate::memory_binding_loader::resolve_access(
        Some(&state.memory_binding),
        &state.sandbox_name,
        &fallback,
    )
    .await
    .map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Required memory scope binding is unavailable",
        )
            .into_response()
    })?;
    authorize(&access, &state.memory_updates, method, segments, body)
        .map(Some)
        .map_err(|reason| Box::new(denied(reason)))
}

fn authorize(
    access: &MemoryAccess,
    updates: &MemoryUpdates,
    method: &Method,
    segments: &[String],
    body: &[u8],
) -> Result<AuthorizedMemoryRequest, &'static str> {
    let mut request = AuthorizedMemoryRequest {
        store: access.store_name.clone(),
        scope: None,
        update: false,
    };
    let body = || super::request_json::parse(body).map_err(|_| "memory request must be valid JSON");
    if segments.len() == 1 && method == Method::POST {
        if body()?["name"].as_str() != Some(access.store_name.as_str()) {
            return Err("memory store creation is not authorized");
        }
        return Ok(request);
    }
    let Some(resource) = segments.get(1) else {
        return Err("memory store listing is not authorized");
    };
    if segments.len() == 2 && resource == &access.store_name && method == Method::GET {
        return Ok(request);
    }
    if segments.len() == 2 && method == Method::POST {
        if let Some((store, operation)) = resource.rsplit_once(':') {
            if store == access.store_name
                && matches!(
                    operation,
                    "search_memories" | "update_memories" | "delete_scope"
                )
            {
                let value = body()?;
                let scope = value["scope"]
                    .as_str()
                    .ok_or("memory scope must be explicit")?;
                access.authorize(scope)?;
                request.scope = Some(scope.into());
                request.update = operation == "update_memories";
                return Ok(request);
            }
        }
    }
    if segments.len() == 4
        && resource == &access.store_name
        && segments[2] == "updates"
        && method == Method::GET
    {
        let scope = updates.scope(&access.store_name, &segments[3]).ok_or(
            "memory update ownership is unknown or expired; do not resubmit an accepted update",
        )?;
        access.authorize(&scope)?;
        request.scope = Some(scope);
        return Ok(request);
    }
    Err("memory store or operation is not authorized")
}

pub fn observe(
    request: &AuthorizedMemoryRequest,
    updates: &MemoryUpdates,
    status: StatusCode,
    body: &[u8],
) {
    if request.update && status.is_success() {
        if let (Some(scope), Ok(value)) = (&request.scope, serde_json::from_slice::<Value>(body)) {
            if let Some(id) = value["update_id"].as_str().filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            }) {
                updates.remember(&request.store, id, scope);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access() -> MemoryAccess {
        MemoryAccess {
            store_name: "bound".into(),
            scope: "agent_alice".into(),
            shared_scopes: vec!["team_beta".into()],
        }
    }
    fn path(value: &str) -> Vec<String> {
        value.split('/').map(String::from).collect()
    }

    #[test]
    fn memory_rest_request_byte_limit_preserves_scope_checks() {
        let updates = MemoryUpdates::default();
        const LIMIT: usize = 2 * 1024 * 1024;
        for (resource, input, allowed) in [
            ("memory_stores", br#"{"name":"bound"}"#.as_slice(), true),
            (
                "memory_stores/bound:update_memories",
                br#"{"scope":"agent_alice"}"#.as_slice(),
                true,
            ),
            (
                "memory_stores/bound:search_memories",
                br#"{"scope":"other"}"#.as_slice(),
                false,
            ),
        ] {
            let mut body = input.to_vec();
            body.resize(LIMIT, b' ');
            assert_eq!(
                authorize(&access(), &updates, &Method::POST, &path(resource), &body).is_ok(),
                allowed
            );
            body.push(b' ');
            assert_eq!(
                authorize(&access(), &updates, &Method::POST, &path(resource), &body).err(),
                Some("memory request must be valid JSON")
            );
        }
    }

    #[test]
    fn memory_update_response_is_not_capped_at_request_byte_limit() {
        let updates = MemoryUpdates::default();
        let request = authorize(
            &access(),
            &updates,
            &Method::POST,
            &path("memory_stores/bound:update_memories"),
            br#"{"scope":"agent_alice"}"#,
        )
        .unwrap();
        let mut response = br#"{"update_id":"accepted"}"#.to_vec();
        response.resize(2 * 1024 * 1024 + 1, b' ');
        observe(&request, &updates, StatusCode::ACCEPTED, &response);
        assert_eq!(
            updates.scope("bound", "accepted").as_deref(),
            Some("agent_alice")
        );
    }

    #[test]
    fn memory_rest_denies_foreign_scopes_for_all_operations() {
        let updates = MemoryUpdates::default();
        for op in ["search_memories", "update_memories", "delete_scope"] {
            for scope in [
                "other-sandbox",
                "tenant:other-tenant",
                "system",
                "*",
                "../other",
                "team:beta",
                " team_beta",
            ] {
                assert!(
                    authorize(
                        &access(),
                        &updates,
                        &Method::POST,
                        &path(&format!("memory_stores/bound:{op}")),
                        &serde_json::to_vec(&json!({"scope":scope})).unwrap()
                    )
                    .is_err()
                );
            }
            for scope in ["agent_alice", "team_beta"] {
                assert!(
                    authorize(
                        &access(),
                        &updates,
                        &Method::POST,
                        &path(&format!("memory_stores/bound:{op}")),
                        &serde_json::to_vec(&json!({"scope":scope})).unwrap()
                    )
                    .is_ok()
                );
            }
        }
    }

    #[test]
    fn memory_rest_blocks_alternate_store_and_unscoped_routes() {
        let updates = MemoryUpdates::default();
        for resource in [
            "memory_stores",
            "memory_stores/other",
            "memory_stores/other:search_memories",
            "memory_stores/bound/memories",
            "memory_stores/bound/memories/m1",
            "memory_stores/bound/updates/unknown",
        ] {
            for method in [Method::GET, Method::POST, Method::DELETE, Method::PUT] {
                assert!(authorize(&access(), &updates, &method, &path(resource), b"{}").is_err());
            }
        }
        assert!(
            authorize(
                &access(),
                &updates,
                &Method::DELETE,
                &path("memory_stores/bound"),
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn memory_rest_preserves_bound_provisioning_and_metadata() {
        let updates = MemoryUpdates::default();
        assert!(
            authorize(
                &access(),
                &updates,
                &Method::GET,
                &path("memory_stores/bound"),
                b""
            )
            .is_ok()
        );
        assert!(
            authorize(
                &access(),
                &updates,
                &Method::POST,
                &path("memory_stores"),
                br#"{"name":"bound"}"#
            )
            .is_ok()
        );
        assert!(
            authorize(
                &access(),
                &updates,
                &Method::POST,
                &path("memory_stores"),
                br#"{"name":"other"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn memory_update_ownership_expires_and_rejects_unaccepted_responses() {
        let updates = MemoryUpdates::default();
        updates.0.lock().unwrap().insert(
            ("bound".into(), "old".into()),
            (
                "agent_alice".into(),
                Instant::now() - Duration::from_secs(3601),
            ),
        );
        assert!(updates.scope("bound", "old").is_none());
        let request = authorize(
            &access(),
            &updates,
            &Method::POST,
            &path("memory_stores/bound:update_memories"),
            br#"{"scope":"agent_alice"}"#,
        )
        .unwrap();
        observe(
            &request,
            &updates,
            StatusCode::FORBIDDEN,
            br#"{"update_id":"denied"}"#,
        );
        observe(
            &request,
            &updates,
            StatusCode::ACCEPTED,
            br#"{"update_id":"../foreign"}"#,
        );
        assert!(updates.0.lock().unwrap().is_empty());
        for i in 0..4097 {
            updates.remember("bound", &format!("id-{i}"), "agent_alice");
        }
        assert_eq!(updates.0.lock().unwrap().len(), 4096);
        assert!(updates.scope("other", "id-4096").is_none());
        assert!(updates.scope("bound", "id-0").is_none());
        assert_eq!(
            updates.scope("bound", "id-4096").as_deref(),
            Some("agent_alice")
        );
    }

    #[test]
    fn memory_rest_update_poll_requires_observed_scope_and_current_grant() {
        let updates = MemoryUpdates::default();
        let mut policy = access();
        let request = authorize(
            &policy,
            &updates,
            &Method::POST,
            &path("memory_stores/bound:update_memories"),
            br#"{"scope":"team_beta"}"#,
        )
        .unwrap();
        observe(
            &request,
            &updates,
            StatusCode::ACCEPTED,
            br#"{"update_id":"id-1"}"#,
        );
        assert!(
            authorize(
                &policy,
                &updates,
                &Method::GET,
                &path("memory_stores/bound/updates/id-1"),
                b""
            )
            .is_ok()
        );
        policy.shared_scopes.clear();
        assert!(
            authorize(
                &policy,
                &updates,
                &Method::GET,
                &path("memory_stores/bound/updates/id-1"),
                b""
            )
            .is_err()
        );
    }
}
