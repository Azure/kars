//! MCP 2026 Streamable HTTP — negative-only edge-case integration tests.
//!
//! These complement the broad in-tree pipeline corpus
//! (`mcp::pipeline::tests`) with the three off-happy-path scenarios
//! the Phase 1 plan explicitly calls out:
//!
//! 1. **Oversized frame at exactly `MAX_FRAME_BYTES + 1`** — the
//!    boundary between "accepted" and "rejected". The in-tree
//!    `body_at_exactly_max_frame_bytes_is_accepted` test pins the
//!    accept side; this file pins the reject side at the +1 boundary.
//!
//! 2. **`Mcp-Session-Id` minter collision** — a buggy minter that
//!    emits the same id twice must not panic the pipeline. This locks
//!    the contract for downstream session-tracking code (Phase 2).
//!
//! 3. **Batch with mixed JSON-RPC id types (Number, String, Null)** —
//!    JSON-RPC 2.0 §4 explicitly allows all three; the pipeline must
//!    preserve each id verbatim in the corresponding response slot.
//!
//! All tests call [`process_request`] directly, no axum, no network.

use azureclaw_inference_router::mcp::initialize::{InitializeConfig, SessionMinter};
use azureclaw_inference_router::mcp::pipeline::{ProcessOutcome, process_request};
use azureclaw_inference_router::mcp::streamable_http::{
    MAX_FRAME_BYTES, MCP_PROTOCOL_VERSION, SessionId,
};
use serde_json::{Value, json};

struct FixedMinter(&'static str);
impl SessionMinter for FixedMinter {
    fn mint(&self) -> SessionId {
        SessionId::try_new(self.0).expect("valid session id literal")
    }
}

fn ok_accept() -> Option<&'static str> {
    Some("application/json, text/event-stream")
}

// ---------------------------------------------------------------- 1.

#[test]
fn body_one_byte_over_max_frame_bytes_is_rejected() {
    // The in-tree pipeline corpus pins MAX_FRAME_BYTES *exactly* as
    // accepted. Here we pin MAX_FRAME_BYTES+1 as rejected. Together
    // they lock the boundary against any future off-by-one.
    let body = vec![b'x'; MAX_FRAME_BYTES + 1];
    let out = process_request(
        &body,
        ok_accept(),
        &InitializeConfig::default(),
        &FixedMinter("s-1"),
        None,
    );
    assert!(matches!(out, ProcessOutcome::PayloadTooLarge), "{out:?}");
}

#[test]
fn body_well_under_max_is_processed_normally() {
    // Sanity check that the boundary test is meaningful — i.e. a
    // small body doesn't ever map to PayloadTooLarge.
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "ping",
        "id": 1,
    }))
    .unwrap();
    assert!(body.len() < MAX_FRAME_BYTES);
    let out = process_request(
        &body,
        ok_accept(),
        &InitializeConfig::default(),
        &FixedMinter("s-2"),
        None,
    );
    assert!(
        matches!(out, ProcessOutcome::JsonRpcResponse { .. }),
        "{out:?}"
    );
}

// ---------------------------------------------------------------- 2.

#[test]
fn duplicate_session_id_from_minter_does_not_panic_pipeline() {
    // A minter that produces the same id every call is a bug, but it
    // must not crash the pipeline — we don't yet have an in-process
    // session ledger that would dedup, so what we *guarantee* is the
    // pipeline returns a normal JsonRpcResponse for each call. The
    // session-tracking layer's job is to detect collision; this test
    // pins the pipeline contract.
    let minter = FixedMinter("collision-id");
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0.0.0"},
        },
        "id": 1,
    }))
    .unwrap();
    let cfg = InitializeConfig::default();

    for call in 0..3 {
        let out = process_request(&body, ok_accept(), &cfg, &minter, None);
        match out {
            ProcessOutcome::JsonRpcResponse { session_id, .. } => {
                let sid = session_id.expect("initialize must yield a session id");
                assert_eq!(
                    sid.as_str(),
                    "collision-id",
                    "call #{call}: minter returns the same id deterministically"
                );
            }
            other => panic!("call #{call}: expected JsonRpcResponse, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------- 3.

#[test]
fn batch_with_mixed_id_types_preserves_each_id_verbatim() {
    // JSON-RPC 2.0 §4: id is allowed as String, Number, or Null. A
    // single batch may legitimately mix all three. The pipeline must
    // echo each id back in its corresponding response.
    //
    // We use `ping` (a no-op method handled by the pipeline) so each
    // request gets a result, and we assert on `result.timestamp`'s
    // presence rather than its value.
    let body = serde_json::to_vec(&json!([
        { "jsonrpc": "2.0", "method": "ping", "id": 7 },
        { "jsonrpc": "2.0", "method": "ping", "id": "alpha" },
        { "jsonrpc": "2.0", "method": "ping", "id": null },
    ]))
    .unwrap();

    let out = process_request(
        &body,
        ok_accept(),
        &InitializeConfig::default(),
        &FixedMinter("batch-session"),
        None,
    );

    let body = match out {
        ProcessOutcome::JsonRpcResponse { body, .. } => body,
        other => panic!("expected JsonRpcResponse, got {other:?}"),
    };
    let v: Value = serde_json::from_slice(&body).expect("response is JSON");
    let arr = v.as_array().expect("batch response is a JSON array");
    assert_eq!(arr.len(), 3, "one response per request: {arr:?}");

    // The pipeline preserves request order across batch items; assert
    // each id type by position.
    assert_eq!(arr[0]["id"], json!(7), "Number id round-trips");
    assert_eq!(arr[1]["id"], json!("alpha"), "String id round-trips");
    assert_eq!(arr[2]["id"], json!(null), "Null id round-trips");

    // Every response must carry a `result` (ping is a real method).
    for r in arr {
        assert_eq!(r["jsonrpc"], json!("2.0"));
        assert!(r.get("result").is_some(), "ping yields a result: {r:?}");
        assert!(r.get("error").is_none(), "ping must not error: {r:?}");
    }
}

#[test]
fn batch_with_only_null_ids_still_returns_responses_for_each() {
    // Edge case of (3): a batch where every id is Null. Each item is
    // still a *request* (not a notification — notifications omit the
    // id field entirely; explicit null is a request per JSON-RPC §4
    // wording "if it [id] is not included, it is assumed to be a
    // notification").
    let body = serde_json::to_vec(&json!([
        { "jsonrpc": "2.0", "method": "ping", "id": null },
        { "jsonrpc": "2.0", "method": "ping", "id": null },
    ]))
    .unwrap();
    let out = process_request(
        &body,
        ok_accept(),
        &InitializeConfig::default(),
        &FixedMinter("nulls"),
        None,
    );
    let body = match out {
        ProcessOutcome::JsonRpcResponse { body, .. } => body,
        other => panic!("expected JsonRpcResponse, got {other:?}"),
    };
    let v: Value = serde_json::from_slice(&body).unwrap();
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    for r in arr {
        assert_eq!(r["id"], json!(null));
    }
}
