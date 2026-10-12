// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{HANDSHAKE_TIMEOUT, Listener, MAX_PENDING_HANDSHAKES};
use axum::serve::Listener as _;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};

async fn fixture() -> (Listener, TlsConnector) {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let server = crate::providers::signing::inference_budget_tls_config(
        identity.cert.pem().as_bytes(),
        identity.key_pair.serialize_pem().as_bytes(),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(identity.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (
        Listener {
            listener: tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
            tls: TlsAcceptor::from(server),
            pending: Default::default(),
        },
        TlsConnector::from(Arc::new(client)),
    )
}

async fn verified_connection(listener: &mut Listener, connector: &TlsConnector) {
    let stream = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let peer = stream.local_addr().unwrap();
    let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
    let ((_, accepted_peer), connected) =
        tokio::join!(listener.accept(), connector.connect(name, stream));
    connected.unwrap();
    assert_eq!(accepted_peer, peer);
}

#[tokio::test]
async fn stalled_handshake_does_not_block_a_verified_budget_connection() {
    let (mut listener, connector) = fixture().await;
    let stalled = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        verified_connection(&mut listener, &connector),
    )
    .await;
    drop(stalled);
    assert!(
        outcome.is_ok(),
        "an unfinished TLS handshake blocked an independent verified budget connection"
    );
}

#[tokio::test]
async fn plaintext_is_rejected_without_preventing_a_verified_connection() {
    let (mut listener, connector) = fixture().await;
    let mut plaintext = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    plaintext
        .write_all(b"GET / HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    plaintext.shutdown().await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(2),
        verified_connection(&mut listener, &connector),
    )
    .await
    .expect("a rejected plaintext connection must not stop the TLS listener");
}

#[tokio::test]
async fn pending_handshakes_are_bounded_and_closed_peers_release_slots() {
    let (mut listener, connector) = fixture().await;
    let mut stalled = Vec::new();
    for _ in 0..=MAX_PENDING_HANDSHAKES {
        stalled.push(
            TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap(),
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(listener.pending.len(), MAX_PENDING_HANDSHAKES);
    drop(stalled);
    tokio::time::timeout(
        Duration::from_secs(2),
        verified_connection(&mut listener, &connector),
    )
    .await
    .expect("closed peers must release handshake capacity");
}

#[tokio::test]
async fn stalled_handshake_expires_without_admitting_plaintext() {
    let (mut listener, connector) = fixture().await;
    let mut stalled = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(
            HANDSHAKE_TIMEOUT + Duration::from_millis(200),
            listener.accept()
        )
        .await
        .is_err()
    );
    assert!(listener.pending.is_empty());
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        verified_connection(&mut listener, &connector),
    )
    .await
    .expect("expired handshakes must not prevent verified connections");
}

#[tokio::test]
async fn dropping_listener_closes_pending_handshakes() {
    let (mut listener, _) = fixture().await;
    let mut stalled = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(listener.pending.len(), 1);
    drop(listener);
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
