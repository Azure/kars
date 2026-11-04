// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PORT: u16 = 9449;
pub const DIRECTORY: &str = "/etc/kars/services";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TlsIdentity {
    pub identity: Value,
    pub server_name: String,
    pub ca_pem: String,
    pub certificate_pem: String,
    pub private_key_pem: String,
    pub expires_at: i64,
}

pub fn server_name(identity: &Value) -> Result<String, &'static str> {
    let uid = identity["sandbox"]["uid"]
        .as_str()
        .filter(|uid| {
            !uid.is_empty()
                && uid.len() <= 128
                && uid
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .ok_or("Control TLS Sandbox UID invalid")?;
    Ok(format!("control-{uid}.kars.internal"))
}

impl TlsIdentity {
    pub fn valid_for(&self, identity: &Value, now: i64) -> bool {
        self.identity == *identity
            && identity["managed"] == true
            && identity["task"].is_object()
            && server_name(identity).is_ok_and(|name| name == self.server_name)
            && self.expires_at > now
            && !self.ca_pem.is_empty()
            && !self.certificate_pem.is_empty()
            && !self.private_key_pem.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_tls_is_bound_to_the_exact_managed_task_and_expiry() {
        let identity = json!({"managed":true,"sandbox":{"uid":"sandbox-uid"},
            "namespace_uid":"namespace-uid","task":{"uid":"task-uid"},
            "task_generation":1,"task_authorization":"original"});
        let tls = TlsIdentity {
            identity: identity.clone(),
            server_name: server_name(&identity).unwrap(),
            ca_pem: "ca".into(),
            certificate_pem: "certificate".into(),
            private_key_pem: "key".into(),
            expires_at: 100,
        };
        assert!(tls.valid_for(&identity, 99));
        assert!(!tls.valid_for(&identity, 100));
        for (field, value) in [
            ("namespace_uid", json!("replacement")),
            ("task", json!({"uid":"replacement"})),
            ("task_generation", json!(2)),
            ("task_authorization", json!("changed")),
            ("managed", json!(false)),
        ] {
            let mut changed = identity.clone();
            changed[field] = value;
            assert!(!tls.valid_for(&changed, 99));
        }
        for uid in [
            "",
            "x/path",
            "x@host",
            "x:443",
            "x?query",
            "x#fragment",
            "x%2fhost",
        ] {
            assert!(server_name(&json!({"sandbox":{"uid":uid}})).is_err());
        }
        let mut wrong_host = tls.clone();
        wrong_host.server_name = "control-replacement.kars.internal".into();
        assert!(!wrong_host.valid_for(&identity, 99));
    }
}
