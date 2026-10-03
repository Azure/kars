// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use k8s_openapi::{
    ByteString,
    api::{
        apps::v1::Deployment,
        core::v1::{Namespace, Secret},
    },
    apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference},
};
use kube::{
    Api, Client, ResourceExt,
    api::{Patch, PatchParams, PostParams},
};
use serde_json::json;
use std::collections::BTreeMap;

use super::identity::Role;
use super::{Error, Result, annotation, api_error, identity, same_snapshot};
use crate::crd::KarsSandbox;

pub(super) const PIN: &str = "kars.azure.com/mission-root-uid";
pub(super) const ROOT_KEY: &str = "root";
pub(super) const RUNTIME_SECRET: &str = "kars-mission-runtime-identity";
pub(super) const DISPATCHER_SECRET: &str = "kars-mission-dispatcher-identity";
const VERSION: &str = "kars.azure.com/mission-root-version";
const OWNER: &str = "kars.azure.com/mission-owner-uid";
const NAMESPACE: &str = "kars.azure.com/mission-namespace-uid";
const ROLE: &str = "kars.azure.com/mission-identity-role";
const RECOVERY: &str = "kars.azure.com/mission-root-recovery-uid";

// Intentionally not Debug: a root must never enter a diagnostic or status object.
pub(super) struct Root {
    pub uid: String,
    pub value: String,
    pub anchor: ObjectMeta,
}

pub(super) enum Anchor<'a> {
    Runtime(&'a KarsSandbox),
    Dispatcher(&'a Deployment),
}
impl Anchor<'_> {
    fn meta(&self) -> &ObjectMeta {
        match self {
            Self::Runtime(value) => &value.metadata,
            Self::Dispatcher(value) => &value.metadata,
        }
    }
    fn role(&self) -> Role {
        match self {
            Self::Runtime(_) => Role::Runtime,
            Self::Dispatcher(_) => Role::Dispatcher,
        }
    }
    fn secret_name(&self) -> &'static str {
        match self {
            Self::Runtime(_) => RUNTIME_SECRET,
            Self::Dispatcher(_) => DISPATCHER_SECRET,
        }
    }
    async fn current(&self, client: &Client) -> Result<ObjectMeta> {
        let meta = self.meta();
        let ns = meta
            .namespace
            .as_deref()
            .ok_or(Error::Invalid("root anchor namespace missing"))?;
        let name = meta
            .name
            .as_deref()
            .ok_or(Error::Invalid("root anchor name missing"))?;
        let current = match self {
            Self::Runtime(_) => Api::<KarsSandbox>::namespaced(client.clone(), ns)
                .get_metadata(name)
                .await
                .map(|v| v.metadata),
            Self::Dispatcher(_) => Api::<Deployment>::namespaced(client.clone(), ns)
                .get_metadata(name)
                .await
                .map(|v| v.metadata),
        }
        .map_err(|e| api_error("read root anchor", e))?;
        same_snapshot(meta, &current)?;
        Ok(current)
    }
    async fn pin(&self, client: &Client, uid: &str) -> Result<ObjectMeta> {
        self.current(client).await?;
        let meta = self.meta();
        let (owner_uid, rv) = identity(meta)?;
        let patch = Patch::Merge(json!({"metadata": {"uid": owner_uid, "resourceVersion": rv,
            "annotations": {PIN: uid}}}));
        let ns = meta
            .namespace
            .as_deref()
            .ok_or(Error::Invalid("root anchor namespace missing"))?;
        let name = meta
            .name
            .as_deref()
            .ok_or(Error::Invalid("root anchor name missing"))?;
        match self {
            Self::Runtime(_) => Api::<KarsSandbox>::namespaced(client.clone(), ns)
                .patch_metadata(name, &PatchParams::default(), &patch)
                .await
                .map(|v| v.metadata),
            Self::Dispatcher(_) => Api::<Deployment>::namespaced(client.clone(), ns)
                .patch_metadata(name, &PatchParams::default(), &patch)
                .await
                .map(|v| v.metadata),
        }
        .map_err(|e| {
            api_error(
                "pin mission root; interrupted bootstrap requires operator recovery",
                e,
            )
        })
    }
    fn secret_owner(&self, namespace: &Namespace) -> Result<OwnerReference> {
        let (api_version, kind, meta) = match self {
            Self::Runtime(_) => ("v1", "Namespace", &namespace.metadata),
            Self::Dispatcher(value) => ("apps/v1", "Deployment", &value.metadata),
        };
        Ok(OwnerReference {
            api_version: api_version.into(),
            kind: kind.into(),
            name: meta
                .name
                .clone()
                .ok_or(Error::Invalid("root owner name missing"))?,
            uid: identity(meta)?.0.into(),
            controller: Some(true),
            block_owner_deletion: Some(true),
        })
    }
}

