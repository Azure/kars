// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const root=new URL("../../../",import.meta.url);
function render(...args:string[]):any[] {
  return parseAllDocuments(execFileSync("helm",["template","kars",
    fileURLToPath(new URL("deploy/helm/kars",root)),"--namespace","core-private",...args],
    {encoding:"utf8",stdio:["ignore","pipe","pipe"],timeout:30_000}))
    .map(doc=>{if(doc.errors.length)throw doc.errors[0];return doc.toJSON();}).filter(Boolean);
}
const source=(path:string)=>readFileSync(new URL(path,root),"utf8");
const get=(items:any[],kind:string,name:string)=>items.find(item=>item.kind===kind&&item.metadata?.name===name);

describe("controller observation privacy RPC contract",()=>{
  it("leaves default and reuse-values installs without a verifier listener or Service",()=>{
    for(const args of [[],["--is-upgrade","--set","observationPrivacyRpc=null"]]){
      const objects=render(...args);
      expect(get(objects,"Service","kars-observation-privacy")).toBeUndefined();
      const container=get(objects,"Deployment","kars-controller").spec.template.spec.containers[0];
      expect(container.ports.some((port:any)=>port.containerPort===9448)).toBe(false);
      expect(container.env.some((env:any)=>env.name==="KARS_OBSERVATION_PRIVACY_RPC_ENABLED")).toBe(false);
      expect(container.readinessProbe.httpGet.port).toBe("metrics");
    }
  });
  it("exposes only the explicit private TLS port and requires actual running capability advertisement",()=>{
    const objects=render("--set","observationPrivacyRpc.enabled=true");
    const service=get(objects,"Service","kars-observation-privacy");
    expect(service.metadata.namespace).toBe("core-private");
    expect(service.spec.type).toBe("ClusterIP");
    expect(service.spec.ports).toEqual([{name:"privacy-rpc",port:9448,targetPort:9448,protocol:"TCP"}]);
    expect(service.spec.selector["kars.azure.com/observation-privacy-revision"]).toBe("unavailable");
    const deployment=get(objects,"Deployment","kars-controller");
    expect(deployment.spec.template.metadata.labels["kars.azure.com/observation-privacy-revision"]).toBeUndefined();
    const env=deployment.spec.template.spec.containers[0].env;
    expect(env.find((item:any)=>item.name==="POD_UID").valueFrom.fieldRef.fieldPath).toBe("metadata.uid");
    expect(env.find((item:any)=>item.name==="KARS_OBSERVATION_PRIVACY_RPC_ENABLED").value).toBe("true");
    expect(objects.some(item=>item.kind==="Secret"&&item.metadata.name==="kars-observation-privacy-tls")).toBe(false);
  });
  it("protects canonical material and capability markers using real controller and namespace identity",()=>{
    const objects=render();
    for(const name of ["material","pods","service"]){
      const policy=get(objects,"ValidatingAdmissionPolicy",`kars-observation-privacy-${name}`);
      expect(policy.spec.failurePolicy).toBe("Fail");
      expect(JSON.stringify(policy.spec)).toContain("core-private");
      expect(JSON.stringify(policy.spec)).toContain("request.userInfo.uid");
      expect(get(objects,"ValidatingAdmissionPolicyBinding",policy.metadata.name).spec.validationActions).toContain("Deny");
    }
    const material=JSON.stringify(get(objects,"ValidatingAdmissionPolicy","kars-observation-privacy-material").spec);
    expect(material).toContain("namespaceObject.metadata.uid");
    expect(material).toContain("privacy-controller-uid");
  });
  it("gives the router only public descriptor reads and narrow private network paths, never raw Secret inventory",()=>{
    const metadata=source("controller/src/credential_grants/observer_metadata.rs");
    const role=metadata.slice(metadata.indexOf("let rpc_role"),metadata.indexOf("let runtime_peer"));
    expect(role).toContain('"configmaps"');
    expect(role).toContain('"services"');
    expect(role).not.toContain('"secrets"');
    expect(metadata).toContain("observation_privacy::PORT");
    expect(metadata).toContain("rpc_baseline");
    const controller=source("controller/src/privacy_rpc/authority.rs");
    expect(controller).toContain("privacy_epoch");
    expect(controller).toContain("verify_observation_writers");
    expect(controller).toContain("identity_read_only");
    expect(controller).toContain("service_observer::SECRET");
    expect(controller).not.toContain(".patch(");
    expect(controller).not.toContain(".delete(");
    const client=source("inference-router/src/observation_privacy_client.rs");
    for(const guard of [".no_proxy()",".https_only(true)",".tls_built_in_root_certs(false)","Policy::none()","proof.matches"]){
      expect(client).toContain(guard);
    }
    expect(client).not.toContain("Api::<Secret>");
  });
});
