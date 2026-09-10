// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{CONTRACT, PREFIX, bundle, hash};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn pod_spec_digest(value: &Value) -> String {
    hash(value)
}

pub(crate) fn install(
    objects: &mut BTreeMap<String, Value>,
    root: &str,
    root_uid: &str,
    account_uid: &str,
    scopes: &[(&str, &str)],
) -> Value {
    let mut ids = Vec::new();
    for (index, definition) in bundle()["objects"].as_array().unwrap().iter().enumerate() {
        let mut value = definition.clone();
        let kind = value["kind"].as_str().unwrap().to_string();
        let name = value["metadata"]["name"].as_str().unwrap().to_string();
        let uid = format!("private-admission-{index}");
        value["metadata"]["uid"] = uid.clone().into();
        value["metadata"]["resourceVersion"] = "1".into();
        value["metadata"]["generation"] = 1.into();
        if kind == "ValidatingAdmissionPolicy" {
            value["status"] = json!({"observedGeneration":1,"typeChecking":{}});
        }
        ids.push(json!({"kind":kind,"name":name,"uid":uid,"resourceVersion":"1"}));
        let plural = if kind == "ValidatingAdmissionPolicy" {
            "validatingadmissionpolicies"
        } else {
            "validatingadmissionpolicybindings"
        };
        objects.insert(
            format!("/apis/admissionregistration.k8s.io/v1/{plural}/{name}"),
            value,
        );
    }
    let revision = hash(&json!(ids));
    let epoch = "a".repeat(64);
    let mut scope_list = BTreeMap::from([(root, root_uid)]);
    scope_list.extend(scopes.iter().copied());
    let mut namespaces = Vec::new();
    for (name, uid) in scope_list {
        let namespace = objects.entry(format!("/api/v1/namespaces/{name}")).or_insert_with(|| {
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":name,"uid":uid,"resourceVersion":"1"},
                "spec":{"finalizers":["kubernetes"]}})
        });
        for (key, value) in [
            ("enabled", "true"),
            ("state", "Qualified"),
            ("epoch", epoch.as_str()),
            ("namespace-uid", uid),
            ("root-namespace", root),
            ("root-namespace-uid", root_uid),
            ("root-account", "kars-controller"),
            ("root-uid", account_uid),
            ("root-deployment", "kars-controller"),
            ("root-deployment-uid", "controller-deploy"),
            ("bundle-revision", revision.as_str()),
            ("profile", "kcm-certificate"),
        ] {
            namespace["metadata"]["annotations"][format!("{PREFIX}{key}")] = value.into();
        }
        namespace["metadata"]["annotations"][format!("{PREFIX}root-user")] =
            format!("system:serviceaccount:{root}:kars-controller").into();
        namespace["metadata"]["annotations"][format!("{PREFIX}root-template-digest")] =
            "b".repeat(64).into();
        namespaces.push(
            json!({"namespace":{"name":name,"uid":uid,"resourceVersion":"1"},
            "consumers":[],"epoch":epoch}),
        );
    }
    objects.insert(format!("/api/v1/namespaces/{root}/serviceaccounts/kars-controller"), json!({
        "apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"kars-controller","namespace":root,
            "uid":account_uid,"resourceVersion":"1"}
    }));
    objects.insert(format!("/apis/apps/v1/namespaces/{root}/deployments/kars-controller"), json!({
        "apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"kars-controller","namespace":root,
            "uid":"controller-deploy","resourceVersion":"1"},
        "spec":{"template":{"metadata":{},"spec":{"serviceAccountName":"kars-controller",
            "containers":[{"name":"controller","image":"fixture"}]}}}
    }));
    json!({"contract":CONTRACT,"phase":"qualified","bundleRevision":revision,
        "root":{"namespace":{"name":root,"uid":root_uid,"resourceVersion":"1"},
            "account":{"name":"kars-controller","uid":account_uid,"resourceVersion":"1"},
            "deployment":{"name":"kars-controller","uid":"controller-deploy","resourceVersion":"1"},
            "templateDigest":"b".repeat(64)},
        "profile":"kcm-certificate","controllerUids":{},"namespaces":namespaces})
}
