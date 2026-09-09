// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use std::{
    io::{self, BufReader},
    net::SocketAddr,
    sync::Arc,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

pub(crate) struct Listener {
    pub(crate) tcp: TcpListener,
    pub(crate) tls: TlsAcceptor,
}

impl axum::serve::Listener for Listener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.tcp.accept().await {
                Ok((stream, address)) => {
                    if let Ok(Ok(stream)) = tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        self.tls.accept(stream),
                    )
                    .await
                    {
                        return (stream, address);
                    }
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

pub(crate) fn tls_from_pem(certificates: &[u8], key: &[u8]) -> Result<TlsAcceptor, String> {
    let certificates = rustls_pemfile::certs(&mut BufReader::new(certificates))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Private TLS certificate invalid")?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(key))
        .map_err(|_| "Private TLS key invalid")?
        .ok_or("Private TLS key missing")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "Private TLS certificate/key mismatch")?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
