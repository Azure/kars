// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pod contract shared by scheduled and one-shot evaluation Jobs.

use serde_json::{Value, json};

pub(super) fn runner_pod_spec_json(
    eval_name: &str,
    cm_name: &str,
    runner_image: &str,
    target_url: &str,
    _corpus_label: &str,
) -> Value {
    json!({
        "restartPolicy": "Never",
        "securityContext": {
            "runAsNonRoot": true,
            "seccompProfile": {"type": "RuntimeDefault"},
        },
        "containers": [{
            "name": "runner",
            "image": runner_image,
            "imagePullPolicy": "IfNotPresent",
            "securityContext": {
                "allowPrivilegeEscalation": false,
                "runAsNonRoot": true,
                "capabilities": {"drop": ["ALL"]},
                "seccompProfile": {"type": "RuntimeDefault"},
            },
            "args": [
                "--corpus", "/etc/kars/eval-corpus/corpus.json",
                "--router-base", target_url,
                "--output", "/dev/stdout",
            ],
            "env": [
                {"name": "RUST_LOG", "value": "info"},
                {"name": "KARS_EVAL_NAME", "value": eval_name},
                {"name": "KARS_EVAL_REPORT_FORMAT", "value": "v2"},
            ],
            "volumeMounts": [{
                "name": "corpus",
                "mountPath": "/etc/kars/eval-corpus",
                "readOnly": true,
            }],
            "resources": {
                "requests": {"cpu": "50m", "memory": "64Mi"},
                "limits": {"cpu": "500m", "memory": "256Mi"},
            },
        }],
        "volumes": [{
            "name": "corpus",
            "configMap": {"name": cm_name},
        }],
    })
}
