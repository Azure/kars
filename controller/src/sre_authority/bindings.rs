// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::api_error;
use crate::sre_registration::{
    BindingReview, KarsSRERegistration, OWNER, ROUTER_SA, RUNTIME_NAMESPACE,
};
use k8s_openapi::api::{
    core::v1::ServiceAccount,
    rbac::v1::{ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject},
};
use kube::{
    Api, Client,
    api::{DeleteParams, ListParams, Patch, PatchParams, PostParams, Preconditions},
};
use serde_json::json;

const RETIRED: &str = "kars.azure.com/sre-legacy-retired";
const GRANTS: &[(&str, &str)] = &[
    ("kars-sre-private-reader", "kars-sre-private-diagnostics"),
    ("kars-sre-private-author", "kars-sre-action-author"),
    ("kars-sre-private-renew", "kars-sre-router-renew"),
];

pub(crate) fn legacy_subject(subject: &Subject) -> bool {
    (subject.kind == "ServiceAccount"
        && subject.name == "sandbox"
        && subject.namespace.as_deref() == Some(RUNTIME_NAMESPACE))
        || (subject.kind == "User"
            && subject.name == format!("system:serviceaccount:{RUNTIME_NAMESPACE}:sandbox"))
}

fn legacy_group(subject: &Subject) -> bool {
    subject.kind == "Group"
        && [
            "system:authenticated".to_string(),
            "system:serviceaccounts".to_string(),
            format!("system:serviceaccounts:{RUNTIME_NAMESPACE}"),
        ]
        .contains(&subject.name)
}

fn dangerous(rules: &[PolicyRule]) -> bool {
    rules.iter().any(|rule| {
        let verbs = &rule.verbs;
        let resources = rule.resources.as_deref().unwrap_or_default();
        let groups = rule.api_groups.as_deref().unwrap_or_default();
        let has =
            |items: &[String], value: &str| items.iter().any(|item| item == "*" || item == value);
        (has(groups, "")
            && has(resources, "secrets")
            && ["get", "list", "watch"].iter().any(|verb| has(verbs, verb)))
            || (has(groups, "")
                && ["pods/exec", "pods/proxy", "serviceaccounts/token"]
                    .iter()
                    .any(|resource| has(resources, resource))
                && (has(verbs, "create") || has(verbs, "get")))
            || (has(groups, "kars.azure.com")
                && has(resources, "karssreactions")
                && has(verbs, "create"))
    })
}

async fn rules(
    client: &Client,
    role: &RoleRef,
    namespace: Option<&str>,
) -> Result<Vec<PolicyRule>, String> {
    if role.api_group != "rbac.authorization.k8s.io" {
        return Err("Legacy binding roleRef uses an unsupported API group".into());
    }
    match role.kind.as_str() {
        "ClusterRole" => Ok(Api::<ClusterRole>::all(client.clone())
            .get(&role.name)
            .await
            .map_err(|e| api_error("Read legacy ClusterRole", e))?
            .rules
            .unwrap_or_default()),
        "Role" => Ok(Api::<Role>::namespaced(
            client.clone(),
            namespace.ok_or("Role has no namespace")?,
        )
        .get(&role.name)
        .await
        .map_err(|e| api_error("Read legacy Role", e))?
        .rules
        .unwrap_or_default()),
        _ => Err("Legacy binding roleRef kind is invalid".into()),
    }
}

#[derive(Clone)]
pub(super) struct Binding {
    review: BindingReview,
    metadata: kube::api::ObjectMeta,
    subjects: Vec<Subject>,
}

