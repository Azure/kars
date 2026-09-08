// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

fn fixture() -> (GitHubBinding,KarsCredentialGrant,ConfigMap,Secret,Value) {
    let name=format!("kars-github-connection-{}",hex::encode(&Sha256::digest(b"owner-subject")[..8]));
    let selection=GitHubBinding{grant:ObjectIdentity{name:NAME.into(),uid:"grant".into()},
        connection:ObjectIdentity{name:name.clone(),uid:"connection".into()},repositories:vec!["owner/repo".into()],write:false};
    let grant:KarsCredentialGrant=serde_json::from_value(json!({
        "apiVersion":"kars.azure.com/v1alpha1","kind":"KarsCredentialGrant",
        "metadata":{"name":NAME,"namespace":"workspace","uid":"grant","resourceVersion":"1","generation":1},
        "spec":{"workspaceUid":"workspace-uid","writers":[],"integrationStores":[
            {"secret":{"name":"kars-github-app","uid":"app-store"},"purpose":"github-app"}],
            "githubConnections":[{"connection":{"name":name,"uid":"connection"},"appSecret":{"name":"kars-github-app","uid":"app-store"},
                "appId":"123","ownerSubject":"owner-subject","installationId":456,"repositories":["owner/repo"],"write":false}]}
    })).unwrap();
    let connection:ConfigMap=serde_json::from_value(json!({
        "apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":"workspace","uid":"connection","resourceVersion":"2"},
        "data":{"installation_id":"456","account":"owner","repos":"[\"owner/repo\"]"}
    })).unwrap();
    let key=rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap().serialize_pem();
    let store:Secret=serde_json::from_value(json!({
        "apiVersion":"v1","kind":"Secret","type":"Opaque",
        "metadata":{"name":"kars-github-app","namespace":"workspace","uid":"app-store","resourceVersion":"3"},
        "data":{"GITHUB_APP_ID":STANDARD.encode("123"),"GITHUB_APP_PRIVATE_KEY":STANDARD.encode(key)}
    })).unwrap();
    let identity=json!({"sandbox":{"namespace":"workspace","name":"agent","uid":"sandbox"},
        "namespace_uid":"runtime","task":null,"task_authorization":null,"task_generation":null,"managed":true});
    (selection,grant,connection,store,identity)
}

#[test]
fn governed_github_factory_emits_exact_consumer_schema_and_preserves_source_uid_values() {
    let (selection,grant,connection,store,identity)=fixture();
    let before=serde_json::to_value(&store).unwrap();
    let value:Value=serde_json::from_str(&configuration(&selection,&grant,&connection,&store,&identity).unwrap()).unwrap();
    assert_eq!(value["identity"],identity);
    assert_eq!(value["app_id"],"123");
    assert_eq!(value["installation_id"],456);
    assert_eq!(value["repositories"],json!(["owner/repo"]));
    assert_eq!(value["write"],false);
    assert_eq!(value.as_object().unwrap().len(),6);
    assert!(value["private_key_pem"].as_str().unwrap().contains("BEGIN PRIVATE KEY"));
    assert_eq!(serde_json::to_value(&store).unwrap(),before);
}

#[test]
fn governed_github_factory_rejects_replacement_adoption_and_scope_expansion() {
    let (selection,grant,connection,store,identity)=fixture();
    for changed in ["source-uid","connection-uid","app-id","installation","owner","repo","write","enrollment"] {
        let mut selection=selection.clone();
        let mut grant=grant.clone();
        let mut connection=connection.clone();
        let mut store=store.clone();
        match changed {
            "source-uid"=>store.metadata.uid=Some("replacement".into()),
            "connection-uid"=>connection.metadata.uid=Some("replacement".into()),
            "app-id"=>grant.spec.github_connections[0].app_id="999".into(),
            "installation"=>grant.spec.github_connections[0].installation_id=999,
            "owner"=>grant.spec.github_connections[0].owner_subject="foreign".into(),
            "repo"=>selection.repositories.push("owner/foreign".into()),
            "write"=>selection.write=true,
            _=>grant.spec.integration_stores.clear(),
        }
        let error=configuration(&selection,&grant,&connection,&store,&identity).unwrap_err();
        assert!(!error.contains("PRIVATE KEY"),"{changed}");
    }
}
