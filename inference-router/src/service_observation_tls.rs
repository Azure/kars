// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use crate::{routes::AppState, service_observer};
use axum::extract::{ConnectInfo, Request, connect_info::Connected};
use serde_json::Value;
use std::{net::SocketAddr, path::Path};
use tokio::net::TcpListener;

#[cfg(test)]
#[path = "service_observation_tls_tests.rs"]
mod tests;

#[derive(Clone)]
struct Peer(SocketAddr);

impl Connected<axum::serve::IncomingStream<'_, crate::sre_proxy::Listener>> for Peer {
    fn connect_info(stream: axum::serve::IncomingStream<'_, crate::sre_proxy::Listener>) -> Self {
        Self(*stream.remote_addr())
    }
}

async fn socket_peer(mut request: Request) -> Request {
    if let Some(ConnectInfo(Peer(peer))) = request.extensions().get::<ConnectInfo<Peer>>() {
        let peer = *peer;
        request.extensions_mut().insert(ConnectInfo(peer));
    }
    request
}

pub async fn start(state: AppState) -> Result<Option<tokio::task::JoinHandle<()>>, String> {
    let Some(observer) = state.services.observer.as_ref() else {
        return Ok(None);
    };
    let config: Value = serde_json::from_slice(
        &std::fs::read(Path::new(service_observer::TLS_DIRECTORY).join("config.json"))
            .map_err(|_| "Observation TLS identity unavailable")?,
    )
    .map_err(|_| "Observation TLS identity invalid")?;
    if config["identity"] != observer.binding().identity
        || config["serverName"] != observer.binding().server_name
        || config["caPem"] != observer.binding().ca_pem
    {
        return Err("Observation TLS identity does not match its credential scope".into());
    }

    let certificate = config["certificatePem"]
        .as_str()
        .ok_or("Observation certificate missing")?;
    let key = config["privateKeyPem"]
        .as_str()
        .ok_or("Observation private key missing")?;
    let listener = crate::sre_proxy::Listener {
        tcp: TcpListener::bind(("0.0.0.0", service_observer::PORT))
            .await
            .map_err(|_| "Observation TLS listener unavailable")?,
        tls: crate::sre_proxy::tls_from_pem(certificate.as_bytes(), key.as_bytes())?,
    };
    let router = crate::routes::observation_routes(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::routes::observation_purpose_boundary,
        ))
        .layer(axum::middleware::map_request(socket_peer))
        .with_state(state);
    Ok(Some(tokio::spawn(async move {
        if axum::serve(
            listener,
            router.into_make_service_with_connect_info::<Peer>(),
        )
        .await
        .is_err()
        {
            tracing::error!("Private observation listener stopped");
            std::process::exit(1);
        }
    })))
}
