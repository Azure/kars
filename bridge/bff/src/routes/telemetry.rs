// kars Bridge BFF — live telemetry stream (§8). Streams the WHOLE agent tree's
// real per-round / per-tool activity as it happens, so the activity stream and
// the expanding flow graph tick in flight instead of only at delivery.
//
// Source of truth: each sandbox's router exposes its live execution trace at the
// PUBLIC `GET /telemetry/trace` endpoint (derived from the model traffic it
// proxies — honest, never fabricated). This SSE endpoint tails that endpoint for
// the mission's PRINCIPAL sandbox AND every sub-agent the principal spawned
// (KarsSandboxes labelled `kars.azure.com/parent=<principal>`), tags each event
// with the agent that emitted it, and emits new events as they land. It falls
// back to the persisted `kars-mission-trace-<task>` ConfigMap when no live pod
// is reachable (e.g. after the run is retired), and closes when the deliverable
// is captured.

use axum::extract::{Extension, Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::Stream;

use crate::auth::Principal;
use crate::error::{AppError, AppResult};
use crate::routes::ownership::require_owned_task_or_output;
use crate::state::AppState;

/// Tag events with `seq` strictly greater than `watermark` (the router stamps a
/// monotonic `seq` per event) with the agent that emitted them and its role in
/// the tree. Returns the tagged new events AND the new watermark (max seq seen).
/// Keying on `seq` — not array length — is correct even when the router's
/// bounded trace buffer evicts old events (length stops growing while seq keeps
/// climbing), so events are never silently dropped or re-emitted.
fn tag_new(
    events: &[serde_json::Value],
    watermark: u64,
    agent: &str,
    role: &str,
) -> (Vec<serde_json::Value>, u64) {
    let mut max_seq = watermark;
    let out = events
        .iter()
        .filter_map(|e| {
            let seq = e.get("seq").and_then(|s| s.as_u64())?;
            if seq <= watermark {
                return None;
            }
            if seq > max_seq {
                max_seq = seq;
            }
            let mut ev = e.clone();
            if let Some(obj) = ev.as_object_mut() {
                obj.insert("agent".into(), serde_json::json!(agent));
                obj.insert("agentInstance".into(), serde_json::json!(agent));
                obj.insert("agentRole".into(), serde_json::json!(role));
            }
            Some(ev)
        })
        .collect();
    (out, max_seq)
}

/// `GET /api/namespaces/:ns/tasks/:name/stream` — Server-Sent Events of the
/// mission's LIVE execution trace across the whole agent tree. Each event is one
/// real per-tool / per-round record, tagged with `agent` + `agentRole`. Sends a
/// terminal `done` event when the deliverable is captured.
pub async fn stream_mission(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Path((ns, name)): Path<(String, String)>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let live_cluster = state.cluster().ok_or(AppError::ClusterUnavailable)?;
    require_owned_task_or_output(live_cluster, &ns, &name, &principal).await?;
    let cluster = state.cluster().cloned();
    let s = async_stream::stream! {
        // Per-agent high-water mark = the max event `seq` already emitted.
        let mut emitted: HashMap<String, u64> = HashMap::new();
        let mut emitted_count: usize = 0;
        let mut saw_live = false;
        let mut cm_backfilled = false;
        let mut idle = 0u32;

        while let Some(c) = &cluster {

            // Resolve the principal sandbox for this task (its status.sandboxRef).
            let principal_sandbox = c
                .tasks(&ns)
                .get_opt(&name)
                .await
                .ok()
                .flatten()
                .and_then(|t| t.status.and_then(|s| s.sandbox_ref).map(|r| r.name));

            let mut batch: Vec<serde_json::Value> = Vec::new();

            if let Some(principal) = principal_sandbox.as_deref() {
                // Principal live trace.
                let p_events = c.sandbox_live_trace(principal).await;
                if !p_events.is_empty() {
                    saw_live = true;
                    let seen = emitted.entry(principal.to_string()).or_insert(0);
                    let (fresh, new_max) = tag_new(&p_events, *seen, principal, "principal");
                    *seen = new_max;
                    batch.extend(fresh);
                }

                // Every spawned sub-agent's live trace (the folding graph fan-out).
                let mut descendants = c
                    .sub_agent_sandbox_names(&ns, principal)
                    .await
                    .into_iter();
                loop {
                    let sub_batch = descendants.by_ref().take(8).collect::<Vec<_>>();
                    if sub_batch.is_empty() {
                        break;
                    }
                    let mut polling = tokio::task::JoinSet::new();
                    for sub in sub_batch {
                        let cluster = c.clone();
                        polling.spawn(async move {
                            let events = cluster.sandbox_live_trace(&sub).await;
                            (sub, events)
                        });
                    }
                    while let Some(result) = polling.join_next().await {
                        let Ok((sub, s_events)) = result else {
                            continue;
                        };
                        if s_events.is_empty() {
                            continue;
                        }
                        saw_live = true;
                        let seen = emitted.entry(sub.clone()).or_insert(0);
                        let (fresh, new_max) = tag_new(&s_events, *seen, &sub, "subagent");
                        *seen = new_max;
                        batch.extend(fresh);
                    }
                }
            }

            // Fallback: no live pod reachable (pre-launch or post-retire) — tail
            // the persisted trace CM ONCE so the surface still shows the run. The
            // persisted (agent-self-reported) trace may lack the router's `seq`,
            // so we emit it wholesale here rather than via the seq-dedup path;
            // `cm_backfilled` guards the single emission.
            if !saw_live && !cm_backfilled
                && let Some(raw) = c.read_mission_trace(&name).await
                    && let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(&raw) {
                        for e in &events {
                            let mut ev = e.clone();
                            if let Some(obj) = ev.as_object_mut() {
                                obj.insert("agent".into(), serde_json::json!(name));
                                obj.insert("agentRole".into(), serde_json::json!("principal"));
                            }
                            batch.push(ev);
                        }
                        if !events.is_empty() {
                            cm_backfilled = true;
                        }
                    }

            if !batch.is_empty() {
                idle = 0;
                emitted_count += batch.len();
                for ev in batch {
                    if let Ok(sse) = Event::default().json_data(&ev) {
                        yield Ok(sse);
                    }
                }
            }

            // Deliverable captured → run finished; emit done + close.
            if c.read_mission_artifacts(&name).await.is_some() {
                let total: usize = emitted_count;
                if let Ok(done) = Event::default()
                    .event("done")
                    .json_data(serde_json::json!({ "events": total }))
                {
                    yield Ok(done);
                }
                break;
            }

            idle += 1;
            if idle > 150 {
                break; // ~5 min ceiling without progress
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    };
    Ok(Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