async fn current_namespace(client: &Client, expected: &Namespace) -> Result<()> {
    let live = Api::<Namespace>::all(client.clone())
        .get_metadata(&expected.name_any())
        .await
        .map_err(|e| api_error("recheck root namespace", e))?;
    same_snapshot(&expected.metadata, &live.metadata)
}

fn validate_metadata(
    meta: &ObjectMeta,
    anchor: &Anchor<'_>,
    namespace: &Namespace,
    pin: &str,
) -> Result<()> {
    let (uid, _) = identity(meta)?;
    let expected_owner = anchor.secret_owner(namespace)?;
    let owners = meta.owner_references.as_deref().unwrap_or_default();
    if uid != pin
        || meta.name.as_deref() != Some(anchor.secret_name())
        || meta.namespace != namespace.metadata.name
        || owners.len() != 1
        || owners[0] != expected_owner
        || annotation(meta, VERSION) != Some("v1")
        || annotation(meta, OWNER) != Some(identity(anchor.meta())?.0)
        || annotation(meta, NAMESPACE) != Some(identity(&namespace.metadata)?.0)
        || annotation(meta, ROLE) != Some(anchor.role().name())
    {
        return Err(Error::Invalid(
            "mission root custody mismatch; no adoption or rotation",
        ));
    }
    Ok(())
}
fn validate(
    secret: &Secret,
    anchor: &Anchor<'_>,
    namespace: &Namespace,
    pin: &str,
) -> Result<Root> {
    validate_metadata(&secret.metadata, anchor, namespace, pin)?;
    let (uid, _) = identity(&secret.metadata)?;
    if secret.immutable != Some(true) || secret.type_.as_deref() != Some("Opaque") {
        return Err(Error::Invalid("mission root must be immutable and Opaque"));
    }
    let data = secret
        .data
        .as_ref()
        .filter(|data| data.len() == 1)
        .and_then(|data| data.get(ROOT_KEY))
        .ok_or(Error::Invalid("mission root data invalid"))?;
    let value =
        std::str::from_utf8(&data.0).map_err(|_| Error::Invalid("mission root data invalid"))?;
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid("mission root data invalid"));
    }
    Ok(Root {
        uid: uid.into(),
        value: value.into(),
        anchor: anchor.meta().clone(),
    })
}

pub(super) async fn recheck(
    client: &Client,
    anchor: Anchor<'_>,
    namespace: &Namespace,
    uid: &str,
) -> Result<()> {
    if annotation(anchor.meta(), PIN) != Some(uid) {
        return Err(Error::Invalid("mission root pin changed"));
    }
    anchor.current(client).await?;
    current_namespace(client, namespace).await?;
    let secret = Api::<Secret>::namespaced(client.clone(), &namespace.name_any())
        .get_metadata(anchor.secret_name())
        .await
        .map_err(|e| api_error("recheck root custody", e))?;
    validate_metadata(&secret.metadata, &anchor, namespace, uid)
}

