// Copyright (c) Pal Lakatos-Toth.

use axum::Json;
use axum::extract::State;
use kube::core::DynamicObject;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

use super::additional_providers::{INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET};
use super::{created_of, is_dns1123_label, label, name_of, ns_of, require_cluster, upstream};

// ─── Local (in-cluster) inference — AI Runway ModelDeployment ────────────────
// See docs/local-inference.md (kars core). kars does NOT install or manage
// AI Runway/KAITO — an operator installs both once via their own real
// helm/kubectl commands, exactly like the GitHub App or Azure AI Foundry
// connection. This surface only detects presence and manages `ModelDeployment`
// objects on top, in the Bridge's own `kars-local-inference` namespace.

#[derive(Debug, Serialize)]
pub struct LocalInferenceStatusDto {
    /// Whether AI Runway's `modeldeployments.airunway.ai` CRD is present —
    /// i.e. whether an operator has installed it (see docs/local-inference.md).
    pub available: bool,
    /// Real, live-scanned count of nodes advertising `nvidia.com/gpu`
    /// capacity — never a hardcoded guess. Zero means only CPU-tier models
    /// can be offered.
    pub gpu_node_count: u32,
    /// Distinct GPU product names found via the NFD/GPU-feature-discovery
    /// `nvidia.com/gpu.product` node label, when present.
    pub gpu_products: Vec<String>,
}

/// `GET /api/operator/local-inference/status` — detect whether the cluster
/// can host an in-cluster model, and whether it has GPU capacity for the
/// larger tier. Never installs anything.
pub async fn local_inference_status(
    State(state): State<AppState>,
) -> AppResult<Json<LocalInferenceStatusDto>> {
    let cluster = require_cluster(&state)?;
    let available = cluster.local_inference_available().await;
    let gpu = cluster.gpu_node_summary().await.unwrap_or_default();
    Ok(Json(LocalInferenceStatusDto {
        available,
        gpu_node_count: gpu.gpu_node_count,
        gpu_products: gpu.gpu_products,
    }))
}

/// One curated, vetted model the wizard can offer without the operator
/// hand-typing a HuggingFace id or an AIKit image reference. Real values
/// verified live against AI Runway v0.7.0 + KAITO workspace chart 0.11.0 —
/// see docs/local-inference.md.
#[derive(Debug, Serialize, Clone)]
pub struct CuratedLocalModelDto {
    pub id: String,
    pub label: String,
    pub tier: String, // "cpu" | "gpu"
    pub params: String,
}

/// `GET /api/operator/local-inference/catalog` — the curated list + tier
/// availability (a GPU entry is still LISTED when no GPU node exists, so the
/// wizard can show it disabled with a clear reason, rather than silently
/// hiding an option and confusing an operator who just hasn't added GPU
/// nodes yet).
pub async fn local_inference_catalog() -> Json<Vec<CuratedLocalModelDto>> {
    Json(vec![
        CuratedLocalModelDto {
            id: "llama-3.2-1b-instruct".into(),
            label: "Llama 3.2 (1B, CPU)".into(),
            tier: "cpu".into(),
            params: "1B".into(),
        },
        CuratedLocalModelDto {
            id: "llama-3.2-3b-instruct".into(),
            label: "Llama 3.2 (3B, CPU)".into(),
            tier: "cpu".into(),
            params: "3B".into(),
        },
        CuratedLocalModelDto {
            id: "gemma-2-2b-instruct".into(),
            label: "Gemma 2 (2B, CPU)".into(),
            tier: "cpu".into(),
            params: "2B".into(),
        },
        CuratedLocalModelDto {
            id: "microsoft/Phi-4-mini-instruct".into(),
            label: "Phi-4-mini (GPU)".into(),
            tier: "gpu".into(),
            params: "3.8B".into(),
        },
        CuratedLocalModelDto {
            id: "meta-llama/Llama-3.1-8B-Instruct".into(),
            label: "Llama 3.1 (8B, GPU)".into(),
            tier: "gpu".into(),
            params: "8B".into(),
        },
        CuratedLocalModelDto {
            id: "mistralai/Mistral-7B-Instruct-v0.3".into(),
            label: "Mistral (7B, GPU)".into(),
            tier: "gpu".into(),
            params: "7B".into(),
        },
    ])
}

/// The AIKit CPU image for each curated CPU-tier model id — the `llamacpp`
/// engine needs an explicit pre-built image (there is no live HF→GGUF
/// resolution path), so this is the one place that mapping has to be
/// hardcoded. Free-text/advanced deployments must supply their own image.
fn aikit_image_for(model_id: &str) -> Option<&'static str> {
    match model_id {
        "llama-3.2-1b-instruct" => Some("ghcr.io/kaito-project/aikit/llama3.2:1b"),
        "llama-3.2-3b-instruct" => Some("ghcr.io/kaito-project/aikit/llama3.2:3b"),
        "gemma-2-2b-instruct" => Some("ghcr.io/kaito-project/aikit/gemma2:2b"),
        _ => None,
    }
}

