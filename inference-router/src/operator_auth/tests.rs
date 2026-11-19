// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use axum::{Router, body::Body, routing::get};
use std::net::SocketAddrV6;
use tower::ServiceExt;

const SERVER_PORT: u16 = 8443;
const ROUTER_UID: u32 = 1001;

fn v4(a: [u8; 4], port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::from(a)), port)
}

fn entry(local: SocketAddr, state: u8, uid: u32, inode: u64) -> TcpSocketEntry {
    TcpSocketEntry {
        local,
        remote: v4([127, 0, 0, 1], SERVER_PORT),
        state,
        uid,
        inode,
    }
}

fn hex_v4(addr: Ipv4Addr) -> String {
    format!("{:08X}", u32::from_ne_bytes(addr.octets()))
}

fn hex_v6(addr: Ipv6Addr) -> String {
    addr.octets()
        .chunks_exact(4)
        .map(|c| format!("{:08X}", u32::from_ne_bytes([c[0], c[1], c[2], c[3]])))
        .collect()
}

fn row(local: &str, remote: &str, state: &str, uid: u32, inode: u64) -> String {
    format!(
        "   0: {local} {remote} {state} 00000000:00000000 00:00000000 00000000 {uid:>5}        0 {inode} 1 0000000000000000 20 4 30 10 -1"
    )
}

const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

#[test]
fn parses_ipv4_rows() {
    let contents = format!(
        "{HEADER}\n{}\n",
        row(
            &format!("{}:D431", hex_v4(Ipv4Addr::new(127, 0, 0, 1))),
            &format!("{}:20FB", hex_v4(Ipv4Addr::new(10, 244, 1, 4))),
            "01",
            1000,
            4242
        )
    );
    let parsed = parse_proc_net_tcp(&contents);
    assert_eq!(
        parsed,
        vec![TcpSocketEntry {
            local: v4([127, 0, 0, 1], 0xD431),
            remote: v4([10, 244, 1, 4], 8443),
            state: TCP_ESTABLISHED,
            uid: 1000,
            inode: 4242,
        }]
    );
}

#[test]
fn parses_ipv6_rows_and_maps_ipv4_mapped_addresses() {
    let native: Ipv6Addr = "fd00::1:2".parse().unwrap();
    let mapped = Ipv4Addr::new(10, 244, 1, 4).to_ipv6_mapped();
    let contents = format!(
        "{HEADER}\n{}\n{}\n",
        row(
            &format!("{}:1F90", hex_v6(native)),
            &format!("{}:20FB", hex_v6(Ipv6Addr::LOCALHOST)),
            "01",
            0,
            7
        ),
        row(
            &format!("{}:1F91", hex_v6(mapped)),
            &format!("{}:20FB", hex_v6(mapped)),
            "06",
            1001,
            0
        )
    );
    let parsed = parse_proc_net_tcp(&contents);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].local, SocketAddr::new(IpAddr::V6(native), 8080));
    assert_eq!(
        parsed[0].remote,
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8443)
    );
    assert_eq!(parsed[1].local, v4([10, 244, 1, 4], 8081));
    assert_eq!(parsed[1].state, 0x06);
    assert_eq!(parsed[1].uid, 1001);
    assert_eq!(parsed[1].inode, 0);
}

#[test]
fn skips_header_and_malformed_rows() {
    let good = row("0100007F:D431", "0100007F:20FB", "01", 1000, 9);
    let contents = format!(
        "{HEADER}\n   1: 0100007F 0100007F:20FB 01\n   2: ZZZZZZZZ:D431 0100007F:20FB 01 0:0 0:0 0 1000 0 9\n   3: 0100007F:D431 0100007F:20FB 01 0:0 0:0 0 notanumber 0 9\n{good}\n"
    );
    let parsed = parse_proc_net_tcp(&contents);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].inode, 9);
}

#[test]
fn parses_socket_links_and_fsuid() {
    assert_eq!(parse_socket_link("socket:[123456]"), Some(123_456));
    assert_eq!(parse_socket_link("pipe:[123456]"), None);
    assert_eq!(parse_socket_link("socket:[abc]"), None);
    let status =
        "Name:\tkars-inference\nUid:\t1001\t1001\t1001\t1002\nGid:\t1001\t1001\t1001\t1001\n";
    assert_eq!(parse_status_fsuid(status), Some(1002));
    assert_eq!(parse_status_fsuid("Name:\tx\n"), None);
}

