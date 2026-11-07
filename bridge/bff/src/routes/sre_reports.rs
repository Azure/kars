// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    auth::Principal,
    error::{AppError, AppResult},
    sre_report::{self, Identity, Report},
    state::AppState,
};
use axum::{Extension, Json, extract::State};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct ReportDto {
    pub state: &'static str,
    pub latest: Option<Report>,
}

fn empty(state: &'static str) -> ReportDto {
    ReportDto {
        state,
        latest: None,
    }
}

fn authority(reg: Option<&Value>) -> Result<Identity, &'static str> {
    let reg = reg.ok_or("revoked")?;
    if reg["spec"]["enabled"] != true || !reg["metadata"]["deletionTimestamp"].is_null() {
        return Err("revoked");
    }
    let identity = Identity::from_registration(reg).ok_or("replaced")?;
    if reg["status"]["phase"] != "Ready"
        || reg["metadata"]["generation"].as_u64().is_none()
        || reg["status"]["observedGeneration"] != reg["metadata"]["generation"]
        || reg["status"]["legacySecretAccessDenied"] != true
        || reg["status"]["privacyRevision"] != crate::sre_privacy::REVISION
        || reg["status"]["privacyEpoch"]
            .as_str()
            .is_none_or(str::is_empty)
    {
        return Err("stale");
    }
    Ok(identity)
}

fn classify(reg: &Value, identity: &Identity, target: Option<&Value>, now: u64) -> ReportDto {
    let Some(target) = target else {
        return empty("missing");
    };
    if !identity.matches_target(target) {
        return empty("replaced");
    }
    if !target["data"].is_null() && !target["data"].is_object() {
        return empty("invalid");
    }
    if target["data"]
        .as_object()
        .is_some_and(|data| data.keys().any(|key| key != sre_report::KEY))
    {
        return empty("invalid");
    }
    let Some(raw) = target["data"].get(sre_report::KEY) else {
        return empty("missing");
    };
    let Some(report) = raw.as_str().and_then(|raw| Report::decode(raw).ok()) else {
        return empty("invalid");
    };
    if &report.identity != identity {
        return empty("replaced");
    }
    let state = if reg["status"]["privacyEpoch"] != report.privacy_epoch || report.stale(now) {
        "stale"
    } else {
        "observed"
    };
    ReportDto {
        state,
        latest: Some(report),
    }
}

/// An operator read, never an agent publishing endpoint or a remediation decision.
pub async fn get_report(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> AppResult<Json<ReportDto>> {
    if !principal
        .roles
        .iter()
        .any(|role| role == "operator" || role == "admin")
    {
        return Err(AppError::Forbidden(
            "Operator report access required".into(),
        ));
    }
    let Some(cluster) = state.cluster() else {
        return Ok(Json(empty("unavailable")));
    };
    let reg = match cluster.sre_registration().await {
        Ok(reg) => reg,
        Err(_) => return Ok(Json(empty("unavailable"))),
    };
    let identity = match authority(reg.as_ref()) {
        Ok(identity) => identity,
        Err(reason) => return Ok(Json(empty(reason))),
    };
    match cluster.sre_report_source_current(&identity).await {
        Ok(true) => {}
        Ok(false) => return Ok(Json(empty("replaced"))),
        Err(_) => return Ok(Json(empty("unavailable"))),
    }
    let target = match cluster
        .configmaps(sre_report::NAMESPACE)
        .get_opt(sre_report::NAME)
        .await
    {
        Ok(target) => target.map(|v| serde_json::to_value(v).expect("ConfigMap serializes")),
        Err(_) => return Ok(Json(empty("unavailable"))),
    };
    // Re-read authority after storage; never serve a report across an observed retirement/replacement.
    let current = match cluster.sre_registration().await {
        Ok(reg) => reg,
        Err(_) => return Ok(Json(empty("unavailable"))),
    };
    match authority(current.as_ref()) {
        Err(reason) => return Ok(Json(empty(reason))),
        Ok(current_identity) if current_identity != identity => return Ok(Json(empty("replaced"))),
        Ok(_) => {}
    }
    if current != reg {
        return Ok(Json(empty("stale")));
    }
    match cluster.sre_report_source_current(&identity).await {
        Ok(true) => {}
        Ok(false) => return Ok(Json(empty("replaced"))),
        Err(_) => return Ok(Json(empty("unavailable"))),
    }
    Ok(Json(classify(
        reg.as_ref().expect("validated registration"),
        &identity,
        target.as_ref(),
        chrono::Utc::now().timestamp_millis() as u64,
    )))
}

#[cfg(test)]
#[path = "sre_reports_tests.rs"]
mod tests;