/// CREATE and the UID/RV pin are separate writes. Interrupted bootstrap needs an
/// administrator's exact Secret UID on the claimed Namespace; it is never retried
/// by automatically adopting or rotating an unpinned Secret.
pub(super) async fn ensure(
    client: &Client,
    anchor: Anchor<'_>,
    namespace: &Namespace,
) -> Result<Root> {
    identity(anchor.meta())?;
    identity(&namespace.metadata)?;
    if namespace.status.as_ref().and_then(|s| s.phase.as_deref()) != Some("Active") {
        return Err(Error::Invalid("mission root namespace is not Active"));
    }
    anchor.current(client).await?;
    current_namespace(client, namespace).await?;
    let api = Api::<Secret>::namespaced(client.clone(), &namespace.name_any());
    let name = anchor.secret_name();
    if let Some(pin) = annotation(anchor.meta(), PIN) {
        if pin.is_empty() {
            return Err(Error::Invalid("mission root UID pin is empty"));
        }
        // Check metadata custody before requesting the Secret body.
        let meta = api
            .get_metadata(name)
            .await
            .map_err(|e| api_error("read pinned root metadata", e))?;
        validate_metadata(&meta.metadata, &anchor, namespace, pin)?;
        let secret = api
            .get(name)
            .await
            .map_err(|e| api_error("read pinned root", e))?;
        same_snapshot(&meta.metadata, &secret.metadata)?;
        let root = validate(&secret, &anchor, namespace, pin)?;
        current_namespace(client, namespace).await?;
        anchor.current(client).await?;
        return Ok(root);
    }
    if let Some(meta) = api
        .get_metadata_opt(name)
        .await
        .map_err(|e| api_error("check root vacancy", e))?
    {
        let uid = identity(&meta.metadata)?.0;
        if annotation(&namespace.metadata, RECOVERY) != Some(uid) {
            return Err(Error::Invalid(
                "unpinned mission root exists; explicit namespace recovery UID required",
            ));
        }
        validate_metadata(&meta.metadata, &anchor, namespace, uid)?;
        current_namespace(client, namespace).await?;
        anchor.current(client).await?;
        let secret = api
            .get(name)
            .await
            .map_err(|e| api_error("read authorized recovery root", e))?;
        same_snapshot(&meta.metadata, &secret.metadata)?;
        let mut root = validate(&secret, &anchor, namespace, uid)?;
        current_namespace(client, namespace).await?;
        let pinned = anchor.pin(client, uid).await?;
        if pinned.uid != anchor.meta().uid
            || annotation(&pinned, PIN) != Some(uid)
            || pinned.deletion_timestamp.is_some()
        {
            return Err(Error::Invalid(
                "recovered mission root pin was not committed",
            ));
        }
        identity(&pinned)?;
        root.anchor = pinned;
        return Ok(root);
    }
    let root = hex::encode(rand::random::<[u8; 32]>());
    let secret = Secret {
        metadata: ObjectMeta {
            name: Some(name.into()),
            namespace: Some(namespace.name_any()),
            owner_references: Some(vec![anchor.secret_owner(namespace)?]),
            annotations: Some(BTreeMap::from([
                (VERSION.into(), "v1".into()),
                (OWNER.into(), identity(anchor.meta())?.0.into()),
                (NAMESPACE.into(), identity(&namespace.metadata)?.0.into()),
                (ROLE.into(), anchor.role().name().into()),
            ])),
            ..Default::default()
        },
        immutable: Some(true),
        type_: Some("Opaque".into()),
        data: Some(BTreeMap::from([(
            ROOT_KEY.into(),
            ByteString(root.into_bytes()),
        )])),
        ..Default::default()
    };
    current_namespace(client, namespace).await?;
    anchor.current(client).await?;
    let created = api
        .create(&PostParams::default(), &secret)
        .await
        .map_err(|e| api_error("create mission root; no conflict adoption", e))?;
    let uid = identity(&created.metadata)?.0;
    let mut root = validate(&created, &anchor, namespace, uid)?;
    current_namespace(client, namespace).await?;
    let pinned = anchor.pin(client, uid).await?;
    if pinned.uid != anchor.meta().uid
        || annotation(&pinned, PIN) != Some(uid)
        || pinned.deletion_timestamp.is_some()
    {
        return Err(Error::Invalid("mission root pin was not committed"));
    }
    identity(&pinned)?;
    root.anchor = pinned;
    Ok(root)
}
