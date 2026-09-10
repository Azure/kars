// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::{access_request::Identity, governed_services::GovernedServices};

#[tokio::test]
async fn denied_connect_records_a_scoped_request_without_an_implicit_wait() {
    let services = Arc::new(GovernedServices::new(Identity::standalone("test"), None));
    let scope = services.requests.scope().unwrap().id;
    let blocked = Arc::new(BlockedBuffer::with_defaults());
    blocked.bind_services(&services);
    let blocklist = Blocklist::new(None).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        handle_connection(socket, &blocklist, "test", &blocked)
            .await
            .unwrap();
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(b"CONNECT blocked.example:443 HTTP/1.1\r\nHost: blocked.example\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 403"));
    let entries = services.requests.snapshot(&scope).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target, "blocked.example");
    assert_eq!(entries[0].port, Some(443));
    assert_eq!(services.wait_slots(), 16);
}
