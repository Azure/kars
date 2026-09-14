// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use crate::github_app::tests::{KEY, app};

impl GitHubServices {
    pub(super) fn read(&self) -> Result<Option<Vec<u8>>, Error> {
        Self::read_file(std::fs::File::open(&self.path))
    }
}

fn identity() -> Identity {
    serde_json::from_value(serde_json::json!({
        "sandbox":{"namespace":"workspace","name":"agent","uid":"sandbox-uid"},
        "namespace_uid":"namespace-uid","managed":true
    }))
    .unwrap()
}

fn source() -> serde_json::Value {
    serde_json::json!({
        "identity":identity(),"app_id":"42","installation_id":7,
        "private_key_pem":std::str::from_utf8(KEY).unwrap(),"repositories":["owner/repo"]
    })
}

#[tokio::test]
async fn github_secret_removal_rotation_and_invalid_replacement_drop_cached_credentials() {
    let directory = tempfile::Builder::new()
        .prefix(".github-config-test-")
        .tempdir_in(".")
        .unwrap();
    let path = directory.path().join("config.json");
    let mut services = GitHubServices::new(Some(identity()), crate::github_app::client().unwrap());
    services.path = path.clone();
    assert!(services.current().await.unwrap().is_none());
    std::fs::write(&path, serde_json::to_vec(&source()).unwrap()).unwrap();
    let first = services.current().await.unwrap().unwrap();
    assert!(!first.write_enabled());
    assert!(services.unchanged(&first).await);
    let mut replacement = source();
    replacement["installation_id"] = serde_json::json!(8);
    std::fs::write(&path, serde_json::to_vec(&replacement).unwrap()).unwrap();
    assert!(!services.unchanged(&first).await);
    let second = services.current().await.unwrap().unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    std::fs::write(&path, b"{\"private_key_pem\":\"do-not-log\"}").unwrap();
    assert_eq!(
        services.current().await.err().unwrap().to_string(),
        "GitHub service configuration is invalid"
    );
    assert!(services.loaded.lock().await.is_none());
    std::fs::remove_file(&path).unwrap();
    assert!(services.current().await.unwrap().is_none());
}

#[tokio::test]
async fn github_secret_cannot_rebind_to_recreated_namespace_sandbox_or_changed_task() {
    let directory = tempfile::Builder::new()
        .prefix(".github-config-test-")
        .tempdir_in(".")
        .unwrap();
    let path = directory.path().join("config.json");
    let mut services = GitHubServices::new(Some(identity()), crate::github_app::client().unwrap());
    services.path = path.clone();
    for field in ["namespace_uid", "managed", "task_authorization"] {
        let mut value = source();
        value["identity"][field] = serde_json::json!("different");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(services.current().await.is_err(), "{field}");
    }
    let mut value = source();
    value["identity"]["sandbox"]["uid"] = serde_json::json!("recreated");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(services.current().await.is_err());
    std::fs::write(&path, vec![b'x'; MAX_CONFIG + 1]).unwrap();
    assert!(services.current().await.is_err());
}

// Test-only fixture: immutable production origin is replaced only inside this
// private test module; no configurable upstream URL exists in the Secret schema.
pub(crate) async fn fixture(api: &str) -> (tempfile::TempDir, Arc<GitHubServices>) {
    let directory = tempfile::Builder::new()
        .prefix(".github-http-test-")
        .tempdir_in(".")
        .unwrap();
    let path = directory.path().join("config.json");
    let bytes = serde_json::to_vec(&source()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut services = GitHubServices::new(Some(identity()), crate::github_app::client().unwrap());
    services.path = path;
    *services.loaded.lock().await = Some(Loaded {
        source: bytes,
        app: app(api, &["owner/repo"]),
    });
    (directory, Arc::new(services))
}