fn decide(sockets: &[TcpSocketEntry], self_inodes: &[u64]) -> LocalPeerDecision {
    let self_inodes: HashSet<u64> = self_inodes.iter().copied().collect();
    attribute_local_peer(
        sockets,
        v4([127, 0, 0, 1], 40000),
        SERVER_PORT,
        &self_inodes,
        Some(ROUTER_UID),
    )
}

#[test]
fn attribution_allows_router_uid_and_root() {
    let peer = v4([127, 0, 0, 1], 40000);
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, ROUTER_UID, 10)], &[]),
        LocalPeerDecision::Allow { uid: ROUTER_UID }
    );
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, 0, 10)], &[]),
        LocalPeerDecision::Allow { uid: 0 }
    );
}

#[test]
fn attribution_denies_agent_and_other_uids() {
    let peer = v4([127, 0, 0, 1], 40000);
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, 1000, 10)], &[]),
        LocalPeerDecision::Deny(DenyReason::UntrustedUid(1000))
    );
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, 65534, 10)], &[]),
        LocalPeerDecision::Deny(DenyReason::UntrustedUid(65534))
    );
}

#[test]
fn attribution_fails_closed_on_missing_or_unusual_sockets() {
    let peer = v4([127, 0, 0, 1], 40000);
    let other_peer = v4([127, 0, 0, 1], 40001);
    let wrong_server = TcpSocketEntry {
        remote: v4([127, 0, 0, 1], 9000),
        ..entry(peer, TCP_ESTABLISHED, 0, 10)
    };
    assert_eq!(
        decide(&[], &[]),
        LocalPeerDecision::Deny(DenyReason::SocketNotFound)
    );
    assert_eq!(
        decide(
            &[entry(other_peer, TCP_ESTABLISHED, 0, 10), wrong_server],
            &[]
        ),
        LocalPeerDecision::Deny(DenyReason::SocketNotFound)
    );
    // TIME_WAIT rows report UID 0 and inode 0.
    assert_eq!(
        decide(&[entry(peer, 0x06, 0, 0)], &[]),
        LocalPeerDecision::Deny(DenyReason::SocketNotEstablished)
    );
    // Orphaned sockets keep their state but lose the inode.
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, 0, 0)], &[]),
        LocalPeerDecision::Deny(DenyReason::SocketNotEstablished)
    );
    assert_eq!(
        decide(
            &[
                entry(peer, TCP_ESTABLISHED, 0, 10),
                entry(peer, TCP_ESTABLISHED, 1000, 11)
            ],
            &[]
        ),
        LocalPeerDecision::Deny(DenyReason::AmbiguousOwner)
    );
    assert_eq!(
        decide(
            &[entry(peer, TCP_ESTABLISHED, 0, 10), entry(peer, 0x06, 0, 0)],
            &[]
        ),
        LocalPeerDecision::Deny(DenyReason::SocketNotEstablished)
    );
}

#[test]
fn attribution_denies_router_self_connections() {
    let peer = v4([127, 0, 0, 1], 40000);
    assert_eq!(
        decide(&[entry(peer, TCP_ESTABLISHED, ROUTER_UID, 10)], &[10]),
        LocalPeerDecision::Deny(DenyReason::SelfOriginated)
    );
}

#[test]
fn attribution_without_router_uid_still_allows_root_only() {
    let peer = v4([127, 0, 0, 1], 40000);
    let none = HashSet::new();
    assert_eq!(
        attribute_local_peer(
            &[entry(peer, TCP_ESTABLISHED, ROUTER_UID, 10)],
            peer,
            SERVER_PORT,
            &none,
            None
        ),
        LocalPeerDecision::Deny(DenyReason::UntrustedUid(ROUTER_UID))
    );
    assert_eq!(
        attribute_local_peer(
            &[entry(peer, TCP_ESTABLISHED, 0, 10)],
            peer,
            SERVER_PORT,
            &none,
            None
        ),
        LocalPeerDecision::Allow { uid: 0 }
    );
}

#[test]
fn attribution_matches_ipv4_mapped_peer() {
    let mapped = SocketAddr::V6(SocketAddrV6::new(
        Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped(),
        40000,
        0,
        0,
    ));
    let none = HashSet::new();
    assert_eq!(
        attribute_local_peer(
            &[entry(v4([127, 0, 0, 1], 40000), TCP_ESTABLISHED, 1000, 10)],
            mapped,
            SERVER_PORT,
            &none,
            Some(ROUTER_UID)
        ),
        LocalPeerDecision::Deny(DenyReason::UntrustedUid(1000))
    );
}