#[derive(Debug, Serialize)]
pub struct LocalModelDeploymentDto {
    pub name: String,
    pub namespace: String,
    pub managed: bool,
    pub model_id: Option<String>,
    pub engine: Option<String>,
    pub provider: Option<String>,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub endpoint: Option<String>,
    pub created_at: Option<String>,
}

fn project_model_deployment(o: &DynamicObject) -> LocalModelDeploymentDto {
    let name = name_of(o);
    let namespace = ns_of(o);
    let managed = namespace == crate::kars::cluster::LOCAL_INFERENCE_NAMESPACE
        && label(o, "app.kubernetes.io/managed-by").as_deref() == Some("kars-bridge");
    let spec = o.data.get("spec");
    let status = o.data.get("status");
    let model_id = spec
        .and_then(|s| s.get("model"))
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .map(String::from);
    let engine = status
        .and_then(|s| s.get("engine"))
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .map(String::from);
    let provider = status
        .and_then(|s| s.get("provider"))
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .map(String::from);
    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(Value::as_str)
        .map(String::from);
    let message = status
        .and_then(|s| s.get("message"))
        .and_then(Value::as_str)
        .map(String::from);
    // AI Runway publishes the routable Service in status when available. Fall
    // back to the ModelDeployment name and port 80 for older controller builds.
    let endpoint = if phase.as_deref() == Some("Running") {
        let service = status
            .and_then(|s| s.get("endpoint"))
            .and_then(|e| e.get("service"))
            .and_then(Value::as_str)
            .unwrap_or(&name);
        let port = status
            .and_then(|s| s.get("endpoint"))
            .and_then(|e| e.get("port"))
            .and_then(Value::as_u64)
            .unwrap_or(80);
        Some(format!(
            "http://{service}.{namespace}.svc.cluster.local:{port}"
        ))
    } else {
        None
    };
    LocalModelDeploymentDto {
        name,
        namespace,
        managed,
        model_id,
        engine,
        provider,
        phase,
        message,
        endpoint,
        created_at: created_of(o),
    }
}

/// `GET /api/operator/local-inference/deployments` — every ModelDeployment
/// the Bridge manages, with live status.
/// `GET /api/operator/local-inference/deployments` — every ModelDeployment
/// the Bridge manages, with live status. As a side effect, auto-registers
/// any newly-`Running` deployment as a normal additional inference provider
/// (tag `local-<name>`) — reusing the exact multi-provider mechanism proven
/// this session, so no router changes are needed: every sandbox's router
/// already knows how to dial an arbitrary custom OpenAI-compatible endpoint
/// once it's in `kars-inference-providers`. Idempotent (a re-list of an
/// already-wired deployment is a no-op re-write of the same values).
pub async fn list_local_model_deployments(
    State(state): State<AppState>,
) -> AppResult<Json<Vec<LocalModelDeploymentDto>>> {
    let cluster = require_cluster(&state)?;
    let items = cluster.list_model_deployments().await.map_err(upstream)?;
    let dtos: Vec<LocalModelDeploymentDto> = items.iter().map(project_model_deployment).collect();
    for d in &dtos {
        if d.managed
            && d.phase.as_deref() == Some("Running")
            && let (Some(endpoint), Some(model_id)) = (&d.endpoint, &d.model_id)
        {
            auto_wire_local_provider(cluster, &d.name, endpoint, model_id).await;
        }
    }
    Ok(Json(dtos))
}