fn validate_review(
    reg: &KarsSRERegistration,
    kind: &str,
    namespace: Option<&str>,
    metadata: &kube::api::ObjectMeta,
    role: &RoleRef,
    subjects: &[Subject],
) -> Result<Binding, String> {
    let review = reg
        .spec
        .legacy_bindings
        .iter()
        .find(|review| {
            review.kind == kind
                && review.namespace.as_deref() == namespace
                && Some(review.name.as_str()) == metadata.name.as_deref()
        })
        .ok_or("Unreviewed legacy SRE binding exists; no migration mutations are authorized")?;
    let retired =
        metadata.annotations.as_ref().and_then(|a| a.get(RETIRED)) == reg.metadata.uid.as_ref();
    let expected: Vec<_> = review
        .subjects
        .iter()
        .filter(|subject| !legacy_subject(subject))
        .cloned()
        .collect();
    if metadata.uid.as_deref() != Some(review.uid.as_str())
        || metadata.deletion_timestamp.is_some()
        || role != &review.role_ref
        || (!retired
            && (metadata.resource_version.as_deref() != Some(review.resource_version.as_str())
                || subjects != review.subjects))
        || (retired && subjects != expected)
    {
        return Err("Reviewed legacy grant changed or was replaced; migration stopped".into());
    }
    Ok(Binding {
        review: review.clone(),
        metadata: metadata.clone(),
        subjects: subjects.to_vec(),
    })
}

