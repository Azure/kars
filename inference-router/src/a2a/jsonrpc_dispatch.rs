//! A2A 1.0.0 JSON-RPC method dispatch — pure handler layer.
//!
//! Spec: <https://a2a-protocol.org/v1.0.0/specification#33-jsonrpc-binding>
//!
//! Once an inbound caller has been authenticated via
//! [`super::card_verifier::verify_inbound_card`], the gateway daemon
//! decodes the JSON-RPC frame and calls one of the handlers in this
//! module. All handlers are pure functions over a [`TaskStore`]
//! trait + the inbound [`Request`]; no I/O, no logging, no global
//! state.
//!
//! ## Methods covered
//!
//! - `message/send` — start a new task. Synchronous variant per spec
//!   §3.3.1. Returns a freshly minted Task in state `submitted`.
//! - `tasks/get` — fetch a task by id. Spec §3.3.2.
//! - `tasks/cancel` — request cancellation of a running task. Spec
//!   §3.3.3. Returns the (possibly already-terminal) task state.
//!
//! ## Methods NOT covered yet
//!
//! - `message/stream` — SSE streaming variant; binds to live
//!   transport, lands with the gateway daemon.
//! - `tasks/pushNotificationConfig/{set,get}` — push-notification
//!   webhook setup; out of scope for inbound A2A v0.
//! - `agent/getAuthenticatedExtendedCard` — needs the live signing
//!   provider; lands with route binding.
//!
//! ## Total-function discipline
//!
//! Every handler returns a fully-formed
//! [`crate::mcp::jsonrpc::Response`] (we reuse MCP's JSON-RPC
//! envelope — both protocols are JSON-RPC 2.0 over HTTP). Validation
//! failures map to JSON-RPC `error` objects; A2A application errors
//! use the codes in [`super::error::A2aErrorCode`]. No panics, no
//! silent acceptance.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::error::{ErrorCode, JsonRpcError};
use crate::mcp::jsonrpc::{Request, Response};

use super::error::A2aErrorCode;

/// Lifecycle states of a Task per spec §4.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Canceled,
    Failed,
    Rejected,
    AuthRequired,
}

impl TaskState {
    /// Per spec, terminal states cannot transition further. Cancel
    /// requests on terminal tasks raise `TaskNotCancelable`.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed
                | TaskState::Canceled
                | TaskState::Failed
                | TaskState::Rejected
        )
    }
}

/// One A2A Task. Wire format mirrors spec §4.5.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub state: TaskState,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub history: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub artifacts: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// One Message within a Task. Spec §4.6.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: String,
    pub parts: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// Backing store for tasks. The gateway provides a real impl
/// (Foundry-backed or in-cluster ConfigMap/Lease-backed); tests
/// inject [`InMemoryTaskStore`].
pub trait TaskStore: Send + Sync {
    fn create(&self, task: Task) -> Result<Task, StoreError>;
    fn get(&self, id: &str) -> Result<Task, StoreError>;
    fn update_state(&self, id: &str, new_state: TaskState) -> Result<Task, StoreError>;
}

/// Errors a [`TaskStore`] can raise.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StoreError {
    #[error("task `{0}` not found")]
    NotFound(String),
    #[error("task id `{0}` already exists")]
    Duplicate(String),
    #[error("task `{0}` is in terminal state and cannot be updated")]
    TerminalState(String),
    #[error("backing store i/o error: {0}")]
    Io(String),
}

