// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{
    config::Settings,
    service::{self, Broker},
    store::Store,
};
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client, ResourceExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

struct Listener {
    listener: TcpListener,
    tls: TlsAcceptor,
}

impl axum::serve::Listener for Listener {
    type Io = TlsStream<TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, address)) = self.listener.accept().await else {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            if let Ok(Ok(tls)) =
                tokio::time::timeout(std::time::Duration::from_secs(10), self.tls.accept(stream))
                    .await
            {
                return (tls, address);
            }
            tracing::warn!("Inference budget TLS connection rejected");
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
