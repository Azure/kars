// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const root=new URL("../../../",import.meta.url);
const manifests=parseAllDocuments(execFileSync("helm",[
  "template","kars",fileURLToPath(new URL("deploy/helm/kars",root)),
  "--namespace","kars-system",
],{encoding:"utf8",stdio:["ignore","pipe","pipe"],timeout:30_000}))
  .map(document=>{if(document.errors.length)throw document.errors[0];return document.toJSON();}).filter(Boolean);
const resource=(kind:string,name:string)=>manifests.find(item=>item.kind===kind&&item.metadata?.name===name);
const specSchema=(name:string)=>resource("CustomResourceDefinition",`${name}.kars.azure.com`)
  .spec.versions[0].schema.openAPIV3Schema.properties.spec;
const source=(path:string)=>readFileSync(new URL(path,root),"utf8");

describe("governed credential public contract",()=>{
  it("protects non-destructive rebind state and requires paused authority before resuming",()=>{
    const rebind=resource("ValidatingAdmissionPolicy","kars-credential-rebind-authority");
    expect(rebind.spec.matchConstraints.resourceRules[0].resources).toEqual(["karstasks"]);
    expect(JSON.stringify(rebind.spec.validations)).toContain("CredentialsPaused");
    expect(JSON.stringify(rebind.spec.variables)).toContain("project-credentials");
    const hold=resource("ValidatingAdmissionPolicy","kars-credential-runtime-hold");
    expect(hold.spec.matchConstraints.resourceRules[0].resources).toEqual(["karssandboxes","karssandboxes/status"]);
    expect(JSON.stringify(hold.spec.validations)).toContain("owner.uid");
    expect(source("controller/src/kars_team_reconciler/credential_bindings.rs")).not.toContain('"launch":false');
    expect(source("controller/src/kars_task_rebind.rs")).toContain("envelopeDigest");
    expect(source("controller/src/kars_task_rebind.rs")).toContain("credentials_quiescent");
  });
  it("holds only enrolled reader identities through revoke-before-release finalizers",()=>{
    const policy=resource("ValidatingAdmissionPolicy","kars-credential-reader-continuity");
    expect(policy.spec.paramKind).toBeUndefined();
    expect(policy.spec.matchConstraints.resourceRules[0].resources)
      .toEqual(["serviceaccounts","namespaces","namespaces/status","namespaces/finalize"]);
    const text=JSON.stringify(policy.spec);
    expect(text).toContain("request.userInfo.uid");
    expect(text).toContain("variables.before[key]");
    expect(text).toContain("request.subResource != 'finalize'");
    expect(text).not.toContain("request.operation != 'DELETE'");
    expect(resource("ValidatingAdmissionPolicyBinding",policy.metadata.name).spec.validationActions).toContain("Deny");
    expect(resource("ValidatingAdmissionPolicy","kars-credential-reader-rbac-bindings").spec.matchConditions[0].expression)
      .toContain("o.roleRef.name.startsWith(prefix)");
    expect(JSON.stringify(resource("ValidatingAdmissionPolicy","kars-credential-reader-rbac-roles").spec))
      .not.toContain("roleRef");
    const retirement=resource("ValidatingAdmissionPolicy","kars-credential-namespace-retirement");
    expect(retirement.spec.matchConstraints.resourceRules[0].resources)
      .toEqual(["namespaces","namespaces/status","namespaces/finalize"]);
    expect(retirement.spec.validations[0].expression).toContain("'kubernetes' in variables.value.spec.finalizers");
    expect(source("controller/src/credential_grants/writers/guards.rs")).toContain("no_read_authority(client, grant).await?");
  });

  it("separates writer retirement from valid source delivery and preflights observer sender egress",()=>{
    expect(specSchema("karscredentialgrants").properties.writers.minItems??0).toBe(0);
    expect(source("controller/src/credential_grants.rs")).toContain('"WriterReady"');
    expect(source("controller/src/credential_grants/writers.rs")).toContain("valid source delivery is retained");
    expect(JSON.stringify(resource("ValidatingAdmissionPolicy","kars-credential-source-writes").spec))
      .toContain("WriterReady");
    expect(source("controller/src/credential_grants/operator.rs")).toContain("observation_network::verify");
    const egress=source("controller/src/credential_grants/observation_network.rs");
    expect(egress).toContain("Private observations unavailable");
    expect(egress).not.toContain(".create(");
    expect(egress).not.toContain(".patch(");
  });
  it("defines metadata-only namespace authority without installing an operator grant",()=>{
    const crd=resource("CustomResourceDefinition","karscredentialgrants.kars.azure.com");
    expect(crd.spec.scope).toBe("Namespaced");
    expect(crd.metadata.labels["app.kubernetes.io/name"]).toBe("kars");
    expect(crd.spec.versions[0].schema.openAPIV3Schema["x-kubernetes-validations"])
      .toContainEqual({rule:"self.metadata.name == 'workspace'",message:"The namespace credential grant is the canonical workspace instance"});
    const spec=specSchema("karscredentialgrants");
    expect(spec.required).toEqual(["workspaceUid","writers"]);
    expect(spec.properties).not.toHaveProperty("data");
    expect(spec.properties).not.toHaveProperty("stringData");
    expect(spec.properties).not.toHaveProperty("values");
    expect(manifests.some(item=>item.kind==="KarsCredentialGrant")).toBe(false);
    expect(manifests.filter(item=>item.kind==="ClusterRoleBinding")
      .some(item=>item.roleRef.name==="kars-credential-grant-operator")).toBe(false);
  });

  it("declares identical credential binding shapes for Sandbox and effective Task/Team blueprints",()=>{
    const sandbox=specSchema("karssandboxes").properties.credentialBindings;
    const task=specSchema("karstasks").properties.blueprint.properties.credentialBindings;
    const team=specSchema("karsteams").properties.blueprint.properties.credentialBindings;
    const role=specSchema("karsteams").properties.roster.items.properties.blueprint.properties.credentialBindings;
    expect(task).toEqual(sandbox);
    expect(team).toEqual(sandbox);
    expect(role).toEqual(sandbox);
    expect(task.properties.grant.required).toEqual(["name","uid"]);
    expect(task.properties.sources.items.properties.source.required).toEqual(["name","uid"]);
    expect(task.properties.sources.items.properties.scope.enum).toEqual(["workspace","team","target"]);
  });

  it("keeps the source creation fence independent of a live grant parameter",()=>{
    const policy=resource("ValidatingAdmissionPolicy","kars-credential-source-boundary");
    expect(policy.spec.failurePolicy).toBe("Fail");
    expect(policy.spec.paramKind).toBeUndefined();
    const text=JSON.stringify(policy.spec);
    expect(text).toContain("use-agent-credentials");
    expect(text).toContain("request.operation != 'CREATE' || variables.input");
    expect(text).toContain("Opaque");
    expect(text).toContain("process-bootstrap");
    expect(resource("ValidatingAdmissionPolicyBinding",policy.metadata.name).spec.validationActions).toContain("Deny");
  });

  it("binds GitHub authority identically across effective launch schemas",()=>{
    const sandbox=specSchema("karssandboxes").properties.githubBinding;
    const team=specSchema("karsteams").properties;
    expect(specSchema("karstasks").properties.blueprint.properties.githubBinding).toEqual(sandbox);
    expect(team.blueprint.properties.githubBinding).toEqual(sandbox);
    expect(team.roster.items.properties.blueprint.properties.githubBinding).toEqual(sandbox);
    expect(sandbox.properties.connection.required).toEqual(["name","uid"]);
    expect(specSchema("karscredentialgrants").properties.githubConnections.items.required)
      .toEqual(["connection","appSecret","appId","ownerSubject","installationId","repositories"]);
    expect(source("controller/src/kars_task_execution.rs")).toContain('"githubBinding": blueprint.github_binding');
  });

  it("delegates only the separate observation purpose and preserves private TLS material",()=>{
    const rbac=source("controller/src/credential_grants/observer_rbac.rs");
    expect(rbac).toContain('"resourceNames":["router-services-observer"]');
    expect(rbac).not.toContain("router-admin-token");
    expect(rbac).not.toContain("router-services-admin");
    expect(rbac).not.toContain("router-services-observer-identity");
    const route=source("inference-router/src/routes/observations.rs");
    expect(route).toContain("observation_token_is_read_only");
    expect(route).toContain("stale_scope");
    expect(source("inference-router/src/service_observation_tls.rs")).toContain("tls_from_pem");
    expect(source("controller/src/credential_grants/operator.rs")).toContain("privacy_epoch");
  });

  it("gates ordinary Task readiness before execution and preserves state during credential failure",()=>{
    const task=source("controller/src/kars_task_reconciler.rs");
    expect(task.indexOf("readiness::enforce(")).toBeLessThan(task.indexOf("reconcile_execution(&ctx.client"));
    expect(task).toContain("readiness::selected(task)");
    expect(source("controller/src/credential_grants/readiness.rs")).toContain("CredentialAuthorityUnavailable");
    expect(source("controller/src/credential_grants/sources.rs")).toContain("Some(task)");
    expect(source("controller/src/kars_task_execution.rs")).toContain("credential_sources::pause_owned");
    const github=source("controller/src/credential_grants/github.rs");
    expect(github).toContain("Self::Retired(_) => None");
    for(const kind of ["karssandboxes","karstasks","karsteams"]){
      const policy=resource("ValidatingAdmissionPolicy",`kars-credential-consumer-${kind}`);
      expect(JSON.stringify(policy.spec)).toContain("kars.azure.com/github-grant-uid");
    }
  });

  it("allows controller metadata finalization but not grant spec authorship",()=>{
    const controller=resource("ClusterRole","kars-credential-grant-controller");
    const verbs=controller.rules.filter((rule:any)=>rule.resources.includes("karscredentialgrants"))
      .flatMap((rule:any)=>rule.verbs);
    expect(verbs).not.toContain("create");
    expect(verbs).not.toContain("manage");
    const policy=resource("ValidatingAdmissionPolicy","kars-credential-grant-authority");
    expect(JSON.stringify(policy.spec.validations)).toContain("object.spec == oldObject.spec");
    expect(JSON.stringify(policy.spec.validations)).toContain("review.secret.name");
  });

  it("uses resource-specific consumer policies whose fields exist in each schema",()=>{
    for(const kind of ["karssandboxes","karstasks","karsteams"]){
      const policy=resource("ValidatingAdmissionPolicy",`kars-credential-consumer-${kind}`);
      expect(policy.spec.matchConstraints.resourceRules[0].resources).toEqual([kind]);
      const text=JSON.stringify(policy.spec);
      if(kind==="karssandboxes")expect(text).not.toContain("spec.blueprint");
      else expect(text).not.toContain("spec.credentialsRef");
    }
  });

  it("preserves the legacy v1 allowlist and prevents governed-mode fallback",()=>{
    const legacy=source("controller/src/credential_source.rs");
    const keys=legacy.slice(legacy.indexOf("pub const AGENT_KEYS"),legacy.indexOf("pub fn source_name"));
    expect(keys).not.toContain("GITHUB_TOKEN");
    expect(keys).toContain("TELEGRAM_BOT_TOKEN");
    expect(source("controller/src/reconciler/credential_sources.rs"))
      .toContain("governed credential bindings were removed; legacy values remain disabled");
    expect(source("controller/src/kars_task_blueprint.rs")).toContain("spec.blueprint.clone().unwrap_or_default()");
  });
});
