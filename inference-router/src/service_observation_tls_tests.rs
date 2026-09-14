// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{Router, routing::get};

#[tokio::test]
async fn observation_tls_reports_actual_peer_and_rejects_untrusted_or_wrong_uid_hosts() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let host = "observer-sandbox-uid.kars.internal";
    let key = rcgen::KeyPair::generate().unwrap();
    let certificate = rcgen::CertificateParams::new(vec![host.into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let listener = crate::sre_proxy::Listener {
        tcp: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        tls: crate::sre_proxy::tls_from_pem(
            certificate.pem().as_bytes(),
            key.serialize_pem().as_bytes(),
        )
        .unwrap(),
    };
    let address = listener.tcp.local_addr().unwrap();
    let router = Router::new()
        .route(
            "/peer",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        )
        .layer(axum::middleware::map_request(socket_peer));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<Peer>(),
        )
        .await
        .unwrap();
    });
    let client = |hostname: &str, trusted: bool| {
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(hostname, address)
            .timeout(std::time::Duration::from_secs(3));
        if trusted {
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(certificate.pem().as_bytes()).unwrap(),
            );
        }
        builder.build().unwrap()
    };
    let endpoint = |hostname: &str| format!("https://{hostname}:{}/peer", address.port());
    let response = client(host, true).get(endpoint(host)).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.text().await.unwrap(), "127.0.0.1");
    assert!(
        client(host, false)
            .get(endpoint(host))
            .send()
            .await
            .is_err()
    );
    let wrong = "observer-replacement-uid.kars.internal";
    assert!(
        client(wrong, true)
            .get(endpoint(wrong))
            .send()
            .await
            .is_err()
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