/// Strategy injected by the gateway to mint task ids.
pub trait TaskIdMinter: Send + Sync {
    fn mint(&self) -> String;
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSendParams {
    pub message: Message,
    #[serde(default)]
    pub context_id: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksGetParams {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TasksCancelParams {
    pub id: String,
}

/// Handle `message/send`.
pub fn handle_message_send(
    req: &Request,
    store: &dyn TaskStore,
    minter: &dyn TaskIdMinter,
) -> Response {
    let params: MessageSendParams = match parse_params(req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if params.message.role.is_empty() {
        return invalid_params(req, "message.role required");
    }
    if params.message.parts.is_empty() {
        return invalid_params(req, "message.parts required and non-empty");
    }

    let task = Task {
        id: minter.mint(),
        context_id: params.context_id,
        state: TaskState::Submitted,
        history: vec![params.message],
        artifacts: vec![],
        metadata: params.metadata,
    };

    match store.create(task) {
        Ok(t) => ok_response(req, serde_json::to_value(&t).unwrap_or_default()),
        Err(StoreError::Duplicate(id)) => {
            internal_error(req, &format!("task id collision: {id}"))
        }
        Err(e) => internal_error(req, &e.to_string()),
    }
}

/// Handle `tasks/get`.
pub fn handle_tasks_get(req: &Request, store: &dyn TaskStore) -> Response {
    let params: TasksGetParams = match parse_params(req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if params.id.is_empty() {
        return invalid_params(req, "id required and non-empty");
    }
    match store.get(&params.id) {
        Ok(t) => ok_response(req, serde_json::to_value(&t).unwrap_or_default()),
        Err(StoreError::NotFound(_)) => a2a_error(req, A2aErrorCode::TaskNotFound, &params.id),
        Err(e) => internal_error(req, &e.to_string()),
    }
}

/// Handle `tasks/cancel`.
pub fn handle_tasks_cancel(req: &Request, store: &dyn TaskStore) -> Response {
    let params: TasksCancelParams = match parse_params(req) {
        Ok(p) => p,
        Err(resp) => return resp,
    };
    if params.id.is_empty() {
        return invalid_params(req, "id required and non-empty");
    }

    let existing = match store.get(&params.id) {
        Ok(t) => t,
        Err(StoreError::NotFound(_)) => {
            return a2a_error(req, A2aErrorCode::TaskNotFound, &params.id);
        }
        Err(e) => return internal_error(req, &e.to_string()),
    };
    if existing.state.is_terminal() {
        return a2a_error(req, A2aErrorCode::TaskNotCancelable, &params.id);
    }

    match store.update_state(&params.id, TaskState::Canceled) {
        Ok(t) => ok_response(req, serde_json::to_value(&t).unwrap_or_default()),
        Err(StoreError::TerminalState(_)) => {
            a2a_error(req, A2aErrorCode::TaskNotCancelable, &params.id)
        }
        Err(StoreError::NotFound(_)) => a2a_error(req, A2aErrorCode::TaskNotFound, &params.id),
        Err(e) => internal_error(req, &e.to_string()),
    }
}

// ---- helpers -------------------------------------------------------------

#[allow(clippy::result_large_err)]
fn parse_params<T: for<'de> Deserialize<'de>>(req: &Request) -> Result<T, Response> {
    let params = req
        .params
        .as_ref()
        .ok_or_else(|| invalid_params(req, "params required"))?;
    serde_json::from_value::<T>(params.clone())
        .map_err(|e| invalid_params(req, &format!("params parse: {e}")))
}

fn ok_response(req: &Request, result: Value) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: Some(result),
        error: None,
        id: req.id.clone(),
    }
}

fn invalid_params(req: &Request, reason: &str) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::InvalidParams.code(),
            message: "Invalid params".into(),
            data: Some(serde_json::json!({"reason": reason})),
        }),
        id: req.id.clone(),
    }
}

fn internal_error(req: &Request, reason: &str) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: ErrorCode::InternalError.code(),
            message: "Internal error".into(),
            data: Some(serde_json::json!({"reason": reason})),
        }),
        id: req.id.clone(),
    }
}

fn a2a_error(req: &Request, code: A2aErrorCode, task_id: &str) -> Response {
    Response {
        jsonrpc: "2.0".into(),
        result: None,
        error: Some(JsonRpcError {
            code: code.into(),
            message: code.default_message().into(),
            data: Some(serde_json::json!({"taskId": task_id})),
        }),
        id: req.id.clone(),
    }
}

// ---- in-memory store impl (test default + dev path) ---------------------

/// In-memory task store. Real, working implementation used by tests
/// and the dev-mode gateway. Production substitutes a Foundry-backed
/// or Lease-backed store via the [`TaskStore`] trait.
#[derive(Debug, Default)]
pub struct InMemoryTaskStore {
    inner: std::sync::Mutex<BTreeMap<String, Task>>,
}

impl InMemoryTaskStore {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

impl TaskStore for InMemoryTaskStore {
    fn create(&self, task: Task) -> Result<Task, StoreError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        if g.contains_key(&task.id) {
            return Err(StoreError::Duplicate(task.id));
        }
        g.insert(task.id.clone(), task.clone());
        Ok(task)
    }

    fn get(&self, id: &str) -> Result<Task, StoreError> {
        let g = self
            .inner
            .lock()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        g.get(id)
            .cloned()
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    fn update_state(&self, id: &str, new_state: TaskState) -> Result<Task, StoreError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|e| StoreError::Io(e.to_string()))?;
        let t = g
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        if t.state.is_terminal() {
            return Err(StoreError::TerminalState(id.to_string()));
        }
        t.state = new_state;
        Ok(t.clone())
    }
}

