// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    config::Settings,
    service::{self, Broker},
    store::Store,
};
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, ResourceExt};
use std::{net::SocketAddr, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;

const MAX_PENDING_HANDSHAKES: usize = 64;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
type Handshake = BoxFuture<'static, Option<(TlsStream<TcpStream>, SocketAddr)>>;

struct Listener {
    listener: TcpListener,
    tls: TlsAcceptor,
    pending: FuturesUnordered<Handshake>,
}

impl axum::serve::Listener for Listener {
    type Io = TlsStream<TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                completed = self.pending.next(), if !self.pending.is_empty() => {
                    if let Some(Some(connection)) = completed {
                        return connection;
                    }
                }
                accepted = self.listener.accept(), if self.pending.len() < MAX_PENDING_HANDSHAKES => {
                    let (stream, address) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(%error, "Inference budget TCP accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    };
                    let tls = self.tls.clone();
                    self.pending.push(Box::pin(async move {
                        match tokio::time::timeout(HANDSHAKE_TIMEOUT, tls.accept(stream)).await {
                            Ok(Ok(stream)) => Some((stream, address)),
                            Ok(Err(_)) => {
                                tracing::warn!("Inference budget TLS connection rejected");
                                None
                            }
                            Err(_) => {
                                tracing::warn!("Inference budget TLS handshake timed out");
                                None
                            }
                        }
                    }));
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}

pub async fn server_identity(
    client: &Client,
    settings: &Settings,
) -> anyhow::Result<std::sync::Arc<rustls::ServerConfig>> {
    crate::sre_authority::privacy_epoch(client, &settings.accounting_namespace)
        .await
        .map_err(|_| anyhow::anyhow!("Inference budget TLS privacy proof is unavailable"))?;
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &settings.accounting_namespace);
    let secret = secrets
        .get(&settings.tls_secret)
        .await
        .map_err(|_| anyhow::anyhow!("Inference budget TLS Secret is unavailable"))?;
    if secret.type_.as_deref() != Some("kubernetes.io/tls")
        || secret.metadata.uid.as_deref().is_none_or(str::is_empty)
        || secret.metadata.deletion_timestamp.is_some()
        || secret
            .annotations()
            .get("kars.azure.com/inference-budget-tls")
            .map(String::as_str)
            != Some("v1")
    {
        anyhow::bail!("Inference budget TLS Secret identity is invalid");
    }
    let data = secret
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Inference budget TLS material is absent"))?;
    let cert = data
        .get("tls.crt")
        .ok_or_else(|| anyhow::anyhow!("Inference budget certificate is absent"))?;
    let key = data
        .get("tls.key")
        .ok_or_else(|| anyhow::anyhow!("Inference budget key is absent"))?;
    crate::providers::signing::inference_budget_tls_config(&cert.0, &key.0)
}

pub async fn run(client: Client, settings: Settings) -> anyhow::Result<()> {
    settings
        .catalog(&client, chrono::Utc::now().timestamp())
        .await?;
    let tls = server_identity(&client, &settings).await?;
    let socket = TcpListener::bind(&settings.address).await?;
    let listener = Listener {
        listener: socket,
        tls: TlsAcceptor::from(tls),
        pending: FuturesUnordered::new(),
    };
    let store = Store::new(client.clone(), &settings.accounting_namespace);
    let router = service::router(Broker {
        client,
        settings,
        store,
    });
    axum::serve(listener, router).await?;
    Ok(())
}