/// Register a Running local ModelDeployment's Service as an additional
/// inference provider tagged `local-<name>`, no API key (in-cluster,
/// unauthenticated). Best-effort: a write failure here degrades to "the
/// model runs but isn't yet selectable from an InferencePolicy" rather than
/// failing the status poll the wizard depends on.
async fn auto_wire_local_provider(
    cluster: &crate::kars::cluster::Cluster,
    name: &str,
    endpoint: &str,
    model_id: &str,
) {
    let tag_upper = format!("LOCAL_{}", name.to_ascii_uppercase().replace('-', "_"));
    let endpoint = endpoint.to_string();
    let model_id = model_id.to_string();
    if let Err(e) = cluster
        .mutate_secret_keys(
            INFERENCE_PROVIDERS_NS,
            INFERENCE_PROVIDERS_SECRET,
            move |keys| {
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"),
                    endpoint.clone(),
                );
                keys.insert(
                    format!("KARS_PROVIDER_{tag_upper}_MODELS"),
                    model_id.clone(),
                );
            },
        )
        .await
    {
        tracing::warn!(deployment = name, error = %e, "failed to auto-wire local model as an inference provider");
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateLocalModelDeploymentRequest {
    /// DNS-label name for this deployment (becomes the Service name kars
    /// wires into the inference-providers secret).
    pub name: String,
    /// A curated id (see `local_inference_catalog`) or, for the advanced
    /// free-text path, any HuggingFace model id.
    pub model_id: String,
    /// "cpu" or "gpu" — selects the engine/provider shape. Advanced/free-text
    /// requests must pick "cpu" (with an explicit `image`) or "gpu".
    pub tier: String,
    /// Required for tier=cpu when `model_id` isn't one of the curated ids
    /// (the llamacpp engine needs a pre-built AIKit/GGUF image — there's no
    /// live HF→GGUF resolution path).
    #[serde(default)]
    pub image: Option<String>,
    /// GPU count for tier=gpu. Default 1.
    #[serde(default)]
    pub gpu_count: Option<i64>,
}

/// `POST /api/operator/local-inference/deployments` — create (or update, via
/// SSA) a `ModelDeployment`. Rejects tier=cpu requests with no resolvable
/// image rather than creating a ModelDeployment doomed to fail validation
/// with an opaque upstream error.
pub async fn create_local_model_deployment(
    State(state): State<AppState>,
    Json(req): Json<CreateLocalModelDeploymentRequest>,
) -> AppResult<Json<LocalModelDeploymentDto>> {
    let cluster = require_cluster(&state)?;
    if !is_dns1123_label(&req.name) {
        return Err(AppError::BadRequest(
            "name must be lowercase letters, digits, hyphens".into(),
        ));
    }
    let spec = match req.tier.as_str() {
        "cpu" => {
            let image = req.image.as_deref().filter(|i| !i.trim().is_empty())
                .or_else(|| aikit_image_for(&req.model_id))
                .ok_or_else(|| AppError::BadRequest(
                    "a CPU deployment needs a pre-built AIKit image — pick a curated model or supply spec.image for an advanced/free-text one".into(),
                ))?;
            serde_json::json!({
                "model": {"id": req.model_id},
                "engine": {"type": "llamacpp"},
                "image": image,
            })
        }
        "gpu" => {
            serde_json::json!({
                "model": {"id": req.model_id},
                "resources": {"gpu": {"count": req.gpu_count.unwrap_or(1), "type": "nvidia.com/gpu"}},
            })
        }
        other => {
            return Err(AppError::BadRequest(format!(
                "tier must be \"cpu\" or \"gpu\", got {other:?}"
            )));
        }
    };
    let obj = cluster
        .apply_model_deployment(&req.name, spec)
        .await
        .map_err(upstream)?;
    Ok(Json(project_model_deployment(&obj)))
}

/// `GET /api/operator/local-inference/deployments/:name/status` — rich LIVE
/// status for the deploy progress tracker: a milestone-derived percentage,
/// real pod/container state, and the actual Kubernetes event stream (image
/// pull, scheduling, container start/fail) for this deployment's pods.
pub async fn local_deployment_live_status(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<crate::kars::cluster::LocalDeployLiveStatus>> {
    let cluster = require_cluster(&state)?;
    let status = cluster
        .local_deployment_live_status(&name)
        .await
        .map_err(upstream)?;
    Ok(Json(status))
}

/// `DELETE /api/operator/local-inference/deployments/:name` — undeploy a
/// local model. The Bridge also removes it from the connected-providers list
/// if it had been auto-wired (see `auto_wire_local_provider` in routes/run.rs
/// or the corresponding poll path) — callers should not assume the
/// InferencePolicy-facing tag disappears atomically with the CR.
pub async fn delete_local_model_deployment(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    let cluster = require_cluster(&state)?;
    cluster
        .delete_model_deployment(&name)
        .await
        .map_err(upstream)?;
    // Best-effort: also drop it from the additional-providers secret if it
    // was auto-wired. Not fatal if it wasn't (e.g. deleted before Ready).
    let tag = format!("local-{name}");
    let tag_upper = tag.to_ascii_uppercase().replace('-', "_");
    let _ = cluster
        .mutate_secret_keys(INFERENCE_PROVIDERS_NS, INFERENCE_PROVIDERS_SECRET, |keys| {
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_ENDPOINT"));
            keys.remove(&format!("KARS_PROVIDER_{tag_upper}_MODELS"));
        })
        .await;
    Ok(Json(serde_json::json!({"deleted": true, "name": name})))
}

#[cfg(test)]
mod tests {
    use super::project_model_deployment;
    use kube::core::DynamicObject;
    use serde_json::json;

    #[test]
    fn projects_discovered_airunway_model_in_its_actual_namespace() {
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "airunway.ai/v1alpha1",
            "kind": "ModelDeployment",
            "metadata": {
                "name": "gpt-oss-120b",
                "namespace": "default"
            },
            "spec": {
                "model": {"id": "openai/gpt-oss-120b"}
            },
            "status": {
                "phase": "Running",
                "endpoint": {"service": "gpt-oss-120b", "port": 80},
                "engine": {"type": "vllm"},
                "provider": {"name": "kaito"}
            }
        }))
        .expect("valid dynamic object");

        let projected = project_model_deployment(&object);

        assert_eq!(projected.namespace, "default");
        assert!(!projected.managed);
        assert_eq!(
            projected.endpoint.as_deref(),
            Some("http://gpt-oss-120b.default.svc.cluster.local:80")
        );
        assert_eq!(projected.model_id.as_deref(), Some("openai/gpt-oss-120b"));
    }
}
