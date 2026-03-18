//! Controller reconciliation loop — watches ClawSandbox CRDs and reconciles state.

use anyhow::Result;
use futures::StreamExt;
use kube::{
    api::{Api, ListParams},
    runtime::controller::{Action, Controller},
    Client, ResourceExt,
};
use std::sync::Arc;
use tokio::time::Duration;

use crate::crd::ClawSandbox;

/// Shared controller context.
struct Context {
    client: Client,
}

/// Main reconciliation function — called whenever a ClawSandbox changes.
async fn reconcile(sandbox: Arc<ClawSandbox>, ctx: Arc<Context>) -> Result<Action, kube::Error> {
    let name = sandbox.name_any();
    let ns = sandbox.namespace().unwrap_or_default();
    tracing::info!("Reconciling ClawSandbox {name} in {ns}");

    // TODO: Implementation phases:
    // 1. Create namespace `azureclaw-{name}` if not exists
    // 2. Create ServiceAccount with Workload Identity annotations
    // 3. Apply NetworkPolicy (default-deny + allowlist from spec)
    // 4. Create sandbox pod:
    //    - Azure Linux 4 base image with OpenClaw
    //    - seccomp profile, SELinux context, read-only rootfs
    //    - Envoy sidecar for L7 filtering
    //    - Resource limits from spec
    // 5. Configure inference routing (model, endpoint, content safety)
    // 6. Provision Azure service connectors (Workload Identity + RBAC)
    // 7. Update status (phase, pod name, token usage)

    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy — what to do when reconciliation fails.
fn error_policy(sandbox: Arc<ClawSandbox>, error: &kube::Error, _ctx: Arc<Context>) -> Action {
    tracing::error!(
        "Reconciliation error for {}: {:?}",
        sandbox.name_any(),
        error
    );
    Action::requeue(Duration::from_secs(30))
}

/// Run the controller — blocks forever, watching ClawSandbox CRDs.
pub async fn run(client: Client) -> Result<()> {
    let sandboxes: Api<ClawSandbox> = Api::all(client.clone());

    // Verify CRD is installed
    sandboxes.list(&ListParams::default().limit(1)).await?;
    tracing::info!("ClawSandbox CRD found — starting controller");

    let ctx = Arc::new(Context { client });

    Controller::new(sandboxes, kube::runtime::watcher::Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok(o) => tracing::info!("Reconciled {:?}", o),
                Err(e) => tracing::warn!("Reconcile failed: {e:?}"),
            }
        })
        .await;

    Ok(())
}