/// Counter-based task id minter (tests + reproducible dev runs).
#[derive(Debug, Default)]
pub struct CounterTaskIdMinter {
    inner: std::sync::atomic::AtomicU64,
}

impl CounterTaskIdMinter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TaskIdMinter for CounterTaskIdMinter {
    fn mint(&self) -> String {
        let n = self
            .inner
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("task-{n:020}")
    }
}

/// CSPRNG-backed task id minter (production). 128 bits of randomness,
/// hex-encoded → 32 chars.
pub struct OsRngTaskIdMinter;

impl TaskIdMinter for OsRngTaskIdMinter {
    fn mint(&self) -> String {
        use rand::RngCore;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        let mut s = String::with_capacity(32);
        for b in bytes {
            use std::fmt::Write;
            write!(s, "{b:02x}").expect("write to String never fails");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::jsonrpc::Id;
    use serde_json::json;

    fn req(method: &str, params: Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params: Some(params),
            id: Id::Number(1),
        }
    }

    fn store_and_minter() -> (InMemoryTaskStore, CounterTaskIdMinter) {
        (InMemoryTaskStore::new(), CounterTaskIdMinter::new())
    }

    #[test]
    fn message_send_creates_task_in_submitted_state() {
        let (store, minter) = store_and_minter();
        let r = req(
            "message/send",
            json!({
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "hi"}]
                }
            }),
        );
        let resp = handle_message_send(&r, &store, &minter);
        let result = resp.result.unwrap();
        assert_eq!(result["state"], json!("submitted"));
        let id = result["id"].as_str().unwrap().to_string();
        let stored = store.get(&id).unwrap();
        assert_eq!(stored.state, TaskState::Submitted);
        assert_eq!(stored.history.len(), 1);
    }

    #[test]
    fn message_send_camel_case_state_field() {
        let (store, minter) = store_and_minter();
        let r = req(
            "message/send",
            json!({"message": {"role": "user", "parts": [json!({})]}}),
        );
        let resp = handle_message_send(&r, &store, &minter);
        let raw = serde_json::to_string(&resp).unwrap();
        assert!(raw.contains("submitted"));
        // No snake_case leaks from internal Rust field names.
        assert!(!raw.contains("context_id"));
        assert!(!raw.contains("message_id"));
    }

    #[test]
    fn message_send_missing_params_is_invalid_params() {
        let (store, minter) = store_and_minter();
        let r = Request {
            jsonrpc: "2.0".into(),
            method: "message/send".into(),
            params: None,
            id: Id::Number(1),
        };
        let resp = handle_message_send(&r, &store, &minter);
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn message_send_empty_role_is_invalid_params() {
        let (store, minter) = store_and_minter();
        let r = req(
            "message/send",
            json!({"message": {"role": "", "parts": [json!({})]}}),
        );
        let resp = handle_message_send(&r, &store, &minter);
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn message_send_empty_parts_is_invalid_params() {
        let (store, minter) = store_and_minter();
        let r = req(
            "message/send",
            json!({"message": {"role": "user", "parts": []}}),
        );
        let resp = handle_message_send(&r, &store, &minter);
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn tasks_get_returns_task() {
        let (store, minter) = store_and_minter();
        let send = handle_message_send(
            &req(
                "message/send",
                json!({"message": {"role": "user", "parts": [json!({})]}}),
            ),
            &store,
            &minter,
        );
        let id = send.result.unwrap()["id"].as_str().unwrap().to_string();
        let resp = handle_tasks_get(&req("tasks/get", json!({"id": id.clone()})), &store);
        assert_eq!(resp.result.unwrap()["id"], json!(id));
    }

    #[test]
    fn tasks_get_unknown_id_is_a2a_task_not_found() {
        let (store, _) = store_and_minter();
        let resp = handle_tasks_get(&req("tasks/get", json!({"id": "nope"})), &store);
        let err = resp.error.unwrap();
        assert_eq!(err.code, i32::from(A2aErrorCode::TaskNotFound));
    }

    #[test]
    fn tasks_get_empty_id_is_invalid_params() {
        let (store, _) = store_and_minter();
        let resp = handle_tasks_get(&req("tasks/get", json!({"id": ""})), &store);
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn tasks_cancel_transitions_non_terminal_task() {
        let (store, minter) = store_and_minter();
        let send = handle_message_send(
            &req(
                "message/send",
                json!({"message": {"role": "user", "parts": [json!({})]}}),
            ),
            &store,
            &minter,
        );
        let id = send.result.unwrap()["id"].as_str().unwrap().to_string();
        let resp = handle_tasks_cancel(&req("tasks/cancel", json!({"id": id.clone()})), &store);
        assert_eq!(resp.result.unwrap()["state"], json!("canceled"));
        assert_eq!(store.get(&id).unwrap().state, TaskState::Canceled);
    }

    #[test]
    fn tasks_cancel_terminal_task_is_task_not_cancelable() {
        let (store, minter) = store_and_minter();
        let send = handle_message_send(
            &req(
                "message/send",
                json!({"message": {"role": "user", "parts": [json!({})]}}),
            ),
            &store,
            &minter,
        );
        let id = send.result.unwrap()["id"].as_str().unwrap().to_string();
        // Force terminal state via direct mutation (bypass the
        // `update_state` terminal-rejection so we can test the
        // cancel-on-terminal path).
        {
            let mut g = store.inner.lock().unwrap();
            g.get_mut(&id).unwrap().state = TaskState::Completed;
        }
        let resp = handle_tasks_cancel(&req("tasks/cancel", json!({"id": id.clone()})), &store);
        let err = resp.error.unwrap();
        assert_eq!(err.code, i32::from(A2aErrorCode::TaskNotCancelable));
    }

    #[test]
    fn tasks_cancel_unknown_id_is_task_not_found() {
        let (store, _) = store_and_minter();
        let resp = handle_tasks_cancel(&req("tasks/cancel", json!({"id": "no"})), &store);
        let err = resp.error.unwrap();
        assert_eq!(err.code, i32::from(A2aErrorCode::TaskNotFound));
    }

    #[test]
    fn tasks_cancel_empty_id_is_invalid_params() {
        let (store, _) = store_and_minter();
        let resp = handle_tasks_cancel(&req("tasks/cancel", json!({"id": ""})), &store);
        assert_eq!(resp.error.unwrap().code, ErrorCode::InvalidParams.code());
    }

    #[test]
    fn task_state_is_terminal_matches_spec() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Rejected.is_terminal());
        assert!(!TaskState::Submitted.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
        assert!(!TaskState::AuthRequired.is_terminal());
    }

    #[test]
    fn store_duplicate_id_is_collision_error() {
        let store = InMemoryTaskStore::new();
        let task = Task {
            id: "dup".into(),
            context_id: None,
            state: TaskState::Submitted,
            history: vec![],
            artifacts: vec![],
            metadata: None,
        };
        store.create(task.clone()).unwrap();
        assert!(matches!(
            store.create(task),
            Err(StoreError::Duplicate(_))
        ));
    }

    #[test]
    fn store_update_terminal_task_is_terminal_state_error() {
        let store = InMemoryTaskStore::new();
        let task = Task {
            id: "t".into(),
            context_id: None,
            state: TaskState::Completed,
            history: vec![],
            artifacts: vec![],
            metadata: None,
        };
        store.create(task).unwrap();
        assert!(matches!(
            store.update_state("t", TaskState::Canceled),
            Err(StoreError::TerminalState(_))
        ));
    }

    #[test]
    fn counter_minter_emits_unique_padded_ids() {
        let m = CounterTaskIdMinter::new();
        let a = m.mint();
        let b = m.mint();
        assert_ne!(a, b);
        assert!(a.starts_with("task-"));
        assert_eq!(a.len(), 25);
    }

    #[test]
    fn os_rng_minter_emits_32_hex_chars() {
        let m = OsRngTaskIdMinter;
        let id = m.mint();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, m.mint());
    }

    #[test]
    fn message_send_id_is_preserved_in_response() {
        let (store, minter) = store_and_minter();
        let r = Request {
            jsonrpc: "2.0".into(),
            method: "message/send".into(),
            params: Some(json!({"message": {"role": "user", "parts": [json!({})]}})),
            id: Id::String("client-supplied-7".into()),
        };
        let resp = handle_message_send(&r, &store, &minter);
        assert_eq!(resp.id, Id::String("client-supplied-7".into()));
    }

    #[test]
    fn parse_failure_is_invalid_params_not_panic() {
        let (store, minter) = store_and_minter();
        // `message` field is required; missing it triggers serde error.
        let r = req("message/send", json!({"contextId": "ctx-1"}));
        let resp = handle_message_send(&r, &store, &minter);
        let err = resp.error.unwrap();
        assert_eq!(err.code, ErrorCode::InvalidParams.code());
    }
}