pub(super) async fn review(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<Vec<Binding>, String> {
    let cluster = Api::<ClusterRoleBinding>::all(client.clone())
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Inventory legacy ClusterRoleBindings", e))?;
    let local = Api::<RoleBinding>::all(client.clone())
        .list(&ListParams::default())
        .await
        .map_err(|e| api_error("Inventory legacy RoleBindings", e))?;
    let mut bindings = Vec::new();
    let mut inspect = Vec::new();
    for binding in cluster {
        inspect.push((
            "ClusterRoleBinding",
            binding.metadata,
            binding.role_ref,
            binding.subjects.unwrap_or_default(),
        ));
    }
    for binding in local {
        inspect.push((
            "RoleBinding",
            binding.metadata,
            binding.role_ref,
            binding.subjects.unwrap_or_default(),
        ));
    }
    for (kind, metadata, role, subjects) in inspect {
        if subjects.iter().any(legacy_group)
            && dangerous(&rules(client, &role, metadata.namespace.as_deref()).await?)
        {
            return Err("A broad group grant gives legacy SRE credentials privileged access; restructure that grant explicitly".into());
        }
        let was_reviewed = reg.spec.legacy_bindings.iter().any(|review| {
            review.kind == kind
                && review.name == metadata.name.as_deref().unwrap_or_default()
                && review.namespace == metadata.namespace
        });
        let legacy = subjects.iter().any(legacy_subject);
        let ordinary_spawner = legacy
            && role.kind == "ClusterRole"
            && role.name == "kars-sandbox-spawner"
            && rules(client, &role, metadata.namespace.as_deref())
                .await?
                .iter()
                .all(|rule| {
                    rule.api_groups.as_deref() == Some(&["kars.azure.com".into()][..])
                        && rule.resources.as_deref() == Some(&["karssandboxes".into()][..])
                        && rule.non_resource_urls.is_none()
                        && rule.verbs.iter().all(|verb| {
                            ["get", "list", "create", "delete"].contains(&verb.as_str())
                        })
                });
        if (legacy && !ordinary_spawner) || was_reviewed {
            bindings.push(validate_review(
                reg,
                kind,
                metadata.namespace.as_deref(),
                &metadata,
                &role,
                &subjects,
            )?);
        }
    }
    Ok(bindings)
}

pub(super) async fn retire_legacy(
    client: &Client,
    reg: &KarsSRERegistration,
    bindings: &[Binding],
) -> Result<(), String> {
    let plans: Vec<_> = bindings
        .iter()
        .filter(|binding| binding.subjects.iter().any(legacy_subject))
        .map(|binding| {
            let subjects: Vec<_> = binding
                .subjects
                .iter()
                .filter(|subject| !legacy_subject(subject))
                .cloned()
                .collect();
            let patch = json!({
                "metadata":{"uid":binding.metadata.uid,"resourceVersion":binding.metadata.resource_version,
                    "annotations":{RETIRED:reg.metadata.uid}},
                "subjects":subjects,
            });
            (binding, patch)
        })
        .collect();
    // RBAC escalation checks also apply to subtractive binding updates.
    // Preflight every exact patch before any write; this is not a transaction.
    for dry_run in [true, false] {
        let params = PatchParams {
            dry_run,
            ..Default::default()
        };
        for (binding, patch) in &plans {
            if binding.review.kind == "ClusterRoleBinding" {
                Api::<ClusterRoleBinding>::all(client.clone())
                    .patch(&binding.review.name, &params, &Patch::Merge(patch))
                    .await
                    .map_err(|e| {
                        api_error(
                            if dry_run {
                                "Preflight reviewed SRE ClusterRoleBinding retirement"
                            } else {
                                "Retire reviewed SRE ClusterRoleBinding"
                            },
                            e,
                        )
                    })?;
            } else {
                Api::<RoleBinding>::namespaced(
                    client.clone(),
                    binding.review.namespace.as_deref().unwrap(),
                )
                .patch(&binding.review.name, &params, &Patch::Merge(patch))
                .await
                .map_err(|e| {
                    api_error(
                        if dry_run {
                            "Preflight reviewed SRE RoleBinding retirement"
                        } else {
                            "Retire reviewed SRE RoleBinding"
                        },
                        e,
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn owned(meta: &kube::api::ObjectMeta, reg: &KarsSRERegistration) -> bool {
    meta.annotations.as_ref().is_some_and(|a| {
        a.get(OWNER) == reg.metadata.uid.as_ref()
            && a.get("kars.azure.com/namespace-uid") == Some(&reg.spec.runtime_namespace.uid)
    }) && meta.uid.as_deref().is_some_and(|uid| !uid.is_empty())
        && meta
            .resource_version
            .as_deref()
            .is_some_and(|rv| !rv.is_empty())
        && meta.deletion_timestamp.is_none()
}

pub(super) async fn validate_private_targets(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let bindings: Api<ClusterRoleBinding> = Api::all(client.clone());
    for (name, role) in GRANTS {
        let definition = Api::<ClusterRole>::all(client.clone())
            .get(role)
            .await
            .map_err(|e| api_error("Preflight private SRE role", e))?;
        if !safe_private_role(role, definition.rules.as_deref().unwrap_or_default()) {
            return Err(
                "Private SRE role definition has excessive or unsupported authority".into(),
            );
        }
        if let Some(binding) = bindings
            .get_opt(name)
            .await
            .map_err(|e| api_error("Preflight private SRE grant", e))?
            && (!owned(&binding.metadata, reg)
                || binding.role_ref.name != *role
                || binding.role_ref.kind != "ClusterRole"
                || binding.role_ref.api_group != "rbac.authorization.k8s.io"
                || binding.subjects.as_deref().is_none_or(|subjects| {
                    subjects.len() != 1
                        || subjects[0].kind != "ServiceAccount"
                        || subjects[0].name != ROUTER_SA
                        || subjects[0].namespace.as_deref() != Some(RUNTIME_NAMESPACE)
                }))
        {
            return Err(
                "Reserved private SRE binding already has another owner or authority".into(),
            );
        }
    }
    let roles: Api<Role> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(role) = roles
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Preflight renewal Role", e))?
        && (!owned(&role.metadata, reg)
            || serde_json::to_value(&role.rules).ok()
                != Some(json!([{
                    "apiGroups":[""],"resources":["serviceaccounts/token"],"resourceNames":[ROUTER_SA],"verbs":["create"],
                }])))
    {
        return Err(
            "Reserved renewal Role belongs to another owner or has different authority".into(),
        );
    }
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(binding) = bindings
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Preflight renewal binding", e))?
        && (!owned(&binding.metadata, reg)
            || binding.role_ref.kind != "Role"
            || binding.role_ref.name != "sre-api-self-renew"
            || binding.subjects.as_deref().is_none_or(|subjects| {
                subjects.len() != 1
                    || subjects[0].kind != "ServiceAccount"
                    || subjects[0].name != ROUTER_SA
                    || subjects[0].namespace.as_deref() != Some(RUNTIME_NAMESPACE)
            }))
    {
        return Err(
            "Reserved renewal binding belongs to another owner or has different authority".into(),
        );
    }
    Ok(())
}

pub(super) async fn grant_private(
    client: &Client,
    reg: &KarsSRERegistration,
    sa: &ServiceAccount,
) -> Result<(), String> {
    super::credential_guard::scan(client, reg).await?;
    if !owned(&sa.metadata, reg) {
        return Err("Private SRE ServiceAccount ownership is not proven".into());
    }
    let subjects = vec![Subject {
        kind: "ServiceAccount".into(),
        name: ROUTER_SA.into(),
        namespace: Some(RUNTIME_NAMESPACE.into()),
        api_group: None,
    }];
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    for (name, role) in GRANTS {
        let definition = Api::<ClusterRole>::all(client.clone())
            .get(role)
            .await
            .map_err(|e| api_error("Validate private SRE role definition", e))?;
        if !safe_private_role(role, definition.rules.as_deref().unwrap_or_default()) {
            return Err(
                "Private SRE role definition contains unreviewed or excessive authority".into(),
            );
        }
        super::live::verify(client, reg).await?;
        let body: ClusterRoleBinding = serde_json::from_value(json!({
            "apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRoleBinding",
            "metadata":{"name":name,"annotations":{OWNER:reg.metadata.uid,
                "kars.azure.com/namespace-uid":reg.spec.runtime_namespace.uid}},
            "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":role},
            "subjects":subjects,
        }))
        .map_err(|_| "Private SRE binding serialization failed")?;
        match api
            .get_opt(name)
            .await
            .map_err(|e| api_error("Read private SRE binding", e))?
        {
            None => {
                api.create(&PostParams::default(), &body)
                    .await
                    .map_err(|e| api_error("Create private SRE binding", e))?;
            }
            Some(existing) => {
                if !owned(&existing.metadata, reg)
                    || existing.role_ref != body.role_ref
                    || existing.subjects != body.subjects
                {
                    return Err("Private SRE binding conflicts with an existing resource".into());
                }
            }
        }
    }
    let roles: Api<Role> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    let role: Role = serde_json::from_value(json!({
        "apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role",
        "metadata":{"name":"sre-api-self-renew","namespace":RUNTIME_NAMESPACE,"annotations":{
            OWNER:reg.metadata.uid,"kars.azure.com/namespace-uid":reg.spec.runtime_namespace.uid}},
        "rules":[{"apiGroups":[""],"resources":["serviceaccounts/token"],"resourceNames":[ROUTER_SA],"verbs":["create"]}],
    })).map_err(|_| "SRE token renewal Role serialization failed")?;
    match roles
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Read renewal Role", e))?
    {
        None => {
            roles
                .create(&PostParams::default(), &role)
                .await
                .map_err(|e| api_error("Create renewal Role", e))?;
        }
        Some(existing) if owned(&existing.metadata, reg) && existing.rules == role.rules => {}
        Some(_) => {
            return Err(
                "Existing SRE token renewal Role is not owned or has different authority".into(),
            );
        }
    }
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    let binding: RoleBinding = serde_json::from_value(json!({
        "apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding",
        "metadata":{"name":"sre-api-self-renew","namespace":RUNTIME_NAMESPACE,"annotations":{
            OWNER:reg.metadata.uid,"kars.azure.com/namespace-uid":reg.spec.runtime_namespace.uid}},
        "roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"sre-api-self-renew"},
        "subjects":subjects,
    })).map_err(|_| "SRE renewal binding serialization failed")?;
    match bindings
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Read renewal binding", e))?
    {
        None => {
            bindings
                .create(&PostParams::default(), &binding)
                .await
                .map_err(|e| api_error("Create renewal binding", e))?;
        }
        Some(existing)
            if owned(&existing.metadata, reg)
                && existing.subjects == binding.subjects
                && existing.role_ref == binding.role_ref => {}
        Some(_) => {
            return Err(
                "Existing SRE renewal binding is not owned or has different authority".into(),
            );
        }
    }
    Ok(())
}

fn safe_private_role(name: &str, rules: &[PolicyRule]) -> bool {
    !rules.is_empty()
        && rules.iter().all(|rule| {
            let resources = rule.resources.as_deref().unwrap_or_default();
            let groups = rule.api_groups.as_deref().unwrap_or_default();
            if resources.is_empty()
                || resources.iter().any(|r| {
                    r == "*"
                        || ["/proxy", "/exec", "/attach", "/portforward", "/token"]
                            .iter()
                            .any(|suffix| r.ends_with(suffix))
                })
            {
                return false;
            }
            match name {
                "kars-sre-private-diagnostics" => rule
                    .verbs
                    .iter()
                    .all(|verb| ["get", "list", "watch"].contains(&verb.as_str())),
                "kars-sre-action-author" => {
                    groups == ["kars.azure.com"]
                        && resources.iter().all(|r| {
                            ["karssreactions", "karssreactions/status"].contains(&r.as_str())
                        })
                        && rule
                            .verbs
                            .iter()
                            .all(|verb| ["get", "list", "watch", "create"].contains(&verb.as_str()))
                }
                "kars-sre-router-renew" => {
                    (groups == ["kars.azure.com"]
                        && resources == ["karssreregistrations"]
                        && rule.resource_names.as_deref() == Some(&["canonical".into()][..])
                        && rule
                            .verbs
                            .iter()
                            .all(|verb| ["get", "renew"].contains(&verb.as_str())))
                        || (groups == ["authorization.k8s.io"]
                            && resources == ["subjectaccessreviews"]
                            && rule.verbs == ["create"]
                            && rule.non_resource_urls.is_none())
                }
                _ => false,
            }
        })
}

pub(super) async fn revoke_private(
    client: &Client,
    reg: &KarsSRERegistration,
) -> Result<(), String> {
    let private_subject = |subject: &&Subject| {
        (subject.kind == "ServiceAccount"
            && subject.name == ROUTER_SA
            && subject.namespace.as_deref() == Some(RUNTIME_NAMESPACE))
            || (subject.kind == "User"
                && subject.name == format!("system:serviceaccount:{RUNTIME_NAMESPACE}:{ROUTER_SA}"))
    };
    let mut foreign = false;
    let api: Api<ClusterRoleBinding> = Api::all(client.clone());
    for (name, _) in GRANTS {
        if let Some(binding) = api
            .get_opt(name)
            .await
            .map_err(|e| api_error("Read retiring SRE binding", e))?
        {
            if !owned(&binding.metadata, reg) {
                foreign = true;
                continue;
            }
            let retained: Vec<_> = binding
                .subjects
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|subject| !private_subject(subject))
                .cloned()
                .collect();
            if !retained.is_empty() {
                api.patch(name, &PatchParams::default(), &Patch::Merge(json!({
                    "metadata":{"uid":binding.metadata.uid,"resourceVersion":binding.metadata.resource_version},
                    "subjects":retained,
                }))).await.map_err(|e| api_error("Revoke only the owned private SRE subject", e))?;
                continue;
            }
            api.delete(
                name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: binding.metadata.uid,
                        resource_version: binding.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Retire private SRE binding", e))?;
        }
    }
    let mut retain_role = false;
    let bindings: Api<RoleBinding> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(binding) = bindings
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Read retiring SRE renewal binding", e))?
    {
        if !owned(&binding.metadata, reg) {
            return Err("Refusing to retire an unowned renewal binding".into());
        }
        let retained: Vec<_> = binding
            .subjects
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|subject| !private_subject(subject))
            .cloned()
            .collect();
        if !retained.is_empty() {
            bindings.patch("sre-api-self-renew", &PatchParams::default(), &Patch::Merge(json!({
                "metadata":{"uid":binding.metadata.uid,"resourceVersion":binding.metadata.resource_version},
                "subjects":retained,
            }))).await.map_err(|e| api_error("Revoke only the private SRE renewal subject", e))?;
            retain_role = true;
        } else {
            bindings
                .delete(
                    "sre-api-self-renew",
                    &DeleteParams {
                        preconditions: Some(Preconditions {
                            uid: binding.metadata.uid,
                            resource_version: binding.metadata.resource_version,
                        }),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| api_error("Retire SRE renewal binding", e))?;
        }
    }
    let roles: Api<Role> = Api::namespaced(client.clone(), RUNTIME_NAMESPACE);
    if let Some(role) = roles
        .get_opt("sre-api-self-renew")
        .await
        .map_err(|e| api_error("Read retiring SRE renewal Role", e))?
        && !retain_role
    {
        if !owned(&role.metadata, reg) {
            return Err("Refusing to retire an unowned renewal Role".into());
        }
        roles
            .delete(
                "sre-api-self-renew",
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: role.metadata.uid,
                        resource_version: role.metadata.resource_version,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| api_error("Retire SRE renewal Role", e))?;
    }
    if foreign {
        Err("Unowned private SRE bindings were preserved; owned grants were revoked".into())
    } else {
        Ok(())
    }
}
