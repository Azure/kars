// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execa } from "execa";
import { describe, expect, it } from "vitest";
import { parseAllDocuments } from "yaml";

const root=new URL("../../../",import.meta.url);
function render(...args:string[]):any[] {
  return parseAllDocuments(execFileSync("helm",["template","kars",
    fileURLToPath(new URL("deploy/helm/kars",root)),"--namespace","kars-system",...args],
    {encoding:"utf8",stdio:["ignore","pipe","pipe"],timeout:30_000}))
    .map(doc=>{if(doc.errors.length)throw doc.errors[0];return doc.toJSON();}).filter(Boolean);
}
const source=(path:string)=>readFileSync(new URL(path,root),"utf8");
const get=(items:any[],kind:string,name:string)=>items.find(item=>item.kind===kind&&item.metadata?.name===name);

function publishedService() {
  return {
    apiVersion: "v1", kind: "Service",
    metadata: {
      name: "kars-observation-privacy", namespace: "core-private", uid: "service-uid",
      labels: { "app.kubernetes.io/managed-by": "Helm" },
      annotations: { "meta.helm.sh/release-name": "kars", "meta.helm.sh/release-namespace": "core-private" },
    },
    spec: {
      type: "ClusterIP", ports: [{ name: "privacy-rpc", port: 9448, targetPort: 9448, protocol: "TCP" }],
      selector: {
        "app.kubernetes.io/name": "kars", "app.kubernetes.io/component": "controller",
        "kars.azure.com/observation-privacy-revision": "a".repeat(32),
      },
    },
  };
}

async function renderUpgrade(service?: unknown, forbidden = false) {
  const directory = mkdtempSync(join(tmpdir(), "kars-privacy-service-"));
  const server = createServer((request, response) => {
    const path = new URL(request.url!, "http://localhost").pathname;
    response.setHeader("content-type", "application/json");
    let result: unknown;
    if (path === "/version") result = { major: "1", minor: "35", gitVersion: "v1.35.0" };
    if (path === "/api") result = { kind: "APIVersions", apiVersion: "v1", versions: ["v1"] };
    if (path === "/apis") result = {
      kind: "APIGroupList", apiVersion: "v1", groups: [{
        name: "admissionregistration.k8s.io",
        versions: [{ groupVersion: "admissionregistration.k8s.io/v1", version: "v1" }],
        preferredVersion: { groupVersion: "admissionregistration.k8s.io/v1", version: "v1" },
      }],
    };
    if (path === "/apis/admissionregistration.k8s.io/v1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "admissionregistration.k8s.io/v1",
      resources: [
        { name: "validatingadmissionpolicies", kind: "ValidatingAdmissionPolicy", namespaced: false, verbs: ["get"] },
        { name: "validatingadmissionpolicybindings", kind: "ValidatingAdmissionPolicyBinding", namespaced: false, verbs: ["get"] },
      ],
    };
    if (path === "/api/v1") result = {
      kind: "APIResourceList", apiVersion: "v1", groupVersion: "v1",
      resources: [{ name: "services", kind: "Service", namespaced: true, verbs: ["get"] }],
    };
    if (path === "/api/v1/namespaces/core-private/services/kars-observation-privacy") {
      result = service;
      if (forbidden) {
        response.statusCode = 403;
        result = { kind: "Status", apiVersion: "v1", reason: "Forbidden", code: 403 };
      }
    }
    if (!result) {
      response.statusCode = 404;
      result = { kind: "Status", apiVersion: "v1", reason: "NotFound", code: 404 };
    }
    response.end(JSON.stringify(result));
  });
  try {
    await new Promise<void>(resolve => server.listen(0, "127.0.0.1", resolve));
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Expected TCP test API address");
    const chart = join(directory, "chart");
    mkdirSync(join(chart, "templates"), { recursive: true });
    writeFileSync(join(chart, "Chart.yaml"), "apiVersion: v2\nname: kars\nversion: 0.1.0\n");
    copyFileSync(fileURLToPath(new URL("deploy/helm/kars/templates/observation-privacy.yaml", root)),
      join(chart, "templates/observation-privacy.yaml"));
    const config = join(directory, "config");
    writeFileSync(config, JSON.stringify({
      apiVersion: "v1", kind: "Config",
      clusters: [{ name: "fixture", cluster: { server: `http://127.0.0.1:${address.port}` } }],
      users: [{ name: "fixture", user: {} }],
      contexts: [{ name: "fixture", context: { cluster: "fixture", user: "fixture" } }],
      "current-context": "fixture",
    }), { mode: 0o600 });
    const { stdout } = await execa("helm", ["template", "kars", chart, "-n", "core-private",
      "--is-upgrade", "--dry-run=server", "--disable-openapi-validation", "--kubeconfig", config,
      "--set", "observationPrivacyRpc.enabled=true"], { timeout: 20_000 });
    return parseAllDocuments(stdout).map(document => {
      if (document.errors.length) throw document.errors[0];
      return document.toJSON();
    }).filter(Boolean);
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
    rmSync(directory, { recursive: true, force: true });
  }
}