#[test]
fn locality_distinguishes_local_and_remote_addresses() {
    assert!(is_local_address(v4([127, 0, 0, 1], 5)));
    assert!(is_local_address(SocketAddr::new(
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        5
    )));
    // TEST-NET-3 (RFC 5737) is never assigned to a local interface.
    assert!(!is_local_address(v4([203, 0, 113, 7], 5)));
}

async fn ok() -> &'static str {
    "ok"
}

fn gated_app(token: &str) -> Router {
    let auth = Arc::new(OperatorAuth::with_router_uid(
        Arc::new(token.to_string()),
        Arc::new(Vec::new()),
        SERVER_PORT,
        Some(ROUTER_UID),
    ));
    Router::new()
        .route("/egress/learned", get(ok))
        .layer(axum::middleware::from_fn(move |req, next| {
            operator_auth_middleware(auth.clone(), req, next)
        }))
}

fn request(peer: Option<SocketAddr>, token: Option<&str>) -> Request {
    let mut builder = Request::builder().uri("/egress/learned");
    if let Some(t) = token {
        builder = builder.header("authorization", format!("Bearer {t}"));
    }
    let mut req = builder.body(Body::empty()).unwrap();
    if let Some(peer) = peer {
        req.extensions_mut().insert(ConnectInfo(peer));
    }
    req
}

#[tokio::test]
async fn remote_and_unknown_peers_keep_admin_token_semantics() {
    let app = gated_app("s3cret");
    let remote = Some(v4([203, 0, 113, 7], 40000));
    for peer in [None, remote] {
        let resp = app.clone().oneshot(request(peer, None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .clone()
            .oneshot(request(peer, Some("wrong")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = app
            .clone()
            .oneshot(request(peer, Some("s3cret")))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn remote_peers_still_honour_origin_allowlist() {
    let auth = Arc::new(OperatorAuth::with_router_uid(
        Arc::new("s3cret".to_string()),
        Arc::new(vec!["203.0.113.8".parse().unwrap()]),
        SERVER_PORT,
        Some(ROUTER_UID),
    ));
    let app = Router::new()
        .route("/egress/learned", get(ok))
        .layer(axum::middleware::from_fn(move |req, next| {
            operator_auth_middleware(auth.clone(), req, next)
        }));
    let resp = app
        .clone()
        .oneshot(request(Some(v4([203, 0, 113, 7], 1)), Some("s3cret")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = app
        .oneshot(request(Some(v4([203, 0, 113, 8], 1)), Some("s3cret")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn local_peer_without_matching_socket_is_refused_even_with_token() {
    // No live socket uses this source port, so attribution fails
    // closed — a valid token does not substitute for attribution.
    let resp = gated_app("s3cret")
        .oneshot(request(Some(v4([127, 0, 0, 1], 1)), Some("s3cret")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn attributes_real_kernel_sockets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let server_port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());

        let sockets = read_tcp_sockets().unwrap();
        let fsuid = read_self_fsuid().expect("fsuid");
        let self_inodes = read_self_socket_inodes().unwrap();

        // As seen from another process owning this UID: allowed only
        // when the UID is trusted.
        let none = HashSet::new();
        assert_eq!(
            attribute_local_peer(&sockets, peer, server_port, &none, Some(fsuid)),
            LocalPeerDecision::Allow { uid: fsuid }
        );
        if fsuid != 0 {
            assert_eq!(
                attribute_local_peer(&sockets, peer, server_port, &none, None),
                LocalPeerDecision::Deny(DenyReason::UntrustedUid(fsuid))
            );
        }
        // As seen from the process that opened the client socket.
        assert_eq!(
            attribute_local_peer(&sockets, peer, server_port, &self_inodes, Some(fsuid)),
            LocalPeerDecision::Deny(DenyReason::SelfOriginated)
        );
    }

    #[tokio::test]
    async fn served_router_refuses_self_originated_requests() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let auth = Arc::new(OperatorAuth::new(
            Arc::new("s3cret".to_string()),
            Arc::new(Vec::new()),
            addr.port(),
        ));
        let app = Router::new()
            .route("/egress/learned", get(ok))
            .layer(axum::middleware::from_fn(move |req, next| {
                operator_auth_middleware(auth.clone(), req, next)
            }));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let response = tokio::task::spawn_blocking(move || {
            let mut stream = TcpStream::connect(addr).unwrap();
            write!(
                stream,
                "GET /egress/learned HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer s3cret\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut out = String::new();
            stream.read_to_string(&mut out).unwrap();
            out
        })
        .await
        .unwrap();
        server.abort();

        assert!(
            response.starts_with("HTTP/1.1 403"),
            "unexpected response: {response}"
        );
        assert!(response.contains(OPERATOR_DENIED_BODY));
    }
}
