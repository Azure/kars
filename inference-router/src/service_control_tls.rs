// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{
    routes::AppState,
    service_control::{self, TlsIdentity},
};
use std::path::Path;
use tokio::net::TcpListener;

#[cfg(test)]
#[path = "service_control_tls_tests.rs"]
mod tests;

fn configuration(state: &AppState, bytes: &[u8]) -> Result<TlsIdentity, String> {
    let scope = state
        .services
        .requests
        .scope()
        .map_err(|_| "Control scope unavailable")?;
    let identity = serde_json::to_value(&scope.identity).map_err(|_| "Control identity invalid")?;
    let tls: TlsIdentity =
        serde_json::from_slice(bytes).map_err(|_| "Control TLS identity invalid")?;
    if !state.services.control_configured()
        || !scope.identity.valid(&scope.identity.sandbox.name)
        || !tls.valid_for(&identity, chrono::Utc::now().timestamp())
    {
        return Err("Control TLS identity does not match its credential scope".into());
    }
    Ok(tls)
}

pub async fn start(state: AppState) -> Result<Option<tokio::task::JoinHandle<()>>, String> {
    let scope = state
        .services
        .requests
        .scope()
        .map_err(|_| "Control scope unavailable")?;
    if scope.identity.task.is_none() {
        return Ok(None);
    }
    let bytes = std::fs::read(Path::new(service_control::DIRECTORY).join("config.json"))
        .map_err(|_| "Control TLS identity unavailable")?;
    let tls = configuration(&state, &bytes)?;
    let listener = crate::sre_proxy::Listener {
        tcp: TcpListener::bind(("0.0.0.0", service_control::PORT))
            .await
            .map_err(|_| "Control TLS listener unavailable")?,
        tls: crate::sre_proxy::tls_from_pem(
            tls.certificate_pem.as_bytes(),
            tls.private_key_pem.as_bytes(),
        )?,
    };
    let router = crate::routes::access_request_control_routes(state.clone()).with_state(state);
    Ok(Some(crate::service_observation_tls::serve(
        listener,
        router,
        "service-control",
    )))
}