describe("controller observation privacy RPC contract",()=>{
  it("preserves the running controller's exact selector through a real Helm lookup", async () => {
    const service = publishedService();
    const result = get(await renderUpgrade(service), "Service", service.metadata.name);
    expect(result.spec.selector).toEqual(service.spec.selector);
    expect(result.spec.ports).toEqual(service.spec.ports);
  });
  it("keeps missing and unpublished verifier Services fail-closed on upgrade", async () => {
    const service = publishedService();
    service.spec.selector["kars.azure.com/observation-privacy-revision"] = "unavailable";
    for (const existing of [undefined, service]) {
      expect(get(await renderUpgrade(existing), "Service", service.metadata.name)
        .spec.selector["kars.azure.com/observation-privacy-revision"]).toBe("unavailable");
    }
  });
  it("does not turn a denied Service read into an unavailable seed", async () => {
    await expect(renderUpgrade(undefined, true)).rejects.toThrow(/error calling lookup/);
  });
  it.each(["owner", "namespace", "manager", "port", "selector", "revision", "terminating"] as const)(
    "refuses changed verifier %s instead of adopting or overwriting it", async field => {
      const service = publishedService();
      if (field === "owner") service.metadata.annotations["meta.helm.sh/release-name"] = "foreign";
      if (field === "namespace") service.metadata.annotations["meta.helm.sh/release-namespace"] = "foreign";
      if (field === "manager") service.metadata.labels["app.kubernetes.io/managed-by"] = "foreign";
      if (field === "port") service.spec.ports[0]!.targetPort = 9449;
      if (field === "selector") Object.assign(service.spec.selector, { unexpected: "pod" });
      if (field === "revision") service.spec.selector["kars.azure.com/observation-privacy-revision"] = "forged";
      if (field === "terminating") Object.assign(service.metadata, { deletionTimestamp: "2026-10-12T00:00:00Z" });
      await expect(renderUpgrade(service)).rejects.toThrow(/Service ownership or fixed endpoint changed/);
    },
  );
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
    expect(service.metadata.namespace).toBe("kars-system");
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
      expect(JSON.stringify(policy.spec)).toContain("kars-system");
      expect(JSON.stringify(policy.spec)).toContain("request.userInfo.uid");
      expect(get(objects,"ValidatingAdmissionPolicyBinding",policy.metadata.name).spec.validationActions).toContain("Deny");
    }
    const material=JSON.stringify(get(objects,"ValidatingAdmissionPolicy","kars-observation-privacy-material").spec);
    expect(material).toContain("dyn(namespaceObject.metadata).uid");
    expect(material).not.toContain("namespaceObject.metadata.UID");
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
